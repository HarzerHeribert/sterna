//! Phase 58, capability map lines 2014–2019 (`GH-TRANSLATE-CACHE-STABILITY`):
//! a default Claude Code launch — `cache_control` on the system prompt, a
//! content block and a tool definition, sent unconditionally by the harness —
//! stays usable on every translated pairing, instead of needing
//! `DISABLE_PROMPT_CACHING=1` (the limit `phase-56.md`'s T1 entry recorded).
//!
//! # Where these tests enter
//!
//! (a)-(d) at `gateway::start_if_required_with_degrade_sink`, the same door
//! `tests/gateway_translate.rs` and `tests/gateway_translate_gemini.rs` use —
//! the shipped binary's own accept loop, with an [`Upstream`] built by the
//! **production** `profile::gateway_upstream`. (e) is the one test that goes
//! further, through `glasshouse launch` itself, the way that file's own
//! launch test does.
//!
//! # Fixtures: copied, not shared
//!
//! `chat`, `gemini`, the provider/upstream builders, the ledger fixture, the
//! launch harness — all copied from `gateway_translate.rs` and
//! `gateway_translate_gemini.rs` rather than lifted into a `tests/common`
//! module. Every existing pair file already carries its own copy of this
//! machinery; a shared module would be the first exception rather than a
//! continuation of the convention, and touching those files was outside this
//! package's `YOURS`.
//!
//! # Tool order and prefix stability: one target, not three
//!
//! (c) and (d) exercise the chat-only pairing only, not all three
//! translated targets. `Request::normalized` (`canonical.rs`) sorts tools
//! **once**, at the single seam every translated request passes through
//! (`translate::serve`, before any codec's `encode_request` is called) — so
//! the property is a fact about the canonical form, not about any one
//! encoder, and `canonical::tests::normalized_sorts_tools_by_name_regardless_of_the_harnesss_order`
//! already pins it independent of target. One end-to-end target is the
//! honest amount of additional proof that the seam is actually wired into
//! `serve`; a second and third fixture (Gemini, OpenAI Responses) would
//! re-prove the same seam, not a different one.

mod host;
use host::Ledger;

use std::io::{Read, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use inference_gateway::gateway::translate::{CacheDisposition, field_rows};
use inference_gateway::gateway::{Gateway, Upstream};
use inference_gateway::provider::Provider;
use inference_gateway::routing::AssignedModel;
use inference_gateway::routing::evidence::Outcome;
use inference_gateway::secret::EnvironmentSecretStore;
use serde_json::{Value, json};

/// Planted provider credentials, unique to this test binary. Never real
/// keys, and asserted on so that `!contains` has something to bite.
const CHAT_KEY: &str = "sk-planted-translate-cache-000111";
const CHAT_CREDENTIAL_VAR: &str = "GLASSHOUSE_GATEWAY_TRANSLATE_CACHE_CHAT_TEST_KEY";
const GEMINI_KEY: &str = "AIza-planted-translate-cache-000111";
const GEMINI_CREDENTIAL_VAR: &str = "GLASSHOUSE_GATEWAY_TRANSLATE_CACHE_GEMINI_TEST_KEY";

const PLANTED_PROMPT: &str = "PLANTED-PROMPT-TEXT-FOR-CACHE-TESTS";
const CLIENT_TIMEOUT: Duration = Duration::from_secs(20);

/// Every set/resolve/remove of a `CREDENTIAL_VAR` happens under this lock —
/// the environment is process state and this binary's tests run in
/// parallel (`gateway_translate.rs`'s own comment on `ENV_LOCK` explains the
/// failure this avoids).
static ENV_LOCK: Mutex<()> = Mutex::new(());

fn env_lock() -> MutexGuard<'static, ()> {
    ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

// --- recorded requests, shared shape ------------------------------------------

#[derive(Debug, Clone)]
struct RecordedRequest {
    target: String,
    body: Vec<u8>,
}

impl RecordedRequest {
    fn json(&self) -> Value {
        serde_json::from_slice(&self.body).expect("the fixture received a JSON body")
    }

    fn body_text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
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
    let request_line = lines.next()?;
    let mut parts = request_line.split(' ');
    let _method = parts.next()?;
    let target = parts.next()?.to_owned();
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
    Some(RecordedRequest { target, body })
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

/// One `ChatCompletion` reply with a text part, finishing normally, and
/// `prompt_tokens_details.cached_tokens` set — the shape (a) needs to prove
/// the reading half of 2019 (already-production `Usage.cached` plumbing) is
/// unaffected by this package.
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
        "usage": {"prompt_tokens": 40, "completion_tokens": 12, "total_tokens": 52, "prompt_tokens_details": {"cached_tokens": 8}}
    })
    .to_string()
}

