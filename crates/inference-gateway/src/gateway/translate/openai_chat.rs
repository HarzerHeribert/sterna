//! The OpenAI Chat Completions codec: that wire's requests, responses and
//! stream chunks, into and out of [`super::canonical`].
//!
//! The same three categories as the Anthropic codec — carried, refused by
//! name, or ignored by name — and the same rule that an unknown key is a
//! refusal. Two decisions particular to this wire are worth reading before
//! the code:
//!
//! History: design-decisions.md, "Trims: gateway module docs", translate/openai_chat.rs module doc.
//!
//! **Tool-call ids are OpenAI's and stay OpenAI's.** A `call_…` id issued
//! by the provider becomes the Anthropic `tool_use.id` the harness sees, and
//! comes back as `tool_call_id` verbatim. There is no id table; the id *is*
//! the mapping.

use serde_json::{Map, Value, json};

use super::canonical::{
    Block, BlockStart, Delta, ImageSource, Message, Request, Response, Role, StopReason,
    StreamEvent, ToolChoice, ToolDefinition, Unsupported, Usage, json_kind, parse_tool_input,
};
use super::fields::{Fields, element};
use super::stream::{self, SseEvent};
use super::{CacheDisposition, Codec, EffortDisposition, StreamDecoder, StreamEncoder};

pub(super) const PROTOCOL: &str = "openai-chat";

/// The one target this codec translates, version segment stripped.
pub(super) const ENDPOINT: &str = "/chat/completions";

/// The first line of a `tool` message whose result was an error — see the
/// module doc.
pub const TOOL_ERROR_MARKER: &str = "[tool_result is_error=true]";

/// The request fields and shapes this codec refuses, with the reason each
/// refusal carries.
pub(super) const REFUSED_FIELDS: &[(&str, &str)] = &[
    (
        "n",
        "more than one completion per request has no Anthropic Messages equivalent",
    ),
    (
        "logprobs",
        "log probabilities have no Anthropic Messages equivalent",
    ),
    (
        "top_logprobs",
        "log probabilities have no Anthropic Messages equivalent",
    ),
    (
        "response_format",
        "a structured output format has no Anthropic Messages equivalent",
    ),
    (
        "seed",
        "deterministic sampling has no Anthropic Messages equivalent",
    ),
    (
        "presence_penalty",
        "presence penalties have no Anthropic Messages equivalent",
    ),
    (
        "frequency_penalty",
        "frequency penalties have no Anthropic Messages equivalent",
    ),
    (
        "logit_bias",
        "logit bias has no Anthropic Messages equivalent",
    ),
    (
        "reasoning_effort",
        "reasoning effort has no Anthropic Messages equivalent in this codec",
    ),
    (
        "audio",
        "audio content has no Anthropic Messages equivalent",
    ),
    (
        "refusal",
        "a refusal message part has no Anthropic Messages equivalent",
    ),
    (
        "strict",
        "strict schema adherence has no Anthropic Messages equivalent",
    ),
    (
        "function_call",
        "the legacy function-calling API is not translated; use tools",
    ),
    (
        "name",
        "a per-message participant name has no Anthropic Messages equivalent",
    ),
    (
        "thinking block",
        "an Anthropic thinking or redacted_thinking block is another provider's private \
         reasoning and has no OpenAI Chat equivalent",
    ),
];

/// [`CacheDisposition::Carried`]'s note for this codec's `prompt_cache_key`
/// (2018): the pair table's answer to *how* it is derived.
const PROMPT_CACHE_KEY_NOTE: &str = "set to the harness's own per-session identifier (Claude Code's \
     `metadata.user_id`, decoded into `Request::user` and already sent to the provider under \
     that name) so repeat requests in the same session route to the same cache; omitted when the \
     harness set no user id, and never derived from a credential or the gateway's own token";

/// [`EffortDisposition::Carried`]'s note for this codec's `reasoning_effort`
/// (GH-EFFORT-CARRY): the vocabulary is
/// `developers.openai.com/api/docs/guides/reasoning` (fetched 2026-09-02),
/// which documents `none`, `minimal`, `low`, `medium`, `high`, `xhigh` and
/// `max` as model-dependent supported values; this codec writes the harness's own word when it
/// stated one (`output_config.effort`, up to `max`), otherwise the word its budget maps to
/// [`super::canonical::level_for_budget`] derives from a token budget — and
/// never the wider or narrower words a specific model's own page might also
/// accept.
const EFFORT_NOTE: &str = "set to the harness's own `output_config.effort` word when it stated one (up to `max`), otherwise to the word `level_for_budget` maps its `thinking.budget_tokens` onto \
     (minimal/low/medium/high, never rounded up); omitted when the harness set no effort at all";

/// Response fields ignored by name: informational, never asked for.
pub(super) const IGNORED_FIELDS: &[&str] = &[
    "object",
    "created",
    "system_fingerprint",
    "service_tier",
    "usage.total_tokens",
    "usage.prompt_tokens_details.audio_tokens",
    "usage.completion_tokens_details (all but reasoning_tokens)",
    "choices[].index",
    "choices[].message.annotations",
    "stream_options.include_usage",
    "image_url.detail",
];

fn reason(field: &str) -> &'static str {
    REFUSED_FIELDS
        .iter()
        .find(|(name, _)| *name == field)
        .map(|(_, reason)| *reason)
        .expect("every refusal named in this file is listed in REFUSED_FIELDS")
}

pub(super) struct OpenAiChat;

impl Codec for OpenAiChat {
    fn protocol(&self) -> &'static str {
        PROTOCOL
    }

    fn endpoint(&self) -> &'static str {
        ENDPOINT
    }

    fn refuse_unencodable(&self, request: &Request) -> Result<(), Unsupported> {
        let carries_thinking = request.messages.iter().any(|message| {
            message.blocks.iter().any(|block| {
                matches!(
                    block,
                    Block::Thinking { .. } | Block::RedactedThinking { .. }
                )
            })
        });
        if carries_thinking {
            Err(Unsupported::new("thinking block", reason("thinking block")))
        } else {
            Ok(())
        }
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

    fn error_kind(&self, status: u16) -> &'static str {
        match status {
            400 | 413 => "invalid_request_error",
            401 => "authentication_error",
            403 => "permission_error",
            404 => "not_found_error",
            429 => "rate_limit_error",
            _ => "server_error",
        }
    }

    fn encode_error(&self, kind: &str, message: &str) -> Vec<u8> {
        json!({"error": {"message": message, "type": kind, "code": null}})
            .to_string()
            .into_bytes()
    }

    fn encode_stream_error(&self, kind: &str, message: &str) -> Vec<u8> {
        stream::encode(
            None,
            &json!({"error": {"message": message, "type": kind, "code": null}}).to_string(),
        )
    }

    fn decode_error(&self, body: &[u8]) -> Option<String> {
        let value: Value = serde_json::from_slice(body).ok()?;
        value
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
        Some(CacheDisposition::Carried {
            field: "prompt_cache_key",
            note: PROMPT_CACHE_KEY_NOTE,
        })
    }

    fn effort_disposition(&self) -> Option<EffortDisposition> {
        Some(EffortDisposition::Carried {
            field: "reasoning_effort",
            note: EFFORT_NOTE,
        })
    }
}

// --- requests -----------------------------------------------------------------

