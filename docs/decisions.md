# Decisions: Jev

A **decision** is a typed question answered with one of the caller's named
criteria, a probability for each and a confidence — never free text. Sterna
asks them of **Jev**, TypeSafe's classifier, through the gateway's
`/v1/systemone` route; an answer takes about two seconds. Code:
`crates/sterna/src/decide.rs`.

A decision is advice with a bounded reach. It never sets a tool's purity,
adds or removes a sandbox grant, answers an approval, or proves a lifting
equivalence. A failed, slow (over 15 s) or absent answer leaves the task
exactly as it would be without one; it is recorded, never shown as a
failure.

## Configuration

```toml
[decisions]
model = "jev-latest"   # the default when the gateway serves a TypeSafe account
mode  = "shadow"       # off | shadow | on
```

- **`off`** asks nothing.
- **`shadow`** asks and records what *would* have happened; nothing that runs
  changes.
- **`on`** lets the answers act, within the limits below.

An explicit `model` or `mode = "off"` is never overridden. `sterna doctor`
says which model will be asked, or that decisions are inert. Every
threshold below is a `decisions.*` setting with the default shown.

## What it is asked, and what an answer may do

| question | when | what a confident answer may do (`on`) |
|---|---|---|
| **intent** — `read_only`, `modify`, `run`, `other` | once, before the first turn | hold the first effectful cell once (`hold_above` 0.85); re-issued, it runs |
| **kind** — `explore`, `fix`, `implement`, `question`, `run` | same request | lower effort to `low` for `explore`/`question` when you left effort at `auto` |
| **completion** — does the diff satisfy the request | a claimed completion | a confident no adds a note beside the answer (`completion_no_below` 0.10) |
| **hygiene** — `has_tests`, `out_of_scope`, `debug_leftovers`, `deletes_tests`, `changes_signature` | same request, when there is a diff | a finding held once (`hygiene_*` 0.10 / 0.90) |
| **field shape** — `log`, `listing`, `source`, `prose`, `data` | a returned field over `limits.reduce_above_tokens`, with `decisions.reduce_returns` | shorten a `log` by the rules ([runtime](runtime.md#shortened-output)); the whole value stays bound |
| **approval hint** | a pending confirmation | one line beside it: `fits the request: 0.91` |

Every "held once" means the same call re-issued runs: a decision can make
the model look again, never stop it. A decision never answers a
confirmation: what asks is the sandbox level ([sandbox](sandbox.md)).

## From inside a cell

`decide.choice(question, criteria, subject?)` is declared when a decision
model is configured:

```typescript
const diff = await bash({command: "git diff --stat"});
const call = await decide.choice(
  "Does this diff do more than rename a symbol?",
  {rename_only: "every hunk renames one symbol", wider: "anything else changed"},
  diff.stdout);
if (call.choice === "wider" && call.confidence > 0.8) { /* look closer */ }
```

Two to eight criteria, each with a sentence saying when it applies. A cell
may ask eight questions.

## Telemetry

`--output-format json` and `stream-json` carry a `decisions` object: the
model and mode, how many questions were asked, answered and failed, total
latency, the intent answer, and per question what held, what would have
held, and what was skipped. It is `null` when no decision model is
configured. `/cell` shows one line for the task, such as
`decision: read_only 0.94 · holds 1 · overrides 0`.
