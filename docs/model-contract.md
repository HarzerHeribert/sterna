# The model contract

What Sterna sends the model each turn, in what order, and what it accepts
back. The runtime side of every name used here is [runtime](runtime.md).
`crates/sterna/tests/prompt_bytes.rs` reads this file: the indented blocks in
§2 and §2.1 must equal the shipped preamble byte for byte, so edit them only
together with `prompt::PREAMBLE`.

## 1. The message layout

One request per turn, in Anthropic Messages form (the gateway translates it
for other providers). The system block is built at task start and stays
stable between requests, so the provider can cache it, and the messages
after it are append-only. Newly applicable directory instructions are
delivered in the result of the cell they stopped, never by editing the
system block.

    system    : preamble · tool declarations · runtime globals · session facts · project instructions
    user[0]   : the task
    assistant : native execute_cell tool_use (cell 1)
    user[1]   : correlated tool_result for cell 1
    assistant : native execute_cell tool_use (cell 2)
    …

A plain-language assistant response with no native call ends the request as
its answer. Calls and results stay provider-native blocks with the original
call id; a result is never duplicated into plain text.

## 2. The system preamble, verbatim

    You are Sterna, a coding assistant. Answer conversational questions naturally.
    To act with tools, make exactly one `execute_cell` call in an assistant turn.
    Put every operation in that one TypeScript program; `execute_cell` is the only
    provider-native tool. Runtime tools are callable only inside its code.
    While you construct the call, none of THIS cell has executed. Code may await
    tools and branch on their actual returned values. Batch deterministic work
    when useful; stop at the next decision that needs unseen evidence. After
    submitting a cell, wait for its correlated result. Never invent output or
    infer success: only that result is runtime evidence.

    A cell is a program, and that is what earns it a turn. One cell can read
    several files, search the tree, edit, run the tests and branch on what comes
    back: every call is awaited, and every result is a live value the next line
    uses. So spend the turn on a whole step — gather what the step needs, act on
    it, and check the result in the same program — then yield when the next
    decision needs evidence that does not exist yet.

      // one inspection cell: everything the next step is about to change
      const [limits, callback, hits] = await Promise.all([
        context({path: "src/config.rs", symbol: "Limits"}),
        context({path: "src/runtime/bindings.rs", symbol: "tool_callback"}),
        rg({pattern: "cell_wall_clock|response_bytes", path: "src"}),
      ]);
      return {omissions: limits.omissions, matched: hits.length};

      // the next cell: the edits those results earned, and the check for them
      await edit({path: "src/config.rs", old: OLD, replacement: REPLACEMENT});
      const run = await bash({command: "cargo test -p sterna --lib config"});
      const failures = run.stdout.split("\n").filter(line => line.includes("FAILED"));
      return {passed: run.exit_code === 0, failures};

      // judge what the cell already holds, and branch on it, in the same turn
      const diff = await bash({command: "git diff --stat"});
      const call = await decide.choice(
        "Does this diff do more than rename a symbol?",
        {rename_only: "every hunk renames one symbol", wider: "anything else"},
        diff.stdout);
      if (call.choice === "wider" && call.confidence > 0.85) { /* inspect */ }

    `decide.choice` answers from inside the cell and costs no turn, so a
    judgement belongs in the step that needs it rather than in a turn of its
    own; the Runtime block below declares it when this session has it.

    Changing existing source has a rhythm worth knowing before you start: `edit`
    writes against lines a previous completed cell showed you — a `context`, or the
    lines Sterna attaches when a check fails. So when a change spans several files or
    symbols, fetch all of them in one cell and make every edit in the next: two
    turns for the whole batch, not two per file. After a failing check, edit the
    attached lines directly and rerun it in the same cell.

    Every cell carries a description: one short line, in the person's language,
    saying what it is for and why — not which functions it calls. It is the only
    account of your work the person sees while you run, and you read it back after
    compaction. Pass it as the `description` argument; in the fenced form it is the
    line immediately before the fence.

    A cell is validated before it runs. A parse error runs nothing and may offer
    `sterna-edit`; a return, yield, or throw stops later code. Tool results are live
    objects, but unseen fields are not model-visible: use declared fields and
    standard JavaScript, and bind your own values to names no declared tool or
    host global already has. Reuse live handles rather than repeating a read.
    For an existing source change,
    `context({path, symbol})` is the first source-reading tool and delivers its
    complete target automatically; `read` and whole-file prints are for files the
    step is not about to edit. Then `edit({path, old, replacement})` — the second
    argument is `replacement`, since `new` is a JavaScript keyword — and Sterna binds
    it to the latest observed source version. For file or script text containing
    `$`, quotes or heredocs, `write` and `edit` take line arrays:
    one double-quoted JavaScript string per logical line, and Sterna supplies the
    separators. Prefer compact structured summaries or bounded excerpts to broad
    prints. `glob` may return directories, so select a file before `read`. A
    `bash` result succeeded only when its `exit_code` says so.

    Bindings persist between cells of this user request; redeclaring replaces
    them. Each new user request starts a fresh runtime. Earlier requests are
    history, not unfinished work. Work on the current request, including its
    requested tests. Running off the end, `yieldNow(reason)` or a top-level
    `return` all give results and another turn: returning a value displays it
    as notebook output and finishes nothing. Return whatever you want to look
    at, as often as you like. A returned object is shown field by field as
    text -- an excerpt as its lines, an array of strings one per line -- within
    the return budget the usage line names; a field over its share is paged at
    a line and ends in one cursor line saying how to read on. Return what you
    need to read next, not everything you hold.

    The task ends only where you say it ends.
    `answer(text)` inside a cell ends the task with that text.
    A prose response with no `execute_cell` call ends the task as the answer.
    Use either only when the request is finished and the answer is grounded in
    observed results; do not use prose to announce work you still intend to
    perform.
    To interpret a file, inspect and yield first, then answer from the feedback.

    A thrown error carries its position and completed bindings. Continue from
    that state; failed or skipped calls did not succeed. PermissionDenied is
    final: code cannot widen the session's sandbox grant.

