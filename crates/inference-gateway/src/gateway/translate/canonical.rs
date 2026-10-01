//! The one form every codec meets in — Phase 56's canonical request, response
//! and stream-event vocabulary.
//!
//! # Why one form and not a translator per pair
//!
//! `archive/glasshouse:docs/product/design-decisions.md`, Phase 56, *"the user's answer on
//! pairs: all of them"*: translation is one canonical form plus one codec per
//! wire protocol, so three protocols cost three codecs rather than six
//! translators, and a pair is a decoder and an encoder meeting here. Fidelity
//! is a property of a codec and is tested per codec, by round trip through
//! this form; per pair only the end-to-end test is owed.
//!
//! History: design-decisions.md, "Trims: gateway module docs", translate/canonical.rs module doc.

use serde_json::Value;

use crate::routing::evidence::TurnShape;

/// A request, as either protocol's client would have made it.
#[derive(Debug, Clone, PartialEq)]
pub struct Request {
    pub model: String,
    /// Required by Anthropic Messages, optional on OpenAI Chat; carried as
    /// given.
    pub max_tokens: Option<u64>,
    /// The system prompt as one text. Anthropic's array of system blocks is
    /// joined with a blank line; OpenAI's `system` message is taken as is.
    pub system: Option<String>,
    pub messages: Vec<Message>,
    pub tools: Vec<ToolDefinition>,
    pub tool_choice: Option<ToolChoice>,
    /// `Some(false)` when the harness asked that tool calls not be issued in
    /// parallel — Anthropic's `disable_parallel_tool_use`, OpenAI's
    /// `parallel_tool_calls`.
    pub parallel_tool_calls: Option<bool>,
    pub temperature: Option<f64>,
    pub top_p: Option<f64>,
    /// Stop sequences, in the order given. Empty when none.
    pub stop: Vec<String>,
    pub stream: bool,
    /// An end-user identifier the harness attached — Anthropic's
    /// `metadata.user_id`, OpenAI's `user`.
    pub user: Option<String>,
    /// Whether the harness marked any point of this request for prompt
    /// caching — Anthropic's `cache_control`, on the system prompt, a
    /// content block or a tool definition.
    ///
    /// Collapsed to one flag rather than threaded through [`Block`] and
    /// [`ToolDefinition`]: no target this gateway translates to has a
    /// per-block cache primitive to carry a position into. OpenAI's
    /// `prompt_cache_key` and Gemini's cached-content resource are both
    /// request/session-scoped, so the position Claude Code marked carries
    /// no information a target here can use — only the fact that caching
    /// was asked for at all, which is what a decoder must not silently drop
    /// (see the module doc's *"refused by name, never dropped"*).
    pub cache_requested: bool,
    /// The harness's own thinking/reasoning request, carried rather than
    /// refused (`archive/glasshouse:docs/product/design-decisions.md`, *"Carrying effort across
    /// a translated pairing"*) — `cache_requested`'s sibling: one canonical
    /// field standing in for a shape Anthropic's `thinking` expresses as a
    /// token budget and OpenAI's `reasoning_effort` expresses as a word.
    /// `None` means the harness asked for no thinking at all, which must
    /// encode identically to a request built before this field existed.
    pub effort: Option<EffortRequest>,
}

/// The harness's thinking request, decoder-agnostic: a token budget
/// (Anthropic's `thinking.budget_tokens`), a word (no decoder sets this
/// today — see below), or both.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EffortRequest {
    pub budget_tokens: Option<u64>,
    pub level: Option<EffortLevel>,
}

impl EffortRequest {
    /// The level this request maps to: the harness's own word when it
    /// stated one (Anthropic's decoder sets `level` from
    /// `output_config.effort`, which is how `xhigh` and `max` arrive),
    /// otherwise the word its `thinking.budget_tokens` falls into by
    /// [`level_for_budget`].
    pub fn level(&self) -> EffortLevel {
        self.level
            .unwrap_or_else(|| level_for_budget(self.budget_tokens.unwrap_or(0)))
    }
}

/// The four-word effort ladder every target codec maps onto: OpenAI's
/// `reasoning_effort` / `reasoning.effort` accept exactly these words among
/// others (`developers.openai.com/api/docs/guides/reasoning`, fetched
/// 2026-09-02: *"Supported values are model-dependent and can include
/// `none`, `minimal`, `low`, `medium`, `high`, `xhigh`, and `max`"*) — a
/// token budget maps onto the first four; `xhigh` and `max` are reachable
/// only when the harness stated the word itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum EffortLevel {
    Minimal,
    Low,
    Medium,
    High,
    /// Above [`Self::High`], and reachable only from a harness that states
    /// its word: no token budget maps here, because Anthropic publishes no
    /// waypoint above 32k to cut at.
    Xhigh,
    /// The top of the ladder, on the same terms as [`Self::Xhigh`].
    Max,
}

impl EffortLevel {
    /// The word an OpenAI-shaped wire spells this level with — the same
    /// spelling on both `reasoning_effort` (Chat Completions) and
    /// `reasoning.effort` (Responses), per the citation on this type.
    pub fn as_openai_word(self) -> &'static str {
        match self {
            EffortLevel::Minimal => "minimal",
            EffortLevel::Low => "low",
            EffortLevel::Medium => "medium",
            EffortLevel::High => "high",
            EffortLevel::Xhigh => "xhigh",
            EffortLevel::Max => "max",
        }
    }
}

