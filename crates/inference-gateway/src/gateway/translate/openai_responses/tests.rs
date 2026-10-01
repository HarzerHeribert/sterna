use super::*;

use super::super::anthropic::tests::full_request;
use super::super::canonical::{accumulate, tests::tool_call_response};
use super::super::stream::SseReader;
use std::io::BufReader;

/// `wire` with top-level `key` removed, for a test that must decode
/// bytes this codec's own encoder wrote minus one field.
fn drop_key(wire: &[u8], key: &str) -> Vec<u8> {
    let mut document: Value = serde_json::from_slice(wire).expect("valid JSON");
    document
        .as_object_mut()
        .expect("a top-level object")
        .remove(key);
    document.to_string().into_bytes()
}

/// The supported subset on this wire: [`full_request`] minus the stop
/// sequences the Responses API has no parameter for.
fn responses_request() -> Request {
    let mut request = full_request();
    request.stop = Vec::new();
    request
}

#[test]
fn a_request_round_trips_through_the_openai_responses_wire() {
    let request = responses_request();
    let wire = encode_request(&request);
    // `encode_request` always writes `prompt_cache_key` when `user` is
    // set (2018) — this codec's own hint, added only when it plays
    // *target*. No supported pair has openai-responses as both source
    // and target of itself (`SAME_PROTOCOL` is always refused), so this
    // codec's decoder never needs to read it back, and still refuses it
    // by name like a real Codex-shaped client that set its own
    // (`REFUSED_FIELDS`). Stripped here so the fidelity round trip
    // below covers everything else this codec carries.
    let wire = drop_key(&wire, "prompt_cache_key");
    let decoded = decode_request(&wire).expect("the codec reads what it wrote");
    assert_eq!(decoded, request);
}

#[test]
fn a_thinking_block_is_refused_rather_than_dropped() {
    let mut request = responses_request();
    request.messages.push(Message {
        role: Role::Assistant,
        blocks: vec![Block::Thinking {
            thinking: "reasoning the harness never sees".to_owned(),
            signature: "sig".to_owned(),
        }],
    });
    let refusal = OpenAiResponses
        .refuse_unencodable(&request)
        .expect_err("a thinking block has no OpenAI Responses equivalent");
    assert_eq!(refusal.field, "thinking block");
    assert!(!refusal.reason.contains("reasoning the harness never sees"));

    let mut request = responses_request();
    request.messages.push(Message {
        role: Role::Assistant,
        blocks: vec![Block::RedactedThinking {
            data: "opaque".to_owned(),
        }],
    });
    OpenAiResponses
        .refuse_unencodable(&request)
        .expect_err("a redacted thinking block has no OpenAI Responses equivalent either");
}

#[test]
fn effort_is_carried_as_nested_reasoning_effort_and_omitted_when_the_harness_asked_for_none() {
    use super::super::canonical::EffortRequest;

    let mut request = responses_request();
    request.effort = None;
    let wire = encode_request(&request);
    let document: Value = serde_json::from_slice(&wire).unwrap();
    assert_eq!(
        document.get("reasoning"),
        None,
        "no thinking asked for, no reasoning object emitted"
    );

    for (budget, word) in [
        (500u64, "minimal"),
        (4_096, "low"),
        (16_000, "medium"),
        (64_000, "high"),
    ] {
        request.effort = Some(EffortRequest {
            budget_tokens: Some(budget),
            level: None,
        });
        let wire = encode_request(&request);
        let document: Value = serde_json::from_slice(&wire).unwrap();
        assert_eq!(document["reasoning"]["effort"], word, "budget {budget}");
    }
}

