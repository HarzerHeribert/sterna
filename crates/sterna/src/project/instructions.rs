//! Bounded, sandbox-aware discovery of project instruction documents.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fs::File;
use std::io::Read;
use std::path::{Component, Path, PathBuf};

use crate::sandbox::profile::{Access, Profile};

const NAMES: [&str; 2] = ["AGENTS.md", "CLAUDE.md"];
const MAX_FILE_BYTES: u64 = 64 * 1024;
const MAX_TOTAL_BYTES: usize = 256 * 1024;
const MAX_ENTRIES: usize = 10_000;
const MAX_DOCS: usize = 128;
const MAX_TARGETS: usize = 256;
const MAX_DEPTH: usize = 32;
const SKIP_DIRS: [&str; 14] = [
    ".git",
    ".hg",
    ".svn",
    ".sterna",
    ".cache",
    ".venv",
    "target",
    "node_modules",
    "vendor",
    "dist",
    "build",
    "coverage",
    ".worktrees",
    ".agent-runtime",
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstructionDocument {
    /// Canonical path returned by the immutable read profile.
    pub path: PathBuf,
    /// Canonical directory to whose subtree this document applies.
    pub scope: PathBuf,
    /// Complete UTF-8 contents; never a truncated prefix.
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstructionOmission {
    pub path: Option<PathBuf>,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstructionLoad {
    pub documents: Vec<InstructionDocument>,
    pub omissions: Vec<InstructionOmission>,
    pub complete: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstructionIndex {
    pub paths: Vec<PathBuf>,
    pub omissions: Vec<InstructionOmission>,
    pub complete: bool,
}

/// The scan budgets: an index stopped by one of these has enumerated every
/// directory it reached and simply ran out of allowance, in breadth-first
/// order (shallow scopes first). A read failure or a denied document is
/// never one of these.
pub const BUDGET_OMISSIONS: [&str; 2] = ["directory entry limit", "directory depth limit"];

impl InstructionIndex {
    /// Incomplete only because a scan budget ran out, never because
    /// something that exists could not be read.
    #[must_use]
    pub fn only_budget_omissions(&self) -> bool {
        !self.complete
            && self.omissions.iter().all(|omission| {
                omission.path.is_none() && BUDGET_OMISSIONS.contains(&omission.reason.as_str())
            })
    }
}

#[derive(Default)]
struct Limits {
    bytes: usize,
    docs: usize,
    entries: usize,
    omitted: BTreeSet<&'static str>,
    unavailable: Vec<InstructionOmission>,
}

/// Root instructions, followed by a bounded contents-free index of nested
/// documents the model can load when a later tool path makes them relevant.
pub fn root(profile: &Profile) -> String {
    root_as(profile, false)
}

/// A document over this many bytes is outlined rather than inlined when the
/// outline is asked for: under it the whole text costs little, and a short
/// AGENTS.md is exactly the one whose every rule should be in view.
pub const OUTLINE_ABOVE_BYTES: usize = 8 * 1024;

/// [`root`], with each document over [`OUTLINE_ABOVE_BYTES`] shown as its
/// headings and their line numbers (`[limits] instructions_outline`). The
/// model reads a section when its task reaches it; measured 2026-09-23, this
/// repository's 49 KB CLAUDE.md was a third of every request's input.
pub fn root_outlined(profile: &Profile) -> String {
    root_as(profile, true)
}

fn root_as(profile: &Profile, outline: bool) -> String {
    let mut limits = Limits::default();
    let docs = load_candidates(
        profile,
        NAMES.iter().map(|name| profile.root().join(name)),
        &mut limits,
    );
    let paths = discover(profile, &mut limits);
    let load = structured(docs, &limits);
    let index = InstructionIndex {
        paths,
        omissions: load.omissions.clone(),
        complete: load.complete,
    };
    let mut rendered = super::workflows::user_instructions();
    rendered.push_str(&render(profile, &load, Some(&index), outline));
    rendered
}

/// Instructions whose directory scopes contain at least one target path.
/// Sibling scopes are excluded. Missing write targets inherit from their
/// lexical parent chain after the profile resolves existing symlink prefixes.
pub fn for_paths(profile: &Profile, paths: &[PathBuf]) -> String {
    let load = docs_for_paths(profile, paths);
    render_documents(profile, &load)
}

/// Complete applicable documents for a runtime path gate. A false `complete`
/// means the caller must not treat the returned subset as the whole policy.
pub fn docs_for_paths(profile: &Profile, paths: &[PathBuf]) -> InstructionLoad {
    let mut candidates = BTreeSet::new();
    for path in paths.iter().take(MAX_TARGETS) {
        if path
            .file_name()
            .is_some_and(|file| NAMES.iter().any(|name| file == *name))
        {
            candidates.insert(path.clone());
        }
        let Ok(resolved) = profile.check("instructions", Access::Read, path) else {
            continue;
        };
        if !resolved.starts_with(profile.root()) {
            continue;
        }
        let mut directory = if resolved.is_dir() {
            resolved
        } else {
            resolved.parent().unwrap_or(profile.root()).to_path_buf()
        };
        loop {
            if directory.starts_with(profile.root()) {
                for name in NAMES {
                    candidates.insert(directory.join(name));
                }
            }
            if directory == profile.root() || !directory.pop() {
                break;
            }
        }
    }
    let mut limits = Limits::default();
    if paths.len() > MAX_TARGETS {
        limits.omitted.insert("target path limit");
    }
    let docs = load_candidates(profile, candidates, &mut limits);
    structured(docs, &limits)
}

/// Canonical paths only; document contents are never read for this index.
pub fn index(profile: &Profile) -> InstructionIndex {
    let mut limits = Limits::default();
    let paths = discover(profile, &mut limits);
    let omissions = omissions(&limits);
    InstructionIndex {
        paths,
        complete: omissions.is_empty(),
        omissions,
    }
}

fn load_candidates(
    profile: &Profile,
    candidates: impl IntoIterator<Item = PathBuf>,
    limits: &mut Limits,
) -> BTreeMap<(PathBuf, PathBuf), String> {
    let mut docs: BTreeMap<(PathBuf, PathBuf), String> = BTreeMap::new();
    for candidate in candidates {
        if limits.docs >= MAX_DOCS {
            limits.omitted.insert("document count limit");
            break;
        }
        let known = std::fs::symlink_metadata(&candidate).is_ok();
        let Ok(resolved) = profile.check("instructions", Access::Read, &candidate) else {
            if known {
                limits.unavailable.push(InstructionOmission {
                    path: Some(candidate),
                    reason: "instruction document denied or unresolved".into(),
                });
            }
            continue;
        };
        if !resolved.starts_with(profile.root()) {
            limits.unavailable.push(InstructionOmission {
                path: Some(candidate),
                reason: "instruction document resolves outside project root".into(),
            });
            continue;
        }
        if !resolved.is_file() {
            continue;
        }
        let scope = candidate
            .parent()
            .and_then(|parent| std::fs::canonicalize(parent).ok())
            .filter(|parent| parent.starts_with(profile.root()))
            .unwrap_or_else(|| resolved.parent().unwrap_or(profile.root()).to_path_buf());
        if docs.contains_key(&(resolved.clone(), scope.clone())) {
            continue;
        }
        let Ok(file) = File::open(&resolved) else {
            limits.unavailable.push(InstructionOmission {
                path: Some(resolved),
                reason: "instruction document could not be opened".into(),
            });
            continue;
        };
        let mut bytes = Vec::new();
        if file
            .take(MAX_FILE_BYTES + 1)
            .read_to_end(&mut bytes)
            .is_err()
        {
            limits.unavailable.push(InstructionOmission {
                path: Some(resolved),
                reason: "instruction document could not be read".into(),
            });
            continue;
        }
        if bytes.len() as u64 > MAX_FILE_BYTES {
            limits.omitted.insert("per-document byte limit");
            limits.unavailable.push(InstructionOmission {
                path: Some(resolved),
                reason: "per-document byte limit".into(),
            });
            continue;
        }
        if limits.bytes.saturating_add(bytes.len()) > MAX_TOTAL_BYTES {
            limits.omitted.insert("total document byte limit");
            limits.unavailable.push(InstructionOmission {
                path: Some(resolved),
                reason: "total document byte limit".into(),
            });
            continue;
        }
        let Ok(text) = String::from_utf8(bytes) else {
            limits.omitted.insert("non-UTF-8 document");
            limits.unavailable.push(InstructionOmission {
                path: Some(resolved),
                reason: "non-UTF-8 document".into(),
            });
            continue;
        };
        // A copy of a document already in this scope -- an AGENTS.md
        // duplicated as CLAUDE.md for another harness -- says nothing new
        // and would be sent with every request twice.
        if docs
            .iter()
            .any(|((_, held), known)| *held == scope && known.trim() == text.trim())
        {
            continue;
        }
        limits.bytes += text.len();
        limits.docs += 1;
        docs.insert((resolved, scope), text);
    }
    docs
}

fn omissions(limits: &Limits) -> Vec<InstructionOmission> {
    let mut out = limits.unavailable.clone();
    out.extend(limits.omitted.iter().map(|reason| InstructionOmission {
        path: None,
        reason: (*reason).into(),
    }));
    out.sort_by(|a, b| a.path.cmp(&b.path).then_with(|| a.reason.cmp(&b.reason)));
    out.dedup();
    out
}

fn structured(docs: BTreeMap<(PathBuf, PathBuf), String>, limits: &Limits) -> InstructionLoad {
    let mut documents: Vec<_> = docs
        .into_iter()
        .map(|((path, scope), text)| InstructionDocument { scope, path, text })
        .collect();
    documents.sort_by(|a, b| {
        a.scope
            .components()
            .count()
            .cmp(&b.scope.components().count())
            .then_with(|| a.path.cmp(&b.path))
    });
    let omissions = omissions(limits);
    InstructionLoad {
        documents,
        complete: omissions.is_empty(),
        omissions,
    }
}

fn render_documents(profile: &Profile, load: &InstructionLoad) -> String {
    render(profile, load, None, false)
}

fn discover(profile: &Profile, limits: &mut Limits) -> Vec<PathBuf> {
    let mut found = BTreeSet::new();
    let mut queue = VecDeque::from([(profile.root().to_path_buf(), 0usize)]);
    while let Some((directory, depth)) = queue.pop_front() {
        if depth > MAX_DEPTH {
            limits.omitted.insert("directory depth limit");
            continue;
        }
        let Ok(checked) = profile.check("instructions", Access::Read, &directory) else {
            continue;
        };
        let Ok(read) = std::fs::read_dir(&checked) else {
            limits.unavailable.push(InstructionOmission {
                path: Some(checked),
                reason: "instruction directory could not be read".into(),
            });
            continue;
        };
        let remaining = MAX_ENTRIES.saturating_sub(limits.entries);
        let mut entries = Vec::new();
        let mut seen = 0usize;
        for result in read.take(remaining.saturating_add(1)) {
            seen += 1;
            match result {
                Ok(entry) => entries.push(entry),
                Err(_) => limits.unavailable.push(InstructionOmission {
                    path: Some(checked.clone()),
                    reason: "instruction directory entry could not be read".into(),
                }),
            }
        }
        if seen > remaining {
            limits.omitted.insert("directory entry limit");
            return found.into_iter().collect();
        }
        entries.sort_by_key(|entry| entry.file_name());
        for entry in entries {
            limits.entries += 1;
            if limits.entries > MAX_ENTRIES {
                limits.omitted.insert("directory entry limit");
                return found.into_iter().collect();
            }
            let path = entry.path();
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            if kind.is_symlink() {
                if depth > 0 && NAMES.iter().any(|name| entry.file_name() == *name) {
                    match profile.check("instructions", Access::Read, &path) {
                        Ok(resolved)
                            if resolved.starts_with(profile.root()) && resolved.is_file() =>
                        {
                            found.insert(path);
                        }
                        _ => limits.unavailable.push(InstructionOmission {
                            path: Some(path),
                            reason: "indexed instruction link denied or outside project root"
                                .into(),
                        }),
                    }
                }
                continue;
            }
            if kind.is_dir() {
                if !SKIP_DIRS.iter().any(|name| entry.file_name() == *name) {
                    queue.push_back((path, depth + 1));
                }
            } else if depth > 0
                && kind.is_file()
                && NAMES.iter().any(|name| entry.file_name() == *name)
                && profile
                    .check("instructions", Access::Read, &path)
                    .is_ok_and(|resolved| resolved.starts_with(profile.root()))
            {
                found.insert(path);
                if found.len().saturating_add(limits.docs) >= MAX_DOCS {
                    limits.omitted.insert("document count limit");
                    return found.into_iter().collect();
                }
            }
        }
    }
    found.into_iter().collect()
}

fn render(
    profile: &Profile,
    load: &InstructionLoad,
    index: Option<&InstructionIndex>,
    outline: bool,
) -> String {
    let mut out = String::new();
    if !load.documents.is_empty() {
        out.push_str("## Project instructions\n\nDocuments in the same directory have equal scope; their order below does not define precedence. Resolve contradictions explicitly. Deeper directory scopes apply only inside that directory.\n");
        for document in &load.documents {
            let relative = document
                .path
                .strip_prefix(profile.root())
                .unwrap_or(&document.path);
            let scope = document
                .scope
                .strip_prefix(profile.root())
                .ok()
                .filter(|path| !path.as_os_str().is_empty())
                .map(display_relative)
                .unwrap_or_else(|| ".".into());
            let body = if outline && document.text.len() > OUTLINE_ABOVE_BYTES {
                outlined(&display_relative(relative), &document.text)
            } else {
                document.text.trim_end().to_string()
            };
            out.push_str(&format!(
                "\n### `{}` (scope `{scope}`)\n\n{body}\n",
                display_relative(relative),
            ));
        }
    }
    if let Some(index) = index {
        out.push_str("\n## Scoped instruction index\n\nContents are loaded only when a tool path enters the listed scope.\n");
        out.push_str("Generated and auxiliary directories are excluded from this index; explicit tool paths still load their applicable instructions.\n");
        if index.paths.is_empty() {
            out.push_str("\n(none)\n");
        }
        for path in &index.paths {
            let relative = path.strip_prefix(profile.root()).unwrap_or(path);
            let scope = relative
                .parent()
                .filter(|p| !p.as_os_str().is_empty())
                .map(display_relative)
                .unwrap_or_else(|| ".".into());
            out.push_str(&format!(
                "- `{}` · scope `{scope}`\n",
                display_relative(relative)
            ));
        }
    }
    if !load.complete {
        let reasons: BTreeSet<_> = load
            .omissions
            .iter()
            .map(|omission| omission.reason.as_str())
            .collect();
        out.push_str("\nInstruction coverage is incomplete: ");
        out.push_str(&reasons.into_iter().collect::<Vec<_>>().join(", "));
        out.push_str(".\n");
        out.push_str(&render_named_omissions(profile, &load.omissions));
    }
    out
}

/// A long document as its headings, each with the line it starts on, and the
/// one instruction that makes the outline safe: read a section before acting
/// where it applies.
fn outlined(path: &str, text: &str) -> String {
    let lines = text.lines().count();
    let mut out = format!(
        "Only the headings of this document ({} KB, {lines} lines) are shown, to keep every \
         request small. Before you act in an area a heading covers, read that section: \
         `(await read({{path: \"{path}\"}})).excerpt({{start: <line>, lines: 60}})`.\n",
        text.len() / 1024
    );
    let mut fenced = false;
    for (index, line) in text.lines().enumerate() {
        if line.trim_start().starts_with("```") {
            fenced = !fenced;
        }
        if !fenced && line.starts_with('#') {
            out.push_str(&format!("L{} {line}\n", index + 1));
        }
    }
    out
}

/// The documents an omission names, listed by path.
///
/// **A bound that fires names what it dropped.** The reason line above says
/// only *that* something was left out; a person who wrote an instruction
/// document and a model asked to obey it both need to know *which* one is
/// missing, and the model can then read it with `read` rather than proceeding
/// as though the policy it was given were whole. An omission with no path is
/// a scan budget rather than a document and is already covered by the reason
/// line, so it is not repeated here.
fn render_named_omissions(profile: &Profile, omissions: &[InstructionOmission]) -> String {
    let named: Vec<&InstructionOmission> = omissions
        .iter()
        .filter(|omission| omission.path.is_some())
        .collect();
    if named.is_empty() {
        return String::new();
    }
    let mut out = String::from("Instruction documents not loaded:\n");
    for omission in named {
        let path = omission.path.as_deref().unwrap_or(Path::new(""));
        let relative = path.strip_prefix(profile.root()).unwrap_or(path);
        let shown = display_relative(relative);
        let shown = if shown.is_empty() {
            path.display().to_string()
        } else {
            shown
        };
        out.push_str(&format!("- `{shown}` — {}\n", omission.reason));
    }
    out
}

fn display_relative(path: &Path) -> String {
    path.components()
        .filter_map(|component| match component {
            Component::Normal(value) => Some(value.to_string_lossy()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("/")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_outline_lists_headings_with_their_lines_and_skips_fenced_ones() {
        let text = "# Title\nintro\n```sh\n# not a heading\n```\n## Rules\nbody\n";
        let out = outlined("CLAUDE.md", text);
        assert!(out.contains("L1 # Title\n"), "{out}");
        assert!(out.contains("L6 ## Rules\n"), "{out}");
        assert!(!out.contains("not a heading"), "{out}");
        assert!(out.contains(r#"read({path: "CLAUDE.md"})"#), "{out}");
    }
}
