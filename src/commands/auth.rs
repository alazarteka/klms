use super::Ctx;
use crate::{
    auth,
    client::KlmsClient,
    error::AppError,
    models::SessionTime,
    output::{self, CommandResult},
    parse, safe_url,
    url::Url,
};
use serde::Serialize;

pub(super) fn auth_status(status: &auth::AuthStatus) -> Result<CommandResult, AppError> {
    let mut human = match status.configured {
        true => format!(
            "Owned session: {}\nSource: {}\nCookies: {}\nTrusted devices: {}",
            status.path, status.source, status.cookie_count, status.device_count,
        ),
        false => format!(
            "Owned session: not configured\nRun `klms auth login`.\nExpected path: {}",
            status.path
        ),
    };
    human.push_str(&match (&status.remembered, &status.remembered_error) {
        (Some(login), _) => {
            let factor = (login.second_factor.as_deref())
                .map(|factor| format!(", {factor} code"))
                .unwrap_or_default();
            format!(
                "\nRemembered login: {} ({}{factor})\nStored password: {}",
                login.username, login.method, login.password_backend
            )
        }
        (None, Some(error)) => format!("\nRemembered login: unreadable ({error})"),
        (None, None) => "\nRemembered login: none".into(),
    });
    output::result("auth.status", status, human)
}

#[derive(Serialize)]
struct Doctor {
    version: &'static str,
    base_url: String,
    auth: auth::AuthStatus,
    session_status: &'static str,
    session_error: Option<AppError>,
    dashboard_url: Option<String>,
    check_may_have_extended_session: bool,
}

pub(super) fn doctor(
    base: &Url,
    session: auth::AuthSession,
    timeout: u64,
) -> Result<CommandResult, AppError> {
    let checked = session.cookie_header.is_some();
    let probe = match session.cookie_header.as_deref() {
        Some(cookie) => KlmsClient::new(base.as_str(), Some(cookie), timeout)
            .and_then(|client| client.get("/my/")),
        None => Err(AppError::auth_required("no usable KLMS session was found")),
    };
    let (session_status, session_error, dashboard_url) = match probe {
        Ok(response) => ("valid", None, Some(safe_url::display(&response.url))),
        Err(error) => {
            let status = match error.code {
                _ if !checked => "not_configured",
                "AUTH_REQUIRED" => "expired",
                "NETWORK_ERROR" => "unreachable",
                _ => "error",
            };
            (status, Some(error), None)
        }
    };
    let model = Doctor {
        version: env!("CARGO_PKG_VERSION"),
        base_url: base.to_string(),
        auth: session.status,
        session_status,
        session_error,
        dashboard_url,
        check_may_have_extended_session: checked,
    };
    if let Some(error) = model.session_error.clone() {
        let details = serde_json::to_value(&model).map_err(|error| {
            AppError::internal(format!("failed to encode doctor diagnostics: {error}"))
        })?;
        return Err(error.with_details(details));
    }
    let owned = if model.auth.configured {
        model.auth.source
    } else {
        "missing"
    };
    let human = format!(
        "klms {}\nOrigin: {}\nOwned session: {owned}\nSession: {}",
        model.version, model.base_url, model.session_status
    );
    let mut result = output::result("doctor", &model, human)?;
    result.warnings.push(
        "The dashboard request used to validate the session may refresh KLMS activity time.".into(),
    );
    Ok(result)
}

impl Ctx<'_> {
    pub(super) fn session_time(&self, extend: bool) -> Result<CommandResult, AppError> {
        let key = parse::sesskey(&self.client.get("/my/")?.text)?;
        if extend {
            self.client.ajax(&key, "core_session_touch")?;
        }
        let data = self.client.ajax(&key, "core_session_time_remaining")?;
        let seconds = (data.get("timeremaining"))
            .and_then(serde_json::Value::as_u64)
            .or_else(|| data.as_u64())
            .ok_or_else(|| {
                AppError::shape("session time response did not contain timeremaining")
            })?;
        let model = SessionTime {
            remaining_seconds: seconds,
            remaining: format!(
                "{:02}:{:02}:{:02}",
                seconds / 3600,
                seconds % 3600 / 60,
                seconds % 60
            ),
            bootstrap_may_have_extended_session: true,
            extended: extend,
        };
        let command = if extend {
            "auth.extend"
        } else {
            "auth.time-left"
        };
        let suffix = if extend { " (extended)" } else { "" };
        let human = format!("Session time remaining: {}{suffix}", model.remaining);
        let mut result = output::result(command, &model, human)?;
        result.warnings.push(
            "A dashboard request was needed to discover the session key and may have refreshed KLMS activity time."
                .into(),
        );
        Ok(result)
    }
}
