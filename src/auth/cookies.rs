use serde::{Deserialize, Serialize};
use url::Url;

use crate::error::AppError;

use crate::http::{HeaderMap, SET_COOKIE};

use super::{cookie_rules, model::StoredCookie};

#[derive(Debug, Clone)]
struct Cookie {
    name: String,
    value: String,
    domain: String,
    path: String,
    secure: bool,
    source_origin: url::Origin,
}

#[derive(Debug, Default)]
pub struct TransientCookies {
    cookies: Vec<Cookie>,
    devices: Vec<String>,
}

impl TransientCookies {
    pub fn capture(&mut self, url: &Url, headers: &HeaderMap) -> Result<(), AppError> {
        for value in headers.get_all(SET_COOKIE) {
            let value = value
                .to_str()
                .map_err(|_| AppError::auth_protocol("SSO returned a non-text cookie"))?;
            self.capture_one(url, value)?;
        }
        Ok(())
    }

    fn capture_one(&mut self, url: &Url, value: &str) -> Result<(), AppError> {
        let host = url
            .host_str()
            .ok_or_else(|| AppError::auth_protocol("SSO response URL has no host"))?;
        let request_path = url.path();
        let mut parts = value.split(';');
        let pair = parts.next().unwrap_or_default();
        let (name, cookie_value) = pair
            .split_once('=')
            .ok_or_else(|| AppError::auth_protocol("SSO returned a malformed cookie"))?;
        // An empty value is how a server deletes a cookie; it is never stored.
        let mut remove = cookie_value.is_empty();
        let acceptable = if remove {
            cookie_rules::valid_name(name)
        } else {
            cookie_rules::valid_cookie(name, cookie_value)
        };
        if !acceptable {
            return Err(AppError::auth_protocol("SSO returned an unsafe cookie"));
        }
        let mut domain = host.to_ascii_lowercase();
        let mut path = default_path(request_path);
        let mut secure = false;
        for attribute in parts {
            let attribute = attribute.trim();
            let (key, attr_value) = attribute.split_once('=').unwrap_or((attribute, ""));
            match key.to_ascii_lowercase().as_str() {
                "domain" => {
                    let candidate = attr_value.trim_start_matches('.').to_ascii_lowercase();
                    if candidate.is_empty()
                        || !(host == candidate || host.ends_with(&format!(".{candidate}")))
                    {
                        return Err(AppError::auth_protocol(
                            "SSO attempted to set a cookie for an unrelated domain",
                        ));
                    }
                    domain = candidate;
                }
                "path" if attr_value.starts_with('/') => path = attr_value.to_owned(),
                "secure" => secure = true,
                "max-age" if attr_value.parse::<i64>().is_ok_and(|age| age <= 0) => remove = true,
                _ => {}
            }
        }
        self.cookies.retain(|cookie| {
            !(cookie.name == name && cookie.domain == domain && cookie.path == path)
        });
        if !remove {
            self.cookies.push(Cookie {
                name: name.into(),
                value: cookie_value.into(),
                domain,
                path,
                secure,
                source_origin: url.origin(),
            });
        }
        Ok(())
    }

    /// Everything needed to rebuild this jar in a later process.
    pub fn snapshot(&self) -> CookieSnapshot {
        CookieSnapshot {
            cookies: self
                .cookies
                .iter()
                .map(|cookie| SavedCookie {
                    name: cookie.name.clone(),
                    value: cookie.value.clone(),
                    domain: cookie.domain.clone(),
                    path: cookie.path.clone(),
                    secure: cookie.secure,
                    source_origin: cookie.source_origin.ascii_serialization(),
                })
                .collect(),
            devices: self.devices.clone(),
        }
    }

    /// Rebuild a jar from a snapshot, re-checking every cookie because the
    /// snapshot file is untrusted input.
    pub fn restore(snapshot: &CookieSnapshot) -> Result<Self, AppError> {
        let corrupt = || AppError::config("saved login state contains an invalid cookie");
        let mut cookies = Vec::new();
        for saved in &snapshot.cookies {
            if !cookie_rules::valid_cookie(&saved.name, &saved.value)
                || saved.domain.is_empty()
                || !saved.path.starts_with('/')
                || saved
                    .domain
                    .contains(|c: char| c.is_whitespace() || c == ';')
            {
                return Err(corrupt());
            }
            let origin = Url::parse(&saved.source_origin)
                .map_err(|_| corrupt())?
                .origin();
            cookies.push(Cookie {
                name: saved.name.clone(),
                value: saved.value.clone(),
                domain: saved.domain.clone(),
                path: saved.path.clone(),
                secure: saved.secure,
                source_origin: origin,
            });
        }
        for device in &snapshot.devices {
            if !valid_device(device) {
                return Err(corrupt());
            }
        }
        Ok(Self {
            cookies,
            devices: snapshot.devices.clone(),
        })
    }

    pub fn header(&self, url: &Url) -> Option<String> {
        let host = url.host_str()?;
        let path = url.path();
        let values = self
            .cookies
            .iter()
            .filter(|cookie| {
                (host == cookie.domain || host.ends_with(&format!(".{}", cookie.domain)))
                    && path_matches(path, &cookie.path)
                    && (!cookie.secure || url.scheme() == "https")
            })
            .map(|cookie| format!("{}={}", cookie.name, cookie.value))
            .collect::<Vec<_>>();
        (!values.is_empty()).then(|| values.join("; "))
    }

