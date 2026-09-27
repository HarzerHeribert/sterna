//! The evidence ledger's value vocabulary — the types a routed turn is
//! *described* in, with no database underneath them.
//!
//! **Invariant: this file names neither `rusqlite` nor the crate's database
//! layer nor the ledger that stores these values**, enforced by a scan in
//! `tests.rs`. It holds because every type here is a plain value: the SQL
//! that reads and writes them lives in `super` and its `joins`, `readers`
//! and `signals` siblings, and each of those converts at its own edge. The
//! gateway produces these observations and must be able to do so without a
//! database present at all.
//!
//! Moved out of `mod.rs` verbatim (Phase 59 decomposition rule 2); every
//! `crate::routing::evidence::X` path still resolves through `mod.rs`'s
//! `pub use vocabulary::*`.

use crate::provider::quota::{Confidence, Freshness, Reading, ReadingSource};

/// How many reliable observations a bucket needs before
/// `EvidenceLedger::summarize` answers anything but "unknown" for it —
/// capability map line 1340: *"too small a sample yields unknown, not a wide
/// error bar."*
///
/// Five, matching `crate::config::pairing::CONFIDENT_AT_OBSERVATIONS` — not
/// because the two numbers must agree, but because both answer the same
/// underlying question ("how many local observations before this project
/// trusts them at all") and picking a different number here with no evidence
/// either way would be exactly the kind of unearned precision line 1234
/// exists to forbid on the quota side.
pub const MIN_SAMPLE_FOR_SUMMARY: usize = 5;

/// The fallback bucket for a `routing_observations.harness` column that is
/// `NULL`, or for a `sessions.harness` join that found no row — the same
/// convention `crate::evaluation::UNKNOWN_COST_CLASS` gives the tier and
/// pairing-class readers, spelled once here so
/// `EvidenceLedger::request_stats_by_harness` and
/// `evaluation::EvaluationObservations::outcomes_by_tier_and_harness`
/// cannot drift into two different words for the same absence.
pub const UNKNOWN_HARNESS: &str = "unknown";

/// What `routing_observations.purpose` records for a routing-model
/// classification call — `main.rs`'s `glasshouse classify` producer writes
/// it, and `EvidenceLedger::classification_record` reads it back.
///
/// Spelled once, here, because two spellings of one word would silently
/// split the only producer from the only reader: `purpose` is a `TEXT`
/// column with no `CHECK` (migration 11), so nothing in the schema would
/// notice.
pub const CLASSIFICATION_PURPOSE: &str = "classification";

/// What `routing_observations.purpose` records for a memory-extraction call
/// — `main.rs`'s `record_extraction_observation` producer writes it, and
/// `RoutingOverhead` reads it back as its own bucket.
///
/// **Rows written before this constant existed carry `NULL` and stay that
/// way.** [`NewObservation::with_purpose`]'s own doc comment records why:
/// back-filling them would make *"this build recorded nothing here"*
/// indistinguishable from *"this build recorded a purpose"*. So the stamp
/// applies from now on, an unstamped row is counted as unstamped, and
/// nothing is ever re-labelled — which is what makes capability map line
/// 1832's separation honest rather than retroactive.
pub const EXTRACTION_PURPOSE: &str = "memory-extraction";

/// What `routing_observations.purpose` records for map line 1849's
/// decision-latency row — `main.rs`'s `record_routing_latency` producer
/// writes it.
///
/// Spelled here rather than only at that producer so `RoutingOverhead` can
/// read the same word: a second spelling would silently split the only
/// producer from the only reader, exactly as [`CLASSIFICATION_PURPOSE`]'s
/// own doc says.
///
/// **A row under this purpose is not a model call.** It records the wall
/// clock a routing decision took, and carries no tokens at all — which is
/// why it is its own bucket rather than folded into
/// [`CLASSIFICATION_PURPOSE`]'s, where it would inflate a count of model
/// requests with rows no model ever served.
pub const ROUTING_LATENCY_PURPOSE: &str = "routing-latency";

/// What `routing_observations.purpose` records when the session router
/// **escalated** the tier a decision prefers — capability map line 1566,
/// written by `main.rs`'s `record_tier_movement` on the launch path (the
/// path that acts; `glasshouse route` reports and records nothing).
///
/// Spelled here beside [`CLASSIFICATION_PURPOSE`] for its reason, and read
/// back by `RoutingOverhead` into its own bucket: a movement row is not a
/// model call and carries no tokens, so it must be neither counted as one
/// nor left to the unstamped bucket as though no producer had named it.
///
/// **The row records that a movement happened and its direction, and
/// nothing else.** The tiers it moved between and the destination it landed
/// on have no column, and adding one is a migration this producer's package
/// may not make; it writes the same `glasshouse`/`session-router` identity
/// [`ROUTING_LATENCY_PURPOSE`]'s producer writes, so it can never blend into
/// a real model's latency summary.
pub const TIER_ESCALATION_PURPOSE: &str = "tier-escalation";

/// [`TIER_ESCALATION_PURPOSE`]'s other direction — line 1566 asks for both,
/// and a reader counting one must not have to subtract the other.
pub const TIER_DOWNGRADE_PURPOSE: &str = "tier-downgrade";

/// Capability map line 1970: one ledger row per pool fallback the launch
/// path acted on, under this purpose or
/// [`ENTITLEMENT_FALLBACK_THROTTLED_PURPOSE`], so a later evaluation can
/// count how often the broker left an account and why. `quota_context`
/// carries the account the work **left**, while `provider` and `model` are
/// the chosen destination's; the purpose column is what keeps these rows
/// out of any model's own summary. A decision that made no fallback writes
/// nothing — "the broker stayed put" is the row's absence, exactly as a
/// held tier is.
pub const ENTITLEMENT_FALLBACK_EXHAUSTED_PURPOSE: &str = "entitlement-fallback-exhausted";

/// [`ENTITLEMENT_FALLBACK_EXHAUSTED_PURPOSE`]'s other trigger.
pub const ENTITLEMENT_FALLBACK_THROTTLED_PURPOSE: &str = "entitlement-fallback-throttled";

/// Capability map line 1987: one row per tool result the context firewall
/// deterministically reduced — mirroring
/// [`ENTITLEMENT_FALLBACK_EXHAUSTED_PURPOSE`]'s own shape, this purpose or
/// [`CONTEXT_FIREWALL_BYPASS_PURPOSE`] beside it. `quota_context` carries
/// the tool name (`crate::firewall`'s own categorical label, the same role
/// [`ENTITLEMENT_FALLBACK_EXHAUSTED_PURPOSE`]'s doc comment gives the
/// account it records), and `harness` carries the adapter's own harness
/// slug — today always `claude-code`, since `firewall::adapter` is
/// that harness's own. **This purpose's rows never set
/// `input_tokens`/`output_tokens`**: those columns are documented as a
/// provider's own reported count ([`NewObservation::with_tokens`]), and
/// this build's raw/forwarded figures are `chars/4` estimates, never a
/// provider's word — writing them there would make "recorded nothing"
/// indistinguishable from "the provider reported this", the exact
/// confusion [`NewObservation::with_tokens`]'s own doc comment exists to
/// prevent.
pub const CONTEXT_FIREWALL_REDUCTION_PURPOSE: &str = "context-firewall-reduction";

/// [`CONTEXT_FIREWALL_REDUCTION_PURPOSE`]'s other outcome: one row per
/// eligible result the firewall passed through unreduced, `route` carrying
/// `firewall::BypassReason::as_str`'s word so a later reader can
/// count bypasses by reason.
pub const CONTEXT_FIREWALL_BYPASS_PURPOSE: &str = "context-firewall-bypass";

/// Map line 1988: one row per `context-firewall show` call — a raw-result
/// expansion request, the primary recall signal design-decisions.md's
/// Phase 57 section names. Not one of the packet's original two constants;
/// added beside them because line 1988 is its own box and an expansion is
/// neither a reduction nor a bypass — folding it into either purpose would
/// make expansion volume unreadable from reduction volume. `route` carries
/// `"found"` or `"not-found"`; `quota_context` carries the stored entry's
/// tool name when the id resolved, `None` otherwise.
pub const CONTEXT_FIREWALL_EXPANSION_PURPOSE: &str = "context-firewall-expansion";

