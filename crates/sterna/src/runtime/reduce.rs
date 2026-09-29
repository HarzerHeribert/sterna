//! Reducing an oversized command result, and saying what that cost.
//!
//! `little-helpers.md`'s `CallSite::PostResult` in one place. The ladder, in
//! the order `reduce_oversized` climbs it, each rung cheaper than the one
//! below:
//!
//! 1. under `[helpers] reduce_above_tokens` — nothing is attempted;
//! 2. [`super::reduce_rules`] — predicates that remove passing test lines and
//!    build chatter for free, and often end it here;
//! 3. the digest cache — this task already reduced these exact bytes;
//! 4. the **filter cache** — this task already wrote a filter for output of
//!    this *shape*, which is the key that repeats where a digest does not;
//! 5. the economics test — is removing these tokens worth a cheap model's;
//! 6. one request, and one retry with a wider sample if the filter is
//!    refused;
//! 7. otherwise today's refusal, with the exact output untouched.
//!
//! **The reducer writes a filter; it does not retype its evidence.** It is
//! shown a [sample](super::reduce_sample) — sizes, a histogram of normalised
//! line shapes, head, tail, and the lines it must keep — and answers with a
//! JavaScript function that selects lines. [`super::reduce_run`] runs it in
//! an isolate with nothing bound, [`super::reduce_filter`] refuses any output
//! line that was not in the input, and what reaches the program is therefore
//! evidence rather than recollection. A model asked to reproduce text
//! verbatim by inference can misquote it; a filter cannot.
//!
//! Split out of `bindings.rs` when it crossed the Phase 59 ratchet. The
//! binding itself stays there; what a reduction *is* lives here.

use std::rc::Rc;

use super::bindings::helper::asked_summary;
use super::preview::{estimate_tokens, thousands};
use super::state::RuntimeState;
use crate::tools::invoke::ToolResult;

/// What became of a pushed reduction, for a caller that must tell "not needed"
/// from "attempted and failed".
///
/// The distinction is the whole point (user ruling, 2026-09-10): a reducer is
/// never handed a truncated log, so an output too large for the helper model
/// fails the request — and a parent that cannot see that failure cannot do the
/// one thing that fixes it, which is narrow the command and run it again. An
/// absent key used to mean both "small enough to read yourself" and "we tried
/// and could not", which are opposite instructions.
pub(super) enum Reduction {
    /// Under the cap, or no helper configured: nothing was attempted.
    NotAttempted,
    Made(String),
    /// Attempted and failed, with the one sentence the parent needs.
    Failed(String),
}

/// The share of its budget a *selection* may fill before it is reported as
/// possibly partial.
///
/// A hard truncation is already a failure, never an answer: the provider
/// sets `stop_reason: "max_tokens"`, [`crate::wire`] turns that into
/// `IncompleteResponse`, the helper's `ok` is false and the result carries
/// `reduction_error` instead of `reduced`. This is the *soft* case — a
/// reduction that reads exactly like a complete one and is not.
///
/// **It measures the selection, not the model's answer, and that is the
/// change a filter forces.** When the reducer retyped its evidence, its
/// output tokens *were* the reduction, so a spend near the allowance meant a
/// summary that had run out of room. A filter is a few lines whatever the
/// log holds, so spend now measures how long the filter was — which says
/// nothing at all about how much evidence it had to leave behind. What still
/// means what it used to is the size of what the filter selected against the
/// budget it had to select within.
const CROWDED: f64 = 0.9;

/// Whether this reduction should be read as partial, and why.
///
/// Two independent reasons, both computed here rather than guessed:
///
/// - **The selection filled its budget.** `reduce_filter::validate` refuses a
///   selection over `cap`, so one landing just under it is one that had to
///   stop choosing — there may have been more worth keeping.
/// - **The model was shown a bounded must-keep list.** `must_keep` caps each
///   failure shape at a few examples, so a filter cannot be required to keep
///   lines nobody showed it. A reader deciding whether three failures means
///   three needs that number, and it is exact.
fn partial_note(
    output: &str,
    cap: u32,
    must: &crate::runtime::reduce_sample::MustKeep,
) -> Option<String> {
    let selected = estimate_tokens(output);
    let mut reasons: Vec<String> = Vec::new();
    if f64::from(u32::try_from(selected).unwrap_or(u32::MAX)) >= f64::from(cap) * CROWDED {
        reasons.push(format!(
            "the selection filled {} of its {}-token budget, so it had to stop choosing",
            thousands(selected as u64),
            thousands(u64::from(cap)),
        ));
    }
    if must.elided > 0 {
        reasons.push(format!(
            "{} further marked lines wore {}, and the reducer was not required to keep them",
            thousands(must.elided as u64),
            if must.shapes == 1 {
                "the one failure shape it was shown".to_string()
            } else {
                format!("one of the {} failure shapes it was shown", must.shapes)
            },
        ));
    }
    (!reasons.is_empty()).then(|| {
        format!(
            "{} — read this as partial and check `stdout` before relying on it",
            reasons.join("; ")
        )
    })
}

