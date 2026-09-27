//! The client-neutral pairing values a routing policy ranks candidates with.
//!
//! User ruling 2026-09-10: the gateway ranks same-model resources on
//! compatibility, entitlement and cost, quota, health, cache locality,
//! stickiness and failure-domain information. *Which* client is talking to it
//! is not on that list, and a gateway that derived a routing prior from a
//! harness identity would be a gateway only Glasshouse could ship.
//!
//! [`RouteAffinity`] is how a caller that does know says what it knows, in
//! terms this side can honour without learning what a client is: a
//! preference, and the caller's own sentence for why. [`PairingAffinities`]
//! carries those judgements keyed by the two things a
//! [`crate::routing::Backend`] already shows — its provider and its model —
//! so nothing here has to be told how the caller decided. **Empty is the
//! honest default**: nothing preferred, every prior `0.0`, which is exactly
//! what a caller with nothing to say should produce.
//!
//! [`ServingRoute`], [`wire_protocol_from_slug`] and [`EvidenceKey`] live
//! here rather than beside a harness model for the same reason: they are
//! route identity and evidence identity, which a routing policy needs and a
//! harness model does not own. The host re-exports all three at its old
//! paths, so every existing import there stays valid.

use std::collections::BTreeMap;

use crate::routing::AssignedModel;
use crate::routing::wire::WireProtocol;

/// Who is serving the model, and over what.
///
/// Three fields, stored apart from the model and apart from the harness,
/// because line 554 says so and because line 555 is the failure that happens
/// when they are not: a reseller in `provider` must never become an answer to
/// "who developed this".
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ServingRoute {
    /// The service the request is sent to. `None` for a harness running on
    /// its own vendor's first-party service.
    pub provider: Option<String>,
    /// The gateway in front of it, when there is one.
    pub gateway: Option<String>,
    /// The wire protocol the request is carried over.
    pub protocol: Option<WireProtocol>,
}

/// The reverse of [`WireProtocol::slug`], for a caller that only has the
/// slug a [`crate::routing::Backend`] carries — that type's own doc comment
/// explains why `routing` keeps the protocol as a string and never parses it
/// back; this is that parse, for the callers that need a
/// [`ServingRoute::protocol`] to identify a route.
///
/// `None` for a slug none of the four known variants produced — an affinity's
/// `preferred` never depends on it (see
/// [`native_pairing_prior_contribution`]'s own doc),
/// so this only ever weakens a classification, never invents one.
pub fn wire_protocol_from_slug(slug: &str) -> Option<WireProtocol> {
    [
        WireProtocol::AnthropicMessages,
        WireProtocol::OpenAiResponses,
        WireProtocol::OpenAiChat,
        WireProtocol::GeminiGenerateContent,
    ]
    .into_iter()
    .find(|protocol| protocol.slug() == slug)
}

/// The four-part identity Phase 9J line 572 requires local evidence to be
/// kept apart by: client, launch profile, model, and the exact serving route.
///
/// A nominal model id is not enough — the same id reached through a different
/// gateway, quantization, revision or protocol translation is different
/// evidence, and [`ServingRoute`] is exactly the value that already carries
/// that distinction (its `gateway` and `protocol` fields), so this type reuses
/// it rather than inventing a parallel notion of "route". Two
/// [`EvidenceKey`]s compare equal only when all four parts match; nothing
/// here collapses a model to itself across two routes.
///
/// [`EvidenceKey::client`] is an **opaque slug**, never a harness identifier:
/// evidence is partitioned by whoever asked, and this side is not told what
/// the parts of that partition mean. `crate::routing::interactive::Assignment`
/// already carried the harness as a slug string for exactly this reason, and
/// this is the same string.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EvidenceKey {
    client: String,
    launch_profile: String,
    model: AssignedModel,
    route: ServingRoute,
}

impl EvidenceKey {
    pub fn new(
        client: impl Into<String>,
        launch_profile: impl Into<String>,
        model: AssignedModel,
        route: ServingRoute,
    ) -> Self {
        Self {
            client: client.into(),
            launch_profile: launch_profile.into(),
            model,
            route,
        }
    }

    /// Which client's evidence this is, as the opaque slug the caller chose.
    pub fn client(&self) -> &str {
        &self.client
    }

    pub fn launch_profile(&self) -> &str {
        &self.launch_profile
    }

    pub fn model(&self) -> &AssignedModel {
        &self.model
    }

    pub fn route(&self) -> &ServingRoute {
        &self.route
    }
}

/// The caller's own judgement about one candidate, in terms the gateway can
/// honour without knowing what the caller is.
///
/// `preferred` is a **preference, never a filter** — it is worth a decaying
/// prior and nothing else (design decision 1, and map line 566's "never proof
/// of quality or a hard routing requirement"). `reason` is the caller's own
/// sentence, carried through to the routing explanation verbatim so that "why
/// this backend?" is answered in the words of whoever actually knew.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RouteAffinity {
    preferred: bool,
    reason: String,
}

