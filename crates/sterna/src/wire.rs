//! The Anthropic Messages wire format: turning a [`crate::contract::Conversation`]
//! into a request body, and a response back into a [`crate::contract::Message`].
//!
//! `docs/model-contract.md` fixes the one invariant this
//! module exists for: the request body is byte-identical whether
//! `ANTHROPIC_BASE_URL` names Glasshouse's gateway or nothing at all, because
//! a gateway hop that changed one byte would break the prompt cache on the
//! far side. [`request_body`] is why that invariant holds by construction --
//! it has no parameter through which a base URL could reach the body.

use std::collections::BTreeMap;
use std::env;
use std::fmt;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::contract::{Block, Conversation, Message, Role};

/// The Anthropic Messages endpoint used when `ANTHROPIC_BASE_URL` names
/// nothing.
pub const DEFAULT_BASE_URL: &str = "https://api.anthropic.com";

/// The Messages API path, appended to whichever base URL applies.
const MESSAGES_PATH: &str = "/v1/messages";

/// The model this request is for, named in the **head**.
///
/// Glasshouse's gateway routes on the request head and never on the body, so
/// a session that wants a frontier model for reasoning and a cheap one for
/// reduction has to say which is which somewhere the gateway can read before
/// it forwards a byte. The model is already in the body; this repeats it in
/// one header so the gateway need not buffer the payload to find it.
///
/// Harmless everywhere else: a provider reached directly ignores a header it
/// does not know, and Glasshouse strips it rather than forwarding it.
const MODEL_HEADER: &str = "x-glasshouse-model";

/// The `anthropic-version` header sterna sends on every request. 61C's
/// `/model` slash command points at this and [`MODEL`] rather than a
/// literal, so both stay in one place.
pub const ANTHROPIC_VERSION: &str = "2023-06-01";

/// User-selected response effort. `auto` sends no effort: the model's own
/// choice, and the wire body stays untouched.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Effort {
    #[default]
    Auto,
    Low,
    Medium,
    High,
    Xhigh,
    Max,
}
impl Effort {
    /// What a main-model turn asks for: one the person chose is sent as
    /// chosen; `auto` on an OpenAI-family model is `low`
    /// ([`crate::session`]'s `turn_effort` states why). The screen shows this,
    /// so the effort it names is the one sent.
    pub fn sent_for(self, model: &str) -> Effort {
        let openai = matches!(
            crate::abi::Dialect::for_model(model),
            crate::abi::Dialect::OpenAi
        );
        if self == Effort::Auto && openai {
            Effort::Low
        } else {
            self
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "auto" => Some(Self::Auto),
            "low" => Some(Self::Low),
            "medium" => Some(Self::Medium),
            "high" => Some(Self::High),
            "xhigh" => Some(Self::Xhigh),
            "max" => Some(Self::Max),
            _ => None,
        }
    }
    /// The notice every route that sets the effort prints.
    pub fn now(self) -> String {
        format!(
            "Effort is now {} · applies from the next request",
            self.name()
        )
    }
    pub fn name(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
            Self::Xhigh => "xhigh",
            Self::Max => "max",
        }
    }
}

/// A compiled-in model name, and the last one in this binary.
///
/// **It is not a routing default and must never become one.** Startup already
/// refuses to spend anything against a model nobody chose: `--model`, then
/// `[model] parent`, then the picker on a terminal and a refusal in a script
/// (`session/startup.rs`). Nothing here is consulted on that path.
///
/// What it is today: the placeholder `RuntimeState` holds before the first
/// turn (`runtime/state.rs`, overwritten by `set_task_context` on every turn),
/// and the model this crate's own tests run a session on. The first of those
/// is a latent hazard rather than a working default -- `RuntimeState::
/// agent_model` falls back to this field, so a subagent started before a turn
/// set it would inherit a name the person never chose.
///
/// The successor, which belongs to whoever next edits `runtime/state.rs`:
/// initialise that field empty, have `agent_model` refuse rather than invent
/// when it is empty, and let this constant be what it already is in practice --
/// a test fixture, moved to the tests that use it. Phase 73's direction is
/// that no model name is compiled in, and this is the one that remains.
pub const MODEL: &str = "claude-opus-5";

/// The `max_tokens` sterna asks for when nothing published says otherwise.
///
/// **A documented fallback, not a policy, and not the common case.** The
/// right figure is the model's own, which [`max_tokens_for`] uses whenever
/// the gateway published one -- measured 2026-09-19, `inference-gateway
/// models --json` carries `max_output_tokens` for 170 of 647 entries,
/// including every GPT and Claude model this project runs (128,000 each).
///
/// This is for a model the catalogue does not name. It was 8,192, chosen so
/// no provider would refuse the request, on the reasoning that a truncation
/// is survivable -- and for a reasoning model it is not: the thinking spends
/// the allowance before the program is written, nothing finished survives,
/// and the task ends. DeepSeek V4.1 Flash, which its API names
/// `deepseek-flash`, ended 11 of 18 SWE tasks that way on 2026-09-29. So the
/// fallback is what current models accept, a turn still cut off is given
/// more ([`raise_output_room`]), and a provider that refuses this much is
/// asked for less ([`lower_output_room`]).
pub const MAX_TOKENS: u32 = 32_768;

/// The most room [`raise_output_room`] gives a model with no published limit.
pub const MAX_RAISED_TOKENS: u32 = 131_072;

/// What a model whose provider refused [`MAX_TOKENS`] is asked for instead.
pub const REFUSED_SIZE_TOKENS: u32 = 8_192;

/// A model's room after a cut-off or a refusal, for the rest of the process.
#[derive(Clone, Copy)]
struct Room {
    tokens: u32,
    /// The provider refused a larger request, so this is never raised again.
    refused: bool,
}

static OUTPUT_ROOM: std::sync::Mutex<BTreeMap<String, Room>> =
    std::sync::Mutex::new(BTreeMap::new());

fn room_for(model: &str) -> Option<Room> {
    OUTPUT_ROOM.lock().ok()?.get(model).copied()
}

fn set_room(model: &str, room: Room) {
    if let Ok(mut rooms) = OUTPUT_ROOM.lock() {
        rooms.insert(model.to_string(), room);
    }
}

/// What one turn of `model` may produce: the model's own published maximum;
/// otherwise the room a cut-off or a refusal left it, or [`MAX_TOKENS`].
#[must_use]
pub fn max_tokens_for(model: &str) -> u32 {
    let limits = crate::models::limits_for(model);
    if limits.max_output_tokens.is_some() {
        return max_tokens_from(limits);
    }
    room_for(model).map_or(MAX_TOKENS, |room| room.tokens)
}

/// Gives `model` four times its room, up to [`MAX_RAISED_TOKENS`], after a
/// turn was cut off before anything in it could run. `false` -- send nothing
/// again -- for a model with a published limit, one already at the most, and
/// one whose provider refused a larger request.
pub fn raise_output_room(model: &str) -> bool {
    if crate::models::limits_for(model).max_output_tokens.is_some()
        || room_for(model).is_some_and(|room| room.refused)
    {
        return false;
    }
    let current = max_tokens_for(model);
    if current >= MAX_RAISED_TOKENS {
        return false;
    }
    set_room(
        model,
        Room {
            tokens: current.saturating_mul(4).min(MAX_RAISED_TOKENS),
            refused: false,
        },
    );
    true
}

/// Asks [`REFUSED_SIZE_TOKENS`] of `model` from now on, after its provider
/// refused the size asked for. `false` when it already asks no more than that
/// or publishes its own limit.
pub fn lower_output_room(model: &str) -> bool {
    if crate::models::limits_for(model).max_output_tokens.is_some()
        || max_tokens_for(model) <= REFUSED_SIZE_TOKENS
    {
        return false;
    }
    set_room(
        model,
        Room {
            tokens: REFUSED_SIZE_TOKENS,
            refused: true,
        },
    );
    true
}

/// Whether `error` is a provider refusing the output size asked for: a 400
/// whose body names the output-token field.
#[must_use]
pub fn refused_size(error: &WireError) -> bool {
    matches!(error, WireError::Status { status: 400, body_head }
        if ["max_tokens", "max_completion_tokens", "max_output_tokens"]
            .iter()
            .any(|field| body_head.contains(field)))
}

/// [`max_tokens_for`]'s decision, with the lookup already done -- the seam a
/// test drives, because the published figures are a process-wide answer.
///
/// A published figure larger than `u32` is the provider's, not ours, and is
/// clamped rather than refused: asking for more than the wire can express is
/// a request nobody can serve.
#[must_use]
pub fn max_tokens_from(limits: crate::models::ModelLimits) -> u32 {
    limits.max_output_tokens.map_or(MAX_TOKENS, |published| {
        u32::try_from(published).unwrap_or(u32::MAX)
    })
}

/// The base URL a turn's request goes to: `ANTHROPIC_BASE_URL` if it is set
/// to a non-empty value, [`DEFAULT_BASE_URL`] otherwise. This is the entire
/// decision behind map line 2445 -- everything else about the request is
/// fixed regardless of which URL this returns.
pub fn base_url() -> String {
    env::var("ANTHROPIC_BASE_URL")
        .ok()
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| DEFAULT_BASE_URL.to_string())
}

#[derive(Serialize)]
struct RequestBody<'a> {
    model: &'a str,
    max_tokens: u32,
    system: Vec<SystemBlock<'a>>,
    messages: Vec<WireMessage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tools: Option<Vec<Value>>,
    /// `Some(true)` only on the streaming path.
    ///
    /// **Skipped when absent, which is what keeps the non-streaming body byte
    /// identical** — `the_gateway_hop_changes_no_byte` and the golden request
    /// test both compare whole bodies, and a `"stream":false` would be a new
    /// byte in every ordinary turn.
    #[serde(skip_serializing_if = "Option::is_none")]
    stream: Option<bool>,
    /// `{"user_id": <the session's cache key>}` once [`set_cache_key`] ran:
    /// the gateway carries it to the Responses API as `prompt_cache_key`,
    /// which keeps a session's requests on one cache ([`cache_key_for`]).
    /// Absent before that.
    #[serde(skip_serializing_if = "Option::is_none")]
    metadata: Option<Metadata>,
}

#[derive(Serialize)]
struct Metadata {
    user_id: &'static str,
}

static CACHE_KEY: std::sync::OnceLock<String> = std::sync::OnceLock::new();

/// The header CLIProxyAPI derives a Codex `prompt_cache_key` from for a
/// Claude-format request; without it the key is unstable and consecutive
/// requests land on machines that do not hold their prefix (measured
/// 2026-09-24: 0 of 4 replayed requests hit without it, 3 of 3 with it).
pub const SESSION_HEADER: &str = "x-claude-code-session-id";

/// How many prompt-cache keys Sterna's sessions share between them.
const CACHE_BUCKETS: u8 = 4;

/// The prompt-cache key a session's requests carry: one of
/// [`CACHE_BUCKETS`] keys every Sterna session shares, chosen by its id.
///
/// **Shared, so a new session starts warm.** The provider routes a request by
/// the prompt's first tokens and this key, and caches on the machine it lands
/// on. A key per session sent each new session to a machine that had never
/// seen Sterna's system prompt: measured 2026-09-30 through the gateway, a new
/// key found the prompt cached 3 times in 6 and a repeated key 96 % of the
/// time, and over 30 SWE-bench tasks the uncached first requests were a fifth
/// of Sterna's fresh input. Several keys rather than one, because a key that
/// carries more than about fifteen requests a minute spills onto machines
/// that do not hold the prefix; a session keeps its one key throughout.
pub fn cache_key_for(session: &str) -> String {
    use sha2::{Digest, Sha256};
    let bucket = Sha256::digest(session.as_bytes())[0] % CACHE_BUCKETS;
    format!("sterna-{bucket}")
}

/// Sets the prompt-cache key every later request carries, in the body's
/// metadata and in [`SESSION_HEADER`], from the process's session. The first
/// call wins; one process is one session.
pub fn set_cache_key(session: &str) {
    let _ = CACHE_KEY.set(cache_key_for(session));
}

