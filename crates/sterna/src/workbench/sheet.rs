//! One sheet: every picker, form, settings page, wizard step, info page and
//! decision prompt is built from this, and each answers the keys and the
//! mouse the same way (`docs/audit/tui-audit.md`, "One interaction model").
//!
//! **The grammar lives here and nowhere else.** A surface says what its rows
//! are -- a choice, a row of values, a switch, a way somewhere, a one-shot
//! action, a dangerous one, a field, or plain text -- and this module decides
//! what a key, a click, the wheel and hover do to them, what the foot's hint
//! says, and what shows when something does not fit. A surface that needs a
//! different answer to one of those questions is a surface that has left the
//! interaction model.
use super::{Action, Geometry, Tone, chrome, document::clip, theme};
use crate::tui::Theme;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{Frame, layout::Rect};
use std::time::{Duration, Instant};

/// How long a decision must have been on screen, with no key pressed, before
/// a key can answer it: a sentence typed as it appears answers nothing.
pub const ARMING: Duration = Duration::from_millis(500);

/// What one row is. Every row is exactly one kind.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Kind {
    /// Plain text. Not focusable; focus skips it.
    Info,
    /// A group's name. Not focusable.
    Heading,
    /// One choice of a setting, as a whole row: a click applies it and the
    /// sheet stays open. The current one is marked `● now`.
    Choice { current: bool },
    /// A setting's values as chips in one row; the current one is filled and
    /// marked `●`. ←→ step through them, and each step applies.
    Value {
        values: Vec<(String, Action)>,
        current: Option<usize>,
    },
    /// On or off; a click flips it.
    Toggle(bool),
    /// Goes somewhere: another sheet, a step, a longer list. `title ›`.
    Open,
    /// A one-shot action: `⏎ title`.
    Run,
    /// An action that cannot be taken back; it opens a confirmation that
    /// starts on Cancel.
    Danger,
    /// A text field in a form.
    Field(Field),
}

/// A text field's contents and caret.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Field {
    pub text: String,
    /// Byte offset of the caret in `text`.
    pub cursor: usize,
    /// Drawn as bullets, never as the text.
    pub secret: bool,
    /// The value the field opened with, all selected: the first key or
    /// paste replaces it, and an arrow key keeps it.
    pub fresh: bool,
    /// A list: pasted lines become entries rather than being run together.
    pub list: bool,
}

/// One row of a sheet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Item {
    /// Stable across rebuilds: focus is kept by id, not by index.
    pub id: String,
    pub title: String,
    /// The line under the title, muted; a disabled row's reason goes here.
    pub detail: String,
    pub kind: Kind,
    pub action: Option<Action>,
    /// Why this row cannot be used now. It stays visible and muted, and
    /// activating it puts the reason in the notice and does nothing else.
    pub disabled: Option<String>,
    /// A letter or digit that acts on this row from anywhere on the sheet,
    /// printed on it. Decision prompts use these.
    pub key: Option<char>,
    /// Drawn as a chip beside the inline rows next to it, rather than as a
    /// line of its own: the answers of a decision prompt, a pair of buttons.
    pub inline: bool,
    /// The tone of an Info row's text.
    pub tone: Tone,
    /// A swatch drawn before the title, part of the row's target: a theme's
    /// accent, or the terminal's own ink for one with none (mono), so every
    /// name in the list starts in one column.
    pub swatch: Option<Tone>,
}

impl Item {
    fn new(id: impl Into<String>, title: impl Into<String>, kind: Kind) -> Self {
        Self {
            id: id.into(),
            title: title.into(),
            detail: String::new(),
            kind,
            action: None,
            disabled: None,
            key: None,
            inline: false,
            tone: Tone::Normal,
            swatch: None,
        }
    }
    /// Text that is read, never chosen.
    pub fn info(text: impl Into<String>) -> Self {
        let text = text.into();
        Self::new(format!("info:{text}"), text, Kind::Info)
    }
    pub fn heading(text: impl Into<String>) -> Self {
        let text = text.into();
        Self::new(format!("heading:{text}"), text, Kind::Heading)
    }
    pub fn choice(
        id: impl Into<String>,
        title: impl Into<String>,
        current: bool,
        action: Action,
    ) -> Self {
        Self::new(id, title, Kind::Choice { current }).act(action)
    }
    pub fn value(
        id: impl Into<String>,
        title: impl Into<String>,
        values: Vec<(String, Action)>,
        current: Option<usize>,
    ) -> Self {
        Self::new(id, title, Kind::Value { values, current })
    }
    pub fn toggle(
        id: impl Into<String>,
        title: impl Into<String>,
        on: bool,
        action: Action,
    ) -> Self {
        Self::new(id, title, Kind::Toggle(on)).act(action)
    }
    pub fn open(id: impl Into<String>, title: impl Into<String>, action: Action) -> Self {
        Self::new(id, title, Kind::Open).act(action)
    }
    pub fn run(id: impl Into<String>, title: impl Into<String>, action: Action) -> Self {
        Self::new(id, title, Kind::Run).act(action)
    }
    pub fn danger(id: impl Into<String>, title: impl Into<String>, action: Action) -> Self {
        Self::new(id, title, Kind::Danger).act(action)
    }
    pub fn field(id: impl Into<String>, title: impl Into<String>, field: Field) -> Self {
        Self::new(id, title, Kind::Field(field))
    }
    #[must_use]
    pub fn act(mut self, action: Action) -> Self {
        self.action = Some(action);
        self
    }
    #[must_use]
    pub fn detail(mut self, detail: impl Into<String>) -> Self {
        self.detail = detail.into();
        self
    }
    #[must_use]
    pub fn disabled(mut self, reason: Option<String>) -> Self {
        self.disabled = reason;
        self
    }
    #[must_use]
    pub fn key(mut self, key: char) -> Self {
        self.key = Some(key);
        self
    }
    #[must_use]
    pub fn inline(mut self) -> Self {
        self.inline = true;
        self
    }
    #[must_use]
    pub fn swatch(mut self, rgb: Option<u32>) -> Self {
        self.swatch = Some(rgb.map_or(Tone::Normal, |rgb| Tone::Pixel(Some(rgb), None)));
        self
    }
    #[must_use]
    pub fn tone(mut self, tone: Tone) -> Self {
        self.tone = tone;
        self
    }
    /// Whether focus may rest here.
    pub fn focusable(&self) -> bool {
        !matches!(self.kind, Kind::Info | Kind::Heading)
    }
    /// Whether this row is the value the session is on. A Value row holds
    /// its current value inside it, so it is never "the current row" of a
    /// list the way one choice among several is.
    pub fn is_current(&self) -> bool {
        matches!(self.kind, Kind::Choice { current: true })
    }
}

