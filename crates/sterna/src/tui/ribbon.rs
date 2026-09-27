//! The activity band: a raking dither sweep, in the transcript's own ramp.
//!
//! **This is the one thing on screen whose whole job is "is it alive".** It
//! carries no measurement -- the numbers live in the telemetry rail and the
//! status row -- so it may be decorative, but it may never read as stopped
//! while a turn is running. Everything below serves that one question.
//!
//! **It speaks the poster's language.** `░▒▓█` are [`super::poster`]'s ramp,
//! accent-coloured glyphs on the normal ground rather than a filled slab,
//! which is what keeps Mono legible: Mono's accent is white, and a white
//! background band would be a searchlight across the composer. The sine
//! ribbon this replaces baked its own RGB -- including a channel-swapped
//! second colour -- so it was the one surface that ignored the theme.
//!
//! **It reacts to arrivals rather than only to the clock.** The head advances
//! every frame, so a thinking turn that has produced nothing yet still moves;
//! and the band widens with the bytes actually delivered, so a turn that is
//! streaming hard looks different from one that is waiting on a first token.
//! A purely byte-driven band would freeze during thinking, which is the one
//! failure this surface cannot have.

use super::{Activity, ScreenState};
use ratatui::{
    Frame,
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
};

/// The dither ramp, lightest first. Four steps and no more: a hard band is
/// the house style, and a longer ramp would read as a gradient.
const RAMP: [char; 4] = ['░', '▒', '▓', '█'];

/// How far the head moves per frame. One column per frame at the 50ms draw
/// budget crosses an eighty-column band in four seconds, which reads as
/// purposeful rather than frantic.
const SPEED: usize = 1;

/// The band's width before arrivals widen it, and the most they may widen it
/// by. A floor, because a thinking turn with no deliveries still has to move
/// something; a ceiling, because a band as wide as the region is a fill and
/// not a sweep.
const CORE: usize = 6;
const SWELL: usize = 10;

/// How wide the band is this frame, given the bytes recently delivered.
///
/// Deliveries are the transport's own record of what arrived. A turn that is
/// thinking has none and gets [`CORE`]; one that is streaming swells toward
/// `CORE + SWELL`. The scale is deliberately coarse -- this is a sign of
/// life, not a rate chart, and the telemetry rail is where a number belongs.
fn band_width(deliveries: &[usize]) -> usize {
    let recent: usize = deliveries.iter().rev().take(4).sum();
    CORE + (recent / 96).min(SWELL)
}

/// The ramp step at `distance` columns behind the head, or `None` for ground.
///
/// The falloff is by whole steps rather than by a curve: the band is four
/// hard edges, which is what makes it read as print rather than as glow.
fn step(distance: usize, band: usize) -> Option<char> {
    if distance >= band {
        return None;
    }
    let bucket = distance * RAMP.len() / band.max(1);
    Some(RAMP[(RAMP.len() - 1).saturating_sub(bucket.min(RAMP.len() - 1))])
}

/// One row of the sweep.
///
/// Each row's head trails the one above it by `row * 2`, so the band rakes
/// across rather than marching square -- the diagonal is the only thing here
/// that is purely a look, and it costs nothing.
fn row(width: usize, head: usize, band: usize, offset: usize) -> String {
    let span = width + band;
    (0..width)
        .map(|x| {
            let head = (head + span - (offset % span)) % span;
            let distance = (x + span - head) % span;
            step(distance, band).unwrap_or(' ')
        })
        .collect()
}

/// A bar whose weight changes in place and never travels.
///
/// **Thinking is pressure that goes nowhere**, so its form does not move
/// across the strip. A travelling band would say the same thing as receiving,
/// which is the confusion this whole dispatch exists to remove.
fn breathing(width: usize, band: usize, frame: usize, offset: usize) -> String {
    let step = (frame / 2 + offset) % (RAMP.len() * 2);
    let weight = if step < RAMP.len() {
        step
    } else {
        RAMP.len() * 2 - 1 - step
    };
    let glyph = RAMP[weight.min(RAMP.len() - 1)];
    let start = width.saturating_sub(band) / 2;
    (0..width)
        .map(|x| {
            if x >= start && x < start + band {
                glyph
            } else {
                ' '
            }
        })
        .collect()
}

/// Two heads leaving the centre and returning to it.
///
/// **Searching goes out and comes back**, which is the one thing that makes
/// it not executing. The pair is symmetric about the centre, so the return is
/// as legible as the departure.
fn sweeping_out(width: usize, band: usize, frame: usize, offset: usize) -> String {
    let half = width / 2;
    let span = half.max(1);
    let phase = (frame + offset) % (span * 2);
    let reach = if phase < span {
        phase
    } else {
        span * 2 - phase
    };
    let mut row = vec![' '; width];
    for depth in 0..band.min(RAMP.len() * 2) {
        let glyph = RAMP[(RAMP.len() - 1).saturating_sub(depth * RAMP.len() / band.max(1))];
        for side in [reach.saturating_sub(depth), reach + depth] {
            for x in [half.saturating_sub(side), (half + side).min(width - 1)] {
                if x < width && row[x] == ' ' {
                    row[x] = glyph;
                }
            }
        }
    }
    row.into_iter().collect()
}

