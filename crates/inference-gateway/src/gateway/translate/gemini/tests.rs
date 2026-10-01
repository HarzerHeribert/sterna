use super::*;

use crate::gateway::translate::canonical;

fn decode(body: &str) -> Request {
    decode_request(body.as_bytes()).expect("a well-formed Gemini request")
}

fn encoded(request: &Request) -> Value {
    serde_json::from_slice(&encode_request(request)).expect("the codec writes JSON")
}

/// A canonical request after one tool round, the shape every supported
/// pair hands this codec.
fn tool_round() -> Request {
    Request {
        model: "gemini-2.5-pro".to_owned(),
        max_tokens: Some(4096),
        system: Some("You are terse.".to_owned()),
        messages: vec![
            Message {
                role: Role::User,
                blocks: vec![Block::Text("List the files.".to_owned())],
            },
            Message {
                role: Role::Assistant,
                blocks: vec![
                    Block::Text("Sure.".to_owned()),
                    Block::ToolUse {
                        id: "call_prior_1".to_owned(),
                        name: "Bash".to_owned(),
                        input: json!({"command": "ls /nope"}),
                    },
                ],
            },
            Message {
                role: Role::User,
                blocks: vec![
                    Block::ToolResult {
                        tool_use_id: "call_prior_1".to_owned(),
                        content: "ls: cannot access '/nope'".to_owned(),
                        is_error: true,
                    },
                    Block::Text("Try again.".to_owned()),
                ],
            },
        ],
        tools: vec![ToolDefinition {
            name: "Bash".to_owned(),
            description: Some("Run a shell command".to_owned()),
            input_schema: json!({"type": "object", "properties": {"command": {"type": "string"}}}),
        }],
        tool_choice: Some(ToolChoice::Auto),
        parallel_tool_calls: None,
        temperature: None,
        top_p: None,
        stop: Vec::new(),
        stream: false,
        user: Some("user_abc".to_owned()),
        cache_requested: false,
        effort: None,
    }
}

#[test]
fn effort_is_carried_as_thinking_budget_clamped_and_omitted_when_the_harness_asked_for_none() {
    let mut request = tool_round();
    request.effort = None;
    let sent = encoded(&request);
    assert_eq!(
        sent.get("generationConfig")
            .and_then(|g| g.get("thinkingConfig")),
        None,
        "no thinking asked for, no thinkingConfig emitted"
    );

    // A budget within range is carried unchanged.
    request.effort = Some(EffortRequest {
        budget_tokens: Some(4_096),
        level: None,
    });
    let sent = encoded(&request);
    assert_eq!(
        sent["generationConfig"]["thinkingConfig"]["thinkingBudget"],
        4_096
    );

    // A budget above the documented ceiling is clamped down, never
    // raised.
    request.effort = Some(EffortRequest {
        budget_tokens: Some(1_000_000),
        level: None,
    });
    let sent = encoded(&request);
    assert_eq!(
        sent["generationConfig"]["thinkingConfig"]["thinkingBudget"],
        GEMINI_THINKING_BUDGET_MAX
    );
}

/// The module doc's second decision: a tool result becomes a
/// `functionResponse` under the NAME of the call it answers, resolved
/// through the tool-use block carrying its id.
#[test]
fn a_tool_result_is_matched_to_its_call_by_the_name_the_id_resolves_to() {
    let request = tool_round();
    assert_eq!(Gemini.refuse_unencodable(&request), Ok(()));
    let sent = encoded(&request);
    let contents = sent["contents"].as_array().expect("contents");
    assert_eq!(
        contents[0],
        json!({"role": "user", "parts": [{"text": "List the files."}]})
    );
    assert_eq!(contents[1]["role"], "model");
    assert_eq!(
        contents[1]["parts"],
        json!([
            {"text": "Sure."},
            {"functionCall": {"name": "Bash", "args": {"command": "ls /nope"}}},
        ])
    );
    assert_eq!(contents[2]["role"], "user");
    assert_eq!(
        contents[2]["parts"][0],
        json!({"functionResponse": {
            "name": "Bash",
            "response": {"error": "ls: cannot access '/nope'"},
        }}),
        "the result names the call's function, and an erroring result says so"
    );
    assert_eq!(contents[2]["parts"][1], json!({"text": "Try again."}));
    assert_eq!(
        sent["systemInstruction"],
        json!({"parts": [{"text": "You are terse."}]})
    );
    assert_eq!(sent["generationConfig"], json!({"maxOutputTokens": 4096}));
    assert_eq!(
        sent["toolConfig"],
        json!({"functionCallingConfig": {"mode": "AUTO"}})
    );
    assert_eq!(
        sent["tools"],
        json!([{"functionDeclarations": [{
            "name": "Bash",
            "description": "Run a shell command",
            "parameters": {"type": "object", "properties": {"command": {"type": "string"}}},
        }]}])
    );
    // The one dropped request field, dropped by name and listed.
    assert_eq!(sent.get("user"), None);
    assert!(IGNORED_FIELDS.contains(&"user"));
}

