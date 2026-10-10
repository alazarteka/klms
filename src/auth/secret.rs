//! Optional password storage behind one small trait.
//!
//! Backends are chosen automatically: the macOS login keychain through
//! `/usr/bin/security`, a Linux Secret Service through `secret-tool`, or, only
//! when the user explicitly opts in, a plaintext 0600 file. Secrets reach the
//! helper programs on standard input only; they are never placed in argv or in
//! the environment. Tests use [`PlaintextFile`] (or fake helper programs); they
//! never reach a real keychain.

use std::{
    collections::BTreeMap,
    ffi::OsString,
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

use crate::error::AppError;

use super::fsutil;

pub const KEYCHAIN: &str = "keychain";
pub const SECRET_SERVICE: &str = "secret-service";
pub const PLAINTEXT_FILE: &str = "plaintext-file";

const SERVICE: &str = "klms";
const MAC_SECURITY: &str = "/usr/bin/security";
const HELPER_TIMEOUT: Duration = Duration::from_secs(30);
const PROBE_ACCOUNT: &str = "klms-availability-probe";

pub trait SecretStore {
    /// One of [`KEYCHAIN`], [`SECRET_SERVICE`], [`PLAINTEXT_FILE`].
    fn kind(&self) -> &'static str;
    fn store(&self, account: &str, secret: &str) -> Result<(), AppError>;
    fn lookup(&self, account: &str) -> Result<Option<Zeroizing<String>>, AppError>;
    /// Delete the secret; deleting an absent one succeeds.
    fn clear(&self, account: &str) -> Result<(), AppError>;
}

/// Chooses and reopens backends. Production uses [`SystemProvider`]; tests
/// substitute file-backed providers.
pub trait Provider {
    /// Pick a backend for a new password. `allow_plaintext` is the user's
    /// explicit `--insecure-storage` consent.
    fn choose(&self, allow_plaintext: bool) -> Result<Box<dyn SecretStore>, AppError>;
    /// Reopen the backend a `login.json` recorded.
    fn open(&self, kind: &str) -> Result<Box<dyn SecretStore>, AppError>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Platform {
    MacOs,
    Other,
}

impl Platform {
    pub fn current() -> Self {
        if cfg!(target_os = "macos") {
            Self::MacOs
        } else {
            Self::Other
        }
    }
}

pub struct SystemProvider {
    pub platform: Platform,
    pub security_program: PathBuf,
    /// Value of `PATH` used to find `secret-tool`.
    pub search_path: OsString,
    pub credentials_file: PathBuf,
}

impl SystemProvider {
    pub fn new(credentials_file: PathBuf) -> Self {
        Self {
            platform: Platform::current(),
            security_program: PathBuf::from(MAC_SECURITY),
            search_path: std::env::var_os("PATH").unwrap_or_default(),
            credentials_file,
        }
    }

    fn secret_tool(&self) -> Option<PathBuf> {
        std::env::split_paths(&self.search_path)
            .map(|dir| dir.join("secret-tool"))
            .find(|candidate| is_executable(candidate))
    }
}

impl Provider for SystemProvider {
    fn choose(&self, allow_plaintext: bool) -> Result<Box<dyn SecretStore>, AppError> {
        if self.platform == Platform::MacOs {
            return Ok(Box::new(Keychain {
                program: self.security_program.clone(),
            }));
        }
        let mut reason = "`secret-tool` (libsecret) is not installed";
        if let Some(program) = self.secret_tool() {
            let service = SecretService { program };
            match service.lookup(PROBE_ACCOUNT) {
                Ok(_) => return Ok(Box::new(service)),
                Err(_) => {
                    reason = "`secret-tool` cannot reach a Secret Service (no D-Bus session or unlocked keyring)"
                }
            }
        }
        if allow_plaintext {
            return Ok(Box::new(PlaintextFile {
                path: self.credentials_file.clone(),
            }));
        }
        Err(AppError::config(format!(
            "cannot remember the password: there is no OS keyring here ({reason})"
        ))
        .with_hint(format!(
            "Rerun with `--insecure-storage` to store the password in a plaintext file readable only by you ({}), or omit `--remember-password` and enter the password each time.",
            self.credentials_file.display()
        )))
    }

    fn open(&self, kind: &str) -> Result<Box<dyn SecretStore>, AppError> {
        match kind {
            KEYCHAIN => Ok(Box::new(Keychain {
                program: self.security_program.clone(),
            })),
            SECRET_SERVICE => {
                let program = self.secret_tool().ok_or_else(|| {
                    AppError::config(
                        "the stored password lives in the Secret Service but `secret-tool` is not installed",
                    )
                })?;
                Ok(Box::new(SecretService { program }))
            }
            PLAINTEXT_FILE => Ok(Box::new(PlaintextFile {
                path: self.credentials_file.clone(),
            })),
            other => Err(AppError::config(format!(
                "unknown password backend {other:?} in remembered login"
            ))
            .with_hint("Run `klms auth forget` to discard it.")),
        }
    }
}

fn is_executable(path: &Path) -> bool {
    let Ok(metadata) = fs::metadata(path) else {
        return false;
    };
    if !metadata.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        metadata.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

fn check_secret_text(account: &str, secret: &str) -> Result<(), AppError> {
    if secret.is_empty() || secret.chars().any(char::is_control) {
        return Err(AppError::usage(
            "a remembered password cannot be empty or contain control characters",
        ));
    }
    if account.is_empty() || account.chars().any(char::is_control) {
        return Err(AppError::usage("invalid account name for password storage"));
    }
    Ok(())
}

struct Finished {
    code: Option<i32>,
    stdout: Zeroizing<Vec<u8>>,
    stderr: String,
}

/// Run a helper with `stdin` (never argv or env) and a hard timeout.
fn run_helper(program: &Path, args: &[&str], stdin: Option<&[u8]>) -> Result<Finished, AppError> {
    let name = program
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    let mut child = Command::new(program)
        .args(args)
        .stdin(if stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| AppError::config(format!("cannot run `{name}`: {error}")))?;
    if let (Some(bytes), Some(mut pipe)) = (stdin, child.stdin.take()) {
        // A helper that exits early closes the pipe; its status reports why.
        let _ = pipe.write_all(bytes);
    }
    let mut stdout = child.stdout.take().expect("piped stdout");
    let mut stderr = child.stderr.take().expect("piped stderr");
    let out = thread::spawn(move || {
        let mut buffer = Zeroizing::new(Vec::new());
        let _ = stdout.by_ref().take(1 << 20).read_to_end(&mut buffer);
        buffer
    });
    let err = thread::spawn(move || {
        let mut buffer = Vec::new();
        let _ = stderr.by_ref().take(1 << 16).read_to_end(&mut buffer);
        buffer
    });
    let deadline = Instant::now() + HELPER_TIMEOUT;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(5)),
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(AppError::config(format!(
                    "`{name}` did not finish within {} seconds",
                    HELPER_TIMEOUT.as_secs()
                )));
            }
            Err(error) => {
                return Err(AppError::config(format!(
                    "cannot wait for `{name}`: {error}"
                )));
            }
        }
    };
    Ok(Finished {
        code: status.code(),
        stdout: out.join().unwrap_or_default(),
        stderr: String::from_utf8_lossy(&err.join().unwrap_or_default())
            .trim()
            .to_owned(),
    })
}

