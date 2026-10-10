use std::{io::Read, time::Duration};

use crate::url::Url;
use serde_json::Value;
use ureq::Agent;

use crate::{
    error::AppError,
    http::{self, Failure, Follow, Method, Payload, Response},
};

use super::cookies::TransientCookies;

const MAX_BODY: usize = 1024 * 1024;

pub struct SsoTransport {
    client: Agent,
    pub cookies: TransientCookies,
    klms: Url,
    sso: Url,
    document_url: Option<Url>,
}

impl SsoTransport {
    pub fn new(klms: Url, sso: Url, timeout: u64) -> Result<Self, AppError> {
        validate_pair(&klms, &sso)?;
        let client = http::agent(Duration::from_secs(timeout), Some(Duration::from_secs(8)));
        Ok(Self {
            client,
            cookies: TransientCookies::default(),
            klms,
            sso,
            document_url: None,
        })
    }

    pub fn sso_url(&self, path: &str) -> Result<Url, AppError> {
        self.join(&self.sso, path)
    }
    pub fn document_url(&self) -> Option<&Url> {
        self.document_url.as_ref()
    }

    pub fn set_document_url(&mut self, url: Url) -> Result<(), AppError> {
        ensure_allowed(&self.klms, &self.sso, &url)?;
        self.document_url = Some(url);
        Ok(())
    }

    pub fn klms(&self) -> &Url {
        &self.klms
    }

    pub fn is_klms_origin(&self, url: &Url) -> bool {
        url.origin() == self.klms.origin()
    }

    pub fn get_text(&mut self, url: Url) -> Result<(Url, String), AppError> {
        let response = self.send_follow(Method::GET, url, None)?;
        self.document_url = Some(response.url().clone());
        self.text(response)
    }

    pub fn post_form_json(&mut self, url: Url, form: &[(&str, String)]) -> Result<Value, AppError> {
        let response = self.send_single(url, form)?;
        let (_, text) = self.text(response)?;
        serde_json::from_str(&text)
            .map_err(|_| AppError::auth_protocol("KAIST SSO returned invalid JSON"))
    }

    pub fn post_form_follow(
        &mut self,
        url: Url,
        form: &[(&str, String)],
    ) -> Result<(Url, String), AppError> {
        let response = self.send_follow(Method::POST, url, Some(form))?;
        self.document_url = Some(response.url().clone());
        self.text(response)
    }

    fn send_follow(
        &mut self,
        method: Method,
        url: Url,
        form: Option<&[(&str, String)]>,
    ) -> Result<Response, AppError> {
        ensure_allowed(&self.klms, &self.sso, &url)?;
        let mut policy = SsoPolicy {
            cookies: &mut self.cookies,
            document_url: self.document_url.as_ref(),
            klms: &self.klms,
            sso: &self.sso,
            ajax: false,
        };
        http::follow(
            &self.client,
            Follow {
                method,
                url,
                payload: form.map(form_payload),
                max_redirects: 8,
                strict: true,
            },
            &mut policy,
        )
        .map_err(|failure| match failure {
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
        })
    }

    /// An AJAX form POST whose redirects are returned, not followed.
    fn send_single(&mut self, url: Url, form: &[(&str, String)]) -> Result<Response, AppError> {
        ensure_allowed(&self.klms, &self.sso, &url)?;
        let mut policy = SsoPolicy {
            cookies: &mut self.cookies,
            document_url: self.document_url.as_ref(),
            klms: &self.klms,
            sso: &self.sso,
            ajax: true,
        };
        let headers = http::Policy::headers(&mut policy, &Method::POST, &url);
        let response = http::send_once(
            &self.client,
            Method::POST,
            &url,
            &headers,
            Some(&form_payload(form)),
        )
        .map_err(|failure| match failure {
            Failure::Transport(message) => {
                AppError::network(format!("KAIST SSO request failed: {message}"))
            }
            Failure::Refused(error) => error,
            Failure::MissingLocation | Failure::InvalidLocation | Failure::TooManyRedirects => {
                AppError::internal("unexpected redirect failure on a single SSO request")
            }
        })?;
        http::Policy::inspect(&mut policy, &response)?;
        Ok(response)
    }

    fn text(&self, response: Response) -> Result<(Url, String), AppError> {
        if response
            .content_length()
            .is_some_and(|len| len > MAX_BODY as u64)
        {
            return Err(AppError::limit("KAIST SSO response exceeded 1 MiB"));
        }
        let url = response.url().clone();
        let mut bytes = Vec::new();
        response
            .take(MAX_BODY as u64 + 1)
            .read_to_end(&mut bytes)
            .map_err(|error| AppError::network(format!("failed to read SSO response: {error}")))?;
        if bytes.len() > MAX_BODY {
            return Err(AppError::limit("KAIST SSO response exceeded 1 MiB"));
        }
        Ok((url, String::from_utf8_lossy(&bytes).into_owned()))
    }

    fn join(&self, base: &Url, path: &str) -> Result<Url, AppError> {
        let url = base
            .join(path)
            .map_err(|_| AppError::internal("invalid built-in SSO path"))?;
        ensure_allowed(&self.klms, &self.sso, &url)?;
        Ok(url)
    }
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

fn form_payload(form: &[(&str, String)]) -> Payload {
    let bytes = crate::url::form_urlencode(form.iter().map(|(key, value)| (*key, value.as_str())))
        .into_bytes();
    Payload {
        content_type: "application/x-www-form-urlencoded",
        bytes,
    }
}

/// Per-hop SSO rules: trusted origins only, Referer/Origin from the current
/// document, cookies recomputed per URL and captured from every response.
struct SsoPolicy<'a> {
    cookies: &'a mut TransientCookies,
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
            let origin = self
                .document_url
                .unwrap_or(url)
                .origin()
                .ascii_serialization();
            headers.push(("origin", origin));
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

fn validate_pair(klms: &Url, sso: &Url) -> Result<(), AppError> {
    let production = klms.scheme() == "https"
        && klms.host_str() == Some("klms.kaist.ac.kr")
        && sso.scheme() == "https"
        && sso.host_str() == Some("sso.kaist.ac.kr");
    let loopback = [klms, sso].iter().all(|url| {
        url.scheme() == "http" && matches!(url.host_str(), Some("localhost" | "127.0.0.1" | "::1"))
    });
    if !production && !loopback {
        return Err(AppError::config(
            "native login permits only KAIST production origins or loopback test origins",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{io::Write, net::TcpListener, thread};

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
        let error = transport.get_text(url).unwrap_err();
        assert_eq!(error.code, "LIMIT_EXCEEDED");
        server.join().unwrap();
    }
}
