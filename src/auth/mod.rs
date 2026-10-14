//! Native KAIST SSO login and the files it leaves behind. This module is the
//! orchestration (`login`, `logout`, `forget`, `load`); the SSO conversation
//! is in `flow`, the on-disk state in `store`, password storage in `secret`.

mod cookies;
mod flow;
mod secret;
mod store;
mod transport;

/// The in-memory cookie jar, shared with the Panopto exchange in `client`.
pub(crate) use cookies::Jar;

use std::io::IsTerminal;

use serde::{Deserialize, Serialize};
use serde_json::json;
use zeroize::Zeroizing;

use crate::{
    date::epoch_now,
    error::AppError,
    output::{self, CommandResult},
    url::Url,
};

use flow::{Attempt, AuthPrompt, Outcome, TerminalPrompt};
use secret::{Backend, Secrets};
use store::{Dirs, Identity};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, clap::ValueEnum)]
#[serde(rename_all = "lowercase")]
pub enum LoginMethod {
    Easy,
    Password,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, clap::ValueEnum)]
#[serde(rename_all = "lowercase")]
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
}

impl SecondFactor {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Email => "email",
            Self::Sms => "sms",
        }
    }
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

pub fn load(base_url: &Url) -> Result<AuthSession, AppError> {
    let mut session = store::load_session(&Dirs::state_dir()?.join("session.json"), base_url)?;
    match Dirs::from_env().and_then(|dirs| store::load_identity(&dirs.identity())) {
        Ok(found) => {
            session.status.remembered = found.map(|identity| Remembered {
                username: identity.username,
                method: identity.method.as_str().to_owned(),
                second_factor: identity.second_factor.map(|f| f.as_str().to_owned()),
                password_backend: identity.password_backend.unwrap_or_else(|| "none".into()),
            });
        }
        Err(error) => session.status.remembered_error = Some(error.message),
    }
    Ok(session)
}

/// Flags of `klms auth login`, already parsed.
#[derive(Debug, Default)]
pub struct LoginOptions {
    pub user: Option<String>,
    pub method: Option<LoginMethod>,
    pub factor: Option<SecondFactor>,
    pub remember_password: bool,
    pub insecure_storage: bool,
    pub code: Option<String>,
}

/// Where a login runs: endpoints plus how interactive the process is.
struct Env<'a> {
    dirs: &'a Dirs,
    klms: &'a Url,
    sso: &'a Url,
    timeout: u64,
    secrets: &'a Secrets,
    /// Standard input is a terminal, so prompts can be answered.
    interactive: bool,
    /// Stop after sending a second-factor code instead of prompting for it.
    defer_code: bool,
    /// `--json`: stderr carries only the JSON error document, so stay quiet.
    json: bool,
}

pub fn login(
    base_url: &Url,
    timeout: u64,
    options: &LoginOptions,
    json: bool,
) -> Result<CommandResult, AppError> {
    let sso_url = match base_url.host_str() == Some("klms.kaist.ac.kr") {
        true => Url::parse("https://sso.kaist.ac.kr/").expect("valid SSO URL"),
        false => base_url.clone(),
    };
    let dirs = Dirs::from_env()?;
    let secrets = Secrets::new(dirs.credentials());
    let interactive = std::io::stdin().is_terminal();
    let env = Env {
        dirs: &dirs,
        klms: base_url,
        sso: &sso_url,
        timeout,
        secrets: &secrets,
        interactive,
        defer_code: !interactive || json,
        json,
    };
    login_with(&env, options, TerminalPrompt)
}

