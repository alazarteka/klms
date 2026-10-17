//! End-to-end tests of the login orchestration against loopback SSO and KLMS
//! servers. Passwords go to a plaintext file in a temp dir, never a keychain.

use std::{
    cell::RefCell,
    fs,
    path::PathBuf,
    rc::Rc,
    sync::{Arc, Mutex},
};

use tempfile::TempDir;

use serde_json::Value;

use super::*;
use crate::fixture_executable::write_executable;
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
    password_result: Arc<Mutex<Option<&'static str>>>,
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
                let accepted = if device { "SS0099" } else { "SS0098" };
                json(
                    "result_code",
                    password_result.lock().unwrap().unwrap_or(accepted),
                )
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
    /// KAIST's answer to the password step; `None` accepts it.
    password_result: Arc<Mutex<Option<&'static str>>>,
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
        let password_result = Arc::new(Mutex::new(None));
        let klms = Site::start(klms_route);
        let sso = Site::start(sso_route(
            klms.url.to_string(),
            otp.clone(),
            password_result.clone(),
            device,
        ));
        Self {
            sso,
            klms,
            otp,
            password_result,
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
fn a_stored_password_kaist_rejects_is_removed_so_it_is_not_retried() {
    let fx = Fixture::new();
    fx.seed_remembered_password();
    *fx.password_result.lock().unwrap() = Some("EAU001");
    let error = failure(fx.login(false, true, &LoginOptions::default()));
    assert_eq!(error.code, "CREDENTIALS_REJECTED");
    assert!(error.hint.unwrap().contains("--remember-password"));
    assert_eq!(fx.password("student"), None);
    let identity = fx.identity().unwrap();
    assert_eq!(identity.username, "student");
    assert_eq!(identity.password_backend, None);

    let attempts = fx.sso.count("/auth/user/login/auth");
    let again = failure(fx.login(false, true, &LoginOptions::default()));
    assert_eq!(again.code, "AUTH_REQUIRED");
    assert_eq!(fx.sso.count("/auth/user/login/auth"), attempts);
}

#[test]
fn a_lockout_or_a_rejected_typed_password_keeps_what_is_stored() {
    let fx = Fixture::new();
    fx.seed_remembered_password();
    *fx.password_result.lock().unwrap() = Some("EAU005");
    let locked = failure(fx.login(false, true, &LoginOptions::default()));
    assert_eq!(locked.code, "AUTH_REQUIRED");
    assert_eq!(fx.password("student").as_deref(), Some("s3cret"));

    let typed = LoginOptions {
        user: Some("other".into()),
        ..LoginOptions::default()
    };
    *fx.password_result.lock().unwrap() = Some("EAU001");
    let rejected = failure(fx.login(true, false, &typed));
    assert_eq!(rejected.code, "CREDENTIALS_REJECTED");
    assert_eq!(fx.asked(), ["password"]);
    assert_eq!(fx.password("student").as_deref(), Some("s3cret"));
    let identity = fx.identity().unwrap();
    assert_eq!(identity.password_backend.as_deref(), Some("plaintext-file"));
}

#[test]
fn a_rejected_password_the_keyring_cannot_delete_is_not_reported_removed() {
    let mut fx = Fixture::new();
    let (backend, root) = fake_keyring(&mut fx);
    backend.store("student", "s3cret").unwrap();
    let mut identity = Identity::new("student", LoginMethod::Password, Some(SecondFactor::Email));
    identity.password_backend = Some("secret-service".into());
    store::save_json(&fx.dirs.identity(), &identity, "login").unwrap();
    fs::write(root.join("fail-clear"), "").unwrap();
    *fx.password_result.lock().unwrap() = Some("EAU001");
    let error = failure(fx.login(false, true, &LoginOptions::default()));
    assert_eq!(error.code, "CREDENTIALS_REJECTED");
    assert!(error.hint.unwrap().contains("could not be deleted"));
    assert!(
        error
            .message
            .contains("could not remove the rejected password")
    );
    assert_eq!(fx.identity().unwrap().password_backend, None);
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
        "CODE_INCORRECT"
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
    // Exit 10 for all of these; the stable code tells them apart.
    let message = |options: &LoginOptions, want: &str| {
        let error = failure(fx.login(false, true, options));
        assert_eq!((error.code, error.exit_code()), (want, 10));
        error.message
    };
    assert!(message(&code("123456"), "CODE_EXPIRED").contains("no login is waiting"));

    fx.pend();
    *fx.otp.lock().unwrap() = "E001";
    assert!(message(&code("000000"), "CODE_INCORRECT").contains("incorrect"));
    assert!(!fx.dirs.pending().exists());
    assert!(message(&code("000000"), "CODE_EXPIRED").contains("no login is waiting"));

    fx.pend();
    *fx.otp.lock().unwrap() = "E002";
    assert!(message(&code("000000"), "CODE_EXPIRED").contains("expired"));

    fx.pend();
    let path = fx.dirs.pending();
    let mut pending: store::PendingLogin =
        serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    pending.expires_at = epoch_now() as u64 - 1;
    store::save_json(&path, &pending, "pending login").unwrap();
    let validations = fx.sso.count(VALIDATE);
    assert!(message(&code("123456"), "CODE_EXPIRED").contains("expired"));
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
    assert!(register.contains("x-requested-with: xmlhttprequest"));
    assert!(register.contains("referer: ") && register.contains(DEVICE_VIEW));
    let origin = fx.sso.url.origin().ascii_serialization();
    assert!(register.contains(&format!("origin: {origin}")));
    // The handoff form was submitted to KLMS with its hidden ticket.
    let handoff = &fx.klms.requests("/login/ssologin.php")[0];
    assert!(handoff.starts_with("post ") && handoff.contains("ticket=opaque"));
}

#[test]
fn saved_password_notice_names_the_backend_and_stays_quiet_for_json() {
    let path = PathBuf::from("/cfg/klms/credentials.json");
    let file = Backend::File(path.clone());
    assert_eq!(
        password_saved_notice(false, &file).unwrap(),
        "Password saved in plaintext at /cfg/klms/credentials.json (0600)."
    );
    let keychain = Backend::Keychain(PathBuf::from("security"));
    assert_eq!(
        password_saved_notice(false, &keychain).unwrap(),
        "Password saved to the macOS Keychain."
    );
    let service = Backend::SecretService(PathBuf::from("secret-tool"));
    assert_eq!(
        password_saved_notice(false, &service).unwrap(),
        "Password saved to the Secret Service keyring."
    );
    assert_eq!(password_saved_notice(true, &file), None);
}

#[test]
fn a_failed_save_does_not_claim_success_or_keep_the_old_password() {
    /// Types a new password too long for any backend to remember.
    struct LongPassword;
    impl AuthPrompt for LongPassword {
        fn identifier(&mut self) -> Result<String, AppError> {
            Ok("student".into())
        }
        fn password(&mut self) -> Result<Zeroizing<String>, AppError> {
            Ok(Zeroizing::new("p".repeat(1025)))
        }
        fn otp(&mut self, _channel: &str) -> Result<Zeroizing<String>, AppError> {
            Ok(Zeroizing::new("123456".into()))
        }
    }
    let fx = Fixture::new();
    fx.seed_remembered_password();
    let previous = fx.identity();
    let env = Env {
        dirs: &fx.dirs,
        klms: &fx.klms.url,
        sso: &fx.sso.url,
        timeout: 5,
        secrets: &fx.secrets,
        interactive: true,
        defer_code: false,
        json: false,
    };
    let backend = Backend::File(fx.dirs.credentials());
    let login = Identity::new("student", LoginMethod::Password, Some(SecondFactor::Email));
    let long = Zeroizing::new("p".repeat(1025));
    let mut warnings = Vec::new();
    let (identity, stored_now) = remember_login(
        &env,
        previous.as_ref(),
        login,
        Some((&backend, &long)),
        &mut warnings,
    );
    assert!(!stored_now, "no \"Password saved\" notice");
    assert_eq!(identity.password_backend, None);
    assert_eq!(
        warnings,
        [
            "could not remember the password: a remembered password cannot be longer than 1024 bytes",
            "the previously remembered password was removed",
        ]
    );

    // The same through a full sign-in: login.json no longer points at a
    // stored password, and the stale one is gone.
    fx.seed_remembered_password();
    let result = fx.login_as(true, false, &remember(), LongPassword).unwrap();
    assert_eq!(result.data["password_backend"], Value::Null);
    assert_eq!(result.warnings, warnings);
    let identity = fx.identity().unwrap();
    assert_eq!(
        (identity.username.as_str(), identity.password_backend),
        ("student", None)
    );
    assert_eq!(fx.password("student"), None);
    assert!(!fx.dirs.credentials().exists());
}

#[test]
fn a_code_that_never_reached_kaist_keeps_the_pending_login() {
    let fx = Fixture::new();
    fx.seed_remembered_password();
    fx.pend();
    // Drop the SSO page the pending login came from so it can be resumed
    // against another SSO origin: one that refuses connections.
    let path = fx.dirs.pending();
    let mut pending: store::PendingLogin =
        serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    pending.document_url = None;
    store::save_json(&path, &pending, "pending login").unwrap();
    // A port that was just bound and released refuses connections.
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let refused = Url::parse(&format!("http://127.0.0.1:{port}/")).unwrap();
    let env = Env {
        dirs: &fx.dirs,
        klms: &fx.klms.url,
        sso: &refused,
        timeout: 5,
        secrets: &fx.secrets,
        interactive: false,
        defer_code: true,
        json: true,
    };
    let error = failure(login_with(
        &env,
        &code("654321"),
        Terminal(fx.asked.clone()),
    ));
    assert_eq!((error.code, error.exit_code()), ("NETWORK_ERROR", 20));
    assert!(path.is_file(), "the pending login survives");
    assert!(!fx.dirs.session().exists());

    // The retry against a working SSO finishes the same login.
    let result = fx.login(false, true, &code("654321")).unwrap();
    assert_eq!(result.data["user"], "student");
    assert!(fx.dirs.session().is_file() && !path.exists());
    assert_eq!(fx.sso.count(VALIDATE), 1);
}

/// Synthetic keyring with real secret-tool argument shapes and injectable failures.
fn fake_keyring(fx: &mut Fixture) -> (Backend, PathBuf) {
    let root = fx._root.path().join("keyring");
    fs::create_dir_all(root.join("items")).unwrap();
    let helper = root.join("secret-tool");
    write_executable(
        &helper,
        &format!(
            r#"#!/bin/sh
cd '{}' || exit 2
printf '%s\n' "$*" >> calls
case "$*" in
  'store --label=klms service klms account '*)
    [ ! -f fail-store ] || {{ echo 'store refused' >&2; exit 1; }}
    cat > "items/$6" ;;
  'lookup service klms account '*)
    [ -f "items/$5" ] || exit 1
    cat "items/$5" ;;
  'clear service klms'|'clear service klms account '*)
    [ ! -f fail-clear ] || {{ echo 'delete refused' >&2; exit 1; }}
    [ ! -f locked ] || exit 1
    if [ "$#" = 3 ]; then
      removed=1
      for item in items/*; do
        [ -f "$item" ] || continue
        rm "$item" || exit 2
        removed=0
      done
      exit "$removed"
    fi
    [ -f "items/$5" ] || exit 1
    rm "items/$5" ;;
  'search --all service klms'|'search --all service klms account '*)
    for item in items/*; do [ ! -f "$item" ] || echo '[remaining-item]'; done
    exit 0 ;;
  *) echo 'unexpected arguments' >&2; exit 2 ;;
esac
"#,
            root.display()
        ),
    );
    fx.secrets.search_path = root.clone().into_os_string();
    (Backend::SecretService(helper), root)
}

fn remember_direct(
    fx: &Fixture,
    previous: Option<&Identity>,
    user: &str,
    password: Option<(&Backend, &Zeroizing<String>)>,
) -> (Identity, bool, Vec<String>) {
    let env = Env {
        dirs: &fx.dirs,
        klms: &fx.klms.url,
        sso: &fx.sso.url,
        timeout: 5,
        secrets: &fx.secrets,
        interactive: true,
        defer_code: false,
        json: true,
    };
    let mut warnings = Vec::new();
    let (identity, stored) = remember_login(
        &env,
        previous,
        Identity::new(user, LoginMethod::Password, None),
        password,
        &mut warnings,
    );
    (identity, stored, warnings)
}

#[test]
fn forget_corrupt_identity_purges_every_account_in_both_keyrings() {
    let mut fx = Fixture::new();
    let (backend, root) = fake_keyring(&mut fx);
    backend.store("one", "pw1").unwrap();
    backend.store("two", "pw2").unwrap();
    fs::create_dir_all(&fx.dirs.config).unwrap();
    fs::write(fx.dirs.identity(), "{broken").unwrap();
    fs::write(fx.dirs.credentials(), "{broken plaintext").unwrap();
    fs::write(root.join("mac-one"), "pw").unwrap();
    fs::write(root.join("mac-two"), "pw").unwrap();
    fx.secrets.security = root.join("security");
    write_executable(
        &fx.secrets.security,
        &format!(
            r#"#!/bin/sh
[ "$*" = 'delete-generic-password -s klms' ] || exit 2
for item in '{}'/mac-*; do
  [ -f "$item" ] || continue
  rm "$item"
  exit $?
done
exit 44
"#,
            root.display()
        ),
    );
    let result = forget_with(&fx.dirs, &fx.secrets).unwrap();
    assert_eq!(result.data["password_removed"], true);
    assert_eq!(result.data["password_cleanup_complete"], true);
    assert_eq!(
        result.data["password_backends"],
        json!({"keychain": true, "secret-service": true, "plaintext-file": true})
    );
    assert_eq!(result.warnings.len(), 1);
    assert!(result.warnings[0].contains("identity was unreadable"));
    assert!(!root.join("mac-one").exists() && !root.join("mac-two").exists());
    assert!(backend.lookup("one").unwrap().is_none());
    assert!(backend.lookup("two").unwrap().is_none());
    let again = forget_with(&fx.dirs, &fx.secrets).unwrap();
    assert_eq!(again.data["password_removed"], false);
    assert_eq!(again.data["password_cleanup_complete"], true);
    assert!(again.warnings.is_empty());
}

#[test]
fn forget_reports_failed_or_locked_keyring_deletion_and_retries_without_identity() {
    for fault in ["fail-clear", "locked"] {
        let mut fx = Fixture::new();
        let (backend, root) = fake_keyring(&mut fx);
        backend.store("orphan", "pw").unwrap();
        fs::write(root.join(fault), "").unwrap();
        fx.seed_remembered_password();
        let result = forget_with(&fx.dirs, &fx.secrets).unwrap();
        assert_eq!(result.data["password_removed"], false, "{fault}");
        assert_eq!(result.data["password_cleanup_complete"], false);
        assert!(result.data["password_backends"]["secret-service"].is_null());
        assert_eq!(result.data["password_backends"]["plaintext-file"], true);
        assert!(!result.warnings.is_empty());
        assert!(result.human.contains("incomplete"));
        assert!(!fx.dirs.identity().exists());
        fs::remove_file(root.join(fault)).unwrap();
        let retry = forget_with(&fx.dirs, &fx.secrets).unwrap();
        assert_eq!(retry.data["password_removed"], true);
        assert!(backend.lookup("orphan").unwrap().is_none());
    }
}

#[test]
fn forget_without_metadata_or_keyring_is_complete_and_quiet() {
    let fx = Fixture::new();
    let result = forget_with(&fx.dirs, &fx.secrets).unwrap();
    assert!(result.warnings.is_empty());
    assert_eq!(result.data["login_removed"], false);
    assert_eq!(result.data["password_removed"], false);
    assert_eq!(result.data["password_backend"], Value::Null);
    assert_eq!(result.data["password_cleanup_complete"], true);
    assert_eq!(
        result.data["password_backends"],
        json!({"plaintext-file": false})
    );
    assert!(!result.human.contains("incomplete"));
}

#[test]
fn forget_unavailable_backend_keeps_metadata_until_purge_succeeds() {
    let mut fx = Fixture::new();
    let (backend, root) = fake_keyring(&mut fx);
    backend.store("student", "pw").unwrap();
    let mut identity = Identity::new("student", LoginMethod::Password, None);
    identity.password_backend = Some("secret-service".into());
    store::save_json(&fx.dirs.identity(), &identity, "login").unwrap();
    let temporary = fx.dirs.identity().with_extension("json.123.tmp");
    fs::write(&temporary, "old identity").unwrap();
    fs::rename(root.join("secret-tool"), root.join("disabled")).unwrap();
    for _ in 0..2 {
        let result = forget_with(&fx.dirs, &fx.secrets).unwrap();
        assert_eq!(result.data["password_removed"], false);
        assert_eq!(result.data["login_removed"], false);
        assert_eq!(result.data["password_cleanup_complete"], false);
        assert_eq!(result.data["password_backend"], "secret-service");
        assert_eq!(
            result.data["password_backends"]["secret-service"],
            Value::Null
        );
        assert_eq!(result.warnings.len(), 1);
        assert!(result.warnings[0].contains("secret-tool` is not installed"));
        assert_eq!(fx.identity(), Some(identity.clone()));
        assert!(!temporary.exists());
    }
    fs::rename(root.join("disabled"), root.join("secret-tool")).unwrap();
    let result = forget_with(&fx.dirs, &fx.secrets).unwrap();
    assert_eq!(result.data["password_removed"], true);
    assert_eq!(result.data["login_removed"], true);
    assert_eq!(result.data["password_cleanup_complete"], true);
    assert_eq!(result.data["password_backends"]["secret-service"], true);
    assert!(result.warnings.is_empty());
    assert!(!fx.dirs.identity().exists());
    assert!(backend.lookup("student").unwrap().is_none());
}

#[test]
fn forget_failed_named_backend_keeps_metadata_until_purge_succeeds() {
    for fault in ["fail-clear", "locked"] {
        let mut fx = Fixture::new();
        let (backend, root) = fake_keyring(&mut fx);
        backend.store("student", "pw").unwrap();
        let mut identity = Identity::new("student", LoginMethod::Password, None);
        identity.password_backend = Some("secret-service".into());
        store::save_json(&fx.dirs.identity(), &identity, "login").unwrap();
        fs::write(root.join(fault), "").unwrap();
        let result = forget_with(&fx.dirs, &fx.secrets).unwrap();
        assert_eq!(result.data["login_removed"], false);
        assert_eq!(result.data["password_cleanup_complete"], false);
        assert!(!result.warnings.is_empty());
        assert_eq!(fx.identity(), Some(identity));
        fs::remove_file(root.join(fault)).unwrap();
        let result = forget_with(&fx.dirs, &fx.secrets).unwrap();
        assert_eq!(result.data["login_removed"], true);
        assert_eq!(result.data["password_removed"], true);
        assert!(result.warnings.is_empty());
        assert!(backend.lookup("student").unwrap().is_none());
    }
}

#[test]
fn identity_save_failure_rolls_back_password_and_reports_failed_rollback() {
    for fail_rollback in [false, true] {
        let mut fx = Fixture::new();
        let (backend, root) = fake_keyring(&mut fx);
        fs::create_dir_all(fx.dirs.identity()).unwrap();
        if fail_rollback {
            fs::write(root.join("fail-clear"), "").unwrap();
        }
        let password = Zeroizing::new("pw".to_owned());
        let (identity, stored, warnings) =
            remember_direct(&fx, None, "student", Some((&backend, &password)));
        assert!(!stored);
        assert!(identity.password_backend.is_none());
        assert!(warnings[0].contains("could not remember the login"));
        assert_eq!(warnings.len(), if fail_rollback { 2 } else { 1 });
        assert_eq!(backend.lookup("student").unwrap().is_some(), fail_rollback);
        if fail_rollback {
            assert!(warnings[1].contains("could not roll back"));
            fs::remove_file(root.join("fail-clear")).unwrap();
        }
        // An unreadable identity directory must not prevent keyring cleanup.
        let forgotten = forget_with(&fx.dirs, &fx.secrets).unwrap();
        assert_eq!(forgotten.data["login_removed"], false);
        assert!(
            forgotten
                .warnings
                .iter()
                .any(|w| w.contains("cannot remove remembered login"))
        );
        assert!(backend.lookup("student").unwrap().is_none());
    }
}

#[test]
fn failed_account_switch_migration_and_stale_password_cleanup_are_recoverable() {
    for (user, fail_store) in [("other", false), ("student", false), ("student", true)] {
        let mut fx = Fixture::new();
        let (old, root) = fake_keyring(&mut fx);
        old.store("student", "old").unwrap();
        let mut previous = Identity::new("student", LoginMethod::Password, None);
        previous.password_backend = Some("secret-service".into());
        store::save_json(&fx.dirs.identity(), &previous, "login").unwrap();
        fs::write(root.join("fail-clear"), "").unwrap();
        let new = Backend::File(fx.dirs.credentials());
        let password = Zeroizing::new(if fail_store {
            "p".repeat(1025)
        } else {
            "new".into()
        });
        let (identity, stored, warnings) =
            remember_direct(&fx, Some(&previous), user, Some((&new, &password)));
        assert_eq!(stored, !fail_store);
        assert!(!warnings.is_empty());
        assert_eq!(fx.identity().unwrap(), identity);
        assert!(old.lookup("student").unwrap().is_some());
        fs::remove_file(root.join("fail-clear")).unwrap();
        let result = forget_with(&fx.dirs, &fx.secrets).unwrap();
        assert_eq!(result.data["password_removed"], true);
        assert!(old.lookup("student").unwrap().is_none());
        assert!(!fx.dirs.credentials().exists());
    }
}

#[test]
fn forget_and_logout_clean_only_their_owned_crash_temporaries() {
    let fx = Fixture::new();
    fx.seed_remembered_password();
    fs::create_dir_all(&fx.dirs.state).unwrap();
    for path in [
        fx.dirs.identity(),
        fx.dirs.credentials(),
        fx.dirs.pending(),
        fx.dirs.session(),
    ] {
        fs::write(path.with_extension("json.123.tmp"), "secret").unwrap();
    }
    let unrelated = fx.dirs.config.join("credentials.json.backup.tmp");
    fs::write(&unrelated, "unrelated").unwrap();
    forget_with(&fx.dirs, &fx.secrets).unwrap();
    for path in [fx.dirs.identity(), fx.dirs.credentials(), fx.dirs.pending()] {
        assert!(!path.with_extension("json.123.tmp").exists());
    }
    assert!(unrelated.exists());
    assert!(fx.dirs.session().with_extension("json.123.tmp").exists());
    fx.seed_remembered_password();
    fs::write(
        fx.dirs.credentials().with_extension("json.123.tmp"),
        "secret",
    )
    .unwrap();
    let result = logout_with(&fx.dirs.state).unwrap();
    assert!(result.warnings.is_empty());
    assert!(!fx.dirs.session().with_extension("json.123.tmp").exists());
    assert!(
        fx.dirs.identity().exists()
            && fx
                .dirs
                .credentials()
                .with_extension("json.123.tmp")
                .exists()
    );
}

#[test]
fn logout_and_forget_report_file_and_temp_deletion_failures() {
    let fx = Fixture::new();
    let mut identity = Identity::new("student", LoginMethod::Password, None);
    identity.password_backend = Some(secret::PLAINTEXT_FILE.into());
    store::save_json(&fx.dirs.identity(), &identity, "login").unwrap();
    for path in [
        fx.dirs.session(),
        fx.dirs.pending(),
        fx.dirs.credentials(),
        fx.dirs.credentials().with_extension("json.123.tmp"),
    ] {
        fs::create_dir_all(path).unwrap();
    }
    let result = logout_with(&fx.dirs.state).unwrap();
    assert_eq!(result.warnings.len(), 2);
    assert_eq!(result.data["removed"], false);
    assert_eq!(result.data["cleanup_complete"], false);
    assert!(result.human.contains("incomplete"));
    let result = forget_with(&fx.dirs, &fx.secrets).unwrap();
    assert_eq!(result.data["login_removed"], false);
    assert_eq!(fx.identity(), Some(identity));
    assert_eq!(result.data["password_removed"], false);
    assert_eq!(result.data["pending_removed"], false);
    assert!(
        result
            .warnings
            .iter()
            .any(|w| w.contains("credentials.json.123.tmp"))
    );
}

#[test]
fn consumed_code_cleanup_failures_are_reported_on_success_and_error() {
    for correct in [false, true] {
        let fx = Fixture::new();
        fx.seed_remembered_password();
        fx.pend();
        fs::create_dir(fx.dirs.pending().with_extension("json.123.tmp")).unwrap();
        if !correct {
            *fx.otp.lock().unwrap() = "E001";
        }
        let result = fx.login(false, true, &code("123456"));
        if correct {
            assert!(
                result
                    .unwrap()
                    .warnings
                    .iter()
                    .any(|w| w.contains("pending-login.json.123.tmp"))
            );
        } else {
            let error = failure(result);
            assert_eq!(error.code, "CODE_INCORRECT");
            assert!(
                error.details.unwrap()["warnings"][0]
                    .as_str()
                    .unwrap()
                    .contains("pending-login.json.123.tmp")
            );
            assert!(error.message.contains("cleanup warnings"));
        }
        assert!(!fx.dirs.pending().exists());
    }
}

#[test]
fn forget_reports_partial_keychain_purge_as_incomplete() {
    let mut fx = Fixture::new();
    let (_, root) = fake_keyring(&mut fx);
    fs::write(root.join("first"), "pw").unwrap();
    fx.secrets.security = root.join("security");
    write_executable(
        &fx.secrets.security,
        &format!(
            r#"#!/bin/sh
[ "$*" = 'delete-generic-password -s klms' ] || exit 2
if [ -f '{0}/first' ]; then rm '{0}/first'; exit 0; fi
echo 'keychain denied deletion' >&2
exit 1
"#,
            root.display()
        ),
    );
    let result = forget_with(&fx.dirs, &fx.secrets).unwrap();
    assert!(!root.join("first").exists());
    assert_eq!(result.data["password_removed"], false);
    assert_eq!(result.data["password_cleanup_complete"], false);
    assert!(result.data["password_backends"]["keychain"].is_null());
    assert!(
        result
            .warnings
            .iter()
            .any(|w| w.contains("keychain denied deletion"))
    );
}

#[test]
fn deferred_login_reports_failed_old_password_deletion_in_human_and_json_errors() {
    let mut fx = Fixture::new();
    let (backend, root) = fake_keyring(&mut fx);
    backend.store("student", "pw").unwrap();
    let mut previous = Identity::new("student", LoginMethod::Password, None);
    previous.password_backend = Some("secret-service".into());
    store::save_json(&fx.dirs.identity(), &previous, "login").unwrap();
    fs::write(root.join("fail-clear"), "").unwrap();
    let error = failure(fx.login(
        true,
        true,
        &LoginOptions {
            user: Some("other".into()),
            ..remember()
        },
    ));
    assert_eq!(error.code, "CODE_REQUIRED");
    assert!(error.message.contains("delete refused"));
    let details = error.details.unwrap();
    assert!(
        details["warnings"][0]
            .as_str()
            .unwrap()
            .contains("delete refused")
    );
    assert_eq!(details["resume"], "klms auth login --code CODE");
}
