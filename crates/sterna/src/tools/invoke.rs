//! One tool call, confined — map line 2455's first caller and map line
//! 2463's per-call half.
//!
//! The invariant: **there is no path through this module that spawns a child
//! outside the sandbox**, and since the AppContainer landed it is structural
//! rather than positional. [`confined_spawn`] takes the `Command` by value,
//! applies the platform's confinement and creates the process, all in one
//! call — so there is no intermediate value a caller could hold and spawn
//! without checking a result, and a platform that cannot confine returns a
//! refusal instead of a process. "Unconfined" is not a degraded mode here, it
//! is a refusal like any other.
//!
//! The second invariant is `sandbox-grants.md` §1.4: **a refusal is a
//! value.** Every refusal below is a returned [`PermissionDenied`]; nothing
//! in this module reads from a terminal, asks a question, retries, or ends a
//! turn.
//!
//! The third is the 61D exec-roots ruling
//! (`.agent-runtime/sterna/ruling-61d-exec-roots.md`): a tool's own resolved
//! executable is a derived input, so the child is spawned on the **resolved**
//! binary and never on the bare name. The platform applier's executable roots
//! are the fallback for a name that cannot be resolved, and [`ExecGrant`]
//! records when that happened so a caller sees it without reading a log.
//!
//! The fourth is cancellation, and it is deliberately the *weakest* thing
//! that works: a [`CancellationToken`] the caller holds, checked once
//! immediately before the spawn and then at a bounded interval while the
//! child runs. It sits **below** the confinement rather than beside it, so
//! the first invariant is untouched — the only expression that starts a
//! child is [`confined_spawn`] itself, and it confines before it spawns.
//!
//! **Nothing model-authored runs here.** The only programs this module can
//! spawn are the spawning entries in [`registry::ALL`], each resolved from a name fixed
//! at compile time; there is no argument, no path and no branch through
//! which assistant text selects or becomes a program.

/// What a broad search covers: the generated trees it steps over, and the
/// output filter for the one that cannot be stepped over by name.
mod broad;
mod process;
/// `read` and `grep` performed inside this process. Used where the registry
/// declares both in-process — Windows — and compiled under `test` on every
/// host so the ordinary gate asserts the matcher and the walker.
#[cfg(any(windows, test))]
mod search;

use broad::{
    contains_component, explicitly_roots_component, filter_grep_artifacts, ignored_directories,
    is_broad_search, is_sterna_artifact, ripgrep_is_installed,
};
use std::collections::BTreeMap;
use std::fmt;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::Duration;

use serde_json::{Value, json};

use crate::contract::SessionId;
use crate::sandbox::profile::{Access, PermissionDenied, Profile};
use crate::tools::registry::{self, ArgKind, Argv, Tool};

/// How much of a tool's output reaches the hook payload.
///
/// `runtime-contract.md` §3 caps a *handle preview* at 256 tokens, and that
/// cap belongs to the runtime that renders handles — it does not exist yet.
/// This is the separate, cruder bound on what one hook delivery may carry,
/// stated in bytes because that is what a truncation can actually be
/// performed on.
const PREVIEW_BYTES: usize = 2048;

/// How often a running call asks whether it has been cancelled.
///
/// The bound is on *latency*, not on the child: 20 ms is far below the two
/// seconds the acceptance test allows and far above anything that would make
/// the poll itself measurable next to a process that is doing work.
const CANCEL_POLL: Duration = Duration::from_millis(20);

/// The cancellation facility, and it is the whole of it: a flag whoever
/// started the call may set from another thread.
///
/// The invariant is that **setting it can never widen anything and never
/// starts anything** — every path that observes it either returns before a
/// child exists or kills one that does. It is deliberately not a channel,
/// not a signal handler and not an async runtime: a call is cancellable
/// between calls (the holder checks [`is_cancelled`](Self::is_cancelled)
/// itself) and during one ([`run_cancellable`] checks it), and nothing else
/// is needed for `runtime-contract.md` §5 to render the result, because a
/// cancelled call lands in the shape a refusal already has.
///
/// Cloning is cheap and every clone names the same flag, which is what lets
/// the holder keep one while the call borrows another.
#[derive(Debug, Clone, Default)]
pub struct CancellationToken(Arc<Cancellation>);

/// The flag and, when there was one, the reason it was raised.
#[derive(Debug, Default)]
struct Cancellation {
    cancelled: AtomicBool,
    timed_out: AtomicBool,
}

impl CancellationToken {
    /// A fresh, un-cancelled token.
    pub fn new() -> Self {
        Self::default()
    }

    /// Cancels every call holding this token or a clone of it. Idempotent,
    /// and there is no way back: a token is one call's decision, not a
    /// reusable switch.
    pub fn cancel(&self) {
        self.0.cancelled.store(true, Ordering::SeqCst);
    }

    /// [`cancel`](Self::cancel), attributed to a wall clock that ran out.
    ///
    /// **The canceller states the reason; an observer never re-derives it
    /// from a clock of its own.** A deadline is armed from one instant and
    /// the loop it stops starts its own a thread-spawn later, so a loop
    /// asking whether *its* clock had run out answered `cancelled` for every
    /// expiry that landed in the gap between the two — measured 2026-09-18 on
    /// the Linux sweep, where a subagent given 400 ms reported a bare
    /// cancellation after 101 turns.
    ///
    /// The reason is stored before the flag, so no observer can see the
    /// cancellation without it.
    pub fn cancel_for_deadline(&self) {
        self.0.timed_out.store(true, Ordering::SeqCst);
        self.cancel();
    }

    pub fn is_cancelled(&self) -> bool {
        self.0.cancelled.load(Ordering::SeqCst)
    }

    /// Whether [`cancel_for_deadline`](Self::cancel_for_deadline) raised it.
    pub fn timed_out(&self) -> bool {
        self.0.timed_out.load(Ordering::SeqCst)
    }
}

/// One argument as authored by the cell.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Argument {
    Text(String),
    Lines(Vec<String>),
}

/// The arguments of one call, by declared name.
///
/// Most values are one argv element or one command line. `Lines` is the one
/// structured form: it lets a cell carry literal multiline file content
/// without putting that content in a JavaScript template literal.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Args(BTreeMap<String, Argument>);

impl Args {
    pub fn new() -> Self {
        Self::default()
    }

    /// Builder form, so a call site reads like the contract's own
    /// `read({ path: … })`.
    pub fn with(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.0.insert(name.into(), Argument::Text(value.into()));
        self
    }

    /// Builder form for a literal line array. The tool declaration decides
    /// whether the named argument admits this structured value.
    pub fn with_lines<I, S>(mut self, name: impl Into<String>, lines: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.0.insert(
            name.into(),
            Argument::Lines(lines.into_iter().map(Into::into).collect()),
        );
        self
    }

    pub fn get(&self, name: &str) -> Option<&str> {
        match self.0.get(name) {
            Some(Argument::Text(value)) => Some(value),
            Some(Argument::Lines(_)) | None => None,
        }
    }

    /// Moves the value at `from` to `to`, keeping its form.
    ///
    /// The whole of a dialect adapter's work for most rows: a provider
    /// family that learned `file_path` is shown `file_path`, and the tool
    /// that has always taken `path` receives `path`
    /// (`tool-abi.md` §5). The `Lines` form survives the move, because
    /// `Write`'s literal content is exactly the argument a rename must not
    /// flatten. Renaming onto an occupied name is a no-op, so an explicit
    /// canonical argument always wins over an aliased one.
    #[must_use]
    pub fn rename(mut self, from: &str, to: &str) -> Self {
        if from == to || self.0.contains_key(to) {
            return self;
        }
        if let Some(value) = self.0.remove(from) {
            self.0.insert(to.to_string(), value);
        }
        self
    }

    /// The items of a `Lines` or `Texts` argument; `None` for a string or
    /// an absent one.
    pub fn items(&self, name: &str) -> Option<&[String]> {
        match self.0.get(name) {
            Some(Argument::Lines(items)) => Some(items),
            Some(Argument::Text(_)) | None => None,
        }
    }

    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.0.keys().map(String::as_str)
    }
}

/// Which binary the child was exec'd on, and whether that was decided here
/// or left to the platform's executable roots.
///
/// The 61D ruling in one type: `binary` is the resolved path sterna hands to
/// `execvp`, and `fell_back_to_roots` is `true` exactly when the name could
/// not be resolved and the applier's directory roots are what bound the exec
/// instead. The fallback is logged when it happens **and** recorded here,
/// because a log line is not observable to the caller that has to decide
/// what to say about it.
///
/// **`fell_back_to_roots` is never honoured on Windows, and the reason is
/// that there is nothing there for it to mean.** The fallback works on unix
/// because `execvp` searches `PATH` and the applier's executable roots bound
/// where that search may land. `sandbox::windows::spawn` hands
/// `CreateProcessW` an `lpApplicationName`, so no search happens at all; a
/// partial name there is completed from **Sterna's own current directory**,
/// which is the project root and is writable by invariant 3. A program that
/// wrote `<project>\grep` and then called a tool whose program was not
/// installed had sterna execute it — measured on the Windows ARM64 VM,
/// 2026-09-09. `spawn_confined` refuses the fallback on that platform and
/// `windows::spawn` refuses a non-absolute program independently, so neither
/// is the only thing standing between the model and its own binary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecGrant {
    pub binary: PathBuf,
    pub fell_back_to_roots: bool,
}

/// Environment variables a confined child never sees.
///
/// The invariant: **a credential is withheld by the shape of its name, and
/// the session says how many.** Two rules and no cleverness — the three
/// variables sterna itself reads, and any name that ends in a credential word.
/// A name-shaped rule is predictable in a way an entropy test on values is
/// not, and predictable is what a person needs when a build fails for it.
///
/// The cost is real and stated rather than hidden: a project whose build
/// genuinely wants `GITHUB_TOKEN` cannot get it, because `bg::run` refuses
/// `env` too. That is the safer default to be wrong in.
pub fn is_credential_variable(name: &str) -> bool {
    /// Names with no credential word in them that are still Sterna's own.
    const STERNA_OWN: [&str; 1] = ["ANTHROPIC_BASE_URL"];
    /// A whole underscore-separated segment that makes a name a credential.
    const CREDENTIAL_WORDS: [&str; 8] = [
        "KEY",
        "TOKEN",
        "SECRET",
        "PASSWORD",
        "PASSWD",
        "CREDENTIAL",
        "CREDENTIALS",
        "CREDS",
    ];
    let upper = name.to_ascii_uppercase();
    if STERNA_OWN.contains(&upper.as_str()) {
        return true;
    }
    // **Whole segments, not substrings.** `AWS_SECRET_ACCESS_KEY` has to
    // match on `SECRET` and `KEY` wherever they sit, so a suffix rule is too
    // narrow; a substring rule is too wide and takes `TOKENIZER` with it.
    upper
        .split('_')
        .any(|segment| CREDENTIAL_WORDS.contains(&segment))
}

/// Which mechanism confined the child, or the explicit session-start bypass
/// selected for a disposable outer container. A platform with nothing to
/// install still returns [`PermissionDenied`] unless that dangerous flag was
/// supplied by the host before the session started.
/// The in-process state is **not** a hole in the invariant above: a tool
/// declared [`Argv::InProcess`] never becomes a child, so there is no process
/// to confine. Its one argument that touches the filesystem went through
/// `Profile::check` before anything happened, which is the same gate the
/// spawning tools' paths pass and the only one that ever enforced a path
/// rule — the OS layer is directory-granular and never saw them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Confinement {
    /// Host HTTP broker with domain/DNS policy; no process was spawned.
    BrokeredNetwork,
    /// macOS seatbelt, entered between `fork` and `exec`.
    Seatbelt,
    /// Linux Landlock plus socket-denial seccomp, installed before `exec`.
    Landlock,
    /// A Windows AppContainer, entered at `CreateProcessW` itself. There is
    /// no earlier moment to enter it: the container is an argument to the
    /// call that creates the process.
    AppContainer,
    /// Sterna installed no OS sandbox because the person explicitly selected
    /// `--dangerously-bypass-os-sandbox` together with `--yolo`.
    DangerouslyUnconfined,
    /// No child was created. The call ran inside sterna, and its path was
    /// checked by `Profile::check` before it did.
    InProcess,
}

