//! The HTTP side of a login: only KLMS and SSO origins are ever contacted,
//! redirects are followed by hand, and cookies live in a per-login jar.

use std::{io::Read, time::Duration};

use serde_json::Value;
use ureq::Agent;

use crate::{
    error::AppError,
    http::{self, Failure, Follow, Method, Payload, Response},
    url::Url,
};

use super::cookies::Jar;

const MAX_BODY: usize = 1024 * 1024;

pub struct SsoTransport {
    client: Agent,
    pub cookies: Jar,
    klms: Url,
    sso: Url,
    document_url: Option<Url>,
}

impl SsoTransport {
    pub fn new(klms: Url, sso: Url, timeout: u64) -> Result<Self, AppError> {
        let production = klms.scheme() == "https"
            && klms.host_str() == Some("klms.kaist.ac.kr")
            && sso.scheme() == "https"
            && sso.host_str() == Some("sso.kaist.ac.kr");
        let loopback = klms.is_http_loopback() && sso.is_http_loopback();
        if !production && !loopback {
            return Err(AppError::config(
                "native login permits only KAIST production origins or loopback test origins",
            ));
        }
        Ok(Self {
            client: http::agent(Duration::from_secs(timeout), Some(Duration::from_secs(8))),
            cookies: Jar::default(),
            klms,
            sso,
            document_url: None,
        })
    }

    pub fn klms(&self) -> &Url {
        &self.klms
    }

    pub fn document_url(&self) -> Option<&Url> {
        self.document_url.as_ref()
    }

    pub fn set_document_url(&mut self, url: Url) -> Result<(), AppError> {
        ensure_allowed(&self.klms, &self.sso, &url)?;
        self.document_url = Some(url);
        Ok(())
    }

    pub fn is_klms_origin(&self, url: &Url) -> bool {
        url.origin() == self.klms.origin()
    }

    /// Resolve `reference` against the SSO origin; only trusted origins pass.
    pub fn url(&self, reference: &str) -> Result<Url, AppError> {
        let url = self
            .sso
            .join(reference)
            .map_err(|_| AppError::internal("invalid SSO URL"))?;
        ensure_allowed(&self.klms, &self.sso, &url)?;
        Ok(url)
    }

    pub fn get(&mut self, reference: &str) -> Result<(Url, String), AppError> {
        self.fetch(Method::GET, reference, None, false)
    }

    pub fn post(
        &mut self,
        reference: &str,
        form: &[(&str, &str)],
    ) -> Result<(Url, String), AppError> {
        self.fetch(Method::POST, reference, Some(form), false)
    }

    /// An AJAX form POST answered with JSON; its redirects are not followed.
    pub fn ajax(&mut self, reference: &str, form: &[(&str, &str)]) -> Result<Value, AppError> {
        let (_, body) = self.fetch(Method::POST, reference, Some(form), true)?;
        serde_json::from_str(&body)
            .map_err(|_| AppError::auth_protocol("KAIST SSO returned invalid JSON"))
    }

    fn fetch(
        &mut self,
        method: Method,
        reference: &str,
        form: Option<&[(&str, &str)]>,
        ajax: bool,
    ) -> Result<(Url, String), AppError> {
        let url = self.url(reference)?;
        let payload = form.map(|form| Payload {
            content_type: "application/x-www-form-urlencoded",
            bytes: crate::url::form_urlencode(form.iter().copied()).into_bytes(),
        });
        let mut policy = SsoPolicy {
            cookies: &mut self.cookies,
            document_url: self.document_url.as_ref(),
            klms: &self.klms,
            sso: &self.sso,
            ajax,
        };
        // AJAX answers are never redirected: any redirect is a protocol error.
        let request = Follow {
            method,
            url,
            payload,
            max_redirects: if ajax { 0 } else { 8 },
            strict: !ajax,
        };
        let response = http::follow(&self.client, request, &mut policy).map_err(failure)?;
        if !ajax {
            self.document_url = Some(response.url().clone());
        }
        text(response)
    }
}

