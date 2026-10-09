# Project guidance

Keep `klms` a small Rust client that is read-only toward KLMS. Preserve the dependency direction
documented in `docs/ARCHITECTURE.md`; Moodle/KLMS selectors belong in `parse`,
credential discovery in `auth`, network policy in `client`, and `corpus` is the
only module allowed to touch SQLite or the object store.

Verify code changes with `make check`. When the dependency graph changes, also
run `./scripts/cargo-deny.sh check`. Use exact dependency versions, commit
`Cargo.lock`, and pin GitHub Actions to full commit SHAs.

Parser changes need a redacted fixture or narrow synthetic case and, when
practical, a read-only live shape check. Never commit KLMS HTML, cookies,
grades, attendance records, or other personal course data captured from a real
account.
