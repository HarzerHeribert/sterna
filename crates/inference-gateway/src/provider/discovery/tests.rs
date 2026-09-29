use super::*;
use crate::provider::fixture::FixtureProvider;

/// Short enough that the whole suite does not wait on them, long enough
/// that a loopback round trip is never mistaken for a stall.
fn quick() -> ProbeTimeouts {
    ProbeTimeouts {
        connect: Duration::from_millis(500),
        response: Duration::from_millis(400),
        total: Duration::from_millis(900),
    }
}

fn request_at(base_url: &str, target: ProbeTarget) -> ProbeRequest {
    ProbeRequest::new(
        "test-provider",
        WireProtocol::OpenAiChat,
        base_url,
        target,
        Vec::new(),
        Some(Secret::mint_for_test("sk-probe-credential")),
    )
}

// --- the timeouts themselves ----------------------------------------

/// A default of "no timeout" is a hang, and this is the assertion that
/// says so. Every field is checked against its own constant so that
/// losing one — the response timeout especially — fails here rather than
/// in a frozen terminal.
#[test]
fn the_default_timeouts_are_the_named_constants_and_none_is_unset() {
    let defaults = ProbeTimeouts::default();
    assert_eq!(defaults.connect, CONNECT_TIMEOUT);
    assert_eq!(defaults.response, RESPONSE_TIMEOUT);
    assert_eq!(defaults.total, TOTAL_TIMEOUT);
    for (what, value) in [
        ("connect", defaults.connect),
        ("response", defaults.response),
        ("total", defaults.total),
    ] {
        assert!(!value.is_zero(), "the {what} timeout must not be zero");
        assert!(
            value <= Duration::from_secs(30),
            "the {what} timeout must stay short enough that a user waits rather than \
             wonders whether the interface has frozen"
        );
    }
    assert!(
        defaults.total >= defaults.response,
        "the whole-call ceiling must not be shorter than the phase it contains"
    );
}

// --- what a probe actually requests ----------------------------------

#[test]
fn a_model_list_probe_requests_models_under_the_base_url() {
    let request = request_at("https://a.example/v1", ProbeTarget::ModelList);
    assert_eq!(request.url(), "https://a.example/v1/models");
}

#[test]
fn a_trailing_slash_on_the_base_url_does_not_double_the_separator() {
    let request = request_at("https://a.example/v1/", ProbeTarget::ModelList);
    assert_eq!(request.url(), "https://a.example/v1/models");
}

#[test]
fn a_base_url_probe_appends_no_path_at_all() {
    let request = request_at("https://a.example/v1", ProbeTarget::BaseUrl);
    assert_eq!(request.url(), "https://a.example/v1");
}

// --- Phase 32B line 1229: the headers a response carried --------------

/// The header block `https://anyrouter.dev/api/v1/models` really answered
/// with on 2026-08-27, in wire format.
///
/// Served by the fixture so that the capture is proven **through
/// `connectivity_with_headers`** rather than by handing
/// `RateLimitHeaders::read` a list a test built. Practice §35: the
/// telemetry tests all enter below this function, and a capture nothing
/// enters through is a capture the suite would not miss.
const ANYROUTER_HEADER_BLOCK: &str = "ratelimit-limit: 300\r\n\
     ratelimit-policy: 300;w=60\r\n\
     x-ratelimit-limit: 300\r\n\
     x-ratelimit-tier: ip\r\n\
     x-ratelimit-window: 60\r\n\
     access-control-expose-headers: X-RateLimit-Limit,RateLimit-Remaining\r\n\
     set-cookie: __cf_bm=oGkHQJmsGX6wCH7Quh5JYzAK6KXu1icwUg5MExQ2LqQ\r\n";

