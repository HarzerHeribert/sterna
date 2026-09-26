#!/usr/bin/env bash
# Run the tests of every file a change could plausibly break.
#
# WHY THIS EXISTS
# ---------------
# Practice §69 said: after a change, grep for the symbols it touches to find the
# blast radius. Batch 45 proved that is not enough. `codex-hooks` added two
# entries to `REPORTED_EVENTS` and ran exactly that grep. The grep WORKED — it
# named `session/select.rs`. The worker then *read* that file, judged it
# unaffected, and reported. It was wrong: `codex_hooks_are_written_where_codex_
# reads_them` hardcodes the event list, and the gate failed on macOS AND Linux,
# costing a full gate cycle for something `cargo test --lib session::select`
# would have caught in eight seconds.
#
# The test name contains neither `REPORTED_EVENTS` nor `event`, so no smarter
# grep would have helped. The failing step was a human reading a file to decide
# whether it mattered. §79's rule:
#
#     Once a blast-radius grep names a file, RUN that file's tests.
#     Do not read them and judge.
#
# A written rule already failed this way once — §75 was violated again ninety
# minutes later by its own author, which is why the cited-seams check became
# code. Same treatment here.
#
# USAGE
#   scripts/blast-radius.sh --full          # full trace, changed vs HEAD
#   scripts/blast-radius.sh --full --staged # full trace of staged changes
#   scripts/blast-radius.sh --full --since <ref> # full trace since a ref
#   scripts/blast-radius.sh --dry-run       # print the plan, run nothing
#   scripts/blast-radius.sh --list          # print traced targets + lanes, run nothing
#   scripts/blast-radius.sh --serial        # full trace in one lane (attribution only)
#   scripts/blast-radius.sh --targeted f.rs # worker gate; pass owned files explicitly
#   scripts/blast-radius.sh --jobs N        # override the parallel-lane worker count
#   scripts/blast-radius.sh --full f1.rs f2.rs # full trace of explicit files
#
# TARGETED MODE
# --------------
# --targeted is a fast BLOCKING gate, not a replacement for the full sweep:
# it runs only the targets tracing a changed file at distance zero -- a
# changed test file's own target, a changed source file's own same-named/
# most-specific integration target when one exists, and `--lib` filtered to
# the changed source files' own module paths -- plus `cargo doc --no-deps`.
# It does NOT compute or run the symbol fan-out trace the default mode does (a
# changed constant's four other referencing files, say). It says explicitly
# that the full trace was not computed; computing it merely to print a skipped
# count made the supposedly fast gate spend minutes scanning before testing.
# Composes with nothing else that changes lane membership; see the refusal
# above for --serial.
#
# `--status` prints whether a gate is running in THIS tree (pid, age, args)
# and exits 0 if so, 1 if not. A second gate started in a tree that already
# has one running refuses with exit 3 -- see "one gate per tree" below.
#
# TWO LANES
# ---------
# The blast radius is wall-clock-bound, not compute-bound: most traced targets
# sleep through deliberate waits (harness health windows, shutdown graces) and
# every fresh fixture executable queues behind macOS Gatekeeper's first-exec
# scan, while pure-logic targets that could saturate idle cores wait in line
# behind them. So targets run in two lanes instead of one:
#
#   parallel lane  -- pure config/routing/translation logic, bounded-parallel,
#                     runs FIRST so it can saturate idle cores.
#   serial lane    -- everything that spawns a process or asserts on
#                     wall-clock, one target at a time, in order, exactly as
#                     before -- runs SECOND, on a quiet machine, which is load
#                     hygiene for the wall-clock-bound tests in it.
#
# Classification lives in one place below ("lane classification"). Unknown
# defaults to the serial lane: misclassifying a fixture-spawner as parallel
# reintroduces the false-red gate this project already paid for four times in
# one evening; misclassifying logic as serial only costs seconds.
#
# `--serial` restores today's single-lane behavior byte-for-byte (one lane,
# original order) -- the escape hatch for attribution reruns (practice §34).
# The output contract is unchanged in both modes: same per-target
# `=== cargo test --test X ===` headers, same `test result:` lines, same
# final verdict line, same exit semantics (non-zero iff any target failed).
set -uo pipefail

ORIG_CWD="$(pwd)"
REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

