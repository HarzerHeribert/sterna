//! Fullscreen presentation. The caller owns terminal lifecycle, input and ticks.

pub mod background;
mod controls;
pub mod history;
pub use history::{HistoryNote, NoteKind};
mod bands;
mod message;
use message::*;
mod composer;
mod paths;
pub(crate) use paths::{forget as forget_paths, found as found_paths, resolve as resolve_path};
mod poster;
mod regions;
mod selection;
use composer::{composer_cursor, wrapped_input};
pub use selection::Selection;
pub(crate) use selection::draw as draw_selection;

pub mod form;
mod lane;
mod look;
pub(crate) mod theme;
pub use form::Form;
pub use look::Motion;
pub use theme::{Family, Theme};
mod markdown;
mod ribbon;
mod scroll;
mod value;
pub use scroll::SCROLL_INDICATOR_LINGER;
use scroll::render_scrollbar;
pub(crate) mod status;
use regions::{
    push_changes, push_error_region, push_folded_region, push_output_region, push_text_region,
};
use status::{compact_tokens, context_summary, footer_row};
pub(crate) mod telemetry;
pub use controls::{Assignment, Catalogue, ModelGroup, Panel, PanelRow, StatusLine, TierModels};
pub(crate) use lane::helper_in_flight;
use lane::{helper_fold, helper_lane, push_helper_lane};
pub use telemetry::Pulse;

use crate::commands::{BUILT_INS, BuiltIn};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};

use crate::contract::{Block as ContentBlock, Conversation, Message, Role, ServedBy};
use crate::helpers::HelperRecord;
use crate::prompt::{Extracted, extract_program};
use crate::runtime::handles::{HandleTable, render_table};
use crate::runtime::preview::PREVIEW_TOKEN_CAP;
use crate::runtime::preview::TABLE_TOKEN_CAP;

const ACCENT: Color = Color::LightGreen;
const MUTED: Color = Color::Gray;
const NOT_CONNECTED: &str = "gateway not connected.";

impl ScreenState {
    /// Whether this session runs without a sandbox right now.
    #[must_use]
    pub fn full_access(&self) -> bool {
        self.level.level() == crate::permissions::Level::Full
    }
}

/// Session-owned presentation state. Missing instrumentation stays unknown.
/// Pass this to `render_screen` on input, resize, runtime events and activity ticks.
#[derive(Debug, Clone, Default)]
pub struct ScreenState {
    pub model: Option<String>,
    pub project: Option<String>,
    pub sandbox: Option<String>,
    /// Whether Sterna confines the children this session spawns, as one word
    /// (`confined`, `unconfined`). Separate from [`ScreenState::sandbox`]
    /// because the posture row cannot always afford it: `tui_live`'s
    /// `telemetry_and_motion_are_local_controls_with_real_response_usage`
    /// pins the context reading as this row's highest-priority right-edge
    /// signal, and at 60 columns a longer left half takes it away. The
    /// startup line and `sterna doctor` say it in full on every width.
    pub confinement: Option<String>,
    pub network: Option<String>,
    /// Whether the little helpers are configured to run, and how work is
    /// handed to a subagent -- the two facts the status strip offers as
    /// controls.
    ///
    /// They are on the screen's own state rather than read from the config
    /// at draw time because the renderer may not touch a `RefCell` the
    /// session thread owns; the session sets them when it starts and
    /// whenever they change.
    pub helpers_on: bool,
    pub subagents: Option<String>,
    pub connected: Option<bool>,
    pub input: String,
    /// UTF-8 byte offset supplied by the live editor; None hides the cursor.
    pub cursor: Option<usize>,
    pub completion_selected: usize,
    /// What the composer's popup offers for the draft as it stands: the
    /// commands a slash word matches, or the paths an `@` word does. Empty
    /// when there are none or the popup was put away.
    pub completions: Vec<(String, String)>,
    /// A keystroke hint the next keystroke replaces; everything a person may
    /// want to read again is a [`HistoryNote`] instead.
    pub notice: Option<String>,
    /// Messages submitted while a task was running, in the order they were
    /// sent, waiting for the turn to end.
    ///
    /// **A keystroke must not clear this, which is why it is not a
    /// [`notice`](ScreenState::notice).** Someone who queues a message and
    /// keeps typing has to be able to see what they already handed over;
    /// the whole point of the queue is that they stopped watching.
    pub queued: Vec<String>,
    /// A stop was asked for and the turn has not ended yet -- the state in
    /// which a second Escape means *cancel the call in flight* rather than
    /// *stop after this cell*.
    pub stopping: bool,
    /// Notices kept in the conversation, in arrival order.
    pub history: Vec<HistoryNote>,
    /// Messages in the conversation this state last drew, where a new note goes.
    pub messages_seen: usize,
    /// A modal masked prompt, open over the composer. While it is set the
    /// composer is not what the keyboard reaches.
    /// A form sheet that has the keyboard: a key, an endpoint, a pasted
    /// sign-in address. Nothing typed into it reaches the editor.
    pub form: Option<Form>,
    /// Fold long code and previews locally; never changes the model messages.
    pub compact: bool,
    pub pretty: bool,
    pub activity: Activity,
    /// Partial provider text for the active response; never persisted as a completed turn.
    /// The streaming caller replaces this accumulated text and clears it on completion.
    pub streaming_text: Option<String>,
    /// Raw native tool-input fragments, presentation-only; never executed or shown as results.
    pub streaming_tool_input: Option<String>,
    /// The model's readable reasoning for the response in flight, shown live
    /// and cleared with the rest of the stream; the conversation keeps the
    /// block itself.
    pub streaming_reasoning: Option<String>,
    pub animation_frame: usize,
    pub completion_tick: Option<usize>,
    /// Rows back from the transcript's end; zero follows the current turn.
    pub scrollback: usize,
    /// User preference, retained across resizes. The caller toggles this field.
    pub sidebar: SidebarVisibility,
    /// Chrome off, composer kept: the transcript takes the whole terminal.
    ///
    /// It overrides `sidebar`, `status_line` and the activity ribbon for as
    /// long as it is set rather than writing to them, which is why leaving
    /// fullscreen restores exactly the layout the user had before entering.
    /// The composer is never part of the hide-set: a screen that cannot be
    /// typed into has taken something away rather than given room back.
    pub fullscreen: bool,
    /// A scroll happened recently, so the position indicator is up. The
    /// caller owns the clock and clears this after
    /// [`SCROLL_INDICATOR_LINGER`]; the renderer only obeys it, which keeps
    /// the drawing pure and testable.
    pub scrolling: bool,
    /// Mouse reporting is released to the terminal, so the person can select
    /// and copy with a drag. Clicks do not land while this is set, which is
    /// why the status line says so (the user, 2026-09-17: click-and-drag
    /// selection must stay available).
    pub mouse_off: bool,
    /// A drag in progress or just finished, in screen cells. Drawn over
    /// everything else, because a person selects what they can see.
    pub selection: Option<selection::Selection>,
    pub theme: Theme,
    pub settings_root: Option<std::path::PathBuf>,
    /// The person's own settings folder. `None` -- a test, a machine with no
    /// home -- saves nothing globally, so no test reaches the real one.
    pub settings_global: Option<std::path::PathBuf>,
    pub settings_profile: Option<String>,
    pub settings_models: Vec<String>,
    /// The subscription a sign-in is running for, beside the session: the
    /// dock's chip that brings its panel back.
    pub signing_in: Option<String>,
    /// The sandbox level, shared live with the approval gate: the settings
    /// sheet changes it from this thread while a task runs.
    pub level: crate::permissions::LiveLevel,
    /// The approval gate's memory: every call answered for the whole
    /// session, which the Sandbox sheet lists and can forget.
    pub memory: Option<crate::approval::Memory>,
    /// The live list of hosts the session's network proxy lets through,
    /// shared with it: the hosts sheet changes it from this thread. `None`
    /// when no proxy runs.
    pub allowed: Option<crate::sandbox::proxy::Allowed>,
    pub effort: crate::wire::Effort,
    pub status_line: StatusLine,
    pub panel: Option<Panel>,
    pub telemetry_open: bool,
    pub telemetry_selected: Option<usize>,
    pub reduced_motion: bool,
    /// The terminal shows 24-bit colour, so a bird theme's sprite can be drawn.
    pub truecolor: bool,
    /// The `ui.background` choice.
    pub background: background::Background,
    /// The ground is light: what [`ScreenState::background`] comes to on
    /// this terminal, resolved when it is set and once the terminal has
    /// answered.
    pub light: bool,
    pub pulse: Pulse,
    /// How many of the notes in [`ScreenState::history`] are the session's
    /// own opening, and therefore belong to the card at the top rather than
    /// to the conversation.
    ///
    /// **It is frozen by the first keystroke, not computed per frame.** The
    /// notes a session emits before anyone has touched it -- its resume id,
    /// its rung, three sentences about the sandbox -- are a header; a notice
    /// the person then *causes*, at a conversation that is still empty, is a
    /// reply to them and has to be visible where they are looking. Both sets
    /// carry `after == 0`, so only the moment they were produced tells them
    /// apart. `None` means nothing has frozen it yet.
    pub startup_notes: Option<usize>,
    /// The completion gate's recap of the task just accepted, when
    /// `[helpers] completion = "recap"` asked for one.
    ///
    /// `None` -- the silent default, no helper model, or a call that never
    /// came back -- renders nothing at all, and neither does a record that
    /// failed: a recap must never replace or delay the answer it follows.
    /// The caller clears it when the next task begins.
    pub recap: Option<HelperRecord>,
    /// How much moves (`ui.motion`); `reduced_motion` is kept equal to `Off`.
    pub motion: Motion,
    /// Work running behind the answer (the checker, the notes writer), by name.
    pub behind: Vec<String>,
    /// A verdict that just landed in `history`, and its settle frame.
    pub note_landing: Option<(usize, usize)>,
    /// What is shown of a cell while the model is still writing it.
    pub stream: Stream,
    /// The local hour when the session started, for the greeting; `None`
    /// when the platform could not say, and the greeting then has no time
    /// of day in it.
    pub local_hour: Option<u8>,
    /// What the opening offers to do, read from the project itself: each
    /// entry is the chip's label and the message it puts in the composer.
    pub suggestions: Vec<(String, String)>,
}

