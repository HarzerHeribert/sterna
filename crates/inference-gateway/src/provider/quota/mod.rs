//! Phase 32A: one provider-independent model for how much usable capacity a
//! resource has left — [`CapacityState`].
//!
//! "Unknown" does four different jobs:
//!
//! - [`Capacity::Inapplicable`] — there is no such pool. A caller that sees
//!   this must not go looking for the number elsewhere.
//! - [`Capacity::ProviderOpaque`] — there is such a pool and the provider
//!   publishes no number for it. **This one can never become a measurement.**
//! - [`Capacity::Unmeasured`] — there is such a pool, the provider does
//!   publish it, and nothing has read it yet.
//! - [`Capacity::DelegatedUpstream`] — there is such a pool and it belongs to
//!   whichever upstream this resource is currently bound to, not to this
//!   resource — the Glasshouse gateway, and only the gateway.
//!
//! It reads nothing: no HTTP client, no header parser, no clock and no
//! configuration reader here, and every [`Reading`] this module can build
//! must be handed its value, its observation time and its source by a
//! caller.
// History: design-decisions.md, "Trims: gateway, profile and provider module docs", quota/mod.rs module doc.

use std::collections::BTreeSet;

use crate::provider::registry::{Locality, QuotaModel, ResourceKind};

/// Whether a [`NativeAmount`]'s integer is a count of whole units or of
/// millionths of one.
///
/// Fixed point, never a float, for the reason
/// `config::RouterCostMicroUsd` already gives: a human-editable
/// integer stays exact and policy comparisons stay exact. Tokens and requests
/// come in whole units; money and provider credits are fractional, and
/// millionths is the scale this project already uses for a US dollar.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum UnitScale {
    /// The integer counts whole units — tokens, requests.
    Whole,
    /// The integer counts millionths of one unit — microdollars, and any
    /// provider credit that is not a whole number.
    Millionths,
}

/// A quantity in **the provider's own unit**, kept as the provider stated it.
///
/// Capability map line 1217 asks that provider-native quota units be
/// preserved alongside any normalized percentage. The unit is a field of the
/// amount rather than a fact remembered somewhere else, so there is no
/// operation that can carry the number and drop the unit — including
/// normalization, which refuses outright when two amounts disagree about it
/// (see [`Pool::normalized`]).
///
/// `unit` is the provider's own word, not a Glasshouse enumeration:
/// `"credits"`, `"USD"`, `"tokens"`, `"requests"`, `"input tokens"`. A closed
/// enumeration here would force a translation step at exactly the boundary
/// where the map wants the native unit kept.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeAmount {
    value: i64,
    scale: UnitScale,
    unit: String,
}

impl NativeAmount {
    /// A count of whole native units — `1500` `"tokens"`.
    pub fn whole(value: i64, unit: impl Into<String>) -> Self {
        Self {
            value,
            scale: UnitScale::Whole,
            unit: unit.into(),
        }
    }

    /// A count of millionths of a native unit — `2_500_000` `"USD"` is
    /// two dollars fifty.
    pub fn millionths(value: i64, unit: impl Into<String>) -> Self {
        Self {
            value,
            scale: UnitScale::Millionths,
            unit: unit.into(),
        }
    }

    /// The integer, at whatever [`NativeAmount::scale`] says.
    pub fn value(&self) -> i64 {
        self.value
    }

    pub fn scale(&self) -> UnitScale {
        self.scale
    }

    /// The provider's own name for this unit.
    pub fn unit(&self) -> &str {
        &self.unit
    }

    /// Whether two amounts are stated in the same unit at the same scale.
    ///
    /// Two numbers that do not answer this cannot be divided into a
    /// percentage, and [`Pool::normalized`] refuses rather than guessing a
    /// conversion nobody established.
    pub fn commensurable_with(&self, other: &NativeAmount) -> bool {
        self.scale == other.scale && self.unit == other.unit
    }
}

/// What *kind of claim* a reading is — capability map line 1227.
///
/// [`ReadingSource`] names **where** a number came from; this names **what
/// kind of claim it is** — two axes, not one.
///
/// Line 1227 lists five words and only four of them are classes. Unknown is
/// already [`Capacity`]'s four non-[`Capacity::Measured`] states, whose
/// [`Capacity::reading`] answers `None` so a caller cannot read a number
/// that was never taken. [`Capacity::telemetry_class`] is therefore
/// `Option<TelemetryClass>` and [`Capacity::telemetry_class_str`] renders
/// the fifth word for the `None` case — see [`UNKNOWN_TELEMETRY`].
// History: design-decisions.md, "Trims: provider module docs", quota/mod.rs `TelemetryClass` doc.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum TelemetryClass {
    /// The party that owns the quota said so about itself: a provider's own
    /// response header or usage endpoint, or a harness reporting on its own
    /// first-party account. Capability map line 1228's "authoritative".
    Authoritative,
    /// Glasshouse measured it locally — it counted something it did itself.
    /// True about Glasshouse's own activity and not about the account, which
    /// may be shared with other clients.
    Observed,
    /// Derived from something that is not a statement of this quantity.
    /// Never exact — see [`Percentage`].
    Estimated,
    /// The user told Glasshouse. Exactly as reliable as the user, which is
    /// to say it is a statement of intent about a ceiling and a recollection
    /// about a plan.
    Manual,
}

/// The word rendered where no reading exists — line 1227's fifth term.
///
/// A constant rather than a literal repeated at each surface, because the
/// whole point of the [`Capacity`] states is that "unknown" is one answer a
/// reader can recognise wherever it appears.
pub const UNKNOWN_TELEMETRY: &str = "unknown";

impl TelemetryClass {
    /// A short, stable word for a diagnostic — the four terms of line 1227.
    pub fn as_str(self) -> &'static str {
        match self {
            TelemetryClass::Authoritative => "authoritative",
            TelemetryClass::Observed => "observed",
            TelemetryClass::Estimated => "estimated",
            TelemetryClass::Manual => "manual",
        }
    }

    /// Whether this claim came from the party that owns the quota.
    ///
    /// The predicate capability map line 1228 is about: *prefer authoritative
    /// provider or harness usage telemetry when it is available*. Deliberately
    /// a method rather than an equality check at each call site, so that
    /// "authoritative" has exactly one definition.
    pub fn is_authoritative(self) -> bool {
        matches!(self, TelemetryClass::Authoritative)
    }

    /// Whether a value of this class may ever be presented as exact.
    ///
    /// Only [`TelemetryClass::Authoritative`] may. [`TelemetryClass::Manual`]
    /// may not, and that is not a slight on the user: a plan the user typed
    /// is a recollection about a contract, not a measurement of what is left
    /// in it. [`TelemetryClass::Observed`] may not either — Glasshouse can
    /// only ever have counted its own share of a pool something else may also
    /// be spending.
    pub fn may_be_exact(self) -> bool {
        self.is_authoritative()
    }

    /// Preference order for line 1228, lowest first.
    ///
    /// Authoritative outranks everything. Between the other three, a number
    /// Glasshouse actually counted outranks one the user remembered, which
    /// outranks one Glasshouse inferred — an inference is the only one of the
    /// three with no observation of any kind behind it.
    pub fn rank(self) -> u8 {
        match self {
            TelemetryClass::Authoritative => 0,
            TelemetryClass::Observed => 1,
            TelemetryClass::Manual => 2,
            TelemetryClass::Estimated => 3,
        }
    }
}

/// Where a reading came from.
///
/// Carried so that a number can be argued with later: "the provider said so
/// in a response header" and "the user typed it into `config.toml`" are
/// different kinds of claim, and a router weighing a stale one against a
/// fresh one needs to know which it has.
///
/// Every variant maps to exactly one [`TelemetryClass`] through
/// [`ReadingSource::class`], which is total: there is no way to build a
/// reading whose kind of claim is undecided.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReadingSource {
    /// A response header the provider sent, named verbatim.
    ResponseHeader(String),
    /// A dedicated endpoint the provider serves, named by URL or path.
    ProviderEndpoint(String),
    /// A harness reported it about its own first-party account, named by the
    /// command or interface that was read.
    HarnessReport(String),
    /// The user configured it. The only source for a user-defined monetary
    /// ceiling — capability map line 1203.
    UserConfiguration,
    /// Glasshouse measured it itself, named by what it counted.
    ///
    /// The origin line 1227's "observed" needs and [`ReadingSource`] lacked:
    /// a number nobody told Glasshouse, which it arrived at by counting its
    /// own activity. Distinct from [`ReadingSource::InferredEstimate`],
    /// which counted nothing.
    LocalObservation(String),
    /// Glasshouse derived it from something that was not a statement of this
    /// quantity, named by the derivation.
    ///
    /// The weakest origin, and the one line 1234 exists to keep from being
    /// rendered as fact.
    InferredEstimate(String),
}

impl ReadingSource {
    /// What kind of claim a reading from this origin is — capability map
    /// line 1227.
    ///
    /// **Total, and deliberately so.** Every origin has a class, so a caller
    /// can never hold a reading whose kind of claim is undecided, and adding
    /// an origin later is a compile error here rather than a silent default.
    ///
    /// The three authoritative origins are the ones where the party that owns
    /// the quota is the party speaking: its own response header, its own
    /// usage endpoint, its own harness reporting on its own account. A
    /// harness report is authoritative for exactly that reason and not
    /// because a harness is trustworthy in general — it is reporting on
    /// itself.
    pub fn class(&self) -> TelemetryClass {
        match self {
            ReadingSource::ResponseHeader(_)
            | ReadingSource::ProviderEndpoint(_)
            | ReadingSource::HarnessReport(_) => TelemetryClass::Authoritative,
            ReadingSource::LocalObservation(_) => TelemetryClass::Observed,
            ReadingSource::InferredEstimate(_) => TelemetryClass::Estimated,
            ReadingSource::UserConfiguration => TelemetryClass::Manual,
        }
    }

