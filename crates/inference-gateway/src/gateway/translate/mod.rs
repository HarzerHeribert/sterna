//! Wire-protocol translation at the gateway — Phase 56, lines 1948–1950 and
//! 1956, under the ruling recorded in `archive/glasshouse:docs/product/design-decisions.md` as
//! *"the user's answer on pairs: all of them"*.
//!
//! History: design-decisions.md, "Trims: gateway module docs", translate/mod.rs module doc.
//!
//! # Structurally not a harness
//!
//! This directory keeps `gateway/`'s rule: no file here names
//! `crate::harness`, so the table is keyed by protocol **slug** — the same
//! spelling `WireProtocol::slug` produces and [`super::upstream::Route`]
//! already carries — and `crate::provider` is the caller that turns a
//! `WireProtocol` into one.

pub mod canonical;
pub mod stream;

mod anthropic;
mod gemini;
mod openai_chat;
mod openai_responses;

use std::io::{BufReader, Read, Write};
use std::net::{Shutdown, TcpStream};
use std::time::Instant;

use ureq::http::{HeaderValue, Request as HttpRequest, StatusCode, header};
use ureq::{Agent, SendBody};

use crate::provider::telemetry::RateLimitHeaders;

use super::http::{self, RequestHead};
use super::ingress::{
    Exchange, Framing, Outcome, StreamEnd, Tokens, millis_since, transport_detail,
};
use super::upstream::{Route, ServedBy, Upstream, UpstreamBackend, VERSION_SEGMENT, path_of};
use canonical::{BlockStart, Delta, Request, Response, StreamEvent, Unsupported};
use stream::{SseEvent, SseReader};

pub use openai_chat::TOOL_ERROR_MARKER;

/// The largest request or response document the translator will hold whole.
///
/// The relay beside this module holds nothing and so bounds nothing; a codec
/// has to hold one document to translate it, and 32 MiB is Anthropic's own
/// request limit.
pub const MAX_BODY_BYTES: u64 = 32 * 1024 * 1024;

/// The five wire protocols, by slug, in the gateway's own order.
pub const PROTOCOLS: [&str; 5] = [
    "anthropic-messages",
    "openai-responses",
    "openai-chat",
    "gemini-generate-content",
    "typesafe-systemone",
];

/// Whether an ordered pair is offered, and if not, why not.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PairStatus {
    /// Both codecs exist and the pair's end-to-end test is green — the only
    /// way a row becomes supported (capability map line 1956).
    Supported,
    Refused(&'static str),
}

/// One ordered pair of wire protocols: the harness's protocol, the
/// provider's, and whether the gateway translates between them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Pair {
    /// The protocol the harness speaks — the request target's.
    pub from: &'static str,
    /// The protocol the provider serves.
    pub to: &'static str,
    pub status: PairStatus,
}

impl Pair {
    pub fn is_supported(&self) -> bool {
        matches!(self.status, PairStatus::Supported)
    }

    /// `from->to`, the spelling a diagnostic and the evidence ledger's
    /// `route` column carry.
    pub fn slug(&self) -> String {
        format!("{}->{}", self.from, self.to)
    }

    /// The reason a refused pair is refused, or `None` when it is supported.
    pub fn refusal(&self) -> Option<&'static str> {
        match self.status {
            PairStatus::Supported => None,
            PairStatus::Refused(reason) => Some(reason),
        }
    }
}

const NOT_YET_REVERSE: &str = "not yet: both codecs exist, but the pair has no end-to-end test through the shipped \
     binary against a fixture upstream, and no pair is offered before its test (1956)";
const SAME_PROTOCOL: &str =
    "same protocol: the relay carries it byte for byte and no codec is entered";
/// Why every row **out of** Gemini is refused, and it is not "untested".
///
/// The gateway translates a harness's protocol into the provider's. Nothing
/// installed speaks Gemini at the ingress — the Gemini CLI adapter is T3b,
/// a separate package — so a `gemini-generate-content -> …` row would
/// describe a request no harness here can make. It is refused for the thing
/// that is actually missing rather than parked behind an end-to-end test
/// nobody could write yet.
const NO_GEMINI_HARNESS: &str = "not offered: no supported client speaks gemini-generate-content at the ingress, so no \
     request of this shape can arrive; the Gemini CLI adapter is a separate package (T3b) and \
     these rows are decided when it lands";

/// Why every row touching `typesafe-systemone` is refused, and it is not
/// "untested" or "not yet". A decision request has no messages, tools or
/// stream to translate, and a chat request has no questions — there is no
/// shared shape for a codec to bridge, so no pair with this protocol is ever
/// offered, regardless of which side it is on.
const NOT_A_CHAT_PROTOCOL: &str = "not offered: typesafe-systemone is a decision protocol with no messages, tools or \
     stream, and a chat protocol has no questions — there is no shared shape for a codec to \
     translate, so this pair is never offered";

/// The API version header `api.anthropic.com` requires on every request —
/// the same value real clients send and the relay path already forwards
/// verbatim (`gateway/mod.rs`'s fixture tests pin it). A translated request
/// has no client header to relay this from, so [`serve`] states it itself,
/// and only toward an Anthropic-serving outbound protocol (T2 finding 2).
const ANTHROPIC_VERSION: &str = "2023-06-01";

/// The header Google's Generative Language API takes its API key in.
const GOOGLE_API_KEY_HEADER: &str = "x-goog-api-key";

/// The provider credential as the bare key `x-goog-api-key` carries, taken
/// from the one door `super::upstream` opens onto it.
///
/// [`UpstreamBackend`] deliberately exposes **no** getter for its credential
/// — only [`UpstreamBackend::authorization`], the header the gateway
/// attaches — so this un-prefixes that header rather than asking for a
/// second door. `bearer` builds the value as `Bearer {key}` and the backend
/// refuses at construction any credential that is not header-safe, so the
/// strip is total; a value that somehow is not gets attached unchanged,
/// which fails at the provider rather than silently sending a wrong key.
///
/// The right long-term home is an `UpstreamBackend::api_key` beside
/// `authorization`, in `upstream.rs`. That file was outside this package's
/// expected files, and one accessor's worth of tidiness is not worth an
/// unannounced edit to a module every other worker also builds on.
fn api_key(serving: &UpstreamBackend) -> HeaderValue {
    let attached = serving.authorization();
    let mut value = attached
        .to_str()
        .ok()
        .and_then(|text| text.strip_prefix("Bearer "))
        .and_then(|key| HeaderValue::from_str(key).ok())
        .unwrap_or(attached);
    value.set_sensitive(true);
    value
}

