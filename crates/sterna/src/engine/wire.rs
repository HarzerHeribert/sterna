//! The seam's words (`docs/engine.md`): what a client sends a session, a
//! [`Command`], and what a session sends its clients, an [`Envelope`]
//! around an [`Event`]. One JSON object per line; the terminal receives the
//! same values in process, unserialised.

use serde::{Deserialize, Serialize};

use crate::contract::{Conversation, ServedBy};
use crate::tui::{Activity, Notebook, Stopper};

/// The protocol a hello names. A client that names another is refused.
pub const PROTOCOL: u32 = 1;

/// The terminal's name on the seam: what a prompt it answered is settled by.
pub const TERMINAL: &str = "terminal";

/// A client's first line.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Hello {
    pub token: String,
    pub protocol: u32,
    #[serde(default)]
    pub client: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HelloLine {
    pub hello: Hello,
}

/// What a client asks of a session.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "do", rename_all = "snake_case")]
pub enum Command {
    /// A snapshot and then every event after it; with `from`, every event
    /// from that sequence number instead.
    Attach {
        #[serde(default)]
        from: Option<u64>,
    },
    /// A message, held in the session's queue while a turn runs.
    Submit {
        text: String,
        #[serde(default)]
        images: Vec<String>,
    },
    /// The newest queued message comes off the queue.
    TakeBack,
    /// Stop after this cell.
    Stop {
        #[serde(default)]
        by: Option<StoppedBy>,
    },
    /// Cancel the call in flight.
    Cancel,
    Answer {
        prompt: u64,
        answer: Answer,
    },
    /// A slash command, as typed in a composer.
    Control {
        line: String,
    },
    /// The sandbox level by its word; `save` keeps it in the global settings.
    SetLevel {
        level: String,
        #[serde(default)]
        save: bool,
    },
    /// Forget an answer remembered for the session.
    Forget {
        id: String,
    },
    /// Let a host through, or stop letting it, for the session.
    Host {
        host: String,
        allow: bool,
    },
    SignInPaste {
        text: String,
    },
    SignInCancel,
    /// Undo the newest cell that changed files, without a preview.
    Rollback,
    /// End this session, and start `id` in its place.
    Resume {
        id: String,
    },
    End,
}

/// Who asked for a stop: a person, or an interrupt (Ctrl-C).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StoppedBy {
    You,
    Interrupt,
}

impl From<Stopper> for StoppedBy {
    fn from(stopper: Stopper) -> Self {
        match stopper {
            Stopper::You => Self::You,
            Stopper::Interrupt => Self::Interrupt,
        }
    }
}

impl From<StoppedBy> for Stopper {
    fn from(by: StoppedBy) -> Self {
        match by {
            StoppedBy::You => Self::You,
            StoppedBy::Interrupt => Self::Interrupt,
        }
    }
}

/// An answer to a prompt.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Answer {
    Approval(Choice),
    /// Refuse the call, and say what to do instead.
    Redirect(String),
    /// One of a question's choices, by its text.
    Choice(String),
    /// A form's fields, in order.
    Form(Vec<String>),
    /// Nobody answers: a question is left to Sterna, a form is put away, an
    /// approval is refused this once.
    Dismiss,
}

/// An approval's answers, as `approval::Decision` names them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Choice {
    AllowOnce,
    AllowForSession,
    Deny,
    DenyOnce,
    Cancel,
    AllowHostSession,
    AllowHostAlways,
}

/// One event, numbered and timed.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Envelope {
    pub seq: u64,
    /// When it happened, in Unix milliseconds.
    pub at: u64,
    #[serde(flatten)]
    pub event: Event,
}

