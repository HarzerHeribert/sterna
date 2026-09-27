//! Real network probes against a configured provider: connectivity, and the
//! model catalogue behind a `GET <base>/models`.
//!
//! The preconditions are still checked first, because a request that cannot
//! possibly work is not worth opening a socket for, but they are no longer
//! the answer.
//!
//! Nothing in this module runs on a timer, on start, or on a cache expiry.
//! Every function here makes exactly one HTTP request and only because a
//! keystroke asked for it — see [`mod@crate::provider::cache`].
//! Every function in this module blocks its own thread, and every one of them
//! is bounded — see [`ProbeTimeouts`]. The caller is responsible for running
//! them somewhere other than the thread drawing the terminal;
//! `shell::spawn_provider_probe` is the one place that does it.
//! A [`ProbeRequest`] may carry a resolved [`Secret`]. It has a hand-written
//! [`Debug`](std::fmt::Debug) that prints [`crate::secret::REDACTED`] in its
//! place, it is never written to the cache, and no failure message in this
//! module is built from text that passed anywhere near it — see
//! the private `unreachable_reason`.
// History: design-decisions.md, "Trims: provider module docs", discovery/mod.rs module doc.

use std::fmt;
use std::time::{Duration, Instant};

use ureq::Agent;
use ureq::config::AutoHeaderValue;

use crate::provider::cache::ModelEntry;
use crate::routing::wire::WireProtocol;
use crate::secret::{REDACTED, Secret};

/// How long a probe waits for the TCP connection and TLS handshake.
///
/// A handshake to a live host completes in well under a second on any
/// ordinary connection; five seconds is enough for a slow resolver and a
/// congested link and is still short enough that a host which is simply not
/// there is reported rather than waited on.
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

/// How long a probe waits for the response head once the request is sent.
///
/// This is the timeout that matters, and the one whose absence would be a
/// hang: a server that accepts a connection and then says nothing is
/// indistinguishable from a healthy one until this expires. A model
/// catalogue is a read, not a generation — every one of the five catalogues
/// probed on 2026-08-26 answered in well under a second — so ten seconds is
/// already generous by a factor of ten.
pub const RESPONSE_TIMEOUT: Duration = Duration::from_secs(10);

/// A ceiling on the whole call, body included.
///
/// The other two bound the phases a stall is *likely* in. This bounds the
/// one nobody thinks of: a server that answers its head promptly and then
/// dribbles the body one byte at a time forever would satisfy both of the
/// others indefinitely. With this set, no probe can outlive it whatever the
/// peer does.
pub const TOTAL_TIMEOUT: Duration = Duration::from_secs(20);

/// The largest response body a probe will read.
///
/// OpenRouter's 417-model catalogue was 687,721 bytes when read on
/// 2026-08-26 and Nous's 372-model catalogue was 660,362; eight mebibytes is
/// an order of magnitude of headroom over the largest real catalogue anyone
/// has measured here and is still a bound. `ureq`'s own default is ten
/// mebibytes, so this is not relying on that default staying what it is.
const MAX_BODY_BYTES: u64 = 8 * 1024 * 1024;

/// Every timeout a probe is bounded by.
///
/// A type rather than three constants read directly, for one reason: the
/// tests that prove a stalled endpoint is bounded have to be able to pick
/// short values, and a test that had to wait out [`RESPONSE_TIMEOUT`] would
/// be a ten-second test that people would eventually mark `#[ignore]`. The
/// production path uses [`ProbeTimeouts::default`] and nothing else, and a
/// test asserts that default is exactly the three constants above — because
/// "no timeout" is a hang, and a default that quietly lost one would be the
/// regression this whole batch exists to prevent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProbeTimeouts {
    pub connect: Duration,
    pub response: Duration,
    pub total: Duration,
}

impl Default for ProbeTimeouts {
    fn default() -> Self {
        Self {
            connect: CONNECT_TIMEOUT,
            response: RESPONSE_TIMEOUT,
            total: TOTAL_TIMEOUT,
        }
    }
}

