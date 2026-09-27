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
            Activity::Idle | Activity::Starting => Self::Idle,
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
        Activity::Failed => "action failed — inspect the cell".into(),
        Activity::Complete => "complete".into(),
    }
}
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
        "Request sent · completion estimate unknown".to_string()
    } else {
        format!("Elapsed {elapsed} · nothing is assumed complete")
    };
    (label, detail)
}
/// The one line on the composer's edge that teaches. It turns with the
/// session -- one more cell, one more notice -- rather than with the clock,
/// so it holds still while someone reads it.
pub fn hint(n: usize) -> &'static str {
    const HINTS: [&str; 8] = [
        "Shift-Tab changes how often Sterna asks before it acts",
        "F2 opens settings · every choice there applies to this session now",
        "Ctrl-T opens the instruments · Esc closes them",
        "Click any control in the top bar to change it",
        "Esc once stops after the current cell · twice cancels the call",
        "Ctrl-B shows or hides the session card · Ctrl-F hides the chrome",
        "/diff opens the last cell's changes · F4 does the same",
        "? lists every key · / for commands · @ for a path in this project",
    ];
    HINTS[n % HINTS.len()]
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
            u8::try_from(tm.tm_hour).ok()
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
    let dirty = git(&["status", "--porcelain"])
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

    #[test]
    fn every_line_states_the_fact_plainly() {
        for a in [
            Activity::Idle,
            Activity::Executing,
            Activity::Waiting,
            Activity::Failed,
            Activity::Complete,
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
