//! Capability map lines 1316, 1318, 1334, 1364 and 1365, through the shipped
//! gateway.
//!
//! Behavioral contract: given a gateway-backed exchange, when the provider
//! answers or fails to, Glasshouse records **what kind** of failure it was —
//! throttle, exhausted quota, upstream 5xx, timeout, stream abort, empty
//! completion, credential failure, request incompatibility, or unknown — from
//! the status line, the headers, byte counts and timing alone, so rate-limit
//! responses are counted apart from transport and model failures and cadence
//! throttling apart from a spent window, while preserving the relay's rule
//! that it never reads, buffers, or interprets a single byte of response
//! content.
//!
//! Every test here drives a real `TcpStream` through a real gateway started
//! by the real production entry point,
//! `gateway::start_if_required_with_telemetry`, against a stub provider on
//! loopback — practice §35: the capability being closed is the wire from a
//! provider's answer to a classified row, not any one function's unit test.
//! The one class this file cannot drive live is `timeout`: the upstream agent
//! (`gateway::upstream::agent`) sets no timeout, by its own documented
//! decision, so `ureq::Error::Timeout` cannot arise from the shipped binary
//! today. Its mapping is unit-tested beside `classify`; this file records the
//! absence rather than faking the class.

mod host;
use host::Ledger;

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use inference_gateway::gateway::{Gateway, Route, Upstream, UpstreamBackend};
use inference_gateway::provider::telemetry::GatewayQuotaCache;
use inference_gateway::routing::evidence::{FailureClass, Outcome, RoutingObservation};
use inference_gateway::routing::{AssignedModel, Cost, CredentialId};
use inference_gateway::secret::{EnvironmentSecretStore, Secret, SecretRef, SecretStore};

/// The model bound on both ends — the bind call and the request body. A stub
/// never reads it; it has to be the same string in both places.
const MODEL: &str = "stub-model";

// --- fixtures, after `tests/gateway_retry_after.rs` ---------------------------

fn test_credential(var: &str) -> Secret {
    // SAFETY: `var` is unique to the one caller that set it, and it is
    // removed again immediately below, before the resolved value is even
    // inspected, so no other test can observe it set.
    unsafe {
        std::env::set_var(var, "sk-planted-not-a-real-key-taxonomy");
    }
    let resolved = EnvironmentSecretStore::new()
        .resolve(&SecretRef::Environment {
            var: var.to_owned(),
        })
        .expect("the variable was just set");
    unsafe {
        std::env::remove_var(var);
    }
    resolved
}

fn credential_id(provider: &str, var: &str) -> CredentialId {
    CredentialId::new(
        provider,
        SecretRef::Environment {
            var: var.to_owned(),
        },
    )
}

/// One HTTP/1.1 response, `Connection: close` always, so the gateway's
/// pooled agent never tries to reuse a socket this stub has already closed.
fn response(status_line: &str, headers: &[&str], body: &[u8]) -> Vec<u8> {
    let mut head = format!("HTTP/1.1 {status_line}\r\nConnection: close\r\n");
    for header in headers {
        head.push_str(header);
        head.push_str("\r\n");
    }
    head.push_str("\r\n");
    let mut bytes = head.into_bytes();
    bytes.extend_from_slice(body);
    bytes
}

/// A local HTTP server that answers its connections **in order**, one
/// scripted response each, and closes every connection after writing — so a
/// response whose framing promises more than it carries is cut off exactly
/// the way a provider that died mid-stream cuts it off.
///
/// Bounded rather than a plain blocking `accept`: the listener is
/// non-blocking and polled against a deadline, so a gateway that never
/// dialled it fails a test with an assertion instead of hanging the suite.
fn stub_server(responses: Vec<Vec<u8>>) -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").expect("loopback is bindable");
    let address = listener
        .local_addr()
        .expect("a bound listener has a local address");
    listener
        .set_nonblocking(true)
        .expect("a listener can be put in polling mode");

    std::thread::Builder::new()
        .name("gateway-failure-taxonomy-stub".to_owned())
        .spawn(move || {
            for scripted in responses {
                let deadline = Instant::now() + Duration::from_secs(20);
                let mut stream = loop {
                    match listener.accept() {
                        Ok((stream, _peer)) => break Some(stream),
                        Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                            if Instant::now() >= deadline {
                                break None;
                            }
                            std::thread::sleep(Duration::from_millis(5));
                        }
                        Err(_) => break None,
                    }
                };
                let Some(stream) = stream.as_mut() else {
                    return;
                };
                let _ = stream.set_nonblocking(false);
                let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
                // The request is read whole, never parsed: the stub answers
                // its script regardless of what the gateway sent, but it must
                // not close on unread bytes — see `read_whole_request`.
                read_whole_request(stream);
                let _ = stream.write_all(&scripted);
                let _ = stream.flush();
                // Dropped here: the close is the point for every response
                // whose framing promised more.
            }
        })
        .expect("can spawn the stub server thread");

    address
}

