#!/usr/bin/env python3
"""Offline malformed-archive tests for the cargo-vet bootstrap."""

from __future__ import annotations

import io
import stat
import sys
import tarfile
import tempfile
import unittest
from pathlib import Path


ROOT = "cargo-vet-test-target"
EXECUTABLE = "cargo-vet"
EXPECTED = (
    f"{ROOT}/",
    f"{ROOT}/LICENSE-APACHE",
    f"{ROOT}/LICENSE-MIT",
    f"{ROOT}/README.md",
    f"{ROOT}/{EXECUTABLE}",
)
sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "scripts"))
from safe_tool_archive import copy_executable, validated_members  # noqa: E402


def make_archive(path: Path, members: tuple[str, ...], member_type: bytes | None = None) -> None:
    with tarfile.open(path, "w:xz") as archive:
        for member in members:
            info = tarfile.TarInfo(member)
            if member.endswith("/"):
                info.type = tarfile.DIRTYPE
                archive.addfile(info)
            elif member_type is not None and member == f"{ROOT}/{EXECUTABLE}":
                info.type = member_type
                info.linkname = f"{ROOT}/README.md"
                archive.addfile(info)
            else:
                info.size = 4 if member == f"{ROOT}/{EXECUTABLE}" else 0
                archive.addfile(info, io.BytesIO(b"tool" if info.size else b""))


class CargoVetArchiveContract(unittest.TestCase):
    def verify(self, members: tuple[str, ...], member_type: bytes | None = None) -> bool:
        with tempfile.TemporaryDirectory() as directory:
            archive = Path(directory) / "fixture.tar.xz"
            make_archive(archive, members, member_type)
            try:
                with tarfile.open(archive, "r:xz") as opened:
                    validated_members(opened, ROOT, EXECUTABLE)
                return True
            except ValueError:
                return False

    def test_exact_member_set_passes(self) -> None:
        self.assertTrue(self.verify(EXPECTED))

    def test_missing_extra_duplicate_and_traversal_members_fail(self) -> None:
        for members in (
            EXPECTED[:-1],
            (*EXPECTED, f"{ROOT}/surprise"),
            (*EXPECTED, EXPECTED[-1]),
            (*EXPECTED, f"{ROOT}/../escape"),
        ):
            with self.subTest(members=members):
                self.assertFalse(self.verify(members))

    def test_links_fail(self) -> None:
        self.assertFalse(self.verify(EXPECTED, tarfile.SYMTYPE))
        self.assertFalse(self.verify(EXPECTED, tarfile.LNKTYPE))

    def test_stream_copy_creates_private_regular_executable(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            archive = root / "fixture.tar.xz"
            output = root / EXECUTABLE
            make_archive(archive, EXPECTED)
            copy_executable(archive, ROOT, EXECUTABLE, output)
            metadata = output.lstat()
            self.assertTrue(stat.S_ISREG(metadata.st_mode))
            self.assertEqual(stat.S_IMODE(metadata.st_mode), 0o700)
            self.assertEqual(metadata.st_nlink, 1)
            self.assertEqual(output.read_bytes(), b"tool")


if __name__ == "__main__":
    unittest.main()
