//! What the context meter knows: how much of the window a task has used, how
//! much the window is, and how much the screen may claim about that figure.
//!
//! Moved out of `session.rs` whole on 2026-09-17 when the window's provenance
//! arrived and pushed that file past the size ratchet -- the user's rule for
//! that wall is that it is a signal to split rather than to shorten. Nothing
//! here is new: these are the three functions that were already the meter's,
//! and they are the meter's together.
//!
//! The one invariant worth stating in one place: **a figure and where it came
//! from travel together.** A cap kept without its source would let the status
//! line draw a percentage against a number nobody measured, which is the one
//! claim `crate::models::WindowSource` exists to prevent.

use super::*;

pub(super) fn record_request(notebook: &mut Notebook, measurement: RequestMeasurement) {
    output::parent_response(&measurement);
    if let Some(used) = measurement.context_tokens() {
        let cap = notebook.context.and_then(|context| context.cap);
        // The cap and where it came from travel together: a figure kept
        // without its provenance would let the meter claim a percentage it
        // has not earned.
        let cap_source = notebook
            .context
            .map(|context| context.cap_source)
            .unwrap_or_default();
        notebook.context = Some(ContextTokens {
            used,
            cap,
            cap_source,
            counted: Counted::Gateway,
        });
    }
    if notebook.requests.len() >= REQUEST_MEASUREMENT_CAP {
        let remove = notebook.requests.len() + 1 - REQUEST_MEASUREMENT_CAP;
        notebook.requests.drain(..remove);
    }
    notebook.requests.push(measurement);
}

pub(super) fn estimate_context(
    notebook: &mut Notebook,
    estimate: u64,
    cap: Option<u64>,
    cap_source: crate::models::WindowSource,
) {
    notebook.context = Some(ContextTokens {
        used: estimate,
        cap,
        cap_source,
        counted: Counted::Estimated,
    });
}

pub(super) fn context_cap(
    session: &Session<'_>,
    model: &str,
) -> (Option<u64>, crate::models::WindowSource) {
    let configured = session
        .context_window
        .as_ref()
        .and_then(|(configured_model, cap)| (configured_model == model).then_some(*cap));
    // `--context-window-tokens` first, then a window this route was watched
    // enforcing, then whatever a catalogue published for the model; an
    // unknown window stays unknown and the meter says so. The source travels
    // with the figure because the meter may only draw a percentage against a
    // measured one.
    crate::models::window_source_for(model, configured)
}

/// How much of the window a returned value may fill on the next cell:
/// [`prompt::return_budget`] over what the meter knows now. An estimated
/// figure counts -- it is the same one the meter draws -- and an unknown
/// window gives the unknown budget.
pub(super) fn return_budget(notebook: &Notebook, model: &str) -> u64 {
    let (used, cap) = notebook
        .context
        .map(|context| (Some(context.used), context.cap))
        .unwrap_or((None, None));
    prompt::return_budget(used, cap, u64::from(wire::max_tokens_for(model)))
}

// --- the deliberate sweep -----------------------------------------------

/// How many messages the conversation must gain before a second sweep is
/// considered.
///
/// A sweep is free in tokens — [`prompt::compact_conversation`] is local
/// string work and asks nobody anything — but it rewrites messages the
/// provider has already cached, so the next request pays to re-read the
/// whole prefix once. Sweeping every turn would pay that price every turn
/// and remove a few bytes each time, which is the trickle the ruling is
/// against: *"rewriting cached tokens is dumb and if so should be done in
/// one deliberate sweep."*
pub(super) const SWEEP_REGROWTH_MESSAGES: usize = 12;

/// Whether the work in flight can spare its older results.
///
/// **The judgement is Jev's, and this is the shape of the question.** The
/// ruling: *"This should never happen during an agent's implementation where
/// context is very relevant. Jev could eval this. To compact at the right
/// times."* A sweep drops the superseded sections of older cell results —
/// their handle tables, plans and usage figures — which is exactly the
/// material a model re-reads while it is repairing something.
///
/// The decision model is not asked here yet: `decide.rs` belongs to another
/// package as this lands, and the question it will ask is written out in
/// this package's report with this call site named. Until it answers, the
/// deterministic reading below stands in, and it is the same reading the
/// question will be given as state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Moment {
    /// The last cell finished cleanly: nothing is half-repaired.
    Settled,
    /// The last cell threw, so the model is mid-repair and the results it is
    /// working from are the ones a sweep would shorten.
    InFlight,
}

