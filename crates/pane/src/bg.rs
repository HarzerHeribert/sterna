//! Background jobs — `docs/product/pane/events-contract.md` §5.
//!
//! **A background job is a foreground tool call that nobody waits for.** Every
//! job runs through [`crate::tools::invoke::run_cancellable`], on a thread of
//! its own, so the grant, the argument check, the confinement, the process
//! group and the kill-and-reap are the ones a `bash()` call in a cell already
//! gets: there is no second spawn path here, no second confinement decision,
//! and no `Command` in this file. Nothing widens because it is asynchronous.
//!
//! The grant is asked **twice on purpose**, and the first ask is what §5's
//! "throws `PermissionDenied` at the call, before any handle exists" means:
//! [`run`] calls [`Profile::admits_command`] — the same function
//! `invoke::check_arguments` runs for `bash`'s command line — before it mints
//! a handle or starts a thread. The thread's own call asks again through
//! `invoke`, so a profile that refuses cannot be got past by reaching the
//! thread.
//!
//! One board per session id, in a process-wide registry. It is not a field of
//! [`crate::runtime::state::RuntimeState`] because a job outlives the cell
//! that started it and its completion has to be readable from the session's
//! turn loop, which holds no isolate; the session id keys it so two sessions
//! in one process never see each other's jobs.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crate::contract::SessionId;
use crate::events::{Event, Kind, PayloadRef, Priority, now};
use crate::sandbox::profile::{PermissionDenied, Profile};
use crate::tools::invoke::{self, Args, CancellationToken, ToolContext, ToolError};

/// How long [`cancel`] lets a job settle before it stops waiting for the
/// thread to notice — the token is already set by then, and
/// `invoke::spawn_confined`'s poll loop kills the whole process group at its
/// next tick.
const CANCEL_SETTLE: Duration = Duration::from_millis(50);

/// The slice a deadline thread sleeps in, so a timed job's timer does not
/// keep [`shutdown`] waiting for the whole timeout.
const DEADLINE_SLICE: Duration = Duration::from_millis(25);

/// The slice [`cancel`]'s settle polls the job's own `finished` flag in.
/// Finer than [`DEADLINE_SLICE`] because this one is on the exit path: a job
/// that dies in 22 ms (`invoke`'s cancel poll is 20 ms) should cost 25 ms,
/// not the whole of [`CANCEL_SETTLE`].
const SETTLE_SLICE: Duration = Duration::from_millis(5);

/// The fastest cadence `bg.watch` will accept, in milliseconds.
///
/// **A floor, not a default.** Every tick is a fresh confined `bash`, so the
/// cadence is a resource bound on model-chosen work rather than a
/// preference: measured on this machine, a watch runs 31 confined spawns a
/// second when it is asked for `{every: 1}` and 7.5 at this floor, and the
/// lifecycle verifier measured the unfloored version burning 13.20 s of CPU
/// in a 24.5 s session against 1.03 s at a second — about 54% of a core to
/// deliver, by §1's dedup, exactly the events a slow watch delivers. 100 ms
/// holds a watch's own polling near an eighth of a core, which is the most a
/// program should be able to spend on *looking* without asking for it.
///
/// It is enforced by refusing rather than by clamping, for the reason `cwd`
/// and `env` are refused: a program told nothing, and silently slowed, would
/// believe it had chosen a cadence it had not.
const WATCH_FLOOR_MS: u64 = 100;

/// How long [`shutdown`] waits for a cancelled job's thread to come back
/// before it stops waiting for it.
///
/// **A session must be able to end.** An unbounded `join` here is a session
/// that cannot exit at all, and reaching it needs only one job the kill could
/// not stop; a bounded wait leaves at worst a detached thread. This is not
/// hypothetical — the mutation that hands `cancel` a token nobody holds
/// reproduces it exactly, and hung the test that found it for twelve minutes
/// rather than failing it.
const SHUTDOWN_GRACE: Duration = Duration::from_secs(10);

/// The most characters of a job's own summary line a batch preview carries.
const SUMMARY_CHARS: usize = 160;

/// What one emission of a job produced. `status` is small enough to render;
/// `stdout` and `stderr` are not, which is the whole reason §5 makes them
/// handles of their own — a job that printed 40 MB costs a status line until
/// the model's own program asks for the bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JobResult {
    pub stdout: String,
    pub stderr: String,
    pub status: String,
}

/// What one look at a running subagent says: the turns it has taken, the
/// tools it has called in order, how long it has been working, whether it
/// still is, where its own record is being written, and whether it can still
/// be told something.
///
/// The last two are the way in: a person who wants to read a subagent needs
/// the path, and one who wants to speak to it needs to know whether anything
/// would hear it ([`tell`] is honest about a job that has already finished,
/// and this is the same answer asked in advance).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JobProgress {
    pub turns: u64,
    pub calls: Vec<String>,
    pub elapsed_ms: u64,
    pub running: bool,
    /// The subagent's own rollout file, or `None` for a job that writes none.
    pub rollout: Option<PathBuf>,
    /// Whether a message sent now would be delivered.
    pub takes_messages: bool,
}

