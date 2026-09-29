//! The shapes every surface is made of: a frame, a chip, a rule with a joint.
//!
//! **One component language.** Every control on every surface is a chip,
//! `⟨ label ⟩`, filled when it is the current choice of a set and flashed
//! filled for the frames a finger is on it; every region is a frame drawn
//! with lines and never with paint. Learn the chip once and the whole
//! application is learned, which is why nothing here takes a style of its
//! own.
use super::{Action, Geometry, Tone, theme};
use crate::tui::Theme;
use ratatui::{Frame, layout::Rect, text::Span, widgets::Paragraph};

pub(super) fn width(text: &str) -> u16 {
    Span::raw(text).width() as u16
}
/// One line of text in one tone, clipped to its rectangle.
pub(super) fn text(f: &mut Frame<'_>, r: Rect, text: &str, tone: Tone, t: Theme) {
    let r = r.intersection(f.area());
    if r.height > 0 && r.width > 0 {
        f.render_widget(
            Paragraph::new(text.chars().filter(|c| !c.is_control()).collect::<String>())
                .style(theme::style(tone, t)),
            Rect::new(r.x, r.y, r.width, 1),
        );
    }
}
/// A chip at `(x, y)`: returns the columns it took. `on` fills it; a press
/// resting on it fills it too, for as long as the finger is down, which is
/// the whole of the feedback a click gets before its release does the work.
#[allow(clippy::too_many_arguments)]
pub(super) fn chip(
    f: &mut Frame<'_>,
    g: &mut Geometry,
    x: u16,
    y: u16,
    limit: u16,
    label: &str,
    action: Action,
    on: bool,
    tone: Tone,
    press: Option<(u16, u16)>,
    t: Theme,
) -> u16 {
    let inner = format!(" {label} ");
    let w = width(&inner) + 2;
    if x >= limit || w > limit - x || y >= f.area().bottom() {
        return 0;
    }
    let r = Rect::new(x, y, w, 1);
    let pressed = press.is_some_and(|(px, py)| super::contains(r, px, py));
    if on || pressed {
        f.render_widget(
            Paragraph::new(format!("⟨{inner}⟩")).style(theme::chip_on(t)),
            r,
        );
    } else {
        let bracket = match tone {
            Tone::Warning | Tone::Failure => tone,
            _ => Tone::Accent,
        };
        text(f, Rect::new(x, y, 1, 1), "⟨", bracket, t);
        text(f, Rect::new(x + 1, y, w - 2, 1), &inner, tone, t);
        text(f, Rect::new(x + w - 1, y, 1, 1), "⟩", bracket, t);
    }
    g.hits.push((r, action));
    w
}
/// Which chips of a row are drawn in `room` columns, given each one's width
/// with the column after it: every one when they fit; otherwise the `kept`
/// ones -- the active value, the tab that is open -- and then, in order, as
/// many of the rest as leave room for the `⟨ +N ▾ ⟩` chip that holds the
/// others. Nothing is dropped without that chip saying so.
pub(super) fn fitting(widths: &[u16], room: u16, kept: &[usize]) -> Vec<usize> {
    if widths.iter().sum::<u16>().saturating_sub(1) <= room {
        return (0..widths.len()).collect();
    }
    let mut budget = room.saturating_sub(width("+99 ▾") + 5);
    let mut shown: Vec<usize> = kept.to_vec();
    for &k in kept {
        budget = budget.saturating_sub(widths[k]);
    }
    for (i, w) in widths.iter().enumerate() {
        if !kept.contains(&i) && *w <= budget {
            budget -= w;
            shown.push(i);
        }
    }
    shown.sort_unstable();
    shown
}
/// Where each chip of a row lands from `x`: the ones [`fitting`] keeps,
/// then `⟨ +N ▾ ⟩` holding the rest, each one column after the last.
fn placed(
    items: &[(String, Action, bool)],
    x: u16,
    limit: u16,
) -> Vec<(String, Action, bool, u16)> {
    let widths: Vec<u16> = items.iter().map(|(label, ..)| width(label) + 5).collect();
    let kept: Vec<usize> = (0..items.len()).filter(|i| items[*i].2).collect();
    let mut at = x;
    let mut out = Vec::new();
    let mut drawn = Vec::new();
    for i in fitting(&widths, limit.saturating_sub(x), &kept) {
        let w = widths[i] - 1;
        if at >= limit || w > limit - at {
            break;
        }
        let (label, action, on) = &items[i];
        out.push((label.clone(), action.clone(), *on, at));
        drawn.push(i);
        at += w + 1;
    }
    let folded: Vec<(String, Action)> = (0..items.len())
        .filter(|i| !drawn.contains(i))
        .map(|i| (items[i].0.clone(), items[i].1.clone()))
        .collect();
    if !folded.is_empty() {
        let label = format!("+{} ▾", folded.len());
        if at < limit && width(&label) + 4 <= limit - at {
            out.push((label, Action::More(folded), false, at));
        }
    }
    out
}
/// Where a row of chips from `x` ends, the column after its last chip
/// included: what a strip clears before the row is drawn on it.
pub(super) fn chips_end(items: &[(String, Action, bool)], x: u16, limit: u16) -> u16 {
    placed(items, x, limit)
        .last()
        .map_or(x, |(label, _, _, at)| at + width(label) + 5)
}
/// A row of chips from `x`, one column apart; returns where it ended. The
/// ones that do not fit fold into `⟨ +N ▾ ⟩`, which lists them, and a chip
/// that is on is never the one folded.
#[allow(clippy::too_many_arguments)]
pub(super) fn chips(
    f: &mut Frame<'_>,
    g: &mut Geometry,
    x: u16,
    y: u16,
    limit: u16,
    items: &[(String, Action, bool)],
    press: Option<(u16, u16)>,
    t: Theme,
) -> u16 {
    let mut end = x;
    for (label, action, on, at) in placed(items, x, limit) {
        let w = chip(
            f,
            g,
            at,
            y,
            limit,
            &label,
            action,
            on,
            Tone::Normal,
            press,
            t,
        );
        end = at + w + 1;
    }
    end
}
/// A horizontal rule across `r`, with `joints` -- (column, glyph) pairs --
/// where another line meets it.
pub(super) fn rule(f: &mut Frame<'_>, r: Rect, joints: &[(u16, &str)], tone: Tone, t: Theme) {
    if r.width == 0 {
        return;
    }
    text(f, r, &"─".repeat(r.width as usize), tone, t);
    for (x, glyph) in joints {
        if *x >= r.x && *x < r.right() {
            text(f, Rect::new(*x, r.y, 1, 1), glyph, tone, t);
        }
    }
}
/// **Hover, drawn once for every surface**: the target under the pointer
/// lit in the accent. A filled chip keeps its fill (accent on accent would
/// read as nothing), blank cells stay blank, and the composer -- a place to
/// type, not a thing to press -- is never lit.
pub(crate) fn glow(f: &mut Frame<'_>, hits: &[(Rect, Action)], at: Option<(u16, u16)>, t: Theme) {
    let Some((x, y)) = at else {
        return;
    };
    let target = hits
        .iter()
        .rev()
        .find(|(r, _)| super::contains(*r, x, y))
        .filter(|(_, action)| *action != Action::Composer);
    if let Some((r, _)) = target {
        light(f, *r, t);
    }
}
/// One target lit: its ink in the accent and bold, its fill left alone.
pub(crate) fn light(f: &mut Frame<'_>, r: Rect, t: Theme) {
    let r = r.intersection(f.area());
    let lit = theme::style(Tone::Accent, t);
    let buffer = f.buffer_mut();
    for y in r.y..r.bottom() {
        for x in r.x..r.right() {
            let cell = &mut buffer[(x, y)];
            if cell.bg == ratatui::style::Color::Reset && !cell.symbol().trim().is_empty() {
                cell.set_style(lit);
            }
        }
    }
}
/// A framed region: rounded corners, and a title in the top edge.
pub(crate) fn frame(f: &mut Frame<'_>, r: Rect, tone: Tone, t: Theme) {
    if r.width < 2 || r.height < 2 {
        return;
    }
    let inner_w = (r.width - 2) as usize;
    text(
        f,
        Rect::new(r.x, r.y, r.width, 1),
        &format!("╭{}╮", "─".repeat(inner_w)),
        tone,
        t,
    );
    for y in r.y + 1..r.bottom() - 1 {
        text(f, Rect::new(r.x, y, 1, 1), "│", tone, t);
        text(f, Rect::new(r.right() - 1, y, 1, 1), "│", tone, t);
    }
    text(
        f,
        Rect::new(r.x, r.bottom() - 1, r.width, 1),
        &format!("╰{}╯", "─".repeat(inner_w)),
        tone,
        t,
    );
}
