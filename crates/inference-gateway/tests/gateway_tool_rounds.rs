//! `GH-TOOL-ROUNDS-ON-TRANSLATED` (map lines 1334's last two quantities and
//! 1350): `tool_rounds` and `repairs`, counted as the translate seam decodes
//! the response into canonical events and the request into canonical
//! blocks — through a real socket and a real [`Gateway`], the same door
//! `tests/gateway_first_events.rs` enters, and this file's fixture is its
//! sibling.
//!
//! Four rows: (a) a translated stream whose response carries two tool-use
//! blocks and whose request carries one `is_error: true` tool result; (b) a
//! translated document with one tool-use block and no error result; (c) a
//! translated stream with no tool use at all; (d) a relayed exchange whose
//! body carries a real tool call and whose request carries a real error
//! result, proving nothing is invented for either on that path.

mod host;
use host::Ledger;

use std::io::{Read, Write};
use std::net::{Ipv4Addr, Shutdown, SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use inference_gateway::gateway::{Gateway, Route, Upstream, UpstreamBackend};
use inference_gateway::routing::evidence::Outcome;
use inference_gateway::routing::{AssignedModel, Cost, CredentialId};
use inference_gateway::secret::{EnvironmentSecretStore, SecretRef, SecretStore};
use serde_json::{Value, json};

/// A planted provider credential, unique to this test binary. Never a real
/// key.
const PLANTED_KEY: &str = "sk-planted-tool-rounds-000111222333";
const CREDENTIAL_VAR: &str = "GLASSHOUSE_GATEWAY_TOOL_ROUNDS_TEST_KEY";

const CLIENT_TIMEOUT: Duration = Duration::from_secs(20);

/// Every set→resolve→remove of [`CREDENTIAL_VAR`] happens under this lock —
/// the environment is process state shared by every test in this binary.
static ENV_LOCK: Mutex<()> = Mutex::new(());

// --- reading a request off a raw socket, discarding it -----------------------------

fn read_request(stream: &mut TcpStream) {
    let mut head = Vec::new();
    let mut byte = [0u8; 1];
    while !head.ends_with(b"\r\n\r\n") {
        match stream.read(&mut byte) {
            Ok(0) | Err(_) => return,
            Ok(_) => head.push(byte[0]),
        }
        if head.len() > 64 * 1024 {
            return;
        }
    }
    let text = String::from_utf8_lossy(&head);
    let mut content_length = 0usize;
    for line in text.split("\r\n") {
        if let Some((name, value)) = line.split_once(':')
            && name.trim().eq_ignore_ascii_case("content-length")
        {
            content_length = value.trim().parse().unwrap_or(0);
        }
    }
    let mut body = vec![0u8; content_length];
    if content_length > 0 {
        let _ = stream.read_exact(&mut body);
    }
}

// --- a one-shot document fixture: answers with a fixed body, no streaming ----------

struct DocumentUpstream {
    address: SocketAddr,
    stop: Arc<AtomicBool>,
    accept: Option<JoinHandle<()>>,
}

impl DocumentUpstream {
    fn answering(body: String) -> Self {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("loopback is bindable");
        let address = listener.local_addr().expect("bound");
        listener.set_nonblocking(true).expect("polling mode");
        let stop = Arc::new(AtomicBool::new(false));
        let accept = std::thread::spawn({
            let stop = Arc::clone(&stop);
            move || {
                while !stop.load(Ordering::Relaxed) {
                    match listener.accept() {
                        Ok((stream, _)) => {
                            let body = body.clone();
                            std::thread::spawn(move || serve_document(stream, &body));
                        }
                        Err(_) => std::thread::sleep(Duration::from_millis(10)),
                    }
                }
            }
        });
        Self {
            address,
            stop,
            accept: Some(accept),
        }
    }

    fn openai_base_url(&self) -> String {
        format!("http://{}/v1", self.address)
    }

    fn root_url(&self) -> String {
        format!("http://{}", self.address)
    }
}

impl Drop for DocumentUpstream {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(accept) = self.accept.take() {
            let _ = accept.join();
        }
    }
}

