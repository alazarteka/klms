#!/usr/bin/env python3
"""Validate a pinned tool archive and stream-copy one executable safely."""

from __future__ import annotations

import argparse
import os
import shutil
import stat
import tarfile
from pathlib import Path


def expected_names(root: str, executable: str) -> list[str]:
    return sorted(
        (
            root,
            f"{root}/LICENSE-APACHE",
            f"{root}/LICENSE-MIT",
            f"{root}/README.md",
            f"{root}/{executable}",
        )
    )


def validated_members(
    archive: tarfile.TarFile, root: str, executable: str
) -> dict[str, tarfile.TarInfo]:
    members = archive.getmembers()
    names = sorted(member.name for member in members)
    if names != expected_names(root, executable):
        raise ValueError("archive does not contain the exact expected five-member set")
    by_name = {member.name: member for member in members}
    if len(by_name) != len(members):
        raise ValueError("archive contains duplicate member names")
    if not by_name[root].isdir():
        raise ValueError("archive root is not a directory")
    for name in expected_names(root, executable):
        if name != root and not by_name[name].isreg():
            raise ValueError(f"archive member is not a regular file: {name}")
    return by_name


def copy_executable(
    archive_path: Path, root: str, executable: str, destination: Path
) -> None:
    with tarfile.open(archive_path, "r:*") as archive:
        members = validated_members(archive, root, executable)
        source = archive.extractfile(members[f"{root}/{executable}"])
        if source is None:
            raise ValueError(f"regular {executable} member could not be read")
        flags = os.O_WRONLY | os.O_CREAT | os.O_EXCL
        if hasattr(os, "O_NOFOLLOW"):
            flags |= os.O_NOFOLLOW
        descriptor = os.open(destination, flags, 0o600)
        try:
            with os.fdopen(descriptor, "wb", closefd=False) as output:
                shutil.copyfileobj(source, output)
                output.flush()
            opened = os.fstat(descriptor)
            if not stat.S_ISREG(opened.st_mode) or opened.st_nlink != 1:
                raise ValueError(f"copied {executable} descriptor is not a private ordinary file")
            os.fchmod(descriptor, 0o700)
        finally:
            os.close(descriptor)
            source.close()

    metadata = os.lstat(destination)
    if (
        not stat.S_ISREG(metadata.st_mode)
        or metadata.st_nlink != 1
        or metadata.st_dev != opened.st_dev
        or metadata.st_ino != opened.st_ino
    ):
        raise ValueError(f"{executable} path no longer names the validated ordinary file")


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("archive", type=Path)
    parser.add_argument("expected_root")
    parser.add_argument("executable")
    parser.add_argument("destination", type=Path)
    args = parser.parse_args()
    copy_executable(args.archive, args.expected_root, args.executable, args.destination)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
