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
/// A row of chips from `x`, each two columns apart; returns where it ended.
#[allow(clippy::too_many_arguments)]
pub(super) fn chips(
    f: &mut Frame<'_>,
    g: &mut Geometry,
    mut x: u16,
    y: u16,
    limit: u16,
    items: &[(String, Action, bool)],
    press: Option<(u16, u16)>,
    t: Theme,
) -> u16 {
    for (label, action, on) in items {
        let w = chip(
            f,
            g,
            x,
            y,
            limit,
            label,
            action.clone(),
            *on,
            Tone::Normal,
            press,
            t,
        );
        if w == 0 {
            break;
        }
        x += w + 1;
    }
    x
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
/// A framed region: rounded corners, and a title in the top edge.
pub(super) fn frame(f: &mut Frame<'_>, r: Rect, tone: Tone, t: Theme) {
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
