//! A host-only, exact-call suspension seam. This is not a grant mechanism.
//!
//! A caller installs a gate on a runtime, receives requests on another thread,
//! and answers the one suspended Rust callback. The JavaScript cell stays on
//! its stack throughout; no source or continuation is returned for replay.
//! The immutable base profile must admit the call before a request is sent.
//! Neither a decision nor a remembered decision can add a sandbox capability.
//!
//! The live terminal can install this seam for explicit exact-call approval.
//! Interactive missing-grant approvals still require platform grants; see
//! `docs/sandbox.md`.

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
    Deny,
    /// A refusal that says what to do instead. The call is refused exactly
    /// as `Deny` refuses it -- remembered, never widened -- and the words
    /// travel back to the program as the refusal's rule, so the model reads
    /// them where it reads every other refusal. Empty text asks the model
    /// to propose another way itself.
    Redirect(String),
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
    /// `[decisions] command_runs_above` — the confidence at or above which
    /// this model's word lets a command line run without asking.
    command_runs_above: f64,
    /// This gate's cumulative count of approval hints that answered.
    asked: Arc<AtomicU32>,
    /// This gate's cumulative count of approval-hint requests that failed
    /// or timed out.
    failed: Arc<AtomicU32>,
}

/// The decision model, asked the one question the static permission reader
/// could not answer.
///
/// It exists for the length of one [`Gate::admit`] call and borrows the
/// gate's own session-scoped state, so no second copy of the model name,
/// the mode or the session's memory can drift from the gate's.
struct ModelJudge<'a> {
    decisions: &'a Decisions,
    vouched: &'a crate::permissions::Judged<String>,
}

impl crate::permissions::CommandJudge for ModelJudge<'_> {
    fn vouches_for(&self, line: &str) -> bool {
        let key = line.to_string();
        if let Some(remembered) = self.vouched.answer(&key) {
            return remembered;
        }
        // Synchronous, and that is the right shape here: the alternative to
        // waiting is asking the person, which costs far more than the
        // `DECISION_TIMEOUT` this call is bounded by. It is reached only for
        // the lines the static reader could not place, and only once per
        // distinct line per session.
        let answered = crate::decide::permission(&self.decisions.model, line);
        match answered {
            Ok(answer) => {
                self.decisions.asked.fetch_add(1, Ordering::Relaxed);
                let vouched = crate::decide::permission_for(
                    self.decisions.mode,
                    Some(&answer),
                    self.decisions.command_runs_above,
                );
                self.vouched.remember(key, vouched);
                vouched
            }
            // Not an answer, so not remembered: a timeout must not bar this
            // line from ever being vouched for again in this session.
            Err(_) => {
                self.decisions.failed.fetch_add(1, Ordering::Relaxed);
                false
            }
        }
    }
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
    /// Answers a person gave: allow-for-session, and refusals.
    ///
    /// **Only answers that could have been given differently are kept.** A
    /// static verdict is recomputed on every call because it is already
    /// deterministic — and because remembering it would make a person who
    /// moves *down* the ladder mid-task keep the permissions of the rung
    /// they left.
    judged: Arc<crate::permissions::Judged<Action>>,
    /// Which rung this session is on, live: the key handler moves it from
    /// the UI thread while a task runs.
    ladder: crate::permissions::Ladder,
    /// `[modes] commands` — the person's own extra read-only segment
    /// patterns, honoured by the ladder exactly as a narrowing mode honours
    /// them.
    read_only: Arc<Vec<String>>,
    wait_clock: Option<Arc<WaitClock>>,
    decisions: Option<Decisions>,
    /// What the decision model has already said about a command line.
    ///
    /// **A model answer is exactly the kind that could have been given
    /// differently**, so unlike a static verdict it is remembered: the same
    /// line asked twice gets the same answer for the rest of the session,
    /// and a retry cannot turn a question into a run (the user, 2026-09-18,
    /// on a classifier that "funktioniert meist wenn du es nochmal
    /// probierst"). Only a real answer is remembered — a timeout is not an
    /// answer and must not bar that line for the session.
    vouched: Arc<crate::permissions::Judged<String>>,
    /// The current task's request text, attached to the clone a `Runtime`
    /// holds for one task (`with_task`) -- `Gate` itself is session-scoped
    /// and outlives any one task.
    task: Option<String>,
    /// What the person asked for instead, keyed by the exact action they
    /// refused; taken once by the refusal that carries it to the program.
    redirects: Arc<Mutex<std::collections::BTreeMap<Action, String>>>,
}

