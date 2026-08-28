#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

fail() {
  echo "supply-chain contract: $*" >&2
  exit 1
}

PYTHONDONTWRITEBYTECODE=1 python3 scripts/check_workflow_security.py
PYTHONDONTWRITEBYTECODE=1 python3 tests/cargo_deny_archive_contract.py
PYTHONDONTWRITEBYTECODE=1 python3 tests/cargo_vet_archive_contract.py

for crate in proc-macro1 proc-macro-en aovine arone aronenao tinymember; do
  grep -Fq "crate = \"$crate\"" deny.toml || fail "deny.toml does not ban $crate"
done
for crate_version in arrayref@0.3.10 internment@0.8.7 append-only-vec@0.1.9; do
  grep -Fq "crate = \"$crate_version\"" deny.toml || \
    fail "deny.toml does not ban compromised release $crate_version"
done

grep -q '^unknown-registry = "deny"$' deny.toml || fail "unknown registries are not denied"
grep -q '^unknown-git = "deny"$' deny.toml || fail "Git dependencies are not denied"
grep -Fq 'allow-registry = ["https://github.com/rust-lang/crates.io-index"]' deny.toml || \
  fail "crates.io is not the sole allowed registry"

for wrapper in scripts/cargo-deny.sh scripts/cargo-vet.sh; do
  [[ -x "$wrapper" ]] || fail "$wrapper is not executable"
  grep -q 'expected_sha256="[0-9a-f]\{64\}"' "$wrapper" || \
    fail "$wrapper lacks pinned archive checksums"
  grep -q 'safe_tool_archive.py' "$wrapper" || fail "$wrapper bypasses the safe archive copier"
done

grep -q '^\[policy\.klms\]$' supply-chain/config.toml || fail "cargo-vet root policy is absent"
[[ "$(grep -c '^\[\[exemptions\.' supply-chain/config.toml)" -gt 0 ]] || \
  fail "cargo-vet baseline exemptions are not explicit"
grep -Fq 'explicit baseline trust debt' supply-chain/BASELINE.md || \
  fail "cargo-vet debt is undocumented"

python3 - <<'PY'
from pathlib import Path
import re

lock = Path("Cargo.lock").read_text(encoding="utf-8")
malicious = {
    "proc-macro1",
    "proc-macro-en",
    "aovine",
    "arone",
    "aronenao",
    "tinymember",
}
compromised = {
    ("arrayref", "0.3.10"),
    ("internment", "0.8.7"),
    ("append-only-vec", "0.1.9"),
}
for block in lock.split("[[package]]")[1:]:
    name = re.search(r'^name = "([^"]+)"$', block, re.MULTILINE)
    version = re.search(r'^version = "([^"]+)"$', block, re.MULTILINE)
    if not name or not version:
        continue
    pair = (name.group(1), version.group(1))
    if pair[0] in malicious or pair in compromised:
        raise SystemExit(f"locked malicious or compromised package: {pair[0]}@{pair[1]}")
print("locked graph malicious-package check passed")
PY

echo "supply-chain contract passed"

