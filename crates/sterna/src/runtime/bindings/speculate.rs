//! `speculate(check, candidates)`: a few candidate changes tried against one
//! check in a single turn.
//!
//! A cell is a program, so deciding between plausible fixes need not cost a
//! turn per fix. Each candidate's edits are written, `check` runs, and every
//! file it touched is written back to the bytes it held before the next
//! candidate starts: nothing is left changed, and the one that passed is
//! applied with an ordinary `edit`. The writes, the restores and the command
//! are the same checked `write` and `bash` calls a cell makes, under the same
//! grant and the same approval gate, so nothing here reaches past what those
//! two may already do.

use super::*;
use crate::sandbox::profile::Access;

/// The most candidates one call tries.
const MAX_CANDIDATES: u32 = 4;
/// The most edits one candidate carries.
const MAX_EDITS: u32 = 16;
/// How much of the check's stdout and stderr a result keeps, from the end,
/// where a runner writes its verdict.
const KEPT_CHARS: usize = 4_000;

struct Edit {
    path: String,
    old: String,
    replacement: String,
}

struct Candidate {
    name: String,
    edits: Vec<Edit>,
}

/// One candidate's outcome.
struct Tried {
    name: String,
    applied: bool,
    error: Option<String>,
    exit_code: Option<i32>,
    stdout: String,
    stderr: String,
}

/// Binds `speculate` where `globals` installs it.
pub(super) fn install(
    scope: &mut v8::PinScope,
    global: v8::Local<v8::Object>,
    globals: HostGlobals,
) {
    if globals.installs("speculate")
        && let Some(function) = v8::Function::builder(speculate_callback).build(scope)
    {
        set_fixed_key(scope, global, "speculate", function.into());
    }
}

fn speculate_callback(
    scope: &mut v8::PinScope,
    args: v8::FunctionCallbackArguments,
    mut retval: v8::ReturnValue,
) {
    let check = args.get(0);
    if !check.is_string() {
        return throw_tool_error(
            scope,
            "`speculate(check, candidates)`: `check` is the command each candidate is tried with",
        );
    }
    let check = check.to_rust_string_lossy(scope);
    let candidates = match read_candidates(scope, args.get(1)) {
        Ok(candidates) => candidates,
        Err(message) => return throw_tool_error(scope, &message),
    };
    let state = state(scope);
    if state.instruction_boundary("bash", &Args::new().with("command", &check)) {
        stop_for_instructions(scope, "`speculate`", "no candidate was tried");
        return;
    }
    let token = state.token.borrow().clone();
    let gate = (!state.subagent.get())
        .then(|| state.approval_gate.borrow().clone())
        .flatten();
    let context = ToolContext {
        profile: &state.profile,
        session: &state.session,
    };
    let call = |name: &str, args: &Args| {
        invoke::run_traced_pausing(
            &context,
            &token,
            name,
            args,
            gate.as_ref(),
            &|| token.is_cancelled(),
            Some(&state.host_clock),
        )
        .outcome
    };
    let mut tried = Vec::new();
    for candidate in &candidates {
        match try_candidate(&state.profile, &call, &check, candidate) {
            Ok(result) => tried.push(result),
            Err(message) => return throw_tool_error(scope, &message),
        }
    }
    let array = v8::Array::new(scope, tried.len() as i32);
    for (index, result) in tried.iter().enumerate() {
        let object = v8::Object::new(scope);
        let name = js_string(scope, &result.name);
        set_key(scope, object, "name", name);
        let applied = v8::Boolean::new(scope, result.applied);
        set_key(scope, object, "applied", applied.into());
        let error: v8::Local<v8::Value> = match &result.error {
            Some(error) => js_string(scope, error),
            None => v8::null(scope).into(),
        };
        set_key(scope, object, "error", error);
        let exit_code: v8::Local<v8::Value> = match result.exit_code {
            Some(code) => v8::Integer::new(scope, code).into(),
            None => v8::null(scope).into(),
        };
        set_key(scope, object, "exit_code", exit_code);
        let stdout = js_string(scope, &result.stdout);
        set_key(scope, object, "stdout", stdout);
        let stderr = js_string(scope, &result.stderr);
        set_key(scope, object, "stderr", stderr);
        array.set_index(scope, index as u32, object.into());
    }
    retval.set(array.into());
}