/// A result whose id names no call in the same request is refused by
/// name, before anything is opened upstream — never written under some
/// other tool's name.
#[test]
fn a_tool_result_whose_id_names_no_call_is_refused_rather_than_guessed() {
    let mut request = tool_round();
    request.messages[1].blocks.truncate(1);
    let refusal = Gemini
        .refuse_unencodable(&request)
        .expect_err("the call this result answers is gone");
    assert_eq!(refusal.field, "tool_use_id");
    assert!(refusal.reason.contains("by NAME"), "{}", refusal.reason);
}

/// An Anthropic thinking block is another provider's private reasoning —
/// the same category this codec already refuses in the other direction as
/// a `thought` part (`REFUSED_FIELDS`) — so it is refused, not silently
/// dropped, before anything is opened upstream.
#[test]
fn a_thinking_block_is_refused_rather_than_dropped() {
    let mut request = tool_round();
    request.messages.push(Message {
        role: Role::Assistant,
        blocks: vec![Block::Thinking {
            thinking: "reasoning the harness never sees".to_owned(),
            signature: "sig".to_owned(),
        }],
    });
    let refusal = Gemini
        .refuse_unencodable(&request)
        .expect_err("a thinking block has no Gemini equivalent");
    assert_eq!(refusal.field, "thinking block");
    assert!(!refusal.reason.contains("reasoning the harness never sees"));

    let mut request = tool_round();
    request.messages.push(Message {
        role: Role::Assistant,
        blocks: vec![Block::RedactedThinking {
            data: "opaque".to_owned(),
        }],
    });
    Gemini
        .refuse_unencodable(&request)
        .expect_err("a redacted thinking block has no Gemini equivalent either");
}

/// The model is a path segment on this wire, so a name that could not
/// be one is refused rather than smuggled into the request line — and
/// Gemini's own `models/<id>` spelling is not doubled.
#[test]
fn the_model_addresses_the_path_and_a_name_a_path_cannot_carry_is_refused() {
    let mut request = tool_round();
    assert_eq!(
        Gemini.outbound_endpoint(&request),
        "/v1beta/models/gemini-2.5-pro:generateContent",
        "the version segment is the codec's, not the base URL's, or a relayed target \
         composes it twice"
    );
    request.stream = true;
    assert_eq!(
        Gemini.outbound_endpoint(&request),
        "/v1beta/models/gemini-2.5-pro:streamGenerateContent?alt=sse",
        "without alt=sse Google answers a JSON array rather than server-sent events"
    );
    request.model = "models/gemini-2.5-flash".to_owned();
    assert_eq!(
        Gemini.outbound_endpoint(&request),
        "/v1beta/models/gemini-2.5-flash:streamGenerateContent?alt=sse"
    );
    for bad in ["a/b", "a?b", "a#b", "a b", ""] {
        let mut request = tool_round();
        request.model = bad.to_owned();
        let refusal = Gemini
            .refuse_unencodable(&request)
            .expect_err("a model a path segment cannot carry");
        assert_eq!(refusal.field, "model", "for {bad:?}");
    }
}

