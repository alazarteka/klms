# Security

Report suspected vulnerabilities privately through this repository's GitHub
“Report a vulnerability” form (Security → Advisories). If that form is
unavailable, contact the repository owner privately;
do not post sensitive details in a public issue.

Include the affected version, a concise impact description, and a synthetic
or redacted reproduction. Never send KLMS cookies, passwords, student data,
grades, attendance records, or HTML captured from a real account.

Run `make supply-chain` to check dependencies with pinned, checksum-verified
cargo-deny and cargo-vet. CI and release builds run both before compilation.
The checks require network access for tool downloads and advisory updates.

Advisory scans cannot detect every vulnerability or compromised release.
Cargo-vet exemptions are recorded trust debt, not source audits; pending owner
reviews and audit scope are described in `supply-chain/BASELINE.md`.
