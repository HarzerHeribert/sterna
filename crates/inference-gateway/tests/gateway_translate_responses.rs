//! Phase 56, lines 1948–1950 and 1956: the two T2 pairs — Claude Code's
//! Anthropic Messages served by an OpenAI-Responses entitlement, and a
//! Codex-shaped client's OpenAI Responses served by an Anthropic Messages
//! entitlement — each end to end against a fixture upstream that speaks
//! **only** the provider's protocol and records what reached it.
//!
//! # Where these tests enter
//!
//! Exactly where `tests/gateway_translate.rs` enters, and for the same
//! reasons its header records: `gateway::start_if_required_with_degrade_sink`
//! — the door the shipped binary calls, real accept loop, real sockets —
//! with an [`Upstream`] built by the **production** `profile::gateway_upstream`
//! from a real provider template narrowed to the one protocol the fixture
//! speaks. The `glasshouse launch` link stays blocked at
//! `profile::apply_gateway`, and T1's witness test in the sibling file is
//! the one that converts the day it lifts; this file does not duplicate it.
//!
//! # What is asserted, and against what
//!
//! Every claim about the outbound request is made against bytes the fixture
//! read off the wire with its own parser; every claim about the inbound
//! answer against the bytes a plain `TcpStream` client received. Ids are
//! compared as strings on both sides — a Responses `call_id` *is* the
//! Anthropic `tool_use.id`, and a wrong one runs the wrong tool.

mod host;
use host::Ledger;

use std::io::{Read, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use inference_gateway::gateway::translate::TOOL_ERROR_MARKER;
use inference_gateway::gateway::{Gateway, Upstream};
use inference_gateway::provider::Provider;
use inference_gateway::routing::AssignedModel;
use inference_gateway::routing::evidence::Outcome;
use inference_gateway::secret::EnvironmentSecretStore;
use serde_json::{Value, json};

/// A planted provider credential, unique to this test binary. Never a real
/// key, and asserted on so that `!contains` has something to bite.
const PLANTED_KEY: &str = "sk-planted-translate-t2-000111222333";
const CREDENTIAL_VAR: &str = "GLASSHOUSE_GATEWAY_TRANSLATE_RESPONSES_TEST_KEY";

/// A body that a request header cannot legitimately carry and that no test
/// wants to see anywhere but in the one place it was planted.
const PLANTED_PROMPT: &str = "PLANTED-PROMPT-TEXT-FOR-RESPONSES-TESTS";

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
    /// One OpenAI Responses document: a text part and two function calls.
    ResponsesCompletion,
    /// The same as a Responses event stream, pausing mid-arguments until
    /// [`Fixture::release`] is called.
    ResponsesGatedStream,
    /// One Anthropic Messages document: a text block and two tool uses.
    AnthropicCompletion,
    /// The same as an Anthropic event stream, pausing mid-arguments until
    /// [`Fixture::release`] is called.
    AnthropicGatedStream,
    /// Every target answered with a fixed document carrying a marker key
    /// the codecs would refuse — so a relayed answer can be told from a
    /// translated one by what the client receives.
    Relay,
}

