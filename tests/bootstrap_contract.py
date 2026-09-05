#!/usr/bin/env python3
"""Exercise checksum rejection through the real bootstrap entrypoints, offline."""

from __future__ import annotations

import os
import subprocess
import sys
import tempfile
import textwrap
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]

# Serve a structurally valid archive containing a marker-writing executable.
# Its digest cannot match the pinned official archive. Nothing mocks hashing,
# extraction, cache promotion, or execution by the real bootstrap scripts.
FAKE_CURL = r'''
import io
import os
import sys
import tarfile
from pathlib import Path

args = sys.argv[1:]
output = Path(args[args.index("--output") + 1])
url = args[-1]
Path(os.environ["REQUESTED_URL"]).write_text(url)
name = url.rsplit("/", 1)[-1]
compression = "gz" if name.endswith(".tar.gz") else "xz"
root = name.removesuffix(f".tar.{compression}")
executable = "cargo-deny" if root.startswith("cargo-deny-") else "cargo-vet"
with tarfile.open(output, f"w:{compression}") as archive:
    for member in ("", "LICENSE-APACHE", "LICENSE-MIT", "README.md", executable):
        info = tarfile.TarInfo(f"{root}/{member}")
        payload = b'#!/bin/sh\nprintf executed > "$EXECUTED_MARKER"\n' if member == executable else b""
        if not member:
            info.type = tarfile.DIRTYPE
        info.size = len(payload)
        info.mode = 0o755
        archive.addfile(info, io.BytesIO(payload))
'''


class BootstrapContract(unittest.TestCase):
    def test_untrusted_download_is_never_executed_or_promoted(self):
        for tool in ("cargo-deny", "cargo-vet"):
            with self.subTest(tool=tool), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                bin_dir = root / "bin"
                bin_dir.mkdir()
                curl = bin_dir / "curl"
                curl.write_text(f"#!{sys.executable}\n" + textwrap.dedent(FAKE_CURL))
                curl.chmod(0o755)
                cache = root / "cache"
                requested = root / "requested-url"
                executed = root / "executed"
                env = {
                    **os.environ,
                    "PATH": f"{bin_dir}{os.pathsep}{os.environ['PATH']}",
                    "CARGO_DENY_CACHE_DIR": str(cache),
                    "CARGO_VET_CACHE_DIR": str(cache),
                    "REQUESTED_URL": str(requested),
                    "EXECUTED_MARKER": str(executed),
                }
                result = subprocess.run(
                    [str(ROOT / "scripts" / f"{tool}.sh"), "--version"],
                    cwd=root, env=env, capture_output=True, text=True, timeout=20,
                )
                self.assertTrue(requested.exists(), result.stderr)
                self.assertNotEqual(result.returncode, 0, result.stdout)
                self.assertFalse(executed.exists(), "unverified executable ran")
                self.assertEqual(list(cache.iterdir()), [], "unverified download was cached or retained")


if __name__ == "__main__":
    unittest.main()
