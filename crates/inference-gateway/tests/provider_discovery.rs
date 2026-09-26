//! Acceptance and matrix tests for provider discovery availability and the
//! discovered-model on-disk cache (Phase 9D).
//!
//! This integration test suite validates:
//! 1. The discovery availability matrix across all 13 built-in provider templates,
//!    ensuring two-way agreement with what the code declares today.
//! 2. The 3-state capability semantics of [`Declared`], proving that `Unverified`
//!    is not the same fact as a verified absence (`Verified(false)`).
//! 3. The public API of [`ModelCache`], ensuring persistence, overwrites, missing
//!    entries, catalogue size extremes (9 to 417 entries), and cross-instance
//!    survival across restarts.
//! 4. That secret credentials never leak into raw cache file bytes on disk.

use inference_gateway::provider::cache::{ModelCache, ModelCatalogue, ModelEntry};
use inference_gateway::provider::quota::{
    Capacity, LimitingUnit, LimitingUnits, NativeAmount, Pool, Reading, ReadingSource, UnitScale,
};
use inference_gateway::provider::registry::{Locality, NativeClient, ResourceKind};
use inference_gateway::routing::wire::Declared;

/// The native harness clients a host names to the registry: a harness's own
/// first-party sign-in, which the gateway carries by slug.
fn native_clients() -> Vec<NativeClient> {
    vec![
        NativeClient::new("claude-code", "Claude Code"),
        NativeClient::new("codex", "Codex"),
        NativeClient::new("opencode", "OpenCode"),
    ]
}

/// Every resource kind the registry describes with those clients.
fn registry() -> Vec<ResourceKind> {
    inference_gateway::provider::registry::registry(&native_clients())
}
use inference_gateway::provider::templates;

/// The complete discovery-availability matrix of built-in templates.
///
/// Built from what the code declares today:
/// - 6 templates offer model discovery (OpenRouter, UnoRouter, AnyRouter,
///   Kilo, Nous, LiteLLM).
/// - 8 templates do not offer model discovery (z.ai, opencode-zen, ollama,
///   llama-cpp, nvidia, openai-compatible, anthropic-compatible, gemini).
///
/// **z.ai is `false` here for a reason worth reading**, because it is the one
/// entry whose state was changed after the batch that wrote this table. It
/// answers `401` to every path under `/api/paas/v4/`, real or invented, so an
/// unauthenticated `401` on `/models` establishes nothing about `/models`.
/// See `provider`'s module documentation for the control run that settled it.
const EXPECTED_DISCOVERY_MATRIX: &[(&str, bool)] = &[
    ("openrouter", true),
    ("unorouter", true),
    ("anyrouter", true),
    ("zai", false),
    ("kilo", true),
    ("nous", true),
    ("opencode-zen", false),
    ("ollama", false),
    ("llama-cpp", false),
    ("nvidia", false),
    ("groq", true),
    ("litellm", true),
    ("openai-compatible", false),
    ("anthropic-compatible", false),
    // provider/documented.rs: from each provider's own docs, none probed.
    ("openai", false),
    ("mistral", false),
    ("deepseek", false),
    ("xai", false),
    ("moonshot", false),
    ("zai-coding", false),
    ("minimax", false),
    ("together", false),
    ("fireworks", false),
    ("cerebras", false),
    ("perplexity", false),
    ("cohere", false),
    ("nebius", false),
    ("deepinfra", false),
    ("sambanova", false),
    ("novita", false),
    ("huggingface", false),
    ("ollama-cloud", false),
    ("vercel", false),
    ("baseten", false),
    ("mimo", false),
    ("stepfun", false),
    ("venice", false),
    ("chutes", false),
    ("lm-studio", false),
    ("gemini-openai", false),
    ("vllm", false),
    // T3 (2026-09-02): Google AI Studio, declared through the unverified
    // generic shape -- no model-list endpoint is claimed until a live probe
    // establishes one, so it offers no discovery.
    ("gemini", false),
    // Phase 66 (2026-09-16): TypeSafe's System One endpoint is a decision
    // protocol, not a chat one; every Declared is Unverified and there is no
    // model-list endpoint, so it offers no discovery.
    ("typesafe", false),
];

// --- A. The discovery-availability matrix, across every shipped template ---