    /// A short description of this origin, for a diagnostic — capability map
    /// line 1235's "source description".
    ///
    /// # This is Glasshouse's own sentence, not the provider's
    ///
    /// Every variant carries a `String` a caller supplied, and the caller
    /// that supplied it read it off a network response. `design-decisions.md`
    /// records that a provider's error body may quote an account identifier
    /// or a masked tail of the submitted credential, so it "must be treated
    /// as sensitive by default: classified against, and never copied whole
    /// into a log, a diagnostic, a session record, or anything a user might
    /// share." A source description is exactly such a diagnostic.
    ///
    /// The rule this module enforces is therefore on the *producers*: the
    /// only strings that reach here are header **names**, endpoint URLs from
    /// Glasshouse's own configuration, and the command line Glasshouse itself
    /// ran — never a header value, never a response body, never an error
    /// message. See [`crate::provider::telemetry`], where that is asserted at
    /// the boundary rather than hoped for here.
    pub fn describe(&self) -> String {
        match self {
            ReadingSource::ResponseHeader(name) => format!("the `{name}` response header"),
            ReadingSource::ProviderEndpoint(url) => format!("the provider usage endpoint {url}"),
            ReadingSource::HarnessReport(interface) => {
                format!("the harness interface `{interface}`")
            }
            ReadingSource::UserConfiguration => "the user's own configuration".to_owned(),
            ReadingSource::LocalObservation(what) => format!("the gateway's own count of {what}"),
            ReadingSource::InferredEstimate(how) => format!("an estimate derived from {how}"),
        }
    }
}

/// One value that was actually read, with when and where from.
///
/// Telemetry ages, so an observation time is not optional: a credit balance
/// from three weeks ago looks exactly like one from three seconds ago, which
/// is the same failure [`crate::provider::cache::ModelCatalogue::fetched_at`]
/// exists to prevent for a model list. Unix seconds, matching
/// [`crate::provider::cache::now_unix_seconds`] — this module has no clock of
/// its own and never stamps a reading itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reading<T> {
    value: T,
    observed_at_unix: i64,
    source: ReadingSource,
}

impl<T> Reading<T> {
    pub fn new(value: T, observed_at_unix: i64, source: ReadingSource) -> Self {
        Self {
            value,
            observed_at_unix,
            source,
        }
    }

    pub fn value(&self) -> &T {
        &self.value
    }

    pub fn observed_at_unix(&self) -> i64 {
        self.observed_at_unix
    }

    pub fn source(&self) -> &ReadingSource {
        &self.source
    }

    /// What kind of claim this reading is — capability map line 1227,
    /// projected out of its origin so the two cannot disagree.
    pub fn class(&self) -> TelemetryClass {
        self.source.class()
    }

    /// Whether this reading has aged past `stale_after_seconds` as of
    /// `now_unix` — capability map line 1237.
    ///
    /// The age limit is a parameter rather than a constant because the map
    /// asks for a **provider-specific configurable** age, and there is no one
    /// right number: a credit balance moves when somebody pays and a
    /// requests-per-minute ceiling is a contract that changes when a plan
    /// does. See `config::QuotaStaleAfterSeconds`, which is where a
    /// user says so per provider, and `provider::resources`, which
    /// is what passes it in.
    ///
    /// A reading stamped in the future is [`Freshness::Fresh`] with a
    /// negative age rather than an error: clock skew between this machine and
    /// a provider is ordinary, and refusing a number because a remote clock
    /// runs fast would be a worse failure than reporting it.
    pub fn freshness(&self, now_unix: i64, stale_after_seconds: i64) -> Freshness {
        Freshness::of(self.observed_at_unix, now_unix, stale_after_seconds)
    }
}

/// Whether a reading is still current — capability map line 1237.
///
/// Both variants carry the age, because "stale" without a number is a verdict
/// a user cannot check and "fresh" without one hides a reading that is one
/// second from turning.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Freshness {
    /// Younger than the configured age.
    Fresh { age_seconds: i64 },
    /// Older than the configured age. Still a real reading — line 1238 is
    /// explicit that Glasshouse falls back rather than fails — but no longer
    /// one to route on without saying so.
    Stale {
        age_seconds: i64,
        stale_after_seconds: i64,
    },
}

impl Freshness {
    /// How old an observation taken at `observed_at_unix` is as of
    /// `now_unix`, against a limit of `stale_after_seconds`.
    ///
    /// [`Reading::freshness`] is this, applied to a reading's own timestamp.
    /// The bare form exists because [`CapacityState::last_observed_at_unix`]
    /// answers a timestamp rather than a reading — it is the latest of many —
    /// and a caller with a timestamp should not have to invent a reading with
    /// a made-up source in order to ask how old it is.
    pub fn of(observed_at_unix: i64, now_unix: i64, stale_after_seconds: i64) -> Self {
        let age_seconds = now_unix.saturating_sub(observed_at_unix);
        if age_seconds > stale_after_seconds {
            Freshness::Stale {
                age_seconds,
                stale_after_seconds,
            }
        } else {
            Freshness::Fresh { age_seconds }
        }
    }

    pub fn is_stale(self) -> bool {
        matches!(self, Freshness::Stale { .. })
    }

    pub fn age_seconds(self) -> i64 {
        match self {
            Freshness::Fresh { age_seconds } | Freshness::Stale { age_seconds, .. } => age_seconds,
        }
    }

    /// A short, stable phrase for a diagnostic.
    pub fn describe(self) -> String {
        match self {
            Freshness::Fresh { age_seconds } => format!("{age_seconds}s old"),
            Freshness::Stale {
                age_seconds,
                stale_after_seconds,
            } => format!("stale: {age_seconds}s old, limit {stale_after_seconds}s"),
        }
    }
}

/// What is known about one quantity — see the module documentation for why
/// "unknown" is four states rather than one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Capacity<T> {
    /// This resource has no quantity of this kind at all.
    Inapplicable,
    /// It has one, and the provider publishes no number for it. Never
    /// becomes a [`Capacity::Measured`].
    ProviderOpaque,
    /// It has one, the provider publishes it, and nothing has read it yet.
    /// Phase 32B.
    Unmeasured,
    /// It has one, and it belongs to whichever upstream this resource is
    /// currently bound to.
    DelegatedUpstream,
    /// It was read.
    Measured(Reading<T>),
}

impl<T> Capacity<T> {
    /// The reading, if there is one. `None` for all four unknown states —
    /// which is the point: a caller cannot accidentally treat "opaque" as a
    /// zero or "inapplicable" as a full tank.
    pub fn reading(&self) -> Option<&Reading<T>> {
        match self {
            Capacity::Measured(reading) => Some(reading),
            _ => None,
        }
    }

    /// The value, if it was read.
    pub fn value(&self) -> Option<&T> {
        self.reading().map(Reading::value)
    }

    pub fn is_measured(&self) -> bool {
        matches!(self, Capacity::Measured(_))
    }

    /// Whether this quantity could ever be filled in by telemetry.
    ///
    /// False for [`Capacity::Inapplicable`] (there is nothing to read) and
    /// for [`Capacity::ProviderOpaque`] (the provider does not publish it) —
    /// the two states Phase 32B must leave alone. A telemetry pass that
    /// wrote a number into either would be inventing it.
    pub fn is_readable(&self) -> bool {
        matches!(
            self,
            Capacity::Unmeasured | Capacity::DelegatedUpstream | Capacity::Measured(_)
        )
    }

    /// A short, stable word for a diagnostic.
    pub fn as_str(&self) -> &'static str {
        match self {
            Capacity::Inapplicable => "not applicable",
            Capacity::ProviderOpaque => "opaque to the provider",
            Capacity::Unmeasured => "unmeasured",
            Capacity::DelegatedUpstream => "delegated to its assigned upstream",
            Capacity::Measured(_) => "measured",
        }
    }

    /// What kind of claim this quantity is — capability map line 1227.
    ///
    /// `None` for all four unknown states, and that `None` is the fifth term
    /// of the line. A quantity nobody read has no class, because there is no
    /// claim: see [`TelemetryClass`]'s own documentation for why an `Unknown`
    /// variant would have meant constructing a [`Reading`] for a measurement
    /// nobody took.
    pub fn telemetry_class(&self) -> Option<TelemetryClass> {
        self.reading().map(Reading::class)
    }

    /// The line-1227 term for this quantity, including
    /// [`UNKNOWN_TELEMETRY`] when there is no reading.
    ///
    /// This is what capability map line 1240 asks a view to surface, in one
    /// call, so that no surface has to remember to spell the unknown case
    /// itself.
    pub fn telemetry_class_str(&self) -> &'static str {
        self.telemetry_class()
            .map_or(UNKNOWN_TELEMETRY, TelemetryClass::as_str)
    }

    /// Where this quantity came from, in a sentence — capability map
    /// line 1235's "source description", and [`UNKNOWN_TELEMETRY`] when
    /// nothing was read.
    pub fn describe_source(&self) -> String {
        self.reading()
            .map_or_else(|| UNKNOWN_TELEMETRY.to_owned(), |r| r.source().describe())
    }

    /// Whichever of two candidate readings for the same quantity should be
    /// believed — capability map line 1228.
    ///
    /// **Authoritative telemetry wins when it is available**, which is the
    /// line's whole content, and [`TelemetryClass::rank`] is the one place
    /// that order is defined. Between two readings of the same class the
    /// fresher one wins, because two statements by the same party about the
    /// same quantity differ only by when they were made.
    ///
    /// A [`Capacity::Measured`] always beats any of the four unknown states,
    /// and between two unknowns `self` is kept — the caller's starting state
    /// carries the distinction between "opaque" and "unmeasured" that
    /// [`CapacityState::for_resource`] established, and a merge that
    /// overwrote it would be exactly the accident
    /// [`Capacity::is_readable`] exists to prevent.
    pub fn prefer(self, other: Capacity<T>) -> Capacity<T> {
        match (self.reading().map(Reading::class), other.reading()) {
            (_, None) => self,
            (None, Some(_)) => other,
            (Some(mine), Some(theirs)) => {
                let theirs_class = theirs.class();
                let take_other = theirs_class.rank() < mine.rank()
                    || (theirs_class == mine
                        && theirs.observed_at_unix()
                            > self
                                .reading()
                                .map(Reading::observed_at_unix)
                                .unwrap_or(i64::MIN));
                if take_other { other } else { self }
            }
        }
    }
}

