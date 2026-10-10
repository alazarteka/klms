//! Everything `klms auth` keeps on disk, all private (0700 directory, 0600
//! file, atomic replace): the session cookies, the remembered login
//! (`login.json`, in the config directory so `auth logout` leaves it) and the
//! pending second-factor login.

use std::{
    env, fs,
    io::{ErrorKind, Write},
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

use crate::{date::epoch_now, error::AppError, url::Url};

use super::{
    AuthSession, AuthStatus, LoginMethod, SecondFactor,
    cookies::{self, Jar},
};

const VERSION: u32 = 1;
const PENDING_TTL_SECS: u64 = 300;

#[derive(Debug, Clone)]
pub struct Dirs {
    /// `$XDG_CONFIG_HOME/klms` or `~/.config/klms`.
    pub config: PathBuf,
    /// `$XDG_STATE_HOME/klms` or `~/.local/state/klms`.
    pub state: PathBuf,
}

fn xdg(var: &str, fallback: &str) -> Result<PathBuf, AppError> {
    env::var_os(var)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .or_else(|| env::var_os("HOME").map(|home| PathBuf::from(home).join(fallback)))
        .map(|root| root.join("klms"))
        .ok_or_else(|| AppError::config(format!("HOME or {var} is required for KLMS login")))
}

impl Dirs {
    pub fn state_dir() -> Result<PathBuf, AppError> {
        xdg("XDG_STATE_HOME", ".local/state")
    }
    pub fn from_env() -> Result<Self, AppError> {
        Ok(Self {
            config: xdg("XDG_CONFIG_HOME", ".config")?,
            state: Self::state_dir()?,
        })
    }
    pub fn session(&self) -> PathBuf {
        self.state.join("session.json")
    }
    pub fn pending(&self) -> PathBuf {
        self.state.join("pending-login.json")
    }
    pub fn identity(&self) -> PathBuf {
        self.config.join("login.json")
    }
    pub fn credentials(&self) -> PathBuf {
        self.config.join("credentials.json")
    }
}

/// Atomically replace `path` with `bytes` at mode 0600 inside a 0700 parent.
/// `what` names the file in error messages.
fn write_private(path: &Path, bytes: &[u8], what: &str) -> Result<(), AppError> {
    let parent = path
        .parent()
        .ok_or_else(|| AppError::internal(format!("invalid {what} path")))?;
    fs::create_dir_all(parent).map_err(|error| {
        AppError::config(format!("cannot create {}: {error}", parent.display()))
    })?;
    let secure = |path: &Path, mode| {
        fs::set_permissions(path, fs::Permissions::from_mode(mode))
            .map_err(|error| AppError::config(format!("cannot secure {}: {error}", path.display())))
    };
    secure(parent, 0o700)?;
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(format!(".{}.tmp", std::process::id()));
    let temp = path.with_file_name(name);
    let _ = fs::remove_file(&temp);
    let written = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&temp)
        .map_err(|error| AppError::config(format!("cannot create private {what} file: {error}")))
        .and_then(|mut file| {
            file.write_all(bytes)
                .and_then(|()| file.sync_all())
                .map_err(|error| AppError::config(format!("cannot write {what} file: {error}")))
        })
        .and_then(|()| {
            fs::rename(&temp, path)
                .map_err(|error| AppError::config(format!("cannot install {what} file: {error}")))
        });
    if written.is_err() {
        let _ = fs::remove_file(&temp);
    }
    written?;
    secure(path, 0o600)
}

pub fn save_json(path: &Path, value: &impl Serialize, what: &str) -> Result<(), AppError> {
    let bytes = Zeroizing::new(
        serde_json::to_vec_pretty(value)
            .map_err(|error| AppError::internal(format!("failed to encode {what}: {error}")))?,
    );
    write_private(path, &bytes, what)
}

/// The file's bytes, or `None` when it does not exist.
pub fn read_file(path: &Path, what: &str) -> Result<Option<Zeroizing<Vec<u8>>>, AppError> {
    match fs::read(path) {
        Ok(bytes) => Ok(Some(Zeroizing::new(bytes))),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(None),
        Err(error) => Err(AppError::config(format!(
            "cannot read {what} {}: {error}",
            path.display()
        ))),
    }
}

