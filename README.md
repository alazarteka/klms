# klms

`klms` is a fast, read-only command-line client for KAIST's Learning
Management System. It reads the same authenticated HTML and Moodle endpoints as
the web interface without launching a browser for ordinary commands.

The project is under active development. Its agent-facing surface covers the
authenticated session, course discovery, weekly activities, assignments,
quizzes, calendar events, course boards, files, video metadata, grades, and
attendance.

## Command surface

```text
klms doctor
klms auth status
klms auth time-left
klms auth extend
klms dashboard
klms today [--course COURSE]
klms upcoming [--through 7d] [--course COURSE]
klms courses list
klms courses resolve QUERY
klms courses show COURSE
klms activities list --course COURSE [--week N] [--kind KIND] [--limit N]
klms assignments list --course COURSE
klms assignments show ASSIGNMENT
klms quizzes list --course COURSE
klms quizzes show QUIZ
klms calendar list
klms boards list --course COURSE
klms boards posts BOARD
klms boards show BOARD_POST_REF
klms notices list --course COURSE
klms notices show NOTICE
klms files list --course COURSE
klms files download FILE_REF_OR_URL --out PATH
klms videos list --course COURSE
klms videos show VIDEO
klms grades show --course COURSE
klms attendance show --course COURSE
klms request get PATH [--max-bytes N]
```

Lists return canonical references that can be passed directly to detail and
download commands, such as `assign:1210516`, `board-post:1189554:439261`, and
`file:1205160`. Assignment and quiz lists expose typed Korea-time deadlines;
`today` and `upcoming` provide the corresponding human workflow.

Pass `--json` before the command for deterministic machine-readable output.
See [docs/COMMAND_CONTRACT.md](docs/COMMAND_CONTRACT.md) for resolution,
output, retry, and safety semantics.

## Authentication

The initial client reads a Playwright-compatible storage-state file containing
an existing authenticated KLMS session. Resolution order is:

1. `KLMS_STORAGE_STATE`;
2. `~/.config/klms/storage-state.json`;
3. `~/.kaist-cli/private/klms/storage_state.json` as a migration bridge.

Cookie values and Moodle session keys are never printed. Use `klms doctor` or
`klms auth status` to see which source was selected and whether its metadata is
usable. `auth time-left` reads the server timer; `auth extend` is the sole
explicit remote mutation and refreshes that timer.

## Development

Dependency review precedes compilation. See [SECURITY.md](SECURITY.md) and
[docs/DEPENDENCY_UPDATES.md](docs/DEPENDENCY_UPDATES.md) before changing
`Cargo.toml` or `Cargo.lock`.

After the dependency policy passes:

```bash
cargo fmt --all -- --check
cargo test --locked
cargo clippy --locked --all-targets -- -D warnings
```

Install locally with:

```bash
make install-local
make install-skill
```

The companion skill teaches Codex to prefer this read-only interface for the
supported KLMS resources and to delegate interactive authentication back to
the established `kaist` CLI.

## Scope

The current product performs remote reads, explicit local downloads, and
explicit session extension only. It does not submit assignments, begin quiz
attempts, post messages, check into attendance, or mutate third-party tools.

## License

MIT
