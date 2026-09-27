//! The `ask` global: one question put to the person, from inside the cell
//! that computed it.
//!
//! **It ends the cell, and it never waits.** A person takes longer to decide
//! than a cell is allowed to run, so `ask` ends the cell in the yield slot
//! exactly as `yieldNow` does and the answer arrives as an observation on the
//! next turn. Nothing here blocks: a program that asks cannot stall the
//! session, and a session with nobody at the keyboard is told so at the call
//! rather than after a timeout.
//!
//! **It is refused before it is recorded.** Whether this session can ask at
//! all is decided once, by the session that built the runtime
//! (`RuntimeState::ask_refusal`), and a refusal is a catchable `ToolError`
//! carrying the reason — nobody present, the setting off, or a request
//! narrowed to `explore`. A program can `try` it and carry on.

use super::*;

/// Binds the `ask` global. Called from [`super::install`] for every runtime,
/// because whether the call is *allowed* is a question the callback asks at
/// call time: a session can gain or lose a person, and a request can be
/// narrowed after the context was built.
pub(crate) fn install_ask(scope: &mut v8::PinScope) {
    let context = scope.get_current_context();
    let global = context.global(scope);
    if let Some(function) = v8::Function::builder(ask_callback).build(scope) {
        set_fixed_key(scope, global, "ask", function.into());
    }
}

/// `ask(question, choices)` — `runtime-contract.md` §9.3's yield slot, with a
/// question attached.
fn ask_callback(
    scope: &mut v8::PinScope,
    args: v8::FunctionCallbackArguments,
    _retval: v8::ReturnValue,
) {
    if let Some(refusal) = state(scope).ask_refusal.borrow().clone() {
        throw_tool_error(scope, &refusal);
        return;
    }
    let wanted = "ask takes the question, then an array of the choices to choose between";
    if !args.get(0).is_string() {
        throw_tool_error(scope, wanted);
        return;
    }
    let question = args.get(0).to_rust_string_lossy(scope);
    let Some(choices) = read_choices(scope, args.get(1)) else {
        throw_tool_error(scope, wanted);
        return;
    };
    let question = match crate::ask::Question::new(&question, choices) {
        Ok(question) => question,
        Err(refusal) => {
            throw_tool_error(scope, &refusal);
            return;
        }
    };

    trace(scope).record(CallRecord {
        tool: "ask".to_string(),
        args: [("asked".to_string(), question.question.clone())]
            .into_iter()
            .collect(),
        evidence: None,
        lifted_from: None,
        exit_code: None,
        repeat_of: None,
        error: None,
        ended: Ended::Ok,
    });
    trace(scope).record_ask(question);
    scope.terminate_execution();
    // The same stack check `yield_now_callback` documents: V8 services a
    // requested termination at a function entry or a loop back-edge, never on
    // the return from an API callback, so entering this loop is what stops
    // the program here instead of letting it run on past the question.
    if let Some(source) = v8::String::new(scope, "for (;;) {}")
        && let Some(script) = v8::Script::compile(scope, source, None)
    {
        script.run(scope);
    }
}

/// The choices as a list of strings, or `None` when the argument is not an
/// array of them. How many and how long they may be is
/// [`crate::ask::Question::new`]'s, checked there so one place decides what a
/// question is.
fn read_choices(scope: &mut v8::PinScope, value: v8::Local<v8::Value>) -> Option<Vec<String>> {
    let array = v8::Local::<v8::Array>::try_from(value).ok()?;
    let mut choices = Vec::new();
    for index in 0..array.length() {
        let entry = array.get_index(scope, index)?;
        if !entry.is_string() {
            return None;
        }
        choices.push(entry.to_rust_string_lossy(scope));
    }
    Some(choices)
}