/// The pair table. Every ordered pair of [`PROTOCOLS`], including each
/// protocol with itself, exactly once — `every_ordered_pair_appears_exactly_once`
/// holds it to that, and `crate::provider`'s own test holds it against
/// `WireProtocol`, which this file may not name.
const TABLE: [Pair; 25] = [
    Pair {
        from: "anthropic-messages",
        to: "anthropic-messages",
        status: PairStatus::Refused(SAME_PROTOCOL),
    },
    // T1: Claude Code served by an OpenAI-Chat entitlement — OpenRouter and
    // every OpenAI-compatible key. The first pair, and the end-to-end test
    // in `tests/gateway_translate.rs` is what lets this row say Supported.
    Pair {
        from: "anthropic-messages",
        to: "openai-chat",
        status: PairStatus::Supported,
    },
    // T2: Claude Code served by an OpenAI-Responses entitlement — a
    // ChatGPT/Codex-plan-shaped upstream. Supported only because its own
    // end-to-end test exists: `tests/gateway_translate_responses.rs`,
    // `a_claude_code_request_is_translated_to_openai_responses_and_back_with_ids_preserved`.
    Pair {
        from: "anthropic-messages",
        to: "openai-responses",
        status: PairStatus::Supported,
    },
    Pair {
        from: "openai-chat",
        to: "anthropic-messages",
        status: PairStatus::Refused(NOT_YET_REVERSE),
    },
    Pair {
        from: "openai-chat",
        to: "openai-chat",
        status: PairStatus::Refused(SAME_PROTOCOL),
    },
    // T2b: an OpenCode-shaped (openai-chat) client served by an
    // OpenAI-Responses entitlement — a ChatGPT/Codex-plan-shaped upstream.
    // Supported only because its own end-to-end test exists:
    // `tests/gateway_translate_t2b.rs`,
    // `an_opencode_request_is_translated_to_openai_responses_and_back_with_tool_call_ids_preserved`.
    Pair {
        from: "openai-chat",
        to: "openai-responses",
        status: PairStatus::Supported,
    },
    // T2's mirror: a Codex-shaped client served by an Anthropic Messages
    // entitlement. Supported only because its own end-to-end test exists:
    // `tests/gateway_translate_responses.rs`,
    // `a_codex_request_is_translated_to_anthropic_messages_and_back_with_ids_preserved`.
    Pair {
        from: "openai-responses",
        to: "anthropic-messages",
        status: PairStatus::Supported,
    },
    // T2b's mirror: a Codex-shaped (openai-responses) client served by an
    // OpenAI-Chat entitlement. Supported only because its own end-to-end
    // test exists: `tests/gateway_translate_t2b.rs`,
    // `a_codex_shaped_request_is_translated_to_openai_chat_and_back_with_tool_call_ids_preserved`.
    Pair {
        from: "openai-responses",
        to: "openai-chat",
        status: PairStatus::Supported,
    },
    Pair {
        from: "openai-responses",
        to: "openai-responses",
        status: PairStatus::Refused(SAME_PROTOCOL),
    },
    // T3: Claude Code served by a Google AI Studio entitlement. Supported
    // only because its own end-to-end test exists:
    // `tests/gateway_translate_gemini.rs`,
    // `a_claude_code_request_is_translated_to_generate_content_and_the_answer_back_with_tool_calls_matched_by_name`.
    Pair {
        from: "anthropic-messages",
        to: "gemini-generate-content",
        status: PairStatus::Supported,
    },
    // T3: a Codex-shaped client served by the same. Supported only because
    // its own end-to-end test exists: `tests/gateway_translate_gemini.rs`,
    // `a_codex_shaped_request_is_translated_to_generate_content_and_back`.
    Pair {
        from: "openai-responses",
        to: "gemini-generate-content",
        status: PairStatus::Supported,
    },
    // T3: an OpenCode-shaped client served by the same. Supported only
    // because its own end-to-end test exists:
    // `tests/gateway_translate_gemini.rs`,
    // `an_opencode_request_is_translated_to_generate_content_and_streamed_back_in_chats_order`.
    Pair {
        from: "openai-chat",
        to: "gemini-generate-content",
        status: PairStatus::Supported,
    },
    // Every row OUT of Gemini: refused for the reason that is true, which is
    // not "no test yet" — see `NO_GEMINI_HARNESS`.
    Pair {
        from: "gemini-generate-content",
        to: "anthropic-messages",
        status: PairStatus::Refused(NO_GEMINI_HARNESS),
    },
    Pair {
        from: "gemini-generate-content",
        to: "openai-responses",
        status: PairStatus::Refused(NO_GEMINI_HARNESS),
    },
    Pair {
        from: "gemini-generate-content",
        to: "openai-chat",
        status: PairStatus::Refused(NO_GEMINI_HARNESS),
    },
    Pair {
        from: "gemini-generate-content",
        to: "gemini-generate-content",
        status: PairStatus::Refused(SAME_PROTOCOL),
    },
    // Every row touching typesafe-systemone, either side: refused for the
    // reason that is true — see NOT_A_CHAT_PROTOCOL — never "not yet".
    Pair {
        from: "anthropic-messages",
        to: "typesafe-systemone",
        status: PairStatus::Refused(NOT_A_CHAT_PROTOCOL),
    },
    Pair {
        from: "openai-responses",
        to: "typesafe-systemone",
        status: PairStatus::Refused(NOT_A_CHAT_PROTOCOL),
    },
    Pair {
        from: "openai-chat",
        to: "typesafe-systemone",
        status: PairStatus::Refused(NOT_A_CHAT_PROTOCOL),
    },
    Pair {
        from: "gemini-generate-content",
        to: "typesafe-systemone",
        status: PairStatus::Refused(NOT_A_CHAT_PROTOCOL),
    },
    Pair {
        from: "typesafe-systemone",
        to: "anthropic-messages",
        status: PairStatus::Refused(NOT_A_CHAT_PROTOCOL),
    },
    Pair {
        from: "typesafe-systemone",
        to: "openai-responses",
        status: PairStatus::Refused(NOT_A_CHAT_PROTOCOL),
    },
    Pair {
        from: "typesafe-systemone",
        to: "openai-chat",
        status: PairStatus::Refused(NOT_A_CHAT_PROTOCOL),
    },
    Pair {
        from: "typesafe-systemone",
        to: "gemini-generate-content",
        status: PairStatus::Refused(NOT_A_CHAT_PROTOCOL),
    },
    Pair {
        from: "typesafe-systemone",
        to: "typesafe-systemone",
        status: PairStatus::Refused(SAME_PROTOCOL),
    },
];

/// Every ordered pair, for a later CLI view. **No production caller reads
/// this enumeration yet**; the two production consumers of the table go
/// through [`lookup`].
pub fn pairs() -> &'static [Pair] {
    &TABLE
}

/// The row for `from -> to`, or `None` when either slug is not a wire
/// protocol this gateway knows.
pub fn lookup(from: &str, to: &str) -> Option<&'static Pair> {
    TABLE.iter().find(|pair| pair.from == from && pair.to == to)
}

/// Whether `from -> to` is a supported pair. The one function
/// `crate::provider::translation_available` calls.
pub fn is_supported(from: &str, to: &str) -> bool {
    lookup(from, to).is_some_and(Pair::is_supported)
}

/// What a codec's own wire does with a prompt-cache marker decoded off
/// another protocol's request (capability map line 2014) — the pair
/// table's per-target answer, read by [`field_rows`] rather than carried on
/// [`canonical::Request`] itself, because it is a property of the encoding
/// codec, not of any one request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CacheDisposition {
    /// Carried under this wire's own hint field, named, with one sentence on
    /// how its value is derived.
    Carried {
        field: &'static str,
        note: &'static str,
    },
    /// This wire has no equivalent; the marker is never encoded, for the
    /// stated reason.
    Stripped(&'static str),
}

/// What a codec's own wire does with a harness's carried thinking/reasoning
/// request (capability map line 2039's prerequisite,
/// `archive/glasshouse:docs/product/design-decisions.md`'s *"Carrying effort across a
/// translated pairing"*) — [`CacheDisposition`]'s shape, one field later:
/// a `Carried` wire names its own field and how the level is derived; a
/// `Stripped` one has no such field at all and never encodes one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EffortDisposition {
    /// Carried under this wire's own hint field, named, with one sentence on
    /// how its value is derived.
    Carried {
        field: &'static str,
        note: &'static str,
    },
    /// This wire has no equivalent; effort is never encoded, for the stated
    /// reason.
    Stripped(&'static str),
}

/// The per-field rows of one codec — what it refuses, with reasons; what it
/// ignores by name in a response; and, where the concept applies, what its
/// encoder does with a prompt-cache marker (`None` for a protocol never
/// asked to carry one, such as Anthropic — see `Codec::cache_disposition`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FieldRows {
    pub refused: &'static [(&'static str, &'static str)],
    pub ignored: &'static [&'static str],
    pub cache: Option<CacheDisposition>,
    pub effort: Option<EffortDisposition>,
}

/// The per-field rows for `protocol`'s codec, or `None` for a protocol with
/// no codec.
pub fn field_rows(protocol: &str) -> Option<FieldRows> {
    codec_for(protocol).map(|codec| FieldRows {
        refused: codec.refused_fields(),
        ignored: codec.ignored_fields(),
        cache: codec.cache_disposition(),
        effort: codec.effort_disposition(),
    })
}

