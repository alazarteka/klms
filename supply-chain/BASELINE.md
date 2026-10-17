# Cargo-vet baseline

`safe-to-run` is required for normal and development dependencies, including
build scripts and procedural macros. It covers unexpected build/runtime effects,
not correctness, memory safety, or independent cryptographic validation.

The store retains 139 exact-version exemptions from the existing baseline and
2 older clap/clap_builder exemptions that anchor the retained delta audits.
The 26 audit records retain their original authors and scope. Exemptions are
trust debt, not audits. Do not advance or add one without recording the decision
here; prefer a full or delta audit and remove unused exemptions.

Run `scripts/supply-chain.sh deny` and `scripts/supply-chain.sh vet` before
compiling a changed dependency graph. Both tools are pinned and checksum-verified.
`make supply-chain` runs both; `make check` remains offline-capable.

## Pending owner review

The following 19 exact versions are temporary exemptions, not certifications.
Owner review is outstanding: inspect the source at the locked checksum, enabled
features, build scripts, process/filesystem/network effects, and upstream origin;
then replace each exemption with an audit or record an explicit retention decision.
“Shipping” covers Apple ARM64 and Linux musl, plus the Linux GNU CI host.
Android/JNI crates below are in the lockfile but are not compiled on those targets.

| Crate | Version | Why present / reachability | Review focus |
| --- | --- | --- | --- |
| ureq | 3.4.2 | Direct HTTP client; shipping | Transport, redirects, TLS features and side effects |
| ureq-proto | 0.6.4 | ureq protocol layer; shipping | HTTP parsing/serialization and side effects |
| base64 | 0.23.1 | ureq/ureq-proto encoding; shipping | Pure computation and enabled features |
| utf8-zero | 0.8.1 | ureq text handling; shipping | String conversion and side effects |
| rustls-platform-verifier | 0.7.1 | ureq platform-verifier; shipping | OS trust access and platform-specific behavior |
| rustls-platform-verifier-android | 0.2.0 | Verifier Android backend; not shipping | Bundled Android code and trust calls |
| jni | 0.22.4 | Verifier Android JNI; not shipping | Native calls and class loading |
| jni-macros | 0.22.4 | JNI proc macros; not shipping | Compile-time effects and generated code |
| jni-sys | 0.4.1 | JNI FFI; not shipping | Native bindings and build behavior |
| jni-sys-macros | 0.4.1 | JNI binding proc macros; not shipping | Compile-time effects and generated bindings |
| combine | 4.6.8 | JNI parser; not shipping | Parsing and enabled features |
| simd_cesu8 | 1.2.0 | JNI text conversion; not shipping | Build probes and SIMD conversion |
| simdutf8 | 0.1.5 | simd_cesu8 dependency; not shipping | SIMD dispatch and side effects |
| rustc_version | 0.4.1 | JNI macros/SIMD version probe; not shipping | Compiler execution and environment use |
| semver | 1.0.28 | rustc_version parsing; not shipping | Pure parsing and side effects |
| walkdir | 2.5.0 | JNI directory traversal; not shipping | Filesystem scope and symlink handling |
| same-file | 1.0.6 | walkdir file identity; not shipping | Filesystem access and platform calls |
| winapi-util | 0.1.11 | walkdir/same-file Windows branch; not shipping | Windows filesystem/handle operations |
| webpki-root-certs | 1.0.9 | Verifier wasm32 roots; not shipping | Root-certificate provenance and generation |

`rustls` is now pinned directly to select ring/std/tls12 for ureq;
it retains its existing exact-version exemption, so is not a new pending entry.
On 2026-10-17 the pin and exemption moved from 0.23.45 to 0.23.46 for
RUSTSEC-2026-0346, 0347, 0348 and 0349. This is a carry-over, not an audit:
the 0.23.45..0.23.46 source diff was not inspected. Reviewer: pending
`@alazarteka` (prepared by Claude Code).
The direct pin is not a new audit. ureq's `rustls-no-provider` feature avoids
its default webpki-roots bundle; shipping verification uses platform trust.
