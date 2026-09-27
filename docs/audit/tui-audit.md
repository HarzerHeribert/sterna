# TUI click-through audit (26 September 2026)

Eight auditors drove the real TUI (pre.15, then named Pane) with keys and mouse through a pseudo-terminal, one area each: first run and sign-in, choosing models, the chips, settings, a whole conversation, session edges, a visual sweep and the keyboard matrix. Every finding was reproduced by a second agent before it counted; a coverage critic sent a second round to twelve surfaces nobody had touched.

**202 confirmed findings** (166 defects), grouped into **18 fix packages by root cause**. The user's decisions on the open questions are in [decisions.md](decisions.md). Paths below are updated to the renamed crate (`crates/sterna`); line numbers are from pre.15 and will have drifted.

## The one interaction model

## One interaction model: the Sheet

**Mouse first.** Anything a person can change is a chip or a row, and one click acts on it. The keyboard reaches the same things through one key map. There is one component, a `Sheet` with a `List` inside, and every surface is built from it: the pickers, forms, Settings, the wizard steps, the sign-in lists, the info pages, and the decision prompts (approval, ask, confirm, rollback).

```
╭────────────────────────────────────────────────────────────────╮
│ SETTINGS › Display                             ⟨ Esc · Back ⟩  │
│ ⟨ Everyday ⟩ ⟨ Display ● ⟩ ⟨ Helpers ⟩ …        ⌕ type to filter │
│────────────────────────────────────────────────────────────────│
│ › Theme        ⟨ amazon ● ⟩ ⟨ mint ⟩ ⟨ rose ⟩ ⟨ 13 more ▾ ⟩     │
│   Motion       ⟨ full ● ⟩ ⟨ calm ⟩ ⟨ off ⟩                      │
│                                                   ↓ 3 more     │
│────────────────────────────────────────────────────────────────│
│ Theme is now mint ⟨ Undo ⟩         ←→ change · Tab section · Esc back │
╰────────────────────────────────────────────────────────────────╯
```

### Anatomy
- **Header:** `TITLE › crumb › crumb`. One chip on the right says what Esc does: `⟨ Esc · Back ⟩` when there is a parent sheet, `⟨ Esc · Close ⟩` at the root.
- **Sections** (optional): chips under the header, such as categories or Main / Helper / Subagents. Every chip is clickable, and `‹` and `›` are separate targets.
- **Search** (lists of more than 12 items): `⌕ query · 30 of 474`. Control characters are stripped from typed or pasted text.
- **Body:** a List of items. It shows `↑ N more` / `↓ N more` whenever something is hidden.
- **Foot:** the notice on the left, cut with `…` so it never reaches the hint. The hint on the right is generated from the focused item's kind, never hard-coded.

### Items: every row is exactly one kind
| kind | shows | click = Enter = Space | ← → |
|---|---|---|---|
| **Value** (one choice of a setting: theme, effort, rung, mode, motion) | chips in the row; the current one is filled and marked `●` | applies now, saves to the scope the sheet names, **sheet stays open**, notice `X is now Y ⟨ Undo ⟩` | previous / next value (applies) |
| **Toggle** | `⟨ on ⟩` / `⟨ off ⟩` | flips it | flips it |
| **Open** (goes somewhere: wizard step, provider, tier, a value list too long for its row) | `title ›` | pushes a child sheet | → opens |
| **Run** (a one-shot action: sign in, try again, copy link, turn handler off) | `⏎ title` | runs it; the row shows the result (`turning off…`, `still not answering · 22:31`). The sheet closes only when the run opens something else | – |
| **Danger** (Never ask, rollback, remove) | warning tone | opens a CONFIRM child that starts focused on **Cancel** | – |
| **Info / Heading** | plain text | not focusable; focus skips it; no `›` | – |
| **Field** (forms) | `LABEL ┃value▏` | focuses the field. Enter on the last field or on `⟨ Save ⟩` submits | cursor moves within the text |

A **disabled** item stays visible, muted, and shows its reason as its detail line. Activating it puts the reason in the notice and does nothing else.

### Keys (the same on every sheet)
- **↑ ↓** move to the previous / next focusable item (no wrap). **PgUp / PgDn** move a page. **Home / End** go to the first / last item.
- **Enter** does what a click does on the focused item. **Space** does the same as Enter, except while a search query is non-empty, when it types a space.
- **← →** change the focused Value. On anything else, **→** opens and **←** goes Back. Neither does this inside a text field.
- **Tab / Shift-Tab** go to the next / previous section, wrapping. In a form every field is a section, so Tab is "next field" there too.
- **Printable keys** feed the search on sheets that have one. On short choice lists without a search, **1–9** pick the Nth choice. Other letters are ignored and never leak to the hidden composer.
- **Backspace** edits text (the search or a field) and nothing else. It never resets a setting; resetting is the `⟨ Use default ⟩` chip.
- **Esc** undoes one layer per press: cancel the field edit → clear the search → **Back** to the parent, with focus back on the row that opened the child → close at the root. Esc never makes a decision.
- **Ctrl-Z** undoes the last change made anywhere (sheet, chip or slash command). It is multi-level and covers the whole session.
- **Ctrl-C** copies when there is a selection, then clears it. Otherwise it is the global interrupt. It is never a sheet action.

### Mouse (the same on every sheet)
- **Left click** focuses the item and does what Enter does. One click, never "click to select, click again to act". The target is the whole row or card: title, detail, swatch and body.
- **Hover** highlights the target under the pointer (needs `?1003`, redrawn only when the hovered target changes). Hover never moves focus and never changes a value.
- The **wheel** scrolls the viewport 3 lines and never moves focus. If focus scrolls off-screen it is clamped back into view.
- A **click on the backdrop** outside a sheet acts as Esc. Decision prompts ignore backdrop clicks.

### Rules the components must follow
1. A sheet opens focused on the **current value**. Failing that it opens on the first actionable item, never on Info and never on Danger.
2. There is **one `SheetStack`**. When a row runs a command that produces a sheet, that sheet is pushed as a child. Back re-runs the parent's reopen action and restores focus by row id.
3. **Row actions are typed** `Action`s, never pseudo slash strings (`/copy`, `/row`, `/open-link`, `/paste-callback`).
4. Current value is marked with `●` or `· now` and focus with `›`, never by colour alone.
5. **Overflow is always visible.** Lists show more-cues. A chip set that does not fit becomes `⟨ current ▾ ⟩`, which is an Open item.
6. **A one-line result is a notice, not a sheet.** A bare command that names a setting opens that setting's row.
7. **A local control is never a turn.** Opening or closing a sheet, or running a slash command, leaves the status line and the turn clock alone.
8. **Availability is one rule.** Read-only sheets and presentation changes always work. A change the running turn cannot take is shown disabled, with one sentence saying why, before anyone clicks it.
9. **Decision prompts are Sheets too.** Choices are clickable, focusable chips, with their letter or digit accelerator printed on the chip. Keys typed before the prompt has been on screen for 500 ms are discarded.
10. A new sheet starts with an **empty notice**. Notices never carry over from the control that opened it.

## Fix packages

### P1 · Decision prompts must be answered on purpose (approval, ask, redirect, rollback)

**Severity:** critical · Red (security gate; a typed letter grants a session-wide allowance, and a reflexive Enter deletes files)

**Root cause.** The decision prompts are older renderers outside the workbench: tui.rs render_approval, tui/ask.rs render_ask and render_redirect. Their keys are matched in session/ui.rs ~1315-1425 as bare letters (o/s/a/d, and Esc or Ctrl-C mapped to Decision::Deny). A key goes to the prompt the moment approvals.front() is Some, whether or not the prompt has ever been drawn. None of the choices is focusable, clickable or labelled with what it does. The Request carries no text for the person: the Auto-review reason is dropped at approval.rs:533, the path is empty in invoke.rs:726, and the Jev score is printed as a raw number. Rollback's panel sets selected to the Confirm row (controls.rs:1567).

**Fix.** 1. **Arming and typeahead.** Record `shown_at` when a request first reaches the front and is drawn. Drop any key or paste event that arrived before shown_at + 500 ms. Drain the queued typeahead into nothing, not into the composer. Give the next queued approval a fresh arming window.
2. **Rebuild as a Sheet** (the P5 contract). Each choice is a focusable, clickable chip that shows its accelerator: `⟨ o · Allow once ⟩ ⟨ s · Allow this call for the session ⟩ ⟨ a · Another way ⟩ ⟨ d · Deny ⟩`. Enter and a click act on the focused chip. Focus starts on Allow once, and never on the session-wide or deny chips. Show `1 of 2` when more requests are queued.
3. **A readable body.** Show the tool, the project-relative path and the size, then a diff or command preview (JSON only behind `⟨ raw ⟩`). Carry `Verdict::Ask(reason)` into Request and show it as a 'why this asks' line. The Jev hint becomes words ('looks unrelated to what you asked'); the decision state gets the tool plus a path or command summary.
4. **Over 16 KiB.** Keep the summary visible. Disabled chips give the reason when pressed. The refusal sent to the model says 'too large to confirm; split it'.
5. **Esc and Ctrl-C.** Esc follows open question 1; until then, Esc = Deny once, not remembered. Ctrl-C reports ToolError::Cancelled rather than PermissionDenied and is not remembered. Ctrl-C over a live selection copies, so the selection check moves ahead of the approval branch.
6. **Remembered denials stay** (the invariant: 'a gate that answers differently on a retry teaches retrying'), but become visible. A refusal from memory carries the rule 'you denied this earlier in this session' and the path. The ASK sheet lists 'allowed for this session' and 'denied for this session', with `⟨ forget ⟩` chips (new Gate::forget and Gate::session_denials).
7. **Redirect prompt.** Reuse the composer's line editor (text plus cursor). Keep the words keyed to the request on Esc. The Event::Paste arm routes to `redirect` and `asking` before the editor.
8. **Ask panel.** Choices are clickable items. Space equals Enter. The Esc label reads 'let Sterna decide'. The legend uses a readable muted foreground (see P16).
9. **Rollback.** Focus Cancel (`rows.len()-1`). Esc runs '/rollback cancel' so the pending state clears. Confirm is a Danger item.
10. **Delete** render_approval's square LightGreen block and the key loop; the Sheet replaces them.

**Smallest failing test.** tests/tui_live.rs `an_approval_ignores_keys_typed_before_it_was_shown`: set up approval_provider with a write cell on the manual rung, send "write it\r", then send "also please make sure" one byte every 80 ms. Assert that the target file does not exist and that the approval is still on screen. Today the 's' allows the call and the file is written. The cheapest companion is a unit test in session/controls.rs: the rollback panel's `rows[selected].command == Some("/rollback cancel")`.

**Findings it closes:**

- Typing ahead answers an approval that appears mid-word; an 's' allows the call for the whole session
- Rollback opens with 'Confirm rollback' selected, so /rollback, Enter, Enter deletes files
- d, Esc and Ctrl-C deny the exact call for the rest of the session, silently; a retry fails without asking
- No surface lists or revokes calls allowed with [s] or remembered denials; Gate::session_actions() has no caller
- Ctrl-C at an approval is recorded as a Deny, which differs from Ctrl-C during a running call
- Ctrl-C with a live selection denies the call when an approval is up, instead of copying
- Approval sheet ignores the mouse and Enter, Esc denies, the body is raw JSON with absolute paths, and the frame is off-theme and bleeds into the sidebar
- An action over 16 KiB can never be approved, and the modal does not say which call it is
- The Jev hint is a bare, uninformed number, and the decision model never sees the call's arguments
- On Auto-review the approval never says why this call needs a person
- Approval scrolling overshoots; End does nothing
- Two approvals from one cell swap in place with no counter; a quick double press leaks into the hidden draft
- With the hint line on, the approval footer loses its last line at 80x24
- A paste in the 'another way' prompt goes into the hidden composer draft
- The 'another way' prompt has no cursor movement, and Esc drops the typed words
- Ask sheet ignores the mouse, Space does nothing, and the Esc label says 'decide yourself'
- Denying an approval shows write("") and 'host call gate' jargon
- Never-asks CONFIRM starts with Confirm focused

**Files:** `crates/sterna/src/session/ui.rs`, `crates/sterna/src/tui.rs`, `crates/sterna/src/tui/ask.rs`, `crates/sterna/src/approval.rs`, `crates/sterna/src/tools/invoke.rs`, `crates/sterna/src/permissions.rs`, `crates/sterna/src/session/controls.rs`, `crates/sterna/src/workbench/sheets.rs`, `crates/sterna/src/workbench/view.rs`

### P2 · The Auto rung hard-denies the read-only commands it promises to run