/// The one line a reduction carries about itself.
///
/// **A summary that looks complete is never re-checked.** The argument that
/// made a reduction safe -- `stdout` and `stderr` stay whole, so the parent
/// can always read the original -- only holds if the parent knows it has
/// reason to, and a reduction with no provenance gives it none: three
/// failures summarised out of two hundred read the same as three out of
/// three.
///
/// One line, because this text is spent on every reduced result. It leads
/// rather than trails so a reader frames the content before trusting it, and
/// it opens with a marker no build log produces, so a reduction *of* a log
/// that discusses reductions cannot be mistaken for it.
fn lossiness_line(input: &str, output: &str, note: Option<&str>) -> String {
    let lines = |text: &str| {
        let n = text.lines().count();
        format!(
            "{} line{}",
            thousands(n as u64),
            if n == 1 { "" } else { "s" }
        )
    };
    let bytes = |text: &str| thousands(text.len() as u64);
    let note = note.map(|note| format!(" · {note}")).unwrap_or_default();
    format!(
        "[sterna:reduction {} / {} bytes → {} / {} bytes{note} · \
         `stdout` and `stderr` on this result are complete and unchanged]",
        lines(input),
        bytes(input),
        lines(output),
        bytes(output),
    )
}

/// [`lossiness_line`] joined to the reduction it describes.
fn with_lossiness(input: &str, output: &str, note: Option<&str>) -> String {
    format!("{}\n{output}", lossiness_line(input, output, note))
}

/// The three parts of a filter's reduction, composed into the one value the
/// program reads as `reduced`.
///
/// **Order and marking are the whole design here.** The provenance line
/// leads, because a reader frames what follows by it. Then the selected
/// lines, which are evidence: every one of them occurred in the output, and
/// `reduce_filter::validate` is what makes that true. Then the model's prose,
/// last and behind a marker that says whose words it is — it is the half a
/// filter cannot produce and the half that can be wrong, and a reader who
/// cannot tell it from the evidence has the worse of both.
fn with_prose(reduction: String, prose: &str) -> String {
    if prose.is_empty() {
        return reduction;
    }
    format!("{reduction}\n[sterna:reduction notes, the reducer's own words]\n{prose}")
}

/// Run a filter over `text` and accept it only if it selected.
///
/// The two failures are kept apart because they are shown to the model
/// differently: one is about the filter as a program, the other about what it
/// chose.
fn apply_filter(
    filter: &str,
    text: &str,
    must: &[String],
    threshold: usize,
) -> Result<String, String> {
    let output =
        crate::runtime::reduce_run::run_filter(filter, text).map_err(|error| error.sentence())?;
    crate::runtime::reduce_filter::validate(text, &output, must, threshold)
        .map_err(|rejected| rejected.sentence())?;
    Ok(output)
}

/// `little-helpers.md`'s `CallSite::PostResult`: a command result the model
/// would otherwise have to page is reduced by REDUCER, without the model
/// spending a turn to ask.
///
/// **Cheap-model tokens are spent only to remove parent tokens, never to add
/// a second view of a small output.** The trigger is `[helpers]
/// reduce_above_tokens`, and above it a request is made only when the
/// expected saving is positive: the output's estimated tokens minus the
/// reducer's own `max_tokens` must exceed half the threshold. Below either
/// line the parent reads the output itself.
///
/// Only [`typed_result`]'s command-output arm reaches here, so `read`,
/// `context`, `edit`, `grep` and `glob` — every shape a model edits or quotes
/// from — are excluded structurally. The exact `stdout` and `stderr` stay
/// complete on the result; the reduction is one more property beside them,
/// and a helper failing here is never fatal.
pub(super) fn reduce_oversized(result: &ToolResult, state: &Rc<RuntimeState>) -> Reduction {
    let threshold = state.reduce_above_tokens();
    let tokens = estimate_tokens(&result.stdout) + estimate_tokens(&result.stderr);
    if tokens <= threshold {
        return Reduction::NotAttempted;
    }
    // Both streams, because a build writes its failures to whichever it
    // likes and the reduction is of the output, not of one pipe.
    let mut text = result.stdout.clone();
    text.push_str(&result.stderr);
    reduce_text(text, state)
}

/// The same ladder for a returned field the decision model read as a log
/// (`session/returned.rs`): the size gate above is the model's answer, so
/// none is applied here, and the rules rung, the caches and the economics
/// test stand exactly as they do for a command result.
pub(super) fn reduce_asked(text: String, state: &Rc<RuntimeState>) -> Reduction {
    reduce_text(text, state)
}

