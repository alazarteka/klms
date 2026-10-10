//! End-to-end tests of the login orchestration against loopback SSO and KLMS
//! servers. Passwords go to a plaintext file in a temp dir, never a keychain.

use std::{
    cell::RefCell,
    fs,
    rc::Rc,
    sync::{Arc, Mutex},
};

use tempfile::TempDir;

use super::*;
use crate::fixture_server::{Request, Response, Server};

/// A fixture server plus its base URL; requests are logged as the lowercased
/// request line, headers and body.
struct Site {
    server: Server,
    url: Url,
}

impl Site {
    fn start(route: impl Fn(&Request) -> Response + Send + Sync + 'static) -> Self {
        let server = Server::new(route);
        let url = Url::parse(&format!("{}/", server.url())).unwrap();
        Self { server, url }
    }

    /// Every logged request to `path`.
    fn requests(&self, path: &str) -> Vec<String> {
        let logged = self.server.recorded().into_iter();
        let hits =
            logged.filter(|r| r.line.split_whitespace().nth(1).map(strip_query) == Some(path));
        hits.map(|r| {
            format!("{}\r\n{}\r\n\r\n{}", r.line, r.headers.join("\r\n"), r.body)
                .to_ascii_lowercase()
        })
        .collect()
    }

    fn count(&self, path: &str) -> usize {
        self.requests(path).len()
    }

    fn total(&self) -> usize {
        self.server.recorded().len()
    }
}

fn strip_query(target: &str) -> &str {
    target.split('?').next().unwrap_or_default()
}

const SEND_MAIL: &str = "/auth/kaist/user/login/second/ajaxSendMail";
const VALIDATE: &str = "/auth/kaist/user/login/second/ajaxValidCrtfcNo";
const DEVICE_VIEW: &str = "/auth/kaist/user/device/view";
const COOKIE: &str = "MoodleSession=owned; Path=/; HttpOnly";

fn redirect(location: &str) -> Response {
    Response::html("")
        .status("302 Found")
        .header("Location", location)
}

