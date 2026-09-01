use std::{thread, time::Duration};

use serde_json::{Value, json};
use url::Url;
use zeroize::Zeroizing;

use crate::error::AppError;

use super::{
    codes::{self, EasyPoll, PrimaryNext},
    crypto::encrypt_user_data,
    model::{LoginMethod, SecondFactor, StoredCookie},
    prompt::AuthPrompt,
    transport::SsoTransport,
};

const AGENT_ID: &str = "kaist-prod-klms";

pub struct CompletedLogin {
    pub cookies: Vec<StoredCookie>,
    pub devices: Vec<String>,
}

pub fn login(
    klms: &Url,
    sso: &Url,
    timeout: u64,
    method: LoginMethod,
    factor: Option<SecondFactor>,
    previous_devices: &[String],
    prompt: &mut impl AuthPrompt,
) -> Result<CompletedLogin, AppError> {
    let mut transport = SsoTransport::new(klms.clone(), sso.clone(), timeout)?;
    let entry = login_entry(&transport)?;
    transport.get_text(entry)?;
    let identifier = Zeroizing::new(prompt.identifier()?);
    match method {
        LoginMethod::Password => password_login(
            &mut transport,
            &identifier,
            factor.unwrap_or(SecondFactor::Email),
            previous_devices,
            prompt,
        )?,
        LoginMethod::Easy => easy_login(&mut transport, &identifier, previous_devices, prompt)?,
    }
    let cookies = transport.cookies.klms_cookies(transport.klms());
    let mut devices = previous_devices.to_vec();
    devices.extend(transport.cookies.device_values());
    devices.sort();
    devices.dedup();
    if cookies.is_empty() {
        return Err(AppError::auth_protocol(
            "KAIST SSO did not establish a KLMS session",
        ));
    }
    Ok(CompletedLogin { cookies, devices })
}

fn password_login(
    transport: &mut SsoTransport,
    identifier: &str,
    factor: SecondFactor,
    devices: &[String],
    prompt: &mut impl AuthPrompt,
) -> Result<(), AppError> {
    let password = prompt.password()?;
    if password.is_empty() {
        return Err(AppError::usage("password cannot be empty"));
    }
    let key = login_key(transport)?;
    let payload = Zeroizing::new(
        serde_json::to_vec(&json!({
            "login_id": identifier,
            "login_pwd": password.as_str(),
            "agt_id": AGENT_ID,
            "linkUrl": "/user/login/link",
            "device_cd": devices,
        }))
        .map_err(|error| AppError::internal(format!("failed to encode login request: {error}")))?,
    );
    let encrypted = Zeroizing::new(encrypt_user_data(&key, &payload)?);
    let url = transport.sso_url("/auth/user/login/auth")?;
    let response = transport.post_form_json(url, &[("user_data", encrypted.to_string())])?;
    match codes::password_primary(result_code(&response)?)? {
        PrimaryNext::Link => link(transport),
        PrimaryNext::SecondFactor => second_factor(transport, factor, prompt),
        PrimaryNext::Device => register_device(transport),
        PrimaryNext::Duplicate => link(transport),
    }
}

fn second_factor(
    transport: &mut SsoTransport,
    factor: SecondFactor,
    prompt: &mut impl AuthPrompt,
) -> Result<(), AppError> {
    let view = transport.sso_url("/auth/kaist/user/login/second/view")?;
    transport.post_form_follow(
        view,
        &[
            ("user_gubun", "user".into()),
            ("linkUrl", "/user/login/link".into()),
        ],
    )?;
    let (endpoint, channel) = match factor {
        SecondFactor::Email => ("/auth/kaist/user/login/second/ajaxSendMail", "email"),
        SecondFactor::Sms => ("/auth/kaist/user/login/second/ajaxSendSms", "SMS"),
    };
    let response = transport.post_form_json(transport.sso_url(endpoint)?, &[])?;
    let code = result_code(&response)?;
    if code != "SS0001" {
        return match code {
            "ES0003" => Err(AppError::auth(
                "KAIST has no usable destination for that verification method",
                "Retry with the other `--second-factor` value.",
            )),
            "ES0018" => Err(AppError::auth(
                "KAIST verification requests are temporarily limited",
                "Wait, then retry login.",
            )),
            "EMS_FAIL" | "UMS_FAIL" | "EMS_ERR_CONNECT" | "UMS_ERR_CONNECT" => Err(
                AppError::network("KAIST could not deliver the verification code"),
            ),
            other => Err(AppError::auth_protocol(format!(
                "KAIST returned unknown verification-send code {other:?}"
            ))),
        };
    }
    prompt.notice("KAIST sent a verification code. It expires in three minutes.");
    let otp = prompt.otp(channel)?;
    if otp.len() != 6 || !otp.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(AppError::usage(
            "verification code must contain exactly six digits",
        ));
    }
    let response = transport.post_form_json(
        transport.sso_url("/auth/kaist/user/login/second/ajaxValidCrtfcNo")?,
        &[("crtfc_no", otp.to_string())],
    )?;
    match codes::otp(result_code(&response)?)? {
        PrimaryNext::Link | PrimaryNext::Duplicate => link(transport),
        PrimaryNext::Device => register_device(transport),
        PrimaryNext::SecondFactor => unreachable!(),
    }
}

