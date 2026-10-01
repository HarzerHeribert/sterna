//! The append-only rollout file: one JSON object per line, one file per
//! session. [`crate::contract::RolloutKind`]'s doc comment is why one file rather
//! than two -- 61C's `"turn"` lines and 61E's `"cell"` lines share it so that
//! append order is the session's order. [`resume`] rebuilds a
//! [`crate::contract::Conversation`] by reading the file, never by replaying it:
//! `runtime-contract.md` §4 fixes that a resumed program must not re-run a
//! side effect, and "a program that deleted a branch would delete it twice"
//! is the reason.

use std::fs::{File, OpenOptions};
use std::io::{self, BufRead, BufReader, Write};
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::contract::{Block, Conversation, Message, Role, RolloutKind, SessionId};
use crate::runtime::outcome::CellRecord;
use crate::tui::CellView;

/// The `kind` sterna writes for the one line per session that carries the
/// system prompt. `RolloutKind` (`contract.rs`, frozen) only names the two
/// kinds 61C and 61E's runtime both need to recognise by name across the
/// shared file; this kind is 61C's own, chosen and documented here, and a
/// reader that does not know it skips it exactly like any other unknown
/// `kind` -- so nothing about the shared format depends on this name.
/// The system prompt's line kind, from the frozen vocabulary rather than
/// spelled here: 61E writes into the same file and both halves must agree on
/// the string, which a shared enum guarantees and a local constant does not.
pub const SYSTEM_KIND: &str = RolloutKind::System.as_str();

#[derive(Serialize, Deserialize)]
struct SystemLine {
    kind: String,
    session_id: String,
    text: String,
}

#[derive(Serialize, Deserialize)]
struct TurnLine {
    kind: String,
    session_id: String,
    turn: u64,
    role: String,
    text: String,
    at_millis: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    historical: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    checkpoint: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    blocks: Option<Vec<PersistedBlock>>,
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum PersistedBlock {
    Image {
        media_type: String,
        data: String,
    },
    Text {
        text: String,
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
    Thinking {
        thinking: String,
        signature: String,
    },
    RedactedThinking {
        data: String,
    },
}

impl PersistedBlock {
    fn from_block(block: &Block) -> Self {
        match block {
            Block::Image { media_type, data } => Self::Image {
                media_type: media_type.clone(),
                data: data.clone(),
            },
            Block::Text(text) => Self::Text { text: text.clone() },
            Block::ToolUse { id, name, input } => Self::ToolUse {
                id: id.clone(),
                name: name.clone(),
                input: input.clone(),
            },
            Block::ToolResult {
                tool_use_id,
                content,
                is_error,
            } => Self::ToolResult {
                tool_use_id: tool_use_id.clone(),
                content: content.clone(),
                is_error: *is_error,
            },
            Block::Thinking {
                thinking,
                signature,
            } => Self::Thinking {
                thinking: thinking.clone(),
                signature: signature.clone(),
            },
            Block::RedactedThinking { data } => Self::RedactedThinking { data: data.clone() },
        }
    }

    fn into_block(self) -> Block {
        match self {
            Self::Image { media_type, data } => Block::Image { media_type, data },
            Self::Text { text } => Block::Text(text),
            Self::ToolUse { id, name, input } => Block::ToolUse { id, name, input },
            Self::ToolResult {
                tool_use_id,
                content,
                is_error,
            } => Block::ToolResult {
                tool_use_id,
                content,
                is_error,
            },
            Self::Thinking {
                thinking,
                signature,
            } => Block::Thinking {
                thinking,
                signature,
            },
            Self::RedactedThinking { data } => Block::RedactedThinking { data },
        }
    }
}

/// One cell's line: `runtime-contract.md` §4's own object, with the two
/// fields every line in this shared file carries in front of it.
///
/// **`#[serde(flatten)]` rather than a struct that repeats §4's fields.** The
/// contract's shape is `CellRecord`'s and the runtime owns it; re-spelling it
/// here would be a second place for it to drift, and the drift would be
/// invisible because both halves would still serialise.
#[derive(Serialize)]
struct CellLine<'a> {
    kind: &'static str,
    session_id: &'a str,
    #[serde(flatten)]
    record: &'a CellRecord,
}

/// One change of the sandbox level. Its own kind, so `resume` passes over it
/// and the conversation it rebuilds is unchanged.
#[derive(Serialize, Deserialize)]
struct LevelLine {
    kind: String,
    session_id: String,
    from: String,
    to: String,
    at_unix: u64,
}

#[derive(Serialize, Deserialize)]
struct ViewLine {
    kind: String,
    session_id: String,
    ordinal: usize,
    view: CellView,
}

fn now_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

/// An open rollout file. Every write appends one JSON line and flushes; no
/// code path in this type seeks or truncates.
pub struct Rollout {
    file: File,
    session_id: SessionId,
    next_turn: u64,
}

impl Rollout {
    /// Opens the rollout file at `path`, creating it if it does not exist.
    /// A newly created file's first line records `system` once, per the
    /// packet: it is never repeated on a later turn.
    pub fn create(path: &Path, session_id: SessionId, system: &str) -> io::Result<Self> {
        let is_new = !path.exists();
        let file = OpenOptions::new().create(true).append(true).open(path)?;
        let mut rollout = Self {
            file,
            session_id,
            next_turn: 0,
        };
        if is_new {
            let line = SystemLine {
                kind: SYSTEM_KIND.to_string(),
                session_id: rollout.session_id.as_str().to_string(),
                text: system.to_string(),
            };
            rollout.append_line(&line)?;
        }
        Ok(rollout)
    }

