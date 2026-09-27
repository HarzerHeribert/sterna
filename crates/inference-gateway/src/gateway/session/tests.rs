use super::*;
use crate::gateway::upstream::{Route, UpstreamBackend};
use crate::gateway::{Observation, ObservationSink};
use crate::routing::evidence::NewObservation;
use crate::routing::free::CooldownCause;
use crate::routing::interactive::RoutingBenefit;
use crate::routing::pairing::RouteAffinity;
use crate::routing::{Cost, CredentialId};
use crate::secret::{Secret, SecretRef};

/// The one channel a finished exchange leaves this process by, and a place
/// to keep what came out of it.
///
/// This gateway keeps no ledger and opens no database, so a test that would
/// once have read a row back out of SQLite reads the [`Observation`] the
/// producer actually emitted instead — the same value, one step earlier, and
/// on this side of the boundary rather than on a host's.
fn capturing_sink() -> (ObservationSink, Arc<Mutex<Vec<Observation>>>) {
    let seen: Arc<Mutex<Vec<Observation>>> = Arc::new(Mutex::new(Vec::new()));
    let sink: ObservationSink = {
        let seen = Arc::clone(&seen);
        Arc::new(move |observation| seen.lock().unwrap().push(observation))
    };
    (sink, seen)
}

/// Every [`Observation::Routed`] captured so far, in the order it was
/// reported, each with the second the producer said it completed at.
fn routed(seen: &Arc<Mutex<Vec<Observation>>>) -> Vec<(NewObservation, i64)> {
    seen.lock()
        .expect("no test panics while holding this")
        .iter()
        .filter_map(|reported| match reported {
            Observation::Routed {
                observation,
                observed_at_unix,
            } => Some((observation.as_ref().clone(), *observed_at_unix)),
            Observation::Degraded { .. } => None,
        })
        .collect()
}

fn upstream_backend(name: &str) -> UpstreamBackend {
    UpstreamBackend::new(
        name.to_owned(),
        vec![Route::new(
            "anthropic-messages".to_owned(),
            &["/messages"],
            "http://127.0.0.1:1",
        )],
        Secret::mint_for_test("test-secret"),
        CredentialId::new(
            name,
            SecretRef::Environment {
                var: format!("{}_API_KEY", name.to_uppercase()),
            },
        ),
        Cost::Metered,
    )
    .expect("a loopback http URL is absolute and this credential is header-safe")
}

fn unreachable_exchange(provider: &str) -> Exchange {
    Exchange {
        outcome: Outcome::Unreachable {
            detail: "connection refused",
        },
        status: 502,
        provider: provider.to_owned(),
        protocol: Some("anthropic-messages".to_owned()),
        purpose: None,
        requested_model: None,
        host: String::new(),
        // No response ever arrived — this outcome exists precisely
        // because the provider could not be reached at all.
        first_byte_at: None,
        first_token_at: None,
        first_tool_call_at: None,
        // The request never left, so there is no monotonic zero to
        // measure any of migration 25's four offsets from.
        first_byte_ms: None,
        first_token_ms: None,
        first_tool_call_ms: None,
        completed_ms: None,
        context_limit_tokens: None,
        framing: None,
        tokens: None,
        effort: None,
        turn_shape: None,
        tool_rounds: None,
        repairs: None,
    }
}

/// A second credential for `provider`, so a test can put two backends on
/// one provider — Phase 9I line 537's rotation candidate.
fn upstream_backend_with_credential(provider: &str, var: &str) -> UpstreamBackend {
    UpstreamBackend::new(
        provider.to_owned(),
        vec![Route::new(
            "anthropic-messages".to_owned(),
            &["/messages"],
            "http://127.0.0.1:1",
        )],
        Secret::mint_for_test("test-secret"),
        CredentialId::new(
            provider,
            SecretRef::Environment {
                var: var.to_owned(),
            },
        ),
        Cost::Metered,
    )
    .expect("a loopback http URL is absolute and this credential is header-safe")
}

fn rate_limited_exchange(provider: &str) -> Exchange {
    forwarded_exchange(provider, 429, Some(0), Some(0), StreamEnd::Complete)
}