/// Remove `path`; `Ok(false)` when it was already absent.
pub fn remove_file(path: &Path, what: &str) -> Result<bool, AppError> {
    match fs::remove_file(path) {
        Ok(()) => Ok(true),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(false),
        Err(error) => Err(AppError::config(format!(
            "cannot remove {what} {}: {error}",
            path.display()
        ))),
    }
}

// ---- session ----

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct StoredCookie {
    pub name: String,
    pub value: String,
}

#[derive(Serialize, Deserialize)]
struct StoredSession {
    version: u32,
    origin: String,
    created_at: u64,
    cookies: Vec<StoredCookie>,
    #[serde(default)]
    devices: Vec<String>,
}

fn check_cookies(cookies: &[StoredCookie]) -> Result<(), AppError> {
    if cookies
        .iter()
        .all(|cookie| cookies::valid_cookie(&cookie.name, &cookie.value))
    {
        return Ok(());
    }
    Err(AppError::config("saved session contains an invalid cookie"))
}

pub fn load_session(path: &Path, base_url: &Url) -> Result<AuthSession, AppError> {
    let mut status = AuthStatus {
        configured: false,
        source: "owned-session",
        path: path.display().to_string(),
        cookie_count: 0,
        device_count: 0,
        created_at: None,
        remembered: None,
        remembered_error: None,
    };
    let Some(bytes) = read_file(path, "KLMS session")? else {
        return Ok(AuthSession {
            status,
            cookie_header: None,
            devices: Vec::new(),
        });
    };
    let stored: StoredSession = serde_json::from_slice(&bytes).map_err(|error| {
        AppError::config(format!("invalid KLMS session {}: {error}", path.display()))
    })?;
    if stored.version != VERSION {
        return Err(AppError::config(format!(
            "unsupported KLMS session version {}; sign in again",
            stored.version
        )));
    }
    if !origin_matches(&stored.origin, base_url) {
        return Err(AppError::config(
            "saved KLMS session belongs to a different origin; sign in again",
        ));
    }
    // Older releases saved under a looser value rule; those still load.
    check_cookies(&stored.cookies)?;
    let header = stored
        .cookies
        .iter()
        .map(|cookie| format!("{}={}", cookie.name, cookie.value))
        .collect::<Vec<_>>()
        .join("; ");
    status.configured = true;
    status.cookie_count = stored.cookies.len();
    status.device_count = stored.devices.len();
    status.created_at = Some(stored.created_at);
    Ok(AuthSession {
        status,
        cookie_header: (!header.is_empty()).then_some(header),
        devices: stored.devices,
    })
}

pub fn save_session(
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
    check_cookies(&cookies)?;
    let stored = StoredSession {
        version: VERSION,
        origin: origin(base_url),
        created_at: epoch_now() as u64,
        cookies,
        devices,
    };
    save_json(path, &stored, "session")
}

pub fn origin(url: &Url) -> String {
    format!(
        "{}://{}:{}",
        url.scheme(),
        url.host_str().unwrap_or_default(),
        url.port_or_known_default().unwrap_or(0)
    )
}

/// Loopback test servers may move between ports; real origins must match.
fn origin_matches(stored: &str, current: &Url) -> bool {
    stored == origin(current)
        || Url::parse(stored).is_ok_and(|url| url.is_http_loopback() && current.is_http_loopback())
}

// ---- remembered login ----

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Identity {
    pub version: u32,
    pub username: String,
    pub method: LoginMethod,
    /// An unrecognized value (older or hand-edited file) loads as `None`.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "lenient_factor"
    )]
    pub second_factor: Option<SecondFactor>,
    /// `keychain`, `secret-service` or `plaintext-file`; absent when no
    /// password is remembered.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub password_backend: Option<String>,
}

fn lenient_factor<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<SecondFactor>, D::Error> {
    let value = Option::<String>::deserialize(deserializer)?;
    Ok(value.and_then(|text| serde_json::from_value(serde_json::Value::String(text)).ok()))
}

impl Identity {
    pub fn new(username: &str, method: LoginMethod, factor: Option<SecondFactor>) -> Self {
        Self {
            version: VERSION,
            username: username.to_owned(),
            method,
            second_factor: factor,
            password_backend: None,
        }
    }
}

/// Reject identifiers that could not be a KAIST login and could corrupt
/// keyring command lines.
pub fn validate_username(username: &str) -> Result<(), AppError> {
    if username.trim().is_empty()
        || username != username.trim()
        || username.len() > 254
        || username.chars().any(char::is_control)
    {
        return Err(AppError::usage(
            "login identifier must be a non-empty KAIST ID or email without control characters",
        ));
    }
    Ok(())
}

