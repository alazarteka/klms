//! The transient SSO cookie jar and the single cookie-acceptance predicate
//! shared by capture and storage.

use serde::{Deserialize, Serialize};

use crate::{
    error::AppError,
    http::{HeaderMap, SET_COOKIE},
    url::Url,
};

use super::store::StoredCookie;

/// Whether `name` is an RFC 6265 cookie name (an RFC 2616 token).
pub fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .bytes()
            .all(|byte| (0x21..0x7f).contains(&byte) && !b"()<>@,;:\\\"/[]?={}".contains(&byte))
}

/// A cookie is storable when its name is a token and its value is 1..=4096
/// printable ASCII bytes other than `;`: nothing that could break out of a
/// `Cookie` header, while still accepting everything earlier releases saved.
/// An empty value never passes: servers send one to delete a cookie.
pub fn valid_cookie(name: &str, value: &str) -> bool {
    valid_name(name)
        && !value.is_empty()
        && value.len() <= 4096
        && value
            .bytes()
            .all(|byte| (0x20..0x7f).contains(&byte) && byte != b';')
}

fn valid_device(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 512
        && value
            .bytes()
            .all(|byte| (0x21..0x7f).contains(&byte) && byte != b';')
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Cookie {
    name: String,
    value: String,
    domain: String,
    path: String,
    secure: bool,
    /// Serialized origin of the response that set it.
    source_origin: String,
}

/// Cookies set during one login, plus trusted-device identifiers. It is also
/// the on-disk form inside a pending login (re-checked by [`Jar::checked`]).
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Jar {
    cookies: Vec<Cookie>,
    devices: Vec<String>,
}

fn in_domain(host: &str, domain: &str) -> bool {
    host == domain || host.ends_with(&format!(".{domain}"))
}

impl Jar {
    pub fn capture(&mut self, url: &Url, headers: &HeaderMap) -> Result<(), AppError> {
        for value in headers.get_all(SET_COOKIE) {
            let value = value
                .to_str()
                .map_err(|_| AppError::auth_protocol("SSO returned a non-text cookie"))?;
            self.capture_one(url, value)?;
        }
        Ok(())
    }

    fn capture_one(&mut self, url: &Url, header: &str) -> Result<(), AppError> {
        let host = url
            .host_str()
            .ok_or_else(|| AppError::auth_protocol("SSO response URL has no host"))?;
        let mut parts = header.split(';');
        let (name, value) = parts
            .next()
            .unwrap_or_default()
            .split_once('=')
            .ok_or_else(|| AppError::auth_protocol("SSO returned a malformed cookie"))?;
        // An empty value is how a server deletes a cookie; it is never stored.
        let mut remove = value.is_empty();
        if !(if remove {
            valid_name(name)
        } else {
            valid_cookie(name, value)
        }) {
            return Err(AppError::auth_protocol("SSO returned an unsafe cookie"));
        }
        let mut domain = host.to_ascii_lowercase();
        let mut path = match url.path().rfind('/') {
            Some(index) if index > 0 => url.path()[..index].to_owned(),
            _ => "/".into(),
        };
        let mut secure = false;
        for attribute in parts {
            let attribute = attribute.trim();
            let (key, attr) = attribute.split_once('=').unwrap_or((attribute, ""));
            match key.to_ascii_lowercase().as_str() {
                "domain" => {
                    let candidate = attr.trim_start_matches('.').to_ascii_lowercase();
                    if candidate.is_empty() || !in_domain(host, &candidate) {
                        return Err(AppError::auth_protocol(
                            "SSO attempted to set a cookie for an unrelated domain",
                        ));
                    }
                    domain = candidate;
                }
                "path" if attr.starts_with('/') => path = attr.to_owned(),
                "secure" => secure = true,
                "max-age" if attr.parse::<i64>().is_ok_and(|age| age <= 0) => remove = true,
                _ => {}
            }
        }
        self.cookies.retain(|cookie| {
            !(cookie.name == name && cookie.domain == domain && cookie.path == path)
        });
        if !remove {
            self.cookies.push(Cookie {
                name: name.into(),
                value: value.into(),
                domain,
                path,
                secure,
                source_origin: url.origin().ascii_serialization(),
            });
        }
        Ok(())
    }

    /// Re-check a jar read from disk, which is untrusted input.
    pub fn checked(self) -> Result<Self, AppError> {
        let sound = self.cookies.iter().all(|saved| {
            valid_cookie(&saved.name, &saved.value)
                && !saved.domain.is_empty()
                && saved.path.starts_with('/')
                && !saved
                    .domain
                    .contains(|c: char| c.is_whitespace() || c == ';')
                && Url::parse(&saved.source_origin).is_ok()
        }) && self.devices.iter().all(|device| valid_device(device));
        if sound {
            Ok(self)
        } else {
            Err(AppError::config(
                "saved login state contains an invalid cookie",
            ))
        }
    }

    /// The `Cookie` header for a request to `url`.
    pub fn header(&self, url: &Url) -> Option<String> {
        let host = url.host_str()?;
        let values = self
            .cookies
            .iter()
            .filter(|cookie| {
                let path = url.path();
                in_domain(host, &cookie.domain)
                    && (path == cookie.path
                        || path.starts_with(&format!("{}/", cookie.path.trim_end_matches('/'))))
                    && (!cookie.secure || url.scheme() == "https")
            })
            .map(|cookie| format!("{}={}", cookie.name, cookie.value))
            .collect::<Vec<_>>();
        (!values.is_empty()).then(|| values.join("; "))
    }

    /// The cookies KLMS itself issued: the only ones that are persisted.
    pub fn klms_cookies(&self, klms: &Url) -> Vec<StoredCookie> {
        let origin = klms.origin().ascii_serialization();
        let mut result = self
            .cookies
            .iter()
            .filter(|cookie| {
                cookie.source_origin == origin
                    && cookie.path == "/"
                    && (!cookie.secure || klms.scheme() == "https")
            })
            .map(|cookie| StoredCookie {
                name: cookie.name.clone(),
                value: cookie.value.clone(),
            })
            .collect::<Vec<_>>();
        result.sort_by(|a, b| a.name.cmp(&b.name));
        result.dedup_by(|a, b| a.name == b.name);
        result
    }

    pub fn device_values(&self) -> Vec<String> {
        let mut values = self
            .cookies
            .iter()
            .filter(|cookie| cookie.name.starts_with("sso.cookie.device."))
            .map(|cookie| cookie.value.clone())
            .chain(self.devices.iter().cloned())
            .collect::<Vec<_>>();
        values.sort();
        values.dedup();
        values
    }

    pub fn remember_device(&mut self, value: &str) -> Result<(), AppError> {
        if !valid_device(value) {
            return Err(AppError::auth_protocol(
                "KAIST returned an invalid trusted-device identifier",
            ));
        }
        self.devices.push(value.to_owned());
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::http::HeaderValue;

    const SSO: &str = "https://sso.kaist.ac.kr/";
    const KLMS: &str = "https://klms.kaist.ac.kr/";

    fn capture(jar: &mut Jar, url: &str, set_cookie: &str) -> Result<(), AppError> {
        let mut headers = HeaderMap::new();
        headers.append(SET_COOKIE, HeaderValue::from_str(set_cookie).unwrap());
        jar.capture(&Url::parse(url).unwrap(), &headers)
    }

    fn klms(jar: &Jar) -> Vec<StoredCookie> {
        jar.klms_cookies(&Url::parse(KLMS).unwrap())
    }

    #[test]
    fn predicates_accept_old_values_but_never_break_the_header() {
        for good in ["MoodleSession", "sso.cookie.device.1"] {
            assert!(valid_name(good), "{good:?}");
        }
        for bad in [
            "",
            "a b",
            "a;b",
            "a=b",
            "a\r\n",
            "na\u{e9}me",
            "a\"b",
            "a,b",
        ] {
            assert!(!valid_name(bad), "{bad:?}");
        }
        for good in ["abc123", "a=b", "\"quoted\"", "a b", "a,b", "a\\b"] {
            assert!(valid_cookie("n", good), "{good:?}");
        }
        for bad in ["", "a;b", "a\r\nX: y", "a\tb", "caf\u{e9}"] {
            assert!(!valid_cookie("n", bad), "{bad:?}");
        }
        assert!(valid_cookie("n", &"a".repeat(4096)) && !valid_cookie("n", &"a".repeat(4097)));
        assert!(!valid_cookie("n;", "v"));
    }

    #[test]
    fn capture_applies_the_predicate_and_persists_only_klms_cookies() {
        let mut jar = Jar::default();
        for bad in ["a b=c", "novalue", "a=caf\u{e9}", "x=1; Domain=example.com"] {
            let error = capture(&mut jar, SSO, bad).unwrap_err();
            assert_eq!(error.code, "AUTH_PROTOCOL_CHANGED", "{bad:?}");
        }
        capture(&mut jar, SSO, "q=\"a b,c\"; Path=/").unwrap();
        capture(&mut jar, SSO, "central=secret; Domain=.kaist.ac.kr; Secure").unwrap();
        capture(
            &mut jar,
            KLMS,
            "MoodleSession=owned; Path=/; Secure; HttpOnly",
        )
        .unwrap();
        let owned = StoredCookie {
            name: "MoodleSession".into(),
            value: "owned".into(),
        };
        assert_eq!(klms(&jar), vec![owned]);
        let header = jar.header(&Url::parse(KLMS).unwrap()).unwrap();
        assert!(header.contains("central=secret") && !header.contains("q="));
    }

    #[test]
    fn empty_value_or_expired_max_age_deletes_and_is_never_stored() {
        for deletion in [
            "MoodleSession=; Path=/; Max-Age=0",
            "MoodleSession=; Path=/",
        ] {
            let mut jar = Jar::default();
            capture(&mut jar, KLMS, "MoodleSession=owned; Path=/").unwrap();
            assert_eq!(klms(&jar).len(), 1);
            capture(&mut jar, KLMS, deletion).unwrap();
            assert!(klms(&jar).is_empty() && jar.header(&Url::parse(KLMS).unwrap()).is_none());
        }
        let mut jar = Jar::default();
        capture(&mut jar, KLMS, "fresh=1; Path=/; Max-Age=-5").unwrap();
        assert!(klms(&jar).is_empty());
    }

    #[test]
    fn snapshot_round_trips_and_rejects_tampering() {
        let mut jar = Jar::default();
        let scoped = "s=1; Domain=kaist.ac.kr; Path=/auth; Secure";
        capture(&mut jar, "https://sso.kaist.ac.kr/auth/x", scoped).unwrap();
        capture(&mut jar, KLMS, "MoodleSession=owned; Path=/").unwrap();
        jar.remember_device("dev").unwrap();
        let back: Jar = serde_json::from_str(&serde_json::to_string(&jar).unwrap()).unwrap();
        assert_eq!(back.clone().checked().unwrap(), jar);
        assert_eq!(klms(&back), klms(&jar));
        let mut evil = jar.clone();
        evil.cookies[0].value = "x; injected=y".into();
        assert!(evil.checked().is_err());
        let mut evil = jar;
        evil.devices.push("bad device".into());
        assert!(evil.checked().is_err());
    }
}