#[test]
fn a_response_carrying_rate_limit_headers_hands_them_back_to_the_caller() {
    let fixture =
        FixtureProvider::answering("HTTP/1.1 200 OK", ANYROUTER_HEADER_BLOCK, "{\"data\":[]}");
    let request = request_at(&fixture.base_url(), ProbeTarget::ModelList);
    let response = connectivity_with_headers(&request, quick());

    assert_eq!(response.outcome(), &ProbeOutcome::Reached { status: 200 });
    let limits = response.rate_limits();
    assert_eq!(limits.limit(), Some(300));
    assert_eq!(limits.window_seconds(), Some(60));
    // The host advertises `RateLimit-Remaining` and did not send it.
    assert_eq!(limits.remaining(), None);
}

/// The allowlist, at the boundary it exists to guard. This response
/// carries a `set-cookie` — OpenRouter's own `GET /api/v1/models` does,
/// measured — and a capture that kept "the response headers" would put a
/// session cookie into a diagnostic a user is invited to share.
#[test]
fn nothing_but_an_allowlisted_header_survives_the_capture() {
    let fixture =
        FixtureProvider::answering("HTTP/1.1 200 OK", ANYROUTER_HEADER_BLOCK, "{\"data\":[]}");
    let request = request_at(&fixture.base_url(), ProbeTarget::ModelList);
    let response = connectivity_with_headers(&request, quick());

    let names: Vec<&str> = response
        .headers()
        .iter()
        .map(|(name, _)| name.as_str())
        .collect();
    assert_eq!(
        names,
        vec![
            "ratelimit-limit",
            "ratelimit-policy",
            "x-ratelimit-limit",
            "x-ratelimit-window"
        ],
        "the capture kept a header nobody asked for"
    );
    let rendered = format!("{response:?}");
    for forbidden in ["set-cookie", "__cf_bm", "oGkHQJmsGX", "x-ratelimit-tier"] {
        assert!(
            !rendered.contains(forbidden),
            "`{forbidden}` survived the capture"
        );
    }
}

#[test]
fn a_response_with_no_rate_limit_header_hands_back_an_empty_capture() {
    let fixture = FixtureProvider::answering(
        "HTTP/1.1 200 OK",
        "content-type: application/json\r\ncf-cache-status: HIT\r\n",
        "{\"data\":[]}",
    );
    let request = request_at(&fixture.base_url(), ProbeTarget::ModelList);
    let response = connectivity_with_headers(&request, quick());
    assert!(response.headers().is_empty());
    assert!(response.rate_limits().is_empty());
}

/// `connectivity` must keep answering exactly what it always did, so no
/// existing caller changed behaviour when the capture was added.
#[test]
fn capturing_headers_did_not_change_what_a_plain_connectivity_probe_answers() {
    let fixture =
        FixtureProvider::answering("HTTP/1.1 200 OK", ANYROUTER_HEADER_BLOCK, "{\"data\":[]}");
    let request = request_at(&fixture.base_url(), ProbeTarget::ModelList);
    assert_eq!(
        connectivity(&request, quick()),
        ProbeOutcome::Reached { status: 200 }
    );
}

/// A refusal carries headers too, and `Retry-After` is the one a provider
/// sends with one. Capability map line 1229 says *rate-limit and usage
/// headers*, not *headers on a success*.
#[test]
fn a_refusal_that_carries_a_retry_after_still_yields_a_reading() {
    let fixture = FixtureProvider::answering(
        "HTTP/1.1 429 Too Many Requests",
        "retry-after: 30\r\nratelimit-remaining: 0\r\n",
        "",
    );
    let request = request_at(&fixture.base_url(), ProbeTarget::ModelList);
    let response = connectivity_with_headers(&request, quick());
    assert_eq!(
        response.outcome(),
        &ProbeOutcome::Unexpected { status: 429 }
    );
    assert_eq!(response.rate_limits().retry_after_seconds(), Some(30));
    assert_eq!(response.rate_limits().remaining(), Some(0));
}

/// A request that never got an answer has no headers to carry, and asking
/// for them must not be a way to turn a transport failure into a panic.
#[test]
fn an_unreachable_endpoint_yields_an_outcome_and_an_empty_capture() {
    let request = request_at("http://127.0.0.1:1/v1", ProbeTarget::ModelList);
    let response = connectivity_with_headers(&request, quick());
    assert!(!response.outcome().answered());
    assert!(response.headers().is_empty());
    assert!(response.rate_limits().is_empty());
}