/// Every built-in template returned by `provider::templates()` must appear in
/// the discovery matrix table, and every entry in the table must correspond
/// to a real built-in template. Two-way agreement ensures that deleting or
/// silently altering a template will fail the test immediately.
#[test]
fn every_shipped_template_agrees_with_the_discovery_availability_matrix_in_both_directions() {
    let built_in = templates();

    // Direction 1: Every shipped template must be in the matrix and match.
    for provider in &built_in {
        // Assert that model_list_endpoint is in one of the exactly three valid states.
        match provider.model_list_endpoint {
            Declared::Verified { value: true, .. }
            | Declared::Verified { value: false, .. }
            | Declared::Unverified => {}
        }

        let expected_entry = EXPECTED_DISCOVERY_MATRIX
            .iter()
            .find(|(name, _)| *name == provider.name.as_str());

        assert!(
            expected_entry.is_some(),
            "template `{}` was returned by provider::templates() but is missing from EXPECTED_DISCOVERY_MATRIX",
            provider.name
        );

        let (_, expected_discovery) = expected_entry.unwrap();
        assert_eq!(
            provider.model_list_endpoint.is_known_present(),
            *expected_discovery,
            "template `{}` model discovery state ({:?}) disagrees with EXPECTED_DISCOVERY_MATRIX ({})",
            provider.name,
            provider.model_list_endpoint,
            expected_discovery
        );
    }

    // Direction 2: Every matrix entry must correspond to an actual shipped template.
    for (name, expected_discovery) in EXPECTED_DISCOVERY_MATRIX {
        let matching_template = built_in.iter().find(|p| p.name == *name);

        assert!(
            matching_template.is_some(),
            "EXPECTED_DISCOVERY_MATRIX contains `{name}`, but no such template is returned by provider::templates()",
        );

        let template = matching_template.unwrap();
        assert_eq!(
            template.model_list_endpoint.is_known_present(),
            *expected_discovery,
            "matrix entry `{name}` expected discovery={} but template has {:?}",
            expected_discovery,
            template.model_list_endpoint
        );
    }
}

/// Every template whose `model_list_endpoint` is `Verified { value: true, .. }`
/// must cite non-empty evidence. A verified claim with no evidence is an
/// ungrounded assertion.
#[test]
fn every_template_offering_model_discovery_cites_non_empty_evidence() {
    for provider in templates() {
        if let Declared::Verified {
            value: true,
            evidence,
        } = provider.model_list_endpoint
        {
            assert!(
                !evidence.trim().is_empty(),
                "template `{}` has model_list_endpoint Verified(true) with empty evidence",
                provider.name
            );
        }
    }
}

/// Every template whose `model_list_endpoint` is `Verified { value: true, .. }`
/// cites evidence naming an HTTP URL (or in LiteLLM's case, official proxy documentation).
///
/// Note on packet discrepancy: Section A states that every Verified(true) template
/// cites evidence containing "http". 6 of the 7 verified templates cite an HTTP probe URL,
/// while `litellm` cites proxy documentation without an HTTP prefix. This assertion checks
/// both to remain robust while noting the difference.
#[test]
fn every_template_offering_model_discovery_cites_evidence_naming_a_url_or_documentation() {
    for provider in templates() {
        if let Declared::Verified {
            value: true,
            evidence,
        } = provider.model_list_endpoint
        {
            if provider.name == "litellm" {
                assert!(
                    evidence.contains("documentation") || evidence.contains("http"),
                    "template `litellm` evidence must cite documentation or a URL, found: {evidence}"
                );
            } else {
                assert!(
                    evidence.contains("http"),
                    "template `{}` offers discovery but evidence does not name a URL containing 'http': {evidence}",
                    provider.name
                );
            }
        }
    }
}

/// The count of templates offering discovery is pinned as a single numeric assertion
/// so that any silent addition, removal, or capability toggle fails loudly.
#[test]
fn the_number_of_built_in_templates_offering_model_discovery_is_exactly_seven() {
    let all_templates = templates();
    let discovery_offering_count = all_templates
        .iter()
        .filter(|p| p.model_list_endpoint.is_known_present())
        .count();

    assert_eq!(
        all_templates.len(),
        43,
        "total built-in templates count changed from 43 to {}; update EXPECTED_DISCOVERY_MATRIX intentionally",
        all_templates.len()
    );

    assert_eq!(
        discovery_offering_count, 7,
        "exactly 7 built-in templates must offer model discovery; found \
         {discovery_offering_count}. This was 6 until PACKET-QUOTA-LIVE added the `groq` \
         template, whose GET /models was measured live, 200, a real catalogue, 2026-08-27."
    );
}

// --- B. Unverified is not the same as Verified(false) ---