/// A request this pair cannot carry, named: the pair, the field in the
/// wire's own spelling, and one sentence a user can act on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TranslationRefusal {
    pub pair: String,
    pub field: String,
    pub reason: String,
}

impl TranslationRefusal {
    fn new(pair: &Pair, unsupported: Unsupported) -> Self {
        Self {
            pair: pair.slug(),
            field: unsupported.field,
            reason: unsupported.reason,
        }
    }
}

impl std::fmt::Display for TranslationRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "the gateway cannot translate this request for the pair {}: `{}` — {}",
            self.pair, self.field, self.reason
        )
    }
}

// --- codecs ---------------------------------------------------------------------

/// One wire protocol's codec.
pub(super) trait Codec: Sync {
    fn protocol(&self) -> &'static str;
    /// The one request target this codec translates, version segment
    /// stripped — the path a client of this protocol posts an inference
    /// request to, and the path the gateway posts to a provider of it.
    ///
    /// For three of the four wires this is a literal path. Gemini addresses
    /// its model in the path, so its answer here is a **shape** a refusal
    /// can name, and its own [`Codec::claim`] and
    /// [`Codec::outbound_endpoint`] are what actually decide and build.
    fn endpoint(&self) -> &'static str;
    /// Whether `path` — a request target's path, query already removed —
    /// belongs to this codec's protocol, and whether it is the endpoint the
    /// codec translates.
    ///
    /// The default is the fixed-path rule the Anthropic and OpenAI wires
    /// share: an optional `/v1`, then exactly [`Codec::endpoint`], with
    /// anything below it a sub-target of the same protocol. A wire whose
    /// path is not fixed overrides this.
    fn claim(&self, path: &str) -> Claim {
        let path = match path.strip_prefix(VERSION_SEGMENT) {
            Some(rest) if rest.is_empty() || rest.starts_with('/') => rest,
            _ => path,
        };
        let endpoint = self.endpoint();
        if path == endpoint {
            Claim::Endpoint
        } else if path.starts_with(endpoint) && path[endpoint.len()..].starts_with('/') {
            Claim::Other
        } else {
            Claim::None
        }
    }
    /// The path a translated request is posted to at a provider of this
    /// protocol, before [`outbound_target`] adds any version segment.
    ///
    /// The request is an argument for the one wire that needs it: Gemini's
    /// path carries the model and says whether the answer is streamed.
    fn outbound_endpoint(&self, _request: &Request) -> String {
        self.endpoint().to_owned()
    }
    /// What this codec cannot encode out of the canonical form, refused by
    /// name before anything is opened upstream.
    ///
    /// The canonical form was built from fields both T1 wires carry, so the
    /// default refuses nothing — but [`Codec::encode_request`] is
    /// infallible, and a codec whose wire has no home for a canonical field
    /// must refuse it here rather than drop it there. OpenAI Responses is
    /// the first such wire: it has no stop-sequence parameter.
    fn refuse_unencodable(&self, _request: &Request) -> Result<(), Unsupported> {
        Ok(())
    }
    fn decode_request(&self, body: &[u8]) -> Result<Request, Unsupported>;
    fn encode_request(&self, request: &Request) -> Vec<u8>;
    fn decode_response(&self, body: &[u8]) -> Result<Response, Unsupported>;
    fn encode_response(&self, response: &Response) -> Vec<u8>;
    fn stream_decoder(&self) -> Box<dyn StreamDecoder + Send>;
    fn stream_encoder(&self) -> Box<dyn StreamEncoder + Send>;
    /// This protocol's error `type` for an HTTP status.
    fn error_kind(&self, status: u16) -> &'static str;
    /// An error document in this protocol's shape.
    fn encode_error(&self, kind: &str, message: &str) -> Vec<u8>;
    /// An error event in this protocol's stream shape.
    fn encode_stream_error(&self, kind: &str, message: &str) -> Vec<u8>;
    /// The message out of an error document in this protocol's shape.
    fn decode_error(&self, body: &[u8]) -> Option<String>;
    fn refused_fields(&self) -> &'static [(&'static str, &'static str)];
    fn ignored_fields(&self) -> &'static [&'static str];
    /// What this codec's [`Codec::encode_request`] does with a prompt-cache
    /// marker carried on [`Request::cache_requested`] (2014). `None` is the
    /// default for a wire never asked to answer the question — Anthropic's
    /// own, since no pair supported today decodes a cache marker from a
    /// protocol other than Anthropic Messages and then encodes back onto it.
    fn cache_disposition(&self) -> Option<CacheDisposition> {
        None
    }
    /// What this codec's [`Codec::encode_request`] does with a carried
    /// thinking/reasoning request ([`Request::effort`]) — the pair table's
    /// per-target answer, mirroring [`Codec::cache_disposition`] exactly.
    /// `None` is the default for a wire never asked to answer the question —
    /// Anthropic's own, since no pair supported today decodes an effort
    /// marker from a protocol other than Anthropic Messages and then encodes
    /// it back onto Anthropic Messages.
    fn effort_disposition(&self) -> Option<EffortDisposition> {
        None
    }
}

/// Turns one wire's stream events into canonical events, as they arrive.
pub(super) trait StreamDecoder {
    fn feed(&mut self, event: &SseEvent) -> Result<Vec<StreamEvent>, Unsupported>;
    /// The stream ended cleanly; whatever closes the message.
    fn finish(&mut self) -> Result<Vec<StreamEvent>, Unsupported>;
    fn is_done(&self) -> bool;
}

/// Turns canonical events into one wire's stream bytes.
pub(super) trait StreamEncoder {
    fn encode(&mut self, event: &StreamEvent) -> Vec<u8>;
}

/// How a codec claims a request path — the answer [`place`] turns into a
/// [`Placement`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Claim {
    /// The one endpoint this codec translates.
    Endpoint,
    /// This codec's protocol, but not the endpoint it translates.
    Other,
    /// Not this codec's path at all.
    None,
}

const CODECS: [&dyn Codec; 4] = [
    &anthropic::Anthropic,
    &openai_chat::OpenAiChat,
    &openai_responses::OpenAiResponses,
    &gemini::Gemini,
];

fn codec_for(protocol: &str) -> Option<&'static dyn Codec> {
    CODECS
        .iter()
        .copied()
        .find(|codec| codec.protocol() == protocol)
}

/// The target a translated request is posted to at a provider of `codec`'s
/// protocol: the exact path that protocol's own native client sends, because
/// every provider's declared base URL is composed for that client.
///
/// Claude Code sends `POST /v1/messages` and the Anthropic-serving base URLs
/// carry no `/v1`; Codex sends `POST /responses` and OpenAI-Chat clients
/// `POST /chat/completions` against base URLs that already carry it (see the
/// provider templates and `profile::ingress_targets`, each read off real
/// request lines). Composing `base + endpoint()` alone would mis-address an
/// Anthropic-serving provider — `…/api/messages` instead of
/// `…/api/v1/messages` — which the T2 mirror pair was the first to reach.
///
/// Gemini's answer comes whole from [`Codec::outbound_endpoint`], version
/// segment included, and nothing is prefixed here: its provider template's
/// base URL is the bare host precisely so that a **relayed** Gemini target —
/// which carries `/v1beta` itself — is not composed into `/v1beta/v1beta/…`.
fn outbound_target(codec: &dyn Codec, request: &Request) -> String {
    if codec.protocol() == anthropic::PROTOCOL {
        format!("{VERSION_SEGMENT}{}", codec.outbound_endpoint(request))
    } else {
        codec.outbound_endpoint(request)
    }
}

// --- placement ------------------------------------------------------------------

