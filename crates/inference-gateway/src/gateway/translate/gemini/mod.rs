//! The Google Generative Language codec: `generateContent` and
//! `streamGenerateContent` requests, responses and stream chunks, into and
//! out of [`super::canonical`].
//!
//! The fourth wire, and the first that differs from the other three in
//! **shape**, not only in spelling. Five decisions hold now: the model
//! addresses a path segment, not the body ([`Gemini::outbound_endpoint`],
//! [`Gemini::refuse_unencodable`]); a function call carries no id, so a tool
//! result is matched by NAME within the same request ([`tool_call_names`],
//! [`minted_id`]); `finishReason: "STOP"` still means [`StopReason::ToolUse`]
//! when the candidate is entirely function calls
//! ([`decode_finish_reason`]); the end-user identifier is the only request
//! field dropped by name ([`IGNORED_FIELDS`]); and a stream ends with no
//! terminator, so `finishReason` itself is the one ([`ChunkDecoder::finish`]).
//!
//! History: design-decisions.md, "Trims: gateway/translate/gemini/mod.rs", module doc.

use serde_json::{Map, Value, json};

use super::canonical::{
    Block, BlockStart, Delta, EffortLevel, EffortRequest, ImageSource, Message, Request, Response,
    Role, StopReason, StreamEvent, ToolChoice, ToolDefinition, Unsupported, Usage, json_kind,
    parse_tool_input,
};
use super::fields::{Fields, element};
use super::stream::{self, SseEvent};
use super::{CacheDisposition, Claim, Codec, EffortDisposition, StreamDecoder, StreamEncoder};

pub(super) const PROTOCOL: &str = "gemini-generate-content";

/// The one target this codec translates, as a person reads it.
///
/// Unlike the other three codecs this is a **shape**, not a path: the model
/// is a path segment, so there is no single literal to match or to post to.
/// [`Gemini::claim`] decides what belongs to this protocol and
/// [`Gemini::outbound_endpoint`] builds the path a translated request is
/// posted to; this constant is what a refusal names.
pub(super) const ENDPOINT: &str = "/models/{model}:generateContent";

/// The API version a translated request is posted to.
///
/// `v1beta` rather than `v1`: function declarations, `toolConfig` and
/// `systemInstruction` — the three things a harness's tooling rides on — are
/// documented there, and it is the version every current Gemini client
/// sends. Stated by this codec rather than left to the provider's base URL,
/// because a relayed request carries the client's own version segment and
/// the two would compose into `/v1beta/v1beta/…`.
const OUTBOUND_VERSION: &str = "/v1beta";

/// The method suffix of a non-streamed request.
const GENERATE: &str = "generateContent";
/// The method suffix of a streamed request.
const STREAM_GENERATE: &str = "streamGenerateContent";

/// Google's own version segments, longest first. `VERSION_SEGMENT` — the
/// `/v1` the rest of the gateway strips before classifying a target — does
/// not cover `v1beta`, which is the segment every current Gemini client
/// sends, so this codec strips its own.
const VERSION_SEGMENTS: [&str; 3] = ["/v1beta", "/v1alpha", "/v1"];

/// The request fields and shapes this codec refuses, with the reason each
/// refusal carries.
pub(super) const REFUSED_FIELDS: &[(&str, &str)] = &[
    (
        "safetySettings",
        "per-request safety thresholds have no equivalent on the protocols this gateway \
         translates from, and silently dropping them would loosen a limit the caller set",
    ),
    (
        "cachedContent",
        "Gemini's explicit context cache is addressed by a resource name no other protocol \
         issues",
    ),
    (
        "generationConfig.responseMimeType",
        "a structured output format has no equivalent in the canonical form",
    ),
    (
        "generationConfig.responseSchema",
        "a structured output schema has no equivalent in the canonical form",
    ),
    (
        "generationConfig.responseJsonSchema",
        "a structured output schema has no equivalent in the canonical form",
    ),
    (
        "generationConfig.thinkingConfig",
        "a thinking budget has no equivalent in the canonical form",
    ),
    (
        "generationConfig.candidateCount",
        "more than one candidate per request has no equivalent in the canonical form",
    ),
    (
        "generationConfig.topK",
        "top-k sampling has no equivalent in the canonical form",
    ),
    (
        "generationConfig.seed",
        "deterministic sampling has no equivalent in the canonical form",
    ),
    (
        "generationConfig.presencePenalty",
        "presence penalties have no equivalent in the canonical form",
    ),
    (
        "generationConfig.frequencyPenalty",
        "frequency penalties have no equivalent in the canonical form",
    ),
    (
        "generationConfig.responseLogprobs",
        "log probabilities have no equivalent in the canonical form",
    ),
    (
        "generationConfig.logprobs",
        "log probabilities have no equivalent in the canonical form",
    ),
    (
        "generationConfig.responseModalities",
        "a non-text response modality has no equivalent in the canonical form",
    ),
    (
        "generationConfig.speechConfig",
        "audio output has no equivalent in the canonical form",
    ),
    (
        "fileData",
        "a Files-API URI is not a URL anything downstream could fetch, and the canonical form \
         carries only inline bytes or a public URL",
    ),
    (
        "thought",
        "a thought part is the model's own reasoning and has no equivalent in the canonical form",
    ),
    (
        "thinking block",
        "an Anthropic thinking or redacted_thinking block is another provider's private \
         reasoning and has no Gemini equivalent, matching this codec's own refusal of a \
         `thought` part in the other direction",
    ),
    (
        "executableCode",
        "Gemini's code-execution tool is a server-side tool no other protocol declares",
    ),
    (
        "codeExecution",
        "Gemini's code-execution tool is a server-side tool no other protocol declares",
    ),
    (
        "googleSearch",
        "Gemini's search grounding is a server-side tool no other protocol declares",
    ),
    (
        "googleSearchRetrieval",
        "Gemini's search grounding is a server-side tool no other protocol declares",
    ),
    (
        "parallel_tool_calls",
        "Gemini has no parameter that disables parallel function calling, and a request that \
         asked for it would be answered as though it had not",
    ),
    (
        "model",
        "a Gemini request addresses its model in the path, so the model's name must be one a \
         path segment can carry: no `/`, `?`, `#` or whitespace",
    ),
    (
        "tool_use_id",
        "Gemini matches a function response to its call by NAME, and this result's id names no \
         tool-use block in the same request, so the call it answers cannot be identified",
    ),
    (
        "image",
        "Gemini carries an image as inline bytes; a URL source has no equivalent, and fetching \
         it here would make the gateway a downloader",
    ),
];

