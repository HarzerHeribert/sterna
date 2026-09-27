//! One connection, from the harness's request line to the last byte of the
//! provider's response: read the head, check the bearer token, rewrite the
//! target, `authorization` and `host` for the provider, forward every other
//! byte unchanged, then relay the status, headers and body back untouched.
//!
//! This module never parses a response body, with one exception: **a
//! seventh thing may now be recorded**, under the user's 2026-09-03 ruling
//! narrowing that rule (`archive/glasshouse:docs/product/design-decisions.md`, *Steering
//! decisions of record* §1). [`super::usage`] scans a **supported** relayed
//! body over a sliding window of at most 512 retained bytes, for a fixed
//! table of JSON key spellings, to extract [`Tokens`], `first_token_at` and
//! `first_tool_call_at`; this file still decodes nothing itself —
//! [`Counted`] hands it a shared borrow of the buffer
//! [`super::http::pump`] is about to write and returns exactly the `read` it
//! was given.
//!
//! History: design-decisions.md, "Trims: gateway/ingress.rs", module doc.

use std::io::{BufReader, Read, Write};
use std::net::{Shutdown, TcpStream};
use std::time::{Duration, Instant};

use ureq::http::{HeaderValue, Request, StatusCode, header};
use ureq::{Agent, SendBody};

use crate::provider::telemetry::RateLimitHeaders;
use crate::routing::evidence::{EffortLevel, TurnShape};

use super::GatewayToken;
use super::http::{self, HeadError};
use super::request_model;
use super::translate;
use super::upstream::{Route, ServedBy, Upstream, UpstreamBackend};
use super::usage;

/// The `authorization` scheme the gateway accepts from a child harness.
///
/// Claude Code 2.1.245 launched with `ANTHROPIC_AUTH_TOKEN=<value>` was
/// observed sending exactly `authorization: Bearer <value>` — see
/// `crate::harness::claude_code`'s `CREDENTIAL_ENV`, where that observation
/// is recorded.
const BEARER_PREFIX: &str = "Bearer ";

/// A client's request header naming which fixed purpose this exchange's
/// ledger row should carry, instead of [`crate::routing::evidence::HARNESS_TURN_PURPOSE`].
/// Never forwarded upstream, whatever its value —
/// `ask-primary-supervisor-purpose-header.md`, ruled yes.
const PURPOSE_HEADER: &str = "x-glasshouse-purpose";

/// How long the gateway waits for a request head before hanging up.
///
/// A connection that has been opened but has sent nothing holds a thread.
/// Loopback clients are not slow, so this is generous by two orders of
/// magnitude and still bounds the damage a stuck client can do.
const HEAD_TIMEOUT: Duration = Duration::from_secs(30);

/// How much of a refused request's body is drained before the socket closes.
///
/// Closing a socket while the client is still writing its body gets the
/// client a connection reset instead of the `401` it was sent, so the body
/// is drained first. Capped, because draining is work done on behalf of a
/// request that has already been refused.
const DRAIN_CAP: u64 = 1024 * 1024;

/// [`transport_detail`]'s phrase for a timeout — see that function's own doc
/// for why this one phrase has a name.
pub(super) const TRANSPORT_TIMEOUT_DETAIL: &str = "the provider did not answer in time";

/// How long a refused request is given to finish sending before the socket
/// closes underneath it.
///
/// Short, and much shorter than [`HEAD_TIMEOUT`]: this is politeness owed to
/// a request that is not going to be served, and a thread should not be held
/// for it.
const SETTLE_TIMEOUT: Duration = Duration::from_secs(2);

/// What happened on one connection. **The only value that reaches a log.**
///
/// Every field is an outcome, a status, a count, a name, one clock reading,
/// or how the stream was framed and ended. There is nowhere here to put a
/// request body, a response body, a header value, a token or a credential —
/// which is a stronger statement than a promise not to log one, and
/// `an_exchange_has_nowhere_to_put_a_body` checks it against this
/// declaration.
#[derive(Debug)]
pub(super) struct Exchange {
    pub(super) outcome: Outcome,
    /// What the harness was told.
    pub(super) status: u16,
    /// The configured provider this gateway forwards to. A name.
    pub(super) provider: String,
    /// The slug of the protocol the request target was placed in, or `None`
    /// when it was refused before it could be placed. A name.
    pub(super) protocol: Option<String>,
    /// A name from [`crate::routing::evidence::CLIENT_NAMEABLE_PURPOSES`],
    /// read off the stripped [`PURPOSE_HEADER`] when its value is in that
    /// list; `None` otherwise — no header, an unknown value, or a request
    /// never reaching [`forward`]'s header loop. Never client text: this
    /// field is a name from a fixed list or nothing.
    pub(super) purpose: Option<String>,
    /// The bounded top-level model name observed on the request wire.
    /// This is what the client asked for, never an inferred backend identity.
    pub(super) requested_model: Option<String>,
    /// The upstream host **of the route that carried it**. A host: never a
    /// path, never a query. Empty when no route was chosen, because there is
    /// then no host this request was ever going to reach — and naming one
    /// anyway would be the log asserting something that did not happen.
    pub(super) host: String,
    /// The instant the provider's status and headers became available to
    /// this relay — a clock reading, never a byte of the response itself.
    /// `None` whenever no response ever arrived: refused before a route was
    /// chosen, refused before an upstream connection was opened, or the
    /// provider could not be reached at all. See this module's own "a third
    /// thing may now be recorded" for what this may and may not become.
    pub(super) first_byte_at: Option<i64>,
    /// The instant the first real generated token passed the seam — see this
    /// module's own "a fifth thing may now be recorded" for the translated
    /// path and "a seventh" for the relayed one. `None` on every refused
    /// exchange, on a response whose protocol has no entry in
    /// [`super::usage`]'s table, and on an answer that never carried one.
    pub(super) first_token_at: Option<i64>,
    /// The instant the first tool-use block started — the same rule and the
    /// same `None` cases as [`Self::first_token_at`].
    pub(super) first_tool_call_at: Option<i64>,
    /// Milliseconds from the instant this exchange's upstream request was
    /// **sent** to the instant the provider's status and headers were in
    /// hand — `crate::database` migration 25, and an offset rather than a
    /// clock reading. Measured from a monotonic [`Instant`] taken
    /// immediately before the send, so it is never negative and never
    /// derived by subtracting two wall-clock readings. `None` on every path
    /// that never sent a request, and on every path that never got an
    /// answer.
    pub(super) first_byte_ms: Option<i64>,
    /// [`Self::first_byte_ms`]'s sibling for the first real generated token —
    /// the same rule and the same `None` cases as [`Self::first_token_at`],
    /// and stamped from the same clock reading, so the two can never
    /// describe different moments.
    pub(super) first_token_ms: Option<i64>,
    /// [`Self::first_byte_ms`]'s sibling for the first tool-use block start.
    pub(super) first_tool_call_ms: Option<i64>,
    /// [`Self::first_byte_ms`]'s sibling for the instant this exchange
    /// stopped moving bytes, on both paths. `None` whenever the request
    /// never left, which is exactly when `first_byte_ms` is `None` too.
    pub(super) completed_ms: Option<i64>,
    /// How the provider's response was framed and how its stream ended —
    /// `None` on every path where no response arrived, exactly like
    /// `first_byte_at`. See this module's own "a fourth thing may now be
    /// recorded".
    pub(super) framing: Option<Framing>,
    /// The context window the provider stated while refusing this request as
    /// too long, in tokens -- [`super::context_limit`]. A count, never a
    /// piece of the provider's sentence: what is kept is the number the route
    /// enforces, which is the one figure a published catalogue cannot know
    /// (`archive/glasshouse:docs/product/design-decisions.md`, *A context window is a property
    /// of the route, not of the model*). `None` on every exchange that was
    /// not refused for length, and on every refusal whose wording this
    /// gateway does not recognise.
    pub(super) context_limit_tokens: Option<u64>,
    /// Token counts the provider stated — exact on a **translated** exchange
    /// because that response was parsed (the module's "narrowed and not
    /// repealed"), and exact on a **relayed** one whose protocol
    /// [`super::usage`] has a spelling for and whose stream ended cleanly
    /// (the module's "a seventh thing"). `None` — unknown, never an estimate
    /// — everywhere else.
    pub(super) tokens: Option<Tokens>,
    /// The four-word effort the request carried, on a **translated**
    /// exchange whose harness asked for thinking — `crate::database`
    /// migration 24. A name from a fixed vocabulary, derived from the
    /// request this gateway had to decode anyway in order to translate it,
    /// and never a byte of it. `None` on every relayed exchange and on a
    /// translated request that asked for no thinking.
    pub(super) effort: Option<EffortLevel>,
    /// Whether the request's last user turn was handing tool results back or
    /// writing a new prompt — migration 24, and [`Self::effort`]'s rule for
    /// `None`. A name from a two-word vocabulary; see
    /// `translate::canonical::Request::turn_shape`.
    pub(super) turn_shape: Option<TurnShape>,
    /// How many tool-use blocks the response requested, on a **translated**
    /// exchange that reached a response — line 1334's `tool_rounds`. See
    /// this module's own "a sixth thing may now be recorded". `None` on
    /// every relayed exchange and on a translated exchange that never
    /// reached a response; `Some(0)` when the seam looked and found none.
    pub(super) tool_rounds: Option<u32>,
    /// How many `is_error: true` tool-result blocks the request carried, on
    /// a **translated** exchange whose request decoded — line 1334's
    /// `repairs`. [`Self::tool_rounds`]'s sibling and the same `None`-vs-
    /// `Some(0)` rule, decided one step earlier: a decoded request always
    /// answers this, whether or not a response ever arrived.
    pub(super) repairs: Option<u32>,
}