/// Read the request whole — its head, then exactly the body its
/// `Content-Length` declares — before the caller answers it.
///
/// This used to be a single `read` into a 4 KiB buffer, on the reasoning
/// that a stub which never parses the request need not read it. That is a
/// race with any client that writes its head and its body separately, and
/// the gateway is one: `ureq` sends the head, then streams the relayed body
/// from the client socket (`gateway::ingress`'s
/// `SendBody::from_owned_reader`). When the stub's single read lands
/// between those two writes it takes the head alone, and the body is still
/// in the socket's receive queue when the stub answers and drops the
/// stream.
///
/// Closing a socket that still holds unread data is an *abortive* close:
/// the stack sends RST instead of FIN. Winsock then discards whatever it
/// had already buffered for the peer, so the gateway's read of the response
/// this stub had just written failed with a connection reset, `agent.run`
/// returned `Err`, and the gateway answered its own `502 Bad Gateway`
/// (`ingress::serve`'s `Outcome::Unreachable`) rather than relaying the
/// scripted status. Unix hands the buffered bytes back first and only
/// reports the reset once they are drained, which is why the same stub was
/// reliable on macOS and Linux and flaked on the Windows ARM64 CI VM.
///
/// Nothing here is conditional on the platform, and no assertion moves:
/// reading a request before answering it is what any HTTP server does, and
/// it is already what `evaluation_producers.rs`'s `serve_json` does in this
/// same suite. On Unix it only reads bytes that were arriving anyway.
fn read_whole_request(stream: &mut TcpStream) {
    let mut reader = BufReader::new(stream);
    let mut declared = 0usize;
    let mut line = String::new();
    loop {
        line.clear();
        match reader.read_line(&mut line) {
            Ok(0) | Err(_) => return,
            Ok(_) => {}
        }
        let line = line.trim_end_matches(['\r', '\n']);
        if line.is_empty() {
            break;
        }
        if let Some((name, value)) = line.split_once(':')
            && name.trim().eq_ignore_ascii_case("content-length")
        {
            declared = value.trim().parse().unwrap_or(0);
        }
    }
    let mut body = vec![0u8; declared];
    let _ = reader.read_exact(&mut body);
}

/// An address nothing listens on: bound, read, and released.
fn refused_address() -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").expect("loopback is bindable");
    listener
        .local_addr()
        .expect("a bound listener has an address")
}

