//! `GH-EFFORT-CARRY`: carrying a Claude Code `thinking` request across a
//! translated pairing instead of refusing it — the prerequisite capability
//! map line 2039's evaluation needs, per `archive/glasshouse:docs/product/design-decisions.md`
//! (*"Carrying effort across a translated pairing"*).
//!
//! # Where these tests enter
//!
//! Exactly where `tests/gateway_translate_cache.rs` enters — this file's
//! sibling and the pattern it copies (its own header explains why: every
//! pair file carries its own fixture machinery rather than a shared `tests`
//! module) — `gateway::start_if_required_with_degrade_sink`, the shipped
//! binary's own accept loop, with an [`Upstream`] built by the
//! **production** `profile::gateway_upstream`.
//!
//! # What is asserted, and against what
//!
//! Every claim about the outbound request is made against bytes the fixture
//! read off the wire with its own parser.

mod host;

use std::io::{Read, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use inference_gateway::gateway::translate::{EffortDisposition, field_rows};
use inference_gateway::gateway::{Gateway, Upstream};
use inference_gateway::provider::Provider;
use inference_gateway::routing::AssignedModel;
use inference_gateway::secret::EnvironmentSecretStore;
use serde_json::{Value, json};

/// Planted provider credentials, unique to this test binary. Never real
/// keys, and asserted on so that `!contains` has something to bite.
const CHAT_KEY: &str = "sk-planted-translate-effort-000111";
const CHAT_CREDENTIAL_VAR: &str = "GLASSHOUSE_GATEWAY_TRANSLATE_EFFORT_CHAT_TEST_KEY";
const RESPONSES_KEY: &str = "sk-planted-translate-effort-000222";
const RESPONSES_CREDENTIAL_VAR: &str = "GLASSHOUSE_GATEWAY_TRANSLATE_EFFORT_RESPONSES_TEST_KEY";
const GEMINI_KEY: &str = "AIza-planted-translate-effort-000111";
const GEMINI_CREDENTIAL_VAR: &str = "GLASSHOUSE_GATEWAY_TRANSLATE_EFFORT_GEMINI_TEST_KEY";

const CLIENT_TIMEOUT: Duration = Duration::from_secs(20);

/// Every set/resolve/remove of a `CREDENTIAL_VAR` happens under this lock —
/// the environment is process state and this binary's tests run in
/// parallel (`gateway_translate.rs`'s own comment on `ENV_LOCK` explains the
/// failure this avoids).
static ENV_LOCK: Mutex<()> = Mutex::new(());

fn env_lock() -> std::sync::MutexGuard<'static, ()> {
    ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

// --- recorded requests, shared shape ------------------------------------------

#[derive(Debug, Clone)]
struct RecordedRequest {
    body: Vec<u8>,
}

impl RecordedRequest {
    fn json(&self) -> Value {
        serde_json::from_slice(&self.body).expect("the fixture received a JSON body")
    }
}

/// Independent of anything under test: reads a request head byte by byte and
/// the body by its declared length.
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
    let _request_line = lines.next()?;
    let mut content_length = 0usize;
    for line in lines {
        if line.is_empty() {
            continue;
        }
        let (name, value) = line.split_once(':')?;
        if name.trim().eq_ignore_ascii_case("content-length") {
            content_length = value.trim().parse().ok()?;
        }
    }
    let mut body = vec![0u8; content_length];
    if content_length > 0 && stream.read_exact(&mut body).is_err() {
        return None;
    }
    Some(RecordedRequest { body })
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

// --- a fixture that speaks only OpenAI Chat -----------------------------------

fn chat_completion_answer() -> String {
    json!({
        "id": "chatcmpl-fixture",
        "object": "chat.completion",
        "created": 1,
        "model": "fixture-model",
        "choices": [{
            "index": 0,
            "message": {"role": "assistant", "content": "Checking."},
            "finish_reason": "stop",
            "logprobs": null
        }],
        "usage": {"prompt_tokens": 40, "completion_tokens": 12, "total_tokens": 52}
    })
    .to_string()
}

struct ChatOnlyUpstream {
    address: SocketAddr,
    requests: Arc<Mutex<Vec<RecordedRequest>>>,
    connections: Arc<std::sync::atomic::AtomicUsize>,
    stop: Arc<AtomicBool>,
    accept: Option<JoinHandle<()>>,
}

impl ChatOnlyUpstream {
    fn start() -> Self {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("loopback is bindable");
        let address = listener.local_addr().expect("bound");
        listener.set_nonblocking(true).expect("polling mode");
        let requests = Arc::new(Mutex::new(Vec::new()));
        let connections = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let stop = Arc::new(AtomicBool::new(false));
        let accept = std::thread::spawn({
            let requests = Arc::clone(&requests);
            let connections = Arc::clone(&connections);
            let stop = Arc::clone(&stop);
            move || {
                while !stop.load(Ordering::Relaxed) {
                    match listener.accept() {
                        Ok((stream, _)) => {
                            connections.fetch_add(1, Ordering::Relaxed);
                            let requests = Arc::clone(&requests);
                            std::thread::spawn(move || serve_chat(stream, &requests));
                        }
                        Err(_) => std::thread::sleep(Duration::from_millis(10)),
                    }
                }
            }
        });
        Self {
            address,
            requests,
            connections,
            stop,
            accept: Some(accept),
        }
    }

    fn base_url(&self) -> String {
        format!("http://{}/v1", self.address)
    }

    fn requests(&self) -> Vec<RecordedRequest> {
        self.requests.lock().unwrap().clone()
    }

    fn only_request(&self) -> RecordedRequest {
        let requests = self.requests();
        assert_eq!(requests.len(), 1, "exactly one request at the fixture");
        requests.into_iter().next().unwrap()
    }

    fn connections(&self) -> usize {
        self.connections.load(Ordering::Relaxed)
    }
}

impl Drop for ChatOnlyUpstream {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(accept) = self.accept.take() {
            let _ = accept.join();
        }
    }
}

