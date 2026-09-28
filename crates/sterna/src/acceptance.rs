//! A request-derived acceptance list: what the finished tree must show for
//! the request to count as done, decided before the model acts and checked
//! when it claims completion (`smarter-cheaper-roadmap.md`, *Request-derived
//! acceptance list*).
//!
//! The invariant: **every item is decided against the tree or a command's
//! exit through the one kernel, never against the model's narrative.** A
//! `judge` item is the one kind Sterna cannot decide mechanically; it goes to
//! the fresh checker with the mechanical results beside it.

use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::completion::{Finding, FindingKind};

/// A list longer than this is a plan, not an acceptance contract.
pub const MAX_ITEMS: usize = 8;
/// How much of a command's output or a file's text an item's evidence keeps.
pub const EVIDENCE_CHARS: usize = 400;
const TEXT_CHARS: usize = 200;
const READ_CAP: u64 = 1024 * 1024;

/// One verifiable expectation, in the request's own terms.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Item {
    FileExists { path: String },
    FileContains { path: String, text: String },
    RunExitsZero { command: String },
    OutputContains { command: String, text: String },
    Judge { text: String },
}

impl Item {
    #[must_use]
    pub fn render(&self) -> String {
        match self {
            Self::FileExists { path } => format!("file `{path}` exists"),
            Self::FileContains { path, text } => format!("file `{path}` contains `{text}`"),
            Self::RunExitsZero { command } => format!("`{command}` exits 0"),
            Self::OutputContains { command, text } => format!("`{command}` prints `{text}`"),
            Self::Judge { text } => format!("judge: {text}"),
        }
    }

    /// [`Self::render`] as the screen shows it, without the backticks
    /// written for the model.
    #[must_use]
    pub fn plain(&self) -> String {
        self.render().replace('`', "")
    }
}

/// The lister helper's whole instruction: the five line forms and nothing
/// else, so what comes back is parsed rather than trusted.
pub const DERIVE_PREAMBLE: &str = "You turn a request into a short checklist of acceptance items a reviewer can verify \
     against the finished files, without doing the request. Answer with one item per line and \
     nothing else, each in exactly one of these forms:\n\
     file: <relative path> exists\n\
     file: <relative path> contains <text>\n\
     run: <command> exits 0\n\
     output: <command> prints <text>\n\
     judge: <one sentence a reviewer can decide by reading the files>\n\
     Use only paths, commands and outputs the request names or clearly implies; never invent \
     a path or a command, and never propose how to do the work. Prefer file, run and output \
     items to judge items. At most eight items. If the request names nothing verifiable, \
     answer with one judge item.\n\
     \n\
     Never propose a fix and never say how the work should be done — you are returning \
     evidence of what done means, and an item that cannot be checked is worse than none. \
     If part of the request cannot be checked against files or commands, say so in one line \
     beginning `unverifiable:` as your last line.";

/// The items in a helper's answer, in order, deduplicated, at most
/// [`MAX_ITEMS`]; a line in no known form is dropped, never guessed at.
#[must_use]
pub fn parse(text: &str) -> Vec<Item> {
    let mut items = Vec::new();
    for raw in text.lines() {
        let line = raw.trim().trim_start_matches(['-', '*', '•']).trim();
        let line = line
            .trim_start_matches(|c: char| c.is_ascii_digit() || c == '.' || c == ')')
            .trim();
        let Some((kind, rest)) = line.split_once(':') else {
            continue;
        };
        let rest = rest.trim();
        let item = match kind.trim().to_ascii_lowercase().as_str() {
            "file" => {
                if let Some(path) = rest.strip_suffix(" exists") {
                    relative(path).map(|path| Item::FileExists { path })
                } else if let Some((path, text)) = rest.split_once(" contains ") {
                    relative(path)
                        .zip(clean(text))
                        .map(|(path, text)| Item::FileContains { path, text })
                } else {
                    None
                }
            }
            "run" => rest
                .strip_suffix(" exits 0")
                .and_then(clean)
                .map(|command| Item::RunExitsZero { command }),
            "output" => rest.split_once(" prints ").and_then(|(command, text)| {
                clean(command)
                    .zip(clean(text))
                    .map(|(command, text)| Item::OutputContains { command, text })
            }),
            "judge" => clean(rest).map(|text| Item::Judge { text }),
            _ => None,
        };
        if let Some(item) = item
            && !items.contains(&item)
        {
            items.push(item);
        }
        if items.len() == MAX_ITEMS {
            break;
        }
    }
    items
}

fn clean(text: &str) -> Option<String> {
    let text = text.trim().trim_matches(['`', '"', '\'']).trim();
    if text.is_empty() {
        return None;
    }
    Some(text.chars().take(TEXT_CHARS).collect())
}

