use std::fmt;

use serde::Serialize;
use serde_json::{Value, json};

const AUTH_RECOVERY_HINT: &str = "Run `klms auth login` to sign in again. `klms auth extend` only extends a session that is still valid.";

#[derive(Debug, Clone, Serialize)]
pub struct AppError {
    pub code: &'static str,
    pub message: String,
    pub hint: Option<String>,
    pub retryable: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<Value>,
    #[serde(skip)]
    exit_code: u8,
}

macro_rules! constructors {
    ($($name:ident $code:literal $exit:literal $hint:expr;)*) => {
        $(pub fn $name(message: impl Into<String>) -> Self {
            Self::new($code, message, $hint, $exit)
        })*
    };
}

impl AppError {
    constructors! {
        usage "USAGE" 2 None;
        auth_required "AUTH_REQUIRED" 10 Some(AUTH_RECOVERY_HINT);
        auth_protocol "AUTH_PROTOCOL_CHANGED" 11 Some("KAIST SSO may have changed; update klms and retry.");
        network "NETWORK_ERROR" 20 None;
        shape "UPSTREAM_SHAPE_CHANGED" 30 Some("KLMS may have changed its markup; rerun with the latest klms release.");
        upstream "UPSTREAM_ERROR" 31 None;
        config "CONFIG_ERROR" 40 None;
        limit "LIMIT_EXCEEDED" 41 None;
        not_found "NOT_FOUND" 44 None;
        migration_required "MIGRATION_REQUIRED" 45 Some("Update klms before opening this library.");
        internal "INTERNAL_ERROR" 50 None;
        corpus_corrupt "CORPUS_CORRUPT" 51 Some("Preserve the library files for recovery before retrying.");
        corpus_busy "CORPUS_BUSY" 52 Some("Retry after the other library operation finishes.");
        library_io "LIBRARY_IO" 53 None;
        curation_conflict "CURATION_CONFLICT" 54 None;
        content_unavailable "CONTENT_UNAVAILABLE" 55 None;
    }

    pub fn auth(message: impl Into<String>, hint: impl Into<String>) -> Self {
        Self::auth_required(message).with_hint(hint)
    }

    /// A password login sent a verification code and stopped; the caller
    /// resumes with `klms auth login --code CODE`. Exit code 12.
    pub fn code_required(channel: &str, expires_at: u64) -> Self {
        let resume = "klms auth login --code CODE";
        Self::new(
            "CODE_REQUIRED",
            format!("KAIST sent a verification code by {channel}; enter it to finish signing in"),
            Some("Run `klms auth login --code CODE` with the six-digit code before the pending login expires."),
            12,
        )
        .with_details(json!({"channel": channel, "expires_at": expires_at, "resume": resume}))
    }

    pub fn http(status: u16, path: &str) -> Self {
        let message = format!("KLMS returned HTTP {status} for {path}");
        match status {
            401 => Self::auth_required(format!("KLMS rejected authentication for {path}")),
            403 => Self::new(
                "PERMISSION_DENIED",
                format!("KLMS denied access to {path}"),
                Some("Confirm that this resource belongs to your account and is still available."),
                13,
            ),
            404 => Self::not_found(format!("KLMS resource was not found: {path}")),
            408 | 425 | 429 | 500..=599 => Self::network(message),
            _ => Self::new("HTTP_ERROR", message, None, 21),
        }
    }

    fn new(code: &'static str, message: impl Into<String>, hint: Option<&str>, exit: u8) -> Self {
        Self {
            code,
            message: message.into(),
            hint: hint.map(Into::into),
            retryable: matches!(code, "NETWORK_ERROR" | "CORPUS_BUSY"),
            details: None,
            exit_code: exit,
        }
    }

    pub fn with_details(mut self, details: Value) -> Self {
        self.details = Some(details);
        self
    }

    pub fn with_hint(mut self, hint: impl Into<String>) -> Self {
        self.hint = Some(hint.into());
        self
    }

    pub fn exit_code(&self) -> u8 {
        self.exit_code
    }
}

impl fmt::Display for AppError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for AppError {}

impl From<rusqlite::Error> for AppError {
    fn from(error: rusqlite::Error) -> Self {
        use rusqlite::ErrorCode::*;
        let code = match &error {
            rusqlite::Error::SqliteFailure(code, _) => Some(code.code),
            _ => None,
        };
        match code {
            Some(DatabaseBusy | DatabaseLocked) => {
                Self::corpus_busy("local library is locked by another process")
            }
            Some(DatabaseCorrupt | NotADatabase) => {
                Self::corpus_corrupt("local library database is corrupt")
            }
            Some(SchemaChanged) => Self::migration_required("local library schema is incompatible"),
            _ => Self::library_io(format!("SQLite failure: {error}")),
        }
    }
}
