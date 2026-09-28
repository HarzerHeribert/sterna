//! The live event stream: what a session is doing *now*, for a program that
//! is watching rather than reading afterwards.
//!
//! **Why this exists beside [`crate::rollout`] rather than inside it.** The
//! rollout is complete and it is late: a `cell` line is written when the cell
//! has finished, a `view` line when its evidence is settled. An observer
//! reading it can say what a session *did* and can never say what it is
//! *doing* — it cannot tell a cell that is working from one that is hung,
//! which is the single question an observer has. This file answers that and
//! nothing else: one line per transition, an opening for every close, flushed as
//! it happens.
//!
//! **Payloads stay out.** A diff, a file's bytes, a command's stdout — those
//! are in the rollout, and an observer that wants them reads it by the path
//! [`Observer::session_begin`] writes. Putting them here would make the
//! stream too heavy to `tail -f` and turn an observability surface into a
//! second, lossier copy of the record.
//!
//! **An opening event carries enough to decide on.** A later hook mechanism
//! is synchronous where this is observational: it sits in the decision path
//! and may refuse, while this can only ever report. The vocabulary has to
//! survive being used in both directions, so the seven events
//! [`Kind::decides`] names carry what a decision needs — not merely what a
//! log needs. Nothing here is synchronous and nothing returns a verdict; the
//! shape is chosen so that adding one later renames nothing.
//!
//! **The unit is the cell, not the call.** Sterna exists so a model writes a
//! program instead of calling tools one at a time, and a vocabulary built on
//! the call would re-impose the thing the product escapes. The decision seam
//! is already cell-shaped: [`crate::runtime::commands`] reads a submitted
//! program's literal command lines and judges them *together, before it
//! runs*, rather than meeting each one in the middle. Seeing the whole
//! program is seeing the intent, which one call at a time never shows. So
//! [`Kind::CellSubmit`] is the event a hook will be handed, and it carries
//! those lines. On a program that makes twenty calls that is **one**
//! question where a per-call harness asks nineteen.
//!
//! [`Kind::CommandJudge`] exists anyway, and for a stated reason rather than by
//! imitation: `commands.rs` reports only what is *certain* in the source and
//! leaves a line assembled from a variable, a call or a template to the
//! runtime gate, because "reporting a guess here would pre-answer a question
//! about a line that never runs". A computed line is knowable only when it
//! is made, so that seam has to exist too — and with [`Kind::CommandEnd`] it
//! is how an observer answers *what is it doing right now* while a long
//! command is in flight.
//!
//! The field names follow `docs/events.md` where
//! they mean the same thing: `kind` is dotted, `at` is ISO-8601 UTC to the
//! millisecond and records when the runtime **accepted** the transition. That
//! contract's `payload` and `priority` are deliberately absent — they belong
//! to inbound events a session consumes, and these are outbound facts a
//! session produces.

use std::collections::BTreeMap;
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

/// The longest an argument value may be before it is cut.
///
/// A command line is decision-relevant and short; a fifty-kilobyte argument
/// is a payload wearing an argument's clothes, and the whole point of this
/// stream is that it stays tailable. The cut is marked so a reader never
/// mistakes a truncated value for a complete one.
const VALUE_CAP: usize = 2048;

/// Shannon entropy per character a tail must clear to read as a real secret
/// rather than a placeholder, matching `scripts/check-secrets.py`'s bar and
/// chosen there against sampled provider tokens.
const ENTROPY_BAR: f64 = 3.5;

/// Prefixes that make the rest of a word a candidate secret. The same shapes
/// `scripts/check-secrets.py` guards; a value carrying one of these with a
/// high-entropy tail never reaches the file.
const KEY_PREFIXES: [&str; 8] = [
    "sk-ant-api03-",
    "sk-proj-",
    "gsk_",
    "ghp_",
    "github_pat_",
    "xoxb-",
    "AKIA",
    "-----BEGIN",
];

/// What was replaced, spelled so a reader can tell redaction from a value
/// that genuinely looked like this.
const REDACTED: &str = "«redacted»";

