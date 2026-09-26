//! `GH-STREAM-FIRST-EVENTS` (map lines 1331/1332): the first real token and
//! the first tool call, noted as canonical stream events pass through the
//! translate seam — through a real socket and a real [`Gateway`], the same
//! door `tests/gateway_translate_evidence.rs` and `tests/gateway_translate.rs`
//! already enter.
//!
//! # Which of 1332's three exclusions this file exercises, and how
//!
//! The rule names three things that must never count as the first generated
//! token: whitespace padding, a transport keepalive, and a reasoning-only
//! delta. This file's fixture speaks OpenAI Chat on the wire (the same pair,
//! `anthropic-messages -> openai-chat`, `tests/gateway_translate.rs` already
//! drives), because that lets every test here also prove the translated
//! *streamed* and *document* shapes through the one pair.
//!
//! - **Whitespace padding**: exercised natively, in
//!   [`a_translated_streamed_exchange_notes_first_token_and_first_tool_call_in_order`]
//!   and [`a_translated_stream_whose_only_text_is_whitespace_records_no_first_token`].
//! - **An SSE transport comment** (`: keep-alive`): exercised natively — it
//!   is dropped by `translate::stream::SseReader` before any codec ever sees
//!   it, which is protocol-agnostic, so sending it ahead of an OpenAI-Chat
//!   stream proves the same thing sending it ahead of an Anthropic one would.
//! - **A provider-specific keepalive event** (Anthropic's `ping`, which
//!   decodes to no canonical event at all) and **a reasoning-only delta**
//!   (refused at decode before it becomes a canonical event): OpenAI Chat's
//!   wire has no equivalent of either, so neither is exercised here. Both
//!   hold **by construction** rather than by this file's own proof:
//!   `anthropic.rs`'s `EventDecoder::feed` maps `"ping"` to `Ok(Vec::new())`
//!   and refuses a `thinking`/`signature_delta` block at decode — in both
//!   cases no `StreamEvent` is ever produced for `FirstEvents::note` to see,
//!   so the rule cannot get either wrong without a decoder defect, which is
//!   `docs/product/design-decisions.md`'s own reading and not re-derived
//!   here.

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
const PLANTED_KEY: &str = "sk-planted-first-events-000111222333";
const CREDENTIAL_VAR: &str = "GLASSHOUSE_GATEWAY_FIRST_EVENTS_TEST_KEY";

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

// --- a streaming fixture: writes chat-completion chunks in order, with real pauses --

