//! What the gateway forwards to, and the credential it attaches on the way —
//! the half of Phase 9G that the child harness must never be able to reach.
//!
//! # The credential lives here and nowhere else
//!
//! An [`Upstream`] holds a [`Secret`] resolved through
//! [`crate::secret::SecretStore`] inside the Glasshouse process. It is
//! attached to each forwarded request as an `authorization` header, and the
//! header value is marked sensitive so that even `http`'s own
//! [`Debug`](std::fmt::Debug) of a header map renders it as `Sensitive`
//! rather than as the key.
//!
//! Nothing hands this value to a child process, writes it to a file, or puts
//! it in a diagnostic. What the child gets instead is the gateway's own
//! per-instance token — see [`super::GatewayToken`] — which is worthless off
//! this machine and dies with the instance. That is the whole of "never
//! expose provider API keys to a child harness when the local gateway can
//! hold the credential itself".
//!
//! History: design-decisions.md, "Trims: gateway module docs", upstream.rs module doc.

use std::sync::atomic::{AtomicUsize, Ordering};

use ureq::Agent;
use ureq::config::AutoHeaderValue;
use ureq::http::{HeaderValue, Uri};

use super::subscription_broker::RunningSubscriptionBroker;
use crate::routing::{AssignedModel, Backend, Cost, CredentialId, ToolSemantics};
use crate::secret::Secret;

/// The scheme-and-host prefix an upstream base URL must carry.
///
/// Checked at construction rather than at the first request: a gateway that
/// bound a port and only then discovered it had nowhere to forward to would
/// have already told a harness it was ready.
const REQUIRED_SCHEMES: &[&str] = &["https", "http"];

/// The API-version segment a request target may or may not carry, and which
/// says nothing about **which** protocol the target belongs to.
///
/// Both harnesses that can back a gateway profile were run against a
/// listener that recorded the request line, pointed at a base URL with no
/// path — which is the only kind [`super::Gateway::base_url`] hands out:
///
/// - Claude Code 2.1.245, `ANTHROPIC_BASE_URL=http://127.0.0.1:<port>` →
///   `POST /v1/messages?beta=true`.
/// - Codex 0.149.1, `base_url = "http://127.0.0.1:<port>"` → `POST
///   /responses`. The same binary pointed at `.../v1` sends `POST
///   /v1/responses`.
///
/// So whether this segment is present depends on the harness's own idea of
/// where its base URL ends, not on the protocol. It is therefore stripped
/// before a target is **classified** — and never before it is
/// **forwarded**: [`Route::uri_for`] still appends the target byte for byte,
/// so the provider receives exactly the path the harness asked for.
pub(super) const VERSION_SEGMENT: &str = "/v1";

/// One protocol the gateway serves: which request targets belong to it, and
/// where they go.
///
/// The protocol is carried as its **slug** rather than as a
/// `harness::WireProtocol`, because no file in this directory may
/// name `crate::harness` — see [`mod@super`]'s header and the scan that
/// enforces it. The slug is a name, of exactly the class this module already
/// puts in a diagnostic, and nothing here ever parses it back into anything.
#[derive(Debug)]
pub struct Route {
    /// The protocol's slug, from `WireProtocol::slug`. A name, never a
    /// credential.
    protocol: String,
    /// What is established about this provider's tool-call behaviour **on
    /// this protocol** — per protocol for the same reason the base URL is,
    /// because a provider may carry tool calls on one and not another.
    ///
    /// Phase 9H line 517 refuses a failover that cannot preserve the
    /// harness's tool semantics, and
    /// [`crate::routing::interactive`] is where that comparison lives. It is
    /// carried here because this is the value that already travels with a
    /// protocol's destination.
    tools: ToolSemantics,
    /// The version-independent path prefixes that belong to this protocol,
    /// composed by the caller that *can* see the protocol enum — see
    /// `crate::profile::ingress_targets`.
    targets: &'static [&'static str],
    /// The provider's declared base URL for this protocol, with any trailing
    /// slash removed so that appending a request target cannot produce `//`.
    base_url: String,
}

impl Route {
    /// Declare that `targets` belong to `protocol` and are forwarded to
    /// `base_url`.
    ///
    /// The base URL is only trimmed here; whether it is usable at all is
    /// checked by [`Upstream::new`], which is the layer that knows the
    /// provider's name and can therefore say whose base URL was wrong.
    pub fn new(protocol: String, targets: &'static [&'static str], base_url: &str) -> Self {
        Self {
            protocol,
            tools: ToolSemantics::Unverified,
            targets,
            base_url: base_url.trim_end_matches('/').to_owned(),
        }
    }

    /// State what is established about tool calls on this protocol.
    ///
    /// A builder rather than a fourth argument to [`Route::new`], so that
    /// every existing call site keeps meaning what it meant:
    /// [`ToolSemantics::Unverified`] — "nobody checked" — which is what a
    /// route that says nothing has always meant.
    pub fn with_tools(mut self, tools: ToolSemantics) -> Self {
        self.tools = tools;
        self
    }

    /// What is established about tool calls on this protocol.
    pub fn tools(&self) -> ToolSemantics {
        self.tools
    }

    /// The protocol's slug, for a diagnostic.
    pub(super) fn protocol(&self) -> &str {
        &self.protocol
    }

    /// The upstream host this route forwards to, for a diagnostic. A host,
    /// never a path and never a query.
    pub(super) fn host(&self) -> String {
        self.base_url
            .parse::<Uri>()
            .ok()
            .and_then(|uri| uri.host().map(str::to_owned))
            .unwrap_or_default()
    }

    /// Whether `target` belongs to this route's protocol.
    ///
    /// The query is dropped — it is part of the target and is forwarded, but
    /// it never decides where a request goes — the [`VERSION_SEGMENT`] is
    /// stripped, and what remains must match one of the declared prefixes
    /// **at a path-segment boundary**. That last part is the difference
    /// between `/messages/count_tokens` belonging to the Anthropic Messages
    /// route, which it does, and `/messagesomethingelse` belonging to it,
    /// which it must not.
    fn claims(&self, target: &str) -> bool {
        let path = path_of(target);
        let path = match path.strip_prefix(VERSION_SEGMENT) {
            Some(rest) if rest.is_empty() || rest.starts_with('/') => rest,
            _ => path,
        };
        self.targets
            .iter()
            .any(|declared| is_segment_prefix(path, declared))
    }

    /// The request target appended to the declared base URL.
    ///
    /// This is one of the exactly three things the gateway rewrites, and it
    /// is a concatenation rather than a URL join: a join would normalise
    /// `..`, re-encode a query and resolve a relative reference, all of
    /// which change what the harness asked for.
    pub(super) fn uri_for(&self, target: &str) -> Option<Uri> {
        let separator = if target.starts_with('/') { "" } else { "/" };
        format!("{}{separator}{target}", self.base_url).parse().ok()
    }
}