/// A main-model request the session chose to summarise
/// ([`TurnRouting::plan_summary`]) asks for a readable summary of its reasoning.
///
/// Behind the subscription broker a GPT model's reasoning otherwise arrives
/// encrypted only; `thinking.display = "summarized"` is what CLIProxyAPI turns
/// into `reasoning.summary`. A request with no thinking object (the default
/// effort) gets `medium`'s, which is GPT-6's own default, so the effort the
/// provider applies does not change. Claude routes are left as they are.
fn with_reasoning_summary(body: Vec<u8>, model: &str) -> Vec<u8> {
    if model.contains("claude") {
        return body;
    }
    let mut value: serde_json::Value = serde_json::from_slice(&body).expect("serialized request");
    if value.get("thinking").is_none() {
        let budget = effort_budget(Effort::Medium);
        let max_tokens = value["max_tokens"].as_u64().unwrap_or(0);
        value["max_tokens"] = serde_json::json!(max_tokens + u64::from(budget));
        value["thinking"] = serde_json::json!({"type": "enabled", "budget_tokens": budget});
    }
    value["thinking"]["display"] = serde_json::json!("summarized");
    serde_json::to_vec(&value).expect("serialized request")
}

/// The main model's Claude request marks its history for the prompt cache.
///
/// Without it only the system block and the tool list carry `cache_control`,
/// so every turn re-sent the whole conversation uncached. Claude Code's shape:
/// one ephemeral breakpoint on the newest message's last block that can carry
/// one (never a reasoning block), moving forward each turn -- the marker is
/// not part of the cached content, so the earlier turn's prefix still matches.
/// That makes three breakpoints of the four allowed. Other models are left
/// as they are.
fn with_history_breakpoint(body: Vec<u8>, model: &str) -> Vec<u8> {
    if !model.contains("claude") {
        return body;
    }
    let mut value: serde_json::Value = serde_json::from_slice(&body).expect("serialized request");
    let block = value["messages"]
        .as_array_mut()
        .and_then(|messages| messages.last_mut())
        .and_then(|message| message["content"].as_array_mut())
        .and_then(|blocks| {
            blocks.iter_mut().rev().find(|block| {
                !matches!(
                    block["type"].as_str(),
                    Some("thinking" | "redacted_thinking")
                )
            })
        });
    match block {
        Some(block) => block["cache_control"] = serde_json::json!({"type": "ephemeral"}),
        None => return body,
    }
    serde_json::to_vec(&value).expect("serialized request")
}

/// The ChatGPT backend's sticky-routing token for one task's requests.
pub const TURN_STATE_HEADER: &str = "x-codex-turn-state";

/// One task's sticky routing: the token the backend hands out on the task's
/// first response, echoed on every later request of that task so they reach
/// the machine holding their cached prefix -- without it each request is
/// routed alone and mostly misses the cache (measured 2026-09-24: 4 of 10
/// hits). Codex's contract, which this follows: keep the first token for the
/// whole turn and never carry it into the next one ([`TurnRouting::clear`]
/// at each task start). Only the main model's requests carry it.
#[derive(Debug, Default)]
pub struct TurnRouting {
    token: std::sync::Mutex<Option<String>>,
    /// When this task last asked for a reasoning summary ([`Self::plan_summary`]).
    summarized: std::sync::Mutex<Option<std::time::Instant>>,
    /// Whether the next main-model request asks for one.
    summary_next: std::sync::atomic::AtomicBool,
}

/// How often, at most, a watched session asks the main model for a readable
/// summary of its reasoning after the task's first request.
///
/// A summary costs time, not tokens: 16 paired requests through the gateway
/// (gpt-6-sol, low effort, 2026-09-29) took 5.1 s with one and 3.4 s without,
/// slower in every pair at the same output. Asked on every request it was a
/// third of a turn's latency; the person watching needs it only often enough
/// to stay in the loop, and a run nobody watches needs none.
pub const SUMMARY_EVERY: std::time::Duration = std::time::Duration::from_secs(30);

impl TurnRouting {
    /// A new task: a new routing token, and the task's first request is
    /// summarised again.
    pub fn clear(&self) {
        *self.token.lock().unwrap_or_else(|e| e.into_inner()) = None;
        *self.summarized.lock().unwrap_or_else(|e| e.into_inner()) = None;
    }

    pub fn token(&self) -> Option<String> {
        self.token.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// Decides whether the next main-model request asks for a reasoning
    /// summary: never when nobody watches the session live; otherwise the
    /// task's first request, and then one every [`SUMMARY_EVERY`].
    pub fn plan_summary(&self, watched: bool) {
        let due = watched && {
            let mut last = self.summarized.lock().unwrap_or_else(|e| e.into_inner());
            let due = last.is_none_or(|at| at.elapsed() >= SUMMARY_EVERY);
            if due {
                *last = Some(std::time::Instant::now());
            }
            due
        };
        self.summary_next
            .store(due, std::sync::atomic::Ordering::Relaxed);
    }

    fn wants_summary(&self) -> bool {
        self.summary_next.load(std::sync::atomic::Ordering::Relaxed)
    }

    fn observe(&self, headers: &ureq::http::HeaderMap) {
        let mut held = self.token.lock().unwrap_or_else(|e| e.into_inner());
        if held.is_none()
            && let Some(value) = headers.get(TURN_STATE_HEADER).and_then(|v| v.to_str().ok())
        {
            *held = Some(value.to_string());
        }
    }
}

/// Cache only the session's system prompt, before volatile conversation state.
/// A separate tools breakpoint lets changed project instructions reuse tools.
#[derive(Serialize)]
struct SystemBlock<'a> {
    #[serde(rename = "type")]
    kind: &'static str,
    text: &'a str,
    cache_control: CacheControl,
}

#[derive(Serialize)]
struct CacheControl {
    #[serde(rename = "type")]
    kind: &'static str,
}

impl<'a> RequestBody<'a> {
    fn new(
        model: &'a str,
        max_tokens: u32,
        conversation: &'a Conversation,
        tools: Vec<serde_json::Value>,
    ) -> Self {
        Self {
            model,
            max_tokens,
            system: if conversation.system.is_empty() {
                Vec::new()
            } else {
                vec![SystemBlock {
                    kind: "text",
                    text: &conversation.system,
                    cache_control: CacheControl { kind: "ephemeral" },
                }]
            },
            messages: conversation.messages.iter().map(to_wire_message).collect(),
            tools: (!tools.is_empty()).then_some(tools),
            stream: None,
            metadata: CACHE_KEY.get().map(|key| Metadata {
                user_id: key.as_str(),
            }),
        }
    }
}

#[derive(Serialize)]
struct WireMessage {
    role: &'static str,
    content: Vec<WireBlock>,
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum WireBlock {
    Image {
        source: ImageSource,
    },
    Text {
        text: String,
    },
    ToolUse {
        id: String,
        name: String,
        input: Value,
    },
    ToolResult {
        tool_use_id: String,
        content: String,
        #[serde(default)]
        is_error: bool,
    },
    Thinking {
        thinking: String,
        #[serde(default)]
        signature: String,
    },
    RedactedThinking {
        data: String,
    },
    /// A response block type this module does not send and does not act
    /// on -- 61D's sandbox does not exist, so a `tool_use` block here is
    /// data to ignore, never something to run.
    #[serde(other)]
    Other,
}

#[derive(Serialize, Deserialize)]
struct ImageSource {
    #[serde(rename = "type")]
    kind: String,
    media_type: String,
    data: String,
}

/// Serialises `conversation` into the JSON body of an Anthropic Messages
/// request. Takes no base URL, no headers, and nothing environment-derived:
/// that is what makes the byte-identity in `the_gateway_hop_changes_no_byte`
/// hold structurally rather than by care.
pub fn request_body(conversation: &Conversation) -> Vec<u8> {
    request_body_on_model(conversation, MODEL)
}

/// The same request body used for sending and estimating an explicitly selected model.
pub fn request_body_on_model(conversation: &Conversation, model: &str) -> Vec<u8> {
    request_body_configured(conversation, model, Effort::Auto)
}

/// Which tool definitions a request carries — `tool-abi.md` §3.
///
/// A visibility choice and nothing else. Every variant reaches the same
/// kernel, so this type decides what the model is *shown* and never how the
/// work runs — which is what makes an interface benchmark measure the
/// interface.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Surface {
    /// A request that may act, and the façade it acts through.
    Acting {
        interface: crate::abi::Interface,
        dialect: crate::abi::Dialect,
    },
}

impl Surface {
    /// `execute_cell` and nothing else.
    ///
    /// The surface for a subagent, because a dialect row advertises a
    /// capability a subagent may not bind, and a declared-but-absent tool is
    /// the one failure its narrowing exists to avoid.
    #[must_use]
    pub fn cells() -> Self {
        Self::Acting {
            interface: crate::abi::Interface::Cells,
            dialect: crate::abi::Dialect::Anthropic,
        }
    }

    /// The tool definitions this surface declares, in declaration order.
    ///
    /// The cache breakpoint sits on the **last** definition so the whole
    /// tool prefix is one cacheable block (`model-contract.md` §8). With a
    /// cells-only surface that is `execute_cell`, exactly as before.
    #[must_use]
    pub fn tool_definitions(self) -> Vec<serde_json::Value> {
        let Self::Acting { interface, dialect } = self;
        let mut tools = Vec::new();
        if interface.declares_cell() {
            tools.push(serde_json::json!({
                "name": crate::prompt::declarations::EXECUTE_CELL_NAME,
                "description": crate::prompt::declarations::execute_cell_description(interface),
                "input_schema": {
                    "type": "object",
                    "properties": {
                        "code": {"type": "string"},
                        "description": {
                            "type": "string",
                            "description": "One short line, in the person's language, saying what this cell is for. It is shown to the person above the cell and stays in your own context after compaction.",
                        },
                    },
                    // Both are required so the model writes the line every
                    // time; the parser accepts its absence regardless, so a
                    // model that omits it still runs (`legibility.md` §2).
                    "required": ["code", "description"],
                    "additionalProperties": false,
                },
            }));
        }
        if interface.declares_direct_tools() {
            tools.extend(
                dialect
                    .shapes()
                    .iter()
                    .map(super::abi::Shape::tool_definition),
            );
        }
        if let Some(last) = tools.last_mut()
            && let Some(object) = last.as_object_mut()
        {
            object.insert(
                "cache_control".into(),
                serde_json::json!({"type": "ephemeral"}),
            );
        }
        tools
    }
}

pub fn request_body_configured(
    conversation: &Conversation,
    model: &str,
    effort: Effort,
) -> Vec<u8> {
    request_body_for_surface(conversation, model, effort, Surface::cells())
}

/// [`request_body_configured`] for a caller that knows its own tool surface.
///
/// The task path passes the session's; a subagent keeps [`Surface::cells`],
/// because it binds fewer capabilities than a dialect row would advertise.
pub fn request_body_for_surface(
    conversation: &Conversation,
    model: &str,
    effort: Effort,
    surface: Surface,
) -> Vec<u8> {
    let max_tokens = max_tokens_for(model);
    configure_effort(
        build_request_body(model, max_tokens, conversation, surface),
        model,
        effort,
        max_tokens,
    )
}

/// The reasoning budget one effort level asks for.
fn effort_budget(effort: Effort) -> u32 {
    match effort {
        Effort::Low => 4096,
        Effort::Medium => 16384,
        Effort::High => 32769,
        Effort::Xhigh => 49152,
        // `Default` never reaches here through [`configure_effort`]; this arm
        // is `Max` and the compiler cannot see that, so it is spelled rather
        // than a wildcard that would silently absorb a sixth level.
        Effort::Max | Effort::Auto => 65536,
    }
}

/// The effort word on every request, and on a translated leg a thinking
/// budget with `response_tokens` of room for the answer above it.
fn configure_effort(body: Vec<u8>, model: &str, effort: Effort, response_tokens: u32) -> Vec<u8> {
    if effort == Effort::Auto {
        return body;
    }
    let mut value: serde_json::Value = serde_json::from_slice(&body).expect("serialized request");
    // The word, always. It is the only form that distinguishes all five
    // levels: a token budget saturates, so `high`, `xhigh` and `max` used to
    // arrive as one thing and two of the three the user picked did not exist
    // on the wire at all.
    value["output_config"] = serde_json::json!({"effort": effort.name()});
    if !model.contains("claude") {
        // A budget as well, for the Anthropic-shaped leg that reads one --
        // Glasshouse's codec keeps both and gives each target the form it
        // uses.
        // Leave response space above the thinking allocation.
        let budget = effort_budget(effort);
        value["max_tokens"] = serde_json::json!(budget + response_tokens);
        value["thinking"] = serde_json::json!({"type":"enabled", "budget_tokens":budget});
    }
    serde_json::to_vec(&value).expect("serialized request")
}

