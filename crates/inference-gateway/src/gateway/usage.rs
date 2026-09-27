//! The bounded observer that reads a provider's own usage figures out of
//! bytes the relay is already forwarding — and nothing else in them.
//!
//! # What changed, and what did not
//!
//! `ingress.rs` moves a response across without looking at it. That rule was
//! **narrowed** on 2026-09-03 (`archive/glasshouse:docs/product/design-decisions.md`, *Steering
//! decisions of record* §1): the gateway may inspect a supported relayed body
//! far enough to extract structured usage and timing, because accurate usage
//! is worth more than byte-for-byte opacity. Everything else in the ruling is
//! a constraint this module has to meet rather than advice, and each one is
//! answered by a property of the code below:
//!
//! History: design-decisions.md, "Trims: gateway module docs", usage.rs module doc.

/// How many bytes of one chunk are kept for the next.
///
/// A figure or a marker can straddle a `read` boundary, so the tail of each
/// chunk is rescanned with the head of the next. It has to cover the longest
/// needle plus the longest thing read after one — [`VALUE_CAP`] bytes of a
/// string, or twenty digits of an integer — and 512 is that with room to
/// spare, which is why nothing here has to be recalculated when a needle is
/// added.
///
/// Rescanning cannot double-count: an integer field is last-value-wins, so a
/// second reading of the same bytes writes the same number, and the two
/// markers are latched by `is_none`-style guards that a second sighting
/// cannot re-fire.
const CARRY: usize = 512;

/// How far into a text value [`text_at`] looks for a non-whitespace byte.
///
/// A padding delta is a handful of spaces. A value whose first 256 bytes are
/// all whitespace is padding by any reading, so the scan stops there rather
/// than following a string of unbounded length — which is the bound that
/// keeps [`CARRY`] a constant instead of a function of the response.
const VALUE_CAP: usize = 256;

/// The JSON key spellings one wire protocol states its usage and its events
/// in.
///
/// A table, not a parser: every field is a literal that must appear in the
/// response for the fact to be recorded, and a protocol whose spellings are
/// not here records nothing. Every needle begins with `"` for the reason the
/// module header gives — a bare quote cannot occur inside JSON string
/// content, so a match is always a real key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Format {
    /// The key stating how many input tokens the provider billed.
    input: &'static str,
    /// The key stating how many output tokens it generated.
    output: &'static str,
    /// The key stating how many input tokens it served from a cache. Read as
    /// the *read* figure only: a cache-creation count is a different quantity
    /// and is not conflated with it. `None` for a protocol whose usage has no
    /// cached-input figure at all — the same "no arming needed" shape
    /// [`Format::text_arm`] already uses for an optional slot, rather than a
    /// literal chosen to never match.
    cached: Option<&'static str>,
    /// The event marker that has to be seen before [`Format::text_value`]
    /// counts as generated text, or `None` where the value key is unambiguous
    /// on its own.
    ///
    /// Anthropic and OpenAI Responses both spell a text field in more than one
    /// kind of event, so the delta's own type is what distinguishes the
    /// generated token from, say, an empty block opening. OpenAI Chat spells
    /// `content` only inside a delta, so it needs no arming.
    text_arm: Option<&'static str>,
    /// The key whose string value is the generated text, up to its opening
    /// quote.
    text_value: &'static str,
    /// The marker that a tool call was requested.
    tool_call: &'static str,
}

/// Anthropic Messages: `message_start` and `message_delta` state usage,
/// `content_block_delta` carries a `text_delta`, `content_block_start` opens a
/// `tool_use` block.
const ANTHROPIC_MESSAGES: Format = Format {
    input: "\"input_tokens\":",
    output: "\"output_tokens\":",
    cached: Some("\"cache_read_input_tokens\":"),
    text_arm: Some("\"text_delta\""),
    text_value: "\"text\":\"",
    tool_call: "\"type\":\"tool_use\"",
};

/// OpenAI Chat Completions: a final chunk (or a document) states `usage`, a
/// delta carries `content`, and a tool call arrives as `tool_calls`.
const OPENAI_CHAT: Format = Format {
    input: "\"prompt_tokens\":",
    output: "\"completion_tokens\":",
    cached: Some("\"cached_tokens\":"),
    text_arm: None,
    text_value: "\"content\":\"",
    tool_call: "\"tool_calls\":",
};

