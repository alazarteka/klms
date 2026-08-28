#!/usr/bin/env python3
"""Offline structural checks for GitHub workflow supply-chain ordering."""

from __future__ import annotations

import re
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
WORKFLOWS = ROOT / ".github" / "workflows"
FULL_SHA = re.compile(r"^[0-9a-f]{40}$")


def fail(message: str) -> None:
    raise SystemExit(f"workflow security contract: {message}")


def main() -> None:
    paths = sorted(WORKFLOWS.glob("*.yml")) + sorted(WORKFLOWS.glob("*.yaml"))
    if not paths:
        fail("no workflows found")

    for path in paths:
        text = path.read_text(encoding="utf-8")
        if "permissions:\n  contents: read" not in text:
            fail(f"{path.name} does not default to read-only contents permission")
        for line in text.splitlines():
            stripped = line.strip()
            if not stripped.startswith("uses:"):
                continue
            value = stripped.removeprefix("uses:").strip().split("#", 1)[0].strip()
            if value.startswith("./"):
                continue
            if "@" not in value:
                fail(f"{path.name} has an unpinned action: {value}")
            revision = value.rsplit("@", 1)[1]
            if not FULL_SHA.fullmatch(revision):
                fail(f"{path.name} action is not pinned by full SHA: {value}")

        cargo_positions = [
            index
            for index, line in enumerate(text.splitlines())
            if re.search(r"\brun:\s*(?:\|\s*)?cargo\s+(?:build|check|clippy|run|test)", line)
            or re.search(r"^\s*cargo\s+(?:build|check|clippy|run|test)", line)
        ]
        if cargo_positions:
            first_cargo = min(cargo_positions)
            required = {
                "static contract": "./tests/supply_chain_contract.sh",
                "cargo-deny": "./scripts/cargo-deny.sh",
                "cargo-vet": "./scripts/cargo-vet.sh",
            }
            positions = {}
            for label, needle in required.items():
                try:
                    positions[label] = next(
                        index for index, line in enumerate(text.splitlines()) if needle in line
                    )
                except StopIteration:
                    fail(f"{path.name} is missing {label}")
            if not (
                positions["static contract"]
                < positions["cargo-deny"]
                < positions["cargo-vet"]
                < first_cargo
            ):
                fail(f"{path.name} does not gate Cargo in the required order")

    print("workflow security contract passed")


if __name__ == "__main__":
    main()