/// What a key or a click on a sheet asks its owner to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// Nothing happened.
    Nothing,
    /// Only the sheet's own state moved (focus, scroll, the search): draw.
    Redraw,
    /// Do this, then keep the sheet open unless the action says otherwise.
    Act(Action),
    /// Esc with nothing left to undo on this sheet: go back one layer.
    Back,
    /// A section chip was chosen; the owner rebuilds the rows.
    Section(usize),
    /// Ctrl-Z: take back the last change, wherever it was made.
    Undo,
    /// `⟨ +N ▾ ⟩` on a Value row: open the whole list of its values.
    Fold(usize),
}

/// One sheet's state. Its rows are rebuilt from their owner's data on every
/// frame ([`Sheet::set_items`]); everything else here is the sheet's own.
#[derive(Debug, Clone, Default)]
pub struct Sheet {
    pub title: String,
    pub crumbs: Vec<String>,
    /// Section chips under the header; Tab and Shift-Tab step through them.
    pub sections: Vec<String>,
    pub section: usize,
    /// Chips under the sections: a scope, a switch that applies to the whole
    /// list. Each is `(label, action, on)`.
    pub tools: Vec<(String, Action, bool)>,
    pub items: Vec<Item>,
    pub focus: usize,
    /// First body line shown.
    pub scroll: usize,
    /// `Some` when this sheet has a search: printable keys feed it.
    pub query: Option<String>,
    /// What just happened, on the left of the foot. A new sheet starts with
    /// none: a notice never carries over from the control that opened it.
    pub notice: String,
    /// The action the notice's `⟨ Undo ⟩` chip takes, while it is up.
    pub undo: Option<Action>,
    /// Whether this is the bottom of the stack: Esc then closes, and the
    /// header chip says so.
    pub root: bool,
    /// A decision prompt: the backdrop does not dismiss it, its answers
    /// carry the letters printed on them, it takes no key until it is armed
    /// ([`Sheet::armed`]), ←→ only move between its answers, and no digit
    /// answers it.
    pub decision: bool,
    /// When a decision was first drawn; set by [`draw`].
    pub shown: Option<Instant>,
    /// The last key that came before the decision was armed: every one
    /// restarts the wait.
    quiet: Option<Instant>,
    /// What Esc does here, when it is not Back or Close: a decision prompt
    /// says what Esc answers.
    pub esc: Option<String>,
    /// Columns kept free on the right for the owner to draw into (the
    /// theme preview).
    pub aside: u16,
    /// Body lines shown at the last draw; PgUp and PgDn move this far.
    pub page: usize,
    /// Body lines in all at the last draw, which bounds the wheel.
    pub lines: usize,
    /// How many things the search is narrowing, and how many of them match,
    /// for `N of M`.
    pub total: Option<usize>,
    pub matched: Option<usize>,
    /// The row a fresh sheet opens on, when its owner knows better than the
    /// current-value rule.
    pub prefer: Option<String>,
    /// Whether focus was placed yet: a sheet opens on the current value.
    placed: bool,
    /// A key moved focus: the next draw scrolls it into view.
    follow: bool,
    /// The search changed: the next draw puts focus on the first match.
    refocus_on_search: bool,
}

impl Sheet {
    pub fn new(title: impl Into<String>) -> Self {
        Self {
            title: title.into(),
            ..Self::default()
        }
    }

    /// Whether a key may answer this sheet now: any sheet that is not a
    /// decision, and a decision that has been on screen for [`ARMING`] with
    /// no key pressed in that time.
    #[must_use]
    pub fn armed(&self, now: Instant) -> bool {
        if !self.decision {
            return true;
        }
        let since = |at: Option<Instant>| at.is_none_or(|at| now.duration_since(at) >= ARMING);
        self.shown.is_some() && since(self.shown) && since(self.quiet)
    }

    /// Holds back an event that came before the decision was armed, and
    /// says so. `true` when it was held back.
    pub fn hold_back(&mut self, now: Instant) -> bool {
        if self.armed(now) {
            return false;
        }
        self.quiet = Some(now);
        self.notice = "Typing was held back: choose below when you are ready.".into();
        true
    }

    /// Replaces the rows, keeping focus on the row with the same id. A sheet
    /// seen for the first time opens on its current value, else on its
    /// first actionable row -- never on text and never on a danger.
    pub fn set_items(&mut self, items: Vec<Item>) {
        let focused = self.items.get(self.focus).map(|item| item.id.clone());
        let editing = self
            .items
            .get(self.focus)
            .and_then(|item| match &item.kind {
                Kind::Field(field) => Some((item.id.clone(), field.clone())),
                _ => None,
            });
        self.items = items;
        // A field keeps its caret and its selection across a rebuild that
        // did not change its text.
        if let Some((id, old)) = editing
            && let Some(item) = self.items.iter_mut().find(|item| item.id == id)
            && let Kind::Field(field) = &mut item.kind
            && field.text == old.text
        {
            field.cursor = old.cursor;
            field.fresh = old.fresh;
        }
        if !self.placed {
            self.placed = true;
            self.focus = self.opening_focus();
            return;
        }
        if let Some(id) = focused
            && let Some(at) = self.items.iter().position(|item| item.id == id)
        {
            self.focus = at;
            return;
        }
        self.focus = self.focus.min(self.items.len().saturating_sub(1));
        if !self.items.get(self.focus).is_some_and(Item::focusable) {
            self.focus = self
                .next_focusable(self.focus, true)
                .or_else(|| self.next_focusable(self.focus, false))
                .unwrap_or(0);
        }
    }

    /// Puts focus back where a fresh sheet would have it: on the current
    /// value. Used when the list under it changes meaning (a new section).
    pub fn refocus(&mut self) {
        self.placed = false;
        self.scroll = 0;
    }

    /// Moves focus to the row with this id, if there is one.
    pub fn focus_id(&mut self, id: &str) {
        if let Some(at) = self.items.iter().position(|item| item.id == id) {
            self.focus = at;
            self.placed = true;
        }
    }

    fn opening_focus(&self) -> usize {
        let preferred = self.prefer.as_ref().and_then(|id| {
            self.items
                .iter()
                .position(|item| item.id == *id && item.focusable())
        });
        preferred
            .or_else(|| {
                self.items
                    .iter()
                    .position(|item| item.focusable() && item.is_current())
            })
            .or_else(|| {
                self.items.iter().position(|item| {
                    item.focusable() && item.kind != Kind::Danger && item.disabled.is_none()
                })
            })
            .or_else(|| self.items.iter().position(Item::focusable))
            .unwrap_or(0)
    }