fn login_with(
    env: &Env<'_>,
    options: &LoginOptions,
    mut terminal: impl AuthPrompt,
) -> Result<CommandResult, AppError> {
    if let Some(code) = &options.code {
        return resume_login(env, code);
    }
    let remembered = store::load_identity(&env.dirs.identity())?;
    let method = options
        .method
        .or(remembered.as_ref().map(|found| found.method))
        .unwrap_or(LoginMethod::Easy);
    let password_login = method == LoginMethod::Password;
    if options.factor.is_some() && !password_login {
        return Err(AppError::usage(
            "--second-factor applies only to password login",
        ));
    }
    if options.remember_password && !password_login {
        return Err(AppError::usage(
            "--remember-password applies only to password login (Easy Login has no password)",
        ));
    }
    if options.remember_password && !env.interactive {
        return Err(AppError::usage(
            "--remember-password needs a terminal to read the password; run it once from an interactive shell",
        ));
    }
    if !password_login && !env.interactive {
        return Err(AppError::auth(
            "Easy Login needs you to approve a request in the KAIST app, so it cannot run without a terminal",
            "Use `klms auth login --method password --second-factor email`; after one interactive `klms auth login --method password --remember-password`, that works non-interactively in two steps (the first prints CODE_REQUIRED, then `klms auth login --code CODE`).",
        ));
    }
    let factor = password_login.then(|| {
        options
            .factor
            .or(remembered.as_ref().and_then(|found| found.second_factor))
            .unwrap_or(SecondFactor::Email)
    });
    let username = options
        .user
        .clone()
        .or_else(|| remembered.as_ref().map(|found| found.username.clone()));
    if let Some(username) = &username {
        store::validate_username(username)?;
    }
    if username.is_none() && !env.interactive {
        return Err(AppError::usage(
            "no KAIST ID is remembered; pass `--user ID` (an interactive login also remembers it)",
        ));
    }

    // Refuse before any network traffic when there is nowhere to keep a password.
    let new_backend = options
        .remember_password
        .then(|| env.secrets.choose(options.insecure_storage))
        .transpose()?;
    let mut warnings = Vec::new();
    let stored = if password_login && !options.remember_password {
        let same_account = remembered
            .as_ref()
            .filter(|found| Some(&found.username) == username.as_ref());
        stored_password(env, same_account, &mut warnings)?
    } else {
        None
    };
    if password_login && !env.interactive && stored.is_none() {
        let who = username.as_deref().unwrap_or("ID");
        return Err(AppError::auth(
            format!("no stored password for {who}, and there is no terminal to ask for one"),
            "Store it once from an interactive shell with `klms auth login --method password --remember-password` (add `--insecure-storage` where there is no OS keyring).",
        ));
    }
    if let Some(username) = username.as_ref().filter(|_| !env.json) {
        eprintln!("Signing in as {username} ({})", method.as_str());
    }

    let previous_devices = store::load_session(&env.dirs.session(), env.klms)
        .map(|session| session.devices)
        .unwrap_or_default();
    let begun = flow::begin(
        Attempt {
            klms: env.klms,
            sso: env.sso,
            timeout: env.timeout,
            method,
            factor,
            previous_devices: &previous_devices,
            defer_code: env.defer_code,
            username,
            password: stored,
        },
        &mut terminal,
    )?;
    // KAIST accepted the credentials (even if it still wants a code), so it
    // is safe to remember them now.
    let (identity, stored_now) = remember_login(
        env,
        remembered.as_ref(),
        Identity::new(&begun.username, method, factor),
        new_backend.as_ref().zip(begun.password.as_ref()),
        &mut warnings,
    );
    match begun.outcome {
        Outcome::Complete(completed) => {
            let saved = new_backend
                .as_ref()
                .filter(|_| stored_now)
                .and_then(|backend| password_saved_notice(env.json, backend));
            let result = finish_login(env, completed, &identity, warnings)?;
            if let Some(notice) = saved {
                eprintln!("{notice}");
            }
            Ok(result)
        }
        Outcome::CodeRequired(pending) => {
            store::save_json(&env.dirs.pending(), &pending, "pending login")
                .map_err(|error| store::with_cleanup_warnings(error, &warnings))?;
            let error = AppError::code_required(pending.second_factor.as_str(), pending.expires_at);
            Err(store::with_cleanup_warnings(error, &warnings))
        }
    }
}

/// Human-mode stderr line naming where a just-saved password lives; `None`
/// under `--json`.
fn password_saved_notice(json: bool, backend: &Backend) -> Option<String> {
    if json {
        return None;
    }
    Some(match backend {
        Backend::Keychain(_) => "Password saved to the macOS Keychain.".into(),
        Backend::SecretService(_) => "Password saved to the Secret Service keyring.".into(),
        Backend::File(path) => {
            format!("Password saved in plaintext at {} (0600).", path.display())
        }
    })
}

/// Look up the remembered password; `Ok(None)` when nothing usable is stored.
fn stored_password(
    env: &Env<'_>,
    remembered: Option<&Identity>,
    warnings: &mut Vec<String>,
) -> Result<Option<Zeroizing<String>>, AppError> {
    let Some((found, kind)) =
        remembered.and_then(|found| Some((found, found.password_backend.as_ref()?)))
    else {
        return Ok(None);
    };
    match env
        .secrets
        .open(kind)
        .and_then(|backend| backend.lookup(&found.username))
    {
        Err(error) if env.interactive => {
            warnings.push(format!(
                "could not read the stored password ({}); asking for it instead",
                error.message
            ));
            Ok(None)
        }
        other => other,
    }
}

