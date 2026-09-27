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
permission denial survives every project overlay and `--yolo`.

## Changing a setting

- **`/settings`** (or F2) opens the settings sheet: Everyday, Display,
  Little helpers, Models & accounts, Subagents and Advanced, each with a
  Global and a Project tab. A choice validates and saves at once; Undo
  restores the previous operation. Viewing creates no files.
- **`/wizard`** walks through sign-in, a model for each workload and Jev.
- **From the shell**, with no model, gateway or terminal needed:

  ```sh
  sterna config                              # every setting, its value and where it came from
  sterna config global session.effort high
  sterna config local ui.theme amber         # local is the default scope; project is an alias
  sterna config local --unset session.effort
  sterna config --help                       # every key, its type and choices
  ```

Display settings (`ui.*`) apply at once. Runtime and permission settings
apply from the next session; `/models` changes the model between turns of a
running one. Unknown keys and invalid values are refused and the file is
left byte-identical. Values are literal: nothing is shell-expanded or run.
Credentials never belong here — keys go to the gateway (`/key`).

## The settings people change

| key | what it decides |
|---|---|
| `model.parent` | the model that answers you |
| `session.effort` | how hard it thinks: `low` … `max` |
| `session.mode` | `execute`, `explore` (reads only) or `plan` |
| `permissions.mode` | how often you are asked: `manual`, `accept-edits`, `auto`, `full` ([sandbox](sandbox.md)) |
| `permissions.allow`, `permissions.deny` | permission patterns; a deny beats every allow |
| `permissions.full_access` | global only: the three halves of `--full-access` |
| `helpers.model`, `helpers.enabled` | the cheap model the helpers run on ([helpers](helpers.md)) |
| `agents.mode`, `agents.slots.<quick\|balanced\|deep\|heavy>.*` | whether and where subagents run |
| `decisions.model`, `decisions.mode` | Jev, the classifier ([decisions](decisions.md)) |
| `supervisor.model`, `supervisor.every` | the loop watcher ([supervisor](supervisor.md)) |
| `ui.theme` | the palette ([workbench](workbench.md#themes)) |
| `ui.motion`, `ui.reduced_motion`, `ui.statusline`, `ui.sidebar`, `ui.stream` | how much moves and what the screen carries |
| `web.enabled`, `web.allow_domains`, `web.search_endpoint`, … | the web broker ([web](web.md)) |
| `limits.cell_wall_clock_s` (30), `limits.response_bytes` (16 KiB), `limits.cells` (none) | per-cell limits; nothing caps a task's cells unless you set it |

`sterna config --help` lists the rest: per-helper effort, the decision
thresholds, the explore mode's extra globs and commands, and the prompt
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