/// An `Unverified` model-list endpoint reads as "not known present", but this
/// is distinct from a verified absence (`Verified(false)`). Both answer `false`
/// to `is_known_present()`, but represent different epistemic facts ("nobody checked"
/// vs "verified absent").
#[test]
fn unverified_is_not_the_same_as_a_verified_absence_even_though_both_are_not_known_present() {
    let unverified: Declared<bool> = Declared::Unverified;
    let verified_absence: Declared<bool> = Declared::verified(
        false,
        "probed GET https://example.com/v1/models on 2026-08-26 and received 404 Not Found",
    );

    // Both answer false to `is_known_present()`.
    assert!(
        !unverified.is_known_present(),
        "Declared::Unverified must answer false to is_known_present()"
    );
    assert!(
        !verified_absence.is_known_present(),
        "Declared::Verified {{ value: false, .. }} must answer false to is_known_present()"
    );

    // But they are NOT the same fact — collapsing them into a simple boolean would
    // erase the distinction between an unprobed provider and one verified not to serve models.
    assert_ne!(
        unverified, verified_absence,
        "Declared::Unverified and Declared::Verified(false) must not be equal; they represent distinct facts"
    );

    assert_eq!(unverified.value(), None);
    assert_eq!(unverified.evidence(), None);
    assert_eq!(verified_absence.value(), Some(&false));
    assert!(verified_absence.evidence().is_some());

    // Contrast with a verified presence, which answers true to `is_known_present()`.
    let verified_presence: Declared<bool> = Declared::verified(
        true,
        "probed GET https://openrouter.ai/api/v1/models on 2026-08-26 and received 200 with 417 entries",
    );
    assert!(
        verified_presence.is_known_present(),
        "Declared::Verified {{ value: true, .. }} must answer true to is_known_present()"
    );
    assert_ne!(verified_presence, unverified);
    assert_ne!(verified_presence, verified_absence);
}

// --- C. The cache, through its public API only ---

/// A stored catalogue loaded back through `ModelCache::load` must preserve
/// all model entries in order and retain the exact timestamp (`fetched_at`).
#[test]
fn a_stored_catalogue_loads_back_with_the_same_models_and_the_same_timestamp() {
    let dir = tempfile::tempdir().expect("temporary cache directory");
    let cache = ModelCache::at(dir.path());

    let original = ModelCatalogue::new(
        "openrouter",
        "https://openrouter.ai/api/v1",
        "https://openrouter.ai/api/v1/models",
        1_787_336_476,
        vec![
            ModelEntry::new("openai/gpt-4o"),
            ModelEntry::new("anthropic/claude-3.5-sonnet"),
            ModelEntry::new("meta-llama/llama-3.1-70b-instruct"),
        ],
    );

    cache.store(&original).expect("store should succeed");

    let loaded = cache
        .load("openrouter")
        .expect("loaded catalogue must be present");

    assert_eq!(
        loaded.provider(),
        original.provider(),
        "provider name must match across store and load"
    );
    assert_eq!(
        loaded.base_url(),
        original.base_url(),
        "base URL must match across store and load"
    );
    assert_eq!(
        loaded.endpoint(),
        original.endpoint(),
        "endpoint must match across store and load"
    );
    assert_eq!(
        loaded.fetched_at(),
        original.fetched_at(),
        "fetched_at timestamp must survive storage and loading without alteration"
    );
    assert_eq!(
        loaded.models(),
        original.models(),
        "model entries must match exactly in content and order"
    );
}

/// A second store for the same provider with a later timestamp and a different
/// model list must completely replace the previous catalogue. Old model IDs must
/// be gone rather than appended.
#[test]
fn a_second_store_with_a_later_timestamp_replaces_both_the_model_list_and_the_timestamp() {
    let dir = tempfile::tempdir().expect("temporary cache directory");
    let cache = ModelCache::at(dir.path());

    let initial = ModelCatalogue::new(
        "test-provider",
        "https://api.example.com/v1",
        "https://api.example.com/v1/models",
        1_700_000_000,
        vec![
            ModelEntry::new("old-model-alpha"),
            ModelEntry::new("old-model-beta"),
        ],
    );
    cache.store(&initial).expect("initial store");

    let replacement = ModelCatalogue::new(
        "test-provider",
        "https://api.example.com/v1",
        "https://api.example.com/v1/models",
        1_700_001_000,
        vec![
            ModelEntry::new("new-model-gamma"),
            ModelEntry::new("new-model-delta"),
        ],
    );
    cache.store(&replacement).expect("replacement store");

    let loaded = cache
        .load("test-provider")
        .expect("replacement catalogue must be cached");

    assert_eq!(
        loaded.fetched_at(),
        1_700_001_000,
        "a second store must update the fetched_at timestamp forward"
    );
    assert_eq!(
        loaded.len(),
        2,
        "the new catalogue must replace the old list entirely, never append"
    );
    assert_eq!(loaded.models()[0].id(), "new-model-gamma");
    assert_eq!(loaded.models()[1].id(), "new-model-delta");

    // Explicitly assert that the previous model IDs are gone.
    assert!(
        !loaded.models().iter().any(|m| m.id() == "old-model-alpha"),
        "old model ID 'old-model-alpha' must be gone after replacement store"
    );
    assert!(
        !loaded.models().iter().any(|m| m.id() == "old-model-beta"),
        "old model ID 'old-model-beta' must be gone after replacement store"
    );
}

/// `ModelCache::load` on a provider that was never stored must return `None`
/// without error or panic.
#[test]
fn loading_a_provider_that_was_never_stored_returns_none() {
    let dir = tempfile::tempdir().expect("temporary cache directory");
    let cache = ModelCache::at(dir.path());

    assert!(
        cache.load("non-existent-provider").is_none(),
        "loading a provider that was never stored must return None (cache miss), not fail"
    );
}

