//! Which session last changed each file in a folder:
//! `<root>/.sterna/changes.jsonl`, one line for every file a cell changed,
//! appended when the cell ends. Two sessions in one folder read it to tell
//! their own change from the other's: a rollback over the other session's
//! change is refused, and says whose it was (`docs/engine.md`).

use std::io::Write;
use std::path::{Path, PathBuf};

fn path(root: &Path) -> PathBuf {
    root.join(".sterna").join("changes.jsonl")
}

/// A path as the ledger names it: relative to the folder, with `/`.
fn name(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

/// Session `session` changed `paths` (relative to `root`) at `at`, in Unix
/// milliseconds. A ledger that cannot be written costs only the naming.
pub fn record(root: &Path, session: &str, paths: &[PathBuf], at: u64) {
    if paths.is_empty() {
        return;
    }
    let file = path(root);
    if let Some(parent) = file.parent()
        && std::fs::create_dir_all(parent).is_err()
    {
        return;
    }
    let Ok(mut out) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&file)
    else {
        return;
    };
    // One write for the whole cell, so another session's lines cannot fall
    // between them.
    let lines: String = paths
        .iter()
        .map(|path| {
            format!(
                "{}\n",
                serde_json::json!({"session": session, "path": name(path), "at": at})
            )
        })
        .collect();
    let _ = out.write_all(lines.as_bytes());
}

/// Who last changed `path` (relative to `root`), and when.
#[must_use]
pub fn last_change(root: &Path, path: &Path) -> Option<(String, u64)> {
    let wanted = name(path);
    std::fs::read_to_string(self::path(root))
        .ok()?
        .lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .filter(|entry| entry["path"] == wanted.as_str())
        .filter_map(|entry| {
            Some((
                entry["session"].as_str()?.to_string(),
                entry["at"].as_u64()?,
            ))
        })
        .max_by_key(|(_, at)| *at)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_newest_change_to_a_path_names_its_session() {
        let root =
            std::env::temp_dir().join(format!("sterna-ledger-{}", crate::engine::data::token()));
        std::fs::create_dir_all(&root).unwrap();
        let shared = PathBuf::from("src").join("shared.txt");
        record(
            &root,
            "a",
            &[shared.clone(), PathBuf::from("only-a.txt")],
            10,
        );
        record(&root, "b", std::slice::from_ref(&shared), 20);
        assert_eq!(last_change(&root, &shared), Some(("b".into(), 20)));
        assert_eq!(
            last_change(&root, Path::new("only-a.txt")),
            Some(("a".into(), 10))
        );
        assert_eq!(last_change(&root, Path::new("never.txt")), None);
        let _ = std::fs::remove_dir_all(&root);
    }
}
