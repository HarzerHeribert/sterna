//! What backend one live gateway-backed session is on, and what moves it —
//! Phase 9H, wired to the request path that actually serves the session.
//!
//! # This is the caller, not another mechanism
//!
//! [`mod@crate::routing::interactive`] decides; this decides nothing. Every
//! rule about stickiness, failover, migration, pins and cache locality lives
//! there as a pure function of values, and this module's whole job is to hold
//! the state those functions need, hand them real observations, and apply
//! what they answer to the [`Upstream`] the gateway is forwarding through.
//!
//! The observations are real. `SessionRouting::observe_exchange` takes an
//! `Exchange` that a connection thread has just finished — the same value
//! the gateway already logs — so the health of a free resource and the
//! decision to fail over both come from work that was going to happen anyway.
//! Phase 9I line 534 asks for exactly that, and the shape of this module is
//! the reason there is no probe to write: there is nowhere here to make a
//! request from.
//!
//! History: design-decisions.md, "Trims: gateway module docs", session/mod.rs module doc.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::provider::telemetry::RateLimitHeaders;
use crate::routing::evidence::{
    ContextState, FailureClass, HARNESS_TURN_PURPOSE, NewObservation, Outcome as RoutingOutcome,
    RouteCorrelations,
};
use crate::routing::free::{FreePool, FreeResource, WorkloadOutcome};
use crate::routing::interactive::{
    Assignment, AssignmentChange, ChangeCause, FailureResponse, InteractiveRouting,
    MigrationRefusal, Pin, ProviderFailure, RoutingRecord, SessionActivity, StayReason,
};
use crate::routing::pairing::{NoObservations, PairingPreference};
// The caller's own judgement about each candidate route, as plain data —
// this module holds it and hands it to the policy, and is structurally unable
// to derive one, because deriving one needs `crate::harness` and this module
// must never name it (see the module documentation above and
// `gateway::tests::the_gateway_imports_none_of_the_modules_that_would_make_it_a_harness`).
use crate::routing::pairing::PairingAffinities;
use crate::routing::request::TaskClass;
use crate::routing::{AssignedModel, Backend, CacheLocality};

#[cfg(test)]
use super::ingress::Tokens;
use super::ingress::{Exchange, Framing, Outcome, StreamEnd, TRANSPORT_TIMEOUT_DETAIL};
use super::upstream::Upstream;

/// Everything one gateway knows about which backend is serving it.
#[derive(Debug, Default)]
pub struct SessionRouting {
    state: Mutex<State>,
}

/// Told what the failure-domain term did to one failover's ranking, once per
/// failover the gateway takes — capability map line 1851's write side.
///
/// **A sink rather than a ledger handle**, exactly like
/// [`super::ObservationSink`] one module up and for practice §65's reason: the
/// gateway holds this for its whole life, and an open SQLite connection held
/// for the life of a session is free on the developer's machine and billed
/// on Windows. The sink's own body opens, writes and drops a handle at the
/// one moment a failover has actually been decided, which is a small
/// minority of exchanges and none of the ones that move nothing.
///
/// It also keeps this module incapable of reaching a database: nothing here
/// knows a project, a path or a `crate::Runtime`, and the only thing it can
/// do with a prevention is hand it to whoever asked for it.
pub type FailoverPreventionSink =
    Arc<dyn Fn(&crate::routing::interactive::FailureDomainEffect) + Send + Sync>;

#[derive(Debug, Default)]
struct State {
    policy: InteractiveRouting,
    /// `None` until a launch profile binds one. A gateway can be bound and
    /// serving before any harness has been pointed at it, and claiming an
    /// assignment then would be recording a decision nobody made.
    assignment: Option<Assignment>,
    record: RoutingRecord,
    free: FreePool,
    /// The most recent rate-limit headers a forwarded response carried, and
    /// when — capability map line 1229's gateway half. `None` until at least
    /// one exchange has produced one. Not `Exchange`'s business — see
    /// `ingress`'s own doc comment on why that type stays incapable of
    /// carrying a header value; this is a second, separate observation.
    quota: Option<(RateLimitHeaders, i64)>,
    /// Phase 9J line 576: the user's configured native-pairing preference,
    /// and the caller's own affinity for each candidate route, as
    /// `crate::profile`'s gateway path resolved them. Held here rather than
    /// on `policy`, because `Self::pin_to_serving_provider` and `Self::unpin`
    /// replace `policy` wholesale, and a resolved preference must survive
    /// that replacement — see `Self::set_pairing_preference`. Defaults match
    /// `EffectiveConfig::native_pairing_preference`'s own out-of-the-box
    /// answer and an empty set of affinities, so a gateway nothing has called
    /// `set_pairing_preference` on yet — every test double, and any future
    /// caller that forgets — prefers nothing and scores every prior `0.0`.
    pairing_preference: PairingPreference,
    pairing_affinities: PairingAffinities,
    /// The Glasshouse session this gateway serves — `crate::database`
    /// migration 24's `routing_observations.session_id`. `None` until a
    /// launch tells it (see [`SessionRouting::serve_session`]), and a
    /// gateway nothing has told is a gateway serving no session: its rows
    /// say so with `NULL` rather than an invented id.
    ///
    /// A plain `String`, never `crate::session::SessionId`: this module may
    /// not name `crate::session` at all (see this file's own module
    /// documentation and `gateway::tests::
    /// the_gateway_imports_none_of_the_modules_that_would_make_it_a_harness`),
    /// so the id crosses into this state as its string and nothing else —
    /// [`SessionRouting::serve_session`]'s own doc says why.
    session_id: Option<String>,
    /// The task class the launch that started this gateway was routed as —
    /// capability map line 1301 and `crate::database` migration 23's
    /// `routing_observations.task_class`, this producer's missing join
    /// (`GH-TASK-CLASS-COST-JOIN`, `archive/glasshouse:docs/product/evidence/phase-32g.md`'s
    /// Censused 2026-09-02 entry). `None` until a launch tells it (see
    /// [`SessionRouting::serve_task_class`]), the same honest absence
    /// [`Self::session_id`] carries for a gateway nothing has told.
    task_class: Option<TaskClass>,
}

