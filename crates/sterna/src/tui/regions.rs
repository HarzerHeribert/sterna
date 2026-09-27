//! How a bounded region of text is drawn into the transcript column.
//!
//! One responsibility, moved out of `tui.rs` whole (2026-09-17) when the cell
//! descriptor pushed that file over its 2500-line ceiling. The user's rule:
//! *"Pushing a file past 2.5k lines is a split signal not a shorten signal."*
//! Nothing here changed in the move.
//!
//! **Every fold says how much it folded and which key opens it**
//! (`docs/workbench.md` rule 4): a region that silently drops
//! lines is the screen's version of a cap that drops work.

use ratatui::style::{Color, Style};
use ratatui::text::Line;

use super::{CellError, MUTED, NO_OUTPUTS, turn_header};

/// Display only. The conversation and executable source remain byte-for-byte intact.
/// Expanded view (Ctrl-O) shows original code, as do cells with source-position errors.
pub(super) fn push_changes(lines: &mut Vec<Line<'static>>, changes: &str, compact: bool) {
    turn_header(lines, "CHANGES OBSERVED".into(), Color::LightCyan);
    push_diff(lines, changes, if compact { 18 } else { usize::MAX });
}

/// A unified diff's body: a gutter by line kind, bounded, control characters
/// neutralised.
///
/// Split out of [`push_changes`] so the cell's own return value can render a
/// diff the same way rather than through a second renderer that could drift
/// from it. The caller supplies the heading, because a diff that arrived as
/// a field of a returned object is not a "change observed".
pub(super) fn push_diff(lines: &mut Vec<Line<'static>>, changes: &str, limit: usize) {
    for line in changes.lines().take(limit) {
        let color = if line.starts_with("+++") || line.starts_with("---") {
            Color::LightCyan
        } else if line.starts_with('+') {
            Color::LightGreen
        } else if line.starts_with('-') {
            Color::LightRed
        } else {
            MUTED
        };
        // A diff is data: terminal controls must never affect rendering.
        let text: String = line
            .chars()
            .map(|c| if c.is_control() { ' ' } else { c })
            .collect();
        lines.push(Line::styled(text, Style::default().fg(color)));
    }
    let remaining = changes.lines().count().saturating_sub(limit);
    if remaining > 0 {
        lines.push(Line::styled(
            format!("… {remaining} more diff lines · Ctrl-O expands"),
            Style::default().fg(MUTED),
        ));
    }
}

/// A throw's own region: the class and message, then the position when the
/// runtime attributed one. An unattributed throw gets no position line rather
/// than a zero -- the sidebar's rule about absent figures, applied here.
pub(super) fn push_error_region(lines: &mut Vec<Line<'static>>, error: &CellError) {
    lines.push(Line::from(format!("{}: {}", error.class, error.message)));
    if let (Some(line), Some(column)) = (error.line, error.column) {
        lines.push(Line::from(format!("line {line}, column {column}")));
    }
}

/// Pushes `text` one line per line, so a program or a multi-line preview is
/// as many rows as it has lines rather than one row carrying its newlines.
/// Empty text still pushes one empty row, so a region never disappears.
pub(super) fn push_text_region(lines: &mut Vec<Line<'static>>, text: &str) {
    if text.is_empty() {
        lines.push(Line::from(String::new()));
        return;
    }
    for line in text.lines() {
        lines.push(Line::from(line.to_string()));
    }
}

/// Draws `table` -- `render_table`'s own return value, line for line -- or
/// `NO_OUTPUTS` when it is empty. The only place a handle's rendering enters
/// the conversation column; nothing else here previews a value on its own.
pub(super) fn push_output_region(lines: &mut Vec<Line<'static>>, table: String, compact: bool) {
    if table.is_empty() {
        lines.push(Line::from(NO_OUTPUTS));
        return;
    }
    push_folded_region(lines, &table, 6, compact);
}

pub(super) fn push_folded_region(
    lines: &mut Vec<Line<'static>>,
    text: &str,
    limit: usize,
    compact: bool,
) {
    if !compact || text.lines().count() <= limit {
        push_text_region(lines, text);
        return;
    }
    for line in text.lines().take(limit) {
        lines.push(Line::from(line.to_string()));
    }
    lines.push(Line::styled(
        format!(
            "… {} more lines · Ctrl-O expands",
            text.lines().count() - limit
        ),
        Style::default().fg(MUTED),
    ));
}
