//! A host-only, exact-call suspension seam. This is not a grant mechanism.
//!
//! A caller installs a gate on a runtime, receives requests on another thread,
//! and answers the one suspended Rust callback. The JavaScript cell stays on
//! its stack throughout; no source or continuation is returned for replay.
//! The immutable base profile must admit the call before a request is sent.
//! What reaches a person is decided by the session's sandbox level
//! (`permissions::judge`): every edit and command on `Ask`, only a request to
//! run outside the sandbox on `Sandboxed`, nothing on `Full`.

use std::collections::BTreeSet;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::time::{Duration, Instant};

use sha2::{Digest, Sha256};

use crate::tools::invoke::CheckedArgs;

/// Human confirmation is bounded independently of the cell compute clock.
pub const MAX_APPROVAL_WAIT: Duration = Duration::from_secs(10 * 60);

/// How long a cell has spent **not executing JavaScript**.
///
/// The cell's wall-clock limit exists to stop `while (true) {}`, which
/// allocates nothing and so is invisible to the heap ceiling. Time spent
/// inside a host callback -- a person deciding, a `cargo test` the cell was
/// granted, a helper answering -- is not that, and the runtime subtracts it
/// (`Watchdog::arm_pausing`). One clock serves every such wait: they differ
/// in what is being waited for and not in what the cell is doing, which is
/// nothing.
#[derive(Default)]
pub(crate) struct WaitClock(Mutex<WaitState>);
#[derive(Default)]
struct WaitState {
    accumulated: Duration,
    since: Option<Instant>,
}
impl WaitClock {
    pub(crate) fn elapsed(&self) -> Duration {
        let state = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.accumulated + state.since.map(|since| since.elapsed()).unwrap_or_default()
    }
    /// Stops the clock until the returned guard is dropped. Re-entrant
    /// waits are not nested: the outer guard owns the span, because
    /// `since` is a single instant and a nested pause would end it early.
    pub(crate) fn pause(self: &Arc<Self>) -> Waiting {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .since = Some(Instant::now());
        Waiting(self.clone())
    }
}
pub(crate) struct Waiting(Arc<WaitClock>);
impl Drop for Waiting {
    fn drop(&mut self) {
        let mut state = self
            .0
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(since) = state.since.take() {
            state.accumulated += since.elapsed();
        }
    }
}

/// The answer to one exact action, never a pattern or a profile edit.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Decision {
    AllowOnce,
    AllowForSession,
    /// Chosen on purpose: refused, and remembered for the session -- visibly,
    /// with a way to forget it on the Sandbox sheet.
    Deny,
    /// Esc: "not now". This call is refused and nothing is remembered.
    DenyOnce,
    /// Ctrl-C: the call is cancelled the way a running call is, reported as
    /// cancelled and never remembered as a refusal.
    Cancel,
    /// A refusal that says what to do instead. The call is refused exactly
    /// as `Deny` refuses it -- remembered, never widened -- and the words
    /// travel back to the program as the refusal's rule, so the model reads
    /// them where it reads every other refusal. Empty text asks the model
    /// to propose another way itself.
    Redirect(String),
    /// Let the hosts the proxy refused through for the rest of the session,
    /// and run the command again inside the sandbox rather than outside it.
    /// Offered only on a request that carries refused hosts.
    AllowHostSession,
    /// The same, and the person's global `sandbox.hosts` keeps the hosts for
    /// every later session. The terminal saves the setting; the gate treats
    /// it exactly as [`Decision::AllowHostSession`].
    AllowHostAlways,
}

/// What the gate answered for one call.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Admission {
    Allowed,
    /// Refused now: by the person.
    Denied,
    /// Refused because the level asks and nobody is at the terminal to
    /// answer.
    NobodyToAsk,
    /// Refused from memory: the person denied this exact call earlier in
    /// the session.
    DeniedEarlier,
    /// Cancelled by the person or stopped while it waited.
    Cancelled,
    /// The person allowed the hosts the proxy refused: the call runs, and
    /// **inside** the sandbox, even though it asked to leave it.
    HostsAllowed,
}