/// The cache must faithfully store and reload catalogues at both ends of the
/// real catalogue size range: 9 entries (z.ai's live size) and 417 entries
/// (OpenRouter's live size). First and last IDs, as well as the exact count,
/// must survive intact.
#[test]
fn both_ends_of_the_real_catalogue_size_range_survive_storage_and_loading_whole() {
    let dir = tempfile::tempdir().expect("temporary cache directory");
    let cache = ModelCache::at(dir.path());

    // 9 entries (z.ai's size) and 417 entries (OpenRouter's size).
    for count in [9usize, 417] {
        let entries: Vec<ModelEntry> = (0..count)
            .map(|i| ModelEntry::new(format!("vendor/model-{i}")))
            .collect();

        let provider_name = format!("provider-{count}");
        let catalogue = ModelCatalogue::new(
            &provider_name,
            "https://api.example.com/v1",
            "https://api.example.com/v1/models",
            1_787_336_476,
            entries,
        );

        cache.store(&catalogue).expect("catalogue must be stored");

        let loaded = cache
            .load(&provider_name)
            .unwrap_or_else(|| panic!("catalogue of size {count} must be loaded"));

        assert_eq!(
            loaded.len(),
            count,
            "catalogue of size {count} must retain exact entry count"
        );
        assert_eq!(
            loaded.models().first().map(|m| m.id()),
            Some("vendor/model-0"),
            "first model entry ID must survive round-trip for catalogue of size {count}"
        );
        let expected_last_id = format!("vendor/model-{}", count - 1);
        assert_eq!(
            loaded.models().last().map(|m| m.id()),
            Some(expected_last_id.as_str()),
            "last model entry ID must survive round-trip for catalogue of size {count}"
        );

        // Verify every entry survives in exact sequential order without corruption.
        for (i, entry) in loaded.models().iter().enumerate() {
            assert_eq!(
                entry.id(),
                format!("vendor/model-{i}"),
                "entry at index {i} corrupted in catalogue of size {count}"
            );
        }
    }
}

/// Two separate `ModelCache` instances pointing at the same directory must see
/// each other's writes. This verifies that cache state lives entirely on disk
/// and survives process restarts without in-memory state leakage.
#[test]
fn two_model_caches_pointed_at_the_same_directory_see_each_others_writes() {
    let dir = tempfile::tempdir().expect("temporary cache directory");
    let cache_instance_a = ModelCache::at(dir.path());
    let cache_instance_b = ModelCache::at(dir.path());

    let catalogue = ModelCatalogue::new(
        "shared-provider",
        "https://shared.example/v1",
        "https://shared.example/v1/models",
        1_787_500_000,
        vec![ModelEntry::new("model-one"), ModelEntry::new("model-two")],
    );

    // Instance A writes to disk.
    cache_instance_a
        .store(&catalogue)
        .expect("instance A store succeeds");

    // Instance B (simulating a restarted process with a new cache handle) reads it back.
    let read_by_b = cache_instance_b
        .load("shared-provider")
        .expect("instance B must see instance A's write on disk");

    assert_eq!(
        read_by_b.models(),
        catalogue.models(),
        "instance B must load the exact models stored by instance A"
    );
    assert_eq!(
        read_by_b.fetched_at(),
        1_787_500_000,
        "instance B must load the exact timestamp stored by instance A"
    );

    // Now instance B updates the catalogue with new data.
    let updated = ModelCatalogue::new(
        "shared-provider",
        "https://shared.example/v1",
        "https://shared.example/v1/models",
        1_787_600_000,
        vec![ModelEntry::new("model-three")],
    );
    cache_instance_b
        .store(&updated)
        .expect("instance B store succeeds");

    // Instance A sees instance B's update.
    let read_by_a = cache_instance_a
        .load("shared-provider")
        .expect("instance A must see instance B's update on disk");

    assert_eq!(
        read_by_a.len(),
        1,
        "instance A must see the replaced catalogue written by instance B"
    );
    assert_eq!(read_by_a.models()[0].id(), "model-three");
    assert_eq!(read_by_a.fetched_at(), 1_787_600_000);
}

// --- D. No credential on disk ---

