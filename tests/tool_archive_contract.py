#!/usr/bin/env python3
"""Both bootstrap archive formats enforce the same extraction contract."""

from __future__ import annotations

import io
import stat
import sys
import tarfile
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "scripts"))
from safe_tool_archive import copy_executable, validated_members  # noqa: E402

FORMATS = (("cargo-deny", "gz"), ("cargo-vet", "xz"))


def make_archive(path, compression, root, executable, members, member_type=None):
    with tarfile.open(path, f"w:{compression}") as archive:
        for name in members:
            info = tarfile.TarInfo(name)
            payload = b"tool" if name == f"{root}/{executable}" else b""
            if name.endswith("/"):
                info.type = tarfile.DIRTYPE
                payload = b""
            elif name == f"{root}/{executable}" and member_type is not None:
                info.type = member_type
                info.linkname = f"{root}/README.md"
                payload = b""
            info.size = len(payload)
            archive.addfile(info, io.BytesIO(payload))


class ToolArchiveContract(unittest.TestCase):
    def test_valid_archive_copies_private_regular_executable(self):
        for executable, compression in FORMATS:
            with self.subTest(executable=executable), tempfile.TemporaryDirectory() as directory:
                root = f"{executable}-test-target"
                members = tuple(f"{root}/{name}" for name in ("", "LICENSE-APACHE", "LICENSE-MIT", "README.md", executable))
                archive = Path(directory) / f"fixture.tar.{compression}"
                output = Path(directory) / executable
                make_archive(archive, compression, root, executable, members)
                copy_executable(archive, root, executable, output)
                metadata = output.lstat()
                self.assertTrue(stat.S_ISREG(metadata.st_mode))
                self.assertEqual(stat.S_IMODE(metadata.st_mode), 0o700)
                self.assertEqual(metadata.st_nlink, 1)
                self.assertEqual(output.read_bytes(), b"tool")

    def test_missing_extra_duplicate_traversal_and_absolute_members_fail(self):
        for executable, compression in FORMATS:
            root = f"{executable}-test-target"
            members = tuple(f"{root}/{name}" for name in ("", "LICENSE-APACHE", "LICENSE-MIT", "README.md", executable))
            for invalid in (
                members[:-1],
                (*members, f"{root}/surprise"),
                (*members, members[-1]),
                (*members, f"{root}/../escape"),
                (*members, "/absolute/escape"),
            ):
                with self.subTest(executable=executable, members=invalid), tempfile.TemporaryDirectory() as directory:
                    archive = Path(directory) / f"fixture.tar.{compression}"
                    make_archive(archive, compression, root, executable, invalid)
                    with tarfile.open(archive) as opened, self.assertRaises(ValueError):
                        validated_members(opened, root, executable)

    def test_symlink_and_hardlink_executables_fail(self):
        for executable, compression in FORMATS:
            root = f"{executable}-test-target"
            members = tuple(f"{root}/{name}" for name in ("", "LICENSE-APACHE", "LICENSE-MIT", "README.md", executable))
            for member_type in (tarfile.SYMTYPE, tarfile.LNKTYPE):
                with self.subTest(executable=executable, member_type=member_type), tempfile.TemporaryDirectory() as directory:
                    archive = Path(directory) / f"fixture.tar.{compression}"
                    make_archive(archive, compression, root, executable, members, member_type)
                    with tarfile.open(archive) as opened, self.assertRaises(ValueError):
                        validated_members(opened, root, executable)


if __name__ == "__main__":
    unittest.main()
