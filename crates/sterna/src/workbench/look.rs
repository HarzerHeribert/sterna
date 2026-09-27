//! How this terminal is painted: on a dark or a light ground, and with or
//! without true colour.
//!
//! **One place turns a role into a colour this terminal shows.** The
//! frame's look is set once at the top of [`super::view::render`], on the
//! thread that draws it, and [`super::theme`] reads it for every style: on a
//! light ground each role takes a variant that stays readable there, and a
//! terminal without true colour gets the nearest of its 256 colours rather
//! than escapes it cannot show.
use ratatui::style::Color;
use std::cell::Cell;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Look {
    pub light: bool,
    pub truecolor: bool,
}

thread_local! {
    static LOOK: Cell<Look> = const {
        Cell::new(Look {
            light: false,
            truecolor: true,
        })
    };
}

/// The look every style on this thread is drawn with, until the next frame.
pub(super) fn set(look: Look) {
    LOOK.with(|cell| cell.set(look));
}

pub(super) fn get() -> Look {
    LOOK.with(Cell::get)
}

/// The grounds text is read against: a dark terminal's and a light one's.
pub const DARK_GROUND: u32 = 0x0a0e12;
pub const LIGHT_GROUND: u32 = 0xf4f6f8;

fn channels(rgb: u32) -> [f64; 3] {
    [rgb >> 16, (rgb >> 8) & 0xff, rgb & 0xff].map(|c| f64::from(c) / 255.0)
}

/// Relative luminance, as WCAG defines it.
fn luminance(rgb: u32) -> f64 {
    let [r, g, b] = channels(rgb).map(|c| {
        if c <= 0.03928 {
            c / 12.92
        } else {
            ((c + 0.055) / 1.055).powf(2.4)
        }
    });
    0.2126 * r + 0.7152 * g + 0.0722 * b
}

/// The WCAG contrast ratio of two colours, 1 to 21.
#[must_use]
pub fn contrast(a: u32, b: u32) -> f64 {
    let (a, b) = (luminance(a), luminance(b));
    (a.max(b) + 0.05) / (a.min(b) + 0.05)
}

/// `rgb` moved toward black on a light ground, or toward white on a dark
/// one, until it reads at `ratio` against that ground.
#[must_use]
pub fn readable(rgb: u32, light: bool, ratio: f64) -> u32 {
    let (ground, toward) = if light {
        (LIGHT_GROUND, 0.0)
    } else {
        (DARK_GROUND, 255.0)
    };
    let mut colour = rgb;
    for _ in 0..40 {
        if contrast(colour, ground) >= ratio {
            break;
        }
        let [r, g, b] = channels(colour).map(|c| {
            let c = c * 255.0;
            (c + (toward - c) * 0.08).round() as u32
        });
        colour = (r << 16) | (g << 8) | b;
    }
    colour
}

/// A colour as this terminal can show it.
#[must_use]
pub fn paint(colour: Color, look: Look) -> Color {
    match colour {
        Color::Rgb(r, g, b) if !look.truecolor => Color::Indexed(ansi256(r, g, b)),
        other => other,
    }
}

/// What a colour is in red, green and blue: its own value, or the one a
/// terminal's default palette gives a named colour.
fn rgb_of(colour: Color) -> Option<u32> {
    Some(match colour {
        Color::Rgb(r, g, b) => u32::from_be_bytes([0, r, g, b]),
        Color::Black => 0x000000,
        Color::Red => 0xcd0000,
        Color::Green => 0x00cd00,
        Color::Yellow => 0xcdcd00,
        Color::Blue => 0x0000ee,
        Color::Magenta => 0xcd00cd,
        Color::Cyan => 0x00cdcd,
        Color::Gray => 0xe5e5e5,
        Color::DarkGray => 0x7f7f7f,
        Color::LightRed => 0xff0000,
        Color::LightGreen => 0x00ff00,
        Color::LightYellow => 0xffff00,
        Color::LightBlue => 0x5c5cff,
        Color::LightMagenta => 0xff00ff,
        Color::LightCyan => 0x00ffff,
        Color::White => 0xffffff,
        _ => return None,
    })
}

/// The colours a drawing that predates the look chose -- the instruments
/// -- taken through it, for `area`: on a light ground each colour written on
/// the terminal's own background is moved until it reads there, and without
/// true colour every colour is the nearest of the 256.
pub(super) fn adopt(buffer: &mut ratatui::buffer::Buffer, area: ratatui::layout::Rect, look: Look) {
    let area = area.intersection(buffer.area);
    for y in area.top()..area.bottom() {
        for x in area.left()..area.right() {
            let cell = &mut buffer[(x, y)];
            if look.light
                && cell.bg == Color::Reset
                && let Some(rgb) = rgb_of(cell.fg)
            {
                cell.fg = crate::tui::theme::rgb(readable(rgb, true, 4.5));
            }
            cell.fg = paint(cell.fg, look);
            cell.bg = paint(cell.bg, look);
        }
    }
}

/// The nearest of the 256 colours: the 6×6×6 cube or the grey ramp,
/// whichever is closer.
fn ansi256(r: u8, g: u8, b: u8) -> u8 {
    const LEVELS: [u8; 6] = [0, 95, 135, 175, 215, 255];
    let step = |c: u8| {
        LEVELS
            .iter()
            .enumerate()
            .min_by_key(|(_, level)| (i32::from(**level) - i32::from(c)).abs())
            .map_or(0, |(i, _)| i as u8)
    };
    let (ri, gi, bi) = (step(r), step(g), step(b));
    let cube = [
        LEVELS[ri as usize],
        LEVELS[gi as usize],
        LEVELS[bi as usize],
    ];
    let grey_step = ((u16::from(r) + u16::from(g) + u16::from(b)) / 3)
        .saturating_sub(8)
        .div_ceil(10)
        .min(23) as u8;
    let grey = 8 + 10 * grey_step;
    let distance = |[x, y, z]: [u8; 3]| {
        [(x, r), (y, g), (z, b)]
            .iter()
            .map(|(a, b)| (i32::from(*a) - i32::from(*b)).pow(2))
            .sum::<i32>()
    };
    if distance([grey, grey, grey]) < distance(cube) {
        232 + grey_step
    } else {
        16 + 36 * ri + 6 * gi + bi
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_colour_is_moved_until_it_reads() {
        let yellow = 0xf7d23a;
        assert!(contrast(yellow, LIGHT_GROUND) < 2.0);
        assert!(contrast(readable(yellow, true, 4.5), LIGHT_GROUND) >= 4.5);
        assert_eq!(readable(0x99a6b7, false, 4.5), 0x99a6b7, "already readable");
    }

    #[test]
    fn without_true_colour_a_colour_is_one_of_256() {
        let look = Look {
            light: false,
            truecolor: false,
        };
        assert_eq!(paint(Color::Rgb(255, 0, 0), look), Color::Indexed(196));
        assert_eq!(paint(Color::Rgb(128, 128, 128), look), Color::Indexed(244));
        assert_eq!(paint(Color::Reset, look), Color::Reset);
    }
}