/// How far back `EvidenceLedger::classification_record` and the routing
/// economics readers look — seven days, the same window the shell's
/// route-evidence view already uses, so a routing model's record and the
/// route table beside it agree on what "recent" means.
pub const CLASSIFICATION_EVIDENCE_WINDOW_SECONDS: i64 = 7 * 24 * 60 * 60;

/// What a row written by `main.rs`'s failover-prevention sink says: a
/// gateway failover was steered off a route whose failures **correlate**
/// with the failed backend's — capability map line 1852's measurement, one
/// row per steered failover, counted back by purpose and never as an
/// exchange.
///
/// Spelled here beside [`CLASSIFICATION_PURPOSE`] for the reason
/// [`ROUTING_LATENCY_PURPOSE`] gives: one producer, one reader, one word.
///
/// **A row under this purpose is not an exchange and not a model call.** It
/// carries no outcome, no failure class and no tokens, so every reader keyed
/// on `outcome` ([`FailureClassCounts::record`]) ignores it by construction,
/// `RoutingOverhead::from_consumption` skips it by name, and
/// `correlate_routes` — which would otherwise read its own consequence
/// back as evidence — excludes it explicitly.
pub const CORRELATION_PURPOSE: &str = "route-correlation";

/// A turn Glasshouse relayed on a harness's behalf, as opposed to the
/// calls Glasshouse makes for its own bookkeeping. See the other
/// `*_PURPOSE` constants in this module: each names why *Glasshouse*
/// called a provider; this one names the case where it did not.
pub const HARNESS_TURN_PURPOSE: &str = "harness-turn";

/// A client-relayed look a supervisor made on its own behalf, not the task
/// it is watching — sterna's `supervisor.md` §3.
pub const SUPERVISOR_PURPOSE: &str = "supervisor";

/// A bounded side request made by Sterna's configured helper tier.
pub const HELPER_PURPOSE: &str = "helper";

/// A typed question Sterna asked a decision model about the task, not the task
/// itself — the purpose a `POST /v1/systemone` request names
/// (`archive/glasshouse:docs/product/evidence/phase-66.md`, *Provider facts*).
pub const DECISION_PURPOSE: &str = "decision";

/// A client may name a purpose only from this list; everything else is
/// [`HARNESS_TURN_PURPOSE`]. The gateway strips the `x-glasshouse-purpose`
/// request header before forwarding regardless of whether its value
/// appears here, so an unrecognised name never reaches a provider and
/// never reaches the ledger either.
pub const CLIENT_NAMEABLE_PURPOSES: &[&str] =
    &[SUPERVISOR_PURPOSE, HELPER_PURPOSE, DECISION_PURPOSE];

/// How far apart two exchanges' windows may sit and still be *the same
/// moment* for `correlate_routes` — capability map line 1370's
/// "temporally overlapping", with the tolerance named rather than assumed.
///
/// Sixty seconds. The overlap this reader most needs to see is the one a
/// failover produces on its own: the failed backend's exchange ends, and the
/// route it failed over to starts its first exchange seconds later. Those
/// two windows never literally intersect — one ends before the other begins
/// — and a tolerance of zero would make every failover's most informative
/// pair of rows invisible. A minute covers that gap with room for a slow
/// client; an hour would fold two separate incidents into one. The
/// conservative error is to see *fewer* overlaps: a missed overlap leaves a
/// pair at [`super::CorrelationVerdict::InsufficientEvidence`] — no correlation,
/// line 1378's safe side — while an invented one penalises a route that did
/// nothing wrong.
pub const CORRELATION_OVERLAP_TOLERANCE_SECONDS: i64 = 60;

/// How many informative failure events a pair of routes needs before
/// [`super::RouteCorrelation::verdict`] reports a confidence at all — line 1376's
/// "sufficient overlapping observations."
///
/// The same five as [`MIN_SAMPLE_FOR_SUMMARY`], on purpose: this ledger has
/// one answer to "how many observations before a figure is trusted", and a
/// second number here would make a correlation trustworthy at a count a
/// failure rate computed from the same rows is not.
pub const MIN_CORRELATION_SAMPLE: usize = MIN_SAMPLE_FOR_SUMMARY;

/// The fraction of task spend above which `RoutingOverhead::exceeds` says
/// so — capability map line 1466's *"non-trivial fraction of the resources
/// it is intended to save"*.
///
/// One in ten. A classifier exists to keep premium capacity for the work
/// that needs it; once it is spending a tenth as many tokens as that work
/// itself, the most it could possibly save is of the same order as what it
/// costs, and a person should be told to look at it.
pub const ROUTING_OVERHEAD_WARNING_FRACTION: f64 = 0.10;

/// Whether the response the harness ultimately saw succeeded, from this
/// producer's point of view — capability map line 1334's "final user-visible
/// outcome," with the honest caveat this module's own doc comment gives:
/// **a gateway exchange only ever supplies a transport-level proxy for this,
/// never the harness's actual verdict.**
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Succeeded,
    Failed,
    Cancelled,
    Unknown,
}

impl Outcome {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
            Self::Unknown => "unknown",
        }
    }

    pub fn from_stored(value: &str) -> Option<Self> {
        match value {
            "succeeded" => Some(Self::Succeeded),
            "failed" => Some(Self::Failed),
            "cancelled" => Some(Self::Cancelled),
            "unknown" => Some(Self::Unknown),
            _ => None,
        }
    }
}

/// Whether the context this turn ran in was known to be warm, known to be
/// cold, or not established either way — capability map line 1337: *"do not
/// average away cache effects."*
///
/// `Unknown` is a real, storable answer rather than the absence of a row —
/// the schema's `context_state` column is `NOT NULL DEFAULT 'unknown'` for
/// exactly this reason, and this type has no fourth, "not recorded" state to
/// keep that true by construction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ContextState {
    Warm,
    Cold,
    #[default]
    Unknown,
}

impl ContextState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Warm => "warm",
            Self::Cold => "cold",
            Self::Unknown => "unknown",
        }
    }

    pub fn from_stored(value: &str) -> Option<Self> {
        match value {
            "warm" => Some(Self::Warm),
            "cold" => Some(Self::Cold),
            "unknown" => Some(Self::Unknown),
            _ => None,
        }
    }
}

/// The four-word effort ladder a translated exchange's row records —
/// migration 24's `routing_observations.effort_level`. The *stored*
/// vocabulary: this module may not depend on `crate::gateway`, so it is
/// declared here rather than reusing `crate::gateway::translate::canonical`'s
/// *wire* `EffortLevel`, and pinned against it exhaustively by that module's
/// own `every_wire_effort_level_stores_and_reads_back_as_the_same_word` test.
///
/// [`Self::from_stored`] answers [`None`] for a word this build does not
/// know, kept as `None` rather than an error — migration 24's own doc
/// comment has the reason.
// History: design-decisions.md, "Trims: routing/evidence/mod.rs", `EffortLevel` doc.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum EffortLevel {
    Minimal,
    Low,
    Medium,
    High,
    Xhigh,
    Max,
}

impl EffortLevel {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Minimal => "minimal",
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
            Self::Xhigh => "xhigh",
            Self::Max => "max",
        }
    }

    pub fn from_stored(value: &str) -> Option<Self> {
        match value {
            "minimal" => Some(Self::Minimal),
            "low" => Some(Self::Low),
            "medium" => Some(Self::Medium),
            "high" => Some(Self::High),
            "xhigh" => Some(Self::Xhigh),
            "max" => Some(Self::Max),
            _ => None,
        }
    }
}

