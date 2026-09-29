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
pub mod facts;
mod hosts;
mod input;
pub mod look;
mod markdown;
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
pub(crate) use chrome::{frame, glow};
pub use document::{Document, Row, RowKind, Tone};
pub use hosts::HostsSheet;
pub use input::{Effect, mid_turn};
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
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    Cell(usize),
    /// A row of the composer's popup: a command runs, a path completes.
    Completion(usize),
    /// Put this in the composer in place of what is there: a command the
    /// completion list or /help offers.
    Insert(String),
    /// A suggested message: the draft when the composer is empty, else added
    /// to it after a blank line.
    Draft(String),
    Path(String),
    Tab(usize, CellTab),
    /// A gate note's further lines, under its first, shown or put away: the
    /// index of the note in the session's history.
    Note(usize),
    Latest,
    Settings,
    Models,
    /// The sandbox sheet: the level, how it is enforced, and the answers
    /// remembered for the session.
    Sandbox,
    /// The hosts sheet: the ecosystems commands may reach, and the person's
    /// own hosts.
    Hosts,
    /// Switch one ecosystem, by its settings word, on or off.
    Ecosystem(String),
    /// Take one of the person's own hosts off the list.
    RemoveHost(String),
    /// Add the host typed on the hosts sheet.
    AddHost,
    Activity,
    Setting(usize, Option<String>),
    /// Go to a named scope. It used to be a bare toggle shared by both
    /// tabs, so clicking the tab you were already on moved you off it --
    /// the one gesture that should have done nothing was the one that
    /// changed which file a save would land in.
    Scope(bool),
    /// Take back the last change on the session's undo list.
    Undo,
    /// Put these keys back as they were in one scope (`true` is Global):
    /// what taking back a settings save does.
    Restore(bool, Vec<(String, Option<String>)>),
    /// Remove the settings row's value from the scope Settings shows, so
    /// the inherited or built-in one applies.
    UseDefault(usize),
    Slot(Option<String>),
    Model(usize),
    ChooseModel,
    /// Give this model to the tier or slot the picker is on, without asking
    /// again: what a confirmation's Yes carries.
    Choose(String),
    /// A favourite's effort, set in place.
    SlotEffort(String, String),
    UnsetModel,
    /// Back one layer: the top sheet closes and the one under it shows.
    Close,
    Composer,
    Command(String),
    /// Set the sandbox level by its word; Full access is confirmed first.
    Level(String),
    /// The confirmed half of a change to Full access.
    ConfirmLevel(String),
    /// The confirmed half of a dangerous settings save: the key and value,
    /// saved to the scope Settings shows.
    ConfirmSetting(String, String),
    Sources,
    Scores,
    /// Open settings already on the category that owns the thing just
    /// clicked, so a control on the strip is one click from the row that
    /// changes it rather than four.
    SettingsAt(usize),
    /// The one-screen sheet of every key.
    Help,
    /// Step what the screen shows of a cell while it is being written.
    Stream,
    /// A theme, applied at once; the sheet stays open.
    Theme(crate::tui::Theme),
    /// The live instruments.
    Telemetry,
    /// Open a link in the person's browser: what a confirmation's Yes does.
    OpenLink(String),
    /// Ask before opening a link in the person's browser.
    AskOpenLink(String),
    /// Stop the sign-in running beside the session.
    CancelSignIn,
    /// Bring back the running sign-in's panel.
    ReopenSignIn,
    /// Put this text on the clipboard.
    Copy(String),
    /// The form that takes the address a browser ended on.
    PasteCallback,
    /// A click on one of the top sheet's own targets.
    Sheet(sheet::Hit),
    /// An answer on a decision prompt.
    Answer(Answer),
    /// Forget a call answered for the whole session: the next identical
    /// call asks again.
    Forget(String),
    /// The text of the field at this row changed.
    FieldEdited(usize),
    /// The controls a row of chips had no room for: a sheet lists them.
    More(Vec<(String, Action)>),
    /// Go to this session: this one ends and that one starts here.
    Resume(String),
}
#[derive(Debug, Clone, Default)]
pub struct Geometry {
    pub transcript: Rect,
    pub composer: Rect,
    /// Where the composer's popup is drawn, when it is.
    pub popup: Option<Rect>,
    /// Where the sidebar is drawn, when it is.
    pub sidebar: Option<Rect>,
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
/// A tone as the workbench paints it in `theme`, for a surface drawn
/// outside the workbench (telemetry) that must keep the same roles.
pub(crate) fn tone_style(tone: Tone, theme: crate::tui::Theme) -> ratatui::style::Style {
    theme::style(tone, theme)
}

/// The workbench's one filled style: a chosen chip, readable ink on the
/// accent.
pub(crate) fn chip_style(theme: crate::tui::Theme) -> ratatui::style::Style {
    theme::chip_on(theme)
}

pub(crate) fn contains(r: Rect, x: u16, y: u16) -> bool {
    x >= r.x && y >= r.y && x < r.right() && y < r.bottom()
}
/// The answers a decision prompt offers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Answer {
    AllowOnce,
    AllowForSession,
    /// Let the refused hosts through for the session; the command runs
    /// again inside the sandbox.
    AllowHost,
    /// The same, and saved to the global `sandbox.hosts`.
    AlwaysAllowHost,
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
    /// The sandbox level, how it is enforced, and what was answered.
    Sandbox,
    /// The allowed hosts, as the global settings hold them.
    Hosts(Box<hosts::HostsSheet>),
    /// The confirmation before a change that lifts a boundary.
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
    Panel(Box<Panel>),
    /// The whole list of values of the Value row with this id on the layer
    /// under it, for when they did not fit in its row.
    Fold(String),
    /// The controls a row of chips folded into its `+N` or `⋯` chip.
    More(Vec<(String, Action)>),
}