    /// Append the effective context for a new task without changing chat history.
    /// The original system row remains an immutable session-start record.
    pub fn record_context(&mut self, system: &str) -> io::Result<()> {
        self.append_line(&SystemLine {
            kind: "context".into(),
            session_id: self.session_id.as_str().to_string(),
            text: system.to_string(),
        })
    }

    /// Appends one turn at the next turn number and advances it.
    pub fn record_turn(&mut self, role: Role, text: &str) -> io::Result<()> {
        self.record_feedback(role, text, None)
    }

    /// Append a protocol message without flattening its typed content blocks.
    pub fn record_message(&mut self, message: &Message) -> io::Result<()> {
        // A person's turn is recorded as they wrote it; the task context that
        // rides after it (mode, plan, environment) is in `blocks`, which is
        // what a resume rebuilds the conversation from.
        let text = if message.role == Role::User {
            message.as_written()
        } else {
            message.content.iter().map(Block::text).collect::<String>()
        };
        let blocks = message
            .content
            .iter()
            .map(PersistedBlock::from_block)
            .collect();
        self.record_turn_parts(
            message.role,
            &text,
            message.historical.as_deref(),
            false,
            Some(blocks),
        )
    }

    /// Preserve renderer provenance across resume without changing the full
    /// original text or inferring runtime state from a message's contents.
    pub fn record_feedback(
        &mut self,
        role: Role,
        text: &str,
        historical: Option<&str>,
    ) -> io::Result<()> {
        self.record_turn_parts(role, text, historical, false, None)
    }

    pub fn record_checkpoint(&mut self, text: &str) -> io::Result<()> {
        self.record_turn_parts(Role::User, text, None, true, None)
    }

    fn record_turn_parts(
        &mut self,
        role: Role,
        text: &str,
        historical: Option<&str>,
        checkpoint: bool,
        blocks: Option<Vec<PersistedBlock>>,
    ) -> io::Result<()> {
        let line = TurnLine {
            kind: RolloutKind::Turn.as_str().to_string(),
            session_id: self.session_id.as_str().to_string(),
            turn: self.next_turn,
            role: role.as_str().to_string(),
            text: text.to_string(),
            at_millis: now_millis(),
            historical: historical.map(str::to_owned),
            checkpoint: checkpoint.then_some(true),
            blocks,
        };
        self.append_line(&line)?;
        self.next_turn += 1;
        Ok(())
    }

