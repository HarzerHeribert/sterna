//! Native conversation workbench. Runtime snapshots are inputs, never UI policy.
//!
//! This replaces the live renderer and controls, not the execution kernel.
//! Local navigation cannot mutate a conversation or grant runtime authority.
//!
//! **One stack of sheets.** Every surface a person opens -- a picker, the
//! settings, a panel the session sent, a confirmation -- is a [`Layer`]: a
//! [`Sheet`] and the data its rows are built from. Esc pops one layer and a
//! row that opens something pushes one; nothing here keeps a flag per
//! surface.
mod chrome;
mod document;
mod input;
mod models;
mod motion;
pub mod plumage;
mod settings;
pub mod sheet;
mod sheets;
mod theme;
mod view;
pub mod voice;

use crate::tui::{Panel, ScreenState};
pub(crate) use chrome::frame;
pub use document::{Document, Row, RowKind, Tone};
pub use input::Effect;
pub use models::Navigator;
use ratatui::layout::Rect;
pub use settings::Preferences;
pub use sheet::{Item, Kind as ItemKind, Outcome, Sheet};
pub use sheets::{FormHit, keymap, render_form};
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
    Setting(usize, Option<String>),
    /// Go to a named scope. It used to be a bare toggle shared by both
    /// tabs, so clicking the tab you were already on moved you off it --
    /// the one gesture that should have done nothing was the one that
    /// changed which file a save would land in.
    Scope(bool),
    Undo,
    Slot(Option<String>),
    Model(usize),
    ChooseModel,
    UnsetModel,
    /// Back one layer: the top sheet closes and the one under it shows.
    Close,
    Composer,
    Command(String),
    Rung(String),
    /// The confirmed half of a dangerous rung change.
    ConfirmRung(String),
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
    /// A theme, applied at once; the sheet stays open.
    Theme(crate::tui::Theme),
    /// The live instruments.
    Telemetry,
    /// Open a link in the person's browser.
    OpenLink(String),
    /// Put this text on the clipboard.
    Copy(String),
    /// The form that takes the address a browser ended on.
    PasteCallback,
    /// Turn a standing handler off.
    HandlerOff(String),
    /// A click on one of the top sheet's own targets.
    Sheet(sheet::Hit),
    /// An answer on a decision prompt.
    Answer(Answer),
    /// Forget a call answered for the whole session: the next identical
    /// call asks again.
    Forget(String),
    /// The text of the field at this row changed.
    FieldEdited(usize),
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
    /// The target under a point, for hover: the rectangle, so a move within
    /// one target owes no frame.
    pub fn hit_rect(&self, x: u16, y: u16) -> Option<Rect> {
        self.hits
            .iter()
            .rev()
            .find(|(r, _)| contains(*r, x, y))
            .map(|(r, _)| *r)
    }
}
pub(crate) fn contains(r: Rect, x: u16, y: u16) -> bool {
    x >= r.x && y >= r.y && x < r.right() && y < r.bottom()
}
/// The answers a decision prompt offers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Answer {
    AllowOnce,
    AllowForSession,
    /// Refuse and say what to do instead.
    AnotherWay,
    Deny,
    /// Send the words behind "another way".
    Send,
    /// One of a question's choices, by index.
    Choice(usize),
    /// Show or hide the exact arguments.
    Raw,
}

/// What a layer's rows are built from.
pub enum Source {
    /// The work mode.
    Work,
    /// How often Sterna asks.
    Ask,
    /// The boundary the session runs under.
    Access,
    /// The confirmation before a rung that cannot be taken back.
    Confirm(String),
    /// Every key, and what it does.
    Keys,
    /// Local notices, newest last.
    Activity,
    /// Every theme; `before` is what Esc and Undo go back to.
    Themes {
        before: crate::tui::Theme,
    },
    Settings(Box<Preferences>),
    Models(Box<Navigator>),
    /// A panel the session sent.
    Panel(Panel),
    /// The whole list of values of the Value row with this id on the layer
    /// under it, for when they did not fit in its row.
    Fold(String),
}

/// One open surface: its sheet and what builds its rows.
pub struct Layer {
    pub sheet: Sheet,
    pub source: Source,
    /// The command that built a session panel, run again when a person comes
    /// back to it, so a list they changed from a child is not stale.
    pub reopen: Option<String>,
}