/// Accent-only themes inherit the terminal background and its transparency.
/// What the screen shows while the model is still writing a cell
/// (`ui.stream`): one row per action with a live character count, the
/// program as it forms, or the raw protocol text the provider sends.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Stream {
    /// The cell laid out by what it will do -- each call on its own row,
    /// counting its characters as they arrive -- because unformatted code
    /// arriving token by token is unpleasant to watch.
    #[default]
    Actions,
    Code,
    Raw,
}
impl Stream {
    pub const ALL: [Self; 3] = [Self::Actions, Self::Code, Self::Raw];
    pub fn parse(name: &str) -> Option<Self> {
        match name {
            "actions" => Some(Self::Actions),
            "code" => Some(Self::Code),
            "raw" => Some(Self::Raw),
            _ => None,
        }
    }
    pub fn name(self) -> &'static str {
        match self {
            Self::Actions => "actions",
            Self::Code => "code",
            Self::Raw => "raw",
        }
    }
    pub fn next(self) -> Self {
        match self {
            Self::Actions => Self::Code,
            Self::Code => Self::Raw,
            Self::Raw => Self::Actions,
        }
    }
}
/// Auto needs a comfortable reading column; Shown can use a tighter one.
/// Below 80 columns even an explicit request collapses to preserve the editor.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum SidebarVisibility {
    #[default]
    Auto,
    Hidden,
    Shown,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Activity {
    #[default]
    Idle,
    Starting,
    Thinking,
    Streaming,
    Executing,
    Searching,
    Waiting,
    Compacting,
    /// An approval or a question is on screen and the turn waits on the
    /// person: the clock stands still until it is answered.
    AwaitingYou,
    Complete,
    Failed,
    /// The turn was stopped before it finished; what ran stands.
    Stopped(Stopper),
}

/// Who stopped a turn: the person, with Esc, or an interrupt (Ctrl-C).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stopper {
    You,
    Interrupt,
}

impl Activity {
    /// A turn is under way: the model, a cell or the person's answer is
    /// what the session waits on. A local control is never one.
    pub fn working(self) -> bool {
        matches!(
            self,
            Self::Thinking
                | Self::Streaming
                | Self::Executing
                | Self::Searching
                | Self::Waiting
                | Self::Compacting
                | Self::AwaitingYou
        )
    }

    /// Fixed four-cell machinery; only the current header moves.
    pub fn indicator(self, tick: usize) -> &'static str {
        let frames = match self {
            Self::Starting => [".  .", "+--.", "+--+", "|/|/"],
            Self::Thinking => [" .  ", " <> ", "<..>", " <> "],
            Self::Streaming => [">...", ".>..", "..>.", "...>"],
            Self::Executing => ["|>..", "|=>.", "|==>", "|..>"],
            Self::Searching => ["/.. ", "./. ", "../ ", "./. "],
            Self::Waiting => ["(  )", "( .)", "(..)", "(. )"],
            Self::Compacting => [">  <", " >< ", " [] ", " >< "],
            Self::AwaitingYou => [" ?? "; 4],
            Self::Idle => [" -- "; 4],
            Self::Complete => [" OK "; 4],
            Self::Failed => [" !! "; 4],
            Self::Stopped(_) => [" || "; 4],
        };
        frames[tick % frames.len()]
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Idle => "ready",
            Self::Starting => "assembling",
            Self::Thinking => "thinking",
            Self::Streaming => "receiving",
            Self::Executing => "executing",
            Self::Searching => "searching",
            Self::Waiting => "waiting",
            Self::Compacting => "compacting",
            Self::AwaitingYou => "waiting for you",
            Self::Complete => "complete",
            Self::Failed => "failed",
            Self::Stopped(_) => "stopped",
        }
    }
}

/// Queued messages listed one per row before the rest collapse to a count.
/// Three is what fits over the composer at the heights this screen is drawn
/// at without pushing the transcript out of view.
pub const QUEUE_ROWS: usize = 3;

/// Disjoint hard bounds shared by the renderer and its structural tests.
#[derive(Debug, Clone, Copy)]
pub struct ScreenRegions {
    pub header: Rect,
    pub transcript: Rect,
    pub details: Rect,
    pub completions: Rect,
    pub notice: Rect,
    pub activity: Rect,
    pub input: Rect,
    pub status: Rect,
}

pub fn screen_regions(area: Rect, state: &ScreenState) -> ScreenRegions {
    let status_h = if state.fullscreen {
        0
    } else {
        area.height.min(match state.status_line {
            StatusLine::Full => {
                if area.width < 140 {
                    3
                } else {
                    2
                }
            }
            StatusLine::Compact => 1,
            StatusLine::Hidden => 0,
        })
    };
    let status = Rect::new(area.x, area.bottom() - status_h, area.width, status_h);
    let remaining = status.y - area.y;
    let input_h = (wrapped_input(state, area.width)
        .len()
        .min(usize::from((area.height / 3).max(1))) as u16)
        .saturating_add(2)
        .max(3)
        .min(remaining);
    let input = Rect::new(area.x, status.y - input_h, area.width, input_h);
    // A masked prompt owns the composer: the editor's text is still there,
    // but it is not what the next keystroke goes to, so completions for it
    // would be an offer the keyboard cannot take.
    let completion_h = (completions_shown(state).len() as u16)
        .min(7)
        .min((input.y - area.y).saturating_sub(5));
    let completions = Rect::new(area.x, input.y - completion_h, area.width, completion_h);
    // The queue shares the notice's row because it belongs in the same
    // place -- directly over the composer, where someone who just typed is
    // already looking -- but it is not a notice: it outlives keystrokes.
    let notice_rows = if state.notice.is_some() { 2 } else { 0 };
    let queued_rows =
        (state.queued.len().min(QUEUE_ROWS) as u16) + u16::from(state.queued.len() > QUEUE_ROWS);
    let notice_h = if notice_rows + queued_rows > 0 {
        (1 + notice_rows + queued_rows).min((completions.y - area.y).saturating_sub(4))
    } else {
        0
    };
    let notice = Rect::new(area.x, completions.y - notice_h, area.width, notice_h);
    let header_h = if state.fullscreen {
        0
    } else {
        (notice.y - area.y).min(2)
    };
    let header = Rect::new(area.x, area.y, area.width, header_h);
    let available_body = notice.y - header.bottom();
    let moving = matches!(
        state.activity,
        Activity::Thinking
            | Activity::Streaming
            | Activity::Executing
            | Activity::Waiting
            | Activity::Searching
            | Activity::Compacting
    ) || state.completion_tick.is_some();
    let activity_h = if moving
        && !state.fullscreen
        && !state.telemetry_open
        && area.height >= 28
        && area.width >= 60
        && available_body >= 18
    {
        if area.width >= 100 { 3 } else { 2 }
    } else {
        0
    };
    let activity = Rect::new(
        area.x,
        notice.y - activity_h,
        area.width.min(180),
        activity_h,
    );
    let body_h = activity.y - header.bottom();
    let sidebar_visible = !state.fullscreen
        && match state.sidebar {
            SidebarVisibility::Auto => area.width >= 120,
            SidebarVisibility::Hidden => false,
            SidebarVisibility::Shown => area.width >= 80,
        };
    let transcript_w = if sidebar_visible {
        area.width.saturating_sub(36)
    } else {
        area.width
    };
    let transcript = Rect::new(area.x, header.bottom(), transcript_w, body_h);
    let details = Rect::new(
        if sidebar_visible {
            area.right() - 34
        } else {
            area.right()
        },
        header.bottom(),
        if sidebar_visible { 34 } else { 0 },
        body_h,
    );
    ScreenRegions {
        header,
        transcript,
        details,
        completions,
        notice,
        activity,
        input,
        status,
    }
}