/// A forwarded exchange with the framing `ingress::forward` would have
/// recorded for it: what was declared, what arrived, and how it ended.
fn forwarded_exchange(
    provider: &str,
    status: u16,
    declared: Option<u64>,
    relayed: Option<u64>,
    ended: StreamEnd,
) -> Exchange {
    Exchange {
        outcome: Outcome::Forwarded {
            upstream_status: status,
            bytes: relayed.unwrap_or(0),
        },
        status,
        provider: provider.to_owned(),
        protocol: Some("anthropic-messages".to_owned()),
        purpose: None,
        requested_model: None,
        host: String::new(),
        first_byte_at: Some(1_700_000_000),
        first_token_at: None,
        first_tool_call_at: None,
        // A relayed exchange: the two offsets its path can measure, and
        // `None` for the two only a decoded stream can supply.
        first_byte_ms: Some(120),
        first_token_ms: None,
        first_tool_call_ms: None,
        completed_ms: Some(900),
        context_limit_tokens: None,
        framing: Some(Framing {
            declared,
            relayed,
            ended,
        }),
        tokens: None,
        effort: None,
        turn_shape: None,
        tool_rounds: None,
        repairs: None,
    }
}

fn no_headers() -> RateLimitHeaders {
    RateLimitHeaders::default()
}

/// The mapping from what the relay observed to line 1364's vocabulary,
/// one case per class and the served case beside them — every input a
/// status, a header, a count or a way of ending, and not one a byte of
/// a body.
#[test]
fn every_failure_class_is_decided_from_status_headers_and_framing_alone() {
    let served = forwarded_exchange("p", 200, Some(512), Some(512), StreamEnd::Complete);
    assert_eq!(failure_class(&served, &no_headers()), None);

    let chunked_served = forwarded_exchange("p", 200, None, Some(9_000), StreamEnd::Complete);
    assert_eq!(failure_class(&chunked_served, &no_headers()), None);

    for (status, expected) in [
        (401, FailureClass::CredentialFailure),
        (403, FailureClass::CredentialFailure),
        (402, FailureClass::ExhaustedQuota),
        (429, FailureClass::Throttle),
        (400, FailureClass::RequestIncompatibility),
        (404, FailureClass::RequestIncompatibility),
        (413, FailureClass::RequestIncompatibility),
        (500, FailureClass::Upstream5xx),
        (503, FailureClass::Upstream5xx),
        (529, FailureClass::Upstream5xx),
    ] {
        let exchange = forwarded_exchange("p", status, Some(40), Some(40), StreamEnd::Complete);
        assert_eq!(
            failure_class(&exchange, &no_headers()),
            Some(expected),
            "status {status}"
        );
    }

    let truncated = forwarded_exchange("p", 200, Some(1000), Some(100), StreamEnd::Truncated);
    assert_eq!(
        failure_class(&truncated, &no_headers()),
        Some(FailureClass::StreamAbort)
    );
    let aborted = forwarded_exchange("p", 200, None, Some(100), StreamEnd::Aborted);
    assert_eq!(
        failure_class(&aborted, &no_headers()),
        Some(FailureClass::StreamAbort)
    );
    // A stream cut at zero bytes is an abort, not an empty completion:
    // the framing said more was coming.
    let cut_at_zero = forwarded_exchange("p", 200, Some(1000), Some(0), StreamEnd::Truncated);
    assert_eq!(
        failure_class(&cut_at_zero, &no_headers()),
        Some(FailureClass::StreamAbort)
    );

    let empty = forwarded_exchange("p", 200, Some(0), Some(0), StreamEnd::Complete);
    assert_eq!(
        failure_class(&empty, &no_headers()),
        Some(FailureClass::EmptyCompletion)
    );
    // No body was *permitted* — a `204`, or a `HEAD` — so nothing is
    // missing from it.
    let no_body_permitted = forwarded_exchange("p", 204, None, None, StreamEnd::Complete);
    assert_eq!(failure_class(&no_body_permitted, &no_headers()), None);

    let timed_out = Exchange {
        outcome: Outcome::Unreachable {
            detail: TRANSPORT_TIMEOUT_DETAIL,
        },
        ..unreachable_exchange("p")
    };
    assert_eq!(
        failure_class(&timed_out, &no_headers()),
        Some(FailureClass::Timeout)
    );
    assert_eq!(
        failure_class(&unreachable_exchange("p"), &no_headers()),
        Some(FailureClass::Unknown)
    );

    // Never reached the provider: nothing to classify, the same filter
    // `classify` applies.
    for outcome in [
        Outcome::Unauthenticated,
        Outcome::Declined,
        Outcome::Unrouted,
        Outcome::ClientGone,
        Outcome::Idle,
    ] {
        let exchange = Exchange {
            outcome,
            ..unreachable_exchange("p")
        };
        assert_eq!(failure_class(&exchange, &no_headers()), None);
    }
}

