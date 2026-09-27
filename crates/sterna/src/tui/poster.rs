//! The transcript's poster vocabulary: filled fields, signage and rows.
//!
//! **The sidebar already spoke this language and the transcript did not.**
//! `tui/telemetry.rs` draws numbered technical panels -- `02 / CONTEXT
//! WINDOW`, `03 / TASK SPEND` -- with uppercase labels, rules and one accent,
//! while the column beside it drew box-drawing corners around raw JSON. The
//! user, looking at their own screen mid-run (2026-09-19): *"the colouring
//! and framing are not human readable anyways"*, and on the direction, *"more
//! artistic kind like the game Marathon does"*. Nothing here is a second
//! design language; it is that one, extended leftward.
//!
//! **Every field degrades rather than clips.** A filled header is a lead run,
//! two labels and a tail run; when the width cannot hold all of it the runs
//! are what goes, never the labels, because the labels carry which cell this
//! is and what became of it. [`field_header`] states the order it sheds in.
//!
//! **Motion never carries information.** A `tick` of zero -- what
//! `reduced_motion` passes -- renders every field at full strength, so
//! `/motion off` loses decoration and nothing else. That is the same rule
//! `tick_helper_clocks` keeps for a running call's elapsed time.

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

use super::{MUTED, Theme};

/// The glyph a filled header's runs are made of.
const FIELD: char = '█';
/// The glyph a call bar's runs are made of -- one step down the dither ramp
/// from [`FIELD`], so a bar reads as subordinate to the header above it
/// without needing a second colour.
const BAR: char = '▓';
/// The dither ramp, darkest first. A raster reveal walks it.
const RAMP: [char; 4] = ['░', '▒', '▓', '█'];
/// The shortest run worth drawing. Below this a run is dropped whole rather
/// than rendered as one or two orphaned blocks.
const RUN: usize = 3;
/// How many lines the headline half of an intent may take.
const HEADLINE_LINES: usize = 2;
/// How many lines the qualifying half may take.
const QUALIFIER_LINES: usize = 2;

/// What became of a cell, as the header field says it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum State {
    /// Nothing has run yet.
    Preparing,
    /// Host calls ran and the cell ended without a throw.
    Executed,
    /// The cell threw; the field inverts.
    Threw,
    /// A repair amended a syntax-failed cell.
    Repaired,
}

impl State {
    fn label(self) -> &'static str {
        match self {
            Self::Preparing => "PREPARING",
            Self::Executed => "EXECUTED",
            Self::Threw => "FAILED",
            Self::Repaired => "REPAIRED",
        }
    }

    /// Whether this state inverts the field: ground on ink rather than ink on
    /// ground. A throw is the one that does, because it is the one a reader
    /// must not be able to skim past.
    fn inverts(self) -> bool {
        matches!(self, Self::Threw)
    }
}

/// The ink a field draws with, and the ground it sits on.
fn field_style(theme: Theme, state: State) -> (Style, Style) {
    let accent = if state.inverts() {
        Color::Red
    } else {
        theme.accent()
    };
    let runs = Style::default().fg(accent);
    let label = if state.inverts() {
        Style::default()
            .fg(theme.dock())
            .bg(accent)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default()
            .fg(accent)
            .bg(theme.dock())
            .add_modifier(Modifier::BOLD)
    };
    (runs, label)
}

/// A run of `n` glyphs, revealed by `tick`.
///
/// A reveal walks [`RAMP`] rather than growing the run's length, so the
/// field's geometry is identical on every frame and only its weight changes
/// -- a run that grew would reflow the labels beside it every frame, which is
/// the jitter this project already paid for once in scrolling.
fn run(n: usize, glyph: char, tick: usize) -> String {
    if n == 0 {
        return String::new();
    }
    let ch = if tick == 0 {
        glyph
    } else {
        // Two frames of reveal, then the glyph itself for every later frame:
        // a landing block, not a pulsing one.
        match tick % 8 {
            1 => RAMP[1],
            2 => RAMP[2],
            _ => glyph,
        }
    };
    ch.to_string().repeat(n)
}

