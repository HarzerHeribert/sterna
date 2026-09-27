# Permissions, modes and the sandbox: who decides what

A read-only map of how a call is allowed to run in `crates/sterna`, written
to plan a cleanup. It describes the code at `7200eb5` (the head of PR #3,
`claude/tui-packages-lb60ai`); nothing here changes behaviour.

Paths are relative to `crates/sterna/src`. Claims marked **✔** were re-read
in the source while writing this; the rest come from four independent
read-throughs (gate, modes, sandbox, entry points) and agree with each other,
but were not each re-opened.

## 1. The four layers

Four separate mechanisms decide whether a call runs. They were built at
different times and each one's comments describe itself as "the boundary".

| Layer | Question it answers | Type | Where | Built from |
|---|---|---|---|---|
| **Profile** | Is this admissible at all? | `Profile` (one per session, immutable) | `sandbox/profile.rs` | project root, `$HOME`, never-grantable set, `permissions.allow`/`deny`, derived toolchain/git grants, `--yolo`/full access, `--add-dir` |
| **Mode narrowing** | Is this allowed *for this request*? | `RequestMode {Execute, Explore, Plan}` → `Narrowing` | `sandbox/modes.rs:19`, `profile.rs:596` | session mode, or the per-request proposal; `[modes.explore]` overlay |
| **Rung** (the ladder) | Does a person see it first? | `Rung {Manual, AcceptEdits, Auto, Full}` in a `Ladder` | `permissions.rs:29,199` | `--permissions`, `--full-access`, `--ask-approval`, `permissions.mode`, `permissions.full_access` |
| **OS confinement** | What can the spawned process physically do? | Landlock+seccomp / Seatbelt / AppContainer | `sandbox/{linux,macos,windows}.rs`, `tools/invoke.rs:2072` | the Profile's `check()` (never the mode, never the rung) + an exec grant per line |

Two more things sit beside them and are easy to confuse with them:

- **The decision model** (`[decisions]`, `decide.rs`): may *vouch* for a command on
  Auto, may *propose* Explore for one request, and shows a "fits" hint on the
  prompt. Three separate uses, one counter (`approval.rs:194`).
- **The web broker** (`web.rs`): all network the session has. The shell never
  has any (seccomp / `deny network*` / no AppContainer capability), except
  under the OS bypass.

### Order for one foreground `bash` call (attended TUI) ✔

```
invoke::checked_call (tools/invoke.rs:705)
 ├─ judged = gate present && !unattended
 ├─ Profile::weigh_command (judged)  or  Profile::admits_command (not judged)   profile.rs:903/926
 │    invalid root → refuse
 │    launcher/debugger → refuse
 │    permissions.deny match → refuse
 │    not in permissions.allow → "unlisted" (weigh)  /  refuse (admits)
 │    mode narrowing: command_reads_only fails → refuse                        modes.rs:264
 ├─ Gate::admit                                                                 approval.rs:683
 │    stopped → Cancelled
 │    person said Deny earlier → DeniedEarlier
 │    person said Allow for session → Allowed
 │    permissions::judge(rung, …)                                               permissions.rs:294
 │       Manual → Ask · Full → Runs · AcceptEdits → Ask for bash, Runs otherwise
 │       Auto → command_reads_only(DEVELOPMENT_COMMANDS + modes.explore.commands)
 │              → Runs, else decision model may vouch → Runs, else Ask
 │    Ask && unattended → Allowed   (unreachable, see F3)
 │    Ask → prompt (session/ui/decision.rs), up to 10 min
 └─ spawn: exec grant from weigh_command again (invoke.rs:1671), OS confinement
```

Who can override whom: **never-grantable & deny > mode > allow-list-or-rung >
the person's earlier answer > static reader > decision model (can only turn
an Ask into a Run) > the person.**

## 2. Execution kinds: which layers apply

