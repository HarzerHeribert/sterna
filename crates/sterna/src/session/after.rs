//! The work behind the answer: the completion checker, and the
//! learned-notes writer (`learned.rs`). The checker was moved behind the answer (2026-09-23, *lanes, not
//! gates*): the answer is delivered first, the fresh checker runs on its own
//! thread, and what it finds is a note for the person -- it never holds the
//! answer and never costs the model a turn.
//!
//! **The invariant: nothing the checker says reaches the model or changes
//! the task's outcome.** Measured the same day: 11 of 11 checker-style
//! findings held attempts that then passed their own tests, each for extra
//! turns and minutes. What still holds is decided in `task.rs`'s gate from
//! facts alone ([`crate::completion::FindingKind::holds`]).

use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::thread::JoinHandle;

use super::ui;

/// One finished check, as the person is shown it.
#[derive(Debug, Clone)]
pub(super) struct Note {
    /// `holds`, `does not hold` or `cannot tell` -- the checker's first line.
    pub verdict: String,
    pub text: String,
    pub record: crate::helpers::HelperRecord,
}

impl Note {
    /// The line the conversation shows: short when the answer held up, the
    /// checker's evidence when it did not.
    pub(super) fn line(&self) -> String {
        let checked = crate::tui::history::CHECKED;
        if self.verdict == "holds" {
            return format!("{checked}holds");
        }
        let body: String = self
            .text
            .lines()
            .skip(1)
            .filter(|line| !line.trim().is_empty())
            .take(6)
            .collect::<Vec<_>>()
            .join("\n");
        format!("{checked}{}\n{body}", self.verdict)
    }
}

/// What the thread needs, owned: it outlives the gate that started it.
pub(super) struct Check {
    pub evidence: String,
    pub model: String,
    pub effort: crate::wire::Effort,
    pub profile: crate::sandbox::profile::Profile,
    pub session: crate::contract::SessionId,
}

/// What one piece of work behind the answer produced.
enum Done {
    Check(Box<Note>),
    /// Lines added to `.sterna/learned.md`.
    Learned(Vec<String>),
    /// The work could not be done, and why: a check that failed is said,
    /// never mistaken for one that found nothing.
    Failed(String),
    Nothing,
}

static PENDING: Mutex<Vec<JoinHandle<Done>>> = Mutex::new(Vec::new());

/// True for the length of a one-task run ([`around`]).
static ONE_TASK: AtomicBool = AtomicBool::new(false);

/// Whether a piece of work behind the answer may start. **A one-task run
/// (`--task`, `-p`, `exec`) starts it only when the person wrote its switch
/// themselves (`chosen`)**: the run exits when the task is done, so the work
/// would either hold the exit or die with its request already paid for.
/// Measured 2026-09-24: in 24 ruler attempts the check held each exit
/// 15–60 s and changed no outcome. A terminal session always may -- nobody
/// waits for it there.
pub(super) fn may_start(chosen: bool) -> bool {
    chosen || !ONE_TASK.load(Ordering::Relaxed)
}

/// Runs `work` on its own thread, and tells a terminal session when the
/// lane starts and ends, so the screen shows it working behind the answer.
/// Any note the work sends arrives before the end, so the verdict replaces
/// the working row rather than following a gap.
fn start(lane: &'static str, work: impl FnOnce() -> Done + Send + 'static) {
    let sender = ui::sender();
    if let Some(sender) = &sender {
        let _ = sender.send(ui::Update::Behind(lane, true));
    }
    let handle = std::thread::spawn(move || {
        let done = work();
        if let Some(sender) = sender {
            let _ = sender.send(ui::Update::Behind(lane, false));
        }
        done
    });
    if let Ok(mut pending) = PENDING.lock() {
        pending.push(handle);
    }
}

/// What the learned-notes writer needs, owned.
pub(super) struct Learn {
    pub ask: String,
    pub model: String,
    pub profile: crate::sandbox::profile::Profile,
    pub session: crate::contract::SessionId,
}

/// Asks the writer once and appends what passes `learned::accept`.
pub(super) fn spawn_learn(learn: Learn) {
    let sender: Option<Sender<ui::Update>> = ui::sender();
    start("learn", move || {
        let token = crate::tools::invoke::CancellationToken::new();
        let call = crate::helpers::run(
            &crate::learned::WRITER,
            crate::helpers::HelperRoute::new(&learn.model, crate::wire::Effort::Low),
            &learn.ask,
            &learn.profile,
            &learn.session,
            &token,
        );
        if !call.outcome.ok {
            return Done::Failed(format!("learn: {}", call.outcome.text));
        }
        let existing = crate::learned::read(&learn.profile);
        let lines = crate::learned::accept(&call.outcome.text, &existing, learn.profile.root());
        if lines.is_empty() || crate::learned::append(&learn.profile, &lines).is_err() {
            return Done::Nothing;
        }
        if let Some(sender) = &sender {
            let _ = sender.send(ui::Update::Notice(learned_line(&lines)));
        }
        Done::Learned(lines)
    });
}

