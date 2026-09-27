//! Input reduction for the new surface: click activates only on release.
//!
//! While a sheet is open every key, click, wheel notch and paste is the
//! sheet's (`sheet.rs` decides what each does); this module only carries out
//! the [`Outcome`] it answers with.
use super::{Action, CellTab, Layer, Outcome, Preferences, Source, Workbench};
use crate::tui::{Notebook, ScreenState, Selection};
use crossterm::event::{Event, KeyCode, KeyModifiers, MouseButton, MouseEventKind};
use ratatui::layout::Rect;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Effect {
    Insert(String),
    /// End this session and start the one with this id in its place.
    Resume(String),
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
    /// The popup's row: the screen completes it or runs it.
    Completion(usize),
    /// The popup's selection, one row down (`true`) or up.
    PopupMove(bool),
    OpenLink(String),
    /// Open the form that takes the address a browser ended on.
    PasteCallback,
    CancelSignIn,
    ReopenSignIn,
    HandlerOff(String),
}
/// A control that may change while a turn runs: a model or effort named in
/// full applies from the turn's next request (decision 9). A bare one opens
/// a sheet the session builds, and that waits for the turn.
pub fn mid_turn(command: &str) -> bool {
    let mut words = command.split_whitespace();
    match (words.next(), words.next()) {
        (Some("/effort"), Some(_)) => true,
        (Some("/model"), Some(word)) => {
            crate::spend::Tier::parse(word).is_none() || words.next().is_some()
        }
        _ => false,
    }
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
    /// Every route that changes the sandbox level. Full access is confirmed
    /// first, on a sheet that opens on Cancel; any other level is set at
    /// once, saved, and offered back.
    fn level(&mut self, level: crate::permissions::Level, s: &mut ScreenState) {
        let before = s.level.level();
        if level == crate::permissions::Level::Full && before != level {
            self.push(Source::Confirm("full".into()));
            return;
        }
        self.set_level(level, s);
    }
    /// Sets, saves and offers back: the one setter every route ends in.
    fn set_level(&mut self, level: crate::permissions::Level, s: &mut ScreenState) {
        let before = s.level.level();
        let notice = super::facts::set_level(s, level);
        if let Some(p) = self.preferences_mut() {
            p.refresh();
        }
        self.say(notice);
        if before != level {
            self.remember(super::Change {
                was: format!("Sandbox {}", before.label()),
                back: Action::Level(before.name().into()),
            });
        }
    }
    pub fn local_command(&mut self, text: &str, s: &mut ScreenState, n: &Notebook) -> bool {
        let parts: Vec<_> = text.split_whitespace().collect();
        // A level is named by its label or its file word, and a label can
        // be two words: `/sandbox full access`.
        if let ["/sandbox", rest @ ..] = parts.as_slice()
            && !rest.is_empty()
            && let Some(level) = crate::permissions::Level::parse(&rest.join(" "))
        {
            self.level(level, s);
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
                if let Some(cell) = n.last_program_cell() {
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
            // Bare /cell is the newest cell that ran.
            ["/cell"] => {
                match n.last_program_cell() {
                    Some(cell) => self.expand_cell(cell, n),
                    None => self.notice = "No cell has run yet.".into(),
                }
                true
            }
            ["/cell", number] => {
                match number.parse::<usize>() {
                    Ok(cell) if n.program_cells().any(|(ran, _)| ran == cell) => {
                        self.expand_cell(cell, n);
                    }
                    Ok(_) => self.notice = "No cell ran at that number.".into(),
                    Err(_) => self.notice = "Use /cell <number>, as in /cell 1.".into(),
                }
                true
            }
            ["/cell", ..] => {
                self.notice = "Use /cell <number>, as in /cell 1.".into();
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
            ["/sandbox"] => {
                self.open(Source::Sandbox);
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
    /// A selection starting at `anchor`: held to the region the anchor fell
    /// in, and, in the transcript, to the transcript's rows as they scroll.
    fn selection_from(&self, anchor: (u16, u16), head: &crossterm::event::MouseEvent) -> Selection {
        let g = &self.geometry;
        let inside = |r: Rect| super::contains(r, anchor.0, anchor.1);
        let region = [Some(g.transcript), g.sidebar, Some(g.composer)]
            .into_iter()
            .flatten()
            .find(|r| inside(*r));
        Selection {
            anchor,
            head: (head.column, head.row),
            region,
            top: (region == Some(g.transcript)).then_some(g.start),
        }
    }
    /// The text under the selection, and the selection put away.
    fn take_selected_text(&self, s: &mut ScreenState) -> String {
        let copied = self.selected_text(s);
        s.selection.take();
        copied
    }
    /// The text under the selection, read off the last drawn screen.
    fn selected_text(&self, s: &ScreenState) -> String {
        match (self.geometry.screen.as_ref(), s.selection) {
            (Some(screen), Some(selection)) if !selection.is_empty() => {
                let mut screen = screen.clone();
                let area = screen.area;
                crate::tui::draw_selection(&mut screen, area, selection, self.geometry.start)
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
                            let top = self.geometry.start;
                            match s.selection.as_mut() {
                                Some(selection) => selection.extend(m.column, m.row, top),
                                None => s.selection = Some(self.selection_from(anchor, m)),
                            }
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
                                if self.held_back() {
                                    return Effect::Consumed;
                                }
                                let outcome = match self.sheets.last_mut() {
                                    Some(layer) => layer.sheet.click(&hit),
                                    None => Outcome::Nothing,
                                };
                                self.apply(outcome, s, n, busy)
                            }
                            // A fold on a sheet's own strip lists what it
                            // holds over the sheet, which stays under it.
                            Some(action @ Action::More(_)) => {
                                let from_sheet = !self.sheets.is_empty();
                                self.activate(action, s, n, busy, from_sheet)
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
                        // Over the popup the wheel moves its selection; over a
                        // draft taller than its window it scrolls the draft.
                        if let Some(popup) = self.geometry.popup
                            && super::contains(popup, m.column, m.row)
                        {
                            return Effect::PopupMove(!up);
                        }
                        if super::contains(self.geometry.composer, m.column, m.row) {
                            let (above, below) = self.composer_hidden;
                            if up && above > 0 {
                                self.composer_scroll -= 1;
                                return Effect::Consumed;
                            }
                            if !up && below > 0 {
                                self.composer_scroll += 1;
                                return Effect::Consumed;
                            }
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
            Event::Paste(_) if self.held_back() => Effect::Consumed,
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
                // one, and the selection goes with it: the next Ctrl-C does
                // what it does without one.
                if ctrl && k.code == KeyCode::Char('c') {
                    let copied = self.take_selected_text(s);
                    if !copied.is_empty() {
                        return Effect::Copy(copied);
                    }
                    return Effect::Pass;
                }
                s.selection = None;
                self.notice.clear();
                if !self.sheets.is_empty() {
                    super::sheets::build(self, s, n);
                    if self.held_back() {
                        return Effect::Consumed;
                    }
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
                    // With nothing typed, Home and End go to the
                    // conversation's first and last rows.
                    KeyCode::Home | KeyCode::End
                        if s.input.is_empty() && k.modifiers.is_empty() =>
                    {
                        s.scrollback = if k.code == KeyCode::Home {
                            self.geometry
                                .rows
                                .saturating_sub(self.geometry.transcript.height as usize)
                        } else {
                            0
                        };
                        s.scrolling = k.code == KeyCode::Home;
                        Effect::Consumed
                    }
                    // With no card selected, the keys act on the newest cell
                    // that ran -- or, for helpers, that called one -- never
                    // on the entry a prose answer leaves in the notebook.
                    KeyCode::F(4) => {
                        let cell = self.selected_cell.or(n.last_program_cell()).unwrap_or(0);
                        self.activate(Action::Tab(cell, CellTab::Diff), s, n, busy, false)
                    }
                    KeyCode::F(5) => {
                        let cell = self.selected_cell.or(n.last_with_helpers()).unwrap_or(0);
                        self.activate(Action::Tab(cell, CellTab::Helpers), s, n, busy, false)
                    }
                    // Alt-↑ and Alt-↓ move the selection between cards.
                    KeyCode::Up | KeyCode::Down if k.modifiers.contains(KeyModifiers::ALT) => {
                        self.selected_cell =
                            n.next_program_cell(self.selected_cell, k.code == KeyCode::Down);
                        self.jump_cell = self.selected_cell;
                        Effect::Consumed
                    }
                    // Shift-Tab means nothing outside a form: the sandbox
                    // level is a setting, changed on its sheet, not a key
                    // one press away from Full access.
                    KeyCode::BackTab => Effect::Pass,
                    // `?` on an empty composer is the sheet of keys, as it is
                    // in the neighbouring product; with anything typed it is
                    // a question mark.
                    KeyCode::Char('?') if s.input.is_empty() && !ctrl => {
                        self.open(Source::Keys);
                        Effect::Consumed
                    }
                    KeyCode::Char('o') if ctrl => self.activate(
                        Action::Cell(self.selected_cell.or(n.last_program_cell()).unwrap_or(0)),
                        s,
                        n,
                        busy,
                        false,
                    ),
                    _ => Effect::Pass,
                }
            }
            // The cells a selection covered no longer hold its text.
            Event::Resize(..) => {
                s.selection = None;
                Effect::Pass
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
    /// Whether the top sheet is a decision that is not armed yet: the event
    /// is held back, and the sheet says so.
    fn held_back(&mut self) -> bool {
        self.sheets
            .last_mut()
            .is_some_and(|layer| layer.sheet.hold_back(std::time::Instant::now()))
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
    ///
    /// It reads the rows the composer drew, at the width it drew them and
    /// from the row its window starts at, so the caret lands under the
    /// pointer on every row of a wrapped draft.
    fn composer_click(&self, s: &ScreenState, column: u16, row: u16) -> Effect {
        let width = self.geometry.composer.width as usize;
        let lines = super::view::composer_lines(&s.input, width);
        let wanted = row.saturating_sub(self.geometry.composer.y) as usize + self.composer_scroll;
        let col = column.saturating_sub(self.geometry.composer.x) as usize;
        let Some((start, line)) = lines.get(wanted) else {
            return Effect::Cursor(s.input.len());
        };
        let mut x = 0;
        for (byte, ch) in line.char_indices() {
            let w = ratatui::text::Span::raw(ch.to_string()).width();
            if x + w > col {
                return Effect::Cursor(start + byte);
            }
            x += w;
        }
        Effect::Cursor(start + line.len())
    }
    /// Carries out what the top sheet answered.
    fn apply(&mut self, outcome: Outcome, s: &mut ScreenState, n: &Notebook, busy: bool) -> Effect {
        match outcome {
            Outcome::Nothing => Effect::Ignored,
            Outcome::Redraw => Effect::Consumed,
            // A row of the list of folded controls does what its chip would
            // have: the list goes, and the control acts from where it was.
            Outcome::Act(action) if self.showing(|source| matches!(source, Source::More(_))) => {
                self.close();
                let from_sheet = !self.sheets.is_empty();
                self.activate(action, s, n, busy, from_sheet)
            }
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
    /// Opens one card, selects it and brings it into view. The task's
    /// decision line, when there is one, is what the notice says.
    fn expand_cell(&mut self, cell: usize, n: &Notebook) {
        self.expanded.insert(cell);
        self.collapsed.remove(&cell);
        self.selected_cell = Some(cell);
        self.jump_cell = Some(cell);
        self.notice = n
            .decision
            .clone()
            .unwrap_or_else(|| format!("Cell {cell} expanded · F4 opens its diff"));
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
            Action::Completion(index) => return Effect::Completion(index),
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
            Action::HelperRaw(cell, h) => {
                self.helper_raw = if self.helper_raw == Some((cell, h)) {
                    None
                } else {
                    Some((cell, h))
                };
            }
            Action::Latest => s.scrollback = 0,
            Action::More(controls) => self.show(Source::More(controls), from_sheet),
            Action::Resume(id) => {
                self.close_all();
                return Effect::Resume(id);
            }
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
                // The picker is built by the session, which is in the turn.
                if busy {
                    self.say(super::voice::BETWEEN_TURNS);
                } else {
                    if from_sheet {
                        self.child_of = Some("/models".into());
                    } else {
                        self.close_all();
                    }
                    return Effect::Command("/models".into());
                }
            }
            Action::Sandbox => self.show(Source::Sandbox, from_sheet),
            Action::Hosts => self.show(
                Source::Hosts(Box::new(super::hosts::HostsSheet::open(s))),
                from_sheet,
            ),
            Action::Ecosystem(name) => {
                if let Some(h) = self.hosts_mut() {
                    let notice = h.toggle(&name, s);
                    self.say(notice);
                }
            }
            Action::RemoveHost(host) => {
                if let Some(h) = self.hosts_mut() {
                    let notice = h.remove(&host, s);
                    self.say(notice);
                }
            }
            Action::AddHost => {
                if let Some(h) = self.hosts_mut() {
                    let notice = h.add(s);
                    self.say(notice);
                }
            }
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
                let field = self.sheets.last().and_then(|layer| {
                    match layer.sheet.items.get(i).map(|item| &item.kind) {
                        Some(super::ItemKind::Field(field)) => Some(field.clone()),
                        _ => None,
                    }
                });
                if let (Some(field), Some(h)) = (field.clone(), self.hosts_mut()) {
                    h.draft = field;
                }
                let text = field.map(|field| field.text);
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
                            // Saved, and applied now like every setting whose
                            // row says it applies now.
                            if let Some(live) = self.drain_save() {
                                return Effect::Command(live);
                            }
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
                // A model applies from the turn's next request; a favourite
                // slot waits for the turn to end.
                if busy && !mid_turn(&command) {
                    self.say(super::voice::BETWEEN_TURNS);
                    return Effect::Consumed;
                }
                let notice = m.chosen(&model);
                self.say(if busy {
                    format!("{notice} · {}", super::voice::NEXT_REQUEST)
                } else {
                    notice
                });
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
                // A confirmation's Yes, or a row that answers a panel whose
                // leaving is itself an answer (the rollback preview): the
                // sheet that asked goes first, and nothing else is sent for
                // it.
                if matches!(
                    self.sheets.last().map(|l| &l.source),
                    Some(Source::Confirm(_))
                ) || matches!(
                    self.sheets.last().map(|l| &l.source),
                    Some(Source::Panel(panel)) if panel.back.is_some()
                ) {
                    self.sheets.pop();
                    self.child_of = None;
                }
                // A picker row's command moves the picker's marks at once,
                // as a chosen model does.
                if (!busy || mid_turn(&cmd))
                    && matches!(
                        self.sheets.last().map(|l| &l.source),
                        Some(Source::Models(_))
                    )
                    && let Some(notice) = self.models_mut().and_then(|m| m.sent(&cmd))
                {
                    self.say(notice);
                }
                if busy && mid_turn(&cmd) {
                    return Effect::Command(cmd);
                } else if busy {
                    self.say(super::voice::BETWEEN_TURNS);
                } else {
                    if from_sheet {
                        self.child_of = Some(cmd.clone());
                    } else {
                        self.close_all();
                    }
                    return Effect::Command(cmd);
                }
            }
            Action::Level(word) => {
                if let Some(level) = crate::permissions::Level::parse(&word) {
                    self.level(level, s);
                }
            }
            Action::ConfirmLevel(word) => {
                if let Some(level) = crate::permissions::Level::parse(&word) {
                    if matches!(
                        self.sheets.last().map(|l| &l.source),
                        Some(Source::Confirm(_))
                    ) {
                        self.sheets.pop();
                    }
                    self.set_level(level, s);
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
            // The level and the effort have one setter each, and a settings
            // row is one more route to it.
            match spec.key {
                "sandbox.level" => {
                    if let Some(level) = crate::permissions::Level::parse(&value) {
                        self.level(level, s);
                    }
                    return Effect::Consumed;
                }
                "session.effort" => {
                    // The session answers the command with this same line;
                    // the sheet says it where the person is looking.
                    p.notice = crate::wire::Effort::parse(&value)
                        .map(|e| e.now())
                        .unwrap_or_default();
                    if let Some(command) = crate::settings::live_command(spec.key, Some(&value)) {
                        return Effect::Command(command);
                    }
                    return Effect::Consumed;
                }
                _ => {}
            }
            // Pinned and favourites name a model. Chosen with none named,
            // each opens where one is named -- the picker for the pinned
            // model, the favourites for a slot -- instead of saving a rule
            // the settings file refuses.
            if spec.key == "agents.mode" {
                let agents = &p.loaded.config.agents;
                let opens = match value.as_str() {
                    "pinned" if agents.model.is_none() => Some("/models"),
                    "roster" if agents.slots.is_empty() => Some("/subagents"),
                    _ => None,
                };
                if let Some(command) = opens {
                    if busy {
                        p.notice = super::voice::BETWEEN_TURNS.into();
                        return Effect::Consumed;
                    }
                    if command == "/models" {
                        self.browsing = Some("agents.model".into());
                    }
                    self.child_of = Some(command.into());
                    return Effect::Command(command.into());
                }
            }
            if let Err(e) = p.save(spec.key, Some(value), s) {
                p.notice = e;
            }
            if let Some(live) = self.drain_save() {
                return Effect::Command(live);
            }
        } else if spec.kind == crate::settings::Kind::Model {
            if busy {
                p.notice = super::voice::BETWEEN_TURNS.into();
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