/// The module doc's third decision, which is the one that keeps a
/// harness's tooling working: `STOP` beside a function call is
/// `tool_use`, not `end_turn`.
#[test]
fn a_candidate_of_function_calls_stops_for_tool_use_even_though_gemini_says_stop() {
    let body = json!({
        "candidates": [{
            "content": {"role": "model", "parts": [
                {"text": "Checking."},
                {"functionCall": {"name": "Bash", "args": {"command": "ls"}}},
            ]},
            "finishReason": "STOP",
            "index": 0,
        }],
        "usageMetadata": {
            "promptTokenCount": 40,
            "candidatesTokenCount": 10,
            "thoughtsTokenCount": 2,
            "cachedContentTokenCount": 8,
            "totalTokenCount": 52,
        },
        "modelVersion": "gemini-2.5-pro-001",
        "responseId": "resp-fixture",
    })
    .to_string();
    let response = decode_response(body.as_bytes()).expect("a well-formed answer");
    assert_eq!(response.stop_reason, StopReason::ToolUse);
    assert_eq!(response.id, "resp-fixture");
    assert_eq!(response.model, "gemini-2.5-pro-001");
    assert_eq!(
        response.blocks[1],
        Block::ToolUse {
            id: "gemini-call-1-Bash".to_owned(),
            name: "Bash".to_owned(),
            input: json!({"command": "ls"}),
        }
    );
    assert_eq!(
        response.usage,
        Usage {
            input: 32,
            output: 12,
            cached: Some(8),
            reasoning: Some(2),
        },
        "the prompt count includes the cached tokens and the output count includes the \
         reasoning ones"
    );

    // ... and the same reason with no call is an ordinary end of turn.
    let plain = json!({
        "candidates": [{
            "content": {"role": "model", "parts": [{"text": "Done."}]},
            "finishReason": "STOP",
        }],
    })
    .to_string();
    assert_eq!(
        decode_response(plain.as_bytes()).unwrap().stop_reason,
        StopReason::EndTurn
    );
}

/// A finish reason the canonical vocabulary has no honest word for is
/// refused by name rather than reported as a normal end of turn.
#[test]
fn a_finish_reason_with_no_honest_canonical_word_is_refused_by_name() {
    for finish in ["MALFORMED_FUNCTION_CALL", "OTHER", "UNEXPECTED_TOOL_CALL"] {
        let body = json!({
            "candidates": [{
                "content": {"role": "model", "parts": [{"text": ""}]},
                "finishReason": finish,
            }],
        })
        .to_string();
        let refusal =
            decode_response(body.as_bytes()).expect_err("no canonical stop reason describes this");
        assert_eq!(refusal.field, "candidates[0].finishReason");
        assert!(refusal.reason.contains(finish), "{}", refusal.reason);
    }
    for (finish, expected) in [
        ("MAX_TOKENS", StopReason::MaxTokens),
        ("SAFETY", StopReason::Refusal),
        ("PROHIBITED_CONTENT", StopReason::Refusal),
    ] {
        let body = json!({
            "candidates": [{
                "content": {"role": "model", "parts": [{"text": "x"}]},
                "finishReason": finish,
            }],
        })
        .to_string();
        assert_eq!(
            decode_response(body.as_bytes()).unwrap().stop_reason,
            expected
        );
    }
}

/// Nothing is dropped silently: a field this codec does not carry is
/// refused by its own path.
#[test]
fn an_unknown_or_unsupported_field_is_refused_by_its_full_path() {
    for (body, field) in [
        (
            json!({"contents": [], "safetySettings": [{"category": "HARM_CATEGORY_HARASSMENT"}]}),
            "safetySettings",
        ),
        (
            json!({"contents": [], "cachedContent": "x"}),
            "cachedContent",
        ),
        (
            json!({"contents": [], "generationConfig": {"topK": 4}}),
            "generationConfig.topK",
        ),
        (
            json!({"contents": [], "generationConfig": {"thinkingConfig": {"thinkingBudget": 1}}}),
            "generationConfig.thinkingConfig",
        ),
        (
            json!({"contents": [], "someFutureField": 1}),
            "someFutureField",
        ),
        (
            json!({"contents": [{"role": "user", "parts": [{"videoMetadata": {"fps": 1}}]}]}),
            "contents[0].parts[0].videoMetadata",
        ),
        // A part that reads as nothing at all: the one silent drop this
        // codec could have had, refused by the part's own path.
        (
            json!({"contents": [{"role": "user", "parts": [{}]}]}),
            "contents[0].parts[0]",
        ),
        (
            json!({"contents": [{"role": "user", "parts": [{"fileData": {"fileUri": "x"}}]}]}),
            "contents[0].parts[0].fileData",
        ),
        (
            json!({"contents": [{"role": "system", "parts": [{"text": "x"}]}]}),
            "contents[0].role",
        ),
    ] {
        let refusal = decode_request(body.to_string().as_bytes())
            .expect_err("this field has no home in the canonical form");
        assert_eq!(refusal.field, field);
    }
}

