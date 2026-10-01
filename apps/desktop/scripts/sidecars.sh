#!/usr/bin/env bash
# Copies the engine binaries into apps/desktop/src-tauri/binaries/ under the
# names Tauri's `bundle.externalBin` expects, <name>-<target-triple> with
# .exe on Windows. `tauri build` and `tauri dev` refuse to start without them.
#
#   sterna            -> sterna-desktop-engine-<triple>
#   inference-gateway -> sterna-desktop-gateway-<triple>
#
# The names are the app's own so a Linux package never installs a generic
# /usr/bin/sterna or /usr/bin/inference-gateway beside the command line's.
#
#   bash apps/desktop/scripts/sidecars.sh [--profile release|debug]
#                                         [--target <triple>] [--from <dir>]
#
# --profile  which cargo profile to copy from. Default: release when both
#            binaries exist in target/release, else debug.
# --target   the target triple (default: the host's, from `rustc -vV`); also
#            makes the source target/<triple>/<profile>.
# --from     copy from this directory instead of the cargo target directory.
#
# The files are copied, never linked, so the bundler embeds real binaries.
# Nothing under target/ is modified. Works on macOS, Linux and Git Bash.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo="$(cd "$here/../../.." && pwd)"
dest="$repo/apps/desktop/src-tauri/binaries"

profile=""
target=""
from=""
while [ $# -gt 0 ]; do
  case "$1" in
    --profile)
      profile="${2:-}"
      shift 2 || { echo "sidecars.sh: --profile needs release or debug" >&2; exit 2; }
      ;;
    --target)
      target="${2:-}"
      shift 2 || { echo "sidecars.sh: --target needs a target triple" >&2; exit 2; }
      ;;
    --from)
      from="${2:-}"
      shift 2 || { echo "sidecars.sh: --from needs a directory" >&2; exit 2; }
      ;;
    -h | --help)
      sed -n '2,23p' "$0" | sed 's/^# \{0,1\}//'
      exit 0
      ;;
    *)
      echo "sidecars.sh: unknown option $1 (see --help)" >&2
      exit 2
      ;;
  esac
done

case "$profile" in
  "" | release | debug) ;;
  *)
    echo "sidecars.sh: --profile must be release or debug, not $profile" >&2
    exit 2
    ;;
esac

host="$(rustc -vV | sed -n 's/^host: //p')"
if [ -z "$host" ]; then
  echo "sidecars.sh: could not read the host triple from 'rustc -vV'" >&2
  exit 1
fi
triple="${target:-$host}"

exe=""
case "$triple" in
  *-windows-*) exe=".exe" ;;
esac

# The cargo target directory: CARGO_TARGET_DIR when set, else <repo>/target.
target_root="${CARGO_TARGET_DIR:-$repo/target}"
case "$target_root" in
  /* | [A-Za-z]:*) ;;
  *) target_root="$repo/$target_root" ;;
esac

profile_dir() {
  if [ -n "$target" ]; then
    echo "$target_root/$target/$1"
  else
    echo "$target_root/$1"
  fi
}

if [ -n "$from" ]; then
  src="$from"
  build_hint="put sterna$exe and inference-gateway$exe in $from"
else
  chosen="$profile"
  if [ -z "$profile" ]; then
    release="$(profile_dir release)"
    if [ -f "$release/sterna$exe" ] && [ -f "$release/inference-gateway$exe" ]; then
      profile="release"
    else
      profile="debug"
    fi
  fi
  src="$(profile_dir "$profile")"
  # With no --profile and neither build present, the release build is the
  # one to make: it is what a bundle ships.
  flags="-p sterna -p inference-gateway"
  if [ "$profile" = "release" ] || [ -z "$chosen" ]; then
    flags="--release $flags"
  fi
  if [ -n "$target" ]; then
    flags="$flags --target $target"
  fi
  build_hint="build it with: cargo build $flags"
  if [ -z "$chosen" ]; then
    build_hint="there is no release build either; $build_hint"
  fi
fi

for name in sterna inference-gateway; do
  if [ ! -f "$src/$name$exe" ]; then
    echo "sidecars.sh: $src/$name$exe does not exist; $build_hint" >&2
    exit 1
  fi
done

mkdir -p "$dest"
for name in sterna inference-gateway; do
  case "$name" in
    sterna) as="sterna-desktop-engine" ;;
    inference-gateway) as="sterna-desktop-gateway" ;;
  esac
  out="$dest/$as-$triple$exe"
  # Remove first: a running copy is replaced rather than written over in place.
  rm -f "$out"
  cp "$src/$name$exe" "$out"
  chmod +x "$out"
  echo "copied $src/$name$exe -> $out"
done
