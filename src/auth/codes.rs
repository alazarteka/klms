use crate::error::AppError;

pub fn password_primary(code: &str) -> Result<PrimaryNext, AppError> {
    match code {
        "SS0001" => Ok(PrimaryNext::Link),
        "SS0098" => Ok(PrimaryNext::SecondFactor),
        "SS0099" => Ok(PrimaryNext::Device),
        "SS0007" => Ok(PrimaryNext::Duplicate),
        "SS0004" | "SS0005" | "SS0006" => Err(AppError::auth_required(
            "KAIST requires a password update before this account can sign in",
        )),
        "EAU001" => Err(AppError::auth(
            "KAIST rejected the login identifier or password",
            "Check the credentials and retry `klms auth login --method password`.",
        )),
        "EAU005" | "EAU006" | "EAU007" => Err(AppError::auth(
            "KAIST temporarily locked password login after repeated failures",
            "Wait for the lockout to expire, then retry.",
        )),
        "EAU016" | "EAU017" | "EAU018" | "ES0017" => Err(AppError::auth_protocol(
            "KAIST rejected the login transaction",
        )),
        other => Err(unknown(other)),
    }
}

pub fn otp(code: &str) -> Result<PrimaryNext, AppError> {
    match code {
        "SS0001" => Ok(PrimaryNext::Link),
        "SS0099" => Ok(PrimaryNext::Device),
        "SS0007" => Ok(PrimaryNext::Duplicate),
        "E001" => Err(AppError::auth(
            "The verification code is incorrect",
            "Retry login and enter the newest six-digit code.",
        )),
        "E002" => Err(AppError::auth(
            "The verification code expired",
            "Retry login to request a new code.",
        )),
        "E003" => Err(AppError::auth(
            "Too many verification attempts",
            "Retry login to request a new code.",
        )),
        "ES0017" => Err(AppError::auth_protocol(
            "KAIST rejected the verification transaction",
        )),
        other => Err(unknown(other)),
    }
}

pub fn easy_poll(code: &str) -> Result<EasyPoll, AppError> {
    match code {
        "" | "SS0001" => Ok(EasyPoll::Approved),
        "ESY020" => Ok(EasyPoll::Pending),
        "ESY021" => Err(AppError::auth(
            "Easy Login is temporarily blocked",
            "Wait and retry after the block expires.",
        )),
        "ESY022" => Err(AppError::auth(
            "Easy Login is blocked for this account",
            "Use password login or contact KAIST support.",
        )),
        "ESY023" => Err(AppError::auth(
            "Easy Login was cancelled",
            "Run `klms auth login --method easy` to start again.",
        )),
        "ESY024" => Err(AppError::auth(
            "Easy Login verification did not match",
            "Start a new Easy Login request.",
        )),
        "E004" => Err(AppError::auth(
            "Easy Login expired",
            "Start a new Easy Login request.",
        )),
        other => Err(unknown(other)),
    }
}

pub fn policy(code: &str) -> Result<PrimaryNext, AppError> {
    match code {
        "SS0001" | "" => Ok(PrimaryNext::Link),
        "SS0099" => Ok(PrimaryNext::Device),
        "SS0007" => Ok(PrimaryNext::Duplicate),
        "SS0004" | "SS0005" | "SS0006" => Err(AppError::auth_required(
            "KAIST requires a password update before this account can sign in",
        )),
        "EAU016" | "EAU017" | "EAU018" | "ES0017" => Err(AppError::auth_protocol(
            "KAIST rejected the login policy transaction",
        )),
        other => Err(unknown(other)),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrimaryNext {
    Link,
    SecondFactor,
    Device,
    Duplicate,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EasyPoll {
    Pending,
    Approved,
}

fn unknown(code: &str) -> AppError {
    AppError::auth_protocol(format!("KAIST SSO returned unknown result code {code:?}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn unknown_codes_are_protocol_changes() {
        assert_eq!(
            password_primary("NEW_CODE").unwrap_err().code,
            "AUTH_PROTOCOL_CHANGED"
        );
        assert_eq!(otp("NEW_CODE").unwrap_err().code, "AUTH_PROTOCOL_CHANGED");
        assert_eq!(
            easy_poll("NEW_CODE").unwrap_err().code,
            "AUTH_PROTOCOL_CHANGED"
        );
    }
}