/// The two halves agree: a request written by this codec decodes back to
/// what produced it, tool round included.
#[test]
fn a_request_written_by_this_codec_decodes_back_to_the_same_form() {
    let request = tool_round();
    let bytes = encode_request(&request);
    let back = decode_request(&bytes).expect("its own output");
    assert_eq!(back.system, request.system);
    assert_eq!(back.tools, request.tools);
    assert_eq!(back.tool_choice, request.tool_choice);
    assert_eq!(back.max_tokens, request.max_tokens);
    assert_eq!(back.messages[0], request.messages[0]);
    assert_eq!(back.messages[1], request.messages[1].clone_with_minted_id());
    // The tool result comes back matched to the call by name, under the
    // id this codec mints for a call of that name at that position.
    let Block::ToolResult {
        content, is_error, ..
    } = &back.messages[2].blocks[0]
    else {
        panic!("a tool result, got {:?}", back.messages[2].blocks[0]);
    };
    assert_eq!(content, "ls: cannot access '/nope'");
    assert!(is_error, "the error flag survives the round trip");
}

/// A forced tool choice round-trips through the one shape this wire has
/// for it.
#[test]
fn a_forced_tool_choice_is_any_plus_one_allowed_name() {
    let mut request = tool_round();
    request.tool_choice = Some(ToolChoice::Tool("Bash".to_owned()));
    let sent = encoded(&request);
    assert_eq!(
        sent["toolConfig"],
        json!({"functionCallingConfig": {"mode": "ANY", "allowedFunctionNames": ["Bash"]}})
    );
    let back = decode(&String::from_utf8(encode_request(&request)).unwrap());
    assert_eq!(back.tool_choice, Some(ToolChoice::Tool("Bash".to_owned())));

    request.tool_choice = Some(ToolChoice::Any);
    assert_eq!(
        encoded(&request)["toolConfig"],
        json!({"functionCallingConfig": {"mode": "ANY"}})
    );
    request.tool_choice = Some(ToolChoice::None);
    assert_eq!(
        encoded(&request)["toolConfig"],
        json!({"functionCallingConfig": {"mode": "NONE"}})
    );
}

/// Gemini has no parameter that disables parallel function calling, so a
/// request that asked for it is refused rather than answered as though
/// it had not asked.
#[test]
fn a_request_that_disabled_parallel_tool_calls_is_refused_by_name() {
    let mut request = tool_round();
    request.parallel_tool_calls = Some(false);
    let refusal = Gemini
        .refuse_unencodable(&request)
        .expect_err("this wire cannot honour it");
    assert_eq!(refusal.field, "parallel_tool_calls");
    request.parallel_tool_calls = Some(true);
    assert_eq!(
        Gemini.refuse_unencodable(&request),
        Ok(()),
        "parallel calling is this wire's own default, so asking for it asks for nothing"
    );
}

fn sse(data: &Value) -> SseEvent {
    SseEvent {
        event: None,
        data: data.to_string(),
    }
}

