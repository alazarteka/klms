use std::{
    borrow::Cow,
    io::{Read, Write},
    time::Duration,
};

use crate::{
    error::AppError,
    http::{self, Failure, Follow, HeaderValue, Method, Payload, Response},
    url::{Origin, Url},
};

pub type Client = ureq::Agent;

/// Redirects any KLMS or release request may follow.
const MAX_REDIRECTS: usize = 5;
const MAX_BODY_BYTES: usize = 8 * 1024 * 1024;

pub struct KlmsClient {
    base_url: Url,
    http: Client,
    cookie: String,
}

pub struct HtmlResponse {
    pub url: Url,
    pub text: String,
}

pub struct ByteResponse {
    pub url: Url,
    pub bytes: Vec<u8>,
}

pub struct PreviewResponse {
    pub url: Url,
    pub content_type: Option<String>,
    pub bytes: Vec<u8>,
    pub truncated: bool,
}

pub struct DownloadResponse {
    pub url: Url,
    pub content_type: Option<String>,
    pub bytes: usize,
}

#[derive(Clone, Debug)]
pub struct RemoteMetadata {
    pub url: Url,
    pub status: u16,
    pub etag: Option<String>,
    pub last_modified: Option<String>,
    pub content_length: Option<u64>,
    pub content_type: Option<String>,
    pub content_range: Option<String>,
}

#[derive(Debug)]
pub struct ConditionalResponse {
    pub metadata: RemoteMetadata,
    pub bytes: Option<Vec<u8>>,
}

fn too_big(max: usize) -> AppError {
    AppError::limit(format!("KLMS response exceeded the {max} byte limit"))
}

fn expired_session() -> AppError {
    AppError::auth_required("the saved KLMS session is missing or expired")
}

fn bad_status(response: &Response) -> AppError {
    AppError::http(response.status(), response.url().path())
}

fn has_userinfo(url: &Url) -> bool {
    !url.username().is_empty() || url.password().is_some()
}

impl KlmsClient {
    pub fn new(
        base: &str,
        cookie_header: Option<&str>,
        timeout_seconds: u64,
    ) -> Result<Self, AppError> {
        let cookie = cookie_header.unwrap_or("");
        HeaderValue::from_str(cookie).map_err(|_| {
            AppError::config("storage state contains a cookie value invalid for HTTP headers")
        })?;
        Ok(Self {
            base_url: validate_base_url(base)?,
            http: http::agent(
                Duration::from_secs(timeout_seconds),
                Some(Duration::from_secs(8)),
            ),
            cookie: cookie.to_owned(),
        })
    }

    pub fn get(&self, path: &str) -> Result<HtmlResponse, AppError> {
        let response = self.get_bytes(path, MAX_BODY_BYTES)?;
        let text = String::from_utf8_lossy(&response.bytes).into_owned();
        if looks_logged_out(&response.url, &text) {
            return Err(expired_session());
        }
        Ok(HtmlResponse {
            url: response.url,
            text,
        })
    }

    pub fn get_bytes(&self, path: &str, max_bytes: usize) -> Result<ByteResponse, AppError> {
        let mut response = self.send_get(path)?;
        let bytes = read_capped(&mut response, max_bytes)?;
        check_logged_out(&response, &bytes)?;
        Ok(ByteResponse {
            url: response.url().clone(),
            bytes,
        })
    }

    pub fn head(&self, path: &str) -> Result<RemoteMetadata, AppError> {
        let url = self.resolve(path)?;
        let response = self.fetch(Method::HEAD, url, None, Vec::new(), "KLMS HEAD failed")?;
        if !(200..300).contains(&response.status()) {
            return Err(bad_status(&response));
        }
        Ok(remote_metadata(&response))
    }

    pub fn get_conditional(
        &self,
        path: &str,
        etag: Option<&str>,
        last_modified: Option<&str>,
        max_bytes: usize,
    ) -> Result<ConditionalResponse, AppError> {
        let url = self.resolve(path)?;
        let mut extra = Vec::new();
        for (name, value, label) in [
            ("if-none-match", etag, "ETag"),
            ("if-modified-since", last_modified, "Last-Modified"),
        ] {
            if let Some(value) = value {
                HeaderValue::from_str(value).map_err(|_| {
                    AppError::config(format!("stored {label} is invalid for an HTTP header"))
                })?;
                extra.push((name, value.to_owned()));
            }
        }
        let mut response = self.fetch(
            Method::GET,
            url,
            None,
            extra,
            "conditional KLMS request failed",
        )?;
        let metadata = remote_metadata(&response);
        if metadata.status == 304 {
            return Ok(ConditionalResponse {
                metadata,
                bytes: None,
            });
        }
        if !matches!(metadata.status, 200 | 206) {
            return Err(bad_status(&response));
        }
        let bytes = read_capped(&mut response, max_bytes)?;
        validate_complete_bytes(&metadata, bytes.len())?;
        check_logged_out(&response, &bytes)?;
        Ok(ConditionalResponse {
            metadata,
            bytes: Some(bytes),
        })
    }