/// What the ingress does with a target the provider does not serve.
#[derive(Debug)]
pub(super) enum Placement {
    /// Translate it through this supported pair.
    Translate(&'static Pair),
    /// The target belongs to a codec's protocol, but every pair to a served
    /// protocol is refused — answered with the pairs and their reasons.
    PairRefused {
        from: &'static str,
        refused: Vec<&'static Pair>,
    },
    /// The target lies under a codec's protocol but is not the one endpoint
    /// that codec translates.
    TargetRefused { from: &'static str },
    /// Not a target any codec claims: the plain `404` stays.
    Unplaceable,
}

/// Decide, from the target alone, whether an unserved request is translated.
///
/// `served` is the serving backend's protocol slugs. A target under a served
/// protocol is [`Placement::Unplaceable`] here even if a codec claims it —
/// the caller's route lookup owns served targets, and this is the second
/// lock on the byte-for-byte rule.
pub(super) fn place(target: &str, served: &[&str]) -> Placement {
    let path = path_of(target);
    let Some((codec, claim)) = CODECS
        .iter()
        .copied()
        .find_map(|codec| match codec.claim(path) {
            Claim::None => None,
            claim => Some((codec, claim)),
        })
    else {
        return Placement::Unplaceable;
    };
    let from = codec.protocol();
    if served.contains(&from) {
        return Placement::Unplaceable;
    }
    if claim == Claim::Other {
        return Placement::TargetRefused { from };
    }
    let mut refused = Vec::new();
    for to in served {
        match lookup(from, to) {
            Some(pair) if pair.is_supported() => return Placement::Translate(pair),
            Some(pair) => refused.push(pair),
            None => {}
        }
    }
    Placement::PairRefused { from, refused }
}

/// The `404` body for a target whose every pair is refused: the pairs by
/// name and the table's reason for each. Built from the table's own text and
/// the protocol slugs; nothing from the request.
pub(super) fn pair_refusal_message(from: &str, refused: &[&Pair]) -> String {
    if refused.is_empty() {
        return format!(
            "this request speaks {from}, which the configured provider does not serve, and no \
             protocol it does serve is one the gateway translates {from} to"
        );
    }
    let pairs = refused
        .iter()
        .map(|pair| format!("{} ({})", pair.slug(), pair.refusal().unwrap_or("refused")))
        .collect::<Vec<_>>()
        .join("; ");
    format!(
        "this request speaks {from}, which the configured provider does not serve, and the \
         gateway refuses the translation by name: {pairs}"
    )
}

/// The `404` body for a target under a translated protocol that is not the
/// one endpoint translated.
pub(super) fn target_refusal_message(from: &str) -> String {
    let endpoint = codec_for(from).map(Codec::endpoint).unwrap_or("");
    format!(
        "this request speaks {from}, which the configured provider does not serve, and only its \
         `{endpoint}` endpoint is translated; the requested endpoint has no equivalent on the \
         provider's protocol"
    )
}

// --- the pipeline -----------------------------------------------------------------

/// Serve one request the ingress placed for translation.
///
/// Everything the relay does not do happens here and only here: the body is
/// read whole (bounded), decoded by the harness's codec, encoded by the
/// provider's, sent with the provider's credential exactly as the relay
/// attaches it, and the answer is decoded by the provider's codec and
/// encoded by the harness's — as a document, or event by event as a stream.
/// A refusal at any point before the upstream request is built answers the
/// harness with **nothing opened upstream**.
#[allow(clippy::too_many_arguments)]
pub(super) fn serve(
    head: RequestHead,
    mut reader: BufReader<TcpStream>,
    out: &mut TcpStream,
    upstream: &Upstream,
    serving: &UpstreamBackend,
    agent: &Agent,
    pair: &'static Pair,
    purpose: Option<String>,
) -> (Exchange, RateLimitHeaders) {
    let from = codec_for(pair.from).expect("a supported pair has a codec on its harness side");
    let to = codec_for(pair.to).expect("a supported pair has a codec on its provider side");
    let route = serving
        .route_named(pair.to)
        .expect("a pair is placed only against a protocol the serving backend routes");
    // Capability map line 2451, for every writer below that reaches the
    // provider: never used by `refuse`, since a refusal here is written
    // before any upstream request exists and nothing served it.
    let served_by = ServedBy::of(serving);

    // Written with the socket left open: every caller below either drains
    // the rest of the client's body through `settle` (which closes once it
    // has drained) or has already consumed the whole body itself, so a
    // shutdown here would only race the drain — killing the read half the
    // client is still writing into and turning the refusal `settle` exists
    // to deliver into the network error it exists to prevent.
    let refuse = |out: &mut TcpStream, status: StatusCode, message: &str| {
        let body = from.encode_error(from.error_kind(status.as_u16()), message);
        let _ = write_document_open(out, status, &body, None);
    };

    if head.method != ureq::http::Method::POST {
        refuse(
            out,
            StatusCode::METHOD_NOT_ALLOWED,
            "only a POST is translated; the provider's protocol has no equivalent for this method",
        );
        settle(&mut reader, out, head.content_length);
        return (
            exchange(Outcome::Declined, 405, upstream, pair, route),
            RateLimitHeaders::default(),
        );
    }
    let Some(length) = head.content_length else {
        refuse(
            out,
            StatusCode::BAD_REQUEST,
            "a request to translate must carry a body framed with content-length",
        );
        settle(&mut reader, out, None);
        return (
            exchange(Outcome::Declined, 400, upstream, pair, route),
            RateLimitHeaders::default(),
        );
    };
    if length > MAX_BODY_BYTES {
        refuse(
            out,
            StatusCode::PAYLOAD_TOO_LARGE,
            "the request body exceeds the size the gateway will translate",
        );
        settle(&mut reader, out, Some(length));
        return (
            exchange(Outcome::Declined, 413, upstream, pair, route),
            RateLimitHeaders::default(),
        );
    }
    // Reserved for what a real request looks like, not for what this one
    // declared: `take` below bounds the result either way, and a
    // declaration is not a delivery. 64 KiB covers a Claude Code request
    // head-on and the vector grows for anything larger.
    let mut body = Vec::with_capacity(length.min(64 * 1024) as usize);
    if (&mut reader).take(length).read_to_end(&mut body).is_err() || body.len() as u64 != length {
        return (
            exchange(Outcome::ClientGone, 0, upstream, pair, route),
            RateLimitHeaders::default(),
        );
    }

    // Decode on the harness's codec, normalize (2016: stable tool order),
    // then let the provider's codec refuse, by name, any canonical field
    // its wire has no home for — all three before anything is opened
    // upstream.
    let request = match from
        .decode_request(&body)
        .map(Request::normalized)
        .and_then(|request| {
            to.refuse_unencodable(&request)?;
            Ok(request)
        }) {
        Ok(request) => request,
        Err(unsupported) => {
            let refusal = TranslationRefusal::new(pair, unsupported);
            refuse(out, StatusCode::BAD_REQUEST, &refusal.to_string());
            let _ = out.shutdown(Shutdown::Both);
            return (
                exchange(Outcome::Declined, 400, upstream, pair, route),
                RateLimitHeaders::default(),
            );
        }
    };
    // Migration 24's two request-derived columns, taken here and nowhere
    // else: this is the only point in the gateway that holds a decoded
    // request, and both are pure functions of it. `effort` is `None` when
    // the harness asked for no thinking; `turn_shape` is always known once
    // a request has decoded, because every request has a last user message
    // or does not, and both answers are words.
    let effort = request
        .effort
        .map(|effort| crate::routing::evidence::EffortLevel::from(effort.level()));
    let turn_shape = Some(request.turn_shape());
    // Line 1334's `repairs`, the other quantity a decoded request answers
    // outright: how many tool-result blocks the harness marked as errors,
    // regardless of where in the conversation they sit. `Some` the moment
    // the request has decoded, same as `effort` and `turn_shape` above.
    let repairs = Some(request.error_tool_results());
    let requested_model = super::request_model::bounded(request.model.clone());
    // Every `Exchange` this function returns from here on carries them —
    // the refusals below included, because a request that decoded and was
    // then refused downstream is still a request whose effort and shape were
    // read. `exchange` above (and the relay's own helper) leaves both `None`,
    // which is what the paths *before* the decode must record.
    let decoded = |outcome: Outcome, status: u16| Exchange {
        effort,
        turn_shape,
        repairs,
        purpose: purpose.clone(),
        requested_model: requested_model.clone(),
        ..exchange(outcome, status, upstream, pair, route)
    };

    // Observability for 2014's strip case: the harness asked for prompt
    // caching and this pairing's target has no equivalent to carry it to.
    // Never a refusal — the request is still served — just a per-exchange
    // record of why the marker did not reach the provider.
    if request.cache_requested
        && let Some(CacheDisposition::Stripped(reason)) = to.cache_disposition()
    {
        tracing::debug!(
            pair = %pair.slug(),
            reason,
            "the harness asked for prompt caching; this pairing has no equivalent and strips the marker"
        );
    }
    let translated = to.encode_request(&request);

    let Some(uri) = route.uri_for(&outbound_target(to, &request)) else {
        refuse(
            out,
            StatusCode::BAD_REQUEST,
            "the translated request could not be addressed to the configured provider",
        );
        return (decoded(Outcome::Declined, 400), RateLimitHeaders::default());
    };
    let mut outbound = HttpRequest::builder()
        .method(ureq::http::Method::POST)
        .uri(uri)
        .header(header::CONTENT_TYPE, "application/json")
        .header(
            header::ACCEPT,
            if request.stream {
                "text/event-stream"
            } else {
                "application/json"
            },
        )
        .header(header::CONTENT_LENGTH, HeaderValue::from(translated.len()));
    if let Some(agent_name) = head.headers.get(header::USER_AGENT) {
        outbound = outbound.header(header::USER_AGENT, agent_name.clone());
    }
    // The one credential, in the one place the outbound protocol reads it.
    //
    // Google's Generative Language API takes its key in `x-goog-api-key` and
    // reads `authorization` as an OAuth bearer token — sending the API key
    // there too would present it as a token it is not, and the request would
    // be rejected for the wrong reason. So this is exclusive, not additive:
    // one credential, one header, chosen by the outbound protocol. Every
    // other protocol keeps the `authorization` the relay attaches, byte for
    // byte.
    if to.protocol() == gemini::PROTOCOL {
        outbound = outbound.header(GOOGLE_API_KEY_HEADER, api_key(serving));
    } else {
        outbound = outbound.header(header::AUTHORIZATION, serving.authorization());
    }
    // api.anthropic.com requires this header; no client header exists to
    // relay it from on a translated request, so it is stated here, and only
    // toward an Anthropic-serving outbound protocol (T2 finding 2).
    if to.protocol() == anthropic::PROTOCOL {
        outbound = outbound.header("anthropic-version", ANTHROPIC_VERSION);
    }
    let Ok(outbound) = outbound.body(SendBody::from_owned_reader(std::io::Cursor::new(
        translated,
    ))) else {
        refuse(
            out,
            StatusCode::BAD_REQUEST,
            "the translated request could not be built for the configured provider",
        );
        return (decoded(Outcome::Declined, 400), RateLimitHeaders::default());
    };

    // Migration 25's zero, taken immediately before the translated request
    // leaves — `ingress::forward`'s own comment applies here word for word:
    // the seconds `dispatched_at` names the hand-off to the gateway, this
    // names the send, and a monotonic `Instant` is the only clock that can
    // answer at this resolution without a step making the answer negative.
    let dispatch = Instant::now();
    let response = match agent.run(outbound) {
        Ok(response) => response,
        Err(err) => {
            let detail = transport_detail(&err);
            refuse(
                out,
                StatusCode::BAD_GATEWAY,
                "the gateway could not reach the configured provider",
            );
            return (
                decoded(Outcome::Unreachable { detail }, 502),
                RateLimitHeaders::default(),
            );
        }
    };
    let first_byte_at = Some(crate::provider::cache::now_unix_seconds());
    // The same instant as an offset from the send — migration 25.
    let first_byte_ms = Some(millis_since(dispatch));
    let (parts, mut body) = response.into_parts();
    let status = parts.status;
    let quota = RateLimitHeaders::read(
        parts
            .headers
            .iter()
            .filter_map(|(name, value)| Some((name.as_str(), value.to_str().ok()?))),
    );
    let is_event_stream = parts
        .headers
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.trim_start().starts_with("text/event-stream"));
    let upstream_status = status.as_u16();

    let finish = |outcome: Outcome,
                  status: u16,
                  framing: Framing,
                  tokens: Option<Tokens>,
                  first: FirstEvents| {
        (
            Exchange {
                first_byte_at,
                first_byte_ms,
                // Migration 25's `completed_ms`. `finish` is the one place
                // every path that reached a response ends, so `elapsed()`
                // here is the end of the exchange on all of them — the same
                // reading `ingress::forward` takes at each of its own three
                // post-response returns.
                completed_ms: Some(millis_since(dispatch)),
                framing: Some(framing),
                tokens,
                first_token_at: first.first_token_at,
                first_tool_call_at: first.first_tool_call_at,
                first_token_ms: first.first_token_ms,
                first_tool_call_ms: first.first_tool_call_ms,
                // Line 1334's `tool_rounds`: `Some` the moment a response
                // arrived — `finish` is only ever reached after one did —
                // `first.tool_uses` honestly `0` for a response the seam
                // looked at and found no tool-use block in, same rule as
                // `first_tool_call_at`'s own `None`-vs-`Some(0)` split one
                // level up.
                tool_rounds: Some(first.tool_uses),
                ..decoded(outcome, status)
            },
            quota.clone(),
        )
    };

    // A provider error, in the harness's own error shape and with the
    // provider's own status: the status is what routing reads, and the
    // message is what the harness needed to show.
    if !status.is_success() {
        let raw = body
            .with_config()
            .limit(MAX_BODY_BYTES)
            .read_to_vec()
            .unwrap_or_default();
        let message = to
            .decode_error(&raw)
            .unwrap_or_else(|| String::from_utf8_lossy(&raw).into_owned());
        let document = from.encode_error(from.error_kind(upstream_status), &message);
        let written = document.len() as u64;
        let ended = match write_document(out, status, &document, &served_by) {
            Ok(()) => StreamEnd::Complete,
            Err(_) => StreamEnd::ClientClosed,
        };
        let outcome = if ended == StreamEnd::ClientClosed {
            Outcome::ClientGone
        } else {
            Outcome::Forwarded {
                upstream_status,
                bytes: written,
            }
        };
        return finish(
            outcome,
            upstream_status,
            Framing {
                declared: None,
                relayed: Some(written),
                ended,
            },
            None,
            FirstEvents::default(),
        );
    }

    if is_event_stream {
        let mut events = SseReader::new(BufReader::new(body.as_reader()));
        let mut decoder = to.stream_decoder();
        if request.stream {
            return stream_events(
                out,
                &mut events,
                decoder.as_mut(),
                from,
                &finish,
                upstream_status,
                dispatch,
                &served_by,
            );
        }
        // The harness asked for a document and the provider streamed anyway:
        // gather the stream into the document it delivered.
        let mut gathered = Vec::new();
        // One event is bounded by `stream::MAX_EVENT_BYTES`; the number of
        // events accumulated here is not, and this is the one branch that
        // holds them all rather than writing each one out as it arrives.
        let mut held = 0u64;
        loop {
            match events.next_event() {
                Ok(Some(event)) => {
                    held += event.data.len() as u64;
                    if held > MAX_BODY_BYTES {
                        return untranslatable(
                            out,
                            from,
                            pair,
                            Unsupported::new(
                                "body",
                                "the provider's stream exceeded the size the \
                                 gateway will hold to answer a request that did not ask for \
                                 a stream",
                            ),
                            &finish,
                            &served_by,
                        );
                    }
                    match decoder.feed(&event) {
                        Ok(more) => gathered.extend(more),
                        Err(unsupported) => {
                            return untranslatable(
                                out,
                                from,
                                pair,
                                unsupported,
                                &finish,
                                &served_by,
                            );
                        }
                    }
                }
                Ok(None) => match decoder.finish() {
                    Ok(more) => {
                        gathered.extend(more);
                        break;
                    }
                    Err(unsupported) => {
                        return untranslatable(out, from, pair, unsupported, &finish, &served_by);
                    }
                },
                Err(_) => {
                    return untranslatable(
                        out,
                        from,
                        pair,
                        Unsupported::new("stream", "the provider's stream failed"),
                        &finish,
                        &served_by,
                    );
                }
            }
            if decoder.is_done() {
                break;
            }
        }
        return match canonical::accumulate(&gathered) {
            Ok(response) => deliver_document(
                out,
                from,
                &response,
                first_byte_at,
                first_byte_ms,
                &finish,
                upstream_status,
                &served_by,
            ),
            Err(unsupported) => untranslatable(out, from, pair, unsupported, &finish, &served_by),
        };
    }

    let raw = match body.with_config().limit(MAX_BODY_BYTES).read_to_vec() {
        Ok(raw) => raw,
        Err(_) => {
            return untranslatable(
                out,
                from,
                pair,
                Unsupported::new("body", "the provider's response could not be read whole"),
                &finish,
                &served_by,
            );
        }
    };
    let response = match to.decode_response(&raw) {
        Ok(response) => response,
        Err(unsupported) => {
            return untranslatable(out, from, pair, unsupported, &finish, &served_by);
        }
    };
    if request.stream {
        // The harness asked for a stream and the provider answered with a
        // document: deliver it as the event sequence it would have streamed.
        let events = response.as_events();
        let mut encoder = from.stream_encoder();
        let mut written = 0u64;
        if write_stream_head(out, &served_by).is_err() {
            return finish(
                Outcome::ClientGone,
                upstream_status,
                Framing {
                    declared: None,
                    relayed: Some(0),
                    ended: StreamEnd::ClientClosed,
                },
                None,
                FirstEvents::default(),
            );
        }
        for event in &events {
            let bytes = encoder.encode(event);
            if write_chunk(out, &bytes).is_err() {
                return finish(
                    Outcome::ClientGone,
                    upstream_status,
                    Framing {
                        declared: None,
                        relayed: Some(written),
                        ended: StreamEnd::ClientClosed,
                    },
                    None,
                    FirstEvents::default(),
                );
            }
            written += bytes.len() as u64;
        }
        let _ = out.write_all(b"0\r\n\r\n");
        let _ = out.flush();
        let _ = out.shutdown(Shutdown::Both);
        // The provider never streamed this response at all — `events` is
        // `Response::as_events`'s own reconstruction — so this is a document
        // delivery exactly like `deliver_document`'s, just written as an
        // event sequence instead of one JSON body: the same
        // `FirstEvents::of_document` derivation applies.
        return finish(
            Outcome::Forwarded {
                upstream_status,
                bytes: written,
            },
            upstream_status,
            Framing {
                declared: None,
                relayed: Some(written),
                ended: StreamEnd::Complete,
            },
            Some(tokens_of(&response)),
            FirstEvents::of_document(&events, first_byte_at, first_byte_ms),
        );
    }
    deliver_document(
        out,
        from,
        &response,
        first_byte_at,
        first_byte_ms,
        &finish,
        upstream_status,
        &served_by,
    )
}

type Finish<'a> =
    &'a dyn Fn(Outcome, u16, Framing, Option<Tokens>, FirstEvents) -> (Exchange, RateLimitHeaders);

/// The 1331/1332 ruling's two clock readings, plus 1334/1350's tool-round
/// count, noted as canonical [`StreamEvent`]s pass through the seam that
/// already decoded them.
///
/// Kept as one name rather than split or renamed to something like
/// `ResponseFacts`: every field here is still a fact [`Self::note`] reads off
/// a canonical event as it passes, the same shape the original two clock
/// readings were, and `tool_uses` is a count of the very block start that
/// already stamps `first_tool_call_at` — a sibling, not a new concern.
///
/// The two clock-reading fields are stamped once and never overwritten;
/// `tool_uses` counts every qualifying event rather than stopping at the
/// first, which is why it cannot share their `is_none()` guard. [`Self::note`]
/// retains no response text — only whatever instant its `now` closure
/// returns at the moment a qualifying event is seen, and an integer count. A
/// document delivery (the provider never streamed, or a stream was gathered
/// into one before delivery) has no finer boundary to observe than its own
/// `first_byte_at` — see [`Self::of_document`] — so the streamed path is the
/// only caller that passes a real wall clock; `tool_uses` counts the same
/// either way, because it never reads the clock at all.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct FirstEvents {
    first_token_at: Option<i64>,
    first_tool_call_at: Option<i64>,
    /// Migration 25's `first_token_ms`: the same event as
    /// [`Self::first_token_at`], measured as milliseconds since the upstream
    /// request was sent rather than named on the wall clock. Stamped by the
    /// same `is_none()` guard and from the same [`Self::note`] call, so the
    /// two can never disagree about *which* event they describe.
    first_token_ms: Option<i64>,
    /// [`Self::first_token_ms`]'s sibling for the first tool-use block start.
    first_tool_call_ms: Option<i64>,
    /// Line 1334's `tool_rounds`: how many [`BlockStart::ToolUse`] events
    /// this response's canonical events carried — the rounds this exchange
    /// *began*. Never stamped, always incremented; `0` is `serve`'s own
    /// honest reading of a response with no tool-use block at all.
    tool_uses: u32,
}