/// What shape the turn a translated exchange carried was — migration 24's
/// `routing_observations.turn_shape`.
///
/// Two words, and unlike [`EffortLevel`] there is no second vocabulary
/// anywhere for this to drift from: no wire spells a turn shape, and
/// [`crate::gateway::translate::canonical::Request::turn_shape`] derives it
/// from the decoded request alone. So it is declared once, here, where the
/// column it is stored in lives.
///
/// [`Self::from_stored`] answers [`None`] for an unrecognised word, on
/// [`EffortLevel`]'s reasoning.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TurnShape {
    /// The last user message carried nothing but tool results: the harness
    /// is handing back what a tool returned, not writing a new prompt.
    ToolResume,
    /// Everything else, a turn with no user message at all included.
    Prompt,
}

impl TurnShape {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ToolResume => "tool-resume",
            Self::Prompt => "prompt",
        }
    }

    pub fn from_stored(value: &str) -> Option<Self> {
        match value {
            "tool-resume" => Some(Self::ToolResume),
            "prompt" => Some(Self::Prompt),
            _ => None,
        }
    }
}

/// How well a stored [`ObservedCost::micro_usd`] is actually known —
/// capability map line 1333's "explicit confidence label," made unforgeable
/// by migration 11's own `CHECK` pairing the two columns.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CostConfidence {
    Exact,
    Estimated,
    Unknown,
}

impl CostConfidence {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Exact => "exact",
            Self::Estimated => "estimated",
            Self::Unknown => "unknown",
        }
    }

    pub fn from_stored(value: &str) -> Option<Self> {
        match value {
            "exact" => Some(Self::Exact),
            "estimated" => Some(Self::Estimated),
            "unknown" => Some(Self::Unknown),
            _ => None,
        }
    }
}

/// A monetary reading with its required confidence label attached — the two
/// always travel together, so there is no way to construct one without the
/// other. See migration 11's `CHECK (cost_micro_usd IS NULL OR
/// cost_confidence IS NOT NULL)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ObservedCost {
    pub micro_usd: i64,
    pub confidence: CostConfidence,
}

/// What kind of failure one exchange was, judged from the status line, the
/// headers, byte counts and timing alone — capability map line 1364's
/// vocabulary, and lines 1316 and 1365's separation: a rate-limit response is
/// counted apart from a transport or model failure, and cadence throttling
/// apart from a spent long-window quota.
///
/// `None` on a [`RoutingObservation`] means the exchange completed and no
/// failure was seen — a served turn — **or** that the row was written before
/// `routing_observations.failure_class` existed (`crate::database` migration
/// 18). The two are not told apart, exactly as every other nullable column on
/// this row treats a pre-migration `NULL`; [`FailureClassCounts`] keeps such
/// rows out of *served* by reading [`Outcome`] beside this.
///
/// No rule deciding a value reads a byte of the body — see
/// `crate::gateway::session`'s `failure_class`, beside `classify`, and
/// `archive/glasshouse:docs/product/design-decisions.md`'s *"Phase 33: framing is not content"*.
// History: design-decisions.md, "Trims: routing/evidence/mod.rs", `FailureClass` doc.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FailureClass {
    /// `429` from a per-window cadence limit: the provider asked for a pause
    /// and its own headers say the window reopens soon, or say nothing.
    Throttle,
    /// The account or the long window is spent: `402`, or a `429` whose
    /// headers say nothing remains until a reset far enough out to be a quota
    /// rather than a cadence — see `crate::gateway::session`'s
    /// `EXHAUSTED_QUOTA_HORIZON_SECONDS`.
    ExhaustedQuota,
    /// The provider answered `5xx`.
    Upstream5xx,
    /// The provider did not answer in time.
    Timeout,
    /// The provider answered, and then its response stream ended before its
    /// own framing said it would — short of a declared length, or before the
    /// terminating chunk.
    StreamAbort,
    /// The provider answered a success status and a body was permitted, and
    /// zero bytes of one arrived.
    EmptyCompletion,
    /// `401` or `403`: the credential, not the provider.
    CredentialFailure,
    /// Any other `4xx`: the request, not the provider.
    RequestIncompatibility,
    /// The provider could not be reached, for a reason this vocabulary does
    /// not name — a refused connection, an unresolvable host, a TLS failure.
    Unknown,
}

impl FailureClass {
    /// Every class, in the order capability map line 1364 lists them.
    pub const ALL: [FailureClass; 9] = [
        Self::Throttle,
        Self::ExhaustedQuota,
        Self::Upstream5xx,
        Self::Timeout,
        Self::StreamAbort,
        Self::EmptyCompletion,
        Self::CredentialFailure,
        Self::RequestIncompatibility,
        Self::Unknown,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Throttle => "throttle",
            Self::ExhaustedQuota => "exhausted_quota",
            Self::Upstream5xx => "upstream_5xx",
            Self::Timeout => "timeout",
            Self::StreamAbort => "stream_abort",
            Self::EmptyCompletion => "empty_completion",
            Self::CredentialFailure => "credential_failure",
            Self::RequestIncompatibility => "request_incompatibility",
            Self::Unknown => "unknown",
        }
    }

    pub fn from_stored(value: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|class| class.as_str() == value)
    }

    /// Whether this class says something about the **provider's health**, as
    /// distinct from its cadence limit, its account's quota, the credential,
    /// or the request — capability map line 1365's third figure.
    ///
    /// A throttle and a spent quota are pacing facts about a window; a
    /// credential failure is about a key; a request incompatibility is about
    /// what the harness sent. None of those says the provider is unwell.
    /// Everything else does: it answered `5xx`, took too long, cut its own
    /// stream, produced nothing, or could not be reached at all.
    pub fn is_provider_health(self) -> bool {
        match self {
            Self::Upstream5xx
            | Self::Timeout
            | Self::StreamAbort
            | Self::EmptyCompletion
            | Self::Unknown => true,
            Self::Throttle
            | Self::ExhaustedQuota
            | Self::CredentialFailure
            | Self::RequestIncompatibility => false,
        }
    }

    /// Whether a failure of this class on one route says anything about
    /// another route failing at the same moment — the classes
    /// `correlate_routes` matches on, capability map line 1373.
    ///
    /// Two and only two. An `Upstream5xx` is the provider's own
    /// infrastructure answering that it is broken, and two front doors
    /// answering so together is the strongest signal this ledger holds that
    /// they are one door. A `Throttle` is a limiter firing, and two limiters
    /// firing together is the "matching serving behaviour" the line names.
    /// Everything else is about the credential, the request, or a transport
    /// this build cannot attribute to either side — a `CredentialFailure` on
    /// two routes at once is two bad keys, not one shared upstream.
    pub fn is_correlatable(self) -> bool {
        matches!(self, Self::Upstream5xx | Self::Throttle)
    }

    fn index(self) -> usize {
        Self::ALL
            .iter()
            .position(|class| *class == self)
            .expect("every class is in ALL")
    }
}

impl std::fmt::Display for FailureClass {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.pad(self.as_str())
    }
}

/// How many exchanges in one window fell into each [`FailureClass`], beside
/// the denominator they are out of — capability map line 1316's count of
/// rate-limit responses *separately from* transport or model failures, and
/// line 1365's three figures, which this type refuses to add together: there
/// is no `failures()` total here on purpose.
///
/// Counts, not rates, so unlike `RoutingSummary`'s aggregates they are not
/// withheld below [`MIN_SAMPLE_FOR_SUMMARY`]: two throttles out of two
/// exchanges is a true statement about two exchanges, and it is the
/// denominator printed beside it that keeps a reader from mistaking it for a
/// rate.
// History: design-decisions.md, "Trims: routing/evidence/mod.rs", `FailureClassCounts` doc.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct FailureClassCounts {
    served: usize,
    unclassified: usize,
    by_class: [usize; FailureClass::ALL.len()],
}

impl FailureClassCounts {
    /// Fold one row in — see the type's own doc for which rows count.
    pub fn record(&mut self, outcome: Option<Outcome>, class: Option<FailureClass>) {
        match (outcome, class) {
            (None, _) => {}
            (Some(_), Some(class)) => self.by_class[class.index()] += 1,
            (Some(Outcome::Succeeded), None) => self.served += 1,
            (Some(_), None) => self.unclassified += 1,
        }
    }