impl Confinement {
    pub fn as_str(self) -> &'static str {
        match self {
            Confinement::BrokeredNetwork => "host web broker (domain/DNS policy)",
            Confinement::Seatbelt => "seatbelt",
            Confinement::Landlock => "landlock+seccomp",
            Confinement::AppContainer => "appcontainer",
            Confinement::DangerouslyUnconfined => {
                "none (explicit OS-sandbox bypass; outer isolation required)"
            }
            Confinement::InProcess => "in-process (no child; the path was checked)",
        }
    }

    /// Where the call ran, as a person reads it after `/tool`: the sandbox's
    /// backend is the doctor's to name, not the result line's.
    pub fn plainly(self) -> &'static str {
        match self {
            Confinement::BrokeredNetwork => "went through the web broker",
            Confinement::Seatbelt | Confinement::Landlock | Confinement::AppContainer => {
                "ran in the sandbox"
            }
            Confinement::DangerouslyUnconfined => "ran without the OS sandbox",
            Confinement::InProcess => "ran inside Sterna",
        }
    }

    /// One word for a status line, where the sentence [`Confinement::as_str`]
    /// returns has no room.
    pub fn short(self) -> &'static str {
        match self {
            Confinement::BrokeredNetwork => "brokered",
            // Which applier is platform-determined and never a surprise;
            // whether there is one is the whole question, so the status
            // line answers that and the doctor names the backend.
            Confinement::Seatbelt | Confinement::Landlock | Confinement::AppContainer => "confined",
            Confinement::DangerouslyUnconfined => "unconfined",
            Confinement::InProcess => "in-process",
        }
    }

    /// Which confinement a child spawned by this session would enter, asked
    /// before any child exists. `None` is a platform with no applier, where
    /// [`confined_spawn`] refuses rather than spawning.
    ///
    /// The invariant: **this is
    /// [`spawn_with_confinement_policy`]'s own decision, asked early.** It
    /// reads the same `profile.os_sandbox_bypassed()` and falls through to
    /// the same per-platform arm, so a session cannot announce one
    /// confinement and spawn into another. It exists because a person who
    /// sets a rung and a grant has made two of three choices, and until
    /// 2026-09-19 nothing told them what the third one was -- the model
    /// found out instead, mid-task, by failing to link.
    #[must_use]
    pub fn for_session(profile: &Profile) -> Option<Self> {
        if profile.os_sandbox_bypassed() {
            return Some(Confinement::DangerouslyUnconfined);
        }
        #[cfg(target_os = "macos")]
        {
            Some(Confinement::Seatbelt)
        }
        #[cfg(target_os = "linux")]
        {
            Some(Confinement::Landlock)
        }
        #[cfg(target_os = "windows")]
        {
            Some(Confinement::AppContainer)
        }
        #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
        {
            None
        }
    }
}

/// What one call returned.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolResult {
    /// Modification time observed on the admitted read path, after reading.
    pub modified: Option<String>,
    pub tool: String,
    pub stdout: String,
    pub stderr: String,
    /// `None` for a child killed by a signal, which is not an exit status.
    pub exit_code: Option<i32>,
    pub grant: ExecGrant,
    pub confinement: Confinement,
}

impl ToolResult {
    /// The observed output, capped, as the hooks carry it.
    pub fn preview(&self) -> String {
        truncate(&self.stdout, PREVIEW_BYTES)
    }
}

/// Everything that can come back other than a result.
///
/// Three variants, and they are different in kind: [`ToolError::Denied`] is
/// the profile's own answer and is a value the caller is expected to handle
/// (§1.4); [`ToolError::Spawn`] is the operating system failing to start a
/// program sterna had already decided to allow; [`ToolError::Cancelled`] is the
/// caller having withdrawn the call. Collapsing any two would report one as
/// the other — a missing binary as a permission decision, or a withdrawal as
/// a refusal a person could fix by editing `settings.json`.
///
/// All three reach the model the same way, and that is the point of not
/// inventing a fourth shape: `runtime-contract.md` §5 already says a throw is
/// a result, so a cancelled call is an `Error` preview in the turn slot a
/// yield would have used and the turn is not retried.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolError {
    Denied(PermissionDenied),
    Spawn {
        tool: String,
        program: PathBuf,
        error: String,
    },
    Cancelled {
        tool: String,
    },
}

impl ToolError {
    /// The refusal, when this was one.
    ///
    /// A cancellation is **not** one: nothing about the profile decided it,
    /// so a caller reporting `denied()` would name a settings file that has
    /// nothing to do with what happened.
    pub fn denied(&self) -> Option<&PermissionDenied> {
        match self {
            ToolError::Denied(denied) => Some(denied),
            ToolError::Spawn { .. } | ToolError::Cancelled { .. } => None,
        }
    }
}

impl From<PermissionDenied> for ToolError {
    fn from(denied: PermissionDenied) -> Self {
        ToolError::Denied(denied)
    }
}

impl fmt::Display for ToolError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ToolError::Denied(denied) => write!(f, "{denied}"),
            ToolError::Spawn {
                tool,
                program,
                error,
            } => write!(
                f,
                "{tool}: could not start {}: {error}",
                program.to_string_lossy()
            ),
            ToolError::Cancelled { tool } => {
                write!(f, "Cancelled: {tool}() was cancelled before it completed")
            }
        }
    }
}

impl std::error::Error for ToolError {}

/// What one call needs that is not the call: the session's single profile,
/// the Glasshouse seam the hooks go through, and the session id they carry.
///
/// It borrows rather than owns, which is what keeps `sandbox-grants.md`
/// §1.5 true through this layer: there is no constructor here that compiles
/// a `Profile`, so the one the session built at start-up is the only one a
/// call can be made against.
pub struct ToolContext<'a> {
    pub profile: &'a Profile,
    pub session: &'a SessionId,
}

/// Runs one tool call and returns its result as a value.
///
/// The order is `sandbox-grants.md` §2's, and it is the order because the
/// two questions are different: the arguments are checked (a path through
/// `Profile::check`, a command line through `Profile::admits_command`), the
/// executable is resolved, the child is confined, and only then is it
/// spawned.
///
/// `PreToolUse` fires once for every call that named a registered tool, and
/// `PostToolUse` fires once for the same call **whatever it returned** — a
/// refusal is an observed output, and a firewall that only saw successes
/// would report a program that probed a hundred paths as having done
/// nothing. An unregistered name is not a call: it fires neither, because
/// there is no tool for the event to name.
pub fn run(ctx: &ToolContext<'_>, name: &str, args: &Args) -> Result<ToolResult, ToolError> {
    run_cancellable(ctx, &CancellationToken::default(), name, args)
}

/// The same call, cancellable by whoever holds `token`.
///
/// The invariant this adds is that **a cancelled call reaches exactly the
/// same two hook deliveries a completed one does**: `PreToolUse` fires before
/// the pre-spawn check, so a call cancelled before it started anything still
/// announces itself and still reports what it returned. A firewall that saw
/// only the calls that ran would report an abandoned branch as never having
/// been attempted.
///
/// [`run`] is this function with a token nobody can set, which is why its
/// signature and its behaviour are unchanged.
pub fn run_cancellable(
    ctx: &ToolContext<'_>,
    token: &CancellationToken,
    name: &str,
    args: &Args,
) -> Result<ToolResult, ToolError> {
    run_traced(ctx, token, name, args).outcome
}

/// The arguments of one call as [`check_arguments`] admitted them — the
/// spelling that reached the child, which is what `runtime-contract.md`
/// §9.4's trajectory records. A path is `Profile::check`'s resolved path; a
/// pattern and a command line are the text the profile admitted.
pub type CheckedArgs = BTreeMap<String, String>;

/// One call's result and, beside it, its arguments as checked. A refused
/// call carries only what was admitted before the refusing argument, so a
/// spelling the profile never admitted is never written down as one it did.
pub struct Traced {
    pub outcome: Result<ToolResult, ToolError>,
    pub checked: CheckedArgs,
}

/// [`run_cancellable`], answering with the checked arguments too. The
/// trajectory is the only reader; nothing about the call itself differs.
pub fn run_traced(
    ctx: &ToolContext<'_>,
    token: &CancellationToken,
    name: &str,
    args: &Args,
) -> Traced {
    run_traced_with_gate(ctx, token, name, args, None, &|| token.is_cancelled())
}

/// The exact-call suspension seam. The gate runs after complete argument
/// admission and before an effect; returning resumes this stack frame only.
/// How the caller supervises one tool call: the seam that may ask a person,
/// the flag that says stop, and the clock that stops while this call's child
/// runs.
///
/// One value rather than three parameters because they are one idea — what
/// the caller does *around* the call, as opposed to what the call is.
#[derive(Clone, Copy)]
struct Watching<'a> {
    gate: Option<&'a crate::approval::Gate>,
    stopped: &'a dyn Fn() -> bool,
    /// Stopped while the child runs, never around the whole call: the hooks
    /// this path delivers have no bound of their own and must stay on the
    /// caller's clock.
    waiting: Option<&'a std::sync::Arc<crate::approval::WaitClock>>,
}

pub(crate) fn run_traced_with_gate(
    ctx: &ToolContext<'_>,
    token: &CancellationToken,
    name: &str,
    args: &Args,
    gate: Option<&crate::approval::Gate>,
    stopped: &dyn Fn() -> bool,
) -> Traced {
    run_traced_pausing(ctx, token, name, args, gate, stopped, None)
}

/// [`run_traced_with_gate`] with the clock a caller wants stopped while this
/// call's child runs.
///
/// **Only the child's own wait is paused, never the whole call.** The hooks
/// this path delivers (`glasshouse::run`) are children too and have no
/// timeout of their own, so they stay on the caller's clock — which is the
/// only thing that ends a hook that hangs.
pub(crate) fn run_traced_pausing(
    ctx: &ToolContext<'_>,
    token: &CancellationToken,
    name: &str,
    args: &Args,
    gate: Option<&crate::approval::Gate>,
    stopped: &dyn Fn() -> bool,
    waiting: Option<&std::sync::Arc<crate::approval::WaitClock>>,
) -> Traced {
    let mut checked = CheckedArgs::new();
    let Some(tool) = registry::lookup(name) else {
        return Traced {
            outcome: Err(ToolError::Denied(PermissionDenied {
                tool: name.to_string(),
                path: String::new(),
                rule: format!(
                    "no tool named `{name}` is registered; the registry declares {}",
                    registry::names().join(", ")
                ),
            })),
            checked,
        };
    };

    let outcome = checked_call(
        ctx,
        token,
        tool,
        args,
        &mut checked,
        Watching {
            gate,
            stopped,
            waiting,
        },
    );
    Traced { outcome, checked }
}

/// One dynamic MCP call.
pub(crate) fn run_mcp(
    ctx: &ToolContext<'_>,
    token: &CancellationToken,
    client: &mut crate::tools::mcp::Mcp,
    name: &str,
    arguments: Value,
) -> Result<ToolResult, ToolError> {
    client.call(ctx.profile, token, name, arguments)
}

/// Discovery: the descriptors of every granted MCP tool.
pub(crate) fn list_mcp(
    ctx: &ToolContext<'_>,
    token: &CancellationToken,
    client: &mut crate::tools::mcp::Mcp,
) -> Result<Vec<crate::tools::mcp::Descriptor>, ToolError> {
    client.list(ctx.profile, token)
}

