# The workbench

Sterna's terminal interface. It draws with ratatui and needs no browser.
Code: `crates/sterna/src/workbench/` (layout, input, sheets, themes),
`src/session/ui.rs` (the live loop).

## The screen

Four regions, drawn with lines, never with fills: the **top bar**, the
**conversation**, the **session card** on the right (shown when the
terminal is wide enough; `/sidebar`), and the **composer dock** at the
bottom.

- **The conversation is turns.** Your message under a bar labelled `you`;
  Sterna's under its mark. A cell is a card whose top edge carries its state
  and whose bottom edge says how it ended; helpers hang under the card;
  notices are tagged rows; a finished turn ends in an answer block with a
  stats line and, on the latest turn, chips for what to do next.
- **A cell shows what really happened.** The program the model wrote (a
  frame Sterna lowered from a direct tool call is marked as such), the calls
  it actually made and how each ended, its output, its before/after diff
  and its helpers. While a cell is still being written, each action gets a
  row with a live character count (`ui.stream = actions | code | raw`).
  Every cell carries the model's one-line description of what it is for.
- **The dock** carries the live status, a notice for a few seconds, an undo
  chip for the last change, the everyday chips, one hint and the context
  reading.
- **Every control is a chip**, `⟨ label ⟩`, filled when it is the current
  choice. The top bar drops chips by rank as the terminal narrows, never a
  warning.

Conversation, cell execution, local notices (the Activity log) and prompts
are separate surfaces. A question to you, an approval or a masked key entry
takes precedence over every other action.

## Mouse and keys

Mouse first, one interaction model: everything is clickable, and every
surface behaves like every other.

- A click acts on **release**; a drag selects and copies text instead.
- The wheel scrolls the open surface or the conversation, never both.
  Scrolling up stops following new output; *Latest* returns to the live
  edge.
- File paths in the conversation are underlined and open on click.

| key | does |
|---|---|
| F2 | settings |
| F3 | models |
| F4 | the selected cell's before/after diff |
| F5 | the selected cell's helpers |
| Ctrl-O | expand a cell |
| Ctrl-T | live telemetry and activity |
| Ctrl-F | fullscreen: transcript and composer only |
| Ctrl-G | release or recapture the mouse |
| Shift-Tab | cycle how often you are asked ([sandbox](sandbox.md#how-often-you-are-asked)) |
| `?` on an empty composer | every key |
| Ctrl-C | copy a selection; otherwise stop the running task |

## Sheets

Setup is a designed sheet, never a one-line prompt: `/login` (a
subscription, an API key or your own endpoint, each with its warnings),
`/key`, `/models` (the model navigator: main, helpers and subagents kept
apart; connected subscriptions first; search; measured intelligence from
the gateway where it exists, unknown where it does not), `/theme`,
`/wizard` and `/settings`. A choice saves at once and Undo reverses it; see
[configuration](configuration.md). Choosing a model in the navigator
assigns the model; the gateway still chooses the account that serves it.

## Themes

`/theme` shows the palettes grouped by family, beside a preview of the one
chosen, and `ui.theme` saves it.

- **Classic** — a palette alone: `neon`, `amber`, `ice`, `mono`, `violet`,
  `cobalt`, `mint`, `rose`.
- **Parrots** — the bird's plumage is the palette, and the bird perches on
  the session card as an 18×24 sprite in colour, with its head in the top
  bar: `amazon`, `sun-conure`, `hyacinth`, `scarlet`, `blue-gold`,
  `green-wing`, `military`, `cockatoo`.

A bird needs a terminal that shows true colour; elsewhere the palette
applies without the bird. With no theme chosen, a true-colour terminal
starts on the Amazon parrot and any other on `neon`. The sprites are traced
from photographs; the credits are in the [README](../README.md#art).

## Rules every surface keeps

- **Nothing black on black.** Every surface keeps the terminal's own
  background; the terminal owns opacity and blur. Normal prose and returned
  helper evidence are never muted; foreground roles are normal, accent,
  failure, warning, success and muted technical detail.
- **Plain copy.** Say what happened in plain words, in one voice. No puns.
- **Motion is decoration, not state.** `ui.motion` (full, calm, off)
  freezes decoration only; real state and elapsed times stay visible. The
  retired `ui.reduced_motion` is removed from a settings file with a
  one-time notice that names `/motion off`.
- **Unknown stays unknown.** A missing measurement, an unfinished helper or
  a context window nobody reported is shown as unknown, never as zero.
- **The diff is this cell's.** Before/after this cell, not against `HEAD`
  and not a proposal; missing captured bytes are reported as missing.

## Tested

`tests/workbench.rs` and `tests/tui_look.rs` pin the layouts;
`tests/tui_live.rs` drives the real binary under a PTY against a scripted
provider — keys, clicks, approvals, sheets and settings persistence.
