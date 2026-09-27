#!/usr/bin/env python3
"""Pin the latest CLIProxyAPI release in release/cliproxyapi.toml.

Reads the upstream release (the latest, or --version), takes each target's
asset SHA-256 from upstream's own checksums.txt, and rewrites the pin file.
Prints the pinned version; exits 0 whether or not it changed, so a caller
compares the file with git. --check exits 1 when a newer version exists
without writing anything.
"""
import argparse
import json
import os
import pathlib
import sys
import urllib.request

REPOSITORY = "router-for-me/CLIProxyAPI"
PIN = pathlib.Path(__file__).resolve().parents[2] / "release" / "cliproxyapi.toml"
# Our release targets, each with the upstream asset suffix that serves it.
TARGETS = {
    "aarch64-apple-darwin": "darwin_aarch64.tar.gz",
    "x86_64-apple-darwin": "darwin_amd64.tar.gz",
    "x86_64-unknown-linux-gnu": "linux_amd64.tar.gz",
    "aarch64-unknown-linux-gnu": "linux_aarch64.tar.gz",
    "x86_64-pc-windows-msvc": "windows_amd64.zip",
    "aarch64-pc-windows-msvc": "windows_aarch64.zip",
}
HEADER = """# The CLIProxyAPI build this release ships with: the subscription broker the
# gateway runs. Written by `scripts/release/bump-broker.py` (and the daily
# broker-bump workflow); the installer and `sterna update` download the named
# upstream asset and refuse it unless its SHA-256 matches the one pinned here.
"""


def fetch(url: str) -> bytes:
    headers = {"accept": "application/vnd.github+json", "user-agent": "glasshouse-bump-broker"}
    token = os.environ.get("GITHUB_TOKEN")
    if token and url.startswith("https://api.github.com/"):
        headers["authorization"] = f"Bearer {token}"
    with urllib.request.urlopen(urllib.request.Request(url, headers=headers), timeout=60) as response:
        return response.read()


def pinned_version() -> str | None:
    if not PIN.exists():
        return None
    for line in PIN.read_text().splitlines():
        if line.startswith("version = "):
            return line.split("=", 1)[1].strip().strip('"')
    return None


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--version", help="pin this upstream version instead of the latest")
    parser.add_argument("--check", action="store_true", help="exit 1 when a newer version exists")
    args = parser.parse_args()

    path = f"tags/v{args.version}" if args.version else "latest"
    release = json.loads(fetch(f"https://api.github.com/repos/{REPOSITORY}/releases/{path}"))
    version = release["tag_name"].removeprefix("v")
    if args.check:
        current = pinned_version()
        print(f"pinned {current}, upstream {version}")
        return 1 if current != version else 0

    assets = {asset["name"]: asset["browser_download_url"] for asset in release["assets"]}
    checksums_url = assets.get("checksums.txt")
    if not checksums_url:
        sys.exit(f"CLIProxyAPI {version} publishes no checksums.txt; refusing to pin it")
    sums = {}
    for line in fetch(checksums_url).decode().splitlines():
        parts = line.split()
        if len(parts) == 2:
            sums[parts[1]] = parts[0]

    lines = [HEADER.rstrip("\n"), f'repository = "{REPOSITORY}"', f'version = "{version}"', "", "[assets]"]
    for target, suffix in TARGETS.items():
        name = f"CLIProxyAPI_{version}_{suffix}"
        if name not in assets or name not in sums:
            sys.exit(f"CLIProxyAPI {version} has no checksummed asset {name}; refusing to pin it")
        lines.append(f'{target} = {{ name = "{name}", sha256 = "{sums[name]}" }}')
    PIN.write_text("\n".join(lines) + "\n")
    print(version)
    return 0


if __name__ == "__main__":
    sys.exit(main())