impl FirstEvents {
    /// The rule, stated once: the first real token is the first
    /// [`Delta::Text`] carrying a non-whitespace character, and the first
    /// tool call is the first [`BlockStart::ToolUse`] — which also counts
    /// toward `tool_uses` every time, not only the first. Everything else —
    /// `Delta::InputJson`, `BlockStart::Text`, a whitespace-only text delta —
    /// leaves all three untouched, and `first_token_at`/`first_tool_call_at`,
    /// once stamped, are never restamped.
    ///
    /// `now` answers **both** readings for the one instant a qualifying
    /// event passes: the unix second the row's `*_at` columns hold, and
    /// migration 25's milliseconds since the upstream request was sent. One
    /// closure and not two, because the whole point of the pair is that they
    /// describe the same moment — asking twice would let them drift by
    /// whatever ran in between.
    fn note(&mut self, event: &StreamEvent, now: &dyn Fn() -> (i64, Option<i64>)) {
        match event {
            StreamEvent::BlockDelta {
                delta: Delta::Text(text),
                ..
            } if self.first_token_at.is_none() && text.chars().any(|c| !c.is_whitespace()) => {
                let (at, ms) = now();
                self.first_token_at = Some(at);
                self.first_token_ms = ms;
            }
            StreamEvent::BlockStart {
                block: BlockStart::ToolUse { .. },
                ..
            } => {
                self.tool_uses += 1;
                if self.first_tool_call_at.is_none() {
                    let (at, ms) = now();
                    self.first_tool_call_at = Some(at);
                    self.first_tool_call_ms = ms;
                }
            }
            _ => {}
        }
    }