/// `███ 10 / CELL ██████████████ EXECUTED ███`
///
/// **What it sheds, in order, as the width falls**: the tail run, then the
/// lead run, then the fill between the labels -- and the two labels are never
/// shed, because which cell this is and what became of it is the whole
/// message. At the narrowest the line is `10 / CELL · EXECUTED`, which still
/// carries both.
pub(super) fn field_header(
    cell: usize,
    state: State,
    width: usize,
    theme: Theme,
    tick: usize,
) -> Line<'static> {
    let (runs, label) = field_style(theme, state);
    let left = format!(" {cell:02} / CELL ");
    let right = format!(" {} ", state.label());
    let labels = left.chars().count() + right.chars().count();

    // Lead, fill and tail, dropped in that priority when the width is short.
    let (lead, tail) = if width >= labels + RUN * 3 {
        (RUN, RUN)
    } else if width >= labels + RUN * 2 {
        (RUN, 0)
    } else {
        (0, 0)
    };
    let fill = width.saturating_sub(labels + lead + tail);
    if lead == 0 && fill < RUN {
        // No room for a field at all: signage alone, still both labels.
        return Line::styled(format!("{}·{}", left, right), label);
    }

    let mut spans = Vec::new();
    if lead > 0 {
        spans.push(Span::styled(run(lead, FIELD, tick), runs));
    }
    spans.push(Span::styled(left, label));
    spans.push(Span::styled(run(fill, FIELD, tick), runs));
    spans.push(Span::styled(right, label));
    if tail > 0 {
        spans.push(Span::styled(run(tail, FIELD, tick), runs));
    }
    Line::from(spans)
}

/// The intent, as the headline it is.
///
/// The model writes one sentence about what the cell is for. Its first half
/// says what is being done and its second half qualifies it, and the two are
/// separated by a comma or by one of a few joining words. The headline takes
/// the first half in uppercase accent; the qualifier follows on a `▸` line in
/// the body colour.
///
/// **A long intent cannot push the cell off the screen.** The headline is
/// capped at [`HEADLINE_LINES`] and the qualifier at [`QUALIFIER_LINES`];
/// whatever is left is cut with an ellipsis rather than wrapped on. The whole
/// text stays in the cell's own record and Ctrl-O shows it.
pub(super) fn intent_block(text: &str, width: usize, theme: Theme) -> Vec<Line<'static>> {
    let text = text.trim();
    if text.is_empty() {
        return Vec::new();
    }
    let (head, tail) = split_intent(text);
    let mut lines: Vec<Line<'static>> = wrap(
        &head.to_uppercase(),
        width.saturating_sub(1),
        HEADLINE_LINES,
    )
    .into_iter()
    .map(|row| {
        Line::styled(
            format!(" {row}"),
            Style::default()
                .fg(theme.accent())
                .add_modifier(Modifier::BOLD),
        )
    })
    .collect();
    if !tail.is_empty() {
        for (index, row) in wrap(tail, width.saturating_sub(3), QUALIFIER_LINES)
            .into_iter()
            .enumerate()
        {
            lines.push(Line::styled(
                format!(" {} {row}", if index == 0 { '▸' } else { ' ' }),
                Style::default().fg(Color::White),
            ));
        }
    }
    lines
}

/// Where the headline ends and the qualifier begins.
///
/// A comma first, because a model's sentence usually has one exactly there.
/// Otherwise one of the joining words, whichever comes first. Neither present
/// means the whole sentence is the headline, which is right for a short one.
fn split_intent(text: &str) -> (String, &str) {
    let stripped = text
        .strip_prefix("I'm ")
        .or_else(|| text.strip_prefix("I am "))
        .or_else(|| text.strip_prefix("I will "))
        .or_else(|| text.strip_prefix("I "))
        .unwrap_or(text);
    if let Some(at) = stripped.find(", ") {
        return (stripped[..at].to_string(), stripped[at + 2..].trim());
    }
    const JOINS: [&str; 5] = [" before ", " so that ", " so ", " then ", " while "];
    let split = JOINS
        .iter()
        .filter_map(|join| stripped.find(join).map(|at| (at, join.len())))
        .min_by_key(|(at, _)| *at);
    match split {
        Some((at, len)) => (stripped[..at].to_string(), stripped[at + len - 1..].trim()),
        None => (stripped.to_string(), ""),
    }
}