/// Which URL a probe actually requests.
///
/// Named rather than inferred, because the difference is visible to the user
/// in the result line and a probe that quietly chose a different URL than the
/// one it reported would be worse than no probe at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProbeTarget {
    /// `<base>/models` — the provider's own model list.
    ///
    /// Only for a provider whose [`crate::provider::Provider::model_list_endpoint`]
    /// is verified. It is the better probe when it is available: one request
    /// exercises the base URL, TLS, the credential and a real route, instead
    /// of only proving something answered.
    ModelList,
    /// The base URL itself, with no path appended.
    ///
    /// For a provider whose model-list endpoint nobody has established. A
    /// probe that appended `/models` anyway would be guessing at a path, and
    /// this module refuses that for exactly the reason
    /// [`mod@crate::provider`] refuses to guess at a base URL.
    BaseUrl,
}

/// One provider probe: where to go, what to send, and the credential to send
/// with it.
///
/// The credential is private and there is no accessor. It reaches exactly one
/// place — the header this module builds — and its [`Debug`] shows
/// [`REDACTED`].
pub struct ProbeRequest {
    provider: String,
    protocol: WireProtocol,
    base_url: String,
    target: ProbeTarget,
    headers: Vec<(String, String)>,
    credential: Option<Secret>,
}

impl ProbeRequest {
    pub fn new(
        provider: impl Into<String>,
        protocol: WireProtocol,
        base_url: impl Into<String>,
        target: ProbeTarget,
        headers: Vec<(String, String)>,
        credential: Option<Secret>,
    ) -> Self {
        Self {
            provider: provider.into(),
            protocol,
            base_url: base_url.into(),
            target,
            headers,
            credential,
        }
    }

    pub fn provider(&self) -> &str {
        &self.provider
    }

    pub fn protocol(&self) -> WireProtocol {
        self.protocol
    }

    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    pub fn target(&self) -> ProbeTarget {
        self.target
    }

    /// The exact URL this probe requests.
    ///
    /// Public because the result line names it: "reached X" is a claim a user
    /// can only check if they are told what was actually asked for.
    pub fn url(&self) -> String {
        match self.target {
            ProbeTarget::BaseUrl => self.base_url.clone(),
            ProbeTarget::ModelList => {
                format!("{}/models", self.base_url.trim_end_matches('/'))
            }
        }
    }
}

/// Prints [`REDACTED`] where the credential is, and whether there is one at
/// all — which is a fact a diagnostic legitimately needs and which reveals
/// nothing about the value.
impl fmt::Debug for ProbeRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ProbeRequest")
            .field("provider", &self.provider)
            .field("protocol", &self.protocol)
            .field("base_url", &self.base_url)
            .field("target", &self.target)
            .field("headers", &self.headers)
            .field(
                "credential",
                &match self.credential {
                    Some(_) => REDACTED,
                    None => "(none)",
                },
            )
            .finish()
    }
}

/// What one probe found.
///
/// The distinction that earns this type its variants is
/// [`ProbeOutcome::Rejected`] against [`ProbeOutcome::Unreachable`]: a
/// provider that answered `401` and a provider that answered nothing are
/// different problems with different fixes — one is a credential, one is a
/// URL or a network — and a user told only "the test failed" has to guess
/// which they have.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProbeOutcome {
    /// Answered `2xx`. The endpoint is there and took the credential.
    Reached { status: u16 },
    /// Answered `401` or `403`. The endpoint is there and refused the
    /// credential — reachable, rejected.
    Rejected { status: u16 },
    /// Answered something else. Reported verbatim rather than translated: a
    /// `404` on a model list and a `503` from a provider having a bad day
    /// are both "it answered", and inventing a friendlier word for either
    /// would throw away the only number the user can act on.
    Unexpected { status: u16 },
    /// Bounded by a timeout instead of answering.
    TimedOut { waited_ms: u64 },
    /// Never answered at all — no route, no host, a refused connection.
    Unreachable { reason: String },
}