pub(super) fn decode_request(body: &[u8]) -> Result<Request, Unsupported> {
    let value: Value = serde_json::from_slice(body)
        .map_err(|_| Unsupported::new("body", "the request body is not a JSON document"))?;
    let mut top = Fields::of(value, "")?;

    let model = top.require_string("model")?;

    let mut system_parts: Vec<String> = Vec::new();
    let mut messages: Vec<Message> = Vec::new();
    // Tool messages answer the assistant turn before them and open the user
    // turn after them; on the Anthropic side they are the first blocks of
    // that user turn, so they wait here until the next message says which
    // turn they belong to.
    let mut pending_results: Vec<Block> = Vec::new();
    for (index, item) in top
        .take_array("messages")?
        .unwrap_or_default()
        .into_iter()
        .enumerate()
    {
        let path = element("messages", index);
        let mut message = Fields::of(item, path)?;
        let role = message.require_string("role")?;
        message.refuse_if_present("name", reason("name"))?;
        match role.as_str() {
            "system" | "developer" => {
                flush_results(&mut pending_results, &mut messages);
                system_parts.push(text_content(&mut message, "content")?);
                message.finish()?;
            }
            "user" => {
                let mut blocks = std::mem::take(&mut pending_results);
                blocks.extend(user_content(&mut message)?);
                message.finish()?;
                messages.push(Message {
                    role: Role::User,
                    blocks,
                });
            }
            "assistant" => {
                flush_results(&mut pending_results, &mut messages);
                let mut blocks = Vec::new();
                message.refuse_if_present("refusal", reason("refusal"))?;
                message.refuse_if_present("function_call", reason("function_call"))?;
                message.refuse_if_present("audio", reason("audio"))?;
                match message.take("content") {
                    None | Some(Value::Null) => {}
                    Some(Value::String(text)) => {
                        if !text.is_empty() {
                            blocks.push(Block::Text(text));
                        }
                    }
                    Some(Value::Array(parts)) => {
                        let content = message.at("content");
                        for (part_index, part) in parts.into_iter().enumerate() {
                            let mut part = Fields::of(part, element(&content, part_index))?;
                            match part.require_string("type")?.as_str() {
                                "text" => blocks.push(Block::Text(part.require_string("text")?)),
                                "refusal" => {
                                    return Err(Unsupported::new(
                                        part.at("type"),
                                        reason("refusal"),
                                    ));
                                }
                                other => {
                                    return Err(Unsupported::new(
                                        part.at("type"),
                                        format!(
                                            "an assistant content part must be text, not `{other}`"
                                        ),
                                    ));
                                }
                            }
                            part.finish()?;
                        }
                    }
                    Some(other) => {
                        return Err(Unsupported::new(
                            message.at("content"),
                            format!(
                                "assistant content must be a string, null or parts, not {}",
                                json_kind(&other)
                            ),
                        ));
                    }
                }
                for (call_index, call) in message
                    .take_array("tool_calls")?
                    .unwrap_or_default()
                    .into_iter()
                    .enumerate()
                {
                    let path = element(&message.at("tool_calls"), call_index);
                    let mut call = Fields::of(call, path)?;
                    let id = call.require_string("id")?;
                    if let Some(kind) = call.take_string("type")?
                        && kind != "function"
                    {
                        return Err(Unsupported::new(
                            call.at("type"),
                            format!("a tool call must be a function call, not `{kind}`"),
                        ));
                    }
                    let mut function = call.take_object("function")?.ok_or_else(|| {
                        Unsupported::new(call.at("function"), "a tool call needs a function")
                    })?;
                    let name = function.require_string("name")?;
                    let arguments = function.take_string("arguments")?.unwrap_or_default();
                    let input = parse_tool_input(&arguments)
                        .map_err(|why| Unsupported::new(function.at("arguments"), why))?;
                    function.finish()?;
                    call.finish()?;
                    blocks.push(Block::ToolUse { id, name, input });
                }
                message.finish()?;
                messages.push(Message {
                    role: Role::Assistant,
                    blocks,
                });
            }
            "tool" => {
                let tool_use_id = message.require_string("tool_call_id")?;
                let content = text_content(&mut message, "content")?;
                message.finish()?;
                let (content, is_error) = match content.strip_prefix(TOOL_ERROR_MARKER) {
                    Some(rest) => (rest.strip_prefix('\n').unwrap_or(rest).to_owned(), true),
                    None => (content, false),
                };
                pending_results.push(Block::ToolResult {
                    tool_use_id,
                    content,
                    is_error,
                });
            }
            "function" => {
                return Err(Unsupported::new(
                    message.at("role"),
                    reason("function_call"),
                ));
            }
            other => {
                return Err(Unsupported::new(
                    message.at("role"),
                    format!("the message role `{other}` is not one this codec carries"),
                ));
            }
        }
    }
    flush_results(&mut pending_results, &mut messages);
    let system = if system_parts.is_empty() {
        None
    } else {
        Some(system_parts.join("\n\n"))
    };

    let mut tools = Vec::new();
    for (index, item) in top
        .take_array("tools")?
        .unwrap_or_default()
        .into_iter()
        .enumerate()
    {
        let mut tool = Fields::of(item, element("tools", index))?;
        let kind = tool.require_string("type")?;
        if kind != "function" {
            return Err(Unsupported::new(
                tool.at("type"),
                format!("a tool must be a function, not `{kind}`"),
            ));
        }
        let mut function = tool
            .take_object("function")?
            .ok_or_else(|| Unsupported::new(tool.at("function"), "a tool needs a function"))?;
        function.refuse_if_present("strict", reason("strict"))?;
        let name = function.require_string("name")?;
        let description = function.take_string("description")?;
        let input_schema = match function.take("parameters") {
            Some(schema @ Value::Object(_)) => schema,
            None => json!({"type": "object", "properties": {}}),
            Some(other) => {
                return Err(Unsupported::new(
                    function.at("parameters"),
                    format!(
                        "a function's parameters must be a JSON object, not {}",
                        json_kind(&other)
                    ),
                ));
            }
        };
        function.finish()?;
        tool.finish()?;
        tools.push(ToolDefinition {
            name,
            description,
            input_schema,
        });
    }

    let tool_choice = match top.take("tool_choice") {
        None | Some(Value::Null) => None,
        Some(Value::String(choice)) => Some(match choice.as_str() {
            "none" => ToolChoice::None,
            "auto" => ToolChoice::Auto,
            "required" => ToolChoice::Any,
            other => {
                return Err(Unsupported::new(
                    "tool_choice",
                    format!("the tool choice `{other}` is not one this codec knows"),
                ));
            }
        }),
        Some(object @ Value::Object(_)) => {
            let mut choice = Fields::of(object, "tool_choice")?;
            let kind = choice.require_string("type")?;
            if kind != "function" {
                return Err(Unsupported::new(
                    choice.at("type"),
                    format!("a forced tool choice must name a function, not `{kind}`"),
                ));
            }
            let mut function = choice.take_object("function")?.ok_or_else(|| {
                Unsupported::new(
                    choice.at("function"),
                    "a forced tool choice needs a function",
                )
            })?;
            let name = function.require_string("name")?;
            function.finish()?;
            choice.finish()?;
            Some(ToolChoice::Tool(name))
        }
        Some(other) => {
            return Err(Unsupported::new(
                "tool_choice",
                format!(
                    "a tool choice must be a string or an object, not {}",
                    json_kind(&other)
                ),
            ));
        }
    };
    let parallel_tool_calls = top.take_bool("parallel_tool_calls")?;

    let max_tokens = match (
        top.take_u64("max_tokens")?,
        top.take_u64("max_completion_tokens")?,
    ) {
        (Some(a), Some(b)) if a != b => {
            return Err(Unsupported::new(
                "max_completion_tokens",
                "max_tokens and max_completion_tokens disagree",
            ));
        }
        (a, b) => a.or(b),
    };
    let temperature = top.take_f64("temperature")?;
    let top_p = top.take_f64("top_p")?;
    let stop = match top.take("stop") {
        None | Some(Value::Null) => Vec::new(),
        Some(Value::String(one)) => vec![one],
        Some(Value::Array(items)) => items
            .into_iter()
            .enumerate()
            .map(|(index, item)| match item {
                Value::String(text) => Ok(text),
                other => Err(Unsupported::new(
                    element("stop", index),
                    format!(
                        "a stop sequence must be a string, not {}",
                        json_kind(&other)
                    ),
                )),
            })
            .collect::<Result<Vec<_>, _>>()?,
        Some(other) => {
            return Err(Unsupported::new(
                "stop",
                format!(
                    "stop must be a string or an array of strings, not {}",
                    json_kind(&other)
                ),
            ));
        }
    };
    let stream = top.take_bool("stream")?.unwrap_or(false);
    if let Some(mut options) = top.take_object("stream_options")? {
        options.ignore("include_usage");
        options.finish()?;
    }
    let user = top.take_string("user")?;

    if let Some(n) = top.take_u64("n")?
        && n != 1
    {
        return Err(Unsupported::new("n", reason("n")));
    }
    if let Some(logprobs) = top.take_bool("logprobs")?
        && logprobs
    {
        return Err(Unsupported::new("logprobs", reason("logprobs")));
    }
    for field in [
        "top_logprobs",
        "response_format",
        "seed",
        "presence_penalty",
        "frequency_penalty",
        "logit_bias",
        "reasoning_effort",
        "audio",
        "function_call",
    ] {
        top.refuse_if_present(field, reason(field))?;
    }
    top.finish()?;

    Ok(Request {
        model,
        max_tokens,
        system,
        messages,
        tools,
        tool_choice,
        parallel_tool_calls,
        temperature,
        top_p,
        stop,
        stream,
        user,
        // OpenAI Chat's own wire has no explicit cache-hint field to
        // decode off a request — its caching is automatic and keyed only
        // by `prompt_cache_key`, which `finish()` above already refuses by
        // name like any other unlisted field when a client sets it.
        cache_requested: false,
        // `reasoning_effort` is refused above by name (REFUSED_FIELDS): a
        // harness speaking this wire natively is a different direction than
        // the one this package carries.
        effort: None,
    })
}