/// Fields dropped by name — see the module doc's fourth decision for `user`,
/// which is the only *request* field in this list and the only one whose
/// absence the caller could notice.
pub(super) const IGNORED_FIELDS: &[&str] = &[
    "user",
    "modelVersion",
    "candidates[].index",
    "candidates[].safetyRatings",
    "candidates[].citationMetadata",
    "candidates[].groundingMetadata",
    "candidates[].avgLogprobs",
    "candidates[].tokenCount",
    "candidates[].finishMessage",
    "promptFeedback",
    "usageMetadata.totalTokenCount",
    "usageMetadata.promptTokensDetails",
    "usageMetadata.candidatesTokensDetails",
    "usageMetadata.cacheTokensDetails",
    "usageMetadata.toolUsePromptTokenCount",
    "usageMetadata.toolUsePromptTokensDetails",
];

fn reason(field: &str) -> &'static str {
    REFUSED_FIELDS
        .iter()
        .find(|(name, _)| *name == field)
        .map(|(_, reason)| *reason)
        .expect("every refusal named in this file is listed in REFUSED_FIELDS")
}

/// The ceiling [`thinking_budget`] clamps to (GH-EFFORT-CARRY) — the
/// tightest of the documented per-model maxima for Gemini's thinking-capable
/// models (`ai.google.dev/gemini-api/docs/thinking`), so a clamped value
/// never exceeds any of them: 2.5 Flash and 2.5 Flash-Lite document a range
/// up to 24,576; 2.5 Pro's own ceiling is higher, at 32,768. Using the
/// smaller number means this codec never has to read the model name back out
/// of the path to pick the right ceiling, at the cost of never using 2.5
/// Pro's full range — a limit worth recording rather than a per-model table
/// this package does not need to build.
const GEMINI_THINKING_BUDGET_MAX: u64 = 24_576;

/// [`EffortDisposition::Carried`]'s note for this codec's
/// `thinkingConfig.thinkingBudget` (GH-EFFORT-CARRY).
const THINKING_BUDGET_NOTE: &str = "set to the harness's `thinking.budget_tokens`, clamped to \
     `GEMINI_THINKING_BUDGET_MAX` and never raised; a harness-stated word (no decoder produces \
     one today) maps to a fraction of that ceiling instead";

/// `effort`'s token budget, clamped down (never up) to
/// [`GEMINI_THINKING_BUDGET_MAX`] — the numeric field this wire takes
/// directly, unlike the word-based `reasoning_effort` the two OpenAI codecs
/// write. A harness-stated word maps to a documented fraction of the
/// ceiling, for the path no decoder in this codebase exercises today (see
/// `EffortRequest::level`'s own doc).
fn thinking_budget(effort: &EffortRequest) -> u64 {
    match effort.budget_tokens {
        Some(budget) => budget.min(GEMINI_THINKING_BUDGET_MAX),
        None => {
            let fraction = match effort.level() {
                EffortLevel::Minimal => 0.10,
                EffortLevel::Low => 0.25,
                EffortLevel::Medium => 0.50,
                // This wire has a hard ceiling, so there is nothing above
                // the whole of it to ask for. `xhigh` and `max` are not
                // dropped -- they arrive here as the most this target can
                // spend, which is the honest translation of "more than high"
                // onto a budget that stops.
                EffortLevel::High | EffortLevel::Xhigh | EffortLevel::Max => 1.0,
            };
            (GEMINI_THINKING_BUDGET_MAX as f64 * fraction) as u64
        }
    }
}

pub(super) struct Gemini;