/// When storing a catalogue with ordinary provider name, base URL, and model IDs,
/// the raw bytes written to the cache file on disk must never contain secret
/// credentials.
///
/// We assert `!contains` on raw bytes rather than `assert_eq!`, ensuring that
/// even in failure secret values are never formatted and emitted into CI logs.
#[test]
fn a_planted_credential_never_reaches_the_raw_bytes_of_the_cache_file_on_disk() {
    const PLANTED_SECRET: &[u8] = b"sk-live-never-leak-this-secret-credential-into-cache-file-9d";

    let dir = tempfile::tempdir().expect("temporary cache directory");
    let cache = ModelCache::at(dir.path());

    let catalogue = ModelCatalogue::new(
        "openrouter",
        "https://openrouter.ai/api/v1",
        "https://openrouter.ai/api/v1/models",
        1_787_336_476,
        vec![
            ModelEntry::new("openai/gpt-4o"),
            ModelEntry::new("anthropic/claude-3.5-sonnet"),
        ],
    );

    let path = cache.store(&catalogue).expect("store should succeed");

    // Read the raw bytes of the file directly from disk.
    let raw_bytes = std::fs::read(&path).expect("reading raw cache file bytes");

    // Assert that the raw bytes do NOT contain the planted credential.
    let contains_secret = raw_bytes
        .windows(PLANTED_SECRET.len())
        .any(|window| window == PLANTED_SECRET);

    assert!(
        !contains_secret,
        "the raw bytes of cache file `{}` must not contain the planted credential; \
         asserted with !contains on raw bytes, never assert_eq!",
        path.display()
    );

    // Positive control: verify that the file actually contains the expected data
    // so that the negative assertion above cannot pass against an empty file.
    let contains_model = raw_bytes
        .windows(b"openai/gpt-4o".len())
        .any(|window| window == b"openai/gpt-4o");
    assert!(
        contains_model,
        "positive control: cache file must contain the stored model ID"
    );

    let contains_timestamp = raw_bytes
        .windows(b"1787336476".len())
        .any(|window| window == b"1787336476");
    assert!(
        contains_timestamp,
        "positive control: cache file must contain the stored timestamp"
    );
}

// ---------------------------------------------------------------------------
// Phase 32A — the capacity model, from outside the crate
//
// `provider::quota` is exercised in depth by its own unit tests. What only an
// integration test can establish is that the model is *usable* through the
// crate's public API and that it holds across every template the binary
// actually ships, rather than across the two or three a unit test names.
// ---------------------------------------------------------------------------

/// Every shipped template classifies into a capacity state, and its quota
/// shape and that state agree.
///
/// The launch path reads `ResourceKind::quota`. If it were computed beside
/// the capacity model instead of projected out of it, a template could be
/// described one way by one and another way by the other, and no test of
/// either half alone would see it.
#[test]
fn every_shipped_template_has_a_capacity_state_that_agrees_with_its_quota_shape() {
    for provider in templates() {
        let kind = ResourceKind::from_direct_provider(&provider.name);
        let capacity = kind.capacity();
        assert_eq!(
            kind.quota(),
            capacity.model(),
            "`{}` has a quota shape its own capacity state disagrees with",
            provider.name
        );
    }
}

/// The map's own rule: Glasshouse must never invent exact token balances for
/// opaque subscriptions — and, since nothing reads telemetry yet, it must not
/// invent one for anything else either.
///
/// Asserted over every resource the registry can describe, not a sample:
/// nothing the shipped binary can classify reports a measured number or a
/// normalized capacity score, because there is nothing anywhere that could
/// have measured one.
#[test]
fn nothing_the_registry_can_describe_reports_a_capacity_number_it_could_not_have_read() {
    for kind in registry() {
        let capacity = kind.capacity();
        assert!(
            capacity.normalized().is_none(),
            "`{}` produced a normalized capacity score with no telemetry behind it",
            kind.label()
        );
        for (pool_label, pool) in capacity.pools() {
            assert!(
                !pool.remaining().is_measured(),
                "`{}` claims a measured `{pool_label}` remaining",
                kind.label()
            );
            assert!(
                !pool.limit().is_measured(),
                "`{}` claims a measured `{pool_label}` limit",
                kind.label()
            );
        }
    }
}

/// A subscription's token pools are not merely unread — they are unreadable,
/// and a local server's are not readable either, for a different reason. Both
/// have to stay distinguishable from the metered case that Phase 32B may
/// legitimately fill in, or a telemetry pass has no way to know which
/// resources it must leave alone.
#[test]
fn a_subscription_a_local_server_and_a_metered_account_give_three_different_unknowns() {
    let subscription = ResourceKind::NativeSubscription {
        client: native_clients().remove(0),
    }
    .capacity();
    let local = ResourceKind::from_direct_provider("ollama").capacity();
    let metered = ResourceKind::from_direct_provider("openrouter").capacity();

    // Only the metered account's token pool is something telemetry may read.
    assert!(!subscription.tokens().combined().remaining().is_readable());
    assert!(!local.tokens().combined().remaining().is_readable());
    assert!(metered.tokens().combined().remaining().is_readable());

    // And the three unknowns are three different words, not one.
    let words = [
        subscription.tokens().combined().remaining().as_str(),
        local.tokens().combined().remaining().as_str(),
        metered.tokens().combined().remaining().as_str(),
    ];
    assert_eq!(
        words
            .iter()
            .collect::<std::collections::BTreeSet<_>>()
            .len(),
        3,
        "three resource shapes collapsed into fewer than three answers: {words:?}"
    );
}