    fn next_focusable(&self, from: usize, forward: bool) -> Option<usize> {
        if forward {
            (from + 1..self.items.len()).find(|i| self.items[*i].focusable())
        } else {
            (0..from.min(self.items.len()))
                .rev()
                .find(|i| self.items[*i].focusable())
        }
    }

    fn first_focusable(&self) -> Option<usize> {
        self.items.iter().position(Item::focusable)
    }

    fn last_focusable(&self) -> Option<usize> {
        self.items.iter().rposition(Item::focusable)
    }

    /// The focused row, when focus rests on a focusable one.
    pub fn focused(&self) -> Option<&Item> {
        self.items.get(self.focus).filter(|item| item.focusable())
    }

    fn move_focus(&mut self, forward: bool, steps: usize) -> Outcome {
        let mut at = self.focus;
        for _ in 0..steps.max(1) {
            match self.next_focusable(at, forward) {
                Some(next) => at = next,
                None => break,
            }
        }
        if at == self.focus {
            return Outcome::Nothing;
        }
        self.focus = at;
        self.follow = true;
        Outcome::Redraw
    }

    /// What activating a row does: the one answer a click, Enter and Space
    /// share.
    pub fn activate(&mut self, index: usize) -> Outcome {
        let Some(item) = self.items.get(index) else {
            return Outcome::Nothing;
        };
        if !item.focusable() {
            return Outcome::Nothing;
        }
        self.focus = index;
        self.follow = true;
        if let Some(reason) = &item.disabled {
            self.notice = reason.clone();
            return Outcome::Redraw;
        }
        match &item.kind {
            Kind::Value { values, current } => {
                if values.is_empty() {
                    return Outcome::Nothing;
                }
                let next = current.map_or(0, |at| (at + 1) % values.len());
                Outcome::Act(values[next].1.clone())
            }
            _ => item.action.clone().map_or(Outcome::Redraw, Outcome::Act),
        }
    }

    /// ←→ on a Value row: the neighbouring value, applied.
    fn step_value(&mut self, forward: bool) -> Option<Outcome> {
        let item = self.items.get(self.focus)?;
        let Kind::Value { values, current } = &item.kind else {
            return None;
        };
        if let Some(reason) = &item.disabled {
            self.notice = reason.clone();
            return Some(Outcome::Redraw);
        }
        if values.is_empty() {
            return Some(Outcome::Nothing);
        }
        let n = values.len();
        let at = current.unwrap_or(0);
        let next = if forward {
            (at + 1).min(n - 1)
        } else {
            at.saturating_sub(1)
        };
        if Some(next) == *current {
            return Some(Outcome::Nothing);
        }
        Some(Outcome::Act(values[next].1.clone()))
    }

    fn searching(&self) -> bool {
        self.query.as_ref().is_some_and(|q| !q.is_empty())
    }

    /// One key, by the table in the interaction model. The same on every
    /// sheet; the owner only ever sees an [`Outcome`].
    pub fn key(&mut self, key: KeyEvent) -> Outcome {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        if let Some(outcome) = self.field_key(key) {
            return outcome;
        }
        // A sheet of text only -- the activity log, a long answer -- has no
        // row to move to, so the keys that move move the view.
        if self.first_focusable().is_none() {
            let lines = match key.code {
                KeyCode::Up => Some((true, 1)),
                KeyCode::Down => Some((false, 1)),
                KeyCode::PageUp => Some((true, self.page.max(1))),
                KeyCode::PageDown => Some((false, self.page.max(1))),
                KeyCode::Home => Some((true, usize::MAX)),
                KeyCode::End => Some((false, usize::MAX)),
                _ => None,
            };
            if let Some((up, by)) = lines {
                let max = self.lines.saturating_sub(self.page);
                let next = if up {
                    self.scroll.saturating_sub(by)
                } else {
                    self.scroll.saturating_add(by).min(max)
                };
                if next == self.scroll {
                    return Outcome::Nothing;
                }
                self.scroll = next;
                return Outcome::Redraw;
            }
        }
        match key.code {
            KeyCode::Char('z') if ctrl => Outcome::Undo,
            KeyCode::Char('u') if ctrl => match self.query.as_mut() {
                Some(query) if !query.is_empty() => {
                    query.clear();
                    self.after_search();
                    Outcome::Redraw
                }
                _ => Outcome::Nothing,
            },
            KeyCode::Up => self.move_focus(false, 1),
            KeyCode::Down => self.move_focus(true, 1),
            KeyCode::PageUp => self.move_focus(false, self.page.max(1)),
            KeyCode::PageDown => self.move_focus(true, self.page.max(1)),
            KeyCode::Home => match self.first_focusable() {
                Some(first) if first != self.focus => {
                    self.focus = first;
                    self.follow = true;
                    Outcome::Redraw
                }
                _ => Outcome::Nothing,
            },
            KeyCode::End => match self.last_focusable() {
                Some(last) if last != self.focus => {
                    self.focus = last;
                    self.follow = true;
                    Outcome::Redraw
                }
                _ => Outcome::Nothing,
            },
            KeyCode::Enter => self.activate(self.focus),
            KeyCode::Char(' ') if !ctrl && !alt && !self.searching() => self.activate(self.focus),
            // A decision's answers sit in a row: the arrows walk it, and
            // answer nothing.
            KeyCode::Left | KeyCode::Right if self.decision => {
                self.move_focus(key.code == KeyCode::Right, 1)
            }
            KeyCode::Left | KeyCode::Right => {
                let forward = key.code == KeyCode::Right;
                if let Some(outcome) = self.step_value(forward) {
                    return outcome;
                }
                if forward {
                    match self.focused().map(|item| &item.kind) {
                        Some(Kind::Open) => self.activate(self.focus),
                        _ => Outcome::Nothing,
                    }
                } else {
                    Outcome::Back
                }
            }
            KeyCode::Tab | KeyCode::BackTab if self.sections.len() > 1 => {
                let n = self.sections.len();
                let next = if key.code == KeyCode::Tab {
                    (self.section + 1) % n
                } else {
                    (self.section + n - 1) % n
                };
                self.choose_section(next)
            }
            KeyCode::Backspace => match self.query.as_mut() {
                Some(query) if !query.is_empty() => {
                    query.pop();
                    self.after_search();
                    Outcome::Redraw
                }
                _ => Outcome::Nothing,
            },
            KeyCode::Esc => {
                if self.searching() {
                    if let Some(query) = self.query.as_mut() {
                        query.clear();
                    }
                    self.after_search();
                    Outcome::Redraw
                } else {
                    Outcome::Back
                }
            }
            KeyCode::Char(c) if !ctrl && !alt => self.printable(c),
            _ => Outcome::Nothing,
        }
    }