impl Admission {
    /// Whether the call runs at all -- confined or not.
    #[must_use]
    pub fn allowed(&self) -> bool {
        matches!(self, Self::Allowed | Self::HostsAllowed)
    }
}

/// What the gate knows of the network proxy: the live list it checks, and
/// the hosts it refused since the gate last looked.
///
/// **The refusals are taken at every command.** A command admitted takes
/// what the proxy refused before it, so the hosts a request carries are the
/// ones refused since the command before it began -- the command that
/// failed and is now asking to leave the sandbox -- and never a host some
/// command refused an hour ago.
#[derive(Clone)]
pub struct Hosts {
    allowed: crate::sandbox::proxy::Allowed,
    refused: Arc<dyn Fn() -> Vec<String> + Send + Sync>,
}

impl Hosts {
    /// `refused` returns and clears the hosts refused so far
    /// ([`crate::sandbox::proxy::Proxy::take_refused`] in a session).
    pub fn new(
        allowed: crate::sandbox::proxy::Allowed,
        refused: impl Fn() -> Vec<String> + Send + Sync + 'static,
    ) -> Self {
        Self {
            allowed,
            refused: Arc::new(refused),
        }
    }

    /// The refused hosts that are still refused, in the order they were.
    fn take(&self) -> Vec<String> {
        (self.refused)()
            .into_iter()
            .filter(|host| !self.allowed.permits(host))
            .collect()
    }
}

/// One call a person answered for the whole session, as the Sandbox sheet lists
/// it: allowed or denied, with a way to forget it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Remembered {
    /// The action's summary, stable for the session: what Forget names.
    pub id: String,
    /// The tool and its path or command, in the project's own terms.
    pub label: String,
    pub allowed: bool,
}

/// The gate's memory, shared with the screen so it can list what was
/// answered for the session and forget an answer.
#[derive(Clone, Default)]
pub struct Memory {
    allowed: Arc<Mutex<BTreeSet<Action>>>,
    judged: Arc<crate::permissions::Judged<Action>>,
}

impl std::fmt::Debug for Memory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Memory({} remembered)", self.entries().len())
    }
}

impl Memory {
    /// Every call answered for the session: allowed ones first.
    pub fn entries(&self) -> Vec<Remembered> {
        let mut out: Vec<Remembered> = self
            .allowed
            .lock()
            .map(|allowed| {
                allowed
                    .iter()
                    .map(|action| Remembered {
                        id: action.summary(),
                        label: action.label(),
                        allowed: true,
                    })
                    .collect()
            })
            .unwrap_or_default();
        out.extend(
            self.judged
                .entries()
                .into_iter()
                .filter(|(_, answer)| !answer)
                .map(|(action, _)| Remembered {
                    id: action.summary(),
                    label: action.label(),
                    allowed: false,
                }),
        );
        out
    }
    /// Forgets the session's answer for the action with this summary: the
    /// next identical call asks again.
    pub fn forget(&self, id: &str) {
        if let Ok(mut allowed) = self.allowed.lock() {
            allowed.retain(|action| action.summary() != id);
        }
        self.judged.forget_where(|action| action.summary() == id);
    }
}

/// The decision model's answer to "does this call fit the request" (F4,
/// `decision-model.md`) -- informational only. It never changes [`Decision`],
/// and it arrives on its own thread, after the confirmation is already shown.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Hint {
    pub fits: f64,
    pub asked_ms: u64,
}

/// The `[decisions]` model and mode, attached to a [`Gate`] once at session
/// start (`with_decisions`). Shared by every clone of that gate, including
/// the per-task clone a `Runtime` holds, so the counts below are session-wide.
#[derive(Clone)]
struct Decisions {
    model: String,
    mode: crate::config::DecisionMode,
    /// This gate's cumulative count of approval hints that answered.
    asked: Arc<AtomicU32>,
    /// This gate's cumulative count of approval-hint requests that failed
    /// or timed out.
    failed: Arc<AtomicU32>,
}

/// A concrete canonical call. Full argument values are available only by an
/// explicit accessor for the confirmation surface; Debug and remembered-action
/// summaries omit them because command lines and file contents can be secrets.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Action {
    tool: String,
    root: String,
    arguments: CheckedArgs,
}