/// One pool that can run down: what it holds when full, and what is left.
///
/// Both halves are separately unknown. A provider that publishes a remaining
/// count and no ceiling is common — `x-ratelimit-remaining` without
/// `x-ratelimit-limit` — and a model that demanded both would have to invent
/// one to record the other.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pool {
    limit: Capacity<NativeAmount>,
    remaining: Capacity<NativeAmount>,
}

impl Pool {
    /// A pool this resource does not have.
    pub fn inapplicable() -> Self {
        Self {
            limit: Capacity::Inapplicable,
            remaining: Capacity::Inapplicable,
        }
    }

    /// A pool the provider keeps but does not publish.
    pub fn opaque() -> Self {
        Self {
            limit: Capacity::ProviderOpaque,
            remaining: Capacity::ProviderOpaque,
        }
    }

    /// A pool the provider publishes and nothing has read — Phase 32B.
    pub fn unmeasured() -> Self {
        Self {
            limit: Capacity::Unmeasured,
            remaining: Capacity::Unmeasured,
        }
    }

    /// A pool belonging to whichever upstream this resource is bound to.
    pub fn delegated() -> Self {
        Self {
            limit: Capacity::DelegatedUpstream,
            remaining: Capacity::DelegatedUpstream,
        }
    }

    pub fn with_limit(mut self, limit: Capacity<NativeAmount>) -> Self {
        self.limit = limit;
        self
    }

    pub fn with_remaining(mut self, remaining: Capacity<NativeAmount>) -> Self {
        self.remaining = remaining;
        self
    }

    pub fn limit(&self) -> &Capacity<NativeAmount> {
        &self.limit
    }

    pub fn remaining(&self) -> &Capacity<NativeAmount> {
        &self.remaining
    }

    /// The percentage of this pool still usable, **carrying the raw readings
    /// it was computed from**.
    ///
    /// Capability map lines 1217 and 1218 together: the normalized score may
    /// exist, and it may not be what is left after the provider's own numbers
    /// were discarded. Returning a bare `u8` would make discarding them the
    /// default; [`NormalizedCapacity`] makes keeping them the only option the
    /// type offers.
    ///
    /// `None` unless both halves were read and both are stated in the same
    /// native unit at the same scale. A remaining count in `"requests"` over
    /// a ceiling in `"tokens"` is not a percentage, and no conversion between
    /// them was ever established.
    pub fn normalized(&self) -> Option<NormalizedCapacity> {
        let remaining = self.remaining.reading()?;
        let limit = self.limit.reading()?;
        if !remaining.value().commensurable_with(limit.value()) {
            return None;
        }
        if limit.value().value() <= 0 {
            return None;
        }
        let ratio = (remaining.value().value().clamp(0, limit.value().value()) as i128 * 100)
            / limit.value().value() as i128;
        Some(NormalizedCapacity {
            percent: ratio as u8,
            remaining: remaining.clone(),
            limit: limit.clone(),
        })
    }
}

/// A normalized capacity score that cannot be separated from its evidence.
///
/// There is no constructor that takes a bare percentage: the only way to get
/// one is [`Pool::normalized`], and it clones both provider-native readings
/// in. That is capability map line 1218 as a shape rather than as a rule
/// somebody has to remember — raw telemetry is not discarded *because* a
/// score was computed, because the score is made of it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NormalizedCapacity {
    percent: u8,
    remaining: Reading<NativeAmount>,
    limit: Reading<NativeAmount>,
}

/// How much a value of class other than [`TelemetryClass::Authoritative`] is
/// worth relying on — capability map line 1235's "confidence value".
///
/// Three bands rather than a number out of a hundred, because a second
/// invented percentage attached to the first is exactly the kind of precision
/// line 1234 is about. There is nothing to calibrate a 0–100 confidence
/// against, and a band is a claim Glasshouse can actually defend.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Confidence {
    /// Nothing but the age of the reading stands between this and fact.
    High,
    /// A real measurement of something adjacent — Glasshouse's own count of
    /// its own activity, or a ceiling the user stated for a plan they hold.
    Medium,
    /// Derived, with no measurement of this quantity behind it at all.
    Low,
}

impl Confidence {
    pub fn as_str(self) -> &'static str {
        match self {
            Confidence::High => "high confidence",
            Confidence::Medium => "medium confidence",
            Confidence::Low => "low confidence",
        }
    }

    /// The lesser of two confidences.
    ///
    /// A figure computed from two readings is worth what its weakest input is
    /// worth, never what its strongest is. Named rather than written as
    /// `a.max(b)` at the call site, because the derived ordering puts
    /// [`Confidence::High`] first and "take the maximum to get the weakest"
    /// is precisely the kind of line that gets inverted in a later edit.
    pub fn weaker(self, other: Confidence) -> Confidence {
        self.max(other)
    }
}

impl TelemetryClass {
    /// How much a value of this class is worth relying on — capability map
    /// line 1235.
    ///
    /// [`TelemetryClass::Manual`] is `Medium` and not `Low`: a plan the user
    /// typed is a statement about a contract they actually hold, which is
    /// more than an inference has behind it and less than a measurement.
    pub fn confidence(self) -> Confidence {
        match self {
            TelemetryClass::Authoritative => Confidence::High,
            TelemetryClass::Observed | TelemetryClass::Manual => Confidence::Medium,
            TelemetryClass::Estimated => Confidence::Low,
        }
    }
}

/// A capacity percentage that **cannot be presented as exact unless it is** —
/// capability map line 1234.
///
/// There is no accessor here that yields a number without also yielding what
/// kind of number it is: [`Percentage::exact`] answers `None` for an
/// estimate, and [`Percentage::estimated`] is the only other way to reach the
/// digits, and it hands back the confidence and the source description with
/// them.
///
/// A first-party subscription's pools are [`Capacity::ProviderOpaque`], so
/// [`Pool::normalized`] answers `None` for one and there is no percentage to
/// mislabel in the first place. This type is the second guard, for the case
/// where a percentage does exist and was computed from something weaker than
/// the provider's own word.
// History: design-decisions.md, "Trims: provider module docs", quota/mod.rs `Percentage` doc.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Percentage {
    /// Every reading behind this number came from the party that owns the
    /// quota. It is what the provider says, divided by what the provider
    /// says.
    Exact(u8),
    /// At least one reading behind this number was observed, remembered or
    /// inferred rather than stated by the provider.
    ///
    /// Carries line 1235's two obligations — a confidence value and a source
    /// description — as fields, so an estimate cannot be constructed without
    /// them.
    Estimated {
        percent: u8,
        confidence: Confidence,
        source: String,
    },
}

impl Percentage {
    /// The number, **only** when it is exact. `None` for an estimate.
    pub fn exact(&self) -> Option<u8> {
        match self {
            Percentage::Exact(percent) => Some(*percent),
            Percentage::Estimated { .. } => None,
        }
    }

    /// The number and its two qualifications, **only** when it is an
    /// estimate. `None` for an exact reading.
    pub fn estimated(&self) -> Option<(u8, Confidence, &str)> {
        match self {
            Percentage::Exact(_) => None,
            Percentage::Estimated {
                percent,
                confidence,
                source,
            } => Some((*percent, *confidence, source.as_str())),
        }
    }

    /// What kind of claim this percentage is.
    pub fn class(&self) -> TelemetryClass {
        match self {
            Percentage::Exact(_) => TelemetryClass::Authoritative,
            Percentage::Estimated { .. } => TelemetryClass::Estimated,
        }
    }

    /// The one way to turn a percentage into text.
    ///
    /// An estimate renders with a `~`, the word `estimated`, its confidence
    /// band and the source it came from; an exact reading renders as the bare
    /// figure. Every surface goes through this rather than formatting the
    /// digits itself, which is what makes line 1234 a property of the code
    /// and not of each view's care.
    pub fn render(&self) -> String {
        match self {
            Percentage::Exact(percent) => format!("{percent}%"),
            Percentage::Estimated {
                percent,
                confidence,
                source,
            } => format!(
                "~{percent}% (estimated, {}, from {source})",
                confidence.as_str()
            ),
        }
    }

    /// The digits alone, for ordering only — private, so no surface can
    /// render a number without its qualification.
    fn number(&self) -> u8 {
        match self {
            Percentage::Exact(percent) => *percent,
            Percentage::Estimated { percent, .. } => *percent,
        }
    }
}

/// Ordered by how much capacity is left, so that
/// [`CapacityState::normalized`] can find the binding pool without any caller
/// unwrapping the digits. An exact reading sorts below an estimate at the
/// same figure: where the two tie, the one Glasshouse can defend is the one
/// to report.
impl Ord for Percentage {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.number()
            .cmp(&other.number())
            .then_with(|| self.class().rank().cmp(&other.class().rank()))
    }
}

impl PartialOrd for Percentage {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl NormalizedCapacity {
    /// Zero to one hundred, **labelled** — capability map line 1234.
    ///
    /// Exact only when both readings this score was computed from came from
    /// the party that owns the quota. Otherwise an estimate carrying the
    /// weaker of the two readings' confidence and a description naming both
    /// origins, so the qualification travels with the figure rather than
    /// being reconstructible from it.
    pub fn percent(&self) -> Percentage {
        let remaining_class = self.remaining.class();
        let limit_class = self.limit.class();
        if remaining_class.may_be_exact() && limit_class.may_be_exact() {
            return Percentage::Exact(self.percent);
        }
        Percentage::Estimated {
            percent: self.percent,
            confidence: remaining_class
                .confidence()
                .weaker(limit_class.confidence()),
            source: format!(
                "{} over {}",
                self.remaining.source().describe(),
                self.limit.source().describe()
            ),
        }
    }

    /// What the provider said is left, in the provider's own unit.
    pub fn remaining(&self) -> &Reading<NativeAmount> {
        &self.remaining
    }

    /// What the provider said the pool holds when full, in the same unit.
    pub fn limit(&self) -> &Reading<NativeAmount> {
        &self.limit
    }

