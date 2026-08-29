use std::fmt;

use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
pub struct AppError {
    pub code: &'static str,
    pub message: String,
    pub hint: Option<String>,
    pub retryable: bool,
    #[serde(skip)]
    exit_code: u8,
}

impl AppError {
    pub fn usage(message: impl Into<String>) -> Self {
        Self::new("USAGE", message, None, false, 2)
    }

    pub fn auth(message: impl Into<String>, hint: impl Into<String>) -> Self {
        Self::new("AUTH_REQUIRED", message, Some(hint.into()), false, 10)
    }

    pub fn network(message: impl Into<String>) -> Self {
        Self::new("NETWORK_ERROR", message, None, true, 20)
    }

    pub fn http(status: u16, path: &str) -> Self {
        match status {
            401 => Self::auth(
                format!("KLMS rejected authentication for {path}"),
                "Refresh the storage-state session and retry.",
            ),
            403 => Self::new(
                "PERMISSION_DENIED",
                format!("KLMS denied access to {path}"),
                Some(
                    "Confirm that this resource belongs to your account and is still available."
                        .into(),
                ),
                false,
                13,
            ),
            404 => Self::not_found(format!("KLMS resource was not found: {path}")),
            408 | 425 | 429 | 500..=599 => {
                Self::network(format!("KLMS returned HTTP {status} for {path}"))
            }
            _ => Self::new(
                "HTTP_ERROR",
                format!("KLMS returned HTTP {status} for {path}"),
                None,
                false,
                21,
            ),
        }
    }

    pub fn shape(message: impl Into<String>) -> Self {
        Self::new(
            "UPSTREAM_SHAPE_CHANGED",
            message,
            Some("KLMS may have changed its markup; rerun with the latest klms release.".into()),
            false,
            30,
        )
    }

    pub fn config(message: impl Into<String>) -> Self {
        Self::new("CONFIG_ERROR", message, None, false, 40)
    }

    pub fn limit(message: impl Into<String>) -> Self {
        Self::new("LIMIT_EXCEEDED", message, None, false, 41)
    }

    pub fn upstream(message: impl Into<String>) -> Self {
        Self::new("UPSTREAM_ERROR", message, None, false, 31)
    }

    pub fn not_found(message: impl Into<String>) -> Self {
        Self::new("NOT_FOUND", message, None, false, 44)
    }

    pub fn internal(message: impl Into<String>) -> Self {
        Self::new("INTERNAL_ERROR", message, None, false, 50)
    }

    fn new(
        code: &'static str,
        message: impl Into<String>,
        hint: Option<String>,
        retryable: bool,
        exit_code: u8,
    ) -> Self {
        Self {
            code,
            message: message.into(),
            hint,
            retryable,
            exit_code,
        }
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
