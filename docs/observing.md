# Watching a session

What a program reads to see what a session is doing **now**. Code:
`crates/sterna/src/observe.rs`.

## Why not the rollout

The rollout (`.sterna/sessions/<id>.jsonl`) is complete and late: a cell's
line is written when the cell has finished. A reader can say what a session
*did* and never what it is *doing* — it cannot tell a working cell from a
hung one. The event stream answers that: one JSON object per line, appended
and flushed as each transition happens, beside the rollout as
`<id>.events.jsonl`.

It is on by default and it can never fail a session: a destination that
cannot be opened leaves the session unwatched and running; a failed write
is dropped. Payloads stay out — a diff, a file's bytes or a command's
output are in the rollout, whose path `session.begin` carries.

## The envelope

| field | meaning |
|---|---|
| `kind` | one of the kinds below, always `noun.verb` |
| `at` | ISO-8601 UTC with milliseconds: when the runtime accepted the transition |
| `source` | `session/<id>` |
| `span` | the id that pairs an opening with its close |

A **span** opens and closes, its halves sharing one `span`: `session`,
`task`, `turn`, `cell`, `command`, `helper`, `agent`. A **moment** happens
once. A reader takes a duration by pairing, never by parsing prose.

## The kinds

| kind | carries |
|---|---|
| `session.begin` / `session.end` | the rollout path and project root |
| `task.begin` / `task.end` | the task, mode, permission rung and the model about to be asked; why it ended |
| `turn.begin` / `turn.end` | the turn number |
| `cell.submit` | the whole program (cut at 2,048 bytes), the command lines it certainly runs, the cell number, its description |
| `cell.repair` | the parse error and the amendment |
| `cell.end` | outcome and calls |
| `command.judge` / `command.end` | a command about to run, with its arguments; how it ended and its exit code |
| `file.change` | the path |
| `helper.begin` / `helper.end` | the helper's name |
| `agent.begin` / `agent.end` | the subagent |
| `answer.propose` | the text that would end the task, before it has |
| `ask.raise` | the question put to you |
| `approval.raise` | what is being confirmed |
| `supervisor.look` | the verdict |
| `ladder.move` | the permission rung, from and to |
| `reduction.made` | what was reduced and by how much |

`cell.submit`'s `cell` is the rollout's own cell number, so the two files
correlate without translation. The unit is the cell, not the call: a reader
that sees the whole program sees the intent, where one that sees a single
call sees only a step. `command.judge` is the one call-level event, because
a command line built from a variable is not knowable until it runs.

Seven kinds are marked as ones a future hook could refuse (`cell.submit`,
`cell.repair`, `command.judge`, `agent.begin`, `answer.propose`,
`ask.raise`, `ladder.move`). Nothing waits for a verdict today; the stream
only reports.

## Redaction

A value with a known provider-key prefix whose tail clears the entropy bar
of `scripts/check-secrets.py` is replaced with `«redacted»`. A value over
2,048 bytes is cut on a character boundary and states its real size. The
file carries command lines by design; a command with a secret in its argv
puts it here, as it already puts it in the rollout.
