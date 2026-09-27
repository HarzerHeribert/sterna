# Using Sterna

## From the shell

```sh
sterna                                   # a session in this folder
sterna -p "find the cause of the failing test"          # one task, then exit
sterna exec "fix the failing test" --output-format json # the same, for scripts
git diff | sterna exec --output-format stream-json      # a task read from stdin
sterna --resume [id]                     # an earlier session: a picker in a terminal, else the newest
sterna --continue                        # the same as a bare --resume
sterna --sessions                        # this folder's sessions, newest first
sterna --plan                            # start in plan mode
sterna --image shot.png -p "explain this" # attach up to four images
sterna --add-dir ../shared-library       # grant one more directory for this session
sterna --full-access                     # no questions, no OS confinement of Sterna's own
sterna doctor [--json]                   # what Sterna found and what is missing; starts nothing
sterna update [--check]                  # install the newest release
sterna config …                          # settings (see configuration.md)
sterna --version | --help
```

`-p`/`--print`, `exec` and `--continue` must be the first argument; session
flags follow. `exec` without a task reads all of stdin as one task.
Ordinary piped input to a session is one turn per line. Every exit prints
the session id and how to come back to it.

Session flags: `--model`, `--profile NAME`, `--mode execute|explore|plan`,
`--permissions manual|accept-edits|auto|full`, `--ask-approval` (the same
as `--permissions manual`), `--interface cells|hybrid|tools`,
`--context-window-tokens N`, `--gateway PATH`, `--root PATH`.

### Machine output

`--output-format json` prints one document; `stream-json` prints JSON Lines,
each with `schema_version: 1`, `type` and `sequence`. Both need a single
task. The final `result` carries `success`, `answer`, `error` and a
`telemetry` object: wall time, cells run and failed, tool calls, provider
requests, and input, output, cache-read and cache-creation tokens split
between the main model and the helpers and again by model. Missing
provider usage shows in coverage counters rather than as a zero.
Diagnostics go to stderr and failures exit non-zero.

### Images

`--image` attaches PNG, JPEG, GIF or WebP files, up to four, each at most
5 MiB, to the first task. They survive resume and compaction and need a
model that accepts images. Pasting an image from the clipboard is not
supported.

### Full access

`--full-access` is one word for the widest session: every command line and
the whole project admitted (`--yolo`), nothing asked (`--permissions full`),
and no OS confinement of the children Sterna spawns
(`--dangerously-bypass-os-sandbox`, which needs `--yolo`). It still refuses
the never-grantable set — no network for shells, no `~/.ssh`, `~/.aws`,
`~/.config`, no sandbox launchers. Use it on a machine or container you
trust; the machine is then the boundary.

## Inside a session

| command | what it does |
|---|---|
| `/login` | sign in: a subscription, an API key or your own endpoint |
| `/key <provider>` | store an API key (not echoed) |
| `/models`, `/model` | browse models; set the main, helper or subagent model |
| `/effort` | reasoning effort |
| `/usage` | how much of each subscription's limits is used |
| `/entitlements` | the accounts the gateway can serve from |
| `/wizard` | sign-in, models for each workload, Jev |
| `/mode execute\|explore\|plan` | what the next request may do |
| `/permissions` | how often you are asked (Shift-Tab cycles it) |
| `/settings`, `/config` | the settings sheet; exact keys |
| `/theme` | choose a palette |
| `/cells`, `/cell`, `/cell N` | open every card, the newest cell that ran, or cell N |
| `/diff` | the last cell's before/after diff |
| `/handles` | what the runtime holds |
| `/rollback` | preview and undo what the session changed, keeping your own edits |
| `/memory` | read or save this project's notes |
| `/subagents` | subagent favourites: `on`, `off`, `SLOT MODEL [EFFORT]` |
| `/handlers` | standing event handlers; `/handlers off <name>` |
| `/context`, `/status`, `/budget` | context and token use, session status, task spend |
| `/activity`, `/telemetry` | local notices; live requests and execution (Ctrl-T) |
| `/statusline`, `/sidebar`, `/motion`, `/fullscreen` | what the screen carries |
| `/tool <name> key=value …` | run one tool directly |
| `/mouse` | release or recapture the mouse (Ctrl-G) |
| `/help`, `/exit` | the list; end the session and print its resume id |

Project commands in `.claude/commands/NAME.md` and skills in
`.claude/skills/NAME/SKILL.md` run as `/NAME`. Keys: F2 settings, F3
models, F4 the selected cell's diff, F5 its helpers, Ctrl-O expand a cell,
`?` on an empty composer shows every key. See [workbench](workbench.md).

## Git inside the sandbox

Ordinary `git status`, `diff`, `log` and `commit` work through admitted
shell commands. Children see `GIT_CONFIG_GLOBAL` and `GIT_CONFIG_SYSTEM`
pointed at the null device, so your global Git configuration and credential
helpers are not imported; pass `-c user.name=… -c user.email=…` or set them
in the repository. Shells have no network, so `push` and `fetch` do not
work from inside a session.

## Subagent templates

`.sterna/agents/NAME.toml`:

```toml
instructions = "Review the supplied change. Explain defects using source evidence."
model = "your-model-id"
effort = "high"
```

A cell selects one with `agent.run(task, {profile: "NAME"})`. A template
grants no permissions and runs under the session's own sandbox. Subagents
are off until you enable them ([helpers](helpers.md#subagents)).

## In CI

Install Sterna and the gateway in the image, supply the provider key
through the job's secret environment, choose a model explicitly, and run
`sterna exec TASK --output-format json`. Keep stdout as the result and
stderr as diagnostics, check both the exit code and `success`, and keep the
`telemetry` object for per-model cost. An agent's success is not a
substitute for running the repository's own tests.
