# Tools

What a cell can call, how the model is shown it, and how a familiar tool
call becomes the same thing as a cell. Code: `crates/sterna/src/tools/`
(the sandboxed programs), `src/abi/` (the provider-facing shapes),
`src/runtime/bindings*` (the host globals).

## The registered tools

Ten programs, each run under the sandbox ([sandbox](sandbox.md)); the set
is the attack surface, so it grows deliberately (`tools/registry.rs::ALL`):

| tool | what it does | pure |
|---|---|---|
| `read` | one file inside the project, with `excerpt()` paging | yes |
| `context` | the editing view of one file or symbol, with its version; `mode` `precise`, `normal` or `generous` ([project context](project-context.md#source-context)) | yes |
| `grep`, `rg` | regular-expression search (ripgrep when installed) | yes |
| `glob`, `fd` | paths by pattern or by name | yes |
| `jq` | one filter over one JSON file | yes |
| `edit` | exact, versioned replacement of one or several hunks | no |
| `write` | replace a whole file | no |
| `bash` | a command line under the sandbox (`cmd.exe` on Windows) | no |

*Pure* is the tool's own declaration and is what lets a handle come back
after resume ([runtime](runtime.md#resume)). No registered tool reaches the
network; `tests/tools.rs::no_registered_tool_needs_the_network` checks both
the names and the programs they run.

## Host globals

Not programs but functions Sterna itself answers, declared in the system
prompt's Runtime block only where the session binds them:

| global | what it is |
|---|---|
| `decide.choice` | a typed question to Jev, answered in about two seconds ([decisions](decisions.md)) |
| `agent.run` | start a subagent; returns a handle at once ([events](events.md#subagents)) |
| `bg.run`, `bg.watch`, `bg.cancel`, `job.result()` | background commands, collected where they are needed ([events](events.md#background-jobs)) |
| `speculate(check, candidates)` | one to four candidate changes tried against one check in one cell; each is written, the check runs, and every file is put back before the next, through the same checked `write` and `bash` calls and approval gate. Nothing stays changed |
| `batch` | the events that arrived while the model was not looking |
| `on`, `off` | standing handlers over future batches |
| `web.fetch`, `web.search` | the host web broker: allowed hosts at once, any other after asking ([web](web.md)) |
| `mcp.list`, `mcp.call` | the project's MCP servers ([project context](project-context.md#mcp-servers)) |
| `checks.list`, `checks.run` | the project's declared verification commands |
| `ask(question, choices)` | put a question to the person (off with nobody at the keyboard) |
| `keep`, `free`, `handles` | manage handles |
| `yieldNow(reason)`, `answer(text)` | hand back; end the task |

A top-level binding may not take one of these names.

## Three interfaces, one executor

`--interface` decides what the model is *shown*; it never changes what runs.

- **`cells`** (default) — only `execute_cell` is declared; the tools are
  functions inside it. Chosen after a matched comparison on 2026-09-13
  (cells 12/12, hybrid 11/12, tools 10/12, with the fewest requests).
- **`hybrid`** — `execute_cell` and the familiar direct tools.
- **`tools`** — only the direct tools.

A direct call such as `Read` or `apply_patch` is decoded into a canonical
intent, lowered into cell source and handed to the same cell executor a
model-written program uses (`src/abi/`). There is no second executor, so a
direct `Read` and `read()` inside a cell are the same code path. The screen
marks a host-lowered frame as such rather than showing it as code the model
wrote. Two provider dialects are spoken: the Claude-style names (`Read`,
`Grep`, `Glob`, `Edit`, `Write`, `Bash`, `RunTests`) and the Codex-style
ones (`shell`, `apply_patch`, `run_tests`).

## Provenance

Every value the ABI returns says which of three things it is
(`abi/provenance.rs`), decided from mechanical facts and never from a
description:

- **exact** — the complete observation within the requested scope;
- **bounded exact** — an exact subset whose remainder is still reachable
  through a handle or a continuation;
- **derived** — a transformation ran; output the rules shortened is this,
  whatever its quality.

A weaker class never stands in for a stronger claim.

## Command lifting

A familiar shell command whose meaning Sterna can prove runs as the stronger
capability underneath (`abi/lift/`). `rg`, `grep`, `fd`, `cat`, `head` and
`tail` are recognised for an accepted subset of their flags; anything
outside it runs exactly as written. A recognizer has two outcomes and no
third: proven, or not touched — no confidence score, no ranking. Lifting
executes nothing itself; both outcomes continue into the one kernel.

The machine result's `lifting` object counts shell-shaped calls, how many
were recognised and fell back, and each family. A repeated pure
observation (same tool, same checked arguments, same SHA-256) is marked
`repeat_of` on its record; it still runs, because the hash is what decides.

## Refusals

A call outside the grant throws `PermissionDenied { tool, path, rule }`
inside the program; `rule` names the deciding rule. An unknown tool name is
a refusal too, not a panic. See [sandbox](sandbox.md#refusal).