| Execution | Admission | Gate / rung | Mode | OS sandbox | Network |
|---|---|---|---|---|---|
| `bash`, foreground cell, attended TUI | `weigh_command` (unlisted → rung) | yes | yes | confined | none |
| `bash`, headless `-p`/`exec`/`--task` | `admits_command` (unlisted → refused) | **no gate at all** | yes | confined | none |
| `bash` inside a subagent (`agent.run`) | `admits_command` | no (`bindings.rs:901`) | parent's narrowed profile | confined | none |
| `bg.run` / `bg.watch` | `admits_command` (`bg.rs:274,296`) | no | parent profile | confined | none |
| Verification checks, acceptance runner | `admits_command` (`verification.rs:407`, `session/task.rs:794`) | no | session profile | confined | none |
| `/tool` typed by the person | `invoke::run` | no | narrowed | confined | none |
| `read` / `grep` / `rg` / `fd` / `jq` | profile path check | yes (Manual asks even for these) | yes | confined child (in-process `read`/`grep` on Windows) | none |
| `write` / `edit` / `glob` / `context` | `Profile::check` | yes | yes | **in-process**, no OS layer ("ran inside Sterna") | – |
| MCP stdio server | profile `mcp__…` patterns | **no** | refused entirely in Explore/Plan | confined (so silently no network) | none |
| MCP remote, `web.fetch`, `web.search` | `[web]` domain policy | no | no | host process | yes, via broker |
| Helpers (no tools) | `profile.check` on file reads | no | – | in-process | model calls only |
| Sterna's own git, gateway, editor, sign-in, update | – | – | – | **unconfined** host spawns | yes |
| Ruler benchmark harnesses | – | – | – | unconfined; claude/codex get their bypass flags, Sterna gets whatever the machine's global settings say | yes |
| Hooks | no hook runner exists; only comments mention one | | | | |

The same `cargo test` is **judged** in foreground `bash`, **refused** in
`checks.run`, a subagent, `bg.run` and a headless run, unless a
`Bash(cargo test*)` allow pattern exists.

## 3. Build / Explore / Plan

`RequestMode` is spelled `execute` in code, `build` in the settings file, and
`Build` on screen (`modes.rs:29-94`).

| | Build (`Execute`) | Explore | Plan |
|---|---|---|---|
| Narrowing | none | yes | yes |
| Writes | profile unchanged | `.sterna/scratch/**` + `modes.explore.writable` | exactly `.sterna/scratch/plan.md` |
| Commands | profile unchanged | `command_reads_only` + `modes.explore.commands` | same, via the *explore* key |
| MCP | profile | all refused | all refused |
| `ask()` from a cell | allowed | refused | **allowed** |
| OS layer | unchanged | **unchanged**: `check()` never sees a mode | unchanged |
| Prompt | no line | "Request mode: explore…" | its own line; next request gets `## Plan` |
| Rung | independent | independent | independent |

"Auto" for a mode means *unpinned*: `mode_proposal::propose`
(`session/mode_proposal.rs:40`) may narrow one Build request to Explore when
a decision model exists, `decisions.mode = on` (the default is `shadow`, so
by default it never acts), the intent is `read_only` and confidence ≥ 0.85.
It never picks Plan and never changes the session's mode.

`/mode auto` and `/permissions auto` are unrelated. "auto" is also a
subagents mode and a preflight scope.

## 4. Settings and flags

| Key / flag | Default | Scope | Takes effect | Feeds |
|---|---|---|---|---|
| `permissions.mode` | `auto` | **global and project** ✔ | live | rung |
| `permissions.full_access` | false | global only (`GLOBAL_ONLY`, `registry.rs:136`) ✔ | next session | yolo + unconfined + Full rung |
| `permissions.allow` | none | a project list **replaces** the global one ✔ | next session | profile |
| `permissions.deny` | none | unioned across scopes ✔ | next session | profile (kept under full access) |
| `session.mode` | `build` | both | live via `/mode` | request mode |
| `modes.explore.writable` | – | both | next session | Explore writes |
| `modes.explore.commands` | – | both | next session | Explore **and** Plan commands **and** the Auto rung's extra list (`startup.rs:209`) |
| `decisions.mode` / `.model` | `shadow` | both | next session | vouching, mode proposal, hint |
| `decisions.command_runs_above` | 0.85 | – | – | no `SettingSpec`: not in Settings |
| `web.*` | off | both | – | broker only |
| `--permissions R` | | | | rung |
| `--ask-approval` | | | | alias for `--permissions manual` |
| `--full-access` | | | | yolo + bypass + Full; rejects a different `--permissions` |
| `--yolo` | | | | profile = root R/W/E + bare `Bash` + denies |
| `--dangerously-bypass-os-sandbox` | | | | requires `--yolo`; also means "container mode" (whole-FS reads, debuggers admissible) |
| `--mode` / `--plan` | | | | pin the mode; `--plan` silently wins over `--mode` |

