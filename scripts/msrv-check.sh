#!/usr/bin/env bash
# Verify the workspace really builds at the minimum Rust version it declares.
#
# Three traps make the obvious command wrong, and this project hit all three:
#
#  1. `rustup run <v> cargo check` is NOT enough. rustup execs the toolchain's
#     cargo, but cargo then resolves `rustc` from PATH — and a Homebrew rustc
#     ahead of ~/.cargo/bin silently wins. The check then compiles with a
#     *current* rustc while appearing to test the floor. Both halves are pinned
#     below with `rustup which --toolchain`.
#  2. Cargo older than ~1.85.1 does not enforce `rust-version` at all. Running
#     the check with the floor toolchain's own cargo can therefore pass a
#     workspace no current cargo will build. Cargo 1.88's does enforce, proven
#     by mutation: raising the declared floor above the toolchain makes this
#     script fail.
#  3. Sharing `target/` with the stable build makes the check a no-op that
#     prints "Finished in 0.39s" and proves nothing. It gets its own directory.
#
# The version is read from Cargo.toml, never repeated here, so this script and
# the manifest cannot drift. CI's `msrv` job reads it the same way.
set -euo pipefail

ORIG_CWD="$(pwd)"
REPO="$(cd "$(dirname "${BASH_SOURCE[0]:-$0}")/.." && pwd)"

# $REPO above is the SCRIPT's own location, not necessarily the CALLER's
# tree: scripts/ is tracked, so every worktree has its own copy, and this
# must build the caller's OWN Cargo.toml/workspace, not whichever copy
# happens to be invoked. Reproduced 2026-08-30 (script-tree-audit): run via
# absolute path from a worktree, this `cd`'d into the main checkout instead.
# Same shape and same fix as scripts/blast-radius.sh.
common_dir() {
  local d
  d="$(git -C "$1" rev-parse --git-common-dir 2>/dev/null)" || return 1
  case "$d" in
    /*) printf '%s\n' "$d" ;;
    *)  (cd "$1/$d" 2>/dev/null && pwd -P) ;;
  esac
}

CALLER_TOPLEVEL="$(git -C "$ORIG_CWD" rev-parse --show-toplevel 2>/dev/null || true)"
if [ -n "$CALLER_TOPLEVEL" ]; then
  REPO_TOPLEVEL="$(git -C "$REPO" rev-parse --show-toplevel 2>/dev/null || true)"
  if [ "$CALLER_TOPLEVEL" != "$REPO_TOPLEVEL" ]; then
    REPO_COMMON="$(common_dir "$REPO" || true)"
    CALLER_COMMON="$(common_dir "$CALLER_TOPLEVEL" || true)"
    if [ -n "$REPO_COMMON" ] && [ "$REPO_COMMON" = "$CALLER_COMMON" ]; then
      echo "msrv-check: checking the caller's worktree at $CALLER_TOPLEVEL (not $REPO)"
      REPO="$CALLER_TOPLEVEL"
    fi
  fi
fi

cd "$REPO"

VERSION="$(grep -m1 '^rust-version' Cargo.toml | sed 's/.*"\(.*\)".*/\1/')"
if [ -z "$VERSION" ]; then
  echo "msrv-check: no rust-version in Cargo.toml" >&2
  exit 1
fi

if ! rustup toolchain list | grep -q "^${VERSION}"; then
  echo "msrv-check: toolchain $VERSION is not installed." >&2
  echo "  rustup toolchain install $VERSION --profile minimal" >&2
  exit 1
fi

CARGO_BIN="$(rustup which --toolchain "$VERSION" cargo)"
RUSTC_BIN="$(rustup which --toolchain "$VERSION" rustc)"

echo "msrv-check: declared floor $VERSION"
echo "  cargo: $CARGO_BIN"
echo "  rustc: $RUSTC_BIN"

RUSTC="$RUSTC_BIN" \
CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-target/msrv}" \
  "$CARGO_BIN" check --locked -p inference-gateway --all-targets "$@"
