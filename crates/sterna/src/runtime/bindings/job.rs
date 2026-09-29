//! The handle `bg.run` and `bg.watch` answer with, and the one wait a
//! program may ask of it: `job.result()`.
//!
//! A slow command whose result the next step does not need runs as a job
//! while the model reads and edits in this cell and the next ones; the
//! program collects it where it needs it instead of every cell waiting on
//! it in turn.

use super::*;

pub(super) fn job_object<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    handle: &str,
) -> v8::Local<'s, v8::Value> {
    let object = v8::Object::new(scope);
    let id = js_string(scope, handle);
    set_fixed_key(scope, object, "id", id);
    let source = js_string(scope, &format!("bg/{handle}"));
    set_fixed_key(scope, object, "source", source);
    // The handle travels in the function's data slot, as `agent_object`'s
    // `progress` does: one `fn` item serves every job object.
    let data = js_string(scope, handle);
    if let Some(function) = v8::Function::builder(job_result_callback)
        .data(data)
        .build(scope)
    {
        set_fixed_key(scope, object, "result", function.into());
    }
    object.into()
}

/// `job.result()` -- `{stdout, stderr, exit_code, status}` once the job has
/// finished, the shape `bash` answers with plus the job's own status word
/// (`cancelled` for a job stopped or past its timeout).
///
/// The wait is not the cell's own computing, so the cell clock is paused
/// through it, as it is through a foreground command. An interrupt stops
/// the wait with `Cancelled` and leaves the job running.
fn job_result_callback(
    scope: &mut v8::PinScope,
    args: v8::FunctionCallbackArguments,
    mut retval: v8::ReturnValue,
) {
    let handle = args.data().to_rust_string_lossy(scope);
    let state = state(scope);
    let token = state.token.borrow().clone();
    let waited = {
        let _away = state.away_from_js();
        crate::bg::wait(&state.session, &handle, || token.is_cancelled())
    };
    let job = match waited {
        crate::bg::Waited::Done(job) => job,
        crate::bg::Waited::Stopped => return throw_cancelled(scope, "job.result"),
        crate::bg::Waited::Unknown => {
            return throw_tool_error(
                scope,
                &format!("`job.result()`: this session has no job `{handle}`"),
            );
        }
    };
    let object = v8::Object::new(scope);
    let stdout = js_string(scope, &job.stdout);
    set_key(scope, object, "stdout", stdout);
    let stderr = js_string(scope, &job.stderr);
    set_key(scope, object, "stderr", stderr);
    let exit_code: v8::Local<v8::Value> = match job.status.parse::<i32>() {
        Ok(code) => v8::Integer::new(scope, code).into(),
        Err(_) => v8::null(scope).into(),
    };
    set_key(scope, object, "exit_code", exit_code);
    let status = js_string(scope, &job.status);
    set_key(scope, object, "status", status);
    retval.set(object.into());
}