/// Persist `login.json` (and the password, when asked) after KAIST accepted
/// the credentials; returns what was saved and whether this call stored the
/// password. Failures are warnings: the sign-in itself already worked.
fn remember_login(
    env: &Env<'_>,
    previous: Option<&Identity>,
    mut identity: Identity,
    new_password: Option<(&Backend, &Zeroizing<String>)>,
    warnings: &mut Vec<String>,
) -> (Identity, bool) {
    let mut stored_now = false;
    let username = identity.username.clone();
    let same = previous.filter(|found| found.username == username);
    identity.second_factor = identity
        .second_factor
        .or(same.and_then(|found| found.second_factor));
    identity.password_backend = same.and_then(|found| found.password_backend.clone());
    // A different account: its password must not linger.
    if let Some((old, kind)) = previous
        .filter(|found| found.username != username)
        .and_then(|old| Some((old, old.password_backend.as_ref()?)))
    {
        let cleared = env
            .secrets
            .open(kind)
            .and_then(|backend| backend.clear(&old.username));
        if let Err(error) = cleared {
            warnings.push(format!(
                "could not remove the stored password of {}: {}",
                old.username, error.message
            ));
        }
    }
    if let Some((backend, password)) = new_password {
        match backend.store(&username, password) {
            Ok(()) => {
                let stale = identity
                    .password_backend
                    .take_if(|kind| kind.as_str() != backend.kind());
                if let Some(kind) = stale {
                    if let Err(error) = env.secrets.open(&kind).and_then(|old| old.clear(&username))
                    {
                        warnings.push(format!("could not remove the previous password from the {kind} backend: {}; retry `klms auth forget` when it is available", error.message));
                    }
                }
                identity.password_backend = Some(backend.kind().to_owned());
                stored_now = true;
            }
            Err(error) => {
                warnings.push(format!(
                    "could not remember the password: {}",
                    error.message
                ));
                // The old password is now stale: never keep using it.
                if let Some(kind) = identity.password_backend.take() {
                    let cleared = env.secrets.open(&kind).and_then(|old| old.clear(&username));
                    warnings.push(match cleared {
                        Ok(()) => "the previously remembered password was removed".to_owned(),
                        Err(error) => format!(
                            "the previously remembered password is no longer used but could not be removed from the {kind} backend: {}",
                            error.message
                        ),
                    });
                }
            }
        }
    }
    if let Err(error) = store::save_json(&env.dirs.identity(), &identity, "remembered login") {
        warnings.push(format!("could not remember the login: {}", error.message));
        if stored_now {
            let (backend, _) = new_password.expect("a password was just stored");
            if let Err(error) = backend.clear(&username) {
                warnings.push(format!("could not roll back the stored password: {}; retry `klms auth forget` when the backend is available", error.message));
            }
            identity.password_backend = None;
            stored_now = false;
        }
    }
    (identity, stored_now)
}

fn finish_login(
    env: &Env<'_>,
    completed: flow::CompletedLogin,
    identity: &Identity,
    mut warnings: Vec<String>,
) -> Result<CommandResult, AppError> {
    let (cookie_count, device_count) = (completed.cookies.len(), completed.devices.len());
    let session_path = env.dirs.session();
    store::save_session(
        &session_path,
        env.klms,
        completed.cookies,
        completed.devices,
    )
    .map_err(|error| store::with_cleanup_warnings(error, &warnings))?;
    store::remove_owned(&env.dirs.pending(), "pending login", &mut warnings);
    let method = identity.method.as_str();
    let human = format!(
        "Signed in to KLMS with {method} login.\nSession: {}",
        session_path.display()
    );
    let data = json!({
        "method": method,
        "second_factor": identity.second_factor.filter(|_| identity.method == LoginMethod::Password).map(SecondFactor::as_str),
        "user": identity.username,
        "session_path": session_path.display().to_string(),
        "cookie_count": cookie_count,
        "device_count": device_count,
        "password_backend": identity.password_backend,
    });
    let mut result = output::result("auth.login", &data, human)?;
    result.warnings = warnings;
    Ok(result)
}

fn resume_login(env: &Env<'_>, code: &str) -> Result<CommandResult, AppError> {
    flow::check_code_format(code)?;
    let pending_path = env.dirs.pending();
    let pending = store::load_pending(&pending_path, env.klms, epoch_now() as u64)?;
    let (answered, completed) = flow::resume(env.klms, env.sso, env.timeout, &pending, code);
    // Once KAIST has answered, the challenge is used up whether or not the
    // code was right; without an answer the same code can be retried.
    let mut warnings = Vec::new();
    if answered {
        store::remove_owned(&pending_path, "pending login", &mut warnings);
    }
    let completed = completed.map_err(|error| store::with_cleanup_warnings(error, &warnings))?;
    let previous = store::load_identity(&env.dirs.identity()).ok().flatten();
    let login = Identity::new(
        &pending.username,
        LoginMethod::Password,
        Some(pending.second_factor),
    );
    let (identity, _) = remember_login(env, previous.as_ref(), login, None, &mut warnings);
    finish_login(env, completed, &identity, warnings)
}

