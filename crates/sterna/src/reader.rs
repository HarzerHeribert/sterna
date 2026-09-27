//! `helper.find` in one request (2026-09-23): Sterna gathers the evidence --
//! the project's file listing and the lines that hold the question's words
//! -- and a cheap model, holding no tools, answers with the line ranges that
//! answer the question. `excerpts.rs` then serves those lines from disk.
//!
//! **The invariant: the model names ranges and Sterna supplies every byte the
//! caller reads.** Why one request: measured offline the same day, a one-shot
//! over the listing and the matching files named as many of the needed files
//! as the Scout's tool loop or more, in 10-16 s against about 60 s; end to end
//! the loop took 60 s and 146k tokens for one call. When nothing it names
//! verifies, the caller falls back to the loop.

use std::path::Path;

/// The most `git grep` lines served, and the most characters of each.
pub const MAX_HIT_LINES: usize = 160;
const HIT_CHARS: usize = 160;
/// The most distinct words searched for.
const MAX_WORDS: usize = 8;

const STOP: &[&str] = &[
    "about",
    "after",
    "again",
    "every",
    "explain",
    "explore",
    "find",
    "from",
    "have",
    "into",
    "just",
    "like",
    "more",
    "most",
    "much",
    "only",
    "other",
    "over",
    "same",
    "should",
    "some",
    "such",
    "that",
    "their",
    "them",
    "then",
    "there",
    "these",
    "they",
    "this",
    "those",
    "very",
    "what",
    "when",
    "where",
    "which",
    "while",
    "with",
    "would",
    "your",
    "does",
    "done",
    "each",
    "file",
    "files",
    "code",
    "lives",
    "live",
    "implementation",
    "return",
    "relevant",
    "spans",
];

pub const FINDER: crate::helpers::HelperSpec = crate::helpers::HelperSpec {
    name: "find",
    summary: "Find where something lives, in one answer over the project's listing and search hits.",
    verb: "finding",
    preamble: "You find where something lives in a project, for a model that will act on your \
        answer. You cannot open files: you have the question, the lines of the project that hold \
        its words, and the project's file listing. Answer with at most six spans, most decisive \
        first, each on its own line as `path/to/file.rs:120-160 — what is there`, the range \
        that holds the answer (a whole function or section, at most 80 lines), taken from the \
        hits' line numbers. Only paths from the listing. The caller is shown exactly those \
        lines, so a precise range is your answer's whole value. If the hits and the paths do not \
        reveal it, name the files whose paths fit best as `path:1-80` and say so in one line. \
        Never answer the question itself and never invent a path.",
    tools: &[],
    max_tokens: 700,
    max_turns: 1,
    input: crate::helpers::InputKind::Request,
    output: crate::helpers::OutputKind::Spans,
    call_sites: &[],
};

/// The words worth searching for: distinct, long enough to mean something,
/// not a stop word, identifiers first (they name code).
#[must_use]
pub fn words(question: &str) -> Vec<String> {
    let mut found: Vec<String> = Vec::new();
    let tokens: Vec<&str> = question
        .split(|c: char| !(c.is_alphanumeric() || c == '_'))
        .filter(|w| w.len() >= 4)
        .collect();
    let identifier = |w: &&&str| w.contains('_') || w.chars().skip(1).any(char::is_uppercase);
    for word in tokens.iter().filter(identifier).chain(tokens.iter()) {
        let lower = word.to_lowercase();
        if STOP.contains(&lower.as_str()) || found.contains(&lower) {
            continue;
        }
        found.push(lower);
        if found.len() == MAX_WORDS {
            break;
        }
    }
    found
}

/// The lines of tracked files that hold any of `words`, as `path:line:text`,
/// the files holding the most distinct words first.
#[must_use]
pub fn hits(root: &Path, words: &[String]) -> String {
    if words.is_empty() {
        return String::new();
    }
    let pattern = words.join("|");
    let Ok(output) = std::process::Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["grep", "-n", "-I", "-i", "-E", "--max-count=6", &pattern])
        .output()
    else {
        return String::new();
    };
    let text = String::from_utf8_lossy(&output.stdout);
    let mut by_file: Vec<(String, Vec<String>, usize)> = Vec::new();
    for line in text.lines() {
        let Some((path, _)) = line.split_once(':') else {
            continue;
        };
        let clipped: String = line.chars().take(HIT_CHARS).collect();
        match by_file.iter_mut().find(|(p, _, _)| p == path) {
            Some(entry) => entry.1.push(clipped),
            None => by_file.push((path.to_string(), vec![clipped], 0)),
        }
    }
    for entry in &mut by_file {
        let joined = entry.1.join("\n").to_lowercase();
        entry.2 = words.iter().filter(|w| joined.contains(w.as_str())).count();
    }
    by_file.sort_by(|a, b| b.2.cmp(&a.2).then(a.0.cmp(&b.0)));
    by_file
        .into_iter()
        .flat_map(|(_, lines, _)| lines)
        .take(MAX_HIT_LINES)
        .collect::<Vec<_>>()
        .join("\n")
}

/// What the finder is asked: the question, the hits, the listing.
#[must_use]
pub fn brief(question: &str, root: &Path) -> String {
    let words = words(question);
    let hits = hits(root, &words);
    let listing = crate::preflight::listing_section(root).unwrap_or_default();
    format!(
        "## Question\n{question}\n\n## Lines holding its words ({})\n{}\n{listing}",
        words.join(", "),
        if hits.is_empty() {
            "(none)".to_string()
        } else {
            hits
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identifiers_come_first_and_stop_words_never() {
        let w = words("Find where the helper_callback serves excerpts and explain ExcerptSpan");
        assert_eq!(w[0], "helper_callback");
        assert_eq!(w[1], "excerptspan");
        assert!(!w.contains(&"find".to_string()) && !w.contains(&"explain".to_string()));
        assert!(w.contains(&"serves".to_string()) && w.contains(&"excerpts".to_string()));
    }
}
