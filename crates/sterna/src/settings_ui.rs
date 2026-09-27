//! Themed Global/Project settings panel.
//!
//! **Backend-independent.** This module never reads or writes a settings
//! file, never validates a value against a schema, and never imports the
//! project/config stores. It accepts already-resolved [`Row`]s (effective
//! value, origin, valid choices) and hands back only the edits a person
//! staged, as `(key, Option<value>)` pairs -- `None` means "remove the
//! override at this scope, use the inherited value". The caller owns
//! reading rows, validating staged edits, writing them, and reconstructing
//! a fresh panel afterward (on Apply, on Cancel, and on every scope switch:
//! this panel never mutates its own `Row`s to reflect a save).
//!
//! The palette comes from Sterna's existing theme, not a second settings theme.

use std::collections::BTreeMap;

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};

use crate::tui::Theme;

/// One editable preference as the caller resolved it: what it is worth now,
/// where that came from, and the bounded choices (empty means free text --
/// used only for values like a model ID with no curated list).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    pub key: String,
    pub label: String,
    pub description: String,
    pub value: String,
    pub origin: String,
    pub choices: Vec<String>,
    pub restart: bool,
}

/// What a keystroke asked the caller to do. Every other keystroke is fully
/// handled inside the panel and returns `None`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    None,
    /// Tab requested the other scope tab; only returned when nothing is
    /// staged, so the caller can rebuild the panel without losing an edit.
    SwitchScope(usize),
    /// Ctrl-S with staged edits: read them with [`SettingsPanel::edits`],
    /// validate and persist, then replace this panel with a fresh one.
    Apply,
    /// Esc with nothing left staged (any staged edits were just discarded
    /// in the same keystroke): close the panel.
    Cancel,
}

const SCOPES: [&str; 2] = ["Global", "Project"];
const INHERIT_LABEL: &str = "(use inherited value)";

/// Owns the rows for one open scope tab, the in-progress free-text buffer
/// (if a row without choices is being typed into), and edits staged but not
/// yet applied.
#[derive(Debug)]
pub struct SettingsPanel {
    scope: usize,
    path: String,
    rows: Vec<Row>,
    /// Row index -> staged edit. `Some(value)` overrides; `Some` wrapping
    /// nothing never appears -- `None` means "unset the override".
    edits: BTreeMap<usize, Option<String>>,
    selected: usize,
    /// `Some` while a free-text row is open for editing; holds the buffer.
    editing: Option<String>,
    replace_on_type: bool,
    notice: Option<String>,
}

impl SettingsPanel {
    #[must_use]
    pub fn new(scope: usize, path: String, rows: Vec<Row>) -> Self {
        Self {
            scope,
            path,
            rows,
            edits: BTreeMap::new(),
            selected: 0,
            editing: None,
            replace_on_type: false,
            notice: None,
        }
    }

    #[must_use]
    pub fn is_dirty(&self) -> bool {
        !self.edits.is_empty()
    }

    /// Staged edits in row order, each key at most once.
    #[must_use]
    pub fn edits(&self) -> Vec<(String, Option<String>)> {
        self.edits
            .iter()
            .map(|(&i, value)| (self.rows[i].key.clone(), value.clone()))
            .collect()
    }

    /// Surfaces a message from the caller (a save result, a validation
    /// rejection) in the panel's footer on the next render.
    pub fn notice(&mut self, text: String) {
        self.notice = Some(text);
    }

    pub fn key(&mut self, key: KeyEvent) -> Action {
        if key.kind == KeyEventKind::Release {
            return Action::None;
        }
        if self.editing.is_some() {
            self.key_editing(key)
        } else {
            self.key_browsing(key)
        }
    }

    fn key_editing(&mut self, key: KeyEvent) -> Action {
        match key.code {
            KeyCode::Esc => self.editing = None,
            KeyCode::Enter => {
                if let Some(value) = self.editing.take() {
                    self.stage_value(value);
                }
            }
            KeyCode::Backspace => {
                self.replace_on_type = false;
                if let Some(buffer) = self.editing.as_mut() {
                    buffer.pop();
                }
            }
            KeyCode::Char(c)
                if !key.modifiers.contains(KeyModifiers::CONTROL) && !c.is_control() =>
            {
                if let Some(buffer) = self.editing.as_mut() {
                    if self.replace_on_type {
                        buffer.clear();
                        self.replace_on_type = false;
                    }
                    buffer.push(c);
                }
            }
            _ => {}
        }
        Action::None
    }

