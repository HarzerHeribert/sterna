//! The `decide` global: the decision model, asked by the running program.
//!
//! **Shaped like `helper`, not like `bash`.** Nothing new runs on the machine,
//! so no grant is consulted; the call is one metered request inside the cell
//! that asked it, so it costs no turn; and it either answers or throws a
//! catchable `ToolError`, so a failed question can never return something
//! that reads like a judgement.
//!
//! What it adds over the harness's own gates (`decide.rs`'s intent,
//! completion, hygiene, drift and supervision questions) is that the *program*
//! composes the question. That is the whole point: a cell that has just
//! computed something — a diff, a command line, a build log — can ask for a
//! judgement about it and branch on the answer without spending a turn asking
//! the task model.

use super::*;
use std::collections::BTreeMap;

/// Binds the `decide` global on the current context. Called once, by
/// `Runtime::with_decisions`, and only when `[decisions] model` names a model
/// and the narrowing admits `decide`: a session that configured none never
/// holds the name, and the Runtime block never declares it.
pub(crate) fn install_decide(scope: &mut v8::PinScope) {
    let context = scope.get_current_context();
    let global = context.global(scope);
    let decide = v8::Object::new(scope);
    if let Some(function) = v8::Function::builder(decide_choice_callback).build(scope) {
        set_fixed_key(scope, decide, "choice", function.into());
    }
    set_fixed_key(scope, global, "decide", decide.into());
}

/// `decide.choice(instructions, criteria, subject?)`.
///
/// The per-cell ceiling is `[helpers] calls_per_cell`, claimed through
/// [`RuntimeState::claim_helper_call`] — **the same budget the helpers spend,
/// deliberately**: both are cheap-model errands a program can put inside a
/// loop, and one shared ceiling is what a model can reason about. A second
/// budget would mean a cell refused for helpers could still spend on
/// judgements, which is not a limit anybody could hold in their head.
fn decide_choice_callback(
    scope: &mut v8::PinScope,
    args: v8::FunctionCallbackArguments,
    mut retval: v8::ReturnValue,
) {
    let wanted = "decide.choice takes the question, then an object of criteria \
                  ({name: when it applies}), and optionally the text to judge";
    if !args.get(0).is_string() {
        throw_tool_error(scope, wanted);
        return;
    }
    let instructions = args.get(0).to_rust_string_lossy(scope);
    let Some(criteria) = read_criteria(scope, args.get(1)) else {
        throw_tool_error(scope, wanted);
        return;
    };
    let subject = if args.get(2).is_undefined() || args.get(2).is_null() {
        String::new()
    } else {
        args.get(2).to_rust_string_lossy(scope)
    };

    let state = state(scope);
    if state.token.borrow().is_cancelled() {
        throw_cancelled(scope, "decide.choice");
        return;
    }
    let model = match state.decision_model() {
        Ok(model) => model,
        Err(reason) => {
            throw_tool_error(scope, &reason);
            return;
        }
    };
    if let Err(reason) = state.claim_helper_call() {
        throw_tool_error(scope, &reason);
        return;
    }

    // A judgement is recorded where a helper call is recorded, so the lane,
    // the inspector and the rollout show it with the same shape: the question
    // is what it was `asked`, and the answer is what came back.
    //
    // Not `asked_summary`, which counts a blob's lines: a helper is handed
    // the payload and the useful summary is its size, while a judgement is
    // handed a *question* and the useful summary is the question. The
    // subject stays out of the record, as a helper's payload does.
    let asked = question_summary(&instructions);
    let slot = state.begin_helper(crate::helpers::HelperRecord {
        helper: "decide".to_string(),
        verb: "deciding".to_string(),
        asked: asked.clone(),
        ..crate::helpers::HelperRecord::default()
    });

    let started = std::time::Instant::now();
    // Waiting on the decision model is the cell waiting, not the cell
    // computing (`RuntimeState::away_from_js`); the question carries
    // `decide::DECISION_TIMEOUT` of its own.
    let answered = {
        let _away = state.away_from_js();
        crate::decide::judgement(&model, &instructions, &subject, criteria)
    };
    let elapsed_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
    let (ok, text) = match &answered {
        Ok(judgement) => (
            true,
            format!("{} ({:.2})", judgement.choice, judgement.confidence),
        ),
        Err(error) => (false, error.to_string()),
    };
    state.finish_helper(
        slot,
        crate::helpers::HelperCall {
            outcome: crate::helpers::HelperOutcome {
                text: text.clone(),
                ok,
                cancelled: false,
                elapsed_ms,
            },
            turns: 1,
            looked: Vec::new(),
            usage: crate::helpers::HelperUsage {
                coverage_known: true,
                model: model.clone(),
                requests: 1,
                responses: u32::from(ok),
                ..crate::helpers::HelperUsage::default()
            },
        },
    );
    trace(scope).record(CallRecord {
        tool: "decide.choice".to_string(),
        args: [("asked".to_string(), asked)].into_iter().collect(),
        evidence: None,
        lifted_from: None,
        exit_code: None,
        repeat_of: None,
        error: None,
        ended: if ok {
            Ended::Ok
        } else {
            Ended::Threw {
                class: "ToolError".into(),
            }
        },
    });

    let judgement = match answered {
        Ok(judgement) => judgement,
        Err(error) => {
            throw_tool_error(scope, &error.to_string());
            return;
        }
    };
    let object = v8::Object::new(scope);
    let choice = js_string(scope, &judgement.choice);
    set_key(scope, object, "choice", choice);
    let confidence = v8::Number::new(scope, judgement.confidence);
    set_key(scope, object, "confidence", confidence.into());
    let probabilities = v8::Object::new(scope);
    for (name, value) in &judgement.probabilities {
        let value = v8::Number::new(scope, *value);
        set_key(scope, probabilities, name, value.into());
    }
    set_key(scope, object, "probabilities", probabilities.into());
    retval.set(object.into());
}

/// The question, as one bounded line for the lane and the inspector.
fn question_summary(instructions: &str) -> String {
    let line = instructions
        .trim()
        .lines()
        .next()
        .unwrap_or_default()
        .trim();
    if line.chars().count() <= QUESTION_SUMMARY_CHARS {
        return line.to_string();
    }
    let kept: String = line.chars().take(QUESTION_SUMMARY_CHARS).collect();
    format!("{kept}…")
}

/// How much of the question the lane carries. One line of a terminal row,
/// which is what the helper lane beside it gets.
const QUESTION_SUMMARY_CHARS: usize = 72;

/// The `{name: when it applies}` object as criteria, or `None` when it is not
/// an object of strings. Bounds are `decide::judgement`'s, checked there so
/// one place decides what a question may carry.
fn read_criteria(
    scope: &mut v8::PinScope,
    value: v8::Local<v8::Value>,
) -> Option<BTreeMap<String, String>> {
    let object = v8::Local::<v8::Object>::try_from(value).ok()?;
    let names = object.get_own_property_names(scope, v8::GetPropertyNamesArgs::default())?;
    let mut criteria = BTreeMap::new();
    for index in 0..names.length() {
        let key = names.get_index(scope, index)?;
        let text = object.get(scope, key)?;
        criteria.insert(
            key.to_rust_string_lossy(scope),
            text.to_rust_string_lossy(scope),
        );
    }
    Some(criteria)
}