/// A request target's path: everything before a query or a fragment.
pub(super) fn path_of(target: &str) -> &str {
    let end = target.find(['?', '#']).unwrap_or(target.len());
    &target[..end]
}

/// Whether `path` is `prefix` or lies underneath it, on a segment boundary.
fn is_segment_prefix(path: &str, prefix: &str) -> bool {
    path == prefix || (path.starts_with(prefix) && path[prefix.len()..].starts_with('/'))
}

/// One provider the gateway can forward to: where each of its protocols
/// lives, and the credential that goes with all of them.
///
/// The credential is resolved once, at gateway start, and moved in. There is
/// no accessor for it — only the crate-private `UpstreamBackend::authorization`,
/// which produces the header the gateway attaches. A getter returning the
/// value would be a second door into the one thing this module exists to keep
/// behind one.
pub struct UpstreamBackend {
    /// The provider's configured name. A name, for diagnostics — never a
    /// credential, and the same class of value `BackendResource::slug`
    /// already puts in a session record.
    provider: String,
    /// One route per protocol this backend serves. Never empty: a backend
    /// with nowhere to forward to is refused at construction.
    routes: Vec<Route>,
    /// The models this backend's account declares it serves, normalised for
    /// comparison, or empty when nobody declared any.
    ///
    /// Empty means **unknown**, never "all": a backend that claimed a model
    /// nobody said it had would send a request to a provider that refuses it,
    /// and the refusal would arrive as a routing failure rather than as the
    /// configuration mistake it is.
    models: Vec<String>,
    /// The provider credential, resolved in-process and never leaving it.
    credential: BackendCredential,
    /// Which credential this is, **by name** — the environment variable or
    /// the store service and account it was resolved through.
    ///
    /// Phase 9I lines 537 and 538 need quota and health state keyed by the
    /// credential rather than by the provider, and a key is a thing that gets
    /// printed. This is the printable half; [`UpstreamBackend::credential`]
    /// is the half that is not.
    credential_id: CredentialId,
    /// Whether this backend costs anything at the margin — Phase 9I line 527,
    /// as the user marked it. [`Cost::Metered`] when nobody marked anything,
    /// which is the fail-closed direction.
    cost: Cost,
    /// The `[accounts.<name>]` this backend was built from, when a pool built
    /// it; a model's pool, its exclusions and its rests are keyed by it.
    account: Option<String>,
}

enum BackendCredential {
    Provider(Secret),
    SubscriptionBroker(Box<RunningSubscriptionBroker>),
}

impl UpstreamBackend {
    /// Build one backend from a provider's name, one [`Route`] per protocol
    /// it serves, its resolved credential and that credential's name.
    pub fn new(
        provider: String,
        routes: Vec<Route>,
        credential: Secret,
        credential_id: CredentialId,
        cost: Cost,
    ) -> Result<Self, UpstreamError> {
        if routes.is_empty() {
            return Err(UpstreamError::NoProtocolServed { provider });
        }
        for route in &routes {
            let uri: Uri =
                route
                    .base_url
                    .parse()
                    .map_err(|_| UpstreamError::BaseUrlNotAbsolute {
                        provider: provider.clone(),
                        protocol: route.protocol.clone(),
                    })?;
            let scheme_is_http = uri
                .scheme_str()
                .is_some_and(|scheme| REQUIRED_SCHEMES.contains(&scheme));
            if !scheme_is_http || uri.host().is_none() {
                return Err(UpstreamError::BaseUrlNotAbsolute {
                    provider,
                    protocol: route.protocol.clone(),
                });
            }
        }
        // Declared models are attached by `with_models`, so every existing
        // caller keeps the behaviour it had: an empty set claims nothing and
        // per-model selection simply never picks this backend.
        // Checked once, here, so that a credential carrying a newline is a
        // refusal to start rather than a header-injection attempt on every
        // forwarded request.
        if HeaderValue::from_str(&bearer(&credential)).is_err() {
            return Err(UpstreamError::CredentialNotHeaderSafe { provider });
        }

        Ok(Self {
            models: Vec::new(),
            provider,
            routes,
            credential: BackendCredential::Provider(credential),
            credential_id,
            cost,
            account: None,
        })
    }

    /// Consume an exact account-specific broker. Keeping the process inside
    /// the credential boundary makes its lifetime identical to the backend's.
    /// `models` is the subscription's own catalogue, read and parsed by the
    /// caller. It arrives as a list rather than being fetched here because
    /// this file is relay code: `no_part_of_the_relay_deserializes_anything`
    /// forbids it naming a deserializer at all, and that rule is blunt on
    /// purpose — a scan cannot tell a catalogue parse from a body parse, and
    /// the second is the one that must never appear.
    pub fn from_subscription_broker(
        routes: Vec<Route>,
        broker: RunningSubscriptionBroker,
        models: Vec<String>,
    ) -> Result<Self, UpstreamError> {
        let credential_id = broker.credential_id().clone();
        let provider = broker.provider_name().to_owned();

        if routes.is_empty() {
            return Err(UpstreamError::NoProtocolServed { provider });
        }
        for route in &routes {
            let uri: Uri =
                route
                    .base_url
                    .parse()
                    .map_err(|_| UpstreamError::BaseUrlNotAbsolute {
                        provider: provider.clone(),
                        protocol: route.protocol.clone(),
                    })?;
            if !uri
                .scheme_str()
                .is_some_and(|scheme| REQUIRED_SCHEMES.contains(&scheme))
                || uri.host().is_none()
            {
                return Err(UpstreamError::BaseUrlNotAbsolute {
                    provider,
                    protocol: route.protocol.clone(),
                });
            }
        }
        if HeaderValue::from_str(&bearer_text(broker.internal_api_key())).is_err() {
            return Err(UpstreamError::CredentialNotHeaderSafe { provider });
        }
        Ok(Self {
            provider,
            routes,
            credential: BackendCredential::SubscriptionBroker(Box::new(broker)),
            credential_id,
            cost: Cost::Free,
            models: Vec::new(),
            account: None,
        }
        .with_models(models))
    }

    /// The provider's name, for a diagnostic.
    pub(super) fn provider(&self) -> &str {
        &self.provider
    }

    /// Which credential this backend uses, by name.
    /// Declares the models this backend's account serves.
    ///
    /// Names are normalised on the way in, because an account catalogue and a
    /// request spell the same model differently often enough that comparing
    /// them raw would silently never match.
    #[must_use]
    pub fn with_models<I, S>(mut self, models: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        self.models = models
            .into_iter()
            .map(|model| normalise_model(model.as_ref()))
            .collect();
        self.models.sort();
        self.models.dedup();
        self
    }