/// A canned provider: its own endpoint is answered in its protocol's shape,
/// every request is recorded, and any other target gets the marker document.
struct Fixture {
    address: SocketAddr,
    connections: Arc<AtomicUsize>,
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
        let connections = Arc::new(AtomicUsize::new(0));
        let requests = Arc::new(Mutex::new(Vec::new()));
        let gate = Arc::new(AtomicBool::new(false));
        let stop = Arc::new(AtomicBool::new(false));
        let accept = std::thread::spawn({
            let connections = Arc::clone(&connections);
            let requests = Arc::clone(&requests);
            let gate = Arc::clone(&gate);
            let stop = Arc::clone(&stop);
            move || {
                while !stop.load(Ordering::Relaxed) {
                    match listener.accept() {
                        Ok((stream, _)) => {
                            connections.fetch_add(1, Ordering::Relaxed);
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
            connections,
            requests,
            gate,
            stop,
            accept: Some(accept),
        }
    }

    /// The base URL an OpenAI-shaped provider declares: with `/v1`, so a
    /// Responses client's `/responses` lands on `/v1/responses`.
    fn base_url(&self) -> String {
        format!("http://{}/v1", self.address)
    }

    /// The base URL an Anthropic-serving provider declares: the root, with
    /// no `/v1` — its native client appends `/v1/messages` itself.
    fn root_url(&self) -> String {
        format!("http://{}", self.address)
    }

    fn connections(&self) -> usize {
        self.connections.load(Ordering::Relaxed)
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
        Answer::ResponsesCompletion | Answer::ResponsesGatedStream => "/v1/responses",
        Answer::AnthropicCompletion | Answer::AnthropicGatedStream => "/v1/messages",
        Answer::Relay => "",
    };
    if matches!(answer, Answer::Relay) || !request.target.starts_with(endpoint) {
        // Not this provider's endpoint (or a relay test): answer with a
        // document the codecs would refuse (an unknown key), so a client
        // that receives it verbatim proves nothing decoded it.
        let body = r#"{"id":"resp_relayed","object":"response","status":"completed","model":"m","output":[],"planted_marker_the_codecs_refuse":true}"#;
        write_document(&mut stream, "200 OK", body);
        return;
    }
    match answer {
        Answer::Relay => unreachable!("handled above"),
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
                    {"type": "tool_use", "id": "toolu_fix_A", "name": "Bash", "input": {"command": "ls"}},
                    {"type": "tool_use", "id": "toolu_fix_B", "name": "Read", "input": {"file_path": "/tmp/x"}}
                ],
                "stop_reason": "tool_use",
                "stop_sequence": null,
                "usage": {"input_tokens": 30, "output_tokens": 9, "cache_read_input_tokens": 5}
            })
            .to_string(),
        ),
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
            // Pause until the test has seen those events arrive at the
            // client: what proves the stream was translated as a stream.
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
        Answer::AnthropicGatedStream => {
            let head = "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ntransfer-encoding: chunked\r\n\r\n";
            let _ = stream.write_all(head.as_bytes());
            let event = |name: &str, data: &str| format!("event: {name}\ndata: {data}\n\n");
            let before = [
                event("message_start", r#"{"type":"message_start","message":{"id":"msg_fix","type":"message","role":"assistant","model":"claude-x","content":[],"stop_reason":null,"stop_sequence":null,"usage":{"input_tokens":30,"cache_read_input_tokens":5,"output_tokens":1}}}"#),
                event("content_block_start", r#"{"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}"#),
                event("content_block_delta", r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Checking."}}"#),
                event("content_block_stop", r#"{"type":"content_block_stop","index":0}"#),
                event("content_block_start", r#"{"type":"content_block_start","index":1,"content_block":{"type":"tool_use","id":"toolu_fix_A","name":"Bash","input":{}}}"#),
                event("content_block_delta", r#"{"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"{\"command\""}}"#),
            ];
            for chunk in before {
                write_chunk(&mut stream, chunk.as_bytes());
            }
            let deadline = Instant::now() + Duration::from_secs(20);
            while !gate.load(Ordering::Relaxed) && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(5));
            }
            let after = [
                event("content_block_delta", r#"{"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":": \"ls\"}"}}"#),
                event("content_block_stop", r#"{"type":"content_block_stop","index":1}"#),
                event("message_delta", r#"{"type":"message_delta","delta":{"stop_reason":"tool_use","stop_sequence":null},"usage":{"output_tokens":9}}"#),
                event("message_stop", r#"{"type":"message_stop"}"#),
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
/// state. See the sibling `gateway_translate.rs` for the measured failure
/// this prevents.
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

struct LedgerFixture {
    _tmp: tempfile::TempDir,
    ledger: Arc<Ledger>,
}

/// The project's evidence ledger, opened through the same bootstrap the
/// binary uses, so the row a translated exchange writes can be read back.
fn ledger_fixture() -> LedgerFixture {
    let tmp = tempfile::tempdir().expect("tempdir");
    let base = std::fs::canonicalize(tmp.path()).expect("canonicalize");
    let root = base.join("workspace");
    std::fs::create_dir_all(root.join(".git")).expect("project root");
    LedgerFixture {
        _tmp: tmp,
        ledger: Ledger::new(),
    }
}

/// The door the shipped binary calls, with the given upstream.
fn start_gateway(upstream: Upstream, ledger: Option<Arc<Ledger>>) -> Gateway {
    inference_gateway::gateway::start_if_required_with_degrade_sink(
        &[inference_gateway::gateway::BackendDemand::LocalGateway],
        || Ok(upstream),
        None,
        None,
        ledger.map(|ledger| host::observation_sink(ledger, None)),
        None,
    )
    .expect("loopback is bindable")
    .expect("a gateway-backed profile requires a gateway")
}

// --- the two clients -----------------------------------------------------------

/// The request Claude Code sends after one tool round — identical in shape
/// to the sibling file's, because the harness does not change when the
/// provider's protocol does.
fn claude_code_body(stream: bool) -> String {
    json!({
        "model": "claude-x",
        "max_tokens": 4096,
        "system": [{"type": "text", "text": PLANTED_PROMPT}],
        "messages": [
            {"role": "user", "content": "List the files."},
            {"role": "assistant", "content": [
                {"type": "text", "text": "Sure."},
                {"type": "tool_use", "id": "call_prior_1", "name": "Bash", "input": {"command": "ls /nope"}}
            ]},
            {"role": "user", "content": [
                {"type": "tool_result", "tool_use_id": "call_prior_1", "content": "ls: cannot access '/nope'", "is_error": true},
                {"type": "text", "text": "Try again."}
            ]}
        ],
        "tools": [{
            "name": "Bash",
            "description": "Run a shell command",
            "input_schema": {"type": "object", "properties": {"command": {"type": "string"}}, "required": ["command"]}
        }],
        "tool_choice": {"type": "auto"},
        "metadata": {"user_id": "user_abc"},
        "stream": stream
    })
    .to_string()
}

/// The request a Codex-shaped client sends after one tool round: typed
/// message items, the assistant's replayed function call, its output — with
/// the error marker, which the pair restores as `is_error` — and a follow-up.
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

fn messages_request(token: &str, body: &str) -> Vec<u8> {
    format!(
        "POST /v1/messages?beta=true HTTP/1.1\r\n\
         Host: 127.0.0.1\r\n\
         Authorization: Bearer {token}\r\n\
         Content-Type: application/json\r\n\
         Anthropic-Version: 2023-06-01\r\n\
         User-Agent: claude-cli/2.1.245\r\n\
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

/// `(event name, data)` for every event in an SSE text.
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

fn wait_for_row(
    ledger: &Ledger,
    provider: &str,
    model: &str,
    route: Option<&str>,
    harness: Option<&str>,
) -> Vec<inference_gateway::routing::evidence::RoutingObservation> {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let rows: Vec<_> = ledger
            .observations_in_window(i64::MAX, i64::MAX)
            .expect("read the ledger")
            .into_iter()
            .filter(|row| {
                row.provider == provider
                    && row.model == model
                    && row.route.as_deref() == route
                    && row.harness.as_deref() == harness
            })
            .collect();
        if !rows.is_empty() || Instant::now() >= deadline {
            return rows;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

// --- pair 1: anthropic-messages -> openai-responses ------------------------------

/// Lines 1948, 1949 and 1950 for the pair, as one exchange: the fixture —
/// which speaks only OpenAI Responses — receives a well-formed
/// `/v1/responses` body with the tool definition, the prior call and its
/// erroring result translated and their ids intact, `store: false`, and
/// `strict: false` on the tool; the client receives an Anthropic Messages
/// response whose `tool_use` ids are the fixture's `call_id`s verbatim; and
/// the exchange is recorded under the pair's own name with the provider's
/// exact usage.
#[test]
fn a_claude_code_request_is_translated_to_openai_responses_and_back_with_ids_preserved() {
    let fixture = Fixture::start(Answer::ResponsesCompletion);
    let ledger = ledger_fixture();
    let gateway = start_gateway(
        upstream_from(&provider_serving_only(
            "openai-responses",
            &fixture.base_url(),
            "responsesonly",
        )),
        Some(Arc::clone(&ledger.ledger)),
    );
    assert_eq!(
        gateway.served_protocols(),
        vec!["openai-responses"],
        "the fixture-backed provider must serve OpenAI Responses and nothing else, or the \
         request below is relayed rather than translated"
    );
    gateway.routing().bind(
        "claude-code",
        "openai-responses",
        AssignedModel::HarnessDefault,
        &gateway
            .upstream()
            .expect("a started gateway has its upstream"),
    );

    let response = send_and_read(
        gateway.address(),
        &messages_request(gateway.token().expose(), &claude_code_body(false)),
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
        "an Anthropic-only header is not forwarded to an OpenAI provider"
    );
    assert_eq!(received.header("user-agent"), Some("claude-cli/2.1.245"));
    assert!(
        !received
            .headers
            .iter()
            .any(|(_, value)| value.contains(gateway.token().expose())),
        "the gateway's own token never leaves the process"
    );
    let sent = received.json();
    assert_eq!(sent["model"], "claude-x");
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
    assert_eq!(
        tools[0]["name"], "Bash",
        "flat, unlike OpenAI Chat's nesting"
    );
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
    let answer: Value = serde_json::from_slice(body).expect("an Anthropic JSON document");
    assert_eq!(answer["type"], "message");
    assert_eq!(answer["role"], "assistant");
    assert_eq!(answer["id"], "resp_fixture");
    assert_eq!(answer["model"], "fixture-model");
    assert_eq!(answer["stop_reason"], "tool_use");
    let content = answer["content"].as_array().expect("content blocks");
    assert_eq!(content[0], json!({"type": "text", "text": "Checking."}));
    assert_eq!(
        content[1],
        json!({"type": "tool_use", "id": "call_fix_A", "name": "Bash", "input": {"command": "ls"}}),
        "the tool_use id is the fixture's call_id, verbatim"
    );
    assert_eq!(
        content[2],
        json!({"type": "tool_use", "id": "call_fix_B", "name": "Read", "input": {"file_path": "/tmp/x"}})
    );
    assert_eq!(content.len(), 3, "two parallel tool calls, both delivered");
    assert_eq!(
        answer["usage"],
        json!({"input_tokens": 40, "output_tokens": 12, "cache_read_input_tokens": 8}),
        "input_tokens includes the cached ones on this wire; Anthropic's does not"
    );
    assert!(
        !String::from_utf8_lossy(&response).contains(PLANTED_KEY),
        "the provider credential never appears in a translated response"
    );

    // Recorded under the pair's own name, with the provider's exact usage.
    let rows = wait_for_row(
        &ledger.ledger,
        "responsesonly",
        "claude-x",
        Some("anthropic-messages->openai-responses"),
        Some("claude-code"),
    );
    assert_eq!(
        rows.len(),
        1,
        "one routing observation, with `route` naming the translated pair"
    );
    assert_eq!(rows[0].outcome, Some(Outcome::Succeeded));
    assert_eq!(rows[0].input_tokens, Some(40));
    assert_eq!(rows[0].output_tokens, Some(12));
    assert_eq!(rows[0].cached_input_tokens, Some(8));
    assert!(rows[0].first_byte_at_unix.is_some());
}

/// The same pair with `stream: true`: the fixture's Responses events become
/// Anthropic's events **as they arrive** — the client holds the tool block's
/// start while the fixture is still paused mid-arguments — in Anthropic's
/// order, chunk-terminated.
#[test]
fn a_streamed_claude_code_request_is_translated_event_by_event_in_anthropics_order() {
    let fixture = Fixture::start(Answer::ResponsesGatedStream);
    let gateway = start_gateway(
        upstream_from(&provider_serving_only(
            "openai-responses",
            &fixture.base_url(),
            "responsesonly",
        )),
        None,
    );

    let mut client = send(
        gateway.address(),
        &messages_request(gateway.token().expose(), &claude_code_body(true)),
    );

    // Read until the tool block has started, with the fixture still paused.
    let mut so_far = Vec::new();
    let mut buffer = [0u8; 4096];
    let deadline = Instant::now() + CLIENT_TIMEOUT;
    while !String::from_utf8_lossy(&so_far).contains("\"partial_json\"") {
        assert!(
            Instant::now() < deadline,
            "the tool block never arrived while the fixture was paused: {}",
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
        !text_so_far.contains("message_stop") && !so_far.ends_with(b"0\r\n\r\n"),
        "the stream was buffered whole rather than translated as it arrived: {text_so_far}"
    );
    assert!(
        text_so_far.contains("\"id\":\"call_fix_A\""),
        "the tool block started with the fixture's call_id before the arguments finished: \
         {text_so_far}"
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
    let events = sse_events(&String::from_utf8(dechunk(body)).expect("UTF-8 events"));
    let names: Vec<&str> = events.iter().map(|(name, _)| name.as_str()).collect();
    assert_eq!(
        names,
        vec![
            "message_start",
            "content_block_start",
            "content_block_delta",
            "content_block_stop",
            "content_block_start",
            "content_block_delta",
            "content_block_delta",
            "content_block_stop",
            "message_delta",
            "message_stop",
        ],
        "Anthropic's event order, each block stopped before the next starts"
    );
    assert_eq!(events[0].1["message"]["id"], "resp_fixture");
    assert_eq!(
        events[1].1["content_block"],
        json!({"type": "text", "text": ""})
    );
    assert_eq!(
        events[2].1["delta"],
        json!({"type": "text_delta", "text": "Checking."})
    );
    assert_eq!(
        events[4].1["content_block"],
        json!({"type": "tool_use", "id": "call_fix_A", "name": "Bash", "input": {}})
    );
    let partial: String = [&events[5].1, &events[6].1]
        .iter()
        .map(|event| event["delta"]["partial_json"].as_str().unwrap())
        .collect();
    assert_eq!(
        serde_json::from_str::<Value>(&partial).unwrap(),
        json!({"command": "ls"}),
        "the argument fragments join into the tool input"
    );
    assert_eq!(events[8].1["delta"]["stop_reason"], "tool_use");
    assert_eq!(events[8].1["usage"]["output_tokens"], 12);
    assert_eq!(events[8].1["usage"]["input_tokens"], 40);
    assert_eq!(events[9].1, json!({"type": "message_stop"}));

    // ... and the fixture was asked for a stream, with storage declined.
    let sent = fixture.only_request().json();
    assert_eq!(sent["stream"], true);
    assert_eq!(sent["store"], false);
}

/// A request this pair cannot carry is answered with a `400` in Claude
/// Code's own error shape naming the pair, the field and the reason — and
/// the fixture never sees a connection. `stop_sequences` is the field the
/// Responses wire has no home for at all, so its refusal happens *after*
/// decoding, on the provider's side of the pair, and still before anything
/// opens upstream.
#[test]
fn a_request_the_responses_pair_cannot_carry_is_refused_by_name_and_nothing_opens_upstream() {
    let fixture = Fixture::start(Answer::ResponsesCompletion);
    let gateway = start_gateway(
        upstream_from(&provider_serving_only(
            "openai-responses",
            &fixture.base_url(),
            "responsesonly",
        )),
        None,
    );

    let with_stop = json!({
        "model": "claude-x",
        "max_tokens": 10,
        "system": [{"type": "text", "text": PLANTED_PROMPT}],
        "messages": [{"role": "user", "content": "hi"}],
        "stop_sequences": ["END"]
    })
    .to_string();
    let response = send_and_read(
        gateway.address(),
        &messages_request(gateway.token().expose(), &with_stop),
    );
    let (head, body) = head_and_body(&response);
    assert!(head.starts_with("HTTP/1.1 400"), "{head}");
    let error: Value = serde_json::from_slice(body).expect("an Anthropic error document");
    assert_eq!(error["type"], "error");
    assert_eq!(error["error"]["type"], "invalid_request_error");
    let message = error["error"]["message"].as_str().unwrap();
    assert!(
        message.contains("anthropic-messages->openai-responses"),
        "{message}"
    );
    assert!(message.contains("`stop_sequences`"), "{message}");
    assert!(message.contains("no stop sequences"), "{message}");
    assert!(
        !message.contains(PLANTED_PROMPT),
        "a refusal names the field and never quotes the request: {message}"
    );
    assert_eq!(
        fixture.connections(),
        0,
        "a refused request opens nothing upstream"
    );
}

// --- pair 2: openai-responses -> anthropic-messages ------------------------------

/// The mirror exchange: a Codex-shaped client POSTs `/responses` against a
/// provider serving only Anthropic Messages. The fixture receives a
/// well-formed `/v1/messages` body — at the path Anthropic's own client
/// sends, on a base URL that carries no `/v1` — with the tool round
/// translated, ids intact and the error marker restored to `is_error`; the
/// client receives a Responses document whose `call_id`s are the fixture's
/// `tool_use` ids verbatim.
#[test]
fn a_codex_request_is_translated_to_anthropic_messages_and_back_with_ids_preserved() {
    let fixture = Fixture::start(Answer::AnthropicCompletion);
    let ledger = ledger_fixture();
    let gateway = start_gateway(
        upstream_from(&provider_serving_only(
            "anthropic-messages",
            &fixture.root_url(),
            "anthroponly",
        )),
        Some(Arc::clone(&ledger.ledger)),
    );
    assert_eq!(
        gateway.served_protocols(),
        vec!["anthropic-messages"],
        "the fixture-backed provider must serve Anthropic Messages and nothing else"
    );
    gateway.routing().bind(
        "codex",
        "anthropic-messages",
        AssignedModel::HarnessDefault,
        &gateway
            .upstream()
            .expect("a started gateway has its upstream"),
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
        received.target, "/v1/messages",
        "the Anthropic-serving base URL carries no /v1, so the translated target must"
    );
    assert_eq!(
        received.header("authorization"),
        Some(format!("Bearer {PLANTED_KEY}").as_str())
    );
    assert_eq!(
        received.header("openai-beta"),
        None,
        "an OpenAI-only header is not forwarded to an Anthropic provider"
    );
    assert_eq!(received.header("user-agent"), Some("codex_cli_rs/0.149.1"));
    let sent = received.json();
    assert_eq!(sent["model"], "gpt-5");
    assert_eq!(sent["max_tokens"], 4096);
    assert_eq!(sent["system"], PLANTED_PROMPT);
    assert_eq!(sent["metadata"], json!({"user_id": "user_xyz"}));
    assert_eq!(
        sent["tool_choice"],
        json!({"type": "auto", "disable_parallel_tool_use": false})
    );
    let messages = sent["messages"].as_array().expect("messages");
    assert_eq!(messages[0]["role"], "user");
    assert_eq!(
        messages[0]["content"],
        json!([{"type": "text", "text": "List the files."}])
    );
    assert_eq!(messages[1]["role"], "assistant");
    assert_eq!(
        messages[1]["content"],
        json!([
            {"type": "text", "text": "Sure."},
            {"type": "tool_use", "id": "call_prior_9", "name": "Bash", "input": {"command": "ls /nope"}}
        ]),
        "the assistant message item and its function_call arrive as one turn, id intact"
    );
    assert_eq!(messages[2]["role"], "user");
    assert_eq!(
        messages[2]["content"],
        json!([
            {"type": "tool_result", "tool_use_id": "call_prior_9", "content": "ls: cannot access '/nope'", "is_error": true},
            {"type": "text", "text": "Try again."}
        ]),
        "the marker becomes is_error, and the result answers the call by its id"
    );
    assert_eq!(messages.len(), 3);
    let tools = sent["tools"].as_array().expect("tools");
    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0]["name"], "Bash");
    assert_eq!(
        tools[0]["input_schema"],
        json!({"type": "object", "properties": {"command": {"type": "string"}}, "required": ["command"]})
    );

    // (b) what the client received.
    let answer: Value = serde_json::from_slice(body).expect("a Responses JSON document");
    assert_eq!(answer["object"], "response");
    assert_eq!(answer["id"], "msg_fix");
    assert_eq!(answer["model"], "claude-x");
    assert_eq!(answer["status"], "completed");
    let output = answer["output"].as_array().expect("output items");
    assert_eq!(output[0]["type"], "message");
    assert_eq!(
        output[0]["content"],
        json!([{"type": "output_text", "text": "Checking.", "annotations": []}])
    );
    assert_eq!(output[1]["type"], "function_call");
    assert_eq!(
        output[1]["call_id"], "toolu_fix_A",
        "the call_id is the provider's tool_use id, verbatim"
    );
    assert_eq!(output[1]["name"], "Bash");
    assert_eq!(output[1]["arguments"], "{\"command\":\"ls\"}");
    assert_eq!(output[2]["call_id"], "toolu_fix_B");
    assert_eq!(output.len(), 3);
    assert_eq!(
        answer["usage"],
        json!({"input_tokens": 35, "input_tokens_details": {"cached_tokens": 5}, "output_tokens": 9, "total_tokens": 44}),
        "Anthropic's input_tokens excludes the cached ones; this wire's includes them"
    );
    assert!(!String::from_utf8_lossy(&response).contains(PLANTED_KEY));

    // Recorded under the mirror pair's own name.
    let rows = wait_for_row(
        &ledger.ledger,
        "anthroponly",
        "gpt-5",
        Some("openai-responses->anthropic-messages"),
        Some("codex"),
    );
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].outcome, Some(Outcome::Succeeded));
    assert_eq!(rows[0].input_tokens, Some(30));
    assert_eq!(rows[0].output_tokens, Some(9));
    assert_eq!(rows[0].cached_input_tokens, Some(5));
}

/// The mirror stream: the Anthropic fixture's events become Responses
/// events **as they arrive** — the client holds the function-call item with
/// its `call_id` while the fixture is still paused mid-arguments — in the
/// Responses event order, ending with the final snapshot carrying the usage.
#[test]
fn a_streamed_codex_request_is_translated_event_by_event_in_the_responses_order() {
    let fixture = Fixture::start(Answer::AnthropicGatedStream);
    let gateway = start_gateway(
        upstream_from(&provider_serving_only(
            "anthropic-messages",
            &fixture.root_url(),
            "anthroponly",
        )),
        None,
    );

    let mut client = send(
        gateway.address(),
        &responses_request(gateway.token().expose(), &codex_body(true)),
    );

    // Read until the function-call arguments have started, fixture paused.
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
        text_so_far.contains("\"call_id\":\"toolu_fix_A\""),
        "the function-call item opened with the provider's tool_use id before the arguments \
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
    assert_eq!(events[0].1["response"]["id"], "msg_fix");
    assert_eq!(events[3].1["delta"], "Checking.");
    assert_eq!(events[7].1["item"]["type"], "function_call");
    assert_eq!(events[7].1["item"]["call_id"], "toolu_fix_A");
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
        completed["usage"]["input_tokens"], 35,
        "the start's input count survives to the final snapshot with the cached tokens added"
    );
    assert_eq!(completed["usage"]["output_tokens"], 9);
    assert_eq!(
        completed["output"][1]["call_id"], "toolu_fix_A",
        "the final snapshot echoes the complete items"
    );

    // ... and the fixture was asked for a stream.
    let sent = fixture.only_request().json();
    assert_eq!(sent["stream"], true);
}

/// A Codex request the mirror pair cannot carry gets its `400` in the
/// Responses error shape, naming the pair, the field and the reason —
/// nothing opened upstream. Server-side state (`previous_response_id`,
/// `store: true`) and hosted tools are the refusals a real Codex could
/// actually send.
#[test]
fn a_codex_request_the_pair_cannot_carry_is_refused_by_name_and_nothing_opens_upstream() {
    let fixture = Fixture::start(Answer::AnthropicCompletion);
    let gateway = start_gateway(
        upstream_from(&provider_serving_only(
            "anthropic-messages",
            &fixture.root_url(),
            "anthroponly",
        )),
        None,
    );

    let base = |extra: &str| {
        format!(
            r#"{{"model": "gpt-5", "instructions": "{PLANTED_PROMPT}", "input": [{{"role": "user", "content": "hi"}}]{extra}}}"#
        )
    };
    for (extra, field) in [
        (
            r#", "previous_response_id": "resp_0""#,
            "`previous_response_id`",
        ),
        (r#", "store": true"#, "`store`"),
        (r#", "tools": [{"type": "web_search"}]"#, "`tools[0].type`"),
    ] {
        let body = base(extra);
        let response = send_and_read(
            gateway.address(),
            &responses_request(gateway.token().expose(), &body),
        );
        let (head, body) = head_and_body(&response);
        assert!(head.starts_with("HTTP/1.1 400"), "{head}");
        let error: Value = serde_json::from_slice(body).expect("a Responses error document");
        assert_eq!(error["error"]["type"], "invalid_request_error");
        let message = error["error"]["message"].as_str().unwrap();
        assert!(
            message.contains("openai-responses->anthropic-messages"),
            "{message}"
        );
        assert!(message.contains(field), "{message}");
        assert!(
            !message.contains(PLANTED_PROMPT),
            "a refusal names the field and never quotes the request: {message}"
        );
    }
    assert_eq!(
        fixture.connections(),
        0,
        "a refused request opens nothing upstream"
    );
}

// --- a served target is still relayed byte for byte ------------------------------

/// The relay rule, narrowed and not repealed, now on the Responses side: a
/// provider that serves OpenAI Responses natively gets the client's body
/// **untouched** — a body the codec would refuse (`previous_response_id`,
/// `store: true`) and would re-serialise (odd spacing, non-ASCII) — and the
/// client gets the provider's document verbatim, a key the codecs refuse
/// included. A gateway that entered a codec for this served target refuses
/// the body instead, and the fixture sees nothing.
#[test]
fn a_served_responses_target_is_relayed_byte_for_byte_even_though_the_codec_exists() {
    let fixture = Fixture::start(Answer::Relay);
    let gateway = start_gateway(
        upstream_from(&provider_serving_only(
            "openai-responses",
            &fixture.base_url(),
            "responsesonly",
        )),
        None,
    );
    assert_eq!(gateway.served_protocols(), vec!["openai-responses"]);

    let body = "{ \"model\":\"gpt-5\" ,\"input\": \"ünïcödé — 日本語\",   \"previous_response_id\": \"resp_zzz\", \"store\": true }";
    let response = send_and_read(
        gateway.address(),
        &responses_request(gateway.token().expose(), body),
    );

    let received = fixture.only_request();
    assert_eq!(received.target, "/v1/responses");
    assert_eq!(
        received.body,
        body.as_bytes(),
        "a served target's body is every byte the client wrote, in order"
    );
    assert_eq!(
        received.header("content-length"),
        Some(body.len().to_string().as_str())
    );
    assert_eq!(
        received.header("authorization"),
        Some(format!("Bearer {PLANTED_KEY}").as_str())
    );

    let (head, answer) = head_and_body(&response);
    assert!(head.starts_with("HTTP/1.1 200"), "{head}");
    assert!(
        String::from_utf8_lossy(answer).contains("\"planted_marker_the_codecs_refuse\":true"),
        "the provider's document reached the client verbatim: {}",
        String::from_utf8_lossy(answer)
    );
}
