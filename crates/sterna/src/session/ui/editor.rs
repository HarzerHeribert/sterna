//! The composer's editor: a multi-line draft, the history behind it, one
//! kill buffer, an undo list, and the completions the popup offers.
//!
//! **The draft is lines, not one line.** ↑ and ↓ move between lines and
//! reach the history only from the first or the last, the line keys act on
//! the caret's own line, and a draft is never swapped for an old message
//! without being kept: a recall files it in the history first.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use std::path::PathBuf;

use crate::tui;

/// How many project paths `@` completion reads, at most.
const MAX_PATHS: usize = 20_000;
/// How many completions one popup offers.
const MAX_COMPLETIONS: usize = 50;

/// One edit state, for undo and redo.
#[derive(Clone, Debug, PartialEq)]
struct Snapshot {
    text: String,
    cursor: usize,
}

#[derive(Default)]
pub(super) struct Editor {
    pub(super) text: String,
    pub(super) cursor: usize,
    /// The popup row the keys point at.
    pub(super) selected: usize,
    history: Vec<String>,
    history_index: Option<usize>,
    draft: String,
    /// What the last kill took, for Ctrl-Y.
    killed: String,
    undo: Vec<Snapshot>,
    redo: Vec<Snapshot>,
    /// The last edit was typing, so the next typed character joins its undo
    /// step instead of starting one.
    typing: bool,
    /// The draft the popup was put away for; it stays away until the draft
    /// changes.
    dismissed: Option<String>,
    /// The project whose paths `@` completes, and those paths once read.
    pub(super) root: Option<PathBuf>,
    paths: Option<Vec<String>>,
}

/// What the popup offers, and what taking it does.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Completions {
    None,
    /// Commands for a draft that is one slash word.
    Commands(Vec<(String, String)>),
    /// Project paths for the `@` word under the caret.
    Paths(Vec<String>),
}

impl Completions {
    pub(super) fn rows(&self) -> Vec<(String, String)> {
        match self {
            Self::None => Vec::new(),
            Self::Commands(rows) => rows.clone(),
            Self::Paths(paths) => paths
                .iter()
                .map(|path| (format!("@{path}"), String::new()))
                .collect(),
        }
    }
    fn len(&self) -> usize {
        match self {
            Self::None => 0,
            Self::Commands(rows) => rows.len(),
            Self::Paths(paths) => paths.len(),
        }
    }
}