/// OpenAI Responses: usage is spelled like Anthropic's but its cached figure
/// is OpenAI's, text arrives as `response.output_text.delta`, and a tool call
/// is an output item of type `function_call`.
const OPENAI_RESPONSES: Format = Format {
    input: "\"input_tokens\":",
    output: "\"output_tokens\":",
    cached: Some("\"cached_tokens\":"),
    text_arm: Some("\"response.output_text.delta\""),
    text_value: "\"delta\":\"",
    tool_call: "\"type\":\"function_call\"",
};

/// TypeSafe System One: `usage` states exactly two counts and no cached-input
/// figure (`archive/glasshouse:docs/product/evidence/phase-66.md`, *Provider facts*). It has no
/// text delta or tool-call marker either — a decision response is not a
/// stream of generated text, so [`Format::text_arm`], [`Format::text_value`]
/// and [`Format::tool_call`] name keys this protocol's usage scan will never
/// see, the same as every other format's unreachable-until-matched fields.
const TYPESAFE_SYSTEMONE: Format = Format {
    input: "\"input_tokens\":",
    output: "\"output_tokens\":",
    cached: None,
    text_arm: None,
    text_value: "\"text\":\"",
    tool_call: "\"type\":\"tool_use\"",
};

/// The format for a protocol slug, or `None` where this relay has no
/// established spelling for one.
///
/// The slug is `harness::WireProtocol::slug`, carried on the [`Route`] the
/// ingress already chose from the request target alone. Deciding the format
/// from it rather than from the body keeps the whole of "which protocol is
/// this" one decision made in one place: `ingress::forward`'s own doc explains
/// why sniffing a body to place a request is forbidden here, and sniffing one
/// to place a *reading* of that request would be the same move with a smaller
/// blast radius rather than a different one.
///
/// `gemini-generate-content` is the slug this answers `None` for today. It
/// states its usage as `usageMetadata`, which is not in the table, so an
/// exchange relayed under it records unknown — which is the ruling's rule for
/// an unsupported format and not an oversight.
///
/// [`Route`]: super::upstream::Route
pub(super) fn format_for(protocol: &str) -> Option<Format> {
    match protocol {
        "anthropic-messages" => Some(ANTHROPIC_MESSAGES),
        "openai-chat" => Some(OPENAI_CHAT),
        "openai-responses" => Some(OPENAI_RESPONSES),
        "typesafe-systemone" => Some(TYPESAFE_SYSTEMONE),
        _ => None,
    }
}

/// The three counts a provider stated, when it stated both of the two that
/// are not optional.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Usage {
    pub(super) input: u64,
    pub(super) output: u64,
    pub(super) cached: Option<u64>,
}

/// Which latched markers a single [`Extractor::feed`] was the first to see.
///
/// Returned rather than stored with a clock reading, so that this module
/// never reads a clock and `ingress` keeps every timestamp it records in one
/// file — the same division `first_byte_at` already follows.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) struct Seen {
    /// This chunk carried the first generated text of the response.
    pub(super) first_text: bool,
    /// This chunk carried the first tool call of the response.
    pub(super) first_tool_call: bool,
}

/// How the provider delivered the response, which decides whether an instant
/// inside it means anything.
///
/// A **streamed** response puts each event on the wire when the provider
/// produced it, so the moment a text delta passes the seam is a real reading
/// of when that token was generated. A **document** arrives as one body: the
/// markers are all in it, but the moment each one passes says only how fast
/// the socket drained, which is a fact about the network and not about the
/// provider.
///
/// So a document records its usage — those are the provider's own digits
/// either way — and records no instants. `translate` answers this differently
/// on its own path (`FirstEvents::of_document` sets both to `first_byte_at`,
/// because a decoded document proves a qualifying event exists), and the
/// difference is deliberate: deriving a timestamp from another timestamp is a
/// step this relay has no license for, and the 2026-09-03 ruling's "never an
/// estimate" is what withholds it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Delivery {
    /// `content-type: text/event-stream` — each event arrived when the
    /// provider sent it.
    Streamed,
    /// Anything else: one body, whose internal boundaries are not observable
    /// as instants.
    Document,
}

/// The observer itself: a sliding window over one response, four integers and
/// three flags.
///
/// It is fed the same bytes `http::pump` is about to write and has no way to
/// change them. Nothing here grows with the length of the response.
pub(super) struct Extractor {
    format: Format,
    delivery: Delivery,
    /// The bytes still worth looking at: at most [`CARRY`] retained from the
    /// last chunk, plus the current one. Overwritten as it slides, never
    /// copied anywhere else, and dropped with the stream.
    window: Vec<u8>,
    input: Option<u64>,
    output: Option<u64>,
    cached: Option<u64>,
    /// Whether a text value seen from here on is generated text — always true
    /// for a format with no [`Format::text_arm`].
    text_armed: bool,
    first_text: bool,
    first_tool_call: bool,
}

