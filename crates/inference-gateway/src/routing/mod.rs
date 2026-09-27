//! Destination vocabulary and the serving policies that use it.
pub mod domain;
pub mod evidence;
pub mod free;
pub mod interactive;
pub mod pairing;
pub mod request;
pub mod tier;
pub mod wire;

use crate::secret::SecretRef;

/// Which credential, by name — never by value.
///
/// A credential is identified by the provider it belongs to **and** the
/// reference it is resolved through, because those two together are what
/// Phase 9I line 538 calls "two separate allowances": two keys for the same
/// router are two entries here, and exhausting one says nothing about the
/// other.
///
/// The provider name is part of the identity rather than a label beside it.
/// Without it, two providers that happened to read the same environment
/// variable would share one allowance, which is the same defect in the
/// opposite direction.
/// Deliberately **not** `Hash` or `Ord`. [`SecretRef`] derives neither, and
/// widening a type in `crate::secret` so that a routing map could be a
/// `HashMap` would be this module reaching into the one module whose surface
/// is kept deliberately narrow. The pools this keys are a handful of entries
/// long — see [`free::FreePool`], which searches a slice and sorts by
/// [`CredentialId::label`] when an order is needed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CredentialId {
    provider: String,
    reference: SecretRef,
}

impl CredentialId {
    pub fn new(provider: impl Into<String>, reference: SecretRef) -> Self {
        Self {
            provider: provider.into(),
            reference,
        }
    }

    pub fn provider(&self) -> &str {
        &self.provider
    }

    pub fn reference(&self) -> &SecretRef {
        &self.reference
    }

    /// A short name for a diagnostic: the provider and the reference's own
    /// name, which is a variable name or a service/account pair.
    ///
    /// Safe to render for exactly the reason [`SecretRef`]'s own
    /// documentation gives — both variants hold names and nothing else.
    pub fn label(&self) -> String {
        match &self.reference {
            SecretRef::Environment { var } => format!("{}/{var}", self.provider),
            SecretRef::OsCredential { service, account } => {
                format!("{}/{service}:{account}", self.provider)
            }
        }
    }
}

/// Whether using a model costs the user anything at the margin.
///
/// Phase 9I line 527 — "mark selected models as free-tier or zero-marginal-cost
/// resources". Two states and no third: "probably free" is not a thing a
/// policy may act on, and a model nobody marked is [`Cost::Metered`], which
/// is the fail-closed direction. A router that guessed a model was free and
/// was wrong spends the user's money.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cost {
    /// Marked free-tier or zero-marginal-cost by the user's own
    /// configuration.
    Free,
    /// Everything else, including anything nobody has marked.
    Metered,
}

impl Cost {
    pub fn is_free(self) -> bool {
        matches!(self, Self::Free)
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Free => "free",
            Self::Metered => "metered",
        }
    }
}

/// What is established about a backend's tool-call behaviour.
///
/// Three states, not two, and the third is why this type exists rather than a
/// `bool`. Phase 9H line 517 forbids failing over to a backend that "cannot
/// preserve the harness's required protocol or tool semantics", and answering
/// that needs "known not to" told apart from "nobody checked" — the same
/// distinction `harness::Declared` draws, narrowed to the one
/// question routing asks. `crate::profile` builds these from the provider's
/// own `Declared<bool>`; this module never sees a `Declared` because it never
/// needs the evidence string, only the verdict.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolSemantics {
    /// Established to carry tool calls on this protocol.
    Verified,
    /// Nobody established it either way. Not a "no".
    Unverified,
    /// Established **not** to carry them.
    KnownAbsent,
}

/// One destination a request could be sent to: a provider, over a protocol,
/// with one credential, at a marginal cost.
///
/// This is deliberately not [`crate::provider::Provider`]. A `Provider` is
/// configuration — several protocols, several credential variables, no notion
/// of which model is in play. A `Backend` is one already-resolved choice, and
/// a routing policy that took the configuration shape would have to make the
/// same narrowing decision at every call site.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Backend {
    provider: String,
    /// `WireProtocol::slug`. A name; this module never parses it back.
    protocol: String,
    model: AssignedModel,
    credential: CredentialId,
    cost: Cost,
    tools: ToolSemantics,
    /// The `Declared` evidence behind [`Backend::tools`]'s `KnownAbsent`
    /// verdict, when it can reach here — see [`Backend::with_tools_evidence`].
    /// `None` by default, for every one of [`Backend::new`]'s existing
    /// callers.
    tools_evidence: Option<&'static str>,
}

