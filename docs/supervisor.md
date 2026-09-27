# The supervisor

Every few cells Sterna looks at what the session has been doing and decides
one thing: nudge the working model, or not. Code:
`crates/sterna/src/supervisor.rs`, `src/progress.rs`.

## Configuration

```toml
[supervisor]
enabled = true
every   = 4        # cells between looks
model   = "…"      # unset: falls back to helpers.model
```

With neither `supervisor.model` nor `helpers.model` set, and no decision
model, the session runs unwatched.

## The look

What the supervisor sees since its last look is each cell's first line,
its outcome and the calls it made (tool, checked arguments, how each
ended) — the rollout's own record, never a payload. The look has three
layers, cheapest first:

| layer | who | answers |
|---|---|---|
| 1 | nobody — a counter | how many cells in a row changed nothing |
| 2 | the decision model | one typed choice: `making_progress`, `repeating_a_failing_call`, `looping_over_the_same_reads`, `stopped_without_returning` |
| 3 | the supervisor model | the nudge's one sentence, **only after layer 2 said something is wrong** |

The counter is evidence for layer 2, not a gate: a model repeating a failing
call while still writing files looks like progress to a counter that
watches the tree. Layer 2 decides (anything other than `making_progress` at
or above `decisions.supervision_above`, 0.85); layer 3 only phrases, and
cannot overturn it. With no supervisor or helper model the nudge still
fires in the criterion's own words. With no decision model, layer 3 decides
alone with one short look answered as `{"intervene": bool, "reason": "…"}`;
anything unparseable is *no*.

`decisions.mode = off` silences the nudge. `shadow` and `on` behave alike
here, because a nudge runs nothing and blocks nothing.

## The nudge

One line, `supervisor: <reason>`, at the head of the next result the model
receives — including the native `tool_result` — recorded in the rollout
and shown in the interface. A nudge never ends a task, changes a grant or
runs code. A task also ends when the progress counter sees a long run of
cells that produced nothing; there is no fixed cell cap unless
`limits.cells` sets one.

## Tested

`tests/session.rs` plants a three-turn loop and asserts the nudge heads the
turn after the second repeat, and that `enabled = false` sends no
supervisor request. `tests/decisions.rs` covers the layers: a confident
`making_progress` buys no prose request, a confident loop buys exactly one,
and a failed supervision question nudges nothing.