impl ProbeOutcome {
    /// Whether the endpoint produced an HTTP response of any kind.
    ///
    /// True for a `401` and a `503` alike. Deliberately not called
    /// "succeeded": reaching a provider is not the same as being able to
    /// route to it, and nothing in this module decides the second question.
    pub fn answered(&self) -> bool {
        matches!(
            self,
            Self::Reached { .. } | Self::Rejected { .. } | Self::Unexpected { .. }
        )
    }
}

/// What a model-catalogue fetch produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModelFetch {
    /// A catalogue was read, in the order the provider listed it.
    Catalogue(Vec<ModelEntry>),
    /// The endpoint answered `2xx` with something that is not a catalogue.
    ///
    /// Separate from [`ProbeOutcome::Unexpected`] because the failure is in
    /// the body rather than the status, and the two have different fixes:
    /// one is "that URL is wrong", the other is "that URL is right and this
    /// parser does not understand what it said".
    NotACatalogue { status: u16, reason: String },
    /// Everything a connectivity probe can report, reported the same way.
    Probe(ProbeOutcome),
}

/// What one probe found, **with the rate-limit headers it came back with** —
/// capability map line 1229.
///
/// A [`ProbeOutcome`] answers "did this endpoint answer, and how". This type
/// wraps that answer instead of adding a header list to its variants, so
/// [`connectivity`] keeps its exact signature and nothing that only wants
/// the outcome changes at all.
///
/// `headers` is what [`crate::provider::telemetry::retain_rate_limit_headers`]
/// kept, which is an allowlist and not a filter — see
/// [`crate::provider::telemetry::RATE_LIMIT_HEADERS`] for why that matters.
/// A probe result is a diagnostic a user may be invited to share, and a
/// diagnostic that captured "the response headers" would carry a session
/// cookie into it.
// History: design-decisions.md, "Trims: provider module docs", discovery/mod.rs `ProbeResponse` doc.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProbeResponse {
    outcome: ProbeOutcome,
    headers: Vec<(String, String)>,
}

impl ProbeResponse {
    pub fn new(outcome: ProbeOutcome, headers: Vec<(String, String)>) -> Self {
        Self { outcome, headers }
    }

    pub fn outcome(&self) -> &ProbeOutcome {
        &self.outcome
    }

    /// The rate-limit headers this response carried, lowercased, in the
    /// order [`crate::provider::telemetry::RATE_LIMIT_HEADERS`] lists them.
    pub fn headers(&self) -> &[(String, String)] {
        &self.headers
    }

    /// What those headers said — capability map line 1229.
    ///
    /// Empty when the response carried none, which was the answer for seven
    /// of the eight hosts Glasshouse ships templates for when this was
    /// measured; see [`mod@crate::provider::telemetry`].
    pub fn rate_limits(&self) -> crate::provider::telemetry::RateLimitHeaders {
        crate::provider::telemetry::RateLimitHeaders::read(
            self.headers
                .iter()
                .map(|(name, value)| (name.as_str(), value.as_str())),
        )
    }
}

/// Make one request and report what happened, without reading a body.
///
/// This is Phase 9D line 1. It reports; it decides nothing. A failure here
/// must never disable a provider and a success must never enable one — see
/// the caller in `shell::state`, where that separation is the
/// thing under test.
pub fn connectivity(request: &ProbeRequest, timeouts: ProbeTimeouts) -> ProbeOutcome {
    connectivity_with_headers(request, timeouts).outcome
}

/// [`connectivity`], keeping the rate-limit headers the response carried —
/// capability map line 1229's API half, and the first of two places in
/// Glasshouse that read a quota number off a response.
///
/// Reading them costs no extra request, which is capability map
/// line 1230's "without excessive request cost" applied to the whole
/// telemetry story: a probe that already ran is free.
///
/// Reading a response *header* is not reading the *payload*: the
/// gateway already parses the status line and header block to forward them,
/// and only the body is what it is forbidden to look inside. So
/// `crate::gateway::ingress` now reads this same allowlist, headers only,
/// from every response it forwards — see that module and
/// [`mod@crate::provider::telemetry`]'s "a second seam".
// History: design-decisions.md, "Trims: provider module docs", discovery/mod.rs `connectivity_with_headers` doc.
pub fn connectivity_with_headers(request: &ProbeRequest, timeouts: ProbeTimeouts) -> ProbeResponse {
    let started = Instant::now();
    match send(request, timeouts) {
        Ok(response) => {
            let outcome = classify(response.status().as_u16());
            ProbeResponse::new(outcome, rate_limit_headers_of(&response))
        }
        Err(err) => ProbeResponse::new(transport_outcome(&err, started), Vec::new()),
    }
}