fn easy_login(
    transport: &mut SsoTransport,
    identifier: &str,
    previous_devices: &[String],
    prompt: &mut impl AuthPrompt,
) -> Result<(), AppError> {
    let key = login_key(transport)?;
    let payload = Zeroizing::new(
        serde_json::to_vec(&json!({"login_id": identifier, "agt_id": AGENT_ID})).map_err(
            |error| AppError::internal(format!("failed to encode Easy Login request: {error}")),
        )?,
    );
    let encrypted = Zeroizing::new(encrypt_user_data(&key, &payload)?);
    let response = transport.post_form_json(
        transport.sso_url("/auth/twofactor/mfa/init")?,
        &[("user_data", encrypted.to_string())],
    )?;
    let code = result_code(&response)?;
    if code == "ESY008" {
        return Err(AppError::auth(
            "Easy Login is not registered for this account",
            "Use `klms auth login --method password`.",
        ));
    }
    if !code.is_empty() && code != "SS0001" {
        return Err(AppError::auth_protocol(format!(
            "KAIST returned unknown Easy Login init code {code:?}"
        )));
    }
    let (_, challenge_html) = transport.post_form_follow(
        transport.sso_url("/auth/twofactor/mfa/login2Factor")?,
        &[("linkUrl", "/user/login/link".into())],
    )?;
    if let Some(display) =
        crate::parse::easy_login_code(&challenge_html).or_else(|| display_code(&response))
    {
        prompt.notice(&format!(
            "Approve Easy Login in the KAIST app. Confirmation code: {display}"
        ));
    } else {
        prompt.notice("Approve the Easy Login request in the KAIST app within three minutes.");
    }
    let mut approved = false;
    for _ in 0..60 {
        let response =
            transport.post_form_json(transport.sso_url("/auth/twofactor/mfa/auth")?, &[])?;
        match codes::easy_poll(result_code(&response)?)? {
            EasyPoll::Approved => {
                approved = true;
                break;
            }
            EasyPoll::Pending => thread::sleep(Duration::from_secs(3)),
        }
    }
    if !approved {
        return Err(AppError::auth(
            "Easy Login timed out",
            "Run `klms auth login --method easy` to start again.",
        ));
    }
    let mut form = previous_devices
        .iter()
        .map(|value| ("device", value.clone()))
        .collect::<Vec<_>>();
    form.extend(
        transport
            .cookies
            .device_values()
            .into_iter()
            .map(|value| ("device", value)),
    );
    let response = transport.post_form_json(
        transport.sso_url("/auth/kaist/user/login/check/policy")?,
        &form,
    )?;
    match codes::policy(result_code(&response)?)? {
        PrimaryNext::Link | PrimaryNext::Duplicate => link(transport),
        PrimaryNext::Device => register_device(transport),
        PrimaryNext::SecondFactor => unreachable!(),
    }
}

fn login_key(transport: &mut SsoTransport) -> Result<Zeroizing<String>, AppError> {
    let response = transport.post_form_json(transport.sso_url("/auth/user/login/init")?, &[])?;
    let key = response
        .get("result_data")
        .or_else(|| response.get("resultData"))
        .and_then(Value::as_str)
        .ok_or_else(|| AppError::auth_protocol("KAIST SSO login init omitted its key"))?;
    Ok(Zeroizing::new(key.to_owned()))
}

