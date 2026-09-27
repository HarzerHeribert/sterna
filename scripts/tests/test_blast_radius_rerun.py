"""GH-GATE-RERUN-ALONE: a red target in a rule-4 load-sensitive family gets
exactly one rerun, alone, before it counts as red.

CLAUDE.md's Decompression rule 4: "A red target in a known load-sensitive
family (the pty fixtures: tui_live, lifecycle_cutoff) is re-run alone
once by the gate and reported flaky-pass, which is not red and gets no
attribution write-up ... Until GH-GATE-RERUN-ALONE lands, do the one rerun by
hand and stop there." This is that line landing.

None of these tests run the real cargo suite: `cargo` is stubbed on PATH so
each case is driven by a counter file, not by an actual test's timing.
"""
from __future__ import annotations

import os
import pathlib
import subprocess
import tempfile
import textwrap
import unittest

SCRIPT = pathlib.Path(__file__).resolve().parents[1] / "blast-radius.sh"

# Records one invocation per call: increments a per-target counter file and
# consults a per-target "behavior" file to decide what this call should do.
# `doc` (cargo doc --no-deps) always succeeds -- this test is about the
# rerun decision on test targets, not rustdoc.
CARGO_STUB = textwrap.dedent(r"""#!/usr/bin/env bash
    set -u
    LOGDIR="${CARGO_STUB_DIR:?CARGO_STUB_DIR not set}"
    mkdir -p "$LOGDIR"

    if [ "${1:-}" = "doc" ]; then
      exit 0
    fi

    name=""
    prev=""
    for a in "$@"; do
      if [ "$prev" = "--test" ] || [ "$prev" = "--lib" ]; then
        name="$a"
      fi
      prev="$a"
    done
    [ -n "$name" ] || name="_lib_"

    countfile="$LOGDIR/count-$name"
    n=0
    [ -f "$countfile" ] && n="$(cat "$countfile")"
    n=$((n + 1))
    echo "$n" > "$countfile"

    behfile="$LOGDIR/behavior-$name"
    behavior="always_pass"
    [ -f "$behfile" ] && behavior="$(cat "$behfile")"

    case "$behavior" in
      fail_then_pass)
        if [ "$n" -eq 1 ]; then
          echo "test tests::x ... FAILED"
          echo "test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out"
          exit 101
        fi
        echo "test tests::x ... ok"
        echo "test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out"
        exit 0
        ;;
      always_fail)
        echo "test tests::x ... FAILED"
        echo "test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out"
        exit 101
        ;;
      *)
        echo "test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out"
        exit 0
        ;;
    esac
    """)

# check-file-sizes.py resolves its own repo root from `__file__`, not cwd, so
# a fake tree needs its own stub or it would measure the real checkout. This test is not about the size ratchet.
CHECK_FILE_SIZES_STUB = textwrap.dedent("""\
    #!/usr/bin/env python3
    print("check-file-sizes: ok (stub)")
    raise SystemExit(0)
    """)


class Rerun(unittest.TestCase):
    def setUp(self):
        self.tmp = pathlib.Path(tempfile.mkdtemp())
        (self.tmp / "scripts").mkdir()
        (self.tmp / "scripts" / "blast-radius.sh").write_bytes(SCRIPT.read_bytes())
        os.chmod(self.tmp / "scripts" / "blast-radius.sh", 0o755)
        (self.tmp / "scripts" / "check-file-sizes.py").write_text(CHECK_FILE_SIZES_STUB)
        os.chmod(self.tmp / "scripts" / "check-file-sizes.py", 0o755)

        self.bin = self.tmp / "bin"
        self.bin.mkdir()
        cargo = self.bin / "cargo"
        cargo.write_text(CARGO_STUB)
        os.chmod(cargo, 0o755)

        self.cargo_log = self.tmp / "cargo-log"
        self.cargo_log.mkdir()

        self.git("init", "-q")
        self.git("config", "user.email", "t@example.com")
        self.git("config", "user.name", "t")
        (self.tmp / "README.md").write_text("x\n")
        self.git("add", "-A")
        self.git("commit", "-q", "-m", "init")

    def git(self, *args):
        return subprocess.run(["git", *args], cwd=self.tmp, capture_output=True, text=True)

    def set_behavior(self, target: str, behavior: str) -> None:
        (self.cargo_log / f"behavior-{target}").write_text(behavior)

    def call_count(self, target: str) -> int:
        f = self.cargo_log / f"count-{target}"
        return int(f.read_text()) if f.exists() else 0

    def add_test_target(self, name: str) -> None:
        p = self.tmp / "crates" / "sterna" / "tests" / f"{name}.rs"
        p.parent.mkdir(parents=True, exist_ok=True)
        p.write_text("#[test]\nfn x() {}\n")

    def run_gate(self):
        env = dict(os.environ)
        env["PATH"] = f"{self.bin}:{env['PATH']}"
        env["CARGO_STUB_DIR"] = str(self.cargo_log)
        return subprocess.run(
            ["bash", str(self.tmp / "scripts" / "blast-radius.sh"), "--serial"],
            cwd=self.tmp, capture_output=True, text=True, env=env, timeout=60,
        )

    # 1. a red in a load-sensitive family that passes on rerun exits 0 and
    # prints flaky-pass, quoting both test result: lines.
    def test_a_flaky_family_that_passes_on_rerun_exits_zero(self):
        self.add_test_target("tui_live")
        self.set_behavior("tui_live", "fail_then_pass")

        r = self.run_gate()
        out = r.stdout + r.stderr

        self.assertEqual(r.returncode, 0, out)
        self.assertIn("flaky-pass", out)
        self.assertIn("tui_live", out)
        self.assertIn("test result: FAILED", out)
        self.assertIn("test result: ok", out)
        self.assertIn("1 flaky-pass", out)
        self.assertEqual(self.call_count("tui_live"), 2)

    # 2. a red in a load-sensitive family that fails on rerun exits non-zero
    # and says the rerun was made -- and is rerun exactly once, not looped.
    def test_a_flaky_family_that_fails_on_rerun_exits_nonzero(self):
        self.add_test_target("lifecycle_cutoff")
        self.set_behavior("lifecycle_cutoff", "always_fail")

        r = self.run_gate()
        out = r.stdout + r.stderr

        self.assertNotEqual(r.returncode, 0, out)
        self.assertIn("lifecycle_cutoff", out)
        self.assertIn("rerun", out.lower())
        self.assertIn("failed on the rerun too", out)
        self.assertEqual(self.call_count("lifecycle_cutoff"), 2)

    # 3. a red outside those families exits non-zero and is not rerun --
    # the stub is invoked once, not twice. This is also the mutation target:
    # a mutation that makes the family check always true must fail THIS case.
    def test_a_red_outside_the_named_families_is_not_rerun(self):
        self.add_test_target("ordinary_logic_test")
        self.set_behavior("ordinary_logic_test", "always_fail")

        r = self.run_gate()
        out = r.stdout + r.stderr

        self.assertNotEqual(r.returncode, 0, out)
        self.assertNotIn("flaky-pass", out)
        self.assertEqual(self.call_count("ordinary_logic_test"), 1)



if __name__ == "__main__":
    unittest.main()
