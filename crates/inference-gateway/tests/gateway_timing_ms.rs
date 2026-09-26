//! `GH-STREAM-TIMING-MS` (`crate::database` migration 25; map lines 1347,
//! 1348, 1349 and 1355): the four millisecond offsets, measured through a
//! real socket and a real [`Gateway`] — the same door
//! `tests/gateway_first_events.rs` and `tests/gateway_tool_rounds.rs` enter,
//! with the same fixture, timed in milliseconds instead of unix seconds.
//!
//! # What each test here is the proof of, and what it is not
//!
//! The offsets exist because a one-second timestamp cannot express a time to
//! first token. So the assertions are about **resolution and zero**, and
//! they are written so that the two ways of getting either wrong fail:
//!
//! - **The zero must be the send, not the hand-off.** `dispatched_at`, the
//!   seconds column, is stamped in the gateway's accept loop the moment a
//!   connection is handed to `ingress::serve` — before the request head is
//!   read, before its body is read, and before anything is sent anywhere.
//!   The offsets' zero is the send itself. [`PRE_SEND_DELAY`] is planted
//!   between the two, by a client that writes its head, waits, and only then
//!   writes its body, and the assertion is that the first-byte offset does
//!   **not** contain that wait.
//! - **The clock must be monotonic, not the wall.** A `first_token_ms`
//!   computed from two `now_unix_seconds()` readings is always an exact
//!   multiple of 1,000, because that is all a one-second clock can say. The
//!   fixture's 1.2s pauses put the honest reading strictly between two
//!   second boundaries, and [`WALL_CLOCK_WINDOW_MS`] is the window that
//!   contains the honest reading and excludes both values a wall clock could
//!   have produced.
//!
//! What this file does not prove: `effective TTFC` (line 1351/1352's term)
//! has no producer in this build at all and nothing here measures one; the
//! support-work rows `main.rs::record_extraction_observation` writes keep
//! their seconds and are untouched; and one protocol pair is driven, as in
//! the two sibling files, because the mechanism is pair-agnostic by
//! construction.
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

/// Local stand-in for the ledger's own (now-internal) `ObservationQuery`:
/// this file filters exactly these four fields client-side against
/// [`Ledger::observations_in_window`], the successor to the deleted
/// `Ledger::recent`.
#[derive(Debug, Clone, Copy)]
struct ObservationQuery<'a> {
    provider: &'a str,
    model: &'a str,
    route: Option<&'a str>,
    harness: Option<&'a str>,
}

fn recent(
    ledger: &Ledger,
    query: ObservationQuery<'_>,
    _limit: usize,
) -> Vec<inference_gateway::routing::evidence::RoutingObservation> {
    ledger
        .observations_in_window(i64::MAX, i64::MAX)
        .expect("read the ledger")
        .into_iter()
        .filter(|row| {
            row.provider == query.provider
                && row.model == query.model
                && row.route.as_deref() == query.route
                && row.harness.as_deref() == query.harness
        })
        .collect()
}
use inference_gateway::routing::{AssignedModel, Cost, CredentialId};
use inference_gateway::secret::{EnvironmentSecretStore, SecretRef, SecretStore};
use serde_json::{Value, json};

