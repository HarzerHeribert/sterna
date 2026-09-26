#!/usr/bin/env bash
# Run the CI matrix locally, because GitHub Actions minutes are exhausted.
#
# WHY THIS EXISTS
# ---------------
# From 2026-08-26 the Actions quota for this private repository is spent until
# it resets. A push still creates a run; every job fails instantly with no
# steps and no log, which looks exactly like a broken build and is not one.
# Until the quota returns, THIS script is the gate. Run it before every commit.
#
# It mirrors .github/workflows/ci-extended.yml deliberately and closely —
# `--locked`, clippy without `--all-features` — because a local gate that tests
# something easier than CI is not a gate, it is a rehearsal. If you change the
# workflow, change this in the same commit.
#
# Warnings are denied by [workspace.lints.rust] in Cargo.toml rather than by
# RUSTFLAGS here, so every invocation shares one fingerprint namespace --
# see that file for the measurement that forced it.
#
# WHAT IT COVERS, HONESTLY
#   lint            ubuntu   -> Linux container
#   test / msrv     ubuntu   -> Linux container
#   test / msrv     macOS    -> this machine, natively
#   test            WINDOWS  -> the GitHub sweep's windows cells; --windows only compiles.
#
# Nothing in the default run is evidence about Windows. `--windows` adds a
# cross-compile check of the gateway, which proves the Windows code path still
# *compiles* and nothing about whether it works; the GitHub sweep's windows
# cells run it for real.
#
# HOW LONG IT TAKES, AND WHY (measured 2026-08-29, 12-core M-series)
#   Warm, the whole default gate is ~2–4 minutes and nearly all of it is test
#   execution: ~80s per platform, of which terminal_loss (24s) and
#   session_supervision (14s) are timer-bound — identical to 0.1s at load 2.6
#   and load 5.5. Every compile step is 0–2s warm. A fresh worktree with no
#   target/ and no Linux volume is a full rebuild on both sides and still
#   finished in 135s. Seeding a worktree's caches from main (APFS clone of
#   target/, copy of the Linux volume) was built, proven sound with a planted
#   failure that FAILed the same test cold and seeded, and then rejected: it
#   cost 88s to seed and saved 13s. Do not re-try it without new numbers.
#
# USAGE
#   scripts/ci-local.sh              # macOS + Linux  (the default gate)
#   scripts/ci-local.sh --scoped     # fast tier: lints + blast radius, macOS only
#   scripts/ci-local.sh --macos      # native jobs only, fastest
#   scripts/ci-local.sh --linux      # container jobs only
#   scripts/ci-local.sh --windows    # add the compile-only cross check
set -uo pipefail

ORIG_CWD="$(pwd)"
REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