/// A path the list may name: relative to the root, or absolute and resolved
/// against it at evaluation. `..` never survives.
fn relative(path: &str) -> Option<String> {
    let path = clean(path)?;
    let path = path.strip_prefix("./").unwrap_or(&path).to_string();
    if path.split(['/', '\\']).any(|component| component == "..") {
        return None;
    }
    Some(path)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Met,
    Unmet,
    /// Could not be decided here: a refused command, a path outside the
    /// root, an unreadable file. Never counted as met.
    Unknown,
    /// Decided by the fresh checker, not here.
    Judge,
    /// Not checked yet: every item before the task's first completion
    /// check that the tree does not already meet. Only [`standing`] holds
    /// it; [`evaluate`] never does.
    Open,
}

impl Status {
    /// The mark the screen draws beside an item.
    #[must_use]
    pub fn mark(self) -> &'static str {
        match self {
            Self::Met => "✓",
            Self::Unmet => "✕",
            Self::Unknown => "?",
            Self::Judge | Self::Open => "○",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Verdict {
    pub item: Item,
    pub status: Status,
    pub evidence: String,
}

/// Runs one command through the one kernel: the exit code (`None` for a
/// signal) and the combined output, or the refusal.
pub type Runner<'a> = dyn FnMut(&str) -> Result<(Option<i32>, String), String> + 'a;

/// A path an item names, inside the root or refused.
fn resolve(root: &Path, path: &str) -> Result<PathBuf, String> {
    let candidate = Path::new(path);
    let joined = if candidate.is_absolute() {
        candidate.to_path_buf()
    } else {
        root.join(candidate)
    };
    let root_canonical = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    let checked = joined.canonicalize().unwrap_or_else(|_| joined.clone());
    if checked.starts_with(&root_canonical) || joined.starts_with(root) {
        Ok(joined)
    } else {
        Err(format!("`{path}` is outside the project root"))
    }
}

fn excerpt(text: &str) -> String {
    let trimmed = text.trim();
    if trimmed.chars().count() <= EVIDENCE_CHARS {
        return trimmed.to_string();
    }
    let tail: String = trimmed
        .chars()
        .rev()
        .take(EVIDENCE_CHARS)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    format!("…{tail}")
}

/// Every item decided against the tree and the runner, in list order.
pub fn evaluate(items: &[Item], root: &Path, run: &mut Runner<'_>) -> Vec<Verdict> {
    items
        .iter()
        .map(|item| {
            let (status, evidence) = match item {
                Item::FileExists { path } => match resolve(root, path) {
                    Ok(full) => match std::fs::metadata(&full) {
                        Ok(meta) => (Status::Met, format!("present, {} bytes", meta.len())),
                        Err(_) => (Status::Unmet, "absent".to_string()),
                    },
                    Err(reason) => (Status::Unknown, reason),
                },
                Item::FileContains { path, text } => match resolve(root, path) {
                    Ok(full) => match read_bounded(&full) {
                        Ok(content) if content.contains(text.as_str()) => {
                            (Status::Met, "text found".to_string())
                        }
                        Ok(content) => (
                            Status::Unmet,
                            format!("text not found; the file ends with: {}", excerpt(&content)),
                        ),
                        Err(reason) => (Status::Unmet, reason),
                    },
                    Err(reason) => (Status::Unknown, reason),
                },
                Item::RunExitsZero { command } => match run(command) {
                    Ok((Some(0), _)) => (Status::Met, "exit 0".to_string()),
                    Ok((code, output)) => (
                        Status::Unmet,
                        format!(
                            "exit {}: {}",
                            code.map_or("signal".to_string(), |code| code.to_string()),
                            excerpt(&output)
                        ),
                    ),
                    Err(reason) => (Status::Unknown, reason),
                },
                Item::OutputContains { command, text } => match run(command) {
                    Ok((_, output)) if output.contains(text.as_str()) => {
                        (Status::Met, "text printed".to_string())
                    }
                    Ok((code, output)) => (
                        Status::Unmet,
                        format!(
                            "text not printed (exit {}): {}",
                            code.map_or("signal".to_string(), |code| code.to_string()),
                            excerpt(&output)
                        ),
                    ),
                    Err(reason) => (Status::Unknown, reason),
                },
                Item::Judge { .. } => (Status::Judge, String::new()),
            };
            Verdict {
                item: item.clone(),
                status,
                evidence,
            }
        })
        .collect()
}

/// The list as it stands between completion checks, for the screen.
///
/// A file item is decided against the tree now: a read, cheap enough after
/// every cell. Before the first check an item the tree does not meet yet is
/// [`Status::Open`], not unmet: the work has not claimed it. Every other
/// item is as the last check left it, or open before the first -- a command
/// is never run here, because running a test suite after every cell would
/// cost more than the task.
#[must_use]
pub fn standing(items: &[Item], checked: &[Verdict], root: &Path) -> Vec<Verdict> {
    let mut never = |_: &str| -> Result<(Option<i32>, String), String> {
        Err("commands run only at the completion check".to_string())
    };
    items
        .iter()
        .enumerate()
        .map(|(index, item)| match item {
            Item::FileExists { .. } | Item::FileContains { .. } => {
                let mut verdict = evaluate(std::slice::from_ref(item), root, &mut never).remove(0);
                if verdict.status == Status::Unmet && checked.is_empty() {
                    verdict.status = Status::Open;
                }
                verdict
            }
            _ => checked
                .get(index)
                .filter(|verdict| verdict.item == *item)
                .cloned()
                .unwrap_or_else(|| Verdict {
                    item: item.clone(),
                    status: Status::Open,
                    evidence: String::new(),
                }),
        })
        .collect()
}

/// How many items stand met, of how many.
#[must_use]
pub fn tally(verdicts: &[Verdict]) -> (usize, usize) {
    let met = verdicts
        .iter()
        .filter(|verdict| verdict.status == Status::Met)
        .count();
    (met, verdicts.len())
}

fn read_bounded(path: &Path) -> Result<String, String> {
    let meta = std::fs::metadata(path).map_err(|_| "absent".to_string())?;
    if meta.len() > READ_CAP {
        return Err(format!("{} bytes, larger than the read cap", meta.len()));
    }
    std::fs::read_to_string(path).map_err(|_| "not readable as text".to_string())
}

/// The unmet items as final-state findings: the sentence names the item and
/// what was observed, never what to do about it.
#[must_use]
pub fn findings(verdicts: &[Verdict]) -> Vec<Finding> {
    verdicts
        .iter()
        .filter(|verdict| verdict.status == Status::Unmet)
        .map(|verdict| Finding {
            kind: FindingKind::AcceptanceUnmet,
            path: None,
            sentence: format!(
                "Acceptance item not met: {} — {}.",
                verdict.item.render(),
                verdict.evidence
            ),
        })
        .collect()
}

/// The judge items' sentences, for the fresh checker.
#[must_use]
pub fn judge_texts(items: &[Item]) -> Vec<String> {
    items
        .iter()
        .filter_map(|item| match item {
            Item::Judge { text } => Some(text.clone()),
            _ => None,
        })
        .collect()
}

/// The block the model sees once, before its first turn.
#[must_use]
pub fn render_list(items: &[Item]) -> String {
    let mut out = String::from(
        "\n## Acceptance list\nDerived from the request; each item is checked against the \
         files and commands when you finish, and an unmet item holds the completion once.\n",
    );
    for item in items {
        out.push_str(&format!("- {}\n", item.render()));
    }
    out
}

/// The machine summary the result carries.
#[must_use]
pub fn summary(items: &[Item], verdicts: &[Verdict]) -> serde_json::Value {
    let count = |status: Status| verdicts.iter().filter(|v| v.status == status).count();
    serde_json::json!({
        "items": items.len(),
        "met": count(Status::Met),
        "unmet": count(Status::Unmet),
        "unknown": count(Status::Unknown),
        "judged": count(Status::Judge),
        "verdicts": verdicts,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_line_form_parses_and_the_rest_is_dropped() {
        let text = "1. file: out/result.txt exists\n- file: README.md contains `Usage`\nrun: make test exits 0\noutput: ./bin/tool --version prints 1.2\njudge: the parser rejects a malformed header\nplan: first do X\nfile: ../etc/passwd exists\n";
        let items = parse(text);
        assert_eq!(items.len(), 5, "{items:?}");
        assert_eq!(
            items[0],
            Item::FileExists {
                path: "out/result.txt".into()
            }
        );
        assert_eq!(
            items[1],
            Item::FileContains {
                path: "README.md".into(),
                text: "Usage".into()
            }
        );
        assert_eq!(
            items[2],
            Item::RunExitsZero {
                command: "make test".into()
            }
        );
        assert_eq!(
            items[3],
            Item::OutputContains {
                command: "./bin/tool --version".into(),
                text: "1.2".into()
            }
        );
        assert_eq!(
            items[4],
            Item::Judge {
                text: "the parser rejects a malformed header".into()
            }
        );
    }

    #[test]
    fn the_list_is_capped_and_deduplicated() {
        let text = (0..12)
            .map(|i| format!("file: f{}.txt exists", i % 10))
            .collect::<Vec<_>>()
            .join("\n");
        assert_eq!(parse(&text).len(), MAX_ITEMS);
    }

    #[test]
    fn file_items_are_decided_against_the_tree_and_commands_against_the_runner() {
        let root = std::env::temp_dir().join(format!("sterna-acceptance-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("a.txt"), "hello world\n").unwrap();
        let items = vec![
            Item::FileExists {
                path: "a.txt".into(),
            },
            Item::FileExists {
                path: "b.txt".into(),
            },
            Item::FileContains {
                path: "a.txt".into(),
                text: "world".into(),
            },
            Item::FileContains {
                path: "a.txt".into(),
                text: "mars".into(),
            },
            Item::RunExitsZero {
                command: "true".into(),
            },
            Item::RunExitsZero {
                command: "false".into(),
            },
            Item::OutputContains {
                command: "echo hi".into(),
                text: "hi".into(),
            },
            Item::Judge {
                text: "the tone is friendly".into(),
            },
        ];
        let mut runner = |command: &str| -> Result<(Option<i32>, String), String> {
            match command {
                "true" => Ok((Some(0), String::new())),
                "false" => Ok((Some(1), "nope".into())),
                "echo hi" => Ok((Some(0), "hi\n".into())),
                other => Err(format!("refused: {other}")),
            }
        };
        let verdicts = evaluate(&items, &root, &mut runner);
        let statuses: Vec<Status> = verdicts.iter().map(|v| v.status).collect();
        assert_eq!(
            statuses,
            vec![
                Status::Met,
                Status::Unmet,
                Status::Met,
                Status::Unmet,
                Status::Met,
                Status::Unmet,
                Status::Met,
                Status::Judge
            ]
        );
        let findings = findings(&verdicts);
        assert_eq!(findings.len(), 3);
        assert!(
            findings[0].sentence.contains("b.txt"),
            "{}",
            findings[0].sentence
        );
        assert!(
            findings[2].sentence.contains("exit 1"),
            "{}",
            findings[2].sentence
        );
        assert_eq!(
            judge_texts(&items),
            vec!["the tone is friendly".to_string()]
        );
        let summary = summary(&items, &verdicts);
        assert_eq!(summary["met"], 4);
        assert_eq!(summary["unmet"], 3);
        let _ = std::fs::remove_dir_all(root);
    }

    /// Between checks the screen's list reads files now and never runs a
    /// command: an item not met yet is open before the first check, and
    /// after one, every item is what the tree or that check says.
    #[test]
    fn the_standing_list_reads_files_now_and_leaves_commands_to_the_check() {
        let root =
            std::env::temp_dir().join(format!("sterna-acceptance-standing-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("a.txt"), "hello\n").unwrap();
        let items = vec![
            Item::FileExists {
                path: "a.txt".into(),
            },
            Item::FileExists {
                path: "b.txt".into(),
            },
            Item::RunExitsZero {
                command: "make test".into(),
            },
            Item::Judge {
                text: "the greeting is friendly".into(),
            },
        ];
        let statuses = |verdicts: &[Verdict]| verdicts.iter().map(|v| v.status).collect::<Vec<_>>();
        let before = standing(&items, &[], &root);
        assert_eq!(
            statuses(&before),
            vec![Status::Met, Status::Open, Status::Open, Status::Open]
        );
        assert_eq!(tally(&before), (1, 4));

        let mut runner =
            |_: &str| -> Result<(Option<i32>, String), String> { Ok((Some(1), "1 failed".into())) };
        let checked = evaluate(&items, &root, &mut runner);
        std::fs::write(root.join("b.txt"), "later\n").unwrap();
        let after = standing(&items, &checked, &root);
        assert_eq!(
            statuses(&after),
            vec![Status::Met, Status::Met, Status::Unmet, Status::Judge]
        );
        assert!(
            after[2].evidence.contains("exit 1"),
            "{}",
            after[2].evidence
        );
        std::fs::remove_file(root.join("a.txt")).unwrap();
        assert_eq!(standing(&items, &checked, &root)[0].status, Status::Unmet);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn a_path_outside_the_root_is_unknown_never_read() {
        let root =
            std::env::temp_dir().join(format!("sterna-acceptance-out-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let items = vec![Item::FileExists {
            path: "/etc/hostname".into(),
        }];
        let mut runner = |_: &str| -> Result<(Option<i32>, String), String> { unreachable!() };
        let verdicts = evaluate(&items, &root, &mut runner);
        assert_eq!(verdicts[0].status, Status::Unknown);
        assert!(
            verdicts[0].evidence.contains("outside"),
            "{}",
            verdicts[0].evidence
        );
        let _ = std::fs::remove_dir_all(root);
    }
}
