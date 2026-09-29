# The runtime

What a tool result becomes, what the model is shown of it, how long it
lives, how it survives a resume, and what happens when a program throws.
Code: `crates/sterna/src/runtime/`. The bytes the model receives are the
[model contract](model-contract.md)'s.

## Cells

The model acts by writing one TypeScript program per turn: a **cell**. All
cells of one user request share one persistent scope in one V8 isolate,
REPL-style: a top-level `const`, `let`, `function` or `class` from cell *n*
is in scope in cell *n+1*. A new user request starts a fresh runtime.

A cell is parsed and checked before anything runs; a parse error runs
nothing. Tools are async functions inside it, so one cell can read several
files, search, edit, run the tests and branch on what came back. The model
yields when the next decision needs evidence that does not exist yet.

## Handles

**A handle is a binding the model named.** `const hits = await grep(…)`
makes the handle `hits`. There are no generated ids. Three things become
handles: a top-level binding, the value a cell yields, and an object the
program passes to `keep(name, value)`.

**Redeclaring replaces.** The earlier object is freed at once and the next
table shows `hits  (replaced at cell 4)`.

**Lifetime.** A handle lives until the model redeclares it, calls
`free("name")`, or the request ends — nothing else. Nothing is evicted by
an LRU, a heap watermark or a timer: a handle vanishing under a program that
still names it would make the whole channel untrustworthy. When the
isolate's heap crosses its ceiling the *cell* fails with
`RuntimeOutOfMemory`, and the error lists the five largest live handles so
the model can choose what to free. `handles()` lists them all.

## Previews

Every live handle is one entry in the handle table, shaped by its type and
then bounded (`runtime/preview.rs`):

| type | preview |
|---|---|
| `T[]` | `n=<len>`, then elements `[0] [1] [2]` and `[len-1]`, each cut at 120 characters |
| `File` | path, bytes, line count, mtime, then lines 1–2 — never the contents |
| `string` | `len=<chars>`, the first 200 characters, then `…(+N chars)` |
| number, boolean, null | the value |
| other objects | up to 12 key names with each value's *type*, never its value |
| `Error` | class, message cut at 200 characters, and the top three frames inside the model's program |

Two ceilings, in estimated tokens (characters / 4):

- **256 per preview.** A preview over it shrinks by its own rule first —
  element and key counts step down — before any string is cut.
- **2,048 for the whole table.** Over it, the oldest entries are left out of
  the rendering, never freed, with one line naming how many.

`console.log` output shares a per-cell budget of 8,192 estimated tokens;
each argument is bounded to 24 KiB of characters, and a cut keeps a true
tail and says what was omitted. Getters and proxy traps are not invoked by
inspection.

`crates/sterna/tests/handles.rs::a_grep_of_122kb_costs_under_300_tokens_and_survives_one_yield`
generates a tree whose grep output is over 122 KB and fails if the rendered
table costs 300 tokens or more.

## Ending a task

Falling off the end of a cell, calling `yieldNow(reason)`, or a top-level
`return` all hand back: the model gets the results and another turn.
**A `return` never ends the task, whatever its type.** The returned value is
notebook output: shown to the person, recorded on the cell and sent as
`## Output`.

**`answer(text)` is the only terminal response.** It ends the cell where it
is called, renders the text in the conversation and records it as the
assistant's turn. A prose reply with no `execute_cell` call also ends the
task. A cell that threw ends nothing, whatever it answered before failing.

**A returned value is paged, never cut.** The isolate reads it within 1 MiB
and keeps an object's top-level fields apart: a string as text, an array of
strings as lines, a tool object as its preview, anything else as JSON.
Fields share the turn's return budget (see the model contract §6): small
ones whole, large ones levelled with a 600-token floor, and a field over its
share ends at a line boundary with one cursor line saying how to read on —
`.excerpt({start, lines})` for a file, `.slice(n)` for an array. No field is
ever replaced by its type name.

**No return exceeds its budget, whatever its shape.** A line longer than
the whole page is cut inside itself and the cursor says how much is missing
and which `.slice(n)` holds it; JSON the walk stopped inside is split one
record per line so it pages like any array; a return of many fields names
the ones left out once the budget is spent. Free text keeps its last lines
after the cursor, since a log's verdict is at its end, and the cursor names
the hidden lines that look like failures (`error`, `panic`, `FAILED`,
`warning:` …) with the `.split('\n').slice(a, b)` that reads around the
first. A thrown message is capped the same way in `## Error`: its start and
its end, and how many characters are between them.

With a decision model configured, `helpers.reduce_returns` (on by default)
lets it read a large field as a log and send it to the reducer first — the
whole value stays bound — and `helpers.prefetch_returns` (off by default)
follows a return that names unread project files with those files
([decisions](decisions.md)).

## Throws

A cell that throws produces, in the slot a yield would have used: the error
preview, the line and column inside the model's program, and the handle
table **as it stood after the last statement that completed** — bindings
made before the throw persist, so recovery is one cheap cell. No host
frames, no tool payload.

**Nothing is retried automatically.** A retry would re-run side effects the
runtime cannot know are idempotent. A refused call (`PermissionDenied`, see
[sandbox](sandbox.md)) is an ordinary throw: catchable inside the program,
never a prompt.

**An untaken branch never ran.** The runtime never evaluates both arms of a
guard or prefetches a call to see what it would return.

**The limit, stated.** A program can catch a failed call and answer a
confident sentence anyway; no mechanical check detects that. What Sterna
guarantees instead is the trajectory below: every call that ran is recorded
and shown, so an answer that does not follow from it is visible.

## The rollout

Every session appends to `.sterna/sessions/<id>.jsonl`: one line per cell
with its source, outcome (`yielded`, `returned`, `threw`), handle previews
and provenance, and the calls that actually ran — tool, arguments *as
checked* (the resolved path the child was given), and how each ended (ok,
threw, or denied with the deciding rule). It records programs and previews,
never objects. Data interpolated into an answer is text; nothing in it is
executed.

## Resume

`sterna --resume` replays no cell: a program that deleted a branch would
delete it twice. Every handle comes back **stale** — listed, marked
`stale`, and throwing `StaleHandle("hits")` with its recorded call so the
model can re-derive it in one line.

The exception is a handle whose recorded call names a tool that declares
itself **pure** (`read`, `grep`, `glob`, …). It re-materialises on first
access by re-running that exact call; if the result's SHA-256 matches the
recorded one the handle is live again, otherwise it stays stale and the
message says the tree moved.

## Reading on

- [tools](tools.md) — the tools a cell calls, and the interfaces that expose them
- [events](events.md) — background jobs, subagents and the `batch` handle
- [observing](observing.md) — the live event stream beside the rollout
