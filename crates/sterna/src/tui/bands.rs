//! The three bands of the transcript a reader navigates by.
//!
//! `tui.rs` renders every block of the conversation the same way -- a
//! `╭─ LABEL` header, a body, a `╰─` closer -- and this module turns that
//! skeleton into what is drawn: a framed request, a hushed answer, a docked
//! cell header, and nothing for the rest. It was cut out of `tui.rs` for the
//! size ratchet (Phase 59) and is a pure move plus the band it adds.

use ratatui::style::{Modifier, Style};
use ratatui::text::Line;

use super::{Activity, ScreenState};

/// What a block of transcript is, read off the label `turn_header` wrote.
///
/// The renderer emits every block the same way — `╭─ LABEL`, its body, `╰─` —
/// and this is where the three that a reader navigates by are told apart.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Band {
    /// The person's own words. Framed, because a reader scrolling back is
    /// looking for *what was asked*, and a request that reads like output is
    /// the thing they have to hunt for.
    Request,
    /// The model's reply, and the end of a turn.
    Answer,
    /// A cell header, which keeps the dock bar it has always had.
    Cell,
    /// Everything else: outputs, tables, previews, the scout's lane.
    Plain,
}

impl Band {
    fn of(label: &str) -> Self {
        // `STERNA / CODE` is a cell's own header and is matched first, so the
        // `STERNA` arm below cannot claim it.
        if label.contains("Cell ")
            || label.contains("CELL ")
            || label.contains("Action failed")
            || label.contains("STERNA / CODE")
        {
            Self::Cell
        } else if label == "USER" {
            Self::Request
        } else if label.starts_with("STERNA") {
            Self::Answer
        } else {
            Self::Plain
        }
    }
}

/// Whether this row is a cell's poster field.
///
/// Matched on the signage rather than on a glyph run: the runs are what a
/// narrow width sheds first (`poster::field_header`), and a field that lost
/// them is still the row that opens the band.
fn is_field(line: &Line<'static>) -> bool {
    line.spans
        .iter()
        .any(|span| span.content.contains("/ CELL "))
}

/// A rule that fills the row: the label, then the line drawn out to `width`.
///
/// **A divider a reader can see.** Until 2026-09-18 a block opened with its
/// bare label and closed with an empty line, so a long transcript was one
/// column of text with nothing marking where a turn began or ended — the
/// screen that prompted this had no separator anywhere in it.
pub(super) fn rule(label: &str, glyph: char, width: usize) -> String {
    let head = if label.is_empty() {
        String::new()
    } else {
        format!(" {label} ")
    };
    let drawn = width.saturating_sub(head.chars().count() + 2);
    let mut out = String::from(glyph);
    out.push(glyph);
    out.push_str(&head);
    for _ in 0..drawn {
        out.push(glyph);
    }
    out
}

/// Turns the renderer's `╭─`/`╰─` skeleton into what is drawn.
///
/// **One line in, one line out.** `headers` carries line indices for hit
/// testing, so this rewrites in place and never inserts or removes a row.
pub(super) fn decorate(content: &mut [Line<'static>], state: &ScreenState, width: u16) {
    let width = usize::from(width);
    let mut active = false;
    let mut kind = Band::Plain;
    for line in content.iter_mut() {
        let mut header = false;
        // A poster field is already finished: it opens no `╭─` skeleton and
        // needs no rule drawn into it. It still opens a band, so the rows
        // beneath it are tinted as the cell's and not as the block before it
        // — and it is where the live *running* word belongs, because the
        // field is the only row that says what state the cell is in.
        if is_field(line) {
            if state.activity == Activity::Executing {
                for span in line.spans.iter_mut() {
                    if span.content.contains(" PREPARING ") {
                        span.content = span
                            .content
                            .replace(
                                " PREPARING ",
                                &format!(
                                    " {} RUNNING ",
                                    Activity::Executing.indicator(state.animation_frame)
                                ),
                            )
                            .into();
                    }
                }
            }
            kind = Band::Cell;
            active = true;
            continue;
        }
        if let Some(first) = line.spans.first_mut() {
            if first.content.starts_with("╭─ ") {
                if state.activity == Activity::Executing
                    && first.content.contains("◇ Cell preparing · nothing has run")
                {
                    first.content = first
                        .content
                        .replace(
                            "◇ Cell preparing · nothing has run",
                            &format!(
                                "{} Cell running",
                                Activity::Executing.indicator(state.animation_frame)
                            ),
                        )
                        .into();
                }
                let label = first.content.trim_start_matches("╭─ ").to_string();
                kind = Band::of(&label);
                first.content = match kind {
                    Band::Request => rule(&label, '━', width),
                    Band::Answer => rule(&label, '─', width),
                    Band::Cell | Band::Plain => label,
                }
                .into();
                header = true;
                active = true;
            } else if first.content == "╰─" {
                // The closer belongs to the block that just ended.
                *line = match kind {
                    Band::Request => Line::styled(
                        rule("", '━', width),
                        Style::default()
                            .fg(state.theme.accent())
                            .add_modifier(Modifier::BOLD)
                            .bg(state.theme.backlight()),
                    ),
                    Band::Answer => Line::default().style(Style::default().bg(state.theme.hush())),
                    Band::Cell | Band::Plain => Line::default(),
                };
                active = false;
                kind = Band::Plain;
                continue;
            }
        }
        if header {
            line.style = match kind {
                Band::Request => Style::default()
                    .fg(state.theme.accent())
                    .add_modifier(Modifier::BOLD)
                    .bg(state.theme.backlight()),
                Band::Answer => line.style.bg(state.theme.hush()),
                Band::Cell => line.style.bg(state.theme.dock()),
                Band::Plain => line.style.bg(state.theme.backlight()),
            };
            continue;
        }
        if active {
            line.style = line.style.bg(match kind {
                // The whole reply is hushed, not just its header: the block
                // is what the eye finds, and a tint on one row is a label.
                Band::Answer => state.theme.hush(),
                _ => state.theme.backlight(),
            });
        }
    }
}
