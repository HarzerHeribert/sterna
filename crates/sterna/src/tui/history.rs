//! Notices kept in the conversation. A status line a person may want to
//! scroll back to (a startup note, an error, a changed setting) is part of
//! the history, drawn where it happened, not a box the next keystroke
//! replaces.
use ratatui::style::{Color, Style};
use ratatui::text::Line;

use super::{MUTED, ScreenState};

/// One notice and where it belongs: after the first `after` messages.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistoryNote {
    pub after: usize,
    pub text: String,
}

impl ScreenState {
    /// Keeps `text` in the conversation after the messages seen so far.
    pub fn note(&mut self, text: impl Into<String>) {
        let text = text.into();
        if !text.trim().is_empty() {
            self.history.push(HistoryNote {
                after: self.messages_seen,
                text,
            });
        }
    }

    /// Keeps `text` the way [`note`](Self::note) does, but as the current
    /// answer to one recurring question rather than another line under the
    /// last one: a note already standing at this position whose text begins
    /// with `prefix` is replaced. Ten drag-selections leave one "Copied"
    /// line, not ten.
    pub fn note_replacing(&mut self, prefix: &str, text: impl Into<String>) {
        if self
            .history
            .last()
            .is_some_and(|note| note.after == self.messages_seen && note.text.starts_with(prefix))
        {
            self.history.pop();
        }
        self.note(text);
    }
}

/// The first words of the lines Sterna's work behind an answer leaves
/// (`session/after.rs`, the gate's notes); [`NoteKind::of`] reads them.
pub const CHECKED: &str = "checked after the answer: ";
pub const LEARNED: &str = "learned: ";
pub const NOTED: &str = "noted, not held: ";

/// What a note is, from its first line, so each lane behind an answer has
/// its own mark and a check reads differently from a notice at a glance.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoteKind {
    Error,
    /// The checker behind the answer found it holds.
    Checked,
    /// The checker could not confirm it, or the gate noted something.
    Flagged,
    /// Lines added to `.sterna/learned.md`.
    Learned,
    Plain,
}

impl NoteKind {
    #[must_use]
    pub fn of(text: &str) -> Self {
        if text.starts_with("ERROR:") {
            Self::Error
        } else if text.starts_with(&format!("{CHECKED}holds")) {
            Self::Checked
        } else if text.starts_with(CHECKED) || text.starts_with(NOTED) {
            Self::Flagged
        } else if text.starts_with(LEARNED) {
            Self::Learned
        } else {
            Self::Plain
        }
    }

    /// The mark before the note's first line.
    #[must_use]
    pub fn mark(self) -> &'static str {
        match self {
            Self::Error => "✕",
            Self::Checked => "✓",
            Self::Flagged => "!",
            Self::Learned => "✎",
            Self::Plain => "·",
        }
    }
}

/// Draws the notes from `next` on that belong at or before message `upto`.
/// An `ERROR:` note is red, a check behind the answer green or yellow, a
/// learned note in the helpers' colour; every other note is muted.
pub(super) fn push_notes(
    lines: &mut Vec<Line<'static>>,
    notes: &[HistoryNote],
    next: &mut usize,
    upto: usize,
) {
    if notes.get(*next).is_some_and(|note| note.after <= upto) && !lines.is_empty() {
        // Outside the open turn's block, which a closer ends.
        lines.push(Line::styled("╰─", Style::default().fg(MUTED)));
        lines.push(Line::from(""));
    }
    while let Some(note) = notes.get(*next).filter(|note| note.after <= upto) {
        let kind = NoteKind::of(&note.text);
        let style = Style::default().fg(match kind {
            NoteKind::Error => Color::Red,
            NoteKind::Checked => Color::Green,
            NoteKind::Flagged => Color::Yellow,
            NoteKind::Learned => Color::Cyan,
            NoteKind::Plain => MUTED,
        });
        for (index, line) in note.text.lines().enumerate() {
            let lead = if index == 0 {
                format!("{} ", kind.mark())
            } else {
                "  ".to_string()
            };
            lines.push(Line::styled(format!("{lead}{line}"), style));
        }
        *next += 1;
    }
}
