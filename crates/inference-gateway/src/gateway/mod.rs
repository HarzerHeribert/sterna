//! The local Glasshouse gateway: the process, and now its protocol (Phase
//! 9G).
//!
//! The gateway is an **optional local proxy** — a transport, credential,
//! telemetry, reliability and backend-routing hop for requests that originate
//! in a real harness. It is never a coding harness, never an agent loop,
//! never the owner of an interactive session, and never a replacement for a
//! harness's own tools.
//!
//! That rule is **structural here rather than promised**. No file in this
//! directory imports `crate::session`, `crate::shell`, `crate::tui`,
//! `crate::harness`, `crate::profile` or `crate::events` — the launch one
//! because how a host *launches* a harness is not something a gateway
//! serving arbitrary HTTP clients can be allowed to see — nor names
//! `glasshouse::`, and
//! `tests::the_gateway_imports_none_of_the_modules_that_would_make_it_a_harness`
//! scans every one of them to keep it that way.
//!
//! **It also stores nothing a host would.** What this directory observes
//! leaves through one [`ObservationSink`] and is written down, if at all, on
//! the far side of a process boundary — `tests::the_gateway_names_no_glasshouse_path`
//! holds the whole crate to that, `rusqlite` included.
// History: design-decisions.md, "Trims: gateway, profile and provider module docs", gateway/mod.rs module doc.

mod context_limit;
mod http;
mod ingress;
mod request_model;
pub mod session;
pub mod subscription_broker;
pub mod translate;
pub mod upstream;
mod usage;

use std::fmt;
use std::io::{ErrorKind, Read};
use std::net::{Ipv4Addr, SocketAddr, TcpListener};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError, RwLock};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};

use crate::routing::evidence::NewObservation;
use crate::routing::free::FreeResource;
use crate::routing::interactive::Assignment;
use crate::secret::REDACTED;

pub use session::SessionRouting;
pub use upstream::{Route, Upstream, UpstreamBackend, UpstreamError};

/// What one client asks of this gateway — and the only thing the gateway
/// needs to know about the configuration a caller started that client from.
///
/// The gateway serves HTTP clients. How a client is launched, what program
/// it is, and what else its configuration says are concepts on the other
/// side of this door, and the gateway is a better component for being
/// unable to see them. The one question it has to ask is whether a client's
/// requests arrive *here*, because that answer alone decides whether a
/// listener is bound at all.
///
/// Two variants rather than three. A caller may well distinguish several
/// ways for a client to reach a backend without this gateway — its own
/// first-party account, a provider it talks to directly — but none of those
/// is a distinction the gateway can act on differently, and a variant that
/// could only ever be matched alongside another is one invented for the
/// type rather than read off behaviour.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackendDemand {
    /// This client reaches its backend itself. Nothing need be bound for it.
    Direct,
    /// This client's requests are to be served by this gateway.
    LocalGateway,
}

/// The name this gateway reports *itself* under to an [`ObservationSink`].
///
/// The gateway is one resource among the several a caller may be moving a
/// client between, and a sink is told which of them failed. This is the
/// spelling of this one. It is owned here because the gateway is the thing
/// being named: a caller asked to supply a name for it could supply a
/// different one on each of its start paths, and the failures recorded
/// against them would not add up.
///
/// Glasshouse's own records use the same string — `BackendResource::
/// GlasshouseGateway.slug()` in `crate::profile`, whose tests assert the
/// two spellings still agree.
pub const LOCAL_GATEWAY_RESOURCE: &str = "glasshouse-gateway";

/// Why a resource the gateway serves stopped being usable.
///
/// Three variants because three is what an exchange's outcome can actually
/// distinguish: nothing answered, something answered too late, something
/// answered badly. They are deliberately the same three a host records, so
/// the mapping across the boundary is total in both directions and cannot
/// lose or invent a reason.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DegradeReason {
    /// Nothing is listening, or the connection was refused.
    Unreachable,
    /// It accepted the request and never answered within the bound.
    TimedOut,
    /// It answered, and the answer was an error.
    Rejected,
}

impl DegradeReason {
    /// The reason in words — the spelling that crosses the process
    /// boundary, and the same three strings the host's own vocabulary uses.
    ///
    /// A method and deliberately **not** a `std::fmt` impl:
    /// `tests::a_gateway_token_has_no_display_no_deref_and_no_asref` forbids
    /// that trait's name anywhere in this file's production code, because
    /// [`GatewayToken`] lives here and a printable credential is how one
    /// reaches a log by accident. The rule costs this enum nothing.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Unreachable => "unreachable",
            Self::TimedOut => "timed out",
            Self::Rejected => "rejected",
        }
    }
}

/// What the gateway saw, stated in its own vocabulary so a host can act on
/// it without the gateway knowing what the host is.
///
/// Every field is something this directory already knows on its own: a
/// resource is the slug the gateway itself minted, a reason is one of this
/// module's own [`DegradeReason`]s. Nothing here is a host type, and nothing
/// may become one — see [`ObservationSink`].
///
/// One variant per thing this directory actually observes, and none for
/// anything it does not: a variant nobody emits is a shape every host has to
/// handle for no reason.
///
/// Not [`Eq`]: [`NewObservation`] carries optional readings that compare by
/// value and no more, and an equality nobody can rely on for hashing is not
/// worth deriving.
#[derive(Debug, Clone, PartialEq)]
pub enum Observation {
    /// A resource the gateway serves has become unusable — map line 1735,
    /// "detect gateway failure separately from harness process failure".
    Degraded {
        /// [`LOCAL_GATEWAY_RESOURCE`] for this gateway -- the name it
        /// mints for itself, owned rather than borrowed.
        resource: String,
        /// Which of the three failures it was.
        reason: DegradeReason,
    },
    /// One measurable turn finished — Phase 33A's producer, **reported
    /// outward instead of written down**.
    ///
    /// The gateway keeps no ledger and opens no database. What crosses the
    /// sink is a value in this crate's own vocabulary
    /// (`crate::routing::evidence`, whose whole point is that it names
    /// neither a database nor `rusqlite`), so a host that keeps a ledger
    /// writes the row on **its** side of the boundary and a standalone
    /// gateway drops it. Which of the two is listening is not observable
    /// from here — see [`ObservationSink`].
    Routed {
        /// Everything the exchange said about itself, as
        /// `session::SessionRouting::record_routing_observation` built it —
        /// a code span and not a link, because that producer is private to
        /// this module and this field is public.
        ///
        /// Boxed, the way `upstream::BackendCredential` boxes its own large
        /// variant: [`NewObservation`] carries thirty-odd fields and
        /// [`Observation::Degraded`] carries two, and an enum sized for the
        /// larger is moved at the size of the larger on every degrade too.
        observation: Box<NewObservation>,
        /// The wall-clock second the exchange completed, kept beside the
        /// observation because [`NewObservation`] deliberately carries no
        /// "when was this recorded" of its own — it is the recorder's fact,
        /// and the recorder is on the other side of the sink.
        observed_at_unix: i64,
    },
}

