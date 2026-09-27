//! Native conversation workbench. Runtime snapshots are inputs, never UI policy.
//!
//! This replaces the live renderer and controls, not the execution kernel.
//! Local navigation cannot mutate a conversation or grant runtime authority.
mod chrome;
mod document;
mod input;
mod models;
mod motion;
pub mod plumage;
mod settings;
mod sheets;
mod theme;
mod view;
pub mod voice;

use crate::tui::{Panel, ScreenState};
pub use document::{Document, Row, RowKind, Tone};
pub use input::Effect;
pub use models::Navigator;
use ratatui::layout::Rect;
pub use settings::Preferences;
pub use sheets::render_form;
use std::collections::BTreeSet;
pub use view::{layout, render};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CellTab {
    Code,
    Diff,
    Output,
    Helpers,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    Cell(usize),
    Insert(String),
    Path(String),
    Tab(usize, CellTab),
    Helper(usize, usize),
    Latest,
    Settings,
    Models,
    Work,
    Approvals,
    Access,
    Activity,
    Category(usize),
    Setting(usize, Option<String>),
    /// Go to a named scope. It used to be a bare toggle shared by both
    /// tabs, so clicking the tab you were already on moved you off it --
    /// the one gesture that should have done nothing was the one that
    /// changed which file a save would land in.
    Scope(bool),
    Undo,
    ModelRole(usize),
    Slot(Option<String>),
    Model(usize),
    ChooseModel,
    UnsetModel,
    Close,
    PanelRow(usize),
    Composer,
    Command(String),
    Rung(String),
    Sources,
    Scores,
    /// Step the reasoning effort one place along its own ladder, in place.
    /// The status strip's first control: the setting a person changes most
    /// often, and the one the session bar has no room for.
    Effort,
    /// Open settings already on the category that owns the thing just
    /// clicked, so a control on the strip is one click from the row that
    /// changes it rather than four.
    SettingsAt(usize),
    /// The one-screen sheet of every key.
    Help,
    /// Take back the last live change the dock made, while its notice is
    /// still up.
    UndoLive,
    /// Step what the screen shows of a cell while it is being written.
    Stream,
}
#[derive(Debug, Clone, Default)]
pub struct Geometry {
    pub transcript: Rect,
    pub composer: Rect,
    pub local: Option<Rect>,
    pub hits: Vec<(Rect, Action)>,
    pub rows: usize,
    pub start: usize,
    pub copied: String,
    pub screen: Option<ratatui::buffer::Buffer>,
}
impl Geometry {
    pub fn hit(&self, x: u16, y: u16) -> Option<Action> {
        self.hits
            .iter()
            .rev()
            .find(|(r, _)| contains(*r, x, y))
            .map(|(_, a)| a.clone())
    }
}
pub(crate) fn contains(r: Rect, x: u16, y: u16) -> bool {
    x >= r.x && y >= r.y && x < r.right() && y < r.bottom()
}
#[derive(Default)]
pub struct Workbench {
    pub expanded: BTreeSet<usize>,
    pub collapsed: BTreeSet<usize>,
    pub tabs: std::collections::BTreeMap<usize, CellTab>,
    pub helper: Option<(usize, usize)>,
    pub selected_cell: Option<usize>,
    pub preferences: Option<Preferences>,
    pub models: Option<Navigator>,
    pub model_role: Option<usize>,
    pub model_slot: Option<String>,
    pub model_preference: Option<(Preferences, String)>,
    pub activity: bool,
    pub access: bool,
    pub work: bool,
    pub approvals: bool,
    pub local_scroll: usize,
    pub press: Option<(u16, u16)>,
    pub dragged: bool,
    pub notice: String,
    pub confirm: Option<String>,
    pub geometry: Geometry,
    pub anchor: Option<((usize, usize), String)>,
    pub last_scrollback: usize,
    pub jump_cell: Option<usize>,
    pub help: bool,
    /// The last live change a dock chip made -- what to call it, and the
    /// command that takes it back -- offered beside its notice.
    pub undo: Option<(String, String)>,
    /// When [`Workbench::notice`] was last set, so it can fade.
    pub notice_at: Option<std::time::Instant>,
    pub shown_notice: String,
}
/// How long a notice rides the dock's edge before it fades. It is still in
/// the transcript and on the Activity surface after that.
pub const NOTICE_LINGER: std::time::Duration = std::time::Duration::from_secs(4);
impl Workbench {
    pub fn open_settings(&mut self, state: &ScreenState) {
        self.model_preference = None;
        match Preferences::open(state) {
            Ok(p) => {
                self.close();
                self.preferences = Some(p);
            }
            Err(e) => self.notice = e,
        }
    }
    pub fn close(&mut self) {
        self.preferences = None;
        self.models = None;
        self.activity = false;
        self.access = false;
        self.work = false;
        self.approvals = false;
        self.help = false;
        self.confirm = None;
        self.local_scroll = 0;
    }
    pub fn is_local(&self) -> bool {
        self.preferences.is_some()
            || self.models.is_some()
            || self.activity
            || self.access
            || self.work
            || self.approvals
            || self.help
            || self.confirm.is_some()
    }
    /// Called once per frame: starts the clock on a notice that just
    /// appeared, and clears one that has had its time.
    pub fn age_notice(&mut self) {
        if self.notice != self.shown_notice {
            self.shown_notice = self.notice.clone();
            self.notice_at = if self.notice.is_empty() {
                None
            } else {
                Some(std::time::Instant::now())
            };
        } else if self.notice_expired() {
            self.notice.clear();
            self.shown_notice.clear();
            self.notice_at = None;
            self.undo = None;
        }
    }
    pub fn notice_visible(&self) -> bool {
        !self.notice.is_empty() && !self.notice_expired()
    }
    /// A notice whose time is up, which the next frame will clear -- the
    /// loop draws one when this says so, since nothing else would.
    pub fn notice_expired(&self) -> bool {
        !self.notice.is_empty()
            && self
                .notice_at
                .is_some_and(|at| at.elapsed() >= NOTICE_LINGER)
    }
    pub fn absorb_panel(&mut self, state: &mut ScreenState) {
        if state.panel.as_ref().is_some_and(|p| p.assignment.is_some()) {
            let panel = state.panel.take().expect("checked panel");
            self.close();
            self.models = Navigator::from_panel(&panel);
            if let (Some(role), Some(model)) = (self.model_role.take(), self.models.as_mut()) {
                model.role = role;
                model.select_current();
            }
            if let Some(model) = self.models.as_mut() {
                if let Some(root) = &state.settings_root
                    && let Ok(loaded) = crate::settings::Store::new(root)
                        .and_then(|store| store.load(state.settings_profile.as_deref()))
                {
                    model.assignment = loaded.config.agents;
                }
                model.slot = self.model_slot.take();
                model.target_key = self.model_preference.as_ref().map(|(_, key)| key.clone());
                model.select_current();
            }
        }
    }
    pub fn browse_preference(&mut self, key: &str) {
        self.model_role = Some(if key.starts_with("agents.") {
            2
        } else if key == "helpers.model" {
            1
        } else {
            0
        });
        self.model_slot = key
            .strip_prefix("agents.slots.")
            .and_then(|k| k.strip_suffix(".model"))
            .map(str::to_string);
        self.model_preference = self.preferences.take().map(|p| (p, key.into()));
        self.close();
    }
    /// Preserve a reader's content anchor when rows reflow or arrive below it.
    pub fn anchor_document(&mut self, doc: &Document, state: &mut ScreenState, height: usize) {
        let jump = self.jump_cell.take().and_then(|cell| {
            doc.rows
                .iter()
                .position(|r| r.action == Some(Action::Cell(cell)))
        });
        let anchor = if state.scrollback > 0 && state.scrollback == self.last_scrollback {
            self.anchor.as_ref().and_then(|(key, text)| {
                doc.rows
                    .iter()
                    .position(|r| r.key.0 == key.0 && r.text == *text)
                    .or_else(|| doc.rows.iter().position(|r| r.key == *key))
            })
        } else {
            None
        };
        if let Some(first) = jump.or(anchor) {
            state.scrollback = doc.rows.len().saturating_sub(height).saturating_sub(first);
        }
        state.scrollback = state.scrollback.min(doc.rows.len().saturating_sub(height));
    }
    pub fn panel_command(panel: &Panel) -> Option<String> {
        panel.rows.get(panel.selected)?.command.clone()
    }
}