impl Codec for Gemini {
    fn protocol(&self) -> &'static str {
        PROTOCOL
    }

    fn endpoint(&self) -> &'static str {
        ENDPOINT
    }

    /// `/models/<model>:<method>`, with Google's own version segment
    /// stripped rather than the gateway's `/v1`.
    ///
    /// A path with no `:` is not claimed at all — that includes `/models`
    /// itself, which is a model *listing* on this wire and on OpenAI's, and
    /// which the gateway has always answered with its plain `404`.
    fn claim(&self, path: &str) -> Claim {
        let path = VERSION_SEGMENTS
            .iter()
            .find_map(|segment| match path.strip_prefix(segment) {
                Some(rest) if rest.starts_with('/') => Some(rest),
                _ => None,
            })
            .unwrap_or(path);
        let Some(rest) = path.strip_prefix("/models/") else {
            return Claim::None;
        };
        let Some((model, method)) = rest.split_once(':') else {
            return Claim::None;
        };
        if model.is_empty() || model.contains('/') || method.is_empty() {
            return Claim::None;
        }
        if method == GENERATE || method == STREAM_GENERATE {
            Claim::Endpoint
        } else {
            // `:countTokens`, `:embedContent`, `:batchEmbedContents` — this
            // protocol's own surface, and none of it is inference this
            // gateway translates.
            Claim::Other
        }
    }

    /// `/v1beta/models/<model>:generateContent`, or the streaming method
    /// with `alt=sse` when the harness asked for a stream.
    ///
    /// The version segment is here rather than on the provider's base URL —
    /// see the module doc's first decision, and the relay test that found
    /// it. `alt=sse` is not decoration either: without it Google answers a
    /// streamed JSON **array** rather than server-sent events, and the
    /// reader on the other side of this call frames events.
    fn outbound_endpoint(&self, request: &Request) -> String {
        let model = model_segment(&request.model);
        if request.stream {
            format!("{OUTBOUND_VERSION}/models/{model}:{STREAM_GENERATE}?alt=sse")
        } else {
            format!("{OUTBOUND_VERSION}/models/{model}:{GENERATE}")
        }
    }

    fn refuse_unencodable(&self, request: &Request) -> Result<(), Unsupported> {
        let model = model_segment(&request.model);
        if model.is_empty()
            || model
                .chars()
                .any(|c| c.is_whitespace() || matches!(c, '/' | '?' | '#'))
        {
            return Err(Unsupported::new("model", reason("model")));
        }
        if request.parallel_tool_calls == Some(false) {
            return Err(Unsupported::new(
                "parallel_tool_calls",
                reason("parallel_tool_calls"),
            ));
        }
        // Every tool result must name a call in this same request — see the
        // module doc's second decision.
        let names = tool_call_names(request);
        for message in &request.messages {
            for block in &message.blocks {
                match block {
                    Block::ToolResult { tool_use_id, .. } => {
                        if !names.iter().any(|(id, _)| id == tool_use_id) {
                            return Err(Unsupported::new("tool_use_id", reason("tool_use_id")));
                        }
                    }
                    Block::Image(ImageSource::Url(_)) => {
                        return Err(Unsupported::new("image", reason("image")));
                    }
                    Block::Thinking { .. } | Block::RedactedThinking { .. } => {
                        return Err(Unsupported::new("thinking block", reason("thinking block")));
                    }
                    Block::Text(_) | Block::Image(_) | Block::ToolUse { .. } => {}
                }
            }
        }
        Ok(())
    }

    fn decode_request(&self, body: &[u8]) -> Result<Request, Unsupported> {
        decode_request(body)
    }

    fn encode_request(&self, request: &Request) -> Vec<u8> {
        encode_request(request)
    }

    fn decode_response(&self, body: &[u8]) -> Result<Response, Unsupported> {
        decode_response(body)
    }

    fn encode_response(&self, response: &Response) -> Vec<u8> {
        encode_response(response)
    }

    fn stream_decoder(&self) -> Box<dyn StreamDecoder + Send> {
        Box::new(ChunkDecoder::default())
    }

    fn stream_encoder(&self) -> Box<dyn StreamEncoder + Send> {
        Box::new(ChunkEncoder::default())
    }

    /// Google's `status` vocabulary, which is what a Gemini client reads out
    /// of an error document — never the HTTP number twice.
    fn error_kind(&self, status: u16) -> &'static str {
        match status {
            400 => "INVALID_ARGUMENT",
            401 => "UNAUTHENTICATED",
            403 => "PERMISSION_DENIED",
            404 => "NOT_FOUND",
            413 => "INVALID_ARGUMENT",
            429 => "RESOURCE_EXHAUSTED",
            _ => "INTERNAL",
        }
    }

    fn encode_error(&self, kind: &str, message: &str) -> Vec<u8> {
        // `code` is null rather than a number: Google's `code` is *its* own
        // numeric status, and a refusal written here came from the gateway,
        // not from Google. Inventing one would let a reader attribute this
        // document to the provider.
        json!({"error": {"code": Value::Null, "message": message, "status": kind}})
            .to_string()
            .into_bytes()
    }

    fn encode_stream_error(&self, kind: &str, message: &str) -> Vec<u8> {
        stream::encode(
            None,
            &json!({"error": {"code": Value::Null, "message": message, "status": kind}})
                .to_string(),
        )
    }

    fn decode_error(&self, body: &[u8]) -> Option<String> {
        let value: Value = serde_json::from_slice(body).ok()?;
        // Google answers a batch of errors as a one-element array at the top
        // level; a single error is an object. Both carry `error.message`.
        let object = match &value {
            Value::Array(items) => items.first()?,
            other => other,
        };
        object
            .get("error")?
            .get("message")?
            .as_str()
            .map(str::to_owned)
    }

    fn refused_fields(&self) -> &'static [(&'static str, &'static str)] {
        REFUSED_FIELDS
    }

    fn ignored_fields(&self) -> &'static [&'static str] {
        IGNORED_FIELDS
    }

    fn cache_disposition(&self) -> Option<CacheDisposition> {
        Some(CacheDisposition::Stripped(
            "Gemini's caching is either implicit (an automatic prefix cache the request does \
             not address) or the explicit `cachedContent` resource this codec already refuses \
             on decode — neither is a per-request marker a translated request can set, so a \
             harness's cache_control is never encoded onto this wire",
        ))
    }

    fn effort_disposition(&self) -> Option<EffortDisposition> {
        Some(EffortDisposition::Carried {
            field: "generationConfig.thinkingConfig.thinkingBudget",
            note: THINKING_BUDGET_NOTE,
        })
    }
}

/// The model as a path segment: Gemini's own full resource name is
/// `models/<id>`, and a harness pointed at a Gemini provider may send either
/// spelling. The prefix is stripped rather than doubled — `/models/models/x`
/// is a `404` nobody could diagnose.
fn model_segment(model: &str) -> &str {
    model.strip_prefix("models/").unwrap_or(model)
}

/// Every `(tool-use id, tool name)` in a request, in order.
fn tool_call_names(request: &Request) -> Vec<(&str, &str)> {
    request
        .messages
        .iter()
        .flat_map(|message| &message.blocks)
        .filter_map(|block| match block {
            Block::ToolUse { id, name, .. } => Some((id.as_str(), name.as_str())),
            _ => None,
        })
        .collect()
}

// --- requests -----------------------------------------------------------------

