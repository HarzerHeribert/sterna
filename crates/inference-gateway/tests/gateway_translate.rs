//! Phase 56, lines 1948–1950 and 1956: the first translated pair — Claude
//! Code's Anthropic Messages served by an OpenAI-Chat entitlement — end to
//! end against a fixture upstream that speaks **only** `openai-chat` and
//! records what reached it.
//!
//! # Where these tests enter, and why not `glasshouse launch`
//!
//! Line 1956 asks for the pairing to be covered "through the shipped binary
//! against a fixture upstream". The tests below enter at
//! `gateway::start_if_required_with_degrade_sink` — the same door the shipped
//! binary calls, in front of the same accept loop and the same
//! `ingress::serve` every gateway-backed request in the binary goes through
//! — with an [`Upstream`] built by the **production** `profile::gateway_upstream`
//! from a real provider template. Nothing in the translation path is
//! bypassed: the request is placed, decoded, encoded, sent with the
//! provider's credential, and the answer translated back, by the code the
//! binary ships.
//!
//! They do not enter at `glasshouse launch`, and the last test in this file
//! is the witness for why: `profile::apply_gateway` refuses a Claude Code
//! launch whose *serving* backend has no route for `anthropic-messages`
//! (`Refusal::GatewayProtocolUnserved`), before the harness starts and before
//! any request could reach the gateway. That refusal is one link before the
//! seam this package fills, it lives in `profile/`, and it was not this
//! package's to change. When it is lifted — accept a harness protocol the
//! pair table translates to a served one, and bind the session to the served
//! protocol — that witness test fails, and its replacement is the three tests
//! above it driven through `glasshouse launch` the way
//! `tests/gateway_degrade.rs` drives its own.
//!
//! # What is asserted, and against what
//!
//! Every claim about the outbound request is made against bytes the fixture
//! read off the wire with its own parser, never against the gateway's idea
//! of what it sent. Every claim about the inbound answer is made against the
//! bytes a plain `TcpStream` client received. Ids are compared as strings on
//! both sides, because the id is what makes a tool result land on the tool
//! call that asked for it.

mod host;
use host::Ledger;