fn messages_request(token: &str) -> Vec<u8> {
    let body = format!(r#"{{"model":"{MODEL}"}}"#);
    format!(
        "POST /v1/messages HTTP/1.1\r\n\
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

/// Send `raw` and return everything the gateway wrote back, to the close.
fn send_and_read(address: SocketAddr, raw: &[u8]) -> Vec<u8> {
    let mut client = TcpStream::connect(address).expect("the gateway accepts connections");
    client
        .set_read_timeout(Some(Duration::from_secs(20)))
        .expect("a non-zero read timeout is valid");
    client
        .write_all(raw)
        .expect("the gateway reads the request");
    client.flush().expect("the gateway reads the request");
    let mut out = Vec::new();
    client
        .read_to_end(&mut out)
        .expect("the gateway answers and then closes");
    out
}

/// An empty in-memory ledger for this test's observations.
fn ledger_at(_base: &Path) -> Arc<Ledger> {
    Ledger::new()
}

fn backend(provider: &str, credential_var: &str, address: SocketAddr) -> UpstreamBackend {
    UpstreamBackend::new(
        provider.to_owned(),
        vec![Route::new(
            "anthropic-messages".to_owned(),
            &["/messages"],
            &format!("http://{address}"),
        )],
        test_credential(credential_var),
        credential_id(provider, credential_var),
        Cost::Metered,
    )
    .expect("a loopback http URL is absolute and this credential is header-safe")
}

/// A gateway over `upstream`, started through the production entry point,
/// recording to `ledger` (and to `quota_cache` when given), with an
/// assignment bound so `record_routing_observation` has an identity to
/// write.
fn gateway_over(
    upstream: Upstream,
    ledger: Option<Arc<Ledger>>,
    quota_cache: Option<GatewayQuotaCache>,
) -> Gateway {
    let gateway = inference_gateway::gateway::start_if_required_with_degrade_sink(
        &[inference_gateway::gateway::BackendDemand::LocalGateway],
        || Ok(upstream),
        quota_cache,
        None,
        ledger.map(|ledger| host::observation_sink(ledger, None)),
        None,
    )
    .expect("loopback is bindable")
    .expect("a gateway-backed profile requires a gateway");

    gateway.routing().bind(
        "claude-code",
        "anthropic-messages",
        AssignedModel::named(MODEL),
        &gateway
            .upstream()
            .expect("a started gateway has its upstream"),
    );
    gateway
}

/// Poll the ledger until `expected` rows exist for `provider`, oldest first —
/// the connection thread writes its row after `ingress::serve` has already
/// closed the client's socket, so `send_and_read` returning is not proof the
/// row is there yet.
fn wait_for_rows(ledger: &Ledger, provider: &str, expected: usize) -> Vec<RoutingObservation> {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let mut rows: Vec<_> = ledger
            .observations_in_window(i64::MAX, i64::MAX)
            .unwrap()
            .into_iter()
            .filter(|row| {
                row.provider == provider
                    && row.model == MODEL
                    && row.route.as_deref() == Some("anthropic-messages")
                    && row.harness.as_deref() == Some("claude-code")
            })
            .collect();
        if rows.len() >= expected || Instant::now() >= deadline {
            rows.sort_by_key(|row| row.seq);
            return rows;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// Claim the first still-unclaimed row whose class is `class`, for the case
/// named `what`. Rows are matched to cases by content, never by position:
/// the connection thread writes its row after the client's socket has
/// closed, and on a Windows runner a stream-abort row landed after the row
/// of the exchange that followed it, so `seq` order is not exchange order.
/// A row carries no status line and no byte count, so its class is the key;
/// cases that expect the same class expect identical rows, and each still
/// claims a distinct one. A case that finds no row fails naming the rows
/// still unclaimed.
fn claim_row_with_class<'a>(
    unclaimed: &mut [Option<&'a RoutingObservation>],
    class: Option<FailureClass>,
    what: &str,
) -> &'a RoutingObservation {
    let Some(position) = unclaimed
        .iter()
        .position(|row| matches!(row, Some(row) if row.failure_class == class))
    else {
        panic!(
            "{what}: no unclaimed row has class {class:?}; unclaimed: {:#?}",
            unclaimed.iter().flatten().collect::<Vec<_>>()
        );
    };
    unclaimed[position]
        .take()
        .expect("`position` was found among the unclaimed rows")
}

/// The other half of [`claim_row_with_class`]: once every case has claimed
/// its row none may be left, so every row was asserted against exactly one
/// case.
fn assert_every_row_claimed(unclaimed: &[Option<&RoutingObservation>]) {
    assert!(
        unclaimed.iter().all(Option::is_none),
        "every row belongs to exactly one case; left over: {:#?}",
        unclaimed.iter().flatten().collect::<Vec<_>>()
    );
}

fn status_of(reply: &[u8]) -> String {
    String::from_utf8_lossy(reply)
        .lines()
        .next()
        .unwrap_or_default()
        .to_owned()
}

// --- line 1364 ---------------------------------------------------------------

/// One provider, one gateway, one scripted response per class the wire can
/// produce, and the row each one left — asserted by class, by the outcome
/// that must agree with it, and by the two line-1334 counters the gateway
/// can honestly write.
#[test]
fn each_failure_class_is_recorded_from_status_headers_and_framing_alone() {
    const PROVIDER: &str = "fixture-taxonomy";
    const CREDENTIAL_VAR: &str = "GLASSHOUSE_FAILURE_TAXONOMY_KEY_EACH";

    let hundred = vec![b'x'; 100];
    // "a 429 with a stated wait" is the one case here that carries a
    // `Retry-After` header, and since ca439cd (map line 1368) `paced_refusal`
    // refuses the *next* request locally, without dialing, whenever the
    // assigned resource is still inside a declared wait and no sibling
    // credential exists to rotate to — which every other case here would be,
    // were this case not last. This file's own row-by-row assertions below
    // require exactly one backend ("one backend, so nothing to fail over
    // to"), so a sibling credential is not an option here (it would cause a
    // real failover and a nonzero `failovers` on the row that follows);
    // ordering this case last is what keeps the single-backend premise true
    // while still reaching every one of the other eight cases. Order here is
    // load-bearing: a case added after this one would inherit its wait.
    let cases: Vec<(&str, Vec<u8>, &str, Option<FailureClass>)> = vec![
        (
            "a 429 whose headers say nothing remains for an hour",
            response(
                "429 Too Many Requests",
                &[
                    "X-RateLimit-Remaining: 0",
                    "X-RateLimit-Reset: 3600",
                    "Content-Length: 0",
                ],
                b"",
            ),
            "HTTP/1.1 429",
            Some(FailureClass::ExhaustedQuota),
        ),
        (
            "a 503",
            response("503 Service Unavailable", &["Content-Length: 0"], b""),
            "HTTP/1.1 503",
            Some(FailureClass::Upstream5xx),
        ),
        (
            "a 200 with a zero-byte body",
            response("200 OK", &["Content-Length: 0"], b""),
            "HTTP/1.1 200",
            Some(FailureClass::EmptyCompletion),
        ),
        (
            "a 200 that closes 100 bytes into a declared 1000",
            response("200 OK", &["Content-Length: 1000"], &hundred),
            "HTTP/1.1 200",
            Some(FailureClass::StreamAbort),
        ),
        (
            "a chunked 200 that closes before its terminating chunk",
            response("200 OK", &["Transfer-Encoding: chunked"], &{
                let mut chunk = b"64\r\n".to_vec();
                chunk.extend_from_slice(&hundred);
                chunk.extend_from_slice(b"\r\n");
                chunk
            }),
            "HTTP/1.1 200",
            Some(FailureClass::StreamAbort),
        ),
        (
            "a 401",
            response("401 Unauthorized", &["Content-Length: 0"], b""),
            "HTTP/1.1 401",
            Some(FailureClass::CredentialFailure),
        ),
        (
            "a 400",
            response("400 Bad Request", &["Content-Length: 0"], b""),
            "HTTP/1.1 400",
            Some(FailureClass::RequestIncompatibility),
        ),
        (
            "a 200 with a whole body",
            response(
                "200 OK",
                &["Content-Type: application/json", "Content-Length: 11"],
                b"{\"ok\":true}",
            ),
            "HTTP/1.1 200",
            None,
        ),
        (
            "a 429 with a stated wait",
            response(
                "429 Too Many Requests",
                &["Retry-After: 2", "Content-Length: 0"],
                b"",
            ),
            "HTTP/1.1 429",
            Some(FailureClass::Throttle),
        ),
    ];

    let tmp = tempfile::tempdir().expect("a temp directory can be created");
    let ledger = ledger_at(tmp.path());
    let upstream_address = stub_server(cases.iter().map(|case| case.1.clone()).collect());
    let upstream =
        Upstream::with_failover(vec![backend(PROVIDER, CREDENTIAL_VAR, upstream_address)])
            .expect("one backend is not none");
    let gateway = gateway_over(upstream, Some(Arc::clone(&ledger)), None);

    for (what, _, relayed_status, _) in &cases {
        let reply = send_and_read(
            gateway.address(),
            &messages_request(gateway.token().expose()),
        );
        assert!(
            status_of(&reply).starts_with(relayed_status),
            "{what}: the gateway must relay the provider's own status: {}",
            status_of(&reply)
        );
    }

    let rows = wait_for_rows(&ledger, PROVIDER, cases.len());
    assert_eq!(
        rows.len(),
        cases.len(),
        "one row per exchange that reached the provider: {rows:#?}"
    );
    let mut unclaimed: Vec<Option<&RoutingObservation>> = rows.iter().map(Some).collect();
    for (what, _, _, expected) in &cases {
        let row = claim_row_with_class(&mut unclaimed, *expected, what);
        assert_eq!(row.failure_class, *expected, "{what}: {row:#?}");
        let expected_outcome = if expected.is_some() {
            Outcome::Failed
        } else {
            Outcome::Succeeded
        };
        assert_eq!(
            row.outcome,
            Some(expected_outcome),
            "{what}: a class exactly when the outcome is not a success: {row:#?}"
        );
        assert_eq!(
            row.retries,
            Some(0),
            "{what}: the gateway forwards exactly once"
        );
        assert_eq!(
            row.failovers,
            Some(0),
            "{what}: one backend, so nothing to fail over to"
        );
        assert_eq!(row.tool_rounds, None, "{what}: never counted at this layer");
        assert_eq!(row.repairs, None, "{what}: never counted at this layer");
        assert!(
            row.first_byte_at_unix.is_some(),
            "{what}: every forwarded exchange has a first byte"
        );
    }
    assert_every_row_claimed(&unclaimed);
}

/// A provider that cannot be reached at all: the transport said "refused",
/// which line 1364's vocabulary has no name for, so `unknown` — and
/// explicitly **not** `timeout`, which no exchange through the shipped agent
/// can produce today (see this file's header).
#[test]
fn a_refused_connection_is_recorded_as_unknown_not_guessed_as_a_timeout() {
    const PROVIDER: &str = "fixture-refused";
    const CREDENTIAL_VAR: &str = "GLASSHOUSE_FAILURE_TAXONOMY_KEY_REFUSED";

    let tmp = tempfile::tempdir().expect("a temp directory can be created");
    let ledger = ledger_at(tmp.path());
    let upstream =
        Upstream::with_failover(vec![backend(PROVIDER, CREDENTIAL_VAR, refused_address())])
            .expect("one backend is not none");
    let gateway = gateway_over(upstream, Some(Arc::clone(&ledger)), None);

    let reply = send_and_read(
        gateway.address(),
        &messages_request(gateway.token().expose()),
    );
    assert!(
        status_of(&reply).starts_with("HTTP/1.1 502"),
        "an unreachable provider is a 502 to the harness: {}",
        status_of(&reply)
    );

    let rows = wait_for_rows(&ledger, PROVIDER, 1);
    assert_eq!(rows.len(), 1, "{rows:#?}");
    assert_eq!(rows[0].failure_class, Some(FailureClass::Unknown));
    assert_ne!(rows[0].failure_class, Some(FailureClass::Timeout));
    assert_eq!(rows[0].outcome, Some(Outcome::Failed));
    assert_eq!(
        rows[0].first_byte_at_unix, None,
        "no response ever arrived, so there is no first byte to time"
    );
}

// --- lines 1364 and 1365: the header rule ------------------------------------

/// The one `429` distinction that is a *reading* rather than a status: a
/// spent window is a `429` whose own headers say nothing remains until a
/// reset beyond the horizon. Everything else about a `429` is a throttle — a
/// stated wait alone, a near reset, something still remaining.
#[test]
fn throttle_and_exhausted_quota_are_told_apart_by_headers_not_guessed() {
    const PROVIDER: &str = "fixture-headers";
    // Two of these cases (the stated two-second wait, and the long-stated-
    // wait exhausted case) each declare their own `Retry-After`, and since
    // ca439cd `paced_refusal` refuses the next request locally, without
    // dialing, when the assigned resource is still inside a declared wait
    // and no sibling credential exists to rotate to. Reordering cannot fix
    // this here the way it does in the previous test: with two different
    // cases each declaring a wait, whichever one is not last still has a
    // case after it that would inherit its cooldown, and one of the waits
    // (1800s) cannot be outlasted by a fast test at all. So each case gets
    // its own credential (all siblings of the same provider, all pointed at
    // the one ordered stub) rather than one shared credential across the
    // sequence. This test makes no assertion about `failovers`, so the real
    // rotation a sibling causes on a failed exchange (`observe_exchange`,
    // not `paced_refusal` itself) is not in tension with anything here —
    // unlike the previous test's "one backend" premise. One spare sibling
    // was not enough: after the first wait forces a rotation, the *second*
    // wait paces the sibling it rotated to, and with only two credentials in
    // the pool that leaves `rotate_from` with nobody again. A fresh,
    // never-yet-failed credential per case guarantees rotation always has
    // somewhere available to go.
    let cases: Vec<(&str, Vec<&str>, FailureClass)> = vec![
        (
            "a stated wait of two seconds",
            vec!["Retry-After: 2"],
            FailureClass::Throttle,
        ),
        (
            "nothing remaining until a reset an hour out",
            vec!["X-RateLimit-Remaining: 0", "X-RateLimit-Reset: 3600"],
            FailureClass::ExhaustedQuota,
        ),
        (
            "nothing remaining but a reset thirty seconds out",
            vec!["X-RateLimit-Remaining: 0", "X-RateLimit-Reset: 30"],
            FailureClass::Throttle,
        ),
        (
            "seven remaining, whatever the reset says",
            vec!["X-RateLimit-Remaining: 7", "X-RateLimit-Reset: 3600"],
            FailureClass::Throttle,
        ),
        (
            "nothing remaining and only a long stated wait",
            vec!["X-RateLimit-Remaining: 0", "Retry-After: 1800"],
            FailureClass::ExhaustedQuota,
        ),
        (
            "no rate-limit header at all",
            vec![],
            FailureClass::Throttle,
        ),
    ];

    let tmp = tempfile::tempdir().expect("a temp directory can be created");
    let ledger = ledger_at(tmp.path());
    let upstream_address = stub_server(
        cases
            .iter()
            .map(|(_, headers, _)| {
                let mut headers = headers.clone();
                headers.push("Content-Length: 0");
                response("429 Too Many Requests", &headers, b"")
            })
            .collect(),
    );
    let backends = (0..cases.len())
        .map(|index| {
            backend(
                PROVIDER,
                &format!("GLASSHOUSE_FAILURE_TAXONOMY_KEY_HEADERS_{index}"),
                upstream_address,
            )
        })
        .collect();
    let upstream = Upstream::with_failover(backends).expect("cases is non-empty");
    let gateway = gateway_over(upstream, Some(Arc::clone(&ledger)), None);

    for _ in &cases {
        let reply = send_and_read(
            gateway.address(),
            &messages_request(gateway.token().expose()),
        );
        assert!(
            status_of(&reply).starts_with("HTTP/1.1 429"),
            "{}",
            status_of(&reply)
        );
    }

    let rows = wait_for_rows(&ledger, PROVIDER, cases.len());
    assert_eq!(rows.len(), cases.len(), "{rows:#?}");
    let mut unclaimed: Vec<Option<&RoutingObservation>> = rows.iter().map(Some).collect();
    for (what, headers, expected) in &cases {
        let row = claim_row_with_class(
            &mut unclaimed,
            Some(*expected),
            &format!("{what} ({headers:?})"),
        );
        assert_eq!(
            row.failure_class,
            Some(*expected),
            "{what} ({headers:?}): {row:#?}"
        );
    }
    assert_every_row_claimed(&unclaimed);
}

// --- line 1334: failovers ----------------------------------------------------

/// The `503` that moved the session is the row that says `failovers = 1`;
/// the assignment afterwards names the other provider; and the routing
/// record calls the change a failover.
#[test]
fn a_failover_caused_by_this_exchange_is_counted_on_its_row() {
    const FIRST: &str = "fixture-failover-first";
    const SECOND: &str = "fixture-failover-second";

    let tmp = tempfile::tempdir().expect("a temp directory can be created");
    let ledger = ledger_at(tmp.path());
    let first_address = stub_server(vec![response(
        "503 Service Unavailable",
        &["Content-Length: 0"],
        b"",
    )]);
    let upstream = Upstream::with_failover(vec![
        backend(
            FIRST,
            "GLASSHOUSE_FAILURE_TAXONOMY_KEY_FO_FIRST",
            first_address,
        ),
        backend(
            SECOND,
            "GLASSHOUSE_FAILURE_TAXONOMY_KEY_FO_SECOND",
            refused_address(),
        ),
    ])
    .expect("two backends is not none");
    let gateway = gateway_over(upstream, Some(Arc::clone(&ledger)), None);
    assert_eq!(
        gateway
            .routing()
            .assignment()
            .map(|a| a.provider().to_owned()),
        Some(FIRST.to_owned())
    );

    let reply = send_and_read(
        gateway.address(),
        &messages_request(gateway.token().expose()),
    );
    assert!(
        status_of(&reply).starts_with("HTTP/1.1 503"),
        "{}",
        status_of(&reply)
    );

    let rows = wait_for_rows(&ledger, FIRST, 1);
    assert_eq!(rows.len(), 1, "{rows:#?}");
    assert_eq!(rows[0].failure_class, Some(FailureClass::Upstream5xx));
    assert_eq!(
        rows[0].failovers,
        Some(1),
        "this exchange's 503 is what moved the session: {rows:#?}"
    );
    assert_eq!(
        gateway
            .routing()
            .assignment()
            .map(|a| a.provider().to_owned()),
        Some(SECOND.to_owned()),
        "the session must now be assigned to the other provider"
    );
    let changes = gateway.routing().changes();
    assert_eq!(changes.len(), 1, "{changes:?}");
    assert_eq!(changes[0].cause.as_str(), "failover");
}

// --- the boundary that stays -------------------------------------------------

#[derive(Clone)]
struct Capture(Arc<Mutex<Vec<u8>>>);

impl std::io::Write for Capture {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0
            .lock()
            .expect("no test panics while holding this")
            .extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for Capture {
    type Writer = Capture;
    fn make_writer(&'a self) -> Capture {
        self.clone()
    }
}

/// A body carrying a planted secret goes through the gateway to the harness
/// and to nowhere else: not a log line, not a row, not any file the project
/// wrote — and the production half of `ingress.rs` contains no call that
/// could decode one.
#[test]
fn a_relayed_body_is_never_read_and_never_leaks_into_the_ledger_or_logs() {
    const PROVIDER: &str = "fixture-secret";
    const CREDENTIAL_VAR: &str = "GLASSHOUSE_FAILURE_TAXONOMY_KEY_SECRET";
    const PLANTED: &str = "sk-test-PLANTED-BODY-SECRET-0xDEADBEEF";

    // Every `tracing` line this process emits from here on lands in `sink`,
    // exchange lines from the gateway's own connection thread included —
    // which is why this is the global default and not a thread-local one.
    let sink = Arc::new(Mutex::new(Vec::new()));
    let subscriber = tracing_subscriber::fmt()
        .with_writer(Capture(Arc::clone(&sink)))
        .with_max_level(tracing::Level::TRACE)
        .with_ansi(false)
        .without_time()
        .finish();
    tracing::subscriber::set_global_default(subscriber)
        .expect("this is the only test in this binary that installs a global subscriber");

    let tmp = tempfile::tempdir().expect("a temp directory can be created");
    let ledger = ledger_at(tmp.path());
    let body = format!("{{\"secret\": \"{PLANTED}\"}}");
    let upstream_address = stub_server(vec![response(
        "200 OK",
        &[
            "Content-Type: application/json",
            &format!("Content-Length: {}", body.len()),
        ],
        body.as_bytes(),
    )]);
    let upstream =
        Upstream::with_failover(vec![backend(PROVIDER, CREDENTIAL_VAR, upstream_address)])
            .expect("one backend is not none");
    let gateway = gateway_over(upstream, Some(Arc::clone(&ledger)), None);

    let reply = send_and_read(
        gateway.address(),
        &messages_request(gateway.token().expose()),
    );
    let reply_text = String::from_utf8_lossy(&reply);
    assert!(
        reply_text.contains(PLANTED),
        "the harness is the one place the body must arrive, intact: {reply_text}"
    );

    let rows = wait_for_rows(&ledger, PROVIDER, 1);
    assert_eq!(rows.len(), 1, "{rows:#?}");
    assert_eq!(rows[0].failure_class, None, "a whole 200 is served");
    assert_eq!(rows[0].outcome, Some(Outcome::Succeeded));
    assert!(
        !format!("{:?}", rows[0]).contains(PLANTED),
        "the row has nowhere to hold a body: {:?}",
        rows[0]
    );

    // The exchange's own log line is written after its row; wait for it so
    // the scan below is over a log that actually contains the line.
    let deadline = Instant::now() + Duration::from_secs(5);
    let captured = loop {
        let captured = String::from_utf8_lossy(&sink.lock().unwrap()).into_owned();
        if captured.contains(PROVIDER) || Instant::now() >= deadline {
            break captured;
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    assert!(
        captured.contains("glasshouse gateway exchange") && captured.contains(PROVIDER),
        "the exchange must have been logged, or the scan below proves nothing: {captured}"
    );
    assert!(
        !captured.contains(PLANTED),
        "a byte of a relayed body reached a log line: {captured}"
    );
    assert!(
        captured.contains("relayed=Some(") && captured.contains("ended=Some(\"complete\")"),
        "the log carries the framing facts — a count and a way of ending: {captured}"
    );

    drop(gateway);

    // And the source: the production half of the relay has no call that
    // could turn body bytes into anything else. Comment lines are dropped,
    // because the module's prose names the very calls it forbids.
    //
    // Both files, since the user's ruling of 2026-09-03: `usage.rs` is where
    // a relayed body is now read at all, so leaving it out would let the
    // rule be evaded by moving one line across a module boundary rather than
    // by arguing for it. What it may do — scan a sliding window for literal
    // key spellings — needs none of these calls, and what it may not do
    // needs all of them.
    let production_code = |source: &str| -> String {
        source
            .split("#[cfg(test)]")
            .next()
            .expect("split always yields at least one part")
            .lines()
            .filter(|line| !line.trim_start().starts_with("//"))
            .collect::<Vec<_>>()
            .join("\n")
    };
    let relay = [
        (
            "gateway/ingress.rs",
            production_code(include_str!(
                "../../inference-gateway/src/gateway/ingress.rs"
            )),
        ),
        (
            "gateway/usage.rs",
            production_code(include_str!("../../inference-gateway/src/gateway/usage.rs")),
        ),
    ];
    assert!(
        relay[0].1.contains("struct Counted") && relay[0].1.contains("struct Framing"),
        "the framing observer must be in the production half, or this scan is vacuous"
    );
    assert!(
        relay[1].1.contains("struct Extractor"),
        "the usage observer must be in the production half, or this scan is vacuous"
    );
    for (name, code) in &relay {
        for needle in [
            "serde_json",
            "::from_slice",
            "from_str(",
            "read_to_string",
            "read_to_end",
            "from_reader",
            "json!",
        ] {
            assert!(
                !code.contains(needle),
                "`{needle}` appeared in the production half of {name}: the relay may count, \
                 time, and read the usage figures a supported provider states, never decode \
                 the response they arrived in"
            );
        }
    }

    // **One reading is allowed, and only where it is allowed.** A provider's
    // *refusal* is the one place the window a route actually enforces is
    // stated, and no catalogue can know it (`design-decisions.md`, *A context
    // window is a property of the route, not of the model*). So a body
    // carrying a client error is copied, bounded, read for one integer, and
    // dropped -- `gateway/context_limit.rs`. A successful response is still
    // never decoded at any size, which is what the blanket rule above was
    // protecting and what this keeps protecting: the exception is pinned to
    // the refusal buffer by name rather than trusted to stay there.
    //
    // Counted, not pattern-matched: a second decoding site anywhere in these
    // two files fails this even if it sits next to the first one.
    const REFUSAL_READING: &str = ".and_then(|bytes| std::str::from_utf8(bytes).ok())";
    for (name, code) in &relay {
        let sites = code.matches("from_utf8").count();
        let expected = usize::from(*name == "gateway/ingress.rs");
        assert_eq!(
            sites, expected,
            "{name} decodes bytes at {sites} site(s) and may decode at {expected}: the relay \
             may decode a client error's own bytes to learn the window the route enforces, \
             and nothing else"
        );
    }
    assert!(
        relay[0].1.contains(REFUSAL_READING)
            && relay[0].1.contains("context_limit::stated_limit")
            && relay[0].1.contains("counted\n            .refusal"),
        "the one allowed decoding must still be the refusal reading itself: the refusal \
         buffer, decoded, handed to `context_limit::stated_limit`"
    );
    // `::from_slice` rather than `from_slice`, and the difference is checked
    // rather than trusted: the needle has to fire on the realistic violation
    // and not on `Vec::extend_from_slice`, which is how a bounded observer
    // takes a copy of the chunk it was handed.
    assert!("let v: Value = serde_json::from_slice(bytes)?;".contains("::from_slice"));
    assert!(!"self.window.extend_from_slice(chunk);".contains("::from_slice"));
}

// --- lines 1316 and 1365: the rendering --------------------------------------

// --- line 1318 ---------------------------------------------------------------