/// Tool results with no user turn after them still make a user turn.
fn flush_results(pending: &mut Vec<Block>, messages: &mut Vec<Message>) {
    if !pending.is_empty() {
        messages.push(Message {
            role: Role::User,
            blocks: std::mem::take(pending),
        });
    }
}

/// A `content` that must be text: a string, or text parts joined with
/// newlines.
fn text_content(message: &mut Fields, key: &str) -> Result<String, Unsupported> {
    match message.take(key) {
        Some(Value::String(text)) => Ok(text),
        Some(Value::Array(parts)) => {
            let path = message.at(key);
            let mut texts = Vec::with_capacity(parts.len());
            for (index, part) in parts.into_iter().enumerate() {
                let mut part = Fields::of(part, element(&path, index))?;
                let kind = part.require_string("type")?;
                if kind != "text" {
                    return Err(Unsupported::new(
                        part.at("type"),
                        format!("this content must be text, not `{kind}`"),
                    ));
                }
                texts.push(part.require_string("text")?);
                part.finish()?;
            }
            Ok(texts.join("\n"))
        }
        Some(other) => Err(Unsupported::new(
            message.at(key),
            format!(
                "content must be a string or text parts, not {}",
                json_kind(&other)
            ),
        )),
        None => Err(Unsupported::new(message.at(key), "content is required")),
    }
}

fn user_content(message: &mut Fields) -> Result<Vec<Block>, Unsupported> {
    match message.take("content") {
        Some(Value::String(text)) => Ok(vec![Block::Text(text)]),
        Some(Value::Array(parts)) => {
            let path = message.at("content");
            let mut blocks = Vec::with_capacity(parts.len());
            for (index, part) in parts.into_iter().enumerate() {
                let mut part = Fields::of(part, element(&path, index))?;
                match part.require_string("type")?.as_str() {
                    "text" => blocks.push(Block::Text(part.require_string("text")?)),
                    "image_url" => {
                        let mut image = part.take_object("image_url")?.ok_or_else(|| {
                            Unsupported::new(part.at("image_url"), "an image part needs a URL")
                        })?;
                        let url = image.require_string("url")?;
                        image.ignore("detail");
                        image.finish()?;
                        blocks.push(Block::Image(image_source(&url).ok_or_else(|| {
                            Unsupported::new(
                                part.at("image_url.url"),
                                "an inline image must be a base64 data URL with a media type",
                            )
                        })?));
                    }
                    "input_audio" => {
                        return Err(Unsupported::new(part.at("type"), reason("audio")));
                    }
                    other => {
                        return Err(Unsupported::new(
                            part.at("type"),
                            format!("a user content part must be text or image_url, not `{other}`"),
                        ));
                    }
                }
                part.finish()?;
            }
            Ok(blocks)
        }
        Some(other) => Err(Unsupported::new(
            message.at("content"),
            format!(
                "user content must be a string or parts, not {}",
                json_kind(&other)
            ),
        )),
        None => Err(Unsupported::new(
            message.at("content"),
            "a user message needs content",
        )),
    }
}

/// An `image_url` URL as a source: a `data:` URL is inline base64, anything
/// else is fetched by URL.
fn image_source(url: &str) -> Option<ImageSource> {
    let Some(rest) = url.strip_prefix("data:") else {
        return Some(ImageSource::Url(url.to_owned()));
    };
    let (media_type, data) = rest.split_once(";base64,")?;
    if media_type.is_empty() {
        return None;
    }
    Some(ImageSource::Base64 {
        media_type: media_type.to_owned(),
        data: data.to_owned(),
    })
}

fn image_url(source: &ImageSource) -> String {
    match source {
        ImageSource::Base64 { media_type, data } => format!("data:{media_type};base64,{data}"),
        ImageSource::Url(url) => url.clone(),
    }
}

pub(super) fn encode_request(request: &Request) -> Vec<u8> {
    let mut messages = Vec::new();
    if let Some(system) = &request.system {
        messages.push(json!({"role": "system", "content": system}));
    }
    for message in &request.messages {
        match message.role {
            Role::User => {
                let mut parts = Vec::new();
                let mut all_text = true;
                for block in &message.blocks {
                    match block {
                        Block::ToolResult {
                            tool_use_id,
                            content,
                            is_error,
                        } => {
                            // Results first, as a run of tool messages; the
                            // user's own blocks below follow them.
                            let content = if *is_error {
                                format!("{TOOL_ERROR_MARKER}\n{content}")
                            } else {
                                content.clone()
                            };
                            messages.push(json!({
                                "role": "tool",
                                "tool_call_id": tool_use_id,
                                "content": content,
                            }));
                        }
                        Block::Text(text) => parts.push(json!({"type": "text", "text": text})),
                        Block::Image(source) => {
                            all_text = false;
                            parts.push(json!({
                                "type": "image_url",
                                "image_url": {"url": image_url(source)},
                            }));
                        }
                        // A tool use in a user turn is not a shape either
                        // wire produces; carried as text so nothing is lost
                        // silently, and never reached from a decoder.
                        Block::ToolUse { id, name, input } => {
                            parts.push(json!({
                                "type": "text",
                                "text": format!("[tool_use {id} {name}] {input}"),
                            }));
                        }
                        Block::Thinking { .. } | Block::RedactedThinking { .. } => {
                            unreachable!("refused by `refuse_unencodable` before this point")
                        }
                    }
                }
                if parts.is_empty() {
                    continue;
                }
                let content = if all_text && parts.len() == 1 {
                    parts[0]["text"].clone()
                } else {
                    Value::Array(parts)
                };
                messages.push(json!({"role": "user", "content": content}));
            }
            Role::Assistant => {
                let mut texts = Vec::new();
                let mut calls = Vec::new();
                for block in &message.blocks {
                    match block {
                        Block::Text(text) => texts.push(text.as_str()),
                        Block::ToolUse { id, name, input } => calls.push(json!({
                            "id": id,
                            "type": "function",
                            "function": {"name": name, "arguments": input.to_string()},
                        })),
                        Block::Image(source) => texts.push(match source {
                            ImageSource::Url(url) => url.as_str(),
                            ImageSource::Base64 { .. } => "[image]",
                        }),
                        Block::ToolResult { content, .. } => texts.push(content.as_str()),
                        Block::Thinking { .. } | Block::RedactedThinking { .. } => {
                            unreachable!("refused by `refuse_unencodable` before this point")
                        }
                    }
                }
                let mut entry = Map::new();
                entry.insert("role".to_owned(), json!("assistant"));
                if texts.is_empty() && !calls.is_empty() {
                    entry.insert("content".to_owned(), Value::Null);
                } else {
                    entry.insert("content".to_owned(), json!(texts.join("\n\n")));
                }
                if !calls.is_empty() {
                    entry.insert("tool_calls".to_owned(), Value::Array(calls));
                }
                messages.push(Value::Object(entry));
            }
        }
    }

    let mut document = Map::new();
    document.insert("model".to_owned(), json!(request.model));
    document.insert("messages".to_owned(), Value::Array(messages));
    if !request.tools.is_empty() {
        document.insert(
            "tools".to_owned(),
            Value::Array(
                request
                    .tools
                    .iter()
                    .map(|tool| {
                        let mut function = Map::new();
                        function.insert("name".to_owned(), json!(tool.name));
                        if let Some(description) = &tool.description {
                            function.insert("description".to_owned(), json!(description));
                        }
                        function.insert("parameters".to_owned(), tool.input_schema.clone());
                        json!({"type": "function", "function": Value::Object(function)})
                    })
                    .collect(),
            ),
        );
    }
    if let Some(choice) = &request.tool_choice {
        document.insert(
            "tool_choice".to_owned(),
            match choice {
                ToolChoice::Auto => json!("auto"),
                ToolChoice::Any => json!("required"),
                ToolChoice::None => json!("none"),
                ToolChoice::Tool(name) => json!({"type": "function", "function": {"name": name}}),
            },
        );
    }
    if let Some(parallel) = request.parallel_tool_calls {
        document.insert("parallel_tool_calls".to_owned(), json!(parallel));
    }
    if let Some(max_tokens) = request.max_tokens {
        document.insert("max_tokens".to_owned(), json!(max_tokens));
    }
    if let Some(temperature) = request.temperature {
        document.insert("temperature".to_owned(), json!(temperature));
    }
    if let Some(top_p) = request.top_p {
        document.insert("top_p".to_owned(), json!(top_p));
    }
    if !request.stop.is_empty() {
        document.insert("stop".to_owned(), json!(request.stop));
    }
    if request.stream {
        document.insert("stream".to_owned(), json!(true));
        // Without this the provider never states usage on a stream, and the
        // evidence ledger's exact reading — the reason a translated exchange
        // may record usage at all — would have nothing to read.
        document.insert("stream_options".to_owned(), json!({"include_usage": true}));
    }
    if let Some(user) = &request.user {
        document.insert("user".to_owned(), json!(user));
    }
    // A stable per-session cache-routing hint (2018): unconditional on
    // whether this request's harness marked anything with `cache_control`,
    // because OpenAI Chat's own caching is automatic and prefix-based — the
    // key only helps colocate a session's requests, not gate caching itself.
    if let Some(key) = request.prompt_cache_key() {
        document.insert("prompt_cache_key".to_owned(), json!(key));
    }
    // Carried (GH-EFFORT-CARRY), never invented: only when the harness asked
    // for thinking at all, and always the word its budget maps to at or
    // below what was asked — `level_for_budget` never rounds up.
    if let Some(effort) = &request.effort {
        document.insert(
            "reasoning_effort".to_owned(),
            json!(effort.level().as_openai_word()),
        );
    }
    Value::Object(document).to_string().into_bytes()
}