impl RouteAffinity {
    pub fn new(preferred: bool, reason: impl Into<String>) -> Self {
        Self {
            preferred,
            reason: reason.into(),
        }
    }

    /// What a route the caller said nothing about is worth: no preference,
    /// and an explanation line that says the caller was silent rather than
    /// implying it judged and declined.
    pub fn none() -> Self {
        Self {
            preferred: false,
            reason: "the caller stated no affinity for this route, so no preference is claimed \
                     for it"
                .to_owned(),
        }
    }

    /// Whether the caller would rather this candidate served.
    pub fn preferred(&self) -> bool {
        self.preferred
    }

    /// The caller's own words, for the routing explanation.
    pub fn reason(&self) -> &str {
        &self.reason
    }
}

impl Default for RouteAffinity {
    fn default() -> Self {
        Self::none()
    }
}

/// The caller's judgements, keyed by what the gateway can already see on a
/// [`crate::routing::Backend`]: its provider and its model.
///
/// Keyed by those two rather than handed in per candidate because the caller
/// resolves them once, at setup, and the ranking that consumes them happens
/// later — at a provider failure the caller is not present for. A route with
/// no entry gets [`RouteAffinity::none`], so an empty value scores every
/// candidate at `0.0` and reproduces "first compatible candidate" exactly.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PairingAffinities {
    by_route: BTreeMap<(String, String), RouteAffinity>,
}

impl PairingAffinities {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record what the caller thinks of the route `provider` serves `model`
    /// on. A second call for the same route replaces the first.
    pub fn set(&mut self, provider: &str, model: &AssignedModel, affinity: RouteAffinity) {
        self.by_route.insert(route_key(provider, model), affinity);
    }

    /// The builder form, for a caller assembling a whole set in one
    /// expression.
    #[must_use]
    pub fn with(mut self, provider: &str, model: &AssignedModel, affinity: RouteAffinity) -> Self {
        self.set(provider, model, affinity);
        self
    }

    /// What the caller said about this route, or [`RouteAffinity::none`] when
    /// it said nothing. Never `None`: a silent caller is an answer.
    pub fn for_route(&self, provider: &str, model: &AssignedModel) -> RouteAffinity {
        self.by_route
            .get(&route_key(provider, model))
            .cloned()
            .unwrap_or_else(RouteAffinity::none)
    }

    pub fn is_empty(&self) -> bool {
        self.by_route.is_empty()
    }

    pub fn len(&self) -> usize {
        self.by_route.len()
    }
}

/// The two strings a route is keyed by. [`AssignedModel::label`] rather than
/// the value itself so the key is printable and so
/// [`AssignedModel::HarnessDefault`] keys as the one thing it is, rather than
/// needing an ordering on a type that has no natural one.
fn route_key(provider: &str, model: &AssignedModel) -> (String, String) {
    (provider.to_owned(), model.label().to_owned())
}

// ---- The scoring half of what was `config::pairing`: the preference
// strength, the observed-evidence and continuity sources, and the prior
// contribution the failover scorer reads. The override/config half stays
// with the host; only serving arithmetic is here.

/// Line 576 and Phase 49's line 1797: how strongly a user wants a
/// vendor-native pairing preferred, as a configuration value a policy reads —
/// never as a vendor name a policy branches on.
///
/// Four values, and the fourth is not a strength. `Strong`, `Weak` and `Off`
/// scale [`native_pairing_prior_contribution`]'s magnitude; `Pin` does not
/// convert to a magnitude at all — see [`PairingPreference::strength`] — it
/// is the one value the map allows to behave like a hard rule, because the
/// user asked for it by name for an explicitly chosen session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PairingPreference {
    Strong,
    Weak,
    Off,
    Pin,
}

impl Default for PairingPreference {
    /// `EffectiveConfig::native_pairing_preference`'s own out-of-the-box
    /// answer when nothing is configured — see that method's doc comment.
    /// Kept here, next to the type, so a caller that needs "no preference
    /// resolved yet" (Phase 9J line 576's own patch) gets the same default
    /// the configuration layer would have, rather than a second place this
    /// could drift from it.
    fn default() -> Self {
        Self::Strong
    }
}

