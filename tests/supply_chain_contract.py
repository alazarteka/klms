# /// script
# requires-python = ">=3.11"
# dependencies = ["PyYAML==6.0.3"]
# ///
"""Check policy values and run offline behavioral supply-chain tests."""

import subprocess
import sys
import tomllib
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / "scripts"))
from check_workflow_security import check_repository


def check_policy(root: Path) -> None:
    with (root / "deny.toml").open("rb") as source:
        deny = tomllib.load(source)
    if deny["sources"] != {
        "unknown-registry": "deny",
        "unknown-git": "deny",
        "allow-registry": ["https://github.com/rust-lang/crates.io-index"],
        "allow-git": [],
    }:
        raise ValueError("dependency sources must be restricted to crates.io")
    with (root / "supply-chain/config.toml").open("rb") as source:
        vet = tomllib.load(source)
    policy = vet.get("policy", {}).get("klms", {})
    if policy.get("criteria") != "safe-to-run" or policy.get("dev-criteria") != "safe-to-run":
        raise ValueError("klms must require safe-to-run dependency reviews")
    # Zero exemptions is a valid, fully reviewed state. cargo-vet validates
    # remaining exemptions; BASELINE.md records human trust decisions.
    with (root / "Cargo.lock").open("rb") as source:
        packages = tomllib.load(source)["package"]
    banned = {entry["crate"] for entry in deny["bans"]["deny"]}
    for package in packages:
        name = package["name"]
        if name in banned or f"{name}@{package['version']}" in banned:
            raise ValueError(f"locked package is explicitly banned: {name}")


if __name__ == "__main__":
    check_repository(ROOT)
    check_policy(ROOT)
    for script in ("install.sh", "cargo-deny.sh", "cargo-vet.sh"):
        subprocess.run(["bash", "-n", str(ROOT / "scripts" / script)], check=True)
    suite = unittest.defaultTestLoader.discover(str(ROOT / "tests"), pattern="*_contract.py")
    result = unittest.TextTestRunner(verbosity=1).run(suite)
    if not result.wasSuccessful():
        raise SystemExit(1)
    print("Supply-chain policy and behavioral checks passed")