struct ChatOnlyUpstream {
    address: SocketAddr,
    requests: Arc<Mutex<Vec<RecordedRequest>>>,
    stop: Arc<AtomicBool>,
    accept: Option<JoinHandle<()>>,
}

impl ChatOnlyUpstream {
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

/// `profile::gateway_upstream`, the production builder, run with the planted
/// credential set only for the duration of the call.
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

// --- the ledger and the gateway, built the way the binary builds them ----------

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

// --- Claude Code request bodies --------------------------------------------------

/// A default Claude Code request: `cache_control` on the system block, the
/// one content block and the one tool — exactly what 2015 says a default
/// launch sends, and what the packet's own recorded limit says was refused
/// before this package.
fn body_with_cache_control(user: Option<&str>) -> String {
    let metadata = match user {
        Some(user) => json!({"user_id": user}),
        None => json!({}),
    };
    json!({
        "model": "claude-x",
        "max_tokens": 10,
        "system": [{"type": "text", "text": PLANTED_PROMPT, "cache_control": {"type": "ephemeral"}}],
        "messages": [{"role": "user", "content": [
            {"type": "text", "text": "hi", "cache_control": {"type": "ephemeral"}}
        ]}],
        "tools": [{
            "name": "Bash",
            "description": "Run a shell command",
            "input_schema": {"type": "object", "properties": {"command": {"type": "string"}}},
            "cache_control": {"type": "ephemeral"}
        }],
        "metadata": metadata,
        "stream": false
    })
    .to_string()
}

fn tool(name: &str) -> Value {
    json!({
        "name": name,
        "description": format!("Run {name}"),
        "input_schema": {"type": "object", "properties": {"x": {"type": "string"}}}
    })
}

/// A plain request with the two named tools, in the order given — for (c),
/// where the only thing that varies between two sends is this order.
fn body_with_tools(tools: &[&str]) -> String {
    json!({
        "model": "claude-x",
        "max_tokens": 10,
        "messages": [{"role": "user", "content": "hi"}],
        "tools": tools.iter().map(|name| tool(name)).collect::<Vec<_>>(),
        "stream": false
    })
    .to_string()
}

/// A turn with `messages.len()` canonical messages, all plain text so each
/// maps to exactly one encoded OpenAI Chat message — the shape (d) needs so
/// the encoded prefix's length is predictable.
fn body_with_turn(messages: &[(&str, &str)]) -> String {
    json!({
        "model": "claude-x",
        "max_tokens": 10,
        "system": [{"type": "text", "text": PLANTED_PROMPT}],
        "messages": messages.iter().map(|(role, text)| json!({"role": role, "content": text})).collect::<Vec<_>>(),
        "tools": [tool("Bash")],
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

// --- (a): carried as the target's own hint, and the reading half untouched -----

/// A default Claude Code request — `cache_control` on the system, a content
/// block and a tool — reaches a chat-only entitlement as `200`, not the
/// `400` `phase-56.md` recorded: the fixture's own bytes carry no
/// `cache_control` anywhere and a `prompt_cache_key` derived from
/// `metadata.user_id`; the fixture's stated `cached_tokens` still reaches
/// the harness as `cache_read_input_tokens` and the ledger as
/// `cached_input_tokens` — the measurement half of 2019, already production,
/// undisturbed by carrying the marker instead of refusing it.
#[test]
fn cache_control_is_carried_as_prompt_cache_key_and_the_read_ratio_still_reaches_the_ledger() {
    let fixture = ChatOnlyUpstream::start();
    let ledger = ledger_fixture();
    let gateway = start_gateway(
        chat_upstream(&chat_only_provider(&fixture)),
        Some(Arc::clone(&ledger.ledger)),
    );
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
        &messages_request(
            gateway.token().expose(),
            &body_with_cache_control(Some("user_abc")),
        ),
    );
    let (head, body) = head_and_body(&response);
    assert!(head.starts_with("HTTP/1.1 200"), "{head}");

    let received = fixture.only_request();
    assert!(
        !received.body_text().contains("cache_control"),
        "no target here has a literal cache_control field: {}",
        received.body_text()
    );
    let sent = received.json();
    assert_eq!(
        sent["prompt_cache_key"], "user_abc",
        "derived from metadata.user_id, carried as `user` on the same request"
    );
    assert_eq!(sent["user"], "user_abc");

    let answer: Value = serde_json::from_slice(body).expect("an Anthropic JSON document");
    assert_eq!(
        answer["usage"]["cache_read_input_tokens"], 8,
        "the fixture's cached_tokens still reaches the harness"
    );

    let rows = wait_for_row(
        &ledger.ledger,
        "chat",
        "claude-x",
        Some("anthropic-messages->openai-chat"),
        Some("claude-code"),
    );
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].outcome, Some(Outcome::Succeeded));
    assert_eq!(
        rows[0].cached_input_tokens,
        Some(8),
        "the ledger's cached-token column, unaffected by carrying the marker"
    );
}

/// A body naming no `metadata.user_id` gets no `prompt_cache_key` — nothing
/// here guesses a session, and the field is simply absent rather than a
/// placeholder.
#[test]
fn a_request_with_no_user_id_gets_no_prompt_cache_key() {
    let fixture = ChatOnlyUpstream::start();
    let gateway = start_gateway(chat_upstream(&chat_only_provider(&fixture)), None);

    let response = send_and_read(
        gateway.address(),
        &messages_request(gateway.token().expose(), &body_with_cache_control(None)),
    );
    let (head, _body) = head_and_body(&response);
    assert!(head.starts_with("HTTP/1.1 200"), "{head}");

    let sent = fixture.only_request().json();
    assert_eq!(
        sent.get("prompt_cache_key"),
        None,
        "no user id in the request, so no key is invented for it"
    );
}

// --- (b): stripped, by name, on a pairing with no equivalent -------------------

/// The same default Claude Code request against a Gemini-only entitlement:
/// still `200`, the fixture's bytes carry no `cache_control` and no
/// `prompt_cache_key` (Gemini has neither), and the pair table's field rows
/// say `Stripped`, naming the reason, for a reader who wants to know why.
#[test]
fn the_same_request_at_a_gemini_only_fixture_is_served_with_the_marker_stripped() {
    let fixture = GeminiOnlyUpstream::start();
    let gateway = start_gateway(gemini_upstream(&gemini_provider(&fixture)), None);

    let response = send_and_read(
        gateway.address(),
        &messages_request(
            gateway.token().expose(),
            &body_with_cache_control(Some("user_abc")),
        ),
    );
    let (head, _body) = head_and_body(&response);
    assert!(head.starts_with("HTTP/1.1 200"), "{head}");

    let received = fixture.only_request();
    assert!(
        received.target.starts_with("/v1beta/models/"),
        "the request actually reached the Gemini inference endpoint: {}",
        received.target
    );
    assert!(
        !received.body_text().contains("cache_control"),
        "{}",
        received.body_text()
    );
    assert!(
        !received.body_text().contains("prompt_cache_key"),
        "Gemini has no per-request cache-hint field to carry it to: {}",
        received.body_text()
    );

    match field_rows("gemini-generate-content")
        .expect("the gemini codec has field rows")
        .cache
    {
        Some(CacheDisposition::Stripped(reason)) => assert!(!reason.is_empty()),
        other => panic!("expected Stripped, got {other:?}"),
    }
}

// --- (c): deterministic tool order ----------------------------------------------

/// The same two tools, sent in opposite orders across two requests, encode
/// identically — `Request::normalized` sorts them once, before either
/// request's `encode_request` runs.
#[test]
fn the_same_tools_in_two_orders_encode_to_the_same_bytes() {
    let fixture = ChatOnlyUpstream::start();
    let gateway = start_gateway(chat_upstream(&chat_only_provider(&fixture)), None);

    for order in [["Zebra", "Alpha"], ["Alpha", "Zebra"]] {
        let response = send_and_read(
            gateway.address(),
            &messages_request(gateway.token().expose(), &body_with_tools(&order)),
        );
        let (head, _) = head_and_body(&response);
        assert!(head.starts_with("HTTP/1.1 200"), "{head}");
    }

    let requests = fixture.requests();
    assert_eq!(requests.len(), 2, "one request per order sent");
    let first = requests[0].json()["tools"].clone();
    let second = requests[1].json()["tools"].clone();
    assert_eq!(
        first, second,
        "the harness listed the tools in opposite orders; the wire bytes must not differ"
    );
    let names: Vec<&str> = first
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| entry["function"]["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, vec!["Alpha", "Zebra"], "sorted by name");
}

// --- (d): prefix stability across turns ------------------------------------------

/// Turn two adds two messages to turn one's exact conversation, keeping the
/// same system prompt and tools. The first three encoded messages — system,
/// then the two turn-one messages — and the `tools` array must be identical
/// across both requests: nothing in the encoder may vary a byte of a message
/// already sent upstream once the harness sends it again in a later turn,
/// which is what a translated pairing owes to match the relay's own
/// byte-for-byte guarantee on a served target.
#[test]
fn a_second_turn_repeats_the_first_turns_encoded_prefix_byte_for_byte() {
    let fixture = ChatOnlyUpstream::start();
    let gateway = start_gateway(chat_upstream(&chat_only_provider(&fixture)), None);

    let turn_one = [("user", "List the files."), ("assistant", "One moment.")];
    let turn_two = [
        ("user", "List the files."),
        ("assistant", "One moment."),
        ("user", "Now read one."),
        ("assistant", "Reading now."),
    ];

    for turn in [&turn_one[..], &turn_two[..]] {
        let response = send_and_read(
            gateway.address(),
            &messages_request(gateway.token().expose(), &body_with_turn(turn)),
        );
        let (head, _) = head_and_body(&response);
        assert!(head.starts_with("HTTP/1.1 200"), "{head}");
    }

    let requests = fixture.requests();
    assert_eq!(requests.len(), 2);
    let first = requests[0].json();
    let second = requests[1].json();

    // The system message plus turn one's two messages: three encoded
    // messages, unchanged by turn two's two additional ones.
    let prefix_len = turn_one.len() + 1;
    let first_prefix = &first["messages"].as_array().unwrap()[..prefix_len];
    let second_prefix = &second["messages"].as_array().unwrap()[..prefix_len];
    assert_eq!(
        first_prefix, second_prefix,
        "the system segment and turn one's messages must stay byte-identical in turn two"
    );
    assert_eq!(
        first["tools"], second["tools"],
        "the tools segment must stay byte-identical across turns"
    );
    assert_eq!(
        second["messages"].as_array().unwrap().len(),
        turn_two.len() + 1,
        "turn two's own two new messages are still appended after the shared prefix"
    );
}