/// Counts the kinds of host call a cell made, in the order they first ran.
///
/// The execution record is one line per call, `├─ <tool>[ arg] · <status>`,
/// and a lifted call spells `<written> ↳ <tool>` -- the tool that ran is the
/// half after the arrow, because that is what the cell actually did.
///
/// **Mixed kinds are named, never totalled away.** `rg ×4 · context ×2` is
/// what a reader acts on; a bare `6 calls` is what they have to open the
/// inspector to recover.
pub(super) fn call_summary(execution: &str) -> Option<String> {
    let mut kinds: Vec<(String, usize)> = Vec::new();
    let mut failed = 0usize;
    for line in execution.lines() {
        let Some(rest) = line
            .strip_prefix("├─ ")
            .or_else(|| line.strip_prefix("└─ "))
        else {
            continue;
        };
        let (call, status) = rest.split_once(" · ").unwrap_or((rest, ""));
        if !status.is_empty() && status != "returned" {
            failed += 1;
        }
        let ran = call.rsplit(" ↳ ").next().unwrap_or(call);
        let tool = ran.split_whitespace().next().unwrap_or(ran).to_string();
        if tool.is_empty() {
            continue;
        }
        match kinds.iter_mut().find(|(name, _)| *name == tool) {
            Some((_, count)) => *count += 1,
            None => kinds.push((tool, 1)),
        }
    }
    if kinds.is_empty() {
        return None;
    }
    let mut summary = kinds
        .into_iter()
        .map(|(name, count)| {
            if count == 1 {
                name
            } else {
                format!("{name} ×{count}")
            }
        })
        .collect::<Vec<_>>()
        .join(" · ");
    if failed > 0 {
        summary.push_str(&format!(" · {failed} failed"));
    }
    Some(summary)
}

/// `▓▓▓▓▓▓ rg × 6 ▓▓▓▓▓▓▓▓▓▓▓▓▓▓ 2.4s ▓▓▓`
///
/// One bar for every host call the cell made, because the per-call tree
/// belongs in inspection rather than in the flow. `summary` already names the
/// kinds and their counts -- mixed kinds read `rg ×4 · context ×2`, never a
/// bare total, because which tools ran is the half a reader acts on.
pub(super) fn call_bar(
    summary: &str,
    trailing: Option<String>,
    width: usize,
    theme: Theme,
    tick: usize,
) -> Line<'static> {
    let ink = Style::default().fg(theme.accent());
    let label = Style::default()
        .fg(theme.accent())
        .bg(theme.dock())
        .add_modifier(Modifier::BOLD);
    let left = format!(" {summary} ");
    let right = trailing.map(|t| format!(" {t} ")).unwrap_or_default();
    let labels = left.chars().count() + right.chars().count();
    let (lead, tail) = if width >= labels + RUN * 3 {
        (RUN * 2, RUN)
    } else if width >= labels + RUN * 2 {
        (RUN, 0)
    } else {
        (0, 0)
    };
    let fill = width.saturating_sub(labels + lead + tail);
    let mut spans = Vec::new();
    if lead > 0 {
        spans.push(Span::styled(run(lead, BAR, tick), ink));
    }
    spans.push(Span::styled(left, label));
    if fill >= RUN {
        spans.push(Span::styled(run(fill, BAR, tick), ink));
    }
    if !right.is_empty() {
        spans.push(Span::styled(right, label));
    }
    if tail > 0 {
        spans.push(Span::styled(run(tail, BAR, tick), ink));
    }
    Line::from(spans)
}

/// The rule that closes a cell.
pub(super) fn closing_rule(width: usize, theme: Theme, tick: usize) -> Line<'static> {
    Line::styled(run(width, FIELD, tick), Style::default().fg(theme.accent()))
}

/// One binding of a cell's handle table, as a numbered row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Row {
    pub name: String,
    pub type_label: String,
    pub count: Option<String>,
    /// Whether an earlier cell produced this binding and this one only
    /// carried it. `render_table_delta` decides it, not this module: an
    /// entry declared, replaced or changed in the current cell renders in
    /// full, and every other live entry renders as one line saying which
    /// cell it last changed in.
    pub carried: bool,
}

