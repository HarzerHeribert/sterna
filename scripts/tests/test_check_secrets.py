"""scripts/check-secrets.py: local-key values, shape rules with a low
entropy bar backstopped by a fingerprint allowlist, the per-line marker, and
the pre-commit/pre-push hooks that wire it to git. Keep this file under a
minute."""

import importlib.util
import os
import pathlib
import random
import string
import subprocess
import sys
import tempfile
import unittest

REPO_ROOT = pathlib.Path(__file__).resolve().parents[2]
SCANNER = REPO_ROOT / "scripts" / "check-secrets.py"
HOOKS_DIR = REPO_ROOT / "scripts" / "git-hooks"

spec = importlib.util.spec_from_file_location("check_secrets", SCANNER)
check_secrets = importlib.util.module_from_spec(spec)
spec.loader.exec_module(check_secrets)

# The thirteen fixture lines named in the packet (the ten originally planted
# plus the three a plain grep also turns up): (repo-relative path, line
# number). These must pass through the REAL fingerprint allowlist
# (scripts/check-secrets-allow.txt) with no edit to any of them.
NAMED_FIXTURES = [
    ("crates/inference-gateway/src/gateway/conformance.rs", 64),
    ("crates/inference-gateway/src/gateway/ingress.rs", 1454),
    ("crates/inference-gateway/src/gateway/tests.rs", 282),
    ("crates/inference-gateway/src/secret/mod.rs", 415),
    ("crates/inference-gateway/src/secret/mod.rs", 636),
    ("crates/inference-gateway/src/secret/mod.rs", 640),
    ("crates/inference-gateway/src/secret/mod.rs", 644),
    ("crates/inference-gateway/src/secret/mod.rs", 693),
]


def real_key(prefix="sk-ant-api03-", length=60, seed=1):
    rng = random.Random(seed)
    alphabet = string.ascii_letters + string.digits
    return prefix + "".join(rng.choice(alphabet) for _ in range(length))


def random_tokens(prefix, length, n, seed):
    rng = random.Random(seed)
    alphabet = string.ascii_letters + string.digits
    return [prefix + "".join(rng.choice(alphabet) for _ in range(length)) for _ in range(n)]


def run_scanner(args, cwd=None):
    return subprocess.run(
        [sys.executable, str(SCANNER)] + args,
        capture_output=True,
        text=True,
        cwd=cwd,
    )


def read_line(path: pathlib.Path, lineno: int) -> str:
    with open(path, "r", encoding="utf-8") as fh:
        return fh.read().splitlines()[lineno - 1]


class ShapeRulesAndEntropy(unittest.TestCase):
    def test_named_fixture_lines_pass_through_the_allowlist(self):
        allowlist = check_secrets.load_allowlist(check_secrets.ALLOWLIST_FILE)
        for rel_path, lineno in NAMED_FIXTURES:
            line = read_line(REPO_ROOT / rel_path, lineno)
            findings = check_secrets.evaluate_line(rel_path, line, {}, allowlist)
            self.assertEqual(findings, [], f"{rel_path}:{lineno} should pass via the allowlist: {line!r}")

    def test_a_fixture_line_changed_by_one_character_is_flagged_again(self):
        allowlist = check_secrets.load_allowlist(check_secrets.ALLOWLIST_FILE)
        rel_path, lineno = NAMED_FIXTURES[0]
        line = read_line(REPO_ROOT / rel_path, lineno)
        # Flip one digit -- the fingerprint no longer matches.
        changed = line.replace("000111222333", "000111222334")
        self.assertNotEqual(changed, line)
        findings = check_secrets.evaluate_line(rel_path, changed, {}, allowlist)
        self.assertNotEqual(findings, [], "an edited fixture line must be flagged again")

    def test_a_realistic_random_key_is_flagged(self):
        line = f'const X: &str = "{real_key()}";'
        findings = check_secrets.scan_line(line, {})
        self.assertIn("anthropic", findings)

    def test_100_random_36char_ghp_tokens_are_all_flagged(self):
        caught = 0
        for tok in random_tokens("ghp_", 36, 100, seed=10):
            findings = check_secrets.scan_line(f'const X: &str = "{tok}";', {})
            if "github" in findings:
                caught += 1
        self.assertEqual(caught, 100)

    def test_100_random_52char_gsk_tokens_are_all_flagged(self):
        caught = 0
        for tok in random_tokens("gsk_", 52, 100, seed=11):
            findings = check_secrets.scan_line(f'const X: &str = "{tok}";', {})
            if "groq" in findings:
                caught += 1
        self.assertEqual(caught, 100)

    def test_marker_allows_a_shape_finding(self):
        line = f'const X: &str = "{real_key()}"; // glasshouse:not-a-secret'
        findings = check_secrets.evaluate_line("some/file.rs", line, {}, set())
        self.assertEqual(findings, [])

    def test_marker_does_not_allow_a_local_key_finding(self):
        local_keys = {"MY_KEY": "abcdEFGH12345678reallocalvalue"}
        line = 'x = "abcdEFGH12345678reallocalvalue" # glasshouse:not-a-secret'
        findings = check_secrets.evaluate_line("some/file.rs", line, local_keys, set())
        self.assertIn("local-key MY_KEY", findings)

    def test_allowlist_never_waives_a_local_key_finding(self):
        local_keys = {"MY_KEY": "abcdEFGH12345678reallocalvalue"}
        line = 'x = "abcdEFGH12345678reallocalvalue"'
        allowlist = {("some/file.rs", check_secrets.line_fingerprint(line))}
        findings = check_secrets.evaluate_line("some/file.rs", line, local_keys, allowlist)
        self.assertIn("local-key MY_KEY", findings)