/// Only real built-ins, with descriptions of their vocabulary rather than
/// claims that a command was successfully executed.
/// The completions this screen is offering, which is none at all while a
/// masked prompt has the keyboard.
fn completions_shown(state: &ScreenState) -> Vec<(String, &'static str)> {
    if state.form.is_some() {
        return Vec::new();
    }
    slash_matches(&state.input)
}

pub fn slash_matches(input: &str) -> Vec<(String, &'static str)> {
    let Some(prefix) = input.strip_prefix('/') else {
        return Vec::new();
    };
    if prefix.chars().any(char::is_whitespace) {
        return Vec::new();
    }
    let matches = BUILT_INS
        .iter()
        .filter(|command| command.name().starts_with(prefix))
        .map(|command| {
            (
                format!("/{}", command.name()),
                match command {
                    BuiltIn::Model => "set the parent, helper or subagent model",
                    BuiltIn::Models => "browse models by agent, provider or intelligence",
                    BuiltIn::Login => "sign in: a subscription, an API key or your own endpoint",
                    BuiltIn::Setup => "set Sterna up: sign in, models for each workload, Jev",
                    BuiltIn::Rollback => "undo the files the newest cell changed",
                    BuiltIn::Memory => "read or save project memory",
                    BuiltIn::Exit => "end the session, printing its resume id",
                },
            )
        })
        .chain(
            [
                (
                    "/diff".to_string(),
                    "inspect the last cell before/after diff",
                ),
                (
                    "/subagents".to_string(),
                    "configure explicit favorite slots · on|off|SLOT MODEL [EFFORT]",
                ),
                ("/help".to_string(), "show available commands"),
                ("/theme".to_string(), "choose a bird or a classic palette"),
                (
                    "/telemetry".to_string(),
                    "live activity, requests and execution · Ctrl-T",
                ),
                ("/motion".to_string(), "full, calm or off · how much moves"),
                (
                    "/stream".to_string(),
                    "actions, code or raw · what a cell shows while it is written",
                ),
                ("/cells".to_string(), "open every cell's card"),
                (
                    "/cell".to_string(),
                    "open the newest cell that ran · /cell 12 for another",
                ),
                ("/chat".to_string(), "return to the conversation"),
                (
                    "/key".to_string(),
                    "enter a provider API key · /key anthropic",
                ),
                (
                    "/effort".to_string(),
                    "auto, low, medium, high, xhigh or max",
                ),
                (
                    "/context".to_string(),
                    "what fills the context window, by kind",
                ),
                (
                    "/status".to_string(),
                    "this session: models, sandbox, and each subscription's limits",
                ),
                (
                    "/resume".to_string(),
                    "go back to an earlier session in this folder",
                ),
                ("/settings".to_string(), "Global / Project settings"),
                (
                    "/config".to_string(),
                    "advanced settings · limits, thresholds, the web broker",
                ),
                ("/statusline".to_string(), "full, compact or hidden status"),
                (
                    "/fullscreen".to_string(),
                    "transcript only, composer kept · Ctrl-F",
                ),
                (
                    "/sandbox".to_string(),
                    "how much runs without asking · Ask, Sandboxed or Full access",
                ),
                (
                    "/plan".to_string(),
                    "plan one request · it reads, and writes only the plan",
                ),
                // Three commands that worked and were in no list, which is
                // how a command that works comes to look like one Sterna does
                // not have -- the same defect `/exit` was fixed for.
                (
                    "/tool".to_string(),
                    "run one tool directly · /tool read path=…",
                ),
                (
                    "/mouse".to_string(),
                    "release or recapture the mouse · Ctrl-G",
                ),
            ]
            .into_iter()
            .filter(|(name, _)| name.trim_start_matches('/').starts_with(prefix)),
        )
        .collect::<Vec<_>>();
    // The name typed in full comes first, so Enter on "/plan" runs /plan,
    // and "/cell" runs /cell and not /cells.
    let mut matches = matches;
    if let Some(at) = matches.iter().position(|(name, _)| name[1..] == *prefix) {
        let exact = matches.remove(at);
        matches.insert(0, exact);
    }
    matches
}

/// What one cell produced, beside the assistant message the notebook already
/// draws.
///
/// **Every field arrives already rendered or already plain.** The runtime
/// hands its caller a rendered handle table and a rendered preview and never
/// its table or its value, so this module turns no live object into text --
/// which is the invariant `tests/tui.rs::the_tui_renders_no_handle_itself`
/// scans this file for.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct CellView {
    /// Helper calls this cell made, in call order -- the lane while they run
    /// and the `HELPERS` inspector section afterwards read this one field.
    pub helpers: Vec<crate::helpers::HelperRecord>,
    /// Corrected source for an executed sterna-edit; display only, never another model message.
    pub executed_source: Option<String>,
    /// The one line the model wrote about what this cell is for, drawn above
    /// the cell (`docs/workbench.md`). `None` for a cell
    /// whose model said nothing and for every rollout row written before the
    /// field existed; the screen then falls back to what it drew before.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Who authored the frame this view shows.
    ///
    /// The screen may not imply that a direct provider call was written as
    /// JavaScript by the model (`tool-abi.md` §19): the source of a lowered
    /// frame is Sterna's spelling of the call, and labelling it the model's own
    /// would misreport what happened. A rollout written before this field
    /// existed deserializes as an authored cell, which is what it was.
    pub origin: crate::abi::Origin,
    pub repaired_from: Option<u64>,
    /// Local before/after file diff. Never included in model context.
    pub changes: Option<String>,
    /// The handle table as this cell ended, already rendered by the one
    /// renderer. `None` for a cell the notebook never saw run -- a resumed
    /// session's earlier cells came from the rollout file.
    pub table: Option<String>,
    /// Already bounded by the runtime; display only, never an extra model message.
    pub stdout: Option<String>,
    /// A non-terminal structured value returned for notebook inspection.
    pub output: Option<String>,
    /// Recorded tool outcomes, not calls inferred from generated source.
    pub execution: Option<String>,
    /// Host call count from the runtime record; never inferred from display lines.
    pub call_count: Option<usize>,
    /// A throw, `runtime-contract.md` §5.
    pub error: Option<CellError>,
    /// A top-level `return`'s terminal response, already rendered by the
    /// caller (§1, `runtime-contract.md` §9.2).
    pub returned: Option<String>,
    /// Why the cell yielded on purpose (§9.3), drawn in the output region
    /// beside the table and never in the error region: it is not an error.
    pub yield_reason: Option<String>,
    /// Whether the user message that follows this cell is the runtime's own
    /// answer to it rather than a person typing. Every section of that answer
    /// is already on screen as this cell's output, error and return regions,
    /// so drawing it again would put the handle table on the screen twice.
    pub answered: bool,
    /// The question this cell asked and who chose what, as its card's row.
    pub asked: Option<String>,
    /// `/rollback` undid what this cell changed.
    pub rolled_back: bool,
    /// The task capsule as it stood when this cell ended — goal, state,
    /// verified facts, risks and next action (`runtime::capsule`). Display
    /// and rollout state; the model receives it through the result block,
    /// never through this field. Absent on rows written before it existed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capsule: Option<serde_json::Value>,
}

/// A throw's class, message and position inside the model's own program --
/// `runtime-contract.md` §5's first two items, and nothing from inside the
/// runtime. The position is optional because the runtime could not always
/// attribute a throw to a line of the model's program.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct CellError {
    pub class: String,
    pub message: String,
    pub line: Option<u32>,
    pub column: Option<u32>,
}

/// Where the task's token total came from.
///
/// **It is a field rather than a footnote because the two figures are not
/// comparable.** `model-contract.md` §6 reads the gateway's own usage row
/// when there is one and estimates otherwise; a total that silently mixed
/// them would be the one number on this screen a reader would trust without
/// knowing what it was.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Counted {
    /// Every turn so far carried a provider-reported usage row.
    Gateway,
    /// No turn did; every figure in the total is `estimate_tokens`'.
    Estimated,
    /// Some turns reported and some did not.
    Mixed,
}

impl Counted {
    /// Short provenance label shared by telemetry and compact status.
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Counted::Gateway => "reported",
            Counted::Estimated => "estimated",
            Counted::Mixed => "part estimated",
        }
    }

    /// Who counted, in a sentence's words.
    pub(crate) fn by(self) -> &'static str {
        match self {
            Counted::Gateway => "counted by the provider",
            Counted::Estimated => "estimated",
            Counted::Mixed => "partly estimated",
        }
    }
}

/// Cumulative task spend and its provenance. It deliberately has no cap.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskTokens {
    pub used: u64,
    /// Parent task turns only. `used - parent_used` is never needed to infer
    /// helper spend because the complete helper breakdown is carried below.
    pub parent_used: u64,
    pub helpers: HelperTokens,
    pub counted: Counted,
}

/// Known helper usage and the coverage required to interpret it honestly.
/// Missing provider usage contributes no invented tokens.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HelperTokens {
    pub calls: u32,
    pub usage_known_calls: u32,
    pub used: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub requests: u32,
    pub reported_requests: u32,
    pub cache_read_input_tokens: u64,
    pub cache_creation_input_tokens: u64,
    pub cache_read_reported_requests: u32,
    pub cache_creation_reported_requests: u32,
    pub models: Vec<HelperModelTokens>,
}