/// What a sweep did, or why it did not happen. Observed, never counted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Sweep {
    /// Under the fraction, or the window is unknown.
    NotYet,
    /// Over the fraction, but this is not a moment to take it.
    Held,
    /// Taken, and this is what it removed.
    Taken(prompt::Compaction),
}

/// Whether a proactive sweep is due, and if so, taking it.
///
/// **An unknown window never sweeps proactively.** Without a figure there is
/// no fraction to be over, and guessing one would compact a conversation that
/// had room — so the reactive recovery in `send_task_turn_recovering` stays
/// the floor, exactly as it was.
pub(super) fn consider_sweep(
    conversation: &mut Conversation,
    used: u64,
    cap: Option<u64>,
    cap_source: crate::models::WindowSource,
    percent: u64,
    moment: Moment,
    swept_at_messages: &mut Option<usize>,
) -> Sweep {
    // Known, not trusted: `Published` is an estimate the meter may not draw a
    // percentage against, but it is still a figure, and compacting a little
    // early costs less than discovering the window by being refused.
    let known = !matches!(cap_source, crate::models::WindowSource::Unknown);
    let Some(cap) = cap.filter(|_| known) else {
        return Sweep::NotYet;
    };
    if used.saturating_mul(100) < cap.saturating_mul(percent) {
        return Sweep::NotYet;
    }
    if let Some(last) = *swept_at_messages
        && conversation.messages.len() < last.saturating_add(SWEEP_REGROWTH_MESSAGES)
    {
        return Sweep::NotYet;
    }
    if moment == Moment::InFlight {
        return Sweep::Held;
    }
    let report = prompt::compact_conversation(conversation);
    if report.is_empty() {
        // Nothing was removed, so nothing the provider cached changed: this
        // costs the session nothing and is not worth announcing.
        return Sweep::NotYet;
    }
    *swept_at_messages = Some(conversation.messages.len());
    Sweep::Taken(report)
}

/// The one line the person gets, which is the only way they can judge it.
pub(super) fn sweep_notice(report: prompt::Compaction, used: u64, cap: u64) -> String {
    format!(
        "context: {} older result(s) shortened by {} KiB at {}% of the window; \
         the turns after this one carry less",
        report.messages,
        report.bytes / 1024,
        used.saturating_mul(100) / cap.max(1),
    )
}

// --- the reactive rung --------------------------------------------------
//
// The proactive sweep above and the recovery below are the two answers a
// session has to a window that ran out, so they live together: sweep near the
// top of a window it knows, and if a request is refused as too long anyway --
// which is the only thing a session with an unknown window can do -- replace
// the conversation with a checkpoint once and carry on. Moved here whole from
// `session.rs` when the sweep pushed that file past its size ratchet; the
// user's rule for that wall is that it is a signal to split.

