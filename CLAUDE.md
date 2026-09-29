# Sterna — instructions for coding agents

This repository builds two programs:

- **`sterna`** (`crates/sterna`) — a coding agent for the terminal. The model
  writes one TypeScript program per turn; tool results stay live in an
  embedded V8 isolate as named handles. A subagent takes a whole goal on
  another model loop beside it.
- **`inference-gateway`** (`crates/inference-gateway`) — the standalone model
  gateway Sterna starts beside itself: providers, API keys, subscriptions,
  pooling, protocol translation and usage.

`docs/architecture.md` is the one-page map; `docs/README.md` indexes the rest.
Read a doc when the task reaches it, not up front.

## Build and test

```sh
cargo build -p sterna -p inference-gateway          # sterna embeds V8: the first build is long
cargo test -p sterna --test <target>                # in the edit loop: the one target you touched
cargo test -p inference-gateway --lib <module>
scripts/install-local.sh                            # build, install into a fresh version dir, make it current
scripts/install-local.sh --rollback                 # point `current` back at the previous version
```

- Run cargo plainly. Do not prefix it with `env -u …` out of habit; only when
  `env | grep ^ANTHROPIC_` actually names a variable.
- Never set `RUSTFLAGS`. Warnings are denied by `[workspace.lints.rust]`;
  a `RUSTFLAGS` value splits the build cache.
- One compiler: `rust-toolchain.toml` and `[workspace.metadata.ci] toolchain`
  name the same version and are bumped together, in one commit.
- Batch edits and verify once; do not compile after every small edit.

## The gate before a commit

1. `scripts/blast-radius.sh --targeted <every changed .rs file>` — list the
   files explicitly. Bare `scripts/blast-radius.sh` is the full sweep and a
   hook refuses it; `--full` is for a deliberate pre-push sweep.
2. `cargo fmt --all` and `cargo clippy -p <crate> --all-targets` for every
   crate you touched.
3. The size ratchet, `python3 scripts/check-file-sizes.py`: a file over
   2,500 production lines may only shrink (`scripts/file-size-baseline.txt`).
   Over the ceiling means split the module, not trim its comments.
4. When a change alters a contract (a new refusal, a removed option), grep
   `crates/*/tests` for the feature's name and run every target it names —
   the targeted gate only traces distance-zero targets.
5. A decision you add (a branch, a threshold, an ordering) gets one mutation
   that a test kills: `scripts/mutate.sh --allow-dirty --file F --find X
   --replace Y --test <cargo test args>` (flags before `--test`).
6. Workbench changes (`crates/sterna/src/workbench/`, `tui*`) also run
   `cargo test -p sterna --test tui_live` (the real binary under a PTY) and
   the `workbench` and `tui_look` targets: they pin screen strings the
   targeted gate does not trace.
7. Windows-only code (`#[cfg(windows)]`) is checked by the CI's Windows cells;
   gate its test helpers and imports by reading, because dead code is an
   error under deny-warnings.

Report real `test result:` lines, never "tests pass". A red target in a
known timing-sensitive family (`tui_live`, `lifecycle_cutoff`, `session::ui`)
is rerun alone once — the gate does this itself; a pass there is a flaky
pass, not a red.

## Git

- Commit by pathspec: `git commit -m … -- <paths>`. Never `git add -A`,
  `git checkout --`, `git restore`, `git stash` or `git clean`; a hook refuses
  the destructive forms.
- Every push runs the twelve-cell GitHub sweep (`ci-extended.yml`: five
  OS/arch targets on the declared compiler and the MSRV, plus beta and
  nightly). It trails the local gate; a red cell is fixed forward.
- Tag a release only after that sweep is green on the commit being tagged.

## Keys and processes

- Provider keys stay local. Never print one, never put one on a command line
  (argv is visible to every process), never hand one to an agent, and never
  read the key files or the gateway's credential store to "check" them.
- `scripts/check-secrets.py` runs in the pre-commit and pre-push hooks
  (`scripts/install-git-hooks.sh` sets `core.hooksPath`). A fixture that
  looks like a key gets the `glasshouse:not-a-secret` marker (the scanner's literal) or an allowlist fingerprint.
- Kill only processes you started, by PID. No `pkill`, `killall` or pattern
  kills: other sessions and the user's own programs share this machine.

## Installing a build

An installed version is immutable. `scripts/install-local.sh` builds into a
fresh `~/.local/lib/sterna/versions/<id>` and repoints `current`. Never
overwrite an installed binary in place: macOS kills a re-signed binary at
the same path.

## Product rules

- **No legacy code.** A replaced feature is deleted outright: no shims,
  aliases or compatibility branches. The one exception is a saved settings
  file: never break one — migrate it, or retire the old word with a one-time
  notice that says what to do.
- **Setup is a designed sheet, never a one-line prompt.** Pickers, keys,
  sign-in, settings and wizards get a laid-out surface; mock it up first.
- **Mouse first, one interaction model.** Every surface is clickable and
  behaves like every other surface; keys are there too.
- **Plain copy.** Say what happened in plain words. No bird puns, no
  cute voice.
- **Nothing black on black.** Every foreground stays readable on the
  terminal's own background, in every theme.
- **Animal art is drawn from reference photos.** Sprites are traced from
  real photographs (credited in the README), never drawn from memory.

## How work is done here

- Do the work in this session. A fork (a subagent that shares this context)
  is for a disjoint file set that can run beside you; a plain subagent is for
  reading, design questions and independent review.
- Nothing beyond that: no hand-off documents, no separate long-running
  sessions to supervise, no process machinery. State the behaviour you are
  changing in one sentence, prove it with the smallest failing test, pass
  the gate, commit, stop.
- An investigation starts with a falsifiable question and gets two probes;
  then act, drop it, or write down the open question and move on.
- Suggestions never block: note a non-essential question and continue on a
  sensible default.
- A decision for the user is written as distinct options (A/B/C) with what
  each means and costs.