fn link(transport: &mut SsoTransport) -> Result<(), AppError> {
    let klms_origin = format!(
        "{}://{}{}",
        transport.klms().scheme(),
        transport.klms().host_str().unwrap_or_default(),
        transport
            .klms()
            .port()
            .map(|port| format!(":{port}"))
            .unwrap_or_default()
    );
    let (url, html) = transport.post_form_follow(
        transport.sso_url("/auth/user/login/link")?,
        &[
            ("agt_id", AGENT_ID.into()),
            ("agt_url", klms_origin.clone()),
            ("add_param_url", format!("{klms_origin}/")),
            ("linkUrl", "/user/login/link".into()),
        ],
    )?;
    if transport.is_klms_origin(&url) {
        return Ok(());
    }
    if url.path() != "/auth/user/login/link" {
        return Err(AppError::auth_protocol(format!(
            "KAIST SSO link ended at {}{} instead of KLMS",
            url.host_str().unwrap_or("unknown-host"),
            url.path()
        )));
    }
    let handoff = crate::parse::auth_handoff_form(&html, &url, transport.klms())?;
    let form = handoff
        .fields
        .iter()
        .map(|(name, value)| (name.as_str(), value.clone()))
        .collect::<Vec<_>>();
    let (url, _) = transport.post_form_follow(handoff.action, &form)?;
    if !transport.is_klms_origin(&url) {
        return Err(AppError::auth_protocol(
            "KAIST SSO handoff did not establish a KLMS session",
        ));
    }
    Ok(())
}

fn login_entry(transport: &SsoTransport) -> Result<Url, AppError> {
    let mut url = transport.sso_url("/auth/kaist/user/login/view")?;
    let target = transport.klms().as_str();
    url.query_pairs_mut()
        .append_pair("agt_id", AGENT_ID)
        .append_pair("agt_url", target)
        .append_pair("add_param_url", target);
    Ok(url)
}

fn result_code(value: &Value) -> Result<&str, AppError> {
    value
        .get("result_code")
        .or_else(|| value.get("resultCode"))
        .or_else(|| value.get("errorCode"))
        .or_else(|| value.get("error_code"))
        .or_else(|| value.get("code"))
        .and_then(Value::as_str)
        .or_else(|| value.as_bool().and_then(|ok| ok.then_some("")))
        .or_else(|| {
            value
                .get("result")
                .and_then(Value::as_bool)
                .and_then(|ok| ok.then_some(""))
        })
        .or_else(|| value.get("result_data").is_some().then_some(""))
        .ok_or_else(|| {
            let keys = value
                .as_object()
                .map(|object| object.keys().cloned().collect::<Vec<_>>())
                .unwrap_or_default();
            AppError::auth_protocol(format!(
                "KAIST SSO response omitted its result code (fields: {keys:?})"
            ))
        })
}

fn display_code(value: &Value) -> Option<String> {
    ["display_code", "displayCode", "auth_no", "authNo"]
        .iter()
        .find_map(|key| value.get(key).and_then(Value::as_str))
        .map(str::to_owned)
}