/// What became of a message sent to a job.
///
/// **A message to a job that has finished is not an error.** `bg::cancel` is
/// idempotent for the same reason: the caller raced the work and lost, which
/// is an ordinary outcome and not a mistake it could have avoided.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Delivery {
    /// Queued for the subagent's next turn boundary.
    Queued,
    /// Nothing will read it: the job has finished, never existed, or is a
    /// command rather than a turn loop.
    Undelivered,
}

/// `bg.run`'s options object.
///
/// `cwd` and `env` are **refused rather than ignored**: `invoke` runs every
/// child in the project root with the session's own environment, and honouring
/// either here would mean a second spawn path — which is exactly what this
/// module exists not to have. A silent ignore would be worse: a program would
/// believe it had chosen a directory it had not.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RunOptions {
    pub cwd: Option<String>,
    pub env: Option<String>,
    pub timeout_ms: Option<u64>,
}

/// `bg.watch`'s options object. `until` matches when the run's stdout
/// contains it — a substring, not a pattern, because a pattern language here
/// would be a second grammar for the model to learn and §5 names none.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WatchOptions {
    pub every_ms: u64,
    pub until: Option<String>,
    pub timeout_ms: Option<u64>,
}

/// One live job: the token its call is cancellable through, whether a
/// cancellation was already asked for (so [`cancel`] is idempotent), and
/// whether the thread has finished.
struct JobEntry {
    token: CancellationToken,
    cancelled: bool,
    finished: bool,
    thread: Option<JoinHandle<()>>,
    /// When the job was put on the board, so a parent looking in is told how
    /// long its subagent has been working rather than having to time it.
    started: Instant,
    /// The other end of a running subagent's [`crate::agent::ProgressSink`],
    /// or `None` for a command job, which has no turns to report.
    progress: Option<crate::agent::ProgressSink>,
    /// Where [`tell`] leaves what a person said, or `None` for a command job,
    /// which has no turn boundary to read it at.
    inbox: Option<crate::agent::InboxSink>,
    /// Where the subagent writes its own rollout, so a look can name it.
    record: Option<crate::agent::AgentRollout>,
}

/// One session's jobs, the events they have raised and the payloads those
/// events name.
#[derive(Default)]
struct Board {
    next: u64,
    jobs: HashMap<String, JobEntry>,
    pending: Vec<Event>,
    payloads: HashMap<String, JobResult>,
}

static BOARDS: OnceLock<Mutex<HashMap<String, Board>>> = OnceLock::new();

fn boards() -> &'static Mutex<HashMap<String, Board>> {
    BOARDS.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Runs `f` against this session's board.
///
/// A poisoned mutex is recovered from rather than propagated: a panicking job
/// thread must not take the session's whole event channel with it, and every
/// field here is a plain collection with no invariant a panic could have left
/// half-applied.
fn with_board<T>(session: &SessionId, f: impl FnOnce(&mut Board) -> T) -> T {
    let mut boards = boards().lock().unwrap_or_else(|e| e.into_inner());
    f(boards.entry(session.as_str().to_string()).or_default())
}

/// One line, bounded, with no newline that could forge a second row of a
/// batch preview.
fn summary(text: &str) -> String {
    let one_line = text.lines().collect::<Vec<_>>().join(" ");
    one_line.chars().take(SUMMARY_CHARS).collect()
}

/// A short digest of one emission's output, so §1's `bg.done` dedup key
/// (`bg/<handle> + emission`) collapses two identical outputs inside one
/// window into one event — which is `bg.watch`'s whole "identical output in
/// one window is one event".
fn emission_of(stdout: &str) -> String {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(stdout.as_bytes());
    format!("{digest:x}")[..16].to_string()
}

/// Refuses the two options `invoke` gives this module no way to honour.
fn honourable(options: &RunOptions) -> Result<(), PermissionDenied> {
    let refuse = |name: &str, rule: &str| PermissionDenied {
        tool: "bg.run".to_string(),
        path: name.to_string(),
        rule: rule.to_string(),
    };
    if options.cwd.is_some() {
        return Err(refuse(
            "cwd",
            "a background job runs in the project root, the same directory a foreground tool call \
             runs in; choosing another would need a second spawn path outside the one confinement \
             (events-contract.md §5)",
        ));
    }
    if options.env.is_some() {
        return Err(refuse(
            "env",
            "a background job runs with the session's own environment; adding to it is a grant \
             question sandbox-grants.md has not been asked",
        ));
    }
    Ok(())
}