    /// This backend as a member of `account`'s pool.
    #[must_use]
    pub fn with_account(mut self, account: &str) -> Self {
        self.account = Some(account.to_owned());
        self
    }

    /// The account name a pool knows this backend by, else its provider.
    #[must_use]
    pub fn account(&self) -> &str {
        self.account.as_deref().unwrap_or(&self.provider)
    }

    /// Whether this backend's account declares `model`.
    ///
    /// A backend that declares nothing answers `false` for everything. That
    /// is the direction that keeps an unconfigured pool behaving exactly as it
    /// does today: per-model selection finds no candidate and the session's
    /// own backend serves, unchanged.
    #[must_use]
    pub fn serves_model(&self, model: &str) -> bool {
        let wanted = normalise_model(model);
        self.models.contains(&wanted)
    }

    /// Whether this backend may be a candidate for `model`: a backend that
    /// declares a catalogue is a candidate only for what it lists, and one
    /// that declares nothing is a candidate for everything, as it always was.
    ///
    /// The invariant this keeps: a same-model failover lands only where the
    /// model can be served. Without it every broker-backed subscription —
    /// which carries all four protocols — is a "compatible" candidate for
    /// every other's model, and a session fails over into a model-not-found
    /// it then reads as `Served` and never leaves.
    #[must_use]
    pub fn can_serve(&self, model: &AssignedModel) -> bool {
        match model.name() {
            Some(name) => self.models.is_empty() || self.serves_model(name),
            None => true,
        }
    }

    pub fn credential_id(&self) -> &CredentialId {
        &self.credential_id
    }

    /// The slug of every protocol this backend can carry, in the order the
    /// routes were declared.
    pub fn served_protocols(&self) -> Vec<&str> {
        self.routes.iter().map(Route::protocol).collect()
    }

    /// The route a request target belongs to, or `None`.
    ///
    /// `None` is a refusal, never an invitation to pick the first route: a
    /// target the gateway cannot place is one it would otherwise append to
    /// whichever base URL happened to be declared first, which is a request
    /// sent somewhere nobody asked for it to go.
    pub(super) fn route_for(&self, target: &str) -> Option<&Route> {
        self.routes.iter().find(|route| route.claims(target))
    }

    /// The route for `protocol`, by slug, or `None` when this backend does
    /// not serve it. What a translated exchange forwards through: the pair
    /// table named the served protocol, and this is where it goes.
    pub(super) fn route_named(&self, protocol: &str) -> Option<&Route> {
        self.routes
            .iter()
            .find(|route| route.protocol() == protocol)
    }

    /// The `authorization` header the gateway attaches, replacing whatever
    /// the child sent.
    ///
    /// Marked sensitive, so `http`'s own rendering of a header map prints
    /// `Sensitive` in its place. That is belt over braces — nothing here
    /// renders a request's headers — but it costs one call and removes a
    /// whole class of future accident.
    pub(super) fn authorization(&self) -> HeaderValue {
        let text = match &self.credential {
            BackendCredential::Provider(secret) => bearer(secret),
            BackendCredential::SubscriptionBroker(broker) => bearer_text(broker.internal_api_key()),
        };
        let mut value = HeaderValue::from_str(&text).expect("checked when the backend was built");
        value.set_sensitive(true);
        value
    }

    /// This backend as a routing candidate for one protocol and one model, or
    /// `None` when it does not serve that protocol.
    ///
    /// The translation from "a place to send bytes" to "a thing a routing
    /// policy can compare" happens exactly here, so that
    /// [`mod@crate::routing`] never has to learn what a base URL is and this
    /// module never has to learn what a policy is.
    pub fn as_routing_backend(&self, protocol: &str, model: &AssignedModel) -> Option<Backend> {
        let route = self
            .routes
            .iter()
            .find(|route| route.protocol() == protocol)?;
        Some(Backend::new(
            self.provider.clone(),
            protocol.to_owned(),
            model.clone(),
            self.credential_id.clone(),
            self.cost,
            route.tools(),
        ))
    }
}

/// Prints the provider and its routes, and the credential's own redaction
/// marker.
///
/// Manual rather than derived for the same reason
/// `profile::LaunchOverlay`'s is: the field this type exists to
/// hold must not be renderable, and a derive is one added field away from
/// making it so. [`Secret`]'s own rendering would already print the marker;
/// this makes that independent of it.
impl std::fmt::Debug for UpstreamBackend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("UpstreamBackend")
            .field("provider", &self.provider)
            .field("routes", &self.routes)
            .field("credential", &crate::secret::REDACTED)
            .field("credential_id", &self.credential_id.label())
            .field("cost", &self.cost.as_str())
            .finish()
    }
}

/// Capability map line 2451's two response header names: which entitlement
/// served an exchange, on every response head the gateway writes for one —
/// never on a refusal, since nothing served those. See [`ServedBy`].
pub(super) const PROVIDER_HEADER: &str = "x-glasshouse-provider";
pub(super) const ENTITLEMENT_HEADER: &str = "x-glasshouse-entitlement";

/// The provider and entitlement label a served exchange's response head
/// carries — never the secret, only [`UpstreamBackend::provider`] and
/// [`UpstreamBackend::credential_id`]'s label, the same string the session
/// already records as `quota_context`.
///
/// Threaded through the translated path's writers as one value rather than
/// two loose strings (CLAUDE.md rule 8): both are known together, from the
/// one backend that served the exchange, and travel together to the two
/// writers that need them.
pub(super) struct ServedBy {
    provider: String,
    entitlement: String,
}

impl ServedBy {
    pub(super) fn of(backend: &UpstreamBackend) -> Self {
        Self {
            provider: backend.provider().to_owned(),
            entitlement: backend.credential_id().label(),
        }
    }

    /// A stand-in value for a test that exercises a writer directly, with no
    /// backend to build one from — `translate::tests`'s own
    /// `stream_events_refuses_a_delta_...` is the caller.
    #[cfg(test)]
    pub(super) fn for_test(provider: &str, entitlement: &str) -> Self {
        Self {
            provider: provider.to_owned(),
            entitlement: entitlement.to_owned(),
        }
    }

    /// Push this exchange's two response headers onto `headers`, in the
    /// order every writer emits them.
    pub(super) fn push_onto(&self, headers: &mut Vec<(String, Vec<u8>)>) {
        headers.push((
            PROVIDER_HEADER.to_owned(),
            self.provider.clone().into_bytes(),
        ));
        headers.push((
            ENTITLEMENT_HEADER.to_owned(),
            self.entitlement.clone().into_bytes(),
        ));
    }
}

