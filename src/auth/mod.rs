mod codes;
mod cookie_rules;
mod cookies;
mod crypto;
mod flow;
mod fsutil;
mod identity;
mod model;
mod pending;
mod prompt;
mod secret;
mod store;
mod transport;

use std::io::IsTerminal;

use url::Url;
use zeroize::Zeroizing;

use crate::{
    date::epoch_now,
    error::AppError,
    output::{self, CommandResult},
};

use flow::{Attempt, OtpMode, Outcome};
use identity::{Dirs, Identity};
pub use model::{AuthSession, AuthStatus, LoginMethod, SecondFactor};
use model::{ForgetResult, LoginResult, LogoutResult, Remembered};
use prompt::{AuthPrompt, KnownAnswers, TerminalPrompt};
use secret::{Provider, SecretStore, SystemProvider};

pub fn load(base_url: &Url) -> Result<AuthSession, AppError> {
    let mut session = store::load(base_url)?;
    match Dirs::from_env().and_then(|dirs| identity::load(&dirs.identity())) {
        Ok(found) => session.status.remembered = found.as_ref().map(remembered),
        Err(error) => session.status.remembered_error = Some(error.message),
    }
    Ok(session)
}

fn remembered(identity: &Identity) -> Remembered {
    Remembered {
        username: identity.username.clone(),
        method: identity.method.clone(),
        second_factor: identity.second_factor.clone(),
        password_backend: identity
            .password_backend
            .clone()
            .unwrap_or_else(|| "none".into()),
    }
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
    provider: &'a dyn Provider,
    /// Standard input is a terminal, so prompts can be answered.
    interactive: bool,
    /// Stop after sending a second-factor code instead of prompting for it.
    defer_code: bool,
    /// `--json`: stderr carries only the JSON error document, so stay quiet.
    json: bool,
}

pub fn login(
    base_url: &Url,
    sso_url: &Url,
    timeout: u64,
    options: &LoginOptions,
    json: bool,
) -> Result<CommandResult, AppError> {
    let dirs = Dirs::from_env()?;
    let provider = SystemProvider::new(dirs.credentials());
    let interactive = std::io::stdin().is_terminal();
    let env = Env {
        dirs: &dirs,
        klms: base_url,
        sso: sso_url,
        timeout,
        provider: &provider,
        interactive,
        defer_code: !interactive || json,
        json,
    };
    login_with(&env, options, TerminalPrompt)
}

