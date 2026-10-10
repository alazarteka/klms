# Project instructions

`klms` is a small Rust CLI for KAIST KLMS, read-only toward course data. Keep
that scope: no submitting, posting, quiz starts, or attendance, and no browser
automation. `klms --help` is the user and agent reference; keep it accurate.

## Code

One crate with a thin binary. Keep KLMS/Moodle selectors in `parse`,
credential and SSO handling in `auth`, network policy in `client`, SQLite and
the object store only in `corpus`, grammar and help in `cli`, and the JSON
envelope in `output`. Dependency direction and module map: `docs/ARCHITECTURE.md`;
JSON, refs, and library invariants: `docs/CONTRACT.md`. `unsafe_code` stays
forbidden.

## Checks

Run `make check` (fmt, `cargo test --locked --all-targets`, clippy with
`-D warnings`) before finishing. Use the pinned toolchain, exact `=` dependency
pins in `Cargo.toml`, the committed `Cargo.lock`, and `--locked`; pin GitHub
Actions to full commit SHAs. Behavior changes need tests. No ignored tests,
lint suppressions, or placeholder success paths.

## Data

Never commit KLMS HTML, cookies, grades, attendance, or other personal course
data from a real account. Parser changes need a redacted or synthetic fixture.