fn to_wire_message(message: &Message) -> WireMessage {
    WireMessage {
        role: message.role.as_str(),
        content: message.content.iter().map(to_wire_block).collect(),
    }
}

fn to_wire_block(block: &Block) -> WireBlock {
    match block {
        Block::Image { media_type, data } => WireBlock::Image {
            source: ImageSource {
                kind: "base64".into(),
                media_type: media_type.clone(),
                data: data.clone(),
            },
        },
        Block::Text(text) => WireBlock::Text { text: text.clone() },
        Block::ToolUse { id, name, input } => WireBlock::ToolUse {
            id: id.clone(),
            name: name.clone(),
            input: input.clone(),
        },
        Block::ToolResult {
            tool_use_id,
            content,
            is_error,
        } => WireBlock::ToolResult {
            tool_use_id: tool_use_id.clone(),
            content: content.clone(),
            is_error: *is_error,
        },
        Block::Thinking {
            thinking,
            signature,
        } => WireBlock::Thinking {
            thinking: thinking.clone(),
            signature: signature.clone(),
        },
        Block::RedactedThinking { data } => WireBlock::RedactedThinking { data: data.clone() },
    }
}

#[derive(Deserialize)]
struct ResponseBody {
    role: String,
    content: Vec<WireBlock>,
    #[serde(default)]
    stop_reason: Option<String>,
    #[serde(default)]
    usage: Option<UsageRow>,
}

/// The Messages response's own `usage` object, before the "absent or
/// malformed is `None`, never zero" rule is applied -- either field can be
/// missing without the object itself being.
#[derive(Debug, Clone, Copy, Deserialize)]
struct UsageRow {
    #[serde(default)]
    input_tokens: Option<u64>,
    #[serde(default)]
    output_tokens: Option<u64>,
    #[serde(default, deserialize_with = "optional_cache_count")]
    cache_read_input_tokens: Option<u64>,
    #[serde(default, deserialize_with = "optional_cache_count")]
    cache_creation_input_tokens: Option<u64>,
}

// Optional provider extensions must not make an otherwise valid reply fail.
// A malformed count is unknown; never coerce strings or negatives into a hit.
fn optional_cache_count<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<u64>, D::Error> {
    Ok(Value::deserialize(deserializer)?.as_u64())
}

/// The provider's own token count for the request that produced one turn.
/// `model-contract.md` §6 reads this "rather than estimated" when there is
/// nothing else to prefer -- see [`Turn::usage`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Usage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    /// Provider-reported cache reads; `None` is unknown, `Some(0)` is no hit.
    pub cache_read_input_tokens: Option<u64>,
    /// Provider-reported cache writes, separate from both reads and input tokens.
    pub cache_creation_input_tokens: Option<u64>,
}

impl Usage {
    /// Every provider-reported token class consumed by this exchange.
    /// Anthropic reports uncached input, cache reads, and cache creation as
    /// disjoint fields, so omitting either cache class would make a warm
    /// session appear to consume fewer context tokens than it actually did.
    pub fn total_tokens(&self) -> u64 {
        self.input_tokens
            .saturating_add(self.output_tokens)
            .saturating_add(self.cache_read_input_tokens.unwrap_or(0))
            .saturating_add(self.cache_creation_input_tokens.unwrap_or(0))
    }
}

/// One assistant turn: the message the runtime and rollout act on, plus the
/// provider's own usage for the request that produced it.
///
/// `usage` is `None` whenever the response carried no `usage` object, or one
/// missing either field -- never a fabricated zero, because a zero here would
/// read as "the provider reported no tokens" rather than "it reported
/// nothing".
#[derive(Debug)]
pub struct Turn {
    pub message: Message,
    pub usage: Option<Usage>,
    /// The provider stopped at its output ceiling, and this turn is what
    /// survived it.
    ///
    /// The invariant: **a turn is truncated or it is not, and the blocks it
    /// carries are complete either way.** A response that runs out of room
    /// after finishing a tool call has produced a program the model really
    /// wrote, and destroying it costs the whole turn for nothing --
    /// `parse_response` and [`Streamed::finish`] therefore deliver every
    /// block they can prove finished and set this. What is *not* proven
    /// finished is dropped, never repaired: a truncated `input` that happens
    /// to parse would be a program nobody wrote.
    ///
    /// A caller that appends this turn owes the person and the model a word
    /// about it; the rest of the plan the model was writing did not arrive.
    pub truncated: bool,
}

fn to_usage(row: Option<UsageRow>) -> Option<Usage> {
    let row = row?;
    Some(Usage {
        input_tokens: row.input_tokens?,
        output_tokens: row.output_tokens?,
        cache_read_input_tokens: row.cache_read_input_tokens,
        cache_creation_input_tokens: row.cache_creation_input_tokens,
    })
}

/// The most of a response body an error will ever carry, in bytes.
const BODY_HEAD_LIMIT: usize = 240;

/// Everything that can go wrong sending or parsing one turn.
#[derive(Debug)]
pub enum WireError {
    /// The request could not reach a server at all: DNS, a refused
    /// connection, or any other transport-level failure below HTTP status.
    Http(Box<ureq::Error>),
    /// The server answered with a non-2xx status. `body_head` is the first
    /// `BODY_HEAD_LIMIT` bytes of its response body, escaped onto one
    /// line -- never anything from the request.
    Status { status: u16, body_head: String },
    /// The response body was not the JSON shape a Messages response has.
    Json(serde_json::Error),
    /// The response parsed, but its `role` was not `"assistant"`.
    UnexpectedRole(String),
    /// A streamed response ended without a complete reply, or carried an
    /// `error` event of its own. Distinct from [`WireError::Json`] because
    /// the bytes parsed and the *stream* was wrong.
    Stream(String),
    /// The provider exhausted its generation budget. Partial prose remains
    /// inspectable as diagnostic data, never as a completed assistant turn.
    IncompleteResponse { partial_text: String },
}

impl WireError {
    /// Whether this is the provider saying the conversation no longer fits.
    ///
    /// The invariant: **an overflow is a state to recover from, not a
    /// failure to report.** Before this existed a conversation that outgrew
    /// the window became `Status { 400 }`, the task ended, and the session
    /// was over — the one failure a long task is guaranteed to reach.
    ///
    /// Matched on the message rather than a code because there is no
    /// distinct code: every provider here answers 400, and only the body
    /// separates "too long" from "malformed". The phrases are the ones
    /// Anthropic and the OpenAI-compatible gateways actually send; an
    /// unrecognised 400 stays an ordinary error rather than being retried
    /// as an overflow, so a mis-match costs a report and never a loop.
    pub fn is_context_overflow(&self) -> bool {
        let WireError::Status { status, body_head } = self else {
            return false;
        };
        if !matches!(status, 400 | 413) {
            return false;
        }
        let body = body_head.to_ascii_lowercase();
        [
            "prompt is too long",
            "context length",
            "context_length_exceeded",
            "maximum context",
            "too many tokens",
            "exceeds the maximum",
            "input length and `max_tokens` exceed",
        ]
        .iter()
        .any(|phrase| body.contains(phrase))
    }
}

impl fmt::Display for WireError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            WireError::Http(err) => write!(f, "request failed: {err}"),
            WireError::Status { status, body_head } => {
                write!(f, "http status: {status} — {body_head}")
            }
            WireError::Json(err) => write!(f, "could not parse response: {err}"),
            WireError::UnexpectedRole(role) => write!(f, "unexpected response role {role:?}"),
            WireError::Stream(what) => write!(f, "the stream ended without a reply: {what}"),
            WireError::IncompleteResponse { partial_text } => {
                write!(
                    f,
                    "provider stopped at max_tokens; response incomplete and no code from this response was executed"
                )?;
                if !partial_text.is_empty() {
                    // Preserve the entire partial text while escaping terminal
                    // controls. It is a diagnostic, not executable input.
                    write!(f, "; partial assistant text: {partial_text:?}")?;
                }
                Ok(())
            }
        }
    }
}

impl std::error::Error for WireError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            WireError::Http(err) => Some(err),
            WireError::Status { .. } => None,
            WireError::Json(err) => Some(err),
            WireError::UnexpectedRole(_) => None,
            WireError::Stream(_) => None,
            WireError::IncompleteResponse { .. } => None,
        }
    }
}

/// Renders a provider's response body as an error's `body_head`: the first
/// `BODY_HEAD_LIMIT` bytes, cut on a char boundary, with control
/// characters escaped so the whole thing prints on one line, `…` appended
/// when the body was longer, and a fixed placeholder for an empty body.
fn body_head(body: &str) -> String {
    if body.is_empty() {
        return "(empty body)".to_string();
    }
    let mut cut = body.len().min(BODY_HEAD_LIMIT);
    while !body.is_char_boundary(cut) {
        cut -= 1;
    }
    let truncated = cut < body.len();
    let mut head: String = body[..cut]
        .chars()
        .flat_map(|c| match c {
            '\n' => "\\n".chars().collect::<Vec<_>>(),
            '\r' => "\\r".chars().collect::<Vec<_>>(),
            '\t' => "\\t".chars().collect::<Vec<_>>(),
            c if c.is_control() => format!("\\u{{{:x}}}", c as u32).chars().collect::<Vec<_>>(),
            c => vec![c],
        })
        .collect();
    if truncated {
        head.push('…');
    }
    head
}

/// The credential sterna attaches to a request, and the header it goes in.
///
/// `ANTHROPIC_AUTH_TOKEN` carries a bearer token (the shape a gateway hands
/// out); `ANTHROPIC_API_KEY` carries a provider key sent as `x-api-key`.
/// Neither is read by any test -- see the packet's SECURITY section.
///
/// `pub(crate)` rather than private: `decide.rs` attaches the same
/// credential to the decision request, since it is the only credential path
/// and it is this module's.
pub(crate) fn credential_header() -> Option<(&'static str, String)> {
    if let Ok(token) = env::var("ANTHROPIC_AUTH_TOKEN")
        && !token.is_empty()
    {
        return Some(("Authorization", format!("Bearer {token}")));
    }
    if let Ok(key) = env::var("ANTHROPIC_API_KEY")
        && !key.is_empty()
    {
        return Some(("x-api-key", key));
    }
    None
}

/// Sends `conversation` as one Anthropic Messages request to [`base_url`]
/// and returns the assistant's reply. Blocking, and it does not stream --
/// see the packet's OBJECTIVE for why a streaming reader is out of scope
/// here.
///
/// `http_status_as_error(false)` turns off `ureq`'s default of folding a
/// non-2xx status into `Err(ureq::Error::StatusCode)` before the body can be
/// read at all -- with it on, [`WireError::Status`]'s `body_head` would
/// always be empty. With it off, `send` only errors on an actual transport
/// failure, and status is read and handled here instead.
pub fn send_turn(conversation: &Conversation) -> Result<Turn, WireError> {
    send_turn_on_model(conversation, MODEL)
}

/// A task request with its active model, preserving provider usage accounting.
pub fn send_turn_on_model(conversation: &Conversation, model: &str) -> Result<Turn, WireError> {
    send_turn_configured(conversation, model, Effort::Auto)
}
pub fn send_turn_configured(
    conversation: &Conversation,
    model: &str,
    effort: Effort,
) -> Result<Turn, WireError> {
    send_turn_bounded(conversation, model, effort, None)
}

/// A task turn, optionally bounded by a whole-answer ceiling. The task path
/// and a subagent pass `None`: a person is watching one, and the other's
/// parent can cancel it.
pub fn send_turn_bounded(
    conversation: &Conversation,
    model: &str,
    effort: Effort,
    timeout: Option<std::time::Duration>,
) -> Result<Turn, WireError> {
    send_turn_bounded_on(conversation, model, effort, timeout, None, Surface::cells())
}

/// [`send_turn_bounded`] for a caller that knows its own tool surface.
pub fn send_turn_bounded_on(
    conversation: &Conversation,
    model: &str,
    effort: Effort,
    timeout: Option<std::time::Duration>,
    extra_header: Option<(&str, &str)>,
    surface: Surface,
) -> Result<Turn, WireError> {
    send_turn_bounded_routed(
        conversation,
        model,
        effort,
        timeout,
        extra_header,
        surface,
        None,
    )
}