/// Token counts the provider stated — Phase 56's consequence for the refusal
/// register's P1b, and since the 2026-09-03 ruling the relayed path's too.
/// Never derived: every value here was written as digits by the provider, on
/// whichever path read them.
///
/// Three counts and nothing else. `an_exchange_has_nowhere_to_put_a_body`
/// scans this declaration under [`Outcome`]'s stricter list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Tokens {
    pub(super) input: u64,
    pub(super) output: u64,
    /// Input tokens the provider served from a prompt cache, when it said.
    pub(super) cached: Option<u64>,
}

/// How the provider's response was framed and how its stream ended — the
/// facts this relay must already handle to move the bytes at all, and
/// nothing it had to look inside them to learn. Capability map line 1364's
/// `stream abort` and `empty completion`; see the module's own "a fourth
/// thing may now be recorded".
///
/// Every field is a count or a way of ending.
/// `an_exchange_has_nowhere_to_put_a_body` scans this declaration under the
/// same rule as [`Exchange`]'s and [`Outcome`]'s.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Framing {
    /// The length the provider declared, when it declared one. `None` for a
    /// chunked or close-delimited stream, whose length nobody knew in
    /// advance.
    pub(super) declared: Option<u64>,
    /// How many bytes of the response were relayed, or `None` when the
    /// framing said none could follow at all — a `HEAD` response, a `204`,
    /// a `304` — so that "nothing arrived" and "nothing was permitted" stay
    /// two different facts. A size, never what the bytes were.
    pub(super) relayed: Option<u64>,
    pub(super) ended: StreamEnd,
}

/// How the provider's response stream ended, judged only against what its
/// own framing said should happen.
///
/// A close-delimited stream — no declared length, no chunking — has no way
/// to say where it meant to end, so one that was cut off reads as
/// [`StreamEnd::Complete`]. Recorded as a limit rather than guessed at: no
/// provider protocol this gateway serves answers close-delimited in
/// practice, and inventing a verdict for one would be reading intent into
/// silence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum StreamEnd {
    /// Where the framing said it would: the declared length was reached, the
    /// terminating chunk arrived, or no body was permitted in the first
    /// place.
    Complete,
    /// The provider's stream stopped short of the length it declared.
    Truncated,
    /// The provider's unbounded stream failed before its terminating chunk.
    Aborted,
    /// The harness closed its side first. The provider's stream was left
    /// unread past that point, so how it would have ended is not known, and
    /// nothing about the provider is concluded from it.
    ClientClosed,
}

impl StreamEnd {
    pub(super) fn as_str(self) -> &'static str {
        match self {
            Self::Complete => "complete",
            Self::Truncated => "truncated",
            Self::Aborted => "aborted",
            Self::ClientClosed => "client-closed",
        }
    }
}

/// A reader that counts what passes through it, remembers whether the
/// provider's side failed, and — since the 2026-09-03 ruling — offers each
/// chunk to a bounded usage observer on its way past.
///
/// **It still returns exactly what `inner` returned.** Every observation
/// below is made from a shared borrow of the buffer the caller is about to
/// write; there is no path here that shortens, reorders or rewrites a read,
/// which is what "the forwarded bytes are preserved" means at the only place
/// it could stop being true. An `Interrupted` read is passed through
/// untouched, exactly as [`super::http::pump`] retries it, and is not a
/// failure.
///
/// [`Self::usage`] is `None` — nothing is looked at at all — whenever the
/// route's protocol has no established usage spelling; see
/// [`usage::format_for`].
struct Counted<R> {
    inner: R,
    relayed: u64,
    upstream_failed: bool,
    /// The bounded observer, or `None` for a protocol whose usage this relay
    /// has no spelling for. See [`super::usage`] for what it may read.
    usage: Option<usage::Extractor>,
    /// Migration 25's zero, so a marker's two readings — the unix second and
    /// the offset from the send — describe the same instant.
    dispatch: Instant,
    /// The instant the first generated token passed, as the pair
    /// `(first_token_at, first_token_ms)`. Latched on the first sighting.
    first_token: Option<(i64, i64)>,
    /// [`Self::first_token`]'s sibling for the first tool call.
    first_tool_call: Option<(i64, i64)>,
    /// A bounded copy of a **refusal's** body, and only a refusal's.
    ///
    /// `Some` exactly when the provider answered with a client error, capped
    /// at [`super::context_limit::SCAN_LIMIT_BYTES`]. It exists for one
    /// question -- did the provider state the window it enforces
    /// ([`super::context_limit`]) -- and nothing but the integer that
    /// question yields outlives this struct: the buffer is read once after
    /// the pump and dropped with the reader. A successful response is never
    /// copied here at any size.
    refusal: Option<Vec<u8>>,
}

impl<R: Read> Read for Counted<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        match self.inner.read(buf) {
            Ok(read) => {
                self.relayed += read as u64;
                if let Some(extractor) = self.usage.as_mut() {
                    let seen = extractor.feed(&buf[..read]);
                    if seen.first_text && self.first_token.is_none() {
                        self.first_token = Some(self.now());
                    }
                    if seen.first_tool_call && self.first_tool_call.is_none() {
                        self.first_tool_call = Some(self.now());
                    }
                }
                if let Some(refusal) = self.refusal.as_mut()
                    && refusal.len() < super::context_limit::SCAN_LIMIT_BYTES
                {
                    let room = super::context_limit::SCAN_LIMIT_BYTES - refusal.len();
                    refusal.extend_from_slice(&buf[..read.min(room)]);
                }
                Ok(read)
            }
            Err(err) if err.kind() == std::io::ErrorKind::Interrupted => Err(err),
            Err(err) => {
                self.upstream_failed = true;
                Err(err)
            }
        }
    }
}

impl<R> Counted<R> {
    /// Both readings of the instant a marker passed the seam: the unix
    /// second the row's `*_at` column holds, and migration 25's milliseconds
    /// since the upstream request was sent. Read together for
    /// `translate::FirstEvents::note`'s reason — asking twice would let the
    /// pair drift by whatever ran in between.
    fn now(&self) -> (i64, i64) {
        (
            crate::provider::cache::now_unix_seconds(),
            millis_since(self.dispatch),
        )
    }
}