    /// The provider's own name for the unit both readings are stated in.
    pub fn native_unit(&self) -> &str {
        self.remaining.value().unit()
    }
}

/// Token budget, kept as up to four independent pools.
///
/// Capability map line 1205 asks that input-token budget be tracked
/// independently from output-token budget **when the provider exposes
/// separate limits**, which means the model must also be able to say that a
/// provider exposes one combined pool instead — otherwise "independently" is
/// unachievable in the common case and a caller has to guess which of two
/// fields the single number went into.
///
/// So a provider with one pool measures [`TokenBudget::combined`] and leaves
/// input and output [`Capacity::Inapplicable`]; a provider with separate
/// limits does the reverse. [`TokenBudget::cached_input`] is line 1206 and is
/// its own pool either way, because cache telemetry is a separate provider
/// feature from a token limit and is present or absent on its own.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TokenBudget {
    combined: Pool,
    input: Pool,
    output: Pool,
    cached_input: Pool,
}

impl TokenBudget {
    /// Every token pool in the same state — the shape a resource-level
    /// constructor needs, since none of the four is known before telemetry.
    pub fn uniform(state: Pool) -> Self {
        Self {
            combined: state.clone(),
            input: state.clone(),
            output: state.clone(),
            cached_input: state,
        }
    }

    pub fn with_combined(mut self, pool: Pool) -> Self {
        self.combined = pool;
        self
    }

    pub fn with_input(mut self, pool: Pool) -> Self {
        self.input = pool;
        self
    }

    pub fn with_output(mut self, pool: Pool) -> Self {
        self.output = pool;
        self
    }

    pub fn with_cached_input(mut self, pool: Pool) -> Self {
        self.cached_input = pool;
        self
    }

    /// One pool covering input and output together, for a provider that
    /// exposes no separate limits.
    pub fn combined(&self) -> &Pool {
        &self.combined
    }

    pub fn input(&self) -> &Pool {
        &self.input
    }

    pub fn output(&self) -> &Pool {
        &self.output
    }

    /// Cached-input usage — capability map line 1206.
    pub fn cached_input(&self) -> &Pool {
        &self.cached_input
    }
}

/// Whether a window slides or turns.
///
/// Capability map line 1212 asks for rolling-window capacity to be tracked
/// **separately from** fixed calendar-window capacity, which is a statement
/// about them coexisting: a subscription can have a five-hour rolling
/// allowance and a monthly cap at the same time, and a model with one window
/// field would have to pick one and lose the other. [`Windows`] therefore
/// holds one of each rather than a discriminant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WindowShape {
    /// Capacity replenishes continuously across a trailing period.
    Rolling,
    /// Capacity resets at a fixed calendar boundary.
    Calendar,
}

/// One quota window: what it holds, when it started, when it resets.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WindowCapacity {
    shape: WindowShape,
    pool: Pool,
    started_at_unix: Capacity<i64>,
    resets_at_unix: Capacity<i64>,
}

impl WindowCapacity {
    /// A window whose pool and both timestamps are all in the same unknown
    /// state.
    pub fn uniform(shape: WindowShape, pool: Pool, unknown: Capacity<i64>) -> Self {
        Self {
            shape,
            pool,
            started_at_unix: unknown.clone(),
            resets_at_unix: unknown,
        }
    }

    pub fn with_started_at(mut self, started: Capacity<i64>) -> Self {
        self.started_at_unix = started;
        self
    }

    pub fn with_resets_at(mut self, resets: Capacity<i64>) -> Self {
        self.resets_at_unix = resets;
        self
    }

    pub fn with_pool(mut self, pool: Pool) -> Self {
        self.pool = pool;
        self
    }

    pub fn shape(&self) -> WindowShape {
        self.shape
    }

    pub fn pool(&self) -> &Pool {
        &self.pool
    }

    /// When the current window began — capability map line 1210.
    pub fn started_at_unix(&self) -> &Capacity<i64> {
        &self.started_at_unix
    }

    /// When the current window resets — capability map line 1211.
    pub fn resets_at_unix(&self) -> &Capacity<i64> {
        &self.resets_at_unix
    }
}

/// A resource's rolling window and its calendar window, tracked apart.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Windows {
    rolling: WindowCapacity,
    calendar: WindowCapacity,
}

impl Windows {
    /// Both windows in the same unknown state.
    pub fn uniform(pool: Pool, unknown: Capacity<i64>) -> Self {
        Self {
            rolling: WindowCapacity::uniform(WindowShape::Rolling, pool.clone(), unknown.clone()),
            calendar: WindowCapacity::uniform(WindowShape::Calendar, pool, unknown),
        }
    }

    pub fn with_rolling(mut self, window: WindowCapacity) -> Self {
        self.rolling = window;
        self
    }

    pub fn with_calendar(mut self, window: WindowCapacity) -> Self {
        self.calendar = window;
        self
    }

    pub fn rolling(&self) -> &WindowCapacity {
        &self.rolling
    }

    pub fn calendar(&self) -> &WindowCapacity {
        &self.calendar
    }
}

/// A request pool over a window longer than a minute.
///
/// Capability map line 1216 says "requests-per-day **or equivalent**", so the
/// window length is a field rather than a hardcoded day: a provider with a
/// weekly or per-hour pool is describable without a new variant, and a
/// caller reading `1000` never has to assume which period it covers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LongWindowRequests {
    limit: NativeAmount,
    window_seconds: i64,
}

impl LongWindowRequests {
    pub fn new(limit: NativeAmount, window_seconds: i64) -> Self {
        Self {
            limit,
            window_seconds,
        }
    }

    pub fn limit(&self) -> &NativeAmount {
        &self.limit
    }

    pub fn window_seconds(&self) -> i64 {
        self.window_seconds
    }
}

/// Rate ceilings, which bound throughput rather than exhausting.
///
/// Deliberately separate from [`CapacityState`]'s pools: hitting a
/// requests-per-minute ceiling makes a resource unusable for the next few
/// seconds, and running a credit balance to zero makes it unusable until
/// somebody pays. A model that put both in the same field would let a router
/// treat a momentary throttle as an exhausted account.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RateCeilings {
    requests_per_minute: Capacity<NativeAmount>,
    tokens_per_minute: Capacity<NativeAmount>,
    long_window_requests: Capacity<LongWindowRequests>,
    max_concurrent_requests: Capacity<NativeAmount>,
}

impl RateCeilings {
    /// Every ceiling in the same unknown state.
    pub fn uniform(amount: Capacity<NativeAmount>, long: Capacity<LongWindowRequests>) -> Self {
        Self {
            requests_per_minute: amount.clone(),
            tokens_per_minute: amount.clone(),
            long_window_requests: long,
            max_concurrent_requests: amount,
        }
    }

    pub fn with_requests_per_minute(mut self, value: Capacity<NativeAmount>) -> Self {
        self.requests_per_minute = value;
        self
    }

    pub fn with_tokens_per_minute(mut self, value: Capacity<NativeAmount>) -> Self {
        self.tokens_per_minute = value;
        self
    }

    pub fn with_long_window_requests(mut self, value: Capacity<LongWindowRequests>) -> Self {
        self.long_window_requests = value;
        self
    }

    /// Capability map line 1214.
    pub fn requests_per_minute(&self) -> &Capacity<NativeAmount> {
        &self.requests_per_minute
    }

    /// Capability map line 1215.
    pub fn tokens_per_minute(&self) -> &Capacity<NativeAmount> {
        &self.tokens_per_minute
    }

    /// Capability map line 1216.
    pub fn long_window_requests(&self) -> &Capacity<LongWindowRequests> {
        &self.long_window_requests
    }

    /// Capability map line 1213.
    pub fn max_concurrent_requests(&self) -> &Capacity<NativeAmount> {
        &self.max_concurrent_requests
    }
}

/// A unit whose exhaustion makes a resource unusable until it is topped up
/// or its window turns.
///
/// Not a rate ceiling: [`RateCeilings`] bounds how fast a resource may be
/// used, this names what running out of actually stops it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum LimitingUnit {
    /// Tokens — capability map line 1199.
    Tokens,
    /// Requests — capability map line 1200.
    Requests,
    /// Provider credits, which are the actual limiting unit on a metered
    /// account even when tokens are what is counted — capability map lines
    /// 1201 and 1208.
    Credits,
    /// A first-party subscription's allowance, whose limit the provider
    /// defines and does not publish — capability map line 1202.
    OpaqueProviderAllowance,
    /// A spending ceiling the user configured, which binds before the
    /// provider's own quota does — capability map lines 1203 and 1209.
    UserMonetaryBudget,
}

impl LimitingUnit {
    pub fn as_str(self) -> &'static str {
        match self {
            LimitingUnit::Tokens => "tokens",
            LimitingUnit::Requests => "requests",
            LimitingUnit::Credits => "credits",
            LimitingUnit::OpaqueProviderAllowance => "an opaque provider allowance",
            LimitingUnit::UserMonetaryBudget => "a user-configured monetary budget",
        }
    }
}

/// What can exhaust a resource, including the two answers that are not a set
/// of units.
///
/// "Nothing can" and "whatever limits something else" are real answers and
/// are not the empty set: an empty set of limiting units would read as
/// "nothing can exhaust it", which is true of local inference and false of
/// the gateway. Capability map line 1204 asks specifically that effectively
/// unlimited local inference be representable *separately from* remote
/// quota, and [`LimitingUnits::None`] is that separation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LimitingUnits {
    /// Nothing can exhaust this resource — a local inference server.
    None,
    /// Whatever limits the upstream this resource is currently bound to —
    /// the Glasshouse gateway, and only the gateway.
    Delegated,
    /// These units can. Never empty; use [`LimitingUnits::None`] for that.
    These(BTreeSet<LimitingUnit>),
}

impl LimitingUnits {
    /// The units named, or none for [`LimitingUnits::None`] and
    /// [`LimitingUnits::Delegated`] — both of which are answers about the
    /// resource rather than a list of units, and neither of which a caller
    /// may iterate as if it were an empty list.
    pub fn named(&self) -> Option<&BTreeSet<LimitingUnit>> {
        match self {
            LimitingUnits::These(units) => Some(units),
            _ => None,
        }
    }

    pub fn includes(&self, unit: LimitingUnit) -> bool {
        self.named().is_some_and(|units| units.contains(&unit))
    }

