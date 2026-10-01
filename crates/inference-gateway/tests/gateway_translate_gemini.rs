//! Phase 56 T3, lines 1948–1950 and 1956: the three translated pairs whose
//! provider side is Google's Generative Language API, end to end against a
//! fixture upstream that speaks **only** `gemini-generate-content` and
//! records what reached it.
//!
//! # Where these tests enter
//!
//! At `gateway::start_if_required_with_degrade_sink` — the same door the
//! shipped binary calls, in front of the same accept loop and the same
//! `ingress::serve` — with an [`Upstream`] built by the **production**
//! `profile::gateway_upstream` from the **production** `gemini` provider
//! template. Nothing in the translation path is bypassed: the request is
//! placed, decoded, encoded, addressed at the model's own path, sent with
//! the provider's credential in `x-goog-api-key`, and the answer translated
//! back, by the code the binary ships. `tests/gateway_translate.rs`'s own
//! header explains why this is the entry point and not `glasshouse launch`;
//! the same refusal at `profile::apply_gateway` stands here.
//!
//! # What this file asserts that the other pair files cannot
//!
//! Gemini differs from the other three wires in shape, not only in
//! spelling, and each difference is a test here:
//!
//! - the **model is in the path**, so the fixture asserts the request line
//!   it received, not only the body;
//! - the **credential goes in `x-goog-api-key`** and, deliberately, not in
//!   `authorization` — Google reads that header as an OAuth bearer token;
//! - a **function call has no id**, so a tool result is matched to its call
//!   by name, and the id the harness sees is one Glasshouse minted;
//! - `finishReason: "STOP"` beside a function call is **`tool_use`**, not
//!   `end_turn`;
//! - the stream has **no terminator**, so the finish reason is one.
//!
//! Every claim about the outbound request is made against bytes the fixture
//! read off the wire with its own parser. Every claim about the inbound
//! answer is made against the bytes a plain `TcpStream` client received.

mod host;
use host::Ledger;

use std::io::{Read, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Mutex, MutexGuard};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use inference_gateway::gateway::{Gateway, Route, Upstream, UpstreamBackend};
use inference_gateway::provider::Provider;
use inference_gateway::routing::evidence::Outcome;
use inference_gateway::routing::{AssignedModel, Cost, CredentialId};
use inference_gateway::secret::{EnvironmentSecretStore, Secret, SecretRef, SecretStore};
use serde_json::{Value, json};

/// A planted provider credential, unique to this test binary. Never a real
/// key, and asserted on so that `!contains` has something to bite.
const PLANTED_KEY: &str = "AIza-planted-gemini-000111222333";
const CREDENTIAL_VAR: &str = "GLASSHOUSE_GATEWAY_TRANSLATE_GEMINI_TEST_KEY";

/// A body that no test wants to see anywhere but where it was planted.
const PLANTED_PROMPT: &str = "PLANTED-PROMPT-TEXT-FOR-GEMINI-TESTS";

const CLIENT_TIMEOUT: Duration = Duration::from_secs(20);

/// The model the clients ask for, and therefore the path segment the fixture
/// must receive.
const MODEL: &str = "gemini-2.5-pro";

// --- a fixture that speaks only Gemini -----------------------------------------

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

/// How the fixture answers a `:generateContent` POST.
#[derive(Clone, Copy)]
enum Answer {
    /// One `GenerateContentResponse` with a text part and two function
    /// calls, finishing `STOP` — the ambiguity the codec has to resolve.
    Document,
    /// The same as an SSE chunk stream, pausing before the chunk that
    /// carries `finishReason` until [`GeminiOnlyUpstream::release`].
    GatedStream,
    /// A stream that stops before any `finishReason` ever arrives.
    TruncatedStream,
    /// A document carrying a key the codecs refuse, whatever the target —
    /// so a client that receives it verbatim proves nothing decoded it.
    /// The relay case, which by definition reaches the same inference
    /// target a translated request would.
    RelayMarker,
}

/// A canned Gemini upstream: `POST /v1beta/models/<model>:generateContent`
/// (or `:streamGenerateContent`) is answered in that protocol's shape, and
/// every request is recorded. Any other target is answered with a document
/// carrying a key the codecs refuse, so a relayed request can be told from a
/// translated one by what the client receives.
struct GeminiOnlyUpstream {
    address: SocketAddr,
    connections: Arc<AtomicUsize>,
    requests: Arc<Mutex<Vec<RecordedRequest>>>,
    gate: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
    accept: Option<JoinHandle<()>>,
}

impl GeminiOnlyUpstream {
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

