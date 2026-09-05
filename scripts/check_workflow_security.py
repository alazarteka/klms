"""Check this repository's explicit, per-job dependency gates in parsed YAML.

Gates must be unconditional, standalone run steps in each Cargo job. This is
not a shell interpreter; shell scripts remain reviewable code. Run through
tests/supply_chain_contract.sh for the locked Python environment.
"""

import re
import shlex
from pathlib import Path

import yaml

FULL_SHA = re.compile(r"[0-9a-f]{40}")
GATES = (
    "./tests/supply_chain_contract.sh",
    "./scripts/cargo-deny.sh",
    "./scripts/cargo-vet.sh",
)
CARGO_BUILD = {"build", "check", "clippy", "run", "test"}


def check_workflow(text: str) -> None:
    workflow = yaml.safe_load(text)
    if not isinstance(workflow, dict) or workflow.get("permissions") != {"contents": "read"}:
        raise ValueError("workflow must default to read-only contents permission")
    jobs = workflow.get("jobs")
    if not isinstance(jobs, dict) or not jobs:
        raise ValueError("workflow must contain jobs")
    for name, job in jobs.items():
        gates = []
        for step in job.get("steps", []):
            action = step.get("uses")
            if action and not action.startswith("./"):
                if "@" not in action or not FULL_SHA.fullmatch(action.rsplit("@", 1)[1]):
                    raise ValueError(f"{name}: action must be pinned by full SHA: {action}")
            run = step.get("run", "")
            # Discard comments before examining executable commands.
            commands = [shlex.split(line, comments=True) for line in run.splitlines()]
            commands = [words for words in commands if words]
            if len(commands) == 1 and commands[0][0] in GATES:
                if "if" in step or step.get("continue-on-error", False):
                    raise ValueError(f"{name}: dependency gate must always succeed")
                if any(token in run for token in (";", "|", "&", chr(96), "$", "\\")):
                    raise ValueError(f"{name}: dependency gate must be a standalone invocation")
                words = commands[0]
                if words[0] != GATES[0] and (
                    "check" not in words
                    or any(flag in words for flag in ("--help", "--version", "-h", "-V"))
                ):
                    raise ValueError(f"{name}: dependency gate must run a check")
                gates.append(commands[0][0])
            builds = any(
                word == "cargo" and words[index + 1] in CARGO_BUILD
                for words in commands
                for index, word in enumerate(words[:-1])
            )
            if builds and gates != list(GATES):
                raise ValueError(f"{name}: Cargo requires the three gates in order in this job")


def check_repository(root: Path) -> None:
    paths = sorted((root / ".github/workflows").glob("*.y*ml"))
    if not paths:
        raise ValueError("no workflows found")
    for path in paths:
        try:
            check_workflow(path.read_text())
        except (ValueError, yaml.YAMLError) as error:
            raise ValueError(f"{path.name}: {error}") from error