    /// What a document delivery records: both instants equal to
    /// `first_byte_at` when `events` (a real streamed sequence gathered into
    /// one response, or [`Response::as_events`]'s reconstruction of one that
    /// never streamed at all) contains a qualifying event, `None` otherwise
    /// — the protocol exposed no finer boundary than the document's own
    /// arrival, so there is one rule ([`Self::note`]) and not two, run here
    /// with a clock that always answers the same instant.
    ///
    /// Migration 25's two offsets follow the same rule and for the same
    /// reason: a document exposed no finer boundary than its own arrival, so
    /// `first_token_ms` and `first_tool_call_ms` are the document's own
    /// `first_byte_ms` — equality, exactly as the seconds are, rather than
    /// an offset invented to look more precise than the protocol was.
    fn of_document(
        events: &[StreamEvent],
        first_byte_at: Option<i64>,
        first_byte_ms: Option<i64>,
    ) -> Self {
        let mut first = Self::default();
        let Some(at) = first_byte_at else {
            return first;
        };
        for event in events {
            first.note(event, &|| (at, first_byte_ms));
        }
        first
    }
}

fn tokens_of(response: &Response) -> Tokens {
    Tokens {
        input: response.usage.input,
        output: response.usage.output,
        cached: response.usage.cached,
    }
}