fn secret_from(stdout: &[u8]) -> Result<Zeroizing<String>, AppError> {
    let text = std::str::from_utf8(stdout)
        .map_err(|_| AppError::config("the stored password is not valid UTF-8"))?;
    Ok(Zeroizing::new(
        text.trim_end_matches(['\n', '\r']).to_owned(),
    ))
}

/// macOS login keychain via `security -i` (commands on stdin).
pub struct Keychain {
    pub program: PathBuf,
}

/// Quote one word for the `security -i` command parser.
fn security_quote(word: &str) -> String {
    let mut quoted = String::with_capacity(word.len() + 2);
    quoted.push('"');
    for character in word.chars() {
        if matches!(character, '"' | '\\') {
            quoted.push('\\');
        }
        quoted.push(character);
    }
    quoted.push('"');
    quoted
}

impl SecretStore for Keychain {
    fn kind(&self) -> &'static str {
        KEYCHAIN
    }

    fn store(&self, account: &str, secret: &str) -> Result<(), AppError> {
        check_secret_text(account, secret)?;
        let command = Zeroizing::new(format!(
            "add-generic-password -U -s {SERVICE} -a {} -w {}\n",
            security_quote(account),
            security_quote(secret)
        ));
        let finished = run_helper(&self.program, &["-i"], Some(command.as_bytes()))?;
        // `security -i` can report command failures only on stderr, so prove
        // the write by reading it back.
        let stored = self.lookup(account)?;
        if finished.code != Some(0) || stored.as_deref().map(String::as_str) != Some(secret) {
            return Err(
                AppError::config("the macOS keychain did not accept the password").with_hint(
                    "Unlock the login keychain and retry, or omit `--remember-password`.",
                ),
            );
        }
        Ok(())
    }

    fn lookup(&self, account: &str) -> Result<Option<Zeroizing<String>>, AppError> {
        let finished = run_helper(
            &self.program,
            &["find-generic-password", "-s", SERVICE, "-a", account, "-w"],
            None,
        )?;
        match finished.code {
            Some(0) => secret_from(&finished.stdout).map(Some),
            // errSecItemNotFound
            Some(44) => Ok(None),
            _ => Err(AppError::config(format!(
                "macOS keychain lookup failed: {}",
                finished.stderr
            ))),
        }
    }

    fn clear(&self, account: &str) -> Result<(), AppError> {
        let finished = run_helper(
            &self.program,
            &["delete-generic-password", "-s", SERVICE, "-a", account],
            None,
        )?;
        match finished.code {
            Some(0 | 44) => Ok(()),
            _ => Err(AppError::config(format!(
                "macOS keychain delete failed: {}",
                finished.stderr
            ))),
        }
    }
}

