//! CLI-level contract for remembered logins, forgetting, and the
//! non-interactive two-step password login. Passwords live in a plaintext
//! credentials file inside a temp dir; no OS keyring is ever consulted.

use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Output, Stdio},
};

use serde_json::Value;
use tempfile::TempDir;

mod fixture;
use fixture::server::{Response, Server};

struct Home {
    root: TempDir,
}

impl Home {
    fn new() -> Self {
        let root = TempDir::new().unwrap();
        fs::create_dir_all(root.path().join("bin")).unwrap();
        Self { root }
    }

    fn config(&self) -> PathBuf {
        self.root.path().join("config/klms")
    }

    fn state(&self) -> PathBuf {
        self.root.path().join("state/klms")
    }

    /// Runs klms with no terminal on stdin and no helper programs on PATH.
    fn run(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_klms"))
            .env("HOME", self.root.path())
            .env("XDG_CONFIG_HOME", self.root.path().join("config"))
            .env("XDG_STATE_HOME", self.root.path().join("state"))
            .env("PATH", self.root.path().join("bin"))
            .stdin(Stdio::null())
            .args(args)
            .output()
            .unwrap()
    }

    fn seed_login(&self, backend: Option<&str>) {
        fs::create_dir_all(self.config()).unwrap();
        let backend = backend
            .map(|name| format!(r#","password_backend":"{name}""#))
            .unwrap_or_default();
        fs::write(
            self.config().join("login.json"),
            format!(
                r#"{{"version":1,"username":"student","method":"password","second_factor":"email"{backend}}}"#
            ),
        )
        .unwrap();
    }

    fn seed_password(&self) {
        self.seed_login(Some("plaintext-file"));
        fs::write(
            self.config().join("credentials.json"),
            r#"{"version":1,"passwords":{"student":"s3cret"}}"#,
        )
        .unwrap();
    }
}

fn json(bytes: &[u8]) -> Value {
    serde_json::from_slice(bytes).unwrap()
}

#[test]
fn login_help_documents_every_new_flag_specifically() {
    let home = Home::new();
    let help = String::from_utf8(home.run(&["auth", "login", "--help"]).stdout).unwrap();
    for needle in [
        "--user <ID>",
        "--remember-password",
        "--insecure-storage",
        "--code <CODE>",
        "CODE_REQUIRED",
        "plaintext",
        "Signing in as",
        "12 CODE_REQUIRED",
        "login.json",
    ] {
        assert!(help.contains(needle), "missing {needle:?} in:\n{help}");
    }
    assert!(!help.contains("--password"));
    assert!(!help.contains("--otp"));
    let forget = String::from_utf8(home.run(&["auth", "forget", "--help"]).stdout).unwrap();
    assert!(forget.contains("plaintext credentials file"));
    assert!(forget.contains("keychain"));
}

#[test]
fn flag_combinations_are_rejected_by_the_parser() {
    let home = Home::new();
    for args in [
        &["--json", "auth", "login", "--insecure-storage"][..],
        &[
            "--json", "auth", "login", "--code", "123456", "--method", "password",
        ],
        &["--json", "auth", "login", "--code", "123456", "--user", "x"],
        &[
            "--json",
            "auth",
            "login",
            "--code",
            "123456",
            "--remember-password",
        ],
    ] {
        let output = home.run(args);
        assert_eq!(output.status.code(), Some(2), "{args:?}");
        assert_eq!(json(&output.stderr)["error"]["code"], "USAGE");
    }
}

#[test]
fn status_reports_remembered_login_and_backend_without_secrets() {
    let home = Home::new();
    let none = home.run(&["--json", "auth", "status"]);
    assert!(json(&none.stdout)["data"]["remembered"].is_null());

    home.seed_password();
    let output = home.run(&["--json", "auth", "status"]);
    assert!(output.status.success());
    let text = String::from_utf8_lossy(&output.stdout);
    assert!(!text.contains("s3cret"));
    let data = &json(&output.stdout)["data"]["remembered"];
    assert_eq!(data["username"], "student");
    assert_eq!(data["method"], "password");
    assert_eq!(data["second_factor"], "email");
    assert_eq!(data["password_backend"], "plaintext-file");

    home.seed_login(None);
    let bare = json(&home.run(&["--json", "auth", "status"]).stdout);
    assert_eq!(bare["data"]["remembered"]["password_backend"], "none");
    let human = String::from_utf8(home.run(&["auth", "status"]).stdout).unwrap();
    assert!(human.contains("Remembered login: student (password, email code)"));
    assert!(human.contains("Stored password: none"));
}

#[test]
fn logout_keeps_the_remembered_login_and_forget_removes_it() {
    let home = Home::new();
    home.seed_password();
    fixture::seed_session(&home.root.path().join("state"), "cookie");
    fs::write(home.state().join("pending-login.json"), "{}").unwrap();

    let logout = home.run(&["--json", "auth", "logout"]);
    assert!(logout.status.success());
    assert!(!home.state().join("session.json").exists());
    assert!(!home.state().join("pending-login.json").exists());
    assert!(home.config().join("login.json").is_file());
    assert!(home.config().join("credentials.json").is_file());

    fixture::seed_session(&home.root.path().join("state"), "cookie");
    let forget = home.run(&["--json", "auth", "forget"]);
    assert!(forget.status.success(), "{:?}", forget.stderr);
    let data = &json(&forget.stdout)["data"];
    assert_eq!(forget_command(&forget.stdout), "auth.forget");
    assert_eq!(data["login_removed"], true);
    assert_eq!(data["password_removed"], true);
    assert_eq!(data["password_backend"], "plaintext-file");
    assert!(!home.config().join("login.json").exists());
    assert!(!home.config().join("credentials.json").exists());
    assert!(
        home.state().join("session.json").is_file(),
        "session untouched"
    );

    let again = home.run(&["--json", "auth", "forget"]);
    assert!(again.status.success());
    let data = &json(&again.stdout)["data"];
    assert_eq!(data["login_removed"], false);
    assert_eq!(data["password_removed"], false);
}

fn forget_command(stdout: &[u8]) -> String {
    json(stdout)["command"].as_str().unwrap().to_owned()
}

#[test]
fn non_interactive_login_fails_clearly_before_touching_the_network() {
    let home = Home::new();
    let base = [
        "--json",
        "--base-url",
        "http://127.0.0.1:9",
        "auth",
        "login",
    ];
    let run = |extra: &[&str]| home.run(&[&base[..], extra].concat());

    let easy = run(&["--user", "student"]);
    assert_eq!(easy.status.code(), Some(10));
    let error = &json(&easy.stderr)["error"];
    assert!(error["message"].as_str().unwrap().contains("approve"));
    assert!(
        error["hint"]
            .as_str()
            .unwrap()
            .contains("--method password")
    );

    let nopass = run(&["--user", "student", "--method", "password"]);
    assert_eq!(nopass.status.code(), Some(10));
    assert!(
        json(&nopass.stderr)["error"]["hint"]
            .as_str()
            .unwrap()
            .contains("--remember-password")
    );

    let nobody = run(&["--method", "password"]);
    assert_eq!(nobody.status.code(), Some(2));

    let remember = run(&[
        "--method",
        "password",
        "--remember-password",
        "--insecure-storage",
    ]);
    assert_eq!(remember.status.code(), Some(2));
    assert!(!home.config().join("credentials.json").exists());
}

fn sso_router(target: &str) -> Response {
    let path = target.split('?').next().unwrap();
    let json = |body: &str| Response::bytes("application/json", body.as_bytes().to_vec());
    match path {
        "/auth/kaist/user/login/view" => {
            Response::html("login").header("Set-Cookie", "sso-session=one; Path=/")
        }
        "/auth/user/login/init" => json(&format!(r#"{{"result_data":"{}"}}"#, "00".repeat(48))),
        "/auth/user/login/auth" => json(r#"{"result_code":"SS0098"}"#),
        "/auth/kaist/user/login/second/view" => Response::html("second"),
        "/auth/kaist/user/login/second/ajaxSendMail" => json(r#"{"errorCode":"SS0001"}"#),
        "/auth/kaist/user/login/second/ajaxValidCrtfcNo" => json(r#"{"result_code":"SS0001"}"#),
        "/auth/user/login/link" => Response::html("")
            .status("302 Found")
            .header("Location", "/"),
        "/" => Response::html("ok").header("Set-Cookie", "MoodleSession=owned; Path=/; HttpOnly"),
        other => panic!("unexpected request {other}"),
    }
}

fn mode(path: &Path) -> u32 {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::metadata(path).unwrap().permissions().mode() & 0o777
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        0o600
    }
}

#[test]
fn two_step_password_login_end_to_end() {
    let home = Home::new();
    home.seed_password();
    let server = Server::new(|request| sso_router(&request.target));
    let url = server.url();

    let first = home.run(&["--json", "--base-url", &url, "auth", "login"]);
    assert_eq!(
        first.status.code(),
        Some(12),
        "{}",
        String::from_utf8_lossy(&first.stderr)
    );
    assert!(first.stdout.is_empty());
    let stderr = String::from_utf8_lossy(&first.stderr).into_owned();
    assert!(!stderr.contains("s3cret"));
    let error = &json(&first.stderr)["error"];
    assert_eq!(error["code"], "CODE_REQUIRED");
    assert_eq!(error["retryable"], false);
    assert_eq!(error["details"]["channel"], "email");
    assert_eq!(error["details"]["resume"], "klms auth login --code CODE");
    assert!(error["details"]["expires_at"].as_u64().unwrap() > 1_700_000_000);
    let pending = home.state().join("pending-login.json");
    assert_eq!(mode(&pending), 0o600);
    assert!(!fs::read_to_string(&pending).unwrap().contains("s3cret"));
    assert!(!home.state().join("session.json").exists());

    let second = home.run(&[
        "--json",
        "--base-url",
        &url,
        "auth",
        "login",
        "--code",
        "123456",
    ]);
    assert!(
        second.status.success(),
        "{}",
        String::from_utf8_lossy(&second.stderr)
    );
    let value = json(&second.stdout);
    assert_eq!(value["command"], "auth.login");
    assert_eq!(value["data"]["user"], "student");
    assert_eq!(value["data"]["method"], "password");
    assert!(!String::from_utf8_lossy(&second.stdout).contains("owned"));
    assert!(!pending.exists());
    assert_eq!(mode(&home.state().join("session.json")), 0o600);

    let bodies: Vec<_> = server
        .recorded()
        .into_iter()
        .filter(|seen| seen.line.contains("ajaxValidCrtfcNo"))
        .map(|seen| seen.body)
        .collect();
    assert_eq!(bodies, ["crtfc_no=123456"]);
    // Resuming again has nothing to resume.
    let third = home.run(&[
        "--json",
        "--base-url",
        &url,
        "auth",
        "login",
        "--code",
        "123456",
    ]);
    assert_eq!(third.status.code(), Some(10));
    assert!(
        json(&third.stderr)["error"]["message"]
            .as_str()
            .unwrap()
            .contains("no login is waiting")
    );
}

#[test]
fn resume_rejects_malformed_codes_without_consuming_the_pending_login() {
    let home = Home::new();
    home.seed_password();
    let server = Server::new(|request| sso_router(&request.target));
    let url = server.url();
    assert_eq!(
        home.run(&["--json", "--base-url", &url, "auth", "login"])
            .status
            .code(),
        Some(12)
    );
    let bad = home.run(&[
        "--json",
        "--base-url",
        &url,
        "auth",
        "login",
        "--code",
        "12x",
    ]);
    assert_eq!(bad.status.code(), Some(2));
    assert!(home.state().join("pending-login.json").is_file());
}

#[test]
fn bare_login_announces_the_remembered_identity_on_stderr() {
    let home = Home::new();
    home.seed_password();
    let server = Server::new(|request| sso_router(&request.target));
    let output = home.run(&["--base-url", &server.url(), "auth", "login"]);
    assert_eq!(output.status.code(), Some(12));
    assert!(output.stdout.is_empty());
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(
        stderr.contains("Signing in as student (password)"),
        "{stderr}"
    );
    assert!(stderr.contains("error [CODE_REQUIRED]"));
    assert!(stderr.contains("klms auth login --code CODE"));
    assert!(!stderr.contains("s3cret"));
}
