# Subagents

A subagent is another model loop that owns a goal. It runs beside the
parent, under the session's grant, and its answer arrives later as an
event. Starting one costs the parent no turn.

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
- A subagent holds every registered tool, runs under the session's grant,
  cannot start a subagent of its own, and stops with `bg.cancel`.
- `.sterna/agents/NAME.toml` templates add instructions, a model and an
  effort within the configured policy; they grant no permissions.

## Choosing

Use a subagent for a separable goal that takes several turns and would
otherwise fill the parent's context. Use nothing for what one `read` or
`grep` answers.

## What was here before

Until 2026-09-30 a second kind of delegate, the helpers, ran cheap models
on narrow questions: a Scout before the first turn, a reducer for long
output, a checker behind the answer, an acceptance lister, a mender for
parse failures and a learned-notes writer. They showed no benefit in the
measurements, and on three SWE-bench tasks on 2026-09-29 a cheap model's
brief of the request made resolution worse (2/9 against 4/9), so they were
removed ([measurements](measurements.md)). A saved `helpers.*` setting is removed
from its file with a one-time notice; `helpers.reduce_above_tokens` and
`helpers.reduce_returns` moved to `limits.reduce_above_tokens` and
`decisions.reduce_returns`, and their saved values moved with them.
