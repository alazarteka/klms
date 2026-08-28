# klms

`klms` is a fast, read-only command-line client for KAIST's Learning
Management System. It reads the same authenticated HTML and Moodle endpoints as
the web interface without launching a browser for ordinary commands.

The project is under active development. The initial vertical slice covers the
dashboard, courses, weekly activities, grades, attendance, and authentication
diagnostics.

## Command surface

```text
klms doctor
klms auth status
klms dashboard
klms courses list
klms courses show COURSE
klms activities list --course COURSE [--week N]
klms grades show --course COURSE
klms attendance show --course COURSE
```

Pass `--json` before the command for deterministic machine-readable output.

## Authentication

The initial client reads a Playwright-compatible storage-state file containing
an existing authenticated KLMS session. Resolution order is:

1. `KLMS_STORAGE_STATE`;
2. `~/.config/klms/storage-state.json`;
3. `~/.kaist-cli/private/klms/storage_state.json` as a migration bridge.

Cookie values are never printed. Use `klms doctor` or `klms auth status` to see
which source was selected and whether its metadata is usable.

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

The current product performs remote reads and explicit local downloads only.
It does not submit assignments, begin quiz attempts, post messages, check into
attendance, or mutate third-party tools.

## License

MIT