fn register_device(transport: &mut SsoTransport) -> Result<(), AppError> {
    let (_, html) = transport.get_text(transport.sso_url("/auth/kaist/user/device/view")?)?;
    let shape = crate::parse::auth_policy_shape(&html)?;
    const REGISTER: &str = "/auth/kaist/user/device/ajaxRegist";
    const COMPLETE: &str = "/auth/kaist/user/device/login";
    if !shape.actions.iter().any(|action| action == REGISTER)
        || !shape.actions.iter().any(|action| action == COMPLETE)
    {
        return Err(AppError::auth_protocol(
            "KAIST device-registration page omitted its expected actions",
        ));
    }
    let response = transport.post_form_json(transport.sso_url(REGISTER)?, &[])?;
    let code = result_code(&response)?;
    if !matches!(code, "" | "SS0001") {
        return Err(AppError::auth_protocol(format!(
            "KAIST returned unknown device-registration code {code:?}"
        )));
    }
    let device = response
        .get("device_cd")
        .or_else(|| response.get("deviceCd"))
        .and_then(Value::as_str)
        .ok_or_else(|| {
            AppError::auth_protocol("KAIST device registration omitted its identifier")
        })?;
    transport.cookies.remember_device(device)?;
    let (url, _) = transport.get_text(transport.sso_url(COMPLETE)?)?;
    if !transport.is_klms_origin(&url) {
        if url.path() == "/auth/user/login/link" {
            return link(transport);
        }
        return Err(AppError::auth_protocol(format!(
            "KAIST device completion ended at {}{}",
            url.host_str().unwrap_or("unknown-host"),
            url.path()
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::{
        io::{Read, Write},
        net::TcpListener,
        thread,
    };

    use zeroize::Zeroizing;

    use super::*;

    struct FakePrompt;

    impl AuthPrompt for FakePrompt {
        fn identifier(&mut self) -> Result<String, AppError> {
            Ok("student".into())
        }
        fn password(&mut self) -> Result<Zeroizing<String>, AppError> {
            Ok(Zeroizing::new("password".into()))
        }
        fn otp(&mut self, _channel: &str) -> Result<Zeroizing<String>, AppError> {
            Ok(Zeroizing::new("123456".into()))
        }
        fn notice(&mut self, _message: &str) {}
    }

    #[test]
    fn password_email_flow_crosses_only_sso_and_klms_origins() {
        let sso_listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let klms_listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let sso_address = sso_listener.local_addr().unwrap();
        let klms_address = klms_listener.local_addr().unwrap();
        let sso = thread::spawn(move || {
            let responses = [
                (
                    "200 OK",
                    "text/html",
                    "login".to_owned(),
                    Some("sso-session=one; Path=/"),
                ),
                (
                    "200 OK",
                    "application/json",
                    format!(r#"{{"result_data":"{}"}}"#, "00".repeat(48)),
                    None,
                ),
                (
                    "200 OK",
                    "application/json",
                    r#"{"result_code":"SS0098"}"#.into(),
                    None,
                ),
                ("200 OK", "text/html", "second".into(), None),
                (
                    "200 OK",
                    "application/json",
                    r#"{"errorCode":"SS0001"}"#.into(),
                    None,
                ),
                (
                    "200 OK",
                    "application/json",
                    r#"{"result_code":"SS0001"}"#.into(),
                    None,
                ),
                ("302 Found", "text/plain", String::new(), None),
            ];
            for (index, (status, content_type, body, cookie)) in responses.into_iter().enumerate() {
                let (mut stream, _) = sso_listener.accept().unwrap();
                let mut request = [0_u8; 16 * 1024];
                let length = stream.read(&mut request).unwrap();
                let request = String::from_utf8_lossy(&request[..length]);
                if (1..=2).contains(&index) || (4..=5).contains(&index) {
                    assert!(
                        request
                            .to_ascii_lowercase()
                            .contains("x-requested-with: xmlhttprequest")
                    );
                }
                let mut extra = String::new();
                if let Some(cookie) = cookie {
                    extra.push_str(&format!("Set-Cookie: {cookie}\r\n"));
                }
                if index == 6 {
                    extra.push_str(&format!("Location: http://{klms_address}/\r\n"));
                }
                write!(stream, "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\n{extra}Content-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
            }
        });
        let klms = thread::spawn(move || {
            let (mut stream, _) = klms_listener.accept().unwrap();
            let mut request = [0_u8; 4096];
            let length = stream.read(&mut request).unwrap();
            assert!(String::from_utf8_lossy(&request[..length]).starts_with("GET / HTTP/1.1"));
            write!(stream, "HTTP/1.1 200 OK\r\nSet-Cookie: MoodleSession=owned; Path=/; HttpOnly\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok").unwrap();
        });

        let completed = login(
            &Url::parse(&format!("http://{klms_address}/")).unwrap(),
            &Url::parse(&format!("http://{sso_address}/")).unwrap(),
            5,
            LoginMethod::Password,
            Some(SecondFactor::Email),
            &[],
            &mut FakePrompt,
        )
        .unwrap();
        sso.join().unwrap();
        klms.join().unwrap();
        assert_eq!(
            completed.cookies,
            vec![StoredCookie {
                name: "MoodleSession".into(),
                value: "owned".into()
            }]
        );
    }

    #[test]
    fn password_flow_registers_first_device_and_submits_klms_handoff() {
        let sso_listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let klms_listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let sso_address = sso_listener.local_addr().unwrap();
        let klms_address = klms_listener.local_addr().unwrap();
        let sso_origin = format!("http://{sso_address}");
        let sso = thread::spawn(move || {
            let handoff = format!(
                r#"<form action="http://{klms_address}/login/ssologin.php" method="post"><input type="hidden" name="ticket" value="opaque"></form>"#
            );
            let responses = [
                (
                    "200 OK",
                    "text/html",
                    "login".to_owned(),
                    Some("sso-session=one; Path=/"),
                    None,
                ),
                (
                    "200 OK",
                    "application/json",
                    format!(r#"{{"result_data":"{}"}}"#, "00".repeat(48)),
                    None,
                    None,
                ),
                (
                    "200 OK",
                    "application/json",
                    r#"{"result_code":"SS0099"}"#.into(),
                    None,
                    None,
                ),
                (
                    "200 OK",
                    "text/html",
                    r#"<script>fetch('/auth/kaist/user/device/ajaxRegist'); location.href='/auth/kaist/user/device/login';</script>"#.into(),
                    None,
                    None,
                ),
                (
                    "200 OK",
                    "application/json",
                    r#"{"code":"","device_cd":"trusted-device"}"#.into(),
                    None,
                    None,
                ),
                (
                    "302 Found",
                    "text/plain",
                    String::new(),
                    None,
                    Some("/auth/user/login/link"),
                ),
                (
                    "200 OK",
                    "text/html",
                    "link".into(),
                    None,
                    None,
                ),
                ("200 OK", "text/html", handoff, None, None),
            ];
            for (index, (status, content_type, body, cookie, location)) in
                responses.into_iter().enumerate()
            {
                let (mut stream, _) = sso_listener.accept().unwrap();
                let mut request = [0_u8; 16 * 1024];
                let length = stream.read(&mut request).unwrap();
                let request = String::from_utf8_lossy(&request[..length]);
                if matches!(index, 1 | 2 | 4) {
                    assert!(
                        request
                            .to_ascii_lowercase()
                            .contains("x-requested-with: xmlhttprequest")
                    );
                }
                if index == 4 {
                    let lower = request.to_ascii_lowercase();
                    assert!(lower.contains("referer: "));
                    assert!(lower.contains("/auth/kaist/user/device/view"));
                    assert!(lower.contains(&format!("origin: {sso_origin}").to_ascii_lowercase()));
                }
                let mut extra = String::new();
                if let Some(cookie) = cookie {
                    extra.push_str(&format!("Set-Cookie: {cookie}\r\n"));
                }
                if let Some(location) = location {
                    extra.push_str(&format!("Location: {location}\r\n"));
                }
                write!(stream, "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\n{extra}Content-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
            }
        });
        let klms = thread::spawn(move || {
            let (mut stream, _) = klms_listener.accept().unwrap();
            let mut request = [0_u8; 4096];
            let length = stream.read(&mut request).unwrap();
            let request = String::from_utf8_lossy(&request[..length]);
            assert!(request.starts_with("POST /login/ssologin.php HTTP/1.1"));
            assert!(request.contains("ticket=opaque"));
            write!(stream, "HTTP/1.1 302 Found\r\nLocation: /\r\nSet-Cookie: MoodleSession=owned; Path=/; HttpOnly\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").unwrap();

            let (mut stream, _) = klms_listener.accept().unwrap();
            let mut request = [0_u8; 4096];
            let length = stream.read(&mut request).unwrap();
            assert!(String::from_utf8_lossy(&request[..length]).starts_with("GET / HTTP/1.1"));
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok"
            )
            .unwrap();
        });

        let completed = login(
            &Url::parse(&format!("http://{klms_address}/")).unwrap(),
            &Url::parse(&format!("http://{sso_address}/")).unwrap(),
            5,
            LoginMethod::Password,
            Some(SecondFactor::Email),
            &[],
            &mut FakePrompt,
        )
        .unwrap();
        sso.join().unwrap();
        klms.join().unwrap();
        assert_eq!(completed.devices, vec!["trusted-device"]);
        assert_eq!(
            completed.cookies,
            vec![StoredCookie {
                name: "MoodleSession".into(),
                value: "owned".into()
            }]
        );
    }
}