    fn key_browsing(&mut self, key: KeyEvent) -> Action {
        match key.code {
            KeyCode::Up => {
                self.selected = self.selected.saturating_sub(1);
                Action::None
            }
            KeyCode::Down => {
                if self.selected + 1 < self.rows.len() {
                    self.selected += 1;
                }
                Action::None
            }
            KeyCode::Left => {
                self.cycle(false);
                Action::None
            }
            KeyCode::Right => {
                self.cycle(true);
                Action::None
            }
            KeyCode::Enter => {
                match self.rows.get(self.selected) {
                    Some(row) if row.choices.is_empty() => {
                        let start = match self.edits.get(&self.selected) {
                            Some(Some(value)) => value.clone(),
                            Some(None) | None => row.value.clone(),
                        };
                        self.editing = Some(if start == "unset" {
                            String::new()
                        } else {
                            start
                        });
                        self.replace_on_type = true;
                    }
                    Some(_) => self.cycle(true),
                    None => {}
                }
                Action::None
            }
            KeyCode::Backspace => {
                self.reset_selected();
                Action::None
            }
            KeyCode::Tab => {
                if self.is_dirty() {
                    self.notice =
                        Some("Apply or Cancel staged changes before switching tabs".into());
                    Action::None
                } else {
                    Action::SwitchScope((self.scope + 1) % 2)
                }
            }
            KeyCode::Char('s' | 'S') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                if self.is_dirty() {
                    Action::Apply
                } else {
                    self.notice = Some("Nothing to apply".into());
                    Action::None
                }
            }
            KeyCode::Esc => {
                if self.is_dirty() {
                    self.edits.clear();
                    self.notice = Some("Discarded staged changes".into());
                }
                Action::Cancel
            }
            _ => Action::None,
        }
    }

    /// Records `value` as the row's staged edit, or clears any staged edit
    /// when it matches the row's own effective value -- so undoing a change
    /// by hand is indistinguishable from never having made it.
    fn stage_value(&mut self, value: String) {
        let Some(row) = self.rows.get(self.selected) else {
            return;
        };
        if value == row.value {
            self.edits.remove(&self.selected);
        } else {
            self.edits.insert(self.selected, Some(value));
        }
    }

    fn cycle(&mut self, forward: bool) {
        let Some(row) = self.rows.get(self.selected) else {
            return;
        };
        if row.choices.is_empty() {
            return;
        }
        let current = match self.edits.get(&self.selected) {
            Some(Some(value)) => value.clone(),
            Some(None) | None => row.value.clone(),
        };
        let n = row.choices.len();
        let next = match row.choices.iter().position(|c| *c == current) {
            Some(at) if forward => (at + 1) % n,
            Some(at) => (at + n - 1) % n,
            None if forward => 0,
            None => n - 1,
        };
        self.stage_value(row.choices[next].clone());
    }

    /// "Use inherited value": only meaningful when this scope itself holds
    /// an override. A first press on such a row stages the unset; pressing
    /// it again on a row with any other staged edit un-stages that edit
    /// instead. On a row this scope never overrode, it is a no-op that says
    /// so, rather than staging a pointless unset.
    fn reset_selected(&mut self) {
        let Some(row) = self.rows.get(self.selected) else {
            return;
        };
        let scope_label = SCOPES[self.scope % 2];
        if self.edits.remove(&self.selected).is_some() {
            self.notice = Some(format!("{} reverted", row.label));
        } else if row.origin.eq_ignore_ascii_case(scope_label) {
            self.edits.insert(self.selected, None);
            self.notice = Some(format!("{} will use the inherited value", row.label));
        } else {
            self.notice = Some(format!("{} already uses the inherited value", row.label));
        }
    }

    fn display_value(&self, index: usize) -> String {
        match self.edits.get(&index) {
            Some(Some(value)) => value.clone(),
            Some(None) => INHERIT_LABEL.to_string(),
            None => self.rows[index].value.clone(),
        }
    }

    pub fn render(&self, frame: &mut Frame, theme: Theme) {
        let area = frame.area();
        let width = area.width.saturating_sub(4).min(140);
        let height = area.height.saturating_sub(2).min(36);
        let overlay = Rect::new(
            area.x + area.width.saturating_sub(width) / 2,
            area.y + area.height.saturating_sub(height) / 2,
            width,
            height,
        );
        if overlay.width == 0 || overlay.height == 0 {
            return;
        }
        frame.render_widget(Clear, overlay);
        let accent_style = Style::default().fg(accent(theme));
        let muted_style = Style::default().fg(Color::DarkGray);
        let scope_label = SCOPES[self.scope % 2];
        let block = Block::default()
            .title(format!(" Settings — {scope_label} "))
            .borders(Borders::ALL)
            .border_style(accent_style);
        let inner = block.inner(overlay);
        frame.render_widget(block, overlay);
        if inner.height == 0 || inner.width == 0 {
            return;
        }

        let mut cursor = inner;
        let tabs = take_line(&mut cursor);
        self.render_tabs(frame, tabs, theme, muted_style);
        let path_height = cursor.height.saturating_sub(1).min(2);
        let path_area = Rect::new(cursor.x, cursor.y, cursor.width, path_height);
        frame.render_widget(
            Paragraph::new(self.path.as_str())
                .wrap(Wrap { trim: false })
                .style(muted_style),
            path_area,
        );
        cursor.y += path_height;
        cursor.height = cursor.height.saturating_sub(path_height);

        // The selected setting must remain visible even in a short terminal.
        // Optional detail/help shrink before consuming the final row.
        let help_h = cursor.height.saturating_sub(1).min(3);
        let help = Rect::new(cursor.x, cursor.bottom() - help_h, cursor.width, help_h);
        cursor.height = cursor.height.saturating_sub(help_h);

        let detail_h = cursor.height.saturating_sub(1).min(5);
        let detail = Rect::new(cursor.x, cursor.bottom() - detail_h, cursor.width, detail_h);
        cursor.height = cursor.height.saturating_sub(detail_h);

        self.render_rows(frame, cursor, accent_style, muted_style);
        self.render_detail(frame, detail, theme, accent_style, muted_style);
        self.render_help(frame, help, accent_style, muted_style);
    }

    fn render_tabs(&self, frame: &mut Frame, area: Rect, theme: Theme, muted_style: Style) {
        if area.height == 0 {
            return;
        }
        let mut spans = Vec::new();
        for (i, label) in SCOPES.iter().enumerate() {
            if i > 0 {
                spans.push(Span::raw("  "));
            }
            let style = if i == self.scope % 2 {
                Style::default().bg(accent(theme)).fg(Color::Black)
            } else {
                muted_style
            };
            spans.push(Span::styled(format!("[{label}]"), style));
        }
        frame.render_widget(Paragraph::new(Line::from(spans)), area);
    }

    fn render_rows(&self, frame: &mut Frame, area: Rect, accent_style: Style, muted_style: Style) {
        if area.height == 0 || area.width == 0 || self.rows.is_empty() {
            return;
        }
        let height = usize::from(area.height);
        let start = self.selected.saturating_sub(height.saturating_sub(1));
        for (visible, (i, row)) in self
            .rows
            .iter()
            .enumerate()
            .skip(start)
            .take(height)
            .enumerate()
        {
            let rect = Rect::new(area.x, area.y + visible as u16, area.width, 1);
            let focused = i == self.selected;
            let dirty = self.edits.contains_key(&i);
            let marker = if focused { "▸" } else { " " };
            let dirty_marker = if dirty { "*" } else { " " };
            let restart = if row.restart { " (restart)" } else { "" };
            let text = format!(
                "{marker}{dirty_marker} {label:<18} {value:<24} [{origin}]{restart}",
                label = row.label,
                value = self.display_value(i),
                origin = row.origin,
            );
            let style = if focused {
                accent_style
            } else if dirty {
                Style::default().fg(Color::Yellow)
            } else {
                muted_style
            };
            frame.render_widget(
                Paragraph::new(clip(&text, area.width as usize)).style(style),
                rect,
            );
        }
    }

    fn render_detail(
        &self,
        frame: &mut Frame,
        area: Rect,
        theme: Theme,
        accent_style: Style,
        muted_style: Style,
    ) {
        if area.height == 0 || area.width == 0 {
            return;
        }
        let Some(row) = self.rows.get(self.selected) else {
            return;
        };
        let mut lines: Vec<Line> = vec![Line::from(Span::styled(
            row.description.clone(),
            muted_style,
        ))];
        if let Some(buffer) = &self.editing {
            lines.push(Line::from(vec![
                Span::styled("> ", accent_style),
                Span::raw(buffer.clone()),
                Span::styled("_", accent_style),
            ]));
        } else if row.choices.is_empty() {
            lines.push(Line::from(Span::styled(
                "free text — ⏎ to edit",
                muted_style,
            )));
        } else {
            let current = self.display_value(self.selected);
            let mut spans = Vec::new();
            for (i, choice) in row.choices.iter().enumerate() {
                if i > 0 {
                    spans.push(Span::raw("  "));
                }
                let style = if *choice == current {
                    Style::default().bg(accent(theme)).fg(Color::Black)
                } else {
                    muted_style
                };
                spans.push(Span::styled(format!("[{choice}]"), style));
            }
            lines.push(Line::from(spans));
            if let Some(preview) = statusline_preview(&row.choices, &current) {
                for line in preview.lines() {
                    lines.push(Line::from(Span::styled(line.to_string(), muted_style)));
                }
            }
        }
        frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), area);
    }

    fn render_help(&self, frame: &mut Frame, area: Rect, accent_style: Style, muted_style: Style) {
        if area.height == 0 {
            return;
        }
        let hint = if area.width >= 78 {
            "↑↓ move · ←→ cycle · ⏎ edit/cycle · ⌫ inherit · TAB scope · ^S apply · ESC cancel"
        } else {
            "↑↓ ←→ ⏎ ⌫ TAB ^S ESC"
        };
        let dirty = if self.is_dirty() {
            "● unsaved changes staged"
        } else {
            "saved"
        };
        let mut lines = vec![
            Line::from(Span::styled(hint, accent_style)),
            Line::from(Span::styled(dirty, muted_style)),
        ];
        if let Some(notice) = &self.notice {
            lines.push(Line::from(Span::styled(notice.clone(), muted_style)));
        }
        frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), area);
    }
}