impl Editor {
    fn previous(&self) -> usize {
        self.text[..self.cursor]
            .char_indices()
            .last()
            .map(|(i, _)| i)
            .unwrap_or(0)
    }
    fn next(&self) -> usize {
        self.text[self.cursor..]
            .chars()
            .next()
            .map(|c| self.cursor + c.len_utf8())
            .unwrap_or(self.cursor)
    }
    fn line_start(&self, at: usize) -> usize {
        self.text[..at].rfind('\n').map_or(0, |i| i + 1)
    }
    fn line_end(&self, at: usize) -> usize {
        self.text[at..]
            .find('\n')
            .map_or(self.text.len(), |i| at + i)
    }
    /// The start of the word before the caret: past any gap, then the word.
    fn word_left(&self) -> usize {
        let before = &self.text[..self.cursor];
        let mut chars = before.char_indices().rev().peekable();
        while chars.next_if(|(_, c)| !is_word(*c)).is_some() {}
        let mut start = chars.peek().map_or(0, |(i, c)| i + c.len_utf8());
        while let Some((i, _)) = chars.next_if(|(_, c)| is_word(*c)) {
            start = i;
        }
        start
    }
    /// The end of the word after the caret: past any gap, then the word.
    fn word_right(&self) -> usize {
        let after = &self.text[self.cursor..];
        let mut chars = after.char_indices().peekable();
        while chars.next_if(|(_, c)| !is_word(*c)).is_some() {}
        while chars.next_if(|(_, c)| is_word(*c)).is_some() {}
        self.cursor + chars.peek().map_or(after.len(), |(i, _)| *i)
    }
    /// Keeps the state before an edit, so Ctrl-Z can bring it back. Typing
    /// in a run is one step; anything else starts a new one.
    fn edit(&mut self, typing: bool) {
        if !(typing && self.typing) {
            self.undo.push(Snapshot {
                text: self.text.clone(),
                cursor: self.cursor,
            });
        }
        self.redo.clear();
        self.typing = typing;
        self.selected = 0;
    }
    /// A caret move ends a run of typing.
    fn moved(&mut self) {
        self.typing = false;
    }
    fn insert_raw(&mut self, text: &str) {
        // Terminal transports may turn pasted LF into CR. Preserve either
        // newline convention as one LF, while stripping other controls.
        let mut text = text.chars().peekable();
        let mut normalized = String::with_capacity(text.size_hint().0);
        while let Some(c) = text.next() {
            match c {
                '\r' => {
                    if text.peek() == Some(&'\n') {
                        text.next();
                    }
                    normalized.push('\n');
                }
                '\n' | '\t' => normalized.push(c),
                c if !c.is_control() => normalized.push(c),
                _ => {}
            }
        }
        self.text.insert_str(self.cursor, &normalized);
        self.cursor += normalized.len();
    }
    /// Pasted or inserted text: one undo step.
    pub(super) fn insert(&mut self, text: &str) {
        self.edit(false);
        self.insert_raw(text);
    }
    fn typed(&mut self, c: char) {
        // A space ends the word being typed, and with it the undo step.
        self.edit(!c.is_whitespace());
        self.insert_raw(&c.to_string());
    }
    /// Replaces the draft, as a chip does: Ctrl-Z brings the old one back.
    pub(super) fn replace(&mut self, text: String) {
        self.edit(false);
        self.text = text;
        self.cursor = self.text.len();
    }
    /// Clears the draft, as Ctrl-C does: Ctrl-Z brings it back.
    pub(super) fn clear(&mut self) {
        self.edit(false);
        self.text.clear();
        self.cursor = 0;
    }
    /// Removes `range` into the kill buffer.
    fn kill(&mut self, from: usize, to: usize) {
        if from >= to {
            return;
        }
        self.edit(false);
        self.killed = self.text[from..to].to_string();
        self.text.drain(from..to);
        self.cursor = from;
    }
    fn restore(&mut self, snapshot: Snapshot) {
        self.text = snapshot.text;
        self.cursor = snapshot.cursor.min(self.text.len());
        self.typing = false;
        self.selected = 0;
    }
    fn undo(&mut self) {
        if let Some(snapshot) = self.undo.pop() {
            self.redo.push(Snapshot {
                text: self.text.clone(),
                cursor: self.cursor,
            });
            self.restore(snapshot);
        }
    }
    fn redo(&mut self) {
        if let Some(snapshot) = self.redo.pop() {
            self.undo.push(Snapshot {
                text: self.text.clone(),
                cursor: self.cursor,
            });
            self.restore(snapshot);
        }
    }
    /// An older (`older`) or newer message from the history. The draft is
    /// filed in the history first, so a recall never loses it.
    pub(super) fn recall(&mut self, older: bool) {
        let index = match (self.history_index, older) {
            (None, true) => {
                let before = self.history.len();
                if before == 0 {
                    return;
                }
                self.draft = self.text.clone();
                if !self.text.trim().is_empty() && self.history.last() != Some(&self.text) {
                    self.history.push(self.text.clone());
                }
                Some(before - 1)
            }
            (None, false) => return,
            (Some(i), true) => Some(i.saturating_sub(1)),
            (Some(i), false) if i + 1 < self.history.len() => Some(i + 1),
            (Some(_), false) => None,
        };
        self.text = index
            .map(|i| self.history[i].clone())
            .unwrap_or_else(|| self.draft.clone());
        self.history_index = index;
        self.cursor = self.text.len();
        self.selected = 0;
        self.typing = false;
    }
    /// The caret one line up, at the same column where the line allows, or
    /// the history from the first line.
    fn up(&mut self) {
        let start = self.line_start(self.cursor);
        if start == 0 {
            self.recall(true);
            return;
        }
        let column = self.text[start..self.cursor].chars().count();
        let above = self.line_start(start - 1);
        self.cursor = at_column(&self.text, above, start - 1, column);
        self.moved();
    }
    /// The caret one line down, or the history from the last line.
    fn down(&mut self) {
        let end = self.line_end(self.cursor);
        if end == self.text.len() {
            self.recall(false);
            return;
        }
        let column = self.text[self.line_start(self.cursor)..self.cursor]
            .chars()
            .count();
        let below = end + 1;
        self.cursor = at_column(&self.text, below, self.line_end(below), column);
        self.moved();
    }
    pub(super) fn take(&mut self) -> String {
        let text = std::mem::take(&mut self.text);
        if !text.trim().is_empty() && self.history.last() != Some(&text) {
            self.history.push(text.clone());
        }
        self.cursor = 0;
        self.selected = 0;
        self.history_index = None;
        self.draft.clear();
        self.undo.clear();
        self.redo.clear();
        self.typing = false;
        self.dismissed = None;
        text
    }