// --- line 1: connectivity --------------------------------------------

#[test]
fn a_reachable_endpoint_is_reported_as_reached_with_its_status() {
    let fixture = FixtureProvider::answering("HTTP/1.1 200 OK", "", "{\"data\":[]}");
    let request = request_at(&fixture.base_url(), ProbeTarget::ModelList);
    assert_eq!(
        connectivity(&request, quick()),
        ProbeOutcome::Reached { status: 200 }
    );
    assert_eq!(fixture.requests().len(), 1, "exactly one request, no other");
}

/// Acceptance test 2. `401` is not "the test failed" — it is "the
/// endpoint is there and did not accept this credential", which is a
/// different problem with a different fix.
#[test]
fn an_endpoint_answering_401_is_reachable_but_rejected_not_unreachable() {
    let fixture = FixtureProvider::answering("HTTP/1.1 401 Unauthorized", "", "{}");
    let request = request_at(&fixture.base_url(), ProbeTarget::ModelList);
    let outcome = connectivity(&request, quick());
    assert_eq!(outcome, ProbeOutcome::Rejected { status: 401 });
    assert!(outcome.answered(), "a 401 means something answered");
    assert!(
        !matches!(outcome, ProbeOutcome::Unreachable { .. }),
        "a rejected credential must never be reported as an unreachable host"
    );
}

#[test]
fn an_endpoint_answering_403_is_also_reachable_but_rejected() {
    let fixture = FixtureProvider::answering("HTTP/1.1 403 Forbidden", "", "{}");
    let request = request_at(&fixture.base_url(), ProbeTarget::ModelList);
    assert_eq!(
        connectivity(&request, quick()),
        ProbeOutcome::Rejected { status: 403 }
    );
}

#[test]
fn an_endpoint_answering_404_is_reported_with_the_status_it_gave() {
    let fixture = FixtureProvider::answering("HTTP/1.1 404 Not Found", "", "nope");
    let request = request_at(&fixture.base_url(), ProbeTarget::ModelList);
    assert_eq!(
        connectivity(&request, quick()),
        ProbeOutcome::Unexpected { status: 404 }
    );
}

/// **Acceptance test 3, and the one that matters.**
///
/// Not a refused connection — that is the easy case and it proves very
/// little. This fixture accepts the connection, reads the request, and
/// then says nothing at all, which is exactly what a wedged provider
/// looks like and exactly what would hang a client with no read timeout.
/// The assertion is that the probe comes back *and* reports a timeout,
/// and that it does so within a bound derived from the timeout rather
/// than from hope.
#[test]
fn an_endpoint_that_accepts_and_never_answers_is_bounded_by_the_timeout() {
    let fixture = FixtureProvider::hanging();
    let base_url = fixture.base_url();

    // **The probe runs on a thread and the assertion waits with a
    // deadline**, rather than calling `connectivity` directly. That is
    // not ceremony: the mutation that proves this test — deleting the
    // read timeout — makes the call never return, and a direct call
    // would hang the whole test binary rather than failing it. A test
    // that wedges CI reports nothing; this one reports a failure in a
    // bounded time whatever the peer or the code does.
    let (done, answer) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let request = request_at(&base_url, ProbeTarget::ModelList);
        let started = Instant::now();
        let outcome = connectivity(&request, quick());
        let _ = done.send((outcome, started.elapsed()));
    });

    let (outcome, elapsed) = answer.recv_timeout(Duration::from_secs(5)).expect(
        "the probe never came back — an endpoint that accepts and then says \
                 nothing must be bounded by the read timeout, not waited on forever",
    );

    assert!(
        matches!(outcome, ProbeOutcome::TimedOut { .. }),
        "a stalled endpoint must be reported as a timeout, got {outcome:?}"
    );
    assert!(
        elapsed < Duration::from_secs(5),
        "the probe must be bounded by its own timeout, not by the peer; took {elapsed:?}"
    );
    assert_eq!(
        fixture.connections(),
        1,
        "the probe must have actually connected — a test that proved a refused \
         connection would prove nothing about a stall"
    );
}