    /// Add `unit` to the set of things that can exhaust this resource, now
    /// that a reading has actually evidenced it — capability map lines 1199
    /// and 1200.
    ///
    /// A no-op for [`LimitingUnits::None`] and [`LimitingUnits::Delegated`]:
    /// neither is a set a unit can be added to, and both are answers about
    /// the resource itself — "nothing can exhaust this" and "whatever limits
    /// its upstream" — that a telemetry reading does not get to overrule. Only
    /// [`LimitingUnits::These`] grows.
    pub fn with_evidenced(self, unit: LimitingUnit) -> Self {
        match self {
            LimitingUnits::These(mut units) => {
                units.insert(unit);
                LimitingUnits::These(units)
            }
            other => other,
        }
    }
}

/// A provider- or harness-defined plan name — capability map line 1233.
///
/// A plan is a [`Capacity`] like every other quantity here, and which of a
/// user's own entry or the harness's own report supplied it is a
/// [`ReadingSource`] rather than two separate fields. [`Capacity::prefer`]
/// then does line 1228's work for free: a harness that reports its own plan
/// overrides one the user typed, because the harness is authoritative about
/// its own account and the user is remembering.
///
/// It is **not** a capacity. Knowing an account is on `max` says nothing
/// about how much of this window is left, and this type carries no number
/// for exactly that reason.
// History: design-decisions.md, "Trims: provider module docs", quota/mod.rs `KnownPlan` doc.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KnownPlan {
    name: String,
}

impl KnownPlan {
    pub fn new(name: impl Into<String>) -> Self {
        Self { name: name.into() }
    }

    /// The plan's name as the provider or harness spells it — `"max"`,
    /// `"pro"`, `"team"`. Not a Glasshouse enumeration: every vendor names
    /// its own tiers and a closed set here would be wrong within a quarter.
    pub fn name(&self) -> &str {
        &self.name
    }
}

/// How much usable capacity a resource has left — capability map line 1198.
///
/// Provider-independent: nothing here names a provider, a harness or a
/// protocol. A [`CapacityState`] is built for a
/// [`crate::provider::registry::ResourceKind`] by
/// [`CapacityState::for_resource`], and every field it carries is a quantity
/// any provider could in principle expose.
///
/// # What it deliberately is not
///
/// It is not a number, and it has no `percent` field. See
/// [`Pool::normalized`] and [`NormalizedCapacity`] for why the normalized
/// score is derived and carries its own evidence.
///
/// It is also not a routability decision. Whether a resource with eight
/// percent of a credit balance left should be routed to is a policy question
/// with a threshold the user already configures
/// (`config::PremiumReservePercent`); this type answers only what is
/// known about the capacity, and answers `unknown` far more often than not
/// until Phase 32B exists.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapacityState {
    model: QuotaModel,
    locality: Locality,
    limits: LimitingUnits,
    plan: Capacity<KnownPlan>,
    tokens: TokenBudget,
    requests: Pool,
    credits: Pool,
    user_budget: Pool,
    windows: Windows,
    rates: RateCeilings,
}

impl CapacityState {
    /// A first-party subscription: a rolling window whose limits the provider
    /// defines and does not publish — capability map line 1202.
    ///
    /// Every token and request pool is [`Capacity::ProviderOpaque`] rather
    /// than [`Capacity::Unmeasured`], and that is the map's own rule in code:
    /// Glasshouse must never invent exact token balances for opaque
    /// subscriptions, and [`Capacity::is_readable`] answers `false` for these
    /// so a later telemetry pass cannot fill one in by accident.
    ///
    /// The window's *reset time* is [`Capacity::Unmeasured`], not opaque —
    /// harnesses do print when a subscription window turns, so that one is a
    /// number Phase 32B can legitimately read.
    ///
    /// A credit balance is [`Pool::inapplicable`] — a flat-fee subscription
    /// has none — and so is a user monetary budget: line 1203 scopes those to
    /// metered APIs, and a spending ceiling on a subscription would bound
    /// nothing.
    pub fn opaque_subscription() -> Self {
        Self {
            model: QuotaModel::RollingWindowSubscription,
            locality: Locality::Remote,
            limits: LimitingUnits::These(BTreeSet::from([LimitingUnit::OpaqueProviderAllowance])),
            // Unmeasured, not opaque: a subscription is exactly the resource
            // whose *plan* both a harness and a user can state even though
            // the allowance behind it is published nowhere. Capability map
            // lines 1231 and 1233.
            plan: Capacity::Unmeasured,
            tokens: TokenBudget::uniform(Pool::opaque()),
            requests: Pool::opaque(),
            credits: Pool::inapplicable(),
            user_budget: Pool::inapplicable(),
            windows: Windows::uniform(Pool::opaque(), Capacity::Unmeasured),
            rates: RateCeilings::uniform(Capacity::Unmeasured, Capacity::Unmeasured),
        }
    }

    /// A metered account, where credits are what actually runs out —
    /// capability map lines 1201 and 1208.
    ///
    /// Tokens and requests are counted too and both may be published, so both
    /// pools are [`Capacity::Unmeasured`] rather than inapplicable; what is
    /// named as *limiting* is credits, because that is the unit whose
    /// exhaustion stops the account.
    pub fn metered_balance() -> Self {
        Self {
            model: QuotaModel::MeteredBalance,
            locality: Locality::Remote,
            limits: LimitingUnits::These(BTreeSet::from([LimitingUnit::Credits])),
            // A metered account may well be on a named tier, and a user may
            // record one; nothing has read it.
            plan: Capacity::Unmeasured,
            tokens: TokenBudget::uniform(Pool::unmeasured()),
            requests: Pool::unmeasured(),
            credits: Pool::unmeasured(),
            user_budget: Pool::unmeasured(),
            windows: Windows::uniform(Pool::unmeasured(), Capacity::Unmeasured),
            rates: RateCeilings::uniform(Capacity::Unmeasured, Capacity::Unmeasured),
        }
    }

    /// Local inference, which cannot run out — capability map line 1204.
    ///
    /// [`LimitingUnits::None`], and every pool, window and rate ceiling
    /// [`Capacity::Inapplicable`]. Not "unmeasured": there is nothing to
    /// measure, and a telemetry pass that treated an unmeasured local server
    /// as work to do would be looking for a number that does not exist.
    /// [`CapacityState::locality`] is [`Locality::Local`], which is the other
    /// half of the line's "separately from remote quota".
    pub fn unmetered_local() -> Self {
        Self {
            model: QuotaModel::Unmetered,
            locality: Locality::Local,
            limits: LimitingUnits::None,
            // A local server sells no plans.
            plan: Capacity::Inapplicable,
            tokens: TokenBudget::uniform(Pool::inapplicable()),
            requests: Pool::inapplicable(),
            credits: Pool::inapplicable(),
            user_budget: Pool::inapplicable(),
            windows: Windows::uniform(Pool::inapplicable(), Capacity::Inapplicable),
            rates: RateCeilings::uniform(Capacity::Inapplicable, Capacity::Inapplicable),
        }
    }

    /// The Glasshouse gateway, whose capacity is whichever upstream it is
    /// currently bound to.
    ///
    /// Every quantity is [`Capacity::DelegatedUpstream`]. Answering
    /// [`LimitingUnits::None`] here would be the same mistake Phase 32
    /// refused when it declined to call the gateway `MeteredBalance`: a
    /// gateway bound to a metered upstream can absolutely run out, and one
    /// bound to Ollama cannot, so the honest answer names neither.
    pub fn delegated_to_upstream() -> Self {
        Self {
            model: QuotaModel::DelegatedToUpstream,
            locality: Locality::Local,
            limits: LimitingUnits::Delegated,
            // Whatever plan the bound upstream is on, not the gateway's own.
            plan: Capacity::DelegatedUpstream,
            tokens: TokenBudget::uniform(Pool::delegated()),
            requests: Pool::delegated(),
            credits: Pool::delegated(),
            user_budget: Pool::delegated(),
            windows: Windows::uniform(Pool::delegated(), Capacity::DelegatedUpstream),
            rates: RateCeilings::uniform(Capacity::DelegatedUpstream, Capacity::DelegatedUpstream),
        }
    }

    /// The capacity model for one kind of resource.
    ///
    /// **This is the production entry point.** `ResourceKind::quota` is
    /// implemented as `for_resource(self).model()`, so every direct-provider
    /// and gateway launch — `profile::apply_direct_provider` and
    /// `profile::apply_gateway`, which push the `"resource kind"` mechanism
    /// note Phase 32 wired — computes a [`CapacityState`] and reads its quota
    /// shape out of it. The launch path reads exactly that one projection;
    /// every pool below it is proven only by this module's own tests, which
    /// is recorded in the evidence ledger rather than implied.
    pub fn for_resource(kind: &ResourceKind) -> Self {
        match kind {
            ResourceKind::NativeSubscription { .. } => Self::opaque_subscription(),
            ResourceKind::DirectProvider {
                locality: Locality::Local,
                ..
            } => Self::unmetered_local(),
            ResourceKind::DirectProvider {
                locality: Locality::Remote,
                ..
            } => Self::metered_balance(),
            ResourceKind::GlasshouseGateway => Self::delegated_to_upstream(),
            // A configured entitlement (Phase 56A line 1963) as a bare
            // resource: opaque, with the plan statable and the allowance
            // published nowhere this build looks. Line 1965's resolver
            // (`crate::config::ResolvedEntitlement::with_telemetry`) does
            // not go through this arm — a `ResourceKind::Entitlement`
            // carries only the account's name, so it cannot say which
            // provider's telemetry would apply; the resolver reads the
            // *backing provider's* own shape instead and marks the reading
            // provider-wide. No launch path reaches this arm — no profile
            // resolves to an entitlement resource yet — so nothing
            // user-visible reports a shape for one.
            ResourceKind::Entitlement { .. } => Self::opaque_subscription(),
        }
    }

    /// Name a different set of limiting units — for a resource whose
    /// exhausting unit is established to be something other than its shape's
    /// default, such as a free pool that runs out of requests rather than
    /// credits.
    pub fn limited_by(mut self, limits: LimitingUnits) -> Self {
        self.limits = limits;
        self
    }

