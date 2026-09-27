//! Every line Sterna says, in one plain voice, whatever the theme.
//!
//! **The words live in one place so they cannot drift.** Each line states a
//! fact and nothing else. The parrot of a parrot theme is decoration --
//! reduced motion holds it still, and nothing on screen carries state
//! through the bird alone.
use crate::tui::Activity;

/// Which of six states the session is in, for the card's mark and the
/// parrot's mood. It is read off the session's activity, never stored, so
/// it cannot go stale.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Face {
    Idle,
    Thinking,
    Working,
    Done,
    Asking,
    Oops,
}
impl Face {
    pub fn of(activity: Activity, asking: bool) -> Self {
        if asking {
            return Self::Asking;
        }
        match activity {
            Activity::Idle | Activity::Starting | Activity::Stopped(_) => Self::Idle,
            Activity::AwaitingYou => Self::Asking,
            Activity::Thinking | Activity::Waiting | Activity::Compacting | Activity::Searching => {
                Self::Thinking
            }
            Activity::Streaming | Activity::Executing => Self::Working,
            Activity::Complete => Self::Done,
            Activity::Failed => Self::Oops,
        }
    }
}

/// The label on a turn of the person's, and on one of Sterna's.
pub const YOU: &str = "you";
pub const STERNA: &str = "sterna";

/// The card's first line. The header already names the project, so the
/// greeting does not.
pub fn greeting(hour: Option<u8>) -> String {
    match hour {
        Some(5..=11) => "Good morning. What should we build?".into(),
        Some(12..=17) => "Good afternoon. What should we build?".into(),
        Some(18..=22) => "Good evening. What should we build?".into(),
        Some(_) | None => "What should we build?".into(),
    }
}
/// The line under the greeting on an empty conversation.
pub const INVITATION: &str = "Describe a task, or pick one of these:";
/// What the composer says when it is empty.
pub const PLACEHOLDER: &str = "Describe the next step — a message or / for commands";
/// The composer edge's word for what the session is doing right now.
///
/// A person watching this is asking *is it alive and on what*; the answer is
/// the activity's own name, never a claim about progress toward an end
/// nobody can see.
pub fn status(activity: Activity, cell: Option<usize>, writing_cell: bool) -> String {
    match activity {
        Activity::Streaming if writing_cell => match cell {
            Some(n) => format!("writing cell {n:03}"),
            None => "writing a cell".into(),
        },
        Activity::Idle => "ready".into(),
        Activity::Starting => "starting session".into(),
        Activity::Thinking => "thinking".into(),
        Activity::Streaming => "receiving response".into(),
        Activity::Executing => match cell {
            Some(n) => format!("executing cell {n:03}"),
            None => "executing cell".into(),
        },
        Activity::Searching => "searching".into(),
        Activity::Waiting => "waiting on a response · estimate unknown".into(),
        Activity::Compacting => "compacting · preparing bounded context".into(),
        Activity::AwaitingYou => "waiting for you".into(),
        // A failed request has no cell; a failed cell says so on its card.
        Activity::Failed => "failed · the message above says why".into(),
        Activity::Complete => "complete".into(),
        Activity::Stopped(crate::tui::Stopper::You) => "stopped · what ran stands".into(),
        Activity::Stopped(crate::tui::Stopper::Interrupt) => {
            "stopped by Ctrl-C · what ran stands".into()
        }
    }
}
/// The status while the answer is in and its check is still running: the
/// turn is not complete until the check has had its say.
pub const CHECKING: &str = "answered · checking it";
/// The one sentence for a control that waits for the turn to end.
pub const BETWEEN_TURNS: &str = "Available when this turn ends · Esc stops it";
/// A model, mode or effort chosen while a turn runs.
pub const NEXT_REQUEST: &str = "Saved · applies from this turn's next request";
/// A message held until the session is free.
pub const QUEUED: &str = "Queued for when this turn ends · Esc takes the last one back";
/// Ctrl-C over a draft, between turns.
pub const DRAFT_CLEARED: &str = "Draft cleared · Ctrl-Z brings it back";
/// Ctrl-C on an empty composer, between turns.
pub const QUIT_ARMED: &str = "Ctrl-C again within 2 s to quit";
/// Ctrl-D while the session is busy.
pub const CTRL_D_BUSY: &str = "Ctrl-D quits between turns · /exit stops this turn and quits";
/// Ctrl-C while a turn runs.
pub const CTRL_C_STOPPING: &str = "Stopping · Ctrl-C again within 2 s quits";
/// The three lines beside the bird inside a running cell: what is happening,
/// how long it has been, and at whose cost.
pub fn working(activity: Activity, helper_waiting: bool, elapsed: &str) -> (&'static str, String) {
    let label = match activity {
        Activity::Executing => "Executing this cell",
        Activity::Waiting => "Waiting on the provider",
        Activity::Searching => "Searching",
        Activity::Compacting => "Preparing bounded context",
        _ if helper_waiting => "Little helper working",
        _ => "Model is responding",
    };
    let detail = if helper_waiting {
        "Asked · waiting for its answer".to_string()
    } else {
        format!("Elapsed {elapsed} · nothing is assumed complete")
    };
    (label, detail)
}
/// The one line on the composer's edge that teaches. It turns with the
/// session -- one more cell, one more notice -- rather than with the clock,
/// so it holds still while someone reads it. A hint that would not apply
/// now is not offered: Escape stops only a running turn, and there is no
/// diff before a cell has changed something.
pub fn hint(n: usize, busy: bool, changed: bool) -> &'static str {
    const HINTS: [(&str, Needs); 8] = [
        (
            "Shift-Tab changes how often Sterna asks before it acts",
            Needs::Nothing,
        ),
        (
            "F2 opens settings · choices save themselves; most apply now",
            Needs::Nothing,
        ),
        ("Ctrl-T opens telemetry · Esc closes it", Needs::Nothing),
        (
            "Click any control in the top bar to change it",
            Needs::Nothing,
        ),
        (
            "Esc once stops after the current cell · twice cancels the call",
            Needs::Turn,
        ),
        (
            "Ctrl-B shows or hides the sidebar · Ctrl-F hides the chrome",
            Needs::Nothing,
        ),
        (
            "/diff opens the last cell's changes · F4 does the same",
            Needs::Change,
        ),
        (
            "? lists every key · / for commands · @ for a path in this project",
            Needs::Nothing,
        ),
    ];
    let offered: Vec<&str> = HINTS
        .iter()
        .filter(|(_, needs)| match needs {
            Needs::Nothing => true,
            Needs::Turn => busy,
            Needs::Change => changed,
        })
        .map(|(hint, _)| *hint)
        .collect();
    offered[n % offered.len()]
}