// --- responses ----------------------------------------------------------------

pub(super) fn decode_response(body: &[u8]) -> Result<Response, Unsupported> {
    let value: Value = serde_json::from_slice(body)
        .map_err(|_| Unsupported::new("body", "the response body is not a JSON document"))?;
    let mut top = Fields::of(value, "")?;
    let id = top.require_string("id")?;
    let model = top.take_string("model")?.unwrap_or_default();
    let mut choices = top.take_array("choices")?.unwrap_or_default();
    if choices.len() != 1 {
        return Err(Unsupported::new(
            "choices",
            format!(
                "exactly one completion is translated; the provider returned {}",
                choices.len()
            ),
        ));
    }
    let mut choice = Fields::of(choices.remove(0), "choices[0]")?;
    choice.ignore("index");
    if let Some(logprobs) = choice.take("logprobs")
        && !logprobs.is_null()
    {
        return Err(Unsupported::new(choice.at("logprobs"), reason("logprobs")));
    }
    let finish = choice.take_string("finish_reason")?;
    let mut message = choice
        .take_object("message")?
        .ok_or_else(|| Unsupported::new(choice.at("message"), "a completion needs a message"))?;
    choice.finish()?;
    message.ignore("role");
    message.ignore("annotations");
    if let Some(refusal) = message.take("refusal")
        && !refusal.is_null()
    {
        return Err(Unsupported::new(message.at("refusal"), reason("refusal")));
    }
    message.refuse_if_present("audio", reason("audio"))?;
    message.refuse_if_present("function_call", reason("function_call"))?;
    let mut blocks = Vec::new();
    match message.take("content") {
        None | Some(Value::Null) => {}
        Some(Value::String(text)) => {
            if !text.is_empty() {
                blocks.push(Block::Text(text));
            }
        }
        Some(other) => {
            return Err(Unsupported::new(
                message.at("content"),
                format!(
                    "a completion's content must be a string or null, not {}",
                    json_kind(&other)
                ),
            ));
        }
    }
    for (index, call) in message
        .take_array("tool_calls")?
        .unwrap_or_default()
        .into_iter()
        .enumerate()
    {
        let mut call = Fields::of(call, element(&message.at("tool_calls"), index))?;
        let id = call.require_string("id")?;
        call.ignore("type");
        call.ignore("index");
        let mut function = call
            .take_object("function")?
            .ok_or_else(|| Unsupported::new(call.at("function"), "a tool call needs a function"))?;
        let name = function.require_string("name")?;
        let arguments = function.take_string("arguments")?.unwrap_or_default();
        let input = parse_tool_input(&arguments)
            .map_err(|why| Unsupported::new(function.at("arguments"), why))?;
        function.finish()?;
        call.finish()?;
        blocks.push(Block::ToolUse { id, name, input });
    }
    message.finish()?;
    let stop_reason = match finish.as_deref() {
        Some(reason) => decode_finish_reason(reason, "choices[0].finish_reason")?,
        None => {
            return Err(Unsupported::new(
                "choices[0].finish_reason",
                "a complete response must say why it stopped",
            ));
        }
    };
    let usage = match top.take_object("usage")? {
        Some(usage) => decode_usage(usage)?,
        None => Usage::default(),
    };
    for ignored in ["object", "created", "system_fingerprint", "service_tier"] {
        top.ignore(ignored);
    }
    top.finish()?;
    Ok(Response {
        id,
        model,
        blocks,
        stop_reason,
        stop_sequence: None,
        usage,
    })
}

fn decode_finish_reason(reason: &str, path: &str) -> Result<StopReason, Unsupported> {
    Ok(match reason {
        "stop" => StopReason::EndTurn,
        "length" => StopReason::MaxTokens,
        "tool_calls" | "function_call" => StopReason::ToolUse,
        "content_filter" => StopReason::Refusal,
        other => {
            return Err(Unsupported::new(
                path.to_owned(),
                format!("the finish reason `{other}` is not one this codec knows"),
            ));
        }
    })
}

fn finish_reason_json(reason: StopReason) -> Value {
    json!(match reason {
        // OpenAI Chat cannot say which stop sequence matched: both are
        // `stop`, and the Anthropic decoder of this response reads `end_turn`.
        StopReason::EndTurn | StopReason::StopSequence => "stop",
        StopReason::MaxTokens => "length",
        StopReason::ToolUse => "tool_calls",
        StopReason::Refusal => "content_filter",
    })
}

fn decode_usage(mut usage: Fields) -> Result<Usage, Unsupported> {
    let prompt = usage.take_u64("prompt_tokens")?.unwrap_or(0);
    let output = usage.take_u64("completion_tokens")?.unwrap_or(0);
    let cached = match usage.take_object("prompt_tokens_details")? {
        Some(mut details) => {
            let cached = details.take_u64("cached_tokens")?;
            details.ignore("audio_tokens");
            details.finish()?;
            cached
        }
        None => None,
    };
    usage.ignore("total_tokens");
    // Read for the reasoning count; the rest of the object is dropped.
    let reasoning = usage
        .take("completion_tokens_details")
        .and_then(|details| details.get("reasoning_tokens").and_then(Value::as_u64));
    usage.finish()?;
    // `prompt_tokens` includes the cached ones; the form's `input` does not.
    Ok(Usage {
        input: prompt.saturating_sub(cached.unwrap_or(0)),
        output,
        cached,
        reasoning,
    })
}

fn usage_json(usage: &Usage) -> Value {
    let prompt = usage.input + usage.cached.unwrap_or(0);
    let mut entry = Map::new();
    entry.insert("prompt_tokens".to_owned(), json!(prompt));
    entry.insert("completion_tokens".to_owned(), json!(usage.output));
    entry.insert("total_tokens".to_owned(), json!(prompt + usage.output));
    if let Some(cached) = usage.cached {
        entry.insert(
            "prompt_tokens_details".to_owned(),
            json!({"cached_tokens": cached}),
        );
    }
    if let Some(reasoning) = usage.reasoning {
        entry.insert(
            "completion_tokens_details".to_owned(),
            json!({"reasoning_tokens": reasoning}),
        );
    }
    Value::Object(entry)
}

fn tool_call_json(index: usize, id: &str, name: &str, arguments: &str) -> Value {
    json!({
        "index": index,
        "id": id,
        "type": "function",
        "function": {"name": name, "arguments": arguments},
    })
}