/// A hard-edged block marching one way, with no wake behind it.
///
/// **Executing goes forward and does not come back.** Solid rather than
/// dithered: a cell's program is the most definite thing the session does,
/// and the ramp is what the tentative states use.
fn marching(width: usize, band: usize, frame: usize, offset: usize) -> String {
    let span = width + band;
    let head = (frame * 2 + offset) % span;
    (0..width)
        .map(|x| {
            let distance = (x + span - head) % span;
            if distance < band { RAMP[3] } else { ' ' }
        })
        .collect()
}

/// Both ends closing toward the centre.
///
/// **Compacting is compression**, so the form compresses: the run shortens
/// from both sides rather than travelling.
fn converging(width: usize, frame: usize, offset: usize) -> String {
    let half = width / 2;
    let phase = (frame + offset) % (half.max(1));
    (0..width)
        .map(|x| {
            let from_edge = x.min(width.saturating_sub(x + 1));
            if from_edge >= phase && from_edge < phase + 2 {
                RAMP[2]
            } else {
                ' '
            }
        })
        .collect()
}

/// A sparse drift: the least motion that still proves a session is alive.
fn drifting(width: usize, frame: usize, offset: usize) -> String {
    let span = width.max(1);
    let head = (frame / 3 + offset) % span;
    (0..width)
        .map(|x| if x == head { RAMP[1] } else { ' ' })
        .collect()
}

/// The band, still or moving, in the form its state calls for.
///
/// **Every mode had the same shape and the strip said only "something is
/// happening".** The user, watching a live run (2026-09-19): *"ich finde es
/// schade, dass alle Modes … eben über dem Prompt anzeigen. Hier wäre
/// Diversity gefragt."* The forms differ structurally rather than by colour,
/// because a colour-only distinction dies on Mono and this crate has eight
/// palettes.
///
/// `reduced_motion` fixes the frame rather than emptying the band: a still
/// screen still shows what the surface is, and a reader who turned motion off
/// asked for stillness, not for a blank strip.
pub(super) fn lines(width: usize, height: usize, state: &ScreenState) -> Vec<Line<'static>> {
    let band = band_width(&state.pulse.deliveries);
    let frame = if state.reduced_motion {
        width / 3
    } else {
        state.animation_frame.wrapping_mul(SPEED)
    };
    let ink = Style::default().fg(state.theme.accent());
    (0..height)
        .map(|index| {
            let offset = index * 2;
            let text = match state.activity {
                Activity::Thinking => breathing(width, band, frame, offset),
                Activity::Searching => sweeping_out(width, band, frame, offset),
                Activity::Executing => marching(width, band, frame, offset),
                Activity::Compacting => converging(width, frame, offset),
                Activity::Waiting => drifting(width, frame, offset),
                // Receiving keeps the raking sweep, which is the one form
                // that already widened with arrivals -- a stream is the state
                // that has a rate, and the only one whose form should show it.
                _ => row(width, frame, band, offset),
            };
            Line::from(Span::styled(
                text,
                if index == 0 {
                    ink.add_modifier(Modifier::BOLD)
                } else {
                    ink
                },
            ))
        })
        .collect()
}

/// The label column's width, including its leading space.
const LABEL: u16 = 14;

pub(super) fn activity(frame: &mut Frame, area: Rect, state: &ScreenState) {
    if area.width < 24 || area.height == 0 {
        return;
    }
    let label = if state.completion_tick.is_some() {
        "complete"
    } else {
        state.activity.label()
    };
    frame.render_widget(
        Paragraph::new(format!(" {}", label.to_uppercase())).style(
            Style::default()
                .fg(state.theme.accent())
                .add_modifier(Modifier::BOLD),
        ),
        Rect::new(area.x, area.y + area.height / 2, LABEL - 1, 1),
    );
    frame.render_widget(
        Paragraph::new(lines(
            usize::from(area.width - LABEL),
            usize::from(area.height),
            state,
        )),
        Rect::new(area.x + LABEL, area.y, area.width - LABEL, area.height),
    );
}

#[cfg(test)]
mod tests {
    use super::super::Theme;
    use super::*;

    fn theme_state(theme: Theme, frame: usize, still: bool, deliveries: Vec<usize>) -> ScreenState {
        let mut state = ScreenState {
            theme,
            animation_frame: frame,
            reduced_motion: still,
            ..Default::default()
        };
        state.pulse.deliveries = deliveries;
        state
    }

    fn mode_state(activity: Activity, frame: usize) -> ScreenState {
        ScreenState {
            theme: Theme::Neon,
            activity,
            animation_frame: frame,
            ..Default::default()
        }
    }

