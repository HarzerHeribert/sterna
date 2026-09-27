//! One fixture answer, rendered in whichever transport the request asked for.
//!
//! A helper sets `"stream": true` and reads Server-Sent Events, so that its
//! ceiling can measure silence rather than duration
//! (`wire::SIDE_ERRAND_SILENCE`) — the one-shot errand and, since
//! 2026-09-19, the narrowed loop that holds tools. Supervisor and decision
//! traffic still take the whole response. Rendering the same assistant JSON
//! both ways keeps
//! every expectation about content and usage identical across the two paths
//! — a fixture that answered SSE to everybody would be testing a transport no
//! caller asked for.

use serde_json::Value;

/// The content type and body a fixture should write back, given the request
/// it just read and the complete Messages response it means to send.
pub fn response_for(request: &Value, whole: &str) -> (&'static str, String) {
    if request.get("stream").and_then(Value::as_bool) != Some(true) {
        return ("application/json", whole.to_string());
    }

    let value: Value = serde_json::from_str(whole).expect("fixture reply is a JSON object");
    let blocks = value
        .get("content")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    // **Every block, at its own index.** A narrowed loop streams and calls
    // tools, so a reply's `tool_use` has to survive the rendering — an
    // earlier version of this module collapsed the whole reply to one text
    // string and refused anything else, which was right while only the
    // one-shot errand streamed and became a fixture that could not express
    // what the path under test does.
    let mut blocks_sse = Vec::new();
    for (index, block) in blocks.iter().enumerate() {
        match block.get("type").and_then(Value::as_str) {
            Some("text") => {
                let text = block
                    .get("text")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                blocks_sse.push(serde_json::json!({"type":"content_block_start","index":index,"content_block":{"type":"text","text":""}}));
                blocks_sse.push(serde_json::json!({"type":"content_block_delta","index":index,"delta":{"type":"text_delta","text":text}}));
            }
            Some("tool_use") => {
                let input = block
                    .get("input")
                    .cloned()
                    .unwrap_or_else(|| serde_json::json!({}));
                blocks_sse.push(serde_json::json!({"type":"content_block_start","index":index,"content_block":{
                    "type":"tool_use",
                    "id": block.get("id").cloned().unwrap_or_else(|| serde_json::json!("fixture-call")),
                    "name": block.get("name").cloned().unwrap_or_else(|| serde_json::json!("execute_cell")),
                    "input": {},
                }}));
                // A real provider sends the arguments as JSON fragments, so
                // the fixture sends them as one fragment rather than as an
                // object the accumulator would never see over the wire.
                blocks_sse.push(serde_json::json!({"type":"content_block_delta","index":index,"delta":{
                    "type":"input_json_delta",
                    "partial_json": serde_json::to_string(&input).expect("fixture tool input is serialisable"),
                }}));
            }
            other => panic!("a streamed fixture reply cannot express a {other:?} block"),
        }
        blocks_sse.push(serde_json::json!({"type":"content_block_stop","index":index}));
    }

    let stop_reason = value
        .get("stop_reason")
        .and_then(Value::as_str)
        .unwrap_or("end_turn");
    let usage = value
        .get("usage")
        .cloned()
        .unwrap_or_else(|| serde_json::json!({}));

    // The whole usage object rides on `message_start` and `message_delta`
    // carries none. A real provider splits it — input there, output here —
    // and the accumulator merges the two; splitting it here as well would
    // make these fixtures report different numbers over the two transports,
    // which is the one thing this function exists to prevent.
    let mut events = vec![
        serde_json::json!({"type":"message_start","message":{"role":"assistant","usage":usage}}),
        serde_json::json!({"type":"ping"}),
    ];
    events.extend(blocks_sse);
    events.push(serde_json::json!({"type":"message_delta","delta":{"stop_reason":stop_reason}}));
    events.push(serde_json::json!({"type":"message_stop"}));
    let body = events
        .iter()
        .map(|event| format!("data: {event}\n\n"))
        .collect::<String>();
    ("text/event-stream", body)
}
