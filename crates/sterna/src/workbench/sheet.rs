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
//!
//! **One grid, and only rows in the list.** Every row is one line: its label
//! in one column and its value in the next, the same column for every row on
//! the sheet. Groups are a bold name and a rule with an empty line above.
//! What the focused row means is said once, in the strip under the list;
//! nothing is drawn between two rows.
use super::{Action, Geometry, Tone, chrome, document::clip, theme};
use crate::tui::Theme;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{Frame, layout::Rect};
use std::time::{Duration, Instant};

/// How long a decision must have been on screen, with no key pressed, before
/// a key can answer it: a sentence typed as it appears answers nothing.
pub const ARMING: Duration = Duration::from_millis(500);

/// What one row is. Every row is exactly one kind.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Kind {
    /// Plain text, or with a value a fact: `Child processes   Seatbelt`.
    /// Not focusable; focus skips it.
    Info,
    /// A group's name. Not focusable.
    Heading,
    /// One choice of a setting, as a whole row: a click applies it and the
    /// sheet stays open. The current one is marked `●` before its name.
    Choice { current: bool },
    /// A setting's values in one row; the current one is filled. ←→ step
    /// through them, and each step applies.
    Value {
        values: Vec<(String, Action)>,
        current: Option<usize>,
    },
    /// On or off; a click flips it.
    Toggle(bool),
    /// Goes somewhere: another sheet, a step, a longer list. `title ›`, or
    /// its value and `›`.
    Open,
    /// A one-shot action.
    Run,
    /// An action that cannot be taken back; it opens a confirmation that
    /// starts on Cancel.
    Danger,
    /// A text field in a form.
    Field(Field),
}

/// A text field's contents and caret.
#[derive(Debug, Clone, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
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
    /// What the row means, said in the strip under the list while the row
    /// has the focus.
    pub detail: String,
    /// What stands in the value column on the row's own line: a setting's
    /// value, a fact, a few words on what the row does.
    pub value: Option<String>,
    pub kind: Kind,
    pub action: Option<Action>,
    /// Why this row cannot be used now. It stays visible and muted, the
    /// strip says why, and activating it puts the reason in the notice and
    /// does nothing else.
    pub disabled: Option<String>,
    /// A letter or digit that acts on this row from anywhere on the sheet,
    /// printed on it. Decision prompts use these.
    pub key: Option<char>,
    /// Drawn as a chip beside the inline rows next to it, rather than as a
    /// line of its own: the answers of a decision prompt, a pair of buttons.
    pub inline: bool,
    /// A chip at the end of the line above it -- the row or group it acts
    /// on -- instead of on a line of its own: `⟨ Use default ⟩`, `⟨ Forget ⟩`.
    pub trail: bool,
    /// The tone of an Info row's text, and of a chip.
    pub tone: Tone,
    /// A swatch drawn before the title, part of the row's target: a theme's
    /// accent, or the terminal's own ink for one with none (mono), so every
    /// name in the list starts in one column.
    pub swatch: Option<Tone>,
    /// What the strip says on its first line, beside the name, while this
    /// row has the focus.
    pub card: Option<Card>,
}

/// When a change on the focused row applies and where its value comes from,
/// said beside its name in the strip.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Card {
    pub when: String,
    pub source: String,
}