impl Action {
    pub(crate) fn new(tool: &str, root: &std::path::Path, arguments: CheckedArgs) -> Self {
        Self {
            tool: tool.into(),
            root: root.to_string_lossy().into_owned(),
            arguments,
        }
    }

    pub fn tool(&self) -> &str {
        &self.tool
    }

    pub fn root(&self) -> &str {
        &self.root
    }

    pub fn arguments(&self) -> &CheckedArgs {
        &self.arguments
    }

    /// A non-secret identifier for a session permissions view. Decisions are
    /// matched by the full action value, not by this short display hash.
    pub fn summary(&self) -> String {
        let bytes = serde_json::to_vec(&(&self.tool, &self.root, &self.arguments))
            .expect("canonical actions contain only strings");
        let hash = Sha256::digest(bytes);
        let hex = format!("{hash:x}");
        format!("{} · exact action {}", self.tool, &hex[..12])
    }

    /// A bounded, terminal-safe description. Approval is disabled when the
    /// complete action does not fit the display budget.
    pub fn confirmation(&self) -> Confirmation {
        Confirmation::new(&self.tool, &self.root, &self.arguments)
    }

    /// The tool and what it acts on, in the project's own terms.
    pub fn label(&self) -> String {
        let target = self.target();
        if target.is_empty() {
            self.tool.clone()
        } else {
            format!("{} {target}", self.tool)
        }
    }