/// Which chunks [`serve_stream`] writes, after the head and the SSE comment
/// every script sends first.
#[derive(Clone, Copy)]
enum Script {
    /// (a): a whitespace-only delta, a real pause, real text, a real pause,
    /// then a tool call — the shape
    /// [`a_translated_streamed_exchange_notes_first_token_and_first_tool_call_in_order`]
    /// times.
    TextThenToolCallWithPauses,
    /// (b): real text and no tool call at all.
    TextOnlyNoToolCall,
    /// (e): a whitespace-only delta and nothing else that could qualify.
    WhitespaceOnly,
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

/// The pause each of [`Script::TextThenToolCallWithPauses`]'s two gaps
/// holds — long enough that a unix-second clock reading taken before and
/// after can never round to the same second, and never asserted against as
/// an exact value (`CROSS-PLATFORM REQUIREMENTS`: assert gaps, not exact
/// durations).
const PAUSE: Duration = Duration::from_millis(1200);

fn serve_stream(mut stream: TcpStream, script: Script) {
    stream.set_nonblocking(false).expect("blocking mode");
    let _ = stream.set_read_timeout(Some(Duration::from_secs(60)));
    read_request(&mut stream);
    let head =
        "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ntransfer-encoding: chunked\r\n\r\n";
    let _ = stream.write_all(head.as_bytes());

    // Every script opens with an SSE transport comment ahead of the first
    // real event — dropped by `SseReader` before any codec sees it, and the
    // same fixture for every script since it never becomes a canonical
    // event either way.
    write_chunk(&mut stream, b": keep-alive\r\n");
    write_chunk(
        &mut stream,
        chat_chunk(json!({"role": "assistant", "content": ""}), Value::Null).as_bytes(),
    );

    match script {
        Script::TextThenToolCallWithPauses => {
            write_chunk(
                &mut stream,
                chat_chunk(json!({"content": "   "}), Value::Null).as_bytes(),
            );
            std::thread::sleep(PAUSE);
            write_chunk(
                &mut stream,
                chat_chunk(json!({"content": "Checking."}), Value::Null).as_bytes(),
            );
            std::thread::sleep(PAUSE);
            write_chunk(
                &mut stream,
                chat_chunk(
                    json!({"tool_calls": [{"index": 0, "id": "call_fix_A", "type": "function", "function": {"name": "Bash", "arguments": ""}}]}),
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
        Script::WhitespaceOnly => {
            write_chunk(
                &mut stream,
                chat_chunk(json!({"content": "   \n"}), Value::Null).as_bytes(),
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

// --- the gateway, built the way the binary builds it, same shape as gateway_translate_evidence.rs ---

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

/// A minimal Anthropic-messages harness request, `stream` as given.
fn harness_body(stream: bool) -> String {
    json!({
        "model": "claude-x",
        "max_tokens": 10,
        "messages": [{"role": "user", "content": "hi"}],
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
// (a) A translated streamed exchange: order and real gaps.
// ---------------------------------------------------------------------------

/// The keep-alive comment, the whitespace delta, and the two real pauses are
/// all in place before the tool call — asserting the row's own three
/// instants are ordered with real gaps, never exact durations
/// (`CROSS-PLATFORM REQUIREMENTS`).
///
/// Mutation target `never-stamped`: removing the streamed loop's `note` call
/// must fail this test with `first_token_at` staying `None`.
#[test]
fn a_translated_streamed_exchange_notes_first_token_and_first_tool_call_in_order() {
    let fixture = StreamUpstream::start(Script::TextThenToolCallWithPauses);
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
        &messages_request(gateway.token().expose(), &harness_body(true)),
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

    let first_byte = row
        .first_byte_at_unix
        .expect("a translated exchange that reached the provider has a first byte");
    let first_token = row
        .first_token_at_unix
        .expect("the real text delta must have stamped first_token_at");
    let first_tool_call = row
        .first_tool_call_at_unix
        .expect("the tool-use block must have stamped first_tool_call_at");

    assert!(
        first_byte <= first_token,
        "first_byte_at ({first_byte}) must not follow first_token_at ({first_token})"
    );
    assert!(
        first_token - first_byte >= 1,
        "the 1.2s pause before the real text delta must show up as a real gap: \
         first_byte_at={first_byte}, first_token_at={first_token}"
    );
    assert!(
        first_tool_call - first_token >= 1,
        "the 1.2s pause before the tool call must show up as a real gap: \
         first_token_at={first_token}, first_tool_call_at={first_tool_call}"
    );
}

// ---------------------------------------------------------------------------
// (b) Text and no tool use.
// ---------------------------------------------------------------------------

/// Mutation target `tool-call-at-any-block`: widening the `BlockStart::ToolUse`
/// match to any `BlockStart` must turn this test's `None` into `Some` — the
/// text block's own `BlockStart::Text` would then wrongly stamp it.
#[test]
fn a_translated_stream_with_text_and_no_tool_use_records_no_first_tool_call() {
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
        &messages_request(gateway.token().expose(), &harness_body(true)),
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
    assert!(
        rows[0].first_token_at_unix.is_some(),
        "the real text delta must have stamped first_token_at"
    );
    assert_eq!(
        rows[0].first_tool_call_at_unix, None,
        "no tool-use block ever started; first_tool_call_at must stay NULL"
    );
}

// ---------------------------------------------------------------------------
// (c) A relayed document: both NULL, and the usage is still exact.
// ---------------------------------------------------------------------------

/// Both instants stay NULL, and since the 2026-09-03 ruling the *reason* has
/// changed: the relay does read a supported body's usage figures now
/// (`GH-RELAY-USAGE`, `gateway/usage.rs`), and it still records no instant
/// here because this response is a **document**. Every marker is in that one
/// body, so the moment each passes the seam is a reading of how fast the
/// socket drained rather than of when the provider produced it —
/// `usage::Delivery`'s rule, and the reason the relay does not copy
/// `translate`'s `FirstEvents::of_document` trick of setting both to
/// `first_byte_at`.
///
/// The usage columns below are the other half of the same row and the reason
/// this is not merely an unchanged test: they are exact, from the document's
/// own digits.
///
/// Mutation target `relay-stamps`: giving the relayed path's `Exchange`
/// `first_token_at: first_byte_at` must turn this test's `None` into `Some`.
#[test]
fn a_relayed_document_records_its_usage_and_no_first_token_or_first_tool_call() {
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
        &messages_request(gateway.token().expose(), &harness_body(false)),
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
        rows[0].first_token_at_unix, None,
        "a document exposes no boundary an instant could describe; nothing may be derived \
         for it"
    );
    assert_eq!(rows[0].first_tool_call_at_unix, None);
    assert_eq!(
        (
            rows[0].input_tokens,
            rows[0].output_tokens,
            rows[0].cached_input_tokens
        ),
        (Some(999), Some(888), None),
        "the usage the document stated is exact — read, not derived — and the cache figure \
         it never stated stays unknown"
    );
}

// ---------------------------------------------------------------------------
// (d) A translated document: both equal first_byte_at.
// ---------------------------------------------------------------------------

#[test]
fn a_translated_document_with_text_and_a_tool_call_records_both_as_first_byte_at() {
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
        &messages_request(gateway.token().expose(), &harness_body(false)),
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
    let first_byte = row
        .first_byte_at_unix
        .expect("a translated exchange that reached the provider has a first byte");
    assert_eq!(
        row.first_token_at_unix,
        Some(first_byte),
        "a document has no finer boundary than its own arrival"
    );
    assert_eq!(row.first_tool_call_at_unix, Some(first_byte));
}

// ---------------------------------------------------------------------------
// (e) A stream whose only text is whitespace: no first token.
// ---------------------------------------------------------------------------

/// Mutation target `whitespace-counts`: replacing the non-whitespace check
/// with `true` must turn this test's `None` into `Some`.
#[test]
fn a_translated_stream_whose_only_text_is_whitespace_records_no_first_token() {
    let fixture = StreamUpstream::start(Script::WhitespaceOnly);
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
        &messages_request(gateway.token().expose(), &harness_body(true)),
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
        rows[0].first_token_at_unix, None,
        "a whitespace-only text delta must never count as the first real token"
    );
    assert_eq!(rows[0].first_tool_call_at_unix, None);
}
