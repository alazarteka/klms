//! The one bounded copy loop for streamed video bodies, shared by native
//! `/vod1/` MP4 downloads and Panopto media.
use std::{
    io::{Read, Write},
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};

use crate::{error::AppError, http::Response};

/// Bounds for one video transfer.
#[derive(Clone, Copy, Debug)]
pub struct Limits {
    pub max_bytes: usize,
    /// Overall deadline (`--max-seconds`).
    pub deadline: Instant,
    /// Longest wait for the next bytes (`--stall-seconds`).
    pub stall: Duration,
}

impl Limits {
    pub fn new(max_bytes: usize, max_seconds: u64, stall_seconds: u64) -> Self {
        Self {
            max_bytes,
            deadline: Instant::now() + Duration::from_secs(max_seconds),
            stall: Duration::from_secs(stall_seconds),
        }
    }

    /// Time left before the overall deadline, for a request's own timeout.
    pub fn remaining(&self) -> Result<Duration, AppError> {
        self.deadline
            .checked_duration_since(Instant::now())
            .filter(|left| !left.is_zero())
            .ok_or_else(deadline_error)
    }

    pub fn with_max(self, max_bytes: usize) -> Self {
        Self { max_bytes, ..self }
    }
}

fn deadline_error() -> AppError {
    AppError::network("video transfer exceeded its --max-seconds time limit")
}

fn too_big(max: usize) -> AppError {
    AppError::limit(format!("video exceeds the {max} byte limit"))
}

/// What the start of the body must look like before anything is written.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Expect {
    /// An MP4 whose first box is a well-formed `ftyp`.
    Mp4,
    /// Any media that does not sniff as HTML.
    Media,
}

const PREFIX: usize = 12;

/// Streams `response` into `writer` under `limits` and returns the byte
/// count. It refuses a declared or streamed size over the cap, an HTML or
/// non-MP4 start (via `refuse`, before writing), a stall, the deadline, and
/// a body shorter than its Content-Length.
pub fn copy(
    response: Response,
    writer: &mut (impl Write + ?Sized),
    limits: Limits,
    expect: Expect,
    refuse: &dyn Fn() -> AppError,
) -> Result<usize, AppError> {
    let max = limits.max_bytes;
    let expected = response.content_length();
    if expected.is_some_and(|n| n > max as u64) {
        return Err(too_big(max));
    }
    if response
        .header("content-type")
        .is_some_and(|t| t.to_ascii_lowercase().contains("html"))
    {
        return Err(refuse());
    }
    let chunks = read_in_background(response);
    let mut head = Vec::new();
    let mut checked = false;
    let mut total = 0_usize;
    loop {
        let wait = limits.remaining()?.min(limits.stall);
        let chunk = match chunks.recv_timeout(wait) {
            // The request's own timeout ends at the deadline too.
            Ok(Err(_)) if Instant::now() >= limits.deadline => return Err(deadline_error()),
            Ok(chunk) => {
                chunk.map_err(|e| AppError::network(format!("video transfer interrupted: {e}")))?
            }
            Err(mpsc::RecvTimeoutError::Timeout) if Instant::now() >= limits.deadline => {
                return Err(deadline_error());
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                return Err(AppError::network(format!(
                    "video transfer stalled: no data for {} seconds (--stall-seconds)",
                    limits.stall.as_secs()
                )));
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => Vec::new(),
        };
        let eof = chunk.is_empty();
        total = total.checked_add(chunk.len()).ok_or_else(|| too_big(max))?;
        if total > max {
            return Err(too_big(max));
        }
        let data = if checked {
            chunk
        } else {
            // Hold the start back until it can be checked.
            head.extend_from_slice(&chunk);
            if head.len() < PREFIX && !eof {
                continue;
            }
            check_start(&head, expect, refuse)?;
            checked = true;
            std::mem::take(&mut head)
        };
        writer
            .write_all(&data)
            .map_err(|e| AppError::config(format!("failed to write video: {e}")))?;
        if eof {
            break;
        }
    }
    if total == 0 || expected.is_some_and(|n| n != total as u64) {
        return Err(AppError::network(
            "video transfer ended before the full body",
        ));
    }
    Ok(total)
}

fn check_start(head: &[u8], expect: Expect, refuse: &dyn Fn() -> AppError) -> Result<(), AppError> {
    let leading = String::from_utf8_lossy(&head[..head.len().min(512)]).to_ascii_lowercase();
    let html = leading.contains("<html") || leading.contains("<!doctype");
    let mp4 = head.len() >= PREFIX
        && &head[4..8] == b"ftyp"
        && u32::from_be_bytes([head[0], head[1], head[2], head[3]]) >= PREFIX as u32;
    if html || expect == Expect::Mp4 && !mp4 {
        return Err(refuse());
    }
    Ok(())
}

/// Reads the body on its own thread so a silent connection is noticed after
/// the stall timeout. An empty chunk marks the end; the channel holds at most
/// two chunks, and the reader stops once the receiver is gone. A read that
/// never returns keeps its connection until the request's own deadline; the
/// command fails and exits long before that matters.
fn read_in_background(mut response: Response) -> mpsc::Receiver<std::io::Result<Vec<u8>>> {
    let (send, receive) = mpsc::sync_channel(2);
    thread::spawn(move || {
        loop {
            let mut buffer = vec![0_u8; 64 * 1024];
            let result = response.read(&mut buffer).map(|n| {
                buffer.truncate(n);
                buffer
            });
            let last = !matches!(&result, Ok(chunk) if !chunk.is_empty());
            if send.send(result).is_err() || last {
                return;
            }
        }
    });
    receive
}