/// How a connection ended.
#[derive(Debug)]
pub(super) enum Outcome {
    /// Forwarded, and the provider answered. `upstream_status` is what the
    /// provider said and `bytes` is how much body was moved — a size, never
    /// a content.
    Forwarded { upstream_status: u16, bytes: u64 },
    /// The bearer token was absent or wrong. **Nothing was opened
    /// upstream**; the refusal happens before a connection exists.
    Unauthenticated,
    /// A request the gateway will not carry: malformed, oversized, or framed
    /// with `transfer-encoding` — see [`super::http::read_head`].
    Declined,
    /// The request target belongs to none of the protocols this gateway's
    /// upstream serves. **Nothing was opened upstream**: a target the
    /// gateway cannot place is one it must not append to whichever base URL
    /// was declared first.
    Unrouted,
    /// The provider could not be reached at all, so there is no status to
    /// forward. `detail` is one of [`transport_detail`]'s fixed phrases —
    /// `&'static str`, so it is a string written in this file and can never
    /// be text something else produced.
    Unreachable { detail: &'static str },
    /// The client hung up part-way through.
    ClientGone,
    /// A connection that sent no request at all — a port scan, or a health
    /// check. Not worth a response.
    Idle,
}

impl Exchange {
    /// Record this exchange at debug level.
    ///
    /// Every field is named explicitly rather than rendered as one blob, so
    /// the event is structured and a reader can see, field by field, that
    /// there is nothing here but an outcome, two statuses, a size and two
    /// names. Widening what is logged means widening a type that has nowhere
    /// to put a body.
    ///
    /// Debug level, and Glasshouse's logging is off unless `GLASSHOUSE_LOG`
    /// is set — see [`crate::logging`]. That is what "gateway logs are
    /// opt-in" means here: the existing mechanism, not a second switch.
    pub(super) fn record(&self) {
        let (outcome, upstream_status, bytes, detail) = match &self.outcome {
            Outcome::Forwarded {
                upstream_status,
                bytes,
            } => ("forwarded", Some(*upstream_status), Some(*bytes), None),
            Outcome::Unauthenticated => ("unauthenticated", None, None, None),
            Outcome::Declined => ("declined", None, None, None),
            Outcome::Unrouted => ("unrouted", None, None, None),
            Outcome::Unreachable { detail } => ("unreachable", None, None, Some(*detail)),
            Outcome::ClientGone => ("client-gone", None, None, None),
            Outcome::Idle => ("idle", None, None, None),
        };
        // Three more counts-or-names, never a byte of what was relayed:
        // the length the provider declared, how much of it arrived, and how
        // the stream ended as its framing said it should.
        let (declared, relayed, ended) = match &self.framing {
            Some(framing) => (
                framing.declared,
                framing.relayed,
                Some(framing.ended.as_str()),
            ),
            None => (None, None, None),
        };
        tracing::debug!(
            outcome,
            status = self.status,
            upstream_status = ?upstream_status,
            bytes = ?bytes,
            detail = ?detail,
            declared = ?declared,
            relayed = ?relayed,
            ended = ?ended,
            provider = %self.provider,
            protocol = ?self.protocol,
            host = %self.host,
            tokens = ?self.tokens,
            // Two more names from fixed vocabularies, never a byte of the
            // request they were derived from — migration 24's `effort_level`
            // and `turn_shape`.
            effort = ?self.effort.map(EffortLevel::as_str),
            turn_shape = ?self.turn_shape.map(TurnShape::as_str),
            "gateway exchange"
        );
    }
}

/// Serve exactly one request on `stream`, and close.
///
/// # Why one request per connection
///
/// The inbound hop is loopback, where a new connection costs a syscall pair
/// and no handshake. The outbound hop is where reconnecting is expensive —
/// a TLS handshake to the provider — and that one *is* pooled, by `ureq`'s
/// own connection pool inside the shared [`Agent`]. So the cheap hop is kept
/// simple and the expensive hop is kept warm, which is the opposite of what
/// implementing keep-alive here would have optimised.
pub(super) fn serve(
    stream: TcpStream,
    token: &GatewayToken,
    upstream: &Upstream,
    agent: &Agent,
) -> (Exchange, RateLimitHeaders) {
    // **Not optional, and not tidiness.** The listener is non-blocking so
    // that shutdown cannot hang on `accept` — and on macOS, the BSDs and
    // Windows an accepted socket inherits that flag from its listener, while
    // on Linux it does not. A non-blocking stream would turn every read here
    // into `WouldBlock` on two of the three platforms Glasshouse supports.
    if stream.set_nonblocking(false).is_err() {
        return (exchange(Outcome::ClientGone, 0, upstream, None), no_quota());
    }
    let _ = stream.set_read_timeout(Some(HEAD_TIMEOUT));
    // Nagle's algorithm coalesces small writes and waits for an
    // acknowledgement before sending the next one; the receiver's delayed
    // acknowledgement then waits too. On a stream of small server-sent
    // events that pair adds a stall to every event, which is a latency
    // defect in exactly the property this gateway exists to preserve.
    // `ureq` already turns it off on the connection it makes to the
    // provider; this is the same decision on the connection the harness
    // makes to us.
    let _ = stream.set_nodelay(true);

    let Ok(mut out) = stream.try_clone() else {
        return (exchange(Outcome::ClientGone, 0, upstream, None), no_quota());
    };
    let mut reader = BufReader::new(stream);

    let head = match http::read_head(&mut reader) {
        Ok(head) => head,
        Err(HeadError::Empty) => {
            return (exchange(Outcome::Idle, 0, upstream, None), no_quota());
        }
        Err(HeadError::Io) => {
            return (exchange(Outcome::ClientGone, 0, upstream, None), no_quota());
        }
        Err(error) => {
            let (status, kind, message) = decline(&error);
            refuse(&mut out, status, kind, message, None);
            // The client may still be writing the body of a request whose
            // head was already refused — a chunked one, for instance. Closing
            // now would reset the connection and the client would see a
            // network error instead of the status explaining what was wrong.
            settle(&mut reader, &mut out, None);
            return (
                exchange(Outcome::Declined, status.as_u16(), upstream, None),
                no_quota(),
            );
        }
    };

    if !presented_token_matches(&head, token) {
        // Before any upstream connection is opened — which is asserted on the
        // upstream's own connection count, not on this ordering.
        refuse(
            &mut out,
            StatusCode::UNAUTHORIZED,
            "authentication_error",
            UNAUTHENTICATED_MESSAGE,
            Some(&head.method),
        );
        settle(&mut reader, &mut out, head.content_length);
        return (
            exchange(Outcome::Unauthenticated, 401, upstream, None),
            no_quota(),
        );
    }

    let purpose = client_purpose(&head);
    forward(head, reader, &mut out, upstream, agent, purpose)
}

fn client_purpose(head: &http::RequestHead) -> Option<String> {
    head.headers.iter().find_map(|(name, value)| {
        name.as_str()
            .eq_ignore_ascii_case(PURPOSE_HEADER)
            .then(|| value.to_str().ok())
            .flatten()
            .filter(|value| crate::routing::evidence::CLIENT_NAMEABLE_PURPOSES.contains(value))
            .map(str::to_owned)
    })
}

/// The three rewrites, and everything that is not one: the request target is
/// appended to the base URL for the protocol it belongs to
/// ([`Upstream::route_for`], [`Route::uri_for`]); `authorization` is
/// replaced with the provider's own credential
/// ([`Upstream::authorization`]); `host` is dropped so the outbound layer
/// derives the upstream's own. Everything else — method, every other
/// header, every body byte, and the hop-by-hop headers of
/// [`super::http::HOP_BY_HOP`] — passes through unrewritten.
///
/// Which protocol is decided **by the request target alone**, chosen before
/// a byte of the body is read: looking at the body to guess a protocol would
/// make this module a parser of the payload it exists to be unable to
/// distinguish from any other bytes. A target belonging to no served
/// protocol gets a `404` and **nothing is opened upstream**. Once routing is
/// fixed, a bounded observer may retain the request's top-level model for
/// evidence, without using it to place or rewrite the request.
///
/// History: design-decisions.md, "Trims: gateway/ingress.rs", fn forward.
fn forward(
    head: http::RequestHead,
    mut reader: BufReader<TcpStream>,
    out: &mut TcpStream,
    upstream: &Upstream,
    agent: &Agent,
    purpose: Option<String>,
) -> (Exchange, RateLimitHeaders) {
    // The serving backend is read **once**, here, and used for the whole of
    // this exchange. Phase 9H's failover moves which backend serves from
    // another thread; reading it twice would let one request take its route
    // from one provider and its credential from another.
    // Per-model routing, from the **head** and never the body. A harness that
    // names its model in `MODEL_HEADER` reaches the account declaring that
    // model, so one session can reason on one provider's model and reduce on
    // another's. Absent, or declared by nobody, this is exactly the session's
    // own backend — which is every request that existed before the header did.
    let requested = head
        .headers
        .get(super::http::MODEL_HEADER)
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|model| !model.is_empty());
    // `serving_for_target` is `serving_for`'s own answer for every target the
    // model-chosen backend claims, and only steps in for the one it does not
    // — see its doc comment for why that never ranks between two claimants.
    let serving = upstream.serving_for_target(requested, &head.target);
    let Some(route) = serving.route_for(&head.target) else {
        // Phase 56's one branch — see `unrouted`. A served target has a
        // route and never reaches it.
        return unrouted(head, reader, out, upstream, serving, agent, purpose);
    };

    let Some(uri) = route.uri_for(&head.target) else {
        refuse(
            out,
            StatusCode::BAD_REQUEST,
            "invalid_request_error",
            "the request target could not be appended to the configured provider's base URL",
            Some(&head.method),
        );
        // The refusal is only useful if the client can read it, which is
        // `settle`'s whole subject: the body this request declared is still
        // arriving, and closing under it resets the connection.
        settle(&mut reader, out, head.content_length);
        return (
            exchange(Outcome::Declined, 400, upstream, Some(route)),
            no_quota(),
        );
    };

    // `Some` only when the header named a purpose from the fixed list — see
    // `Exchange::purpose`. The header itself is never forwarded, whatever
    // its value: the `continue` below drops it before the `is_hop_by_hop`
    // check even runs.
    let mut request = Request::builder().method(head.method.clone()).uri(uri);
    for (name, value) in head.headers.iter() {
        if name.as_str().eq_ignore_ascii_case(PURPOSE_HEADER) {
            continue;
        }
        if http::is_hop_by_hop(name) || name == header::HOST || name == header::AUTHORIZATION {
            continue;
        }
        request = request.header(name.clone(), value.clone());
    }
    request = request.header(header::AUTHORIZATION, serving.authorization());

    let mut model_observation = None;
    let body = match head.content_length {
        Some(length) => {
            request = request.header(header::CONTENT_LENGTH, HeaderValue::from(length));
            // The body is moved from the client socket to the provider
            // socket without ever being held whole: `take` bounds it at the
            // length the client declared and nothing copies it into a
            // buffer of its own.
            let (observed, observation) = request_model::observe(reader.take(length), length);
            model_observation = Some(observation);
            SendBody::from_owned_reader(observed)
        }
        None => SendBody::none(),
    };