    /// Appends one cell's line -- `runtime-contract.md` §4.
    ///
    /// **It does not advance the turn number.** A cell is not a turn: the
    /// user and assistant messages around it write their own `turn` lines and
    /// [`resume`] rebuilds the conversation from those alone, so a cell line
    /// that consumed a turn number would leave a hole in the sequence
    /// `record_turn` maintains.
    pub fn record_cell(&mut self, record: &CellRecord) -> io::Result<()> {
        let session_id = self.session_id.clone();
        let line = CellLine {
            kind: RolloutKind::Cell.as_str(),
            session_id: session_id.as_str(),
            record,
        };
        self.append_line(&line)
    }

    /// A standing run is distinct from a model cell and advances no turn counter.
    pub fn record_handler(&mut self, name: &str, record: &CellRecord) -> io::Result<()> {
        let mut line = serde_json::to_value(record)?;
        line["kind"] = "cell".into();
        line["session_id"] = self.session_id.as_str().into();
        line["handler"] = name.into();
        self.append_line(&line)
    }

    /// Persist display-only cell evidence so restart can restore inspection
    /// without parsing rendered prose or replaying source.
    pub fn record_view(&mut self, ordinal: usize, view: &CellView) -> io::Result<()> {
        self.append_line(&ViewLine {
            kind: "view".into(),
            session_id: self.session_id.as_str().into(),
            ordinal,
            view: view.clone(),
        })
    }

    /// One line per change of the sandbox level: which levels, and when.
    ///
    /// **A level the person changed is a fact about the session, not a turn
    /// in it.** Its own `kind` keeps it out of the conversation `resume`
    /// rebuilds -- that reader keeps the kinds it knows and ignores the rest
    /// -- while a person or an audit reading the file afterwards can see that
    /// the session moved, and when.
    pub fn record_level(&mut self, from: &str, to: &str, at_unix: u64) -> io::Result<()> {
        self.append_line(&LevelLine {
            kind: "sandbox".into(),
            session_id: self.session_id.as_str().into(),
            from: from.into(),
            to: to.into(),
            at_unix,
        })
    }

    /// Writes every level change since the last drain, and drops what it
    /// wrote.
    ///
    /// A failed write is silent for the same reason a failed hook delivery
    /// is: a session that cannot append one bookkeeping line should still
    /// finish the person's work. `None` is a session with no live level --
    /// the constructed ones in tests, which never ask anybody anything.
    pub fn record_moves(&mut self, level: Option<&crate::permissions::LiveLevel>) {
        let Some(level) = level else { return };
        for moved in level.drain_moves() {
            let at = moved
                .at
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();
            let _ = self.record_level(moved.from.name(), moved.to.name(), at);
        }
    }