impl Extractor {
    pub(super) fn new(format: Format, delivery: Delivery) -> Self {
        Self {
            format,
            delivery,
            window: Vec::new(),
            input: None,
            output: None,
            cached: None,
            text_armed: format.text_arm.is_none(),
            first_text: false,
            first_tool_call: false,
        }
    }

    /// Look at the bytes just read, and say which markers this chunk was the
    /// first to carry.
    ///
    /// `chunk` is borrowed, scanned and let go. The caller writes exactly what
    /// it read regardless of what this returns.
    pub(super) fn feed(&mut self, chunk: &[u8]) -> Seen {
        if chunk.is_empty() {
            return Seen::default();
        }
        self.window.extend_from_slice(chunk);
        let seen = self.scan();
        if self.window.len() > CARRY {
            self.window.drain(..self.window.len() - CARRY);
        }
        seen
    }

    /// What the provider stated, or `None` for unknown.
    ///
    /// Both counts or neither. A response that stated an input figure and then
    /// stopped has an honest input count and no output count, and there is
    /// nothing to put in the output column that would not be invented — so the
    /// row says unknown, which is the ruling's own rule and the reason this
    /// returns an `Option` rather than defaulting a zero.
    pub(super) fn usage(&self) -> Option<Usage> {
        Some(Usage {
            input: self.input?,
            output: self.output?,
            cached: self.cached,
        })
    }

    /// One pass over the window.
    ///
    /// Only positions holding a `"` are candidates, which is what keeps this
    /// linear in the chunk rather than in the chunk times the table: a needle
    /// can start nowhere else.
    fn scan(&mut self) -> Seen {
        let mut seen = Seen::default();
        // Moved out and back so the loop can read the window while writing
        // this struct's counters. A move, not a copy.
        let window = std::mem::take(&mut self.window);
        let buf = window.as_slice();
        let mut i = 0;
        while i < buf.len() {
            if buf[i] != b'"' {
                i += 1;
                continue;
            }
            let rest = &buf[i..];
            for (needle, slot) in [
                (self.format.input, &mut self.input),
                (self.format.output, &mut self.output),
            ] {
                if rest.starts_with(needle.as_bytes())
                    && let Digits::Value(value) = digits_at(buf, i + needle.len())
                {
                    // Last value wins: Anthropic states an output count in
                    // `message_start` and restates the final one in
                    // `message_delta`, and the later statement is the one the
                    // provider means.
                    *slot = Some(value);
                }
            }
            if let Some(needle) = self.format.cached
                && rest.starts_with(needle.as_bytes())
                && let Digits::Value(value) = digits_at(buf, i + needle.len())
            {
                self.cached = Some(value);
            }
            // The markers are only looked for on a streamed delivery — see
            // [`Delivery`] for why an instant inside a document is a reading
            // of the socket rather than of the provider.
            let streamed = self.delivery == Delivery::Streamed;
            if streamed && !self.first_text {
                if let Some(arm) = self.format.text_arm
                    && rest.starts_with(arm.as_bytes())
                {
                    self.text_armed = true;
                }
                if self.text_armed
                    && rest.starts_with(self.format.text_value.as_bytes())
                    && text_at(buf, i + self.format.text_value.len()) == Text::Real
                {
                    self.first_text = true;
                    seen.first_text = true;
                }
            }
            if streamed
                && !self.first_tool_call
                && rest.starts_with(self.format.tool_call.as_bytes())
            {
                self.first_tool_call = true;
                seen.first_tool_call = true;
            }
            i += 1;
        }
        self.window = window;
        seen
    }
}

/// What reading digits after a key found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Digits {
    /// The run reached the end of the window, so the number may continue in
    /// the next chunk. Not recorded: [`CARRY`] guarantees this position is
    /// rescanned once the rest has arrived.
    Incomplete,
    /// Not a number this module will record — no digits at all (`null`, a
    /// string, an object), or more than a `u64` can hold. Never approximated.
    Refused,
    Value(u64),
}