# $REPO above is the SCRIPT's own location, not necessarily the CALLER's
# tree: scripts/ is tracked, so every worktree has its own copy, and this is
# THE gate to run before every commit -- it must build and test the caller's
# OWN tree, not whichever copy happens to be invoked. Reproduced 2026-08-30
# (script-tree-audit): run via absolute path from a worktree -- the exact
# form CLAUDE.md itself recommends for other scripts in this project, to
# route around a DIFFERENT worktree-resolution bug -- this cd'd into and
# would have tested the main checkout instead, silently. Same shape and same
# fix as scripts/blast-radius.sh.
common_dir() {
  local d
  d="$(git -C "$1" rev-parse --git-common-dir 2>/dev/null)" || return 1
  case "$d" in
    /*) printf '%s\n' "$d" ;;
    *)  (cd "$1/$d" 2>/dev/null && pwd -P) ;;
  esac
}

CALLER_TOPLEVEL="$(git -C "$ORIG_CWD" rev-parse --show-toplevel 2>/dev/null)"
if [ -n "$CALLER_TOPLEVEL" ]; then
  REPO_TOPLEVEL="$(git -C "$REPO" rev-parse --show-toplevel 2>/dev/null)"
  if [ "$CALLER_TOPLEVEL" != "$REPO_TOPLEVEL" ]; then
    REPO_COMMON="$(common_dir "$REPO")"
    CALLER_COMMON="$(common_dir "$CALLER_TOPLEVEL")"
    if [ -n "$REPO_COMMON" ] && [ "$REPO_COMMON" = "$CALLER_COMMON" ]; then
      echo "ci-local: testing the caller's worktree at $CALLER_TOPLEVEL (not $REPO)"
      REPO="$CALLER_TOPLEVEL"
    fi
  fi
fi

cd "$REPO" || exit 1

# Compiler cache, if this machine has one. No-ops otherwise -- see the file's
# own header for why this is sourced rather than set in .cargo/config.toml.
# shellcheck source=scripts/lib/accel.sh
. "$REPO/scripts/lib/accel.sh"

DO_MAC=0; DO_LINUX=0; DO_WIN=0; SCOPED=0
if [ $# -eq 0 ]; then DO_MAC=1; DO_LINUX=1; fi
for a in "$@"; do
  case "$a" in
    --macos)   DO_MAC=1 ;;
    --linux)   DO_LINUX=1 ;;
    --windows) DO_WIN=1 ;;
    --scoped)  SCOPED=1 ;;
    --all)     DO_MAC=1; DO_LINUX=1; DO_WIN=1 ;;
    *) echo "unknown option: $a" >&2; exit 2 ;;
  esac
done

# --scoped is a TIER, not a mode of the gate, and the two must never be
# confused. The full run's contract is that it mirrors ci.yml closely enough
# that passing it predicts passing CI; a run that chose its targets from a
# diff cannot make that claim about anything the diff did not touch. So it is
# refused wherever the run would otherwise be presented as authoritative --
# a Linux or Windows leg is a platform claim, and there is no such thing as a
# platform claim about targets you did not build.
if [ "$SCOPED" -eq 1 ] && { [ "$DO_LINUX" -eq 1 ] || [ "$DO_WIN" -eq 1 ]; }; then
  echo "ci-local: --scoped is macOS-only -- it selects targets from the diff, which is not a platform claim." >&2
  echo "          Run 'scripts/ci-local.sh --scoped' for the fast tier, then the full gate before pushing." >&2
  exit 2
fi
# Bare `--scoped` means the macOS leg, not the default macOS+Linux pair.
if [ "$SCOPED" -eq 1 ]; then DO_MAC=1; DO_LINUX=0; fi

accel_enable

# A provider variable inherited from the CALLER fails the gate for a reason
# that has nothing to do with the tree. tests/pty_smoke.rs asserts that a
# launch overlay's ANTHROPIC_BASE_URL never leaks into the parent process --
# a Phase 46 contamination check, and correct. Claude Code exports exactly that
# variable into every child it spawns, so a gate run from inside a Claude Code
# session (every worker pane, and the orchestrator's own Bash tool) fails that
# one assertion deterministically while the same tree passes from a terminal
# and from the Linux container, whose environment is clean. Measured
# 2026-09-05: 75/76 with it set, 76/76 with `env -u ANTHROPIC_BASE_URL`.
#
# User ruling 2026-09-05 (.agent-runtime/answers/pty-smoke-env.txt): scrub
# these three from the gate's own cargo children, so a gate run from any pane
# is clean. This is not scrubbing the caller's shell -- ci-local.sh's own
# process still sees them, and this loop still warns -- it is that every
# cargo invocation below runs under `env -u`, so the *test process* pty_smoke
# inspects never inherits them, matching the Linux container's environment.
ENV_SCRUB=(env -u ANTHROPIC_BASE_URL -u ANTHROPIC_AUTH_TOKEN -u ANTHROPIC_API_KEY)
for leaked in ANTHROPIC_BASE_URL ANTHROPIC_AUTH_TOKEN ANTHROPIC_API_KEY; do
  if [ -n "${!leaked:-}" ]; then
    echo "ci-local: NOTE -- $leaked is set in this environment (inherited from the caller)." >&2
    echo "          it is scrubbed for the gate's own cargo children below, so pty_smoke's" >&2
    echo "          overlay-leak assertion sees a clean environment either way." >&2
  fi
done

MSRV="$(grep -m1 '^rust-version' Cargo.toml | sed 's/.*"\(.*\)".*/\1/')"
[ -n "$MSRV" ] || { echo "could not read rust-version from Cargo.toml" >&2; exit 2; }
# The stable compiler, declared once in [workspace.metadata.ci] and read by
# ci.yml the same way. It pins the Linux image below: `rust:latest` floated,
# and every bump silently threw away the whole Linux build cache.
TOOLCHAIN="$(grep -m1 '^toolchain' Cargo.toml | sed 's/.*"\(.*\)".*/\1/')"
[ -n "$TOOLCHAIN" ] || { echo "could not read [workspace.metadata.ci] toolchain from Cargo.toml" >&2; exit 2; }

# ...and now actually USE it on this machine, which until 2026-09-04 it did not.
# $TOOLCHAIN pinned the Linux image and nothing else, so the container built
# with 1.98.0 while the macOS leg beside it built with whatever `cargo` PATH
# happened to name -- a Homebrew rust 1.96.1, with rustup's own `stable` a
# staler 1.94.0 behind it. Three compilers, one declared version, and the
# "declared once, cannot drift" guarantee stopping at the container boundary.
# Two costs: the native leg was not testing what CI tests, and rustc's version
# is part of cargo's fingerprint, so each compiler kept invalidating the others'
# artifacts in one target/.
#
# Prepending the toolchain's own bin directory fixes both, and fixes the trap
# scripts/msrv-check.sh documents at its head: `rustup run <v> cargo` is not
# enough because cargo then resolves `rustc` from PATH, where a Homebrew rustc
# silently wins. Both binaries live in this one directory, so both are pinned.
# msrv-check.sh is unaffected -- it resolves absolute paths per toolchain itself.
if command -v rustup >/dev/null 2>&1; then
  TOOLCHAIN_BIN="$(dirname "$(rustup which --toolchain "$TOOLCHAIN" cargo 2>/dev/null)" 2>/dev/null)"
  if [ -n "$TOOLCHAIN_BIN" ] && [ -x "$TOOLCHAIN_BIN/cargo" ]; then
    PATH="$TOOLCHAIN_BIN:$PATH"; export PATH
    echo "ci-local: using the declared toolchain $TOOLCHAIN from $TOOLCHAIN_BIN"
  else
    # Loud, not silent: building with an undeclared compiler is the defect this
    # block exists to remove, so it must never be what happens by accident.
    echo "ci-local: WARNING -- declared toolchain $TOOLCHAIN is not installed." >&2
    echo "          Native jobs will build with $(cargo -V 2>/dev/null), which is NOT what CI runs." >&2
    echo "          Fix: rustup toolchain install $TOOLCHAIN --profile minimal --component clippy rustfmt" >&2
  fi
fi

if ! scripts/install-git-hooks.sh --check >/dev/null 2>&1; then
  echo "WARNING: core.hooksPath is not scripts/git-hooks -- run scripts/install-git-hooks.sh" >&2
fi

RESULTS=()
FAILED=0
# Whether Windows was actually exercised, and how. Both feed the closing NOTE,
# which must never claim more or less than the run earned.
WIN_CROSS_RAN=0

step() {           # step <label> <command...>
  local label="$1"; shift
  printf '\n\033[1m=== %s\033[0m\n' "$label"
  if "$@"; then
    RESULTS+=("PASS  $label")
  else
    RESULTS+=("FAIL  $label")
    FAILED=1
  fi
}

# --- native macOS jobs -------------------------------------------------------
if [ "$DO_MAC" -eq 1 ]; then
  step "lint / fmt" "${ENV_SCRUB[@]}" cargo fmt --all -- --check
  step "lint / clippy" "${ENV_SCRUB[@]}" cargo clippy --locked --workspace --all-targets -- -D warnings
  step "lint / rustdoc" "${ENV_SCRUB[@]}" env RUSTDOCFLAGS='-D warnings' cargo doc -p inference-gateway --no-deps
  step "lint / file sizes"      python3 scripts/check-file-sizes.py
  step "lint / secrets"         python3 scripts/check-secrets.py --tree
  # The gate's own scripts have tests: cheap to break, expensive to have wrong.
  step "lint / script tests" sh -c 'for t in scripts/tests/test_*.py; do python3 "$t" || exit 1; done'
  # The two heaviest steps in the whole gate, and the reason --scoped exists.
  # `--all-targets` is 160 separate integration-test crates: 160 compilations
  # and 160 link steps against the whole library, every one of them redone
  # when any library file changes, whether or not the diff can reach them.
  # `--workspace` then RUNS all of them, including the process-spawning ones
  # that sleep through deliberate health windows.
  #
  # The scoped tier hands both jobs to scripts/blast-radius.sh, which already
  # knows how to trace a changed file to the targets that can observe it and
  # which of those must run serially. It is a different question -- "did I
  # break what I touched" rather than "does this tree pass CI" -- and the
  # summary says so rather than letting a fast pass read as the real one.
  if [ "$SCOPED" -eq 1 ]; then
    step "test (macos) / targeted blast radius" scripts/blast-radius.sh --targeted
  else
    step "test (macos) / gateway build" "${ENV_SCRUB[@]}" cargo build --locked -p inference-gateway --all-targets
    step "test (macos) / gateway test"  env -u ANTHROPIC_BASE_URL -u ANTHROPIC_AUTH_TOKEN -u ANTHROPIC_API_KEY sh -c 'cargo test --locked -p inference-gateway -- --nocapture < /dev/null'
  fi
  # `pane` gets its own step, unconditionally: --scoped need not skip it.
  step "test (macos) / pane build+test" env -u ANTHROPIC_BASE_URL -u ANTHROPIC_AUTH_TOKEN -u ANTHROPIC_API_KEY sh -c 'cargo build --locked -p pane --all-targets && cargo test --locked -p pane -- --nocapture < /dev/null'
  # Call the project's own script rather than `cargo +$MSRV`: its header
  # documents three traps, and `cargo +<v>` needs the rustup shim, which is
  # exactly how the first version of this file got a false red.
  step "msrv (macos) $MSRV" scripts/msrv-check.sh
fi

# --- Linux jobs, in a container ---------------------------------------------
# No env -u prefix is needed here: `docker run` does not forward host
# environment variables unless a bare `-e NAME` names them, and run_linux()
# below only ever passes `-e CARGO_TERM_COLOR` and `-e STEP`. ANTHROPIC_BASE_URL
# and friends never reach the container regardless of the caller's shell.
if [ "$DO_LINUX" -eq 1 ]; then
  if ! docker info >/dev/null 2>&1; then
    RESULTS+=("SKIP  linux jobs — Docker is not running")
  else
    # A separate CARGO_TARGET_DIR inside the container: Linux artifacts must
    # never land in the host's target/, and a shared one would make every
    # local build recompile the world in both directions.
    # Volumes are keyed to THIS worktree. One shared pair was wrong twice
    # over: two team leads running this concurrently raced on the same
    # /home/ci, and a lead's worktree left files behind that then compiled
    # into main's build. A build cache may be shared; a source tree may not.
    TAG="$(printf '%s' "$REPO" | shasum | cut -c1-12)"
    docker volume create "glasshouse-ci-home-$TAG" >/dev/null 2>&1
    docker volume create glasshouse-ci-registry >/dev/null 2>&1
    # rustup's home, so clippy/rustfmt and the MSRV toolchain install once
    # rather than on every run (measured ~10s and a download each). Keyed by
    # the pinned toolchain: an empty named volume is seeded from the image on
    # first mount, and a volume seeded from rust:1.98 would otherwise keep
    # serving 1.98 as "default" under a later image. Toolchains are not
    # source-dependent, so unlike /home/ci this one is shared by every worktree.
    docker volume create "glasshouse-ci-rustup-$TOOLCHAIN" >/dev/null 2>&1
    # Two things here are not incidental; both were found by this script
    # producing a red on a tree that real ubuntu-latest had passed.
    #
    #  1. The tree is COPIED in, not bind-mounted. A test that writes an
    #     executable and immediately spawns it gets ETXTBSY ("Text file busy")
    #     across a macOS->Linux bind mount, which looks like a product defect
    #     and is not one.
    #  2. The build runs as a NON-ROOT user. `chmod 000` does not stop root,
    #     so a test asserting a directory cannot be listed passes vacuously —
    #     and one of this project's tests says so in its own failure message.
    #
    # target/ is excluded from the copy; it holds macOS artifacts and is large.
    run_linux() {
      docker run --rm \
        -v "$REPO":/src:ro \
        -v "glasshouse-ci-home-$TAG":/home/ci \
        -v glasshouse-ci-registry:/usr/local/cargo/registry \
        -v "glasshouse-ci-rustup-$TOOLCHAIN":/usr/local/rustup \
        -e CARGO_TERM_COLOR=always \
        -e STEP="$1" \
        "rust:$TOOLCHAIN" bash -c '
          set -e
          # The Linux Secret Service backend (Phase 9E line 442) links libdbus
          # through libdbus-sys, whose build script needs the dev headers and
          # pkg-config on the build host -- accepted by the user 2026-09-05.
          # gnome-keyring and libsecret-tools are the CI fixture provider
          # and its proof CLI (GH-SECRET-SERVICE-CI-FIXTURE); dbus is
          # dbus-run-session, which the fixture uses for a private bus.
          # Root here, before the ci user exists; the image ships none of it.
          apt-get update -q >/dev/null && apt-get install -y -q libdbus-1-dev pkg-config dbus gnome-keyring libsecret-tools >/dev/null
          id -u ci >/dev/null 2>&1 || useradd -m -u 1000 ci
          # Wipe before extracting. `tar -x` writes over a tree, it never
          # removes what is no longer in the source — so a file deleted (or
          # belonging to a different worktree) survives and compiles. That is
          # how tests/checkpoint_portability.rs from another branch broke a
          # build of main, and it could as easily have hidden a failure.
          # /home/ci/target is deliberately NOT wiped: it is the build cache.
          rm -rf /home/ci/repo
          mkdir -p /home/ci/repo
          tar -C /src --exclude=./target --exclude=./.worktrees -cf - . | tar -C /home/ci/repo -xf -
          chown -R ci:ci /home/ci
          # rustup/cargo homes are root-owned in the image; the msrv step
          # installs a toolchain and must be able to write them.
          chown -R ci:ci /usr/local/rustup /usr/local/cargo
          # The step arrives as $STEP in the environment and is never
          # interpolated into a quoted string. It used to be nested inside
          # su -c "…$1…", and a step containing RUSTFLAGS="-D warnings" closed
          # that string early: the command was mangled and its exit status
          # meaningless, so `test (ubuntu)` reported PASS on a tree that had
          # just failed by hand. A gate that cannot fail is not a gate (§20).
          runuser -u ci -- bash -c '"'"'cd /home/ci/repo && export CARGO_TARGET_DIR=/home/ci/target && eval "$STEP"'"'"'
        '
    }
    step "test (ubuntu) / build+test" run_linux \
      'set -e; rustup component add clippy rustfmt >/dev/null 2>&1 || true;
       cargo build --locked -p inference-gateway --all-targets;
       . scripts/lib/secret-service-fixture.sh;
       secret_service_fixture_run cargo test --locked -p inference-gateway -- --nocapture < /dev/null'
    step "lint (ubuntu) / clippy" run_linux \
      'set -e; rustup component add clippy >/dev/null 2>&1 || true;
       cargo clippy --locked -p inference-gateway --all-targets -- -D warnings'
    step "msrv (ubuntu) $MSRV" run_linux \
      "rustup toolchain install $MSRV --profile minimal && scripts/msrv-check.sh"
  fi
fi

# --- Windows: compile-only, and labelled as such -----------------------------
if [ "$DO_WIN" -eq 1 ]; then
  TARGET=x86_64-pc-windows-gnu
  if rustup target list --installed | grep -q "$TARGET"; then
    WIN_CROSS_RAN=1
    # The toolchain's OWN cargo and rustc, pinned by sysroot -- not `rustup run
    # stable cargo`, and not bare `cargo`. The guard above asks rustup whether
    # the target is installed, so the step must use the toolchain rustup
    # answered about; but `rustup run stable` only puts the toolchain's cargo
    # first, and cargo then shells out to whichever `rustc` is on PATH -- on
    # this host Homebrew's, whose sysroot has no windows-gnu std, so the step
    # still failed with E0463 "can't find crate for core" after the first fix
    # (GH-WINDOWS-EXIT-OBSERVATION, 2026-09-02, measured: `rustup run stable
    # sh -c 'command -v rustc'` printed /opt/homebrew/bin/rustc). Pinning
    # RUSTC beside cargo is what makes the target's std visible.
    WIN_TC="$(rustup run stable rustc --print sysroot)/bin"
    step "windows CROSS-CHECK (compiles only, proves nothing about behaviour)" \
      env RUSTC="$WIN_TC/rustc" "$WIN_TC/cargo" check --locked -p inference-gateway --target "$TARGET"
    # The earlier note, kept for the history it records: bare `cargo` is
    # Homebrew's, and E0463 reads like a broken dependency (GH-WINDOWS-TEST-BUILD,
    # 2026-09-02).
  else
    RESULTS+=("SKIP  windows cross-check — rustup target add $TARGET (and brew install mingw-w64)")
  fi
fi

printf '\n\033[1m=== summary ===\033[0m\n'
printf '%s\n' "${RESULTS[@]}"
accel_report
# Three different true statements, and the run picks the one it earned. The
# old version keyed on `--windows` alone, so `--windows-vm` could run the
# whole Windows suite on a real machine and still be told Windows was not
# exercised at all — and a `--windows` whose target was not installed was
# told the opposite.
if [ "$SCOPED" -eq 1 ]; then
  printf '\n\033[33mNOTE\033[0m  SCOPED run. Targets were selected from the diff, so this is evidence about\n'
  printf '      what you changed and about nothing else. It is not a CI prediction and it\n'
  printf '      does not replace the pre-push gate: run scripts/ci-local.sh with no flags.\n'
fi
if [ "$WIN_CROSS_RAN" -eq 1 ]; then
  printf '\n\033[33mNOTE\033[0m  The Windows check compiles the target; it does not run a single test there.\n'
else
  printf '\n\033[33mNOTE\033[0m  Windows was not exercised at all. Nothing here is evidence about Windows.\n'
fi
[ "$FAILED" -eq 0 ] || printf '\n\033[31mCI-LOCAL FAILED\033[0m\n'
exit "$FAILED"