/// What one finished exchange said about the backend that served it.
///
/// Three separable facts, because they have different consequences: the
/// resource's health (Phase 9I line 529), whether the credential itself was
/// refused (line 537), and whether the **provider** failed in the sense
/// Phase 9H line 512 means.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Observation {
    workload: WorkloadOutcome,
    failure: Option<ProviderFailure>,
}

/// What observing one exchange did to the session's assignment — returned
/// by [`SessionRouting::observe_exchange`] so the same connection thread can
/// write it onto the exchange's own evidence row, capability map line 1334's
/// `failovers`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ExchangeEffect {
    /// The assignment stands.
    Unchanged,
    /// Another of the same provider's credentials took over — Phase 9I line
    /// 537. **Not a failover**: the provider serving the session did not
    /// change, and the routing record keeps the two apart
    /// (`ChangeCause::CredentialRotation` beside `ChangeCause::Failover`),
    /// so this column does too.
    RotatedCredential,
    /// The session moved to another backend — Phase 9H line 512.
    FailedOver,
}

impl ExchangeEffect {
    /// Line 1334's `failovers` for the exchange this describes: `1` when it
    /// caused one, else `0`. A count rather than a flag because the column
    /// is one, and because a later producer that spans several exchanges may
    /// have more than one to report.
    pub(super) fn failovers(self) -> u32 {
        match self {
            Self::FailedOver => 1,
            Self::Unchanged | Self::RotatedCredential => 0,
        }
    }
}

/// What the accept loop knows about one finished exchange that [`Exchange`]
/// itself does not carry — handed to
/// [`SessionRouting::record_routing_observation`] as one value, because every
/// field is a fact about the same exchange and only the connection thread
/// that served it holds all of them at once.
pub(super) struct ExchangeReading<'a> {
    /// This response's own rate-limit headers, exactly as `ingress::serve`
    /// returned them. Read here for one purpose — capability map lines 1364
    /// and 1365's distinction between a cadence throttle and a spent quota,
    /// in [`failure_class`] — and for nothing else. This is not the
    /// narrowing [`stated_retry_after`] performs for a *routing decision*
    /// being undone: what is written from these headers is a class name,
    /// never a header value, and nothing here changes where a session is
    /// routed.
    pub(super) quota: &'a RateLimitHeaders,
    /// The instant the accept loop handed the connection to `ingress::serve`
    /// — an honest upper-bound proxy for dispatch, see
    /// `crate::routing::evidence`'s own header.
    pub(super) dispatched_at_unix: i64,
    /// The instant `ingress::serve` returned.
    pub(super) completed_at_unix: i64,
    /// The assignment as of dispatch — see the method's own doc for why this
    /// is a snapshot rather than a fresh read.
    pub(super) assignment: Option<Assignment>,
    /// What observing this same exchange did to the assignment —
    /// [`SessionRouting::observe_exchange`]'s own return, so the row can say
    /// whether *this* exchange caused a failover.
    pub(super) effect: ExchangeEffect,
}

/// The client slug a session nobody bound is recorded under — opaque, as
/// every client slug is to this crate; a host that binds names its own.
pub const UNBOUND_CLIENT: &str = "client";

/// The route an exchange actually took, as the backend a self-binding
/// session records: the serving backend, over the exchange's own protocol,
/// for the model the request named (or the harness's default when it named
/// none). `None` when the exchange reached no protocol, which is not a route.
fn bound_by_exchange(upstream: &Upstream, exchange: &Exchange) -> Option<crate::routing::Backend> {
    let protocol = exchange.protocol.as_deref()?;
    let model = match &exchange.requested_model {
        Some(model) => AssignedModel::named(model.clone()),
        None => AssignedModel::HarnessDefault,
    };
    upstream.serving().as_routing_backend(protocol, &model)
}

impl SessionRouting {
    pub fn new() -> Self {
        Self::default()
    }

    /// Phase 9H lines 505, 506 and 507: record which provider, model and
    /// credential are serving this harness's session, now that it is starting.
    ///
    /// Called by `crate::profile`'s gateway path, which is the only place
    /// that knows all three of the harness, the protocol resolved for it,
    /// and the model the launch profile named. The gateway itself knows
    /// none of them — it knows where to forward bytes.
    ///
    /// `protocol` is the **served** protocol, not necessarily the one the
    /// harness itself speaks: `crate::profile::apply_gateway` (Phase 56,
    /// GH-GATEWAY-TRANSLATE-LAUNCH) hands this the pair table's `to` slug
    /// for a translated launch, so a served-but-not-native pairing binds a
    /// real route the same way a native one always has.
    ///
    /// The assignment names the **serving** backend, which is the first
    /// configured one. Nothing here chooses; the choice was made when the
    /// upstream was built, and this records it.
    pub fn bind(&self, harness: &str, protocol: &str, model: AssignedModel, upstream: &Upstream) {
        let Some(backend) = upstream.serving().as_routing_backend(protocol, &model) else {
            // The serving backend has no route for `protocol`. `apply_gateway`
            // refuses both the unserved and the table-refused case before a
            // child exists, so reaching here means the caller and this
            // backend disagreed about what the serving backend carries;
            // recording an assignment that names a route which does not
            // exist would be worse than recording none.
            return;
        };
        let mut state = self.lock();
        state.assignment = Some(state.policy.assign(harness, backend));
    }