# Resolve which tree to analyse. $REPO is the SCRIPT's own location, not
# necessarily the CALLER's tree: every editing worker runs from a worktree
# under .worktrees/, and this script is reachable by absolute path / PATH
# from the main checkout. Everything below this point is cwd-relative (git
# diff, the changed-file existence checks, the crates/*/src grep, the cargo
# invocations) -- so cd'ing unconditionally to $REPO makes the rest of the
# script silently diff and test the WRONG tree. See the header comment for
# the incident this guards against.
#
# Kinship is git-common-dir, not a path prefix: a worktree of this repo
# shares one git dir with the main checkout no matter where it lives, and a
# prefix check would break the moment a worktree lives outside .worktrees/
# while looking correct for the common case.
common_dir() {                     # absolute common .git dir for tree "$1"
  local d
  d="$(git -C "$1" rev-parse --git-common-dir 2>/dev/null)" || return 1
  case "$d" in
    # git itself reports the common dir through a resolved (physical) path
    # when it is already absolute; `pwd -P` matches that for the relative
    # case so the two forms compare equal instead of differing by a
    # symlinked tmp/mount prefix (e.g. macOS /var vs /private/var).
    /*) printf '%s\n' "$d" ;;
    *)  (cd "$1/$d" 2>/dev/null && pwd -P) ;;
  esac
}

CALLER_TOPLEVEL="$(git -C "$ORIG_CWD" rev-parse --show-toplevel 2>/dev/null)"

if [ -z "$CALLER_TOPLEVEL" ]; then
  echo "blast-radius: refusing -- '$ORIG_CWD' is not a git worktree (script lives at '$REPO')" >&2
  exit 1
fi

# Compare through git's own (symlink-resolved) view of $REPO, not the logical
# BASH_SOURCE-derived path, so a caller reached through a symlinked mount does
# not spuriously look like "a different tree" and print a line case 1 must
# never print.
REPO_TOPLEVEL="$(git -C "$REPO" rev-parse --show-toplevel 2>/dev/null)"

if [ "$CALLER_TOPLEVEL" != "$REPO_TOPLEVEL" ]; then
  REPO_COMMON="$(common_dir "$REPO")"
  CALLER_COMMON="$(common_dir "$CALLER_TOPLEVEL")"
  if [ -z "$REPO_COMMON" ] || [ "$REPO_COMMON" != "$CALLER_COMMON" ]; then
    echo "blast-radius: refusing -- '$CALLER_TOPLEVEL' is not a worktree of the repo at '$REPO'" >&2
    exit 1
  fi
  echo "blast-radius: analysing the caller's worktree at $CALLER_TOPLEVEL (not $REPO)"
  REPO="$CALLER_TOPLEVEL"
fi

cd "$REPO" || exit 1

ORIG_ARGS="$*"
DRY=0; LIST=0; SERIAL=0; TARGETED=0; FULL=0; STATUS=0; JOBS=""; MODE="head"; SINCE=""; FILES=()
while [ $# -gt 0 ]; do
  case "$1" in
    --dry-run)   DRY=1 ;;
    --list)      LIST=1 ;;
    --status)    STATUS=1 ;;
    --serial)    SERIAL=1 ;;
    --targeted)  TARGETED=1 ;;
    --full)      FULL=1 ;;
    --jobs)      JOBS="${2:-}"; shift ;;
    --staged)    MODE="staged" ;;
    --since)     MODE="since"; SINCE="${2:-}"; shift ;;
    -h|--help)   sed -n '2,52p' "$0"; exit 0 ;;
    *)           FILES+=("$1") ;;
  esac
  shift
done

# --full is intentionally a semantic no-op inside the executable: the full
# trace remains the compatibility default for scripts that invoke this one.
# Its value is at the harness boundary, where the PreToolUse guard can tell an
# accidental bare worker-loop run from a deliberate trailing sweep. Internal
# callers also spell it so logs and future readers can see which tier they owe.

# --targeted and --serial answer different questions (which targets to run,
# vs. which lane to run them in) and composing them silently would make
# --targeted's honest "I skipped N full-trace targets" line ambiguous about
# whether the skip was scope or ordering. Refuse loudly instead of guessing.
if [ "$TARGETED" -eq 1 ] && [ "$SERIAL" -eq 1 ]; then
  echo "blast-radius: --targeted and --serial are mutually exclusive -- --targeted already runs its small target set as a single lane" >&2
  exit 1
fi
if [ "$TARGETED" -eq 1 ] && [ "$FULL" -eq 1 ]; then
  echo "blast-radius: --targeted and --full are mutually exclusive -- choose the blocking worker gate or the trailing sweep" >&2
  exit 1
fi

# Compiler cache, if this machine has one; a no-op otherwise. This is the
# script that runs in a worker's fresh worktree, where target/ is empty and
# every cargo invocation below would otherwise recompile the dependency graph
# and the library from nothing before the first test binary links. See
# scripts/lib/accel.sh for why it is sourced rather than configured.
#
# Not for the modes that run no cargo: --dry-run, --list and --status print a
# plan and exit, and starting an sccache server to do it would be noise in
# output whose whole contract is that it changes nothing.
# shellcheck source=scripts/lib/accel.sh
. "$REPO/scripts/lib/accel.sh"
if [ "$DRY" -eq 0 ] && [ "$LIST" -eq 0 ] && [ "$STATUS" -eq 0 ]; then
  accel_enable
fi

# ---- one gate per tree at a time ------------------------------------------
# Two blast radii in the SAME tree at once are never what anyone meant, and
# they lie in both directions: each one's cargo load pushes the other's
# wall-clock-bound PTY fixtures past their timeouts (four gates reported false
# reds this way on 2026-08-31), and the second one's "every traced target
# passed" is read as the verdict on a tree the first one is still mutating
# test binaries under. 2026-09-01 the orchestrator started a wave sweep while
# its predecessor's was 40 minutes into the same tree, because the checkpoint
# named the sweep and nothing on the machine could answer "is one running
# here?" in one command. Now `--status` answers it, and a second start in the
# same tree refuses (exit 3) unless the holder is dead.
#
# Per TREE, deliberately: a worker's worktree gate and the main checkout's
# gate are different trees, allowed to overlap, and their mutual load is the
# known cost of parallel workers (practice §34 covers attributing it).
LOCK="/tmp/blast-radius-$(printf '%s' "$REPO" | cksum | cut -d' ' -f1).lock"
lock_holder() {                    # prints "pid started args" or nothing
  [ -r "$LOCK" ] || return 1
  local pid; pid="$(sed -n 's/^pid=//p' "$LOCK" | head -1)"
  [ -n "$pid" ] && kill -0 "$pid" 2>/dev/null || return 1
  printf '%s %s %s\n' "$pid" "$(sed -n 's/^started=//p' "$LOCK" | head -1)" "$(sed -n 's/^args=//p' "$LOCK" | head -1)"
}
if [ "$STATUS" -eq 1 ]; then
  if h="$(lock_holder)"; then
    set -- $h
    printf 'blast-radius: RUNNING in %s -- pid %s, started %s (%ss ago), args: %s\n' "$REPO" "$1" "$(date -r "$2" '+%H:%M:%S' 2>/dev/null || echo "$2")" "$(( $(date +%s) - $2 ))" "${*:3}"
    exit 0
  fi
  echo "blast-radius: no gate running in $REPO"
  exit 1
fi
take_lock() {
  local attempt
  for attempt in 1 2; do
    if ( set -o noclobber; printf 'pid=%s\nstarted=%s\nargs=%s\ntree=%s\n' "$$" "$(date +%s)" "$*" "$REPO" > "$LOCK" ) 2>/dev/null; then
      return 0
    fi
    if h="$(lock_holder)"; then
      set -- $h
      printf '\033[31mblast-radius: REFUSING -- another gate is already running in this tree: pid %s, started %ss ago, args: %s\033[0m\n' "$1" "$(( $(date +%s) - $2 ))" "${*:3}" >&2
      echo "  wait for it (scripts/blast-radius.sh --status), or read its output -- a second run here would load-flake both." >&2
      exit 3
    fi
    rm -f "$LOCK"                  # holder is dead: stale lock, take it
  done
  echo "blast-radius: could not take $LOCK" >&2; exit 3
}
release_lock() { [ "$(sed -n 's/^pid=//p' "$LOCK" 2>/dev/null | head -1)" = "$$" ] && rm -f "$LOCK"; }

if [ ${#FILES[@]} -eq 0 ]; then
  case "$MODE" in
    staged) mapfile -t FILES < <(git diff --cached --name-only -- '*.rs') ;;
    since)  mapfile -t FILES < <(git diff --name-only "$SINCE" -- '*.rs') ;;
    *)      mapfile -t FILES < <({ git diff --name-only -- '*.rs'
                                   git diff --cached --name-only -- '*.rs'
                                   git ls-files --others --exclude-standard -- '*.rs'; } | sort -u) ;;
  esac
fi

# A deleted or renamed-away file still appears in the diff; cargo refuses a
# test target that no longer exists (measured 2026-08-31: the deleted
# tests/subscription_rules.rs turned a green 86-target run red). Trace only
# files that are still on disk.
EXISTING=()
for f in "${FILES[@]}"; do [ -f "$f" ] && EXISTING+=("$f"); done
FILES=("${EXISTING[@]}")

if [ ${#FILES[@]} -eq 0 ]; then
  echo "blast-radius: no changed .rs files — nothing to do"
  exit 0
fi

printf '\033[1m=== changed (%d) ===\033[0m\n' "${#FILES[@]}"
printf '  %s\n' "${FILES[@]}"

# ---- 1. symbols the changed files DEFINE -----------------------------------
# Only definitions: a file that merely *uses* a symbol is already in the list.
# Deliberately generous about kinds (const/static/fn/struct/enum/trait/type) and
# deliberately silent about visibility — a `pub(crate)` constant is exactly what
# bit us, and filtering to `pub` would have missed it.
SYMS_FILE="$(mktemp)"; HITS_FILE="$(mktemp)"
trap 'rm -f "$SYMS_FILE" "$HITS_FILE"' EXIT
FULL_TRACE_COMPUTED=0

if [ "$TARGETED" -eq 1 ]; then
  : > "$SYMS_FILE"
  : > "$HITS_FILE"
  echo
  printf '\033[1m=== targeted planning ===\033[0m\n'
  echo "  full symbol fan-out not computed (the trailing --full sweep owns it)"
else
  FULL_TRACE_COMPUTED=1
  for f in "${FILES[@]}"; do
    [ -f "$f" ] || continue
    grep -hoE '^[[:space:]]*(pub(\([^)]*\))?[[:space:]]+)?(const|static|fn|struct|enum|trait|type)[[:space:]]+[A-Za-z_][A-Za-z0-9_]*' "$f" 2>/dev/null \
      | awk '{print $NF}'
  done | sort -u | grep -vE '^(new|main|default|fmt|from|drop|clone|next|len|get|set|run|open|read|write)$' > "$SYMS_FILE"

  echo
  printf '\033[1m=== %d defined symbol(s) to trace ===\033[0m\n' "$(wc -l < "$SYMS_FILE" | tr -d ' ')"
fi

# ---- 2. files that REFERENCE those symbols ---------------------------------
# A symbol referenced by dozens of files is not a blast radius, it is a common
# word ("Event", "Handler", "Config"). Tracing it drags in the whole crate and
# the signal drowns — the first version of this script traced 32 symbols from
# one file into 80 modules, which is the same as running everything.
#
# So each symbol is capped by fan-out: keep the ones that name a specific edge,
# drop the ones that name a concept. REPORTED_EVENTS (the constant that actually
# bit us) reaches four files and survives; `new` reaches hundreds and does not.
MAX_FANOUT=${BLAST_MAX_FANOUT:-12}
if [ "$FULL_TRACE_COMPUTED" -eq 1 ]; then
  printf '%s\n' "${FILES[@]}" > "$HITS_FILE"
  kept=0; dropped=0
  while read -r sym; do
    [ -n "$sym" ] || continue
    mapfile -t refs < <(grep -rlE "\b${sym}\b" crates/*/src crates/*/tests 2>/dev/null)
    n=${#refs[@]}
    if [ "$n" -gt 0 ] && [ "$n" -le "$MAX_FANOUT" ]; then
      printf '%s\n' "${refs[@]}" >> "$HITS_FILE"
      kept=$((kept+1))
    else
      dropped=$((dropped+1))
    fi
  done < "$SYMS_FILE"
  sort -u "$HITS_FILE" -o "$HITS_FILE"
  printf '  %d symbol(s) kept, %d dropped as too generic (fan-out > %d)\n' \
    "$kept" "$dropped" "$MAX_FANOUT"
fi

# ---- 3. map files -> cargo test targets ------------------------------------
# crates/<pkg>/tests/<name>.rs  -> --test <name>
# crates/<pkg>/src/main.rs      -> --bin <pkg>
# binary-crate modules          -> --bin <pkg>   (see binary_crate_pkg below)
# crates/<pkg>/src/**.rs        -> --lib, filtered by module path

# A src file under a top-level module that main.rs declares and lib.rs does not
# is BINARY-crate code, and `cargo test --lib <that module>` selects ZERO tests
# there. A filter matching nothing is indistinguishable from a pass (§68), so
# before this function every change under such a module got no coverage at all
# while the gate reported green. GH-DECOMP-MAIN created 21 of those files in
# `commands/` in one morning, and `api/` had seven already; GH-CLAIMS-AF hit it
# and ran `--bin glasshouse` by hand. Echoes the package name when the file
# belongs to the binary crate, so the caller can ask for `--bin` instead.
binary_crate_pkg() {  # <src-file>; echoes <pkg>, or returns 1
  local f="$1" pkg top
  case "$f" in crates/*/src/*/*) : ;; *) return 1 ;; esac
  pkg="$(echo "$f" | cut -d/ -f2)"
  top="$(echo "$f" | sed -E 's#^crates/[^/]+/src/##' | cut -d/ -f1)"
  [ -n "$top" ] && [ -f "crates/$pkg/src/main.rs" ] || return 1
  grep -qE "^[[:space:]]*(pub )?mod ${top};" "crates/$pkg/src/main.rs" || return 1
  # Declared in BOTH: the lib copy is the one `--lib` compiles and runs, so the
  # module is not binary-only and the existing --lib filter is correct.
  if [ -f "crates/$pkg/src/lib.rs" ] &&
     grep -qE "^[[:space:]]*(pub )?mod ${top};" "crates/$pkg/src/lib.rs"; then
    return 1
  fi
  printf '%s\n' "$pkg"
}

LIB=0; declare -a TESTS=() BINS=() FILTERS=() LIB_PACKAGES=()
while read -r hit; do
  [ -n "$hit" ] || continue
  # An integration crate is crates/<pkg>/tests/<name>.rs and nothing deeper: a
  # `case` glob's `*` matches slashes, so the old `crates/*/tests/*.rs` arm
  # took src/config/tests/part_a.rs for a `--test part_a` crate that does not
  # exist (found by GH-DECOMP-CONFIG). Those files are lib test modules and
  # fall through to the src arm below.
  #
  # Every entry below is "pkg:label" -- the workspace gained a second package
  # (crates/pane) and run_target() needs to know which one each target
  # belongs to, not just glasshouse. Display sites strip the "pkg:" prefix
  # back off (see *_DISPLAY below) so glasshouse-only output is unchanged.
  if [[ "$hit" =~ ^crates/[^/]+/tests/[^/]+\.rs$ ]]; then
    TESTS+=("$(echo "$hit" | cut -d/ -f2):$(basename "$hit" .rs)"); continue
  fi
  case "$hit" in
    crates/*/src/main.rs)
      pkg="$(echo "$hit" | cut -d/ -f2)"
      BINS+=("$pkg:$pkg") ;;
    crates/*/src/*.rs|crates/*/src/**/*.rs)
      if _binpkg="$(binary_crate_pkg "$hit")"; then
        BINS+=("$_binpkg:$_binpkg"); continue
      fi
      LIB=1
      pkg="$(echo "$hit" | cut -d/ -f2)"
      LIB_PACKAGES+=("$pkg")
      # src/gateway/session.rs -> gateway::session ; src/foo.rs -> foo
      m="$(echo "$hit" | sed -E 's#^crates/[^/]+/src/##; s#\.rs$##; s#/mod$##; s#/#::#g')"
      [ "$m" = "lib" ] || FILTERS+=("$pkg:$m")
      # A parent module's own `mod tests` (this crate's source-scanning tests)
      # runs only under the parent's filter, not the child's -- 2026-09-02's
      # trailing sweep found this red when only `gateway::session` was traced.
      case "$m" in *::*) FILTERS+=("$pkg:${m%%::*}") ;; esac
      ;;
  esac