    /// Every exchange these counts are out of — the denominator.
    pub fn observed(&self) -> usize {
        self.served + self.unclassified + self.by_class.iter().sum::<usize>()
    }

    /// Whether anything at all was folded in.
    pub fn is_empty(&self) -> bool {
        self.observed() == 0
    }

    /// Exchanges that completed with no failure seen.
    pub fn served(&self) -> usize {
        self.served
    }

    /// Exchanges that recorded a non-success outcome and no class — see the
    /// type's own doc.
    pub fn unclassified(&self) -> usize {
        self.unclassified
    }

    pub fn count(&self, class: FailureClass) -> usize {
        self.by_class[class.index()]
    }

    /// Line 1365's first figure: temporary cadence throttling.
    pub fn cadence_throttled(&self) -> usize {
        self.count(FailureClass::Throttle)
    }

    /// Line 1365's second figure: an exhausted long-window quota.
    pub fn exhausted_quota(&self) -> usize {
        self.count(FailureClass::ExhaustedQuota)
    }

    /// Line 1365's third figure: the provider itself failing — every class
    /// [`FailureClass::is_provider_health`] says yes to, and none it says no
    /// to.
    pub fn provider_health_failures(&self) -> usize {
        FailureClass::ALL
            .into_iter()
            .filter(|class| class.is_provider_health())
            .map(|class| self.count(class))
            .sum()
    }
}

/// What one producer has to say about one measurable turn — capability map
/// lines 1330 to 1334, before it is stored.
///
/// Every field beyond `provider` and `model` is optional, for the reason this
/// module's own header gives at length: most producers, this round's gateway
/// included, can supply only a subset, and `None` here is what becomes `NULL`
/// in the ledger — "the build that wrote this row recorded nothing here,"
/// never a zero.
#[derive(Debug, Clone, PartialEq)]
pub struct NewObservation {
    pub provider: String,
    pub model: String,
    pub route: Option<String>,
    pub quota_context: Option<String>,
    pub harness: Option<String>,
    pub purpose: Option<String>,

    pub dispatched_at_unix: Option<i64>,
    pub first_byte_at_unix: Option<i64>,
    pub first_token_at_unix: Option<i64>,
    pub first_tool_call_at_unix: Option<i64>,
    pub completed_at_unix: Option<i64>,

    /// Milliseconds from the instant the upstream request was sent to the
    /// first response byte — `crate::database` migration 25, and never an
    /// absolute instant. See [`Self::with_first_byte_ms`].
    pub first_byte_ms: Option<i64>,
    /// [`Self::first_byte_ms`]'s sibling for the first real generated token.
    pub first_token_ms: Option<i64>,
    /// [`Self::first_byte_ms`]'s sibling for the first tool-use block start.
    pub first_tool_call_ms: Option<i64>,
    /// [`Self::first_byte_ms`]'s sibling for the end of the exchange.
    pub completed_ms: Option<i64>,

    pub input_tokens: Option<i64>,
    pub output_tokens: Option<i64>,
    pub cached_input_tokens: Option<i64>,
    pub cost: Option<ObservedCost>,

    pub tool_rounds: Option<i64>,
    pub retries: Option<i64>,
    pub repairs: Option<i64>,
    pub failovers: Option<i64>,
    pub outcome: Option<Outcome>,
    /// What kind of failure this was, when it was one — see [`FailureClass`].
    pub failure_class: Option<FailureClass>,
    /// Which class of work this request was, when the producer classified one
    /// — see [`crate::routing::request::TaskClass`] and `crate::database` migration
    /// 23.
    pub task_class: Option<crate::routing::request::TaskClass>,
    /// The Glasshouse session this exchange belonged to, when the producer
    /// was told one — migration 24. See [`Self::with_session_id`].
    pub session_id: Option<String>,
    /// The effort the request carried, on a translated exchange — migration
    /// 24. See [`Self::with_effort_level`].
    pub effort_level: Option<EffortLevel>,
    /// The shape of the turn the request carried, on a translated exchange
    /// — migration 24. See [`Self::with_turn_shape`].
    pub turn_shape: Option<TurnShape>,

    pub context_state: ContextState,
}

impl NewObservation {
    /// A bare observation naming only what every row must: which provider,
    /// and which model. Everything else starts absent.
    pub fn new(provider: impl Into<String>, model: impl Into<String>) -> Self {
        Self {
            provider: provider.into(),
            model: model.into(),
            route: None,
            quota_context: None,
            harness: None,
            purpose: None,
            dispatched_at_unix: None,
            first_byte_at_unix: None,
            first_token_at_unix: None,
            first_tool_call_at_unix: None,
            completed_at_unix: None,
            first_byte_ms: None,
            first_token_ms: None,
            first_tool_call_ms: None,
            completed_ms: None,
            input_tokens: None,
            output_tokens: None,
            cached_input_tokens: None,
            cost: None,
            tool_rounds: None,
            retries: None,
            repairs: None,
            failovers: None,
            outcome: None,
            failure_class: None,
            task_class: None,
            session_id: None,
            effort_level: None,
            turn_shape: None,
            context_state: ContextState::Unknown,
        }
    }

    pub fn with_route(mut self, route: Option<impl Into<String>>) -> Self {
        self.route = route.map(Into::into);
        self
    }

    pub fn with_quota_context(mut self, quota_context: Option<impl Into<String>>) -> Self {
        self.quota_context = quota_context.map(Into::into);
        self
    }

    pub fn with_harness(mut self, harness: Option<impl Into<String>>) -> Self {
        self.harness = harness.map(Into::into);
        self
    }

    /// What this call was *for* — the axis that separates what routing spent
    /// from what the user's own work spent.
    ///
    /// `routing_observations.purpose` is a `TEXT` column with no `CHECK`, so
    /// the vocabulary is the producers' to agree on rather than the schema's
    /// to enforce; today there is exactly one producer that sets it, `main.rs`'s
    /// `glasshouse classify`, writing `"classification"`.
    ///
    /// # Why the existing producer keeps writing `NULL`
    ///
    /// `memory::extract::ModelCall::observation` does not call this, and must
    /// not be made to: its own doc comment records that every column it could
    /// plausibly fill with a nearby value stays unwritten, and extraction's
    /// rows are already on disk with `purpose` `NULL`. Back-filling them from
    /// a builder added later would make "this build recorded nothing here"
    /// indistinguishable from "this build recorded a purpose", which is the
    /// one thing the nullable columns on this type exist to keep apart.
    pub fn with_purpose(mut self, purpose: Option<impl Into<String>>) -> Self {
        self.purpose = purpose.map(Into::into);
        self
    }

    pub fn with_timing(
        mut self,
        dispatched_at_unix: Option<i64>,
        completed_at_unix: Option<i64>,
    ) -> Self {
        self.dispatched_at_unix = dispatched_at_unix;
        self.completed_at_unix = completed_at_unix;
        self
    }

    /// Line 1331's one timing column [`Self::with_timing`] does not carry:
    /// the instant the first response byte arrived, supplied only by the one
    /// producer that can honestly observe it — the gateway relay, mid-exchange,
    /// before its own body-parsing prohibition would apply. A separate
    /// builder rather than a third parameter on [`Self::with_timing`], so
    /// every other producer's existing two-argument call is untouched by a
    /// column only one producer can ever supply. `None` becomes `NULL`,
    /// exactly like every other absent column on this type.
    pub fn with_first_byte_at(mut self, first_byte_at_unix: Option<i64>) -> Self {
        self.first_byte_at_unix = first_byte_at_unix;
        self
    }