/// Where the gateway forwards, and the credential it forwards with.
///
/// # One serving backend, and the ones it could move to
///
/// Built once per Glasshouse instance and shared by every connection thread.
/// The **set** of backends is immutable; which of them is serving is an
/// index, and moving that index is the whole of Phase 9H's failover.
///
/// Phase 9G deliberately left this as exactly one provider and said so:
/// *"which backend a session runs against is Phase 9H's sticky routing"*, and
/// `crate::profile::gateway_upstream` refused a configuration with more than
/// one candidate rather than choosing between them. This is that phase. The
/// first candidate in the user's own configuration order is **assigned**, and
/// the rest are where a real provider failure may move the session — never
/// per turn, never for a cheaper model, and never across a protocol or a
/// weakening of tool semantics. [`crate::routing::interactive`] owns every
/// one of those decisions; this type owns only the consequence.
// History: design-decisions.md, "Trims: gateway, profile and provider module docs", gateway/upstream.rs `Upstream` struct doc.
pub struct Upstream {
    /// The assigned backend first, then failover candidates in the user's own
    /// configuration order. Never empty.
    backends: Vec<UpstreamBackend>,
    /// Which of `backends` is serving. Only ever set to a valid index.
    serving: AtomicUsize,
    /// Accounts answering `429` rest until the instant beside them: the
    /// next request for a model several accounts serve goes to the next
    /// one in its pool (2026-09-23, the user's pooled subscriptions).
    cooling: std::sync::Mutex<std::collections::HashMap<String, std::time::Instant>>,
    /// Whether the person took an account out of its pool; read per
    /// request (`provider::pool_state`). `None` excludes nothing.
    excluded: Option<Exclusion>,
}

/// Whether an account (by name) is out of its pool.
pub type Exclusion = std::sync::Arc<dyn Fn(&str) -> bool + Send + Sync>;

/// Why an [`Upstream`] could not be built.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum UpstreamError {
    #[error(
        "the provider `{provider}` serves none of the protocols the gateway's \
         ingress offers, so the gateway would have nowhere to forward to"
    )]
    NoProtocolServed { provider: String },
    #[error(
        "the provider `{provider}` declares a base URL for {protocol} that is not an absolute \
         http(s) URL, so the gateway has nowhere to forward to"
    )]
    BaseUrlNotAbsolute { provider: String, protocol: String },
    #[error(
        "the credential for the provider `{provider}` cannot be attached to a request; it \
         contains a character that is not allowed in an HTTP header value"
    )]
    CredentialNotHeaderSafe { provider: String },
    #[error("the gateway was given no provider to forward to")]
    NoBackend,
}

/// One spelling for a model name, so a catalogue and a request compare.
///
/// Case and the dot/dash split are the two differences that actually occur
/// between an account catalogue and the identifier a request carries. Nothing
/// else is rewritten: a date or effort suffix distinguishes two real models
/// and dropping it would make them collide.
fn normalise_model(model: &str) -> String {
    model.trim().to_ascii_lowercase().replace(['.', '_'], "-")
}

impl Upstream {
    /// One backend and no failover candidates.
    ///
    /// The shape every caller written before Phase 9H assumes, kept so that
    /// those callers still say what they meant: a gateway with nowhere else
    /// to go.
    pub fn new(
        provider: String,
        routes: Vec<Route>,
        credential: Secret,
        credential_id: CredentialId,
    ) -> Result<Self, UpstreamError> {
        let backend = UpstreamBackend::new(
            provider,
            routes,
            credential,
            credential_id,
            // Nobody said this backend was free, so it is metered — see
            // `Cost`'s own documentation for why that is the safe direction.
            Cost::Metered,
        )?;
        Ok(Self {
            backends: vec![backend],
            serving: AtomicUsize::new(0),
            cooling: std::sync::Mutex::default(),
            excluded: None,
        })
    }

    /// The assigned backend, followed by the candidates a real provider
    /// failure may move the session to.
    pub fn with_failover(backends: Vec<UpstreamBackend>) -> Result<Self, UpstreamError> {
        if backends.is_empty() {
            return Err(UpstreamError::NoBackend);
        }
        Ok(Self {
            backends,
            serving: AtomicUsize::new(0),
            cooling: std::sync::Mutex::default(),
            excluded: None,
        })
    }

    /// The same upstream, asking `excluded` which accounts are out of their
    /// pool on every request.
    #[must_use]
    pub fn with_exclusion(mut self, excluded: Exclusion) -> Self {
        self.excluded = Some(excluded);
        self
    }

    /// `account` answered `429`: rest it for `rest`, so the next request for
    /// a model its pool shares goes to another account.
    pub fn cool_down(&self, account: &str, rest: std::time::Duration) {
        if let Ok(mut cooling) = self.cooling.lock() {
            cooling.insert(account.to_string(), std::time::Instant::now() + rest);
        }
    }

    fn cooling(&self, account: &str) -> bool {
        self.cooling
            .lock()
            .ok()
            .and_then(|cooling| cooling.get(account).copied())
            .is_some_and(|until| std::time::Instant::now() < until)
    }

    fn is_excluded(&self, account: &str) -> bool {
        self.excluded
            .as_ref()
            .is_some_and(|excluded| excluded(account))
    }

    /// The backend currently serving.
    ///
    /// A connection thread calls this **once**, at the top of its exchange,
    /// and uses the reference for the whole of it. That is what makes a
    /// failover on another thread unable to split one request between two
    /// providers.
    pub(super) fn serving(&self) -> &UpstreamBackend {
        let index = self.serving.load(Ordering::Relaxed);
        self.backends
            .get(index)
            .expect("`serving` is only ever set to an index that exists")
    }

    /// The backend that should carry a request for `model`.
    ///
    /// This is the whole of per-model routing: a pool holding several accounts
    /// resolves each request against the account that declares the model it
    /// names, instead of sending everything to whichever account the session
    /// happened to start on. It is what lets one session reason on a frontier
    /// model, reduce on a cheap one from another provider, and delegate to a
    /// third.
    ///
    /// **It falls back to the serving backend**, so a pool where nothing
    /// declares the model — which is every pool that has not been given
    /// catalogues — behaves exactly as it did before this existed.
    #[must_use]
    pub fn serving_for(&self, model: Option<&str>) -> &UpstreamBackend {
        model
            .and_then(|model| self.for_model(model))
            .unwrap_or_else(|| self.serving())
    }