/// Capability map line 1204, from outside: local inference is unlimited in a
/// way that is not the gateway's answer and not a remote provider's.
#[test]
fn local_inference_the_gateway_and_a_remote_provider_answer_the_limiting_unit_question_apart() {
    let local = ResourceKind::from_direct_provider("llama-cpp").capacity();
    let gateway = ResourceKind::GlasshouseGateway.capacity();
    let remote = ResourceKind::from_direct_provider("nous").capacity();

    assert_eq!(*local.limiting_units(), LimitingUnits::None);
    assert_eq!(local.locality(), Locality::Local);

    assert_eq!(*gateway.limiting_units(), LimitingUnits::Delegated);
    assert!(gateway.limiting_units().named().is_none());

    assert!(remote.limiting_units().includes(LimitingUnit::Credits));
    assert_eq!(remote.locality(), Locality::Remote);
}

/// The public API is enough to build a fully measured capacity state and read
/// a normalized score back out of it *with the provider's own unit intact* —
/// which is what Phase 32B will have to do from outside this module.
#[test]
fn a_caller_outside_the_crate_can_record_a_reading_and_normalize_it_without_losing_the_unit() {
    const OBSERVED: i64 = 1_756_000_000;
    let source = ReadingSource::ProviderEndpoint("https://openrouter.ai/api/v1/credits".to_owned());

    let capacity = ResourceKind::from_direct_provider("openrouter")
        .capacity()
        .with_credits(
            Pool::unmeasured()
                .with_limit(Capacity::Measured(Reading::new(
                    NativeAmount::millionths(8_000_000, "USD"),
                    OBSERVED,
                    source.clone(),
                )))
                .with_remaining(Capacity::Measured(Reading::new(
                    NativeAmount::millionths(1_200_000, "USD"),
                    OBSERVED,
                    source.clone(),
                ))),
        );

    let (pool, score) = capacity.normalized().expect("the credit pool was read");
    assert_eq!(pool, "credits");
    assert_eq!(score.percent().exact(), Some(15));
    assert_eq!(score.native_unit(), "USD");
    assert_eq!(score.remaining().value().value(), 1_200_000);
    assert_eq!(score.remaining().value().scale(), UnitScale::Millionths);
    assert_eq!(score.remaining().source(), &source);
    // The score did not consume the state it came from.
    assert_eq!(
        capacity.credits().remaining().value().unwrap().value(),
        1_200_000
    );
}

// ===== Phase 32B: quota telemetry, from outside the crate ==================

use inference_gateway::provider::quota::{
    CapacityState, Confidence, Freshness, KnownPlan, Percentage, TelemetryClass, UNKNOWN_TELEMETRY,
};
use inference_gateway::provider::telemetry::{
    HarnessTelemetry, RateLimitHeaders, apply_harness_report, apply_provider_headers,
    apply_user_configuration, read_harness_plan,
};

const TELEMETRY_OBSERVED: i64 = 1_787_800_000;

// ===== Phase 33: resource health, through the shipped binary ===============
//
// The health twin of `a_planted_gateway_reading_now_reaches_the_shipped_binarys_report`,
// above: capability map lines 1311/1321/1322/1324, each proven the same way —
// a reading planted exactly where `GatewayHealthCache::new` would put it, and
// the real `Command::Resources` arm asserted against.

/// Capability map line 1229, from outside the crate: the header set a real
/// provider sent becomes a reading with the header's own name on it.
#[test]
fn a_real_providers_rate_limit_headers_become_a_reading_outside_the_crate() {
    let headers = RateLimitHeaders::read(vec![
        ("ratelimit-limit", "300"),
        ("ratelimit-policy", "300;w=60"),
        ("x-ratelimit-limit", "300"),
        ("x-ratelimit-tier", "ip"),
        ("x-ratelimit-window", "60"),
    ]);
    let state = apply_provider_headers(
        ResourceKind::from_direct_provider("anyrouter").capacity(),
        &headers,
        TELEMETRY_OBSERVED,
    );

    let limit = state
        .requests()
        .limit()
        .value()
        .expect("a ceiling was read");
    assert_eq!(limit.value(), 300);
    assert_eq!(limit.unit(), "requests");
    assert_eq!(
        state.requests().limit().telemetry_class(),
        Some(TelemetryClass::Authoritative)
    );
    assert!(
        state
            .requests()
            .limit()
            .describe_source()
            .contains("ratelimit-limit")
    );
    // What the provider did not send stays unknown.
    assert_eq!(state.requests().remaining().telemetry_class(), None);
    assert_eq!(
        state.requests().remaining().telemetry_class_str(),
        UNKNOWN_TELEMETRY
    );
    // Line 1236.
    assert_eq!(state.last_observed_at_unix(), Some(TELEMETRY_OBSERVED));
}

