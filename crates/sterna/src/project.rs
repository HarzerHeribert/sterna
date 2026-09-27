//! Loads `contract::ProjectConfig` from a project root, and writes nothing.
//!
//! Map line 2448's whole contract is that loading a project edits none of
//! it: every read below goes through [`safe_read`], which resolves symlinks
//! before comparing against the root so a link pointing outside the project
//! is skipped rather than followed, and nothing here ever opens a path for
//! writing.

pub mod agents;
pub mod instructions;
pub mod mcp;
pub mod orientation;
pub mod source_context;
pub mod workflows;

use crate::contract::ProjectConfig;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Loads everything map line 2448 names from `root`. A missing file or
/// directory is simply absent from the result -- never an error -- because
/// most projects have no `CLAUDE.md`, no `.claude/` directory at all, and a
/// loader that refused to start without them would be useless.
pub fn load(root: impl AsRef<Path>) -> ProjectConfig {
    let root = root.as_ref().to_path_buf();

    let mut instructions = Vec::new();
    for name in ["CLAUDE.md", "AGENTS.md"] {
        let path = root.join(name);
        if let Some(text) = safe_read(&root, &path) {
            instructions.push((path, text));
        }
    }

    let settings = safe_read(&root, &root.join(".claude").join("settings.json"));
    let mcp = safe_read(&root, &root.join(".mcp.json"));
    let commands = load_commands(&root);
    let skills = load_skills(&root);

    ProjectConfig {
        root,
        instructions,
        settings,
        commands,
        skills,
        mcp,
    }
}

fn load_commands(root: &Path) -> BTreeMap<String, String> {
    let dir = root.join(".claude").join("commands");
    let mut commands = BTreeMap::new();
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return commands;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("md") {
            continue;
        }
        let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
            continue;
        };
        if let Some(text) = safe_read(root, &path) {
            commands.insert(stem.to_string(), text);
        }
    }
    commands
}

fn load_skills(root: &Path) -> BTreeMap<String, PathBuf> {
    let dir = root.join(".claude").join("skills");
    let mut skills = BTreeMap::new();
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return skills;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|s| s.to_str()) else {
            continue;
        };
        if !is_within_root(root, &path) {
            continue;
        }
        if path.is_dir() {
            skills.insert(name.to_string(), path);
        }
    }
    skills
}

/// Reads `path`'s exact bytes as text, or `None` if it is absent, unreadable,
/// not valid UTF-8, or escapes `root` through a symlink. Never repairs and
/// never reserialises: whatever bytes are on disk are the string returned.
fn safe_read(root: &Path, path: &Path) -> Option<String> {
    if !is_within_root(root, path) {
        return None;
    }
    let bytes = std::fs::read(path).ok()?;
    String::from_utf8(bytes).ok()
}

/// Resolves symlinks on both sides and checks containment, so a symlink
/// whose target lands outside `root` is rejected rather than followed.
fn is_within_root(root: &Path, path: &Path) -> bool {
    let Ok(root) = std::fs::canonicalize(root) else {
        return false;
    };
    let Ok(resolved) = std::fs::canonicalize(path) else {
        return false;
    };
    resolved.starts_with(&root)
}