/// A translated document, written whole.
///
/// `first_byte_at` and migration 25's `first_byte_ms` are threaded in rather
/// than read off `finish` — the caller's own captures — because
/// [`FirstEvents::of_document`] needs both to derive the 1331/1332 pair and
/// its millisecond siblings from `response.as_events()` before `finish`
/// attaches them to the [`Exchange`].
#[allow(clippy::too_many_arguments)]
fn deliver_document(
    out: &mut TcpStream,
    from: &dyn Codec,
    response: &Response,
    first_byte_at: Option<i64>,
    first_byte_ms: Option<i64>,
    finish: Finish<'_>,
    upstream_status: u16,
    served_by: &ServedBy,
) -> (Exchange, RateLimitHeaders) {
    let document = from.encode_response(response);
    let written = document.len() as u64;
    match write_document(out, StatusCode::OK, &document, served_by) {
        Ok(()) => finish(
            Outcome::Forwarded {
                upstream_status,
                bytes: written,
            },
            200,
            Framing {
                declared: None,
                relayed: Some(written),
                ended: StreamEnd::Complete,
            },
            Some(tokens_of(response)),
            FirstEvents::of_document(&response.as_events(), first_byte_at, first_byte_ms),
        ),
        Err(_) => finish(
            Outcome::ClientGone,
            200,
            Framing {
                declared: None,
                relayed: Some(0),
                ended: StreamEnd::ClientClosed,
            },
            None,
            FirstEvents::default(),
        ),
    }
}

/// The provider answered, and its answer is not one the pair can carry.
///
/// A `502` rather than a `4xx`: the harness's request was fine, and what
/// cannot be translated is the provider's side. Nothing of the provider's
/// body reaches the harness — the refusal names the field.
fn untranslatable(
    out: &mut TcpStream,
    from: &dyn Codec,
    pair: &Pair,
    unsupported: Unsupported,
    finish: Finish<'_>,
    served_by: &ServedBy,
) -> (Exchange, RateLimitHeaders) {
    let refusal = TranslationRefusal::new(pair, unsupported);
    let message = format!("the provider's answer could not be translated — {refusal}");
    let document = from.encode_error(from.error_kind(502), &message);
    let written = document.len() as u64;
    let ended = match write_document(out, StatusCode::BAD_GATEWAY, &document, served_by) {
        Ok(()) => StreamEnd::Complete,
        Err(_) => StreamEnd::ClientClosed,
    };
    finish(
        Outcome::Declined,
        502,
        Framing {
            declared: None,
            relayed: Some(written),
            ended,
        },
        None,
        FirstEvents::default(),
    )
}

/// Translate a provider's stream to the harness, one event at a time.
#[allow(clippy::too_many_arguments)]
fn stream_events<R: Read>(
    out: &mut TcpStream,
    events: &mut SseReader<BufReader<R>>,
    decoder: &mut dyn StreamDecoder,
    from: &dyn Codec,
    finish: Finish<'_>,
    upstream_status: u16,
    dispatch: Instant,
    served_by: &ServedBy,
) -> (Exchange, RateLimitHeaders) {
    let mut encoder = from.stream_encoder();
    let mut written = 0u64;
    let mut usage = None;
    let mut order = canonical::Order::default();
    // Line 1331/1332's pair and migration 25's two offsets, noted in real
    // time as each canonical event passes — the one case where
    // `FirstEvents::note` gets a real clock rather than the constant
    // `of_document` feeds it, and therefore the only path on which the two
    // token offsets can differ from `first_byte_ms`.
    let mut first_events = FirstEvents::default();
    let client_gone = |written: u64| {
        finish(
            Outcome::ClientGone,
            upstream_status,
            Framing {
                declared: None,
                relayed: Some(written),
                ended: StreamEnd::ClientClosed,
            },
            None,
            FirstEvents::default(),
        )
    };
    if write_stream_head(out, served_by).is_err() {
        return client_gone(0);
    }
    let mut ended = StreamEnd::Complete;
    loop {
        let translated = match events.next_event() {
            Ok(Some(event)) => decoder.feed(&event),
            Ok(None) => decoder.finish(),
            Err(_) => Err(Unsupported::new(
                "stream",
                "the provider's stream failed before it finished",
            )),
        };
        let at_end = decoder.is_done();
        // The encoders write bytes and cannot refuse, so the one place an
        // out-of-order provider stream can still be refused by name is here,
        // before a delta is handed to an encoder that would attach it to
        // whichever block is open — see `canonical::Order`.
        let translated = translated.and_then(|events| {
            for event in &events {
                order.check(event)?;
            }
            Ok(events)
        });
        match translated {
            Ok(canonical_events) => {
                for event in &canonical_events {
                    if let StreamEvent::MessageDelta {
                        usage: final_usage, ..
                    } = event
                    {
                        usage = Some(Tokens {
                            input: final_usage.input,
                            output: final_usage.output,
                            cached: final_usage.cached,
                        });
                    }
                    first_events.note(event, &|| {
                        (
                            crate::provider::cache::now_unix_seconds(),
                            Some(millis_since(dispatch)),
                        )
                    });
                    let bytes = encoder.encode(event);
                    if bytes.is_empty() {
                        continue;
                    }
                    if write_chunk(out, &bytes).is_err() {
                        return client_gone(written);
                    }
                    written += bytes.len() as u64;
                }
            }
            Err(unsupported) => {
                // The head has been sent: the only channel left is the
                // stream itself, so the refusal goes down it, by name, and
                // the stream ends.
                let message =
                    format!("the provider's stream could not be translated: {unsupported}");
                let bytes = from.encode_stream_error(from.error_kind(502), &message);
                if write_chunk(out, &bytes).is_err() {
                    return client_gone(written);
                }
                written += bytes.len() as u64;
                ended = StreamEnd::Aborted;
                break;
            }
        }
        if at_end {
            break;
        }
    }
    let _ = out.write_all(b"0\r\n\r\n");
    let _ = out.flush();
    let _ = out.shutdown(Shutdown::Both);
    finish(
        Outcome::Forwarded {
            upstream_status,
            bytes: written,
        },
        upstream_status,
        Framing {
            declared: None,
            relayed: Some(written),
            ended,
        },
        if ended == StreamEnd::Complete {
            usage
        } else {
            None
        },
        first_events,
    )
}

/// One document, written with the connection left open: for a refusal that
/// still owes the client a drain of whatever it is still sending — `out` is
/// a `try_clone` of the same socket as the reader doing that draining, so a
/// shutdown here would close the read half out from under it too.
///
/// `served_by` is `None` for exactly the refusals written before any
/// upstream request exists — capability map line 2451 — and `Some` on every
/// other path, which all go through [`write_document`] instead.
fn write_document_open(
    out: &mut TcpStream,
    status: StatusCode,
    body: &[u8],
    served_by: Option<&ServedBy>,
) -> std::io::Result<()> {
    let mut headers = vec![
        ("content-type".to_owned(), b"application/json".to_vec()),
        (
            "content-length".to_owned(),
            body.len().to_string().into_bytes(),
        ),
        ("connection".to_owned(), b"close".to_vec()),
    ];
    if let Some(served_by) = served_by {
        served_by.push_onto(&mut headers);
    }
    http::write_head(out, status, &headers)?;
    out.write_all(body)?;
    out.flush()
}

/// One document, and the connection closed after it: for every answer that
/// is not followed by a drain of the client's own socket — always a served
/// exchange, so `served_by` is required rather than optional.
fn write_document(
    out: &mut TcpStream,
    status: StatusCode,
    body: &[u8],
    served_by: &ServedBy,
) -> std::io::Result<()> {
    write_document_open(out, status, body, Some(served_by))?;
    let _ = out.shutdown(Shutdown::Both);
    Ok(())
}

fn write_stream_head(out: &mut TcpStream, served_by: &ServedBy) -> std::io::Result<()> {
    let mut headers = vec![
        (
            "content-type".to_owned(),
            b"text/event-stream; charset=utf-8".to_vec(),
        ),
        ("cache-control".to_owned(), b"no-cache".to_vec()),
        ("transfer-encoding".to_owned(), b"chunked".to_vec()),
        ("connection".to_owned(), b"close".to_vec()),
    ];
    served_by.push_onto(&mut headers);
    http::write_head(out, StatusCode::OK, &headers)
}