impl Backend {
    pub fn new(
        provider: impl Into<String>,
        protocol: impl Into<String>,
        model: AssignedModel,
        credential: CredentialId,
        cost: Cost,
        tools: ToolSemantics,
    ) -> Self {
        Self {
            provider: provider.into(),
            protocol: protocol.into(),
            model,
            credential,
            cost,
            tools,
            tools_evidence: None,
        }
    }

    /// Carry the `Declared` evidence behind [`Backend::tools`]'s verdict —
    /// the tool-semantics half of the 1517/1513 recorded limit
    /// (`archive/glasshouse:docs/product/evidence/phase-35a.md`). Inert unless `tools() ==
    /// ToolSemantics::KnownAbsent`; the caller supplies `None` for every
    /// other verdict.
    #[must_use]
    pub fn with_tools_evidence(mut self, evidence: Option<&'static str>) -> Self {
        self.tools_evidence = evidence;
        self
    }

    pub fn tools_evidence(&self) -> Option<&'static str> {
        self.tools_evidence
    }

    pub fn provider(&self) -> &str {
        &self.provider
    }

    pub fn protocol(&self) -> &str {
        &self.protocol
    }

    pub fn model(&self) -> &AssignedModel {
        &self.model
    }

    pub fn credential(&self) -> &CredentialId {
        &self.credential
    }

    pub fn cost(&self) -> Cost {
        self.cost
    }

    pub fn tools(&self) -> ToolSemantics {
        self.tools
    }
}

/// Which model Glasshouse assigned, including the honest case where it
/// assigned none.
///
/// Phase 9H line 505 asks for "a provider **and model**" at session start. A
/// gateway-backed launch profile need not name one, and when it does not the
/// harness sends whatever model it decided on. Recording that as
/// `model: None` would leave a reader unable to tell "no model" from "we
/// forgot"; this type says which happened.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AssignedModel {
    /// Glasshouse named this model for the session, from the launch
    /// profile's own `model` field.
    Named(String),
    /// The launch profile named no model, so the harness's own default
    /// serves the session and Glasshouse assigned none. Not a failure.
    HarnessDefault,
}

impl AssignedModel {
    pub fn named(model: impl Into<String>) -> Self {
        Self::Named(model.into())
    }

    /// The model's name, or `None` when the harness chose it.
    pub fn name(&self) -> Option<&str> {
        match self {
            Self::Named(model) => Some(model),
            Self::HarnessDefault => None,
        }
    }

    /// For a diagnostic. Never `""`: an empty column reads as missing data.
    pub fn label(&self) -> &str {
        match self {
            Self::Named(model) => model,
            Self::HarnessDefault => "the harness's own default",
        }
    }
}

/// Whether a change of backend leaves provider-side prompt caching usable —
/// Phase 9H line 516's "warn when failover is likely to invalidate
/// provider-side prompt caching", written down once so every warning in
/// Glasshouse comes from it:
///
/// - **Different provider or different model**: certain, [`CacheLocality::Lost`],
///   since the cache is held by the provider and keyed by the model as well
///   as the prefix.
/// - **Same provider and model, different credential**: [`CacheLocality::LikelyLost`],
///   a likelihood rather than a fact, since Glasshouse has established
///   account-scoping for **no** configured provider — every template in
///   [`crate::provider::templates`] declares its capabilities `Unverified`.
/// - **Nothing moved**: [`CacheLocality::Preserved`].
///
/// Rotating a credential (Phase 9I line 537) is a cache event too, which is
/// why this is a function rather than a comment.
// History: design-decisions.md, "Trims: routing module docs", routing/mod.rs `enum CacheLocality` doc.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CacheLocality {
    /// The request still goes to the same provider, model and credential.
    Preserved,
    /// A provider-side cache cannot survive this change.
    Lost(CacheLossReason),
    /// It probably cannot, and nothing has established that it can.
    LikelyLost(CacheLossReason),
}

/// What moved, for a warning that names the cause.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CacheLossReason {
    ProviderChanged,
    ModelChanged,
    /// Both, which is worth distinguishing so the warning does not have to
    /// pick one.
    ProviderAndModelChanged,
    CredentialChanged,
}