/// Line 1365's boundary, read off the headers rather than guessed: a
/// `429` is a spent quota only when nothing remains **and** the window
/// reopens at or beyond the horizon; anything else about it is a
/// throttle.
#[test]
fn a_429_is_exhausted_quota_only_when_nothing_remains_until_a_reset_beyond_the_horizon() {
    let exchange = rate_limited_exchange("p");
    let horizon = EXHAUSTED_QUOTA_HORIZON_SECONDS.to_string();
    let just_under = (EXHAUSTED_QUOTA_HORIZON_SECONDS - 1).to_string();

    let cases: [(&[(&str, &str)], FailureClass); 8] = [
        (&[("retry-after", "2")], FailureClass::Throttle),
        (
            &[
                ("x-ratelimit-remaining", "0"),
                ("x-ratelimit-reset", "3600"),
            ],
            FailureClass::ExhaustedQuota,
        ),
        (
            &[
                ("x-ratelimit-remaining", "0"),
                ("x-ratelimit-reset", &horizon),
            ],
            FailureClass::ExhaustedQuota,
        ),
        (
            &[
                ("x-ratelimit-remaining", "0"),
                ("x-ratelimit-reset", &just_under),
            ],
            FailureClass::Throttle,
        ),
        (
            &[
                ("x-ratelimit-remaining", "5"),
                ("x-ratelimit-reset", "3600"),
            ],
            FailureClass::Throttle,
        ),
        (
            &[("x-ratelimit-remaining", "0"), ("retry-after", "3600")],
            FailureClass::ExhaustedQuota,
        ),
        (&[("x-ratelimit-remaining", "0")], FailureClass::Throttle),
        (
            // An absolute reset an hour past the response's own arrival.
            &[
                ("ratelimit-remaining", "0"),
                ("ratelimit-reset", "1700003600"),
            ],
            FailureClass::ExhaustedQuota,
        ),
    ];
    for (headers, expected) in cases {
        let quota = RateLimitHeaders::read(headers.iter().copied());
        assert_eq!(
            failure_class(&exchange, &quota),
            Some(expected),
            "headers {headers:?}"
        );
    }
}

/// The observation's `outcome` and its `failure_class` agree by
/// construction: a class exactly when the outcome is not a success. Driven
/// through the real producer and read back off the sink it reported to.
#[test]
fn record_routing_observation_writes_the_class_the_failover_count_and_zero_retries() {
    let (sink, seen) = capturing_sink();
    let upstream = Upstream::with_failover(vec![upstream_backend("openrouter")])
        .expect("one backend is not none");
    let routing = SessionRouting::new();
    routing.bind(
        "claude-code",
        "anthropic-messages",
        AssignedModel::named("the-routed-model"),
        &upstream,
    );
    let assignment = routing.assignment();
    let exhausted = RateLimitHeaders::read(vec![
        ("x-ratelimit-remaining", "0"),
        ("x-ratelimit-reset", "7200"),
    ]);

    let cases = [
        (
            rate_limited_exchange("openrouter"),
            &exhausted,
            ExchangeEffect::Unchanged,
        ),
        (
            forwarded_exchange("openrouter", 200, Some(64), Some(64), StreamEnd::Complete),
            &no_headers(),
            ExchangeEffect::Unchanged,
        ),
        (
            forwarded_exchange(
                "openrouter",
                200,
                Some(1000),
                Some(100),
                StreamEnd::Truncated,
            ),
            &no_headers(),
            ExchangeEffect::FailedOver,
        ),
    ];
    for (i, (exchange, quota, effect)) in cases.iter().enumerate() {
        routing.record_routing_observation(
            &sink,
            exchange,
            ExchangeReading {
                quota,
                dispatched_at_unix: 1_700_000_000 + i as i64,
                completed_at_unix: 1_700_000_001 + i as i64,
                assignment: assignment.clone(),
                effect: *effect,
            },
        );
    }

    let reported = routed(&seen);
    // The identity the ledger query used to select on is now the producer's
    // own to state, so it is asserted rather than assumed: a host filtering
    // these rows filters on exactly these four fields.
    for (row, _) in &reported {
        assert_eq!(row.provider, "openrouter");
        assert_eq!(row.model, "the-routed-model");
        assert_eq!(row.route.as_deref(), Some("anthropic-messages"));
        assert_eq!(row.harness.as_deref(), Some("claude-code"));
    }
    // The recorded-at second travels beside the observation, because
    // `NewObservation` carries no "when was this recorded" of its own and a
    // host has no other way to learn it.
    assert_eq!(
        reported.iter().map(|(_, at)| *at).collect::<Vec<_>>(),
        vec![1_700_000_001, 1_700_000_002, 1_700_000_003],
        "each reported observation must carry the completion second of its own exchange"
    );
    let rows: Vec<NewObservation> = reported.into_iter().map(|(row, _)| row).collect();
    assert_eq!(rows.len(), 3);

    assert_eq!(rows[0].failure_class, Some(FailureClass::ExhaustedQuota));
    assert_eq!(rows[0].outcome, Some(RoutingOutcome::Failed));
    assert_eq!(rows[1].failure_class, None);
    assert_eq!(rows[1].outcome, Some(RoutingOutcome::Succeeded));
    assert_eq!(rows[2].failure_class, Some(FailureClass::StreamAbort));
    assert_eq!(
        rows[2].outcome,
        Some(RoutingOutcome::Failed),
        "a 2xx whose stream was cut is not a success at the transport level either"
    );
    assert_eq!(rows[2].failovers, Some(1));
    for row in &rows[..2] {
        assert_eq!(row.failovers, Some(0));
    }
    for row in &rows {
        assert_eq!(row.retries, Some(0), "the gateway forwards exactly once");
        assert_eq!(row.tool_rounds, None);
        assert_eq!(row.repairs, None);
        assert_eq!(
            row.failure_class.is_some(),
            row.outcome != Some(RoutingOutcome::Succeeded),
            "a class exactly when the outcome is not a success: {row:?}"
        );
    }
}