/// Where the gateway reports what it observed — one call per
/// [`Observation`], from the connection thread that observed it.
///
/// **The gateway runs as its own process, and a Rust closure cannot cross
/// that boundary.** This type is therefore the inside of the boundary, never
/// the transport across it. When a host is present, the closure the host
/// installs is an IPC emitter — a local socket, an HTTP post, an event
/// stream — that serialises the [`Observation`] and sends it; when no host is
/// present the sink is [`null_sink`], which drops everything. Those are the
/// only two shapes, and both are choices somebody made.
///
/// **The gateway must never import host types.** That is why an
/// [`Observation`] carries only this directory's own vocabulary, and why the
/// mapping into whatever a host records happens on the host's side of the
/// wire.
/// `tests::the_gateway_imports_none_of_the_modules_that_would_make_it_a_harness`
/// is what keeps it that way rather than a promise.
///
/// **The obvious "fix" is the defect.** Embedding the gateway back inside
/// the host process so this closure can call host code directly compiles,
/// passes, and silently undoes the separation the extraction exists for. It
/// is not a simplification of the IPC hop; it is the removal of the process
/// boundary that made the hop necessary.
///
/// A closure rather than a trait, which is what its predecessor
/// (`DegradeSink`) was and for the same reason: one call, no state, and
/// nothing an implementor would need that `Fn` does not already give.
pub type ObservationSink = Arc<dyn Fn(Observation) + Send + Sync>;

/// The sink a gateway with no host uses: every observation is dropped.
///
/// A first-class, named choice rather than the absence of one. A standalone
/// gateway — the extracted crate with no Glasshouse anywhere — observes
/// exactly what a hosted one observes; it simply has nowhere to report it,
/// and saying so with a sink keeps "nobody is listening" something somebody
/// decided rather than a `None` nobody noticed.
pub fn null_sink() -> ObservationSink {
    Arc::new(|_observation| {})
}

/// The only interface a Glasshouse gateway ever binds.
///
/// Named as a constant so that "loopback only" is one greppable fact rather
/// than a literal repeated at a call site and a test.
const GATEWAY_INTERFACE: Ipv4Addr = Ipv4Addr::LOCALHOST;

/// Ask the operating system for a free port.
///
/// This is what "multiple Glasshouse instances can coexist" actually rests
/// on: an instance that never names a port cannot collide with one that also
/// never names a port.
const EPHEMERAL_PORT: u16 = 0;

/// How much entropy the per-instance token carries.
///
/// 32 bytes — 256 bits — is the size at which guessing is not a strategy,
/// and it is the size a reader can recognise without having to do the
/// arithmetic.
const TOKEN_BYTES: usize = 32;

/// How long the accept loop sleeps between polls of the stop flag.
///
/// This is the whole shutdown mechanism's only cost, and the number is a
/// trade: it bounds how long dropping a [`Gateway`] can take, and it is how
/// often a thread wakes while a gateway-backed session is open. 25ms puts
/// shutdown well inside "immediately" for a human and costs forty wakeups a
/// second on a thread that does nothing else — see [`Gateway`] for why this
/// approach and not one of the alternatives.
const ACCEPT_POLL: Duration = Duration::from_millis(25);

/// A per-instance gateway authentication token.
///
/// Handed to child harnesses so that a request arriving at the gateway can be
/// shown to have come from *this* Glasshouse instance. It is minted fresh at
/// start, held only in memory, and dies with the instance: nothing writes it
/// to disk, so nothing can leave it behind.
///
/// Everything about this type mirrors [`crate::secret::Secret`], deliberately
/// and item for item: no `Display`, no `Deref`, no `AsRef<str>`, no
/// `Clone`, no serde, and a manual [`Debug`](std::fmt::Debug) rendering
/// [`crate::secret::REDACTED`]. The only way out is [`GatewayToken::expose`],
/// whose name is the point.
///
/// It is not *itself* a [`crate::secret::Secret`] only because that type's
/// field is private to its own module, so nothing outside `crate::secret`
/// can mint one — see this module's report for that finding. Widening that
/// module's API to make this possible was not this slice's call to make.
pub struct GatewayToken(String);

impl GatewayToken {
    /// 32 bytes from the operating system's cryptographic generator,
    /// rendered as hex.
    ///
    /// `getrandom` rather than a hand-rolled read of `/dev/urandom`,
    /// `getrandom(2)`, `BCryptGenRandom` and whatever the next platform
    /// wants: entropy is the one place where the failure mode of "it
    /// silently returned something predictable on the platform nobody
    /// tested" is unrecoverable, and that is exactly what a hand-rolled
    /// version gets wrong.
    ///
    /// The error is propagated rather than swallowed with a fallback. A
    /// gateway that started with a guessable token would be worse than one
    /// that refused to start.
    fn generate() -> Result<Self> {
        let mut bytes = [0u8; TOKEN_BYTES];
        getrandom::fill(&mut bytes)
            .context("could not read cryptographic randomness for the gateway token")?;
        Ok(Self(hex::encode(bytes)))
    }

    /// Hand the token to something that genuinely needs it — the environment
    /// of a child harness, and the ingress's own comparison against it.
    ///
    /// Every call site is a place the token leaves this type, so each one
    /// should be short-lived, obvious, and easy to count. There are two:
    /// `profile::resolve_with_gateway`, which writes it into one
    /// child process's environment, and `ingress`'s check that an arriving
    /// request carries it.
    pub fn expose(&self) -> &str {
        &self.0
    }
}

/// Prints [`crate::secret::REDACTED`] and nothing else.
///
/// Not derived, and not a prefix, a suffix or a length either: each of those
/// narrows the space an attacker has to search, so this rendering is
/// identical for every token.
impl fmt::Debug for GatewayToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(REDACTED)
    }
}