pub(super) fn decode_request(body: &[u8]) -> Result<Request, Unsupported> {
    let value: Value = serde_json::from_slice(body)
        .map_err(|_| Unsupported::new("body", "the request body is not a JSON document"))?;
    let mut top = Fields::of(value, "")?;

    // The model is in the path on this wire, so a body that names one is
    // read if it does and left empty if it does not. `serve` re-addresses a
    // translated request from `Request::model`, and every supported pair
    // decodes some *other* protocol, so this only ever matters to a round
    // trip through this codec's own two halves.
    let model = top.take_string("model")?.unwrap_or_default();

    top.refuse_if_present("safetySettings", reason("safetySettings"))?;
    top.refuse_if_present("cachedContent", reason("cachedContent"))?;

    let system = match top.take_object("systemInstruction")? {
        Some(instruction) => Some(content_text(instruction)?),
        None => None,
    };

    let mut messages = Vec::new();
    for (index, item) in top
        .take_array("contents")?
        .unwrap_or_default()
        .into_iter()
        .enumerate()
    {
        let path = element("contents", index);
        let mut content = Fields::of(item, path)?;
        let role = content.require_string("role")?;
        let role = match role.as_str() {
            "user" => Role::User,
            "model" => Role::Assistant,
            other => {
                return Err(Unsupported::new(
                    content.at("role"),
                    format!("a Gemini content role is `user` or `model`, not `{other}`"),
                ));
            }
        };
        let blocks = decode_parts(&mut content, role)?;
        content.finish()?;
        messages.push(Message { role, blocks });
    }

    let mut tools = Vec::new();
    for (index, item) in top
        .take_array("tools")?
        .unwrap_or_default()
        .into_iter()
        .enumerate()
    {
        let mut entry = Fields::of(item, element("tools", index))?;
        entry.refuse_if_present("codeExecution", reason("codeExecution"))?;
        entry.refuse_if_present("googleSearch", reason("googleSearch"))?;
        entry.refuse_if_present("googleSearchRetrieval", reason("googleSearchRetrieval"))?;
        let declarations = entry
            .take_array("functionDeclarations")?
            .unwrap_or_default();
        let path = entry.at("functionDeclarations");
        for (position, declaration) in declarations.into_iter().enumerate() {
            let mut declaration = Fields::of(declaration, element(&path, position))?;
            let name = declaration.require_string("name")?;
            let description = declaration.take_string("description")?;
            let input_schema = match declaration.take("parameters") {
                Some(schema @ Value::Object(_)) => schema,
                None | Some(Value::Null) => json!({"type": "object", "properties": {}}),
                Some(other) => {
                    return Err(Unsupported::new(
                        declaration.at("parameters"),
                        format!(
                            "a function declaration's parameters must be a JSON object, not {}",
                            json_kind(&other)
                        ),
                    ));
                }
            };
            declaration.finish()?;
            tools.push(ToolDefinition {
                name,
                description,
                input_schema,
            });
        }
        entry.finish()?;
    }

    let tool_choice = match top.take_object("toolConfig")? {
        None => None,
        Some(mut config) => {
            let choice = match config.take_object("functionCallingConfig")? {
                None => None,
                Some(mut calling) => {
                    let mode = calling.take_string("mode")?;
                    let allowed = calling.take_array("allowedFunctionNames")?;
                    calling.finish()?;
                    let allowed_one = match allowed.as_deref() {
                        Some([Value::String(name)]) => Some(name.clone()),
                        Some([]) | None => None,
                        Some(_) => {
                            return Err(Unsupported::new(
                                "toolConfig.functionCallingConfig.allowedFunctionNames",
                                "the canonical form can force one named tool or none, not a \
                                 subset of several",
                            ));
                        }
                    };
                    match (mode.as_deref(), allowed_one) {
                        (Some("AUTO") | None, None) => Some(ToolChoice::Auto),
                        (Some("ANY"), None) => Some(ToolChoice::Any),
                        (Some("ANY"), Some(name)) => Some(ToolChoice::Tool(name)),
                        (Some("NONE"), None) => Some(ToolChoice::None),
                        (Some(other), _) => {
                            return Err(Unsupported::new(
                                "toolConfig.functionCallingConfig.mode",
                                format!(
                                    "the function-calling mode `{other}` is not one this \
                                         codec knows"
                                ),
                            ));
                        }
                        (None, Some(name)) => Some(ToolChoice::Tool(name)),
                    }
                }
            };
            config.finish()?;
            choice
        }
    };

    let (max_tokens, temperature, top_p, stop) = match top.take_object("generationConfig")? {
        None => (None, None, None, Vec::new()),
        Some(mut generation) => {
            for field in [
                "responseMimeType",
                "responseSchema",
                "responseJsonSchema",
                "thinkingConfig",
                "topK",
                "seed",
                "presencePenalty",
                "frequencyPenalty",
                "responseLogprobs",
                "logprobs",
                "responseModalities",
                "speechConfig",
            ] {
                let named = format!("generationConfig.{field}");
                generation.refuse_if_present(field, reason(&named))?;
            }
            if let Some(count) = generation.take_u64("candidateCount")?
                && count != 1
            {
                return Err(Unsupported::new(
                    generation.at("candidateCount"),
                    reason("generationConfig.candidateCount"),
                ));
            }
            let max_tokens = generation.take_u64("maxOutputTokens")?;
            let temperature = generation.take_f64("temperature")?;
            let top_p = generation.take_f64("topP")?;
            let path = generation.at("stopSequences");
            let stop = generation
                .take_array("stopSequences")?
                .unwrap_or_default()
                .into_iter()
                .enumerate()
                .map(|(index, item)| match item {
                    Value::String(text) => Ok(text),
                    other => Err(Unsupported::new(
                        element(&path, index),
                        format!(
                            "a stop sequence must be a string, not {}",
                            json_kind(&other)
                        ),
                    )),
                })
                .collect::<Result<Vec<_>, _>>()?;
            generation.finish()?;
            (max_tokens, temperature, top_p, stop)
        }
    };

    top.finish()?;

    Ok(Request {
        model,
        max_tokens,
        system,
        messages,
        tools,
        tool_choice,
        // Gemini has no parameter for it, so nothing was asked — which is
        // different from asking for parallel calls, and both are `None`
        // here for the same reason the other codecs use `None`.
        parallel_tool_calls: None,
        temperature,
        top_p,
        stop,
        // A Gemini client asks for a stream by posting to
        // `:streamGenerateContent`, not by a body field. A decoder that
        // only sees the body cannot know, and `place` has already used the
        // target: `super::serve` re-reads the target, so this is the
        // honest answer from the body alone.
        stream: false,
        user: None,
        // Gemini's own wire has no cache-hint field to decode off a
        // request, and no installed harness speaks it at the ingress
        // anyway (`NO_GEMINI_HARNESS`), so this is never asked for from
        // this side.
        cache_requested: false,
        // `generationConfig.thinkingConfig` is refused above by name
        // (REFUSED_FIELDS), and no installed harness speaks this wire at the
        // ingress anyway (`NO_GEMINI_HARNESS`).
        effort: None,
    })
}

/// A `{parts: [...]}` object that must be text — `systemInstruction`.
fn content_text(mut content: Fields) -> Result<String, Unsupported> {
    content.ignore("role");
    let path = content.at("parts");
    let mut texts = Vec::new();
    for (index, part) in content
        .take_array("parts")?
        .unwrap_or_default()
        .into_iter()
        .enumerate()
    {
        let mut part = Fields::of(part, element(&path, index))?;
        match part.take("text") {
            Some(Value::String(text)) => texts.push(text),
            None | Some(Value::Null) => {
                return Err(Unsupported::new(
                    part.at("text"),
                    "a system instruction carries text and nothing else",
                ));
            }
            Some(other) => {
                return Err(Unsupported::new(
                    part.at("text"),
                    format!("a text part must be a string, not {}", json_kind(&other)),
                ));
            }
        }
        part.finish()?;
    }
    content.finish()?;
    Ok(texts.join("\n\n"))
}