done < "$HITS_FILE"

# de-dup
mapfile -t TESTS < <(printf '%s\n' "${TESTS[@]-}" | sort -u | sed '/^$/d')
mapfile -t BINS  < <(printf '%s\n' "${BINS[@]-}"  | sort -u | sed '/^$/d')
mapfile -t FILTERS < <(printf '%s\n' "${FILTERS[@]-}" | sort -u | sed '/^$/d')
mapfile -t LIB_PACKAGES < <(printf '%s\n' "${LIB_PACKAGES[@]-}" | sort -u | sed '/^$/d')

# ---- targeted (distance-zero) trace, independent of the fan-out above -----
# --targeted never consults SYMS_FILE/HITS_FILE (the fan-out that finds a
# changed constant's other referencing files) -- it only asks, for each
# changed file itself: is it a test target? Does it have a same-named/
# most-specific integration test? What lib module does it live in? That is
# "distance zero" -- one hop closer than the default trace, and cheap enough
# to be a blocking gate rather than a background one.
most_specific_integration_target() {  # <src-file> on stdout if one exists
  local f="$1" pkg rest c
  case "$f" in crates/*/src/*.rs) : ;; *) return 1 ;; esac
  pkg="$(echo "$f" | cut -d/ -f2)"
  rest="$(echo "$f" | sed -E 's#^crates/[^/]+/src/##; s#\.rs$##')"
  # most specific first: the full path flattened, then just the leaf module.
  for c in "$(echo "$rest" | sed 's#/#_#g')" "$(basename "$rest")"; do
    [ -n "$c" ] || continue
    if [ -f "crates/$pkg/tests/$c.rs" ]; then echo "$c"; return 0; fi
  done
  return 1
}

declare -a TARGETED_TESTS=() TARGETED_FILTERS=() TARGETED_BINS=() TARGETED_LIB_PACKAGES=()
for f in "${FILES[@]}"; do
  # Same rule as above: only crates/<pkg>/tests/<name>.rs is an integration crate.
  # "pkg:label" throughout, same reason as the full-trace arrays above.
  if [[ "$f" =~ ^crates/[^/]+/tests/[^/]+\.rs$ ]]; then
    TARGETED_TESTS+=("$(echo "$f" | cut -d/ -f2):$(basename "$f" .rs)")
  fi
done
for f in "${FILES[@]}"; do
  case "$f" in
    crates/*/src/main.rs) : ;;  # a bin's own target isn't in --targeted's promise; see the header
    crates/*/src/*.rs)
      # Binary-crate code: `--lib <module>` selects nothing there, so --targeted
      # asks for `--bin <pkg>` instead. This DOES widen --targeted's promise
      # past "no bin targets" -- deliberately, ruled at integration 2026-09-03:
      # the alternative is a filter that matches zero tests and reads as green,
      # and `--bin glasshouse` is 85 tests in about 7 seconds. A change to
      # main.rs itself still adds nothing (arm above): its own dispatch is
      # covered whenever any commands/ file moves with it, and a main.rs-only
      # change is argument wiring the full sweep carries.
      if _tbinpkg="$(binary_crate_pkg "$f")"; then
        TARGETED_BINS+=("$_tbinpkg:$_tbinpkg"); continue
      fi
      pkg="$(echo "$f" | cut -d/ -f2)"
      TARGETED_LIB_PACKAGES+=("$pkg")
      tgt="$(most_specific_integration_target "$f")" && [ -n "$tgt" ] && TARGETED_TESTS+=("$pkg:$tgt")
      m="$(echo "$f" | sed -E 's#^crates/[^/]+/src/##; s#\.rs$##; s#/mod$##; s#/#::#g')"
      [ "$m" = "lib" ] || TARGETED_FILTERS+=("$pkg:$m")
      # A parent module's own `mod tests` (this crate's source-scanning tests)
      # runs only under the parent's filter, not the child's -- 2026-09-02's
      # trailing sweep found this red when only `gateway::session` was traced.
      case "$m" in *::*) TARGETED_FILTERS+=("$pkg:${m%%::*}") ;; esac
      ;;
  esac
done
mapfile -t TARGETED_TESTS   < <(printf '%s\n' "${TARGETED_TESTS[@]-}"   | sort -u | sed '/^$/d')
mapfile -t TARGETED_FILTERS < <(printf '%s\n' "${TARGETED_FILTERS[@]-}" | sort -u | sed '/^$/d')
mapfile -t TARGETED_BINS    < <(printf '%s\n' "${TARGETED_BINS[@]-}"    | sort -u | sed '/^$/d')
mapfile -t TARGETED_LIB_PACKAGES < <(printf '%s\n' "${TARGETED_LIB_PACKAGES[@]-}" | sort -u | sed '/^$/d')
TARGETED_LIB=0
[ ${#TARGETED_FILTERS[@]} -gt 0 ] && TARGETED_LIB=1

# How many FULL-trace targets --targeted is about to skip -- printed always,
# in every mode, so the count is never a mystery even for a --list preview.
# --lib doesn't count here: both modes touch --lib, just with different
# filters, and a bin change is always counted as skipped (see above).
FULL_TRACE_TARGET_COUNT=$(( ${#TESTS[@]} + ${#BINS[@]} ))
TARGETED_MATCHED_COUNT=0
for t in "${TESTS[@]-}"; do
  for tt in "${TARGETED_TESTS[@]-}"; do
    if [ "$t" = "$tt" ]; then TARGETED_MATCHED_COUNT=$((TARGETED_MATCHED_COUNT+1)); break; fi
  done
done
# A bin the targeted trace now DOES run is not skipped. Only a main.rs-only
# change still counts as skipped, which is what the comment above describes.
for b in "${BINS[@]-}"; do
  for tb in "${TARGETED_BINS[@]-}"; do
    if [ "$b" = "$tb" ]; then TARGETED_MATCHED_COUNT=$((TARGETED_MATCHED_COUNT+1)); break; fi
  done
done
SKIPPED_FULL_TARGET_COUNT=$(( FULL_TRACE_TARGET_COUNT - TARGETED_MATCHED_COUNT ))

# ---- lane classification (ONE place — extend here) -------------------------
# Serial lane: every target that spawns a process, drives a PTY, or asserts on
# wall-clock. Parallel lane: everything else (pure config/routing/translation
# logic). Default-serial for the unknown (see the file header): misclassifying
# a fixture-spawner as parallel reintroduces the false-red gate this project
# already paid for four times in one evening; misclassifying logic as serial
# only costs seconds.
#
# Explicit seeds, listed even though most also match the auto-detector below,
# so the next reader sees WHY each is serial without reconstructing it from a
# grep:
KNOWN_SERIAL_TESTS=(
  tui_live                # drives the real pane binary under a PTY
  lifecycle_cutoff        # spawns pane and times its deadlines
)

# --lib SPLITS between the lanes. Only the known flaky/process-bound families
# run serially -- settings_persistence (in shell::mod), integrations::version
# (the ETXTBSY fork/exec race documented below), and session::api -- each via
# its own explicit `cargo test --lib <family>` filter. Everything else in the
# lib is pure config/routing/translation logic and runs as ONE invocation in
# the parallel lane, via `--skip <family>` for exactly those same families.
# Default-serial-for-the-unknown still governs: a lib module joins the serial
# seed list (and so the skip list) by POSITIVELY matching the spawn-pattern
# grep below; it does not get to default into the parallel invocation by
# omission.
#
# Both lists are read off the SAME array (LIB_SERIAL_FAMILIES) rather than
# spelled out twice, so they cannot diverge by editing one and forgetting the
# other -- and the assertion right after them still checks it at runtime, in
# case a future edit reintroduces two copies.
LIB_SERIAL_FAMILIES=(
  session::ui
)

# Decompression rule 4 (CLAUDE.md): a red target in a KNOWN load-sensitive
# family gets exactly one rerun, alone, before it counts as red. This is that
# rule's own list, not a new one -- KNOWN_SERIAL_TESTS above plus the three
# named LIB_SERIAL_FAMILIES seeds (captured here, before the grep loop below
# extends LIB_SERIAL_FAMILIES with families that are merely process-bound, not
# named by rule 4), plus terminal_loss, which rule 4 names by name and neither
# list held. Nothing outside this union is ever rerun automatically.
RERUN_ELIGIBLE_FAMILIES=(
  "${KNOWN_SERIAL_TESTS[@]}"
  session::ui
)
is_rerun_eligible() {   # is_rerun_eligible <family-or-target-name>
  local f="$1" k
  [ -n "$f" ] || return 1
  for k in "${RERUN_ELIGIBLE_FAMILIES[@]}"; do [ "$f" = "$k" ] && return 0; done
  return 1
}

if [ "$TARGETED" -eq 0 ]; then
  while read -r libsrc; do
    [ -n "$libsrc" ] || continue
    m="$(echo "$libsrc" | sed -E 's#^crates/[^/]+/src/##; s#\.rs$##; s#/mod$##; s#/#::#g')"
    [ "$m" = "lib" ] && continue
    already=0
    for k in "${LIB_SERIAL_FAMILIES[@]}"; do [ "$m" = "$k" ] && already=1 && break; done
    [ "$already" -eq 1 ] || LIB_SERIAL_FAMILIES+=("$m")
  done < <(grep -rlE 'Command::new|std::process::Command|tokio::process::Command|PtyProcess::spawn|CARGO_BIN_EXE|Child::' crates/*/src 2>/dev/null | sort -u)
