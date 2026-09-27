//! One palette, read from the workbench mockup, expressed as terminal colour.
//!
//! **The background is never painted and normal prose keeps the terminal's own
//! foreground.** Everything else is a role — accent, helper, failure, warning,
//! success, muted, rule — and a role is one fixed colour so that two surfaces
//! drawn by different code cannot disagree about what "muted" looks like. Only
//! the accent moves with the chosen theme, exactly as the mockup's `--accent`
//! custom property does.
use super::Tone;
use crate::tui::Theme;
use ratatui::style::{Color, Modifier, Style};

/// `--accent` per theme, as the mockup's dark shell defines it.
pub(super) fn accent(theme: Theme) -> Color {
    match theme {
        Theme::Neon => Color::Rgb(0xda, 0xff, 0x50),
        Theme::Amber => Color::Rgb(0xff, 0xce, 0x72),
        Theme::Ice => Color::Rgb(0x8b, 0xe3, 0xff),
        // Mono keeps the terminal's own foreground: its accent is emphasis,
        // not hue, and a user who chose mono asked for exactly that.
        Theme::Mono => Color::Reset,
        Theme::Violet => Color::Rgb(0xd4, 0xb4, 0xff),
        Theme::Cobalt => Color::Rgb(0x9e, 0xc9, 0xff),
        Theme::Mint => Color::Rgb(0x86, 0xf1, 0xd0),
        Theme::Rose => Color::Rgb(0xff, 0xb3, 0xd4),
        Theme::Bird(bird) => crate::tui::theme::rgb(bird.plumage().accent),
    }
}
/// `--muted`: technical detail that must stay readable, never decoration.
pub(super) const MUTED: Color = Color::Rgb(0x99, 0xa6, 0xb7);
/// `--line`: rules and separators, the only thing quieter than muted.
pub(super) const LINE: Color = Color::Rgb(0x55, 0x64, 0x76);
/// `--cyan`: little helpers and their returned evidence.
pub(super) const HELPER: Color = Color::Rgb(0x8b, 0xe3, 0xff);
const WARN: Color = Color::Rgb(0xff, 0xca, 0x80);
const RED: Color = Color::Rgb(0xff, 0x84, 0x94);
const GREEN: Color = Color::Rgb(0xa4, 0xf1, 0xbd);

pub(super) fn style(tone: Tone, theme: Theme) -> Style {
    // Never paint a background: Ghostty and other terminals own opacity.
    let base = Style::default().fg(Color::Reset).bg(Color::Reset);
    match tone {
        Tone::Normal | Tone::Code => base,
        Tone::Strong => base.add_modifier(Modifier::BOLD),
        Tone::Accent => base.fg(accent(theme)).add_modifier(Modifier::BOLD),
        Tone::Helper => base.fg(HELPER),
        Tone::Failure => base.fg(RED).add_modifier(Modifier::BOLD),
        Tone::Warning => base.fg(WARN).add_modifier(Modifier::BOLD),
        Tone::Success => base.fg(GREEN),
        Tone::Muted => base.fg(MUTED),
        Tone::Line => base.fg(LINE),
        Tone::You => base.fg(YOU).add_modifier(Modifier::BOLD),
        // A sprite's pixel is the one place a background is painted: the
        // lower half of a half block is its colour.
        Tone::Pixel(fg, bg) => Style::default()
            .fg(fg.map_or(Color::Reset, crate::tui::theme::rgb))
            .bg(bg.map_or(Color::Reset, crate::tui::theme::rgb)),
    }
}
/// `--you`: the person's own turn, one hue no other role uses.
const YOU: Color = Color::Rgb(0xc9, 0xb8, 0xff);
/// The one filled thing on the screen: a chip that is the current choice, or
/// one a finger is on. Painted in the accent, with dark ink on it, because
/// every accent is light; mono, whose accent is the terminal's own
/// foreground, reverses instead.
pub(super) fn chip_on(theme: Theme) -> Style {
    match accent(theme) {
        Color::Reset => Style::default()
            .fg(Color::Reset)
            .bg(Color::Reset)
            .add_modifier(Modifier::REVERSED | Modifier::BOLD),
        accent => Style::default()
            .fg(Color::Black)
            .bg(accent)
            .add_modifier(Modifier::BOLD),
    }
}
