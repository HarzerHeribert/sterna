# The sandbox

The model's TypeScript is already contained: a V8 isolate has no
filesystem, no sockets and no ambient authority. This page is about the
other half — the tools a program calls, which spawn real processes and
touch real files. Code: `crates/sterna/src/sandbox/{profile,proxy,macos,linux,windows}.rs`,
`src/permissions.rs`, `src/approval.rs`.

## One setting

How much runs without asking is one setting, `sandbox.level`, with three
levels:

| Level | Sandbox | Asks |
|---|---|---|
| **Ask** | on | before every edit and every command; reading runs |
| **Sandboxed** (default) | on | only when a command asks to leave the sandbox |
| **Full access** | none | nothing |

It is chosen on the Sandbox sheet (the level chip in the top bar, F2 or
`/sandbox`), with `--sandbox ask|sandboxed|full` for one session, and saved
to your **global** settings only: a project's own settings cannot set it,
because cloning a repository must never be enough to turn questions off.
Full access is confirmed on a sheet that opens on Cancel, and the chip stays
red while it is on. A change applies from the next request.

With nobody at the terminal (`sterna -p`, `--task`), `ask` refuses to
start, and on Sandboxed a request to leave the sandbox is refused rather
than asked; the run carries on inside.

## What the sandbox allows

| | Runs without asking | Refused |
|---|---|---|
| Read | every file your user can read | the secrets below, and any `deny` pattern |
| Write | the project, `--add-dir` roots, a worktree's repository, temp folders, and tool caches and toolchains (`~/.cargo`, `~/.rustup`, `~/.npm`, `~/.bun`, pnpm's store, `~/.cache`, `~/.m2`, `~/.gradle`, `~/go`, …) | the rest of your home folder, other repositories, system folders; `.sterna/**` (except `.sterna/scratch/**`) and `.claude/**` |
| Run | any program: compilers, linkers, test runners | sandbox launchers; debuggers outside Full access |
| Network | the allowed hosts, through Sterna's proxy | any other host |

**Every command line runs** unless a `Bash(...)` pattern in
`permissions.deny` or a never-grantable name refuses it. There is no list of
admitted commands to maintain; `Bash(...)` patterns in `permissions.allow`
pre-approve commands on the Ask level.

## Never grantable

On every level, by any pattern — Full access included, because
`Profile::check` runs in Sterna's own process before anything is spawned:

1. **Secrets:** `~/.ssh`, `~/.aws`, `~/.gnupg`, `~/.config`, `~/.claude`,
   `~/.codex`, the OS keyring (Keychain, Secret Service, DPAPI), a
   toolchain's registry credentials (`~/.cargo/credentials.toml`), and the
   inference gateway's state directory.
2. **The machine's identity files, for writing:** `/etc/sudoers*`,
   `/etc/shadow`, `/etc/passwd`, `/etc/group`, `/etc/pam.d`, `/etc/ssh`,
   `/etc/security`, `%SystemRoot%\System32\config`.
3. Any path a `deny` matches.
4. **Process escapes:** re-invoking a sandbox launcher (`sandbox-exec`,
   `bwrap`) from inside, and outside Full access attaching a debugger
   (`lldb`, `gdb`, `strace`, `dtrace`, …).

A write to a file with more than one hard link is refused.

## Allowed hosts

Commands reach the network only through Sterna's proxy
(`HTTPS_PROXY`/`HTTP_PROXY` point at it), which lets through the allowed
hosts and refuses the rest. The rule is the host, not the tool: every client
of a registry works. On by default, one switch per ecosystem
(`sandbox.ecosystems`), plus your own `sandbox.hosts`; both are global.

| Ecosystem | Hosts | Clients, for example |
|---|---|---|
| Rust | `crates.io` `index.crates.io` `static.crates.io` `static.rust-lang.org` | cargo, rustup |
| JavaScript | `registry.npmjs.org` `registry.yarnpkg.com` `nodejs.org` | npm, pnpm, yarn, bun |
| Deno, JSR | `jsr.io` `deno.land` `dl.deno.land` | deno |
| Python | `pypi.org` `files.pythonhosted.org` | pip, uv, poetry |
| Go | `proxy.golang.org` `sum.golang.org` | go |
| Java, Kotlin | `repo.maven.apache.org` `repo1.maven.org` `plugins.gradle.org` `services.gradle.org` | maven, gradle |
| Ruby | `rubygems.org` `index.rubygems.org` | gem, bundler |
| .NET | `api.nuget.org` | dotnet |
| PHP | `repo.packagist.org` | composer |
| Source hosts | `github.com` `codeload.github.com` `objects.githubusercontent.com` `raw.githubusercontent.com` `gitlab.com` | git over https, installers |