/// Refuses a watch cadence whose own polling would be the load.
///
/// [`WATCH_FLOOR_MS`] carries the measurement; this is where a program is
/// told, and the message names the floor so a program can retry at it.
fn fast_enough_to_be_free(every_ms: u64) -> Result<(), PermissionDenied> {
    if every_ms >= WATCH_FLOOR_MS {
        return Ok(());
    }
    Err(PermissionDenied {
        tool: "bg.watch".to_string(),
        path: "every".to_string(),
        rule: format!(
            "a watch runs a fresh confined shell every tick, so `every` has a floor of \
             {WATCH_FLOOR_MS} ms and {every_ms} ms is under it; ask for {WATCH_FLOOR_MS} or more \
             (events-contract.md §5)"
        ),
    })
}

/// §5's `bg.run(cmd, {cwd, env, timeout})` — answers with the job's handle
/// **before the process has done anything**.
///
/// The refusal is the profile's own and it happens here, at the call: a
/// command outside the grant leaves no job on the board, no thread running
/// and no handle for the model to hold.
pub fn run(
    profile: &Profile,
    session: &SessionId,
    command: &str,
    options: &RunOptions,
) -> Result<String, PermissionDenied> {
    honourable(options)?;
    profile.admits_command(command)?;
    Ok(start(
        profile,
        session,
        Work::Command(command.to_string()),
        options.timeout_ms,
        None,
    ))
}

/// §5's `bg.watch(cmd, {every, until})`, built on [`run`]'s machinery rather
/// than a second spawn path: the same thread, the same call, in a loop.
///
/// A cadence under [`WATCH_FLOOR_MS`] is refused here, before a handle
/// exists, exactly as a command outside the grant is.
pub fn watch(
    profile: &Profile,
    session: &SessionId,
    command: &str,
    options: &WatchOptions,
) -> Result<String, PermissionDenied> {
    fast_enough_to_be_free(options.every_ms)?;
    profile.admits_command(command)?;
    Ok(start(
        profile,
        session,
        Work::Command(command.to_string()),
        options.timeout_ms,
        Some(options.clone()),
    ))
}

/// Mints the handle, registers the job and starts its thread — the one place
/// a job comes into existence, so `run` and `watch` cannot drift about what a
/// job is.
/// Phase 64's entry: start a subagent and answer with its handle at once.
///
/// It admits no command line, because a subagent runs none of its own — its
/// tools go through the same `Profile` its parent's do, and the profile is
/// cloned rather than recompiled so the two cannot drift. The refusals that
/// belong to the *caller* — a subagent starting a subagent, a budget that
/// cannot pay — are made where the caller is known, in the binding, before
/// this is reached and before a handle exists.
pub fn agent(
    profile: &Profile,
    session: &SessionId,
    task: &str,
    options: &crate::agent::AgentOptions,
) -> String {
    agent_with_config(profile, session, task, options, None)
}

pub fn agent_with_config(
    profile: &Profile,
    session: &SessionId,
    task: &str,
    options: &crate::agent::AgentOptions,
    config: Option<&crate::config::PaneConfig>,
) -> String {
    // **The wall clock is the bound, and it is armed once.** `arm_deadline`
    // is what stops a provider call that hangs, because it cancels the token
    // from outside the loop. It cancels *for the deadline*, and that is how
    // the expiry is reported as `deadline`: the loop reads the reason off the
    // token rather than measuring a second clock that started later.
    let deadline_ms = options
        .deadline
        .map(|deadline| u64::try_from(deadline.as_millis()).unwrap_or(u64::MAX));
    start(
        profile,
        session,
        Work::Agent {
            task: task.to_string(),
            options: options.clone(),
            config: config.cloned().map(Box::new),
        },
        deadline_ms,
        None,
    )
}

fn start(
    profile: &Profile,
    session: &SessionId,
    work: Work,
    timeout_ms: Option<u64>,
    watching: Option<WatchOptions>,
) -> String {
    let token = CancellationToken::new();
    // Only a turn loop has progress to report, an inbox to read or a record
    // to write. All three are minted here rather than on the job's thread, so
    // a person who looks in during the first turn reads an honest zero and
    // finds the path rather than finding nothing at all.
    let is_agent = matches!(work, Work::Agent { .. });
    let progress = is_agent.then(crate::agent::ProgressSink::default);
    let inbox = is_agent.then(crate::agent::InboxSink::default);
    let root = profile.root().to_path_buf();
    let (handle, record) = with_board(session, |board| {
        board.next += 1;
        let handle = format!("job{}", board.next);
        let record = is_agent.then(|| crate::agent::AgentRollout::for_job(&root, session, &handle));
        board.jobs.insert(
            handle.clone(),
            JobEntry {
                token: token.clone(),
                cancelled: false,
                finished: false,
                thread: None,
                started: Instant::now(),
                progress: progress.clone(),
                inbox: inbox.clone(),
                record: record.clone(),
            },
        );
        (handle, record)
    });

    let job = JobThread {
        handle: handle.clone(),
        profile: profile.clone(),
        session: session.clone(),
        work,
        token: token.clone(),
        watching,
        progress,
        inbox,
        record,
    };
    let thread = std::thread::spawn(move || job.serve());

    if let Some(ms) = timeout_ms {
        arm_deadline(session.clone(), handle.clone(), token, ms);
    }

    with_board(session, |board| {
        if let Some(entry) = board.jobs.get_mut(&handle) {
            entry.thread = Some(thread);
        }
    });
    handle
}