/// The content blocks of one `contents[]` entry.
fn decode_parts(content: &mut Fields, role: Role) -> Result<Vec<Block>, Unsupported> {
    let path = content.at("parts");
    let mut blocks = Vec::new();
    for (index, part) in content
        .take_array("parts")?
        .unwrap_or_default()
        .into_iter()
        .enumerate()
    {
        let mut part = Fields::of(part, element(&path, index))?;
        let before = blocks.len();
        part.refuse_if_present("fileData", reason("fileData"))?;
        part.refuse_if_present("thought", reason("thought"))?;
        part.refuse_if_present("executableCode", reason("executableCode"))?;
        part.refuse_if_present("codeExecutionResult", reason("executableCode"))?;
        if let Some(text) = part.take_string("text")? {
            blocks.push(Block::Text(text));
        }
        if let Some(mut call) = part.take_object("functionCall")? {
            let name = call.require_string("name")?;
            let input = match call.take("args") {
                Some(args @ Value::Object(_)) => args,
                None | Some(Value::Null) => json!({}),
                Some(other) => {
                    return Err(Unsupported::new(
                        call.at("args"),
                        format!(
                            "a function call's arguments must be an object, not {}",
                            json_kind(&other)
                        ),
                    ));
                }
            };
            // Gemini issues no id; the one minted here is what the harness
            // will send back, and it names the call it belongs to. See the
            // module doc's second decision.
            let id = minted_id(blocks.len(), &name);
            call.ignore("id");
            call.finish()?;
            blocks.push(Block::ToolUse { id, name, input });
        }
        if let Some(mut response) = part.take_object("functionResponse")? {
            let name = response.require_string("name")?;
            let payload = response.take("response").unwrap_or(Value::Null);
            response.ignore("id");
            response.finish()?;
            let (content, is_error) = decode_function_response(&payload);
            blocks.push(Block::ToolResult {
                // The id a decoded result carries is the one this codec
                // would have minted for the call of that name — the pair of
                // halves round-trips, and nothing else reads it.
                tool_use_id: minted_id(blocks.len(), &name),
                content,
                is_error,
            });
        }
        if let Some(mut inline) = part.take_object("inlineData")? {
            let media_type = inline.require_string("mimeType")?;
            let data = inline.require_string("data")?;
            inline.finish()?;
            blocks.push(Block::Image(ImageSource::Base64 { media_type, data }));
        }
        let path = part.path().to_owned();
        // Whatever nobody read, refused by name — and a part that read as
        // NOTHING refused by its own path. An empty or wholly unrecognised
        // part would otherwise be the one thing this codec drops in silence,
        // which is the rule the whole module is built on.
        part.finish()?;
        if blocks.len() == before {
            return Err(Unsupported::new(
                path,
                "a Gemini part must carry text, functionCall, functionResponse or inlineData; \
                 this one carried none of them",
            ));
        }
    }
    // A model turn with no parts at all is a shape the wire allows and the
    // canonical form carries as an empty assistant message, exactly as
    // `openai_chat` carries an assistant message with null content.
    let _ = role;
    Ok(blocks)
}

/// The id this codec mints for a function call — see the module doc.
fn minted_id(index: usize, name: &str) -> String {
    format!("gemini-call-{index}-{name}")
}

/// A `functionResponse.response` as the canonical form's text plus its error
/// flag.
///
/// Gemini's `response` is an arbitrary object. This codec writes
/// `{"output": <text>}` for a success and `{"error": <text>}` for a failure,
/// which is Google's own documented convention, and reads both back. Any
/// other object is carried as its JSON text rather than refused: a tool
/// result is content the model reads, not a field the codec has to
/// understand.
fn decode_function_response(payload: &Value) -> (String, bool) {
    match payload {
        Value::Object(map) => match (map.get("output"), map.get("error")) {
            (_, Some(error)) => (text_of(error), true),
            (Some(output), None) => (text_of(output), false),
            (None, None) => (payload.to_string(), false),
        },
        other => (text_of(other), false),
    }
}

fn text_of(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        other => other.to_string(),
    }
}

pub(super) fn encode_request(request: &Request) -> Vec<u8> {
    let names = tool_call_names(request);
    let mut contents = Vec::new();
    for message in &request.messages {
        let role = match message.role {
            Role::User => "user",
            Role::Assistant => "model",
        };
        let mut parts = Vec::new();
        for block in &message.blocks {
            match block {
                Block::Text(text) => parts.push(json!({"text": text})),
                Block::Image(ImageSource::Base64 { media_type, data }) => {
                    parts.push(json!({"inlineData": {"mimeType": media_type, "data": data}}));
                }
                // Refused before this point by `refuse_unencodable`; carried
                // as its URL rather than dropped, so that a future caller
                // reaching here without the refusal still loses nothing.
                Block::Image(ImageSource::Url(url)) => parts.push(json!({"text": url})),
                Block::ToolUse { name, input, .. } => {
                    parts.push(json!({"functionCall": {"name": name, "args": input}}));
                }
                Block::ToolResult {
                    tool_use_id,
                    content,
                    is_error,
                } => {
                    // Resolved through the call's own block — see the module
                    // doc. `refuse_unencodable` has already refused an id
                    // with no such block, so the fallback below is
                    // unreachable from `serve`; it names the id rather than
                    // inventing a tool name for a caller that skipped it.
                    let name = names
                        .iter()
                        .find(|(id, _)| *id == tool_use_id)
                        .map(|(_, name)| *name)
                        .unwrap_or(tool_use_id.as_str());
                    let payload = if *is_error {
                        json!({"error": content})
                    } else {
                        json!({"output": content})
                    };
                    parts.push(json!({
                        "functionResponse": {"name": name, "response": payload},
                    }));
                }
                Block::Thinking { .. } | Block::RedactedThinking { .. } => {
                    unreachable!("refused by `refuse_unencodable` before this point")
                }
            }
        }
        contents.push(json!({"role": role, "parts": Value::Array(parts)}));
    }

    let mut document = Map::new();
    document.insert("contents".to_owned(), Value::Array(contents));
    if let Some(system) = &request.system {
        document.insert(
            "systemInstruction".to_owned(),
            json!({"parts": [{"text": system}]}),
        );
    }
    if !request.tools.is_empty() {
        let declarations: Vec<Value> = request
            .tools
            .iter()
            .map(|tool| {
                let mut declaration = Map::new();
                declaration.insert("name".to_owned(), json!(tool.name));
                if let Some(description) = &tool.description {
                    declaration.insert("description".to_owned(), json!(description));
                }
                // Carried as given. Gemini's schema dialect is a subset of
                // JSON Schema, so a tool declaring a keyword it rejects is
                // answered `400` by Google and that error reaches the
                // harness in its own shape — rewriting the schema here
                // would change what the harness declared.
                declaration.insert("parameters".to_owned(), tool.input_schema.clone());
                Value::Object(declaration)
            })
            .collect();
        document.insert(
            "tools".to_owned(),
            json!([{"functionDeclarations": Value::Array(declarations)}]),
        );
    }
    if let Some(choice) = &request.tool_choice {
        document.insert(
            "toolConfig".to_owned(),
            match choice {
                ToolChoice::Auto => json!({"functionCallingConfig": {"mode": "AUTO"}}),
                ToolChoice::Any => json!({"functionCallingConfig": {"mode": "ANY"}}),
                ToolChoice::None => json!({"functionCallingConfig": {"mode": "NONE"}}),
                // ANY plus one allowed name is how this wire says "call
                // exactly this tool"; there is no `mode` that means it.
                ToolChoice::Tool(name) => json!({
                    "functionCallingConfig": {"mode": "ANY", "allowedFunctionNames": [name]},
                }),
            },
        );
    }
    let mut generation = Map::new();
    if let Some(max_tokens) = request.max_tokens {
        generation.insert("maxOutputTokens".to_owned(), json!(max_tokens));
    }
    if let Some(temperature) = request.temperature {
        generation.insert("temperature".to_owned(), json!(temperature));
    }
    if let Some(top_p) = request.top_p {
        generation.insert("topP".to_owned(), json!(top_p));
    }
    if !request.stop.is_empty() {
        generation.insert("stopSequences".to_owned(), json!(request.stop));
    }
    // Carried (GH-EFFORT-CARRY), never invented: only when the harness asked
    // for thinking at all, and always clamped down to
    // `GEMINI_THINKING_BUDGET_MAX` — never raised past it.
    if let Some(effort) = &request.effort {
        generation.insert(
            "thinkingConfig".to_owned(),
            json!({"thinkingBudget": thinking_budget(effort)}),
        );
    }
    if !generation.is_empty() {
        document.insert("generationConfig".to_owned(), Value::Object(generation));
    }
    Value::Object(document).to_string().into_bytes()
}

