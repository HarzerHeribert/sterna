//! One palette, read from the workbench mockup, expressed as terminal colour.
//!
//! **The background is never painted and normal prose keeps the terminal's own
//! foreground.** Everything else is a role — accent, evidence, failure, warning,
//! success, muted, rule — and a role is one fixed colour per ground so that
//! two surfaces drawn by different code cannot disagree about what "muted"
//! looks like. Only the accent moves with the chosen theme, exactly as the
//! mockup's `--accent` custom property does. On a light ground every role
//! takes its light variant, and the accent is darkened until it reads; on a
//! terminal without true colour every colour is the nearest of 256
//! ([`super::look`]).
use super::Tone;
use super::look::{self, Look};
use crate::tui::Theme;
use ratatui::style::{Color, Modifier, Style};

/// `--accent` per theme, as the mockup's dark shell defines it.
fn dark_accent(theme: Theme) -> Option<u32> {
    Some(match theme {
        Theme::Neon => 0xdaff50,
        Theme::Amber => 0xffce72,
        Theme::Ice => 0x8be3ff,
        // Mono keeps the terminal's own foreground: its accent is emphasis,
        // not hue, and a user who chose mono asked for exactly that.
        Theme::Mono => return None,
        Theme::Violet => 0xd4b4ff,
        Theme::Cobalt => 0x9ec9ff,
        Theme::Mint => 0x86f1d0,
        Theme::Rose => 0xffb3d4,
        Theme::Bird(bird) => bird.plumage().accent,
    })
}

/// The accent on this frame's ground: the dark shell's, or on a light ground
/// the same hue darkened until it reads as text.
fn accent_rgb(theme: Theme, look: Look) -> Option<u32> {
    let dark = dark_accent(theme)?;
    Some(if look.light {
        look::readable(dark, true, 4.5)
    } else {
        dark
    })
}

/// The accent's colour on this frame's ground, for a swatch; mono has none.
pub(super) fn accent_value(theme: Theme) -> Option<u32> {
    accent_rgb(theme, look::get())
}

pub(super) fn accent(theme: Theme) -> Color {
    let look = look::get();
    accent_rgb(theme, look).map_or(Color::Reset, |rgb| paint(rgb, look))
}

/// A role's colour on a dark ground and on a light one. The light ones hold
/// 4.5:1 against a light terminal's white (3:1 for a rule).
struct Role {
    dark: u32,
    light: u32,
}

/// `--muted`: technical detail that must stay readable, never decoration.
const MUTED: Role = Role {
    dark: 0x99a6b7,
    light: 0x56606c,
};
/// `--line`: rules and separators, the only thing quieter than muted.
const LINE: Role = Role {
    dark: 0x556476,
    light: 0x7d8894,
};
/// `--cyan`: observed evidence, what came back from a call.
const EVIDENCE: Role = Role {
    dark: 0x8be3ff,
    light: 0x0b6e8a,
};
const WARN: Role = Role {
    dark: 0xffca80,
    light: 0x8a5a00,
};
const RED: Role = Role {
    dark: 0xff8494,
    light: 0xb3261e,
};
const GREEN: Role = Role {
    dark: 0xa4f1bd,
    light: 0x1e7b3c,
};
/// `--you`: the person's own turn, one hue no other role uses.
const YOU: Role = Role {
    dark: 0xc9b8ff,
    light: 0x5b3fb5,
};

fn paint(rgb: u32, look: Look) -> Color {
    look::paint(crate::tui::theme::rgb(rgb), look)
}

fn role(role: &Role, look: Look) -> Color {
    paint(if look.light { role.light } else { role.dark }, look)
}

