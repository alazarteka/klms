# Dependency updates

Dependency changes execute third-party code and require a separate trust
decision. Do not run `cargo build`, `cargo test`, `cargo check`, `cargo clippy`,
`cargo run`, rust-analyzer, or IDE Cargo import after changing `Cargo.toml` or
`Cargo.lock` until this sequence passes.

1. Confirm the dependency is necessary and that the standard library or an
   existing reviewed crate cannot solve the job clearly.
2. Check the exact crate name for typosquatting and review its crates.io owner,
   repository, recent publisher changes, release timing, and download history.
3. Generate or inspect the lockfile without compiling.
4. Review the complete added graph with `cargo metadata --locked` and
   `cargo tree --locked`.
5. Inspect every new crate source, especially `build.rs`, procedural macros,
   native code, network calls, filesystem access, and environment reads.
6. Run the offline structural supply-chain contract.
7. Run cargo-deny against the current RustSec database.
8. Resolve cargo-vet failures through an audit, deliberately trusted import, or
   an exact-version exemption with a written rationale. Never describe an
   exemption as an audit.
9. Only then compile and run the normal formatter, tests, and Clippy gates.

Keep dependency updates isolated from product changes when practical. Commit
the resulting `Cargo.lock` and supply-chain ledger changes together.

