use std::{
    io::{Read, Write},
    net::{SocketAddr, TcpListener},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread::{self, JoinHandle},
    time::Duration,
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
                let mut buffer = [0_u8; 16 * 1024];
                let length = stream.read(&mut buffer).unwrap();
                let raw = String::from_utf8_lossy(&buffer[..length]);
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
                stream.write_all(headers.as_bytes()).unwrap();
                if request.method != "HEAD" {
                    stream.write_all(&response.body).unwrap();
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

impl Drop for Server {
    fn drop(&mut self) {
        self.stopped.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            thread.join().unwrap();
        }
    }
}