    pub fn get_preview(&self, path: &str, max_bytes: usize) -> Result<PreviewResponse, AppError> {
        let mut response = self.send_get(path)?;
        let mut bytes = read_prefix(&mut response, max_bytes)?;
        let truncated = bytes.len() > max_bytes;
        bytes.truncate(max_bytes);
        check_logged_out(&response, &bytes)?;
        Ok(PreviewResponse {
            url: response.url().clone(),
            content_type: response.header("content-type").map(str::to_owned),
            bytes,
            truncated,
        })
    }

    pub fn download_to(
        &self,
        path: &str,
        max_bytes: usize,
        writer: &mut impl Write,
    ) -> Result<DownloadResponse, AppError> {
        const SAMPLE: usize = 64 * 1024;
        let mut response = self.send_get(path)?;
        let metadata = remote_metadata(&response);
        if metadata
            .content_length
            .is_some_and(|n| n > max_bytes as u64)
        {
            return Err(too_big(max_bytes));
        }
        let mut sample = Vec::new();
        let mut buffer = [0_u8; SAMPLE];
        let mut total = 0_usize;
        loop {
            let read = response
                .read(&mut buffer)
                .map_err(|error| AppError::network(format!("failed to read download: {error}")))?;
            if read == 0 {
                break;
            }
            total += read;
            if total > max_bytes {
                return Err(too_big(max_bytes));
            }
            let keep = read.min(SAMPLE - sample.len());
            sample.extend_from_slice(&buffer[..keep]);
            writer
                .write_all(&buffer[..read])
                .map_err(|error| AppError::config(format!("failed to write download: {error}")))?;
        }
        validate_complete_bytes(&metadata, total)?;
        check_logged_out(&response, &sample)?;
        Ok(DownloadResponse {
            url: metadata.url,
            content_type: metadata.content_type,
            bytes: total,
        })
    }

    fn send_get(&self, path: &str) -> Result<Response, AppError> {
        let url = self.resolve(path)?;
        let response = self.fetch(Method::GET, url, None, Vec::new(), "KLMS request failed")?;
        if !(200..300).contains(&response.status()) {
            return Err(bad_status(&response));
        }
        Ok(response)
    }