/// One candidate: its edits written, `check` run, and every written file put
/// back. `Ok` with `applied: false` for a candidate whose edits could not be
/// made; `Err` only when a file could not be put back, which the program
/// must hear about before it edits anything.
fn try_candidate(
    profile: &crate::sandbox::profile::Profile,
    call: &dyn Fn(&str, &Args) -> Result<ToolResult, ToolError>,
    check: &str,
    candidate: &Candidate,
) -> Result<Tried, String> {
    let mut tried = Tried {
        name: candidate.name.clone(),
        applied: false,
        error: None,
        exit_code: None,
        stdout: String::new(),
        stderr: String::new(),
    };
    let mut originals: Vec<(String, String)> = Vec::new();
    let mut patched: Vec<(String, String)> = Vec::new();
    for edit in &candidate.edits {
        let current = match patched.iter().find(|(path, _)| path == &edit.path) {
            Some((_, text)) => text.clone(),
            None => {
                let read = profile
                    .check("speculate", Access::Read, std::path::Path::new(&edit.path))
                    .ok()
                    .and_then(|path| std::fs::read_to_string(path).ok());
                let Some(text) = read else {
                    tried.error = Some(format!("`{}` could not be read as text", edit.path));
                    return Ok(tried);
                };
                originals.push((edit.path.clone(), text.clone()));
                text
            }
        };
        let count = if edit.old.is_empty() {
            0
        } else {
            current.matches(edit.old.as_str()).count()
        };
        if count != 1 {
            tried.error = Some(format!(
                "an `old` in `{}` occurs {count} times; it must occur exactly once",
                edit.path
            ));
            return Ok(tried);
        }
        let next = current.replacen(edit.old.as_str(), &edit.replacement, 1);
        match patched.iter_mut().find(|(path, _)| path == &edit.path) {
            Some((_, text)) => *text = next,
            None => patched.push((edit.path.clone(), next)),
        }
    }
    let mut written = Vec::new();
    for (path, text) in &patched {
        match call(
            "write",
            &Args::new().with("path", path).with("content", text),
        ) {
            Ok(_) => written.push(path.clone()),
            Err(error) => {
                tried.error = Some(error.to_string());
                break;
            }
        }
    }
    if tried.error.is_none() {
        tried.applied = true;
        match call("bash", &Args::new().with("command", check)) {
            Ok(result) => {
                tried.exit_code = result.exit_code;
                tried.stdout = tail(&result.stdout);
                tried.stderr = tail(&result.stderr);
            }
            Err(error) => tried.error = Some(error.to_string()),
        }
    }
    for path in &written {
        let original = originals
            .iter()
            .find(|(seen, _)| seen == path)
            .map(|(_, text)| text.as_str())
            .unwrap_or_default();
        let restored = call(
            "write",
            &Args::new().with("path", path).with("content", original),
        )
        .is_ok()
            && profile
                .check("speculate", Access::Read, std::path::Path::new(path))
                .ok()
                .and_then(|resolved| std::fs::read_to_string(resolved).ok())
                .as_deref()
                == Some(original);
        if !restored {
            return Err(format!(
                "`speculate` could not put `{path}` back after trying `{}`: it may still hold \
                 that candidate's change, so read it before editing",
                candidate.name
            ));
        }
    }
    Ok(tried)
}

/// The last [`KEPT_CHARS`] characters of `text`.
fn tail(text: &str) -> String {
    let count = text.chars().count();
    text.chars()
        .skip(count.saturating_sub(KEPT_CHARS))
        .collect()
}

fn read_candidates(
    scope: &mut v8::PinScope,
    value: v8::Local<v8::Value>,
) -> Result<Vec<Candidate>, String> {
    let usage = format!(
        "`speculate(check, candidates)`: `candidates` is an array of 1 to {MAX_CANDIDATES} \
         `{{name, edits: [{{path, old, replacement}}]}}`, each with 1 to {MAX_EDITS} edits"
    );
    let array = v8::Local::<v8::Array>::try_from(value).map_err(|_| usage.clone())?;
    if array.length() == 0 || array.length() > MAX_CANDIDATES {
        return Err(usage);
    }
    let mut candidates = Vec::new();
    for index in 0..array.length() {
        let item = array.get_index(scope, index).ok_or_else(|| usage.clone())?;
        let name =
            read_option(scope, item, "name").unwrap_or_else(|| format!("candidate {}", index + 1));
        let edits = property(scope, item, "edits")
            .and_then(|edits| v8::Local::<v8::Array>::try_from(edits).ok())
            .filter(|edits| edits.length() > 0 && edits.length() <= MAX_EDITS)
            .ok_or_else(|| usage.clone())?;
        let mut read = Vec::new();
        for at in 0..edits.length() {
            let edit = edits.get_index(scope, at).ok_or_else(|| usage.clone())?;
            let field = |scope: &mut v8::PinScope, key: &str| {
                read_option(scope, edit, key).ok_or_else(|| usage.clone())
            };
            read.push(Edit {
                path: field(scope, "path")?,
                old: field(scope, "old")?,
                replacement: field(scope, "replacement")?,
            });
        }
        candidates.push(Candidate { name, edits: read });
    }
    Ok(candidates)
}

/// One property of an object value, or `None` when there is none.
fn property<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    value: v8::Local<'s, v8::Value>,
    key: &str,
) -> Option<v8::Local<'s, v8::Value>> {
    let object = v8::Local::<v8::Object>::try_from(value).ok()?;
    let key = v8::String::new(scope, key)?;
    object
        .get(scope, key.into())
        .filter(|found| !found.is_undefined() && !found.is_null())
}