#[test]
fn the_tool_round_crosses_the_responses_wire_with_ids_and_the_error_flag_intact() {
    let request = responses_request();
    let wire = encode_request(&request);
    let document: Value = serde_json::from_slice(&wire).unwrap();

    assert_eq!(document["instructions"], "You are careful.");
    assert_eq!(document["max_output_tokens"], 4096);
    assert_eq!(
        document["store"], false,
        "never left to the provider's default"
    );
    assert_eq!(document["user"], "user_123");
    assert_eq!(
        document["tool_choice"],
        json!({"type": "function", "name": "Bash"})
    );
    assert_eq!(document["parallel_tool_calls"], false);
    assert_eq!(document["stream"], true);
    assert_eq!(
        document.get("stop"),
        None,
        "no stop parameter exists on this wire"
    );

    let input = document["input"].as_array().expect("input items");
    assert_eq!(input[0]["type"], "message");
    assert_eq!(input[0]["role"], "user");
    assert_eq!(input[1]["role"], "assistant");
    assert_eq!(
        input[1]["content"],
        json!([{"type": "output_text", "text": "On it."}])
    );
    assert_eq!(input[2]["type"], "function_call");
    assert_eq!(input[2]["call_id"], "toolu_01A");
    assert_eq!(input[2]["name"], "Bash");
    assert_eq!(input[2]["arguments"], r#"{"command":"ls"}"#);
    assert_eq!(input[3]["call_id"], "toolu_01B");
    assert_eq!(input[4]["type"], "function_call_output");
    assert_eq!(input[4]["call_id"], "toolu_01A");
    assert_eq!(
        input[4]["output"],
        format!("{TOOL_ERROR_MARKER}\nls: cannot access"),
        "an erroring result is carried, labelled, in the only channel the wire has"
    );
    assert_eq!(input[5]["call_id"], "toolu_01B");
    assert_eq!(input[5]["output"], "# notes\nhello");
    assert_eq!(input[6]["type"], "message");
    assert_eq!(input[6]["role"], "user");
    let parts = input[6]["content"].as_array().unwrap();
    assert_eq!(
        parts[0],
        json!({"type": "input_text", "text": "Now summarise."})
    );
    assert_eq!(
        parts[1],
        json!({"type": "input_image", "image_url": "data:image/png;base64,iVBORw0KGgo="})
    );
    assert_eq!(
        parts[2],
        json!({"type": "input_image", "image_url": "https://example.test/a.png"})
    );

    let tools = document["tools"].as_array().expect("tools");
    assert_eq!(tools[0]["type"], "function");
    assert_eq!(
        tools[0]["name"], "Bash",
        "flat, unlike OpenAI Chat's nesting"
    );
    assert_eq!(
        tools[0]["strict"], false,
        "strict is never left to this wire's default, which is true"
    );
    assert_eq!(tools[0]["parameters"]["required"][0], "command");
}

#[test]
fn the_request_codex_sends_decodes_with_the_tool_run_merged_into_turns() {
    let wire = br#"{
        "model": "gpt-5",
        "instructions": "sys",
        "input": [
            {"type": "message", "role": "user", "content": [{"type": "input_text", "text": "go"}, {"type": "input_image", "image_url": "data:image/png;base64,AAAA", "detail": "auto"}]},
            {"type": "message", "role": "assistant", "content": [{"type": "output_text", "text": "Sure.", "annotations": []}]},
            {"type": "function_call", "id": "fc_1", "call_id": "call_1", "name": "f", "arguments": "", "status": "completed"},
            {"type": "function_call_output", "call_id": "call_1", "output": "ok"},
            {"role": "user", "content": "thanks"}
        ],
        "tools": [{"type": "function", "name": "f", "description": "d", "strict": false}],
        "tool_choice": "required",
        "max_output_tokens": 50,
        "store": false,
        "truncation": "disabled",
        "safety_identifier": "u1",
        "parallel_tool_calls": true,
        "stream": false
    }"#;
    let request = decode_request(wire).expect("a Codex-shaped request decodes");
    assert_eq!(request.system.as_deref(), Some("sys"));
    assert_eq!(request.messages.len(), 3);
    assert_eq!(
        request.messages[1].blocks,
        vec![
            Block::Text("Sure.".to_owned()),
            Block::ToolUse {
                id: "call_1".to_owned(),
                name: "f".to_owned(),
                input: json!({}),
            },
        ],
        "the assistant message item and its function_call are one turn"
    );
    assert_eq!(
        request.messages[2].blocks,
        vec![
            Block::ToolResult {
                tool_use_id: "call_1".to_owned(),
                content: "ok".to_owned(),
                is_error: false,
            },
            Block::Text("thanks".to_owned()),
        ]
    );
    assert_eq!(
        request.messages[0].blocks[1],
        Block::Image(ImageSource::Base64 {
            media_type: "image/png".to_owned(),
            data: "AAAA".to_owned()
        })
    );
    assert_eq!(request.tools[0].input_schema["type"], "object");
    assert_eq!(request.tool_choice, Some(ToolChoice::Any));
    assert_eq!(request.parallel_tool_calls, Some(true));
    assert_eq!(request.max_tokens, Some(50));
    assert_eq!(request.user.as_deref(), Some("u1"));
    assert!(request.stop.is_empty());
}