/// Nothing listening is reported as a non-answer, and *how* it is reported
/// follows what the operating system actually did.
///
/// # Why this is not simply `assert Unreachable`
///
/// It was, and Windows CI failed it with
/// `nothing listening is unreachable, got TimedOut { waited_ms: 502 }`.
/// That is not a defect in the code under test — it is a difference in
/// what the platforms *do*. A Unix host answers a connection to a closed
/// port with an immediate RST, which surfaces as
/// [`std::io::ErrorKind::ConnectionRefused`] and classifies as
/// [`ProbeOutcome::Unreachable`]. On the Windows runner the attempt
/// instead ran out the connect timeout (502 ms against a 500 ms budget),
/// so there was no refusal to classify and `TimedOut` is the honest
/// answer.
///
/// So the assertion is split. **The part that is true everywhere** — a
/// host that nothing is listening on never counts as having answered — is
/// asserted unconditionally, and it is the property the product actually
/// promises. The classification of a *refusal* is asserted only where a
/// refusal is what the platform produces.
///
/// This is the same shape as the pty rule in the practice file: local
/// behaviour that a `cfg` flip cannot reproduce, because it is a runtime
/// property of the platform rather than a compile-time one.
#[test]
fn nothing_listening_never_counts_as_an_answer() {
    // Bind and drop, so the port is one nothing is listening on.
    let port = {
        let listener = std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .expect("loopback is bindable");
        listener
            .local_addr()
            .expect("a bound listener has an address")
            .port()
    };
    let request = request_at(&format!("http://127.0.0.1:{port}"), ProbeTarget::BaseUrl);
    let outcome = connectivity(&request, quick());

    // True on every platform, and the property that matters: a probe that
    // never reached anything must not report that it did.
    assert!(
        !outcome.answered(),
        "nothing was listening, so nothing can have answered, got {outcome:?}"
    );
    assert!(
        matches!(
            outcome,
            ProbeOutcome::Unreachable { .. } | ProbeOutcome::TimedOut { .. }
        ),
        "a port nothing is listening on is either refused or never answers, got {outcome:?}"
    );

    // Where the platform refuses, the refusal must be classified as a
    // refusal and not as a stall — the two have different fixes and the
    // user is told which they have.
    #[cfg(unix)]
    assert!(
        matches!(outcome, ProbeOutcome::Unreachable { .. }),
        "on Unix a closed port answers with RST, so this must classify as unreachable \
         rather than as a timeout, got {outcome:?}"
    );
}

/// The classification itself, with no operating system involved.
///
/// This is what the platform-dependent test above can no longer prove
/// everywhere, so it is proved here instead: a refusal is `Unreachable`,
/// and it says so in words this module chose.
#[test]
fn a_refused_connection_classifies_as_unreachable_rather_than_a_timeout() {
    let refused = ureq::Error::Io(std::io::Error::from(std::io::ErrorKind::ConnectionRefused));
    assert!(!is_timeout(&refused), "a refusal is not a timeout");
    assert_eq!(unreachable_reason(&refused), "the connection was refused");

    let outcome = transport_outcome(&refused, Instant::now());
    assert!(
        matches!(outcome, ProbeOutcome::Unreachable { .. }),
        "got {outcome:?}"
    );
    assert!(!outcome.answered());
}

// --- line 2: the catalogue -------------------------------------------

#[test]
fn a_catalogue_is_read_from_the_data_array_in_the_order_given() {
    let fixture = FixtureProvider::answering(
        "HTTP/1.1 200 OK",
        "content-type: application/json\r\n",
        r#"{"object":"list","data":[{"id":"b/one"},{"id":"a/two"}]}"#,
    );
    let request = request_at(&fixture.base_url(), ProbeTarget::ModelList);
    match model_catalogue(&request, quick()) {
        ModelFetch::Catalogue(models) => assert_eq!(
            models,
            vec![ModelEntry::new("b/one"), ModelEntry::new("a/two")],
            "the provider's own order is preserved"
        ),
        other => panic!("expected a catalogue, got {other:?}"),
    }
}

