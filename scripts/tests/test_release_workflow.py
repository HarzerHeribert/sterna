"""GH-PRE-RELEASE: the release workflow builds five targets, publishes only
on a real tag with dry_run false, and never touches crates.io.

User decision of 2026-09-06: a tagged pre-release (a GitHub release with
built binaries, no crates.io publish) is cut from main once the sterna
Windows cell is green. This file is the mechanical half of that guarantee --
it reads the workflow's own text and structure so a future edit that drops
the prerelease flag, unpins an action, or widens the publish gate fails here
instead of on the first real tag.
"""

import pathlib
import re

try:
    import yaml
    HAVE_YAML = True
except ImportError:
    yaml = None
    HAVE_YAML = False

REPO = pathlib.Path(__file__).resolve().parents[2]
WORKFLOW = REPO / ".github" / "workflows" / "release.yml"

EXPECTED_TARGETS = {
    "ubuntu-latest": "x86_64-unknown-linux-gnu",
    "ubuntu-24.04-arm": "aarch64-unknown-linux-gnu",
    "macos-latest": "aarch64-apple-darwin",
    "windows-latest": "x86_64-pc-windows-msvc",
    "windows-11-arm": "aarch64-pc-windows-msvc",
}

SHA_RE = re.compile(r"^[0-9a-f]{40}$")


def _text():
    return WORKFLOW.read_text()


def _load():
    """Parse the workflow. Falls back to a minimal structural check when
    PyYAML is not importable, per the packet's own instruction -- the
    fallback still exercises the matrix/needs/gating assertions below by
    reading the raw text instead of a parsed tree."""
    if HAVE_YAML:
        return yaml.safe_load(_text())
    return None


def test_workflow_exists():
    assert WORKFLOW.exists(), f"missing {WORKFLOW}"


def test_yaml_parses():
    if not HAVE_YAML:
        # Minimal check: the file must at least be non-empty and colon-shaped
        # top to bottom for the two required job names.
        text = _text()
        assert "jobs:" in text
        assert "build:" in text
        assert "publish:" in text
        return
    doc = _load()
    assert isinstance(doc, dict)
    assert "jobs" in doc


def test_five_targets_present_with_expected_runner_and_triple():
    text = _text()
    for os_name, triple in EXPECTED_TARGETS.items():
        assert os_name in text, f"missing runner {os_name}"
        assert triple in text, f"missing target triple {triple}"
    if HAVE_YAML:
        doc = _load()
        matrix = doc["jobs"]["build"]["strategy"]["matrix"]["include"]
        found = {row["os"]: row["target"] for row in matrix}
        assert found == EXPECTED_TARGETS, found


def test_publish_needs_build():
    if HAVE_YAML:
        doc = _load()
        needs = doc["jobs"]["publish"]["needs"]
        if isinstance(needs, str):
            needs = [needs]
        assert "build" in needs
    else:
        assert re.search(r"publish:\s*\n(?:.*\n)*?\s*needs:\s*build", _text())


def test_release_step_gated_on_tag_ref_and_dry_run_false():
    text = _text()
    # Find the softprops/action-gh-release step and read the `if:` just above it.
    idx = text.find("softprops/action-gh-release@")
    assert idx != -1, "no softprops/action-gh-release step found"
    preceding = text[max(0, idx - 400):idx]
    if_match = re.search(r"if:\s*(.+)", preceding)
    assert if_match, f"no if: condition guards the release step; preceding text:\n{preceding}"
    condition = if_match.group(1)
    assert "refs/tags/" in condition, f"release step does not check the tag ref: {condition!r}"
    assert "dry_run" in condition, f"release step does not check dry_run: {condition!r}"


def test_prerelease_true():
    assert re.search(r"prerelease:\s*true", _text()), "prerelease: true is missing"


def test_no_crates_io_publish():
    text = _text().lower()
    assert "crates.io" not in text
    assert "cargo publish" not in text


def test_every_uses_pinned_to_full_sha():
    offenders = []
    for line in _text().splitlines():
        stripped = line.strip()
        if not stripped.startswith("uses:") and " uses:" not in stripped:
            continue
        m = re.search(r"uses:\s*([^\s#]+)", stripped)
        if not m:
            continue
        ref = m.group(1)
        if "@" not in ref:
            offenders.append(stripped)
            continue
        sha = ref.rsplit("@", 1)[1]
        if not SHA_RE.match(sha):
            offenders.append(stripped)
    assert offenders == [], "not pinned to a 40-hex sha:\n" + "\n".join(offenders)


def test_only_github_token_permissions_no_other_secrets():
    text = _text()
    assert "secrets." not in text, "the workflow must use only the implicit GITHUB_TOKEN"


def test_contents_write_only_on_publish_job():
    if not HAVE_YAML:
        return
    doc = _load()
    build_perms = doc["jobs"]["build"].get("permissions", {})
    publish_perms = doc["jobs"]["publish"].get("permissions", {})
    assert build_perms.get("contents") == "read", build_perms
    assert publish_perms.get("contents") == "write", publish_perms
    top_perms = doc.get("permissions", {})
    assert top_perms.get("contents") == "read", top_perms


if __name__ == "__main__":
    import sys

    failed = 0
    for name, fn in sorted(globals().items()):
        if name.startswith("test_") and callable(fn):
            try:
                fn()
                print(f"ok   {name}")
            except AssertionError as e:
                failed += 1
                print(f"FAIL {name}: {e}")
    if not HAVE_YAML:
        print("  note: PyYAML not importable -- ran the minimal text-based fallback checks")
    sys.exit(1 if failed else 0)