fn login_with(
    env: &Env<'_>,
    options: &LoginOptions,
    terminal: impl AuthPrompt,
) -> Result<CommandResult, AppError> {
    if let Some(code) = &options.code {
        return resume_login(env, code);
    }
    let remembered = identity::load(&env.dirs.identity())?;
    let method = options
        .method
        .or_else(|| remembered.as_ref().map(Identity::method))
        .unwrap_or(LoginMethod::Easy);
    if options.factor.is_some() && method != LoginMethod::Password {
        return Err(AppError::usage(
            "--second-factor applies only to password login",
        ));
    }
    if options.remember_password && method != LoginMethod::Password {
        return Err(AppError::usage(
            "--remember-password applies only to password login (Easy Login has no password)",
        ));
    }
    if options.remember_password && !env.interactive {
        return Err(AppError::usage(
            "--remember-password needs a terminal to read the password; run it once from an interactive shell",
        ));
    }
    if method == LoginMethod::Easy && !env.interactive {
        return Err(AppError::auth(
            "Easy Login needs you to approve a request in the KAIST app, so it cannot run without a terminal",
            "Use `klms auth login --method password --second-factor email`; after one interactive `klms auth login --method password --remember-password`, that works non-interactively in two steps (the first prints CODE_REQUIRED, then `klms auth login --code CODE`).",
        ));
    }
    let factor = (method == LoginMethod::Password).then(|| {
        options
            .factor
            .or_else(|| remembered.as_ref().and_then(Identity::factor))
            .unwrap_or(SecondFactor::Email)
    });
    let username = options
        .user
        .clone()
        .or_else(|| remembered.as_ref().map(|found| found.username.clone()));
    if let Some(username) = &username {
        identity::validate_username(username)?;
    }
    if username.is_none() && !env.interactive {
        return Err(AppError::usage(
            "no KAIST ID is remembered; pass `--user ID` (an interactive login also remembers it)",
        ));
    }
    let same_account = match (&username, &remembered) {
        (Some(username), Some(found)) => *username == found.username,
        _ => false,
    };

    // Refuse before any network traffic when there is nowhere to keep a password.
    let new_backend = if options.remember_password {
        Some(env.provider.choose(options.insecure_storage)?)
    } else {
        None
    };
    let mut warnings = Vec::new();
    let stored_password = if method == LoginMethod::Password && !options.remember_password {
        stored_password(
            env,
            remembered.as_ref().filter(|_| same_account),
            &mut warnings,
        )?
    } else {
        None
    };
    if method == LoginMethod::Password && !env.interactive && stored_password.is_none() {
        let who = username.as_deref().unwrap_or("ID");
        return Err(AppError::auth(
            format!("no stored password for {who}, and there is no terminal to ask for one"),
            "Store it once from an interactive shell with `klms auth login --method password --remember-password` (add `--insecure-storage` where there is no OS keyring).",
        ));
    }
    if let Some(username) = username.as_ref().filter(|_| !env.json) {
        eprintln!("Signing in as {username} ({})", method.as_str());
    }

    let previous_devices = store::load_at(&env.dirs.session(), env.klms)
        .map(|session| session.devices)
        .unwrap_or_default();
    let mut prompt = KnownAnswers::new(terminal, username.clone(), stored_password);
    let otp_mode = if env.defer_code {
        OtpMode::Defer
    } else {
        OtpMode::Prompt
    };
    let outcome = flow::begin(
        &Attempt {
            klms: env.klms,
            sso: env.sso,
            timeout: env.timeout,
            method,
            factor,
            previous_devices: &previous_devices,
            otp_mode,
        },
        &mut prompt,
    )?;
    let used_name = prompt
        .identifier_used
        .clone()
        .ok_or_else(|| AppError::internal("login finished without an identifier"))?;
    let remember = |warnings: &mut Vec<String>| {
        remember_login(
            env,
            remembered.as_ref(),
            &used_name,
            method,
            factor,
            new_backend.as_deref().zip(prompt.password_used.as_ref()),
            warnings,
        )
    };
    match outcome {
        Outcome::Complete(completed) => {
            let backend = remember(&mut warnings);
            let mut result = finish_login(env, completed, &used_name, method, factor, backend)?;
            result.warnings.extend(warnings);
            Ok(result)
        }
        Outcome::CodeRequired(pending) => {
            // KAIST accepted the password, so it is safe to remember it now.
            remember(&mut warnings);
            pending::save(&env.dirs.pending(), &pending)?;
            let mut error = AppError::code_required(&pending.second_factor, pending.expires_at);
            if !warnings.is_empty() {
                if let Some(details) = error.details.as_mut() {
                    details["warnings"] = serde_json::json!(warnings);
                }
            }
            Err(error)
        }
    }
}

/// Look up the remembered password; `Ok(None)` when nothing usable is stored.
fn stored_password(
    env: &Env<'_>,
    remembered: Option<&Identity>,
    warnings: &mut Vec<String>,
) -> Result<Option<Zeroizing<String>>, AppError> {
    let Some(found) = remembered else {
        return Ok(None);
    };
    let Some(kind) = &found.password_backend else {
        return Ok(None);
    };
    match env
        .provider
        .open(kind)
        .and_then(|backend| backend.lookup(&found.username))
    {
        Ok(password) => Ok(password),
        Err(error) if env.interactive => {
            warnings.push(format!(
                "could not read the stored password ({}); asking for it instead",
                error.message
            ));
            Ok(None)
        }
        Err(error) => Err(error),
    }
}

/// Persist `login.json` (and the password, when asked) after KAIST accepted
/// the credentials. Returns the backend now holding the password. Failures
/// are warnings: the sign-in itself already worked.
fn remember_login(
    env: &Env<'_>,
    previous: Option<&Identity>,
    username: &str,
    method: LoginMethod,
    factor: Option<SecondFactor>,
    new_password: Option<(&dyn SecretStore, &Zeroizing<String>)>,
    warnings: &mut Vec<String>,
) -> Option<String> {
    let same_account = previous.is_some_and(|found| found.username == username);
    let mut identity = Identity::new(
        username,
        method,
        factor.or_else(|| previous.filter(|_| same_account).and_then(Identity::factor)),
    );
    if same_account {
        identity.password_backend = previous.and_then(|found| found.password_backend.clone());
    }
    if let Some(old) = previous.filter(|found| found.username != username) {
        // A different account: its password must not linger.
        if let Some(kind) = &old.password_backend {
            if let Err(error) = env
                .provider
                .open(kind)
                .and_then(|backend| backend.clear(&old.username))
            {
                warnings.push(format!(
                    "could not remove the stored password of {}: {}",
                    old.username, error.message
                ));
            }
        }
    }
    if let Some((backend, password)) = new_password {
        match backend.store(username, password) {
            Ok(()) => {
                let previous_kind = identity.password_backend.clone();
                if let Some(kind) = previous_kind.filter(|kind| kind != backend.kind()) {
                    let _ = env.provider.open(&kind).and_then(|old| old.clear(username));
                }
                identity.password_backend = Some(backend.kind().to_owned());
            }
            Err(error) => warnings.push(format!(
                "could not remember the password: {}",
                error.message
            )),
        }
    }
    if let Err(error) = identity::save(&env.dirs.identity(), &identity) {
        warnings.push(format!("could not remember the login: {}", error.message));
    }
    identity.password_backend
}