/// Map line 1545: warm, cold and unknown must not collapse into one
/// another. Driven through the real producer and read off its sink, exactly
/// like this test's neighbour above, so this proves the shipped binary's
/// own producer rather than a hand-mirrored mapping.
///
/// Four cases: a real cache read stamps `warm`; a stated usage with a zero
/// cache read stamps `cold`; no usage at all stamps `unknown`; and — the
/// distinction the packet calls out by name — usage stated with the cache
/// count itself unstated *also* stamps `unknown`, never `cold`, because
/// "the provider said nothing about the cache" is not "the cache was
/// empty".
#[test]
fn record_routing_observation_stamps_warm_cold_and_unknown_context_state_from_the_providers_own_cache_read()
 {
    use crate::routing::evidence::ContextState;

    let (sink, seen) = capturing_sink();
    let upstream = Upstream::with_failover(vec![upstream_backend("openrouter")])
        .expect("one backend is not none");
    let routing = SessionRouting::new();
    routing.bind(
        "claude-code",
        "anthropic-messages",
        AssignedModel::named("the-routed-model"),
        &upstream,
    );
    let assignment = routing.assignment();

    fn with_tokens(tokens: Option<Tokens>) -> Exchange {
        Exchange {
            tokens,
            ..forwarded_exchange("openrouter", 200, Some(64), Some(64), StreamEnd::Complete)
        }
    }

    let cases = [
        (
            with_tokens(Some(Tokens {
                input: 100,
                output: 20,
                cached: Some(30),
            })),
            ContextState::Warm,
        ),
        (
            with_tokens(Some(Tokens {
                input: 100,
                output: 20,
                cached: Some(0),
            })),
            ContextState::Cold,
        ),
        (with_tokens(None), ContextState::Unknown),
        (
            with_tokens(Some(Tokens {
                input: 100,
                output: 20,
                cached: None,
            })),
            ContextState::Unknown,
        ),
    ];
    for (i, (exchange, _)) in cases.iter().enumerate() {
        routing.record_routing_observation(
            &sink,
            exchange,
            ExchangeReading {
                quota: &no_headers(),
                dispatched_at_unix: 1_700_000_000 + i as i64,
                completed_at_unix: 1_700_000_001 + i as i64,
                assignment: assignment.clone(),
                effect: ExchangeEffect::Unchanged,
            },
        );
    }

    let rows: Vec<NewObservation> = routed(&seen).into_iter().map(|(row, _)| row).collect();
    assert_eq!(rows.len(), cases.len());
    for (row, (_, expected)) in rows.iter().zip(cases.iter()) {
        assert_eq!(
            row.context_state, *expected,
            "row dispatched at {:?}: {row:?}",
            row.dispatched_at_unix
        );
    }
}

