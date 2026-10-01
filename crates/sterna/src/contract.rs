//! The vocabulary 61C's packages share, frozen so they can be built in
//! parallel: what a conversation is, what identifies a session, what the
//! project's own documents said, and who served a request.
//!
//! Nothing here decides anything and nothing here talks to a network, a
//! process or a file. Each 61C package owns its own module and fills it
//! against these types.

use std::collections::BTreeMap;
use std::path::PathBuf;

/// Who said it. There is no `System` variant: the Anthropic Messages
/// protocol carries the system prompt beside the message list rather than
/// inside it, and a `Role::System` here would invite a message that cannot
/// be serialised.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    User,
    Assistant,
}

impl Role {
    pub fn as_str(self) -> &'static str {
        match self {
            Role::User => "user",
            Role::Assistant => "assistant",
        }
    }
}

/// One ordered provider content block. Native cell calls and their results
/// remain structured so correlation survives replay and gateway translation.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Block {
    Text(String),
    Image {
        media_type: String,
        data: String,
    },
    ToolUse {
        id: String,
        name: String,
        input: serde_json::Value,
    },
    ToolResult {
        tool_use_id: String,
        content: String,
        is_error: bool,
    },
    /// The model's own reasoning, kept exactly as the provider returned it
    /// and sent back unchanged: the signature is what makes it valid again
    /// (for a GPT model behind the subscription broker it is the encrypted
    /// reasoning item). Dropping it loses the reasoning and the provider's
    /// cache for everything after it.
    Thinking {
        thinking: String,
        signature: String,
    },
    RedactedThinking {
        data: String,
    },
}

impl Block {
    /// The block's actual display payload: prose, submitted cell source, or
    /// returned cell feedback. Protocol ids and names are never synthesized
    /// into prose here.
    pub fn text(&self) -> &str {
        match self {
            Block::Image { .. } => "[image attachment]",
            Block::Thinking { .. } | Block::RedactedThinking { .. } => "",
            Block::Text(text) | Block::ToolResult { content: text, .. } => text,
            Block::ToolUse { input, .. } => input
                .get("code")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default(),
        }
    }
}

/// One message in the conversation.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Message {
    pub role: Role,
    pub content: Vec<Block>,
    /// Trusted renderer-produced history after a newer live-state snapshot
    /// supersedes this one. Never inferred from user/model text or included as
    /// metadata in provider requests; the rollout preserves it for resume.
    pub(crate) historical: Option<String>,
}

impl Message {
    pub fn text(role: Role, text: impl Into<String>) -> Self {
        Self {
            role,
            content: vec![Block::Text(text.into())],
            historical: None,
        }
    }

    /// A person's message as they wrote it: its first text block, which is
    /// the request itself, and a marker for each image they attached.
    ///
    /// A task's message also carries the task's context for the model --
    /// mode, plan, the Scout's brief, the acceptance list -- in the blocks
    /// after the request (`carry_task_context`). Those blocks are not the
    /// person's words, and a screen that shows them under their name shows
    /// them something they never wrote.
    #[must_use]
    pub fn as_written(&self) -> String {
        let mut words = self
            .content
            .iter()
            .find_map(|block| match block {
                Block::Text(text) => Some(text.clone()),
                _ => None,
            })
            .unwrap_or_default();
        for image in self
            .content
            .iter()
            .filter(|block| matches!(block, Block::Image { .. }))
        {
            if !words.is_empty() {
                words.push('\n');
            }
            words.push_str(image.text());
        }
        words
    }

    pub fn runtime(text: impl Into<String>, historical: impl Into<String>) -> Self {
        Self {
            historical: Some(historical.into()),
            ..Self::text(Role::User, text)
        }
    }

    /// One real cell result correlated to the assistant call that requested
    /// it. It is not duplicated into an adjacent text block.
    pub fn tool_result(
        tool_use_id: impl Into<String>,
        content: impl Into<String>,
        is_error: bool,
    ) -> Self {
        Self {
            role: Role::User,
            content: vec![Block::ToolResult {
                tool_use_id: tool_use_id.into(),
                content: content.into(),
                is_error,
            }],
            historical: None,
        }
    }