/// [`send_turn_bounded_on`] for the main model's own task requests, which
/// carry and keep the task's [`TurnRouting`].
pub fn send_turn_bounded_routed(
    conversation: &Conversation,
    model: &str,
    effort: Effort,
    timeout: Option<std::time::Duration>,
    extra_header: Option<(&str, &str)>,
    surface: Surface,
    routing: Option<&TurnRouting>,
) -> Result<Turn, WireError> {
    let url = format!("{}{MESSAGES_PATH}", base_url());
    let mut body = request_body_for_surface(conversation, model, effort, surface);
    if let Some(routing) = routing {
        if routing.wants_summary() {
            body = with_reasoning_summary(body, model);
        }
        body = with_history_breakpoint(body, model);
    }

    let mut builder = ureq::post(&url).config().http_status_as_error(false);
    if let Some(timeout) = timeout {
        builder = builder.timeout_global(Some(timeout));
    }
    let mut request = builder
        .build()
        .header("content-type", "application/json")
        .header("anthropic-version", ANTHROPIC_VERSION)
        .header(MODEL_HEADER, model);
    if let Some(key) = CACHE_KEY.get() {
        request = request.header(SESSION_HEADER, key.as_str());
    }
    if let Some((name, value)) = credential_header() {
        request = request.header(name, value);
    }
    if let Some((name, value)) = extra_header {
        request = request.header(name, value);
    }
    if let Some(token) = routing.and_then(TurnRouting::token) {
        request = request.header(TURN_STATE_HEADER, token);
    }

    let mut response = request
        .send(body.as_slice())
        .map_err(|err| WireError::Http(Box::new(err)))?;
    if let Some(routing) = routing {
        routing.observe(response.headers());
    }
    let status = response.status().as_u16();
    let text = response
        .body_mut()
        .read_to_string()
        .map_err(|err| WireError::Http(Box::new(err)))?;
    if !response.status().is_success() {
        return Err(WireError::Status {
            status,
            body_head: body_head(&text),
        });
    }
    parse_response(&text)
}

/// Shared serialization for task requests.
fn build_request_body(
    model: &str,
    max_tokens: u32,
    conversation: &Conversation,
    surface: Surface,
) -> Vec<u8> {
    let body = RequestBody::new(model, max_tokens, conversation, surface.tool_definitions());
    serde_json::to_vec(&body).expect("Conversation has no non-serialisable field")
}

fn parse_response(text: &str) -> Result<Turn, WireError> {
    let parsed: ResponseBody = serde_json::from_str(text).map_err(WireError::Json)?;
    if parsed.role != "assistant" {
        return Err(WireError::UnexpectedRole(parsed.role));
    }
    let truncated = parsed.stop_reason.as_deref() == Some("max_tokens");
    if truncated {
        // **Only the last block was cut.** Generation stops where the budget
        // runs out, so every block before the final one was finished before
        // the ceiling was reached; the final one is the casualty. Nothing
        // here inspects a block's *contents* to judge it -- a truncated
        // `input` object that still parses is exactly the case that inference
        // cannot see, and position is a fact rather than a guess.
        let finished = parsed.content.len().saturating_sub(1);
        let salvaged = deliver(parsed.content.iter().take(finished))?;
        // **Prose alone is not worth salvaging, and both paths agree on
        // that.** There is nothing to run in it, and appending a reply that
        // stops mid-thought is the outcome `Streamed::finish` was written to
        // prevent. What this exists to rescue is a finished program, so a
        // salvage without one is the error it always was.
        if !salvaged
            .iter()
            .any(|block| matches!(block, Block::ToolUse { .. }))
        {
            return Err(WireError::IncompleteResponse {
                partial_text: parsed
                    .content
                    .iter()
                    .filter_map(|block| match block {
                        WireBlock::Text { text } => Some(text.as_str()),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
                    .join("\n"),
            });
        }
        return Ok(Turn {
            message: Message {
                role: Role::Assistant,
                content: salvaged,
                historical: None,
            },
            usage: to_usage(parsed.usage),
            truncated: true,
        });
    }
    let content = deliver(parsed.content.iter())?;
    Ok(Turn {
        message: Message {
            role: Role::Assistant,
            content,
            historical: None,
        },
        usage: to_usage(parsed.usage),
        truncated: false,
    })
}

/// The assistant blocks a non-streamed response hands to the session.
///
/// One reader for both the whole response and the salvaged prefix of a
/// truncated one, so the two cannot come to disagree about what an assistant
/// block is or which of them are refused outright.
fn deliver<'a>(blocks: impl Iterator<Item = &'a WireBlock>) -> Result<Vec<Block>, WireError> {
    let mut content = Vec::new();
    for block in blocks {
        match block {
            WireBlock::Text { text } => content.push(Block::Text(text.clone())),
            WireBlock::ToolUse { id, name, input } => content.push(Block::ToolUse {
                id: id.clone(),
                name: name.clone(),
                input: input.clone(),
            }),
            WireBlock::Image { .. } => {
                return Err(WireError::Stream(
                    "assistant response contained a user-only image block".into(),
                ));
            }
            WireBlock::ToolResult { .. } => {
                return Err(WireError::Stream(
                    "assistant response contained a user-only tool_result block".into(),
                ));
            }
            // Unsigned reasoning cannot be sent back, so it is not kept.
            WireBlock::Thinking {
                thinking,
                signature,
            } if !signature.is_empty() => content.push(Block::Thinking {
                thinking: thinking.clone(),
                signature: signature.clone(),
            }),
            WireBlock::RedactedThinking { data } => {
                content.push(Block::RedactedThinking { data: data.clone() })
            }
            WireBlock::Thinking { .. } | WireBlock::Other => {}
        }
    }
    Ok(content)
}

/// Builds one [`Turn`] out of a Messages stream, one `data:` payload at a
/// time.
///
/// The invariant: **a streamed turn and a whole-response turn are the same
/// value.** The session and the rollout see no difference,
/// so streaming stays a transport concern and nothing downstream branches on
/// it. Pure over its input and holding no socket, so the parse is tested
/// without a server.
#[derive(Debug, Default)]
pub struct StreamAccumulator {
    blocks: BTreeMap<u64, PendingBlock>,
    usage: Option<UsageRow>,
    saw_stop: bool,
    stopped_at_max_tokens: bool,
}

#[derive(Debug)]
enum PendingBlock {
    Text(String),
    ToolUse {
        id: String,
        name: String,
        input: Value,
        partial: String,
        saw_delta: bool,
        stopped: bool,
    },
    Thinking {
        thinking: String,
        signature: String,
    },
    RedactedThinking(String),
    Ignored,
}

/// Presentation-only progress from a stream. Recorded messages retain typed
/// blocks; callers must not append these fragments to conversation history.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StreamDelta {
    Text(String),
    /// Raw incremental JSON input bytes, for progress only. Never render as
    /// code and never append to conversation history.
    ToolInput(String),
    /// Complete decoded source after the provider closed valid tool input.
    ToolReady(String),
    /// The model's readable reasoning (a summary behind the subscription
    /// broker) as it arrives. Shown live, never appended as text; the block
    /// itself goes back with the next request.
    Reasoning(String),
}

impl StreamAccumulator {
    pub fn new() -> Self {
        Self::default()
    }

    /// Feeds one SSE `data:` payload and returns presentation progress. Tool
    /// source appears only once its complete JSON input has validated.
    ///
    /// An event this does not know is ignored rather than refused — the
    /// Messages stream gains event types over time, and a `ping` or a
    /// `thinking` block is not a reason to fail a turn that is arriving
    /// correctly.
    pub fn event(&mut self, data: &str) -> Result<Option<StreamDelta>, WireError> {
        let value: serde_json::Value = serde_json::from_str(data).map_err(WireError::Json)?;
        match value.get("type").and_then(|t| t.as_str()) {
            Some("content_block_start") => {
                let index = value
                    .get("index")
                    .and_then(Value::as_u64)
                    .ok_or_else(|| WireError::Stream("content block start has no index".into()))?;
                let block = value
                    .get("content_block")
                    .ok_or_else(|| WireError::Stream("content block start has no block".into()))?;
                let pending = match block.get("type").and_then(Value::as_str) {
                    Some("text") => PendingBlock::Text(
                        block
                            .get("text")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .into(),
                    ),
                    Some("tool_use") => PendingBlock::ToolUse {
                        id: block
                            .get("id")
                            .and_then(Value::as_str)
                            .ok_or_else(|| WireError::Stream("tool_use has no id".into()))?
                            .into(),
                        name: block
                            .get("name")
                            .and_then(Value::as_str)
                            .ok_or_else(|| WireError::Stream("tool_use has no name".into()))?
                            .into(),
                        input: block
                            .get("input")
                            .cloned()
                            .unwrap_or_else(|| serde_json::json!({})),
                        partial: String::new(),
                        saw_delta: false,
                        stopped: false,
                    },
                    Some("tool_result") => {
                        return Err(WireError::Stream(
                            "assistant stream contained a user-only tool_result block".into(),
                        ));
                    }
                    Some("thinking") => PendingBlock::Thinking {
                        thinking: block
                            .get("thinking")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .into(),
                        signature: block
                            .get("signature")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .into(),
                    },
                    Some("redacted_thinking") => PendingBlock::RedactedThinking(
                        block
                            .get("data")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .into(),
                    ),
                    _ => PendingBlock::Ignored,
                };
                if self.blocks.insert(index, pending).is_some() {
                    return Err(WireError::Stream(format!(
                        "duplicate content block index {index}"
                    )));
                }
                Ok(None)
            }
            Some("content_block_delta") => {
                // Reasoning is kept whole for the next request and never
                // shown as text: its deltas return no presentation progress.
                let delta = value.get("delta");
                let kind = delta.and_then(|d| d.get("type")).and_then(Value::as_str);
                if let Some((field, is_signature)) = match kind {
                    Some("thinking_delta") => Some(("thinking", false)),
                    Some("signature_delta") => Some(("signature", true)),
                    _ => None,
                } {
                    let index = value.get("index").and_then(Value::as_u64).unwrap_or(0);
                    let fragment = delta
                        .and_then(|d| d.get(field))
                        .and_then(Value::as_str)
                        .unwrap_or_default();
                    if let Some(PendingBlock::Thinking {
                        thinking,
                        signature,
                    }) = self.blocks.get_mut(&index)
                    {
                        if is_signature {
                            signature.push_str(fragment);
                        } else {
                            thinking.push_str(fragment);
                            if !fragment.is_empty() {
                                return Ok(Some(StreamDelta::Reasoning(fragment.to_string())));
                            }
                        }
                    }
                    return Ok(None);
                }
                let is_text = delta
                    .and_then(|d| d.get("type"))
                    .and_then(|t| t.as_str())
                    .is_some_and(|t| t == "text_delta");
                let index = value.get("index").and_then(Value::as_u64).unwrap_or(0);
                if is_text {
                    let Some(text) = delta.and_then(|d| d.get("text")).and_then(|t| t.as_str())
                    else {
                        return Ok(None);
                    };
                    match self
                        .blocks
                        .entry(index)
                        .or_insert_with(|| PendingBlock::Text(String::new()))
                    {
                        PendingBlock::Text(held) => held.push_str(text),
                        _ => {
                            return Err(WireError::Stream(format!(
                                "text delta targets non-text block {index}"
                            )));
                        }
                    }
                    return Ok(Some(StreamDelta::Text(text.to_string())));
                }
                if delta.and_then(|d| d.get("type")).and_then(Value::as_str)
                    == Some("input_json_delta")
                {
                    let fragment = delta
                        .and_then(|d| d.get("partial_json"))
                        .and_then(Value::as_str)
                        .ok_or_else(|| {
                            WireError::Stream("input_json_delta has no partial_json".into())
                        })?;
                    match self.blocks.get_mut(&index) {
                        Some(PendingBlock::ToolUse {
                            stopped: false,
                            partial,
                            saw_delta,
                            ..
                        }) => {
                            partial.push_str(fragment);
                            *saw_delta = true;
                        }
                        Some(PendingBlock::ToolUse { stopped: true, .. }) => {
                            return Err(WireError::Stream(format!(
                                "input delta arrived after tool_use block {index} stopped"
                            )));
                        }
                        _ => {
                            return Err(WireError::Stream(format!(
                                "input delta targets no tool_use block {index}"
                            )));
                        }
                    }
                    return Ok(Some(StreamDelta::ToolInput(fragment.to_string())));
                }
                Ok(None)
            }
            Some("content_block_stop") => {
                let index = value
                    .get("index")
                    .and_then(Value::as_u64)
                    .ok_or_else(|| WireError::Stream("content block stop has no index".into()))?;
                if let Some(PendingBlock::ToolUse {
                    input,
                    partial,
                    saw_delta,
                    stopped,
                    ..
                }) = self.blocks.get_mut(&index)
                {
                    if *stopped {
                        return Err(WireError::Stream(format!(
                            "duplicate stop for tool_use block {index}"
                        )));
                    }
                    if *saw_delta {
                        *input = serde_json::from_str(partial).map_err(|_| {
                            WireError::Stream(format!(
                                "tool_use block {index} ended with malformed input JSON"
                            ))
                        })?;
                    }
                    *stopped = true;
                    return Ok(input
                        .get("code")
                        .and_then(Value::as_str)
                        .map(|code| StreamDelta::ToolReady(code.to_string())));
                }
                Ok(None)
            }
            Some("message_start") => {
                if let Some(role) = value
                    .get("message")
                    .and_then(|m| m.get("role"))
                    .and_then(|r| r.as_str())
                    && role != "assistant"
                {
                    return Err(WireError::UnexpectedRole(role.to_string()));
                }
                self.read_usage(value.get("message").and_then(|m| m.get("usage")));
                Ok(None)
            }
            // The final `usage` lands here, and it is the one that carries
            // the output tokens; `message_start`'s is a header with zeroes.
            Some("message_delta") => {
                if value
                    .get("delta")
                    .and_then(|d| d.get("stop_reason"))
                    .and_then(Value::as_str)
                    == Some("max_tokens")
                {
                    self.stopped_at_max_tokens = true;
                }
                self.read_usage(value.get("usage"));
                Ok(None)
            }
            Some("message_stop") => {
                self.saw_stop = true;
                Ok(None)
            }
            Some("error") => Err(WireError::Stream(body_head(data))),
            _ => Ok(None),
        }
    }

