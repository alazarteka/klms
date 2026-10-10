//! The one blocking HTTP transport. The library never follows redirects:
//! every caller supplies its own origin policy to [`follow`].
use std::{io, time::Duration};

use ureq::{
    Agent, BodyReader,
    config::Config,
    http::{self},
    tls::{RootCerts, TlsConfig},
};
use url::Url;

use crate::error::AppError;

pub use ureq::http::{HeaderMap, HeaderValue, Method, StatusCode, header::SET_COOKIE};

pub const USER_AGENT: &str = concat!("klms/", env!("CARGO_PKG_VERSION"));

/// Builds an agent that reports every HTTP status as a normal response and
/// leaves redirects to the caller. Proxy settings come from the environment
/// (`HTTPS_PROXY`, `NO_PROXY`, ...) and trust roots from the platform store.
pub fn agent(timeout: Duration, connect_timeout: Option<Duration>) -> Agent {
    let tls = TlsConfig::builder()
        .root_certs(RootCerts::PlatformVerifier)
        .build();
    let config: Config = Agent::config_builder()
        .timeout_global(Some(timeout))
        .timeout_connect(connect_timeout)
        .user_agent(USER_AGENT)
        .max_redirects(0)
        .max_redirects_will_error(false)
        .http_status_as_error(false)
        .tls_config(tls)
        .build();
    Agent::new_with_config(config)
}

/// A response whose body is read on demand through [`io::Read`].
pub struct Response {
    url: Url,
    status: u16,
    headers: HeaderMap,
    reader: BodyReader<'static>,
}

impl Response {
    pub fn url(&self) -> &Url {
        &self.url
    }

    pub fn status(&self) -> u16 {
        self.status
    }

    pub fn headers(&self) -> &HeaderMap {
        &self.headers
    }

    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.get(name).and_then(|value| value.to_str().ok())
    }

    pub fn content_length(&self) -> Option<u64> {
        self.header("content-length")
            .and_then(|value| value.parse().ok())
    }
}

impl io::Read for Response {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.reader.read(buf)
    }
}

pub struct Payload {
    pub content_type: &'static str,
    pub bytes: Vec<u8>,
}

#[derive(Debug)]
pub enum Failure {
    /// Connection, TLS, timeout or malformed-request failure (URL redacted).
    Transport(String),
    MissingLocation,
    InvalidLocation,
    TooManyRedirects,
    /// The caller's policy rejected a redirect target or a hop's response.
    Refused(AppError),
}

/// One request without redirect handling. The status may be any HTTP status.
pub fn send_once(
    agent: &Agent,
    method: Method,
    url: &Url,
    headers: &[(&'static str, String)],
    payload: Option<&Payload>,
) -> Result<Response, Failure> {
    let transport = |message: String| Failure::Transport(message.replace(url.as_str(), "<url>"));
    let mut builder = http::Request::builder().method(method).uri(url.as_str());
    for (name, value) in headers {
        builder = builder.header(*name, value.as_str());
    }
    let result = match payload {
        Some(payload) => {
            let request = builder
                .header("content-type", payload.content_type)
                .body(payload.bytes.clone())
                .map_err(|error| transport(error.to_string()))?;
            agent.run(request)
        }
        None => {
            let request = builder
                .body(())
                .map_err(|error| transport(error.to_string()))?;
            agent.run(request)
        }
    };
    let response = result.map_err(|error| transport(error.to_string()))?;
    let (parts, body) = response.into_parts();
    Ok(Response {
        url: url.clone(),
        status: parts.status.as_u16(),
        headers: parts.headers,
        reader: body.into_reader(),
    })
}

/// Per-hop behavior supplied by the caller of [`follow`].
pub trait Policy {
    /// Extra headers for one hop, computed from that hop's method and URL.
    fn headers(&mut self, method: &Method, url: &Url) -> Vec<(&'static str, String)>;
    /// Origin policy applied to every redirect target before it is requested.
    fn allow(&mut self, url: &Url) -> Result<(), AppError>;
    /// Observes every hop's response, redirects included.
    fn inspect(&mut self, _response: &Response) -> Result<(), AppError> {
        Ok(())
    }
}

pub struct Follow {
    pub method: Method,
    pub url: Url,
    pub payload: Option<Payload>,
    /// Redirects followed before [`Failure::TooManyRedirects`].
    pub max_redirects: usize,
    /// Treat any 3xx as a redirect (and require `Location`) instead of only
    /// 301/302/303/307/308 responses that carry one.
    pub strict: bool,
}

/// Sends the request and follows redirects by hand. 307/308 keep the method
/// and body; every other redirect becomes a body-less GET.
pub fn follow(
    agent: &Agent,
    request: Follow,
    policy: &mut dyn Policy,
) -> Result<Response, Failure> {
    let Follow {
        mut method,
        mut url,
        mut payload,
        max_redirects,
        strict,
    } = request;
    let mut redirects = 0;
    loop {
        let extra = policy.headers(&method, &url);
        let response = send_once(agent, method.clone(), &url, &extra, payload.as_ref())?;
        policy.inspect(&response).map_err(Failure::Refused)?;
        let status = response.status();
        let redirect = if strict {
            (300..400).contains(&status)
        } else {
            matches!(status, 301 | 302 | 303 | 307 | 308)
        };
        if !redirect {
            return Ok(response);
        }
        let Some(location) = response.header("location").map(str::to_owned) else {
            if strict {
                return Err(Failure::MissingLocation);
            }
            return Ok(response);
        };
        if redirects >= max_redirects {
            return Err(Failure::TooManyRedirects);
        }
        redirects += 1;
        let next = url.join(&location).map_err(|_| Failure::InvalidLocation)?;
        policy.allow(&next).map_err(Failure::Refused)?;
        if !matches!(status, 307 | 308) {
            method = Method::GET;
            payload = None;
        }
        url = next;
    }
}