pub fn logout() -> Result<CommandResult, AppError> {
    logout_with(&Dirs::state_dir()?)
}

fn logout_with(state: &std::path::Path) -> Result<CommandResult, AppError> {
    let path = state.join("session.json");
    let mut warnings = Vec::new();
    let removed = store::remove_owned(&path, "KLMS session", &mut warnings);
    let pending_removed = store::remove_owned(
        &state.join("pending-login.json"),
        "pending login",
        &mut warnings,
    );
    let human = if !warnings.is_empty() {
        "Local logout cleanup was incomplete; see warnings.".to_owned()
    } else if removed {
        format!("Removed local KLMS session: {}", path.display())
    } else {
        format!("No local KLMS session was present at {}", path.display())
    };
    let data = json!({"session_path": path.display().to_string(), "removed": removed,
        "pending_removed": pending_removed, "cleanup_complete": warnings.is_empty()});
    let mut result = output::result("auth.logout", &data, human)?;
    result.warnings = warnings;
    Ok(result)
}

pub fn forget() -> Result<CommandResult, AppError> {
    let dirs = Dirs::from_env()?;
    forget_with(&dirs, &Secrets::new(dirs.credentials()))
}

fn forget_with(dirs: &Dirs, secrets: &Secrets) -> Result<CommandResult, AppError> {
    let mut warnings = Vec::new();
    let found = match store::load_identity(&dirs.identity()) {
        Ok(found) => found,
        Err(error) => {
            warnings.push(format!(
                "remembered identity was unreadable: {}",
                error.message
            ));
            None
        }
    };
    let backend = found
        .as_ref()
        .and_then(|found| found.password_backend.as_deref());
    let mut password_backends = std::collections::BTreeMap::from([(secret::PLAINTEXT_FILE, None)]);
    // Always purge available keyrings, including accounts absent from metadata.
    if secrets.mac || secrets.security.is_file() {
        password_backends.insert("keychain", None);
    }
    if secrets.secret_tool().is_some() {
        password_backends.insert("secret-service", None);
    }
    if let Some(kind) = backend {
        password_backends.insert(kind, None);
    }
    for (kind, removed) in &mut password_backends {
        if *kind == secret::PLAINTEXT_FILE {
            let before = warnings.len();
            let deleted =
                store::remove_owned(&dirs.credentials(), "credentials file", &mut warnings);
            *removed = (warnings.len() == before).then_some(deleted);
            continue;
        }
        match secrets.open(kind).and_then(|target| target.clear_all()) {
            Ok(deleted) => *removed = Some(deleted),
            Err(error) => warnings.push(format!(
                "could not purge {kind} passwords: {}; passwords may remain there; retry `klms auth forget` when the backend is available",
                error.message
            )),
        }
    }
    let password_complete = password_backends.values().all(Option::is_some);
    let password_removed =
        password_complete && password_backends.values().any(|v| *v == Some(true));
    // Retain the backend pointer until that backend confirms its purge, so a
    // later forget retries even when its helper is currently unavailable.
    let login_removed = if backend.is_none_or(|kind| password_backends[kind].is_some()) {
        store::remove_owned(&dirs.identity(), "remembered login", &mut warnings)
    } else {
        store::remove_temporaries(&dirs.identity(), "remembered login", &mut warnings);
        false
    };
    let backend = backend.or_else(|| {
        password_backends
            .iter()
            .find_map(|(kind, removed)| (*removed == Some(true)).then_some(*kind))
    });
    let pending_removed = store::remove_owned(&dirs.pending(), "pending login", &mut warnings);
    let human = if !warnings.is_empty() {
        "Forget cleanup was incomplete; see warnings. The saved KLMS session is unchanged."
            .to_owned()
    } else {
        "Remembered login cleanup finished. The saved KLMS session is unchanged; `klms auth logout` removes it.".to_owned()
    };
    let data = json!({
        "login_removed": login_removed,
        "password_removed": password_removed,
        "password_backend": backend,
        "password_backends": password_backends,
        "password_cleanup_complete": password_complete,
        "pending_removed": pending_removed,
    });
    let mut result = output::result("auth.forget", &data, human)?;
    result.warnings = warnings;
    Ok(result)
}

#[cfg(test)]
mod tests;