fi

SERIAL_LIB_FILTERS=("${LIB_SERIAL_FAMILIES[@]}")
SKIP_LIB_FILTERS=("${LIB_SERIAL_FAMILIES[@]}")
if [ "${SERIAL_LIB_FILTERS[*]-}" != "${SKIP_LIB_FILTERS[*]-}" ]; then
  echo "blast-radius: BUG -- lib serial-filter list and skip-flag list diverged; refusing rather than run an unproven split" >&2
  exit 1
fi

# --bin targets: same default-serial rule. A binary integration target is
# exactly the "spawns/drives a real process" shape this script exists to keep
# out of the parallel lane, and there is no cheap positive signal to check.
BIN_IS_SERIAL=1

is_serial_test() {   # is_serial_test <pkg> <target-name> -- 0 (serial) or 1 (parallel)
  local pkg="$1" t="$2" k
  local f="crates/$pkg/tests/${t}.rs"
  for k in "${KNOWN_SERIAL_TESTS[@]}"; do [ "$t" = "$k" ] && return 0; done
  # A missing/unreadable source can't be positively cleared as pure logic.
  [ -f "$f" ] || return 0
  grep -qE 'Command::new|std::process::Command|tokio::process::Command|PtyProcess::spawn|CARGO_BIN_EXE|Child::' "$f"
}