    /// The backend that should carry a request for `model`, at `target`.
    ///
    /// [`Self::serving_for`] alone sends a target the model-chosen backend
    /// does not claim into `unrouted`, even when some other configured
    /// backend claims it — reachable whenever a session is bound to a chat
    /// account and a request names a route only a relay-only provider
    /// declares, such as `typesafe-systemone`'s `/systemone`
    /// (`archive/glasshouse:docs/product/evidence/phase-66.md`, *Provider facts*). This holds
    /// as long as such a target has exactly one claimant: **when the
    /// model-chosen backend does not claim `target`, and exactly one backend
    /// does, that backend serves the request.** Two or more claimants keep
    /// today's behaviour — the model-chosen backend, then `unrouted` — because
    /// ranking between two decision providers is not this method's decision
    /// to make.
    #[must_use]
    pub fn serving_for_target(&self, model: Option<&str>, target: &str) -> &UpstreamBackend {
        let chosen = self.serving_for(model);
        if chosen.route_for(target).is_some() {
            return chosen;
        }
        let mut claimants = self
            .backends
            .iter()
            .filter(|backend| backend.route_for(target).is_some());
        match (claimants.next(), claimants.next()) {
            (Some(only), None) => only,
            _ => chosen,
        }
    }

    /// The first backend of `model`'s pool that is in it and not resting,
    /// else the first in it at all, or `None`.
    ///
    /// A model's pool is every backend declaring it, in configuration
    /// order. First rather than best: the order is the configured one, and
    /// staying on one account keeps the provider's prompt cache warm, where
    /// spreading requests would throw it away every switch. An account the
    /// person took out of the pool is never chosen; one resting after a
    /// `429` is chosen only when every other one rests too. Choosing on
    /// price or measured quality is a ranking decision that belongs to
    /// routing, not to the thing that carries the bytes.
    #[must_use]
    pub fn for_model(&self, model: &str) -> Option<&UpstreamBackend> {
        let mut pool = self
            .backends
            .iter()
            .filter(|backend| backend.serves_model(model))
            .filter(|backend| !self.is_excluded(backend.account()))
            .peekable();
        let first = *pool.peek()?;
        Some(
            pool.find(|backend| !self.cooling(backend.account()))
                .unwrap_or(first),
        )
    }

    /// Every backend, assigned first.
    pub fn backends(&self) -> &[UpstreamBackend] {
        &self.backends
    }

    /// The provider's name, for a diagnostic.
    pub(super) fn provider(&self) -> &str {
        self.serving().provider()
    }

    /// The slug of every protocol the **serving** backend can carry.
    ///
    /// The serving one and not the union, deliberately: a launch profile that
    /// refused against the union would start a harness against an ingress
    /// whose current backend has no route for it.
    pub fn served_protocols(&self) -> Vec<&str> {
        self.serving().served_protocols()
    }

    /// Move the session onto the backend using `credential`, and say whether
    /// it moved.
    ///
    /// Keyed by [`CredentialId`] rather than by provider name because a
    /// provider with two keys is two backends here — Phase 9E's credential
    /// pool — and Phase 9I line 537's rotation moves between exactly those
    /// two. A provider name would not distinguish them.
    ///
    /// `false` means no backend uses that credential, which a caller should
    /// treat as a defect rather than as a refusal: the candidate it was given
    /// came from this same list.
    pub fn switch_to(&self, credential: &CredentialId) -> bool {
        match self
            .backends
            .iter()
            .position(|backend| backend.credential_id() == credential)
        {
            Some(index) => {
                self.serving.store(index, Ordering::Relaxed);
                true
            }
            None => false,
        }
    }

    /// Every credential configured for `provider`, in configuration order.
    ///
    /// What Phase 9I line 537's rotation is offered: *this provider's* other
    /// keys, so that one key's exhaustion stays that key's limit.
    pub fn credentials_of(&self, provider: &str) -> Vec<CredentialId> {
        self.backends
            .iter()
            .filter(|backend| backend.provider() == provider)
            .map(|backend| backend.credential_id().clone())
            .collect()
    }

    /// The backend using `credential`, as a routing candidate for `protocol`
    /// and `model`.
    pub fn backend_for(
        &self,
        credential: &CredentialId,
        protocol: &str,
        model: &AssignedModel,
    ) -> Option<Backend> {
        self.backends
            .iter()
            .find(|backend| backend.credential_id() == credential)
            .and_then(|backend| backend.as_routing_backend(protocol, model))
    }

    /// Every backend other than the serving one, as routing candidates for
    /// `protocol` and `model`, in configuration order.
    ///
    /// This is what a failure decision is handed. A backend that does not
    /// serve `protocol` is simply absent — it could never be a candidate, and
    /// including it so that the policy could reject it would put the same
    /// knowledge in two places.
    pub fn failover_candidates(&self, protocol: &str, model: &AssignedModel) -> Vec<Backend> {
        let serving = self.serving.load(Ordering::Relaxed);
        self.backends
            .iter()
            .enumerate()
            .filter(|(index, backend)| *index != serving && backend.can_serve(model))
            .filter_map(|(_, backend)| backend.as_routing_backend(protocol, model))
            .collect()
    }

    /// Every backend, serving one included, as routing candidates for
    /// `protocol` and `model`, in configuration order.
    ///
    /// [`Self::failover_candidates`] answers "where could this session go
    /// *now*", which is a question about the moment it is asked. This answers
    /// "which routes could this session ever be on", which is what a caller
    /// resolving something per route — `crate::profile`'s
    /// `crate::routing::pairing::PairingAffinities`, at launch — needs: the
    /// serving index moves when a failover is taken, so a set built from the
    /// failover candidates alone would be missing exactly the backend the
    /// session started on the first time it failed back.
    pub fn routing_backends(&self, protocol: &str, model: &AssignedModel) -> Vec<Backend> {
        self.backends
            .iter()
            .filter(|backend| backend.can_serve(model))
            .filter_map(|backend| backend.as_routing_backend(protocol, model))
            .collect()
    }
}

impl std::fmt::Debug for Upstream {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Upstream")
            .field("serving", &self.serving.load(Ordering::Relaxed))
            .field("backends", &self.backends)
            .finish()
    }
}

/// `Bearer <credential>`, the one place the resolved value is read.
fn bearer(credential: &Secret) -> String {
    bearer_text(credential.expose())
}

fn bearer_text(credential: &str) -> String {
    format!("Bearer {credential}")
}

