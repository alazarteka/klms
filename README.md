# klms

`klms` is a terminal client for KAIST's Learning Management System (KLMS):
what is due, course notices, grades and attendance, and course file downloads,
plus an optional private local library. It is read-only toward course data.
It never submits work, starts quizzes, posts, or checks you into class.

`klms --help` is the manual: workflow, JSON contract, exit codes and ref
formats. `klms <command> --help` covers each command.

## Install

Release binaries exist for Apple Silicon macOS and x86-64 Linux:

```bash
curl --proto '=https' --tlsv1.2 -fsSLo install-klms.sh \
  https://raw.githubusercontent.com/alazarteka/klms/main/scripts/install.sh
bash install-klms.sh
```

The script verifies the archive's SHA-256 and installs `klms` into `~/.local/bin`
(set `KLMS_INSTALL_DIR` to change it). From source, with Rust 1.86 or newer:
`make build`, then `target/release/klms __install --destination ~/.local/bin/klms`.

## Sign in

```bash
klms auth login
```

Run `klms auth login --help` for password, remembered-login and agent
(two-step) flows.

## Examples

```bash
klms today
klms upcoming --through 7d
klms --json assignments list --course course:12345
klms files download file:1205160 --out ./notes.pdf
klms library sync --notices --files && klms library search "compiler"
```

## Upgrading

`klms update --check` reports the latest stable release; `klms update` installs
it after verifying its SHA-256. Nothing checks for updates unless you ask.
Earlier releases installed an Agent Skill copy of the help text. Installing or
updating removes that managed copy and leaves anything else alone; the help
text is the agent guidance now.

## Development

`make check` runs fmt, tests and clippy. See [AGENTS.md](AGENTS.md),
[docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) and [docs/CONTRACT.md](docs/CONTRACT.md).
`python3 scripts/release_smoke.py ARCHIVE.tar.gz` checks a release archive offline.

## License

MIT
