# The sandbox

The model's TypeScript is already contained: a V8 isolate has no
filesystem, no sockets and no ambient authority. This page is about the
other half — the tools a program calls, which spawn real processes and
touch real files. Code: `crates/sterna/src/sandbox/{profile,macos,linux,windows}.rs`,
`src/permissions.rs`, `src/approval.rs`.

## The invariants

1. **No grant is ever widened at the model's request.** No tool, argument
   or prompt adds a path to a profile. Widening takes your configuration or
   `--add-dir`, before a session starts.
2. **`deny` beats `allow`**, at every specificity.
3. **The project root is the only writable root by default.** Not the home
   directory, not a temp directory, not the project's parent.
4. **A request outside the grant is refused inside the program.** The call
   throws `PermissionDenied { tool, path, rule }`; the program may catch it.
   It never becomes a question to you and never escalates.
5. **The profile is compiled once, at session start, and never changes.**
   `.sterna/` lives inside the writable project, so a profile re-read from
   disk would let a program widen its own sandbox. `.sterna/**` and
   `.claude/**` are write-denied, except `.sterna/scratch/**`, the agent's
   scratchpad. A write to a file with more than one hard link is refused.

## Permission patterns

`permissions.allow` and `permissions.deny` in [configuration](configuration.md):

| pattern | kind | becomes |
|---|---|---|
| `Read(<glob>)` | filesystem | read grant |
| `Write(<glob>)` | filesystem | create and write grant |
| `Edit(<glob>)` | filesystem | read and write on existing files |
| `Bash(<prefix>*)` | command admission | the admitted command's own executables may run |
| `Bash` | command admission | every command line admitted |
| `mcp__<server>__<tool>` | tool admission | that MCP tool may be listed and called |

Paths are resolved before matching: `~` expands, relative paths resolve
against the project root, and every candidate is compared after following
symlinks. Command admission and file authority stay separate: admitting a
command grants none of its data files, writes or network.

## What is never grantable

On every platform, by any pattern, in every mode — `--full-access`
included, because `Profile::check` runs in Sterna's own process before
anything is spawned:

1. **Network for shells and tools.** No pattern names a host, so none can
   grant one. The web tools are a separate host broker ([web](web.md)).
2. **The OS credential store** (Keychain, Secret Service, DPAPI), the
   inference gateway's state directory (its database and subscription
   sign-ins, wherever `INFERENCE_GATEWAY_DATA_DIR` or the platform puts
   it) and, for writing, the machine's own identity files: `/etc/sudoers*`,
   `/etc/shadow`, `/etc/passwd`, `/etc/group`, `/etc/pam.d`, `/etc/ssh`,
   `/etc/security`, and `%SystemRoot%\System32\config`.
3. **Your home outside the project** — `~/.ssh`, `~/.aws`, `~/.claude`,
   `~/.codex`, `~/.config` are examples, not the rule.
4. Any path a `deny` matches, and `.claude/**` / `.sterna/**` for writing.
5. **Process escapes:** debugger attach (`lldb`, `gdb`, `strace`, `dtrace`,
   …) and re-invoking a sandbox launcher (`sandbox-exec`, `bwrap`) from
   inside.

Two grants are derived rather than configured, because an ordinary build
cannot run without them: **toolchain stores**, read and execute only
(`$CARGO_HOME`/`~/.cargo`, `$RUSTUP_HOME`/`~/.rustup`, `~/.npm`, `~/.nvm`,
`~/.pyenv`, `$UV_CACHE_DIR`, but never `cargo login`'s credentials file),
and **a linked worktree's repository** (the `.git/worktrees/<name>` and
common directory its `.git` file points to). `~/.gitconfig` is readable;
`~/.git-credentials` is not.

## Per platform

- **macOS — Seatbelt.** A generated profile applied to each spawned tool
  before `exec`: deny by default, the project and granted roots, the
  `.claude`/`.sterna` write carve-outs, `(deny network*)`.