/// The deadline half of `{timeout}` and of `bg.watch`'s `until` that never
/// matches: a thread that sleeps in [`DEADLINE_SLICE`] slices so `shutdown`
/// is never held up by a long timeout, and that cancels the job when the
/// deadline arrives.
///
/// §5: "the deadline expiring emits a `timer`" — and the job's own thread
/// then emits the `bg.done` with `status: "cancelled"`, so nothing waits for
/// a dead result.
fn arm_deadline(session: SessionId, handle: String, token: CancellationToken, ms: u64) {
    std::thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_millis(ms);
        while Instant::now() < deadline {
            if token.is_cancelled() || finished(&session, &handle) {
                return;
            }
            std::thread::sleep(DEADLINE_SLICE);
        }
        if finished(&session, &handle) {
            return;
        }
        raise(
            &session,
            Event::pending(
                Kind::Timer {
                    deadline: ms.to_string(),
                },
                format!("bg/{handle}"),
                now(),
                PayloadRef::new(format!("{handle}#timer")),
                Priority::Batch,
                summary(&format!("{handle} reached its {ms} ms deadline")),
            ),
        );
        token.cancel_for_deadline();
    });
}

fn finished(session: &SessionId, handle: &str) -> bool {
    with_board(session, |board| {
        board.jobs.get(handle).is_none_or(|job| job.finished)
    })
}

/// Puts one event on the session's board for the turn loop to drain.
fn raise(session: &SessionId, event: Event) {
    with_board(session, |board| board.pending.push(event));
}

/// Everything one job's thread needs, owned — a thread cannot borrow the
/// session's profile, and cloning it is what `RuntimeState` already does.
/// Cloning cannot widen anything: [`Profile`] has no method that mutates it
/// and no constructor outside `Profile::compile`.
/// What a job actually does.
///
/// **A subagent is a job whose work is a turn loop rather than a spawned
/// command** (Phase 64). It is a variant here and not a second system because
/// everything around the work — the board, the cancellation token, the
/// deadline, the payload store, the emission, the batch and its dedup — is
/// identical for both, and a parallel path would have had to reproduce all of
/// it to gain nothing.
enum Work {
    Command(String),
    Agent {
        task: String,
        options: crate::agent::AgentOptions,
        config: Option<Box<crate::config::PaneConfig>>,
    },
}

impl Work {
    /// How the completion line names this job.
    fn summary_subject(&self) -> String {
        match self {
            Work::Command(command) => command.clone(),
            Work::Agent { task, .. } => format!("subagent: {task}"),
        }
    }
}

struct JobThread {
    handle: String,
    profile: Profile,
    session: SessionId,
    work: Work,
    token: CancellationToken,
    watching: Option<WatchOptions>,
    progress: Option<crate::agent::ProgressSink>,
    inbox: Option<crate::agent::InboxSink>,
    record: Option<crate::agent::AgentRollout>,
}

/// How many trajectory entries one note carries before it counts the rest.
///
/// A subagent's trajectory is one entry per tool call, and nothing caps its
/// turns, so a long loop leaves hundreds. The first entries are the ones that
/// say what it was doing; the tail is a number.
const NOTED_TRAJECTORY: usize = 24;

/// The line a subagent's `stderr` carries, or empty when it returned normally
/// with an answer.
///
/// **Nothing here is a second shape.** It is prose for the parent to read,
/// built only from what `AgentResult` already holds: the status, the turns
/// taken, and the tool names of the trajectory (never an argument, never a
/// payload). `stdout` still carries the subagent's own last words, so this
/// says why it stopped and never replaces what it produced.
fn note_for(answered: &crate::agent::AgentResult) -> String {
    if answered.status == "returned" && !answered.answer.is_empty() {
        return String::new();
    }
    let stopped = match answered.status.as_str() {
        "deadline" => "ran out of time",
        "turns" => "reached the turn hint it was given",
        "cancelled" => "was cancelled",
        "failed" => "failed",
        other => other,
    };
    let mut note = format!("subagent {stopped} after {} turn(s)", answered.turns);
    if answered.trajectory.is_empty() {
        note.push_str("; it made no tool call");
    } else {
        let shown: Vec<&str> = answered
            .trajectory
            .iter()
            .take(NOTED_TRAJECTORY)
            .map(String::as_str)
            .collect();
        note.push_str("; it called: ");
        note.push_str(&shown.join(", "));
        let rest = answered.trajectory.len().saturating_sub(shown.len());
        if rest > 0 {
            note.push_str(&format!(", and {rest} more"));
        }
    }
    note.push('\n');
    note
}