    /// Phase 9J line 576, called beside [`Self::bind`]: record the
    /// native-pairing preference and the caller's affinity for each candidate
    /// route, so `Self::observe_exchange`'s failover scores candidates against
    /// what the caller actually knows instead of the out-of-the-box default
    /// [`InteractiveRouting::on_provider_failure`] used before this method
    /// existed.
    ///
    /// Both arguments are **client-neutral by construction** — user ruling
    /// 2026-09-10. `preference_slug` is [`PairingPreference::slug`]'s own
    /// spelling, not the type itself, and `affinities` is a table of
    /// preferences keyed by provider and model rather than the harness
    /// knowledge behind them: `crate::profile`, the only caller, resolves
    /// both and hands over the answers, so nothing here has to learn what a
    /// harness is (or, for the preference, import `crate::config`). An
    /// unrecognised spelling degrades to [`PairingPreference::Strong`], the
    /// same out-of-the-box default `EffectiveConfig::native_pairing_preference`
    /// itself falls back to — this method never refuses a launch over a
    /// configuration value it cannot parse.
    // History: design-decisions.md, "Trims: gateway, profile and provider module docs", gateway/session/mod.rs `set_pairing_preference`.
    pub fn set_pairing_preference(&self, preference_slug: &str, affinities: PairingAffinities) {
        let preference =
            PairingPreference::from_slug(preference_slug).unwrap_or(PairingPreference::Strong);
        let mut state = self.lock();
        state.pairing_preference = preference;
        state.pairing_affinities = affinities;
    }

    /// Capability map line 2019, and `crate::database` migration 24: record
    /// which Glasshouse session this gateway is serving, so that every row
    /// this type's own `record_routing_observation` writes can name it.
    ///
    /// `session_id` is a plain `&str`, not `crate::session::SessionId`: this
    /// module is structurally unable to see the session model at all (this
    /// file's own module documentation, and `gateway::tests::
    /// the_gateway_imports_none_of_the_modules_that_would_make_it_a_harness`
    /// enforces it with a source scan), so the id crosses this boundary as
    /// its string and nothing else — the caller's own typed id, narrowed at
    /// the one call.
    // History: design-decisions.md, "Trims: gateway, profile and provider module docs", gateway/session/mod.rs `serve_session`.
    pub fn serve_session(&self, session_id: &str) {
        self.lock().session_id = Some(session_id.to_owned());
    }

    /// Capability map line 1301, and `crate::database` migration 23: record
    /// which task class the launch that started this gateway was routed as,
    /// so every row `record_routing_observation` writes can join to
    /// `routing::burn::output_tokens_by_class`'s reader the way
    /// `record_routing_latency`'s own row already does.
    ///
    /// [`Self::serve_session`]'s shape exactly: `None` for a launch with no
    /// routing decision — `main.rs::launch_session` passes nothing when
    /// routing was off or no task was classified — and a gateway nothing has
    /// told stamps `NULL`, never an invented class.
    pub fn serve_task_class(&self, task_class: Option<TaskClass>) {
        self.lock().task_class = task_class;
    }

    /// The backend serving this session, once one has been bound.
    pub fn assignment(&self) -> Option<Assignment> {
        self.lock().assignment.clone()
    }

    /// Phase 9H line 518: pin this session to the provider now serving it and
    /// turn automatic failover off.
    ///
    /// Returns the provider it pinned to, or `None` when nothing is bound
    /// yet — a pin before an assignment would name nothing.
    pub fn pin_to_serving_provider(&self) -> Option<String> {
        let mut state = self.lock();
        let provider = state.assignment.as_ref()?.provider().to_owned();
        state.policy = InteractiveRouting::pinned_to(provider.clone());
        Some(provider)
    }

    /// Lift a pin.
    pub fn unpin(&self) {
        let mut state = self.lock();
        state.policy = InteractiveRouting::new();
    }

    /// Whether, and where, this session is pinned.
    pub fn pin(&self) -> Pin {
        self.lock().policy.pin().clone()
    }

    /// Phase 9H line 511: move this session to another backend, explicitly,
    /// at a task boundary.
    ///
    /// The refusals are the policy's, not this module's. What happens here is
    /// only the consequence: the upstream is switched and the change is
    /// recorded with its cache locality.
    pub fn migrate(
        &self,
        to: &Backend,
        activity: SessionActivity,
        upstream: &Upstream,
    ) -> Result<Assignment, MigrationRefusal> {
        let mut state = self.lock();
        let Some(current) = state.assignment.clone() else {
            return Err(MigrationRefusal::MidTurn);
        };
        let migrated = state.policy.migrate(&current, to.clone(), activity)?;
        if !upstream.switch_to(migrated.backend().credential()) {
            // The caller offered a backend this gateway does not hold. A
            // refusal rather than a silent no-op: the session would otherwise
            // believe it had moved.
            return Err(MigrationRefusal::Incompatible(
                crate::routing::interactive::Incompatibility::Protocol {
                    provider: migrated.provider().to_owned(),
                    speaks: migrated.protocol().to_owned(),
                    needed: current.protocol().to_owned(),
                },
            ));
        }
        let cache = CacheLocality::between(current.backend(), migrated.backend());
        state.record.note(AssignmentChange {
            from: current,
            to: migrated.clone(),
            cause: ChangeCause::Migration,
            cache,
        });
        state.assignment = Some(migrated.clone());
        Ok(migrated)
    }

    /// Phase 9H line 515: every change of the backend serving this session,
    /// in order.
    pub fn changes(&self) -> Vec<AssignmentChange> {
        self.lock().record.entries().to_vec()
    }