impl Gate {
    pub fn channel(ladder: crate::permissions::Ladder) -> (Self, mpsc::Receiver<Request>) {
        let (requests, receiver) = mpsc::channel();
        (
            Self {
                requests,
                remembered: Arc::new(Mutex::new(BTreeSet::new())),
                judged: Arc::new(crate::permissions::Judged::default()),
                ladder,
                read_only: Arc::new(Vec::new()),
                wait_clock: None,
                decisions: None,
                vouched: Arc::new(crate::permissions::Judged::default()),
                task: None,
                redirects: Arc::new(Mutex::new(std::collections::BTreeMap::new())),
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

    /// The rung this gate is judging on, shared with whatever moves it.
    pub fn ladder(&self) -> &crate::permissions::Ladder {
        &self.ladder
    }

    /// Attaches `[modes] commands`, the person's own extra read-only segment
    /// patterns, once at session start.
    #[must_use]
    pub fn with_read_only(mut self, patterns: Vec<String>) -> Self {
        self.read_only = Arc::new(patterns);
        self
    }

    /// Attaches the `[decisions]` model and mode once, at session start
    /// (`decision-model.md`). `None` leaves every clone of this gate exactly
    /// as it is today: no thread, no request.
    pub(crate) fn with_decisions(
        mut self,
        model: Option<String>,
        mode: crate::config::DecisionMode,
        command_runs_above: f64,
    ) -> Self {
        self.decisions = model.map(|model| Decisions {
            model,
            mode,
            command_runs_above,
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

    /// Values that can be displayed without leaking command arguments,
    /// file contents, or MCP parameters. This seam has no persisted state.
    pub fn session_actions(&self) -> Vec<String> {
        self.remembered
            .lock()
            .map(|actions| actions.iter().map(Action::summary).collect())
            .unwrap_or_default()
    }

    /// Asks the decision model about every command line the cell already
    /// spells out, together, before the cell runs.
    ///
    /// **It answers nothing that the gate would not answer the same way.**
    /// Each line goes through the very judge [`Gate::admit`] uses, keyed by
    /// the same exact line, so a line pre-judged here and then run answers
    /// from memory instead of asking twice — and a line this never saw is
    /// met by the gate exactly as before. No person is asked here: a
    /// confirmation belongs beside the call that needs it, not in a queue at
    /// the head of a program.
    ///
    /// The win is the waiting. Serially, in the middle of a cell, each
    /// unplaced line costs its own round trip to the decision model; here
    /// they are asked at once, so a cell with six unplaced lines waits once
    /// rather than six times.
    pub(crate) fn prejudge(&self, lines: &[String]) {
        let Some(decisions) = self.decisions.clone() else {
            return;
        };
        if decisions.mode == crate::config::DecisionMode::Off
            || self.ladder.rung() != crate::permissions::Rung::Auto
        {
            return;
        }
        let unanswered: Vec<String> = lines
            .iter()
            .filter(|line| self.vouched.answer(line).is_none())
            .cloned()
            .collect();
        if unanswered.is_empty() {
            return;
        }
        // The same pause an approval takes: this is a wait on something
        // outside the isolate, not the cell computing.
        let _waiting = self.wait_clock.as_ref().map(WaitClock::pause);
        let threads: Vec<_> = unanswered
            .into_iter()
            .map(|line| {
                let decisions = decisions.clone();
                let vouched = self.vouched.clone();
                thread::spawn(move || {
                    use crate::permissions::CommandJudge;
                    ModelJudge {
                        decisions: &decisions,
                        vouched: &vouched,
                    }
                    .vouches_for(&line);
                })
            })
            .collect();
        for thread in threads {
            let _ = thread.join();
        }
    }

    /// Whether this already-admitted call may run, asking the person when
    /// the rung says to.
    ///
    /// The order is the whole policy: an answer already given stands, then
    /// the rung judges, and only a judgement of *ask* reaches a person. A
    /// session with no terminal to ask at runs what it would have asked
    /// about — the profile is still the boundary, and refusing instead would
    /// break every scripted run for a question nobody is there to answer.
    pub(crate) fn admit(&self, action: Action, stopped: impl Fn() -> bool) -> bool {
        if stopped() {
            return false;
        }
        if let Some(answer) = self.judged.answer(&action) {
            return answer && !stopped();
        }
        let Ok(remembered) = self.remembered.lock() else {
            return false;
        };
        if remembered.contains(&action) {
            return !stopped();
        }
        drop(remembered);
        let model = self.decisions.as_ref().and_then(|decisions| {
            (decisions.mode != crate::config::DecisionMode::Off).then(|| ModelJudge {
                decisions,
                vouched: &self.vouched,
            })
        });
        match crate::permissions::judge(
            self.ladder.rung(),
            action.tool(),
            action.arguments(),
            &self.read_only,
            model
                .as_ref()
                .map(|judge| judge as &dyn crate::permissions::CommandJudge),
        ) {
            crate::permissions::Verdict::Runs => return !stopped(),
            crate::permissions::Verdict::Refuse(_) => {
                self.judged.remember(action, false);
                return false;
            }
            crate::permissions::Verdict::Ask(_) => {
                if self.ladder.is_unattended() {
                    return !stopped();
                }
            }
        }
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
            })
            .is_err()
        {
            return false;
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
            let summary = action.summary();
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
            if stopped() || waiting_started.elapsed() >= MAX_APPROVAL_WAIT {
                return false;
            }
            match response.recv_timeout(Duration::from_millis(20)) {
                Ok(decision) => {
                    // A response queued before a cancellation is still denied
                    // if the call has stopped before it can consume that answer.
                    if stopped() || waiting_started.elapsed() >= MAX_APPROVAL_WAIT {
                        return false;
                    }
                    return match decision {
                        // Once is once: not remembered, because the person
                        // said so.
                        Decision::AllowOnce => true,
                        Decision::AllowForSession => {
                            let Ok(mut remembered) = self.remembered.lock() else {
                                return false;
                            };
                            remembered.insert(action);
                            true
                        }
                        // Remembered, so that asking again cannot turn a no
                        // into a yes: a gate that answers differently on a
                        // retry teaches retrying.
                        Decision::Deny => {
                            self.judged.remember(action, false);
                            false
                        }
                        // Refused exactly as a denial is, and the words wait
                        // for the refusal that reports it (`redirect_for`).
                        Decision::Redirect(text) => {
                            if let Ok(mut redirects) = self.redirects.lock() {
                                redirects.insert(action.clone(), text);
                            }
                            self.judged.remember(action, false);
                            false
                        }
                    };
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => return false,
                Err(mpsc::RecvTimeoutError::Timeout) => {}
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::permissions::{Ladder, Rung};

    fn bash(line: &str) -> Action {
        let mut arguments = std::collections::BTreeMap::new();
        arguments.insert("command".to_string(), line.to_string());
        Action::new("bash", std::path::Path::new("/tmp/root"), arguments)
    }

    fn gate_with_a_model(rung: Rung) -> (Gate, mpsc::Receiver<Request>) {
        let (gate, requests) = Gate::channel(Ladder::new(rung));
        // A model name no request ever reaches: every test here answers from
        // the gate's own memory, which is the property under test.
        (
            gate.with_decisions(
                Some("unreachable-model".to_string()),
                crate::config::DecisionMode::On,
                0.85,
            ),
            requests,
        )
    }

    /// **The pre-judgement and the gate share one memory, keyed by the exact
    /// command line.** Without this the reading done at submit time buys
    /// nothing: the call would ask again when it happens.
    #[test]
    fn a_line_already_vouched_for_runs_without_reaching_anybody() {
        let (gate, requests) = gate_with_a_model(Rung::Auto);
        gate.vouched
            .remember("./scripts/build.sh --release".to_string(), true);
        assert!(gate.admit(bash("./scripts/build.sh --release"), || false));
        assert!(
            requests.try_recv().is_err(),
            "a remembered answer must cost no confirmation"
        );
    }

    /// And a remembered *no* is a question, never a refusal — the ladder's
    /// one structural property, held at the seam a person meets.
    #[test]
    fn a_line_vouched_against_is_put_to_the_person() {
        let (gate, requests) = gate_with_a_model(Rung::Auto);
        gate.vouched
            .remember("rm -rf /var/tmp/x".to_string(), false);
        let asking = std::thread::spawn(move || gate.admit(bash("rm -rf /var/tmp/x"), || false));
        let request = requests
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("the person must be asked");
        assert_eq!(request.action().tool(), "bash");
        assert!(request.respond(Decision::AllowOnce));
        assert!(asking.join().unwrap());
    }

    /// A refusal with words is a refusal: not admitted, remembered, and the
    /// words wait for the one refusal that reports them.
    #[test]
    fn a_redirect_refuses_like_a_denial_and_hands_its_words_over_once() {
        let (gate, requests) = gate_with_a_model(Rung::Auto);
        gate.vouched
            .remember("rm -rf /var/tmp/x".to_string(), false);
        let asked = gate.clone();
        let asking = std::thread::spawn(move || asked.admit(bash("rm -rf /var/tmp/x"), || false));
        let request = requests
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("the person must be asked");
        assert!(request.respond(Decision::Redirect("use fd instead".into())));
        assert!(!asking.join().unwrap(), "refused");
        assert_eq!(
            gate.redirect_for(&bash("rm -rf /var/tmp/x")).as_deref(),
            Some("use fd instead")
        );
        assert_eq!(
            gate.redirect_for(&bash("rm -rf /var/tmp/x")),
            None,
            "taken once"
        );
        // Asking again cannot turn the no into a yes.
        assert!(!gate.admit(bash("rm -rf /var/tmp/x"), || false));
        assert!(
            requests.try_recv().is_err(),
            "a remembered refusal asks nobody"
        );
    }

    /// `prejudge` is the `auto` rung's own machinery and nothing else's: the
    /// rungs that ask about every command line must not have their questions
    /// pre-answered, and `full` asks nothing to begin with.
    #[test]
    fn pre_judgement_happens_on_the_auto_rung_alone() {
        for rung in [Rung::Manual, Rung::AcceptEdits, Rung::Full] {
            let (gate, _requests) = gate_with_a_model(rung);
            // The model is unreachable, so a rung that did ask would spend
            // the decision timeout here and remember nothing either way.
            gate.prejudge(&["some-unplaceable-line --now".to_string()]);
            assert!(
                gate.vouched.is_empty(),
                "{} must not pre-judge anything",
                rung.name()
            );
        }
    }

    /// With decisions off there is nothing to ask, and no thread is started
    /// to discover that.
    #[test]
    fn pre_judgement_asks_nothing_with_decisions_off() {
        let (gate, _requests) = Gate::channel(Ladder::new(Rung::Auto));
        gate.prejudge(&["some-unplaceable-line --now".to_string()]);
        assert!(gate.vouched.is_empty());

        let (gate, _requests) = Gate::channel(Ladder::new(Rung::Auto));
        let gate = gate.with_decisions(
            Some("unreachable-model".to_string()),
            crate::config::DecisionMode::Off,
            0.85,
        );
        gate.prejudge(&["some-unplaceable-line --now".to_string()]);
        assert!(gate.vouched.is_empty());
    }

    /// A line already answered is not asked about again, however many times
    /// it appears — the bound that keeps a loop of one command cheap.
    #[test]
    fn a_line_already_answered_is_not_asked_again() {
        let (gate, _requests) = gate_with_a_model(Rung::Auto);
        gate.vouched
            .remember("already --answered".to_string(), true);
        // Reaching the model would block for the decision timeout against a
        // name that does not resolve; returning at once is the assertion.
        let started = Instant::now();
        gate.prejudge(&["already --answered".to_string()]);
        assert!(
            started.elapsed() < Duration::from_millis(500),
            "an answered line must not be asked again"
        );
    }
}
