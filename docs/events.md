# Events, background jobs and subagents

Work that finishes while no turn is running — a background command, a
subagent, a watch that matched, a deadline — reaches the model as one
batch, never as a turn of its own. Code: `crates/sterna/src/events/`,
`src/bg.rs`, `src/runtime/handlers.rs`.

## The event

| field | meaning |
|---|---|
| `kind` | `bg.done`, `agent.done` or `timer` |
| `source` | `bg/<handle>` or `agent/<handle>` — match a job with `source: job.source`, not its bare `id` |
| `at` | when the runtime **accepted** it, not when its source claims it happened |
| `payload` | a handle, read on first access; never previewed |
| `age` | how many batches it has appeared in, `0` on first delivery |

A second arrival with the same dedup key (`bg/<handle>` + emission,
`agent/<handle>` + emission, `bg/<handle>` + deadline) is dropped; the first
stands with its original time. A still-running job raises nothing: only a
transition is an event.

## The window and the batch

One window is always open. It closes **2,000 ms after its first event**
(measured from the first, so a storm cannot hold it open) and is delivered
as one handle named `batch`, always the last row of the handle table. The
batch cap is 200; a wider window spills into the next one, oldest first.

The preview lists counts by kind, then five samples — one per kind, rarest
kind first — then `… and K more`, inside the 256-token preview cap. A cell
works with it through:

    batch.n                       // how many events
    batch.where({kind, source})   // both optional; kind matches by prefix
    batch.ack(ids)                // what the program has dealt with
    batch.rest()                  // this batch's events not yet acked

Before the first delivery `batch` exists with `n = 0`, so a program can
launch work and check for it without an undefined name. **Unacked events
roll into the next batch** with their `age` raised; at age 4 an unacked
event is dropped, the rollout records `event.dropped`, and reading its
payload throws `PayloadDropped(id)`. Rolled events never take more than half
the cap. A turn with an empty batch and no user input does not happen: the
runtime waits.

## Background jobs

    bg.run(command, {timeout})             → Job, immediately
    bg.watch(command, {every, until, timeout}) → Job, immediately
    bg.cancel(job)                         → idempotent
    await job.result({wait})               → {stdout, stderr, exit_code, status}

`bg.run` returns before the process has done anything; the model never
blocks on output and never polls. On exit a `bg.done` event carries a
payload whose `stdout` and `stderr` are themselves handles, so a job that
printed 40 MB costs a status line. `bg.watch` re-runs the command every
`every` ms (default 1,000, floor 100) and emits one `bg.done` per match
until the `until` substring appears or the job is cancelled. A cancelled or
timed-out job still emits `bg.done` with a cancelled status, so nothing
waits on a dead result.

`job.result()` is the one wait a program may ask for: it returns when the
job finishes, in the cell that started it or any later one, so a slow
command whose result the next step does not need runs while the model reads
and edits. The wait pauses the cell's clock as a foreground command does,
and an interrupt stops the wait and leaves the job running. A result a
program waited for is withdrawn from the events not yet delivered, so it
does not arrive again as a `bg.done`.

**No wait holds a cell longer than its wall clock** (`limits.cell_wall_clock_s`,
30 s), and nothing is killed when it runs out. A foreground command still
running then goes on as a background job: the job is bound as `job1`,
`job2`, … and the call throws, naming it, so the model decides whether to
wait again (`await job1.result()`), stop it (`bg.cancel(job1)`) or leave it
to arrive as a `bg.done`. `job.result()` hands back the same way;
`result({wait: ms})` waits longer, for a build the model knows is slow.
Before 2026-09-30 a foreground command had no bound at all, and a command
that never ended held its session until someone pressed Ctrl-C.

A background job runs under the same sandbox grant as a foreground call: a
command outside the grant throws `PermissionDenied` at the call, before any
handle exists. It runs in the project root with the session's environment;
`cwd` and `env` options are refused rather than ignored.

## Subagents

    agent.run(task, {slot, model, effort, profile, turns}) → Job, immediately

A subagent is a background job whose work is a turn loop. It returns a
handle at once, never blocks, is stopped by `bg.cancel`, and its answer
arrives later as an `agent.done` event (`stdout` is the answer; `status`
says how it ended). `job.progress()` looks in on a running one — turns,
tools called, elapsed — without waiting.

- It runs under the parent session's own compiled profile, cloned rather
  than recompiled, and spends the same task's budget.
- It cannot start a subagent of its own; that is refused before a handle
  exists.
- Nothing counts its turns unless `turns` asks for a short errand; it runs
  until it answers or its wall clock runs out, and a stopped one still
  returns what it had.
- **Delegation is off by default.** Enable it with `agents.mode`, and give
  it models through the four favourite slots `quick`, `balanced`, `deep` and
  `heavy` (`/subagents quick MODEL [EFFORT]`). An empty slot never inherits
  the main model; a pinned model cannot be escaped by a template or an
  explicit `model`. See [subagents](subagents.md).

## Standing handlers

    on({kind, source}, program)   → Handler
    off(handler)

`program` is TypeScript source, compiled once and kept as a callable in the
task's isolate. It runs against every future batch that matches, **before**
the batch reaches the model; events it acks never reach the model. A task
may register at most 64 handlers of at most 65,536 source bytes each.

- A handler runs under the same grant and per-cell timeout as a turn, and
  its run is one rollout line.
- A handler cannot register a handler: `on()` inside one throws
  `HandlerNesting`.
- A handler that throws is disabled, not retried; a handler ends with the
  task that registered it.
- Handlers are task-scoped and do not survive a resume.

The interface shows the open window, the batches delivered to the model this
task, and the live handlers.