    pub fn klms_cookies(&self, klms: &Url) -> Vec<StoredCookie> {
        let mut result = self
            .cookies
            .iter()
            .filter(|cookie| {
                cookie.source_origin == klms.origin()
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
            .collect::<Vec<_>>();
        values.extend(self.devices.iter().cloned());
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

fn valid_device(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 512
        && value
            .bytes()
            .all(|byte| (0x21..0x7f).contains(&byte) && byte != b';')
}

/// Serializable form of the transient SSO jar (used for the pending login).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CookieSnapshot {
    pub cookies: Vec<SavedCookie>,
    pub devices: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SavedCookie {
    pub name: String,
    pub value: String,
    pub domain: String,
    pub path: String,
    pub secure: bool,
    pub source_origin: String,
}

fn default_path(path: &str) -> String {
    let Some(index) = path.rfind('/') else {
        return "/".into();
    };
    if index == 0 {
        "/".into()
    } else {
        path[..index].into()
    }
}

fn path_matches(request: &str, cookie: &str) -> bool {
    request == cookie || request.starts_with(&format!("{}/", cookie.trim_end_matches('/')))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::http::{HeaderMap, HeaderValue, SET_COOKIE};

    #[test]
    fn persists_only_cookies_issued_by_klms() {
        let mut jar = TransientCookies::default();
        let sso = Url::parse("https://sso.kaist.ac.kr/auth/start").unwrap();
        let klms = Url::parse("https://klms.kaist.ac.kr/").unwrap();
        let mut headers = HeaderMap::new();
        headers.append(
            SET_COOKIE,
            HeaderValue::from_static("central=secret; Domain=.kaist.ac.kr; Path=/; Secure"),
        );
        jar.capture(&sso, &headers).unwrap();
        headers.clear();
        headers.append(
            SET_COOKIE,
            HeaderValue::from_static("MoodleSession=owned; Path=/; Secure; HttpOnly"),
        );
        jar.capture(&klms, &headers).unwrap();
        assert_eq!(
            jar.klms_cookies(&klms),
            vec![StoredCookie {
                name: "MoodleSession".into(),
                value: "owned".into()
            }]
        );
        assert!(jar.header(&klms).unwrap().contains("central=secret"));
    }

    fn capture_header(
        jar: &mut TransientCookies,
        url: &str,
        set_cookie: &str,
    ) -> Result<(), AppError> {
        let mut headers = HeaderMap::new();
        headers.append(SET_COOKIE, HeaderValue::from_str(set_cookie).unwrap());
        jar.capture(&Url::parse(url).unwrap(), &headers)
    }

    #[test]
    fn empty_value_deletes_and_is_never_stored() {
        let mut jar = TransientCookies::default();
        let klms = Url::parse("https://klms.kaist.ac.kr/").unwrap();
        capture_header(
            &mut jar,
            "https://klms.kaist.ac.kr/",
            "MoodleSession=owned; Path=/",
        )
        .unwrap();
        assert_eq!(jar.klms_cookies(&klms).len(), 1);
        capture_header(
            &mut jar,
            "https://klms.kaist.ac.kr/",
            "MoodleSession=; Path=/; Max-Age=0",
        )
        .unwrap();
        assert!(jar.klms_cookies(&klms).is_empty());
        capture_header(
            &mut jar,
            "https://klms.kaist.ac.kr/",
            "MoodleSession=owned; Path=/",
        )
        .unwrap();
        capture_header(
            &mut jar,
            "https://klms.kaist.ac.kr/",
            "MoodleSession=; Path=/",
        )
        .unwrap();
        assert!(jar.klms_cookies(&klms).is_empty());
        assert!(jar.header(&klms).is_none());
        capture_header(
            &mut jar,
            "https://klms.kaist.ac.kr/",
            "fresh=1; Path=/; Max-Age=-5",
        )
        .unwrap();
        assert!(jar.klms_cookies(&klms).is_empty());
    }

    #[test]
    fn capture_applies_the_shared_predicate() {
        let mut jar = TransientCookies::default();
        for bad in ["a b=c", "=v", "novalue", "a=caf\u{e9}", "a=b\tc"] {
            let result = capture_header(&mut jar, "https://sso.kaist.ac.kr/", bad);
            assert!(result.is_err(), "{bad:?}");
        }
        for good in ["s=b c", "t=b,c", "u=b\\c"] {
            capture_header(&mut jar, "https://sso.kaist.ac.kr/", good).unwrap();
        }
        capture_header(&mut jar, "https://sso.kaist.ac.kr/", "q=\"quoted\"; Path=/").unwrap();
        assert!(
            jar.header(&Url::parse("https://sso.kaist.ac.kr/").unwrap())
                .unwrap()
                .contains("q=\"quoted\"")
        );
    }

    #[test]
    fn snapshot_round_trips_and_rejects_tampering() {
        let mut jar = TransientCookies::default();
        capture_header(
            &mut jar,
            "https://sso.kaist.ac.kr/auth/x",
            "s=1; Domain=kaist.ac.kr; Path=/auth; Secure",
        )
        .unwrap();
        capture_header(
            &mut jar,
            "https://klms.kaist.ac.kr/",
            "MoodleSession=owned; Path=/",
        )
        .unwrap();
        jar.remember_device("dev").unwrap();
        let snapshot = jar.snapshot();
        let json = serde_json::to_string(&snapshot).unwrap();
        let back: CookieSnapshot = serde_json::from_str(&json).unwrap();
        let restored = TransientCookies::restore(&back).unwrap();
        assert_eq!(restored.snapshot(), snapshot);
        let klms = Url::parse("https://klms.kaist.ac.kr/").unwrap();
        assert_eq!(restored.klms_cookies(&klms), jar.klms_cookies(&klms));
        let mut evil = snapshot.clone();
        evil.cookies[0].value = "x; injected=y".into();
        assert!(TransientCookies::restore(&evil).is_err());
        let mut evil = snapshot;
        evil.devices.push("bad device".into());
        assert!(TransientCookies::restore(&evil).is_err());
    }
}