    /// A printable key: an answer's letter, then the search, then a digit
    /// choosing the Nth choice. Anything else is ignored -- it never reaches
    /// the composer hidden behind the sheet.
    fn printable(&mut self, c: char) -> Outcome {
        let lower = c.to_ascii_lowercase();
        if let Some(at) = self
            .items
            .iter()
            .position(|item| item.key.is_some_and(|k| k.to_ascii_lowercase() == lower))
        {
            return self.activate(at);
        }
        if let Some(query) = self.query.as_mut() {
            query.push(c);
            self.after_search();
            return Outcome::Redraw;
        }
        // A digit picks the Nth choice of a list; it never answers a
        // decision and never reaches a row that cannot be taken back.
        if let Some(n) = c
            .to_digit(10)
            .filter(|n| (1..=9).contains(n) && !self.decision)
        {
            let choices: Vec<usize> = (0..self.items.len())
                .filter(|i| {
                    self.items[*i].focusable() && !matches!(self.items[*i].kind, Kind::Danger)
                })
                .collect();
            if let Some(at) = choices.get(n as usize - 1) {
                return self.activate(*at);
            }
        }
        Outcome::Nothing
    }

    /// Keys a focused text field takes before the sheet sees them.
    fn field_key(&mut self, key: KeyEvent) -> Option<Outcome> {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let item = self.items.get_mut(self.focus)?;
        let Kind::Field(field) = &mut item.kind else {
            return None;
        };
        let fresh = std::mem::take(&mut field.fresh);
        if fresh && matches!(key.code, KeyCode::Char(_) | KeyCode::Backspace) && !ctrl {
            field.text.clear();
            field.cursor = 0;
            if key.code == KeyCode::Backspace {
                return Some(Outcome::Act(Action::FieldEdited(self.focus)));
            }
        }
        let changed = match key.code {
            KeyCode::Char('u') if ctrl => {
                field.text.clear();
                field.cursor = 0;
                true
            }
            KeyCode::Char(c) if !ctrl => {
                field.text.insert(field.cursor, c);
                field.cursor += c.len_utf8();
                true
            }
            KeyCode::Backspace => {
                let before = field.text[..field.cursor]
                    .char_indices()
                    .last()
                    .map(|(i, _)| i);
                if let Some(i) = before {
                    field.text.drain(i..field.cursor);
                    field.cursor = i;
                }
                true
            }
            KeyCode::Left => {
                field.cursor = field.text[..field.cursor]
                    .char_indices()
                    .last()
                    .map_or(0, |(i, _)| i);
                true
            }
            KeyCode::Right => {
                field.cursor = field.text[field.cursor..]
                    .chars()
                    .next()
                    .map_or(field.cursor, |c| field.cursor + c.len_utf8());
                true
            }
            KeyCode::Home => {
                field.cursor = 0;
                true
            }
            KeyCode::End => {
                field.cursor = field.text.len();
                true
            }
            _ => false,
        };
        (changed || fresh).then_some(Outcome::Act(Action::FieldEdited(self.focus)))
    }

    fn after_search(&mut self) {
        self.scroll = 0;
        self.refocus_on_search = true;
    }

    /// Text pasted onto the sheet: into the focused field or the search,
    /// with control characters stripped. Anywhere else it is dropped.
    pub fn paste(&mut self, text: &str) -> Outcome {
        let clean: String = text.chars().filter(|c| !c.is_control()).collect();
        if let Some(item) = self.items.get_mut(self.focus)
            && let Kind::Field(field) = &mut item.kind
        {
            let clean = if field.list {
                text.lines()
                    .map(|line| line.trim().chars().filter(|c| !c.is_control()))
                    .map(String::from_iter)
                    .filter(|line| !line.is_empty())
                    .collect::<Vec<_>>()
                    .join(", ")
            } else {
                clean
            };
            if std::mem::take(&mut field.fresh) {
                field.text.clear();
                field.cursor = 0;
            }
            field.text.insert_str(field.cursor, &clean);
            field.cursor += clean.len();
            return Outcome::Act(Action::FieldEdited(self.focus));
        }
        if let Some(query) = self.query.as_mut() {
            query.push_str(&clean);
            self.after_search();
            return Outcome::Redraw;
        }
        Outcome::Nothing
    }

    fn choose_section(&mut self, section: usize) -> Outcome {
        if section == self.section || section >= self.sections.len() {
            return Outcome::Nothing;
        }
        self.section = section;
        if let Some(query) = self.query.as_mut() {
            query.clear();
        }
        self.refocus();
        Outcome::Section(section)
    }

    /// A click on one of this sheet's targets. One click acts: it focuses
    /// the row and does what Enter does.
    pub fn click(&mut self, hit: &Hit) -> Outcome {
        match hit {
            Hit::Item(i) => self.activate(*i),
            Hit::Value(i, v) => {
                let Some(item) = self.items.get(*i) else {
                    return Outcome::Nothing;
                };
                self.focus = *i;
                self.follow = true;
                if let Some(reason) = &item.disabled {
                    self.notice = reason.clone();
                    return Outcome::Redraw;
                }
                match &item.kind {
                    Kind::Value { values, .. } => values
                        .get(*v)
                        .map_or(Outcome::Nothing, |(_, action)| Outcome::Act(action.clone())),
                    _ => Outcome::Nothing,
                }
            }
            Hit::Section(s) => self.choose_section(*s),
            Hit::SectionStep(forward) => {
                let n = self.sections.len();
                if n < 2 {
                    return Outcome::Nothing;
                }
                let next = if *forward {
                    (self.section + 1) % n
                } else {
                    (self.section + n - 1) % n
                };
                self.choose_section(next)
            }
            Hit::Tool(t) => self
                .tools
                .get(*t)
                .map_or(Outcome::Nothing, |(_, action, _)| {
                    Outcome::Act(action.clone())
                }),
            Hit::Back => Outcome::Back,
            Hit::Fold(i) => {
                self.focus = *i;
                Outcome::Fold(*i)
            }
            Hit::Undo => self.undo.clone().map_or(Outcome::Undo, Outcome::Act),
        }
    }