/// UnoRouter's real envelope, quoted from the body read on 2026-08-26.
#[test]
fn an_envelope_around_the_data_array_does_not_stop_the_catalogue_being_read() {
    let fixture = FixtureProvider::answering(
        "HTTP/1.1 200 OK",
        "",
        r#"{"success":true,"message":"","data":[{"id":"glm-deep-research-thinking:free","object":"model","owned_by":"custom"}]}"#,
    );
    let request = request_at(&fixture.base_url(), ProbeTarget::ModelList);
    match model_catalogue(&request, quick()) {
        ModelFetch::Catalogue(models) => {
            assert_eq!(
                models,
                vec![ModelEntry::new("glm-deep-research-thinking:free")]
            );
        }
        other => panic!("expected a catalogue, got {other:?}"),
    }
}

// --- line 1230: a body read whole, and handed to a caller unread -------

/// `GET https://openrouter.ai/api/v1/key`'s real shape, authenticated,
/// 2026-08-27 — the same body `provider::telemetry`'s own fixture uses,
/// proving the two modules agree on what the wire actually said.
#[test]
fn read_response_body_hands_back_the_whole_body_unparsed() {
    let fixture = FixtureProvider::answering(
        "HTTP/1.1 200 OK",
        "content-type: application/json\r\n",
        r#"{"data":{"limit":null,"limit_remaining":null,"limit_reset":null}}"#,
    );
    let request = request_at(&fixture.base_url(), ProbeTarget::BaseUrl);
    match read_response_body(&request, quick()) {
        BodyFetch::Answered { status, body } => {
            assert_eq!(status, 200);
            assert!(body.contains("\"limit\":null"), "{body}");
        }
        other => panic!("expected an answered body, got {other:?}"),
    }
}

#[test]
fn a_refused_usage_endpoint_is_reported_as_a_probe_outcome_not_a_body() {
    let fixture = FixtureProvider::answering(
        "HTTP/1.1 401 Unauthorized",
        "content-type: application/json\r\n",
        r#"{"error":{"message":"No auth credentials found","code":401}}"#,
    );
    let request = request_at(&fixture.base_url(), ProbeTarget::BaseUrl);
    match read_response_body(&request, quick()) {
        BodyFetch::Probe(ProbeOutcome::Rejected { status: 401 }) => {}
        other => panic!("expected a 401 rejection, got {other:?}"),
    }
}

