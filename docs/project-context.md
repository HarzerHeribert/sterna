# Project context

What Sterna reads from the project and from you before and during a task.
None of it runs a model: it is gathered by Sterna itself, then counted in
the model's input like everything else. Code:
`crates/sterna/src/project/`.

## Instructions

- **Your global instructions:** `$XDG_CONFIG_HOME/sterna/AGENTS.md`,
  otherwise `~/.config/sterna/AGENTS.md`. Reading it grants no tool access
  to your home.
- **The project's:** `AGENTS.md` and `CLAUDE.md` at the root, refreshed at
  the start of every task (a resumed one included), each labelled with its
  file and directory scope.
- **Nested ones** (`AGENTS.md`/`CLAUDE.md` in subdirectories) are listed in a
  path-only index. Before a path tool first touches an unseen scope, the
  runtime stops that cell, delivers the complete applicable documents in
  that cell's result and asks the model to continue; the blocked call and
  the rest of that cell did not run, and nothing is replayed automatically.
  The system prompt is never edited mid-task, so the provider's prompt cache
  keeps holding the conversation. A shell command is opaque, so its first
  call loads all indexed guidance before spawning. An index that ran out of
  its scan budget stops nothing: the cell runs and its result says once that
  deeper folders were not indexed.
  Generated directories (`.git`, `.sterna`, `target`, `node_modules`, …) are
  not indexed; guidance outside the project root is not loaded.

Documents are complete UTF-8 text: 64 KiB per document, 256 KiB and 128
documents per batch, index discovery bounded at 10,000 entries and 32
levels. An oversized or unreadable required document stops the operation
with a visible reason; a truncated prefix is never presented as the whole.

## The environment snapshot

At task start, a bounded snapshot: platform and architecture, UTC time, the
shell, which common executables exist, the project root and shell working
directory, a scratch path, Git metadata the grant permits, the sorted
top-level entries and recognised project files. It runs no project
executable, creates nothing, guesses no language version, and stays the
same between requests of the task.

## Commands and skills

`.claude/commands/NAME.md` and `.claude/skills/NAME/SKILL.md` are invoked
as `/NAME [arguments]`. Arguments stay literal text; a skill grants no
permissions, and its relative references resolve from its own directory.

## MCP servers

`.mcp.json` servers are callable from a cell:

```typescript
const tools = mcp.list();                        // name, server, tool, description, inputSchema
const out = mcp.call(tools[0].name, {query: "…"}); // content stays a handle
```

```json
{"mcpServers": {
  "local":  {"command": "my-mcp-server", "args": ["--stdio"], "env": {"TOKEN": "${TOKEN}"}},
  "remote": {"type": "http", "url": "https://tools.example.org/mcp"}
}}
```

- The environment orientation names the servers the grant admits, read
  from `.mcp.json` without starting one, so the model knows to look. No tool
  schema is ever in the system prompt: `mcp.list()` delivers them in a
  cell's result when asked, so the prompt stays one fixed, cached block.
- A server's tools need `mcp__<server>__<tool>` allow rules before
  discovery starts it; denies win. Every advertised tool and every call is
  checked against the session's profile.
- **Local (stdio)** servers start lazily, under the OS sandbox with the
  project as working directory and no network. Ambient credential
  variables are removed; only the server's own `env` entries are passed.
  Protocol 2024-11-05: initialize, paginated `tools/list`, `tools/call`.
- **Remote** servers use Streamable HTTP (2025-03-26) through the web broker,
  so `web.enabled` must be on, as it is by default ([web](web.md)). JSON and
  bounded finite SSE responses; no redirects, no automatic replay, no OAuth
  discovery or server-initiated requests.
- Limits: a 1 MiB configuration, 128 tools per server, 16 discovery pages,
  32 KiB per schema, 8 MiB per frame, 10 s per exchange. Excess content
  fails explicitly rather than arriving as a silently truncated result.
- Every MCP call counts as effectful for resume, whatever the server claims.
  Server stderr is not copied into logs; call records omit argument values.
  A cancelled or broken server is not restarted, so nothing is replayed.

## Source context

`context({path, symbol})` returns a small file whole, or for a large file
the named definition with nearby definitions, imports, callers and tests.
Definition boundaries are recognised in Rust, Python, JavaScript and
TypeScript (by the same parser Sterna uses for cells), Go and Java; leading
doc comments, decorators and annotations stay with their definition.
A definition is marked complete only when its boundaries were established;
ambiguous syntax falls back to a bounded window and names the omission.
A definition is capped at 24,000 bytes and a result at 18 supporting
excerpts.

`symbol` may name a member as `Class.member` (or `Outer.Inner.member`),
walked member by member, so a method name two classes share is not
ambiguous. A nearby definition is the one declared near the target, never
the file's first definition of the same name.

A long class is delivered whole (to the byte cap), not as a skeleton. A
skeleton -- head, one line per member, setup and last bodies -- was measured
against it on the SWE-bench subset on 2026-09-29 and resolved fewer tasks
for 14% less cost, so it was taken out.

The conversation is append-only, so a context whose exact rendering an
earlier result already carries is not printed again: it arrives as its
header (path, symbol, version) and one line naming the cell whose result
holds it. It still binds an `edit` to that version. A changed file renders
different bytes and is printed in full, and after a checkpoint replaces the
conversation every context is printed in full again.

A file the model has read that changes on disk by anything but its own
`edit` or `write` -- a formatter it ran, a background job, the person's
editor -- is checked at the start and end of every cell. The change arrives
with that cell's result as its changed lines, each with its own number, and
the model's view follows it: the version an `edit` binds to moves to the new
bytes when the model had the old one whole, and the lines it has seen are
renumbered through the change. No new `context` is needed, so nothing is
read twice and the earlier conversation stays as it was. A change of more
than 200 lines, or one that does not fit the turn's feedback, is named in
one line instead, and an `edit` of that file is refused as stale until the
model reads what it needs again. An `edit` in the same cell a change is
found is told to make it in the next one, where the change has been read.
