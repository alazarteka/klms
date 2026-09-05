use std::{
    io::{self, Read, Write},
    net::{SocketAddr, TcpListener, TcpStream},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

#[derive(Debug)]
pub struct Request {
    pub method: String,
    pub target: String,
}

/// One recorded request: the request line plus its header lines.
#[derive(Debug, Clone)]
pub struct Recorded {
    pub line: String,
    pub headers: Vec<String>,
}

impl Recorded {
    pub fn has_header(&self, name: &str) -> bool {
        let prefix = format!("{}:", name.to_ascii_lowercase());
        self.headers
            .iter()
            .any(|header| header.to_ascii_lowercase().starts_with(&prefix))
    }
}

pub struct Response {
    status: &'static str,
    content_type: &'static str,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl Response {
    pub fn html(body: impl Into<Vec<u8>>) -> Self {
        Self {
            status: "200 OK",
            content_type: "text/html; charset=utf-8",
            headers: Vec::new(),
            body: body.into(),
        }
    }

    pub fn bytes(content_type: &'static str, body: impl Into<Vec<u8>>) -> Self {
        Self {
            status: "200 OK",
            content_type,
            headers: Vec::new(),
            body: body.into(),
        }
    }

    pub fn status(mut self, status: &'static str) -> Self {
        self.status = status;
        self
    }

    pub fn header(mut self, name: &str, value: &str) -> Self {
        self.headers.push((name.into(), value.into()));
        self
    }
}

pub struct Server {
    address: SocketAddr,
    requests: Arc<Mutex<Vec<Recorded>>>,
    stopped: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Server {
    pub fn new(router: impl Fn(&Request) -> Response + Send + Sync + 'static) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let address = listener.local_addr().unwrap();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let stopped = Arc::new(AtomicBool::new(false));
        let thread_requests = Arc::clone(&requests);
        let thread_stopped = Arc::clone(&stopped);
        let thread = thread::spawn(move || {
            while !thread_stopped.load(Ordering::Relaxed) {
                let (mut stream, _) = match listener.accept() {
                    Ok(value) => value,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(2));
                        continue;
                    }
                    Err(error) => panic!("fixture server accept failed: {error}"),
                };
                // Accepted sockets inherit nonblocking mode on some platforms.
                stream.set_nonblocking(false).unwrap();
                stream
                    .set_write_timeout(Some(Duration::from_secs(1)))
                    .unwrap();
                let Some(buffer) = read_headers(&mut stream)
                    .unwrap_or_else(|error| panic!("fixture server read failed: {error}"))
                else {
                    continue;
                };
                let raw = String::from_utf8_lossy(&buffer);
                let mut lines = raw.lines();
                let line = lines.next().unwrap_or_default().to_owned();
                let headers = lines
                    .take_while(|header| !header.is_empty())
                    .map(ToOwned::to_owned)
                    .collect();
                thread_requests.lock().unwrap().push(Recorded {
                    line: line.clone(),
                    headers,
                });
                let mut parts = line.split_whitespace();
                let request = Request {
                    method: parts.next().unwrap_or_default().into(),
                    target: parts.next().unwrap_or_default().into(),
                };
                let response = router(&request);
                let mut headers = format!(
                    "HTTP/1.1 {}\r\nContent-Type: {}\r\nContent-Length: {}\r\n",
                    response.status,
                    response.content_type,
                    response.body.len()
                );
                for (name, value) in response.headers {
                    headers.push_str(&format!("{name}: {value}\r\n"));
                }
                headers.push_str("Connection: close\r\n\r\n");
                let written = stream.write_all(headers.as_bytes()).and_then(|()| {
                    if request.method != "HEAD" {
                        stream.write_all(&response.body)
                    } else {
                        Ok(())
                    }
                });
                if let Err(error) = written {
                    // Cancellation may close a client before its response is sent.
                    if !disconnected(&error) {
                        panic!("fixture server write failed: {error}");
                    }
                }
            }
        });
        Self {
            address,
            requests,
            stopped,
            thread: Some(thread),
        }
    }

    pub fn url(&self) -> String {
        format!("http://{}", self.address)
    }

    pub fn requests(&self) -> Vec<String> {
        self.recorded().into_iter().map(|r| r.line).collect()
    }

    pub fn recorded(&self) -> Vec<Recorded> {
        self.requests.lock().unwrap().clone()
    }
}

fn disconnected(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::BrokenPipe
            | io::ErrorKind::ConnectionReset
            | io::ErrorKind::ConnectionAborted
    )
}

/// Incomplete, oversized, or stalled requests are discarded, never routed.
fn read_headers(stream: &mut TcpStream) -> io::Result<Option<Vec<u8>>> {
    let deadline = Instant::now() + Duration::from_secs(1);
    let mut buffer = [0_u8; 16 * 1024];
    let mut length = 0;
    while length < buffer.len() {
        let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
            return Ok(None);
        };
        if remaining.is_zero() {
            return Ok(None);
        }
        stream.set_read_timeout(Some(remaining))?;
        match stream.read(&mut buffer[length..]) {
            Ok(0) => return Ok(None),
            Ok(read) => {
                length += read;
                if let Some(end) = buffer[..length].windows(4).position(|s| s == b"\r\n\r\n") {
                    return Ok(Some(buffer[..end + 4].to_vec()));
                }
            }
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::Interrupted
                        | io::ErrorKind::WouldBlock
                        | io::ErrorKind::TimedOut
                ) => {}
            Err(error) if disconnected(&error) => return Ok(None),
            Err(error) => return Err(error),
        }
    }
    Ok(None)
}

impl Drop for Server {
    fn drop(&mut self) {
        self.stopped.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            if let Err(error) = thread.join() {
                if !thread::panicking() {
                    std::panic::resume_unwind(error);
                }
            }
        }
    }
}
