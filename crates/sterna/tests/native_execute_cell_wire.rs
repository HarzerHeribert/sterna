use sterna::contract::{Block, Conversation, Message, Role};
use sterna::wire::{StreamAccumulator, request_body};

#[test]
fn normal_requests_advertise_one_native_cell_tool_and_keep_large_code_intact() {
    let code = format!(
        "const source = {:?}; return source.length;",
        "x".repeat(96_000)
    );
    let conversation = Conversation {
        system: "system".into(),
        messages: vec![{
            let mut message = Message::text(Role::Assistant, "");
            message.content = vec![Block::ToolUse {
                id: "cell-call-42".into(),
                name: "execute_cell".into(),
                input: serde_json::json!({"code": code}),
            }];
            message
        }],
    };
    let body: serde_json::Value = serde_json::from_slice(&request_body(&conversation)).unwrap();
    assert_eq!(body["tools"].as_array().unwrap().len(), 1);
    assert_eq!(body["tools"][0]["name"], "execute_cell");
    assert_eq!(
        body["tools"][0]["input_schema"]["required"],
        serde_json::json!(["code", "description"])
    );
    assert_eq!(body["messages"][0]["content"][0]["input"]["code"], code);
}

#[test]
fn a_truncated_streamed_tool_call_is_never_returned_as_executable_input() {
    let mut stream = StreamAccumulator::new();
    stream.event(r#"{"type":"content_block_start","index":2,"content_block":{"type":"tool_use","id":"call-x","name":"execute_cell","input":{}}}"#).unwrap();
    stream.event(r#"{"type":"content_block_delta","index":2,"delta":{"type":"input_json_delta","partial_json":"{\"code\":\"await read("}}"#).unwrap();
    stream.event(r#"{"type":"message_stop"}"#).unwrap();
    assert!(stream.finish().is_err());
}
