use std::{
    env, fs,
    fs::OpenOptions,
    io::Write,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use url::Url;

use crate::error::AppError;

use super::model::{AuthSession, AuthStatus, SESSION_VERSION, StoredCookie, StoredSession};

pub fn path() -> Result<PathBuf, AppError> {
    env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .or_else(|| env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/state")))
        .map(|root| root.join("klms/session.json"))
        .ok_or_else(|| AppError::config("HOME or XDG_STATE_HOME is required for KLMS login"))
}

pub fn load(base_url: &Url) -> Result<AuthSession, AppError> {
    let path = path()?;
    if !path.is_file() {
        return Ok(AuthSession {
            status: AuthStatus {
                configured: false,
                source: "owned-session",
                path: path.display().to_string(),
                cookie_count: 0,
                device_count: 0,
                created_at: None,
            },
            cookie_header: None,
            devices: Vec::new(),
        });
    }
    let bytes = fs::read(&path).map_err(|error| {
        AppError::config(format!(
            "cannot read KLMS session {}: {error}",
            path.display()
        ))
    })?;
    let stored: StoredSession = serde_json::from_slice(&bytes).map_err(|error| {
        AppError::config(format!("invalid KLMS session {}: {error}", path.display()))
    })?;
    if stored.version != SESSION_VERSION {
        return Err(AppError::config(format!(
            "unsupported KLMS session version {}; sign in again",
            stored.version
        )));
    }
    if !stored_origin_matches(&stored.origin, base_url) {
        return Err(AppError::config(
            "saved KLMS session belongs to a different origin; sign in again",
        ));
    }
    for cookie in &stored.cookies {
        validate_cookie(cookie)?;
    }
    let header = (!stored.cookies.is_empty()).then(|| {
        stored
            .cookies
            .iter()
            .map(|cookie| format!("{}={}", cookie.name, cookie.value))
            .collect::<Vec<_>>()
            .join("; ")
    });
    Ok(AuthSession {
        status: AuthStatus {
            configured: true,
            source: "owned-session",
            path: path.display().to_string(),
            cookie_count: stored.cookies.len(),
            device_count: stored.devices.len(),
            created_at: Some(stored.created_at),
        },
        cookie_header: header,
        devices: stored.devices,
    })
}

pub fn save(
    base_url: &Url,
    cookies: Vec<StoredCookie>,
    devices: Vec<String>,
) -> Result<PathBuf, AppError> {
    if cookies.is_empty() {
        return Err(AppError::auth_protocol(
            "KAIST SSO completed without issuing a KLMS session cookie",
        ));
    }
    for cookie in &cookies {
        validate_cookie(cookie)?;
    }
    let path = path()?;
    let parent = path
        .parent()
        .ok_or_else(|| AppError::internal("invalid session path"))?;
    fs::create_dir_all(parent).map_err(|error| {
        AppError::config(format!("cannot create {}: {error}", parent.display()))
    })?;
    set_private_dir(parent)?;
    let stored = StoredSession {
        version: SESSION_VERSION,
        origin: origin(base_url),
        created_at: now(),
        cookies,
        devices,
    };
    let bytes = serde_json::to_vec_pretty(&stored)
        .map_err(|error| AppError::internal(format!("failed to encode session: {error}")))?;
    let temp = path.with_extension(format!("json.{}.tmp", std::process::id()));
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&temp).map_err(|error| {
        AppError::config(format!("cannot create private session file: {error}"))
    })?;
    if let Err(error) = file.write_all(&bytes).and_then(|_| file.sync_all()) {
        let _ = fs::remove_file(&temp);
        return Err(AppError::config(format!(
            "cannot write session file: {error}"
        )));
    }
    if let Err(error) = fs::rename(&temp, &path) {
        let _ = fs::remove_file(&temp);
        return Err(AppError::config(format!(
            "cannot install session file: {error}"
        )));
    }
    set_private_file(&path)?;
    Ok(path)
}

pub fn remove() -> Result<(PathBuf, bool), AppError> {
    let path = path()?;
    if !path.exists() {
        return Ok((path, false));
    }
    fs::remove_file(&path).map_err(|error| {
        AppError::config(format!(
            "cannot remove KLMS session {}: {error}",
            path.display()
        ))
    })?;
    Ok((path, true))
}

fn validate_cookie(cookie: &StoredCookie) -> Result<(), AppError> {
    let valid_name = !cookie.name.is_empty()
        && cookie
            .name
            .bytes()
            .all(|byte| byte > 0x20 && byte < 0x7f && !b"()<>@,;:\\\"/[]?={} \t".contains(&byte));
    let valid_value = !cookie.value.is_empty()
        && cookie.value.len() <= 4096
        && cookie
            .value
            .bytes()
            .all(|byte| (0x21..0x7f).contains(&byte) && byte != b';');
    if !valid_name || !valid_value {
        return Err(AppError::config("saved session contains an invalid cookie"));
    }
    Ok(())
}

fn origin(url: &Url) -> String {
    format!(
        "{}://{}:{}",
        url.scheme(),
        url.host_str().unwrap_or_default(),
        url.port_or_known_default().unwrap_or(0)
    )
}

fn stored_origin_matches(stored: &str, current: &Url) -> bool {
    if stored == origin(current) {
        return true;
    }
    let Ok(stored) = Url::parse(stored) else {
        return false;
    };
    is_loopback(&stored) && is_loopback(current)
}

fn is_loopback(url: &Url) -> bool {
    url.scheme() == "http" && matches!(url.host_str(), Some("localhost" | "127.0.0.1" | "::1"))
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

#[cfg(unix)]
fn set_private_dir(path: &Path) -> Result<(), AppError> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
        .map_err(|error| AppError::config(format!("cannot secure {}: {error}", path.display())))
}

#[cfg(not(unix))]
fn set_private_dir(_path: &Path) -> Result<(), AppError> {
    Ok(())
}

#[cfg(unix)]
fn set_private_file(path: &Path) -> Result<(), AppError> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))
        .map_err(|error| AppError::config(format!("cannot secure {}: {error}", path.display())))
}

#[cfg(not(unix))]
fn set_private_file(_path: &Path) -> Result<(), AppError> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_header_injection() {
        assert!(
            validate_cookie(&StoredCookie {
                name: "MoodleSession".into(),
                value: "abc123".into()
            })
            .is_ok()
        );
        assert!(
            validate_cookie(&StoredCookie {
                name: "bad\r\n".into(),
                value: "x".into()
            })
            .is_err()
        );
        assert!(
            validate_cookie(&StoredCookie {
                name: "ok".into(),
                value: "x; injected=y".into()
            })
            .is_err()
        );
    }
}