    /// Line 1331's other two timing columns [`Self::with_timing`] does not
    /// carry — the instant the first real generated token passed the seam.
    /// Supplied only by a **translated** exchange, whose seam already decodes
    /// every canonical event in order to re-encode it for the harness; a
    /// relayed exchange never enters a codec and passes [`None`], exactly
    /// like [`Self::with_first_byte_at`]'s own relayed case. See
    /// `archive/glasshouse:docs/product/design-decisions.md`'s *"first real token and first
    /// tool call on the translated path — the 1331/1332 ruling"*.
    pub fn with_first_token_at(mut self, first_token_at_unix: Option<i64>) -> Self {
        self.first_token_at_unix = first_token_at_unix;
        self
    }

    /// [`Self::with_first_token_at`]'s sibling: the instant the first
    /// tool-use block started, under the same rule and the same `None` case.
    pub fn with_first_tool_call_at(mut self, first_tool_call_at_unix: Option<i64>) -> Self {
        self.first_tool_call_at_unix = first_tool_call_at_unix;
        self
    }

    /// Migration 25's first offset: milliseconds from the instant the
    /// upstream request was **sent** to the instant the provider's status
    /// and headers were in hand.
    ///
    /// Not a duration derived from the columns above. Those are unix
    /// seconds, and their zero — `dispatched_at` — is the instant the
    /// gateway handed a connection to `ingress::serve`, which is earlier
    /// than the send by however long reading and rebuilding the request
    /// took. This offset's zero is the send itself, and it is read from a
    /// monotonic `std::time::Instant` rather than from two wall-clock
    /// readings subtracted, so a clock step cannot make it negative. The
    /// column's own `CHECK` refuses a negative value if one ever arrives
    /// anyway. See `archive/glasshouse:docs/product/design-decisions.md`'s *"Millisecond
    /// offsets on the routing row — Cluster G's second column set"*.
    ///
    /// A separate builder rather than a parameter on [`Self::with_timing`],
    /// for exactly [`Self::with_first_byte_at`]'s reason: only the producer
    /// that holds the dispatch `Instant` can supply it, and every other
    /// producer's existing call stays untouched.
    pub fn with_first_byte_ms(mut self, first_byte_ms: Option<i64>) -> Self {
        self.first_byte_ms = first_byte_ms;
        self
    }

    /// [`Self::with_first_byte_ms`]'s sibling for the first real generated
    /// token — supplied only by a **translated** exchange, whose seam
    /// decodes the canonical events, exactly like
    /// [`Self::with_first_token_at`]'s own relayed `None`.
    pub fn with_first_token_ms(mut self, first_token_ms: Option<i64>) -> Self {
        self.first_token_ms = first_token_ms;
        self
    }

    /// [`Self::with_first_token_ms`]'s sibling for the first tool-use block
    /// start.
    pub fn with_first_tool_call_ms(mut self, first_tool_call_ms: Option<i64>) -> Self {
        self.first_tool_call_ms = first_tool_call_ms;
        self
    }

    /// [`Self::with_first_byte_ms`]'s sibling for the end of the exchange —
    /// supplied on both the relayed and the translated path, since both know
    /// when they stopped moving bytes. [`RoutingObservation::duration_ms`]
    /// prefers it over the seconds difference precisely because this one was
    /// measured rather than subtracted.
    pub fn with_completed_ms(mut self, completed_ms: Option<i64>) -> Self {
        self.completed_ms = completed_ms;
        self
    }

    /// The token counts a provider reported for this turn.
    ///
    /// Three `Option`s rather than a struct, matching [`Self::with_timing`]
    /// next door: a producer that read one field and not the others passes
    /// [`None`] for the rest, and [`None`] becomes `NULL` — *this build
    /// recorded nothing here* — exactly as it does for every other optional
    /// column on this type. **A producer that did not read a count must
    /// never pass `Some(0)` for it**: the columns are nullable so that
    /// "unreported" and "zero" stay two different facts, and a consumer
    /// cannot recover the difference once it is lost.
    ///
    /// `cost_micro_usd` is deliberately not part of this. A cost needs
    /// per-model pricing, migration 11 `CHECK`s it against a
    /// `cost_confidence` label for that reason, and tokens are a thing a
    /// provider reports while a price is a thing somebody would have to
    /// supply.
    pub fn with_tokens(
        mut self,
        input_tokens: Option<i64>,
        output_tokens: Option<i64>,
        cached_input_tokens: Option<i64>,
    ) -> Self {
        self.input_tokens = input_tokens;
        self.output_tokens = output_tokens;
        self.cached_input_tokens = cached_input_tokens;
        self
    }

    pub fn with_outcome(mut self, outcome: Outcome) -> Self {
        self.outcome = Some(outcome);
        self
    }

    /// What kind of failure this exchange was — capability map line 1364.
    /// `None` is a served exchange, and it stays `None` rather than becoming
    /// a class that means "nothing": a row with no failure has no kind of
    /// failure to name.
    pub fn with_failure_class(mut self, failure_class: Option<FailureClass>) -> Self {
        self.failure_class = failure_class;
        self
    }

    /// Which class of work this request was — capability map line 1276, and
    /// the missing link between `crate::routing::request::RouterAnswer::task_class`
    /// (which has existed since Phase 34C) and any reader of history.
    ///
    /// `None` is "this producer did not classify", the same honest absence
    /// every other nullable column carries, and it is what every gateway row
    /// carries: the gateway relays a turn and never runs the classifier.
    /// `routing::burn::task_class_request_rates` counts only rows that name
    /// a class, so an absent one lowers no average rather than joining a
    /// bucket it did not earn.
    pub fn with_task_class(
        mut self,
        task_class: Option<crate::routing::request::TaskClass>,
    ) -> Self {
        self.task_class = task_class;
        self
    }

    /// Which Glasshouse session this exchange served — capability map line
    /// 2019's *per-session* clause and migration 24's first column.
    ///
    /// The value is `crate::session::SessionId`'s own string and nothing
    /// else: never the harness's `metadata.user_id`, never a native session
    /// id, never a credential. `archive/glasshouse:docs/product/design-decisions.md`'s *A
    /// session identity on the routing evidence rows* argues each of those
    /// three exclusions.
    ///
    /// `None` is *this producer was never told which session it serves* —
    /// the same honest absence every other nullable column on this type
    /// carries — and it is what a gateway nothing has called
    /// [`crate::gateway::session::SessionRouting::serve_session`] on writes,
    /// never an invented id. `main.rs::record_routing_latency`'s row keeps
    /// it too, deliberately: that row is about a routing decision taken
    /// before any session record existed.
    pub fn with_session_id(mut self, session_id: Option<impl Into<String>>) -> Self {
        self.session_id = session_id.map(Into::into);
        self
    }

    /// The effort the harness asked for on this exchange — migration 24's
    /// second column, and half of what capability map line 2039's shadow
    /// measurement joins.
    ///
    /// A fact of the *request*, read at the one seam that holds a decoded
    /// one (`crate::gateway::translate::serve`). `None` on every relayed
    /// exchange, whose body this gateway never reads, and on a translated
    /// request that asked for no thinking at all — the same absence to this
    /// column, with the row's own `route` telling the two apart.
    pub fn with_effort_level(mut self, effort_level: Option<EffortLevel>) -> Self {
        self.effort_level = effort_level;
        self
    }

    /// Whether this exchange's turn handed back tool results or wrote a new
    /// prompt — migration 24's third column, and the other half of what line
    /// 2039's shadow measurement selects on.
    ///
    /// [`Self::with_effort_level`]'s rule for `None`, for the same reason: a
    /// relayed exchange has no decoded request to derive a shape from.
    pub fn with_turn_shape(mut self, turn_shape: Option<TurnShape>) -> Self {
        self.turn_shape = turn_shape;
        self
    }

    /// How many times this exchange's own outcome moved the session to
    /// another backend — capability map line 1334's `failovers`, the one of
    /// its four counters a gateway exchange can honestly supply, because the
    /// failover it caused is decided in the same connection thread before
    /// its row is written (`crate::gateway::session::SessionRouting::observe_exchange`).
    ///
    /// A `u32` here and an `i64` in the ledger, so a negative count cannot be
    /// built even though the column's `CHECK` would refuse it anyway. `None`
    /// is "this producer did not count", as for every other nullable column.
    pub fn with_failovers(mut self, failovers: Option<u32>) -> Self {
        self.failovers = failovers.map(i64::from);
        self
    }

