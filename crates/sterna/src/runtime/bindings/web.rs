//! The `web` global: bound while the broker is on, and every call recorded
//! as one rollout line (map 2656, 2658).
//!
//! **A fetch reaches the session's allowed hosts at once and asks for any
//! other**, the way a command asks to leave the sandbox: the person can let
//! the host through once, for the session or always. At Full access nothing
//! asks; with nobody to ask, the fetch is refused and says what to do.

use super::*;
use std::sync::Arc;

/// Binds the `web` global on the current context. Called once, by
/// `Runtime::with_web_broker`, and only while `web.enabled` is on and the
/// narrowing admits `web` ([`HostGlobals::installs_with`]): a session that
/// turned the broker off never holds the name, and the Runtime block never
/// declares it.
pub(crate) fn install_web(scope: &mut v8::PinScope) {
    let context = scope.get_current_context();
    let global = context.global(scope);
    let web = v8::Object::new(scope);
    for name in ["fetch", "search"] {
        let data = js_string(scope, name);
        if let Some(function) = v8::Function::builder(web_callback).data(data).build(scope) {
            set_fixed_key(scope, web, name, function.into());
        }
    }
    set_fixed_key(scope, global, "web", web.into());
}

fn web_callback(
    scope: &mut v8::PinScope,
    args: v8::FunctionCallbackArguments,
    mut retval: v8::ReturnValue,
) {
    let operation = args.data().to_rust_string_lossy(scope);
    let name = format!("web.{operation}");
    if !args.get(0).is_string() {
        throw_tool_error(
            scope,
            "web.fetch requires a URL string; web.search requires a query string",
        );
        return;
    }
    let input = args.get(0).to_rust_string_lossy(scope);
    let state = state(scope);
    if state.token.borrow().is_cancelled() {
        throw_cancelled(scope, &name);
        return;
    }
    let result = (|| -> Result<serde_json::Value, Stop> {
        let broker = state.web.borrow();
        let broker = broker
            .as_ref()
            .ok_or_else(|| Stop::Error("web tools are off; `web.enabled` turns them on".into()))?;
        let token = state.token.borrow().clone();
        match operation.as_str() {
            "fetch" => {
                let host = broker.host_of(&input).map_err(Stop::Error)?;
                let reaches = reaches(&state, &input, &host)?;
                serde_json::to_value(
                    broker
                        .fetch_cancellable(&input, &token, reaches)
                        .map_err(Stop::Error)?,
                )
                .map_err(|e| Stop::Error(e.to_string()))
            }
            "search" => serde_json::to_value(
                broker
                    .search_cancellable(&input, &token)
                    .map_err(Stop::Error)?,
            )
            .map_err(|e| Stop::Error(e.to_string())),
            _ => Err(Stop::Error("unknown web operation".into())),
        }
    })();
    // The rollout line (map 2656): what was asked and what came back — the
    // URL or query as given, then the status, size and type of the answer.
    let mut recorded = std::collections::BTreeMap::new();
    recorded.insert(
        if operation == "search" {
            "query"
        } else {
            "url"
        }
        .to_string(),
        input.clone(),
    );
    if let Ok(json) = &result {
        for (key, field) in [("status", "status"), ("content_type", "content_type")] {
            if let Some(value) = json.get(field) {
                let text = value
                    .as_str()
                    .map_or_else(|| value.to_string(), str::to_string);
                recorded.insert(key.to_string(), text);
            }
        }
        if let Some(content) = json.get("content").and_then(|c| c.as_str()) {
            recorded.insert("bytes".to_string(), content.len().to_string());
        }
        if let Some(results) = json.get("results").and_then(|r| r.as_array()) {
            recorded.insert("results".to_string(), results.len().to_string());
        }
        if let Some(provider) = json.get("provider").and_then(|p| p.as_str()) {
            recorded.insert("provider".to_string(), provider.to_string());
        }
    }
    let (error, ended) = match &result {
        Ok(_) => (None, Ended::Ok),
        Err(Stop::Error(reason)) => (
            Some(reason.clone()),
            Ended::Threw {
                class: "ToolError".into(),
            },
        ),
        Err(Stop::Denied(rule)) => (Some(rule.clone()), Ended::Denied { rule: rule.clone() }),
        Err(Stop::Cancelled) => (
            Some("cancelled".into()),
            Ended::Threw {
                class: "Cancelled".into(),
            },
        ),
    };
    trace(scope).record(CallRecord {
        tool: name.clone(),
        args: recorded,
        evidence: None,
        lifted_from: None,
        exit_code: None,
        repeat_of: None,
        error,
        ended,
    });
    if state.token.borrow().is_cancelled() {
        throw_cancelled(scope, &name);
        return;
    }
    match result {
        Ok(json) => {
            let value = json_to_v8(scope, &json);
            tag_mcp_result(scope, &state, &name, value, &json.to_string());
            retval.set(value);
        }
        Err(Stop::Error(reason)) => throw_tool_error(scope, &reason),
        Err(Stop::Denied(rule)) => throw_denied(
            scope,
            &PermissionDenied {
                tool: name,
                path: input,
                rule,
            },
        ),
        Err(Stop::Cancelled) => throw_cancelled(scope, &name),
    }
}

