"""blast-radius.sh runs one gate per tree at a time, and can say whether one is running.

2026-08-31: two concurrent blast radii in the main checkout made four gates
report false reds (each one's cargo load pushed the other's PTY fixtures past
their timeouts). 2026-09-01: an orchestrator started a wave sweep beside its
predecessor's 40-minute-old one because nothing on the machine could answer
"is a gate running in this tree?" in one command. `--status` answers it; a
second start in the same tree refuses with exit 3 unless the holder is dead.

These tests never run cargo: the refusal and the status query both resolve
before section 4, and the stale-lock case is exercised with `--dry-run`, which
needs no lock at all.
"""

import os
import pathlib
import subprocess
import time
import unittest
import zlib

SCRIPT = pathlib.Path(__file__).resolve().parents[1] / "blast-radius.sh"
REPO = SCRIPT.parents[1]
# A small test file: few symbols to trace, so the refusal path is reached in
# a second or two rather than after a minute of grep.
SMALL = "crates/inference-gateway/tests/boundary.rs"


def lock_path():
    # Mirror the script's key: `cksum` of the tree path (POSIX CRC, same as
    # zlib.crc32 over the bytes plus the length, which cksum appends).
    out = subprocess.run(["cksum"], input=str(REPO).encode(), capture_output=True).stdout
    return pathlib.Path(f"/tmp/blast-radius-{out.split()[0].decode()}.lock")


def write_lock(pid, args="--since abc123", age=40):
    lock_path().write_text(
        f"pid={pid}\nstarted={int(time.time()) - age}\nargs={args}\ntree={REPO}\n"
    )


def run(*args, timeout=60):
    return subprocess.run(
        [str(SCRIPT), *args], cwd=REPO, capture_output=True, text=True, timeout=timeout
    )


class Lock(unittest.TestCase):
    def setUp(self):
        self.holder = None
        lock_path().unlink(missing_ok=True)

    def tearDown(self):
        if self.holder:
            self.holder.kill()
            self.holder.wait()
        lock_path().unlink(missing_ok=True)

    def start_holder(self):
        self.holder = subprocess.Popen(["sleep", "300"])
        write_lock(self.holder.pid)

    def test_status_with_no_gate_running_exits_1(self):
        r = run("--status")
        self.assertEqual(r.returncode, 1, r.stdout + r.stderr)
        self.assertIn("no gate running", r.stdout)

    def test_status_names_the_live_holder(self):
        self.start_holder()
        r = run("--status")
        self.assertEqual(r.returncode, 0, r.stdout + r.stderr)
        self.assertIn(f"pid {self.holder.pid}", r.stdout)
        self.assertIn("--since abc123", r.stdout)

    def test_a_second_gate_in_the_same_tree_refuses_with_exit_3(self):
        self.start_holder()
        r = run("--targeted", SMALL)
        self.assertEqual(r.returncode, 3, r.stdout + r.stderr)
        self.assertIn("REFUSING", r.stderr)
        self.assertIn(f"pid {self.holder.pid}", r.stderr)
        self.assertIn("--status", r.stderr)
        # and it did not touch the holder's lock
        self.assertIn(f"pid={self.holder.pid}", lock_path().read_text())

    def test_a_dead_holders_lock_is_stale_and_status_says_so(self):
        p = subprocess.Popen(["sleep", "300"])
        pid = p.pid
        p.kill()
        p.wait()
        write_lock(pid)
        r = run("--status")
        self.assertEqual(r.returncode, 1, r.stdout + r.stderr)

    def test_dry_run_and_list_need_no_lock(self):
        """Read-only modes must never be refused: that teaches routing around."""
        self.start_holder()
        r = run("--dry-run", SMALL)
        self.assertEqual(r.returncode, 0, r.stdout + r.stderr)
        self.assertIn("nothing executed", r.stdout)


if __name__ == "__main__":
    unittest.main()