    /// A correlated cell result with the trusted compact history renderer
    /// produced for the same cell.
    pub fn runtime_tool_result(
        tool_use_id: impl Into<String>,
        content: impl Into<String>,
        is_error: bool,
        historical: impl Into<String>,
    ) -> Self {
        Self {
            historical: Some(historical.into()),
            ..Self::tool_result(tool_use_id, content, is_error)
        }
    }
}

/// A task's whole conversation: the system prompt the project's documents
/// produced, and the messages so far.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Conversation {
    pub system: String,
    pub messages: Vec<Message>,
}

/// What identifies one sterna session.
///
/// It is the rollout file's name, the `--session` argument every
/// `glasshouse hook` invocation carries, and the string Glasshouse records in
/// `routing_observations.session_id` when it launched the session itself.
/// **The ruler does not use it as a join key** and the reason is worth
/// keeping here: a sterna launched directly by a person or by the ruler has no
/// Glasshouse session at all, so a row filtered on this id would match
/// nothing while looking exactly like a meter that was never configured.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SessionId(String);

impl SessionId {
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for SessionId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// What the project's own files said, read and never written.
///
/// Map line 2448's invariant is that sterna edits none of this. The type is
/// therefore read-only by construction — every field is owned data already
/// parsed out of the file, and nothing here holds a handle that could write
/// back to the path it came from.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProjectConfig {
    /// The project root every path below is relative to.
    pub root: PathBuf,
    /// `CLAUDE.md` and `AGENTS.md`, in the order they were found, each with
    /// the path it came from so a reader can say which document said what.
    pub instructions: Vec<(PathBuf, String)>,
    /// `.claude/settings.json`, verbatim, unparsed beyond JSON. 61D compiles
    /// its `permissions` into sandbox grants; 61C only carries it.
    pub settings: Option<String>,
    /// `.claude/commands/<name>.md`, by command name without the extension.
    pub commands: BTreeMap<String, String>,
    /// Skills by name, each the directory that holds it.
    pub skills: BTreeMap<String, PathBuf>,
    /// `.mcp.json`, verbatim.
    pub mcp: Option<String>,
}

/// Which entitlement served one request and what it cost — map line 2451.
///
/// **Every field is optional and absent is not zero**, the same rule the
/// ruler's `Tokens` keeps: these come from Glasshouse's routing ledger after
/// the fact, and a request nobody metered must never render as a free one.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ServedBy {
    pub provider: Option<String>,
    pub model: Option<String>,
    pub route: Option<String>,
    /// The entitlement, as `routing_observations.quota_context` records it.
    pub quota_context: Option<String>,
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub cached_input_tokens: Option<u64>,
}

impl ServedBy {
    /// Whether anything at all was learned about this request. A `ServedBy`
    /// where this is `false` renders as unknown, never as a zero-cost
    /// request served by nobody.
    pub fn is_known(&self) -> bool {
        self.provider.is_some()
            || self.model.is_some()
            || self.quota_context.is_some()
            || self.input_tokens.is_some()
            || self.output_tokens.is_some()
            || self.cached_input_tokens.is_some()
    }
}

/// The kind of a rollout line.
///
/// **One rollout file per session, one JSON object per line, append-only, and
/// a `kind` on every line.** 61C writes `System` once and then `Turn` lines; 61E's runtime writes
/// `Cell` lines against the format `runtime-contract.md` §4 fixes. They share
/// one file rather than two so that append order *is* the session's order —
/// two files would need a clock to reconcile, and a resumed session would
/// depend on it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RolloutKind {
    /// The session's system prompt, written once on the file's first line.
    System,
    Turn,
    Cell,
}

impl RolloutKind {
    /// `const` so a module writing one kind can name it in a `const` of its
    /// own rather than re-spelling the string.
    pub const fn as_str(self) -> &'static str {
        match self {
            RolloutKind::System => "system",
            RolloutKind::Turn => "turn",
            RolloutKind::Cell => "cell",
        }
    }
}
