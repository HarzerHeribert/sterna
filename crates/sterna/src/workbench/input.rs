//! Input reduction for the new surface: click activates only on release.
//!
//! While a sheet is open every key, click, wheel notch and paste is the
//! sheet's (`sheet.rs` decides what each does); this module only carries out
//! the [`Outcome`] it answers with.
use super::{Action, CellTab, Layer, Outcome, Preferences, Source, Workbench};
use crate::tui::{Notebook, ScreenState, Selection};
use crossterm::event::{Event, KeyCode, KeyModifiers, MouseButton, MouseEventKind};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Effect {
    Insert(String),
    /// Add to the draft: in place of an empty one, else after a blank line.
    Draft(String),
    OpenPath(String),
    Pass,
    /// Handled; draw.
    Consumed,
    /// Handled, and nothing on screen changed: no frame is owed.
    Ignored,
    Command(String),
    Copy(String),
    Cursor(usize),
    OpenLink(String),
    /// Open the form that takes the address a browser ended on.
    PasteCallback,
    CancelSignIn,
    ReopenSignIn,
    HandlerOff(String),
}
impl Workbench {
    /// Writes one presentation key to the settings (globally, decision 6),
    /// puts it on the undo list, and says whether the file took it. A
    /// session with no project root is not an error here -- the choice
    /// still applies to the running screen, and the notice says which of
    /// the two happened.
    fn persist(&mut self, key: &str, value: &str, s: &mut ScreenState) -> bool {
        let Ok(mut p) = super::Preferences::open(s) else {
            return false;
        };
        let saved = p.save(key, Some(value.to_string()), s).is_ok();
        if saved {
            self.notice = p.notice.clone();
        }
        if let Some(change) = p.take_change() {
            self.remember(change);
        }
        saved
    }
    /// Puts a change on the session's undo list, unless it is an undo being
    /// carried out.
    pub(crate) fn remember(&mut self, change: super::Change) {
        if !self.undoing {
            self.changes.push(change);
            self.offer_undo = true;
        }
    }
    /// After a save on the open Settings: its change goes on the undo list
    /// and the command it owes the running session is handed back.
    fn drain_save(&mut self) -> Option<String> {
        let (change, live) = match self.preferences_mut() {
            Some(p) => (p.take_change(), p.take_live()),
            None => (None, None),
        };
        if let Some(change) = change {
            self.remember(change);
        }
        live
    }
    /// Takes back the newest change: the same route that made it, run
    /// backwards, and a notice that names what came back.
    fn undo_last(&mut self, s: &mut ScreenState, n: &Notebook, busy: bool) -> Effect {
        let Some(change) = self.changes.pop() else {
            self.say("Nothing to undo.");
            return Effect::Consumed;
        };
        self.undoing = true;
        let effect = self.activate(change.back, s, n, busy, true);
        self.undoing = false;
        if let Effect::Command(command) = &effect {
            self.undoing_command = Some(command.clone());
        }
        self.offer_undo = false;
        self.say(format!("Restored: {}.", change.was));
        effect
    }
    /// Where a rung, mode or effort chosen now is saved: the scope Settings
    /// is open on, else the global settings (decision 6).
    pub fn scope(&self) -> crate::settings::Scope {
        self.sheets
            .iter()
            .rev()
            .find_map(|layer| match &layer.source {
                Source::Settings(p) => Some(p.scope),
                _ => None,
            })
            .unwrap_or(crate::settings::Scope::Global)
    }
    /// A command is on its way to the session: the mode or effort it sets
    /// is saved here, whichever chip, row or keyboard sent it.
    pub fn sent(&mut self, command: &str, s: &ScreenState) {
        super::facts::saving(s, command, self.scope());
        // What it replaces goes on the undo list, unless it is an undo.
        if self.undoing_command.as_deref() == Some(command) {
            self.undoing_command = None;
        } else {
            let words: Vec<_> = command.split_whitespace().collect();
            let change = match words.as_slice() {
                ["/effort", word] if crate::wire::Effort::parse(word) != Some(s.effort) => {
                    Some(super::Change {
                        was: format!("effort {}", s.effort.name()),
                        back: Action::Command(format!("/effort {}", s.effort.name())),
                    })
                }
                ["/mode", _] => Some(super::Change {
                    was: format!("mode {}", super::facts::mode_word(s)),
                    back: Action::Command(super::facts::mode_command(
                        s.mode_pinned.then_some(s.mode),
                    )),
                }),
                _ => None,
            };
            if let Some(change) = change {
                self.remember(change);
            }
        }
        if let Some(p) = self.preferences_mut() {
            p.refresh();
        }
    }
    /// Every route that moves the rung. Never asks is confirmed first, on a
    /// sheet that opens on Cancel; any other rung is set at once, saved,
    /// and offered back.
    fn rung(&mut self, rung: crate::permissions::Rung, s: &mut ScreenState) {
        let before = s.permissions.rung();
        if rung == crate::permissions::Rung::Full && before != rung {
            self.push(Source::Confirm("full".into()));
            return;
        }
        let notice = super::facts::set_rung(s, rung, self.scope());
        if let Some(p) = self.preferences_mut() {
            p.refresh();
        }
        self.say(notice);
        if before != rung {
            self.remember(super::Change {
                was: format!("Ask {}", before.label()),
                back: Action::Rung(before.name().into()),
            });
        }
    }
    pub fn local_command(&mut self, text: &str, s: &mut ScreenState, n: &Notebook) -> bool {
        let parts: Vec<_> = text.split_whitespace().collect();
        // A rung is named by its label or its file word, and a label can be
        // two words: `/permissions every call`.
        if let ["/permissions", rest @ ..] = parts.as_slice()
            && let Some(rung) = crate::permissions::Rung::parse(&rest.join(" "))
        {
            self.rung(rung, s);
            return true;
        }
        match parts.as_slice() {
            ["/settings"] => {
                self.open_settings(s);
                true
            }
            ["/config"] => {
                self.open_settings_at(s, 5, None);
                true
            }
            // A word after /settings or /config is a setting to open on.
            ["/settings" | "/config", words @ ..] => {
                self.open_on_setting(s, &words.join(" "));
                true
            }
            // Bare, each of these opens on its own row.
            ["/motion"] => {
                self.open_settings_at(s, 1, Some("ui.motion"));
                true
            }
            ["/sidebar"] => {
                self.open_settings_at(s, 1, Some("ui.sidebar"));
                true
            }
            ["/stream"] => {
                self.open_settings_at(s, 1, Some("ui.stream"));
                true
            }
            // A command that names one setting opens where that setting is.
            // Bare `/statusline` used to land on the everyday category with
            // the status line nowhere in sight.
            ["/statusline"] => {
                self.open_settings_at(s, 1, Some("ui.statusline"));
                true
            }
            ["/diff"] => {
                if !n.cells.is_empty() {
                    let cell = n.cells.len();
                    self.expanded.insert(cell);
                    self.collapsed.remove(&cell);
                    self.tabs.insert(cell, CellTab::Diff);
                    self.selected_cell = Some(cell);
                    s.scrollback = 0;
                } else {
                    self.notice = "No recorded cell diff yet.".into();
                }
                true
            }
            ["/cell", number] => {
                if let Ok(cell) = number.parse::<usize>() {
                    if cell > 0 && cell <= n.cells.len() {
                        self.expanded.insert(cell);
                        self.collapsed.remove(&cell);
                        self.selected_cell = Some(cell);
                        self.jump_cell = Some(cell);
                        self.notice = format!("Cell {cell} expanded · F4 opens its diff");
                    } else {
                        self.notice = "No recorded cell at that number.".into();
                    }
                }
                true
            }
            ["/cells"] => {
                self.expanded.extend(1..=n.cells.len());
                self.collapsed.clear();
                s.scrollback = 0;
                true
            }
            ["/chat"] => {
                self.close_all();
                s.inspection = None;
                s.telemetry_open = false;
                true
            }
            ["/activity"] => {
                s.telemetry_open = false;
                self.open(Source::Activity);
                true
            }
            ["/telemetry"] => {
                self.close_all();
                s.telemetry_open = true;
                true
            }
            ["/mode"] => {
                self.open(Source::Work);
                true
            }
            ["/permissions"] => {
                self.open(Source::Ask);
                true
            }
            // Presentation is this layer's own business: a palette, a
            // status line, a sidebar and motion never reach the model, and
            // each one says what it did where a person can scroll back to it.
            ["/theme"] => {
                s.notice = None;
                self.open(Source::Themes { before: s.theme });
                true
            }
            ["/theme", name] => {
                match crate::tui::Theme::parse(name) {
                    Some(theme) => {
                        self.persist("ui.theme", theme.name(), s);
                        s.theme = theme;
                        s.note(format!(
                            "Theme: {} · /theme opens the palette",
                            theme.name()
                        ));
                    }
                    None => s.note("Unknown theme. /theme opens the palette."),
                }
                true
            }
            ["/motion", word] if crate::tui::Motion::parse(word).is_some() => {
                let motion = crate::tui::Motion::parse(word).unwrap_or_default();
                self.persist("ui.motion", motion.name(), s);
                s.set_motion(motion);
                s.note(match motion {
                    crate::tui::Motion::Off => {
                        "Motion reduced: off, nothing moves. /motion full or calm restores it."
                    }
                    crate::tui::Motion::Calm => {
                        "Motion: calm · working marks turn slowly, no heartbeat or scanner."
                    }
                    crate::tui::Motion::Full => "Motion: full · /motion calm or off for less.",
                });
                true
            }
            ["/motion", ..] => {
                s.note("Usage: /motion full | calm | off");
                true
            }
            ["/stream", word @ ("actions" | "code" | "raw")] => {
                self.persist("ui.stream", word, s);
                s.stream = crate::tui::Stream::parse(word).unwrap_or_default();
                s.note(format!("Streaming cell: {word} · /stream actions|code|raw"));
                true
            }
            ["/stream", ..] => {
                s.note("Usage: /stream actions | code | raw");
                true
            }
            ["/sidebar", word @ ("auto" | "show" | "hide")] => {
                self.persist("ui.sidebar", word, s);
                s.sidebar = match *word {
                    "show" => crate::tui::SidebarVisibility::Shown,
                    "hide" => crate::tui::SidebarVisibility::Hidden,
                    _ => crate::tui::SidebarVisibility::Auto,
                };
                // The line names the three words and the key, exactly as it
                // always has: someone who just used one of them is the
                // likeliest person to want another.
                s.note(format!(
                    "Sidebar: /sidebar auto|show|hide · Ctrl-B toggles · now {word}"
                ));
                true
            }
            ["/fullscreen"] => {
                s.fullscreen = !s.fullscreen;
                s.note(if s.fullscreen {
                    "Fullscreen. Ctrl-F or /fullscreen restores the chrome."
                } else {
                    "Chrome restored."
                });
                true
            }
            [
                "/statusline",
                word @ ("full" | "compact" | "hidden" | "hide"),
            ] => {
                let word = if *word == "hide" { "hidden" } else { word };
                let saved = self.persist("ui.statusline", word, s);
                s.status_line = match word {
                    "compact" => crate::tui::StatusLine::Compact,
                    "hidden" => crate::tui::StatusLine::Hidden,
                    _ => crate::tui::StatusLine::Full,
                };
                if saved {
                    s.note(format!("Status line saved: {word}"));
                } else {
                    s.note(format!("Status line: {word} · this session only"));
                }
                true
            }
            ["/sidebar", ..] => {
                s.note("Usage: /sidebar auto | show | hide");
                true
            }
            ["/statusline", ..] => {
                s.note("Usage: /statusline full | compact | hidden");
                true
            }
            _ => false,
        }
    }
    /// The text under the selection, read off the last drawn screen.
    fn selected_text(&self, s: &ScreenState) -> String {
        match (self.geometry.screen.as_ref(), s.selection) {
            (Some(screen), Some(selection)) if !selection.is_empty() => {
                let mut screen = screen.clone();
                let area = screen.area;
                crate::tui::draw_selection(&mut screen, area, selection)
            }
            _ => String::new(),
        }
    }
    /// Opens settings on one category, and on one row of it when named.
    /// `/settings <word>`: Settings, open on the row the word names -- its
    /// key, the key's last part or its label, in any case. A word that
    /// names none still opens Settings, and says so.
    fn open_on_setting(&mut self, s: &ScreenState, word: &str) {
        let wanted = word.trim().to_lowercase();
        let found = crate::settings::specs().iter().find(|spec| {
            !crate::settings::hidden(spec.key)
                && (spec.key == wanted
                    || spec.key.rsplit('.').next() == Some(wanted.as_str())
                    || spec.label.to_lowercase() == wanted)
        });
        match found {
            Some(spec) => {
                self.open_settings_at(s, super::settings::category_of(spec), Some(spec.key));
            }
            None => {
                self.open_settings(s);
                self.say(format!("No setting named {word} · opened Settings"));
            }
        }
    }
    pub fn open_settings_at(&mut self, s: &ScreenState, category: usize, key: Option<&str>) {
        self.open_settings(s);
        if let Some(Layer {
            sheet,
            source: Source::Settings(p),
            ..
        }) = self.sheets.last_mut()
        {
            p.category = category.min(super::settings::CATEGORIES.len() - 1);
            sheet.prefer = key.map(|key| format!("setting:{key}"));
        }
    }
    /// Opens a surface: as the only one from the chrome, as a child from a
    /// sheet row.
    fn show(&mut self, source: Source, from_sheet: bool) {
        if from_sheet {
            self.push(source);
        } else {
            self.open(source);
        }
    }
    pub fn event(&mut self, e: &Event, s: &mut ScreenState, n: &Notebook, busy: bool) -> Effect {
        match e {
            Event::Mouse(m) => {
                if s.mouse_off {
                    return Effect::Consumed;
                }
                match m.kind {
                    // **Hover never moves focus and never changes a value.**
                    // It is drawn, and only when the target under the
                    // pointer changes is a frame owed for it.
                    MouseEventKind::Moved => {
                        let before = self.hover.and_then(|(x, y)| self.geometry.hit_rect(x, y));
                        self.hover = Some((m.column, m.row));
                        if self.geometry.hit_rect(m.column, m.row) == before {
                            Effect::Ignored
                        } else {
                            Effect::Consumed
                        }
                    }
                    MouseEventKind::Down(MouseButton::Left) => {
                        self.press = Some((m.column, m.row));
                        self.dragged = false;
                        s.selection = None;
                        Effect::Consumed
                    }
                    MouseEventKind::Drag(MouseButton::Left) => {
                        if let Some(anchor) = self.press
                            && self.sheets.is_empty()
                        {
                            self.dragged = true;
                            s.selection = Some(Selection {
                                anchor,
                                head: (m.column, m.row),
                            });
                        }
                        Effect::Consumed
                    }
                    MouseEventKind::Up(MouseButton::Left) => {
                        let anchor = self.press.take();
                        if self.dragged {
                            self.dragged = false;
                            let copied = self.selected_text(s);
                            return if copied.is_empty() {
                                Effect::Consumed
                            } else {
                                Effect::Copy(copied)
                            };
                        }
                        if anchor != Some((m.column, m.row)) {
                            return Effect::Consumed;
                        }
                        match self.geometry.hit(m.column, m.row) {
                            Some(Action::Composer) => self.composer_click(s, m.column, m.row),
                            Some(Action::Sheet(hit)) => {
                                let outcome = match self.sheets.last_mut() {
                                    Some(layer) => layer.sheet.click(&hit),
                                    None => Outcome::Nothing,
                                };
                                self.apply(outcome, s, n, busy)
                            }
                            Some(action) => self.activate(action, s, n, busy, false),
                            // A click on the backdrop outside a sheet is Esc,
                            // except on a decision, which only its answers end.
                            None if self.backdrop(m.column, m.row) => self.back(busy),
                            None => Effect::Consumed,
                        }
                    }
                    MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => {
                        let up = m.kind == MouseEventKind::ScrollUp;
                        s.selection = None;
                        if let Some(layer) = self.sheets.last_mut() {
                            // The wheel scrolls the view; the focused row
                            // stays where it is.
                            return match layer.sheet.wheel(up) {
                                Outcome::Nothing => Effect::Ignored,
                                _ => Effect::Consumed,
                            };
                        }
                        s.scrollback = if up {
                            s.scrollback.saturating_add(3).min(
                                self.geometry
                                    .rows
                                    .saturating_sub(self.geometry.transcript.height as usize),
                            )
                        } else {
                            s.scrollback.saturating_sub(3)
                        };
                        s.scrolling = true;
                        Effect::Consumed
                    }
                    _ => Effect::Ignored,
                }
            }
            Event::Paste(text) => match self.sheets.last_mut() {
                Some(layer) => {
                    let outcome = layer.sheet.paste(text);
                    self.apply(outcome, s, n, busy)
                }
                None if s.telemetry_open => Effect::Consumed,
                None => Effect::Pass,
            },
            Event::Key(k) => {
                if k.kind == crossterm::event::KeyEventKind::Release {
                    return Effect::Consumed;
                }
                let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
                // Ctrl-C over a selection is a copy, as in any terminal with
                // one: it never interrupts, and the selection stays.
                if ctrl && k.code == KeyCode::Char('c') {
                    let copied = self.selected_text(s);
                    if !copied.is_empty() {
                        return Effect::Copy(copied);
                    }
                    return Effect::Pass;
                }
                s.selection = None;
                self.notice.clear();
                if !self.sheets.is_empty() {
                    super::sheets::build(self, s, n);
                    if let Some(action) = self.accelerator(k) {
                        return self.activate(action, s, n, busy, true);
                    }
                    let outcome = match self.sheets.last_mut() {
                        Some(layer) => layer.sheet.key(*k),
                        None => Outcome::Nothing,
                    };
                    return self.apply(outcome, s, n, busy);
                }
                // The instruments take the transcript's room and leave the
                // composer where it is: Esc leaves them, ↑↓ walks the
                // requests they list, and typing still reaches the draft.
                if s.telemetry_open {
                    match k.code {
                        KeyCode::Esc => {
                            s.telemetry_open = false;
                            return Effect::Consumed;
                        }
                        KeyCode::Up => {
                            s.telemetry_selected =
                                Some(s.telemetry_selected.unwrap_or(0).saturating_add(1));
                            return Effect::Consumed;
                        }
                        KeyCode::Down => {
                            s.telemetry_selected =
                                s.telemetry_selected.and_then(|i| i.checked_sub(1));
                            return Effect::Consumed;
                        }
                        _ => {}
                    }
                }
                match k.code {
                    KeyCode::Char('t') if ctrl => {
                        s.telemetry_open = !s.telemetry_open;
                        Effect::Consumed
                    }
                    KeyCode::F(2) => {
                        self.open_settings(s);
                        Effect::Consumed
                    }
                    KeyCode::F(3) => self.activate(Action::Models, s, n, busy, false),
                    KeyCode::F(4) => {
                        let cell = self.selected_cell.unwrap_or(n.cells.len());
                        self.activate(Action::Tab(cell, CellTab::Diff), s, n, busy, false)
                    }
                    KeyCode::F(5) => {
                        let cell = self.selected_cell.unwrap_or(n.cells.len());
                        self.activate(Action::Tab(cell, CellTab::Helpers), s, n, busy, false)
                    }
                    // **Shift-Tab moves the rung; it does not open a place
                    // where a rung can be moved.** Opening the surface cost
                    // three cursor moves and an Enter to reach a choice the
                    // key could have made by itself, which is four keystrokes
                    // of ceremony on the single control a person touches most
                    // -- and it is why the acceptance test for this path was
                    // the one that kept flaking. Both neighbouring products
                    // cycle here. The surface is still one click away on the
                    // control itself, so the visible route and the fast route
                    // are the same route, found in stages.
                    KeyCode::BackTab => Effect::Pass,
                    // `?` on an empty composer is the sheet of keys, as it is
                    // in the neighbouring product; with anything typed it is
                    // a question mark.
                    KeyCode::Char('?') if s.input.is_empty() && !ctrl => {
                        self.open(Source::Keys);
                        Effect::Consumed
                    }
                    KeyCode::Char('o') if ctrl => self.activate(
                        Action::Cell(self.selected_cell.unwrap_or(n.cells.len())),
                        s,
                        n,
                        busy,
                        false,
                    ),
                    _ => Effect::Pass,
                }
            }
            _ => Effect::Pass,
        }
    }
    /// A key that stands for one of the top sheet's tool chips: F6 for the
    /// settings scope, Ctrl-A and Ctrl-O for which accounts and which order
    /// the model list shows. Each is also a chip on the sheet.
    fn accelerator(&self, k: &crossterm::event::KeyEvent) -> Option<Action> {
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        match (&self.sheets.last()?.source, k.code) {
            (Source::Settings(p), KeyCode::F(6)) => {
                Some(Action::Scope(p.scope != crate::settings::Scope::Global))
            }
            (Source::Models(_), KeyCode::Char('a')) if ctrl => Some(Action::Sources),
            (Source::Models(_), KeyCode::Char('o')) if ctrl => Some(Action::Scores),
            _ => None,
        }
    }
    /// Whether a point is outside the open sheet, on its backdrop, and the
    /// sheet is one a backdrop click may dismiss.
    fn backdrop(&self, x: u16, y: u16) -> bool {
        let Some(layer) = self.sheets.last() else {
            return false;
        };
        !layer.sheet.decision
            && self
                .geometry
                .local
                .is_some_and(|area| !super::contains(area, x, y))
    }
    /// Where a click in the composer puts the caret.
    fn composer_click(&self, s: &ScreenState, column: u16, row: u16) -> Effect {
        let width = self.geometry.composer.width as usize;
        let lines = super::view::wrap_input(&s.input, width);
        let cursor = s.cursor.unwrap_or(s.input.len()).min(s.input.len());
        let before = &s.input[..s.input.floor_char_boundary(cursor)];
        let cr = super::view::wrap_input(before, width)
            .len()
            .saturating_sub(1);
        let skip = cr.saturating_sub(self.geometry.composer.height.saturating_sub(1) as usize);
        let wanted = row.saturating_sub(self.geometry.composer.y) as usize + skip;
        let col = column.saturating_sub(self.geometry.composer.x) as usize;
        let mut offset = 0;
        for (i, line) in lines.iter().enumerate() {
            if i == wanted {
                let mut x = 0;
                for (byte, ch) in line.char_indices() {
                    let w = ratatui::text::Span::raw(ch.to_string()).width();
                    if x + w > col {
                        return Effect::Cursor(offset + byte);
                    }
                    x += w;
                }
                return Effect::Cursor((offset + line.len()).min(s.input.len()));
            }
            offset += line.len();
            if s.input.as_bytes().get(offset) == Some(&b'\n') {
                offset += 1;
            }
        }
        Effect::Cursor(s.input.len())
    }
    /// Carries out what the top sheet answered.
    fn apply(&mut self, outcome: Outcome, s: &mut ScreenState, n: &Notebook, busy: bool) -> Effect {
        match outcome {
            Outcome::Nothing => Effect::Ignored,
            Outcome::Redraw => Effect::Consumed,
            Outcome::Act(action) => self.activate(action, s, n, busy, true),
            Outcome::Back => self.back(busy),
            Outcome::Section(section) => {
                if let Some(layer) = self.sheets.last_mut() {
                    match &mut layer.source {
                        Source::Settings(p) => {
                            p.category = section.min(super::settings::CATEGORIES.len() - 1);
                            p.editing = None;
                        }
                        Source::Models(m) if m.target_key.is_none() => {
                            m.role = section.min(2);
                            m.select_current();
                        }
                        _ => {}
                    }
                }
                Effect::Consumed
            }
            Outcome::Undo => self.activate(Action::Undo, s, n, busy, true),
            Outcome::Fold(index) => {
                let id = self
                    .sheets
                    .last()
                    .and_then(|layer| layer.sheet.items.get(index))
                    .map(|item| item.id.clone());
                if let Some(id) = id {
                    self.push(Source::Fold(id));
                }
                Effect::Consumed
            }
        }
    }
    /// Esc with nothing left to undo on the sheet: cancel a settings field
    /// edit first, then back one layer. A session panel under it is asked
    /// for again, so a list changed from its child is not shown stale.
    fn back(&mut self, busy: bool) -> Effect {
        if let Some(Layer {
            source: Source::Settings(p),
            ..
        }) = self.sheets.last_mut()
            && p.editing.take().is_some()
        {
            return Effect::Consumed;
        }
        let leaving = self.sheets.pop();
        self.child_of = None;
        // Leaving some panels is an answer: the rollback preview's Esc
        // cancels the rollback rather than leaving it pending.
        if let Some(Layer {
            source: Source::Panel(panel),
            ..
        }) = leaving
            && let Some(Action::Command(command)) = panel.back
        {
            return Effect::Command(command);
        }
        if let Some(parent) = self.sheets.last_mut() {
            parent.sheet.root = false;
            if !busy
                && matches!(parent.source, Source::Panel(_))
                && let Some(command) = parent.reopen.clone()
            {
                self.reopening = true;
                return Effect::Command(command);
            }
        }
        if let Some(first) = self.sheets.first_mut() {
            first.sheet.root = true;
        }
        Effect::Consumed
    }
    /// The top sheet's notice, where a sheet is open; the dock's otherwise.
    fn say(&mut self, text: impl Into<String>) {
        let text = text.into();
        match self.sheets.last_mut() {
            Some(layer) => layer.sheet.notice = text,
            None => self.notice = text,
        }
    }
    fn activate(
        &mut self,
        action: Action,
        s: &mut ScreenState,
        n: &Notebook,
        busy: bool,
        from_sheet: bool,
    ) -> Effect {
        match action {
            Action::Insert(command) => {
                // What is inserted is typed next, so the sheet that offered
                // it gives the keyboard back.
                self.close_all();
                return Effect::Insert(command);
            }
            Action::Draft(message) => {
                self.close_all();
                return Effect::Draft(message);
            }
            Action::Path(path) => return Effect::OpenPath(path),
            Action::Cell(cell) => {
                if cell > 0 {
                    self.selected_cell = Some(cell);
                    let open = self.expanded.contains(&cell)
                        || (cell >= n.cells.len() && !self.collapsed.contains(&cell));
                    if open {
                        self.expanded.remove(&cell);
                        self.collapsed.insert(cell);
                    } else {
                        self.expanded.insert(cell);
                        self.collapsed.remove(&cell);
                    }
                }
            }
            Action::Tab(cell, tab) => {
                if cell > 0 {
                    self.expanded.insert(cell);
                    self.collapsed.remove(&cell);
                    self.tabs.insert(cell, tab);
                    self.selected_cell = Some(cell);
                }
            }
            Action::Helper(cell, h) => {
                self.helper = if self.helper == Some((cell, h)) {
                    None
                } else {
                    Some((cell, h))
                };
            }
            Action::Latest => s.scrollback = 0,
            Action::Settings => match Preferences::open(s) {
                Ok(p) => self.show(Source::Settings(Box::new(p)), from_sheet),
                Err(e) => self.say(e),
            },
            Action::SettingsAt(category) => {
                self.open_settings_at(s, category, None);
            }
            // **The whole point of the strip is that it acts where it
            // stands.** Stepping the effort opens nothing: the word on the
            // strip is the next word before the finger has left the mouse,
            // because `/effort` was always a live control and this is it.
            Action::Effort => {
                // What it was goes on the undo list on its way out
                // (`sent`), and is offered beside the notice the step
                // produces: reversibility over confirmation.
                let next = super::facts::next_effort(s.effort);
                return Effect::Command(format!("/effort {}", next.name()));
            }
            Action::Help => self.show(Source::Keys, from_sheet),
            // The dock's fourth chip steps in place, like the effort one.
            Action::Stream => {
                let next = s.stream.next();
                let word = next.name();
                self.persist("ui.stream", word, s);
                s.stream = next;
                self.notice = format!(
                    "streaming cell: {word} — {}",
                    match s.stream {
                        crate::tui::Stream::Actions => "each action with its size as it arrives",
                        crate::tui::Stream::Code => "the program as it forms",
                        crate::tui::Stream::Raw => "the raw protocol text",
                    }
                );
            }
            Action::Models => {
                if busy {
                    self.say(
                        "Change models after the current turn; in-flight calls retain their model.",
                    );
                } else {
                    if from_sheet {
                        self.child_of = Some("/models".into());
                    } else {
                        self.close_all();
                    }
                    return Effect::Command("/models".into());
                }
            }
            Action::Work => self.show(Source::Work, from_sheet),
            Action::Approvals => self.show(Source::Ask, from_sheet),
            Action::Access => self.show(Source::Access, from_sheet),
            Action::Activity => self.show(Source::Activity, from_sheet),
            Action::Telemetry => {
                self.close_all();
                s.telemetry_open = true;
            }
            Action::Close if self.sheets.is_empty() => s.telemetry_open = false,
            Action::Close => return self.back(busy),
            Action::Scope(global) => {
                let wanted = if global {
                    crate::settings::Scope::Global
                } else {
                    crate::settings::Scope::Local
                };
                if let Some(p) = self.preferences_mut()
                    && p.scope != wanted
                    && let Err(e) = p.switch_scope()
                {
                    p.notice = e;
                }
            }
            Action::Undo => return self.undo_last(s, n, busy),
            Action::Restore(global, keys) => {
                let restored = super::settings::restore(s, global, &keys);
                if let Some(p) = self.preferences_mut() {
                    p.refresh();
                }
                match restored {
                    Ok(Some(command)) => return Effect::Command(command),
                    Ok(None) => {}
                    Err(error) => self.say(error),
                }
            }
            Action::UseDefault(i) => {
                let Some(p) = self.preferences_mut() else {
                    return Effect::Consumed;
                };
                let Some(spec) = p.rows().get(i).copied() else {
                    return Effect::Consumed;
                };
                if let Err(error) = p.save(spec.key, None, s) {
                    p.notice = error;
                }
                if let Some(live) = self.drain_save() {
                    return Effect::Command(live);
                }
            }
            Action::Setting(i, value) => return self.setting(i, value, s, busy),
            Action::FieldEdited(i) => {
                let text = self.sheets.last().and_then(|layer| {
                    match layer.sheet.items.get(i).map(|item| &item.kind) {
                        Some(super::ItemKind::Field(field)) => Some(field.text.clone()),
                        _ => None,
                    }
                });
                if let (Some(text), Some(p)) = (text, self.preferences_mut())
                    && let Some((_, buffer)) = &mut p.editing
                {
                    *buffer = text;
                }
            }
            Action::Slot(slot) => {
                if let Some(m) = self.models_mut()
                    && m.target_key.is_none()
                {
                    m.slot = slot;
                    m.select_current();
                }
            }
            Action::Model(i) => {
                if let Some(m) = self.models_mut() {
                    m.selected = i;
                }
                return self.activate(Action::ChooseModel, s, n, busy, true);
            }
            Action::UnsetModel => {
                let key = self.models().and_then(|m| m.target_key.clone());
                if let Some(key) = key {
                    self.sheets.pop();
                    if let Some(p) = self.preferences_mut()
                        && let Err(e) = p.save(&key, None, s)
                    {
                        p.notice = e;
                    }
                    if let Some(live) = self.drain_save() {
                        return Effect::Command(live);
                    }
                }
            }
            Action::ChooseModel => {
                if busy {
                    self.say("The current turn must finish before changing models.");
                    return Effect::Consumed;
                }
                let Some(m) = self.models() else {
                    return Effect::Consumed;
                };
                match m.choose() {
                    Ok(cmd) => {
                        if m.target_key.is_none() {
                            let model = m.selected_model().unwrap_or_default();
                            // Pinning one model over the favourites turns them
                            // off, so it is asked first.
                            if m.role == 2
                                && m.slot.is_none()
                                && m.assignment.mode == crate::config::AgentsMode::Roster
                            {
                                self.push(Source::Confirm(format!("pin:{model}")));
                                return Effect::Consumed;
                            }
                            return self.activate(Action::Choose(model), s, n, busy, true);
                        }
                        if let Some(key) = m.target_key.clone() {
                            let selected = m.candidates().get(m.selected).map(|c| c.model.clone());
                            self.sheets.pop();
                            if let Some(p) = self.preferences_mut()
                                && let Some(selected) = selected
                                && let Err(error) = p.save(&key, Some(selected), s)
                            {
                                p.notice = error;
                            }
                            self.drain_save();
                        } else {
                            return Effect::Command(cmd);
                        }
                    }
                    Err(e) => self.say(e),
                }
            }
            // **The picker stays open while choosing.** The choice is sent,
            // the mark moves at once, and the notice names what changed.
            Action::Choose(model) => {
                if matches!(
                    self.sheets.last().map(|l| &l.source),
                    Some(Source::Confirm(_))
                ) {
                    self.sheets.pop();
                }
                let Some(m) = self.models_mut() else {
                    return Effect::Consumed;
                };
                let command = m.command_for(&model);
                let notice = m.chosen(&model);
                self.say(notice);
                return Effect::Command(command);
            }
            Action::SlotEffort(slot, effort) => {
                let Some(m) = self.models_mut() else {
                    return Effect::Consumed;
                };
                let Some(held) = m.assignment.slots.get_mut(&slot) else {
                    return Effect::Consumed;
                };
                held.effort = crate::wire::Effort::parse(&effort).unwrap_or(held.effort);
                let command = format!("/subagents {slot} {} {effort}", held.model);
                self.say(format!("{} now runs at {effort}", slot.to_uppercase()));
                return Effect::Command(command);
            }
            Action::Sources => {
                if let Some(m) = self.models_mut() {
                    m.all_sources = !m.all_sources;
                    m.selected = 0;
                }
            }
            Action::Scores => {
                if let Some(m) = self.models_mut() {
                    m.measured_order = !m.measured_order;
                    m.selected = 0;
                }
            }
            Action::Command(cmd) => {
                // A confirmation's Yes: the sheet that asked goes first.
                if matches!(
                    self.sheets.last().map(|l| &l.source),
                    Some(Source::Confirm(_))
                ) {
                    self.sheets.pop();
                }
                if busy {
                    self.say("This runtime change applies between turns; finish or stop the current turn first.");
                } else {
                    if from_sheet {
                        self.child_of = Some(cmd.clone());
                    } else {
                        self.close_all();
                    }
                    return Effect::Command(cmd);
                }
            }
            Action::Rung(rung) => {
                if let Some(rung) = crate::permissions::Rung::parse(&rung) {
                    self.rung(rung, s);
                }
            }
            Action::ConfirmRung(rung) => {
                if let Some(rung) = crate::permissions::Rung::parse(&rung) {
                    if matches!(
                        self.sheets.last().map(|l| &l.source),
                        Some(Source::Confirm(_))
                    ) {
                        self.sheets.pop();
                    }
                    let before = s.permissions.rung();
                    let notice = super::facts::set_rung(s, rung, self.scope());
                    if let Some(p) = self.preferences_mut() {
                        p.refresh();
                    }
                    self.say(notice);
                    self.remember(super::Change {
                        was: format!("Ask {}", before.label()),
                        back: Action::Rung(before.name().into()),
                    });
                }
            }
            Action::ConfirmSetting(key, value) => {
                if matches!(
                    self.sheets.last().map(|l| &l.source),
                    Some(Source::Confirm(_))
                ) {
                    self.sheets.pop();
                }
                if let Some(p) = self.preferences_mut()
                    && let Err(error) = p.save(&key, Some(value), s)
                {
                    p.notice = error;
                }
                if let Some(live) = self.drain_save() {
                    return Effect::Command(live);
                }
            }
            Action::Theme(theme) => {
                self.persist("ui.theme", theme.name(), s);
                s.theme = theme;
                self.notice.clear();
                self.say(format!("Theme is now {}", theme.title()));
            }
            Action::OpenLink(link) => {
                if matches!(
                    self.sheets.last().map(|l| &l.source),
                    Some(Source::Confirm(_))
                ) {
                    self.sheets.pop();
                }
                return Effect::OpenLink(link);
            }
            // **Nothing opens a browser on a single click.**
            Action::AskOpenLink(link) => self.push(Source::Confirm(format!("open:{link}"))),
            Action::CancelSignIn => return Effect::CancelSignIn,
            Action::ReopenSignIn => return Effect::ReopenSignIn,
            Action::Copy(text) => return Effect::Copy(text),
            Action::PasteCallback => return Effect::PasteCallback,
            Action::HandlerOff(name) => {
                if self.turning_off.insert(name.clone()) {
                    return Effect::HandlerOff(name);
                }
                self.say(format!("{name} is already turning off."));
            }
            Action::Forget(id) => {
                let label = s.memory.as_ref().and_then(|memory| {
                    let label = memory
                        .entries()
                        .into_iter()
                        .find(|entry| entry.id == id)
                        .map(|entry| entry.label);
                    memory.forget(&id);
                    label
                });
                if let Some(label) = label {
                    self.say(format!(
                        "Forgot {label}; the next identical call asks again."
                    ));
                }
            }
            // A prompt's own answer: the loop that owns the prompt takes it.
            Action::Answer(_) => {}
            Action::Sheet(hit) => {
                let outcome = match self.sheets.last_mut() {
                    Some(layer) => layer.sheet.click(&hit),
                    None => Outcome::Nothing,
                };
                return self.apply(outcome, s, n, busy);
            }
            Action::Composer => {}
        }
        Effect::Consumed
    }
    /// A settings row: a value chip saves, a model row opens the picker as
    /// the settings' child, and any other row opens its value for editing --
    /// or, while it is being edited, saves it.
    fn setting(
        &mut self,
        i: usize,
        value: Option<String>,
        s: &mut ScreenState,
        busy: bool,
    ) -> Effect {
        let Some(p) = self.preferences_mut() else {
            return Effect::Consumed;
        };
        let Some(spec) = p.rows().get(i).copied() else {
            return Effect::Consumed;
        };
        if let Some(value) = value {
            // The rung, the mode and the effort have one setter each, and a
            // settings row is one more route to it.
            match spec.key {
                "permissions.mode" => {
                    if let Some(rung) = crate::permissions::Rung::parse(&value) {
                        self.rung(rung, s);
                    }
                    return Effect::Consumed;
                }
                "session.mode" | "session.effort" => {
                    // The session answers the command with this same line;
                    // the sheet says it where the person is looking.
                    p.notice = match spec.key {
                        "session.mode" => crate::tui::Mode::parse(&value).map(|m| m.now()),
                        _ => crate::wire::Effort::parse(&value).map(|e| e.now()),
                    }
                    .unwrap_or_default();
                    if let Some(command) = crate::settings::live_command(spec.key, Some(&value)) {
                        return Effect::Command(command);
                    }
                    return Effect::Consumed;
                }
                _ => {}
            }
            // Full access lifts the sandbox from the next session: it is
            // confirmed first, on the sheet the Never asks rung uses.
            if spec.key == "permissions.full_access" && value == "true" {
                self.push(Source::Confirm("access".into()));
                return Effect::Consumed;
            }
            if let Err(e) = p.save(spec.key, Some(value), s) {
                p.notice = e;
            }
            if let Some(live) = self.drain_save() {
                return Effect::Command(live);
            }
        } else if spec.kind == crate::settings::Kind::Model {
            if busy {
                p.notice = "Change models after the current turn.".into();
            } else {
                self.browsing = Some(spec.key.to_string());
                self.child_of = Some("/models".into());
                return Effect::Command("/models".into());
            }
        } else if let Some((key, buffer)) = p.editing.clone()
            && key == spec.key
        {
            match p.save(&key, Some(buffer), s) {
                Ok(()) => {
                    p.editing = None;
                    if let Some(live) = self.drain_save() {
                        return Effect::Command(live);
                    }
                }
                Err(e) => p.notice = e,
            }
        } else if super::Preferences::choices(spec).is_empty() {
            p.editing = Some((spec.key.into(), p.effective(spec.key)));
        }
        Effect::Consumed
    }
}
