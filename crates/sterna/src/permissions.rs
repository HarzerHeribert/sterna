//! How much runs without asking: the one sandbox setting, and the judgement
//! under it.
//!
//! **One setting, three levels.** `Ask` confirms every edit and command,
//! `Sandboxed` lets everything inside the sandbox run and asks only when a
//! command wants to leave it, and `Full` runs without a sandbox and asks
//! nothing. The level decides both halves -- whether a person is asked and
//! whether Sterna confines what it spawns -- so there is no second axis to
//! keep in step with it.
//!
//! **The answer to one action does not change during a session.** A gate
//! that says no and then yes on a retry teaches retrying (the user,
//! 2026-09-18, on a classifier that "funktioniert meist wenn du es nochmal
//! probierst"). [`Judged`] is that memory: a refusal the person gave stands
//! for the rest of the session unless they take it back.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

/// The three levels, from the most asking to the least.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
pub enum Level {
    /// Every edit and every command is confirmed; reading runs. The sandbox
    /// is on.
    Ask,
    /// Everything inside the sandbox runs. A command that asks to run
    /// outside it is confirmed. The default.
    #[default]
    Sandboxed,
    /// No sandbox, and nothing is confirmed. `permissions.deny` and the
    /// never-grantable set still refuse.
    Full,
}

impl Level {
    /// The word the settings file stores and `--sandbox` takes.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Ask => "ask",
            Self::Sandboxed => "sandboxed",
            Self::Full => "full",
        }
    }

    /// Every level's word, for a settings choice list and a refusal sentence.
    pub const NAMES: [&'static str; 3] = ["ask", "sandboxed", "full"];

    /// Every level, in the order a sheet lists them.
    pub const ALL: [Level; 3] = [Level::Ask, Level::Sandboxed, Level::Full];

    /// The word a screen shows.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Ask => "Ask",
            Self::Sandboxed => "Sandboxed",
            Self::Full => "Full access",
        }
    }

    /// What choosing this level does, in one sentence.
    #[must_use]
    pub fn sentence(self) -> &'static str {
        match self {
            Self::Ask => "Every edit and command asks first. Nothing leaves the project.",
            Self::Sandboxed => "Everything in the project runs. Leaving the sandbox asks.",
            Self::Full => "No sandbox, nothing asks. Refused commands stay refused.",
        }
    }

    /// The same promise, short enough for a sidebar line and the session card.
    #[must_use]
    pub fn asks(self) -> &'static str {
        match self {
            Self::Ask => "asks before every edit and command",
            Self::Sandboxed => "asks only to leave the sandbox",
            Self::Full => "nothing is asked",
        }
    }

    /// Why a call asks when the level alone asks for it, in the screen's
    /// words: "you chose Ask, which asks before every edit and command".
    #[must_use]
    pub fn chosen(self) -> String {
        format!("you chose {}, which {}", self.label(), self.asks())
    }

    /// The notice every route that changes the level prints.
    #[must_use]
    pub fn now(self) -> String {
        format!("Sandbox is now {} · {}", self.label(), self.sentence())
    }

    /// The file's word or the screen's, in any case.
    #[must_use]
    pub fn parse(word: &str) -> Option<Self> {
        match word.trim().to_ascii_lowercase().as_str() {
            "ask" => Some(Self::Ask),
            "sandboxed" => Some(Self::Sandboxed),
            "full" | "full access" => Some(Self::Full),
            _ => None,
        }
    }

    /// Whether this level is unusable without a person at the keyboard: it
    /// confirms the edits and commands every task makes.
    #[must_use]
    pub fn needs_a_person(self) -> bool {
        self == Self::Ask
    }

    /// Whether Sterna confines what it spawns on this level.
    #[must_use]
    pub fn confined(self) -> bool {
        self != Self::Full
    }

    fn code(self) -> u8 {
        match self {
            Self::Ask => 0,
            Self::Sandboxed => 1,
            Self::Full => 2,
        }
    }

    fn from_code(code: u8) -> Self {
        match code {
            0 => Self::Ask,
            2 => Self::Full,
            _ => Self::Sandboxed,
        }
    }
}

/// What the level says about one call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    /// Run it without asking.
    Runs,
    /// Put it in front of the person, with this reason shown beside it.
    Ask(String),
}

/// The live level: readable by the gate on the session thread, writable by
/// the key handler on the UI thread.
///
/// A person notices the wrong level exactly when something they did not
/// expect stops to ask them, which is while a task is running, so the level
/// is an atomic rather than an input the turn loop reads between turns.
/// Confinement follows it from the next request: a request's profile is
/// built once, when the request starts.
#[derive(Clone, Debug)]
pub struct LiveLevel {
    level: Arc<AtomicU8>,
    /// Every change, for the rollout. Drained by the session at a turn
    /// boundary: the UI thread that makes the change does no file I/O.
    moves: Arc<Mutex<Vec<Move>>>,
    /// True when nobody can answer a confirmation: then a question is a
    /// refusal, never a silent yes.
    unattended: bool,
}

