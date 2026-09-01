use scraper::{Html, Selector};
use url::Url;

use crate::error::AppError;

#[derive(Debug)]
pub struct AuthPolicyShape {
    pub actions: Vec<String>,
}

pub struct AuthHandoffForm {
    pub action: Url,
    pub fields: Vec<(String, String)>,
}

pub fn auth_handoff_form(
    html: &str,
    document_url: &Url,
    klms: &Url,
) -> Result<AuthHandoffForm, AppError> {
    let document = Html::parse_document(html);
    let forms = Selector::parse("form[action]").expect("valid selector");
    let inputs = Selector::parse("input[name]").expect("valid selector");
    for form in document.select(&forms) {
        let Some(action) = form.value().attr("action") else {
            continue;
        };
        let Ok(action) = document_url.join(action) else {
            continue;
        };
        if action.scheme() != klms.scheme()
            || action.host_str() != klms.host_str()
            || action.port_or_known_default() != klms.port_or_known_default()
        {
            continue;
        }
        let fields = form
            .select(&inputs)
            .filter_map(|input| {
                let name = input.value().attr("name")?;
                let value = input.value().attr("value").unwrap_or_default();
                (valid_field_name(name) && value.len() <= 16 * 1024)
                    .then(|| (name.to_owned(), value.to_owned()))
            })
            .take(32)
            .collect::<Vec<_>>();
        if fields.is_empty() {
            return Err(AppError::auth_protocol(
                "KAIST SSO handoff form contained no fields",
            ));
        }
        return Ok(AuthHandoffForm { action, fields });
    }
    Err(AppError::auth_protocol(
        "KAIST SSO link page omitted its KLMS handoff form",
    ))
}

fn valid_field_name(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
}

pub fn auth_policy_shape(html: &str) -> Result<AuthPolicyShape, AppError> {
    let document = Html::parse_document(html);
    let action_selector = Selector::parse("button[onclick], a[href]").expect("valid selector");
    let mut actions = document
        .select(&action_selector)
        .flat_map(|element| {
            [
                element.value().attr("onclick"),
                element.value().attr("href"),
            ]
            .into_iter()
            .flatten()
        })
        .filter_map(auth_path)
        .collect::<Vec<_>>();
    for script in document.select(&Selector::parse("script:not([src])").expect("valid selector")) {
        let text = script.text().collect::<String>();
        actions.extend(auth_paths(&text));
    }
    actions.sort();
    actions.dedup();
    Ok(AuthPolicyShape { actions })
}

pub fn easy_login_code(html: &str) -> Option<String> {
    let document = Html::parse_document(html);
    let accessible = Selector::parse(".auth_number .sr-only").expect("valid selector");
    if let Some(code) = document
        .select(&accessible)
        .map(|element| element.text().collect::<String>())
        .find(|text| digits_only(text).is_some())
    {
        return digits_only(code);
    }
    let visible = Selector::parse(".auth_number .nember_wrap span").expect("valid selector");
    let code = document
        .select(&visible)
        .flat_map(|element| element.text())
        .collect::<String>();
    digits_only(code)
}

fn digits_only(value: impl AsRef<str>) -> Option<String> {
    let digits = value
        .as_ref()
        .chars()
        .filter(|character| character.is_ascii_digit())
        .collect::<String>();
    (!digits.is_empty() && digits.len() <= 8).then_some(digits)
}

fn auth_paths(text: &str) -> Vec<String> {
    let mut paths = Vec::new();
    let mut rest = text;
    while let Some(index) = rest.find("/auth/") {
        rest = &rest[index..];
        let end = rest
            .find(|character: char| {
                character.is_ascii_whitespace()
                    || matches!(character, '\'' | '"' | '`' | '<' | '>' | ')' | '}' | ']')
            })
            .unwrap_or(rest.len());
        paths.push(rest[..end].trim_end_matches([';', ',']).to_owned());
        rest = &rest[end..];
    }
    paths
}

fn auth_path(text: &str) -> Option<String> {
    auth_paths(text).into_iter().next()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_structure_without_values_or_visible_text() {
        let html = r#"<form action="/auth/device/save"><input name="device_name" value="secret"><button onclick="post('/auth/device/check')">Save</button></form>"#;
        let shape = auth_policy_shape(html).unwrap();
        assert_eq!(shape.actions, ["/auth/device/check"]);
    }

    #[test]
    fn extracts_accessible_easy_login_code() {
        let html = r#"<div class="auth_number"><div class="nember_wrap"><span>8</span><span>0</span></div><div class="sr-only">80</div></div>"#;
        assert_eq!(easy_login_code(html).as_deref(), Some("80"));
    }

    #[test]
    fn accepts_only_handoff_forms_targeting_klms() {
        let document = Url::parse("https://sso.kaist.ac.kr/auth/user/login/link").unwrap();
        let klms = Url::parse("https://klms.kaist.ac.kr/").unwrap();
        let html = r#"<form action="https://evil.example/steal"><input name="ticket" value="bad"></form><form action="https://klms.kaist.ac.kr/login/ssologin.php"><input type="hidden" name="ticket" value="opaque"></form>"#;
        let form = auth_handoff_form(html, &document, &klms).unwrap();
        assert_eq!(form.action.path(), "/login/ssologin.php");
        assert_eq!(form.fields, [("ticket".into(), "opaque".into())]);
    }
}