impl PairingPreference {
    pub fn slug(self) -> &'static str {
        match self {
            Self::Strong => "strong",
            Self::Weak => "weak",
            Self::Off => "off",
            Self::Pin => "pin",
        }
    }

    /// Parse the spelling a configuration file uses, or `None`. A value this
    /// build does not understand is ignored rather than refused — the same
    /// visible-degradation rule `ModelBehaviourFit::from_slug` follows.
    pub fn from_slug(slug: &str) -> Option<Self> {
        match slug {
            "strong" => Some(Self::Strong),
            "weak" => Some(Self::Weak),
            "off" => Some(Self::Off),
            "pin" => Some(Self::Pin),
            _ => None,
        }
    }

    /// This value's strength, when it has one.
    ///
    /// `Pin` returns `None` on purpose: [`native_pairing_prior_contribution`]
    /// takes a [`PriorStrength`], not a [`PairingPreference`], so a caller
    /// cannot pass a pin into the additive scorer even by mistake — it has to
    /// notice the `None` and apply the pin as the hard rule it is, before any
    /// scoring runs. That is design decision 7 made structural rather than a
    /// convention a later edit could quietly break.
    pub fn strength(self) -> Option<PriorStrength> {
        match self {
            Self::Strong => Some(PriorStrength::Strong),
            Self::Weak => Some(PriorStrength::Weak),
            Self::Off => Some(PriorStrength::Off),
            Self::Pin => None,
        }
    }
}

impl std::fmt::Display for PairingPreference {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.pad(self.slug())
    }
}

/// [`PairingPreference`] with `Pin` removed — the type
/// [`native_pairing_prior_contribution`] actually accepts, so that the one
/// value that must never be scored cannot type-check as an argument to the
/// function that scores.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PriorStrength {
    Strong,
    Weak,
    Off,
}

impl PriorStrength {
    /// The prior's magnitude at zero reliable observations, before decay.
    /// Bounded and small relative to what a handful of real observations can
    /// contribute (see [`evidence_signal`]) — design decision 1: a prior a
    /// sufficiently strong observation can always outrank.
    fn base_magnitude(self) -> f64 {
        match self {
            Self::Strong => 1.0,
            Self::Weak => 0.4,
            Self::Off => 0.0,
        }
    }
}

/// Reliable local observations after which the native-pairing prior
/// contributes exactly nothing.
///
/// Design decision 4: the prior decays to zero, not to a floor. A count at or
/// past this contributes `0.0` exactly — not merely small — which is what
/// makes it possible to write a test that asserts zero rather than "smaller
/// than before".
const FULL_DECAY_OBSERVATIONS: usize = 20;

/// The prior's decay factor at `count` reliable observations: `1.0` at zero,
/// linear down to exactly `0.0` at [`FULL_DECAY_OBSERVATIONS`] and beyond.
fn decay_factor(count: usize) -> f64 {
    if count >= FULL_DECAY_OBSERVATIONS {
        0.0
    } else {
        1.0 - (count as f64 / FULL_DECAY_OBSERVATIONS as f64)
    }
}

/// What local observation has established about one [`EvidenceKey`],
/// if anything.
///
/// Line 571 names five kinds of evidence and a user override; this is that
/// list, reduced to a bounded summary rather than one opaque score. Every
/// field is independent and optional on purpose — lines 573 and 574 each need
/// exactly one component to move while the others say nothing, and a single
/// pre-blended number could not be driven that way by a test.
///
/// **A real production source exists**: `crate::routing::evidence::ObservedEvidenceSource`
/// wraps Phase 33A's routing evidence ledger and is what
/// `crate::routing::interactive::score_candidate` hands to
/// [`native_pairing_prior_contribution`] on a real provider failure. A test
/// double (`NoObservations`, or a fixed stand-in) is still what most tests in
/// this file construct, because most of what this file proves is the scoring
/// policy itself, independent of where the numbers came from.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ObservedEvidence {
    /// How many reliable observations back the rest of this struct. Also
    /// what the native-pairing prior decays against — the same count answers
    /// "how much do we trust this" for both the prior's decay and the
    /// evidence signal's confidence.
    ///
    /// Not always the literal raw tally: map line 1548 asks a stale
    /// observation window to count for less, and
    /// `crate::routing::evidence::ObservedEvidenceSource` implements that by
    /// discounting this count before it ever reaches here, rather than adding
    /// a second, parallel "how much do we trust this" number. This field's
    /// own role — "how much do we trust this" — is exactly what makes that a
    /// faithful discount rather than a misrepresentation.
    pub reliable_observation_count: usize,
    /// `0.0..=1.0`. Higher is better.
    pub task_success_rate: Option<f64>,
    /// `0.0..=1.0`. Higher is better.
    pub usable_tool_call_rate: Option<f64>,
    /// `0.0..=1.0`. Lower is better — a repair is a correction the harness
    /// needed after the model's own turn.
    pub repair_rate: Option<f64>,
    /// Effective time-to-first-content, as a ratio to a baseline pairing.
    /// Below `1.0` is faster than the baseline; above is slower.
    pub effective_ttfc_ratio: Option<f64>,
    /// `0.0..=1.0`. Higher is better.
    pub reliability: Option<f64>,
    /// An explicit user override, `-1.0..=1.0`: negative is "I moved away
    /// from this pairing", positive is "I chose it and kept it".
    pub user_override_signal: Option<f64>,
}