impl JobThread {
    fn serve(self) {
        match &self.watching {
            None => self.serve_once(),
            Some(options) => self.serve_watch(options),
        }
        with_board(&self.session, |board| {
            if let Some(entry) = board.jobs.get_mut(&self.handle) {
                entry.finished = true;
            }
        });
    }

    /// One call through `invoke`, then one `bg.done`. A job completes once,
    /// so the emission is the constant §1's dedup table needs to make that
    /// true.
    fn serve_once(&self) {
        match &self.work {
            Work::Command(_) => {
                let result = self.call();
                self.emit("exit", result);
            }
            Work::Agent {
                task,
                options,
                config,
            } => {
                let answered = crate::agent::run_watched(
                    &self.profile,
                    &self.session,
                    task,
                    options,
                    &self.token,
                    config.as_deref(),
                    crate::agent::Watch {
                        progress: self.progress.as_ref(),
                        inbox: self.inbox.as_ref(),
                        record: self.record.as_ref(),
                    },
                );
                // The answer is the job's output, so a subagent's result is
                // read exactly as a command's is — `stdout`, `stderr`,
                // `status` — and nothing downstream learns a second shape.
                //
                // **A subagent that stopped without returning still says what
                // it did.** `stderr` is where a command's diagnostics go, so
                // it is where these belong: `stdout` stays the answer and
                // nothing that prints it changes. Measured 2026-09-17
                // (session `tlitep-13fv`): three subagents came back
                // `{status: "cancelled", stdout: "", stderr: ""}`, the parent
                // could not tell an exhausted turn budget from a refusal, and
                // it spent about twenty cells starting the same doomed
                // subagent again.
                let status = answered.status.clone();
                let stderr = note_for(&answered);
                self.emit(
                    &status,
                    Ok(JobResult {
                        stdout: answered.answer,
                        stderr,
                        status: answered.status,
                    }),
                );
            }
        }
    }

    /// §5's watch: `cmd` every `every` ms, one `bg.done` per match, until
    /// `until` matches or the job is cancelled.
    ///
    /// A match is a run that produced output — a still-running tick raises
    /// nothing, which is §1's "polling raises no event, only a transition
    /// does". Two identical outputs inside one window collapse to one event
    /// by the emission digest, without this loop knowing anything about
    /// windows.
    fn serve_watch(&self, options: &WatchOptions) {
        // Unreachable by construction -- [`watch`] refuses anything under the
        // floor before a job exists -- so this is what makes the floor a
        // property of the loop rather than only of its one caller.
        let every = Duration::from_millis(options.every_ms.max(WATCH_FLOOR_MS));
        loop {
            if self.token.is_cancelled() {
                self.emit("cancelled", Err(cancelled()));
                return;
            }
            let result = self.call();
            let matched = match &result {
                Ok(job) => !job.stdout.trim().is_empty(),
                Err(_) => true,
            };
            let stop = match (&result, &options.until) {
                (Ok(job), Some(until)) => job.stdout.contains(until.as_str()),
                (Err(_), _) => true,
                (Ok(_), None) => false,
            };
            if matched {
                let emission = match &result {
                    Ok(job) => emission_of(&job.stdout),
                    Err(_) => "cancelled".to_string(),
                };
                self.emit(&emission, result);
            }
            if stop {
                return;
            }
            // Slept in slices so a cancellation is noticed within one slice
            // rather than within `every`, which a model may have set to
            // minutes.
            let until_next = Instant::now() + every;
            while Instant::now() < until_next {
                if self.token.is_cancelled() {
                    self.emit("cancelled", Err(cancelled()));
                    return;
                }
                std::thread::sleep(DEADLINE_SLICE.min(every));
            }
        }
    }

    /// The one call this module makes, and the only path by which a job
    /// reaches a process: `invoke`'s own `bash`, checked, confined, spawned
    /// as its own process group leader and killed as one.
    fn call(&self) -> Result<JobResult, ToolError> {
        let context = ToolContext {
            profile: &self.profile,
            session: &self.session,
        };
        let Work::Command(command) = &self.work else {
            // Unreachable: `serve_once` sends an agent down the other arm and
            // a watch is refused for one at `agent`, so nothing reaches here
            // with anything but a command line.
            return Err(ToolError::Cancelled {
                tool: "bash".to_string(),
            });
        };
        let args = Args::new().with("command", command.clone());
        invoke::run_cancellable(&context, &self.token, "bash", &args).map(|result| JobResult {
            stdout: result.stdout,
            stderr: result.stderr,
            status: result
                .exit_code
                .map_or_else(|| "signal".to_string(), |code| code.to_string()),
        })
    }

