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
    let terminal = without_printed_contexts(terminal, result.stdout_tail.as_deref());
    let shaped = shape(session, runtime, task_state, &terminal);
    let text = budget.render_return(&shaped);
    result.budget.feedback = Some(budget.return_usage());
    result.output = Some(text.clone());
    text
}

/// The terminal with every returned copy of a source context this result
/// already prints replaced by one line saying where it is.
///
/// `context` prints its result into the cell's output by itself, so a
/// program that also returned `ctx.text` showed the model the same
/// definitions twice: 1.4–3.3K tokens an attempt over 30 SWE-bench tasks
/// (2026-10-01), the largest single repeat in what a context costs. A
/// printed context that shed excerpts to fit the turn is not the same text,
/// so a return of it stands, and so does any text that is not one context.
fn without_printed_contexts(terminal: &Terminal, stdout: Option<&str>) -> Terminal {
    let Some(stdout) = stdout else {
        return terminal.clone();
    };
    match terminal {
        Terminal::Text(text) => {
            Terminal::Text(pointer_for(text, stdout).unwrap_or_else(|| text.clone()))
        }
        Terminal::Fields(fields) => Terminal::Fields(
            fields
                .iter()
                .map(|field| match &field.body {
                    FieldBody::Text(text) => match pointer_for(text, stdout) {
                        Some(pointer) => ReturnedField {
                            body: FieldBody::Text(pointer),
                            ..field.clone()
                        },
                        None => field.clone(),
                    },
                    FieldBody::Lines(_) | FieldBody::Json(_) => field.clone(),
                })
                .collect(),
        ),
        Terminal::Json { .. } => terminal.clone(),
    }
}

/// The line that stands in for `text` when it is one source context whose
/// header -- path, language, symbol, version -- `stdout` prints whole.
fn pointer_for(text: &str, stdout: &str) -> Option<String> {
    const HEAD: &str = "## Source context\n";
    if !text.starts_with(HEAD) || text.matches(HEAD).count() != 1 {
        return None;
    }
    let header: String = text.split_inclusive('\n').take(5).collect();
    let field = |name: &str| {
        header
            .lines()
            .find_map(|line| line.strip_prefix(name))
            .unwrap_or_default()
            .to_string()
    };
    let at = stdout.find(&header)?;
    let printed = &stdout[at + header.len()..];
    let printed = &printed[..printed.find(HEAD).unwrap_or(printed.len())];
    if printed.contains("to fit this turn's feedback budget") {
        return None;
    }
    Some(format!(
        "(the source context for `{}`, symbol {}, version {}: printed in this result's output, not repeated here)",
        field("path: "),
        field("symbol: "),
        field("version: ")
    ))
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

#[cfg(test)]
mod tests {
    use super::pointer_for;

    const CONTEXT: &str = "## Source context\npath: src/a.py\nlanguage: python\nsymbol: f\nversion: 0123456789ab\ncomplete: true\n\n### TargetDefinition: src/a.py:1-2 [complete]\n    1 | def f():\n    2 |     return 1\n";

    /// A returned copy of a context this result prints is one line, and it
    /// names what it stands for.
    #[test]
    fn a_returned_context_the_output_prints_is_a_pointer() {
        let stdout = format!("before\n{CONTEXT}after\n");
        let pointer = pointer_for(CONTEXT, &stdout).expect("printed, so pointed at");
        assert!(
            pointer.contains("`src/a.py`") && pointer.contains("0123456789ab"),
            "{pointer}"
        );
        assert!(!pointer.contains("return 1"), "{pointer}");
    }

    /// What the output does not print the same way stands: a context it
    /// printed short to fit the turn, one it never printed, and plain text.
    #[test]
    fn anything_but_a_context_printed_whole_stands() {
        let shed = CONTEXT.replace(
            "    2 |     return 1\n",
            "omission: 1 lower-ranked supporting excerpt(s) omitted to fit this turn's feedback budget\n",
        );
        assert_eq!(pointer_for(CONTEXT, &shed), None);
        assert_eq!(pointer_for(CONTEXT, "nothing printed"), None);
        assert_eq!(pointer_for("return value", CONTEXT), None);
    }
}