impl ObservedEvidence {
    /// No local evidence at all — the state every pairing starts in before
    /// Phase 33A ever records anything for it.
    pub fn none() -> Self {
        Self {
            reliable_observation_count: 0,
            task_success_rate: None,
            usable_tool_call_rate: None,
            repair_rate: None,
            effective_ttfc_ratio: None,
            reliability: None,
            user_override_signal: None,
        }
    }
}

/// A source of local observations for one evidence key.
///
/// A trait rather than a concrete store, on purpose: Phase 33A (the routing
/// evidence ledger this would eventually read) does not exist, verified by
/// `grep -rn 'fn score\|Score' crates/glasshouse/src` finding no match and by
/// `archive/glasshouse:docs/product/evidence/phase-9j.md`'s own account of the two routing
/// callers, neither of which ranks anything. Scoring against a trait means
/// the policy below compiles and is provable with a test double today, and
/// gets a real implementation the day Phase 33A lands — without this file
/// changing.
pub trait ObservationSource {
    /// What has been observed for exactly this evidence key, or `None` when
    /// nothing has.
    fn observed(&self, key: &EvidenceKey) -> Option<ObservedEvidence>;
}

/// An [`ObservationSource`] that answers from nothing, for a caller with no
/// evidence store yet. Every pairing prior computed against this decays
/// exactly like a fresh session's, because a fresh session is exactly what
/// this represents.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoObservations;

impl ObservationSource for NoObservations {
    fn observed(&self, _key: &EvidenceKey) -> Option<ObservedEvidence> {
        None
    }
}

/// Whether an existing session on one pairing can still be *continued*, and
/// how.
///
/// Two states and no third, for the same reason [`crate::routing::Cost`] has
/// two: a session Glasshouse cannot say is continuable must not be credited
/// as if it were. `crate::session::store::SessionRecord::disposition` is what
/// a caller reads this off — `Active` is [`Self::Live`], `Resumable` is
/// [`Self::Resumable`], and `Closed` or `Failed` are not warm sessions at
/// all, so they produce no [`WarmSession`] rather than a third variant here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WarmSessionState {
    /// The session is still running. Continuing it costs nothing and keeps
    /// every byte of accumulated context.
    Live,
    /// The session has stopped but recorded a native identifier, so the
    /// harness can be asked to resume rather than start fresh. Worth less
    /// than [`Self::Live`]: the context survives, the process does not.
    Resumable,
}

impl WarmSessionState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Live => "live",
            Self::Resumable => "resumable",
        }
    }
}

impl std::fmt::Display for WarmSessionState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.pad(self.as_str())
    }
}

/// Map line 569's "relevant warm session", as the two facts a caller can
/// actually establish about one.
///
/// Deliberately **not** a pre-blended "continuity value". Line 569 says a
/// warm session may outweigh the prior *when continuity evidence is
/// stronger*, which means the comparison has to be driveable from evidence
/// rather than from a number somebody already decided — the same reason
/// [`ObservedEvidence`] is a bounded summary of components instead of one
/// opaque score.
///
/// Both fields come straight off one `crate::session::store::SessionRecord`:
/// `state` from its `disposition()`, `idle_seconds` from the clock minus its
/// `last_activity_at`. Nothing here is derived, estimated or guessed, and
/// there is deliberately no field for "how much context accumulated" — the
/// session store records no turn count, and a field a caller could only fill
/// by inventing a number is worse than an absent one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WarmSession {
    pub state: WarmSessionState,
    /// Seconds since that session last did anything. Negative values (a
    /// clock that moved backwards) are treated as zero rather than as
    /// extra freshness — see `continuity_factor`.
    pub idle_seconds: i64,
}

/// A source of relevant warm sessions, one evidence key at a time.
///
/// A trait for exactly the reason [`ObservationSource`] is one, and with a
/// second reason on top: the only module that can answer this is
/// `crate::session`, and `crate::routing::interactive` is forbidden from
/// naming it — `routing::interactive::tests::the_assignment_is_not_a_session_of_its_own`
/// scans the source and fails the build if it ever does, because Phase 9H
/// line 507 requires a gateway assignment not to become a session in its own
/// right. Continuity therefore arrives as a **value the caller looked up**,
/// never as a lookup the policy performs.
pub trait ContinuitySource {
    /// The warm session relevant to exactly this evidence key, or `None`
    /// when there is none.
    ///
    /// "Relevant" is the caller's word to keep: the key already pins the
    /// harness, the launch profile, the model and the route, so a source
    /// that answers `Some` for a key has said those four match. It must not
    /// answer for a *near* match — line 572's whole point is that the same
    /// nominal model served two ways is not one body of evidence, and it is
    /// not one warm session either.
    fn warm_session(&self, key: &EvidenceKey) -> Option<WarmSession>;
}

/// A [`ContinuitySource`] that answers from nothing — a build with no session
/// store to ask, and the state of a machine that has never run this pairing.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoWarmSessions;

