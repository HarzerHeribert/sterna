//! Finding the file paths a person can click in the drawn transcript.
//!
//! **A path is clickable because it exists, not because it looks like one.**
//! A transcript is full of slash-shaped text that is not a file — a URL, a
//! ratio, a regex — and underlining those would teach the reader that the
//! underline means nothing. Every candidate below is resolved and stat-ed
//! once, through a bounded cache, and only what is really there is offered.
//!
//! The user's ruling of 2026-09-18 governs the other half: a clickable
//! filename is the one surface here with **no keyboard twin**, deliberately,
//! because naming a path to a slash command is slower than the click is
//! worth. `hit.rs`'s rule stands for every other surface.

use ratatui::style::Modifier;
use ratatui::text::Line;

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// The most paths remembered at once. A transcript repeats the same handful
/// of paths across every frame, so the cache hits almost always; the bound is
/// what stops a long session's scrollback from growing a map without end.
const REMEMBERED: usize = 1024;

/// Characters that end a path in prose, JSON or a table, and are never part
/// of one here.
const EDGES: &[char] = &[
    '"', '\'', '`', ',', ';', '(', ')', '[', ']', '{', '}', '<', '>', '|', '=',
];

thread_local! {
    static KNOWN: RefCell<HashMap<String, bool>> = RefCell::new(HashMap::new());
}

/// Whether `candidate` resolves to something that exists, remembered.
fn exists(candidate: &str, root: &Path) -> bool {
    KNOWN.with(|known| {
        let mut known = known.borrow_mut();
        if let Some(answer) = known.get(candidate) {
            return *answer;
        }
        if known.len() >= REMEMBERED {
            known.clear();
        }
        let resolved = resolve(candidate, root);
        let answer = resolved.is_some_and(|path| path.exists());
        known.insert(candidate.to_string(), answer);
        answer
    })
}

/// A candidate as a path on this machine, or `None` when it is not one.
///
/// A relative spelling resolves against the session's own root, which is what
/// makes `crates/sterna/src/tui.rs` -- the form a model writes -- clickable.
pub(crate) fn resolve(candidate: &str, root: &Path) -> Option<PathBuf> {
    if candidate.contains("://") {
        return None;
    }
    let path = Path::new(candidate);
    if path.is_absolute() {
        return Some(path.to_path_buf());
    }
    // A bare word is a word. Requiring a separator is what keeps "returned"
    // and "ok" out, and it is the same rule that keeps a sentence's last
    // word from being stat-ed on every frame.
    if !candidate.contains('/') && !(cfg!(windows) && candidate.contains('\\')) {
        return None;
    }
    Some(root.join(path))
}

/// Every existing path in `line`, as `(start, end)` character offsets.
///
/// Offsets, not bytes: the caller indexes the drawn row's cells, and a row is
/// one cell per grapheme.
pub(crate) fn found(line: &str, root: &Path) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let chars: Vec<char> = line.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        if chars[i].is_whitespace() || EDGES.contains(&chars[i]) {
            i += 1;
            continue;
        }
        let start = i;
        while i < chars.len() && !chars[i].is_whitespace() && !EDGES.contains(&chars[i]) {
            i += 1;
        }
        // A trailing full stop or colon belongs to the sentence, not the
        // path, so the trimmed spelling is the one tried first whenever
        // trimming removed anything -- and the untrimmed one is still tried,
        // because a path may legitimately end in either.
        //
        // **The order is the fix, and it is not separator-dependent.** Win32
        // discards the trailing dots and spaces of a path component, so
        // `src/tui.rs.` and `src\tui.rs.` both stat as the file itself on
        // Windows; asking about the untrimmed spelling first therefore
        // underlined the sentence's full stop there and nowhere else
        // (`a_sentences_full_stop_is_not_part_of_the_path`, the
        // `sterna (windows-latest)` cell). Asking about the trimmed spelling
        // first gives one answer on every platform.
        let token: String = chars[start..i].iter().collect();
        let trimmed = token.trim_end_matches(['.', ':']);
        if !trimmed.is_empty() && trimmed.len() < token.len() && exists(trimmed, root) {
            out.push((start, start + trimmed.chars().count()));
        } else if exists(&token, root) {
            out.push((start, i));
        }
    }
    out
}

