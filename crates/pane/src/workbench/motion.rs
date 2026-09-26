//! Every glyph on the workbench that moves.
//!
//! **Only what is changing state moves, and each thing moves in one cell or
//! two.** The dock's mark while a turn runs, the caret on arriving prose,
//! the scanner in a running cell, the mark of a helper or of work behind
//! the answer -- and nothing else: no border, rule, chip or header ever
//! takes a frame. Each function here returns a complete resting glyph when
//! [`ScreenState::motion_live`] is false, so `off` is a whole drawing, not
//! a paused one. The parrot's own art is in [`super::plumage`].
use super::{Tone, voice};
use crate::tui::{Activity, Motion, ScreenState};

/// Six frames, so two draws a multiple of four frames apart still differ.
const ORBIT: [&str; 6] = ["⠋", "⠙", "⠸", "⠴", "⠦", "⠇"];
const TURN: [&str; 4] = ["◐", "◓", "◑", "◒"];
const LEVEL: [&str; 8] = ["▁", "▂", "▃", "▄", "▅", "▆", "▇", "█"];
const CARET: [&str; 4] = ["▍", "▌", "▋", "▌"];
/// How wide the scanner in a running cell is.
const SCAN: usize = 10;

fn frame(s: &ScreenState) -> usize {
    s.animation_frame
}

/// The mark before the dock's status word: which kind of wait this is --
/// an orbit while the model thinks, the size of the last delivery while it
/// streams, a turning disc while a cell runs -- and at rest a diamond whose
/// one slow heartbeat is the idle screen's only motion.
pub(super) fn dock_mark(s: &ScreenState) -> String {
    let live = s.motion_live();
    match s.activity {
        Activity::Complete => "✓".into(),
        Activity::Failed => "✕".into(),
        Activity::Idle | Activity::Starting => if live && frame(s) % 2 == 1 {
            "◆"
        } else {
            "◇"
        }
        .into(),
        _ if !live => "◆".into(),
        Activity::Streaming => level(s).into(),
        Activity::Executing => TURN[frame(s) % TURN.len()].into(),
        _ => ORBIT[frame(s) % ORBIT.len()].into(),
    }
}

/// The last delivery's size against the largest of the recent ones: the
/// streaming mark is a reading of the transport, so it moves only when text
/// actually arrives.
fn level(s: &ScreenState) -> &'static str {
    let recent = &s.pulse.deliveries;
    let (Some(last), Some(most)) = (recent.last(), recent.iter().max()) else {
        return LEVEL[0];
    };
    LEVEL[(last * (LEVEL.len() - 1)) / (*most).max(1)]
}

/// The caret at the end of prose that is still arriving -- the model's
/// thinking as it is written. It breathes in one cell, and holds its still
/// frame unless this row carries the document's one motion.
pub(super) fn caret_moving(s: &ScreenState, moving: bool) -> &'static str {
    if moving && s.motion_live() {
        CARET[frame(s) % CARET.len()]
    } else {
        CARET[0]
    }
}

/// The one-cell mark of something small at work: a helper waiting on its
/// answer, a cell being written, the checker behind the answer. It holds
/// its still frame unless this row carries the document's one motion.
pub(super) fn busy_moving(s: &ScreenState, moving: bool) -> &'static str {
    if moving && s.motion_live() {
        ORBIT[frame(s) % ORBIT.len()]
    } else {
        "◌"
    }
}

/// The scanner beside a running cell's label: a two-cell head sweeping a
/// quiet track, so a long cell visibly lives. Two cells change a frame;
/// calm and off hold the head at the start.
pub(super) fn scanner(s: &ScreenState) -> Vec<(String, Tone)> {
    let head = if s.motion_live() && s.motion == Motion::Full {
        // Out and back: 0..SCAN-2 and down again.
        let span = SCAN - 2;
        let t = frame(s) % (2 * span);
        if t <= span { t } else { 2 * span - t }
    } else {
        0
    };
    // No empty span: the view stops a row at the first zero-width one.
    [
        ("·".repeat(head), Tone::Line),
        ("━━".to_string(), Tone::Accent),
        ("·".repeat(SCAN - 2 - head), Tone::Line),
    ]
    .into_iter()
    .filter(|(text, _)| !text.is_empty())
    .collect()
}

/// The still mark for the session card: what state the
/// session is in, in one glyph. The card is chrome, so it never moves.
pub(super) fn card_mark(face: voice::Face) -> &'static str {
    match face {
        voice::Face::Idle => "◇",
        voice::Face::Thinking => "◈",
        voice::Face::Working => "◆",
        voice::Face::Done => "✓",
        voice::Face::Asking => "?",
        voice::Face::Oops => "✕",
    }
}

/// Whether the latest result is in its settle frames.
pub(super) fn settling(s: &ScreenState) -> bool {
    s.motion_live() && s.completion_tick.is_some_and(|t| t < 5)
}

/// Whether the note at `index` in the history is the verdict just landed.
pub(super) fn note_settling(s: &ScreenState, index: usize) -> bool {
    s.motion_live() && s.note_landing.is_some_and(|(at, _)| at == index)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn changed(a: &[(String, Tone)], b: &[(String, Tone)]) -> usize {
        let paint = |spans: &[(String, Tone)]| -> Vec<(char, Tone)> {
            spans
                .iter()
                .flat_map(|(t, tone)| t.chars().map(move |c| (c, *tone)))
                .collect()
        };
        paint(a)
            .iter()
            .zip(paint(b))
            .filter(|(x, y)| **x != *y)
            .count()
    }

    #[test]
    fn the_scanner_moves_two_cells_a_frame_and_holds_still_when_off() {
        let mut s = ScreenState::default();
        for tick in 0..40 {
            s.animation_frame = tick;
            let a = scanner(&s);
            s.animation_frame = tick + 1;
            let b = scanner(&s);
            assert!(changed(&a, &b) <= 2, "frame {tick}");
            assert_eq!(
                a.iter().map(|(t, _)| t.chars().count()).sum::<usize>(),
                SCAN
            );
        }
        s.set_motion(Motion::Off);
        let a = scanner(&s);
        s.animation_frame = 7;
        assert_eq!(a, scanner(&s));
        s.set_motion(Motion::Calm);
        assert_eq!(a, scanner(&s), "calm holds the scanner too");
    }

    #[test]
    fn every_moving_mark_has_a_complete_resting_frame() {
        let mut s = ScreenState::default();
        s.set_motion(Motion::Off);
        for activity in [
            Activity::Idle,
            Activity::Thinking,
            Activity::Streaming,
            Activity::Executing,
        ] {
            s.activity = activity;
            let rest = (dock_mark(&s), caret_moving(&s, true), busy_moving(&s, true));
            for tick in 0..12 {
                s.animation_frame = tick;
                assert_eq!(
                    rest,
                    (dock_mark(&s), caret_moving(&s, true), busy_moving(&s, true)),
                    "{activity:?}"
                );
            }
        }
    }

    #[test]
    fn the_streaming_mark_reads_the_transport() {
        let mut s = ScreenState {
            activity: Activity::Streaming,
            ..ScreenState::default()
        };
        s.pulse.deliveries = vec![80, 10];
        assert_eq!(dock_mark(&s), "▁");
        s.pulse.deliveries.push(80);
        assert_eq!(dock_mark(&s), "█");
    }
}
