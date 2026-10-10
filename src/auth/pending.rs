//! State carried between "KAIST sent the code" and "the code was typed".
//!
//! Only what the verification step needs survives: the SSO cookie jar (the
//! second-factor challenge is bound to the SSO session cookie), the trusted
//! devices known so far, the last document URL (used for `Referer`/`Origin`),
//! and who is signing in. No password is stored here. The file is 0600 and
//! expires after five minutes.

use std::path::Path;

use crate::url::Url;
use serde::{Deserialize, Serialize};

use crate::{date::epoch_now, error::AppError};

use super::{cookies::CookieSnapshot, fsutil, store};

pub const PENDING_VERSION: u32 = 1;
pub const PENDING_TTL_SECS: u64 = 300;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PendingLogin {
    pub version: u32,
    pub klms_origin: String,
    pub username: String,
    pub method: String,
    pub second_factor: String,
    /// Human name of the delivery channel (`email` or `SMS`).
    pub channel: String,
    pub created_at: u64,
    pub expires_at: u64,
    pub document_url: Option<String>,
    pub previous_devices: Vec<String>,
    pub jar: CookieSnapshot,
}

impl PendingLogin {
    pub fn new(
        klms: &Url,
        username: &str,
        second_factor: &str,
        channel: &str,
        document_url: Option<&Url>,
        previous_devices: Vec<String>,
        jar: CookieSnapshot,
    ) -> Self {
        let now = epoch_now() as u64;
        Self {
            version: PENDING_VERSION,
            klms_origin: store::origin(klms),
            username: username.to_owned(),
            method: "password".into(),
            second_factor: second_factor.to_owned(),
            channel: channel.to_owned(),
            created_at: now,
            expires_at: now + PENDING_TTL_SECS,
            document_url: document_url.map(Url::to_string),
            previous_devices,
            jar,
        }
    }
}

pub fn save(path: &Path, pending: &PendingLogin) -> Result<(), AppError> {
    let bytes = serde_json::to_vec_pretty(pending)
        .map_err(|error| AppError::internal(format!("failed to encode pending login: {error}")))?;
    fsutil::write_private(path, &bytes, "pending login")
}

pub fn remove(path: &Path) -> Result<bool, AppError> {
    fsutil::remove_file(path, "pending login")
}

fn restart_hint() -> &'static str {
    "Run `klms auth login` to request a new verification code."
}

/// Read a pending login that is still valid for `klms`. A missing, expired,
/// unreadable or foreign file is an error; an expired or unreadable one is
/// also deleted so it cannot be retried.
pub fn load(path: &Path, klms: &Url, now: u64) -> Result<PendingLogin, AppError> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Err(AppError::auth(
                "no login is waiting for a verification code",
                restart_hint(),
            ));
        }
        Err(error) => {
            return Err(AppError::config(format!(
                "cannot read pending login {}: {error}",
                path.display()
            )));
        }
    };
    let discard = |message: &str| {
        let _ = remove(path);
        AppError::auth(message.to_owned(), restart_hint())
    };
    let pending: PendingLogin = serde_json::from_slice(&bytes)
        .map_err(|_| discard("the pending login file is unreadable"))?;
    if pending.version != PENDING_VERSION {
        return Err(discard(
            "the pending login file is from another klms version",
        ));
    }
    if now >= pending.expires_at {
        return Err(discard("the pending login expired"));
    }
    if !store::stored_origin_matches(&pending.klms_origin, klms) {
        return Err(discard(
            "the pending login belongs to a different KLMS origin",
        ));
    }
    Ok(pending)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(klms: &Url) -> PendingLogin {
        PendingLogin::new(
            klms,
            "student",
            "email",
            "email",
            None,
            vec!["dev".into()],
            CookieSnapshot {
                cookies: vec![],
                devices: vec![],
            },
        )
    }

    #[test]
    fn round_trips_privately_until_it_expires() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("klms/pending-login.json");
        let klms = Url::parse("https://klms.kaist.ac.kr/").unwrap();
        let pending = sample(&klms);
        save(&path, &pending).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        assert_eq!(
            load(&path, &klms, pending.created_at + 299).unwrap(),
            pending
        );
        let error = load(&path, &klms, pending.expires_at).unwrap_err();
        assert!(error.message.contains("expired"));
        assert!(!path.exists(), "expired state is deleted");
    }

    #[test]
    fn missing_foreign_and_corrupt_files_fail_clearly() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("pending-login.json");
        let klms = Url::parse("https://klms.kaist.ac.kr/").unwrap();
        let error = load(&path, &klms, 0).unwrap_err();
        assert_eq!(error.code, "AUTH_REQUIRED");
        assert!(error.message.contains("no login is waiting"));
        assert!(error.hint.unwrap().contains("klms auth login"));

        save(&path, &sample(&klms)).unwrap();
        let other = Url::parse("https://klms.example.org/").unwrap();
        let now = epoch_now() as u64;
        assert!(
            load(&path, &other, now)
                .unwrap_err()
                .message
                .contains("different")
        );
        assert!(!path.exists());

        std::fs::write(&path, "garbage").unwrap();
        assert!(
            load(&path, &klms, now)
                .unwrap_err()
                .message
                .contains("unreadable")
        );
        assert!(!path.exists());
    }
}