/// One checked argument, carrying which of §2's two questions answered it.
#[derive(Debug, Clone)]
enum Checked {
    /// The path `Profile::check` **resolved**, which is the only spelling
    /// that may reach the child.
    Path(PathBuf),
    Pattern(String),
    /// Literal lines normalized to text. A non-empty array is newline
    /// terminated; callers that need byte-exact control keep using a string.
    Lines(String),
    /// Opaque strings kept as separate items — a multi-hunk edit's hunks.
    Texts(Vec<String>),
    CommandLine(String),
}

impl Checked {
    /// The spelling the trajectory records: the resolved path, or the text
    /// the profile admitted. Items are recorded as one JSON array so their
    /// boundaries survive.
    fn spelling(&self) -> String {
        match self {
            Checked::Path(path) => path.to_string_lossy().into_owned(),
            Checked::Pattern(text) | Checked::Lines(text) | Checked::CommandLine(text) => {
                text.clone()
            }
            Checked::Texts(items) => {
                Value::Array(items.iter().cloned().map(Value::String).collect()).to_string()
            }
        }
    }
}

fn checked_call(
    ctx: &ToolContext<'_>,
    token: &CancellationToken,
    tool: &Tool,
    args: &Args,
    trace: &mut CheckedArgs,
    watching: Watching<'_>,
) -> Result<ToolResult, ToolError> {
    let Watching {
        gate,
        stopped,
        waiting,
    } = watching;
    let stop = || token.is_cancelled() || stopped();
    if stop() {
        return Err(ToolError::Cancelled {
            tool: tool.name().into(),
        });
    }
    let checked = check_arguments(ctx.profile, tool, args, trace)?;
    // A command that asks to run outside the sandbox needs somebody to say
    // yes. Where nobody can be asked -- a subagent, a background job -- it is
    // refused here, and on Full access there is no sandbox to leave.
    let outside = checked
        .iter()
        .any(|(name, _)| *name == crate::permissions::OUTSIDE);
    if outside && gate.is_none() && !ctx.profile.os_sandbox_bypassed() {
        return Err(PermissionDenied {
            tool: tool.name().into(),
            path: String::new(),
            rule: "nobody can be asked here, so this command cannot leave the sandbox; do the work inside it".into(),
        }
        .into());
    }
    let mut run_outside = false;
    if let Some(gate) = gate {
        if ctx.profile.root().to_str().is_none()
            || checked
                .iter()
                .any(|(_, value)| matches!(value, Checked::Path(path) if path.to_str().is_none()))
        {
            return Err(PermissionDenied {
                tool: tool.name().into(),
                path: String::new(),
                rule: "the exact action contains a path that cannot be represented as UTF-8".into(),
            }
            .into());
        }
        let action = crate::approval::Action::new(tool.name(), ctx.profile.root(), trace.clone());
        let rule = match gate.admit(action.clone(), stopped) {
            crate::approval::Admission::Allowed => {
                run_outside = outside && !ctx.profile.os_sandbox_bypassed();
                None
            }
            // Cancelled the way a running call is: reported as cancelled,
            // never as a refusal the model should work around.
            crate::approval::Admission::Cancelled => {
                return Err(ToolError::Cancelled {
                    tool: tool.name().into(),
                });
            }
            crate::approval::Admission::NobodyToAsk => Some(
                "nobody is at the terminal to answer, so this call is refused; do the work inside the sandbox or say plainly what needs a person"
                    .to_string(),
            ),
            crate::approval::Admission::DeniedEarlier => Some(
                "you denied this exact call earlier in this session; it stays denied until you forget it on the Ask sheet"
                    .to_string(),
            ),
            // A person who refused with words gets them delivered where the
            // model reads every refusal: as the rule. The refusal itself is
            // the same refusal either way.
            crate::approval::Admission::Denied => Some(match gate.redirect_for(&action) {
                Some(text) if text.trim().is_empty() => {
                    "the person declined this exact call and asks you to propose another way to do it: say what you would do instead, then do that".to_string()
                }
                Some(text) => format!(
                    "the person declined this exact call and asks for another way: \"{}\" -- do that instead",
                    text.trim()
                ),
                None if !action.confirmation().complete => {
                    "too large to confirm in one prompt; split it into smaller calls".to_string()
                }
                None => "the person denied this call".to_string(),
            }),
        };
        if let Some(rule) = rule {
            return Err(PermissionDenied {
                tool: tool.name().into(),
                path: action.label(),
                rule,
            }
            .into());
        }
        // The filesystem can change while the callback is suspended. Resolve
        // the original arguments again; a retargeted symlink gets no authority
        // from the old answer even when both destinations are in the root.
        let mut current = CheckedArgs::new();
        check_arguments(ctx.profile, tool, args, &mut current)?;
        if *trace != current || stopped() {
            return Err(PermissionDenied {
                tool: tool.name().into(),
                path: String::new(),
                rule: "the exact action changed or was cancelled while awaiting its answer".into(),
            }
            .into());
        }
    }
    let broad_search = resolved_path(&checked, "path")
        .is_some_and(|path| is_broad_search(ctx.profile.root(), path));
    let requested = tool.name();
    let skipped = if requested == "grep" && broad_search {
        resolved_path(&checked, "path")
            .map(ignored_directories)
            .unwrap_or_default()
    } else {
        Vec::new()
    };
    // Branching here and not inside `spawn_confined` is what makes
    // `Argv::InProcess`'s claim structural: an in-process tool never reaches
    // a `Command`, an `exec_grant` or a sandbox applier, because the only
    // call site of all three is the other arm.
    // **`grep` is ripgrep when ripgrep is installed.** The two tools answer
    // the same question from the same two arguments, and one of them answers
    // it seven thousand times faster: measured on 2026-09-20 against a 9.2 GB
    // checkout, `grep -r` took 143 s and `rg` took 20 ms. Worse than the time,
    // `grep` is BRE and `rg` is not, so the same pattern meant two different
    // things depending on which name the model happened to write -- and the
    // model writes `grep`, because that is the name it knows.
    //
    // The visible difference is that ignored files stop matching, which is
    // what `rg`'s own summary has promised all along and what a person
    // searching a checkout means. Aiming `path` at a directory searches it
    // whatever an ignore file says, exactly as `.git` and `.sterna` already
    // work. Where ripgrep is absent, `grep` runs as before, with `-E` and
    // the search root's own directory rules.
    let tool = if requested == "grep" && ripgrep_is_installed() {
        registry::GREP_BY_RIPGREP
    } else {
        tool
    };
    let mut result = if tool.argv() == Argv::InProcess {
        perform_in_process(ctx.profile, &stop, tool, &checked, &skipped)?
    } else {
        let mut argv = build_argv(tool, &checked)?;
        // Keyed on the argv shape, not on the name: once ripgrep is serving
        // the call the tool is still called `grep` and these are not its
        // flags -- passing them made every search exit on an unknown option
        // and answer with nothing, which the search tests caught at once.
        if tool.argv() == Argv::GrepIn && broad_search {
            // Git internals are an entire generated tree and grep can avoid
            // traversing them. The rollout is one exact path rather than a
            // basename-wide exclusion, so it is removed from output below.
            // The in-process `grep` prunes the same directories itself.
            argv.insert(1, "--exclude-dir=.git".into());
            for name in &skipped {
                argv.insert(1, format!("--exclude-dir={name}").into());
            }
        }
        if run_outside {
            // The one command the person let out, and nothing after it: a
            // copy of the profile without Sterna's own confinement.
            let unconfined = ctx.profile.clone().with_os_sandbox_bypass();
            spawn_confined(&unconfined, &stop, tool, &argv, waiting)?
        } else {
            spawn_confined(ctx.profile, &stop, tool, &argv, waiting)?
        }
    };
    if requested == "grep" && broad_search {
        result.stdout = filter_grep_artifacts(ctx.profile.root(), &result.stdout);
    }
    if tool.name() == "read" && result.exit_code == Some(0) {
        result.modified = resolved_path(&checked, "path")
            .and_then(|path| std::fs::metadata(path).ok())
            .and_then(|metadata| metadata.modified().ok())
            .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
            .and_then(|time| i64::try_from(time.as_millis()).ok())
            .map(|millis| crate::events::Stamp::from_millis(millis).to_string());
    }
    Ok(result)
}

