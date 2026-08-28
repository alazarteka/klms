#!/usr/bin/env python3
"""Offline malformed-archive tests for the cargo-deny bootstrap."""

from __future__ import annotations

import io
import os
import stat
import sys
import tarfile
import tempfile
import unittest
from pathlib import Path


ROOT = "cargo-deny-0.20.2-test-target"
EXPECTED = (
    f"{ROOT}/",
    f"{ROOT}/LICENSE-APACHE",
    f"{ROOT}/LICENSE-MIT",
    f"{ROOT}/README.md",
    f"{ROOT}/cargo-deny",
)
sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "scripts"))
from safe_tool_archive import copy_executable as safe_copy_executable  # noqa: E402
from safe_tool_archive import validated_members as safe_validated_members  # noqa: E402


def validated_members(archive: tarfile.TarFile, root: str):
    return safe_validated_members(archive, root, "cargo-deny")


def copy_executable(archive_path: Path, root: str, destination: Path) -> None:
    safe_copy_executable(archive_path, root, "cargo-deny", destination)


def make_archive(path: Path, members: tuple[str, ...], member_type: bytes | None = None) -> None:
    with tarfile.open(path, "w:gz") as archive:
        for member in members:
            info = tarfile.TarInfo(member)
            if member.endswith("/"):
                info.type = tarfile.DIRTYPE
                archive.addfile(info)
            else:
                if member_type is not None and member == f"{ROOT}/cargo-deny":
                    info.type = member_type
                    info.linkname = f"{ROOT}/README.md"
                    archive.addfile(info)
                else:
                    info.size = 4 if member == f"{ROOT}/cargo-deny" else 0
                    archive.addfile(info, io.BytesIO(b"tool" if info.size else b""))


class CargoDenyArchiveContract(unittest.TestCase):
    def verify(self, members: tuple[str, ...], member_type: bytes | None = None) -> bool:
        with tempfile.TemporaryDirectory() as directory:
            archive = Path(directory) / "fixture.tar.gz"
            make_archive(archive, members, member_type)
            try:
                with tarfile.open(archive, "r:gz") as opened:
                    validated_members(opened, ROOT)
                return True
            except ValueError:
                return False

    def test_exact_member_set_passes(self) -> None:
        self.assertTrue(self.verify(EXPECTED))

    def test_missing_member_fails(self) -> None:
        self.assertFalse(self.verify(EXPECTED[:-1]))

    def test_unexpected_member_fails(self) -> None:
        self.assertFalse(self.verify((*EXPECTED, f"{ROOT}/surprise")))

    def test_duplicate_member_fails(self) -> None:
        self.assertFalse(self.verify((*EXPECTED, EXPECTED[-1])))

    def test_traversal_member_fails(self) -> None:
        self.assertFalse(self.verify((*EXPECTED, f"{ROOT}/../escape")))

    def test_symlink_executable_fails(self) -> None:
        self.assertFalse(self.verify(EXPECTED, tarfile.SYMTYPE))

    def test_hardlink_executable_fails(self) -> None:
        self.assertFalse(self.verify(EXPECTED, tarfile.LNKTYPE))

    def test_stream_copy_creates_private_regular_executable(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            archive = root / "fixture.tar.gz"
            output = root / "cargo-deny"
            make_archive(archive, EXPECTED)
            copy_executable(archive, ROOT, output)
            metadata = output.lstat()
            self.assertTrue(stat.S_ISREG(metadata.st_mode))
            self.assertEqual(metadata.st_nlink, 1)
            self.assertEqual(output.read_bytes(), b"tool")

    @unittest.skipUnless(sys.platform.startswith("linux"), "Linux capability contract")
    def test_linux_fd_validation_and_extraction_path(self) -> None:
        self.assertTrue(hasattr(os, "O_NOFOLLOW"))
        self.assertTrue(hasattr(os, "fchmod"))
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            archive = root / "fixture.tar.gz"
            output = root / "cargo-deny"
            make_archive(archive, EXPECTED)
            copy_executable(archive, ROOT, output)
            self.assertEqual(stat.S_IMODE(output.lstat().st_mode), 0o700)


if __name__ == "__main__":
    unittest.main()