// --- responses ----------------------------------------------------------------

pub(super) fn decode_response(body: &[u8]) -> Result<Response, Unsupported> {
    let value: Value = serde_json::from_slice(body)
        .map_err(|_| Unsupported::new("body", "the response body is not a JSON document"))?;
    let mut top = Fields::of(value, "")?;
    // `responseId` on current versions and nothing on older ones. Empty
    // rather than minted: an id nobody issued is indistinguishable from one
    // somebody did, and the harness only echoes it.
    let id = top.take_string("responseId")?.unwrap_or_default();
    let model = top.take_string("modelVersion")?.unwrap_or_default();
    top.ignore("promptFeedback");
    let usage = match top.take_object("usageMetadata")? {
        Some(usage) => decode_usage(usage)?,
        None => Usage::default(),
    };
    let mut candidates = top.take_array("candidates")?.unwrap_or_default();
    if candidates.len() != 1 {
        return Err(Unsupported::new(
            "candidates",
            format!(
                "exactly one candidate is translated; the provider returned {}",
                candidates.len()
            ),
        ));
    }
    let mut candidate = Fields::of(candidates.remove(0), "candidates[0]")?;
    for ignored in [
        "index",
        "safetyRatings",
        "citationMetadata",
        "groundingMetadata",
        "avgLogprobs",
        "logprobsResult",
        "tokenCount",
        "finishMessage",
        "urlContextMetadata",
    ] {
        candidate.ignore(ignored);
    }
    let finish = candidate.require_string("finishReason")?;
    let blocks = match candidate.take_object("content")? {
        Some(mut content) => {
            let blocks = decode_parts(&mut content, Role::Assistant)?;
            content.ignore("role");
            content.finish()?;
            blocks
        }
        None => Vec::new(),
    };
    candidate.finish()?;
    top.finish()?;
    let stop_reason = decode_finish_reason(&finish, "candidates[0].finishReason", &blocks)?;
    Ok(Response {
        id,
        model,
        blocks,
        // Gemini names no matched stop sequence, and the canonical form's
        // `None` is "the provider did not say" rather than "none matched".
        stop_sequence: None,
        stop_reason,
        usage,
    })
}

/// Gemini's `finishReason`, plus the content, as a canonical stop reason.
///
/// The content is an argument because of the module doc's third decision: a
/// candidate made entirely of function calls still finishes `STOP`, and a
/// harness told `end_turn` there stops instead of running the tool.
fn decode_finish_reason(
    finish: &str,
    path: &str,
    blocks: &[Block],
) -> Result<StopReason, Unsupported> {
    let calls_a_tool = blocks
        .iter()
        .any(|block| matches!(block, Block::ToolUse { .. }));
    Ok(match finish {
        "STOP" if calls_a_tool => StopReason::ToolUse,
        "STOP" => StopReason::EndTurn,
        "MAX_TOKENS" => StopReason::MaxTokens,
        "SAFETY" | "RECITATION" | "LANGUAGE" | "BLOCKLIST" | "PROHIBITED_CONTENT" | "SPII"
        | "IMAGE_SAFETY" => StopReason::Refusal,
        // Each of these says the answer went wrong in a way the canonical
        // vocabulary has no word for. `end_turn` would tell the harness a
        // broken answer finished normally, so they are refused by name and
        // the harness is told the provider's answer could not be carried.
        other => {
            return Err(Unsupported::new(
                path.to_owned(),
                format!(
                    "the finish reason `{other}` is not one this codec can carry: the \
                         canonical form has no stop reason that would not misdescribe it"
                ),
            ));
        }
    })
}

fn finish_reason_json(reason: StopReason) -> Value {
    json!(match reason {
        // Gemini says `STOP` for both, and for a candidate of function
        // calls too — the decoder above is where that ambiguity is resolved
        // on the way back.
        StopReason::EndTurn | StopReason::StopSequence | StopReason::ToolUse => "STOP",
        StopReason::MaxTokens => "MAX_TOKENS",
        StopReason::Refusal => "SAFETY",
    })
}