/// Reads the handle table's own rendering back into rows.
///
/// `render_table` emits one unindented header line per binding and indents
/// every body line beneath it, so the headers are exactly the unindented
/// lines. A header is `name<pad>Type<gap>n=<len><gap>inline cost …`: the
/// fields after the type are the model's token budget and are dropped here,
/// because a person reading the transcript is not spending it.
///
/// **This reads a rendering rather than a value, and that is deliberate.**
/// The table is what the cell recorded and what a resumed session has; going
/// back to the structured value would mean a second renderer that could
/// disagree with the one the model was given.
pub(super) fn rows_of(table: &str) -> Vec<Row> {
    table
        .lines()
        .filter(|line| !line.is_empty() && !line.starts_with(char::is_whitespace))
        .filter_map(|line| {
            let mut fields = line.split("  ").filter(|f| !f.trim().is_empty());
            let name = fields.next()?.trim().to_string();
            let type_label = fields.next()?.trim().to_string();
            let count = fields.find_map(|field| {
                field
                    .trim()
                    .strip_prefix("n=")
                    .map(|count| count.trim().to_string())
            });
            Some(Row {
                name,
                type_label,
                count,
                carried: line.contains(CARRIED),
            })
        })
        .collect()
}

/// `   01  profileCoupling    Match[]   18`
///
/// Numbered like the sidebar's panels, aligned on the widest name so the type
/// column reads as a column. A row never carries the token costs the model's
/// copy does.
pub(super) fn value_rows(
    rows: &[Row],
    width: usize,
    limit: usize,
    theme: Theme,
) -> Vec<Line<'static>> {
    if rows.is_empty() {
        return Vec::new();
    }
    let name_width = rows
        .iter()
        .take(limit)
        .map(|row| row.name.chars().count())
        .max()
        .unwrap_or(0);
    let type_width = rows
        .iter()
        .take(limit)
        .map(|row| row.type_label.chars().count())
        .max()
        .unwrap_or(0);
    let mut lines: Vec<Line<'static>> = rows
        .iter()
        .take(limit)
        .enumerate()
        .map(|(index, row)| {
            let count = row.count.as_deref().unwrap_or("");
            let text = format!(
                "   {:02}  {:<name_width$}  {:<type_width$}  {:>5}",
                index + 1,
                row.name,
                row.type_label,
                count,
            );
            let text: String = text.chars().take(width).collect();
            Line::from(vec![
                Span::styled(
                    text[..text.len().min(5)].to_string(),
                    Style::default().fg(MUTED),
                ),
                Span::styled(
                    text[text.len().min(5)..].to_string(),
                    Style::default().fg(theme.accent()),
                ),
            ])
        })
        .collect();
    if rows.len() > limit {
        lines.push(Line::styled(
            format!(
                "   … {} more binding{} · Ctrl-O expands",
                rows.len() - limit,
                if rows.len() - limit == 1 { "" } else { "s" }
            ),
            Style::default().fg(MUTED),
        ));
    }
    lines
}

/// How many bindings a compact cell shows before it says how many are left.
const BINDING_ROWS: usize = 8;

/// The marker `render_table_delta` writes on a live binding the current cell
/// did not change. Matching on the rendering rather than on a value is the
/// same deliberate choice [`rows_of`] documents: one renderer, one answer.
const CARRIED: &str = "(unchanged since cell ";