/// One kind of transition, named for Sterna's own units of work rather than
/// borrowed from another harness. Dotted like `events-contract.md` §1's
/// kinds.
///
/// Two shapes. A **span** opens and closes, and its two halves share a
/// `span` id so a reader can pair them and take a duration: `session`,
/// `task`, `turn`, `cell`, `helper`, `agent`. A **moment** happens once and
/// closes nothing: `cell.repair`, `command.judge`, `file.change`,
/// `answer.propose`, `ask.raise`, `supervisor.look`, `sandbox.move`,
/// `reduction.made`, `approval.raise`.
///
/// **Seven can be refused** once the synchronous half exists
/// ([`Kind::decides`]). Those carry what a decision needs; the rest carry
/// what a record needs. The headline of the shape is [`Kind::CellSubmit`]:
/// on a program that makes twenty calls it is **one** question where a
/// per-call harness asks nineteen, and it is the better question as well as
/// the cheaper one, because a reader that sees the whole program sees the
/// intent and one that sees a single call never can.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    SessionBegin,
    SessionEnd,
    TaskBegin,
    TaskEnd,
    TurnBegin,
    TurnEnd,
    /// The program is parsed and nothing has run — the pre-cell seam.
    CellSubmit,
    /// A parse failure is being amended before the cell is abandoned.
    CellRepair,
    CellEnd,
    /// A command line is now certain, including one computed at runtime.
    CommandJudge,
    /// What the command the judge admitted actually did.
    ///
    /// **Not in the approved vocabulary, and added deliberately.**
    /// `command.judge` alone is a decision with no outcome: an observer that
    /// sees only the seam cannot tell a command that is running from one
    /// that has hung, which is the single question this stream exists to
    /// answer. It closes `command.judge`'s span and nothing else.
    CommandEnd,
    FileChange,
    HelperBegin,
    HelperEnd,
    AgentBegin,
    AgentEnd,
    /// `answer(text)` was called and the task has not ended yet.
    AnswerPropose,
    AskRaise,
    SupervisorLook,
    SandboxMove,
    ReductionMade,
    ApprovalRaise,
}

impl Kind {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::SessionBegin => "session.begin",
            Self::SessionEnd => "session.end",
            Self::TaskBegin => "task.begin",
            Self::TaskEnd => "task.end",
            Self::TurnBegin => "turn.begin",
            Self::TurnEnd => "turn.end",
            Self::CellSubmit => "cell.submit",
            Self::CellRepair => "cell.repair",
            Self::CellEnd => "cell.end",
            Self::CommandJudge => "command.judge",
            Self::CommandEnd => "command.end",
            Self::FileChange => "file.change",
            Self::HelperBegin => "helper.begin",
            Self::HelperEnd => "helper.end",
            Self::AgentBegin => "agent.begin",
            Self::AgentEnd => "agent.end",
            Self::AnswerPropose => "answer.propose",
            Self::AskRaise => "ask.raise",
            Self::SupervisorLook => "supervisor.look",
            Self::SandboxMove => "sandbox.move",
            Self::ReductionMade => "reduction.made",
            Self::ApprovalRaise => "approval.raise",
        }
    }

    /// The opening event this closing one completes. `None` for an opening
    /// event and for a moment.
    #[must_use]
    pub fn opens(self) -> Option<Self> {
        match self {
            Self::SessionEnd => Some(Self::SessionBegin),
            Self::TaskEnd => Some(Self::TaskBegin),
            Self::TurnEnd => Some(Self::TurnBegin),
            Self::CellEnd => Some(Self::CellSubmit),
            Self::CommandEnd => Some(Self::CommandJudge),
            Self::HelperEnd => Some(Self::HelperBegin),
            Self::AgentEnd => Some(Self::AgentBegin),
            _ => None,
        }
    }

    /// Whether a synchronous reader at this seam could refuse what follows.
    ///
    /// Recorded now so the vocabulary carries the distinction before
    /// anything acts on it: these are the events a hook would be *asked*,
    /// and every other one it would merely be *told*. Nothing in this module
    /// acts on the answer — there is no answer yet.
    #[must_use]
    pub fn decides(self) -> bool {
        matches!(
            self,
            Self::CellSubmit
                | Self::CellRepair
                | Self::CommandJudge
                | Self::AgentBegin
                | Self::AnswerPropose
                | Self::AskRaise
                | Self::SandboxMove
        )
    }
}

/// The file a session appends to, or nothing at all.
///
/// Cloning is cheap and every clone writes to the same file under the same
/// lock: the runtime holds one, the session holds one, and a tool call deep
/// inside a V8 callback writes through the same handle without threading a
/// mutable borrow down to it.
#[derive(Clone, Default)]
pub struct Observer(Option<Arc<Sink>>);

struct Sink {
    file: Mutex<File>,
    session: String,
}

