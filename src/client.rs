use std::{
    io::{Read, Write},
    time::Duration,
};

use reqwest::{
    blocking::Client,
    header::{CONTENT_LENGTH, CONTENT_TYPE, COOKIE, HeaderValue},
    redirect::{Attempt, Policy},
};
use url::Url;

use crate::error::AppError;

const MAX_BODY_BYTES: usize = 8 * 1024 * 1024;

pub struct KlmsClient {
    base_url: Url,
    http: Client,
    cookie: HeaderValue,
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

impl KlmsClient {
    pub fn new(
        base: &str,
        cookie_header: Option<&str>,
        timeout_seconds: u64,
    ) -> Result<Self, AppError> {
        let base_url = validate_base_url(base)?;
        let expected_origin = origin(&base_url);
        let policy = Policy::custom(move |attempt: Attempt<'_>| {
            if attempt.previous().len() >= 5 {
                return attempt.error("too many redirects");
            }
            if origin(attempt.url()) != expected_origin {
                return attempt.error("cross-origin redirect refused");
            }
            if !attempt.url().username().is_empty() || attempt.url().password().is_some() {
                return attempt.error("URL userinfo refused");
            }
            attempt.follow()
        });
        let http = Client::builder()
            .timeout(Duration::from_secs(timeout_seconds))
            .connect_timeout(Duration::from_secs(8))
            .redirect(policy)
            .user_agent(concat!("klms/", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(|error| {
                AppError::internal(format!("failed to create HTTP client: {error}"))
            })?;
        let cookie = HeaderValue::from_str(cookie_header.unwrap_or("")).map_err(|_| {
            AppError::config("storage state contains a cookie value invalid for HTTP headers")
        })?;
        Ok(Self {
            base_url,
            http,
            cookie,
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
        let final_url = response.url().clone();
        if response
            .headers()
            .get(CONTENT_LENGTH)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.parse::<usize>().ok())
            .is_some_and(|length| length > max_bytes)
        {
            return Err(AppError::limit(format!(
                "KLMS response exceeded the {max_bytes} byte limit"
            )));
        }
        let content_type = content_type(&response);
        let bytes = read_bounded(&mut response, max_bytes, false)?;
        check_logged_out(&final_url, content_type.as_deref(), &bytes)?;
        Ok(ByteResponse {
            url: final_url,
            bytes,
        })
    }

    pub fn get_preview(&self, path: &str, max_bytes: usize) -> Result<PreviewResponse, AppError> {
        let mut response = self.send_get(path)?;
        let final_url = response.url().clone();
        let content_type = content_type(&response);
        let bytes = read_bounded(&mut response, max_bytes, true)?;
        let truncated = bytes.len() > max_bytes;
        let bytes = if truncated {
            bytes[..max_bytes].to_vec()
        } else {
            bytes
        };
        check_logged_out(&final_url, content_type.as_deref(), &bytes)?;
        Ok(PreviewResponse {
            url: final_url,
            content_type,
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
        let mut response = self.send_get(path)?;
        let final_url = response.url().clone();
        if response
            .headers()
            .get(CONTENT_LENGTH)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.parse::<usize>().ok())
            .is_some_and(|length| length > max_bytes)
        {
            return Err(AppError::limit(format!(
                "KLMS download exceeded the {max_bytes} byte limit"
            )));
        }
        let content_type = content_type(&response);
        let mut sample = Vec::new();
        let mut buffer = [0_u8; 64 * 1024];
        let mut total = 0_usize;
        loop {
            let read = response
                .read(&mut buffer)
                .map_err(|error| AppError::network(format!("failed to read download: {error}")))?;
            if read == 0 {
                break;
            }
            total = total
                .checked_add(read)
                .ok_or_else(|| AppError::network("download size overflow"))?;
            if total > max_bytes {
                return Err(AppError::limit(format!(
                    "KLMS download exceeded the {max_bytes} byte limit"
                )));
            }
            if sample.len() < 64 * 1024 {
                let keep = read.min(64 * 1024 - sample.len());
                sample.extend_from_slice(&buffer[..keep]);
            }
            writer
                .write_all(&buffer[..read])
                .map_err(|error| AppError::config(format!("failed to write download: {error}")))?;
        }
        check_logged_out(&final_url, content_type.as_deref(), &sample)?;
        Ok(DownloadResponse {
            url: final_url,
            content_type,
            bytes: total,
        })
    }

    fn send_get(&self, path: &str) -> Result<reqwest::blocking::Response, AppError> {
        let url = self
            .base_url
            .join(path)
            .map_err(|error| AppError::config(format!("invalid KLMS path: {error}")))?;
        if origin(&url) != origin(&self.base_url) {
            return Err(AppError::config("cross-origin request path refused"));
        }
        if !url.username().is_empty() || url.password().is_some() {
            return Err(AppError::config("request URL must not contain userinfo"));
        }
        let mut request = self.http.get(url);
        if !self.cookie.is_empty() {
            request = request.header(COOKIE, self.cookie.clone());
        }
        let response = request.send().map_err(|error| {
            AppError::network(format!("KLMS request failed: {}", error.without_url()))
        })?;
        let status = response.status();
        let final_url = response.url().clone();
        if !status.is_success() {
            return Err(AppError::http(status.as_u16(), final_url.path()));
        }
        Ok(response)
    }

    pub fn ajax(&self, sesskey: &str, method: &'static str) -> Result<serde_json::Value, AppError> {
        const ALLOWED: &[&str] = &["core_session_time_remaining", "core_session_touch"];
        if !ALLOWED.contains(&method) {
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
        let payload = serde_json::json!([{"index": 0, "methodname": method, "args": {}}]);
        let body = serde_json::to_vec(&payload).map_err(|error| {
            AppError::internal(format!("failed to encode AJAX request: {error}"))
        })?;
        let mut response = self
            .http
            .post(url)
            .header(COOKIE, self.cookie.clone())
            .header("X-Requested-With", "XMLHttpRequest")
            .header(CONTENT_TYPE, "application/json")
            .body(body)
            .send()
            .map_err(|error| {
                AppError::network(format!("KLMS AJAX request failed: {}", error.without_url()))
            })?;
        if !response.status().is_success() {
            return Err(AppError::http(
                response.status().as_u16(),
                "/lib/ajax/service.php",
            ));
        }
        if response
            .headers()
            .get(CONTENT_LENGTH)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.parse::<usize>().ok())
            .is_some_and(|length| length > MAX_BODY_BYTES)
        {
            return Err(AppError::limit(
                "KLMS AJAX response exceeded the 8 MiB limit",
            ));
        }
        let mut response_body = Vec::with_capacity(64 * 1024);
        response
            .by_ref()
            .take(MAX_BODY_BYTES as u64 + 1)
            .read_to_end(&mut response_body)
            .map_err(|error| {
                AppError::network(format!("failed to read KLMS AJAX response: {error}"))
            })?;
        if response_body.len() > MAX_BODY_BYTES {
            return Err(AppError::limit(
                "KLMS AJAX response exceeded the 8 MiB limit",
            ));
        }
        let value: serde_json::Value = serde_json::from_slice(&response_body)
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
            if lower.contains("session") || lower.contains("sesskey") || lower.contains("login") {
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

fn content_type(response: &reqwest::blocking::Response) -> Option<String> {
    response
        .headers()
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned)
}

fn read_bounded(
    response: &mut reqwest::blocking::Response,
    max_bytes: usize,
    allow_truncation: bool,
) -> Result<Vec<u8>, AppError> {
    let mut body = Vec::with_capacity(max_bytes.min(64 * 1024));
    response
        .by_ref()
        .take(max_bytes as u64 + 1)
        .read_to_end(&mut body)
        .map_err(|error| AppError::network(format!("failed to read KLMS response: {error}")))?;
    if body.len() > max_bytes && !allow_truncation {
        return Err(AppError::limit(format!(
            "KLMS response exceeded the {max_bytes} byte limit"
        )));
    }
    Ok(body)
}

fn check_logged_out(url: &Url, content_type: Option<&str>, bytes: &[u8]) -> Result<(), AppError> {
    if url.path().to_ascii_lowercase().contains("/login/") {
        return Err(expired_session());
    }
    let leading = String::from_utf8_lossy(&bytes[..bytes.len().min(512)]).to_ascii_lowercase();
    let looks_html = content_type.is_some_and(|value| value.to_ascii_lowercase().contains("html"))
        || leading.contains("<!doctype html")
        || leading.contains("<html");
    if looks_html {
        let text = String::from_utf8_lossy(bytes);
        if looks_logged_out(url, &text) {
            return Err(expired_session());
        }
    }
    Ok(())
}

fn expired_session() -> AppError {
    AppError::auth_required("the saved KLMS session is missing or expired")
}

pub fn validate_base_url(value: &str) -> Result<Url, AppError> {
    let mut url = Url::parse(value)
        .map_err(|error| AppError::config(format!("invalid KLMS base URL: {error}")))?;
    if !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(AppError::config(
            "KLMS base URL must not contain credentials, query, or fragment",
        ));
    }
    let host = url
        .host_str()
        .ok_or_else(|| AppError::config("KLMS base URL has no host"))?;
    let loopback = host == "localhost" || host == "127.0.0.1" || host == "::1";
    if url.scheme() != "https" && !(url.scheme() == "http" && loopback) {
        return Err(AppError::config(
            "KLMS base URL must use HTTPS (HTTP is loopback-only)",
        ));
    }
    url.set_path("/");
    Ok(url)
}

fn origin(url: &Url) -> (String, Option<String>, Option<u16>) {
    (
        url.scheme().into(),
        url.host_str().map(str::to_owned),
        url.port_or_known_default(),
    )
}

fn looks_logged_out(url: &Url, html: &str) -> bool {
    let path = url.path().to_ascii_lowercase();
    let lower = html.to_ascii_lowercase();
    path.contains("/login/")
        || lower.contains("name=\"username\"") && lower.contains("name=\"password\"")
        || lower.contains("id=\"loginbtn\"")
}

#[cfg(test)]
mod tests {
    use super::validate_base_url;

    #[test]
    fn accepts_https_and_loopback_http_only() {
        assert!(validate_base_url("https://klms.kaist.ac.kr").is_ok());
        assert!(validate_base_url("http://127.0.0.1:9999").is_ok());
        assert!(validate_base_url("http://example.com").is_err());
        assert!(validate_base_url("https://user@example.com").is_err());
    }
}