impl HelperTokens {
    pub fn complete(&self) -> bool {
        self.usage_known_calls == self.calls
            && self.reported_requests == self.requests
            && self.cache_read_reported_requests == self.reported_requests
            && self.cache_creation_reported_requests == self.reported_requests
    }
}

/// One helper model's contribution to [`HelperTokens`]. The model name is
/// provider configuration, not a price tier: Sterna reports what ran and never
/// infers a rate from it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HelperModelTokens {
    pub model: String,
    pub calls: u32,
    pub usage_known_calls: u32,
    pub used: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub requests: u32,
    pub reported_requests: u32,
    pub cache_read_input_tokens: u64,
    pub cache_creation_input_tokens: u64,
    pub cache_read_reported_requests: u32,
    pub cache_creation_reported_requests: u32,
}

impl HelperModelTokens {
    pub fn complete(&self) -> bool {
        self.usage_known_calls == self.calls
            && self.reported_requests == self.requests
            && self.cache_read_reported_requests == self.reported_requests
            && self.cache_creation_reported_requests == self.reported_requests
    }
}

/// Occupancy of the most recent (or currently assembling) provider request.
/// This is intentionally separate from [`TaskTokens`], which accumulates the
/// cost of every request made for the task.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ContextTokens {
    pub used: u64,
    pub cap: Option<u64>,
    /// Where `cap` came from. The meter draws a percentage only against a
    /// figure somebody measured -- see [`crate::models::WindowSource`].
    pub cap_source: crate::models::WindowSource,
    pub counted: Counted,
}

/// What the session knows about the conversation beyond the messages
/// themselves: one view per assistant cell, in cell order, and the task's
/// token total.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Notebook {
    /// The pushed Scout running before the task model's first turn. This is
    /// presentation-only and is never persisted as a model-authored cell.
    pub preflight: Option<HelperRecord>,
    /// The task's acceptance list as it stands (`acceptance::standing`),
    /// empty when no list was derived. Presentation only, like `preflight`.
    pub acceptance: Vec<crate::acceptance::Verdict>,
    /// Where that list came from: the request's words, or the Scout's look.
    pub acceptance_from: crate::acceptance::Origin,
    pub inbox_depth: usize,
    pub batches_delivered: u64,
    pub handlers: Vec<crate::runtime::handlers::HandlerInfo>,
    pub requests: Vec<crate::telemetry::RequestMeasurement>,
    pub cells: Vec<CellView>,
    pub tokens: Option<TaskTokens>,
    pub context: Option<ContextTokens>,
    /// The decision model's summary line for this task
    /// (`decide::summary_line`), `None` only when no decision model is
    /// configured at all.
    pub decision: Option<String>,
}

impl Notebook {
    /// Records `view` as cell `ordinal`'s (1-based), padding with empty views
    /// for any earlier cell this notebook never saw -- a resumed session's
    /// cells came back from the rollout file, and padding is what keeps a new
    /// cell's view under the cell the screen numbers it.
    pub fn set(&mut self, ordinal: usize, view: CellView) {
        if ordinal == 0 {
            return;
        }
        if self.cells.len() < ordinal {
            self.cells.resize(ordinal, CellView::default());
        }
        self.cells[ordinal - 1] = view;
    }

    fn cell(&self, ordinal: usize) -> Option<&CellView> {
        self.cells.get(ordinal.checked_sub(1)?)
    }

    /// The cells that ran a program, by the number the transcript gives
    /// them. The entry a prose answer leaves in the notebook is not one.
    pub fn program_cells(&self) -> impl DoubleEndedIterator<Item = (usize, &CellView)> {
        self.cells
            .iter()
            .enumerate()
            .filter(|(_, view)| view.ran())
            .map(|(index, view)| (index + 1, view))
    }

    /// The newest cell that ran a program: what F4 and Ctrl-O act on when
    /// no card is selected.
    pub fn last_program_cell(&self) -> Option<usize> {
        self.program_cells().next_back().map(|(cell, _)| cell)
    }

    /// The newest cell that called a helper: what F5 and the sidebar's
    /// helper rows show when no card is selected.
    pub fn last_with_helpers(&self) -> Option<usize> {
        // A running cell counts: its helpers are working now.
        self.cells
            .iter()
            .rposition(|view| !view.helpers.is_empty())
            .map(|index| index + 1)
    }

    /// The program cell before or after `from`; the newest one from none.
    pub fn next_program_cell(&self, from: Option<usize>, forward: bool) -> Option<usize> {
        let Some(from) = from else {
            return self.last_program_cell();
        };
        let found = if forward {
            self.program_cells().find(|(cell, _)| *cell > from)
        } else {
            self.program_cells().rev().find(|(cell, _)| *cell < from)
        };
        found.map(|(cell, _)| cell).or(Some(from))
    }
}

impl CellView {
    /// A program ran here: something was executed, recorded or returned.
    pub fn ran(&self) -> bool {
        self.executed_source.is_some()
            || self.execution.is_some()
            || self.error.is_some()
            || self.output.is_some()
            || self.returned.is_some()
    }
}

/// Compatibility entry point: the old caller supplies no live editor or
/// session instrumentation. Never infer sandbox authority from the transcript.
pub fn render(
    frame: &mut Frame,
    conversation: &Conversation,
    served_by: &ServedBy,
    handles: &HandleTable,
    notebook: &Notebook,
) {
    render_screen(
        frame,
        conversation,
        served_by,
        handles,
        notebook,
        &ScreenState::default(),
    );
}

