//! Capability map line 1319 — *"Treat provider-declared Retry-After or
//! equivalent cooldown information as authoritative for temporary scheduling
//! blocks."*
//!
//! `gateway::ingress::forward` already reads a response's `Retry-After` into
//! `RateLimitHeaders` for any status, `gateway::mod`'s accept loop already
//! passes `session::stated_retry_after(&quota)` into
//! `routing.observe_exchange(..)`, `gateway::session::classify`'s `429` arm
//! already turns that into `WorkloadOutcome::RateLimited { retry_after }`, and
//! `routing::free::ResourceHealth::fail` already applies a **declared** wait
//! immediately and unclamped, where an **invented** one needs
//! `FAILURES_BEFORE_COOLDOWN` failures first. All of that is production code
//! before this file exists; what is missing is proof that a real exchange
//! actually walks it end to end.
//!
//! That is why this drives a real `TcpStream` through a real gateway started
//! by the real production entry point, `gateway::start_if_required_with_telemetry`,
//! rather than calling `SessionRouting::observe_exchange` or
//! `ResourceHealth::fail` directly: practice §35 is that a caller every test
//! bypasses is not a caller, and the capability being closed here is
//! specifically the wire from a socket to a cooldown, not either mechanism's
//! own unit tests in isolation.
//!
//! One `429` is enough to tell a declared wait from an invented one apart,
//! because `FAILURES_BEFORE_COOLDOWN` is 2: an invented cooldown cannot exist
//! after a single failure at all, so any cooldown this file observes after
//! exactly one exchange must have come from the provider's own header.

mod host;

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use inference_gateway::gateway::{Route, Upstream, UpstreamBackend};
use inference_gateway::provider::telemetry::{GatewayHealthCache, GatewayHealthReading};
use inference_gateway::routing::{AssignedModel, Cost, CredentialId};
use inference_gateway::secret::{EnvironmentSecretStore, Secret, SecretRef, SecretStore};

/// The name used for both the upstream backend and the credential's provider,
/// so that whichever one `gateway::session` stamps onto the exchange as its
/// provider, `GatewayHealthCache::load` is asked for the right file.
const PROVIDER: &str = "fixture-retry-after";

/// The model this test binds an assignment to, on both ends: the bind call
/// and the request body. Its exact value is arbitrary — a stub server never
/// reads it — but it has to be the same string in both places.
const MODEL: &str = "stub-model";

/// A stand-in provider credential, resolved through the real environment
/// store rather than a crate-private test constructor, exactly the way
/// `gateway_degrade.rs`'s own `test_credential` does and for the same reason:
/// `secret::Secret` has no public way to mint one outside `crate::secret`.
/// `var` is unique per call site so the two tests in this file, which may run
/// concurrently, never race on the same environment variable.
fn test_credential(var: &str) -> Secret {
    // SAFETY: `var` is unique to the one caller that set it, and it is
    // removed again immediately below, before the resolved value is even
    // inspected, so no other test can observe it set.
    unsafe {
        std::env::set_var(var, "sk-planted-not-a-real-key-retryafter");
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

fn credential_id(var: &str) -> CredentialId {
    CredentialId::new(
        PROVIDER,
        SecretRef::Environment {
            var: var.to_owned(),
        },
    )
}

/// A local HTTP server that answers every connection it accepts with a
/// `429`, optionally carrying `Retry-After`, up to a deadline — and counts
/// how many connections it actually accepted.
///
/// Bounded rather than a plain blocking `accept`: the listener is
/// non-blocking and polled against a deadline, so a gateway that never
/// dialled it — a defect on its own — fails this test with a normal
/// assertion instead of hanging the suite. Serving more than the one
/// connection the original version of this helper stopped at is what lets
/// the counter distinguish "the gateway refused the second request in
/// place" from "the gateway forwarded it and got a second 429" — a test that
/// only checked the response code cannot tell those apart.
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

fn stub_429_server(retry_after_seconds: Option<u64>) -> (SocketAddr, Arc<AtomicUsize>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("loopback is bindable");
    let address = listener
        .local_addr()
        .expect("a bound listener has a local address");
    listener
        .set_nonblocking(true)
        .expect("a listener can be put in polling mode");

    let hits = Arc::new(AtomicUsize::new(0));
    let counted = Arc::clone(&hits);

    std::thread::Builder::new()
        .name("gateway-retry-after-stub".to_owned())
        .spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(10);
            loop {
                let mut stream = match listener.accept() {
                    Ok((stream, _peer)) => stream,
                    Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                        if Instant::now() >= deadline {
                            return;
                        }
                        std::thread::sleep(Duration::from_millis(10));
                        continue;
                    }
                    Err(_) => return,
                };
                counted.fetch_add(1, Ordering::SeqCst);
                // The accepted socket may have inherited the listener's
                // non-blocking flag (macOS and Windows do this; Linux does
                // not — `gateway::Gateway`'s own doc names the split) so it
                // is cleared explicitly rather than assumed, exactly as
                // `gateway::ingress` does for the identical reason.
                let _ = stream.set_nonblocking(false);
                let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));

                read_whole_request(&mut stream);

                let response = match retry_after_seconds {
                    Some(seconds) => format!(
                        "HTTP/1.1 429 Too Many Requests\r\nRetry-After: {seconds}\r\nContent-Length: 0\r\n\r\n"
                    ),
                    None => "HTTP/1.1 429 Too Many Requests\r\nContent-Length: 0\r\n\r\n".to_owned(),
                };
                let _ = stream.write_all(response.as_bytes());
                let _ = stream.flush();
            }
        })
        .expect("can spawn the stub server thread");

    (address, hits)
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