fn decode_usage(mut usage: Fields) -> Result<Usage, Unsupported> {
    let prompt = usage.take_u64("promptTokenCount")?.unwrap_or(0);
    let candidates = usage.take_u64("candidatesTokenCount")?.unwrap_or(0);
    // Reasoning tokens are reported apart from `candidatesTokenCount` and
    // are output the provider generated and charged for; the canonical
    // `output` is what was produced, so they are summed rather than
    // ignored. `cachedContentTokenCount` is a SUBSET of the prompt count,
    // exactly as OpenAI's `cached_tokens` is a subset of `prompt_tokens`.
    let thoughts = usage.take_u64("thoughtsTokenCount")?;
    let cached = usage.take_u64("cachedContentTokenCount")?;
    for ignored in [
        "totalTokenCount",
        "promptTokensDetails",
        "candidatesTokensDetails",
        "cacheTokensDetails",
        "toolUsePromptTokenCount",
        "toolUsePromptTokensDetails",
        "thoughtsTokensDetails",
    ] {
        usage.ignore(ignored);
    }
    usage.finish()?;
    Ok(Usage {
        input: prompt.saturating_sub(cached.unwrap_or(0)),
        output: candidates.saturating_add(thoughts.unwrap_or(0)),
        cached,
        reasoning: thoughts,
    })
}

fn usage_json(usage: &Usage) -> Value {
    let prompt = usage.input + usage.cached.unwrap_or(0);
    let mut entry = Map::new();
    entry.insert("promptTokenCount".to_owned(), json!(prompt));
    entry.insert("candidatesTokenCount".to_owned(), json!(usage.output));
    entry.insert("totalTokenCount".to_owned(), json!(prompt + usage.output));
    if let Some(cached) = usage.cached {
        entry.insert("cachedContentTokenCount".to_owned(), json!(cached));
    }
    Value::Object(entry)
}

/// The `parts` of one assistant answer.
fn response_parts(blocks: &[Block]) -> Vec<Value> {
    blocks
        .iter()
        .filter_map(|block| match block {
            Block::Text(text) => Some(json!({"text": text})),
            Block::ToolUse { name, input, .. } => {
                Some(json!({"functionCall": {"name": name, "args": input}}))
            }
            // `decode_response` on no codec produces these in an answer.
            Block::Image(_)
            | Block::ToolResult { .. }
            | Block::Thinking { .. }
            | Block::RedactedThinking { .. } => None,
        })
        .collect()
}

pub(super) fn encode_response(response: &Response) -> Vec<u8> {
    let mut document = Map::new();
    document.insert(
        "candidates".to_owned(),
        json!([{
            "content": {"role": "model", "parts": Value::Array(response_parts(&response.blocks))},
            "finishReason": finish_reason_json(response.stop_reason),
            "index": 0,
        }]),
    );
    document.insert("usageMetadata".to_owned(), usage_json(&response.usage));
    document.insert("modelVersion".to_owned(), json!(response.model));
    if !response.id.is_empty() {
        document.insert("responseId".to_owned(), json!(response.id));
    }
    Value::Object(document).to_string().into_bytes()
}

// --- streams ------------------------------------------------------------------

/// Which canonical block is open.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Open {
    Text,
    Tool,
}

/// Turns `streamGenerateContent` chunks into the canonical event order.
///
/// See the module doc's fifth decision and its table for what is emitted at
/// which chunk, and for why `finishReason` is the terminator.
#[derive(Default)]
struct ChunkDecoder {
    started: bool,
    open: Option<(Open, usize)>,
    next_index: usize,
    minted: usize,
    finish: Option<StopReason>,
    /// Whether any chunk's candidate carried a function call — the third
    /// decision, applied across the whole stream rather than one chunk.
    calls_a_tool: bool,
    /// The raw `finishReason` text, held so the stop reason can be decided
    /// once the whole stream's content is known.
    finish_text: Option<String>,
    usage: Usage,
    done: bool,
}

impl ChunkDecoder {
    fn close_open(&mut self, events: &mut Vec<StreamEvent>) {
        if let Some((_, index)) = self.open.take() {
            events.push(StreamEvent::BlockStop { index });
        }
    }

    fn open_block(&mut self, block: BlockStart, events: &mut Vec<StreamEvent>) -> usize {
        self.close_open(events);
        let index = self.next_index;
        self.next_index += 1;
        events.push(StreamEvent::BlockStart { index, block });
        index
    }
}

impl StreamDecoder for ChunkDecoder {
    fn feed(&mut self, event: &SseEvent) -> Result<Vec<StreamEvent>, Unsupported> {
        if self.done {
            return Ok(Vec::new());
        }
        let data = event.data.trim();
        if data.is_empty() {
            return Ok(Vec::new());
        }
        let value: Value = serde_json::from_str(data)
            .map_err(|_| Unsupported::new("chunk", "a stream chunk was not a JSON document"))?;
        let mut top = Fields::of(value, "")?;
        let mut events = Vec::new();
        let id = top.take_string("responseId")?.unwrap_or_default();
        let model = top.take_string("modelVersion")?.unwrap_or_default();
        top.ignore("promptFeedback");
        if let Some(usage) = top.take_object("usageMetadata")? {
            self.usage = decode_usage(usage)?;
        }
        if !self.started {
            self.started = true;
            events.push(StreamEvent::MessageStart {
                id,
                model,
                usage: Usage {
                    input: self.usage.input,
                    output: 0,
                    cached: self.usage.cached,
                    reasoning: None,
                },
            });
        }
        for (index, candidate) in top
            .take_array("candidates")?
            .unwrap_or_default()
            .into_iter()
            .enumerate()
        {
            let path = element("candidates", index);
            let mut candidate = Fields::of(candidate, path)?;
            if candidate.take_u64("index")?.unwrap_or(0) != 0 {
                return Err(Unsupported::new(
                    candidate.at("index"),
                    reason("generationConfig.candidateCount"),
                ));
            }
            for ignored in [
                "safetyRatings",
                "citationMetadata",
                "groundingMetadata",
                "avgLogprobs",
                "logprobsResult",
                "tokenCount",
                "finishMessage",
                "urlContextMetadata",
            ] {
                candidate.ignore(ignored);
            }
            if let Some(finish) = candidate.take_string("finishReason")? {
                self.finish_text = Some(finish);
            }
            let Some(mut content) = candidate.take_object("content")? else {
                candidate.finish()?;
                continue;
            };
            candidate.finish()?;
            let blocks = decode_parts(&mut content, Role::Assistant)?;
            content.ignore("role");
            content.finish()?;
            for block in blocks {
                match block {
                    Block::Text(text) => {
                        if text.is_empty() {
                            continue;
                        }
                        let index = match self.open {
                            Some((Open::Text, index)) => index,
                            _ => {
                                let index = self.open_block(BlockStart::Text, &mut events);
                                self.open = Some((Open::Text, index));
                                index
                            }
                        };
                        events.push(StreamEvent::BlockDelta {
                            index,
                            delta: Delta::Text(text),
                        });
                    }
                    Block::ToolUse { name, input, .. } => {
                        self.calls_a_tool = true;
                        // The block index this codec mints ids from is the
                        // stream's own count, not the chunk's, so two calls
                        // in two chunks never share an id.
                        let id = minted_id(self.minted, &name);
                        self.minted += 1;
                        let index = self.open_block(
                            BlockStart::ToolUse {
                                id,
                                name: name.clone(),
                            },
                            &mut events,
                        );
                        self.open = Some((Open::Tool, index));
                        // Gemini sends a call's arguments whole; there is
                        // nothing to fragment and nothing to wait for.
                        events.push(StreamEvent::BlockDelta {
                            index,
                            delta: Delta::InputJson(input.to_string()),
                        });
                    }
                    // A streamed answer carries none of these, and
                    // `decode_parts` only produces the first two from a
                    // request's own shapes; Gemini has no thinking-block
                    // equivalent to decode into the other two at all.
                    Block::Image(_)
                    | Block::ToolResult { .. }
                    | Block::Thinking { .. }
                    | Block::RedactedThinking { .. } => {}
                }
            }
        }
        top.finish()?;
        Ok(events)
    }