/// The ladder from the rules rung down, over text already judged worth it.
fn reduce_text(text: String, state: &Rc<RuntimeState>) -> Reduction {
    let threshold = state.reduce_above_tokens();

    // **The deterministic rung, before a model is considered at all.** Most
    // of what trips the threshold -- a thousand passing test lines, a page of
    // `Compiling`, a warning repeated forty times -- is removable by a
    // predicate, and a request is worth making for what is left rather than
    // for what a rule could have deleted for free. An input no rule
    // recognises comes back byte-identical, so the path below is unchanged
    // where this does not help.
    let original = text;
    let ruled = crate::runtime::reduce_rules::apply(&original);
    let text = ruled.text;
    let tokens = estimate_tokens(&text);

    // **With no helper model the rules are all there is, and they are
    // free.** This rung sat behind the helper route until 2026-09-29, so a
    // session with helpers off read every `test … ok` line of a failing run
    // that the rules would have folded into its `test result:` count.
    let route = crate::helpers::HELPERS
        .iter()
        .find(|spec| {
            spec.call_sites
                .contains(&crate::helpers::CallSite::PostResult)
        })
        .and_then(|spec| {
            state
                .helper_route(spec.name)
                .ok()
                .map(|route| (spec, route))
        });
    let Some((spec, (model, effort))) = route else {
        if ruled.applied.is_empty() {
            return Reduction::NotAttempted;
        }
        state.count_reduction(|stats| {
            stats.ruled += 1;
            stats.bytes_out += text.len() as u64;
        });
        let note = format!("rules only, no helper model: {}", ruled.applied.join(", "));
        return Reduction::Made(with_lossiness(&original, &text, Some(&note)));
    };

    if tokens <= threshold {
        state.count_reduction(|stats| {
            stats.ruled += 1;
            stats.bytes_out += text.len() as u64;
        });
        let note = format!("rules only, no model: {}", ruled.applied.join(", "));
        return Reduction::Made(with_lossiness(&original, &text, Some(&note)));
    }

    // The allowance the wire will actually carry, not the one the spec
    // declares. They were seventeen times apart: `REDUCER` names 1,024 and
    // `configure_effort` sent 16,384 of thinking beside it, so this test --
    // written to stop a reduction costing more than it saves -- was computed
    // against a number that made every reduction look free.
    //
    // The cap scales with the work (`helpers::reduction_cap`): a dense
    // failure log needs about a quarter of its own size to name every
    // distinct failure, and a constant 1,024 turned that into a refusal the
    // caller had already paid a request for.
    //
    // **It now bounds the filter's *selection* rather than the model's
    // prose**, which is the same budget spent through a different mechanism:
    // `reduce_filter::validate` refuses a selection over it. Keeping the same
    // number is what makes the test below the one it has always been, so no
    // output that is reduced today stops being reduced.
    let cap = crate::helpers::reduction_cap(tokens);
    let allowance_tokens =
        crate::wire::wire_max_tokens(&model, crate::wire::Allowance::Capped(cap), effort);
    let allowance = allowance_tokens as usize;

    use sha2::{Digest, Sha256};
    let digest = format!("{:x}", Sha256::digest(text.as_bytes()));
    if let Some(reduction) = state.reduction_of(&digest) {
        state.count_reduction(|stats| {
            stats.cached += 1;
            stats.bytes_out += reduction.len() as u64;
        });
        return Reduction::Made(reduction);
    }

    // Computed once and used by every rung below, and after the digest cache
    // because normalising a long output is real work an exact hit need not
    // pay for: the lines the caller's own markers say carry a failure,
    // bounded by shape rather than truncated (`reduce_sample::must_keep` says
    // why), and the shape signature this task's filters are keyed on.
    let marked: Vec<String> = text
        .lines()
        .filter(|line| crate::runtime::reduce_rules::never_drop(line))
        .map(str::to_string)
        .collect();
    let must = crate::runtime::reduce_sample::must_keep(&marked);
    let signature = crate::runtime::reduce_sample::shapes_of(&text).signature();

    // **A filter this task already wrote for output of this shape.** The
    // digest cache above needs the same bytes twice; this needs only the same
    // tool. It is tried before the economics test and before any slot is
    // claimed, because it spends no request at all -- one filter run over the
    // new text, and the provenance recomputed from *that* text, never served
    // with the filter. A cached line saying "4,000 lines → 1" about a
    // different log is the exact silent wrong answer this package exists to
    // prevent.
    if let Some(filter) = state.filter_of(&signature)
        && let Ok(output) = apply_filter(&filter, &text, &must.lines, cap as usize)
    {
        // The same partiality facts as a fresh selection: they are about
        // *this* output and its own must-keep list, so they are recomputed
        // here exactly as the provenance is.
        let reused = "selected by a filter written earlier in this task; no model was asked";
        let note = match partial_note(&output, cap, &must) {
            Some(partial) => format!("{reused} · {partial}"),
            None => reused.to_string(),
        };
        let answer = with_lossiness(&text, &output, Some(&note));
        state.count_reduction(|stats| {
            stats.filter_reused += 1;
            stats.bytes_out += answer.len() as u64;
        });
        return Reduction::Made(answer);
    }

    if tokens.saturating_sub(allowance) <= threshold / 2 {
        return Reduction::NotAttempted;
    }

    // **One retry, and it widens the evidence rather than the capability.**
    // A filter is rejected for a reason the model can act on, so the second
    // attempt carries that sentence and a wider sample -- more head, more
    // tail, more shapes. It is never given tools: that would move the
    // reducer onto the agent loop, where a global deadline and an unreachable
    // `max_tokens` are still unfixed.
    let mut width = crate::runtime::reduce_sample::Width::Ordinary;
    let mut rejected: Option<String> = None;
    let mut asked = false;
    for _ in 0..2 {
        // Before each call, because a claimed slot is always a request made
        // and a served answer claims none. Reaching the cell's ceiling is
        // **not** a failed reduction: nothing was asked, so the parent is
        // told nothing was attempted rather than being handed a refusal
        // about an output that was never looked at.
        if state.claim_pushed_helper_call().is_err() {
            break;
        }
        asked = true;
        let sample = crate::runtime::reduce_sample::sample(&text, &must, width);
        let input = match &rejected {
            None => sample,
            Some(why) => format!(
                "{sample}\n\n## Your previous filter was not accepted\n{why}\n\n\
                 Send one corrected ```sterna-filter fence.",
            ),
        };
        state.count_reduction(|stats| {
            stats.attempted += 1;
            stats.bytes_in += input.len() as u64;
        });

        let slot = state.begin_helper(crate::helpers::HelperRecord {
            helper: spec.name.to_string(),
            verb: spec.verb.to_string(),
            asked: asked_summary(&text),
            ..crate::helpers::HelperRecord::default()
        });
        let token = state.token.borrow().clone();
        // A helper thinking is the cell waiting (`RuntimeState::away_from_js`);
        // the call outlasts the whole cell limit by design — a one-shot errand
        // ends on silence (`wire::SIDE_ERRAND_SILENCE`) and never on duration —
        // so without this one helper could spend the cell's clock.
        let _away = state.away_from_js();
        let call = crate::helpers::run(
            spec,
            crate::helpers::HelperRoute {
                model: &model,
                effort,
                cap: Some(cap),
            },
            &input,
            &state.profile,
            &state.session,
            &token,
        );
        let ok = call.outcome.ok;
        let cancelled = call.outcome.cancelled;
        let answered = call.outcome.text.clone();
        state.finish_helper(slot, call);
        if cancelled {
            return Reduction::NotAttempted;
        }
        if !ok {
            rejected = None;
            break;
        }

        match crate::runtime::reduce_filter::parse(&answered).and_then(|answer| {
            apply_filter(&answer.filter, &text, &must.lines, cap as usize)
                .map(|output| (answer, output))
        }) {
            Ok((answer, output)) => {
                let note = partial_note(&output, cap, &must);
                let reduction = with_prose(
                    with_lossiness(&text, &output, note.as_deref()),
                    &answer.prose,
                );
                // Counted on the answer that reaches the program, not on the
                // model's bare text: the served copy counts the same value,
                // and two spellings of `bytes_out` would make the pair
                // disagree about one cache.
                state.count_reduction(|stats| {
                    stats.made += 1;
                    stats.filtered += 1;
                    stats.bytes_out += reduction.len() as u64;
                });
                state.remember_reduction(digest, reduction.clone());
                state.remember_filter(signature, answer.filter);
                return Reduction::Made(reduction);
            }
            Err(why) => {
                rejected = Some(why);
                width = crate::runtime::reduce_sample::Width::Wide;
            }
        }
    }

    if !asked {
        return Reduction::NotAttempted;
    }
    if rejected.is_some() {
        state.count_reduction(|stats| stats.filter_rejected += 1);
    }
    state.count_reduction(|stats| stats.failed += 1);
    // The exact output is untouched and still on the result; what the parent
    // is being told is that it will not get a summary of it unless it narrows
    // what the command prints.
    let why = rejected.unwrap_or_else(|| "the reducer did not answer".to_string());
    Reduction::Failed(format!(
        "This output was too large to summarise and no summary was made \
         ({why}). `stdout` and `stderr` are complete and unchanged. \
         Narrow what the command prints, or select the part you need, \
         before relying on a summary."
    ))
}
