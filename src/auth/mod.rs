mod codes;
mod cookies;
mod crypto;
mod flow;
mod model;
mod prompt;
mod store;
mod transport;

use url::Url;

use crate::{
    error::AppError,
    output::{self, CommandResult},
};

pub use model::{AuthSession, AuthStatus, LoginMethod, SecondFactor};
use model::{LoginResult, LogoutResult};
use prompt::TerminalPrompt;

pub fn load(base_url: &Url) -> Result<AuthSession, AppError> {
    store::load(base_url)
}

pub fn login(
    base_url: &Url,
    sso_url: &Url,
    timeout: u64,
    method: LoginMethod,
    factor: Option<SecondFactor>,
) -> Result<CommandResult, AppError> {
    let effective_factor = match method {
        LoginMethod::Password => Some(factor.unwrap_or(SecondFactor::Email)),
        LoginMethod::Easy => None,
    };
    let previous_devices = store::load(base_url)
        .map(|session| session.devices)
        .unwrap_or_default();
    let mut prompt = TerminalPrompt;
    let completed = flow::login(
        base_url,
        sso_url,
        timeout,
        method,
        effective_factor,
        &previous_devices,
        &mut prompt,
    )?;
    let cookie_count = completed.cookies.len();
    let device_count = completed.devices.len();
    let path = store::save(base_url, completed.cookies, completed.devices)?;
    let method_name = match method {
        LoginMethod::Easy => "easy",
        LoginMethod::Password => "password",
    };
    let factor_name = effective_factor.map(|factor| match factor {
        SecondFactor::Email => "email",
        SecondFactor::Sms => "sms",
    });
    let result = LoginResult {
        method: method_name,
        second_factor: factor_name,
        session_path: path.display().to_string(),
        cookie_count,
        device_count,
    };
    output::result(
        "auth.login",
        &result,
        format!(
            "Signed in to KLMS with {method_name} login.\nSession: {}",
            path.display()
        ),
    )
}

pub fn logout() -> Result<CommandResult, AppError> {
    let (path, removed) = store::remove()?;
    let result = LogoutResult {
        session_path: path.display().to_string(),
        removed,
    };
    let human = if removed {
        format!("Removed local KLMS session: {}", path.display())
    } else {
        format!("No local KLMS session was present at {}", path.display())
    };
    output::result("auth.logout", &result, human)
}