fn failure(failure: Failure) -> AppError {
    match failure {
        Failure::Transport(message) => {
            AppError::network(format!("KAIST SSO request failed: {message}"))
        }
        Failure::Refused(error) => error,
        Failure::MissingLocation => {
            AppError::auth_protocol("KAIST SSO returned a redirect without Location")
        }
        Failure::InvalidLocation => {
            AppError::auth_protocol("KAIST SSO returned an invalid redirect")
        }
        Failure::TooManyRedirects => {
            AppError::auth_protocol("KAIST SSO exceeded the redirect limit")
        }
    }
}

/// The response URL and its body, capped at 1 MiB.
fn text(response: Response) -> Result<(Url, String), AppError> {
    let too_big = || AppError::limit("KAIST SSO response exceeded 1 MiB");
    if response
        .content_length()
        .is_some_and(|len| len > MAX_BODY as u64)
    {
        return Err(too_big());
    }
    let url = response.url().clone();
    let mut bytes = Vec::new();
    response
        .take(MAX_BODY as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| AppError::network(format!("failed to read SSO response: {error}")))?;
    if bytes.len() > MAX_BODY {
        return Err(too_big());
    }
    Ok((url, String::from_utf8_lossy(&bytes).into_owned()))
}

fn ensure_allowed(klms: &Url, sso: &Url, url: &Url) -> Result<(), AppError> {
    if url.origin() != klms.origin() && url.origin() != sso.origin() {
        return Err(AppError::auth_protocol(
            "KAIST SSO attempted a redirect to an untrusted origin",
        ));
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(AppError::auth_protocol("SSO URL contained userinfo"));
    }
    Ok(())
}

/// Per-hop SSO rules: trusted origins only, Referer/Origin from the current
/// document, cookies recomputed per URL and captured from every response.
struct SsoPolicy<'a> {
    cookies: &'a mut Jar,
    document_url: Option<&'a Url>,
    klms: &'a Url,
    sso: &'a Url,
    ajax: bool,
}

impl http::Policy for SsoPolicy<'_> {
    fn headers(&mut self, method: &Method, url: &Url) -> Vec<(&'static str, String)> {
        let mut headers = Vec::new();
        if let Some(referer) = self.document_url {
            headers.push(("referer", referer.as_str().to_owned()));
        }
        if *method == Method::POST {
            let origin = self.document_url.unwrap_or(url).origin();
            headers.push(("origin", origin.ascii_serialization()));
        }
        if let Some(cookie) = self.cookies.header(url) {
            headers.push(("cookie", cookie));
        }
        if self.ajax {
            headers.push(("x-requested-with", "XMLHttpRequest".to_owned()));
        }
        headers
    }

    fn allow(&mut self, url: &Url) -> Result<(), AppError> {
        ensure_allowed(self.klms, self.sso, url)
    }

    fn inspect(&mut self, response: &Response) -> Result<(), AppError> {
        self.cookies.capture(response.url(), response.headers())?;
        if !(200..400).contains(&response.status()) {
            return Err(AppError::network(format!(
                "KAIST SSO returned HTTP {}",
                response.status()
            )));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::{io::Write, net::TcpListener, thread};

    use super::*;

    #[test]
    fn bounds_sso_response_without_content_length_before_eof() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = Url::parse(&format!("http://{}", listener.local_addr().unwrap())).unwrap();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut request = [0; 4096];
            let mut received = 0;
            while !request[..received]
                .windows(4)
                .any(|bytes| bytes == b"\r\n\r\n")
            {
                let read = stream.read(&mut request[received..]).unwrap();
                assert!(read > 0, "fixture request headers were incomplete");
                received += read;
            }
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nConnection: close\r\n\r\n")
                .unwrap();
            // Keep the connection open after the cap. An unbounded reader would
            // wait for EOF and fail with a timeout instead of the size limit.
            let _ = stream.write_all(&vec![b'x'; MAX_BODY + 1]);
            let _ = stream.read(&mut request);
        });
        let mut transport = SsoTransport::new(url.clone(), url.clone(), 2).unwrap();
        let error = transport.get(url.as_str()).unwrap_err();
        assert_eq!(error.code, "LIMIT_EXCEEDED");
        server.join().unwrap();
    }
}