    let Ok(request) = request.body(body) else {
        refuse(
            out,
            StatusCode::BAD_REQUEST,
            "invalid_request_error",
            "the request could not be rebuilt for the configured provider",
            Some(&head.method),
        );
        // Past the point where the body was handed to the outbound hop, so
        // the drain goes through `out` — see `settle_queued`.
        settle_queued(out);
        return (
            Exchange {
                purpose: purpose.clone(),
                ..exchange(Outcome::Declined, 400, upstream, Some(route))
            },
            no_quota(),
        );
    };

    // Migration 25's zero, and the reason it is taken *here* rather than in
    // the accept loop: `dispatched_at` up there is the instant the
    // connection was handed to `serve`, which is before the head was read
    // and before this request was rebuilt for the provider. The offsets
    // measure the provider's own responsiveness, so their zero is the send.
    // A monotonic `Instant`, never a wall clock: two wall readings
    // subtracted across a clock step produce a negative "duration", which is
    // what migration 25's `CHECK` refuses and what this measurement cannot
    // produce at all.
    let dispatch = Instant::now();
    let response = agent.run(request);
    let requested_model = model_observation.and_then(|observation| observation.model());
    let response = match response {
        Ok(response) => response,
        Err(err) => {
            let detail = transport_detail(&err);
            refuse(
                out,
                StatusCode::BAD_GATEWAY,
                "api_error",
                "the gateway could not reach the configured provider",
                Some(&head.method),
            );
            // The body was handed to `agent.run` and dropped there unread
            // when the connection failed, so every byte of it the reader had
            // not already buffered is still queued on this socket. Closing
            // over it is the reset the Windows leg reported — see
            // `settle_queued`.
            settle_queued(out);
            return (
                Exchange {
                    purpose: purpose.clone(),
                    requested_model: requested_model.clone(),
                    ..exchange(Outcome::Unreachable { detail }, 502, upstream, Some(route))
                },
                no_quota(),
            );
        }
    };
    // The status and headers just arrived — see this module's own "a third
    // thing may now be recorded". Read once, here, before anything below
    // touches the body: every return past this point carries it.
    let first_byte_at = Some(crate::provider::cache::now_unix_seconds());
    // The same instant, measured against the send rather than named on the
    // wall clock — migration 25's `first_byte_ms`.
    let first_byte_ms = Some(millis_since(dispatch));

    let (parts, mut body) = response.into_parts();
    let status = parts.status;
    // A rate-limited account rests, so the next request for a model its
    // pool shares goes to the next account in it (`Upstream::for_model`).
    if status == StatusCode::TOO_MANY_REQUESTS {
        upstream.cool_down(serving.account(), rest_after(&parts.headers));
    }
    let declared_length = body.content_length();
    // Capability map line 1229's gateway half. Headers only, read before
    // anything below rewrites or filters them for relay — never the body,
    // which stays a byte stream this function never parses. See the module
    // documentation's "a second thing may now be recorded" entry.
    let quota = RateLimitHeaders::read(
        parts
            .headers
            .iter()
            .filter_map(|(name, value)| Some((name.as_str(), value.to_str().ok()?))),
    );
    // How the provider delivered it, from the one header that says so —
    // read here beside the quota headers, before anything below touches the
    // body. See [`usage::Delivery`] for why an instant inside a document
    // would be a reading of the socket rather than of the provider.
    let delivery = match parts.headers.get(header::CONTENT_TYPE) {
        Some(value) if value.as_bytes().starts_with(b"text/event-stream") => {
            usage::Delivery::Streamed
        }
        _ => usage::Delivery::Document,
    };
    // A `HEAD` response carries no body however ordinary its status is, and
    // writing one would be read by the client as the start of the *next*
    // response. No harness in scope sends `HEAD`; the method is forwarded
    // rather than vetted, so this is here because the method can arrive and
    // not because something sends it.
    let carries_body = status_carries_a_body(status) && head.method != ureq::http::Method::HEAD;

    let mut headers: Vec<(String, Vec<u8>)> = parts
        .headers
        .iter()
        .filter(|(name, _)| !http::is_hop_by_hop(name))
        .map(|(name, value)| (name.as_str().to_owned(), value.as_bytes().to_vec()))
        .collect();

    // Framing belongs to this hop, so it is decided here rather than copied.
    // A length the provider declared is re-stated; anything else — a chunked
    // response, an HTTP/2 one, a close-delimited one — is re-framed as
    // chunked, which is the only framing that can carry a stream whose
    // length nobody knows yet.
    let chunked = carries_body && declared_length.is_none();
    if carries_body {
        match declared_length {
            Some(length) => {
                headers.push(("content-length".to_owned(), length.to_string().into_bytes()))
            }
            None => headers.push(("transfer-encoding".to_owned(), b"chunked".to_vec())),
        }
    }
    // One request per connection — see `serve`. Said out loud so the client
    // does not wait for a second response on a socket that is about to
    // close.
    headers.push(("connection".to_owned(), b"close".to_vec()));

    // Capability map line 2451: this response was actually served, by
    // `serving` — never pushed on a refusal, since nothing served those.
    ServedBy::of(serving).push_onto(&mut headers);

    // The framing, as known before a byte of the body has moved: what the
    // provider declared, and whether a body may follow at all. `relayed` is
    // `Some(0)` rather than `None` the moment a body is permitted, so a
    // stream that then delivers nothing reads as *empty*, not as *no body
    // was expected*.
    let mut framing = Framing {
        declared: declared_length,
        relayed: carries_body.then_some(0),
        ended: StreamEnd::Complete,
    };

    if http::write_head(out, status, &headers).is_err() {
        // The provider did answer — its headers were captured above — even
        // though the harness never saw them: the client going away here is a
        // fact about the inbound hop, not about whether the outbound one
        // produced a reading.
        framing.ended = StreamEnd::ClientClosed;
        return (
            Exchange {
                first_byte_at,
                first_byte_ms,
                completed_ms: Some(millis_since(dispatch)),
                context_limit_tokens: None,
                framing: Some(framing),
                purpose: purpose.clone(),
                requested_model: requested_model.clone(),
                ..exchange(Outcome::ClientGone, status.as_u16(), upstream, Some(route))
            },
            quota,
        );
    }

    let mut moved = 0;
    // What the provider stated about its own usage, and when the first
    // generated token and tool call passed — `None` until the stream both
    // ends cleanly and turns out to have stated them. See the module's own
    // "a seventh thing may now be recorded".
    let mut tokens = None;
    let mut first_token = None;
    let mut first_tool_call = None;
    // What the provider said it enforces, when it refused this request for
    // being too long. `None` on every other exchange.
    let mut context_limit_tokens = None;
    if carries_body {
        // `Counted` is the observer: it sees how many bytes each read
        // returned and whether the provider's side failed, and `pump` still
        // does every byte of the moving. The relayed count is read off the
        // observer on every path, including the two where `pump` returns no
        // count of its own.
        let mut counted = Counted {
            inner: body.as_reader(),
            relayed: 0,
            upstream_failed: false,
            // The format is chosen from the route's protocol slug — the
            // decision `route_for` already made from the target alone — and
            // never from the body, for the reason this function's own doc
            // gives about placing a request. A slug with no entry is read as
            // nothing at all.
            usage: usage::format_for(route.protocol())
                .map(|format| usage::Extractor::new(format, delivery)),
            dispatch,
            first_token: None,
            first_tool_call: None,
            // Only a refusal is copied, and only far enough to read one
            // integer out of it. A 2xx response is relayed without this
            // struct retaining a byte of it, exactly as before.
            refusal: status.is_client_error().then(Vec::new),
        };
        let pumped = http::pump(&mut counted, out, chunked);
        moved = counted.relayed;
        framing.relayed = Some(moved);
        first_token = counted.first_token;
        first_tool_call = counted.first_tool_call;
        match pumped {
            Ok(_) => {
                // A clean end from `ureq` that nonetheless fell short of the
                // declared length is a truncation too; `ureq` reports that
                // as an error today, and this comparison does not depend on
                // it continuing to.
                if declared_length.is_some_and(|declared| moved < declared) {
                    framing.ended = StreamEnd::Truncated;
                }
            }
            Err(_) if counted.upstream_failed => {
                // The provider's stream failed under the relay. The harness
                // has been sent exactly what arrived and the socket closes
                // short — short of the `content-length` it was told, or
                // without the terminating chunk — which is how it learns
                // the stream was cut, and nothing here pretends otherwise
                // by writing a terminator the provider never sent.
                framing.ended = match declared_length {
                    Some(_) => StreamEnd::Truncated,
                    None => StreamEnd::Aborted,
                };
            }
            Err(_) => {
                framing.ended = StreamEnd::ClientClosed;
                return (
                    Exchange {
                        first_byte_at,
                        first_byte_ms,
                        completed_ms: Some(millis_since(dispatch)),
                        context_limit_tokens: None,
                        framing: Some(framing),
                        purpose: purpose.clone(),
                        requested_model: requested_model.clone(),
                        ..exchange(Outcome::ClientGone, status.as_u16(), upstream, Some(route))
                    },
                    quota,
                );
            }
        }
        // Only a stream that ended where its own framing said it would has a
        // usage figure worth writing down. A truncated, aborted or
        // client-closed stream may well have stated an input count before it
        // stopped, and pairing that with an output count the provider never
        // finished stating is the estimate the ruling forbids — so the row
        // says unknown, which is a different fact from zero.
        // The route answering for itself. A refusal states the window it
        // enforces, which no catalogue can know; the integer is taken here
        // and the bytes it came from are dropped with `counted`.
        context_limit_tokens = counted
            .refusal
            .as_deref()
            .and_then(|bytes| std::str::from_utf8(bytes).ok())
            .and_then(super::context_limit::stated_limit);
        if framing.ended == StreamEnd::Complete {
            tokens = counted
                .usage
                .as_ref()
                .and_then(usage::Extractor::usage)
                .map(|stated| Tokens {
                    input: stated.input,
                    output: stated.output,
                    cached: stated.cached,
                });
        }
    } else {
        let _ = out.flush();
    }
    // The write half only. Shutting down the read half as well is harmless on
    // Unix and an abortive close on Windows: `shutdown(SD_RECEIVE)` there
    // resets the connection the moment any byte is still queued or arrives
    // afterwards, and the harness then reads a connection reset in place of
    // the response it was just sent (the Windows VM leg, 2026-09-02,
    // `conformance::no_rendering_the_gateway_can_produce_carries_either_planted_secret`).
    // The socket is dropped right after this, which closes the read half on
    // every platform without that hazard.
    let _ = out.shutdown(Shutdown::Write);

