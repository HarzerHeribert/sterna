//! `bg` -- `run`, `watch`, `cancel` -- the handle the first two answer with,
//! and the one wait a program may ask of it: `job.result()`.
//!
//! A slow command whose result the next step does not need runs as a job
//! while the model reads and edits in this cell and the next ones; the
//! program collects it where it needs it instead of every cell waiting on
//! it in turn.

use super::*;
use crate::bg::{RunOptions, WatchOptions};

/// `events-contract.md` §5's three background-job entry points, on one
/// fixed object for the same reason every host function is fixed: a
/// program that replaced `bg` would lose the only way it has to stop what
/// it started, and nothing could put it back.
pub(super) fn install(
    scope: &mut v8::PinScope,
    global: v8::Local<v8::Object>,
    globals: HostGlobals,
) {
    if !globals.installs("bg") {
        return;
    }
    let background = v8::Object::new(scope);
    if let Some(function) = v8::Function::builder(bg_run_callback).build(scope) {
        set_fixed_key(scope, background, "run", function.into());
    }
    if let Some(function) = v8::Function::builder(bg_watch_callback).build(scope) {
        set_fixed_key(scope, background, "watch", function.into());
    }
    if let Some(function) = v8::Function::builder(bg_cancel_callback).build(scope) {
        set_fixed_key(scope, background, "cancel", function.into());
    }
    set_fixed_key(scope, global, "bg", background.into());
}

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

/// A foreground call whose child went on as a job ([`invoke::HandOver`]):
/// the job is bound under its own name, so the next cell can write
/// `await job3.result()` or `bg.cancel(job3)`, and the call throws saying so.
/// The program's own `await` never receives a result that is not there.
pub(super) fn hand_back(scope: &mut v8::PinScope, running: &ToolError) {
    if let ToolError::StillRunning { job, .. } = running {
        let context = scope.get_current_context();
        let global = context.global(scope);
        let object = job_object(scope, job);
        set_key(scope, global, job, object);
    }
    throw_tool_error(scope, &running.to_string());
}

/// `job.result()` -- `{stdout, stderr, exit_code, status}` once the job has
/// finished, the shape `bash` answers with plus the job's own status word
/// (`cancelled` for a job stopped or past its timeout).
///
/// The wait is not the cell's own computing, so the cell clock is paused
/// through it, as it is through a foreground command -- and like one, it
/// holds the cell for the cell's wall clock at most, then throws with the
/// job still running. An interrupt stops the wait with `Cancelled` and
/// leaves the job running.
fn job_result_callback(
    scope: &mut v8::PinScope,
    args: v8::FunctionCallbackArguments,
    mut retval: v8::ReturnValue,
) {
    let handle = args.data().to_rust_string_lossy(scope);
    // `{wait}` in milliseconds: how long this wait may hold the cell, for a
    // build the model knows takes minutes. Absent, the cell's wall clock.
    let wait = read_millis(scope, args.get(0), "wait").map(std::time::Duration::from_millis);
    let state = state(scope);
    let token = state.token.borrow().clone();
    let waited = {
        let _away = state.away_from_js();
        let patience = Some(wait.unwrap_or_else(|| state.patience.get()));
        crate::bg::wait(&state.session, &handle, || token.is_cancelled(), patience)
    };
    let job = match waited {
        crate::bg::Waited::Done(job) => job,
        crate::bg::Waited::Stopped => return throw_cancelled(scope, "job.result"),
        crate::bg::Waited::StillRunning(running) => {
            return throw_tool_error(
                scope,
                &format!(
                    "`{handle}` is still running ({} s so far); `result()` waits for it again \
                     (`result({{wait: ms}})` for longer), `bg.cancel(\"{handle}\")` stops it, and \
                     if you do neither its result arrives as a `bg.done` event",
                    running.as_secs()
                ),
            );
        }
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

/// §5's `bg.run`. The refusal is `Profile::admits_command`'s own and it
/// happens inside [`bg::run`] **before** a handle exists, so a program that
/// catches this exception is holding nothing.
fn bg_run_callback(
    scope: &mut v8::PinScope,
    args: v8::FunctionCallbackArguments,
    mut retval: v8::ReturnValue,
) {
    let command = args.get(0).to_rust_string_lossy(scope);
    let options = RunOptions {
        cwd: read_option(scope, args.get(1), "cwd"),
        env: read_option(scope, args.get(1), "env"),
        timeout_ms: read_millis(scope, args.get(1), "timeout"),
    };
    let state = state(scope);
    let instruction_args = Args::new().with("command", &command);
    if state.instruction_boundary("bg.run", &instruction_args) {
        stop_for_instructions(scope, "`bg.run`", "the job did not start");
        return;
    }
    match bg::run(&state.profile, &state.session, &command, &options) {
        Ok(handle) => {
            let object = job::job_object(scope, &handle);
            retval.set(object);
        }
        Err(denied) => throw_denied(scope, &denied),
    }
}

/// §5's `bg.watch`. `every` defaults to a second, which is the smallest
/// cadence a shell command can be run at without the polling itself being
/// the load.
///
/// **The default is not the bound.** A program that names its own `every` is
/// answered by `bg::watch`'s floor, which refuses a cadence under it rather
/// than clamping silently — the enforcement is there and not here so that no
/// caller of the module can get under it.
fn bg_watch_callback(
    scope: &mut v8::PinScope,
    args: v8::FunctionCallbackArguments,
    mut retval: v8::ReturnValue,
) {
    let command = args.get(0).to_rust_string_lossy(scope);
    let options = WatchOptions {
        every_ms: read_millis(scope, args.get(1), "every").unwrap_or(DEFAULT_WATCH_EVERY_MS),
        until: read_option(scope, args.get(1), "until"),
        timeout_ms: read_millis(scope, args.get(1), "timeout"),
    };
    let state = state(scope);
    let instruction_args = Args::new().with("command", &command);
    if state.instruction_boundary("bg.watch", &instruction_args) {
        stop_for_instructions(scope, "`bg.watch`", "the watcher did not start");
        return;
    }
    match bg::watch(&state.profile, &state.session, &command, &options) {
        Ok(handle) => {
            let object = job::job_object(scope, &handle);
            retval.set(object);
        }
        Err(denied) => throw_denied(scope, &denied),
    }
}

/// How often `bg.watch` runs its command when the model named no cadence.
const DEFAULT_WATCH_EVERY_MS: u64 = 1_000;

/// §5's `bg.cancel(handle)`: idempotent, and it takes either the object
/// `bg.run` answered with or the bare id off it, because a model that kept
/// only `job.id` should not have to reconstruct the object.
fn bg_cancel_callback(
    scope: &mut v8::PinScope,
    args: v8::FunctionCallbackArguments,
    _retval: v8::ReturnValue,
) {
    let given = args.get(0);
    let id = match v8::Local::<v8::Object>::try_from(given) {
        Ok(object) => v8::String::new(scope, "id")
            .and_then(|key| object.get(scope, key.into()))
            .map(|value| value.to_rust_string_lossy(scope))
            .unwrap_or_default(),
        Err(_) => given.to_rust_string_lossy(scope),
    };
    if id.is_empty() {
        return;
    }
    let session = state(scope).session.clone();
    bg::cancel(&session, &id);
}