/// One HTTP chunk, written and flushed at once — the same one-write rule
/// `super::http::pump` keeps, for the same reason.
fn write_chunk(out: &mut TcpStream, bytes: &[u8]) -> std::io::Result<()> {
    let mut framed = format!("{:x}\r\n", bytes.len()).into_bytes();
    framed.extend_from_slice(bytes);
    framed.extend_from_slice(b"\r\n");
    out.write_all(&framed)?;
    out.flush()
}

/// Drain what the client is still sending, then close — `ingress::settle`,
/// for the refusals written here.
fn settle(reader: &mut BufReader<TcpStream>, out: &mut TcpStream, content_length: Option<u64>) {
    super::ingress::settle(reader, out, content_length);
}

fn exchange(
    outcome: Outcome,
    status: u16,
    upstream: &Upstream,
    pair: &Pair,
    route: &Route,
) -> Exchange {
    Exchange {
        outcome,
        status,
        provider: upstream.provider().to_owned(),
        protocol: Some(pair.slug()),
        // The translated path never reads `ingress::PURPOSE_HEADER` — only
        // `ingress::forward`'s relay loop does. Decoded requests override
        // this with the already allowlisted client purpose.
        purpose: None,
        requested_model: None,
        host: route.host(),
        first_byte_at: None,
        // Line 1331/1332's pair: `None` for the same reason as `first_byte_at`
        // above — this helper serves refusals before any response arrived —
        // and `finish`'s own struct-update overrides both once one has.
        first_token_at: None,
        first_tool_call_at: None,
        // Migration 25's four: `None` for `first_byte_at`'s reason again —
        // this helper serves refusals raised before the upstream answered,
        // and two of them before it was even asked. `finish`'s own
        // struct-update overrides all four once a response has arrived.
        first_byte_ms: None,
        first_token_ms: None,
        first_tool_call_ms: None,
        completed_ms: None,
        framing: None,
        // The translated path decodes a response rather than relaying it, and
        // nothing here reads a refusal's wording yet: a window observed on a
        // translated exchange is the successor this field's own documentation
        // in `ingress` describes, not something this helper may invent.
        context_limit_tokens: None,
        tokens: None,
        // Migration 24's two: `None` here, because this helper serves the
        // refusals that happen *before* a request has decoded and there is
        // nothing to derive them from yet. `serve`'s own `decoded` closure
        // overrides both on every return after the decode.
        effort: None,
        turn_shape: None,
        // Line 1334's pair: `None` for the same reason as `effort` and
        // `turn_shape` above — a request that never decoded has no tool-use
        // count and no error tool-result count to derive. `decoded` sets
        // `repairs` on every return past the decode, and `finish` sets
        // `tool_rounds` on every return past a response arriving.
        tool_rounds: None,
        repairs: None,
    }
}

// --- field access -------------------------------------------------------------------

/// Strict, path-carrying access to a JSON object, for the decoders.
///
/// Every key is taken out as it is read; what is left at [`Fields::finish`]
/// is a key nobody looked at, and that is a refusal naming it. The path is
/// in the wire's own spelling — `messages[2].content[0].cache_control` — so
/// a refusal points at the exact field.
pub(super) mod fields {
    use serde_json::{Map, Value};

    use super::canonical::{Unsupported, json_kind};

    pub(crate) struct Fields {
        path: String,
        map: Map<String, Value>,
    }

    /// `path[index]`.
    pub(crate) fn element(path: &str, index: usize) -> String {
        format!("{path}[{index}]")
    }

    impl Fields {
        pub(crate) fn of(value: Value, path: impl Into<String>) -> Result<Self, Unsupported> {
            let path = path.into();
            match value {
                Value::Object(map) => Ok(Self { path, map }),
                other => Err(Unsupported::new(
                    if path.is_empty() {
                        "body".to_owned()
                    } else {
                        path
                    },
                    format!("expected a JSON object, not {}", json_kind(&other)),
                )),
            }
        }

        pub(crate) fn path(&self) -> &str {
            &self.path
        }

        /// The path of `key` under this object.
        pub(crate) fn at(&self, key: &str) -> String {
            if self.path.is_empty() {
                key.to_owned()
            } else {
                format!("{}.{key}", self.path)
            }
        }

        pub(crate) fn take(&mut self, key: &str) -> Option<Value> {
            self.map.remove(key)
        }

        pub(crate) fn take_string(&mut self, key: &str) -> Result<Option<String>, Unsupported> {
            match self.take(key) {
                None | Some(Value::Null) => Ok(None),
                Some(Value::String(text)) => Ok(Some(text)),
                Some(other) => Err(self.wrong(key, "a string", &other)),
            }
        }

        pub(crate) fn require_string(&mut self, key: &str) -> Result<String, Unsupported> {
            self.take_string(key)?
                .ok_or_else(|| Unsupported::new(self.at(key), "this field is required"))
        }

        pub(crate) fn take_u64(&mut self, key: &str) -> Result<Option<u64>, Unsupported> {
            match self.take(key) {
                None | Some(Value::Null) => Ok(None),
                Some(Value::Number(number)) if number.as_u64().is_some() => Ok(number.as_u64()),
                Some(other) => Err(self.wrong(key, "a non-negative integer", &other)),
            }
        }

        pub(crate) fn take_f64(&mut self, key: &str) -> Result<Option<f64>, Unsupported> {
            match self.take(key) {
                None | Some(Value::Null) => Ok(None),
                Some(Value::Number(number)) if number.as_f64().is_some() => Ok(number.as_f64()),
                Some(other) => Err(self.wrong(key, "a number", &other)),
            }
        }

        pub(crate) fn take_bool(&mut self, key: &str) -> Result<Option<bool>, Unsupported> {
            match self.take(key) {
                None | Some(Value::Null) => Ok(None),
                Some(Value::Bool(flag)) => Ok(Some(flag)),
                Some(other) => Err(self.wrong(key, "a boolean", &other)),
            }
        }

        pub(crate) fn take_array(&mut self, key: &str) -> Result<Option<Vec<Value>>, Unsupported> {
            match self.take(key) {
                None | Some(Value::Null) => Ok(None),
                Some(Value::Array(items)) => Ok(Some(items)),
                Some(other) => Err(self.wrong(key, "an array", &other)),
            }
        }

        pub(crate) fn take_object(&mut self, key: &str) -> Result<Option<Fields>, Unsupported> {
            match self.take(key) {
                None | Some(Value::Null) => Ok(None),
                Some(object @ Value::Object(_)) => Ok(Some(Fields::of(object, self.at(key))?)),
                Some(other) => Err(self.wrong(key, "an object", &other)),
            }
        }

        /// Refuse, by name, a key that is present — whatever its value.
        pub(crate) fn refuse_if_present(
            &mut self,
            key: &str,
            reason: &str,
        ) -> Result<(), Unsupported> {
            match self.take(key) {
                None => Ok(()),
                Some(_) => Err(Unsupported::new(self.at(key), reason)),
            }
        }

        /// Drop a key on purpose. Every call site is a named decision, and
        /// the codec's `IGNORED_FIELDS` lists it.
        pub(crate) fn ignore(&mut self, key: &str) {
            self.map.remove(key);
        }

        /// Refuse whatever nobody read, by name.
        pub(crate) fn finish(self) -> Result<(), Unsupported> {
            let mut left: Vec<&String> = self.map.keys().collect();
            left.sort();
            match left.first() {
                None => Ok(()),
                Some(key) => Err(Unsupported::new(
                    self.at(key),
                    "this field is not one the codec carries, and nothing is dropped silently",
                )),
            }
        }

        fn wrong(&self, key: &str, expected: &str, got: &Value) -> Unsupported {
            Unsupported::new(
                self.at(key),
                format!("expected {expected}, not {}", json_kind(got)),
            )
        }
    }
}

#[cfg(test)]
mod tests;
