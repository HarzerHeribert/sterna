//! The reader's half that a model cannot get wrong: the exact lines behind
//! the spans a `helper.find` answer names, read from disk by Sterna.
//!
//! **The invariant: every excerpt is text Sterna read from the file at the
//! time of the call, through the session profile's own read check, and
//! inside the project root.** A span naming a missing file, a denied path or
//! a line past the end is listed as unverified and served nothing. So the
//! acting model can trust an excerpt without reopening the file -- the
//! reason a list of paths alone saved it no reads (measured 2026-09-23: the
//! parent opened every file a brief named).

use std::collections::BTreeMap;
use std::path::Path;

use crate::sandbox::profile::{Access, Profile};

/// The whole appended block's bound: excerpts past it are named, not served.
pub const MAX_BYTES: usize = 16 * 1024;
/// The most lines one span serves, whatever range it names.
pub const MAX_SPAN_LINES: usize = 80;
/// A span naming one line serves this many before it and ...
const BEFORE: usize = 3;
/// ... this many after it: enough to hold the function or block it points at.
const AFTER: usize = 20;

pub const HEADING: &str =
    "## Excerpts (read from disk by Sterna just now; exact, no need to reopen these lines)";

/// One named span: a path relative to the root and a 1-based inclusive range.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Span {
    pub path: String,
    pub start: usize,
    pub end: usize,
}

/// Every `path:N` or `path:N-M` token in `answer`, in order, once each.
#[must_use]
pub fn spans(answer: &str) -> Vec<Span> {
    let mut found: Vec<Span> = Vec::new();
    for raw in answer.split(|c: char| {
        c.is_whitespace() || matches!(c, '`' | '(' | ')' | '[' | ']' | ',' | ';' | '"' | '\'')
    }) {
        let token = raw.trim_end_matches(['.', ':']);
        let Some((path, lines)) = token.rsplit_once(':') else {
            continue;
        };
        if path.is_empty() || !(path.contains('/') || path.contains('.')) || path.contains("://") {
            continue;
        }
        let (start, end) = match lines.split_once(['-', '–']) {
            Some((a, b)) => (a.parse::<usize>().ok(), b.parse::<usize>().ok()),
            None => {
                let n = lines.parse::<usize>().ok();
                (n, n)
            }
        };
        let (Some(start), Some(end)) = (start, end) else {
            continue;
        };
        if start == 0 || end < start {
            continue;
        }
        let span = Span {
            path: path.trim_start_matches("./").to_string(),
            start,
            end,
        };
        if !found.contains(&span) {
            found.push(span);
        }
    }
    found
}

