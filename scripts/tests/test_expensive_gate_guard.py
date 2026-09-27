"""Direct full blast-radius runs require an explicit opt-in at the harness boundary."""

import json
import pathlib
import subprocess
import unittest

REPO = pathlib.Path(__file__).resolve().parents[2]
HOOK = REPO / "scripts" / "hooks" / "guard-expensive-gate.sh"


def run(command: str, tool: str = "Bash") -> tuple[int, str]:
    payload = json.dumps({"tool_name": tool, "tool_input": {"command": command}})
    result = subprocess.run([str(HOOK)], input=payload, capture_output=True, text=True)
    return result.returncode, result.stderr


class ExpensiveGateGuard(unittest.TestCase):
    def test_accidental_full_forms_are_blocked(self):
        commands = [
            "scripts/blast-radius.sh",
            "timeout 580 scripts/blast-radius.sh 2>&1 | grep blast-radius | tail -6",
            "bash scripts/blast-radius.sh --since HEAD~2",
            "cargo fmt && ./scripts/blast-radius.sh --jobs 4",
        ]
        for command in commands:
            rc, error = run(command)
            self.assertEqual(rc, 2, f"should block: {command!r}\n{error}")

    def test_targeted_read_only_and_explicit_full_forms_are_allowed(self):
        commands = [
            "scripts/blast-radius.sh --targeted crates/sterna/src/tui.rs",
            "scripts/blast-radius.sh --status",
            "scripts/blast-radius.sh --dry-run",
            "scripts/blast-radius.sh --list",
            "scripts/blast-radius.sh --full",
            "scripts/blast-radius.sh --full --since HEAD~2",
            "scripts/blast-radius.sh --serial",
            "scripts/integrate.sh worker-name",
            "scripts/ci-local.sh --scoped",
        ]
        for command in commands:
            rc, error = run(command)
            self.assertEqual(rc, 0, f"should allow: {command!r}\n{error}")

    def test_targeted_requires_the_workers_explicit_files(self):
        rc, error = run("timeout 580 scripts/blast-radius.sh --targeted | tail -14")
        self.assertEqual(rc, 2)
        self.assertIn("without filenames means every dirty Rust", error)
        self.assertIn("--targeted <every .rs file this worker changed>", error)

    def test_block_explains_both_tiers(self):
        rc, error = run("scripts/blast-radius.sh")
        self.assertEqual(rc, 2)
        self.assertIn("--targeted <every changed .rs file>", error)
        self.assertIn("--full", error)
        self.assertIn("specific cargo test", error)

    def test_non_bash_tools_are_ignored(self):
        rc, _ = run("scripts/blast-radius.sh", tool="Read")
        self.assertEqual(rc, 0)

    def test_both_harnesses_register_the_guard(self):
        claude = json.loads((REPO / ".claude" / "settings.json").read_text())
        codex = json.loads((REPO / ".codex" / "hooks.json").read_text())
        self.assertIn("guard-expensive-gate.sh", json.dumps(claude))
        self.assertIn("guard-expensive-gate.sh", json.dumps(codex))


if __name__ == "__main__":
    unittest.main()
