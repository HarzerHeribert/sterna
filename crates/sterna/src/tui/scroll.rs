//! The scroll indicator: what the transcript says about what it is hiding.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::widgets::Paragraph;

use super::{ACCENT, MUTED};

/// How long the scroll position indicator stays up after the last scroll.
///
/// Short on purpose: it is an overlay on the transcript's own right edge, so
/// it may follow a flick of the wheel and must not become furniture. A
/// transcript scrolled away from its live edge keeps a quiet marker after
/// this elapses -- that is a state worth showing, and it is not this timer's.
pub const SCROLL_INDICATOR_LINGER: std::time::Duration = std::time::Duration::from_millis(900);

/// The scroll indicator: **one column, overlaid on the transcript's own right
/// edge, and never furniture** (the user, 2026-09-17: *"the scrollbar
/// introduces as a second bar next to sidebar. Bad if they both coexist
/// overlay. Very thin and only show scroll indicator while actually
/// scrolling"* and *"Indicate the tui is scrollable when content is hidden by
/// user having scrolled"*).
///
/// Three states, and they are the whole of it:
///
/// - **at the live edge** — nothing is hidden below, so nothing is drawn;
/// - **scrolled away from it** — one `↓` at the bottom of the same column,
///   because a person who cannot see the newest lines must be told that is
///   why, and it costs no layout to say;
/// - **while scrolling** — the thumb, showing where in the transcript the
///   view sits, until [`SCROLL_INDICATOR_LINGER`] passes with no further
///   scroll.
///
/// It draws inside `area`, which is the transcript's rectangle, so it can
/// never take a column beside the sidebar: `the_scroll_marker_overlays_the_
/// transcript_and_never_the_sidebar` pins that.
pub(super) fn render_scrollbar(
    frame: &mut Frame,
    area: Rect,
    total: usize,
    start: usize,
    scrolling: bool,
) {
    let height = usize::from(area.height);
    if total <= height || height == 0 || area.width == 0 {
        return;
    }
    let column = area.right() - 1;
    if scrolling {
        let thumb = (height * height / total).max(1);
        let top = start.min(total - height) * (height - thumb) / (total - height);
        for row in top..(top + thumb).min(height) {
            frame.render_widget(
                Paragraph::new("▐").style(Style::default().fg(ACCENT)),
                Rect::new(column, area.y + row as u16, 1, 1),
            );
        }
        return;
    }
    if start + height < total {
        frame.render_widget(
            Paragraph::new("↓").style(Style::default().fg(MUTED)),
            Rect::new(column, area.bottom() - 1, 1, 1),
        );
    }
}