/// The stream table in the module doc, as behaviour: the first chunk
/// starts the message, a text chunk delivers its fragment, a function
/// call arrives whole, and the message's own delta waits for the end.
#[test]
fn a_stream_becomes_the_canonical_order_one_chunk_at_a_time() {
    let mut decoder = ChunkDecoder::default();
    let first = decoder
        .feed(&sse(&json!({
            "candidates": [{"content": {"role": "model", "parts": [{"text": "Check"}]}}],
            "modelVersion": "gemini-2.5-pro",
            "responseId": "resp-1",
        })))
        .expect("a well-formed chunk");
    assert_eq!(
        first,
        vec![
            StreamEvent::MessageStart {
                id: "resp-1".to_owned(),
                model: "gemini-2.5-pro".to_owned(),
                usage: Usage::default(),
            },
            StreamEvent::BlockStart {
                index: 0,
                block: BlockStart::Text,
            },
            StreamEvent::BlockDelta {
                index: 0,
                delta: Delta::Text("Check".to_owned()),
            },
        ]
    );
    let second = decoder
        .feed(&sse(&json!({
            "candidates": [{"content": {"role": "model", "parts": [{"text": "ing."}]}}],
        })))
        .expect("a well-formed chunk");
    assert_eq!(
        second,
        vec![StreamEvent::BlockDelta {
            index: 0,
            delta: Delta::Text("ing.".to_owned()),
        }],
        "a fragment leaves on the chunk that carried it, with no new block"
    );
    let third = decoder
        .feed(&sse(&json!({
            "candidates": [{
                "content": {"role": "model", "parts": [
                    {"functionCall": {"name": "Bash", "args": {"command": "ls"}}},
                ]},
                "finishReason": "STOP",
            }],
            "usageMetadata": {"promptTokenCount": 40, "candidatesTokenCount": 12},
        })))
        .expect("a well-formed chunk");
    assert_eq!(
        third,
        vec![
            StreamEvent::BlockStop { index: 0 },
            StreamEvent::BlockStart {
                index: 1,
                block: BlockStart::ToolUse {
                    id: "gemini-call-0-Bash".to_owned(),
                    name: "Bash".to_owned(),
                },
            },
            StreamEvent::BlockDelta {
                index: 1,
                delta: Delta::InputJson("{\"command\":\"ls\"}".to_owned()),
            },
        ],
        "the finish reason is held for the message's own delta"
    );
    assert!(!decoder.is_done());
    let end = decoder.finish().expect("the stream finished");
    assert_eq!(
        end,
        vec![
            StreamEvent::BlockStop { index: 1 },
            StreamEvent::MessageDelta {
                stop_reason: StopReason::ToolUse,
                stop_sequence: None,
                usage: Usage {
                    input: 40,
                    output: 12,
                    cached: None,
                    reasoning: None,
                },
            },
            StreamEvent::MessageStop,
        ]
    );
    assert!(decoder.is_done());

    // ... and the whole sequence is one the canonical order accepts and
    // accumulates into the answer it delivered.
    let mut order = canonical::Order::default();
    let all: Vec<StreamEvent> = first
        .into_iter()
        .chain(second)
        .chain(third)
        .chain(end)
        .collect();
    for event in &all {
        order.check(event).expect("a well-ordered stream");
    }
    let response = canonical::accumulate(&all).expect("one message");
    assert_eq!(response.blocks[0], Block::Text("Checking.".to_owned()));
    assert_eq!(response.stop_reason, StopReason::ToolUse);
}

/// The module doc's fifth decision: with no `[DONE]` on this wire, a
/// stream that ends without a finish reason is a truncated message and
/// is refused rather than completed.
#[test]
fn a_stream_that_ends_without_a_finish_reason_is_refused_as_truncated() {
    let mut decoder = ChunkDecoder::default();
    decoder
        .feed(&sse(&json!({
            "candidates": [{"content": {"role": "model", "parts": [{"text": "half"}]}}],
        })))
        .expect("a well-formed chunk");
    let refusal = decoder.finish().expect_err("the stream was cut");
    assert_eq!(refusal.field, "candidates[0].finishReason");
    assert!(refusal.reason.contains("truncated"), "{}", refusal.reason);

    let mut empty = ChunkDecoder::default();
    assert_eq!(
        empty.finish().expect_err("nothing arrived at all").field,
        "chunk"
    );
}

/// The encoder's one wire-imposed property: a tool call's fragments are
/// held until the block stops, because Gemini has no partial-arguments
/// event.
#[test]
fn the_encoder_writes_a_function_call_whole_because_this_wire_has_no_fragment() {
    let mut encoder = ChunkEncoder::default();
    assert!(
        encoder
            .encode(&StreamEvent::MessageStart {
                id: "resp-1".to_owned(),
                model: "gemini-2.5-pro".to_owned(),
                usage: Usage::default(),
            })
            .is_empty()
    );
    assert!(
        encoder
            .encode(&StreamEvent::BlockStart {
                index: 0,
                block: BlockStart::ToolUse {
                    id: "call_A".to_owned(),
                    name: "Bash".to_owned(),
                },
            })
            .is_empty()
    );
    assert!(
        encoder
            .encode(&StreamEvent::BlockDelta {
                index: 0,
                delta: Delta::InputJson("{\"command\": ".to_owned()),
            })
            .is_empty(),
        "a fragment has no shape on this wire and is held"
    );
    assert!(
        encoder
            .encode(&StreamEvent::BlockDelta {
                index: 0,
                delta: Delta::InputJson("\"ls\"}".to_owned()),
            })
            .is_empty()
    );
    let written = encoder.encode(&StreamEvent::BlockStop { index: 0 });
    let text = String::from_utf8(written).expect("UTF-8");
    let data = text
        .trim()
        .strip_prefix("data: ")
        .expect("one framed event");
    let chunk: Value = serde_json::from_str(data).expect("JSON");
    assert_eq!(
        chunk["candidates"][0]["content"]["parts"][0],
        json!({"functionCall": {"name": "Bash", "args": {"command": "ls"}}}),
        "the whole call is written once its arguments are complete"
    );
}

