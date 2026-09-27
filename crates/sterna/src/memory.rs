//! A project's memory: the notes a session keeps, one JSON line each, in a
//! file beside its sessions.
use std::fs;
use std::io::Write;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// One note in a project's memory.
///
/// **A plain table of notes is right; authority classes and decay are not.**
/// Its whole job is to hold what a session asked to remember and hand it
/// back by substring or as the latest one.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Note {
    pub text: String,
}

/// A JSON-Lines table of [`Note`]s under a directory the caller owns.
pub struct LocalMemory {
    path: PathBuf,
}

impl LocalMemory {
    /// `dir` is Sterna's own state directory; this owns one file inside it.
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self {
            path: dir.into().join("notes.jsonl"),
        }
    }

    pub fn add(&self, text: impl Into<String>) -> std::io::Result<()> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)?;
        }
        let note = Note { text: text.into() };
        let mut line = serde_json::to_string(&note).unwrap_or_default();
        line.push('\n');
        let mut file = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)?;
        file.write_all(line.as_bytes())
    }

    fn all(&self) -> Vec<Note> {
        let Ok(contents) = fs::read_to_string(&self.path) else {
            return Vec::new();
        };
        contents
            .lines()
            .filter_map(|line| serde_json::from_str(line).ok())
            .collect()
    }

    /// Notes whose text contains `query`, case-insensitively.
    pub fn search(&self, query: &str) -> Vec<Note> {
        let query = query.to_lowercase();
        self.all()
            .into_iter()
            .filter(|note| note.text.to_lowercase().contains(&query))
            .collect()
    }

    /// The most recently added note: the session's checkpoint.
    pub fn latest(&self) -> Option<String> {
        self.all().into_iter().next_back().map(|note| note.text)
    }
}