fn serve_chat(mut stream: TcpStream, requests: &Mutex<Vec<RecordedRequest>>) {
    stream.set_nonblocking(false).expect("blocking mode");
    let _ = stream.set_read_timeout(Some(Duration::from_secs(60)));
    let _ = stream.set_nodelay(true);
    let Some(request) = read_request(&mut stream) else {
        return;
    };
    requests.lock().unwrap().push(request.clone());
    write_document(&mut stream, "200 OK", &chat_completion_answer());
}

fn chat_only_provider(fixture: &ChatOnlyUpstream) -> Provider {
    let mut provider = inference_gateway::provider::templates()
        .into_iter()
        .find(|provider| provider.name == "openai-compatible")
        .expect("the openai-compatible template exists");
    provider.name = "chat".to_owned();
    provider.protocols[0].base_url = fixture.base_url();
    provider.credential_env = vec![CHAT_CREDENTIAL_VAR.to_owned()];
    provider
}

fn chat_upstream(provider: &Provider) -> Upstream {
    let _guard = env_lock();
    // SAFETY: `CHAT_CREDENTIAL_VAR` is unique to this test binary, set and
    // removed around the one resolve that reads it, under `ENV_LOCK` for the
    // whole window.
    unsafe {
        std::env::set_var(CHAT_CREDENTIAL_VAR, CHAT_KEY);
    }
    let upstream = inference_gateway::pool::gateway_upstream(
        std::slice::from_ref(provider),
        &EnvironmentSecretStore::new(),
        &|_| false,
    );
    unsafe {
        std::env::remove_var(CHAT_CREDENTIAL_VAR);
    }
    upstream.expect("one chat-only provider with a resolvable credential builds an upstream")
}

// --- a fixture that speaks only OpenAI Responses -------------------------------

fn responses_completion_answer() -> String {
    json!({
        "id": "resp_fixture",
        "object": "response",
        "created_at": 1,
        "status": "completed",
        "error": null,
        "incomplete_details": null,
        "model": "fixture-model",
        "output": [
            {"type": "message", "id": "msg_1", "status": "completed", "role": "assistant", "content": [{"type": "output_text", "text": "Checking.", "annotations": []}]}
        ],
        "parallel_tool_calls": true,
        "tool_choice": "auto",
        "tools": [],
        "store": false,
        "usage": {"input_tokens": 40, "output_tokens": 12, "total_tokens": 52},
        "user": null,
        "metadata": {}
    })
    .to_string()
}

struct ResponsesOnlyUpstream {
    address: SocketAddr,
    requests: Arc<Mutex<Vec<RecordedRequest>>>,
    stop: Arc<AtomicBool>,
    accept: Option<JoinHandle<()>>,
}

