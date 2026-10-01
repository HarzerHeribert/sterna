//! Phase 56, lines 1948–1950 and 1956: the two T2b pairs — an OpenCode-shaped
//! (`openai-chat`) client served by an OpenAI-Responses entitlement, and a
//! Codex-shaped (`openai-responses`) client served by an OpenAI-Chat
//! entitlement — each end to end against a fixture upstream that speaks
//! **only** the provider's protocol and records what reached it. Also the
//! outbound `anthropic-version` header hook (T2 finding 2), driven through
//! T2's own already-supported pair 2 (`openai-responses -> anthropic-messages`)
//! since neither T2b pair ever targets an Anthropic-serving provider.
//!
//! # Where these tests enter
//!
//! Exactly where the sibling files enter: `gateway::start_if_required_with_degrade_sink`
//! — the door the shipped binary calls, real accept loop, real sockets — with
//! an [`Upstream`] built by the **production** `profile::gateway_upstream`
//! from a real provider template narrowed to the one protocol the fixture
//! speaks. The `glasshouse launch` link stays blocked at
//! `profile::apply_gateway`; this file does not re-prove that.
//!
//! # What is asserted, and against what
//!
//! Every claim about the outbound request is made against bytes the fixture
//! read off the wire with its own parser; every claim about the inbound
//! answer against the bytes a plain `TcpStream` client received. Ids are
//! compared as strings on both sides — a chat `tool_calls[].id` *is* the
//! Responses `call_id`, and a wrong one runs the wrong tool.

mod host;

use std::io::{Read, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use inference_gateway::gateway::translate::TOOL_ERROR_MARKER;
use inference_gateway::gateway::{Gateway, Upstream};
use inference_gateway::provider::Provider;
use inference_gateway::secret::EnvironmentSecretStore;
use serde_json::{Value, json};

/// A planted provider credential, unique to this test binary. Never a real
/// key, and asserted on so that `!contains` has something to bite.
const PLANTED_KEY: &str = "sk-planted-translate-t2b-000111222333";
const CREDENTIAL_VAR: &str = "GLASSHOUSE_GATEWAY_TRANSLATE_T2B_TEST_KEY";

/// A body that a request header cannot legitimately carry and that no test
/// wants to see anywhere but in the one place it was planted.
const PLANTED_PROMPT: &str = "PLANTED-PROMPT-TEXT-FOR-T2B-TESTS";

const CLIENT_TIMEOUT: Duration = Duration::from_secs(20);

// --- a fixture that speaks exactly one protocol --------------------------------

#[derive(Debug, Clone)]
struct RecordedRequest {
    method: String,
    target: String,
    /// Names lower-cased; values as received.
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl RecordedRequest {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(header, _)| header == name)
            .map(|(_, value)| value.as_str())
    }

    fn json(&self) -> Value {
        serde_json::from_slice(&self.body).expect("the fixture received a JSON body")
    }
}

/// How the fixture answers its own protocol's endpoint.
#[derive(Clone, Copy)]
enum Answer {
    /// One OpenAI Chat completion document: a text part and two tool calls.
    ChatCompletion,
    /// The same as an OpenAI Chat streamed set of chunks, pausing mid-argument
    /// until [`Fixture::release`] is called.
    ChatGatedStream,
    /// One OpenAI Responses document: a text part and two function calls.
    ResponsesCompletion,
    /// The same as a Responses event stream, pausing mid-arguments until
    /// [`Fixture::release`] is called.
    ResponsesGatedStream,
    /// One Anthropic Messages document — used only to drive the outbound
    /// `anthropic-version` header hook through T2's own pair 2.
    AnthropicCompletion,
}

/// A canned provider: its own endpoint is answered in its protocol's shape,
/// every request is recorded, and any other target gets a marker document.
struct Fixture {
    address: SocketAddr,
    requests: Arc<Mutex<Vec<RecordedRequest>>>,
    gate: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
    accept: Option<JoinHandle<()>>,
}

impl Fixture {
    fn start(answer: Answer) -> Self {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("loopback is bindable");
        let address = listener.local_addr().expect("bound");
        listener.set_nonblocking(true).expect("polling mode");
        let requests = Arc::new(Mutex::new(Vec::new()));
        let gate = Arc::new(AtomicBool::new(false));
        let stop = Arc::new(AtomicBool::new(false));
        let accept = std::thread::spawn({
            let requests = Arc::clone(&requests);
            let gate = Arc::clone(&gate);
            let stop = Arc::clone(&stop);
            move || {
                while !stop.load(Ordering::Relaxed) {
                    match listener.accept() {
                        Ok((stream, _)) => {
                            let requests = Arc::clone(&requests);
                            let gate = Arc::clone(&gate);
                            std::thread::spawn(move || serve(stream, &requests, answer, &gate));
                        }
                        Err(_) => std::thread::sleep(Duration::from_millis(10)),
                    }
                }
            }
        });
        Self {
            address,
            requests,
            gate,
            stop,
            accept: Some(accept),
        }
    }

    /// The base URL an OpenAI-shaped provider declares: with `/v1`, so a
    /// Responses client's `/responses` lands on `/v1/responses`, and a Chat
    /// client's `/chat/completions` lands on `/v1/chat/completions`.
    fn base_url(&self) -> String {
        format!("http://{}/v1", self.address)
    }

    /// The base URL an Anthropic-serving provider declares: the root, with
    /// no `/v1` — its native client appends `/v1/messages` itself.
    fn root_url(&self) -> String {
        format!("http://{}", self.address)
    }

    fn only_request(&self) -> RecordedRequest {
        let requests = self.requests.lock().unwrap().clone();
        assert_eq!(requests.len(), 1, "exactly one request at the fixture");
        requests.into_iter().next().unwrap()
    }

    /// Let a gated stream finish.
    fn release(&self) {
        self.gate.store(true, Ordering::Relaxed);
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        self.gate.store(true, Ordering::Relaxed);
        if let Some(accept) = self.accept.take() {
            let _ = accept.join();
        }
    }
}