    /// How many times the request was re-sent in place before this outcome —
    /// line 1334's `retries`. The gateway forwards each request exactly once
    /// (`ureq` 3 has no transparent retry, and `crate::gateway::ingress::forward`
    /// calls `Agent::run` once), so its producer writes `Some(0)`: a count it
    /// took, not a count it declined to take.
    pub fn with_retries(mut self, retries: Option<u32>) -> Self {
        self.retries = retries.map(i64::from);
        self
    }

    /// Line 1334's last two quantities. How many tool-use blocks this
    /// exchange's response requested — the rounds this exchange *began* —
    /// supplied only by a **translated** exchange, whose seam already
    /// decodes the response to re-encode it. `None` is "this producer never
    /// decoded a response to count from," as for every other nullable
    /// column; `Some(0)` is "it counted and found none," which is not the
    /// same fact and must not be confused with it. See
    /// `archive/glasshouse:docs/product/design-decisions.md`'s *"Tool rounds and repairs on the
    /// translated path"*.
    pub fn with_tool_rounds(mut self, tool_rounds: Option<u32>) -> Self {
        self.tool_rounds = tool_rounds.map(i64::from);
        self
    }

    /// [`Self::with_tool_rounds`]'s sibling: how many `is_error: true`
    /// tool-result blocks this exchange's request carried — the harness's
    /// own report that a previous round failed. Supplied only by a
    /// **translated** exchange whose request decoded, under the same
    /// `None`-vs-`Some(0)` rule.
    pub fn with_repairs(mut self, repairs: Option<u32>) -> Self {
        self.repairs = repairs.map(i64::from);
        self
    }

    pub fn with_context_state(mut self, context_state: ContextState) -> Self {
        self.context_state = context_state;
        self
    }

    /// Map line 1307: the estimated cost the routing decision this
    /// observation records actually used, carried in from
    /// `crate::routing::session::Routed::cost` rather than recomputed here.
    /// `None` — the default — means unknown size or unknown price, and this
    /// row then leaves `cost_micro_usd` `NULL` exactly like every other
    /// producer's absent reading, never a fabricated zero.
    pub fn with_cost(mut self, cost: Option<ObservedCost>) -> Self {
        self.cost = cost;
        self
    }
}

/// One observation exactly as it came out of `routing_observations` — the raw
/// row line 1335 requires to stay available beside any aggregate computed
/// from it.
#[derive(Debug, Clone, PartialEq)]
pub struct RoutingObservation {
    pub seq: i64,
    pub project_id: String,
    pub observed_at_unix: i64,

    pub provider: String,
    pub model: String,
    pub route: Option<String>,
    pub quota_context: Option<String>,
    pub harness: Option<String>,
    pub purpose: Option<String>,

    pub dispatched_at_unix: Option<i64>,
    pub first_byte_at_unix: Option<i64>,
    pub first_token_at_unix: Option<i64>,
    pub first_tool_call_at_unix: Option<i64>,
    pub completed_at_unix: Option<i64>,

    /// Milliseconds from the send to the first response byte — migration 25,
    /// and `None` for every row written before it as well as for every
    /// exchange whose request never left. See
    /// [`NewObservation::with_first_byte_ms`] for why this is an offset from
    /// the send rather than from `dispatched_at`.
    pub first_byte_ms: Option<i64>,
    /// [`Self::first_byte_ms`]'s sibling for the first real generated token
    /// — additionally `None` on every relayed exchange.
    pub first_token_ms: Option<i64>,
    /// [`Self::first_token_ms`]'s sibling for the first tool-use block start.
    pub first_tool_call_ms: Option<i64>,
    /// [`Self::first_byte_ms`]'s sibling for the end of the exchange — the
    /// figure [`Self::duration_ms`] prefers over the seconds difference.
    pub completed_ms: Option<i64>,

    pub input_tokens: Option<i64>,
    pub output_tokens: Option<i64>,
    pub cached_input_tokens: Option<i64>,
    pub cost: Option<ObservedCost>,

    pub tool_rounds: Option<i64>,
    pub retries: Option<i64>,
    pub repairs: Option<i64>,
    pub failovers: Option<i64>,
    pub outcome: Option<Outcome>,
    /// `None` for a served exchange, and for every row written before
    /// migration 18 — see [`FailureClass`].
    pub failure_class: Option<FailureClass>,
    /// `None` for a row whose producer ran no classifier, for every row
    /// written before migration 23, **and** for a row whose stored word this
    /// build does not recognise — see migration 23's own doc comment for why
    /// the third case is not an error the way an unknown `failure_class` is.
    pub task_class: Option<crate::routing::request::TaskClass>,
    /// The Glasshouse session this exchange served, `None` for a row whose
    /// producer was never told one and for every row written before
    /// migration 24 — see [`NewObservation::with_session_id`].
    pub session_id: Option<String>,
    /// `None` for a relayed exchange, for a translated request that asked
    /// for no thinking, for every row written before migration 24, **and**
    /// for a row whose stored word this build does not recognise — see
    /// [`EffortLevel`].
    pub effort_level: Option<EffortLevel>,
    /// [`Self::effort_level`]'s four cases, for [`TurnShape`]'s two words.
    pub turn_shape: Option<TurnShape>,

    pub context_state: ContextState,
}

impl RoutingObservation {
    /// How long this exchange took, in milliseconds — [`Self::completed_ms`]
    /// when the producer measured it, and the second-resolution difference
    /// `completed_at - dispatched_at` otherwise.
    ///
    /// The preference is the point, and it is silent: every consumer of this
    /// method — `EvidenceLedger::classification_record`,
    /// `EvidenceLedger::support_work_latency` and the medians they compute
    /// — improves from a figure that was zero or one second to one that was
    /// actually measured, without any of them changing. A row written before
    /// migration 25, or by a producer holding no dispatch `Instant`, keeps
    /// the fallback and reads exactly as it always did.
    ///
    /// The two are not the same span, and the difference is smaller than the
    /// resolution the fallback has: `completed_ms` is measured from the
    /// instant the upstream request was **sent**, and the fallback from the
    /// instant the connection was handed to the gateway's ingress. See
    /// [`NewObservation::with_first_byte_ms`].
    pub fn duration_ms(&self) -> Option<i64> {
        if let Some(completed_ms) = self.completed_ms {
            return Some(completed_ms);
        }
        let dispatched = self.dispatched_at_unix?;
        let completed = self.completed_at_unix?;
        if completed < dispatched {
            return None;
        }
        (completed - dispatched).checked_mul(1000)
    }

    /// The wall-clock span this exchange occupied, as `(start_unix,
    /// end_unix)` — the same shape [`AggregateReading::window`] and
    /// `ObservedIdentity::window` return — and the interval
    /// `correlate_routes` tests for overlap.
    ///
    /// `dispatched_at` and `completed_at` when the producer recorded them
    /// (the gateway always does); `observed_at` stands in for either end a
    /// producer left absent, so a row that recorded only when it was written
    /// is a point in time rather than no interval at all. An end before its
    /// start — the case [`Self::duration_ms`] answers `None` to — is clamped
    /// to the start rather than producing a negative span.
    pub fn window(&self) -> (i64, i64) {
        let start = self.dispatched_at_unix.unwrap_or(self.observed_at_unix);
        let end = self.completed_at_unix.unwrap_or(self.observed_at_unix);
        (start, end.max(start))
    }
}

/// One aggregate figure computed from raw [`RoutingObservation`] rows —
/// design decision 2: *every aggregate carries source, window, sample size,
/// freshness and confidence, never a bare number.*
///
/// Wraps [`crate::provider::quota::Reading`] rather than reinventing its
/// value/observed-at/source shape — the precedent design decision 2 names —
/// and adds exactly the two things `Reading` does not carry on its own:
/// how many raw rows went into it, and the time span they were drawn from.
#[derive(Debug, Clone, PartialEq)]
pub struct AggregateReading<T> {
    reading: Reading<T>,
    sample_count: usize,
    window_start_unix: i64,
    window_end_unix: i64,
}

