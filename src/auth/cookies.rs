use reqwest::header::HeaderMap;
use url::Url;

use crate::error::AppError;

use super::model::StoredCookie;

#[derive(Debug, Clone)]
struct Cookie {
    name: String,
    value: String,
    domain: String,
    path: String,
    secure: bool,
    source_origin: String,
}

#[derive(Debug, Default)]
pub struct TransientCookies {
    cookies: Vec<Cookie>,
    devices: Vec<String>,
}

impl TransientCookies {
    pub fn capture(&mut self, url: &Url, headers: &HeaderMap) -> Result<(), AppError> {
        url.host_str()
            .ok_or_else(|| AppError::auth_protocol("SSO response URL has no host"))?;
        for value in headers.get_all(reqwest::header::SET_COOKIE) {
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
        if !valid_name(name) || !valid_value(cookie_value) {
            return Err(AppError::auth_protocol("SSO returned an unsafe cookie"));
        }
        let mut domain = host.to_ascii_lowercase();
        let mut path = default_path(request_path);
        let mut secure = false;
        let mut remove = false;
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
                "max-age" if attr_value == "0" => remove = true,
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
                source_origin: origin(url),
            });
        }
        Ok(())
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
                cookie.source_origin == origin(klms)
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
        if value.is_empty()
            || value.len() > 512
            || !value
                .bytes()
                .all(|byte| (0x21..0x7f).contains(&byte) && byte != b';')
        {
            return Err(AppError::auth_protocol(
                "KAIST returned an invalid trusted-device identifier",
            ));
        }
        self.devices.push(value.to_owned());
        Ok(())
    }
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

fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .bytes()
            .all(|byte| byte > 0x20 && byte < 0x7f && !b"()<>@,;:\\\"/[]?={} \t".contains(&byte))
}

fn valid_value(value: &str) -> bool {
    value.len() <= 4096
        && value
            .bytes()
            .all(|byte| (0x20..0x7f).contains(&byte) && byte != b';')
}

fn origin(url: &Url) -> String {
    format!(
        "{}://{}:{}",
        url.scheme(),
        url.host_str().unwrap_or_default(),
        url.port_or_known_default().unwrap_or(0)
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use reqwest::header::{HeaderMap, HeaderValue, SET_COOKIE};

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
}