- **Linux — Landlock and seccomp.** Landlock applies per-path rights;
  seccomp refuses socket creation on x86_64 and aarch64 (local Unix-socket
  build daemons are unavailable too). Landlock has no globs, so
  `Read(**/*.rs)` becomes a grant on the enclosing directory and the
  extension filter is enforced by Sterna's own pre-call check. Landlock
  cannot subtract a denied path inside a writable root.
- **Windows — an AppContainer** entered at `CreateProcessW`, with no
  capabilities (`internetClient` included). The project's ACL grants the
  container's SID, which is derived from the project *and* the user. A job
  object ties a command's children to it for cancellation; it is not a
  sandbox. `bash` runs under `cmd.exe`. Tools that shell out to MSYS2
  binaries (Git for Windows' coreutils) cannot start inside an AppContainer.

On all three, the OS layer renders the project root, added roots and the
carve-outs rather than every allow and deny pattern; the finer patterns are
enforced by the in-process check before anything spawns.

## Refusal

    PermissionDenied: read("/Users/you/.ssh/id_ed25519")
      rule: no grant covers this path; the project root is the only readable root
      tool: read

A JavaScript exception inside the cell: the program may catch it and go on;
the runtime does not end the turn, retry or ask you. `rule` names the
deciding rule so the settings can be fixed without re-deriving the profile.

## Request modes

`execute` (the session profile unchanged), `explore` or `plan`, chosen by
`--mode`, `--plan` or `/mode`. **A mode narrows and never widens**: it is
asked only after the never-grantable set, `deny` and `allow` have admitted a
call.

- **`explore`** — reading tools run; `write`/`edit` only under
  `.sterna/scratch/**` and `modes.explore.writable`; `bash` runs only a
  read-only command (`ls cat head tail wc grep rg find stat file git du df
  ps env which pwd echo date uname`, `git` limited to
  `status log diff show blame ls-files`), per segment, with no redirects,
  substitutions, variable prefixes or writing flags. `modes.explore.commands`
  adds patterns.
- **`plan`** — the same, plus one write: `.sterna/scratch/plan.md`. The next
  request outside `plan` carries it once as a `## Plan` system section.
- On Windows `bash` is `cmd.exe`, whose lines are not parsed, so every
  `bash` call is refused in both modes.

A confident read-only request may *propose* `explore` for one request when
the mode is not pinned ([decisions](decisions.md)).

## How often you are asked

A second axis beside the mode: of what is already admissible, how much is
put to you before it runs. **A rung never widens a grant.** Four rungs, in
the order Shift-Tab cycles them:

- **`manual`** — every admitted foreground file and shell call is confirmed
  (`--ask-approval` is this rung).
- **`accept-edits`** — file tools run; every `bash` line is confirmed.
- **`auto`** (default) — edits run; a `bash` line a static reader can vouch
  for (the read-only list above plus ordinary build verbs such as
  `cargo test`, `cargo fmt`, `sed -n`) runs; anything else is confirmed.
- **`full`** — nothing is confirmed.

Set by `--permissions`, `permissions.mode`, `/permissions` or Shift-Tab. One
exact action gets one answer per session. With no terminal to ask at,
`manual` and `accept-edits` refuse to start, and `auto` runs what it would
have confirmed and says so.

**The confirmation** shows the exact checked arguments: `o` once, `s` this
exact action for the session, `d` or Escape deny, `a` ask the model for
another way. Paste and Enter cannot approve. An action whose arguments do
not fit the 16 KiB display is deny-only. Waiting pauses the cell's clock,
and a confirmation expires after ten minutes. Web, MCP, background jobs and
subagents are outside this gate.

## Wider on purpose

- **`--add-dir PATH`** grants one more existing directory for the session
  (macOS and Linux; refused on Windows and together with filesystem deny
  patterns). Its `.claude`/`.sterna` stay write-denied; its siblings are
  not granted.
- **`--full-access`** removes every question and Sterna's own OS
  confinement — the machine becomes the boundary. The never-grantable set
  above still applies. It is a command-line flag or the global
  `permissions.full_access` setting, never something a cell can reach.

There is no per-command "run this one outside the sandbox" today. A command
that needs the network (`cargo fetch`, `npm install`) has three routes: run
it yourself, use `--full-access` for the session, or run Sterna inside a
container that is the boundary.