/// Why a web call did not answer.
enum Stop {
    /// The broker or the page said no: a `ToolError`.
    Error(String),
    /// The host is out of reach and nobody let it through: a
    /// `PermissionDenied` whose rule says what to do instead.
    Denied(String),
    Cancelled,
}

/// What a fetch of `url`, whose first hop is `host`, reaches without
/// asking again: the allowed hosts, and -- once the person let it through
/// once -- `host` itself. Asks the person when `host` is not allowed.
fn reaches(state: &RuntimeState, url: &str, host: &str) -> Result<crate::web::Reaches, Stop> {
    // Full access has no sandbox to leave, for a fetch as for a command.
    if state.profile.os_sandbox_bypassed() {
        return Ok(Arc::new(|_: &str| true));
    }
    let allowed = state.hosts.borrow().clone();
    let listed = move |candidate: &str| {
        allowed
            .as_ref()
            .is_some_and(|allowed| allowed.permits(candidate))
    };
    if listed(host) {
        return Ok(Arc::new(listed));
    }
    let gate = state.approval_gate.borrow().clone();
    let Some(gate) = gate else {
        return Err(Stop::Denied(format!(
            "{host} is not an allowed host, and nobody can be asked here; fetch from an \
             allowed host, or say which host the work needs"
        )));
    };
    let arguments: crate::tools::invoke::CheckedArgs = [
        ("url".to_string(), url.to_string()),
        ("host".to_string(), host.to_string()),
        (
            crate::permissions::OUTSIDE.to_string(),
            format!("fetches from {host}, which is not an allowed host"),
        ),
    ]
    .into_iter()
    .collect();
    let action =
        crate::approval::Action::new(crate::approval::WEB_FETCH, state.profile.root(), arguments);
    let token = state.token.borrow().clone();
    match gate.admit(action.clone(), || token.is_cancelled()) {
        // Once is once: this fetch's first host, and the list for the rest.
        crate::approval::Admission::Allowed => {
            let once = host.to_string();
            Ok(Arc::new(move |candidate: &str| {
                candidate == once || listed(candidate)
            }))
        }
        // The host is on the list now, for every fetch and command after.
        crate::approval::Admission::HostsAllowed => Ok(Arc::new(listed)),
        crate::approval::Admission::Cancelled => Err(Stop::Cancelled),
        crate::approval::Admission::NobodyToAsk => Err(Stop::Denied(format!(
            "{host} is not an allowed host, and nobody is at the terminal to answer; fetch \
             from an allowed host, or say which host the work needs"
        ))),
        crate::approval::Admission::DeniedEarlier => Err(Stop::Denied(format!(
            "the person refused {host} earlier in this session; it stays refused until they \
             forget it on the Sandbox sheet (/sandbox)"
        ))),
        crate::approval::Admission::Denied => Err(Stop::Denied(match gate.redirect_for(&action) {
            Some(text) if text.trim().is_empty() => {
                "the person declined this fetch and asks you to propose another way to do it: \
                 say what you would do instead, then do that"
                    .to_string()
            }
            Some(text) => format!(
                "the person declined this fetch and asks for another way: \"{}\" -- do that \
                 instead",
                text.trim()
            ),
            None => format!("the person refused {host}"),
        })),
    }
}