Rung at start (`session/startup.rs:151-162`) ✔: `--permissions`, then
`--full-access`, then `--ask-approval`, then `permissions.mode` (either
scope), then `permissions.full_access`, then `auto`. Since every rung move
saves `permissions.mode`, the `full_access` fallback almost never decides.

### Routes that move the rung (P8 promised one setter)

| Route | Calls | Saves | Confirms Full |
|---|---|---|---|
| Ask sheet, chip, typed `/permissions R` in the workbench, Settings row | `facts::set_rung` (`workbench/facts.rs:42`) | yes | yes |
| `ConfirmRung` | a copy of `Input::rung`'s body (`input.rs:1182`) | yes | – |
| Shift-Tab | `rung_change` (`session/ui.rs:643`), skips Full by hand | yes | skipped |
| `/permissions R` on the session thread (not caught by the workbench, or a Settings live command) | `ladder.set` (`session/controls.rs:1507`) | **no** | **no** |
| Undo / restore | `s.permissions.set` (`workbench/settings.rs:382`) | – | no |
| `Ladder::cycle` | no production caller | | would wrap into Full |

Mode: the chip and sheet send `/mode X`, which pins it and saves it
(`facts::saving`). A value loaded from the file is **not** pinned ✔
(`session.rs:599-601`). `/mode auto` saves nothing ✔. Headless `/mode` never
saves.

## 5. What the model is told vs what happens

| Told | Source | True? |
|---|---|---|
| Writable roots, command pattern count | `Profile` (`prompt/mod.rs:614`) | yes |
| "no command may be run at all" when no `Bash` pattern exists | `prompt/mod.rs:620`, `manifest.rs:181` | **no** in an attended session: unlisted lines are judged or asked |
| "PermissionDenied is final" | `prompt/mod.rs:111,633` | no: the person may answer differently after a mode or rung change |
| "network: no" | `profile.grants_network()`, hard-coded `false` ✔ | **no** under the OS bypass: a plain `spawn()` ✔ |
| Request-mode line | `READ_ONLY_COMMANDS` + overlay | yes, **but** it is built from `session.mode` before the proposal runs ✔ (`session.rs:1277` vs `:1286`), so an auto-narrowed request is refused without being told |
| The rung, asking, unattended | nothing | the model never learns the rung |
| "DANGER … disabled by an explicit CLI bypass" | `session/system.rs:60` | also triggered by the settings key; not given to subagents |

The person-facing rung text (`Rung::sentence`, `Rung::asks`,
`permissions_line`) is hand-written. It is not generated from `judge`, which
P2 of the audit asked for.

## 6. Findings, most serious first

**F1 · A cloned repository can switch asking off.** ✔
`permissions.mode` is not in `GLOBAL_ONLY` (`registry.rs:136`), and
`startup.rs:156-161` even says a project file's value wins. A repo carrying
`.sterna/` settings with `permissions.mode = "full"` starts the session on
Never asks. In an attended session `weigh_command` treats unlisted lines as
judgeable and Full runs them, so any non-denied command runs without a
prompt (inside the OS sandbox, without network). A project `permissions.allow`
also *replaces* the global list, and the comment calling that "narrowing"
(`settings.rs:398`) is wrong: a project can add `Bash`. Chained with F4, a
command in one session can rewrite `.sterna/` settings for the next.

**F2 · "A rung never widens a grant" is no longer true.** ✔
Since P2, `weigh_command` admits lines that no `permissions.allow` pattern
names; on Auto they run if the reader places them, on Full they all run. But
`permissions.rs:4-9`, `approval.rs:6-11`, `registry.rs:365`,
`docs/sandbox.md:130` and the model's prompt all still promise the opposite.
The admission layer and the rung now overlap, and nobody owns the boundary
between them.

**F3 · Two admission predicates, chosen by an unreachable flag.** ✔
`admits_command` vs `weigh_command` is picked by `judged = gate && !unattended`.
A gate exists only in the TUI (`session.rs:765`), and the TUI condition is
exactly the "attended" one (`startup.rs:167`), so `unattended` is never true
where a gate exists. The unattended branch in `Gate::admit` is dead, and
headless runs *refuse* unlisted commands, while `permissions_line` and
`docs/sandbox.md:143` say "what would be confirmed runs". Every other run
kind (bg, checks, acceptance, subagents, `/tool`, verification reuse, the
`bash`→`read` lift at `bindings.rs:819`) hard-codes the strict one.