impl Observer {
    /// A session nobody is watching. Every method is a no-op and costs one
    /// `Option` test.
    #[must_use]
    pub fn none() -> Self {
        Self(None)
    }

    /// The stream that sits beside the rollout at `rollout`, sharing its
    /// stem: `session.jsonl` is watched through `session.events.jsonl`.
    ///
    /// **A stream that cannot be opened is not an error.** The session runs
    /// unwatched: a harness that fails a turn because nobody could observe it
    /// is worse than one that runs without telemetry.
    #[must_use]
    pub fn beside(rollout: &Path, session: &str) -> Self {
        let Some(path) = Self::path_beside(rollout) else {
            return Self::none();
        };
        match OpenOptions::new().create(true).append(true).open(&path) {
            Ok(file) => Self(Some(Arc::new(Sink {
                file: Mutex::new(file),
                session: session.to_string(),
            }))),
            Err(_) => Self::none(),
        }
    }

    /// Where the stream for a rollout lives. One file per session, because
    /// the rollout is one file per session and a reader that has found one
    /// has found the other by changing the extension — no registry, no
    /// lookup, no guessing.
    #[must_use]
    pub fn path_beside(rollout: &Path) -> Option<PathBuf> {
        let stem = rollout.file_stem()?.to_str()?;
        Some(rollout.with_file_name(format!("{stem}.events.jsonl")))
    }

    /// Whether anything is being written, for a caller that wants to say so.
    #[must_use]
    pub fn is_watching(&self) -> bool {
        self.0.is_some()
    }

    /// The session opened, and where its full record is.
    ///
    /// Carries the rollout's path so a tool that found this file can read
    /// the payloads this stream deliberately omits.
    /// The session opened, with where its full record is and what it is
    /// working on.
    ///
    /// **No model here**, deliberately: `/model` can change it mid-session,
    /// so a model recorded once at the top would be a fact with a shelf
    /// life. [`Observer::task_begin`] carries the model that is actually
    /// about to be asked.
    pub fn session_begin(&self, rollout: &Path, root: &Path) {
        self.write(Kind::SessionBegin, "session", |line| {
            line.insert("rollout".into(), rollout.display().to_string().into());
            line.insert("root".into(), root.display().to_string().into());
        });
    }

    pub fn session_end(&self) {
        self.write(Kind::SessionEnd, "session", |_| {});
    }

    /// One request from the person, and the whole of it, with the settings
    /// that decide what it may do. A reader at this seam is judging the
    /// request itself, so the text rides along cut but never summarised.
    pub fn task_begin(&self, task: &str, mode: &str, rung: &str, model: &str) {
        self.write(Kind::TaskBegin, "task", |line| {
            line.insert("task".into(), safe(task).into());
            line.insert("mode".into(), mode.into());
            line.insert("rung".into(), rung.into());
            line.insert("model".into(), model.into());
        });
    }

    /// The task is over and why. **No cell count**: this seam does not hold
    /// one, and a zero written here would be a lie a reader could not tell
    /// from a task that truly ran nothing. Counting `cell.end` lines gives
    /// the real figure.
    pub fn task_end(&self, reason: &str) {
        self.write(Kind::TaskEnd, "task", |line| {
            line.insert("reason".into(), reason.into());
        });
    }

    pub fn turn_begin(&self, turn: u64) {
        self.write(Kind::TurnBegin, &format!("t{turn}"), |line| {
            line.insert("turn".into(), turn.into());
        });
    }

    pub fn turn_end(&self, turn: u64) {
        self.write(Kind::TurnEnd, &format!("t{turn}"), |line| {
            line.insert("turn".into(), turn.into());
        });
    }

    /// The program is parsed and nothing has run — **the pre-cell seam**,
    /// and the event a hook will be handed.
    ///
    /// It carries the whole source, the model's own one-line description so
    /// a watcher reads the same words as a person at the same moment, and
    /// the command lines [`crate::runtime::commands`] has proved are certain
    /// in that source. A line assembled from a variable is deliberately
    /// absent, because reporting a guess would pre-answer a question about a
    /// line that never runs; it arrives as its own [`Kind::CommandJudge`]
    /// when it is made.
    ///
    /// **The program is the one payload this stream carries**, and it earns
    /// the exception: a reader that may refuse a program cannot judge it
    /// from a summary. It is still cut at [`VALUE_CAP`].
    ///
    /// It is spelled `program` and not `source` because `source` is the
    /// envelope's own field — `events-contract.md` §1 gives it to the origin
    /// of an event — and a cell's text written under that key silently
    /// replaced it.
    pub fn cell_submit(
        &self,
        cell: usize,
        source: &str,
        description: Option<&str>,
        certain_commands: &[String],
    ) {
        self.write(Kind::CellSubmit, &format!("c{cell}"), |line| {
            line.insert("cell".into(), cell.into());
            line.insert("program".into(), safe(source).into());
            if let Some(description) = description {
                line.insert("description".into(), cut(description).into());
            }
            let commands: Vec<serde_json::Value> = certain_commands
                .iter()
                .map(|command| safe(command).into())
                .collect();
            line.insert("commands".into(), commands.into());
        });
    }