impl ContinuitySource for NoWarmSessions {
    fn warm_session(&self, _key: &EvidenceKey) -> Option<WarmSession> {
        None
    }
}

/// How long after its last activity a warm session is worth exactly nothing.
///
/// One working day, decayed linearly to **exactly** zero rather than to a
/// floor — the same shape as `FULL_DECAY_OBSERVATIONS`: a test can assert
/// `0.0` instead of "smaller than before".
///
/// Provisional, reasoning rather than measurement: what is valued is the
/// *conversation* a person could pick back up, not a provider-side prompt
/// cache. Deliberately much shorter than
/// `crate::routing::interactive::FAILOVER_EVIDENCE_WINDOW_SECONDS` (7 days),
/// because that window asks "has this backend behaved well lately" (stays
/// true across days) and this one asks "is this thread still live in
/// someone's head" (does not).
///
/// The measurement that would change it: the distribution of
/// `last_activity_at`-to-resume gaps in the session store — if half of real
/// resumes happen after this window, it is too short.
// History: design-decisions.md, "Trims: api, events, harness and config module docs, second packet", crates/glasshouse/src/config/pairing.rs `WARM_SESSION_RELEVANCE_WINDOW_SECONDS`.
pub const WARM_SESSION_RELEVANCE_WINDOW_SECONDS: i64 = 8 * 60 * 60;

/// What a *live* warm session contributes at zero idle time.
///
/// Above [`PriorStrength::Strong`]'s own `1.0` ceiling on purpose: line 569
/// requires a warm session to be *able* to outweigh the prior, and a value
/// at or below the prior's maximum could never do it however fresh the
/// session was, which would make the line unreachable by construction.
///
/// And only `1.5`, not `10.0`: the crossover has to fall somewhere a person
/// could recognise. At this value a live session outweighs a full-strength
/// `Strong` prior while it has been idle less than a third of the window —
/// about two hours and forty minutes — and loses to it after that. A prior a
/// warm session could never beat is a rule; a prior no warm session could
/// ever survive is also a rule, in the other direction.
const LIVE_WARM_SESSION_VALUE: f64 = 1.5;

/// What a *resumable* warm session contributes at zero idle time.
///
/// Half the live value, and deliberately **below** `PriorStrength::Strong`'s
/// `1.0`: resuming a stopped harness restores the context and not the
/// process, and Glasshouse has no evidence that a resume lands as cleanly as
/// a session that never stopped. So a stopped session never outweighs a
/// full-strength strong preference, and does outweigh a `Weak` one (`0.4`)
/// for the first several hours — which is the interaction line 576's four
/// preference values are supposed to have with this decision.
const RESUMABLE_WARM_SESSION_VALUE: f64 = 0.75;

/// A warm session's freshness factor: `1.0` at zero idle time, linear down to
/// exactly `0.0` at [`WARM_SESSION_RELEVANCE_WINDOW_SECONDS`] and beyond.
///
/// A negative `idle_seconds` — a clock that moved backwards between the
/// session's last activity and now — clamps to `1.0` rather than exceeding
/// it. Freshness is not a thing a wrong clock may award extra of.
fn continuity_factor(idle_seconds: i64) -> f64 {
    if idle_seconds <= 0 {
        return 1.0;
    }
    if idle_seconds >= WARM_SESSION_RELEVANCE_WINDOW_SECONDS {
        return 0.0;
    }
    1.0 - (idle_seconds as f64 / WARM_SESSION_RELEVANCE_WINDOW_SECONDS as f64)
}

/// Map line 569, as one contribution: what a relevant warm session for `key`
/// is worth to a routing decision, on the same scale as
/// [`native_pairing_prior_contribution`]'s own prior.
///
/// Always returns a contribution, `0.0` when there is no warm session, for
/// the same reason the prior is always present even at `0.0`: an explanation
/// that silently omits a term a reader is looking for cannot be distinguished
/// from one where the term was never computed, and a mutation that deletes
/// the lookup would then be invisible.
///
/// **Never negative.** The absence of a warm session is not evidence against
/// a candidate, exactly as [`crate::routing::interactive`]'s failure-domain
/// signal refuses to reward a candidate for an independence nothing
/// established. A cold candidate is scored on its prior and its evidence, and
/// this term simply says nothing about it.
///
/// This is additive and never a filter: a candidate with no warm session is
/// still ranked, and a strong enough `evidence_signal` outranks the
/// warmest session there is — `evidence_signal` is unbounded and this is
/// bounded by `LIVE_WARM_SESSION_VALUE`.
pub fn session_continuity_contribution(
    key: &EvidenceKey,
    continuity: &dyn ContinuitySource,
) -> crate::routing::Contribution {
    use crate::routing::Contribution;

    let Some(warm) = continuity.warm_session(key) else {
        return Contribution::new(
            "session continuity",
            0.0,
            "no relevant warm session for this exact harness, launch profile, model and backend \
             combination — a cold candidate is not penalised for it",
        );
    };

    let base = match warm.state {
        WarmSessionState::Live => LIVE_WARM_SESSION_VALUE,
        WarmSessionState::Resumable => RESUMABLE_WARM_SESSION_VALUE,
    };
    let factor = continuity_factor(warm.idle_seconds);
    Contribution::new(
        "session continuity",
        base * factor,
        format!(
            "a {} warm session for this exact combination, idle {}s of the \
             {WARM_SESSION_RELEVANCE_WINDOW_SECONDS}s a warm session stays relevant for \
             (worth nothing at or past it)",
            warm.state,
            warm.idle_seconds.max(0)
        ),
    )
}

