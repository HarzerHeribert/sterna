//! `pane session`'s flag set.
//!
//! Its own file because it is a declaration and nothing else: every flag
//! here is read by `session::run` or by one of `session::startup`'s
//! resolvers, and none of them is decided in this module. Split out of
//! `session.rs` on 2026-09-18 when that file met the size ratchet — a split
//! signal, not a shorten signal.

use std::path::PathBuf;

use clap::Parser;

use super::output;
use crate::tui;

/// `pane session`'s whole flag set. A project root and a way to identify the
/// rollout file are the only things every run needs; `--task` is the
/// non-interactive, scriptable entry point this package's own acceptance
/// tests drive (`env!("CARGO_BIN_EXE_pane")` subprocesses can pipe a task in
/// as an argument far more simply than as timed stdin), and the same flag is
/// what the ruler's future `pane` harness row will pass a statement through.
/// Absent `--task`, terminals use the live composer; piped input is read
/// one input per line until EOF.
#[derive(Parser, Debug)]
#[command(name = "pane session")]
pub struct SessionArgs {
    /// The project root map line 2448 loads from.
    #[arg(long)]
    pub root: PathBuf,

    /// One scripted user input (a slash command or a task) run once, non-
    /// interactively. Omitted opens the live composer on a terminal, or reads
    /// piped inputs one per line until EOF.
    #[arg(long)]
    pub task: Option<String>,

    /// Machine formats require one-shot --task; progress records are versioned JSON.
    #[arg(long, value_enum, default_value = "text")]
    pub output_format: output::Format,

    /// Initial request model; can also be changed with /model.
    #[arg(long)]
    pub model: Option<String>,

    /// Which entry points the model is shown: `cells` (default, `execute_cell`
    /// only), `hybrid` (both) or `tools`. Visibility only, one executor.
    #[arg(long, value_parser = crate::abi::Interface::parse)]
    pub interface: Option<crate::abi::Interface>,

    /// Input context capacity for the initial model. Pane does not guess
    /// provider-specific limits; switching models makes the capacity unknown
    /// until a future catalogue supplies per-model metadata.
    #[arg(long, value_parser = clap::value_parser!(u64).range(1..))]
    pub context_window_tokens: Option<u64>,

    /// Resume a session by the id `/exit` prints, or the newest in this
    /// folder when given no value. A bare `pane` always starts a new one.
    #[arg(long, value_name = "ID", num_args = 0..=1, default_missing_value = "")]
    pub resume: Option<String>,

    /// List this folder's resumable sessions, newest first, and exit.
    #[arg(long)]
    pub sessions: bool,

    /// Where turns are appended, overriding the resolved session's own file.
    #[arg(long)]
    pub rollout: Option<PathBuf>,

    /// This session's id: the name `--resume` takes. Defaults to a
    /// generated one -- `--resume` is why it is no longer the process id.
    #[arg(long)]
    pub session: Option<String>,

    /// The `inference-gateway` executable this session's provider traffic and
    /// its entitlement, subscription and routing-cost controls go through.
    ///
    /// Bare `"inference-gateway"`, absent this flag, resolves through `PATH`.
    /// **It is only spawned when `ANTHROPIC_BASE_URL` is unset**: a set URL
    /// means a gateway is already serving and pane attaches to it
    /// (`gateway::start_or_attach`).
    #[arg(long)]
    pub gateway: Option<PathBuf>,

    /// Grant the whole project root and every command line, retaining native
    /// permission denials and the never-grantable set.
    ///
    /// **This is the person widening their own grant at session start, which
    /// is the only widening `sandbox-grants.md` §1.1 permits** — it is a flag
    /// on the command that starts the session, never something a cell can
    /// reach, ask for or set. It compiles a synthesised settings document
    /// rather than adding a second way to build a profile, so §4's
    /// never-grantable set still applies: a debugger is refused under
    /// `--yolo` exactly as it is without it.
    #[arg(long)]
    pub yolo: bool,

    /// Skip Pane's native OS child-process confinement after an explicit
    /// acknowledgement. Intended for disposable benchmark/CI containers
    /// whose outer runtime is the security boundary; requires --yolo.
    ///
    /// The long-standing spelling of one half of [`SessionArgs::full_access`],
    /// kept because benchmark harnesses and CI files already name it.
    #[arg(long)]
    pub dangerously_bypass_os_sandbox: bool,

    /// **Full access: the widest session Pane has, under one name.**
    ///
    /// Three separate choices had three separate flags, and a person who
    /// wanted all three had to know all three and pair them correctly
    /// (the user, 2026-09-18: *"voller Zugriff ist das einzige was Sinn macht
    /// in modernen Zeiten"*, and *"ich will nicht wie ein Affe dasitzen und
    /// 'y' tippen"*). This is the one word for it: the project root and
    /// every command line are admitted (`--yolo`), no question is put to the
    /// person (`--permissions full`), and Pane applies no OS confinement of
    /// its own to the children it spawns
    /// (`--dangerously-bypass-os-sandbox`).
    ///
    /// **It is still not unrestricted, and this is not a slogan.** Every one
    /// of those three is a *removal of a question*, never a widening of a
    /// grant. `Profile::check` runs unchanged on every path, before the
    /// container-mode reading grant and before anything is spawned, so §4's
    /// never-grantable set is exactly as refusing here as it is in a session
    /// started with no flags at all: no network, no `~/.ssh`, `~/.aws`,
    /// `~/.claude`, `~/.codex` or `~/.config`, no registry credential inside
    /// the toolchain, no sandbox launcher. What changes is that the outer
    /// machine — a container, a VM, or the person's own trusted workstation
    /// — becomes the boundary underneath all of that instead of the seatbelt
    /// or Landlock layer.
    #[arg(long)]
    pub full_access: bool,

    /// Ask before admitted foreground file/shell tools. O allows once, S
    /// remembers this exact call, D denies. Web, MCP, background and agents
    /// are excluded; this never grants additional permissions.
    ///
    /// The alias for `--permissions manual`, kept because it shipped first.
    #[arg(long)]
    pub ask_approval: bool,

    /// How often you are asked: `manual`, `accept-edits`, `auto` (default)
    /// or `full`. Shift-Tab cycles it in a live session and `/permissions`
    /// sets one. A rung never widens a grant.
    #[arg(long, value_parser = |w: &str| crate::permissions::Rung::parse(w)
        .ok_or("manual, accept-edits, auto or full"))]
    pub permissions: Option<crate::permissions::Rung>,

    /// Start in planning mode: reads run, no change executes. Same as `--mode plan`.
    #[arg(long)]
    pub plan: bool,

    /// Start in `execute`, `explore` (read-only shell, writes only to scratch
    /// and documentation globs) or `plan`.
    #[arg(long, value_parser = |w: &str| tui::Mode::parse(w).ok_or("execute, explore or plan"))]
    pub mode: Option<tui::Mode>,

    /// Select [profiles.NAME] in pane.toml over the base configuration.
    #[arg(long)]
    pub profile: Option<String>,

    /// Attach a local PNG, JPEG, GIF, or WebP to the first task (up to four).
    #[arg(long = "image", value_name = "PATH")]
    pub images: Vec<PathBuf>,

    /// Grant an additional existing directory for this session only.
    #[arg(long = "add-dir", value_name = "PATH")]
    pub additional_dirs: Vec<PathBuf>,
}