/// What a hint needs before it applies.
#[derive(Clone, Copy)]
enum Needs {
    Nothing,
    Turn,
    Change,
}
/// The row for work still running behind the answer: `check` is the fresh
/// checker, `learn` the notes writer. The answer above it stands either way.
pub fn behind(lane: &str) -> String {
    match lane {
        "check" => "checking the answer · it stands as given · off in /settings".into(),
        "learn" => "writing learned notes · .sterna/learned.md".into(),
        other => format!("{other} · behind the answer"),
    }
}
/// The first line of a finished turn's block, when the model returned no
/// words of its own to put there.
pub fn done_line(failed: bool) -> &'static str {
    if failed { "Failed." } else { "Complete." }
}
/// Labels for the suggestion chips on an empty conversation, in the order
/// they are offered. Each is the chip's text and the message it types.
pub fn suggestions(
    last_commit: Option<&str>,
    dirty_files: usize,
    has_tests: bool,
) -> Vec<(String, String)> {
    let mut out = Vec::new();
    if let Some(subject) = last_commit {
        let short = super::document::clip(subject, 34);
        out.push((
            format!("pick up: {short}"),
            format!(
                "Pick up where the last commit left off (\"{subject}\"). What is the next step?"
            ),
        ));
    }
    if dirty_files > 0 {
        out.push((
            format!(
                "review {dirty_files} uncommitted {}",
                if dirty_files == 1 {
                    "change"
                } else {
                    "changes"
                }
            ),
            "Review my uncommitted changes and tell me what is unfinished.".into(),
        ));
    }
    if has_tests {
        out.push((
            "run the tests".into(),
            "Run the tests and tell me what fails.".into(),
        ));
    }
    if out.is_empty() {
        out.push((
            "explore this project".into(),
            "Give me a tour of this project: layout, entry points, how to run and test it.".into(),
        ));
    }
    out
}

/// The local hour, for the greeting. `None` where the platform cannot say,
/// and the greeting then simply has no time of day in it.
pub fn local_hour() -> Option<u8> {
    local_time().map(|(hour, _)| hour)
}

/// The local time as `HH:MM`, where the platform can say.
pub fn local_hhmm() -> Option<String> {
    local_time().map(|(hour, minute)| format!("{hour:02}:{minute:02}"))
}