/// Underlines every existing file path in one drawn row.
///
/// **Only what exists is marked** -- see `paths.rs`. A row whose spans are not
/// one-per-grapheme is left alone rather than mis-measured; `wrap_lines`
/// produces that shape for every transcript row.
pub(super) fn mark(line: &mut Line<'static>, root: &Path) {
    let text: String = line
        .spans
        .iter()
        .map(|span| span.content.as_ref())
        .collect();
    if !text.contains('/') && !(cfg!(windows) && text.contains('\\')) {
        return;
    }
    if line
        .spans
        .iter()
        .any(|span| span.content.chars().count() > 1)
    {
        return;
    }
    for (start, end) in found(&text, root) {
        let last = end.min(line.spans.len());
        for span in &mut line.spans[start.min(last)..last] {
            span.style = span.style.add_modifier(Modifier::UNDERLINED);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn root() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
    }

    #[test]
    fn a_real_relative_path_is_found_and_a_bare_word_is_not() {
        let line = "edited src/tui.rs and returned ok";
        let found = found(line, &root());
        assert_eq!(found.len(), 1, "{found:?}");
        let (start, end) = found[0];
        assert_eq!(&line[start..end], "src/tui.rs");
    }

    #[test]
    fn a_path_that_does_not_exist_is_not_offered() {
        assert!(found("see src/no-such-file.rs for it", &root()).is_empty());
    }

    #[test]
    fn a_url_is_never_a_path() {
        assert!(found("https://example.com/src/tui.rs", &root()).is_empty());
        assert!(resolve("https://example.com/x", &root()).is_none());
    }

    #[test]
    fn quotes_and_commas_are_not_part_of_the_path() {
        let line = r#"  "src/tui.rs","src/lib.rs""#;
        let found = found(line, &root());
        assert_eq!(found.len(), 2, "{found:?}");
        assert_eq!(&line[found[0].0..found[0].1], "src/tui.rs");
        assert_eq!(&line[found[1].0..found[1].1], "src/lib.rs");
    }

    #[test]
    fn a_sentences_full_stop_is_not_part_of_the_path() {
        let line = "it is in src/tui.rs.";
        let found = found(line, &root());
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(&line[found[0].0..found[0].1], "src/tui.rs");
    }

    /// One grapheme per span, which is the shape `wrap_lines` leaves and the
    /// shape `mark` measures columns from.
    fn row(text: &str) -> Line<'static> {
        Line::from(
            text.chars()
                .map(|c| ratatui::text::Span::raw(c.to_string()))
                .collect::<Vec<_>>(),
        )
    }

    /// A real path in a drawn row is underlined at exactly its own columns.
    #[test]
    fn a_drawn_path_is_underlined_at_its_own_columns() {
        let mut line = row("  edited src/tui.rs today");
        mark(&mut line, &root());
        let start = "  edited ".chars().count();
        let end = start + "src/tui.rs".chars().count();
        for (index, span) in line.spans.iter().enumerate() {
            let underlined = span.style.add_modifier.contains(Modifier::UNDERLINED);
            assert_eq!(
                underlined,
                (start..end).contains(&index),
                "column {index} ({:?}) is marked wrongly",
                span.content
            );
        }
    }

    /// A row whose spans are not one-per-grapheme would be measured wrongly,
    /// so it is left alone.
    #[test]
    fn a_row_that_is_not_one_span_per_grapheme_is_left_alone() {
        let mut line = Line::from("edited src/tui.rs today");
        mark(&mut line, &root());
        assert!(
            line.spans
                .iter()
                .all(|span| !span.style.add_modifier.contains(Modifier::UNDERLINED))
        );
    }

    #[test]
    fn the_cache_is_bounded() {
        for n in 0..(REMEMBERED + 50) {
            let _ = exists(&format!("src/absent-{n}.rs"), &root());
        }
        KNOWN.with(|known| assert!(known.borrow().len() <= REMEMBERED));
    }
}