/// `answer` with the verified excerpts of its spans appended, or `answer`
/// unchanged when it names none.
#[must_use]
pub fn attach(answer: &str, profile: &Profile) -> String {
    let spans = spans(answer);
    if spans.is_empty() {
        return answer.to_string();
    }
    let root = profile.root();
    let canonical_root = root.canonicalize().ok();
    let inside = |resolved: &Path| {
        resolved.starts_with(root)
            || canonical_root
                .as_deref()
                .is_some_and(|r| resolved.starts_with(r))
    };
    // One read per file, and ranges in one file merged, so two spans a few
    // lines apart are served once.
    // (first line served, last line served, the line the span named)
    let mut by_file: BTreeMap<String, Vec<(usize, usize, usize)>> = BTreeMap::new();
    let mut order: Vec<String> = Vec::new();
    for span in &spans {
        let (start, end) = if span.start == span.end {
            (span.start.saturating_sub(BEFORE).max(1), span.start + AFTER)
        } else {
            (span.start, span.end.min(span.start + MAX_SPAN_LINES - 1))
        };
        if !by_file.contains_key(&span.path) {
            order.push(span.path.clone());
        }
        by_file
            .entry(span.path.clone())
            .or_default()
            .push((start, end, span.start));
    }
    let mut served = String::new();
    let mut unverified: Vec<String> = Vec::new();
    let mut left_out: Vec<String> = Vec::new();
    for path in order {
        let text = match profile.check("read", Access::Read, Path::new(&path)) {
            Ok(resolved) if inside(&resolved) => std::fs::read_to_string(&resolved).ok(),
            _ => None,
        };
        let Some(text) = text else {
            unverified.push(format!("{path} (not a readable file in this project)"));
            continue;
        };
        let lines: Vec<&str> = text.lines().collect();
        let mut ranges = by_file.remove(&path).unwrap_or_default();
        ranges.sort_unstable();
        let mut merged: Vec<(usize, usize, usize)> = Vec::new();
        for (start, end, named) in ranges {
            match merged.last_mut() {
                Some(last) if start <= last.1 + 1 => last.1 = last.1.max(end),
                _ => merged.push((start, end, named)),
            }
        }
        for (start, end, named) in merged {
            if named > lines.len() {
                unverified.push(format!(
                    "{path}:{named} (the file has {} lines)",
                    lines.len()
                ));
                continue;
            }
            let end = end.min(lines.len());
            let width = end.to_string().len();
            let mut block = format!("### {path}:{start}-{end}\n");
            for (index, line) in lines[start - 1..end].iter().enumerate() {
                block.push_str(&format!("{:>width$} | {line}\n", start + index));
            }
            if served.len() + block.len() > MAX_BYTES {
                left_out.push(format!("{path}:{start}-{end}"));
                continue;
            }
            served.push_str(&block);
        }
    }
    let mut out = answer.trim_end().to_string();
    if !served.is_empty() {
        out.push_str(&format!("\n\n{HEADING}\n{served}"));
    }
    if !unverified.is_empty() {
        out.push_str(&format!(
            "\nUnverified, served nothing: {}\n",
            unverified.join("; ")
        ));
    }
    if !left_out.is_empty() {
        out.push_str(&format!(
            "Not served, over the {} KB bound: {}\n",
            MAX_BYTES / 1024,
            left_out.join(", ")
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fixture directory removed when the test ends.
    struct Dir(std::path::PathBuf);
    impl Dir {
        fn path(&self) -> &Path {
            &self.0
        }
    }
    impl Drop for Dir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn spans_are_read_from_answer_text_in_every_spelling_the_scout_uses() {
        let answer = "`src/a.rs:12` holds it\n- src/b.rs:3-9: the parser\nsee (lib/c.py:40).\nhttps://x.io:80 is not one";
        let spans = spans(answer);
        assert_eq!(
            spans,
            vec![
                Span {
                    path: "src/a.rs".into(),
                    start: 12,
                    end: 12
                },
                Span {
                    path: "src/b.rs".into(),
                    start: 3,
                    end: 9
                },
                Span {
                    path: "lib/c.py".into(),
                    start: 40,
                    end: 40
                },
            ]
        );
    }

    #[test]
    fn an_excerpt_is_the_files_own_lines_and_a_span_past_the_end_serves_nothing() {
        let root = std::env::temp_dir().join(format!("sterna-excerpts-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let dir = Dir(root);
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        let body: String = (1..=30).map(|n| format!("line {n}\n")).collect();
        std::fs::write(dir.path().join("src/a.rs"), body).unwrap();
        let profile = Profile::compile(dir.path(), None);
        let out = attach("src/a.rs:4-6 is it; src/a.rs:99 too; gone.rs:1", &profile);
        assert!(out.contains(HEADING), "{out}");
        assert!(
            out.contains("### src/a.rs:4-6\n4 | line 4\n5 | line 5\n6 | line 6\n"),
            "{out}"
        );
        assert!(out.contains("src/a.rs:99 (the file has 30 lines)"), "{out}");
        assert!(
            out.contains("gone.rs (not a readable file in this project)"),
            "{out}"
        );
    }
}