fn serve(
    mut stream: TcpStream,
    requests: &Mutex<Vec<RecordedRequest>>,
    answer: Answer,
    gate: &AtomicBool,
) {
    stream.set_nonblocking(false).expect("blocking mode");
    let _ = stream.set_read_timeout(Some(Duration::from_secs(60)));
    let _ = stream.set_nodelay(true);
    let Some(request) = read_request(&mut stream) else {
        return;
    };
    requests.lock().unwrap().push(request.clone());
    let endpoint = match answer {
        Answer::ChatCompletion | Answer::ChatGatedStream => "/v1/chat/completions",
        Answer::ResponsesCompletion | Answer::ResponsesGatedStream => "/v1/responses",
        Answer::AnthropicCompletion => "/v1/messages",
    };
    if !request.target.starts_with(endpoint) {
        let body = r#"{"id":"unexpected","planted_marker_the_codecs_refuse":true}"#;
        write_document(&mut stream, "200 OK", body);
        return;
    }
    match answer {
        Answer::ChatCompletion => write_document(
            &mut stream,
            "200 OK",
            &json!({
                "id": "chatcmpl_fix",
                "object": "chat.completion",
                "created": 1,
                "model": "fixture-model",
                "choices": [{
                    "index": 0,
                    "message": {
                        "role": "assistant",
                        "content": "Checking.",
                        "tool_calls": [
                            {"id": "call_fix_A", "type": "function", "function": {"name": "Bash", "arguments": "{\"command\": \"ls\"}"}},
                            {"id": "call_fix_B", "type": "function", "function": {"name": "Read", "arguments": "{\"file_path\": \"/tmp/x\"}"}}
                        ]
                    },
                    "finish_reason": "tool_calls",
                    "logprobs": null
                }],
                "usage": {"prompt_tokens": 48, "completion_tokens": 12, "total_tokens": 60, "prompt_tokens_details": {"cached_tokens": 8}}
            })
            .to_string(),
        ),
        Answer::ResponsesCompletion => write_document(
            &mut stream,
            "200 OK",
            &json!({
                "id": "resp_fixture",
                "object": "response",
                "created_at": 1,
                "status": "completed",
                "error": null,
                "incomplete_details": null,
                "model": "fixture-model",
                "output": [
                    {"type": "message", "id": "msg_1", "status": "completed", "role": "assistant", "content": [{"type": "output_text", "text": "Checking.", "annotations": []}]},
                    {"type": "function_call", "id": "fc_1", "status": "completed", "call_id": "call_fix_A", "name": "Bash", "arguments": "{\"command\": \"ls\"}"},
                    {"type": "function_call", "id": "fc_2", "status": "completed", "call_id": "call_fix_B", "name": "Read", "arguments": "{\"file_path\": \"/tmp/x\"}"}
                ],
                "parallel_tool_calls": true,
                "tool_choice": "auto",
                "tools": [],
                "store": false,
                "usage": {"input_tokens": 48, "input_tokens_details": {"cached_tokens": 8}, "output_tokens": 12, "output_tokens_details": {"reasoning_tokens": 0}, "total_tokens": 60},
                "user": null,
                "metadata": {}
            })
            .to_string(),
        ),
        Answer::AnthropicCompletion => write_document(
            &mut stream,
            "200 OK",
            &json!({
                "id": "msg_fix",
                "type": "message",
                "role": "assistant",
                "model": "claude-x",
                "content": [
                    {"type": "text", "text": "Checking."},
                    {"type": "tool_use", "id": "toolu_fix_A", "name": "Bash", "input": {"command": "ls"}}
                ],
                "stop_reason": "tool_use",
                "stop_sequence": null,
                "usage": {"input_tokens": 30, "output_tokens": 9, "cache_read_input_tokens": 5}
            })
            .to_string(),
        ),
        Answer::ChatGatedStream => {
            let head = "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ntransfer-encoding: chunked\r\n\r\n";
            let _ = stream.write_all(head.as_bytes());
            let data = |value: Value| format!("data: {value}\n\n");
            let before = [
                data(json!({"id": "chatcmpl_fix", "object": "chat.completion.chunk", "created": 0, "model": "fixture-model", "choices": [{"index": 0, "delta": {"role": "assistant", "content": ""}, "finish_reason": null, "logprobs": null}]})),
                data(json!({"id": "chatcmpl_fix", "object": "chat.completion.chunk", "created": 0, "model": "fixture-model", "choices": [{"index": 0, "delta": {"content": "Checking."}, "finish_reason": null, "logprobs": null}]})),
                data(json!({"id": "chatcmpl_fix", "object": "chat.completion.chunk", "created": 0, "model": "fixture-model", "choices": [{"index": 0, "delta": {"tool_calls": [{"index": 0, "id": "call_fix_A", "type": "function", "function": {"name": "Bash", "arguments": ""}}]}, "finish_reason": null, "logprobs": null}]})),
                data(json!({"id": "chatcmpl_fix", "object": "chat.completion.chunk", "created": 0, "model": "fixture-model", "choices": [{"index": 0, "delta": {"tool_calls": [{"index": 0, "function": {"arguments": "{\"command\""}}]}, "finish_reason": null, "logprobs": null}]})),
            ];
            for chunk in before {
                write_chunk(&mut stream, chunk.as_bytes());
            }
            let deadline = Instant::now() + Duration::from_secs(20);
            while !gate.load(Ordering::Relaxed) && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(5));
            }
            let after = [
                data(json!({"id": "chatcmpl_fix", "object": "chat.completion.chunk", "created": 0, "model": "fixture-model", "choices": [{"index": 0, "delta": {"tool_calls": [{"index": 0, "function": {"arguments": ": \"ls\"}"}}]}, "finish_reason": null, "logprobs": null}]})),
                data(json!({"id": "chatcmpl_fix", "object": "chat.completion.chunk", "created": 0, "model": "fixture-model", "choices": [{"index": 0, "delta": {}, "finish_reason": "tool_calls", "logprobs": null}]})),
                data(json!({"id": "chatcmpl_fix", "object": "chat.completion.chunk", "created": 0, "model": "fixture-model", "choices": [], "usage": {"prompt_tokens": 48, "completion_tokens": 12, "total_tokens": 60, "prompt_tokens_details": {"cached_tokens": 8}}})),
            ];
            for chunk in after {
                write_chunk(&mut stream, chunk.as_bytes());
            }
            write_chunk(&mut stream, b"data: [DONE]\n\n");
            let _ = stream.write_all(b"0\r\n\r\n");
            let _ = stream.flush();
            let _ = stream.shutdown(std::net::Shutdown::Write);
        }
        Answer::ResponsesGatedStream => {
            let head = "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ntransfer-encoding: chunked\r\n\r\n";
            let _ = stream.write_all(head.as_bytes());
            let event = |name: &str, data: &str| format!("event: {name}\ndata: {data}\n\n");
            let before = [
                event("response.created", r#"{"type":"response.created","sequence_number":0,"response":{"id":"resp_fixture","object":"response","created_at":1,"status":"in_progress","model":"fixture-model","output":[],"usage":null}}"#),
                event("response.output_item.added", r#"{"type":"response.output_item.added","sequence_number":1,"output_index":0,"item":{"id":"msg_1","type":"message","status":"in_progress","role":"assistant","content":[]}}"#),
                event("response.content_part.added", r#"{"type":"response.content_part.added","sequence_number":2,"item_id":"msg_1","output_index":0,"content_index":0,"part":{"type":"output_text","text":"","annotations":[]}}"#),
                event("response.output_text.delta", r#"{"type":"response.output_text.delta","sequence_number":3,"item_id":"msg_1","output_index":0,"content_index":0,"delta":"Checking.","obfuscation":"aB"}"#),
                event("response.output_text.done", r#"{"type":"response.output_text.done","sequence_number":4,"item_id":"msg_1","output_index":0,"content_index":0,"text":"Checking."}"#),
                event("response.content_part.done", r#"{"type":"response.content_part.done","sequence_number":5,"item_id":"msg_1","output_index":0,"content_index":0,"part":{"type":"output_text","text":"Checking.","annotations":[]}}"#),
                event("response.output_item.done", r#"{"type":"response.output_item.done","sequence_number":6,"output_index":0,"item":{"id":"msg_1","type":"message","status":"completed","role":"assistant","content":[{"type":"output_text","text":"Checking.","annotations":[]}]}}"#),
                event("response.output_item.added", r#"{"type":"response.output_item.added","sequence_number":7,"output_index":1,"item":{"id":"fc_1","type":"function_call","status":"in_progress","call_id":"call_fix_A","name":"Bash","arguments":""}}"#),
                event("response.function_call_arguments.delta", r#"{"type":"response.function_call_arguments.delta","sequence_number":8,"item_id":"fc_1","output_index":1,"delta":"{\"command\""}"#),
            ];
            for chunk in before {
                write_chunk(&mut stream, chunk.as_bytes());
            }
            let deadline = Instant::now() + Duration::from_secs(20);
            while !gate.load(Ordering::Relaxed) && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(5));
            }
            let after = [
                event("response.function_call_arguments.delta", r#"{"type":"response.function_call_arguments.delta","sequence_number":9,"item_id":"fc_1","output_index":1,"delta":": \"ls\"}"}"#),
                event("response.function_call_arguments.done", r#"{"type":"response.function_call_arguments.done","sequence_number":10,"item_id":"fc_1","output_index":1,"name":"Bash","arguments":"{\"command\": \"ls\"}"}"#),
                event("response.output_item.done", r#"{"type":"response.output_item.done","sequence_number":11,"output_index":1,"item":{"id":"fc_1","type":"function_call","status":"completed","call_id":"call_fix_A","name":"Bash","arguments":"{\"command\": \"ls\"}"}}"#),
                event("response.completed", r#"{"type":"response.completed","sequence_number":12,"response":{"id":"resp_fixture","object":"response","created_at":1,"status":"completed","model":"fixture-model","output":[],"usage":{"input_tokens":48,"input_tokens_details":{"cached_tokens":8},"output_tokens":12,"total_tokens":60}}}"#),
            ];
            for chunk in after {
                write_chunk(&mut stream, chunk.as_bytes());
            }
            let _ = stream.write_all(b"0\r\n\r\n");
            let _ = stream.flush();
            let _ = stream.shutdown(std::net::Shutdown::Write);
        }
    }
}

fn write_document(stream: &mut TcpStream, status: &str, body: &str) {
    let _ = write!(
        stream,
        "HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\r\n{body}",
        body.len()
    );
    let _ = stream.flush();
    let _ = stream.shutdown(std::net::Shutdown::Write);
}

fn write_chunk(stream: &mut TcpStream, bytes: &[u8]) {
    let mut framed = format!("{:x}\r\n", bytes.len()).into_bytes();
    framed.extend_from_slice(bytes);
    framed.extend_from_slice(b"\r\n");
    let _ = stream.write_all(&framed);
    let _ = stream.flush();
}

/// Independent of anything under test: reads a request head byte by byte
/// and the body by its declared length.
fn read_request(stream: &mut TcpStream) -> Option<RecordedRequest> {
    let mut head = Vec::new();
    let mut byte = [0u8; 1];
    while !head.ends_with(b"\r\n\r\n") {
        match stream.read(&mut byte) {
            Ok(0) | Err(_) => return None,
            Ok(_) => head.push(byte[0]),
        }
        if head.len() > 64 * 1024 {
            return None;
        }
    }
    let text = String::from_utf8(head).ok()?;
    let mut lines = text.split("\r\n");
    let request_line = lines.next()?;
    let mut parts = request_line.split(' ');
    let method = parts.next()?.to_owned();
    let target = parts.next()?.to_owned();
    let mut headers = Vec::new();
    let mut content_length = 0usize;
    for line in lines {
        if line.is_empty() {
            continue;
        }
        let (name, value) = line.split_once(':')?;
        let name = name.trim().to_ascii_lowercase();
        let value = value.trim().to_owned();
        if name == "content-length" {
            content_length = value.parse().ok()?;
        }
        headers.push((name, value));
    }
    let mut body = vec![0u8; content_length];
    if content_length > 0 && stream.read_exact(&mut body).is_err() {
        return None;
    }
    Some(RecordedRequest {
        method,
        target,
        headers,
        body,
    })
}

// --- the gateway, built the way the binary builds it ---------------------------

/// The openrouter template narrowed to the one protocol the fixture speaks —
/// the same `Provider` value `config` would produce for a provider that
/// declares exactly that protocol, with the planted credential in this
/// file's own variable.
fn provider_serving_only(protocol_slug: &str, base_url: &str, name: &str) -> Provider {
    let mut provider = inference_gateway::provider::templates()
        .into_iter()
        .find(|provider| provider.name == "openrouter")
        .expect("the openrouter template exists");
    provider.name = name.to_owned();
    provider
        .protocols
        .retain(|support| support.protocol.slug() == protocol_slug);
    assert_eq!(
        provider.protocols.len(),
        1,
        "the openrouter template declares {protocol_slug} exactly once"
    );
    provider.protocols[0].base_url = base_url.to_owned();
    provider.credential_env = vec![CREDENTIAL_VAR.to_owned()];
    provider
}

/// Every set→resolve→remove of [`CREDENTIAL_VAR`] happens under this lock —
/// the tests in this binary run in parallel and the environment is process
/// state. See `gateway_translate.rs` for the measured failure this prevents.
static ENV_LOCK: Mutex<()> = Mutex::new(());

/// `profile::gateway_upstream`, the production builder, run with the planted
/// credential set only for the duration of the call.
fn upstream_from(provider: &Provider) -> Upstream {
    let _guard = ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    // SAFETY: `CREDENTIAL_VAR` is unique to this test binary, set and removed
    // around the one resolve that reads it, and every path that touches it
    // holds `ENV_LOCK` for the whole of that window.
    unsafe {
        std::env::set_var(CREDENTIAL_VAR, PLANTED_KEY);
    }
    let upstream = inference_gateway::pool::gateway_upstream(
        std::slice::from_ref(provider),
        &EnvironmentSecretStore::new(),
        &|_| false,
    );
    unsafe {
        std::env::remove_var(CREDENTIAL_VAR);
    }
    upstream.expect("one single-protocol provider with a resolvable credential builds an upstream")
}

/// The door the shipped binary calls, with the given upstream.
fn start_gateway(upstream: Upstream) -> Gateway {
    inference_gateway::gateway::start_if_required_with_degrade_sink(
        &[inference_gateway::gateway::BackendDemand::LocalGateway],
        || Ok(upstream),
        None,
        None,
        None,
        None,
    )
    .expect("loopback is bindable")
    .expect("a gateway-backed profile requires a gateway")
}

// --- the two clients -----------------------------------------------------------

/// The request an OpenCode-shaped (`openai-chat`) client sends after one tool
/// round: a system message, the prior assistant turn with its tool call, the
/// tool result carrying the error marker — which the pair restores as
/// Anthropic's `is_error` and Responses' output-marker convention on the far
/// side — and a follow-up.
fn opencode_body(stream: bool) -> String {
    json!({
        "model": "gpt-4o-mini",
        "messages": [
            {"role": "system", "content": PLANTED_PROMPT},
            {"role": "user", "content": "List the files."},
            {"role": "assistant", "content": "Sure.", "tool_calls": [
                {"id": "call_prior_1", "type": "function", "function": {"name": "Bash", "arguments": "{\"command\": \"ls /nope\"}"}}
            ]},
            {"role": "tool", "tool_call_id": "call_prior_1", "content": format!("{TOOL_ERROR_MARKER}\nls: cannot access '/nope'")},
            {"role": "user", "content": "Try again."}
        ],
        "tools": [{
            "type": "function",
            "function": {
                "name": "Bash",
                "description": "Run a shell command",
                "parameters": {"type": "object", "properties": {"command": {"type": "string"}}, "required": ["command"]}
            }
        }],
        "tool_choice": "auto",
        "max_tokens": 4096,
        "user": "user_abc",
        "stream": stream
    })
    .to_string()
}

/// The request a Codex-shaped (`openai-responses`) client sends after one
/// tool round — identical in shape to the sibling file's, because the
/// harness does not change when the provider's protocol does.
fn codex_body(stream: bool) -> String {
    json!({
        "model": "gpt-5",
        "instructions": PLANTED_PROMPT,
        "input": [
            {"type": "message", "role": "user", "content": [{"type": "input_text", "text": "List the files."}]},
            {"type": "message", "role": "assistant", "content": [{"type": "output_text", "text": "Sure."}]},
            {"type": "function_call", "call_id": "call_prior_9", "name": "Bash", "arguments": "{\"command\": \"ls /nope\"}"},
            {"type": "function_call_output", "call_id": "call_prior_9", "output": format!("{TOOL_ERROR_MARKER}\nls: cannot access '/nope'")},
            {"type": "message", "role": "user", "content": "Try again."}
        ],
        "tools": [{
            "type": "function",
            "name": "Bash",
            "description": "Run a shell command",
            "parameters": {"type": "object", "properties": {"command": {"type": "string"}}, "required": ["command"]},
            "strict": false
        }],
        "tool_choice": "auto",
        "parallel_tool_calls": true,
        "max_output_tokens": 4096,
        "store": false,
        "safety_identifier": "user_xyz",
        "stream": stream
    })
    .to_string()
}

fn chat_request(token: &str, body: &str) -> Vec<u8> {
    format!(
        "POST /chat/completions HTTP/1.1\r\n\
         Host: 127.0.0.1\r\n\
         Authorization: Bearer {token}\r\n\
         Content-Type: application/json\r\n\
         User-Agent: opencode/1.18.22 ai-sdk/provider-utils/4.0.23 runtime/bun/1.3.14\r\n\
         Content-Length: {}\r\n\
         \r\n\
         {body}",
        body.len()
    )
    .into_bytes()
}

/// The request line Codex was observed sending: `POST /responses`, no
/// version segment — the provider's base URL carries it.
fn responses_request(token: &str, body: &str) -> Vec<u8> {
    format!(
        "POST /responses HTTP/1.1\r\n\
         Host: 127.0.0.1\r\n\
         Authorization: Bearer {token}\r\n\
         Content-Type: application/json\r\n\
         OpenAI-Beta: responses=experimental\r\n\
         User-Agent: codex_cli_rs/0.149.1\r\n\
         Content-Length: {}\r\n\
         \r\n\
         {body}",
        body.len()
    )
    .into_bytes()
}

fn send(address: SocketAddr, raw: &[u8]) -> TcpStream {
    let mut client = TcpStream::connect(address).expect("the gateway accepts connections");
    client
        .set_read_timeout(Some(CLIENT_TIMEOUT))
        .expect("a non-zero timeout is valid");
    client
        .write_all(raw)
        .expect("the gateway reads the request");
    client.flush().expect("flush");
    client
}

fn send_and_read(address: SocketAddr, raw: &[u8]) -> Vec<u8> {
    let mut client = send(address, raw);
    let mut received = Vec::new();
    client
        .read_to_end(&mut received)
        .or_else(|err| match err.kind() {
            std::io::ErrorKind::ConnectionReset => Ok(received.len()),
            _ => Err(err),
        })
        .expect("the gateway answers and then closes");
    received
}

fn head_and_body(response: &[u8]) -> (String, &[u8]) {
    let end = response
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .expect("a response head ends with a blank line");
    (
        String::from_utf8_lossy(&response[..end]).into_owned(),
        &response[end + 4..],
    )
}

/// The payload of a chunked body, de-chunked.
fn dechunk(mut body: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    loop {
        let line_end = body
            .windows(2)
            .position(|w| w == b"\r\n")
            .expect("a chunk size line");
        let size =
            usize::from_str_radix(std::str::from_utf8(&body[..line_end]).unwrap().trim(), 16)
                .expect("a hex chunk size");
        body = &body[line_end + 2..];
        if size == 0 {
            return out;
        }
        out.extend_from_slice(&body[..size]);
        body = &body[size + 2..];
    }
}

/// `(event name, data)` for every **named** SSE event in a Responses-shaped
/// stream.
fn sse_events(text: &str) -> Vec<(String, Value)> {
    text.split("\n\n")
        .filter(|block| !block.trim().is_empty())
        .map(|block| {
            let mut event = None;
            let mut data = String::new();
            for line in block.lines() {
                if let Some(name) = line.strip_prefix("event: ") {
                    event = Some(name.to_owned());
                } else if let Some(payload) = line.strip_prefix("data: ") {
                    data.push_str(payload);
                }
            }
            (
                event.expect("every event this gateway emits is named"),
                serde_json::from_str(&data).expect("every event is JSON"),
            )
        })
        .collect()
}

/// One data frame for each OpenAI-Chat-shaped SSE chunk — this wire names no
/// events, only `data:` lines, ending with the literal `[DONE]`.
enum ChatFrame {
    Chunk(Value),
    Done,
}

fn chat_stream_frames(text: &str) -> Vec<ChatFrame> {
    text.split("\n\n")
        .filter(|block| !block.trim().is_empty())
        .map(|block| {
            let data = block
                .strip_prefix("data: ")
                .expect("every chat chunk is a data: line");
            if data.trim() == "[DONE]" {
                ChatFrame::Done
            } else {
                ChatFrame::Chunk(serde_json::from_str(data).expect("every chunk is JSON"))
            }
        })
        .collect()
}

// --- pair 1: openai-chat -> openai-responses ------------------------------------

/// Lines 1948, 1949 and 1950 for the pair, as one exchange, document and
/// stream: the fixture — which speaks only OpenAI Responses — receives a
/// well-formed `/v1/responses` body with the tool definition, the prior call
/// and its erroring result translated and their ids intact, `store: false`,
/// and `strict: false` on the tool; the client receives an OpenAI-Chat
/// response whose `tool_calls[].id`s are the fixture's `call_id`s verbatim,
/// then the same exchange with `stream: true`, translated event by event in
/// the chat-completion-chunk order and not buffered whole.
#[test]
fn an_opencode_request_is_translated_to_openai_responses_and_back_with_tool_call_ids_preserved() {
    // --- document ---
    let fixture = Fixture::start(Answer::ResponsesCompletion);
    let gateway = start_gateway(upstream_from(&provider_serving_only(
        "openai-responses",
        &fixture.base_url(),
        "responsesonly",
    )));
    assert_eq!(
        gateway.served_protocols(),
        vec!["openai-responses"],
        "the fixture-backed provider must serve OpenAI Responses and nothing else, or the \
         request below is relayed rather than translated"
    );

    let response = send_and_read(
        gateway.address(),
        &chat_request(gateway.token().expose(), &opencode_body(false)),
    );
    let (head, body) = head_and_body(&response);
    assert!(head.starts_with("HTTP/1.1 200"), "{head}");
    assert!(head.contains("content-type: application/json"), "{head}");

    // (a) what the fixture received.
    let received = fixture.only_request();
    assert_eq!(received.method, "POST");
    assert_eq!(
        received.target, "/v1/responses",
        "the translated request goes to the path the provider's native client sends"
    );
    assert_eq!(
        received.header("authorization"),
        Some(format!("Bearer {PLANTED_KEY}").as_str()),
        "the provider's credential is attached exactly where the relay attaches it"
    );
    assert_eq!(
        received.header("anthropic-version"),
        None,
        "the outbound header hook fires only toward an Anthropic-serving protocol"
    );
    assert_eq!(
        received.header("user-agent"),
        Some("opencode/1.18.22 ai-sdk/provider-utils/4.0.23 runtime/bun/1.3.14")
    );
    assert!(
        !received
            .headers
            .iter()
            .any(|(_, value)| value.contains(gateway.token().expose())),
        "the gateway's own token never leaves the process"
    );
    let sent = received.json();
    assert_eq!(sent["model"], "gpt-4o-mini");
    assert_eq!(sent["max_output_tokens"], 4096);
    assert_eq!(sent["instructions"], PLANTED_PROMPT);
    assert_eq!(sent["stream"], Value::Null);
    assert_eq!(sent["user"], "user_abc");
    assert_eq!(sent["tool_choice"], "auto");
    assert_eq!(
        sent["store"], false,
        "never left to the provider's default, which is to store"
    );
    let input = sent["input"].as_array().expect("input items");
    assert_eq!(
        input[0],
        json!({"type": "message", "role": "user", "content": [{"type": "input_text", "text": "List the files."}]})
    );
    assert_eq!(
        input[1],
        json!({"type": "message", "role": "assistant", "content": [{"type": "output_text", "text": "Sure."}]})
    );
    assert_eq!(input[2]["type"], "function_call");
    assert_eq!(
        input[2]["call_id"], "call_prior_1",
        "the replayed call keeps the id it was issued under"
    );
    assert_eq!(input[2]["arguments"], "{\"command\":\"ls /nope\"}");
    assert_eq!(input[3]["type"], "function_call_output");
    assert_eq!(input[3]["call_id"], "call_prior_1");
    assert_eq!(
        input[3]["output"],
        format!("{TOOL_ERROR_MARKER}\nls: cannot access '/nope'"),
        "an erroring result is carried, labelled, in the only channel the wire has"
    );
    assert_eq!(
        input[4],
        json!({"type": "message", "role": "user", "content": [{"type": "input_text", "text": "Try again."}]})
    );
    assert_eq!(input.len(), 5);
    let tools = sent["tools"].as_array().expect("tools");
    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0]["type"], "function");
    assert_eq!(tools[0]["name"], "Bash");
    assert_eq!(tools[0]["description"], "Run a shell command");
    assert_eq!(
        tools[0]["strict"], false,
        "never left to this wire's default, which is strict"
    );
    assert_eq!(
        tools[0]["parameters"],
        json!({"type": "object", "properties": {"command": {"type": "string"}}, "required": ["command"]})
    );

    // (b) what the client received.
    let answer: Value = serde_json::from_slice(body).expect("an OpenAI Chat JSON document");
    assert_eq!(answer["object"], "chat.completion");
    assert_eq!(answer["id"], "resp_fixture");
    assert_eq!(answer["model"], "fixture-model");
    let message = &answer["choices"][0]["message"];
    assert_eq!(message["role"], "assistant");
    assert_eq!(message["content"], "Checking.");
    let calls = message["tool_calls"].as_array().expect("tool_calls");
    assert_eq!(calls.len(), 2, "two parallel tool calls, both delivered");
    assert_eq!(
        calls[0]["id"], "call_fix_A",
        "the tool_calls[].id is the fixture's call_id, verbatim"
    );
    assert_eq!(calls[0]["function"]["name"], "Bash");
    assert_eq!(
        serde_json::from_str::<Value>(calls[0]["function"]["arguments"].as_str().unwrap()).unwrap(),
        json!({"command": "ls"})
    );
    assert_eq!(calls[1]["id"], "call_fix_B");
    assert_eq!(answer["choices"][0]["finish_reason"], "tool_calls");
    assert_eq!(
        answer["usage"],
        json!({"prompt_tokens": 48, "completion_tokens": 12, "total_tokens": 60, "prompt_tokens_details": {"cached_tokens": 8}, "completion_tokens_details": {"reasoning_tokens": 0}}),
        "prompt_tokens includes the cached ones on this wire; the form's input does not"
    );
    assert!(
        !String::from_utf8_lossy(&response).contains(PLANTED_KEY),
        "the provider credential never appears in a translated response"
    );

    // --- stream ---
    let fixture = Fixture::start(Answer::ResponsesGatedStream);
    let gateway = start_gateway(upstream_from(&provider_serving_only(
        "openai-responses",
        &fixture.base_url(),
        "responsesonly2",
    )));

    let mut client = send(
        gateway.address(),
        &chat_request(gateway.token().expose(), &opencode_body(true)),
    );

    // Read until the tool call has opened, with the fixture still paused.
    let mut so_far = Vec::new();
    let mut buffer = [0u8; 4096];
    let deadline = Instant::now() + CLIENT_TIMEOUT;
    while !String::from_utf8_lossy(&so_far).contains("\"id\":\"call_fix_A\"") {
        assert!(
            Instant::now() < deadline,
            "the tool call never arrived while the fixture was paused: {}",
            String::from_utf8_lossy(&so_far)
        );
        let read = client.read(&mut buffer).expect("the stream is open");
        assert!(
            read > 0,
            "the gateway closed the stream before the fixture finished"
        );
        so_far.extend_from_slice(&buffer[..read]);
    }
    let text_so_far = String::from_utf8_lossy(&so_far).into_owned();
    assert!(
        !text_so_far.contains("[DONE]") && !so_far.ends_with(b"0\r\n\r\n"),
        "the stream was buffered whole rather than translated as it arrived: {text_so_far}"
    );

    fixture.release();
    client
        .read_to_end(&mut so_far)
        .or_else(|err| match err.kind() {
            std::io::ErrorKind::ConnectionReset => Ok(0),
            _ => Err(err),
        })
        .expect("the stream ends");

    let (head, body) = head_and_body(&so_far);
    assert!(head.starts_with("HTTP/1.1 200"), "{head}");
    assert!(head.contains("content-type: text/event-stream"), "{head}");
    assert!(
        body.ends_with(b"0\r\n\r\n"),
        "a chunked stream ends with the zero-length chunk"
    );
    let frames = chat_stream_frames(&String::from_utf8(dechunk(body)).expect("UTF-8 frames"));
    assert_eq!(
        frames.len(),
        8,
        "message start, one text delta, tool-call open, two argument deltas, finish, usage, [DONE]"
    );
    let chunk = |index: usize| match &frames[index] {
        ChatFrame::Chunk(value) => value.clone(),
        ChatFrame::Done => panic!("frame {index} was [DONE], not a chunk"),
    };
    assert_eq!(
        chunk(0)["choices"][0]["delta"],
        json!({"role": "assistant", "content": ""})
    );
    assert_eq!(
        chunk(1)["choices"][0]["delta"],
        json!({"content": "Checking."})
    );
    assert_eq!(
        chunk(2)["choices"][0]["delta"]["tool_calls"][0],
        json!({"index": 0, "id": "call_fix_A", "type": "function", "function": {"name": "Bash", "arguments": ""}}),
        "the tool call opened with the fixture's call_id before the arguments finished"
    );
    let partial: String = [3, 4]
        .iter()
        .map(|&index| {
            chunk(index)["choices"][0]["delta"]["tool_calls"][0]["function"]["arguments"]
                .as_str()
                .unwrap()
                .to_owned()
        })
        .collect();
    assert_eq!(
        serde_json::from_str::<Value>(&partial).unwrap(),
        json!({"command": "ls"}),
        "the argument fragments join into the tool input"
    );
    assert_eq!(chunk(5)["choices"][0]["finish_reason"], "tool_calls");
    assert_eq!(chunk(6)["choices"], json!([]));
    assert_eq!(
        chunk(6)["usage"],
        json!({"prompt_tokens": 48, "completion_tokens": 12, "total_tokens": 60, "prompt_tokens_details": {"cached_tokens": 8}})
    );
    assert!(
        matches!(frames[7], ChatFrame::Done),
        "the stream ends with the literal [DONE]"
    );

    // ... and the fixture was asked for a stream, with storage declined.
    let sent = fixture.only_request().json();
    assert_eq!(sent["stream"], true);
    assert_eq!(sent["store"], false);
}

// --- pair 2: openai-responses -> openai-chat ------------------------------------

/// The mirror exchange, document and stream: a Codex-shaped client POSTs
/// `/responses` against a provider serving only OpenAI Chat. The fixture
/// receives a well-formed `/chat/completions` body — tool round translated,
/// ids intact, the error marker restored — and the client receives a
/// Responses document whose `call_id`s are the fixture's `tool_calls[].id`s
/// verbatim, then the same exchange streamed, event by event, in the
/// Responses order.
#[test]
fn a_codex_shaped_request_is_translated_to_openai_chat_and_back_with_tool_call_ids_preserved() {
    // --- document ---
    let fixture = Fixture::start(Answer::ChatCompletion);
    let gateway = start_gateway(upstream_from(&provider_serving_only(
        "openai-chat",
        &fixture.base_url(),
        "chatonly",
    )));
    assert_eq!(
        gateway.served_protocols(),
        vec!["openai-chat"],
        "the fixture-backed provider must serve OpenAI Chat and nothing else"
    );

    let response = send_and_read(
        gateway.address(),
        &responses_request(gateway.token().expose(), &codex_body(false)),
    );
    let (head, body) = head_and_body(&response);
    assert!(head.starts_with("HTTP/1.1 200"), "{head}");
    assert!(head.contains("content-type: application/json"), "{head}");

    // (a) what the fixture received.
    let received = fixture.only_request();
    assert_eq!(received.method, "POST");
    assert_eq!(
        received.target, "/v1/chat/completions",
        "the OpenAI-Chat-serving base URL carries /v1, and the codec's own endpoint completes it"
    );
    assert_eq!(
        received.header("authorization"),
        Some(format!("Bearer {PLANTED_KEY}").as_str())
    );
    assert_eq!(
        received.header("anthropic-version"),
        None,
        "the outbound header hook fires only toward an Anthropic-serving protocol"
    );
    assert_eq!(
        received.header("openai-beta"),
        None,
        "a header the client sent for its own protocol is not forwarded verbatim"
    );
    assert_eq!(received.header("user-agent"), Some("codex_cli_rs/0.149.1"));
    let sent = received.json();
    assert_eq!(sent["model"], "gpt-5");
    assert_eq!(sent["max_tokens"], 4096);
    let messages = sent["messages"].as_array().expect("messages");
    assert_eq!(
        messages[0],
        json!({"role": "system", "content": PLANTED_PROMPT})
    );
    assert_eq!(
        messages[1],
        json!({"role": "user", "content": "List the files."})
    );
    assert_eq!(messages[2]["role"], "assistant");
    assert_eq!(messages[2]["content"], "Sure.");
    assert_eq!(
        messages[2]["tool_calls"][0],
        json!({"id": "call_prior_9", "type": "function", "function": {"name": "Bash", "arguments": "{\"command\":\"ls /nope\"}"}}),
        "the replayed call keeps the id it was issued under"
    );
    assert_eq!(messages[3]["role"], "tool");
    assert_eq!(messages[3]["tool_call_id"], "call_prior_9");
    assert_eq!(
        messages[3]["content"],
        format!("{TOOL_ERROR_MARKER}\nls: cannot access '/nope'"),
        "an erroring result is carried, labelled, in the only channel the wire has"
    );
    assert_eq!(
        messages[4],
        json!({"role": "user", "content": "Try again."})
    );
    assert_eq!(messages.len(), 5);
    let tools = sent["tools"].as_array().expect("tools");
    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0]["type"], "function");
    assert_eq!(tools[0]["function"]["name"], "Bash");
    assert_eq!(
        tools[0]["function"]["parameters"],
        json!({"type": "object", "properties": {"command": {"type": "string"}}, "required": ["command"]})
    );
    assert_eq!(sent["tool_choice"], "auto");
    assert_eq!(sent["parallel_tool_calls"], true);

    // (b) what the client received.
    let answer: Value = serde_json::from_slice(body).expect("a Responses JSON document");
    assert_eq!(answer["object"], "response");
    assert_eq!(answer["id"], "chatcmpl_fix");
    assert_eq!(answer["model"], "fixture-model");
    assert_eq!(answer["status"], "completed");
    let output = answer["output"].as_array().expect("output items");
    assert_eq!(output[0]["type"], "message");
    assert_eq!(
        output[0]["content"],
        json!([{"type": "output_text", "text": "Checking.", "annotations": []}])
    );
    assert_eq!(output[1]["type"], "function_call");
    assert_eq!(
        output[1]["call_id"], "call_fix_A",
        "the call_id is the provider's tool_calls[].id, verbatim"
    );
    assert_eq!(output[1]["name"], "Bash");
    assert_eq!(output[1]["arguments"], "{\"command\":\"ls\"}");
    assert_eq!(output[2]["call_id"], "call_fix_B");
    assert_eq!(output.len(), 3);
    assert_eq!(
        answer["usage"],
        json!({"input_tokens": 48, "input_tokens_details": {"cached_tokens": 8}, "output_tokens": 12, "total_tokens": 60}),
        "this wire's input_tokens includes the cached ones, same as the form's prompt_tokens"
    );
    assert!(!String::from_utf8_lossy(&response).contains(PLANTED_KEY));

    // --- stream ---
    let fixture = Fixture::start(Answer::ChatGatedStream);
    let gateway = start_gateway(upstream_from(&provider_serving_only(
        "openai-chat",
        &fixture.base_url(),
        "chatonly2",
    )));

    let mut client = send(
        gateway.address(),
        &responses_request(gateway.token().expose(), &codex_body(true)),
    );

    let mut so_far = Vec::new();
    let mut buffer = [0u8; 4096];
    let deadline = Instant::now() + CLIENT_TIMEOUT;
    while !String::from_utf8_lossy(&so_far).contains("function_call_arguments.delta") {
        assert!(
            Instant::now() < deadline,
            "the function-call item never arrived while the fixture was paused: {}",
            String::from_utf8_lossy(&so_far)
        );
        let read = client.read(&mut buffer).expect("the stream is open");
        assert!(
            read > 0,
            "the gateway closed the stream before the fixture finished"
        );
        so_far.extend_from_slice(&buffer[..read]);
    }
    let text_so_far = String::from_utf8_lossy(&so_far).into_owned();
    assert!(
        !text_so_far.contains("response.completed"),
        "the stream was buffered whole rather than translated as it arrived: {text_so_far}"
    );
    assert!(
        text_so_far.contains("\"call_id\":\"call_fix_A\""),
        "the function-call item opened with the provider's tool_calls[].id before the arguments \
         finished: {text_so_far}"
    );

    fixture.release();
    client
        .read_to_end(&mut so_far)
        .or_else(|err| match err.kind() {
            std::io::ErrorKind::ConnectionReset => Ok(0),
            _ => Err(err),
        })
        .expect("the stream ends");

    let (head, body) = head_and_body(&so_far);
    assert!(head.starts_with("HTTP/1.1 200"), "{head}");
    assert!(head.contains("content-type: text/event-stream"), "{head}");
    assert!(body.ends_with(b"0\r\n\r\n"));
    let events = sse_events(&String::from_utf8(dechunk(body)).expect("UTF-8 events"));
    let names: Vec<&str> = events.iter().map(|(name, _)| name.as_str()).collect();
    assert_eq!(
        names,
        vec![
            "response.created",
            "response.output_item.added",
            "response.content_part.added",
            "response.output_text.delta",
            "response.output_text.done",
            "response.content_part.done",
            "response.output_item.done",
            "response.output_item.added",
            "response.function_call_arguments.delta",
            "response.function_call_arguments.delta",
            "response.function_call_arguments.done",
            "response.output_item.done",
            "response.completed",
        ],
        "the Responses event order, every item added, streamed, and closed"
    );
    assert_eq!(events[0].1["response"]["id"], "chatcmpl_fix");
    assert_eq!(events[3].1["delta"], "Checking.");
    assert_eq!(events[7].1["item"]["type"], "function_call");
    assert_eq!(events[7].1["item"]["call_id"], "call_fix_A");
    let arguments: String = [&events[8].1, &events[9].1]
        .iter()
        .map(|event| event["delta"].as_str().unwrap())
        .collect();
    assert_eq!(
        serde_json::from_str::<Value>(&arguments).unwrap(),
        json!({"command": "ls"})
    );
    assert_eq!(events[10].1["arguments"], arguments);
    let completed = &events[12].1["response"];
    assert_eq!(completed["status"], "completed");
    assert_eq!(
        completed["usage"]["input_tokens"], 48,
        "the chat wire's prompt_tokens (cached included) survives to the final snapshot"
    );
    assert_eq!(completed["usage"]["output_tokens"], 12);
    assert_eq!(
        completed["output"][1]["call_id"], "call_fix_A",
        "the final snapshot echoes the complete items"
    );

    let sent = fixture.only_request().json();
    assert_eq!(sent["stream"], true);
}

// --- the outbound-header hook (T2 finding 2) ------------------------------------

/// A translated request toward an Anthropic-serving provider carries
/// `anthropic-version: 2023-06-01` — the version real clients send and the
/// relay path already forwards verbatim. Driven through T2's own already-
/// supported pair 2 (`openai-responses -> anthropic-messages`), because
/// neither T2b pair ever targets an Anthropic-serving protocol; the two
/// tests above assert the header's absence on the T2b pairs' own outbound
/// requests.
#[test]
fn a_translated_request_toward_an_anthropic_serving_provider_carries_the_version_header() {
    let fixture = Fixture::start(Answer::AnthropicCompletion);
    let gateway = start_gateway(upstream_from(&provider_serving_only(
        "anthropic-messages",
        &fixture.root_url(),
        "anthroponly",
    )));
    assert_eq!(gateway.served_protocols(), vec!["anthropic-messages"]);

    let response = send_and_read(
        gateway.address(),
        &responses_request(gateway.token().expose(), &codex_body(false)),
    );
    let (head, _) = head_and_body(&response);
    assert!(head.starts_with("HTTP/1.1 200"), "{head}");

    let received = fixture.only_request();
    assert_eq!(
        received.target, "/v1/messages",
        "the Anthropic-serving base URL carries no /v1, so the translated target must"
    );
    assert_eq!(
        received.header("anthropic-version"),
        Some("2023-06-01"),
        "api.anthropic.com requires this header on every request, and a translated request has \
         no client header to relay it from"
    );
}