#[test]
fn a_bare_top_level_array_is_read_as_a_catalogue_too() {
    let fixture = FixtureProvider::answering("HTTP/1.1 200 OK", "", r#"[{"id":"local/model"}]"#);
    let request = request_at(&fixture.base_url(), ProbeTarget::ModelList);
    match model_catalogue(&request, quick()) {
        ModelFetch::Catalogue(models) => {
            assert_eq!(models, vec![ModelEntry::new("local/model")]);
        }
        other => panic!("expected a catalogue, got {other:?}"),
    }
}

#[test]
fn an_empty_catalogue_is_a_catalogue_and_not_a_failure() {
    let fixture = FixtureProvider::answering("HTTP/1.1 200 OK", "", r#"{"data":[]}"#);
    let request = request_at(&fixture.base_url(), ProbeTarget::ModelList);
    assert_eq!(
        model_catalogue(&request, quick()),
        ModelFetch::Catalogue(Vec::new()),
        "a provider that genuinely offers no models said so; that is an answer"
    );
}

#[test]
fn a_200_that_is_not_a_catalogue_says_so_rather_than_reporting_an_empty_list() {
    let fixture = FixtureProvider::answering("HTTP/1.1 200 OK", "", "<html>hello</html>");
    let request = request_at(&fixture.base_url(), ProbeTarget::ModelList);
    match model_catalogue(&request, quick()) {
        ModelFetch::NotACatalogue { status, reason } => {
            assert_eq!(status, 200);
            assert!(!reason.is_empty());
        }
        other => panic!("expected NotACatalogue, got {other:?}"),
    }
}

#[test]
fn a_catalogue_fetch_that_is_rejected_reports_the_rejection_not_an_empty_list() {
    let fixture = FixtureProvider::answering("HTTP/1.1 401 Unauthorized", "", "{}");
    let request = request_at(&fixture.base_url(), ProbeTarget::ModelList);
    assert_eq!(
        model_catalogue(&request, quick()),
        ModelFetch::Probe(ProbeOutcome::Rejected { status: 401 })
    );
}

#[test]
fn a_stalled_catalogue_fetch_is_bounded_by_the_timeout_too() {
    let fixture = FixtureProvider::hanging();
    let request = request_at(&fixture.base_url(), ProbeTarget::ModelList);
    let started = Instant::now();
    let fetched = model_catalogue(&request, quick());
    assert!(
        matches!(fetched, ModelFetch::Probe(ProbeOutcome::TimedOut { .. })),
        "got {fetched:?}"
    );
    assert!(started.elapsed() < Duration::from_secs(5));
}

/// The two-orders-of-magnitude range the packet named: nine models
/// against four hundred and seventeen. Both ends are read from the same
/// parser with no truncation and no cap.
#[test]
fn a_catalogue_of_nine_and_a_catalogue_of_four_hundred_and_seventeen_both_read_whole() {
    for count in [9usize, 417] {
        let entries: Vec<String> = (0..count)
            .map(|index| format!("{{\"id\":\"vendor/model-{index}\"}}"))
            .collect();
        let body = format!("{{\"data\":[{}]}}", entries.join(","));
        let fixture = FixtureProvider::start(move |_request, out| {
            use std::io::Write as _;
            let _ = write!(
                out,
                "HTTP/1.1 200 OK\r\ncontent-length: {}\r\n\r\n{body}",
                body.len()
            );
            let _ = out.flush();
            let _ = out.shutdown(std::net::Shutdown::Write);
        });
        let request = request_at(&fixture.base_url(), ProbeTarget::ModelList);
        match model_catalogue(&request, quick()) {
            ModelFetch::Catalogue(models) => {
                assert_eq!(models.len(), count, "every entry must survive the read");
                assert_eq!(models[0], ModelEntry::new("vendor/model-0"));
                assert_eq!(
                    models[count - 1],
                    ModelEntry::new(format!("vendor/model-{}", count - 1))
                );
            }
            other => panic!("expected a catalogue of {count}, got {other:?}"),
        }
    }
}

// --- the credential ---------------------------------------------------

/// Acceptance test 7, at this module's own boundary: the value goes into
/// a header and nowhere else. Asserted with `!contains` rather than
/// `assert_eq!`, because a failing `assert_eq!` on secret material
/// prints both sides.
#[test]
fn the_credential_reaches_the_authorization_header_and_no_other_surface() {
    const VALUE: &str = "sk-planted-credential-value-9d";
    let fixture = FixtureProvider::answering("HTTP/1.1 200 OK", "", r#"{"data":[]}"#);
    let request = ProbeRequest::new(
        "leak-check",
        WireProtocol::OpenAiChat,
        fixture.base_url(),
        ProbeTarget::ModelList,
        vec![("x-config-header".to_owned(), "plain".to_owned())],
        Some(Secret::mint_for_test(VALUE)),
    );

    // It must not be in the type's own rendering...
    let debug = format!("{request:?}");
    assert!(
        !debug.contains(VALUE),
        "a credential reached ProbeRequest's Debug"
    );
    assert!(debug.contains(REDACTED), "{debug}");

    // ... nor in the URL ...
    assert!(!request.url().contains(VALUE));

    let outcome = connectivity(&request, quick());
    assert_eq!(outcome, ProbeOutcome::Reached { status: 200 });

    // ... and on the wire it is in exactly one header and no other.
    let sent = fixture.requests();
    assert_eq!(sent.len(), 1);
    let sent = &sent[0];
    assert_eq!(
        sent.header("authorization"),
        Some(format!("Bearer {VALUE}").as_str()),
        "the credential must be attached as a bearer token"
    );
    assert!(!sent.target.contains(VALUE), "a credential reached the URL");
    for (name, value) in &sent.headers {
        if name != "authorization" {
            assert!(
                !value.contains(VALUE),
                "a credential reached the `{name}` header"
            );
        }
    }
    assert!(
        !String::from_utf8_lossy(&sent.body).contains(VALUE),
        "a credential reached the request body"
    );

    // ... and none of the reported outcome carries it either.
    assert!(!format!("{outcome:?}").contains(VALUE));
}

#[test]
fn an_anthropic_provider_sends_its_credential_as_x_api_key_not_a_bearer_token() {
    const VALUE: &str = "sk-ant-planted-9d";
    let fixture = FixtureProvider::answering("HTTP/1.1 200 OK", "", "{}");
    let request = ProbeRequest::new(
        "anthropic-ish",
        WireProtocol::AnthropicMessages,
        fixture.base_url(),
        ProbeTarget::BaseUrl,
        Vec::new(),
        Some(Secret::mint_for_test(VALUE)),
    );
    connectivity(&request, quick());
    let sent = fixture.requests();
    assert_eq!(sent[0].header("x-api-key"), Some(VALUE));
    assert!(
        sent[0].header("authorization").is_none(),
        "an Anthropic-protocol provider must not be sent a bearer token"
    );
}

#[test]
fn a_provider_with_no_credential_sends_no_credential_header() {
    let fixture = FixtureProvider::answering("HTTP/1.1 200 OK", "", "{}");
    let request = ProbeRequest::new(
        "no-credential",
        WireProtocol::OpenAiChat,
        fixture.base_url(),
        ProbeTarget::BaseUrl,
        Vec::new(),
        None,
    );
    connectivity(&request, quick());
    let sent = fixture.requests();
    assert!(sent[0].header("authorization").is_none());
    assert!(format!("{request:?}").contains("(none)"));
}

/// A provider's configured headers are configuration, so they are sent —
/// but they are sent as written, and adding one must not disturb the
/// credential header beside it.
#[test]
fn a_providers_configured_headers_are_sent_as_written() {
    let fixture = FixtureProvider::answering("HTTP/1.1 200 OK", "", "{}");
    let request = ProbeRequest::new(
        "with-headers",
        WireProtocol::OpenAiChat,
        fixture.base_url(),
        ProbeTarget::BaseUrl,
        vec![
            (
                "http-referer".to_owned(),
                "https://glasshouse.dev".to_owned(),
            ),
            ("x-title".to_owned(), "Glasshouse".to_owned()),
        ],
        None,
    );
    connectivity(&request, quick());
    let sent = fixture.requests();
    assert_eq!(
        sent[0].header("http-referer"),
        Some("https://glasshouse.dev")
    );
    assert_eq!(sent[0].header("x-title"), Some("Glasshouse"));
}

// --- redirects are not followed ---------------------------------------

/// `kilocode.ai` answers `308` pointing at `kilo.ai` today, which is why
/// the `kilo` template declares the new host. A probe that quietly
/// followed a redirect would hide exactly that fact, and would also be
/// deciding on its own to hand the credential to whatever host the
/// redirect named.
#[test]
fn a_redirect_is_reported_rather_than_followed() {
    let fixture = FixtureProvider::answering(
        "HTTP/1.1 308 Permanent Redirect",
        "location: https://elsewhere.example/api/models\r\n",
        "",
    );
    let request = request_at(&fixture.base_url(), ProbeTarget::ModelList);
    assert_eq!(
        connectivity(&request, quick()),
        ProbeOutcome::Unexpected { status: 308 },
        "a redirect is a fact about the endpoint, not something to chase"
    );
}

// --- the parser, without a socket -------------------------------------

#[test]
fn a_body_that_is_not_json_is_refused_by_name() {
    assert_eq!(
        parse_catalogue("not json at all"),
        Err("the response was not JSON".to_owned())
    );
}

#[test]
fn a_json_object_with_no_data_array_is_refused_by_name() {
    assert!(parse_catalogue(r#"{"object":"list"}"#).is_err());
}

#[test]
fn entries_without_an_id_do_not_silently_become_an_empty_catalogue() {
    assert!(parse_catalogue(r#"{"data":[{"name":"no id here"}]}"#).is_err());
}

// --- every known way to read an api-key provider's list ---------------

fn provider_with(routes: &[(WireProtocol, String)]) -> crate::provider::Provider {
    use crate::routing::wire::Declared;
    crate::provider::Provider {
        name: "vendor".into(),
        protocols: routes
            .iter()
            .map(|(protocol, base_url)| crate::provider::ProtocolSupport {
                protocol: *protocol,
                base_url: base_url.clone(),
                streaming: Declared::Unverified,
                tool_calls: Declared::Unverified,
                reasoning: Declared::Unverified,
            })
            .collect(),
        model_list_endpoint: Declared::Unverified,
        usage_telemetry: Declared::Unverified,
        credential_env: Vec::new(),
        headers: Vec::new(),
    }
}

fn key() -> Option<Secret> {
    Some(Secret::mint_for_test("listing-probe-key"))
}

/// An OpenAI-style route that has no list does not end the search: the
/// Anthropic-style route is asked at `/v1/models`, with its own key header
/// and version, and its list is the answer.
#[test]
fn a_route_without_a_list_falls_through_to_the_next_known_way() {
    let openai = FixtureProvider::answering("HTTP/1.1 404 Not Found", "", "{}");
    let anthropic = FixtureProvider::answering(
        "HTTP/1.1 200 OK",
        "",
        r#"{"data":[{"id":"vendor-flash"},{"id":"vendor-pro"}]}"#,
    );
    let provider = provider_with(&[
        (WireProtocol::OpenAiChat, openai.base_url()),
        (WireProtocol::AnthropicMessages, anthropic.base_url()),
    ]);
    let list = read_model_list(&provider, &key, quick()).expect("the second route lists models");
    assert_eq!(list.url, format!("{}/v1/models", anthropic.base_url()));
    assert_eq!(
        list.models,
        vec![
            ModelEntry::new("vendor-flash"),
            ModelEntry::new("vendor-pro")
        ]
    );
    assert_eq!(openai.requests()[0].target, "/models");
    let asked = &anthropic.requests()[0];
    assert_eq!(asked.target, "/v1/models");
    assert_eq!(asked.header("x-api-key"), Some("listing-probe-key"));
    assert_eq!(asked.header("anthropic-version"), Some("2023-06-01"));
}

/// When no known way answers with a list, every URL tried is named with
/// what came back, so the person knows it was tried and why it failed.
#[test]
fn every_way_failing_names_each_url_and_what_came_back() {
    let openai = FixtureProvider::answering("HTTP/1.1 404 Not Found", "", "{}");
    let anthropic = FixtureProvider::answering("HTTP/1.1 401 Unauthorized", "", "{}");
    let provider = provider_with(&[
        (WireProtocol::OpenAiChat, openai.base_url()),
        (WireProtocol::AnthropicMessages, anthropic.base_url()),
    ]);
    let tried = read_model_list(&provider, &key, quick()).expect_err("nothing lists models");
    assert_eq!(
        tried,
        vec![
            format!("GET {}/models: answered 404", openai.base_url()),
            format!(
                "GET {}/v1/models: refused the key (401)",
                anthropic.base_url()
            ),
        ]
    );
}

/// Two routes under one base URL share one listing URL, and it is asked once.
#[test]
fn a_listing_url_two_routes_share_is_asked_once() {
    let fixture = FixtureProvider::answering("HTTP/1.1 404 Not Found", "", "{}");
    let provider = provider_with(&[
        (WireProtocol::OpenAiChat, fixture.base_url()),
        (WireProtocol::OpenAiResponses, fixture.base_url()),
    ]);
    let tried = read_model_list(&provider, &key, quick()).expect_err("nothing lists models");
    assert_eq!(tried.len(), 1, "{tried:?}");
    assert_eq!(fixture.requests().len(), 1);
}
