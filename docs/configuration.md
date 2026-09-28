# Configuration

Sterna owns its configuration. Another harness's files are read for
instructions and commands (see [project context](project-context.md)) but
never become Sterna's settings unless you import them.

## Where settings live

| what | file |
|---|---|
| your defaults, including permissions | `$XDG_CONFIG_HOME/sterna/config.toml`, otherwise `~/.config/sterna/config.toml` |
| this project's overrides and named profiles | `<project>/.sterna/config.toml` |
| your global instructions (not settings) | `AGENTS.md` in the same user directory |
| verification commands for the checker | `<project>/.sterna/checks.toml` |
| subagent templates | `<project>/.sterna/agents/<name>.toml` |

*Project* is the folder Sterna started in, or `--root`, Git repository or
not; there is no upward search. *Global* is this OS user.

Precedence, lowest first: built-in defaults → global → project → the named
profile selected with `--profile NAME` → command-line flags. A global
permission denial survives every project overlay. `sandbox.level`,
`sandbox.hosts` and `sandbox.ecosystems` are global only: a project's copy
is ignored and says so.

## Changing a setting

- **`/settings`** (or F2) opens the settings sheet in five sections:
  Everyday (what you change most), Models (which model does which job,
  Main's effort, subagents, Jev and accounts), Little helpers, Display and
  Advanced (limits, the web, permissions, and the confidence thresholds
  last). It opens on Global; a choice is written to the
  project only when the Project tab (F6) is chosen, and a row the project
  overrides says so. Each row shows what the running session uses now. A
  choice validates and saves at once; Ctrl-Z, or the undo chip beside the
  notice, takes back the newest change -- from Settings, a chip or a
  command alike. Viewing creates no files. `/settings <word>` opens on
  the row the word names.
- **`/wizard`** walks through sign-in, a model for each workload and Jev.
- **From the shell**, with no model, gateway or terminal needed:

  ```sh
  sterna config                              # every setting, its value and where it came from
  sterna config global session.effort high
  sterna config local ui.theme amber         # local is the default scope; project is an alias
  sterna config local --unset session.effort
  sterna config --help                       # every key, its type and choices
  ```

Display settings (`ui.*`), the sandbox level, the effort and the models
apply at once; a row that waits for the next session says so. Unknown keys and invalid values are refused and the file is
left byte-identical. Values are literal: nothing is shell-expanded or run.
Credentials never belong here — keys go to the gateway (`/key`).

## The settings people change

| key | what it decides |
|---|---|
| `model.parent` | the model that answers you |
| `session.effort` | how hard Main thinks: `auto` (the model chooses) or `low` … `max`; a saved `default` is read as `auto` |
| `sandbox.level` | global only: `ask`, `sandboxed` (default) or `full` ([sandbox](sandbox.md)) |
| `sandbox.hosts`, `sandbox.ecosystems` | global only: hosts commands may reach through the proxy, beside the ecosystems switched on |
| `permissions.allow`, `permissions.deny` | permission patterns; a deny beats every allow, and a `Bash(...)` allow pre-approves a command on Ask |
| `helpers.model`, `helpers.enabled` | the cheap model the helpers run on ([helpers](helpers.md)) |
| `agents.mode`, `agents.slots.<quick\|balanced\|deep\|heavy>.*` | whether and where subagents run |
| `decisions.model`, `decisions.mode` | Jev, the classifier ([decisions](decisions.md)) |
| `ui.theme` | the palette ([workbench](workbench.md#themes)) |
| `ui.motion`, `ui.statusline`, `ui.sidebar`, `ui.stream` | how much moves and what the screen carries |
| `web.enabled`, `web.allow_domains`, `web.search_endpoint`, … | the web broker ([web](web.md)) |
| `limits.cell_wall_clock_s` (30), `limits.response_bytes` (16 KiB), `limits.cells` (none) | per-cell limits; nothing caps a task's cells unless you set it |

`sterna config --help` lists the rest: per-helper effort, the decision
thresholds and the prompt
experiments under `limits.*` (off until measured).

## Named profiles

```toml
[model]
parent = "your-usual-model"

[profiles.review.model]
parent = "your-review-model"

[profiles.review.helpers.effort]
check = "high"
```

`sterna --profile review` overlays the table on the base settings. An
unknown profile name is an error; selecting one edits nothing. `--model`
still wins over any file.

## Importing

```sh
sterna config import claude            # preview: permissions from .claude/settings.json
sterna config import claude --apply
sterna config import legacy            # preview: an older project configuration file
sterna config import legacy --apply
```

An import previews unless `--apply` is given and never modifies its source.
The Claude import takes `permissions.allow` and `permissions.deny` and
nothing else — never hooks, environment or credentials — and a rule it
cannot represent blocks the apply, so a denial is never silently dropped.

## How the files are written

The store keeps comments and unrelated keys, validates before writing,
refuses a save made against a stale read, refuses a symbolic link on the
path, and replaces the file atomically. A settings file is capped at
256 KiB.

On Linux, Landlock cannot carve `.sterna/` out of a writable project for a
spawned shell, so an admitted command could edit settings a *future*
session reads; the running session's settings are an immutable snapshot and
startup says so. macOS excludes the folder in the sandbox itself.
