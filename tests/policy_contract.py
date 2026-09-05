"""Policy tests use parsed values, not TOML layout or baseline prose."""

import tempfile
import unittest
from pathlib import Path

from supply_chain_contract import check_policy


class PolicyContract(unittest.TestCase):
    def test_zero_exemptions_is_valid_and_banned_packages_are_rejected(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "supply-chain").mkdir()
            (root / "supply-chain/config.toml").write_text(
                '[policy.klms]\ncriteria="safe-to-run"\ndev-criteria="safe-to-run"\n'
            )
            (root / "deny.toml").write_text(
                '[sources]\nunknown-registry="deny"\nunknown-git="deny"\n'
                'allow-registry=["https://github.com/rust-lang/crates.io-index"]\n'
                'allow-git=[]\n[bans]\ndeny=[{crate="bad@1.0.0"}]\n'
            )
            lock = root / "Cargo.lock"
            lock.write_text('[[package]]\nname="bad"\nversion="1.0.1"\n')
            check_policy(root)
            lock.write_text('[[package]]\nname="bad"\nversion="1.0.0"\n')
            with self.assertRaisesRegex(ValueError, "explicitly banned"):
                check_policy(root)