    /// One request with same-origin, userinfo-free redirect following. The
    /// cookie header accompanies every hop because every hop is same-origin.
    fn fetch(
        &self,
        method: Method,
        url: Url,
        payload: Option<Payload>,
        mut headers: Vec<(&'static str, String)>,
        context: &str,
    ) -> Result<Response, AppError> {
        if !self.cookie.is_empty() {
            headers.push(("cookie", self.cookie.clone()));
        }
        let mut policy = KlmsPolicy {
            origin: self.base_url.origin(),
            headers,
        };
        let request = Follow {
            method,
            url,
            payload,
            max_redirects: MAX_REDIRECTS,
            strict: false,
        };
        http::follow(&self.http, request, &mut policy).map_err(|failure| {
            AppError::network(format!("{context}: {}", describe(failure, "redirect")))
        })
    }

    fn resolve(&self, path: &str) -> Result<Url, AppError> {
        let url = self
            .base_url
            .join(path)
            .map_err(|error| AppError::config(format!("invalid KLMS path: {error}")))?;
        if url.origin() != self.base_url.origin() {
            return Err(AppError::config("cross-origin request path refused"));
        }
        if has_userinfo(&url) {
            return Err(AppError::config("request URL must not contain userinfo"));
        }
        Ok(url)
    }

    pub fn ajax(&self, sesskey: &str, method: &'static str) -> Result<serde_json::Value, AppError> {
        if !["core_session_time_remaining", "core_session_touch"].contains(&method) {
            return Err(AppError::internal(
                "attempted a non-allowlisted Moodle AJAX method",
            ));
        }
        let mut url = self
            .base_url
            .join("/lib/ajax/service.php")
            .expect("valid built-in path");
        url.query_pairs_mut()
            .append_pair("sesskey", sesskey)
            .append_pair("info", method);
        let body = serde_json::json!([{"index": 0, "methodname": method, "args": {}}]);
        let payload = Payload {
            content_type: "application/json",
            bytes: body.to_string().into_bytes(),
        };
        let extra = vec![("x-requested-with", "XMLHttpRequest".to_owned())];
        let mut response = self.fetch(
            Method::POST,
            url,
            Some(payload),
            extra,
            "KLMS AJAX request failed",
        )?;
        if !(200..300).contains(&response.status()) {
            return Err(AppError::http(response.status(), "/lib/ajax/service.php"));
        }
        let body = read_capped(&mut response, MAX_BODY_BYTES)?;
        let value: serde_json::Value = serde_json::from_slice(&body)
            .map_err(|error| AppError::shape(format!("invalid KLMS AJAX response: {error}")))?;
        let first = value
            .as_array()
            .and_then(|rows| rows.first())
            .ok_or_else(|| AppError::shape("KLMS AJAX response was not a non-empty array"))?;
        if first.get("error").and_then(serde_json::Value::as_bool) == Some(true) {
            let message = first
                .pointer("/exception/message")
                .or_else(|| first.get("message"))
                .and_then(serde_json::Value::as_str)
                .unwrap_or("KLMS AJAX returned an error");
            let lower = message.to_ascii_lowercase();
            if ["session", "sesskey", "login"]
                .iter()
                .any(|word| lower.contains(word))
            {
                return Err(expired_session());
            }
            return Err(AppError::upstream(message));
        }
        Ok(first
            .get("data")
            .cloned()
            .unwrap_or(serde_json::Value::Null))
    }
}

fn describe(failure: Failure, redirect: &str) -> String {
    match failure {
        Failure::Transport(message) => message,
        Failure::Refused(error) => error.message,
        Failure::TooManyRedirects => "too many redirects".into(),
        Failure::MissingLocation | Failure::InvalidLocation => format!("invalid {redirect}"),
    }
}

fn remote_metadata(response: &Response) -> RemoteMetadata {
    let header = |name| response.header(name).map(str::to_owned);
    RemoteMetadata {
        url: response.url().clone(),
        status: response.status(),
        etag: header("etag"),
        last_modified: header("last-modified"),
        content_length: response.content_length(),
        content_type: header("content-type"),
        content_range: header("content-range"),
    }
}

fn validate_complete_bytes(metadata: &RemoteMetadata, body_length: usize) -> Result<(), AppError> {
    let length = body_length as u64;
    if metadata.content_length.is_some_and(|n| n != length) {
        return Err(AppError::upstream(
            "KLMS response Content-Length did not match the received bytes",
        ));
    }
    if metadata.status != 206 {
        return Ok(());
    }
    let range = metadata.content_range.as_deref().and_then(|value| {
        let (bounds, total) = value.strip_prefix("bytes ")?.split_once('/')?;
        let (start, end) = bounds.split_once('-')?;
        Some((
            start.parse::<u64>().ok()?,
            end.parse::<u64>().ok()?,
            total.parse::<u64>().ok()?,
        ))
    });
    match range {
        Some((0, end, total)) if end.checked_add(1) == Some(total) && total == length => Ok(()),
        _ => Err(AppError::upstream(
            "KLMS returned a partial byte range, not a complete object",
        )),
    }
}

/// Reads at most `max_bytes + 1` bytes; a longer result means the body was cut.
fn read_prefix(response: &mut Response, max_bytes: usize) -> Result<Vec<u8>, AppError> {
    let mut body = Vec::with_capacity(max_bytes.min(64 * 1024));
    response
        .by_ref()
        .take(max_bytes as u64 + 1)
        .read_to_end(&mut body)
        .map_err(|error| AppError::network(format!("failed to read KLMS response: {error}")))?;
    Ok(body)
}

/// The whole body, or a limit error when it is declared or turns out larger.
fn read_capped(response: &mut Response, max_bytes: usize) -> Result<Vec<u8>, AppError> {
    if response
        .content_length()
        .is_some_and(|n| n > max_bytes as u64)
    {
        return Err(too_big(max_bytes));
    }
    let body = read_prefix(response, max_bytes)?;
    if body.len() > max_bytes {
        return Err(too_big(max_bytes));
    }
    Ok(body)
}

fn check_logged_out(response: &Response, bytes: &[u8]) -> Result<(), AppError> {
    let leading = String::from_utf8_lossy(&bytes[..bytes.len().min(512)]).to_ascii_lowercase();
    let looks_html = response
        .header("content-type")
        .is_some_and(|value| value.to_ascii_lowercase().contains("html"))
        || leading.contains("<!doctype html")
        || leading.contains("<html");
    let text = if looks_html {
        String::from_utf8_lossy(bytes)
    } else {
        Cow::Borrowed("")
    };
    if looks_logged_out(response.url(), &text) {
        return Err(expired_session());
    }
    Ok(())
}

pub fn validate_base_url(value: &str) -> Result<Url, AppError> {
    let mut url = Url::parse(value)
        .map_err(|error| AppError::config(format!("invalid KLMS base URL: {error}")))?;
    if has_userinfo(&url) || url.query().is_some() || url.fragment().is_some() {
        return Err(AppError::config(
            "KLMS base URL must not contain credentials, query, or fragment",
        ));
    }
    if url.scheme() != "https" && !url.is_http_loopback() {
        return Err(AppError::config(
            "KLMS base URL must use HTTPS (HTTP is loopback-only)",
        ));
    }
    url.set_path("/");
    Ok(url)
}

fn looks_logged_out(url: &Url, html: &str) -> bool {
    let lower = html.to_ascii_lowercase();
    url.path().to_ascii_lowercase().contains("/login/")
        || lower.contains("name=\"username\"") && lower.contains("name=\"password\"")
        || lower.contains("id=\"loginbtn\"")
}

/// Separate unauthenticated transport for explicit release checks/downloads.
/// KLMS cookies and origin policy never enter this client.
pub fn release_client(timeout: u64) -> Result<Client, AppError> {
    Ok(http::agent(
        Duration::from_secs(timeout.clamp(1, 300)),
        None,
    ))
}

struct ReleasePolicy;

impl http::Policy for ReleasePolicy {
    fn headers(&mut self, _method: &Method, _url: &Url) -> Vec<(&'static str, String)> {
        Vec::new()
    }