    /// Record what plan this resource is on — capability map lines 1231
    /// (a harness said so) and 1233 (the user said so).
    pub fn with_plan(mut self, plan: Capacity<KnownPlan>) -> Self {
        self.plan = plan;
        self
    }

    pub fn with_tokens(mut self, tokens: TokenBudget) -> Self {
        self.tokens = tokens;
        self
    }

    pub fn with_requests(mut self, requests: Pool) -> Self {
        self.requests = requests;
        self
    }

    pub fn with_credits(mut self, credits: Pool) -> Self {
        self.credits = credits;
        self
    }

    pub fn with_user_budget(mut self, budget: Pool) -> Self {
        self.user_budget = budget;
        self
    }

    pub fn with_windows(mut self, windows: Windows) -> Self {
        self.windows = windows;
        self
    }

    pub fn with_rate_ceilings(mut self, rates: RateCeilings) -> Self {
        self.rates = rates;
        self
    }

    /// Which quota shape this resource has — Phase 32's
    /// [`QuotaModel`], projected out of the capacity model rather than
    /// computed beside it, so the two cannot disagree.
    pub fn model(&self) -> QuotaModel {
        self.model
    }

    /// Where this resource's compute runs. Carried here as well as on
    /// [`ResourceKind`] because line 1204's separation is a property of the
    /// capacity — "unlimited" means something different local and remote.
    pub fn locality(&self) -> Locality {
        self.locality
    }

    /// What can exhaust this resource.
    pub fn limiting_units(&self) -> &LimitingUnits {
        &self.limits
    }

    /// What plan this resource is on, if anything has said — capability map
    /// lines 1231 and 1233.
    pub fn plan(&self) -> &Capacity<KnownPlan> {
        &self.plan
    }

    /// Token budget — capability map lines 1205 and 1206.
    pub fn tokens(&self) -> &TokenBudget {
        &self.tokens
    }

    /// Request count, independent of token consumption — capability map
    /// line 1207.
    pub fn requests(&self) -> &Pool {
        &self.requests
    }

    /// Provider credits, independent of raw tokens — capability map
    /// line 1208.
    pub fn credits(&self) -> &Pool {
        &self.credits
    }

    /// The user's own spending ceiling, independent of provider quota —
    /// capability map lines 1203 and 1209.
    ///
    /// Nothing populates this today: no configuration field records a
    /// spending ceiling, so there is no user-defined budget for a caller to
    /// read. See the evidence ledger.
    pub fn user_budget(&self) -> &Pool {
        &self.user_budget
    }

    /// Rolling and calendar windows, tracked apart — capability map lines
    /// 1210, 1211 and 1212.
    pub fn windows(&self) -> &Windows {
        &self.windows
    }

    /// Throughput ceilings — capability map lines 1213 to 1216.
    pub fn rate_ceilings(&self) -> &RateCeilings {
        &self.rates
    }

    /// Every exhaustible pool, in a fixed order, with the unit it belongs to.
    ///
    /// The order is stable so a diagnostic reads the same way twice; the
    /// labels are Glasshouse's own names for the pools, never the provider's
    /// units — those live on the [`NativeAmount`] inside each reading and are
    /// never flattened into this list.
    pub fn pools(&self) -> Vec<(&'static str, &Pool)> {
        vec![
            ("tokens", self.tokens.combined()),
            ("input tokens", self.tokens.input()),
            ("output tokens", self.tokens.output()),
            ("cached input tokens", self.tokens.cached_input()),
            ("requests", &self.requests),
            ("credits", &self.credits),
            ("user budget", &self.user_budget),
            ("rolling window", self.windows.rolling().pool()),
            ("calendar window", self.windows.calendar().pool()),
        ]
    }

    /// When this resource's most recent successful quota observation was
    /// taken — capability map line 1236.
    ///
    /// The **latest** unix second across every reading this state carries,
    /// and `None` when nothing has been read at all, which is the honest
    /// answer for every resource the shipped binary constructs without a
    /// telemetry pass. "Successful" is not a separate flag: a
    /// [`Capacity::Measured`] is the only state that exists *because* an
    /// observation succeeded, and a failed one leaves the quantity in
    /// whichever unknown state it started in — which is also line 1238, since
    /// a failure therefore cannot overwrite a good earlier reading with a
    /// worse one.
    ///
    /// Every pool, both window timestamps, every rate ceiling and the plan
    /// are all considered, because any one of them may be the thing that was
    /// last refreshed.
    pub fn last_observed_at_unix(&self) -> Option<i64> {
        let mut latest: Option<i64> = None;
        let mut consider = |at: Option<i64>| {
            if let Some(at) = at {
                latest = Some(latest.map_or(at, |best: i64| best.max(at)));
            }
        };

        for (_, pool) in self.pools() {
            consider(pool.limit().reading().map(Reading::observed_at_unix));
            consider(pool.remaining().reading().map(Reading::observed_at_unix));
        }
        for window in [self.windows.rolling(), self.windows.calendar()] {
            consider(
                window
                    .started_at_unix()
                    .reading()
                    .map(Reading::observed_at_unix),
            );
            consider(
                window
                    .resets_at_unix()
                    .reading()
                    .map(Reading::observed_at_unix),
            );
        }
        consider(
            self.rates
                .requests_per_minute()
                .reading()
                .map(Reading::observed_at_unix),
        );
        consider(
            self.rates
                .tokens_per_minute()
                .reading()
                .map(Reading::observed_at_unix),
        );
        consider(
            self.rates
                .long_window_requests()
                .reading()
                .map(Reading::observed_at_unix),
        );
        consider(
            self.rates
                .max_concurrent_requests()
                .reading()
                .map(Reading::observed_at_unix),
        );
        consider(self.plan.reading().map(Reading::observed_at_unix));
        latest
    }

    /// The strongest kind of claim anything in this state rests on —
    /// capability map lines 1227 and 1240.
    ///
    /// `None` when nothing has been read, which renders as
    /// [`UNKNOWN_TELEMETRY`]. This is the one-line answer a resource view
    /// leads with; the per-pool classes are still each their own, and
    /// [`CapacityState::pools`] is how a view reaches them.
    pub fn telemetry_class(&self) -> Option<TelemetryClass> {
        let mut best: Option<TelemetryClass> = None;
        let mut consider = |class: Option<TelemetryClass>| {
            if let Some(class) = class {
                best = Some(best.map_or(
                    class,
                    |b: TelemetryClass| {
                        if class.rank() < b.rank() { class } else { b }
                    },
                ));
            }
        };
        for (_, pool) in self.pools() {
            consider(pool.limit().telemetry_class());
            consider(pool.remaining().telemetry_class());
        }
        consider(self.plan.telemetry_class());
        consider(self.rates.requests_per_minute().telemetry_class());
        consider(self.rates.tokens_per_minute().telemetry_class());
        consider(self.rates.long_window_requests().telemetry_class());
        consider(self.rates.max_concurrent_requests().telemetry_class());
        best
    }

    /// The line-1227 term for this resource as a whole, including
    /// [`UNKNOWN_TELEMETRY`].
    pub fn telemetry_class_str(&self) -> &'static str {
        self.telemetry_class()
            .map_or(UNKNOWN_TELEMETRY, TelemetryClass::as_str)
    }

    /// The tightest normalized score across every pool that has one, with the
    /// pool it came from.
    ///
    /// The *binding* pool, not an average: a resource with ninety percent of
    /// its tokens and two percent of its credits left has two percent of
    /// usable capacity, and averaging the two would report a resource that is
    /// about to stop working as comfortable. `None` when no pool was measured
    /// on both halves in one commensurable unit — which is every resource
    /// today, because nothing reads telemetry.
    ///
    /// Every raw reading survives this call untouched: it takes `&self`, and
    /// the value it returns carries its own evidence. Capability map
    /// line 1218.
    pub fn normalized(&self) -> Option<(&'static str, NormalizedCapacity)> {
        self.pools()
            .into_iter()
            .filter_map(|(label, pool)| pool.normalized().map(|score| (label, score)))
            .min_by_key(|(_, score)| score.percent())
    }
}

impl ResourceKind {
    /// This resource's capacity model — capability map line 1198.
    ///
    /// See [`CapacityState::for_resource`], which this delegates to and which
    /// documents what the production launch path actually reads out of it.
    pub fn capacity(&self) -> CapacityState {
        CapacityState::for_resource(self)
    }
}

// ---------------------------------------------------------------------------
// Phase 32D — a normalized remaining-capacity score, and the bands a routing
// policy reads off it.
// ---------------------------------------------------------------------------

/// A normalized remaining-capacity score for one resource — capability map
/// line 1259.
///
/// Beside [`NormalizedCapacity`] and [`Percentage`], not a replacement for
/// either — packet PHASE-32D's own design decision #1. Line 1259 asks for a
/// score "between zero and one", not a redefinition of the existing labelled
/// 0–100 percentage line 1234 already closed, and both survive. There is no
/// constructor that takes a bare `f64`: the only way to build one is
/// [`CapacityState::remaining_capacity_score`], and every value carries the
/// dimension that bound it and the [`Percentage`] it was derived from — the
/// same discipline [`NormalizedCapacity`] already enforces for its own raw
/// readings, applied one layer up so a number can never reach a caller with
/// its reasoning discarded.
#[derive(Debug, Clone, PartialEq)]
pub struct RemainingCapacityScore {
    dimension: &'static str,
    percent: Percentage,
}

impl RemainingCapacityScore {
    /// The digits alone, out of the two public accessors [`Percentage`]
    /// already offers — never [`Percentage::number`], which is private
    /// precisely so nothing outside that type can read a figure without its
    /// qualification. This reassembles the same figure from the qualified
    /// accessors instead of reaching around them.
    fn digits(&self) -> u8 {
        self.percent
            .exact()
            .or_else(|| self.percent.estimated().map(|(percent, _, _)| percent))
            .expect("a Percentage is always Exact or Estimated")
    }