/// A running local gateway: a bound loopback listener, the token that
/// authenticates against it, the upstream it forwards to, and the thread
/// that accepts.
///
/// `accept` blocks, and a blocked `accept` cannot be interrupted portably.
/// Dropping a `Gateway` must still return promptly, so the listener is set
/// **non-blocking** and the accept thread polls a stop flag every
/// `ACCEPT_POLL` (25ms), then joins.
/// History: design-decisions.md, "Trims: gateway/mod.rs", Gateway struct doc.
///
/// The consequence that *is* platform-specific is handled where it lands: on
/// Windows and the BSDs (including macOS) an accepted socket inherits the
/// listener's non-blocking flag, while on Linux it does not — so `ingress`
/// clears it on every accepted stream rather than assuming.
///
/// Dropping this value stops the accept loop, joins its thread, and with it
/// drops the listener, which releases the port. That covers a normal return
/// and an unwinding panic alike.
///
/// In-flight connection threads are **not** joined.
/// What this gateway forwards to — or, until it can, why it cannot.
///
/// The invariant: **a gateway listens before it has an upstream, and a
/// request never waits on one being built.** A standalone gateway started
/// with no credential anywhere would otherwise refuse to start, and the one
/// flow that stores a credential — the client's own login control — runs
/// through a session that needs the gateway listening first. So a slot may
/// start empty; the first request asks its supplier again at once, a
/// refused rebuild stands for `REBUILD_INTERVAL` so a burst does not
/// rebuild once each, and every refused request is answered `503` with the
/// refusal that stands.
/// A host's gateway is never deferred: every `start_if_required_*` door
/// fills the slot before the listener accepts.
///
/// A serving slot may also be told how to **reload** ([`Gateway::reload_when`]):
/// when its `changed` check says the configuration moved, the next request
/// rebuilds the pool from it. A rebuild that fails keeps the running pool --
/// a bad edit never takes a working session down.
pub struct UpstreamSlot {
    current: RwLock<Option<Arc<Upstream>>>,
    supplier: Option<Supplier>,
    reload: std::sync::OnceLock<Reload>,
    /// The last refusal and when it was made, so a burst of requests does
    /// not rebuild the pool once each. Held across a rebuild, which
    /// serialises requests only while there is nothing to serve them with.
    last_refusal: Mutex<(String, Option<Instant>)>,
    rebuild_interval: Duration,
}

type Supplier = Box<dyn Fn() -> Result<Upstream, String> + Send + Sync>;

/// How a serving slot rebuilds: the supplier, and a check that answers true
/// once for each change to what the supplier reads.
struct Reload {
    supplier: Supplier,
    changed: Box<dyn Fn() -> bool + Send + Sync>,
}

/// How long a refused rebuild stands before a request tries again.
const REBUILD_INTERVAL: Duration = Duration::from_secs(1);

impl fmt::Debug for UpstreamSlot {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.current() {
            Some(upstream) => fmt::Debug::fmt(&upstream, f),
            None => f.write_str("<no upstream yet>"),
        }
    }
}

impl UpstreamSlot {
    fn ready(upstream: Upstream) -> Self {
        Self {
            current: RwLock::new(Some(Arc::new(upstream))),
            supplier: None,
            reload: std::sync::OnceLock::new(),
            last_refusal: Mutex::new((String::new(), None)),
            rebuild_interval: REBUILD_INTERVAL,
        }
    }

    /// Empty, carrying the start's own refusal for a request that arrives
    /// before any rebuild — unstamped, so the first request rebuilds at
    /// once: a key stored right after the start must serve the very next
    /// turn, however soon it comes.
    fn deferred(refusal: String, supplier: Supplier) -> Self {
        Self {
            current: RwLock::new(None),
            supplier: Some(supplier),
            reload: std::sync::OnceLock::new(),
            last_refusal: Mutex::new((refusal, None)),
            rebuild_interval: REBUILD_INTERVAL,
        }
    }

    #[cfg(test)]
    fn with_rebuild_interval(mut self, interval: Duration) -> Self {
        self.rebuild_interval = interval;
        self
    }

    /// The upstream, if this gateway has one. Never builds.
    pub fn current(&self) -> Option<Arc<Upstream>> {
        self.current
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// The upstream, building it when the slot is empty and the last
    /// refusal is older than the interval. `Err` is what the request is
    /// told.
    fn current_or_build(&self) -> Result<Arc<Upstream>, String> {
        if let Some(reload) = self.reload.get()
            && self.current().is_some()
        {
            let _one_at_a_time = self
                .last_refusal
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            if (reload.changed)() {
                match (reload.supplier)() {
                    Ok(upstream) => {
                        *self.current.write().unwrap_or_else(PoisonError::into_inner) =
                            Some(Arc::new(upstream));
                    }
                    Err(reason) => {
                        eprintln!(
                            "the configuration changed and does not serve ({reason}); the running pool stays"
                        );
                    }
                }
            }
        }
        if let Some(upstream) = self.current() {
            return Ok(upstream);
        }
        let Some(supplier) = &self.supplier else {
            return Err("this gateway has no upstream".to_owned());
        };
        let mut last = self
            .last_refusal
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        // Built by the thread that held the lock while this one waited.
        if let Some(upstream) = self.current() {
            return Ok(upstream);
        }
        if let (reason, Some(at)) = &*last
            && at.elapsed() < self.rebuild_interval
        {
            return Err(reason.clone());
        }
        match supplier() {
            Ok(upstream) => {
                let upstream = Arc::new(upstream);
                *self.current.write().unwrap_or_else(PoisonError::into_inner) =
                    Some(Arc::clone(&upstream));
                Ok(upstream)
            }
            Err(reason) => {
                *last = (reason.clone(), Some(Instant::now()));
                Err(reason)
            }
        }
    }
}

impl Gateway {
    /// Rebuild this gateway's pool with `supplier` whenever `changed` says
    /// what it reads has moved -- a sign-in that declares an account, a
    /// custom endpoint added -- with no restart. Set once; a second call is
    /// ignored.
    pub fn reload_when(
        &self,
        supplier: impl Fn() -> Result<Upstream, String> + Send + Sync + 'static,
        changed: impl Fn() -> bool + Send + Sync + 'static,
    ) {
        let _ = self.upstream.reload.set(Reload {
            supplier: Box::new(supplier),
            changed: Box::new(changed),
        });
    }
}

#[derive(Debug)]
pub struct Gateway {
    address: SocketAddr,
    token: Arc<GatewayToken>,
    /// Shared with the accept loop rather than moved into it, so that a
    /// launch profile can ask what this gateway actually serves. Its
    /// [`Debug`](fmt::Debug) renders the credential's redaction marker, not
    /// the credential — see [`Upstream`]. Empty only for a deferred start —
    /// see [`UpstreamSlot`].
    upstream: Arc<UpstreamSlot>,
    /// Which backend is serving this session, what has moved it, and what
    /// real work has said about each resource — Phase 9H and Phase 9I.
    ///
    /// Shared with every connection thread rather than owned by the accept
    /// loop, because a launch profile binds the assignment into it from the
    /// main thread while connection threads observe into it. It holds no
    /// credential value: an assignment names a credential, and
    /// [`crate::routing::CredentialId`] is two names.
    routing: Arc<SessionRouting>,
    /// Set by [`Drop`]; read by the accept loop every [`ACCEPT_POLL`].
    stop: Arc<AtomicBool>,
    /// `None` only after [`Drop`] has taken it.
    accept: Option<JoinHandle<()>>,
}

impl Gateway {
    /// Bind the listener, mint the token, and start accepting, with no
    /// durable quota cache — capability map line 1229's gateway half stays
    /// in memory only, exactly as before this package.
    ///
    /// Private on purpose. [`start_if_required`] is the only way in from
    /// outside this module, which is what stops a gateway from being started
    /// by a caller that simply decided to, rather than by a profile that
    /// asked for one.
    fn start(upstream: Upstream) -> Result<Self> {
        Self::start_with_quota_cache(upstream, None)
    }