/// The unsigned integer a key introduces, if the provider wrote one.
fn digits_at(buf: &[u8], mut i: usize) -> Digits {
    while i < buf.len() && (buf[i] == b' ' || buf[i] == b'\t') {
        i += 1;
    }
    let start = i;
    let mut value: u64 = 0;
    let mut overflowed = false;
    while i < buf.len() && buf[i].is_ascii_digit() {
        match value
            .checked_mul(10)
            .and_then(|shifted| shifted.checked_add(u64::from(buf[i] - b'0')))
        {
            Some(next) => value = next,
            None => overflowed = true,
        }
        i += 1;
    }
    if i == start {
        // Either the value is not a number, or the window ended before one
        // began — and the second case is `Incomplete` rather than `Refused`
        // so a key at the very end of a chunk is not decided against.
        return if i >= buf.len() {
            Digits::Incomplete
        } else {
            Digits::Refused
        };
    }
    if i >= buf.len() {
        return Digits::Incomplete;
    }
    if overflowed {
        return Digits::Refused;
    }
    Digits::Value(value)
}

/// What [`text_at`] found in a text field's value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Text {
    /// The string ended before the window did, and every byte of it was
    /// whitespace — line 1332's padding, which is not a generated token.
    Padding,
    /// A non-whitespace byte, so a real token passed the seam.
    Real,
    /// The window ended mid-value; [`CARRY`] has it and the next chunk decides.
    Incomplete,
}

