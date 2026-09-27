use super::*;
use crate::routing::evidence::RouteCorrelation;
use crate::routing::pairing::{self, NoObservations};
use crate::routing::{AssignedModel, Cost, CredentialId};
use crate::secret::SecretRef;

fn backend(provider: &str, model: &str) -> Backend {
    backend_with(
        provider,
        model,
        "anthropic-messages",
        ToolSemantics::Unverified,
    )
}

fn backend_with(provider: &str, model: &str, protocol: &str, tools: ToolSemantics) -> Backend {
    Backend::new(
        provider,
        protocol,
        AssignedModel::named(model),
        CredentialId::new(
            provider,
            SecretRef::Environment {
                var: format!("{}_API_KEY", provider.to_uppercase()),
            },
        ),
        Cost::Metered,
        tools,
    )
}

fn session() -> Assignment {
    Assignment::new("claude-code", backend("openrouter", "the-model"))
}

/// What the caller says about `candidates`, stated rather than derived.
///
/// Nothing in this crate may see a harness, an integration identity or a
/// pairing taxonomy (user ruling 2026-09-10), so no test here may build one
/// either: the caller supplies client-neutral affinities and this module
/// ranks on them. This function *is* that caller, and it speaks for one
/// whose own vendor serves the `claude-*` family — those routes are
/// preferred, the rest carry no preference, and each carries the caller's
/// own sentence, which `native_pairing_prior_contribution` copies into the
/// explanation verbatim.
fn stated_affinities(candidates: &[Backend]) -> PairingAffinities {
    let mut affinities = PairingAffinities::new();
    for candidate in candidates {
        affinities.set(
            candidate.provider(),
            candidate.model(),
            caller_affinity_for(candidate.model()),
        );
    }
    affinities
}

/// One route's affinity, as [`stated_affinities`]' caller states it.
fn caller_affinity_for(model: &AssignedModel) -> RouteAffinity {
    if model.label().starts_with("claude-") {
        RouteAffinity::new(
            true,
            "vendor-native — the caller's own vendor serves this model",
        )
    } else {
        RouteAffinity::new(
            false,
            "not vendor-native — the caller's own vendor does not serve this model",
        )
    }
}

/// [`stated_affinities`] for [`session_start_candidates`], which every
/// `start` test below ranks.
fn session_start_affinities() -> PairingAffinities {
    stated_affinities(&session_start_candidates())
}

/// [`InteractiveRouting::on_provider_failure`] with [`stated_affinities`]
/// filled in for `candidates`, so no test below has to repeat the caller's
/// judgement. A test that needs a *different* caller — one that states
/// nothing at all — calls `on_provider_failure` itself.
trait FailOverWithAffinities {
    #[allow(clippy::too_many_arguments)]
    fn fail_over(
        &self,
        current: &Assignment,
        failure: ProviderFailure,
        candidates: &[Backend],
        preference: PairingPreference,
        evidence: &dyn ObservationSource,
        correlations: &RouteCorrelations,
    ) -> FailureResponse;
}

impl FailOverWithAffinities for InteractiveRouting {
    fn fail_over(
        &self,
        current: &Assignment,
        failure: ProviderFailure,
        candidates: &[Backend],
        preference: PairingPreference,
        evidence: &dyn ObservationSource,
        correlations: &RouteCorrelations,
    ) -> FailureResponse {
        let affinities = stated_affinities(candidates);
        self.on_provider_failure(
            current,
            failure,
            candidates,
            preference,
            &affinities,
            evidence,
            correlations,
        )
    }
}

/// A backend on `provider` using a specific credential variable, so a
/// test can put two backends on the same provider with two different
/// quota domains — the exact shape `Upstream::failover_candidates`
/// produces for a provider with two configured keys (see this package's
/// own feasibility note).
fn backend_with_credential(provider: &str, model: &str, var: &str) -> Backend {
    Backend::new(
        provider,
        "anthropic-messages",
        AssignedModel::named(model),
        CredentialId::new(
            provider,
            SecretRef::Environment {
                var: var.to_owned(),
            },
        ),
        Cost::Metered,
        ToolSemantics::Unverified,
    )
}