/// The cell's bindings, as rows — or its raw stdout when it has no table.
///
/// A cell that never ran has no handle table, and a resumed session's earlier
/// cells came from the rollout: both fall back to the stdout region that was
/// the only thing this column drew before. A planning-mode table is not a
/// binding table and keeps its own region.
pub(super) fn push_bindings(
    lines: &mut Vec<Line<'static>>,
    table: Option<&str>,
    stdout: Option<&str>,
    width: usize,
    theme: Theme,
) {
    let rows = table
        .filter(|table| !table.starts_with("Planning mode"))
        .map(rows_of)
        .unwrap_or_default();
    if !rows.is_empty() {
        // **A cell shows what it produced, not the task's whole table.** The
        // accumulation grows every cell, so by cell ten a reader was looking
        // at a list that was almost entirely earlier cells' work -- the user,
        // on their own screen: *"wie das hier stacked ist, ist ganz
        // schlimm"* (2026-09-19). The carried ones are history and Ctrl-O
        // has them; the count stays, because a reader who wonders where a
        // binding went must not have to guess that it still exists.
        let (produced, carried): (Vec<Row>, Vec<Row>) =
            rows.into_iter().partition(|row| !row.carried);
        lines.extend(value_rows(&produced, width, BINDING_ROWS, theme));
        if !carried.is_empty() {
            lines.push(Line::styled(
                format!(
                    "   {} binding{} carried from earlier cells · Ctrl-O expands",
                    carried.len(),
                    if carried.len() == 1 { "" } else { "s" }
                ),
                Style::default().fg(MUTED),
            ));
        }
        return;
    }
    if let Some(stdout) = stdout.filter(|s| !s.trim().is_empty() && s.trim() != "undefined") {
        super::regions::push_folded_region(lines, stdout, 6, true);
    }
}

/// The cell's footer: where to open it, and the rule that closes the field.
///
/// Pushed after the regions rather than beside the header, because a rule
/// drawn between the call bar and the bindings would cut the cell in half.
pub(super) fn push_footer(
    lines: &mut Vec<Line<'static>>,
    cell: usize,
    width: usize,
    theme: Theme,
    tick: usize,
) {
    lines.push(Line::styled(
        format!(" Ctrl-O · code and results · /cell {cell} or click this header"),
        Style::default().fg(MUTED),
    ));
    lines.push(closing_rule(width, theme, tick));
}

/// Wraps `text` to `width`, at most `limit` rows, cutting the last with an
/// ellipsis when there is more. Never returns an empty row for empty input.
fn wrap(text: &str, width: usize, limit: usize) -> Vec<String> {
    if width == 0 || text.is_empty() {
        return Vec::new();
    }
    let mut rows: Vec<String> = Vec::new();
    let mut row = String::new();
    for word in text.split_whitespace() {
        let candidate = if row.is_empty() {
            word.to_string()
        } else {
            format!("{row} {word}")
        };
        if candidate.chars().count() > width && !row.is_empty() {
            rows.push(std::mem::take(&mut row));
            if rows.len() == limit {
                break;
            }
            row = word.to_string();
        } else {
            row = candidate;
        }
    }
    if rows.len() < limit && !row.is_empty() {
        rows.push(row);
        return rows;
    }
    // More text than the cap allows: the last row keeps an ellipsis so the
    // cut is visible rather than silent.
    if let Some(last) = rows.last_mut() {
        let keep = width.saturating_sub(1);
        if last.chars().count() > keep {
            *last = last.chars().take(keep).collect();
        }
        last.push('…');
    }
    rows
}

/// The transient notice, as signage on a rule.
///
/// **The rule stays.** It divides the composer from the transcript, and a
/// rule that divides honestly is not decoration -- it is the thing that makes
/// a notice legible as a notice. What changed is the label on it: uppercase
/// signage in this module's own voice rather than a lowercase word nested in
/// a border.
///
/// An error inverts, the way [`State::Threw`]'s field does and for the same
/// reason: it is the one a reader must not be able to skim past. A plain
/// notice sits on `dock()` rather than on the accent, which is what keeps
/// Mono legible -- its accent is white, and a white label would be a bar of
/// light above the composer.
pub(super) fn notice(
    frame: &mut ratatui::Frame,
    area: ratatui::layout::Rect,
    state: &super::ScreenState,
) {
    if area.height == 0 {
        return;
    }
    let queue = queued_lines(state);
    if !queue.is_empty() {
        let split = (queue.len() as u16 + 1).min(area.height);
        let (top, rest) = (
            ratatui::layout::Rect {
                height: split,
                ..area
            },
            ratatui::layout::Rect {
                y: area.y + split,
                height: area.height - split,
                ..area
            },
        );
        frame.render_widget(
            ratatui::widgets::Paragraph::new(queue)
                .block(
                    ratatui::widgets::Block::default()
                        .borders(ratatui::widgets::Borders::TOP)
                        .title(Span::styled(
                            " QUEUED ",
                            Style::default()
                                .fg(state.theme.dock())
                                .bg(state.theme.accent())
                                .add_modifier(Modifier::BOLD),
                        )),
                )
                .style(Style::default().fg(state.theme.accent())),
            top,
        );
        if rest.height > 0 {
            notice_only(frame, rest, state);
        }
        return;
    }
    notice_only(frame, area, state);
}

