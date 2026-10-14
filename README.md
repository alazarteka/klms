# klms

`klms` is a terminal client for KAIST's Learning Management System (KLMS):
what is due, course notices, grades and attendance, and course file downloads,
plus an optional private local library. Activity reads may record views or
completion; see [side effects and safety](docs/CONTRACT.md#safety).

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

Coursework indexes and grade/attendance report headers recognize English and
Korean KLMS labels. Calendar cards prefer structural timestamps and their event
heading, with English/Korean date text as a fallback in Asia/Seoul. Other text
locales are not verified; an unrecognized dated event fails agenda generation
rather than being silently omitted.

## Upgrading

`klms update --check` reports the latest stable release; `klms update` installs
it after verifying its SHA-256. Nothing checks for updates unless you ask.
The former `klms skill` command and bundled SKILL.md were removed. Updating
preserves any installed legacy copy and warns with its paths so you can delete
it yourself.

## Development

`make check` runs fmt, tests and clippy. See [AGENTS.md](AGENTS.md),
[docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) and [docs/CONTRACT.md](docs/CONTRACT.md).
`python3 scripts/release_smoke.py ARCHIVE.tar.gz` checks a release archive offline.

## License

MIT