impl ResponsesOnlyUpstream {
    fn start() -> Self {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("loopback is bindable");
        let address = listener.local_addr().expect("bound");
        listener.set_nonblocking(true).expect("polling mode");
        let requests = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let accept = std::thread::spawn({
            let requests = Arc::clone(&requests);
            let stop = Arc::clone(&stop);
            move || {
                while !stop.load(Ordering::Relaxed) {
                    match listener.accept() {
                        Ok((stream, _)) => {
                            let requests = Arc::clone(&requests);
                            std::thread::spawn(move || serve_responses(stream, &requests));
                        }
                        Err(_) => std::thread::sleep(Duration::from_millis(10)),
                    }
                }
            }
        });
        Self {
            address,
            requests,
            stop,
            accept: Some(accept),
        }
    }

    /// The base URL an OpenAI-shaped provider declares: with `/v1`, so a
    /// Responses client's `/responses` lands on `/v1/responses` — the same
    /// convention `tests/gateway_translate_responses.rs`'s own fixture uses.
    fn base_url(&self) -> String {
        format!("http://{}/v1", self.address)
    }

    fn requests(&self) -> Vec<RecordedRequest> {
        self.requests.lock().unwrap().clone()
    }

    fn only_request(&self) -> RecordedRequest {
        let requests = self.requests();
        assert_eq!(requests.len(), 1, "exactly one request at the fixture");
        requests.into_iter().next().unwrap()
    }
}

impl Drop for ResponsesOnlyUpstream {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(accept) = self.accept.take() {
            let _ = accept.join();
        }
    }
}

fn serve_responses(mut stream: TcpStream, requests: &Mutex<Vec<RecordedRequest>>) {
    stream.set_nonblocking(false).expect("blocking mode");
    let _ = stream.set_read_timeout(Some(Duration::from_secs(60)));
    let _ = stream.set_nodelay(true);
    let Some(request) = read_request(&mut stream) else {
        return;
    };
    requests.lock().unwrap().push(request.clone());
    write_document(&mut stream, "200 OK", &responses_completion_answer());
}

/// The openrouter template narrowed to `openai-responses` — the same
/// `Provider` value `config` would produce for a provider declaring exactly
/// that protocol (`tests/gateway_translate_responses.rs`'s own
/// `provider_serving_only`, copied rather than shared per this file's own
/// header).
fn responses_only_provider(fixture: &ResponsesOnlyUpstream) -> Provider {
    let mut provider = inference_gateway::provider::templates()
        .into_iter()
        .find(|provider| provider.name == "openrouter")
        .expect("the openrouter template exists");
    provider.name = "responses".to_owned();
    provider
        .protocols
        .retain(|support| support.protocol.slug() == "openai-responses");
    assert_eq!(
        provider.protocols.len(),
        1,
        "the openrouter template declares openai-responses exactly once"
    );
    provider.protocols[0].base_url = fixture.base_url();
    provider.credential_env = vec![RESPONSES_CREDENTIAL_VAR.to_owned()];
    provider
}

fn responses_upstream(provider: &Provider) -> Upstream {
    let _guard = env_lock();
    // SAFETY: as `chat_upstream` — under `ENV_LOCK` for the whole window.
    unsafe {
        std::env::set_var(RESPONSES_CREDENTIAL_VAR, RESPONSES_KEY);
    }
    let upstream = inference_gateway::pool::gateway_upstream(
        std::slice::from_ref(provider),
        &EnvironmentSecretStore::new(),
        &|_| false,
    );
    unsafe {
        std::env::remove_var(RESPONSES_CREDENTIAL_VAR);
    }
    upstream.expect("one responses-only provider with a resolvable credential builds an upstream")
}

// --- a fixture that speaks only Gemini -----------------------------------------

fn gemini_document_answer() -> String {
    json!({
        "candidates": [{
            "content": {"role": "model", "parts": [{"text": "Checking."}]},
            "finishReason": "STOP",
            "index": 0,
            "safetyRatings": [],
        }],
        "usageMetadata": {
            "promptTokenCount": 40,
            "candidatesTokenCount": 10,
            "totalTokenCount": 50,
        },
        "modelVersion": "gemini-2.5-pro-001",
        "responseId": "resp-fixture",
    })
    .to_string()
}

struct GeminiOnlyUpstream {
    address: SocketAddr,
    requests: Arc<Mutex<Vec<RecordedRequest>>>,
    stop: Arc<AtomicBool>,
    accept: Option<JoinHandle<()>>,
}