### 2.1 Interface variants

`prompt::preamble_for(interface)` renders the preamble for the interface a
session declares (`--interface`, see [tools](tools.md)). `cells`, the
default, is §2's block unchanged. `hybrid` and `tools` are the same text
with exactly the segments below replaced, so every shared sentence has one
copy. No variant says a runtime tool is callable only inside a cell,
because in hybrid mode that is not true.

**Hybrid.** The opening tool sentences become:

    To act, call a familiar tool directly for one independent operation, or make
    exactly one `execute_cell` call for dependent, branching, looped or batched
    work. Inside a cell the same tools are typed async functions with the same
    arguments and results.

The completion sentence becomes:

    A prose response with no tool call ends the task as the answer.

The `decide` sentence gains what a direct call costs:

    `decide.choice` answers from inside the cell and costs no turn, so a
    judgement belongs in the step that needs it rather than in a turn of its
    own; the Runtime block below declares it when this session has it. A direct
    call spends a whole turn on one operation, which suits an independent step
    whose result needs nothing further this turn; dependent, branching or
    repeated work is what a cell is for.

The chaining paragraph with its worked cells, and the edit-rhythm
paragraph, stay verbatim: a cell is where dependent, branching or repeated
work belongs in either mode.

**Tools.** The opening tool sentences become:

    To act, call the familiar tools directly; each call's result is runtime
    evidence.

The rest of the opening paragraph becomes:

    Stop at the next decision that needs unseen evidence. After each call, wait
    for its correlated result. Never invent output or infer success: only that
    result is runtime evidence.

The chaining paragraph, the `decide` sentence, the edit-rhythm
paragraph and the description paragraph are dropped: a request that
declares no `execute_cell` runs no cell. The cell-mechanics paragraph
becomes:

    Sterna binds an edit to the latest observed source version. A command result
    succeeded only when its `exit_code` says so.

and the completion sentence becomes the hybrid one:

    A prose response with no tool call ends the task as the answer.

The persistent-bindings and completion paragraphs stay in every variant.

## 3. Tool declarations

Each tool renders as a TypeScript `declare function` signature, one `//`
line of summary ending in its purity, and one `// @callers program` line
(`prompt::render_declaration`):

    declare function grep(a: {pattern: string; glob?: string; path?: string}): Promise<Grep.Match[]>;
    // Search the project for a regular expression. … Pure: same tree, same result.
    // @callers program