fn local_time() -> Option<(u8, u8)> {
    #[cfg(unix)]
    {
        // SAFETY: `localtime_r` writes only into the `tm` we hand it, and
        // `time` takes a null pointer to mean "now".
        unsafe {
            let now = libc::time(std::ptr::null_mut());
            let mut tm: libc::tm = std::mem::zeroed();
            if libc::localtime_r(&now, &mut tm).is_null() {
                return None;
            }
            Some((
                u8::try_from(tm.tm_hour).ok()?,
                u8::try_from(tm.tm_min).ok()?,
            ))
        }
    }
    #[cfg(not(unix))]
    {
        None
    }
}
/// What the opening offers to do, read from the project itself: the last
/// commit's subject, whether anything is uncommitted, whether there is
/// something to test. Best effort and quick; a project without git, or a
/// machine without it, gets the one suggestion that needs nothing.
pub fn project_suggestions(root: &std::path::Path) -> Vec<(String, String)> {
    let git = |args: &[&str]| -> Option<String> {
        let out = std::process::Command::new("git")
            .arg("-C")
            .arg(root)
            .args(args)
            .stdin(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .output()
            .ok()?;
        out.status
            .success()
            .then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
    };
    let last_commit = git(&["log", "-1", "--format=%s"]).filter(|s| !s.is_empty());
    // Sterna's own folder is not the person's change.
    let dirty = git(&["status", "--porcelain", "--", ".", ":(exclude).sterna"])
        .map(|s| s.lines().filter(|l| !l.trim().is_empty()).count())
        .unwrap_or(0);
    let has_tests = [
        "Cargo.toml",
        "package.json",
        "pyproject.toml",
        "Makefile",
        "go.mod",
    ]
    .iter()
    .any(|f| root.join(f).exists());
    suggestions(last_commit.as_deref(), dirty, has_tests)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Sterna's own folder in a project is not the person's change: a clean
    /// repository with only `.sterna/` in it offers no review.
    #[test]
    fn sternas_own_folder_is_not_an_uncommitted_change() {
        let root = std::env::temp_dir().join(format!("sterna-dirty-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join(".sterna")).unwrap();
        let git = |args: &[&str]| {
            std::process::Command::new("git")
                .arg("-C")
                .arg(&root)
                .args(args)
                .output()
                .is_ok_and(|out| out.status.success())
        };
        if !git(&["init", "-q"]) {
            return;
        }
        std::fs::write(root.join(".sterna").join("config.toml"), "[ui]\n").unwrap();
        let offered = project_suggestions(&root);
        assert!(
            !offered
                .iter()
                .any(|(label, _)| label.contains("uncommitted")),
            "{offered:?}"
        );
        std::fs::write(root.join("notes.txt"), "mine\n").unwrap();
        let offered = project_suggestions(&root);
        assert!(
            offered
                .iter()
                .any(|(label, _)| label == "review 1 uncommitted change"),
            "{offered:?}"
        );
        std::fs::remove_dir_all(&root).unwrap();
    }

    /// A hint that would not apply now is not offered.
    #[test]
    fn a_hint_applies_to_the_moment() {
        for n in 0..16 {
            let idle = hint(n, false, false);
            assert!(!idle.starts_with("Esc"), "Esc stops nothing while idle");
            assert!(!idle.starts_with("/diff"), "no diff before a change");
        }
        assert!((0..16).any(|n| hint(n, true, true).starts_with("Esc")));
        assert!((0..16).any(|n| hint(n, true, true).starts_with("/diff")));
    }

    #[test]
    fn every_line_states_the_fact_plainly() {
        for a in [
            Activity::Idle,
            Activity::Executing,
            Activity::Waiting,
            Activity::Failed,
            Activity::Complete,
            Activity::AwaitingYou,
            Activity::Stopped(crate::tui::Stopper::You),
        ] {
            assert!(!status(a, Some(2), false).is_empty());
        }
        assert!(status(Activity::Executing, Some(7), false).contains("007"));
        assert!(status(Activity::Streaming, Some(4), true).contains("004"));
        assert_eq!(greeting(Some(14)), "Good afternoon. What should we build?");
        assert_eq!(greeting(Some(2)), "What should we build?");
        assert_eq!(greeting(None), "What should we build?");
    }

    #[test]
    fn suggestions_come_from_the_project_and_never_from_nothing() {
        let s = suggestions(Some("fix the guard"), 2, true);
        assert_eq!(s.len(), 3);
        assert!(s[0].0.starts_with("pick up: fix the guard"));
        assert!(s[1].0.contains("2 uncommitted changes"));
        let none = suggestions(None, 0, false);
        assert_eq!(none.len(), 1);
        assert!(none[0].1.contains("tour"));
    }
}