fn serve_document(mut stream: TcpStream, body: &str) {
    stream.set_nonblocking(false).expect("blocking mode");
    let _ = stream.set_read_timeout(Some(Duration::from_secs(60)));
    read_request(&mut stream);
    let _ = write!(
        stream,
        "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\r\n{body}",
        body.len()
    );
    let _ = stream.flush();
    let _ = stream.shutdown(Shutdown::Write);
}

// --- a streaming fixture: writes chat-completion chunks in order -------------------

/// Which chunks [`serve_stream`] writes, after the head and the assistant
/// role's opening chunk every script sends first.
#[derive(Clone, Copy)]
enum Script {
    /// (a): two separate tool-use blocks, the same shape
    /// `openai_chat::tests` scripts for `ChunkDecoder`'s own two-call test —
    /// `call_A` (`Bash`) fully at index 0, then `call_B` (`Read`) at index 1.
    TwoToolCalls,
    /// (c): real text and no tool call at all.
    TextOnlyNoToolCall,
}

struct StreamUpstream {
    address: SocketAddr,
    stop: Arc<AtomicBool>,
    accept: Option<JoinHandle<()>>,
}

impl StreamUpstream {
    fn start(script: Script) -> Self {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("loopback is bindable");
        let address = listener.local_addr().expect("bound");
        listener.set_nonblocking(true).expect("polling mode");
        let stop = Arc::new(AtomicBool::new(false));
        let accept = std::thread::spawn({
            let stop = Arc::clone(&stop);
            move || {
                while !stop.load(Ordering::Relaxed) {
                    match listener.accept() {
                        Ok((stream, _)) => {
                            std::thread::spawn(move || serve_stream(stream, script));
                        }
                        Err(_) => std::thread::sleep(Duration::from_millis(10)),
                    }
                }
            }
        });
        Self {
            address,
            stop,
            accept: Some(accept),
        }
    }

    fn openai_base_url(&self) -> String {
        format!("http://{}/v1", self.address)
    }
}

impl Drop for StreamUpstream {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(accept) = self.accept.take() {
            let _ = accept.join();
        }
    }
}

fn write_chunk(stream: &mut TcpStream, bytes: &[u8]) {
    let mut framed = format!("{:x}\r\n", bytes.len()).into_bytes();
    framed.extend_from_slice(bytes);
    framed.extend_from_slice(b"\r\n");
    let _ = stream.write_all(&framed);
    let _ = stream.flush();
}

fn chat_chunk(delta: Value, finish_reason: Value) -> String {
    format!(
        "data: {}\n\n",
        json!({
            "id": "chatcmpl-fixture",
            "object": "chat.completion.chunk",
            "created": 1,
            "model": "fixture-model",
            "choices": [{"index": 0, "delta": delta, "finish_reason": finish_reason, "logprobs": null}],
        })
    )
}

fn chat_usage_chunk() -> String {
    format!(
        "data: {}\n\n",
        json!({
            "id": "chatcmpl-fixture",
            "object": "chat.completion.chunk",
            "created": 1,
            "model": "fixture-model",
            "choices": [],
            "usage": {"prompt_tokens": 40, "completion_tokens": 12, "total_tokens": 52},
        })
    )
}