impl<T> AggregateReading<T> {
    pub fn new(
        value: T,
        window_start_unix: i64,
        window_end_unix: i64,
        sample_count: usize,
        source: ReadingSource,
    ) -> Self {
        Self {
            reading: Reading::new(value, window_end_unix, source),
            sample_count,
            window_start_unix,
            window_end_unix,
        }
    }

    pub fn value(&self) -> &T {
        self.reading.value()
    }

    pub fn source(&self) -> &ReadingSource {
        self.reading.source()
    }

    /// How many raw observations this figure was computed from — always at
    /// least [`MIN_SAMPLE_FOR_SUMMARY`], because nothing below that count is
    /// ever wrapped in one; see `RoutingSummary`'s own `Option` fields.
    pub fn sample_count(&self) -> usize {
        self.sample_count
    }

    /// The observation window this figure was drawn from, as
    /// `(earliest_unix, latest_unix)`.
    pub fn window(&self) -> (i64, i64) {
        (self.window_start_unix, self.window_end_unix)
    }

    /// Whether this figure has aged past `stale_after_seconds`, measured from
    /// its most recent contributing observation.
    pub fn freshness(&self, now_unix: i64, stale_after_seconds: i64) -> Freshness {
        self.reading.freshness(now_unix, stale_after_seconds)
    }

    /// How much this figure is worth relying on.
    ///
    /// Always [`Confidence::Medium`]: every aggregate this ledger produces is
    /// Glasshouse's own count of its own gateway activity —
    /// [`ReadingSource::LocalObservation`]'s own class, matching
    /// `TelemetryClass::Observed.confidence()` — never the provider's own
    /// word, and never a derived estimate either.
    pub fn confidence(&self) -> Confidence {
        Confidence::Medium
    }
}

// Route-correlation and credential-cost vocabulary: values the ledger
// computes and the scorer reads. The computation stays with the ledger.

/// One route as `correlate_routes` tells routes apart: the `provider` and
/// `model` already on every [`RoutingObservation`] — capability map line
/// 1373's "provider metadata", and nothing fetched from anywhere.
///
/// `model` is part of the identity because line 1373 asks for
/// *model-specific* 5xx events: two providers whose `claude-x` both fail at
/// once may share an upstream for that model and nothing else, and a
/// correlation keyed on provider alone would carry that pair's evidence to
/// models it was never observed on. The ledger's `route` column (the wire
/// protocol) is deliberately **not** part of it: the question is whether two
/// front doors lead to one room, and the protocol spoken at the door does
/// not change what is behind it.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct RouteIdentity {
    pub provider: String,
    pub model: String,
}

impl RouteIdentity {
    pub fn new(provider: impl Into<String>, model: impl Into<String>) -> Self {
        Self {
            provider: provider.into(),
            model: model.into(),
        }
    }
}

/// What [`RouteCorrelation::verdict`] answers — capability map line 1376.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum CorrelationVerdict {
    /// Fewer than [`MIN_CORRELATION_SAMPLE`] informative events — line
    /// 1376's refusal, carrying the count so a reader prints *2 of 5* rather
    /// than *unknown*. **A consumer treats this exactly as no correlation.**
    InsufficientEvidence { sample_size: usize, required: usize },
    /// Enough events to say something, and what they say: the share of them
    /// in which the other route failed the same way at the same moment.
    Measured { confidence: f64, sample_size: usize },
}

/// Every pair of routes `correlate_routes` found anything about, looked
/// up by either ordering of the pair. [`Default`] is the empty set — every
/// pair unmeasured — which is what a caller with no ledger passes.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RouteCorrelations {
    pub pairs: std::collections::BTreeMap<(RouteIdentity, RouteIdentity), RouteCorrelation>,
}

impl RouteCorrelations {
    /// What is known about `a` and `b` failing together — never `None`: a
    /// pair with no rows is [`RouteCorrelation::unmeasured`], so "nothing
    /// observed" and "too little observed" reach a consumer as the same
    /// verdict rather than as two shapes to handle.
    pub fn between(&self, a: &RouteIdentity, b: &RouteIdentity) -> RouteCorrelation {
        let key = if a <= b {
            (a.clone(), b.clone())
        } else {
            (b.clone(), a.clone())
        };
        self.pairs
            .get(&key)
            .cloned()
            .unwrap_or_else(|| RouteCorrelation::unmeasured(key.0, key.1))
    }

    /// Every pair with at least one informative event, in route order.
    pub fn iter(&self) -> impl Iterator<Item = &RouteCorrelation> {
        self.pairs.values()
    }

    pub fn is_empty(&self) -> bool {
        self.pairs.is_empty()
    }
}

/// Stating a correlation, for a test standing in where the ledger would be.
///
/// The counting lives with the ledger — this crate holds the vocabulary and
/// reads the verdict, and has no rows to count. A consumer test therefore has
/// no way to reach a *measured* pair through production code, and hand-rolling
/// the counting rule here would be mirroring a function this crate does not
/// own. It states the answer instead. Test-only, so the shipped crate still
/// offers a caller exactly one way in: [`Default`], the empty set.
#[cfg(test)]
impl RouteCorrelations {
    pub(crate) fn stating(pairs: impl IntoIterator<Item = RouteCorrelation>) -> Self {
        let mut map = std::collections::BTreeMap::new();
        for correlation in pairs {
            let key = {
                let (a, b) = correlation.routes();
                (a.clone(), b.clone())
            };
            map.insert(key, correlation);
        }
        Self { pairs: map }
    }
}

/// [`RouteCorrelations::stating`]'s element, for the same reason.
#[cfg(test)]
impl RouteCorrelation {
    pub(crate) fn stated(a: RouteIdentity, b: RouteIdentity, overlaps: usize, lone: usize) -> Self {
        let mut correlation = Self::unmeasured(a, b);
        correlation.overlaps = overlaps;
        correlation.lone = lone;
        correlation
    }
}

/// Map line 1519's own reader, beside `recent_credential_spend`: what a
/// **provider's own money budget** costs, in the currency it is actually
/// stated in, rather than in tokens.
///
/// # Why this reader may answer in money and `recent_credential_spend` may
/// not
///
/// `recent_credential_spend`'s own doc explains why a *ceiling* is stated
/// in tokens: `routing_observations.cost_micro_usd` has almost no producer,
/// so a reader keyed on that column would answer `None` for nearly every
/// window. This reader does not read that column at all — it multiplies the
/// same token counts by `PriceTable::price_for`, the user's own
/// `pricing.toml`, exactly as `routing::session::expected_marginal_cost`
/// already does to price one decision. A row this table has no price for is
/// not silently zero; see [`CredentialCost::unpriced_rows`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CredentialCost {
    /// The priced rows' cost, summed in micro-USD. `None` exactly when
    /// [`Self::priced_rows`] is `0` — *nothing could be priced*, which is not
    /// the same claim as *nothing was spent*, and a caller may judge a
    /// budget exhausted only against `Some`.
    pub micro_usd: Option<u64>,
    /// How many rows contributed to `micro_usd` — carried a token count
    /// **and** matched a `pricing.toml` entry.
    pub priced_rows: usize,
    /// How many rows carried no token count at all — a relayed exchange, or
    /// one written before token counts existed. Not priced, and not the same
    /// gap as [`Self::unpriced_rows`].
    pub unread_rows: usize,
    /// How many rows carried a token count with no matching `pricing.toml`
    /// entry — `PriceTable::price_for` answered `None`. Not priced, and not
    /// the same gap as [`Self::unread_rows`].
    pub unpriced_rows: usize,
    /// Whether the rows behind `micro_usd` are the named credential's own
    /// spend rather than the provider-wide total — `recent_credential_spend`'s
    /// own narrowing rule, applied verbatim.
    pub account_narrowed: bool,
}