    /// The dimension that bound this score — capability map line 1260's own
    /// requirement that a score name its own constraint rather than average
    /// it away.
    pub fn dimension(&self) -> &'static str {
        self.dimension
    }

    /// The percentage this score was derived from, still fully labelled —
    /// capability map line 1268: native units and the class of claim behind
    /// them are never replaced by this score, only joined by it.
    pub fn percent(&self) -> &Percentage {
        &self.percent
    }

    /// The raw fraction, `0.0..=1.0` — capability map line 1259's own words.
    pub fn fraction(&self) -> f64 {
        f64::from(self.digits()) / 100.0
    }

    /// A conservative fraction for comparing this resource against another —
    /// capability map line 1266, design decision #4.
    ///
    /// An estimate is attenuated **downward** by how weak its confidence is,
    /// so a low-confidence estimate can never outrank a high-confidence
    /// measurement that is actually tighter: a [`Confidence::Low`] estimate
    /// of 90% (`0.90 - 0.30 = 0.60`) still loses to a `High`-confidence
    /// measured 80% (`0.80`). Never inflated — an exact reading passes
    /// through unchanged, and there is no path in this function that adds to
    /// the raw fraction.
    pub fn routing_fraction(&self) -> f64 {
        let penalty = match &self.percent {
            Percentage::Exact(_) => 0.0,
            Percentage::Estimated { confidence, .. } => match confidence {
                Confidence::High => 0.05,
                Confidence::Medium => 0.15,
                Confidence::Low => 0.30,
            },
        };
        (self.fraction() - penalty).max(0.0)
    }

    /// Effective availability once a known quota reset is taken into account
    /// — capability map lines 1264 and 1265, design decision #3.
    ///
    /// **The raw score is never mutated by this.** `effective` is a third,
    /// separately-named number derived from [`RemainingCapacityScore::routing_fraction`]
    /// and how soon the binding pool's window turns. `seconds_until_reset`
    /// of `None` is the identity: effective equals `routing_fraction` exactly.
    ///
    /// A reset within [`RESET_IMMINENT_SECONDS`] is treated as "happening
    /// now": conservation stops mattering and the effective value rises
    /// toward `1.0`. A reset [`RESET_DISTANT_SECONDS`] away or further is
    /// treated exactly like no reset was known, and the value stays at the
    /// routing fraction. Linear between the two, so a resource crossing
    /// either boundary does not jump.
    // History: design-decisions.md, "Trims: provider module docs", quota/mod.rs `effective` doc.
    pub fn effective(&self, seconds_until_reset: Option<i64>) -> f64 {
        let raw = self.routing_fraction();
        let Some(seconds) = seconds_until_reset else {
            return raw;
        };
        let urgency = reset_urgency(seconds);
        raw + (1.0 - raw) * urgency
    }

    /// Which capacity band this score falls in against `thresholds` —
    /// capability map line 1268.
    pub fn band(&self, thresholds: &CapacityBandThresholds) -> CapacityBand {
        thresholds.band_for_percent(self.digits())
    }
}

/// A reset within this many seconds is treated as imminent —
/// [`RemainingCapacityScore::effective`] line 1265.
pub const RESET_IMMINENT_SECONDS: i64 = 300;

/// A reset this many seconds away or further is treated exactly like no
/// reset is known — [`RemainingCapacityScore::effective`] line 1264, and
/// [`evaluate_reserve_spend`]'s own "distant reset" branch.
pub const RESET_DISTANT_SECONDS: i64 = 3_600;

/// How much a known quota reset should currently matter, `0.0` (far away, or
/// no different from unknown) to `1.0` (imminent, or already past).
///
/// A reset already in the past (`seconds_until_reset <= 0`) is the most
/// urgent case, not an error: a window that just turned is the clearest
/// possible reason to stop conserving, and this module has no clock of its
/// own to have detected the turn any sooner.
fn reset_urgency(seconds_until_reset: i64) -> f64 {
    if seconds_until_reset <= RESET_IMMINENT_SECONDS {
        1.0
    } else if seconds_until_reset >= RESET_DISTANT_SECONDS {
        0.0
    } else {
        1.0 - (seconds_until_reset - RESET_IMMINENT_SECONDS) as f64
            / (RESET_DISTANT_SECONDS - RESET_IMMINENT_SECONDS) as f64
    }
}

/// A capacity band a routing policy may read off a [`RemainingCapacityScore`]
/// — capability map line 1268.
///
/// An enum rather than a raw comparison at each call site, so a policy names
/// a band instead of re-deriving one from a threshold it has to remember.
/// `Ord`, with [`CapacityBand::Exhausted`] lowest — the packet's own
/// requirement — so a policy may compare bands directly
/// (`band <= CapacityBand::Reserve`) without a match of its own.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum CapacityBand {
    Exhausted,
    Reserve,
    Tight,
    Healthy,
    Plenty,
}

impl CapacityBand {
    pub fn as_str(self) -> &'static str {
        match self {
            CapacityBand::Exhausted => "exhausted",
            CapacityBand::Reserve => "reserve",
            CapacityBand::Tight => "tight",
            CapacityBand::Healthy => "healthy",
            CapacityBand::Plenty => "plenty",
        }
    }
}

impl std::fmt::Display for CapacityBand {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Where each [`CapacityBand`] boundary sits, as a percent (`0..=100`) —
/// capability map line 1270. Ascending and inclusive-below-the-next: a
/// resource at exactly `reserve_percent` is [`CapacityBand::Reserve`], not
/// [`CapacityBand::Exhausted`].
///
/// # Fail-closed, not sorted
///
/// The only ways to build one are [`CapacityBandThresholds::new`] and
/// [`CapacityBandThresholds::DEFAULT`]; `new` refuses a non-monotonic set
/// outright rather than sorting it into shape, matching design decision #5
/// and how every other quota value in this crate is fail-closed rather than
/// best-effort (see `config::QuotaStaleAfterSeconds` and its
/// siblings). `config` is where a user overrides these; see
/// `crate::config::CapacityBandThresholdsConfig`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CapacityBandThresholds {
    exhausted_percent: u8,
    reserve_percent: u8,
    tight_percent: u8,
    healthy_percent: u8,
}

impl CapacityBandThresholds {
    /// Two percent exhausted, fifteen reserve, thirty-five tight, seventy
    /// healthy. Round numbers with no measurement behind them — this
    /// build has no usage history to calibrate against — chosen so that
    /// the five bands are not degenerate: every one of them is reachable.
    pub const DEFAULT: Self = Self {
        exhausted_percent: 2,
        reserve_percent: 15,
        tight_percent: 35,
        healthy_percent: 70,
    };

    /// Build a set of thresholds, refusing one that is not ascending.
    pub fn new(
        exhausted_percent: u8,
        reserve_percent: u8,
        tight_percent: u8,
        healthy_percent: u8,
    ) -> Result<Self, CapacityBandThresholdsError> {
        if exhausted_percent <= reserve_percent
            && reserve_percent <= tight_percent
            && tight_percent <= healthy_percent
            && healthy_percent <= 100
        {
            Ok(Self {
                exhausted_percent,
                reserve_percent,
                tight_percent,
                healthy_percent,
            })
        } else {
            Err(CapacityBandThresholdsError {
                exhausted_percent,
                reserve_percent,
                tight_percent,
                healthy_percent,
            })
        }
    }

    /// The same thresholds, with the reserve boundary replaced by one
    /// resource's own protected reserve percentage — capability map line
    /// 1288, design decision #6: *"reserve is one band, and Phase 32F is the
    /// policy that reads it… a resource's protected reserve percentage is
    /// where the reserve band begins for that resource."*
    ///
    /// **Not re-validated as a fresh [`CapacityBandThresholds::new`] call,
    /// and deliberately not clamped either.** [`CapacityBandThresholds::band_for_percent`]'s
    /// sequential comparisons are total for any `u8` ordering: a reserve
    /// percentage set above the tight boundary does not panic or invert
    /// anything, it only makes [`CapacityBand::Tight`] unreachable for this
    /// resource — every percentage that would have been Tight is Reserve
    /// instead, which is exactly what a user asking to protect a larger
    /// share of a premium resource's capacity means. There is nothing to
    /// clamp; the earlier version of this method did, and the doc comment
    /// it left behind was solving a problem the total comparison chain
    /// never had.
    pub fn with_resource_reserve(mut self, reserve_percent: u8) -> Self {
        self.reserve_percent = reserve_percent;
        self
    }

    pub fn exhausted_percent(&self) -> u8 {
        self.exhausted_percent
    }

    pub fn reserve_percent(&self) -> u8 {
        self.reserve_percent
    }

    pub fn tight_percent(&self) -> u8 {
        self.tight_percent
    }

    pub fn healthy_percent(&self) -> u8 {
        self.healthy_percent
    }

    /// Which band `percent` falls in.
    pub fn band_for_percent(&self, percent: u8) -> CapacityBand {
        if percent < self.exhausted_percent {
            CapacityBand::Exhausted
        } else if percent < self.reserve_percent {
            CapacityBand::Reserve
        } else if percent < self.tight_percent {
            CapacityBand::Tight
        } else if percent < self.healthy_percent {
            CapacityBand::Healthy
        } else {
            CapacityBand::Plenty
        }
    }
}

impl Default for CapacityBandThresholds {
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// A [`CapacityBandThresholds::new`] call whose four percentages are not
/// ascending — capability map line 1270's own "fail closed" requirement,
/// stated in code rather than sorted around.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error(
    "capacity band thresholds must be ascending and at most 100 \
     (exhausted {exhausted_percent} <= reserve {reserve_percent} <= tight {tight_percent} <= \
     healthy {healthy_percent} <= 100); refusing rather than sorting them"
)]
pub struct CapacityBandThresholdsError {
    pub exhausted_percent: u8,
    pub reserve_percent: u8,
    pub tight_percent: u8,
    pub healthy_percent: u8,
}