**F4 · The OS layer and the 09-19 "call-level judgement" ruling disagree.** ✔
The ruling changed admission but not confinement. Without bare `Bash`, the OS
exec grant covers only each segment's literal first word (`linux.rs:193`,
`invoke.rs:1666`), so a judged-and-approved `cargo test` still cannot exec
`rustc` or the linker (`startup.rs:108` records exactly that happening). The
only way out is full access, which is the "cage or nothing" choice the
ruling rejected. On Linux, Landlock cannot carve `.sterna/`/`.claude/` out
of a writable root (`linux.rs:245,318`); the bubblewrap view that could is
built only in tests (`bwrap_argv`, `available_regime`).

**F5 · Four places set the rung, and two skip save and confirm.** See the
route table. `/permissions` on the session thread neither saves nor confirms
Full; undo writes the ladder directly; `ConfirmRung` duplicates `rung()`;
Shift-Tab skips Full by hand while `Rung::next` is documented as "Shift-Tab's
whole definition".

**F6 · The mode is decided twice per request.** ✔ The prompt line and
`observe.task_begin` use `session.mode`; the profile and the `ask` refusal use
`proposal.narrow_mode`. The chip never shows an auto-narrowing.

**F7 · Mode persistence depends on the route.** ✔ A sheet-chosen Plan is
saved and silently starts every later session in Plan (unpinned). A pinned
Build reloads as unpinned. `/mode auto` cannot be saved.

**F8 · One settings key does three jobs.** `modes.explore.commands` is the
Explore list, the Plan list, and the Auto rung's extra read-only list. The
Auto judge ignores `permissions.allow` entirely, so a person's own
`Bash(npm test)` is still asked about on Auto.

**F9 · "Full access" is three things with one name.**
`permissions.full_access` / `--full-access` = `--yolo` (profile widened to
bare `Bash`, and MCP allows dropped, `session.rs:2779`) + the OS bypass (which
also means container mode: whole-FS reads, debuggers) + the Full rung. The
TUI's "FULL ACCESS · nothing is confined" follows `yolo`, not the bypass
(`session.rs:791`). The Confirm sheet says "approval prompts still apply", the
registry says "no question asked", and the startup notice and
`docs/sandbox.md` say network is unchanged, which is false.

**F10 · Dead or test-only code.** `Verdict::Refuse` ✔, the `Allowed` arm of
`judged` (it never stores `true`), `Gate::session_actions`,
`Ladder::cycle`, `RequestMode::next`, `Profile::request_mode`,
`CommandGrant::listed`, the Linux bubblewrap regime and `Regime::describe`
("the sentence a session prints at start-up", printed by nothing).

**F11 · Naming.** `execute`/`build`/`Build`; `accept-edits`/`Commands`;
"auto" for four unrelated things; "network" meaning the web broker on the
chip and the shell in the prompt; `command_reads_only` names its result
`refuse` though the ladder turns it into Ask. Legacy aliases remain
(`--ask-approval`, `Rung::parse`'s `acceptedits`, the `execute` word in
refusal texts and CLI help), against the no-legacy rule.

**F12 · Docs describe an older system.** `docs/sandbox.md` and the
`profile.rs` header say the profile comes from `.claude/settings.json`
(sessions use the TOML store, `session.rs:579-584`) and that hooks fire (no
runner exists). The 09-19 ruling is quoted in code but not recorded in
`docs/audit/decisions.md`. `profile.rs:580` says the bypass is refused off
Linux; it has been accepted on macOS and Windows since 09-18.

## 7. Where a cleanup could go

Stated as decisions for the owner. None of this is implemented.

**Decision 1: what is the boundary a person configures?**

- **A. Rung only, no command allow-list for the foreground.** The profile keeps
  paths, denies and never-grantables; `permissions.allow`'s `Bash(...)`
  patterns become the Auto rung's "runs without asking" list (merging
  `DEVELOPMENT_COMMANDS` and `modes.explore.commands` into it). Every run kind
  without a person (bg, checks, subagents, headless) uses the same judge with
  Ask mapped to Refuse. One predicate, one list. Costs: a contract change in
  `sandbox_profile` tests and the docs; headless behaviour must be decided
  explicitly.