/// Capability map line 1234, from outside the crate, on the type a caller
/// actually holds: an estimate has no accessor that yields a bare figure.
#[test]
fn a_caller_outside_the_crate_cannot_read_an_estimate_as_an_exact_figure() {
    let pool = Pool::unmeasured()
        .with_limit(Capacity::Measured(Reading::new(
            NativeAmount::millionths(10_000_000, "USD"),
            TELEMETRY_OBSERVED,
            ReadingSource::UserConfiguration,
        )))
        .with_remaining(Capacity::Measured(Reading::new(
            NativeAmount::millionths(2_500_000, "USD"),
            TELEMETRY_OBSERVED,
            ReadingSource::LocalObservation("this session's own spend".to_owned()),
        )));
    let score = pool.normalized().expect("both halves were read");

    assert_eq!(score.percent().exact(), None);
    let percentage = score.percent();
    let (percent, confidence, source) = percentage.estimated().expect("an estimate");
    assert_eq!(percent, 25);
    assert_eq!(confidence, Confidence::Medium);
    assert!(!source.is_empty());
    assert!(matches!(score.percent(), Percentage::Estimated { .. }));
    assert!(score.percent().render().contains("estimated"));

    // And the exact case, so the test is not passing for want of a path to
    // the other answer.
    let authoritative = Pool::unmeasured()
        .with_limit(Capacity::Measured(Reading::new(
            NativeAmount::whole(1_000, "requests"),
            TELEMETRY_OBSERVED,
            ReadingSource::ResponseHeader("ratelimit-limit".to_owned()),
        )))
        .with_remaining(Capacity::Measured(Reading::new(
            NativeAmount::whole(250, "requests"),
            TELEMETRY_OBSERVED,
            ReadingSource::ResponseHeader("ratelimit-remaining".to_owned()),
        )))
        .normalized()
        .expect("both halves were read");
    assert_eq!(authoritative.percent().exact(), Some(25));
    assert_eq!(authoritative.percent().render(), "25%");
}

/// Capability map line 1228, from outside the crate, over all four classes.
#[test]
fn authoritative_telemetry_is_preferred_over_every_weaker_class() {
    let authoritative = Capacity::Measured(Reading::new(
        NativeAmount::whole(10, "requests"),
        TELEMETRY_OBSERVED,
        ReadingSource::ResponseHeader("ratelimit-remaining".to_owned()),
    ));
    let weaker = [
        ReadingSource::LocalObservation("this session".to_owned()),
        ReadingSource::UserConfiguration,
        ReadingSource::InferredEstimate("the previous window".to_owned()),
    ];
    for source in weaker {
        // Fresher and still weaker: the authoritative reading survives.
        let candidate = Capacity::Measured(Reading::new(
            NativeAmount::whole(999, "requests"),
            TELEMETRY_OBSERVED + 10_000,
            source.clone(),
        ));
        assert_eq!(
            authoritative
                .clone()
                .prefer(candidate.clone())
                .value()
                .unwrap()
                .value(),
            10,
            "{source:?} displaced an authoritative reading"
        );
        assert_eq!(
            candidate
                .prefer(authoritative.clone())
                .value()
                .unwrap()
                .value(),
            10,
            "{source:?} displaced an authoritative reading, applied the other way"
        );
    }
}

/// Capability map line 1232, from outside the crate: the two seams are
/// independent, and a harness's own report never touches a provider's fields.
#[test]
fn harness_telemetry_and_provider_telemetry_stay_independent_outside_the_crate() {
    let plan = read_harness_plan(
        r#"{"subscriptionType":"max","email":"someone@example.com"}"#,
        TELEMETRY_OBSERVED,
        "claude auth status --json",
    );
    let subscription = apply_harness_report(CapacityState::opaque_subscription(), &plan);
    assert_eq!(
        subscription.plan().value().map(KnownPlan::name),
        Some("max")
    );
    // Nothing about the account holder survived the read.
    assert!(!format!("{subscription:?}").contains("someone@example.com"));
    // And no pool learned anything from a plan.
    assert_eq!(
        subscription.requests(),
        CapacityState::opaque_subscription().requests()
    );

    let provider = apply_provider_headers(
        CapacityState::metered_balance(),
        &RateLimitHeaders::read(vec![("ratelimit-limit", "300")]),
        TELEMETRY_OBSERVED,
    );
    // And no plan learned anything from a header.
    assert_eq!(provider.plan().telemetry_class(), None);
}

/// Capability map line 1237, from outside the crate: the same reading, two
/// ages, two answers.
#[test]
fn staleness_is_decided_by_the_age_a_caller_supplies_and_not_by_a_constant() {
    let reading = Reading::new(
        NativeAmount::whole(300, "requests"),
        TELEMETRY_OBSERVED,
        ReadingSource::ResponseHeader("ratelimit-limit".to_owned()),
    );
    let now = TELEMETRY_OBSERVED + 300;
    assert!(!reading.freshness(now, 900).is_stale());
    assert!(reading.freshness(now, 120).is_stale());
    assert_eq!(reading.freshness(now, 900).age_seconds(), 300);
    assert_eq!(
        Freshness::of(TELEMETRY_OBSERVED, now, 900).age_seconds(),
        300
    );
}

