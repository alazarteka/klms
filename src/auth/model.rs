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
    pub session_path: String,
    pub cookie_count: usize,
    pub device_count: usize,
}

#[derive(Debug, Serialize)]
pub struct LogoutResult {
    pub session_path: String,
    pub removed: bool,
}