/// Whether the string starting at `i` holds a real character or only padding.
///
/// It reads forward from the opening quote and stops at the first byte that
/// answers the question: any non-whitespace byte means yes, the closing quote
/// means no. A backslash is non-whitespace, so an escape sequence answers
/// *yes* at its first byte and no unescaping is ever needed — which is why
/// this reads a value without being able to reconstruct one.
fn text_at(buf: &[u8], i: usize) -> Text {
    let end = buf.len().min(i + VALUE_CAP);
    let mut j = i;
    while j < end {
        match buf[j] {
            b'"' => return Text::Padding,
            b' ' | b'\t' | b'\r' | b'\n' => j += 1,
            _ => return Text::Real,
        }
    }
    if end == i + VALUE_CAP {
        // [`VALUE_CAP`] bytes of whitespace with no character in them is
        // padding, and stopping here is what bounds the scan.
        Text::Padding
    } else {
        Text::Incomplete
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One `pump` chunk, so the bound below is stated against the size the
    /// relay actually reads with.
    const CHUNK: usize = 8 * 1024;

    fn anthropic() -> Extractor {
        Extractor::new(ANTHROPIC_MESSAGES, Delivery::Streamed)
    }

    #[test]
    fn a_protocol_the_table_does_not_name_has_no_format_at_all() {
        assert!(format_for("gemini-generate-content").is_none());
        assert!(format_for("").is_none());
        assert!(format_for("anthropic").is_none());
        // ... and the three that are named do have one, or the assertion
        // above would pass for a table that had gone empty.
        assert!(format_for("anthropic-messages").is_some());
        assert!(format_for("openai-chat").is_some());
        assert!(format_for("openai-responses").is_some());
        assert!(format_for("typesafe-systemone").is_some());
    }

    #[test]
    fn a_typesafe_systemone_response_yields_the_two_counts_and_no_cached_figure() {
        let mut extractor = Extractor::new(TYPESAFE_SYSTEMONE, Delivery::Document);
        extractor.feed(
            br#"{"model":"jev-latest","answers":{"key":{"choice":"a"}},"usage":{"input_tokens":330,"output_tokens":34}}"#,
        );
        assert_eq!(
            extractor.usage(),
            Some(Usage {
                input: 330,
                output: 34,
                cached: None,
            })
        );
    }

    #[test]
    fn an_anthropic_stream_yields_the_counts_the_provider_stated() {
        let mut extractor = anthropic();
        extractor.feed(
            br#"data: {"type":"message_start","message":{"usage":{"input_tokens":120,"cache_read_input_tokens":100,"output_tokens":1}}}"#,
        );
        extractor.feed(br#"data: {"type":"message_delta","usage":{"output_tokens":33}}"#);
        assert_eq!(
            extractor.usage(),
            Some(Usage {
                input: 120,
                output: 33,
                cached: Some(100),
            }),
            "the later output figure is the one the provider means"
        );
    }

    #[test]
    fn an_openai_chat_stream_yields_the_counts_the_provider_stated() {
        let mut extractor = Extractor::new(OPENAI_CHAT, Delivery::Streamed);
        extractor.feed(
            br#"data: {"choices":[],"usage":{"prompt_tokens":40,"completion_tokens":12,"total_tokens":52,"prompt_tokens_details":{"cached_tokens":32}}}"#,
        );
        assert_eq!(
            extractor.usage(),
            Some(Usage {
                input: 40,
                output: 12,
                cached: Some(32),
            })
        );
    }

    #[test]
    fn an_openai_responses_stream_yields_the_counts_the_provider_stated() {
        let mut extractor = Extractor::new(OPENAI_RESPONSES, Delivery::Streamed);
        extractor.feed(
            br#"data: {"type":"response.completed","response":{"usage":{"input_tokens":7,"input_tokens_details":{"cached_tokens":2},"output_tokens":9}}}"#,
        );
        assert_eq!(
            extractor.usage(),
            Some(Usage {
                input: 7,
                output: 9,
                cached: Some(2),
            })
        );
    }

    #[test]
    fn a_response_that_states_only_one_of_the_two_counts_is_unknown() {
        let mut extractor = anthropic();
        extractor.feed(br#"{"usage":{"input_tokens":120}}"#);
        assert_eq!(
            extractor.usage(),
            None,
            "half a reading is unknown, not a zero in the other column"
        );
    }

    #[test]
    fn a_response_that_states_no_usage_at_all_is_unknown() {
        let mut extractor = anthropic();
        extractor.feed(br#"{"type":"error","error":{"type":"overloaded_error"}}"#);
        assert_eq!(extractor.usage(), None);
    }

    /// The property the module header rests on: a bare `"` cannot occur inside
    /// a JSON string, so text a model generated cannot spell a key.
    #[test]
    fn a_count_spelled_inside_generated_text_is_not_read_as_a_count() {
        let mut extractor = anthropic();
        extractor.feed(
            br#"data: {"type":"content_block_delta","delta":{"type":"text_delta","text":"set \"input_tokens\":99 in the request"}}"#,
        );
        assert_eq!(
            extractor.usage(),
            None,
            "an escaped key inside generated text must not be read as usage"
        );
    }

    #[test]
    fn a_count_split_across_two_chunks_is_still_read() {
        let mut extractor = anthropic();
        extractor.feed(br#"{"usage":{"input_tok"#);
        extractor.feed(br#"ens":12"#);
        extractor.feed(br#"0,"output_tokens":33}}"#);
        assert_eq!(
            extractor.usage(),
            Some(Usage {
                input: 120,
                output: 33,
                cached: None,
            }),
            "a figure straddling two reads must not be truncated to its first digits"
        );
    }

    #[test]
    fn a_non_numeric_or_oversized_count_is_refused_rather_than_approximated() {
        let mut extractor = anthropic();
        extractor.feed(br#"{"input_tokens":null,"output_tokens":99999999999999999999999}"#);
        assert_eq!(extractor.usage(), None);
    }

    #[test]
    fn the_first_real_text_delta_is_the_first_token_and_padding_is_not() {
        let mut extractor = anthropic();
        let opening = extractor.feed(
            br#"data: {"type":"content_block_start","content_block":{"type":"text","text":""}}"#,
        );
        assert!(!opening.first_text, "an empty block opening is not a token");
        let padding = extractor.feed(
            br#"data: {"type":"content_block_delta","delta":{"type":"text_delta","text":"   "}}"#,
        );
        assert!(!padding.first_text, "whitespace padding is not a token");
        let real = extractor.feed(
            br#"data: {"type":"content_block_delta","delta":{"type":"text_delta","text":"Checking."}}"#,
        );
        assert!(real.first_text);
        let second = extractor.feed(
            br#"data: {"type":"content_block_delta","delta":{"type":"text_delta","text":"More."}}"#,
        );
        assert!(
            !second.first_text,
            "the first token is latched, not restamped"
        );
    }

    #[test]
    fn an_openai_chat_role_chunk_is_not_a_token_and_real_content_is() {
        let mut extractor = Extractor::new(OPENAI_CHAT, Delivery::Streamed);
        let role =
            extractor.feed(br#"data: {"choices":[{"delta":{"role":"assistant","content":""}}]}"#);
        assert!(!role.first_text, "an empty content delta is not a token");
        let real = extractor.feed(br#"data: {"choices":[{"delta":{"content":"Hi"}}]}"#);
        assert!(real.first_text);
    }

    #[test]
    fn the_first_tool_call_is_latched_on_each_format() {
        for (format, event) in [
            (
                ANTHROPIC_MESSAGES,
                &br#"data: {"type":"content_block_start","content_block":{"type":"tool_use","id":"tu_1"}}"#[..],
            ),
            (
                OPENAI_CHAT,
                &br#"data: {"choices":[{"delta":{"tool_calls":[{"index":0}]}}]}"#[..],
            ),
            (
                OPENAI_RESPONSES,
                &br#"data: {"type":"response.output_item.added","item":{"type":"function_call"}}"#[..],
            ),
        ] {
            let mut extractor = Extractor::new(format, Delivery::Streamed);
            assert!(extractor.feed(event).first_tool_call, "{format:?}");
            assert!(
                !extractor.feed(event).first_tool_call,
                "the first tool call is latched, not restamped: {format:?}"
            );
        }
    }

    /// A marker split across a read boundary is still seen — the same
    /// [`CARRY`] property as the split count, on the other kind of needle.
    #[test]
    fn a_marker_split_across_two_chunks_is_still_seen() {
        let mut extractor = anthropic();
        assert!(!extractor.feed(br#"{"delta":{"type":"text_de"#).first_text);
        assert!(extractor.feed(br#"lta","text":"Hi"}}"#).first_text);
    }

    /// The bound the ruling asks for, checked against the state rather than
    /// promised: a megabyte of response leaves the observer holding [`CARRY`]
    /// bytes plus at most the chunk it was last handed.
    #[test]
    fn the_window_never_grows_with_the_length_of_the_response() {
        let mut extractor = anthropic();
        let chunk = vec![b'x'; CHUNK];
        extractor.feed(&chunk);
        extractor.feed(&chunk);
        // Whatever the allocator settled on after two reads is what it still
        // holds after a hundred and thirty more: the window is a function of
        // the chunk size, not of how much has gone past.
        let settled = extractor.window.capacity();
        for _ in 0..128 {
            extractor.feed(&chunk);
            assert!(
                extractor.window.len() <= CARRY,
                "between reads the observer holds at most {CARRY} bytes, held {}",
                extractor.window.len()
            );
        }
        assert_eq!(
            extractor.window.capacity(),
            settled,
            "a megabyte of response grew the observer's own buffer"
        );
        assert!(
            settled <= 2 * (CARRY + CHUNK),
            "the window reserved {settled} bytes for a {CHUNK}-byte read"
        );
    }

    /// A usage figure that arrives after a megabyte of body is still read —
    /// the sliding window is not a cap on how much of the stream is observed,
    /// only on how much is held at once.
    #[test]
    fn a_count_stated_at_the_end_of_a_long_response_is_still_read() {
        let mut extractor = anthropic();
        for _ in 0..128 {
            extractor.feed(&vec![b'x'; CHUNK]);
        }
        extractor.feed(br#"{"usage":{"input_tokens":4,"output_tokens":5}}"#);
        assert_eq!(
            extractor.usage(),
            Some(Usage {
                input: 4,
                output: 5,
                cached: None,
            })
        );
    }

    /// A document states its usage like any other body and records no
    /// instants — the whole of [`Delivery`]'s rule, on a body that carries
    /// every marker.
    #[test]
    fn a_document_states_its_usage_and_no_instants() {
        let mut extractor = Extractor::new(ANTHROPIC_MESSAGES, Delivery::Document);
        let seen = extractor.feed(
            br#"{"type":"message","content":[{"type":"text","text":"hi there"},{"type":"tool_use","id":"tu_1"}],"usage":{"input_tokens":999,"output_tokens":888}}"#,
        );
        assert_eq!(
            extractor.usage(),
            Some(Usage {
                input: 999,
                output: 888,
                cached: None,
            }),
            "a document's own digits are the provider's, however it was delivered"
        );
        assert!(
            !seen.first_text && !seen.first_tool_call,
            "a document exposes no boundary an instant could describe"
        );
    }

    /// Compressed bytes are not JSON, so nothing matches and the row says
    /// unknown — the ruling's rule for an unrecognised body shape, reached
    /// without a special case for it.
    #[test]
    fn a_body_that_is_not_json_states_nothing() {
        let mut extractor = anthropic();
        extractor.feed(&[0x1f, 0x8b, 0x08, 0x00, 0xde, 0xad, 0xbe, 0xef]);
        assert_eq!(extractor.usage(), None);
        assert!(!extractor.first_text);
        assert!(!extractor.first_tool_call);
    }
}
