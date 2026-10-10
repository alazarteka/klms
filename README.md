# klms

`klms` puts KAIST's Learning Management System (KLMS) in your terminal: what is
due, course notices, grades and attendance, and course file downloads, plus an
optional private local library that keeps history across KLMS changes. It is
read-only toward course data: it never submits work, starts quizzes, posts to a
board, or checks you into class. Normal use does not open a browser.

`klms --help` is the reference (workflow, JSON contract, exit codes, refs);
`klms <command> --help` has the details and examples.

## Install

Release binaries exist for Apple Silicon macOS and x86-64 Linux. Download the
installer, read it if you like, and run it:

```bash
curl --proto '=https' --tlsv1.2 -fsSLo install-klms.sh \
  https://raw.githubusercontent.com/alazarteka/klms/main/scripts/install.sh
bash install-klms.sh
```

It verifies the release archive's SHA-256 and installs `klms` in
`~/.local/bin` (set `KLMS_INSTALL_DIR` to change that; make sure the directory
is on your `PATH`). From source (Rust 1.86 or newer): `make build`, then
`target/release/klms __install --destination ~/.local/bin/klms`.

## Sign in

```bash
klms auth login
```

Easy Login (approve the number in the KAIST app) is the default. Password login
uses an emailed or SMS six-digit code:

```bash
klms auth login --method password --second-factor email --remember-password
```

Passwords are read with terminal echo off and are never flags or environment
variables. `--remember-password` stores the password in the macOS keychain or
the Linux Secret Service (`--insecure-storage` opts in to a private plaintext
file where there is neither).

Agents and scripts have no terminal, so Easy Login cannot work. After one
interactive `--remember-password` login, sign in in two steps:

```bash
klms --json auth login --method password   # sends the code, exits 12 CODE_REQUIRED
klms --json auth login --code 123456       # the code from the email or SMS
```

An agent that can read that inbox may fetch the code itself; otherwise ask the
user for the code (never the password). The session is saved at
`$XDG_STATE_HOME/klms/session.json` (default `~/.local/state/klms/`), mode 0600.
`klms auth status`, `doctor`, `auth logout` and `auth forget` inspect or clear
local state.

## Use it

```bash
klms today
klms upcoming --through 7d
klms courses list
klms assignments list --course course:12345
klms notices list --course course:12345
klms files download file:1205160 --out /abs/path/notes.pdf
klms library sync --notices --files   # explicit; builds the private local library
klms library search "compiler"
```

List commands print `ref` values (`assign:ID`, `board-post:BOARD:POST`,
`file:ID`, ...); pass them back to the matching `show`/`download` command.
Put `--json` before the command for one versioned JSON document;
`klms --json spec` prints the full argument tree and `klms completions SHELL`
generates completions.

## Upgrading

`klms update --check` and `klms update` fetch the latest stable GitHub release,
verify its SHA-256, and replace the executable you ran (following a symlink).
Nothing checks for updates unless you ask. Earlier releases installed an Agent
Skill copy of the help text; installing or updating removes that managed copy
(`~/.local/share/klms/skills/klms` and the `~/.agents/skills/klms` symlink to
it) and leaves anything else alone. The help text is the agent guidance now.

## Development

`make check` runs fmt, tests, and clippy; see [AGENTS.md](AGENTS.md),
[docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) and [docs/CONTRACT.md](docs/CONTRACT.md).
`python3 scripts/release_smoke.py ARCHIVE.tar.gz` checks a release archive offline.

## License

MIT