fn serve_stream(mut stream: TcpStream, script: Script) {
    stream.set_nonblocking(false).expect("blocking mode");
    let _ = stream.set_read_timeout(Some(Duration::from_secs(60)));
    read_request(&mut stream);
    let head =
        "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ntransfer-encoding: chunked\r\n\r\n";
    let _ = stream.write_all(head.as_bytes());

    write_chunk(
        &mut stream,
        chat_chunk(json!({"role": "assistant", "content": ""}), Value::Null).as_bytes(),
    );

    match script {
        Script::TwoToolCalls => {
            write_chunk(
                &mut stream,
                chat_chunk(
                    json!({"tool_calls": [{"index": 0, "id": "call_A", "type": "function", "function": {"name": "Bash", "arguments": ""}}]}),
                    Value::Null,
                )
                .as_bytes(),
            );
            write_chunk(
                &mut stream,
                chat_chunk(
                    json!({"tool_calls": [{"index": 0, "function": {"arguments": "{\"command\": \"ls\"}"}}]}),
                    Value::Null,
                )
                .as_bytes(),
            );
            write_chunk(
                &mut stream,
                chat_chunk(
                    json!({"tool_calls": [{"index": 1, "id": "call_B", "type": "function", "function": {"name": "Read", "arguments": "{\"file_path\": \"x\"}"}}]}),
                    Value::Null,
                )
                .as_bytes(),
            );
            write_chunk(
                &mut stream,
                chat_chunk(json!({}), json!("tool_calls")).as_bytes(),
            );
        }
        Script::TextOnlyNoToolCall => {
            write_chunk(
                &mut stream,
                chat_chunk(json!({"content": "Checking."}), Value::Null).as_bytes(),
            );
            write_chunk(&mut stream, chat_chunk(json!({}), json!("stop")).as_bytes());
        }
    }
    write_chunk(&mut stream, chat_usage_chunk().as_bytes());
    write_chunk(&mut stream, b"data: [DONE]\n\n");
    let _ = stream.write_all(b"0\r\n\r\n");
    let _ = stream.flush();
    let _ = stream.shutdown(Shutdown::Write);
}

// --- the gateway, built the way the binary builds it, same shape as gateway_first_events.rs ---