/// Send `raw` and return everything the gateway wrote back, to the close —
/// `gateway_degrade.rs`'s own `send_and_read`.
fn send_and_read(address: SocketAddr, raw: &[u8]) -> String {
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
    String::from_utf8_lossy(&out).into_owned()
}

fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("the system clock is after the epoch")
        .as_secs() as i64
}

/// Poll `cache` for a reading, exactly the way the accept loop's own write
/// happens after `send_and_read` has already returned: the connection
/// thread's routing bookkeeping runs after `ingress::serve` has closed the
/// response socket, so the client side finishing is not proof the cache has
/// been written yet.
fn wait_for_readings(cache: &GatewayHealthCache, provider: &str) -> Vec<GatewayHealthReading> {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let readings = cache.load(provider);
        if !readings.is_empty() || Instant::now() >= deadline {
            return readings;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// A gateway pointed at a stub upstream that always answers `429`, writing
/// what it observes to `health_cache`, with an assignment already bound so
/// `SessionRouting::observe_exchange` has somewhere to record it.
fn gateway_to_stub(
    credential_var: &str,
    upstream_address: SocketAddr,
    health_cache: GatewayHealthCache,
) -> inference_gateway::gateway::Gateway {
    let backend = UpstreamBackend::new(
        PROVIDER.to_owned(),
        vec![Route::new(
            "anthropic-messages".to_owned(),
            &["/messages"],
            &format!("http://{upstream_address}"),
        )],
        test_credential(credential_var),
        credential_id(credential_var),
        Cost::Metered,
    )
    .expect("a loopback http URL is absolute and this credential is header-safe");
    let upstream = Upstream::with_failover(vec![backend]).expect("one backend is not none");
    let gateway = inference_gateway::gateway::start_if_required_with_telemetry(
        &[inference_gateway::gateway::BackendDemand::LocalGateway],
        || Ok(upstream),
        None,
        Some(health_cache),
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

/// Deleting this test removes the only proof that a provider's own stated
/// wait reaches the resource's cooldown at all: without it, nothing would
/// fail if `session::stated_retry_after` stopped being read, or if
/// `ResourceHealth::fail` started treating a declared wait the same as an
/// invented one.
#[test]
fn a_provider_stated_retry_after_blocks_the_resource_for_the_wait_it_stated() {
    const CREDENTIAL_VAR: &str = "GLASSHOUSE_GATEWAY_RETRY_AFTER_TEST_KEY_STATED";
    const STATED_WAIT_SECONDS: i64 = 3600;

    let health_dir = tempfile::tempdir().expect("a temp directory can be created");
    let health_cache = GatewayHealthCache::at(health_dir.path());

    // Premise first (§17): nothing has been observed for this provider
    // before the exchange runs at all.
    assert!(
        health_cache.load(PROVIDER).is_empty(),
        "no reading should exist for this provider before any exchange has run"
    );

    let (upstream_address, _hits) = stub_429_server(Some(STATED_WAIT_SECONDS as u64));
    let gateway = gateway_to_stub(CREDENTIAL_VAR, upstream_address, health_cache.clone());

    let response = send_and_read(
        gateway.address(),
        &messages_request(gateway.token().expose()),
    );
    assert!(
        response.starts_with("HTTP/1.1 429"),
        "the gateway must relay the provider's own 429 to the harness: {response}"
    );

    let readings = wait_for_readings(&health_cache, PROVIDER);
    assert_eq!(
        readings.len(),
        1,
        "exactly one resource must have been observed: {readings:?}"
    );
    let reading = &readings[0];
    assert_eq!(
        reading.consecutive_failures, 1,
        "one 429 is one failure, whether or not the provider stated a wait"
    );

    let now = now_unix();
    let until = reading
        .cooling_down_until_unix
        .expect("a declared retry-after must cool the resource down on the very first failure");
    let wait = until - now;
    assert!(
        wait > 900,
        "the wait ({wait}s) must be far beyond anything an invented cooldown could produce \
         (MAX_COOLDOWN is 900s and this is the first failure, so an invented cooldown could not \
         exist at all yet) — a wait this long can only have come from the provider's own \
         {STATED_WAIT_SECONDS}s header"
    );
    assert!(
        wait <= STATED_WAIT_SECONDS + 60,
        "the wait ({wait}s) exceeds the declared {STATED_WAIT_SECONDS}s by more than clock \
         tolerance"
    );
    assert!(
        !reading.is_available(now),
        "a resource cooling down for another {wait}s must not read as available now"
    );
}

/// Deleting this test removes the other half of the same proof: without it,
/// nothing would fail if an ordinary `429` started cooling a resource down on
/// its own, which would make every rate limit look like a declared one and
/// would make the first test's own assertion vacuous.
#[test]
fn one_rate_limit_with_no_stated_wait_blocks_nothing() {
    const CREDENTIAL_VAR: &str = "GLASSHOUSE_GATEWAY_RETRY_AFTER_TEST_KEY_UNSTATED";

    let health_dir = tempfile::tempdir().expect("a temp directory can be created");
    let health_cache = GatewayHealthCache::at(health_dir.path());

    // Premise first (§17): nothing has been observed for this provider
    // before the exchange runs at all.
    assert!(
        health_cache.load(PROVIDER).is_empty(),
        "no reading should exist for this provider before any exchange has run"
    );

    let (upstream_address, _hits) = stub_429_server(None);
    let gateway = gateway_to_stub(CREDENTIAL_VAR, upstream_address, health_cache.clone());

    let response = send_and_read(
        gateway.address(),
        &messages_request(gateway.token().expose()),
    );
    assert!(
        response.starts_with("HTTP/1.1 429"),
        "the gateway must relay the provider's own 429 to the harness: {response}"
    );

    let readings = wait_for_readings(&health_cache, PROVIDER);
    assert_eq!(
        readings.len(),
        1,
        "the exchange must be observed even though nothing should cool down: {readings:?}"
    );
    let reading = &readings[0];
    assert_eq!(
        reading.consecutive_failures, 1,
        "the failure was still observed — this must not look like an empty cache"
    );
    assert_eq!(
        reading.cooling_down_until_unix, None,
        "one rate limit with no stated wait must not invent a cooldown on the first failure"
    );
    assert!(
        reading.is_available(now_unix()),
        "a resource with no cooldown must read as available"
    );
}

/// Capability map line 1368 — *"Avoid retrying a paced route in place when
/// the current cadence makes the retry predictably unavailable."*
///
/// Deleting this test removes the only proof that the gateway actually
/// consults the cooldown its own first exchange recorded before dialling the
/// next one: without it, nothing would fail if the accept loop went back to
/// forwarding every request regardless of what `FreePool::is_available`
/// says — which is exactly what this file's mutation on that guard
/// exercises.
#[test]
fn a_second_request_while_still_paced_is_refused_locally_without_dialing_upstream() {
    const CREDENTIAL_VAR: &str = "GLASSHOUSE_GATEWAY_RETRY_AFTER_TEST_KEY_PACED";
    const STATED_WAIT_SECONDS: i64 = 3600;

    let health_dir = tempfile::tempdir().expect("a temp directory can be created");
    let health_cache = GatewayHealthCache::at(health_dir.path());

    // Premise first (§17): nothing has been observed for this provider
    // before the exchange runs at all.
    assert!(
        health_cache.load(PROVIDER).is_empty(),
        "no reading should exist for this provider before any exchange has run"
    );

    let (upstream_address, hits) = stub_429_server(Some(STATED_WAIT_SECONDS as u64));
    let gateway = gateway_to_stub(CREDENTIAL_VAR, upstream_address, health_cache.clone());

    // First request: the provider's own stated wait cools the one
    // configured credential down — the same premise the first test in this
    // file already proves end to end.
    let first_response = send_and_read(
        gateway.address(),
        &messages_request(gateway.token().expose()),
    );
    assert!(
        first_response.starts_with("HTTP/1.1 429"),
        "the gateway must relay the provider's own 429 to the harness: {first_response}"
    );

    let readings = wait_for_readings(&health_cache, PROVIDER);
    assert_eq!(
        readings.len(),
        1,
        "exactly one resource must have been observed: {readings:?}"
    );
    assert!(
        readings[0].cooling_down_until_unix.is_some(),
        "the first 429's stated wait must already be cooling the resource down: {readings:?}"
    );
    assert_eq!(
        hits.load(Ordering::SeqCst),
        1,
        "the stub must have been dialled exactly once, by the first request"
    );

    // Second request, sent while the wait is still in force. There is one
    // credential and no sibling to rotate to, so the gateway must refuse it
    // locally rather than spend an upstream request on a route it already
    // knows is paced.
    let second_response = send_and_read(
        gateway.address(),
        &messages_request(gateway.token().expose()),
    );
    assert!(
        second_response.starts_with("HTTP/1.1 429"),
        "the gateway must still answer 429 to the harness: {second_response}"
    );
    // The load-bearing assertion: a response code of 429 alone proves
    // nothing here, because forwarding to the still-paced route would also
    // produce a 429 from the stub. Only the stub's own request counter
    // tells "refused in place" and "forwarded and failed again" apart.
    assert_eq!(
        hits.load(Ordering::SeqCst),
        1,
        "the stub's request counter must not have increased: a paced route must be refused in \
         place, not forwarded and failed again"
    );
}
