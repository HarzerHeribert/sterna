//! `helper.<name>(...)` — the roster's pulled half, moved out of
//! `bindings.rs` on 2026-09-18 for the Phase 59 size ratchet. Nothing here is
//! new: it is `helper_callback` and the lane summary it writes, whole, and it
//! sits beside `bindings/decide.rs`, which is the same shape for the
//! decision model.

use super::*;

/// `helper.<name>(text)` for every roster entry — `little-helpers.md`'s
/// pulled half: one metered wire call from inside the running cell, so a
/// question costs no turn.
///
/// The invariant: **a helper either answers or throws.** Unconfigured, over
/// the cell's ceiling, and a call that failed are all a catchable
/// `ToolError`; nothing here can return text that looks like an answer when
/// no answer was made. Shaped like `mcp`, not like `bash`: nothing new runs
/// on the machine, so no grant is consulted.
///
/// Which helper this is comes from the function's own data slot, set by
/// [`install`] from the spec's `name` — the same routing `tool_callback`
/// uses, and the reason one `fn` item serves the whole roster.
pub(super) fn helper_callback(
    scope: &mut v8::PinScope,
    args: v8::FunctionCallbackArguments,
    mut retval: v8::ReturnValue,
) {
    let name = args.data().to_rust_string_lossy(scope);
    let Some(spec) = crate::helpers::lookup(&name) else {
        // Unreachable through `install`, which binds only roster names, and a
        // refusal rather than a panic if it ever is reached.
        throw_tool_error(scope, &format!("no helper named `{name}` is in the roster"));
        return;
    };
    let wanted = format!("helper.{name} takes the text to work on");
    if !args.get(0).is_string() {
        throw_tool_error(scope, &wanted);
        return;
    }
    let input = args.get(0).to_rust_string_lossy(scope);
    if input.trim().is_empty() {
        throw_tool_error(scope, &wanted);
        return;
    }

    let state = state(scope);
    let (model, effort) = match state.helper_route(spec.name) {
        Ok(route) => route,
        Err(reason) => {
            throw_tool_error(scope, &reason);
            return;
        }
    };
    if let Err(reason) = state.claim_helper_call() {
        throw_tool_error(scope, &reason);
        return;
    }

    let asked = asked_summary(&input);
    let slot = state.begin_helper(crate::helpers::HelperRecord {
        helper: spec.name.to_string(),
        verb: spec.verb.to_string(),
        asked: asked.clone(),
        ..crate::helpers::HelperRecord::default()
    });
    // The reader starts from the project's file listing: measured
    // 2026-09-23, a find loop given only the question searched the wrong
    // directories and answered "could not determine" in 2 of 2 calls, while
    // a listing lifted offline recall from 0.62 to 0.88 on the same task.
    let question = input.clone();
    let input = if spec.name == "check" {
        format!(
            "Original checker request:\n{}\n\n{}",
            input,
            crate::runtime::checks::checker_evidence(scope)
        )
    } else {
        input
    };

    let token = state.token.borrow().clone();
    // A helper thinking is the cell waiting (`RuntimeState::away_from_js`);
    // the call outlasts the whole cell limit by design — a one-shot errand
    // ends on silence (`wire::SIDE_ERRAND_SILENCE`) and never on duration —
    // so without this one helper could spend the cell's clock.
    let _away = state.away_from_js();
    let call = if spec.name == crate::helpers::SCOUT.name {
        find(&question, &model, effort, &state, &token)
    } else {
        crate::helpers::run(
            spec,
            crate::helpers::HelperRoute::new(&model, effort),
            &input,
            &state.profile,
            &state.session,
            &token,
        )
    };
    let ok = call.outcome.ok;
    let cancelled = call.outcome.cancelled;
    let answer = call.outcome.text.clone();
    // `turns` is what the call took, not what the spec allowed: a Scout that
    // burned its ceiling to serve two files is a bad call the inspector must
    // show as one.
    state.finish_helper(slot, call);
    // The trajectory says a helper ran and how big the question was, never
    // the payload: §9.4 explains the cell, and a build log is not an
    // explanation.
    trace(scope).record(CallRecord {
        tool: format!("helper.{}", spec.name),
        args: [("asked".to_string(), asked)].into_iter().collect(),
        evidence: None,
        lifted_from: None,
        exit_code: None,
        repeat_of: None,
        error: None,
        ended: if ok {
            Ended::Ok
        } else if cancelled {
            Ended::Threw {
                class: "Cancelled".into(),
            }
        } else {
            Ended::Threw {
                class: "ToolError".into(),
            }
        },
    });
    if cancelled {
        throw_cancelled(scope, &format!("helper.{name}"));
        return;
    }
    if !ok {
        throw_tool_error(scope, &answer);
        return;
    }
    // The reader: a Scout's spans come back with the lines behind them,
    // read from disk here rather than retyped by the helper (`excerpts.rs`).
    // The finder's answer already carries its excerpts (`find` below).
    let value = js_string(scope, &answer);
    retval.set(value);
}

/// The reader: one toolless request over Pane's own evidence
/// (`reader.rs`), its spans served from disk (`excerpts.rs`) -- and the
/// Scout's search loop, given the listing, only when nothing it named
/// verified. Measured 2026-09-23: the loop alone searched the wrong
/// directories twice and took 60 s for the one call that worked.
fn find(
    question: &str,
    model: &str,
    effort: crate::wire::Effort,
    state: &RuntimeState,
    token: &crate::tools::invoke::CancellationToken,
) -> crate::helpers::HelperCall {
    let root = state.profile.root();
    let route = || crate::helpers::HelperRoute::new(model, effort);
    let mut call = crate::helpers::run(
        &crate::reader::FINDER,
        route(),
        &crate::reader::brief(question, root),
        &state.profile,
        &state.session,
        token,
    );
    if call.outcome.ok {
        let served = crate::excerpts::attach(&call.outcome.text, &state.profile);
        if served.contains(crate::excerpts::HEADING) {
            call.outcome.text = served;
            return call;
        }
    }
    let listing = crate::preflight::listing_section(root).unwrap_or_default();
    let mut looked = crate::helpers::run(
        &crate::helpers::SCOUT,
        route(),
        &format!("{question}\n{listing}"),
        &state.profile,
        &state.session,
        token,
    );
    if looked.outcome.ok {
        looked.outcome.text = crate::excerpts::attach(&looked.outcome.text, &state.profile);
    }
    looked
}

/// What the lane and the `/cell` inspector show for one helper call.
///
/// A size, never the payload: the caller still holds the text, the record is
/// persisted to the rollout, and a 4,000-line build log in a lane line is
/// neither readable nor cheap.
pub(crate) fn asked_summary(input: &str) -> String {
    format!("{} lines", thousands(input.lines().count() as u64))
}