    /// Merges a `usage` object, keeping whichever field actually carried a
    /// number: the stream reports input tokens at the start and output
    /// tokens at the end, and neither message has both.
    fn read_usage(&mut self, usage: Option<&serde_json::Value>) {
        let Some(usage) = usage else { return };
        let Ok(row) = serde_json::from_value::<UsageRow>(usage.clone()) else {
            return;
        };
        let held = self.usage.take().unwrap_or(UsageRow {
            input_tokens: None,
            output_tokens: None,
            cache_read_input_tokens: None,
            cache_creation_input_tokens: None,
        });
        self.usage = Some(UsageRow {
            input_tokens: row.input_tokens.or(held.input_tokens),
            output_tokens: row.output_tokens.or(held.output_tokens),
            cache_read_input_tokens: row.cache_read_input_tokens.or(held.cache_read_input_tokens),
            cache_creation_input_tokens: row
                .cache_creation_input_tokens
                .or(held.cache_creation_input_tokens),
        });
    }

    /// The turn the stream carried.
    ///
    /// **A stream that stopped early is an error, not a short reply.** A
    /// connection cut mid-reply would otherwise become a truncated answer the
    /// session appends and treats as final.
    pub fn finish(self) -> Result<Turn, WireError> {
        if !self.saw_stop {
            return Err(WireError::Stream(
                "no message_stop arrived; the connection ended mid-reply".to_string(),
            ));
        }
        if self.stopped_at_max_tokens {
            // **`stopped` is proof, not an inference.** It is set in exactly
            // one place -- `content_block_stop`, where the accumulated
            // `input` is parsed and a malformed one is already an error -- so
            // a tool block carrying it is one the provider finished sending.
            // A block without it is dropped whatever its `partial` looks
            // like: a cut `input` that happens to parse is a program the
            // model never wrote, and running it is worse than losing the
            // turn. Text is kept as the model sent it; it is prose, it is
            // never executed, and `truncated` is what tells the reader it
            // stops mid-thought.
            let mut salvaged = Vec::new();
            for block in self.blocks.values() {
                match block {
                    PendingBlock::Text(text) if !text.is_empty() => {
                        salvaged.push(Block::Text(text.clone()));
                    }
                    PendingBlock::ToolUse {
                        id,
                        name,
                        input,
                        stopped: true,
                        ..
                    } => salvaged.push(Block::ToolUse {
                        id: id.clone(),
                        name: name.clone(),
                        input: input.clone(),
                    }),
                    other => salvaged.extend(reasoning(other)),
                }
            }
            if !salvaged
                .iter()
                .any(|block| matches!(block, Block::ToolUse { .. }))
            {
                return Err(WireError::IncompleteResponse {
                    partial_text: self
                        .blocks
                        .values()
                        .filter_map(|block| match block {
                            PendingBlock::Text(text) => Some(text.as_str()),
                            _ => None,
                        })
                        .collect::<Vec<_>>()
                        .join("\n"),
                });
            }
            return Ok(Turn {
                message: Message {
                    role: Role::Assistant,
                    content: salvaged,
                    historical: None,
                },
                usage: to_usage(self.usage),
                truncated: true,
            });
        }
        let mut content = Vec::new();
        for (index, block) in self.blocks {
            match block {
                PendingBlock::Text(text) if !text.is_empty() => content.push(Block::Text(text)),
                PendingBlock::ToolUse {
                    id,
                    name,
                    input,
                    stopped: true,
                    ..
                } => content.push(Block::ToolUse { id, name, input }),
                PendingBlock::ToolUse { .. } => {
                    return Err(WireError::Stream(format!(
                        "tool_use block {index} never completed"
                    )));
                }
                other => content.extend(reasoning(&other)),
            }
        }
        Ok(Turn {
            message: Message {
                role: Role::Assistant,
                content,
                historical: None,
            },
            usage: to_usage(self.usage),
            truncated: false,
        })
    }
}

/// A finished stream block as the reasoning the next request sends back:
/// signed thinking or redacted thinking, nothing else. Unsigned reasoning
/// cannot be sent back, so it is not kept.
fn reasoning(block: &PendingBlock) -> Option<Block> {
    match block {
        PendingBlock::Thinking {
            thinking,
            signature,
        } if !signature.is_empty() => Some(Block::Thinking {
            thinking: thinking.clone(),
            signature: signature.clone(),
        }),
        PendingBlock::RedactedThinking(data) => {
            Some(Block::RedactedThinking { data: data.clone() })
        }
        _ => None,
    }
}

/// [`send_turn`] over a Server-Sent Events stream, calling `on_delta` with
/// each fragment of text as it arrives and returning the identical [`Turn`]
/// at the end.
///
/// The caller decides whether to stream; nothing here reads a setting. A
/// gateway that cannot stream is not detected and not fallen back to — the
/// session picks the path, so a failure is reported rather than silently
/// changing transport underneath a screen already drawing deltas.
pub fn send_turn_streaming(
    conversation: &Conversation,
    model: &str,
    on_delta: &mut dyn FnMut(StreamDelta),
) -> Result<Turn, WireError> {
    send_turn_streaming_configured(conversation, model, Effort::Auto, on_delta)
}
pub fn send_turn_streaming_configured(
    conversation: &Conversation,
    model: &str,
    effort: Effort,
    on_delta: &mut dyn FnMut(StreamDelta),
) -> Result<Turn, WireError> {
    send_turn_streaming_on(conversation, model, effort, Surface::cells(), on_delta)
}

/// [`send_turn_streaming_configured`] for a caller that knows its own tool
/// surface. `model-contract.md` §8 requires the streamed body to use the same
/// serializer and the same cache boundaries as the whole-response one, so the
/// surface reaches both through the same [`Surface::tool_definitions`].
pub fn send_turn_streaming_on(
    conversation: &Conversation,
    model: &str,
    effort: Effort,
    surface: Surface,
    on_delta: &mut dyn FnMut(StreamDelta),
) -> Result<Turn, WireError> {
    send_turn_streaming_while(
        conversation,
        model,
        effort,
        surface,
        None,
        &mut || true,
        on_delta,
    )
}

/// What a turn the person cancelled ends with.
pub const CANCELLED_TURN: &str = "the person cancelled the call in flight";

/// [`send_turn_streaming_on`], but one the caller can give up on at any
/// moment: the request runs on its own thread, deltas come back over a
/// channel, and `cancelled` is asked every [`CANCEL_POLL`].
///
/// **Giving up never waits on the socket.** A blocking read cannot be
/// interrupted, and a reasoning model sends nothing for minutes, so a check
/// between lines is a check that never runs — measured 2026-09-23: a second
/// Escape printed "Cancelling the call in flight" seven times while the
/// turn went on. The abandoned thread stops reading at its next line, which
/// drops the connection and ends the generation upstream.
pub fn send_turn_streaming_cancellable(
    conversation: Conversation,
    model: String,
    effort: Effort,
    surface: Surface,
    routing: Option<std::sync::Arc<TurnRouting>>,
    cancelled: &dyn Fn() -> bool,
    on_delta: &mut dyn FnMut(StreamDelta),
) -> Result<Turn, WireError> {
    enum Event {
        Delta(StreamDelta),
        Done(Result<Turn, WireError>),
    }
    let (events, received) = std::sync::mpsc::channel::<Event>();
    let abandoned = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let reader_abandoned = abandoned.clone();
    std::thread::spawn(move || {
        let deltas = events.clone();
        let result = send_turn_streaming_while(
            &conversation,
            &model,
            effort,
            surface,
            routing.as_deref(),
            &mut || !reader_abandoned.load(std::sync::atomic::Ordering::SeqCst),
            &mut |delta| {
                let _ = deltas.send(Event::Delta(delta));
            },
        );
        let _ = events.send(Event::Done(result));
    });
    loop {
        match received.recv_timeout(CANCEL_POLL) {
            Ok(Event::Delta(delta)) => on_delta(delta),
            Ok(Event::Done(result)) => return result,
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                if cancelled() {
                    abandoned.store(true, std::sync::atomic::Ordering::SeqCst);
                    return Err(WireError::Stream(CANCELLED_TURN.to_string()));
                }
            }
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                return Err(WireError::Stream(
                    "the turn's thread ended without an answer".to_string(),
                ));
            }
        }
    }
}

/// How often a cancellable turn asks whether it was given up on.
pub const CANCEL_POLL: std::time::Duration = std::time::Duration::from_millis(50);

fn send_turn_streaming_while(
    conversation: &Conversation,
    model: &str,
    effort: Effort,
    surface: Surface,
    routing: Option<&TurnRouting>,
    keep_reading: &mut dyn FnMut() -> bool,
    on_delta: &mut dyn FnMut(StreamDelta),
) -> Result<Turn, WireError> {
    let url = format!("{}{MESSAGES_PATH}", base_url());
    // **The same allowance the whole-response path asks for.** This read
    // `MAX_TOKENS` — the documented fallback — while
    // [`request_body_for_surface`] beside it asked `max_tokens_for(model)`,
    // so turning streaming on silently cut a turn to a sixteenth of what
    // every model this project runs publishes (128,000), and the truncation
    // arrived as a turn that stopped mid-sentence.
    let max_tokens = max_tokens_for(model);
    let mut body = RequestBody::new(model, max_tokens, conversation, surface.tool_definitions());
    body.stream = Some(true);
    let body = configure_effort(
        serde_json::to_vec(&body).expect("Conversation has no non-serialisable field"),
        model,
        effort,
        max_tokens,
    );
    let body = match routing {
        Some(routing) if routing.wants_summary() => {
            with_history_breakpoint(with_reasoning_summary(body, model), model)
        }
        Some(_) => with_history_breakpoint(body, model),
        None => body,
    };

    let mut request = ureq::post(&url)
        .config()
        .http_status_as_error(false)
        .build()
        .header("content-type", "application/json")
        .header("accept", "text/event-stream")
        .header("anthropic-version", ANTHROPIC_VERSION)
        .header(MODEL_HEADER, model);
    if let Some(key) = CACHE_KEY.get() {
        request = request.header(SESSION_HEADER, key.as_str());
    }
    if let Some((name, value)) = credential_header() {
        request = request.header(name, value);
    }
    if let Some(token) = routing.and_then(TurnRouting::token) {
        request = request.header(TURN_STATE_HEADER, token);
    }

    let mut response = request
        .send(body.as_slice())
        .map_err(|err| WireError::Http(Box::new(err)))?;
    if let Some(routing) = routing {
        routing.observe(response.headers());
    }
    let status = response.status().as_u16();
    if !response.status().is_success() {
        let text = response
            .body_mut()
            .read_to_string()
            .map_err(|err| WireError::Http(Box::new(err)))?;
        return Err(WireError::Status {
            status,
            body_head: body_head(&text),
        });
    }

    read_sse_stream(&mut response, keep_reading, on_delta)
}

