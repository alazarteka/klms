use std::time::Duration;

use reqwest::{
    blocking::Client,
    header::{CONTENT_LENGTH, COOKIE, HeaderValue},
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

impl KlmsClient {
    pub fn new(base: &str, cookie_header: Option<&str>) -> Result<Self, AppError> {
        let base_url = validate_base_url(base)?;
        let expected_origin = origin(&base_url);
        let policy = Policy::custom(move |attempt: Attempt<'_>| {
            if attempt.previous().len() >= 5 {
                return attempt.error("too many redirects");
            }
            if origin(attempt.url()) != expected_origin {
                return attempt.error("cross-origin redirect refused");
            }
            attempt.follow()
        });
        let http = Client::builder()
            .timeout(Duration::from_secs(20))
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
        let url = self
            .base_url
            .join(path)
            .map_err(|error| AppError::config(format!("invalid KLMS path: {error}")))?;
        if origin(&url) != origin(&self.base_url) {
            return Err(AppError::config("cross-origin request path refused"));
        }
        let mut request = self.http.get(url);
        if !self.cookie.is_empty() {
            request = request.header(COOKIE, self.cookie.clone());
        }
        let response = request
            .send()
            .map_err(|error| AppError::network(format!("KLMS request failed: {error}")))?;
        let status = response.status();
        let final_url = response.url().clone();
        if !status.is_success() {
            return Err(AppError::network(format!(
                "KLMS returned HTTP {status} for {}",
                final_url.path()
            )));
        }
        if response
            .headers()
            .get(CONTENT_LENGTH)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.parse::<usize>().ok())
            .is_some_and(|length| length > MAX_BODY_BYTES)
        {
            return Err(AppError::network("KLMS response exceeded the 8 MiB limit"));
        }
        let body = response
            .bytes()
            .map_err(|error| AppError::network(format!("failed to read KLMS response: {error}")))?;
        if body.len() > MAX_BODY_BYTES {
            return Err(AppError::network("KLMS response exceeded the 8 MiB limit"));
        }
        let text = String::from_utf8_lossy(&body).into_owned();
        if looks_logged_out(&final_url, &text) {
            return Err(AppError::auth(
                "the saved KLMS session is missing or expired",
                "Sign in with kaist-cli once, or set KLMS_STORAGE_STATE to a fresh Playwright storage-state file.",
            ));
        }
        Ok(HtmlResponse {
            url: final_url,
            text,
        })
    }
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