#[test]
fn every_refused_request_field_is_refused_by_its_name() {
    let base = |extra: &str| {
        format!(r#"{{"model": "m", "input": [{{"role": "user", "content": "x"}}]{extra}}}"#)
    };
    let cases: Vec<(String, &str)> = vec![
        (base(r#", "previous_response_id": "resp_1""#), "previous_response_id"),
        (base(r#", "store": true"#), "store"),
        (base(r#", "background": true"#), "background"),
        (base(r#", "include": ["reasoning.encrypted_content"]"#), "include"),
        (base(r#", "reasoning": {"effort": "medium"}"#), "reasoning"),
        (base(r#", "truncation": "auto""#), "truncation"),
        (base(r#", "metadata": {"a": "b"}"#), "metadata"),
        (base(r#", "top_logprobs": 3"#), "top_logprobs"),
        (base(r#", "max_tool_calls": 2"#), "max_tool_calls"),
        (base(r#", "service_tier": "flex""#), "service_tier"),
        (base(r#", "prompt": {"id": "p1"}"#), "prompt"),
        (base(r#", "prompt_cache_key": "k""#), "prompt_cache_key"),
        (base(r#", "text": {"format": {"type": "json_object"}}"#), "text.format.type"),
        (base(r#", "text": {"verbosity": "low"}"#), "text.verbosity"),
        (base(r#", "user": "a", "safety_identifier": "b""#), "safety_identifier"),
        (base(r#", "unknown_future_field": 1"#), "unknown_future_field"),
        (
            base(r#", "tools": [{"type": "web_search"}]"#),
            "tools[0].type",
        ),
        (
            base(r#", "tools": [{"type": "function", "name": "f", "strict": true}]"#),
            "tools[0].strict",
        ),
        (
            base(r#", "tool_choice": {"type": "mcp", "server_label": "s"}"#),
            "tool_choice.type",
        ),
        (
            r#"{"model": "m", "input": [{"type": "reasoning", "id": "rs_1", "summary": [], "encrypted_content": "xx"}]}"#.to_owned(),
            "input[0].type",
        ),
        (
            r#"{"model": "m", "input": [{"type": "item_reference", "id": "msg_1"}]}"#.to_owned(),
            "input[0].type",
        ),
        (
            r#"{"model": "m", "input": [{"type": "web_search_call", "id": "ws_1"}]}"#.to_owned(),
            "input[0].type",
        ),
        (
            r#"{"model": "m", "input": [{"role": "user", "content": [{"type": "input_file", "file_id": "f1"}]}]}"#.to_owned(),
            "input[0].content[0].type",
        ),
        (
            r#"{"model": "m", "input": [{"role": "assistant", "content": [{"type": "refusal", "refusal": "no"}]}]}"#.to_owned(),
            "input[0].content[0].type",
        ),
    ];
    for (wire, field) in cases {
        let refusal = decode_request(wire.as_bytes())
            .expect_err(&format!("{field} must be refused, in: {wire}"));
        assert_eq!(refusal.field, field, "{wire}");
        assert!(!refusal.reason.is_empty());
    }
}

#[test]
fn the_stop_sequences_this_wire_cannot_carry_are_refused_before_encoding() {
    let refusal = OpenAiResponses
        .refuse_unencodable(&full_request())
        .expect_err("full_request carries stop sequences");
    assert_eq!(refusal.field, "stop_sequences");
    assert!(refusal.reason.contains("no stop sequences"));
    assert!(
        OpenAiResponses
            .refuse_unencodable(&responses_request())
            .is_ok()
    );
}

#[test]
fn a_response_round_trips_through_the_openai_responses_wire() {
    let response = tool_call_response();
    let wire = encode_response(&response);
    let document: Value = serde_json::from_slice(&wire).unwrap();
    assert_eq!(document["status"], "completed");
    assert_eq!(document["output"][0]["type"], "message");
    assert_eq!(
        document["output"][1]["call_id"], "call_abc123",
        "the call_id is the canonical tool-use id, verbatim"
    );
    assert_eq!(document["output"][2]["call_id"], "call_def456");
    assert_eq!(document["usage"]["input_tokens"], 220);
    assert_eq!(
        document["usage"]["input_tokens_details"]["cached_tokens"],
        100
    );
    assert_eq!(
        decode_response(&wire).expect("the codec reads what it wrote"),
        response
    );

    // ... and a text-only max-tokens response, which crosses as
    // incomplete with its reason.
    let cut_short = Response {
        id: "resp_2".to_owned(),
        model: "m".to_owned(),
        blocks: vec![Block::Text("partial".to_owned())],
        stop_reason: StopReason::MaxTokens,
        stop_sequence: None,
        usage: Usage {
            input: 3,
            output: 2,
            cached: None,
            reasoning: None,
        },
    };
    let wire = encode_response(&cut_short);
    let document: Value = serde_json::from_slice(&wire).unwrap();
    assert_eq!(document["status"], "incomplete");
    assert_eq!(
        document["incomplete_details"]["reason"],
        "max_output_tokens"
    );
    assert_eq!(decode_response(&wire).unwrap(), cut_short);
}

#[test]
fn an_incomplete_response_maps_its_reason_and_a_failed_one_carries_the_providers_message() {
    let incomplete = |reason: &str| {
        format!(
            r#"{{"id": "r", "status": "incomplete", "incomplete_details": {{"reason": "{reason}"}}, "model": "m", "output": []}}"#
        )
    };
    assert_eq!(
        decode_response(incomplete("max_output_tokens").as_bytes())
            .unwrap()
            .stop_reason,
        StopReason::MaxTokens
    );
    assert_eq!(
        decode_response(incomplete("content_filter").as_bytes())
            .unwrap()
            .stop_reason,
        StopReason::Refusal
    );

    let failed = br#"{"id": "r", "status": "failed", "error": {"code": "server_error", "message": "the model ran aground"}, "model": "m", "output": []}"#;
    let refusal = decode_response(failed).unwrap_err();
    assert_eq!(refusal.field, "error");
    assert!(refusal.reason.contains("the model ran aground"));

    let in_progress = br#"{"id": "r", "status": "in_progress", "model": "m", "output": []}"#;
    assert_eq!(decode_response(in_progress).unwrap_err().field, "status");
}

#[test]
fn an_empty_reasoning_item_is_skipped_and_one_carrying_anything_is_refused() {
    let with_empty = br#"{"id": "r", "status": "completed", "model": "m", "output": [
        {"type": "reasoning", "id": "rs_1", "summary": []},
        {"type": "message", "id": "msg_1", "status": "completed", "role": "assistant", "content": [{"type": "output_text", "text": "hi", "annotations": []}]}
    ]}"#;
    let response = decode_response(with_empty).expect("an empty reasoning item is skipped");
    assert_eq!(response.blocks, vec![Block::Text("hi".to_owned())]);

    let with_summary = br#"{"id": "r", "status": "completed", "model": "m", "output": [
        {"type": "reasoning", "id": "rs_1", "summary": [{"type": "summary_text", "text": "thinking..."}]}
    ]}"#;
    assert_eq!(
        decode_response(with_summary).unwrap_err().field,
        "output[0].type"
    );

    let with_builtin = br#"{"id": "r", "status": "completed", "model": "m", "output": [
        {"type": "web_search_call", "id": "ws_1", "status": "completed"}
    ]}"#;
    let refusal = decode_response(with_builtin).unwrap_err();
    assert_eq!(refusal.field, "output[0].type");
    assert!(refusal.reason.contains("hosted tool"));
}

#[test]
fn a_stream_round_trips_through_the_openai_responses_wire() {
    let response = tool_call_response();
    let events = response.as_events();
    let mut encoder = EventEncoder::default();
    let mut wire = Vec::new();
    for event in &events {
        wire.extend(encoder.encode(event));
    }
    let text = String::from_utf8_lossy(&wire);
    assert!(text.contains("event: response.created\n"));
    assert!(text.contains("event: response.completed\n"));
    assert!(text.contains(r#""call_id":"call_abc123""#));

    let mut reader = SseReader::new(BufReader::new(&wire[..]));
    let mut decoder = EventDecoder::default();
    let mut decoded = Vec::new();
    while let Some(event) = reader.next_event().unwrap() {
        decoded.extend(decoder.feed(&event).expect("the codec reads what it wrote"));
    }
    assert!(decoder.is_done());
    assert_eq!(decoded, events);
}

/// The event shape a real Responses upstream sends for a text part and a
/// function call: lifecycle snapshots, `sequence_number` on every event,
/// `obfuscation` padding on deltas, the full-text and full-arguments
/// `done` echoes, and usage only in the final snapshot.
#[test]
fn real_provider_events_become_anthropics_event_order_with_ids_preserved() {
    let chunks = [
        r#"{"type":"response.created","sequence_number":0,"response":{"id":"resp_1","object":"response","created_at":1,"status":"in_progress","model":"gpt-5","output":[],"usage":null,"store":false,"tools":[]}}"#,
        r#"{"type":"response.in_progress","sequence_number":1,"response":{"id":"resp_1","object":"response","created_at":1,"status":"in_progress","model":"gpt-5","output":[],"usage":null}}"#,
        r#"{"type":"response.output_item.added","sequence_number":2,"output_index":0,"item":{"id":"msg_1","type":"message","status":"in_progress","role":"assistant","content":[]}}"#,
        r#"{"type":"response.content_part.added","sequence_number":3,"item_id":"msg_1","output_index":0,"content_index":0,"part":{"type":"output_text","text":"","annotations":[]}}"#,
        r#"{"type":"response.output_text.delta","sequence_number":4,"item_id":"msg_1","output_index":0,"content_index":0,"delta":"Sure.","logprobs":[],"obfuscation":"xK9"}"#,
        r#"{"type":"response.output_text.done","sequence_number":5,"item_id":"msg_1","output_index":0,"content_index":0,"text":"Sure.","logprobs":[]}"#,
        r#"{"type":"response.content_part.done","sequence_number":6,"item_id":"msg_1","output_index":0,"content_index":0,"part":{"type":"output_text","text":"Sure.","annotations":[]}}"#,
        r#"{"type":"response.output_item.done","sequence_number":7,"output_index":0,"item":{"id":"msg_1","type":"message","status":"completed","role":"assistant","content":[{"type":"output_text","text":"Sure.","annotations":[]}]}}"#,
        r#"{"type":"response.output_item.added","sequence_number":8,"output_index":1,"item":{"id":"fc_1","type":"function_call","status":"in_progress","call_id":"call_A","name":"Bash","arguments":""}}"#,
        r#"{"type":"response.function_call_arguments.delta","sequence_number":9,"item_id":"fc_1","output_index":1,"delta":"{\"command\"","obfuscation":"a"}"#,
        r#"{"type":"response.function_call_arguments.delta","sequence_number":10,"item_id":"fc_1","output_index":1,"delta":": \"ls\"}"}"#,
        r#"{"type":"response.function_call_arguments.done","sequence_number":11,"item_id":"fc_1","output_index":1,"name":"Bash","arguments":"{\"command\": \"ls\"}"}"#,
        r#"{"type":"response.output_item.done","sequence_number":12,"output_index":1,"item":{"id":"fc_1","type":"function_call","status":"completed","call_id":"call_A","name":"Bash","arguments":"{\"command\": \"ls\"}"}}"#,
        r#"{"type":"response.completed","sequence_number":13,"response":{"id":"resp_1","object":"response","created_at":1,"status":"completed","model":"gpt-5","output":[],"usage":{"input_tokens":50,"input_tokens_details":{"cached_tokens":10},"output_tokens":9,"output_tokens_details":{"reasoning_tokens":0},"total_tokens":59}}}"#,
    ];
    let mut decoder = EventDecoder::default();
    let mut events = Vec::new();
    for data in chunks {
        events.extend(
            decoder
                .feed(&SseEvent {
                    event: None,
                    data: data.to_owned(),
                })
                .expect("a real provider's events decode"),
        );
    }
    assert!(decoder.is_done());
    let response = accumulate(&events).expect("a complete message");
    assert_eq!(
        response.blocks,
        vec![
            Block::Text("Sure.".to_owned()),
            Block::ToolUse {
                id: "call_A".to_owned(),
                name: "Bash".to_owned(),
                input: json!({"command": "ls"}),
            },
        ]
    );
    assert_eq!(response.stop_reason, StopReason::ToolUse);
    assert_eq!(
        response.usage,
        Usage {
            input: 40,
            output: 9,
            cached: Some(10),
            reasoning: Some(0),
        }
    );
    // The order is Anthropic's: every block stops before the next
    // starts, and the message delta comes last but one.
    let stops: Vec<usize> = events
        .iter()
        .filter_map(|event| match event {
            StreamEvent::BlockStop { index } => Some(*index),
            _ => None,
        })
        .collect();
    assert_eq!(stops, vec![0, 1]);
    assert!(matches!(
        events[events.len() - 2],
        StreamEvent::MessageDelta {
            stop_reason: StopReason::ToolUse,
            ..
        }
    ));
}

#[test]
fn a_reasoning_summary_a_refusal_part_and_a_stream_error_are_named_refusals() {
    let mut decoder = EventDecoder::default();
    let summary = SseEvent {
        event: None,
        data: r#"{"type":"response.reasoning_summary_text.delta","sequence_number":1,"item_id":"rs_1","output_index":0,"summary_index":0,"delta":"hmm"}"#.to_owned(),
    };
    let refusal = decoder.feed(&summary).unwrap_err();
    assert!(refusal.reason.contains("reasoning item"));

    let mut decoder = EventDecoder::default();
    let refusal_part = SseEvent {
        event: None,
        data: r#"{"type":"response.content_part.added","sequence_number":1,"item_id":"msg_1","output_index":0,"content_index":0,"part":{"type":"refusal","refusal":"no"}}"#.to_owned(),
    };
    assert_eq!(decoder.feed(&refusal_part).unwrap_err().field, "part.type");

    let mut decoder = EventDecoder::default();
    let error = SseEvent {
        event: None,
        data: r#"{"type":"error","code":"server_error","message":"Overloaded","param":null,"sequence_number":2}"#.to_owned(),
    };
    let refusal = decoder.feed(&error).unwrap_err();
    assert_eq!(refusal.field, "error");
    assert!(refusal.reason.contains("Overloaded"));

    let mut decoder = EventDecoder::default();
    let failed = SseEvent {
        event: None,
        data: r#"{"type":"response.failed","sequence_number":3,"response":{"id":"r","status":"failed","error":{"code":"server_error","message":"upstream fell over"}}}"#.to_owned(),
    };
    let refusal = decoder.feed(&failed).unwrap_err();
    assert!(refusal.reason.contains("upstream fell over"));
}

#[test]
fn a_stream_that_ends_before_response_completed_is_refused_at_finish() {
    let mut decoder = EventDecoder::default();
    assert_eq!(decoder.finish().unwrap_err().field, "response.completed");

    // ... including one that started and streamed a block first.
    let mut decoder = EventDecoder::default();
    for data in [
        r#"{"type":"response.created","response":{"id":"r","model":"m","output":[],"usage":null}}"#,
        r#"{"type":"response.output_item.added","output_index":0,"item":{"id":"msg_1","type":"message","status":"in_progress","role":"assistant","content":[]}}"#,
    ] {
        decoder
            .feed(&SseEvent {
                event: None,
                data: data.to_owned(),
            })
            .unwrap();
    }
    assert!(!decoder.is_done());
    assert_eq!(decoder.finish().unwrap_err().field, "response.completed");

    // An orphan delta is refused rather than misfiled.
    let mut decoder = EventDecoder::default();
    let orphan = SseEvent {
        event: None,
        data: r#"{"type":"response.output_text.delta","item_id":"msg_1","output_index":0,"content_index":0,"delta":"x"}"#.to_owned(),
    };
    assert_eq!(decoder.feed(&orphan).unwrap_err().field, "delta");
}

/// The provider's reasoning count is read from `output_tokens_details` and
/// written back the same way; with none, the object is not written.
#[test]
fn a_reasoning_count_is_carried_both_ways() {
    let mut response = tool_call_response();
    response.usage.reasoning = Some(240);
    let wire = encode_response(&response);
    let json: serde_json::Value = serde_json::from_slice(&wire).unwrap();
    assert_eq!(
        json["usage"]["output_tokens_details"]["reasoning_tokens"],
        240
    );
    assert_eq!(decode_response(&wire).unwrap().usage.reasoning, Some(240));
    response.usage.reasoning = None;
    let json: serde_json::Value = serde_json::from_slice(&encode_response(&response)).unwrap();
    assert!(
        json["usage"].get("output_tokens_details").is_none(),
        "{json}"
    );
}

/// A streamed answer's reasoning count, stated in its final delta, reaches
/// the Responses wire's final snapshot.
#[test]
fn a_streamed_reasoning_count_reaches_the_final_snapshot() {
    let mut response = tool_call_response();
    response.usage.reasoning = Some(240);
    let mut encoder = EventEncoder::default();
    let wire: Vec<u8> = response
        .as_events()
        .iter()
        .flat_map(|event| encoder.encode(event))
        .collect();
    let text = String::from_utf8_lossy(&wire);
    let completed = text
        .split("\n\n")
        .find(|frame| frame.contains("event: response.completed"))
        .unwrap_or_else(|| panic!("{text}"));
    assert!(
        completed.contains(r#""output_tokens_details":{"reasoning_tokens":240}"#),
        "{completed}"
    );
}
