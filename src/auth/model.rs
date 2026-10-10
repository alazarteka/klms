use serde::{Deserialize, Serialize};

pub const SESSION_VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoginMethod {
    Easy,
    Password,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SecondFactor {
    Email,
    Sms,
}

impl LoginMethod {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Easy => "easy",
            Self::Password => "password",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "easy" => Some(Self::Easy),
            "password" => Some(Self::Password),
            _ => None,
        }
    }
}

impl SecondFactor {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Email => "email",
            Self::Sms => "sms",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "email" => Some(Self::Email),
            "sms" => Some(Self::Sms),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct StoredCookie {
    pub name: String,
    pub value: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredSession {
    pub version: u32,
    pub origin: String,
    pub created_at: u64,
    pub cookies: Vec<StoredCookie>,
    #[serde(default)]
    pub devices: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct AuthStatus {
    pub configured: bool,
    pub source: &'static str,
    pub path: String,
    pub cookie_count: usize,
    pub device_count: usize,
    pub created_at: Option<u64>,
    /// The remembered login (`login.json`), without any secret.
    pub remembered: Option<Remembered>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remembered_error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Remembered {
    pub username: String,
    pub method: String,
    pub second_factor: Option<String>,
    /// `none`, `keychain`, `secret-service` or `plaintext-file`.
    pub password_backend: String,
}

#[derive(Debug)]
pub struct AuthSession {
    pub status: AuthStatus,
    pub cookie_header: Option<String>,
    pub devices: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct LoginResult {
    pub method: &'static str,
    pub second_factor: Option<&'static str>,
    pub user: String,
    pub session_path: String,
    pub cookie_count: usize,
    pub device_count: usize,
    /// Where the password is remembered, when it is.
    pub password_backend: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct ForgetResult {
    pub login_removed: bool,
    pub password_removed: bool,
    pub password_backend: Option<String>,
    pub pending_removed: bool,
}

#[derive(Debug, Serialize)]
pub struct LogoutResult {
    pub session_path: String,
    pub removed: bool,
}
