//! Why a task ends, and it is never a count of the work it did.
//!
//! **The user's ruling of 2026-09-17: "Limits are dumb for abstract tasks."**
//! Two counters used to end a task here — a 120-cell default and three prose
//! turns — and on the morning of that ruling the first of them ended a
//! four-hour session at cell 120 of 120, mid-implementation, with code that
//! did not compile, cancelling that session's own `cargo test` job on the way
//! out. Neither survives.
//!
//! **The user's ruling of 2026-09-19, after an audit of every cap here:
//! "alles was den Harness gegen sich selbst arbeiten lässt muss raus — ganz
//! klare Linie."** Two more enders went with it, and what is left is two:
//!
//! - a ceiling **this person set themselves**;
//! - a run of stall windows — the deterministic ender, which needs no model
//!   and is the only thing standing between an unattended session and a task
//!   that has genuinely stopped producing anything.
//!
//! What went, and why each was the harness working against itself:
//!
//! - **the supervisor's repeated verdict.** Three consecutive *model
//!   opinions* ended the task, and the criteria it matches on
//!   (`looping_over_the_same_reads`) describe exactly what a careful re-read
//!   looks like — so a supervisor with a wrong prior ended real work and the
//!   model had no appeal. Measured across three benchmark runs on
//!   2026-09-19, the supervisor's nudge fired four, two and five times and
//!   was ignored every time with no consequence: simultaneously too weak to
//!   help and strong enough to kill. It nudges now and does not end.
//! - **turns that ran no program.** A pure count of work, and
//!   `session.rs`'s own field doc had already concluded the right thing —
//!   *"a model reasoning its way toward a hard decision in prose is
//!   indistinguishable, to a counter, from a model stuck"* — before the
//!   count came back anyway. A prose turn is observed by the stall now,
//!   fingerprinted by what it said, so repeating oneself is a stall and
//!   thinking out loud is not.
//!
//! The order matters and is tested: a ceiling the person set is reported as
//! theirs before anything infers a reason on their behalf.

use crate::prompt::ExhaustedReason;

/// Everything the loop knows that bears on whether this task should end.
///
/// A plain snapshot rather than a borrow of the loop's state, so the decision
/// is a pure function of observations and can be read — and tested — without
/// a session.
pub(super) struct Ending {
    /// `[limits] cells`, when this person set one, and whether it is reached.
    pub(super) cap: Option<u64>,
    pub(super) cap_reached: bool,
    /// Whole stall windows since the last frame this task had not seen.
    pub(super) stalled_windows: u32,
}

/// The reason this task ends now, or `None` to keep going.
pub(super) fn exhausted(
    ending: &Ending,
    stall_limit: u32,
    stall_window: u32,
) -> Option<ExhaustedReason> {
    if let Some(cap) = ending.cap.filter(|_| ending.cap_reached) {
        return Some(ExhaustedReason::CellLimit { cap });
    }
    if ending.stalled_windows >= stall_limit {
        return Some(ExhaustedReason::Stalled {
            windows: ending.stalled_windows,
            cells: ending.stalled_windows * stall_window,
        });
    }
    None
}

/// Put the ending's sentence in front of the model, on **every carrier that
/// message uses**.
///
/// A model answering with `execute_cell` as a provider-native call receives
/// its feedback as a `tool_result`, not as the text answer — so a sentence
/// written only into the answer reaches exactly the sessions a tool-calling
/// model does not run. That defect was found in the supervisor's nudge on
/// 2026-09-17 and was still live here, unnoticed, until a test for the ending
/// drove a session down the native path. A task that ends without telling the
/// model why ends the same way the next time.
pub(super) fn announce(
    reason: &ExhaustedReason,
    answer: &mut Option<String>,
    historical: &mut Option<String>,
    native: Option<&mut crate::contract::Message>,
) {
    let preamble = crate::prompt::exhausted_preamble(reason);
    *answer = answer.take().map(|text| format!("{preamble}\n\n{text}"));
    *historical = historical
        .take()
        .map(|text| format!("{preamble}\n\n{text}"));
    if let Some(result) = native {
        for block in &mut result.content {
            if let crate::contract::Block::ToolResult { content, .. } = block {
                *content = format!("{preamble}\n\n{content}");
            }
        }
    }
}

/// Whether this cell delivered the pending interrupt: any call of its
/// trajectory that ended as a `Cancelled` throw did.
///
/// **The trajectory rather than the cell's own ending**, because a program
/// may catch the throw (`runtime-contract.md` §9.1's stated limit) and a
/// Ctrl-C the program swallowed was still delivered -- reading the cell's
/// outcome instead would leave the flag raised and cancel the next cell too.
pub(super) fn delivered_the_interrupt(record: &super::CellRecord) -> bool {
    record.calls.iter().any(
        |call| matches!(&call.ended, super::Ended::Threw { class } if class == super::CANCELLED),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn nothing() -> Ending {
        Ending {
            cap: None,
            cap_reached: false,
            stalled_windows: 0,
        }
    }

    fn decide(ending: &Ending) -> Option<ExhaustedReason> {
        exhausted(ending, 3, 6)
    }

    #[test]
    fn a_task_that_is_getting_somewhere_never_ends_here() {
        assert_eq!(decide(&nothing()), None);
        // Short of every threshold, including a cap that exists and is not
        // reached: none of these is a reason on its own.
        let working = Ending {
            cap: Some(40),
            stalled_windows: 2,
            ..nothing()
        };
        assert_eq!(decide(&working), None);
    }

    #[test]
    fn a_ceiling_this_person_set_is_reported_as_theirs_first() {
        // The stall is also true here; the configured one wins, because
        // inferring a reason over the person's own is presumptuous.
        let ended = Ending {
            cap: Some(40),
            cap_reached: true,
            stalled_windows: 9,
        };
        assert_eq!(decide(&ended), Some(ExhaustedReason::CellLimit { cap: 40 }));
    }

    /// The two enders removed on 2026-09-19 stay removed: neither a
    /// supervisor's opinion nor a run of prose turns is a reason here, and
    /// `Ending` no longer carries a field for either to arrive in.
    #[test]
    fn neither_an_opinion_nor_a_count_of_turns_can_end_a_task_any_more() {
        let only_a_stall_ends_it = Ending {
            stalled_windows: 3,
            ..nothing()
        };
        assert!(matches!(
            decide(&only_a_stall_ends_it),
            Some(ExhaustedReason::Stalled { .. })
        ));
        // A task under the stall threshold cannot be ended by anything else
        // this function knows, however long it has been talking.
        assert_eq!(
            decide(&Ending {
                stalled_windows: 2,
                ..nothing()
            }),
            None
        );
    }

    #[test]
    fn a_cap_that_is_not_set_can_never_be_reached() {
        let ended = Ending {
            cap: None,
            cap_reached: true,
            ..nothing()
        };
        assert_eq!(decide(&ended), None);
    }

    #[test]
    fn the_stall_names_its_cells() {
        let stalled = Ending {
            stalled_windows: 3,
            ..nothing()
        };
        assert_eq!(
            decide(&stalled),
            Some(ExhaustedReason::Stalled {
                windows: 3,
                cells: 18,
            })
        );
    }
}
