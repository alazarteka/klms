//! Optional password storage: the macOS login keychain through
//! `/usr/bin/security`, a Linux Secret Service through `secret-tool`, or, only
//! when the user explicitly opts in, a plaintext 0600 file. Secrets reach the
//! helper programs on standard input only, never in argv or the environment.

use std::{
    collections::BTreeMap,
    ffi::OsString,
    io::Read,
    io::Write,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

use serde::{Deserialize, Serialize};
use zeroize::{Zeroize, Zeroizing};

use crate::error::AppError;

use super::store::{read_file, remove_file, save_json};

pub const PLAINTEXT_FILE: &str = "plaintext-file";
const KEYCHAIN: &str = "keychain";
const SECRET_SERVICE: &str = "secret-service";
const SERVICE: &str = "klms";
const PROBE_ACCOUNT: &str = "klms-availability-probe";
const HELPER_TIMEOUT: Duration = Duration::from_secs(30);
/// Longest password (in bytes) any backend will remember.
const MAX_SECRET: usize = 1024;

/// Chooses and reopens backends. Tests point it at fake helpers.
pub struct Secrets {
    pub mac: bool,
    pub security: PathBuf,
    /// Value of `PATH` used to find `secret-tool`.
    pub search_path: OsString,
    pub file: PathBuf,
}

/// Where one password lives; each variant holds its helper program or file.
pub enum Backend {
    Keychain(PathBuf),
    SecretService(PathBuf),
    File(PathBuf),
}

impl Secrets {
    pub fn new(file: PathBuf) -> Self {
        Self {
            mac: cfg!(target_os = "macos"),
            security: "/usr/bin/security".into(),
            search_path: std::env::var_os("PATH").unwrap_or_default(),
            file,
        }
    }

    pub(super) fn secret_tool(&self) -> Option<PathBuf> {
        std::env::split_paths(&self.search_path)
            .map(|dir| dir.join("secret-tool"))
            .find(|path| {
                path.metadata()
                    .is_ok_and(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
            })
    }

    /// Pick a backend for a new password. `allow_plaintext` is the user's
    /// explicit `--insecure-storage` consent.
    pub fn choose(&self, allow_plaintext: bool) -> Result<Backend, AppError> {
        if self.mac {
            return Ok(Backend::Keychain(self.security.clone()));
        }
        let mut reason = "`secret-tool` (libsecret) is not installed";
        if let Some(program) = self.secret_tool() {
            let service = Backend::SecretService(program);
            if service.lookup(PROBE_ACCOUNT).is_ok() {
                return Ok(service);
            }
            reason = "`secret-tool` cannot reach a Secret Service (no D-Bus session or unlocked keyring)";
        }
        if allow_plaintext {
            return Ok(Backend::File(self.file.clone()));
        }
        Err(AppError::config(format!(
            "cannot remember the password: there is no OS keyring here ({reason})"
        ))
        .with_hint(format!(
            "Rerun with `--insecure-storage` to store the password in a plaintext file readable only by you ({}), or omit `--remember-password` and enter the password each time.",
            self.file.display()
        )))
    }

    /// Reopen the backend a `login.json` recorded.
    pub fn open(&self, kind: &str) -> Result<Backend, AppError> {
        match kind {
            KEYCHAIN => Ok(Backend::Keychain(self.security.clone())),
            SECRET_SERVICE => self.secret_tool().map(Backend::SecretService).ok_or_else(|| {
                AppError::config(
                    "the stored password lives in the Secret Service but `secret-tool` is not installed",
                )
            }),
            PLAINTEXT_FILE => Ok(Backend::File(self.file.clone())),
            other => Err(AppError::config(format!(
                "unknown password backend {other:?} in remembered login"
            ))
            .with_hint("Check the password backend recorded in login.json.")),
        }
    }
}

struct Finished {
    code: Option<i32>,
    stdout: Zeroizing<Vec<u8>>,
    stderr: String,
}

/// Run a helper with `stdin` (never argv or env), capped output and a hard timeout.
fn run_helper(program: &Path, args: &[&str], stdin: Option<&[u8]>) -> Result<Finished, AppError> {
    let name = program.file_name().unwrap_or_default().to_string_lossy();
    let piped = |wanted: bool| {
        if wanted {
            Stdio::piped()
        } else {
            Stdio::null()
        }
    };
    let mut child = Command::new(program)
        .args(args)
        .stdin(piped(stdin.is_some()))
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| AppError::config(format!("cannot run `{name}`: {error}")))?;
    if let (Some(bytes), Some(mut pipe)) = (stdin, child.stdin.take()) {
        // A helper that exits early closes the pipe; its status reports why.
        let _ = pipe.write_all(bytes);
    }
    let drain = |pipe: Option<Box<dyn Read + Send>>, cap: u64| {
        thread::spawn(move || {
            let mut buffer = Zeroizing::new(Vec::new());
            if let Some(pipe) = pipe {
                let _ = pipe.take(cap).read_to_end(&mut buffer);
            }
            buffer
        })
    };
    let out = drain(child.stdout.take().map(|p| Box::new(p) as _), 1 << 20);
    let err = drain(child.stderr.take().map(|p| Box::new(p) as _), 1 << 16);
    let deadline = Instant::now() + HELPER_TIMEOUT;
    let status = loop {
        let waited = child.try_wait();
        if let Some(status) = waited
            .map_err(|error| AppError::config(format!("cannot wait for `{name}`: {error}")))?
        {
            break status;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err(AppError::config(format!(
                "`{name}` did not finish within {} seconds",
                HELPER_TIMEOUT.as_secs()
            )));
        }
        thread::sleep(Duration::from_millis(5));
    };
    let stderr = err.join().unwrap_or_default();
    Ok(Finished {
        code: status.code(),
        stdout: out.join().unwrap_or_default(),
        stderr: String::from_utf8_lossy(&stderr).trim().to_owned(),
    })
}

impl Finished {
    fn secret(&self) -> Result<Zeroizing<String>, AppError> {
        let text = std::str::from_utf8(&self.stdout)
            .map_err(|_| AppError::config("the stored password is not valid UTF-8"))?;
        Ok(Zeroizing::new(
            text.trim_end_matches(['\n', '\r']).to_owned(),
        ))
    }

    fn failure(&self, what: &str) -> AppError {
        AppError::config(format!("{what} failed: {}", self.stderr))
    }
}

/// Append one word quoted for the `security -i` command parser.
fn push_quoted(out: &mut String, word: &str) {
    out.push('"');
    for c in word.chars() {
        if matches!(c, '\\' | '"') {
            out.push('\\');
        }
        out.push(c);
    }
    out.push('"');
}

#[derive(Default, Serialize, Deserialize)]
struct CredentialFile {
    #[serde(default)]
    version: u32,
    #[serde(default)]
    passwords: BTreeMap<String, String>,
}

impl Drop for CredentialFile {
    fn drop(&mut self) {
        self.passwords.values_mut().for_each(Zeroize::zeroize);
    }
}

fn read_credentials(path: &Path) -> Result<CredentialFile, AppError> {
    let Some(bytes) = read_file(path, "credentials file")? else {
        return Ok(CredentialFile::default());
    };
    serde_json::from_slice(&bytes).map_err(|error| {
        AppError::config(format!(
            "invalid credentials file {}: {error}",
            path.display()
        ))
        .with_hint("Run `klms auth forget` to remove it.")
    })
}

fn write_credentials(path: &Path, file: &CredentialFile) -> Result<(), AppError> {
    if file.passwords.is_empty() {
        remove_file(path, "credentials file").map(drop)
    } else {
        save_json(path, file, "credentials")
    }
}

fn clear_secret_service(program: &Path, account: Option<&str>) -> Result<bool, AppError> {
    let mut args = vec!["clear", "service", SERVICE];
    if let Some(account) = account {
        args.extend(["account", account]);
    }
    let finished = run_helper(program, &args, None)?;
    let removed = match (finished.code, finished.stderr.is_empty()) {
        (Some(0), true) => true,
        (Some(1), true) => false,
        _ => return Err(finished.failure("Secret Service delete")),
    };
    // clear skips locked items. search --all includes them, without unlocking;
    // never expose its output (which may include secrets).
    args[0] = "search";
    args.insert(1, "--all");
    let remaining = run_helper(program, &args, None)?;
    if remaining.code != Some(0) || !remaining.stderr.is_empty() || !remaining.stdout.is_empty() {
        return Err(AppError::config(
            "could not confirm Secret Service deletion; klms entries may remain (unlock the keyring and retry `klms auth forget`)",
        ));
    }
    Ok(removed)
}

impl Backend {
    /// Delete every account in klms's dedicated service namespace.
    /// `true` means at least one item was removed; errors leave the result unknown.
    pub fn clear_all(&self) -> Result<bool, AppError> {
        match self {
            Self::Keychain(program) => {
                let mut removed = false;
                for _ in 0..256 {
                    let finished =
                        run_helper(program, &["delete-generic-password", "-s", SERVICE], None)?;
                    match finished.code {
                        Some(0) => removed = true,
                        Some(44) => return Ok(removed),
                        _ => return Err(finished.failure("macOS keychain delete")),
                    }
                }
                Err(AppError::config(
                    "macOS keychain purge reached the limit of 256 deletions; passwords may remain",
                ))
            }
            Self::SecretService(program) => clear_secret_service(program, None),
            Self::File(path) => remove_file(path, "credentials file"),
        }
    }

    pub fn kind(&self) -> &'static str {
        match self {
            Self::Keychain(_) => KEYCHAIN,
            Self::SecretService(_) => SECRET_SERVICE,
            Self::File(_) => PLAINTEXT_FILE,
        }
    }

    pub fn store(&self, account: &str, secret: &str) -> Result<(), AppError> {
        if secret.is_empty() || secret.chars().any(char::is_control) {
            return Err(AppError::usage(
                "a remembered password cannot be empty or contain control characters",
            ));
        }
        // Keeps a `security -i` command line far below its 4096-byte limit.
        if secret.len() > MAX_SECRET {
            return Err(AppError::usage(format!(
                "a remembered password cannot be longer than {MAX_SECRET} bytes"
            )));
        }
        if account.is_empty() || account.len() > MAX_SECRET || account.chars().any(char::is_control)
        {
            return Err(AppError::usage("invalid account name for password storage"));
        }
        match self {
            Self::Keychain(program) => {
                // `security -w` prints a password with any byte outside
                // printable ASCII as hex, so it could never be read back.
                if !secret.bytes().all(|byte| (b' '..=b'~').contains(&byte)) {
                    return Err(AppError::usage(
                        "the macOS keychain can only remember passwords of printable ASCII characters",
                    )
                    .with_hint("Omit `--remember-password` and enter the password each time."));
                }
                let mut command = Zeroizing::new(String::with_capacity(
                    64 + 2 * (account.len() + secret.len()),
                ));
                command.push_str("add-generic-password -U -s ");
                command.push_str(SERVICE);
                command.push_str(" -a ");
                push_quoted(&mut command, account);
                command.push_str(" -w ");
                push_quoted(&mut command, secret);
                command.push('\n');
                let finished = run_helper(program, &["-i"], Some(command.as_bytes()))?;
                // `security -i` can report command failures only on stderr, so
                // prove the write by reading it back.
                let stored = self.lookup(account);
                let stored = stored.as_ref().ok().and_then(Option::as_deref);
                if finished.code != Some(0) || stored.map(String::as_str) != Some(secret) {
                    // Whatever did land there is not the password we meant.
                    let mut error = AppError::config(
                        "the macOS keychain did not accept the password",
                    )
                    .with_hint(
                        "Unlock the login keychain and retry, or omit `--remember-password`.",
                    );
                    if let Err(cleanup) = self.clear(account) {
                        error
                            .message
                            .push_str(&format!("; rollback failed: {}", cleanup.message));
                    }
                    return Err(error);
                }
                Ok(())
            }
            Self::SecretService(program) => {
                let args = [
                    "store",
                    "--label=klms",
                    "service",
                    SERVICE,
                    "account",
                    account,
                ];
                let finished = run_helper(program, &args, Some(secret.as_bytes()))?;
                if finished.code != Some(0) {
                    return Err(AppError::config(format!(
                        "the Secret Service refused the password: {}",
                        finished.stderr
                    )));
                }
                Ok(())
            }
            Self::File(path) => {
                let mut file = read_credentials(path)?;
                file.version = 1;
                file.passwords.insert(account.to_owned(), secret.to_owned());
                write_credentials(path, &file)
            }
        }
    }

    pub fn lookup(&self, account: &str) -> Result<Option<Zeroizing<String>>, AppError> {
        match self {
            Self::Keychain(program) => {
                let args = ["find-generic-password", "-s", SERVICE, "-a", account, "-w"];
                let finished = run_helper(program, &args, None)?;
                match finished.code {
                    Some(0) => finished.secret().map(Some),
                    Some(44) => Ok(None), // errSecItemNotFound
                    _ => Err(finished.failure("macOS keychain lookup")),
                }
            }
            Self::SecretService(program) => {
                let args = ["lookup", "service", SERVICE, "account", account];
                let finished = run_helper(program, &args, None)?;
                match (finished.code, finished.stderr.is_empty()) {
                    (Some(0), _) if !finished.stdout.is_empty() => finished.secret().map(Some),
                    // Not found: nonzero (or empty success) with nothing on stderr.
                    (Some(0 | 1), true) => Ok(None),
                    _ => Err(finished.failure("Secret Service lookup")),
                }
            }
            Self::File(path) => Ok(read_credentials(path)?
                .passwords
                .get(account)
                .map(|secret| Zeroizing::new(secret.clone()))),
        }
    }

    /// Delete the secret; deleting an absent one succeeds.
    pub fn clear(&self, account: &str) -> Result<(), AppError> {
        match self {
            Self::Keychain(program) => {
                let args = ["delete-generic-password", "-s", SERVICE, "-a", account];
                let finished = run_helper(program, &args, None)?;
                match finished.code {
                    Some(0 | 44) => Ok(()),
                    _ => Err(finished.failure("macOS keychain delete")),
                }
            }
            Self::SecretService(program) => clear_secret_service(program, Some(account)).map(drop),
            Self::File(path) => {
                let mut file = read_credentials(path)?;
                if file.passwords.remove(account).is_some() {
                    write_credentials(path, &file)?;
                }
                Ok(())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    fn script(dir: &Path, name: &str, body: &str) -> PathBuf {
        let path = dir.join(name);
        crate::fixture_executable::write_executable(&path, &format!("#!/bin/sh\n{body}\n"));
        path
    }

    #[test]
    fn keychain_purge_stops_after_256_successful_deletions() {
        let dir = tempfile::tempdir().unwrap();
        let calls = dir.path().join("calls");
        let program = script(
            dir.path(),
            "security",
            &format!("echo delete >> '{}'; exit 0", calls.display()),
        );
        let error = Backend::Keychain(program).clear_all().unwrap_err();
        assert_eq!(error.code, "CONFIG_ERROR");
        assert!(error.message.contains("limit of 256 deletions"));
        assert_eq!(fs::read_to_string(calls).unwrap().lines().count(), 256);
    }

    /// A fake `secret-tool` keeping secrets in `$STORE/<account>`; argv is
    /// logged so tests can prove the secret never appears there.
    fn fake_secret_tool(dir: &Path, store: &Path) -> PathBuf {
        let store = store.display();
        let body = format!(
            r#"echo "$@" >> "{store}/argv.log"
case "$1" in
  store) cat > "{store}/$6" ;;
  lookup) if [ -f "{store}/$5" ]; then cat "{store}/$5"; else exit 1; fi ;;
  clear) rm -f "{store}/$5" ;;
esac"#
        );
        script(dir, "secret-tool", &body)
    }

    fn secrets(search_path: &Path, dir: &Path) -> Secrets {
        Secrets {
            mac: false,
            security: "/nonexistent/security".into(),
            search_path: search_path.as_os_str().to_owned(),
            file: dir.join("credentials.json"),
        }
    }

    /// store -> lookup -> clear (twice) behave the same on every backend.
    fn exercise(backend: &Backend, secret: &str) {
        assert!(backend.lookup("student").unwrap().is_none());
        backend.store("student", secret).unwrap();
        assert_eq!(backend.lookup("student").unwrap().unwrap().as_str(), secret);
        backend.clear("student").unwrap();
        backend.clear("student").unwrap();
        assert!(backend.lookup("student").unwrap().is_none());
    }

    fn helpers_get_secrets_on_stdin_only() {
        let (bin, store) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        exercise(
            &Backend::SecretService(fake_secret_tool(bin.path(), store.path())),
            "hunter2 \"x\" y",
        );
        let argv = fs::read_to_string(store.path().join("argv.log")).unwrap();
        assert!(!argv.contains("hunter2"), "{argv}");
        assert!(argv.contains("store --label=klms service klms account student"));

        // Fake `security`: `-i` stores the quoted -w word read from stdin.
        let (bin, store) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let store = store.path();
        let program = script(
            bin.path(),
            "security",
            &format!(
                r#"echo "$@" >> "{s}/argv.log"
case "$1" in
  -i) IFS= read -r line; printf '%s\n' "$line" > "{s}/stdin.log"
      printf '%s' "$line" | sed 's/.* -w "\(.*\)"$/\1/' | sed 's/\\\(.\)/\1/g' > "{s}/secret" ;;
  find-generic-password) if [ -f "{s}/secret" ]; then cat "{s}/secret"; echo; else echo "not found" >&2; exit 44; fi ;;
  delete-generic-password) if [ -f "{s}/secret" ]; then rm "{s}/secret"; else exit 44; fi ;;
esac"#,
                s = store.display()
            ),
        );
        exercise(&Backend::Keychain(program), "pa\"ss\\word");
        let stdin = fs::read_to_string(store.join("stdin.log")).unwrap();
        assert!(stdin.starts_with("add-generic-password -U -s klms -a \"student\" -w "));
        assert!(
            !fs::read_to_string(store.join("argv.log"))
                .unwrap()
                .contains("pa\"ss")
        );
    }

    fn keychain_store_fails_loudly_when_nothing_was_written() {
        let dir = tempfile::tempdir().unwrap();
        let program = script(
            dir.path(),
            "security",
            r#"case "$1" in find-generic-password) exit 44;; *) cat >/dev/null; exit 0;; esac"#,
        );
        let error = Backend::Keychain(program)
            .store("student", "pw")
            .unwrap_err();
        assert_eq!(error.code, "CONFIG_ERROR");
    }

    fn keychain_refuses_what_it_cannot_read_back_and_removes_a_bad_write() {
        let missing = Backend::Keychain("/nonexistent/security".into());
        for (secret, message) in [
            (
                "pässword",
                "the macOS keychain can only remember passwords of printable ASCII characters",
            ),
            (
                &"p".repeat(1025)[..],
                "a remembered password cannot be longer than 1024 bytes",
            ),
        ] {
            let error = missing.store("student", secret).unwrap_err();
            assert_eq!((error.code, error.message.as_str()), ("USAGE", message));
        }
        assert!(
            missing
                .store("student", &"p".repeat(1024))
                .is_err_and(|error| {
                    error.message == "cannot run `security`: No such file or directory (os error 2)"
                })
        );

        // The write lands, but reads back as something else (as `security`
        // does with hex): the item is deleted again.
        let (bin, store) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let program = script(
            bin.path(),
            "security",
            &format!(
                r#"case "$1" in
  -i) cat >/dev/null; touch "{s}/item" ;;
  find-generic-password) echo 70c3a4; exit 0 ;;
  delete-generic-password) rm "{s}/item" ;;