/// What a session tells its clients.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Event {
    Snapshot {
        state: Box<State>,
    },
    /// The record changed. `activity` is `None` when a control was answered,
    /// which leaves the turn's ending and its clock alone.
    Transcript {
        conversation: Box<Conversation>,
        notebook: Box<Notebook>,
        served: ServedBy,
        #[serde(default, with = "activity_word_opt")]
        activity: Option<Activity>,
        reading: Reading,
    },
    Activity {
        #[serde(with = "activity_word")]
        activity: Activity,
        since: u64,
    },
    Delta {
        text: String,
    },
    ToolDelta {
        text: String,
    },
    Reasoning {
        text: String,
    },
    Prompt {
        prompt: Prompt,
    },
    /// The decision model's reading of an approval arrived.
    Hint {
        id: u64,
        fits: f64,
    },
    Settled {
        id: u64,
        by: String,
        answer: Answer,
    },
    Queue {
        items: Vec<String>,
    },
    Notice {
        text: String,
    },
    Panel {
        panel: Box<crate::tui::Panel>,
    },
    Facts {
        facts: Facts,
    },
    Usage {
        usage: Usage,
    },
    /// What was answered for the session, as the Sandbox sheet lists it.
    Memory {
        entries: Vec<Remembered>,
    },
    /// The hosts a command may reach now.
    Hosts {
        hosts: Vec<String>,
    },
    SignIn {
        sign_in: SignIn,
    },
    /// A chip offered first on the opening screen: its label, and what it
    /// sends.
    Suggest {
        label: String,
        types: String,
    },
    Unsuggest {
        types: String,
    },
    /// A command this client sent was not taken.
    Refused {
        to: String,
        reason: String,
    },
    Ended {
        reason: String,
    },
}

/// What a running sign-in says.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "step", rename_all = "snake_case")]
pub enum SignIn {
    Started { label: String },
    Note { text: String },
    Panel { panel: Box<crate::tui::Panel> },
    Done,
}

/// Everything a client needs to draw a session as it stands.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct State {
    pub session: String,
    pub seq: u64,
    pub facts: Facts,
    pub conversation: Conversation,
    pub notebook: Notebook,
    pub served: ServedBy,
    #[serde(with = "activity_word")]
    pub activity: Activity,
    pub since: u64,
    pub streaming: Streaming,
    pub queue: Vec<String>,
    pub prompts: Vec<Prompt>,
    pub notes: Vec<String>,
    pub memory: Vec<Remembered>,
    pub hosts: Option<Vec<String>>,
    pub sign_in: Option<String>,
    pub suggestions: Vec<(String, String)>,
    pub reading: Reading,
    pub usage: Usage,
}

/// Text that has arrived for the request in flight and is not yet part of
/// the record.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Streaming {
    pub text: Option<String>,
    pub tool: Option<String>,
    pub reasoning: Option<String>,
}

/// What the session is: its model, effort and level, and the facts about
/// how it is confined.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Facts {
    pub model: Option<String>,
    pub effort: String,
    pub level: String,
    pub root: String,
    pub project: Option<String>,
    pub sandbox: Option<String>,
    pub confinement: Option<String>,
    pub network: Option<String>,
    pub subagents: Option<String>,
    pub settings_models: Vec<String>,
}

/// Tokens the session's requests used, as the providers counted them.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Usage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub reasoned_tokens: u64,
    pub requests: u64,
}

/// One answer remembered for the session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Remembered {
    pub id: String,
    pub label: String,
    pub allowed: bool,
}

impl From<crate::approval::Remembered> for Remembered {
    fn from(remembered: crate::approval::Remembered) -> Self {
        Self {
            id: remembered.id,
            label: remembered.label,
            allowed: remembered.allowed,
        }
    }
}

/// Something that waits for a person's answer.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Prompt {
    pub id: u64,
    #[serde(flatten)]
    pub asks: Asks,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Asks {
    /// One exact call the level puts to a person.
    Approval {
        tool: String,
        root: String,
        arguments: std::collections::BTreeMap<String, String>,
        /// The tool and what it acts on, in the project's terms.
        label: String,
        target: String,
        /// The call exactly, as the confirmation shows it.
        confirmation: String,
        /// Whether the whole call fits the confirmation; a call that does
        /// not cannot be allowed.
        complete: bool,
        reason: Option<String>,
        hosts: Vec<String>,
        leaves_sandbox: bool,
        fits: Option<f64>,
    },
    /// A question a cell asked.
    Question {
        question: String,
        choices: Vec<String>,
        /// The decision model's share of each choice, when it read the
        /// question; its pick is `guess`.
        weights: Option<Vec<f64>>,
        guess: Option<String>,
    },
    /// A sheet of fields. Its values travel only in the answer.
    Form { form: Box<crate::tui::Form> },
}