    /// The wheel scrolls the view; focus stays where it is.
    pub fn wheel(&mut self, up: bool) -> Outcome {
        let max = self.lines.saturating_sub(self.page);
        let next = if up {
            self.scroll.saturating_sub(3)
        } else {
            (self.scroll + 3).min(max)
        };
        if next == self.scroll {
            return Outcome::Nothing;
        }
        self.scroll = next;
        self.follow = false;
        Outcome::Redraw
    }

    /// The foot's hint, generated from what the focused row is.
    pub fn hint(&self) -> String {
        self.hint_parts().join(" · ")
    }

    /// The hint in the room a notice leaves: the parts in the middle give
    /// way first, and what the focused row does and what Esc does stay.
    fn hint_within(&self, room: usize) -> String {
        let mut parts = self.hint_parts();
        while parts.len() > 2 && parts.join(" · ").chars().count() > room {
            parts.remove(parts.len() - 2);
        }
        parts.join(" · ")
    }

    fn hint_parts(&self) -> Vec<String> {
        let mut parts: Vec<String> = Vec::new();
        match self
            .focused()
            .map(|item| (&item.kind, item.disabled.is_some()))
        {
            Some((_, true)) => parts.push("Enter says why".into()),
            Some((Kind::Choice { .. }, _)) => parts.push("Enter choose".into()),
            Some((Kind::Value { .. }, _)) => parts.push("←→ change".into()),
            Some((Kind::Toggle(_), _)) => parts.push("Enter switch".into()),
            Some((Kind::Open, _)) => parts.push("Enter open".into()),
            Some((Kind::Run, _)) => parts.push("Enter run".into()),
            Some((Kind::Danger, _)) => parts.push("Enter confirm".into()),
            Some((Kind::Field(_), _)) => parts.push("type to edit".into()),
            _ => {}
        }
        if self.sections.len() > 1 {
            parts.push("Tab section".into());
        }
        if self.query.is_some() {
            parts.push("type to filter".into());
        }
        parts.push(match &self.esc {
            Some(esc) => format!("Esc {}", esc.to_lowercase()),
            None if self.root => "Esc close".into(),
            None => "Esc back".into(),
        });
        parts
    }
}

/// A click target on a sheet, carried in [`Action::Sheet`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Hit {
    Item(usize),
    Value(usize, usize),
    Section(usize),
    /// `‹` (false) or `›` (true) beside the section chips.
    SectionStep(bool),
    Tool(usize),
    Back,
    Undo,
    /// The `⟨ +N ▾ ⟩` chip of the Value row at this index.
    Fold(usize),
}

/// Where a sheet was drawn: the body and, when it keeps one, the room on
/// the right its owner draws into.
pub struct Drawn {
    pub body: Rect,
    pub aside: Option<Rect>,
}