/// How many reliable observations it takes for [`evidence_signal`] to speak
/// at full confidence. Below this, a real but thin observation record still
/// contributes — scaled down, never zeroed — because "no evidence yet" and
/// "one data point" must not read identically to a routing explanation.
const CONFIDENT_AT_OBSERVATIONS: f64 = 5.0;

/// Map line 1542's own named threshold: how many reliable observations local
/// evidence needs before it is trusted to **outrank** the native-pairing
/// prior at all, not merely how confidently [`evidence_signal`] speaks once
/// it is included.
///
/// This is a different question from [`CONFIDENT_AT_OBSERVATIONS`], which
/// only scales a signal that is already in the explanation — a two-sample
/// 100%-success record and a twenty-sample 60%-success record can both clear
/// that scaling and still have the thin one carry a *larger* signal, which is
/// exactly the case line 1542's "must not outrank" (acceptance test 3) rules
/// out. This threshold is the gate that keeps thin evidence out of the
/// comparison entirely, however strong it looks, rather than merely
/// discounted — see [`native_pairing_prior_contribution`]'s own use.
///
/// **Provisional**, exactly like `RETRIEVAL_WEIGHT_FLOOR`: no experiment
/// tuned this number. Equal to [`CONFIDENT_AT_OBSERVATIONS`] and
/// `crate::routing::evidence::MIN_SAMPLE_FOR_SUMMARY` because matching an
/// existing, already-provisional pair was the least arbitrary choice
/// available, not because a fourth independent measurement agreed with the
/// first three.
const SUFFICIENT_EVIDENCE_OBSERVATIONS: usize = 5;

/// Reduce one [`ObservedEvidence`] to a single signed number: positive means
/// the observations support this pairing, negative means they contradict it.
///
/// Unbounded, deliberately, unlike [`PriorStrength::base_magnitude`] — this
/// is design decision 1's escape hatch. A handful of real observations must
/// be able to outrank the prior's bounded maximum, and a signal that saturated
/// at the same ceiling as the prior could never do that no matter how bad or
/// good the evidence was.
fn evidence_signal(observed: &ObservedEvidence) -> f64 {
    let mut signal = 0.0;
    if let Some(rate) = observed.task_success_rate {
        signal += (rate - 0.5) * 2.0;
    }
    if let Some(rate) = observed.usable_tool_call_rate {
        signal += (rate - 0.5) * 2.0;
    }
    if let Some(rate) = observed.repair_rate {
        // Lower is better, so the sign flips relative to the rates above.
        signal += (0.5 - rate) * 2.0;
    }
    if let Some(ratio) = observed.effective_ttfc_ratio {
        signal += (1.0 - ratio).clamp(-1.0, 1.0);
    }
    if let Some(rate) = observed.reliability {
        signal += (rate - 0.5) * 2.0;
    }
    if let Some(override_signal) = observed.user_override_signal {
        signal += override_signal;
    }
    let confidence =
        (observed.reliable_observation_count as f64 / CONFIDENT_AT_OBSERVATIONS).min(1.0);
    signal * confidence
}

/// A sentence naming which of [`ObservedEvidence`]'s components were
/// actually established, for a routing explanation's evidence text.
fn describe_observed(observed: &ObservedEvidence) -> String {
    let mut parts = Vec::new();
    if let Some(v) = observed.task_success_rate {
        parts.push(format!("task success {v:.2}"));
    }
    if let Some(v) = observed.usable_tool_call_rate {
        parts.push(format!("usable tool calls {v:.2}"));
    }
    if let Some(v) = observed.repair_rate {
        parts.push(format!("repair rate {v:.2}"));
    }
    if let Some(v) = observed.effective_ttfc_ratio {
        parts.push(format!("effective TTFC {v:.2}x baseline"));
    }
    if let Some(v) = observed.reliability {
        parts.push(format!("reliability {v:.2}"));
    }
    if let Some(v) = observed.user_override_signal {
        parts.push(format!("user override signal {v:.2}"));
    }
    if parts.is_empty() {
        format!(
            "{} reliable observation(s), no component established",
            observed.reliable_observation_count
        )
    } else {
        format!(
            "{} reliable observation(s): {}",
            observed.reliable_observation_count,
            parts.join(", ")
        )
    }
}

