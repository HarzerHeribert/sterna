# Architecture

Two programs, each its own crate and binary. Sterna needs the gateway to
reach a model; the gateway needs nothing from Sterna.

```
you ──► sterna ──HTTP──► inference-gateway ──► providers
        (one session)    (keys, subscriptions,     (Anthropic, OpenAI,
                          pooling, translation)     Gemini, xAI, …)
```

## Sterna

`crates/sterna`, binary `sterna`. One session of a coding agent in one
project folder:

- **The loop.** Each turn the model writes one TypeScript program, a *cell*,
  that runs in an embedded V8 isolate. Tools (`read`, `grep`, `edit`,
  `bash`, …) are functions inside it; what they return stays in the isolate
  as a named handle, and the model is shown a bounded preview
  ([runtime](runtime.md), [model contract](model-contract.md)).
- **The sandbox.** Every tool that spawns a process runs under the OS's own
  confinement, with grants compiled once at session start ([sandbox](sandbox.md)).
- **Subagents.** Another model loop that takes a whole goal and runs
  beside the task ([subagents](subagents.md)). A classifier, Jev,
  answers typed questions in about two seconds in a live session
  ([decisions](decisions.md)). A task that stops producing anything ends
  on its own, without a model deciding so (`progress.rs`).
- **The workbench.** The terminal interface ([workbench](workbench.md)).
- **Its own files.** Configuration in `~/.config/sterna/config.toml` and
  `<project>/.sterna/config.toml`; sessions, memory and the scratchpad under
  `<project>/.sterna/` ([configuration](configuration.md)).

Sterna links no gateway code. It starts `inference-gateway serve --listen
127.0.0.1:0` as a child, reads the one ready line
(`{"listening": …, "token": …}`), sends every model request to that URL,
and closes the child's stdin when the session ends. It asks the same binary
for accounts, subscriptions and keys (`entitlements`, `subscriptions`,
`credentials`, `models`). Three cases, decided once at start
(`src/gateway.rs`):

| situation | what Sterna does |
|---|---|
| `inference-gateway` on `PATH`, or named with `--gateway <path>` | starts it and owns its lifetime |
| `ANTHROPIC_BASE_URL` names a running gateway (with a token, or on loopback) | attaches and starts nothing |
| no gateway installed and none named | talks to the provider directly and says so; a gateway named by path that fails to start is a startup refusal |

## The inference gateway

`crates/inference-gateway`, library and binary `inference-gateway`. It owns
everything about reaching a model that is not one session's business:
providers, API keys, subscription sign-in (through a pinned CLIProxyAPI
broker), several accounts pooled per model, same-model failover, protocol
translation (Anthropic Messages, OpenAI Chat Completions and Responses,
Gemini) and usage accounting. It serves `/v1/messages`,
`/v1/chat/completions`, `/v1/responses`, `/v1/models` and `/v1/systemone`
(the classifier route) on an ephemeral loopback port. Details:
[gateway](gateway.md).

## The rules between them

- A caller sends the model, the effort, the request and a fallback policy.
- The gateway chooses the provider, the account and the entitlement.
- The gateway never changes the model or the effort unless the caller's
  fallback policy allows it.
- The gateway holds no project, session or harness state: a standalone
  gateway serves HTTP clients and does not know what any of them is.
- Anything inside one turn or one session belongs to Sterna. Anything about
  accounts, keys and providers belongs to the gateway.

## How the boundary is enforced

| invariant | test |
|---|---|
| The gateway crate imports none of the modules that would make it a harness | `crates/inference-gateway/src/gateway/tests.rs::the_gateway_imports_none_of_the_modules_that_would_make_it_a_harness` |
| The gateway keeps no database and names no host crate | `crates/inference-gateway/src/gateway/tests.rs::the_gateway_names_no_glasshouse_path` |
| A dead model is refused, never served by another model | `crates/inference-gateway/tests/boundary.rs::a_request_for_a_dead_model_is_refused_rather_than_served_by_another_model` |
| A Sterna session runs against a gateway it started, with nothing else installed | `crates/sterna/tests/session.rs::a_session_runs_standalone_against_a_gateway_it_started` |
