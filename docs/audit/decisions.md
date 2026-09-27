# Decisions on the audit's open questions

The user answered 1, 2 and 7 on 2026-09-27. The other nine take the audit's
recommendation; the user saw them and did not object (2026-09-27: "do these").

| # | Question | Decision | By |
|---|---|---|---|
| 1 | What does Esc do on an approval? | **Deny this one call, not remembered.** Esc means "not now", as everywhere else. Choosing Deny explicitly is a separate choice. | user |
| 2 | Does a refusal stay remembered for the session? | **Yes, but visibly.** The card says "you denied this earlier"; a sheet lists every session allowance and denial with a Forget chip (wire `Gate::session_actions()`). | user |
| 3 | What does the mouse wheel do inside a list? | Scroll the view; the focused row stays put. | recommendation |
| 4 | How does a mouse user preview a theme? | A click applies it at once and the sheet stays open; Esc keeps what is current, Undo goes back. | recommendation |
| 5 | Hover? | On: any-motion mouse reporting (?1003), redraw only when the target under the pointer changes. | recommendation |
| 6 | Where are first-run and model choices saved? | Global settings. The project file is written only when Project scope is chosen in Settings. | recommendation |
| 7 | Auto rung, a command that matches no allow pattern? | **Call-level judgement: read-only commands run, anything else asks.** No hard deny for a miss. | user |
| 8 | Messages typed while a turn runs? | Held in Sterna's queue until the turn ends, so Esc can take the last one back, as the hint promises. | recommendation |
| 9 | Changing the model or mode during a turn? | Allowed; applies from the next request, and the notice says so. | recommendation |
| 10 | Light-background terminals? | Detect automatically (OSC 11 query and COLORFGBG), with a `ui.background = auto\|dark\|light` override. | recommendation |
| 11 | One name per concept? | The chip words everywhere (permission rungs: Every call / Commands / Auto-review / Never asks; work mode: Build; subagent picks: favourites; the live view: Telemetry), with the config keys still accepted as typed arguments. | recommendation |
| 12 | The resume picker? | A normal Sheet inside the TUI, with the conversation behind it. | recommendation |

Also decided along the way:
- A subscription sign-in that opens the browser confirms first ("this opens your browser to sign in to ChatGPT"); nothing opens a browser on a single click.
- Copy is plain: no bird puns anywhere. Nothing is black on black: on terminal black a black plumage part uses slate (`#4B5663`) and dim text is at least `#7F8C98`.