pub(super) fn encode_response(response: &Response) -> Vec<u8> {
    let mut texts = Vec::new();
    let mut calls = Vec::new();
    for block in &response.blocks {
        match block {
            Block::Text(text) => texts.push(text.as_str()),
            Block::ToolUse { id, name, input } => {
                calls.push(tool_call_json(calls.len(), id, name, &input.to_string()));
            }
            // `decode_response` on no codec produces any of these in an
            // answer — a thinking block least of all: `decode_response_block`
            // refuses one outright (see its own doc comment).
            Block::Image(_)
            | Block::ToolResult { .. }
            | Block::Thinking { .. }
            | Block::RedactedThinking { .. } => {}
        }
    }
    let mut message = Map::new();
    message.insert("role".to_owned(), json!("assistant"));
    message.insert(
        "content".to_owned(),
        if texts.is_empty() {
            Value::Null
        } else {
            json!(texts.join("\n\n"))
        },
    );
    if !calls.is_empty() {
        message.insert("tool_calls".to_owned(), Value::Array(calls));
    }
    json!({
        "id": response.id,
        "object": "chat.completion",
        "created": 0,
        "model": response.model,
        "choices": [{
            "index": 0,
            "message": Value::Object(message),
            "finish_reason": finish_reason_json(response.stop_reason),
            "logprobs": null,
        }],
        "usage": usage_json(&response.usage),
    })
    .to_string()
    .into_bytes()
}

// --- streams ------------------------------------------------------------------

/// Which canonical block is open, and which OpenAI tool-call slot it is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Open {
    Text { index: usize },
    Tool { index: usize, slot: u64 },
}

/// Turns chat-completion chunks into the canonical event order.
///
/// A chunk carries a text fragment, a tool-call fragment, a finish reason, a
/// usage reading, or nothing; Anthropic's order wants blocks that start,
/// receive deltas and stop in sequence. The decoder keeps one block open at a
/// time: a text fragment opens a text block if none is open, a tool call
/// with an `id` opens a tool block (closing whatever was open), and an
/// argument fragment continues the tool block for its slot. The message's
/// own delta and stop are emitted at `[DONE]`, because the usage chunk
/// arrives after the finish reason and nothing before `[DONE]` says the
/// provider has finished speaking.
#[derive(Default)]
struct ChunkDecoder {
    started: bool,
    open: Option<Open>,
    next_index: usize,
    /// Every tool slot that has opened a block, with the block index and the
    /// id it opened under, so a continuation for a closed block — or one
    /// that names a different call — is refused rather than misfiled.
    slots: Vec<(u64, usize, String)>,
    finish: Option<StopReason>,
    usage: Option<Usage>,
    done: bool,
}