/// Linux Secret Service (GNOME Keyring, KWallet) via `secret-tool`.
pub struct SecretService {
    pub program: PathBuf,
}

impl SecretStore for SecretService {
    fn kind(&self) -> &'static str {
        SECRET_SERVICE
    }

    fn store(&self, account: &str, secret: &str) -> Result<(), AppError> {
        check_secret_text(account, secret)?;
        let finished = run_helper(
            &self.program,
            &[
                "store",
                "--label=klms",
                "service",
                SERVICE,
                "account",
                account,
            ],
            Some(secret.as_bytes()),
        )?;
        if finished.code != Some(0) {
            return Err(AppError::config(format!(
                "the Secret Service refused the password: {}",
                finished.stderr
            )));
        }
        Ok(())
    }

    fn lookup(&self, account: &str) -> Result<Option<Zeroizing<String>>, AppError> {
        let finished = run_helper(
            &self.program,
            &["lookup", "service", SERVICE, "account", account],
            None,
        )?;
        match (finished.code, finished.stderr.is_empty()) {
            (Some(0), _) if !finished.stdout.is_empty() => secret_from(&finished.stdout).map(Some),
            // Not found: nonzero (or empty success) with nothing on stderr.
            (Some(0 | 1), true) => Ok(None),
            _ => Err(AppError::config(format!(
                "Secret Service lookup failed: {}",
                finished.stderr
            ))),
        }
    }

    fn clear(&self, account: &str) -> Result<(), AppError> {
        let finished = run_helper(
            &self.program,
            &["clear", "service", SERVICE, "account", account],
            None,
        )?;
        match (finished.code, finished.stderr.is_empty()) {
            (Some(0), _) | (Some(1), true) => Ok(()),
            _ => Err(AppError::config(format!(
                "Secret Service delete failed: {}",
                finished.stderr
            ))),
        }
    }
}

/// Plaintext 0600 file, used only after explicit `--insecure-storage`.
pub struct PlaintextFile {
    pub path: PathBuf,
}

#[derive(Default, Serialize, Deserialize)]
struct CredentialFile {
    #[serde(default)]
    version: u32,
    #[serde(default)]
    passwords: BTreeMap<String, String>,
}

impl PlaintextFile {
    fn read(&self) -> Result<CredentialFile, AppError> {
        match fs::read(&self.path) {
            Ok(bytes) => serde_json::from_slice(&bytes).map_err(|error| {
                AppError::config(format!(
                    "invalid credentials file {}: {error}",
                    self.path.display()
                ))
                .with_hint("Run `klms auth forget` to remove it.")
            }),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                Ok(CredentialFile::default())
            }
            Err(error) => Err(AppError::config(format!(
                "cannot read credentials file {}: {error}",
                self.path.display()
            ))),
        }
    }

    fn write(&self, file: &CredentialFile) -> Result<(), AppError> {
        if file.passwords.is_empty() {
            fsutil::remove_file(&self.path, "credentials file")?;
            return Ok(());
        }
        let bytes =
            Zeroizing::new(serde_json::to_vec_pretty(file).map_err(|error| {
                AppError::internal(format!("failed to encode password: {error}"))
            })?);
        fsutil::write_private(&self.path, &bytes, "credentials")
    }
}