/// Normal requests already project superseded state out of history. If that
/// request still overflows, checkpoint once while keeping the runtime alive.
/// Do not mutate old feedback or retry an identical projected request. Other
/// errors propagate without compaction or retry.
pub(super) fn send_task_turn_recovering(
    transcript: &mut Transcript,
    session: &Session<'_>,
    runtime: &Runtime,
    task: &str,
    rollout: &mut Rollout,
    cause: crate::abi::telemetry::RequestCause,
) -> Result<(wire::Turn, u64), String> {
    let provider_view = |transcript: &Transcript| {
        if let Some(checkpoint) = &transcript.provider_checkpoint {
            let mut checkpoint_message = Message::text(Role::User, checkpoint);
            if let Some((index, message)) = transcript.conversation.messages.iter().enumerate().rev().find(|(_, message)| {
                message.role == Role::User && matches!(message.content.first(), Some(Block::Text(text)) if text == task)
            }) && index < transcript.provider_start {
                checkpoint_message.content.extend(message.content.iter().filter(|block| matches!(block, Block::Image { .. })).cloned());
            }
            let mut messages = vec![checkpoint_message];
            messages
                .extend_from_slice(&transcript.conversation.messages[transcript.provider_start..]);
            let mut view = Conversation {
                system: transcript.conversation.system.clone(),
                messages,
            };
            prompt::drop_reasoning(&mut view);
            view
        } else {
            transcript.conversation.clone()
        }
    };
    let first_request = provider_view(transcript);
    let first = match timed_send_task_turn(&first_request, session, task, cause) {
        Ok(sent) => {
            // A turn that hit the provider's output ceiling still carries
            // whatever the model finished before it, and saying so is the
            // difference between a short reply and a cut one.
            if sent.0.truncated {
                session_println!(
                    "the model reached its output limit for this turn; what it had finished was \
                     kept and the rest of what it was writing did not arrive"
                );
            }
            return Ok(sent);
        }
        Err(error) if error.is_context_overflow() => error,
        Err(error) => return Err(format!("request failed: {error}")),
    };

    let checkpoint = prompt::checkpoint(
        task,
        &runtime.plan(),
        &runtime.handle_names(),
        Some(&first.to_string()),
    );
    transcript.provider_start = transcript.conversation.messages.len();
    transcript.provider_checkpoint = Some(checkpoint.clone());
    {
        let _line = session.interrupt.writing();
        rollout
            .record_checkpoint(&checkpoint)
            .map_err(|e| format!("could not record the checkpoint: {e}"))?;
    }
    session_println!(
        "context: still did not fit, so provider context was replaced by a checkpoint; {} handle(s) \
         are still live, visible history was preserved, and nothing was re-run",
        runtime.handle_names().len()
    );
    let retry = provider_view(transcript);
    timed_send_task_turn(&retry, session, task, cause)
        .map_err(|error| format!("request failed after a checkpoint: {error}"))
}

/// The whole sweep decision as one call, so the turn loop carries the intent
/// and not the mechanism: whether it is due, whether this is the moment, and
/// the one line the person gets when it is taken.
pub(super) fn sweep_if_due(
    conversation: &mut Conversation,
    used: u64,
    cap: Option<u64>,
    cap_source: crate::models::WindowSource,
    percent: u64,
    last_cell_threw: bool,
    swept_at_messages: &mut Option<usize>,
) -> Option<String> {
    let moment = if last_cell_threw {
        Moment::InFlight
    } else {
        Moment::Settled
    };
    match consider_sweep(
        conversation,
        used,
        cap,
        cap_source,
        percent,
        moment,
        swept_at_messages,
    ) {
        Sweep::Taken(report) => Some(sweep_notice(report, used, cap.unwrap_or(used.max(1)))),
        Sweep::NotYet | Sweep::Held => None,
    }
}

#[cfg(test)]
mod sweep_tests {
    use super::*;
    use crate::contract::Message;
    use crate::models::WindowSource;

    /// Four cell results, the newest of which is the copy the others are
    /// redundant against.
    fn conversation(cells: usize) -> Conversation {
        let mut messages = vec![Message::text(crate::contract::Role::User, "the task")];
        for cell in 0..cells {
            messages.push(Message::runtime_tool_result(
                format!("cell-{cell}"),
                format!(
                    "[cell {cell} yielded]\n\n## stdout\nwhat cell {cell} printed\n\n## Handles\n\
                     hits n={cell}\n\n## Plan\n[~] the step\n\n## Usage\ncells {cell}"
                ),
                false,
                format!("[cell {cell} yielded]"),
            ));
        }
        Conversation {
            system: "system".into(),
            messages,
        }
    }

    fn sweep(
        used: u64,
        cap: Option<u64>,
        source: WindowSource,
        moment: Moment,
    ) -> (Sweep, Conversation) {
        let mut conversation = conversation(4);
        let mut swept = None;
        let outcome = consider_sweep(&mut conversation, used, cap, source, 85, moment, &mut swept);
        (outcome, conversation)
    }