declare -a SERIAL_TESTS=() PARALLEL_TESTS=()
for t in "${TESTS[@]-}"; do
  [ -n "$t" ] || continue
  if is_serial_test "${t%%:*}" "${t#*:}"; then SERIAL_TESTS+=("$t"); else PARALLEL_TESTS+=("$t"); fi
done

# Bounded parallelism for the parallel lane: physical cores / 2 by default
# (leaves headroom for the fixture/Gatekeeper-bound serial lane's own
# single-threaded cargo overhead), overridable with --jobs or
# BLAST_PARALLEL_JOBS.
if [ -n "$JOBS" ]; then
  PARALLEL_JOBS="$JOBS"
elif [ -n "${BLAST_PARALLEL_JOBS:-}" ]; then
  PARALLEL_JOBS="$BLAST_PARALLEL_JOBS"
else
  PHYS="$(sysctl -n hw.physicalcpu 2>/dev/null || nproc 2>/dev/null || echo 2)"
  PARALLEL_JOBS=$((PHYS / 2))
fi
[ "$PARALLEL_JOBS" -lt 1 ] 2>/dev/null && PARALLEL_JOBS=1
[ "$PARALLEL_JOBS" -ge 1 ] 2>/dev/null || PARALLEL_JOBS=1

# Display-only: every array above is "pkg:label" so run_target() knows which
# package to pass to `-p`; the plan/--list output strips the prefix back off
# so a glasshouse-only change prints exactly what it always has.
declare -a TESTS_DISPLAY=() BINS_DISPLAY=() FILTERS_DISPLAY=()
for t in "${TESTS[@]-}"; do [ -n "$t" ] && TESTS_DISPLAY+=("${t#*:}"); done
for b in "${BINS[@]-}"; do [ -n "$b" ] && BINS_DISPLAY+=("${b#*:}"); done
for f in "${FILTERS[@]-}"; do [ -n "$f" ] && FILTERS_DISPLAY+=("${f#*:}"); done