    fn allow(&mut self, url: &Url) -> Result<(), AppError> {
        if url.scheme() == "https" {
            Ok(())
        } else {
            Err(AppError::network("release download requires HTTPS"))
        }
    }
}

pub fn release_bytes(client: &Client, url: &str, limit: u64) -> Result<Vec<u8>, AppError> {
    let url = Url::parse(url).map_err(|e| AppError::network(e.to_string()))?;
    let request = Follow {
        method: Method::GET,
        url,
        payload: None,
        max_redirects: MAX_REDIRECTS,
        strict: false,
    };
    let response = http::follow(client, request, &mut ReleasePolicy)
        .map_err(|failure| AppError::network(describe(failure, "release redirect")))?;
    if response.status() != 200 {
        return Err(AppError::network(format!(
            "release request returned HTTP {}",
            response.status()
        )));
    }
    let expected = response.content_length();
    let mut bytes = Vec::new();
    response
        .take(limit + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| AppError::network(format!("release response read failed: {e}")))?;
    if bytes.len() as u64 > limit {
        return Err(AppError::limit("release response exceeds size limit"));
    }
    if expected.is_some_and(|length| length != bytes.len() as u64) {
        return Err(AppError::network("incomplete release response"));
    }
    Ok(bytes)
}

struct KlmsPolicy {
    origin: Origin,
    headers: Vec<(&'static str, String)>,
}

impl http::Policy for KlmsPolicy {
    fn headers(&mut self, _method: &Method, _url: &Url) -> Vec<(&'static str, String)> {
        self.headers.clone()
    }