pub fn render_screen(
    frame: &mut Frame,
    conversation: &Conversation,
    served_by: &ServedBy,
    handles: &HandleTable,
    notebook: &Notebook,
    state: &ScreenState,
) {
    let regions = screen_regions(frame.area(), state);
    // An explicit canvas also paints blank cells when the caller creates a
    // fresh Terminal over pre-existing stdout; default blank cells do not.
    frame.render_widget(
        Block::default().style(Style::default().bg(Color::Reset).fg(Color::White)),
        frame.area(),
    );
    let header = Line::from(vec![
        Span::styled(
            " STERNA / ",
            Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
        ),
        Span::raw(abbreviate(
            state.project.as_deref().unwrap_or("project unknown"),
            usize::from(regions.header.width.saturating_sub(32)),
        )),
        Span::styled(
            format!(
                "   {} {}",
                if let Some(tick) = state.completion_tick {
                    ["[> ]", "[>>]", "[><]", "[<>]", "[+ ]", "[ +]"][tick.min(5)]
                } else {
                    state.activity.indicator(
                        if matches!(state.activity, Activity::Starting | Activity::Streaming) {
                            3
                        } else {
                            state.animation_frame
                        },
                    )
                },
                if state.completion_tick.is_some() {
                    "cell completed"
                } else {
                    state.activity.label()
                }
            ),
            Style::default().fg(if state.activity == Activity::Failed {
                Color::Red
            } else {
                ACCENT
            }),
        ),
    ]);
    frame.render_widget(
        Paragraph::new(header).block(Block::default().borders(Borders::BOTTOM)),
        regions.header,
    );
    if let Some(tick) = state.completion_tick {
        let width = regions.header.width.min(12);
        if width > 0 && regions.header.height > 0 {
            let travel = regions.header.width.saturating_sub(width);
            let x = regions.header.x + (u32::from(travel) * tick.min(5) as u32 / 5) as u16;
            frame.render_widget(
                Paragraph::new("╶──━━━━━━──╴")
                    .style(Style::default().fg(ACCENT).add_modifier(Modifier::BOLD)),
                Rect::new(x, regions.header.bottom() - 1, width, 1),
            );
        }
    }
    render_conversation(
        frame,
        regions.transcript,
        conversation,
        handles,
        notebook,
        state,
    );
    if regions.details.width > 0 {
        telemetry::rail(frame, regions.details, served_by, notebook, state);
    }
    if state.telemetry_open {
        let area = Rect::new(
            regions.transcript.x,
            regions.transcript.y,
            regions.transcript.width
                + if regions.details.width > 0 {
                    regions.details.width + 2
                } else {
                    0
                },
            regions.transcript.height,
        );
        frame.render_widget(Clear, area);
        telemetry::expanded(frame, area, conversation, served_by, notebook, state);
    }
    ribbon::activity(frame, regions.activity, state);
    poster::notice(frame, regions.notice, state);
    let completion_skip = state
        .completion_selected
        .saturating_sub(usize::from(regions.completions.height).saturating_sub(1));
    let matches: Vec<Line> = completions_shown(state)
        .into_iter()
        .enumerate()
        .skip(completion_skip)
        .map(|(index, (name, description))| {
            Line::from(format!(
                "{} {name:<15}{description}",
                if index == state.completion_selected {
                    "›"
                } else {
                    " "
                }
            ))
            .style(if index == state.completion_selected {
                Style::default().fg(Color::Black).bg(ACCENT)
            } else {
                Style::default().fg(MUTED)
            })
        })
        .collect();
    frame.render_widget(Paragraph::new(matches), regions.completions);
    let input_lines = wrapped_input(state, regions.input.width);
    let visible = usize::from(regions.input.height.saturating_sub(2));
    let cursor = state
        .cursor
        .map(|offset| composer_cursor(&state.input, offset, regions.input.width.saturating_sub(2)));
    let skip = cursor
        .map(|(row, _)| row.saturating_sub(visible.saturating_sub(1)))
        .unwrap_or_else(|| input_lines.len().saturating_sub(visible));
    frame.render_widget(
        Block::default().style(Style::default().bg(state.theme.dock())),
        regions.input,
    );
    if regions.input.height > 0 {
        let width = usize::from(regions.input.width);
        let top = "─".repeat(width);
        for (y, rule) in [
            (regions.input.y, top),
            (regions.input.bottom() - 1, "─".repeat(width)),
        ] {
            frame.render_widget(
                Paragraph::new(rule).style(Style::default().fg(ACCENT).bg(state.theme.dock())),
                Rect::new(regions.input.x, y, regions.input.width, 1),
            );
        }
    }
    if regions.input.height > 2 {
        frame.render_widget(
            Paragraph::new(input_lines.into_iter().skip(skip).collect::<Vec<_>>())
                .style(Style::default().bg(state.theme.dock())),
            Rect::new(
                regions.input.x,
                regions.input.y + 1,
                regions.input.width,
                regions.input.height - 2,
            ),
        );
    }
    if let Some((row, column)) = cursor
        && regions.input.width > 2
        && visible > 0
    {
        frame.set_cursor_position((
            regions.input.x + 2 + column.min(usize::from(regions.input.width - 3)) as u16,
            regions.input.y + 1 + (row - skip).min(visible - 1) as u16,
        ));
    }
    let model = state
        .model
        .as_deref()
        .or(served_by.model.as_deref())
        .unwrap_or("unknown");
    let project = state.project.as_deref().unwrap_or("unknown");
    let sandbox = state.sandbox.as_deref().unwrap_or("unknown");
    let network = state.network.as_deref().unwrap_or("unknown");
    let connection = match state.connected {
        Some(true) => "gateway connected",
        Some(false) => "gateway offline",
        None if served_by.is_known() => "gateway routed",
        None => NOT_CONNECTED,
    };
    let width = usize::from(regions.status.width);
    // The sandbox level and the effort: how much runs without asking, and
    // how hard the model thinks. The level is here rather than in the
    // sidebar because a person on `full` must never be able to forget it,
    // and the sidebar can be hidden.
    let mode = format!(
        "{} · effort {}",
        state.level.level().name(),
        state.effort.sent_for(model).name()
    );
    let identity = format!(" {} · {}", abbreviate(model, 28), abbreviate(project, 24));
    // The third fact, and the width it needs. `3p/1c YOLO unconfined` is 21
    // columns against this field's 16, and a wider field takes the context
    // reading off the right edge at 60 -- the one thing the resize test
    // says must survive. So it is appended where there is room and read
    // from the startup line and `sterna doctor` everywhere else.
    let posture_head = if width >= 100 {
        format!(
            " sandbox {}{} · net:{}",
            abbreviate(sandbox, 16),
            state
                .confinement
                .as_deref()
                .map(|word| format!(" {word}"))
                .unwrap_or_default(),
            abbreviate(network, 8)
        )
    } else {
        format!(
            " sandbox {} · net:{}",
            abbreviate(sandbox, 16),
            abbreviate(network, 8)
        )
    };
    // **Only the released state is news.** Captured is the default and a
    // permanent "· mouse" would be furniture, the same objection the scroll
    // indicator answers. Released must be visible, or dead clicks read as a
    // broken TUI -- and it is withheld below 90 columns because the context
    // reading owns that row's right edge and a longer left half would collapse
    // it away (`tui_live::telemetry_and_motion_are_local_controls_with_real_
    // response_usage` pins that priority).
    let mouse_mark = if state.mouse_off && width >= 90 {
        " · mouse off"
    } else {
        ""
    };
    let posture = format!("{posture_head}{mouse_mark}");
    let spent = notebook
        .tokens
        .as_ref()
        .filter(|_| width >= 78)
        .map(|tokens| {
            let scopes = if tokens.helpers.calls == 0 {
                format!("spent {}", compact_tokens(tokens.used))
            } else {
                format!(
                    "spent {} · parent {} + helpers {}{}",
                    compact_tokens(tokens.used),
                    compact_tokens(tokens.parent_used),
                    compact_tokens(tokens.helpers.used),
                    if tokens.helpers.complete() {
                        ""
                    } else {
                        " partial"
                    }
                )
            };
            format!("{scopes} · {}", tokens.counted.as_str())
        });
    let context = notebook.context.map(|tokens| {
        context_summary(
            tokens,
            if width >= 160 { 12 } else { 7 },
            state.animation_frame,
            matches!(state.activity, Activity::Thinking | Activity::Streaming),
        )
    });
    let status = if state.status_line == StatusLine::Compact {
        vec![footer_row(
            identity,
            context.unwrap_or(mode.clone()),
            width,
            ACCENT,
        )]
    } else if width < 140 {
        vec![
            footer_row(
                identity,
                if width >= 100 {
                    mode.clone()
                } else {
                    String::new()
                },
                width,
                ACCENT,
            ),
            footer_row(
                posture,
                context.clone().unwrap_or_else(|| {
                    if width < 100 {
                        mode.clone()
                    } else {
                        String::new()
                    }
                }),
                width,
                ACCENT,
            ),
            footer_row(
                format!(" {connection}"),
                spent.unwrap_or_else(|| {
                    "PgUp/PgDn chat · /cells inspect · /mouse frees drag-select".into()
                }),
                width,
                MUTED,
            ),
        ]
    } else {
        vec![
            footer_row(identity, mode.clone(), width, ACCENT),
            footer_row(
                format!("{posture} · {connection}"),
                match (context, spent) {
                    (Some(context), Some(spent)) => format!("{context} · {spent}"),
                    (Some(context), None) => context,
                    (None, Some(spent)) => spent,
                    (None, None) => {
                        "PgUp/PgDn chat · /cells inspect · /mouse frees drag-select".into()
                    }
                },
                width,
                ACCENT,
            ),
        ]
    };
    frame.render_widget(
        Paragraph::new(status).style(Style::default().fg(MUTED)),
        regions.status,
    );
    // Apply the palette once so every accent follows the same local preference.
    for cell in &mut frame.buffer_mut().content {
        if cell.fg == ACCENT {
            cell.set_fg(state.theme.accent());
        }
        if cell.bg == ACCENT {
            cell.set_bg(state.theme.accent());
        }
        if cell.bg == ACCENT {
            cell.set_bg(state.theme.accent());
        }
    }
    // Last, over everything already drawn: the selection is a rectangle of
    // cells, so what is copied is exactly what is on the screen.
    if let Some(span) = state.selection.filter(|span| !span.is_empty()) {
        let area = frame.area();
        selection::draw(frame.buffer_mut(), area, span, 0);
    }
}

/// Startup is caller-driven and immediately replaced by any real transcript.
/// One small assembling wireframe; no timer, sleep or terminal ownership here.
fn abbreviate(text: &str, width: usize) -> String {
    if Line::from(text).width() <= width {
        return text.to_string();
    }
    let mut out = String::new();
    for glyph in Span::raw(text).styled_graphemes(Style::default()) {
        if Line::from(out.as_str()).width() + Span::raw(glyph.symbol).width() + 1 > width {
            break;
        }
        out.push_str(glyph.symbol);
    }
    if width > 0 {
        out.push('…');
    }
    out
}