    fn finish(&mut self) -> Result<Vec<StreamEvent>, Unsupported> {
        if self.done {
            return Ok(Vec::new());
        }
        if !self.started {
            return Err(Unsupported::new(
                "chunk",
                "the provider's stream ended before it sent a single chunk",
            ));
        }
        // `streamGenerateContent` has no `[DONE]`, so `finishReason` is the
        // terminator — see the module doc's fifth decision. Without one the
        // stream was cut, and completing it here would hand the harness a
        // partial answer wearing a stop reason nobody sent.
        let Some(finish) = self.finish_text.clone() else {
            return Err(Unsupported::new(
                "candidates[0].finishReason",
                "the provider's stream ended without a finish reason, so the message it \
                 delivered is truncated and not finished",
            ));
        };
        let calls = if self.calls_a_tool {
            vec![Block::ToolUse {
                id: String::new(),
                name: String::new(),
                input: Value::Null,
            }]
        } else {
            Vec::new()
        };
        self.finish = Some(decode_finish_reason(
            &finish,
            "candidates[0].finishReason",
            &calls,
        )?);
        self.done = true;
        let mut events = Vec::new();
        self.close_open(&mut events);
        events.push(StreamEvent::MessageDelta {
            stop_reason: self
                .finish
                .expect("set immediately above, or this line is not reached"),
            stop_sequence: None,
            usage: self.usage,
        });
        events.push(StreamEvent::MessageStop);
        Ok(events)
    }

    fn is_done(&self) -> bool {
        self.done
    }
}

/// Turns the canonical event order into `streamGenerateContent` chunks.
///
/// Reachable only when a harness speaks Gemini at the ingress, which no
/// installed harness does — every `gemini-generate-content -> …` row is
/// refused (T3b). It is written and tested anyway, because a codec whose
/// halves do not agree is a codec that cannot be trusted in the direction
/// that *is* used.
///
/// One property differs from the other encoders: Gemini's wire has **no
/// partial-arguments event**, so a tool call's `InputJson` fragments are
/// held until its block stops and the whole `functionCall` is written then.
/// That is a property of the wire, not a buffering choice — there is no
/// shape in which a fragment could be sent.
#[derive(Default)]
struct ChunkEncoder {
    id: String,
    model: String,
    open_tool: Option<(String, String)>,
    pending_args: String,
}

impl ChunkEncoder {
    fn chunk(&self, parts: Vec<Value>, finish: Option<Value>, usage: Option<Value>) -> Vec<u8> {
        let mut candidate = Map::new();
        candidate.insert(
            "content".to_owned(),
            json!({"role": "model", "parts": Value::Array(parts)}),
        );
        candidate.insert("index".to_owned(), json!(0));
        if let Some(finish) = finish {
            candidate.insert("finishReason".to_owned(), finish);
        }
        let mut document = Map::new();
        document.insert(
            "candidates".to_owned(),
            Value::Array(vec![Value::Object(candidate)]),
        );
        document.insert("modelVersion".to_owned(), json!(self.model));
        if !self.id.is_empty() {
            document.insert("responseId".to_owned(), json!(self.id));
        }
        if let Some(usage) = usage {
            document.insert("usageMetadata".to_owned(), usage);
        }
        stream::encode(None, &Value::Object(document).to_string())
    }
}

impl StreamEncoder for ChunkEncoder {
    fn encode(&mut self, event: &StreamEvent) -> Vec<u8> {
        match event {
            StreamEvent::MessageStart { id, model, .. } => {
                self.id = id.clone();
                self.model = model.clone();
                // Gemini's wire has no start event: the first chunk a client
                // sees is the first one carrying content.
                Vec::new()
            }
            StreamEvent::BlockStart {
                block: BlockStart::ToolUse { id, name },
                ..
            } => {
                self.open_tool = Some((id.clone(), name.clone()));
                self.pending_args.clear();
                Vec::new()
            }
            StreamEvent::BlockStart {
                block: BlockStart::Text,
                ..
            } => Vec::new(),
            StreamEvent::BlockDelta {
                delta: Delta::Text(text),
                ..
            } => self.chunk(vec![json!({"text": text})], None, None),
            StreamEvent::BlockDelta {
                delta: Delta::InputJson(partial),
                ..
            } => {
                self.pending_args.push_str(partial);
                Vec::new()
            }
            StreamEvent::BlockStop { .. } => match self.open_tool.take() {
                None => Vec::new(),
                Some((_, name)) => {
                    let args = parse_tool_input(&self.pending_args).unwrap_or(json!({}));
                    self.pending_args.clear();
                    self.chunk(
                        vec![json!({"functionCall": {"name": name, "args": args}})],
                        None,
                        None,
                    )
                }
            },
            StreamEvent::MessageDelta {
                stop_reason, usage, ..
            } => self.chunk(
                Vec::new(),
                Some(finish_reason_json(*stop_reason)),
                Some(usage_json(usage)),
            ),
            // The socket closing is this wire's end of stream.
            StreamEvent::MessageStop => Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests;
