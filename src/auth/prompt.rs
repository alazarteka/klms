use std::io::{self, Write};

use zeroize::Zeroizing;

use crate::error::AppError;

pub trait AuthPrompt {
    fn identifier(&mut self) -> Result<String, AppError>;
    fn password(&mut self) -> Result<Zeroizing<String>, AppError>;
    fn otp(&mut self, channel: &str) -> Result<Zeroizing<String>, AppError>;
    fn notice(&mut self, message: &str);
}

pub struct TerminalPrompt;

impl AuthPrompt for TerminalPrompt {
    fn identifier(&mut self) -> Result<String, AppError> {
        eprint!("KAIST ID or email: ");
        io::stderr().flush().ok();
        let mut value = String::new();
        io::stdin()
            .read_line(&mut value)
            .map_err(|error| AppError::config(format!("cannot read login identifier: {error}")))?;
        let value = value.trim().to_owned();
        if value.is_empty() {
            return Err(AppError::usage("login identifier cannot be empty"));
        }
        Ok(value)
    }

    fn password(&mut self) -> Result<Zeroizing<String>, AppError> {
        rpassword::prompt_password("KAIST password: ")
            .map(Zeroizing::new)
            .map_err(|error| {
                AppError::config(format!("cannot read password from terminal: {error}"))
            })
    }

    fn otp(&mut self, channel: &str) -> Result<Zeroizing<String>, AppError> {
        rpassword::prompt_password(format!("Six-digit code sent by {channel}: "))
            .map(Zeroizing::new)
            .map_err(|error| AppError::config(format!("cannot read verification code: {error}")))
    }

    fn notice(&mut self, message: &str) {
        eprintln!("{message}");
    }
}

/// Wraps another prompt with answers already known (a remembered or
/// `--user` identifier, a stored password) and records what was actually used
/// so the caller can remember it after a successful sign-in.
pub struct KnownAnswers<P: AuthPrompt> {
    inner: P,
    identifier: Option<String>,
    password: Option<Zeroizing<String>>,
    pub identifier_used: Option<String>,
    pub password_used: Option<Zeroizing<String>>,
}

impl<P: AuthPrompt> KnownAnswers<P> {
    pub fn new(inner: P, identifier: Option<String>, password: Option<Zeroizing<String>>) -> Self {
        Self {
            inner,
            identifier,
            password,
            identifier_used: None,
            password_used: None,
        }
    }
}

impl<P: AuthPrompt> AuthPrompt for KnownAnswers<P> {
    fn identifier(&mut self) -> Result<String, AppError> {
        let value = match self.identifier.take() {
            Some(value) => value,
            None => self.inner.identifier()?,
        };
        self.identifier_used = Some(value.clone());
        Ok(value)
    }

    fn password(&mut self) -> Result<Zeroizing<String>, AppError> {
        let value = match self.password.take() {
            Some(value) => value,
            None => self.inner.password()?,
        };
        self.password_used = Some(value.clone());
        Ok(value)
    }

    fn otp(&mut self, channel: &str) -> Result<Zeroizing<String>, AppError> {
        self.inner.otp(channel)
    }

    fn notice(&mut self, message: &str) {
        self.inner.notice(message);
    }
}
