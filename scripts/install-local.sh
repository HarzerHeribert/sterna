#!/usr/bin/env bash
#
# install-local.sh — install the binaries this repository builds, for real,
# onto this machine.
#
# THE INVARIANT: an installed version is immutable and identified by the commit
# it was built from, and `current` is a symlink. That is what makes an update a
# pointer flip and a rollback the same flip backwards, so dogfooding a bad
# build never leaves the machine without a working binary.
#
#   scripts/install-local.sh              build release, install, make current
#   scripts/install-local.sh --list       what is installed, and which is live
#   scripts/install-local.sh --rollback   point `current` at the previous version
#   scripts/install-local.sh --uninstall  remove the PATH entries and the tree
#
# NO CERTIFICATE IS INVOLVED. On this machine the linker ad-hoc signs every
# binary it produces and nothing carries `com.apple.quarantine`, because
# nothing was downloaded. Code-signing certificates buy Gatekeeper and
# SmartScreen trust for binaries OTHER people download; they have no part in
# installing your own build on your own laptop.
set -euo pipefail

# THE BINARIES THIS PROJECT SHIPS, in one place.
#
# `inference-gateway` was missing from every one of the four copies of this
# list that used to be spelled out below, while `$BINDIR/inference-gateway`
# was a symlink into `current/bin/` all the same. So every install flipped
# `current` to a version directory that did not contain it and left that link
# dangling -- and `pane`, which reaches the gateway as a sibling executable,
# lost its whole model catalogue. Measured 2026-09-11, on this machine.
BINARIES=(inference-gateway pane)

PREFIX="${GLASSHOUSE_PREFIX:-$HOME/.local}"
ROOT="$PREFIX/lib/glasshouse"
VERSIONS="$ROOT/versions"
CURRENT="$ROOT/current"
BINDIR="$PREFIX/bin"
PROFILE=release
ALLOW_DIRTY=0
ACTION=install

die() { echo "install-local: $*" >&2; exit 1; }

while [ $# -gt 0 ]; do
  case "$1" in
    --list)       ACTION=list ;;
    --rollback)   ACTION=rollback ;;
    --uninstall)  ACTION=uninstall ;;
    --allow-dirty) ALLOW_DIRTY=1 ;;
    --debug)      PROFILE=debug ;;
    --prefix)     shift; PREFIX="${1:?--prefix needs a path}"
                  ROOT="$PREFIX/lib/glasshouse"; VERSIONS="$ROOT/versions"
                  CURRENT="$ROOT/current"; BINDIR="$PREFIX/bin" ;;
    -h|--help)    sed -n '2,20p' "$0"; exit 0 ;;
    *)            die "unknown argument: $1" ;;
  esac
  shift
done

# The repository is the one this script lives in. Unlike the dev shim, an
# install is not about "the checkout you are standing in": it produces the
# artifact you will run everywhere else.
REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
[ -f "$REPO/crates/pane/Cargo.toml" ] || die "not a Pane checkout: $REPO"

live_version() { [ -L "$CURRENT" ] && basename "$(readlink "$CURRENT")" || true; }

# Repoint a symlink onto an existing symlink, atomically.
#
# NOT `mv`: BSD mv stats the destination, follows `current` to the directory it
# names, and deposits the temp link INSIDE the old version -- which leaves
# `current` still pointing at the previous build while the script reports a
# successful flip. rename(2) operates on the link itself and never follows it.
# Measured on 2026-09-08, on this script's own second install.
point_current_at() {
  python3 -c 'import os,sys
target, link = sys.argv[1], sys.argv[2]
tmp = link + ".tmp"
try: os.remove(tmp)
except FileNotFoundError: pass
os.symlink(target, tmp)
os.replace(tmp, link)' "$1" "$CURRENT"
}