impl Item {
    fn new(id: impl Into<String>, title: impl Into<String>, kind: Kind) -> Self {
        Self {
            id: id.into(),
            title: title.into(),
            detail: String::new(),
            value: None,
            kind,
            action: None,
            disabled: None,
            key: None,
            inline: false,
            trail: false,
            tone: Tone::Normal,
            swatch: None,
            card: None,
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
    /// What the strip says beside the row's name: when a change applies,
    /// and where the value comes from.
    #[must_use]
    pub fn card(mut self, when: impl Into<String>, source: impl Into<String>) -> Self {
        self.card = Some(Card {
            when: when.into(),
            source: source.into(),
        });
        self
    }
    #[must_use]
    pub fn detail(mut self, detail: impl Into<String>) -> Self {
        self.detail = detail.into();
        self
    }
    /// The words in the value column.
    #[must_use]
    pub fn shows(mut self, value: impl Into<String>) -> Self {
        self.value = Some(value.into());
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
    /// A chip at the end of the line above: see [`Item::trail`].
    #[must_use]
    pub fn trail(mut self) -> Self {
        self.inline = true;
        self.trail = true;
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

/// A switch over the whole list, on the title line: its choices in a row,
/// the current one filled.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tool {
    pub options: Vec<(String, Action)>,
    pub current: Option<usize>,
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
    /// A section was chosen; the owner rebuilds the rows.
    Section(usize),
    /// Ctrl-Z: take back the last change, wherever it was made.
    Undo,
    /// `current ▾` on a Value row whose values do not fit: open the whole
    /// list of its values.
    Fold(usize),
}

/// One sheet's state. Its rows are rebuilt from their owner's data on every
/// frame ([`Sheet::set_items`]); everything else here is the sheet's own.
#[derive(Debug, Clone, Default)]
pub struct Sheet {
    pub title: String,
    pub crumbs: Vec<String>,
    /// Section names under the title; Tab and Shift-Tab step through them.
    pub sections: Vec<String>,
    pub section: usize,
    /// Switches over the whole list, on the title line.
    pub tools: Vec<Tool>,
    pub items: Vec<Item>,
    pub focus: usize,
    /// First body line shown.
    pub scroll: usize,
    /// `Some` when this sheet has a search: printable keys feed it.
    pub query: Option<String>,
    /// What just happened, on the left of the foot. A new sheet starts with
    /// none: a notice never carries over from the control that opened it.
    pub notice: String,
    /// What the foot says when nothing just happened: where a choice is
    /// saved, what Esc answers.
    pub status: String,
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
    /// The body line each row starts on at the last draw (`None` for a row
    /// not laid out), so PgUp and PgDn move by what a page shows.
    pub item_lines: Vec<Option<usize>>,
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

    /// PgUp and PgDn move the focus by what one page shows: to the farthest
    /// row that starts within a page of the focused one. **Counting rows
    /// instead of lines skipped every row whose description pushed it below
    /// the page**, so one PgDn on Advanced passed thirteen settings no one
    /// had seen. A row taller than a page moves one row.
    fn page_focus(&mut self, forward: bool) -> Outcome {
        let line_of = |i: usize| self.item_lines.get(i).copied().flatten();
        let Some(from) = line_of(self.focus) else {
            return self.move_focus(forward, 1);
        };
        let page = self.page.max(1);
        let mut at = self.focus;
        while let Some(next) = self.next_focusable(at, forward) {
            let within = line_of(next).is_some_and(|line| {
                if forward {
                    line <= from + page
                } else {
                    line + page >= from
                }
            });
            if !within {
                break;
            }
            at = next;
        }
        if at == self.focus {
            return self.move_focus(forward, 1);
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
            KeyCode::PageUp => self.page_focus(false),
            KeyCode::PageDown => self.page_focus(true),
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
        // Enter belongs to the sheet: it saves, or says nothing changed. As
        // a field's first key it was swallowed, and the second saved the
        // word the field opened with.
        if key.code == KeyCode::Enter {
            return None;
        }
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
    /// the row and does what Enter does. A click on the value that is
    /// already chosen changes nothing.
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
                    Kind::Toggle(on) if (*v == 1) != *on => {
                        item.action.clone().map_or(Outcome::Nothing, Outcome::Act)
                    }
                    _ => Outcome::Redraw,
                }
            }
            Hit::Section(s) => self.choose_section(*s),
            Hit::Tool(t, o) => self
                .tools
                .get(*t)
                .filter(|tool| tool.current != Some(*o))
                .and_then(|tool| tool.options.get(*o))
                .map_or(Outcome::Nothing, |(_, action)| Outcome::Act(action.clone())),
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
            .map(|item| (&item.kind, item.disabled.is_some(), item.title.as_str()))
        {
            Some((_, true, _)) => parts.push("Enter says why".into()),
            Some((Kind::Choice { .. }, ..)) => parts.push("Enter choose".into()),
            Some((Kind::Value { .. }, ..)) => parts.push("←→ change".into()),
            Some((Kind::Toggle(_), ..)) => parts.push("Enter switch".into()),
            Some((Kind::Open, ..)) => parts.push("Enter open".into()),
            // Enter does what the row says: "Enter cancel" on Cancel, never
            // "Enter run" on a row that runs nothing.
            Some((Kind::Run, _, title)) if title.chars().count() <= 28 => {
                parts.push(format!("Enter {}", sentence_case(title)));
            }
            Some((Kind::Run, ..)) => parts.push("Enter run".into()),
            Some((Kind::Danger, ..)) => parts.push("Enter confirm".into()),
            Some((Kind::Field(_), ..)) => parts.push("type to edit".into()),
            _ => {}
        }
        if self.sections.len() > 1 {
            parts.push("Tab section".into());
        }
        if self.query.is_some() {
            parts.push("type to filter".into());
        }
        parts.push(match &self.esc {
            Some(esc) => format!("Esc {}", sentence_case(esc)),
            None if self.root => "Esc close".into(),
            None => "Esc back".into(),
        });
        parts
    }
}

/// A label as it reads after "Enter" or "Esc": its first letter lowered,
/// every other word as written, so a name keeps its capital ("let Sterna
/// decide") and an acronym stays whole.
fn sentence_case(label: &str) -> String {
    let mut chars = label.chars();
    match (chars.next(), label.chars().nth(1)) {
        (Some(first), Some(second)) if !second.is_uppercase() => {
            first.to_lowercase().chain(chars).collect()
        }
        _ => label.to_string(),
    }
}

/// A click target on a sheet, carried in [`Action::Sheet`].
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Hit {
    Item(usize),
    Value(usize, usize),
    Section(usize),
    /// One choice of one of the title line's switches.
    Tool(usize, usize),
    Back,
    Undo,
    /// The `current ▾` of the Value row at this index.
    Fold(usize),
}

/// Where a sheet was drawn: the body and, when it keeps one, the room on
/// the right its owner draws into, and where a list longer than the body
/// stands.
pub struct Drawn {
    pub body: Rect,
    pub aside: Option<Rect>,
    pub scroll: Option<Scroll>,
}

/// A list that does not fit its body: the rows the body covers on screen,
/// the first line in view and how many lines there are.
#[derive(Debug, Clone, Copy)]
pub struct Scroll {
    pub y: u16,
    pub height: u16,
    pub first: usize,
    pub total: usize,
}

/// The scroll thumb in column `x` -- the frame's right edge -- where the
/// lines in view sit among all of them. It takes no line of the list.
pub fn scrollbar(f: &mut Frame<'_>, x: u16, s: Scroll, t: Theme) {
    let height = s.height as usize;
    if height == 0 || s.total <= height {
        return;
    }
    let thumb = (height * height / s.total).max(1);
    let top = s.first.min(s.total - height) * (height - thumb) / (s.total - height);
    for dy in top..(top + thumb).min(height) {
        chrome::text(f, Rect::new(x, s.y + dy as u16, 1, 1), "┃", Tone::Muted, t);
    }
}

/// Columns before a row's label: the focus bar and a space.
const BAR: u16 = 2;
/// Columns between the longest label and the value column.
const GAP: u16 = 3;
/// The widest a label column grows; a longer label is cut.
const LABEL_MAX: u16 = 32;
/// Columns between two section names, and between two switches.
const SPREAD: u16 = 3;

/// Where a sheet's columns stand, worked out from all its rows at once so
/// every value on the sheet starts in one column.
struct Grid {
    /// Columns for the `●` of the current choice, when the sheet has
    /// choices.
    mark: u16,
    /// The value column, from the row's left edge, when any row has a value.
    value: Option<u16>,
    /// Where a value's words start inside the value column: past a chip's
    /// bracket, when the sheet has rows of values, so words and chips line up.
    pad: u16,
}

/// Whether a row puts something in the value column.
fn valued(item: &Item) -> bool {
    matches!(
        item.kind,
        Kind::Value { .. } | Kind::Toggle(_) | Kind::Field(_)
    ) || item.value.is_some()
}

/// A row's label as drawn: its key, its name, and `›` or `▲` after it.
fn label_text(item: &Item) -> String {
    let mut label = match item.key {
        Some(k) => format!("{k} · {}", item.title),
        None => item.title.clone(),
    };
    match item.kind {
        Kind::Open if item.value.is_none() => label.push_str(" ›"),
        Kind::Danger => label.push_str(" ▲"),
        _ => {}
    }
    label
}

fn swatch_width(item: &Item) -> u16 {
    if item.swatch.is_some() { 3 } else { 0 }
}

fn grid(sheet: &Sheet) -> Grid {
    let rows = || {
        sheet
            .items
            .iter()
            .filter(|item| !item.inline && item.kind != Kind::Heading)
    };
    let mark = if rows().any(|item| matches!(item.kind, Kind::Choice { .. })) {
        2
    } else {
        0
    };
    let widest = rows()
        .filter(|item| valued(item))
        .map(|item| (chrome::width(&label_text(item)) + swatch_width(item)).min(LABEL_MAX))
        .max();
    let pad = if rows().any(|item| matches!(item.kind, Kind::Value { .. } | Kind::Toggle(_))) {
        2
    } else {
        0
    };
    Grid {
        mark,
        value: widest.map(|w| BAR + mark + w + GAP),
        pad,
    }
}

/// Where plain text starts: in the label column when the sheet has rows, so
/// a sentence lines up with the names above and below it.
fn text_indent(sheet: &Sheet, grid: &Grid) -> u16 {
    if sheet.items.iter().any(Item::focusable) {
        BAR + grid.mark
    } else {
        0
    }
}

/// The title line: `TITLE › crumb`.
fn heading_text(sheet: &Sheet) -> String {
    let mut heading = sheet.title.to_uppercase();
    for crumb in &sheet.crumbs {
        heading.push_str(" › ");
        heading.push_str(crumb);
    }
    heading
}

fn esc_text(sheet: &Sheet) -> String {
    match &sheet.esc {
        Some(esc) => format!("Esc · {esc}"),
        None if sheet.root => "Esc · Close".to_string(),
        None => "Esc · Back".to_string(),
    }
}

/// The columns a row of words takes, each as wide as its chip and one
/// apart.
fn words_width<'a>(words: impl Iterator<Item = &'a String>) -> u16 {
    words
        .map(|word| chrome::width(word) + 5)
        .sum::<u16>()
        .saturating_sub(1)
}

fn tools_width(sheet: &Sheet) -> u16 {
    let each: Vec<u16> = sheet
        .tools
        .iter()
        .map(|tool| words_width(tool.options.iter().map(|(label, _)| label)))
        .collect();
    each.iter().sum::<u16>() + SPREAD * (each.len() as u16).saturating_sub(1)
}

/// Whether the switches fit on the title line, between the title and the
/// Esc chip; otherwise they take a line of their own under it.
fn tools_on_title(sheet: &Sheet, width: u16) -> bool {
    !sheet.tools.is_empty()
        && chrome::width(&heading_text(sheet))
            + SPREAD
            + tools_width(sheet)
            + SPREAD
            + chrome::width(&esc_text(sheet))
            + 4
            <= width
}

/// How a row of words shows the current one.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Current {
    /// A value: the one filled chip on the row.
    Filled,
    /// A switch over the whole sheet, read like a section: in the accent.
    Accent,
}

/// A row of words, the current one marked and the others plain, quiet
/// unless the row has the focus. The one under the pointer or a finger is a
/// chip. `false`, and nothing drawn, when they do not fit.
#[allow(clippy::too_many_arguments)]
fn words(
    f: &mut Frame<'_>,
    g: &mut Geometry,
    x: u16,
    y: u16,
    limit: u16,
    words: &[String],
    (current, shown): (Option<usize>, Current),
    quiet: bool,
    hit: impl Fn(usize) -> Action,
    t: Theme,
    press: Option<(u16, u16)>,
    hover: Option<(u16, u16)>,
) -> bool {
    if x + words_width(words.iter()) > limit {
        return false;
    }
    let mut x = x;
    for (i, word) in words.iter().enumerate() {
        let w = chrome::width(word) + 4;
        let r = Rect::new(x, y, w, 1);
        let under = |at: Option<(u16, u16)>| at.is_some_and(|(px, py)| super::contains(r, px, py));
        let filled = Some(i) == current && shown == Current::Filled;
        if filled || under(hover) || under(press) {
            chrome::chip(
                f,
                g,
                x,
                y,
                limit,
                word,
                hit(i),
                filled,
                Tone::Normal,
                press,
                t,
            );
        } else {
            let tone = if Some(i) == current {
                Tone::Accent
            } else if quiet {
                Tone::Muted
            } else {
                Tone::Normal
            };
            chrome::text(f, r, &format!("  {word}  "), tone, t);
            g.hits.push((r, hit(i)));
        }
        x += w + 1;
    }
    true
}

/// Draws a sheet into `area`: the title line with its switches and its Esc
/// chip, the sections and the search, the list, the strip that says what the
/// focused row means, and the foot -- the notice on the left, cut before it
/// reaches the hint on the right.
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
            scroll: None,
        };
    }
    // The title line: TITLE › crumb, the switches, and the chip that says
    // what Esc does.
    let back = esc_text(sheet);
    let back_w = chrome::width(&back) + 4;
    let inline_tools = tools_on_title(sheet, area.width);
    let tools_w = tools_width(sheet);
    let room = area
        .width
        .saturating_sub(back_w + 1)
        .saturating_sub(if inline_tools { tools_w + SPREAD } else { 0 });
    chrome::text(
        f,
        Rect::new(area.x, y, room, 1),
        &clip(&heading_text(sheet), room as usize),
        Tone::Accent,
        t,
    );
    if area.width > back_w + 8 {
        chrome::chip(
            f,
            g,
            area.right().saturating_sub(back_w),
            y,
            area.right(),
            &back,
            Action::Sheet(Hit::Back),
            false,
            Tone::Normal,
            press,
            t,
        );
    }
    let draw_tools = |f: &mut Frame<'_>, g: &mut Geometry, x: u16, y: u16, limit: u16| {
        let mut x = x;
        for (ti, tool) in sheet.tools.iter().enumerate() {
            let labels: Vec<String> = tool.options.iter().map(|(l, _)| l.clone()).collect();
            let w = words_width(labels.iter());
            if !words(
                f,
                g,
                x,
                y,
                limit,
                &labels,
                (tool.current, Current::Accent),
                false,
                |o| Action::Sheet(Hit::Tool(ti, o)),
                t,
                press,
                hover,
            ) {
                break;
            }
            x += w + SPREAD;
        }
    };
    if inline_tools {
        let x = area.right().saturating_sub(back_w + SPREAD + tools_w);
        draw_tools(f, g, x, y, area.right().saturating_sub(back_w + 1));
    }
    y += 1;
    if !sheet.tools.is_empty() && !inline_tools && y < bottom {
        draw_tools(f, g, area.x, y, area.right());
        y += 1;
    }
    // The sections as words, the open one in the accent, and the search at
    // the right end of the same line. Sections that do not fit fold into
    // `+N ▾`, never the open one.
    let search = sheet.query.as_ref().map(|query| {
        let count = match sheet.total {
            Some(total) => format!(
                " · {} of {total}",
                sheet.matched.unwrap_or_else(|| sheet
                    .items
                    .iter()
                    .filter(|i| i.focusable())
                    .count())
            ),
            None => String::new(),
        };
        if query.is_empty() {
            format!("⌕ type to filter{count}")
        } else {
            format!("⌕ {query}▏{count}")
        }
    });
    let mut open: Option<(u16, u16)> = None;
    if !sheet.sections.is_empty() || search.is_some() {
        let search_w = search.as_deref().map_or(0, chrome::width);
        let limit = area.right().saturating_sub(search_w + 2);
        if sheet.sections.len() > 1 {
            let widths: Vec<u16> = sheet
                .sections
                .iter()
                .map(|name| chrome::width(name) + SPREAD)
                .collect();
            let room = limit.saturating_sub(area.x) + SPREAD;
            let shown = chrome::fitting(&widths, room, &[sheet.section]);
            let mut x = area.x;
            for i in shown.iter().copied() {
                let name = &sheet.sections[i];
                let w = chrome::width(name);
                if x + w > limit {
                    break;
                }
                let tone = if i == sheet.section {
                    Tone::Accent
                } else {
                    Tone::Normal
                };
                chrome::text(f, Rect::new(x, y, w, 1), name, tone, t);
                hit(g, Rect::new(x, y, w, 1), Hit::Section(i));
                if i == sheet.section {
                    open = Some((x, w));
                }
                x += w + SPREAD;
            }
            let folded: Vec<(String, Action)> = (0..sheet.sections.len())
                .filter(|i| !shown.contains(i))
                .map(|i| (sheet.sections[i].clone(), Action::Sheet(Hit::Section(i))))
                .collect();
            if !folded.is_empty() {
                let more = format!("+{} ▾", folded.len());
                let w = chrome::width(&more);
                if x + w <= limit {
                    chrome::text(f, Rect::new(x, y, w, 1), &more, Tone::Normal, t);
                    g.hits.push((Rect::new(x, y, w, 1), Action::More(folded)));
                }
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
    // The rule under the head, drawn heavy in the accent under the open
    // section.
    if y < bottom {
        chrome::rule(f, Rect::new(area.x, y, area.width, 1), &[], Tone::Line, t);
        if let Some((x, w)) = open {
            chrome::text(
                f,
                Rect::new(x, y, w, 1),
                &"━".repeat(w as usize),
                Tone::Accent,
                t,
            );
        }
        y += 1;
    }
    // The foot takes the last two lines when there is room for them.
    let foot = bottom.saturating_sub(2);
    let has_foot = foot > y + 1;
    let mut body_bottom = if has_foot { foot } else { bottom };
    // The strip keeps one height whichever row is focused, so the list never
    // jumps as the focus moves; it gives way to the list on a short screen.
    let strip_h = strip_height(sheet, area.width).min(body_bottom.saturating_sub(y + 3));
    if strip_h >= 3 {
        body_bottom -= strip_h;
        draw_strip(
            f,
            Rect::new(area.x, body_bottom, area.width, strip_h),
            sheet.focused(),
            t,
        );
    }
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
    let scroll = draw_body(f, g, body, sheet, t, press, hover);
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
        let (said, tone) = if sheet.notice.is_empty() {
            (&sheet.status, Tone::Muted)
        } else {
            (&sheet.notice, Tone::Accent)
        };
        if !said.is_empty() && room > 1 {
            let said = clip(said, room);
            let w = chrome::width(&said);
            chrome::text(f, Rect::new(area.x, foot + 1, w, 1), &said, tone, t);
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
    Drawn {
        body,
        aside,
        scroll,
    }
}

/// The rows a sheet needs to show everything without scrolling, `width`
/// wide inside its frame: the head, every body line, the strip and the
/// foot. The surface is made no taller than this.
#[must_use]
pub fn wanted_height(sheet: &Sheet, width: u16) -> u16 {
    let head = 1
        + u16::from(!sheet.tools.is_empty() && !tools_on_title(sheet, width))
        + u16::from(!sheet.sections.is_empty() || sheet.query.is_some())
        + 1;
    let list_width = width.saturating_sub(sheet.aside);
    let body = layout_lines(sheet, list_width as usize).len() as u16;
    head + body.max(1) + strip_height(sheet, width) + 2
}

/// The strip's words for a row: what it means, then why it cannot be used.
fn strip_lines(item: &Item, width: u16) -> Vec<(String, Tone)> {
    let room = (width as usize).max(8);
    let mut lines: Vec<(String, Tone)> = super::view::wrap_words(&item.detail, room)
        .into_iter()
        .map(|line| (line, Tone::Normal))
        .collect();
    if let Some(reason) = &item.disabled {
        lines.extend(
            super::view::wrap_words(reason, room)
                .into_iter()
                .map(|line| (line, Tone::Warning)),
        );
    }
    lines
}

/// The strip's height on this sheet: its rule and the name line, and the
/// most words any focusable row has, up to four lines; nothing when no row
/// says anything.
fn strip_height(sheet: &Sheet, width: u16) -> u16 {
    let rows = || sheet.items.iter().filter(|item| item.focusable());
    let most = rows()
        .map(|item| strip_lines(item, width).len())
        .max()
        .unwrap_or(0);
    if most == 0 && !rows().any(|item| item.card.is_some()) {
        0
    } else {
        most.min(4) as u16 + 2
    }
}

/// The strip under the list: the focused row's name, when a change applies
/// and where its value comes from on the same line, and what it means under
/// them.
fn draw_strip(f: &mut Frame<'_>, r: Rect, item: Option<&Item>, t: Theme) {
    chrome::rule(f, Rect::new(r.x, r.y, r.width, 1), &[], Tone::Line, t);
    let Some(item) = item else {
        return;
    };
    let edges = item
        .card
        .as_ref()
        .map(|card| {
            [card.when.as_str(), card.source.as_str()]
                .into_iter()
                .filter(|part| !part.is_empty())
                .collect::<Vec<_>>()
                .join(" · ")
        })
        .unwrap_or_default();
    let edges = clip(&edges, (r.width / 2) as usize);
    let edges_w = chrome::width(&edges);
    if r.height > 1 {
        let name_room = r.width.saturating_sub(edges_w + 2);
        chrome::text(
            f,
            Rect::new(r.x, r.y + 1, name_room, 1),
            &clip(&item.title, name_room as usize),
            Tone::Strong,
            t,
        );
        chrome::text(
            f,
            Rect::new(r.right().saturating_sub(edges_w), r.y + 1, edges_w, 1),
            &edges,
            Tone::Muted,
            t,
        );
    }
    for (row, (line, tone)) in strip_lines(item, r.width)
        .into_iter()
        .take(r.height.saturating_sub(2) as usize)
        .enumerate()
    {
        chrome::text(
            f,
            Rect::new(r.x, r.y + 2 + row as u16, r.width, 1),
            &line,
            tone,
            t,
        );
    }
}

/// One drawn line of the body.
enum Line {
    /// The empty line above a group.
    Blank,
    /// A group's name and the chips at the end of its line.
    Heading(usize, Vec<usize>),
    /// A row and the chips at the end of its line.
    Row(usize, Vec<usize>),
    /// A run of inline items drawn as chips on one line.
    Chips(Vec<usize>),
    /// A line of an Info row's text.
    Text(usize, String),
}

fn layout_lines(sheet: &Sheet, width: usize) -> Vec<Line> {
    let grid = grid(sheet);
    let indent = text_indent(sheet, &grid) as usize;
    let mut lines: Vec<Line> = Vec::new();
    let mut i = 0;
    while i < sheet.items.len() {
        let item = &sheet.items[i];
        if item.trail
            && let Some(Line::Row(_, run) | Line::Heading(_, run)) = lines.last_mut()
        {
            run.push(i);
            i += 1;
            continue;
        }
        if item.inline {
            // A run of chips wraps to as many lines as it needs: an answer
            // pushed past the edge is an answer nobody can see.
            let chip_width = |item: &Item| {
                let key = item.key.map_or(0, |_| 4);
                // Room for the focus mark too, so focus never rewraps.
                chrome::width(&item.title) as usize + key + 2 + 5
            };
            let mut run = Vec::new();
            let mut used = BAR as usize;
            while i < sheet.items.len() && sheet.items[i].inline {
                let w = chip_width(&sheet.items[i]);
                if !run.is_empty() && used + w > width {
                    lines.push(Line::Chips(std::mem::take(&mut run)));
                    used = BAR as usize;
                }
                run.push(i);
                used += w;
                i += 1;
            }
            lines.push(Line::Chips(run));
            continue;
        }
        match item.kind {
            Kind::Heading => {
                if !matches!(lines.last(), Some(Line::Blank)) {
                    lines.push(Line::Blank);
                }
                lines.push(Line::Heading(i, Vec::new()));
            }
            // Text with a chip after it is one line: the name the chip acts
            // on, `api.example.com   ⟨ Remove ⟩`.
            Kind::Info
                if item.value.is_none()
                    && !sheet.items.get(i + 1).is_some_and(|next| next.trail) =>
            {
                // What a person approves is shown exactly: code keeps every
                // character, and other text keeps the spaces it was laid
                // out with.
                let room = width.saturating_sub(indent).max(8);
                let wrapped = if item.tone == Tone::Code {
                    super::view::wrap_exact(&item.title, room)
                } else {
                    super::view::wrap_spaced(&item.title, room)
                };
                if wrapped.is_empty() {
                    lines.push(Line::Text(i, String::new()));
                }
                for line in wrapped {
                    lines.push(Line::Text(i, line));
                }
            }
            _ => lines.push(Line::Row(i, Vec::new())),
        }
        i += 1;
    }
    lines
}

/// Every row a line holds: its own and the chips at its end.
fn line_items(line: &Line) -> Vec<usize> {
    match line {
        Line::Blank => Vec::new(),
        Line::Text(i, _) => vec![*i],
        Line::Chips(run) => run.clone(),
        Line::Row(i, trail) | Line::Heading(i, trail) => {
            std::iter::once(*i).chain(trail.iter().copied()).collect()
        }
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
) -> Option<Scroll> {
    let lines = layout_lines(sheet, body.width as usize);
    let height = body.height as usize;
    sheet.lines = lines.len();
    let mut starts = vec![None; sheet.items.len()];
    for (n, line) in lines.iter().enumerate() {
        for i in line_items(line) {
            if let Some(start) = starts.get_mut(i) {
                start.get_or_insert(n);
            }
        }
    }
    sheet.item_lines = starts;
    sheet.page = height.max(1);
    if sheet.refocus_on_search {
        sheet.refocus_on_search = false;
        sheet.focus = sheet.items.iter().position(Item::focusable).unwrap_or(0);
        sheet.follow = true;
    }
    // Keep the focused row in view after a key moved it, with the group
    // name above it; the wheel alone leaves it wherever it went.
    if sheet.follow {
        sheet.follow = false;
        let holds = |l: &Line| line_items(l).contains(&sheet.focus);
        let first = lines.iter().position(holds);
        let last = lines.iter().rposition(holds);
        if let (Some(first), Some(last)) = (first, last) {
            let visible = height.max(1);
            if first < sheet.scroll {
                let lead = lines[..first]
                    .iter()
                    .rev()
                    .take(2)
                    .take_while(|l| matches!(l, Line::Blank | Line::Heading(..)))
                    .count();
                sheet.scroll = first - lead;
            } else if last >= sheet.scroll + visible {
                sheet.scroll = (last + 1).saturating_sub(visible);
            }
        }
    }
    sheet.scroll = sheet.scroll.min(lines.len().saturating_sub(height));
    let grid = grid(sheet);
    for (j, line) in lines.iter().skip(sheet.scroll).take(height).enumerate() {
        let r = Rect::new(body.x, body.y + j as u16, body.width, 1);
        draw_line(f, g, r, line, sheet, &grid, t, press, hover);
    }
    (lines.len() > height).then_some(Scroll {
        y: body.y,
        height: body.height,
        first: sheet.scroll,
        total: lines.len(),
    })
}

/// A chip's words and tone: its letter, and `›` while it has the focus.
fn chip_of(item: &Item, focused: bool) -> (String, Tone) {
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
    } else if item.kind == Kind::Danger || item.tone == Tone::Warning {
        Tone::Warning
    } else {
        Tone::Normal
    };
    (label, tone)
}

/// The chips at the end of a line, right-aligned; returns the column they
/// start at.
fn draw_trail(
    f: &mut Frame<'_>,
    g: &mut Geometry,
    r: Rect,
    trail: &[usize],
    sheet: &Sheet,
    t: Theme,
    press: Option<(u16, u16)>,
) -> u16 {
    let chips: Vec<(String, Tone, usize)> = trail
        .iter()
        .map(|i| {
            let (label, tone) = chip_of(&sheet.items[*i], *i == sheet.focus);
            (label, tone, *i)
        })
        .collect();
    let total: u16 = chips
        .iter()
        .map(|(label, ..)| chrome::width(label) + 5)
        .sum::<u16>()
        .saturating_sub(1);
    if chips.is_empty() || total + 8 > r.width {
        return r.right();
    }
    let start = r.right() - total;
    let mut x = start;
    for (label, tone, i) in chips {
        let w = chrome::chip(
            f,
            g,
            x,
            r.y,
            r.right(),
            &label,
            Action::Sheet(Hit::Item(i)),
            i == sheet.focus,
            tone,
            press,
            t,
        );
        x += w + 1;
    }
    start
}

#[allow(clippy::too_many_arguments)]
fn draw_line(
    f: &mut Frame<'_>,
    g: &mut Geometry,
    r: Rect,
    line: &Line,
    sheet: &Sheet,
    grid: &Grid,
    t: Theme,
    press: Option<(u16, u16)>,
    hover: Option<(u16, u16)>,
) {
    match line {
        Line::Blank => {}
        Line::Text(i, text) => {
            let x = r.x + text_indent(sheet, grid);
            let item = &sheet.items[*i];
            chrome::text(
                f,
                Rect::new(x, r.y, r.right().saturating_sub(x), 1),
                text,
                item.tone,
                t,
            );
        }
        Line::Heading(i, trail) => {
            let end = draw_trail(f, g, r, trail, sheet, t, press);
            let name = &sheet.items[*i].title;
            let room = end.saturating_sub(r.x + 1);
            let name = clip(name, room as usize);
            let w = chrome::width(&name);
            chrome::text(f, Rect::new(r.x, r.y, w, 1), &name, Tone::Strong, t);
            let x = r.x + w + 1;
            let fill = end.saturating_sub(x + u16::from(end < r.right()));
            chrome::text(
                f,
                Rect::new(x, r.y, fill, 1),
                &"─".repeat(fill as usize),
                Tone::Line,
                t,
            );
        }
        Line::Chips(run) => {
            let mut x = r.x + BAR;
            for i in run {
                let (label, tone) = chip_of(&sheet.items[*i], *i == sheet.focus);
                let w = chrome::chip(
                    f,
                    g,
                    x,
                    r.y,
                    r.right(),
                    &label,
                    Action::Sheet(Hit::Item(*i)),
                    *i == sheet.focus,
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
        Line::Row(i, trail) => {
            let end = draw_trail(f, g, r, trail, sheet, t, press);
            let width = if end < r.right() {
                end.saturating_sub(r.x + 1)
            } else {
                r.width
            };
            draw_row(
                f,
                g,
                Rect::new(r.x, r.y, width, 1),
                *i,
                sheet,
                grid,
                t,
                press,
                hover,
            );
        }
    }
}

/// One row on the grid: the focus bar, the `●` of the current choice, the
/// label, and in the value column what the row holds.
#[allow(clippy::too_many_arguments)]
fn draw_row(
    f: &mut Frame<'_>,
    g: &mut Geometry,
    r: Rect,
    i: usize,
    sheet: &Sheet,
    grid: &Grid,
    t: Theme,
    press: Option<(u16, u16)>,
    hover: Option<(u16, u16)>,
) {
    let item = &sheet.items[i];
    let focused = i == sheet.focus && item.focusable();
    if focused {
        chrome::text(f, Rect::new(r.x, r.y, 1, 1), "▌", Tone::Accent, t);
    }
    let mut x = r.x + BAR;
    if grid.mark > 0 && item.is_current() {
        chrome::text(f, Rect::new(x, r.y, 1, 1), "●", Tone::Accent, t);
    }
    x += grid.mark;
    if let Some(ink) = item.swatch {
        chrome::text(f, Rect::new(x, r.y, 2, 1), "██", ink, t);
        x += 3;
    }
    let tone = if item.disabled.is_some() {
        Tone::Muted
    } else if focused {
        Tone::Accent
    } else if item.kind == Kind::Danger {
        Tone::Warning
    } else if item.kind == Kind::Info {
        Tone::Normal
    } else {
        item.tone
    };
    let value_x = grid
        .value
        .filter(|_| valued(item))
        .map(|v| r.x + v)
        .filter(|v| *v < r.right());
    let label_end = value_x.map_or(r.right(), |v| v.saturating_sub(1));
    let label = clip(&label_text(item), label_end.saturating_sub(x) as usize);
    chrome::text(
        f,
        Rect::new(x, r.y, label_end.saturating_sub(x), 1),
        &label,
        tone,
        t,
    );
    // The label is the row's target; a row whose value column holds its own
    // targets keeps them, and a row with only words there is one target.
    let own_targets = matches!(item.kind, Kind::Value { .. } | Kind::Toggle(_));
    if item.focusable() {
        let end = if own_targets {
            value_x.unwrap_or(r.right())
        } else {
            r.right()
        };
        g.hits.push((
            Rect::new(r.x, r.y, end.saturating_sub(r.x), 1),
            Action::Sheet(Hit::Item(i)),
        ));
    }
    let Some(vx) = value_x else {
        return;
    };
    let quiet = !focused || item.disabled.is_some();
    match &item.kind {
        Kind::Value { values, current } => {
            let labels: Vec<String> = values.iter().map(|(label, _)| label.clone()).collect();
            let fits = words(
                f,
                g,
                vx,
                r.y,
                r.right(),
                &labels,
                (*current, Current::Filled),
                quiet,
                |v| Action::Sheet(Hit::Value(i, v)),
                t,
                press,
                hover,
            );
            if !fits {
                // Too many to show in a row: the current one, and the whole
                // list one click away.
                let shown = current
                    .and_then(|at| labels.get(at).cloned())
                    .unwrap_or_else(|| "choose".into());
                let said = format!("{shown} ▾");
                let x = vx + grid.pad;
                let w = chrome::width(&said).min(r.right().saturating_sub(x));
                chrome::text(f, Rect::new(x, r.y, w, 1), &said, Tone::Normal, t);
                g.hits
                    .push((Rect::new(x, r.y, w, 1), Action::Sheet(Hit::Fold(i))));
            }
        }
        Kind::Toggle(on) => {
            words(
                f,
                g,
                vx,
                r.y,
                r.right(),
                &["Off".to_string(), "On".to_string()],
                (Some(usize::from(*on)), Current::Filled),
                quiet,
                |v| Action::Sheet(Hit::Value(i, v)),
                t,
                press,
                hover,
            );
        }
        Kind::Field(field) => {
            let shown = if field.secret {
                "•".repeat(field.text.chars().count())
            } else {
                field.text.clone()
            };
            let x = vx + 2;
            let room = r.right().saturating_sub(x + 1) as usize;
            let ink = if focused { Tone::Accent } else { Tone::Line };
            chrome::text(f, Rect::new(vx, r.y, 1, 1), "┃", ink, t);
            let shown = clip(&shown, room);
            if focused && field.fresh && !shown.is_empty() {
                // Selected: the next key or paste replaces all of it.
                f.render_widget(
                    ratatui::widgets::Paragraph::new(shown.clone()).style(theme::chip_on(t)),
                    Rect::new(x, r.y, chrome::width(&shown), 1).intersection(f.area()),
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
                    Rect::new(x, r.y, r.right().saturating_sub(x), 1),
                    &format!("{before}{caret}{after}"),
                    if focused { Tone::Strong } else { Tone::Normal },
                    t,
                );
            }
        }
        kind => {
            let Some(value) = &item.value else {
                return;
            };
            let said = if *kind == Kind::Open {
                format!("{value} ›")
            } else {
                value.clone()
            };
            let x = vx + grid.pad;
            let room = r.right().saturating_sub(x);
            // A value is what the row holds; words beside a choice or an
            // action say what it does, and stay quiet.
            let tone = match kind {
                _ if item.disabled.is_some() => Tone::Muted,
                Kind::Info => item.tone,
                Kind::Open => Tone::Normal,
                _ if focused => Tone::Normal,
                _ => Tone::Muted,
            };
            chrome::text(
                f,
                Rect::new(x, r.y, room, 1),
                &clip(&said, room as usize),
                tone,
                t,
            );
        }
    }
}

#[cfg(test)]
mod tests {
    /// A label after Enter or Esc keeps a name's capital and an acronym.
    #[test]
    fn a_label_keeps_its_names() {
        assert_eq!(
            super::sentence_case("Let Sterna decide"),
            "let Sterna decide"
        );
        assert_eq!(super::sentence_case("Not now"), "not now");
        assert_eq!(super::sentence_case("MCP servers"), "MCP servers");
    }

    use super::*;

    /// A sheet never opens on a row that cannot be taken back, even when it
    /// is the first row: a reflexive Enter must not be the dangerous one.
    #[test]
    fn a_sheet_never_opens_on_a_danger_row() {
        let mut sheet = Sheet::new("Confirm");
        sheet.set_items(vec![
            Item::danger(
                "yes",
                "Yes · Full access",
                Action::ConfirmLevel("full".into()),
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