/// Pull the allowlisted rate-limit headers off a response.
///
/// A header whose value is not valid UTF-8 is skipped rather than escaped:
/// every field this module reads is an integer or a short parameter list, and
/// a non-UTF-8 value in one of them is not a number this parser could have
/// understood anyway.
fn rate_limit_headers_of(response: &ureq::http::Response<ureq::Body>) -> Vec<(String, String)> {
    let named: Vec<(&str, &str)> = response
        .headers()
        .iter()
        .filter_map(|(name, value)| Some((name.as_str(), value.to_str().ok()?)))
        .collect();
    crate::provider::telemetry::retain_rate_limit_headers(named)
}

/// What a request made for its response **body** produced — capability map
/// line 1230.
///
/// Distinct from [`ModelFetch`] only in what it carries on success: the raw
/// body text rather than a parsed catalogue, because a usage endpoint's
/// schema is provider-specific and this module does not decide what a
/// provider's response means, only fetches what it said.
/// [`crate::provider::telemetry::ProviderUsage::read`] is the one parser.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BodyFetch {
    /// A `2xx` whose body was read whole.
    Answered { status: u16, body: String },
    /// A `2xx` whose body could not be read — a stalled connection mid-body,
    /// most likely. Distinct from a non-2xx status: the route answered, the
    /// *read* is what failed.
    NotRead { status: u16, reason: String },
    /// Everything a connectivity probe can report, reported the same way —
    /// including a non-2xx status, which is not a failure of this function
    /// but a fact about the account (see design decision D3: an endpoint
    /// that refuses is not the same finding as one that answers `null`).
    Probe(ProbeOutcome),
}

/// Make one request to `request`'s URL and read its body whole, without
/// deciding what the body means.
///
/// Bounded by the same body-size limit and [`ProbeTimeouts`] every other
/// probe in this module is. Built for capability map line 1230's usage
/// endpoint, and generic enough that any future provider-specific body read
/// can reuse it rather than growing a second copy of [`model_catalogue`]'s
/// own request/timeout/body-limit plumbing.
pub fn read_response_body(request: &ProbeRequest, timeouts: ProbeTimeouts) -> BodyFetch {
    let started = Instant::now();
    let mut response = match send(request, timeouts) {
        Ok(response) => response,
        Err(err) => return BodyFetch::Probe(transport_outcome(&err, started)),
    };

    let status = response.status().as_u16();
    let outcome = classify(status);
    if !matches!(outcome, ProbeOutcome::Reached { .. }) {
        return BodyFetch::Probe(outcome);
    }

    match response
        .body_mut()
        .with_config()
        .limit(MAX_BODY_BYTES)
        .read_to_string()
    {
        Ok(body) => BodyFetch::Answered { status, body },
        Err(ureq::Error::Timeout(_)) => BodyFetch::Probe(ProbeOutcome::TimedOut {
            waited_ms: elapsed_ms(started),
        }),
        Err(_) => BodyFetch::NotRead {
            status,
            reason: "the response body could not be read".to_owned(),
        },
    }
}

