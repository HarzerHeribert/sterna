//! The shape of a large returned field, read by the decision model before
//! the value is rendered, and the reducer for the one shape where less is
//! more.
//!
//! The user's reading (2026-09-23): *sometimes reduction is enrichment, by
//! not flooding context with what nobody needs -- maybe Jev can decide
//! that.* So it does. For every field at or over `[limits]
//! reduce_above_tokens`, Jev is asked what kind of text it is (`decide::
//! field_shape`, 2 s, one question). A `log` goes to the reduction rules, and
//! what comes back carries a lossiness line saying what was removed; every
//! other shape is paged as it stands. No answer, no rule that applied or
//! `shadow` mode leaves the field exactly as `render_within` would have shown
//! it, so Sterna without Jev is dumber here and never broken.

use super::*;
use crate::runtime::outcome::{FieldBody, ReturnedField, Terminal};

/// A returned value shown to the model and the screen: its large fields'
/// shapes asked for and a log reduced ([`shape`]), then rendered within this
/// turn's return budget (`TaskSpend::render_return`), with the usage line
/// carrying the figures. The screen shows the same text the model reads.
pub(super) fn show(
    session: &Session<'_>,
    runtime: &Runtime,
    task_state: &mut TaskState,
    budget: &mut TaskSpend,
    terminal: &Terminal,
    result: &mut CellResult,
) -> String {
    let shaped = shape(session, runtime, task_state, terminal);
    let text = budget.render_return(&shaped);
    result.budget.feedback = Some(budget.return_usage());
    result.output = Some(text.clone());
    text
}

/// The confidence at or above which a `log` answer sends the field to the
/// reducer. Below it the field is paged: a reduction is lossy on purpose,
/// and an uncertain reading is not a reason to lose anything.
const REDUCE_ABOVE: f64 = 0.6;

/// The terminal as the model will read it: each large field's shape asked
/// for, and a log reduced when the answer and the mode allow it.
pub(super) fn shape(
    session: &Session<'_>,
    runtime: &Runtime,
    task_state: &mut TaskState,
    terminal: &Terminal,
) -> Terminal {
    let config = session.config();
    let Terminal::Fields(fields) = terminal else {
        return terminal.clone();
    };
    if !config.decisions.reduce_returns {
        return terminal.clone();
    }
    let Some(model) = config.decisions.model.clone() else {
        return terminal.clone();
    };
    if config.decisions.mode == crate::config::DecisionMode::Off {
        return terminal.clone();
    }
    let threshold = config.limits.reduce_above_tokens;
    // Acts in `shadow` too since 2026-09-23: a log shortened with the whole
    // value still bound stops nothing, so it is advice-shaped, not a gate.
    // Every large field's question at once: each waits up to Jev's timeout,
    // and asked in turn six fields held the turn for twelve seconds
    // (2026-09-23) -- no Escape reaches a call like that.
    let answers: Vec<Option<Result<crate::decide::FieldShape, crate::decide::DecideError>>> =
        std::thread::scope(|scope| {
            let asked: Vec<_> = fields
                .iter()
                .map(|field| {
                    let text = field.text();
                    let large = crate::runtime::preview::estimate_tokens(&text) >= threshold;
                    let model = model.as_str();
                    let name = field.name.as_str();
                    large.then(|| {
                        scope.spawn(move || crate::decide::field_shape(model, name, &text))
                    })
                })
                .collect();
            asked
                .into_iter()
                .map(|handle| handle.map(|h| h.join().expect("a shape question does not panic")))
                .collect()
        });
    let shaped = fields
        .iter()
        .zip(answers)
        .map(|(field, answer)| match answer {
            None => field.clone(),
            Some(answer) => shape_field(answer, runtime, task_state, field),
        })
        .collect();
    Terminal::Fields(shaped)
}

fn shape_field(
    answer: Result<crate::decide::FieldShape, crate::decide::DecideError>,
    runtime: &Runtime,
    task_state: &mut TaskState,
    field: &ReturnedField,
) -> ReturnedField {
    let text = field.text();
    let answer = match answer {
        Ok(answer) => answer,
        Err(error) => {
            task_state.field_shapes.push(serde_json::json!({
                "field": field.name,
                "failed": error.to_string(),
            }));
            return field.clone();
        }
    };
    let wants_reduction =
        answer.choice == crate::decide::FIELD_LOG && answer.confidence >= REDUCE_ABOVE;
    let reduced = wants_reduction
        .then(|| runtime.reduce_returned(&text))
        .flatten();
    task_state.field_shapes.push(serde_json::json!({
        "field": field.name,
        "choice": answer.choice,
        "confidence": answer.confidence,
        "latency_ms": answer.latency_ms,
        "reduced": reduced.is_some(),
    }));
    match reduced {
        Some(reduced) => ReturnedField {
            name: field.name.clone(),
            body: FieldBody::Text(format!(
                "{reduced}\n[the whole value is still live in your bindings; return a slice of it to see more]"
            )),
            whole: field.whole,
        },
        None => field.clone(),
    }
}