/// What `observe_exchange` says it did is what it did: a real failover
/// answers `FailedOver`, a rotation within the provider answers
/// `RotatedCredential` and is not counted as a failover, and an exchange
/// with nowhere to go answers `Unchanged`.
#[test]
fn observe_exchange_reports_what_it_did_to_the_assignment() {
    let three = Upstream::with_failover(vec![
        upstream_backend("first"),
        upstream_backend("second"),
        upstream_backend("third"),
    ])
    .expect("three backends is not none");
    let routing = SessionRouting::new();
    routing.bind(
        "claude-code",
        "anthropic-messages",
        AssignedModel::named("the-routed-model"),
        &three,
    );
    let effect = routing.observe_exchange(
        &three,
        &unreachable_exchange("first"),
        Instant::now(),
        None,
        None,
    );
    assert_eq!(effect, ExchangeEffect::FailedOver);
    assert_eq!(effect.failovers(), 1);

    let alone =
        Upstream::with_failover(vec![upstream_backend("only")]).expect("one backend is not none");
    let routing = SessionRouting::new();
    routing.bind(
        "claude-code",
        "anthropic-messages",
        AssignedModel::named("the-routed-model"),
        &alone,
    );
    let effect = routing.observe_exchange(
        &alone,
        &unreachable_exchange("only"),
        Instant::now(),
        None,
        None,
    );
    assert_eq!(effect, ExchangeEffect::Unchanged);
    assert_eq!(effect.failovers(), 0);

    let two_keys = Upstream::with_failover(vec![
        upstream_backend_with_credential("openrouter", "OPENROUTER_KEY_A"),
        upstream_backend_with_credential("openrouter", "OPENROUTER_KEY_B"),
    ])
    .expect("two backends is not none");
    let routing = SessionRouting::new();
    routing.bind(
        "claude-code",
        "anthropic-messages",
        AssignedModel::named("the-routed-model"),
        &two_keys,
    );
    let effect = routing.observe_exchange(
        &two_keys,
        &forwarded_exchange("openrouter", 401, Some(0), Some(0), StreamEnd::Complete),
        Instant::now(),
        None,
        None,
    );
    assert_eq!(effect, ExchangeEffect::RotatedCredential);
    assert_eq!(
        effect.failovers(),
        0,
        "a rotation within one provider is not a failover"
    );
}

/// The standalone gateway's failover, which is now the only one there is:
/// with nothing observed about any other turn, the first compatible
/// survivor in configuration order wins.
///
/// This used to be the "no ledger was handed in" half of a pair. It is the
/// whole behaviour since the gateway stopped being able to read a ledger at
/// all — the ranking still runs, on compatibility, entitlement, quota,
/// health, cache locality, stickiness and the failure domain, every one of
/// which this side observed itself; what is gone is only the prior over
/// somebody else's history.
#[test]
fn observe_exchange_falls_back_to_configuration_order_with_no_observations() {
    let upstream = Upstream::with_failover(vec![
        upstream_backend("first"),
        upstream_backend("second"),
        upstream_backend("third"),
    ])
    .expect("three backends is not none");

    let routing = SessionRouting::new();
    routing.bind(
        "claude-code",
        "anthropic-messages",
        AssignedModel::named("the-routed-model"),
        &upstream,
    );

    routing.observe_exchange(
        &upstream,
        &unreachable_exchange("first"),
        Instant::now(),
        None,
        None,
    );

    assert_eq!(
        routing.assignment().map(|a| a.provider().to_owned()),
        Some("second".to_owned())
    );
}

/// A session nobody bound — the standalone binary's — binds itself from its
/// first exchange, so that exchange's failure already fails over. Without
/// this arm `observe_exchange` returns before touching health or the policy
/// and a gateway with no host never moves off a dead account.
#[test]
fn observe_exchange_binds_itself_from_the_first_exchange_when_nobody_bound_it() {
    let upstream =
        Upstream::with_failover(vec![upstream_backend("first"), upstream_backend("second")])
            .expect("two backends is not none");

    let routing = SessionRouting::new();
    assert!(routing.assignment().is_none(), "nothing bound this session");

    routing.observe_exchange(
        &upstream,
        &unreachable_exchange("first"),
        Instant::now(),
        None,
        None,
    );

    let assignment = routing
        .assignment()
        .expect("the first exchange bound the session");
    assert_eq!(
        assignment.provider(),
        "second",
        "and its failure failed over"
    );
    assert_eq!(assignment.harness(), super::UNBOUND_CLIENT);
}