pub fn load_identity(path: &Path) -> Result<Option<Identity>, AppError> {
    let Some(bytes) = read_file(path, "remembered login")? else {
        return Ok(None);
    };
    let invalid = |detail: String| {
        AppError::config(format!(
            "invalid remembered login {}: {detail}",
            path.display()
        ))
        .with_hint("Run `klms auth forget` to discard it, then sign in again.")
    };
    let identity: Identity =
        serde_json::from_slice(&bytes).map_err(|error| invalid(error.to_string()))?;
    if identity.version != VERSION {
        return Err(invalid(format!("unsupported version {}", identity.version)));
    }
    validate_username(&identity.username).map_err(|error| invalid(error.message))?;
    Ok(Some(identity))
}

// ---- pending second-factor login ----

/// What survives between "KAIST sent the code" and "the code was typed": the
/// SSO cookie jar (the challenge is bound to the SSO session cookie), the
/// trusted devices known so far, the last document URL (for `Referer` and
/// `Origin`) and who is signing in. Never a password; expires after five
/// minutes.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PendingLogin {
    pub version: u32,
    pub klms_origin: String,
    pub username: String,
    pub second_factor: SecondFactor,
    pub expires_at: u64,
    pub document_url: Option<String>,
    pub previous_devices: Vec<String>,
    pub jar: Jar,
}

impl PendingLogin {
    pub fn new(
        klms: &Url,
        username: &str,
        second_factor: SecondFactor,
        document_url: Option<&Url>,
        previous_devices: &[String],
        jar: &Jar,
    ) -> Self {
        Self {
            version: VERSION,
            klms_origin: origin(klms),
            username: username.to_owned(),
            second_factor,
            expires_at: epoch_now() as u64 + PENDING_TTL_SECS,
            document_url: document_url.map(Url::to_string),
            previous_devices: previous_devices.to_vec(),
            jar: jar.clone(),
        }
    }
}