/// Draws a sheet into `area`: the header and its Esc chip, the sections and
/// the search, the tools, the rows with a more-cue wherever something is
/// hidden, and the foot -- the notice on the left, cut before it reaches the
/// hint on the right.
pub fn draw(
    f: &mut Frame<'_>,
    g: &mut Geometry,
    area: Rect,
    sheet: &mut Sheet,
    t: Theme,
    press: Option<(u16, u16)>,
    hover: Option<(u16, u16)>,
) -> Drawn {
    let hit = |g: &mut Geometry, r: Rect, h: Hit| g.hits.push((r, Action::Sheet(h)));
    // The first frame a decision is drawn on starts its arming window.
    if sheet.decision && sheet.shown.is_none() {
        sheet.shown = Some(Instant::now());
    }
    let mut y = area.y;
    let bottom = area.bottom();
    if area.width < 4 || area.height < 3 {
        return Drawn {
            body: area,
            aside: None,
        };
    }
    // Header: TITLE › crumb › crumb, and the chip that says what Esc does.
    let back = match &sheet.esc {
        Some(esc) => format!("Esc · {esc}"),
        None if sheet.root => "Esc · Close".to_string(),
        None => "Esc · Back".to_string(),
    };
    let back = back.as_str();
    let back_w = chrome::width(back) + 4;
    let mut heading = sheet.title.to_uppercase();
    for crumb in &sheet.crumbs {
        heading.push_str(" › ");
        heading.push_str(crumb);
    }
    let room = area.width.saturating_sub(back_w + 1);
    chrome::text(
        f,
        Rect::new(area.x, y, room, 1),
        &clip(&heading, room as usize),
        Tone::Accent,
        t,
    );
    if area.width > back_w + 8 {
        let w = chrome::chip(
            f,
            g,
            area.right().saturating_sub(back_w),
            y,
            area.right(),
            back,
            Action::Sheet(Hit::Back),
            false,
            Tone::Normal,
            press,
            t,
        );
        let _ = w;
    }
    y += 1;
    // Sections, with `‹` and `›` as targets of their own, and the search at
    // the right end of the same line.
    // The least the section strip needs: its arrows, the open section and
    // the `⟨ +N ▾ ⟩` that holds the rest. The search's words give way
    // before the strip loses its fold.
    let strip = match sheet.sections.get(sheet.section) {
        Some(open) if sheet.sections.len() > 1 => {
            chrome::width(open) + chrome::width(" ●") + chrome::width("+99 ▾") + 14
        }
        _ => 0,
    };
    let search = sheet.query.as_ref().map(|query| {
        let count = match sheet.total {
            Some(total) => format!(
                "{} of {total}",
                sheet.matched.unwrap_or_else(|| sheet
                    .items
                    .iter()
                    .filter(|i| i.focusable())
                    .count())
            ),
            None => String::new(),
        };
        let said = match (query.is_empty(), count.is_empty()) {
            (true, true) => "⌕ type to filter".to_string(),
            (true, false) => format!("⌕ type to filter · {count}"),
            (false, true) => format!("⌕ {query}▏"),
            (false, false) => format!("⌕ {query}▏ · {count}"),
        };
        if chrome::width(&said) + 2 + strip <= area.width {
            said
        } else if query.is_empty() {
            format!("⌕ {count}")
        } else {
            format!("⌕ {query}▏")
        }
    });
    if !sheet.sections.is_empty() || search.is_some() {
        let search_w = search.as_deref().map_or(0, chrome::width);
        let limit = area.right().saturating_sub(search_w + 2);
        let mut x = area.x;
        if sheet.sections.len() > 1 {
            chrome::text(f, Rect::new(x, y, 1, 1), "‹", Tone::Accent, t);
            hit(g, Rect::new(x, y, 1, 1), Hit::SectionStep(false));
            x += 2;
            // The sections that do not fit fold into `⟨ +N ▾ ⟩`, never the
            // open one.
            let sections: Vec<(String, Action, bool)> = sheet
                .sections
                .iter()
                .enumerate()
                .map(|(i, name)| {
                    let label = if i == sheet.section {
                        format!("{name} ●")
                    } else {
                        name.clone()
                    };
                    (label, Action::Sheet(Hit::Section(i)), i == sheet.section)
                })
                .collect();
            x = chrome::chips(f, g, x, y, limit.saturating_sub(2), &sections, press, t);
            if x < limit {
                chrome::text(f, Rect::new(x, y, 1, 1), "›", Tone::Accent, t);
                hit(g, Rect::new(x, y, 1, 1), Hit::SectionStep(true));
            }
        }
        if let Some(search) = &search {
            let x = area.right().saturating_sub(search_w);
            chrome::text(
                f,
                Rect::new(x, y, search_w, 1),
                search,
                if sheet.searching() {
                    Tone::Strong
                } else {
                    Tone::Muted
                },
                t,
            );
        }
        y += 1;
    }
    if !sheet.tools.is_empty() && y < bottom {
        let tools: Vec<(String, Action, bool)> = sheet
            .tools
            .iter()
            .enumerate()
            .map(|(i, (label, _, on))| (label.clone(), Action::Sheet(Hit::Tool(i)), *on))
            .collect();
        chrome::chips(f, g, area.x, y, area.right(), &tools, press, t);
        y += 1;
    }
    if y < bottom {
        chrome::rule(f, Rect::new(area.x, y, area.width, 1), &[], Tone::Line, t);
        y += 1;
    }
    // The foot takes the last two lines when there is room for them.
    let foot = bottom.saturating_sub(2);
    let has_foot = foot > y + 1;
    let body_bottom = if has_foot { foot } else { bottom };
    let list_width = area.width.saturating_sub(sheet.aside);
    let body = Rect::new(area.x, y, list_width, body_bottom.saturating_sub(y));
    let aside = (sheet.aside > 0 && area.width > sheet.aside + 10).then(|| {
        Rect::new(
            area.x + list_width + 1,
            y,
            sheet.aside.saturating_sub(1),
            body.height,
        )
    });
    draw_body(f, g, body, sheet, t, press, hover);
    if has_foot {
        chrome::rule(
            f,
            Rect::new(area.x, foot, area.width, 1),
            &[],
            Tone::Line,
            t,
        );
        // A notice is what just happened, and it is read once: the hint
        // gives way to it, down to its first and last parts. Without one the
        // hint still gives way to the width, and what cannot fit even then
        // ends in an ellipsis rather than mid-word.
        let hint = if sheet.notice.is_empty() {
            sheet.hint_within(area.width as usize)
        } else {
            let wanted = chrome::width(&sheet.notice) as usize + 12;
            sheet.hint_within((area.width as usize).saturating_sub(wanted))
        };
        let hint = clip(&hint, area.width as usize);
        let hint_w = chrome::width(&hint).min(area.width);
        chrome::text(
            f,
            Rect::new(area.right().saturating_sub(hint_w), foot + 1, hint_w, 1),
            &hint,
            Tone::Muted,
            t,
        );
        let undo_w = if sheet.undo.is_some() && !sheet.notice.is_empty() {
            chrome::width("Undo") + 5
        } else {
            0
        };
        let room = area.width.saturating_sub(hint_w + 2 + undo_w) as usize;
        if !sheet.notice.is_empty() && room > 1 {
            let notice = clip(&sheet.notice, room);
            let w = chrome::width(&notice);
            chrome::text(
                f,
                Rect::new(area.x, foot + 1, w, 1),
                &notice,
                Tone::Accent,
                t,
            );
            if undo_w > 0 {
                chrome::chip(
                    f,
                    g,
                    area.x + w + 1,
                    foot + 1,
                    area.right().saturating_sub(hint_w + 1),
                    "Undo",
                    Action::Sheet(Hit::Undo),
                    false,
                    Tone::Normal,
                    press,
                    t,
                );
            }
        }
    }
    Drawn { body, aside }
}

/// One drawn line of the body: which item it belongs to, and whether it is
/// the item's title line or one of its detail lines.
enum Line {
    Title(usize),
    /// A run of inline items drawn as chips on one line.
    Chips(Vec<usize>),
    Detail(usize, String),
    /// Wrapped continuation of an Info or Heading row.
    Text(usize, String),
}

fn layout_lines(sheet: &Sheet, width: usize) -> Vec<Line> {
    let mut lines = Vec::new();
    let mut i = 0;
    while i < sheet.items.len() {
        let item = &sheet.items[i];
        if item.inline {
            // A run of chips wraps to as many lines as it needs: an answer
            // pushed past the edge is an answer nobody can see.
            let chip_width = |item: &Item| {
                let key = item.key.map_or(0, |_| 4);
                // Room for the focus mark too, so focus never rewraps.
                super::chrome::width(&item.title) as usize + key + 2 + 5
            };
            let mut run = Vec::new();
            let mut used = 2;
            while i < sheet.items.len() && sheet.items[i].inline {
                let w = chip_width(&sheet.items[i]);
                if !run.is_empty() && used + w > width {
                    lines.push(Line::Chips(std::mem::take(&mut run)));
                    used = 2;
                }
                run.push(i);
                used += w;
                i += 1;
            }
            lines.push(Line::Chips(run));
            continue;
        }
        match item.kind {
            Kind::Info | Kind::Heading => {
                let text = if item.kind == Kind::Heading {
                    item.title.to_uppercase()
                } else {
                    item.title.clone()
                };
                let wrapped = super::view::wrap_words(&text, width.max(8));
                if wrapped.is_empty() {
                    lines.push(Line::Text(i, String::new()));
                }
                for line in wrapped {
                    lines.push(Line::Text(i, line));
                }
            }
            _ => {
                lines.push(Line::Title(i));
                // A reason shared by a run of rows -- a locked account's
                // models -- is said under the first of them only.
                let repeated = item.detail.is_empty()
                    && item.disabled.is_some()
                    && i > 0
                    && sheet.items[i - 1].disabled == item.disabled;
                if !repeated && (!item.detail.is_empty() || item.disabled.is_some()) {
                    let detail = match &item.disabled {
                        Some(reason) if item.detail.is_empty() => reason.clone(),
                        Some(reason) => format!("{} · {reason}", item.detail),
                        None => item.detail.clone(),
                    };
                    for line in super::view::wrap_words(&detail, width.saturating_sub(4).max(8)) {
                        lines.push(Line::Detail(i, line));
                    }
                }
            }
        }
        i += 1;
    }
    lines
}