    (
        Exchange {
            first_byte_at,
            first_byte_ms,
            // The exchange is over here — every byte has been relayed and
            // the socket is shut down — so this is the reading migration
            // 25's `completed_ms` means, taken before anything below it can
            // push it later.
            completed_ms: Some(millis_since(dispatch)),
            framing: Some(framing),
            // The seventh thing. Both readings of each instant come from the
            // one `Counted::now` call that stamped it, so a `*_at` and its
            // `*_ms` sibling can never describe different moments.
            first_token_at: first_token.map(|(at, _)| at),
            first_token_ms: first_token.map(|(_, ms)| ms),
            first_tool_call_at: first_tool_call.map(|(at, _)| at),
            first_tool_call_ms: first_tool_call.map(|(_, ms)| ms),
            tokens,
            context_limit_tokens,
            purpose,
            requested_model,
            ..exchange(
                Outcome::Forwarded {
                    upstream_status: status.as_u16(),
                    bytes: moved,
                },
                status.as_u16(),
                upstream,
                Some(route),
            )
        },
        quota,
    )
}

/// The `404` a target belonging to no served protocol has always been
/// answered with.
const UNROUTED_MESSAGE: &str = "this request target does not belong to any protocol the \
                                gateway is serving; a gateway ingress carries only \
                                the protocols the configured provider declares a base URL for, \
                                and forwards nothing it cannot place";

/// A target the serving backend has no route for — the one branch a codec
/// may enter (Phase 56), and the branch that stays a `404` otherwise.
///
/// The decision is made from the target alone, by [`translate::place`], and
/// it is made *after* `route_for` said no: a served target was relayed byte
/// for byte by [`forward`] and never arrives here. A supported pair hands
/// the whole exchange to [`translate::serve`], which records it under the
/// pair's name. A refused pair, or a target under a translated protocol
/// other than its one endpoint, is a `404` whose body names the pair and
/// the table's reason. Anything else is the `404` exactly as before, and
/// **nothing is opened upstream** on any refusing path.
fn unrouted(
    head: http::RequestHead,
    mut reader: BufReader<TcpStream>,
    out: &mut TcpStream,
    upstream: &Upstream,
    serving: &UpstreamBackend,
    agent: &Agent,
    purpose: Option<String>,
) -> (Exchange, RateLimitHeaders) {
    let served = serving.served_protocols();
    let message = match translate::place(&head.target, &served) {
        translate::Placement::Translate(pair) => {
            return translate::serve(head, reader, out, upstream, serving, agent, pair, purpose);
        }
        translate::Placement::PairRefused { from, refused } => {
            translate::pair_refusal_message(from, &refused)
        }
        translate::Placement::TargetRefused { from } => translate::target_refusal_message(from),
        translate::Placement::Unplaceable => UNROUTED_MESSAGE.to_owned(),
    };
    refuse(
        out,
        StatusCode::NOT_FOUND,
        "not_found_error",
        &message,
        Some(&head.method),
    );
    // Drained before the socket closes for the same reason the `401` path
    // drains: a client still writing a body would get a connection reset
    // instead of the status that explains what was wrong.
    settle(&mut reader, out, head.content_length);
    (exchange(Outcome::Unrouted, 404, upstream, None), no_quota())
}

/// The `401` body, shared by the served and the deferred path so the two
/// cannot say different things about the same rule.
const UNAUTHENTICATED_MESSAGE: &str = "this request did not carry this gateway's own bearer token; the ingress is reachable only \
     by the client that was handed the token when the gateway started";

/// Refuse a request because this gateway has no upstream yet — see
/// `super::UpstreamSlot`. The head is read so the bearer rule and the `HEAD`
/// rule apply exactly as on a served request; nothing is opened upstream
/// because there is no upstream to open, and no exchange is recorded because
/// there is no provider to attribute one to.
pub(super) fn refuse_unserved(stream: TcpStream, token: &GatewayToken, reason: &str) {
    if stream.set_nonblocking(false).is_err() {
        return;
    }
    let _ = stream.set_read_timeout(Some(HEAD_TIMEOUT));
    let Ok(mut out) = stream.try_clone() else {
        return;
    };
    let mut reader = BufReader::new(stream);
    let head = match http::read_head(&mut reader) {
        Ok(head) => head,
        Err(HeadError::Empty | HeadError::Io) => return,
        Err(error) => {
            let (status, kind, message) = decline(&error);
            refuse(&mut out, status, kind, message, None);
            settle(&mut reader, &mut out, None);
            return;
        }
    };
    if !presented_token_matches(&head, token) {
        refuse(
            &mut out,
            StatusCode::UNAUTHORIZED,
            "authentication_error",
            UNAUTHENTICATED_MESSAGE,
            Some(&head.method),
        );
        settle(&mut reader, &mut out, head.content_length);
        return;
    }
    refuse(
        &mut out,
        StatusCode::SERVICE_UNAVAILABLE,
        "api_error",
        &json_text(reason),
        Some(&head.method),
    );
    settle(&mut reader, &mut out, head.content_length);
}

/// `text` as the inside of a JSON string literal. The refusal bodies this
/// file writes by hand are fixed text; a reason built from configured
/// provider names is not, and a quote in one must not end the body early.
fn json_text(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

/// No response was received, so there is nothing a quota reading could have
/// come from — every early-return path in [`serve`] and [`forward`] before a
/// provider answers.
fn no_quota() -> RateLimitHeaders {
    RateLimitHeaders::default()
}

/// Whether the presented bearer token is this instance's own.
///
/// # Constant time, as far as safe Rust can promise it
///
/// The comparison below folds every byte before answering, so it does not
/// return early on the first mismatch and its running time does not depend
/// on how many leading characters an attacker guessed. That is the property
/// that matters: a token is 256 bits of entropy and the only realistic
/// attack on it is one that learns a prefix.
///
/// It is **not** a hardware guarantee. Nothing in safe Rust stops an
/// optimiser from introducing a branch, and the honest fix is a crate whose
/// job that is — `subtle`, which is already in this workspace's lock file as
/// a transitive dependency of `rustls`. Promoting it to a direct dependency
/// was outside this slice's remit, so this is what is here and this comment
/// is the disclosure rather than a claim that it is equivalent.
fn presented_token_matches(head: &http::RequestHead, token: &GatewayToken) -> bool {
    let Some(value) = head.headers.get(header::AUTHORIZATION) else {
        return false;
    };
    let Ok(value) = value.to_str() else {
        return false;
    };
    let Some(presented) = value.strip_prefix(BEARER_PREFIX) else {
        return false;
    };
    constant_time_eq(presented.as_bytes(), token.expose().as_bytes())
}

/// `a == b`, without returning early.
///
/// A length mismatch is folded in rather than short-circuited, and the loop
/// still runs over the presented value, so an attacker learns no more from
/// the timing than "wrong".
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if b.is_empty() {
        // An empty expected token would make the index below divide by zero,
        // and would accept everything. It cannot happen — a `GatewayToken`
        // is 64 hex characters — but "cannot happen" is not a guard.
        return false;
    }
    let mut difference = u8::from(a.len() != b.len());
    for (index, byte) in a.iter().enumerate() {
        difference |= byte ^ b[index % b.len()];
    }
    difference == 0
}

