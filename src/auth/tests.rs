//! End-to-end tests of the login orchestration against a loopback SSO/KLMS
//! fixture. Passwords go to a plaintext file in a temp dir, never a keychain.

use std::{
    cell::RefCell,
    fs,
    io::{Read, Write},
    net::TcpListener,
    rc::Rc,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread::{self, JoinHandle},
    time::Duration,
};

use tempfile::TempDir;

use super::{secret::PlaintextFile, *};

#[derive(Clone, Debug)]
struct Seen {
    path: String,
    body: String,
}

struct Sso {
    url: Url,
    seen: Arc<Mutex<Vec<Seen>>>,
    otp_result: Arc<Mutex<&'static str>>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Sso {
    fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let url = Url::parse(&format!("http://{}/", listener.local_addr().unwrap())).unwrap();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let otp_result = Arc::new(Mutex::new("SS0001"));
        let stop = Arc::new(AtomicBool::new(false));
        let (log, otp, halt) = (seen.clone(), otp_result.clone(), stop.clone());
        let thread = thread::spawn(move || {
            while !halt.load(Ordering::Relaxed) {
                let Ok((mut stream, _)) = listener.accept() else {
                    thread::sleep(Duration::from_millis(2));
                    continue;
                };
                stream.set_nonblocking(false).unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                let mut raw = Vec::new();
                let mut chunk = [0_u8; 4096];
                let (head, mut body) = loop {
                    let read = stream.read(&mut chunk).unwrap();
                    raw.extend_from_slice(&chunk[..read]);
                    if let Some(at) = raw.windows(4).position(|w| w == b"\r\n\r\n") {
                        let head = String::from_utf8_lossy(&raw[..at]).into_owned();
                        break (head, raw[at + 4..].to_vec());
                    }
                    assert!(read > 0, "incomplete request");
                };
                let length = head
                    .lines()
                    .find_map(|line| {
                        let (key, value) = line.split_once(':')?;
                        key.eq_ignore_ascii_case("content-length")
                            .then(|| value.trim().parse::<usize>().unwrap())
                    })
                    .unwrap_or(0);
                while body.len() < length {
                    let read = stream.read(&mut chunk).unwrap();
                    body.extend_from_slice(&chunk[..read]);
                }
                let mut line = head.lines().next().unwrap().split_whitespace();
                line.next();
                let path = line.next().unwrap();
                let path = path.split('?').next().unwrap().to_owned();
                log.lock().unwrap().push(Seen {
                    path: path.clone(),
                    body: String::from_utf8_lossy(&body).into_owned(),
                });
                let json = |text: String| ("200 OK", "application/json", None, None, text);
                let otp_code = *otp.lock().unwrap();
                let (status, kind, cookie, location, text) = match path.as_str() {
                    "/auth/kaist/user/login/view" => (
                        "200 OK",
                        "text/html",
                        Some("sso-session=one; Path=/"),
                        None,
                        "login".to_owned(),
                    ),
                    "/auth/user/login/init" => {
                        json(format!(r#"{{"result_data":"{}"}}"#, "00".repeat(48)))
                    }
                    "/auth/user/login/auth" => json(r#"{"result_code":"SS0098"}"#.into()),
                    "/auth/kaist/user/login/second/view" => {
                        ("200 OK", "text/html", None, None, "second".into())
                    }
                    "/auth/kaist/user/login/second/ajaxSendMail" => {
                        json(r#"{"errorCode":"SS0001"}"#.into())
                    }
                    "/auth/kaist/user/login/second/ajaxValidCrtfcNo" => {
                        json(format!(r#"{{"result_code":"{otp_code}"}}"#))
                    }
                    "/auth/user/login/link" => {
                        ("302 Found", "text/plain", None, Some("/"), String::new())
                    }
                    "/" => (
                        "200 OK",
                        "text/html",
                        Some("MoodleSession=owned; Path=/; HttpOnly"),
                        None,
                        "ok".into(),
                    ),
                    other => panic!("unexpected request {other}"),
                };
                let mut extra = String::new();
                if let Some(cookie) = cookie {
                    extra.push_str(&format!("Set-Cookie: {cookie}\r\n"));
                }
                if let Some(location) = location {
                    extra.push_str(&format!("Location: {location}\r\n"));
                }
                write!(
                    stream,
                    "HTTP/1.1 {status}\r\nContent-Type: {kind}\r\n{extra}Content-Length: {}\r\nConnection: close\r\n\r\n{text}",
                    text.len()
                )
                .unwrap();
            }
        });
        Self {
            url,
            seen,
            otp_result,
            stop,
            thread: Some(thread),
        }
    }

    fn requests(&self) -> Vec<Seen> {
        self.seen.lock().unwrap().clone()
    }

    fn count(&self, path: &str) -> usize {
        self.requests()
            .iter()
            .filter(|seen| seen.path == path)
            .count()
    }
}

impl Drop for Sso {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// `unwrap_err` for results whose success type has no `Debug`.
trait Failure {
    fn failure(self) -> AppError;
}

impl Failure for Result<CommandResult, AppError> {
    fn failure(self) -> AppError {
        match self {
            Err(error) => error,
            Ok(result) => panic!("expected a failure, got {}", result.command),
        }
    }
}

const VALIDATE: &str = "/auth/kaist/user/login/second/ajaxValidCrtfcNo";

/// Hands out file-backed storage for every backend kind.
struct FileProvider {
    credentials: std::path::PathBuf,
    refuse: bool,
}

impl Provider for FileProvider {
    fn choose(&self, allow_plaintext: bool) -> Result<Box<dyn SecretStore>, AppError> {
        if self.refuse && !allow_plaintext {
            return Err(AppError::config("no OS keyring"));
        }
        self.open(secret::PLAINTEXT_FILE)
    }
    fn open(&self, _kind: &str) -> Result<Box<dyn SecretStore>, AppError> {
        Ok(Box::new(PlaintextFile {
            path: self.credentials.clone(),
        }))
    }
}

#[derive(Default)]
struct Asked(Rc<RefCell<Vec<&'static str>>>);

struct Terminal {
    id: &'static str,
    password: &'static str,
    otp: &'static str,
    asked: Rc<RefCell<Vec<&'static str>>>,
}

impl AuthPrompt for Terminal {
    fn identifier(&mut self) -> Result<String, AppError> {
        self.asked.borrow_mut().push("id");
        Ok(self.id.into())
    }
    fn password(&mut self) -> Result<Zeroizing<String>, AppError> {
        self.asked.borrow_mut().push("password");
        Ok(Zeroizing::new(self.password.into()))
    }
    fn otp(&mut self, _channel: &str) -> Result<Zeroizing<String>, AppError> {
        self.asked.borrow_mut().push("otp");
        Ok(Zeroizing::new(self.otp.into()))
    }
    fn notice(&mut self, _message: &str) {}
}

struct Fixture {
    sso: Sso,
    dirs: Dirs,
    provider: FileProvider,
    _root: TempDir,
}

impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let dirs = Dirs {
            config: root.path().join("config/klms"),
            state: root.path().join("state/klms"),
        };
        let provider = FileProvider {
            credentials: dirs.credentials(),
            refuse: false,
        };
        Self {
            sso: Sso::start(),
            dirs,
            provider,
            _root: root,
        }
    }

    fn env(&self, interactive: bool, defer_code: bool) -> Env<'_> {
        Env {
            dirs: &self.dirs,
            klms: &self.sso.url,
            sso: &self.sso.url,
            timeout: 5,
            provider: &self.provider,
            interactive,
            defer_code,
            json: defer_code,
        }
    }

    fn terminal(&self, asked: &Asked) -> Terminal {
        Terminal {
            id: "student",
            password: "s3cret",
            otp: "123456",
            asked: asked.0.clone(),
        }
    }

    fn identity(&self) -> Option<Identity> {
        identity::load(&self.dirs.identity()).unwrap()
    }

    fn password(&self, user: &str) -> Option<String> {
        PlaintextFile {
            path: self.dirs.credentials(),
        }
        .lookup(user)
        .unwrap()
        .map(|password| password.to_string())
    }

    fn remember_options() -> LoginOptions {
        LoginOptions {
            method: Some(LoginMethod::Password),
            factor: Some(SecondFactor::Email),
            remember_password: true,
            insecure_storage: true,
            ..LoginOptions::default()
        }
    }
}

#[test]
fn remember_password_stores_identity_and_secret_after_success() {
    let fx = Fixture::new();
    let asked = Asked::default();
    let result = login_with(
        &fx.env(true, false),
        &Fixture::remember_options(),
        fx.terminal(&asked),
    )
    .unwrap();
    assert_eq!(result.data["user"], "student");
    assert_eq!(result.data["password_backend"], "plaintext-file");
    assert_eq!(*asked.0.borrow(), ["id", "password", "otp"]);
    let identity = fx.identity().unwrap();
    assert_eq!(identity.username, "student");
    assert_eq!(identity.method, "password");
    assert_eq!(identity.second_factor.as_deref(), Some("email"));
    assert_eq!(identity.password_backend.as_deref(), Some("plaintext-file"));
    assert_eq!(fx.password("student").as_deref(), Some("s3cret"));
    assert!(fx.dirs.session().is_file());
    // Neither file the login wrote may expose the password to the session store.
    assert!(
        !fs::read_to_string(fx.dirs.session())
            .unwrap()
            .contains("s3cret")
    );
    assert!(
        !fs::read_to_string(fx.dirs.identity())
            .unwrap()
            .contains("s3cret")
    );
}

#[test]
fn failed_login_remembers_nothing() {
    let fx = Fixture::new();
    *fx.sso.otp_result.lock().unwrap() = "E001";
    let asked = Asked::default();
    let error = login_with(
        &fx.env(true, false),
        &Fixture::remember_options(),
        fx.terminal(&asked),
    )
    .failure();
    assert_eq!(error.code, "AUTH_REQUIRED");
    assert!(fx.identity().is_none());
    assert!(fx.password("student").is_none());
    assert!(!fx.dirs.session().exists());
}

#[test]
fn next_login_reuses_remembered_user_method_factor_and_password() {
    let fx = Fixture::new();
    let first = Asked::default();
    login_with(
        &fx.env(true, false),
        &Fixture::remember_options(),
        fx.terminal(&first),
    )
    .unwrap();
    // Removing the session (what `auth logout` does) keeps the identity.
    fs::remove_file(fx.dirs.session()).unwrap();
    let asked = Asked::default();
    let result = login_with(
        &fx.env(true, false),
        &LoginOptions::default(),
        fx.terminal(&asked),
    )
    .unwrap();
    assert_eq!(
        *asked.0.borrow(),
        ["otp"],
        "id and password come from storage"
    );
    assert_eq!(result.data["method"], "password");
    assert_eq!(result.data["second_factor"], "email");
    assert_eq!(result.data["user"], "student");
    assert_eq!(
        fx.sso.count("/auth/kaist/user/login/second/ajaxSendMail"),
        2
    );
}

#[test]
fn explicit_flags_override_and_update_remembered_values() {
    let fx = Fixture::new();
    let first = Asked::default();
    login_with(
        &fx.env(true, false),
        &Fixture::remember_options(),
        fx.terminal(&first),
    )
    .unwrap();
    assert!(fx.password("student").is_some());

    // Switch account: the old account's stored password must be dropped.
    let asked = Asked::default();
    let options = LoginOptions {
        user: Some("other".into()),
        ..LoginOptions::default()
    };
    login_with(&fx.env(true, false), &options, fx.terminal(&asked)).unwrap();
    assert_eq!(
        *asked.0.borrow(),
        ["password", "otp"],
        "password was not reused for another user"
    );
    let identity = fx.identity().unwrap();
    assert_eq!(identity.username, "other");
    assert_eq!(identity.method, "password", "method stays remembered");
    assert_eq!(identity.password_backend, None);
    assert!(fx.password("student").is_none());
}

#[test]
fn explicit_method_and_factor_replace_remembered_ones() {
    let fx = Fixture::new();
    let first = Asked::default();
    login_with(
        &fx.env(true, false),
        &Fixture::remember_options(),
        fx.terminal(&first),
    )
    .unwrap();
    // A second-factor flag with an easy method is a usage error.
    let bad = LoginOptions {
        method: Some(LoginMethod::Easy),
        factor: Some(SecondFactor::Sms),
        ..LoginOptions::default()
    };
    let asked = Asked::default();
    assert_eq!(
        login_with(&fx.env(true, false), &bad, fx.terminal(&asked))
            .failure()
            .code,
        "USAGE"
    );
    assert_eq!(
        fx.identity().unwrap().second_factor.as_deref(),
        Some("email")
    );
}

#[test]
fn remember_password_refuses_without_a_keyring_before_any_request() {
    let mut fx = Fixture::new();
    fx.provider.refuse = true;
    let asked = Asked::default();
    let options = LoginOptions {
        insecure_storage: false,
        ..Fixture::remember_options()
    };
    let error = login_with(&fx.env(true, false), &options, fx.terminal(&asked)).failure();
    assert!(error.message.contains("no OS keyring"));
    assert!(fx.sso.requests().is_empty());
    assert!(asked.0.borrow().is_empty());
}

#[test]
fn remember_password_needs_a_terminal_and_a_password_method() {
    let fx = Fixture::new();
    let asked = Asked::default();
    let error = login_with(
        &fx.env(false, true),
        &Fixture::remember_options(),
        fx.terminal(&asked),
    )
    .failure();
    assert_eq!(error.code, "USAGE");
    let easy = LoginOptions {
        method: Some(LoginMethod::Easy),
        remember_password: true,
        ..LoginOptions::default()
    };
    let error = login_with(&fx.env(true, false), &easy, fx.terminal(&asked)).failure();
    assert_eq!(error.code, "USAGE");
    assert!(fx.sso.requests().is_empty());
}

fn seed_remembered_password(fx: &Fixture) {
    let mut identity = Identity::new("student", LoginMethod::Password, Some(SecondFactor::Email));
    identity.password_backend = Some(secret::PLAINTEXT_FILE.into());
    identity::save(&fx.dirs.identity(), &identity).unwrap();
    PlaintextFile {
        path: fx.dirs.credentials(),
    }
    .store("student", "s3cret")
    .unwrap();
}

#[test]
fn non_interactive_login_is_two_steps_through_a_private_pending_file() {
    let fx = Fixture::new();
    seed_remembered_password(&fx);
    let asked = Asked::default();
    let error = login_with(
        &fx.env(false, true),
        &LoginOptions::default(),
        fx.terminal(&asked),
    )
    .failure();
    assert_eq!(error.code, "CODE_REQUIRED");
    assert_eq!(error.exit_code(), 12);
    let details = error.details.clone().unwrap();
    assert_eq!(details["channel"], "email");
    assert_eq!(details["resume"], "klms auth login --code CODE");
    let expires = details["expires_at"].as_u64().unwrap();
    let now = epoch_now() as u64;
    assert!(
        (now + 295..=now + 301).contains(&expires),
        "{expires} vs {now}"
    );
    assert!(
        asked.0.borrow().is_empty(),
        "never prompts without a terminal"
    );
    assert!(!fx.dirs.session().exists());
    assert_eq!(fx.sso.count(VALIDATE), 0);

    let pending_text = fs::read_to_string(fx.dirs.pending()).unwrap();
    assert!(
        !pending_text.contains("s3cret"),
        "the password is never persisted here"
    );
    assert!(pending_text.contains("sso-session"));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = fs::metadata(fx.dirs.pending())
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600);
    }

    let result = login_with(
        &fx.env(false, true),
        &LoginOptions {
            code: Some("654321".into()),
            ..LoginOptions::default()
        },
        fx.terminal(&asked),
    )
    .unwrap();
    assert_eq!(result.data["user"], "student");
    assert_eq!(result.data["method"], "password");
    assert!(fx.dirs.session().is_file());
    assert!(!fx.dirs.pending().exists());
    let validates: Vec<_> = fx
        .sso
        .requests()
        .into_iter()
        .filter(|seen| seen.path == VALIDATE)
        .collect();
    assert_eq!(validates.len(), 1);
    assert!(validates[0].body.contains("crtfc_no=654321"));
    // Resuming did not resend the code or log in again.
    assert_eq!(
        fx.sso.count("/auth/kaist/user/login/second/ajaxSendMail"),
        1
    );
    assert_eq!(fx.sso.count("/auth/user/login/auth"), 1);
    assert_eq!(
        fx.identity().unwrap().password_backend.as_deref(),
        Some("plaintext-file")
    );
}

#[test]
fn resume_sends_the_original_sso_cookie_back() {
    let fx = Fixture::new();
    seed_remembered_password(&fx);
    let asked = Asked::default();
    login_with(
        &fx.env(false, true),
        &LoginOptions::default(),
        fx.terminal(&asked),
    )
    .failure();
    let jar = fs::read_to_string(fx.dirs.pending()).unwrap();
    assert!(jar.contains("\"sso-session\""));
    login_with(
        &fx.env(false, true),
        &LoginOptions {
            code: Some("111111".into()),
            ..LoginOptions::default()
        },
        fx.terminal(&asked),
    )
    .unwrap();
    // The resumed process rebuilt the jar; the link step reached KLMS with it.
    assert_eq!(fx.sso.count("/auth/user/login/link"), 1);
}

#[test]
fn wrong_code_deletes_the_pending_file() {
    let fx = Fixture::new();
    seed_remembered_password(&fx);
    let asked = Asked::default();
    login_with(
        &fx.env(false, true),
        &LoginOptions::default(),
        fx.terminal(&asked),
    )
    .failure();
    *fx.sso.otp_result.lock().unwrap() = "E001";
    let code = || LoginOptions {
        code: Some("000000".into()),
        ..LoginOptions::default()
    };
    let error = login_with(&fx.env(false, true), &code(), fx.terminal(&asked)).failure();
    assert!(error.message.contains("incorrect"));
    assert!(!fx.dirs.pending().exists());
    let again = login_with(&fx.env(false, true), &code(), fx.terminal(&asked)).failure();
    assert!(again.message.contains("no login is waiting"));
}

#[test]
fn bad_code_format_keeps_the_pending_file() {
    let fx = Fixture::new();
    seed_remembered_password(&fx);
    let asked = Asked::default();
    login_with(
        &fx.env(false, true),
        &LoginOptions::default(),
        fx.terminal(&asked),
    )
    .failure();
    let error = login_with(
        &fx.env(false, true),
        &LoginOptions {
            code: Some("12ab".into()),
            ..LoginOptions::default()
        },
        fx.terminal(&asked),
    )
    .failure();
    assert_eq!(error.code, "USAGE");
    assert!(fx.dirs.pending().is_file());
}

#[test]
fn expired_or_missing_pending_state_is_an_error() {
    let fx = Fixture::new();
    seed_remembered_password(&fx);
    let asked = Asked::default();
    let resume = || LoginOptions {
        code: Some("123456".into()),
        ..LoginOptions::default()
    };
    let missing = login_with(&fx.env(false, true), &resume(), fx.terminal(&asked)).failure();
    assert!(missing.message.contains("no login is waiting"));

    login_with(
        &fx.env(false, true),
        &LoginOptions::default(),
        fx.terminal(&asked),
    )
    .failure();
    let mut pending: pending::PendingLogin =
        serde_json::from_slice(&fs::read(fx.dirs.pending()).unwrap()).unwrap();
    pending.expires_at = epoch_now() as u64 - 1;
    pending::save(&fx.dirs.pending(), &pending).unwrap();
    let expired = login_with(&fx.env(false, true), &resume(), fx.terminal(&asked)).failure();
    assert!(expired.message.contains("expired"));
    assert!(!fx.dirs.pending().exists());
    assert_eq!(
        fx.sso.count(VALIDATE),
        0,
        "an expired login never reaches KAIST"
    );
}

#[test]
fn non_interactive_failures_say_what_to_do() {
    let fx = Fixture::new();
    let asked = Asked::default();
    // Easy Login cannot be approved without a terminal.
    let easy = login_with(
        &fx.env(false, true),
        &LoginOptions {
            user: Some("student".into()),
            ..LoginOptions::default()
        },
        fx.terminal(&asked),
    )
    .failure();
    assert!(easy.message.contains("approve"));
    assert!(easy.hint.unwrap().contains("--method password"));

    // Password login with nothing stored.
    let password = LoginOptions {
        user: Some("student".into()),
        method: Some(LoginMethod::Password),
        ..LoginOptions::default()
    };
    let missing = login_with(&fx.env(false, true), &password, fx.terminal(&asked)).failure();
    assert!(missing.message.contains("no stored password"));
    assert!(missing.hint.unwrap().contains("--remember-password"));

    // No user at all.
    let nobody = LoginOptions {
        method: Some(LoginMethod::Password),
        ..LoginOptions::default()
    };
    let error = login_with(&fx.env(false, true), &nobody, fx.terminal(&asked)).failure();
    assert!(error.message.contains("--user"));
    assert!(fx.sso.requests().is_empty());
    assert!(asked.0.borrow().is_empty());
}

#[test]
fn interactive_json_login_also_defers_the_code() {
    let fx = Fixture::new();
    seed_remembered_password(&fx);
    let asked = Asked::default();
    let error = login_with(
        &fx.env(true, true),
        &LoginOptions::default(),
        fx.terminal(&asked),
    )
    .failure();
    assert_eq!(error.code, "CODE_REQUIRED");
    assert!(
        asked.0.borrow().is_empty(),
        "stored password, no code prompt"
    );
}

#[test]
fn deferring_with_remember_password_remembers_once_kaist_accepts_it() {
    let fx = Fixture::new();
    let asked = Asked::default();
    // A terminal user who asked for JSON output gets the two-step result but
    // the password is remembered because KAIST already accepted it.
    let error = login_with(
        &fx.env(true, true),
        &Fixture::remember_options(),
        fx.terminal(&asked),
    )
    .failure();
    assert_eq!(error.code, "CODE_REQUIRED");
    assert_eq!(fx.password("student").as_deref(), Some("s3cret"));
    assert_eq!(
        fx.identity().unwrap().password_backend.as_deref(),
        Some("plaintext-file")
    );
}

#[test]
fn forget_removes_everything_remembered_and_is_idempotent() {
    let fx = Fixture::new();
    let asked = Asked::default();
    login_with(
        &fx.env(true, false),
        &Fixture::remember_options(),
        fx.terminal(&asked),
    )
    .unwrap();
    let first = forget_with(&fx.dirs, &fx.provider).unwrap();
    assert_eq!(first.data["login_removed"], true);
    assert_eq!(first.data["password_removed"], true);
    assert_eq!(first.data["password_backend"], "plaintext-file");
    assert!(fx.identity().is_none());
    assert!(!fx.dirs.credentials().exists());
    assert!(
        fx.dirs.session().is_file(),
        "forget leaves the session alone"
    );
    let second = forget_with(&fx.dirs, &fx.provider).unwrap();
    assert_eq!(second.data["login_removed"], false);
    assert_eq!(second.data["password_removed"], false);
}

#[test]
fn forget_discards_a_corrupt_login_file() {
    let fx = Fixture::new();
    fs::create_dir_all(&fx.dirs.config).unwrap();
    fs::write(fx.dirs.identity(), "{broken").unwrap();
    let result = forget_with(&fx.dirs, &fx.provider).unwrap();
    assert_eq!(result.data["login_removed"], true);
}

#[test]
fn status_projection_reports_backend_without_secrets() {
    let fx = Fixture::new();
    seed_remembered_password(&fx);
    let identity = fx.identity().unwrap();
    let view = remembered(&identity);
    let text = serde_json::to_string(&view).unwrap();
    assert!(text.contains("\"password_backend\":\"plaintext-file\""));
    assert!(!text.contains("s3cret"));
    let mut bare = identity;
    bare.password_backend = None;
    assert_eq!(remembered(&bare).password_backend, "none");
}