- **B. Keep the allow-list as the boundary; the rung only asks.** Revert P2's
  widening: `weigh_command` goes, unlisted lines are refused everywhere, and
  Auto only decides whether a *listed* line is shown first. Costs: undoes the
  09-19 ruling; a fresh project runs almost nothing until patterns are written.
- **C. Keep both, but make them honest.** Leave the behaviour, fix F1, F3, F5
  and all the texts. Cheapest, but it keeps two predicates and three lists.

**Decision 2: what does the OS layer enforce?**

- **A. Paths and network only, not exec.** Exec rights cover the roots and
  toolchains whenever the call was admitted, so an approved `cargo test` can
  link. The OS layer stops being a second allow-list. Costs: a wider exec surface
  inside the sandbox (still no network, still path-confined).
- **B. Keep per-word exec grants** and document that full access is how you
  build. Costs: F4 stays.

**Decision 3: modes.** Either make modes a property of the OS profile too
(Explore/Plan enforced by `check()`, so a read-only command admitted in Plan
cannot write through a side door), or state plainly that modes are a
call-level contract only. Separately: rename `Execute` to `Build` everywhere,
give Plan its own command key or drop the Explore/Plan asymmetry, and decide
whether a saved Plan should survive into the next session.

**Independent of those, and small:**

1. Add `permissions.mode` to `GLOBAL_ONLY` (or refuse a project value that
   lowers asking) and make a project `permissions.allow` intersect, not
   replace (F1).
2. One rung setter that saves and confirms, used by every route including
   `controls.rs:1507` and undo; delete `Ladder::cycle` or make Shift-Tab use
   it (F5).
3. Build the prompt's mode line from `proposal.narrow_mode` (F6).
4. Delete the dead code in F10 and the unreachable unattended branch, or make
   headless runs actually use a gate (F3).
5. `grants_network()` answers from the bypass bit; fix the full-access
   network texts (F9).
6. Split the OS-bypass bit from "container mode" (F9).
7. Rewrite `docs/sandbox.md` against the TOML store and record the 09-19
   ruling in `docs/audit/decisions.md` (F12).

## 8. Unattended runs

Every level in section 7 relies on someone answering "ask". A long run with
nobody at the terminal breaks that:

- **Ask** stalls on its first question.
- **Auto** must either refuse whenever the classifier is unsure (so the run
  stalls or gives up), or run anyway (so the classifier alone is trusted for
  hours).
- **Full access** works, but on the person's real machine, with their
  credentials and network.

Today a headless run refuses every command no `Bash(...)` pattern names (F3),
so a long run either stalls or is started with `--full-access`.

**Codex and Claude Code solve this with a box, not a better judge.** For hands-off
runs both put the agent in a disposable environment: Codex points at a
container, and Claude Code recommends `--dangerously-skip-permissions` only
inside a container and ships a reference devcontainer. Inside the box nothing
is asked; only what leaves it is controlled:

| Leaving the box | Control |
|---|---|
| Network | the web broker's domain list (registries, the git remote) |
| Git | push to the run's own branch only; never the default branch, never force |
| Secrets | only those the task names, handed in on purpose |
| The result | a branch and a report, reviewed afterwards |

The classifier then guards only the exits and refuses when unsure, because a
stall at an exit is cheap.

**Local boxes, strongest first.**

| Box | Available on | Cost |
|---|---|---|
| VM (Lima, WSL2, Hyper-V) | all three | heaviest; strongest separation |
| Container (Docker, Podman, devcontainer) | Linux natively; macOS and Windows through a VM | needs a runtime; the toolchain must be in the image |
| Separate worktree + today's OS sandbox | everywhere, already built | lightest; brings back the per-program exec fight (F4) |

**What exists already:**
- the OS-bypass flag already doubles as "container mode" (`profile.rs:620`),
  which assumes the box is the boundary;
- the ruler already runs Sterna headless in worktrees;
- the supervisor watches for loops;
- change snapshots and `/rollback`;
- the web broker controls network by domain.

**What is missing:**
- Sterna starting the box itself. For example, `sterna run --boxed "task"`
  would create a worktree, start a container with only that worktree mounted,
  run Sterna inside it with full access, send network through the broker, and
  hand back a branch and a report. Without a container runtime it falls back
  to the worktree and the OS sandbox, and says so.
- Rules for pushing and for secrets in that mode.
- The end-of-run review surface.

Nothing here is decided or built.
