#!/usr/bin/env python3
# /// script
# requires-python = ">=3.10"
# dependencies = []
# ///
"""Check a packaged release and its installation offline, outside the checkout."""

import hashlib
import json
import os
from pathlib import Path
import subprocess
import sys
import tarfile
import tempfile


def main():
    if len(sys.argv) != 2:
        raise SystemExit("usage: release_smoke.py RELEASE.tar.gz")
    archive = Path(sys.argv[1]).resolve()
    checksum = Path(str(archive) + ".sha256").read_text().split()
    if len(checksum) != 2 or checksum[1].lstrip("*") != archive.name:
        raise SystemExit("invalid archive checksum file")
    if hashlib.sha256(archive.read_bytes()).hexdigest() != checksum[0]:
        raise SystemExit("archive checksum mismatch")
    with tempfile.TemporaryDirectory(prefix="klms-release-smoke-") as directory:
        root = Path(directory)
        candidate = root / "candidate"
        member_name = archive.name.removesuffix(".tar.gz") + "/klms"
        with tarfile.open(archive) as package:
            members = [member for member in package if member.name == member_name]
            if len(members) != 1 or not members[0].isfile() or members[0].size > 128 * 1024 * 1024:
                raise SystemExit("archive must contain one regular klms executable")
            candidate.write_bytes(package.extractfile(members[0]).read())
        candidate.chmod(0o755)
        # Isolate the child application's user directories; never touch the real install.
        env = dict(os.environ, HOME=str(root / "home"), XDG_DATA_HOME=str(root / "data"), XDG_STATE_HOME=str(root / "state"))

        def invoke(binary, *args, success=True):
            result = subprocess.run([str(binary), *args], cwd=root, env=env, capture_output=True, text=True, timeout=30)
            if (result.returncode == 0) != success:
                raise RuntimeError(f"{args}: exit {result.returncode}: {result.stderr}")
            return result

        def structured(binary, *args, success=True):
            result = invoke(binary, "--json", *args, success=success)
            stream, other = (result.stdout, result.stderr) if success else (result.stderr, result.stdout)
            if other:
                raise RuntimeError(f"unexpected output stream: {other}")
            value = json.loads(stream)
            if value["ok"] != success:
                raise RuntimeError("JSON success disagrees with exit status")
            return value

        version = structured(candidate, "--version")["data"]["version"]
        if f"klms {version}" != invoke(candidate, "--version").stdout.strip():
            raise RuntimeError("human and JSON versions disagree")
        if not archive.name.startswith(f"klms-v{version}-"):
            raise RuntimeError("archive name and executable version disagree")
        structured(candidate, "--help")
        structured(candidate, "upgrade", "--help")
        spec = structured(candidate, "spec")["data"]
        paths = [command["path"] for command in spec["commands"]]
        if ["update"] not in paths or ["__install"] in paths:
            raise RuntimeError("incorrect public command discovery")
        structured(candidate, "update", "--bogus", success=False)
        installed = root / "bin directory" / "klms"
        structured(candidate, "__install", "--destination", str(installed))
        if structured(installed, "--version")["data"]["version"] != version:
            raise RuntimeError("installed executable version mismatch")
        status = structured(installed, "skill", "status")["data"]
        if not status["payload_current"] or not status["link_current"]:
            raise RuntimeError("matching companion skill was not installed")
        structured(candidate, "__install", "--destination", str(installed))
        if (root / "state" / "klms" / "session.json").exists():
            raise RuntimeError("offline installation unexpectedly created authentication state")
    print(f"Release smoke passed: klms {version}; checksum, discovery, errors, installation and skill")


if __name__ == "__main__":
    main()