case "$ACTION" in
list)
  [ -d "$VERSIONS" ] || die "nothing installed under $ROOT"
  live="$(live_version)"
  for d in "$VERSIONS"/*/; do
    [ -d "$d" ] || continue
    v="$(basename "$d")"
    built="$(python3 -c "import json;print(json.load(open('$d/manifest.json'))['built_at'])" 2>/dev/null || echo '?')"
    printf '%s  %-40s %s\n' "$([ "$v" = "$live" ] && echo '*' || echo ' ')" "$v" "$built"
  done
  exit 0 ;;
uninstall)
  for l in "${BINARIES[@]}"; do
    [ -L "$BINDIR/$l" ] && rm -f "$BINDIR/$l"
  done
  rm -rf "$ROOT"
  echo "removed $ROOT and the ${BINARIES[*]} links in $BINDIR"
  exit 0 ;;
rollback)
  live="$(live_version)"
  [ -n "$live" ] || die "nothing is current; nothing to roll back to"
  prev="$(for d in "$VERSIONS"/*/; do
            v="$(basename "$d")"; [ "$v" = "$live" ] && continue
            printf '%s\t%s\n' "$(python3 -c "import json;print(json.load(open('$d/manifest.json'))['built_at'])" 2>/dev/null || echo 0)" "$v"
          done | sort -r | head -1 | cut -f2)"
  [ -n "$prev" ] || die "only $live is installed; nothing to roll back to"
  point_current_at "$VERSIONS/$prev"
  echo "current: $live -> $prev"
  exit 0 ;;
esac

# ---- install -------------------------------------------------------------

if [ "$ALLOW_DIRTY" -eq 0 ] && [ -n "$(git -C "$REPO" status --porcelain)" ]; then
  die "working tree is dirty; an installed binary must map to a commit (--allow-dirty to override)"
fi

VERSION="$(git -C "$REPO" describe --tags --always --dirty 2>/dev/null || echo unknown)"
COMMIT="$(git -C "$REPO" rev-parse HEAD 2>/dev/null || echo unknown)"
DEST="$VERSIONS/$VERSION"

# `pane` is not in default-members (it carries an embedded V8), so both crates
# are named explicitly. RUSTFLAGS is never set here -- see Cargo.toml's
# [workspace.lints] note; a second value would fork every fingerprint in target/.
# shellcheck source=scripts/lib/accel.sh
. "$REPO/scripts/lib/accel.sh"
echo "building $VERSION ($PROFILE) ..."
BUILD_FLAGS=(--profile "$PROFILE")
[ "$PROFILE" = debug ] && BUILD_FLAGS=()
( cd "$REPO" && cargo build "${BUILD_FLAGS[@]}" -p inference-gateway -p pane )

BUILT="$REPO/target/$PROFILE"
for b in "${BINARIES[@]}"; do
  [ -x "$BUILT/$b" ] || die "build produced no $BUILT/$b"
done

# Smoke the artifacts BEFORE anything becomes current: an install that cannot
# print its own version is not one to point `current` at.
for b in "${BINARIES[@]}"; do
  "$BUILT/$b" --version >/dev/null 2>&1 || die "$b does not run; refusing to install"
done

rm -rf "$DEST"
mkdir -p "$DEST/bin"
for b in "${BINARIES[@]}"; do
  cp "$BUILT/$b" "$DEST/bin/$b"
done

python3 - "$DEST" "$VERSION" "$COMMIT" "$PROFILE" <<'PY'
import hashlib, json, pathlib, subprocess, sys, datetime
dest, version, commit, profile = sys.argv[1:5]
d = pathlib.Path(dest)
def sha(p): return hashlib.sha256(p.read_bytes()).hexdigest()
manifest = {
    "version": version,
    "commit": commit,
    "profile": profile,
    "built_at": datetime.datetime.now(datetime.timezone.utc).isoformat(),
    "toolchain": subprocess.run(["rustc", "--version"], capture_output=True, text=True).stdout.strip(),
    "binaries": {b.name: {"sha256": sha(b), "bytes": b.stat().st_size}
                 for b in sorted((d / "bin").iterdir())},
}
(d / "manifest.json").write_text(json.dumps(manifest, indent=2) + "\n")
PY

previous="$(live_version)"
mkdir -p "$BINDIR"
point_current_at "$DEST"
for b in "${BINARIES[@]}"; do
  ln -sfn "$CURRENT/bin/$b" "$BINDIR/$b"
done

echo
echo "installed $VERSION -> $DEST"
[ -n "$previous" ] && [ "$previous" != "$VERSION" ] && echo "current:  $previous -> $VERSION"
echo "pane:       $BINDIR/pane"
case ":$PATH:" in
  *":$BINDIR:"*) ;;
  *) echo; echo "NOTE: $BINDIR is not on PATH. Add it to your shell profile." ;;
esac
