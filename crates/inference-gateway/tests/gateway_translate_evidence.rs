//! GH-TRANSLATED-USAGE-PROOF (map line 1333): proof, not production change,
//! that a translated exchange's *stated* usage reaches
//! `routing_observations` — through a
//! real socket and a real [`Gateway`], the same door
//! `tests/gateway_translate.rs` and `gateway::conformance` already enter.
//!
//! # Why this file exists at all
//!
//! `tests/gateway_translate.rs`'s
//! `a_claude_code_request_is_translated_to_chat_completions_and_the_answer_back_with_ids_preserved`
//! already reads the row back and asserts `input_tokens == Some(32)`,
//! `output_tokens == Some(12)`, `cached_input_tokens == Some(8)` for a
//! translated exchange (lines 802-821 there) — the recon this packet was
//! dispatched from (`report-recon-33a-32g.md`, 1333) said no such test
//! existed; it does, and it is the mutation-1 witness (below) already. So the
//! genuinely missing half — the one this file adds — is the **relay** side.
//!
//! **It was added as the *restraint* half and inverted on 2026-09-03.** As
//! written, a relayed exchange whose body carried a real `usage` object had
//! to write `NULL` to all three columns, because the gateway never read that
//! body. The user then approved reading usage and timing out of *supported*
//! relayed bodies, `GH-RELAY-USAGE` built it, and the assertion became a pin
//! on replaced behaviour. It now asserts the provider's own digits; what
//! survives unchanged is that the relay **invents** nothing, and the cases
//! where a column is legitimately empty moved to `tests/relay_usage.rs`,
//! which can still say *why* it is empty. Nothing in `gateway/conformance.rs` or
//! `gateway_translate.rs` asserts that; grepped for
//! `input_tokens|output_tokens|cached_input_tokens` in the former, and there
//! is a relay-through-a-translate-capable-provider test in the latter
//! (`a_target_the_provider_serves_natively_is_relayed_byte_for_byte...`) that
//! never opens a ledger at all.
//!
//! Both tests below are still written out, self-contained, so this file
//! stands as the one place both halves of 1333's translated slice are proven
//! together, and so mutation 1 has a witness that does not depend on another
//! file's test staying unchanged.

mod host;
use host::Ledger;

