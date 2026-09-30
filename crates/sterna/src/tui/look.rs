//! How much of the workbench moves (`ui.motion`).
//!
//! **Motion is a signal, never decoration.** Something moves only where a
//! state is changing -- arriving prose, a running cell, a subagent that is
//! working, a result in the moment it lands -- and an idle screen
//! is still but for one slow heartbeat. The loop that owns the clock reads
//! its periods from here, so the rate a level promises is the rate drawn.
use std::time::Duration;

use super::{Activity, ScreenState};

/// How much moves. `Off` is fully static: every glyph holds a complete
/// resting frame and no frame is drawn for motion's sake.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Motion {
    #[default]
    Full,
    /// A third of the frame rate, no idle heartbeat and no scanner: the
    /// marks that say *working* still turn, slowly.
    Calm,
    Off,
}
impl Motion {
    pub const ALL: [Self; 3] = [Self::Full, Self::Calm, Self::Off];
    pub fn parse(name: &str) -> Option<Self> {
        match name {
            "full" | "on" => Some(Self::Full),
            "calm" => Some(Self::Calm),
            "off" | "reduce" => Some(Self::Off),
            _ => None,
        }
    }
    pub fn name(self) -> &'static str {
        match self {
            Self::Full => "full",
            Self::Calm => "calm",
            Self::Off => "off",
        }
    }
    /// How long one frame lasts while something works at this level.
    pub fn period(self) -> Duration {
        match self {
            Self::Full => FULL_FRAME,
            Self::Calm => CALM_FRAME,
            Self::Off => STILL_FRAME,
        }
    }
}

/// One frame while something works, at full motion: about eight a second,
/// which is the ceiling the loop ever redraws at for motion's sake.
pub const FULL_FRAME: Duration = Duration::from_millis(120);
/// One frame at calm motion.
pub const CALM_FRAME: Duration = Duration::from_millis(360);
/// With motion off the loop still ticks once a second while a task runs,
/// for the elapsed clock -- a fact, not an animation.
pub const STILL_FRAME: Duration = Duration::from_millis(1000);
/// The idle heartbeat: one cell, every two seconds, at full motion only.
pub const HEARTBEAT: Duration = Duration::from_millis(2000);
/// How long the heartbeat's cell stays lit.
pub const BEAT: Duration = Duration::from_millis(240);
/// How many frames a result or a verdict is emphasised after it lands.
pub const SETTLE_FRAMES: usize = 6;

impl ScreenState {
    /// Sets the motion level and keeps [`ScreenState::reduced_motion`] --
    /// which every renderer reads as *hold still* -- equal to `Off`.
    pub fn set_motion(&mut self, motion: Motion) {
        self.motion = motion;
        self.reduced_motion = motion == Motion::Off;
        if self.reduced_motion {
            self.completion_tick = None;
            self.note_landing = None;
        }
    }
    /// Whether decoration may move in this frame: motion is on and no drag
    /// is selecting text, which must hold still under the hand.
    #[must_use]
    pub fn motion_live(&self) -> bool {
        !self.reduced_motion && self.motion != Motion::Off && self.selection.is_none()
    }
    /// How long one frame lasts while something is working.
    #[must_use]
    pub fn frame_period(&self) -> Duration {
        if self.reduced_motion {
            STILL_FRAME
        } else {
            self.motion.period()
        }
    }
    /// When an idle screen draws next: at full motion a beat -- the mark
    /// lit on an odd frame for [`BEAT`], dark for [`HEARTBEAT`] -- and
    /// nothing at all otherwise.
    #[must_use]
    pub fn heartbeat(&self) -> Option<Duration> {
        (self.motion == Motion::Full && !self.reduced_motion && self.activity == Activity::Idle)
            .then_some(if self.animation_frame % 2 == 1 {
                BEAT
            } else {
                HEARTBEAT
            })
    }
    /// Whether anything on screen is changing state and so owes frames: a
    /// result or a note settling.
    #[must_use]
    pub fn settling(&self) -> bool {
        self.completion_tick.is_some() || self.note_landing.is_some()
    }
    /// One frame's worth of settling: a landed result or verdict counts
    /// down and then goes still for good.
    pub fn advance_landing(&mut self) {
        self.note_landing = self
            .note_landing
            .and_then(|(index, tick)| (tick + 1 < SETTLE_FRAMES).then_some((index, tick + 1)));
    }
    /// A note just arrived: a note the gate left beside the answer is
    /// emphasised for a few frames, then settles.
    pub fn landed_note(&mut self) {
        let Some(last) = self.history.len().checked_sub(1) else {
            return;
        };
        let flagged = super::NoteKind::of(&self.history[last].text) == super::NoteKind::Flagged;
        if flagged && self.motion_live() {
            self.note_landing = Some((last, 0));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn off_is_still_and_draws_no_idle_frame() {
        let mut s = ScreenState::default();
        assert!(s.heartbeat().is_some(), "full motion beats when idle");
        s.set_motion(Motion::Calm);
        assert_eq!(s.frame_period(), CALM_FRAME);
        assert!(s.heartbeat().is_none());
        s.set_motion(Motion::Off);
        assert!(s.reduced_motion && !s.motion_live());
        assert!(s.heartbeat().is_none());
        assert_eq!(s.frame_period(), STILL_FRAME);
        assert_eq!(Motion::parse("on"), Some(Motion::Full));
    }

    #[test]
    fn a_gate_note_settles_and_a_notice_does_not() {
        let mut s = ScreenState::default();
        s.note("Theme: amber");
        s.landed_note();
        assert!(s.note_landing.is_none());
        s.note(format!("{}no test ran", super::super::history::NOTED));
        s.landed_note();
        assert_eq!(s.note_landing, Some((1, 0)));
        for _ in 0..SETTLE_FRAMES {
            s.advance_landing();
        }
        assert!(s.note_landing.is_none(), "it goes still");
    }
}