    /// The `@` word under the caret: where it starts and what follows the
    /// `@` up to the caret.
    fn at_word(&self) -> Option<(usize, &str)> {
        let before = &self.text[..self.cursor];
        let start = before.rfind(char::is_whitespace).map_or(0, |i| {
            i + before[i..].chars().next().map_or(1, char::len_utf8)
        });
        let word = &before[start..];
        word.strip_prefix('@').map(|query| (start, query))
    }
    /// What the popup offers for the draft as it stands.
    pub(super) fn completions(&mut self) -> Completions {
        if self.dismissed() {
            return Completions::None;
        }
        let commands = tui::slash_matches(&self.text);
        if !commands.is_empty() {
            return Completions::Commands(
                commands
                    .into_iter()
                    .map(|(name, help)| (name, help.to_string()))
                    .collect(),
            );
        }
        let Some((_, query)) = self.at_word() else {
            return Completions::None;
        };
        let query = query.to_lowercase();
        let paths = self.paths();
        let mut found: Vec<(u8, &String)> = paths
            .iter()
            .filter_map(|path| {
                let lower = path.to_lowercase();
                let name = lower.rsplit('/').next().unwrap_or(&lower);
                let rank = if name.starts_with(&query) {
                    0
                } else if lower.starts_with(&query) {
                    1
                } else if lower.contains(&query) {
                    2
                } else {
                    return None;
                };
                Some((rank, path))
            })
            .collect();
        found.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.len().cmp(&b.1.len())));
        let found: Vec<String> = found
            .into_iter()
            .take(MAX_COMPLETIONS)
            .map(|(_, path)| path.clone())
            .collect();
        if found.is_empty() {
            Completions::None
        } else {
            Completions::Paths(found)
        }
    }
    /// The project's paths, read once, the first time an `@` asks.
    fn paths(&mut self) -> &[String] {
        if self.paths.is_none() {
            self.paths = Some(self.root.as_deref().map(project_paths).unwrap_or_default());
        }
        self.paths.as_deref().unwrap_or_default()
    }
    /// Puts the popup away for this draft; it comes back when the draft
    /// changes. False when there was none to put away.
    pub(super) fn dismiss(&mut self) -> bool {
        if matches!(self.completions(), Completions::None) {
            return false;
        }
        self.dismissed = Some(self.text.clone());
        true
    }
    /// The popup was put away for the draft as it stands.
    pub(super) fn dismissed(&self) -> bool {
        self.dismissed.as_deref() == Some(self.text.as_str())
    }
    /// A second Escape after the popup was put away takes back the word it
    /// was for -- the `@` word under the caret, or the slash command that is
    /// the whole draft -- and keeps the rest of the draft.
    pub(super) fn drop_popup_word(&mut self) {
        match self.at_word() {
            Some((start, _)) => self.kill(start, self.cursor),
            None => self.clear(),
        }
    }
    /// The popup's selection, one row on or back.
    pub(super) fn move_selection(&mut self, down: bool) {
        let count = self.completions().len();
        if count == 0 {
            return;
        }
        self.selected = if down {
            (self.selected + 1) % count
        } else {
            (self.selected + count - 1) % count
        };
    }
    /// Takes the popup's row `index`: a command becomes the draft; a path
    /// replaces the `@` word it completes. True when it was a command.
    pub(super) fn complete(&mut self, index: usize) -> Option<bool> {
        match self.completions() {
            Completions::None => None,
            Completions::Commands(rows) => {
                let (name, _) = rows.get(index)?.clone();
                self.edit(false);
                self.text = name;
                self.cursor = self.text.len();
                Some(true)
            }
            Completions::Paths(paths) => {
                let path = paths.get(index)?.clone();
                let (start, _) = self.at_word()?;
                let end = self.text[self.cursor..]
                    .find(char::is_whitespace)
                    .map_or(self.text.len(), |i| self.cursor + i);
                self.edit(false);
                let with = format!("@{path} ");
                self.text.replace_range(start..end, &with);
                self.cursor = start + with.len();
                Some(false)
            }
        }
    }

    /// One key. True when it sends the draft.
    pub(super) fn key(&mut self, key: KeyEvent) -> bool {
        let control = key.modifiers.contains(KeyModifiers::CONTROL);
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        let shift = key.modifiers.contains(KeyModifiers::SHIFT);
        let popup = !matches!(self.completions(), Completions::None);
        match key.code {
            KeyCode::Char('p') if control && popup => self.move_selection(false),
            KeyCode::Char('n') if control && popup => self.move_selection(true),
            KeyCode::Char('p') if control => self.up(),
            KeyCode::Char('n') if control => self.down(),
            KeyCode::Char('a') if control => {
                self.cursor = self.line_start(self.cursor);
                self.moved();
            }
            KeyCode::Char('e') if control => {
                self.cursor = self.line_end(self.cursor);
                self.moved();
            }
            KeyCode::Char('u') if control => {
                self.kill(self.line_start(self.cursor), self.cursor);
            }
            // At a line's end the newline goes, so repeated presses join
            // the lines rather than doing nothing.
            KeyCode::Char('k') if control => {
                let end = self.line_end(self.cursor);
                let end = if end == self.cursor && end < self.text.len() {
                    end + 1
                } else {
                    end
                };
                self.kill(self.cursor, end);
            }
            KeyCode::Char('y') if control => {
                let killed = self.killed.clone();
                if !killed.is_empty() {
                    self.insert(&killed);
                }
            }
            KeyCode::Char('w') if control => self.kill(self.word_left(), self.cursor),
            KeyCode::Backspace if alt || control => self.kill(self.word_left(), self.cursor),
            KeyCode::Char('b') if alt => {
                self.cursor = self.word_left();
                self.moved();
            }
            KeyCode::Char('f') if alt => {
                self.cursor = self.word_right();
                self.moved();
            }
            KeyCode::Left if control || alt => {
                self.cursor = self.word_left();
                self.moved();
            }
            KeyCode::Right if control || alt => {
                self.cursor = self.word_right();
                self.moved();
            }
            KeyCode::Char('z' | 'Z') if control && shift => self.redo(),
            KeyCode::Char('Z') if control => self.redo(),
            KeyCode::Char('z') if control => self.undo(),
            // LF: what a terminal sends for a newline typed or pasted
            // without bracketed paste.
            KeyCode::Char('j') if control => self.insert("\n"),
            KeyCode::Left => {
                self.cursor = self.previous();
                self.moved();
            }
            KeyCode::Right => {
                self.cursor = self.next();
                self.moved();
            }
            KeyCode::Home => {
                self.cursor = self.line_start(self.cursor);
                self.moved();
            }
            KeyCode::End => {
                self.cursor = self.line_end(self.cursor);
                self.moved();
            }
            KeyCode::Backspace => {
                let prev = self.previous();
                if prev < self.cursor {
                    self.edit(false);
                    self.text.drain(prev..self.cursor);
                    self.cursor = prev;
                }
            }
            KeyCode::Delete => {
                let next = self.next();
                if next > self.cursor {
                    self.edit(false);
                    self.text.drain(self.cursor..next);
                }
            }
            KeyCode::Up | KeyCode::Down if popup => {
                self.move_selection(key.code == KeyCode::Down);
            }
            KeyCode::Up => self.up(),
            KeyCode::Down => self.down(),
            KeyCode::Tab => {
                if self.complete(self.selected) == Some(true) {
                    self.text.push(' ');
                    self.cursor = self.text.len();
                }
            }
            KeyCode::Enter if alt || shift => self.insert("\n"),
            KeyCode::Enter => {
                // A path is completed in place; a command is sent. The
                // exact name is the popup's first row, so Enter on "/plan"
                // sends /plan.
                return self.complete(self.selected).unwrap_or(true);
            }
            KeyCode::Char(c) if !control && !alt => self.typed(c),
            _ => {}
        }
        false
    }
}

fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// The byte offset `column` characters into the line `start..end`, or its
/// end when it is shorter.
fn at_column(text: &str, start: usize, end: usize, column: usize) -> usize {
    text[start..end]
        .char_indices()
        .nth(column)
        .map_or(end, |(i, _)| start + i)
}

/// The project's files as `@` offers them: what git tracks and would track,
/// or else a bounded walk that skips hidden folders and build output.
fn project_paths(root: &std::path::Path) -> Vec<String> {
    let listed = std::process::Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["ls-files", "--cached", "--others", "--exclude-standard"])
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .ok()
        .filter(|out| out.status.success())
        .map(|out| {
            String::from_utf8_lossy(&out.stdout)
                .lines()
                .take(MAX_PATHS)
                .map(str::to_string)
                .collect::<Vec<_>>()
        });
    if let Some(paths) = listed.filter(|paths| !paths.is_empty()) {
        return paths;
    }
    let mut paths = Vec::new();
    let mut folders = vec![root.to_path_buf()];
    while let Some(folder) = folders.pop() {
        let Ok(entries) = std::fs::read_dir(&folder) else {
            continue;
        };
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if name.starts_with('.') || name == "target" || name == "node_modules" {
                continue;
            }
            let path = entry.path();
            if path.is_dir() {
                folders.push(path);
            } else if let Ok(relative) = path.strip_prefix(root) {
                paths.push(relative.to_string_lossy().replace('\\', "/"));
                if paths.len() >= MAX_PATHS {
                    return paths;
                }
            }
        }
    }
    paths.sort();
    paths
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }
    fn ctrl(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)
    }
    fn alt(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::ALT)
    }
    fn typed(editor: &mut Editor, text: &str) {
        for c in text.chars() {
            editor.key(key(KeyCode::Char(c)));
        }
    }

    #[test]
    fn up_in_a_multiline_draft_moves_a_line_not_into_history() {
        let mut editor = Editor {
            history: vec!["old".into()],
            ..Editor::default()
        };
        editor.insert("first\nsecond");
        editor.key(key(KeyCode::Up));
        assert_eq!(editor.text, "first\nsecond");
        assert_eq!(editor.line_start(editor.cursor), 0, "on the first line");
        // From the first line, Up reaches the history -- and the draft is
        // kept there, so nothing is lost.
        editor.key(key(KeyCode::Up));
        assert_eq!(editor.text, "old");
        editor.key(key(KeyCode::Down));
        assert_eq!(editor.text, "first\nsecond");
    }

    #[test]
    fn a_recalled_message_sent_keeps_the_draft_in_history() {
        let mut editor = Editor {
            history: vec!["old".into()],
            ..Editor::default()
        };
        editor.insert("my draft");
        editor.key(key(KeyCode::Up));
        assert_eq!(editor.take(), "old");
        editor.key(key(KeyCode::Up));
        editor.key(key(KeyCode::Up));
        assert_eq!(editor.text, "my draft", "the draft was filed, not lost");
    }

    #[test]
    fn the_line_keys_act_on_the_caret_line_and_a_kill_comes_back() {
        let mut editor = Editor::default();
        editor.insert("one two\nthree four\nfive");
        editor.cursor = "one two\nthree".len();
        editor.key(ctrl('a'));
        assert_eq!(editor.cursor, "one two\n".len());
        editor.key(ctrl('e'));
        assert_eq!(editor.cursor, "one two\nthree four".len());
        editor.key(ctrl('k'));
        assert_eq!(editor.text, "one two\nthree fourfive", "only the newline");
        editor.key(ctrl('u'));
        assert_eq!(editor.text, "one two\nfive");
        editor.key(ctrl('y'));
        assert_eq!(editor.text, "one two\nthree fourfive");
    }

    #[test]
    fn word_keys_move_and_delete_a_word() {
        let mut editor = Editor::default();
        editor.insert("cargo test --lib");
        editor.key(ctrl('w'));
        assert_eq!(editor.text, "cargo test --");
        editor.key(alt(KeyCode::Char('b')));
        assert_eq!(editor.cursor, "cargo ".len());
        editor.key(alt(KeyCode::Char('f')));
        assert_eq!(editor.cursor, "cargo test".len());
        editor.key(alt(KeyCode::Backspace));
        assert_eq!(editor.text, "cargo  --");
    }

    #[test]
    fn undo_brings_back_a_cleared_or_replaced_draft() {
        let mut editor = Editor::default();
        typed(&mut editor, "a careful draft");
        editor.replace("commit this".into());
        editor.key(ctrl('z'));
        assert_eq!(editor.text, "a careful draft");
        editor.clear();
        editor.key(ctrl('z'));
        assert_eq!(editor.text, "a careful draft");
        editor.key(ctrl('z'));
        assert_eq!(editor.text, "a careful ", "typing undoes a word at a time");
        editor.key(KeyEvent::new(
            KeyCode::Char('Z'),
            KeyModifiers::CONTROL | KeyModifiers::SHIFT,
        ));
        assert_eq!(editor.text, "a careful draft");
    }

    #[test]
    fn a_line_feed_is_a_newline() {
        let mut editor = Editor::default();
        typed(&mut editor, "one");
        editor.key(ctrl('j'));
        typed(&mut editor, "two");
        assert_eq!(editor.text, "one\ntwo");
    }

    #[test]
    fn enter_keeps_the_exact_command_and_esc_puts_the_popup_away() {
        let mut editor = Editor::default();
        typed(&mut editor, "/plan");
        assert!(editor.key(key(KeyCode::Enter)));
        assert_eq!(editor.text, "/plan");
        let mut editor = Editor::default();
        typed(&mut editor, "/cell");
        assert!(editor.key(key(KeyCode::Enter)));
        assert_eq!(editor.text, "/cell");
        let mut editor = Editor::default();
        typed(&mut editor, "/mo");
        assert!(editor.dismiss());
        assert_eq!(editor.completions(), Completions::None);
        typed(&mut editor, "d");
        assert!(!matches!(editor.completions(), Completions::None));
    }

    #[test]
    fn a_second_escape_takes_back_only_the_word_the_popup_was_for() {
        let mut editor = Editor {
            paths: Some(vec!["docs/workbench.md".into()]),
            ..Editor::default()
        };
        typed(&mut editor, "look at @work");
        assert!(editor.dismiss());
        editor.drop_popup_word();
        assert_eq!(editor.text, "look at ");
        assert_eq!(editor.cursor, editor.text.len());
        let mut editor = Editor::default();
        typed(&mut editor, "/mo");
        assert!(editor.dismiss());
        editor.drop_popup_word();
        assert_eq!(editor.text, "");
    }

    #[test]
    fn at_completes_a_project_path_in_place() {
        let mut editor = Editor {
            paths: Some(vec![
                "crates/sterna/src/main.rs".into(),
                "docs/workbench.md".into(),
            ]),
            ..Editor::default()
        };
        typed(&mut editor, "look at @work");
        assert_eq!(
            editor.completions(),
            Completions::Paths(vec!["docs/workbench.md".into()])
        );
        assert!(
            !editor.key(key(KeyCode::Enter)),
            "Enter completes, not sends"
        );
        assert_eq!(editor.text, "look at @docs/workbench.md ");
    }

    #[test]
    fn editing_preserves_unicode_boundaries_and_multiline_paste() {
        let mut editor = Editor::default();
        editor.insert("a界\nb");
        editor.key(key(KeyCode::Left));
        editor.key(key(KeyCode::Backspace));
        assert_eq!(editor.text, "a界b");
        editor.key(key(KeyCode::Backspace));
        assert_eq!(editor.text, "ab");
        editor.key(KeyEvent::new(KeyCode::Enter, KeyModifiers::ALT));
        assert_eq!(editor.text, "a\nb");
    }
    #[test]
    fn paste_normalizes_terminal_newlines_without_admitting_controls() {
        let mut editor = Editor::default();
        editor.insert("first\rsecond\r\nthird\nfourth\tcolumn\x00\x1bfinal");
        assert_eq!(editor.text, "first\nsecond\nthird\nfourth\tcolumnfinal");
        assert_eq!(editor.cursor, editor.text.len());
    }
    #[test]
    fn selection_changes_what_tab_completes() {
        let mut editor = Editor::default();
        editor.insert("/");
        editor.key(key(KeyCode::Down));
        editor.key(key(KeyCode::Tab));
        assert_eq!(editor.text, "/models ");
        assert_eq!(editor.cursor, editor.text.len());
    }
    #[test]
    fn history_restores_an_unsent_draft() {
        let mut editor = Editor::default();
        editor.insert("sent");
        editor.take();
        editor.insert("draft");
        editor.recall(true);
        assert_eq!(editor.text, "sent");
        editor.recall(false);
        assert_eq!(editor.text, "draft");
    }
}
