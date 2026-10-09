# Security

## Reporting a vulnerability

Do not open a public issue containing a session cookie, owned session file,
student record, grade, attendance record, or reproducible credential leak.
Contact the repository owner privately with a redacted reproduction and the
affected version.

## Credential boundary

`klms` treats its owned session file as a secret. Diagnostics report only its
path, cookie/device counts, creation time, and whether a live read succeeded.
Human errors and JSON never include cookie values, `Cookie` or `Set-Cookie`
headers, URL userinfo, passwords, verification codes, encryption keys, Moodle
session keys, or complete authenticated HTML.

Authentication accepts secrets only through hidden terminal prompts. A
short-lived transport permits only the exact KLMS and KAIST SSO origins,
follows bounded redirects itself, and discards general SSO cookies at
completion. Only cookies issued by the KLMS host plus trusted-device
identifiers are written to `$XDG_STATE_HOME/klms/session.json` (or
`~/.local/state/klms/session.json`). The directory is mode 0700 and the file is
atomically installed at mode 0600 on Unix. Existing Playwright and kaist-cli
state files are ignored and are never deleted automatically.

Non-loopback service URLs must use HTTPS. Cleartext HTTP exists only for
fixture-backed loopback integration tests.

## Dependency policy

Dependencies are pinned to exact versions and `Cargo.lock` is committed. CI
runs `cargo-deny` before compiling anything:

```bash
./scripts/cargo-deny.sh check advisories bans sources licenses
```

The script downloads a pinned `cargo-deny` release and verifies its SHA-256
before extracting or running it. `deny.toml` rejects RustSec advisories,
yanked crates, unknown registries, repository-hosted dependencies, wildcard
requirements, and unapproved licenses, and blocks the malicious crates and
compromised versions from the 2026-08-20 Rust supply-chain incident.

All external workflow actions are pinned to full commit SHAs, and workflows use
read-only permissions and do not persist checkout credentials. Dependabot
proposes updates; review build scripts, procedural macros, and publisher
changes by hand, since a clean advisory scan does not prove code is benign.

The HTML selector stack includes MPL-2.0 dependencies. Linking and ordinary
use are permitted; copying or modifying MPL-covered source requires preserving
the license obligations for the affected files.
