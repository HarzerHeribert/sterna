# Helpers and subagents

Two ways Sterna hands work to another model, with different lifetimes:

> **A helper owns a question** and is scoped to the call that asked it: it
> returns a value and is gone. **A subagent owns a goal** and is scoped to
> the task: it runs beside the parent and its answer arrives later.

| | helper | subagent |
|---|---|---|
| returns | a value, evidence, a verdict | work and findings, later |
| lifetime | atomic to its caller | independent of the cell that started it |
| effects | none: `write`, `edit` and `bash` are never in its toolset | whatever the session's grant allows |
| started by | a cell (`helper.find(…)`) or a fixed point of the task | a cell (`agent.run(…)`) |
| costs the parent | no turn | no turn; the answer is an event |

## Helpers

A helper is a cheap model with a narrow toolset answering one question.
**The roster is data**: each helper is a `HelperSpec` literal in
`crates/sterna/src/helpers.rs`, and the guardrails live in the one runtime
that executes them all, so a new helper cannot add a new failure mode.

| helper | answers | tools | where it runs |
|---|---|---|---|
| **scout** (`find`) | where something lives, as `file:line` spans; Sterna appends those exact lines from disk | `read`, `rg`, `fd`, `grep`, `context` | in a cell; before the first turn (preflight) |
| **dissector** (`dissect`) | the same, in one answer from the project's file listing and instruction headings | none | preflight, when `helpers.scout_oneshot` picks it |
| **reducer** (`reduce`) | a log or command output reduced to its distinct failures, with `file:line` where the text names one | none | in a cell; after a large tool result |
| **checker** (`check`) | whether a claim holds, as a verdict plus its evidence | `read`, `grep` | in a cell; at the completion gate |
| **acceptance** | the request turned into a checklist of verifiable items | none | before the first turn, when `helpers.acceptance_list` is on and no dissection writes the list (its `## Accept` section, after a look at the project); after a dissection that named none |
| **mender** | the syntax of a cell that failed to parse, repaired without changing its meaning | none | when a cell does not parse |

Inside a cell a helper call is an ordinary `await`, so it can sit in a
`catch` or a branch and costs no turn:

```typescript
const run = await bash({command: "cargo test"});
if (run.exit_code !== 0) keep("failures", await helper.reduce(run.stdout));
```

- **Model.** `helpers.model`; with none set, helpers never run.
  Effort per role: `find` low, `reduce` medium, `check` medium by default
  (`helpers.effort.*`).
- **The reducer's free rung needs no model.** Before any request, rules
  fold what a log already counts (passing test lines, `Compiling` chatter,
  repeated lines) and never drop a failure line. They run on a command
  result over `helpers.reduce_above_tokens` with helpers off too, and the
  result says `rules only, no helper model`.
- **At most 8 helper calls per cell** (`helpers.calls_per_cell`); `decide`
  questions share that allowance.
- **Told to be quick, never cut off.** Nothing counts a helper's turns; a
  helper that cannot answer says what is missing instead of guessing.
- **Cancellation.** Ctrl-C stops waiting at once; a request already sent may
  still finish on the provider's side.
- **Visible.** A running helper is one row under its cell with a live
  timer; F5 or `/cell N` shows what each one did.

### Prepared evidence

Before an invoked scout, checker or reducer starts, Sterna prepares bounded
starting evidence itself: a pruned tree, manifests and task-term matches for
the scout; the original request and changed files for the checker, plus the
results of the project's declared checks; mechanical failure windows for
the reducer. At most 8 KiB per file, 1,024 nodes, seven levels and
32 KiB in all, with omissions named.

Declared checks live in `.sterna/checks.toml`:

```toml
checker = ["tests"]
[checks.tests]
command = "python3 -m unittest discover -s tests -v"
inputs = ["src", "tests", "README.md"]
reuse = true
```

A cell runs them with `checks.run("tests")`, inside the sandbox like any
command. With `reuse`, an unchanged successful result (same inputs, same
environment) is returned marked `reused` instead of run again.

### The completion gate

When a task claims to be finished, the checker compares the answer with the
request, the diff and the evidence. It runs behind the answer, so it costs no
wait, and its note says what it used: `checked after the answer: holds ·
9.8k tokens`, with its reasons folded under that line. When it runs is
`helpers.completion_check`:

| value | checks |
|---|---|
| `auto` (default) | after big work: a list the Scout wrote, or 4 files, 150 lines or 10 cells (`completion::BIG_*`) |
| `always` | every answer that changed something |
| `off` | never |

A turn that changed no file is never checked: a question has nothing in the
files to check, and the checker used to spend its turns finding that out. A
saved `true` or `false` from before is rewritten once as `auto` or `off`.
A checker called inside the same
cell as a completion turns that completion into a candidate: the parent sees
the checker's verdict in a further turn before the task may end.

### Tests that exercise the change

When a command that runs a changed test file passes and the task has
changed code too, Sterna copies the project into a temp folder, puts the
changed code back as it was when the task started, keeps the tests, and
runs the same command there on its own thread. If it passes there too, the
next cell result says the tests do not exercise what changed; if it fails,
as a test of the change should, nothing is said. It needs no model and knows
no language: a test file is recognised by its path (`tests/`, `test_`,
`_test.`, `.test.`, `.spec.`, `…Test.java`), the command is the model's own,
and the verdict is an exit code. A command that writes the test file (a `>`
into it, `tee`, `cp`) is not a run. Git projects only; one run at a time.

Measured 2026-09-29 on three SWE-bench tasks, a brief of the request written
by a cheap model and a completion hold on untested related tests made
resolution worse (2/9 against 4/9) and were taken out: the brief turned the
issue's one example into the whole requirement, and the hold never led to a
fix.

After a task that had to search, Sterna notes where things live in
`.sterna/learned.md` and reads those notes into the next task
(`helpers.learn`, on).

## Subagents

```typescript
const probe = agent.run(
  "Find why the auth regression happens. Do not modify files.",
  {slot: "quick"});
return {started: probe.source};
```

`agent.run` returns a handle immediately; the cell ends; the answer arrives
as an `agent.done` event in a later batch ([events](events.md#subagents)).
A cell never stays open waiting for one.

- **Off by default** (`agents.mode = off`). `pinned` sends every subagent to
  `agents.model`; `roster` offers up to four favourites, `quick`,
  `balanced`, `deep` and `heavy`, each a model and an effort
  (`/subagents quick MODEL [EFFORT]`). An empty slot never inherits the
  main model, and a call cannot raise a slot's effort.
- A subagent runs under the session's grant, cannot start a subagent, may
  use helpers, and stops with `bg.cancel`.
- `.sterna/agents/NAME.toml` templates add instructions, a model and an
  effort within the configured policy; they grant no permissions.

## Choosing

Use a helper for one bounded question whose answer the current step needs:
where is X, what failed in this log, does this diff satisfy that item. Use a
subagent for a separable goal that takes several turns and would otherwise
fill the parent's context. Use neither for something one `read` or `grep`
answers.