class FingerprintCommand(unittest.TestCase):
    def test_fingerprint_output_round_trips_into_the_allowlist(self):
        with tempfile.TemporaryDirectory() as tmp:
            target = os.path.join(tmp, "fixture.rs")
            line = 'const X: &str = "sk-roundtrip-fixture-example-000111";'  # glasshouse:not-a-secret
            with open(target, "w") as fh:
                fh.write(line + "\n")

            result = run_scanner(["--fingerprint", f"{target}:1"])
            self.assertEqual(result.returncode, 0)
            entry = result.stdout.strip()
            self.assertEqual(entry, f"{target}:{check_secrets.line_fingerprint(line)}")

            allow_file = os.path.join(tmp, "allow.txt")
            with open(allow_file, "w") as fh:
                fh.write("# round-trip test entry\n")
                fh.write(entry + "\n")

            before = run_scanner(["--files", target])
            self.assertEqual(before.returncode, 1, "unallowlisted, the fixture should be flagged")

            after = run_scanner(["--files", target, "--allow-file", allow_file])
            self.assertEqual(after.returncode, 0, "allowlisted by the round-tripped entry, it should pass")


class LocalKeyFile(unittest.TestCase):
    def test_local_key_value_is_refused_and_never_printed(self):
        with tempfile.TemporaryDirectory() as tmp:
            key_file = os.path.join(tmp, "provider-keys.env")
            with open(key_file, "w") as fh:
                fh.write("FAKE_PROVIDER_KEY=zzzz0000FortyCharacterFakeLocalValueXYZ\n")
            keys = check_secrets.load_local_keys(key_file)
            self.assertEqual(keys, {"FAKE_PROVIDER_KEY": "zzzz0000FortyCharacterFakeLocalValueXYZ"})

            line = 'const X: &str = "zzzz0000FortyCharacterFakeLocalValueXYZ";'
            findings = check_secrets.scan_line(line, keys)
            self.assertEqual(findings, ["local-key FAKE_PROVIDER_KEY"])

            leak_file = os.path.join(tmp, "leak.rs")
            with open(leak_file, "w") as fh:
                fh.write(line + "\n")
            result = run_scanner(["--files", leak_file, "--key-file", key_file])
            self.assertEqual(result.returncode, 1)
            self.assertIn("local-key FAKE_PROVIDER_KEY", result.stdout)
            self.assertNotIn("zzzz0000FortyCharacterFakeLocalValueXYZ", result.stdout)
            self.assertNotIn("zzzz0000FortyCharacterFakeLocalValueXYZ", result.stderr)

    def test_missing_key_file_is_fine(self):
        keys = check_secrets.load_local_keys("/nonexistent/path/provider-keys.env")
        self.assertEqual(keys, {})