/// Wrap graphemes before viewport slicing: newest rows cannot be lost to a
/// logical-line scroll offset, and wide/combining characters keep cell bounds.
fn wrap_lines(lines: Vec<Line<'static>>, width: u16) -> Vec<Line<'static>> {
    if width == 0 {
        return Vec::new();
    }
    let mut out = Vec::new();
    for line in lines {
        let mut row = Vec::new();
        let mut used = 0;
        for span in line.spans {
            for glyph in span.styled_graphemes(line.style) {
                let size = Span::raw(glyph.symbol).width();
                if size > usize::from(width) {
                    continue;
                }
                if used + size > usize::from(width) {
                    out.push(Line::from(std::mem::take(&mut row)).style(line.style));
                    used = 0;
                }
                row.push(Span::styled(glyph.symbol.to_string(), glyph.style));
                used += size;
            }
        }
        out.push(Line::from(row).style(line.style));
    }
    out
}

/// How many rows the notebook column needs before any wrapping.
///
/// A caller drawing into an off-screen buffer sizes it by this: a fixed
/// height clips the newest cell away exactly when a task has run long enough
/// to be worth reading, and the pipe that gets the clipped frame has no
/// scrollback to recover it from.
pub fn notebook_height(
    conversation: &Conversation,
    handles: &HandleTable,
    notebook: &Notebook,
) -> usize {
    notebook_lines(
        conversation,
        handles,
        notebook,
        &[],
        false,
        false,
        98,
        0,
        Theme::default(),
        &mut Vec::new(),
    )
    .len()
}

/// One line with nothing to show. Never collapses to no line at all -- the
/// same rule the sidebar keeps for an unmetered request.
const NO_OUTPUTS: &str = "(no outputs)";

/// How many cells the conversation holds, counted the one way the screen
/// numbers them: the task is the first message and is drawn as a header, a
/// cell is an assistant message after it -- **except the terminal
/// response**, the assistant message that follows a cell whose view
/// returned (`runtime-contract.md` §9.2). That message is the model's reply,
/// not a program, and numbering it would put the next task's first cell one
/// off from its view. The session reads this to place each new view.
pub fn cell_ordinal(conversation: &Conversation, notebook: &Notebook) -> usize {
    let mut cells = 0usize;
    let mut after_return = false;
    for message in conversation.messages.iter().skip(1) {
        match message.role {
            Role::Assistant if after_return => after_return = false,
            Role::Assistant => {
                cells += 1;
                after_return = notebook
                    .cell(cells)
                    .is_some_and(|view| view.returned.is_some());
            }
            Role::User if is_tool_feedback(message) => {}
            Role::User => after_return = false,
        }
    }
    cells
}

pub fn conversation_rows(
    conversation: &Conversation,
    handles: &HandleTable,
    notebook: &Notebook,
    state: &ScreenState,
    width: u16,
) -> usize {
    conversation_lines(
        conversation,
        handles,
        notebook,
        state,
        width,
        &mut Vec::new(),
    )
    .len()
}

fn conversation_lines(
    conversation: &Conversation,
    handles: &HandleTable,
    notebook: &Notebook,
    state: &ScreenState,
    width: u16,
    headers: &mut Vec<(usize, usize)>,
) -> Vec<Line<'static>> {
    let mut content = notebook_lines(
        conversation,
        handles,
        notebook,
        &state.history,
        state.compact,
        state.pretty,
        usize::from(width.saturating_sub(2)),
        if state.reduced_motion {
            0
        } else {
            state.animation_frame
        },
        state.theme,
        headers,
    );
    if let Some(raw_partial) = state.streaming_text.as_deref() {
        let visible = streaming_message_text(raw_partial);
        let partial = visible.as_str();
        turn_header(
            &mut content,
            format!(
                "STERNA / RECEIVING  {} · {} bytes",
                Activity::Streaming.indicator(state.animation_frame),
                raw_partial.len()
            ),
            ACCENT,
        );
        if state.compact && (partial.contains("```") || partial.contains("<php-sterna>")) {
            let prose = partial
                .split("```")
                .next()
                .unwrap_or_default()
                .split("<php-sterna>")
                .next()
                .unwrap_or_default();
            if !prose.trim().is_empty() {
                push_text_region(&mut content, prose.trim());
            }
            push_text_region(
                &mut content,
                "Preparing actions · Ctrl-O shows incoming code",
            );
        } else if partial.contains("```") {
            push_text_region(&mut content, partial);
        } else {
            content.extend(markdown::render(
                partial,
                usize::from(width.saturating_sub(2)),
            ));
        }
    }
    if let Some(input) = &state.streaming_tool_input {
        turn_header(
            &mut content,
            format!(
                "CELL {} · preparing · nothing has run",
                cell_ordinal(conversation, notebook) + 1
            ),
            Color::LightCyan,
        );
        content.push(Line::styled(
            format!(
                "Receiving program · {} bytes · waiting for execution handoff",
                input.len()
            ),
            Style::default().fg(MUTED),
        ));
        if !state.compact
            && let Ok(value) = serde_json::from_str::<serde_json::Value>(input)
            && let Some(code) = value.get("code").and_then(|value| value.as_str())
        {
            content.extend(markdown::code(code));
        }
    }
    push_recap(&mut content, state.recap.as_ref());
    bands::decorate(&mut content, state, width.saturating_sub(2));
    wrap_lines(content, width.saturating_sub(2))
}

fn render_conversation(
    frame: &mut Frame,
    area: Rect,
    conversation: &Conversation,
    handles: &HandleTable,
    notebook: &Notebook,
    state: &ScreenState,
) {
    let mut headers = Vec::new();
    let lines = conversation_lines(
        conversation,
        handles,
        notebook,
        state,
        area.width,
        &mut headers,
    );
    let start = lines
        .len()
        .saturating_sub(usize::from(area.height))
        .saturating_sub(state.scrollback);
    let total_rows = lines.len();
    // The project's own directory, for a relative path a model wrote. Read
    // once per draw, not once per token.
    let root = std::env::current_dir().unwrap_or_default();
    let lines: Vec<Line> = lines
        .into_iter()
        .skip(start)
        .take(usize::from(area.height))
        .map(|mut line| {
            if line.style.bg.is_some() {
                line.spans.insert(0, Span::raw(" "));
                let padding = usize::from(area.width).saturating_sub(line.width());
                line.spans.push(Span::raw(" ".repeat(padding)));
            }
            // **After the padding, because the padding moves the columns.**
            // `wrap_lines` left one span per grapheme, so a column range is a
            // span range.
            paths::mark(&mut line, &root);
            line
        })
        .collect();
    frame.render_widget(Paragraph::new(lines), area);
    render_scrollbar(frame, area, total_rows, start, state.scrolling);
}

/// Preserve the viewed rows as new content arrives; zero remains live-follow.
pub fn anchor_scrollback(scroll: usize, previous: usize, current: usize, height: usize) -> usize {
    if scroll == 0 {
        return 0;
    }
    let adjusted = if current >= previous {
        scroll.saturating_add(current - previous)
    } else {
        scroll.saturating_sub(previous - current)
    };
    adjusted.min(current.saturating_sub(height))
}

/// Pushes a turn's header and answers **which line it is**, so a caller that
/// knows the header belongs to a cell can record that line as clickable
/// without searching for it afterwards (`hit.rs`: the map is built by the
/// draw). Callers that have nothing to record ignore the index.
fn turn_header(lines: &mut Vec<Line<'static>>, label: String, color: Color) -> usize {
    if !lines.is_empty() {
        lines.push(Line::styled("╰─", Style::default().fg(MUTED)));
        lines.push(Line::from(""));
    }
    lines.push(Line::styled(
        format!("╭─ {label}"),
        Style::default().fg(color).add_modifier(Modifier::BOLD),
    ));
    lines.len() - 1
}

/// The recap's own header. It names the author and denies the mistake,
/// because a recap read as the assistant's answer is worse than no recap:
/// the answer is the model's, this is a cheap helper's summary of it.
const RECAP_LABEL: &str = "RECAP · a helper's summary, not the assistant";

/// The session's closing output when `[helpers] completion = "recap"` asked
/// for one: what the session did, then the `Next:` line the preamble asks
/// for, under the transcript they summarise.
///
/// **Nothing at all unless a recap was asked for and came back.** A missing
/// or failed record renders no header, no reason and no blank frame -- the
/// screen is the one the silent default already draws, because a recap must
/// never replace or delay the answer above it.
///
/// It is drawn in [`MUTED`] with the helper lane's indent rather than in the
/// model's own prose style, and nothing here adds a tick, a colour or a word
/// that would claim more than the sentences themselves do.
fn push_recap(lines: &mut Vec<Line<'static>>, recap: Option<&HelperRecord>) {
    let Some(record) = recap.filter(|record| record.outcome.ok) else {
        return;
    };
    let text = record.outcome.text.trim();
    if text.is_empty() {
        return;
    }
    turn_header(lines, RECAP_LABEL.to_string(), MUTED);
    for line in text.lines() {
        lines.push(Line::styled(
            format!("  {}", line.trim_end()),
            Style::default().fg(MUTED),
        ));
    }
    lines.push(Line::styled("╰─", Style::default().fg(MUTED)));
}