/// A failover lands only where the model can be served: a backend that
/// declares a catalogue without the routed model is skipped even though it
/// carries the protocol, and one that declares nothing is still a candidate.
#[test]
fn observe_exchange_skips_a_candidate_whose_catalogue_omits_the_model() {
    let upstream = Upstream::with_failover(vec![
        upstream_backend("first"),
        upstream_backend("second").with_models(["some-other-model"]),
        upstream_backend("third"),
    ])
    .expect("three backends is not none");

    let routing = SessionRouting::new();
    routing.bind(
        "claude-code",
        "anthropic-messages",
        AssignedModel::named("the-routed-model"),
        &upstream,
    );

    routing.observe_exchange(
        &upstream,
        &unreachable_exchange("first"),
        Instant::now(),
        None,
        None,
    );

    assert_eq!(
        routing.assignment().map(|a| a.provider().to_owned()),
        Some("third".to_owned()),
        "`second` declares a catalogue that does not list the routed model"
    );
}

/// The rendered explanation `Self::observe_exchange` logs for a real
/// failover, captured the way `gateway::ingress::tests::recorded` reads
/// `Exchange::record`'s own log line — through the exact `tracing` call
/// site the accept loop's own build would emit from, not a value handed
/// back for a test to inspect.
fn failover_explanation_log(preference_slug: &str) -> String {
    use std::sync::{Arc, Mutex, Once};

    // `tracing`'s callsite `Interest` cache is a *process-global* static,
    // not a per-thread one: the first time the failover-explanation log
    // line anywhere in this test binary fires, if no subscriber has ever
    // been registered yet, `tracing_core` permanently caches
    // `Interest::never()` for that callsite (an empty dispatcher list
    // folds to "never" — see `tracing_core::callsite::rebuild_callsite_interest`).
    // Another test in this module (e.g. the real-failover assertions
    // near this one) can win that race on its own thread before this
    // helper's `with_default` subscriber ever registers, which is why
    // the capture comes back empty roughly one run in five under
    // `cargo test`'s default thread pool. A `with_default` scope cannot
    // fix this by itself — thread-local scoping only decides who
    // *receives* an event once interest says to emit one. Registering a
    // permanent, sufficiently-verbose global default once, before this
    // helper ever calls into production code, keeps the dispatcher list
    // non-empty for the rest of the process: any later rebuild
    // (including the one triggered by installing this helper's own
    // `with_default` subscriber below) recomputes interest against a
    // live dispatcher instead of an empty list, so the callsite can
    // never get stuck at `never` again.
    static ENSURE_GLOBAL_DISPATCH: Once = Once::new();
    ENSURE_GLOBAL_DISPATCH.call_once(|| {
        let _ = tracing::subscriber::set_global_default(
            tracing_subscriber::fmt()
                .with_writer(std::io::sink)
                .with_max_level(tracing::Level::TRACE)
                .finish(),
        );
    });

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

    // A route the caller calls its own vendor's — the same fixture
    // `routing::interactive::tests::a_failover_explanation_names_the_pairing_class_it_scored`
    // uses — so the native-pairing prior actually has a nonzero
    // magnitude to vary under `Strong` and zero it under `Off`.
    let upstream = Upstream::with_failover(vec![
        upstream_backend("openrouter"),
        upstream_backend("nous"),
    ])
    .expect("two backends is not none");
    let routing = SessionRouting::new();
    routing.bind(
        "claude-code",
        "anthropic-messages",
        AssignedModel::named("claude-fable-5"),
        &upstream,
    );
    // The affinities the caller states for exactly this upstream and model.
    // This crate cannot derive one — the ruling of 2026-09-10 took harness
    // and integration identity out of it entirely — so the table is written
    // here, which is what a caller does: it says which of the routes it
    // configured it would rather have served, and this module carries that
    // through to the policy untouched. Every route in the upstream is named,
    // so what varies between the two calls below is the preference alone.
    let model = AssignedModel::named("claude-fable-5");
    let native = RouteAffinity::new(
        true,
        "vendor-native — the caller's own vendor serves this model",
    );
    routing.set_pairing_preference(
        preference_slug,
        PairingAffinities::new()
            .with("openrouter", &model, native.clone())
            .with("nous", &model, native),
    );

    let sink = Arc::new(Mutex::new(Vec::new()));
    let subscriber = tracing_subscriber::fmt()
        .with_writer(Capture(Arc::clone(&sink)))
        .with_max_level(tracing::Level::DEBUG)
        .without_time()
        .finish();
    tracing::subscriber::with_default(subscriber, || {
        routing.observe_exchange(
            &upstream,
            &unreachable_exchange("openrouter"),
            Instant::now(),
            None,
            None,
        );
    });

    let captured = sink
        .lock()
        .expect("no test panics while holding this")
        .clone();
    String::from_utf8_lossy(&captured).into_owned()
}