impl CacheLossReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ProviderChanged => "the provider changed",
            Self::ModelChanged => "the model changed",
            Self::ProviderAndModelChanged => "the provider and the model both changed",
            Self::CredentialChanged => "the credential changed",
        }
    }
}

impl CacheLocality {
    /// Compare two backends for cache locality, by the rule in this type's
    /// documentation.
    ///
    /// The one place the rule exists. Every warning, every stickiness
    /// justification and every migration note reads it from here, so there is
    /// no second copy to drift.
    pub fn between(from: &Backend, to: &Backend) -> Self {
        let provider_changed = from.provider() != to.provider();
        let model_changed = from.model() != to.model();
        match (provider_changed, model_changed) {
            (true, true) => Self::Lost(CacheLossReason::ProviderAndModelChanged),
            (true, false) => Self::Lost(CacheLossReason::ProviderChanged),
            (false, true) => Self::Lost(CacheLossReason::ModelChanged),
            (false, false) => {
                if from.credential() == to.credential() {
                    Self::Preserved
                } else {
                    Self::LikelyLost(CacheLossReason::CredentialChanged)
                }
            }
        }
    }

    /// Whether this change is worth warning the user about at all.
    pub fn warrants_a_warning(&self) -> bool {
        !matches!(self, Self::Preserved)
    }

    /// The reason, when there is one.
    pub fn reason(&self) -> Option<CacheLossReason> {
        match self {
            Self::Preserved => None,
            Self::Lost(reason) | Self::LikelyLost(reason) => Some(*reason),
        }
    }
}

impl std::fmt::Display for CacheLocality {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Preserved => f.write_str("provider-side prompt caching is unaffected"),
            Self::Lost(reason) => write!(
                f,
                "provider-side prompt caching is invalidated: {}",
                reason.as_str()
            ),
            Self::LikelyLost(reason) => write!(
                f,
                "provider-side prompt caching is likely to be invalidated: {} — provider caches \
                 are commonly scoped to the account a key belongs to, and the gateway has not \
                 established otherwise for this provider",
                reason.as_str()
            ),
        }
    }
}

/// One named contribution to a routing decision, with the magnitude it added
/// and the evidence behind it.
///
/// Phase 9J line 575 asks for "the contribution of the pairing prior in
/// routing explanations"; this type is deliberately not named after pairing.
/// `phase-32d`'s protected-quota reserve needs the identical shape for a
/// completely different contribution, and a type only pairing could populate
/// would have to be rebuilt for it. A magnitude of `0.0` is a legitimate
/// contribution — an informational line (which class a pairing is, how much
/// evidence exists) that adds nothing to the total but still belongs in the
/// explanation.
#[derive(Debug, Clone, PartialEq)]
pub struct Contribution {
    name: String,
    magnitude: f64,
    evidence: String,
}

impl Contribution {
    pub fn new(name: impl Into<String>, magnitude: f64, evidence: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            magnitude,
            evidence: evidence.into(),
        }
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn magnitude(&self) -> f64 {
        self.magnitude
    }

    pub fn evidence(&self) -> &str {
        &self.evidence
    }
}

/// An ordered list of named contributions behind one routing decision, and
/// their sum.
///
/// Ordered because a reader compares a decision to its reasons top to bottom,
/// and because the caller that builds one (a scoring policy) is the only
/// party that knows which contribution logically comes first. Nothing here
/// deduplicates or reorders by name: two contributions with the same name are
/// two lines, and that is a policy's own affair, not this type's.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct RoutingExplanation {
    contributions: Vec<Contribution>,
}

impl RoutingExplanation {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(&mut self, contribution: Contribution) -> &mut Self {
        self.contributions.push(contribution);
        self
    }

    pub fn contributions(&self) -> &[Contribution] {
        &self.contributions
    }

    /// The sum of every contribution's magnitude — the score a policy would
    /// rank candidates by, not a value this type interprets on its own.
    pub fn total(&self) -> f64 {
        self.contributions.iter().map(Contribution::magnitude).sum()
    }

    /// One line per contribution, signed magnitude first, for a diagnostic.
    pub fn render(&self) -> String {
        use std::fmt::Write as _;
        let mut out = String::new();
        for contribution in &self.contributions {
            let _ = writeln!(
                out,
                "  {:+.3}  {} — {}",
                contribution.magnitude(),
                contribution.name(),
                contribution.evidence()
            );
        }
        out
    }
}