    /// What has been learned about each free resource, from real work only.
    pub fn free_pool(&self) -> FreePool {
        self.lock().free.clone()
    }

    /// Every resource's health this gateway has observed for `provider`, as
    /// [`crate::provider::telemetry::GatewayHealthReading`]s ready to cross
    /// the process boundary — capability map lines 1311/1321/1322/1324's
    /// gateway half, called once per exchange from the accept loop,
    /// symmetric with [`Self::observe_quota_headers`] rather than folded into
    /// [`Self::observe_exchange`] itself.
    ///
    /// `now` and `now_unix` name the same instant in the two clocks this
    /// crosses between: `now` is what [`FreePool::observe`] measured
    /// `cooling_down_until` against, and `now_unix` is the wall-clock second
    /// the caller is about to persist this snapshot under. The remaining
    /// duration between `cooling_down_until` and `now` is added to
    /// `now_unix` — never `cooling_down_until` compared against `now_unix`
    /// directly, which would mix an [`std::time::Instant`], with no fixed
    /// epoch, into unix-second arithmetic.
    pub(super) fn health_readings_for(
        &self,
        provider: &str,
        now: Instant,
        now_unix: i64,
    ) -> Vec<crate::provider::telemetry::GatewayHealthReading> {
        self.free_pool()
            .observed()
            .into_iter()
            .filter(|(resource, _)| resource.provider() == provider)
            .map(
                |(resource, health)| crate::provider::telemetry::GatewayHealthReading {
                    credential_label: resource.credential().label(),
                    model: resource.model().to_owned(),
                    consecutive_failures: health.consecutive_failures(),
                    cooling_down_until_unix: health.cooling_down_until().map(|until| {
                        now_unix + until.saturating_duration_since(now).as_secs() as i64
                    }),
                    cooldown_cause: health.cooldown_cause(),
                    credential_rejected: health.credential_was_rejected(),
                },
            )
            .collect()
    }

    /// Record what a forwarded response's headers said — capability map line
    /// 1229's gateway half, called once per exchange from the accept loop.
    ///
    /// A no-op when `headers` is empty, which is the ordinary case: most
    /// exchanges forward a response that carries no rate-limit header this
    /// reader understands, and a no-op leaves whatever the last real reading
    /// was in place rather than clearing it.
    pub(super) fn observe_quota_headers(&self, headers: RateLimitHeaders, observed_at_unix: i64) {
        if headers.is_empty() {
            return;
        }
        self.lock().quota = Some((headers, observed_at_unix));
    }

    /// The most recent rate-limit headers observed, and when — capability map
    /// line 1229's gateway half, read by [`super::Gateway::quota_headers`].
    pub fn quota_headers(&self) -> Option<(RateLimitHeaders, i64)> {
        self.lock().quota.clone()
    }