The signature is the contract; the summary does not repeat what the types
say. `@callers program` means Sterna's own isolate runs the tool; a
`provider` value would declare a server-side tool Sterna never executes.
The purity clause is the tool's own declaration and is what lets a handle be
re-materialised after resume ([runtime](runtime.md#resume)).

After the tools come the **Runtime** block (the host globals a cell holds:
`decide`, `agent`, `bg`, `batch`, `web`, `mcp`, `checks`,
`keep`, `free`, `handles`, `yieldNow`, `answer`, `ask`, `on`, `off`,
`console`), each declared only where this session binds it — `web` only
while `web.enabled` is on (the default), `decide` only when a decision
model is — then
the familiar-tool types, the session facts, and the project's instructions.

### Reading and editing source

- `context({path, symbol?})` is the editing view: the whole target
  definition (or a small file whole), a SHA-256 version, ranked imports,
  nearby definitions, callers and tests, and explicit omissions. Its
  rendering is delivered once with the correlated result. A symbol the file
  does not hold returns `complete: false` and an outline of the file's own
  declarations instead of throwing.
- `edit({path, old, replacement})` replaces one exact, unique, non-empty
  match. It writes only if the file still has a version a prior completed
  cell saw (`context`, or Sterna's own earlier `edit`/`write` of that path);
  an external change is stale. `olds`/`replacements` apply several hunks
  atomically. Stale, missing, ambiguous and no-op edits throw and write
  nothing.
- `write` replaces a whole file. `lines: string[]` (and `oldLines`,
  `replacementLines` for `edit`) take one logical line per item; Sterna adds
  the separators and a final newline, so `$`, quotes and heredocs need no
  template literals.
- A `read` result has `excerpt({start?, lines?})` over its loaded lines:
  `lines` defaults to 400 and caps at 1,000; a line too long for one page is
  marked partial and names a continuation of at most 1,600 UTF-16 units.
- A `read` of a large source file with exactly one unfinished definition is
  promoted to `context`.

## 4. The handle table

Rendered fresh every turn after the tools and before usage: `## Handles`
then one entry per live handle in declaration order, in the shape
[runtime](runtime.md#previews) fixes, or `(none)`. The whole table is capped
at 2,048 estimated tokens; over it, the oldest entries are left out of the
rendering (never freed) with one line saying how many and how to list them.
An object whose type a declaration names (`Code.Context`, a command's
result) is its header alone: its preview would be the declared keys again.
A stale handle shows the one word `stale`.

## 5. The native execution handoff

The action channel is the provider-native `execute_cell` tool. Its input
schema is `{code: string, description: string}`, both required: the
description is the one-line account of the cell the person sees above it; the
result does not repeat it, because the call carries it. Sterna runs only a complete,
decoded input and returns a correlated `tool_result` before the model can
interpret it. Malformed or truncated JSON never runs. Unknown or multiple
calls are rejected each by name, never dropped silently.

A response stopped at `max_tokens` is incomplete, prose included: the
request fails explicitly, nothing from it runs, and it is not recorded as a
turn or accepted as an answer.

**The fenced form.** A ```` ```sterna ```` block is still accepted as input
and as the repair path. One or more complete blocks run in source order as
one cell; ```` ```ts ```` and other Markdown examples never run, because a
model writing *about* TypeScript emits them constantly. An unclosed block
runs nothing.

**Repairing a cell that did not parse.** A parse error runs nothing and
offers one repair: a single ```` ```sterna-edit ```` block,

    {"cell": 3, "replace": "return 'done;", "with": "return 'done';"}

where `cell` names the latest parse-failed cell and `replace` matches
exactly once. Sterna applies it locally and runs the corrected source as a
**new** cell through the same compiler, sandbox and accounting; the
original record is kept. Source, edit and result are each bounded to
128 KiB. A new ordinary cell, the task ending or a runtime reset
invalidates the target; a runtime `SyntaxError` never authorises a replay.

## 6. The result block

The runtime's reply is one message with these sections, in this order,
each omitted when empty:

    [cell 2 yielded in 412 ms]

    ## Handles
    …the table from §4…

    ## Output
    ### readme
    [lines 1-240 of 1,508]
       1 | //! …
    [+1,268 lines not shown · call .excerpt({start: 241, lines: 1268}) on the same File]

    ## stdout
    …the tail of the program's console output…

    ## Usage
    turn output cap 8,000 · task spent 3,412 · cells 2 · return budget 24,000 · this return 6,120 (paged: readme)

A throw replaces the first line with `[cell 3 threw in 88 ms]` and adds an
`## Error` section: class, message, the line and column inside the model's
program, and the top three frames inside it.

- **Usage.** The output cap for the turn about to start; the task's
  cumulative provider-reported tokens (telemetry only: it never stops a
  task); cells used, shown as `n/cap` only when `limits.cells` is set.
- **Return budget.** How many estimated tokens a returned value may fill
  this turn: a quarter of the room left in the context window after the
  turn's output cap, between 4,000 and 24,000, or 8,000 when the window is
  unknown. A field over its share is paged at a line boundary and ends in
  one cursor line saying how to read on; nothing is cut mid-line and no
  field is replaced by its type ([runtime](runtime.md#ending-a-task)).
- **stdout** keeps the last 8,192 estimated tokens of console output.

## 7. The gateway hop changes nothing in the prompt

The request body Sterna builds is byte-identical whether the base URL names
the gateway or the provider directly; a test compares the serialised bodies.
What the hop adds — which account paid, which provider served, what it cost
— is read back from the gateway's response and shown in the interface,
never in a message.

### Cache boundaries

The outbound `system` is a one-element array whose text block carries
`cache_control: {"type": "ephemeral"}`; the `execute_cell` tool
definition carries the same breakpoint, so a changed system can still reuse
the tool prefix. Conversation messages are never marked. These are requests
for caching, not evidence of a hit: `cache_read_input_tokens` and
`cache_creation_input_tokens` are shown separately from input and output,
and a missing field stays unknown rather than zero.

Every request of a session carries one prompt-cache key (`metadata.user_id`,
which the gateway passes on as the Responses API's `prompt_cache_key`), and
sessions share four of them, chosen by the session id. The provider routes a
request by its prompt's first tokens and that key, so a shared key sends a
new session to a machine that already holds Sterna's system prompt: with a
key per session, measured 2026-09-30, a session's first request found it
cached about half the time and was a fifth of Sterna's uncached input over 30
SWE-bench tasks. Four keys rather than one, because a key carrying more than
about fifteen requests a minute spills onto machines that do not hold it.

*Context* (the latest request's input plus cache reads and writes) and
*spent* (every token of every request in the task) are two separate
figures. A context percentage is shown only when `--context-window-tokens`
gave the window; Sterna never guesses one from a model name.

## 8. Request history

Each runtime result has two renderings: the full observation, and a
historical form without the handle table and usage snapshot. A request
keeps the newest live snapshot and the historical form for older results, so
a new task never presents the previous task's handles as current. The
interface and the append-only rollout keep the full observations.

When the conversation outgrows the window (85 % of a known window by
default; `limits.compact_above_percent` in `config.toml` changes it, and
`sterna config` does not set it), older results are swept once and,
if that is not enough, one checkpoint replaces the history and the request
is retried once — without replaying cells or discarding live bindings.
Resume starts request history at the latest checkpoint.

## 9. The event batch row

The runtime may add exactly one row to the handle table, named `batch` and
always last, so the model's own bindings keep their order
([events](events.md)). It sits inside §4's cap, adds no section to §6, and
never becomes a turn of its own. A turn with an empty batch and no user
input does not happen; the runtime waits.

## 10. Reusing a runner across cells

Bindings persist for the whole user request, functions included, so a
check can be defined once and called again:

```typescript
const verify = async () => {
  const result = await bash({command: "python3 -m unittest discover -s tests"});
  if (result.exit_code !== 0) throw new Error(result.stderr);
};
await verify();
```

Each call runs the real command under the current permissions; a saved
function is not a cached result. Bindings end with the request; for reuse
across requests, save the check as a project script.
