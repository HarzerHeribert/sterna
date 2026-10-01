#!/usr/bin/env python3
"""``sites/public/install.ps1`` against a release served from a local directory.

The Windows installer is run under PowerShell (``pwsh``, which the CI's
Linux runners carry) through its ``STERNA_*`` seams, exactly as ``install.sh``
would be: the newest release is chosen by version and not by the list's
order, the zip is verified against ``SHA256SUMS`` and unpacked into a fresh
``versions/<tag>``, ``current`` points at it, the pinned broker is adopted
through the new gateway, and a second run reinstalls nothing. A release whose
zip does not match its sum installs nothing at all. With ``STERNA_DESKTOP``
the desktop app's zip is verified and unpacked beside the binaries, and the
install root is marked so the updater keeps it in step.

The fake ``inference-gateway.exe`` is a shell script, so the adoption half
runs only where a script can be executed by path (not on Windows); the rest
runs anywhere ``pwsh`` does. Without ``pwsh`` the test says so and passes:
it is the CI's Linux runners that hold it to account.
"""

from __future__ import annotations

import hashlib
import http.server
import json
import os
import shutil
import subprocess
import sys
import tempfile
import threading
import zipfile
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]
SCRIPT = REPO / "sites" / "public" / "install.ps1"
TARGET = "x86_64-pc-windows-msvc"
BROKER = "9.9.9"


def sha256(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def serve(root: Path) -> str:
    class Quiet(http.server.SimpleHTTPRequestHandler):
        def __init__(self, *args, **kwargs):
            super().__init__(*args, directory=str(root), **kwargs)

        def log_message(self, *args):
            pass

    server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Quiet)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    return f"http://127.0.0.1:{server.server_address[1]}"


def publish(site: Path, tag: str, tamper: bool = False) -> None:
    """A release as release.yml shapes it: one zip holding one top folder."""
    version = tag[1:]
    folder = f"sterna-{version}-{TARGET}"
    downloads = site / "download" / tag
    downloads.mkdir(parents=True)
    broker_zip = f"CLIProxyAPI_{BROKER}_windows_amd64.zip"
    broker_path = site / "broker" / f"v{BROKER}" / broker_zip
    if not broker_path.exists():
        broker_path.parent.mkdir(parents=True)
        with zipfile.ZipFile(broker_path, "w") as z:
            z.writestr("cli-proxy-api.exe", "broker")
    pin = (
        'repository = "example/CLIProxyAPI"\n'
        f'version = "{BROKER}"\n\n[assets]\n'
        f'{TARGET} = {{ name = "{broker_zip}", sha256 = "{sha256(broker_path)}" }}\n'
    )
    gateway = '#!/bin/sh\nprintf \'%s\\n\' "$*" >> "$ADOPT_LOG"\n'
    archive = downloads / f"{folder}.zip"
    with zipfile.ZipFile(archive, "w") as z:
        z.writestr(f"{folder}/sterna.exe", f"sterna {tag}")
        info = zipfile.ZipInfo(f"{folder}/inference-gateway.exe")
        info.external_attr = 0o755 << 16
        z.writestr(info, gateway)
        z.writestr(f"{folder}/cliproxyapi.toml", pin)
    desktop = downloads / f"sterna-desktop-{version}-{TARGET}.zip"
    with zipfile.ZipFile(desktop, "w") as z:
        z.writestr("desktop/sterna-desktop.exe", f"desktop {tag}")
        z.writestr("desktop/sterna.exe", f"sterna {tag}")
        z.writestr("desktop/inference-gateway.exe", "gateway")
    digest = sha256(archive)
    if tamper:
        digest = "0" * 64
    (downloads / "SHA256SUMS").write_text(
        f"{digest}  {folder}.zip\n{sha256(desktop)}  {desktop.name}\n"
    )


def run(
    pwsh: str, base: str, home: Path, log: Path, desktop: bool = False
) -> subprocess.CompletedProcess:
    env = dict(os.environ)
    env.pop("STERNA_DESKTOP", None)
    if desktop:
        env["STERNA_DESKTOP"] = "1"
        env["STERNA_START_MENU"] = str(home.parent / "menu")
    env.update(
        STERNA_RELEASES_API=f"{base}/releases.json",
        STERNA_RELEASE_DOWNLOADS=f"{base}/download",
        STERNA_BROKER_DOWNLOADS=f"{base}/broker",
        STERNA_HOME=str(home),
        STERNA_TARGET=TARGET,
        STERNA_PATH_SCOPE="Process",
        ADOPT_LOG=str(log),
    )
    # `irm | iex` is how it is run for real: the script text, not a file.
    command = f"Get-Content -Raw '{SCRIPT}' | Invoke-Expression"
    return subprocess.run(
        [pwsh, "-NoProfile", "-NonInteractive", "-Command", command],
        env=env,
        capture_output=True,
        text=True,
        timeout=120,
    )