/// Make one request to the provider's model list and read it.
///
/// This is Phase 9D line 2's network half. It is only ever called for a
/// provider whose model-list endpoint is established — the caller decides
/// that, and says so plainly when it is not, because "this provider does not
/// offer model discovery" is an answer and not an error.
pub fn model_catalogue(request: &ProbeRequest, timeouts: ProbeTimeouts) -> ModelFetch {
    match read_response_body(request, timeouts) {
        BodyFetch::Answered { status, body } => match parse_catalogue(&body) {
            Ok(models) => ModelFetch::Catalogue(models),
            Err(reason) => ModelFetch::NotACatalogue { status, reason },
        },
        BodyFetch::NotRead { status, reason } => ModelFetch::NotACatalogue { status, reason },
        BodyFetch::Probe(outcome) => ModelFetch::Probe(outcome),
    }
}

/// Read a model catalogue out of a `GET /models` response body.
///
/// # The shape, as actually read
///
/// Five live catalogues were read unauthenticated on 2026-08-26 —
/// OpenRouter (417 entries), Nous (372), UnoRouter (374), Kilo (367) and
/// AnyRouter (102). All five put their entries under a top-level `data`
/// array and **every entry in all five carried a string `id`**. Nothing else
/// was universal: UnoRouter wraps the array in `{"success":…,"message":…}`,
/// AnyRouter adds `"object":"list"`, and UnoRouter's entries have no `name`
/// field at all where the other four do. So `id` is the one field this
/// parser reads and the one field [`ModelEntry`] stores; anything else would
/// be a field that is absent for at least one provider already shipping a
/// template here.
///
/// A bare top-level array is accepted too. That shape was not observed in
/// any of the five, but it costs one match arm and it is the obvious way a
/// small self-hosted proxy — the `llama-cpp` and `ollama` templates point at
/// two — answers this route.
fn parse_catalogue(body: &str) -> Result<Vec<ModelEntry>, String> {
    let value: serde_json::Value =
        serde_json::from_str(body).map_err(|_| "the response was not JSON".to_owned())?;

    let entries = match &value {
        serde_json::Value::Array(entries) => entries,
        serde_json::Value::Object(object) => match object.get("data") {
            Some(serde_json::Value::Array(entries)) => entries,
            _ => return Err("the response has no `data` array of models".to_owned()),
        },
        _ => return Err("the response was not a model list".to_owned()),
    };

    let models: Vec<ModelEntry> = entries
        .iter()
        .filter_map(|entry| match entry {
            serde_json::Value::String(id) => Some(ModelEntry::new(id)),
            serde_json::Value::Object(object) => object
                .get("id")
                .and_then(serde_json::Value::as_str)
                .map(ModelEntry::new),
            _ => None,
        })
        .collect();

    if models.is_empty() && !entries.is_empty() {
        return Err("no entry in the model list had an `id`".to_owned());
    }
    Ok(models)
}

/// Send the request, with every timeout set and every one of `ureq`'s
/// helpful behaviours turned off.
///
/// `http_status_as_error(false)` is the load-bearing one: with the default, a
/// `401` arrives as an `Err` indistinguishable in shape from a refused
/// connection, and telling those two apart is the whole point of
/// [`ProbeOutcome::Rejected`]. `max_redirects(0)` is the second: following a
/// redirect would mean re-attaching the credential to a host the provider
/// named at runtime, which is a decision this module has no business making
/// silently — and it is not hypothetical, because `kilocode.ai` answers
/// `308` to `kilo.ai` today.
fn send(
    request: &ProbeRequest,
    timeouts: ProbeTimeouts,
) -> Result<ureq::http::Response<ureq::Body>, ureq::Error> {
    let agent = Agent::new_with_config(
        Agent::config_builder()
            .http_status_as_error(false)
            .max_redirects(0)
            .accept_encoding(AutoHeaderValue::None)
            .timeout_connect(Some(timeouts.connect))
            .timeout_recv_response(Some(timeouts.response))
            .timeout_global(Some(timeouts.total))
            .build(),
    );

    let mut builder = agent.get(request.url());
    for (name, value) in &request.headers {
        builder = builder.header(name, value);
    }
    if let Some(credential) = &request.credential {
        // The one place a resolved credential is read in this module. It goes
        // into a header value and nowhere else — not into the URL, not into
        // a log line, not into the error path below.
        builder = match request.protocol {
            WireProtocol::AnthropicMessages => builder.header("x-api-key", credential.expose()),
            WireProtocol::OpenAiChat | WireProtocol::OpenAiResponses => {
                builder.header("authorization", format!("Bearer {}", credential.expose()))
            }
            // Google's Generative Language API takes its key here and reads
            // `authorization` as an OAuth bearer token, so a probe that sent
            // the key there would be rejected for the wrong reason and the
            // provider would read as unreachable rather than as reachable
            // and un-probed.
            WireProtocol::GeminiGenerateContent => {
                builder.header("x-goog-api-key", credential.expose())
            }
            // No model-list endpoint is declared for System One
            // (`archive/glasshouse:docs/product/evidence/phase-66.md`, *Provider facts*), so this
            // arm is never reached by a real probe; it exists only to keep
            // the match exhaustive. Its wire uses a bearer token, so it
            // matches the OpenAI arm's shape if it is ever reached.
            WireProtocol::TypesafeSystemOne => {
                builder.header("authorization", format!("Bearer {}", credential.expose()))
            }
        };
    }
    builder.call()
}