fn line_item(line: &Line) -> Option<usize> {
    match line {
        Line::Title(i) | Line::Detail(i, _) | Line::Text(i, _) => Some(*i),
        Line::Chips(run) => run.first().copied(),
    }
}

fn contains_focus(line: &Line, focus: usize) -> bool {
    match line {
        Line::Chips(run) => run.contains(&focus),
        other => line_item(other) == Some(focus),
    }
}

#[allow(clippy::too_many_arguments)]
fn draw_body(
    f: &mut Frame<'_>,
    g: &mut Geometry,
    body: Rect,
    sheet: &mut Sheet,
    t: Theme,
    press: Option<(u16, u16)>,
    hover: Option<(u16, u16)>,
) {
    let lines = layout_lines(sheet, body.width as usize);
    let height = body.height as usize;
    sheet.lines = lines.len();
    // One line each for the cues when they are needed.
    let page = height.max(1);
    sheet.page = page.saturating_sub(2).max(1);
    if sheet.refocus_on_search {
        sheet.refocus_on_search = false;
        sheet.focus = sheet.items.iter().position(Item::focusable).unwrap_or(0);
        sheet.follow = true;
    }
    // Keep the focused row in view after a key moved it; the wheel alone
    // leaves it wherever it went.
    if sheet.follow {
        sheet.follow = false;
        let first = lines.iter().position(|l| contains_focus(l, sheet.focus));
        let last = lines.iter().rposition(|l| contains_focus(l, sheet.focus));
        if let (Some(first), Some(last)) = (first, last) {
            let visible = page.saturating_sub(2).max(1);
            if first < sheet.scroll + usize::from(sheet.scroll > 0) {
                sheet.scroll = first.saturating_sub(1);
            } else if last >= sheet.scroll + visible {
                sheet.scroll = (last + 1).saturating_sub(visible);
            }
        }
    }
    sheet.scroll = sheet
        .scroll
        .min(lines.len().saturating_sub(page.saturating_sub(1)));
    let above = sheet.scroll;
    let mut room = height;
    let mut y = body.y;
    if above > 0 && room > 0 {
        chrome::text(
            f,
            Rect::new(body.x, y, body.width, 1),
            &format!("↑ {above} more"),
            Tone::Muted,
            t,
        );
        y += 1;
        room -= 1;
    }
    let remaining = lines.len().saturating_sub(above);
    let shown = if remaining > room {
        room.saturating_sub(1)
    } else {
        remaining
    };
    for line in lines.iter().skip(above).take(shown) {
        draw_line(
            f,
            g,
            Rect::new(body.x, y, body.width, 1),
            line,
            sheet,
            t,
            press,
            hover,
        );
        y += 1;
    }
    let below = remaining.saturating_sub(shown);
    if below > 0 && y < body.bottom() {
        chrome::text(
            f,
            Rect::new(body.x, y, body.width, 1),
            &format!("↓ {below} more"),
            Tone::Muted,
            t,
        );
    }
}

