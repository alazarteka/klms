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

Dependency policy is evaluated before any project build, test, procedural
macro, or dependency build script executes in CI:

```bash
./tests/supply_chain_contract.sh
./scripts/cargo-deny.sh --log-level info --locked check advisories bans sources licenses
./scripts/cargo-vet.sh check --locked --no-registry-suggestions
```

`cargo-deny` rejects RustSec advisories, malware, yanked dependencies, unknown
registries, Git dependencies, wildcard requirements, and unapproved licenses.
`deny.toml` independently blocks the malicious crates and exact compromised
versions identified in the 2026-08-20 Rust supply-chain incident.

`cargo-vet` records exact-version trust decisions and prevents silent graph
drift. An exemption is review debt, not proof that code is safe. New or updated
third-party code requires an explicit audit, trusted import, or documented
reviewed exemption in the same change.

All external GitHub Actions are pinned to full commit SHAs. Workflows use
read-only permissions by default, do not persist checkout credentials, and run
the dependency gates before the first Cargo compilation command.

Neither a clean advisory scan nor a cargo-vet ledger proves that unknown code
is benign. Review build scripts, procedural macros, publisher changes, unusual
file access, network behavior, and newly introduced native code manually.

The HTML selector stack includes MPL-2.0 dependencies. Linking and ordinary
use are permitted; copying or modifying MPL-covered source requires preserving
the license obligations for the affected files.

See [docs/DEPENDENCY_UPDATES.md](docs/DEPENDENCY_UPDATES.md) for the required
update sequence.