/// Statusline is the one row this panel renders a live preview for, per the
/// brief: a description of the layout, never a shelled-out or arbitrary
/// rendering of it.
fn statusline_preview(choices: &[String], current: &str) -> Option<&'static str> {
    let is_statusline = choices.iter().any(|c| c == "full")
        && choices.iter().any(|c| c == "compact")
        && choices.iter().any(|c| c == "hide" || c == "hidden");
    if !is_statusline {
        return None;
    }
    Some(match current {
        "full" => {
            "preview: [model] [sandbox] [tokens] [cost] [clock] — wraps to two lines when narrow"
        }
        "compact" => "preview: model · sandbox · tokens · cost — one line",
        "hide" | "hidden" => "preview: (status line hidden)",
        _ => "preview: pending — apply to see the inherited layout",
    })
}

/// Splits the first row off `area`, shrinking it in place. Panics never:
/// a zero-height area yields a zero-height row and is left untouched.
fn take_line(area: &mut Rect) -> Rect {
    let row = Rect::new(area.x, area.y, area.width, area.height.min(1));
    area.y = area.y.saturating_add(row.height);
    area.height = area.height.saturating_sub(row.height);
    row
}

/// Truncates to `width` columns, ellipsis-terminated. Character-counted --
/// this panel's own labels and values are plain ASCII/curated strings, never
/// arbitrary wide text, so grapheme-cluster precision is not worth a second
/// dependency on top of what `crate::tui` already carries for the transcript.
fn clip(text: &str, width: usize) -> String {
    if text.chars().count() <= width {
        return text.to_string();
    }
    if width == 0 {
        return String::new();
    }
    let mut out: String = text.chars().take(width.saturating_sub(1)).collect();
    out.push('…');
    out
}

/// Mirrors `crate::tui::Theme`'s private accent palette (see the module doc
/// comment above for why this can't just call it).
fn accent(theme: Theme) -> Color {
    theme.accent()
}
