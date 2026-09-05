use std::{io::Read, time::Duration};

use reqwest::{
    Method,
    blocking::{Client, Response},
    header::{CONTENT_LENGTH, COOKIE, LOCATION, ORIGIN, REFERER},
    redirect::Policy,
};
use serde_json::Value;
use url::Url;

use crate::error::AppError;

use super::cookies::TransientCookies;

const MAX_BODY: usize = 1024 * 1024;

pub struct SsoTransport {
    client: Client,
    pub cookies: TransientCookies,
    klms: Url,
    sso: Url,
    document_url: Option<Url>,
}

impl SsoTransport {
    pub fn new(klms: Url, sso: Url, timeout: u64) -> Result<Self, AppError> {
        validate_pair(&klms, &sso)?;
        let client = Client::builder()
            .timeout(Duration::from_secs(timeout))
            .connect_timeout(Duration::from_secs(8))
            .redirect(Policy::none())
            .user_agent(concat!("klms/", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(|error| AppError::internal(format!("failed to create SSO client: {error}")))?;
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
    pub fn klms(&self) -> &Url {
        &self.klms
    }

    pub fn is_klms_origin(&self, url: &Url) -> bool {
        origin(url) == origin(&self.klms)
    }

    pub fn get_text(&mut self, url: Url) -> Result<(Url, String), AppError> {
        let response = self.send_follow(Method::GET, url, None)?;
        self.document_url = Some(response.url().clone());
        self.text(response)
    }

    pub fn post_form_json(&mut self, url: Url, form: &[(&str, String)]) -> Result<Value, AppError> {
        let response = self.send(Method::POST, url, Some(form), true)?;
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
        mut method: Method,
        mut url: Url,
        mut form: Option<&[(&str, String)]>,
    ) -> Result<Response, AppError> {
        for _ in 0..=8 {
            let response = self.send(method.clone(), url.clone(), form, false)?;
            if !response.status().is_redirection() {
                return Ok(response);
            }
            let location = response
                .headers()
                .get(LOCATION)
                .and_then(|value| value.to_str().ok())
                .ok_or_else(|| {
                    AppError::auth_protocol("KAIST SSO returned a redirect without Location")
                })?;
            let next = url
                .join(location)
                .map_err(|_| AppError::auth_protocol("KAIST SSO returned an invalid redirect"))?;
            self.ensure_allowed(&next)?;
            let preserve = matches!(response.status().as_u16(), 307 | 308);
            url = next;
            if !preserve {
                method = Method::GET;
                form = None;
            }
        }
        Err(AppError::auth_protocol(
            "KAIST SSO exceeded the redirect limit",
        ))
    }

    fn send(
        &mut self,
        method: Method,
        url: Url,
        form: Option<&[(&str, String)]>,
        ajax: bool,
    ) -> Result<Response, AppError> {
        self.ensure_allowed(&url)?;
        let is_post = method == Method::POST;
        let mut request = self.client.request(method, url.clone());
        if let Some(referer) = &self.document_url {
            request = request.header(REFERER, referer.as_str());
        }
        if is_post {
            let origin = self
                .document_url
                .as_ref()
                .unwrap_or(&url)
                .origin()
                .ascii_serialization();
            request = request.header(ORIGIN, origin);
        }
        if let Some(cookie) = self.cookies.header(&url) {
            request = request.header(COOKIE, cookie);
        }
        if ajax {
            request = request.header("X-Requested-With", "XMLHttpRequest");
        }
        if let Some(form) = form {
            request = request.form(form);
        }
        let response = request.send().map_err(|error| {
            AppError::network(format!("KAIST SSO request failed: {}", error.without_url()))
        })?;
        self.cookies.capture(response.url(), response.headers())?;
        if !(response.status().is_success() || response.status().is_redirection()) {
            return Err(AppError::network(format!(
                "KAIST SSO returned HTTP {}",
                response.status().as_u16()
            )));
        }
        Ok(response)
    }

    fn text(&self, response: Response) -> Result<(Url, String), AppError> {
        if response
            .headers()
            .get(CONTENT_LENGTH)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse::<usize>().ok())
            .is_some_and(|len| len > MAX_BODY)
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
        self.ensure_allowed(&url)?;
        Ok(url)
    }

    fn ensure_allowed(&self, url: &Url) -> Result<(), AppError> {
        if origin(url) != origin(&self.klms) && origin(url) != origin(&self.sso) {
            return Err(AppError::auth_protocol(
                "KAIST SSO attempted a redirect to an untrusted origin",
            ));
        }
        if !url.username().is_empty() || url.password().is_some() {
            return Err(AppError::auth_protocol("SSO URL contained userinfo"));
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

fn origin(url: &Url) -> (String, String, u16) {
    (
        url.scheme().into(),
        url.host_str().unwrap_or_default().into(),
        url.port_or_known_default().unwrap_or(0),
    )
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