    /// Phase 33A's one production producer this round: turn one finished
    /// exchange into a [`NewObservation`] and **report it outward** through
    /// `sink`, when there is enough to say. See
    /// `crate::routing::evidence`'s own module documentation for exactly
    /// which fields this can and cannot supply and why — this method is
    /// simply where that honest subset gets built.
    ///
    /// **It writes nothing down.** The gateway runs as its own process and
    /// keeps no ledger; a host that keeps one receives
    /// [`super::Observation::Routed`] and writes the row on its own side of
    /// the boundary, and a standalone gateway's sink drops it. Which of the
    /// two is listening is not observable from here, and must not become so
    /// — see [`super::ObservationSink`].
    ///
    /// Two conditions must both hold before anything is recorded, mirroring
    /// [`classify`]'s own filter:
    ///
    /// - the exchange must have reached the provider (`Forwarded` or
    ///   `Unreachable` — the same two outcomes [`Self::observe_exchange`]
    ///   treats as saying something about the backend), because nothing else
    ///   is a measurable turn;
    /// - `assignment` must be `Some`, because a provider/model identity
    ///   recorded for an unbound session would be invented rather than
    ///   observed.
    // History: design-decisions.md, "Trims: gateway, profile and provider module docs", gateway/session/mod.rs `record_routing_observation`.
    pub(super) fn record_routing_observation(
        &self,
        sink: &super::ObservationSink,
        exchange: &Exchange,
        reading: ExchangeReading<'_>,
    ) {
        let failure_class = failure_class(exchange, reading.quota);
        let outcome = match &exchange.outcome {
            Outcome::Forwarded {
                upstream_status, ..
            } => Some(
                if (200..400).contains(upstream_status) && failure_class.is_none() {
                    RoutingOutcome::Succeeded
                } else {
                    RoutingOutcome::Failed
                },
            ),
            Outcome::Unreachable { .. } => Some(RoutingOutcome::Failed),
            Outcome::Unauthenticated
            | Outcome::Declined
            | Outcome::Unrouted
            | Outcome::ClientGone
            | Outcome::Idle => None,
        };
        let Some(outcome) = outcome else {
            return;
        };

        let Some(assignment) = reading.assignment else {
            return;
        };

        // Migration 24's three. The session is what this gateway was told it
        // serves — `None`, and so `NULL`, for a gateway nothing told; the
        // other two are the decoded request's own facts, carried on the
        // exchange from `super::translate::serve`. A relay may additionally
        // carry the one bounded top-level model observation used below.
        let session_id = self.lock().session_id.clone();
        // Map line 1301's missing join: `task_class` has been migration 23's
        // column since Phase 34C, and this producer is the first to stamp it
        // on a gateway-served row — `record_routing_latency`'s own row is
        // the only other writer and is unaffected by this one.
        let task_class = self.lock().task_class;

        let new = NewObservation::new(
            exchange.provider.clone(),
            exchange
                .requested_model
                .clone()
                .unwrap_or_else(|| assignment.backend().model().label().to_owned()),
        )
        .with_route(exchange.protocol.clone())
        .with_harness(Some(assignment.harness().to_owned()))
        .with_purpose(Some(
            exchange.purpose.as_deref().unwrap_or(HARNESS_TURN_PURPOSE),
        ))
        .with_quota_context(Some(assignment.backend().credential().label()))
        .with_timing(
            Some(reading.dispatched_at_unix),
            Some(reading.completed_at_unix),
        )
        .with_first_byte_at(exchange.first_byte_at)
        // Line 1331/1332's pair: `translate::serve` derives both from the
        // canonical events it already had to decode, and since the
        // 2026-09-03 ruling `ingress::forward` latches both as the markers
        // pass the relay seam — `ingress`'s own "a seventh thing may now be
        // recorded". `None` on either path where no marker was recognised,
        // exactly like `first_byte_at` above.
        .with_first_token_at(exchange.first_token_at)
        .with_first_tool_call_at(exchange.first_tool_call_at)
        // Migration 25's four offsets, beside the second-resolution
        // timestamps above rather than instead of them. Their zero is the
        // instant each path sent its upstream request — `ingress::forward`
        // and `translate::serve` each hold their own `Instant` and neither
        // hands one back through `ExchangeReading`, because the accept
        // loop's `dispatched_at` is the hand-off and not the send. Each
        // offset is stamped from the same clock reading as the `*_at` one
        // line up, on whichever path stamped it.
        .with_first_byte_ms(exchange.first_byte_ms)
        .with_first_token_ms(exchange.first_token_ms)
        .with_first_tool_call_ms(exchange.first_tool_call_ms)
        .with_completed_ms(exchange.completed_ms)
        // Line 1334's last two quantities: `translate::serve` derives both
        // from the request and response it already had to decode, and
        // `None` on a relayed exchange (this method's own caller never
        // gives it one), exactly like the pair above.
        .with_tool_rounds(exchange.tool_rounds)
        .with_repairs(exchange.repairs)
        .with_outcome(outcome)
        .with_failure_class(failure_class)
        .with_failovers(Some(reading.effect.failovers()))
        .with_retries(Some(0))
        .with_session_id(session_id)
        .with_task_class(task_class)
        .with_effort_level(exchange.effort)
        .with_turn_shape(exchange.turn_shape)
        // Phase 56: a translated exchange has a parsed response, so its
        // usage is exact where the provider stated it. Since the 2026-09-03
        // ruling a relayed exchange is exact too, where its protocol has a
        // usage spelling and its stream ended cleanly — and `None`, meaning
        // unknown rather than zero, everywhere else.
        .with_tokens(
            exchange
                .tokens
                .and_then(|tokens| i64::try_from(tokens.input).ok()),
            exchange
                .tokens
                .and_then(|tokens| i64::try_from(tokens.output).ok()),
            exchange
                .tokens
                .and_then(|tokens| tokens.cached)
                .and_then(|cached| i64::try_from(cached).ok()),
        )
        // Line 1545: a provider's own prompt-cache read count is the only
        // observation this relay ever makes of the session's context, so a
        // read greater than zero is the evidence it was warm. `cached`
        // follows `with_tokens`' own rule immediately above -- `None`
        // there meant "the provider stated no usage at all"; here it also
        // covers "usage was stated but the cache count was not," and both
        // collapse to `Unknown` rather than to `Cold`, exactly as that
        // rule requires.
        .with_context_state(match exchange.tokens.and_then(|tokens| tokens.cached) {
            Some(0) => ContextState::Cold,
            Some(_) => ContextState::Warm,
            None => ContextState::Unknown,
        });

        // One call, and nothing to fail: the sink is `Fn`, so the accept
        // loop cannot lose a real session's exchange to a full disk or a
        // locked database on the far side. Whatever the host does with this
        // — write it, queue it, drop it — is the host's own risk, taken
        // after the exchange is already served.
        sink(super::Observation::Routed {
            observation: Box::new(new),
            observed_at_unix: reading.completed_at_unix,
        });
    }

