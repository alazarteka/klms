"""Behavioral fixtures for the supported workflow gate policy."""

import textwrap
import unittest

from check_workflow_security import check_workflow


def workflow(steps: str, extra_jobs: str = "") -> str:
    return "permissions: {contents: read}\njobs:\n  build:\n    steps:\n" + textwrap.indent(steps, "      ") + extra_jobs


GATES = """- run: ./tests/supply_chain_contract.sh
- run: ./scripts/cargo-deny.sh check
- run: ./scripts/cargo-vet.sh check --locked --no-registry-suggestions
"""


class WorkflowPolicy(unittest.TestCase):
    def test_flow_and_block_yaml_allow_pinned_actions_and_ordered_gates(self):
        check_workflow(workflow('- uses: "actions/checkout@' + "a" * 40 + '"\n' + GATES + "- run: cargo test --locked\n"))
        check_workflow(workflow(GATES + "- run: |\n    # compile the tests\n    cargo test --locked\n"))

    def test_unpinned_action_fails_in_either_step_spelling(self):
        for action in ("- uses: actions/checkout@main\n", "- name: Checkout\n  uses: actions/checkout@v7\n"):
            with self.subTest(action=action), self.assertRaisesRegex(ValueError, "full SHA"):
                check_workflow(workflow(action))

    def test_comment_only_gates_do_not_protect_build(self):
        steps = "- run: |\n    # ./tests/supply_chain_contract.sh\n    # ./scripts/cargo-deny.sh\n    # ./scripts/cargo-vet.sh\n    cargo test\n"
        with self.assertRaisesRegex(ValueError, "requires the three gates"):
            check_workflow(workflow(steps))

    def test_another_job_cannot_supply_gates(self):
        other = "  unrelated:\n    steps:\n" + textwrap.indent(GATES, "      ")
        with self.assertRaisesRegex(ValueError, "requires the three gates"):
            check_workflow(workflow("- run: cargo test\n", other))

    def test_skippable_or_masked_gate_fails(self):
        for gate in (
            "- if: false\n  run: ./scripts/cargo-vet.sh check\n",
            "- continue-on-error: true\n  run: ./scripts/cargo-vet.sh check\n",
            "- run: ./scripts/cargo-vet.sh check || true\n",
            "- run: ./scripts/cargo-vet.sh --version\n",
        ):
            with self.subTest(gate=gate), self.assertRaises(ValueError):
                check_workflow(workflow(GATES.rsplit("- run:", 1)[0] + gate + "- run: cargo test\n"))

    def test_reordered_or_late_gates_fail(self):
        for steps in ("- run: cargo test\n" + GATES, "\n".join(reversed(GATES.strip().splitlines())) + "\n- run: cargo test\n"):
            with self.subTest(steps=steps), self.assertRaisesRegex(ValueError, "requires the three gates"):
                check_workflow(workflow(steps))