    /// Records one emission's payload and raises its `bg.done`.
    ///
    /// A cancelled or timed-out job emits one too, with `status:
    /// "cancelled"` — §5's "nothing waits for a dead result".
    fn emit(&self, emission: &str, result: Result<JobResult, ToolError>) {
        let job = match result {
            Ok(job) => job,
            Err(ToolError::Cancelled { .. }) => JobResult {
                stdout: String::new(),
                stderr: String::new(),
                status: "cancelled".to_string(),
            },
            Err(other) => JobResult {
                stdout: String::new(),
                stderr: other.to_string(),
                status: "failed".to_string(),
            },
        };
        let payload = format!("{}#{emission}", self.handle);
        let line = summary(&format!(
            "{} → {} ({} B out)",
            self.work.summary_subject(),
            job.status,
            job.stdout.len()
        ));
        with_board(&self.session, |board| {
            board.payloads.insert(payload.clone(), job);
            board.pending.push(Event::pending(
                match &self.work {
                    Work::Command(_) => Kind::BgDone {
                        emission: emission.to_string(),
                    },
                    Work::Agent { .. } => Kind::AgentDone {
                        emission: emission.to_string(),
                    },
                },
                match &self.work {
                    Work::Command(_) => format!("bg/{}", self.handle),
                    Work::Agent { .. } => format!("agent/{}", self.handle),
                },
                now(),
                PayloadRef::new(payload.clone()),
                Priority::Batch,
                line.clone(),
            ));
        });
    }
}

fn cancelled() -> ToolError {
    ToolError::Cancelled {
        tool: "bash".to_string(),
    }
}

/// §5's `bg.cancel(handle)`: idempotent, and it stops everything the job
/// started.
///
/// Setting the token is the whole mechanism, and it is deliberately the only
/// one: `invoke::spawn_confined`'s poll loop reads it and calls
/// `kill_and_reap`, which signals the **process group** the call created
/// before it reaps the child — so a job whose command backgrounded something
/// leaves nothing spinning, and a job that ignores a polite signal dies
/// anyway.
pub fn cancel(session: &SessionId, handle: &str) {
    cancel_by(session, handle, Instant::now() + CANCEL_SETTLE);
}

/// [`cancel`], with the settle's own end named by the caller so a shutdown
/// pays one grace for a whole board rather than one per job.
///
/// **The settle waits for the job rather than for the clock.** It is there so
/// `invoke::spawn_confined`'s poll loop reaches its next tick, and a job that
/// has already finished has no tick left to reach: sleeping through
/// [`CANCEL_SETTLE`] for it charged every caller 50 ms for work that was over
/// before it was asked for. `shutdown` calls this for every handle on the
/// board, so the bill was 50 ms × every job the session ever started, paid at
/// the end of every task with the user watching — 12.34 s for 200 short jobs,
/// measured against the shipped binary. A model's own `bg.cancel` paid it
/// too.
fn cancel_by(session: &SessionId, handle: &str, deadline: Instant) {
    let token = with_board(session, |board| {
        let entry = board.jobs.get_mut(handle)?;
        if entry.cancelled {
            return None;
        }
        entry.cancelled = true;
        Some(entry.token.clone())
    });
    let Some(token) = token else { return };
    token.cancel();
    while !finished(session, handle) && Instant::now() < deadline {
        std::thread::sleep(SETTLE_SLICE);
    }
}

/// Every event raised since the last drain, oldest first.
pub fn drain(session: &SessionId) -> Vec<Event> {
    with_board(session, |board| std::mem::take(&mut board.pending))
}

/// One emission's output, by the `PayloadRef` its event carries — §1's
/// "materialised on first access".
pub fn payload(session: &SessionId, id: &str) -> Option<JobResult> {
    with_board(session, |board| board.payloads.get(id).cloned())
}

/// What a running subagent has done so far, or `None` for a handle that is
/// not a subagent's.
///
/// **A look, not a wait.** It reads the board and the sink and returns; it
/// makes no provider request, starts nothing, and cannot block on the job's
/// thread. The user, 2026-09-17: *"A parent model should check on a subagent
/// from some time. But even Claude Code does not do that."*
pub fn progress(session: &SessionId, handle: &str) -> Option<JobProgress> {
    with_board(session, |board| {
        let job = board.jobs.get(handle)?;
        let sink = job.progress.as_ref()?;
        let held = sink.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        Some(JobProgress {
            turns: held.turns,
            calls: held.calls.clone(),
            elapsed_ms: u64::try_from(job.started.elapsed().as_millis()).unwrap_or(u64::MAX),
            running: !job.finished,
            rollout: job.record.as_ref().map(|record| record.path.clone()),
            takes_messages: !job.finished && job.inbox.is_some(),
        })
    })
}

