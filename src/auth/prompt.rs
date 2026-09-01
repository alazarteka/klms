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
