use crate::{
    client::KlmsClient,
    error::AppError,
    models::RawGet,
    output::{self, CommandResult},
    parse, safe_url,
};
use url::Url;

pub(super) fn get(
    client: &KlmsClient,
    path: &str,
    max_bytes: usize,
) -> Result<CommandResult, AppError> {
    validate_read_target(path)?;
    let response = client.get_preview(path, max_bytes)?;
    let content_type = response
        .content_type
        .as_deref()
        .map(str::to_ascii_lowercase)
        .unwrap_or_default();
    let html = content_type.contains("text/html");
    let json = content_type.contains("json");
    if !html && !json {
        return Err(AppError::usage(
            "request get previews HTML and JSON only; use `files download` for other content",
        ));
    }
    let body = if html {
        let source = String::from_utf8_lossy(&response.bytes);
        redact_secrets(&parse::safe_html_preview(&source))
    } else if response.truncated {
        "[JSON preview omitted because the bounded response is incomplete]".into()
    } else {
        let mut value: serde_json::Value = serde_json::from_slice(&response.bytes)
            .map_err(|error| AppError::shape(format!("invalid JSON response: {error}")))?;
        redact_json(&mut value);
        serde_json::to_string(&value).map_err(|error| {
            AppError::internal(format!("failed to encode JSON preview: {error}"))
        })?
    };
    let model = RawGet {
        url: safe_url::display(&response.url),
        content_type: response.content_type,
        bytes: response.bytes.len(),
        body,
        truncated: response.truncated,
        redacted: true,
    };
    output::result("request.get", &model, model.body.clone())
}

fn validate_read_target(value: &str) -> Result<(), AppError> {
    let base = Url::parse("https://klms.invalid/").expect("valid fixed URL");
    let url = base
        .join(value)
        .map_err(|error| AppError::usage(format!("invalid request target: {error}")))?;
    let path = url.path();
    let module_read =
        path.starts_with("/mod/") && (path.ends_with("/view.php") || path.ends_with("/index.php"));
    let allowed = path == "/my/"
        || path == "/course/view.php"
        || module_read
        || path == "/calendar/view.php"
        || path.starts_with("/grade/report/")
        || path == "/local/lmsattendance/index.php"
        || path.starts_with("/pluginfile.php/");
    if !allowed {
        return Err(AppError::usage(
            "request get accepts known content-read paths only; use a typed command when available",
        ));
    }
    if url.query_pairs().any(|(key, _)| {
        safe_url::sensitive_key(&key)
            || matches!(
                key.to_ascii_lowercase().as_str(),
                "action" | "delete" | "confirm" | "logout"
            )
    }) {
        return Err(AppError::usage(
            "request get refuses action and secret query parameters",
        ));
    }
    Ok(())
}

fn redact_secrets(value: &str) -> String {
    let mut output = value.to_owned();
    for key in ["sesskey", "logintoken", "moodlesession", "token"] {
        output = redact_key_values(&output, key);
    }
    output
}

fn redact_json(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::Object(values) => {
            for (key, value) in values {
                if safe_url::sensitive_key(key) {
                    *value = serde_json::Value::String("[REDACTED]".into());
                } else {
                    redact_json(value);
                }
            }
        }
        serde_json::Value::Array(values) => values.iter_mut().for_each(redact_json),
        serde_json::Value::String(value) => *value = redact_secrets(value),
        _ => {}
    }
}

fn redact_key_values(value: &str, key: &str) -> String {
    let lower = value.to_ascii_lowercase();
    let mut result = String::with_capacity(value.len());
    let mut cursor = 0;
    while let Some(relative) = lower[cursor..].find(key) {
        let start = cursor + relative;
        result.push_str(&value[cursor..start + key.len()]);
        let bytes = value.as_bytes();
        let mut position = start + key.len();
        while position < bytes.len()
            && matches!(bytes[position], b' ' | b'\t' | b'\r' | b'\n' | b'"' | b'\'')
        {
            result.push(bytes[position] as char);
            position += 1;
        }
        if position >= bytes.len() || !matches!(bytes[position], b':' | b'=') {
            cursor = start + key.len();
            continue;
        }
        result.push(bytes[position] as char);
        position += 1;
        while position < bytes.len() && bytes[position].is_ascii_whitespace() {
            result.push(bytes[position] as char);
            position += 1;
        }
        let quote = bytes
            .get(position)
            .copied()
            .filter(|byte| matches!(byte, b'"' | b'\''));
        if let Some(quote) = quote {
            result.push(quote as char);
            position += 1;
        }
        result.push_str("[REDACTED]");
        while position < bytes.len() {
            let byte = bytes[position];
            if quote.is_some_and(|quote| byte == quote)
                || quote.is_none()
                    && (byte.is_ascii_whitespace() || matches!(byte, b'&' | b',' | b'}' | b']'))
            {
                break;
            }
            position += 1;
        }
        cursor = position;
    }
    result.push_str(&value[cursor..]);
    result
}

#[cfg(test)]
mod tests {
    use super::{redact_json, redact_secrets, validate_read_target};

    #[test]
    fn raw_preview_redacts_common_secret_assignments() {
        let source = r#"{"sesskey":"abc123","name":"safe"}&token=xyz789"#;
        let redacted = redact_secrets(source);
        assert!(!redacted.contains("abc123"));
        assert!(!redacted.contains("xyz789"));
        assert!(redacted.contains(r#""name":"safe""#));
        assert_eq!(redacted.matches("[REDACTED]").count(), 2);
    }

    #[test]
    fn raw_json_preview_redacts_nested_secret_fields() {
        let mut value = serde_json::json!({"data": {"access_token": "abc123", "name": "safe"}});
        redact_json(&mut value);
        assert_eq!(value["data"]["access_token"], "[REDACTED]");
        assert_eq!(value["data"]["name"], "safe");
    }

    #[test]
    fn raw_preview_rejects_action_routes_and_secret_queries() {
        assert!(validate_read_target("/login/logout.php?sesskey=abc").is_err());
        assert!(validate_read_target("/mod/assign/view.php?id=7&action=delete").is_err());
        assert!(validate_read_target("/mod/assign/view.php?id=7").is_ok());
    }
}