/// Says `text` to a running subagent, for its next turn boundary.
///
/// **It queues; it does not interrupt.** A subagent inside a provider call
/// finishes that call first: cutting a request short spends the tokens and
/// throws the answer away, and the message is worth more delivered to a
/// subagent that can see its own last result than to one that cannot.
///
/// The message becomes an ordinary user turn in the subagent's conversation
/// and is written to its rollout, so the record shows what it was told. It
/// changes nothing about what the parent receives: the parent asked a
/// question through `agent.run` and still gets that question's answer through
/// `agent.done`.
///
/// **The job is the same job.** No new handle, no second token, no restart:
/// `cancel`, `progress` and the pending `agent.done` all continue to mean
/// what they meant, which is what makes attaching to a subagent a look at the
/// work rather than a fork of it.
pub fn tell(session: &SessionId, handle: &str, text: &str) -> Result<Delivery, &'static str> {
    // The parent's own inbox bound, not a second one: a message is a message,
    // and two limits for one idea is how they drift.
    if text.is_empty() || text.len() > crate::agent::MESSAGE_BYTES {
        return Err("a message to a subagent must be 1–65536 UTF-8 bytes");
    }
    Ok(with_board(session, |board| {
        let Some(job) = board.jobs.get(handle) else {
            return Delivery::Undelivered;
        };
        if job.finished {
            return Delivery::Undelivered;
        }
        let Some(inbox) = job.inbox.as_ref() else {
            return Delivery::Undelivered;
        };
        inbox
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .push(text.to_string());
        Delivery::Queued
    }))
}

/// How many of this session's jobs have not finished.
pub fn live(session: &SessionId) -> usize {
    with_board(session, |board| {
        board.jobs.values().filter(|job| !job.finished).count()
    })
}

/// Cancels every job of this session and waits for its thread, then forgets
/// the board.
///
/// **A background job outlives no session.** This is called where
/// `session::run_task` ends a task and again where `session::run` returns,
/// and [`shutdown_within`] is called where `Interrupter::end_the_session`
/// exits from the signal watcher — so an end of input, a task that returns,
/// a task that failed mid-flight and the double Ctrl-C all reach it; every
/// job dies through [`cancel`]'s ladder, which is `invoke`'s own group kill.
pub fn shutdown(session: &SessionId) {
    shutdown_within(session, SHUTDOWN_GRACE);
}