    fn allow(&mut self, url: &Url) -> Result<(), AppError> {
        if url.origin() != self.origin {
            return Err(AppError::network("cross-origin redirect refused"));
        }
        if has_userinfo(url) {
            return Err(AppError::network("URL userinfo refused"));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::{
        io::{Read, Write},
        net::{SocketAddr, TcpListener},
        thread::{self, JoinHandle},
        time::{Duration, Instant},
    };

    use super::{KlmsClient, validate_base_url};

    const REDIRECT: &str =
        "HTTP/1.1 302 Found\r\nLocation: /again\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";

    /// Answers each connection with the next response; returns the lowercased requests.
    fn serve(responses: Vec<String>) -> (SocketAddr, JoinHandle<Vec<String>>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let handle = thread::spawn(move || {
            let mut seen = Vec::new();
            for response in responses {
                let (mut stream, _) = listener.accept().unwrap();
                let mut request = [0_u8; 4096];
                let length = stream.read(&mut request).unwrap();
                seen.push(String::from_utf8_lossy(&request[..length]).to_ascii_lowercase());
                stream.write_all(response.as_bytes()).unwrap();
            }
            seen
        });
        (address, handle)
    }

    fn client(address: SocketAddr, cookie: Option<&str>, timeout: u64) -> KlmsClient {
        KlmsClient::new(&format!("http://{address}"), cookie, timeout).unwrap()
    }

    #[test]
    fn accepts_https_and_loopback_http_only() {
        assert!(validate_base_url("https://klms.kaist.ac.kr").is_ok());
        assert!(validate_base_url("http://127.0.0.1:9999").is_ok());
        assert!(validate_base_url("http://[::1]:9").is_ok());
        assert!(validate_base_url("http://[::2]:9").is_err());
        assert!(validate_base_url("http://example.com").is_err());
        assert!(validate_base_url("https://user@example.com").is_err());
    }

    #[test]
    fn conditional_get_handles_opaque_etags_and_only_accepts_complete_206() {
        let partial = |range: &str| {
            format!(
                "HTTP/1.1 206 Partial Content\r\nETag: \"changed\"\r\nContent-Type: application/octet-stream\r\nContent-Length: 3\r\nContent-Range: {range}\r\nConnection: close\r\n\r\nnew"
            )
        };
        let (address, server) = serve(vec![
            "HTTP/1.1 304 Not Modified\r\nETag: W/\"opaque-value\"\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".into(),
            partial("bytes 0-2/8"),
            partial("bytes 0-2/3"),
        ]);
        let client = client(address, None, 5);
        let etag = Some("W/\"opaque-value\"");
        let unchanged = client.get_conditional("/file", etag, None, 8).unwrap();
        assert_eq!(unchanged.metadata.status, 304);
        assert!(unchanged.bytes.is_none());
        let prefix = client.get_conditional("/file", etag, None, 8).unwrap_err();
        assert_eq!(prefix.code, "UPSTREAM_ERROR");
        let complete = client.get_conditional("/file", None, None, 8).unwrap();
        assert_eq!(complete.bytes.as_deref(), Some(&b"new"[..]));
        let seen = server.join().unwrap();
        assert!(
            seen[..2]
                .iter()
                .all(|r| r.contains("if-none-match: w/\"opaque-value\""))
        );
    }

    #[test]
    fn follows_same_origin_redirects_with_the_cookie_and_reports_the_final_url() {
        let (address, server) = serve(vec![
            "HTTP/1.1 302 Found\r\nLocation: /final?x=1\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".into(),
            "HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok".into(),
        ]);
        let response = client(address, Some("MoodleSession=a"), 5)
            .get_bytes("/start", 16)
            .unwrap();
        assert_eq!(response.bytes, b"ok");
        assert_eq!(response.url.path(), "/final");
        let seen = server.join().unwrap();
        assert!(seen.iter().all(|r| r.contains("cookie: moodlesession=a")));
        assert!(seen[1].starts_with("get /final?x=1 "));
    }

    #[test]
    fn refuses_cross_origin_redirects_before_sending_the_cookie() {
        let (address, server) = serve(vec![
            "HTTP/1.1 302 Found\r\nLocation: http://127.0.0.2:1/steal\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".into(),
        ]);
        let error = client(address, Some("MoodleSession=a"), 5)
            .get_bytes("/start", 16)
            .err()
            .unwrap();
        assert_eq!(error.code, "NETWORK_ERROR");
        assert!(error.message.contains("cross-origin redirect refused"));
        assert_eq!(server.join().unwrap().len(), 1);
    }

    #[test]
    fn the_timeout_bounds_the_whole_redirect_chain_not_each_hop() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        // Each hop answers well inside the 1s timeout; together they exceed it.
        thread::spawn(move || {
            while let Ok((mut stream, _)) = listener.accept() {
                let _ = stream.read(&mut [0_u8; 4096]);
                thread::sleep(Duration::from_millis(450));
                let _ = stream.write_all(REDIRECT.as_bytes());
            }
        });
        let started = Instant::now();
        let error = client(address, None, 1)
            .get_bytes("/slow", 16)
            .err()
            .unwrap();
        assert_eq!(error.code, "NETWORK_ERROR");
        assert!(started.elapsed() < Duration::from_millis(1600));
    }

    #[test]
    fn head_stops_after_the_cap_on_endless_redirects() {
        let (address, server) = serve(vec![REDIRECT.to_owned(); 6]);
        let error = client(address, None, 5).head("/loop").unwrap_err();
        assert!(error.message.contains("too many redirects"));
        assert_eq!(server.join().unwrap().len(), 6);
    }
}