/// The one HTTP client the gateway uses, configured for pass-through.
///
/// Every setting here exists to stop `ureq` from being helpful:
///
/// - `http_status_as_error(false)` — a `429` is a response to forward, not an
///   error to swallow. With the default, the provider's own error body would
///   never reach the harness.
/// - `max_redirects(0)` — a redirect is a response the harness is entitled to
///   see and decide about. Following one here would also mean deciding
///   whether to re-attach the credential to a host the provider named at
///   runtime.
/// - `user_agent`, `accept` and `accept_encoding` set to
///   [`AutoHeaderValue::None`] — the harness's own headers are forwarded, and
///   a gateway that added its own would be visible to the provider as a
///   client the harness is not.
///
/// Timeouts are left at `ureq`'s defaults, which are unset. A streaming
/// response may legitimately go minutes between events, and a receive
/// timeout here would cut a long generation off mid-stream.
// History: design-decisions.md, "Trims: gateway, profile and provider module docs", gateway/upstream.rs `agent` doc.
pub(super) fn agent() -> Agent {
    Agent::new_with_config(
        Agent::config_builder()
            .http_status_as_error(false)
            .max_redirects(0)
            .user_agent(AutoHeaderValue::None)
            .accept(AutoHeaderValue::None)
            .accept_encoding(AutoHeaderValue::None)
            .allow_non_standard_methods(true)
            .build(),
    )
}

#[cfg(test)]
mod per_model_tests {
    use super::*;

    fn backend(provider: &str, models: &[&str]) -> UpstreamBackend {
        UpstreamBackend::new(
            provider.to_string(),
            vec![Route::new(
                "anthropic-messages".into(),
                &["/v1/messages"],
                "https://example.invalid",
            )],
            Secret::mint_for_test("sk-test-credential-value"),
            CredentialId::new(
                provider,
                crate::secret::SecretRef::Environment {
                    var: format!("{provider}_KEY"),
                },
            ),
            Cost::Metered,
        )
        .expect("a backend with one route")
        .with_models(models.iter().copied())
    }

    /// The point of the whole thing: one session, several accounts, and each
    /// request going to the account that declares the model it names.
    #[test]
    fn a_request_reaches_the_account_declaring_its_model() {
        let pool = Upstream::with_failover(vec![
            backend("claude-max", &["claude-opus-5", "claude-sonnet-5"]),
            backend("chatgpt", &["gpt-5.6-luna", "gpt-6-astra"]),
        ])
        .unwrap();

        assert_eq!(
            pool.serving_for(Some("claude-opus-5")).provider(),
            "claude-max"
        );
        assert_eq!(pool.serving_for(Some("gpt-5.6-luna")).provider(), "chatgpt");
        assert_eq!(pool.serving_for(Some("gpt-6-astra")).provider(), "chatgpt");
    }

    /// A model's pool: every account of that kind, in order. The first
    /// serves until it answers `429`, then the next; an account taken out of
    /// the pool is never chosen; and when every one rests, the first in the
    /// pool is used rather than nothing.
    #[test]
    fn a_pool_stays_on_one_account_skips_a_resting_one_and_never_an_excluded_one() {
        let excluded = std::sync::Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        let seen = std::sync::Arc::clone(&excluded);
        let pool = Upstream::with_failover(vec![
            backend("cliproxyapi", &["claude-opus-5-5"]).with_account("claude-a"),
            backend("cliproxyapi", &["claude-opus-5-5"]).with_account("claude-b"),
            backend("cliproxyapi", &["claude-opus-5-5"]).with_account("claude-c"),
        ])
        .unwrap()
        .with_exclusion(std::sync::Arc::new(move |account: &str| {
            seen.lock().unwrap().iter().any(|name| name == account)
        }));
        let chosen = |pool: &Upstream| {
            pool.for_model("claude-opus-5-5")
                .map(|b| b.account().to_string())
        };

        assert_eq!(chosen(&pool).as_deref(), Some("claude-a"));
        pool.cool_down("claude-a", std::time::Duration::from_secs(600));
        assert_eq!(chosen(&pool).as_deref(), Some("claude-b"));
        excluded.lock().unwrap().push("claude-b".into());
        assert_eq!(chosen(&pool).as_deref(), Some("claude-c"));
        pool.cool_down("claude-c", std::time::Duration::from_secs(600));
        assert_eq!(
            chosen(&pool).as_deref(),
            Some("claude-a"),
            "all resting: the first in the pool"
        );
        excluded
            .lock()
            .unwrap()
            .extend(["claude-a".into(), "claude-c".into()]);
        assert_eq!(chosen(&pool), None, "every account taken out: none");
    }

    /// A catalogue and a request spell a version differently often enough
    /// that comparing them raw would silently never match.
    #[test]
    fn a_dotted_request_matches_a_dashed_catalogue_entry() {
        let pool = Upstream::with_failover(vec![backend("chatgpt", &["gpt-5-6-luna"])]).unwrap();
        assert!(pool.for_model("gpt-5.6-luna").is_some());
        assert!(pool.for_model("GPT-5.6-LUNA").is_some());
    }

    /// A pool nobody gave catalogues to behaves exactly as it did before per
    /// model routing existed: the session's own backend serves everything.
    #[test]
    fn a_pool_that_declares_nothing_is_unchanged() {
        let pool = Upstream::with_failover(vec![backend("a", &[]), backend("b", &[])]).unwrap();
        assert_eq!(pool.serving_for(Some("anything-at-all")).provider(), "a");
        assert_eq!(pool.serving_for(None).provider(), "a");
        assert!(pool.for_model("anything-at-all").is_none());
    }

    /// A model no account declares falls back rather than failing: the
    /// request goes where it would have gone anyway.
    #[test]
    fn an_undeclared_model_falls_back_to_the_serving_backend() {
        let pool = Upstream::with_failover(vec![
            backend("claude-max", &["claude-opus-5"]),
            backend("chatgpt", &["gpt-6-astra"]),
        ])
        .unwrap();
        assert_eq!(pool.serving_for(Some("llama-9")).provider(), "claude-max");
    }

    /// Declaring nothing must never mean declaring everything, or a
    /// misconfigured account would swallow every request and the refusal
    /// would arrive from the provider instead of from configuration.
    #[test]
    fn an_empty_catalogue_claims_no_model() {
        let empty = backend("a", &[]);
        assert!(!empty.serves_model("claude-opus-5"));
    }
}

#[cfg(test)]
mod target_rule_tests {
    use super::*;