pub(super) fn learned_line(lines: &[String]) -> String {
    format!(
        "{}{} note{} → {}\n{}",
        crate::tui::history::LEARNED,
        lines.len(),
        if lines.len() == 1 { "" } else { "s" },
        crate::learned::PATH,
        lines.join("\n")
    )
}

/// Starts the check. In a terminal session the note is sent to the screen
/// the moment it exists; otherwise it waits for [`settle`].
pub(super) fn spawn(check: Check) {
    let sender: Option<Sender<ui::Update>> = ui::sender();
    start("check", move || {
        let token = crate::tools::invoke::CancellationToken::new();
        let context = crate::helpers::HelperContext {
            profile: &check.profile,
            session: &check.session,
            token: &token,
        };
        let Some((record, _)) = crate::helpers::check_completion_judged(
            &check.evidence,
            crate::helpers::HelperRoute::new(&check.model, check.effort),
            context,
            None,
        ) else {
            return Done::Failed("check: no checker in the roster".into());
        };
        if !record.outcome.ok {
            // A check that ran and failed is said, never mistaken for one
            // that found nothing.
            if let Some(sender) = &sender {
                let checked = crate::tui::history::CHECKED;
                let reason = record.outcome.text.lines().next().unwrap_or("");
                let _ = sender.send(ui::Update::Notice(format!(
                    "{checked}could not run · {reason}"
                )));
            }
            return Done::Failed(format!("check: {}", record.outcome.text));
        }
        let verdict = record
            .outcome
            .text
            .lines()
            .next()
            .unwrap_or("")
            .trim()
            .trim_matches(['*', '`', '.'])
            .to_ascii_lowercase();
        let note = Note {
            verdict,
            text: record.outcome.text.clone(),
            record,
        };
        if let Some(sender) = sender {
            let _ = sender.send(ui::Update::Notice(note.line()));
        }
        Done::Check(Box::new(note))
    });
}

/// What finished behind the answer: the checks' notes and the learned
/// lines -- of the work that has ended, or with `wait`, of all of it. A
/// session about to exit waits, so its result carries them; a terminal
/// session between tasks does not.
pub(super) fn settle(wait: bool) -> (Vec<Note>, Vec<String>, Vec<String>) {
    let Ok(mut pending) = PENDING.lock() else {
        return (Vec::new(), Vec::new(), Vec::new());
    };
    let mut notes = Vec::new();
    let mut learned = Vec::new();
    let mut failed = Vec::new();
    let mut still = Vec::new();
    for handle in pending.drain(..) {
        if wait || handle.is_finished() {
            match handle.join() {
                Ok(Done::Check(note)) => notes.push(*note),
                Ok(Done::Learned(lines)) => learned.extend(lines),
                Ok(Done::Failed(reason)) => failed.push(reason),
                Ok(Done::Nothing) => {}
                Err(_) => failed.push("a thread behind the answer panicked".into()),
            }
        } else {
            still.push(handle);
        }
    }
    *pending = still;
    (notes, learned, failed)
}

/// A one-task run, then [`finish_run`] -- inside `run`, while the session's
/// gateway still serves: the work behind the answer asks it too, and it
/// stops when `run` returns (measured: `Connection reset by peer`).
pub(super) fn around<T>(task: impl FnOnce() -> T) -> T {
    ONE_TASK.store(true, Ordering::Relaxed);
    let result = task();
    finish_run();
    ONE_TASK.store(false, Ordering::Relaxed);
    result
}

/// The end of a one-task run: wait for the work behind the answer so the
/// result carries it, and say what it found. The answer itself was already
/// delivered; `wait_ms` in the result is how long this took.
pub(super) fn finish_run() {
    let waited = std::time::Instant::now();
    let (checks, learned, failed) = settle(true);
    if checks.is_empty() && learned.is_empty() && failed.is_empty() {
        return;
    }
    super::output::after_checks(
        &checks,
        &learned,
        &failed,
        waited.elapsed().as_millis() as u64,
    );
    if !super::output::active() {
        for note in &checks {
            eprintln!("{}", note.line());
        }
        if !learned.is_empty() {
            eprintln!("{}", learned_line(&learned));
        }
        for reason in &failed {
            eprintln!("behind the answer, failed: {reason}");
        }
    }
}