impl CapacityState {
    /// Seconds from `now_unix` until the nearer of this resource's two
    /// windows resets, if either has ever been read — the reset half of
    /// capability map lines 1264 and 1265's own antecedent, and what
    /// [`RemainingCapacityScore::effective`] takes as its second argument.
    ///
    /// `None` when neither window's reset is known, which — per the phase
    /// 32B/QUOTA-FOLLOWUP evidence this packet's own hypothesis section
    /// cites — is most resources in this build. A negative result (the
    /// window already turned, by this machine's clock) is returned as-is
    /// rather than clamped to zero: [`RemainingCapacityScore::effective`]'s
    /// own urgency calculation already treats anything non-positive as
    /// maximally imminent, so clamping here would
    /// only discard information a caller could otherwise log.
    pub fn seconds_until_reset(&self, now_unix: i64) -> Option<i64> {
        [
            self.windows.rolling().resets_at_unix(),
            self.windows.calendar().resets_at_unix(),
        ]
        .into_iter()
        .filter_map(|resets| resets.reading().map(|r| *r.value() - now_unix))
        .min()
    }

    /// The normalized remaining-capacity score for this resource —
    /// capability map lines 1259 and 1260.
    ///
    /// A resource with [`LimitingUnits::None`] has nothing to normalize:
    /// every pool is [`Capacity::Inapplicable`] by construction. Line 1267
    /// asks that local inference be treated as high-capacity while still
    /// being able to fall on measured latency or concurrency (design
    /// decision #7), so this returns a fixed high estimate carrying an
    /// explicit "no evidence" note instead of `None` — this build has no
    /// latency or concurrency reader anywhere, so the honest answer is a
    /// score that says it is not backed by a measurement.
    // History: design-decisions.md, "Trims: gateway, profile and provider module docs", quota/mod.rs `remaining_capacity_score` doc.
    pub fn remaining_capacity_score(&self) -> Option<RemainingCapacityScore> {
        if matches!(self.limits, LimitingUnits::None) {
            return Some(RemainingCapacityScore {
                dimension: "local inference (no latency or concurrency evidence)",
                percent: Percentage::Estimated {
                    percent: 100,
                    confidence: Confidence::Medium,
                    source: "local inference has no provider-defined ceiling; no latency or \
                             concurrency reading exists in this build to lower it below full"
                        .to_owned(),
                },
            });
        }

        let mut best = self.normalized();

        if let (Some(remaining), Some(ceiling)) = (
            self.requests.remaining().reading(),
            self.rates.requests_per_minute().reading(),
        ) && remaining.value().commensurable_with(ceiling.value())
        {
            let synthetic = Pool::inapplicable()
                .with_remaining(Capacity::Measured(remaining.clone()))
                .with_limit(Capacity::Measured(ceiling.clone()));
            if let Some(score) = synthetic.normalized() {
                let candidate = ("requests per minute", score);
                best = Some(match best {
                    Some(current) if current.1.percent() <= candidate.1.percent() => current,
                    _ => candidate,
                });
            }
        }

        best.map(|(dimension, score)| RemainingCapacityScore {
            dimension,
            percent: score.percent(),
        })
    }
}

// ---------------------------------------------------------------------------
// Phase 32F — the protected quota reserve, as policy functions rather than a
// scheduler. Capability map lines 1287-1292 and 1294.
// ---------------------------------------------------------------------------

/// One reserve-spend decision — allow or deny, with the reason stated rather
/// than left for a caller to infer from a bare boolean.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReserveDecision {
    Allow { reason: String },
    Deny { reason: String },
}

impl ReserveDecision {
    pub fn is_allowed(&self) -> bool {
        matches!(self, ReserveDecision::Allow { .. })
    }

    pub fn reason(&self) -> &str {
        match self {
            ReserveDecision::Allow { reason } | ReserveDecision::Deny { reason } => reason,
        }
    }
}

/// Everything [`evaluate_reserve_spend`] needs to decide whether spending a
/// resource's protected reserve is justified — capability map lines
/// 1287-1292 and 1294, gathered into one input rather than six positional
/// booleans a call site could transpose.
#[derive(Debug, Clone, Copy)]
pub struct ReserveDecisionInputs {
    /// This resource's current band — see [`RemainingCapacityScore::band`],
    /// built against thresholds that already carry this resource's own
    /// protected reserve percentage via
    /// [`CapacityBandThresholds::with_resource_reserve`] — capability map
    /// line 1287.
    pub band: CapacityBand,
    /// The task's required workload tier —
    /// [`crate::routing::tier::WorkloadTier`], read here rather than
    /// re-invented: a task's capability *requirement* is exactly what Phase
    /// 35 already models, and duplicating it would be two scales for one
    /// question.
    ///
    /// `routing::classify::TaskClassification::hard_capabilities`
    /// must not be plumbed in beside it: a
    /// `routing::classify::HardCapability` names something a
    /// *harness* must be wired for, not something a stronger model makes
    /// more likely to succeed — a signal *not satisfiable by choosing a
    /// stronger model* is not this field's question. The tier is the scale
    /// that varies with demand, and it is the one this field carries.
    // History: design-decisions.md, "Trims: provider module docs", quota/mod.rs `ReserveContext::tier` doc.
    pub tier: crate::routing::tier::WorkloadTier,
    /// Whether a resource outside the reserve band could adequately serve
    /// this task instead — capability map line 1288.
    pub cheaper_adequate_resource_exists: bool,
    /// Whether the user explicitly overrode reserve protection for this task
    /// or session — capability map line 1290.
    ///
    /// A `bool` here and a *scope* at the producer: see
    /// `routing::disposable::ReserveOverride`, which is the only
    /// thing in this build that may set this true, and which is true only
    /// where the session the user named is the session being decided for.
    /// Nothing may set this from a global preference — line 1290 says "for a
    /// specific task or session", and a switch that covered everything would
    /// be the reserve turned off rather than overridden.
    pub user_override: bool,
    /// Seconds until this resource's quota resets, if known — see
    /// [`CapacityState::seconds_until_reset`]. Capability map lines 1291 and
    /// its distant-reset complement, 1292.
    pub seconds_until_reset: Option<i64>,
    /// Whether the task this decision is for has been **declared** almost
    /// complete — capability map line 1294's guard on migration, and line
    /// 1610's on quota-driven migration.
    ///
    /// **Nothing in this build may infer it.** No harness this build
    /// integrates reports task progress, and this field is the *first*
    /// branch [`evaluate_reserve_spend`] takes, outranking every other
    /// signal including the user override: a fabricated value here does not
    /// degrade the policy, it inverts it. `tests/subscription_pressure.rs`'s
    /// source scan holds both construction sites to reading the declaration
    /// rather than writing a literal.
    // History: design-decisions.md, "Trims: gateway, profile and provider module docs", quota/mod.rs `ReserveContext::task_nearly_complete` doc.
    pub task_nearly_complete: bool,
}

/// Whether spending this resource's protected reserve is justified —
/// capability map lines 1288 through 1292 and 1294, as one decision function
/// over the tuple the packet's design decision #8 names, rather than a
/// scheduler. Every branch states its own reason and cites the line it
/// answers, so the reason a caller receives is also the box it closes.
///
/// 1. **Line 1294 first, unconditionally.** A task somebody declared almost
///    complete is never moved "solely because a reserve threshold was
///    crossed" — scoped and expiring at its producer, so this can only ever
///    be true of a session somebody named, recently.
// History: design-decisions.md, "Trims: gateway, profile and provider module docs", quota/mod.rs `evaluate_reserve_spend` doc.
pub fn evaluate_reserve_spend(inputs: ReserveDecisionInputs) -> ReserveDecision {
    use crate::routing::tier::WorkloadTier;

    if inputs.task_nearly_complete {
        return ReserveDecision::Allow {
            reason: "this session's operator declared its current task nearly complete, so the \
                     reserve threshold is not the sole reason to move the work; capability map \
                     line 1294 forbids moving an almost-complete high-value task to another \
                     session solely because a reserve threshold was crossed, and line 1610 \
                     forbids migrating one solely to preserve a small amount of quota"
                .to_owned(),
        };
    }

    if inputs.user_override {
        return ReserveDecision::Allow {
            reason: "the user explicitly overrode reserve protection for this session \
                     (line 1290)"
                .to_owned(),
        };
    }

    if inputs.band > CapacityBand::Reserve {
        return ReserveDecision::Allow {
            reason: format!(
                "the resource is in the {} band, which has not crossed into its protected \
                 reserve",
                inputs.band
            ),
        };
    }

    if let Some(seconds) = inputs.seconds_until_reset {
        if seconds <= RESET_IMMINENT_SECONDS {
            return ReserveDecision::Allow {
                reason: format!(
                    "the quota resets in {seconds}s, within the reserve policy's imminent \
                     window; conserving now buys little (line 1291)"
                ),
            };
        }
        // `< Heavy`, not `!= Heavy`: a threshold against the tier-3 marker,
        // so a Tier 4 (`Frontier`) task — one step above `Heavy` — still
        // reads as "at or above the heavy tier" and is not denied here. An
        // equality left in place would have compared a Tier 4 task unequal
        // to `Heavy` and fallen through to `Deny`, denying the reserve to
        // the strongest work in the system.
        if seconds >= RESET_DISTANT_SECONDS && inputs.tier < WorkloadTier::Heavy {
            return ReserveDecision::Deny {
                reason: format!(
                    "the next reset is {seconds}s away, past the reserve policy's distant \
                     threshold, so only heavy-tier (tier 3) or stronger work may spend the \
                     reserve"
                ),
            };
        }
    }

    // `>= Heavy`, not `== Heavy`, for the same reason as the threshold
    // above: a Tier 4 task must also justify spending the reserve here.
    if inputs.tier >= WorkloadTier::Heavy {
        return ReserveDecision::Allow {
            reason: "the task requires at least the heavy workload tier (tier 3 or higher), \
                     which justifies spending protected reserve (line 1289)"
                .to_owned(),
        };
    }

    if inputs.cheaper_adequate_resource_exists {
        return ReserveDecision::Deny {
            reason: "a cheaper adequate resource exists and this task does not require the \
                     heavy tier, so protected reserve is not spent on it (line 1288)"
                .to_owned(),
        };
    }

    ReserveDecision::Allow {
        reason: "no cheaper adequate resource exists, so spending protected reserve is the \
                 least-bad option available"
            .to_owned(),
    }
}

#[cfg(test)]
mod tests;