pub(super) fn style(tone: Tone, theme: Theme) -> Style {
    let look = look::get();
    // Never paint a background: Ghostty and other terminals own opacity.
    let base = Style::default().fg(Color::Reset).bg(Color::Reset);
    // Mono is monochrome: evidence and the person are told apart by weight,
    // not hue.
    let mono = theme == Theme::Mono;
    match tone {
        Tone::Normal | Tone::Code => base,
        Tone::Strong => base.add_modifier(Modifier::BOLD),
        Tone::Accent => base.fg(accent(theme)).add_modifier(Modifier::BOLD),
        Tone::Evidence if mono => base,
        Tone::Evidence => base.fg(role(&EVIDENCE, look)),
        Tone::Failure => base.fg(role(&RED, look)).add_modifier(Modifier::BOLD),
        Tone::Warning => base.fg(role(&WARN, look)).add_modifier(Modifier::BOLD),
        Tone::Success => base.fg(role(&GREEN, look)),
        Tone::Muted => base.fg(role(&MUTED, look)),
        Tone::Line => base.fg(role(&LINE, look)),
        Tone::You if mono => base.add_modifier(Modifier::BOLD),
        Tone::You => base.fg(role(&YOU, look)).add_modifier(Modifier::BOLD),
        // A sprite's pixel is the one place a background is painted: the
        // lower half of a half block is its colour.
        Tone::Pixel(fg, bg) => Style::default()
            .fg(fg.map_or(Color::Reset, |rgb| paint(rgb, look)))
            .bg(bg.map_or(Color::Reset, |rgb| paint(rgb, look))),
    }
}
/// The one filled thing on the screen: a chip that is the current choice, or
/// one a finger is on. Painted in the accent, with whichever of black and
/// white ink reads on it; mono, whose accent is the terminal's own
/// foreground, reverses instead.
pub(super) fn chip_on(theme: Theme) -> Style {
    let look = look::get();
    match accent_rgb(theme, look) {
        None => Style::default()
            .fg(Color::Reset)
            .bg(Color::Reset)
            .add_modifier(Modifier::REVERSED | Modifier::BOLD),
        Some(accent) => {
            let ink = if look::contrast(accent, 0x000000) >= look::contrast(accent, 0xffffff) {
                0x000000
            } else {
                0xffffff
            };
            Style::default()
                .fg(paint(ink, look))
                .bg(paint(accent, look))
                .add_modifier(Modifier::BOLD)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every role reads on the ground it is drawn for: text at 4.5:1, a rule
    /// at 3:1, and on a dark ground dim text is at least #7F8C98's worth.
    #[test]
    fn every_role_reads_on_its_ground() {
        for (role, ratio) in [
            (&MUTED, 4.5),
            (&EVIDENCE, 4.5),
            (&WARN, 4.5),
            (&RED, 4.5),
            (&GREEN, 4.5),
            (&YOU, 4.5),
            (&LINE, 3.0),
        ] {
            assert!(
                look::contrast(role.light, look::LIGHT_GROUND) >= ratio,
                "{:06x} on light",
                role.light
            );
        }
        assert!(
            look::contrast(MUTED.dark, look::DARK_GROUND)
                >= look::contrast(0x7f8c98, look::DARK_GROUND)
        );
    }

    /// No bird's accent is the failure red, so a bird's chrome never reads
    /// as an error.
    #[test]
    fn no_birds_accent_reads_as_failure() {
        let hue = |rgb: u32| {
            let [r, g, b] = [rgb >> 16, (rgb >> 8) & 0xff, rgb & 0xff].map(f64::from);
            let (max, min) = (r.max(g).max(b), r.min(g).min(b));
            let d = max - min;
            if d == 0.0 {
                0.0
            } else if max == r {
                60.0 * ((g - b) / d).rem_euclid(6.0)
            } else if max == g {
                60.0 * ((b - r) / d + 2.0)
            } else {
                60.0 * ((r - g) / d + 4.0)
            }
        };
        for bird in crate::workbench::plumage::Bird::ALL {
            let apart = (hue(bird.plumage().accent) - hue(RED.dark)).abs();
            let apart = apart.min(360.0 - apart);
            assert!(
                apart >= 30.0,
                "{bird:?}'s accent is {apart:.0}° from failure red"
            );
        }
    }
}