/// Why the provider could not be reached, as one of a **fixed vocabulary**
/// written in this file — never the error's own text. `ureq`'s own string
/// was tried first and rejected: [`crate::secret::redact`] only removes
/// credential-shaped runs and makes no promise about the rest, so foreign
/// text could still leak (`a_recorded_exchange_writes_a_line_with_no_secret_in_it`
/// caught exactly that). The variant *is* read from the error — so the
/// answer is a real observation, never a constant — but the phrase it maps
/// to is always one written here.
///
/// [`TRANSPORT_TIMEOUT_DETAIL`] is named outside this function because
/// `super::session`'s `failure_class` tells a timeout from every other
/// transport failure by it; no upstream agent sets a timeout today, so this
/// arm has no live producer until one is configured.
///
/// History: design-decisions.md, "Trims: gateway/ingress.rs", fn transport_detail.
pub(super) fn transport_detail(err: &ureq::Error) -> &'static str {
    match err {
        ureq::Error::HostNotFound => "the provider's host name did not resolve",
        ureq::Error::ConnectionFailed => "the connection to the provider could not be made",
        ureq::Error::Io(_) => "the connection to the provider failed",
        ureq::Error::Timeout(_) => TRANSPORT_TIMEOUT_DETAIL,
        ureq::Error::Tls(_) | ureq::Error::Rustls(_) | ureq::Error::Pem(_) => {
            "the TLS connection to the provider could not be established"
        }
        ureq::Error::BadUri(_) | ureq::Error::Http(_) => {
            "the request could not be addressed to the provider"
        }
        ureq::Error::Protocol(_) => "the provider's answer was not valid HTTP",
        _ => "the provider could not be reached",
    }
}

/// The status, error kind and message for a head that would not parse.
fn decline(error: &HeadError) -> (StatusCode, &'static str, &'static str) {
    match error {
        HeadError::TooLarge => (
            StatusCode::REQUEST_HEADER_FIELDS_TOO_LARGE,
            "invalid_request_error",
            "the request head exceeded the size the gateway will read",
        ),
        HeadError::ChunkedRequest => (
            StatusCode::LENGTH_REQUIRED,
            "invalid_request_error",
            "the gateway forwards request bodies framed with content-length; a \
             chunked request body would have to be parsed to be re-framed",
        ),
        _ => (
            StatusCode::BAD_REQUEST,
            "invalid_request_error",
            "the gateway could not read this request",
        ),
    }
}

/// Write an error the harness can understand, in the shape its own protocol
/// uses.
///
/// The message is written here and never quotes the request: a malformed
/// head is still a head someone wrote, and echoing it back is how prompt
/// text ends up in a terminal scrollback. Nothing in this body is derived
/// from anything the client sent.
fn refuse(
    out: &mut TcpStream,
    status: StatusCode,
    kind: &str,
    message: &str,
    method: Option<&ureq::http::Method>,
) {
    let body = format!(
        "{{\"type\":\"error\",\"error\":{{\"type\":\"{kind}\",\"message\":\"{message}\"}}}}"
    );
    // A response to `HEAD` carries the headers a `GET` would have and none
    // of the body, and a client that reads the body anyway reads it as the
    // start of the next response. `forward` already applies this rule to a
    // provider's answer; a refusal written here needs it just as much, and
    // needs it more now — Claude Code 2.1.245's `HEAD /api/hello` belongs to
    // no protocol, so the refusal above is the first response in this
    // gateway's life that a `HEAD` can actually reach.
    //
    // `None` is a request whose head would not parse, so there is no method
    // to honour and the body is the only thing that can explain why.
    let carries_body = method != Some(&ureq::http::Method::HEAD);
    let headers = vec![
        ("content-type".to_owned(), b"application/json".to_vec()),
        (
            "content-length".to_owned(),
            body.len().to_string().into_bytes(),
        ),
        ("connection".to_owned(), b"close".to_vec()),
    ];
    if http::write_head(out, status, &headers).is_ok() {
        if carries_body {
            let _ = out.write_all(body.as_bytes());
        }
        let _ = out.flush();
    }
}

/// Let a refused request finish arriving, then close.
///
/// Both halves matter. Reading what is still in flight is what stops the
/// close below from becoming a connection reset that discards the response
/// the client was just sent — a client that got a reset sees a network
/// error, not the `401` or `411` that would have told it what was wrong.
/// And both the byte cap ([`DRAIN_CAP`]) and the time cap
/// ([`SETTLE_TIMEOUT`]) are there because this is work done on behalf of a
/// request that has already been refused: neither a large body nor a client
/// that stops sending may hold this thread.
pub(super) fn settle(
    reader: &mut BufReader<TcpStream>,
    out: &mut TcpStream,
    content_length: Option<u64>,
) {
    let _ = reader.get_ref().set_read_timeout(Some(SETTLE_TIMEOUT));
    let cap = content_length.unwrap_or(DRAIN_CAP).min(DRAIN_CAP);
    let _ = std::io::copy(&mut reader.take(cap), &mut std::io::sink());
    // The write half only, for the reason given at `serve`'s own close: on
    // Windows `shutdown(SD_RECEIVE)` resets the connection while bytes are
    // still queued — and a refused request capped by `DRAIN_CAP` or
    // `SETTLE_TIMEOUT` is exactly the case where some may be.
    let _ = out.shutdown(Shutdown::Write);
}

/// Discard whatever the client has already queued, then half-close.
///
/// The invariant is [`settle`]'s: a client that has been answered observes
/// an end of stream, and it holds because nothing of the client's is left
/// unread when the socket closes. Closing one whose receive queue still
/// holds the client's own bytes sends a reset in place of the `FIN`, and the
/// harness then reads a connection reset instead of the response it was just
/// sent — the Windows VM leg, run 5 and run 14,
/// `conformance::no_rendering_the_gateway_can_produce_carries_either_planted_secret`.
///
/// Why this is not [`settle`]: on the two refusals below the request body
/// was already handed to the outbound hop, so the reader that would have
/// drained it went with the request, and `socket` — a `try_clone` of the
/// same connection — is the only thing left that can empty the queue. It
/// reads non-blocking and stops the moment the socket says there is nothing
/// queued, so no wait stands in for the drain; [`DRAIN_CAP`] bounds a client
/// that is still writing, exactly as it bounds [`settle`]'s.
fn settle_queued(socket: &mut TcpStream) {
    if socket.set_nonblocking(true).is_ok() {
        let mut discard = [0u8; 8192];
        let mut drained: u64 = 0;
        while drained < DRAIN_CAP {
            match socket.read(&mut discard) {
                Ok(0) => break,
                Ok(read) => drained += read as u64,
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(_) => break,
            }
        }
        // Back to blocking before the close: `try_clone` duplicates the
        // descriptor and the flag is the socket's, not the descriptor's.
        let _ = socket.set_nonblocking(false);
    }
    let _ = socket.shutdown(Shutdown::Write);
}

/// Whether a response with this status is allowed to carry a body at all.
///
/// A `204` or a `304` with a `content-length` or a chunked framing is a
/// protocol error that some clients treat as the start of the *next*
/// response, so this is a correctness rule rather than an optimisation.
fn status_carries_a_body(status: StatusCode) -> bool {
    !(status.is_informational()
        || status == StatusCode::NO_CONTENT
        || status == StatusCode::NOT_MODIFIED)
}

/// Milliseconds elapsed since `dispatch`, as the ledger's column holds them.
///
/// [`Instant`] is monotonic on every platform Glasshouse ships on, so this
/// can never be negative and migration 25's `CHECK` can never fire on a
/// value this function produced. The saturation is for a duration no
/// exchange can survive to report — a `u128` of milliseconds that will not
/// fit an `i64` is roughly 292 million years — and exists so that the
/// conversion has one stated answer rather than a panic or a wrap.
pub(super) fn millis_since(dispatch: Instant) -> i64 {
    i64::try_from(dispatch.elapsed().as_millis()).unwrap_or(i64::MAX)
}

/// One [`Exchange`], with the upstream's non-secret identity filled in.
///
/// `route` is `None` for everything refused before a target could be placed.
/// It is not defaulted to the first route: a log that named a protocol and a
/// host for a request that never reached either would be inventing the one
/// fact it exists to record.
fn exchange(outcome: Outcome, status: u16, upstream: &Upstream, route: Option<&Route>) -> Exchange {
    Exchange {
        outcome,
        status,
        provider: upstream.provider().to_owned(),
        protocol: route.map(|route| route.protocol().to_owned()),
        // Every caller of this helper is a refusal path that returns before
        // `forward`'s header loop can have read `PURPOSE_HEADER` — the loop
        // is the only producer, and `forward`'s own returns override this
        // via struct-update syntax once it has run.
        purpose: None,
        requested_model: None,
        host: route.map(Route::host).unwrap_or_default(),
        // Every caller of this helper returns before a response ever
        // arrived; [`forward`]'s own three post-response returns override
        // both of these with the real readings via struct-update syntax.
        first_byte_at: None,
        // Line 1331/1332's pair: no response arrived on any path through
        // this helper, so no marker can have passed the seam and both are
        // `NULL`. [`forward`]'s own completed return overrides them from
        // `Counted`'s latched readings via struct-update syntax, and
        // `translate::serve` fills them on its path.
        first_token_at: None,
        first_tool_call_at: None,
        // No response arrived on any path through this helper, so nothing
        // was refused for length here; `forward`'s completed return
        // overrides this via struct-update syntax.
        context_limit_tokens: None,
        // Migration 25's four offsets, and the same rule one line up: every
        // caller of this helper returns before the upstream request was
        // sent or before an answer came back, so there is no monotonic zero
        // to measure from. [`forward`]'s own three post-response returns
        // override `first_byte_ms` and `completed_ms` via struct-update
        // syntax; the two token offsets stay `None` on this path for
        // `first_token_at`'s reason.
        first_byte_ms: None,
        first_token_ms: None,
        first_tool_call_ms: None,
        completed_ms: None,
        framing: None,
        // Unknown, and never an estimate: no response arrived on any path
        // through this helper, so there is nothing a provider stated for
        // this exchange at all.
        tokens: None,
        // The relay never decodes a request, so it has nothing to derive
        // either of these from and writes `NULL` for both — unread, not
        // absent. `translate::serve` fills them; nothing on this path does,
        // on any of its returns.
        effort: None,
        turn_shape: None,
        // Line 1334's pair: the relay never decodes a request or a
        // response, so it has nothing to count either from — `NULL` for
        // both, unread rather than absent, exactly like `effort` and
        // `turn_shape` above. `translate::serve` fills them; nothing on this
        // path does, on any of its returns.
        tool_rounds: None,
        repairs: None,
    }
}

