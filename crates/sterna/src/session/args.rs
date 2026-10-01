//! `sterna session`'s flag set.
//!
//! Its own file because it is a declaration and nothing else: every flag
//! here is read by `session::run` or by one of `session::startup`'s
//! resolvers, and none of them is decided in this module. Split out of
//! `session.rs` on 2026-09-18 when that file met the size ratchet — a split
//! signal, not a shorten signal.

use std::path::PathBuf;

use clap::Parser;

use super::output;

/// `sterna session`'s whole flag set. A project root and a way to identify the
/// rollout file are the only things every run needs; `--task` is the
/// non-interactive, scriptable entry point this package's own acceptance
/// tests drive (`env!("CARGO_BIN_EXE_sterna")` subprocesses can pipe a task in
/// as an argument far more simply than as timed stdin), and the same flag is
/// what the ruler's future `sterna` harness row will pass a statement through.
/// Absent `--task`, terminals use the live composer; piped input is read
/// one input per line until EOF.
#[derive(Parser, Debug, Clone)]
#[command(name = "sterna session")]
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

    /// Input context capacity for the initial model. Sterna does not guess
    /// provider-specific limits; switching models makes the capacity unknown
    /// until a future catalogue supplies per-model metadata.
    #[arg(long, value_parser = clap::value_parser!(u64).range(1..))]
    pub context_window_tokens: Option<u64>,

    /// Resume a session by the id `/exit` prints, or the newest in this
    /// folder when given no value. A bare `sterna` always starts a new one.
    #[arg(long, value_name = "ID", num_args = 0..=1, default_missing_value = "")]
    pub resume: Option<String>,

    /// A bare `--resume` at a terminal: the resume sheet opens over the
    /// newest session. Set by the session, never typed.
    #[arg(skip)]
    pub pick: bool,

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
    /// means a gateway is already serving and sterna attaches to it
    /// (`gateway::start_or_attach`).
    #[arg(long)]
    pub gateway: Option<PathBuf>,

    /// How much runs without asking, for this session: `ask` (every edit and
    /// command is confirmed), `sandboxed` (everything in the project runs;
    /// leaving the sandbox asks) or `full` (no sandbox, nothing asks). The
    /// saved `sandbox.level` otherwise; `sandboxed` when nothing is saved.
    #[arg(long, value_parser = |w: &str| crate::permissions::Level::parse(w)
        .ok_or("ask, sandboxed or full"))]
    pub sandbox: Option<crate::permissions::Level>,

    /// A host commands may reach for this session, beside the allowed hosts
    /// in settings (`--allow-host api.example.com`). Repeatable.
    #[arg(long = "allow-host", value_name = "HOST")]
    pub allow_hosts: Vec<String>,

    /// Select [profiles.NAME] in .sterna/config.toml over the base configuration.
    #[arg(long)]
    pub profile: Option<String>,

    /// Attach a local PNG, JPEG, GIF, or WebP to the first task (up to four).
    #[arg(long = "image", value_name = "PATH")]
    pub images: Vec<PathBuf>,

    /// Grant an additional existing directory for this session only.
    #[arg(long = "add-dir", value_name = "PATH")]
    pub additional_dirs: Vec<PathBuf>,

    /// Run with no terminal, for clients that reach the session on its port
    /// (`docs/engine.md`): one ready line on stdout says where, and the
    /// session ends when stdin closes. What `sterna host` starts.
    #[arg(long, conflicts_with_all = ["task", "sessions"])]
    pub serve: bool,
}