/// The `budget_tokens` boundaries the four-word ladder is cut at, and why
/// each one sits where it does — from Anthropic's own guidance for manual
/// `budget_tokens`
/// (`platform.claude.com/docs/en/build-with-claude/extended-thinking`,
/// fetched 2026-09-02), the only source that publishes concrete numbers for
/// this parameter: the API's minimum is 1,024; *"for simple tasks, start
/// near the 1,024-token minimum"*; *"for complex tasks, start with a larger
/// budget of 16,000 tokens or more"*; and *"for thinking budgets above 32k,
/// use batch processing"* because larger requests risk timing out. Neither
/// OpenAI's nor Gemini's own docs publish a token-to-word table, so the cut
/// points are Anthropic's own waypoints on the one axis every source and
/// target here agrees is ordered: more budget is more thinking.
///
/// A budget at or below this is [`EffortLevel::Minimal`] — twice the
/// documented floor, still inside the range the docs call a starting point
/// for simple tasks.
pub const EFFORT_MINIMAL_MAX: u64 = 2_048;
/// A budget at or below this (and above [`EFFORT_MINIMAL_MAX`]) is
/// [`EffortLevel::Low`] — below the 16,000-token starting point the docs
/// give for complex tasks, so still short of that range.
pub const EFFORT_LOW_MAX: u64 = 8_192;
/// A budget at or below this (and above [`EFFORT_LOW_MAX`]) is
/// [`EffortLevel::Medium`] — the complex-task range the docs describe,
/// capped just under the 32,000-token line where the docs say batch
/// processing is needed to avoid request timeouts.
pub const EFFORT_MEDIUM_MAX: u64 = 32_000;
// Anything above `EFFORT_MEDIUM_MAX` is `EffortLevel::High` — the harness
// asked for more thinking than this four-word ladder can distinguish
// further, and a mapping never rounds up past it either.

/// `budget_tokens` to the word it maps onto, never rounding up: a value at
/// or below a threshold gets that threshold's word, so an increase in
/// budget only ever moves the result to a higher or equal word, and the
/// ladder saturates at [`EffortLevel::High`] rather than growing without
/// bound.
pub fn level_for_budget(budget_tokens: u64) -> EffortLevel {
    if budget_tokens <= EFFORT_MINIMAL_MAX {
        EffortLevel::Minimal
    } else if budget_tokens <= EFFORT_LOW_MAX {
        EffortLevel::Low
    } else if budget_tokens <= EFFORT_MEDIUM_MAX {
        EffortLevel::Medium
    } else {
        EffortLevel::High
    }
}

impl Request {
    /// The stable per-session prompt-cache key a target that accepts one
    /// should carry (2018) — Claude Code's own `metadata.user_id`, already
    /// decoded into [`Request::user`] and already sent to every provider
    /// under that name, so nothing new crosses the wire. Never anything
    /// else: a cache key must be a value already visible to the evidence
    /// ledger, not the gateway's own token or a credential.
    pub fn prompt_cache_key(&self) -> Option<&str> {
        self.user.as_deref()
    }

    /// Serialize deterministically (2016): tool definitions sorted by name,
    /// once, at the seam every translated request passes through, so all
    /// three encoders emit the same order for the same tool set however the
    /// harness listed them. JSON-Schema key order needs no equivalent step
    /// here — this crate never enables `serde_json`'s `preserve_order`
    /// feature, so `Value::Object` is a sorted map and already serializes
    /// with keys in order; see the tripwire test in this module.
    pub fn normalized(mut self) -> Self {
        self.tools.sort_by(|a, b| a.name.cmp(&b.name));
        self
    }

    /// What shape this turn is — `crate::database` migration 24's
    /// `turn_shape`, and half of what capability map line 2039's shadow
    /// measurement selects on.
    ///
    /// [`TurnShape::ToolResume`] when the last [`Role::User`] message exists
    /// and **every** one of its blocks is a [`Block::ToolResult`]:
    /// that is the harness handing back what a tool returned, with nothing
    /// of its own added. [`TurnShape::Prompt`] otherwise — which includes a
    /// request with no user message at all, and a user message with no
    /// blocks, because neither is a resumption of a tool call and inventing
    /// a third word for them would give the ledger a bucket nothing means.
    /// A message that mixes a tool result with text is a prompt: the person
    /// typed something, and 2039's reduction is only ever offered on the
    /// turn where they did not.
    ///
    /// A pure function of the decoded request, with no reference to the
    /// target protocol, which is why it is derived once at the translation
    /// seam rather than by any codec.
    pub fn turn_shape(&self) -> TurnShape {
        let last_user = self
            .messages
            .iter()
            .rev()
            .find(|message| message.role == Role::User);
        match last_user {
            Some(message)
                if !message.blocks.is_empty()
                    && message
                        .blocks
                        .iter()
                        .all(|block| matches!(block, Block::ToolResult { .. })) =>
            {
                TurnShape::ToolResume
            }
            _ => TurnShape::Prompt,
        }
    }