/// Which of the two facts `HardConstraint::ProviderUnavailable` is
/// reporting — named for what each is, the same way
/// `free::CooldownCause` is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderUnavailableCause {
    /// The provider itself refused the credential. Not a cooldown: waiting
    /// does not fix a revoked key.
    CredentialRejected,
    /// The provider declared a cooldown (`free::CooldownCause::Declared`)
    /// and it has not yet elapsed.
    DeclaredCooldown,
}

impl std::fmt::Display for ProviderUnavailableCause {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::CredentialRejected => write!(
                f,
                "was refused by its provider — waiting does not fix a revoked key"
            ),
            Self::DeclaredCooldown => write!(
                f,
                "is in a cooldown its provider declared, which has not yet elapsed"
            ),
        }
    }
}

/// Why a candidate cannot serve this session at all -- the refusals the
/// gateway can establish on its own: the wire shape, tool semantics, a
/// privacy rule, the caller's own pin, or a provider it has observed down.
///
/// The host's `HardConstraint` is a superset of this: it adds the capability
/// axis, the workload tier and the entitlement rules, all of which need the
/// classifier, the capability model or the host's configuration to judge.
/// Those never reach here. `apply_hard_constraints` is generic over the
/// refusal type so both sides use the one function.
#[derive(Debug, Clone, PartialEq)]
pub enum CompatibilityRefusal {
    Protocol,
    /// Line 1517/1513's tool-semantics half: `Backend::tools() ==
    /// KnownAbsent`. `evidence` is the `Declared` string behind that
    /// verdict, carried from `harness::pairing::classify` through
    /// `Backend::with_tools_evidence` and read back by
    /// `session::hard_constraint`; `Some` exactly when the verdict is
    /// `KnownAbsent`, `None` for `Verified` and `Unverified`, where the
    /// evidence would be inert.
    ToolSemantics {
        evidence: Option<&'static str>,
    },
    Privacy,
    UserConstraint,
    /// Line 1518. The provider behind this destination cannot serve right
    /// now, established via [`free::FreePool::health`]: its
    /// credential was rejected, or it declared a cooldown still in force
    /// (`free::CooldownCause::Declared`, authoritative per line
    /// 1319). A resource under Glasshouse's own **invented** cooldown is
    /// never given this constraint — line 534 keeps that guess probeable by
    /// real work, so it stays `session::provider_health`'s soft
    /// penalty instead (`session::hard_constraint`).
    ProviderUnavailable {
        /// The credential's label, for the sentence a person reads.
        credential: String,
        cause: ProviderUnavailableCause,
    },
}

/// A candidate that has survived every hard constraint, and therefore the
/// only thing a scoring policy — a pairing prior among them — may be asked to
/// rank.
///
/// Phase 9J's design settled this as a structural requirement rather than a
/// convention (design decision 2): a policy function that scores a bare `T`
/// could be called before hard constraints ever ran, and nothing would say
/// so. A policy that scores `EligibleCandidate<T>` cannot be called that way,
/// because the only way to produce one is [`apply_hard_constraints`] actually
/// running the check. The private field is the whole mechanism — there is no
/// public constructor here to bypass it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EligibleCandidate<T> {
    value: T,
}

impl<T> EligibleCandidate<T> {
    pub fn value(&self) -> &T {
        &self.value
    }

    pub fn into_inner(self) -> T {
        self.value
    }
}

/// Filter `candidates` by `check`, in order, into what survives every hard
/// constraint and what was rejected and why.
///
/// This is the one function in Glasshouse that can produce an
/// [`EligibleCandidate`]. `check` is supplied by the caller rather than fixed
/// here, because "capability" and "privacy" are decided by configuration this
/// module does not read (line 568 names them; it does not define them) — this
/// function's job is only to make the *ordering* structural, not to invent
/// what a capability or a privacy constraint is.
pub fn apply_hard_constraints<T, E>(
    candidates: Vec<T>,
    check: impl Fn(&T) -> Result<(), E>,
) -> (Vec<EligibleCandidate<T>>, Vec<(T, E)>) {
    let mut eligible = Vec::new();
    let mut rejected = Vec::new();
    for candidate in candidates {
        match check(&candidate) {
            Ok(()) => eligible.push(EligibleCandidate { value: candidate }),
            Err(reason) => rejected.push((candidate, reason)),
        }
    }
    (eligible, rejected)
}
