use super::Download;
use crate::{client::stream::Limits, error::AppError};
use std::{
    io::{Read, Write},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};

fn ffmpeg_error(e: &std::io::Error) -> AppError {
    if e.kind() == std::io::ErrorKind::NotFound {
        AppError::config("ffmpeg is required for HLS or multiple streams; install ffmpeg and retry")
    } else {
        AppError::config("could not start local ffmpeg")
    }
}
const FFMPEG_PROBE_LIMIT: Duration = Duration::from_secs(10);

/// Fails before any media is fetched when the remux step could not run.
pub(super) fn require_ffmpeg(ffmpeg: &std::ffi::OsStr, limits: Limits) -> Result<(), AppError> {
    probe_ffmpeg(ffmpeg, limits, FFMPEG_PROBE_LIMIT)
}

pub(super) fn probe_ffmpeg(
    ffmpeg: &std::ffi::OsStr,
    limits: Limits,
    probe_limit: Duration,
) -> Result<(), AppError> {
    let mut child = ChildGuard(
        Command::new(ffmpeg)
            .arg("-version")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| ffmpeg_error(&e))?,
    );
    // Generous enough for a cold start on a loaded machine; a hang here is a
    // local install problem, so it is not reported as a retryable network error.
    let deadline = limits.deadline.min(Instant::now() + probe_limit);
    loop {
        limits.remaining()?;
        if Instant::now() >= deadline {
            return Err(AppError::config(format!(
                "ffmpeg -version did not finish within {} seconds",
                probe_limit.as_secs_f64()
            ))
            .with_hint(
                "Check that `ffmpeg -version` runs promptly on this machine, or reinstall ffmpeg.",
            ));
        }
        if let Some(status) = child
            .0
            .try_wait()
            .map_err(|_| AppError::config("could not monitor ffmpeg"))?
        {
            return if status.success() {
                Ok(())
            } else {
                Err(AppError::config(
                    "ffmpeg -version failed; check the ffmpeg install",
                ))
            };
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Covers monitoring failures and every early return as well as normal completion.
struct ChildGuard(Child);
impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

pub(super) const STDERR_TAIL: usize = 4096;
fn stderr_tail(mut reader: impl Read) -> Vec<u8> {
    let mut tail = std::collections::VecDeque::with_capacity(STDERR_TAIL);
    let mut buffer = [0; STDERR_TAIL];
    while let Ok(n) = reader.read(&mut buffer) {
        if n == 0 {
            break;
        }
        let excess = (tail.len() + n).saturating_sub(STDERR_TAIL);
        tail.drain(..excess);
        tail.extend(&buffer[..n]);
    }
    tail.into_iter().collect()
}
pub(super) fn remux(
    ffmpeg: &std::ffi::OsStr,
    inputs: &[PathBuf],
    offsets: &[f64],
    scratch: &Path,
    writer: &mut impl Write,
    limits: Limits,
) -> Result<Download, AppError> {
    let max = limits.max_bytes;
    limits.remaining()?;
    let output = scratch.join("remux.mp4");
    let mut c = Command::new(ffmpeg);
    c.args(["-nostdin", "-hide_banner", "-loglevel", "error", "-xerror"]);
    for (p, offset) in inputs.iter().zip(offsets) {
        if *offset != 0.0 {
            c.arg("-itsoffset").arg(offset.to_string());
        }
        c.args(["-protocol_whitelist", "file", "-i"]).arg(p);
    }
    for i in 0..inputs.len() {
        c.arg("-map")
            .arg(format!("{i}:v?"))
            .arg("-map")
            .arg(format!("{i}:a?"));
    }
    c.args(["-c", "copy", "-movflags", "+faststart", "-n"])
        .arg(&output)
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    let mut child = ChildGuard(c.spawn().map_err(|e| ffmpeg_error(&e))?);
    let stderr = child.0.stderr.take().expect("piped stderr");
    let tail = std::thread::spawn(move || stderr_tail(stderr));
    let status = loop {
        limits.remaining()?;
        if std::fs::metadata(&output).is_ok_and(|m| m.len() > max as u64) {
            return Err(AppError::limit("video remux exceeds byte limit"));
        }
        if let Some(status) = child
            .0
            .try_wait()
            .map_err(|_| AppError::config("could not monitor ffmpeg"))?
        {
            break status;
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    let tail = tail.join().unwrap_or_default();
    limits.remaining()?;
    if !status.success() {
        return Err(AppError::content_unavailable(format!(
            "ffmpeg could not remux downloaded video fragments: {}",
            String::from_utf8_lossy(&tail).trim()
        )));
    }
    let size = std::fs::metadata(&output)
        .map_err(|_| AppError::config("ffmpeg produced no output"))?
        .len();
    if size == 0 || size > max as u64 {
        return Err(AppError::limit(
            "remuxed video exceeds byte limit or is empty",
        ));
    }
    // Free the fetched inputs before the copy so peak disk use stays near
    // twice the media size rather than three times.
    for input in std::fs::read_dir(scratch).into_iter().flatten().flatten() {
        if input.path() != output {
            let _ = std::fs::remove_file(input.path());
        }
    }
    let mut f = std::fs::File::open(&output)
        .map_err(|_| AppError::config("could not read remuxed video"))?;
    let mut buffer = [0; 64 * 1024];
    loop {
        limits.remaining()?;
        let n = f
            .read(&mut buffer)
            .map_err(|_| AppError::config("could not read remuxed video"))?;
        if n == 0 {
            break;
        }
        writer
            .write_all(&buffer[..n])
            .map_err(|_| AppError::config("could not write remuxed video"))?;
        limits.remaining()?;
    }
    drop(f);
    std::fs::remove_file(&output)
        .map_err(|_| AppError::config("could not remove remuxed video"))?;
    Ok(Download {
        bytes: size as usize,
        content_type: "video/mp4",
    })
}
