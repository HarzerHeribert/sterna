//! The status line's row layout: one left half, one right half, and where
//! each lands -- so what is clickable is recorded from the same arithmetic
//! that draws it.

use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};

use super::{ContextTokens, MUTED, abbreviate};

pub(super) fn footer_row(
    left: String,
    right: String,
    width: usize,
    right_color: Color,
) -> Line<'static> {
    let occupied = Line::from(left.as_str()).width() + Line::from(right.as_str()).width() + 2;
    if right.is_empty() || occupied > width {
        return Line::styled(abbreviate(&left, width), Style::default().fg(MUTED));
    }
    Line::from(vec![
        Span::styled(left, Style::default().fg(MUTED)),
        Span::raw(" ".repeat(width - occupied + 1)),
        Span::styled(right, Style::default().fg(right_color)),
        Span::raw(" "),
    ])
}

pub(super) fn compact_tokens(value: u64) -> String {
    if value >= 1_000_000 {
        format!("{:.1}M", value as f64 / 1_000_000.0)
    } else if value >= 1_000 {
        format!("{:.1}k", value as f64 / 1_000.0)
    } else {
        value.to_string()
    }
}

/// A truthful, fixed-width occupancy trace. Motion changes only the marker at
/// the measured boundary; it never changes how many cells appear filled.
pub(crate) fn context_summary(
    tokens: ContextTokens,
    width: usize,
    tick: usize,
    moving: bool,
) -> String {
    let Some(cap) = tokens.cap else {
        // No window known: the count alone. A `?` where a size belongs
        // reads as something broken.
        return format!(
            "ctx {} · {}",
            compact_tokens(tokens.used),
            tokens.counted.as_str()
        );
    };
    let (bar, percent) = context_bar(tokens, width, tick, moving);
    // **A percentage is a claim, and it is only made against a measured
    // figure.** A window a catalogue published describes the model, not the
    // route serving it: a re-host caps what it resells and a subscription
    // tier narrows it again, so the meter marks the cap as an estimate and
    // says nothing about how much room is left
    // (`archive/glasshouse:docs/product/design-decisions.md`, *A context window is a property of
    // the route, not of the model*).
    if tokens.cap_source.is_trusted() {
        format!(
            "ctx {bar} {}/{} {percent}%",
            compact_tokens(tokens.used),
            compact_tokens(cap)
        )
    } else {
        format!(
            "ctx {bar} {}/~{}",
            compact_tokens(tokens.used),
            compact_tokens(cap)
        )
    }
}

pub(super) fn context_bar(
    tokens: ContextTokens,
    width: usize,
    tick: usize,
    moving: bool,
) -> (String, u64) {
    let cap = tokens.cap.unwrap_or(0);
    let eighths = if cap == 0 {
        0
    } else {
        ((tokens.used.min(cap) as u128 * (width * 8) as u128) / cap as u128) as usize
    };
    let full = eighths / 8;
    let partial = eighths % 8;
    let parts = [' ', '▏', '▎', '▍', '▌', '▋', '▊', '▉'];
    let mut bar = String::with_capacity(width);
    for index in 0..width {
        if index < full {
            let glint = moving && full > 1 && index == tick % full;
            bar.push(if glint { '◆' } else { '━' });
        } else if index == full && partial > 0 {
            bar.push(parts[partial]);
        } else {
            bar.push('─');
        }
    }
    let percent = if cap == 0 {
        0
    } else {
        ((tokens.used.min(cap) as u128 * 100) / cap as u128) as u64
    };
    (bar, percent)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::WindowSource;
    use crate::tui::Counted;

    fn meter(cap: Option<u64>, cap_source: WindowSource) -> String {
        context_summary(
            ContextTokens {
                used: 7_400,
                cap,
                cap_source,
                counted: Counted::Gateway,
            },
            8,
            0,
            false,
        )
    }

    #[test]
    fn a_measured_window_earns_a_percentage() {
        for source in [WindowSource::Observed, WindowSource::Configured] {
            let shown = meter(Some(922_000), source);
            assert!(shown.contains("7.4k/922.0k"), "{shown}");
            assert!(shown.contains('%'), "{shown}");
            assert!(!shown.contains('~'), "{shown}");
        }
    }

    #[test]
    fn a_published_window_is_marked_an_estimate_and_claims_no_percentage() {
        // The rule: being wrong about how much room is left is worse than
        // admitting the number came from a table.
        let shown = meter(Some(922_000), WindowSource::Published);
        assert!(shown.contains("7.4k/~922.0k"), "{shown}");
        assert!(
            !shown.contains('%'),
            "a percentage against an unmeasured cap is a claim the figure has not earned: {shown}"
        );
    }

    #[test]
    fn no_window_at_all_says_so_rather_than_drawing_a_bar() {
        let shown = meter(None, WindowSource::Unknown);
        assert!(shown.starts_with("ctx ") && !shown.contains('?'), "{shown}");
        assert!(!shown.contains('%'), "{shown}");
    }
}