    /// A backend serving `protocol` at `target`, optionally declaring
    /// `models` — the shape a chat account (some models, `/v1/messages`) and
    /// a decision-only account (no models, `/systemone`) both need.
    fn backend_for(
        provider: &str,
        protocol: &str,
        registered_targets: &'static [&'static str],
        models: &[&str],
    ) -> UpstreamBackend {
        UpstreamBackend::new(
            provider.to_string(),
            vec![Route::new(
                protocol.to_owned(),
                registered_targets,
                "https://example.invalid",
            )],
            Secret::mint_for_test("sk-test-credential-value"),
            CredentialId::new(
                provider,
                crate::secret::SecretRef::Environment {
                    var: format!("{provider}_KEY"),
                },
            ),
            Cost::Metered,
        )
        .expect("a backend with one route")
        .with_models(models.iter().copied())
    }

    /// The rule this package exists for: bound to A (a chat account), a
    /// target only B (a decision-only account) claims is served by B, and
    /// a target A does claim stays with A.
    #[test]
    fn a_target_claimed_by_exactly_one_backend_is_served_even_when_the_session_is_bound_elsewhere()
    {
        let pool = Upstream::with_failover(vec![
            backend_for(
                "claude-max",
                "anthropic-messages",
                &["/messages"],
                &["claude-opus-5"],
            ),
            backend_for("typesafe", "typesafe-systemone", &["/systemone"], &[]),
        ])
        .unwrap();

        // Bound to claude-max (its model), /v1/systemone is claimed only by
        // typesafe.
        let serving = pool.serving_for_target(Some("claude-opus-5"), "/v1/systemone");
        assert_eq!(serving.provider(), "typesafe");

        // The same session's /v1/messages request still goes to claude-max —
        // the rule must not steal a target the model-chosen backend claims.
        let serving = pool.serving_for_target(Some("claude-opus-5"), "/v1/messages");
        assert_eq!(serving.provider(), "claude-max");
    }

    /// Two backends claiming the same target: today's behaviour holds
    /// unchanged, because ranking between two decision providers is not this
    /// method's call to make.
    #[test]
    fn two_claimants_keep_todays_behaviour() {
        let pool = Upstream::with_failover(vec![
            backend_for(
                "claude-max",
                "anthropic-messages",
                &["/messages"],
                &["claude-opus-5"],
            ),
            backend_for("typesafe-a", "typesafe-systemone", &["/systemone"], &[]),
            backend_for("typesafe-b", "typesafe-systemone", &["/systemone"], &[]),
        ])
        .unwrap();

        // claude-max is model-chosen and does not claim /v1/systemone; two
        // backends do, so the model-chosen one keeps serving it (and its own
        // route_for lookup then reports unrouted downstream, exactly as
        // before this rule existed).
        let serving = pool.serving_for_target(Some("claude-opus-5"), "/v1/systemone");
        assert_eq!(serving.provider(), "claude-max");
        assert!(serving.route_for("/v1/systemone").is_none());
    }

    /// No claimant at all: the model-chosen backend keeps serving, and it is
    /// never `None` — `serving_for_target` always returns a backend.
    #[test]
    fn no_claimant_falls_back_to_the_model_chosen_backend() {
        let pool = Upstream::with_failover(vec![backend_for(
            "claude-max",
            "anthropic-messages",
            &["/messages"],
            &["claude-opus-5"],
        )])
        .unwrap();
        let serving = pool.serving_for_target(Some("claude-opus-5"), "/v1/nothing-claims-this");
        assert_eq!(serving.provider(), "claude-max");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The three protocols the ingress serves, spelled here as the slugs and
    /// target prefixes `crate::profile` composes them from.
    ///
    /// Duplicated deliberately rather than imported: this module cannot name
    /// `crate::harness`, and `crate::profile`'s own
    /// `the_gateway_routes_every_protocol_its_ingress_declares` is what
    /// checks that these two spellings have not drifted apart.
    const ANTHROPIC: (&str, &[&str]) = ("anthropic-messages", &["/messages"]);
    const RESPONSES: (&str, &[&str]) = ("openai-responses", &["/responses"]);
    const CHAT: (&str, &[&str]) = ("openai-chat", &["/chat/completions"]);

    fn route((protocol, targets): (&str, &'static [&'static str]), base_url: &str) -> Route {
        Route::new(protocol.to_owned(), targets, base_url)
    }

    /// A credential identity for a test upstream: a provider name and a
    /// variable name, both names.
    fn test_credential_id(provider: &str) -> CredentialId {
        CredentialId::new(
            provider,
            crate::secret::SecretRef::Environment {
                var: format!("{}_API_KEY", provider.to_uppercase().replace('-', "_")),
            },
        )
    }

    fn upstream_with(routes: Vec<Route>) -> Result<Upstream, UpstreamError> {
        Upstream::new(
            "test-provider".to_owned(),
            routes,
            Secret::mint_for_test("sk-test-credential-value"),
            test_credential_id("test-provider"),
        )
    }

    fn upstream_at(base_url: &str) -> Result<Upstream, UpstreamError> {
        upstream_with(vec![route(ANTHROPIC, base_url)])
    }

    /// The upstream a multi-protocol test forwards through: one provider,
    /// three protocols, three visibly different base URLs.
    fn three_protocol_upstream() -> Upstream {
        upstream_with(vec![
            route(ANTHROPIC, "https://provider.example/anthropic"),
            route(RESPONSES, "https://provider.example/openai/v1"),
            route(CHAT, "https://provider.example/chat/v1"),
        ])
        .expect("three absolute https URLs")
    }

    fn uri_for(upstream: &Upstream, target: &str) -> Option<String> {
        let route = upstream.serving().route_for(target)?;
        Some(route.uri_for(target)?.to_string())
    }

    #[test]
    fn a_request_target_is_appended_to_the_declared_base_url_verbatim() {
        let upstream = upstream_at("https://openrouter.ai/api").expect("an absolute https URL");
        assert_eq!(
            uri_for(&upstream, "/v1/messages?beta=true").unwrap(),
            "https://openrouter.ai/api/v1/messages?beta=true"
        );
    }

    #[test]
    fn a_trailing_slash_on_the_base_url_does_not_double_up() {
        let upstream = upstream_at("https://openrouter.ai/api/").expect("an absolute https URL");
        assert_eq!(
            uri_for(&upstream, "/v1/messages").unwrap(),
            "https://openrouter.ai/api/v1/messages"
        );
    }

    /// The property line 1 and line 2 of Phase 9G are: a target reaches the
    /// base URL its **own** protocol declared, and not the one that happens
    /// to be first.
    #[test]
    fn each_protocols_target_reaches_that_protocols_own_base_url() {
        let upstream = three_protocol_upstream();
        for (target, expected) in [
            (
                "/v1/messages",
                "https://provider.example/anthropic/v1/messages",
            ),
            (
                "/v1/messages/count_tokens",
                "https://provider.example/anthropic/v1/messages/count_tokens",
            ),
            // Codex 0.149.1 pointed at a base URL with no path sends exactly
            // this, which is why the version segment cannot be required.
            ("/responses", "https://provider.example/openai/v1/responses"),
            (
                "/v1/responses",
                "https://provider.example/openai/v1/v1/responses",
            ),
            (
                "/chat/completions",
                "https://provider.example/chat/v1/chat/completions",
            ),
        ] {
            assert_eq!(
                uri_for(&upstream, target).as_deref(),
                Some(expected),
                "{target} was not routed to its own protocol's base URL"
            );
        }
    }

    /// The refusal half of the same property. A target that belongs to no
    /// served protocol must not be appended to whichever base URL came
    /// first — which is precisely what the single-upstream implementation
    /// this replaced would have done with every one of these.
    #[test]
    fn a_target_belonging_to_no_served_protocol_is_not_routed_anywhere() {
        let upstream = three_protocol_upstream();
        for target in [
            // Claude Code 2.1.245 really does send this, before its first
            // `/v1/messages` — observed against a recording listener.
            "/api/hello",
            "/v1/models",
            "/",
            "",
            // A prefix match that is not on a segment boundary.
            "/v1/messagesomethingelse",
            "/messagesomethingelse",
            // The version segment is stripped only when it *is* a segment.
            "/v1beta/messages",
            // Absolute-form targets are not origin-form and are not placed.
            "https://elsewhere.example/v1/messages",
        ] {
            assert!(
                upstream.serving().route_for(target).is_none(),
                "{target:?} was routed somewhere"
            );
        }
    }

    /// A gateway that serves one protocol places that protocol's targets and
    /// refuses everything else — the same rule, not a special case.
    #[test]
    fn a_single_protocol_upstream_places_only_its_own_targets() {
        let upstream = upstream_at("https://openrouter.ai/api").expect("an absolute https URL");
        assert_eq!(upstream.served_protocols(), vec!["anthropic-messages"]);
        assert!(upstream.serving().route_for("/v1/messages").is_some());
        assert!(upstream.serving().route_for("/responses").is_none());
        assert!(
            upstream
                .serving()
                .route_for("/v1/chat/completions")
                .is_none()
        );
    }

    #[test]
    fn an_upstream_with_no_route_at_all_is_refused_at_construction() {
        assert_eq!(
            upstream_with(Vec::new()).err(),
            Some(UpstreamError::NoProtocolServed {
                provider: "test-provider".to_owned()
            })
        );
    }

    #[test]
    fn a_base_url_that_is_not_an_absolute_http_url_is_refused_at_construction() {
        for base_url in ["", "openrouter.ai/api", "/api", "ftp://openrouter.ai"] {
            assert_eq!(
                upstream_at(base_url).err(),
                Some(UpstreamError::BaseUrlNotAbsolute {
                    provider: "test-provider".to_owned(),
                    protocol: "anthropic-messages".to_owned(),
                }),
                "accepted {base_url:?}"
            );
        }
    }

    /// Every route is checked, not just the first — otherwise a provider
    /// with a good Anthropic URL and a broken Responses one would bind a
    /// port and fail at the first Codex request instead of at start.
    #[test]
    fn a_broken_base_url_on_a_later_route_is_refused_too() {
        let broken = upstream_with(vec![
            route(ANTHROPIC, "https://provider.example/anthropic"),
            route(RESPONSES, "not-a-url"),
        ]);
        assert_eq!(
            broken.err(),
            Some(UpstreamError::BaseUrlNotAbsolute {
                provider: "test-provider".to_owned(),
                protocol: "openai-responses".to_owned(),
            })
        );
    }

    #[test]
    fn a_credential_that_could_inject_a_header_is_refused_at_construction() {
        let injected = Upstream::new(
            "test-provider".to_owned(),
            vec![route(ANTHROPIC, "https://openrouter.ai/api")],
            Secret::mint_for_test("value\r\nx-injected: yes"),
            test_credential_id("test-provider"),
        );
        assert_eq!(
            injected.err(),
            Some(UpstreamError::CredentialNotHeaderSafe {
                provider: "test-provider".to_owned()
            })
        );
    }

    /// The credential is reachable through the whole upstream, so the whole
    /// upstream has to be safe to render — and a `Debug` on the owner is
    /// exactly how a field gets printed by accident.
    #[test]
    fn debug_on_an_upstream_never_reaches_its_credential() {
        const VALUE: &str = "sk-planted-credential-qqqqwwwweeee";
        let upstream = Upstream::new(
            "test-provider".to_owned(),
            vec![route(ANTHROPIC, "https://openrouter.ai/api")],
            Secret::mint_for_test(VALUE),
            test_credential_id("test-provider"),
        )
        .expect("an absolute https URL");

        let rendered = format!("{upstream:?}");
        assert!(
            !rendered.contains(VALUE),
            "the credential survived into {rendered:?}"
        );
        assert!(
            rendered.contains(crate::secret::REDACTED),
            "the redaction marker must be shown rather than the field omitted: {rendered:?}"
        );
        // ... and the parts that are not secret are still there, or the
        // diagnostic would be useless and would get switched off.
        assert!(rendered.contains("test-provider"));
        assert!(rendered.contains("anthropic-messages"));
    }

    /// `http`'s own header rendering is the other place a value can escape,
    /// and it is one this module does not control — so the header is marked
    /// sensitive rather than trusted to stay unprinted.
    #[test]
    fn the_attached_authorization_header_renders_as_sensitive() {
        const VALUE: &str = "sk-planted-credential-qqqqwwwweeee";
        let upstream = Upstream::new(
            "test-provider".to_owned(),
            vec![route(ANTHROPIC, "https://openrouter.ai/api")],
            Secret::mint_for_test(VALUE),
            test_credential_id("test-provider"),
        )
        .expect("an absolute https URL");

        let header = upstream.serving().authorization();
        assert!(header.is_sensitive());
        let rendered = format!("{header:?}");
        assert!(
            !rendered.contains(VALUE),
            "the credential survived into {rendered:?}"
        );

        // ... while the value itself really is the credential, or nothing
        // above would be protecting anything.
        assert_eq!(header.as_bytes(), format!("Bearer {VALUE}").as_bytes());
    }

    /// One credential for every protocol: the same header goes out whichever
    /// route carried the request, because there is only one credential to
    /// attach. This is the observable consequence of the shape chosen in
    /// this module's header.
    #[test]
    fn every_route_forwards_with_the_one_credential_the_upstream_holds() {
        let upstream = three_protocol_upstream();
        assert_eq!(upstream.served_protocols().len(), 3);
        let attached = upstream.serving().authorization();
        assert!(attached.is_sensitive());
        assert_eq!(
            attached.as_bytes(),
            b"Bearer sk-test-credential-value".as_slice()
        );
    }

    #[test]
    fn the_upstream_host_is_a_host_and_never_a_path() {
        let upstream = upstream_at("https://openrouter.ai/api").expect("an absolute https URL");
        let route = upstream
            .serving()
            .route_for("/v1/messages")
            .expect("the anthropic route");
        assert_eq!(route.host(), "openrouter.ai");
        assert_eq!(route.protocol(), "anthropic-messages");
        assert_eq!(upstream.provider(), "test-provider");
    }
}