/// Line 566 through 575, as one function: what the caller's affinity for a
/// candidate and the local evidence for `key` contribute to routing it.
///
/// `affinity` is the caller's own judgement, never derived here — user ruling
/// 2026-09-10: the ranking side is told *whether* a route is preferred and
/// *why*, and is never told what the caller is. Everything else this function
/// weighs is local evidence it reads for itself.
/// The explanation always carries the affinity's reason and evidence count
/// (line 575's first two terms, informational), then either a `pinned`
/// line (a pin is a hard rule, not scored), or a `native-pairing prior`
/// (zero unless the caller preferred this route, decayed toward zero as
/// observations accumulate) plus `local observed evidence` (present only with
/// at least one reliable observation, unbounded — so a strong observation can
/// always outrank the prior, design decision 1, and enough bad ones can
/// make a preferred candidate's total lower than a neutral one's, line 574).
///
/// The production caller is `InteractiveRouting::on_provider_failure`, by
/// way of `score_candidate`, reached from
/// `crate::gateway::session::SessionRouting::observe_exchange`.
// History: design-decisions.md, "Trims: api, events, harness and config module docs, second packet", crates/glasshouse/src/config/pairing.rs `native_pairing_prior_contribution`.
pub fn native_pairing_prior_contribution(
    affinity: &RouteAffinity,
    key: &EvidenceKey,
    preference: PairingPreference,
    evidence: &dyn ObservationSource,
) -> crate::routing::RoutingExplanation {
    use crate::routing::Contribution;

    let observed = evidence.observed(key);
    let count = observed
        .as_ref()
        .map(|o| o.reliable_observation_count)
        .unwrap_or(0);

    let mut explanation = crate::routing::RoutingExplanation::new();
    explanation.push(Contribution::new(
        "pairing class",
        0.0,
        affinity.reason().to_owned(),
    ));
    explanation.push(Contribution::new(
        "local evidence strength",
        0.0,
        format!(
            "{count} reliable observation(s) for this exact harness, launch profile, model and \
             backend combination"
        ),
    ));

    let Some(strength) = preference.strength() else {
        explanation.push(Contribution::new(
            "native-pairing preference",
            0.0,
            "pinned: this session was explicitly chosen, so the preference is applied as a hard \
             rule before scoring rather than as a prior contribution"
                .to_owned(),
        ));
        return explanation;
    };

    let is_native = affinity.preferred();
    let magnitude = if is_native {
        strength.base_magnitude() * decay_factor(count)
    } else {
        0.0
    };
    explanation.push(Contribution::new(
        "native-pairing prior",
        magnitude,
        format!(
            "{preference} preference, {} vendor-native, decayed for {count} reliable \
             observation(s) (fully decayed at {FULL_DECAY_OBSERVATIONS})",
            if is_native { "is" } else { "is not" }
        ),
    ));

    if let Some(observed) = observed {
        if observed.reliable_observation_count >= SUFFICIENT_EVIDENCE_OBSERVATIONS {
            // Line 1542: sufficient evidence is free to outrank the prior —
            // `evidence_signal` is unbounded (design decision 1), and nothing
            // here caps it back down.
            explanation.push(Contribution::new(
                "local observed evidence",
                evidence_signal(&observed),
                describe_observed(&observed),
            ));
        } else if observed.reliable_observation_count > 0 {
            // Below the threshold: named and visible (never a silent
            // adjustment), but always `0.0` — a thin record must not be able
            // to outrank the prior no matter how strong it looks, which
            // `evidence_signal`'s own confidence scaling alone cannot
            // guarantee (see `SUFFICIENT_EVIDENCE_OBSERVATIONS`'s own doc
            // comment for the case that scaling misses).
            explanation.push(Contribution::new(
                "local observed evidence",
                0.0,
                format!(
                    "{} reliable observation(s), below the {SUFFICIENT_EVIDENCE_OBSERVATIONS} \
                     needed before local evidence is trusted to outrank the native-pairing prior",
                    observed.reliable_observation_count
                ),
            ));
        }
    }

    explanation
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_route_nobody_judged_is_not_preferred_and_says_so() {
        let affinities = PairingAffinities::new();
        let affinity = affinities.for_route("openrouter", &AssignedModel::named("the-model"));
        assert!(!affinity.preferred());
        assert!(affinity.reason().contains("no affinity"));
    }

    #[test]
    fn an_affinity_is_found_by_provider_and_model_together() {
        let model = AssignedModel::named("the-model");
        let other = AssignedModel::named("another-model");
        let affinities = PairingAffinities::new().with(
            "openrouter",
            &model,
            RouteAffinity::new(true, "the caller's own words"),
        );

        assert!(affinities.for_route("openrouter", &model).preferred());
        assert_eq!(
            affinities.for_route("openrouter", &model).reason(),
            "the caller's own words"
        );
        assert!(
            !affinities.for_route("nous", &model).preferred(),
            "a different provider is a different route"
        );
        assert!(
            !affinities.for_route("openrouter", &other).preferred(),
            "a different model is a different route"
        );
    }

    #[test]
    fn the_harness_default_model_keys_as_itself() {
        let affinities = PairingAffinities::new().with(
            "openrouter",
            &AssignedModel::HarnessDefault,
            RouteAffinity::new(true, "declared"),
        );
        assert!(
            affinities
                .for_route("openrouter", &AssignedModel::HarnessDefault)
                .preferred()
        );
        assert!(
            !affinities
                .for_route("openrouter", &AssignedModel::named("the-model"))
                .preferred()
        );
    }

    #[test]
    fn an_evidence_key_separates_two_routes_to_the_same_model() {
        let one = EvidenceKey::new(
            "claude-code",
            "default",
            AssignedModel::named("the-model"),
            ServingRoute {
                provider: Some("openrouter".to_owned()),
                gateway: None,
                protocol: Some(WireProtocol::AnthropicMessages),
            },
        );
        let two = EvidenceKey::new(
            "claude-code",
            "default",
            AssignedModel::named("the-model"),
            ServingRoute {
                provider: Some("nous".to_owned()),
                gateway: None,
                protocol: Some(WireProtocol::AnthropicMessages),
            },
        );
        assert_ne!(one, two);
        assert_eq!(one.client(), "claude-code");
    }

    #[test]
    fn an_unknown_protocol_slug_is_none_rather_than_a_guess() {
        assert_eq!(
            wire_protocol_from_slug("anthropic-messages"),
            Some(WireProtocol::AnthropicMessages)
        );
        assert_eq!(wire_protocol_from_slug("something-nobody-ships"), None);
    }

    // --- scoring helpers: tests that came with them from the host's config/pairing.rs ---

    #[test]
    fn a_warm_sessions_freshness_is_bounded_at_both_ends() {
        assert_eq!(continuity_factor(0), 1.0);
        assert_eq!(continuity_factor(-3600), 1.0);
        assert_eq!(
            continuity_factor(WARM_SESSION_RELEVANCE_WINDOW_SECONDS),
            0.0
        );
        assert_eq!(
            continuity_factor(WARM_SESSION_RELEVANCE_WINDOW_SECONDS * 100),
            0.0
        );
        let half = continuity_factor(WARM_SESSION_RELEVANCE_WINDOW_SECONDS / 2);
        assert!(
            (half - 0.5).abs() < 1e-9,
            "linear decay, not a curve: {half}"
        );
    }

    #[test]
    fn the_warm_session_values_straddle_the_strongest_prior() {
        assert!(
            LIVE_WARM_SESSION_VALUE > PriorStrength::Strong.base_magnitude(),
            "a live warm session that could never outweigh the prior makes line 569 \
             unreachable"
        );
        assert!(
            RESUMABLE_WARM_SESSION_VALUE < PriorStrength::Strong.base_magnitude(),
            "a prior no warm session of any kind could survive is a rule, not a prior"
        );
        assert!(
            RESUMABLE_WARM_SESSION_VALUE > PriorStrength::Weak.base_magnitude(),
            "continuity must interact with the user's four preference values, not sit \
             above or below all of them"
        );
    }

    #[test]
    fn the_prior_decays_to_exactly_zero_not_a_floor() {
        assert_eq!(decay_factor(0), 1.0);
        assert!(decay_factor(FULL_DECAY_OBSERVATIONS / 2) > 0.0);
        assert!(decay_factor(FULL_DECAY_OBSERVATIONS / 2) < 1.0);
        assert_eq!(decay_factor(FULL_DECAY_OBSERVATIONS), 0.0);
        assert_eq!(decay_factor(FULL_DECAY_OBSERVATIONS * 10), 0.0);
    }

    #[test]
    fn evidence_signal_has_both_signs() {
        let mut good = ObservedEvidence::none();
        good.reliable_observation_count = 20;
        good.task_success_rate = Some(1.0);
        good.reliability = Some(1.0);
        assert!(evidence_signal(&good) > 0.0);

        let mut bad = ObservedEvidence::none();
        bad.reliable_observation_count = 20;
        bad.task_success_rate = Some(0.0);
        bad.reliability = Some(0.0);
        assert!(evidence_signal(&bad) < 0.0);
    }

    #[test]
    fn evidence_signal_scales_with_how_many_observations_back_it() {
        let mut thin = ObservedEvidence::none();
        thin.reliable_observation_count = 1;
        thin.task_success_rate = Some(1.0);

        let mut thick = thin;
        thick.reliable_observation_count = 20;

        assert!(evidence_signal(&thin).abs() < evidence_signal(&thick).abs());
    }
}