    #[test]
    fn a_task_with_room_left_is_never_swept() {
        let (outcome, _) = sweep(
            100_000,
            Some(1_000_000),
            WindowSource::Observed,
            Moment::Settled,
        );
        assert_eq!(outcome, Sweep::NotYet);
    }

    /// The mutation this kills: sweeping without asking where the figure came
    /// from. A window nobody knows has no fraction to be over, so the
    /// reactive recovery stays the only compaction — guessing one would
    /// rewrite a cached prefix for a conversation that had room.
    #[test]
    fn an_unknown_window_is_never_swept_proactively() {
        let (outcome, conversation) = sweep(900_000, None, WindowSource::Unknown, Moment::Settled);
        assert_eq!(outcome, Sweep::NotYet);
        assert!(
            conversation.messages[1].content.iter().any(|block| matches!(
                block,
                crate::contract::Block::ToolResult { content, .. } if content.contains("## Handles")
            )),
            "an unknown window must leave the conversation untouched"
        );
        // A figure with no source is the same absence spelled the other way.
        let (outcome, _) = sweep(
            900_000,
            Some(1_000_000),
            WindowSource::Unknown,
            Moment::Settled,
        );
        assert_eq!(outcome, Sweep::NotYet);
    }

    #[test]
    fn a_full_window_is_swept_once_and_not_again_immediately() {
        let mut conversation = conversation(4);
        let mut swept = None;
        let first = consider_sweep(
            &mut conversation,
            900_000,
            Some(1_000_000),
            WindowSource::Observed,
            85,
            Moment::Settled,
            &mut swept,
        );
        assert!(matches!(first, Sweep::Taken(report) if report.messages == 3));
        assert_eq!(swept, Some(conversation.messages.len()));
        let second = consider_sweep(
            &mut conversation,
            900_000,
            Some(1_000_000),
            WindowSource::Observed,
            85,
            Moment::Settled,
            &mut swept,
        );
        assert_eq!(
            second,
            Sweep::NotYet,
            "a second sweep would rebuild the cached prefix to remove nothing"
        );
    }

    /// The ruling's own sentence: *"This should never happen during an
    /// agent's implementation where context is very relevant."*
    #[test]
    fn a_sweep_is_held_while_the_model_is_mid_repair() {
        let (outcome, conversation) = sweep(
            900_000,
            Some(1_000_000),
            WindowSource::Observed,
            Moment::InFlight,
        );
        assert_eq!(outcome, Sweep::Held);
        assert!(
            conversation.messages[1].content.iter().any(|block| matches!(
                block,
                crate::contract::Block::ToolResult { content, .. } if content.contains("## Handles")
            )),
            "a held sweep changes nothing"
        );
    }

    /// The correctness test, and the one that matters most: a sweep that lost
    /// the task would be worse than any token it saved.
    #[test]
    fn what_the_model_still_needs_survives_a_sweep() {
        let (outcome, conversation) = sweep(
            900_000,
            Some(1_000_000),
            WindowSource::Observed,
            Moment::Settled,
        );
        assert!(matches!(outcome, Sweep::Taken(_)));
        let text = |index: usize| -> String {
            conversation.messages[index]
                .content
                .iter()
                .map(crate::contract::Block::text)
                .collect()
        };
        // The person's own words are never edited, however long it gets.
        assert!(text(0).contains("the task"));
        for cell in 0..3 {
            let older = text(cell + 1);
            assert!(older.contains(&format!("[cell {cell} yielded]")), "{older}");
            assert!(
                older.contains(&format!("what cell {cell} printed")),
                "stdout belongs to the cell that produced it: {older}"
            );
            assert!(
                !older.contains("## Handles"),
                "the newest result restates the table in full: {older}"
            );
        }
        // The newest result is the copy the others are redundant against, so
        // it keeps everything.
        let newest = text(4);
        assert!(newest.contains("## Handles") && newest.contains("## Plan"));
    }
}