esac"#,
                s = store.path().display()
            ),
        );
        let error = Backend::Keychain(program)
            .store("student", "pw")
            .unwrap_err();
        assert_eq!(
            (error.code, error.message.as_str()),
            (
                "CONFIG_ERROR",
                "the macOS keychain did not accept the password"
            )
        );
        assert!(!store.path().join("item").exists());
    }

    #[test]
    fn helper_programs_and_backend_choice() {
        helpers_get_secrets_on_stdin_only();
        keychain_store_fails_loudly_when_nothing_was_written();
        keychain_refuses_what_it_cannot_read_back_and_removes_a_bad_write();
        backend_choice_follows_the_platform_and_consent();
    }

    #[test]
    fn plaintext_file_is_private_validated_and_removed_when_empty() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("klms/credentials.json");
        let backend = Backend::File(path.clone());
        backend.store("a", "one").unwrap();
        backend.store("b", "two").unwrap();
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        backend.clear("a").unwrap();
        assert!(backend.lookup("a").unwrap().is_none());
        assert_eq!(backend.lookup("b").unwrap().unwrap().as_str(), "two");
        backend.clear("b").unwrap();
        assert!(!path.exists());
        for (account, secret) in [("a", "x\ny"), ("a", ""), ("a\n", "x")] {
            assert!(
                backend.store(account, secret).is_err(),
                "{account:?} {secret:?}"
            );
        }
    }

    fn backend_choice_follows_the_platform_and_consent() {
        let (empty, config) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let headless = secrets(empty.path(), config.path());
        let error = headless.choose(false).err().unwrap();
        assert!(error.message.contains("no OS keyring"));
        assert!(error.hint.unwrap().contains("--insecure-storage"));
        assert_eq!(headless.choose(true).unwrap().kind(), PLAINTEXT_FILE);

        let (bin, store) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        fake_secret_tool(bin.path(), store.path());
        let working = secrets(bin.path(), config.path());
        assert_eq!(working.choose(false).unwrap().kind(), SECRET_SERVICE);
        assert_eq!(working.open(SECRET_SERVICE).unwrap().kind(), SECRET_SERVICE);

        let broken = tempfile::tempdir().unwrap();
        script(
            broken.path(),
            "secret-tool",
            "echo 'Cannot autolaunch D-Bus without X11 $DISPLAY' >&2; exit 1",
        );
        let dead = secrets(broken.path(), config.path());
        assert!(dead.choose(false).is_err());
        assert_eq!(dead.choose(true).unwrap().kind(), PLAINTEXT_FILE);

        let mut mac = secrets(config.path(), config.path());
        mac.mac = true;
        assert_eq!(mac.choose(false).unwrap().kind(), KEYCHAIN);
        assert_eq!(mac.open(KEYCHAIN).unwrap().kind(), KEYCHAIN);
        assert!(mac.open("bogus").is_err());
    }
}