/// One recorded change.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Move {
    pub from: Level,
    pub to: Level,
    pub at: SystemTime,
}

impl LiveLevel {
    #[must_use]
    pub fn new(level: Level) -> Self {
        Self {
            level: Arc::new(AtomicU8::new(level.code())),
            moves: Arc::new(Mutex::new(Vec::new())),
            unattended: false,
        }
    }

    /// The same level, for a session with no terminal to ask at.
    #[must_use]
    pub fn unattended(mut self) -> Self {
        self.unattended = true;
        self
    }

    #[must_use]
    pub fn level(&self) -> Level {
        Level::from_code(self.level.load(Ordering::SeqCst))
    }

    #[must_use]
    pub fn is_unattended(&self) -> bool {
        self.unattended
    }

    /// Changes to `level` and records it. Returns the change, or `None` when
    /// the level is already the current one.
    pub fn set(&self, level: Level) -> Option<Move> {
        let from = Level::from_code(self.level.swap(level.code(), Ordering::SeqCst));
        if from == level {
            return None;
        }
        let moved = Move {
            from,
            to: level,
            at: SystemTime::now(),
        };
        if let Ok(mut moves) = self.moves.lock() {
            moves.push(moved.clone());
        }
        Some(moved)
    }

    /// Every change since the last drain, for the rollout.
    pub fn drain_moves(&self) -> Vec<Move> {
        self.moves
            .lock()
            .map(|mut moves| std::mem::take(&mut *moves))
            .unwrap_or_default()
    }
}

impl Default for LiveLevel {
    fn default() -> Self {
        Self::new(Level::default())
    }
}

/// The argument a command call carries when it asks to run outside the
/// sandbox; its value is the model's reason, shown to the person.
pub const OUTSIDE: &str = "outside";

/// The reason a command call gave for running outside the sandbox -- or a
/// fetch for reaching a host outside the allowed list -- or `None` when it
/// did not ask to.
#[must_use]
pub fn outside_reason(tool: &str, arguments: &BTreeMap<String, String>) -> Option<String> {
    if tool != "bash" && tool != crate::approval::WEB_FETCH {
        return None;
    }
    arguments
        .get(OUTSIDE)
        .map(|reason| reason.trim())
        .filter(|reason| !reason.is_empty())
        .map(str::to_string)
}

/// The level's answer for one call the profile already admitted.
///
/// `effectful` is whether the tool changes anything (`registry::Purity`);
/// `pre_approved` answers whether a command line matches one of the
/// person's own `Bash(...)` patterns in `permissions.allow`, which run
/// without asking on `Ask`.
#[must_use]
pub fn judge(
    level: Level,
    tool: &str,
    arguments: &BTreeMap<String, String>,
    effectful: bool,
    pre_approved: &dyn Fn(&str) -> bool,
) -> Verdict {
    if level == Level::Full {
        return Verdict::Runs;
    }
    if let Some(reason) = outside_reason(tool, arguments) {
        return Verdict::Ask(if tool == "bash" {
            format!("asks to run outside the sandbox: {reason}")
        } else {
            reason
        });
    }
    match level {
        Level::Sandboxed | Level::Full => Verdict::Runs,
        Level::Ask if !effectful => Verdict::Runs,
        Level::Ask => match arguments.get("command") {
            Some(line) if pre_approved(line) => Verdict::Runs,
            Some(_) => Verdict::Ask(level.chosen()),
            None => Verdict::Ask(level.chosen()),
        },
    }
}

/// The session's memory of what has already been answered.
///
/// Keyed by the exact action, so two different command lines are two
/// questions and the same one twice is one.
pub struct Judged<K: Ord>(Mutex<BTreeMap<K, bool>>);

impl<K: Ord> Default for Judged<K> {
    fn default() -> Self {
        Self(Mutex::new(BTreeMap::new()))
    }
}

impl<K: Ord + Clone> Judged<K> {
    /// The remembered answer, or `None` when this action has not been
    /// answered yet.
    pub fn answer(&self, key: &K) -> Option<bool> {
        self.0
            .lock()
            .ok()
            .and_then(|answers| answers.get(key).copied())
    }

    /// Remembers `answer` for `key`. A second call with the same key does
    /// not change it: the first answer is the session's answer.
    pub fn remember(&self, key: K, answer: bool) {
        if let Ok(mut answers) = self.0.lock() {
            answers.entry(key).or_insert(answer);
        }
    }