    fn drawn(lines: &[Line<'static>]) -> Vec<String> {
        lines
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.content.to_string())
                    .collect()
            })
            .collect()
    }

    #[test]
    fn show_every_mode() {
        if std::env::var("SHOW_MODES").is_err() {
            return;
        }
        for activity in [
            Activity::Thinking,
            Activity::Streaming,
            Activity::Searching,
            Activity::Executing,
            Activity::Compacting,
            Activity::Waiting,
        ] {
            println!("--- {} ---", activity.label().to_uppercase());
            for frame in [0usize, 2, 4, 6] {
                for row in drawn(&lines(56, 2, &mode_state(activity, frame))) {
                    println!("|{row}|");
                }
                println!();
            }
        }
    }

    #[test]
    fn every_mode_is_distinguishable_from_every_other_at_one_frame() {
        let modes = [
            Activity::Thinking,
            Activity::Streaming,
            Activity::Searching,
            Activity::Executing,
            Activity::Compacting,
            Activity::Waiting,
        ];
        let shapes: Vec<Vec<String>> = modes
            .iter()
            .map(|a| drawn(&lines(56, 2, &mode_state(*a, 5))))
            .collect();
        for (i, a) in shapes.iter().enumerate() {
            for (j, b) in shapes.iter().enumerate().skip(i + 1) {
                assert_ne!(
                    a, b,
                    "{:?} and {:?} draw the same shape",
                    modes[i], modes[j]
                );
            }
        }
    }

    #[test]
    fn every_mode_moves_between_frames_and_freezes_under_reduced_motion() {
        for activity in [
            Activity::Thinking,
            Activity::Streaming,
            Activity::Searching,
            Activity::Executing,
            Activity::Compacting,
            Activity::Waiting,
        ] {
            let a = drawn(&lines(56, 2, &mode_state(activity, 6)));
            let b = drawn(&lines(56, 2, &mode_state(activity, 9)));
            assert_ne!(a, b, "{activity:?} never moves, so it reads as stalled");
            let mut still = mode_state(activity, 6);
            still.reduced_motion = true;
            let mut later = mode_state(activity, 91);
            later.reduced_motion = true;
            assert_eq!(
                drawn(&lines(56, 2, &still)),
                drawn(&lines(56, 2, &later)),
                "{activity:?} moved with motion off"
            );
        }
    }

    #[test]
    fn every_mode_fits_its_width_at_sixty_columns_and_narrower() {
        for activity in [
            Activity::Thinking,
            Activity::Searching,
            Activity::Executing,
            Activity::Compacting,
            Activity::Waiting,
            Activity::Streaming,
        ] {
            for width in [24usize, 46, 60, 120] {
                for line in lines(width, 2, &mode_state(activity, 7)) {
                    assert_eq!(line.width(), width, "{activity:?} at {width}");
                }
            }
        }
    }

    #[test]
    fn the_band_moves_between_consecutive_frames() {
        let a = lines(60, 2, &theme_state(Theme::Neon, 7, false, vec![]));
        let b = lines(60, 2, &theme_state(Theme::Neon, 8, false, vec![]));
        assert_ne!(
            format!("{a:?}"),
            format!("{b:?}"),
            "a live band that does not move reads as a stalled session"
        );
    }

    #[test]
    fn a_thinking_turn_with_no_arrivals_still_moves() {
        let a = lines(60, 2, &theme_state(Theme::Neon, 3, false, vec![]));
        let b = lines(60, 2, &theme_state(Theme::Neon, 4, false, vec![]));
        assert_ne!(format!("{a:?}"), format!("{b:?}"));
    }

    #[test]
    fn reduced_motion_freezes_the_band_without_emptying_it() {
        let a = lines(60, 2, &theme_state(Theme::Neon, 3, true, vec![]));
        let b = lines(60, 2, &theme_state(Theme::Neon, 44, true, vec![]));
        assert_eq!(format!("{a:?}"), format!("{b:?}"));
        let drawn: String = a
            .iter()
            .flat_map(|line| line.spans.iter().map(|span| span.content.to_string()))
            .collect();
        assert!(
            drawn.chars().any(|c| RAMP.contains(&c)),
            "a still band must still show what it is: {drawn:?}"
        );
    }

    #[test]
    fn arrivals_widen_the_band_and_a_quiet_turn_keeps_the_core() {
        assert_eq!(band_width(&[]), CORE);
        let busy = band_width(&[400, 400, 400, 400]);
        assert!(busy > CORE, "streaming must look different: {busy}");
        assert!(
            busy <= CORE + SWELL,
            "a sweep must not become a fill: {busy}"
        );
    }

    #[test]
    fn every_row_is_exactly_the_width_it_was_given() {
        for width in [24usize, 46, 80, 160] {
            for height in [1usize, 2, 3] {
                for line in lines(
                    width,
                    height,
                    &theme_state(Theme::Amber, 11, false, vec![64]),
                ) {
                    assert_eq!(line.width(), width, "{width}x{height} drew past its region");
                }
            }
        }
    }

    #[test]
    fn every_theme_inks_the_band_with_its_own_accent_and_no_background() {
        for theme in Theme::ALL {
            for line in lines(48, 2, &theme_state(theme, 5, false, vec![])) {
                for span in &line.spans {
                    assert_eq!(span.style.fg, Some(theme.accent()), "{theme:?} off-theme");
                    assert_eq!(
                        span.style.bg, None,
                        "{theme:?} filled a slab; Mono would be a searchlight"
                    );
                }
            }
        }
    }
}
