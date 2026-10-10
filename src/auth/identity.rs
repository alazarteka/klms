//! The remembered login identity (`login.json`) and the directories every
//! auth file lives in.
//!
//! `login.json` holds who signed in, how, and which backend (if any) stores
//! their password. It lives in the config directory, not the state directory,
//! so `auth logout` (which removes only the session) leaves it in place and
//! only `auth forget` deletes it.

use std::{
    env,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};

use crate::error::AppError;

use super::{
    fsutil,
    model::{LoginMethod, SecondFactor},
    store,
};

pub const IDENTITY_VERSION: u32 = 1;

#[derive(Debug, Clone)]
pub struct Dirs {
    /// `$XDG_CONFIG_HOME/klms` or `~/.config/klms`.
    pub config: PathBuf,
    /// The directory holding `session.json` (`$XDG_STATE_HOME/klms`).
    pub state: PathBuf,
}

impl Dirs {
    pub fn from_env() -> Result<Self, AppError> {
        let config = env::var_os("XDG_CONFIG_HOME")
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
            .or_else(|| env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))
            .map(|root| root.join("klms"))
            .ok_or_else(|| {
                AppError::config("HOME or XDG_CONFIG_HOME is required for KLMS login")
            })?;
        let session = store::path()?;
        let state = session
            .parent()
            .ok_or_else(|| AppError::internal("invalid session path"))?
            .to_path_buf();
        Ok(Self { config, state })
    }

    pub fn session(&self) -> PathBuf {
        self.state.join("session.json")
    }
    pub fn identity(&self) -> PathBuf {
        self.config.join("login.json")
    }
    pub fn credentials(&self) -> PathBuf {
        self.config.join("credentials.json")
    }
    pub fn pending(&self) -> PathBuf {
        self.state.join("pending-login.json")
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Identity {
    pub version: u32,
    pub username: String,
    pub method: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub second_factor: Option<String>,
    /// `keychain`, `secret-service` or `plaintext-file`; absent when no
    /// password is remembered.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub password_backend: Option<String>,
}

impl Identity {
    pub fn new(username: &str, method: LoginMethod, factor: Option<SecondFactor>) -> Self {
        Self {
            version: IDENTITY_VERSION,
            username: username.to_owned(),
            method: method.as_str().to_owned(),
            second_factor: factor.map(|factor| factor.as_str().to_owned()),
            password_backend: None,
        }
    }

    pub fn method(&self) -> LoginMethod {
        LoginMethod::parse(&self.method).unwrap_or(LoginMethod::Easy)
    }

    pub fn factor(&self) -> Option<SecondFactor> {
        self.second_factor.as_deref().and_then(SecondFactor::parse)
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

pub fn load(path: &Path) -> Result<Option<Identity>, AppError> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(AppError::config(format!(
                "cannot read remembered login {}: {error}",
                path.display()
            )));
        }
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
    if identity.version != IDENTITY_VERSION {
        return Err(invalid(format!("unsupported version {}", identity.version)));
    }
    validate_username(&identity.username).map_err(|error| invalid(error.message))?;
    if LoginMethod::parse(&identity.method).is_none() {
        return Err(invalid(format!("unknown method {:?}", identity.method)));
    }
    Ok(Some(identity))
}

pub fn save(path: &Path, identity: &Identity) -> Result<(), AppError> {
    let bytes = serde_json::to_vec_pretty(identity)
        .map_err(|error| AppError::internal(format!("failed to encode login: {error}")))?;
    fsutil::write_private(path, &bytes, "remembered login")
}

pub fn remove(path: &Path) -> Result<bool, AppError> {
    fsutil::remove_file(path, "remembered login")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_privately_and_omits_empty_fields() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("klms/login.json");
        assert_eq!(load(&path).unwrap(), None);
        let identity = Identity::new("student", LoginMethod::Password, Some(SecondFactor::Sms));
        save(&path, &identity).unwrap();
        assert_eq!(load(&path).unwrap(), Some(identity));
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(!text.contains("password_backend"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600);
        }
        assert!(remove(&path).unwrap());
        assert!(!remove(&path).unwrap());
    }

    #[test]
    fn corrupt_files_error_with_a_forget_hint() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("login.json");
        for body in [
            "not json",
            r#"{"version":9,"username":"a","method":"easy"}"#,
            r#"{"version":1,"username":"","method":"easy"}"#,
            r#"{"version":1,"username":"a","method":"magic"}"#,
        ] {
            std::fs::write(&path, body).unwrap();
            let error = load(&path).unwrap_err();
            assert_eq!(error.code, "CONFIG_ERROR");
            assert!(error.hint.unwrap().contains("auth forget"));
        }
    }

    #[test]
    fn usernames_reject_whitespace_and_controls() {
        assert!(validate_username("20201234").is_ok());
        assert!(validate_username("a@kaist.ac.kr").is_ok());
        for bad in ["", " ", " a", "a\n", "a\u{0}b"] {
            assert!(validate_username(bad).is_err(), "{bad:?}");
        }
    }
}