/// Read a pending login that is still valid for `klms`. A missing, expired,
/// unreadable or foreign file is an error; all but "missing" also delete it
/// so it cannot be retried.
pub fn load_pending(path: &Path, klms: &Url, now: u64) -> Result<PendingLogin, AppError> {
    let restart = "Run `klms auth login` to request a new verification code.";
    let Some(bytes) = read_file(path, "pending login")? else {
        return Err(AppError::auth(
            "no login is waiting for a verification code",
            restart,
        ));
    };
    let discard = |message: &str| {
        let _ = remove_file(path, "pending login");
        AppError::auth(message.to_owned(), restart)
    };
    let pending: PendingLogin = serde_json::from_slice(&bytes)
        .map_err(|_| discard("the pending login file is unreadable"))?;
    if pending.version != VERSION {
        return Err(discard(
            "the pending login file is from another klms version",
        ));
    }
    if now >= pending.expires_at {
        return Err(discard("the pending login expired"));
    }
    if !origin_matches(&pending.klms_origin, klms) {
        return Err(discard(
            "the pending login belongs to a different KLMS origin",
        ));
    }
    Ok(pending)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn klms() -> Url {
        Url::parse("https://klms.kaist.ac.kr/").unwrap()
    }

    fn mode(path: &Path) -> u32 {
        fs::metadata(path).unwrap().permissions().mode() & 0o777
    }

    fn cookie(name: &str, value: &str) -> StoredCookie {
        StoredCookie {
            name: name.into(),
            value: value.into(),
        }
    }

    #[test]
    fn sessions_round_trip_privately_and_reject_unsafe_cookies() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("klms/session.json");
        save_session(&path, &klms(), vec![cookie("MoodleSession", "abc")], vec![]).unwrap();
        let session = load_session(&path, &klms()).unwrap();
        assert_eq!(session.cookie_header.as_deref(), Some("MoodleSession=abc"));
        assert_eq!((mode(&path), mode(path.parent().unwrap())), (0o600, 0o700));
        for (name, value) in [
            ("n", "a;b"),
            ("n", ""),
            ("n", "a\r\nX: y"),
            ("bad\r\n", "x"),
        ] {
            let result = save_session(&path, &klms(), vec![cookie(name, value)], vec![]);
            assert_eq!(result.unwrap_err().code, "CONFIG_ERROR", "{value:?}");
        }
        // The same rule guards what is read back.
        for value in ["", "a;b", r"a\tb"] {
            let body = format!(
                r#"{{"version":1,"origin":"https://klms.kaist.ac.kr:443","created_at":1,"cookies":[{{"name":"n","value":"{value}"}}],"devices":[]}}"#
            );
            fs::write(&path, body).unwrap();
            let error = load_session(&path, &klms()).unwrap_err();
            assert_eq!(error.code, "CONFIG_ERROR");
        }
    }

    #[test]
    fn session_saved_under_the_old_rules_still_loads() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session.json");
        // Values the previous release accepted: commas, quotes, backslashes.
        fs::write(
            &path,
            r#"{"version":1,"origin":"https://klms.kaist.ac.kr:443","created_at":7,
               "cookies":[{"name":"MoodleSession","value":"abc,def\\x\"y"},
                          {"name":"other","value":"plain"}],"devices":["dev1"]}"#,
        )
        .unwrap();
        let session = load_session(&path, &klms()).unwrap();
        assert_eq!(session.status.cookie_count, 2);
        assert_eq!(
            session.cookie_header.as_deref(),
            Some("MoodleSession=abc,def\\x\"y; other=plain")
        );
        assert_eq!(session.devices, vec!["dev1"]);
    }

    #[test]
    fn identity_round_trips_and_corrupt_files_point_to_forget() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("klms/login.json");
        assert_eq!(load_identity(&path).unwrap(), None);
        let identity = Identity::new("student", LoginMethod::Password, Some(SecondFactor::Sms));
        save_json(&path, &identity, "remembered login").unwrap();
        assert_eq!(load_identity(&path).unwrap(), Some(identity));
        let text = fs::read_to_string(&path).unwrap();
        assert!(!text.contains("password_backend"));
        assert_eq!(mode(&path), 0o600);
        assert!(remove_file(&path, "login").unwrap());
        assert!(!remove_file(&path, "login").unwrap());
        for body in [
            "not json",
            r#"{"version":9,"username":"a","method":"easy"}"#,
            r#"{"version":1,"username":"","method":"easy"}"#,
            r#"{"version":1,"username":"a","method":"magic"}"#,
        ] {
            fs::write(&path, body).unwrap();
            let error = load_identity(&path).unwrap_err();
            assert_eq!(error.code, "CONFIG_ERROR");
            assert!(error.hint.unwrap().contains("auth forget"));
        }
        // An unknown second factor loads as none, so older files keep working.
        fs::write(
            &path,
            r#"{"version":1,"username":"a","method":"password","second_factor":"fax"}"#,
        )
        .unwrap();
        let loaded = load_identity(&path).unwrap().unwrap();
        assert_eq!(loaded.second_factor, None);
        assert!(validate_username("a@kaist.ac.kr").is_ok());
        for bad in ["", " ", " a", "a\n", "a\u{0}b"] {
            assert!(validate_username(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn pending_logins_expire_and_foreign_or_corrupt_files_are_discarded() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("klms/pending-login.json");
        let missing = load_pending(&path, &klms(), 0).unwrap_err();
        assert_eq!(missing.code, "AUTH_REQUIRED");
        assert!(missing.message.contains("no login is waiting"));
        assert!(missing.hint.unwrap().contains("klms auth login"));

        let jar = Jar::default();
        let pending = PendingLogin::new(&klms(), "s", SecondFactor::Email, None, &[], &jar);
        let other = Url::parse("https://klms.example.org/").unwrap();
        let now = epoch_now() as u64;
        for (url, now, message, garbage) in [
            (klms(), pending.expires_at, "expired", false),
            (other, now, "different", false),
            (klms(), now, "unreadable", true),
        ] {
            save_json(&path, &pending, "pending login").unwrap();
            if garbage {
                fs::write(&path, "garbage").unwrap();
            }
            let error = load_pending(&path, &url, now).unwrap_err();
            assert!(error.message.contains(message), "{}", error.message);
            assert!(!path.exists(), "{message}: state is deleted");
        }
        save_json(&path, &pending, "pending login").unwrap();
        assert_eq!(mode(&path), 0o600);
        let loaded = load_pending(&path, &klms(), pending.expires_at - 1).unwrap();
        assert_eq!(loaded, pending);
    }
}