/// One change on the undo list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Change {
    /// What comes back, in the words the screen uses: `Theme ice`,
    /// `effort medium`.
    pub was: String,
    /// What brings it back.
    pub back: Action,
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
    /// The gate notes whose further lines are open.
    pub notes_open: BTreeSet<usize>,
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
    /// The first composer row in view. It moves only when the caret leaves
    /// the window, or with the wheel, so a click never makes the draft jump.
    pub composer_scroll: usize,
    /// The caret the window last followed.
    composer_caret: Option<usize>,
    /// Rows of the draft out of view, above and below the window.
    pub composer_hidden: (usize, usize),
    /// **One undo list for the whole session**, newest last: every change a
    /// sheet, a chip, a settings row or a command made, and how to take it
    /// back. Ctrl-Z on a sheet and the undo chip beside a notice both take
    /// the newest.
    pub changes: Vec<Change>,
    /// Whether the newest change is the one the notice on screen is about,
    /// so its undo chip rides beside that notice and no other.
    pub offer_undo: bool,
    /// An undo is being carried out: what it does is not itself a change.
    pub(crate) undoing: bool,
    /// The command an undo sent to the session, which is not a change either
    /// when it comes back through [`Workbench::sent`].
    pub(crate) undoing_command: Option<String>,
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
}
/// How long a notice rides the dock's edge before it fades. It is still in
/// the transcript and on the Activity surface after that.
pub const NOTICE_LINGER: std::time::Duration = std::time::Duration::from_secs(4);
impl Workbench {
    /// The first composer row to draw. The window follows the caret only
    /// when the caret has moved, and never shows past the draft's end.
    pub(super) fn composer_window(
        &mut self,
        rows: usize,
        visible: usize,
        cursor: usize,
        caret_row: usize,
    ) -> usize {
        if self.composer_caret != Some(cursor) {
            self.composer_caret = Some(cursor);
            if caret_row < self.composer_scroll {
                self.composer_scroll = caret_row;
            } else if visible > 0 && caret_row >= self.composer_scroll + visible {
                self.composer_scroll = caret_row + 1 - visible;
            }
        }
        self.composer_scroll = self.composer_scroll.min(rows.saturating_sub(visible));
        self.composer_hidden = (
            self.composer_scroll,
            rows.saturating_sub(self.composer_scroll + visible),
        );
        self.composer_scroll
    }
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
    /// The hosts sheet, if it is the top sheet.
    pub(crate) fn hosts_mut(&mut self) -> Option<&mut hosts::HostsSheet> {
        match &mut self.top_mut()?.source {
            Source::Hosts(h) => Some(h.as_mut()),
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
                Source::Panel(panel) if panel.title == title => Some(panel.as_mut()),
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
            self.offer_undo = false;
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
                && let Ok(loaded) =
                    crate::settings::Store::with_global(root, state.settings_global.clone())
                        .and_then(|store| store.load(state.settings_profile.as_deref()))
            {
                model.assignment = loaded.config.agents;
            }
            if let Some(key) = self.browsing.take() {
                model.role = usize::from(key.starts_with("agents."));
                model.slot = key
                    .strip_prefix("agents.slots.")
                    .and_then(|k| k.strip_suffix(".model"))
                    .map(str::to_string);
                model.target_key = Some(key);
            }
            model.select_current();
            Source::Models(Box::new(model))
        } else {
            Source::Panel(Box::new(panel))
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