#[derive(Default)]
pub struct Workbench {
    pub expanded: BTreeSet<usize>,
    pub collapsed: BTreeSet<usize>,
    pub tabs: std::collections::BTreeMap<usize, CellTab>,
    pub helper: Option<(usize, usize)>,
    pub selected_cell: Option<usize>,
    /// Every open surface, the top one last.
    pub sheets: Vec<Layer>,
    pub press: Option<(u16, u16)>,
    pub dragged: bool,
    /// Where the pointer rests, for hover; `None` when it has not moved.
    pub hover: Option<(u16, u16)>,
    pub notice: String,
    pub geometry: Geometry,
    pub anchor: Option<((usize, usize), String)>,
    pub last_scrollback: usize,
    pub jump_cell: Option<usize>,
    /// The last live change a dock chip made -- what to call it, and the
    /// command that takes it back -- offered beside its notice.
    pub undo: Option<(String, String)>,
    /// When [`Workbench::notice`] was last set, so it can fade.
    pub notice_at: Option<std::time::Instant>,
    pub shown_notice: String,
    /// A sheet row sent this command to the session: the panel it produces
    /// opens as that sheet's child.
    pub child_of: Option<String>,
    /// Back re-ran the parent's command: the panel it produces replaces the
    /// parent rather than stacking on it.
    pub reopening: bool,
    /// A settings row asked for the model picker: the setting it fills.
    pub browsing: Option<String>,
    /// Handlers a person turned off whose turning-off has not shown yet.
    pub turning_off: BTreeSet<String>,
}
/// How long a notice rides the dock's edge before it fades. It is still in
/// the transcript and on the Activity surface after that.
pub const NOTICE_LINGER: std::time::Duration = std::time::Duration::from_secs(4);
impl Workbench {
    /// Opens a surface as the only one: what a chip on the chrome does.
    pub fn open(&mut self, source: Source) {
        self.sheets.clear();
        self.push(source);
    }
    /// Opens a surface on top of the one showing: what a row that goes
    /// somewhere does.
    pub fn push(&mut self, source: Source) {
        let mut sheet = Sheet::default();
        sheet.root = self.sheets.is_empty();
        self.sheets.push(Layer {
            sheet,
            source,
            reopen: None,
        });
    }
    pub fn open_settings(&mut self, state: &ScreenState) {
        match Preferences::open(state) {
            Ok(p) => self.open(Source::Settings(Box::new(p))),
            Err(e) => self.notice = e,
        }
    }
    /// Back one layer.
    pub fn close(&mut self) {
        self.sheets.pop();
    }
    /// Every layer, at once.
    pub fn close_all(&mut self) {
        self.sheets.clear();
        self.child_of = None;
        self.reopening = false;
    }
    pub fn is_local(&self) -> bool {
        !self.sheets.is_empty()
    }
    pub fn top(&self) -> Option<&Layer> {
        self.sheets.last()
    }
    pub fn top_mut(&mut self) -> Option<&mut Layer> {
        self.sheets.last_mut()
    }
    /// The settings being edited, if a settings sheet is open anywhere in
    /// the stack.
    pub fn preferences(&self) -> Option<&Preferences> {
        self.sheets
            .iter()
            .rev()
            .find_map(|layer| match &layer.source {
                Source::Settings(p) => Some(p.as_ref()),
                _ => None,
            })
    }
    pub fn preferences_mut(&mut self) -> Option<&mut Preferences> {
        self.sheets
            .iter_mut()
            .rev()
            .find_map(|layer| match &mut layer.source {
                Source::Settings(p) => Some(p.as_mut()),
                _ => None,
            })
    }
    /// The model picker, if it is the top sheet.
    pub fn models(&self) -> Option<&Navigator> {
        match &self.top()?.source {
            Source::Models(m) => Some(m.as_ref()),
            _ => None,
        }
    }
    pub fn models_mut(&mut self) -> Option<&mut Navigator> {
        match &mut self.top_mut()?.source {
            Source::Models(m) => Some(m.as_mut()),
            _ => None,
        }
    }
    /// Whether the top sheet is built from this kind of source.
    pub fn showing(&self, wanted: fn(&Source) -> bool) -> bool {
        self.top().is_some_and(|layer| wanted(&layer.source))
    }
    /// The open session panel with this title, for the loop to refresh in
    /// place.
    pub fn panel_mut(&mut self, title: &str) -> Option<&mut Panel> {
        self.sheets
            .iter_mut()
            .rev()
            .find_map(|layer| match &mut layer.source {
                Source::Panel(panel) if panel.title == title => Some(panel),
                _ => None,
            })
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
    /// Takes a panel the session sent and opens it: as the child of the sheet
    /// whose row asked for it, in place of a parent being reopened, and
    /// otherwise as the only sheet.
    pub fn absorb_panel(&mut self, state: &mut ScreenState) {
        let Some(panel) = state.panel.take() else {
            return;
        };
        // A one-line result is a notice, not a sheet: a sheet would take the
        // keys typed next for the sake of one sentence.
        if panel.assignment.is_none()
            && let [only] = panel.rows.as_slice()
            && !only.acts()
        {
            let text = only.text.clone();
            self.child_of = None;
            match self.sheets.last_mut() {
                Some(layer) => layer.sheet.notice = text,
                None => self.notice = text,
            }
            return;
        }
        let source = if panel.assignment.is_some() {
            let Some(mut model) = Navigator::from_panel(&panel) else {
                return;
            };
            if let Some(root) = &state.settings_root
                && let Ok(loaded) = crate::settings::Store::new(root)
                    .and_then(|store| store.load(state.settings_profile.as_deref()))
            {
                model.assignment = loaded.config.agents;
            }
            if let Some(key) = self.browsing.take() {
                model.role = if key.starts_with("agents.") {
                    2
                } else if key == "helpers.model" {
                    1
                } else {
                    0
                };
                model.slot = key
                    .strip_prefix("agents.slots.")
                    .and_then(|k| k.strip_suffix(".model"))
                    .map(str::to_string);
                model.target_key = Some(key);
            }
            model.select_current();
            Source::Models(Box::new(model))
        } else {
            Source::Panel(panel)
        };
        // The same panel again -- a sign-in that redraws while it waits --
        // replaces itself, keeping the row a person is on.
        if let (Source::Panel(next), Some(top)) = (&source, self.sheets.last_mut())
            && matches!(&top.source, Source::Panel(now) if now.title == next.title)
        {
            top.source = source;
            self.reopening = false;
            return;
        }
        let reopen = self.child_of.take();
        if std::mem::take(&mut self.reopening)
            && let Some(top) = self.sheets.last_mut()
        {
            top.source = source;
            return;
        }
        if reopen.is_none() {
            self.sheets.clear();
        }
        self.push(source);
        if let Some(top) = self.sheets.last_mut() {
            top.reopen = reopen;
        }
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
}
