//! How a session reads, decided once: the state on a finished cell's card,
//! the line on its bottom edge, and the facts under an answer. The engine
//! sends these words with every record (`wire::Reading`); a client draws
//! them and decides none of them. Every claim here is observed in the
//! cell's own view, never inferred from its source.

use super::wire::{AnswerReading, CellReading, Part, Reading};
use crate::tui::{CellView, Notebook};

/// Every cell's words, and the newest answer's.
#[must_use]
pub fn of(notebook: &Notebook) -> Reading {
    let cells: Vec<CellReading> = notebook
        .cells
        .iter()
        .enumerate()
        .map(|(index, view)| cell(index + 1, view))
        .collect();
    let answer = notebook
        .cells
        .iter()
        .enumerate()
        .rev()
        .find(|(_, view)| view.returned.is_some())
        .map(|(index, view)| answer(index + 1, view));
    Reading { cells, answer }
}

/// One finished cell's card. A cell still running is drawn from the live
/// activity instead (`RUNNING`, `WAITING FOR YOU`), which is not a reading.
#[must_use]
pub fn cell(ordinal: usize, v: &CellView) -> CellReading {
    let (mark, state, tone) = if v.error.is_some() {
        ("✕", "FAILED", "failure")
    } else if v.rolled_back {
        ("↶", "ROLLED BACK", "warning")
    } else if v.execution.is_some() {
        ("✓", "EXECUTED", "success")
    } else {
        ("", "RECORDED", "muted")
    };
    let parts = line(v);
    CellReading {
        cell: ordinal,
        state: state.into(),
        mark: mark.into(),
        tone: tone.into(),
        line: parts.iter().map(|part| part.text.as_str()).collect(),
        parts,
        facts: v.returned.is_some().then(|| answer(ordinal, v)),
    }
}

/// The words on a card's bottom edge: how it ended, and how many files it
/// touched.
#[must_use]
pub fn line(v: &CellView) -> Vec<Part> {
    let mut out = Vec::new();
    if let Some(e) = &v.error {
        out.push(Part::new(format!("✕ {}", e.class), "failure"));
    } else if v.execution.is_some() {
        out.push(Part::new("✓ executed", "success"));
    }
    let files = match changed_files(v) {
        _ if v.rolled_back => "its changes were rolled back".to_string(),
        0 => "no files changed".to_string(),
        1 => "1 file changed".to_string(),
        n => format!("{n} files changed"),
    };
    if !out.is_empty() {
        out.push(Part::new(" · ", "line"));
    }
    out.push(Part::new(files, "muted"));
    out
}

/// The facts under an answer: the files it changed and how, and the calls
/// it made; `complete` or `failed` when there are none.
#[must_use]
pub fn answer(ordinal: usize, v: &CellView) -> AnswerReading {
    let files = changed_files(v);
    let (added, removed) = v.changes.as_deref().map_or((0, 0), count_changes);
    let mut facts = Vec::new();
    if files > 0 {
        facts.push(format!(
            "{files} {} · +{added} −{removed}",
            if files == 1 { "file" } else { "files" }
        ));
    }
    if let Some(calls) = v.call_count.filter(|c| *c > 0) {
        facts.push(format!(
            "{calls} {}",
            if calls == 1 { "call" } else { "calls" }
        ));
    }
    let failed = v.error.is_some();
    AnswerReading {
        cell: ordinal,
        mark: if failed { "✕" } else { "✓" }.into(),
        failed,
        facts: if facts.is_empty() {
            crate::workbench::voice::done_line(failed).to_lowercase()
        } else {
            facts.join(" · ")
        },
    }
}

/// How many files a cell's diff names.
#[must_use]
pub fn changed_files(v: &CellView) -> usize {
    v.changes
        .as_deref()
        .map_or(0, |d| d.lines().filter(|l| l.starts_with("+++ ")).count())
}

/// Lines added and removed in a diff, its headers left out.
#[must_use]
pub fn count_changes(diff: &str) -> (usize, usize) {
    diff.lines().fold((0, 0), |(a, r), line| {
        if line.starts_with("+++") || line.starts_with("---") {
            (a, r)
        } else if line.starts_with('+') {
            (a + 1, r)
        } else if line.starts_with('-') {
            (a, r + 1)
        } else {
            (a, r)
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn executed(changes: &str, calls: usize) -> CellView {
        CellView {
            execution: Some("write · returned".into()),
            changes: Some(changes.into()),
            call_count: Some(calls),
            ..CellView::default()
        }
    }

    #[test]
    fn a_cell_that_changed_one_file_reads_executed_with_that_file() {
        let v = executed("--- a/n.txt\n+++ b/n.txt\n+hello\n", 1);
        let reading = cell(1, &v);
        assert_eq!(reading.state, "EXECUTED");
        assert_eq!(reading.line, "✓ executed · 1 file changed");
        assert!(reading.facts.is_none(), "it returned no answer");
    }

    #[test]
    fn an_answer_counts_its_cells_files_lines_and_calls() {
        let mut v = executed("--- a/n.txt\n+++ b/n.txt\n-old\n+new\n+more\n", 2);
        v.returned = Some("done".into());
        assert_eq!(answer(1, &v).facts, "1 file · +2 −1 · 2 calls");
        let bare = CellView {
            returned: Some("done".into()),
            ..CellView::default()
        };
        assert_eq!(answer(1, &bare).facts, "complete.");
    }

    #[test]
    fn a_rolled_back_cell_says_so_before_it_says_executed() {
        let mut v = executed("+++ b/n.txt\n", 1);
        v.rolled_back = true;
        let reading = cell(3, &v);
        assert_eq!(reading.state, "ROLLED BACK");
        assert_eq!(reading.line, "✓ executed · its changes were rolled back");
    }
}