    /// [`Self::start`], with a [`crate::provider::telemetry::GatewayQuotaCache`]
    /// a real forwarded exchange's rate-limit headers are persisted to —
    /// capability map lines 1217/1218/1229's bridge across the process
    /// boundary between this gateway and a later `glasshouse resources`
    /// invocation.
    ///
    /// `None` reproduces [`Self::start`] exactly. No caller resolves
    /// [`crate::paths::RuntimePaths::resolve`] here, and none may be added:
    /// this module has never had a project or a data directory in scope. A
    /// caller that wants persistence resolves its own
    /// [`crate::paths::RuntimePaths`] and hands this a
    /// [`crate::provider::telemetry::GatewayQuotaCache::new`] built from it.
    /// History: design-decisions.md, "Trims: gateway/mod.rs", start_with_quota_cache doc.
    ///
    /// Private on purpose, exactly as [`Self::start`] is: reached from
    /// outside this module only through [`start_if_required_with_quota_cache`].
    fn start_with_quota_cache(
        upstream: Upstream,
        quota_cache: Option<crate::provider::telemetry::GatewayQuotaCache>,
    ) -> Result<Self> {
        Self::start_with_telemetry(upstream, quota_cache, None)
    }

    /// [`Self::start_with_quota_cache`], with a
    /// [`crate::provider::telemetry::GatewayHealthCache`] every real
    /// forwarded exchange's resource health is persisted to — capability map
    /// lines 1311/1321/1322/1324's bridge across the process boundary between
    /// this gateway and a later `glasshouse resources` invocation, additive
    /// the same way the quota cache is: `None` reproduces
    /// [`Self::start_with_quota_cache`] exactly.
    ///
    /// **Both caches are the gateway's own.** They hold what this process
    /// measured — headers off its own responses, health off its own free
    /// pool — and nothing a host observed. There is no third parameter for a
    /// host's ledger and there may not be one: a turn this gateway measures
    /// leaves as an [`Observation`], through the sink.
    /// History: design-decisions.md, "Trims: gateway/mod.rs", start_with_telemetry doc.
    ///
    /// Private on purpose: reached from outside this module only through
    /// [`start_if_required_with_telemetry`].
    fn start_with_telemetry(
        upstream: Upstream,
        quota_cache: Option<crate::provider::telemetry::GatewayQuotaCache>,
        health_cache: Option<crate::provider::telemetry::GatewayHealthCache>,
    ) -> Result<Self> {
        Self::start_with_degrade_sink(upstream, quota_cache, health_cache, None, None)
    }

    /// [`Self::start_with_telemetry`], with the [`ObservationSink`] every
    /// [`Observation`] this gateway makes is reported through — a resource
    /// that went unusable (map line 1735) and every measurable turn (Phase
    /// 33A) alike.
    ///
    /// **This is the only way anything leaves.** `None` is a gateway nobody
    /// is listening to, which reproduces [`Self::start_with_telemetry`]
    /// exactly and is what a standalone run does when it has not even
    /// bothered to name [`null_sink`]. It is not a way to keep a second
    /// channel: there is no ledger handle, no database and no host type on
    /// this path, and adding one would be the embedding
    /// [`ObservationSink`]'s own documentation refuses.
    ///
    /// The name still says `degrade_sink` because
    /// [`start_if_required_with_degrade_sink`] is what the host's own scans
    /// name; the parameter it forwards is `observation_sink`, which is what
    /// it now carries.
    /// History: design-decisions.md, "Trims: gateway/mod.rs", start_with_telemetry doc.
    ///
    /// Private on purpose, exactly as [`Self::start_with_telemetry`] is:
    /// reached from outside this module only through
    /// [`start_if_required_with_degrade_sink`].
    fn start_with_degrade_sink(
        upstream: Upstream,
        quota_cache: Option<crate::provider::telemetry::GatewayQuotaCache>,
        health_cache: Option<crate::provider::telemetry::GatewayHealthCache>,
        observation_sink: Option<ObservationSink>,
        prevention_sink: Option<session::FailoverPreventionSink>,
    ) -> Result<Self> {
        Self::start_slot(
            UpstreamSlot::ready(upstream),
            quota_cache,
            health_cache,
            observation_sink,
            prevention_sink,
        )
    }

