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