/// Capability map line 1238, from outside the crate: nothing in the telemetry
/// path is fallible, so a caller cannot write an error branch that fails a
/// session on a bad header or an unreadable status report.
#[test]
fn no_telemetry_reader_can_hand_a_caller_an_error_to_fail_a_session_on() {
    // Garbage in every position, and a complete capacity state out.
    let state = apply_user_configuration(
        apply_harness_report(
            apply_provider_headers(
                CapacityState::metered_balance(),
                &RateLimitHeaders::read(vec![
                    ("ratelimit-limit", "not a number"),
                    ("ratelimit-policy", "nonsense"),
                    ("ratelimit-remaining", "-1"),
                ]),
                TELEMETRY_OBSERVED,
            ),
            &read_harness_plan(
                "<html>not json</html>",
                TELEMETRY_OBSERVED,
                "some --interface",
            ),
        ),
        Some("   "),
        None,
        None,
        TELEMETRY_OBSERVED,
    );

    assert_eq!(state.last_observed_at_unix(), None);
    assert_eq!(state.telemetry_class_str(), UNKNOWN_TELEMETRY);
    assert!(state.normalized().is_none());
    // Still a complete, usable model — the exact one a resource with no
    // telemetry has.
    assert_eq!(state, CapacityState::metered_balance());
}

/// Every unknown quantity in the shipped binary answers `unknown` rather than
/// a number, over every entry of the registry and every pool of each — the
/// standing guard against this phase's characteristic failure, extended from
/// Phase 32A's version to cover the telemetry class as well as the value.
#[test]
fn nothing_the_registry_can_describe_claims_a_telemetry_class_it_did_not_earn() {
    for kind in registry() {
        let capacity = kind.capacity();
        assert_eq!(
            capacity.telemetry_class(),
            None,
            "`{}` claims a telemetry class with nothing read",
            kind.label()
        );
        for (label, pool) in capacity.pools() {
            for half in [pool.limit(), pool.remaining()] {
                assert_eq!(
                    half.telemetry_class(),
                    None,
                    "`{}`'s {label} claims a telemetry class with nothing read",
                    kind.label()
                );
                assert_eq!(half.telemetry_class_str(), UNKNOWN_TELEMETRY);
            }
        }
        assert!(capacity.plan().reading().is_none());
        assert_eq!(capacity.last_observed_at_unix(), None);
    }
}

/// Capability map line 1231: the plan reader takes one field and leaves the
/// rest of the body where it found it. Driven from outside the crate, over
/// the shape the real interface was measured emitting.
#[test]
fn a_harness_status_body_yields_a_plan_and_nothing_else_about_the_account() {
    let body = r#"{"loggedIn":true,"authMethod":"claude.ai","apiProvider":"firstParty",
        "email":"someone@example.com","orgId":"5916b68d-0000-0000-0000-000000000000",
        "orgName":"someone@example.com's Organization","subscriptionType":"max"}"#;
    let report = read_harness_plan(body, TELEMETRY_OBSERVED, "claude auth status --json");
    let rendered = format!("{report:?}");

    assert!(rendered.contains("max"));
    for forbidden in [
        "someone@example.com",
        "5916b68d",
        "Organization",
        "claude.ai",
    ] {
        assert!(
            !rendered.contains(forbidden),
            "`{forbidden}` survived the read"
        );
    }
    // And a body with no plan yields nothing rather than a partial account.
    assert!(
        !read_harness_plan(
            r#"{"email":"someone@example.com"}"#,
            TELEMETRY_OBSERVED,
            "x"
        )
        .known_plan()
        .is_measured()
    );
}

/// A `HarnessTelemetry` that read nothing must not blank a plan that is
/// already known — capability map line 1238 on the harness seam.
#[test]
fn a_harness_that_reported_nothing_does_not_erase_a_plan_already_known() {
    let known = apply_harness_report(
        CapacityState::opaque_subscription(),
        &HarnessTelemetry::plan("max", TELEMETRY_OBSERVED, "claude auth status --json"),
    );
    let after = apply_harness_report(known, &HarnessTelemetry::nothing());
    assert_eq!(after.plan().value().map(KnownPlan::name), Some("max"));
}

// ===== Phase 34C was the routing model in resource diagnostics =============
//
// Capability map lines 1443 and 1433 asked for the currently selected
// automatic routing model and a provider-health filter over its candidates
// in `glasshouse resources`'s diagnostics — both through
// `main.rs::render_routing_model`, which ran the same
// `automatic_classification_choice` decision `glasshouse classify` made.
// Both the decision and the renderer are gone with the routing deletion
// (design-decisions, 2026-09-16): Glasshouse never decides which model is
// used, so there is no more automatic routing model for a resource report
// to name, and no ROUTING MODEL block for these six tests to find.