fn finish_login(
    env: &Env<'_>,
    completed: flow::CompletedLogin,
    username: &str,
    method: LoginMethod,
    factor: Option<SecondFactor>,
    password_backend: Option<String>,
) -> Result<CommandResult, AppError> {
    let cookie_count = completed.cookies.len();
    let device_count = completed.devices.len();
    let session_path = env.dirs.session();
    store::save_at(
        &session_path,
        env.klms,
        completed.cookies,
        completed.devices,
    )?;
    let _ = pending::remove(&env.dirs.pending());
    let method_name = method.as_str();
    let result = LoginResult {
        method: method_name,
        second_factor: factor.map(SecondFactor::as_str),
        user: username.to_owned(),
        session_path: session_path.display().to_string(),
        cookie_count,
        device_count,
        password_backend,
    };
    output::result(
        "auth.login",
        &result,
        format!(
            "Signed in to KLMS with {method_name} login.\nSession: {}",
            session_path.display()
        ),
    )
}

fn resume_login(env: &Env<'_>, code: &str) -> Result<CommandResult, AppError> {
    flow::check_code_format(code)?;
    let pending_path = env.dirs.pending();
    let pending = pending::load(&pending_path, env.klms, epoch_now() as u64)?;
    let completed = flow::resume(env.klms, env.sso, env.timeout, &pending, code);
    // The challenge is single-use whether or not the code was right.
    let _ = pending::remove(&pending_path);
    let completed = completed?;
    let factor = SecondFactor::parse(&pending.second_factor);
    let previous = identity::load(&env.dirs.identity()).ok().flatten();
    let mut warnings = Vec::new();
    let backend = remember_login(
        env,
        previous.as_ref(),
        &pending.username,
        LoginMethod::Password,
        factor,
        None,
        &mut warnings,
    );
    let mut result = finish_login(
        env,
        completed,
        &pending.username,
        LoginMethod::Password,
        factor,
        backend,
    )?;
    result.warnings.extend(warnings);
    Ok(result)
}

pub fn logout() -> Result<CommandResult, AppError> {
    let (path, removed) = store::remove()?;
    if let Ok(dirs) = Dirs::from_env() {
        let _ = pending::remove(&dirs.pending());
    }
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

pub fn forget() -> Result<CommandResult, AppError> {
    let dirs = Dirs::from_env()?;
    let provider = SystemProvider::new(dirs.credentials());
    forget_with(&dirs, &provider)
}

fn forget_with(dirs: &Dirs, provider: &dyn Provider) -> Result<CommandResult, AppError> {
    // A corrupt login.json must still be removable.
    let found = identity::load(&dirs.identity()).ok().flatten();
    let mut password_removed = false;
    let mut backend_name = None;
    if let Some(found) = &found {
        if let Some(kind) = &found.password_backend {
            provider.open(kind)?.clear(&found.username)?;
            password_removed = true;
            backend_name = Some(kind.clone());
        }
    }
    // The plaintext file only ever holds klms passwords; drop it entirely.
    if fsutil::remove_file(&dirs.credentials(), "credentials file")? {
        password_removed = true;
        backend_name.get_or_insert_with(|| secret::PLAINTEXT_FILE.to_owned());
    }
    let login_removed = identity::remove(&dirs.identity())?;
    let pending_removed = pending::remove(&dirs.pending())?;
    let result = ForgetResult {
        login_removed,
        password_removed,
        password_backend: backend_name,
        pending_removed,
    };
    let human = if login_removed || password_removed || pending_removed {
        let mut parts = Vec::new();
        if login_removed {
            parts.push("remembered login");
        }
        if password_removed {
            parts.push("stored password");
        }
        if pending_removed {
            parts.push("pending verification");
        }
        format!(
            "Forgot {}. The saved KLMS session is unchanged; `klms auth logout` removes it.",
            parts.join(", ")
        )
    } else {
        "Nothing remembered; nothing to forget.".to_owned()
    };
    output::result("auth.forget", &result, human)
}

#[cfg(test)]
mod tests;
