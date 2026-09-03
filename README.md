# klms

`klms` puts KAIST's Learning Management System in your terminal. It can show
what is due today, look through a course, read notices, inspect grades and
attendance, and download course files. Normal use does not open a browser.

The client is deliberately read-oriented. It will not submit an assignment,
start a quiz, post to a board, or check you into class.

## Install

The release page has binaries for Apple Silicon macOS and x86-64 Linux. Download
the installer, have a look if you like, and run it:

```bash
curl --proto '=https' --tlsv1.2 -fsSLo install-klms.sh \
  https://raw.githubusercontent.com/alazarteka/klms/main/scripts/install.sh
less install-klms.sh
bash install-klms.sh
```

The script finds the latest release, downloads the archive and its published
SHA-256 file, verifies the checksum, and installs `klms` under `~/.local/bin`.
It also installs the matching companion Agent Skill embedded in that release.
Set `KLMS_INSTALL_DIR` to choose another binary directory. From a cloned
checkout, run `bash scripts/install.sh` instead.

If `~/.local/bin` is not already on your `PATH`, add it in your shell setup:

```bash
export PATH="$HOME/.local/bin:$PATH"
```

The archives and their checksums are also available on the
[GitHub releases page](https://github.com/alazarteka/klms/releases).

## Sign in

Easy Login is the default:

```bash
klms auth login
```

Enter your KAIST email address or phone number, then approve the comparison
number in the KAIST app. Password login works with either email or SMS
verification:

```bash
klms auth login --method password --second-factor email
klms auth login --method password --second-factor sms
```

Passwords and six-digit verification codes are read with terminal echo turned
off. They are not accepted as flags or environment variables. If KAIST treats
the client as a new device, `klms` registers it during login; there is no
separate browser step.

The saved session contains only the KLMS cookies and trusted-device identifiers
needed for later logins. It lives at `$XDG_STATE_HOME/klms/session.json`, or
`~/.local/state/klms/session.json` when `XDG_STATE_HOME` is unset. Passwords,
verification codes, general KAIST SSO cookies, raw HTML, and Moodle session
keys are never written there.

Useful checks:

```bash
klms auth status       # describe the saved session without printing secrets
klms doctor            # make a small live request and check that it still works
klms auth time-left    # ask KLMS how much time remains
klms auth logout       # remove the local session
```

`klms auth extend` refreshes an existing session timer. It cannot revive an
expired session.

## Use it

Start with the day in front of you:

```bash
klms today
klms upcoming --through 7d
klms dashboard
```

Then narrow things down by course:

```bash
klms courses list
klms courses resolve "machine learning"
klms assignments list --course course:12345
klms notices list --course course:12345
klms grades show --course course:12345
klms attendance show --course course:12345
```

List commands return references such as `assign:1210516`,
`board-post:1189554:439261`, and `file:1205160`. Pass a reference directly to a
matching detail or download command:

```bash
klms assignments show assign:1210516
klms notices show board-post:1189554:439261
klms files download file:1205160 --out lecture-notes.pdf
```

Run `klms --help` or `klms <command> --help` for the rest of the command
surface, including activities, quizzes, calendar events, boards, videos, and
course files.

For a private history that survives KLMS changes, initialize and synchronize
the local versioned library explicitly:

```bash
klms library status
klms library sync --files
klms library sync --notices --files --download changed
klms library search "compiler" --limit 20
klms library changes
klms library activity --subject file:1205160
```

The library stores normalized observations in SQLite and exact downloaded
bytes once in a private SHA-256 object store under the XDG data directory. It
does not schedule itself, write to KLMS, follow authenticated third-party
links, or infer that an unlisted course was dropped. See
[the local-library contract](docs/LOCAL_LIBRARY.md).

## JSON and agent use

Put `--json` before the command for stable machine-readable output:

```bash
klms --json today
klms --json assignments list --course course:12345
```

The [command contract](docs/COMMAND_CONTRACT.md) documents reference
resolution, output schemas, retries, and safety boundaries.

`klms skill install` installs the companion Agent Skill embedded in the binary
under `~/.local/share/klms/skills/klms` and links it from
`~/.agents/skills/klms`. Set `XDG_DATA_HOME` to use another data root. Run
`klms skill status` to inspect the installation.

## Build from source

This project uses Rust 1.86 or newer. Dependency changes have an additional
review gate; read [SECURITY.md](SECURITY.md) and
[docs/DEPENDENCY_UPDATES.md](docs/DEPENDENCY_UPDATES.md) before editing
`Cargo.toml` or `Cargo.lock`.

For an unchanged lockfile:

```bash
make check
make install-local
```

`make install-local` builds the release binary, installs it under
`~/.local/bin` by default, and installs the matching companion skill.

## License

MIT
