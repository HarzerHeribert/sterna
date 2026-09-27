//! The mode proposed from the request's intent (map 2639):
//! `docs/decisions.md`.

use super::*;
use crate::sandbox::modes::RequestMode;

/// What [`propose`] decided for one request, and what it hands
/// [`TaskState::decisions_telemetry`](super::task::TaskState::decisions_telemetry)
/// (`session/task.rs`).
#[derive(Debug, Clone, Copy)]
pub(super) struct Proposal {
    /// The mode this one request is narrowed to. `session.mode` is never
    /// touched: a proposal narrows one request, not the session.
    pub(super) narrow_mode: RequestMode,
    /// A `read_only` intent cleared `mode_above` this request.
    pub(super) proposed: bool,
    /// `mode = on` and [`Self::proposed`]: [`Self::narrow_mode`] is `Explore`.
    pub(super) applied: bool,
    /// `mode = shadow` and [`Self::proposed`]: nothing narrowed, only counted.
    pub(super) would_apply: bool,
    pub(super) pinned: bool,
}

/// Given the task's own intent answer, whether this request proposes
/// `explore` (2639). `session.mode.get() != Execute`, a pin, no model,
/// `mode = off`, a non-`read_only` intent or a failed decision all leave
/// [`Proposal::narrow_mode`] at the session's mode, unchanged -- the same
/// fail-open shape as the hold (`decision-model.md` §3). `mode = shadow`
/// counts [`Proposal::would_apply`] and prints nothing, exactly as the hold's
/// own shadow counts `would_hold` silently.
pub(super) fn propose(
    session: &Session<'_>,
    decision: Option<&crate::decide::TaskDecision>,
) -> Proposal {
    let decisions = session.config().decisions.clone();
    let mut proposal = Proposal {
        narrow_mode: session.mode.get(),
        proposed: false,
        applied: false,
        would_apply: false,
        pinned: session.mode_pinned.get(),
    };
    if decisions.model.is_none()
        || decisions.mode == crate::config::DecisionMode::Off
        || proposal.narrow_mode != RequestMode::Execute
        || proposal.pinned
    {
        return proposal;
    }
    let Some(intent) = decision.map(|decision| &decision.intent) else {
        return proposal;
    };
    if intent.choice != crate::decide::READ_ONLY || intent.confidence < 0.5 {
        return proposal;
    }
    if intent.confidence < decisions.mode_above {
        if decisions.mode == crate::config::DecisionMode::On {
            session_println!(
                "decision: {} {:.2} below mode_above; /mode explore to pin",
                intent.choice,
                intent.confidence
            );
        }
        return proposal;
    }
    proposal.proposed = true;
    match decisions.mode {
        crate::config::DecisionMode::On => {
            proposal.applied = true;
            proposal.narrow_mode = RequestMode::Explore;
            session_println!(
                "decision: explore for this request ({} {:.2}); /mode execute to pin",
                intent.choice,
                intent.confidence
            );
        }
        crate::config::DecisionMode::Shadow => proposal.would_apply = true,
        crate::config::DecisionMode::Off => unreachable!("returned above"),
    }
    proposal
}
