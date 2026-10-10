use std::{
    env, fs,
    path::{Path, PathBuf},
};

use crate::url::Url;

use crate::date::epoch_now;

use crate::error::AppError;

use super::{
    cookie_rules, fsutil,
    model::{AuthSession, AuthStatus, SESSION_VERSION, StoredCookie, StoredSession},
};

pub fn path() -> Result<PathBuf, AppError> {
    env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .or_else(|| env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/state")))
        .map(|root| root.join("klms/session.json"))
        .ok_or_else(|| AppError::config("HOME or XDG_STATE_HOME is required for KLMS login"))
}

pub fn load(base_url: &Url) -> Result<AuthSession, AppError> {
    load_at(&path()?, base_url)
}

pub fn load_at(path: &Path, base_url: &Url) -> Result<AuthSession, AppError> {
    if !path.is_file() {
        return Ok(AuthSession {
            status: AuthStatus {
                configured: false,
                source: "owned-session",
                path: path.display().to_string(),
                cookie_count: 0,
                device_count: 0,
                created_at: None,
                remembered: None,
                remembered_error: None,
            },
            cookie_header: None,
            devices: Vec::new(),
        });
    }
    let bytes = fs::read(path).map_err(|error| {
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
        // Older releases saved under a looser value rule; keep reading them.
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
            remembered: None,
            remembered_error: None,
        },
        cookie_header: header,
        devices: stored.devices,
    })
}

pub fn save_at(
    path: &Path,
    base_url: &Url,
    cookies: Vec<StoredCookie>,
    devices: Vec<String>,
) -> Result<(), AppError> {
    if cookies.is_empty() {
        return Err(AppError::auth_protocol(
            "KAIST SSO completed without issuing a KLMS session cookie",
        ));
    }
    for cookie in &cookies {
        validate_cookie(cookie)?;
    }
    let stored = StoredSession {
        version: SESSION_VERSION,
        origin: origin(base_url),
        created_at: epoch_now() as u64,
        cookies,
        devices,
    };
    let bytes = serde_json::to_vec_pretty(&stored)
        .map_err(|error| AppError::internal(format!("failed to encode session: {error}")))?;
    fsutil::write_private(path, &bytes, "session")
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
    if cookie_rules::valid_cookie(&cookie.name, &cookie.value) {
        Ok(())
    } else {
        Err(AppError::config("saved session contains an invalid cookie"))
    }
}

pub(super) fn origin(url: &Url) -> String {
    format!(
        "{}://{}:{}",
        url.scheme(),
        url.host_str().unwrap_or_default(),
        url.port_or_known_default().unwrap_or(0)
    )
}

pub(super) fn stored_origin_matches(stored: &str, current: &Url) -> bool {
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

#[cfg(test)]
mod tests {
    use super::*;

    fn cookie(name: &str, value: &str) -> StoredCookie {
        StoredCookie {
            name: name.into(),
            value: value.into(),
        }
    }

    #[test]
    fn rejects_header_injection() {
        assert!(validate_cookie(&cookie("MoodleSession", "abc123")).is_ok());
        assert!(validate_cookie(&cookie("bad\r\n", "x")).is_err());
        assert!(validate_cookie(&cookie("ok", "x; injected=y")).is_err());
        assert!(validate_cookie(&cookie("ok", "")).is_err());
    }

    fn klms() -> Url {
        Url::parse("https://klms.kaist.ac.kr/").unwrap()
    }

    #[test]
    fn session_saved_under_the_old_rules_still_loads() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("klms/session.json");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        // Values the previous release accepted: commas, quotes, backslashes.
        fs::write(
            &path,
            r#"{"version":1,"origin":"https://klms.kaist.ac.kr:443","created_at":7,
               "cookies":[{"name":"MoodleSession","value":"abc,def\\x\"y"},
                          {"name":"other","value":"plain"}],
               "devices":["dev1"]}"#,
        )
        .unwrap();
        let session = load_at(&path, &klms()).unwrap();
        assert!(session.status.configured);
        assert_eq!(session.status.cookie_count, 2);
        assert_eq!(
            session.cookie_header.as_deref(),
            Some("MoodleSession=abc,def\\x\"y; other=plain")
        );
        assert_eq!(session.devices, vec!["dev1"]);
    }

    #[test]
    fn load_still_rejects_unsafe_saved_cookies() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session.json");
        for value in ["", "a;b", r"a\tb"] {
            let body = format!(
                r#"{{"version":1,"origin":"https://klms.kaist.ac.kr:443","created_at":1,
                   "cookies":[{{"name":"n","value":"{value}"}}],"devices":[]}}"#
            );
            fs::write(&path, body).unwrap();
            assert_eq!(load_at(&path, &klms()).unwrap_err().code, "CONFIG_ERROR");
        }
    }

    #[test]
    fn save_round_trips_privately_and_rejects_unsafe_cookies() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("klms/session.json");
        save_at(&path, &klms(), vec![cookie("MoodleSession", "abc")], vec![]).unwrap();
        assert_eq!(
            load_at(&path, &klms()).unwrap().cookie_header.as_deref(),
            Some("MoodleSession=abc")
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = |p: &Path| fs::metadata(p).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode(&path), 0o600);
            assert_eq!(mode(path.parent().unwrap()), 0o700);
        }
        for value in ["a;b", "", "a\r\nX: y"] {
            let result = save_at(&path, &klms(), vec![cookie("n", value)], vec![]);
            assert_eq!(result.unwrap_err().code, "CONFIG_ERROR", "{value:?}");
        }
    }
}