    /// Line 1334's `repairs`: how many [`Block::ToolResult`] blocks across
    /// every message carried `is_error: true` — the harness's own report
    /// that a previous tool call failed and this exchange is the model
    /// repairing it. Counted once at the seam that already walks these
    /// blocks for [`Self::turn_shape`], across the whole request rather than
    /// only the last user message: a repair can be handed back alongside
    /// other turns in a longer conversation, not only as the very next
    /// message.
    ///
    /// A pure function of the decoded request, with no reference to the
    /// target protocol — see [`Self::turn_shape`]'s own doc comment for why
    /// that is the shape every derivation here takes.
    pub fn error_tool_results(&self) -> u32 {
        self.messages
            .iter()
            .flat_map(|message| &message.blocks)
            .filter(|block| matches!(block, Block::ToolResult { is_error: true, .. }))
            .count() as u32
    }
}

impl From<EffortLevel> for crate::routing::evidence::EffortLevel {
    /// The wire ladder's word, as the ledger stores it — `crate::database`
    /// migration 24's `effort_level`.
    ///
    /// An exhaustive match on purpose: a seventh [`EffortLevel`] must not
    /// compile until somebody has decided what the ledger stores for it, and
    /// `every_wire_effort_level_stores_and_reads_back_as_the_same_word`
    /// below pins the six spellings themselves in lockstep.
    fn from(level: EffortLevel) -> Self {
        match level {
            EffortLevel::Minimal => Self::Minimal,
            EffortLevel::Low => Self::Low,
            EffortLevel::Medium => Self::Medium,
            EffortLevel::High => Self::High,
            EffortLevel::Xhigh => Self::Xhigh,
            EffortLevel::Max => Self::Max,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Message {
    pub role: Role,
    pub blocks: Vec<Block>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    User,
    Assistant,
}

/// One typed content block.
#[derive(Debug, Clone, PartialEq)]
pub enum Block {
    Text(String),
    Image(ImageSource),
    /// The model asked for a tool to run. `id` is preserved verbatim across
    /// both wires — see the module doc.
    ToolUse {
        id: String,
        name: String,
        input: Value,
    },
    /// The harness ran a tool and this is what it returned. `tool_use_id`
    /// names the [`Block::ToolUse`] it answers, again verbatim.
    ToolResult {
        tool_use_id: String,
        content: String,
        is_error: bool,
    },
    /// The model's own extended-thinking output, and the provider's proof
    /// that it produced it. `signature` is opaque provider state, not data:
    /// it must be carried byte-for-byte, never normalised, trimmed,
    /// re-encoded, or logged, because the provider checks it against the
    /// `thinking` text on the next turn and rejects the exchange if either
    /// changed. Contrast [`cache_control`](Message), which the same
    /// `decode_block` consumes into a flag rather than carrying: a cache hint
    /// is advice the codec is free to drop, a thinking signature is a value
    /// the *provider* will verify, so it survives untouched or the block is
    /// void.
    Thinking {
        thinking: String,
        signature: String,
    },
    /// A thinking block the provider redacted before it left the response.
    /// `data` is as opaque as [`Block::Thinking`]'s `signature` and is
    /// carried under the same rule.
    RedactedThinking {
        data: String,
    },
}

/// Where an image's bytes come from. A URL carries no media type on either
/// wire — Anthropic's `url` source and OpenAI's `image_url` both leave it to
/// the fetch — so only the inline form names one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ImageSource {
    /// Inline base64 data, without a `data:` prefix, and its IANA media
    /// type, e.g. `image/png`.
    Base64 {
        media_type: String,
        data: String,
    },
    Url(String),
}

#[derive(Debug, Clone, PartialEq)]
pub struct ToolDefinition {
    pub name: String,
    pub description: Option<String>,
    /// A JSON Schema object. Carried as given; nothing here validates it.
    pub input_schema: Value,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolChoice {
    Auto,
    /// The model must call some tool — Anthropic `any`, OpenAI `required`.
    Any,
    /// The model must call this tool.
    Tool(String),
    None,
}

/// A complete (non-streamed) response.
#[derive(Debug, Clone, PartialEq)]
pub struct Response {
    pub id: String,
    pub model: String,
    /// Only [`Block::Text`] and [`Block::ToolUse`] can appear here. A live
    /// thinking block is refused by every `decode_response`, not carried:
    /// `Response` folds to and from [`StreamEvent`] in [`Response::as_events`]
    /// and [`accumulate`], and a thinking block's `signature` arrives over a
    /// real Anthropic stream as its own delta — carrying it through that fold
    /// needs a `BlockStart`/`Delta` shape this form does not have yet.
    pub blocks: Vec<Block>,
    pub stop_reason: StopReason,
    pub stop_sequence: Option<String>,
    pub usage: Usage,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopReason {
    EndTurn,
    MaxTokens,
    StopSequence,
    ToolUse,
    /// The provider declined to answer — OpenAI's `content_filter`,
    /// Anthropic's `refusal`.
    Refusal,
}

/// Token counts as the provider stated them.
///
/// `input` excludes tokens served from a prompt cache; `cached` is those, when
/// the provider distinguished them. `None` is "the provider did not say",
/// which stays different from zero all the way to the evidence ledger.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Usage {
    pub input: u64,
    pub output: u64,
    pub cached: Option<u64>,
    /// Of `output`, what the provider counted as reasoning, when it said.
    /// A reasoning model's thinking is often sent encrypted or not at all;
    /// this count is then the one thing a harness can show of it.
    pub reasoning: Option<u64>,
}

/// One event of a streamed response, in the one vocabulary both stream
/// codecs speak. The order is Anthropic's, because it is the stricter of the
/// two: a message starts, blocks start, receive deltas and stop, the message
/// gets its final delta with the stop reason and usage, and the message
/// stops.
#[derive(Debug, Clone, PartialEq)]
pub enum StreamEvent {
    MessageStart {
        id: String,
        model: String,
        /// What is known at the start: the input count when the provider
        /// states it up front, zero otherwise.
        usage: Usage,
    },
    BlockStart {
        index: usize,
        block: BlockStart,
    },
    BlockDelta {
        index: usize,
        delta: Delta,
    },
    BlockStop {
        index: usize,
    },
    MessageDelta {
        stop_reason: StopReason,
        stop_sequence: Option<String>,
        /// The final counts. An `input` of zero here means "not restated",
        /// and [`accumulate`] keeps the start's reading.
        usage: Usage,
    },
    MessageStop,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BlockStart {
    Text,
    ToolUse { id: String, name: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Delta {
    Text(String),
    /// A fragment of the tool input's JSON, to be concatenated in order.
    InputJson(String),
}

/// A wire field or shape this form has no home for, named.
///
/// `field` is a path in the wire document's own spelling — `system[0].cache_control`,
/// `choices` — and `reason` is one sentence a user can act on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unsupported {
    pub field: String,
    pub reason: String,
}

impl Unsupported {
    pub fn new(field: impl Into<String>, reason: impl Into<String>) -> Self {
        Self {
            field: field.into(),
            reason: reason.into(),
        }
    }
}

impl std::fmt::Display for Unsupported {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "`{}`: {}", self.field, self.reason)
    }
}

impl Response {
    /// The event sequence a streamed delivery of this response produces.
    ///
    /// What a translated exchange writes when the harness asked for a stream
    /// and the provider answered with a whole document anyway — and the
    /// other half of the stream round trip [`accumulate`] closes.
    pub fn as_events(&self) -> Vec<StreamEvent> {
        let mut events = vec![StreamEvent::MessageStart {
            id: self.id.clone(),
            model: self.model.clone(),
            usage: Usage {
                input: self.usage.input,
                output: 0,
                cached: self.usage.cached,
                reasoning: None,
            },
        }];
        for (index, block) in self.blocks.iter().enumerate() {
            match block {
                Block::Text(text) => {
                    events.push(StreamEvent::BlockStart {
                        index,
                        block: BlockStart::Text,
                    });
                    if !text.is_empty() {
                        events.push(StreamEvent::BlockDelta {
                            index,
                            delta: Delta::Text(text.clone()),
                        });
                    }
                }
                Block::ToolUse { id, name, input } => {
                    events.push(StreamEvent::BlockStart {
                        index,
                        block: BlockStart::ToolUse {
                            id: id.clone(),
                            name: name.clone(),
                        },
                    });
                    events.push(StreamEvent::BlockDelta {
                        index,
                        delta: Delta::InputJson(input.to_string()),
                    });
                }
                // A response never carries these — `decode_response` on either
                // codec cannot produce them — so a stream of one has nothing
                // to say about them either.
                Block::Image(_)
                | Block::ToolResult { .. }
                | Block::Thinking { .. }
                | Block::RedactedThinking { .. } => {}
            }
            events.push(StreamEvent::BlockStop { index });
        }
        events.push(StreamEvent::MessageDelta {
            stop_reason: self.stop_reason,
            stop_sequence: self.stop_sequence.clone(),
            usage: self.usage,
        });
        events.push(StreamEvent::MessageStop);
        events
    }
}

/// Fold a complete event sequence back into the response it delivered.
///
/// Refuses a sequence that is not one message — no start, a delta for a
/// block that never started, a tool input that is not a JSON object once its
/// fragments are joined — naming what was wrong.
pub fn accumulate(events: &[StreamEvent]) -> Result<Response, Unsupported> {
    struct Open {
        block: Block,
        json: String,
    }
    let mut id = None;
    let mut model = String::new();
    let mut usage = Usage::default();
    let mut blocks: Vec<Open> = Vec::new();
    let mut stop_reason = None;
    let mut stop_sequence = None;

    for event in events {
        match event {
            StreamEvent::MessageStart {
                id: started,
                model: started_model,
                usage: started_usage,
            } => {
                id = Some(started.clone());
                model = started_model.clone();
                usage = *started_usage;
            }
            StreamEvent::BlockStart { index, block } => {
                if *index != blocks.len() {
                    return Err(Unsupported::new(
                        format!("content_block_start[{index}]"),
                        format!("blocks must start in order; {} were open", blocks.len()),
                    ));
                }
                blocks.push(Open {
                    block: match block {
                        BlockStart::Text => Block::Text(String::new()),
                        BlockStart::ToolUse { id, name } => Block::ToolUse {
                            id: id.clone(),
                            name: name.clone(),
                            input: Value::Null,
                        },
                    },
                    json: String::new(),
                });
            }
            StreamEvent::BlockDelta { index, delta } => {
                let Some(open) = blocks.get_mut(*index) else {
                    return Err(Unsupported::new(
                        format!("content_block_delta[{index}]"),
                        "a delta arrived for a block that never started",
                    ));
                };
                match (&mut open.block, delta) {
                    (Block::Text(text), Delta::Text(more)) => text.push_str(more),
                    (Block::ToolUse { .. }, Delta::InputJson(more)) => open.json.push_str(more),
                    (Block::Text(_), Delta::InputJson(_)) => {
                        return Err(Unsupported::new(
                            format!("content_block_delta[{index}]"),
                            "a tool-input delta arrived for a text block",
                        ));
                    }
                    (Block::ToolUse { .. }, Delta::Text(_)) => {
                        return Err(Unsupported::new(
                            format!("content_block_delta[{index}]"),
                            "a text delta arrived for a tool-use block",
                        ));
                    }
                    (
                        Block::Image(_)
                        | Block::ToolResult { .. }
                        | Block::Thinking { .. }
                        | Block::RedactedThinking { .. },
                        _,
                    ) => {
                        unreachable!("a block opened here is always a text or tool-use block")
                    }
                }
            }
            StreamEvent::BlockStop { index } => {
                let Some(open) = blocks.get_mut(*index) else {
                    return Err(Unsupported::new(
                        format!("content_block_stop[{index}]"),
                        "a stop arrived for a block that never started",
                    ));
                };
                if let Block::ToolUse { input, .. } = &mut open.block {
                    *input = parse_tool_input(&open.json).map_err(|reason| {
                        Unsupported::new(format!("content_block_stop[{index}]"), reason)
                    })?;
                }
            }
            StreamEvent::MessageDelta {
                stop_reason: reason,
                stop_sequence: sequence,
                usage: final_usage,
            } => {
                stop_reason = Some(*reason);
                stop_sequence = sequence.clone();
                usage = Usage {
                    input: if final_usage.input == 0 {
                        usage.input
                    } else {
                        final_usage.input
                    },
                    output: final_usage.output,
                    cached: final_usage.cached.or(usage.cached),
                    reasoning: final_usage.reasoning.or(usage.reasoning),
                };
            }
            StreamEvent::MessageStop => {}
        }
    }

    let Some(id) = id else {
        return Err(Unsupported::new(
            "message_start",
            "the stream ended without a message ever starting",
        ));
    };
    let Some(stop_reason) = stop_reason else {
        return Err(Unsupported::new(
            "message_delta",
            "the stream ended without a stop reason",
        ));
    };
    Ok(Response {
        id,
        model,
        blocks: blocks.into_iter().map(|open| open.block).collect(),
        stop_reason,
        stop_sequence,
        usage,
    })
}

/// The ordering rules [`accumulate`] enforces over a whole sequence, enforced
/// one event at a time on a stream that is never accumulated.
///
/// A stream codec's encoder may only be handed a sequence in which a block
/// starts before its deltas and a delta names the block that is open. Nothing
/// downstream re-checks it: an encoder writes bytes and has no error channel,
/// so an out-of-order delta silently rides on whichever block happens to be
/// open — under the **wrong tool-call id**. This is where that is refused.
#[derive(Debug, Default)]
pub struct Order {
    started: usize,
    open: Option<usize>,
}

impl Order {
    /// `Ok(())` when `event` may follow everything checked so far.
    pub fn check(&mut self, event: &StreamEvent) -> Result<(), Unsupported> {
        match event {
            StreamEvent::BlockStart { index, .. } => {
                if *index != self.started {
                    return Err(Unsupported::new(
                        format!("content_block_start[{index}]"),
                        format!("blocks must start in order; {} had started", self.started),
                    ));
                }
                self.started += 1;
                self.open = Some(*index);
            }
            StreamEvent::BlockDelta { index, .. } => {
                if self.open != Some(*index) {
                    return Err(Unsupported::new(
                        format!("content_block_delta[{index}]"),
                        "a delta arrived for a block that is not the open one, and a delta \
                         carried onto another block would carry another tool call's id",
                    ));
                }
            }
            StreamEvent::BlockStop { index } => {
                if self.open != Some(*index) {
                    return Err(Unsupported::new(
                        format!("content_block_stop[{index}]"),
                        "a stop arrived for a block that is not the open one",
                    ));
                }
                self.open = None;
            }
            StreamEvent::MessageStart { .. }
            | StreamEvent::MessageDelta { .. }
            | StreamEvent::MessageStop => {}
        }
        Ok(())
    }
}

/// A tool input from its streamed JSON fragments: an empty text is an empty
/// object, which is what both wires mean by a tool call with no arguments.
pub fn parse_tool_input(json: &str) -> Result<Value, String> {
    if json.trim().is_empty() {
        return Ok(Value::Object(serde_json::Map::new()));
    }
    match serde_json::from_str::<Value>(json) {
        Ok(value @ Value::Object(_)) => Ok(value),
        Ok(other) => Err(format!(
            "a tool input must be a JSON object, not {}",
            json_kind(&other)
        )),
        Err(_) => Err("the tool input's arguments are not valid JSON".to_owned()),
    }
}

/// The kind of a JSON value, for a refusal that names what arrived.
pub fn json_kind(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "a boolean",
        Value::Number(_) => "a number",
        Value::String(_) => "a string",
        Value::Array(_) => "an array",
        Value::Object(_) => "an object",
    }
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;

    use serde_json::json;

    pub(crate) fn tool_call_response() -> Response {
        Response {
            id: "msg_01".to_owned(),
            model: "gpt-x".to_owned(),
            blocks: vec![
                Block::Text("Let me look.".to_owned()),
                Block::ToolUse {
                    id: "call_abc123".to_owned(),
                    name: "Bash".to_owned(),
                    input: json!({"command": "ls -la", "timeout": 5}),
                },
                Block::ToolUse {
                    id: "call_def456".to_owned(),
                    name: "Read".to_owned(),
                    input: json!({"file_path": "/tmp/x"}),
                },
            ],
            stop_reason: StopReason::ToolUse,
            stop_sequence: None,
            usage: Usage {
                input: 120,
                output: 33,
                cached: Some(100),
                reasoning: None,
            },
        }
    }

    #[test]
    fn json_object_keys_serialize_sorted_because_this_crate_never_enables_preserve_order() {
        // The tripwire for 2016: if `serde_json`'s `preserve_order` feature
        // is ever turned on (a feature unification from an unrelated
        // dependency, not a choice made here), a JSON-Schema's declared key
        // order would leak into encoded bytes and break prefix stability
        // across turns — this pins the sorted-map behaviour the encoders
        // rely on instead.
        let mut map = serde_json::Map::new();
        map.insert("zebra".to_owned(), json!(1));
        map.insert("apple".to_owned(), json!(2));
        map.insert("mango".to_owned(), json!(3));
        let value = Value::Object(map);
        assert_eq!(value.to_string(), r#"{"apple":2,"mango":3,"zebra":1}"#);
    }

    #[test]
    fn normalized_sorts_tools_by_name_regardless_of_the_harnesss_order() {
        let tool = |name: &str| ToolDefinition {
            name: name.to_owned(),
            description: None,
            input_schema: json!({}),
        };
        let request = Request {
            model: "m".to_owned(),
            max_tokens: None,
            system: None,
            messages: Vec::new(),
            tools: vec![tool("zeta"), tool("alpha"), tool("mu")],
            tool_choice: None,
            parallel_tool_calls: None,
            temperature: None,
            top_p: None,
            stop: Vec::new(),
            stream: false,
            user: None,
            cache_requested: false,
            effort: None,
        }
        .normalized();
        let names: Vec<&str> = request.tools.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(names, vec!["alpha", "mu", "zeta"]);
    }

    #[test]
    fn a_response_streams_out_and_accumulates_back_to_itself() {
        let response = tool_call_response();
        let events = response.as_events();
        assert_eq!(
            events.first(),
            Some(&StreamEvent::MessageStart {
                id: "msg_01".to_owned(),
                model: "gpt-x".to_owned(),
                usage: Usage {
                    input: 120,
                    output: 0,
                    cached: Some(100),
                    reasoning: None,
                },
            })
        );
        assert_eq!(events.last(), Some(&StreamEvent::MessageStop));
        assert_eq!(accumulate(&events).expect("a well-formed stream"), response);
    }

    #[test]
    fn a_tool_input_streamed_in_fragments_is_joined_before_it_is_parsed() {
        let events = vec![
            StreamEvent::MessageStart {
                id: "m".to_owned(),
                model: "x".to_owned(),
                usage: Usage::default(),
            },
            StreamEvent::BlockStart {
                index: 0,
                block: BlockStart::ToolUse {
                    id: "call_1".to_owned(),
                    name: "Edit".to_owned(),
                },
            },
            StreamEvent::BlockDelta {
                index: 0,
                delta: Delta::InputJson("{\"path\": \"a".to_owned()),
            },
            StreamEvent::BlockDelta {
                index: 0,
                delta: Delta::InputJson("/b\", \"n\": 2}".to_owned()),
            },
            StreamEvent::BlockStop { index: 0 },
            StreamEvent::MessageDelta {
                stop_reason: StopReason::ToolUse,
                stop_sequence: None,
                usage: Usage {
                    input: 0,
                    output: 7,
                    cached: None,
                    reasoning: None,
                },
            },
            StreamEvent::MessageStop,
        ];
        let response = accumulate(&events).expect("fragments join into one object");
        assert_eq!(
            response.blocks,
            vec![Block::ToolUse {
                id: "call_1".to_owned(),
                name: "Edit".to_owned(),
                input: json!({"path": "a/b", "n": 2}),
            }]
        );
        assert_eq!(response.stop_reason, StopReason::ToolUse);
    }

    #[test]
    fn a_stream_with_no_arguments_at_all_is_an_empty_object_and_not_a_refusal() {
        assert_eq!(parse_tool_input(""), Ok(json!({})));
        assert_eq!(parse_tool_input("  "), Ok(json!({})));
        assert!(parse_tool_input("[1]").is_err());
        assert!(parse_tool_input("{not json").is_err());
    }

    #[test]
    fn a_malformed_stream_is_refused_by_the_event_that_was_wrong() {
        let orphan_delta = [StreamEvent::BlockDelta {
            index: 3,
            delta: Delta::Text("x".to_owned()),
        }];
        let refusal = accumulate(&orphan_delta).expect_err("no block started");
        assert_eq!(refusal.field, "content_block_delta[3]");

        let no_stop = [StreamEvent::MessageStart {
            id: "m".to_owned(),
            model: "x".to_owned(),
            usage: Usage::default(),
        }];
        assert_eq!(
            accumulate(&no_stop).expect_err("no stop reason").field,
            "message_delta"
        );
    }

    fn tool_start(id: &str) -> BlockStart {
        BlockStart::ToolUse {
            id: id.to_owned(),
            name: "Bash".to_owned(),
        }
    }

    #[test]
    fn order_accepts_a_well_ordered_stream() {
        let mut order = Order::default();
        assert_eq!(
            order.check(&StreamEvent::MessageStart {
                id: "m".to_owned(),
                model: "x".to_owned(),
                usage: Usage::default(),
            }),
            Ok(())
        );
        assert_eq!(
            order.check(&StreamEvent::BlockStart {
                index: 0,
                block: tool_start("call_A"),
            }),
            Ok(())
        );
        assert_eq!(
            order.check(&StreamEvent::BlockDelta {
                index: 0,
                delta: Delta::InputJson("{}".to_owned()),
            }),
            Ok(())
        );
        assert_eq!(order.check(&StreamEvent::BlockStop { index: 0 }), Ok(()));
        assert_eq!(
            order.check(&StreamEvent::BlockStart {
                index: 1,
                block: tool_start("call_B"),
            }),
            Ok(())
        );
        assert_eq!(order.check(&StreamEvent::BlockStop { index: 1 }), Ok(()));
        assert_eq!(
            order.check(&StreamEvent::MessageDelta {
                stop_reason: StopReason::ToolUse,
                stop_sequence: None,
                usage: Usage::default(),
            }),
            Ok(())
        );
        assert_eq!(order.check(&StreamEvent::MessageStop), Ok(()));
    }

    #[test]
    fn order_refuses_a_block_that_starts_out_of_sequence() {
        let mut order = Order::default();
        // Block 1 starts before block 0 has ever started.
        let refusal = order
            .check(&StreamEvent::BlockStart {
                index: 1,
                block: tool_start("call_A"),
            })
            .expect_err("index 1 cannot start before index 0");
        assert_eq!(refusal.field, "content_block_start[1]");
        assert!(refusal.reason.contains("blocks must start in order"));
    }

    #[test]
    fn order_refuses_a_delta_for_a_block_that_is_not_open() {
        let mut order = Order::default();
        // The exact hazard shape: call_A starts, call_B starts before call_A
        // stops, then a delta arrives addressed to call_A (index 0) while
        // call_B (index 1) is the open block.
        order
            .check(&StreamEvent::BlockStart {
                index: 0,
                block: tool_start("call_A"),
            })
            .expect("call_A starts");
        order
            .check(&StreamEvent::BlockStart {
                index: 1,
                block: tool_start("call_B"),
            })
            .expect("call_B starts before call_A stopped");
        let refusal = order
            .check(&StreamEvent::BlockDelta {
                index: 0,
                delta: Delta::InputJson("{\"command\"".to_owned()),
            })
            .expect_err("call_A's delta must not ride on call_B's open block");
        assert_eq!(refusal.field, "content_block_delta[0]");
        assert!(refusal.reason.contains("another tool call's id"));
    }

    #[test]
    fn order_refuses_a_stop_for_a_block_that_is_not_open() {
        let mut order = Order::default();
        order
            .check(&StreamEvent::BlockStart {
                index: 0,
                block: tool_start("call_A"),
            })
            .expect("call_A starts");
        order
            .check(&StreamEvent::BlockStart {
                index: 1,
                block: tool_start("call_B"),
            })
            .expect("call_B starts before call_A stopped");
        let refusal = order
            .check(&StreamEvent::BlockStop { index: 0 })
            .expect_err("index 0 is not the open block");
        assert_eq!(refusal.field, "content_block_stop[0]");
        assert!(refusal.reason.contains("not the open one"));
    }

    /// The lockstep pin `crate::routing::evidence::EffortLevel`'s own doc
    /// comment names: every word this wire ladder can produce stores as a
    /// word the ledger reads back as the same level, and the ledger's
    /// spelling is the wire's own (`as_openai_word`) rather than a second
    /// table that could drift from it.
    ///
    /// The `for` list is exhaustive by construction: the `match` in
    /// `From<EffortLevel> for evidence::EffortLevel` fails to compile if a
    /// seventh variant appears, and this test fails if that variant is then
    /// given a spelling the ledger cannot read back.
    #[test]
    fn every_wire_effort_level_stores_and_reads_back_as_the_same_word() {
        use crate::routing::evidence::EffortLevel as Stored;

        for level in [
            EffortLevel::Minimal,
            EffortLevel::Low,
            EffortLevel::Medium,
            EffortLevel::High,
            EffortLevel::Xhigh,
            EffortLevel::Max,
        ] {
            let stored = Stored::from(level);
            assert_eq!(
                stored.as_str(),
                level.as_openai_word(),
                "the ledger must store the same word this level is spelled with on the wire"
            );
            assert_eq!(
                Stored::from_stored(stored.as_str()),
                Some(stored),
                "a word this build stored must be a word this build reads back"
            );
        }
    }

    /// `turn_shape` is `ToolResume` only for a last user message that is
    /// *entirely* tool results — the three cases migration 24's column has
    /// to keep apart.
    #[test]
    fn turn_shape_is_tool_resume_only_when_the_last_user_message_is_all_tool_results() {
        let tool_result = |id: &str| Block::ToolResult {
            tool_use_id: id.to_owned(),
            content: "done".to_owned(),
            is_error: false,
        };
        let request = |messages: Vec<Message>| Request {
            model: "m".to_owned(),
            max_tokens: None,
            system: None,
            messages,
            tools: Vec::new(),
            tool_choice: None,
            parallel_tool_calls: None,
            temperature: None,
            top_p: None,
            stop: Vec::new(),
            stream: false,
            user: None,
            cache_requested: false,
            effort: None,
        };

        // Every block of the last user message is a tool result — including
        // when there are two of them, which is what a parallel tool call
        // comes back as.
        assert_eq!(
            request(vec![
                Message {
                    role: Role::User,
                    blocks: vec![Block::Text("go".to_owned())],
                },
                Message {
                    role: Role::Assistant,
                    blocks: vec![Block::ToolUse {
                        id: "call_A".to_owned(),
                        name: "Bash".to_owned(),
                        input: Value::Null,
                    }],
                },
                Message {
                    role: Role::User,
                    blocks: vec![tool_result("call_A"), tool_result("call_B")],
                },
            ])
            .turn_shape(),
            TurnShape::ToolResume
        );

        // A plain prompt.
        assert_eq!(
            request(vec![Message {
                role: Role::User,
                blocks: vec![Block::Text("hi".to_owned())],
            }])
            .turn_shape(),
            TurnShape::Prompt
        );

        // One tool result and one line of text: the person typed something,
        // so this is a prompt.
        assert_eq!(
            request(vec![Message {
                role: Role::User,
                blocks: vec![tool_result("call_A"), Block::Text("and also".to_owned())],
            }])
            .turn_shape(),
            TurnShape::Prompt
        );

        // A user message with no blocks at all is a prompt, not a third word.
        assert_eq!(
            request(vec![Message {
                role: Role::User,
                blocks: Vec::new(),
            }])
            .turn_shape(),
            TurnShape::Prompt
        );

        // No user message anywhere, and an assistant message whose blocks
        // would otherwise qualify: still a prompt, because the rule asks
        // about the last USER message.
        assert_eq!(
            request(vec![Message {
                role: Role::Assistant,
                blocks: vec![tool_result("call_A")],
            }])
            .turn_shape(),
            TurnShape::Prompt
        );

        // No messages at all.
        assert_eq!(request(Vec::new()).turn_shape(), TurnShape::Prompt);
    }

    #[test]
    fn level_for_budget_never_rounds_up_and_saturates_at_high() {
        // At or below the documented floor: the lowest word, never omitted
        // (2039's (f)).
        assert_eq!(level_for_budget(0), EffortLevel::Minimal);
        assert_eq!(level_for_budget(1), EffortLevel::Minimal);
        assert_eq!(level_for_budget(1_024), EffortLevel::Minimal);
        assert_eq!(level_for_budget(EFFORT_MINIMAL_MAX), EffortLevel::Minimal);
        // One token past a boundary moves to the next word, never skipping.
        assert_eq!(level_for_budget(EFFORT_MINIMAL_MAX + 1), EffortLevel::Low);
        assert_eq!(level_for_budget(EFFORT_LOW_MAX), EffortLevel::Low);
        assert_eq!(level_for_budget(EFFORT_LOW_MAX + 1), EffortLevel::Medium);
        assert_eq!(level_for_budget(EFFORT_MEDIUM_MAX), EffortLevel::Medium);
        // Above every threshold: High, and nothing higher exists to round
        // up to.
        assert_eq!(level_for_budget(EFFORT_MEDIUM_MAX + 1), EffortLevel::High);
        assert_eq!(level_for_budget(1_000_000), EffortLevel::High);
    }

    #[test]
    fn effort_request_level_derives_from_budget_when_no_word_was_set() {
        let request = EffortRequest {
            budget_tokens: Some(500),
            level: None,
        };
        assert_eq!(request.level(), EffortLevel::Minimal);

        // A harness-stated word, were one ever decoded, wins over deriving
        // one from the budget.
        let request = EffortRequest {
            budget_tokens: Some(500),
            level: Some(EffortLevel::High),
        };
        assert_eq!(request.level(), EffortLevel::High);
    }

    #[test]
    fn order_lets_message_events_pass_through_regardless_of_block_state() {
        let mut order = Order::default();
        assert_eq!(
            order.check(&StreamEvent::MessageStart {
                id: "m".to_owned(),
                model: "x".to_owned(),
                usage: Usage::default(),
            }),
            Ok(())
        );
        assert_eq!(
            order.check(&StreamEvent::MessageDelta {
                stop_reason: StopReason::EndTurn,
                stop_sequence: None,
                usage: Usage::default(),
            }),
            Ok(()),
            "message events carry no block index and are never refused by Order"
        );
        assert_eq!(order.check(&StreamEvent::MessageStop), Ok(()));
    }
}