**Severity:** high · Red (sandbox/permission semantics; the tour's first turn fails)

**Root cause.** Profile::admits_command in sandbox/profile.rs (~892-950) denies any command segment that matches no `Bash(...)` pattern in command_allow. With the default of zero patterns, every command is a hard PermissionDenied at the profile layer. The permission ladder's read-only / ask judgement (gate.with_read_only(config.modes.explore.commands), permissions.rs judge_command) is therefore never reached, while startup.rs permissions_line and the sidebar promise that 'a command that only reads runs, anything else is confirmed'.

**Fix.** Pick one of the two routes in open question 7. The recommended one follows the 2026-09-19 ruling 'the Sterna sandbox follows Claude Code: call-level judgement, not an OS cage'. On a rung other than manual, a command_allow miss is no longer a terminal deny; it becomes 'unlisted' and falls through to the ladder. There, command_reads_only means run, and anything else means Verdict::Ask (on Auto-review, the reviewer, then the person). An explicit `permissions.deny` match still hard-denies. The alternative seeds the default command_allow from modes.explore.commands. Either way, permissions_line and Rung::sentence must be generated from the same predicate the gate uses, so the promise cannot drift from the behaviour.

**Smallest failing test.** tests/sandbox_profile.rs (or competitive_approvals.rs) `auto_rung_runs_a_read_only_command_with_no_allow_patterns`: default Profile (0 command patterns), Ladder at Rung::Auto, gate.with_read_only(default explore commands); invoke bash `ls -la && git log --oneline -3`. Assert the outcome is not PermissionDenied: it either runs or returns Ask. Today it returns PermissionDenied('no `Bash` pattern…'). The mutation is to restore the hard deny in admits_command.

**Findings it closes:**

- The start card promises reading commands run, but the first read-only command (`ls -la && git log --oneline -3`) is denied outright with no question
- /status shows 'Sandbox: 0 path rules · 0 command patterns'

**Files:** `crates/sterna/src/sandbox/profile.rs`, `crates/sterna/src/permissions.rs`, `crates/sterna/src/tools/invoke.rs`, `crates/sterna/src/session/startup.rs`, `crates/sterna/src/sandbox/modes.rs`

### P3 · Local controls are run and reported as model turns

**Severity:** high · Amber

**Root cause.** Two causes. First, any slash text the workbench does not answer is sent over `Input::Submit`. The UI then sets `busy = true; activity = Thinking; task_started; pulse` (session/ui.rs ~2003-2020), and session.rs `drive()` (~968-981) publishes `Activity::Complete` after every `process_input`, whether or not a model ran. Second, `tui::Activity` has no Stopped or AwaitingYou variant, so every ending reads 'complete': stop, cancel, Ctrl-C, and a wait on approval or ask. The workbench also refuses every `Action::Command` while busy without checking what the command is (input.rs:974), and the same refusal is written five different ways.

**Fix.** 1. **Controls report no turn.** `process_input` returns `Ran::Turn(Result)` or `Ran::Control(Result)`. `drive()` publishes Complete or Failed only for a Turn. A Control publishes a new `Update::ControlDone`, which clears `busy` and leaves `activity`, `task_started` and `pulse` untouched.
2. **Slash text starts no turn in the UI.** In session/ui.rs, slash text does not set Thinking or task_started. The sidebar clock line prints only while a turn runs or just ran (view.rs ~843-849).
3. **New Activity states.** Add `Activity::Stopped { by: You | Interrupt }`, published when stopped_by_request or INTERRUPT fired (session.rs ~1178, ~1771, ~1804), and `Activity::AwaitingYou`, set while an approval or ask is pending. AwaitingYou pauses the pulse clock and gives the card 'waiting for you'. Complete is gated on `behind` (the check lane) being empty.
4. **Ctrl-C mid-turn** posts 'Stopping · Ctrl-C again within 2 s quits', the same way Esc posts its note.
5. **Busy gate.** Read-only views get their own actions (Action::Telemetry, Action::Tab(cell, Diff) for 'open diff'), so the gate never sees them. Everything else draws its refusal from one `voice::BETWEEN_TURNS` sentence, shown on the disabled item before a click (P5 rule 8). The model row's description stays true: a model change applies to the next request (see open question 9).
6. **Queue.** Hold queued prompts in `state.queued` and send them on the turn's end, so Esc can take one back. Otherwise drop the hint (open question 8). Clear the queue notice when the queue drains.
7. **/exit** while busy is honoured: interrupt, then exit.

**Smallest failing test.** tests/tui_live.rs `a_sheet_opened_and_closed_is_not_a_turn`: App::start; send "/help\r"; wait for "Commands"; send "\x1b"; settle 500 ms; refute("complete") and refute("on this turn"). Today both strings appear. Second test: with a held provider, send a prompt, Esc, then Esc again; contains("stopped") and refute("complete").

**Findings it closes:**

- Opening and closing a sheet (/wizard, /login, /key, /config, /help, /models) reports '✓ complete ✓' and '00:00 on this turn'
- Stopped, cancelled and Ctrl-C'd turns all read complete; Ctrl-C posts no note
- The card says RUNNING and the timer counts while the turn waits for the person's approval or answer
- '✓ complete' shows while the completion check is still running
- Mid-turn, the telemetry chip and 'open diff ↗' are refused as a 'runtime change' though their key twins work
- Five wordings for 'not during a turn', and the Main model row promises the opposite
- WORK rows look live mid-turn but refuse; ASK rows apply at once
- 'Esc takes the last one back' is false: queued messages are already sent
- /exit is refused while working
- A failed /subagents or /config shows ERROR under '✓ complete'
- Doubled check mark '✓ complete ✓' (the playful string; disappears with the voice deletion package)

**Files:** `crates/sterna/src/session.rs`, `crates/sterna/src/session/ui.rs`, `crates/sterna/src/tui.rs`, `crates/sterna/src/workbench/input.rs`, `crates/sterna/src/workbench/view.rs`, `crates/sterna/src/workbench/voice.rs`, `crates/sterna/src/workbench/document.rs`, `crates/sterna/src/tui/ribbon.rs`

### P4 · The transcript confuses cells, answers and synthetic messages

**Severity:** high · Amber (a user message disappears from view)

**Root cause.** Indexing and identity in workbench/document.rs and the Notebook have four faults.
- The `feedback` flag set for runtime feedback is not cleared when a historical message is skipped (`if m.historical.is_some() { continue; }` comes before the User branch, ~284-301), so the next real prompt is swallowed.
- `last_assistant` (~277) selects the synthetic after-return message that session.rs ~1755 appends, so `latest` is never true on a real answer.
- `n.cells` also holds the plain-answer entry, so every 'latest cell' fallback lands on a phantom: `selected_cell.unwrap_or(n.cells.len())` for F4, F5 and Ctrl-O, the sidebar's `n.cells.last()` helpers, 'SO FAR 2 cells', and 'executing cell len()+1'.
- A bare `/cell` still sets the old `state.inspection`, which is no longer drawn but still captures keys.

**Fix.** 1. **Feedback flag.** Reset `feedback` (and `pane_open`) whenever a historical message is skipped, or scope the flag to the one message that immediately follows the feedback.
2. **Latest answer.** Compute `last_assistant` over the messages that actually render. Skip a trailing after-return message whose prose equals the returned text, which is the same dedup rule as ~303-310.
3. **Program cells.** Give the Notebook a `program_cells()` view (entries with a program), plus `last_program_cell()` and `last_with_helpers()`. F4 and Ctrl-O fall back to `last_program_cell`, F5 to `last_with_helpers`. The sidebar counts and helper rows read the same view. The status label uses len() while Executing and len()+1 only while Streaming.
4. **Keyboard cell selection.** Alt-↑ / Alt-↓ move `selected_cell` between cards. The selected card keeps a `›` glyph even when it failed.
5. **Card diff chip.** 'open diff ↗' becomes `Action::Tab(cell, CellTab::Diff)`. The tabstrip never renders a Diff body without its tab.
6. **Bare /cell.** Add a `["/cell"]` arm that expands the latest program cell. `/cell <word>` gets 'use /cell <number>'. Delete `state.inspection`, `open_cell`, the Inspection key interceptor (ui.rs ~1467-1510) and the render_screen-only overlay (no legacy compat).

**Smallest failing test.** tests/workbench.rs `a_prompt_after_a_cell_turn_is_shown`: build a Conversation [User m1, Assistant plain, User m2, Assistant with execute_cell ToolUse, Message::runtime(feedback, historical), Assistant 'Wrote a.', User 'message 3', Assistant 'Plain three.'] with one CellView. Assert that words(doc(...)) contains "message 3". It is missing today.

**Findings it closes:**

- The user message sent right after a turn that ran a cell is never shown (queued follow-ups too)
- The 'show the diff · commit this · full output' chips never appear
- 'SO FAR 2 cells' after a one-cell turn; F4, F5 and Ctrl-O act on a phantom cell
- When the turn ends, the sidebar forgets the helper calls and says 'none yet'
- F5 does nothing after the turn ends until a card is clicked
- The label says 'executing cell 002' while the only card is 001
- 'open diff ↗' on an earlier cell opens the latest cell's diff
- Bare /cell opens an invisible modal that swallows keys and the first Esc-to-stop
- '/cell abc' does nothing; bare /cell with no cells says 'at that number'
- No way to select a cell from the keyboard; selection is invisible on failed cells

**Files:** `crates/sterna/src/workbench/document.rs`, `crates/sterna/src/workbench/input.rs`, `crates/sterna/src/workbench/view.rs`, `crates/sterna/src/session/ui.rs`, `crates/sterna/src/session.rs`, `crates/sterna/src/tui.rs`

### P4b · An answered ask() ends its cell as an unexplained termination

**Severity:** high · Amber (every successful ask shows ✕ FAILED)

**Root cause.** runtime/bindings/ask.rs ask_callback records the question and then calls scope.terminate_execution() without first calling `trace(scope).request_yield(..)`. The yield_now and answer callbacks both call it before terminating. Because no yield is marked, isolate.rs's Ending::Terminated arm (~1886-1897) reports Threw(RuntimeTerminated). Separately, the card has no row showing what the person chose.

**Fix.** In ask_callback, call trace(scope).request_yield(None) before terminate_execution, the same way answer_callback does. Add a card row '? <question> → you chose: <answer>' (or '→ Sterna decided') from the recorded ask.

**Smallest failing test.** tests/ask_user.rs `an_answered_ask_is_not_a_failed_cell`: run a cell that calls `ask("Which?", ["a","b"])` with the gate answering 'b'. Assert the cell outcome is not Threw(RuntimeTerminated) and that the model feedback does not start with '[cell 1 threw'. Mutation: remove the request_yield call.

**Findings it closes:**

- A question answered correctly is shown as a FAILED cell, and the answer never appears; Esc gives the same FAILED card

**Files:** `crates/sterna/src/runtime/bindings/ask.rs`, `crates/sterna/src/runtime/isolate.rs`, `crates/sterna/src/workbench/document.rs`

### P5 · One Sheet component and a sheet stack (the interaction model itself)

**Severity:** high · Amber (the user's core complaint; P1, P6 and P9 build on it)

**Root cause.** There is no shared sheet component. Workbench keeps six flags (activity, access, work, approvals, help, confirm), Option<Preferences> and Option<Navigator>, and dispatches ScreenState.panel on title strings ("Themes", "Setup…") in view.rs surface(). Each has a private key match in input.rs, and session/ui.rs has its own for forms, approvals, ask, redirect, telemetry and an older Panel handler. `close()` wipes everything, so no sheet has a parent and Esc can only close all of them. The hints are hard-coded strings. The wheel moves the selection 3 rows (`move_index(..,3,..)`). Panel rows are `{text, command: Option<String>}`, so action rows are pseudo slash strings, info rows get a cursor, and Panel::text sheets (/help, /status) can never act. Mouse reporting enables ?1000/?1002/?1006 only, so there is no hover. Some older code is unreachable: the Panel search, stage and tier block in session/ui.rs (~1538-1640), settings_session::Editor with src/settings_ui.rs, and the ui.rs /theme branch.

**Fix.** Land in four commits, each with the targeted gate plus tui_live and pane_launch (they pin screen strings and are not traced to workbench files).

1. **The component.** In workbench/sheet.rs, define `Item { id, title, detail, kind: Info|Heading|Value{values,current}|Toggle(bool)|Open|Run|Danger|Field(FieldState), action: Option<Action>, disabled: Option<String> }` and `Sheet { title, crumbs, sections, section, items, focus, scroll, query: Option<String>, notice, reopen: Option<Action> }`. Add `fn key(&mut Sheet, KeyEvent) -> Outcome` and `fn mouse(&mut Sheet, &Geometry, MouseEvent) -> Outcome` (Outcome = Nothing | Act(Action) | Back | Section(i)), which implement the key and mouse tables verbatim. Add `fn draw(...)`, which produces the header chip, section chips, search line, more-cues, notice clipped to (width − hint − 2) with '…', and the hint generated from the focused kind. Workbench gains `sheets: Vec<Sheet>`. `close()` becomes pop, and `close_all()` is explicit. Paste into a query strips control characters.
2. **Port the local sheets.** WORK, ASK, effort, ACCESS, CONFIRM, KEYS (generated from one keymap table that the help sheet and the tests share), ACTIVITY, telemetry (with a Back chip; hits are no longer cleared) and every tui::Panel. PanelRow gains `kind` and a typed `action: Option<Action>`. Add Action::OpenLink, Action::Copy(String), Action::PasteCallback and Action::HandlerOff(name), and remove the `/copy`, `/row`, `/open-link` and `/paste-callback` string parsing in ui.rs 1205-1230 and 1637-1660. Panel::text rows become Info items, and /help builds Run items. Session-side panels arrive through Update::Panel, which pushes a child when the command came from a sheet row: the parent's `reopen` is the command that built it, and focus is restored by item id. Wizard, themes and forms get Kind-driven layouts. Themes follow open question 4 (default: click applies live and the sheet stays open; the swatch and name are one target). Forms become Field items with click-to-focus. The first-start wizard stops taking keys: it shows a one-line 'finish setup' chip instead of opening (setup.rs at_start). A one-line result such as `/permissions <rung>` becomes a notice. A Run item shows pending state (the handler row reads 'turning off…', and a duplicate is refused).
3. **Hover and wheel.** Enable ?1003 (reset it too). Handle MouseEventKind::Moved by storing `hover: Option<Rect>` and redrawing only when the hit changes. The wheel scrolls `Sheet.scroll`. move_index(…,3,…) goes away.
4. **Delete the old paths.** The session/ui.rs Panel key and paste block, the settings_editor with settings_session::Editor and src/settings_ui.rs (tests/settings_ui.rs goes with them), the unreachable ui.rs /theme branch, Panel::stage, staged_commands, cycle_tier, move_provider, cycle_order and the search_* functions once nothing calls them, and the six Workbench flags. Grep `crates/*/tests` for 'Esc closes', 'saved choices stay', 'Esc · Back' and 'finish later', and run every target found (the CLAUDE.md contract-change rule).

mod.rs stays a dispatch layer (rule 8). The grammar lives in sheet.rs, and Preferences and Navigator build their Sheet from their own data.

**Smallest failing test.** tests/workbench.rs `every_sheet_speaks_one_grammar`, table-driven over these openers: click(Action::Approvals) with s.permissions at Auto; click(Action::Work) with mode Plan; s.panel = Theme::picker(current); s.panel = the wizard overview; click(Action::Help); Action::Settings. For each, assert:
(a) the focused item is the current value (ASK focuses Auto);
(b) key(Space) and key(Enter) give the same Effect on clones;
(c) a ScrollDown mouse event leaves focus unchanged;
(d) key(End) then key(Home) reach the last and first actionable items;
(e) Esc on a child pushed from a parent row reopens the parent with focus on that row.
Today (a) already fails on ASK: Enter drops Auto to Manual.

**Findings it closes:**

- Each setup sheet has its own navigation concept (Tab, Space, ←→ mean different things per sheet)
- Esc is labelled 'Back' but closes the whole setup stack; one sheet names Esc three ways
- One mouse-wheel notch jumps the selection three rows, so some rows cannot be reached by wheel
- No chip or row reacts to hover
- The Ask and Work sheets (and effort) open on row 1, not the current value, so Enter changes the setting
- In the Access sheet the arrows highlight the buttons but Enter and Space do nothing
- /help is a dead end; the KEYS rows are not clickable; info sheets show a › cursor that does nothing
- Wizard sub-steps: the cursor starts on a description row, gaps are uneven, Back is missing, focus is lost on return
- Every sheet ends with 'Esc closes · saved choices stay', and a long notice overprints it
- Clicking '‹' in Settings goes to the next category
- The theme swatch is not clickable; a click applies and closes, so there is no mouse preview; no key legend
- Themes and panels ignore Home, End, Tab and Space
- Telemetry has no mouse exit, passes keys to the hidden composer, and leaves stray ┬ ┴ joints
- The sign-in forms ignore the mouse entirely; Esc in a form closes to chat
- Models: Esc drops a typed search; Shift-Tab, Home and End do nothing
- A pasted newline stays invisibly in the search query
- A refusal from one chip shows up as the next sheet's own message
- '/permissions manual' answers with a full-screen one-line sheet that eats what is typed next
- /copy, /row, /open-link and /paste-callback say 'unknown command' when typed
- The wizard opens itself at start and swallows whatever the person types first
- The /handlers row stays 'active' after 'off', a second press duplicates it, and the cursor starts on the header
- The 37-provider key list has no search and no more-cue; the subscription list has no hints or filter

**Files:** `crates/sterna/src/workbench/sheet.rs (new: Item, Kind, Sheet, key/mouse → Outcome, draw)`, `crates/sterna/src/workbench/mod.rs`, `crates/sterna/src/workbench/input.rs`, `crates/sterna/src/workbench/view.rs`, `crates/sterna/src/workbench/sheets.rs`, `crates/sterna/src/workbench/chrome.rs`, `crates/sterna/src/tui/controls.rs`, `crates/sterna/src/tui/form.rs`, `crates/sterna/src/session/ui.rs`, `crates/sterna/src/session/ui/terminal_input.rs`, `crates/sterna/src/session/controls.rs`, `crates/sterna/src/session/setup.rs`, `crates/sterna/src/session.rs`, `crates/sterna/src/settings_ui.rs (delete)`, `crates/sterna/src/settings_session.rs`, `crates/sterna/tests/settings_ui.rs (delete with its module)`, `crates/sterna/tests/workbench.rs`

### P6 · Settings edits a file snapshot instead of the running session

**Severity:** high · Amber

**Root cause.** Preferences (workbench/settings.rs) sets every value from `effective()` = `self.loaded.values`, the merged config layers, and never from the live session. So values changed by chips, Shift-Tab or /mode show stale. It saves to the selected scope without checking precedence. Undo is one `Option<Vec<..>>` inside the Preferences object, and `Workbench::persist` builds a temporary Preferences and drops it, so every 'Ctrl-Z undoes it' outside the sheet is false. Some keys are special-cased into raw `key = value` staged edits (`permissions.*`, `agents.mode`). Backspace with an empty query means 'save None'. The Advanced filter is `!spec.basic`, which exposes internal keys. Validation errors come back as raw TOML or parser text.

**Fix.** 1. **Live values.** `effective(key)` first asks a `LiveValues` snapshot taken from ScreenState (effort, mode plus pinned, rung, theme, motion, sidebar, stream, statusline), then falls back to config. The detail line names the true origin with `p.origin(key)`. When the edited scope is shadowed, the row says 'Project sets rose; this Global value applies elsewhere'. `cycle()` steps from the scope's saved value.
2. **One undo stack.** Move undo to `Workbench.undo: Vec<Change>` for the whole session, shared by sheets, chips and slash commands. `persist()` pushes to it. The Undo chip is disabled when the stack is empty, and 'Restored.' names what came back.
3. **One route for rung, mode and effort.** permissions.mode, session.mode and agents.mode rows call the P8 setters. Delete the `permissions.* || agents.mode` special cases (settings.rs ~252 and input.rs ~877). Full routes to the Danger confirm. Rows whose value cannot be set are disabled with a reason: permissions.full_access in Project scope ('global only · F6'), and slot efforts without a model.
4. **Keys.** Backspace never resets; a `⟨ Use default ⟩` chip in the detail line does. Space follows P5. Enter on a Value row cycles and the legend says so.
5. **Field editor.** An inline Field item with a cursor and the range shown ('1–64'). The edit opens with its value selected, so a paste replaces it. A multi-line paste into a list field splits into items. Errors come back in words.
6. **Delete the legacy key.** Remove ui.reduced_motion (no legacy compat): the registry row, the presentation() override, and the /motion double-persist.
7. **Advanced.** Add a `hidden` flag to SettingSpec for wizard.seen and legacy.imported. Keys already in another category are not repeated. Thresholds go in a 'Tuning' sub-sheet.
8. **Notices.** Use human_value in notices. The header and F2 hint are derived from `applies_now` ('saves itself; most choices apply now').
9. **Bare commands and words.** A bare /motion, /sidebar, /stream, /settings <word> or /config <word> opens Settings on the matching row. An unknown word says 'no setting named X · opened Settings'. Ctrl-B persists and notes the change the way /sidebar does. Fix the /stream usage string and add /stream to the slash catalogue.
10. **Long chip sets** collapse to `⟨ current ▾ ⟩` (P15).

**Smallest failing test.** tests/workbench.rs `backspace_on_an_empty_search_never_resets_a_setting`: let (_t, mut s, p) = prefs(); save ui.theme="rose"; set u.preferences = Some(p); type 'mo' and press Backspace three times; assert that Preferences::saved("ui.theme") == Some("rose"). Today the third Backspace unsets it. Companion: set s.effort = Medium via Action::Effort, then open Settings; the effort row's lit chip is 'medium'.

**Findings it closes:**

- Settings shows stale effort, mode and rung after the chips, Shift-Tab or /mode change them
- One Backspace too many after clearing a search resets a setting without asking
- Space starts an invisible search; the legend's 'Enter Edit' actually changes the value
- Undo is one level, and a second Undo looks like it worked
- 'Ctrl-Z undoes it' is promised after /theme, /sidebar, /statusline, /motion and /stream, but does nothing
- Editing Global while the project overrides it announces a change that never happens and names the wrong origin
- Two controls set motion; the legacy ui.reduced_motion silently wins
- Full access is offered in Project scope and refused only after Enter, with a truncated, sticky error
- The field editor is a raw 'key = value' line with no cursor and no range; a paste appends to the prefilled value; list newlines are dropped
- Advanced is 68 rows, including internal (wizard.seen, legacy.imported), duplicate and raw threshold keys
- Descriptions are cut mid-sentence; notices show raw values ('Sidebar is now hide', 'Theme is now unset')
- 'Everything there applies right now' contradicts rows that apply next session
- '/settings theme' opens a dead-end sheet; /config arguments answer with CLI --help
- Four display commands (/motion, /sidebar, /statusline, /theme) have four different bare behaviours; Ctrl-B does not save
- The /stream usage names the wrong words, and /stream is missing from the slash list
- Six of sixteen themes are off-screen in the Theme row, and the chosen one lights no chip

**Files:** `crates/sterna/src/workbench/settings.rs`, `crates/sterna/src/workbench/input.rs`, `crates/sterna/src/workbench/view.rs`, `crates/sterna/src/settings/registry.rs`, `crates/sterna/src/settings.rs`, `crates/sterna/src/settings_session.rs`, `crates/sterna/src/config.rs`, `crates/sterna/src/session/controls.rs`, `crates/sterna/src/tui.rs`

### P7 · First-run setup writes to the project and skips the live session

**Severity:** high · Amber

**Root cause.** session/setup.rs writes the wizard's picks to `Scope::Local`: controls::assign_model reads and writes Local (controls.rs:251), apply_recommended writes at :348, and jev_on at :420. Only wizard.seen goes global, so every new project reopens setup, and `.pane/` appears as an uncommitted change in the user's repository. apply_models calls assign_model, which updates settings but not `session.model` or `ui.model()`; the hand-picked /model path does both (session.rs ~2577-2587). apply_models also calls apply_recommended, which silently switches on helpers and Jev. The opening chips are all `Action::Insert`, so a command chip types text instead of running. The opening block renders only when no other row exists (document.rs ~584), and project_suggestions counts Sterna's own .pane/ files as dirty.

**Fix.** 1. **Scope** (open question 6; default global). assign_model takes a Scope. The wizard, /model and /models write Global, and only Settings in Project scope writes Local.
2. **Live model.** Factor the /model parent path (settle_model, then assign, then `*session.model.borrow_mut()`, then `ui.model()`) into one `controls::use_model(session, tier, id, scope)`, and call it from apply_models and the hand path.
3. **Picks without side effects.** apply_models stops calling apply_recommended. If 'Sterna's picks' should include helpers or Jev, the row says so in its detail line, and Jev stays step 3.
4. **Choosing by hand.** 'Choose each one myself' opens /models as a child of the wizard, starting on Sterna's recommended models, sorted first. It walks Main, then Helper, then Subagents, and returns to the wizard (P5 stack, P9 picker staying open).
5. **Opening chips.** Command chips (finish setup, /wizard) carry Action::Command and run. Prompt chips insert only into an empty composer; otherwise they append after a blank line, with the draft recoverable through the composer undo (P10). The setup chip is recomputed on every idle publish while setup is incomplete, including after the auto-wizard. The opening block renders above notes rather than only when rows are empty.
6. **Dirty count.** project_suggestions uses `git status --porcelain -- . ':(exclude).pane'`.

**Smallest failing test.** A unit test in session/setup.rs (temp project root plus temp global dir): run apply_models with fixed picks. Assert that Store::read(Scope::Global) holds model.parent, that `<root>/.pane/config.toml` does not exist, and that `*session.model.borrow() == picks.main`. Today the file is local and session.model is empty.

**Findings it closes:**

- Setup is saved per project: the wizard returns in every new project and writes .pane/ into the repo
- After 'Use Sterna's picks', the header, sidebar and /status still say no model is chosen
- 'Use Sterna's picks' also turns on Jev decisions and helpers without saying so
- 'Choose each one myself' opens a raw 474-model list alphabetically and exits after the first choice
- The way back to setup disappears: no 'finish setup' chip after the auto-wizard, and any note wipes the chips
- The 'finish setup' chip only types /wizard; it does not open setup
- Clicking a suggestion chip silently replaces what the person already typed
- 'review 1 uncommitted change' on a clean repo counts Sterna's own .pane/ directory
- The Main model chosen in /models is saved only to this project

**Files:** `crates/sterna/src/session/setup.rs`, `crates/sterna/src/session/controls.rs`, `crates/sterna/src/session.rs`, `crates/sterna/src/session/ui.rs`, `crates/sterna/src/workbench/document.rs`, `crates/sterna/src/workbench/voice.rs`, `crates/sterna/src/settings/registry.rs`

### P8 · One fact, one name, one setter: rung, mode, effort, helpers in the chrome

**Severity:** medium-high · Amber

**Root cause.** Each surface computes the same session fact its own way. The greeting's permission line is `startup.first()`, frozen from session start (document.rs ~1158). The sidebar shows `effort.sent_for(model)` while the ladder steps the stored value. The strip chip renders an empty string at Effort::Default, and the sidebar line 'effort · helpers' is one Action::Effort target. `helpers_on` (enabled and model) disagrees with Settings' plain helpers.enabled. mode_pinned never reaches ScreenState. The rung has three setters with different persistence and confirmation: Action::Rung (session only, CONFIRM), Settings (a raw staged edit, persisted, no confirm) and /permissions (no confirm, one-line sheet). Names are printed from internal keys (execute, manual, accept-edits, full, roster) at about ten sites instead of from Rung::label, a Mode label and so on.

**Fix.** 1. **One setter per fact.** `set_rung(rung, from)`, `set_mode(mode, pinned)` and `set_effort(e)`, each in the module that owns the fact, not in mod.rs. Every entry point calls it: chip, sheet, Settings row, Shift-Tab and slash command. Each setter persists the same way (open question 6 decides the scope), confirms Full the same way (one Danger CONFIRM whose focus starts on Cancel), and emits the same notice.
2. **Names from one source.** Rung::label and sentence, Mode::label ('Build / Explore / Plan', plus '· auto' when unpinned) and Effort::label are the only printed names. The completion table, /status, notices, the sidebar and Settings chips all use them (open question 11 picks the words). /permissions and /mode accept both the label and the config key.
3. **Rendered live.** The greeting's permission line is rendered from `s.permissions`. It is a clickable Open item to ASK and shows its full sentence on hover, or wraps. ScreenState carries `mode_pinned`.
4. **Effort.** The effort chip is always present, reading `effort default (low)` when the wire value differs. The ladder steps from the stored value. The sidebar splits into two targets: effort steps, and 'helpers …' opens Little helpers.
5. **Helpers.** Settings shows 'On, but no helper model chosen · ⟨ choose ⟩' when enabled without a model.
6. **Sidebar helper lines** get Action::Helper(cell, i).
7. **/status** is rebuilt from the same live facts, and its info rows are not focusable.
8. **Mode chip** gets rank 2 and a Warning tone for Plan and Explore, so it is never the first dropped.

**Smallest failing test.** tests/workbench.rs `the_greeting_names_the_rung_in_force`: take the fixture ScreenState with the startup note from startup::permissions_line at Auto; set s.permissions to Manual; draw at 140x42. Assert that the text contains Rung::Manual.label() on the card row and does not contain "permissions: auto". Companion: with s.effort = Effort::Default, draw; assert an 'effort' chip is in the dock rects in g.hits.

**Findings it closes:**

- The greeting keeps the old permission rung after Shift-Tab or the ASK sheet
- Clicking the 'permissions: auto — …' line gives a quip instead of opening permissions; the line is cut off and can never be read in full
- One permission setting goes by at least six names; the Auto rung is described two ways; the /permissions completion text is wrong
- The work mode is 'Build' on the chip and 'execute' everywhere else
- The effort chip vanishes when it cycles back to default; the effort sheet opens on 'default'
- Clicking 'helpers off' in the sidebar changes the effort
- With gpt-5.5, the first click on 'effort low' seems to do nothing
- Settings says Helpers On while the sidebar says off, with no reason
- '/mode auto' sets a mode that no chip, sheet or /status shows
- /status leaves out most chip values and prints 'network false'
- A rung picked on the ASK sheet is not saved, though the sheet says 'saved choices stay'
- Never asks has three different confirmation flows
- Rung changes in Settings become a raw TOML edit, and 'full' skips the confirm
- Sidebar helper lines cannot be clicked
- The mode chip is dropped first at 80 columns, so Plan and Explore have no marker

**Files:** `crates/sterna/src/workbench/view.rs`, `crates/sterna/src/workbench/document.rs`, `crates/sterna/src/workbench/input.rs`, `crates/sterna/src/permissions.rs`, `crates/sterna/src/session/startup.rs`, `crates/sterna/src/session/controls.rs`, `crates/sterna/src/sandbox/modes.rs`, `crates/sterna/src/session/mode_proposal.rs`, `crates/sterna/src/tui.rs`, `crates/sterna/src/wire.rs`

### P9 · The model picker closes on every choice and ignores tiers, ranking and capability

**Severity:** medium-high · Amber

**Root cause.** Navigator (workbench/models.rs) and its input arms are a separate state machine. Action::ChooseModel and Action::Command both call `self.close()`, so every commit, slot fill or toggle leaves the sheet. Tab and Action::ModelRole set `selected = 0` instead of `select_current()`. Action::Model(i) only moves the highlight. candidates() filters by substring with no relevance order. catalogue_len counts unselectable groups. There is no chat-capability filter anywhere between entitlements and assign_model. Action::Sources and Action::Scores exist but nothing builds them. The subagent assignment has two UIs (/models Subagents and Settings › Subagents) with different vocabularies, and config/agents.rs rejects intermediate states with raw errors. '/model helper' is parsed as a Main model id, and settle_model accepts any string.

**Fix.** Rebuild the picker as a P5 Sheet with sections Main, Helper and Subagents.
1. **Choosing.** A click or Enter on a model chooses it for the section's tier or slot. The sheet stays open and moves on: on Subagents, to the next empty slot. The notice names what changed ('QUICK is now claude-haiku-4-5 · low'). Tab and ModelRole call select_current(). The current row is marked `● now`. 'Now:' shows 'not chosen yet' when empty.
2. **Ranking.** Search ranks exact id, then prefix, then token-and-number match, then substring. The count is 'N of M in scope'. More-cues and sticky group headers come from the component. Locked rows are muted and non-choosable, with one `⟨ Sign in to Groq ⟩` Run item. Names are clipped before the status column.
3. **Capability.** Filter non-chat ids: gpt-image-*, whisper-*, :batch, codex-auto-review. Prefer a chat capability flag in the gateway's entitlements --json, with a pane-side filter until then. assign_model and /model refuse an id that is not in served_models when a catalogue is available.
4. **Visible toggles.** `⟨ connected accounts | all accounts ⟩` and `⟨ order: name | intelligence ⟩` chips are wired to Action::Sources and Action::Scores.
5. **Subagents.** One place: the strip chip, the sidebar line and Settings › Subagents rows all open this section. One word, per open question 11. A slot card is one target with an inline effort Value (low…max) instead of the silent per-slot default. Filling a slot while off offers `⟨ turn favourites on ⟩`. The toggle is disabled with a reason while every slot is empty, and a raw config error never reaches the screen. Pinning over favourites asks first. The PINNED card shows the pinned model or 'none'. Settings › Subagents keeps only the mode row, which links here.
6. **Commands.** Bare /subagents opens this section. `/model helper` or `/model subagent` without a name opens that section.

**Smallest failing test.** tests/workbench.rs `switching_tier_selects_that_tiers_current_model`: take navigator() with current.helper = Some("gpt-5.6-luna") present in the catalogue; set u.models = Some(nav); key(Tab). Assert that candidates()[selected].model == "gpt-5.6-luna". Today it is row 0. Companion: click(Action::Model(i)) on the selected row returns an Effect::Command and u.models stays Some.

**Findings it closes:**

- Switching tier moves the highlight to the first row (claude-3-5-haiku), not the tier's current model
- Clicking a model only highlights it; a second click does nothing
- Filling a subagent slot closes the picker, and it reopens on Main
- A filled slot leaves subagents OFF; 'turn them on' closes the sheet and prints a jargon error
- Choosing a PINNED model silently switches favourites off; the PINNED card shows 'favorite roster'
- No way to choose a slot's effort in the picker; the card truncates the effort away
- The status-strip subagents chip opens a second, differently navigated UI in Settings
- 'Use the inherited value' contradicts 'an empty slot never inherits Main'
- Favourite effort chips look live but do nothing until a model is set
- Subagent mode offers pinned and roster, which are then refused with raw TOML errors
- `/subagents` alone errors with a truncated usage line instead of opening the picker
- `/model helper` and `/model subagent` set the MAIN model to 'helper'; any string is accepted
- Image, speech and batch models are listed as choosable and accepted
- Search is unranked: 'sonnet 5' puts claude-3-7-sonnet first; '0 of 474' counts out-of-scope groups
- Locked rows look selectable, names run into 'locked', the reason shows twice
- 'All accounts' and 'by intelligence' have no clickable control and show no state
- The list is cut off with no more-cue, a group header is orphaned, and 80x24 shows 2 models
- Only the slot title is clickable, not the card body
- 'Main answers you. Now:' ends with nothing
- Cryptic copy and four names for one concept (PINNED / slot / favourite / roster)

**Files:** `crates/sterna/src/workbench/models.rs`, `crates/sterna/src/workbench/sheets.rs`, `crates/sterna/src/workbench/input.rs`, `crates/sterna/src/workbench/view.rs`, `crates/sterna/src/session/controls.rs`, `crates/sterna/src/session/controls/subagents.rs`, `crates/sterna/src/session.rs`, `crates/sterna/src/session/startup.rs`, `crates/sterna/src/config/agents.rs`, `crates/sterna/src/settings/registry.rs`, `crates/inference-gateway/src/main.rs`

### P10 · The composer editor is a single-line editor with a multi-line draft

**Severity:** medium-high · Amber (Up discards a draft for good; characters are hidden at each wrap; an LF burst silently changes the text sent)

**Root cause.** The Editor in session/ui.rs (~524-690) treats the draft as one line. Up and Down always call recall(); Ctrl-A/E and Ctrl-K/U act on the whole draft; there is no word motion and no kill buffer or undo; Ctrl-J (the LF byte) matches no arm. The slash popup's Enter replaces even an exact match with `slash_matches()[selected]`, and slash_matches puts BUILT_INS first (/model before /mode, /cells before /cell). The popup has no dismissed state, so Esc cannot close it. In view.rs the wrap width (textwidth-4) and the drawn width (textwidth-5 when boxed) disagree, so one character per wrap is not drawn, and the click mapping re-wraps at the drawn width. The composer is capped at 5 lines, with skip recomputed from the cursor on every frame. '@' completion is advertised in four places and does not exist. Shift-Enter relies on a keyboard protocol pane never requests.

**Fix.** Split the Editor out of session/ui.rs into its own module (the composer owns it), as a multi-line buffer.
1. **Line motion.** ↑/↓ move by line and recall history only from the first or last line. Before recall, the current draft is pushed to history, so nothing is lost. Ctrl-A/E and Home/End are per line. Ctrl-K/U kill to line end and line start into a kill ring, and Ctrl-Y yanks. Ctrl-W and Alt-Backspace delete a word; Alt-B/F and Ctrl-←/→ move by word. Ctrl-Z/Shift-Ctrl-Z give an undo and redo of edit snapshots, including chip inserts and Ctrl-C clears. Ctrl-J inserts a newline, and the typing_waiting rewrite also covers Char('j') with CONTROL.
2. **Keyboard protocol.** Push the kitty keyboard enhancement flags (DISAMBIGUATE_ESCAPE_CODES) on entry and pop them on exit, so Shift-Enter is distinct. The KEYS sheet lists Alt-Enter as the fallback.
3. **Slash popup.** Enter keeps an exact match (sort the exact match to index 0 in slash_matches). Esc sets `popup_dismissed` for the current text, and a second Esc clears it. Ctrl-P/N follow the popup like ↑↓. The popup gets a `›` cursor, a more-cue, wheel scrolling, and a click that runs the command (it is a Run item).
4. **One composer width.** Compute it once in layout() and use it for wrap_input, g.composer.width, the cursor line and the click mapping.
5. **Scroll offset.** A persistent composer scroll offset changes only when the caret leaves the window. Show '↑ N more' and '↓ N more'. The wheel over g.composer scrolls it.
6. **@ completion.** Add '@' path completion in the same popup, sourced from git ls-files and the project walk. If that is out of scope, remove the four advertisements in the same commit.
7. **Empty composer.** Home/End scroll the transcript to top and bottom.

**Smallest failing test.** A unit test in session/ui.rs's tests module, `up_in_a_multiline_draft_moves_a_line_not_into_history`: create Editor with history ["old"] and text "first\nsecond" with the cursor at the end; editor.key(key(KeyCode::Up)). Assert editor.text == "first\nsecond" and the cursor is on line 0. Today the text becomes "old". Companion in tests/workbench.rs: s.input = 75 × "%03d,"; draw at 140x42; the composer rows concatenated equal s.input.

**Findings it closes:**

- The composer hides one character at every wrap point (ASCII and CJK)
- Clicking in a wrapped draft puts the cursor 1 character too far left per extra line
- Up (or Ctrl-P) in a multi-line draft swaps it for an old message; Enter then sends that message and the draft is gone
- Ctrl-K deletes every following line, and nothing brings it back
- Editing keys disagree: Home/End work per line while Ctrl-A/E work on the whole draft; word keys do nothing
- A draft taller than five lines hides lines with no cue; clicking a line makes the draft jump; the wheel doesn't scroll it
- Multi-line text arriving with LF line ends and no bracketed paste is glued into one line; Ctrl-J is ignored
- Typing '/mode' and pressing Enter opens Models; typing '/cell' and Enter runs /cells
- Esc does not close the slash popup
- Slash popup: cursor shown by colour only, no wheel, a click inserts without running, no more-cue
- '@ for a file' is advertised in four places but typing @ does nothing
- Shift-Enter depends on a keyboard protocol pane never requests; Alt-Enter works but is not listed
- Ctrl-Z does not bring back a draft an Insert chip overwrote
- Home/End don't scroll the transcript with an empty composer

**Files:** `crates/sterna/src/session/ui.rs`, `crates/sterna/src/workbench/view.rs`, `crates/sterna/src/workbench/input.rs`, `crates/sterna/src/tui.rs`, `crates/sterna/src/workbench/voice.rs`, `crates/sterna/src/workbench/document.rs`

### P11 · Selection is screen cells, and Ctrl-C has four owners

**Severity:** medium · Amber

**Root cause.** Selection stores absolute screen cells ({anchor, head} from m.column/m.row). tui/selection.rs::columns() makes every middle row span the whole area, and selected_text re-reads the last drawn buffer, so a multi-line selection copies card borders and the sidebar. It survives a resize or streaming at stale coordinates, because Event::Resize only redraws. Ctrl-C is matched in four places, each with its own meaning:
- workbench input.rs:372 copies and 'never interrupts, and the selection stays', so every later press copies again.
- the ui.rs composer branch (~1725) silently clears a non-empty draft.
- the approval branch denies.
- session.rs watch() arms a 2 s double-press window without telling the screen.

**Fix.** 1. **Selection by content.** Anchor a selection to document rows (row key plus char offset), not screen cells. Clip it to the region the anchor falls in (the transcript, not the sidebar). Skip RowKind frame glyphs and gutters when extracting text. Clear it on Event::Resize.
2. **One Ctrl-C function.** `fn ctrl_c(state) -> CtrlC`, applied in order:
   - a selection: copy it, then clear the selection;
   - a pending approval: cancel (P1);
   - a busy turn: interrupt, with the note 'Stopping · Ctrl-C again within 2 s quits';
   - a non-empty draft: clear it into the undo stack, with the note 'Draft cleared · Ctrl-Z brings it back';
   - an empty composer: arm the quit window with the notice 'Ctrl-C again within 2 s to quit', which clears when the window lapses. session.rs watch() reports arm and lapse through an Update.
3. **Ctrl-D while busy** gives a note. /exit while busy is honoured (P3).

**Smallest failing test.** tests/workbench.rs `a_second_ctrl_c_after_a_copy_passes_through`: set s.selection over transcript text; key(Ctrl-C) returns Effect::Copy; key(Ctrl-C) again. Assert Effect::Pass and s.selection == None. Today it returns Copy again. Companion: a selection spanning three rows of an expanded card copies text with no '│'.

**Findings it closes:**

- Dragging across several lines copies card borders and sidebar text
- A drag selection survives a resize at stale coordinates; Ctrl-C then copies the composer frame
- While rows stream, the highlight slides onto other text
- Once text is selected, Ctrl-C only copies and can never interrupt
- Ctrl-C silently throws away the typed draft, with no notice and no undo
- The first Ctrl-C gives no warning that a second one ends the session
- /exit is refused while working, and the refusal says Ctrl-C only 'interrupts tools'
- Ctrl-D during a turn does nothing and says nothing

**Files:** `crates/sterna/src/tui/selection.rs`, `crates/sterna/src/workbench/input.rs`, `crates/sterna/src/workbench/view.rs`, `crates/sterna/src/session/ui.rs`, `crates/sterna/src/session.rs`

### P12 · Sign-in flows: device-flow parsing, cancel and SSH states

**Severity:** medium · Amber/Red (the broker child process lifecycle)

**Root cause.** In the gateway, LoginOutput::read (subscription_broker/login.rs ~186-222) knows only the browser flow and OpenAI's device-flow lines, so xAI's 'To authenticate, please visit:' and 'Then enter this code:' produce nothing. Connect declares the account (declare_default_subscription, main.rs ~486-519) before the sign-in and never rolls that back. On the pane side, stream_connect (controls.rs ~930-1005) is a blocking loop on the session thread that only Ctrl-C ends. SignIn::render has no cancelled state, always offers '/open-link' and preselects it even over SSH, and titles itself with the entitlement id. key() on /key failure shows a text panel instead of re-opening the form with_error. key_shape approves any non-space string, and form verdicts are only Ok or Err, so a warning renders with ✓.

**Fix.** 1. **Gateway parsing.** Add the xAI device-flow lines to LoginOutput::read, and correct the login_flag doc.
2. **Declare after success.** Declare the account only after a successful connect, or roll the declaration back on failure or cancel.
3. **Sign-in off the session thread.** Run stream_connect on a worker, with progress sent as Updates. The panel carries a Run item `⟨ cancel sign-in ⟩`. Esc goes Back, leaving a `⟨ signing in to Grok ▸ ⟩` chip in the dock that reopens the panel, and commands keep working.
4. **Panel states.** SignIn::render gains Cancelled and TimedOut (for example after 5 minutes) states that offer `⟨ start again ⟩`. Over SSH (the links::open predicate) the open row is dropped and 'copy the link' or 'copy the code' is focused. The title uses the subscription label.
5. **Links in notes** wrap rather than being cut (document.rs note rows use wrapped()) and are clickable (Action::Copy).
6. **/key.** Re-open the form with_error on a failed save. A bare /key shows key_panel. Providers are sorted by display name, searchable and cued (P5).
7. **Form verdicts.** Add a Warning verdict kind ('! http, not https…'). key_shape says 'unrecognised shape' instead of '✓ pasted'. Drop 'right-click' from the hint.
8. **Warning sheet.** The subscription warning is wrapped Info text, with focus starting on Back.
9. **Try again** stamps 'still not answering · HH:MM'. Backticks are rendered or removed.

**Smallest failing test.** A gateway unit test in login.rs, `xai_device_flow_yields_a_link_and_a_code`: feed LoginOutput the lines ["To authenticate, please visit:", "https://accounts.x.ai/oauth2/device?user_code=ABCD", "Then enter this code: ABCD"]. Assert that a Progress carrying the URL and code 'ABCD' is emitted. Today it emits nothing.

**Findings it closes:**

- Grok sign-in never shows a link, a code or a failure; it waits forever
- A cancelled sign-in leaves a declared account; the row becomes 'grok-subscription · unknown · sign in'
- Esc hides a sign-in that keeps running and silently blocks every command
- After Ctrl-C cancels a sign-in, the panel stays up with live rows
- Over SSH the 'open in browser' row is offered and preselected, and its advice points to a row that does not exist
- The sign-in link kept in the chat is cut off, cannot be clicked, and copying it gives a broken URL
- The panel title and notices use internal ids instead of the name the person picked
- A failed key save throws the form away and sends the person to a shell
- /key alone is a dead-end sentence instead of the provider list
- Key form claims contradict each other ('never shown' vs 'Ctrl-R shows it'); the check approves anything
- Subscription rows mix plan names with internal ids and hide the risk once connected
- The risk warning before 'Sign in anyway' is cut off after one line and drawn in the selection accent
- The 'http, not https' warning carries a green check mark
- The hint promises right-click paste, which does nothing
- 'Try again' gives no sign that it tried; backticks show literally

**Files:** `crates/inference-gateway/src/gateway/subscription_broker/login.rs`, `crates/inference-gateway/src/main.rs`, `crates/sterna/src/session/controls.rs`, `crates/sterna/src/session/ui/links.rs`, `crates/sterna/src/tui/form.rs`, `crates/sterna/src/workbench/sheets.rs`, `crates/sterna/src/gateway.rs`

### P13 · The transcript renderer bypasses markdown and draws helper lanes outside their card

**Severity:** medium · Amber

**Root cause.** workbench/document.rs `answer()` sends prose through `wrapped()`, which splits on '\n' and hard-wraps by character. It never calls tui/markdown.rs render() (grep: no 'markdown' in src/workbench), and wrap_lines re-wraps at a width that ignores the band pad and gutter. `helpers()` is called after the card's CardBottom row with the full width (~511), and the Helpers tab arm draws nothing. Card rows are span rows, and draw_row only runs paths() for spanless rows, so the diff header is never a link. tui/paths.rs caches exists() per string for the session, negative answers included. Card copy is assembled without guards: '{n} lines', empty 'Handles', the answer() head slice, and no rolled-back state.

**Fix.** 1. **Markdown.** Render answer prose through tui/markdown.rs render(), which moves into the owner that the workbench calls. Add `[text](url)` links, rendered as underlined link spans with Action::OpenLink. Pass flow() the real remaining width (answer indent + pad + gutter), so nothing re-wraps by character.
2. **Helper lane inside the card.** Call helpers() inside the card body with `inner`, before CardBottom. The Helpers tab arm renders that lane, and detail lines use wrapped(…, indent).
3. **Paths.** draw_row runs paths() over span text at each span's x offset, and the diff '+++' header gets Action::Path. Invalidate the paths.rs cache when a turn ends or a change is captured, and never cache a negative answer. links::show returns an enum (Ssh, Missing, NoOpener). Over SSH it copies the path through OSC 52 ('Can't open files over SSH · path copied'); for a missing file it says 'src/main.py no longer exists'. Failures get a Warning tone.
4. **Card copy.** Pluralise lines. Skip empty sections. Include the answer in Full output. Cut answer() at the closing quote, adding '…' only when shortened, and at a word boundary. The '⊘ bash' call row shows the command and 'denied'.
5. **Rollback.** Record the rollback on the cell ('↶ rolled back') and send a one-line rollback notice.

**Smallest failing test.** tests/workbench.rs `answer_markdown_is_rendered`: an assistant message "Wrote **a.txt**, see `x` and [docs](https://example.com)". Assert words(doc) contains "Wrote a.txt, see x and docs" and contains no "**", backticks or "](". Fails today.

**Findings it closes:**

- Markdown in answers is not rendered (**, links, fences, backticks); inline code splits at wraps
- Answer prose runs flush into the sidebar border, and wrapped lines start one column further in
- Helper rows and the Helpers tab's content are drawn outside the cell card
- Path underlines are cached for the session: a new file never becomes clickable, and a deleted one stays underlined
- A path click that cannot open says 'No application could open this file.' whatever the reason
- The file name in the Changes tab is not clickable
- Full output shows an empty 'Handles' heading and leaves out the answer
- The card title says '1 lines'
- The card garbles an answer() line: `answer("…");…")`
- After a rollback the cell still claims its change; the status line is cut at the colon
- Loose ends in the cell card: '⊘ bash' with no reason; model-facing ask text shown to the person

**Files:** `crates/sterna/src/workbench/document.rs`, `crates/sterna/src/workbench/view.rs`, `crates/sterna/src/tui/markdown.rs`, `crates/sterna/src/tui/paths.rs`, `crates/sterna/src/session/ui/links.rs`, `crates/sterna/src/session/ui.rs`, `crates/sterna/src/session/controls.rs`, `crates/sterna/src/session.rs`

### P14 · Person-facing text is the model-facing or diagnostic string

**Severity:** medium · Green/Amber (copy)

**Root cause.** Strings meant for the model or for diagnostics reach the screen unchanged:
- The WireError Display is prefixed a second time: context.rs:244 and helpers.rs:949 prepend 'request failed: ' to a WireError whose Display already writes it.
- Decision-model output goes out through session_println (system.rs:488-505, returned.rs, decide.rs).
- The helper expansion prints h.looked prepare steps and the excerpts block written for the model (excerpts.rs HEADING).
- ask::DISABLED is shown to the person.
- The context meter prints Counted::as_str ('reported', 'estimated').
- /tool prints the sandbox backend ('seatbelt'), and /status formats a bool.
- voice::hint rotates a fixed array whatever the state.
- Voice::Failed always says 'open the cell'.

**Fix.** 1. **Errors.** Drop the extra 'request failed:' at both sites. A request error gets its own status: 'couldn't reach <endpoint> · check /login', with no cell reference.
2. **Decision lines** go to the telemetry and instruments only, never to session_println.
3. **Helpers.** asked_summary uses bounded_ask's 60-character first line. The captured model falls back to the configured helper model. The expansion shows Asked, Answer (or 'Failed: <plain sentence>') and the cited file:line list. Prepare steps and the excerpt block move behind `⟨ raw ⟩`.
4. **Ask.** The person sees 'asking is off (Settings › Advanced)'; the model keeps ask::DISABLED.
5. **Meters.** 'context 12 tokens · provider count' or '≈7.9k tokens (estimate)'. /tool says 'ran in the sandbox'. /status uses on/off words.
6. **One name** for telemetry ('Instruments' everywhere or 'Telemetry' everywhere, per open question 11) and for the sidebar ('sidebar').
7. **Hints.** voice::hint takes state (busy, has_changes, where the effort chip is) and filters the list. A hint that names a key is either clickable or clearly plain text.
8. **Sidebar.** Skip 'THIS SESSION' when it is empty. The tally row reads '✓ 0 ran · ● 0 running · ✕ 1 failed'.

**Smallest failing test.** A unit test in helpers.rs or session/context.rs: a WireError::Http(io refused) passed through the request path yields a message starting with "request failed: io:" that contains "request failed" exactly once. Today it appears twice.

**Findings it closes:**

- A failure repeats itself ('request failed: request failed: io: …') and points to a cell that does not exist
- A failed helper reads 'request failed: request failed…' under the label 'Returned:'
- Helper details dump internals and model-facing markdown, which breaks the layout
- Helper rows never say what was asked ('Asked: 1 lines'; 'captured model unknown')
- With Jev decisions on, raw classifier output is printed in the dock edge and the transcript
- Cryptic copy: 'ctx 12 · reported', '/tool read: exit 0 under seatbelt', 'network false', 'Cell limit: none (a task ends on evidence…)'
- The context meter and instruments text say 'unreported' / 'window unknown'
- Telemetry has three names (telemetry / instruments / LIVE INSTRUMENTS); Ctrl-B is 'session card' and /sidebar
- Rotating footer hints name actions that don't apply (Esc stops while idle; /diff with no change)
- The '✓ 0 ● 0 ✕ 1' row has no legend; 'THIS SESSION' is an empty heading

**Files:** `crates/sterna/src/session/context.rs`, `crates/sterna/src/helpers.rs`, `crates/sterna/src/wire.rs`, `crates/sterna/src/session/system.rs`, `crates/sterna/src/session/returned.rs`, `crates/sterna/src/decide.rs`, `crates/sterna/src/runtime/bindings/helper.rs`, `crates/sterna/src/workbench/document.rs`, `crates/sterna/src/workbench/voice.rs`, `crates/sterna/src/workbench/view.rs`, `crates/sterna/src/tui/status.rs`, `crates/sterna/src/tui/telemetry.rs`, `crates/sterna/src/session/controls.rs`, `crates/sterna/src/session.rs`, `crates/sterna/src/ask.rs`

### P15 · There is no overflow policy for chrome: things are clipped, dropped or overprinted without a word

**Severity:** medium · Amber

**Root cause.** Every renderer handles 'doesn't fit' on its own, and mostly silently:
- chrome::chips and the settings row loop `break` when a chip doesn't fit, which drops the current value.
- controls() drops top-bar chips by rank, with no overflow chip.
- Sheet feet draw the notice at full width and then the hint over it.
- draw_settings uses fixed offsets (the foot at bottom-7, rows at y+5), so the parts overlap at small heights.
- The '↓ latest' chip is placed on the transcript's right border.
- The legacy modals Clear only their own rect.
- dock_bottom clears one cell even with no chips.
- The theme sheet's list height and heading width are hard-coded.

**Fix.** 1. **Chip rows.** chrome::chips takes an overflow policy. When chips don't fit, keep the current or active chip and collapse the rest into `⟨ +N ▾ ⟩` (an Open item with the full list). Never drop the active value.
2. **Top bar.** The top bar gets the same `⟨ ⋯ ⟩` overflow chip. Warning-toned and mode-in-Plan chips are never dropped.
3. **Sheet foot.** The notice width is width − hint − 2, ellipsised (this belongs to the P5 component, so do it there).
4. **draw_settings** computes its regions from the top down: header, sections, list with more-cues, then the detail lines only if at least 3 rows remain. It never paints the foot above the rows.
5. **'↓ latest'** sits inside the transcript, left of the scrollbar column.
6. **Modal backdrop.** Every modal renders a full-frame backdrop (dimmed or Clear) before its box; this comes free once P1 and P5 move them into surface().
7. **Theme sheet.** Heading width is the full list width with wrapping. The preview starts below the list's first heading or to its right, with a gap. Rows = inner.height − 2, with more-cues.
8. **dock_bottom** clears nothing when span == 0.
9. Use CATEGORIES.len() instead of 5 and 6.

**Smallest failing test.** tests/workbench.rs `the_active_theme_chip_is_always_drawn`: open Settings on Display with ui.theme = "cockatoo" (the last theme); draw at 120x40. Assert the buffer row for Theme contains 'cockatoo' drawn with the chip_on style, or a '▾' overflow chip that holds it. Fails today.

**Findings it closes:**

- The Theme row shows 10 of 16 themes and the chosen one lights no chip; the effort row drops xhigh and max at 80 columns
- At 80 columns the top bar drops Build, This project and ? with no fallback; at 60 the model is dropped
- Settings clips silently at 80x24 and overlaps itself at 50x15
- A long notice overprints the sheet footer hint (Settings, Models, ASK)
- '⟨ ↓ latest ⟩' overwrites the cell card's right border
- Modals let sidebar text bleed past their frame
- The PARROTS header is cut mid-sentence and the preview sprite runs into it; 80x24 hides three parrots, and two rows stay blank
- The composer bottom border has a one-column gap
- The Models list is cut off with no cue and a header is orphaned (the component part is in P9)
- The Settings category is hard-coded as `min(5)`

**Files:** `crates/sterna/src/workbench/chrome.rs`, `crates/sterna/src/workbench/view.rs`, `crates/sterna/src/workbench/sheets.rs`, `crates/sterna/src/tui.rs`

### P16 · Palette and sprites assume a dark true-colour terminal

**Severity:** medium · Amber (the ask legend is invisible and light terminals are unreadable; the rest is polish)

**Root cause.** workbench/theme.rs and tui/theme.rs hold fixed Color::Rgb role constants (MUTED, HELPER, YOU, WARN, RED, GREEN), with no Indexed or 256-colour fallback when plumage::truecolor() is false and no background detection (no OSC 11 or COLORFGBG). Theme::hush() is a background tone ('the answer's own ground') but ask.rs:64/105/138 uses it as a foreground. Bird accents come straight from the plumage table with no hue-distance check against RED. Tone::Helper and Tone::You ignore the theme, so mono is not monochrome. The sprite data has no outline region. The no-truecolor path prints a sentence about a braille 'outline bird' that commit 8bbd7fe3 removed.

**Fix.** 1. **Colour depth.** Add a `ColorDepth` (True, 256, 16) chosen at start from COLORTERM or TERM. A single `paint(Color)` quantises every Rgb to Indexed when needed; apply it in theme::style and the pixel path.
2. **Light backgrounds.** Detect them with an OSC 11 query on start (short timeout) and COLORFGBG, with a `ui.background = auto|dark|light` setting (open question 10). Each role colour gets a light variant that keeps at least 4.5:1 for text and 3:1 for chips.
3. **Muted foreground.** Replace hush-as-foreground with a new `Theme::muted_fg()`.
4. **Accent checks.** A hue-distance guard against RED swaps the accent to the bird's `second` or `highlight` colour. Give Green-winged a split green wing region and a green swatch.
5. **Theme-following roles.** Helper and You follow the theme; in mono they map to Reset or bold.
6. **Sprites.** Add a 1-pixel outline region to the sprites: a light edge on dark backgrounds, a dark edge on light ones. This covers the beak and the cockatoo. The done tick becomes a clean 3-pixel check.
7. **No true colour.** Drop the 'outline bird' sentence, or draw the head in 256 colours.
8. **Classic preview.** The theme sheet previews a classic palette with swatches and a sample heading and chip.
9. **Tern tail.** Redraw it with a shallower angle and at least two pixels thick, using the user's reference.

**Smallest failing test.** tests/tui_look.rs `without_true_colour_no_rgb_is_emitted`: set ScreenState with truecolor off (ColorDepth::Ansi256); draw the workbench at 140x42 with a neon theme. Assert no buffer cell has fg or bg Color::Rgb(..). Companion: for every Theme, the ask legend fg against the default dark bg has contrast ≥ 3:1.

**Findings it closes:**

- The ask panel's key legend is black on black in every theme
- Sidebar and chip colours have low contrast on a light terminal (1.3:1 to 2.4:1); cells unreadable
- 24-bit colour escapes are still sent when true colour is off (65 × 38;2, 0 × 38;5)
- It promises an 'outline bird' that is never drawn
- The Scarlet and Green-winged Macaw accent is the error red, so all chrome reads as failure
- Scarlet and Green-winged are near-duplicates; the 'Green-winged' swatch is red
- Hard-coded colours ignore the theme: HELPERS is cyan, 'you' is purple, and mono is not mono
- The parrot beak is almost invisible on a dark background; the 'done' tick is loose pixels
- The Sulphur-crested Cockatoo turns invisible on a light background
- Classic themes have no preview
- User note (2026-09-27): the tern's tail angle is too steep and the tail looks very thin
- The braille ⠿ still marks Sterna in the header and reply labels (hand to the Sterna rename package)

**Files:** `crates/sterna/src/workbench/theme.rs`, `crates/sterna/src/tui/theme.rs`, `crates/sterna/src/tui/ask.rs`, `crates/sterna/src/workbench/plumage.rs`, `crates/sterna/src/workbench/sheets.rs`, `crates/sterna/src/workbench/document.rs`, `crates/sterna/src/tui/poster.rs`, `crates/sterna/src/session/ui/terminal_input.rs`

### P17 · Session edges: resume, empty sessions, fullscreen

**Severity:** low-medium · Green/Amber

**Root cause.** session/resume.rs pick() is a separate crossterm loop: no mouse mode, square frame, and Esc and Enter-on-empty both `break None`, which choose() turns into a silent exit 0. resumable() lists every jsonl, including sessions with no prompt, and resume_hint prints unconditionally. Ctrl-F toggles fullscreen with no note, and Esc never leaves fullscreen.

**Fix.** 1. **Resume picker.** Port it onto the P5 Sheet, either drawn inside the TUI or as a pre-session Sheet with mouse capture (open question 12). The first Esc clears the search. Enter with no match says 'nothing matches'.
2. **Empty sessions.** Delete or skip session files with zero user turns, both in resumable/pickable and at exit. Print the resume hint only when prompts > 0.
3. **Fullscreen.** Ctrl-F posts the same note /fullscreen does and keeps a one-line 'Ctrl-F restores' hint. Esc on an idle, empty composer leaves fullscreen.

**Smallest failing test.** A unit test in session/resume.rs, `pickable_skips_a_session_with_no_prompt`: in a temp dir, write one jsonl with a user turn and one with only a header. Assert pickable() returns one entry, and that it is the one with the prompt. Today the empty one comes first.

**Findings it closes:**

- Enter on 'nothing matches' quits pane silently; Esc with search text also quits instead of clearing
- The resume picker ignores the mouse entirely and looks like no other sheet
- Sessions where nothing was asked are saved and preselected at the top of the resume list; Ctrl-D still prints 'resume it with'
- Fullscreen entered with Ctrl-F shows no way back, and Esc does not restore

**Files:** `crates/sterna/src/session/resume.rs`, `crates/sterna/src/session.rs`, `crates/sterna/src/session/ui.rs`, `crates/sterna/src/workbench/input.rs`, `crates/sterna/src/workbench/view.rs`

## Keyboard and mouse as found (pre.15)

Legend: ✗ = departs from the model above · ✓ = matches it · – = not applicable or not observed

| Surface | Move | Choose | Space | Tab | Esc | Click | Wheel | Other deviations |
|---|---|---|---|---|---|---|---|---|
| Opening chips | ✗ no keyboard route | ✗ a click only inserts text; 'finish setup' types `/wizard` | – | ✗ dead | – | ✗ overwrites the draft with no undo; card rows give a quip, and the permissions row does not open ASK | scrolls the transcript | ✗ chip missing after the auto-wizard; any note wipes the chips |
| Composer | ←→; Home/End per line; ✗ Ctrl-A/E cover the whole draft; ✗ ↑↓ recall history even inside a multi-line draft (the draft is lost) | Enter sends; Alt-Enter new line; ✗ Shift-Enter only with CSI-u; ✗ Ctrl-J / LF dropped | types a space | Shift-Tab steps the rung | idle: ✗ does nothing (popup stays); busy: stop, then cancel | puts the cursor, ✗ 1 column early per wrapped line | ✗ scrolls the transcript, not the draft | ✗ Ctrl-K kills to the end of the draft; ✗ no Ctrl-W / Alt-B / undo; ✗ one character hidden at each wrap; ✗ `@` advertised but absent |
| Slash popup | ↑↓; ✗ Ctrl-P/N ignore the popup; ✗ cursor shown by colour only | ✗ Enter runs the highlighted match, not the exact text (`/mode`→`/model`, `/cell`→`/cells`) | – | completes | ✗ does nothing | ✗ inserts without running | ✗ nothing | ✗ 7 rows with no more-cue |
| Transcript & cell cards | PgUp/PgDn; Ctrl-Home/End; ✗ Home/End do nothing | click on a header expands; tab chips work; ✗ F4/F5/Ctrl-O act on a phantom cell | – | – | nothing | ✗ a drag copies screen cells, including borders and sidebar; path click opens the file | scrolls | ✗ 'open diff ↗' opens the latest cell and is refused mid-turn; ✗ no keyboard cell selection |
| Top bar / sidebar / strip chips | ✗ no focus (F2, F3, Shift-Tab as twins) | click opens a sheet or steps a value | – | – | – | ✗ the effort chip vanishes at default; ✗ 'helpers off' steps effort; ✗ the subagents chip opens Settings, not the picker; ✗ the telemetry chip is refused mid-turn | – | ✗ no hover anywhere; ✗ sidebar helper lines are inert |
| Wizard (Setup panel) | ↑↓; ✗ Home/End/PgUp dead | Enter or click on the title line; ✗ the detail line is dead | ✗ dead | ✗ dead | ✗ closes the whole wizard although labelled Back | ✗ '⟨ Esc · Back ⟩' closes everything | ✗ 3 rows per notch skips steps | ✗ opens with the cursor on a description row; ✗ Back loses focus; ✗ swallows typed characters at first start |
| Panel lists (Sign in, subscription, API keys, /help, /status, /context, /handlers, /key, gateway error, /permissions result, sign-in progress) | ↑↓, PgUp/PgDn 10; ✗ Home/End dead | Enter or click runs the row; ✗ inert rows get a `›` cursor (/help, /status never act) | ✗ dead | ✗ dead | ✗ closes to chat | selects and runs | ✗ 3 rows, lands on info rows | ✗ no filter over 37 providers; ✗ no more-cue; ✗ typing swallowed; ✗ handler row stays 'active' and a second press duplicates |
| Theme picker | ↑↓ (wraps); PgUp to top; ✗ Home/End dead | Enter applies, saves and closes | ✗ dead | ✗ dead | closes and keeps the old theme | ✗ applies and closes, with no mouse preview; ✗ the swatch is dead | ✗ 3 rows | ✗ no key legend; ✗ 80×24 hides 3 parrots; ✗ heading cut off; ✗ classic themes have no preview |
| Settings | ↑↓, PgUp/PgDn 8 | ←→ cycles and saves (✓); Enter cycles choice rows (✗ the legend says 'Edit'); ✗ permissions.* / agents.mode stage a raw `key = value` edit | ✗ starts an invisible search | Tab/Shift-Tab category ✓; F6 scope | layered: edit → search → close ✓ | chip click applies and the sheet stays open ✓; ✗ `‹` goes forward | ✗ 3 rows | ✗ Backspace on an empty search resets the row; ✗ undo is one level; ✗ values are stale after chip changes |
| Settings field editor | none | Enter saves | – | – | cancels the field ✓ | ✗ none | – | ✗ no cursor or range; ✗ a paste appends to the prefilled value; ✗ list newlines dropped |
| Models (Main / Helper) | ↑↓, PgUp/PgDn 10; ✗ Home/End dead | ✗ Enter commits **and closes** | types into the search | Tab tier (✗ resets to row 0, not the current model); ✗ Shift-Tab dead | ✗ closes even with a query typed | ✗ a row click only highlights and a second click does nothing; ✗ count and hints not clickable | ✗ 3 rows | ✗ search unranked; ✗ locked rows look live; ✗ image and speech models offered; ✗ 'Now:' blank |
| Models (Subagents) | ↑↓ model, ←→ slot | ✗ Enter fills the slot and closes the whole sheet | types | Tab | closes | ✗ only the slot title is clickable; ✗ 'turn them on' closes with a jargon error | ✗ moves the list from over the cards | ✗ no slot-effort control; ✗ PINNED card reads 'favorite roster' |
| Model picker opened from a Settings row | ↑↓ | Enter saves and returns to Settings ✓ | types | ignored | back to Settings ✓ (the only nested Back that works) | selects | ✗ 3 rows | ✗ 'Use the inherited value' contradicts the row's text |
| ASK (rung) | ↑↓; ✗ starts on row 1, not the current rung; ✗ cursor shown by colour only | Enter or click applies and closes; Full goes to CONFIRM | ✗ dead | ✗ dead | closes | applies at once | ✗ unclamped, so ↑ looks dead | ✗ not saved (Settings saves); ✗ three different confirm flows for Full |
| WORK (mode) | ↑↓; ✗ starts on row 1 | Enter or click applies; ✗ refused mid-turn while ASK applies | ✗ dead | ✗ dead | closes | applies | – | ✗ no Auto row; bare `/mode` can only be reached with a trailing space |
| Effort sheet | ↑↓; ✗ starts on 'default', not the current value | Enter or click | ✗ dead | ✗ dead | closes | applies | ✗ 3 rows | – |
| ACCESS | ↑↓ scroll 3 lines | ✗ Enter and Space dead, so buttons are mouse-only | ✗ dead | ✗ dead | closes | chips work | – | – |
| CONFIRM (Never asks) | none | Enter (✗ Confirm is pre-focused) | – | – | ✗ closes everything, not back to ASK | chip | – | ✗ heading says 'not undone by Esc', yet Esc cancels |
| KEYS (?) | none | none | – | – | closes | ✗ rows not clickable | ✗ nothing | ✗ leaves out scrolling, Ctrl-C and editing keys; ✗ footer 'saved choices stay' |
| ACTIVITY | ↑↓ scroll 3 | – | – | – | closes | – | ✗ nothing | – |
| Telemetry | ↑↓ requests (✗ Down is dead at the start) | – | ✗ leaks into the hidden composer | – | Esc / Ctrl-T | ✗ no Back chip; ✗ all hits cleared; ✗ the telemetry chip is gone | ✗ nothing | ✗ stray ┬ ┴ joints |
| Forms (API key, custom endpoint, finish signing in) | Tab/↓ next, Shift-Tab/↑ previous ✓ | ←→ choice; Enter submits the whole form | ✗ dead on a choice | next field ✓ | ✗ closes to chat, not the list | ✗ no mouse at all | ✗ falls through to the scrollback | ✗ 'right-click' paste promised; ✗ a failed save throws the form away |
| Approval modal | ↑↓/PgUp/PgDn scroll (✗ unclamped); ✗ End dead | ✗ letters o / s / a only; ✗ Enter dead | ✗ dead | – | ✗ **Deny, remembered for the session** | ✗ clicks ignored | 3 lines | ✗ typeahead answers it (an 's' in a draft allows for the session); ✗ Ctrl-C = remembered Deny, even over a selection; ✗ no counter across queued approvals; ✗ >16 KiB can never be approved |
| Redirect prompt ([a]) | ✗ no cursor movement | Enter sends | types | – | back to the approval ✓, ✗ but the typed words are dropped | ✗ none | – | ✗ a paste lands in the hidden composer |
| Ask panel | ↑↓, j/k (✗ the only surface using them), 1–9 | Enter | ✗ dead | – | ✗ 'decide yourself', which means let the model decide | ✗ ignored | ✗ ignored | ✗ legend drawn black on black; ✗ the answered cell shows FAILED |
| Rollback preview | ↑↓; ✗ **starts on Confirm** | Enter or click (Confirm deletes files) | – | – | ✗ closes with no cancel notice and leaves the pending state set | acts | – | ✗ the cell still claims its change after rollback |
| Resume picker (--resume) | ↑↓, PgDn/End; ✗ Home dead | Enter; ✗ on 'nothing matches' it quits pane | types | ✗ dead | ✗ quits even with a search typed | ✗ no mouse mode | ✗ none | ✗ empty sessions listed and preselected; ✗ square frame, reversed bar |
| Bare `/cell` (inspection) | ✗ swallows ↑↓, PgUp, Home/End and ←→ for an overlay that is never drawn | – | – | – | ✗ closes the invisible view | – | scrolls the transcript | ✗ dead legacy path |
| Settings / Models search (paste) | – | – | – | – | – | – | – | ✗ a pasted newline stays hidden in the query, and Settings then shows an empty result with no message |

## All confirmed findings, by surface

### composer (5)

- **The composer hides one character at every wrap point (ASCII and CJK), while a blank column is left at the right** — defect, visual
  - expected: Every typed character is visible and wrapped lines join seamlessly.
  - actual: At 140 cols line 1 ends '…031,032,0' and line 2 starts '3,034,…': one '3' of 033 is missing, and line 2 ends '065,06' with the comma hidden. At 80 cols: '…017,0' / '8,019'. CJK: 150 typed, 148 shown (indices 66 and 133 missing). Two blank columns remain before the right border on each wrapped line.
  - repro: `Tui(cols=140): t.paste(''.join('%03d,' % i for i in range(75))). Also Tui(cols=80,rows=24) with range(45), and 150 CJK characters chr(0x4E00+i). (gaps-2/scripts/s10_wrap.py, s32_wrap80.py, s23_cjk.py)`
  - code: crates/sterna/src/workbench/view.rs, two lines disagree on the composer's width.
- **Wrap width** is `textwidth - 4`. layout() (around line 289) wraps with `wrap_input(&s.input, textwidth.saturating_sub(4))`, and the cursor code at line 505 uses the same width.
- **Draw width** is `textwidth - 5` when boxed. The drawing code (around line 493) sets `g.composer.width = textwidth.saturating_sub(promp
- **Up (or Ctrl-P) in a multi-line draft swaps it for an old message; Enter then sends that and the draft is gone for good** — defect, keyboard
  - expected: In a multi-line draft Up moves to the line above, and history is reached only from the first line, or at least the draft is not discarded.
  - actual: Up replaced the two-line draft with 'old message'. Enter sent 'old message'. The draft was not kept anywhere: Up x1, x2 and x3 all show only 'old message'. No key moves the cursor up or down a line.
  - repro: `Send 'old message'. Type 'first line of a long draft', Alt-Enter, 'second line'; press Up (meaning 'go to line 1'); press Enter. Then press Up three times. (gaps-2/scripts/s26_uploss.py, s12_keys.py)`
  - code: crates/sterna/src/session/ui.rs. Lines 647-648 map `KeyCode::Up => self.recall(true)` and `KeyCode::Down => self.recall(false)` unconditionally, without checking whether the cursor is on the first or last line of a multi-line text. There is no up/down line-motion branch. recall() (line 570) stashes the draft in self.draft only while browsing. take() (line 590) runs on Enter, clears self.draft (lin
- **Ctrl-K deletes every following line, not the rest of the current line, and nothing brings it back** — defect, keyboard
  - expected: Ctrl-K kills to the end of the current line, as in readline and every editor, and Ctrl-Y (or Ctrl-Z) restores the killed text.
  - actual: Lines 2 and 3 both vanish and the message sent is '[A]line one\n'. Ctrl-Y and Ctrl-Z do nothing ('❯ keep' stays 'keep').
  - repro: `Type 'line one' Alt-Enter 'line two' Alt-Enter 'line three'; put the cursor at the start of line 2 (Ctrl-E, Home, Left, Home); press Ctrl-K; then try Ctrl-Y and Ctrl-Z. (gaps-2/scripts/s12_keys.py, s26_uploss.py)`
  - code: crates/sterna/src/session/ui.rs, around line 612, in the composer's fn key: `KeyCode::Char('k') if control => { self.text.truncate(self.cursor); }` cuts everything after the cursor, not up to the next '\n'. The End handler just below already finds the line end: `self.text[self.cursor..].find('\n')`. Ctrl-U at about line 608 (`self.text.drain(..self.cursor)`) has the mirror bug. This key handler ha
- **Editing keys disagree with each other and with the keys sheet: Home/End are per line but Ctrl-A/Ctrl-E are per draft; word keys do nothing** — defect, inconsistency
  - expected: Ctrl-A equals Home and Ctrl-E equals End, as in readline. Ctrl-W deletes a word and Alt-B / Ctrl-Left move by a word. Ctrl-P behaves like Up with the slash popup open. The '?' sheet ('every key, and what it does') lists the editing keys.
  - actual: Home/End stay on line 3 ('[H]line three[E]'), while Ctrl-A goes to the start of line 1 ('[A]line one') and Ctrl-E to the end of the draft. Ctrl-W and Alt-B do nothing, and Ctrl-Left moves one character ('keep~^'). With the popup open, Up moves the popup but Ctrl-P replaces '/th' with a history entry. The '?' sheet lists none of Ctrl-A/E/K/U/P/N, Home/End, Up/Down history or Alt-Enter.
  - repro: `Three-line draft: Home, type [H]; End, type [E]; Ctrl-A, type [A]; Ctrl-E, type [Z]. Then Ctrl-W, Alt-B, Ctrl-Left. With '/th' typed, Ctrl-P vs Up. Open '?'. (gaps-2/scripts/s12_keys.py, s26_uploss.py, s34_keys_sheet.py)`
  - code: crates/sterna/src/session/ui.rs, composer fn key() (~line 601).
- Lines 604–605: Ctrl-P and Ctrl-N always call recall() and ignore the slash popup. Up/Down get a popup guard at ~line 639; Ctrl-P/N do not.
- Lines 606–607: Ctrl-A sets cursor=0 and Ctrl-E sets cursor=text.len() (the whole draft). Home/End at lines 617–628 use rfind/find('\n') and stay on the current line.
- There is no word-motion o
- **A draft taller than five lines hides lines with no cue, and clicking a line makes the whole draft jump** — defect, mouse
  - expected: A cue such as '↑ 3 more' shows that lines are hidden above. The clicked line stays under the pointer. The wheel scrolls the draft.
  - actual: The composer shows L4–L8 with '❯' beside L4, as if it were the first line, and nothing says L1–L3 exist. Clicking L5 on row 38 re-scrolls the draft to L1–L5, so L5 jumps to row 41 under the pointer and L6–L8 disappear, again with no cue. The wheel over the composer does not scroll it.
  - repro: `Type L1-text … L8-text separated by Alt-Enter; click inside 'L5-text'; then click 'L1-text'; then wheel down over the composer. (gaps-2/scripts/s13_hidden.py)`
  - code: The draft has no scroll offset of its own. crates/sterna/src/workbench/view.rs caps composer_height at 5 lines (around line 291, `.clamp(1, 5)`). It then recomputes `skip = cursor_row.saturating_sub(visible-1)` on every frame (around line 502), so the visible window always pins the caret's line to the bottom row. Nothing marks the lines skipped above or below; the '❯' mark is drawn on the first vi

### approval modal (4)

- **d, Esc and Ctrl-C deny the exact call for the rest of the session, silently; asking for it again fails with no prompt** — defect, bug
  - expected: The footer '[d/Esc] Deny' refuses this attempt only, or says it lasts for the session. A later identical call asks again, or at least tells the person it was refused because of their earlier answer.
  - actual: Turn 2 shows no approval. The cell fails in about 15 ms with '✕ PermissionDenied: write("") / rule: the host call gate denied or cancelled this exact attempt', but the person was not asked this time. The same happens after Ctrl-C at an approval ('t2: REFUSED WITHOUT ASKING'). Nothing on screen says the refusal comes from an earlier answer, and nothing can lift it.
  - repro: `Every call rung. Turn 1: cell writes a.txt 'one' -> press d (or Esc, or Ctrl-C). Turn 2: t.type('ok now really write a.txt'); Enter; the model sends the identical cell. (gaps-2/scripts/s06_deny.py, s28_ctrlc_remembered.py)`
  - code: crates/sterna/src/approval.rs Gate::admit: Decision::Deny/Redirect -> self.judged.remember(action,false), and an early return from judged.answer() before anyone is asked. crates/sterna/src/session/ui.rs ~1354-1371: Esc and Ctrl-C map to Decision::Deny. crates/sterna/src/tools/invoke.rs:726: the same generic rule text is used for a remembered refusal, with path String::new().
- **Ctrl-C at an approval is recorded as a Deny: the model reads it as a refusal, unlike Ctrl-C during a running call** — defect, inconsistency
  - expected: Ctrl-C means stop, in the same way in both places: the call is reported as cancelled by the person and is not remembered as a refusal.
  - actual: At the approval the cell reads '✕ PermissionDenied ... denied or cancelled this exact attempt' and the model receives the same text as for d. The call is then banned for the session (see the denial finding). Mid-command, Ctrl-C reads '✕ Cancelled: Cancelled: bash() was cancelled before it completed'. Both still send one more model request.
  - repro: `Every call rung; a cell writes c.txt; at the approval press Ctrl-C. Compare: allow Bash(sleep *), approve ʼsleep 6ʼ with o, then press Ctrl-C while it runs. (gaps-2/scripts/s06_deny.py turn 3, s27_ctrlc_midcell.py)`
  - code: crates/sterna/src/session/ui.rs:1367-1370 and 1327-1331 map Ctrl-C to approval::Decision::Deny. crates/sterna/src/approval.rs:643-646 remembers Deny as judged=false for the session, and admit() at approval.rs:499 returns a bool. So crates/sterna/src/tools/invoke.rs:713-731 reports a PermissionDenied ("denied or cancelled") instead of ToolError::Cancelled. Esc at ui.rs:1355 is also Deny. The double
- **Scrolling overshoots: past the end, Up and the wheel seem dead for many presses; End does nothing** — defect, keyboard
  - expected: The scroll stops at the last page, so the first Up moves the text. End jumps to the end, as Home jumps to the top.
  - actual: After Down x30 the view stops at 'line 223…', but the offset keeps counting. Up x1, Up x6 and a wheel-up notch leave the view unchanged, and End does nothing.
  - repro: `Every call rung; a cell writes 400 lines ('line 000'..); at the modal press PgDn, then Down x30, then Up, Up x5, wheel up once, End. (gaps-2/scripts/s35_scroll.py)`
  - code: crates/sterna/src/session/ui.rs:1372-1391: the approval key handler grows `approval_scroll` with saturating_add (Down +1, PgDn +10) and the wheel handler at ~1254-1256 adds 3 per notch, all with no upper bound. It has no KeyCode::End arm, so End reaches the `_ => None` arm. The clamp exists only at draw time: crates/sterna/src/tui.rs render_approval (~lines 106-122) does `.scroll((scroll.min(maxim
- **Two approvals from one cell swap in place with no counter; a quick double press leaks into the hidden draft** — defect, visual
  - expected: The modal says '1 of 2' / 'next: write b.txt', or visibly changes when a new call replaces the answered one. Keys are not lost into the composer.
  - actual: The second modal looks the same as the first. Only 'alpha' becomes 'beta' and a.txt becomes b.txt. The second 'o' arrived before the next request and was typed into the composer ('│ ❯ o'), while the b.txt approval stayed up.
  - repro: `Every call rung; cell ʼPromise.all([write a.txt 'alpha', write b.txt 'beta'])ʼ; at the first modal send 'oo' in one write. (gaps-2/scripts/s03_two.py, s29_double.py)`
  - code: Both causes are in crates/sterna/src/session/ui.rs.

- Key routing: the approval `VecDeque` is created around line 858. Keys go to it at about lines 1289 and 1346 (`if let Some(request) = approvals.front()`); 'o' calls `pop_front` and responds. The cell's writes pass through the gate one at a time, so the b.txt request is pushed (line 883) only after a.txt is answered. For that short gap `approval

### /models › Subagents tab (3)

- **Filling a subagent slot closes the whole picker and it reopens on Main; four slots take four full round trips** — defect, inconsistency
  - expected: Slots are filled in one sitting: Enter (or a click) puts the model in the slot, the sheet stays open on the Subagents tab and moves to the next slot, and the confirmation names the slot and model.
  - actual: Every Enter closes the sheet. The sheet reopens on 'Main answers you'. Per slot the user repeats /models, Tab, Tab, → ×n, search, Enter. The confirmation does not name the slot or model: '· Subagents: off · 1 configured favorites · next launch uses this assignment; in-flight jobs unchanged'.
  - repro: `real mode: t.type('/models'); t.key('enter'); t.key('tab'); t.key('tab'); t.key('right'); t.type('haiku-4-5'); t.key('enter') → back on the home screen. t.type('/models'); t.key('enter') → the Main tab is showing again.`
  - code: crates/sterna/src/workbench/input.rs Action::ChooseModel (~l.934-958): it always calls self.close() before returning Effect::Command. crates/sterna/src/workbench/models.rs choose() (~l.146) turns a slot pick into '/subagents {slot} {model}', and models.rs:33 opens the picker on the tab in assignment.active. crates/sterna/src/session/controls/subagents.rs:13 builds the confirmation, which leaves ou
- **A filled slot leaves subagents OFF; the 'turn them on' line closes the sheet and, with empty slots, prints a jargon error** — defect, bug
  - expected: Putting a model in a favourite slot turns favourites on, or the sheet says plainly that it is still off. The on/off control is a real toggle that stays in the sheet, and it is disabled with a reason while every slot is empty.
  - actual: After filling QUICK the sidebar shows 'subagents off' and the transcript '· Subagents: off · 1 configured favorites'. Clicking 'Favourites are off · turn them on' closes the picker. With every slot empty it prints '✕ ERROR: settings: configure a favorite before enabling roster delegation' in the transcript, and the prompt header still reads '✓ complete'. The control is drawn as plain text with no chip frame.
  - repro: `real mode: /models, Tab, Tab, click the text 'Favourites are off · turn them on' with all slots empty. Separately: /subagents quick claude-haiku-4-5-20251001, then look at the sidebar and status strip.`
  - code: 1. crates/sterna/src/workbench/sheets.rs around lines 342-356 draws the toggle with `add(...)` and `Action::Command("/subagents on|off")`. It is always enabled, with no check for empty `m.assignment.slots`. `add` in workbench/view.rs:45 draws a plain `button` row, not a `chrome::chip`, which is why it has no frame.
2. crates/sterna/src/workbench/input.rs:974, `Action::Command(cmd) => { self.close(
- **No way to choose a slot's effort in the picker; slots silently get a default effort and the card truncates it away** — defect, loose-end
  - expected: The slot card shows model and effort, and an effort control (keys and click) sits next to the slot.
  - actual: Effort can only be set by typing `/subagents deep claude-opus-5 high`. A picker-filled slot gets an unannounced default ('low' for QUICK). A long model name truncates the card and hides the effort. A mouse user cannot set slot effort at all.
  - repro: `real mode: /subagents quick gpt-5.6-luna (no effort), then /models, Tab, Tab → the QUICK card reads 'gpt-5.6-luna · low'. Fill QUICK with claude-haiku-4-5-20251001 → the card reads 'claude-haiku-4-5-20251…'.`
  - code: crates/sterna/src/workbench/sheets.rs ~lines 300-340: the slot cards render `format!("{} · {}", held.model, held.effort.name())`, then `clip(&holds, width-3)`, so the effort is the part that gets cut. Only the title row is a button (Action::Slot); the value row cannot be clicked. The hint line (line ~531, "type to search · ↑↓ model · ←→ slot · Ctrl-A … · Ctrl-O …") offers no effort key, and the sh

### opening screen suggestion chips (2)

- **'review 1 uncommitted change' on a clean repo counts Sterna's own .pane/ directory** — defect, bug
  - expected: A freshly committed repository gets no 'uncommitted change' suggestion.
  - actual: The only untracked files are Sterna's session logs (.pane/sessions/*.jsonl, created at launch), yet the chip offers to 'review 1 uncommitted change'. After the wizard, .pane/config.toml adds to it too.
  - repro: `t=Tui(fake=True); t.wait('What next',15); print(subprocess.run(['git','status','--porcelain','--untracked-files=all'],cwd=t.cwd,capture_output=True,text=True).stdout)`
  - code: crates/sterna/src/workbench/voice.rs, project_suggestions() (around lines 364-391). It sets `dirty = git(&["status", "--porcelain"])` and counts every non-empty line, with no pathspec that excludes `.pane/`. Sterna also writes no .gitignore for .pane/. The count goes to suggestions() (voice.rs:290), which pushes 'review N uncommitted change(s)' whenever dirty_files > 0. It is called from session.r
- **Clicking a suggestion chip silently replaces what the person already typed** — defect, bug
  - expected: The chip text is appended, the person is asked first, or the draft can be undone.
  - actual: The draft is gone and the composer shows 'Pick up where the last commit left off ("init"). What is the next step?'. Ctrl-Z does not bring the draft back.
  - repro: `t=Tui(fake=True); t.wait('What next',15); t.type('my own draft message'); x,y=t.find('pick up: init'); t.click(x+3,y); t.key('ctrl-z')`
  - code: crates/sterna/src/session/ui.rs:1169-1174. The `Effect::Insert` handler sets `editor.text = command`, replacing any draft, and keeps no undo copy. It is reached from the chip's `Action::Insert` in workbench/document.rs:~1260 through workbench/input.rs:717.

### ask sheet (2)

- **The ask sheet ignores the mouse, Space does nothing, and the Esc label is confusing** — defect, mouse
  - expected: Clicking a choice selects and confirms it, as clicking a row does in /rollback or the ASK rung sheet. Space behaves as it does on the settings sheets.
  - actual: Hover and click leave the '▶' on 1. README.md and nothing is sent: the model's last message is still just 'change something'. Space does nothing. Only digits, arrows and Enter work. The footer reads 'Esc: decide yourself', which tells the person to decide when it means 'let the model decide'. The frame is square-cornered, titled '⠿ pane is asking', and overlaps the sidebar.
  - repro: `(ask enabled as above) t.type('change something'); t.key('enter'); t.settle(2.5); x,y=t.find('src/main.py',1); t.hover(x+1,y); t.click(x+1,y); t.settle(2.5); then separately t.key('space')`
  - code: crates/sterna/src/tui/ask.rs: key() handles only arrows, j/k, 1-9, Enter and Esc (no Space), line 146 has the footer "Esc: decide yourself", and render() centres the box over the full frame and styles the footer with theme.hush() (near-black in the default theme). crates/sterna/src/session/ui.rs: the Event::Mouse branch (~line 1249) never checks `asking`, so a click or hover on a choice does nothi
- **A question answered correctly is shown as a FAILED cell, and the answer never appears** — defect, bug
  - expected: The card shows the question and 'you chose: src/main.py' as a normal, non-failed outcome.
  - actual: The card turns to '✕ FAILED' with '✕ RuntimeTerminated: the isolate was terminated before the cell finished' and '✓ ask'. The chosen answer appears nowhere in the transcript. The model receives '[cell 1 threw in 0 ms] ... ## Answer src/main.py (you asked; the person chose)'. Esc ('decide yourself') gives the same FAILED card.
  - repro: `XDG config pane/config.toml '[ask]\nenabled = true' passed via Tui(env={'XDG_CONFIG_HOME': dir}); t.model.script=['ʼʼʼpane\nask("Which file should I change?", ["README.md", "src/main.py", "neither"]);\nʼʼʼ', 'Understood...']; t.type('change something'); t.key('enter'); t.settle(2.5); t.key('down'); t.key('enter')`
  - code: crates/sterna/src/runtime/bindings/ask.rs, ask_callback. It records the call and the question (trace.record_ask), then calls scope.terminate_execution() without trace(scope).request_yield(..) first. Both yield_now_callback and answer_callback in crates/sterna/src/runtime/bindings.rs call request_yield so the stop reads as deliberate. answer_callback's own comment says why: 'so the isolate answers 

### /login subscription list (2)

- **Subscription rows mix plan names with internal ids and hide the risk once connected** — defect, copy
  - expected: Every row reads the same way ('Claude · Pro, Max · connected · ⚠ read first'). The list has key hints and type-to-filter like the other lists.
  - actual: Rows read 'ChatGPT · chatgpt-subscription · account-declared · connected' and 'Claude · claude-max · account-declared · connected'. Claude loses its '⚠ read first' once connected. There is no hint footer (the wizard has one). Typed letters are ignored, and Home/End do nothing.
  - repro: `t=Tui(fake=False); t.wait('Setup ·',25); t.key('esc'); t.type('/login subscription'); t.key('enter'); t.settle(2); print(t.screen())`
  - code: crates/sterna/src/session/controls.rs, subscription_panel() (around lines 807-860). The `risk` suffix (" · ⚠ read first") and `subscription.plans` are used only in the `declared.is_empty()` branch. The declared-account branch formats `"{label} · {entry.account} · {entry.scope} · {state}"`, which prints the internal id and scope and drops the plan names and the risk marker. The panel is a generic `
- **A cancelled sign-in leaves a declared account behind; the Grok row turns into 'grok-subscription · unknown · sign in'** — defect, inconsistency
  - expected: A cancelled sign-in changes nothing; the row still reads 'Grok · SuperGrok, X Premium+'.
  - actual: The row now reads 'Grok · grok-subscription · unknown · sign in' (internal id plus 'unknown'). The gateway created and kept an empty broker entitlement directory for it.
  - repro: `Real mode, SSH_CONNECTION set: '/login subscription', Enter on 'Grok · SuperGrok, X Premium+', then Ctrl-C to cancel. Open '/login subscription' again.`
  - code: The account is written to the config before the sign-in starts, and nothing removes it on cancel.
- crates/inference-gateway/src/main.rs, the Command::Subscriptions::Connect arm (~line 338): when no entitlement is named it calls `declare_default_subscription(&cli, *provider)?` (lines 486-519). That calls `config::declare_table(... "accounts.grok-subscription" ...)` and persists the account to gate

### /models list (2)

- **Clicking a model only highlights it; a second click does nothing. In Settings a single click on a row activates it** — defect, mouse
  - expected: The same click grammar everywhere: a click (or double-click) on a model row chooses it, as a click on a Settings row acts.
  - actual: The row gets '›'. Clicking again does nothing, so the user must travel to the '⟨ Enter · use for Main ⟩' chip at the bottom.
  - repro: `real mode: /models; x,y=t.find('claude-opus-5 '); t.click(x+3,y); t.click(x+3,y). Compare F2 › Subagents: one t.click on 'Balanced favorite' opens the picker at once.`
  - code: crates/sterna/src/workbench/input.rs, the activate() match. `Action::Model(i) => { if let Some(m) = &mut self.models { m.selected = i; } }` (around line 915) only moves the selection. It never falls through to the Action::ChooseModel path (m.choose()), even when i == m.selected, and there is no double-click detection. The mouse Up handler (around line 255) activates on release with no click-count 
- **Image, speech and batch models are listed as choosable for Main and accepted** — defect, bug
  - expected: Only chat-capable models are offered for Main, Helper and Subagents (or they are marked and refused).
  - actual: '· model changed to gpt-image-2.5-sunburst', and the chip is truncated to '⟨ gpt-image-2.5-sun… ▾ ⟩'. Connected accounts list gpt-image-1.5/2/2.5/-flare/-sunburst and codex-auto-review. Ctrl-A adds whisper-large-v3(-turbo) and ':batch' variants.
  - repro: `real mode: /models; t.type('sunburst'); t.key('enter').`
  - code: There is no modality filter anywhere on this path. In crates/sterna/src/session/controls.rs, model_panel (line 1177) copies each account's models as-is into tui::ModelGroup ('models: account.models'), taken from the gateway's 'entitlements --json' catalogue (catalogue(), line 750). A row is refused only when it is unauthenticated or not selectable. On commit, crates/sterna/src/session.rs around li

### --resume picker (2)

- **Enter on 'nothing matches' quits pane silently; Esc with search text also quits instead of clearing** — defect, keyboard
  - expected: Enter with no match does nothing (or says so). The first Esc clears the search.
  - actual: Both leave the alternate screen and exit with status 0, and nothing is printed: no 'no session resumed', no hint.
  - repro: `Create a session, then t=Tui(fake=True, args=['--resume'], cwd=S, env={'XDG_CONFIG_HOME':H}); t.type('xyzzy'); t.key('enter')  (and separately: t.type('ref'); t.key('esc'))`
  - code: crates/sterna/src/session/resume.rs, pick(), key loop at lines 333-335. `KeyCode::Esc => break None` always cancels, even when the search has text. `KeyCode::Enter => break shown.get(selected).map(|s| s.id.clone())` gives None when the filtered list `shown` is empty, so an Enter with no match is treated like a cancel. choose() (lines 200-216) turns None into Ok(false). crates/sterna/src/session.rs
- **The resume picker ignores the mouse entirely and does not look like any other sheet** — defect, mouse
  - expected: Clicking a row selects it (and a double-click, or a click on a selected row, resumes it); the wheel scrolls. The frame matches the rest of the app: rounded frame, ⟨ Esc · Back ⟩ chip, › cursor.
  - actual: No mouse mode is ever enabled (no ESC[?1000h in its output), so click and wheel do nothing. It uses square corners, a full-width inverted selection bar and no chip. Home does not move to the first row, although PgDn and End reach the last. The search field has no cursor movement (Left/Right ignored), and Ctrl-U, Ctrl-W and Tab do nothing. The right column is jagged ('1 prompt' shifts with the id's length), and the ids touch the border.
  - repro: `t=Tui(fake=True, args=['--resume'], …); x,y=t.find('hello there'); t.click(x,y); t.scroll(x,y,up=False,times=2)`
  - code: crates/sterna/src/session/resume.rs pick() (lines 221-358): a standalone crossterm/ratatui loop. It never enables mouse capture and drops every non-key event. It draws square Borders::ALL with a REVERSED full-width selection and no chip. Its key match lacks Home/End/Left/Right/Ctrl-U/Ctrl-W/Tab. The right-hand 'N prompt(s)  id' string has no trailing padding.

### composer slash popup (2)

- **Esc does not close the slash popup** — defect, keyboard
  - expected: Esc closes the popup (and a second Esc clears the text), like Esc on every sheet.
  - actual: The popup and '❯ /' stay after two Escs. Only Backspace removes it. Esc also leaves text in place that a welcome chip ('pick up: init', 'finish setup') inserted; Ctrl-U is needed.
  - repro: `t=Tui(fake=True); t.wait('What next',15); t.type('/'); t.key('esc'); t.key('esc')`
  - code: crates/sterna/src/session/ui.rs around lines 1696-1709, in the idle-composer Esc branch. When nothing is busy, Esc only pops a queued message (state.queued.pop()) and then does `continue`. It never clears editor.text, and it has no popup-dismiss state. The popup has no dismissed flag of its own: it is derived purely from the text, via tui::slash_matches(&self.text) (ui.rs:639-664). So it can only 
- **Slash popup: colour-only cursor, no wheel, click doesn't run** — defect, mouse
  - expected: The popup shows its cursor like the sheets (›) and says that more commands exist; the wheel scrolls it; a click runs the command (or it is at least clear that Enter is still needed).
  - actual: The cursor is green text only. 7 of ~40 commands are shown, with no scroll hint. The wheel does nothing. A click fills '❯ /wizard' and leaves it there.
  - repro: `t=Tui(fake=True); t.wait('What next',15); t.type('/'); t.key('down',2); t.png('popup-down2'); t.scroll(3,36,up=False,times=2); p=t.find('/wizard '); t.click(*p)`
  - code: crates/sterna/src/workbench/view.rs:562-587 (popup render: min(7) cap, no overflow hint, button() highlight as the only cursor, Action::Insert on click); crates/sterna/src/session/ui.rs:639-664 (keyboard-only popup navigation, no wheel)

### Sign-in progress panel (2)

- **Esc hides a sign-in that keeps running and silently blocks every command** — defect, bug
  - expected: Esc either cancels the sign-in or leaves a visible, clickable 'signing in to X' indicator that reopens the panel. Other commands still run or say why they wait.
  - actual: The panel closes and the broker process keeps running. '/status' is not run: the composer shows 'Working. Your draft is kept; Ctrl-C interrupts tools; twice exits.' The label reads '✓ complete ✓' or '⠇ thinking…' and the sidebar '00:00 on this turn'. Nothing names the sign-in, and the panel cannot be reopened. Only Ctrl-C ends it, and the draft '/status' is left unsent.
  - repro: `Real mode, SSH_CONNECTION set: '/login subscription' → Enter on Grok (or '/login claude anyway'); wait for the panel; esc; type '/status' Enter`
  - code: crates/sterna/src/session/controls.rs stream_connect(): a blocking recv loop on the session thread that only Ctrl-C (interrupt token) ends; Esc just drops the panel on the UI side and nothing reopens it. crates/sterna/src/session/ui.rs ~1951: the busy branch refuses slash commands with a generic 'Working… Ctrl-C interrupts tools' notice that never names the sign-in.
- **After Ctrl-C cancels a sign-in, the panel stays up unchanged with live rows** — defect, inconsistency
  - expected: The panel says the sign-in was cancelled (or closes), and the code/link rows go away or offer 'start again'.
  - actual: Only the footer says 'Sign-in to chatgpt-subscription cancelled.'. The panel still shows the device code, '⏎ copy the code', '⏎ open the link in your default browser' and even 'waiting for the sign-in, then one request to check it works…'.
  - repro: `Real mode, SSH_CONNECTION set: '/login chatgpt' Enter; wait 'copy the code'; ctrl-c`
  - code: The cause is in crates/sterna/src/session/controls.rs, in `stream_connect` (around lines 986-1005). The `Err(RecvTimeoutError::Timeout) if token.is_cancelled()` branch kills the child, calls `session_println!("Sign-in to {account} cancelled.")` and breaks. It never sets `panel.outcome` and never calls `show(session, panel.render())`, so the last render stays on screen. `SignIn::render` (around lin

### approval redirect prompt ([a]) (2)

- **A paste in the 'another way' prompt goes into the hidden composer draft** — defect, bug
  - expected: The pasted text appears in the prompt after 'use tmp'.
  - actual: The prompt still shows '❯ use tmp▏' while the composer behind it shows '❯  PASTED'. The stray text stays in the draft after the turn ends.
  - repro: `Every call rung; at an approval press a; t.type('use tmp'); t.paste(' PASTED'). (gaps-2/scripts/s06_deny.py turn 4)`
  - code: crates/sterna/src/session/ui.rs, the Event::Paste arm (about line 1285). It skips a paste only when settings_editor is open or approvals is non-empty. Pressing [a] pops the request out of `approvals` into `redirect` (line 1363), so `approvals` is empty while the redirect prompt is open. The paste then falls through to editor.insert(&text), which is the hidden composer. The Key arm (lines 1315-1345
- **The 'another way' prompt has no cursor movement, and Esc back to the call throws the typed words away** — defect, keyboard
  - expected: Left/Right move the cursor like in the composer. The prompt says 'Esc goes back to the call', so the words are kept for the next [a].
  - actual: '#' is appended at the end ('put it in docs/ instead#'). After Esc and a again the prompt is empty ('❯ ▏').
  - repro: `At an approval press a; t.type('put it in docs/ instead'); t.key('left'); t.key('left'); t.type('#'); then Esc; then a again. (gaps-2/scripts/s22_redirect.py)`
  - code: In crates/sterna/src/session/ui.rs, around lines 1315-1345, the redirect state is `Option<(approval::Request, String)>` (declared at line 867) and has no cursor index. The key match handles only Enter, Esc, Ctrl-C, Ctrl-U, Backspace (`text.pop()`) and Char (`text.push(c)`), and every other key, including Left, Right, Home and End, falls through to `_ => {}`. On Esc, `redirect.take()` puts the requ

### composer edge / sidebar (1)

- **Opening and closing a sheet is reported as a completed turn** — defect, bug
  - expected: The edge stays '◇ ready when you are' because nothing ran.
  - actual: The edge says '✓ complete ✓', the sidebar adds 'THIS SESSION 00:00 on this turn' (00:06 and 00:07 after longer stays in /key and /login custom), and the parrot changes pose. The same happens after /login, /key and /config.
  - repro: `t=Tui(fake=True); t.wait('What next',15); t.type('/wizard'); t.key('enter'); t.key('esc'); print(t.screen())`
  - code: crates/sterna/src/session.rs around lines 968-981, in the TUI input loop `while let Some(input) = ui.next()?`. Each input goes through process_input, and then ui.publish(..., Activity::Complete) runs no matter what, including when the input was only a slash command that opened a sheet: `ui.publish(transcript, &ServedBy::default(), if result.is_err() { tui::Activity::Failed } else { tui::Activity::

### composer edge status (1)

- **Doubled check mark: '✓ complete ✓'** — polish, copy
  - expected: One mark: '✓ complete'.
  - actual: The edge reads '╭─ ✓ complete ✓ ──' in the playful voice. The classic/plain voice shows '✓ complete'.
  - repro: `any parrot theme: t.type('hello'); t.key('enter'); t.wait('Nothing else',20); print(t.screen().split('\n')[-3])`
  - code: crates/sterna/src/workbench/voice.rs:146 `Activity::Complete => if playful { "complete ✓" } else { "complete" }.into()` adds a ✓ to the playful status word. crates/sterna/src/workbench/motion.rs:32 dock_mark already puts '✓' in front of that word when Activity::Complete. Fix: drop the trailing ' ✓' from the playful string in voice.rs.

### status line (1)

- **Stopped, cancelled and Ctrl-C'd turns all read '✓ complete ✓', and Ctrl-C shows nothing at all** — defect, bug
  - expected: The status and the transcript say the turn was stopped or cancelled, for example '■ stopped by you', and the start of the stop is acknowledged.
  - actual: Ctrl-C: the turn silently ends with '✓ complete ✓', no answer and no note. Esc twice: 'Cancelling the call in flight.' followed by '✓ complete ✓'. Esc once: the last transcript line stays 'Stopping after this cell…' while the status reads '✓ complete ✓'. Ctrl-C is not listed on the ? keys sheet.
  - repro: `t=Tui(fake=True) with the model slowed by 5 s; t.type('slow question'); t.key('enter'); t.settle(1.2); t.key('ctrl-c'); t.settle(6)   (also: t.key('esc') once or twice instead)`
  - code: crates/sterna/src/tui.rs:397 `enum Activity` has no Stopped/Cancelled variant (Idle, Starting, Thinking, Streaming, Executing, Searching, Waiting, Compacting, Complete, Failed). Every way a turn ends publishes Activity::Complete when it returns Ok:
- crates/sterna/src/session.rs ~1178 (the run_task_inner end: `if result.is_ok() { Complete } else { Failed }`)
- session.rs ~973 (the ui.next loop)
- 

### status strip / sidebar effort (1)

- **The effort chip vanishes when it cycles back to default, and clicking 'helpers off' in the sidebar changes effort instead** — defect, mouse
  - expected: The effort control stays where it was clicked in every state, including default. Each word on the sidebar line acts on what it names.
  - actual: At 'default' the strip chip disappears from under the pointer (the user's complaint: 'soon as its default it disappears'), and the next click must go to the sidebar or /effort. Clicking 'helpers off' printed 'Effort: medium · applied to the next request' and left helpers off. Effort also cycles while no Main model is chosen. The top-bar model chip never shows the effort.
  - repro: `real mode: /model claude-opus-5; click the sidebar 'effort default' → the strip gains '⟨ effort low ⟩'; click the strip chip repeatedly: medium → high → xhigh → max → default. Separately: fresh session, x,y=t.find('helpers off'); t.click(x+2,y).`
  - code: crates/sterna/src/workbench/view.rs about line 1161: the status-strip chip text is String::new() when s.effort == Effort::Default, following the rule 'a chip says only what differs from Sterna's own default'. So after the effort ladder wraps to default, the chip it was clicked on is gone. crates/sterna/src/workbench/view.rs about lines 883-891: the sidebar renders 'effort {} · helpers {}' as one l

### Sidebar · 'effort default · helpers off' line (1)

- **Clicking 'helpers off' in the sidebar changes the effort** — defect, mouse
  - expected: Clicking the helpers half opens the helpers settings, as the '◇ HELPERS / off' line does.
  - actual: The whole line is one Action::Effort target, so effort steps default -> low. The notice reads 'Effort: low · applied to the next request ⟨ undo effort default ⟩' and a '⟨ effort low ⟩' chip appears.
  - repro: `p=t.find('helpers off'); t.click(p[0]+3,p[1])`
  - code: crates/sterna/src/workbench/view.rs:883-891. The sidebar MODEL section builds one line, format!("effort {} · helpers {}", ...), and gives the whole row the single target Some(Action::Effort), so any column on it steps effort. Handled at crates/sterna/src/workbench/input.rs:761. Possible fixes: split it into two click targets (the helpers part opening the helpers sheet), or drop 'helpers' from this

### first turn (show me around) (1)

- **The start card promises reading commands run, but the first read-only command is denied outright** — defect, bug
  - expected: As the card says ('permissions: auto — edits run, a command that only reads runs, anything else is confirmed') and the sidebar says ('asks before risky commands'), `ls -la && git log --oneline -3` runs or is asked about.
  - actual: The first cell fails with '✕ PermissionDenied: Bash("ls -la && git log --oneline -3") rule: no `Bash` pattern in permissions.allow admits `ls -la`', and the header says ✕ FAILED. Nothing was asked. /status shows 'Sandbox: 0 path rules · 0 command patterns'.
  - repro: `d=tempfile.mkdtemp(); t=Tui(fake=True,cwd=d); t.wait('What next',15); x,y=t.find('show me around'); t.click(x+2,y); t.key('enter'); t.settle(6); print(t.screen())`
  - code: crates/sterna/src/sandbox/profile.rs Profile::admits_command (~lines 892-950): each command segment must match a `Bash(...)` pattern in command_allow, or the call is denied outright. With the default 0 command patterns, every command is a hard PermissionDenied at the profile layer. The permission ladder never reaches its "only reads runs / else confirm" decision. The card copy that promises this c

### Home greeting after a permission change (1)

- **The greeting keeps the old permission rung** — defect, bug
  - expected: The greeting line matches the rung in force
  - actual: The top bar says '⟨ Every call ⟩' and the sidebar 'asks before every call', but the greeting still says 'permissions: auto — edits run, a command that only reads runs, anything else is con…'. The same happens after clicking 'Commands' on the ASK sheet.
  - repro: `/settings; set Permission rung to manual (Right…, Enter); Esc`
  - code: The greeting row is not built from live state. In crates/sterna/src/workbench/document.rs (around line 1158), the card takes `startup.first()` from the startup log lines. At session start, crates/sterna/src/session.rs:628 writes `session_println!("{}", startup::permissions_line(&ladder))`, which crates/sterna/src/session/startup.rs:218 builds from the ladder's rung at that moment. Nothing rewrites

### opening card (1)

- **Clicking 'permissions: auto — …' tells a joke instead of opening permissions, and the truncated line can never be read in full** — defect, mouse
  - expected: Clicking the permissions line opens the permission rung picker (like Shift-Tab), or at least shows the full sentence.
  - actual: The composer edge shows a parrot quip ('I read the whole file. Both times.'). The line stays cut at '…anything else is con…' at 140 cols and '…a command that only re…' at 80 cols.
  - repro: `t=Tui(fake=True); t.wait('What next',15); x,y=t.find('permissions: auto'); t.click(x+2,y)`
  - code: In crates/sterna/src/workbench/document.rs, fn perched (~lines 1196-1238) builds every sprite row with `self.line(spans, Some(Action::Quip), 0)`. That includes the rows carrying the startup facts, the permissions line among them, so the whole row goes to Quip and the fact text never gets its own action. The same function clips the text with `clip(text, room)`, where room = width - WIDTH - 6. It ad

### Permissions chip / Shift-Tab / sidebar / Settings / notices (1)

- **One permission setting goes by at least six different names** — defect, copy
  - expected: One name per rung, used everywhere.
  - actual: Chip: 'Every call / Commands / Auto-review / Never asks'. Settings row: 'Permission rung ⟨ manual ⟩ ⟨ accept-edits ⟩ ⟨ auto ⟩ ⟨ full ⟩'. Notices: 'permissions manual — Shift-Tab cycles, /permissions <rung> sets one'. Result sheet: 'permissions: auto → accept-edits (Shift-Tab cycles; 3 of the four rungs ask)'. Access: 'asks auto-review'. Sheet title: 'ASK'. Sidebar: 'asks before nothing' (on full). The Auto rung is described two ways: welcome 'a command that only reads runs', ASK sheet 'a command a reviewer can vouch for runs'. The /permissions completion text 'inspect or configure next-session grants' is wrong: it changes the current session.
  - repro: `Compare: top chip after t.key('backtab') x1..3; sidebar GUARDRAILS line; t.find('⟨ Settings') click -> Permission rung row; p=t.find('Auto-review') click then Enter (notice); t.type('/permissions accept-edits'); t.key('enter'); p=t.find('This project') click (Access sheet)`
  - code: crates/sterna/src/permissions.rs Rung::label()/sentence() is the intended single source of the names, but several sites print the stored key or their own wording instead: settings/registry.rs:363 (choices come from Rung::NAMES), workbench/input.rs:990, session/controls.rs:1631, session/startup.rs:217 permissions_line, workbench/view.rs:867 and :1483 (ask_before/ask_word), and tui.rs:669 (the compl

### Build chip / /mode / /status (1)

- **The work mode is 'Build' on the chip and 'execute' everywhere else** — polish, copy
  - expected: 'Build' everywhere, or 'execute' everywhere.
  - actual: Chip and sheet: 'Build'. Completion: '/mode — execute, explore (reads only) or plan'. Usage: 'Use /mode execute|explore|plan|auto'. '/mode build' is accepted, but the notice replies 'Mode: execute · pinned · session sandbox applies'. /status shows 'Mode: execute'. 'pinned/unpinned' is never explained.
  - repro: `t.type('/mode'); (read completion) ; t.type(' build'); t.key('enter'); t.type('/status'); t.key('enter')`
  - code: crates/sterna/src/workbench/view.rs:92 maps Mode::Execute to "Build". Every other user-facing string uses the internal name "execute": crates/sterna/src/tui.rs:671, crates/sterna/src/session/controls.rs:1280-1305 and 1414, crates/sterna/src/sandbox/modes.rs:206/220, crates/sterna/src/session/mode_proposal.rs:72, and crates/sterna/src/session/args.rs:153. The notices print the mode's internal name,

### opening card copy (1)

- **First-ever run greets 'Back in the nest' and clicks produce bird puns** — polish, copy
  - expected: A first run is not a return. Plain, factual copy by default (the user does not want bird puns).
  - actual: 'Evening! Back in the nest.' shows on a brand-new config, and 'Late one! Back in the nest.' at night. Clicks give 'The early bird gets the diff.', 'Perched and ready.', and 'I'm a wren. Probably.' (on a card that says Blue-fronted Amazon). Even with ui.voice=plain the card keeps 'Blue-fronted Amazon · a talker, and it listens first'.
  - repro: `t=Tui(fake=True); t.wait('What next',15); t.click(8,6); t.click(8,6); x,y=t.find('Blue-fronted'); t.click(x+2,y)   # Tui(env={'TZ':'Etc/GMT-6'}) for the night greeting`
  - code: crates/sterna/src/workbench/voice.rs:greeting() (lines 48-64): the playful branch always says "Back in the nest." and has no first-run check. The Voice default is Playful (crates/sterna/src/tui.rs Voice / settings.rs:1312). voice.rs:quip() (lines 236-260) holds the 24 bird puns, including "I'm a wren. Probably." whatever species is shown. crates/sterna/src/workbench/document.rs:1211 in perched() s

### status / sidebar / inspection sheets (1)

- **Cryptic or contradictory copy across the conversation chrome** — polish, copy
  - expected: Plain words, and one name per concept.
  - actual: The composer border shows 'ctx 12 · reported' / 'ctx 7.9k · estimated'. /tool prints '/tool read: exit 0 under seatbelt'. /status says 'Mode: execute' while the top bar says 'Build' and the sidebar 'mode build'. Other strings: 'Sandbox: … network false', 'Cell limit: none (a task ends on evidence, not a count)', /context 'Task spend: cumulative telemetry, no cap'. The sidebar header 'THIS SESSION' is followed by '00:00 on this turn' after the turn has ended. Ctrl-B is 'session card' in the help and tips but '/sidebar' as a command. The '✓ 0  ● 0  ✕ 1' row has no legend.
  - repro: `t.type('what is in this project?'); t.key('enter'); t.wait('complete'); then /status, /tool read path=README.md, /context, and read the composer border`
  - code: Where each string lives (read-only):
- crates/sterna/src/session/controls.rs:1414 has the /status format string ('Mode: {}', 'network {}' from a bool, 'Task spend: tracked, uncapped'). Line 1371 has the /context text ('Task spend: cumulative telemetry, no cap', 'rollout'). Line 16 has 'none (a task ends on evidence, not a count)'. Lines 1280–1305 hold the /mode strings, which use the execute/explo

### wizard / Sign in sheets / key form / picker opened from the wizard (1)

- **Esc is labelled 'Back' but closes the whole setup stack, and one sheet gives it three different names** — defect, inconsistency
  - expected: Esc (and the '⟨ Esc · Back ⟩' chip) goes back one level: Setup › Models goes to Setup, Sign in › API key goes to Sign in, and the key form goes to the key list. The label says what the key does.
  - actual: Every Esc, and every click on the chip, drops to the chat. The same sheet names Esc three ways: '⟨ Esc · Back ⟩' in the header, 'Esc finish later' in the hints, 'Esc closes · saved choices stay' in the footer. The form says 'Esc · back' and the picker says 'Esc back · the assignment stays unchanged'. Only the in-list 'Back' rows actually go back.
  - repro: `t=Tui(fake=True); t.wait('What next',15); t.type('/wizard'); t.key('enter'); t.key('down'); t.key('enter'); t.key('esc'); print(t.screen())   # also: /wizard jev -> Esc; real /login key -> click '⟨ Esc · Back ⟩'; real /login key -> Enter on anthropic -> Esc; real wizard -> Models -> Choose each one myself -> Esc`
  - code: In crates/sterna/src/workbench/input.rs, around line 405, Esc on any local sheet or panel calls self.close() and sets s.panel = None. The only earlier steps are clearing a preference edit or query, and there is a restore of model_preference. No parent sheet is kept anywhere, so Esc cannot return to 'Setup' or to the key list.

In crates/sterna/src/workbench/view.rs, around line 1338, the header ch

### /wizard › Models › Choose each one myself (model picker) (1)

- **'Choose each one myself' opens a raw 474-model list and exits after the first choice** — defect, loose-end
  - expected: The picker opens on Sterna's recommended models and walks Main -> Helper -> Subagents, returning to the wizard.
  - actual: The picker opens on 'claude-3-5-haiku-20241022' (alphabetically first) with Sterna's gpt-6-* picks below the fold. 'Main answers you. Now:' shows nothing after 'Now:'. The Helper and Subagents tabs say 'Now: off' yet offer '⟨ Turn this tier off ⟩'. The Subagents tab shows two '›' cursors ('› PINNED' and '› claude-3-5-haiku…') and 'off empty empty empty empty'. One Enter on Main closes everything to chat ('· model changed to gpt-6-sol'), with Helper and Subagents never offered.
  - repro: `t=Tui(fake=False); t.wait('Setup ·',25); t.key('down'); t.key('enter'); t.key('down'); t.key('enter'); t.settle(2); print(t.screen()); t.key('tab',2); print(t.screen()); t.key('tab'); t.type('gpt-6-sol'); t.key('enter')`
  - code: crates/sterna/src/session/setup.rs models_panel() maps "Choose each one myself" to a bare `/models`, and crates/sterna/src/workbench/input.rs Action::ChooseModel does close() plus Effect::Command after one Enter, so there is no tier walk and no return to the wizard. crates/sterna/src/workbench/sheets.rs (roles/"Now:" around lines 237-296 and the off chip around line 495) prints an empty "Now:" for

### all panel sheets (wizard, Sign in, Subscription, API key list) (1)

- **One mouse-wheel notch jumps the selection three rows, so some rows cannot be reached by wheel** — defect, mouse
  - expected: The wheel scrolls, or moves one row per notch.
  - actual: In the wizard, one notch goes from step 1 straight to step 3, and step 2 cannot be reached by wheel. In the subscription list the wheel only ever lands on ChatGPT, Claude and Muse Code (Grok, Kimi, Gemini and Devin are skipped). In the model picker, 3 notches move the highlight 9 models. Hover gives no highlight anywhere.
  - repro: `t=Tui(fake=True); t.wait('What next',15); t.type('/wizard'); t.key('enter'); t.scroll(40,10,up=False)   # selection 1 -> 3; also real '/login subscription': three notches go ChatGPT -> Claude -> Muse Code`
  - code: crates/sterna/src/workbench/input.rs lines 312-319: the ScrollUp/ScrollDown arm calls move_index(selected, up, 3, len) for preferences (p.rows()), models (m.candidates()) and s.panel (p.rows). move_index at line 1007 does i.saturating_add(step).min(len-1) or i.saturating_sub(step), so a step of 3 skips rows. The fix is step 1 for selection lists, or scroll the viewport and leave the selection alon

### All chips (1)

- **No chip reacts to hover** — polish, mouse
  - expected: A hover highlight, so a mouse user can tell what is clickable. The muted sidebar lines, for example, look like plain text.
  - actual: No cell changes on any chip. The app enables only ?1000, ?1002 and ?1006 (no ?1003 any-motion), so real terminals never report hover. A press does show feedback: Build turns black on green while held, and releasing elsewhere cancels.
  - repro: `for each of fixture-model, Auto-review, Build, This project, Settings, ?, effort high, activity, telemetry: p=t.find(label); t.hover(p[0]+1,p[1]); compare cell fg/bg`
  - code: crates/sterna/src/session/ui.rs:86-108. ENABLE_MOUSE_REPORTING = b"\x1b[?1000h\x1b[?1002h\x1b[?1006h". The doc comment explains that ?1003 (any motion) was left out on purpose because stray motion reports risk being split into text at read boundaries (session/ui/terminal_input.rs). terminal_input.rs:525 already parses motion into MouseEventKind::Moved, but no code draws a hover state. Adding hover

### first-run sheets (wizard, Sign in, forms, Settings, model picker) (1)

- **Each setup sheet has its own navigation concept** — defect, inconsistency
  - expected: One model: the same key moves, the same key toggles or chooses, and the same key goes back on every sheet.
  - actual: Tab is dead on panels, but moves fields on forms, switches category in Settings and switches tier in the picker. Space is dead on panels and forms, and in Settings silently starts a search. ←→ is dead on panels, but picks a choice on forms and changes a value in Settings. The hint lines use different vocabularies: '↑↓ choose · Enter open · Esc finish later' vs '↑↓ Select · ←→ Change · Enter Edit · Backspace Inherit' vs 'Enter connect · Tab next field · ←→ choose · Ctrl-U clear · Esc back'.
  - repro: `Wizard/Sign in: t.key('tab'); t.key('space'); t.key('right') -> nothing. Form (/login custom): Tab/↓ = next field, ←→ = choice, Space on the choice = nothing, Enter = submit the whole form. /config: ←→ = change, Space = typed into search (the list jumps to 'Main model' in another category), Tab = next category, F6 = scope. Model picker: Tab = Main/Helper/Subagents, ←→ = slot, Enter commits and closes.`
  - code: Each sheet handles keys in its own match arm and there is no shared key map. crates/sterna/src/workbench/input.rs:448-520 (Settings: Tab changes category, ←→ cycles values, other characters go to search) and :554-575 (model picker: Tab changes tier, ←→ only on Subagents); crates/sterna/src/session/ui.rs:1449 (form: Tab and Down move the field focus); the hint strings are in crates/sterna/src/workb

### /settings keyboard (1)

- **Space starts an invisible search instead of toggling, and the legend's 'Enter Edit' actually changes the value** — defect, keyboard
  - expected: Space toggles or cycles the selected choice, as on a checkbox. Enter either edits or the legend says what it does.
  - actual: Space adds ' ' to the search. 'Search:' appears with nothing visible after it, the cursor jumps to row 0 (Main model), and the list turns into a mixed cross-category result while the heading still reads '‹ Everyday ›'. Enter on the Motion row immediately saves 'Motion is now calm' although the legend reads '↑↓ Select · ←→ Change · Enter Edit'. On /permissions and /theme, Space does nothing.
  - repro: `/settings; down x4 (Helpers); t.key('space')  |  /settings; down x6 (Motion); t.key('enter')`
  - code: crates/sterna/src/workbench/input.rs, the settings-sheet key match (around lines 448-527). `KeyCode::Char(c) if !ctrl => { p.query.push(c); p.selected = 0; }` catches ' ', because there is no Space arm before it, so Space becomes a search character and resets the selection. `KeyCode::Enter` calls `p.cycle(true, s)` for any row whose `Preferences::choices(spec)` is not empty; it only enters edit mo

### /models keyboard (1)

- **Picker keys differ from the neighbouring Settings sheet: Esc drops a typed search, and Shift-Tab, Home and End do nothing** — defect, keyboard
  - expected: Esc clears the query first, as in Settings. Shift-Tab goes to the previous tier. Home and End jump to the ends of the list.
  - actual: Esc closes the picker along with the query. Shift-Tab, Home and End do nothing. Space types a space into the search ('┃  ▏'). The code comment claims Space 'stages a choice', but it does not.
  - repro: `real mode: /models; t.type('opus-5'); t.key('esc') → the whole sheet closes. Compare F2 › type 'model' › Esc, which clears the search first. Then in /models: t.key('backtab'); t.key('home'); t.key('end'); t.key('space').`
  - code: crates/sterna/src/workbench/input.rs. The Esc branch at about lines 405-420 clears the query first only for `self.preferences`, not for `self.models`. The models key match at about lines 541-596 has no BackTab, Home or End arms, and `KeyCode::Char(c)` swallows Space into the query. The comments at workbench/models.rs:80 and tui/controls.rs:457 claiming Space stages a choice are out of date.

### /settings Backspace (1)

- **One Backspace too many after clearing a search resets a setting without asking** — defect, keyboard
  - expected: Backspace on an empty search does nothing, or 'reset to inherited' needs a deliberate key or confirmation
  - actual: The first two Backspaces clear 'mo'. The third resets the selected row: the notice says 'Theme is now unset. Ctrl-Z undoes it.' and the theme falls back to amazon. The same thing happened to Main model ('Main model is saved.') when Backspace was held down to clear a long query.
  - repro: `/settings; Tab (Display); Right (theme -> sun-conure); type 'mo'; backspace x3`
  - code: crates/sterna/src/workbench/input.rs lines ~480-489, in the settings sheet's Backspace arm: `if !p.query.is_empty() { p.query.pop(); p.selected = 0; Ok(()) } else if let Some(spec) = spec { p.save(spec.key, None, s) }`. With the query empty, Backspace saves None (unset) for the selected spec with no guard. A fix would ignore Backspace for about one key-repeat after the query empties, or move reset

### opening screen 'finish setup' chip (1)

- **The 'finish setup' chip only types /wizard; it does not open setup** — defect, mouse
  - expected: Clicking the chip opens the wizard, as clicking a wizard row opens its step.
  - actual: The composer shows '❯ /wizard' with the completion popup, and a second action (Enter) is needed. The opening chips also cannot be reached from the keyboard: Tab, ↓ and ↑ do nothing on the opening.
  - repro: `t=Tui(fake=True); t.wait('What next',15); x,y=t.find('finish setup'); t.click(x+3,y); print(t.screen())`
  - code: crates/sterna/src/workbench/document.rs about line 1254-1262: every opening-screen suggestion chip is mapped to `Action::Insert(message)`, which types the text into the composer instead of running it. The setup chip comes from crates/sterna/src/session/setup.rs:460-466, `ui.suggest("finish setup · N steps left", "/wizard")`, through session/ui.rs:438 `suggest` and ui.rs:995-1001 `Update::Suggest`,

### Composer · /mode (1)

- **Typing '/mode' and pressing Enter opens the Models picker** — defect, bug
  - expected: The exact command /mode opens the WORK sheet. Bare /mode is wired to do that (input.rs local_command ['/mode']).
  - actual: The completion list shows '/model', '/models', '/mode' and highlights /model first. Enter accepts /model, so the 'Models' sheet opens. /mode with no argument therefore cannot be reached by typing it.
  - repro: `t.wait('What next',15); t.type('/mode'); t.key('enter')`
  - code: In crates/sterna/src/session/ui.rs around lines 663-668, the KeyCode::Enter arm unconditionally replaces the typed text with tui::slash_matches(&self.text).get(self.selected), and selected defaults to 0, even when the text already exactly matches a command. In crates/sterna/src/tui.rs, slash_matches (line ~586) lists BUILT_INS first (Model, Models, ...) and appends the local commands such as '/mod

### Top bar · This project (ACCESS sheet) (1)

- **In the Access sheet the arrow keys highlight the buttons, but Enter and Space do nothing** — defect, keyboard
  - expected: Enter on a highlighted button activates it, as a mouse click does.
  - actual: '⟨ Change how often it asks ⟩' is drawn highlighted, but Enter and Space leave the ACCESS sheet exactly as it was. Only a mouse click opens ASK or SETTINGS.
  - repro: `p=t.find('This project'); t.click(p[0]+2,p[1]); t.key('down'); t.key('enter')   (also: t.key('down'); t.key('space'); and down,down,enter)`
  - code: crates/sterna/src/workbench/input.rs ~L630: the `self.activity || self.access` key branch has no Enter/Space arm and no focus index. crates/sterna/src/workbench/view.rs ~L1514-1535: the access chips' `on` flag is the constant true/false instead of a focus index.

### Top bar · Auto-review (ASK sheet) and Build (WORK sheet) (1)

- **The Ask and Work sheets open with the cursor on row 1, not the current row, so Enter changes the setting** — defect, keyboard
  - expected: The cursor opens on the '▸ … · now' row, so Enter (or a stray Enter) keeps the current value.
  - actual: The cursor (green highlight, no glyph) opens on 'Every call' / 'Build'. In A, one Enter drops the session from Auto-review to Every call ('⟨ Every call ⟩', sidebar 'asks before every call'). In B, one Enter switches Plan to Build ('Mode: execute · pinned …'). Neither change gets an undo chip. The effort sheet has the same start position: it opens on 'default' even when the title says 'current high'.
  - repro: `A) t.wait('What next',15); p=t.find('Auto-review'); t.click(p[0]+2,p[1]); t.key('enter').  B) t.type('/mode plan'); t.key('enter'); p=t.find('⟨ Plan'); t.click(p[0]+3,p[1]); t.key('enter')`
  - code: crates/sterna/src/workbench/input.rs:110-118 and 817-824 open the WORK and ASK sheets without setting local_scroll to the current mode or rung. It stays at 0 from close() in crates/sterna/src/workbench/mod.rs:168, and the Enter handler at input.rs:606-624 applies whatever row index it holds. Fix: after self.work = true, set local_scroll to the index of s's current mode, and after self.approvals = 

### approval sheet (1)

- **Approval sheet: no mouse, Enter does nothing, Esc denies, raw JSON, off-theme frame** — defect, inconsistency
  - expected: Allow, Allow for session, Another way and Deny are clickable chips, arrows plus Enter pick one like every other sheet, Esc means Back rather than a decision, and the call is shown readably (the path relative to the project, or a diff of what will be written).
  - actual: Clicking '[o] Allow once' does nothing: the file is not written and the timer keeps running. Enter and Down do nothing. Only the letter keys o/s/a/d work, and Esc is Deny. The body is a raw JSON dump with absolute '/private/var/folders/…/notes.txt' and a 'workspace' key. The frame is square with neon green text, unlike the themed rounded sheets, and clipped sidebar fragments show beside it ('ff', 're every call', 'odel').
  - repro: `t=Tui(fake=True); t.wait('What next',15); x,y=t.find('Auto-review'); t.click(x+2,y); x,y=t.find('Every call'); t.click(x+2,y); t.key('esc'); t.model.script=[W,'I wrote notes.txt']; t.type('write a notes file'); t.key('enter'); t.wait('Approve exact tool call'); t.key('enter'); t.key('down'); x,y=t.find('[o] Allow once'); t.click(x+5,y)`
  - code: crates/sterna/src/tui.rs render_approval (lines 73-140): a square Borders::ALL block with the hard-coded ACCENT = Color::LightGreen instead of the theme, the confirmation.text JSON shown verbatim, and a static footer string rather than chips. The overlay is width min(100) and centred, but Clear only covers the overlay, so the sidebar bleeds beside it. crates/sterna/src/session/ui.rs: the mouse bra

### layout (modals, narrow, light) (1)

- **Overlap, clipping and low contrast in edge layouts** — defect, visual
  - expected: Modals cover the sidebar cleanly, the '↓ latest' pill does not overwrite content, and the text stays readable on light terminals.
  - actual: (a) Clipped sidebar text shows beside the modal's right border ('┐ff', '│re every call', 'odel'). (b) At 80 columns, '⟨ ↓ latest ⟩' overwrites the cell card's right border, and the top bar drops the Build / This project / ? chips, leaving no mouse path to the mode or to help. (c) On a light background, '✓ EXECUTED', 'returned', '✓ write a.txt', the 'you' label, 'network off' and the placeholder are pale green or grey on white and barely legible. There is no background detection (no OSC 11 query in the source).
  - repro: `(a) the approval or ask sheet at 140x42; (b) Tui(cols=80,rows=24), one turn, t.scroll(30,10,up=True,times=5); (c) t.png('x', light=True) after a cell turn`
  - code: (a) In crates/sterna/src/tui.rs, render_approval (around lines 74-95) sets `width = area.width.saturating_sub(4).min(100)` and centres the box on frame.area(). It runs `Clear` over the overlay only, with no backdrop, so the sidebar shows to the right of the box. It also uses the old LightGreen/Gray constants instead of the workbench theme.
(b) In crates/sterna/src/workbench/view.rs (around lines 4

### /subagents command (1)

- **`/subagents` with no arguments errors with a truncated usage line instead of opening the picker** — defect, loose-end
  - expected: Opens /models on the Subagents tab, as `/model` opens the picker.
  - actual: '✕ ERROR: Use /subagents on|off, or /subagents quick|balanced|deep|heavy MODEL [EFFORT]. Use MODEL=off to…' is cut off with an ellipsis, and the prompt frame header says '✓ complete ✓'. `/subagents quick` without a model gives the same error.
  - repro: `real or fake mode: t.type('/subagents'); t.key('enter').`
  - code: crates/sterna/src/session/controls.rs:1237 sends "subagents" straight to subagents::assign(session, argument.unwrap_or_default()) and prints "ERROR: {error}" when that fails. It never checks for an empty argument, so it cannot open the models sheet on its Subagents tab. The catch-all arm `_ => return Err("Use /subagents on|off, ...")` is at crates/sterna/src/session/controls/subagents.rs:46 and ma

### /models copy (1)

- **Cryptic copy and four names for one concept (PINNED / slot / favourite / roster)** — polish, copy
  - expected: Plain words: one name for the favourites, a 'Now' value that is never blank, a legend for ★, readable group headers.
  - actual: Title 'MODELS  one Enter commits one selection'. Headers 'CLAUDE · CLAUDE-MAX · ACCOUNT-DECLARED' and 'GROQ · GROQ · PROVIDER-DECLARED'. '★ 50' with no scale or legend, and most rows unscored. 'Main answers you. Now:' is blank when nothing is chosen. 'Now: favorite roster'. '· Subagents: roster · 1 configured favorites · next launch uses this assignment; in-flight jobs unchanged' mixes 'roster', '1 … favorites', and the spelling 'favorites' against the sheet's 'Favourites'. The cards say PINNED, the hint says '←→ slot', the toggle says 'Favourites', Settings says 'Subagent mode … roster'. 'Turn this tier off' is offered while the tier is already off and gives no feedback when clicked.
  - repro: `real mode: /models; Tab ×2; /subagents quick claude-haiku-4-5-20251001; /subagents on.`
  - code: Copy is spread across these sites (all under crates/sterna/src):
- Title: workbench/view.rs:1288 ('MODELS', 'one Enter commits one selection').
- PINNED card title: workbench/sheets.rs:309 (`slot.map_or("PINNED"...)`).
- 'Turn this tier off' button: workbench/sheets.rs:506. It is offered even when the tier is already off.
- '←→ slot' hint: workbench/sheets.rs:531.
- 'favorite roster' for the Now v

### /motion, /sidebar, /statusline, /theme entry points (1)

- **Four display commands have four different bare behaviours** — defect, inconsistency
  - expected: Each bare command opens a place to choose, e.g. Settings at its row
  - actual: /theme opens a picker. /statusline opens Settings › Display on its row. /motion only prints 'Usage: /motion full | calm | off'. /sidebar only prints 'Sidebar: /sidebar auto|show|hide · Ctrl-B toggles' and does not say the current value. Ctrl-B toggles the sidebar with no notice and does not save, while /sidebar hide saves to the project.
  - repro: `Run /motion, /sidebar, /statusline and /theme each with no argument`
  - code: The bare-command arms live in two places, each written separately: crates/sterna/src/workbench/input.rs (/theme at line 123, the /motion catch-all at 163, /sidebar with a word at 177) and crates/sterna/src/session/ui.rs (/statusline opens settings at 1881; the bare /sidebar fallback at 1989, which also resets to Auto; the Ctrl-B toggle at 1744 has no note and no persist). A fix would send bare /mo

### /models tabs (1)

- **Switching tier (Tab or clicking a tab chip) moves the highlight to the first row (claude-3-5-haiku-20241022), not the tier's current model** — defect, keyboard
  - expected: Each tab opens with its current model highlighted (and marked as current), so Enter never silently assigns the oldest model.
  - actual: The highlight resets to row 0 on every tier change. Enter would assign claude-3-5-haiku-20241022. The list never marks which row is the current one.
  - repro: `real mode: /model claude-opus-5; /model helper gpt-5.6-luna; /models → the highlight is on claude-opus-5; t.key('tab') → Helper 'Now: gpt-5.6-luna' but the highlight is on claude-3-5-haiku-20241022; Tab ×2 back to Main → the highlight is still row 0. Clicking the 'Helper' chip does the same.`
  - code: Both tier-switch paths set `m.selected = 0` instead of calling the existing `Navigator::select_current()` (crates/sterna/src/workbench/models.rs:53). That function already picks the right model for each tier: the helper, the subagent slot, or the parent. The two paths are: (1) crates/sterna/src/workbench/input.rs:554-556, `KeyCode::Tab if m.target_key.is_none() => { m.role = (m.role + 1) % 3; m.se

### /settings Theme row (Everyday and Display) (1)

- **Six of sixteen themes are off-screen in the Theme row, and choosing one leaves no chip lit** — defect, visual
  - expected: Every theme can be seen and clicked, and the chip for the active theme is highlighted (wrap the row, scroll it, or show only the current value plus a picker)
  - actual: The row ends at 'sun-conure'. hyacinth, scarlet, blue-gold, green-wing, military and cockatoo are never drawn, so the mouse cannot reach them. After Right x2 the detail line reads 'hyacinth · set in this project's settings' but no chip in the row is highlighted. At 120 cols the row stops at 'rose', so even the default 'amazon' is hidden. At 80 cols it stops at 'mono'. The Reasoning effort row has the same problem: at 80 cols it drops xhigh and max, and setting max lights nothing.
  - repro: `t=Tui(cols=140,rows=42); t.type('/settings'); t.key('enter'); t.key('down',5); t.key('right',2); t.png('theme-row-hyacinth')`
  - code: crates/sterna/src/workbench/view.rs, around lines 1741-1782, the settings row renderer. It lays the `Preferences::choices(spec)` chips out left to right with `chrome::chip(..., a.right(), ...)` and does `if w == 0 { break; }` once a chip no longer fits. Chips that do not fit are dropped silently: nothing wraps, nothing scrolls, there is no overflow marker, and the active value is not forced into v

### /theme, /sidebar, /statusline, /motion notices in the composer (1)

- **'Ctrl-Z undoes it' is promised outside Settings, but Ctrl-Z there does nothing** — defect, bug
  - expected: Ctrl-Z restores the previous theme, or the notice does not offer it
  - actual: The composer header reads 'Theme is now rose. Ctrl-Z undoes it.' After Ctrl-Z the notice just disappears. The theme stays rose and .pane/config.toml still holds theme = "rose". /sidebar hide ('Sidebar is now hide. Ctrl-Z undoes it.') and /statusline hidden show the same promise.
  - repro: `t.type('/theme'); t.key('enter'); t.key('up'); t.key('enter')  # rose; then t.key('ctrl-z'); read the screen and .pane/config.toml`
  - code: crates/sterna/src/workbench/input.rs:21 `Workbench::persist`. It opens a temporary `Preferences`, calls `p.save(...)`, copies `p.notice` into `self.notice`, and then drops `p`. That throws away `p.undo` and never sets `self.preferences`. The notice text comes from crates/sterna/src/workbench/settings.rs:212 (`"{} is now {}. Ctrl-Z undoes it."`), which is written for anything live or under `ui.*`. 

### /stream + stream chip (1)

- **The /stream usage names the wrong words, and its 'Ctrl-Z undoes it' does not work** — defect, copy
  - expected: The usage lists the accepted words (actions | code | raw), and Ctrl-Z reverts as the notice promises.
  - actual: Bare /stream posts 'Usage: /stream code | quiet | raw'. '/stream quiet' then says '/stream: unknown command' (the command exists; the argument is wrong). 'actions' is missing from the usage. '/stream code' says 'Streaming cell is now code. Ctrl-Z undoes it.', but after Ctrl-Z the chip still reads '⟨ stream code ⟩'. /stream is also absent from the slash list.
  - repro: `t.type('/stream'); t.key('enter'); then t.type('/stream quiet'); t.key('enter'); then t.type('/stream code'); t.key('enter'); t.key('ctrl-z')`
  - code: crates/sterna/src/workbench/input.rs:173-175 has the wrong usage string 'code | quiet | raw', where the accepted words are actions|code|raw per line 167. A '/stream <bad word>' matches no arm and falls through to the generic 'unknown command'; /motion has a catch-all arm ['/motion', ..] that /stream lacks. The false Ctrl-Z promise: persist() at input.rs:21-30 opens a temporary Preferences, calls p

### /theme picker (mouse) (1)

- **The mouse cannot preview a theme: one click applies it and closes the picker; hover does nothing** — defect, mouse
  - expected: Click (or hover) moves the preview, and a second click or Enter applies it, as ↑↓ + Enter do with keys. Settings keeps its sheet open after a chip click.
  - actual: The first click applies military and closes the sheet ('Theme is now military. Ctrl-Z undoes it.'). A mouse user never sees the sprite and swatch preview before committing. Hover produces no change anywhere, in the picker or in Settings. The notice uses the internal id 'military', not 'Military Macaw'.
  - repro: `/theme; x,y=t.find('Military Macaw'); t.click(x+2,y)`
  - code: crates/sterna/src/workbench/input.rs:994, the Action::PanelRow(i) arm: a click sets p.selected = i and then returns Effect::Command(panel_command(p)) straight away, so select and apply happen in one click. It should only select (preview) when i != p.selected and apply on a second click of the row that is already selected. The theme rows get Action::PanelRow in crates/sterna/src/workbench/sheets.rs

### /theme picker layout (1)

- **The PARROTS header is cut mid-sentence and the preview sprite runs into it; the sheet has no key legend** — polish, visual
  - expected: The full family line 'PARROTS · the bird's plumage, and the bird' with the preview beside it, plus a key hint like Settings has
  - actual: It reads 'PARROTS · the bird's plumage, and the' (at 80 cols 'and th'). The Sun Conure sprite's tail cells ('▀▀▀▀', '▀▀') are drawn on the header and first parrot rows. There is no ↑↓/Enter legend, only 'Esc closes · saved choices stay', and moving the cursor does not change the sheet's accent. The tails are a one-cell diagonal staircase that hangs below the perch.
  - repro: `/theme (140x42); t.key('down'); t.png('theme-picker-down')`
  - code: `crates/sterna/src/workbench/sheets.rs` `draw_themes` (lines 83-215):
- `let list = (inner.width / 2).clamp(24, 40);` sets the list width, and the heading row is drawn with width `list.saturating_sub(2)`, so at most 38 columns. `format!("{} · {}", label.to_uppercase(), blurb)` is 42 characters, so the text is clipped.
- The preview area starts at `inner.y + 2`, the same top row as the list, and th

### /theme on a terminal without true colour (1)

- **It promises an 'outline bird' that is never drawn, and still sends 24-bit colour codes** — defect, bug
  - expected: An outline bird appears (or the sentence is dropped), and colours use 256-colour codes
  - actual: The preview shows 'This terminal shows no true colour: the outline bird stands in.' with no bird of any kind. In one open-and-scroll there were 101 38;2;R;G;B sequences and 0 38;5 sequences. The swatches and accents are all 24-bit, which a 256-colour terminal misrenders.
  - repro: `Tui(colorterm=None); /theme; t.key('down',9); count re.findall(rb'38;2;', raw output)`
  - code: crates/sterna/src/workbench/sheets.rs, around lines 185-224 (the theme preview draw function). The sprite is drawn only when `s.truecolor` is set. When it is not, the code prints the fixed sentence "This terminal shows no true colour: the outline bird stands in." but draws nothing in the bird's place. The "outline bird" was the braille bird, which commit 8bbd7fe3 removed ("the parrot's head replac

### Sidebar · telemetry chip -> telemetry view (1)

- **The telemetry view has no mouse exit and leaves stray sidebar junction lines** — defect, mouse
  - expected: An '⟨ Esc · Back ⟩' chip like the other sheets have, a telemetry chip that toggles, and no leftover sidebar glyphs.
  - actual: Once the view is open the telemetry chip is gone, and clicking the header or hint does nothing. Only Esc or Ctrl-T returns. The rule and the dock border still show '┬' and '┴' at column 110 where the hidden sidebar was. The feature has three names: chip 'telemetry', hint 'Ctrl-T opens the instruments', header 'LIVE INSTRUMENTS'.
  - repro: `p=t.find('⟨ telemetry'); t.click(p[0]+3,p[1]); p=t.find('TELEMETRY'); t.click(p[0]+2,p[1]); p=t.find('Esc returns'); t.click(p[0]+2,p[1])`
  - code: crates/sterna/src/workbench/view.rs:589-598. When s.telemetry_open is set, the transcript area is overlaid with Clear. g.hits.clear() then drops every click target, which is why the header and hint cannot be clicked and why top-bar hits are probably lost too. After that, crate::tui::telemetry::expanded draws the view. But `gutter = sidebar.map(|side| side.x - 2)` (view.rs:379) is still computed fr

### Sidebar and chips on a light-background terminal (1)

- **Sidebar and chip colours have low contrast on a light terminal** — defect, visual
  - expected: At least 3:1 for labels and 4.5:1 for text.
  - actual: '◇ HELPERS' in #8be3ff on #F4F6F8 is about 1.3:1. Muted sidebar lines ('effort high · helpers off', 'network off') in #99a6b7 are about 2.3:1. Green chip brackets in #45b653 are about 2.4:1.
  - repro: `t.type('/effort high'); t.key('enter'); t.png('s41-light', light=True)`
  - code: In crates/sterna/src/workbench/theme.rs the role colours are fixed constants with no variant for a light background: MUTED = 0x99a6b7, HELPER = 0x8be3ff (also the accent of Theme::Ice), plus WARN, GREEN and YOU, which are all light pastels. accent() returns the bird's plumage accent, from crates/sterna/src/workbench/plumage.rs (AMAZON accent: 0x45b653). No code queries the background (OSC 11) or r

### Sidebar HELPERS vs Settings › Helpers (1)

- **Settings says Helpers On while the sidebar says helpers off, with no reason given** — defect, inconsistency
  - expected: One answer, or a reason on the row, e.g. 'On, but no helper model is chosen'.
  - actual: The '‹ Little helpers ›' category highlights ⟨ On ⟩ with the text 'Whether the little helpers run… · next session'. The sidebar and HELPERS block say 'off'. Helpers only count as on when enabled and a helper model is set, and nothing on screen says that.
  - repro: `t=Tui(fake=True); t.wait('What next',15); (sidebar shows 'helpers off', HELPERS 'off'); h=t.find('◇ HELPERS'); t.click(h[0]+2,h[1]+1)`
  - code: The sidebar and chips read `helpers_on`, computed at crates/sterna/src/session/controls.rs:201 as `config.helpers.enabled && config.helpers.model.is_some()`. It is shown at crates/sterna/src/workbench/view.rs:887 ("on"/"off") and :929 ("none yet"/"off"), and the chip is hidden when off at :1169. The Settings row is the plain `helpers.enabled` Bool at crates/sterna/src/settings/registry.rs:271-278,

### Settings sheet footer (1)

- **A long notice overprints the footer hint** — polish, visual
  - expected: The notice is clipped or wrapped before the hint.
  - actual: 'Subagents: roster · 1 configured favorites · next launch uses this assignment; in-flight jobs unchaEsc closes · saved choices stay'
  - repro: `real mode: /subagents quick claude-haiku-4-5-20251001; /subagents on; click '⟨ subagents' in the strip.`
  - code: In crates/sterna/src/workbench/view.rs around lines 1363-1390, the notice row is drawn across the full inner width (Rect::new(inner.x, y, inner.width, 1)). The hint ('Esc closes · saved choices stay', or 'Esc back · the assignment stays unchanged') is then drawn over the right end of that same row at inner.right() - hint.len() - 1. The notice's width is never reduced by hint.len()+gap, and it is n

### /help sheet (1)

- **/help is a dead end: selecting an entry and pressing Enter or clicking does nothing** — defect, mouse
  - expected: Clicking or Entering a command runs it or puts it in the composer. End jumps to the end of the list.
  - actual: The click only moves '›' to /diff, and Enter does nothing (the composer stays empty after Esc). End does nothing; only Down scrolls. Every info sheet (/help, /activity, /context, /status, /handlers, /budget) ends with 'Esc closes · saved choices stay', where there are no choices. They also show a '›' cursor on non-actionable text.
  - repro: `t.type('/help'); t.key('enter'); x,y=t.find('/diff'); t.click(x+2,y); t.key('enter'); t.key('end')`
  - code: crates/sterna/src/session.rs:2646 `offer_commands` builds the sheet with `tui::Panel::text("Commands", lines.join("\n"))`. `Panel::text` (crates/sterna/src/tui/controls.rs:175) turns every line into a `PanelRow { command: None }`, so no row can be run. The sheet still draws the selection cursor. The fix is `Panel::rows(...)` with `command: Some("/diff")` on each row, which is the selectable-row co

### slash commands (1)

- **/copy, /row, /open-link and /paste-callback say 'unknown command' when typed** — polish, loose-end
  - expected: Either they are real, documented commands, or they are not presented anywhere as commands.
  - actual: Each prints '· /<name>: unknown command'. They exist only as internal row actions (sign-in panel) in session/controls.rs. Typing '/co' in the slash menu offers /context and /config, with no /copy. There is no keyboard route to copy a selection or a cell's code; only mouse drag copies.
  - repro: `t.type('/copy'); t.key('enter'); t.type('/row'); t.key('enter'); t.type('/open-link'); t.key('enter'); t.type('/paste-callback'); t.key('enter')`
  - code: crates/sterna/src/session/controls.rs:1125-1142 and 1801-1808 build the row actions as pseudo-slash strings: format!("/open-link {link}"), format!("/copy {link}"), "/copy {code}" and "/paste-callback". crates/sterna/src/session/ui.rs:2159 builds format!("/row {i}"). crates/sterna/src/session/ui.rs:1205-1215 and 1637-1646 intercept these strings with strip_prefix only when a row is activated. The t

### sidebar (1)

- **Empty 'THIS SESSION' heading on the start screen, and a cell count that disagrees with the transcript** — polish, visual
  - expected: No empty section, and counts that match what is shown.
  - actual: 'THIS SESSION' has nothing under it until a sheet is closed. After the one-cell tour turn the sidebar says 'SO FAR 2 cells · 0 files · 104 tok' while the transcript shows only cell 001.
  - repro: `t=Tui(fake=True); t.wait('What next',15); print(t.screen())   # then the show-me-around turn from the permissions finding`
  - code: In crates/sterna/src/workbench/view.rs, around lines 832-905, the sidebar's line list works like this.

- 'THIS SESSION' is always pushed. The line under it is `match (id, s.pulse.elapsed_ms)`, and `(None, 0) => String::new()` gives the empty line under the heading. When `elapsed_ms` is non-zero after the turn, it prints '00:00 on this turn'.
- 'SO FAR' prints `n.cells.len()`. The ✓/✕ tally only c

### /fullscreen and Ctrl-F (1)

- **Fullscreen entered with Ctrl-F shows no way back** — defect, loose-end
  - expected: A hint such as 'Ctrl-F restores', and Esc leaves fullscreen
  - actual: All chrome and hints are gone, with no notice (only /fullscreen prints 'Ctrl-F or /fullscreen restores'). Esc does not restore, and there is nothing to click.
  - repro: `t.key('ctrl-f'); t.png('fullscreen'); t.key('esc')`
  - code: In crates/sterna/src/session/ui.rs around line 1751, the KeyCode::Char('f') arm flips state.fullscreen and then continues without setting any note. In crates/sterna/src/workbench/input.rs lines 192-199, the "/fullscreen" arm is the only place that calls s.note("Fullscreen. Ctrl-F or /fullscreen restores the chrome."). The Esc handling in workbench/input.rs around lines 388 and 405 only closes loca

### Top bar at 80 columns (1)

- **At 80 columns the mode chip is dropped, so Plan and Explore have no visible indicator** — defect, visual
  - expected: A Plan or Explore session keeps a visible marker, as warning-tone chips are kept.
  - actual: The top bar keeps '⟨ fixture-model ▾ ⟩ ⟨ Auto-review ⟩ ⟨ Settings ⟩' and drops Plan, This project and ?. The sidebar is hidden at this width. The only trace of Plan is a transcript line that scrolls away.
  - repro: `t=Tui(fake=True,cols=80,rows=24); t.wait('What next',15); t.type('/mode plan'); t.key('enter'); t.settle(6)`
  - code: crates/sterna/src/workbench/view.rs, session_bar() (~lines 240-265) and controls() (~line 164). controls() drops the chip with the highest rank first and never drops a chip in Tone::Warning. The mode chip is `(work_word(s), Action::Work, Tone::Normal, 3)`, which has rank 3 and is always Normal tone, even in Plan or Explore. The model chip has rank 2, so the mode chip is dropped before it. That con

### first start with no model (real) (1)

- **The wizard opens itself at start and swallows whatever the person types first** — defect, keyboard
  - expected: Typing a message at first start either reaches the composer or is refused visibly.
  - actual: The characters vanish. The wizard does not change, and after Esc the composer is empty: '❯ What next? A message or / for commands, @ for a file'.
  - repro: `t=Tui(fake=False,cols=80,rows=24); t.wait('Setup ·',25); t.type('hello there'); t.key('esc'); print([l for l in t.screen().split('\n') if '❯' in l])`
  - code: Two parts. (1) crates/sterna/src/session/setup.rs `at_start` (around line 435): when `no_model` is set it calls `controls::show(session, overview(&progress))` and opens the panel at start. The same function has a comment on its other branch that already names the problem: "one line, never a panel -- a panel at start would take the first keys the person types". (2) crates/sterna/src/workbench/input

### /wizard (first-run setup) (1)

- **Setup is saved per project, so the wizard comes back in every new project and writes .pane/ into the repo** — defect, bug
  - expected: Picks made in first-run setup are the person's own (global) settings. A second project opens ready to use, and the user's repository is left untouched.
  - actual: The models, helpers, agents and decisions go to <project>/.pane/config.toml, and only 'wizard.seen = 1' goes to the global file. The next project opens the wizard again at 'Setup · 1 of 3 done' with 'recommended picks ready'. The scratch repo now shows '?? .pane/' as uncommitted.
  - repro: `t=Tui(fake=False); home=t.home; t.wait('Setup ·',25); t.key('down'); t.key('enter'); x,y=t.find("Use Sterna's picks"); t.click(x+3,y); t.settle(3); t.close(); t2=Tui(fake=False, env={'XDG_CONFIG_HOME': home}); t2.wait('Setup',25); print(t2.screen())`
  - code: The cause is in crates/sterna/src/session/setup.rs. Four writes go to the project instead of the global file:
- apply_models (around line 391) calls controls::assign_model for each tier. assign_model in src/session/controls.rs (around line 251) reads and writes crate::settings::Scope::Local.
- apply_recommended (line 348) calls `controls::save_settings(session, crate::settings::Scope::Local, &edit

### opening screen (1)

- **The way back to setup disappears: no 'finish setup' chip after the auto-wizard, and any note wipes the chips** — defect, loose-end
  - expected: While setup is incomplete, the opening keeps offering '⟨ finish setup · N steps left ⟩', and a one-line note does not replace the opening.
  - actual: (a) The real first start with setup at 1 of 3 shows only '⟨ pick up: init ⟩ ⟨ review 1 uncommitted change ⟩', with no setup chip. (b) After Esc from the TypeSafe key form, the whole 'Where to?' block, including 'finish setup · 3 steps left', is replaced by '· no key entered'. The same happens after '/config show' (an ERROR note) and after a model change ('· model changed to gpt-6-sol'). Only a remembered /wizard gets back.
  - repro: `(a) t=Tui(fake=False); t.wait('Setup ·',25); t.key('esc'); print(t.screen())  (b) t=Tui(fake=True); t.wait('What next',15); t.type('/wizard jev'); t.key('enter'); t.key('down'); t.key('enter'); t.key('esc'); print(t.screen())`
  - code: Two causes. Both are in crates/pane.

(1) src/session/setup.rs, at_start() (around lines 435-470). When no_model is true it shows the overview panel and returns early. The ui.suggest("finish setup · N steps left", "/wizard") branch is reached only when a model is already chosen. So on a real first start with no model, Esc from the panel leaves no setup chip. The chip is also only suggested once at

### /settings category line (mouse) (1)

- **Clicking '‹' goes to the next category, not the previous one** — defect, mouse
  - expected: ‹ goes to the previous category (Advanced) and › to the next
  - actual: Clicking ‹ opens Display (the next one). Clicking anywhere on the line, including the 'Tab: next category' hint, also moves forward. With the mouse there is no way back to the previous category.
  - repro: `/settings; x,y=t.find('‹ Everyday ›'); t.click(x,y)`
  - code: In crates/sterna/src/workbench/view.rs around lines 1695-1706, the whole category line is drawn by a single add(...) call covering `‹ {cat} ›   Tab: next category · type to search`, with one hit action: Action::Category(next), where next = (p.category + 1) % CATEGORIES.len(). There is no separate hit region for '‹' pointing to (category + len - 1) % len. The fix is to split it into three regions: 

### Settings › Subagents › favourite picker (1)

- **'Use the inherited value' contradicts 'an empty slot never inherits Main' and leaves favourites on with zero favourites** — defect, copy
  - expected: The button says 'Empty this slot' (as /models does), and emptying the last favourite either turns favourites off or warns.
  - actual: The row description says 'an empty slot never inherits Main'. After the click the detail reads 'Not set · Sterna uses its own default · applies from the next session'. Subagent mode stays on roster with no favourites, a state `/subagents on` refuses. The timing copy also conflicts: this sheet says 'This session keeps what it started with; the next one takes it', while /models says 'next launch uses this assignment; in-flight jobs unchanged'.
  - repro: `real mode: /subagents quick claude-haiku-4-5-20251001; /subagents on; click the strip chip '⟨ subagents'; click 'Quick favorite'; click 'Use the inherited value'.`
  - code: crates/sterna/src/workbench/sheets.rs around lines 493-500: when m.target_key.is_some() (the picker was opened from a Settings row), the off-button is always ("Use the inherited value", Action::UnsetModel). It never takes the role==2 && slot branch, which gives "Empty this slot" -> '/subagents <slot> off'. UnsetModel only clears the key and never re-checks that roster mode still has a favourite. /

### answer text (1)

- **Markdown in answers is not rendered** — defect, visual
  - expected: Bold, links, inline code and fenced code are rendered, with no raw syntax.
  - actual: Raw '**a.txt**', '[docs](https://example.com)', '```python' / '```', and backticks around every inline code span (`src/main.py`, `add`). At 80 columns the wrap splits an inline code span ('prints `' / 'add(2, 3)`').
  - repro: `t.model.script=[W1, 'Wrote **a.txt**. See the [docs](https://example.com) and:\n\n- one\n- two\n\nʼʼʼpython\nprint(1)\nʼʼʼ']; t.type('now write a'); t.key('enter'); t.wait('complete')`
  - code: The live workbench view never calls the Markdown renderer that already exists. crates/sterna/src/workbench/document.rs `fn answer` (around lines 676-707) sends the answer's first line and the rest to `self.wrapped(...)`. `wrapped` (lines 118-158) splits the text on '\n' and hard-wraps it character by character, with no Markdown parsing and no word boundaries. That is why the `add(2, 3)` span is cu

### wizard sub-steps (1)

- **Wizard sub-steps are built differently: no cursor, uneven gaps, missing Back, focus lost on return** — defect, inconsistency
  - expected: Each step opens with its first action selected, uses the same spacing, and has Back. Returning to the overview keeps the step you came from selected.
  - actual: Jev opens with the cursor on the description (nothing highlighted, Enter does nothing), with two blank rows between 'Add a TypeSafe API key' and 'Back'. The not-signed-in Models step has only 'Sign in first…' and no Back row. The 'Back' row returns to the overview with step 1 selected instead of the step just left. Clicking a step's second (description) line does nothing, and Home/End do nothing.
  - repro: `t=Tui(fake=True); t.wait('What next',15); t.type('/wizard jev'); t.key('enter'); t.png('x'); t.key('enter'); t.key('esc'); t.type('/wizard models'); t.key('enter')`
  - code: In crates/sterna/src/session/setup.rs:
- `jev_panel` (line ~280) puts the description in as row 0 as a `PanelRow` with `command: None`. `Panel::rows` in crates/sterna/src/tui/controls.rs:198 always sets `selected: 0`, so the cursor lands on a row that cannot be chosen.
- `models_panel` (line ~247) returns early when `recommend()` is None, building a one-row panel with no 'Back' row. The signed-in 

### /login <subscription> warning sheet (1)

- **The risk warning a person must read before 'Sign in anyway' is cut off after one line** — defect, visual
  - expected: The whole warning wraps and can be read, including the allowed alternative (an API key), before the dangerous 'Sign in anyway' row, which is one ↓ below.
  - actual: The warning is one clipped row. Claude's stops at '...has blocked and suspended accounts for it. Sig'. Gemini's stops at '...with a permanent ban on'. At 80 cols Kimi's reads only 'Kimi allows its Kimi Code membership in third-party tools for personal u'. The sentence that names the allowed route is never visible. The warning is also drawn in the green selection accent, so it reads as a positive line.
  - repro: `t=Tui(fake=False,cols=140,rows=42); t.wait('Setup ·',25); t.key('esc'); t.type('/login claude'); t.key('enter'); t.settle(2); print(t.screen())   # also '/login gemini', and Tui(fake=False,cols=80,rows=24) with '/login kimi'`
  - code: In crates/sterna/src/session/controls.rs, `warning_panel` (about line 483) builds the warning with `Panel::rows` as an ordinary one-line `tui::PanelRow { text: format!("⚠ {warning}"), command: None }`. That row is cut at the panel width instead of wrapping. It is also row 0, so the cursor starts on it and it takes the selection accent. The warning texts are at lines 430 and 439. The fix is to rend

### /wizard › Models › Use Sterna's picks (1)

- **After 'Use Sterna's picks' the session still says no model is chosen** — defect, bug
  - expected: The header chip, the sidebar MODEL and /status show gpt-6-sol right away, as they do when a model is picked by hand. By hand, the header immediately reads ⟨ gpt-6-sol ▾ ⟩.
  - actual: The wizard shows '✓ 2 … main gpt-6-sol' and Settings shows 'Main model gpt-6-sol · applies now', but the header still reads '⟨ choose model ▾ ⟩', the sidebar MODEL reads 'choose model', and /status prints 'Model:' with nothing after it (and 'Mode: execute' where the sidebar says 'mode build').
  - repro: `t=Tui(fake=False); t.wait('Setup ·',25); t.key('down'); t.key('enter'); x,y=t.find("Use Sterna's picks"); t.click(x+3,y); t.settle(3); t.key('esc'); t.settle(4); print(t.screen().split('\n')[0]); t.type('/status'); t.key('enter')`
  - code: In crates/sterna/src/session/setup.rs, `apply_models` (around line 392) calls `controls::assign_model(session, Tier::Parent, &picks.main)`. That call saves settings and updates `live.model.parent` in crates/sterna/src/session/controls.rs:230, but it never sets the live request model. The hand-picked path in crates/sterna/src/session.rs (around lines 2577-2587) does both extra steps after assign_mo

### /wizard › Models (1)

- **'Use Sterna's picks' also turns on Jev decisions and helpers without saying so** — defect, bug
  - expected: The row reads 'Use Sterna's picks · main gpt-6-sol · helpers gpt-6-luna · subagents gpt-6-sol', so clicking it sets those three models and nothing else. Step 3 (Jev) stays the person's own choice.
  - actual: The overview jumps from '1 of 3' to 'Setup · 3 of 3 done' and step 3 reads '✓ 3 Jev, the decision model · on'. decisions.mode = "on" and helpers.enabled = true are written to config. Nothing on the Models step mentions either change.
  - repro: `t=Tui(fake=False); t.wait('Setup ·',25); t.key('down'); t.key('enter'); x,y=t.find("Use Sterna's picks"); t.click(x+3,y); t.settle(3); print(t.screen())`
  - code: crates/sterna/src/session/setup.rs, apply_models (around lines 393-410). It assigns the three tiers, then under the comment "// Sterna's picks come with Sterna's settings." it calls apply_recommended(session, progress(session).jev_key). apply_recommended (line 342) writes every RECOMMENDED entry from changes_since(0, ...) to Scope::Local: helpers.enabled=true (line 54) and decisions.mode=on (line 

### key form, custom endpoint form (1)

- **The sign-in forms ignore the mouse entirely** — defect, mouse
  - expected: Clicking a field focuses it, clicking 'Anthropic Messages' selects it, and clicking 'Esc · back' leaves, as chips do everywhere else ('click: any chip changes the thing it names').
  - actual: None of the clicks changes anything, and neither does the wheel. The only way through the form is Tab/↓/←→/Esc.
  - repro: `t=Tui(fake=False); t.wait('Setup ·',25); t.key('esc'); t.type('/login custom'); t.key('enter'); t.settle(2); x,y=t.find('Anthropic Messages'); t.click(x+2,y); x,y=t.find('BASE URL'); t.click(x+2,y+1); x,y=t.find('Esc · back'); t.click(x+2,y)`
  - code: crates/sterna/src/session/ui.rs: the Event::Mouse branch (around line 1249) handles only the settings editor, approvals scrolling and wheel scrolling of the inspection or scrollback. It never checks state.form, so clicks on a form are dropped. A wheel event while a form is open falls through to the scrollback behind it. All form input is keyboard-only, in the Event::Key branch around lines 1437-14

### /config (Advanced) effort rows (1)

- **Favourite effort chips look live but clicking or ←→ does nothing until a model is set** — defect, bug
  - expected: The chip is selected and saved, or the row is visibly disabled with 'choose the Quick favorite model first' next to it.
  - actual: All five chips are drawn the same as active ones. Clicking 'high' leaves 'Not set · Sterna uses its own default' and writes nothing. ←→ (advertised as '←→ Change') do nothing. A small line at the bottom of the sheet says 'settings: a configured favorite needs a concrete model'. No chip is highlighted for an unset effort, so the default in use is invisible.
  - repro: `t=Tui(fake=True); t.wait('What next',15); t.type('/config'); t.key('enter'); # click the '⟨ high ⟩' on the 'Quick effort' row twice, then t.key('right'); t.key('left')`
  - code: crates/sterna/src/settings/registry.rs, lines ~297-360: the four settings `agents.slots.{quick,balanced,deep,heavy}.effort` are plain `Kind::Choice` with HARD_EFFORT choices. Nothing ties them to the sibling `.model` row, so they render as always enabled.

crates/sterna/src/config/agents.rs, lines ~142-165: when the settings writer re-validates the `[agents.slots.<name>]` table, it rejects any tab

### /key and the key form (1)

- **A failed key save throws the form away and sends the person to a shell** — defect, bug
  - expected: The form stays open with the error under the field and a way to retry, as /login custom does ('with_error').
  - actual: The form closes, the key is lost, and a text panel says 'The gateway did not store the key; run `inference-gateway credentials set anthropic` in a shell to see why.' Its only row is that sentence, and it cannot be acted on.
  - repro: `t=Tui(fake=True); t.wait('What next',15); t.type('/key anthropic'); t.key('enter'); t.type('not-a-real-key-000'); t.key('enter'); t.settle(3); print(t.screen())`
  - code: crates/sterna/src/session/controls.rs, around lines 895-920 (the /key handler). It calls fill(session, key_form(provider)) once, then crate::gateway::store_credential(...). On None it calls show(Panel::text("API key", "The gateway did not store the key; run `inference-gateway credentials set {provider}`...")) and returns, instead of looping with key_form(provider).with_error(0, ...) the way the cu

### /key with no provider (1)

- **/key alone is a dead-end sentence instead of the provider list** — defect, loose-end
  - expected: /key opens the same provider list as /login key, and the person picks one.
  - actual: The panel's only (selectable, inert) row reads '/key <provider> takes an API key for one provider -- `/key anthropic`. /login lists the providers this gateway knows.' It uses literal backticks and '--', and ↓/Enter do nothing. Typing '/key ' offers no provider completions.
  - repro: `t=Tui(fake=True); t.wait('What next',15); t.type('/key'); t.key('enter'); t.key('down'); t.key('enter')`
  - code: crates/sterna/src/session/controls.rs:883-893, fn key(): when provider is None it calls show(Panel::text("API key", "/key <provider> takes an API key ... -- `/key anthropic`. /login lists ...")) and returns. The fix is to show key_panel(&api_keys(session)) instead (key_panel is at line 866, key_rows at 710 and already emits `/key <provider>` commands). The composer offers no argument completion fo

### /login key (API key list) (1)

- **37 unsorted provider ids with no search and no sign that the list continues** — defect, visual
  - expected: Providers are readable names in a sensible order (the panel promises 'Anthropic, OpenAI, OpenRouter …'), typing filters them, and a cue shows more rows below.
  - actual: The list order is 'anyrouter, experiential, anthropic, openrouter, unorouter, zai, …', with openai 13th. Every row repeats '· API key ·'. Typing is ignored. venice, chutes and gemini-openai only appear after scrolling past the bottom with no indicator. Home/End do nothing. Two rows say 'a Keychain item exists that this build may not read; enter it again'.
  - repro: `t=Tui(fake=False); t.wait('Setup ·',25); t.key('esc'); t.type('/login key'); t.key('enter'); t.settle(2); t.type('open'); t.key('down',40); print(t.screen())`
  - code: crates/sterna/src/session/controls.rs key_panel() (~line 865) passes key_rows(keys) straight to Panel::rows("Sign in › API key", rows). Rows follow the gateway CredentialRow order and are formatted '{provider} · API key · {state}' with no sorting and no display names. The Keychain text is at controls.rs:719. Panel::rows / the generic panel in crates/sterna/src/tui/controls.rs:198 has no filter, Ho

### key form copy (1)

- **Key form claims contradict each other and the check approves anything** — polish, copy
  - expected: Accurate, specific copy.
  - actual: Typed text gets a green '✓ pasted' ('abc123' passes as a key). The intro says the key 'is never shown' while the hint says 'Ctrl-R shows it', and Ctrl-R does reveal 'abc123'. The copy uses the raw id ('Paste your anthropic key', 'typesafe'). Reached from Setup › Jev, the breadcrumb says 'Sign in › API key · typesafe'. The footer lists 'paste here' as if it were a key.
  - repro: `t=Tui(fake=False); t.wait('Setup ·',25); t.key('esc'); t.type('/key anthropic'); t.key('enter'); t.type('abc123'); t.key('ctrl-r')   # also fake: /wizard jev -> Add a TypeSafe API key`
  - code: - crates/sterna/src/session/controls.rs:609-630 (`fn key_form`): the 'Sign in › API key · {provider}' title for every entry point, the '{provider}' raw id, the 'never shown' intro, and the 'Ctrl-R shows it' hint.
- crates/sterna/src/tui/form.rs:255-275 (`key_shape`): any string without whitespace that matches no known prefix returns Ok("pasted"). The form renders that as a green ✓, even for typed 

### composer footer / Settings header (1)

- **'Everything there applies right now' is contradicted by rows that apply next session** — polish, copy
  - expected: The promise matches the rows.
  - actual: The footer says 'F2 opens settings · everything there applies right now' and the sheet header says 'every choice applies now and saves itself', but Helpers, every favourite and every helper effort say '· next session' / 'applies from the next session'. Other footer hints are cryptic: 'Ctrl-T opens the instruments', 'ctx 12 · reported'.
  - repro: `t=Tui(fake=True); t.wait('What next',15); print(t.screen().split('\n')[-1]); t.key('f2'); print(t.screen())`
  - code: The first two files are under crates/sterna/src/workbench/:
- voice.rs:210 (PLAYFUL hint "F2 opens settings · everything there applies right now") and the PLAIN twin at about line 220 ("every choice there applies to this session now").
- view.rs:1285, the Settings header "every choice applies now and saves itself; there is no Apply".
Both are blanket claims. The per-row truth comes from crate::set

### /config with arguments (1)

- **/config arguments answer with CLI help (--help, raw keys, truncated usage)** — defect, copy
  - expected: In the TUI, an unknown argument opens the settings sheet or names valid words, with no CLI flags.
  - actual: '✕ ERROR: unknown setting `show`; use --help to list supported keys' replaces the opening. '/config --help' dumps 'Usage: config [global|local|project] [<key> [<value>...]] …' with each description truncated at the panel edge.
  - repro: `t=Tui(fake=True); t.wait('What next',15); t.type('/config show'); t.key('enter'); t.type('/config --help'); t.key('enter')`
  - code: The '"config"' arm of the slash-command match in crates/sterna/src/session/controls.rs (around line 1382) sends the slash arguments unchanged to crate::settings_commands::execute. That is the CLI path, in crates/sterna/src/settings_commands.rs.
- On success it shows the CLI output in Panel::text("Sterna configuration", ...), and that output includes help() from line 189, which is the Usage block a

### gateway-unreachable Sign in panel (fake) (1)

- **'Try again' gives no sign that it tried** — defect, bug
  - expected: A visible retry result ('still not answering · 22:31').
  - actual: The panel is identical after Enter. The copy also shows literal backticks ('Why: `no-gateway` is not installed').
  - repro: `t=Tui(fake=True); t.wait('What next',15); t.type('/login'); t.key('enter'); s=t.screen(); t.key('enter'); t.settle(2); print(s==t.screen())`
  - code: `unreachable_panel()` in crates/sterna/src/session/controls.rs (lines 137-172) builds the rows. The last row is "Try again", and its command is the same `/login` (or `/model`) that opened the sheet; the callers are at lines 320 and 99. Running it again calls `why_unreachable()` again, gets the same answer and draws an identical panel, because the panel keeps no retry count, time or "still not answ

### /model command (1)

- **`/model helper` and `/model subagent` set the MAIN model to a model literally called 'helper'/'subagent'; any string is accepted** — defect, bug
  - expected: `/model helper` without a name opens the picker on the Helper tab (or explains usage); an unknown model name is refused against the catalogue.
  - actual: Top chip becomes '⟨ helper ▾ ⟩', sidebar MODEL shows 'helper', transcript '· model changed to helper'. `/model subagent` gives '· model changed to subagent'. `/model nonsense-model-xyz` gives chip '⟨ nonsense-model-xyz ▾ ⟩'. Reopening /models shows 'Main answers you. Now: helper'.
  - repro: `t=Tui(fake=False); Esc the Setup sheet; t.type('/model claude-opus-5'); t.key('enter'); t.type('/model helper'); t.key('enter'). Also t.type('/model nonsense-model-xyz'); t.key('enter').`
  - code: The `/model` branch of `answer_command` in crates/sterna/src/session.rs (around lines 2539-2590) only treats the first word as a tier when whitespace follows it (`argument.split_once(char::is_whitespace)`). So a bare `helper` or `subagent` falls through to `None => (Tier::Parent, argument)` and becomes the main model. `startup::settle_model` (crates/sterna/src/session/startup.rs:342) returns the u

### /models › Subagents tab (PINNED card) (1)

- **Choosing a PINNED model silently switches favourites off, and the PINNED card shows 'favorite roster' as its model** — defect, bug
  - expected: The PINNED card shows the pinned model or 'none'. Pinning while favourites are on asks first, or at least says 'favourites are now off'.
  - actual: The PINNED card body and the Now line read 'favorite roster'. After pinning, the strip changes to '⟨ subagents pinned ⟩'. The only message is '· subagent model set to claude-sonnet-5', and the configured DEEP favourite is silently unused. I did this by accident: two wheel ticks over the slot row moved the highlight, and Enter pinned claude-opus-4-20250514 over the favourites.
  - repro: `real mode: /subagents deep claude-opus-5 high; /subagents on; /models, Tab, Tab (PINNED is selected) → the card reads 'favorite roster'. Then t.type('/model subagent claude-sonnet-5') or press Enter on any row while PINNED is selected.`
  - code: 1. Card text: crates/sterna/src/session/controls.rs tier_models() maps AgentsMode::Roster => Some("favorite roster"). crates/sterna/src/workbench/sheets.rs (around line 294 for the 'Now:' line, and around 327 for the PINNED card) renders that value directly (`None => m.current.subagent.clone()...`). The PINNED card should read config.agents.model or 'none'.
2. Silent switch: controls.rs assign_mod

### status-strip subagents chip → Settings › Subagents (1)

- **A second, differently navigated UI for the same subagent assignment, with different vocabulary** — defect, inconsistency
  - expected: One place to assign subagent models: the chip opens /models on the Subagents tab, with the same words and keys.
  - actual: It opens SETTINGS (Project scope) › Subagents: 'Subagent mode ⟨ off ⟩ ⟨ pinned ⟩ ⟨ roster ⟩', 'Subagent model', 'Quick favorite'…'Heavy favorite'. The keys are '↑↓ Select · ←→ Change · Enter Edit · Backspace Inherit'. In /models the same data is 'PINNED/QUICK…/Favourites', with Tab for tiers, ←→ for slots and 'Enter · put in QUICK'. Backspace means 'inherit/unset' here and 'delete a letter' in the picker. Enter on a favourite opens a third, restricted picker showing only '⟨ Subagents ⟩' and 'Enter · save to these settings'. Descriptions leak internals: 'Provider/account routing remains gateway-owned.'
  - repro: `real mode: /subagents quick claude-haiku-4-5-20251001; /subagents on; p=t.find('⟨ subagents'); t.click(p[0]+3,p[1]). Clicking 'subagents off' in the sidebar also opens it.`
  - code: In crates/sterna/src/workbench/view.rs, the sidebar row (around lines 893-897) and the status-strip chip (around lines 1176-1182) both bind to Action::SettingsAt(4), which opens the Settings sheet at the Subagents category, instead of an action that opens the /models sheet on its Subagents role. That action is handled in src/workbench/input.rs:750, the SettingsAt branch. The Settings rows and thei

### /models (1)

- **'All accounts' (Ctrl-A) and 'by intelligence' (Ctrl-O) have no clickable control, and the sort state is never shown** — defect, mouse
  - expected: Visible toggles (chips) for account scope and ordering that work by click and show their state.
  - actual: The clicks change nothing: the count stays '30 of 474 · connected accounts'. After Ctrl-O the list is re-sorted, but the hint line still reads 'Ctrl-O by intelligence' and nothing says the order changed.
  - repro: `real mode: /models; click the text '30 of 474 · connected accounts'; click the hint 'Ctrl-A all accounts'; click the hint 'Ctrl-O by intelligence'; then t.key('ctrl-o').`
  - code: crates/sterna/src/workbench/sheets.rs, around lines 364-392: the count ('N of M · connected/all accounts') is drawn with plain row() and no action. Around lines 530-535, the hint is a fixed label() string with no hit region and no state. crates/sterna/src/workbench/input.rs lines 962-973 handle Action::Sources and Action::Scores, which toggle all_sources and measured_order. A grep finds nothing th

### /models layout (1)

- **The list is cut off with no 'more below' cue; a group header is orphaned; at 80x24 the Subagents tab shows 2 models** — defect, visual
  - expected: A scroll cue or count ('7 more'), no header without rows, and a usable list at 80x24 (collapse the slot cards, drop the blank spacer lines).
  - actual: As described. The detail line 'claude-3-5-haiku-20241022 · via claude · …' sits directly under the last row and reads like another list row.
  - repro: `real mode at 140x42: /models → 23 of the 30 rows show, and gpt-6-* and gpt-image-* are hidden with no indicator. Tab, Tab → 17 rows, and the last line is the header 'OPENAI · CHATGPT-SUBSCRIPTION · ACCOUNT-DECLARED' with no rows under it. Tui(cols=80, rows=24) → the Subagents tab shows 2 model rows and the hint is cut to 'Ctrl-O by intell'.`
  - code: crates/sterna/src/workbench/sheets.rs, the model picker render at about lines 398-431. The code puts header lines (None) and model rows into one `lines` vec. It then takes `capacity = bottom - y` lines from `start = at - (capacity-1)` and draws them.
- There is no 'N more' indicator above or below the window.
- Nothing stops the last visible line from being a header (None), which leaves the header

### /models locked rows (1)

- **Locked rows look selectable: not dimmed, name overflows into 'locked', Enter chip stays active, reason shown twice with no way to act** — defect, visual
  - expected: Locked rows are dimmed and the Enter chip is disabled. The reason appears once, with a clickable 'Sign in to Groq' action. Long names are clipped before the status column.
  - actual: Locked rows are drawn in the same bright text as available ones, and 'locked' repeats on 444 rows. The selected locked row is accent-green with '⟨ Enter · use for Main ⟩' still highlighted. After Enter the line 'No credential for this provider; configure it in /login first.' appears twice (detail line plus notice) and cannot be clicked. Names collide with the column: 'mistralai/mistral-small-24b-instruct-2501locked'. In Ctrl-O mode the route column is ragged between scored and unscored rows.
  - repro: `real mode: /models; t.key('ctrl-a'); t.key('pgdn',3); t.key('enter'). Also t.type('instruct') with Ctrl-A on.`
  - code: Row and chip code: crates/sterna/src/workbench/sheets.rs, around lines 400-490.
- Rows are built with format!("{:<34}{lock}{score}{via}", c.model). The model name is neither clipped nor followed by a separator, so names of 34 or more characters run into 'locked' and push the route column out of line.
- Every row, locked or not, goes through add(..., Action::Model(i), selected, theme) with no dimme

### /models search (1)

- **Search is unranked: 'sonnet 5' puts claude-3-7-sonnet-20250219 first and Enter picks it** — defect, bug
  - expected: The exact or closest match (claude-sonnet-5) comes first.
  - actual: 4 matches in alphabetical order: claude-3-7-sonnet-20250219 is highlighted (the '5' matched its date), and claude-sonnet-5 is last. With 0 matches the '⟨ Enter · use for Main ⟩' chip still looks active, and the count reads '0 of 474' although only 30 are in scope.
  - repro: `real mode: /models; t.type('sonnet 5').`
  - code: crates/sterna/src/workbench/models.rs, ModelsSheet::candidates() (about lines 79-134). The filter is a plain substring AND over "id provider account scope" (hay.contains(t)) and gives no relevance score. The sort uses only subscription vs account-declared, then route, then model id alphabetically (or the intelligence score under Ctrl-O), so the query never affects order. catalogue_len() (line 76) 

### /models slot cards (1)

- **Only the slot title is clickable; the card body ('empty' or the model name) ignores clicks; no hover feedback** — defect, mouse
  - expected: The whole card is one target, and hovering shows it is clickable.
  - actual: The selection stays on PINNED. Clicking the title 'DEEP' works. No row or card reacts to hover.
  - repro: `real mode: /models, Tab, Tab; x,y=t.find('empty',3); t.click(x+1,y) (the body of the HEAVY card).`
  - code: crates/sterna/src/workbench/sheets.rs, around lines 300-338 (the m.role == 2 slot cards). Each card's title is drawn with button(..., Rect::new(x, y, width-1, 1), Action::Slot(..)). That hit rect is one row tall and covers only the title line. The body ('empty' / 'model · effort' / 'off') is drawn with row(...) at y+1, which registers no hit target. Growing the button rect to 2 rows (or registerin

### Settings (opened from the Settings chip) vs live chips (1)

- **Settings shows stale values after the chips, Shift-Tab or /mode change the session, while its header says every choice applies now** — defect, inconsistency
  - expected: Reasoning effort, Working mode and Permission rung in Settings show what the session is running now: medium, plan, manual/Every call.
  - actual: The sidebar says 'effort medium', 'mode plan' and 'asks before every call'. Settings highlights ⟨ default ⟩, ⟨ build ⟩ and ⟨ auto ⟩, under the header 'every choice applies now and saves itself; there is no Apply'. On the same screen, the mode notice runs into the footer: 'applies from the nextEsc closes · saved choices stay'.
  - repro: `p=t.find('effort default'); t.click(p[0]+2,p[1]); p=t.find('⟨ effort'); t.click(p[0]+3,p[1]); t.key('backtab'); t.type('/mode plan'); t.key('enter'); p=t.find('⟨ Settings'); t.click(p[0]+3,p[1])`
  - code: In crates/sterna/src/workbench/settings.rs:138, `SettingsPanel::effective(key)` reads only `self.loaded.values` (the config layers). When a key is unset there, it falls back to `crate::settings::shown_default`. It never asks the live session for effort, mode or rung, so changes made through the chips, Shift-Tab or /mode never reach the sheet.

The footer overlap is in crates/sterna/src/workbench/v

### Top bar · Build chip / /mode auto (1)

- **'/mode auto' sets a mode that no chip, sheet or /status shows** — defect, inconsistency
  - expected: The chip and sidebar show that the mode is automatic (e.g. 'Build · auto'), and the WORK sheet offers that choice.
  - actual: The notice says 'Mode: execute · unpinned; a confident read-only request may propose explore'. After that, the chip says 'Build', the sidebar says 'mode build', the WORK sheet marks '▸ Build · now', and /status says 'Mode: execute'. All of these are identical to a pinned Build.
  - repro: `t.type('/mode auto'); t.key('enter'); p=t.find('⟨ Build'); t.click(p[0]+3,p[1]); t.key('esc'); t.type('/status'); t.key('enter')`
  - code: In crates/sterna/src/session/controls.rs, around lines 1277-1295, the "mode" "auto" branch only calls session.mode_pinned.set(false) and prints a line. Unlike the explicit-mode branch, it never calls session.ui.mode(...) or any other UI update. The workbench ScreenState does not carry a pinned flag: crates/sterna/src/workbench/view.rs:90 work_word() maps s.mode only to Build/Explore/Plan, and the 

### /status (1)

- **/status leaves out most chip values, and its cursor rows do nothing** — defect, inconsistency
  - expected: The same facts the chips and sidebar show: effort high, Every call, helpers, subagents, stream.
  - actual: The sheet 'Session configuration' lists Model, Mode, Project, 'Sandbox: … network false', 'Permissions: native global/project config (loaded at startup)' and so on. It has no effort, no rung and no stream value. 'network false' contradicts the sidebar's 'network off'. The rows carry a '›' cursor that moves on click, but Enter and click do nothing else.
  - repro: `t.type('/effort high'); t.key('enter'); t.type('/mode plan'); t.key('enter'); t.key('backtab'); t.type('/stream code'); t.key('enter'); t.type('/status'); t.key('enter'); then t.key('enter') and click a row`
  - code: crates/sterna/src/session/controls.rs around line 1410, the "status" arm. It builds the sheet as a static Panel::text("Session configuration", format!(...)) from model, mode, project root, profile.rule_count/command_pattern_count/grants_network() (a bool formatted with {}, which prints 'network false'), web, limits, supervisor and config().helpers.effort.{find,reduce,check}. It never reads the ses

### ASK sheet · Never asks confirmation (1)

- **The Never-asks choice has three different confirmation flows, and the confirm page's heading is confusing** — defect, inconsistency
  - expected: One confirmation step with clear wording, used on every path.
  - actual: A: the CONFIRM page's heading says 'this one is not undone by Esc', yet Esc cancels it. Its '⟨ Confirm · Never ask ⟩' button is pre-focused, so Down×3 then Enter, Enter lands on Never asks. B: Settings shows the raw key 'permissions.mode = full · Enter confirms this field · Esc cancels field only', under a header that says 'there is no Apply'. C: no confirmation at all, then a full-screen 'Permissions' sheet with the single line 'permissions: auto → full (Shift-Tab cycles; 3 of the four rungs ask)'. The sidebar then reads 'asks before nothing'.
  - repro: `A) p=t.find('Auto-review'); click; p=t.find('Never asks'); click; t.key('enter').  B) Settings chip -> click '⟨ full ⟩' on the Permission rung row.  C) t.type('/permissions full'); t.key('enter')`
  - code: Three separate implementations of the same change:
- crates/sterna/src/workbench/view.rs:1300 hard-codes the heading ("CONFIRM", "this one is not undone by Esc") for ui.confirm; the button label is at view.rs:1600.
- Settings uses the generic field-edit footer at view.rs:1872 ("Enter confirms this field · Esc cancels field only") and prints the raw key permissions.mode.
- /permissions <rung> in cr

### Sidebar effort line with an OpenAI-dialect model (1)

- **With gpt-5.5 the first click on 'effort low' seems to do nothing** — defect, inconsistency
  - expected: The sidebar and strip agree on what 'default' means, and the click changes something visible.
  - actual: At start the sidebar reads 'effort low' (default is sent as low for GPT), and the strip shows no effort chip. After the click the sidebar still reads 'effort low'. A '⟨ effort low ⟩' chip appears with 'undo effort default', so the same word now names two different settings.
  - repro: `Run the session with --model gpt-5.5 (wrapper gptwrap.sh in the audit dir); t.wait('effort low',30); p=t.find('effort low'); t.click(p[0]+2,p[1])`
  - code: crates/sterna/src/workbench/view.rs:~885 renders the sidebar line with `s.effort.sent_for(model).name()`. Effort::sent_for in crates/sterna/src/wire.rs:60 maps Default to Low for OpenAI-dialect models, so the sidebar shows the value on the wire. The click handler, Action::Effort in crates/sterna/src/workbench/input.rs:761, steps the ladder ["default","low",...] from the stored `s.effort.name()`, s

### /settings Global scope (1)

- **Editing Global while the project overrides it announces a change that never happens and names the wrong origin** — defect, bug
  - expected: The row shows the Global value (mint) and says the project overrides it with rose, or it says the change has no effect here. Right steps from mint to the next theme.
  - actual: The notice says 'Theme is now mint. Ctrl-Z undoes it.' The rose chip stays lit, the screen stays rose, and the detail line says 'rose · set in your global settings', which is false because rose comes from the project file. Pressing Right then saves global theme = "amazon" (the step after rose, not after mint).
  - repro: `/settings; click '⟨ rose ⟩' (project); F6 (Global); select the Theme row; click '⟨ mint ⟩'; then press Right`
  - code: The chips, the detail line and cycling all read the merged effective value, but saving writes to the selected scope.

- crates/sterna/src/workbench/settings.rs `effective()` (about line 137) reads `self.loaded.values`, the merged project-over-global value. `cycle()` (about line 235) takes the old index from `effective()` instead of the scope's saved value (`self.saved()`, from `self.snapshot`), so

### /settings Display: Motion vs Reduced motion (1)

- **Two controls set motion, and the legacy one silently wins while the Motion row reports a different value** — defect, inconsistency
  - expected: One motion control. If both must exist, Motion shows 'off' while Reduced motion is on.
  - actual: After 'Reduced motion is now true' the Motion row says 'full · from Sterna's own default · applies now'. Setting Motion to calm says 'Motion is now calm', but the file holds reduced_motion = true, motion = "calm", and presentation() forces Off whenever reduced_motion is true. The row claims calm while nothing moves.
  - repro: `/settings; Tab (Display); down x6 (Reduced motion); Right; up x3 (Motion); Right`
  - code: The override is in crates/sterna/src/settings_session.rs lines 34-44 (the presentation code). It reads `ui.reduced_motion` first and sets Motion::Off when it is true, ignoring `ui.motion`. The comment there says "`ui.reduced_motion` predates the level".

The settings sheet lists both keys as separate basic rows in crates/sterna/src/settings/registry.rs: `ui.motion` at about line 398 and `ui.reduce

### /permissions sheet vs /settings (1)

- **A rung picked on the ASK sheet is not saved, though the sheet footer says 'saved choices stay'** — defect, inconsistency
  - expected: The ASK sheet and the Settings row write to the same place, or the sheet says 'this session only'
  - actual: The new session's top bar shows '⟨ Auto-review ⟩' and no .pane/config.toml was written. The same change in /settings is written to .pane/config.toml. The ASK and CONFIRM sheets both end with 'Esc closes · saved choices stay'.
  - repro: `/permissions; click 'Commands'; close; start a new Tui(env={'XDG_CONFIG_HOME':home}, cwd=cwd)`
  - code: crates/sterna/src/workbench/input.rs:982 (Action::Rung only calls s.permissions.set and never saves permissions.mode). The misleading footer is at crates/sterna/src/workbench/view.rs:1375-1379, where the 'saved choices stay' hint is shared by the ASK, WORK and CONFIRM sheets.

### /settings Permission rung row (1)

- **Rung changes become a raw TOML field edit, and 'full' skips the confirm the ASK sheet requires** — defect, inconsistency
  - expected: A chip click or arrow applies like every other row. Choosing full gets the same 'Never ask' confirm as /permissions.
  - actual: Right from auto shows 'permissions.mode = full' and 'Enter confirms this field · Esc cancels field only' in the detail pane. Enter saves full with no warning. Clicking a chip does not apply it, unlike every other chip row, and needs a second Enter. Subagent mode behaves the same way.
  - repro: `/settings; down x3; Right; Enter  |  click '⟨ manual ⟩'; Enter`
  - code: Two hard-coded special cases make any key starting with "permissions." or equal to "agents.mode" stage a raw edit (p.editing) instead of saving. One is crates/sterna/src/workbench/settings.rs ~l.252-256, in the arrow-cycle function: `if spec.key.starts_with("permissions.") || spec.key == "agents.mode" { self.editing = Some((spec.key.into(), choices[i].clone())); return Ok(()); }`. The other is cra

### /settings <argument> (1)

- **'/settings theme' opens a dead-end sheet saying to use an interactive terminal** — defect, loose-end
  - expected: Settings opens at the matching row (as /statusline does), or at least the normal sheet
  - actual: A 'Settings' sheet with a single selectable row, '› Open /settings in an interactive terminal. CLI: pane config --help'. We are in an interactive terminal. Enter and click do nothing, and Esc is the only way out.
  - repro: `t.type('/settings theme'); enter; t.key('enter'); click the row`
  - code: Two call sites intercept /settings, and both match only the exact bare text. The first is crates/sterna/src/session/ui.rs:1881, `matches!(editor.text.trim(), "/settings" | "/statusline")`, which opens settings_session::Editor. The second is crates/sterna/src/workbench/input.rs:34, where local_command matches the exact slice `["/settings"]`. Once there is an argument, neither matches. The command t

### /settings Advanced: Full access (1)

- **Full access is offered in Project scope, refused only after Enter, and the error is truncated and stays on screen** — defect, loose-end
  - expected: In Project scope the row is disabled with 'global only, press F6', or it switches scope
  - actual: Right shows 'permissions.full_access = true / Enter confirms'. Enter prints 'settings: `permissions.full_access` is a global setting only. A project file travels inside a repository, so a project-scoped `permi' (cut mid-word). The error stays visible under unrelated rows after Esc and a new search.
  - repro: `/settings; type 'full access'; Right; Enter; then Esc and search 'recommended'`
  - code: - **The refusal:** it only happens when the value is written. crates/sterna/src/settings.rs:355-361 (Store write) returns the long Err string when `scope == Scope::Local && registry::is_global_only(key)`. registry::is_global_only is in settings/registry.rs:139, and the settings sheet never calls it to disable the row or switch scope.
- **The message placement:** the panel puts that Err into `p.not

### /settings Subagents (1)

- **Subagent mode offers 'pinned' and 'roster' that are refused with raw TOML errors** — defect, loose-end
  - expected: Choosing pinned or roster leads straight to picking the model or favourites, or the choice is disabled with a plain reason
  - actual: Enter on pinned gives 'settings: [agents] mode = pinned requires `model`'. Clicking roster leaves that pinned error on screen until Enter replaces it with 'settings: configure a favorite before enabling roster delegation'. The value stays off, so it is two dead ends in a row.
  - repro: `/settings; Tab x4; Right; Enter; click '⟨ roster ⟩'; Enter`
  - code: The refusal strings come from config validation in crates/sterna/src/config/agents.rs: line 114 is `return Err("[agents] mode = pinned requires `model`".into())` and line 179 is `return Err("configure a favorite before enabling roster delegation".into())`. The settings sheet writes agents.mode, and the parse rejects it; the text is shown as a raw "settings: {err}" notice. The sheet's mode choice n

### /settings Advanced category (1)

- **Advanced is 68 rows that include internal and legacy keys, duplicates and raw thresholds** — debt, loose-end
  - expected: Only real user decisions, each once, with readable values
  - actual: It exposes 'Recommended settings seen 0' (wizard bookkeeping), 'Legacy file imported ⟨ Off ⟩ ⟨ On ⟩' and 'Reduced motion'. It repeats rows already in Subagents and Little helpers (the favourites, Find/Reduce/Check effort). It lists 14 bare probability thresholds (0.85 / 0.1 / 0.9). Empty lists show as '[]' for domains but 'none' for patterns. One label collides with its chips: 'Request-derived acceptance li⟨ Off ⟩ ⟨ On ⟩'. /config opens this category directly.
  - repro: `/settings; Shift-Tab; walk with Down to the end`
  - code: crates/sterna/src/workbench/settings.rs ~line 115: the Advanced filter is `_ => !spec.basic && spec.key != "limits.task_tokens"`. Every key with basic:false lands in Advanced, even if another category already lists it, and there is no hidden flag for internal keys. Internal keys are declared as ordinary SettingSpecs in crates/sterna/src/settings/registry.rs: `wizard.seen` (~l.950) and `legacy.impo

### /settings number/text editor (1)

- **The field editor is a raw 'key = value' line in the footer, with no cursor and no range shown until you get it wrong** — defect, visual
  - expected: An inline field on the row with a visible cursor, showing the allowed range
  - actual: The row still shows '8 ›' while the detail pane shows 'helpers.calls_per_cell = 8' with no cursor. The errors read 'settings: `helpers.calls_per_cell` must be a whole number, not `abc`' and then 'settings: `calls_per_cell` must be between 1 and 64' (a different key spelling). Allowed patterns rejects 'git status' with 'use Read, Write, Edit, Bash or mcp__server__tool' but never shows the Bash(…) syntax.
  - repro: `/settings; type 'helper calls'; Enter; backspace; type 'abc'; Enter; then '-5'; Enter`
  - code: - **Editor drawing:** crates/sterna/src/workbench/view.rs around line 1859. When `p.editing` is set, it draws `format!("{key} = {value}")` as a plain label under the list, with no cursor and no range. The row itself is never turned into an input field.
- **Key spelling mismatch:** crates/sterna/src/settings/registry.rs:1032 writes the integer parse error with the full key `helpers.calls_per_cell`.

### /settings Undo (1)

- **Undo is one level, and a second Undo looks like it worked** — defect, bug
  - expected: Several levels of undo, or the chip disables and says 'nothing to undo'
  - actual: The first Ctrl-Z goes back to medium with 'Restored.'. The second Ctrl-Z and the chip click change nothing, but 'Restored.' stays on screen and the Undo chip always looks active.
  - repro: `/settings; down; Right x3 (low, medium, high); Ctrl-Z; Ctrl-Z; click '⟨ Undo · ^Z ⟩'`
  - code: Two places. First, crates/sterna/src/workbench/settings.rs: the field `undo: Option<Vec<(String, Option<String>)>>` (line 51) holds one step only, and save() overwrites it (line 190). In undo() (lines 259-292), `self.undo = None` is set after the first restore, and when `self.undo` is None the function returns Ok(()) and never touches `self.notice`, so the old 'Restored.' stays. Second, crates/ste

### /settings detail pane and copy (1)

- **Descriptions are cut mid-sentence and several notices show raw values** — polish, copy
  - expected: Full sentences in the detail pane and human wording in notices
  - actual: The effort detail ends at '…which is not the same as clearing an override you'. The row descriptions end in '…'. Notices read 'Reduced motion is now true', 'Sidebar is now hide', 'Theme is now unset' and 'Main model is saved.' (after it was unset). The header promises 'every choice applies now … there is no Apply' while most rows say 'applies from the next session'. During a search the heading still shows '‹ Everyday ›' over mixed results. At 80x24 the list shows about 2.5 settings while the detail pane holds 3 blank rows, and the footer runs together as 'Effort: max · applied to the next request  Esc closes · saved choices stay'. Permission descriptions at 80 cols are cut without an ellipsis ('Anything else is c').
  - repro: `/settings; down (Reasoning effort); also Reduced motion On, /sidebar hide, Backspace-reset of Theme`
  - code: crates/sterna/src/workbench/view.rs draw_settings lines 1803-1820 (detail cut by take(2), block fixed at bottom-7). crates/sterna/src/workbench/settings.rs lines 210-220 plus fn show at line 319 (raw value or 'unset' in notices, not human_value). view.rs lines 1355-1390 (notice and hint share one row).

### conversation transcript (1)

- **The user message sent right after a turn that ran a cell is never shown** — defect, bug
  - expected: Every prompt appears as a '┃ you / message N' block followed by its own '⠿ pane' block.
  - actual: 'message 3' (the prompt after the cell turn) has no 'you' block and no 'pane' header; its answer 'Plain three.' is glued under the previous turn's 'Wrote a.'. Same with the default fixture: of five prompts, 'prompt number 2' is missing. Plain-answer-only turns do not trigger it. It also swallows a queued follow-up sent after a cell turn.
  - repro: `W='ʼʼʼpane\nawait write({path: "a.txt", content: "a\\n"});\nreturn "wrote a";\nʼʼʼ'; t=Tui(fake=True,cols=140,rows=42); t.wait('What next',15); t.model.script=['Plain one.', W, 'Wrote a.', 'Plain three.']; for i in 1..4: t.type(f'message {i}'); t.key('enter'); t.settle(2.2); print(t.screen())`
  - code: crates/sterna/src/workbench/document.rs Document::build lines ~284-301: `if m.historical.is_some() { continue; }` comes before the User branch that clears `feedback`. Runtime cell feedback is Message::runtime (historical=Some, contract.rs:101; pushed in session.rs ~1728), so the stale `feedback=true` set at line 327 swallows the next real user prompt via `if feedback { feedback = false; continue; 

### composer queue (1)

- **'Esc takes the last one back' is false: Esc stops the turn and the queued messages still go out** — defect, bug
  - expected: Esc puts 'queued B' back in the composer and it is not sent. The queue hint disappears once the queue is empty.
  - actual: Esc shows 'Stopping after this cell · Esc again cancels the call in flight' and marks the status 'stop requested'. The QUEUED rows vanish, the composer stays empty, and both 'queued A' and 'queued B' are still sent (model requests 2 and 3 carry them). With one queued message the result is the same. After everything finishes, the status still reads '✓ complete ✓ ── Queued for when this turn ends · Esc takes the last one back'.
  - repro: `t=Tui(fake=True); slow the model (wrap t.model.reply with time.sleep(4)); t.type('slow question'); t.key('enter'); t.settle(1); t.type('queued A'); t.key('enter'); t.type('queued B'); t.key('enter'); t.key('esc'); t.settle(12)`
  - code: In crates/sterna/src/session/ui.rs, submitting while busy (about lines 1951-1969) sends the text straight away with answers.inputs.send(Input::Submit(text)). After that, state.queued is only a display list. It then sets the notice 'Queued for when this turn ends · Esc takes the last one back'. The Esc handler (about lines 1696-1721) only pops state.queued when !busy. While busy it runs steer.reque

### text selection (1)

- **Dragging across several lines copies card borders and sidebar text into the clipboard** — defect, mouse
  - expected: Only the code text is copied: three program lines.
  - actual: The copied text is 'const files = await bash({command: "ls -la && git log --oneline -3"});                            │  │ subagents off\n    │   const readme = …'. It includes the card's '│' borders, the padding and the sidebar's 'subagents off'. A single-line drag over the answer copies cleanly, and the highlight renders (23-drag-select.png).
  - repro: `t.type('what is in this project?'); t.key('enter'); t.wait('complete'); x,y=t.find('const files'); t.drag(x,y,x+30,y+2); read the OSC 52 payload from t.raw`
  - code: crates/sterna/src/tui/selection.rs: Selection::columns() makes every row in the middle and at the start span the whole `area`, up to area.right(), and draw() copies raw buffer cells, borders included. The module doc says this is deliberate: "what is copied is exactly what is on the screen". crates/sterna/src/workbench/view.rs:605 passes the full-screen rect `a` into draw_selection, not the transcr

### cell card (denied write) (1)

- **Denying an approval shows `write("")` and 'host call gate' jargon** — defect, copy
  - expected: Something like '⊘ write notes.txt — you denied it'.
  - actual: '✕ PermissionDenied: write("")': the path is empty although the call was notes.txt. It is followed by 'rule: the host call gate denied or cancelled this exact attempt'. The turn then shows the model's text 'I wrote `notes.txt`…' under a failed card.
  - repro: `(Every call rung as above) t.model.script=[W,'…']; t.type('write a notes file'); t.key('enter'); t.wait('Approve exact tool call'); t.key('esc'); t.settle(2.5)`
  - code: crates/sterna/src/tools/invoke.rs lines ~700-745: when gate.admit() refuses, the code builds PermissionDenied { path: String::new(), rule: "the host call gate denied or cancelled this exact attempt" } (line 726). The two sibling refusals at lines 706 and 741 also leave path empty. crates/sterna/src/sandbox/profile.rs:83-90: Display prints "PermissionDenied: {tool}(\"{path}\")\n  rule: {rule}\n  to

### transcript keys (1)

- **Home/End don't scroll, the keys sheet omits scrolling and Ctrl-C, and there is no way to select a cell from the keyboard** — defect, keyboard
  - expected: With an empty composer, Home/End jump to the top/bottom of the transcript. The ? sheet lists PgUp/PgDn, the wheel and Ctrl-C. Some key moves the 'selected cell' that Ctrl-O/F4/F5 act on, and the selection is visible.
  - actual: Home and End leave the scroll position unchanged (PgUp/PgDn and the wheel work). The ? sheet lists neither scrolling nor Ctrl-C. Ctrl-O and F5 do nothing, with no feedback, until a cell has been clicked, and nothing shows which cell is selected.
  - repro: `after 7 turns: t.scroll(50,20,up=True,times=5); t.key('end'); t.key('home')  — and on a fresh transcript with two collapsed cells: t.key('ctrl-o'); t.key('f5')`
  - code: crates/sterna/src/session/ui.rs ~617-628: KeyCode::Home/End only move the composer cursor within the line. Nothing maps them to transcript scrollback (s.scrollback), even when the composer is empty.
crates/sterna/src/workbench/input.rs ~670-702: F4, F5 and Ctrl-O use `self.selected_cell.unwrap_or(n.cells.len())`. The only places that set selected_cell are clicks and the /cell and /diff commands (i

### cell card (1)

- **Loose ends in the cell card** — polish, loose-end
  - expected: No empty sections, correct plurals, and a readable call row.
  - actual: 'Full output' shows a 'Handles' heading with nothing under it. The default view's call row reads only '⊘ bash', with no command, reason or 'denied' (a denied write row shows 'notes.txt … denied'). A one-line cell is titled '001 · 1 lines'. The status reads '✓ complete ✓' with a doubled check mark. The rollback status is 'Rollback complete:' with a dangling colon. A bare '/cell' does nothing and gives no usage hint. The model-facing text 'ToolError: ask: asking is off for this session (`[ask] enabled`), so decide yourself' is shown to the person.
  - repro: `t.type('what is in this project?'); t.key('enter'); t.wait('complete'); x,y=t.find('Full output'); t.click(x+2,y)  — and the ask cell above; and t.type('/cell'); t.key('enter')`
  - code: - **Handles and the plural:** crates/sterna/src/workbench/document.rs. Line 394 is `format!("{} lines", program_now.lines().count())`, with no singular case. Lines 445-455, the CellTab::Output loop over ("Observed calls", "Result", "stdout", "Handles"), print a heading whenever the value is Some, so an empty `v.table` still gets a heading. It should skip when the value is empty.
- **Ask text:** th

### cell card / status (1)

- **The card says RUNNING while it is really waiting for the person** — defect, inconsistency
  - expected: The card and status say 'waiting for your approval' or 'waiting for your answer', and the run timer pauses.
  - actual: The card header reads '● RUNNING 00:03' with '◆ executing this cell' animating, the status reads '◓ executing cell 001', and the timer keeps counting while the approval or ask sheet waits.
  - repro: `(Every call rung) t.model.script=[W,'…']; t.type('write a notes file'); t.key('enter'); t.wait('Approve exact tool call'); t.settle(2); print(t.screen())`
  - code: crates/sterna/src/workbench/voice.rs: the Activity enum has no variant for waiting on approval or an answer. Activity::Executing maps to 'executing cell {n:03}' (line ~121), and working() maps it to 'executing this cell' (line ~161). crates/sterna/src/workbench/document.rs:~366-370 draws '● RUNNING' + clock(s.pulse.elapsed_ms) while `running`, and the pulse clock does not pause while the approval 

### AUDITOR ACTION, not a TUI finding: read this first (1)

- **The auditor ran a broad `pkill -u 501 -f cat` that may have sent SIGTERM to unrelated user processes** — defect, loose-end
  - expected: The only process stopped is the stray cat.
  - actual: Right after it, the cmux-cua MCP server reported that it had disconnected and failed to reconnect. My follow-up `ps` to see what else was hit was blocked by the auto-mode classifier, so I could not check which other processes, if any, received SIGTERM (the Bash tool may be sandboxed from signalling them). No repository file was touched. The user should be told, and should check that cmux-cua and any apps they expect to be running are still up.
  - repro: `At the start of this audit a stray ʼcat > /tmp/dummyʼ in my shell setup blocked on stdin. To clear it I ran ʼpkill -f 'cat' -u $(id -u)ʼ. That pattern matches every process of this user whose full command line contains the letters 'cat', and that includes anything launched from /Applications/…`
  - code: No repository code is involved. The cause was the auditor's shell command `pkill -f 'cat' -u $(id -u)`: -f matches the substring anywhere in the full argv, and "cat" appears inside "Application", "Location" and "Notification".

### /rollback preview sheet (1)

- **Rollback preselects the destructive 'Confirm rollback': /rollback, Enter, Enter deletes files** — defect, keyboard
  - expected: A sheet titled 'confirmation required' opens with Cancel (or nothing) selected, so a reflexive Enter cannot destroy work.
  - actual: The › cursor starts on 'Confirm rollback'. The second Enter deletes notes.txt (the file-exists check returned False) and the status reads 'Rollback complete:'.
  - repro: `WriterFake scripted model (a cell that runs write({path:'notes.txt', …})); t=Tui(fake=True); t.type('write notes'); t.key('enter'); t.wait('Wrote notes',30); t.type('/rollback'); t.key('enter'); t.key('enter'); os.path.exists(cwd+'/notes.txt')`
  - code: crates/sterna/src/session/controls.rs, fn rollback, around lines 1551-1569. The code pushes the rows 'Confirm rollback' (command '/rollback confirm') and then 'Cancel' (command '/rollback cancel'). It then sets `panel.selected = panel.rows.len().saturating_sub(2);`, and that index is the 'Confirm rollback' row. The fix is to select the Cancel row (rows.len()-1), or to list Cancel first. The confir

### composer: Ctrl-C (1)

- **Ctrl-C silently throws away the typed draft, with no notice and no undo** — polish, keyboard
  - expected: The draft is kept (as the running-turn notice promises: 'Your draft is kept'), or clearing it is announced and can be undone.
  - actual: The composer goes back to the placeholder. Ctrl-Z and Up do not bring the text back, /activity has no record of it, and nothing on screen says the text was discarded.
  - repro: `t=Tui(fake=True); t.wait('What next',15); t.type('a long carefully written prompt'); t.key('ctrl-c'); t.key('ctrl-z'); t.key('up')`
  - code: crates/sterna/src/session/ui.rs around lines 1725-1731: `KeyCode::Char('c') => { if !busy && !editor.text.is_empty() { editor.text.clear(); editor.cursor = 0; } else { INTERRUPT.store(true) } }` clears the draft with no state.note(...), no history push and no undo snapshot. The workbench layer (crates/sterna/src/workbench/input.rs:372-383) turns Ctrl-C into a copy only when there is a selection; o

### composer: double Ctrl-C (1)

- **The first Ctrl-C gives no warning that a second one ends the session** — defect, keyboard
  - expected: After the first press, a visible 'Ctrl-C again to quit' line, as the in-turn notice says ('twice exits').
  - actual: The screen diff after one Ctrl-C on an empty composer is empty: nothing changes. A second press inside a short window prints 'pane: interrupted twice; ending the session' and exits with code 130. A second press about 3 s later does nothing, and the screen does not show that either.
  - repro: `t=Tui(fake=True); t.wait('What next',15); a=t.screen(); t.key('ctrl-c'); b=t.screen(); (diff a,b); then t.key('ctrl-c') quickly`
  - code: - crates/sterna/src/session/ui.rs, around lines 1725-1732 (composer key handling). When the composer is empty, Ctrl-C only does `super::INTERRUPT.store(true, ...)`. Unlike Esc just above it, it sets no state.note/notice.
- crates/sterna/src/session.rs, fn watch (around lines 341-366). It sets `first = Some(now)` and on a second press within DOUBLE_INTERRUPT_WINDOW (2 s, line 110) calls end_the_ses

### @ path completion (KEYS sheet, placeholder, welcome hint) (1)

- **'@ for a file' is advertised in four places but typing @ does nothing** — defect, loose-end
  - expected: A list of project paths appears and can be picked with keys or the mouse.
  - actual: No popup, no completion; Tab and Down do nothing. The source contains no @ handler, only the strings that advertise it.
  - repro: `t=Tui(fake=True); t.wait('What next',15); t.type('@'); t.settle(1.5); t.type('ma'); t.key('tab'); t.type('@src/'); t.key('down')`
  - code: crates/sterna/src/tui.rs:586 slash_matches() (used by completions_shown() just above it and by workbench/view.rs:563) returns nothing unless the input starts with '/'. Nothing handles '@' or completes paths. The advertising strings are at workbench/voice.rs:78, 216 and 226, workbench/document.rs:1266 and 1268, and the KEYS sheet row at workbench/view.rs:1571 ("@", "a path in this project"). The fi

### --resume picker / session store (1)

- **Sessions where nothing was asked are saved and preselected at the top of the resume list** — defect, bug
  - expected: A session with no prompt is not offered (or is at least not the default), because resuming it resumes nothing.
  - actual: The first row, selected by default, is '› just now  (nothing asked yet)  0 prompts  tlzn7g-1iwk', above the real conversation. Ctrl-D on an empty session still prints 'resume it with: pane --resume <id>'.
  - repro: `In one project: send a prompt and /exit; then start again and immediately press Ctrl-D; then Tui(args=['--resume'], same cwd and XDG)`
  - code: crates/sterna/src/session/resume.rs: resumable() (around lines 125-147) lists every <id>.jsonl, sorted by mtime, newest first. pickable() (lines 166-197) turns 0 prompts into the title '(nothing asked yet)' instead of filtering those sessions out or ranking them below the rest, so the empty session becomes row 0, which is selected by default. resume_hint() (line 150) is printed without any conditi

### KEYS sheet: 'Shift-Enter for a new line' (1)

- **Shift-Enter depends on a keyboard protocol Sterna never requests; Alt-Enter works but is not listed; Ctrl-J is ignored** — defect, keyboard
  - expected: Shift-Enter makes a newline in common terminals; if that cannot be guaranteed, the sheet also lists the fallback that always works.
  - actual: CSI-u Shift-Enter and Alt-Enter both insert a newline. But Sterna emits no kitty keyboard-protocol push (no ESC[>…u) and no modifyOtherKeys request, so terminals that report Shift-Enter distinctly only after that opt-in will send a plain CR (this last step is inferred, not observed here). Alt-Enter is not documented. Ctrl-J did nothing: 'line threeline fourline five' ran together on one line.
  - repro: `t=Tui(fake=True); inspect t.raw at startup for ESC[>…u; t.type('line one'); t.send('\x1b[13;2u'); t.type('line two'); t.send('\x1b\r'); t.type('line three'); t.send('\n'); t.type('line five')`
  - code: In crates/sterna/src/session/ui.rs, around line 656, the editor's key() inserts "\n" only for KeyCode::Enter with ALT or SHIFT. Ctrl-J arrives as Char('j') with CONTROL, falls through to `_ => {}` and is dropped. Terminal setup (ui.rs:121 EnableMouseCapture, ui.rs:816 EnterAlternateScreen + EnableBracketedPaste) never runs crossterm PushKeyboardEnhancementFlags, and the crate contains no KeyboardE

### /exit during a running turn (1)

- **/exit is refused while working, and the refusal says Ctrl-C only 'interrupts tools'** — defect, copy
  - expected: /exit ends the session, or queues it; the notice describes what Ctrl-C actually does.
  - actual: /exit stays in the composer and the edge says 'Working. Your draft is kept; Ctrl-C interrupts tools; twice exits.' But one Ctrl-C cancels the whole turn, not just tools (see the '✓ complete' finding). Ctrl-D during a turn does nothing and says nothing.
  - repro: `SlowFake; t.type('slow one'); t.key('enter'); t.settle(1.5); t.type('/exit'); t.key('enter')`
  - code: crates/sterna/src/session/ui.rs ~1951-1959: when busy, any text starting with '/' (including /exit) is refused and the notice is set to the hard-coded 'Working. Your draft is kept; Ctrl-C interrupts tools; twice exits.' There is no /exit special case, and nothing clears the notice when the turn ends. ui.rs ~1725-1731: Ctrl-C while busy sets super::INTERRUPT, which cancels the whole turn. ui.rs ~17

### sheet footer (KEYS, Commands, ACTIVITY, ACCESS, Rollback, Models) (1)

- **Every sheet ends with 'Esc closes · saved choices stay', even read-only ones, and on the Models sheet a notice runs into it** — polish, visual
  - expected: The footer fits the sheet (a read-only sheet has no choices to save), and a long notice is truncated with an ellipsis, not overwritten.
  - actual: Footer: 'Every call · Confirms every admitted file and command call before it runs. Shift-Tab again for the Esc closes · saved choices stay'. The KEYS and Commands sheets say 'saved choices stay'. Sheet titles vary in style: 'KEYS' in caps with a subtitle, 'Commands', 'Session configuration', 'Rollback preview · confirmation required'.
  - repro: `t.key('backtab'); t.key('f3')  (fake mode) and read the footer; also ? and /help`
  - code: crates/sterna/src/workbench/view.rs, around lines 1353-1390 (the sheet foot). The notice (ui.notice, or s.notice) is drawn with row() across the full inner.width. The hint is then drawn over the notice's right end at inner.right() - hint.len() - 1, so nothing truncates the notice or adds an ellipsis. The hint is hard-coded to "Esc closes · saved choices stay" for every sheet except ui.models.is_so

### KEYS sheet (?) (1)

- **Key rows are not clickable, though the keys work while the sheet is open** — defect, mouse
  - expected: Clicking 'F2  settings…' opens settings, as pressing F2 on the same sheet does.
  - actual: The click does nothing; F2 opens SETTINGS. Most of the sheet is empty space below the 16 rows.
  - repro: `t.type('?'); x,y=t.find('settings, applied'); t.click(x,y)  vs  t.key('f2')`
  - code: crates/sterna/src/workbench/view.rs, around lines 1553 to 1583 (the `else if ui.help` branch). The 16 (key, what) pairs are drawn only with `label(...)` and `row(...)`. Nothing registers a hit target for them. Clickable elements elsewhere in the same function, such as `add_chip(f, g, ...)` in the confirm branch just below, take the hit-map `g`. The KEYS rows never touch `g`, so a mouse press on th

### /rollback result (1)

- **After a rollback the cell still claims its change; the status line is cut at the colon** — defect, inconsistency
  - expected: Cell 001 is marked rolled back, and the status names what was undone.
  - actual: Cell 001 still reads '✓ EXECUTED' (expanded: '✓ executed · 1 file changed'). The edge shows 'Rollback complete:' with nothing after the colon. Esc from the preview gives no 'cancelled' notice, unlike clicking Cancel.
  - repro: `WriterFake turn, then /rollback, click 'Confirm rollback'`
  - code: 1. /crates/sterna/src/session/controls.rs, around lines 1549-1590 (the /rollback handler). On confirm it only pops session.rollbacks and prints session_println!("Rollback complete:\n{}", plan.preview()). Nothing marks the cell record as rolled back; a grep for 'rolled back', RolledBack or rolled_back finds nothing in src. So the cell header and footer ('✓ EXECUTED', '1 file changed', 'Changes +2 −

### Scarlet Macaw and Green-winged Macaw themes (1)

- **Scarlet/Green-winged accent is the error red, so all chrome reads as failure** — defect, visual
  - expected: Accent (headings, selected chips, notices) is clearly different from the failure colour, so '✕ FAILED' and PermissionDenied stand out.
  - actual: The accent #ff4a3d (scarlet) / #e0404f (green-wing) sits next to the error colour #ff8494. Headings, the 'Cell program' chip, the 'pane' label, every selected Settings chip and the bold 'Theme is now scarlet. Ctrl-Z undoes it.' notice all look like errors, and the red '✕ FAILED' cell blends in. Rose (#f29cda) comes close too.
  - repro: `config theme = "scarlet" (or "green-wing"); t=Tui(fake=True,cols=140,rows=42,cwd=proj); t.wait('ready',15); t.type('add a subtract function'); t.key('enter'); t.wait('Nothing else',20); t.png('x'). Also /statusline in scarlet.`
  - code: - The accent comes from the plumage table in crates/sterna/src/workbench/plumage.rs: SCARLET.accent = 0xff4a3d (line 171) and GREEN_WING.accent = 0xe0404f (line 199).
- crates/sterna/src/tui/theme.rs:116 passes it through unchanged as the chrome accent (`Self::Bird(bird) => rgb(bird.plumage().accent)`).
- The error colour is the fixed `const RED: Color = Color::Rgb(0xff, 0x84, 0x94)` in crates/ste

### terminal without true colour (COLORTERM unset) (1)

- **24-bit colour escapes are still sent when truecolor is off** — defect, bug
  - expected: When Sterna itself decides the terminal has no true colour (it drops the parrot and falls back to neon), it sends 256- or 16-colour sequences.
  - actual: 65 `38;2;r;g;b` sequences (e.g. 218;255;80 neon, 139;227;255, 85;100;118) and 0 `38;5;` sequences on the start screen. The /theme sheet swatches are RGB too. On a 256-colour terminal, those colours are approximated or come out wrong.
  - repro: `t=Tui(fake=True,cols=140,rows=42,colorterm=None); t.wait('ready',15); count re.findall(rb'38;2;', bytes(t.raw)) vs rb'38;5;'`
  - code: The check is `crates/sterna/src/workbench/plumage.rs:348` `truecolor()`, which reads COLORTERM. Its result only picks the theme (`crates/sterna/src/tui/theme.rs` `Theme::natural()`) and the bird art (`crates/sterna/src/workbench/sheets.rs:185/215`, `document.rs:1123`). The palette in `crates/sterna/src/workbench/theme.rs` is all hard-coded `Color::Rgb`: accents on lines 16-25 and MUTED/LINE/HELPER

### failed request (1)

- **Failure text repeats itself and points to a cell that does not exist** — defect, copy
  - expected: One plain error (provider unreachable, where it tried, what to do), and no reference to a cell when none ran.
  - actual: The conversation shows '✕ ERROR: request failed: request failed: io: Connection refused'. The composer edge says 'that one failed — open the cell and I'll show you why', but there is no cell. The card still says 'Evening! Back in the nest.' and the sidebar 'THIS SESSION 00:00 on this turn'.
  - repro: `t=Tui(fake=True,cols=140,rows=42); t.wait('ready',15); t.model.server.shutdown(); t.model.server.server_close(); t.type('add a subtract function'); t.key('enter'); time.sleep(8); t.png('x')`
  - code: Doubled prefix: in crates/sterna/src/session/context.rs:244 the code does `Err(error) => return Err(format!("request failed: {error}"))`, but WireError's Display in crates/sterna/src/wire.rs:841 already writes `WireError::Http(err) => write!(f, "request failed: {err}")`. Wrong cell reference: in crates/sterna/src/workbench/voice.rs:138-143 the text for Activity::Failed is always "that one failed —

### /theme sheet (mouse) (1)

- **Clicking a theme's colour swatch does nothing** — defect, mouse
  - expected: The swatch, the most obvious target on a palette row, selects the theme like the name does.
  - actual: Clicks on the '██' swatch (columns x-5 and x-4) leave the cursor on Blue-fronted Amazon. A click from x-2 onward applies mint.
  - repro: `t.type('/theme'); t.key('enter'); x,y=t.find('mint'); t.click(x-5,y); t.click(x-4,y)  then t.click(x-2,y)`
  - code: crates/sterna/src/workbench/sheets.rs, draw_themes (around lines 136-158). The swatch is painted with a plain `row(f, Rect::new(inner.x + 4, y, 2, 1), "██", swatch, t)`, which registers no hit region. Only the `add(f, g, Rect::new(inner.x + 7, ...), ..., Action::PanelRow(i), ...)` call registers a clickable Action, and its rect starts at inner.x+7. Columns inner.x+4 to inner.x+6 (the swatch and th

### /theme sheet (keyboard) (1)

- **Themes sheet ignores Home/End/Tab/Space; wheel skips 3 rows** — defect, keyboard
  - expected: The same list keys as other pickers: Home/End jump, Space or Enter chooses, and the wheel moves one row.
  - actual: Home, End, Tab, Shift-Tab and Space do nothing, and typing letters does nothing. Up/Down wrap, and PgUp jumps to neon. Two wheel notches down moved Hyacinth → Sulphur-crested Cockatoo, and one up went to Blue-and-gold (3 rows per notch in a 16-item list that does not scroll).
  - repro: `t.type('/theme'); t.key('enter'); t.key('home'); t.key('end'); t.key('tab'); t.key('backtab'); t.key('space'); x,y=t.find('amber'); t.scroll(x,y,up=False,times=2); t.scroll(x,y,up=True)`
  - code: Keys: crates/sterna/src/session/ui.rs around lines 1592-1611, the generic panel key match. It handles only Esc, Up, Down, PageUp/PageDown (±10, clamped, which is why pgup lands on neon) and Enter. Tab goes to panel.cycle_tier(), which does nothing on the Themes panel. There is no Home, End or Space arm, and there is no search for non-model panels. Wheel: crates/sterna/src/workbench/input.rs lines 

### header / conversation labels (1)

- **Braille '⠿' still marks Sterna in the header and turn labels, not the parrot head** — polish, loose-end
  - expected: The header brand uses the parrot head (the head replaced the braille bird), and no braille bird glyph remains.
  - actual: The header row is ' ⠿ PANE / tui-project-…' in every theme, including classic and no-truecolor. Every reply is labelled '⠿ pane', and the thinking spinner is braille '⠸ thinking…'. The parrot head only appears in the card after the first message, never in the header.
  - repro: `t=Tui(fake=True,cols=140,rows=42); t.wait('ready',15); print(t.screen().split('\n')[0]); send a prompt and look at the reply label`
  - code: crates/sterna/src/workbench/view.rs:226 `let brand = " ⠿ PANE /";` (header); crates/sterna/src/workbench/document.rs:666 `format!("⠿ {}", voice::PANE)` (reply label); also crates/sterna/src/tui/ask.rs:36 and :83 (" ⠿ pane is asking ", " ⠿ ask pane to do it another way " sheet titles). The spinner in crates/sterna/src/workbench/motion.rs:14 ORBIT is a generic braille spinner and is not the bird mar

### /theme sheet (1)

- **Classic themes have no preview** — polish, visual
  - expected: Highlighting a classic palette shows its colours (swatches, sample heading/chip) the way a parrot shows plumage swatches.
  - actual: The preview is only 'neon / no bird · the palette alone'. The sheet itself stays in the current theme's colours, so you cannot see a classic palette before committing to it.
  - repro: `t.type('/theme'); t.key('enter'); t.key('pgup'); t.png('x')`
  - code: crates/sterna/src/workbench/sheets.rs, the theme-sheet draw function, around line 171. `let Theme::Bird(bird) = chosen else { label(title); label("no bird · the palette alone"); return; };` returns early for every non-Bird theme, and only the Bird branch draws the swatches (plumage.accent/second/highlight around line 195). The list swatch comes from super::theme::accent(theme), which is only the a

### /theme sheet at 80x24 (1)

- **Narrow Themes sheet hides three parrots while two rows stay blank** — polish, visual
  - expected: All entries fit (there is room), or a scroll hint shows that more are hidden.
  - actual: Green-winged, Military and Cockatoo are not shown, and two empty rows sit above the footer. After PgDn the list scrolls, CLASSIC/neon/amber disappear off the top and the two blank rows remain. There is no scroll indicator, and no bird preview at this width.
  - repro: `t=Tui(fake=True,cols=80,rows=24); t.wait('ready',15); t.type('/theme'); t.key('enter'); print(t.screen()); t.key('pgdn',3)`
  - code: crates/sterna/src/workbench/sheets.rs, draw_themes (line 83). It sets `let rows = inner.height.saturating_sub(4) as usize;` while drawing from inner.y + 2. Only 2 rows go to the header and separator; the other 2 are lost, which leaves the two blank rows above the footer drawn in view.rs:1378. The scroll logic is `start = at.saturating_sub(rows-1)` with skip(start).take(rows), and it draws no overf

### composer edge / instruments (1)

- **Context meter text is cryptic: 'ctx 12 · reported'** — polish, copy
  - expected: Plain words, e.g. 'context 12 tokens (provider count, window size unknown)'.
  - actual: The edge shows 'ctx 12 · reported' after the turn and 'ctx 7.9k · estimated' while thinking. The instruments say 'context 12 / window unknown · reported', 'Cached input unreported · cache write unreported · cost unreported', 'Provider unreported · route unreported'.
  - repro: `t.type('hello'); t.key('enter'); read the bottom-right edge; during the turn it reads 'ctx 7.9k · estimated'; t.key('ctrl-t')`
  - code: - crates/sterna/src/tui/status.rs:40-75, `context_summary`: when there is no window cap it returns format!("ctx {} · {}", compact_tokens(used), counted.as_str()). It also writes "ctx {bar} {used}/{cap} {percent}%" and "ctx {bar} {used}/~{cap}".
- crates/sterna/src/tui.rs:787-793, `Counted::as_str`: returns "reported", "estimated" or "part estimated".
- crates/sterna/src/tui/telemetry.rs:250-255: "

### parrot sprite (all parrot themes) (1)

- **Parrot beak is almost invisible on dark background; 'done' tick is loose pixels** — polish, visual
  - expected: The hooked beak, which defines a parrot, reads clearly, and mood marks are recognisable.
  - actual: The beak is #3b3b3d on a near-black background (~1.7:1). In the /theme preview it breaks into separate dark blocks. The Done-mood tick (green #5fd07a pixels at the upper right) reads as a stray squiggle, and in the sheet preview as a '!' with dots. The preview always shows the Done mood.
  - repro: `t=Tui(fake=True,cols=140,rows=42); t.wait('ready',15); t.png('a'); t.type('/theme'); t.key('enter'); t.png('b'); zoom the head`
  - code: crates/sterna/src/workbench/plumage.rs: the beak colour is regions[5] in each Plumage (AMAZON 0x3b3b3d; others 0x1c1c1e–0x2b2b30, with no light outline pixel against dark terminals). The Done tick pixels are in pixels() under `Mood::Done` (lines ~314-318, tick = 0x5fd07a). The preview's mood is fixed at crates/sterna/src/workbench/sheets.rs:186 `sprite(bird, Mood::Done)`.

### Sulphur-crested Cockatoo on a light terminal (1)

- **Cockatoo turns invisible on a light background** — polish, visual
  - expected: The bird keeps an outline or a darker shade against a light background.
  - actual: The white/cream body merges into the background. Only the yellow crest, blue eye ring and dark beak float in the card, and the compact head after a turn is a few isolated pixels.
  - repro: `config theme = "cockatoo"; t=Tui(fake=True,cols=140,rows=42,cwd=proj); t.wait('ready',15); t.png('x', light=True)`
  - code: crates/sterna/src/workbench/plumage.rs. In COCKATOO (line ~222), every region is near-white: forehead/head f7f5ef, cheek f5eec4, body/shoulder f4f2ec, wing e6e2d6, tail ece8dc. `cells()` (line ~332) maps an empty pixel to `None`, which means the terminal's own background. Nothing in plumage.rs checks for a light background, and the sprite has no outline region. The full sprite is drawn at document

### theme palettes (1)

- **Hard-coded colours ignore the theme: HELPERS cyan, 'you' purple, mono not mono** — polish, inconsistency
  - expected: Sidebar headings share the theme accent, and mono is monochrome. HELPERS state is shown once.
  - actual: '◇ HELPERS' is cyan #8be3ff in all 16 themes, while the other sidebar headings use the accent. It also repeats 'helpers off' from the MODEL block. In mono, 'you' is purple, '◇ HELPERS' cyan and '⊘ bash' orange. 'THIS SESSION' is an empty heading on the first screen.
  - repro: `config theme = "mono" (or amber); send one prompt; t.png('x')`
  - code: In crates/sterna/src/workbench/theme.rs the module doc says only the accent moves with the theme. `accent()` returns Color::Reset for Mono, but `style()` maps Tone::Helper to the fixed `HELPER` = Rgb(0x8b,0xe3,0xff) (line 34, which is also Ice's accent), Tone::You to the fixed `YOU` = Rgb(0xc9,0xb8,0xff), and Warning/Failure/Success to the fixed WARN/RED/GREEN constants, whatever the theme is. In 

### theme list (1)

- **Scarlet and Green-winged Macaw are near-duplicates; 'Green-winged' swatch is red** — polish, visual
  - expected: Each theme is clearly different from the others.
  - actual: Both are a red bird with blue wings and a red accent (#ff4a3d vs #e0404f). They differ by a few yellow or green pixels. In the list, 'Green-winged Macaw' has a red swatch next to Scarlet's red swatch.
  - repro: `compare scarlet-140x42-empty.png and green-wing-140x42-empty.png; t.type('/theme'); t.key('enter')`
  - code: crates/sterna/src/workbench/plumage.rs: GREEN_WING (line ~194) vs SCARLET (line ~166). Their regions() tables differ only in the shoulder colour, and 'S' is ~4 pixels of ART. The accent 0xe0404f is red and is what sheets.rs:136-142 draws as the list swatch. A fix would colour the upper 'W' wing rows green on Green-wing (a split wing region, e.g. a new coverts letter) and/or use the green as its li

### composer frame (1)

- **Composer bottom border has a one-column gap** — polish, visual
  - expected: A continuous border line.
  - actual: Every screen at every width ends in '╰─ ─────…' with a blank cell at column 4. In Ctrl-F (chromeless) mode the composer box disappears completely.
  - repro: `t=Tui(fake=True,cols=140,rows=42); t.wait('ready',15); print(t.screen().split('\n')[-1])`
  - code: crates/sterna/src/workbench/view.rs dock_bottom(): row(f, Rect::new(x - 1, a.y, span + 1, 1), " "*(span+1)) still clears one cell at a.x+2 when there are no chips (span==0).

### Telemetry (1)

- **Telemetry passes keys to the hidden composer** — defect, inconsistency
  - expected: Like other full-screen surfaces: typed keys are swallowed (or clearly go to a visible composer), and there is a clickable way back.
  - actual: Space and q land in the composer ('❯  q'). The view has no ⟨ Esc · Back ⟩ chip, and clicking 'Esc returns' does nothing. Down does nothing at the start.
  - repro: `t=Tui(fake=True); t.wait('What next',15); p=t.find('⟨ telemetry ⟩'); t.click(*p); t.key('space'); t.type('q'); p=t.find('Esc returns'); t.click(*p)`
  - code: In crates/sterna/src/workbench/input.rs around lines 384-403, the telemetry key block handles only Esc, Up and Down. Every other key falls through with `_ => {}` and ends up in the composer.

In crates/sterna/src/workbench/view.rs around lines 589-598, the telemetry overlay calls `g.hits.clear()` and registers no hit targets (no Back chip). The header 'Esc returns' is plain text drawn at crates/st

### composer footer hints (1)

- **Rotating footer hints name actions that don't apply** — polish, copy
  - expected: Hints apply to the current state, and a hint that names a click target is right.
  - actual: While idle it says 'Esc once stops after this cell · twice cancels the call'. With no change made it says '/diff shows what I just changed · F4 does the same'. Beside the effort chip, which sits in the composer border, it says 'click anything in the top bar to change it'. Clicking the hint 'F2 opens settings' does nothing.
  - repro: `t=Tui(fake=True); t.wait('What next',15); t.type('/effort'); t.key('enter'); t.key('down',3); t.key('enter'); p=t.find('F2 opens settings') or t.find('Esc once'); t.click(*p)`
  - code: crates/sterna/src/workbench/voice.rs, `pub fn hint(voice, n)` (about lines 206-233). It returns PLAYFUL[n % 8] or PLAIN[n % 8] from a fixed array. `n` counts notices and cells, and nothing about the session's state (running, changes made) is checked. The rendered hint has no mouse hit region either, so clicking 'F2 opens settings' does nothing. A fix would filter the list by state (the Esc hint on

### Sign-in progress panel (Grok / xai) (1)

- **Grok sign-in never shows a link, a code or a failure; it waits forever** — defect, bug
  - expected: A link or device code to sign in with, or a failure line after a timeout.
  - actual: Twice for 40 s the only row was '› waiting for the sign-in, then one request to check it works…'. The broker was running (`cliproxyapi … -xai-login -no-browser`), but nothing it printed reached the panel. There is no timeout. Only Ctrl-C ends it. The title reads 'Connecting xai' on the first attempt and 'Connecting grok-subscription' later, while the list row says 'Grok'.
  - repro: `Tui(fake=False, env={'SSH_CONNECTION':'10.0.0.1 22 10.0.0.2 22'}); dismiss wizard; '/login subscription' Enter; wait 'SuperGrok'; down; enter; wait('sign-in link', 40)`
  - code: crates/inference-gateway/src/gateway/subscription_broker/login.rs, `LoginOutput::read` (around lines 186-222). It only recognises "Visit the following URL" followed by an https line (the browser flow), "Codex device URL:" / "Codex device code:" (OpenAI's device flow), "Authentication saved to", and "authentication failed" / "[error". xAI's broker login is a device flow that prints "To authenticate

### Sign-in progress panel over SSH (device-code and link variants) (1)

- **Over SSH the 'open in browser' row is offered and preselected, and its advice points to a row that does not exist** — defect, loose-end
  - expected: Sterna already knows there is no browser, so it hides the open row or makes copy the default. Any advice names a row that exists.
  - actual: Device variant: Enter on '⏎ open the link in your default browser' says 'No browser available; copy the link.', but the panel only has '⏎ copy the code'; the link cannot be copied. Link variant: the dead 'open' row is the preselected default, so the first Enter does nothing useful.
  - repro: `Tui(fake=False, env={'SSH_CONNECTION':...}); '/login chatgpt' Enter (device variant) → down → enter. Separately '/login claude anyway' → the panel opens with '› ⏎ open the sign-in link in your default browser' selected → enter`
  - code: The panel rows are built in crates/sterna/src/session/controls.rs, `SignIn::render` (around lines 1113-1160):
- Both variants always push the "/open-link" row, whatever the environment.
- The device variant pushes only "⏎ copy the code" (`/copy {code}`) and has no "/copy {link}" row.
- The selection is `panel.selected = rows.position(|r| r.command.is_some())`, which is the first command row. In th

### Chat transcript after a sign-in link (1)

- **The sign-in link kept in the chat is truncated, cannot be clicked, and copying it gives a broken URL** — defect, bug
  - expected: Per the code comment, the chat keeps 'the whole link … so it can be read and selected after the panel is gone': it is shown in full or wrapped, and a click copies or opens it.
  - actual: The line ends '…&code_challe…' at the sidebar edge. A click does nothing (no OSC 52). Drag + Ctrl-C copies '…&code=true&code_chal', a truncated link that cannot work. The footer notice for it shows only 'Sign-in link for claude-max:' and stops at the colon.
  - repro: `Real mode, SSH_CONNECTION set: '/login claude anyway' Enter; wait for the panel; esc; click the 'https://claude.ai/oauth/authorize…' line; then drag across it and press ctrl-c`
  - code: crates/sterna/src/workbench/document.rs:1296-1310 renders each line of a chat note as `clip(line, width.saturating_sub(5))`, and `clip()` (document.rs:1866) cuts with '…'. It never wraps. So the note built in crates/sterna/src/session/controls.rs:1087 (`"Sign-in link for {account}:\n{link}"`, in SignIn::apply) loses most of the link, even though the doc comment at controls.rs:1077-1079 promises "t

### Sign-in progress panel title / notices (1)

- **The panel title and notices use internal ids instead of the name the person picked** — polish, copy
  - expected: 'Connecting ChatGPT', 'Connecting Claude', 'Connecting Grok'.
  - actual: 'Connecting chatgpt-subscription', 'Connecting claude-max', 'Connecting xai' / 'Connecting grok-subscription'; notices 'Sign-in to grok-subscription cancelled.', 'ERROR: signing in to claude-max failed …'.
  - repro: `'/login subscription' → Enter on 'ChatGPT' / 'Claude' / 'Grok'`
  - code: In crates/sterna/src/session/controls.rs, stream_connect (around line 930) sets `let account = declared.unwrap_or(match provider { "openai" => "ChatGPT", "anthropic" => "Claude", other => other })`. The friendly name is used only when no account is declared. When the gateway declares one (the normal case; the caller at about line 346 passes entry.account), the internal id wins. The id then feeds S

### Global: Ctrl-C with a selection on screen (1)

- **Once text is selected, Ctrl-C only copies and can never interrupt** — defect, keyboard
  - expected: Copying does not take away the interrupt: after one copy the selection clears, or a second Ctrl-C interrupts. Otherwise a hint says to clear the selection.
  - actual: All three presses show 'Copied selection.' and the cell keeps RUNNING. Ctrl-C is the only way to cancel a hidden sign-in or a turn, so a leftover selection silently disables it.
  - repro: `Fake mode with a slow helper cell running: drag across 'where is add defined?' in the transcript; key ctrl-c three times`
  - code: crates/sterna/src/workbench/input.rs lines ~369-376. The Key handler returns Effect::Copy(selected_text) whenever Ctrl-C is pressed and the selection is non-empty. The comment there reads "it never interrupts, and the selection stays". It returns before the `s.selection = None` line, so the selection stays and every later Ctrl-C copies again. The Copy effect is handled in crates/sterna/src/session

### /login custom endpoint form (1)

- **The 'http, not https: the key would travel unencrypted' warning carries a green check mark** — defect, visual
  - expected: A warning styled as a warning (e.g. '! http, not https …' in the warning colour).
  - actual: '✓ http, not https: the key would travel unencrypted' in the same green as '✓ a URL'.
  - repro: `Real mode: '/login custom' Enter; paste('http://10.0.0.5:8080/v1')`
  - code: crates/sterna/src/tui/form.rs base_url() (around lines 277-296) returns the http warning as Ok("http, not https: the key would travel unencrypted"). A check can only return Ok (pass) or Err (problem), so this warning has to go out as an Ok. crates/sterna/src/workbench/sheets.rs around line 659 then draws every Ok verdict as format!("✓ {praise}") with Tone::Success. A fix needs a third verdict kind

### /login key form (and custom endpoint key field) (1)

- **The hint promises 'right-click' to paste, but a right-click does nothing** — defect, mouse
  - expected: A right-click pastes, or the hint does not offer it. Sterna captures the mouse, so the terminal's own right-click paste does not happen.
  - actual: The field stays empty, with no feedback. The hint reads 'paste here: Cmd+V, Ctrl+Shift+V or right-click · Ctrl-R shows it'.
  - repro: `Real mode: '/login key' Enter; down, down, enter (anthropic); x,y = find('┃'); click(x+3, y, button='right')`
  - code: The hint text is in crates/sterna/src/session/controls.rs:619 in key_form(): "paste here: Cmd+V, Ctrl+Shift+V or right-click · Ctrl-R shows it". The mouse parser in crates/sterna/src/session/ui/terminal_input.rs:520 does decode a right-button press (MouseEventKind::Down(MouseButton::Right)), but nothing handles it. A grep across crates/sterna/src finds no handler for Down(MouseButton::Right). The 

### Settings search and Models picker search (1)

- **A pasted newline stays invisibly in the search query: Settings then matches nothing and says nothing** — defect, bug
  - expected: Control characters are stripped (as the key form and the Settings edit buffer do), so a pasted 'theme' finds Theme. An empty result says 'nothing matches'.
  - actual: Settings shows 'Search: theme' with an empty list and no message; typing 'theme' lists Theme and Voice. The third Backspace removes the invisible newline without changing the visible text. /models shows 'gpt6' but filters like 'gpt 6', and two Backspaces leave 'gpt', one of them spent on the hidden character.
  - repro: `Fake mode: f2; paste('the\nme'). Compare typing 'theme'. Real mode: '/models' Enter; paste('gpt\n6'); backspace x2`
  - code: crates/sterna/src/workbench/input.rs, the Event::Paste arm (around lines 344-358). The Settings edit buffer filters control characters: `v.push_str(&text.chars().filter(|c| !c.is_control()).collect::<String>())`. The two search queries do not: `p.query.push_str(text)` (Settings search) and `m.query.push_str(text)` (Models picker). Applying the same `!c.is_control()` filter to both fixes the hidden

### Settings field editor (number and list) (1)

- **A paste is appended to the pre-filled value, and the newlines of a pasted list are dropped** — defect, bug
  - expected: Editing starts with the value selected, so a paste replaces it. A multi-line paste into a list becomes list items.
  - actual: 'helpers.calls_per_cell = 812' → 'settings: `calls_per_cell` must be between 1 and 64'. 'web.allow_domains = []example.comdocs.rs' → 'settings: `web.allow_domains` is not a valid list: TOML parse error at line 1, column 11  |1 | value = []example.comdocs.rs  |' (raw, cut at the edge). The two domains are merged into one word.
  - repro: `Fake mode: f2; type 'calls'; enter; paste('12\n'); enter. Then f2; type 'domain'; enter (Allowed domains); paste('example.com\ndocs.rs\n'); enter`
  - code: crates/sterna/src/workbench/input.rs:344-352, the Event::Paste handler for the preferences sheet. When p.editing is Some it runs `v.push_str(&text.chars().filter(|c| !c.is_control()).collect::<String>())`, which appends to the pre-filled buffer. The filter drops '\n', so the list items join, and there is no split into list items. Editing starts with the current value already in the buffer (p.editi

### Models picker header (1)

- **'Main answers you. Now:' ends with nothing when no main model is set** — polish, copy
  - expected: 'Now: not chosen yet' or the line is omitted.
  - actual: 'Main answers you. Now:' followed by blank space.
  - repro: `Real mode (fresh isolated config): dismiss wizard; '/models' Enter`
  - code: crates/sterna/src/workbench/sheets.rs, draw_models (around lines 238-289): the roles array builds ("Main", "answers you", m.current.parent.clone()) with no fallback when parent is empty (Helper uses unwrap_or "off" and Subagents uses "favourites"). It then renders format!("{name} {purpose}. Now: {now}"), so an empty parent string gives 'Now: ' with nothing after it. Fix: map an empty parent to "no

### Sidebar ◇ HELPERS (1)

- **When the turn ends, the sidebar forgets the helper calls and says 'none yet'** — defect, inconsistency
  - expected: The sidebar lists 'find · returned' and 'reduce · failed' for the turn that just ran.
  - actual: During the wait it shows 'find · waiting'. After the answer it shows 'none yet', while cell 001 above still shows the find and reduce rows. The failed reduce never appears as 'failed' in the sidebar.
  - repro: `Fake mode with XDG config '[helpers] model="fixture-helper" enabled=true', fake cell calling helper.find (slow) and helper.reduce (fails); send 'where is add defined?'; wait 'EXECUTED'`
  - code: In crates/sterna/src/workbench/view.rs around line 926, the sidebar takes its helper rows from `n.cells.last()`: `let helpers = n.cells.last().map(|c| c.helpers.as_slice()).unwrap_or(&[]);`. When the turn ends, a second cell is appended to the turn ('SO FAR 2 cells' while the transcript shows only cell 001; it is likely the post-answer check that prints '! checked after the answer'). That cell has

### Sidebar ◇ HELPERS lines (1)

- **Sidebar helper lines cannot be clicked** — defect, mouse
  - expected: The click opens that helper's details in its cell, as clicking the lane row does.
  - actual: Nothing happens. The lines have no action (view.rs:958 pushes None), while 'off'/'none yet' in the same block do open Settings.
  - repro: `Same helper scenario; while waiting: x,y = find('find · waiting'); click(x+2, y)`
  - code: crates/sterna/src/workbench/view.rs, around lines 934-953, in the sidebar card's '◇ HELPERS' block. Each helper line is pushed as (format!("{} · {}", helper.helper, returned|waiting|failed), tone, None). Its action is None, so the hit loop below (`if let Some(action) = action` -> g.hits.push) never registers a hit rect for it. The 'none yet'/'off' line in the same block gets Some(Action::SettingsA

### Cell helper lane row / card while waiting (1)

- **Helper rows never say what was asked: 'Asked: 1 lines', 'scanning 1 lines', 'captured model unknown'** — polish, copy
  - expected: 'Asked: where is the add function defined?', plain wording for the wait, and the helper's model (known from config).
  - actual: The lane row reads 'find  scanning 1 lines · 1.9s · estimate unknown'. The card reads 'asked · no ETA, I'll say the moment it's back' and 'find · captured model unknown'. The expansion shows 'Asked: 1 lines' and 'Waiting for a returned value; no completed result yet.' After return: 'Captured model: fixture-helper'.
  - repro: `Helper scenario; while find is waiting read the card, then click the '⠸ find …' row`
  - code: 1. The "Asked" text comes from `asked_summary` in crates/sterna/src/runtime/bindings/helper.rs:192-194. It is `format!("{} lines", thousands(input.lines().count()))`, a line count on purpose, with no singular form. `helpers::bounded_ask` (src/helpers.rs:1184) already makes a 60-character first-line summary, but the binding path overrides it.
2. The lane row is built in src/workbench/document.rs:15

### Expanded helper row / Helpers tab (1)

- **Helper details dump internals and model-facing text, which breaks the layout** — defect, copy
  - expected: The question, the answer, and the cited lines, indented under the row.
  - actual: Six 'Observed: prepare: tree .', 'Observed: prepare omitted: task matches (no permitted textual matches)' lines, then 'Returned: …'. Next come raw model-facing markdown '## Excerpts (read from disk by Sterna just now; exact, no need to reopen these lines)' / '### src/main.py:1-2' / '1 | def add(a, b):', printed flush left at column 1 outside the row's indent. The Cell program tab shows the same with '✓ ### where'.
  - repro: `Helper scenario; after 'EXECUTED' click the '◇ find' row`
  - code: crates/sterna/src/workbench/document.rs, around lines 1601-1645 (helper row expansion). It pushes 'Asked: {h.asked}' (a count), 'Captured model', one 'Observed: {step}' per h.looked entry (internal prepare-trace steps), and 'Returned: {h.outcome.text}' as a single push. outcome.text already has the excerpt block from crates/sterna/src/excerpts.rs appended (HEADING const at line 26-27, '## Excerpts

### Failed helper row (1)

- **A failed helper reads 'request failed: request failed: io: Peer disconnected' under the label 'Returned:'** — defect, copy
  - expected: One plain failure sentence (e.g. 'reduce failed: the helper model closed the connection'), labelled as a failure.
  - actual: Row: 'reduce  request failed: request failed: io: Peer disconnected  0.0s'. Expansion: 'Returned: request failed: request failed: io: Peer disconnected'. Cell output: 'short: reduce failed: ToolError: request failed: request failed: io: Peer disconnected'.
  - repro: `Helper scenario (fake reduce request drops the connection); click the '◇ reduce' row`
  - code: 1. crates/sterna/src/helpers.rs:949 adds a second "request failed: " prefix. The code is `Err(err) => HelperOutcome::failed(format!("request failed: {err}"), started)`, and the Display impl for WireError::Http in crates/sterna/src/wire.rs:841 already writes "request failed: {err}".
2. crates/sterna/src/workbench/document.rs:1640 always writes `format!("       Returned: {}", h.outcome.text)`, even 

### Cell card with helper lane / Helpers tab (1)

- **Helper rows and the Helpers tab's content are drawn outside the cell card** — defect, visual
  - expected: Helper rows live inside the card frame. The Helpers tab puts its content inside the card body like the other tabs.
  - actual: The lane rows sit under the card's bottom border ('╰─ no captured file changes ─╯'), and their ▸/▾ toggle is at column 108, past the card's right edge. With the Helpers tab lit, the card body is empty ('╰─ ✓ executed · no captured file changes') and every helper detail is below the frame.
  - repro: `Helper scenario; look at the card while find waits; then click the card and its '⟨ Helpers ⟩' chip`
  - code: All in crates/sterna/src/workbench/document.rs.
- Around line 511, `d.helpers(cell, v, s, ui, width, id)` is called after the card's `RowKind::CardBottom` row has been emitted (about lines 502-508). It is outside the card block and passes the full `width`, not `inner`, which puts the ▸/▾ toggle at column 108, past the card edge.
- Around lines 458-467, the `CellTab::Helpers` match arm draws nothin

### F5 (Helpers tab key) (1)

- **F5 does nothing after the turn ends until a card is clicked** — defect, keyboard
  - expected: F5 opens the Helpers tab of the latest cell that had helpers.
  - actual: The card stays collapsed ('▸ 001') and the helper rows stay '▸'. F5 targets `n.cells.len()`, the answer, which has no helpers. After clicking the card, F5 works. F5 while the helper was still waiting did work.
  - repro: `Helper scenario; wait 'EXECUTED'; key f5`
  - code: crates/sterna/src/workbench/input.rs around line 674. With nothing selected, `KeyCode::F(5) => { let cell = self.selected_cell.unwrap_or(n.cells.len()); self.activate(Action::Tab(cell, CellTab::Helpers), ...) }` falls back to the last entry, `n.cells.len()`. After a turn that entry is the plain-text answer, not program cell 001, so Action::Tab (line 733) expands and tabs a cell that has no helpers

### Composer frame label (1)

- **The label says 'executing cell 002' while the only card on screen is 001** — defect, inconsistency
  - expected: 'executing cell 001', matching the card.
  - actual: '◒ executing cell 002' at 140x42 and 80x24.
  - repro: `Helper scenario (or any cell): send a prompt; while the cell runs read the composer frame label`
  - code: crates/sterna/src/workbench/view.rs, around line 1032: `let cell = matches!(s.activity, Activity::Executing | Activity::Streaming).then_some(n.cells.len() + 1);`. Its value goes to voice::status (crates/sterna/src/workbench/voice.rs:120-121, format!("executing cell {n:03}")). While a cell is Executing it is already in n.cells, because its card is rendered, so the +1 counts one too many. The +1 fit

### Completion check status line (helpers on) (1)

- **'✓ complete ✓' shows while the check is still running, with the bird pun 'a second bird is checking the answer…'** — defect, copy
  - expected: The turn is not called complete until the check ends, and the status is plain ('checking the answer…').
  - actual: The composer reads '✓ complete ✓' while '⠇ a second bird is checking the answer… · turn it off in /settings' still spins. A few seconds later '! checked after the answer: …' appears.
  - repro: `Helper scenario; right after the answer appears read the transcript and composer label`
  - code: workbench/voice.rs behind() (playful 'check' string) and voice.rs:146 Activity::Complete 'complete ✓'. The Complete label/completion_tick (session/ui.rs ~927, tui/ribbon.rs:236) ignores state.behind (tui/look.rs settling_or_behind), so it shows while the check lane is still running.

### Bare /cell (full-screen Inspection) (1)

- **Bare /cell opens an invisible modal that swallows history recall, cursor keys, PgUp and the first Esc-to-stop** — defect, bug
  - expected: Bare /cell either shows the latest cell's inspection or does what '/cell N' does (expand it in place); while nothing is drawn, keys go to the composer and Esc stops the turn on the first press.
  - actual: Nothing on screen changes and no notice appears, but state.inspection is set: Up, Left/Right, PgUp/PgDn/Home/End are eaten, the first Esc during a later running turn only closes the invisible view, and the wheel scrolls the transcript behind it instead.
  - repro: `t=Tui(fake=True,cols=140,rows=42); t.wait('What next',30); t.type('list the files'); t.key('enter'); t.wait('Nothing else has changed',20); t.type('/cell '); t.key('enter')  # (or '/cell', 'down', 'enter'). Then: t.key('up') -> composer stays empty; t.type('hello'); t.key('left',2); t.type('X') -> 'helloX' (control without /cell: 'helXlo'); with a long answer on screen t.key('pgup') -> nothing moves; t.key('esc') then t.key('up') -> '/cell' recalled. Mid-turn: slow the fake model, '/cell ' Enter, send 'do it again', press Esc once -> nothing; second Esc -> 'Stopping after this cell'. Wheel: t.`
  - code: The /cell handling is split across an old path and the new workbench.

- crates/sterna/src/workbench/input.rs `local_command` handles only `["/cell", number]`, which expands the cell in place. Bare "/cell" is not matched there.
- It falls through to the old branch at crates/sterna/src/session/ui.rs ~1834-1855. That branch calls `open_cell(&mut state, &notebook, Inspection::latest(..))` (ui.rs:725)

### Slash popup / bare /cell (1)

- **Typing '/cell' and pressing Enter runs /cells instead** — defect, inconsistency
  - expected: An exactly typed command runs as typed; the popup only completes a partial word.
  - actual: Enter replaces the text with the popup's highlighted first match, so '/cell' becomes '/cells' (expand all). With the cell already expanded nothing visible happens, and the bare '/cell' the popup documents can only be reached with Down+Enter or a trailing space.
  - repro: `After a turn with a cell: t.type('/cell'); t.key('enter'); then t.key('up') -> the composer shows '/cells' (the command that actually ran). The popup lists '/cells' first and '/cell' second.`
  - code: crates/sterna/src/session/ui.rs:663-668 (the KeyCode::Enter arm replaces self.text with slash_matches(...)[selected] even when the text is an exact match), together with crates/sterna/src/tui.rs:586 slash_matches (prefix filter in BUILT_INS order, where Cells comes before Cell)

### Transcript drag selection + terminal resize (1)

- **A drag selection survives a resize at stale coordinates; Ctrl-C then copies the composer frame** — defect, mouse
  - expected: A reflow clears the selection, or keeps it on the same text.
  - actual: The highlight stays on the same screen cells. At 80x24 they cover 'Paragraph 39…', the composer's top border and the footer hint. Ctrl-C copies 'Paragraph 39: the project notes line number 39.\n\n╭─ ✓ complete ✓ ── Copied selection. ───…╮\n│ ❯ What next? A message or / for commands…│\n╰─ ─── Ctrl-T opens the'.
  - repro: `LONG answer (39 paragraphs) on screen at 140x42; a=t.find('Paragraph 30'); b=t.find('Paragraph 32'); t.drag(a[0],a[1],b[0]+20,b[1]); resize to 80x24 (ioctl TIOCSWINSZ); t.key('ctrl-c'); decode the OSC 52 payloads from t.raw.`
  - code: Selection holds absolute screen cells (Selection { anchor, head } set from m.column/m.row in crates/sterna/src/workbench/input.rs around line 240). crates/sterna/src/session/ui.rs:1246, `Event::Resize(_, _) => { dirty = true; }`, only redraws and never clears the selection. Nothing under workbench/ handles Resize either. workbench/input.rs:222 `selected_text` then re-reads the new last-drawn scree

### Chips routed through Action::Command while a turn runs (1)

- **Mid-turn, the telemetry chip and the card's 'open diff ↗' are refused as a 'runtime change', though their key twins work** — defect, inconsistency
  - expected: Both chips do what their twins do: they open telemetry or show the diff. Neither changes the runtime.
  - actual: Both chips show 'This runtime change applies between turns; finish or stop the current turn first.' In the same moment Ctrl-T opens TELEMETRY, the '⟨ activity ⟩' chip beside it opens ACTIVITY, and F4 switches card 001 to its Changes diff. So the telemetry view cannot be reached by mouse during a turn, and the known finding says it has no mouse exit either.
  - repro: `Catalogue project; turn 1 with a write+answer() cell; slow(t,14); send 'next step'. While it is thinking (and again while a 14 s busy-loop cell executes): click '⟨ telemetry ⟩' in the sidebar; press Ctrl-T; Esc; click '⟨ activity ⟩'; click 'open diff ↗' on card 001; press F4.`
  - code: crates/sterna/src/workbench/input.rs:974-980. `Action::Command(cmd) => if busy { self.notice = "This runtime change applies between turns; …" }` blocks every Action::Command while a turn runs, without checking which command it is. Read-only views are routed through that same action: the sidebar telemetry chip (view.rs:985, `Action::Command("/telemetry")`), the card's 'open diff ↗' (view.rs:729, `A

### Cell card 'open diff ↗' (1)

- **'open diff ↗' on an earlier cell opens the latest cell's diff instead** — defect, bug
  - expected: Cell 001 switches to its '⟨ Changes +1 −0 ⟩' tab.
  - actual: Cell 001 stays on 'Cell program'. Cell 002, which has no Changes tab, switches to a diff view with neither tab lit, reading 'Observed changes · before → after this cell · already applied / No textual diff captured. This does not prove no files changed.' in orange.
  - repro: `Turn 1: write+answer() cell (changes notes.txt). Turn 2: read-only cell with answer(). p=t.find('001'); t.click(*p) to expand cell 1; p=t.find('open diff'); t.click(p[0]+2,p[1]).`
  - code: In crates/sterna/src/workbench/view.rs (around lines 719-731), the leftover 'open diff ↗' text on a cell's tab strip is wired as a button to Action::Command("/diff"). The button carries no cell index, even though the row's action is Action::Tab(cell, current). The '/diff' handler in crates/sterna/src/workbench/input.rs:60-71 always uses cell = n.cells.len(), the latest cell, and sets its tab to Ce

### Mid-turn refusals across chips, keys and Settings (1)

- **Five wordings for 'not during a turn', and the Main model row says the opposite** — polish, copy
  - expected: One refusal sentence that names what will happen and when. If the model cannot change mid-turn, the row description should not promise that it can.
  - actual: The five wordings: 'This runtime change applies between turns; finish or stop the current turn first.' / 'Change models after the current turn; in-flight calls retain their model.' (chip, F3) / 'Open model selection after the current turn.' (Enter on row) / 'Change models after the current turn.' (click on row) / 'Working. Your draft is kept; Ctrl-C interrupts tools; twice exits.' (typed /models). Right on the row does nothing at all. Meanwhile the row's own text says 'Takes effect on your next message; a call already in flight keeps the model it started on.' and the sheet header says 'every choice applies now'.
  - repro: `Mid-turn (slow fake model): click the sidebar telemetry chip; click the top-bar 'fixture-model ▾' chip; press F3; press F2 then Enter on 'Main model'; click 'choose a model'; press Right on that row; type '/models' + Enter.`
  - code: The strings are hard-coded separately at each entry point instead of coming from one shared busy-refusal message:
- crates/sterna/src/workbench/input.rs:976 (telemetry chip / runtime change)
- input.rs:810 (model chip / F3)
- input.rs:507 (Enter on the Main model row)
- input.rs:892 (click on the row)
- crates/sterna/src/session/ui.rs:1956 (the generic 'Working…' notice that swallows a typed /mode

### WORK sheet vs ASK sheet mid-turn (1)

- **WORK rows look live mid-turn but refuse; the neighbouring ASK rows apply at once** — defect, inconsistency
  - expected: Both sheets behave alike, or rows that cannot apply now are dimmed and say why before they are clicked.
  - actual: WORK opens with every row looking selectable, and a click or Enter gives 'This runtime change applies between turns…' in its footer. ASK changes the rung immediately (the top bar shows '⟨ Every call ⟩').
  - repro: `Mid-turn: click '⟨ Build ⟩' and click 'Explore' (or press Down, Enter). Then click '⟨ Auto-review ⟩' and click 'Every call'.`
  - code: crates/sterna/src/workbench/input.rs around line 974. WORK rows dispatch Action::Command(cmd), which checks `busy` and only sets `self.notice = "This runtime change applies between turns…"`. ASK rows dispatch Action::Rung(rung) (line ~981), which calls s.permissions.set(r) and closes the sheet with no busy check. The row rendering (the WORK/ASK sheet headers are at crates/sterna/src/workbench/view

### Notices carried into sheets (1)

- **A refusal from one chip shows up as the next sheet's own message** — defect, bug
  - expected: A newly opened sheet starts without the previous control's notice.
  - actual: The WORK sheet's footer reads 'Change models after the current turn; in-flight calls retain their model.', which looks like a statement about work modes.
  - repro: `Mid-turn: click the 'fixture-model ▾' chip (refused), then click '⟨ Build ⟩'.`
  - code: There are two causes. First, the Action::Models busy branch sets self.notice in crates/sterna/src/workbench/input.rs (around lines 808-811). Action::Work (around line 817) then calls self.close() and sets work=true without clearing self.notice, and Workbench::close() in workbench/mod.rs:159 does not clear the notice either. Second, the sheet footer in crates/sterna/src/workbench/view.rs:1363-1367 

### Ask panel ('pane is asking') (1)

- **The ask panel's key legend is black on black in every theme** — defect, visual
  - expected: '1-9 or ↑/↓ choose · Enter confirms · Esc: decide yourself' is readable. It is the only instruction on a panel that ignores the mouse.
  - actual: It is drawn with SGR 38;2;11;18;13 (#0b120d; neon #0a100d, mono #0d0e0f) on the default background, so it cannot be seen on a dark terminal. The approval modal's legend is bright green.
  - repro: `t=Tui(fake=True,cwd=project_with('[ask]\nenabled = true\n')); t.model.script=['ʼʼʼpane\nawait ask("Which file first?", ["README.md","src/main.py"]);\nʼʼʼ','Done.']; t.type('summarise'); t.key('enter'); t.wait('is asking'); inspect the cell colours of the line starting '1-9 or'.`
  - code: crates/sterna/src/tui/ask.rs:64 styles footer(request) with `Style::default().fg(theme.hush())`. In crates/sterna/src/tui/theme.rs:99, Theme::hush() is dock() halved: a background colour, 'the answer's own ground', which is how bands.rs:157/171/181 uses it as bg. For Neon, dock is (20,32,26), so hush is (10,16,13), and every theme's hush is near-black. The same misuse of hush as a foreground colou

### /permissions <rung> (1)

- **'/permissions manual' answers with a full-screen one-line sheet that eats what is typed next** — defect, bug
  - expected: A one-line notice (as Shift-Tab gives), with the composer still usable.
  - actual: A 'Permissions' sheet opens holding one '›'-marked line 'permissions: auto → manual (Shift-Tab cycles; 3 of the four rungs ask)'. The typed 'edit readme' goes nowhere, Enter does nothing, and after Esc the composer is empty and the model received 0 requests.
  - repro: `t.type('/permissions manual'); t.key('enter'); t.type('edit readme'); t.key('enter'); t.key('esc')`
  - code: crates/sterna/src/session/controls.rs:1471. The "permissions" arm sends the one-line result of a rung change (fn permissions, around line 1631) to show(Panel::text("Permissions", ...)), which opens a full-screen sheet, instead of a notice like Shift-Tab's (workbench/input.rs around line 990).

### Answer next-step chips (1)

- **The 'show the diff · commit this · full output' chips under the latest answer never appear** — defect, loose-end
  - expected: Under the latest answer's fact line '✓ 1 file · +1 −0 · 1 call', the chip row from the code: ⟨ show the diff ⟩ ⟨ commit this ⟩ ⟨ full output ⟩.
  - actual: The fact line is drawn but no chip row follows. find('show the diff') is None in every flow tried (answer() cell, return + text reply, return + empty reply).
  - repro: `Catalogue or plain fake project; t.model.script=['ʼʼʼpane\nawait write({path: "notes.txt", content: "hello\\n"});\nanswer("Wrote notes.txt with one line.");\nʼʼʼ']; send 'write notes'; wait; t.find('show the diff').`
  - code: crates/sterna/src/workbench/document.rs, build(). Around line 277 `last_assistant` is set to `c.messages.iter().rposition(|m| m.role == Role::Assistant && m.historical.is_none())`. At line 516 the answer block is drawn with `d.answer(..., last_assistant == Some(idx), ...)`, and the chips at line 751 depend on that `latest` flag. But crates/sterna/src/session.rs around line 1755 pushes a second, sy

### Settings sheet at small sizes (after live resize) (1)

- **Settings clips silently at 80x24 and overlaps itself at 50x15** — defect, visual
  - expected: Rows scroll with a 'more below' cue, and the list keeps priority over the blank detail rows.
  - actual: At 80x24: only Main model, Reasoning effort and Working mode show, with no cue that four more rows exist, while three blank rows sit in the detail pane. The effort row loses 'xhigh' and 'max' with no cue, and the picker legend ends 'Ctrl-O by intell'. At 50x15: no row name is visible, and text overlaps: '⟨ Global ⟩ ⟨ Project ⟩  F6 swit⟨ Undo · ^Z ⟩', 'a call already in flight keepsse', and a row description drawn below the key legend. Resizing back restores everything, so the layout itself is stateless.
  - repro: `t.type('/settings'); t.key('enter'); resize to 80x24, then to 50x15.`
  - code: The code is draw_settings in crates/sterna/src/workbench/view.rs, around lines 1633-1830.
- **Rows cut off with no cue:** the row count is `capacity = ((a.height.saturating_sub(14).max(2)) / stride).max(1)`, with stride 2, and nothing draws a 'more below' cue. The foot always reserves 7 rows (`bottom = a.bottom().saturating_sub(7)`) even when fewer are used, which leaves the blank detail rows.
- *

### Cell card program view (1)

- **The card garbles an answer() line: `answer("Wrote notes.txt with one line.");…")  · the answer is below`** — polish, copy
  - expected: The answer line is either shown verbatim or abbreviated cleanly.
  - actual: The full call is shown and then '…")' is appended anyway. A longer answer is cut mid-word: `answer("The README has a title and one sente…")  · the answer is below`.
  - repro: `Run the write+answer() cell above; look at card 001's program lines.`
  - code: The cause is in crates/sterna/src/workbench/document.rs, fn program (around lines 834-862). It takes `line[start+8..].chars().take(36)` as the head, and the remaining text of the line is not removed. So when the answer is 36 characters or shorter, the head already contains the closing `");` (and anything after it on that line), and then `…")  · the answer is below` is added regardless. When the an

### Answer text in the transcript (1)

- **Answer prose runs flush into the sidebar border, and its wrapped line starts one column further in** — polish, visual
  - expected: A gutter before the '│' and aligned continuation lines.
  - actual: '   The project has a README … Last commit: init.│' touches the border, and the next line is '    Nothing else has changed.' with a leading space kept from the wrap.
  - repro: `Default 140x42 fake session; send 'list the files'; read the answer lines.`
  - code: In crates/sterna/src/tui/markdown.rs, render() (around lines 324-337) word-wraps a paragraph with flow() at width.min(110). Here width is the conversation width minus 2, passed from conversation_lines in crates/sterna/src/tui.rs:1486. After that the answer row gets more leading columns: the answer band has a background, so render_conversation inserts a leading " " (tui.rs:1590), and there is also 

### /cell arguments (1)

- **'/cell abc' does nothing; bare /cell with no cells says 'at that number'** — polish, copy
  - expected: '/cell abc' explains that /cell takes a cell number, as '/cell 99' does ('No recorded cell at that number.').
  - actual: '/cell abc' produces no notice and no change. With no cells yet, bare /cell prints '· No recorded cell at that number yet. Use /cells after an action.' although no number was given.
  - repro: `After one turn: t.type('/cell abc'); t.key('enter'). In a fresh session: t.type('/cell '); t.key('enter').`
  - code: 1. crates/sterna/src/workbench/input.rs:73-85, the `["/cell", number]` arm of `local_command`: `if let Ok(cell) = number.parse::<usize>() {...}` has no else branch, yet the arm still returns true. A word that is not a number is swallowed with no notice. It needs an else branch that sets a notice such as "/cell takes a cell number, e.g. /cell 12".

2. crates/sterna/src/session/ui.rs:1845-1849: bare

### approval modal / composer (1)

- **Typing ahead answers an approval that pops up mid-word: an 's' in the draft allows the call for the whole session** — defect, keyboard
  - expected: A key typed before the approval was on screen is not an answer. The modal ignores input for a short time after it appears, or it needs a deliberate confirm, and the draft keeps every character.
  - actual: The approval appeared after 'also please make'. The next keystrokes ' ' and 's' went to the modal, and 's' is 'Allow this exact call for session'. secret.txt was written and the cell shows '✓ EXECUTED'. The draft lost both characters: 'also please makeure the tests still pass afterwards'. On the Every call rung, ordinary typing grants a session-wide approval.
  - repro: `config_home('[permissions]\nmode = "manual"\n'); fake model with pre_delay=1.5 answering a cell ʼawait write({path: "secret.txt", content: "x"})ʼ. t.type('write it'); t.key('enter'); then type 'also please make sure the tests still pass afterwards' one character every 80 ms (os.write per char). (script: gaps-2/scripts/s30_typeahead.py)`
  - code: crates/sterna/src/session/ui.rs, around lines 1345-1395, the `if let Some(request) = approvals.front()` branch of the key loop. The moment an approval is at the front of the queue, every key event goes to it. There is no arming delay counted from when the modal was first drawn, and no confirm step. `KeyCode::Char('s'|'S') if complete && key.modifiers.is_empty()` maps straight to Decision::AllowFor

### approval modal / session (1)

- **No surface lists or revokes calls allowed with [s] (or remembered denials); the session-permissions view is never built** — defect, loose-end
  - expected: Some place (the ASK sheet, /status or the sidebar) lists 'write · notes.txt allowed for this session' with a way to revoke it.
  - actual: /permissions shows only the four rungs and /status only the configuration. No screen mentions the allowance, which stays until the session ends. The later identical call runs with nothing on its card saying why it was not asked (cell 003 just reads '✓ EXECUTED').
  - repro: `Every call rung; a cell writes notes.txt; press s. Then t.type('/permissions'); Enter; and t.type('/status'); Enter. (gaps-2/scripts/s21_allowed_list.py)`
  - code: crates/sterna/src/approval.rs: Gate::session_actions() (line 429) has no caller. The `remembered` set filled by Decision::AllowForSession (lines 633-638) has no removal API, and admit() (line ~507) returns early for remembered actions without tagging the cell. The modal text is at crates/sterna/src/tui.rs:126, and the 's' key is handled at crates/sterna/src/session/ui.rs:1353. The /permissions she

### approval modal / selection (1)

- **Ctrl-C with a live selection denies (and bans) the call when an approval is up, instead of copying** — defect, keyboard
  - expected: Everywhere else Ctrl-C over a selection copies and never interrupts (input.rs:372), so it copies here too, or the selection is cleared when the modal opens so Ctrl-C clearly means stop.
  - actual: No OSC 52 is sent. The write is denied and the turn goes on. The selection and the 'Copied selection.' notice are still live under the modal when this happens.
  - repro: `Every call rung. After one answer, send 'write notes' (model delayed 1.5 s). Drag-select 'cargo test --workspace' in the earlier answer ('Copied selection.' shows), wait for the approval, press Ctrl-C. (gaps-2/scripts/s31_sel_approval.py)`
  - code: crates/sterna/src/session/ui.rs around lines 1346-1370: when `approvals.front()` is Some, the key match maps `KeyCode::Char('c')` with CONTROL straight to `INTERRUPT.store(true)` plus `Some(Decision::Deny)` (the redirect branch at ~1326 does the same). This runs before the workbench input handler, so the selection check in crates/sterna/src/workbench/input.rs:~370 ("Ctrl-C over a selection is a co

### approval modal (>16 KiB action) (1)

- **An action over 16 KiB can never be approved, and the modal hides which call it is** — defect, bug
  - expected: The modal still names the tool, the path and the size (a summary). The keys it disables say why when pressed. The model is told the call was too large to confirm, so it can split it. Writing a normal 20 KB source file is possible on the Every call rung.
  - actual: The body is only 'Action exceeds the 16 KiB confirmation limit. Approval is disabled; deny this call and ask for a smaller action.' It gives no tool, no path and no size. o/s/a/Enter do nothing and show no feedback. After d, the model gets the generic 'the host call gate denied or cancelled this exact attempt' with no word about size. Any write over 16 KiB is therefore impossible on this rung.
  - repro: `Every call rung; cell ʼawait write({path: "big.txt", content: "x".repeat(20000)})ʼ; at the modal press o, s, a, Enter, Down, PgDn, then d. (gaps-2/scripts/s07_big.py)`
  - code: - **The modal body:** crates/sterna/src/approval.rs:213-235, `Confirmation::new`. When the escaped JSON is over 16*1024 bytes, `text` is replaced wholesale by the fixed "Action exceeds the 16 KiB confirmation limit..." string. No tool, path or size summary is kept.
- **The footer:** crates/sterna/src/tui.rs:125-129 swaps the choices for "[d/Esc] Deny · ...". The key handler just drops o/s/a/Enter 

### approval modal (Jev hint line) (1)

- **The Jev hint is a bare, uninformed number: 'fits the request: 0.07 (decision, 2505 ms)', and the model never sees the call's arguments** — defect, copy
  - expected: The hint says in words what it means (for example 'looks unrelated to what you asked'), a low fit stands out, and no latency is shown. The score is computed from what the call does (tool, path, content or command).
  - actual: The line reads 'fits the request: 0.07 (decision, 2505 ms)' in the same green as the key legend, and 0.42 and 0.90 look identical. The decision request carries only {"request": "say hi", "summary": "write · exact action b662a9c8c860", "tool": "write"}. Jev never sees the path or the content, so the number cannot reflect the call it sits on.
  - repro: `config [decisions] mode="on", model="jev-latest", rung manual; fake /v1/systemone answers noul 0.07 after 2.5 s; t.type('say hi'); Enter; the cell writes notes.txt with content 'rm -rf everything'. (gaps-2/scripts/s08_hint.py, s09_hint_late.py)`
  - code: crates/sterna/src/approval.rs (~line 567-580, the decision `state` holds tool + summary hash only, no arguments; `summary()` ~line 190 is a display hash). crates/sterna/src/tui.rs (~line 130-139, the footer format string prints the raw score and latency, and the whole footer, legend included, is one ACCENT-styled Paragraph).

### approval modal (Auto-review) (1)

- **On Auto-review the approval never says why this call needs a person** — defect, copy
  - expected: The modal shows the reviewer's reason (the static reader computes 'deletes a directory', or the decision model's word), because Auto-review lets most things run and the person needs to know what made this one different.
  - actual: The modal is the same raw JSON and key legend as every other approval. No word on screen matches delet|directory|why|because|reason.
  - repro: `config [permissions] mode="auto", allow=["Bash(rm *)"]; the cell runs ʼbash({command: "rm -rf build"})ʼ. (gaps-2/scripts/s33_reason.py)`
  - code: The reason is computed and then dropped. In crates/sterna/src/permissions.rs, judge_command (around lines 350-368) builds Verdict::Ask(why), where why is the static reader's reason from sandbox::modes::command_reads_only or the decision model's reason(line). The doc comment on Verdict::Ask (line 162) promises "with this reason shown beside it". In crates/sterna/src/approval.rs:533, the match arm `

### approval modal (narrow) (1)

- **With the hint line on, the approval footer loses its last line at 80x24 and 60x20** — defect, visual
  - expected: All of the footer is visible: the hint, the choices and '↑/↓ PgUp/PgDn scroll · Expires after 10 min · Sandbox unchanged'.
  - actual: The footer is fixed at 3 rows. The hint takes one and the wrapped choices take two, so the scroll and expiry line is cut at both sizes. The modal also overdraws the composer's top border ('╭─│another way  [d/Esc] Deny').
  - repro: `Tui(cols=80, rows=24) and Tui(cols=60, rows=20) with decisions on, rung manual, a write cell. (gaps-2/scripts/s20_narrow.py)`
  - code: In crates/sterna/src/tui.rs, render_approval (around lines 73-140) sets `let footer_height = inner.height.min(3);` as a fixed 3 rows. The footer text is the hint line, then the choices line ('[o] Allow once ... [d/Esc] Deny', about 97 characters, which wraps to 2 rows below about 100 columns), then '↑/↓ PgUp/PgDn scroll · Expires after 10 min · Sandbox unchanged'. That text is rendered as a wrappi

### composer (mouse) (1)

- **Clicking in a wrapped draft puts the cursor 1 character too far left on line 2 and 2-3 on line 3** — defect, mouse
  - expected: The character lands where the pointer was, on every line.
  - actual: Line 1 is exact ('Y010'). On line 2, X lands before the preceding comma ('049X,050'). On line 3, Z lands 3 characters early ('06Z9,070'). CJK: clicking idx 80 inserts after 78 (expected 79), and clicking idx 140 inserts after 137 (expected 139). The error grows by one per wrapped line.
  - repro: `Paste the 300-char '%03d,' draft; click on the '0' of '050,' (line 2) and type X; click '070,' (line 3) and type Z. CJK: paste 150 CJK chars, click the exact column of idx 30/80/140 and type X. (gaps-2/scripts/s11_click.py, s24_cjk_click.py)`
  - code: crates/sterna/src/workbench/view.rs lines ~289 and ~494-505: the wrap width (textwidth-4) does not match g.composer.width (textwidth-5 when boxed). crates/sterna/src/workbench/input.rs lines ~270-300: the click-to-offset code re-wraps with geometry.composer.width, so its line breaks differ from the rendered ones. The same mismatch clips one character at every wrap boundary on screen.

### composer (unbracketed input) (1)

- **Multi-line text that arrives with LF line ends, without bracketed paste, is glued into one line** — defect, bug
  - expected: The typing_waiting rewrite (ui.rs:1820) keeps line breaks for LF as it does for CR, so the model receives 'a\nb\nc'.
  - actual: The LF burst is sent as 'abc': both line breaks are silently dropped, so the text changes meaning. The CR burst arrives correctly as 'x\ny\nz'. This is the same root as the known 'Ctrl-J is ignored', but here it corrupts sent text, for example from tmux send-keys, scripts or terminals without bracketed paste.
  - repro: `os.write(t.fd, b'a\nb\nc\r'); compare with os.write(t.fd, b'x\ry\rz\r'). (gaps-2/scripts/s14_burst.py)`
  - code: In raw mode crossterm reads byte 0x0a as KeyCode::Char('j') with CONTROL, not as KeyCode::Enter. The rewrite in crates/sterna/src/session/ui.rs around lines 1820-1826 (`if key.code == KeyCode::Enter && key.modifiers.is_empty() && input.typing_waiting()?` turns Enter into ALT+Enter, which inserts a newline) only fires for KeyCode::Enter. The editor's key handler, Editor::key in session/ui.rs at lin

### conversation chrome (Jev decisions on) (1)

- **With Jev decisions on, raw classifier output is printed in the dock edge and the transcript** — defect, copy
  - expected: Plain language, or nothing, unless the person opens the instruments.
  - actual: The dock's top edge reads '◓ executing cell 001 ── decision: intent modify (0.40), complexity needs_exploration (0.40), 1 ms', and a transcript line reads '· decision: inten…', cut off by the modal. When the endpoint does not answer it reads '· decision: no answer (timeout after 15000 ms)'.
  - repro: `config [decisions] mode="on", model="jev-latest" with a fake /v1/systemone; send any prompt. (gaps-2/scripts/s08_hint.py)`
  - code: crates/sterna/src/session/system.rs:488-505, `PendingDecision::settle`. It calls `session_println!` with "decision: intent {} ({:.2}), complexity {} ({:.2}), {} ms" and with "decision: no answer ({error})". The session_println output goes into the transcript, and the latest line is also mirrored into the dock's top-edge status while the turn runs. The same raw-print pattern is in:
- session/return

### Transcript file paths (1)

- **Path underlines are cached for the session: a new file never becomes clickable, a deleted one stays underlined** — defect, bug
  - expected: A path is underlined while the file exists: src/later.py gains an underline once created, src/main.py loses it once deleted, and the answer 'Created `src/new.py`' underlines the file that was just written.
  - actual: After redraws the underlines are unchanged: [(37,10,'src/main.py'),(34,24,'src/main.py')], with src/later.py never underlined. In the second case, because my message named src/new.py before it existed, the answer 'Created `src/new.py` with `sub`.' has no underline at all (underlines == []). The same answer without the earlier mention is underlined. So any path a person names before the model creates it can never be clicked in that session.
  - repro: `env -u SSH_CONNECTION; t=Tui(fake=True,cols=140,rows=42); t.wait('What next',15); t.type('please look at src/later.py and src/main.py'); t.key('enter'); t.wait('Nothing else has changed',30); create <cwd>/src/later.py; os.remove(<cwd>/src/main.py); t.scroll(40,20,up=True); t.scroll(40,20,up=False); t.key('ctrl-b'); t.key('ctrl-b'); underlines(t). Second case: script [WRITE_ANSWER-like cell writing src/new.py, 'Created ʼsrc/new.pyʼ with ʼsubʼ.']; t.type('add a sub function in src/new.py'); t.key('enter')`
  - code: crates/sterna/src/tui/paths.rs, fn exists() and its thread_local `KNOWN: RefCell<HashMap<String, bool>>`. The first stat result for each candidate string is stored for good. The map is only cleared when it passes REMEMBERED = 1024 entries, and nothing clears it when a turn writes or deletes files. A negative answer ("does not exist") is cached the same way as a positive one. The key is the bare ca

### Transcript file paths: click notice (1)

- **A path click that cannot open says 'No application could open this file.' whatever the real reason** — defect, copy
  - expected: The notice names the real cause and a way forward. Over SSH: something like 'Can't open files over SSH; path copied' (the link equivalent says 'No browser can be opened here: copy the link instead.'). For a deleted file: 'src/main.py no longer exists.' A failure should not look like a success.
  - actual: Every case gives the same '── No application could open this file. ──', drawn in the accent green used for success, with no fallback such as copying the path. A directory path would also be called 'this file'. Right-click on the path does nothing, so over SSH the only way to get the path is a drag-select.
  - repro: `t=Tui(fake=True,cols=140,rows=42,env={'SSH_CONNECTION':'1'}); t.wait('What next',15); t.type('what is in this project'); t.key('enter'); t.wait('Nothing else has changed',30); x,y=t.find('src/main.py'); t.click(x+2,y). Then delete src/main.py and click it again, with or without SSH.`
  - code: The cause is in crates/sterna/src/session/ui/links.rs, where `show(path) -> bool` (lines 50-76) returns false for three different reasons: over SSH (SSH_CONNECTION/SSH_TTY set), `!path.exists()`, and an open/xdg-open/explorer command that fails. Returning a bool loses which reason it was. The caller, crates/sterna/src/session/ui.rs:1175-1183 (the `Effect::OpenPath` branch), maps false to the fixed

### /handlers panel: turning a handler off (1)

- **Turning a handler off leaves it saying 'active', and a second press queues a duplicate** — defect, bug
  - expected: After the first Enter the row shows the request pending (for example 'watcher · turning off…') and loses its action. A second Enter does nothing or says it is already requested. The transcript records the handler going off once.
  - actual: After Enter the row still reads '› watcher · active · 0 runs · 0 drained' (reopening /handlers shows the same), and only the sheet footer says 'Handler watcher: cancellation requested.' A second Enter queues the same name again, and the transcript prints '· handler watcher: off' twice. A click on the row behaves exactly like Enter.
  - repro: `t=Tui(fake=True,cols=140,rows=42,env={'SSH_CONNECTION':'1'}); script ['ʼʼʼpane\nconst watcher = on({kind: "hook.*"}, "return 1;");\nreturn "registered";\nʼʼʼ', ('ʼʼʼpane\nreturn "second";\nʼʼʼ', 10), 'All done.']; t.type('watch hooks'); t.key('enter'); t.wait('registered',20); t.type('/handlers'); t.key('enter'); t.key('down'); t.key('enter'); t.key('enter'); t.key('esc'); t.wait('All done',30)`
  - code: crates/sterna/src/session/ui.rs:1226-1228: the Effect::Command branch for `/handlers off <name>` pushes the name onto `handler_cancellations` and sets the notice. It does not check whether the name is already queued and does not mark the row pending. The same push also happens at ui.rs:1658 and ui.rs:1925, which are the other key/click paths. The queue is only drained at a cell boundary in crates/

### /handlers panel: lifecycle and copy (1)

- **Handlers vanish at turn end without a word, and one action goes by four names and jargon** — defect, copy
  - expected: The panel says a standing handler lasts until the current request finishes, and when the turn ends under an open panel it says so (for example 'The turn ended; its handlers ended with it'). A handler the person turned off reads 'off'. One plain wording is used for the action. The panel does not hide that the turn is still running.
  - actual: (a) The full-screen sheet covers the transcript and the '⠙ thinking…' status for the whole turn. When the turn ends, 'watcher · active' silently becomes '› No handlers in this task.' and the cursor jumps to it. Afterwards, idle, '/handlers off watcher' answers 'handler watcher: no active handler with that name' for a handler listed seconds before. (b) The same action appears as 'Handler watcher: cancellation requested.' (panel), 'handler watcher: cancellation queued for the next cell boundary' (typed), and 'handler watcher: off' (result). A handler the person turned off is then listed as 'watcher · stale · 0 runs · 0 drained'. The header 'Task-scoped · /handlers off <name>' tells the person to type a command even though Enter and click work. The sheet opens with the cursor on that non-actionable header row, so the first Enter does nothing.
  - repro: `(a) Script [HANDLER cell, ('All done.', 6)]; t.type('watch hooks'); t.key('enter'); t.wait('registered',20); t.type('/handlers'); t.key('enter'); t.key('down'); time.sleep(8). (b) Same with 3 replies; t.type('/handlers off watcher'); t.key('enter') while busy; after the next cell, t.type('/handlers'); t.key('enter'); after 'All done', t.type('/handlers off watcher'); t.key('enter')`
  - code: - **Sheet:** crates/sterna/src/tui.rs:916-942, `handlers_panel`.
  - It picks the header text 'Task-scoped · /handlers off <name>' or 'No handlers in this task.' and adds that header as the first row, so the cursor starts on a row that has no command.
  - Each handler's status is `if h.active {"active"} else {"stale"}`, with no 'off' or cancel-requested state.
  - The row's command is `/handlers o

### Cell card: diff tab (1)

- **The file name in the Changes tab is not clickable, though the same path in the answer is** — defect, mouse
  - expected: The diff's file header 'src/new.py', which is where a person looks to open the changed file, is underlined and opens like every other path.
  - actual: The header is not underlined and a click does nothing (no notice). The only underline on screen is the 'src/new.py' in the answer prose below the card. The call row '✓ write   new.py' shows only the basename.
  - repro: `Script [WRITE_ANSWER]; t.type('add a sub function'); t.key('enter'); t.wait('complete',30); x,y=t.find('Changes +2'); t.click(x+2,y); x,y=t.find('src/new.py'); t.click(x+2,y)`
  - code: In crates/sterna/src/workbench/view.rs, the row painter's spans_at closure (around line 625) calls paths(f, g, ...) only in the else-branch, the one for rows with empty r.spans (plain text). paths() is what underlines a path and pushes Action::Path into g.hits. Card rows are built from spans, so they take the first branch and never get path detection.

In crates/sterna/src/workbench/document.rs, f

### Cell card: Full output tab (1)

- **Full output shows an empty 'Handles' heading and leaves out the answer** — polish, loose-end
  - expected: Only sections with content are shown. 'Full output' holds everything the cell produced, including the answer text.
  - actual: The body is '  Observed calls' / '    └─ write new.py · returned' / '  Handles' followed by a blank line. The heading has nothing under it, and the cell's answer 'Added sub to src/new.py.' is not in its own full output. The same happens idle and during the next turn.
  - repro: `Script [WRITE_ANSWER]; t.type('add a sub function'); t.key('enter'); t.wait('complete',30); x,y=t.find('Full output'); t.click(x+2,y)`
  - code: In crates/sterna/src/workbench/document.rs:443-455 (CellTab::Output), the renderer draws a heading for every field that is Some(..) and never checks whether the value is empty. In crates/sterna/src/session.rs:2207 the view is built with `table: Some(turn.table.clone())` even when the handle table is empty. stdout on the next line has an `(!is_empty()).then(..)` guard, and table has none. Lines 208

### Cell card title (1)

- **Card title says '1 lines'** — polish, copy
  - expected: '002 · 1 line'
  - actual: '▸ 002 · 1 lines ✓ EXECUTED' (and '╭─ 002 · 1 lines ─' when open)
  - repro: `Script [HANDLER cell, ('ʼʼʼpane\nreturn "second";\nʼʼʼ', 10), 'All done.']; t.type('watch hooks'); t.key('enter'); t.wait('All done',30)`
  - code: crates/sterna/src/workbench/document.rs:394 has `let size = format!("{} lines", program_now.lines().count());`, which never uses the singular. This size is the card's fallback description when the model gave none. Line 1065 has the same unpluralised pattern ('{} lines so far' while a cell is being written) and may show '1 lines so far'. I did not observe that one.

### Composer after an Insert chip (Effect::Insert) (1)

- **Ctrl-Z does not bring back a draft that an Insert chip overwrote** — defect, keyboard
  - expected: Ctrl-Z (or an 'undo' chip beside the notice, like the Settings live-undo) restores the draft the chip replaced. 'commit this' is also an Effect::Insert and would destroy a draft the same way if it rendered.
  - actual: After the click the composer reads '❯ Review my uncommitted changes and tell me what is unfinished.' Ctrl-Z changes nothing, shows no notice, and the typed draft is gone for good. This adds the missing recovery to the known 'chip silently replaces the draft' finding; the composer has no undo at all (the only Ctrl-Z handler is the settings panel's, input.rs:479).
  - repro: `t=Tui(fake=True,cols=140,rows=42,env={'SSH_CONNECTION':'1'}); t.wait('What next',15); t.type('my careful draft about the parser bug'); x,y=t.find('review 1 uncommitted'); t.click(x+3,y); t.key('ctrl-z')`
  - code: crates/sterna/src/session/ui.rs:1169-1175: the Effect::Insert arm overwrites editor.text without saving or restoring it. The composer has no Ctrl-Z; the only one is the settings panel's, at crates/sterna/src/workbench/input.rs:479.
