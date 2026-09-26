//! Input reduction for the new surface: click activates only on release.
use super::{Action, CellTab, Workbench};
use crate::tui::{Notebook, ScreenState, Selection};
use crossterm::event::{Event, KeyCode, KeyModifiers, MouseButton, MouseEventKind};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Effect {
    Insert(String),
    OpenPath(String),
    Pass,
    Consumed,
    Command(String),
    Copy(String),
    Cursor(usize),
}
impl Workbench {
    /// Writes one presentation key to the project's own settings, and says
    /// whether the file actually took it. A session with no project root is
    /// not an error here -- the choice still applies to the running screen,
    /// and the notice says which of the two happened.
    fn persist(&mut self, key: &str, value: &str, s: &mut ScreenState) -> bool {
        let Ok(mut p) = super::Preferences::open(s) else {
            return false;
        };
        let saved = p.save(key, Some(value.to_string()), s).is_ok();
        if saved {
            self.notice = p.notice.clone();
        }
        saved
    }
    pub fn local_command(&mut self, text: &str, s: &mut ScreenState, n: &Notebook) -> bool {
        let parts: Vec<_> = text.split_whitespace().collect();
        match parts.as_slice() {
            ["/settings"] => {
                self.open_settings(s);
                true
            }
            // A command that names one setting opens where that setting is.
            // Bare `/statusline` used to land on the everyday category with
            // the status line nowhere in sight.
            ["/statusline"] => {
                self.open_settings(s);
                if let Some(p) = &mut self.preferences {
                    p.category = 1;
                    p.selected = p
                        .rows()
                        .iter()
                        .position(|spec| spec.key == "ui.statusline")
                        .unwrap_or(0);
                }
                true
            }
            ["/config"] => {
                self.open_settings(s);
                if let Some(p) = &mut self.preferences {
                    p.category = 5;
                }
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
                self.close();
                s.inspection = None;
                s.telemetry_open = false;
                true
            }
            ["/activity"] => {
                self.close();
                s.telemetry_open = false;
                self.activity = true;
                true
            }
            ["/telemetry"] => {
                self.close();
                s.telemetry_open = true;
                true
            }
            ["/mode"] => {
                self.close();
                self.work = true;
                true
            }
            ["/permissions"] => {
                self.close();
                self.approvals = true;
                true
            }
            // Presentation is this layer's own business: a palette, a
            // status line, a sidebar and motion never reach the model, and
            // each one says what it did where a person can scroll back to it.
            ["/theme"] => {
                self.close();
                s.notice = None;
                s.panel = Some(crate::tui::Theme::picker(s.theme));
                true
            }
            ["/theme", name] => {
                match crate::tui::Theme::parse(name) {
                    Some(theme) => {
                        s.theme = theme;
                        s.panel = None;
                        self.persist("ui.theme", theme.name(), s);
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
                s.set_motion(motion);
                // The older switch is kept in step, or a saved `true` would
                // hold every later level at off.
                let off = (motion == crate::tui::Motion::Off).to_string();
                self.persist("ui.reduced_motion", &off, s);
                self.persist("ui.motion", motion.name(), s);
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
                s.stream = crate::tui::Stream::parse(word).unwrap_or_default();
                self.persist("ui.stream", word, s);
                s.note(format!("Streaming cell: {word} · /stream actions|code|raw"));
                true
            }
            ["/stream"] => {
                s.note("Usage: /stream code | quiet | raw");
                true
            }
            ["/sidebar", word @ ("auto" | "show" | "hide")] => {
                s.sidebar = match *word {
                    "show" => crate::tui::SidebarVisibility::Shown,
                    "hide" => crate::tui::SidebarVisibility::Hidden,
                    _ => crate::tui::SidebarVisibility::Auto,
                };
                self.persist("ui.sidebar", word, s);
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
                s.status_line = match word {
                    "compact" => crate::tui::StatusLine::Compact,
                    "hidden" => crate::tui::StatusLine::Hidden,
                    _ => crate::tui::StatusLine::Full,
                };
                if self.persist("ui.statusline", word, s) {
                    s.note(format!("Status line saved for this project: {word}"));
                } else {
                    s.note(format!("Status line: {word} · this session only"));
                }
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
    pub fn event(&mut self, e: &Event, s: &mut ScreenState, n: &Notebook, busy: bool) -> Effect {
        match e {
            Event::Mouse(m) => {
                if s.mouse_off {
                    return Effect::Consumed;
                }
                match m.kind {
                    MouseEventKind::Down(MouseButton::Left) => {
                        self.press = Some((m.column, m.row));
                        self.dragged = false;
                        s.selection = None;
                        Effect::Consumed
                    }
                    MouseEventKind::Drag(MouseButton::Left) => {
                        if let Some(anchor) = self.press {
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
                        if let Some(action) = self.geometry.hit(m.column, m.row) {
                            if action == Action::Composer {
                                let width = self.geometry.composer.width as usize;
                                let lines = super::view::wrap_input(&s.input, width);
                                let cursor = s.cursor.unwrap_or(s.input.len()).min(s.input.len());
                                let before = &s.input[..s.input.floor_char_boundary(cursor)];
                                let cr = super::view::wrap_input(before, width)
                                    .len()
                                    .saturating_sub(1);
                                let skip = cr.saturating_sub(
                                    self.geometry.composer.height.saturating_sub(1) as usize,
                                );
                                let wanted =
                                    m.row.saturating_sub(self.geometry.composer.y) as usize + skip;
                                let col =
                                    m.column.saturating_sub(self.geometry.composer.x) as usize;
                                let mut offset = 0;
                                for (i, line) in lines.iter().enumerate() {
                                    if i == wanted {
                                        let mut x = 0;
                                        for (byte, ch) in line.char_indices() {
                                            let w =
                                                ratatui::text::Span::raw(ch.to_string()).width();
                                            if x + w > col {
                                                return Effect::Cursor(offset + byte);
                                            }
                                            x += w;
                                        }
                                        return Effect::Cursor(
                                            (offset + line.len()).min(s.input.len()),
                                        );
                                    }
                                    offset += line.len();
                                    if s.input.as_bytes().get(offset) == Some(&b'\n') {
                                        offset += 1;
                                    }
                                }
                                return Effect::Cursor(s.input.len());
                            }
                            return self.activate(action, s, n, busy);
                        }
                        Effect::Consumed
                    }
                    MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => {
                        let up = m.kind == MouseEventKind::ScrollUp;
                        if let Some(p) = &mut self.preferences {
                            p.selected = move_index(p.selected, up, 3, p.rows().len());
                        } else if let Some(m) = &mut self.models {
                            m.selected = move_index(m.selected, up, 3, m.candidates().len());
                        } else if let Some(p) = &mut s.panel {
                            p.selected = move_index(p.selected, up, 3, p.rows.len());
                        } else if self.is_local() {
                            self.local_scroll = if up {
                                self.local_scroll.saturating_sub(3)
                            } else {
                                self.local_scroll.saturating_add(3)
                            };
                        } else {
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
                        }
                        s.selection = None;
                        Effect::Consumed
                    }
                    _ => Effect::Consumed,
                }
            }
            Event::Paste(text) => {
                if let Some(p) = &mut self.preferences {
                    if let Some((_, v)) = &mut p.editing {
                        v.push_str(&text.chars().filter(|c| !c.is_control()).collect::<String>());
                    } else {
                        p.query.push_str(text);
                        p.selected = 0;
                    }
                    return Effect::Consumed;
                }
                if let Some(m) = &mut self.models {
                    m.query.push_str(text);
                    m.selected = 0;
                    return Effect::Consumed;
                }
                if self.is_local() || s.panel.is_some() {
                    Effect::Consumed
                } else {
                    Effect::Pass
                }
            }
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
                }
                s.selection = None;
                self.notice.clear();
                if ctrl && k.code == KeyCode::Char('c') {
                    self.close();
                    return Effect::Pass;
                }
                // The instruments are a surface too: Esc leaves them, and
                // ↑↓ walks the requests they list.
                if s.telemetry_open && !self.is_local() && s.panel.is_none() {
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
                if k.code == KeyCode::Esc && (self.is_local() || s.panel.is_some()) {
                    if let Some(p) = &mut self.preferences {
                        if p.editing.take().is_some() {
                            return Effect::Consumed;
                        }
                        if !p.query.is_empty() {
                            p.query.clear();
                            p.selected = 0;
                            return Effect::Consumed;
                        }
                    }
                    self.close();
                    if let Some((preferences, _)) = self.model_preference.take() {
                        self.preferences = Some(preferences);
                    }
                    s.panel = None;
                    s.selection = None;
                    return Effect::Consumed;
                }
                if let Some(p) = &mut self.preferences {
                    if let Some((key, buffer)) = &mut p.editing {
                        match k.code {
                            KeyCode::Enter => {
                                let (key, value) = (key.clone(), buffer.clone());
                                match p.save(&key, Some(value), s) {
                                    Ok(()) => {
                                        p.editing = None;
                                        if let Some(live) = p.take_live() {
                                            return Effect::Command(live);
                                        }
                                    }
                                    Err(e) => p.notice = e,
                                }
                            }
                            KeyCode::Backspace => {
                                buffer.pop();
                            }
                            KeyCode::Char('u') if ctrl => buffer.clear(),
                            KeyCode::Char(c) if !ctrl => buffer.push(c),
                            _ => {}
                        }
                        return Effect::Consumed;
                    }
                    let spec = p.rows().get(p.selected).copied();
                    let result = match k.code {
                        KeyCode::Up => {
                            p.selected = p.selected.saturating_sub(1);
                            Ok(())
                        }
                        KeyCode::Down => {
                            p.selected = (p.selected + 1).min(p.rows().len().saturating_sub(1));
                            Ok(())
                        }
                        KeyCode::PageUp => {
                            p.selected = p.selected.saturating_sub(8);
                            Ok(())
                        }
                        KeyCode::PageDown => {
                            p.selected = (p.selected + 8).min(p.rows().len().saturating_sub(1));
                            Ok(())
                        }
                        KeyCode::Tab => {
                            p.category = (p.category + 1) % 6;
                            p.query.clear();
                            p.selected = 0;
                            Ok(())
                        }
                        KeyCode::BackTab => {
                            p.category = (p.category + 5) % 6;
                            p.query.clear();
                            p.selected = 0;
                            Ok(())
                        }
                        KeyCode::F(6) => p.switch_scope(),
                        KeyCode::Char('z') if ctrl => p.undo(s),
                        KeyCode::Backspace => {
                            if !p.query.is_empty() {
                                p.query.pop();
                                p.selected = 0;
                                Ok(())
                            } else if let Some(spec) = spec {
                                p.save(spec.key, None, s)
                            } else {
                                Ok(())
                            }
                        }
                        KeyCode::Left | KeyCode::Right => {
                            if spec.is_some_and(|s| s.kind == crate::settings::Kind::Model) {
                                if !busy {
                                    self.browse_preference(spec.expect("model row").key);
                                    return Effect::Command("/models".into());
                                }
                                Ok(())
                            } else {
                                p.cycle(k.code == KeyCode::Right, s)
                            }
                        }
                        KeyCode::Enter => {
                            if let Some(spec) = spec {
                                if spec.kind == crate::settings::Kind::Model {
                                    if busy {
                                        p.notice =
                                            "Open model selection after the current turn.".into();
                                        return Effect::Consumed;
                                    }
                                    self.browse_preference(spec.key);
                                    return Effect::Command("/models".into());
                                } else if super::Preferences::choices(spec).is_empty() {
                                    p.editing = Some((spec.key.into(), p.effective(spec.key)));
                                    Ok(())
                                } else {
                                    p.cycle(true, s)
                                }
                            } else {
                                Ok(())
                            }
                        }
                        KeyCode::Char(c) if !ctrl => {
                            p.query.push(c);
                            p.selected = 0;
                            Ok(())
                        }
                        _ => Ok(()),
                    };
                    if let Err(e) = result {
                        p.notice = e;
                    }
                    // **The one drain for every keyboard route into a save.**
                    // Arrow, Enter, Backspace-to-inherit and Ctrl-Z all land
                    // here, so the control a save owes the running session is
                    // taken once, in the place they converge, rather than at
                    // each of the four.
                    if let Some(live) = p.take_live() {
                        return Effect::Command(live);
                    }
                    return Effect::Consumed;
                }
                if let Some(m) = &mut self.models {
                    match k.code {
                        KeyCode::Up => m.selected = m.selected.saturating_sub(1),
                        KeyCode::Down => {
                            m.selected =
                                (m.selected + 1).min(m.candidates().len().saturating_sub(1))
                        }
                        KeyCode::PageUp => m.selected = m.selected.saturating_sub(10),
                        KeyCode::PageDown => {
                            m.selected =
                                (m.selected + 10).min(m.candidates().len().saturating_sub(1))
                        }
                        KeyCode::Tab if m.target_key.is_none() => {
                            m.role = (m.role + 1) % 3;
                            m.selected = 0;
                        }
                        // ←→ steps through the favourite slots, wrapping, on
                        // the Subagents tab; elsewhere the list is one list.
                        KeyCode::Left | KeyCode::Right if m.role == 2 && m.target_key.is_none() => {
                            let slots: Vec<Option<&str>> = std::iter::once(None)
                                .chain(crate::config::SLOT_NAMES.iter().copied().map(Some))
                                .collect();
                            let at = slots
                                .iter()
                                .position(|slot| *slot == m.slot.as_deref())
                                .unwrap_or(0);
                            let n = slots.len();
                            let next = if k.code == KeyCode::Right {
                                (at + 1) % n
                            } else {
                                (at + n - 1) % n
                            };
                            m.slot = slots[next].map(str::to_owned);
                            m.select_current();
                        }
                        KeyCode::Char('o') if ctrl => m.measured_order = !m.measured_order,
                        KeyCode::Char('u') if ctrl => {
                            m.query.clear();
                            m.selected = 0;
                        }
                        KeyCode::Char('a') if ctrl => {
                            m.all_sources = !m.all_sources;
                            m.selected = 0;
                        }
                        KeyCode::Enter => return self.activate(Action::ChooseModel, s, n, busy),
                        KeyCode::Backspace => {
                            m.query.pop();
                            m.selected = 0;
                        }
                        KeyCode::Char(c) if !ctrl => {
                            m.query.push(c);
                            m.selected = 0;
                        }
                        _ => {}
                    }
                    return Effect::Consumed;
                }
                if self.confirm.is_some() {
                    if k.code == KeyCode::Enter {
                        let rung = self.confirm.clone().unwrap_or_default();
                        return self.activate(Action::Rung(rung), s, n, busy);
                    }
                    return Effect::Consumed;
                }
                if self.work || self.approvals {
                    let count = if self.work { 3 } else { 4 };
                    match k.code {
                        KeyCode::Up => self.local_scroll = self.local_scroll.saturating_sub(1),
                        KeyCode::Down => self.local_scroll = (self.local_scroll + 1).min(count - 1),
                        KeyCode::Enter => {
                            let action = if self.work {
                                Action::Command(format!(
                                    "/mode {}",
                                    ["execute", "explore", "plan"][self.local_scroll.min(2)]
                                ))
                            } else {
                                Action::Rung(
                                    ["manual", "accept-edits", "auto", "full"]
                                        [self.local_scroll.min(3)]
                                    .into(),
                                )
                            };
                            return self.activate(action, s, n, busy);
                        }
                        _ => {}
                    }
                    return Effect::Consumed;
                }
                if self.activity || self.access {
                    match k.code {
                        KeyCode::Up | KeyCode::PageUp => {
                            self.local_scroll = self.local_scroll.saturating_sub(3)
                        }
                        KeyCode::Down | KeyCode::PageDown => self.local_scroll += 3,
                        _ => {}
                    }
                    return Effect::Consumed;
                }
                if let Some(p) = &mut s.panel {
                    match k.code {
                        KeyCode::Up => p.selected = p.selected.saturating_sub(1),
                        KeyCode::Down => {
                            p.selected = (p.selected + 1).min(p.rows.len().saturating_sub(1))
                        }
                        KeyCode::PageUp => p.selected = p.selected.saturating_sub(10),
                        KeyCode::PageDown => {
                            p.selected = (p.selected + 10).min(p.rows.len().saturating_sub(1))
                        }
                        KeyCode::Enter => {
                            if let Some(cmd) = Self::panel_command(p) {
                                return Effect::Command(cmd);
                            }
                        }
                        _ => {}
                    }
                    return Effect::Consumed;
                }
                match k.code {
                    KeyCode::Char('t') if ctrl => {
                        self.close();
                        s.telemetry_open = !s.telemetry_open;
                        Effect::Consumed
                    }
                    KeyCode::F(2) => {
                        self.open_settings(s);
                        Effect::Consumed
                    }
                    KeyCode::F(3) => self.activate(Action::Models, s, n, busy),
                    KeyCode::F(4) => {
                        let cell = self.selected_cell.unwrap_or(n.cells.len());
                        self.activate(Action::Tab(cell, CellTab::Diff), s, n, busy)
                    }
                    KeyCode::F(5) => {
                        let cell = self.selected_cell.unwrap_or(n.cells.len());
                        self.activate(Action::Tab(cell, CellTab::Helpers), s, n, busy)
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
                        self.close();
                        self.help = true;
                        Effect::Consumed
                    }
                    KeyCode::Char('o') if ctrl => self.activate(
                        Action::Cell(self.selected_cell.unwrap_or(n.cells.len())),
                        s,
                        n,
                        busy,
                    ),
                    _ => Effect::Pass,
                }
            }
            _ => Effect::Pass,
        }
    }
    fn activate(
        &mut self,
        action: Action,
        s: &mut ScreenState,
        n: &Notebook,
        busy: bool,
    ) -> Effect {
        match action {
            Action::Insert(command) => return Effect::Insert(command),
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
            Action::Settings => self.open_settings(s),
            Action::SettingsAt(category) => {
                self.open_settings(s);
                if let Some(p) = &mut self.preferences {
                    p.category = category.min(super::settings::CATEGORIES.len() - 1);
                    p.selected = 0;
                }
            }
            // **The whole point of the strip is that it acts where it
            // stands.** Stepping the effort opens nothing: the word on the
            // strip is the next word before the finger has left the mouse,
            // because `/effort` was always a live control and this is it.
            Action::Effort => {
                const LADDER: [&str; 6] = ["default", "low", "medium", "high", "xhigh", "max"];
                let here = LADDER
                    .iter()
                    .position(|w| *w == s.effort.name())
                    .unwrap_or(0);
                // What it was is offered back beside the notice the step
                // produces: reversibility over confirmation.
                self.undo = Some((
                    format!("effort {}", s.effort.name()),
                    format!("/effort {}", s.effort.name()),
                ));
                return Effect::Command(format!("/effort {}", LADDER[(here + 1) % LADDER.len()]));
            }
            Action::UndoLive => {
                if let Some((_, command)) = self.undo.take() {
                    self.notice.clear();
                    return Effect::Command(command);
                }
            }
            Action::Help => {
                self.close();
                self.help = true;
            }
            // The dock's fourth chip steps in place, like the effort one.
            Action::Stream => {
                s.stream = s.stream.next();
                let word = s.stream.name();
                self.persist("ui.stream", word, s);
                self.notice = format!(
                    "streaming cell: {word} — {}",
                    match s.stream {
                        crate::tui::Stream::Actions => "each action with its size as it arrives",
                        crate::tui::Stream::Code => "the program as it forms",
                        crate::tui::Stream::Raw => "the raw protocol text",
                    }
                );
            }
            Action::Quip => {
                self.quips += 1;
                self.notice = if s.speaking().playful() {
                    super::voice::quip(self.quips).to_string()
                } else {
                    "Pane · click any chip to change what it names".to_string()
                };
            }
            Action::Models => {
                if busy {
                    self.notice =
                        "Change models after the current turn; in-flight calls retain their model."
                            .into();
                } else {
                    self.close();
                    return Effect::Command("/models".into());
                }
            }
            Action::Work => {
                self.close();
                self.work = true;
            }
            Action::Approvals => {
                self.close();
                self.approvals = true;
            }
            Action::Access => {
                self.close();
                self.access = true;
            }
            Action::Activity => {
                self.close();
                self.activity = true;
            }
            Action::Close => {
                self.close();
                if let Some((p, _)) = self.model_preference.take() {
                    self.preferences = Some(p);
                }
                s.panel = None;
                s.selection = None;
            }
            Action::Scope(global) => {
                let wanted = if global {
                    crate::settings::Scope::Global
                } else {
                    crate::settings::Scope::Local
                };
                if let Some(p) = &mut self.preferences
                    && p.scope != wanted
                    && let Err(e) = p.switch_scope()
                {
                    p.notice = e;
                }
            }
            Action::Undo => {
                if let Some(p) = &mut self.preferences {
                    if let Err(e) = p.undo(s) {
                        p.notice = e;
                    }
                    if let Some(live) = p.take_live() {
                        return Effect::Command(live);
                    }
                }
            }
            Action::Category(c) => {
                if let Some(p) = &mut self.preferences {
                    p.category = c.min(5);
                    p.selected = 0;
                    p.query.clear();
                }
            }
            Action::Setting(i, value) => {
                if let Some(p) = &mut self.preferences {
                    p.selected = i;
                    let spec = p.rows().get(i).copied();
                    if let Some(spec) = spec {
                        if let Some(value) = value {
                            if spec.key.starts_with("permissions.") || spec.key == "agents.mode" {
                                p.editing = Some((spec.key.into(), value));
                            } else {
                                if let Err(e) = p.save(spec.key, Some(value), s) {
                                    p.notice = e;
                                }
                                if let Some(live) = p.take_live() {
                                    return Effect::Command(live);
                                }
                            }
                        } else if spec.kind == crate::settings::Kind::Model {
                            if !busy {
                                self.browse_preference(spec.key);
                                return Effect::Command("/models".into());
                            } else {
                                p.notice = "Change models after the current turn.".into();
                            }
                        } else if super::Preferences::choices(spec).is_empty() {
                            p.editing = Some((spec.key.into(), p.effective(spec.key)));
                        }
                    }
                }
            }
            Action::Slot(slot) => {
                if let Some(m) = &mut self.models
                    && m.target_key.is_none()
                {
                    m.slot = slot;
                    m.select_current();
                }
            }
            Action::ModelRole(r) => {
                if let Some(m) = &mut self.models
                    && m.target_key.is_none()
                {
                    m.role = r.min(2);
                    m.selected = 0;
                }
            }
            Action::Model(i) => {
                if let Some(m) = &mut self.models {
                    m.selected = i;
                }
            }
            Action::UnsetModel => {
                if let Some((mut p, key)) = self.model_preference.take() {
                    if let Err(e) = p.save(&key, None, s) {
                        p.notice = e;
                    }
                    let live = p.take_live();
                    self.close();
                    self.preferences = Some(p);
                    if let Some(live) = live {
                        return Effect::Command(live);
                    }
                }
            }
            Action::ChooseModel => {
                if let Some(m) = &mut self.models {
                    if busy {
                        m.notice = "The current turn must finish before changing models.".into();
                    } else {
                        match m.choose() {
                            Ok(cmd) => {
                                if let Some((mut preferences, key)) = self.model_preference.take() {
                                    let selected =
                                        m.candidates().get(m.selected).map(|c| c.model.clone());
                                    if let Some(selected) = selected
                                        && let Err(error) =
                                            preferences.save(&key, Some(selected), s)
                                    {
                                        preferences.notice = error;
                                    }
                                    self.close();
                                    self.preferences = Some(preferences);
                                } else {
                                    self.close();
                                    return Effect::Command(cmd);
                                }
                            }
                            Err(e) => m.notice = e,
                        }
                    }
                }
            }
            Action::Sources => {
                if let Some(m) = &mut self.models {
                    m.all_sources = !m.all_sources;
                    m.selected = 0;
                }
            }
            Action::Scores => {
                if let Some(m) = &mut self.models {
                    m.measured_order = !m.measured_order;
                    m.selected = 0;
                }
            }
            Action::Command(cmd) => {
                if busy {
                    self.notice="This runtime change applies between turns; finish or stop the current turn first.".into();
                } else {
                    self.close();
                    return Effect::Command(cmd);
                }
            }
            Action::Rung(rung) => {
                if rung == "full" && self.confirm.as_deref() != Some("full") {
                    self.close();
                    self.confirm = Some(rung);
                } else if let Some(r) = crate::permissions::Rung::parse(&rung) {
                    s.permissions.set(r);
                    self.close();
                    self.notice = format!(
                        "permissions {rung} — Shift-Tab cycles, /permissions <rung> sets one"
                    );
                }
            }
            Action::PanelRow(i) => {
                if let Some(p) = &mut s.panel {
                    p.selected = i;
                    if let Some(cmd) = Self::panel_command(p) {
                        return Effect::Command(cmd);
                    }
                }
            }
            Action::Composer => {}
        }
        Effect::Consumed
    }
}
fn move_index(i: usize, up: bool, step: usize, len: usize) -> usize {
    if up {
        i.saturating_sub(step)
    } else {
        i.saturating_add(step).min(len.saturating_sub(1))
    }
}
