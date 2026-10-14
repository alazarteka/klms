use crate::{
    client::KlmsClient,
    error::AppError,
    models::RawGet,
    output::{self, CommandResult},
    parse, safe_url,
    url::Url,
};

pub(super) fn raw_get(
    client: &KlmsClient,
    path: &str,
    max_bytes: usize,
) -> Result<CommandResult, AppError> {
    validate_read_target(path)?;
    let response = client.get_preview(path, max_bytes)?;
    let kind = (response.content_type.as_deref())
        .map(str::to_ascii_lowercase)
        .unwrap_or_default();
    let html = kind.contains("text/html");
    if !html && !kind.contains("json") {
        return Err(AppError::usage(
            "request get previews HTML and JSON only; use `files download` for other content",
        ));
    }
    let (redacted, body) = if html {
        let original = String::from_utf8_lossy(&response.bytes);
        let body = redact_secrets(&parse::safe_html_preview(&original));
        (body != original, body)
    } else if response.truncated {
        (
            true,
            "[JSON preview omitted because the bounded response is incomplete]".into(),
        )
    } else {
        let mut value: serde_json::Value = serde_json::from_slice(&response.bytes)
            .map_err(|error| AppError::shape(format!("invalid JSON response: {error}")))?;
        let original = value.clone();
        redact_json(&mut value);
        (value != original, value.to_string())
    };
    let model = RawGet {
        url: safe_url::display(&response.url),
        content_type: response.content_type,
        bytes: response.bytes.len(),
        body,
        truncated: response.truncated,
        redacted,
    };
    let mut result = output::result("request.get", &model, model.body.clone())?;
    result
        .warnings
        .push("Preview redaction is heuristic and may miss sensitive data.".into());
    Ok(result)
}

fn validate_read_target(value: &str) -> Result<(), AppError> {
    let base = Url::parse("https://klms.invalid/").expect("valid fixed URL");
    let url =
        (base.join(value)).map_err(|e| AppError::usage(format!("invalid request target: {e}")))?;
    crate::client::policy::read_url(&url)
}

fn redact_secrets(value: &str) -> String {
    let keys = ["sesskey", "logintoken", "moodlesession", "token"];
    keys.iter()
        .fold(value.to_owned(), |text, key| redact_key_values(&text, key))
}

fn redact_json(value: &mut serde_json::Value) {
    use serde_json::Value;
    match value {
        Value::Object(values) => {
            values
                .iter_mut()
                .for_each(|(key, value)| match safe_url::sensitive_key(key) {
                    true => *value = Value::String("[REDACTED]".into()),
                    false => redact_json(value),
                })
        }
        Value::Array(values) => values.iter_mut().for_each(redact_json),
        Value::String(value) => *value = redact_secrets(value),
        _ => {}
    }
}

fn redact_key_values(value: &str, key: &str) -> String {
    let (lower, bytes) = (value.to_ascii_lowercase(), value.as_bytes());
    let skip = |from: usize, keep: &dyn Fn(u8) -> bool| {
        from + bytes[from..].iter().take_while(|&&byte| keep(byte)).count()
    };
    let (mut result, mut cursor) = (String::with_capacity(value.len()), 0);
    while let Some(found) = lower[cursor..].find(key) {
        let after = cursor + found + key.len();
        let separator = skip(after, &|b| {
            matches!(b, b' ' | b'\t' | b'\r' | b'\n' | b'"' | b'\'')
        });
        if !matches!(bytes.get(separator), Some(b':' | b'=')) {
            result.push_str(&value[cursor..after]);
            cursor = after;
            continue;
        }
        let mut start = skip(separator + 1, &|b| b.is_ascii_whitespace());
        let quote = bytes
            .get(start)
            .copied()
            .filter(|b| matches!(b, b'"' | b'\''));
        start += usize::from(quote.is_some());
        let end = skip(start, &|b| match quote {
            Some(quote) => b != quote,
            None => !(b.is_ascii_whitespace() || matches!(b, b'&' | b',' | b'}' | b']')),
        });
        result.push_str(&value[cursor..start]);
        result.push_str("[REDACTED]");
        cursor = end;
    }
    result.push_str(&value[cursor..]);
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unchanged_json_preview_does_not_claim_redaction() {
        use crate::fixture_server::{Response, Server};
        let server = Server::new(|_| Response::bytes("application/json", r#"{"name":"safe"}"#));
        let client = KlmsClient::new(&server.url(), None, 5).unwrap();
        let result = raw_get(&client, "/mod/assign/view.php?id=7", 1024).unwrap();
        assert_eq!(result.data["redacted"], false);
        assert!(result.warnings[0].contains("heuristic"));
    }

    #[test]
    fn raw_preview_redacts_secret_assignments_and_json_fields() {
        let redacted = redact_secrets(r#"{"sesskey":"abc123","name":"safe"}&token=xyz789"#);
        assert!(!redacted.contains("abc123") && !redacted.contains("xyz789"));
        assert!(redacted.contains(r#""name":"safe""#));
        assert_eq!(redacted.matches("[REDACTED]").count(), 2);
        assert_eq!(redact_secrets("the token is here"), "the token is here");

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
