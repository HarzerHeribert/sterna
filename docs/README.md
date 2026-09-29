# Sterna documentation

Start with [architecture](architecture.md): two programs, one page.

**Using it**

- [usage](usage.md) — the command line, slash commands, machine output, CI
- [configuration](configuration.md) — settings files, keys, profiles, imports
- [workbench](workbench.md) — the terminal interface, keys, themes
- [subscriptions](subscriptions.md) — which subscriptions can sign in, and the terms risk of each
- [web](web.md) — `web.fetch` and `web.search`

**How it works**

- [model contract](model-contract.md) — what the model is sent and what it may send back
- [runtime](runtime.md) — cells, handles, previews, ending a task, resume
- [tools](tools.md) — the tools, the host globals, the three interfaces, command lifting
- [events](events.md) — background jobs, subagents, standing handlers
- [sandbox](sandbox.md) — grants, platforms, modes, how often you are asked
- [subagents](subagents.md) — delegating a whole goal to another model loop
- [decisions](decisions.md) — Jev, the classifier
- [project context](project-context.md) — instructions, MCP servers, source context
- [observing](observing.md) — the live event stream
- [gateway](gateway.md) — the inference gateway

**Measuring it**

- [ruler](ruler.md) — `sterna ruler run`
- [measurements](measurements.md) — results so far

`assets/` holds the banner the README shows, and the tern sprites as PNGs;
the product page draws its terns from `sites/public/`.