#[allow(clippy::too_many_arguments)]
fn draw_line(
    f: &mut Frame<'_>,
    g: &mut Geometry,
    r: Rect,
    line: &Line,
    sheet: &Sheet,
    t: Theme,
    press: Option<(u16, u16)>,
    hover: Option<(u16, u16)>,
) {
    let hovered = |r: Rect| hover.is_some_and(|(x, y)| super::contains(r, x, y));
    match line {
        Line::Text(i, text) => {
            let item = &sheet.items[*i];
            let tone = if item.kind == Kind::Heading {
                Tone::Muted
            } else {
                item.tone
            };
            chrome::text(f, r, text, tone, t);
        }
        Line::Detail(i, text) => {
            let item = &sheet.items[*i];
            let tone = if item.disabled.is_some() {
                Tone::Warning
            } else {
                Tone::Muted
            };
            chrome::text(
                f,
                Rect::new(r.x + 4, r.y, r.width.saturating_sub(4), 1),
                text,
                tone,
                t,
            );
            g.hits.push((r, Action::Sheet(Hit::Item(*i))));
        }
        Line::Chips(run) => {
            let mut x = r.x + 2;
            for i in run {
                let item = &sheet.items[*i];
                let focused = *i == sheet.focus;
                let label = match item.key {
                    Some(k) => format!("{k} · {}", item.title),
                    None => item.title.clone(),
                };
                let label = if focused {
                    format!("› {label}")
                } else {
                    label
                };
                let tone = if item.disabled.is_some() {
                    Tone::Muted
                } else if item.kind == Kind::Danger {
                    Tone::Warning
                } else {
                    Tone::Normal
                };
                let w = chrome::chip(
                    f,
                    g,
                    x,
                    r.y,
                    r.right(),
                    &label,
                    Action::Sheet(Hit::Item(*i)),
                    focused,
                    tone,
                    press,
                    t,
                );
                if w == 0 {
                    break;
                }
                x += w + 1;
            }
        }
        Line::Title(i) => {
            let item = &sheet.items[*i];
            let focused = *i == sheet.focus;
            let hot = hovered(r);
            let mark = if focused { "›" } else { " " };
            let tone = if item.disabled.is_some() {
                Tone::Muted
            } else if focused {
                Tone::Accent
            } else if item.kind == Kind::Danger {
                Tone::Warning
            } else if hot {
                Tone::Strong
            } else {
                Tone::Normal
            };
            let title = match (&item.kind, item.key) {
                (_, Some(k)) => format!("{k} · {}", item.title),
                (Kind::Open, _) => format!("{} ›", item.title),
                (Kind::Run, _) => format!("⏎ {}", item.title),
                (Kind::Danger, _) => format!("▲ {}", item.title),
                (Kind::Choice { current: true }, _) => format!("{}  ● now", item.title),
                _ => item.title.clone(),
            };
            match &item.kind {
                Kind::Value { values, current } => {
                    let label_w = (r.width / 3).clamp(12, 30);
                    chrome::text(
                        f,
                        Rect::new(r.x, r.y, label_w, 1),
                        &clip(&format!("{mark} {title}"), label_w as usize),
                        tone,
                        t,
                    );
                    g.hits.push((
                        Rect::new(r.x, r.y, label_w, 1),
                        Action::Sheet(Hit::Item(*i)),
                    ));
                    chips_in_row(
                        f,
                        g,
                        Rect::new(r.x + label_w, r.y, r.width.saturating_sub(label_w), 1),
                        *i,
                        values,
                        *current,
                        t,
                        press,
                    );
                }
                Kind::Toggle(on) => {
                    let text = format!("{mark} {title}");
                    let w = chrome::width(&text).min(r.width);
                    chrome::text(f, Rect::new(r.x, r.y, w, 1), &text, tone, t);
                    chrome::chip(
                        f,
                        g,
                        r.x + w + 2,
                        r.y,
                        r.right(),
                        if *on { "on ●" } else { "off" },
                        Action::Sheet(Hit::Item(*i)),
                        *on,
                        Tone::Normal,
                        press,
                        t,
                    );
                    g.hits.push((r, Action::Sheet(Hit::Item(*i))));
                }
                Kind::Field(field) => {
                    let label_w = (chrome::width(&item.title) + 3).min(r.width / 2);
                    chrome::text(
                        f,
                        Rect::new(r.x, r.y, label_w, 1),
                        &format!("{mark} {}", item.title.to_uppercase()),
                        tone,
                        t,
                    );
                    let shown = if field.secret {
                        "•".repeat(field.text.chars().count())
                    } else {
                        field.text.clone()
                    };
                    let x = r.x + label_w + 1;
                    let room = r.right().saturating_sub(x + 2) as usize;
                    let tone = if focused { Tone::Strong } else { Tone::Normal };
                    chrome::text(f, Rect::new(x, r.y, 1, 1), "┃", tone, t);
                    let shown = clip(&shown, room);
                    if focused && field.fresh && !shown.is_empty() {
                        // Selected: the next key or paste replaces all of it.
                        f.render_widget(
                            ratatui::widgets::Paragraph::new(shown.clone())
                                .style(super::theme::chip_on(t)),
                            Rect::new(x + 1, r.y, chrome::width(&shown), 1).intersection(f.area()),
                        );
                    } else {
                        // The caret stands where the next letter goes.
                        let at = field.text[..field.cursor.min(field.text.len())]
                            .chars()
                            .count();
                        let before: String = shown.chars().take(at).collect();
                        let after: String = shown.chars().skip(at).collect();
                        let caret = if focused { "▏" } else { "" };
                        chrome::text(
                            f,
                            Rect::new(x + 1, r.y, r.right().saturating_sub(x + 1), 1),
                            &format!("{before}{caret}{after}"),
                            tone,
                            t,
                        );
                    }
                    g.hits.push((r, Action::Sheet(Hit::Item(*i))));
                }
                _ => {
                    // `› ██ name`: the mark, the swatch, then the name alone,
                    // so the mark is drawn once and never over the swatch.
                    let (r, shown) = match item.swatch {
                        Some(ink) => {
                            chrome::text(f, Rect::new(r.x, r.y, 2, 1), mark, tone, t);
                            chrome::text(f, Rect::new(r.x + 2, r.y, 2, 1), "██", ink, t);
                            g.hits
                                .push((Rect::new(r.x, r.y, 5, 1), Action::Sheet(Hit::Item(*i))));
                            let r = Rect::new(r.x + 5, r.y, r.width.saturating_sub(5), 1);
                            (r, title.to_string())
                        }
                        None => (r, format!("{mark} {title}")),
                    };
                    chrome::text(f, r, &clip(&shown, r.width as usize), tone, t);
                    if item.focusable() {
                        g.hits.push((r, Action::Sheet(Hit::Item(*i))));
                    }
                }
            }
            if hot && item.focusable() && !focused {
                f.buffer_mut()
                    .set_style(Rect::new(r.x, r.y, 1, 1), theme::style(Tone::Accent, t));
            }
        }
    }
}

/// A Value row's chips. When they do not all fit, the current one is kept
/// and the rest fold into `⟨ +N ▾ ⟩`, which opens the whole list: the active
/// value is never the one that is dropped.
#[allow(clippy::too_many_arguments)]
fn chips_in_row(
    f: &mut Frame<'_>,
    g: &mut Geometry,
    r: Rect,
    item: usize,
    values: &[(String, Action)],
    current: Option<usize>,
    t: Theme,
    press: Option<(u16, u16)>,
) {
    let label = |i: usize| {
        if Some(i) == current {
            format!("{} ●", values[i].0)
        } else {
            values[i].0.clone()
        }
    };
    let widths: Vec<u16> = (0..values.len())
        .map(|i| chrome::width(&label(i)) + 4 + 1)
        .collect();
    let shown = chrome::fitting(&widths, r.width, &current.into_iter().collect::<Vec<_>>());
    let folded = values.len() - shown.len();
    let mut x = r.x;
    for i in shown {
        let w = chrome::chip(
            f,
            g,
            x,
            r.y,
            r.right(),
            &label(i),
            Action::Sheet(Hit::Value(item, i)),
            Some(i) == current,
            Tone::Normal,
            press,
            t,
        );
        if w == 0 {
            break;
        }
        x += w + 1;
    }
    if folded > 0 {
        chrome::chip(
            f,
            g,
            x,
            r.y,
            r.right(),
            &format!("+{folded} ▾"),
            Action::Sheet(Hit::Fold(item)),
            false,
            Tone::Normal,
            press,
            t,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A sheet never opens on a row that cannot be taken back, even when it
    /// is the first row: a reflexive Enter must not be the dangerous one.
    #[test]
    fn a_sheet_never_opens_on_a_danger_row() {
        let mut sheet = Sheet::new("Confirm");
        sheet.set_items(vec![
            Item::danger(
                "yes",
                "Yes · Never asks",
                Action::ConfirmRung("full".into()),
            ),
            Item::run("cancel", "Cancel", Action::Close),
        ]);
        assert_eq!(sheet.focused().unwrap().id, "cancel");
    }

    /// A digit picks the Nth choice of a list, but never a row that cannot
    /// be taken back.
    #[test]
    fn a_digit_never_reaches_a_danger_row() {
        let mut sheet = Sheet::new("Rollback");
        sheet.set_items(vec![
            Item::run("cancel", "Cancel", Action::Close),
            Item::danger(
                "yes",
                "Roll back",
                Action::Command("/rollback confirm".into()),
            ),
        ]);
        let key = |c| KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE);
        assert_eq!(sheet.key(key('2')), Outcome::Nothing);
        assert_eq!(sheet.key(key('1')), Outcome::Act(Action::Close));
    }
}