/// Phase 9J line 576's own proof for this package's wiring: what
/// `Self::set_pairing_preference` was given reaches
/// `Self::observe_exchange`'s real failover, not a hardcoded
/// `PairingPreference::Strong`. If the call inside `observe_exchange`
/// still passed a literal `PairingPreference::Strong` instead of
/// `state.pairing_preference`, both log lines below would read
/// `+1.000  native-pairing prior` and this test would fail on the second
/// assertion.
#[test]
fn observe_exchange_scores_a_real_failover_against_the_configured_preference() {
    let strong = failover_explanation_log("strong");
    let off = failover_explanation_log("off");

    assert!(
        strong.contains("+1.000  native-pairing prior"),
        "a Strong preference on a real vendor-native pairing must log a full-magnitude \
         prior: {strong}"
    );
    assert!(
        off.contains("+0.000  native-pairing prior"),
        "an Off preference must log a zeroed prior for the very same pairing: {off}"
    );
}

/// Acceptance test 4, through the real production caller (§35/§36): a
/// single `429` on one credential rotates this session to the same
/// provider's other credential — Phase 9I line 537's existing behaviour
/// — and the recorded change must say honestly that this bought a
/// different queue onto the same upstream, never "independent failure
/// handling", per line 1372's inference ban.
#[test]
fn observe_exchange_records_a_credential_rotation_as_a_different_queue_not_independent_failure_handling()
 {
    let upstream = Upstream::with_failover(vec![
        upstream_backend_with_credential("openrouter", "OPENROUTER_API_KEY"),
        upstream_backend_with_credential("openrouter", "OPENROUTER_API_KEY_2"),
    ])
    .expect("two backends is not none");

    let routing = SessionRouting::new();
    routing.bind(
        "claude-code",
        "anthropic-messages",
        AssignedModel::named("the-routed-model"),
        &upstream,
    );

    routing.observe_exchange(
        &upstream,
        &rate_limited_exchange("openrouter"),
        Instant::now(),
        None,
        None,
    );

    let changes = routing.changes();
    let entry = changes.last().expect("a rotation must have been recorded");
    assert_eq!(entry.cause, ChangeCause::CredentialRotation);

    let benefit = entry.benefit();
    assert_eq!(
        benefit,
        RoutingBenefit::DifferentQueueSameUpstream,
        "a same-provider credential rotation must record a different queue onto the same \
         upstream, not {benefit:?}, which is what implying resilience was gained would look \
         like"
    );
    let rendered = benefit.as_str();
    assert!(
        rendered.contains("different queue onto the same upstream"),
        "a same-provider credential rotation must record the honest reason rather than \
         implying resilience was gained: {rendered}"
    );
    assert_ne!(
        benefit,
        RoutingBenefit::UnconfirmedFailureDomainChange,
        "a credential rotation must never be recorded as an (even unconfirmed) failure-domain \
         change — the failure domain did not move"
    );
}

// --- capability map lines 1311/1321/1322/1324: the health snapshot ----

/// A provider [`Self::health_readings_for`] was never asked about, and a
/// provider it was asked about but that never served an exchange, both
/// come back empty — never a fabricated entry for a resource nothing was
/// observed about.
#[test]
fn health_readings_for_an_unobserved_provider_is_empty() {
    let routing = SessionRouting::new();
    assert_eq!(
        routing.health_readings_for("anyrouter", Instant::now(), 1_800_000_000),
        Vec::new()
    );
}