// Subscription-headroom vocabulary: the estimate the ledger derives and the
// entitlement rules read. Deriving it stays with the ledger.

/// Map lines 1244/1245/1246/1250/1251/1254's estimator output: never a bare
/// number.
///
/// # Why a band, never a percentage
///
/// [`crate::provider::quota::Percentage`] already refuses to label an
/// inferred capacity figure as exact (capability map line 1234); this type
/// goes one step further and carries no number at all, because none of its
/// inputs — accepted-request counts, throttle recency, session history — has
/// a natural denominator to divide by. A computed percentage would be a real
/// number glued to an invented scale, exactly what line 1251 forbids for
/// opaque token counts and what this type refuses to make representable for
/// the same reason.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HeadroomBand {
    /// A throttle inside `RECENT_SIGNAL_HORIZON_SECONDS` of `now`, with no
    /// reset imminent to relieve it.
    Exhausted,
    /// A throttle fell inside the window — recently, with a reset close
    /// behind it to soften the reading, or earlier and not repeated since.
    Low,
    /// Neither pressure nor activity was observed. A reset reading with
    /// nothing else behind it lands exactly here: real evidence the account
    /// is quota-bound, and none at all that it is under pressure right now.
    Moderate,
    /// Requests were accepted, or this project's own session history served
    /// this account, and no throttle fell in the window.
    Ample,
}

/// What kind of row `estimate_subscription_headroom` actually had to work
/// with — carried on the returned value so an opaque-limit account (map line
/// 1244: no token budget its provider will ever publish) and an account
/// whose rows happen to carry a token count render differently, without
/// either claiming more than the estimate has.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HeadroomBasis {
    /// No scoped row carried a token count. Accepted-request counts, throttle
    /// recency, reset behavior and session history are exactly what an
    /// opaque-limit account can supply, and this estimator asks nothing more
    /// of it.
    RequestActivity,
    /// At least one scoped row carried a token count. Recorded as a label
    /// only: map line 1251 forbids turning a raw count into a fictitious
    /// exact figure with no stated ceiling to divide it by, and this
    /// estimator does not duplicate the ceiling check
    /// `routing::Entitlement::spend_constraint` already makes — a
    /// carried token count changes this label alone, never the band.
    TokenUsage,
}

/// Map line 1248 — whose reset reading, if any, informed this estimate's
/// "is a reset imminent" term. Kept off [`HeadroomBand`] itself (1250/1251's
/// own rule: no numeric field, no invented precision) and reported here
/// instead, so a consumer can label an inferred reading as what it is rather
/// than letting it render identically to the provider's own stated word.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResetBasis {
    /// No reset behaviour — stated or inferred — entered this estimate.
    Unknown,
    /// The caller's own authoritative reading: the provider's stated word,
    /// read from the gateway-quota cache. Never displaced by a learned
    /// value — see `estimate_subscription_headroom`.
    Stated,
    /// No stated reading existed. Inferred from
    /// `MIN_LEARNED_RESET_RECOVERIES` or more throttle→success recoveries
    /// already in window.
    Learned,
}

/// Map line 1249 — whether the rows behind this estimate reach back far
/// enough to say anything about pressure beyond
/// `RECENT_SIGNAL_HORIZON_SECONDS`, out to
/// `LONG_SIGNAL_HORIZON_SECONDS`. A third state, not a bucket guessed from
/// thin evidence: two rows an hour apart cannot tell a multi-hour window
/// from a monthly one, and the honest answer there is
/// [`Self::Undistinguished`] rather than a guessed [`Self::NoPressure`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LongWindowPressure {
    /// No informative row reached back far enough to say anything about the
    /// longer window — absence of evidence, not evidence of absence.
    Undistinguished,
    /// Coverage reached the long horizon and no throttle fell inside it.
    NoPressure,
    /// A throttle fell inside the long horizon, outside the short one:
    /// pressure the short window alone would miss entirely.
    Present,
}

/// Map line 1245's estimate, in full: a [`HeadroomBand`], the confidence it
/// is worth, what it was built from, and whose reading it is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SubscriptionHeadroomEstimate {
    pub band: HeadroomBand,
    /// Always [`Confidence::Low`] today — every signal behind this estimate
    /// is Glasshouse's own inference over its own recorded activity, never
    /// the provider's stated word. That is [`Confidence::Low`]'s own
    /// definition: *"derived, with no measurement of this quantity behind it
    /// at all."*
    pub confidence: Confidence,
    pub basis: HeadroomBasis,
    /// Map line 1246's keying rule, reused verbatim from
    /// `recent_credential_throttles`: `true` only when every informative
    /// row this estimate drew from named its own account; widened to
    /// provider scope the moment one does not.
    pub account_narrowed: bool,
    /// Map line 1248 — whose reset reading, if any, fed this estimate.
    pub reset_basis: ResetBasis,
    /// Map line 1249 — whether evidence separates short-window pressure
    /// from pressure that persists into the longer horizon.
    pub long_window_pressure: LongWindowPressure,
    /// Map line 1247's reachable half — the instant Glasshouse last detected
    /// a regime change for this provider (a stated ceiling that moved
    /// between two persisted gateway readings), if one has ever been
    /// recorded. `None` means the whole evidence window is still in play:
    /// no change was ever detected, or nothing here has looked for one.
    ///
    /// Always `None` out of `estimate_subscription_headroom` itself, which
    /// knows nothing about regime changes — its only production caller,
    /// `config::ResolvedEntitlement::with_telemetry`, is the one
    /// that floors the rows it passes in by this same instant and then
    /// stamps it here, so a rendered estimate can say which regime it
    /// describes.
    pub since_unix: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RouteCorrelation {
    routes: (RouteIdentity, RouteIdentity),
    pub overlaps: usize,
    pub lone: usize,
}

impl RouteCorrelation {
    /// A pair nothing has been observed about — zero events, which
    /// [`Self::verdict`] reports as insufficient with a count of zero.
    pub fn unmeasured(a: RouteIdentity, b: RouteIdentity) -> Self {
        let routes = if a <= b { (a, b) } else { (b, a) };
        Self {
            routes,
            overlaps: 0,
            lone: 0,
        }
    }

    /// The two routes, in a fixed order so `(a, b)` and `(b, a)` are the
    /// same pair.
    pub fn routes(&self) -> (&RouteIdentity, &RouteIdentity) {
        (&self.routes.0, &self.routes.1)
    }

    /// Failure events the other route failed the same way during.
    pub fn overlaps(&self) -> usize {
        self.overlaps
    }

    /// Failure events the other route was observed during and did not
    /// fail the same way.
    pub fn lone(&self) -> usize {
        self.lone
    }

    /// Every informative failure event — the denominator, and the count
    /// line 1376 requires beside any confidence.
    pub fn sample_size(&self) -> usize {
        self.overlaps + self.lone
    }

    /// Line 1376: a confidence only once [`MIN_CORRELATION_SAMPLE`] events
    /// exist, and otherwise the count that fell short.
    pub fn verdict(&self) -> CorrelationVerdict {
        let sample_size = self.sample_size();
        if sample_size < MIN_CORRELATION_SAMPLE {
            return CorrelationVerdict::InsufficientEvidence {
                sample_size,
                required: MIN_CORRELATION_SAMPLE,
            };
        }
        CorrelationVerdict::Measured {
            confidence: self.overlaps as f64 / sample_size as f64,
            sample_size,
        }
    }

    /// [`Self::verdict`]'s confidence, or `None` below the minimum — the
    /// shape a consumer composes with, where absent contributes nothing.
    pub fn confidence(&self) -> Option<f64> {
        match self.verdict() {
            CorrelationVerdict::Measured { confidence, .. } => Some(confidence),
            CorrelationVerdict::InsufficientEvidence { .. } => None,
        }
    }
}

impl std::fmt::Display for RouteIdentity {
    /// `provider/model` — what every explanation and report prints.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}/{}", self.provider, self.model)
    }
}