impl SecretStore for PlaintextFile {
    fn kind(&self) -> &'static str {
        PLAINTEXT_FILE
    }

    fn store(&self, account: &str, secret: &str) -> Result<(), AppError> {
        check_secret_text(account, secret)?;
        let mut file = self.read()?;
        file.version = 1;
        file.passwords.insert(account.to_owned(), secret.to_owned());
        self.write(&file)
    }

    fn lookup(&self, account: &str) -> Result<Option<Zeroizing<String>>, AppError> {
        Ok(self
            .read()?
            .passwords
            .get(account)
            .map(|secret| Zeroizing::new(secret.clone())))
    }

    fn clear(&self, account: &str) -> Result<(), AppError> {
        let mut file = self.read()?;
        if file.passwords.remove(account).is_some() {
            self.write(&file)?;
        }
        Ok(())
    }
}

#[cfg(all(test, unix))]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use super::*;

    fn script(dir: &Path, name: &str, body: &str) -> PathBuf {
        let path = dir.join(name);
        fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    /// A fake `secret-tool` keeping secrets in `$STORE/<account>`; the secret
    /// arrives on stdin and argv is logged so tests can prove it never leaks.
    fn fake_secret_tool(dir: &Path, store: &Path) -> PathBuf {
        script(
            dir,
            "secret-tool",
            &format!(
                r#"echo "$@" >> "{store}/argv.log"
verb="$1"
case "$verb" in
  store) account="$6"; cat > "{store}/$account" ;;
  lookup) account="$5"; if [ -f "{store}/$account" ]; then cat "{store}/$account"; exit 0; else exit 1; fi ;;
  clear) account="$5"; rm -f "{store}/$account" ;;
esac"#,
                store = store.display()
            ),
        )
    }

    #[test]
    fn secret_service_keeps_the_secret_on_stdin_only() {
        let dir = tempfile::tempdir().unwrap();
        let store = tempfile::tempdir().unwrap();
        let program = fake_secret_tool(dir.path(), store.path());
        let backend = SecretService { program };
        assert!(backend.lookup("student").unwrap().is_none());
        backend.store("student", "hunter2 \"x\" y").unwrap();
        assert_eq!(
            backend.lookup("student").unwrap().unwrap().as_str(),
            "hunter2 \"x\" y"
        );
        backend.clear("student").unwrap();
        backend.clear("student").unwrap();
        assert!(backend.lookup("student").unwrap().is_none());
        let argv = fs::read_to_string(store.path().join("argv.log")).unwrap();
        assert!(!argv.contains("hunter2"), "{argv}");
        assert!(argv.contains("store --label=klms service klms account student"));
    }

    #[test]
    fn keychain_commands_travel_on_stdin_and_are_read_back() {
        let dir = tempfile::tempdir().unwrap();
        let store = tempfile::tempdir().unwrap();
        // Fake `security`: `-i` stores the quoted -w word from stdin.
        let program = script(
            dir.path(),
            "security",
            &format!(
                r#"echo "$@" >> "{store}/argv.log"
case "$1" in
  -i) IFS= read -r line; printf '%s\n' "$line" > "{store}/stdin.log"
      secret=$(printf '%s' "$line" | sed 's/.* -w "\(.*\)"$/\1/' | sed 's/\\\(.\)/\1/g')
      printf '%s' "$secret" > "{store}/secret" ;;
  find-generic-password) if [ -f "{store}/secret" ]; then cat "{store}/secret"; echo; exit 0; else echo "not found" >&2; exit 44; fi ;;
  delete-generic-password) if [ -f "{store}/secret" ]; then rm "{store}/secret"; exit 0; else exit 44; fi ;;
