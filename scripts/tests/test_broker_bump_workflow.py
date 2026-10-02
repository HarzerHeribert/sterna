#!/usr/bin/env python3
"""The bump workflow refreshes the model figures every release ships, and
the Artificial Analysis key reaches only the step that fetches them.

`.github/workflows/broker-bump.yml` cuts every release the bot makes, and a
Sterna release cut by hand goes through it too (`release` set on a manual
run). These checks read its structure so an edit that hands the key to
another step, prints it from a script, or commits figures the gateway has not
read fails here instead of on the next release.
"""
from __future__ import annotations

import pathlib
import re
import unittest

try:
    import yaml
except ImportError:  # the checks read the workflow's structure, which needs PyYAML
    yaml = None

ROOT = pathlib.Path(__file__).resolve().parent.parent.parent
WORKFLOW = ROOT / ".github" / "workflows" / "broker-bump.yml"
SECRET = "secrets.ARTIFICIAL_ANALYSIS_API_KEY"


def load():
    return yaml.safe_load(WORKFLOW.read_text())


def steps():
    return load()["jobs"]["bump"]["steps"]


def named(name):
    found = [step for step in steps() if step.get("name") == name]
    assert len(found) == 1, f"one step named {name!r}"
    return found[0]


@unittest.skipIf(yaml is None, "PyYAML is not installed")
class BumpWorkflow(unittest.TestCase):
    def test_the_key_reaches_only_the_fetch_step(self):
        handed = [step["name"] for step in steps() if SECRET in str(step.get("env", {}))]
        self.assertEqual(handed, ["Refresh the model figures"])
        for step in steps():
            self.assertNotIn(SECRET, step.get("run", ""), step.get("name"))
            self.assertNotIn("ARTIFICIAL_ANALYSIS_API_KEY", step.get("run", "").replace(
                "python3 scripts/release/fetch-model-index.py", ""), step.get("name"))
        # The job knows only whether the key is there, never its value.
        job_env = load()["jobs"]["bump"]["env"]
        self.assertEqual(job_env["HAVE_ANALYSIS_KEY"], "${{ " + SECRET + " != '' }}")

    def test_the_figures_are_refreshed_for_every_release_and_read_before_the_commit(self):
        refresh = named("Refresh the model figures")
        self.assertIn("steps.pin.outputs.changed == 'true'", refresh["if"])
        self.assertIn("env.CUT == 'true'", refresh["if"])
        self.assertIn("env.HAVE_ANALYSIS_KEY == 'true'", refresh["if"])
        self.assertIn("scripts/release/fetch-model-index.py", refresh["run"])
        self.assertIn("scripts/bake-model-index.py", refresh["run"])
        order = [step.get("name") for step in steps()]
        check = order.index("The gateway reads the refreshed figures")
        commit = order.index("Commit, tag and push")
        self.assertLess(order.index("Refresh the model figures"), check)
        self.assertLess(check, commit)
        self.assertIn("-p inference-gateway --lib models", named("The gateway reads the refreshed figures")["run"])
        self.assertIn("crates/inference-gateway/data/model-index.json", named("Commit, tag and push")["run"])

    def test_a_release_can_be_cut_by_hand_and_a_trial_commits_nothing(self):
        inputs = load()[True]["workflow_dispatch"]["inputs"]
        self.assertEqual(inputs["release"]["type"], "boolean")
        self.assertEqual(inputs["trial"]["type"], "boolean")
        commit = named("Commit, tag and push")["if"]
        self.assertIn("env.CUT == 'true'", commit)
        self.assertNotIn("TRIAL", commit)
        self.assertIn("env.TRIAL == 'true'", named("Refresh the model figures")["if"])

    def test_a_failed_fetch_never_holds_up_a_release(self):
        self.assertIs(named("Refresh the model figures").get("continue-on-error"), True)

    def test_every_action_is_pinned_to_a_commit(self):
        for line in WORKFLOW.read_text().splitlines():
            found = re.search(r"uses:\s*([^\s#]+)", line)
            if found and not found.group(1).startswith("./"):
                self.assertRegex(found.group(1), r"@[0-9a-f]{40}$", line.strip())


if __name__ == "__main__":
    unittest.main()