The proxy sees the host name, not whether a request downloads or uploads,
so only registries and source hosts are listed, and your credentials never
enter the sandbox: without them, publishing or pushing fails. `--allow-host
HOST` adds a host for one session. The model's own web tool keeps its
separate `[web]` list ([web](web.md)).

**The Allowed hosts sheet** (Sandbox sheet › Allowed hosts) has one switch
per ecosystem and your own hosts, each removable, with a field to add one
(`api.example.com`, or `*.example.com` for every name under it; a pasted
URL is refused, not half-allowed). Every change is saved to the global
settings at once. When the session runs a proxy it also reaches the proxy's
live list, so the next command sees it; where no proxy runs, it applies
from the next session, and the sheet says which.

## Leaving the sandbox

When a command needs something the sandbox refuses — a host that is not
allowed, a path outside the writable places — the model can call `bash`
again with `outside` set to one sentence saying why. That is the one
question Sandboxed asks. On Ask it is asked like every other command; on
Full access there is nothing to leave.

**Reach a new host.** The proxy's refusal tells the model to name the
refused host in `outside`. When the proxy refused a host since the command
before, the question is titled *Reach a new host* and offers first to let
the host through rather than the command out:

| key | answer | the command runs |
|---|---|---|
| `h` | Allow *host* for this session | again, inside the sandbox |
| `w` | Always allow *host* — also saved to the global `sandbox.hosts` | again, inside the sandbox |
| `o` | Allow once, outside the sandbox | outside, this once |
| `a` | Another way | not |
| `d` | Deny for this session | not |

Without a refused host the question is titled *Leave the sandbox* and has
the answers of every confirmation below. Each command takes the proxy's
refusals before it, so a question never offers a host an older command was
refused.

**With nobody at the terminal** (`sterna -p`, `--task`) a request to leave
is refused, and a run the proxy refused hosts in ends with one line naming
them and the `--allow-host HOST` flag that allows one next time.

**The confirmation** shows the exact checked arguments, every character
and space as they will run: `o` once, `s` this exact action for the
session, `d` *Deny for this session* (remembered, and listed on the
Sandbox sheet where it can be forgotten), Escape *Not now* (this call
only, asked again next time), `a` another way. One line on the sheet says
the difference between the two refusals. A prompt takes no key until it has been on screen for half a second.
An action whose arguments do not fit the 16 KiB display is deny-only.
Waiting pauses the cell's clock, and a confirmation expires after ten
minutes.

## Per platform

- **macOS — Seatbelt.** A generated profile applied to each spawned tool
  before `exec`: reads everywhere but the secrets, the writable places,
  the `.claude`/`.sterna` write carve-outs (and `.git/hooks`/`.git/config`
  once the repository exists, so `git init` still works), and network only
  to localhost, where the proxy listens.
- **Linux — Landlock and seccomp.** Landlock applies per-path rights;
  seccomp limits sockets. Where user namespaces are available, a command
  runs in its own network namespace whose only way out is the proxy, and
  `.git/hooks`, `.git/config` and `.sterna` are mounted read-only; where
  they are not, commands get no network at all and the doctor says so.
  Without namespaces those paths, and a secret that sits inside a writable
  place, are kept from writes by Sterna's own check only (the secret stays
  unreadable, except one inside a temp folder, which only that check
  keeps), and a Sterna running as root has its environment readable
  by the commands it runs.
- **Windows — an AppContainer** entered at `CreateProcessW`, with no
  capabilities (`internetClient` included). Commands have no network there;
  a command that needs it asks to leave the sandbox. `bash` runs under
  `cmd.exe`.

On all three, the finer patterns are enforced by Sterna's own pre-call
check before anything spawns.

## Refusal

    PermissionDenied: read("/Users/you/.ssh/id_ed25519")
      rule: `~/.ssh` is never grantable by any pattern (docs/sandbox.md, never grantable 3)
      tool: read

A JavaScript exception inside the cell: the program may catch it and go on;
the runtime does not end the turn or retry. `rule` names the deciding rule
so the settings can be fixed without re-deriving the profile.

## Planning

`/plan <task>` runs one request that may read everything the sandbox
allows, run read-only commands (`ls cat head tail wc grep rg find stat file
git du df ps env which pwd echo date uname`, `git` limited to
`status log diff show blame ls-files rev-parse`), and write one file:
`.sterna/scratch/plan.md`. The next request works as usual and carries the
plan once as a `## Plan` section. On Windows, `cmd.exe` constructs are
refused while planning.

## Wider on purpose

- **`--add-dir PATH`** makes one more existing directory writable for the
  session (macOS and Linux). Its `.claude`/`.sterna` stay write-denied.
- **Full access** removes Sterna's own OS confinement: the machine becomes
  the boundary, network included. The never-grantable set still applies.
  For long runs with nobody watching, run Sterna inside a container and use
  Full access there.