impl ChunkDecoder {
    fn close_open(&mut self, events: &mut Vec<StreamEvent>) {
        if let Some(open) = self.open.take() {
            let index = match open {
                Open::Text { index } | Open::Tool { index, .. } => index,
            };
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

    /// Close the message: what `[DONE]` means, and the only way `done` is
    /// ever set. A stream that reaches [`StreamDecoder::finish`] without
    /// having called this was cut before its terminator, and is refused
    /// there rather than completed here.
    fn complete(&mut self) -> Result<Vec<StreamEvent>, Unsupported> {
        self.done = true;
        let mut events = Vec::new();
        self.close_open(&mut events);
        events.push(StreamEvent::MessageDelta {
            stop_reason: self.finish.unwrap_or(StopReason::EndTurn),
            stop_sequence: None,
            usage: self.usage.unwrap_or_default(),
        });
        events.push(StreamEvent::MessageStop);
        Ok(events)
    }
}

impl StreamDecoder for ChunkDecoder {
    fn feed(&mut self, event: &SseEvent) -> Result<Vec<StreamEvent>, Unsupported> {
        if self.done {
            return Ok(Vec::new());
        }
        if event.data.trim() == "[DONE]" {
            return self.complete();
        }
        let value: Value = serde_json::from_str(&event.data)
            .map_err(|_| Unsupported::new("chunk", "a stream chunk was not a JSON document"))?;
        let mut top = Fields::of(value, "")?;
        let mut events = Vec::new();
        if !self.started {
            self.started = true;
            events.push(StreamEvent::MessageStart {
                id: top.take_string("id")?.unwrap_or_default(),
                model: top.take_string("model")?.unwrap_or_default(),
                usage: Usage::default(),
            });
        } else {
            top.ignore("id");
            top.ignore("model");
        }
        for ignored in ["object", "created", "system_fingerprint", "service_tier"] {
            top.ignore(ignored);
        }
        if let Some(usage) = top.take_object("usage")? {
            self.usage = Some(decode_usage(usage)?);
        }
        for (choice_index, choice) in top
            .take_array("choices")?
            .unwrap_or_default()
            .into_iter()
            .enumerate()
        {
            let path = element("choices", choice_index);
            let mut choice = Fields::of(choice, path)?;
            if choice.take_u64("index")?.unwrap_or(0) != 0 {
                return Err(Unsupported::new(choice.at("index"), reason("n")));
            }
            if let Some(logprobs) = choice.take("logprobs")
                && !logprobs.is_null()
            {
                return Err(Unsupported::new(choice.at("logprobs"), reason("logprobs")));
            }
            if let Some(finish) = choice.take_string("finish_reason")? {
                self.finish = Some(decode_finish_reason(&finish, &choice.at("finish_reason"))?);
            }
            let Some(mut delta) = choice.take_object("delta")? else {
                choice.finish()?;
                continue;
            };
            choice.finish()?;
            delta.ignore("role");
            if let Some(refusal) = delta.take("refusal")
                && !refusal.is_null()
            {
                return Err(Unsupported::new(delta.at("refusal"), reason("refusal")));
            }
            delta.refuse_if_present("function_call", reason("function_call"))?;
            delta.refuse_if_present("audio", reason("audio"))?;
            if let Some(Value::String(text)) = delta.take("content")
                && !text.is_empty()
            {
                let index = match self.open {
                    Some(Open::Text { index }) => index,
                    _ => {
                        let index = self.open_block(BlockStart::Text, &mut events);
                        self.open = Some(Open::Text { index });
                        index
                    }
                };
                events.push(StreamEvent::BlockDelta {
                    index,
                    delta: Delta::Text(text),
                });
            }
            for (position, call) in delta
                .take_array("tool_calls")?
                .unwrap_or_default()
                .into_iter()
                .enumerate()
            {
                let mut call = Fields::of(call, element(&delta.at("tool_calls"), position))?;
                let slot = call.take_u64("index")?.unwrap_or(position as u64);
                let id = call.take_string("id")?.filter(|id| !id.is_empty());
                call.ignore("type");
                let (name, arguments) = match call.take_object("function")? {
                    Some(mut function) => {
                        let name = function
                            .take_string("name")?
                            .filter(|name| !name.is_empty());
                        let arguments = function.take_string("arguments")?.unwrap_or_default();
                        function.finish()?;
                        (name, arguments)
                    }
                    None => (None, String::new()),
                };
                let id_path = call.at("id");
                let index_path = call.at("index");
                call.finish()?;
                let opened = self
                    .slots
                    .iter()
                    .find(|(known, _, _)| *known == slot)
                    .map(|(_, _, opened)| opened.clone());
                let index = match opened {
                    None => {
                        let Some(id) = id else {
                            return Err(Unsupported::new(
                                id_path,
                                "a tool call opened without an id, so its result could never be \
                                 matched to it",
                            ));
                        };
                        let name = name.unwrap_or_default();
                        let index = self.open_block(
                            BlockStart::ToolUse {
                                id: id.clone(),
                                name,
                            },
                            &mut events,
                        );
                        self.open = Some(Open::Tool { index, slot });
                        self.slots.push((slot, index, id));
                        index
                    }
                    Some(opened) => {
                        // An id that contradicts the one the slot opened
                        // with is a second call reusing the slot: appending
                        // its arguments to the first call would run the
                        // first tool with the second one's input.
                        if id.is_some_and(|id| id != opened) {
                            return Err(Unsupported::new(
                                id_path,
                                "a tool-call fragment repeated a slot under a different id; its \
                                 arguments cannot be matched to the call that opened the slot",
                            ));
                        }
                        match self.open {
                            Some(Open::Tool {
                                index,
                                slot: open_slot,
                            }) if open_slot == slot => index,
                            _ => {
                                return Err(Unsupported::new(
                                    index_path,
                                    "tool-call fragments interleaved across calls cannot be \
                                     re-ordered into sequential blocks",
                                ));
                            }
                        }
                    }
                };
                if !arguments.is_empty() {
                    events.push(StreamEvent::BlockDelta {
                        index,
                        delta: Delta::InputJson(arguments),
                    });
                }
            }
            delta.finish()?;
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
        // The stream ended without `[DONE]` — the only way `self.done` is
        // ever set is `complete`, above, and this method is not it. What
        // arrived is a truncated message, and completing it here would hand
        // the harness a partial answer wearing `end_turn`.
        Err(Unsupported::new(
            "[DONE]",
            "the provider's stream ended before `data: [DONE]`, so the message it delivered is \
             truncated and not finished",
        ))
    }

    fn is_done(&self) -> bool {
        self.done
    }
}

/// Turns the canonical event order into chat-completion chunks.
#[derive(Default)]
struct ChunkEncoder {
    id: String,
    model: String,
    /// Canonical block index to OpenAI tool-call slot.
    slots: Vec<(usize, usize)>,
}

impl ChunkEncoder {
    fn chunk(&self, delta: Value, finish_reason: Value) -> Vec<u8> {
        stream::encode(
            None,
            &json!({
                "id": self.id,
                "object": "chat.completion.chunk",
                "created": 0,
                "model": self.model,
                "choices": [{"index": 0, "delta": delta, "finish_reason": finish_reason, "logprobs": null}],
            })
            .to_string(),
        )
    }
}

impl StreamEncoder for ChunkEncoder {
    fn encode(&mut self, event: &StreamEvent) -> Vec<u8> {
        match event {
            StreamEvent::MessageStart { id, model, .. } => {
                self.id = id.clone();
                self.model = model.clone();
                self.chunk(json!({"role": "assistant", "content": ""}), Value::Null)
            }
            StreamEvent::BlockStart {
                index,
                block: BlockStart::ToolUse { id, name },
            } => {
                let slot = self.slots.len();
                self.slots.push((*index, slot));
                self.chunk(
                    json!({"tool_calls": [tool_call_json(slot, id, name, "")]}),
                    Value::Null,
                )
            }
            StreamEvent::BlockStart {
                block: BlockStart::Text,
                ..
            }
            | StreamEvent::BlockStop { .. } => Vec::new(),
            StreamEvent::BlockDelta {
                delta: Delta::Text(text),
                ..
            } => self.chunk(json!({"content": text}), Value::Null),
            StreamEvent::BlockDelta {
                index,
                delta: Delta::InputJson(partial),
            } => {
                let slot = self
                    .slots
                    .iter()
                    .find(|(block, _)| block == index)
                    .map(|(_, slot)| *slot)
                    .unwrap_or(0);
                self.chunk(
                    json!({"tool_calls": [{"index": slot, "function": {"arguments": partial}}]}),
                    Value::Null,
                )
            }
            StreamEvent::MessageDelta {
                stop_reason, usage, ..
            } => {
                let mut out = self.chunk(json!({}), finish_reason_json(*stop_reason));
                out.extend(stream::encode(
                    None,
                    &json!({
                        "id": self.id,
                        "object": "chat.completion.chunk",
                        "created": 0,
                        "model": self.model,
                        "choices": [],
                        "usage": usage_json(usage),
                    })
                    .to_string(),
                ));
                out
            }
            StreamEvent::MessageStop => stream::encode(None, "[DONE]"),
        }
    }
}

#[cfg(test)]
mod reasoning_tests {
    use super::*;

    /// The provider's reasoning count is read from
    /// `completion_tokens_details` and written back the same way.
    #[test]
    fn a_reasoning_count_is_carried_both_ways() {
        let mut response = super::super::canonical::tests::tool_call_response();
        response.usage.reasoning = Some(240);
        let wire = encode_response(&response);
        let json: Value = serde_json::from_slice(&wire).unwrap();
        assert_eq!(
            json["usage"]["completion_tokens_details"]["reasoning_tokens"],
            240
        );
        assert_eq!(decode_response(&wire).unwrap().usage.reasoning, Some(240));
        response.usage.reasoning = None;
        let json: Value = serde_json::from_slice(&encode_response(&response)).unwrap();
        assert!(
            json["usage"].get("completion_tokens_details").is_none(),
            "{json}"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use super::super::anthropic::tests::full_request;
    use super::super::canonical::{accumulate, tests::tool_call_response};
    use super::super::stream::SseReader;
    use std::io::BufReader;

    /// `wire` with top-level `key` removed, for a test that must decode
    /// bytes this codec's own encoder wrote minus one field.
    fn drop_key(wire: &[u8], key: &str) -> Vec<u8> {
        let mut document: Value = serde_json::from_slice(wire).expect("valid JSON");
        document
            .as_object_mut()
            .expect("a top-level object")
            .remove(key);
        document.to_string().into_bytes()
    }

    #[test]
    fn a_request_round_trips_through_the_openai_chat_wire() {
        let request = full_request();
        let wire = encode_request(&request);
        // `encode_request` always writes `prompt_cache_key` when `user` is
        // set (2018) — this codec's own hint, added only when it plays
        // *target*. No supported pair has openai-chat as both source and
        // target of itself (`SAME_PROTOCOL` is always refused), so this
        // codec's decoder never needs to read it back, and still refuses it
        // like any other field a real OpenAI-shaped client might set on its
        // own request. Stripped here so the fidelity round trip below
        // covers everything else this codec carries.
        let wire = drop_key(&wire, "prompt_cache_key");
        let decoded = decode_request(&wire).expect("the codec reads what it wrote");
        assert_eq!(decoded, request);
    }

    #[test]
    fn a_thinking_block_is_refused_rather_than_dropped() {
        let mut request = full_request();
        request.messages.push(Message {
            role: Role::Assistant,
            blocks: vec![Block::Thinking {
                thinking: "reasoning the harness never sees".to_owned(),
                signature: "sig".to_owned(),
            }],
        });
        let refusal = OpenAiChat
            .refuse_unencodable(&request)
            .expect_err("a thinking block has no OpenAI Chat equivalent");
        assert_eq!(refusal.field, "thinking block");
        assert!(!refusal.reason.contains("reasoning the harness never sees"));

        let mut request = full_request();
        request.messages.push(Message {
            role: Role::Assistant,
            blocks: vec![Block::RedactedThinking {
                data: "opaque".to_owned(),
            }],
        });
        OpenAiChat
            .refuse_unencodable(&request)
            .expect_err("a redacted thinking block has no OpenAI Chat equivalent either");
    }

    #[test]
    fn effort_is_carried_as_reasoning_effort_and_omitted_when_the_harness_asked_for_none() {
        use super::super::canonical::{EffortLevel, EffortRequest};

        let mut request = full_request();
        request.effort = None;
        let wire = encode_request(&request);
        let document: Value = serde_json::from_slice(&wire).unwrap();
        assert_eq!(
            document.get("reasoning_effort"),
            None,
            "no thinking asked for, no reasoning_effort emitted"
        );

        for (budget, word) in [
            (500u64, "minimal"),
            (4_096, "low"),
            (16_000, "medium"),
            (64_000, "high"),
        ] {
            request.effort = Some(EffortRequest {
                budget_tokens: Some(budget),
                level: None,
            });
            let wire = encode_request(&request);
            let document: Value = serde_json::from_slice(&wire).unwrap();
            assert_eq!(document["reasoning_effort"], word, "budget {budget}");
        }

        // A harness-stated word, were one ever decoded, is used directly.
        request.effort = Some(EffortRequest {
            budget_tokens: None,
            level: Some(EffortLevel::High),
        });
        let wire = encode_request(&request);
        let document: Value = serde_json::from_slice(&wire).unwrap();
        assert_eq!(document["reasoning_effort"], "high");
    }

    #[test]
    fn an_erroring_tool_result_is_carried_as_a_labelled_tool_message_and_restored() {
        let request = full_request();
        let wire = encode_request(&request);
        let document: Value = serde_json::from_slice(&wire).unwrap();
        let tool_messages: Vec<&Value> = document["messages"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|message| message["role"] == "tool")
            .collect();
        assert_eq!(tool_messages.len(), 2);
        assert_eq!(tool_messages[0]["tool_call_id"], "toolu_01A");
        assert_eq!(
            tool_messages[0]["content"],
            format!("{TOOL_ERROR_MARKER}\nls: cannot access")
        );
        assert_eq!(tool_messages[1]["tool_call_id"], "toolu_01B");
        assert_eq!(tool_messages[1]["content"], "# notes\nhello");
        // ... and the tool messages directly follow the assistant turn that
        // made the calls, ids intact.
        let assistant = &document["messages"][2];
        assert_eq!(assistant["role"], "assistant");
        assert_eq!(assistant["tool_calls"][0]["id"], "toolu_01A");
        assert_eq!(assistant["tool_calls"][1]["id"], "toolu_01B");
        assert_eq!(assistant["tool_calls"][0]["function"]["name"], "Bash");
        assert_eq!(
            assistant["tool_calls"][0]["function"]["arguments"],
            r#"{"command":"ls"}"#
        );
        // The tool definitions crossed with their schemas.
        assert_eq!(document["tools"][0]["type"], "function");
        assert_eq!(document["tools"][0]["function"]["name"], "Bash");
        assert_eq!(
            document["tools"][0]["function"]["parameters"]["required"][0],
            "command"
        );
        assert_eq!(
            document["tool_choice"],
            json!({"type": "function", "function": {"name": "Bash"}})
        );
        assert_eq!(document["parallel_tool_calls"], false);
        assert_eq!(document["stream_options"]["include_usage"], true);
        assert_eq!(document["messages"][0]["role"], "system");
    }

    #[test]
    fn the_request_an_openai_client_sends_decodes_with_the_tool_run_merged_into_the_next_turn() {
        let wire = br#"{
            "model": "gpt-x",
            "messages": [
                {"role": "system", "content": "sys"},
                {"role": "user", "content": [{"type": "text", "text": "go"}, {"type": "image_url", "image_url": {"url": "data:image/png;base64,AAAA", "detail": "auto"}}]},
                {"role": "assistant", "content": null, "tool_calls": [{"id": "call_1", "type": "function", "function": {"name": "f", "arguments": ""}}]},
                {"role": "tool", "tool_call_id": "call_1", "content": "ok"},
                {"role": "user", "content": "thanks"}
            ],
            "tools": [{"type": "function", "function": {"name": "f", "description": "d"}}],
            "tool_choice": "required",
            "max_completion_tokens": 50,
            "stop": "END",
            "n": 1,
            "logprobs": false,
            "stream": false
        }"#;
        let request = decode_request(wire).expect("an OpenAI client request decodes");
        assert_eq!(request.system.as_deref(), Some("sys"));
        assert_eq!(request.messages.len(), 3);
        assert_eq!(
            request.messages[1].blocks,
            vec![Block::ToolUse {
                id: "call_1".to_owned(),
                name: "f".to_owned(),
                input: json!({}),
            }]
        );
        assert_eq!(
            request.messages[2].blocks,
            vec![
                Block::ToolResult {
                    tool_use_id: "call_1".to_owned(),
                    content: "ok".to_owned(),
                    is_error: false,
                },
                Block::Text("thanks".to_owned()),
            ]
        );
        assert_eq!(
            request.messages[0].blocks[1],
            Block::Image(ImageSource::Base64 {
                media_type: "image/png".to_owned(),
                data: "AAAA".to_owned()
            })
        );
        assert_eq!(request.tools[0].input_schema["type"], "object");
        assert_eq!(request.tool_choice, Some(ToolChoice::Any));
        assert_eq!(request.max_tokens, Some(50));
        assert_eq!(request.stop, vec!["END".to_owned()]);
    }

    #[test]
    fn every_refused_request_field_is_refused_by_its_name() {
        let base = |extra: &str| {
            format!(r#"{{"model": "m", "messages": [{{"role": "user", "content": "x"}}]{extra}}}"#)
        };
        let cases: Vec<(String, &str)> = vec![
            (base(r#", "n": 2"#), "n"),
            (base(r#", "logprobs": true"#), "logprobs"),
            (base(r#", "top_logprobs": 3"#), "top_logprobs"),
            (base(r#", "response_format": {"type": "json_object"}"#), "response_format"),
            (base(r#", "seed": 7"#), "seed"),
            (base(r#", "presence_penalty": 0.5"#), "presence_penalty"),
            (base(r#", "frequency_penalty": 0.5"#), "frequency_penalty"),
            (base(r#", "logit_bias": {}"#), "logit_bias"),
            (base(r#", "reasoning_effort": "high""#), "reasoning_effort"),
            (base(r#", "unknown_future_field": 1"#), "unknown_future_field"),
            (
                base(r#", "tools": [{"type": "function", "function": {"name": "f", "strict": true}}]"#),
                "tools[0].function.strict",
            ),
            (
                r#"{"model": "m", "messages": [{"role": "user", "content": "x", "name": "bob"}]}"#.to_owned(),
                "messages[0].name",
            ),
            (
                r#"{"model": "m", "messages": [{"role": "assistant", "content": "x", "refusal": "no"}]}"#.to_owned(),
                "messages[0].refusal",
            ),
            (
                r#"{"model": "m", "messages": [{"role": "function", "name": "f", "content": "x"}]}"#.to_owned(),
                "messages[0].name",
            ),
            (
                r#"{"model": "m", "messages": [{"role": "assistant", "tool_calls": [{"id": "c", "type": "function", "function": {"name": "f", "arguments": "not json"}}]}]}"#.to_owned(),
                "messages[0].tool_calls[0].function.arguments",
            ),
        ];
        for (wire, field) in cases {
            let refusal = decode_request(wire.as_bytes())
                .expect_err(&format!("{field} must be refused, in: {wire}"));
            assert_eq!(refusal.field, field, "{wire}");
        }
    }

    #[test]
    fn a_response_round_trips_through_the_openai_chat_wire() {
        let response = tool_call_response();
        let wire = encode_response(&response);
        let document: Value = serde_json::from_slice(&wire).unwrap();
        assert_eq!(document["choices"][0]["finish_reason"], "tool_calls");
        assert_eq!(
            document["choices"][0]["message"]["tool_calls"][0]["id"],
            "call_abc123"
        );
        assert_eq!(document["usage"]["prompt_tokens"], 220);
        assert_eq!(
            document["usage"]["prompt_tokens_details"]["cached_tokens"],
            100
        );
        assert_eq!(
            decode_response(&wire).expect("the codec reads what it wrote"),
            response
        );
    }

    #[test]
    fn a_completion_with_two_choices_or_logprobs_is_refused_by_name() {
        let two = br#"{"id": "x", "choices": [{"index": 0, "message": {"role": "assistant", "content": "a"}, "finish_reason": "stop"}, {"index": 1, "message": {"role": "assistant", "content": "b"}, "finish_reason": "stop"}]}"#;
        assert_eq!(decode_response(two).unwrap_err().field, "choices");
        let logprobs = br#"{"id": "x", "choices": [{"index": 0, "message": {"role": "assistant", "content": "a"}, "finish_reason": "stop", "logprobs": {"content": []}}]}"#;
        assert_eq!(
            decode_response(logprobs).unwrap_err().field,
            "choices[0].logprobs"
        );
    }

    #[test]
    fn a_stream_round_trips_through_the_openai_chat_wire_back_to_the_same_response() {
        let response = tool_call_response();
        let events = response.as_events();
        let mut encoder = ChunkEncoder::default();
        let mut wire = Vec::new();
        for event in &events {
            wire.extend(encoder.encode(event));
        }
        assert!(String::from_utf8_lossy(&wire).ends_with("data: [DONE]\n\n"));

        let mut reader = SseReader::new(BufReader::new(&wire[..]));
        let mut decoder = ChunkDecoder::default();
        let mut decoded = Vec::new();
        while let Some(event) = reader.next_event().unwrap() {
            decoded.extend(decoder.feed(&event).expect("the codec reads what it wrote"));
        }
        assert!(decoder.is_done());
        assert_eq!(accumulate(&decoded).unwrap(), response);
    }

    /// The chunk shape a real OpenAI-compatible provider sends for two
    /// parallel tool calls: a role chunk, a text fragment, each call opened
    /// with its id and continued with argument fragments, a finish chunk, a
    /// usage-only chunk, `[DONE]`.
    #[test]
    fn real_provider_chunks_become_anthropics_event_order_with_ids_preserved() {
        let chunks = [
            r#"{"id":"chatcmpl-1","object":"chat.completion.chunk","created":1,"model":"gpt-x","choices":[{"index":0,"delta":{"role":"assistant","content":""},"finish_reason":null}]}"#,
            r#"{"id":"chatcmpl-1","object":"chat.completion.chunk","created":1,"model":"gpt-x","choices":[{"index":0,"delta":{"content":"Sure."},"finish_reason":null}]}"#,
            r#"{"id":"chatcmpl-1","object":"chat.completion.chunk","created":1,"model":"gpt-x","choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"call_A","type":"function","function":{"name":"Bash","arguments":""}}]},"finish_reason":null}]}"#,
            r#"{"id":"chatcmpl-1","object":"chat.completion.chunk","created":1,"model":"gpt-x","choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"function":{"arguments":"{\"command\""}}]},"finish_reason":null}]}"#,
            r#"{"id":"chatcmpl-1","object":"chat.completion.chunk","created":1,"model":"gpt-x","choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"function":{"arguments":": \"ls\"}"}}]},"finish_reason":null}]}"#,
            r#"{"id":"chatcmpl-1","object":"chat.completion.chunk","created":1,"model":"gpt-x","choices":[{"index":0,"delta":{"tool_calls":[{"index":1,"id":"call_B","type":"function","function":{"name":"Read","arguments":"{\"file_path\": \"x\"}"}}]},"finish_reason":null}]}"#,
            r#"{"id":"chatcmpl-1","object":"chat.completion.chunk","created":1,"model":"gpt-x","choices":[{"index":0,"delta":{},"finish_reason":"tool_calls"}]}"#,
            r#"{"id":"chatcmpl-1","object":"chat.completion.chunk","created":1,"model":"gpt-x","choices":[],"usage":{"prompt_tokens":50,"completion_tokens":9,"total_tokens":59}}"#,
            "[DONE]",
        ];
        let mut decoder = ChunkDecoder::default();
        let mut events = Vec::new();
        for data in chunks {
            events.extend(
                decoder
                    .feed(&SseEvent {
                        event: None,
                        data: data.to_owned(),
                    })
                    .expect("a real provider's chunks decode"),
            );
        }
        let response = accumulate(&events).expect("a complete message");
        assert_eq!(
            response.blocks,
            vec![
                Block::Text("Sure.".to_owned()),
                Block::ToolUse {
                    id: "call_A".to_owned(),
                    name: "Bash".to_owned(),
                    input: json!({"command": "ls"}),
                },
                Block::ToolUse {
                    id: "call_B".to_owned(),
                    name: "Read".to_owned(),
                    input: json!({"file_path": "x"}),
                },
            ]
        );
        assert_eq!(response.stop_reason, StopReason::ToolUse);
        assert_eq!(
            response.usage,
            Usage {
                input: 50,
                output: 9,
                cached: None,
                reasoning: None,
            }
        );
        // The order is Anthropic's: every block stops before the next
        // starts, and the message delta comes last but one.
        let stops: Vec<usize> = events
            .iter()
            .filter_map(|event| match event {
                StreamEvent::BlockStop { index } => Some(*index),
                _ => None,
            })
            .collect();
        assert_eq!(stops, vec![0, 1, 2]);
        assert!(matches!(
            events[events.len() - 2],
            StreamEvent::MessageDelta {
                stop_reason: StopReason::ToolUse,
                ..
            }
        ));
    }

    #[test]
    fn a_tool_call_opened_without_an_id_is_refused_rather_than_given_one() {
        let mut decoder = ChunkDecoder::default();
        let chunk = r#"{"id":"c","model":"m","choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"function":{"name":"f","arguments":"{}"}}]},"finish_reason":null}]}"#;
        let refusal = decoder
            .feed(&SseEvent {
                event: None,
                data: chunk.to_owned(),
            })
            .unwrap_err();
        assert_eq!(refusal.field, "choices[0].delta.tool_calls[0].id");
    }

    /// break/gateway-translate #5: a provider that restarts slot numbering
    /// per call reuses `index: 0` for a second, unrelated tool call. On the
    /// unpatched decoder the second call's `id` was read and dropped, and
    /// its arguments were appended to the *first* call's block under the
    /// *first* call's id — canonical.rs's own header: "a wrong id here runs
    /// the wrong tool."
    #[test]
    fn a_tool_call_that_reuses_a_slot_under_a_different_id_is_refused_rather_than_misfiled() {
        let mut decoder = ChunkDecoder::default();
        decoder
            .feed(&SseEvent {
                event: None,
                data: r#"{"id":"c","model":"m","choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"call_A","type":"function","function":{"name":"Bash","arguments":""}}]},"finish_reason":null}]}"#.to_owned(),
            })
            .expect("the first call opens its block");
        let refusal = decoder
            .feed(&SseEvent {
                event: None,
                data: r#"{"id":"c","model":"m","choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"call_B","type":"function","function":{"name":"Read","arguments":"{}"}}]},"finish_reason":null}]}"#.to_owned(),
            })
            .expect_err("a slot reused under a different id must be refused, not misfiled");
        assert_eq!(refusal.field, "choices[0].delta.tool_calls[0].id");
        assert!(
            refusal.reason.contains("different id"),
            "{}",
            refusal.reason
        );

        // The ordinary case this must not break: a continuation fragment
        // that carries no id at all still continues the open call.
        let mut decoder = ChunkDecoder::default();
        decoder
            .feed(&SseEvent {
                event: None,
                data: r#"{"id":"c","model":"m","choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"call_A","type":"function","function":{"name":"Bash","arguments":""}}]},"finish_reason":null}]}"#.to_owned(),
            })
            .unwrap();
        decoder
            .feed(&SseEvent {
                event: None,
                data: r#"{"id":"c","model":"m","choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"function":{"arguments":"{\"command\":\"ls\"}"}}]},"finish_reason":null}]}"#.to_owned(),
            })
            .expect("a continuation with no id keeps continuing the open call");
    }

    #[test]
    fn a_second_choice_in_a_stream_is_refused_by_name() {
        let mut decoder = ChunkDecoder::default();
        let chunk = r#"{"id":"c","model":"m","choices":[{"index":1,"delta":{"content":"x"},"finish_reason":null}]}"#;
        assert_eq!(
            decoder
                .feed(&SseEvent {
                    event: None,
                    data: chunk.to_owned()
                })
                .unwrap_err()
                .field,
            "choices[0].index"
        );
    }

    /// break/gateway-translate #2: a provider connection that dies mid-answer
    /// — after real content, before `[DONE]` — must not be delivered as a
    /// normally-ended message. On the unpatched decoder this returned
    /// `Ok([BlockStop, MessageDelta{stop_reason: EndTurn}, MessageStop])`,
    /// indistinguishable from a real, complete answer.
    #[test]
    fn a_stream_that_ends_without_done_is_refused_as_truncated_rather_than_closed() {
        let mut decoder = ChunkDecoder::default();
        decoder
            .feed(&SseEvent {
                event: None,
                data: r#"{"id":"c","model":"m","choices":[{"index":0,"delta":{"content":"hi"},"finish_reason":"stop"}]}"#.to_owned(),
            })
            .unwrap();
        let refusal = decoder
            .finish()
            .expect_err("a stream cut before `[DONE]` must not complete cleanly");
        assert_eq!(refusal.field, "[DONE]");
        assert!(refusal.reason.contains("truncated"), "{}", refusal.reason);

        let mut empty = ChunkDecoder::default();
        assert_eq!(empty.finish().unwrap_err().field, "chunk");
    }

    /// The sibling of the truncation case: `[DONE]` still closes the message
    /// normally, and a second `finish` after it is the harmless no-op every
    /// other codec's terminator gives.
    #[test]
    fn a_stream_that_ends_with_done_closes_the_message_and_a_later_finish_is_a_no_op() {
        let mut decoder = ChunkDecoder::default();
        decoder
            .feed(&SseEvent {
                event: None,
                data: r#"{"id":"c","model":"m","choices":[{"index":0,"delta":{"content":"hi"},"finish_reason":"stop"}]}"#.to_owned(),
            })
            .unwrap();
        let closing = decoder
            .feed(&SseEvent {
                event: None,
                data: "[DONE]".to_owned(),
            })
            .expect("`[DONE]` completes the message");
        assert!(matches!(closing[0], StreamEvent::BlockStop { index: 0 }));
        assert!(matches!(
            closing[1],
            StreamEvent::MessageDelta {
                stop_reason: StopReason::EndTurn,
                ..
            }
        ));
        assert_eq!(closing[2], StreamEvent::MessageStop);
        assert!(decoder.is_done());
        assert_eq!(decoder.finish().unwrap(), Vec::new());
    }
}