/// Two consecutive rate-limit failures — `routing::free`'s own
/// `FAILURES_BEFORE_COOLDOWN` threshold, exercised through the real
/// production caller [`Self::observe_exchange`] rather than
/// `routing::free::ResourceHealth` directly — must reach
/// [`Self::health_readings_for`] as a cooldown converted to an absolute
/// unix second, and must never leak into a different provider's
/// snapshot.
#[test]
fn health_readings_for_reports_a_real_cooldown_as_an_absolute_unix_deadline_and_only_for_its_own_provider()
 {
    let upstream = Upstream::with_failover(vec![upstream_backend("openrouter")])
        .expect("one backend is not none");
    let routing = SessionRouting::new();
    routing.bind(
        "claude-code",
        "anthropic-messages",
        AssignedModel::named("the-routed-model"),
        &upstream,
    );

    let now = Instant::now();
    let now_unix = 1_800_000_000_i64;
    routing.observe_exchange(
        &upstream,
        &rate_limited_exchange("openrouter"),
        now,
        None,
        None,
    );
    routing.observe_exchange(
        &upstream,
        &rate_limited_exchange("openrouter"),
        now,
        None,
        None,
    );

    let readings = routing.health_readings_for("openrouter", now, now_unix);
    let reading = readings
        .iter()
        .find(|r| r.model == "the-routed-model")
        .expect("the bound model must have a health reading after two real failures");
    assert_eq!(reading.consecutive_failures, 2);
    assert!(!reading.credential_rejected);
    let until = reading
        .cooling_down_until_unix
        .expect("two consecutive rate-limit failures must trigger a cooldown");
    assert!(
        until > now_unix,
        "a fresh cooldown must read as a deadline still in the future: {until} vs {now_unix}"
    );

    assert_eq!(
        routing.health_readings_for("a-different-provider", now, now_unix),
        Vec::new(),
        "a provider's own snapshot must never include another provider's resource"
    );
}

/// Capability map line 1546's write side: [`Self::health_readings_for`]
/// must carry the *cause* `ResourceHealth::fail` already recorded on the
/// same value it reads `cooling_down_until` from, not merely the
/// deadline. This is the exact gap the line's hold ruling
/// (`archive/glasshouse:docs/product/evidence/phase-35b.md`) named: the mechanism existed
/// and this call site silently dropped it.
#[test]
fn health_readings_for_carries_the_cooldown_cause_the_pool_already_recorded() {
    let upstream = Upstream::with_failover(vec![upstream_backend("openrouter")])
        .expect("one backend is not none");
    let routing = SessionRouting::new();
    routing.bind(
        "claude-code",
        "anthropic-messages",
        AssignedModel::named("the-routed-model"),
        &upstream,
    );

    let now = Instant::now();
    let now_unix = 1_800_000_000_i64;
    // A stated `retry_after` applies immediately, on the first failure —
    // REQUIRED BEHAVIOR of `ResourceHealth::fail`, unchanged here.
    routing.observe_exchange(
        &upstream,
        &rate_limited_exchange("openrouter"),
        now,
        Some(Duration::from_secs(60)),
        None,
    );

    let readings = routing.health_readings_for("openrouter", now, now_unix);
    let reading = readings
        .iter()
        .find(|r| r.model == "the-routed-model")
        .expect("a declared wait must produce a health reading");
    assert_eq!(
        reading.cooldown_cause,
        Some(CooldownCause::Declared),
        "a provider-declared wait must cross as a recorded Declared cause, never dropped to \
         None"
    );
}

/// A resource that served after failing is healthy again — Phase 9I line
/// 534's recovery-from-work half — and [`Self::health_readings_for`]
/// reports that as no cooldown at all, not as a deadline already in the
/// past.
#[test]
fn health_readings_for_clears_a_cooldown_once_the_resource_serves_again() {
    let upstream = Upstream::with_failover(vec![upstream_backend("openrouter")])
        .expect("one backend is not none");
    let routing = SessionRouting::new();
    routing.bind(
        "claude-code",
        "anthropic-messages",
        AssignedModel::named("the-routed-model"),
        &upstream,
    );

    let now = Instant::now();
    routing.observe_exchange(
        &upstream,
        &rate_limited_exchange("openrouter"),
        now,
        None,
        None,
    );
    routing.observe_exchange(
        &upstream,
        &rate_limited_exchange("openrouter"),
        now,
        None,
        None,
    );
    routing.observe_exchange(
        &upstream,
        &forwarded_exchange("openrouter", 200, Some(0), Some(0), StreamEnd::Complete),
        now,
        None,
        None,
    );

    let readings = routing.health_readings_for("openrouter", now, 1_800_000_000);
    let reading = readings
        .iter()
        .find(|r| r.model == "the-routed-model")
        .expect("the bound model must still have a health reading");
    assert_eq!(reading.consecutive_failures, 0);
    assert_eq!(reading.cooling_down_until_unix, None);
}
