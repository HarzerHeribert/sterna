//! The helper lane under a cell (moved out of `tui.rs` for the Phase 59 size
//! ratchet, 2026-09-13; nothing here is new).

use super::*;

/// The helper lane's four frames: a line reaching out, and the dot coming
/// back on the fourth.
///
/// **One frame set for every helper.** The record's `verb` and `asked` carry
/// the difference, so a new `HelperSpec` renders here with no change to this
/// module.
pub(super) const HELPER_FRAMES: [&str; 4] = ["-.  ", "--. ", "---.", ".---"];

/// A call this short gets no lane of its own: it would appear and vanish
/// before it could be read, and a lane nobody can read is a lane nobody
/// reads. A **failed** call is exempt -- see [`helper_shows_a_lane`].
pub(super) const HELPER_LANE_MIN_MS: u64 = 300;

/// Past this many lanes the cell itself would be pushed off screen, so the
/// lane collapses to a count. Helpers run in parallel; the cell is what the
/// user came to read.
pub(super) const HELPER_LANE_MAX: usize = 3;

/// The name column, wide enough for the roster's names and fixed so the
/// glyphs line up down the lane.
pub(super) const HELPER_NAME_WIDTH: usize = 9;

/// Whether a call came back with a failure sentence instead of an answer.
pub(super) fn helper_failed(record: &HelperRecord) -> bool {
    !record.outcome.ok && !record.outcome.text.is_empty()
}

/// Whether a call has neither answered nor failed: the state the lane's
/// moving frames are for, and the one the seconds are counted for.
///
/// It is [`HelperOutcome::default()`] -- what `begin_helper` keeps until
/// `finish_helper` fills it in.
///
/// [`HelperOutcome::default()`]: crate::helpers::HelperOutcome
pub(crate) fn helper_in_flight(record: &HelperRecord) -> bool {
    !record.outcome.ok && record.outcome.text.is_empty()
}

/// Whether one call is worth a lane of its own.
///
/// **A failure always is, however short.** `supervisor.rs` shipped for weeks
/// rendering a permanently failing look as a healthy one; a helper that is
/// not working must never be quieter than one that is.
///
/// **A call still in flight always is too.** The floor is measured on a
/// duration a running call does not have yet, and what it forbids is a lane
/// that *vanishes*: an in-flight lane resolves into its own answer instead.
pub(super) fn helper_shows_a_lane(record: &HelperRecord) -> bool {
    helper_in_flight(record)
        || helper_failed(record)
        || record.outcome.elapsed_ms >= HELPER_LANE_MIN_MS
}

/// Elapsed as text rather than as animation: under `/motion off` the glyph
/// freezes and this keeps counting, because *is it alive* is the question
/// the lane answers.
pub(super) fn helper_seconds(elapsed_ms: u64) -> String {
    format!("{:.1}s", elapsed_ms as f64 / 1000.0)
}

pub(super) fn helper_count(calls: usize) -> String {
    if calls == 1 {
        "1 helper".to_string()
    } else {
        format!("{calls} helpers")
    }
}

/// The cell header's own summary of its helpers -- one line, and it persists
/// in scrollback after the lane is gone.
///
/// **Only calls that have resolved are counted.** The header is what the
/// cell has already got back; counting a call still in flight folds it
/// before its lane has said anything, which is the reverse of
/// `little-helpers.md`'s order -- run, resolve, then fold.
pub(super) fn helper_fold(view: Option<&CellView>) -> String {
    match view.map_or(0, |view| {
        view.helpers
            .iter()
            .filter(|record| !helper_in_flight(record))
            .count()
    }) {
        0 => String::new(),
        calls => format!(" · {}", helper_count(calls)),
    }
}

/// One lane line: the helper, its state, what it is doing or what came back,
/// and its elapsed at the right of the column.
pub(super) fn helper_lane(record: &HelperRecord, tick: usize, width: usize) -> Line<'static> {
    let (glyph, body) = if record.outcome.ok {
        (
            Activity::Complete.indicator(tick),
            record
                .outcome
                .text
                .lines()
                .next()
                .unwrap_or_default()
                .trim()
                .to_string(),
        )
    } else if helper_failed(record) {
        (Activity::Failed.indicator(tick), record.lane_result())
    } else {
        (
            HELPER_FRAMES[tick % HELPER_FRAMES.len()],
            format!("{} {}", record.verb, record.asked)
                .trim()
                .to_string(),
        )
    };
    let head = format!(
        "  {:<name$} {glyph}  ",
        abbreviate(&record.helper, HELPER_NAME_WIDTH),
        name = HELPER_NAME_WIDTH
    );
    let seconds = helper_seconds(record.outcome.elapsed_ms);
    let room = width.saturating_sub(head.chars().count() + seconds.chars().count() + 2);
    let body = abbreviate(&body, room);
    let gap = width
        .saturating_sub(head.chars().count() + body.chars().count() + seconds.chars().count())
        .max(1);
    Line::styled(
        format!("{head}{body}{}{seconds}", " ".repeat(gap)),
        Style::default().fg(if helper_failed(record) {
            Color::Red
        } else {
            MUTED
        }),
    )
}

/// The lane under a cell's header: one line per helper call the cell made.
///
/// Past [`HELPER_LANE_MAX`] lines it collapses to a count and a total -- but
/// a failed call keeps its own line through the collapse, because a helper
/// that is not working must not be summarised into one that is.
pub(super) fn push_helper_lane(
    lines: &mut Vec<Line<'static>>,
    view: Option<&CellView>,
    tick: usize,
    width: usize,
) {
    let Some(helpers) = view.map(|view| view.helpers.as_slice()) else {
        return;
    };
    let shown: Vec<&HelperRecord> = helpers
        .iter()
        .filter(|record| helper_shows_a_lane(record))
        .collect();
    if shown.len() > HELPER_LANE_MAX {
        let total: u64 = helpers.iter().map(|record| record.outcome.elapsed_ms).sum();
        lines.push(Line::styled(
            format!(
                "  {} · {}",
                helper_count(helpers.len()),
                helper_seconds(total)
            ),
            Style::default().fg(MUTED),
        ));
        for record in shown.into_iter().filter(|record| helper_failed(record)) {
            lines.push(helper_lane(record, tick, width));
        }
        return;
    }
    for record in shown {
        lines.push(helper_lane(record, tick, width));
    }
}