    /// What the call acts on without the tool's name: a path relative to
    /// the project, or the start of a command line. A refusal quotes this
    /// as `bash("ls -la")`, so it must not repeat the tool.
    pub fn target(&self) -> String {
        let relative = |path: &str| {
            path.strip_prefix(&self.root)
                .map(|rest| rest.trim_start_matches(['/', '\\']))
                .filter(|rest| !rest.is_empty())
                .unwrap_or(path)
                .to_string()
        };
        match (self.arguments.get("path"), self.arguments.get("command")) {
            (Some(path), _) => relative(path),
            (_, Some(command)) => {
                let line = command.lines().next().unwrap_or_default();
                let short: String = line.chars().take(60).collect();
                let more = if short.len() < command.len() {
                    "…"
                } else {
                    ""
                };
                format!("{short}{more}")
            }
            _ => String::new(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct Confirmation {
    pub text: String,
    pub complete: bool,
}

impl Confirmation {
    pub fn new(tool: &str, root: &str, arguments: &CheckedArgs) -> Self {
        let text = serde_json::to_string_pretty(&serde_json::json!({
            "tool": tool, "workspace": root, "arguments": arguments,
        }))
        .expect("checked arguments contain strings");
        // JSON escapes ASCII controls; escape Unicode formatting controls as
        // well so bidirectional text cannot reorder the confirmation surface.
        let escaped: String = text.chars().flat_map(|c| {
            if c != '\n' && (c.is_control() || matches!(c, '\u{061c}' | '\u{200e}' | '\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')) {
                c.escape_unicode().collect::<Vec<_>>()
            } else { vec![c] }
        }).collect();
        let complete = escaped.len() <= 16 * 1024;
        let text = if complete {
            escaped
        } else {
            "Action exceeds the 16 KiB confirmation limit. Approval is disabled; deny this call and ask for a smaller action.".into()
        };
        Self { text, complete }
    }
}

impl std::fmt::Debug for Action {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.summary())
    }
}

/// The reply sender belongs to this request and is consumed by `respond`.
/// A late reply or a dropped request cannot approve another call.
pub struct Request {
    action: Action,
    reply: mpsc::SyncSender<Decision>,
    pending: Arc<AtomicBool>,
    /// The approval hint, once the decision model has answered. Populated by
    /// a background thread `Gate::admit` spawns; `None` before it answers,
    /// on failure, or when no decision model is configured.
    hint: Arc<Mutex<Option<Hint>>>,
    /// Whether [`Self::hint_line`] may surface the hint at all -- `mode =
    /// shadow` still fills [`Self::hint`] above, but never this gate.
    show_hint: bool,
    /// Why the rung put this call to a person, in its own words.
    reason: Option<String>,
    /// The hosts the proxy refused before this call asked to leave the
    /// sandbox; empty when it refused none or the call does not leave.
    hosts: Vec<String>,
}

impl Request {
    pub fn is_pending(&self) -> bool {
        self.pending.load(Ordering::SeqCst)
    }
    pub fn action(&self) -> &Action {
        &self.action
    }

    /// The recorded hint regardless of `mode` -- telemetry and tests read
    /// this; the confirmation surface reads [`Self::hint_line`] instead.
    pub fn hint(&self) -> Option<Hint> {
        *self
            .hint
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// The hint the confirmation surface may show: `None` before an answer
    /// arrives, on failure, or with `mode = shadow` (recorded, never shown).
    pub fn hint_line(&self) -> Option<Hint> {
        if self.show_hint { self.hint() } else { None }
    }

    /// Why this call needs a person, when the rung said.
    pub fn reason(&self) -> Option<&str> {
        self.reason.as_deref()
    }

    /// The hosts that were refused, which the person can allow instead of
    /// letting the whole command out.
    pub fn hosts(&self) -> &[String] {
        &self.hosts
    }

    /// Whether this call asks to run outside the sandbox.
    pub fn leaves_sandbox(&self) -> bool {
        crate::permissions::outside_reason(self.action.tool(), self.action.arguments()).is_some()
    }

    /// Returns false when the waiting callback has ended. A queued reply may
    /// still be denied if cancellation is observed before it is consumed.
    pub fn respond(self, decision: Decision) -> bool {
        self.is_pending() && self.reply.send(decision).is_ok()
    }
}

struct Pending(Arc<AtomicBool>);
impl Drop for Pending {
    fn drop(&mut self) {
        self.0.store(false, Ordering::SeqCst);
    }
}

/// A session-scoped host channel. Clones share exact session decisions, but
/// the gate is never inherited by `agent.run` or background jobs.
#[derive(Clone)]
pub struct Gate {
    requests: mpsc::Sender<Request>,
    remembered: Arc<Mutex<BTreeSet<Action>>>,
    /// Refusals a person gave, remembered for the session.
    ///
    /// **Only answers that could have been given differently are kept.** The
    /// level's own verdict is recomputed on every call, because remembering
    /// it would make a person who moves to a stricter level mid-task keep
    /// the freedom of the level they left.
    judged: Arc<crate::permissions::Judged<Action>>,
    /// Which level this session is on, live: the settings sheet changes it
    /// from the UI thread while a task runs.
    level: crate::permissions::LiveLevel,
    /// The person's own `Bash(...)` patterns in `permissions.allow`: command
    /// lines that run without asking on `Ask`.
    pre_approved: Arc<Vec<String>>,
    wait_clock: Option<Arc<WaitClock>>,
    decisions: Option<Decisions>,
    /// The current task's request text, attached to the clone a `Runtime`
    /// holds for one task (`with_task`) -- `Gate` itself is session-scoped
    /// and outlives any one task.
    task: Option<String>,
    /// What the person asked for instead, keyed by the exact action they
    /// refused; taken once by the refusal that carries it to the program.
    redirects: Arc<Mutex<std::collections::BTreeMap<Action, String>>>,
    /// The proxy's allowed list and refusals, when the session runs one.
    hosts: Option<Hosts>,
}

impl Gate {
    pub fn channel(level: crate::permissions::LiveLevel) -> (Self, mpsc::Receiver<Request>) {
        let (requests, receiver) = mpsc::channel();
        (
            Self {
                requests,
                remembered: Arc::new(Mutex::new(BTreeSet::new())),
                judged: Arc::new(crate::permissions::Judged::default()),
                level,
                pre_approved: Arc::new(Vec::new()),
                wait_clock: None,
                decisions: None,
                task: None,
                redirects: Arc::new(Mutex::new(std::collections::BTreeMap::new())),
                hosts: None,
            },
            receiver,
        )
    }

    /// The words a person attached to refusing `action`, if they did --
    /// taken, so a later refusal of the same action says only that it was
    /// refused.
    pub fn redirect_for(&self, action: &Action) -> Option<String> {
        self.redirects
            .lock()
            .ok()
            .and_then(|mut redirects| redirects.remove(action))
    }

    /// Attaches the session's proxy once at session start, so a command
    /// that failed on a refused host can ask for that host.
    #[must_use]
    pub fn with_hosts(mut self, hosts: Hosts) -> Self {
        self.hosts = Some(hosts);
        self
    }

    /// The level this gate is judging on, shared with whatever changes it.
    pub fn level(&self) -> &crate::permissions::LiveLevel {
        &self.level
    }

    /// Attaches the person's own `Bash(...)` patterns once at session start.
    #[must_use]
    pub fn with_pre_approved(mut self, patterns: Vec<String>) -> Self {
        self.pre_approved = Arc::new(patterns);
        self
    }

    /// Attaches the `[decisions]` model and mode once, at session start
    /// (`decision-model.md`). `None` leaves every clone of this gate exactly
    /// as it is today: no thread, no request.
    pub(crate) fn with_decisions(
        mut self,
        model: Option<String>,
        mode: crate::config::DecisionMode,
    ) -> Self {
        self.decisions = model.map(|model| Decisions {
            model,
            mode,
            asked: Arc::new(AtomicU32::new(0)),
            failed: Arc::new(AtomicU32::new(0)),
        });
        self
    }

    /// Attaches the current task's request text to the clone a `Runtime`
    /// holds for one task -- the approval hint's one `noul` question asks
    /// whether a call fits this text.
    pub(crate) fn with_task(mut self, task: String) -> Self {
        self.task = Some(task);
        self
    }

    /// This gate's cumulative approval-hint counts: `(answered, failed)`,
    /// `(0, 0)` when no decision model is configured.
    pub(crate) fn hint_counts(&self) -> (u32, u32) {
        self.decisions.as_ref().map_or((0, 0), |decisions| {
            (
                decisions.asked.load(Ordering::Relaxed),
                decisions.failed.load(Ordering::Relaxed),
            )
        })
    }

    /// Attaches the clock the cell's watchdog subtracts, so a confirmation
    /// the person is still reading does not spend the cell's compute budget.
    /// The runtime passes **its own** clock, so an approval wait and a
    /// granted child process accrue into one span rather than two the
    /// watchdog would have to add up.
    pub(crate) fn with_wait_clock(mut self, clock: Arc<WaitClock>) -> Self {
        self.wait_clock = Some(clock);
        self
    }

    /// The memory the screen lists and forgets from.
    pub fn memory(&self) -> Memory {
        Memory {
            allowed: self.remembered.clone(),
            judged: self.judged.clone(),
        }
    }

    /// Values that can be displayed without leaking command arguments,
    /// file contents, or MCP parameters. This seam has no persisted state.
    pub fn session_actions(&self) -> Vec<String> {
        self.remembered
            .lock()
            .map(|actions| actions.iter().map(Action::summary).collect())
            .unwrap_or_default()
    }

    /// Whether this already-admitted call may run, asking the person when
    /// the level says to.
    ///
    /// The order is the whole policy: an answer already given stands, then
    /// the level judges, and only a judgement of *ask* reaches a person. A
    /// question nobody is there to answer is a refusal, never a silent yes.
    pub(crate) fn admit(&self, action: Action, stopped: impl Fn() -> bool) -> Admission {
        let unless_stopped = |admission: Admission| {
            if stopped() {
                Admission::Cancelled
            } else {
                admission
            }
        };
        if stopped() {
            return Admission::Cancelled;
        }
        // Every command takes the refusals before it, answered or not; see
        // [`Hosts`]. Unattended, nobody could allow one, and the run's end
        // names them instead (`startup::refused_hosts`).
        let refused = match &self.hosts {
            Some(hosts) if action.tool() == "bash" && !self.level.is_unattended() => hosts.take(),
            _ => Vec::new(),
        };
        if let Some(answer) = self.judged.answer(&action) {
            return unless_stopped(if answer {
                Admission::Allowed
            } else {
                Admission::DeniedEarlier
            });
        }
        let Ok(remembered) = self.remembered.lock() else {
            return Admission::Denied;
        };
        if remembered.contains(&action) {
            return unless_stopped(Admission::Allowed);
        }
        drop(remembered);
        let effectful = crate::tools::registry::lookup(action.tool())
            .is_none_or(|tool| tool.purity() == crate::tools::registry::Purity::Effectful);
        let pre_approved =
            |line: &str| crate::sandbox::profile::names_every_segment(line, &self.pre_approved);
        let leaves =
            crate::permissions::outside_reason(action.tool(), action.arguments()).is_some();
        let reason = match crate::permissions::judge(
            self.level.level(),
            action.tool(),
            action.arguments(),
            effectful,
            &pre_approved,
        ) {
            crate::permissions::Verdict::Runs => return unless_stopped(Admission::Allowed),
            crate::permissions::Verdict::Ask(why) => {
                if self.level.is_unattended() {
                    return unless_stopped(Admission::NobodyToAsk);
                }
                why
            }
        };
        let _waiting = self.wait_clock.as_ref().map(WaitClock::pause);
        let waiting_started = Instant::now();
        let pending = Arc::new(AtomicBool::new(true));
        let _pending = Pending(pending.clone());
        let (reply, response) = mpsc::sync_channel(1);
        let hint = Arc::new(Mutex::new(None));
        let show_hint = self
            .decisions
            .as_ref()
            .is_some_and(|decisions| decisions.mode == crate::config::DecisionMode::On);
        if self
            .requests
            .send(Request {
                action: action.clone(),
                reply,
                pending,
                hint: hint.clone(),
                show_hint,
                reason: Some(reason),
                hosts: if leaves { refused.clone() } else { Vec::new() },
            })
            .is_err()
        {
            return Admission::Denied;
        }
        // The confirmation above is already sent to the human; this thread
        // never delays it. Human approval waits up to ten minutes (line 23),
        // and the decision model answers in ~0.5-1s (phase-66.md's Provider
        // facts) or times out at its own two-second bound (`decide.rs`), so
        // the hint is almost always ready before anyone reads the prompt.
        if let Some(decisions) = self.decisions.clone()
            && decisions.mode != crate::config::DecisionMode::Off
        {
            let task = self.task.clone().unwrap_or_default();
            let tool = action.tool().to_string();
            // What the call is, in words: a hash would tell the decision
            // model nothing about whether it fits the request.
            let summary = action.label();
            thread::spawn(move || {
                let state = serde_json::json!({
                    "request": task,
                    "tool": tool,
                    "summary": summary,
                });
                let questions = [(
                    "fits".to_string(),
                    crate::decide::Question::Noul {
                        instructions: "The tool call fits what the request asked for \
                                       and does nothing beyond it."
                            .to_string(),
                    },
                )];
                match crate::decide::decide(&decisions.model, state, &questions) {
                    Ok(answers) => {
                        let answer = answers
                            .decisions
                            .into_iter()
                            .find(|decision| decision.key == "fits");
                        match answer {
                            Some(crate::decide::Decision {
                                answer: crate::decide::Answer::Noul(fits),
                                latency_ms,
                                ..
                            }) => {
                                decisions.asked.fetch_add(1, Ordering::Relaxed);
                                if let Ok(mut slot) = hint.lock() {
                                    *slot = Some(Hint {
                                        fits,
                                        asked_ms: latency_ms,
                                    });
                                }
                            }
                            _ => {
                                decisions.failed.fetch_add(1, Ordering::Relaxed);
                            }
                        }
                    }
                    Err(_) => {
                        decisions.failed.fetch_add(1, Ordering::Relaxed);
                    }
                }
            });
        }
        loop {
            if stopped() {
                return Admission::Cancelled;
            }
            if waiting_started.elapsed() >= MAX_APPROVAL_WAIT {
                return Admission::Denied;
            }
            match response.recv_timeout(Duration::from_millis(20)) {
                Ok(decision) => {
                    // A response queued before a cancellation is still denied
                    // if the call has stopped before it can consume that answer.
                    if stopped() {
                        return Admission::Cancelled;
                    }
                    if waiting_started.elapsed() >= MAX_APPROVAL_WAIT {
                        return Admission::Denied;
                    }
                    return match decision {
                        // Once is once: not remembered, because the person
                        // said so.
                        Decision::AllowOnce => Admission::Allowed,
                        Decision::AllowForSession => {
                            let Ok(mut remembered) = self.remembered.lock() else {
                                return Admission::Denied;
                            };
                            remembered.insert(action);
                            Admission::Allowed
                        }
                        // Remembered, so that asking again cannot turn a no
                        // into a yes: a gate that answers differently on a
                        // retry teaches retrying. It is listed on the Ask
                        // sheet, where the person can forget it.
                        Decision::Deny => {
                            self.judged.remember(action, false);
                            Admission::Denied
                        }
                        // Esc is "not now": this call only.
                        Decision::DenyOnce => Admission::Denied,
                        Decision::Cancel => Admission::Cancelled,
                        // Refused exactly as a denial is, and the words wait
                        // for the refusal that reports it (`redirect_for`).
                        Decision::Redirect(text) => {
                            if let Ok(mut redirects) = self.redirects.lock() {
                                redirects.insert(action.clone(), text);
                            }
                            self.judged.remember(action, false);
                            Admission::Denied
                        }
                        // Only the hosts the person was shown, and only
                        // while there were some: an answer cannot allow
                        // what the request did not name.
                        Decision::AllowHostSession | Decision::AllowHostAlways => {
                            match &self.hosts {
                                Some(hosts) if leaves && !refused.is_empty() => {
                                    for host in &refused {
                                        hosts.allowed.add(host);
                                    }
                                    Admission::HostsAllowed
                                }
                                _ => Admission::Denied,
                            }
                        }
                    };
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => return Admission::Denied,
                Err(mpsc::RecvTimeoutError::Timeout) => {}
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::permissions::{Level, LiveLevel, OUTSIDE};

    fn bash(line: &str) -> Action {
        let mut arguments = std::collections::BTreeMap::new();
        arguments.insert("command".to_string(), line.to_string());
        Action::new("bash", std::path::Path::new("/tmp/root"), arguments)
    }

    fn outside(line: &str, why: &str) -> Action {
        let mut arguments = std::collections::BTreeMap::new();
        arguments.insert("command".to_string(), line.to_string());
        arguments.insert(OUTSIDE.to_string(), why.to_string());
        Action::new("bash", std::path::Path::new("/tmp/root"), arguments)
    }

    /// On `Sandboxed` a command runs without reaching anybody; asking to
    /// leave the sandbox is put to the person.
    #[test]
    fn sandboxed_asks_only_to_leave_the_sandbox() {
        let (gate, requests) = Gate::channel(LiveLevel::new(Level::Sandboxed));
        assert!(gate.admit(bash("cargo test"), || false).allowed());
        assert!(
            requests.try_recv().is_err(),
            "a sandboxed command asks nobody"
        );
        let asked = gate.clone();
        let asking =
            std::thread::spawn(move || asked.admit(outside("curl x", "needs x"), || false));
        let request = requests
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("leaving the sandbox must be asked");
        assert!(request.reason().is_some_and(|why| why.contains("needs x")));
        assert!(request.respond(Decision::AllowOnce));
        assert!(asking.join().unwrap().allowed());
    }

    /// A gate whose proxy has refused `hosts` once; later reads find none.
    fn with_refused(gate: Gate, hosts: &[&str]) -> (Gate, crate::sandbox::proxy::Allowed) {
        let allowed = crate::sandbox::proxy::Allowed::new(&[], &[]);
        let pending = Arc::new(Mutex::new(
            hosts.iter().map(|h| h.to_string()).collect::<Vec<_>>(),
        ));
        let gate = gate.with_hosts(Hosts::new(allowed.clone(), move || {
            std::mem::take(&mut *pending.lock().unwrap())
        }));
        (gate, allowed)
    }

    /// The refused hosts ride on a request to leave the sandbox, and
    /// allowing them runs the call confined; the host is then allowed.
    #[test]
    fn a_refused_host_is_offered_and_allowing_it_keeps_the_call_confined() {
        let (gate, requests) = Gate::channel(LiveLevel::new(Level::Sandboxed));
        let (gate, allowed) = with_refused(gate, &["api.example.com"]);
        let asked = gate.clone();
        let asking =
            std::thread::spawn(move || asked.admit(outside("curl api", "needs the api"), || false));
        let request = requests
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("leaving the sandbox must be asked");
        assert_eq!(request.hosts(), ["api.example.com".to_string()]);
        assert!(request.leaves_sandbox());
        assert!(request.respond(Decision::AllowHostSession));
        assert_eq!(asking.join().unwrap(), Admission::HostsAllowed);
        assert!(allowed.permits("api.example.com"));
    }

    /// **The refusals belong to the command after them.** A command that
    /// does not leave the sandbox takes them too, so a later request to
    /// leave carries only what was refused since.
    #[test]
    fn every_command_takes_the_refusals_before_it() {
        let (gate, requests) = Gate::channel(LiveLevel::new(Level::Sandboxed));
        let (gate, allowed) = with_refused(gate, &["old.example.com"]);
        assert!(gate.admit(bash("cargo build"), || false).allowed());
        let asked = gate.clone();
        let asking =
            std::thread::spawn(move || asked.admit(outside("curl api", "needs the api"), || false));
        let request = requests
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("leaving the sandbox must be asked");
        assert!(request.hosts().is_empty(), "{:?}", request.hosts());
        // With no host on the request, allowing a host allows nothing.
        assert!(request.respond(Decision::AllowHostSession));
        assert_eq!(asking.join().unwrap(), Admission::Denied);
        assert!(!allowed.permits("old.example.com"));
    }

    /// With nobody to ask, a question is a refusal and never a silent yes.
    #[test]
    fn nobody_to_ask_is_a_refusal() {
        let (gate, requests) = Gate::channel(LiveLevel::new(Level::Sandboxed).unattended());
        assert_eq!(
            gate.admit(outside("curl x", "needs x"), || false),
            Admission::NobodyToAsk
        );
        assert!(requests.try_recv().is_err());
        assert!(gate.admit(bash("cargo test"), || false).allowed());
    }

    /// `Ask` confirms a command unless the person's own pattern names it.
    #[test]
    fn ask_confirms_commands_the_person_did_not_pre_approve() {
        let (gate, requests) = Gate::channel(LiveLevel::new(Level::Ask));
        let gate = gate.with_pre_approved(vec!["cargo test*".to_string()]);
        assert!(gate.admit(bash("cargo test -q"), || false).allowed());
        let asked = gate.clone();
        let asking = std::thread::spawn(move || asked.admit(bash("rm -rf target"), || false));
        let request = requests
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("an unapproved command asks on Ask");
        assert!(request.respond(Decision::DenyOnce));
        assert_eq!(asking.join().unwrap(), Admission::Denied);
    }

    /// A refusal with words is a refusal: not admitted, remembered, and the
    /// words wait for the one refusal that reports them.
    #[test]
    fn a_redirect_refuses_like_a_denial_and_hands_its_words_over_once() {
        let (gate, requests) = Gate::channel(LiveLevel::new(Level::Ask));
        let asked = gate.clone();
        let asking = std::thread::spawn(move || asked.admit(bash("rm -rf /var/tmp/x"), || false));
        let request = requests
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("the person must be asked");
        assert!(request.respond(Decision::Redirect("use fd instead".into())));
        assert_eq!(asking.join().unwrap(), Admission::Denied, "refused");
        assert_eq!(
            gate.redirect_for(&bash("rm -rf /var/tmp/x")).as_deref(),
            Some("use fd instead")
        );
        assert_eq!(
            gate.redirect_for(&bash("rm -rf /var/tmp/x")),
            None,
            "taken once"
        );
        // Asking again cannot turn the no into a yes, and the refusal says
        // it was the person's own, earlier.
        assert_eq!(
            gate.admit(bash("rm -rf /var/tmp/x"), || false),
            Admission::DeniedEarlier
        );
        assert!(
            requests.try_recv().is_err(),
            "a remembered refusal asks nobody"
        );
    }
}