esac"#,
                store = store.path().display()
            ),
        );
        let backend = Keychain { program };
        assert!(backend.lookup("student").unwrap().is_none());
        backend.store("student", "pa\"ss\\word").unwrap();
        assert_eq!(
            backend.lookup("student").unwrap().unwrap().as_str(),
            "pa\"ss\\word"
        );
        let stdin = fs::read_to_string(store.path().join("stdin.log")).unwrap();
        assert!(stdin.starts_with("add-generic-password -U -s klms -a \"student\" -w "));
        let argv = fs::read_to_string(store.path().join("argv.log")).unwrap();
        assert!(!argv.contains("pa\"ss"), "{argv}");
        backend.clear("student").unwrap();
        backend.clear("student").unwrap();
        assert!(backend.lookup("student").unwrap().is_none());
    }

    #[test]
    fn keychain_store_fails_loudly_when_nothing_was_written() {
        let dir = tempfile::tempdir().unwrap();
        let program = script(
            dir.path(),
            "security",
            r#"case "$1" in find-generic-password) exit 44;; *) cat >/dev/null; exit 0;; esac"#,
        );
        let error = Keychain { program }.store("student", "pw").unwrap_err();
        assert_eq!(error.code, "CONFIG_ERROR");
    }

    #[test]
    fn plaintext_file_is_private_and_removed_when_empty() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("klms/credentials.json");
        let backend = PlaintextFile { path: path.clone() };
        backend.store("a", "one").unwrap();
        backend.store("b", "two").unwrap();
        let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        assert_eq!(backend.lookup("a").unwrap().unwrap().as_str(), "one");
        backend.clear("a").unwrap();
        assert!(backend.lookup("a").unwrap().is_none());
        backend.clear("b").unwrap();
        backend.clear("b").unwrap();
        assert!(!path.exists());
    }

    #[test]
    fn rejects_control_characters_in_secrets() {
        let dir = tempfile::tempdir().unwrap();
        let backend = PlaintextFile {
            path: dir.path().join("c.json"),
        };
        assert!(backend.store("a", "x\ny").is_err());
        assert!(backend.store("a", "").is_err());
        assert!(backend.store("a\n", "x").is_err());
    }

    fn provider(path: &Path, dir: &Path) -> SystemProvider {
        SystemProvider {
            platform: Platform::Other,
            security_program: PathBuf::from("/nonexistent/security"),
            search_path: path.as_os_str().to_owned(),
            credentials_file: dir.join("credentials.json"),
        }
    }

    #[test]
    fn headless_linux_refuses_without_consent_and_uses_a_file_with_it() {
        let empty = tempfile::tempdir().unwrap();
        let config = tempfile::tempdir().unwrap();
        let provider = provider(empty.path(), config.path());
        let error = provider.choose(false).err().unwrap();
        assert!(error.message.contains("no OS keyring"));
        assert!(
            error
                .hint
                .as_deref()
                .unwrap()
                .contains("--insecure-storage")
        );
        assert_eq!(provider.choose(true).unwrap().kind(), PLAINTEXT_FILE);
    }

    #[test]
    fn secret_tool_is_used_only_when_the_probe_reaches_a_service() {
        let config = tempfile::tempdir().unwrap();
        let bin = tempfile::tempdir().unwrap();
        let store = tempfile::tempdir().unwrap();
        fake_secret_tool(bin.path(), store.path());
        let ok = provider(bin.path(), config.path());
        assert_eq!(ok.choose(false).unwrap().kind(), SECRET_SERVICE);
        assert_eq!(ok.open(SECRET_SERVICE).unwrap().kind(), SECRET_SERVICE);

        let broken = tempfile::tempdir().unwrap();
        script(
            broken.path(),
            "secret-tool",
            "echo 'Cannot autolaunch D-Bus without X11 $DISPLAY' >&2; exit 1",
        );
        let dead = provider(broken.path(), config.path());
        assert!(dead.choose(false).is_err());
        assert_eq!(dead.choose(true).unwrap().kind(), PLAINTEXT_FILE);
    }

    #[test]
    fn macos_always_picks_the_keychain() {
        let config = tempfile::tempdir().unwrap();
        let mut mac = provider(config.path(), config.path());
        mac.platform = Platform::MacOs;
        assert_eq!(mac.choose(false).unwrap().kind(), KEYCHAIN);
        assert_eq!(mac.open(KEYCHAIN).unwrap().kind(), KEYCHAIN);
        assert!(mac.open("bogus").is_err());
    }
}