use std::io::{Read, Write};
use std::net::{Ipv4Addr, Shutdown, SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use inference_gateway::gateway::{Gateway, Route, Upstream, UpstreamBackend};
use inference_gateway::routing::evidence::{HELPER_PURPOSE, Outcome};
use inference_gateway::routing::{AssignedModel, Cost, CredentialId};
use inference_gateway::secret::{EnvironmentSecretStore, SecretRef, SecretStore};

/// A planted provider credential, unique to this test binary. Never a real
/// key.
const PLANTED_KEY: &str = "sk-planted-translate-evidence-000111222333";
const CREDENTIAL_VAR: &str = "GLASSHOUSE_GATEWAY_TRANSLATE_EVIDENCE_TEST_KEY";

const CLIENT_TIMEOUT: Duration = Duration::from_secs(20);

/// Every set→resolve→remove of [`CREDENTIAL_VAR`] happens under this lock —
/// same shape as `gateway_translate.rs`'s `ENV_LOCK`, and needed for the same
/// reason: the environment is process state shared by every test in this
/// binary.
static ENV_LOCK: Mutex<()> = Mutex::new(());

// --- a one-shot fixture upstream: records the request, answers with a fixed body ---

#[derive(Debug, Clone)]
struct RecordedRequest {
    target: String,
}

/// A loopback TCP server that answers every connection with the same
/// preset body, and records what it received. One connection is all either
/// test below drives.
struct FixtureUpstream {
    address: SocketAddr,
    requests: Arc<Mutex<Vec<RecordedRequest>>>,
    stop: Arc<std::sync::atomic::AtomicBool>,
    accept: Option<JoinHandle<()>>,
}

impl FixtureUpstream {
    fn answering(status: &'static str, body: &'static str) -> Self {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("loopback is bindable");
        let address = listener.local_addr().expect("bound");
        listener.set_nonblocking(true).expect("polling mode");
        let requests = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let accept = std::thread::spawn({
            let requests = Arc::clone(&requests);
            let stop = Arc::clone(&stop);
            move || {
                while !stop.load(Ordering::Relaxed) {
                    match listener.accept() {
                        Ok((stream, _)) => {
                            let requests = Arc::clone(&requests);
                            std::thread::spawn(move || serve_one(stream, &requests, status, body));
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

    /// The base URL an OpenAI-compatible provider declares: with `/v1`.
    fn openai_base_url(&self) -> String {
        format!("http://{}/v1", self.address)
    }

    /// A base URL with no path, for a route whose target list carries the
    /// full path (the relay case).
    fn root_url(&self) -> String {
        format!("http://{}", self.address)
    }

    fn only_request(&self) -> RecordedRequest {
        let requests = self.requests.lock().unwrap();
        assert_eq!(requests.len(), 1, "exactly one request reached the fixture");
        requests[0].clone()
    }
}

impl Drop for FixtureUpstream {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(accept) = self.accept.take() {
            let _ = accept.join();
        }
    }
}

fn serve_one(
    mut stream: TcpStream,
    requests: &Mutex<Vec<RecordedRequest>>,
    status: &str,
    body: &str,
) {
    stream.set_nonblocking(false).expect("blocking mode");
    let _ = stream.set_read_timeout(Some(Duration::from_secs(60)));
    let Some(request) = read_request(&mut stream) else {
        return;
    };
    requests.lock().unwrap().push(request);
    let _ = write!(
        stream,
        "HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\r\n{body}",
        body.len()
    );
    let _ = stream.flush();
    let _ = stream.shutdown(Shutdown::Write);
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
    let target = request_line.split(' ').nth(1)?.to_owned();
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
    Some(RecordedRequest { target })
}

// --- the gateway, built the way the binary builds it, same shape as gateway_translate.rs ---

/// A hand-built backend with one route — reachable directly via
/// `UpstreamBackend::new`, no provider template needed since neither test
/// here drives `profile::gateway_upstream`.
fn upstream_serving(protocol: &str, targets: &'static [&'static str], base_url: &str) -> Upstream {
    let _guard = ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    // SAFETY: `CREDENTIAL_VAR` is unique to this test binary, set and removed
    // around the one resolve that reads it, under `ENV_LOCK` for the whole
    // window — the same pattern `gateway_translate.rs::upstream_serving` uses.
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

/// The project's evidence ledger, opened through the same bootstrap the
/// binary uses, so the row an exchange writes can be read back.
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
/// profile and the given upstream — same wrapper `gateway_translate.rs` uses.
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

fn messages_request_with_helper_purpose(token: &str, body: &str) -> Vec<u8> {
    let request = messages_request(token, body);
    let split = request
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .expect("request head terminator");
    let mut with_header = request[..split].to_vec();
    with_header.extend_from_slice(b"\r\nX-Glasshouse-Purpose: helper");
    with_header.extend_from_slice(&request[split..]);
    with_header
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

/// A minimal Anthropic-messages harness request: no tools, no cache_control,
/// nothing a codec would refuse — the same request drives both tests below,
/// only the upstream's served protocol differs.
const HARNESS_BODY: &str =
    r#"{"model":"claude-x","max_tokens":10,"messages":[{"role":"user","content":"hi"}]}"#;

/// Line 1333, positive half — re-proven here alongside its restraint half so
/// mutation 1 (deleting `.with_tokens(...)`'s real argument in
/// `gateway/session.rs:478`) has a witness in this file, independent of
/// `tests/gateway_translate.rs`. A harness-protocol (anthropic-messages)
/// request against an upstream that serves a *different* supported protocol
/// (openai-chat) enters `translate::serve`; the fixture's response carries a
/// real, distinct `usage` object, and the row must equal it exactly.
#[test]
fn a_translated_exchanges_stated_usage_reaches_the_routing_row() {
    let fixture = FixtureUpstream::answering(
        "200 OK",
        r#"{"id":"chatcmpl-fixture","object":"chat.completion","created":1,"model":"fixture-model","choices":[{"index":0,"message":{"role":"assistant","content":"hi there"},"finish_reason":"stop","logprobs":null}],"usage":{"prompt_tokens":40,"completion_tokens":12,"total_tokens":52,"prompt_tokens_details":{"cached_tokens":8}}}"#,
    );
    let ledger = ledger_fixture();
    let upstream = upstream_serving(
        "openai-chat",
        &["/chat/completions"],
        &fixture.openai_base_url(),
    );
    let gateway = start_gateway(upstream, Arc::clone(&ledger.ledger));
    assert_eq!(
        gateway.served_protocols(),
        vec!["openai-chat"],
        "the fixture-backed provider must serve only openai-chat, or the request below is \
         relayed rather than translated"
    );
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
        &messages_request_with_helper_purpose(gateway.token().expose(), HARNESS_BODY),
    );
    assert!(
        status_line(&response).starts_with("HTTP/1.1 200"),
        "{}",
        status_line(&response)
    );

    // The fixture received a chat-completions request, confirming
    // translation actually happened rather than a relay.
    assert_eq!(fixture.only_request().target, "/v1/chat/completions");

    let rows = wait_for_row(
        &ledger.ledger,
        "fixture",
        "claude-x",
        Some("anthropic-messages->openai-chat"),
        Some("claude-code"),
    );
    assert_eq!(
        rows.len(),
        1,
        "one routing observation for the translated exchange"
    );
    assert_eq!(rows[0].outcome, Some(Outcome::Succeeded));
    assert_eq!(rows[0].purpose.as_deref(), Some(HELPER_PURPOSE));
    assert_eq!(
        rows[0].input_tokens,
        Some(32),
        "prompt_tokens (40) minus cached_tokens (8) is Anthropic's input_tokens"
    );
    assert_eq!(rows[0].output_tokens, Some(12));
    assert_eq!(rows[0].cached_input_tokens, Some(8));
}

/// Line 1333, relay half. A harness-protocol request against an upstream
/// serving the harness's *own* protocol natively is relayed byte for byte,
/// and the row records the usage that body **states**.
///
/// # This test was inverted on 2026-09-03, deliberately
///
/// It was written as the *restraint* half — the gateway never decodes a
/// relayed body, so a real `usage` object in the response had to reach
/// `NULL` columns, and fabricating `Some(Tokens{..})` on the relay path had
/// to turn it red. The user then approved the gateway reading usage and
/// timing out of **supported** relayed bodies under six constraints
/// (`design-decisions.md`, *Steering decisions of record*), and
/// `GH-RELAY-USAGE` built it. Asserting `NULL` here would now pin the
/// behaviour that approval replaced.
///
/// **What survives, and is what this test proves:** the relay copies the
/// provider's own digits and invents nothing. The restraint half it used to
/// carry moved to the tests that can still express it —
/// `relay_usage::a_protocol_whose_usage_spelling_is_unknown_records_no_usage`,
/// `::a_supported_protocol_that_states_no_usage_records_none`, and
/// `::a_truncated_stream_records_no_usage_however_much_of_it_arrived` —
/// where "no known spelling" and "stated nothing" are the reasons a column
/// is empty, rather than "could not look".
#[test]
fn a_relayed_exchange_records_the_usage_its_body_states_and_invents_none() {
    let fixture = FixtureUpstream::answering(
        "200 OK",
        r#"{"type":"message","id":"msg_relayed","role":"assistant","model":"fixture-model","content":[{"type":"text","text":"hi there"}],"stop_reason":"end_turn","stop_sequence":null,"usage":{"input_tokens":999,"output_tokens":888}}"#,
    );
    let ledger = ledger_fixture();
    let upstream = upstream_serving("anthropic-messages", &["/messages"], &fixture.root_url());
    let gateway = start_gateway(upstream, Arc::clone(&ledger.ledger));
    assert_eq!(gateway.served_protocols(), vec!["anthropic-messages"]);
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
        &messages_request(gateway.token().expose(), HARNESS_BODY),
    );
    assert!(
        status_line(&response).starts_with("HTTP/1.1 200"),
        "{}",
        status_line(&response)
    );
    // The client received the provider's document verbatim, usage object
    // included — this is what makes the row's NULLs below a proof of
    // restraint rather than an accident of a body the gateway could not
    // have read anyway.
    assert!(
        String::from_utf8_lossy(&response).contains("\"input_tokens\":999"),
        "the relayed body must carry a real usage object for the NULL row below to mean anything"
    );

    let rows = wait_for_row(
        &ledger.ledger,
        "fixture",
        "claude-x",
        Some("anthropic-messages"),
        Some("claude-code"),
    );
    assert_eq!(
        rows.len(),
        1,
        "one routing observation for the relayed exchange"
    );
    assert_eq!(rows[0].outcome, Some(Outcome::Succeeded));
    // **This assertion was inverted on 2026-09-03, and the inversion is the
    // point of the rename above.** It used to read `None`, with the reason
    // "a relayed exchange's body is never read; nothing may be invented for
    // it". The first clause stopped being true when the user approved the
    // gateway reading usage and timing out of *supported* relayed bodies
    // (`design-decisions.md`, *Steering decisions of record*;
    // `evidence/phase-33a.md`, *The relay's "never supplied" is lifted*).
    // The second clause never stopped being true and is what this test still
    // proves: these are the provider's own digits, copied, not derived.
    //
    // The route is `anthropic-messages`, whose usage spelling is known, and
    // the body above states 999/888 — so exact digits are the correct
    // outcome. `cached_input_tokens` stays `None` because this body states
    // no cache-read figure, and a figure nobody stated is never filled in.
    assert_eq!(
        rows[0].input_tokens,
        Some(999),
        "a supported relayed route records the digits the provider stated"
    );
    assert_eq!(rows[0].output_tokens, Some(888));
    assert_eq!(
        rows[0].cached_input_tokens, None,
        "this body states no cache-read figure, and an unstated count is never invented"
    );
}
