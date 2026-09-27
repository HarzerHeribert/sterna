//! The `web` global: bound only when `[web]` reaches something, and every
//! call recorded as one rollout line (map 2656, 2658).

use super::*;

/// Binds the `web` global on the current context. Called once, by
/// `Runtime::with_web_broker`, and only when the configuration names a
/// domain or an endpoint and the narrowing admits `web`
/// ([`HostGlobals::installs_with`]): a session that configured nothing
/// never holds the name, and the Runtime block never declares it.
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
    let result = (|| -> Result<serde_json::Value, String> {
        let broker = state.web.borrow();
        let broker = broker
            .as_ref()
            .ok_or("web access is disabled; configure [web] in .sterna/config.toml")?;
        match operation.as_str() {
            "fetch" => {
                serde_json::to_value(broker.fetch_cancellable(&input, &state.token.borrow())?)
                    .map_err(|e| e.to_string())
            }
            "search" => {
                serde_json::to_value(broker.search_cancellable(&input, &state.token.borrow())?)
                    .map_err(|e| e.to_string())
            }
            _ => Err("unknown web operation".into()),
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
    trace(scope).record(CallRecord {
        tool: name.clone(),
        args: recorded,
        evidence: None,
        lifted_from: None,
        exit_code: None,
        repeat_of: None,
        error: result.as_ref().err().cloned(),
        ended: if result.is_ok() {
            Ended::Ok
        } else {
            Ended::Threw {
                class: "ToolError".into(),
            }
        },
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
        Err(reason) => throw_tool_error(scope, &reason),
    }
}
