//! The named-check host binding. Helpers never receive this effectful global.
use std::cell::RefCell;
use std::rc::Rc;

use crate::runtime::bindings::{self, CellTrace};
use crate::runtime::outcome::{CallRecord, Ended};
use crate::tools::invoke::ToolContext;
use crate::verification::{self, Verification};

struct Checks(Rc<RefCell<Verification>>);

pub(crate) fn clear(isolate: &mut v8::Isolate) {
    if let Some(checks) = isolate.get_slot::<Checks>() {
        *checks.0.borrow_mut() = Verification::default();
    }
}

pub(crate) fn install(scope: &mut v8::PinScope, global: v8::Local<v8::Object>) {
    scope.set_slot(Checks(Rc::new(RefCell::new(Verification::default()))));
    let object = v8::Object::new(scope);
    for (name, function) in [
        ("run", v8::Function::builder(run).build(scope)),
        ("list", v8::Function::builder(list).build(scope)),
    ] {
        if let Some(function) = function {
            let key = v8::String::new(scope, name).expect("fixed key");
            object.define_own_property(
                scope,
                key.into(),
                function.into(),
                v8::PropertyAttribute::READ_ONLY | v8::PropertyAttribute::DONT_DELETE,
            );
        }
    }
    let key = v8::String::new(scope, "checks").expect("fixed key");
    global.define_own_property(
        scope,
        key.into(),
        object.into(),
        v8::PropertyAttribute::READ_ONLY | v8::PropertyAttribute::DONT_DELETE,
    );
}

/// Binds the dialect spellings of `checks.run` to the very same callback.
///
/// The invariant: `RunTests` is not a wrapper around `checks.run`, it *is*
/// `checks.run` under another name, so there is no second freshness-and-reuse
/// implementation for the two spellings to disagree about
/// (`tool-abi.md` §13.8).
pub(crate) fn install_aliases(scope: &mut v8::PinScope, global: v8::Local<v8::Object>) {
    for alias in crate::abi::dialect::host_call_aliases("checks.run") {
        let Some(function) = v8::Function::builder(run).build(scope) else {
            continue;
        };
        let Some(key) = v8::String::new(scope, alias) else {
            continue;
        };
        global.define_own_property(
            scope,
            key.into(),
            function.into(),
            v8::PropertyAttribute::READ_ONLY | v8::PropertyAttribute::DONT_DELETE,
        );
    }
}

fn throw(scope: &mut v8::PinScope, error: &str) {
    bindings::throw_tool_error(scope, error);
}

fn value(scope: &mut v8::PinScope, result: &impl serde::Serialize, mut retval: v8::ReturnValue) {
    if let Ok(json) = serde_json::to_string(result)
        && let Some(json) = v8::String::new(scope, &json)
        && let Some(value) = v8::json::parse(scope, json)
    {
        retval.set(value);
    }
}

fn list(scope: &mut v8::PinScope, _args: v8::FunctionCallbackArguments, retval: v8::ReturnValue) {
    let state = bindings::state(scope);
    match verification::load(&state.profile) {
        Ok(config) => value(scope, &config.checks, retval),
        Err(error) => throw(scope, &error),
    }
}

fn execute(
    scope: &mut v8::PinScope,
    name: &str,
    force: bool,
) -> Result<verification::CheckResult, String> {
    let state = bindings::state(scope);
    let checks = scope
        .get_slot::<Checks>()
        .expect("checks installed")
        .0
        .clone();
    let token = state.token.borrow().clone();
    let result = checks.borrow_mut().run(
        name,
        force,
        &ToolContext {
            profile: &state.profile,
            session: &state.session,
        },
        &token,
    );
    let args = match &result {
        Ok(result) => [
            ("name".into(), name.into()),
            ("command".into(), result.command.clone()),
            ("executed".into(), result.executed.to_string()),
            ("reused".into(), result.reused.to_string()),
        ]
        .into_iter()
        .collect(),
        Err(_) => [("name".into(), name.into())].into_iter().collect(),
    };
    scope
        .get_slot::<Rc<CellTrace>>()
        .expect("trace installed")
        .record(CallRecord {
            tool: "checks.run".into(),
            args,
            evidence: None,
            lifted_from: None,
            exit_code: result.as_ref().ok().and_then(|result| result.exit_code),
            repeat_of: None,
            error: result.as_ref().err().cloned(),
            ended: match &result {
                Ok(_) => Ended::Ok,
                Err(_) => Ended::Threw {
                    class: "ToolError".into(),
                },
            },
        });
    result
}

fn run(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, retval: v8::ReturnValue) {
    if !args.get(0).is_string() || (!args.get(1).is_undefined() && !args.get(1).is_boolean()) {
        throw(
            scope,
            "checks.run(name, force?) expects a named check and optional boolean",
        );
        return;
    }
    let name = args.get(0).to_rust_string_lossy(scope);
    let force = args.get(1).is_true();
    match execute(scope, &name, force) {
        Ok(result) => value(scope, &result, retval),
        Err(error) => throw(scope, &error),
    }
}