def main() -> int:
    pwsh = os.environ.get("PWSH") or shutil.which("pwsh")
    if not pwsh:
        print("test_install_ps1: skipped, no pwsh on this machine")
        return 0
    failures: list[str] = []

    def check(condition: bool, what: str, output: subprocess.CompletedProcess) -> None:
        if not condition:
            failures.append(f"{what}\n--- stdout\n{output.stdout}\n--- stderr\n{output.stderr}")

    with tempfile.TemporaryDirectory() as scratch:
        scratch = Path(scratch)
        site = scratch / "site"
        site.mkdir()
        # Neither the list's first nor its last is the newest: pre.10 is, and
        # a string sort would put it below pre.9.
        for tag in ("v0.1.0-pre.9", "v0.1.0-pre.10", "v0.1.0-pre.2"):
            publish(site, tag)
        listed = ["v0.1.0-pre.9", "v0.1.0-pre.10", "archive/old", "v0.1.0-pre.2"]
        (site / "releases.json").write_text(json.dumps([{"tag_name": tag} for tag in listed]))
        base = serve(site)
        home = scratch / "home"
        log = scratch / "adopt.log"

        first = run(pwsh, base, home, log)
        check(first.returncode == 0, "the first install failed", first)
        dest = home / "versions" / "v0.1.0-pre.10"
        check((dest / "bin" / "sterna.exe").is_file(), "pre.10 was not installed", first)
        for other in ("v0.1.0-pre.9", "v0.1.0-pre.2"):
            check(not (home / "versions" / other).exists(), f"{other} was installed instead", first)
        check(not (home / "versions" / "v0.1.0-pre.10.partial").exists(), "the partial directory was left", first)
        current = home / "current"
        check(
            current.is_symlink() and Path(os.readlink(current)).name == "v0.1.0-pre.10",
            "current does not point at pre.10",
            first,
        )
        if os.name != "nt":
            adopted = log.read_text() if log.exists() else ""
            check(
                adopted.strip().startswith("subscriptions adopt-binary")
                and adopted.strip().endswith("cli-proxy-api.exe"),
                f"the broker was not adopted through the gateway: {adopted!r}",
                first,
            )
            check((home / "broker-version").read_text() == BROKER, "the broker stamp is wrong", first)
        check("current" in first.stdout and "PATH" in first.stdout, "the PATH line is missing", first)

        second = run(pwsh, base, home, log)
        check(second.returncode == 0, "the second run failed", second)
        check("already installed" in second.stdout, "the second run reinstalled", second)
        if os.name != "nt":
            check(len(log.read_text().splitlines()) == 1, "the broker was adopted twice", second)
        check(not (home / "desktop").exists(), "the app was marked without being asked for", second)

        # Asked for, the desktop app goes beside the version it ships with.
        third = run(pwsh, base, home, log, desktop=True)
        check(third.returncode == 0, "the desktop install failed", third)
        app = dest / "desktop" / "sterna-desktop.exe"
        check(app.is_file() and app.read_text() == "desktop v0.1.0-pre.10", "the app was not unpacked", third)
        check((home / "desktop").is_file(), "the install root was not marked for the app", third)
        check("Desktop app" in third.stdout, "the run does not say where the app is", third)

        # A zip that does not match its sum installs nothing.
        bad_site = scratch / "bad"
        bad_site.mkdir()
        publish(bad_site, "v0.1.0-pre.11", tamper=True)
        (bad_site / "releases.json").write_text(json.dumps([{"tag_name": "v0.1.0-pre.11"}]))
        bad_home = scratch / "bad-home"
        bad = run(pwsh, serve(bad_site), bad_home, scratch / "bad.log")
        check(bad.returncode != 0, "a tampered zip was accepted", bad)
        check("does not match its SHA-256" in (bad.stdout + bad.stderr), "the refusal does not say why", bad)
        check(not (bad_home / "versions" / "v0.1.0-pre.11").exists(), "a tampered zip was unpacked", bad)
        check(not (bad_home / "current").exists(), "current was made for a tampered zip", bad)

    for failure in failures:
        print(f"test_install_ps1: {failure}\n", file=sys.stderr)
    if failures:
        return 1
    print("test_install_ps1: ok")
    return 0


if __name__ == "__main__":
    sys.exit(main())
