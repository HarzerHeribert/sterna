# Brief: implement the TUI fix packages

You implement the fix packages from the click-through audit of Sterna's TUI.
Everything you need is in this repository.

## Read first
1. `CLAUDE.md` — build, test, the gate, commit rules, the product's UI rules. It binds you.
2. `docs/audit/tui-audit.md` — the one interaction model, 18 fix packages (root
   cause, fix, smallest failing test, findings, files), the keyboard/mouse table as
   found, and all 202 confirmed findings with repro steps. The audit ran on pre.15
   (then named Pane): paths are updated to `crates/sterna`, line numbers have drifted.
3. `docs/audit/decisions.md` — the answers to the audit's open questions. They are
   decided; do not reopen them.
4. `art/birds/` — ten pixel sprites traced from photographs (sprite.txt, palette.json
   with `_light`, landmarks.json, notes.md with photo credits).
5. `scripts/tui-kit/README.md` — drive the real binary with keys and mouse against a
   free fake model. Use it to check every user-visible change the way a person meets it.

## Where to work
A new branch `tui-packages` from `main`. One commit per package (or per tightly
coupled pair), message = what changed for the person + the tests + the mutation.
Push, open a PR against `main`, and keep going until the PR's CI is green on all
twelve test cells plus lint and audit. Do not tag, release, or merge.

## The work, in three waves
Waves exist because later packages build on earlier ones and share files. Inside a
wave, packages whose files do not overlap can go in any order.

**Wave 1 — foundations**
- **P5** One Sheet component and one sheet stack: the interaction model itself
  (the "One interaction model" section of the audit). Every picker, form, settings
  page, wizard step, info page and decision prompt is built from it: a click acts;
  Enter does what a click does; Space = Enter except while typing a search; ↑↓ move;
  ←→ change a value; Tab switches section; Esc goes back one layer; the wheel only
  scrolls; hover highlights; the current value is marked by a glyph, not by colour.
- **P1** Decision prompts are answered on purpose: approval, ask, redirect,
  rollback and the never-asks confirm become Sheets; keys typed within 500 ms of a
  prompt appearing are discarded; danger confirms start on Cancel; **Esc on an
  approval denies that one call and is not remembered**; an explicit Deny is
  remembered **visibly** ("you denied this earlier") and a sheet lists every
  session allowance and denial with a Forget chip (wire `Gate::session_actions()`);
  Ctrl-C at a prompt is never a Deny; the body is readable, not raw JSON.
- **P2** The Auto rung: a command that matches no allow pattern goes to
  **call-level judgement** — read-only commands run, anything else asks. No hard
  deny for a miss. `ls -la && git log --oneline -3` must run on Auto.

**Wave 2 — built on the Sheet**
- **P6** Settings act on the running session, not a file snapshot.
- **P7** First-run setup saves to global settings and applies to the live session;
  it stops writing into the project.
- **P8** One fact, one name, one setter for rung, mode, effort and helpers (the
  chip words everywhere — decision 11). The effort chip never disappears at
  `default`; every control stays visible at every value.
- **P9** The model picker: stays open while choosing, honours tiers, ranking and
  capability; subagent favourite slots are chosen by clicking a slot, then a model.
- **P12** Sign-in flows: device-flow parsing, cancel, SSH states; **a sign-in that
  opens a browser asks first** — nothing opens a browser on a single click.

**Wave 3 — the rest (parallel where files are disjoint)**
- **P3** Local controls are not model turns (status line and turn clock untouched).
- **P4 / P4b** The transcript keeps cells, answers and synthetic messages apart; an
  answered `ask()` does not end its cell as FAILED.
- **P10** The composer is a real multi-line editor (history never eats a draft,
  no hidden characters at wraps, Ctrl-W/Alt-B, undo, `@` paths as advertised).
- **P11** Selection is text, not screen cells; Ctrl-C copies a selection, else interrupts.
- **P13** The transcript renderer uses the markdown path and keeps helper lanes in their card.
- **P14** Person-facing text is written for people, not model-facing or diagnostic strings.
- **P15** An overflow policy for chrome: nothing clipped, dropped or overprinted silently.
- **P16** Palette and sprites:
  - light terminals detected automatically (OSC 11 query, COLORFGBG) with a
    `ui.background = auto|dark|light` override; every bird uses its `_light` palette there;
  - nothing black on black;
  - **the birds from `art/birds/`**: every parrot gets its own sprite, replacing the
    shared outline in `workbench/plumage.rs`. Moods find the eye and beak through
    `landmarks.json` (blink: the eye takes the head colour; oops: a red eye; think,
    work and done marks are placed relative to the sprite's own bounds), so no mood
    code hard-codes coordinates;
  - a third theme family, **Seabirds**, with the **Arctic tern**: `tern-perched` on
    the start card, its 8-px head crop in the header once the conversation starts,
    and `tern-mark` (the flying tern) printed by `sterna --version`;
  - the card and header adapt to each sprite's size (the tern is 24×24, the parrots
    18×24, the mark 38×31).
- **P17** Session edges: resume becomes a normal Sheet inside the TUI (decision 12);
  empty sessions; fullscreen.

## Rules that bite
- A failing test first for every package (the audit names the smallest one), then
  the fix. One mutation per decision the package makes (`scripts/mutate.sh`), and it
  must be KILLED.
- The gate before every commit: `scripts/blast-radius.sh --targeted <every changed
  .rs file>`, `cargo fmt --all`, `cargo clippy -p sterna --all-targets`, the size
  ratchet. Workbench changes also run `cargo test -p sterna --test tui_live`,
  `--test workbench`, `--test tui_look`. When a contract changes, grep
  `crates/*/tests` for it and run every target found.
- Windows: Sterna's Windows test compile is only checked by the CI's Windows cells.
  Unix-only code and its test helpers need `#[cfg(unix)]` by reading — dead code is
  an error under deny-warnings.
- Never `pkill`, `killall` or pattern-kill; kill only PIDs you started. Never read,
  print or pass on key files. No reflexive `env -u` before cargo.
- Copy is plain (no bird puns). Nothing black on black. Designed sheets, never
  one-line prompts. Delete replaced code; migrate or retire saved settings with a
  one-time notice so a settings file never stops Sterna from starting.

## Done means
Every package committed on `tui-packages` with its tests and mutation; the PR's
CI green on all twelve cells, lint and audit; the PR description lists each
package: what changed for the person, the tests, the mutation, and anything left
open with the reason.