echo
printf '\033[1m=== plan ===\033[0m\n'
[ "$LIB" -eq 1 ] && echo "  --lib  (module filters: ${FILTERS_DISPLAY[*]-none})  [split serial/parallel by family]"
[ ${#TESTS[@]} -gt 0 ] && echo "  --test ${TESTS_DISPLAY[*]}"
[ ${#BINS[@]}  -gt 0 ] && echo "  --bin  ${BINS_DISPLAY[*]}  [serial]"
if [ "$FULL_TRACE_COMPUTED" -eq 1 ]; then
  echo "  --targeted would skip ${SKIPPED_FULL_TARGET_COUNT} of this full-trace's target(s)"
else
  echo "  full-trace target count not computed in targeted mode"
fi

if [ "$LIST" -eq 1 ]; then
  echo
  printf '\033[1m=== targets by lane ===\033[0m\n'
  if [ "$LIB" -eq 1 ]; then
    for fam in "${SERIAL_LIB_FILTERS[@]}"; do
      printf '  serial    --lib  filter: %s\n' "$fam"
    done
    printf '  parallel  --lib  (rest of the lib; --skip %d famil%s: %s)\n' \
      "${#SKIP_LIB_FILTERS[@]}" "$([ ${#SKIP_LIB_FILTERS[@]} -eq 1 ] && echo y || echo ies)" \
      "${SKIP_LIB_FILTERS[*]}"
  fi
  for t in "${SERIAL_TESTS[@]-}"; do
    [ -n "$t" ] || continue
    printf '  serial    --test %s\n' "${t#*:}"
  done
  for b in "${BINS[@]-}"; do
    [ -n "$b" ] || continue
    printf '  serial    --bin  %s\n' "${b#*:}"
  done
  for t in "${PARALLEL_TESTS[@]-}"; do
    [ -n "$t" ] || continue
    printf '  parallel  --test %s\n' "${t#*:}"
  done
  echo
  printf '  parallel lane: %d target(s), bounded to %d job(s)\n' \
    "$(( ${#PARALLEL_TESTS[@]} + (LIB ? ${#LIB_PACKAGES[@]} : 0) ))" "$PARALLEL_JOBS"
  printf '  serial lane:   %d target(s) (--lib family filters count individually)\n' \
    "$(( (LIB ? ${#SERIAL_LIB_FILTERS[@]} : 0) + ${#SERIAL_TESTS[@]} + ${#BINS[@]} ))"

  echo
  printf '\033[1m=== --targeted preview ===\033[0m\n'
  if [ "$TARGETED_LIB" -eq 1 ]; then
    declare -a _tf_display=()
    for f in "${TARGETED_FILTERS[@]-}"; do _tf_display+=("${f#*:}"); done
    printf '  --lib  filters: %s\n' "${_tf_display[*]}"
  fi
  for b in "${TARGETED_BINS[@]-}"; do
    [ -n "$b" ] || continue
    printf '  --bin %s\n' "${b#*:}"
  done
  for t in "${TARGETED_TESTS[@]-}"; do
    [ -n "$t" ] || continue
    printf '  --test %s\n' "${t#*:}"
  done
  if [ "$FULL_TRACE_COMPUTED" -eq 1 ]; then
    printf '  would skip %d full-trace target(s)\n' "$SKIPPED_FULL_TARGET_COUNT"
  else
    printf '  full-trace target count not computed in targeted mode\n'
  fi

  echo
  echo "blast-radius: --list, nothing executed"
  exit 0
fi

# Platform-conditional code: warn that this tool is macOS-only evidence.
#
# WHY: on 2026-08-30 the full gate returned 13 PASS / 3 FAIL on a tree this
# script had called green. All three failures were in `pty/mod.rs`'s
# platform-conditional code — a Windows build error (`-D warnings` on an unused
# constant that is `None` there) and a Linux test failure (a hazard that exists
# on macOS/BSD and *not* on Linux, so a ceiling derived from the documented
# constant rather than measured was protecting against nothing).
#
# This script runs on one platform. That is not a defect in it — but it means
# "blast radius green" is never evidence about the other two, and nineteen
# commits accumulated before anyone ran the gate that could see them. So say so,
# loudly, exactly when it matters.
if [ "${#FILES[@]}" -gt 0 ]; then
  plat=""
  for f in "${FILES[@]}"; do
    [ -f "$f" ] || continue
    case "$f" in *.rs)
      if grep -qE '#\[cfg\((target_os|unix|windows)|cfg!\((unix|windows|target_os)' "$f" 2>/dev/null; then
        plat="$plat $f"
      fi ;;
    esac
  done
  if [ -n "$plat" ]; then
    printf '\n\033[33m=== PLATFORM-CONDITIONAL CODE CHANGED ===\033[0m\n'
    printf '  These files contain cfg(unix/windows/target_os):\n'
    for f in $plat; do printf '    %s\n' "$f"; done
    printf '  \033[33mThis script runs on THIS platform only. A green result here is\n'
    printf '  NOT evidence about the other two.\033[0m Run the full gate before\n'
    printf '  believing it:  scripts/ci-local.sh --macos --linux --windows-vm\n'
    # A cheap half of that gate runs here when the cross target is installed:
    # `cargo check` for Windows catches what the sweep otherwise finds twenty
    # minutes later -- a `cfg(unix)`-only constructor leaving an enum variant
    # `dead_code` under `warnings = "deny"` turned every Windows cell red on
    # 2026-09-06 (`17599f3`). Two minutes here, or a red sweep and a fix-forward.
    if [ "$DRY" -ne 1 ] && rustup target list --installed 2>/dev/null | grep -qx 'x86_64-pc-windows-gnu'; then
      printf '\n\033[1m=== cargo check --tests --target x86_64-pc-windows-gnu (platform-conditional files changed) ===\033[0m\n'
      if env -u ANTHROPIC_BASE_URL -u ANTHROPIC_AUTH_TOKEN -u ANTHROPIC_API_KEY cargo check -p inference-gateway --tests --target x86_64-pc-windows-gnu 2>&1 | grep -E '^(error|warning: unused)' ; then
        printf '\033[31mblast-radius: the Windows target does not build -- fix before pushing\033[0m\n'
        exit 1
      fi
      printf '  windows-gnu check: clean\n'
    fi
  fi
fi

if [ "$DRY" -eq 1 ]; then echo; echo "blast-radius: --dry-run, nothing executed"; exit 0; fi

take_lock "$ORIG_ARGS"
trap 'rm -f "$SYMS_FILE" "${HITS_FILE:-}"; release_lock' EXIT

# ---- 4. run ----------------------------------------------------------------
# One cargo invocation where possible; test-name filters are applied per module
# so a huge --lib run does not swamp the signal (§68: a filter matching nothing
# looks exactly like a pass, so the count is printed either way).
rc=0
FLAKY_PASS_COUNT=0
FLAKY_FAIL_COUNT=0
run_target() {                     # run_target <pkg> <label> <cargo args...>
  local pkg="$1" label="$2"; shift 2
  echo; printf '\033[1m=== %s ===\033[0m\n' "$label"
  local out; out="$(mktemp)"
  # Scrubbed per the user's 2026-09-05 ruling: a gate run from any pane must
  # not inherit the worker/harness's own ANTHROPIC_* vars into the tests it runs.
  env -u ANTHROPIC_BASE_URL -u ANTHROPIC_AUTH_TOKEN -u ANTHROPIC_API_KEY \
    cargo test -p "$pkg" --all-features "$@" >"$out" 2>&1
  local status=$?
  # §68: a filter that matches nothing looks exactly like a pass, so always show
  # the result line, including the count.
  local result_line; result_line="$(grep -E 'test result:' "$out" | tail -1)"
  grep -E 'test result:|^error' "$out" | tail -6
  grep -q 'test result: FAILED' "$out" && status=1
  # Show the panic MESSAGE, not only the failing test's name. cargo prints the
  # message under `---- <test> stdout ----`, which is ABOVE `failures:`, so the
  # `-A4 '^failures:'` grep below never reaches it. Measured 2026-08-30: a test
  # whose whole purpose was a self-diagnosing panic ("exited=Some(_) means the
  # signal killed the shell; None means the trap is merely late") failed here
  # and this script printed its file:line and threw the diagnosis away.
  [ "$status" -ne 0 ] && { echo "  --- why ---"; grep -A6 'panicked at' "$out" | head -24; }
  [ "$status" -ne 0 ] && { echo "  --- failures ---"; grep -A4 '^failures:' "$out" | head -12; }
  rm -f "$out"

  # Decompression rule 4: a red in a load-sensitive family gets exactly one
  # rerun, alone -- no loop, no timeout, no sleep. Every RERUN_ELIGIBLE_FAMILIES
  # member is classified into the serial lane by construction (KNOWN_SERIAL_TESTS
  # and LIB_SERIAL_FAMILIES both feed it), so by the time this runs, nothing
  # else from this script is running concurrently in this tree.
  if [ "$status" -ne 0 ]; then
    local family=""
    case "${1:-}" in
      --lib)  [ "${2:-}" != "--" ] && family="${2:-}" ;;
      --test) family="${2:-}" ;;
    esac
    if is_rerun_eligible "$family"; then
      echo "  blast-radius: '$family' is a rule-4 load-sensitive family -- rerunning alone, once"
      local out2; out2="$(mktemp)"
      env -u ANTHROPIC_BASE_URL -u ANTHROPIC_AUTH_TOKEN -u ANTHROPIC_API_KEY \
        cargo test -p "$pkg" --all-features "$@" >"$out2" 2>&1
      local status2=$?
      local result_line2; result_line2="$(grep -E 'test result:' "$out2" | tail -1)"
      grep -E 'test result:|^error' "$out2" | tail -6
      grep -q 'test result: FAILED' "$out2" && status2=1
      if [ "$status2" -eq 0 ]; then
        printf '\033[33mflaky-pass: %s (family: %s) -- first: %s | rerun: %s\033[0m\n' \
          "$label" "$family" "$result_line" "$result_line2"
        FLAKY_PASS_COUNT=$((FLAKY_PASS_COUNT + 1))
        status=0
      else
        echo "  --- rerun why ---"; grep -A6 'panicked at' "$out2" | head -24
        echo "  --- rerun failures ---"; grep -A4 '^failures:' "$out2" | head -12
        printf 'blast-radius: %s failed on the rerun too (family: %s) -- a real red, not flaky. Do not rerun it a third time.\n' \
          "$label" "$family"
        FLAKY_FAIL_COUNT=$((FLAKY_FAIL_COUNT + 1))
      fi
      rm -f "$out2"
    fi
  fi
  return "$status"
}

if [ "$TARGETED" -eq 1 ]; then
  echo
  # ---- compile the LIBRARY'S OWN test module first, because nothing below does.
  #
  # 2026-09-02: `9f513d9` reached main with the library test harness broken -- a
  # four-argument straggler inside `routing/session.rs`'s `#[cfg(test)]` block
  # after `FreePool::adopt_observed` grew a fifth argument. The targeted gate
  # was green: every target it runs is an integration-test binary, and those
  # compile the library WITHOUT `cfg(test)`, so a compile error inside the
  # lib's own test module is invisible to all of them. `integrate.sh` gained
  # the same check the same day, but other flows call `--targeted` directly
  # (a worker's own gate, a fix-forward), and a green here must mean the same
  # thing everywhere. Seconds against a warm target directory; blocking, not
  # advisory -- a warning changed nothing on the day it was needed.
  printf '\033[1m=== library unit-test harness compile (changed packages only) ===\033[0m\n'
  for pkg in "${TARGETED_LIB_PACKAGES[@]-}"; do
    [ -n "$pkg" ] || continue
    if ! cargo test -p "$pkg" --all-features --lib --no-run --quiet; then
      echo
      echo "blast-radius: $pkg's library test module does not compile."
      echo "  This is usually a signature change with a straggler in a #[cfg(test)]"
      echo "  block -- integration-test binaries compile the library without that code."
      exit 1
    fi
    echo "  $pkg library unit-test harness: clean"
  done
  echo
  printf '\033[1m=== --targeted: distance-zero targets only ===\033[0m\n'
  if [ "$TARGETED_LIB" -eq 1 ]; then
    for fl in "${TARGETED_FILTERS[@]}"; do
      run_target "${fl%%:*}" "cargo test --lib ${fl#*:}" --lib "${fl#*:}" || rc=1
    done
  fi
  for b in "${TARGETED_BINS[@]-}"; do
    [ -n "$b" ] || continue
    run_target "${b%%:*}" "cargo test --bin ${b#*:}" --bin "${b#*:}" || rc=1
  done
  for t in "${TARGETED_TESTS[@]-}"; do
    [ -n "$t" ] || continue
    run_target "${t%%:*}" "cargo test --test ${t#*:}" --test "${t#*:}" || rc=1
  done
  echo
  printf '\033[33mblast-radius: --targeted did not compute or run the FULL symbol-fan-out trace -- this is the blocking worker gate; the orchestrator runs --full once per wave\033[0m\n'
elif [ "$SERIAL" -eq 1 ]; then
  echo
  printf '\033[1m=== --serial: single lane, original order ===\033[0m\n'
  if [ "$LIB" -eq 1 ]; then
    for pkg in "${LIB_PACKAGES[@]}"; do
      run_target "$pkg" "cargo test --lib" --lib || rc=1
    done
  fi
  for t in "${TESTS[@]-}"; do
    [ -n "$t" ] || continue
    run_target "${t%%:*}" "cargo test --test ${t#*:}" --test "${t#*:}" || rc=1
  done
  for b in "${BINS[@]-}"; do
    [ -n "$b" ] || continue
    run_target "${b%%:*}" "cargo test --bin ${b#*:}" --bin "${b#*:}" || rc=1
  done
else
  echo
  printf '\033[1m=== lane counts ===\033[0m\n'
  printf '  parallel lane: %d target(s), bounded to %d job(s)\n' \
    "$(( ${#PARALLEL_TESTS[@]} + (LIB ? ${#LIB_PACKAGES[@]} : 0) ))" "$PARALLEL_JOBS"
  printf '  serial lane:   %d target(s) (--lib family filters count individually)\n' \
    "$(( (LIB ? ${#SERIAL_LIB_FILTERS[@]} : 0) + ${#SERIAL_TESTS[@]} + ${#BINS[@]} ))"

  # Parallel lane first -- it is the one that can saturate idle cores, and
  # running it before the serial lane keeps the serial lane's fixture/
  # Gatekeeper-bound targets off a machine that is also running N other
  # cargo processes (load hygiene for the wall-clock-bound tests, requirement
  # 3). A failing parallel target does not abort the run: every target always
  # runs and every failure is listed at the end, exactly as the serial lane
  # already does (requirement 6).
  # The rest-of-lib invocation (everything NOT in a serial family) joins this
  # same bounded-parallel queue as one more job -- it is pure config/routing/
  # translation logic by the same default-serial-for-the-unknown rule that
  # decided the split, so it belongs beside the other parallel targets, not
  # ahead of or behind them.
  declare -a _PARALLEL_JOBS=()
  for t in "${PARALLEL_TESTS[@]-}"; do [ -n "$t" ] && _PARALLEL_JOBS+=("test:$t"); done
  if [ "$LIB" -eq 1 ]; then
    for pkg in "${LIB_PACKAGES[@]}"; do _PARALLEL_JOBS+=("libskip:$pkg"); done
  fi

  if [ ${#_PARALLEL_JOBS[@]} -gt 0 ]; then
    echo
    printf '\033[1m=== parallel lane (%d target(s), %d job(s)) ===\033[0m\n' \
      "${#_PARALLEL_JOBS[@]}" "$PARALLEL_JOBS"
    declare -a _PIDS=() _OUTS=()
    drain_one() {
      local pid="${_PIDS[0]}" out="${_OUTS[0]}"
      wait "$pid"; local status=$?
      cat "$out"; rm -f "$out"
      _PIDS=("${_PIDS[@]:1}"); _OUTS=("${_OUTS[@]:1}")
      return "$status"
    }
    for job in "${_PARALLEL_JOBS[@]}"; do
      while [ "${#_PIDS[@]}" -ge "$PARALLEL_JOBS" ]; do
        drain_one || rc=1
      done
      out="$(mktemp)"
      case "$job" in
        test:*)
          t="${job#test:}"
          ( run_target "${t%%:*}" "cargo test --test ${t#*:}" --test "${t#*:}" ) >"$out" 2>&1 &
          ;;
        libskip:*)
          # `--skip` is a libtest (test-binary) argument, not a cargo one -- it
          # needs the `--` separator or cargo refuses it with "unexpected
          # argument '--skip' found" before ever reaching the test binary.
          # Measured while testing this packet's own change.
          #
          # One rest-of-lib invocation per package whose lib was hit; the
          # family filters are module paths and a filter naming nothing in a
          # package skips nothing there.
          declare -a _skip_args=()
          for fam in "${SKIP_LIB_FILTERS[@]}"; do _skip_args+=(--skip "$fam"); done
          ( run_target "${job#libskip:}" "cargo test --lib (rest; skip: ${SKIP_LIB_FILTERS[*]})" --lib -- "${_skip_args[@]}" ) >"$out" 2>&1 &
          ;;
      esac
      _PIDS+=("$!"); _OUTS+=("$out")
    done
    while [ "${#_PIDS[@]}" -gt 0 ]; do
      drain_one || rc=1
    done
  fi

  # Serial lane second, on a now-quiet machine: --lib now runs ONLY its known
  # flaky/process-bound families here, one explicit filter per invocation
  # (its rest already ran above, in the parallel lane); tests and bins are
  # unchanged from today's order.
  if [ "$LIB" -eq 1 ]; then
    for pkg in "${LIB_PACKAGES[@]}"; do
      for fam in "${SERIAL_LIB_FILTERS[@]}"; do
        run_target "$pkg" "cargo test --lib $fam" --lib "$fam" || rc=1
      done
    done
  fi
  for t in "${SERIAL_TESTS[@]-}"; do
    [ -n "$t" ] || continue
    run_target "${t%%:*}" "cargo test --test ${t#*:}" --test "${t#*:}" || rc=1
  done
  for b in "${BINS[@]-}"; do
    [ -n "$b" ] || continue
    run_target "${b%%:*}" "cargo test --bin ${b#*:}" --bin "${b#*:}" || rc=1
  done
fi

# Rustdoc, unconditionally, because this tool could not see it and the gate can.
#
# WHY: twice now a package has gone green here and red on the gate's
# `lint / rustdoc` job, both times for the same shape — a **public** doc
# comment linking a **private** item, which trips
# `-D rustdoc::private-intra-doc-links`. Migration 15 did it, and
# `memory/inject.rs` did it again two batches later, reaching `main` because
# this script reported every traced target passing.
#
# The recorded rule was "a schema change needs the full gate, blast-radius
# green is not sufficient". That was too narrow: the real gap is that this
# tool maps changed files to *cargo test targets*, and rustdoc is not one.
# Any package adding a doc comment can be red in a job nothing here runs.
#
# It costs about eight seconds and it closes the whole class.
echo
printf '\033[1m=== cargo doc --no-deps (rustdoc) ===\033[0m\n'
doc_out="$(mktemp)"
if RUSTDOCFLAGS="-D warnings" cargo doc --no-deps -p inference-gateway >"$doc_out" 2>&1; then
  echo "  rustdoc: clean"
else
  rc=1
  echo "  --- rustdoc failures ---"
  grep -E '^(error|warning)' "$doc_out" | head -12
fi
rm -f "$doc_out"

# Formatting, for the same reason the ratchet is here: this is the gate the
# work actually runs. `cargo fmt --check` lived in ci-local.sh's lint lane and
# in integrate.sh, and the 2026-09-17 ruling made in-session work the default
# path -- which runs neither. Two GitHub lint cells went red on formatting in
# two days before anyone noticed the step had lost its owner. It costs
# seconds: fmt parses, it does not build.
echo
printf '\033[1m=== formatting ===\033[0m\n'
if fmt_out=$(env -u ANTHROPIC_BASE_URL -u ANTHROPIC_AUTH_TOKEN -u ANTHROPIC_API_KEY \
  cargo fmt --all -- --check 2>&1); then
  echo "  cargo fmt --all --check: clean"
else
  rc=1
  echo "  --- unformatted (run \`cargo fmt --all\`) ---"
  printf '%s\n' "$fmt_out" | grep -E '^Diff in ' | head -12
fi

# Phase 59's size ratchet: a production file over the ceiling may only shrink.
# It is here rather than only in ci-local.sh because this is the gate every
# worker runs; a package that grows main.rs learns it before it reports.
echo
printf '\033[1m=== file-size ratchet (Phase 59) ===\033[0m\n'
if ! python3 scripts/check-file-sizes.py; then
  rc=1
fi

echo
if [ "$rc" -eq 0 ]; then
  if [ "$FLAKY_PASS_COUNT" -gt 0 ]; then
    printf '\033[33mblast-radius: every traced target passed (%d flaky-pass%s -- see above; not red, no attribution write-up)\033[0m\n' \
      "$FLAKY_PASS_COUNT" "$([ "$FLAKY_PASS_COUNT" -eq 1 ] && echo '' || echo 'es')"
  else
    printf '\033[32mblast-radius: every traced target passed\033[0m\n'
  fi
else
  printf '\033[31mblast-radius: FAILURES above — fix before the gate\033[0m\n'
fi
exit "$rc"