use std::io::{Read, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use inference_gateway::gateway::translate::TOOL_ERROR_MARKER;
use inference_gateway::gateway::{Gateway, Route, Upstream, UpstreamBackend};
use inference_gateway::provider::{ProtocolSupport, Provider};
use inference_gateway::routing::evidence::Outcome;
use inference_gateway::routing::wire::{Declared, WireProtocol};
use inference_gateway::routing::{AssignedModel, Cost, CredentialId};
use inference_gateway::secret::{EnvironmentSecretStore, Secret, SecretRef, SecretStore};
use serde_json::{Value, json};

/// A planted provider credential, unique to this test binary. Never a real
/// key, and asserted on so that `!contains` has something to bite.
const PLANTED_KEY: &str = "sk-planted-translate-000111222333";
const CREDENTIAL_VAR: &str = "GLASSHOUSE_GATEWAY_TRANSLATE_TEST_KEY";

/// A body that a request header cannot legitimately carry and that no test
/// wants to see anywhere but in the one place it was planted.
const PLANTED_PROMPT: &str = "PLANTED-PROMPT-TEXT-FOR-TRANSLATE-TESTS";

const CLIENT_TIMEOUT: Duration = Duration::from_secs(20);

// --- a fixture that speaks only OpenAI Chat -----------------------------------

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

/// How the fixture answers a `/v1/chat/completions` POST.
#[derive(Clone, Copy)]
enum Answer {
    /// One JSON completion with a text part and two tool calls.
    Completion,
    /// The same as a chunk stream, pausing before the finish chunk until
    /// [`ChatOnlyUpstream::release`] is called.
    GatedStream,
    /// A provider error with an OpenAI-shaped body.
    Error,
    /// A stream of many chunk events, each under `stream::MAX_EVENT_BYTES`,
    /// summing well past `translate::MAX_BODY_BYTES`, and never sending
    /// `[DONE]` — break/gateway-translate #3.
    UnboundedEventCount,
}

/// A canned OpenAI-compatible provider: `POST /v1/chat/completions` is
/// answered in that protocol's shape, and every request is recorded. Any
/// other target is answered with a fixed document carrying `PLANTED_PROMPT`
/// nowhere and a marker key the codecs would refuse — so a relayed request
/// can be told from a translated one by what the client receives.
struct ChatOnlyUpstream {
    address: SocketAddr,
    connections: Arc<AtomicUsize>,
    requests: Arc<Mutex<Vec<RecordedRequest>>>,
    gate: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
    accept: Option<JoinHandle<()>>,
}

impl ChatOnlyUpstream {
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

    /// The base URL an OpenAI-compatible provider declares: with `/v1`, so
    /// the client's `/chat/completions` lands on `/v1/chat/completions`.
    fn base_url(&self) -> String {
        format!("http://{}/v1", self.address)
    }

    /// A base URL with no path, for a route that is *not* OpenAI Chat.
    fn root_url(&self) -> String {
        format!("http://{}", self.address)
    }

    fn connections(&self) -> usize {
        self.connections.load(Ordering::Relaxed)
    }

    fn requests(&self) -> Vec<RecordedRequest> {
        self.requests.lock().unwrap().clone()
    }

    fn only_request(&self) -> RecordedRequest {
        let requests = self.requests();
        assert_eq!(requests.len(), 1, "exactly one request at the fixture");
        requests.into_iter().next().unwrap()
    }

    /// Let a gated stream finish.
    fn release(&self) {
        self.gate.store(true, Ordering::Relaxed);
    }
}

impl Drop for ChatOnlyUpstream {
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
    if !request.target.starts_with("/v1/chat/completions") {
        // Not the chat endpoint: answer as an Anthropic-ish upstream would,
        // with a document the codecs would refuse (an unknown key), so a
        // client that receives it verbatim proves nothing decoded it.
        let body = r#"{"type":"message","id":"msg_relayed","role":"assistant","model":"m","content":[{"type":"text","text":"relayed"}],"stop_reason":"end_turn","stop_sequence":null,"usage":{"input_tokens":1,"output_tokens":1},"planted_marker_the_codecs_refuse":true}"#;
        write_document(&mut stream, "200 OK", body);
        return;
    }
    match answer {
        Answer::Completion => write_document(
            &mut stream,
            "200 OK",
            &json!({
                "id": "chatcmpl-fixture",
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
                "usage": {"prompt_tokens": 40, "completion_tokens": 12, "total_tokens": 52, "prompt_tokens_details": {"cached_tokens": 8}}
            })
            .to_string(),
        ),
        Answer::Error => write_document(
            &mut stream,
            "429 Too Many Requests",
            &json!({"error": {"message": "fixture says slow down", "type": "rate_limit_error", "code": null}}).to_string(),
        ),
        Answer::GatedStream => {
            let chunk = |choices: Value, extra: &str| {
                format!(
                    "data: {{\"id\":\"chatcmpl-fixture\",\"object\":\"chat.completion.chunk\",\"created\":1,\"model\":\"fixture-model\",\"choices\":{choices}{extra}}}\n\n"
                )
            };
            let head = "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ntransfer-encoding: chunked\r\n\r\n";
            let _ = stream.write_all(head.as_bytes());
            let before = [
                chunk(json!([{"index": 0, "delta": {"role": "assistant", "content": ""}, "finish_reason": null}]), ""),
                chunk(json!([{"index": 0, "delta": {"content": "Checking."}, "finish_reason": null}]), ""),
                chunk(json!([{"index": 0, "delta": {"tool_calls": [{"index": 0, "id": "call_fix_A", "type": "function", "function": {"name": "Bash", "arguments": ""}}]}, "finish_reason": null}]), ""),
                chunk(json!([{"index": 0, "delta": {"tool_calls": [{"index": 0, "function": {"arguments": "{\"command\""}}]}, "finish_reason": null}]), ""),
            ];
            for event in before {
                write_chunk(&mut stream, event.as_bytes());
            }
            // Pause here until the test has seen those events arrive at the
            // client: what proves the stream was translated as a stream.
            let deadline = Instant::now() + Duration::from_secs(20);
            while !gate.load(Ordering::Relaxed) && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(5));
            }
            let after = [
                chunk(json!([{"index": 0, "delta": {"tool_calls": [{"index": 0, "function": {"arguments": ": \"ls\"}"}}]}, "finish_reason": null}]), ""),
                chunk(json!([{"index": 0, "delta": {}, "finish_reason": "tool_calls"}]), ""),
                chunk(json!([]), ",\"usage\":{\"prompt_tokens\":40,\"completion_tokens\":12,\"total_tokens\":52}"),
                "data: [DONE]\n\n".to_owned(),
            ];
            for event in after {
                write_chunk(&mut stream, event.as_bytes());
            }
            let _ = stream.write_all(b"0\r\n\r\n");
            let _ = stream.flush();
            let _ = stream.shutdown(std::net::Shutdown::Write);
        }
        Answer::UnboundedEventCount => {
            let head = "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ntransfer-encoding: chunked\r\n\r\n";
            let _ = stream.write_all(head.as_bytes());
            // Each event is comfortably under `stream::MAX_EVENT_BYTES` (one
            // MiB) on its own; forty of them sum well past
            // `translate::MAX_BODY_BYTES` (32 MiB), and `[DONE]` never
            // arrives — the shape break/gateway-translate #3 describes as
            // unbounded. A patched gateway refuses partway through this
            // loop; an unpatched one holds all forty and then some.
            let payload = "x".repeat(1_000_000);
            for _ in 0..40 {
                let chunk = format!(
                    "data: {{\"id\":\"chatcmpl-fixture\",\"object\":\"chat.completion.chunk\",\"created\":1,\"model\":\"fixture-model\",\"choices\":[{{\"index\":0,\"delta\":{{\"content\":\"{payload}\"}},\"finish_reason\":null}}]}}\n\n"
                );
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

/// An OpenAI-compatible provider template pointed at the fixture, with the
/// planted credential in this file's own variable — the same
/// `[providers.x] template = "openai-compatible"` a user writes, in the
/// `Provider` value `config` would produce from it.
fn chat_only_provider(fixture: &ChatOnlyUpstream) -> Provider {
    let mut provider = inference_gateway::provider::templates()
        .into_iter()
        .find(|provider| provider.name == "openai-compatible")
        .expect("the openai-compatible template exists");
    provider.name = "chat".to_owned();
    assert_eq!(
        provider.protocols.len(),
        1,
        "the openai-compatible template serves exactly one protocol"
    );
    provider.protocols[0].base_url = fixture.base_url();
    provider.credential_env = vec![CREDENTIAL_VAR.to_owned()];
    provider
}

/// Every set→resolve→remove of [`CREDENTIAL_VAR`] happens under this lock.
///
/// The tests in this binary run in parallel and the environment is process
/// state: without the lock, one test's `remove_var` lands between another's
/// `set_var` and its resolve, and the resolve finds nothing — measured, not
/// theoretical: three of six tests failed exactly there the first time this
/// file ran on a loaded machine (`blast-radius.sh`, 2026-08-31), after
/// passing repeatedly on a quiet one.
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
    upstream.expect("one chat-only provider with a resolvable credential builds an upstream")
}

/// A hand-built backend with one route per `(protocol, targets, base URL)`
/// — the refused-pair and byte-for-byte cases.
fn upstream_serving(routes: &[(&str, &'static [&'static str], &str)]) -> Upstream {
    let _guard = ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    // SAFETY: as in `upstream_from` — under `ENV_LOCK` for the whole window.
    unsafe {
        std::env::set_var(CREDENTIAL_VAR, PLANTED_KEY);
    }
    let credential: Secret = EnvironmentSecretStore::new()
        .resolve(&SecretRef::Environment {
            var: CREDENTIAL_VAR.to_owned(),
        })
        .expect("just set");
    unsafe {
        std::env::remove_var(CREDENTIAL_VAR);
    }
    let backend = UpstreamBackend::new(
        "fixture".to_owned(),
        routes
            .iter()
            .map(|(protocol, targets, base_url)| {
                Route::new((*protocol).to_owned(), targets, base_url)
            })
            .collect(),
        credential,
        CredentialId::new(
            "fixture",
            SecretRef::Environment {
                var: CREDENTIAL_VAR.to_owned(),
            },
        ),
        Cost::Metered,
    )
    .expect("a loopback URL is absolute and the planted key is header-safe");
    Upstream::with_failover(vec![backend]).expect("one backend")
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

/// The door the shipped binary calls, with a gateway-backed Claude Code
/// profile and the given upstream.
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

// --- a Claude Code client ------------------------------------------------------

/// The request Claude Code sends after one tool round: a system prompt, a
/// tool definition, the assistant's tool call, the user's (erroring) result
/// carrying the call's id, and a follow-up. `stream` as given.
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
                event.expect("every Anthropic event is named"),
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

// --- (a) and (b): a document, both ways -----------------------------------------

/// Lines 1948, 1949 and 1950 for the first pair, as one exchange: the
/// fixture — which speaks only OpenAI Chat — receives a well-formed
/// `/v1/chat/completions` body with the tool definition, the prior tool call
/// and its erroring result translated and their ids intact; the client
/// receives an Anthropic Messages response whose `tool_use` ids are the
/// fixture's `tool_calls[].id` verbatim; and the exchange is recorded under
/// the pair's own name with the provider's exact usage.
#[test]
fn a_claude_code_request_is_translated_to_chat_completions_and_the_answer_back_with_ids_preserved()
{
    let fixture = ChatOnlyUpstream::start(Answer::Completion);
    let ledger = ledger_fixture();
    let gateway = start_gateway(
        upstream_from(&chat_only_provider(&fixture)),
        Some(Arc::clone(&ledger.ledger)),
    );
    assert_eq!(
        gateway.served_protocols(),
        vec!["openai-chat"],
        "the fixture-backed provider must serve OpenAI Chat and nothing else, or the request \
         below is relayed rather than translated"
    );
    // The assignment `profile::apply_gateway` would record once it accepts a
    // translated pairing: the harness, bound to the protocol the backend
    // *serves*. Bound here only so the exchange's evidence row has an
    // assignment to belong to; see this file's header for why the production
    // binder cannot yet be reached.
    gateway.routing().bind(
        "claude-code",
        "openai-chat",
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
    assert_eq!(received.target, "/v1/chat/completions");
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
    assert_eq!(
        received.header("content-length"),
        Some(received.body.len().to_string().as_str())
    );
    assert!(
        !received
            .headers
            .iter()
            .any(|(_, value)| value.contains(gateway.token().expose())),
        "the gateway's own token never leaves the process"
    );
    let sent = received.json();
    assert_eq!(sent["model"], "claude-x");
    assert_eq!(sent["max_tokens"], 4096);
    assert_eq!(sent["stream"], Value::Null);
    assert_eq!(sent["user"], "user_abc");
    assert_eq!(sent["tool_choice"], "auto");
    let messages = sent["messages"].as_array().expect("messages");
    assert_eq!(messages[0]["role"], "system");
    assert_eq!(messages[0]["content"], PLANTED_PROMPT);
    assert_eq!(
        messages[1],
        json!({"role": "user", "content": "List the files."})
    );
    assert_eq!(messages[2]["role"], "assistant");
    assert_eq!(messages[2]["content"], "Sure.");
    assert_eq!(
        messages[2]["tool_calls"],
        json!([{"id": "call_prior_1", "type": "function", "function": {"name": "Bash", "arguments": "{\"command\":\"ls /nope\"}"}}])
    );
    assert_eq!(messages[3]["role"], "tool");
    assert_eq!(
        messages[3]["tool_call_id"], "call_prior_1",
        "the tool result answers the call by the same id it was issued under"
    );
    assert_eq!(
        messages[3]["content"],
        format!("{TOOL_ERROR_MARKER}\nls: cannot access '/nope'"),
        "an erroring result is carried, labelled, in the only channel OpenAI Chat has"
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
    assert_eq!(tools[0]["function"]["description"], "Run a shell command");
    assert_eq!(
        tools[0]["function"]["parameters"],
        json!({"type": "object", "properties": {"command": {"type": "string"}}, "required": ["command"]})
    );

    // (b) what the client received.
    let answer: Value = serde_json::from_slice(body).expect("an Anthropic JSON document");
    assert_eq!(answer["type"], "message");
    assert_eq!(answer["role"], "assistant");
    assert_eq!(answer["id"], "chatcmpl-fixture");
    assert_eq!(answer["model"], "fixture-model");
    assert_eq!(answer["stop_reason"], "tool_use");
    let content = answer["content"].as_array().expect("content blocks");
    assert_eq!(content[0], json!({"type": "text", "text": "Checking."}));
    assert_eq!(
        content[1],
        json!({"type": "tool_use", "id": "call_fix_A", "name": "Bash", "input": {"command": "ls"}}),
        "the tool_use id is the fixture's tool_call id, verbatim"
    );
    assert_eq!(
        content[2],
        json!({"type": "tool_use", "id": "call_fix_B", "name": "Read", "input": {"file_path": "/tmp/x"}})
    );
    assert_eq!(content.len(), 3, "two parallel tool calls, both delivered");
    assert_eq!(
        answer["usage"],
        json!({"input_tokens": 32, "output_tokens": 12, "cache_read_input_tokens": 8}),
        "prompt_tokens includes the cached ones; Anthropic's input_tokens does not"
    );
    assert!(
        !String::from_utf8_lossy(&response).contains(PLANTED_KEY),
        "the provider credential never appears in a translated response"
    );

    // Recorded under the pair's own name, with the provider's exact usage.
    let rows = wait_for_row(
        &ledger.ledger,
        "chat",
        "claude-x",
        Some("anthropic-messages->openai-chat"),
        Some("claude-code"),
    );
    assert_eq!(
        rows.len(),
        1,
        "one routing observation, with `route` naming the translated pair"
    );
    assert_eq!(rows[0].outcome, Some(Outcome::Succeeded));
    assert_eq!(rows[0].input_tokens, Some(32));
    assert_eq!(rows[0].output_tokens, Some(12));
    assert_eq!(rows[0].cached_input_tokens, Some(8));
    assert!(rows[0].first_byte_at_unix.is_some());
}

// --- (c): a stream, as a stream -------------------------------------------------

/// The same pair with `stream: true`: the fixture's chunks become Anthropic's
/// events **as they arrive** — the client holds the tool block's start while
/// the fixture is still paused before its finish chunk — in Anthropic's
/// order, and the response is chunk-terminated.
#[test]
fn a_streamed_request_is_translated_event_by_event_in_anthropics_order_and_terminated() {
    let fixture = ChatOnlyUpstream::start(Answer::GatedStream);
    let gateway = start_gateway(upstream_from(&chat_only_provider(&fixture)), None);

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
        "the tool block started with the fixture's id before the arguments finished: {text_so_far}"
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
    assert!(head.contains("transfer-encoding: chunked"), "{head}");
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
    assert_eq!(events[0].1["message"]["id"], "chatcmpl-fixture");
    assert_eq!(events[0].1["message"]["role"], "assistant");
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

    // ... and the fixture was asked for a stream with usage included.
    let sent = fixture.only_request().json();
    assert_eq!(sent["stream"], true);
    assert_eq!(sent["stream_options"], json!({"include_usage": true}));
}

// --- (d): refused by name, nothing opened upstream ----------------------------

/// A request the pair cannot carry is answered with a `400` in Claude Code's
/// own error shape, naming the pair, the field and the reason — and the
/// fixture never sees a connection. Then a target whose every pair the table
/// refuses gets the `404` naming the pair and the table's reason, and the
/// endpoint rule is named too.
#[test]
fn a_request_the_pair_cannot_carry_is_refused_by_name_and_nothing_is_opened_upstream() {
    let fixture = ChatOnlyUpstream::start(Answer::Completion);
    let gateway = start_gateway(upstream_from(&chat_only_provider(&fixture)), None);

    // `cache_control` moved from refused to carried (2014); `thinking` moved
    // from refused to carried too (GH-EFFORT-CARRY) — this pair's
    // still-refused field is now `service_tier`, which has no OpenAI Chat
    // equivalent at all.
    let with_service_tier = json!({
        "model": "claude-x",
        "max_tokens": 10,
        "system": [{"type": "text", "text": PLANTED_PROMPT}],
        "messages": [{"role": "user", "content": "hi"}],
        "service_tier": "auto"
    })
    .to_string();
    let response = send_and_read(
        gateway.address(),
        &messages_request(gateway.token().expose(), &with_service_tier),
    );
    let (head, body) = head_and_body(&response);
    assert!(head.starts_with("HTTP/1.1 400"), "{head}");
    let error: Value = serde_json::from_slice(body).expect("an Anthropic error document");
    assert_eq!(error["type"], "error");
    assert_eq!(error["error"]["type"], "invalid_request_error");
    let message = error["error"]["message"].as_str().unwrap();
    assert!(
        message.contains("anthropic-messages->openai-chat"),
        "{message}"
    );
    assert!(message.contains("`service_tier`"), "{message}");
    assert!(message.contains("no equivalent service tier"), "{message}");
    assert!(
        !message.contains(PLANTED_PROMPT),
        "a refusal names the field and never quotes the request: {message}"
    );
    assert_eq!(
        fixture.connections(),
        0,
        "a refused request opens nothing upstream"
    );

    // service_tier, top_k, and a future unknown field: each refused by its
    // name.
    for (extra, field) in [
        (r#""service_tier": "auto""#, "`service_tier`"),
        (r#""top_k": 3"#, "`top_k`"),
        (r#""some_future_field": 1"#, "`some_future_field`"),
    ] {
        let body = format!(
            r#"{{"model": "m", "max_tokens": 1, "messages": [{{"role": "user", "content": "x"}}], {extra}}}"#
        );
        let response = send_and_read(
            gateway.address(),
            &messages_request(gateway.token().expose(), &body),
        );
        let (head, body) = head_and_body(&response);
        assert!(head.starts_with("HTTP/1.1 400"), "{head}");
        let message = String::from_utf8_lossy(body).into_owned();
        assert!(message.contains(field), "{message}");
    }
    assert_eq!(fixture.connections(), 0);

    // The endpoint rule: `/v1/messages/count_tokens` has no chat equivalent.
    let response = send_and_read(
        gateway.address(),
        format!(
            "POST /v1/messages/count_tokens HTTP/1.1\r\nHost: 127.0.0.1\r\nAuthorization: Bearer {}\r\nContent-Length: 2\r\n\r\n{{}}",
            gateway.token().expose()
        )
        .as_bytes(),
    );
    let (head, body) = head_and_body(&response);
    assert!(head.starts_with("HTTP/1.1 404"), "{head}");
    assert!(
        String::from_utf8_lossy(body).contains("only its `/messages` endpoint is translated"),
        "{}",
        String::from_utf8_lossy(body)
    );
    assert_eq!(fixture.connections(), 0);

    // The reverse pair — an OpenAI Chat client at an Anthropic-only provider
    // — has both codecs and is still refused by name, because it has no
    // end-to-end test of its own yet (1956). A table that marked it
    // supported would translate this request and the fixture would see it.
    let anthropic_only = ChatOnlyUpstream::start(Answer::Completion);
    let gateway = start_gateway(
        upstream_serving(&[(
            "anthropic-messages",
            &["/messages"],
            &anthropic_only.root_url(),
        )]),
        None,
    );
    let chat_body =
        json!({"model": "m", "messages": [{"role": "user", "content": "hi"}]}).to_string();
    let response = send_and_read(
        gateway.address(),
        format!(
            "POST /v1/chat/completions HTTP/1.1\r\nHost: 127.0.0.1\r\nAuthorization: Bearer {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{chat_body}",
            gateway.token().expose(),
            chat_body.len()
        )
        .as_bytes(),
    );
    let (head, body) = head_and_body(&response);
    assert!(head.starts_with("HTTP/1.1 404"), "{head}");
    let message = String::from_utf8_lossy(body).into_owned();
    assert!(
        message.contains("openai-chat->anthropic-messages"),
        "{message}"
    );
    assert!(message.contains("1956"), "{message}");
    assert_eq!(
        anthropic_only.connections(),
        0,
        "a refused pair opens nothing upstream"
    );
}

/// The provider's own error travels back in Claude Code's error shape with
/// the provider's status and message — the status is what routing reads.
#[test]
fn a_provider_error_is_delivered_in_the_harnesss_error_shape_with_the_providers_status() {
    let fixture = ChatOnlyUpstream::start(Answer::Error);
    let gateway = start_gateway(upstream_from(&chat_only_provider(&fixture)), None);
    let response = send_and_read(
        gateway.address(),
        &messages_request(gateway.token().expose(), &claude_code_body(false)),
    );
    let (head, body) = head_and_body(&response);
    assert!(head.starts_with("HTTP/1.1 429"), "{head}");
    let error: Value = serde_json::from_slice(body).expect("an Anthropic error document");
    assert_eq!(error["type"], "error");
    assert_eq!(error["error"]["type"], "rate_limit_error");
    assert_eq!(error["error"]["message"], "fixture says slow down");
    assert_eq!(fixture.connections(), 1);
}

// --- break/gateway-translate #3, #4, #6 -------------------------------------------

/// break/gateway-translate #3: the harness asks for a document
/// (`claude_code_body(false)`) and the provider streams anyway — the branch
/// that gathers a stream into the document it delivered. A provider that
/// never stops streaming, and never sends a byte the single-event cap would
/// refuse, must still be bounded in how much it can make the gateway hold.
#[test]
fn a_provider_stream_gathered_into_a_document_is_bounded_in_total_even_though_no_single_event_is() {
    let fixture = ChatOnlyUpstream::start(Answer::UnboundedEventCount);
    let gateway = start_gateway(upstream_from(&chat_only_provider(&fixture)), None);

    let response = send_and_read(
        gateway.address(),
        &messages_request(gateway.token().expose(), &claude_code_body(false)),
    );
    let (head, body) = head_and_body(&response);
    assert!(
        head.starts_with("HTTP/1.1 502"),
        "an unbounded provider stream must be refused, not accumulated whole: {head}"
    );
    let message = String::from_utf8_lossy(body).into_owned();
    assert!(
        message.contains("exceeded the size"),
        "the refusal names the size bound: {message}"
    );
}

/// break/gateway-translate #4: a translated refusal must be readable by the
/// client it was written for, even when that client is still mid-upload —
/// the 413 case, which by construction fires as soon as the declared
/// `content-length` is read, before any of the body has to have arrived.
#[test]
fn a_413_refusal_is_readable_by_a_client_still_uploading_its_declared_body() {
    let fixture = ChatOnlyUpstream::start(Answer::Completion);
    let gateway = start_gateway(upstream_from(&chat_only_provider(&fixture)), None);

    let declared_len = inference_gateway::gateway::translate::MAX_BODY_BYTES + 1024;
    let head = format!(
        "POST /v1/messages?beta=true HTTP/1.1\r\n\
         Host: 127.0.0.1\r\n\
         Authorization: Bearer {}\r\n\
         Content-Type: application/json\r\n\
         Anthropic-Version: 2023-06-01\r\n\
         Content-Length: {declared_len}\r\n\
         \r\n",
        gateway.token().expose()
    );
    let mut client =
        TcpStream::connect(gateway.address()).expect("the gateway accepts connections");
    client
        .set_read_timeout(Some(CLIENT_TIMEOUT))
        .expect("a non-zero timeout is valid");
    client
        .write_all(head.as_bytes())
        .expect("the gateway reads the request head");
    client.flush().expect("flush");

    // Trickle a slice of the declared body slowly: a real client mid-upload
    // when the 413 — and, on the unpatched tree, the premature
    // `shutdown(Shutdown::Both)` — happens.
    let chunk = vec![b'x'; 64 * 1024];
    let mut upload_failed = false;
    for _ in 0..4 {
        std::thread::sleep(Duration::from_millis(30));
        if client.write_all(&chunk).is_err() {
            upload_failed = true;
            break;
        }
    }
    let _ = client.flush();

    let mut received = Vec::new();
    let _ = client.read_to_end(&mut received);
    assert!(
        !received.is_empty() && !upload_failed,
        "the client must read the 413 it was written rather than a reset connection; \
         upload_failed={upload_failed}, received {} bytes: {:?}",
        received.len(),
        String::from_utf8_lossy(&received)
    );
    let (head, body) = head_and_body(&received);
    assert!(head.starts_with("HTTP/1.1 413"), "{head}");
    assert!(
        String::from_utf8_lossy(body).contains("exceeds the size"),
        "{}",
        String::from_utf8_lossy(body)
    );
    assert_eq!(
        fixture.connections(),
        0,
        "a request refused for size never opens anything upstream"
    );
}

// --- (e): a served target is still relayed byte for byte -------------------------

/// The relay rule, narrowed and not repealed: a provider that serves
/// Anthropic Messages natively — **and** OpenAI Chat, so a supported pair and
/// both codecs are right there — gets the client's body **untouched**: a body
/// the codec would carry rather than send as given (`cache_control`,
/// `thinking`) and would re-serialise (odd spacing, non-ASCII), with its
/// Anthropic headers, at the Anthropic route; and the client gets the
/// provider's document verbatim, a key the codecs refuse included. A gateway
/// that entered a codec for a
/// served target sends the fixture a `/v1/chat/completions` body instead,
/// and fails on the first byte comparison.
#[test]
fn a_target_the_provider_serves_natively_is_relayed_byte_for_byte_even_though_a_codec_exists() {
    let fixture = ChatOnlyUpstream::start(Answer::Completion);
    let gateway = start_gateway(
        upstream_serving(&[
            ("anthropic-messages", &["/messages"], &fixture.root_url()),
            ("openai-chat", &["/chat/completions"], &fixture.base_url()),
        ]),
        None,
    );
    assert_eq!(
        gateway.served_protocols(),
        vec!["anthropic-messages", "openai-chat"]
    );

    let body = "{ \"model\":\"claude-x\" ,\"max_tokens\": 10, \"system\": [{\"type\":\"text\",\"text\":\"ünïcödé — 日本語\",\"cache_control\":{\"type\":\"ephemeral\"}}],   \"messages\":[{\"role\":\"user\",\"content\":\"hi\"}] , \"thinking\": {\"type\": \"enabled\", \"budget_tokens\": 1024} }";
    let response = send_and_read(
        gateway.address(),
        &messages_request(gateway.token().expose(), body),
    );

    let received = fixture.only_request();
    assert_eq!(received.target, "/v1/messages?beta=true");
    assert_eq!(
        received.body,
        body.as_bytes(),
        "a served target's body is every byte the client wrote, in order"
    );
    assert_eq!(
        received.header("content-length"),
        Some(body.len().to_string().as_str())
    );
    assert_eq!(received.header("anthropic-version"), Some("2023-06-01"));
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

// --- (f): the embedded gateway serves the ingress the standalone binary does -----

/// `POST /v1/systemone` to the *embedded* gateway.
fn systemone_request(token: &str, body: &str) -> Vec<u8> {
    format!(
        "POST /v1/systemone HTTP/1.1\r\n\
         Host: 127.0.0.1\r\n\
         Authorization: Bearer {token}\r\n\
         Content-Type: application/json\r\n\
         Content-Length: {}\r\n\
         \r\n\
         {body}",
        body.len()
    )
    .into_bytes()
}

/// A provider declaring only `TypesafeSystemOne`, at the fixture's root URL —
/// the shape `chat_only_provider` uses for `openai-chat`, built directly
/// rather than from a template because no `typesafe` provider template
/// exists in this crate yet.
fn typesafe_provider(fixture: &ChatOnlyUpstream) -> Provider {
    Provider {
        name: "typesafe".to_owned(),
        protocols: vec![ProtocolSupport {
            protocol: WireProtocol::TypesafeSystemOne,
            base_url: fixture.root_url(),
            streaming: Declared::Unverified,
            tool_calls: Declared::Unverified,
            reasoning: Declared::Unverified,
        }],
        model_list_endpoint: Declared::Unverified,
        usage_telemetry: Declared::Unverified,
        credential_env: vec![CREDENTIAL_VAR.to_owned()],
        headers: Vec::new(),
    }
}

/// `GH-GATEWAY-INGRESS-ONCE`, map line 2630: `profile::gateway_upstream` —
/// the production builder every Glasshouse-started gateway is built with —
/// now imports the gateway crate's own `GATEWAY_INGRESS_PROTOCOLS` and
/// `gateway_routes`, so a catalogue with a `typesafe` provider gets a route
/// for `TypesafeSystemOne` exactly as the standalone binary's own
/// `pool::gateway_upstream` would. The fixture shape mirrors the gateway
/// crate's own conformance test,
/// `a_systemone_request_reaches_the_typesafe_account_while_messages_stay_with_the_bound_one`:
/// a provider serving only `TypesafeSystemOne`, reached with its own
/// credential, the request relayed byte-for-byte because no codec exists for
/// this protocol.
#[test]
fn a_typesafe_provider_built_through_profile_gateway_upstream_serves_systemone() {
    let fixture = ChatOnlyUpstream::start(Answer::Completion);
    let gateway = start_gateway(upstream_from(&typesafe_provider(&fixture)), None);
    assert_eq!(
        gateway.served_protocols(),
        vec!["typesafe-systemone"],
        "this test proves nothing unless the embedded gateway really serves it"
    );

    let decision_body = "{\"state\":{},\"model\":\"jev-latest\",\"questions\":{}}";
    let response = send_and_read(
        gateway.address(),
        &systemone_request(gateway.token().expose(), decision_body),
    );
    let (head, _body) = head_and_body(&response);
    assert!(head.starts_with("HTTP/1.1 200"), "{head}");

    let received = fixture.only_request();
    assert_eq!(
        received.target, "/v1/systemone",
        "a served target is relayed under its own path, not rewritten"
    );
    assert_eq!(received.body, decision_body.as_bytes());
    assert_eq!(
        received.header("authorization"),
        Some(format!("Bearer {PLANTED_KEY}").as_str()),
        "the typesafe provider's own credential attaches"
    );
    assert!(
        !received
            .headers
            .iter()
            .any(|(_, value)| value.contains(gateway.token().expose())),
        "the gateway's own token never leaves the process"
    );
}
