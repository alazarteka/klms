# klms

`klms` is a fast, read-oriented command-line client for KAIST's Learning
Management System. It reads the same authenticated HTML and Moodle endpoints as
the web interface without launching a browser for ordinary commands.

The project is under active development. Its agent-facing surface covers the
authenticated session, course discovery, weekly activities, assignments,
quizzes, calendar events, course boards, files, video metadata, grades, and
attendance.

## Command surface

```text
klms doctor
klms auth login [--method easy|password] [--second-factor email|sms]
klms auth logout
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

`klms` owns its authentication. Run `klms auth login` for KAIST Easy Login, or
choose password login with email or SMS verification:

```bash
klms auth login --method easy
klms auth login --method password --second-factor email
klms auth login --method password --second-factor sms
```

Passwords and verification codes are read from the terminal with echo disabled;
they are never accepted as command arguments or environment variables. The
resulting KLMS-only cookie set is stored at
`$XDG_STATE_HOME/klms/session.json`, falling back to
`~/.local/state/klms/session.json`, with private permissions and atomic writes.
General KAIST SSO cookies, passwords, verification codes, encryption keys, raw
HTML, and Moodle session keys are not persisted.

Use `klms auth status` to inspect non-secret metadata, `klms auth logout` to
remove the local session, and `klms doctor` to validate it. `auth time-left`
reads the server timer; `auth extend` refreshes an already-valid session but
cannot log in.

If KAIST classifies the native client as a new device, `klms` registers it
automatically during the same login transaction and saves the returned trusted
device identifier for subsequent policy checks. No browser visit is required.

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
```

This installs `klms` under `~/.local/bin` by default. The binary contains the
matching companion Agent Skill and installs it with the binary:

```text
~/.local/share/klms/skills/klms/SKILL.md
~/.agents/skills/klms -> ~/.local/share/klms/skills/klms
```

`XDG_DATA_HOME` replaces `~/.local/share` when set. Run `klms skill install`
to reinstall the embedded skill or `klms skill status` to inspect it. The
compatibility target `make install-skill` invokes the installed binary rather
than copying from the checkout.

The companion skill teaches compatible agents to prefer this interface for
supported KLMS resources and to use its owned authentication commands.

## Scope

The current product performs remote reads, direct KAIST SSO login, explicit
local downloads, local Agent Skill installation, and explicit session
extension. It does not
submit assignments, begin quiz attempts, post messages, check into attendance,
or mutate third-party tools.

## License

MIT