impl GeminiOnlyUpstream {
    fn start() -> Self {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("loopback is bindable");
        let address = listener.local_addr().expect("bound");
        listener.set_nonblocking(true).expect("polling mode");
        let requests = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let accept = std::thread::spawn({
            let requests = Arc::clone(&requests);
            let stop = Arc::clone(&stop);
            move || {
                while !stop.load(Ordering::Relaxed) {
                    match listener.accept() {
                        Ok((stream, _)) => {
                            let requests = Arc::clone(&requests);
                            std::thread::spawn(move || serve_gemini(stream, &requests));
                        }
                        Err(_) => std::thread::sleep(Duration::from_millis(10)),
                    }
                }
            }
        });
        Self {
            address,
            requests,
            stop,
            accept: Some(accept),
        }
    }

    /// The bare host: the codec states `/v1beta` itself.
    fn base_url(&self) -> String {
        format!("http://{}", self.address)
    }

    fn requests(&self) -> Vec<RecordedRequest> {
        self.requests.lock().unwrap().clone()
    }

    fn only_request(&self) -> RecordedRequest {
        let requests = self.requests();
        assert_eq!(requests.len(), 1, "exactly one request at the fixture");
        requests.into_iter().next().unwrap()
    }
}

impl Drop for GeminiOnlyUpstream {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(accept) = self.accept.take() {
            let _ = accept.join();
        }
    }
}

fn serve_gemini(mut stream: TcpStream, requests: &Mutex<Vec<RecordedRequest>>) {
    stream.set_nonblocking(false).expect("blocking mode");
    let _ = stream.set_read_timeout(Some(Duration::from_secs(60)));
    let _ = stream.set_nodelay(true);
    let Some(request) = read_request(&mut stream) else {
        return;
    };
    requests.lock().unwrap().push(request.clone());
    write_document(&mut stream, "200 OK", &gemini_document_answer());
}

fn gemini_provider(fixture: &GeminiOnlyUpstream) -> Provider {
    let mut provider = inference_gateway::provider::templates()
        .into_iter()
        .find(|provider| provider.name == "gemini")
        .expect("the gemini template exists");
    provider.name = "gemini".to_owned();
    provider.protocols[0].base_url = fixture.base_url();
    provider.credential_env = vec![GEMINI_CREDENTIAL_VAR.to_owned()];
    provider
}

fn gemini_upstream(provider: &Provider) -> Upstream {
    let _guard = env_lock();
    // SAFETY: as `chat_upstream` — under `ENV_LOCK` for the whole window.
    unsafe {
        std::env::set_var(GEMINI_CREDENTIAL_VAR, GEMINI_KEY);
    }
    let upstream = inference_gateway::pool::gateway_upstream(
        std::slice::from_ref(provider),
        &EnvironmentSecretStore::new(),
        &|_| false,
    );
    unsafe {
        std::env::remove_var(GEMINI_CREDENTIAL_VAR);
    }
    upstream.expect("one gemini provider with a resolvable credential builds an upstream")
}

// --- the gateway, built the way the binary builds it ---------------------------

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

// --- Claude Code request bodies --------------------------------------------------

fn body_with_thinking(budget_tokens: u64) -> String {
    json!({
        "model": "claude-x",
        "max_tokens": 10,
        "messages": [{"role": "user", "content": "hi"}],
        "thinking": {"type": "enabled", "budget_tokens": budget_tokens},
        "stream": false
    })
    .to_string()
}

fn body_without_thinking() -> String {
    json!({
        "model": "claude-x",
        "max_tokens": 10,
        "messages": [{"role": "user", "content": "hi"}],
        "stream": false
    })
    .to_string()
}