fn production_code(source: &str) -> String {
    source
        .split("#[cfg(test)]")
        .next()
        .expect("split always yields at least one part")
        .lines()
        .filter(|line| !line.trim_start().starts_with("//"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// A session-start candidate set exactly as the shipped binary builds
/// one: `Upstream::backends()` carries a provider, a credential and a
/// cost and **no model**, and `SessionRouting::bind` supplies one
/// `AssignedModel` for all of them. Every candidate therefore shares the
/// harness and the model.
fn session_start_candidates() -> Vec<Backend> {
    vec![
        backend("openrouter", "claude-fable-5"),
        backend("anthropic", "claude-fable-5"),
        backend("bedrock", "claude-fable-5"),
    ]
}

/// **The finding of this package, as an executable fact.**
///
/// Map line 566 asks the native-pairing prior to matter for a fresh
/// session. It cannot, and the reason is not a missing caller: the prior
/// reads nothing but the affinity the caller stated for the route, and a
/// caller states one from its client and the model — never from the route.
/// A real session-start candidate set varies **only** by route, so every
/// candidate carries the same affinity and every prior has the same
/// magnitude. A signal that is constant on the set being ranked cannot
/// change the ranking.
///
/// The half this crate can still hold is the half asserted below: nothing
/// in `score_candidate` adds a route-derived component of its own on top of
/// what the caller said. How a caller reaches its judgement left with the
/// harness on 2026-09-10.
///
/// This is the same structural bar `archive/glasshouse:docs/product/evidence/phase-9j.md`
/// recorded for same-model failover survivors, and it turns out to be
/// general rather than particular to failover.
#[test]
fn the_native_pairing_prior_is_constant_across_a_real_session_start_candidate_set() {
    let routing = InteractiveRouting::new();
    let candidates = session_start_candidates();

    let mut magnitudes = Vec::new();
    for candidate in &candidates {
        let start = routing
            .start(
                "claude-code",
                "default",
                std::slice::from_ref(candidate),
                &SessionStartInputs {
                    preference: PairingPreference::Strong,
                    affinities: &session_start_affinities(),
                    evidence: &pairing::NoObservations,
                    continuity: &pairing::NoWarmSessions,
                },
            )
            .expect("one candidate is not none");
        let prior = start
            .explanation()
            .contributions()
            .iter()
            .find(|c| c.name() == "native-pairing prior")
            .expect("every explanation carries the prior")
            .magnitude();
        magnitudes.push(prior);
    }

    assert!(
        magnitudes[0] > 0.0,
        "the caller preferred this route, so the prior must be positive — otherwise this \
         test proves nothing about a prior that cannot separate"
    );
    assert!(
        magnitudes.windows(2).all(|pair| pair[0] == pair[1]),
        "the native-pairing prior differed across a session-start candidate set that \
         varies only by route ({magnitudes:?}); if this ever fails, scoring has started \
         reading the route itself and map line 566 has become reachable"
    );
}

/// The other half of the same fact: what *does* separate those candidates
/// is session continuity, because it is keyed by `EvidenceKey`, which
/// carries the route.
#[test]
fn session_continuity_separates_the_same_candidate_set_the_prior_cannot() {
    let routing = InteractiveRouting::new();
    let candidates = session_start_candidates();
    let warm = WarmOn {
        provider: "anthropic",
        session: pairing::WarmSession {
            state: pairing::WarmSessionState::Live,
            idle_seconds: 0,
        },
    };

    let start = routing
        .start(
            "claude-code",
            "default",
            &candidates,
            &SessionStartInputs {
                preference: PairingPreference::Strong,
                affinities: &session_start_affinities(),
                evidence: &pairing::NoObservations,
                continuity: &warm,
            },
        )
        .expect("a non-empty candidate set produces a start");

    assert_eq!(
        start.assignment().provider(),
        "anthropic",
        "the second-configured backend holds the warm session and must win despite the \
         first-configured one tying it on every other signal"
    );
}

/// A `ContinuitySource` that answers for exactly one provider, matched
/// through the `EvidenceKey`'s own route — never by a near match, which
/// is what line 572 forbids.
struct WarmOn {
    provider: &'static str,
    session: pairing::WarmSession,
}

impl pairing::ContinuitySource for WarmOn {
    fn warm_session(&self, key: &pairing::EvidenceKey) -> Option<pairing::WarmSession> {
        (key.route().provider.as_deref() == Some(self.provider)).then_some(self.session)
    }
}

/// A build with nothing to say behaves exactly as it did before `start`
/// existed: the first configured backend serves, which is what
/// `SessionRouting::bind` did by taking `Upstream::serving()`.
#[test]
fn a_fresh_session_with_nothing_observed_keeps_the_configured_order() {
    let routing = InteractiveRouting::new();
    let start = routing
        .start(
            "claude-code",
            "default",
            &session_start_candidates(),
            &SessionStartInputs {
                preference: PairingPreference::Strong,
                affinities: &session_start_affinities(),
                evidence: &pairing::NoObservations,
                continuity: &pairing::NoWarmSessions,
            },
        )
        .expect("a non-empty candidate set produces a start");
    assert_eq!(start.assignment().provider(), "openrouter");
}

/// `best` may not be called with nothing, and a caller with no backends
/// gets an honest `None` rather than a panic.
#[test]
fn a_session_start_with_no_candidates_chooses_nothing() {
    let routing = InteractiveRouting::new();
    assert!(
        routing
            .start(
                "claude-code",
                "default",
                &[],
                &SessionStartInputs {
                    preference: PairingPreference::Strong,
                    affinities: &session_start_affinities(),
                    evidence: &pairing::NoObservations,
                    continuity: &pairing::NoWarmSessions,
                },
            )
            .is_none()
    );
}

/// Line 568 at this caller, and the only place the hard-constraint filter is
/// shown to actually reject: it rejects for the user's own pin. `start` is
/// where `apply_hard_constraints` runs, and since the 2026-09-10 ruling it is
/// the only place it runs in this module.
#[test]
fn a_session_pin_removes_every_other_candidate_before_anything_is_scored() {
    let routing = InteractiveRouting::pinned_to("anthropic");
    let start = routing
        .start(
            "claude-code",
            "default",
            &session_start_candidates(),
            &SessionStartInputs {
                preference: PairingPreference::Strong,
                affinities: &session_start_affinities(),
                evidence: &pairing::NoObservations,
                continuity: &WarmOn {
                    provider: "openrouter",
                    session: pairing::WarmSession {
                        state: pairing::WarmSessionState::Live,
                        idle_seconds: 0,
                    },
                },
            },
        )
        .expect("the pinned provider is among the candidates");
    assert_eq!(start.assignment().provider(), "anthropic");
}

/// A pin naming a provider none of the configured backends serve must not
/// leave a session with nowhere to start. It degrades visibly instead —
/// the same rule an unrecognised configuration value follows everywhere
/// else in this crate.
#[test]
fn a_pin_no_configured_backend_can_satisfy_starts_the_session_and_says_so() {
    let routing = InteractiveRouting::pinned_to("a-provider-nobody-configured");
    let start = routing
        .start(
            "claude-code",
            "default",
            &session_start_candidates(),
            &SessionStartInputs {
                preference: PairingPreference::Strong,
                affinities: &session_start_affinities(),
                evidence: &pairing::NoObservations,
                continuity: &pairing::NoWarmSessions,
            },
        )
        .expect("an unsatisfiable pin must not refuse the session a backend");
    assert_eq!(start.assignment().provider(), "openrouter");
    let note = start
        .explanation()
        .contributions()
        .iter()
        .find(|c| c.name() == "session pin")
        .expect("the unappliable pin is named in the explanation");
    assert_eq!(note.magnitude(), 0.0);
    assert!(note.evidence().contains("a-provider-nobody-configured"));
}

/// Line 507, structurally: a value that cannot see the session model
/// cannot become a session.
#[test]
fn the_assignment_is_not_a_session_of_its_own() {
    let code = production_code(include_str!("mod.rs"));
    assert!(
        !code.contains("crate::session"),
        "routing/interactive.rs names `crate::session`: the gateway assignment has started \
         to look like a session in its own right, which Phase 9H line 507 forbids"
    );
}

/// Line 506: the harness is part of the assignment, not implied by it.
#[test]
fn an_assignment_says_which_harness_it_serves() {
    let assignment = session();
    assert_eq!(assignment.harness(), "claude-code");
    assert!(assignment.label().contains("the-model"));
    assert!(assignment.label().contains("openrouter"));
}

/// Line 509, the one that needs the alternatives to be visible: a free
/// model is sitting right there and the session does not move.
#[test]
fn a_normal_turn_keeps_its_backend_even_when_a_free_model_is_available() {
    let routing = InteractiveRouting::new();
    let current = session();
    let free = Backend::new(
        "nous",
        "anthropic-messages",
        AssignedModel::named("something-free"),
        CredentialId::new(
            "nous",
            SecretRef::Environment {
                var: "NOUS_API_KEY".to_owned(),
            },
        ),
        Cost::Free,
        ToolSemantics::Verified,
    );

    let turn = routing.next_turn(&current, &[free]);
    assert_eq!(turn.assignment(), &current);
    assert_eq!(turn.cache(), &CacheLocality::Preserved);
}

/// Line 513: the same model on another router is a failover.
#[test]
fn failover_prefers_the_same_model_on_another_provider() {
    let routing = InteractiveRouting::new();
    let current = session();
    let other_model_first = backend("kilo", "a-different-model");
    let same_model = backend("nous", "the-model");

    let response = routing.fail_over(
        &current,
        ProviderFailure::Unreachable,
        &[other_model_first, same_model],
        PairingPreference::Strong,
        &NoObservations,
        &RouteCorrelations::default(),
    );

    match response {
        FailureResponse::FailOver { to, cache, .. } => {
            assert_eq!(to.provider(), "nous");
            assert_eq!(to.backend().model(), &AssignedModel::named("the-model"));
            assert_eq!(
                cache,
                CacheLocality::Lost(crate::routing::CacheLossReason::ProviderChanged)
            );
        }
        other => panic!("expected a same-model failover, got {other:?}"),
    }
}

/// Line 514: a different model is offered, never taken.
#[test]
fn a_different_model_is_offered_as_a_migration_rather_than_taken() {
    let routing = InteractiveRouting::new();
    let current = session();
    let response = routing.fail_over(
        &current,
        ProviderFailure::Refused { status: 503 },
        &[backend("kilo", "a-different-model")],
        PairingPreference::Strong,
        &NoObservations,
        &RouteCorrelations::default(),
    );
    match response {
        FailureResponse::OfferMigration { to, .. } => {
            assert_eq!(
                to.backend().model(),
                &AssignedModel::named("a-different-model")
            );
        }
        other => panic!("a material model change must not be taken transparently: {other:?}"),
    }
}

/// Characterizes `ranked_with_cache`, extracted from the identical
/// `best` + `CacheLocality::between` setup the `FailOver` and
/// `OfferMigration` arms used to repeat: the migration arm's offered
/// candidate carries the same cache-locality computation as the failover
/// arm's, not a private copy that could silently drift from it.
#[test]
fn a_migration_offer_carries_the_same_cache_locality_computation_as_failover() {
    let routing = InteractiveRouting::new();
    let current = session();
    let response = routing.fail_over(
        &current,
        ProviderFailure::Refused { status: 503 },
        &[backend("kilo", "a-different-model")],
        PairingPreference::Strong,
        &NoObservations,
        &RouteCorrelations::default(),
    );
    match response {
        FailureResponse::OfferMigration { to, cache, .. } => {
            assert_eq!(
                cache,
                CacheLocality::between(current.backend(), to.backend()),
                "the migration arm's cache locality must be the same computation the \
                 failover arm shares through `ranked_with_cache`"
            );
        }
        other => panic!("expected an offered migration: {other:?}"),
    }
}

/// Line 517: a different protocol is never a failover target.
#[test]
fn failover_never_crosses_a_protocol() {
    let routing = InteractiveRouting::new();
    let current = session();
    let wrong_protocol = backend_with(
        "nous",
        "the-model",
        "openai-chat",
        ToolSemantics::Unverified,
    );

    let response = routing.fail_over(
        &current,
        ProviderFailure::Unreachable,
        &[wrong_protocol],
        PairingPreference::Strong,
        &NoObservations,
        &RouteCorrelations::default(),
    );

    match response {
        FailureResponse::Stay {
            reason: StayReason::NoCompatibleBackend { rejected },
        } => {
            assert_eq!(rejected.len(), 1);
            assert!(matches!(rejected[0], Incompatibility::Protocol { .. }));
        }
        other => panic!("a protocol mismatch must not be failed over to: {other:?}"),
    }
}

/// Line 517's quieter half: tool semantics must not go backwards.
#[test]
fn failover_never_weakens_what_is_established_about_tool_calls() {
    let routing = InteractiveRouting::new();
    let current = Assignment::new(
        "claude-code",
        backend_with(
            "openrouter",
            "the-model",
            "anthropic-messages",
            ToolSemantics::Verified,
        ),
    );
    let known_absent = backend_with(
        "nous",
        "the-model",
        "anthropic-messages",
        ToolSemantics::KnownAbsent,
    );
    let unverified = backend_with(
        "kilo",
        "the-model",
        "anthropic-messages",
        ToolSemantics::Unverified,
    );

    let response = routing.fail_over(
        &current,
        ProviderFailure::Unreachable,
        &[known_absent, unverified],
        PairingPreference::Strong,
        &NoObservations,
        &RouteCorrelations::default(),
    );

    match response {
        FailureResponse::Stay {
            reason: StayReason::NoCompatibleBackend { rejected },
        } => {
            assert_eq!(rejected.len(), 2);
            assert!(
                rejected
                    .iter()
                    .all(|why| matches!(why, Incompatibility::ToolSemantics { .. }))
            );
        }
        other => panic!("tool semantics must not be weakened by a failover: {other:?}"),
    }
}

/// Line 518: a pin turns automatic failover off.
#[test]
fn a_pinned_session_does_not_fail_over_even_when_a_perfect_candidate_exists() {
    let routing = InteractiveRouting::pinned_to("openrouter");
    let current = session();
    let perfect = backend("nous", "the-model");

    let response = routing.fail_over(
        &current,
        ProviderFailure::Unreachable,
        &[perfect],
        PairingPreference::Strong,
        &NoObservations,
        &RouteCorrelations::default(),
    );

    assert_eq!(
        response,
        FailureResponse::Stay {
            reason: StayReason::SessionPinned {
                provider: "openrouter".to_owned()
            }
        }
    );
}

/// A fixed [`ObservationSource`] test double that answers strong evidence
/// for one named provider and poor evidence for every other, so a test
/// can assert *which* candidate a ranking picked rather than only that
/// ranking ran at all.
struct FakeEvidence {
    good_provider: &'static str,
}

impl ObservationSource for FakeEvidence {
    fn observed(&self, key: &pairing::EvidenceKey) -> Option<pairing::ObservedEvidence> {
        let provider = key.route().provider.as_deref()?;
        let mut evidence = pairing::ObservedEvidence::none();
        evidence.reliable_observation_count = 20;
        if provider == self.good_provider {
            evidence.task_success_rate = Some(1.0);
            evidence.reliability = Some(1.0);
        } else {
            evidence.task_success_rate = Some(0.0);
            evidence.reliability = Some(0.0);
        }
        Some(evidence)
    }
}

/// Phase 33A's own consumer, proven decisively: the candidate with real
/// local evidence behind it wins even though it is not first in the
/// caller's order — the §35 proof that ranking, not merely "first
/// compatible candidate", drives this decision. Mutating [`best`] to
/// return `candidates.remove(0)` unconditionally fails this test.
#[test]
fn on_provider_failure_ranks_same_model_survivors_by_local_evidence_not_order() {
    let routing = InteractiveRouting::new();
    let current = session();
    let poor_evidence_first = backend("kilo", "the-model");
    let good_evidence_second = backend("nous", "the-model");

    let response = routing.fail_over(
        &current,
        ProviderFailure::Unreachable,
        &[poor_evidence_first, good_evidence_second],
        PairingPreference::Strong,
        &FakeEvidence {
            good_provider: "nous",
        },
        &RouteCorrelations::default(),
    );

    match response {
        FailureResponse::FailOver { to, .. } => {
            assert_eq!(
                to.provider(),
                "nous",
                "the candidate with strong local evidence must win even though it was not \
                 first in the caller's own order"
            );
        }
        other => panic!("expected a same-model failover: {other:?}"),
    }
}

/// Line 575: a failover's explanation actually names the pairing class
/// and cites the evidence behind it — not merely a value nobody reads.
#[test]
fn a_failover_explanation_names_the_pairing_class_it_scored() {
    let routing = InteractiveRouting::new();
    let current = Assignment::new("claude-code", backend("openrouter", "claude-fable-5"));
    let candidate = backend("nous", "claude-fable-5");

    let response = routing.fail_over(
        &current,
        ProviderFailure::Unreachable,
        &[candidate],
        PairingPreference::Strong,
        &NoObservations,
        &RouteCorrelations::default(),
    );

    match response {
        FailureResponse::FailOver { explanation, .. } => {
            let rendered = explanation.render();
            assert!(
                rendered.contains("vendor-native"),
                "claude-code serving claude-fable-5 is a real vendor-native pairing and the \
                 explanation must say so: {rendered}"
            );
        }
        other => panic!("expected a failover: {other:?}"),
    }
}

/// Phase 9J line 576's own proof: the preference the caller passes in
/// reaches the scorer — this is not a hardcoded `PairingPreference::Strong`
/// wearing a parameter. `Off` must zero the native-pairing prior's
/// magnitude for the very same vendor-native pairing that scores nonzero
/// under `Strong`; if `score_candidate` still used a literal `Strong`
/// internally, `off_magnitude` below would still read nonzero and this
/// test would fail. `native_pairing_prior_contribution` itself is proven
/// never to zero the *contribution line*, only its magnitude, by
/// `tests/pairing_prior.rs`'s
/// `the_prior_is_never_a_filter_even_when_the_preference_is_off`; this is
/// that same property reached through the real caller.
#[test]
fn on_provider_failure_reads_the_callers_preference_not_a_hardcoded_default() {
    let routing = InteractiveRouting::new();
    let current = Assignment::new("claude-code", backend("openrouter", "claude-fable-5"));
    let candidate = backend("nous", "claude-fable-5");

    let prior_magnitude = |preference: PairingPreference| {
        let response = routing.fail_over(
            &current,
            ProviderFailure::Unreachable,
            std::slice::from_ref(&candidate),
            preference,
            &NoObservations,
            &RouteCorrelations::default(),
        );
        match response {
            FailureResponse::FailOver { explanation, .. } => explanation
                .contributions()
                .iter()
                .find(|contribution| contribution.name() == "native-pairing prior")
                .expect("score_candidate always pushes a native-pairing prior contribution")
                .magnitude(),
            other => panic!("expected a failover: {other:?}"),
        }
    };

    let strong_magnitude = prior_magnitude(PairingPreference::Strong);
    let off_magnitude = prior_magnitude(PairingPreference::Off);

    assert_ne!(
        strong_magnitude, 0.0,
        "a Strong preference on a real vendor-native pairing must score a nonzero prior"
    );
    assert_eq!(
        off_magnitude, 0.0,
        "an Off preference must zero the prior even for the same vendor-native pairing"
    );
}

/// A caller that states nothing at all — the empty [`PairingAffinities`] a
/// caller with no judgement to offer hands in — degrades to a `0.0`
/// contribution rather than panicking or silently dropping the candidate,
/// and the failover itself still happens.
///
/// Named for an unrecognised harness slug until the 2026-09-10 ruling took
/// slug-to-affinity resolution out of this crate. An unrecognised slug was
/// only ever one way for a caller to arrive with nothing to say; the empty
/// set is what this module actually sees, so that is what it is tested on.
#[test]
fn on_provider_failure_degrades_when_the_caller_states_no_affinity() {
    let routing = InteractiveRouting::new();
    let current = Assignment::new("some-future-harness", backend("openrouter", "the-model"));
    let candidate = backend("nous", "the-model");

    let response = routing.on_provider_failure(
        &current,
        ProviderFailure::Unreachable,
        &[candidate],
        PairingPreference::Strong,
        &PairingAffinities::new(),
        &NoObservations,
        &RouteCorrelations::default(),
    );

    match response {
        FailureResponse::FailOver {
            to, explanation, ..
        } => {
            assert_eq!(to.provider(), "nous");
            assert!(
                explanation
                    .render()
                    .contains("stated no affinity for this route"),
                "{}",
                explanation.render()
            );
            assert_eq!(
                prior_magnitude(&explanation),
                0.0,
                "a caller that said nothing must earn no prior: {}",
                explanation.render()
            );
        }
        other => {
            panic!("expected a same-model failover even with an unrecognised harness: {other:?}")
        }
    }
}

/// Line 512: only a provider's own failure may move a session.
#[test]
fn a_bad_request_and_a_bad_credential_are_not_provider_failures() {
    assert_eq!(ProviderFailure::from_status(400), None);
    assert_eq!(ProviderFailure::from_status(401), None);
    assert_eq!(ProviderFailure::from_status(403), None);
    assert_eq!(ProviderFailure::from_status(404), None);
    assert_eq!(
        ProviderFailure::from_status(429),
        Some(ProviderFailure::Refused { status: 429 })
    );
    assert_eq!(
        ProviderFailure::from_status(503),
        Some(ProviderFailure::Refused { status: 503 })
    );
}

/// Line 511: a migration is taken at a task boundary and not mid-turn.
#[test]
fn a_migration_is_refused_mid_turn_and_allowed_between_tasks() {
    let routing = InteractiveRouting::new();
    let current = session();
    let to = backend("nous", "a-different-model");

    assert_eq!(
        routing.migrate(&current, to.clone(), SessionActivity::MidTurn),
        Err(MigrationRefusal::MidTurn)
    );

    let migrated = routing
        .migrate(&current, to, SessionActivity::Idle)
        .expect("a compatible backend at a task boundary");
    assert_eq!(migrated.provider(), "nous");
    assert_eq!(migrated.harness(), "claude-code");
}

/// A pin refuses an explicit migration away from it, and says so.
#[test]
fn a_pin_refuses_a_migration_rather_than_being_overridden_by_one() {
    let routing = InteractiveRouting::pinned_to("openrouter");
    let current = session();
    let err = routing
        .migrate(
            &current,
            backend("nous", "the-model"),
            SessionActivity::Idle,
        )
        .expect_err("a pinned session refuses a migration away from the pin");
    assert_eq!(
        err,
        MigrationRefusal::SessionPinned {
            provider: "openrouter".to_owned()
        }
    );
    assert!(err.to_string().contains("lift the pin"));
}

/// Line 515 and 516 together: the record says what moved, and carries the
/// cache warning when there is one.
#[test]
fn a_recorded_failover_names_what_changed_and_warns_about_the_cache() {
    let mut record = RoutingRecord::new();
    let from = session();
    let to = Assignment::new("claude-code", backend("nous", "the-model"));
    let cache = CacheLocality::between(from.backend(), to.backend());

    record.note(AssignmentChange {
        from,
        to,
        cause: ChangeCause::Failover(ProviderFailure::Unreachable),
        cache,
    });

    let entry = &record.entries()[0];
    assert!(entry.changed_provider_or_model());
    let warning = entry.cache_warning().expect("a provider change warns");
    assert!(warning.contains("invalidated"));
}

/// Acceptance test 1 (load-bearing): given two same-model survivors, one
/// sharing the failed backend's own provider (a different credential,
/// the exact shape a provider with two keys produces) and one on a
/// genuinely different provider, the diverse one must win — with nothing
/// else to distinguish them (`PairingPreference::Off` and
/// `NoObservations` zero every other contribution). Removing
/// `failure_domain_contribution` from the loop, or inverting its sign,
/// must make the shared-domain candidate win instead — the packet's
/// `remove-guard` and `invert-condition` mutations.
#[test]
fn on_provider_failure_prefers_a_different_failure_domain_over_a_shared_one() {
    let routing = InteractiveRouting::new();
    let current = session();
    let shared_domain = backend_with_credential("openrouter", "the-model", "OPENROUTER_API_KEY_2");
    let diverse_domain = backend("nous", "the-model");

    let response = routing.fail_over(
        &current,
        ProviderFailure::Unreachable,
        &[shared_domain, diverse_domain],
        PairingPreference::Off,
        &NoObservations,
        &RouteCorrelations::default(),
    );

    match response {
        FailureResponse::FailOver { to, .. } => assert_eq!(
            to.provider(),
            "nous",
            "a candidate on a different provider from the failed backend must be preferred \
             over one sharing its provider, when nothing else distinguishes them"
        ),
        other => panic!("expected a same-model failover: {other:?}"),
    }
}

/// `n` moments at which the failed backend (`session()`'s
/// `openrouter/the-model`) and `provider/the-model` both answered `5xx`,
/// as the correlation a host's ledger hands in: every one of those moments
/// is informative for both routes and every one is an overlap, so the pair
/// carries `2 * n` events and nothing lone.
///
/// Stated rather than folded through `correlate_routes`, which counts these
/// from stored rows and stayed with the ledger. What this module is the test
/// of is the **consumer**: it reads [`RouteCorrelation::verdict`] and nothing
/// about how the count was reached.
fn correlated_with_the_failed_backend(provider: &str, n: usize) -> RouteCorrelations {
    let failed = RouteIdentity::new("openrouter", "the-model");
    let other = RouteIdentity::new(provider, "the-model");
    if failed == other {
        // One route is not a pair, and the ledger's own correlation would
        // find nothing to correlate here either.
        return RouteCorrelations::default();
    }
    RouteCorrelations::stating([RouteCorrelation::stated(failed, other, 2 * n, 0)])
}

/// Line 1376 at the consumer: two overlapping moments (four events, one
/// short of `MIN_CORRELATION_SAMPLE`) change nothing — the correlated
/// candidate still wins on configuration order exactly as with no
/// correlations at all — and the explanation says how many of how many
/// rather than pretending to a confidence.
#[test]
fn on_provider_failure_treats_insufficient_correlation_evidence_exactly_as_none() {
    let routing = InteractiveRouting::new();
    let current = session();
    let candidates = [
        backend("nous", "the-model"),
        backend("mistral", "the-model"),
    ];
    let short = correlated_with_the_failed_backend("nous", 2);

    let with_none = routing.fail_over(
        &current,
        ProviderFailure::Unreachable,
        &candidates,
        PairingPreference::Off,
        &NoObservations,
        &RouteCorrelations::default(),
    );
    let with_short = routing.fail_over(
        &current,
        ProviderFailure::Unreachable,
        &candidates,
        PairingPreference::Off,
        &NoObservations,
        &short,
    );
    let (
        FailureResponse::FailOver { to: none_to, .. },
        FailureResponse::FailOver {
            to, explanation, ..
        },
    ) = (with_none, with_short)
    else {
        panic!("both must be same-model failovers");
    };
    assert_eq!(to.provider(), "nous", "configuration order still decides");
    assert_eq!(none_to.provider(), to.provider());
    let rendered = explanation.render();
    assert!(
        rendered.contains("+0.000  route correlation")
            && rendered.contains(
                "observed at the same moment in 4 of the 5 failures a correlation needs — \
                 insufficient evidence, treated as no correlation"
            ),
        "the sample size is named before anything reads as meaningful: {rendered}"
    );
}

/// Lines 1370, 1373, 1374 and 1852 at the consumer: five overlapping
/// moments make `nous` a measured correlation of `1.00`, weighed as the
/// whole shared-provider penalty, so the candidate configured second
/// wins — and the effect names `nous/the-model` as the route the
/// correlation steered off while line 1851's own count stays
/// untouched, because no candidate shared the failed provider.
#[test]
fn on_provider_failure_steers_off_a_measured_correlation_and_names_the_route() {
    let routing = InteractiveRouting::new();
    let current = session();
    let candidates = [
        backend("nous", "the-model"),
        backend("mistral", "the-model"),
    ];
    let measured = correlated_with_the_failed_backend("nous", 5);

    let response = routing.fail_over(
        &current,
        ProviderFailure::Unreachable,
        &candidates,
        PairingPreference::Off,
        &NoObservations,
        &measured,
    );
    let FailureResponse::FailOver {
        to,
        explanation,
        domain_effect,
        ..
    } = response
    else {
        panic!("expected a same-model failover: {response:?}");
    };
    assert_eq!(
        to.provider(),
        "mistral",
        "a route observed failing with the failed backend every time must lose to one \
         with no such record, even though it is configured first: {}",
        explanation.render()
    );
    assert_eq!(
        domain_effect.correlation_displaced(),
        Some(&RouteIdentity::new("nous", "the-model")),
        "line 1852: the route the correlation steered off is named"
    );
    assert!(
        !domain_effect.prevented(),
        "line 1851 counts the provider-identity term alone, and neither candidate shares \
         the failed provider"
    );
    let rendered = explanation.render();
    assert!(
        rendered.contains("+0.000  route correlation")
            && rendered.contains("observed at the same moment in 0 of the 5"),
        "the winner's own term says it was never observed failing with the failed backend: \
         {rendered}"
    );
}

/// A candidate on the failed backend's own provider carries the
/// provider term and no correlation term: one fact, counted once.
#[test]
fn a_same_provider_candidate_carries_no_correlation_term() {
    let routing = InteractiveRouting::new();
    let current = session();
    let shared = backend_with_credential("openrouter", "the-model", "OPENROUTER_API_KEY_2");
    let response = routing.fail_over(
        &current,
        ProviderFailure::Unreachable,
        &[shared],
        PairingPreference::Off,
        &NoObservations,
        &correlated_with_the_failed_backend("openrouter", 5),
    );
    let FailureResponse::FailOver { explanation, .. } = response else {
        panic!("expected a same-model failover: {response:?}");
    };
    assert!(
        !explanation.render().contains("route correlation"),
        "{}",
        explanation.render()
    );
}

/// Acceptance test 2: a candidate on a different provider is scored
/// `Unknown`, and its evidence string says independence is not
/// established rather than crediting it as proven. See
/// `routing::domain::tests::between_can_never_construct_independent` for
/// the structural half of this line — no code path can produce
/// `FailureDomain::Independent` at all.
#[test]
fn a_cross_provider_candidate_is_scored_unknown_not_independence() {
    let routing = InteractiveRouting::new();
    let current = session();
    let candidate = backend("nous", "the-model");

    let response = routing.fail_over(
        &current,
        ProviderFailure::Unreachable,
        &[candidate],
        PairingPreference::Off,
        &NoObservations,
        &RouteCorrelations::default(),
    );

    match response {
        FailureResponse::FailOver { explanation, .. } => {
            let rendered = explanation.render();
            assert!(
                rendered.contains("independence is not established"),
                "a cross-provider candidate must say independence is not established, not \
                 imply it was proven: {rendered}"
            );
            assert!(
                rendered.contains("+0.000  failure-domain diversity"),
                "an unproven cross-provider candidate must score exactly 0.0 — a bonus for \
                 being on a different provider would be crediting independence nothing \
                 established: {rendered}"
            );
        }
        other => panic!("expected a failover: {other:?}"),
    }
}

/// Acceptance test 5: the contribution appears by name in
/// `RoutingExplanation::render()`, with a signed magnitude, exactly like
/// every other named contribution in this module.
#[test]
fn the_failure_domain_contribution_is_named_in_the_explanation_with_a_signed_magnitude() {
    let routing = InteractiveRouting::new();
    let current = session();
    let shared_domain = backend_with_credential("openrouter", "the-model", "OPENROUTER_API_KEY_2");

    let response = routing.fail_over(
        &current,
        ProviderFailure::Unreachable,
        &[shared_domain],
        PairingPreference::Off,
        &NoObservations,
        &RouteCorrelations::default(),
    );

    match response {
        FailureResponse::FailOver { explanation, .. } => {
            assert!(
                explanation
                    .contributions()
                    .iter()
                    .any(|c| c.name() == "failure-domain diversity"),
                "failure-domain diversity must be its own named contribution, never blended \
                 into an opaque score: {explanation:?}"
            );
            let rendered = explanation.render();
            assert!(
                rendered.contains("-1.000  failure-domain diversity"),
                "a shared failure domain must render a negative, signed magnitude: {rendered}"
            );
        }
        other => panic!("expected a failover: {other:?}"),
    }
}

fn prior_magnitude(explanation: &RoutingExplanation) -> f64 {
    explanation
        .contributions()
        .iter()
        .find(|c| c.name() == "native-pairing prior")
        .expect("every scored candidate carries a native-pairing prior line")
        .magnitude()
}

/// A fixed reliable-observation count and success rate, unconditionally
/// — for exercising `score_candidate`'s sufficiency gate in isolation,
/// independent of what a real ledger could ever produce (it can never
/// answer a count below `MIN_SAMPLE_FOR_SUMMARY`).
struct FixedCount {
    count: usize,
    success_rate: f64,
}

impl ObservationSource for FixedCount {
    fn observed(&self, _key: &pairing::EvidenceKey) -> Option<pairing::ObservedEvidence> {
        let mut evidence = pairing::ObservedEvidence::none();
        evidence.reliable_observation_count = self.count;
        evidence.task_success_rate = Some(self.success_rate);
        Some(evidence)
    }
}

/// Acceptance test 3 (1542/1548): a thin-but-perfect record must not
/// outrank a thick-but-modest one. Two samples at 100% success and
/// twenty at 60% success, scored through `score_candidate` (the exact
/// function `on_provider_failure` calls per candidate): without the
/// sufficiency gate, `evidence_signal`'s own confidence scaling alone is
/// not enough — `(1.0-0.5)*2.0*(2.0/5.0) = 0.4` beats
/// `(0.6-0.5)*2.0*1.0 = 0.2` — so the gate is what actually decides this,
/// not merely a discount on top of an already-correct answer. Setting
/// `SUFFICIENT_EVIDENCE_OBSERVATIONS` to `0` (the packet's
/// `alter-boundary`), or deleting the `>=` branch entirely (`remove-guard`),
/// both let the two-sample record back in and fail this test.
#[test]
fn score_candidate_does_not_let_a_thin_sample_outrank_an_established_one() {
    let thin = FixedCount {
        count: 2,
        success_rate: 1.0,
    };
    let thick = FixedCount {
        count: 20,
        success_rate: 0.6,
    };
    let candidate = backend("nous", "unlisted-model-v1");

    let affinities = stated_affinities(std::slice::from_ref(&candidate));
    let affinity = affinities.for_route(candidate.provider(), candidate.model());

    let thin_explanation = score_candidate(
        "claude-code",
        NO_LAUNCH_PROFILE,
        &candidate,
        PairingPreference::Strong,
        &affinity,
        &thin,
    );
    let thick_explanation = score_candidate(
        "claude-code",
        NO_LAUNCH_PROFILE,
        &candidate,
        PairingPreference::Strong,
        &affinity,
        &thick,
    );

    assert!(
        !thin_explanation
            .contributions()
            .iter()
            .any(|c| c.name() == "local observed evidence" && c.magnitude() != 0.0),
        "two reliable observations must never contribute a nonzero evidence signal: \
         {thin_explanation:?}"
    );
    assert!(
        thick_explanation.total() > thin_explanation.total(),
        "a candidate with a 100% success rate over two samples ({}) must not outrank one \
         with a strong record over many ({})",
        thin_explanation.total(),
        thick_explanation.total()
    );
}