    pub fn cell_end(&self, cell: usize, outcome: &str, calls: usize) {
        self.write(Kind::CellEnd, &format!("c{cell}"), |line| {
            line.insert("cell".into(), cell.into());
            line.insert("outcome".into(), outcome.into());
            line.insert("calls".into(), calls.into());
        });
    }

    /// A command line is now certain, including one a running program built
    /// from a variable. **This is the one call-level event**, and it is here
    /// for the reason `commands.rs` states about itself: such a line is
    /// unknowable at submit time, so the seam has to exist. It is also how
    /// an observer answers *what is it doing right now* while a long command
    /// is in flight.
    pub fn command_judge(
        &self,
        span: &str,
        cell: usize,
        tool: &str,
        args: &BTreeMap<String, String>,
    ) {
        self.write(Kind::CommandJudge, span, |line| {
            line.insert("cell".into(), cell.into());
            line.insert("tool".into(), tool.into());
            let arguments: serde_json::Map<String, serde_json::Value> = args
                .iter()
                .map(|(key, value)| (key.clone(), safe(value).into()))
                .collect();
            line.insert("arguments".into(), arguments.into());
        });
    }

    /// One call finished. Shares its `span` with the [`Kind::CommandJudge`]
    /// that opened it, so a reader takes the duration by pairing them.
    pub fn command_end(&self, span: &str, cell: usize, tool: &str, ended: &str, exit: Option<i32>) {
        self.write(Kind::CommandEnd, span, |line| {
            line.insert("cell".into(), cell.into());
            line.insert("tool".into(), tool.into());
            line.insert("ended".into(), ended.into());
            if let Some(exit) = exit {
                line.insert("exit_code".into(), exit.into());
            }
        });
    }

    /// Builds one line and appends it. Nothing here can fail a caller: a
    /// serialisation error, a poisoned lock and a write error are all the
    /// same outcome, which is that this session is not being watched any
    /// more.
    fn write(
        &self,
        kind: Kind,
        span: &str,
        fill: impl FnOnce(&mut serde_json::Map<String, serde_json::Value>),
    ) {
        let Some(sink) = self.0.as_ref() else { return };
        let mut line = serde_json::Map::new();
        line.insert("kind".into(), kind.as_str().into());
        line.insert("at".into(), now_iso8601().into());
        line.insert("source".into(), format!("session/{}", sink.session).into());
        line.insert("span".into(), span.into());
        fill(&mut line);
        let Ok(mut json) = serde_json::to_vec(&serde_json::Value::Object(line)) else {
            return;
        };
        json.push(b'\n');
        // Silent, exactly as `Rollout::record_moves` and
        // `glasshouse::emit_lifecycle` are silent, and for the reason the
        // latter writes down: a harness that fails a turn because nobody
        // could observe it is worse than one that runs unobserved. A full
        // disk stops the watching and never the work.
        if let Ok(mut file) = sink.file.lock() {
            let _ = file.write_all(&json).and_then(|()| file.flush());
        }
    }
}

/// An argument value as it may be written: redacted if it carries a key's
/// shape, and cut if it is long enough to be a payload.
fn safe(value: &str) -> String {
    if looks_secret(value) {
        return REDACTED.to_string();
    }
    cut(value)
}

fn cut(value: &str) -> String {
    if value.len() <= VALUE_CAP {
        return value.to_string();
    }
    let mut end = VALUE_CAP;
    while end > 0 && !value.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}… ({} bytes)", &value[..end], value.len())
}

/// Whether a value carries a known key prefix followed by a tail random
/// enough to be a real one. Both halves are required: `sk-proj-example` is a
/// placeholder and redacting it would teach a reader that redaction is noise.
fn looks_secret(value: &str) -> bool {
    KEY_PREFIXES.iter().any(|prefix| {
        value.match_indices(prefix).any(|(at, _)| {
            if *prefix == "-----BEGIN" {
                return true;
            }
            let tail: String = value[at + prefix.len()..]
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '-')
                .collect();
            tail.len() >= 16 && entropy(&tail) >= ENTROPY_BAR
        })
    })
}