    fn append_line<T: Serialize>(&mut self, line: &T) -> io::Result<()> {
        let mut json = serde_json::to_vec(line).map_err(io::Error::from)?;
        json.push(b'\n');
        self.file.write_all(&json)?;
        self.file.flush()
    }
}

/// Rebuilds a [`Conversation`] from the rollout file at `path` alone.
///
/// Reads every line, keeps the ones it recognises (`system` and `"turn"`),
/// and returns their content in file order -- it replays nothing and issues
/// no request. A `kind` this reader does not know (61E's `"cell"` lines, or
/// anything newer) is skipped. Only the *final* line is allowed to fail to
/// parse: an append-only log's last line is the one most likely to be
/// half-written by a process that died mid-append, so it is dropped and the
/// rest of the file is kept; a parse failure earlier in the file is a
/// genuinely corrupt rollout and is an error.
pub fn resume(path: &Path) -> io::Result<Conversation> {
    let file = File::open(path)?;
    let lines: Vec<String> = BufReader::new(file).lines().collect::<io::Result<_>>()?;

    let mut system = String::new();
    let mut messages = Vec::new();
    let last_index = lines.len().saturating_sub(1);

    for (index, raw) in lines.iter().enumerate() {
        let value: serde_json::Value = match serde_json::from_str(raw) {
            Ok(value) => value,
            Err(_) if index == last_index => continue,
            Err(err) => return Err(io::Error::new(io::ErrorKind::InvalidData, err)),
        };
        let kind = value.get("kind").and_then(|k| k.as_str()).unwrap_or("");
        if kind == SYSTEM_KIND || kind == "context" {
            if let Some(text) = value.get("text").and_then(|t| t.as_str()) {
                system = text.to_string();
            }
        } else if kind == RolloutKind::Turn.as_str() {
            let role = match value.get("role").and_then(|r| r.as_str()) {
                Some("user") => Role::User,
                Some("assistant") => Role::Assistant,
                _ => continue,
            };
            let text = value.get("text").and_then(|t| t.as_str()).unwrap_or("");
            if value.get("checkpoint").and_then(|v| v.as_bool()) == Some(true) {
                continue;
            }
            let blocks = value
                .get("blocks")
                .cloned()
                .and_then(|raw| serde_json::from_value::<Vec<PersistedBlock>>(raw).ok());
            let mut message = if let Some(blocks) = blocks {
                Message {
                    role,
                    content: blocks.into_iter().map(PersistedBlock::into_block).collect(),
                    historical: None,
                }
            } else {
                Message::text(role, text)
            };
            if role == Role::User {
                message.historical = value
                    .get("historical")
                    .and_then(|h| h.as_str())
                    .map(str::to_owned);
            }
            messages.push(message);
        }
        // Any other kind (e.g. `RolloutKind::Cell`'s `"cell"`) is skipped.
    }

    Ok(Conversation { system, messages })
}

/// The latest provider-only checkpoint and the visible-message index after
/// which subsequent turns must be appended. Checkpoint rows are operational
/// metadata, not chat messages, so [`resume`] keeps the full visible chat
/// while this restores the smaller provider projection after restart.
pub fn resume_checkpoint(path: &Path) -> io::Result<Option<(String, usize)>> {
    let file = File::open(path)?;
    let lines: Vec<String> = BufReader::new(file).lines().collect::<io::Result<_>>()?;
    let mut visible_messages = 0usize;
    let mut checkpoint = None;
    for (index, raw) in lines.iter().enumerate() {
        let value: serde_json::Value = match serde_json::from_str(raw) {
            Ok(value) => value,
            Err(_) if index + 1 == lines.len() => continue,
            Err(err) => return Err(io::Error::new(io::ErrorKind::InvalidData, err)),
        };
        if value.get("kind").and_then(|v| v.as_str()) != Some(RolloutKind::Turn.as_str()) {
            continue;
        }
        if value.get("checkpoint").and_then(|v| v.as_bool()) == Some(true) {
            checkpoint = Some((
                value
                    .get("text")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_string(),
                visible_messages,
            ));
        } else if matches!(
            value.get("role").and_then(|v| v.as_str()),
            Some("user" | "assistant")
        ) {
            visible_messages += 1;
        }
    }
    Ok(checkpoint)
}

/// Restore the latest persisted display view for each ordinal. Older logs
/// simply return no views; nothing is inferred from model text.
pub fn resume_views(path: &Path) -> io::Result<Vec<(usize, CellView)>> {
    let file = File::open(path)?;
    let lines: Vec<String> = BufReader::new(file).lines().collect::<io::Result<_>>()?;
    let mut views = std::collections::BTreeMap::new();
    for (index, raw) in lines.iter().enumerate() {
        let value: serde_json::Value = match serde_json::from_str(raw) {
            Ok(value) => value,
            Err(_) if index + 1 == lines.len() => continue,
            Err(err) => return Err(io::Error::new(io::ErrorKind::InvalidData, err)),
        };
        if value.get("kind").and_then(|v| v.as_str()) != Some("view") {
            continue;
        }
        let line: ViewLine = serde_json::from_value(value)
            .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))?;
        views.insert(line.ordinal, line.view);
    }
    Ok(views.into_iter().collect())
}