/// The claim rule: this codec owns `…/models/<model>:generateContent`
/// under any of Google's version segments, refuses its siblings by name,
/// and does not claim a model listing — which the gateway has always
/// answered with its plain `404`.
#[test]
fn the_claim_covers_generate_content_under_googles_own_version_segments() {
    for path in [
        "/v1beta/models/gemini-2.5-pro:generateContent",
        "/v1/models/gemini-2.5-pro:generateContent",
        "/v1alpha/models/gemini-2.5-pro:streamGenerateContent",
        "/models/gemini-2.5-pro:streamGenerateContent",
    ] {
        assert!(
            matches!(Gemini.claim(path), Claim::Endpoint),
            "{path} must be this codec's endpoint"
        );
    }
    for path in [
        "/v1beta/models/gemini-2.5-pro:countTokens",
        "/models/gemini-2.5-pro:embedContent",
    ] {
        assert!(
            matches!(Gemini.claim(path), Claim::Other),
            "{path} is this protocol's own surface and not translated"
        );
    }
    for path in [
        "/models",
        "/v1beta/models",
        "/models/gemini-2.5-pro",
        "/chat/completions",
        "/v1/messages",
        "/models/a/b:generateContent",
    ] {
        assert!(
            matches!(Gemini.claim(path), Claim::None),
            "{path} does not belong to this codec"
        );
    }
}

/// A Gemini error document, in both the shapes Google writes it in.
#[test]
fn a_provider_error_message_is_read_out_of_either_shape_google_writes() {
    let object =
        json!({"error": {"code": 429, "message": "quota", "status": "RESOURCE_EXHAUSTED"}});
    assert_eq!(
        Gemini.decode_error(object.to_string().as_bytes()),
        Some("quota".to_owned())
    );
    let array = json!([{"error": {"code": 400, "message": "bad", "status": "INVALID_ARGUMENT"}}]);
    assert_eq!(
        Gemini.decode_error(array.to_string().as_bytes()),
        Some("bad".to_owned())
    );
    let written: Value =
        serde_json::from_slice(&Gemini.encode_error("INVALID_ARGUMENT", "no")).unwrap();
    assert_eq!(written["error"]["status"], "INVALID_ARGUMENT");
    assert_eq!(
        written["error"]["code"],
        Value::Null,
        "a refusal written here is the gateway's, not Google's, and carries no Google code"
    );
}

/// Every refusal this file raises names a reason from its own table, and
/// every table entry is reachable through `field_rows`.
#[test]
fn every_refused_field_carries_a_reason_and_no_entry_is_a_duplicate() {
    let mut names: Vec<&str> = REFUSED_FIELDS.iter().map(|(name, _)| *name).collect();
    names.sort_unstable();
    let mut unique = names.clone();
    unique.dedup();
    assert_eq!(names, unique, "a field is refused for exactly one reason");
    for (field, why) in REFUSED_FIELDS {
        assert!(!why.is_empty(), "{field} has no reason");
    }
}

impl Message {
    /// The same message with each tool-use id replaced by the one this
    /// codec mints for a call of that name at that position — what a
    /// round trip through the wire produces, since Gemini issues no id.
    fn clone_with_minted_id(&self) -> Message {
        Message {
            role: self.role,
            blocks: self
                .blocks
                .iter()
                .enumerate()
                .map(|(index, block)| match block {
                    Block::ToolUse { name, input, .. } => Block::ToolUse {
                        id: minted_id(index, name),
                        name: name.clone(),
                        input: input.clone(),
                    },
                    other => other.clone(),
                })
                .collect(),
        }
    }
}