fn entropy(text: &str) -> f64 {
    if text.is_empty() {
        return 0.0;
    }
    let mut counts = BTreeMap::new();
    for c in text.chars() {
        *counts.entry(c).or_insert(0usize) += 1;
    }
    let total = text.chars().count() as f64;
    -counts
        .values()
        .map(|&n| {
            let p = n as f64 / total;
            p * p.log2()
        })
        .sum::<f64>()
}

/// ISO-8601 UTC to the millisecond, the shape `events-contract.md` §1 fixes.
///
/// Hand-rolled from the civil-date algorithm rather than taken from a date
/// crate: this crate has no such dependency and one line of a stream is not
/// worth acquiring one.
fn now_iso8601() -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    let millis = now.as_millis() as u64;
    let (secs, ms) = (millis / 1000, millis % 1000);
    let (days, rem) = (secs / 86_400, secs % 86_400);
    let (hour, minute, second) = (rem / 3600, (rem % 3600) / 60, rem % 60);
    let (year, month, day) = civil_from_days(days as i64);
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}.{ms:03}Z")
}

/// Howard Hinnant's `civil_from_days`, the standard branch-free conversion
/// from a day count to a calendar date.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every kind the vocabulary names, so a kind added without a decision
    /// about its shape cannot slip past.
    const ALL: [Kind; 22] = [
        Kind::SessionBegin,
        Kind::SessionEnd,
        Kind::TaskBegin,
        Kind::TaskEnd,
        Kind::TurnBegin,
        Kind::TurnEnd,
        Kind::CellSubmit,
        Kind::CellRepair,
        Kind::CellEnd,
        Kind::CommandJudge,
        Kind::CommandEnd,
        Kind::FileChange,
        Kind::HelperBegin,
        Kind::HelperEnd,
        Kind::AgentBegin,
        Kind::AgentEnd,
        Kind::AnswerPropose,
        Kind::AskRaise,
        Kind::SupervisorLook,
        Kind::SandboxMove,
        Kind::ReductionMade,
        Kind::ApprovalRaise,
    ];

    fn stem(kind: Kind) -> &'static str {
        kind.as_str().split('.').next().expect("a dotted kind")
    }

    /// A closing event shares its stem with the one it closes, and the one
    /// it closes opens nothing itself. A reader pairing the stream by `span`
    /// depends on both halves of that.
    #[test]
    fn a_closing_event_shares_its_stem_with_the_one_it_closes() {
        let closers: Vec<Kind> = ALL.into_iter().filter(|k| k.opens().is_some()).collect();
        assert_eq!(
            closers.len(),
            7,
            "session, task, turn, cell, command, helper, agent"
        );
        for closing in closers {
            let opening = closing.opens().expect("filtered to closers");
            assert_eq!(
                stem(opening),
                stem(closing),
                "{} and {} must share a stem",
                opening.as_str(),
                closing.as_str()
            );
            assert!(
                opening.opens().is_none(),
                "{} opens a span and closes nothing",
                opening.as_str()
            );
        }
    }

    /// Every kind is spelled `noun.verb`, and no two share a spelling. The
    /// stream's whole contract with a reader is these strings.
    #[test]
    fn every_kind_is_dotted_and_spelled_once() {
        let mut seen = std::collections::BTreeSet::new();
        for kind in ALL {
            let name = kind.as_str();
            assert_eq!(name.matches('.').count(), 1, "{name} is noun.verb");
            assert!(seen.insert(name), "{name} is spelled twice");
        }
        assert_eq!(seen.len(), ALL.len());
    }

    /// The seams a synchronous reader could refuse at, pinned by name so
    /// that adding a kind forces a decision about which it is — and so that
    /// widening the set is a deliberate act rather than a slip.
    #[test]
    fn exactly_the_seven_named_seams_can_be_refused() {
        let refusable: Vec<&str> = ALL
            .into_iter()
            .filter(|k| k.decides())
            .map(Kind::as_str)
            .collect();
        assert_eq!(
            refusable,
            [
                "cell.submit",
                "cell.repair",
                "command.judge",
                "agent.begin",
                "answer.propose",
                "ask.raise",
                "sandbox.move",
            ],
            "the refusable set is the approved seven"
        );
        for kind in ALL.into_iter().filter(|k| k.decides()) {
            assert!(
                kind.opens().is_none(),
                "{} must be an opening event or a moment, never a close",
                kind.as_str()
            );
        }
    }

    #[test]
    fn a_placeholder_is_not_redacted_and_a_real_shape_is() {
        assert!(
            !looks_secret("sk-proj-example"),
            "a low-entropy placeholder is not a secret"
        );
        assert!(
            looks_secret("sk-proj-Xq7fR2nZ9mKvB4wL8tYpA3sD6gH1jC5e"), // glasshouse:not-a-secret
            "a prefix with a random tail is"
        );
        assert!(
            looks_secret("-----BEGIN RSA PRIVATE KEY-----"), // glasshouse:not-a-secret
            "a private key header needs no entropy bar"
        );
        assert_eq!(safe("sk-proj-Xq7fR2nZ9mKvB4wL8tYpA3sD6gH1jC5e"), REDACTED); // glasshouse:not-a-secret
    }

    #[test]
    fn a_long_value_is_cut_on_a_character_boundary_and_says_so() {
        let long = "ä".repeat(VALUE_CAP);
        let seen = cut(&long);
        assert!(seen.contains("bytes)"), "a cut value names its real size");
        assert!(
            seen.len() < long.len(),
            "and is shorter than what it replaced"
        );
    }

    #[test]
    fn the_clock_renders_a_known_instant() {
        // Checked against the calendar rather than against itself: an
        // off-by-one here would timestamp every event in the stream wrong.
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(20_715), (2026, 9, 19));
        assert_eq!(civil_from_days(20_716), (2026, 9, 20));
        // A leap day, which is where a naive conversion breaks.
        assert_eq!(civil_from_days(19_782), (2024, 2, 29));
    }

    /// A stream that cannot be written must not be able to end a session.
    ///
    /// Two halves, because the failure has two shapes: a destination that
    /// cannot be opened at all, and one that is opened and then goes bad.
    /// Neither may panic, return an error, or reach a caller in any way —
    /// the caller has no error to handle because there is no error type.
    #[test]
    fn a_stream_that_cannot_be_written_never_reaches_the_session() {
        let dir = std::env::temp_dir().join(format!(
            "sterna-observe-unwritable-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).expect("a scratch directory");

        // A rollout inside a directory that does not exist: nothing to open.
        let missing = dir.join("gone").join("rollout.jsonl");
        let observer = Observer::beside(&missing, "sess-x");
        assert!(
            !observer.is_watching(),
            "an unopenable destination leaves the session unwatched"
        );
        observer.session_begin(&missing, &dir);
        observer.task_begin("t", "execute", "auto", "m");
        observer.cell_submit(1, "return 1;", None, &[]);
        observer.cell_end(1, "returned", 0);
        observer.task_end("answered");
        observer.session_end();

        // And one whose lock a panicking thread poisoned. That is the
        // failure this host can actually produce: removing the directory
        // under an open file does not fail a write on Unix, and there is no
        // `/dev/full` here, so a test built on either would assert nothing.
        let rollout = dir.join("rollout.jsonl");
        let live = Observer::beside(&rollout, "sess-y");
        assert!(live.is_watching(), "an openable destination is watched");
        live.session_begin(&rollout, &dir);
        let poisoned = live.clone();
        let _ = std::thread::spawn(move || {
            let sink = poisoned.0.as_ref().expect("watching");
            let _held = sink.file.lock().expect("first lock");
            panic!("poison the lock while holding it");
        })
        .join();
        assert!(
            live.0.as_ref().expect("watching").file.lock().is_err(),
            "the lock is poisoned, which is the condition under test"
        );
        live.task_begin("t", "execute", "auto", "m");
        live.cell_submit(2, "return 2;", None, &[]);
        live.session_end();
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_silent_observer_writes_nothing_and_never_fails() {
        let quiet = Observer::none();
        assert!(!quiet.is_watching());
        quiet.session_end();
        quiet.cell_submit(1, "return 1;", Some("anything"), &[]);
        quiet.command_end("c1.0", 1, "bash", "ok", Some(0));
    }

    #[test]
    fn the_stream_sits_beside_its_rollout() {
        let path = Observer::path_beside(Path::new("/tmp/x/abc.jsonl")).unwrap();
        assert_eq!(path, Path::new("/tmp/x/abc.events.jsonl"));
    }
}