/// KAIST SSO. `device` makes the password step demand a trusted-device
/// registration (and a KLMS handoff form) instead of a second factor.
fn sso_route(
    klms: String,
    otp: Arc<Mutex<&'static str>>,
    device: bool,
) -> impl Fn(&Request) -> Response + Send + Sync {
    move |request| {
        let path = strip_query(&request.target);
        let json = |key: &str, value: &str| Response::html(format!(r#"{{"{key}":"{value}"}}"#));
        match (request.method.as_str(), path) {
            (_, "/auth/kaist/user/login/view") => {
                Response::html("login").header("Set-Cookie", "sso-session=one; Path=/")
            }
            (_, "/auth/user/login/init") => json("result_data", &"00".repeat(48)),
            (_, "/auth/user/login/auth") => {
                json("result_code", if device { "SS0099" } else { "SS0098" })
            }
            (_, SEND_MAIL) => json("errorCode", "SS0001"),
            (_, VALIDATE) => json("result_code", *otp.lock().unwrap()),
            (_, DEVICE_VIEW) => Response::html(
                "<script>fetch('/auth/kaist/user/device/ajaxRegist'); location.href='/auth/kaist/user/device/login';</script>",
            ),
            (_, "/auth/kaist/user/device/ajaxRegist") => {
                Response::html(r#"{"code":"","device_cd":"trusted-device"}"#)
            }
            (_, "/auth/kaist/user/device/login") => redirect("/auth/user/login/link"),
            ("POST", "/auth/user/login/link") if device => Response::html(format!(
                r#"<form action="{klms}login/ssologin.php"><input type="hidden" name="ticket" value="opaque"></form>"#
            )),
            ("POST", "/auth/user/login/link") => redirect(&klms),
            (_, "/auth/kaist/user/login/second/view" | "/auth/user/login/link") => {
                Response::html("page")
            }
            _ => panic!("unexpected SSO request {path}"),
        }
    }
}

fn klms_route(request: &Request) -> Response {
    match strip_query(&request.target) {
        "/login/ssologin.php" => redirect("/").header("Set-Cookie", COOKIE),
        "/" => Response::html("ok").header("Set-Cookie", COOKIE),
        path => panic!("unexpected KLMS request {path}"),
    }
}

/// Answers every prompt with fixed values and records what was asked.
struct Terminal(Rc<RefCell<Vec<&'static str>>>);

impl Terminal {
    fn ask(&self, what: &'static str, answer: &str) -> Zeroizing<String> {
        self.0.borrow_mut().push(what);
        Zeroizing::new(answer.into())
    }
}

impl AuthPrompt for Terminal {
    fn identifier(&mut self) -> Result<String, AppError> {
        Ok(self.ask("id", "student").to_string())
    }
    fn password(&mut self) -> Result<Zeroizing<String>, AppError> {
        Ok(self.ask("password", "s3cret"))
    }
    fn otp(&mut self, _channel: &str) -> Result<Zeroizing<String>, AppError> {
        Ok(self.ask("otp", "123456"))
    }
}

struct Fixture {
    sso: Site,
    klms: Site,
    otp: Arc<Mutex<&'static str>>,
    dirs: Dirs,
    secrets: Secrets,
    asked: Rc<RefCell<Vec<&'static str>>>,
    _root: TempDir,
}

fn remember() -> LoginOptions {
    LoginOptions {
        method: Some(LoginMethod::Password),
        factor: Some(SecondFactor::Email),
        remember_password: true,
        insecure_storage: true,
        ..LoginOptions::default()
    }
}

fn code(code: &str) -> LoginOptions {
    LoginOptions {
        code: Some(code.into()),
        ..LoginOptions::default()
    }
}

fn failure(result: Result<CommandResult, AppError>) -> AppError {
    result.err().expect("expected a failure")
}

impl Fixture {
    fn new() -> Self {
        Self::with(false)
    }

    fn with(device: bool) -> Self {
        let root = tempfile::tempdir().unwrap();
        let dirs = Dirs {
            config: root.path().join("config/klms"),
            state: root.path().join("state/klms"),
        };
        let secrets = Secrets {
            mac: false,
            security: "/nonexistent/security".into(),
            // No `secret-tool` anywhere: only the plaintext file is available.
            search_path: root.path().join("bin").into_os_string(),
            file: dirs.credentials(),
        };
        let otp = Arc::new(Mutex::new("SS0001"));
        let klms = Site::start(klms_route);
        let sso = Site::start(sso_route(klms.url.to_string(), otp.clone(), device));
        Self {
            sso,
            klms,
            otp,
            dirs,
            secrets,
            asked: Rc::default(),
            _root: root,
        }
    }

    /// Run `klms auth login`; `defer` is the `--json`/no-terminal behavior.
    fn login(
        &self,
        interactive: bool,
        defer: bool,
        options: &LoginOptions,
    ) -> Result<CommandResult, AppError> {
        self.login_as(interactive, defer, options, Terminal(self.asked.clone()))
    }

    fn login_as(
        &self,
        interactive: bool,
        defer: bool,
        options: &LoginOptions,
        prompt: impl AuthPrompt,
    ) -> Result<CommandResult, AppError> {
        self.asked.borrow_mut().clear();
        let env = Env {
            dirs: &self.dirs,
            klms: &self.klms.url,
            sso: &self.sso.url,
            timeout: 5,
            secrets: &self.secrets,
            interactive,
            defer_code: defer,
            json: defer,
        };
        login_with(&env, options, prompt)
    }

    fn asked(&self) -> Vec<&'static str> {
        self.asked.borrow().clone()
    }

    fn identity(&self) -> Option<Identity> {
        store::load_identity(&self.dirs.identity()).unwrap()
    }

    fn password(&self, user: &str) -> Option<String> {
        let found = Backend::File(self.dirs.credentials()).lookup(user).unwrap();
        found.map(|password| password.to_string())
    }

    /// A previous interactive `--remember-password` login, without KAIST.
    fn seed_remembered_password(&self) {
        let mut identity =
            Identity::new("student", LoginMethod::Password, Some(SecondFactor::Email));
        identity.password_backend = Some(secret::PLAINTEXT_FILE.into());
        store::save_json(&self.dirs.identity(), &identity, "login").unwrap();
        Backend::File(self.dirs.credentials())
            .store("student", "s3cret")
            .unwrap();
    }

    /// Start a non-interactive login and stop at the pending code.
    fn pend(&self) -> AppError {
        failure(self.login(false, true, &LoginOptions::default()))
    }
}

#[test]
fn remember_password_stores_identity_and_secret_after_success() {
    let fx = Fixture::new();
    let result = fx.login(true, false, &remember()).unwrap();
    assert_eq!(result.data["user"], "student");
    assert_eq!(result.data["password_backend"], "plaintext-file");
    assert_eq!(fx.asked(), ["id", "password", "otp"]);
    let identity = fx.identity().unwrap();
    assert_eq!(identity.username, "student");
    assert_eq!(identity.method, LoginMethod::Password);
    assert_eq!(identity.second_factor, Some(SecondFactor::Email));
    assert_eq!(identity.password_backend.as_deref(), Some("plaintext-file"));
    assert_eq!(fx.password("student").as_deref(), Some("s3cret"));
    // Neither file the login wrote exposes the password; only KLMS-issued
    // cookies reach the session, and KLMS was only asked for its home page.
    let session = fs::read_to_string(fx.dirs.session()).unwrap();
    assert!(session.contains("MoodleSession") && !session.contains("sso-session"));
    let login = fs::read_to_string(fx.dirs.identity()).unwrap();
    assert!(!session.contains("s3cret") && !login.contains("s3cret"));
    assert_eq!((fx.klms.count("/"), fx.klms.total()), (1, 1));
    // The AJAX endpoints are called as XMLHttpRequest.
    for path in [
        "/auth/user/login/init",
        "/auth/user/login/auth",
        SEND_MAIL,
        VALIDATE,
    ] {
        let request = &fx.sso.requests(path)[0];
        assert!(
            request.contains("x-requested-with: xmlhttprequest"),
            "{path}"
        );
    }
}

#[test]
fn a_prompted_identifier_is_validated_before_it_is_remembered() {
    struct BadId;
    impl AuthPrompt for BadId {
        fn identifier(&mut self) -> Result<String, AppError> {
            Ok("stu\u{1}dent".into())
        }
        fn password(&mut self) -> Result<Zeroizing<String>, AppError> {
            Ok(Zeroizing::new("s3cret".into()))
        }
        fn otp(&mut self, _channel: &str) -> Result<Zeroizing<String>, AppError> {
            Ok(Zeroizing::new("123456".into()))
        }
    }
    let fx = Fixture::new();
    let error = failure(fx.login_as(true, false, &remember(), BadId));
    assert_eq!(error.code, "USAGE");
    assert!(fx.identity().is_none() && !fx.dirs.session().exists());
}

#[test]
fn a_failed_login_remembers_nothing() {
    let fx = Fixture::new();
    *fx.otp.lock().unwrap() = "E001";
    assert_eq!(
        failure(fx.login(true, false, &remember())).code,
        "AUTH_REQUIRED"
    );
    assert!(fx.identity().is_none() && fx.password("student").is_none());
    assert!(!fx.dirs.session().exists());
}

#[test]
fn next_login_reuses_remembered_user_method_factor_and_password() {
    let fx = Fixture::new();
    fx.login(true, false, &remember()).unwrap();
    // Removing the session (what `auth logout` does) keeps the identity.
    fs::remove_file(fx.dirs.session()).unwrap();
    let result = fx.login(true, false, &LoginOptions::default()).unwrap();
    assert_eq!(fx.asked(), ["otp"], "id and password come from storage");
    assert_eq!(result.data["method"], "password");
    assert_eq!(result.data["second_factor"], "email");
    assert_eq!(result.data["user"], "student");
    assert_eq!(fx.sso.count(SEND_MAIL), 2);
}

#[test]
fn explicit_flags_override_and_update_remembered_values() {
    let fx = Fixture::new();
    fx.login(true, false, &remember()).unwrap();
    // A second-factor flag with an easy method is a usage error and changes nothing.
    let bad = LoginOptions {
        method: Some(LoginMethod::Easy),
        factor: Some(SecondFactor::Sms),
        ..LoginOptions::default()
    };
    assert_eq!(failure(fx.login(true, false, &bad)).code, "USAGE");
    assert_eq!(
        fx.identity().unwrap().second_factor,
        Some(SecondFactor::Email)
    );

    // Switch account: the old account's stored password is dropped, not reused.
    let other = LoginOptions {
        user: Some("other".into()),
        ..LoginOptions::default()
    };
    fx.login(true, false, &other).unwrap();
    assert_eq!(fx.asked(), ["password", "otp"]);
    let identity = fx.identity().unwrap();
    assert_eq!(identity.username, "other");
    assert_eq!(
        identity.method,
        LoginMethod::Password,
        "method stays remembered"
    );
    assert_eq!(identity.password_backend, None);
    assert!(fx.password("student").is_none());
}

#[test]
fn remember_password_is_refused_before_any_request() {
    let fx = Fixture::new();
    // No keyring and no --insecure-storage.
    let no_consent = LoginOptions {
        insecure_storage: false,
        ..remember()
    };
    let error = failure(fx.login(true, false, &no_consent));
    assert!(error.message.contains("no OS keyring"));
    // Needs a terminal to read the password, and a password method.
    assert_eq!(failure(fx.login(false, true, &remember())).code, "USAGE");
    let easy = LoginOptions {
        method: Some(LoginMethod::Easy),
        remember_password: true,
        ..LoginOptions::default()
    };
    assert_eq!(failure(fx.login(true, false, &easy)).code, "USAGE");
    assert!(fx.sso.total() == 0 && fx.asked().is_empty());
}

#[test]
fn non_interactive_login_is_two_steps_through_a_pending_file() {
    let fx = Fixture::new();
    fx.seed_remembered_password();
    let error = fx.pend();
    assert_eq!((error.code, error.exit_code()), ("CODE_REQUIRED", 12));
    let details = error.details.unwrap();
    assert_eq!(details["channel"], "email");
    assert_eq!(details["resume"], "klms auth login --code CODE");
    let (expires, now) = (details["expires_at"].as_u64().unwrap(), epoch_now() as u64);
    assert!(
        (now + 295..=now + 301).contains(&expires),
        "{expires} vs {now}"
    );
    assert!(fx.asked().is_empty(), "never prompts without a terminal");
    assert!(!fx.dirs.session().exists() && fx.sso.count(VALIDATE) == 0);
    let pending = fs::read_to_string(fx.dirs.pending()).unwrap();
    assert!(
        !pending.contains("s3cret"),
        "the password is never persisted here"
    );
    assert!(
        pending.contains("\"sso-session\""),
        "the SSO cookie travels with it"
    );

    let result = fx.login(false, true, &code("654321")).unwrap();
    assert_eq!(result.data["user"], "student");
    assert_eq!(result.data["method"], "password");
    assert!(fx.dirs.session().is_file() && !fx.dirs.pending().exists());
    let validates = fx.sso.requests(VALIDATE);
    assert_eq!(validates.len(), 1);
    assert!(validates[0].contains("crtfc_no=654321"));
    // Resuming did not resend the code or log in again, and reached KLMS
    // with the rebuilt jar.
    assert_eq!(fx.sso.count(SEND_MAIL), 1);
    assert_eq!(fx.sso.count("/auth/user/login/auth"), 1);
    assert_eq!(fx.sso.count("/auth/user/login/link"), 1);
    let backend = fx.identity().unwrap().password_backend;
    assert_eq!(backend.as_deref(), Some("plaintext-file"));
}

#[test]
fn a_wrong_code_consumes_the_pending_login_and_an_expired_one_is_rejected() {
    let fx = Fixture::new();
    fx.seed_remembered_password();
    let message = |options: &LoginOptions| failure(fx.login(false, true, options)).message;
    assert!(message(&code("123456")).contains("no login is waiting"));

    fx.pend();
    *fx.otp.lock().unwrap() = "E001";
    assert!(message(&code("000000")).contains("incorrect"));
    assert!(!fx.dirs.pending().exists());
    assert!(message(&code("000000")).contains("no login is waiting"));

    fx.pend();
    let path = fx.dirs.pending();
    let mut pending: store::PendingLogin =
        serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    pending.expires_at = epoch_now() as u64 - 1;
    store::save_json(&path, &pending, "pending login").unwrap();
    let validations = fx.sso.count(VALIDATE);
    assert!(message(&code("123456")).contains("expired"));
    assert!(!path.exists());
    assert_eq!(
        fx.sso.count(VALIDATE),
        validations,
        "expired logins never reach KAIST"
    );
}

#[test]
fn deferring_the_code_still_remembers_what_kaist_accepted() {
    // A terminal user who asked for JSON output gets the two-step result and
    // no code prompt; the stored password is used and not asked for again.
    let fx = Fixture::new();
    fx.seed_remembered_password();
    assert_eq!(
        failure(fx.login(true, true, &LoginOptions::default())).code,
        "CODE_REQUIRED"
    );
    assert!(fx.asked().is_empty());

    // With --remember-password the password is remembered once KAIST took it.
    let fx = Fixture::new();
    assert_eq!(
        failure(fx.login(true, true, &remember())).code,
        "CODE_REQUIRED"
    );
    assert_eq!(fx.password("student").as_deref(), Some("s3cret"));
    let backend = fx.identity().unwrap().password_backend;
    assert_eq!(backend.as_deref(), Some("plaintext-file"));
}

#[test]
fn forget_discards_a_corrupt_login_file() {
    let fx = Fixture::new();
    fs::create_dir_all(&fx.dirs.config).unwrap();
    fs::write(fx.dirs.identity(), "{broken").unwrap();
    let result = forget_with(&fx.dirs, &fx.secrets).unwrap();
    assert_eq!(result.data["login_removed"], true);
}

#[test]
fn device_registration_and_klms_handoff_across_two_origins() {
    let fx = Fixture::with(true);
    let result = fx.login(true, false, &remember()).unwrap();
    assert_eq!(
        fx.asked(),
        ["id", "password"],
        "no second factor was needed"
    );
    assert_eq!(result.data["device_count"], 1);
    assert_eq!(result.data["cookie_count"], 1);
    let session = fs::read_to_string(fx.dirs.session()).unwrap();
    assert!(session.contains("trusted-device") && session.contains("MoodleSession"));
    // The registration POST carries the device page as Referer and the SSO origin.
    let register = &fx.sso.requests("/auth/kaist/user/device/ajaxRegist")[0];
    assert!(register.contains("referer: ") && register.contains(DEVICE_VIEW));
    let origin = fx.sso.url.origin().ascii_serialization();
    assert!(register.contains(&format!("origin: {origin}")));
    // The handoff form was submitted to KLMS with its hidden ticket.
    let handoff = &fx.klms.requests("/login/ssologin.php")[0];
    assert!(handoff.starts_with("post ") && handoff.contains("ticket=opaque"));
}