/// An HTTP status, as an outcome.
fn classify(status: u16) -> ProbeOutcome {
    match status {
        200..=299 => ProbeOutcome::Reached { status },
        401 | 403 => ProbeOutcome::Rejected { status },
        _ => ProbeOutcome::Unexpected { status },
    }
}

/// A transport failure, as an outcome.
fn transport_outcome(err: &ureq::Error, started: Instant) -> ProbeOutcome {
    if is_timeout(err) {
        ProbeOutcome::TimedOut {
            waited_ms: elapsed_ms(started),
        }
    } else {
        ProbeOutcome::Unreachable {
            reason: unreachable_reason(err),
        }
    }
}

fn elapsed_ms(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX)
}

/// Whether `err` is a timeout, however it was raised.
///
/// Two arms rather than one because `ureq` raises its own configured
/// timeouts as [`ureq::Error::Timeout`] but a socket-level deadline can
/// still surface as an `io::Error` — and a timeout reported as "unreachable"
/// would tell the user their URL is wrong when their network is slow.
fn is_timeout(err: &ureq::Error) -> bool {
    match err {
        ureq::Error::Timeout(_) => true,
        ureq::Error::Io(io) => matches!(
            io.kind(),
            std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock
        ),
        _ => false,
    }
}

/// Why a request never got an answer, in words chosen here.
///
/// **Built from a fixed set of phrases on purpose.** Every one of these is a
/// literal in this function, so no text from a peer, a header, a URL or an
/// error's own `Display` can reach a diagnostic through this path. That is a
/// structural guarantee rather than a careful habit, which is the same trade
/// [`crate::secret::Secret`]'s hand-written `Debug` makes: it costs a little
/// detail and it cannot be undone by someone later adding a field.
fn unreachable_reason(err: &ureq::Error) -> String {
    let phrase = match err {
        ureq::Error::HostNotFound => "the host name did not resolve",
        ureq::Error::ConnectionFailed => "the connection failed",
        ureq::Error::BadUri(_) => "the base URL is not a usable URL",
        ureq::Error::RedirectFailed | ureq::Error::TooManyRedirects => {
            "the endpoint redirected, and a probe will not follow one"
        }
        ureq::Error::Protocol(_) => "the reply was not valid HTTP",
        ureq::Error::Io(io) => match io.kind() {
            std::io::ErrorKind::ConnectionRefused => "the connection was refused",
            std::io::ErrorKind::ConnectionReset => "the connection was reset",
            std::io::ErrorKind::ConnectionAborted => "the connection was aborted",
            std::io::ErrorKind::UnexpectedEof => "the connection closed before answering",
            std::io::ErrorKind::NotConnected | std::io::ErrorKind::BrokenPipe => {
                "the connection dropped"
            }
            std::io::ErrorKind::PermissionDenied => "the connection was not permitted",
            _ => "the connection failed",
        },
        _ => "the request could not be made",
    };
    phrase.to_owned()
}

#[cfg(test)]
mod tests;