    /// The one body every start shares: bind, mint, accept. Takes the slot
    /// rather than an upstream so a deferred start ([`start_awaiting_upstream`])
    /// is the same code path with an empty slot, not a second listener.
    fn start_slot(
        upstream: UpstreamSlot,
        quota_cache: Option<crate::provider::telemetry::GatewayQuotaCache>,
        health_cache: Option<crate::provider::telemetry::GatewayHealthCache>,
        observation_sink: Option<ObservationSink>,
        prevention_sink: Option<session::FailoverPreventionSink>,
    ) -> Result<Self> {
        let listener = TcpListener::bind((GATEWAY_INTERFACE, EPHEMERAL_PORT))
            .context("could not bind the local gateway to loopback")?;
        // Port 0 was a request, not an address. This is the answer, and it is
        // the only place the real port ever comes from.
        let address = listener
            .local_addr()
            .context("could not read the local gateway's bound address")?;
        listener
            .set_nonblocking(true)
            .context("could not put the local gateway's listener in polling mode")?;

        let token = Arc::new(GatewayToken::generate()?);
        let stop = Arc::new(AtomicBool::new(false));
        let upstream = Arc::new(upstream);
        let routing = Arc::new(SessionRouting::new());
        let quota_cache = quota_cache.map(Arc::new);
        let health_cache = health_cache.map(Arc::new);

        let accept = std::thread::Builder::new()
            .name("glasshouse-gateway-accept".to_owned())
            .spawn({
                let token = Arc::clone(&token);
                let stop = Arc::clone(&stop);
                let upstream = Arc::clone(&upstream);
                let routing = Arc::clone(&routing);
                let quota_cache = quota_cache.clone();
                let health_cache = health_cache.clone();
                let observation_sink = observation_sink.clone();
                let prevention_sink = prevention_sink.clone();
                move || {
                    accept_loop(
                        listener,
                        stop,
                        token,
                        upstream,
                        routing,
                        quota_cache,
                        health_cache,
                        observation_sink,
                        prevention_sink,
                    )
                }
            })
            .context("could not start the local gateway's accept thread")?;

        Ok(Self {
            address,
            token,
            upstream,
            routing,
            stop,
            accept: Some(accept),
        })
    }

    /// Where this gateway is listening: always loopback, always the port the
    /// operating system chose.
    pub fn address(&self) -> SocketAddr {
        self.address
    }

    /// The base URL a child harness is pointed at.
    ///
    /// A root with no path: Claude Code appends `/v1/messages` to whatever
    /// it is given — see `crate::harness::claude_code`'s `BASE_URL_ENV`,
    /// where that was observed on a real binary — and this gateway appends
    /// whatever arrives to the provider's own root in turn.
    pub fn base_url(&self) -> String {
        format!("http://{}", self.address)
    }

    /// This instance's authentication token.
    pub fn token(&self) -> &GatewayToken {
        &self.token
    }