/// [`shutdown`], bounded by the caller's own grace.
///
/// **The whole shutdown is bounded by `grace`, not each job by its own.** The
/// exit that needs this is the second Ctrl-C, which is a person asking the
/// program to stop *now*: it passes its own reap grace, and a board of two
/// hundred jobs must not turn that into two hundred settles. Cancelling is
/// what kills a job — the token is set for every handle whatever the clock
/// says — so the deadline only ever shortens the *waiting*.
pub fn shutdown_within(session: &SessionId, grace: Duration) {
    let deadline = Instant::now() + grace;
    let handles: Vec<String> = with_board(session, |board| board.jobs.keys().cloned().collect());
    for handle in &handles {
        cancel_by(
            session,
            handle,
            deadline.min(Instant::now() + CANCEL_SETTLE),
        );
    }
    // Waited for by the flag rather than by the join, and bounded: a thread
    // sets `finished` as the last thing it does, so joining only the finished
    // ones is a join that returns, and a job the kill could not stop costs a
    // detached thread instead of a session that cannot exit.
    while Instant::now() < deadline && live(session) > 0 {
        std::thread::sleep(DEADLINE_SLICE);
    }
    // The threads are taken out from under the lock and joined outside it:
    // a job thread takes the same lock to raise its `bg.done`, so joining
    // while holding it would deadlock every time.
    let threads: Vec<JoinHandle<()>> = with_board(session, |board| {
        board
            .jobs
            .values_mut()
            .filter(|job| job.finished)
            .filter_map(|job| job.thread.take())
            .collect()
    });
    for thread in threads {
        let _ = thread.join();
    }
    let mut boards = boards().lock().unwrap_or_else(|e| e.into_inner());
    boards.remove(session.as_str());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cwd_and_env_are_refused_rather_than_ignored() {
        let denied = honourable(&RunOptions {
            cwd: Some("/tmp".to_string()),
            ..RunOptions::default()
        })
        .unwrap_err();
        assert_eq!(denied.path, "cwd");
        let denied = honourable(&RunOptions {
            env: Some("A=1".to_string()),
            ..RunOptions::default()
        })
        .unwrap_err();
        assert_eq!(denied.path, "env");
        assert!(honourable(&RunOptions::default()).is_ok());
    }

    /// Measured 2026-09-17 (session `tlitep-13fv`): three cancelled
    /// subagents answered `{status: "cancelled", stdout: "", stderr: ""}`, so
    /// the parent could not tell an exhausted budget from a refusal and
    /// started the same doomed subagent twice more.
    #[test]
    fn a_subagent_that_stopped_early_still_says_what_it_did() {
        let cancelled = crate::agent::AgentResult {
            answer: String::new(),
            status: "cancelled".into(),
            turns: 3,
            tokens: 0,
            trajectory: vec!["read".into(), "rg".into(), "context".into()],
        };
        let note = note_for(&cancelled);
        assert!(note.contains("was cancelled"), "{note}");
        assert!(note.contains("3 turn(s)"), "{note}");
        assert!(note.contains("read, rg, context"), "{note}");

        // The wall clock and the turn hint are different stops and say so:
        // a parent that reads "ran out of time" knows to give the next one
        // longer, where "cancelled" would have told it nothing.
        let timed_out = crate::agent::AgentResult {
            status: "deadline".into(),
            ..cancelled.clone()
        };
        assert!(note_for(&timed_out).contains("ran out of time"));
        let hinted = crate::agent::AgentResult {
            status: "turns".into(),
            ..cancelled.clone()
        };
        assert!(note_for(&hinted).contains("turn hint"));

        let silent = crate::agent::AgentResult {
            trajectory: Vec::new(),
            ..cancelled.clone()
        };
        assert!(note_for(&silent).contains("no tool call"));

        // A subagent that returned an answer needs no note: the answer is the
        // result, and a second line beside it is noise.
        let returned = crate::agent::AgentResult {
            answer: "done".into(),
            status: "returned".into(),
            ..cancelled.clone()
        };
        assert_eq!(note_for(&returned), "");
    }

    /// A long trajectory is counted, not printed: nothing caps a subagent's
    /// turns, so a long loop leaves hundreds of entries and the note is for
    /// reading.
    #[test]
    fn a_long_trajectory_is_bounded_and_the_rest_counted() {
        let busy = crate::agent::AgentResult {
            answer: String::new(),
            status: "turns".into(),
            turns: 24,
            tokens: 0,
            trajectory: (0..NOTED_TRAJECTORY + 5)
                .map(|_| "read".to_string())
                .collect(),
        };
        let note = note_for(&busy);
        assert!(note.contains("and 5 more"), "{note}");
        assert_eq!(note.matches("read").count(), NOTED_TRAJECTORY);
    }

    /// §1's `bg.done` key is `bg/<handle> + emission`, so two identical
    /// outputs of one watch produce one emission and two different ones do
    /// not.
    #[test]
    fn identical_output_has_the_same_emission() {
        assert_eq!(
            emission_of("still building\n"),
            emission_of("still building\n")
        );
        assert_ne!(emission_of("still building\n"), emission_of("built\n"));
    }

    /// **A session must be able to end**, and this is the property in the
    /// form the world cannot produce: a job whose thread never sets
    /// `finished` at all.
    ///
    /// The lifecycle verifier could not build one — a confined `bash` cannot
    /// `setsid` out of the group `cancel` kills, so every job a model can
    /// write dies — and a bound nothing can exercise is a bound nobody has
    /// checked. Reaching into the board is the only way to hold the exit the
    /// way a bug in this module would: the thread outlives the shutdown,
    /// which is the point, because [`shutdown_within`] joins only what has
    /// finished and detaches the rest.
    #[test]
    fn shutdown_is_bounded_by_its_grace_when_a_job_never_finishes() {
        let session = SessionId::new("bg-never-finishes");
        let stuck = std::thread::spawn(|| std::thread::sleep(Duration::from_secs(3)));
        with_board(&session, |board| {
            board.jobs.insert(
                "job1".to_string(),
                JobEntry {
                    token: CancellationToken::new(),
                    cancelled: false,
                    finished: false,
                    thread: Some(stuck),
                    started: Instant::now(),
                    progress: None,
                    inbox: None,
                    record: None,
                },
            );
        });

        let started = Instant::now();
        shutdown_within(&session, Duration::from_millis(200));
        let elapsed = started.elapsed();

        assert!(
            elapsed < Duration::from_secs(1),
            "a job that never finishes held the exit for {elapsed:?}; the grace is not a bound"
        );
    }

    #[test]
    fn a_summary_is_one_bounded_line() {
        let line = summary(&format!("{}\nsecond", "x".repeat(400)));
        assert_eq!(line.chars().count(), SUMMARY_CHARS);
        assert!(!line.contains('\n'));
    }
}