    /// Fold in one finished exchange: update the resource's health, and, when
    /// it was a real provider failure, ask the policy what to do about it.
    ///
    /// This is the production feed for Phase 9H lines 512 to 517 and Phase 9I
    /// lines 529, 534, 535, 537 and 538. It is called once per connection,
    /// after the exchange is over.
    ///
    /// `stated_retry_after` is what **the provider itself said** about how
    /// long to wait, read off this same response's headers by
    /// [`stated_retry_after`] — capability map line 1319. `None` means the
    /// provider said nothing, and it must stay `None` all the way down: the
    /// free pool's own bounded backoff is what applies then, and a wait
    /// nobody stated is not a fact to record.
    ///
    /// Returns what this exchange did to the assignment, so the accept loop
    /// can write it onto the exchange's own evidence row — capability map
    /// line 1334's `failovers`. Every early return is
    /// [`ExchangeEffect::Unchanged`]: an exchange that said nothing moved
    /// nothing.
    // History: design-decisions.md, "Trims: gateway, profile and provider module docs", gateway/session/mod.rs `observe_exchange`.
    pub(super) fn observe_exchange(
        &self,
        upstream: &Upstream,
        exchange: &Exchange,
        now: Instant,
        stated_retry_after: Option<Duration>,
        prevention: Option<&FailoverPreventionSink>,
    ) -> ExchangeEffect {
        let Some(observation) = classify(exchange, stated_retry_after) else {
            // Nothing reached the provider — an unauthenticated caller, a
            // malformed head, a target belonging to no protocol. Recording
            // health for a request the provider never saw would be inventing
            // a signal.
            return ExchangeEffect::Unchanged;
        };

        let mut state = self.lock();
        let current = match state.assignment.clone() {
            Some(current) => current,
            // Nobody bound this session: the standalone binary has no launch
            // profile to bind from and learns what it serves from the first
            // exchange. Binding here, to the route that exchange took, is
            // what arms health, credential rotation and same-model failover
            // for a gateway with no host at all; a host that bound at launch
            // never reaches this arm.
            None => match bound_by_exchange(upstream, exchange) {
                Some(backend) => {
                    let bound = state.policy.assign(UNBOUND_CLIENT, backend);
                    state.assignment = Some(bound.clone());
                    bound
                }
                None => return ExchangeEffect::Unchanged,
            },
        };
        let credential = current.backend().credential().clone();
        let model = model_key(current.backend().model());

        // Phase 9I lines 529 and 538: health per credential and model, from
        // real workload.
        state.free.observe(
            &FreeResource::new(credential.clone(), model.clone()),
            observation.workload,
            now,
        );

        // Phase 9I line 537: a credential that was refused or is out of
        // requests is *that credential's* problem. Try this provider's other
        // keys before concluding anything about the provider.
        if matches!(
            observation.workload,
            WorkloadOutcome::CredentialRejected | WorkloadOutcome::RateLimited { .. }
        ) {
            let siblings = upstream.credentials_of(current.provider());
            if let Some(next) = state.free.rotate_from(&credential, &siblings, &model, now)
                && let Some(backend) =
                    upstream.backend_for(&next, current.protocol(), current.backend().model())
                && upstream.switch_to(&next)
            {
                let to = Assignment::new(current.harness(), backend);
                let cache = CacheLocality::between(current.backend(), to.backend());
                state.record.note(AssignmentChange {
                    from: current,
                    to: to.clone(),
                    cause: ChangeCause::CredentialRotation,
                    cache,
                });
                state.assignment = Some(to);
                return ExchangeEffect::RotatedCredential;
            }
        }

        let Some(failure) = observation.failure else {
            return ExchangeEffect::Unchanged;
        };

        let candidates =
            upstream.failover_candidates(current.protocol(), current.backend().model());

        // What this gateway has been told about other turns: nothing, and
        // said so with a named value rather than left implicit.
        //
        // Phase 9J's prior and Phase 33C's correlation term are both read
        // out of a durable ledger of past observations, and **this process
        // keeps none**. It reports every turn outward as
        // [`super::Observation::Routed`] and never reads one back; a host
        // that accumulates them ranks on its own side of the boundary, and
        // handing this side a ledger handle would be the embedding that
        // undoes the split (see [`super::ObservationSink`]).
        //
        // What the ranking then weighs, standalone: compatibility, the
        // caller's stated affinity (none → `0.0`) and the failure-domain
        // term, over candidates in catalogue order. Quota, health and a
        // stated `Retry-After` are tracked per credential by `free` and
        // decide rotation and cooldown, not this ranking's order — the
        // successor that weighs them is named in design-decisions (*The
        // inference gateway is its own crate and process*, residuals).
        let evidence = NoObservations;
        let correlations = RouteCorrelations::default();

        match state.policy.on_provider_failure(
            &current,
            failure,
            &candidates,
            state.pairing_preference,
            &state.pairing_affinities,
            &evidence,
            &correlations,
        ) {
            FailureResponse::FailOver {
                to,
                cache,
                explanation,
                domain_effect,
            } => {
                if upstream.switch_to(to.backend().credential()) {
                    // Capability map line 1851, at the one moment a failover
                    // is real: `domain_effect` is the comparison
                    // `on_provider_failure` made between its own ranking and
                    // the same ranking without the failure-domain term. It
                    // is reported here rather than at the `OfferMigration`
                    // arm below, because that arm offers a move nobody takes
                    // and counting it would put it in the denominator of how
                    // often a *failover* was steered.
                    //
                    // Inside the `switch_to` guard on purpose: an upstream
                    // that refused the switch produced no failover, and a row
                    // for it would count a move that did not happen.
                    if let Some(sink) = prevention {
                        sink(&domain_effect);
                    }
                    tracing::debug!(
                        harness = %current.harness(),
                        from = %current.label(),
                        to = %to.label(),
                        explanation = %explanation.render(),
                        "the native-pairing prior and local evidence behind a gateway \
                         failover"
                    );
                    state.record.note(AssignmentChange {
                        from: current,
                        to: to.clone(),
                        cause: ChangeCause::Failover(failure),
                        cache,
                    });
                    state.assignment = Some(to);
                    ExchangeEffect::FailedOver
                } else {
                    ExchangeEffect::Unchanged
                }
            }
            FailureResponse::OfferMigration {
                to,
                cache,
                explanation,
                domain_effect: _,
            } => {
                // Phase 9H line 514: a material model change is not taken.
                // Said out loud, because an offer nobody hears is a decision
                // made by silence.
                tracing::info!(
                    harness = %current.harness(),
                    from = %current.label(),
                    offered = %to.label(),
                    cache = %cache,
                    explanation = %explanation.render(),
                    "a gateway backend failed and the only compatible replacement \
                     serves a different model, which is a migration rather than a failover"
                );
                ExchangeEffect::Unchanged
            }
            FailureResponse::Stay { reason } => {
                let detail = match &reason {
                    StayReason::SessionPinned { provider } => {
                        format!("the session is pinned to `{provider}`")
                    }
                    StayReason::NoCompatibleBackend { rejected } => {
                        if rejected.is_empty() {
                            "no other backend is configured".to_owned()
                        } else {
                            rejected
                                .iter()
                                .map(|why| why.to_string())
                                .collect::<Vec<_>>()
                                .join("; ")
                        }
                    }
                };
                tracing::info!(
                    harness = %current.harness(),
                    backend = %current.label(),
                    failure = %failure.describe(),
                    detail,
                    "a gateway backend failed and the session stayed where it was"
                );
                ExchangeEffect::Unchanged
            }
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        // A poisoned lock is treated as ownership rather than as a reason to
        // give up — the same decision `shutdown`'s bounded retry made, and for
        // the same reason: refusing to route because another thread panicked
        // would turn one failure into every session's failure.
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

/// A stable key for the model an assignment names, including the honest case
/// where the harness chose it.
///
/// [`AssignedModel::HarnessDefault`]'s label is used as the key rather than an
/// empty string: health has to be tracked for it like any other resource, and
/// an empty key would silently share one entry with anything else that had
/// none.
fn model_key(model: &AssignedModel) -> String {
    model.label().to_owned()
}

/// The wait **the provider itself declared** on one response, as a duration —
/// capability map line 1319's producer end, narrowed to the one fact the
/// decision is allowed to carry.
///
/// # `None` stays `None`
///
/// The provider saying nothing is not the same fact as the provider saying
/// zero, and neither is a reason to invent a number: with no stated wait,
/// [`crate::routing::free::ResourceHealth::fail`]'s own bounded backoff is
/// what applies, after the failures it requires. `RateLimitHeaders` has
/// already dropped anything that was not a non-negative integer (see
/// [`RateLimitHeaders::read`]), and [`u64::try_from`] is the second, local
/// refusal rather than a clamp — a negative wait is a header this code does
/// not understand, not a zero-second one.
// History: design-decisions.md, "Trims: gateway, profile and provider module docs", gateway/session/mod.rs `stated_retry_after`.
pub(super) fn stated_retry_after(headers: &RateLimitHeaders) -> Option<Duration> {
    headers
        .retry_after_seconds()
        .and_then(|seconds| u64::try_from(seconds).ok())
        .map(Duration::from_secs)
}

/// What one exchange says, or `None` when it never reached the provider.
///
/// The one place an HTTP status becomes a routing fact. Phase 9H line 512
/// wants a *real provider failure*, and three of these numbers are not one:
///
/// - `401` and `403` are about the credential, and Phase 9I line 537 answers
///   them by rotating keys within the provider;
/// - any other `4xx` is the harness's own request being wrong, and the
///   provider that answered it is healthy — sending the same malformed
///   request somewhere else would fail there too.
///
/// `stated_retry_after` is the wait the provider declared on **this**
/// response, and it is used by exactly one arm — the `429`. A `Retry-After`
/// on any other status is not folded in anywhere, because
/// [`crate::routing::free::WorkloadOutcome`] keeps a rate-limit refusal, a
/// credential rejection and a transport failure apart on purpose, and only
/// the first of the three is a *temporary scheduling block* capability map
/// line 1319 speaks about. Widening it would blur exactly the distinction
/// that type exists to hold.
fn classify(exchange: &Exchange, stated_retry_after: Option<Duration>) -> Option<Observation> {
    match &exchange.outcome {
        Outcome::Forwarded {
            upstream_status, ..
        } => Some(match *upstream_status {
            // `402` is here because a real run put it here. Claude Code was
            // driven through this gateway to OpenRouter on 2026-08-26 for a
            // model OpenRouter itself lists as `:free`, and the answer was
            // `402 Insufficient credits — this account never purchased
            // credits`. That is not a provider outage and it is not a
            // malformed request: it is this **account's** key being unable to
            // pay, which is the same class of fact as a revoked one. Another
            // key on another account would serve. So it rotates like `401`
            // and `403` rather than failing the provider over, and waiting
            // does not fix it — which is why `CredentialRejected` is not a
            // cooldown.
            401..=403 => Observation {
                workload: WorkloadOutcome::CredentialRejected,
                failure: None,
            },
            429 => Observation {
                workload: WorkloadOutcome::RateLimited {
                    // Capability map line 1319. The provider's own answer,
                    // read off this very response by `ingress::forward` and
                    // carried here by the accept loop — *authoritative* for a
                    // temporary scheduling block, which is why
                    // `routing::free::ResourceHealth::fail` applies a stated
                    // wait immediately and unclamped while an invented one
                    // still has to earn `FAILURES_BEFORE_COOLDOWN`.
                    //
                    // `None` when the provider stated nothing, and it stays
                    // `None`: the free pool's own bounded backoff is the
                    // honest fallback, and a wait nobody declared is not one
                    // to invent here.
                    retry_after: stated_retry_after,
                },
                failure: ProviderFailure::from_status(429),
            },
            status @ 500..=599 => Observation {
                workload: WorkloadOutcome::CapacityFailure,
                failure: ProviderFailure::from_status(status),
            },
            // Everything else: the provider answered, so it is healthy, and
            // whether that answer is a *provider failure* is
            // `ProviderFailure::from_status`'s question and not a second
            // reading of the same number written here. Hard-coding `None`
            // was the first version, and a mutation of `from_status` proved
            // it: widening that function to call a `400` a provider failure
            // changed nothing, because this arm had already decided. Two
            // copies of one rule is exactly the shape that lets them drift.
            status => Observation {
                workload: WorkloadOutcome::Served,
                failure: ProviderFailure::from_status(status),
            },
        }),
        Outcome::Unreachable { .. } => Some(Observation {
            workload: WorkloadOutcome::CapacityFailure,
            failure: Some(ProviderFailure::Unreachable),
        }),
        // None of these reached the provider, so none of them says anything
        // about it.
        Outcome::Unauthenticated
        | Outcome::Declined
        | Outcome::Unrouted
        | Outcome::ClientGone
        | Outcome::Idle => None,
    }
}

/// How far out a `429`'s own reset must be before the refusal is read as a
/// spent long-window quota rather than a cadence limit — capability map line
/// 1365's boundary between the two, and [`failure_class`]'s one constant.
///
/// Five minutes. Every per-minute cadence limit this project has read off a
/// real host (`crate::provider::telemetry`'s AnyRouter and Groq fixtures:
/// `w=60`, per-minute request and token pools) reopens within a minute, and
/// a `Retry-After` on one is seconds to a couple of minutes. A window that
/// reopens in hours, or at midnight, is a quota. Five minutes sits between
/// the two with room on both sides. A constant rather than a configuration
/// because nothing measured yet says a user needs to move it; the day one
/// does, this is the one number to lift.
pub(super) const EXHAUSTED_QUOTA_HORIZON_SECONDS: i64 = 300;

/// What kind of failure one exchange was — capability map line 1364's
/// nine-way vocabulary, decided here and nowhere else, from the status line,
/// the rate-limit headers, the byte count and how the stream ended. `None`
/// for a served exchange, and for every exchange that never reached the
/// provider, on the same reasoning as [`classify`]: nothing can be said about
/// a provider that never saw the request.
///
/// # What is never read
///
/// No byte of the body. A `200` whose body describes a model error is
/// served here, and the ledger's own header says so; the harness that
/// received the body is the thing that can read it.
// History: design-decisions.md, "Trims: gateway, profile and provider module docs", gateway/session/mod.rs `failure_class`.
pub(super) fn failure_class(exchange: &Exchange, quota: &RateLimitHeaders) -> Option<FailureClass> {
    match &exchange.outcome {
        Outcome::Forwarded {
            upstream_status, ..
        } => match *upstream_status {
            401 | 403 => Some(FailureClass::CredentialFailure),
            402 => Some(FailureClass::ExhaustedQuota),
            429 => Some(if quota_is_exhausted(quota, exchange.first_byte_at) {
                FailureClass::ExhaustedQuota
            } else {
                FailureClass::Throttle
            }),
            400..=499 => Some(FailureClass::RequestIncompatibility),
            500..=599 => Some(FailureClass::Upstream5xx),
            _ => match exchange.framing {
                Some(Framing {
                    ended: StreamEnd::Truncated | StreamEnd::Aborted,
                    ..
                }) => Some(FailureClass::StreamAbort),
                Some(Framing {
                    relayed: Some(0),
                    ended: StreamEnd::Complete,
                    ..
                }) => Some(FailureClass::EmptyCompletion),
                _ => None,
            },
        },
        Outcome::Unreachable { detail } => Some(if *detail == TRANSPORT_TIMEOUT_DETAIL {
            FailureClass::Timeout
        } else {
            FailureClass::Unknown
        }),
        Outcome::Unauthenticated
        | Outcome::Declined
        | Outcome::Unrouted
        | Outcome::ClientGone
        | Outcome::Idle => None,
    }
}

/// [`failure_class`]'s `429` rule: nothing remains, and the window reopens
/// no sooner than [`EXHAUSTED_QUOTA_HORIZON_SECONDS`] after the response was
/// observed. `observed_at_unix` is the exchange's `first_byte_at`, which is
/// set on every forwarded exchange; a reset field with no observation
/// instant to anchor it is read as the delta the IETF field specifies.
fn quota_is_exhausted(quota: &RateLimitHeaders, observed_at_unix: Option<i64>) -> bool {
    if quota.remaining() != Some(0) {
        return false;
    }
    let reopens_in = match observed_at_unix {
        Some(observed) => quota
            .resets_at_unix(observed)
            .map(|at| at.saturating_sub(observed)),
        None => quota.reset(),
    }
    .or_else(|| quota.retry_after_seconds());
    reopens_in.is_some_and(|seconds| seconds >= EXHAUSTED_QUOTA_HORIZON_SECONDS)
}

/// Map line 1735: whether one finished exchange says the gateway's own
/// upstream failed, separately from anything about the harness process that
/// sent the request — and separately from [`classify`]'s question, which is
/// "does this session need to move" rather than "is the resource itself
/// unhealthy". Called on every exchange, whether or not a session is bound —
/// a gateway can be serving before any harness has been pointed at it, and
/// the resource can still be unreachable.
///
/// Only [`Outcome::Unreachable`] qualifies. A `Forwarded` exchange reached
/// the provider and got an answer — even a `4xx` or `5xx` one — which is the
/// provider or the request being wrong, not the gateway failing to reach it;
/// mapping that to a gateway failure would be exactly the invented signal the
/// packet forbids ("a `Forwarded` exchange that merely returned an
/// application-level error the gateway passed through is not a gateway
/// failure"). Every other outcome (`Unauthenticated`, `Declined`,
/// `Unrouted`, `ClientGone`, `Idle`) never reached the provider at all, for
/// reasons that have nothing to do with the provider's health.
// History: design-decisions.md, "Trims: gateway, profile and provider module docs", gateway/session/mod.rs `gateway_failure`.
pub(super) fn gateway_failure(exchange: &Exchange) -> Option<super::DegradeReason> {
    match &exchange.outcome {
        Outcome::Unreachable { .. } => Some(super::DegradeReason::Unreachable),
        Outcome::Forwarded { .. }
        | Outcome::Unauthenticated
        | Outcome::Declined
        | Outcome::Unrouted
        | Outcome::ClientGone
        | Outcome::Idle => None,
    }
}

#[cfg(test)]
mod tests;