/// The queue over the composer: what was handed over while the model worked,
/// oldest first, each on one line and each clipped rather than wrapped --
/// this row says *that* something is waiting and roughly what, and the
/// composer under it is where a person looks for the rest.
fn queued_lines(state: &super::ScreenState) -> Vec<Line<'static>> {
    if state.queued.is_empty() {
        return Vec::new();
    }
    let mut lines: Vec<Line<'static>> = state
        .queued
        .iter()
        .take(crate::tui::QUEUE_ROWS)
        .map(|message| Line::from(format!("  ⤷ {}", one_line(message))))
        .collect();
    if state.queued.len() > crate::tui::QUEUE_ROWS {
        lines.push(Line::from(format!(
            "  ⤷ and {} more",
            state.queued.len() - crate::tui::QUEUE_ROWS
        )));
    }
    lines
}

/// One line of a message that may be several, so the queue row stays one row
/// per message however the message was typed.
fn one_line(message: &str) -> String {
    let flattened: String = message.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut taken: String = flattened.chars().take(72).collect();
    if flattened.chars().count() > 72 {
        taken.push('…');
    }
    taken
}

fn notice_only(
    frame: &mut ratatui::Frame,
    area: ratatui::layout::Rect,
    state: &super::ScreenState,
) {
    let Some(notice) = &state.notice else {
        return;
    };
    let error = notice.starts_with("ERROR:");
    let label = if error {
        Style::default()
            .fg(state.theme.dock())
            .bg(Color::Red)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default()
            .fg(state.theme.accent())
            .bg(state.theme.dock())
            .add_modifier(Modifier::BOLD)
    };
    frame.render_widget(
        ratatui::widgets::Paragraph::new(notice.as_str())
            .wrap(ratatui::widgets::Wrap { trim: false })
            .block(
                ratatui::widgets::Block::default()
                    .borders(ratatui::widgets::Borders::TOP)
                    .title(Span::styled(
                        if error { " ERROR " } else { " NOTICE " },
                        label,
                    )),
            )
            .style(Style::default().fg(if error { Color::Red } else { MUTED })),
        area,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(line: &Line<'static>) -> String {
        line.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    #[test]
    fn a_queued_message_is_one_row_however_it_was_typed() {
        let state = crate::tui::ScreenState {
            queued: vec!["lass die venvs\n   weg   beim grep".into()],
            ..Default::default()
        };
        let lines = queued_lines(&state);
        assert_eq!(lines.len(), 1, "a multi-line message took several rows");
        assert_eq!(text(&lines[0]), "  ⤷ lass die venvs weg beim grep");
    }

    #[test]
    fn the_queue_names_what_it_cannot_show() {
        let state = crate::tui::ScreenState {
            queued: (0..crate::tui::QUEUE_ROWS + 2)
                .map(|n| format!("message {n}"))
                .collect(),
            ..Default::default()
        };
        let lines = queued_lines(&state);
        assert_eq!(lines.len(), crate::tui::QUEUE_ROWS + 1);
        assert_eq!(
            text(lines.last().unwrap()),
            "  ⤷ and 2 more",
            "queued messages vanished with no count standing for them"
        );
    }

    #[test]
    fn an_empty_queue_takes_no_rows() {
        assert!(queued_lines(&crate::tui::ScreenState::default()).is_empty());
    }

    #[test]
    fn a_header_keeps_both_labels_when_the_width_cannot_hold_its_runs() {
        for width in [40usize, 50, 60, 80, 120] {
            let line = field_header(10, State::Executed, width, Theme::Neon, 0);
            let rendered = text(&line);
            assert!(
                rendered.contains("10 / CELL"),
                "width {width} lost the cell label: {rendered}"
            );
            assert!(
                rendered.contains("EXECUTED"),
                "width {width} lost the state label: {rendered}"
            );
        }
    }

    #[test]
    fn a_header_never_exceeds_the_width_it_was_given() {
        for width in [30usize, 45, 60, 100, 160] {
            let rendered = text(&field_header(7, State::Executed, width, Theme::Neon, 0));
            assert!(
                rendered.chars().count() <= width.max(24),
                "width {width} overflowed: {} chars",
                rendered.chars().count()
            );
        }
    }

    #[test]
    fn every_theme_draws_a_header_with_ink_that_is_not_its_ground() {
        for theme in [
            Theme::Neon,
            Theme::Amber,
            Theme::Ice,
            Theme::Mono,
            Theme::Violet,
            Theme::Cobalt,
            Theme::Mint,
            Theme::Rose,
        ] {
            let line = field_header(3, State::Executed, 80, theme, 0);
            let label = line
                .spans
                .iter()
                .find(|span| span.content.contains("CELL"))
                .expect("the cell label is a span of its own");
            assert_ne!(
                label.style.fg, label.style.bg,
                "{theme:?} drew its label in its own ground"
            );
            assert_eq!(
                label.style.bg,
                Some(theme.dock()),
                "{theme:?} lost its field"
            );
        }
    }

    #[test]
    fn a_throw_inverts_the_field_rather_than_only_renaming_it() {
        let executed = field_header(1, State::Executed, 80, Theme::Neon, 0);
        let threw = field_header(1, State::Threw, 80, Theme::Neon, 0);
        let label_of = |line: &Line<'static>| {
            line.spans
                .iter()
                .find(|span| span.content.contains("CELL"))
                .expect("a label span")
                .style
        };
        assert_ne!(label_of(&executed).bg, label_of(&threw).bg);
        assert_eq!(label_of(&threw).bg, Some(Color::Red));
    }

    #[test]
    fn a_still_tick_renders_a_field_identically_on_every_frame() {
        // The caller decides which cell is live; this is the contract it
        // relies on -- a tick of zero is the same field every frame, so a
        // finished cell handed zero cannot shimmer.
        for state in [State::Executed, State::Threw, State::Repaired] {
            let a = text(&field_header(3, state, 80, Theme::Neon, 0));
            let b = text(&field_header(3, state, 80, Theme::Neon, 0));
            assert_eq!(a, b, "{state:?} moved between consecutive frames");
            assert!(
                !a.contains(RAMP[0]) && !a.contains(RAMP[1]) && !a.contains(RAMP[2]),
                "a still field used a reveal glyph: {a}"
            );
        }
    }

    #[test]
    fn a_live_tick_still_moves_so_the_running_cell_reads_as_alive() {
        let a = text(&field_header(3, State::Preparing, 80, Theme::Neon, 9));
        let b = text(&field_header(3, State::Preparing, 80, Theme::Neon, 10));
        assert_ne!(a, b, "the running cell stopped moving");
    }

    #[test]
    fn a_carried_binding_is_counted_rather_than_listed() {
        let table = concat!(
            "fresh         Array         n=3   inline cost ~12 tok\n",
            "    [0] 1\n",
            "architecture  File          (unchanged since cell 1)\n",
            "exactHits     Grep.Match[]  (unchanged since cell 1)\n",
        );
        let rows = rows_of(table);
        assert_eq!(rows.len(), 3, "{rows:?}");
        assert_eq!(
            rows.iter().filter(|row| row.carried).count(),
            2,
            "the carried marker was not read: {rows:?}"
        );
        let mut lines = Vec::new();
        push_bindings(&mut lines, Some(table), None, 80, Theme::Neon);
        let drawn: Vec<String> = lines.iter().map(text).collect();
        let all = drawn.join("\n");
        assert!(
            all.contains("fresh"),
            "the produced binding is missing: {all}"
        );
        assert!(
            !all.contains("architecture") && !all.contains("exactHits"),
            "a carried binding was listed again: {all}"
        );
        assert!(
            all.contains("2 bindings carried from earlier cells"),
            "the carried count is missing: {all}"
        );
    }

    #[test]
    fn motion_off_draws_every_run_at_full_strength() {
        let still = text(&field_header(4, State::Executed, 80, Theme::Neon, 0));
        assert!(
            !still.contains(RAMP[0]) && !still.contains(RAMP[1]) && !still.contains(RAMP[2]),
            "a still frame used a reveal glyph: {still}"
        );
        assert!(
            still.contains(FIELD),
            "a still frame lost its field: {still}"
        );
    }

    #[test]
    fn an_intent_becomes_a_headline_and_a_qualifier() {
        let lines = intent_block(
            "I'm extracting the remaining coupled expressions, before applying the profile split",
            60,
            Theme::Neon,
        );
        let rendered: Vec<String> = lines.iter().map(text).collect();
        assert!(
            rendered[0].contains("EXTRACTING THE REMAINING"),
            "headline missing: {rendered:?}"
        );
        assert!(
            rendered.iter().any(|row| row.starts_with(" ▸")),
            "qualifier missing: {rendered:?}"
        );
        assert!(
            rendered.iter().any(|row| row.contains("before applying")),
            "qualifier text missing: {rendered:?}"
        );
    }

    #[test]
    fn a_very_long_intent_cannot_push_the_cell_off_the_screen() {
        let long = format!("I'm {}", "extracting expressions ".repeat(80));
        let lines = intent_block(&long, 60, Theme::Neon);
        assert!(
            lines.len() <= HEADLINE_LINES + QUALIFIER_LINES,
            "an intent took {} lines",
            lines.len()
        );
        let rendered: Vec<String> = lines.iter().map(text).collect();
        assert!(
            rendered.last().is_some_and(|row| row.contains('…')),
            "a cut intent did not say it was cut: {rendered:?}"
        );
    }

    #[test]
    fn a_handle_table_becomes_rows_and_never_braces() {
        let table = "profileCoupling   Grep.Match[]   n=18   inline cost ~1,178 tok · preview 129 tok\n  [0] \"profile.rs:51\"\n  [1] \"profile.rs:145\"\nstartupCoupling   Grep.Match[]   n=19   inline cost ~900 tok · preview 90 tok\n";
        let rows = rows_of(table);
        assert_eq!(rows.len(), 2, "{rows:?}");
        assert_eq!(rows[0].name, "profileCoupling");
        assert_eq!(rows[0].type_label, "Grep.Match[]");
        assert_eq!(rows[0].count.as_deref(), Some("18"));
        let lines = value_rows(&rows, 80, 8, Theme::Neon);
        let rendered: Vec<String> = lines.iter().map(text).collect();
        assert!(rendered[0].contains("01"), "{rendered:?}");
        assert!(rendered[0].contains("profileCoupling"), "{rendered:?}");
        assert!(
            rendered
                .iter()
                .all(|row| !row.contains('{') && !row.contains('"')),
            "a row carried JSON: {rendered:?}"
        );
    }

    #[test]
    fn more_bindings_than_the_limit_say_how_many_are_left() {
        let rows: Vec<Row> = (0..12)
            .map(|i| Row {
                name: format!("binding{i}"),
                type_label: "Array".into(),
                count: Some("3".into()),
                carried: false,
            })
            .collect();
        let rendered: Vec<String> = value_rows(&rows, 80, 4, Theme::Neon)
            .iter()
            .map(text)
            .collect();
        assert_eq!(rendered.len(), 5, "{rendered:?}");
        assert!(rendered[4].contains("8 more bindings"), "{rendered:?}");
    }

    #[test]
    fn a_call_bar_names_mixed_kinds_rather_than_a_bare_total() {
        let rendered = text(&call_bar(
            "rg ×4 · context ×2",
            Some("2.4s".into()),
            80,
            Theme::Neon,
            0,
        ));
        assert!(rendered.contains("rg ×4"), "{rendered}");
        assert!(rendered.contains("context ×2"), "{rendered}");
        assert!(rendered.contains("2.4s"), "{rendered}");
        assert!(rendered.contains(BAR), "{rendered}");
    }
}