/// How long a `429`'s account rests: its `Retry-After` seconds, held to
/// between thirty seconds and six hours, else ten minutes.
fn rest_after(headers: &ureq::http::HeaderMap) -> std::time::Duration {
    let seconds = headers
        .get(header::RETRY_AFTER)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.trim().parse::<u64>().ok())
        .unwrap_or(600);
    std::time::Duration::from_secs(seconds.clamp(30, 6 * 60 * 60))
}

#[cfg(test)]
mod tests {
    use super::*;

    use ureq::http::{HeaderMap, HeaderName, Method};

    fn head_with_authorization(value: &str) -> http::RequestHead {
        let mut headers = HeaderMap::new();
        headers.append(
            HeaderName::from_static("authorization"),
            HeaderValue::from_str(value).expect("a header-safe test value"),
        );
        http::RequestHead {
            method: Method::POST,
            target: "/v1/messages".to_owned(),
            headers,
            content_length: None,
        }
    }

    #[test]
    fn only_this_instances_own_token_is_accepted() {
        let token = GatewayToken::generate().expect("the OS has entropy");
        let other = GatewayToken::generate().expect("the OS has entropy");

        let presented = format!("Bearer {}", token.expose());
        assert!(presented_token_matches(
            &head_with_authorization(&presented),
            &token
        ));

        for wrong in [
            format!("Bearer {}", other.expose()),
            format!("Bearer {}", &token.expose()[..32]),
            format!("Bearer {}x", token.expose()),
            format!("bearer {}", token.expose()),
            format!("Basic {}", token.expose()),
            token.expose().to_owned(),
            "Bearer ".to_owned(),
            String::new(),
        ] {
            assert!(
                !presented_token_matches(&head_with_authorization(&wrong), &token),
                "a request presenting a token that is not this instance's own was accepted"
            );
        }

        // ... and no `authorization` header at all is not an accident that
        // passes.
        let bare = http::RequestHead {
            method: Method::POST,
            target: "/v1/messages".to_owned(),
            headers: HeaderMap::new(),
            content_length: None,
        };
        assert!(!presented_token_matches(&bare, &token));
    }

    #[test]
    fn the_comparison_folds_every_byte_rather_than_stopping_at_the_first() {
        assert!(constant_time_eq(b"abcd", b"abcd"));
        assert!(!constant_time_eq(b"abcd", b"abce"));
        assert!(!constant_time_eq(b"abcd", b"abcde"));
        assert!(!constant_time_eq(b"abcde", b"abcd"));
        assert!(!constant_time_eq(b"", b"abcd"));
        // An empty expected value must reject rather than accept, or a
        // gateway whose token failed to generate would accept everything.
        assert!(!constant_time_eq(b"anything", b""));
        assert!(!constant_time_eq(b"", b""));
    }

    /// Everything one call to [`Exchange::record`] actually writes.
    ///
    /// `tracing::subscriber::with_default` installs a **thread-local**
    /// default, not a global one, so this cannot race the other tests in
    /// this binary the way `set_global_default` would.
    fn recorded(exchange: &Exchange) -> String {
        use std::sync::{Arc, Mutex};

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

        let sink = Arc::new(Mutex::new(Vec::new()));
        let subscriber = tracing_subscriber::fmt()
            .with_writer(Capture(Arc::clone(&sink)))
            .with_max_level(tracing::Level::TRACE)
            .without_time()
            .finish();
        tracing::subscriber::with_default(subscriber, || exchange.record());

        let captured = sink
            .lock()
            .expect("no test panics while holding this")
            .clone();
        String::from_utf8_lossy(&captured).into_owned()
    }

    /// The packet asks that neither secret appear "in any Debug, log line, or
    /// error rendering". The `Debug` half is asserted in several places; this
    /// is the **log line** half, asserted against what `tracing` actually
    /// emitted rather than against the argument that it could not have
    /// emitted anything else.
    ///
    /// Lose this and the guard on what a gateway exchange writes to a log is
    /// a reading of [`Exchange`]'s declaration — which is a good guard, and
    /// is not the same as having seen the line.
    #[test]
    fn a_recorded_exchange_writes_a_line_with_no_secret_in_it() {
        const PROVIDER_CREDENTIAL: &str = "sk-ant-planted-provider-000111222333444";
        const PROMPT_BODY: &str = "PLANTED-PROMPT-BODY-DO-NOT-LOG";

        for outcome in [
            Outcome::Forwarded {
                upstream_status: 429,
                bytes: 4096,
            },
            Outcome::Unauthenticated,
            Outcome::Declined,
            Outcome::Unreachable {
                detail: transport_detail(&ureq::Error::Tls(
                    "planted certificate text carrying PLANTED-PROMPT-BODY-DO-NOT-LOG",
                )),
            },
            Outcome::ClientGone,
            Outcome::Idle,
            Outcome::Unrouted,
        ] {
            let exchange = Exchange {
                outcome,
                status: 429,
                provider: "openrouter".to_owned(),
                protocol: Some("anthropic-messages".to_owned()),
                purpose: None,
                requested_model: None,
                host: "openrouter.ai".to_owned(),
                first_byte_at: Some(1_700_000_000),
                first_token_at: Some(1_700_000_001),
                first_tool_call_at: Some(1_700_000_002),
                first_byte_ms: Some(120),
                first_token_ms: Some(1_100),
                first_tool_call_ms: Some(2_400),
                completed_ms: Some(3_600),
                context_limit_tokens: None,
                framing: Some(Framing {
                    declared: Some(4096),
                    relayed: Some(4096),
                    ended: StreamEnd::Complete,
                }),
                tokens: Some(Tokens {
                    input: 120,
                    output: 33,
                    cached: Some(100),
                }),
                effort: Some(EffortLevel::Medium),
                turn_shape: Some(TurnShape::ToolResume),
                tool_rounds: Some(2),
                repairs: Some(1),
            };
            let line = recorded(&exchange);

            assert!(
                !line.is_empty(),
                "nothing was recorded, so the scans below prove nothing"
            );
            assert!(
                !line.contains(PROVIDER_CREDENTIAL),
                "a provider credential reached a log line: {line}"
            );
            assert!(
                !line.contains(PROMPT_BODY),
                "text quoted out of a request reached a log line: {line}"
            );
            // ... and the line is still worth writing: the status and the
            // provider are what a user needs to see when a session starts
            // failing.
            assert!(line.contains("429"), "{line}");
            assert!(line.contains("openrouter"), "{line}");
        }
    }