/// A cell's regions, in the order `runtime-contract.md` §1 and §5 put them:
/// an input region carrying the **program** the message contained (its prose
/// when it contained none), an output region carrying the handle table as
/// that cell ended, then an error region for a throw and a return region for
/// a top-level `return`. The first user message is the task, drawn once as a
/// header rather than a cell; a later user message is a person typing, drawn
/// as `you: <text>` between cells -- unless the cell before it says the
/// runtime answered it, in which case that message *is* the answer whose
/// sections are already drawn above.
///
/// **A cell with no view of its own falls back to the pre-runtime rendering**
/// (the latest cell shows `handles`, an earlier one says `(no outputs)`), so
/// a caller that holds the live table itself -- every test in `tests/tui.rs`
/// -- still gets it drawn through the one renderer.
#[allow(clippy::too_many_arguments)]
fn notebook_lines(
    conversation: &Conversation,
    handles: &HandleTable,
    notebook: &Notebook,
    notes: &[HistoryNote],
    compact: bool,
    pretty: bool,
    width: usize,
    tick: usize,
    // The palette the poster fields draw with. The transcript used the
    // `ACCENT` constant before a cell header became a filled field; a field
    // has a ground as well as an ink, and only the theme knows both.
    theme: Theme,
    // Line index -> the cell that line's header belongs to, filled as the
    // headers are pushed. A caller with no use for it passes a scratch vector.
    headers: &mut Vec<(usize, usize)>,
) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    let mut messages = conversation.messages.iter();
    let mut next_note = 0usize;

    history::push_notes(&mut lines, notes, &mut next_note, 0);
    if let Some(task) = messages.next() {
        turn_header(&mut lines, "USER".into(), ACCENT);
        push_text_region(&mut lines, &task.as_written());
    }
    let total_cells = cell_ordinal(conversation, notebook);

    let mut cell = 0usize;
    let mut answered = false;
    let mut after_return = false;
    for (index, message) in messages.enumerate() {
        // At a turn boundary only: a note drawn between a cell and its
        // feedback would split the cell's block.
        if message.role == Role::User && !is_tool_feedback(message) {
            history::push_notes(&mut lines, notes, &mut next_note, index + 1);
        }
        match message.role {
            // The assistant message after a cell that returned is the
            // terminal response -- the model's reply, drawn as its turn
            // rather than as a cell (`runtime-contract.md` §9.2).
            Role::Assistant if after_return => {
                after_return = false;
                // The identical terminal response is already visible in its return region.
            }
            Role::Assistant => {
                cell += 1;
                // Set when this message drew a cell field, so the field's
                // closing rule can be pushed after the regions rather than
                // between them.
                let mut footer: Option<(usize, usize)> = None;
                let view = notebook.cell(cell);
                answered = view.is_some_and(|view| view.answered);
                after_return = view.is_some_and(|view| view.returned.is_some());

                let original = message_text(message);
                if matches!(message_program(message), Extracted::Prose)
                    && view.is_none_or(|v| {
                        v.table.is_none()
                            && v.execution.is_none()
                            && v.error.is_none()
                            && v.returned.is_none()
                    })
                    && !original.contains("<php-sterna>")
                    && !original.contains("```sterna")
                {
                    turn_header(&mut lines, "STERNA".into(), Color::White);
                    lines.extend(markdown::render(&original, width));
                    continue;
                }
                let (before, after) = natural_message(message);
                if !before.trim().is_empty() {
                    turn_header(&mut lines, "STERNA".into(), ACCENT);
                    lines.extend(markdown::render(before.trim(), width));
                }
                if compact {
                    let original = message_text(message);
                    match message_program(message) {
                        Extracted::Program(source) | Extracted::Edit(source) => {
                            let repairing = matches!(message_program(message), Extracted::Edit(_));
                            let source = view
                                .and_then(|v| v.executed_source.as_ref())
                                .unwrap_or(&source);
                            let possible = possible_tool_calls(source);
                            let only_answer = !repairing
                                && possible.is_empty()
                                && view.is_some_and(|v| {
                                    v.returned.is_some()
                                        && v.execution
                                            .as_deref()
                                            .is_some_and(|calls| calls.starts_with("No tool"))
                                });
                            if !only_answer {
                                let failed = view.is_some_and(|v| v.error.is_some());
                                let evaluated = view.is_some_and(|v| v.execution.is_some());
                                let state = if failed {
                                    poster::State::Threw
                                } else if repairing && evaluated {
                                    poster::State::Repaired
                                } else if evaluated {
                                    poster::State::Executed
                                } else {
                                    poster::State::Preparing
                                };
                                // The header is a filled field rather than a
                                // corner and a label: the sidebar beside it
                                // already speaks in numbered panels, and the
                                // user's reading of the old column was that
                                // "the colouring and framing are not human
                                // readable anyways" (2026-09-19).
                                if !lines.is_empty() {
                                    lines.push(Line::from(""));
                                }
                                headers.push((lines.len(), cell));
                                // **Only the cell doing work moves.** Every
                                // cell used to be handed the same
                                // `animation_frame`, so a screen of twenty
                                // finished records cycled the reveal ramp in
                                // lockstep -- the user, watching a live run:
                                // *"vor allem dass alle blinken"* (2026-09-19).
                                // A finished cell is a record, and a record
                                // does not shimmer; a tick of zero is the
                                // still render `/motion off` already draws.
                                let cell_tick =
                                    if state == poster::State::Preparing && cell == total_cells {
                                        tick
                                    } else {
                                        0
                                    };
                                lines.push(poster::field_header(
                                    cell, state, width, theme, cell_tick,
                                ));
                                let fold = helper_fold(view);
                                if !fold.is_empty() {
                                    lines.push(Line::styled(
                                        format!(" {}", fold.trim()),
                                        Style::default().fg(MUTED),
                                    ));
                                }
                                // The one line the model wrote about what this
                                // cell is for, directly under its header and
                                // above the record of what ran — the order is
                                // the claim: intention first, evidence below
                                // (`legibility.md` §2, §8 rule 2).
                                if let Some(description) = view
                                    .and_then(|v| v.description.as_deref())
                                    .map(str::trim)
                                    .filter(|description| !description.is_empty())
                                {
                                    lines.push(Line::from(""));
                                    lines.extend(poster::intent_block(description, width, theme));
                                    lines.push(Line::from(""));
                                }
                                push_helper_lane(&mut lines, view, tick, width);
                                let none_ran = view
                                    .and_then(|v| v.execution.as_deref())
                                    .is_some_and(|calls| calls.starts_with("No tool"));
                                if let Some(target) = view.and_then(|v| v.repaired_from) {
                                    lines.push(Line::styled(
                                        format!("Amends syntax-failed cell {target}"),
                                        Style::default().fg(MUTED),
                                    ));
                                }
                                if !possible.is_empty()
                                    && (possible.len() > 1 || !evaluated || none_ran)
                                {
                                    lines.push(Line::styled(
                                        format!(
                                            "◇ planned: {} · conditional calls may not run",
                                            possible.join(" → ")
                                        ),
                                        Style::default().fg(Color::LightCyan),
                                    ));
                                }
                                if let Some(actual) = view.and_then(|v| v.execution.as_deref()) {
                                    if actual.starts_with("No tool") {
                                        if failed || !possible.is_empty() {
                                            push_text_region(&mut lines, "No tools ran.");
                                        }
                                    } else if let Some(summary) = poster::call_summary(actual) {
                                        // One filled bar for the whole turn.
                                        // The per-call tree is detail for the
                                        // inspector, not for the flow; the
                                        // kinds and their counts are what a
                                        // reader acts on and they stay here.
                                        lines.push(poster::call_bar(
                                            &summary, None, width, theme, cell_tick,
                                        ));
                                    } else {
                                        push_text_region(&mut lines, actual);
                                    }
                                }
                                // The model's own sentence about why it
                                // stopped here. It exists on 40 of the 123
                                // views of the corpus behind `legibility.md`
                                // and the default screen drew none of them:
                                // the expanded path had it, the compact path
                                // returned before reaching it.
                                if let Some(reason) = view.and_then(|v| v.yield_reason.as_deref()) {
                                    lines.push(Line::styled(
                                        format!("yielded: {reason}"),
                                        Style::default().fg(MUTED),
                                    ));
                                }
                                // The footer closes the field, and it closes
                                // it *after* the regions below rather than
                                // here: a rule drawn between the bar and the
                                // bindings would cut the cell in half.
                                footer = Some((cell, cell_tick));
                            }
                        }
                        Extracted::Invalid(error) => {
                            turn_header(
                                &mut lines,
                                "Response format rejected".into(),
                                Color::Yellow,
                            );
                            push_text_region(&mut lines, &format!("{error} Nothing ran."));
                        }
                        Extracted::TwoBlocks => {
                            turn_header(
                                &mut lines,
                                "Response format rejected".into(),
                                Color::Yellow,
                            );
                            push_text_region(
                                &mut lines,
                                "Ambiguous repair blocks · nothing ran. Ctrl-O shows the response.",
                            );
                        }
                        Extracted::Prose
                            if original.contains("<php-sterna>")
                                || original.contains("```sterna") =>
                        {
                            turn_header(
                                &mut lines,
                                "Response format rejected".into(),
                                Color::Yellow,
                            );
                            push_text_region(
                                &mut lines,
                                "Expected complete Sterna code · nothing ran. Ctrl-O shows the response.",
                            );
                        }
                        Extracted::Prose => {
                            turn_header(&mut lines, "STERNA".into(), ACCENT);
                            push_text_region(&mut lines, &original);
                        }
                    }
                    if let Some(plan) = view
                        .and_then(|v| v.table.as_deref())
                        .filter(|s| s.starts_with("Planning mode"))
                    {
                        push_text_region(&mut lines, plan);
                    }
                    if let Some(error) = view.and_then(|v| v.error.as_ref()) {
                        push_text_region(
                            &mut lines,
                            &format!(
                                "{}: {}",
                                error.class,
                                error.message.lines().next().unwrap_or_default()
                            ),
                        );
                    }
                    // **The bindings, as rows, never as braces.** The readable
                    // form already existed and was given to the model and not
                    // to the person: `render_table` names every binding with
                    // its type and length, while this column drew the cell's
                    // raw stdout -- four kilobytes of one-line JSON was what
                    // the user was actually looking at (2026-09-19). The
                    // token costs in the model's copy are dropped: a reader
                    // is not spending them.
                    poster::push_bindings(
                        &mut lines,
                        view.and_then(|v| v.table.as_deref()),
                        view.and_then(|v| v.stdout.as_deref()),
                        width,
                        theme,
                    );
                    if let Some(output) = view.and_then(|v| v.output.as_deref()) {
                        turn_header(&mut lines, "OUTPUT".into(), MUTED);
                        // A value is drawn by its shape. `to_string_pretty`
                        // escapes the newlines inside a string, so a diff
                        // returned as a field arrived as one enormous line of
                        // `\n` -- the user, on their own screen: *"und das
                        // als Output interessiert einen Menschen auch nicht"*
                        // (2026-09-19). A value this module has no opinion
                        // about keeps the rendering it always had.
                        if !value::push_value(&mut lines, output, width, theme) {
                            lines.extend(markdown::render(&pretty_json(output), width));
                        }
                    }
                    if let Some(changes) = view.and_then(|v| v.changes.as_deref()) {
                        push_changes(&mut lines, changes, true);
                    }
                    if let Some(returned) = view.and_then(|v| v.returned.as_deref()) {
                        turn_header(&mut lines, "STERNA".into(), ACCENT);
                        if !value::push_value(&mut lines, returned, width, theme) {
                            lines.extend(markdown::render(&pretty_json(returned), width));
                        }
                    }
                    if let Some((cell, footer_tick)) = footer {
                        poster::push_footer(&mut lines, cell, width, theme, footer_tick);
                    }
                    if !after.trim().is_empty() {
                        lines.extend(markdown::render(after.trim(), width));
                    }
                    continue;
                }
                let role = if matches!(
                    message_program(message),
                    Extracted::Program(_) | Extracted::Edit(_)
                ) {
                    "STERNA / CODE"
                } else if matches!(
                    message_program(message),
                    Extracted::TwoBlocks | Extracted::Invalid(_)
                ) {
                    "STERNA / NOT EXECUTED: invalid protocol"
                } else if message_text(message).contains("<php-sterna>") {
                    "STERNA / NOT EXECUTED"
                } else {
                    "STERNA"
                };
                let execution = if role == "STERNA / CODE" {
                    if view.is_some_and(|v| v.error.is_some()) {
                        " · failed ×"
                    } else if view.is_some_and(|v| v.execution.is_some()) {
                        " · executed ◆"
                    } else {
                        " · proposed ◇"
                    }
                } else {
                    ""
                };
                headers.push((
                    turn_header(
                        &mut lines,
                        format!("{role}  [{cell}] in{execution}{}", helper_fold(view)),
                        ACCENT,
                    ),
                    cell,
                ));
                push_helper_lane(&mut lines, view, tick, width);
                if let Some(target) = view.and_then(|v| v.repaired_from) {
                    lines.push(Line::styled(
                        format!("Amends syntax-failed cell {target}"),
                        Style::default().fg(MUTED),
                    ));
                }
                let source = view
                    .and_then(|v| v.executed_source.clone())
                    .unwrap_or_else(|| input_region(message));
                let display = if pretty && view.is_none_or(|v| v.error.is_none()) {
                    match message_program(message) {
                        Extracted::Program(_) | Extracted::Edit(_) => pretty_code(&source),
                        _ => source,
                    }
                } else {
                    source
                };
                if role == "STERNA / CODE" {
                    let code = markdown::code(&display);
                    let limit = if compact { 10 } else { usize::MAX };
                    let remaining = code.len().saturating_sub(limit);
                    lines.extend(code.into_iter().take(limit));
                    if remaining > 0 {
                        lines.push(Line::styled(
                            format!("… {remaining} more lines · Ctrl-O expands"),
                            Style::default().fg(MUTED),
                        ));
                    }
                } else {
                    push_folded_region(&mut lines, &display, 10, compact);
                }
                if role == "STERNA / CODE" {
                    let candidates = possible_tool_calls(
                        view.and_then(|v| v.executed_source.as_deref())
                            .unwrap_or(&input_region(message)),
                    );
                    if !candidates.is_empty() {
                        lines.push(Line::styled(
                            format!("◇ possible: {} · branches may skip", candidates.join(" → ")),
                            Style::default().fg(Color::LightCyan),
                        ));
                    }
                }

                if let Some(execution) = view.and_then(|view| view.execution.as_deref()) {
                    let count = execution
                        .lines()
                        .filter(|line| line.starts_with("├─") || line.starts_with("└─"))
                        .count();
                    turn_header(
                        &mut lines,
                        format!("TOOL / ACTUAL  [{cell}] ◆ {count} calls · one cell"),
                        ACCENT,
                    );
                    push_text_region(&mut lines, execution);
                }
                headers.push((
                    turn_header(&mut lines, format!("TOOL / PREVIEW  [{cell}] out"), MUTED),
                    cell,
                ));
                match view.and_then(|view| view.table.as_deref()) {
                    Some(table) => push_output_region(&mut lines, table.to_string(), compact),
                    None if cell == total_cells => push_output_region(
                        &mut lines,
                        render_table(handles, PREVIEW_TOKEN_CAP, TABLE_TOKEN_CAP),
                        compact,
                    ),
                    None => lines.push(Line::from(NO_OUTPUTS)),
                }
                if let Some(stdout) = view.and_then(|view| view.stdout.as_deref()) {
                    turn_header(&mut lines, "OUTPUT".into(), MUTED);
                    push_folded_region(&mut lines, stdout, 6, compact);
                }
                if let Some(output) = view.and_then(|view| view.output.as_deref()) {
                    headers.push((
                        turn_header(&mut lines, format!("OUTPUT  [{cell}]"), MUTED),
                        cell,
                    ));
                    lines.extend(markdown::render(&pretty_json(output), width));
                }
                if let Some(changes) = view.and_then(|v| v.changes.as_deref()) {
                    push_changes(&mut lines, changes, compact);
                }
                if let Some(reason) = view.and_then(|view| view.yield_reason.as_deref()) {
                    lines.push(Line::from(format!("yielded: {reason}")));
                }

                if let Some(error) = view.and_then(|view| view.error.as_ref()) {
                    headers.push((
                        turn_header(&mut lines, format!("ERROR  [{cell}] error"), Color::Red),
                        cell,
                    ));
                    push_error_region(&mut lines, error);
                }
                if let Some(returned) = view.and_then(|view| view.returned.as_deref()) {
                    turn_header(
                        &mut lines,
                        format!("STERNA / RETURN  [{cell}] return"),
                        ACCENT,
                    );
                    let display = if compact {
                        pretty_json(returned)
                    } else {
                        returned.to_string()
                    };
                    lines.extend(markdown::render(&display, width));
                }
                if !after.trim().is_empty() {
                    turn_header(&mut lines, "STERNA".into(), ACCENT);
                    lines.extend(markdown::render(after.trim(), width));
                }
            }
            Role::User if is_tool_feedback(message) => {
                answered = false;
            }
            Role::User => {
                after_return = false;
                if answered {
                    answered = false;
                    continue;
                }
                turn_header(&mut lines, "USER".into(), ACCENT);
                push_text_region(&mut lines, &format!("you: {}", message.as_written()));
            }
        }
    }

    // Preflight belongs to the newest submitted request, after all completed
    // history. Keeping it at the tail also keeps it in the followed viewport
    // during a later task in the same session.
    if let Some(record) = notebook.preflight.as_ref() {
        turn_header(&mut lines, "PREFLIGHT · SCOUT".into(), MUTED);
        lines.push(helper_lane(record, tick, width));
    }
    history::push_notes(&mut lines, notes, &mut next_note, usize::MAX);

    if !lines.is_empty() {
        lines.push(Line::styled("╰─", Style::default().fg(MUTED)));
    }
    lines
}

/// Only the fields the gateway actually reported become a line. A field it
/// never sent is omitted rather than shown as `0` or a placeholder --
/// [`ServedBy`]'s own rule, applied per field rather than only at the top.
fn known_sidebar_lines(served_by: &ServedBy) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    if let Some(quota_context) = &served_by.quota_context {
        lines.push(Line::from(format!("entitlement: {quota_context}")));
    }
    if let Some(provider) = &served_by.provider {
        lines.push(Line::from(format!("provider: {provider}")));
    }
    if let Some(route) = &served_by.route {
        lines.push(Line::from(format!("route: {route}")));
    }
    if let Some(cached) = served_by.cached_input_tokens {
        lines.push(Line::from(format!("cached input: {cached} tok")));
    }
    if let Some(model) = &served_by.model {
        lines.push(Line::from(format!("model: {model}")));
    }
    match (served_by.input_tokens, served_by.output_tokens) {
        (Some(input), Some(output)) => {
            lines.push(Line::from(format!("tokens: {input} in / {output} out")));
        }
        (Some(input), None) => lines.push(Line::from(format!("tokens: {input} in"))),
        (None, Some(output)) => lines.push(Line::from(format!("tokens: {output} out"))),
        (None, None) => {}
    }
    lines
}
