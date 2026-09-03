# Cargo-vet baseline

This store was initialized for `klms` on 2026-08-28 by `@alazarteka` from the
already reviewed `mmai-cli` dependency baseline. The 196 exact-version exemptions
in `config.toml` are explicit baseline trust debt. They are not audits and do
not claim that those crates were inspected or proven benign.

The project requires `safe-to-run` for all third-party dependencies reachable
while building, testing, or running the CLI. This includes ordinary libraries,
build scripts, build dependencies, and procedural macros. Cargo-vet's
`safe-to-run` criterion is about avoiding surprising workstation or automation
effects; it does not establish production correctness.

The baseline has no expiry because removing it all at once would require 197
real audits. Its follow-up is a ratchet: never extend it silently. Prefer a
full or delta certification for each changed crate and shrink exemptions over
time. A new exemption must identify its rationale, reviewer, decision date,
and either an expiry or a concrete audit follow-up in the dependency PR.

No third-party audit set is imported. Adding one is a separate trust decision:
document why its auditors and criteria are accepted and commit the resulting
`imports.lock` update produced by the pinned cargo-vet version.

## 2026-08-28 lockfile reconciliation

Reviewer: `@alazarteka` (recorded by Codex).

The initial `klms` lockfile resolved 20 patch releases newer than the inherited
`mmai-cli` baseline: `cc` 1.4.2, `displaydoc` 0.2.7, `fastrand` 2.5.0,
`find-msvc-tools` 0.1.10, the six `futures-*` crates at 0.3.34, `http` 1.5.0,
`ipnet` 2.12.1, `js-sys` 0.3.104, `schannel` 0.1.29, the four
`wasm-bindgen*` crates at 0.2.127, `wasm-bindgen-futures` 0.4.77, and `web-sys`
0.3.104. Their exemptions were advanced to match the exact locked versions so
the gate records, rather than hides, this trust decision. This is not a source
audit. Before the first public release, replace these exemptions with delta or
full audits, or document why a release must retain each one.

## 2026-09-01 native-auth dependency review

Reviewer: `@alazarteka` (review recorded by Codex).

Native KAIST SSO adds the small RustCrypto SEED/CBC stack and `rpassword` for
hidden terminal input. The exact source archives for the eleven newly reachable
crates were reviewed for the `safe-to-run` criterion and recorded as audits,
not exemptions. The cryptographic libraries have no external side effects;
`generic-array` has a build script limited to a rustc version probe and cfg
output; and `rpassword`/`rtoolbox` perform their documented local TTY access.
This review establishes build/runtime side-effect safety only, not a claim that
the SEED implementation is independently cryptographically certified.

## 2026-09-02 local-library dependency review

Reviewer: Codex agent for task `01a05f8d-b5ae-70d0-bdf9-6598dca0ff11`.

The Phase 1 local-library foundation adds exact `rusqlite` 0.40.2 with default
features disabled and bundled SQLite. The six newly reachable crates were
reviewed at their exact locked versions and recorded as `safe-to-run` audits,
not exemptions:

- `fallible-iterator` 0.3.0 and `fallible-streaming-iterator` 0.1.9 are
  `no_std`/in-memory iterator traits and adapters with no process, filesystem,
  or network effects;
- `rusqlite` 0.40.2 has no build script and performs only caller-directed
  SQLite operations; its optional extension, virtual-table, and serialization
  features are not enabled by this graph;
- `libsqlite3-sys` 0.38.2 selects its bundled branch, copies checked-in
  bindings to Cargo's `OUT_DIR`, and asks the already-baselined `cc` crate to
  compile the included SQLite amalgamation. It performs no downloads or writes
  outside Cargo build output. Build-time bindgen, SQLCipher, and loadable
  extension paths are not enabled;
- `pkg-config` 0.3.34 may execute the explicitly configured/system
  `pkg-config` probe and emits Cargo link metadata, with no network access or
  unrelated writes. The bundled SQLite branch does not invoke that probe;
- `vcpkg` 0.2.15 reads configured vcpkg metadata and can optionally copy
  selected DLLs into `OUT_DIR`; it does not install packages or invoke the
  vcpkg tool. The active bundled non-Windows path does not use it.

This review establishes side-effect safety for building and running the locked
graph. It is not a correctness or memory-safety audit of SQLite, its C
amalgamation, or the Rust FFI wrapper.

## 2026-09-02 SHA-256 dependency review

Reviewer: Codex agent for task `01a05f8d-b5ae-70d0-bdf9-6598dca0ff11`.

The CAS implementation adds exact `sha2` 0.11.0. Its six newly reachable exact
versions (`sha2`, `digest`, `block-buffer`, `crypto-common`, `hybrid-array`, and
`const-oid`) were reviewed and recorded as `safe-to-run` audits. They have no
build scripts, are pure Rust/no-std computation and type-support crates, and
perform no filesystem, process, or network operations. This is a side-effect
safety review, not independent cryptographic validation of SHA-256.

## 2026-09-02 shell-completion dependency review

Reviewer: Claude Fable 5.1 (review recorded for `@alazarteka`).

`klms completions SHELL` adds exact `clap_complete` 4.6.9, the static
completion generator maintained in the clap workspace. It is the only new
crate in the lockfile: its `clap_lex`, `shlex`, `is_executable`, and
`completest` dependencies are optional and disabled because the
`unstable-dynamic` and `unstable-shell-tests` features are off. The crate has
no build script. The modules that read `SHELL`, the current directory, or the
filesystem are behind `unstable-dynamic`; the static path writes only to the
writer or caller-supplied path it is given. Recorded as a `safe-to-run` audit,
not an exemption. This is a side-effect review, not a review of the generated
scripts' behavior in every shell.