    pub fn len(&self) -> usize {
        self.0.lock().map(|answers| answers.len()).unwrap_or(0)
    }

    /// Every remembered answer.
    pub fn entries(&self) -> Vec<(K, bool)> {
        self.0
            .lock()
            .map(|answers| answers.iter().map(|(k, v)| (k.clone(), *v)).collect())
            .unwrap_or_default()
    }

    /// Forgets the answers whose key matches: the person took them back,
    /// so the next identical question is asked again.
    pub fn forget_where(&self, matches: impl Fn(&K) -> bool) {
        if let Ok(mut answers) = self.0.lock() {
            answers.retain(|key, _| !matches(key));
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect()
    }

    fn never(_: &str) -> bool {
        false
    }

    /// Each level answers an edit, a command, a read and a request to leave
    /// the sandbox the way its sentence promises.
    #[test]
    fn every_level_answers_as_its_sentence_says() {
        let edit = args(&[("path", "src/lib.rs")]);
        let command = args(&[("command", "cargo test")]);
        let read = args(&[("path", "src/lib.rs")]);
        let outside = args(&[("command", "curl x"), (OUTSIDE, "needs the api")]);
        let asks = |v: &Verdict| matches!(v, Verdict::Ask(_));

        assert!(asks(&judge(Level::Ask, "edit", &edit, true, &never)));
        assert!(asks(&judge(Level::Ask, "bash", &command, true, &never)));
        assert_eq!(
            judge(Level::Ask, "read", &read, false, &never),
            Verdict::Runs
        );
        assert!(asks(&judge(Level::Ask, "bash", &outside, true, &never)));

        assert_eq!(
            judge(Level::Sandboxed, "edit", &edit, true, &never),
            Verdict::Runs
        );
        assert_eq!(
            judge(Level::Sandboxed, "bash", &command, true, &never),
            Verdict::Runs
        );
        let Verdict::Ask(why) = judge(Level::Sandboxed, "bash", &outside, true, &never) else {
            panic!("leaving the sandbox must ask on Sandboxed");
        };
        assert!(why.contains("needs the api"), "{why}");

        for call in [&edit, &command, &outside] {
            assert_eq!(
                judge(Level::Full, "bash", call, true, &never),
                Verdict::Runs
            );
        }
    }

    /// The person's own `Bash(...)` patterns run without asking on `Ask`,
    /// and only commands they name.
    #[test]
    fn a_pre_approved_command_runs_on_ask() {
        let approved = |line: &str| line == "cargo test";
        assert_eq!(
            judge(
                Level::Ask,
                "bash",
                &args(&[("command", "cargo test")]),
                true,
                &approved
            ),
            Verdict::Runs
        );
        assert!(matches!(
            judge(
                Level::Ask,
                "bash",
                &args(&[("command", "rm -rf x")]),
                true,
                &approved
            ),
            Verdict::Ask(_)
        ));
    }

    /// An empty reason is not a request to leave the sandbox.
    #[test]
    fn a_blank_reason_does_not_leave_the_sandbox() {
        let blank = args(&[("command", "ls"), (OUTSIDE, "  ")]);
        assert_eq!(outside_reason("bash", &blank), None);
        assert_eq!(
            judge(Level::Sandboxed, "bash", &blank, true, &never),
            Verdict::Runs
        );
        assert_eq!(outside_reason("read", &args(&[(OUTSIDE, "x")])), None);
    }

    #[test]
    fn a_level_reads_its_own_words_and_labels() {
        for level in Level::ALL {
            assert_eq!(Level::parse(level.name()), Some(level));
            assert_eq!(Level::parse(level.label()), Some(level));
        }
        assert_eq!(
            Level::parse("manual"),
            None,
            "old rung words are migrated, not parsed"
        );
        assert_eq!(Level::default(), Level::Sandboxed);
        assert!(Level::Ask.needs_a_person());
        assert!(!Level::Full.confined());
    }

    #[test]
    fn a_move_is_recorded_once_and_drained_once() {
        let live = LiveLevel::new(Level::Sandboxed);
        assert!(live.set(Level::Sandboxed).is_none());
        let moved = live.set(Level::Ask).expect("a real change");
        assert_eq!((moved.from, moved.to), (Level::Sandboxed, Level::Ask));
        assert_eq!(live.level(), Level::Ask);
        assert_eq!(live.drain_moves().len(), 1);
        assert!(live.drain_moves().is_empty());
    }

    #[test]
    fn the_first_answer_is_the_sessions_answer() {
        let judged = Judged::default();
        judged.remember("a".to_string(), false);
        judged.remember("a".to_string(), true);
        assert_eq!(judged.answer(&"a".to_string()), Some(false));
        judged.forget_where(|k| k == "a");
        assert!(judged.is_empty());
    }
}