/// How a session reads, decided once (`reading.rs`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Reading {
    pub cells: Vec<CellReading>,
    pub answer: Option<AnswerReading>,
}

/// One cell's card: the state on its top edge and the line on its bottom.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct CellReading {
    pub cell: usize,
    /// `EXECUTED`, `FAILED`, `ROLLED BACK`, `RECORDED`.
    pub state: String,
    /// The mark drawn before the state: `✓`, `✕`, `↶`, or nothing.
    pub mark: String,
    /// `success`, `failure`, `warning` or `muted`: what the state means,
    /// for a client to give it its colour.
    pub tone: String,
    /// The bottom edge: how it ended and how many files it changed.
    pub line: String,
    /// The same line in its parts, each with its tone.
    pub parts: Vec<Part>,
    /// The facts under the answer this cell returned, when it returned one.
    pub facts: Option<AnswerReading>,
}

/// Words and what they mean, for a client to colour: `success`, `failure`,
/// `warning`, `muted` or `line`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Part {
    pub text: String,
    pub tone: String,
}

impl Part {
    #[must_use]
    pub fn new(text: impl Into<String>, tone: &str) -> Self {
        Self {
            text: text.into(),
            tone: tone.into(),
        }
    }
}

/// The facts under a turn's answer.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct AnswerReading {
    pub cell: usize,
    pub mark: String,
    pub failed: bool,
    /// `1 file · +1 −0 · 1 call`, or `complete.` when there are none.
    pub facts: String,
}

/// An activity as one word: `stopped` for a stop by either hand.
pub mod activity_word {
    use crate::tui::{Activity, Stopper};
    use serde::{Deserialize, Deserializer, Serializer};

    #[must_use]
    pub fn word(activity: Activity) -> &'static str {
        match activity {
            Activity::Idle => "idle",
            Activity::Starting => "starting",
            Activity::Thinking => "thinking",
            Activity::Streaming => "streaming",
            Activity::Executing => "executing",
            Activity::Searching => "searching",
            Activity::Waiting => "waiting",
            Activity::Compacting => "compacting",
            Activity::AwaitingYou => "awaiting_you",
            Activity::Complete => "complete",
            Activity::Failed => "failed",
            Activity::Stopped(Stopper::You) => "stopped",
            Activity::Stopped(Stopper::Interrupt) => "interrupted",
        }
    }

    #[must_use]
    pub fn parse(word: &str) -> Option<Activity> {
        Some(match word {
            "idle" => Activity::Idle,
            "starting" => Activity::Starting,
            "thinking" => Activity::Thinking,
            "streaming" => Activity::Streaming,
            "executing" => Activity::Executing,
            "searching" => Activity::Searching,
            "waiting" => Activity::Waiting,
            "compacting" => Activity::Compacting,
            "awaiting_you" => Activity::AwaitingYou,
            "complete" => Activity::Complete,
            "failed" => Activity::Failed,
            "stopped" => Activity::Stopped(Stopper::You),
            "interrupted" => Activity::Stopped(Stopper::Interrupt),
            _ => return None,
        })
    }

    pub fn serialize<S: Serializer>(activity: &Activity, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(word(*activity))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Activity, D::Error> {
        let word = String::deserialize(deserializer)?;
        parse(&word).ok_or_else(|| serde::de::Error::custom(format!("no activity {word:?}")))
    }
}

mod activity_word_opt {
    use crate::tui::Activity;
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(
        activity: &Option<Activity>,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        match activity {
            Some(activity) => serializer.serialize_str(super::activity_word::word(*activity)),
            None => serializer.serialize_none(),
        }
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Option<Activity>, D::Error> {
        Option::<String>::deserialize(deserializer)?
            .map(|word| {
                super::activity_word::parse(&word)
                    .ok_or_else(|| serde::de::Error::custom(format!("no activity {word:?}")))
            })
            .transpose()
    }
}

/// Unix milliseconds now.
#[must_use]
pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| since.as_millis() as u64)
}
