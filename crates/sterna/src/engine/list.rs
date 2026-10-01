//! The one list of every folder and session (`sessions.json` in the data
//! folder): which folders were used, which sessions each holds, and when
//! each was last used. Every session writes its own entry as it is used --
//! a terminal's as much as the host's -- under a lock, so two sessions
//! ending at once both land.
//!
//! The list is an index, not the record: a session's turns stay in its
//! folder (`<root>/.sterna/sessions/<id>.jsonl`), and the terminal's own
//! `/resume` reads only those.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct List {
    pub folders: Vec<Folder>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Folder {
    pub root: String,
    /// When a session in it was last used, in Unix milliseconds.
    pub last_used: u64,
    pub sessions: Vec<Session>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Session {
    pub id: String,
    /// The first thing a person asked in it.
    pub title: String,
    pub last_used: u64,
}

/// The longest title kept: the first line of the first prompt, cut at a
/// word.
const TITLE: usize = 120;

fn path(folder: &Path) -> PathBuf {
    folder.join("sessions.json")
}

/// The list as the data folder holds it, newest first; empty when there is
/// none yet.
#[must_use]
pub fn read() -> List {
    super::data::folder().map_or_else(List::default, |folder| read_in(&folder))
}

fn read_in(folder: &Path) -> List {
    let mut list: List = std::fs::read_to_string(path(folder))
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default();
    order(&mut list);
    list
}

fn order(list: &mut List) {
    for folder in &mut list.folders {
        folder
            .sessions
            .sort_by(|a, b| b.last_used.cmp(&a.last_used).then_with(|| b.id.cmp(&a.id)));
    }
    list.folders.sort_by(|a, b| {
        b.last_used
            .cmp(&a.last_used)
            .then_with(|| a.root.cmp(&b.root))
    });
}

/// Session `id` in `root` was used now; `asked` is what the person asked,
/// which titles a session the list did not hold yet.
pub fn used(root: &Path, id: &str, asked: &str) -> std::io::Result<()> {
    let Some(folder) = super::data::folder() else {
        return Ok(());
    };
    used_in(&folder, root, id, asked, super::wire::now_ms())
}

fn used_in(folder: &Path, root: &Path, id: &str, asked: &str, now: u64) -> std::io::Result<()> {
    std::fs::create_dir_all(folder)?;
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(folder.join("sessions.lock"))?;
    lock.lock()?;
    let mut list = read_in(folder);
    let root = root.to_string_lossy().into_owned();
    let index = match list.folders.iter().position(|f| f.root == root) {
        Some(index) => index,
        None => {
            list.folders.push(Folder {
                root,
                ..Folder::default()
            });
            list.folders.len() - 1
        }
    };
    let entry = &mut list.folders[index];
    entry.last_used = now;
    match entry.sessions.iter_mut().find(|s| s.id == id) {
        Some(session) => session.last_used = now,
        None => entry.sessions.push(Session {
            id: id.to_string(),
            title: title(asked),
            last_used: now,
        }),
    }
    order(&mut list);
    let written = super::data::write_private(
        &path(folder),
        serde_json::to_string_pretty(&list)
            .map_err(std::io::Error::other)?
            .as_bytes(),
    );
    let _ = lock.unlock();
    written
}

/// The first line of what was asked, cut at a word.
fn title(asked: &str) -> String {
    let line = asked
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("");
    if line.chars().count() <= TITLE {
        return line.to_string();
    }
    let cut: String = line.chars().take(TITLE).collect();
    let at = cut.rfind(' ').unwrap_or(cut.len());
    format!("{}…", cut[..at].trim_end())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("sterna-list-{}", super::super::data::token()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn folders_and_their_sessions_read_newest_first() {
        let data = scratch();
        used_in(&data, Path::new("/a"), "s1", "fix the test", 10).unwrap();
        used_in(&data, Path::new("/b"), "s2", "write docs", 20).unwrap();
        used_in(&data, Path::new("/a"), "s3", "and another", 30).unwrap();
        let list = read_in(&data);
        let roots: Vec<_> = list.folders.iter().map(|f| f.root.as_str()).collect();
        assert_eq!(roots, ["/a", "/b"]);
        let ids: Vec<_> = list.folders[0]
            .sessions
            .iter()
            .map(|s| s.id.as_str())
            .collect();
        assert_eq!(ids, ["s3", "s1"]);
        // A session used again keeps the title it was first asked under.
        used_in(&data, Path::new("/a"), "s1", "something else", 40).unwrap();
        let list = read_in(&data);
        assert_eq!(list.folders[0].sessions[0].id, "s1");
        assert_eq!(list.folders[0].sessions[0].title, "fix the test");
        let _ = std::fs::remove_dir_all(&data);
    }

    #[test]
    fn a_long_first_prompt_is_cut_at_a_word() {
        let long = format!("{} tail", "word ".repeat(40));
        let cut = title(&long);
        assert!(cut.ends_with('…'), "{cut}");
        assert!(cut.chars().count() <= TITLE + 1);
        assert_eq!(title("\n  first line\nsecond"), "first line");
    }
}