/// Performs a tool Sterna does itself. The match is exhaustive on name so a new
/// in-process declaration cannot silently acquire another tool's behavior.
///
/// `skipped` is what the spawned `grep` would have received as
/// `--exclude-dir=`: the same names, so the in-process walk skips the same
/// generated trees. It is empty for every call that is not a broad `grep`,
/// and only the Windows-only `grep` arm reads it.
#[cfg_attr(not(windows), allow(unused_variables))]
fn perform_in_process(
    profile: &Profile,
    stopped: &dyn Fn() -> bool,
    tool: &Tool,
    checked: &[(&'static str, Checked)],
    skipped: &[String],
) -> Result<ToolResult, ToolError> {
    if stopped() {
        return Err(ToolError::Cancelled {
            tool: tool.name().into(),
        });
    }
    let refuse = |rule: String| {
        ToolError::Denied(PermissionDenied {
            tool: tool.name().to_string(),
            path: String::new(),
            rule,
        })
    };
    match tool.name() {
        // The two the registry declares in-process on Windows only, for
        // `registry::READ`'s reason: the cage cannot start the MSYS2 images
        // that would otherwise perform them.
        #[cfg(windows)]
        "read" => {
            let Some(path) = resolved_path(checked, "path") else {
                return Err(refuse("read needs a checked path".to_string()));
            };
            Ok(search::read_file(tool.name(), path))
        }
        #[cfg(windows)]
        "grep" => {
            let (Some(root), Some(pattern)) =
                (resolved_path(checked, "path"), text(checked, "pattern"))
            else {
                return Err(refuse("grep needs a checked path and pattern".to_string()));
            };
            search::grep_tree(profile, stopped, tool.name(), root, pattern, skipped)
        }
        "glob" => {
            let (Some(root), Some(pattern)) =
                (resolved_path(checked, "path"), text(checked, "pattern"))
            else {
                return Err(refuse("glob needs a checked path and pattern".to_string()));
            };
            let stdout = glob_paths(profile, stopped, tool.name(), root, pattern)?;
            Ok(ToolResult {
                modified: None,
                tool: tool.name().to_string(),
                stdout,
                stderr: String::new(),
                exit_code: Some(0),
                grant: ExecGrant {
                    binary: PathBuf::new(),
                    fell_back_to_roots: false,
                },
                confinement: Confinement::InProcess,
            })
        }
        "write" => {
            let Some(path) = resolved_path(checked, "path") else {
                return Err(refuse("write needs a checked path".to_string()));
            };
            let content = exclusive_text(checked, "content", "lines").map_err(|message| {
                refuse(format!(
                    "write needs exactly one of content or lines; {message}"
                ))
            })?;
            // The parent is created, because a model that has to `mkdir -p`
            // through `bash` before every `write` gains nothing from having
            // `write`. It is inside the checked path by construction, so it
            // reaches nowhere the write itself could not.
            if let Some(parent) = path.parent()
                && let Err(error) = std::fs::create_dir_all(parent)
            {
                return Err(ToolError::Spawn {
                    tool: tool.name().to_string(),
                    program: PathBuf::from("(in-process)"),
                    error: error.to_string(),
                });
            }
            if stopped() {
                return Err(ToolError::Cancelled {
                    tool: tool.name().into(),
                });
            }
            match std::fs::write(path, content) {
                Ok(()) => Ok(ToolResult {
                    modified: None,
                    tool: tool.name().to_string(),
                    stdout: format!("wrote {} bytes to {}", content.len(), path.display()),
                    stderr: String::new(),
                    exit_code: Some(0),
                    grant: ExecGrant {
                        binary: PathBuf::new(),
                        fell_back_to_roots: false,
                    },
                    confinement: Confinement::InProcess,
                }),
                Err(error) => Err(ToolError::Spawn {
                    tool: tool.name().to_string(),
                    program: PathBuf::from("(in-process)"),
                    error: error.to_string(),
                }),
            }
        }
        "edit" => {
            let (Some(path), Some(expected_sha256)) = (
                resolved_path(checked, "path"),
                text(checked, "expected_sha256"),
            ) else {
                return Err(refuse(
                    "edit needs a checked path and a visible or explicit expected_sha256"
                        .to_string(),
                ));
            };
            let failed = |error: crate::tools::exact_edit::EditError| ToolError::Spawn {
                tool: tool.name().to_string(),
                program: PathBuf::from("(in-process)"),
                error: error.to_string(),
            };
            let olds = texts(checked, "olds");
            let replacements = texts(checked, "replacements");
            // One form per call: the multi-hunk arrays and the single-hunk
            // strings say different things about what the model meant, and
            // silently preferring one would apply an edit it did not ask for.
            let result = if olds.is_some() || replacements.is_some() {
                let single = ["old", "oldLines", "replacement", "replacementLines"]
                    .iter()
                    .any(|name| text(checked, name).is_some());
                if single {
                    return Err(refuse(
                        "edit takes either old/replacement or olds/replacements, not both"
                            .to_string(),
                    ));
                }
                let (Some(olds), Some(replacements)) = (olds, replacements) else {
                    return Err(refuse(
                        "edit needs both olds and replacements for a multi-hunk edit".to_string(),
                    ));
                };
                crate::tools::exact_edit::apply_hunks(
                    profile,
                    path,
                    expected_sha256,
                    olds,
                    replacements,
                )
                .map_err(failed)?
            } else {
                let expected = exclusive_text(checked, "old", "oldLines").map_err(|message| {
                    refuse(format!(
                        "edit needs exactly one of old or oldLines; {message}"
                    ))
                })?;
                let replacement = exclusive_text(checked, "replacement", "replacementLines")
                    .map_err(|message| {
                        refuse(format!(
                            "edit needs exactly one of replacement or replacementLines; {message}"
                        ))
                    })?;
                crate::tools::exact_edit::apply(
                    profile,
                    path,
                    expected_sha256,
                    expected,
                    replacement,
                )
                .map_err(failed)?
            };
            Ok(ToolResult {
                modified: None,
                tool: tool.name().to_string(),
                stdout: serde_json::to_string(&result).expect("edit result serializes"),
                stderr: String::new(),
                exit_code: Some(0),
                grant: ExecGrant {
                    binary: PathBuf::new(),
                    fell_back_to_roots: false,
                },
                confinement: Confinement::InProcess,
            })
        }
        "context" => {
            let Some(path) = resolved_path(checked, "path") else {
                return Err(refuse("context needs a checked path".to_string()));
            };
            let result = crate::project::source_context::pack(
                profile,
                path,
                text(checked, "symbol").filter(|symbol| !symbol.is_empty()),
            )
            .map_err(|error| ToolError::Spawn {
                tool: tool.name().to_string(),
                program: PathBuf::from("(in-process)"),
                error: error.to_string(),
            })?;
            let ranges: Vec<Value> = std::iter::once(&result.target)
                .chain(result.supporting.iter())
                .map(|excerpt| {
                    json!({
                        "path": excerpt.path,
                        "start": excerpt.range.start,
                        "end": excerpt.range.end,
                        "role": excerpt.role,
                    })
                })
                .collect();
            let mut payload = serde_json::to_value(&result).expect("source context serializes");
            if let Some(fields) = payload.as_object_mut() {
                fields.insert("text".into(), Value::String(result.render()));
                fields.insert("ranges".into(), Value::Array(ranges));
            }
            Ok(ToolResult {
                modified: None,
                tool: tool.name().to_string(),
                stdout: serde_json::to_string(&payload).expect("source context serializes"),
                stderr: String::new(),
                exit_code: Some(0),
                grant: ExecGrant {
                    binary: PathBuf::new(),
                    fell_back_to_roots: false,
                },
                confinement: Confinement::InProcess,
            })
        }
        other => Err(refuse(format!(
            "`{other}` is declared in-process and nothing here performs it"
        ))),
    }
}

/// Walks a checked root and matches slash-separated patterns against paths
/// relative to it. A denied directory is pruned before `read_dir`, which
/// keeps names beneath it from becoming an enumeration side channel.
fn glob_paths(
    profile: &Profile,
    stopped: &dyn Fn() -> bool,
    tool: &str,
    root: &Path,
    pattern: &str,
) -> Result<String, ToolError> {
    const MAX_VISITED: usize = 100_000;

    let normalized_pattern = pattern.replace('\\', "/");
    let pattern: Vec<&str> = normalized_pattern
        .split('/')
        .filter(|part| !part.is_empty() && *part != ".")
        .collect();
    if pattern.is_empty() {
        return Ok(String::new());
    }
    let include_sterna_artifacts =
        explicitly_roots_component(profile.root(), root, ".sterna") || pattern.contains(&".sterna");
    let include_git =
        explicitly_roots_component(profile.root(), root, ".git") || pattern.contains(&".git");

    let mut pending = vec![root.to_path_buf()];
    let mut matches = Vec::new();
    let mut visited = 0usize;
    while let Some(directory) = pending.pop() {
        if stopped() {
            return Err(ToolError::Cancelled {
                tool: tool.to_string(),
            });
        }
        let entries = std::fs::read_dir(&directory).map_err(|error| ToolError::Spawn {
            tool: tool.to_string(),
            program: PathBuf::from("(in-process)"),
            error: error.to_string(),
        })?;
        for entry in entries {
            if stopped() {
                return Err(ToolError::Cancelled {
                    tool: tool.to_string(),
                });
            }
            visited += 1;
            if visited > MAX_VISITED {
                return Err(ToolError::Spawn {
                    tool: tool.to_string(),
                    program: PathBuf::from("(in-process)"),
                    error: format!("glob stopped after {MAX_VISITED} directory entries"),
                });
            }
            let entry = entry.map_err(|error| ToolError::Spawn {
                tool: tool.to_string(),
                program: PathBuf::from("(in-process)"),
                error: error.to_string(),
            })?;
            let path = entry.path();
            let relative = path.strip_prefix(root).unwrap_or(&path);
            if (!include_git && contains_component(relative, ".git"))
                || (!include_sterna_artifacts && is_sterna_artifact(relative))
            {
                continue;
            }
            let Ok(checked) = profile.check(tool, Access::Read, &path) else {
                continue;
            };
            let relative = checked.strip_prefix(root).unwrap_or(&checked);
            let components: Vec<String> = relative
                .components()
                .map(|part| part.as_os_str().to_string_lossy().into_owned())
                .collect();
            if glob_components(&pattern, &components) {
                matches.push(checked.clone());
            }
            if entry.file_type().is_ok_and(|kind| kind.is_dir()) {
                pending.push(checked);
            }
        }
    }
    matches.sort();
    Ok(matches
        .into_iter()
        .map(|path| format!("{}\n", path.display()))
        .collect())
}

fn glob_components(pattern: &[&str], path: &[String]) -> bool {
    let mut matched = vec![vec![false; path.len() + 1]; pattern.len() + 1];
    matched[0][0] = true;
    for (index, part) in pattern.iter().enumerate() {
        if *part == "**" {
            for depth in 0..=path.len() {
                matched[index + 1][depth] |= matched[index][depth];
                if depth < path.len() && matched[index + 1][depth] {
                    matched[index + 1][depth + 1] = true;
                }
            }
        } else {
            for depth in 0..path.len() {
                if matched[index][depth] && glob_segment(part, &path[depth]) {
                    matched[index + 1][depth + 1] = true;
                }
            }
        }
    }
    matched[pattern.len()][path.len()]
}

fn glob_segment(pattern: &str, text: &str) -> bool {
    let pattern: Vec<char> = pattern.chars().collect();
    let text: Vec<char> = text.chars().collect();
    let mut matched = vec![vec![false; text.len() + 1]; pattern.len() + 1];
    matched[0][0] = true;
    for (index, byte) in pattern.iter().enumerate() {
        for offset in 0..=text.len() {
            if *byte == '*' {
                matched[index + 1][offset] |= matched[index][offset];
                if offset < text.len() && matched[index + 1][offset] {
                    matched[index + 1][offset + 1] = true;
                }
            } else if offset < text.len()
                && matched[index][offset]
                && (*byte == '?' || *byte == text[offset])
            {
                matched[index + 1][offset + 1] = true;
            }
        }
    }
    matched[pattern.len()][text.len()]
}

/// Admits one argument: onto the argv list, and into the trajectory's
/// record of what was checked.
fn admit(
    checked: &mut Vec<(&'static str, Checked)>,
    trace: &mut CheckedArgs,
    name: &'static str,
    value: Checked,
) {
    trace.insert(name.to_string(), value.spelling());
    checked.push((name, value));
}

/// Checks every declared argument, and refuses every undeclared one.
///
/// An undeclared name is refused rather than ignored: a call carrying
/// `path` to a tool that has no `path` would otherwise run against the
/// project root and look like it had honoured the argument.
fn check_arguments(
    profile: &Profile,
    tool: &Tool,
    args: &Args,
    trace: &mut CheckedArgs,
) -> Result<Vec<(&'static str, Checked)>, PermissionDenied> {
    for given in args.names() {
        if !tool.args().iter().any(|arg| arg.name() == given) {
            return Err(PermissionDenied {
                tool: tool.name().to_string(),
                path: given.to_string(),
                rule: format!("`{}` declares no argument named `{given}`", tool.name()),
            });
        }
    }

    let mut checked = Vec::new();
    for arg in tool.args() {
        let given = args.0.get(arg.name());
        match (arg.kind(), given) {
            (ArgKind::Path, Some(Argument::Text(value))) => {
                let resolved = profile.check(tool.name(), Access::Read, Path::new(value))?;
                admit(&mut checked, trace, arg.name(), Checked::Path(resolved));
            }
            // The same check, asked with the other access. A path the profile
            // grants for reading and not for writing is refused here, which
            // is the whole difference between the two kinds.
            (ArgKind::WritePath, Some(Argument::Text(value))) => {
                // The request mode narrows here and never in `check`, which
                // the OS appliers probe (`Profile::check_request`).
                let resolved =
                    profile.check_request(tool.name(), Access::Write, Path::new(value))?;
                admit(&mut checked, trace, arg.name(), Checked::Path(resolved));
            }
            // The project root stands in for a missing path, and it is
            // checked rather than trusted: `Profile::check` is what says the
            // root is reachable, and a `deny` rule naming it would refuse
            // here exactly as it would for any other path.
            (ArgKind::Path, None) if arg.uses_root_default() => {
                let root = profile.root().to_path_buf();
                let resolved = profile.check(tool.name(), Access::Read, &root)?;
                admit(&mut checked, trace, arg.name(), Checked::Path(resolved));
            }
            (_, None) if !arg.is_required() => {}
            (ArgKind::Pattern, Some(Argument::Text(value))) => {
                admit(
                    &mut checked,
                    trace,
                    arg.name(),
                    Checked::Pattern(value.to_string()),
                );
            }
            (ArgKind::Lines, Some(Argument::Lines(lines))) => {
                let mut normalized = Vec::with_capacity(lines.len());
                for (index, line) in lines.iter().enumerate() {
                    let line = line
                        .strip_suffix("\r\n")
                        .or_else(|| line.strip_suffix('\n'))
                        .unwrap_or(line);
                    if line.contains(['\r', '\n']) {
                        return Err(PermissionDenied {
                            tool: tool.name().to_string(),
                            path: arg.name().to_string(),
                            rule: format!(
                                "`{}` argument `{}` item {} contains an embedded newline; each item must be one logical line",
                                tool.name(),
                                arg.name(),
                                index
                            ),
                        });
                    }
                    normalized.push(line);
                }
                let mut value = normalized.join("\n");
                if !lines.is_empty() {
                    value.push('\n');
                }
                admit(&mut checked, trace, arg.name(), Checked::Lines(value));
            }
            (ArgKind::Texts, Some(Argument::Lines(items))) => {
                admit(
                    &mut checked,
                    trace,
                    arg.name(),
                    Checked::Texts(items.clone()),
                );
            }
            (ArgKind::Reason, Some(Argument::Text(value))) => {
                admit(
                    &mut checked,
                    trace,
                    arg.name(),
                    Checked::Pattern(value.to_string()),
                );
            }
            (ArgKind::CommandLine, Some(Argument::Text(value))) => {
                profile.admits_command(value)?;
                admit(
                    &mut checked,
                    trace,
                    arg.name(),
                    Checked::CommandLine(value.to_string()),
                );
            }
            (_, Some(_)) => {
                return Err(PermissionDenied {
                    tool: tool.name().to_string(),
                    path: arg.name().to_string(),
                    rule: format!(
                        "`{}` argument `{}` has the wrong type",
                        tool.name(),
                        arg.name()
                    ),
                });
            }
            (_, None) => {
                return Err(PermissionDenied {
                    tool: tool.name().to_string(),
                    path: arg.name().to_string(),
                    rule: format!(
                        "`{}` requires an argument named `{}`",
                        tool.name(),
                        arg.name()
                    ),
                });
            }
        }
    }
    Ok(checked)
}

fn resolved_path<'a>(checked: &'a [(&'static str, Checked)], name: &str) -> Option<&'a Path> {
    checked.iter().find_map(|(declared, value)| match value {
        Checked::Path(path) if *declared == name => Some(path.as_path()),
        _ => None,
    })
}

fn text<'a>(checked: &'a [(&'static str, Checked)], name: &str) -> Option<&'a str> {
    checked.iter().find_map(|(declared, value)| match value {
        Checked::Pattern(text) | Checked::Lines(text) | Checked::CommandLine(text)
            if *declared == name =>
        {
            Some(text.as_str())
        }
        _ => None,
    })
}

fn texts<'a>(checked: &'a [(&'static str, Checked)], name: &str) -> Option<&'a [String]> {
    checked.iter().find_map(|(declared, value)| match value {
        Checked::Texts(items) if *declared == name => Some(items.as_slice()),
        _ => None,
    })
}

/// Selects one of a tool's compatible text spellings. Both missing and both
/// present are errors: silently preferring one would make a model believe the
/// other content was written.
fn exclusive_text<'a>(
    checked: &'a [(&'static str, Checked)],
    text_name: &str,
    lines_name: &str,
) -> Result<&'a str, &'static str> {
    match (text(checked, text_name), text(checked, lines_name)) {
        (Some(value), None) | (None, Some(value)) => Ok(value),
        (None, None) => Err("neither was provided"),
        (Some(_), Some(_)) => Err("both were provided"),
    }
}

/// The child's argv, built only from checked values.
///
/// Nothing here quotes anything, because nothing here builds a string for a
/// shell: each element is one `execvp` argument. `bash` is the one tool
/// whose argument *is* a command line, and it got there through
/// `Profile::admits_command` rather than through this function.
fn build_argv(
    tool: &Tool,
    checked: &[(&'static str, Checked)],
) -> Result<Vec<std::ffi::OsString>, PermissionDenied> {
    let missing = |what: &str| PermissionDenied {
        tool: tool.name().to_string(),
        path: what.to_string(),
        rule: format!(
            "`{}`'s declaration and its argv shape disagree about `{what}`",
            tool.name()
        ),
    };
    let mut argv: Vec<std::ffi::OsString> = Vec::new();
    match tool.argv() {
        // Unreachable through `checked_call`, which branches first; a direct
        // caller gets an empty argv rather than a panic, and `spawn_confined`
        // refuses it for having no binary.
        Argv::InProcess => {}
        Argv::ReadPath => {
            let path = resolved_path(checked, "path").ok_or_else(|| missing("path"))?;
            argv.push("--".into());
            argv.push(path.into());
        }
        Argv::GrepIn => {
            let pattern = text(checked, "pattern").ok_or_else(|| missing("pattern"))?;
            let path = resolved_path(checked, "path").ok_or_else(|| missing("path"))?;
            argv.push("-r".into());
            argv.push("-n".into());
            // **`-E`, because the declaration says "a regular expression" and
            // a model writes one.** Without it `grep` is BRE, where `|` is a
            // literal pipe, `+` is a literal plus and `(a|b)` is a literal
            // parenthesis -- so `grep({pattern: "a|b"})` searched for the
            // three-character string `a|b` and found nothing, while `rg`
            // beside it in the same roster read the same pattern as
            // alternation. Measured on 2026-09-20: one session spent 128 s
            // and then 132 s walking a 9 GB tree for a literal
            // sixty-character string with five pipes in it, found nothing,
            // and tried a third way. A search that is silently answering a
            // different question is worse than a slow one.
            argv.push("-E".into());
            argv.push("-e".into());
            argv.push(pattern.into());
            argv.push("--".into());
            argv.push(path.into());
        }
        // The three pure search shapes, and their separators are
        // load-bearing rather than tidy: a declared flag list is the whole
        // flag list only while no model-authored element can become one.
        // `rg` puts the pattern behind `-e` and the path behind `--`; `fd`
        // and `jq` have no option form for theirs, so `--` is what holds.
        // Drop one and `fd --help` and `jq --version` are flags.
        Argv::SearchIn | Argv::SearchInAll => {
            let pattern = text(checked, "pattern").ok_or_else(|| missing("pattern"))?;
            let path = resolved_path(checked, "path").ok_or_else(|| missing("path"))?;
            argv.push("--no-config".into());
            if tool.argv() == Argv::SearchInAll {
                // `grep` has always read a project's own dot-files -- its
                // config, its `.env`, its `.settings` -- and a faster binary
                // underneath must not quietly take that away. `.git` is
                // excluded here rather than filtered from the output, which
                // is the same decision the spawned `grep` makes one arm up.
                argv.push("--hidden".into());
                argv.push("--glob=!.git/".into());
            }
            argv.push("--line-number".into());
            argv.push("--no-heading".into());
            argv.push("--color=never".into());
            argv.push("-e".into());
            argv.push(pattern.into());
            argv.push("--".into());
            argv.push(path.into());
        }
        Argv::FindIn => {
            let pattern = text(checked, "pattern").ok_or_else(|| missing("pattern"))?;
            let path = resolved_path(checked, "path").ok_or_else(|| missing("path"))?;
            argv.push("--color=never".into());
            argv.push("--".into());
            argv.push(pattern.into());
            argv.push(path.into());
        }
        Argv::JsonFilter => {
            let filter = text(checked, "filter").ok_or_else(|| missing("filter"))?;
            let path = resolved_path(checked, "path").ok_or_else(|| missing("path"))?;
            argv.push("--".into());
            argv.push(filter.into());
            argv.push(path.into());
        }
        Argv::ShellCommand => {
            let command = text(checked, "command").ok_or_else(|| missing("command"))?;
            #[cfg(not(windows))]
            argv.push("-c".into());
            // `cmd.exe`'s spelling of `-c`: `/d` skips AutoRun, `/s` makes
            // cmd strip exactly the outer quotes `windows::shell_command_line`
            // puts round the command, and `/c` runs it. On every platform the
            // command is the last element, which `spawn_confined` relies on.
            #[cfg(windows)]
            argv.extend(["/d", "/s", "/c"].map(std::ffi::OsString::from));
            argv.push(command.into());
        }
    }
    Ok(argv)
}

/// Resolves `program` to the binary the child will be exec'd on.
///
/// This is the 61D ruling's mechanism. A name is looked up on `PATH` and
/// then `canonicalize`d, so a `PATH` entry that is a symlink, a relative
/// component or a `..` produces the real path and not the spelling that
/// found it. A name that resolves to nothing is **not** an error here: the
/// call falls back to letting `execvp` search, bounded by the platform
/// applier's executable roots, and says so — in the returned value and in a
/// log line, because the two have different readers.
pub fn exec_grant(program: &str) -> ExecGrant {
    if let Some(binary) = resolve_program(program) {
        return ExecGrant {
            binary,
            fell_back_to_roots: false,
        };
    }
    eprintln!(
        "sterna: sandbox: `{program}` did not resolve to a binary on PATH; exec falls back to the \
         platform applier's executable roots (61D exec-roots ruling)"
    );
    ExecGrant {
        binary: PathBuf::from(program),
        fell_back_to_roots: true,
    }
}

pub fn resolve_program(program: &str) -> Option<PathBuf> {
    resolve_program_from(program, &std::env::current_dir().ok()?)
}

fn resolve_program_from(program: &str, cwd: &Path) -> Option<PathBuf> {
    let written = Path::new(program);
    if written.components().count() > 1 {
        let candidate = if written.is_absolute() {
            written.to_path_buf()
        } else {
            cwd.join(written)
        };
        return resolved(&candidate);
    }
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path).find_map(|dir| {
        let dir = if dir.is_absolute() {
            dir
        } else {
            cwd.join(dir)
        };
        resolved(&dir.join(program))
    })
}

/// The canonical path `candidate` names, if it names a runnable file.
#[cfg(not(windows))]
fn resolved(candidate: &Path) -> Option<PathBuf> {
    if !runnable(candidate) {
        return None;
    }
    std::fs::canonicalize(candidate).ok()
}

/// The canonical path `candidate` names **once Windows has had its say about
/// the extension**.
///
/// Without this, nothing resolves on Windows at all. `PATH` holds directories
/// and a tool is named `rg`, so the join is `…\rg` — a file that does not
/// exist, because the file is `…\rg.exe`. Every spawning tool then fell back
/// to the unresolved branch of [`exec_grant`], which is the wider grant, on
/// the one platform whose applier cannot narrow an exec grant at all.
///
/// The bare spelling is tried first so a name that already carries its
/// extension resolves to itself rather than to `rg.exe.com`, and `PATHEXT` is
/// read from the environment because it is the list the shell itself uses;
/// the default is the one `cmd.exe` ships with, for a session started without
/// it.
#[cfg(windows)]
fn resolved(candidate: &Path) -> Option<PathBuf> {
    if runnable(candidate)
        && let Ok(path) = std::fs::canonicalize(candidate)
    {
        return Some(path);
    }
    let name = candidate.file_name()?.to_os_string();
    let extensions = std::env::var("PATHEXT").unwrap_or_else(|_| ".COM;.EXE;.BAT;.CMD".to_string());
    for extension in extensions.split(';').filter(|part| !part.is_empty()) {
        let mut spelling = name.clone();
        spelling.push(extension);
        let with = candidate.with_file_name(spelling);
        if runnable(&with)
            && let Ok(path) = std::fs::canonicalize(&with)
        {
            return Some(path);
        }
    }
    None
}

#[cfg(unix)]
fn runnable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path)
        .map(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

#[cfg(not(unix))]
fn runnable(path: &Path) -> bool {
    std::fs::metadata(path)
        .map(|meta| meta.is_file())
        .unwrap_or(false)
}

/// Confines the child and spawns it, in that order and in no other.
///
/// The `Command` is built and handed to [`confined_spawn`], which confines
/// and spawns as one act. That is what makes "no unconfined path" mechanical
/// rather than promised: this function never holds a `Command` and a spawn
/// result at the same time, because the call that would produce the second
/// consumes the first.
///
/// The child is spawned rather than run to completion because a cancellation
/// needs a [`ConfinedChild`] to kill; the two drain threads are what `output()` did
/// internally, and the poll loop is where `token` is asked. Three outcomes:
///
/// - set before the spawn: no child is ever created, and the check is the
///   last statement before `spawn()`, so "never created" is a property of the
///   control flow and not of a race;
/// - set while the child runs: `kill` then `wait`, because the `wait` is what
///   leaves nothing behind for `init` to reap;
/// - neither: exactly the `ToolResult` this function returned before.
fn spawn_confined(
    profile: &Profile,
    stopped: &dyn Fn() -> bool,
    tool: &Tool,
    argv: &[std::ffi::OsString],
    waiting: Option<&std::sync::Arc<crate::approval::WaitClock>>,
) -> Result<ToolResult, ToolError> {
    let cancelled = || ToolError::Cancelled {
        tool: tool.name().to_string(),
    };
    // `Some` by construction: `checked_call` sends every `Argv::InProcess`
    // tool down the other arm, and those are the only ones without a binary.
    let Some(executable) = tool.executable() else {
        return Err(ToolError::Spawn {
            tool: tool.name().to_string(),
            program: PathBuf::new(),
            error: "an in-process tool reached spawn_confined".to_string(),
        });
    };
    let grant = exec_grant(executable);
    // The unresolved branch is a refusal on Windows rather than a wider
    // grant, because that platform has no wider grant to fall back *to*: see
    // [`ExecGrant`]. Refused here as well as in `windows::spawn` so the
    // model is told which tool is missing, rather than being told a path it
    // never chose was not absolute.
    #[cfg(windows)]
    if grant.fell_back_to_roots {
        return Err(ToolError::Denied(PermissionDenied {
            tool: tool.name().to_string(),
            path: grant.binary.display().to_string(),
            rule: format!(
                "`{executable}` is not installed on this machine, and on Windows an unresolved \
                 program name would be completed from Sterna's own current directory -- the \
                 project root, which the model can write. The tool is refused rather than \
                 resolved there (docs/sandbox.md, per platform)"
            ),
        }));
    }
    let mut command = Command::new(&grant.binary);
    command.args(argv);
    command.current_dir(profile.root());
    // **The child does not inherit this session's credentials.**
    //
    // `sandbox-grants.md` §4.2 makes the OS keyring never-grantable and §4.1
    // denies the network; a provider key sitting in the environment is the
    // same class of secret and was the one route left to it. Measured
    // 2026-09-06: `printenv ANTHROPIC_API_KEY` from inside a cell returned
    // the key, and a cell's output reaches the transcript, the rollout file
    // on disk and every hook payload -- an exfiltration path that needs no
    // network at all.
    //
    // Removed by name rather than by value: a value-matching scrub cannot
    // see a credential this process never read, and `env_clear` would take
    // `PATH` and `HOME` with it and break every tool.
    for name in std::env::vars_os()
        .map(|(name, _)| name)
        .filter(|name| is_credential_variable(&name.to_string_lossy()))
    {
        command.env_remove(name);
    }

    // Git must not discover host configuration outside the admitted project.
    // Besides containing credential helpers, an unreadable ~/.gitconfig makes
    // even `git status` fail under Seatbelt. Keep repository config and explicit
    // `git -c` options, but give all shell descendants a safe host-config default.
    let null_config = if cfg!(windows) { "NUL" } else { "/dev/null" };
    command.env("GIT_CONFIG_GLOBAL", null_config);
    command.env("GIT_CONFIG_SYSTEM", null_config);

    // **The network is Sterna's proxy, or nothing.** A confined command's
    // tools are pointed at the proxy that lets the allowed hosts through;
    // an outer proxy this process inherited is replaced, never passed on,
    // because the command could not reach it past the sandbox anyway.
    if let Some(route) = profile.proxy() {
        for (name, value) in &route.env {
            command.env(name, value);
        }
    }

    // The child leads a process group of its own, so a cancellation can name
    // everything the call started and not only the handle it holds. See
    // [`kill_and_reap`] for why that is the difference between stopping a
    // call and stopping a process. Windows has no process group and gets the
    // same guarantee from a job object, which `confined_spawn` creates.
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }

    let spawn_failed = |error: std::io::Error| ToolError::Spawn {
        tool: tool.name().to_string(),
        program: grant.binary.clone(),
        error: error.to_string(),
    };

    // The last statement before the spawn, and it is the last statement
    // before the *confinement* too, because they are now one call. A
    // cancellation that landed before this line leaves no child anywhere,
    // which is a property of the control flow and not of a race.
    if stopped() {
        return Err(cancelled());
    }

    // The command tool's line is built the way its interpreter reads it,
    // which on Windows is not `CommandLineToArgvW`'s way — see
    // `windows::shell_command_line`. Every other tool's argv is quoted
    // argument by argument.
    let line = if tool.argv() == Argv::ShellCommand {
        LineShape::CmdTail
    } else {
        LineShape::Argv
    };
    let (mut child, confinement) = spawn_with_confinement_policy(
        profile,
        &grant.binary,
        tool.name(),
        command,
        Pipes {
            stdin: false,
            stdout: true,
            stderr: true,
        },
        line,
    )
    .map_err(|refusal| match refusal {
        SpawnRefusal::Denied(denied) => ToolError::Denied(denied),
        SpawnRefusal::Failed(error) => spawn_failed(error),
    })?;

    // A cancellation that landed in the window between the check above and
    // this line now has something to stop, and the poll loop below would not
    // ask the token again for a whole `CANCEL_POLL`. Killing here rather
    // than a tick later is not about the latency: it is that this is the
    // window a caller is *most* likely to cancel in, because a caller that
    // cancels at all usually cancels early.
    if stopped() {
        kill_and_reap(&mut child);
        return Err(cancelled());
    }

    let stdout = drain(child.take_stdout());
    let stderr = drain(child.take_stderr());

    // From here to the last byte of the pipes, the caller is waiting on a
    // child it was granted rather than computing, so its compute clock stops
    // (`RuntimeState::away_from_js`). `stopped()` still answers, so a person
    // stopping the task still kills this child on the next poll.
    let _waiting = waiting.map(|clock| clock.pause());

    let status = loop {
        if stopped() {
            kill_and_reap(&mut child);
            return Err(cancelled());
        }
        match process::try_complete(&mut child) {
            Ok(Some(status)) => break status,
            Ok(None) => std::thread::sleep(CANCEL_POLL),
            // The wait itself failing leaves a running child nothing here
            // can observe again, so it is killed on the way out. Reporting
            // it as `Spawn` is not new lumping: `output()` raised the same
            // variant for its own wait and read failures.
            Err(error) => {
                kill_and_reap(&mut child);
                return Err(spawn_failed(error));
            }
        }
    };

    // Descendants in the owned group have now been stopped. Keep the final
    // pipe drain cancellable too; a process that deliberately escaped the
    // group must not hold this host callback indefinitely through its pipe.
    while !stdout.is_finished() || !stderr.is_finished() {
        if stopped() {
            return Err(cancelled());
        }
        std::thread::sleep(CANCEL_POLL);
    }
    Ok(ToolResult {
        modified: None,
        tool: tool.name().to_string(),
        stdout: collect(stdout),
        stderr: collect(stderr),
        exit_code: status.code(),
        grant,
        confinement,
    })
}

/// Reads one of the child's pipes to EOF on a thread of its own.
///
/// The invariant is that neither pipe can fill while the other is being
/// read, which is what `Command::output` did internally and what this
/// function restores now that the wait is explicit.
fn drain<R: Read + Send + 'static>(pipe: Option<R>) -> JoinHandle<Vec<u8>> {
    std::thread::spawn(move || {
        let mut buffer = Vec::new();
        if let Some(mut pipe) = pipe {
            let _ = pipe.read_to_end(&mut buffer);
        }
        buffer
    })
}

/// What the drain thread read, or nothing if it panicked — which a
/// `read_to_end` into a `Vec` does not do, so the `unwrap_or_default` is a
/// shape and not a fallback.
fn collect(handle: JoinHandle<Vec<u8>>) -> String {
    let bytes = handle.join().unwrap_or_default();
    String::from_utf8_lossy(&bytes).into_owned()
}

/// Kills everything the call started and reaps the child, in that order, so
/// nothing is left for `init`.
///
/// **The group is killed first, and the group is the point.** A child
/// handle names the process sterna spawned and nothing that process started,
/// so killing the handle alone stops the shell and leaves its background
/// jobs running — a `bash` call that started a server, cancelled, leaves the
/// server spinning at 100% until the machine is rebooted. That is not
/// hypothetical: `tests/tools.rs` reproduces it in one fixture.
///
/// Killing a group is safe here only because [`spawn_confined`] *created*
/// this one: `process_group(0)` makes the child a group leader whose group
/// id is its own pid, so the members are exactly the processes this call
/// started. Windows has no process group; `ConfinedChild::kill` terminates
/// the job object the same spawn created, which has the same membership for
/// the same reason. Killing a group sterna did not create is how a cancellation
/// becomes an outage, which is why the group is established at the spawn
/// rather than guessed at the kill. The order matters for the same reason:
/// once `wait` has reaped the child, its pid — and therefore the group id —
/// may be reused, so the group must be signalled while the child is still
/// unreaped.
///
/// Every result is discarded deliberately. A child that exited between the
/// poll and the `kill` is not an error, it is the race this function exists
/// to be indifferent to, and a group that no longer has members answers
/// `ESRCH` for the same reason.
///
/// The two drain threads are *not* joined here. Their output is discarded by
/// a cancelled call, and joining them would make cancellation wait on a
/// grandchild that inherited the pipe — the one thing a bounded cancellation
/// must not do.
pub(crate) fn kill_and_reap(child: &mut ConfinedChild) {
    #[cfg(unix)]
    kill_group(child.id());
    let _ = child.kill();
    let _ = child.wait();
}

/// `SIGKILL`, which is 9 on every unix sterna builds for.
#[cfg(unix)]
const SIGKILL: i32 = 9;

#[cfg(unix)]
unsafe extern "C" {
    /// POSIX `killpg`. Declared rather than depended on: sterna has no `libc`
    /// in its tree, and this is the whole of what it would be used for.
    fn killpg(pgrp: i32, sig: i32) -> i32;
}

/// `SIGKILL`s the process group led by `pid`.
///
/// `pid` is a child [`spawn_confined`] made a group leader, so the group id
/// is the pid and its members are exactly what that call started.
#[cfg(unix)]
pub(crate) fn kill_group(pid: u32) {
    // SAFETY: `killpg` is a POSIX libc call taking two integers and
    // returning one. There is no pointer, no allocation and no state; the
    // only failure it can report is `ESRCH` for a group whose members have
    // all exited, which is the ordinary case and is discarded above.
    unsafe {
        killpg(pid as i32, SIGKILL);
    }
}

/// Which of a confined child's three standard streams is a pipe.
///
/// `stdout` is a pipe for every caller here; the other two differ, and a
/// spawn that took three `Stdio` values could not describe the Windows path
/// at all — that one creates its own handles, because it creates its own
/// process.
pub(crate) use crate::sandbox::windows::Pipes;

/// How a confined child's command line is assembled — one string on Windows,
/// where `cmd.exe` reads it by its own rules and every other program by
/// `CommandLineToArgvW`'s. The Unix appliers take an argv and ignore it.
pub(crate) use crate::sandbox::windows::LineShape;

/// A child that exists **only** because a confinement was applied first.
///
/// The type has no constructor other than [`confined_spawn`], and
/// [`confined_spawn`] applies the platform's confinement in the same call
/// that creates the process. That is this module's first invariant expressed
/// as a type: there is no longer an intermediate value — a `Command` a
/// confinement decorated — that a caller could hold and spawn without
/// checking a result. Before this, the guarantee was "the `?` is above the
/// `spawn()`", which is a property of a line's position; now the two are one
/// expression.
///
/// It also had to become a type rather than a `std::process::Child`. Stable
/// `std` cannot attach an AppContainer to a `Command` (`raw_attribute` is
/// nightly, rust-lang/rust#114854) and cannot build a `Child` from a handle,
/// so the Windows applier owns its own `CreateProcessW` and hands back its
/// own child.
pub(crate) struct ConfinedChild {
    inner: PlatformChild,
}

#[cfg(target_os = "windows")]
type PlatformChild = crate::sandbox::windows::ContainedChild;
#[cfg(not(target_os = "windows"))]
type PlatformChild = std::process::Child;

/// The reader on the child's standard output. A `File` on Windows because the
/// pipe is this crate's own; `Read + Send + 'static` either way, which is all
/// [`drain`] asks for.
#[cfg(target_os = "windows")]
pub(crate) type StdoutPipe = std::fs::File;
#[cfg(not(target_os = "windows"))]
pub(crate) type StdoutPipe = std::process::ChildStdout;
#[cfg(target_os = "windows")]
pub(crate) type StderrPipe = std::fs::File;
#[cfg(not(target_os = "windows"))]
pub(crate) type StderrPipe = std::process::ChildStderr;
#[cfg(target_os = "windows")]
pub(crate) type StdinPipe = std::fs::File;
#[cfg(not(target_os = "windows"))]
pub(crate) type StdinPipe = std::process::ChildStdin;

impl ConfinedChild {
    /// The child's own pid, which is also its process-group id — the unix
    /// appliers make the child a group leader and then name that group when
    /// they kill or wait. Windows names a job object instead and never needs
    /// the pid, so the accessor does not exist there rather than sitting
    /// unread.
    #[cfg(unix)]
    pub(crate) fn id(&self) -> u32 {
        self.inner.id()
    }

    pub(crate) fn take_stdin(&mut self) -> Option<StdinPipe> {
        self.inner.stdin.take()
    }

    pub(crate) fn take_stdout(&mut self) -> Option<StdoutPipe> {
        self.inner.stdout.take()
    }

    pub(crate) fn take_stderr(&mut self) -> Option<StderrPipe> {
        self.inner.stderr.take()
    }

    /// Kills the child and everything it started — the process group on unix,
    /// the job object on Windows.
    pub(crate) fn kill(&mut self) -> std::io::Result<()> {
        self.inner.kill()
    }

    pub(crate) fn wait(&mut self) -> std::io::Result<std::process::ExitStatus> {
        self.inner.wait()
    }

    /// Only the platforms without the `waitid(WNOWAIT)` dance ask this: on
    /// macOS and Linux the exit has to be observed *without* reaping, so the
    /// group id is still reserved when the descendants are signalled, and
    /// `process::try_complete` does that by hand.
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    pub(crate) fn try_wait(&mut self) -> std::io::Result<Option<std::process::ExitStatus>> {
        self.inner.try_wait()
    }
}

/// Why no child was created.
///
/// The two are different in kind and the callers report them differently:
/// [`SpawnRefusal::Denied`] is the sandbox's own answer and is a
/// `PermissionDenied` a program can catch (§1.4); [`SpawnRefusal::Failed`] is
/// the operating system declining to start a program the sandbox had already
/// admitted.
pub(crate) enum SpawnRefusal {
    Denied(PermissionDenied),
    Failed(std::io::Error),
}

/// Confines and spawns, in that order and as one act.
///
/// `command` is taken **by value**, which is the structural half of the
/// invariant: after this call the caller no longer holds a `Command` and
/// cannot spawn one. A platform with no applier that has ever executed
/// refuses rather than spawning — spawning there "for now" would be the one
/// unconfined path this module exists to not have.
pub(crate) fn confined_spawn(
    profile: &Profile,
    binary: &Path,
    tool: &str,
    command: Command,
    pipes: Pipes,
    line: LineShape,
) -> Result<(ConfinedChild, Confinement), SpawnRefusal> {
    spawn_with_confinement_policy(profile, binary, tool, command, pipes, line)
}

/// Applies the explicit host-selected bypass or delegates to the platform
/// applier. The bypass lives at the point that owns the `Command`, so a
/// confinement error can never turn into an implicit unconfined retry.
fn spawn_with_confinement_policy(
    profile: &Profile,
    binary: &Path,
    tool: &str,
    command: Command,
    pipes: Pipes,
    line: LineShape,
) -> Result<(ConfinedChild, Confinement), SpawnRefusal> {
    if profile.os_sandbox_bypassed() {
        let child = spawn_bypassed(profile, binary, tool, command, pipes, line)?;
        return Ok((child, Confinement::DangerouslyUnconfined));
    }
    platform_confined_spawn(profile, binary, tool, command, pipes, line)
}

/// The explicit host-selected bypass: a plain `std` spawn with the pipes
/// installed, on the two platforms whose child is a `std::process::Child`.
/// The program and the line shape are the `Command`'s own here.
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn spawn_bypassed(
    _profile: &Profile,
    _binary: &Path,
    _tool: &str,
    mut command: Command,
    pipes: Pipes,
    _line: LineShape,
) -> Result<ConfinedChild, SpawnRefusal> {
    apply_pipes(&mut command, pipes);
    let child = command.spawn().map_err(SpawnRefusal::Failed)?;
    Ok(ConfinedChild { inner: child })
}

/// The explicit host-selected bypass on Windows: the applier's own
/// `CreateProcessW` — the same pipes, job and `LineShape` command line as the
/// confined path — entered into no container. The child is still the
/// applier's `ContainedChild`, so nothing about how it is read, waited on or
/// killed differs from a confined one. A failure here is a spawn failure and
/// never a permission decision, because no confinement was asked for.
#[cfg(target_os = "windows")]
fn spawn_bypassed(
    profile: &Profile,
    binary: &Path,
    _tool: &str,
    command: Command,
    pipes: Pipes,
    line: LineShape,
) -> Result<ConfinedChild, SpawnRefusal> {
    use crate::sandbox::windows::SpawnError;
    match crate::sandbox::windows::spawn_unconfined(profile, binary, &command, pipes, line) {
        Ok(child) => Ok(ConfinedChild { inner: child }),
        Err(
            SpawnError::NotConfinable(error)
            | SpawnError::NotStarted(error)
            | SpawnError::NotPrepared(error),
        ) => Err(SpawnRefusal::Failed(error)),
    }
}

/// **Where no applier can hand back this module's own child, the bypass
/// refuses rather than spawning**: a bypass that spawned anyway would be the
/// one unconfined path this module exists not to have.
#[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
fn spawn_bypassed(
    _profile: &Profile,
    _binary: &Path,
    tool: &str,
    _command: Command,
    _pipes: Pipes,
    _line: LineShape,
) -> Result<ConfinedChild, SpawnRefusal> {
    Err(SpawnRefusal::Denied(PermissionDenied {
        tool: tool.to_string(),
        path: String::new(),
        rule: "the dangerously-unconfined bypass has no applier on this platform, so nothing was spawned".to_string(),
    }))
}

#[cfg(target_os = "macos")]
fn platform_confined_spawn(
    profile: &Profile,
    _binary: &Path,
    tool: &str,
    mut command: Command,
    pipes: Pipes,
    _line: LineShape,
) -> Result<(ConfinedChild, Confinement), SpawnRefusal> {
    apply_pipes(&mut command, pipes);
    crate::sandbox::macos::confine(profile, &mut command).map_err(|error| {
        SpawnRefusal::Denied(PermissionDenied {
            tool: tool.to_string(),
            path: String::new(),
            rule: format!(
                "the seatbelt profile could not be applied, so nothing was spawned: {error}"
            ),
        })
    })?;
    let child = command.spawn().map_err(SpawnRefusal::Failed)?;
    Ok((ConfinedChild { inner: child }, Confinement::Seatbelt))
}

#[cfg(target_os = "linux")]
fn platform_confined_spawn(
    profile: &Profile,
    _binary: &Path,
    tool: &str,
    mut command: Command,
    pipes: Pipes,
    _line: LineShape,
) -> Result<(ConfinedChild, Confinement), SpawnRefusal> {
    let refused = |rule: String| {
        SpawnRefusal::Denied(PermissionDenied {
            tool: tool.to_string(),
            path: String::new(),
            rule,
        })
    };
    apply_pipes(&mut command, pipes);
    match crate::sandbox::linux::confine(profile, &mut command) {
        Ok(true) => {}
        // `linux::confine` returns `Ok(false)` below Landlock ABI 3 and
        // installs nothing. That is a refusal here rather than a warning.
        Ok(false) => {
            return Err(refused(
                "this host lacks Landlock ABI 3 or a supported seccomp architecture, so confinement could not be installed and \
                 sterna does not spawn a tool unconfined (docs/sandbox.md, per platform)"
                    .to_string(),
            ));
        }
        Err(error) => {
            return Err(refused(format!(
                "Landlock/seccomp confinement could not be installed, so nothing was spawned: {error}"
            )));
        }
    }
    let child = command.spawn().map_err(SpawnRefusal::Failed)?;
    Ok((ConfinedChild { inner: child }, Confinement::Landlock))
}

/// The AppContainer path, where the confinement and the spawn were never
/// separable.
///
/// `windows::spawn` is one call because `CreateProcessW` takes the container
/// as an argument: there is no earlier moment at which a `Command` could
/// carry it. Every failure inside it — no user SID, no container, an image
/// the container cannot load, an ACL that would not take the grant — comes
/// back as a refusal here and nothing is started.
#[cfg(target_os = "windows")]
fn platform_confined_spawn(
    profile: &Profile,
    binary: &Path,
    tool: &str,
    command: Command,
    pipes: Pipes,
    line: LineShape,
) -> Result<(ConfinedChild, Confinement), SpawnRefusal> {
    use crate::sandbox::windows::SpawnError;
    match crate::sandbox::windows::spawn(profile, binary, &command, pipes, line) {
        Ok(child) => Ok((ConfinedChild { inner: child }, Confinement::AppContainer)),
        Err(refusal @ SpawnError::NotConfinable(_)) => {
            Err(SpawnRefusal::Denied(PermissionDenied {
                tool: tool.to_string(),
                path: binary.display().to_string(),
                rule: refusal.to_string(),
            }))
        }
        // The bypass's own variant; `spawn` never returns it, and a match
        // that named only two arms would stop compiling when it did.
        Err(SpawnError::NotStarted(error) | SpawnError::NotPrepared(error)) => {
            Err(SpawnRefusal::Failed(error))
        }
    }
}

#[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
fn platform_confined_spawn(
    profile: &Profile,
    binary: &Path,
    tool: &str,
    command: Command,
    pipes: Pipes,
    line: LineShape,
) -> Result<(ConfinedChild, Confinement), SpawnRefusal> {
    let _ = (profile, binary, command, pipes, line);
    Err(SpawnRefusal::Denied(PermissionDenied {
        tool: tool.to_string(),
        path: String::new(),
        rule:
            "sterna has no sandbox applier that has ever executed on this platform, and does not \
               spawn a tool unconfined (docs/sandbox.md, per platform)"
                .to_string(),
    }))
}

/// Hands `pipes` to the `Command` the unix appliers spawn from.
///
/// Windows has no equivalent here on purpose: its handles are created by the
/// same call that creates the process, so there is nothing to install on a
/// `Command` that `CreateProcessW` would then read.
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn apply_pipes(command: &mut Command, pipes: Pipes) {
    use std::process::Stdio;
    let stream = |piped: bool| {
        if piped { Stdio::piped() } else { Stdio::null() }
    };
    command.stdin(stream(pipes.stdin));
    command.stdout(stream(pipes.stdout));
    command.stderr(stream(pipes.stderr));
}

#[cfg(test)]
mod descendant_resolution_tests {
    use super::*;

    #[test]
    fn a_relative_descendant_resolves_from_the_requested_project_cwd() {
        let root = std::env::temp_dir().join(format!(
            "sterna-descendant-resolution-{}",
            std::process::id()
        ));
        let bin = root.join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let source = std::env::current_exe().unwrap();
        let wanted = bin.join("python-fixture");
        std::fs::copy(&source, &wanted).unwrap();

        let resolved = resolve_program_from("./bin/python-fixture", &root)
            .expect("the relative executable resolves from the project root");
        assert_eq!(resolved, std::fs::canonicalize(&wanted).unwrap());
        let _ = std::fs::remove_dir_all(root);
    }
}

/// Truncates on a character boundary, marking that it did.
fn truncate(text: &str, limit: usize) -> String {
    if text.len() <= limit {
        return text.to_string();
    }
    let mut end = limit;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…[{} bytes truncated]", &text[..end], text.len() - end)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The `/tool` result line says where a call ran in words, never by the
    /// sandbox backend's name.
    #[test]
    fn where_a_call_ran_is_said_plainly() {
        for backend in [
            Confinement::Seatbelt,
            Confinement::Landlock,
            Confinement::AppContainer,
        ] {
            assert_eq!(backend.plainly(), "ran in the sandbox");
        }
        assert_eq!(Confinement::InProcess.plainly(), "ran inside Sterna");
        assert_eq!(
            Confinement::DangerouslyUnconfined.plainly(),
            "ran without the OS sandbox"
        );
        assert_eq!(
            Confinement::BrokeredNetwork.plainly(),
            "went through the web broker"
        );
    }

    /// **The fallback's dialect, checked structurally because on a machine
    /// with ripgrep it never runs.** `checked_call` hands a `grep` call to
    /// ripgrep wherever ripgrep is installed, so every behavioural search
    /// test on such a machine passes whether or not this flag is here --
    /// measured: the mutation that removed it SURVIVED the whole search
    /// suite. What it guards is the other machine, where `grep` is BRE and
    /// `|` is a literal pipe, and the declaration promises a regular
    /// expression on both.
    // Windows performs `grep` in process (`registry.rs`, `GREP` on
    // Windows), so no argv is built there; its dialect is held by
    // `search::tests::extended_groups_alternation_intervals_and_the_gnu_escapes`.
    #[cfg(not(windows))]
    #[test]
    fn the_spawned_grep_is_given_the_extended_dialect_the_declaration_promises() {
        let tool = crate::tools::registry::lookup("grep").expect("grep is in the roster");
        let checked = vec![
            ("pattern", Checked::Pattern("a|b".into())),
            ("path", Checked::Path(std::path::PathBuf::from("."))),
        ];
        let argv = build_argv(tool, &checked).expect("argv");
        assert!(
            argv.iter().any(|flag| flag == "-E"),
            "the spawned grep would read `a|b` as a literal pipe: {argv:?}"
        );
        // The pattern stays behind `-e`, where a value spelled like an
        // option is that option's value before it is anything else.
        let at = argv.iter().position(|flag| flag == "-e").expect("-e");
        assert_eq!(argv[at + 1], "a|b");
    }

    /// The command the shell fixture runs to print `bypass-ok` exactly:
    /// `printf` on the POSIX shell, `echo` on `cmd.exe`, whose trailing CRLF
    /// the test trims.
    #[cfg(windows)]
    const BYPASS_ECHO: &str = "echo bypass-ok";
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    const BYPASS_ECHO: &str = "printf bypass-ok";

    /// **The explicit bypass spawns a real unconfined child on every platform
    /// that has an applier, and never becomes an implicit fallback.** It is
    /// selected only by `Profile::os_sandbox_bypassed()`, and the child it
    /// makes is this module's own — read, waited on and killed like a
    /// confined one — so the result names the confinement `Dangerously
    /// Unconfined` and nothing else changes. Before 2026-09-17 the Windows
    /// arm refused by name, because the unconfined `CreateProcessW` had not
    /// been written (`GH-PANE-WINDOWS-BYPASS`). Everywhere without an
    /// applier, `spawn_bypassed` refuses by name — the test after the next.
    #[cfg(any(target_os = "macos", target_os = "linux", windows))]
    #[test]
    fn explicit_os_sandbox_bypass_is_never_an_implicit_fallback() {
        let root = std::env::temp_dir().join(format!(
            "sterna-explicit-sandbox-bypass-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let settings = serde_json::json!({"permissions":{"allow":["Bash"]}}).to_string();
        let profile = Profile::compile(&root, Some(&settings)).with_os_sandbox_bypass();
        let ctx = ToolContext {
            profile: &profile,
            session: &SessionId::new("explicit-sandbox-bypass"),
        };
        let result = run(&ctx, "bash", &Args::new().with("command", BYPASS_ECHO)).unwrap();
        assert_eq!(result.stdout.trim_end(), "bypass-ok");
        assert_eq!(result.confinement, Confinement::DangerouslyUnconfined);
        assert!(
            result
                .confinement
                .as_str()
                .contains("outer isolation required")
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    /// The bypass is the one variable, exercised through a real spawn on
    /// both halves: `bash` writes a file outside every writable place. Under
    /// the bypass the write lands; under confinement the seatbelt, Landlock
    /// or AppContainer refuses it inside the child. The directory sits beside
    /// this test binary, because the temp folders are writable places.
    #[cfg(any(target_os = "macos", target_os = "linux", windows))]
    #[test]
    fn a_bypassed_child_writes_what_the_sandbox_would_refuse() {
        let beside = std::env::current_exe()
            .unwrap()
            .parent()
            .unwrap()
            .to_path_buf();
        let base = beside.join(format!(
            "sterna-bypass-reach-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let root = std::env::temp_dir().join(base.file_name().unwrap());
        let outside = base.join("outside");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        let target = outside.join("made.txt");
        let settings = serde_json::json!({"permissions": {"allow": ["Bash"]}}).to_string();
        #[cfg(windows)]
        let command = format!("echo reached> \"{}\"", target.display());
        #[cfg(not(windows))]
        let command = format!("printf reached > '{}'", target.display());

        let write_under = |bypassed: bool| {
            let profile = {
                let p = Profile::compile(&root, Some(&settings));
                if bypassed {
                    p.with_os_sandbox_bypass()
                } else {
                    p
                }
            };
            let ctx = ToolContext {
                profile: &profile,
                session: &SessionId::new("bypass-reach"),
            };
            run(&ctx, "bash", &Args::new().with("command", &command))
        };

        // The confined half spawns -- Sterna admits the line -- and the OS
        // sandbox refuses the write inside the child.
        let confined = write_under(false).expect("the confined child spawns");
        assert_ne!(confined.confinement, Confinement::DangerouslyUnconfined);
        assert!(
            matches!(confined.exit_code, Some(code) if code != 0),
            "the confined write must fail inside the child: {confined:?}"
        );
        assert!(!target.exists(), "the confined child wrote outside");

        let bypassed = write_under(true).expect("the unconfined child spawns");
        assert_eq!(bypassed.confinement, Confinement::DangerouslyUnconfined);
        assert!(
            target.exists(),
            "the unconfined child must write: {bypassed:?}"
        );

        let _ = std::fs::remove_dir_all(base);
        let _ = std::fs::remove_dir_all(root);
    }

    /// Where no applier can hand back this module's own child, the bypass
    /// refuses by name and spawns nothing, rather than becoming the one
    /// unconfined path — `spawn_bypassed`'s third arm.
    #[cfg(not(any(target_os = "macos", target_os = "linux", windows)))]
    #[test]
    fn explicit_os_sandbox_bypass_refuses_by_name_where_no_applier_exists() {
        let root = std::env::temp_dir().join(format!(
            "sterna-explicit-sandbox-bypass-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let settings = serde_json::json!({"permissions":{"allow":["Bash"]}}).to_string();
        let profile = Profile::compile(&root, Some(&settings)).with_os_sandbox_bypass();
        let ctx = ToolContext {
            profile: &profile,
            session: &SessionId::new("explicit-sandbox-bypass"),
        };
        let refusal = run(
            &ctx,
            "bash",
            &Args::new().with("command", "printf bypass-ok"),
        )
        .unwrap_err();
        let text = format!("{refusal:?}");
        assert!(
            text.contains("has no applier on this platform, so nothing was spawned"),
            "the bypass must refuse by name where it cannot spawn its own child: {text}"
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn host_stop_predicate_cancels_glob_during_traversal() {
        let root = std::env::temp_dir().join(format!("sterna-stop-glob-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        for i in 0..20 {
            std::fs::write(root.join(format!("file-{i}")), "x").unwrap();
        }
        let settings =
            serde_json::json!({"permissions":{"allow":[format!("Read({}/**)", root.display())]}})
                .to_string();
        let profile = Profile::compile(&root, Some(&settings));
        let ctx = ToolContext {
            profile: &profile,
            session: &SessionId::new("stop-glob"),
        };
        let polls = std::cell::Cell::new(0);
        let result = run_traced_with_gate(
            &ctx,
            &CancellationToken::new(),
            "glob",
            &Args::new()
                .with("path", root.to_string_lossy())
                .with("pattern", "**"),
            None,
            &|| {
                polls.set(polls.get() + 1);
                polls.get() >= 6
            },
        );
        assert!(matches!(result.outcome, Err(ToolError::Cancelled { .. })));
        assert_eq!(
            polls.get(),
            6,
            "glob never asked the host stop predicate while walking"
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn an_unknown_tool_name_is_a_refusal_and_not_a_panic() {
        let profile = Profile::compile(std::env::temp_dir(), None);
        let session = SessionId::new("test");
        let ctx = ToolContext {
            profile: &profile,
            session: &session,
        };
        let error = run(&ctx, "webfetch", &Args::new()).unwrap_err();
        let denied = error.denied().expect("a refusal, not a spawn failure");
        assert_eq!(denied.tool, "webfetch");
        assert!(
            denied
                .rule
                .contains("no tool named `webfetch` is registered")
        );
    }

    #[test]
    fn an_undeclared_argument_is_refused_rather_than_ignored() {
        let profile = Profile::compile(std::env::temp_dir(), None);
        let tool = registry::lookup("read").unwrap();
        let args = Args::new().with("path", "x").with("depth", "3");
        let denied = check_arguments(&profile, tool, &args, &mut CheckedArgs::new()).unwrap_err();
        assert_eq!(denied.path, "depth");
        assert!(denied.rule.contains("declares no argument named `depth`"));
    }

    #[test]
    fn a_missing_required_argument_is_refused() {
        let profile = Profile::compile(std::env::temp_dir(), None);
        let tool = registry::lookup("grep").unwrap();
        let denied =
            check_arguments(&profile, tool, &Args::new(), &mut CheckedArgs::new()).unwrap_err();
        assert!(denied.rule.contains("requires an argument named `pattern`"));
    }

    #[test]
    fn a_pattern_never_becomes_an_option_of_the_child() {
        let profile = Profile::compile(std::env::temp_dir(), None);
        let tool = registry::lookup("grep").unwrap();
        let args = Args::new().with("pattern", "-rf");
        let mut trace = CheckedArgs::new();
        let checked = check_arguments(&profile, tool, &args, &mut trace).unwrap();
        // The trajectory records the pattern as admitted, and only that.
        assert_eq!(trace.get("pattern").map(String::as_str), Some("-rf"));
        #[cfg(not(windows))]
        {
            let argv = build_argv(tool, &checked).unwrap();
            let position = argv.iter().position(|a| a == "-rf").unwrap();
            assert_eq!(argv[position - 1], "-e", "{argv:?}");
        }
        // On Windows `grep` builds no argv at all: the pattern is data to
        // the in-process matcher, and there is no option for it to become.
        #[cfg(windows)]
        {
            assert_eq!(tool.argv(), Argv::InProcess);
            assert!(build_argv(tool, &checked).unwrap().is_empty());
        }
    }

    #[test]
    fn truncation_marks_itself_and_keeps_a_character_boundary() {
        let text = "é".repeat(40);
        let cut = truncate(&text, 11);
        assert!(cut.starts_with("ééééé"));
        assert!(cut.contains("bytes truncated"));
        assert_eq!(truncate("short", 11), "short");
    }
}