    /// A structural rule, checked against the declaration rather than
    /// promised in prose: there is nowhere in an [`Exchange`] to put a body.
    /// Lose this and the first person who wants "just the error text for
    /// debugging" adds a `String` that holds a prompt.
    #[test]
    fn an_exchange_has_nowhere_to_put_a_body() {
        let source = include_str!("ingress.rs");
        let production = source
            .split("#[cfg(test)]")
            .next()
            .expect("split always yields at least one part");
        // Comment lines are dropped, the same way every other source scan in
        // this crate drops them: these declarations *describe* what they must
        // not hold, in prose that names it. A scan that could not tell the
        // description from the thing would have to be deleted the first time
        // someone wrote the rule down.
        let declaration_of = |header: &str| {
            let start = production
                .find(header)
                .unwrap_or_else(|| panic!("`{header}` is declared in this module"));
            let rest = &production[start..];
            let end = rest.find("\n}").expect("the declaration ends");
            rest[..end]
                .lines()
                .filter(|line| !line.trim_start().starts_with("//"))
                .collect::<Vec<_>>()
                .join("\n")
        };

        // Both types are logged, so both are scanned — but the lists differ,
        // and the difference is the rule rather than an oversight.
        //
        // `Exchange` names *who*: a configured provider and a host, both of
        // which the user wrote in their own configuration file. Owned strings
        // are right there.
        //
        // `Outcome` says *what happened*, and every one of its fields is a
        // status, a count, or a phrase written in this file. It used to hold
        // an owned `String` built from `ureq`'s own error text — see
        // `transport_detail` — and that is exactly the shape this scan exists
        // to refuse: an owned string is somewhere foreign text can be kept,
        // and a borrowed static one is not.
        let body_shaped = ["Vec<u8>", "[u8]", "Bytes", "body", "payload", "content"];
        let body_shaped_or_foreign_text = {
            let mut list = body_shaped.to_vec();
            list.push("String");
            list
        };
        for (name, header, forbidden) in [
            (
                "Exchange",
                "pub(super) struct Exchange {",
                body_shaped.to_vec(),
            ),
            (
                "Outcome",
                "pub(super) enum Outcome {",
                body_shaped_or_foreign_text.clone(),
            ),
            // The framing facts are counts and a way of ending, and the scan
            // holds them to `Outcome`'s stricter list: a `String` here would
            // be somewhere a chunk of the stream could be kept.
            (
                "Framing",
                "pub(super) struct Framing {",
                body_shaped_or_foreign_text.clone(),
            ),
            (
                "StreamEnd",
                "pub(super) enum StreamEnd {",
                body_shaped_or_foreign_text.clone(),
            ),
            // Three counts a translated exchange adds — held to the same
            // list, because a count is where "just the usage object" would
            // otherwise be kept whole.
            (
                "Tokens",
                "pub(super) struct Tokens {",
                body_shaped_or_foreign_text,
            ),
        ] {
            let declaration = declaration_of(header);
            assert!(
                !declaration.is_empty(),
                "{name}'s declaration was not found, so this scan would pass vacuously"
            );
            for needle in forbidden {
                assert!(
                    !declaration.contains(needle),
                    "{name}'s declaration names `{needle}`: what this gateway logs must be \
                     structurally unable to carry a body, or any other text produced outside \
                     this file"
                );
            }
        }
    }

    /// A reader that hands over three bytes and then fails, the way `ureq`
    /// fails a body that ends short of its `content-length`
    /// (`io::ErrorKind::UnexpectedEof`).
    struct ShortReader {
        served: bool,
    }

    impl Read for ShortReader {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            if self.served {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::UnexpectedEof,
                    "planted short read",
                ));
            }
            self.served = true;
            buf[..3].copy_from_slice(b"abc");
            Ok(3)
        }
    }

    /// The observer counts what passed and remembers that the provider's
    /// side failed — and `pump`, which does the moving, still surfaces the
    /// error rather than swallowing it into a clean count.
    #[test]
    fn the_counting_reader_counts_what_passed_and_remembers_an_upstream_failure() {
        let mut counted = Counted {
            inner: ShortReader { served: false },
            relayed: 0,
            upstream_failed: false,
            usage: None,
            dispatch: Instant::now(),
            first_token: None,
            first_tool_call: None,
            refusal: None,
        };
        let mut out = Vec::new();
        let pumped = http::pump(&mut counted, &mut out, false);
        assert!(pumped.is_err(), "the short read must surface as an error");
        assert_eq!(counted.relayed, 3);
        assert!(counted.upstream_failed);
        assert_eq!(
            out, b"abc",
            "what did arrive was relayed before the failure"
        );
    }

    /// The other side of the same distinction: a reader that ends cleanly is
    /// not an upstream failure, and the count agrees with `pump`'s own.
    #[test]
    fn the_counting_reader_agrees_with_pump_on_a_stream_that_ends_cleanly() {
        let mut counted = Counted {
            inner: &b"twelve bytes"[..],
            relayed: 0,
            upstream_failed: false,
            usage: None,
            dispatch: Instant::now(),
            first_token: None,
            first_tool_call: None,
            refusal: None,
        };
        let mut out = Vec::new();
        let moved = http::pump(&mut counted, &mut out, true).expect("a clean stream pumps");
        assert_eq!(moved, 12);
        assert_eq!(counted.relayed, moved);
        assert!(!counted.upstream_failed);
    }

    /// The route answering for itself: a refusal passes the relay unchanged
    /// and leaves behind one integer -- the window it enforces.
    #[test]
    fn a_refusal_is_relayed_whole_and_leaves_the_window_it_stated() {
        let body = br#"{"error":{"message":"prompt is too long: 213000 tokens > 200000 maximum"}}"#;
        let mut counted = Counted {
            inner: &body[..],
            relayed: 0,
            upstream_failed: false,
            usage: None,
            dispatch: Instant::now(),
            first_token: None,
            first_tool_call: None,
            // What `forward` passes for a client error.
            refusal: Some(Vec::new()),
        };
        let mut out = Vec::new();
        http::pump(&mut counted, &mut out, true).expect("a refusal relays like any body");
        // Chunk framing belongs to this hop; the provider's own bytes cross
        // it untouched, which is what "relayed whole" means here.
        assert!(
            out.windows(body.len()).any(|window| window == body),
            "the harness is sent exactly what arrived"
        );
        let stated = counted
            .refusal
            .as_deref()
            .and_then(|bytes| std::str::from_utf8(bytes).ok())
            .and_then(super::super::context_limit::stated_limit);
        assert_eq!(stated, Some(200_000));
    }

    /// A 2xx is never copied, at any size: the buffer exists for refusals.
    #[test]
    fn an_ordinary_response_is_not_copied_while_it_is_relayed() {
        let mut counted = Counted {
            inner: &b"a perfectly ordinary answer"[..],
            relayed: 0,
            upstream_failed: false,
            usage: None,
            dispatch: Instant::now(),
            first_token: None,
            first_tool_call: None,
            refusal: None,
        };
        let mut out = Vec::new();
        http::pump(&mut counted, &mut out, true).expect("a clean stream pumps");
        assert!(counted.refusal.is_none(), "nothing of a 2xx is retained");
    }

    #[test]
    fn a_status_that_may_not_carry_a_body_is_framed_as_carrying_none() {
        for status in [
            StatusCode::NO_CONTENT,
            StatusCode::NOT_MODIFIED,
            StatusCode::CONTINUE,
        ] {
            assert!(!status_carries_a_body(status), "{status}");
        }
        for status in [
            StatusCode::OK,
            StatusCode::TOO_MANY_REQUESTS,
            StatusCode::INTERNAL_SERVER_ERROR,
        ] {
            assert!(status_carries_a_body(status), "{status}");
        }
    }

    /// `Outcome::Unreachable`'s detail is the only place a transport failure
    /// says anything at all, and it says it in this file's own words.
    ///
    /// Two properties, and the second is the one that took a failing test to
    /// find. The phrase must distinguish the failures a user fixes
    /// differently — a refused connection and an unresolvable host have
    /// nothing to do with each other. And it must never be `ureq`'s own
    /// string: `crate::secret::redact` removes credential-shaped runs and
    /// makes no promise about the text around them, so a redacted foreign
    /// string still carries whatever else was in it.
    #[test]
    fn a_transport_detail_is_this_files_own_words_and_never_the_errors() {
        let refused = transport_detail(&ureq::Error::ConnectionFailed);
        let unresolved = transport_detail(&ureq::Error::HostNotFound);
        let tls = transport_detail(&ureq::Error::Tls("planted certificate text"));

        assert_ne!(
            refused, unresolved,
            "two transport failures with completely different fixes are reported identically"
        );
        assert_ne!(refused, tls);
        assert!(
            !tls.contains("planted certificate text"),
            "the error's own text reached the diagnostic: {tls}"
        );

        // Whatever a variant carries, the phrase is drawn from the fixed set
        // written above it. Checked as a set membership rather than one
        // string at a time, so a variant mapped to an interpolated string
        // fails here.
        let vocabulary = [
            "the provider's host name did not resolve",
            "the connection to the provider could not be made",
            "the connection to the provider failed",
            "the provider did not answer in time",
            "the TLS connection to the provider could not be established",
            "the request could not be addressed to the provider",
            "the provider's answer was not valid HTTP",
            "the provider could not be reached",
        ];
        for err in [
            ureq::Error::ConnectionFailed,
            ureq::Error::HostNotFound,
            ureq::Error::Tls("planted certificate text"),
            ureq::Error::BadUri("https://planted.example/sk-ant-planted".to_owned()),
            ureq::Error::BodyExceedsLimit(1),
        ] {
            let detail = transport_detail(&err);
            assert!(
                vocabulary.contains(&detail),
                "a transport detail escaped the fixed vocabulary: {detail:?}"
            );
        }
    }

    #[test]
    fn a_declined_head_maps_to_the_status_that_says_why() {
        assert_eq!(
            decline(&HeadError::TooLarge).0,
            StatusCode::REQUEST_HEADER_FIELDS_TOO_LARGE
        );
        assert_eq!(
            decline(&HeadError::ChunkedRequest).0,
            StatusCode::LENGTH_REQUIRED
        );
        assert_eq!(decline(&HeadError::Malformed).0, StatusCode::BAD_REQUEST);
    }
}