    /// The base URL the `gemini` template declares, pointed at the fixture:
    /// the **bare host**, because the codec states `/v1beta` itself and a
    /// relayed Gemini target carries it too. A base URL holding it as well
    /// composes `/v1beta/v1beta/…` — which is exactly what the relay test
    /// below caught before this convention was settled.
    fn base_url(&self) -> String {
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

impl Drop for GeminiOnlyUpstream {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        self.gate.store(true, Ordering::Relaxed);
        if let Some(accept) = self.accept.take() {
            let _ = accept.join();
        }
    }
}

/// The candidate every non-streamed answer carries: a text part and two
/// function calls, finishing `STOP`.
fn document_answer() -> String {
    json!({
        "candidates": [{
            "content": {
                "role": "model",
                "parts": [
                    {"text": "Checking."},
                    {"functionCall": {"name": "Bash", "args": {"command": "ls"}}},
                    {"functionCall": {"name": "Read", "args": {"file_path": "/tmp/x"}}},
                ],
            },
            "finishReason": "STOP",
            "index": 0,
            "safetyRatings": [],
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
    .to_string()
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
    if matches!(answer, Answer::RelayMarker) || !request.target.starts_with("/v1beta/models/") {
        // Not a Gemini inference target: answer with a document the codecs
        // would refuse, so a client that receives it verbatim proves nothing
        // decoded it.
        let body = r#"{"candidates":[{"content":{"role":"model","parts":[{"text":"relayed"}]},"finishReason":"STOP"}],"planted_marker_the_codecs_refuse":true}"#;
        write_document(&mut stream, "200 OK", body);
        return;
    }
    match answer {
        // Answered above: this variant never reaches the inference shapes.
        Answer::RelayMarker => {}
        Answer::Document => write_document(&mut stream, "200 OK", &document_answer()),
        Answer::GatedStream | Answer::TruncatedStream => {
            let head = "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ntransfer-encoding: chunked\r\n\r\n";
            let _ = stream.write_all(head.as_bytes());
            let chunk = |value: Value| format!("data: {value}\n\n");
            let before = [
                chunk(json!({
                    "candidates": [{"content": {"role": "model", "parts": [{"text": "Check"}]}}],
                    "modelVersion": "gemini-2.5-pro-001",
                    "responseId": "resp-fixture",
                })),
                chunk(json!({
                    "candidates": [{"content": {"role": "model", "parts": [{"text": "ing."}]}}],
                })),
                chunk(json!({
                    "candidates": [{"content": {"role": "model", "parts": [
                        {"functionCall": {"name": "Bash", "args": {"command": "ls"}}},
                    ]}}],
                })),
            ];
            for event in before {
                write_chunk(&mut stream, event.as_bytes());
            }
            if matches!(answer, Answer::GatedStream) {
                // Pause here until the test has seen those events arrive at
                // the client: what proves the stream was translated as a
                // stream.
                let deadline = Instant::now() + Duration::from_secs(20);
                while !gate.load(Ordering::Relaxed) && Instant::now() < deadline {
                    std::thread::sleep(Duration::from_millis(5));
                }
                let last = chunk(json!({
                    "candidates": [{"content": {"role": "model", "parts": []}, "finishReason": "STOP"}],
                    "usageMetadata": {
                        "promptTokenCount": 40,
                        "candidatesTokenCount": 12,
                        "cachedContentTokenCount": 8,
                        "totalTokenCount": 60,
                    },
                }));
                write_chunk(&mut stream, last.as_bytes());
            }
            // A `streamGenerateContent` stream has no terminator event; the
            // socket closing is the end. `TruncatedStream` closes here,
            // having never sent a `finishReason`.
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

/// The **production** `gemini` template pointed at the fixture, with the
/// planted credential in this file's own variable — the same
/// `[providers.x] template = "gemini"` a user writes.
fn gemini_provider(fixture: &GeminiOnlyUpstream) -> Provider {
    let mut provider = inference_gateway::provider::templates()
        .into_iter()
        .find(|provider| provider.name == "gemini")
        .expect("the gemini template exists");
    assert_eq!(
        provider.protocols.len(),
        1,
        "the gemini template serves exactly one protocol"
    );
    assert_eq!(
        provider.protocols[0].base_url, "https://generativelanguage.googleapis.com",
        "the template's base URL is the bare host; the codec owns the version segment, or a \
         relayed target composes it twice"
    );
    provider.name = "gemini".to_owned();
    provider.protocols[0].base_url = fixture.base_url();
    provider.credential_env = vec![CREDENTIAL_VAR.to_owned()];
    provider
}

/// Every set→resolve→remove of [`CREDENTIAL_VAR`] happens under this lock —
/// the environment is process state and this binary's tests run in parallel.
static ENV_LOCK: Mutex<()> = Mutex::new(());

fn env_lock() -> MutexGuard<'static, ()> {
    ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// `profile::gateway_upstream`, the production builder, run with the planted
/// credential set only for the duration of the call.
fn upstream_from(provider: &Provider) -> Upstream {
    let _guard = env_lock();
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
    upstream.expect("one gemini provider with a resolvable credential builds an upstream")
}

/// A hand-built backend with one route per `(protocol, targets, base URL)` —
/// the refused-pair and byte-for-byte cases.
fn upstream_serving(routes: &[(&str, &'static [&'static str], &str)]) -> Upstream {
    let _guard = env_lock();
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

/// The door the shipped binary calls, with a gateway-backed profile and the
/// given upstream.
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

// --- the three harness clients --------------------------------------------------

fn request(target: &str, token: &str, extra: &str, body: &str) -> Vec<u8> {
    format!(
        "POST {target} HTTP/1.1\r\n\
         Host: 127.0.0.1\r\n\
         Authorization: Bearer {token}\r\n\
         Content-Type: application/json\r\n\
         {extra}\
         Content-Length: {}\r\n\
         \r\n\
         {body}",
        body.len()
    )
    .into_bytes()
}

/// Claude Code after one tool round: a system prompt, a tool definition, the
/// assistant's tool call, the user's erroring result carrying that call's id,
/// and a follow-up. `metadata.user_id` is present because Claude Code always
/// sends it — the field this codec drops by name.
fn claude_code_body(stream: bool) -> String {
    json!({
        "model": MODEL,
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

/// A Codex-shaped (`openai-responses`) client after the same tool round.
fn codex_body(stream: bool) -> String {
    json!({
        "model": MODEL,
        "instructions": PLANTED_PROMPT,
        "input": [
            {"type": "message", "role": "user", "content": [{"type": "input_text", "text": "List the files."}]},
            {"type": "message", "role": "assistant", "content": [{"type": "output_text", "text": "Sure."}]},
            {"type": "function_call", "call_id": "call_prior_1", "name": "Bash", "arguments": "{\"command\": \"ls /nope\"}"},
            {"type": "function_call_output", "call_id": "call_prior_1", "output": "ls: cannot access '/nope'"},
            {"type": "message", "role": "user", "content": [{"type": "input_text", "text": "Try again."}]}
        ],
        "tools": [{
            "type": "function",
            "name": "Bash",
            "description": "Run a shell command",
            "parameters": {"type": "object", "properties": {"command": {"type": "string"}}, "required": ["command"]}
        }],
        "tool_choice": "auto",
        "stream": stream
    })
    .to_string()
}

/// An OpenCode-shaped (`openai-chat`) client after the same tool round.
fn opencode_body(stream: bool) -> String {
    json!({
        "model": MODEL,
        "messages": [
            {"role": "system", "content": PLANTED_PROMPT},
            {"role": "user", "content": "List the files."},
            {"role": "assistant", "content": "Sure.", "tool_calls": [
                {"id": "call_prior_1", "type": "function", "function": {"name": "Bash", "arguments": "{\"command\": \"ls /nope\"}"}}
            ]},
            {"role": "tool", "tool_call_id": "call_prior_1", "content": "ls: cannot access '/nope'"},
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
        "stream": stream
    })
    .to_string()
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

/// `(event name or "", data)` for every event in an SSE text. `[DONE]` is
/// kept as a named marker rather than parsed as JSON.
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
            let value = if data.trim() == "[DONE]" {
                json!("[DONE]")
            } else {
                serde_json::from_str(&data)
                    .unwrap_or_else(|_| panic!("every event is JSON, got {data:?}"))
            };
            (event.unwrap_or_default(), value)
        })
        .collect()
}

/// The event names of an SSE text, taking the `type` field for wires whose
/// events are named in the document rather than in the frame.
fn event_names(events: &[(String, Value)]) -> Vec<String> {
    events
        .iter()
        .map(|(name, data)| {
            if name.is_empty() {
                data.get("type")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
                    .unwrap_or_else(|| data.as_str().unwrap_or("chunk").to_owned())
            } else {
                name.clone()
            }
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

/// What every translated request must look like on the wire, whichever
/// harness protocol it came from: the model in the path, the credential in
/// `x-goog-api-key` and **only** there, and the tool round translated with
/// the result matched to its call by name.
fn assert_gemini_request(received: &RecordedRequest, streamed: bool, erroring_result: bool) {
    assert_eq!(received.method, "POST");
    assert_eq!(
        received.target,
        if streamed {
            format!("/v1beta/models/{MODEL}:streamGenerateContent?alt=sse")
        } else {
            format!("/v1beta/models/{MODEL}:generateContent")
        },
        "the model is a path segment on this wire, and a streamed request is a different method"
    );
    assert_eq!(
        received.header("x-goog-api-key"),
        Some(PLANTED_KEY),
        "Google takes its key in this header"
    );
    assert_eq!(
        received.header("authorization"),
        None,
        "the key is not also presented as an OAuth bearer token, which is what Google would \
         read `authorization` as"
    );
    assert_eq!(
        received.header("anthropic-version"),
        None,
        "an Anthropic-only header is not forwarded to a Google provider"
    );
    assert_eq!(
        received.header("content-length"),
        Some(received.body.len().to_string().as_str())
    );

    let sent = received.json();
    assert_eq!(
        sent["systemInstruction"],
        json!({"parts": [{"text": PLANTED_PROMPT}]})
    );
    assert_eq!(
        sent.get("user"),
        None,
        "the end-user identifier has no Gemini field and is dropped by name, not carried"
    );
    assert_eq!(
        sent.get("model"),
        None,
        "the model addresses the path on this wire and is not repeated in the body"
    );
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
    // Google's own convention for a function response: `output` for a
    // success, `error` for a failure. Claude Code's wire has an `is_error`
    // flag and states it; the two OpenAI wires carry the failure in the
    // result's text and this codec is told nothing, which is a property of
    // those wires and not of this one.
    let payload = if erroring_result {
        json!({"error": "ls: cannot access '/nope'"})
    } else {
        json!({"output": "ls: cannot access '/nope'"})
    };
    assert_eq!(
        contents[2]["parts"][0],
        json!({"functionResponse": {"name": "Bash", "response": payload}}),
        "Gemini matches a result to its call by NAME, resolved through the tool-use block \
         carrying the id the harness sent"
    );
    assert_eq!(contents[2]["parts"][1], json!({"text": "Try again."}));
    assert_eq!(contents.len(), 3);
    assert_eq!(
        sent["tools"],
        json!([{"functionDeclarations": [{
            "name": "Bash",
            "description": "Run a shell command",
            "parameters": {"type": "object", "properties": {"command": {"type": "string"}}, "required": ["command"]},
        }]}]),
        "the harness's own schema, carried as given"
    );
    assert_eq!(
        sent["toolConfig"],
        json!({"functionCallingConfig": {"mode": "AUTO"}})
    );
}

// --- (1) anthropic-messages -> gemini-generate-content ---------------------------

/// Line 1948, 1949 and 1950 for the first Gemini pair, as one exchange. The
/// fixture — which speaks only Gemini — receives a well-formed
/// `:generateContent` request at the model's own path with the tool round
/// translated; the client receives an Anthropic Messages response whose
/// `tool_use` ids are the ones this gateway minted for calls Gemini issued
/// no id for; the answer stops for `tool_use` even though Gemini said
/// `STOP`; and the exchange is recorded under the pair's own name with the
/// provider's exact usage.
#[test]
fn a_claude_code_request_is_translated_to_generate_content_and_the_answer_back_with_tool_calls_matched_by_name()
 {
    let fixture = GeminiOnlyUpstream::start(Answer::Document);
    let ledger = ledger_fixture();
    let gateway = start_gateway(
        upstream_from(&gemini_provider(&fixture)),
        Some(Arc::clone(&ledger.ledger)),
    );
    assert_eq!(
        gateway.served_protocols(),
        vec!["gemini-generate-content"],
        "the fixture-backed provider must serve Gemini and nothing else, or the request below \
         is relayed rather than translated"
    );
    gateway.routing().bind(
        "claude-code",
        "gemini-generate-content",
        AssignedModel::HarnessDefault,
        &gateway
            .upstream()
            .expect("a started gateway has its upstream"),
    );

    let response = send_and_read(
        gateway.address(),
        &request(
            "/v1/messages?beta=true",
            gateway.token().expose(),
            "Anthropic-Version: 2023-06-01\r\nUser-Agent: claude-cli/2.1.245\r\n",
            &claude_code_body(false),
        ),
    );
    let (head, body) = head_and_body(&response);
    assert!(head.starts_with("HTTP/1.1 200"), "{head}");
    assert!(head.contains("content-type: application/json"), "{head}");

    // (a) what the fixture received.
    let received = fixture.only_request();
    assert_gemini_request(&received, false, true);
    assert_eq!(received.header("user-agent"), Some("claude-cli/2.1.245"));
    assert_eq!(
        received.json()["generationConfig"],
        json!({"maxOutputTokens": 4096})
    );
    assert!(
        !received
            .headers
            .iter()
            .any(|(_, value)| value.contains(gateway.token().expose())),
        "the gateway's own token never leaves the process"
    );

    // (b) what the client received.
    let answer: Value = serde_json::from_slice(body).expect("an Anthropic JSON document");
    assert_eq!(answer["type"], "message");
    assert_eq!(answer["role"], "assistant");
    assert_eq!(answer["id"], "resp-fixture");
    assert_eq!(answer["model"], "gemini-2.5-pro-001");
    assert_eq!(
        answer["stop_reason"], "tool_use",
        "Gemini says STOP for a candidate of function calls, and a harness told `end_turn` \
         there stops instead of running the tool"
    );
    let content = answer["content"].as_array().expect("content blocks");
    assert_eq!(content[0], json!({"type": "text", "text": "Checking."}));
    assert_eq!(
        content[1],
        json!({"type": "tool_use", "id": "gemini-call-1-Bash", "name": "Bash", "input": {"command": "ls"}}),
        "Gemini issues no call id, so the harness is given one this gateway minted from the \
         call's own position and name"
    );
    assert_eq!(
        content[2],
        json!({"type": "tool_use", "id": "gemini-call-2-Read", "name": "Read", "input": {"file_path": "/tmp/x"}})
    );
    assert_eq!(content.len(), 3, "two parallel tool calls, both delivered");
    assert_ne!(
        content[1]["id"], content[2]["id"],
        "two calls in one answer never share an id, or a result would run the wrong tool"
    );
    assert_eq!(
        answer["usage"],
        json!({
            "input_tokens": 32,
            "output_tokens": 12,
            "cache_read_input_tokens": 8,
            "output_tokens_details": {"thinking_tokens": 2},
        }),
        "the prompt count includes the cached tokens and Anthropic's input_tokens does not; \
         the output count includes the reasoning tokens Gemini reports apart, and those \
         reach the harness as the subscription broker's thinking_tokens"
    );
    assert!(
        !String::from_utf8_lossy(&response).contains(PLANTED_KEY),
        "the provider credential never appears in a translated response"
    );

    // Recorded under the pair's own name, with the provider's exact usage.
    let rows = wait_for_row(
        &ledger.ledger,
        "gemini",
        MODEL,
        Some("anthropic-messages->gemini-generate-content"),
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

/// The same pair with `stream: true`: the fixture's chunks become Anthropic's
/// events **as they arrive** — the client holds the tool block's start while
/// the fixture is still paused before the chunk carrying `finishReason` — in
/// Anthropic's order, and the response is chunk-terminated.
#[test]
fn a_streamed_claude_code_request_is_translated_chunk_by_chunk_in_anthropics_order() {
    let fixture = GeminiOnlyUpstream::start(Answer::GatedStream);
    let gateway = start_gateway(upstream_from(&gemini_provider(&fixture)), None);

    let mut client = send(
        gateway.address(),
        &request(
            "/v1/messages?beta=true",
            gateway.token().expose(),
            "Anthropic-Version: 2023-06-01\r\n",
            &claude_code_body(true),
        ),
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
        text_so_far.contains("\"id\":\"gemini-call-0-Bash\""),
        "the tool block started with its minted id before the stream finished: {text_so_far}"
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
    assert_eq!(
        event_names(&events),
        vec![
            "message_start",
            "content_block_start",
            "content_block_delta",
            "content_block_delta",
            "content_block_stop",
            "content_block_start",
            "content_block_delta",
            "content_block_stop",
            "message_delta",
            "message_stop",
        ],
        "Anthropic's event order, each block stopped before the next starts"
    );
    assert_eq!(events[0].1["message"]["id"], "resp-fixture");
    assert_eq!(events[0].1["message"]["role"], "assistant");
    assert_eq!(
        events[1].1["content_block"],
        json!({"type": "text", "text": ""})
    );
    let text: String = [&events[2].1, &events[3].1]
        .iter()
        .map(|event| event["delta"]["text"].as_str().unwrap())
        .collect();
    assert_eq!(
        text, "Checking.",
        "each chunk's fragment left on the chunk that carried it"
    );
    assert_eq!(
        events[5].1["content_block"],
        json!({"type": "tool_use", "id": "gemini-call-0-Bash", "name": "Bash", "input": {}})
    );
    assert_eq!(
        serde_json::from_str::<Value>(events[6].1["delta"]["partial_json"].as_str().unwrap())
            .unwrap(),
        json!({"command": "ls"}),
        "Gemini sends a call's arguments whole, so there is one fragment and it is complete"
    );
    assert_eq!(events[8].1["delta"]["stop_reason"], "tool_use");
    assert_eq!(events[8].1["usage"]["output_tokens"], 12);
    assert_eq!(events[8].1["usage"]["input_tokens"], 32);
    assert_eq!(events[9].1, json!({"type": "message_stop"}));

    // ... and the fixture was asked for a stream at the streaming method.
    assert_gemini_request(&fixture.only_request(), true, true);
}

// --- (2) openai-responses -> gemini-generate-content -----------------------------

/// A Codex-shaped client on a Gemini entitlement: the same request reaches
/// the fixture, and the answer comes back as a Responses document whose
/// function-call items carry the minted ids.
#[test]
fn a_codex_shaped_request_is_translated_to_generate_content_and_back() {
    let fixture = GeminiOnlyUpstream::start(Answer::Document);
    let gateway = start_gateway(upstream_from(&gemini_provider(&fixture)), None);

    let response = send_and_read(
        gateway.address(),
        &request(
            "/responses",
            gateway.token().expose(),
            "",
            &codex_body(false),
        ),
    );
    let (head, body) = head_and_body(&response);
    assert!(head.starts_with("HTTP/1.1 200"), "{head}");
    assert_gemini_request(&fixture.only_request(), false, false);

    let answer: Value = serde_json::from_slice(body).expect("a Responses JSON document");
    assert_eq!(answer["object"], "response");
    assert_eq!(answer["id"], "resp-fixture");
    assert_eq!(answer["model"], "gemini-2.5-pro-001");
    assert_eq!(
        answer["status"], "completed",
        "a candidate of function calls is a completed response on this wire — Responses has \
         no stop reason for a tool call, and the function-call items below are the whole of \
         how Codex is told to run one"
    );
    assert_eq!(answer["incomplete_details"], Value::Null);
    let output = answer["output"].as_array().expect("output items");
    let calls: Vec<&Value> = output
        .iter()
        .filter(|item| item["type"] == "function_call")
        .collect();
    assert_eq!(calls.len(), 2, "two parallel calls, both delivered");
    assert_eq!(calls[0]["call_id"], "gemini-call-1-Bash");
    assert_eq!(calls[0]["name"], "Bash");
    assert_eq!(
        serde_json::from_str::<Value>(calls[0]["arguments"].as_str().unwrap()).unwrap(),
        json!({"command": "ls"})
    );
    assert_eq!(calls[1]["call_id"], "gemini-call-2-Read");
    assert_eq!(
        answer["usage"],
        json!({
            "input_tokens": 40,
            "output_tokens": 12,
            "total_tokens": 52,
            "input_tokens_details": {"cached_tokens": 8},
            "output_tokens_details": {"reasoning_tokens": 2},
        })
    );
    assert!(!String::from_utf8_lossy(&response).contains(PLANTED_KEY));
}

/// The same pair streamed: the Responses wire's own event order, with the
/// function-call item added before its arguments and the whole thing closed
/// by `response.completed`.
#[test]
fn a_streamed_codex_shaped_request_comes_back_in_the_responses_wires_own_order() {
    let fixture = GeminiOnlyUpstream::start(Answer::GatedStream);
    let gateway = start_gateway(upstream_from(&gemini_provider(&fixture)), None);
    fixture.release();

    let response = send_and_read(
        gateway.address(),
        &request(
            "/responses",
            gateway.token().expose(),
            "",
            &codex_body(true),
        ),
    );
    let (head, body) = head_and_body(&response);
    assert!(head.starts_with("HTTP/1.1 200"), "{head}");
    assert!(head.contains("content-type: text/event-stream"), "{head}");
    let events = sse_events(&String::from_utf8(dechunk(body)).expect("UTF-8 events"));
    let names = event_names(&events);
    assert_eq!(
        names.first().map(String::as_str),
        Some("response.created"),
        "{names:?}"
    );
    assert_eq!(
        names.last().map(String::as_str),
        Some("response.completed"),
        "{names:?}"
    );
    let added = names
        .iter()
        .position(|name| {
            name == "response.output_item.added"
                && events[names.iter().position(|n| n == name).unwrap()].1["item"]["type"]
                    == "function_call"
        })
        .or_else(|| {
            events.iter().position(|(_, data)| {
                data["type"] == "response.output_item.added"
                    && data["item"]["type"] == "function_call"
            })
        })
        .expect("the function-call item is added");
    let delta = events
        .iter()
        .position(|(_, data)| data["type"] == "response.function_call_arguments.delta")
        .expect("its arguments follow");
    assert!(
        added < delta,
        "an item is added before its arguments stream: {names:?}"
    );
    assert_eq!(
        events[added].1["item"]["call_id"], "gemini-call-0-Bash",
        "the minted id is the call_id the harness will answer with"
    );
    assert_gemini_request(&fixture.only_request(), true, false);
}

// --- (3) openai-chat -> gemini-generate-content ----------------------------------

/// An OpenCode-shaped client on a Gemini entitlement, as a document.
#[test]
fn an_opencode_request_is_translated_to_generate_content_and_back() {
    let fixture = GeminiOnlyUpstream::start(Answer::Document);
    let gateway = start_gateway(upstream_from(&gemini_provider(&fixture)), None);

    let response = send_and_read(
        gateway.address(),
        &request(
            "/v1/chat/completions",
            gateway.token().expose(),
            "",
            &opencode_body(false),
        ),
    );
    let (head, body) = head_and_body(&response);
    assert!(head.starts_with("HTTP/1.1 200"), "{head}");
    assert_gemini_request(&fixture.only_request(), false, false);

    let answer: Value = serde_json::from_slice(body).expect("a Chat JSON document");
    assert_eq!(answer["object"], "chat.completion");
    assert_eq!(answer["id"], "resp-fixture");
    assert_eq!(
        answer["choices"][0]["finish_reason"], "tool_calls",
        "a candidate of function calls finishes for tool calls on this wire too"
    );
    let calls = answer["choices"][0]["message"]["tool_calls"]
        .as_array()
        .expect("tool calls");
    assert_eq!(calls[0]["id"], "gemini-call-1-Bash");
    assert_eq!(calls[0]["function"]["name"], "Bash");
    assert_eq!(calls[1]["id"], "gemini-call-2-Read");
    assert_eq!(
        answer["usage"],
        json!({
            "prompt_tokens": 40,
            "completion_tokens": 12,
            "total_tokens": 52,
            "prompt_tokens_details": {"cached_tokens": 8},
            "completion_tokens_details": {"reasoning_tokens": 2},
        })
    );
    assert!(!String::from_utf8_lossy(&response).contains(PLANTED_KEY));
}

/// The same pair streamed, in OpenAI Chat's own order and terminated by
/// `[DONE]`.
#[test]
fn an_opencode_request_is_translated_to_generate_content_and_streamed_back_in_chats_order() {
    let fixture = GeminiOnlyUpstream::start(Answer::GatedStream);
    let gateway = start_gateway(upstream_from(&gemini_provider(&fixture)), None);
    fixture.release();

    let response = send_and_read(
        gateway.address(),
        &request(
            "/v1/chat/completions",
            gateway.token().expose(),
            "",
            &opencode_body(true),
        ),
    );
    let (head, body) = head_and_body(&response);
    assert!(head.starts_with("HTTP/1.1 200"), "{head}");
    assert!(head.contains("content-type: text/event-stream"), "{head}");
    let events = sse_events(&String::from_utf8(dechunk(body)).expect("UTF-8 events"));
    assert_eq!(
        events.last().map(|(_, data)| data.clone()),
        Some(json!("[DONE]")),
        "OpenAI Chat's stream ends with its own terminator, whatever the provider's wire did"
    );
    let text: String = events
        .iter()
        .filter_map(|(_, data)| data["choices"][0]["delta"]["content"].as_str())
        .collect();
    assert_eq!(text, "Checking.");
    let call = events
        .iter()
        .find_map(|(_, data)| {
            let call = &data["choices"][0]["delta"]["tool_calls"][0];
            call["id"].as_str().map(|_| call.clone())
        })
        .expect("the tool call opens with its id");
    assert_eq!(call["id"], "gemini-call-0-Bash");
    assert_eq!(call["function"]["name"], "Bash");
    let finish: Vec<&str> = events
        .iter()
        .filter_map(|(_, data)| data["choices"][0]["finish_reason"].as_str())
        .collect();
    assert_eq!(finish, vec!["tool_calls"]);
    assert_gemini_request(&fixture.only_request(), true, false);
}

// --- (4) refused by name, nothing opened upstream --------------------------------

/// A Gemini-shaped request at the ingress against an Anthropic-only provider
/// is refused **by name**, with the reason the table actually holds — no
/// installed harness speaks Gemini at the ingress (T3b) — and nothing is
/// opened upstream. Then the endpoint rule and the truncated stream.
#[test]
fn a_gemini_shaped_request_at_the_ingress_is_refused_by_name_and_nothing_is_opened_upstream() {
    let anthropic_only = GeminiOnlyUpstream::start(Answer::Document);
    let gateway = start_gateway(
        upstream_serving(&[(
            "anthropic-messages",
            &["/messages"],
            &format!("http://{}", anthropic_only.address),
        )]),
        None,
    );

    let body = json!({"contents": [{"role": "user", "parts": [{"text": "hi"}]}]}).to_string();
    let response = send_and_read(
        gateway.address(),
        &request(
            &format!("/v1beta/models/{MODEL}:generateContent"),
            gateway.token().expose(),
            "",
            &body,
        ),
    );
    let (head, answer) = head_and_body(&response);
    assert!(head.starts_with("HTTP/1.1 404"), "{head}");
    let message = String::from_utf8_lossy(answer).into_owned();
    assert!(
        message.contains("gemini-generate-content->anthropic-messages"),
        "the refusal names the pair: {message}"
    );
    assert!(
        message.contains("T3b") && message.contains("no supported client speaks"),
        "the refusal carries the reason that is TRUE — a missing adapter, not a missing test: \
         {message}"
    );
    assert_eq!(
        anthropic_only.connections(),
        0,
        "a refused pair opens nothing upstream"
    );

    // The endpoint rule: `:countTokens` is this protocol's own surface and
    // is not translated.
    let response = send_and_read(
        gateway.address(),
        &request(
            &format!("/v1beta/models/{MODEL}:countTokens"),
            gateway.token().expose(),
            "",
            "{}",
        ),
    );
    let (head, answer) = head_and_body(&response);
    assert!(head.starts_with("HTTP/1.1 404"), "{head}");
    assert!(
        String::from_utf8_lossy(answer)
            .contains("only its `/models/{model}:generateContent` endpoint is translated"),
        "{}",
        String::from_utf8_lossy(answer)
    );
    assert_eq!(anthropic_only.connections(), 0);
}

/// A request the pair cannot carry is refused in the harness's own error
/// shape, naming the pair and the field, with nothing opened upstream — and
/// a provider stream cut before its finish reason is a `502` rather than a
/// truncated answer wearing `end_turn`.
#[test]
fn a_request_the_pair_cannot_carry_and_a_truncated_stream_are_both_refused_by_name() {
    let fixture = GeminiOnlyUpstream::start(Answer::Document);
    let gateway = start_gateway(upstream_from(&gemini_provider(&fixture)), None);

    // Claude Code's `disable_parallel_tool_use`: Gemini has no parameter for
    // it, so it is refused rather than answered as though it had not asked.
    let body = json!({
        "model": MODEL,
        "max_tokens": 10,
        "messages": [{"role": "user", "content": "hi"}],
        "tools": [{"name": "Bash", "input_schema": {"type": "object"}}],
        "tool_choice": {"type": "auto", "disable_parallel_tool_use": true},
    })
    .to_string();
    let response = send_and_read(
        gateway.address(),
        &request("/v1/messages", gateway.token().expose(), "", &body),
    );
    let (head, answer) = head_and_body(&response);
    assert!(head.starts_with("HTTP/1.1 400"), "{head}");
    let error: Value = serde_json::from_slice(answer).expect("an Anthropic error document");
    assert_eq!(error["type"], "error");
    let message = error["error"]["message"].as_str().unwrap();
    assert!(
        message.contains("anthropic-messages->gemini-generate-content"),
        "{message}"
    );
    assert!(message.contains("`parallel_tool_calls`"), "{message}");
    assert_eq!(
        fixture.connections(),
        0,
        "a refused request opens nothing upstream"
    );

    // A model name a path segment cannot carry is refused before the
    // request line could be built out of it.
    let body = json!({
        "model": "../../v1beta/models/other",
        "max_tokens": 10,
        "messages": [{"role": "user", "content": "hi"}],
    })
    .to_string();
    let response = send_and_read(
        gateway.address(),
        &request("/v1/messages", gateway.token().expose(), "", &body),
    );
    let (head, answer) = head_and_body(&response);
    assert!(head.starts_with("HTTP/1.1 400"), "{head}");
    assert!(
        String::from_utf8_lossy(answer).contains("`model`"),
        "{}",
        String::from_utf8_lossy(answer)
    );
    assert_eq!(fixture.connections(), 0);

    // A stream the provider cut before any finish reason: refused, not
    // completed. `streamGenerateContent` has no terminator event, so this is
    // the only thing that tells the two apart.
    let truncating = GeminiOnlyUpstream::start(Answer::TruncatedStream);
    let gateway = start_gateway(upstream_from(&gemini_provider(&truncating)), None);
    let response = send_and_read(
        gateway.address(),
        &request(
            "/v1/messages",
            gateway.token().expose(),
            "",
            &claude_code_body(true),
        ),
    );
    let text = String::from_utf8_lossy(&response).into_owned();
    assert!(
        text.contains("truncated and not finished"),
        "a stream with no finish reason must be refused down the stream rather than completed: \
         {text}"
    );
    assert!(
        !text.contains("\"stop_reason\":\"end_turn\""),
        "a cut stream must never be delivered wearing a stop reason nobody sent: {text}"
    );
}

// --- (5) a served target is still relayed byte for byte --------------------------

/// The relay rule, narrowed and not repealed, for the fourth protocol: a
/// provider that serves Gemini natively gets the client's body **untouched**
/// — a body the codec would refuse (`safetySettings`) and would re-serialise
/// (odd spacing, non-ASCII) — at the Gemini route, and the client gets the
/// provider's document verbatim, a key the codecs refuse included.
#[test]
fn a_gemini_target_the_provider_serves_natively_is_relayed_byte_for_byte_even_though_a_codec_exists()
 {
    let fixture = GeminiOnlyUpstream::start(Answer::RelayMarker);
    let gateway = start_gateway(upstream_from(&gemini_provider(&fixture)), None);
    assert_eq!(gateway.served_protocols(), vec!["gemini-generate-content"]);

    let body = "{ \"contents\":[{\"role\":\"user\" ,\"parts\":[{\"text\":\"ünïcödé — 日本語\"}]}] ,  \"safetySettings\": [{\"category\":\"HARM_CATEGORY_HARASSMENT\",\"threshold\":\"BLOCK_NONE\"}] }";
    let response = send_and_read(
        gateway.address(),
        &request(
            "/v1beta/models/gemini-2.5-flash:generateContent",
            gateway.token().expose(),
            "",
            body,
        ),
    );

    let received = fixture.only_request();
    assert_eq!(
        received.target, "/v1beta/models/gemini-2.5-flash:generateContent",
        "a served target keeps the path the client asked for, model included"
    );
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
        Some(format!("Bearer {PLANTED_KEY}").as_str()),
        "the relay attaches the credential exactly as it always has; only the TRANSLATED path \
         moves it into x-goog-api-key"
    );
    assert_eq!(received.header("x-goog-api-key"), None);

    let (head, answer) = head_and_body(&response);
    assert!(head.starts_with("HTTP/1.1 200"), "{head}");
    assert!(
        String::from_utf8_lossy(answer).contains("\"planted_marker_the_codecs_refuse\":true"),
        "the provider's document reached the client verbatim: {}",
        String::from_utf8_lossy(answer)
    );
}

// --- the recorded limits, pinned so they fail the day they are lifted -----------

/// The limit this package states rather than hides, pinned so that the
/// package which lifts it has to come here and say so: nothing translates
/// **out of** Gemini, because the ingress half needs a harness that speaks it.
#[test]
fn the_recorded_limit_of_this_package_is_what_the_code_actually_does() {
    use inference_gateway::routing::wire::WireProtocol;

    for to in [
        WireProtocol::AnthropicMessages,
        WireProtocol::OpenAiResponses,
        WireProtocol::OpenAiChat,
    ] {
        assert!(
            !inference_gateway::provider::translation_available(
                WireProtocol::GeminiGenerateContent,
                to
            ),
            "nothing translates out of Gemini until a harness speaks it (T3b): {to}"
        );
        assert!(
            inference_gateway::provider::translation_available(
                to,
                WireProtocol::GeminiGenerateContent
            ),
            "every harness protocol translates INTO Gemini: {to}"
        );
    }
}