/// A planted provider credential, unique to this test binary. Never a real
/// key.
const PLANTED_KEY: &str = "sk-planted-timing-ms-000444555666";
const CREDENTIAL_VAR: &str = "GLASSHOUSE_GATEWAY_TIMING_MS_TEST_KEY";

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
    /// Answer with the status and headers immediately and the body only
    /// after `pause` — the provider-side shape that separates *the headers
    /// arrived* from *the exchange ended* by a gap the relay can measure
    /// without reading a byte of the body.
    fn answering_after(body: String, pause: Duration) -> Self {
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
                            std::thread::spawn(move || serve_document(stream, &body, pause));
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

fn serve_document(mut stream: TcpStream, body: &str, pause: Duration) {
    stream.set_nonblocking(false).expect("blocking mode");
    let _ = stream.set_read_timeout(Some(Duration::from_secs(60)));
    read_request(&mut stream);
    let _ = write!(
        stream,
        "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\r\n",
        body.len()
    );
    let _ = stream.flush();
    if !pause.is_zero() {
        std::thread::sleep(pause);
    }
    let _ = stream.write_all(body.as_bytes());
    let _ = stream.flush();
    let _ = stream.shutdown(Shutdown::Write);
}

// --- a streaming fixture: writes chat-completion chunks in order, with real pauses --

/// Which chunks [`serve_stream`] writes, after the head and the SSE comment
/// every script sends first.
#[derive(Clone, Copy)]
enum Script {
    /// A whitespace-only delta, a real pause, real text, a real pause, then
    /// a tool call — `gateway_first_events.rs`'s own shape, timed here in
    /// milliseconds instead of seconds.
    TextThenToolCallWithPauses,
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
/// holds.
///
/// Long enough that a unix-second clock reading taken before and after can
/// never round to the same second — which is what
/// `gateway_first_events.rs` needs it for — and, for this file, long enough
/// that the millisecond offsets it produces land strictly *between* two
/// second boundaries. That is the whole discriminator against a wall clock:
/// see [`WALL_CLOCK_WINDOW_MS`].
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
    query: ObservationQuery<'_>,
) -> Vec<inference_gateway::routing::evidence::RoutingObservation> {
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    loop {
        let rows = recent(ledger, query, 10);
        if !rows.is_empty() || std::time::Instant::now() >= deadline {
            return rows;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// How long the client waits between writing its request head and writing
/// its body — planted delay that falls entirely **between** the accept
/// loop's `dispatched_at` and the send, because `translate::serve` reads the
/// whole harness body before it builds the upstream request.
///
/// Mutation target `zero-at-handoff`: moving `let dispatch = Instant::now()`
/// from immediately before `agent.run` to the top of `translate::serve` (or
/// to the accept loop, which is earlier still) folds this wait into
/// `first_byte_ms`, and
/// [`a_translated_streamed_exchange_measures_the_offsets_from_the_send`]
/// asserts it is not there.
const PRE_SEND_DELAY: Duration = Duration::from_millis(1200);

/// The window an honest `first_token_ms` falls in, given the fixture's one
/// [`PAUSE`] between the first byte and the first real text delta.
///
/// The honest reading is a few milliseconds (loopback, the fixture answers
/// its head immediately) plus 1,200 — call it 1,200 to 1,400, and this
/// window leaves 500ms of slack above that for a loaded machine.
///
/// Mutation target `wall-clock-offset`: computing the offset from two
/// `now_unix_seconds()` readings instead of from the `Instant` can only ever
/// answer an exact multiple of 1,000 — here 1,000 or 2,000 depending on
/// where in the second the exchange started — and **both** fall outside this
/// window. That is why the window is stated rather than a bare `>= 1000`
/// gap: a gap of one second is exactly what the mutant can also produce.
const WALL_CLOCK_WINDOW_MS: std::ops::RangeInclusive<i64> = 1_200..=1_900;

/// Send a request whose head arrives, then a pause, then its body — the
/// planted [`PRE_SEND_DELAY`].
///
/// Two writes with a sleep between them, rather than one: the gateway reads
/// the head to completion (`\r\n\r\n`) and only afterwards reads
/// `content-length` bytes of body, so a client that withholds the body
/// stalls the exchange at exactly the point this file needs it stalled.
fn send_with_a_pause_before_the_body(address: SocketAddr, token: &str, body: &str) -> Vec<u8> {
    let mut client = TcpStream::connect(address).expect("the gateway accepts connections");
    client
        .set_read_timeout(Some(CLIENT_TIMEOUT))
        .expect("a non-zero timeout is valid");
    let head = format!(
        "POST /v1/messages?beta=true HTTP/1.1\r\n\
         Host: 127.0.0.1\r\n\
         Authorization: Bearer {token}\r\n\
         Content-Type: application/json\r\n\
         Anthropic-Version: 2023-06-01\r\n\
         Content-Length: {}\r\n\
         \r\n",
        body.len()
    );
    client.write_all(head.as_bytes()).expect("the head is read");
    client.flush().expect("flush");
    std::thread::sleep(PRE_SEND_DELAY);
    client.write_all(body.as_bytes()).expect("the body is read");
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

/// An address nothing listens on — a connection attempt that is refused, so
/// the exchange is refused before any request leaves and there is no
/// monotonic zero to measure anything from.
fn unreachable_address() -> SocketAddr {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("loopback is bindable");
    let address = listener.local_addr().expect("bound");
    drop(listener);
    address
}

// ---------------------------------------------------------------------------
// (a) A translated streamed exchange: the four offsets, their zero, and
//     their resolution — with every seconds column unchanged beside them.
// ---------------------------------------------------------------------------

/// The one test the two mutations are aimed at. A translated stream with
/// 1.2s before the first real token and another 1.2s before the tool-use
/// block, behind a client that waits 1.2s between its head and its body:
///
/// - the four offsets are ordered and carry the two real gaps;
/// - `first_byte_ms` does **not** contain the pre-send wait (`zero-at-handoff`);
/// - `first_token_ms` is not a multiple of a second (`wall-clock-offset`);
/// - every seconds column still says exactly what it said before migration
///   25 — the offsets are beside them, never instead of them.
#[test]
fn a_translated_streamed_exchange_measures_the_offsets_from_the_send() {
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

    let response = send_with_a_pause_before_the_body(
        gateway.address(),
        gateway.token().expose(),
        &harness_body(true),
    );
    assert!(
        status_line(&response).starts_with("HTTP/1.1 200"),
        "{}",
        status_line(&response)
    );

    let rows = wait_for_row(
        &ledger.ledger,
        ObservationQuery {
            provider: "fixture",
            model: "claude-x",
            route: Some("anthropic-messages->openai-chat"),
            harness: Some("claude-code"),
        },
    );
    assert_eq!(rows.len(), 1, "one routing observation for the exchange");
    let row = &rows[0];
    assert_eq!(row.outcome, Some(Outcome::Succeeded));

    let first_byte_ms = row
        .first_byte_ms
        .expect("a translated exchange that reached the provider measures its first byte");
    let first_token_ms = row
        .first_token_ms
        .expect("the real text delta must have stamped first_token_ms");
    let first_tool_call_ms = row
        .first_tool_call_ms
        .expect("the tool-use block must have stamped first_tool_call_ms");
    let completed_ms = row
        .completed_ms
        .expect("an exchange that ended measures when it ended");

    // Ordered, and never negative — the property migration 25's `CHECK`
    // refuses to store and a monotonic clock cannot produce.
    assert!(
        first_byte_ms >= 0
            && first_byte_ms <= first_token_ms
            && first_token_ms <= first_tool_call_ms
            && first_tool_call_ms <= completed_ms,
        "the four offsets must be ordered and non-negative: \
         first_byte_ms={first_byte_ms}, first_token_ms={first_token_ms}, \
         first_tool_call_ms={first_tool_call_ms}, completed_ms={completed_ms}"
    );

    // The two planted pauses, as real gaps.
    assert!(
        first_token_ms - first_byte_ms >= 1_000,
        "the 1.2s pause before the real text delta must show up as a real gap: \
         first_byte_ms={first_byte_ms}, first_token_ms={first_token_ms}"
    );
    assert!(
        first_tool_call_ms - first_token_ms >= 1_000,
        "the 1.2s pause before the tool call must show up as a real gap: \
         first_token_ms={first_token_ms}, first_tool_call_ms={first_tool_call_ms}"
    );

    // `zero-at-handoff`. The client held its body for 1.2s, which the
    // gateway spent inside `translate::serve` waiting to read it — before
    // the send, and therefore outside every one of these offsets.
    assert!(
        first_byte_ms < 500,
        "the offsets' zero is the send, so the client's {}ms pre-send wait must not be \
         inside first_byte_ms={first_byte_ms}",
        PRE_SEND_DELAY.as_millis()
    );

    // `wall-clock-offset`. A one-second clock can only answer in whole
    // thousands; the honest reading sits between two of them.
    assert!(
        WALL_CLOCK_WINDOW_MS.contains(&first_token_ms),
        "first_token_ms={first_token_ms} must fall strictly between the two second \
         boundaries a wall clock could have answered ({:?})",
        WALL_CLOCK_WINDOW_MS
    );
    assert!(
        first_token_ms % 1_000 != 0,
        "a millisecond offset that is an exact multiple of a second is a one-second clock \
         wearing millisecond units: first_token_ms={first_token_ms}"
    );

    // And the seconds columns are what they always were — the offsets ride
    // beside them, and this package changed no producer of any of them.
    let dispatched = row
        .dispatched_at_unix
        .expect("the accept loop always stamps the hand-off");
    let completed_at = row
        .completed_at_unix
        .expect("the accept loop always stamps the completion");
    let first_byte_at = row
        .first_byte_at_unix
        .expect("a translated exchange that reached the provider has a first byte");
    let first_token_at = row
        .first_token_at_unix
        .expect("the real text delta must still stamp first_token_at");
    let first_tool_call_at = row
        .first_tool_call_at_unix
        .expect("the tool-use block must still stamp first_tool_call_at");
    assert!(
        dispatched <= first_byte_at
            && first_byte_at <= first_token_at
            && first_token_at <= first_tool_call_at
            && first_tool_call_at <= completed_at,
        "the seconds columns keep their own order: dispatched={dispatched}, \
         first_byte_at={first_byte_at}, first_token_at={first_token_at}, \
         first_tool_call_at={first_tool_call_at}, completed_at={completed_at}"
    );
    assert!(
        first_token_at - first_byte_at >= 1,
        "the seconds columns still show the pause they always showed"
    );
    // The hand-off is genuinely earlier than the send, which is what makes
    // the `first_byte_ms < 500` assertion above mean something rather than
    // being true by coincidence.
    assert!(
        completed_at - dispatched >= 3,
        "the hand-off precedes the send by the planted wait, so the seconds span is \
         longer than the offsets': dispatched={dispatched}, completed_at={completed_at}"
    );
    assert!(
        (completed_at - dispatched) * 1_000 > completed_ms,
        "the seconds span starts at the hand-off and the measured one at the send, so the \
         measured one is the shorter: seconds={}, completed_ms={completed_ms}",
        (completed_at - dispatched) * 1_000
    );
    assert_eq!(
        row.duration_ms(),
        Some(completed_ms),
        "duration_ms prefers the measured completion over the seconds difference"
    );
}

// ---------------------------------------------------------------------------
// (b) A relayed exchange: the two offsets its path can measure, and NULL for
//     the two only a decoded stream can supply.
// ---------------------------------------------------------------------------

/// The relay never decodes a body, so it has no first token and no first
/// tool call to measure — exactly as it has no `first_token_at` and no
/// `first_tool_call_at`. What it does know is when the provider's headers
/// came back and when it stopped moving bytes, and the provider here holds
/// its body for [`PAUSE`] between those two, so the gap between the two
/// offsets is a real one the relay measured without reading a byte.
///
/// **The relay's zero is not provable the way the translated path's is, and
/// this test does not claim it is.** `ingress::forward` hands the client's
/// socket to `ureq` as a lazy body reader, so a client that withholds its
/// body stalls *inside* `agent.run` rather than before it — the wait is
/// genuinely part of this exchange's time to first byte, and
/// [`PRE_SEND_DELAY`] is therefore planted only in the translated test
/// above. What is asserted here is the pair of offsets the relay can
/// honestly supply, the pair it cannot, and the real gap between them.
#[test]
fn a_relayed_exchange_measures_the_first_byte_and_the_completion_and_neither_token_offset() {
    let fixture = DocumentUpstream::answering_after(
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
        PAUSE,
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
        "the relayed body must carry a real tool call for the NULL offsets below to mean \
         anything"
    );

    let rows = wait_for_row(
        &ledger.ledger,
        ObservationQuery {
            provider: "fixture",
            model: "claude-x",
            route: Some("anthropic-messages"),
            harness: Some("claude-code"),
        },
    );
    assert_eq!(rows.len(), 1);
    let row = &rows[0];

    let first_byte_ms = row
        .first_byte_ms
        .expect("the relay reads the provider's headers and knows when they arrived");
    let completed_ms = row
        .completed_ms
        .expect("the relay knows when it stopped moving bytes");
    assert!(
        first_byte_ms >= 0 && first_byte_ms <= completed_ms,
        "first_byte_ms={first_byte_ms}, completed_ms={completed_ms}"
    );
    assert!(
        completed_ms - first_byte_ms >= 1_000,
        "the provider's 1.2s pause before its body must show up as a real gap between the \
         headers and the end: first_byte_ms={first_byte_ms}, completed_ms={completed_ms}"
    );
    assert!(
        WALL_CLOCK_WINDOW_MS.contains(&completed_ms),
        "completed_ms={completed_ms} must fall strictly between the two second boundaries a \
         wall clock could have answered ({:?})",
        WALL_CLOCK_WINDOW_MS
    );
    assert_eq!(
        row.first_token_ms, None,
        "a relayed exchange's body is never read; nothing may be invented for it"
    );
    assert_eq!(row.first_tool_call_ms, None);
    assert_eq!(
        row.duration_ms(),
        Some(completed_ms),
        "duration_ms prefers the measured completion here too"
    );
}

// ---------------------------------------------------------------------------
// (c) A refused exchange: no request left, so nothing was measured.
// ---------------------------------------------------------------------------

/// An exchange that never reached a provider has no monotonic zero, and
/// therefore no offset of any kind — not `0`, which would be a claim that
/// the provider answered instantly.
#[test]
fn an_exchange_that_never_reached_a_provider_measures_none_of_the_four_offsets() {
    let ledger = ledger_fixture();
    let upstream = upstream_serving(
        "anthropic-messages",
        &["/messages"],
        &format!("http://{}", unreachable_address()),
    );
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
    assert!(
        status_line(&response).starts_with("HTTP/1.1 502"),
        "an unreachable provider relays as a 502: {}",
        status_line(&response)
    );

    let rows = wait_for_row(
        &ledger.ledger,
        ObservationQuery {
            provider: "fixture",
            model: "fixture-model",
            route: Some("anthropic-messages"),
            harness: Some("claude-code"),
        },
    );
    assert_eq!(rows.len(), 1);
    let row = &rows[0];
    assert_eq!(
        (
            row.first_byte_ms,
            row.first_token_ms,
            row.first_tool_call_ms,
            row.completed_ms
        ),
        (None, None, None, None),
        "no request left, so there is nothing any of the four could be an offset from"
    );
    // The seconds columns still bracket the attempt, and `duration_ms`
    // falls back to them — the fallback's own production case.
    assert!(row.dispatched_at_unix.is_some());
    assert!(row.completed_at_unix.is_some());
    assert!(
        row.duration_ms().is_some(),
        "with no measured completion the seconds difference is still the answer"
    );
}