/// A `thinking` block inside message content — still refused by name (the
/// module doc's *"thinking block"* row, kept), unlike the request-level
/// `thinking` object this package now carries.
fn body_with_thinking_block() -> String {
    json!({
        "model": "claude-x",
        "max_tokens": 10,
        "messages": [
            {"role": "user", "content": "hi"},
            {"role": "assistant", "content": [
                {"type": "thinking", "thinking": "reasoning...", "signature": "sig"}
            ]}
        ],
        "stream": false
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

// --- (a): carried onto OpenAI Chat's reasoning_effort ---------------------------

/// A Claude Code request with `thinking: {enabled, budget_tokens: 16000}`
/// reaches a chat-only entitlement as `200`, and the recorded request
/// carries `reasoning_effort` at the word `level_for_budget` maps 16,000
/// onto — `medium`, per `canonical.rs`'s own thresholds
/// (`EFFORT_LOW_MAX < 16000 <= EFFORT_MEDIUM_MAX`).
#[test]
fn a_thinking_request_reaches_a_chat_only_entitlement_with_reasoning_effort_at_the_mapped_level() {
    let fixture = ChatOnlyUpstream::start();
    let gateway = start_gateway(chat_upstream(&chat_only_provider(&fixture)));
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
        &messages_request(gateway.token().expose(), &body_with_thinking(16_000)),
    );
    let (head, _body) = head_and_body(&response);
    assert!(head.starts_with("HTTP/1.1 200"), "{head}");

    let sent = fixture.only_request().json();
    assert_eq!(sent["reasoning_effort"], "medium");
    assert_eq!(sent.get("thinking"), None, "no literal thinking field here");
}

// --- (b): carried onto OpenAI Responses' nested reasoning.effort ---------------

/// The same request against a responses-only entitlement: the recorded
/// request carries `reasoning.effort` at the same mapped word.
#[test]
fn a_thinking_request_reaches_a_responses_only_entitlement_with_nested_reasoning_effort() {
    let fixture = ResponsesOnlyUpstream::start();
    let gateway = start_gateway(responses_upstream(&responses_only_provider(&fixture)));
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
        &messages_request(gateway.token().expose(), &body_with_thinking(16_000)),
    );
    let (head, _body) = head_and_body(&response);
    assert!(head.starts_with("HTTP/1.1 200"), "{head}");

    let sent = fixture.only_request().json();
    assert_eq!(sent["reasoning"]["effort"], "medium");
}

// --- (c): carried onto Gemini's numeric thinkingBudget, clamped ----------------

/// The same request against a Gemini-only entitlement: the recorded request
/// carries the raw budget, unchanged, because 16,000 is inside Gemini's
/// documented range (`gemini.rs`'s `GEMINI_THINKING_BUDGET_MAX`).
#[test]
fn a_thinking_request_reaches_a_gemini_only_entitlement_with_the_budget_carried() {
    let fixture = GeminiOnlyUpstream::start();
    let gateway = start_gateway(gemini_upstream(&gemini_provider(&fixture)));

    let response = send_and_read(
        gateway.address(),
        &messages_request(gateway.token().expose(), &body_with_thinking(16_000)),
    );
    let (head, _body) = head_and_body(&response);
    assert!(head.starts_with("HTTP/1.1 200"), "{head}");

    let sent = fixture.only_request().json();
    assert_eq!(
        sent["generationConfig"]["thinkingConfig"]["thinkingBudget"],
        16_000
    );

    match field_rows("gemini-generate-content")
        .expect("the gemini codec has field rows")
        .effort
    {
        Some(EffortDisposition::Carried { field, .. }) => {
            assert_eq!(field, "generationConfig.thinkingConfig.thinkingBudget");
        }
        other => panic!("expected Carried, got {other:?}"),
    }
}

// --- (d): no thinking at all encodes exactly as before this package ------------

/// A body that never set `thinking` carries none of the three effort
/// fields on any target, and the chat-only encoding is byte-identical (as a
/// parsed document, key for key) to a hand-built golden of exactly what
/// `openai_chat::encode_request` wrote for this body before this package —
/// every field a plain Claude Code turn produces, and nothing named
/// `reasoning_effort`.
#[test]
fn a_request_with_no_thinking_carries_no_effort_field_on_any_target() {
    let golden = json!({
        "model": "claude-x",
        "messages": [{"role": "user", "content": "hi"}],
        "max_tokens": 10,
    });

    let fixture = ChatOnlyUpstream::start();
    let gateway = start_gateway(chat_upstream(&chat_only_provider(&fixture)));
    let response = send_and_read(
        gateway.address(),
        &messages_request(gateway.token().expose(), &body_without_thinking()),
    );
    let (head, _) = head_and_body(&response);
    assert!(head.starts_with("HTTP/1.1 200"), "{head}");
    let sent = fixture.only_request().json();
    assert_eq!(
        sent, golden,
        "no thinking asked for: the encoded document is exactly what this codec wrote before \
         GH-EFFORT-CARRY, with no reasoning_effort key added"
    );

    let fixture = ResponsesOnlyUpstream::start();
    let gateway = start_gateway(responses_upstream(&responses_only_provider(&fixture)));
    let response = send_and_read(
        gateway.address(),
        &messages_request(gateway.token().expose(), &body_without_thinking()),
    );
    let (head, _) = head_and_body(&response);
    assert!(head.starts_with("HTTP/1.1 200"), "{head}");
    let sent = fixture.only_request().json();
    assert_eq!(sent.get("reasoning"), None);

    let fixture = GeminiOnlyUpstream::start();
    let gateway = start_gateway(gemini_upstream(&gemini_provider(&fixture)));
    let response = send_and_read(
        gateway.address(),
        &messages_request(gateway.token().expose(), &body_without_thinking()),
    );
    let (head, _) = head_and_body(&response);
    assert!(head.starts_with("HTTP/1.1 200"), "{head}");
    let sent = fixture.only_request().json();
    assert_eq!(
        sent["generationConfig"],
        json!({"maxOutputTokens": 10}),
        "no thinking asked for: generationConfig carries only max_tokens, no thinkingConfig"
    );
}

// --- (e): a thinking block in message content is still refused by name ---------

/// A `thinking` block inside message content — as opposed to the
/// request-level `thinking` object this package carries — is still a named
/// refusal, and nothing is opened upstream.
#[test]
fn a_thinking_block_in_message_content_is_still_refused_by_name() {
    let fixture = ChatOnlyUpstream::start();
    let gateway = start_gateway(chat_upstream(&chat_only_provider(&fixture)));

    let response = send_and_read(
        gateway.address(),
        &messages_request(gateway.token().expose(), &body_with_thinking_block()),
    );
    let (head, body) = head_and_body(&response);
    assert!(head.starts_with("HTTP/1.1 400"), "{head}");
    let error: Value = serde_json::from_slice(body).expect("an Anthropic error document");
    let message = error["error"]["message"].as_str().unwrap();
    assert!(message.contains("thinking"), "{message}");
    assert_eq!(
        fixture.connections(),
        0,
        "a refused request opens nothing upstream"
    );
}

// --- (f): the ladder never rounds up, and the lowest word is never omitted -----

/// A budget above every threshold maps to `high` and nothing higher; a
/// budget below the lowest threshold still gets a word, `minimal`, never
/// omitted.
#[test]
fn a_budget_above_every_threshold_is_high_and_a_budget_below_the_lowest_is_still_a_word() {
    let fixture = ChatOnlyUpstream::start();
    let gateway = start_gateway(chat_upstream(&chat_only_provider(&fixture)));

    let response = send_and_read(
        gateway.address(),
        &messages_request(gateway.token().expose(), &body_with_thinking(500_000)),
    );
    let (head, _) = head_and_body(&response);
    assert!(head.starts_with("HTTP/1.1 200"), "{head}");
    assert_eq!(fixture.only_request().json()["reasoning_effort"], "high");

    let fixture = ChatOnlyUpstream::start();
    let gateway = start_gateway(chat_upstream(&chat_only_provider(&fixture)));
    let response = send_and_read(
        gateway.address(),
        // Anthropic's own documented floor for budget_tokens.
        &messages_request(gateway.token().expose(), &body_with_thinking(1_024)),
    );
    let (head, _) = head_and_body(&response);
    assert!(head.starts_with("HTTP/1.1 200"), "{head}");
    let sent = fixture.only_request().json();
    assert_eq!(
        sent.get("reasoning_effort"),
        Some(&json!("minimal")),
        "the lowest budget still gets a word, never omitted: {sent}"
    );

    // A budget exactly at a threshold gets that threshold's word, not the
    // one above it — the boundary `level_for_budget` (canonical.rs) must
    // never round up past.
    let fixture = ChatOnlyUpstream::start();
    let gateway = start_gateway(chat_upstream(&chat_only_provider(&fixture)));
    let response = send_and_read(
        gateway.address(),
        &messages_request(
            gateway.token().expose(),
            &body_with_thinking(
                inference_gateway::gateway::translate::canonical::EFFORT_MEDIUM_MAX,
            ),
        ),
    );
    let (head, _) = head_and_body(&response);
    assert!(head.starts_with("HTTP/1.1 200"), "{head}");
    assert_eq!(
        fixture.only_request().json()["reasoning_effort"],
        "medium",
        "a budget exactly at EFFORT_MEDIUM_MAX must stay medium, not round up to high"
    );
}
