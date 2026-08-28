use std::{
    env, fs,
    fs::OpenOptions,
    io::Write,
    path::PathBuf,
    time::{SystemTime, UNIX_EPOCH},
};

use serde::{Deserialize, Serialize};
use url::Url;

use crate::error::AppError;

#[derive(Debug, Clone, Serialize)]
pub struct AuthStatus {
    pub configured: bool,
    pub source: String,
    pub path: Option<String>,
    pub cookie_count: usize,
    pub matching_cookie_count: usize,
    pub has_expired_cookies: bool,
}

#[derive(Debug, Deserialize)]
struct StorageState {
    #[serde(default)]
    cookies: Vec<Cookie>,
}

#[derive(Debug, Deserialize)]
struct Cookie {
    name: String,
    value: String,
    domain: String,
    #[serde(default)]
    path: String,
    expires: Option<f64>,
    #[serde(default)]
    secure: bool,
}

#[derive(Debug)]
pub struct AuthSession {
    pub status: AuthStatus,
    pub cookie_header: Option<String>,
}

#[derive(Debug, Deserialize, Serialize)]
struct SessionCache {
    origin: String,
    sesskey: String,
    stored_at: u64,
}

pub fn cached_sesskey(base_url: &Url) -> Option<String> {
    let path = session_cache_path()?;
    let bytes = fs::read(path).ok()?;
    let cache: SessionCache = serde_json::from_slice(&bytes).ok()?;
    (cache.origin == origin(base_url) && valid_sesskey(&cache.sesskey)).then_some(cache.sesskey)
}

pub fn cache_sesskey(base_url: &Url, sesskey: &str) {
    if !valid_sesskey(sesskey) {
        return;
    }
    let Some(path) = session_cache_path() else {
        return;
    };
    let Some(parent) = path.parent() else { return };
    if fs::create_dir_all(parent).is_err() {
        return;
    }
    set_private_dir(parent);
    let cache = SessionCache {
        origin: origin(base_url),
        sesskey: sesskey.into(),
        stored_at: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs(),
    };
    let Ok(bytes) = serde_json::to_vec(&cache) else {
        return;
    };
    let temp = path.with_extension(format!("json.{}.tmp", std::process::id()));
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let Ok(mut file) = options.open(&temp) else {
        return;
    };
    if file
        .write_all(&bytes)
        .and_then(|_| file.sync_all())
        .is_err()
    {
        let _ = fs::remove_file(&temp);
        return;
    }
    if fs::rename(&temp, &path).is_err() {
        let _ = fs::remove_file(temp);
    }
}

pub fn load(base_url: &Url) -> Result<AuthSession, AppError> {
    let (source, path) = discover_path();
    let Some(path) = path else {
        return Ok(AuthSession {
            status: AuthStatus {
                configured: false,
                source,
                path: None,
                cookie_count: 0,
                matching_cookie_count: 0,
                has_expired_cookies: false,
            },
            cookie_header: None,
        });
    };

    let bytes = fs::read(&path).map_err(|error| {
        AppError::config(format!(
            "cannot read storage state {}: {error}",
            path.display()
        ))
    })?;
    let state: StorageState = serde_json::from_slice(&bytes).map_err(|error| {
        AppError::config(format!(
            "invalid Playwright storage state {}: {error}",
            path.display()
        ))
    })?;
    let host = base_url
        .host_str()
        .ok_or_else(|| AppError::config("KLMS base URL has no host"))?;
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64();
    let mut selected = Vec::new();
    let mut has_expired = false;
    for cookie in &state.cookies {
        let expired = cookie
            .expires
            .is_some_and(|expires| expires > 0.0 && expires <= now);
        has_expired |= expired;
        let domain = cookie.domain.trim_start_matches('.');
        let domain_matches = host == domain || host.ends_with(&format!(".{domain}"));
        let path_matches = cookie.path.is_empty() || "/my/".starts_with(&cookie.path);
        let secure_matches = !cookie.secure || base_url.scheme() == "https";
        if !expired
            && domain_matches
            && path_matches
            && secure_matches
            && valid_cookie_name(&cookie.name)
        {
            selected.push(format!("{}={}", cookie.name, cookie.value));
        }
    }
    let matching_cookie_count = selected.len();
    Ok(AuthSession {
        status: AuthStatus {
            configured: true,
            source,
            path: Some(path.display().to_string()),
            cookie_count: state.cookies.len(),
            matching_cookie_count,
            has_expired_cookies: has_expired,
        },
        cookie_header: (!selected.is_empty()).then(|| selected.join("; ")),
    })
}

fn discover_path() -> (String, Option<PathBuf>) {
    if let Some(path) = env::var_os("KLMS_STORAGE_STATE").filter(|value| !value.is_empty()) {
        return ("environment".into(), Some(PathBuf::from(path)));
    }
    let home = env::var_os("HOME").map(PathBuf::from);
    let config = env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| home.as_ref().map(|path| path.join(".config")))
        .map(|path| path.join("klms/storage-state.json"));
    if config.as_ref().is_some_and(|path| path.is_file()) {
        return ("config".into(), config);
    }
    let legacy = home.map(|path| path.join(".kaist-cli/private/klms/storage_state.json"));
    if legacy.as_ref().is_some_and(|path| path.is_file()) {
        return ("legacy-kaist-cli".into(), legacy);
    }
    ("missing".into(), None)
}

fn valid_cookie_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .bytes()
            .all(|byte| byte > 0x20 && byte < 0x7f && !b"()<>@,;:\\\"/[]?={} \t".contains(&byte))
}

fn session_cache_path() -> Option<PathBuf> {
    env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .or_else(|| env::var_os("HOME").map(|home| PathBuf::from(home).join(".cache")))
        .map(|root| root.join("klms/session.json"))
}

fn origin(url: &Url) -> String {
    format!(
        "{}://{}:{}",
        url.scheme(),
        url.host_str().unwrap_or_default(),
        url.port_or_known_default().unwrap_or(0)
    )
}

fn valid_sesskey(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .chars()
            .all(|character| character.is_ascii_alphanumeric())
}

#[cfg(unix)]
fn set_private_dir(path: &std::path::Path) {
    use std::os::unix::fs::PermissionsExt;
    let _ = fs::set_permissions(path, fs::Permissions::from_mode(0o700));
}

#[cfg(not(unix))]
fn set_private_dir(_path: &std::path::Path) {}

#[cfg(test)]
mod tests {
    use super::{valid_cookie_name, valid_sesskey};

    #[test]
    fn rejects_cookie_header_injection() {
        assert!(valid_cookie_name("MoodleSession"));
        assert!(!valid_cookie_name("bad\r\nheader"));
        assert!(!valid_cookie_name("bad;name"));
    }

    #[test]
    fn rejects_invalid_cached_session_keys() {
        assert!(valid_sesskey("abc123"));
        assert!(!valid_sesskey("bad&key"));
        assert!(!valid_sesskey(""));
    }
}