    /// The slug of every wire protocol this gateway's ingress can actually
    /// carry.
    ///
    /// Not the same list as `profile::GATEWAY_INGRESS_PROTOCOLS`,
    /// and that difference is the point: that constant says what the ingress
    /// *knows how to serve*, while this says what the one configured
    /// provider declared a base URL for. A launch profile has to refuse
    /// against the second, or a harness would be started against an ingress
    /// that would answer its first request with a `404`.
    ///
    /// Slugs rather than a protocol enum because no file in this directory
    /// may name `harness` — see this module's header. The
    /// caller that reads them is `crate::profile`, which can.
    pub fn served_protocols(&self) -> Vec<String> {
        self.upstream
            .current()
            .map(|upstream| {
                upstream
                    .served_protocols()
                    .into_iter()
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Which backend is serving this session, and everything that has moved
    /// it — Phase 9H.
    ///
    /// The gateway holds this rather than owning any of the decisions in it:
    /// [`mod@crate::routing::interactive`] decides, [`session`] applies, and
    /// this is where a launch profile and a settings screen reach both.
    pub fn routing(&self) -> &SessionRouting {
        &self.routing
    }

    /// The most recent rate-limit headers a real forwarded response carried,
    /// and when they were observed — capability map line 1229's gateway
    /// half, the `ingress` module's own reading, passed through unread
    /// except for the allowlist [`crate::provider::telemetry`] already
    /// parses.
    ///
    /// `None` until this gateway has forwarded at least one request whose
    /// response carried a rate-limit header this reader understands. A
    /// passive reader: nothing here makes a request of its own, ever — see
    /// the module documentation's "the gateway forwards headers without
    /// reading them" history, now narrowed to the body.
    pub fn quota_headers(&self) -> Option<(crate::provider::telemetry::RateLimitHeaders, i64)> {
        self.routing.quota_headers()
    }

    /// The upstream this gateway forwards through, for a caller that needs to
    /// name one of its backends — a migration, or a settings screen listing
    /// what a session could move to.
    ///
    /// No credential comes out with it: [`Upstream`] has no accessor for one.
    /// `None` only while a deferred start has nothing to forward to.
    pub fn upstream(&self) -> Option<Arc<Upstream>> {
        self.upstream.current()
    }

    /// The name of the provider this gateway is currently forwarding to —
    /// map line 1954's gateway shape: a launch whose client demanded
    /// [`BackendDemand::LocalGateway`] does not know
    /// which entitlement it will charge until this gateway has resolved and
    /// started its upstream, and this is the one fact the launch path needs
    /// to ask `EffectiveConfig::entitlement_for_provider` the same question
    /// the direct-provider path asks before it ever exists.
    ///
    /// Delegates to `Upstream::serving`, which stays `pub(super)`: nothing
    /// else about the upstream — its routes, its credential — is exposed
    /// here or anywhere outside this module.
    pub fn serving_provider(&self) -> Option<String> {
        self.upstream
            .current()
            .map(|upstream| upstream.serving().provider().to_owned())
    }
}

/// Stop accepting, join, and release the port.
impl Drop for Gateway {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(accept) = self.accept.take() {
            // Joining is what makes the port released *by the time this
            // returns*: the listener is owned by the loop's stack frame, so
            // it is dropped when that frame unwinds and not before.
            let _ = accept.join();
        }
    }
}

/// Accept until told to stop, giving each connection a thread.
///
/// Errors other than [`ErrorKind::WouldBlock`] are transient by the time
/// anything can be done about them — a descriptor limit reached, a
/// connection reset between the handshake and the accept — so the loop
/// sleeps and tries again rather than dying and leaving a bound port with
/// nothing behind it.
///
/// Eight parameters, all of them either identity (`listener`, `token`), the
/// three coordination handles (`stop`, `upstream`, `routing`) already threaded
/// through before this package, or one of three additive, independently
/// optional telemetry destinations — two caches this process writes for its
/// own next run, and the one sink everything it observes leaves by. Grouping
/// them into a struct would trade one clippy lint for an abstraction with a
/// single call site and nothing else to say about itself.
#[allow(clippy::too_many_arguments)]
fn accept_loop(
    listener: TcpListener,
    stop: Arc<AtomicBool>,
    token: Arc<GatewayToken>,
    upstream: Arc<UpstreamSlot>,
    routing: Arc<SessionRouting>,
    quota_cache: Option<Arc<crate::provider::telemetry::GatewayQuotaCache>>,
    health_cache: Option<Arc<crate::provider::telemetry::GatewayHealthCache>>,
    observation_sink: Option<ObservationSink>,
    prevention_sink: Option<session::FailoverPreventionSink>,
) {
    // One agent for the life of the gateway: it owns the connection pool to
    // the provider, so a warm TLS connection survives from one request to
    // the next. Built here rather than per connection, which would throw
    // that away every time.
    let agent = Arc::new(upstream::agent());

    while !stop.load(Ordering::Relaxed) {
        match listener.accept() {
            Ok((stream, _peer)) => {
                let token = Arc::clone(&token);
                let slot = Arc::clone(&upstream);
                let agent = Arc::clone(&agent);
                let routing = Arc::clone(&routing);
                let quota_cache = quota_cache.clone();
                let health_cache = health_cache.clone();
                let observation_sink = observation_sink.clone();
                let prevention_sink = prevention_sink.clone();
                let spawned = std::thread::Builder::new()
                    .name("glasshouse-gateway-exchange".to_owned())
                    .spawn(move || {
                        // Nothing to forward to yet: say so and record
                        // nothing — see `UpstreamSlot`.
                        let upstream = match slot.current_or_build() {
                            Ok(upstream) => upstream,
                            Err(reason) => {
                                ingress::refuse_unserved(stream, &token, &reason);
                                return;
                            }
                        };
                        // Phase 33A's own honest caveat, named where it is
                        // stamped rather than only in `routing::evidence`'s
                        // doc: this is the instant the connection was handed
                        // to `ingress::serve`, not the instant a request left
                        // for the provider — the true dispatch instant lives
                        // inside `ingress::forward`, outside this partition.
                        let dispatched_at = crate::provider::cache::now_unix_seconds();
                        // The assignment as of the same instant, so a bind or
                        // re-bind that lands while this exchange is in
                        // flight cannot be attributed to it — see
                        // `SessionRouting::record_routing_observation`'s own
                        // doc for the defect this snapshot closes.
                        let dispatched_assignment = routing.assignment();
                        // Capability map line 1368: consult the cooldown this
                        // very loop already recorded before spending an
                        // upstream request on a route whose declared cadence
                        // says the request will predictably fail.
                        // `observe_exchange` below only ever runs *after* an
                        // exchange completes, so without this check the
                        // resource it just cooled down stays cooled down in
                        // the pool while the very next connection dials it
                        // anyway.
                        if let Some(wait) =
                            paced_refusal(&routing, &upstream, dispatched_assignment.as_ref())
                        {
                            refuse_paced(stream, wait);
                            return;
                        }
                        let (exchange, quota) = ingress::serve(stream, &token, &upstream, &agent);
                        // The exchange is genuinely over here: every byte of
                        // the response has been relayed. Stamped before
                        // anything below it so nothing added later can push
                        // this reading later than the real completion.
                        let completed_at = crate::provider::cache::now_unix_seconds();
                        // Phase 9H and 9I's production feed. After the
                        // exchange, so the routing lock is never held across
                        // the provider hop, and before the log line, so a
                        // failover the exchange caused is already recorded
                        // when its own record is read.
                        //
                        // It reads no store. Everything the failover ranking
                        // needs — health, quota, cache locality, stickiness,
                        // the failure domain — this loop measured itself, and
                        // the observations it produces travel outward through
                        // `observation_sink` rather than into anything this
                        // side can read back.
                        let observed_at_instant = std::time::Instant::now();
                        // Capability map line 1319's missing wire. `quota` is
                        // this very response's own rate-limit headers, bound
                        // seventeen lines above and — before this — used only
                        // for capacity telemetry further down. A provider that
                        // answered `429` and said how long to wait has stated
                        // a temporary scheduling block, and
                        // `routing::free::ResourceHealth::fail` treats a
                        // stated wait as authoritative rather than as one more
                        // failure to count. `session::stated_retry_after`
                        // narrows the headers to that one duration; nothing
                        // else from them travels into a routing decision.
                        // What this exchange did to the assignment is kept
                        // for its own evidence row below — capability map
                        // line 1334's `failovers`, known here and nowhere
                        // else, because this is the thread that decided it.
                        let effect = routing.observe_exchange(
                            &upstream,
                            &exchange,
                            observed_at_instant,
                            session::stated_retry_after(&quota),
                            // Capability map line 1851's write side. `None`
                            // reproduces this loop's behaviour exactly as it
                            // was before this package, the same additive
                            // shape every other sink here follows.
                            prevention_sink.as_ref(),
                        );
                        // Map line 1735: detect a gateway failure separately
                        // from a harness process failure. `session::classify`
                        // above already folded this exchange into routing
                        // health and failover; this is the same exchange
                        // asked the opposite-consequence question — not "does
                        // this session need to move", but "is the resource
                        // itself unhealthy" — and answered without touching
                        // any session's lifecycle, because nothing here calls
                        // anything that could.
                        //
                        // `session::gateway_failure` answers in
                        // [`DegradeReason`] — this module's own word, not a
                        // host's — so no line here names a host type and the
                        // conversion hop this used to make is gone.
                        if let Some(reason) = session::gateway_failure(&exchange)
                            && let Some(sink) = &observation_sink
                        {
                            sink(Observation::Degraded {
                                resource: LOCAL_GATEWAY_RESOURCE.to_owned(),
                                reason,
                            });
                        }
                        // Phase 33A's production producer — see
                        // `crate::gateway::session::SessionRouting::record_routing_observation`
                        // for exactly what this can and cannot supply. It
                        // reports through the same sink the degrade above
                        // uses, because that is the only way out of this
                        // process; a host writes the row, and a gateway with
                        // no host does not build one at all.
                        //
                        // `quota` is borrowed here for line 1364/1365's
                        // throttle-versus-exhausted-quota reading and moved
                        // into `observe_quota_headers` below unchanged; see
                        // `session::ExchangeReading::quota` for why this
                        // borrow is a record and not a routing decision.
                        if let Some(sink) = &observation_sink {
                            routing.record_routing_observation(
                                sink,
                                &exchange,
                                session::ExchangeReading {
                                    quota: &quota,
                                    dispatched_at_unix: dispatched_at,
                                    completed_at_unix: completed_at,
                                    assignment: dispatched_assignment,
                                    effect,
                                },
                            );
                        }
                        // Capability map line 1229's gateway half — a passive
                        // reader, not a prober: this fires only when a real
                        // session actually forwards a request through this
                        // gateway, and `observe_quota_headers` itself is the
                        // one place `is_empty()` is checked, so an ordinary
                        // exchange that carried no rate-limit header is a
                        // silent no-op rather than a cleared reading.
                        let now = crate::provider::cache::now_unix_seconds();
                        // The durable half of the same reading — capability
                        // map lines 1217/1218/1229's bridge across the
                        // process boundary, see
                        // `GatewayQuotaCache::store`'s own doc. `exchange`
                        // already names the configured provider this
                        // response came from; nothing else in this crate
                        // knows that at the point a reading is captured.
                        if let Some(cache) = &quota_cache {
                            cache.store(&exchange.provider, &quota, now);
                            // The route answering for itself. A provider
                            // refusing an over-long request states the window
                            // it enforces, and that figure belongs to this
                            // account on this route rather than to the model
                            // as some catalogue describes it
                            // (`archive/glasshouse:docs/product/design-decisions.md`, *A context
                            // window is a property of the route, not of the
                            // model*). Written beside the quota reading
                            // because it is the same kind of fact, observed
                            // at the same moment, by the same loop.
                            if let Some(tokens) = exchange.context_limit_tokens
                                && let Some(model) = exchange.requested_model.as_deref()
                            {
                                cache.context_limits().store(
                                    &exchange.provider,
                                    model,
                                    tokens,
                                    now,
                                );
                            }
                        }
                        routing.observe_quota_headers(quota, now);
                        // Capability map lines 1311/1321/1322/1324's gateway
                        // half, symmetric with the quota write immediately
                        // above rather than folded into `observe_exchange`
                        // itself: the health this exchange just updated is
                        // read back out of `routing` (already mutated by
                        // `observe_exchange` above) and persisted for
                        // whichever provider this exchange named.
                        if let Some(cache) = &health_cache {
                            let readings = routing.health_readings_for(
                                &exchange.provider,
                                observed_at_instant,
                                now,
                            );
                            cache.store(&exchange.provider, &readings, now);
                        }
                        exchange.record();
                    });
                if spawned.is_err() {
                    // No thread to serve it: the connection closes as the
                    // stream drops. Better than blocking the accept loop.
                    tracing::debug!("the gateway could not start a connection thread");
                }
            }
            Err(err) if err.kind() == ErrorKind::WouldBlock => std::thread::sleep(ACCEPT_POLL),
            Err(_) => std::thread::sleep(ACCEPT_POLL),
        }
    }
}

/// The wait to refuse this connection with, if the resource the session is
/// currently assigned to is still inside a wait its provider itself declared
/// — capability map line 1368. `None` means the accept loop should serve
/// normally.
///
/// **Deliberately narrower than [`FreePool::is_available`].** That check
/// folds two kinds of cooldown into one bool: a provider's own declared wait
/// (line 1319 makes this authoritative) and a bounded cooldown Glasshouse
/// invents after ordinary repeated failures, which real work must still be
/// allowed to probe (Phase 9I line 534,
/// `gateway::conformance::a_pinned_session_stays_on_its_failing_provider_and_never_reaches_the_other_one`).
/// Only the first kind is what line 1368 asks to stop retrying in place, so
/// this reads the most recent rate-limit headers this gateway observed
/// rather than trusting the pool's bool for why it is `false`.
/// History: design-decisions.md, "Trims: gateway/mod.rs", paced_refusal doc.
///
/// A sibling credential is still offered the chance to serve first; actually
/// rotating to it is [`session::SessionRouting::observe_exchange`]'s own job,
/// so this never mutates the assignment itself.
fn paced_refusal(
    routing: &SessionRouting,
    upstream: &Upstream,
    assignment: Option<&Assignment>,
) -> Option<Duration> {
    let assignment = assignment?;
    let resource = FreeResource::new(
        assignment.backend().credential().clone(),
        assignment.backend().model().label(),
    );
    let now = std::time::Instant::now();
    let pool = routing.free_pool();
    if pool.is_available(&resource, now) {
        return None;
    }
    let (headers, observed_at_unix) = routing.quota_headers()?;
    let declared_seconds = headers.retry_after_seconds()?;
    let now_unix = crate::provider::cache::now_unix_seconds();
    let remaining = observed_at_unix + declared_seconds - now_unix;
    if remaining <= 0 {
        // The declared wait has already elapsed; `is_available` will catch
        // up once a real exchange observes it, and nothing here should
        // refuse a request the provider never asked to wait on any more.
        return None;
    }
    let siblings = upstream.credentials_of(assignment.provider());
    if pool
        .rotate_from(resource.credential(), &siblings, resource.model(), now)
        .is_some()
    {
        return None;
    }
    Some(Duration::from_secs(remaining as u64))
}

/// Answer `429` on `stream` without dialling upstream at all — capability
/// map line 1368's refusal. `wait` is the provider-declared wait
/// [`paced_refusal`] read back from [`SessionRouting::quota_headers`],
/// carried back as this gateway's own `Retry-After` rather than a fabricated
/// header.
fn refuse_paced(mut stream: std::net::TcpStream, wait: Duration) {
    // Drained before responding, the same reason `ingress::settle` drains
    // before closing a refusal there: closing a socket with the client's
    // own bytes still unread resets the connection instead of ending it
    // cleanly, and the harness would see a network error instead of this
    // response's 429.
    let _ = stream.set_nonblocking(false);
    let _ = stream.set_read_timeout(Some(Duration::from_millis(500)));
    let mut drained = [0u8; 8192];
    let _ = stream.read(&mut drained);

    let headers = vec![
        ("connection".to_owned(), b"close".to_vec()),
        (
            "retry-after".to_owned(),
            wait.as_secs().to_string().into_bytes(),
        ),
    ];
    let _ = http::write_head(
        &mut stream,
        ureq::http::StatusCode::TOO_MANY_REQUESTS,
        &headers,
    );
    let _ = stream.shutdown(std::net::Shutdown::Both);
}

/// Whether any of these clients needs a local gateway.
///
/// This is the whole of "start the local gateway only when at least one
/// active client requires it", and it is deliberately a function of what
/// the clients are **backed by** rather than of a flag someone remembered
/// to set. A flag can drift from the configuration it was meant to
/// summarise; a predicate read straight off [`BackendDemand`] cannot.
///
/// A client requires the gateway exactly when it is what serves it.
/// [`BackendDemand::Direct`] reaches its backend without one, so it should
/// never cause a socket to exist.
pub fn gateway_is_required(demands: &[BackendDemand]) -> bool {
    demands.contains(&BackendDemand::LocalGateway)
}

/// Start a gateway if — and only if — one of `demands` requires it.
///
/// `Ok(None)` means no active client asked for a gateway, and so **no
/// listener was bound at all**. That absence is the behaviour, not an
/// optimisation of it.
///
/// `upstream` is a closure rather than a value because building one costs a
/// provider lookup and a credential resolution, and a launch that needs no
/// gateway must pay for neither. It is called at most once, and only after
/// the predicate has already said yes.
pub fn start_if_required(
    demands: &[BackendDemand],
    upstream: impl FnOnce() -> Result<Upstream>,
) -> Result<Option<Gateway>> {
    if !gateway_is_required(demands) {
        return Ok(None);
    }
    Gateway::start(upstream()?).map(Some)
}

/// [`start_if_required`], with a
/// [`crate::provider::telemetry::GatewayQuotaCache`] a started gateway
/// persists every real forwarded exchange's rate-limit headers to.
///
/// **Not called from `crates/glasshouse/src/main.rs` today.** That file's
/// two launch paths (`launch_session` and the resume path,
/// `overlay_resolution`) both still call plain [`start_if_required`], which
/// this function reproduces exactly when `quota_cache` is `None`. Wiring a
/// real reading into `glasshouse resources` needs both of those call sites
/// changed to this function, with
/// `Some(crate::provider::telemetry::GatewayQuotaCache::new(runtime.paths()))`
/// — `runtime` is already in scope at both, since
/// `UserConfig::load(runtime.paths())` is the first line of
/// `main.rs::resources_report`'s own read of the same `paths::RuntimePaths`.
/// `crates/glasshouse/src/main.rs` is this package's `FORBIDDEN FILES`; see
/// the report.
pub fn start_if_required_with_quota_cache(
    demands: &[BackendDemand],
    upstream: impl FnOnce() -> Result<Upstream>,
    quota_cache: Option<crate::provider::telemetry::GatewayQuotaCache>,
) -> Result<Option<Gateway>> {
    if !gateway_is_required(demands) {
        return Ok(None);
    }
    Gateway::start_with_quota_cache(upstream()?, quota_cache).map(Some)
}

/// [`start_if_required_with_quota_cache`], with a
/// [`crate::provider::telemetry::GatewayHealthCache`] a started gateway
/// persists every bound exchange's resource health to — capability map lines
/// 1311/1321/1322/1324.
///
/// Both caches hold this process's own measurements and are read back by this
/// process's own next run. **A host's ledger is not among them**: an
/// observation this gateway makes leaves as an [`Observation`] through
/// [`start_if_required_with_degrade_sink`]'s sink, and nothing on this path
/// may take a handle to somewhere it could be written instead.
pub fn start_if_required_with_telemetry(
    demands: &[BackendDemand],
    upstream: impl FnOnce() -> Result<Upstream>,
    quota_cache: Option<crate::provider::telemetry::GatewayQuotaCache>,
    health_cache: Option<crate::provider::telemetry::GatewayHealthCache>,
) -> Result<Option<Gateway>> {
    if !gateway_is_required(demands) {
        return Ok(None);
    }
    Gateway::start_with_telemetry(upstream()?, quota_cache, health_cache).map(Some)
}

/// [`start_if_required_with_telemetry`], with the [`ObservationSink`] a
/// started gateway reports **every** [`Observation`] through: a resource that
/// went unusable (map line 1735) and every measurable turn (Phase 33A).
///
/// `None` reproduces [`start_if_required_with_telemetry`] exactly and is a
/// gateway nobody is listening to. [`null_sink`] says the same thing out
/// loud, and is what a standalone gateway — this crate with no host process
/// anywhere — passes.
///
/// **The name is older than the parameter.** It said `degrade_sink` when a
/// degrade was all that left this way; it stays because the host's own scans
/// name this door by it, and renaming a door to describe its contents would
/// cost more than it explains.
///
/// # The ownership answer, because the obvious one does not compile
///
/// A host's sink needs whatever the host records into, which does not exist
/// when it starts its gateway. A handle created before the gateway and
/// filled once both halves exist is the shape that works, replaying anything
/// that arrived in between. Nothing on this start path waits for a recorder
/// to be ready, and nothing on it may take a recorder instead of a sink.
/// History: design-decisions.md, "Trims: gateway/mod.rs", start_if_required_with_degrade_sink doc.
pub fn start_if_required_with_degrade_sink(
    demands: &[BackendDemand],
    upstream: impl FnOnce() -> Result<Upstream>,
    quota_cache: Option<crate::provider::telemetry::GatewayQuotaCache>,
    health_cache: Option<crate::provider::telemetry::GatewayHealthCache>,
    observation_sink: Option<ObservationSink>,
    // Told what the failure-domain term did to each failover this gateway
    // takes — capability map line 1851. `None` reproduces the behaviour this
    // door had before that line's producer landed, exactly as
    // `observation_sink` above does for line 1735.
    prevention_sink: Option<session::FailoverPreventionSink>,
) -> Result<Option<Gateway>> {
    if !gateway_is_required(demands) {
        return Ok(None);
    }
    Gateway::start_with_degrade_sink(
        upstream()?,
        quota_cache,
        health_cache,
        observation_sink,
        prevention_sink,
    )
    .map(Some)
}

/// Listen with no upstream yet — the standalone binary's door when nothing
/// resolves at start. `refusal` answers every request until `supplier`
/// succeeds; see [`UpstreamSlot`]. No caches and no prevention sink: a
/// deferred start is the standalone binary's, which has neither.
pub fn start_awaiting_upstream(
    refusal: String,
    supplier: impl Fn() -> Result<Upstream, String> + Send + Sync + 'static,
    observation_sink: Option<ObservationSink>,
) -> Result<Gateway> {
    Gateway::start_slot(
        UpstreamSlot::deferred(refusal, Box::new(supplier)),
        None,
        None,
        observation_sink,
        None,
    )
}

#[cfg(test)]
mod conformance;
#[cfg(test)]
mod fixture;

#[cfg(test)]
mod tests;