fn upstream_serving(protocol: &str, targets: &'static [&'static str], base_url: &str) -> Upstream {
    let _guard = ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    // SAFETY: `CREDENTIAL_VAR` is unique to this test binary, set and removed
    // around the one resolve that reads it, under `ENV_LOCK` for the whole
    // window.
    unsafe {
        std::env::set_var(CREDENTIAL_VAR, PLANTED_KEY);
    }
    let credential = EnvironmentSecretStore::new()
        .resolve(&SecretRef::Environment {
            var: CREDENTIAL_VAR.to_owned(),
        })
        .expect("just set");
    unsafe {
        std::env::remove_var(CREDENTIAL_VAR);
    }
    let backend = UpstreamBackend::new(
        "fixture".to_owned(),
        vec![Route::new(protocol.to_owned(), targets, base_url)],
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

fn start_gateway(upstream: Upstream, ledger: Arc<Ledger>) -> Gateway {
    inference_gateway::gateway::start_if_required_with_degrade_sink(
        &[inference_gateway::gateway::BackendDemand::LocalGateway],
        || Ok(upstream),
        None,
        None,
        Some(host::observation_sink(ledger, None)),
        None,
    )
    .expect("loopback is bindable")
    .expect("a gateway-backed profile requires a gateway")
}

fn messages_request(token: &str, body: &str) -> Vec<u8> {
    format!(
        "POST /v1/messages?beta=true HTTP/1.1\r\n\
         Host: 127.0.0.1\r\n\
         Authorization: Bearer {token}\r\n\
         Content-Type: application/json\r\n\
         Anthropic-Version: 2023-06-01\r\n\
         Content-Length: {}\r\n\
         \r\n\
         {body}",
        body.len()
    )
    .into_bytes()
}

/// A minimal Anthropic-messages harness request, `stream` as given, whose
/// last user turn hands back one tool result when `error_result` is given —
/// `Some(true)` for a repair, `Some(false)` for a clean result, `None` for
/// no tool result at all (an ordinary prompt).
fn harness_body(stream: bool, error_result: Option<bool>) -> String {
    let mut messages = vec![json!({"role": "user", "content": "hi"})];
    if let Some(is_error) = error_result {
        messages.push(json!({
            "role": "assistant",
            "content": [{"type": "tool_use", "id": "call_prev", "name": "Bash", "input": {}}],
        }));
        messages.push(json!({
            "role": "user",
            "content": [{
                "type": "tool_result",
                "tool_use_id": "call_prev",
                "content": "boom",
                "is_error": is_error,
            }],
        }));
    }
    json!({
        "model": "claude-x",
        "max_tokens": 10,
        "messages": messages,
        "stream": stream,
    })
    .to_string()
}

fn send_and_read(address: SocketAddr, raw: &[u8]) -> Vec<u8> {
    let mut client = TcpStream::connect(address).expect("the gateway accepts connections");
    client
        .set_read_timeout(Some(CLIENT_TIMEOUT))
        .expect("a non-zero timeout is valid");
    client
        .write_all(raw)
        .expect("the gateway reads the request");
    client.flush().expect("flush");
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

fn status_line(response: &[u8]) -> String {
    let end = response
        .windows(2)
        .position(|window| window == b"\r\n")
        .expect("a response head has a status line");
    String::from_utf8_lossy(&response[..end]).into_owned()
}

fn wait_for_row(
    ledger: &Ledger,
    provider: &str,
    model: &str,
    route: Option<&str>,
    harness: Option<&str>,
) -> Vec<inference_gateway::routing::evidence::RoutingObservation> {
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
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
        if !rows.is_empty() || std::time::Instant::now() >= deadline {
            return rows;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

// ---------------------------------------------------------------------------
// (a) A translated stream: two tool-use blocks, one error tool result.
// ---------------------------------------------------------------------------

/// Mutation target `rounds-never-counted`: removing the increment on
/// `BlockStart::ToolUse` must turn this test's `Some(2)` into `Some(0)`.
///
/// Mutation target `errors-not-counted`: flipping the `is_error: true` check
/// to `is_error: false` in the repairs count must turn this test's `Some(1)`
/// into `Some(0)`.
#[test]
fn a_translated_stream_with_two_tool_calls_and_one_error_result_counts_both() {
    let fixture = StreamUpstream::start(Script::TwoToolCalls);
    let ledger = ledger_fixture();
    let upstream = upstream_serving(
        "openai-chat",
        &["/chat/completions"],
        &fixture.openai_base_url(),
    );
    let gateway = start_gateway(upstream, Arc::clone(&ledger.ledger));
    gateway.routing().bind(
        "claude-code",
        "openai-chat",
        AssignedModel::named("fixture-model"),
        &gateway
            .upstream()
            .expect("a started gateway has its upstream"),
    );

    let response = send_and_read(
        gateway.address(),
        &messages_request(gateway.token().expose(), &harness_body(true, Some(true))),
    );
    assert!(
        status_line(&response).starts_with("HTTP/1.1 200"),
        "{}",
        status_line(&response)
    );

    let rows = wait_for_row(
        &ledger.ledger,
        "fixture",
        "claude-x",
        Some("anthropic-messages->openai-chat"),
        Some("claude-code"),
    );
    assert_eq!(rows.len(), 1, "one routing observation for the exchange");
    let row = &rows[0];
    assert_eq!(row.outcome, Some(Outcome::Succeeded));
    assert_eq!(
        row.tool_rounds,
        Some(2),
        "two BlockStart::ToolUse events must both be counted"
    );
    assert_eq!(
        row.repairs,
        Some(1),
        "the one is_error: true tool result must be counted"
    );
}

// ---------------------------------------------------------------------------
// (b) A translated document: one tool-use block, no error result.
// ---------------------------------------------------------------------------

#[test]
fn a_translated_document_with_one_tool_call_and_no_error_result_counts_both() {
    let fixture = DocumentUpstream::answering(
        json!({
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
                        {"id": "call_fix_A", "type": "function", "function": {"name": "Bash", "arguments": "{\"command\": \"ls\"}"}}
                    ]
                },
                "finish_reason": "tool_calls",
                "logprobs": null
            }],
            "usage": {"prompt_tokens": 40, "completion_tokens": 12, "total_tokens": 52}
        })
        .to_string(),
    );
    let ledger = ledger_fixture();
    let upstream = upstream_serving(
        "openai-chat",
        &["/chat/completions"],
        &fixture.openai_base_url(),
    );
    let gateway = start_gateway(upstream, Arc::clone(&ledger.ledger));
    gateway.routing().bind(
        "claude-code",
        "openai-chat",
        AssignedModel::named("fixture-model"),
        &gateway
            .upstream()
            .expect("a started gateway has its upstream"),
    );

    let response = send_and_read(
        gateway.address(),
        &messages_request(gateway.token().expose(), &harness_body(false, None)),
    );
    assert!(status_line(&response).starts_with("HTTP/1.1 200"));

    let rows = wait_for_row(
        &ledger.ledger,
        "fixture",
        "claude-x",
        Some("anthropic-messages->openai-chat"),
        Some("claude-code"),
    );
    assert_eq!(rows.len(), 1);
    let row = &rows[0];
    assert_eq!(
        row.tool_rounds,
        Some(1),
        "the document's one tool call must be counted"
    );
    assert_eq!(
        row.repairs,
        Some(0),
        "the request carried no tool result at all: 0, not NULL"
    );
}

// ---------------------------------------------------------------------------
// (c) A translated stream with no tool use at all.
// ---------------------------------------------------------------------------

#[test]
fn a_translated_stream_with_no_tool_use_counts_zero() {
    let fixture = StreamUpstream::start(Script::TextOnlyNoToolCall);
    let ledger = ledger_fixture();
    let upstream = upstream_serving(
        "openai-chat",
        &["/chat/completions"],
        &fixture.openai_base_url(),
    );
    let gateway = start_gateway(upstream, Arc::clone(&ledger.ledger));
    gateway.routing().bind(
        "claude-code",
        "openai-chat",
        AssignedModel::named("fixture-model"),
        &gateway
            .upstream()
            .expect("a started gateway has its upstream"),
    );

    let response = send_and_read(
        gateway.address(),
        &messages_request(gateway.token().expose(), &harness_body(true, None)),
    );
    assert!(status_line(&response).starts_with("HTTP/1.1 200"));

    let rows = wait_for_row(
        &ledger.ledger,
        "fixture",
        "claude-x",
        Some("anthropic-messages->openai-chat"),
        Some("claude-code"),
    );
    assert_eq!(rows.len(), 1);
    assert_eq!(
        rows[0].tool_rounds,
        Some(0),
        "the seam looked and found no tool-use block: 0, not NULL"
    );
    assert_eq!(rows[0].repairs, Some(0));
}

// ---------------------------------------------------------------------------
// (d) A relayed exchange: both NULL even though the body has real content.
// ---------------------------------------------------------------------------

/// Mutation target `relay-counts`: giving the relayed path's `Exchange`
/// `tool_rounds: Some(0)` must turn this test's `None` into `Some`.
#[test]
fn a_relayed_exchange_records_no_tool_rounds_or_repairs() {
    let fixture = DocumentUpstream::answering(
        json!({
            "type": "message",
            "id": "msg_relayed",
            "role": "assistant",
            "model": "fixture-model",
            "content": [
                {"type": "text", "text": "hi there"},
                {"type": "tool_use", "id": "call_relayed", "name": "Bash", "input": {"command": "ls"}}
            ],
            "stop_reason": "tool_use",
            "stop_sequence": null,
            "usage": {"input_tokens": 999, "output_tokens": 888}
        })
        .to_string(),
    );
    let ledger = ledger_fixture();
    let upstream = upstream_serving("anthropic-messages", &["/messages"], &fixture.root_url());
    let gateway = start_gateway(upstream, Arc::clone(&ledger.ledger));
    gateway.routing().bind(
        "claude-code",
        "anthropic-messages",
        AssignedModel::named("fixture-model"),
        &gateway
            .upstream()
            .expect("a started gateway has its upstream"),
    );

    let response = send_and_read(
        gateway.address(),
        &messages_request(gateway.token().expose(), &harness_body(false, Some(true))),
    );
    assert!(status_line(&response).starts_with("HTTP/1.1 200"));
    assert!(
        String::from_utf8_lossy(&response).contains("\"call_relayed\""),
        "the relayed body must carry the real tool call for the NULL row below to mean anything"
    );

    let rows = wait_for_row(
        &ledger.ledger,
        "fixture",
        "claude-x",
        Some("anthropic-messages"),
        Some("claude-code"),
    );
    assert_eq!(rows.len(), 1);
    assert_eq!(
        rows[0].tool_rounds, None,
        "a relayed exchange's body is never read; nothing may be invented for it"
    );
    assert_eq!(
        rows[0].repairs, None,
        "a relayed exchange's request is never decoded; nothing may be invented for it"
    );
}
