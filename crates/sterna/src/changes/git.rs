//! What the project's own version control already knows about the working
//! tree, asked for rather than recomputed.
//!
//! **This is the cheap half of change detection.** `git status --porcelain`
//! names every path that differs from `HEAD`, respects `.gitignore`, and sees
//! changes made by a shell command as readily as by a tool -- measured on this
//! repository, 9 ms against the 25.55 MiB a content scan had to read. Nothing
//! here reads a file the status did not name.
//!
//! Every function answers `None` rather than an error: a project that is not a
//! git repository, a git that is not installed, a repository mid-rebase whose
//! status fails -- each means "ask the filesystem instead", and
//! [`super::Snapshot`] has that fallback.

use std::path::{Path, PathBuf};
use std::process::Command;

/// One path git reports as differing from `HEAD`, with its two status letters.
///
/// The letters are kept because they answer a question the filesystem cannot:
/// whether a path that now differs from `HEAD` was ever *in* `HEAD`, which is
/// what separates a file this cell created from one it modified.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Entry {
    pub(super) path: PathBuf,
    pub(super) code: [u8; 2],
}

impl Entry {
    /// The path is absent from the working tree: git's `D` in either column.
    pub(super) fn deleted(&self) -> bool {
        self.code[0] == b'D' || self.code[1] == b'D'
    }
}

/// Runs `git` in `root` and returns stdout, or `None` if it could not be run
/// or answered non-zero.
fn run(root: &Path, args: &[&str]) -> Option<Vec<u8>> {
    // `--no-optional-locks` keeps a status from writing the index while a
    // person's own git is working in the same checkout.
    let output = Command::new("git")
        .arg("--no-optional-locks")
        .args(args)
        .current_dir(root)
        .output()
        .ok()?;
    output.status.success().then_some(output.stdout)
}

/// The commit `HEAD` names, or `None` outside a repository or before its
/// first commit.
pub(super) fn head(root: &Path) -> Option<String> {
    let out = run(root, &["rev-parse", "HEAD"])?;
    Some(String::from_utf8(out).ok()?.trim_end().to_string())
}

/// The repository root, which is what porcelain paths are relative to.
fn toplevel(root: &Path) -> Option<PathBuf> {
    let out = run(root, &["rev-parse", "--show-toplevel"])?;
    let text = String::from_utf8(out).ok()?;
    Some(PathBuf::from(text.trim_end()))
}

/// Every path under `root` that differs from `HEAD`, relative to `root`.
///
/// Scoped by the `.` pathspec, so a project root inside a larger repository
/// reports only its own subtree -- the same boundary the filesystem walk has,
/// and the reason a change outside the project root stays out of scope.
/// `--no-renames` keeps the status letters to the four this module reads.
pub(super) fn dirty(root: &Path) -> Option<Vec<Entry>> {
    let top = toplevel(root)?;
    // **Both sides resolved before they are compared, because git answers in
    // its own spelling of the same directory.** `rev-parse --show-toplevel`
    // prints the path git resolved; `root` is the path the session was given.
    // On Windows those routinely differ without naming different places -- a
    // `TEMP` reached as `C:\Users\RUNNER~1\...` against git's
    // `C:/Users/runneradmin/...` is the shape the sweep failed on -- and a
    // symlinked project root does the same on any host. When they differ,
    // every `strip_prefix` below misses, every entry is dropped, and `dirty`
    // answers `Some(vec![])`: not "I could not tell" but "nothing changed",
    // for a tree that had just been edited. A wrong answer with no way to
    // notice it is worse than the walk this function exists to avoid.
    let resolved = |path: &Path| std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let (top, root) = (resolved(&top), resolved(root));
    let root = root.as_path();
    let out = run(
        root,
        &[
            "status",
            "--porcelain",
            "-z",
            "--untracked-files=all",
            "--no-renames",
            "--",
            ".",
        ],
    )?;
    let mut entries = Vec::new();
    for record in out.split(|byte| *byte == 0) {
        if record.len() < 4 {
            continue;
        }
        let code = [record[0], record[1]];
        let name = String::from_utf8_lossy(&record[3..]).into_owned();
        let absolute = top.join(&name);
        // Porcelain paths are repository-relative; a project root deeper in
        // the repository keeps only what lies beneath it.
        let Ok(relative) = absolute.strip_prefix(root) else {
            continue;
        };
        entries.push(Entry {
            path: relative.to_path_buf(),
            code,
        });
    }
    entries.sort_by(|a, b| a.path.cmp(&b.path));
    Some(entries)
}

/// The committed bytes of `relative` at `HEAD`.
///
/// **This is what makes a clean file diffable without having been read first.**
/// A path git called clean before the cell is, by definition, identical to its
/// `HEAD` blob, so the blob is that file's before-state exactly -- no snapshot
/// had to hold it, and no cell had to be predicted.
pub(super) fn head_blob(root: &Path, relative: &Path, limit: u64) -> Option<Vec<u8>> {
    let name = relative.to_string_lossy().replace('\\', "/");
    let size = run(root, &["cat-file", "-s", &format!("HEAD:./{name}")])
        .and_then(|out| String::from_utf8(out).ok())
        .and_then(|text| text.trim_end().parse::<u64>().ok())?;
    if size > limit {
        return None;
    }
    run(root, &["show", &format!("HEAD:./{name}")])
}