/// The SSE half of a streamed turn, shared by the task path
/// ([`send_turn_streaming_on`]).
///
/// **`on_line` fires for every line the socket yields, `on_delta` only for
/// the ones that carry text.** The two are not the same signal and a caller
/// that confuses them measures the wrong thing: a provider sends
/// `message_start` immediately, then `ping` keepalives, then `thinking`
/// blocks that produce no delta at all. A reasoning model is therefore
/// *noisy* on the wire while it is silent in the transcript, which is
/// exactly what lets an inactivity ceiling tell working from hung.
/// `on_line` returns whether to keep reading: a caller that has already
/// stopped waiting says `false` and the socket is dropped at the next line,
/// rather than dribbling into a channel nobody holds.
fn read_sse_stream(
    response: &mut ureq::http::Response<ureq::Body>,
    on_line: &mut dyn FnMut() -> bool,
    on_delta: &mut dyn FnMut(StreamDelta),
) -> Result<Turn, WireError> {
    use std::io::{BufRead, BufReader};

    let mut accumulator = StreamAccumulator::new();
    let reader = BufReader::new(response.body_mut().as_reader());
    for line in reader.lines() {
        let line = line.map_err(|err| WireError::Http(Box::new(err.into())))?;
        if !on_line() {
            return Err(WireError::Stream(
                "the caller stopped waiting for this request".to_string(),
            ));
        }
        // SSE: `event:` names the type, `data:` carries it, a blank line ends
        // one event. Every payload here is self-describing by its own `type`
        // field, so only `data:` is read and the framing needs no state.
        let Some(payload) = line.strip_prefix("data:") else {
            continue;
        };
        let payload = payload.trim();
        if payload.is_empty() || payload == "[DONE]" {
            continue;
        }
        if let Some(delta) = accumulator.event(payload)? {
            on_delta(delta);
        }
    }
    accumulator.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Sessions share a few cache keys, so a new one lands where Sterna's
    /// prompt is already cached, and one session always keeps the same key.
    #[test]
    fn sessions_share_a_few_cache_keys_and_each_keeps_its_own() {
        let keys: std::collections::BTreeSet<String> = (0..200)
            .map(|n| cache_key_for(&format!("session-{n}")))
            .collect();
        assert_eq!(
            keys.into_iter().collect::<Vec<_>>(),
            ["sterna-0", "sterna-1", "sterna-2", "sterna-3"]
        );
        assert_eq!(cache_key_for("tm6xuq-shx"), cache_key_for("tm6xuq-shx"));
    }

    /// Each model starts at the fallback, is raised fourfold to a ceiling
    /// and no further.
    #[test]
    fn output_room_is_raised_fourfold_to_a_ceiling() {
        let model = "room-test-raise";
        assert_eq!(max_tokens_for(model), MAX_TOKENS);
        assert!(raise_output_room(model));
        assert_eq!(max_tokens_for(model), MAX_RAISED_TOKENS);
        assert!(!raise_output_room(model), "already at the most");
    }

    /// Once a provider refused a size, raising again would ask for it again,
    /// so a refused model is never raised: a refusal and a cut-off cannot
    /// alternate forever.
    #[test]
    fn a_model_whose_provider_refused_a_size_is_never_raised_again() {
        let model = "room-test-refused";
        assert!(lower_output_room(model));
        assert_eq!(max_tokens_for(model), REFUSED_SIZE_TOKENS);
        assert!(!raise_output_room(model));
        assert!(!lower_output_room(model), "already asks no more than that");
    }

    #[test]
    fn a_size_refusal_is_a_400_naming_the_output_field() {
        let status = |status, body: &str| WireError::Status {
            status,
            body_head: body.to_string(),
        };
        assert!(refused_size(&status(400, "max_tokens: 32768 > 8192")));
        assert!(refused_size(&status(
            400,
            "max_completion_tokens is too large"
        )));
        assert!(!refused_size(&status(
            400,
            "messages: roles must alternate"
        )));
        assert!(!refused_size(&status(
            500,
            "max_tokens overflow in upstream"
        )));
    }

    /// `ANTHROPIC_BASE_URL` is process-global, so the tests here that point
    /// it at a fixture serialise against each other exactly as `turns.rs`
    /// serialises its own. Nothing else in this
    /// crate's unit tests writes it.
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// A listener that answers one Messages request with SSE, writing each
    /// frame after `gap` and then, if `then_go_silent`, holding the socket
    /// open saying nothing. Returns its base URL.
    fn streaming_provider(
        frames: Vec<String>,
        gap: std::time::Duration,
        then_go_silent: bool,
    ) -> String {
        use std::io::{BufRead, BufReader, Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            let Ok((mut stream, _)) = listener.accept() else {
                return;
            };
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut length = 0usize;
            loop {
                let mut line = String::new();
                if reader.read_line(&mut line).unwrap_or(0) == 0 {
                    return;
                }
                if line == "\r\n" || line == "\n" {
                    break;
                }
                if let Some(value) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                    length = value.trim().parse().unwrap_or(0);
                }
            }
            let mut body = vec![0; length];
            if reader.read_exact(&mut body).is_err() {
                return;
            }
            if write!(
                stream,
                "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\nconnection: close\r\n\r\n"
            )
            .is_err()
            {
                return;
            }
            let _ = stream.flush();
            for frame in frames {
                std::thread::sleep(gap);
                if write!(stream, "data: {frame}\n\n").is_err() || stream.flush().is_err() {
                    return;
                }
            }
            if then_go_silent {
                // Hold the socket open, saying nothing. This is the shape the
                // ceiling exists for and the one a whole-response call cannot
                // tell apart from a model that is thinking.
                std::thread::sleep(std::time::Duration::from_secs(30));
            }
        });
        format!("http://{address}")
    }

    fn text_frames(text: &str) -> Vec<String> {
        vec![
            r#"{"type":"message_start","message":{"role":"assistant","usage":{"input_tokens":9,"output_tokens":0}}}"#.to_string(),
            r#"{"type":"ping"}"#.to_string(),
            r#"{"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}"#.to_string(),
            serde_json::json!({"type":"content_block_delta","index":0,
                "delta":{"type":"text_delta","text":text}})
            .to_string(),
            r#"{"type":"content_block_stop","index":0}"#.to_string(),
            r#"{"type":"message_delta","delta":{"stop_reason":"end_turn"}}"#.to_string(),
            r#"{"type":"message_stop"}"#.to_string(),
        ]
    }

    /// A caller that has stopped waiting takes the socket with it at the next
    /// line, rather than leaving the worker dribbling into a channel nobody
    /// holds until the backstop.
    #[test]
    fn a_reader_whose_caller_gave_up_stops_at_the_next_line() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|error| error.into_inner());
        let url = streaming_provider(
            text_frames("unwanted"),
            std::time::Duration::from_millis(1),
            false,
        );
        // SAFETY: `_guard` serialises every base-url mutation in this module.
        unsafe { env::set_var("ANTHROPIC_BASE_URL", &url) };
        let mut response = ureq::post(format!("{url}{MESSAGES_PATH}"))
            .config()
            .http_status_as_error(false)
            .build()
            .header("content-type", "application/json")
            .header("accept", "text/event-stream")
            .send(request_body(&sample_conversation()).as_slice())
            .expect("the fixture answered");
        unsafe { env::remove_var("ANTHROPIC_BASE_URL") };
        let mut lines = 0usize;
        let outcome = read_sse_stream(
            &mut response,
            &mut || {
                lines += 1;
                false
            },
            &mut |_| {},
        );
        assert_eq!(lines, 1, "reading must stop at the first line, not run on");
        let error = outcome.expect_err("a reader that gave up does not return a turn");
        assert!(
            error.to_string().contains("stopped waiting"),
            "the reason must name the caller: {error}"
        );
    }

    fn sample_conversation() -> Conversation {
        Conversation {
            system: "You act by writing TypeScript.".to_string(),
            messages: vec![
                Message::text(Role::User, "How many files name IntegrationId?"),
                Message::text(Role::Assistant, "```sterna\nreturn 1;\n```"),
            ],
        }
    }

    #[test]
    fn request_body_carries_the_conversation() {
        let conversation = sample_conversation();
        let value: serde_json::Value =
            serde_json::from_slice(&request_body(&conversation)).unwrap();
        assert_eq!(value["model"], MODEL);
        assert_eq!(value["max_tokens"], MAX_TOKENS);
        assert_eq!(value["system"][0]["text"], conversation.system);
        assert_eq!(value["messages"][0]["role"], "user");
        assert_eq!(value["messages"][1]["role"], "assistant");
    }

    /// The real event sequence a Messages stream sends, captured from the
    /// gateway on 2026-09-06 — including the `thinking` block that arrives
    /// ahead of the text: its text is omitted, its signature is not, and it
    /// is kept to go back with the next request, never shown as text.
    #[test]
    fn a_stream_of_deltas_becomes_the_same_turn_a_whole_response_would() {
        let mut acc = StreamAccumulator::new();
        let mut seen = String::new();
        for data in [
            r#"{"type":"message_start","message":{"role":"assistant","usage":{"input_tokens":13,"output_tokens":0}}}"#,
            r#"{"type":"ping"}"#,
            r#"{"type":"content_block_start","index":0,"content_block":{"type":"thinking","thinking":""}}"#,
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"signature_delta","signature":"EpAC"}}"#,
            r#"{"type":"content_block_stop","index":0}"#,
            r#"{"type":"content_block_start","index":1,"content_block":{"type":"text","text":""}}"#,
            r#"{"type":"content_block_delta","index":1,"delta":{"type":"text_delta","text":"1, 2, "}}"#,
            r#"{"type":"content_block_delta","index":1,"delta":{"type":"text_delta","text":"3"}}"#,
            // The real Anthropic shape: `message_delta`'s usage carries the
            // output count **and no input count**, so a naive overwrite
            // loses the 13 from `message_start` and `to_usage` then reports
            // no usage at all -- the session would silently fall back to
            // estimating a figure the provider had already given it.
            r#"{"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":30}}"#,
            r#"{"type":"message_stop"}"#,
        ] {
            if let Some(StreamDelta::Text(text)) = acc.event(data).unwrap() {
                seen.push_str(&text);
            }
        }
        assert_eq!(
            seen, "1, 2, 3",
            "the deltas were not handed over as they arrived"
        );

        let turn = acc.finish().unwrap();
        assert_eq!(turn.message.role, Role::Assistant);
        assert_eq!(
            turn.message.content,
            vec![
                Block::Thinking {
                    thinking: String::new(),
                    signature: "EpAC".to_string(),
                },
                Block::Text("1, 2, 3".to_string())
            ]
        );
        let usage = turn.usage.expect("the stream reported usage");
        assert_eq!(usage.input_tokens, 13);
        assert_eq!(
            usage.output_tokens, 30,
            "`message_start`'s zero overwrote `message_delta`'s real count"
        );
    }

    /// A connection cut mid-reply must not become a short answer the session
    /// appends and treats as final.
    #[test]
    fn a_stream_that_stops_early_is_an_error_not_a_short_reply() {
        let mut acc = StreamAccumulator::new();
        acc.event(r#"{"type":"content_block_delta","delta":{"type":"text_delta","text":"half"}}"#)
            .unwrap();
        let error = acc.finish().unwrap_err();
        assert!(
            matches!(error, WireError::Stream(_)),
            "expected a stream error, got {error:?}"
        );
    }

    /// Claude Code's history breakpoint: the newest message's last block
    /// that can carry one is marked, never a reasoning block, and only on a
    /// Claude model; the rest of the body is untouched.
    #[test]
    fn a_claude_request_marks_its_newest_message_for_the_cache() {
        let conversation = Conversation {
            system: "s".into(),
            messages: vec![
                Message::text(Role::User, "first"),
                Message {
                    role: Role::Assistant,
                    content: vec![Block::Text("ok".into())],
                    historical: None,
                },
                Message {
                    role: Role::User,
                    content: vec![
                        Block::Text("second".into()),
                        Block::Thinking {
                            thinking: String::new(),
                            signature: "sig".into(),
                        },
                    ],
                    historical: None,
                },
            ],
        };
        let body = request_body(&conversation);
        let marked: serde_json::Value =
            serde_json::from_slice(&with_history_breakpoint(body.clone(), "claude-opus-5-5"))
                .unwrap();
        let messages = marked["messages"].as_array().unwrap();
        assert_eq!(
            messages[2]["content"][0]["cache_control"]["type"],
            "ephemeral"
        );
        assert!(messages[2]["content"][1].get("cache_control").is_none());
        assert!(messages[0]["content"][0].get("cache_control").is_none());
        let mut unmarked = marked.clone();
        unmarked["messages"][2]["content"][0]
            .as_object_mut()
            .unwrap()
            .remove("cache_control");
        assert_eq!(
            unmarked,
            serde_json::from_slice::<serde_json::Value>(&body).unwrap()
        );
        assert_eq!(with_history_breakpoint(body.clone(), "gpt-6-sol"), body);
    }

    /// The main model asks for readable reasoning on a GPT route, at the
    /// effort GPT-6 applies anyway when none is named; Claude routes and every
    /// other caller's requests are left as they were.
    /// Nobody watching gets no summary; a watched task gets one on its
    /// first request and then no more than one per [`SUMMARY_EVERY`]; a new
    /// task starts the count again.
    #[test]
    fn a_reasoning_summary_is_asked_for_only_now_and_then_and_only_when_watched() {
        let routing = TurnRouting::default();
        routing.plan_summary(false);
        assert!(
            !routing.wants_summary(),
            "a run nobody watches asks for none"
        );
        routing.plan_summary(true);
        assert!(
            routing.wants_summary(),
            "the task's first watched request asks"
        );
        routing.plan_summary(true);
        assert!(
            !routing.wants_summary(),
            "the next one, seconds later, does not"
        );
        *routing.summarized.lock().unwrap() =
            Some(std::time::Instant::now() - SUMMARY_EVERY - std::time::Duration::from_secs(1));
        routing.plan_summary(true);
        assert!(routing.wants_summary(), "one per interval after that");
        routing.plan_summary(true);
        assert!(!routing.wants_summary());
        routing.clear();
        routing.plan_summary(true);
        assert!(
            routing.wants_summary(),
            "a new task's first request asks again"
        );
    }

    #[test]
    fn the_main_models_gpt_request_asks_for_a_reasoning_summary() {
        let body = |model: &str| {
            serde_json::to_vec(&serde_json::json!({"model": model, "max_tokens": 1000})).unwrap()
        };
        let gpt: serde_json::Value =
            serde_json::from_slice(&with_reasoning_summary(body("gpt-6-sol"), "gpt-6-sol"))
                .unwrap();
        assert_eq!(gpt["thinking"]["display"], "summarized");
        assert_eq!(
            gpt["thinking"]["budget_tokens"],
            effort_budget(Effort::Medium)
        );
        assert_eq!(gpt["max_tokens"], 1000 + effort_budget(Effort::Medium));
        let claude = with_reasoning_summary(body("claude-opus-5-5"), "claude-opus-5-5");
        assert_eq!(claude, body("claude-opus-5-5"));
        let mut acc = StreamAccumulator::new();
        acc.event(r#"{"type":"content_block_start","index":0,"content_block":{"type":"thinking","thinking":""}}"#).unwrap();
        let delta = acc
            .event(r#"{"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"Plan"}}"#)
            .unwrap();
        assert_eq!(delta, Some(StreamDelta::Reasoning("Plan".into())));
    }

    /// Reasoning arrives as deltas and is kept whole, signature included,
    /// so the next request can send it back; it is never shown as text, and
    /// reasoning that never got a signature is not kept.
    #[test]
    fn streamed_reasoning_is_kept_whole_with_its_signature() {
        let mut acc = StreamAccumulator::new();
        for event in [
            r#"{"type":"message_start","message":{"usage":{"input_tokens":1}}}"#,
            r#"{"type":"content_block_start","index":0,"content_block":{"type":"thinking","thinking":""}}"#,
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"look at "}}"#,
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"the tests"}}"#,
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"signature_delta","signature":"enc-"}}"#,
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"signature_delta","signature":"abc"}}"#,
            r#"{"type":"content_block_stop","index":0}"#,
            r#"{"type":"content_block_start","index":1,"content_block":{"type":"thinking","thinking":""}}"#,
            r#"{"type":"content_block_delta","index":1,"delta":{"type":"thinking_delta","thinking":"unsigned"}}"#,
            r#"{"type":"content_block_stop","index":1}"#,
            r#"{"type":"content_block_start","index":2,"content_block":{"type":"text","text":"done"}}"#,
            r#"{"type":"content_block_stop","index":2}"#,
            r#"{"type":"message_stop"}"#,
        ] {
            let delta = acc.event(event).unwrap();
            assert!(!matches!(delta, Some(StreamDelta::Text(ref t)) if t.contains("tests")));
        }
        let turn = acc.finish().unwrap();
        assert_eq!(
            turn.message.content,
            vec![
                Block::Thinking {
                    thinking: "look at the tests".into(),
                    signature: "enc-abc".into(),
                },
                Block::Text("done".into()),
            ]
        );
    }

    #[test]
    fn an_error_event_fails_the_turn_rather_than_ending_it_empty() {
        let mut acc = StreamAccumulator::new();
        let error = acc
            .event(r#"{"type":"error","error":{"type":"overloaded_error"}}"#)
            .unwrap_err();
        assert!(matches!(error, WireError::Stream(_)), "{error:?}");
    }

    /// Streaming is opt-in at the call, and the ordinary body must not gain
    /// a byte for it — two golden tests compare whole bodies.
    #[test]
    fn the_non_streaming_body_carries_no_stream_field() {
        let conversation = sample_conversation();
        let body = String::from_utf8(request_body(&conversation)).unwrap();
        assert!(!body.contains("stream"), "{body}");
    }

    #[test]
    fn parse_response_reads_the_assistant_text() {
        let body = r#"{"role":"assistant","content":[{"type":"text","text":"hi"}]}"#;
        let turn = parse_response(body).unwrap();
        assert_eq!(turn.message.role, Role::Assistant);
        assert_eq!(turn.message.content, vec![Block::Text("hi".to_string())]);
    }

    #[test]
    fn parse_response_preserves_a_native_call() {
        let body = r#"{"role":"assistant","content":[
            {"type":"tool_use","id":"1","name":"grep","input":{}},
            {"type":"text","text":"hi"}
        ]}"#;
        let turn = parse_response(body).unwrap();
        assert_eq!(
            turn.message.content,
            vec![
                Block::ToolUse {
                    id: "1".into(),
                    name: "grep".into(),
                    input: serde_json::json!({})
                },
                Block::Text("hi".to_string())
            ]
        );
    }

    #[test]
    fn streaming_assembles_split_native_input_and_preserves_backticks() {
        let mut acc = StreamAccumulator::new();
        for data in [
            r#"{"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"call-7","name":"execute_cell","input":{}}}"#,
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"{\"code\":\"const x = `a"}}"#,
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"b`; return x;\"}"}}"#,
            r#"{"type":"content_block_stop","index":0}"#,
            r#"{"type":"message_stop"}"#,
        ] {
            acc.event(data).unwrap();
        }
        assert_eq!(
            acc.finish().unwrap().message.content,
            vec![Block::ToolUse {
                id: "call-7".into(),
                name: "execute_cell".into(),
                input: serde_json::json!({"code":"const x = `ab`; return x;"}),
            }]
        );
    }

    #[test]
    fn incomplete_or_malformed_native_input_never_becomes_a_call() {
        let mut incomplete = StreamAccumulator::new();
        incomplete.event(r#"{"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"x","name":"execute_cell","input":{}}}"#).unwrap();
        incomplete.event(r#"{"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"{\"code\":"}}"#).unwrap();
        incomplete.event(r#"{"type":"message_stop"}"#).unwrap();
        assert!(incomplete.finish().is_err());

        let mut malformed = StreamAccumulator::new();
        malformed.event(r#"{"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"x","name":"execute_cell","input":{}}}"#).unwrap();
        malformed.event(r#"{"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"nope"}}"#).unwrap();
        assert!(
            malformed
                .event(r#"{"type":"content_block_stop","index":0}"#)
                .is_err()
        );
    }

    #[test]
    fn a_stopped_native_block_rejects_late_input_and_survives_the_ceiling() {
        fn ready() -> StreamAccumulator {
            let mut acc = StreamAccumulator::new();
            acc.event(r#"{"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"x","name":"execute_cell","input":{}}}"#).unwrap();
            acc.event(r#"{"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"{\"code\":\"return 1\"}"}}"#).unwrap();
            acc.event(r#"{"type":"content_block_stop","index":0}"#)
                .unwrap();
            acc
        }
        assert!(ready().event(r#"{"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":" "}}"#).is_err());
        assert!(
            ready()
                .event(r#"{"type":"content_block_stop","index":0}"#)
                .is_err()
        );
        // **The two transports answer this differently on purpose, because
        // they hold different proof.** A stream saw `content_block_stop` for
        // this block, so it is known finished and it runs. A whole response
        // carries no such marker, so the only honest reading of a lone block
        // under `max_tokens` is that it is the one the ceiling cut.
        let mut capped = ready();
        capped.event(r#"{"type":"message_delta","delta":{"stop_reason":"max_tokens"},"usage":{"output_tokens":10}}"#).unwrap();
        capped.event(r#"{"type":"message_stop"}"#).unwrap();
        let salvaged = capped.finish().expect("a block the stream finished runs");
        assert!(salvaged.truncated);
        assert!(matches!(
            salvaged.message.content.as_slice(),
            [Block::ToolUse { id, .. }] if id == "x"
        ));

        let whole = r#"{"role":"assistant","stop_reason":"max_tokens","content":[{"type":"tool_use","id":"x","name":"execute_cell","input":{"code":"return 1"}}]}"#;
        assert!(matches!(
            parse_response(whole),
            Err(WireError::IncompleteResponse { partial_text }) if partial_text.is_empty()
        ));
    }

    #[test]
    fn max_token_prose_is_incomplete_and_inspectable_in_both_transports() {
        // More than BODY_HEAD_LIMIT, including Unicode and terminal controls:
        // the diagnostic keeps every character, escaping controls for display.
        let partial = format!("{}\nnext: café\u{1b}[2J", "working… ".repeat(80));
        let whole = serde_json::json!({
            "role": "assistant",
            "stop_reason": "max_tokens",
            "content": [{"type": "text", "text": partial}],
        });
        let json_error = parse_response(&whole.to_string()).unwrap_err();
        let mut streamed = StreamAccumulator::new();
        streamed.event(r#"{"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}"#).unwrap();
        streamed
            .event(
                &serde_json::json!({
                    "type": "content_block_delta", "index": 0,
                    "delta": {"type": "text_delta", "text": partial},
                })
                .to_string(),
            )
            .unwrap();
        streamed
            .event(r#"{"type":"content_block_stop","index":0}"#)
            .unwrap();
        streamed
            .event(r#"{"type":"message_delta","delta":{"stop_reason":"max_tokens"}}"#)
            .unwrap();
        streamed.event(r#"{"type":"message_stop"}"#).unwrap();
        let stream_error = streamed.finish().unwrap_err();

        for error in [&json_error, &stream_error] {
            assert!(
                matches!(error, WireError::IncompleteResponse { partial_text } if partial_text == &partial)
            );
            assert!(!error.is_context_overflow());
            let shown = error.to_string();
            assert!(shown.contains("response incomplete"), "{shown}");
            assert!(shown.contains(&format!("{partial:?}")), "{shown}");
            assert!(!shown.contains('\u{1b}'));
        }
        assert_eq!(json_error.to_string(), stream_error.to_string());
    }

    #[test]
    fn max_token_mixed_text_and_tool_input_never_returns_an_executable_turn() {
        let whole = r#"{"role":"assistant","stop_reason":"max_tokens","content":[{"type":"text","text":"Partial explanation"},{"type":"tool_use","id":"x","name":"execute_cell","input":{"code":"return 1"}}]}"#;
        assert!(
            matches!(parse_response(whole), Err(WireError::IncompleteResponse { partial_text }) if partial_text == "Partial explanation")
        );

        let mut streamed = StreamAccumulator::new();
        streamed.event(r#"{"type":"content_block_start","index":0,"content_block":{"type":"text","text":"Partial explanation"}}"#).unwrap();
        streamed.event(r#"{"type":"content_block_start","index":1,"content_block":{"type":"tool_use","id":"x","name":"execute_cell","input":{}}}"#).unwrap();
        streamed.event(r#"{"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"{\"code\":"}}"#).unwrap();
        streamed
            .event(r#"{"type":"message_delta","delta":{"stop_reason":"max_tokens"}}"#)
            .unwrap();
        streamed.event(r#"{"type":"message_stop"}"#).unwrap();
        assert!(
            matches!(streamed.finish(), Err(WireError::IncompleteResponse { partial_text }) if partial_text == "Partial explanation")
        );
    }

    /// The case that cost a whole turn: the model finished a program and ran
    /// out of room in the prose after it.
    #[test]
    fn a_program_finished_before_the_ceiling_still_runs_and_says_it_was_cut() {
        let whole = r#"{"role":"assistant","stop_reason":"max_tokens","content":[{"type":"tool_use","id":"x","name":"execute_cell","input":{"code":"return 1"}},{"type":"text","text":"and then I will"}]}"#;
        let turn = parse_response(whole).expect("a finished program survives its turn being cut");
        assert!(turn.truncated, "the turn must admit it was cut");
        assert!(
            matches!(
                turn.message.content.as_slice(),
                [Block::ToolUse { name, input, .. }]
                    if name == "execute_cell" && input["code"] == "return 1"
            ),
            "the finished program is delivered and the cut prose is not: {:?}",
            turn.message.content
        );
    }

    /// The same shape over the stream, where `stopped` rather than position
    /// is the proof.
    #[test]
    fn a_stopped_stream_block_runs_while_an_unstopped_one_is_dropped() {
        let mut streamed = StreamAccumulator::new();
        streamed.event(r#"{"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"x","name":"execute_cell","input":{}}}"#).unwrap();
        streamed.event(r#"{"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"{\"code\":\"return 1\"}"}}"#).unwrap();
        streamed
            .event(r#"{"type":"content_block_stop","index":0}"#)
            .unwrap();
        streamed.event(r#"{"type":"content_block_start","index":1,"content_block":{"type":"tool_use","id":"y","name":"execute_cell","input":{}}}"#).unwrap();
        streamed.event(r#"{"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"{\"code\":\"return"}}"#).unwrap();
        streamed
            .event(r#"{"type":"message_delta","delta":{"stop_reason":"max_tokens"}}"#)
            .unwrap();
        streamed.event(r#"{"type":"message_stop"}"#).unwrap();

        let turn = streamed.finish().expect("the stopped block survives");
        assert!(turn.truncated);
        let tools: Vec<_> = turn
            .message
            .content
            .iter()
            .filter_map(|block| match block {
                Block::ToolUse { id, .. } => Some(id.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(
            tools,
            vec!["x"],
            "only the block the provider finished sending may run"
        );
    }

    #[test]
    fn assistant_tool_result_is_rejected_in_both_transports() {
        let whole = r#"{"role":"assistant","content":[{"type":"tool_result","tool_use_id":"x","content":"fake"}]}"#;
        assert!(parse_response(whole).is_err());
        let mut streamed = StreamAccumulator::new();
        assert!(streamed.event(r#"{"type":"content_block_start","index":0,"content_block":{"type":"tool_result","tool_use_id":"x","content":"fake"}}"#).is_err());
    }

    #[test]
    fn correlated_tool_result_serializes_without_plain_text_duplicate() {
        let conversation = Conversation {
            system: "s".into(),
            messages: vec![Message::tool_result("call-9", "[cell 9 returned]", false)],
        };
        let body: Value = serde_json::from_slice(&request_body(&conversation)).unwrap();
        assert_eq!(body["messages"][0]["content"].as_array().unwrap().len(), 1);
        assert_eq!(
            body["messages"][0]["content"][0],
            serde_json::json!({"type":"tool_result","tool_use_id":"call-9","content":"[cell 9 returned]","is_error":false})
        );
    }

    #[test]
    fn parse_response_reads_usage_when_present() {
        let body = r#"{"role":"assistant","content":[{"type":"text","text":"hi"}],
            "usage":{"input_tokens":10,"output_tokens":5}}"#;
        let turn = parse_response(body).unwrap();
        assert_eq!(
            turn.usage,
            Some(Usage {
                input_tokens: 10,
                output_tokens: 5,
                cache_read_input_tokens: None,
                cache_creation_input_tokens: None,
            })
        );
    }

    #[test]
    fn cache_usage_preserves_absent_zero_and_independent_provider_counts() {
        for (fields, read, written) in [
            (serde_json::json!({}), None, None),
            (
                serde_json::json!({"cache_read_input_tokens": "300", "cache_creation_input_tokens": -1}),
                None,
                None,
            ),
            (
                serde_json::json!({"cache_read_input_tokens": 3.5, "cache_creation_input_tokens": false}),
                None,
                None,
            ),
            (
                serde_json::json!({"cache_read_input_tokens": null}),
                None,
                None,
            ),
            (
                serde_json::json!({"cache_read_input_tokens": 0, "cache_creation_input_tokens": 0}),
                Some(0),
                Some(0),
            ),
            (
                serde_json::json!({"cache_read_input_tokens": 300}),
                Some(300),
                None,
            ),
            (
                serde_json::json!({"cache_creation_input_tokens": 400}),
                None,
                Some(400),
            ),
        ] {
            let mut usage = serde_json::json!({"input_tokens": 0, "output_tokens": 0});
            usage
                .as_object_mut()
                .unwrap()
                .extend(fields.as_object().unwrap().clone());
            let whole = parse_response(
                &serde_json::json!({
                    "role": "assistant", "content": [], "usage": usage
                })
                .to_string(),
            )
            .unwrap()
            .usage
            .unwrap();
            assert_eq!(whole.cache_read_input_tokens, read);
            assert_eq!(whole.cache_creation_input_tokens, written);
            let mut stream = StreamAccumulator::new();
            stream
                .event(
                    &serde_json::json!({
                        "type": "message_start", "message": {"role": "assistant", "usage": usage}
                    })
                    .to_string(),
                )
                .unwrap();
            stream
                .event(r#"{"type":"message_delta","usage":{"output_tokens":0}}"#)
                .unwrap();
            stream.event(r#"{"type":"message_stop"}"#).unwrap();
            assert_eq!(stream.finish().unwrap().usage, Some(whole));
        }
    }

    #[test]
    fn streamed_cache_usage_uses_latest_supplied_count_without_summing_or_inventing_hits() {
        let mut stream = StreamAccumulator::new();
        stream.event(r#"{"type":"message_start","message":{"role":"assistant","usage":{"input_tokens":12,"output_tokens":0,"cache_read_input_tokens":100,"cache_creation_input_tokens":200}}}"#).unwrap();
        stream.event(r#"{"type":"message_delta","usage":{"output_tokens":4,"cache_read_input_tokens":0,"cache_creation_input_tokens":300}}"#).unwrap();
        stream.event(r#"{"type":"message_stop"}"#).unwrap();
        let usage = stream.finish().unwrap().usage.unwrap();
        assert_eq!(usage.cache_read_input_tokens, Some(0));
        assert_eq!(usage.cache_creation_input_tokens, Some(300));
    }

    #[test]
    fn parse_response_without_usage_yields_none_not_zero() {
        let body = r#"{"role":"assistant","content":[{"type":"text","text":"hi"}]}"#;
        let turn = parse_response(body).unwrap();
        assert_eq!(turn.usage, None);
    }

    #[test]
    fn parse_response_with_a_partial_usage_row_yields_none() {
        let body = r#"{"role":"assistant","content":[{"type":"text","text":"hi"}],
            "usage":{"input_tokens":10}}"#;
        let turn = parse_response(body).unwrap();
        assert_eq!(turn.usage, None);
    }

    #[test]
    fn parse_response_rejects_a_non_assistant_role() {
        let body = r#"{"role":"user","content":[]}"#;
        let err = parse_response(body).unwrap_err();
        assert!(matches!(err, WireError::UnexpectedRole(role) if role == "user"));
    }
}

#[cfg(test)]
mod effort_tests {
    use super::*;

    #[test]
    fn default_is_the_display_name_and_the_only_spelling() {
        assert_eq!(Effort::parse("auto"), Some(Effort::Auto));
        assert_eq!(Effort::parse("default"), None, "the retired word");
        assert_eq!(Effort::Auto.name(), "auto");
    }

    #[test]
    fn effort_preserves_default_bytes_and_selects_the_supported_wire_form() {
        let conversation = Conversation {
            system: "system".into(),
            messages: vec![],
        };
        assert_eq!(
            request_body_configured(&conversation, MODEL, Effort::Auto),
            request_body(&conversation)
        );
        let claude: serde_json::Value = serde_json::from_slice(&request_body_configured(
            &conversation,
            "claude-fable-5.1",
            Effort::High,
        ))
        .unwrap();
        assert_eq!(claude["output_config"]["effort"], "high");
        assert!(claude.get("thinking").is_none());
        let translated: serde_json::Value = serde_json::from_slice(&request_body_configured(
            &conversation,
            "deepseek-v4-flash",
            Effort::Medium,
        ))
        .unwrap();
        assert_eq!(translated["thinking"]["budget_tokens"], 16384);
        assert!(translated["max_tokens"].as_u64().unwrap() > 16384);
        // The word rides along on the translated leg too, because a budget
        // saturates and cannot say `xhigh` or `max`.
        assert_eq!(translated["output_config"]["effort"], "medium");
    }

    /// The task model keeps every token it had. The figures are spelled out
    /// rather than derived -- a future reader changing the
    /// ladder has to change this test on purpose.
    #[test]
    fn the_task_paths_allowance_is_unchanged_at_every_effort() {
        let conversation = Conversation {
            system: "system".into(),
            messages: vec![],
        };
        for (effort, budget) in [
            (Effort::Low, 4096u64),
            (Effort::Medium, 16384),
            (Effort::High, 32769),
            (Effort::Xhigh, 49152),
            (Effort::Max, 65536),
        ] {
            let value: serde_json::Value = serde_json::from_slice(&request_body_configured(
                &conversation,
                "deepseek-v4-flash",
                effort,
            ))
            .unwrap();
            assert_eq!(value["thinking"]["budget_tokens"].as_u64().unwrap(), budget);
            assert_eq!(
                value["max_tokens"].as_u64().unwrap(),
                budget + u64::from(MAX_TOKENS),
                "the task path adds its reasoning budget on top, at {}",
                effort.name()
            );
        }
    }

    /// Five levels a person can pick must be five things on the wire.
    ///
    /// They were not: `high`, `xhigh` and `max` all became one token budget
    /// on a translated model, and on top of that `xhigh` and `max` were
    /// silently reset to `auto` whenever the model was not a Claude one --
    /// so two of the five could not be used at all.
    #[test]
    fn every_effort_level_is_a_distinct_thing_on_the_wire() {
        let conversation = Conversation {
            system: "system".into(),
            messages: vec![],
        };
        let levels = [
            Effort::Low,
            Effort::Medium,
            Effort::High,
            Effort::Xhigh,
            Effort::Max,
        ];

        let mut budgets = Vec::new();
        for effort in levels {
            let claude: serde_json::Value = serde_json::from_slice(&request_body_configured(
                &conversation,
                "claude-opus-5",
                effort,
            ))
            .unwrap();
            assert_eq!(claude["output_config"]["effort"], effort.name());

            let translated: serde_json::Value = serde_json::from_slice(&request_body_configured(
                &conversation,
                "deepseek-v4-flash",
                effort,
            ))
            .unwrap();
            assert_eq!(translated["output_config"]["effort"], effort.name());
            budgets.push(translated["thinking"]["budget_tokens"].as_u64().unwrap());
        }

        let mut ascending = budgets.clone();
        ascending.sort_unstable();
        ascending.dedup();
        assert_eq!(
            ascending.len(),
            budgets.len(),
            "two levels collapsed onto one budget: {budgets:?}"
        );
        assert_eq!(
            ascending, budgets,
            "a higher level must not buy less: {budgets:?}"
        );
    }

    #[test]
    fn a_published_output_maximum_replaces_the_fallback_and_absence_keeps_it() {
        use crate::models::ModelLimits;
        assert_eq!(
            max_tokens_from(ModelLimits::default()),
            MAX_TOKENS,
            "nothing published keeps the documented fallback"
        );
        assert_eq!(
            max_tokens_from(ModelLimits {
                context_window_tokens: Some(400_000),
                observed_context_window_tokens: None,
                served_context_window_tokens: None,
                max_output_tokens: Some(64_000),
            }),
            64_000,
            "the model's own maximum is the right figure, not ours"
        );
        assert_eq!(
            max_tokens_from(ModelLimits {
                context_window_tokens: None,
                observed_context_window_tokens: None,
                served_context_window_tokens: None,
                max_output_tokens: Some(u64::from(u32::MAX) + 1),
            }),
            u32::MAX,
            "a figure wider than the wire is clamped, not refused"
        );
    }
}