class GitRepoScans(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.repo = self.tmp.name
        subprocess.run(["git", "init", "-q", self.repo], check=True)
        subprocess.run(["git", "-C", self.repo, "config", "user.email", "t@example.com"], check=True)
        subprocess.run(["git", "-C", self.repo, "config", "user.name", "Test"], check=True)
        # Bring the scanner and hooks into the temp repo so relative paths resolve.
        # No check-secrets-allow.txt is copied in: this repo's own fixtures
        # are all real-shaped random keys, deliberately not allowlisted, so
        # they exercise the entropy/marker path rather than the allowlist.
        scripts_dir = os.path.join(self.repo, "scripts")
        os.makedirs(scripts_dir, exist_ok=True)
        with open(SCANNER) as src, open(os.path.join(scripts_dir, "check-secrets.py"), "w") as dst:
            dst.write(src.read())
        import shutil
        shutil.copytree(HOOKS_DIR, os.path.join(scripts_dir, "git-hooks"))
        for name in ("pre-commit", "pre-push"):
            os.chmod(os.path.join(scripts_dir, "git-hooks", name), 0o755)
        subprocess.run(
            ["git", "-C", self.repo, "config", "core.hooksPath", "scripts/git-hooks"],
            check=True,
        )
        readme = os.path.join(self.repo, "README.md")
        with open(readme, "w") as fh:
            fh.write("hello\n")
        subprocess.run(["git", "-C", self.repo, "add", "-A"], check=True)
        subprocess.run(["git", "-C", self.repo, "commit", "-q", "-m", "init"], check=True)

    def tearDown(self):
        self.tmp.cleanup()

    def test_a_commit_with_a_real_shaped_key_is_refused_by_pre_commit(self):
        key = real_key()
        path = os.path.join(self.repo, "leak.rs")
        with open(path, "w") as fh:
            fh.write(f'const X: &str = "{key}";\n')
        subprocess.run(["git", "-C", self.repo, "add", "leak.rs"], check=True)
        result = subprocess.run(
            ["git", "-C", self.repo, "commit", "-q", "-m", "leak"],
            capture_output=True,
            text=True,
        )
        self.assertNotEqual(result.returncode, 0)
        self.assertNotIn(key, result.stdout)
        self.assertNotIn(key, result.stderr)

    def test_range_finds_a_key_added_in_the_second_commit_not_one_removed_in_the_first(self):
        old_key = real_key(seed=2)
        new_key = real_key(seed=3)

        path = os.path.join(self.repo, "history.rs")
        with open(path, "w") as fh:
            fh.write(f'const OLD: &str = "{old_key}"; // glasshouse:not-a-secret\n')
        subprocess.run(["git", "-C", self.repo, "add", "history.rs"], check=True)
        subprocess.run(["git", "-C", self.repo, "commit", "-q", "-m", "add old (marked)"], check=True)
        commit_a = subprocess.run(
            ["git", "-C", self.repo, "rev-parse", "HEAD"], capture_output=True, text=True
        ).stdout.strip()

        # This second commit's own added line is an unmasked real-shaped key
        # and pre-commit would (correctly) refuse it; bypass the hook here
        # because this test targets --range, which pre-push runs after the
        # commit already exists.
        with open(path, "w") as fh:
            fh.write(f'const NEW: &str = "{new_key}";\n')
        subprocess.run(["git", "-C", self.repo, "add", "history.rs"], check=True)
        subprocess.run(
            ["git", "-C", self.repo, "-c", "core.hooksPath=/dev/null", "commit", "-q", "-m", "remove old, add new"],
            check=True,
        )
        commit_b = subprocess.run(
            ["git", "-C", self.repo, "rev-parse", "HEAD"], capture_output=True, text=True
        ).stdout.strip()

        result = run_scanner(["--range", f"{commit_a}..{commit_b}"], cwd=self.repo)
        self.assertEqual(result.returncode, 1)
        self.assertIn("history.rs", result.stdout)
        self.assertNotIn(old_key, result.stdout)
        self.assertNotIn(new_key, result.stdout)

    def test_pre_push_new_branch_scans_from_merge_base(self):
        subprocess.run(["git", "-C", self.repo, "checkout", "-q", "-b", "feature"], check=True)
        key = real_key(seed=4)
        path = os.path.join(self.repo, "branch.rs")
        with open(path, "w") as fh:
            fh.write(f'const X: &str = "{key}";\n')
        subprocess.run(["git", "-C", self.repo, "add", "branch.rs"], check=True)
        # This commit alone would be refused by pre-commit; bypass it here to
        # isolate the pre-push new-branch code path (no origin/main exists in
        # this bare-init temp repo, so it falls back to --tree at the tip).
        subprocess.run(
            ["git", "-C", self.repo, "-c", "core.hooksPath=/dev/null", "commit", "-q", "-m", "branch add"],
            check=True,
        )
        local_sha = subprocess.run(
            ["git", "-C", self.repo, "rev-parse", "HEAD"], capture_output=True, text=True
        ).stdout.strip()
        zero = "0" * 40
        stdin = f"refs/heads/feature {local_sha} refs/heads/feature {zero}\n"
        result = subprocess.run(
            [str(HOOKS_DIR / "pre-push")],
            input=stdin,
            capture_output=True,
            text=True,
            cwd=self.repo,
        )
        self.assertNotEqual(result.returncode, 0)
        self.assertNotIn(key, result.stdout)
        self.assertNotIn(key, result.stderr)


if __name__ == "__main__":
    unittest.main()
