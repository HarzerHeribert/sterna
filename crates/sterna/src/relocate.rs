//! Sterna was called Pane, and kept what it saves under folders named for
//! Pane. Each start moves what it still finds under the old names, so no
//! saved setting, session or note is lost in the rename:
//!
//! * the global folder, `<config home>/pane` to `<config home>/sterna`;
//! * the project folder, `<root>/.pane` to `<root>/.sterna`, with everything
//!   in it (settings, sessions, notes, learned notes, scratch files);
//! * the project's named checks, `<root>/.glasshouse/checks.toml` to
//!   `<root>/.sterna/checks.toml`.
//!
//! **A move is one rename and never lands on anything.** It happens only
//! when the new place does not exist; when both do, the new one is used, the
//! old one is left exactly as it is, and the start says nothing about it --
//! `sterna doctor` names it instead. Nothing is copied, merged or deleted, so
//! a second start finds nothing left to move and says nothing.
//!
//! **A moved project folder stays out of git if the old one was.** When the
//! project's ignore rules covered `.pane/` (or its `sessions/`) and do not
//! cover the new name, the move writes `.sterna/.gitignore` so sessions and
//! notes are not committed by the next `git add -A`, and says so.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::project::workflows::{USER_DIRECTORY, config_home};

/// The folder names the rename retired.
const OLD_USER_DIRECTORY: &str = "pane";
const OLD_PROJECT_DIRECTORY: &str = ".pane";
const PROJECT_DIRECTORY: &str = ".sterna";
const OLD_CHECKS_DIRECTORY: &str = ".glasshouse";
const CHECKS_FILE: &str = "checks.toml";
/// The project folder's own sessions, the part of it most often ignored alone.
const SESSIONS_DIRECTORY: &str = "sessions";

/// What one start found under an old name.
enum Found {
    /// Nothing is there.
    Nothing,
    /// The new name exists too; it is used and the old one is left alone.
    BothPresent,
    /// The old one was renamed to the new one.
    Moved,
    /// The rename was refused.
    Failed(io::Error),
}

/// Moves `old` to `new` when `new` is absent, and returns the line that says
/// what happened -- `None` when there was nothing to move, including when
/// `new` already exists.
#[must_use]
pub fn move_once(old: &Path, new: &Path) -> Option<String> {
    let found = relocate(old, new);
    said(&found, old, new)
}

fn relocate(old: &Path, new: &Path) -> Found {
    if fs::symlink_metadata(old).is_err() {
        return Found::Nothing;
    }
    if fs::symlink_metadata(new).is_ok() {
        return Found::BothPresent;
    }
    let placed = match new.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => fs::create_dir_all(parent),
        _ => Ok(()),
    }
    .and_then(|()| fs::rename(old, new));
    match placed {
        Ok(()) => Found::Moved,
        Err(error) => Found::Failed(error),
    }
}

fn said(found: &Found, old: &Path, new: &Path) -> Option<String> {
    match found {
        Found::Nothing | Found::BothPresent => None,
        Found::Moved => Some(format!(
            "Pane is now Sterna: moved {} to {}.",
            old.display(),
            new.display()
        )),
        Found::Failed(error) => Some(format!(
            "Pane is now Sterna: could not move {} to {} ({error}); move it yourself to keep what it holds.",
            old.display(),
            new.display()
        )),
    }
}

/// The global folder under `home` (the user's configuration home).
#[must_use]
pub fn global(home: &Path) -> Option<String> {
    move_once(&home.join(OLD_USER_DIRECTORY), &home.join(USER_DIRECTORY))
}

/// The project folder and the project's checks under `root`.
#[must_use]
pub fn project(root: &Path) -> Vec<String> {
    let old = root.join(OLD_PROJECT_DIRECTORY);
    let new = root.join(PROJECT_DIRECTORY);
    let mut lines = Vec::new();
    // Asked before the rename, while the old folder is still there to ask
    // about; nothing is asked when there is nothing to move.
    let was_ignored = (fs::symlink_metadata(&old).is_ok() && fs::symlink_metadata(&new).is_err())
        .then(|| ignored_parts(root, OLD_PROJECT_DIRECTORY));
    let found = relocate(&old, &new);
    if let Some(mut line) = said(&found, &old, &new) {
        if let (Found::Moved, Some(was)) = (&found, was_ignored)
            && keep_ignored(root, &new, was)
        {
            line.push_str(&format!(
                " Your git ignore rules named {}, so {} now keeps the same files out of git.",
                old.display(),
                new.join(".gitignore").display()
            ));
        }
        lines.push(line);
    }
    let old_checks = root.join(OLD_CHECKS_DIRECTORY);
    let found = relocate(&old_checks.join(CHECKS_FILE), &new.join(CHECKS_FILE));
    if let Some(line) = said(
        &found,
        &old_checks.join(CHECKS_FILE),
        &new.join(CHECKS_FILE),
    ) {
        lines.push(line);
    }
    // The checks were all that folder held in most projects: emptied by
    // this move, it goes; holding anything else, it stays.
    if matches!(found, Found::Moved) {
        let _ = fs::remove_dir(&old_checks);
    }
    lines
}

/// Which parts of the project folder named `folder` git ignores: the whole
/// folder, and its sessions. Both `false` outside a repository or without
/// git -- nothing is then written, because nothing was ignored.
fn ignored_parts(root: &Path, folder: &str) -> [bool; 2] {
    [
        git_ignores(root, &format!("{folder}/")),
        git_ignores(root, &format!("{folder}/{SESSIONS_DIRECTORY}/")),
    ]
}

fn git_ignores(root: &Path, path: &str) -> bool {
    Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["check-ignore", "-q", "--", path])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

/// Carries the old folder's ignore status over to `new`: for each part the
/// old name had ignored and the new name does not, one line in
/// `new/.gitignore`. Returns whether it wrote anything.
fn keep_ignored(root: &Path, new: &Path, was: [bool; 2]) -> bool {
    let now = ignored_parts(root, PROJECT_DIRECTORY);
    let line = if was[0] && !now[0] {
        "*"
    } else if was[1] && !now[1] {
        "sessions/"
    } else {
        return false;
    };
    let path = new.join(".gitignore");
    let mut text = fs::read_to_string(&path).unwrap_or_default();
    if !text.is_empty() && !text.ends_with('\n') {
        text.push('\n');
    }
    text.push_str(line);
    text.push('\n');
    fs::write(&path, text).is_ok()
}

/// Everything a start moves: the global folder, and the project's when
/// `root` names one.
#[must_use]
pub fn at_start(root: Option<&Path>) -> Vec<String> {
    let mut lines: Vec<String> = config_home()
        .and_then(|home| global(&home))
        .into_iter()
        .collect();
    if let Some(root) = root {
        lines.extend(project(root));
    }
    lines
}

/// The older folders a start left alone because the new name was already in
/// use, one line each, for `sterna doctor`: the global folder, and the
/// project's when `root` names one.
#[must_use]
pub fn left_alone(root: Option<&Path>) -> Vec<String> {
    let mut pairs = Vec::new();
    if let Some(home) = config_home() {
        pairs.push((home.join(OLD_USER_DIRECTORY), home.join(USER_DIRECTORY)));
    }
    if let Some(root) = root {
        let new = root.join(PROJECT_DIRECTORY);
        pairs.push((root.join(OLD_PROJECT_DIRECTORY), new.clone()));
        pairs.push((
            root.join(OLD_CHECKS_DIRECTORY).join(CHECKS_FILE),
            new.join(CHECKS_FILE),
        ));
    }
    pairs
        .into_iter()
        .filter(|(old, new)| fs::symlink_metadata(old).is_ok() && fs::symlink_metadata(new).is_ok())
        .map(|(old, new)| {
            format!(
                "{} is used; the older {} from before the rename is still there and is not read",
                new.display(),
                old.display()
            )
        })
        .collect()
}

/// The project a command's arguments name: `--root <path>` or
/// `--root=<path>`, otherwise the current directory.
#[must_use]
pub fn root_of(args: &[String]) -> PathBuf {
    let mut words = args.iter();
    while let Some(word) = words.next() {
        if word == "--root" {
            if let Some(root) = words.next() {
                return PathBuf::from(root);
            }
        } else if let Some(root) = word.strip_prefix("--root=") {
            return PathBuf::from(root);
        }
    }
    PathBuf::from(".")
}

/// [`at_start`], said on standard error: for the commands that have no
/// session to open their conversation with.
pub fn announce(root: Option<&Path>) {
    for line in at_start(root) {
        eprintln!("{line}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(label: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "sterna-relocate-{label}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn a_project_folder_moves_once_with_everything_in_it() {
        let root = scratch("project");
        fs::create_dir_all(root.join(".pane/sessions")).unwrap();
        fs::write(root.join(".pane/config.toml"), "[model]\nparent = \"m\"\n").unwrap();
        fs::write(root.join(".pane/sessions/a.jsonl"), "{}\n").unwrap();
        fs::create_dir_all(root.join(".glasshouse")).unwrap();
        fs::write(root.join(".glasshouse/checks.toml"), "[checks.test]\n").unwrap();

        let lines = project(&root);

        assert_eq!(lines.len(), 2, "{lines:?}");
        assert!(
            lines[0].contains("moved") && lines[0].contains(".sterna"),
            "{lines:?}"
        );
        assert!(!root.join(".pane").exists());
        assert!(!root.join(".glasshouse").exists(), "an emptied folder goes");
        assert_eq!(
            fs::read_to_string(root.join(".sterna/config.toml")).unwrap(),
            "[model]\nparent = \"m\"\n"
        );
        assert_eq!(
            fs::read_to_string(root.join(".sterna/sessions/a.jsonl")).unwrap(),
            "{}\n"
        );
        assert_eq!(
            fs::read_to_string(root.join(".sterna/checks.toml")).unwrap(),
            "[checks.test]\n"
        );
        assert!(
            project(&root).is_empty(),
            "a second start has nothing to move"
        );
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn a_new_folder_that_exists_is_used_and_the_old_one_is_left_alone() {
        let root = scratch("both");
        fs::create_dir_all(root.join(".pane")).unwrap();
        fs::write(root.join(".pane/config.toml"), "old\n").unwrap();
        fs::create_dir_all(root.join(".sterna")).unwrap();
        fs::write(root.join(".sterna/config.toml"), "new\n").unwrap();
        fs::create_dir_all(root.join(".glasshouse")).unwrap();
        fs::write(root.join(".glasshouse/checks.toml"), "old checks\n").unwrap();
        fs::write(root.join(".glasshouse/pane.toml"), "legacy\n").unwrap();
        fs::write(root.join(".sterna/checks.toml"), "new checks\n").unwrap();

        let lines = project(&root);

        assert!(lines.is_empty(), "a start says nothing about it: {lines:?}");
        let named: Vec<String> = left_alone(Some(&root))
            .into_iter()
            .filter(|line| line.contains(&root.display().to_string()))
            .collect();
        assert_eq!(named.len(), 2, "doctor names both: {named:?}");
        assert!(
            named.iter().all(|line| line.contains("still there")),
            "{named:?}"
        );
        assert_eq!(
            fs::read_to_string(root.join(".sterna/config.toml")).unwrap(),
            "new\n"
        );
        assert_eq!(
            fs::read_to_string(root.join(".pane/config.toml")).unwrap(),
            "old\n"
        );
        assert_eq!(
            fs::read_to_string(root.join(".sterna/checks.toml")).unwrap(),
            "new checks\n"
        );
        assert_eq!(
            fs::read_to_string(root.join(".glasshouse/checks.toml")).unwrap(),
            "old checks\n"
        );
        assert!(
            root.join(".glasshouse/pane.toml").is_file(),
            "a folder that still holds anything stays"
        );
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn nothing_under_the_old_names_says_nothing() {
        let root = scratch("none");
        assert!(project(&root).is_empty());
        assert!(global(&root).is_none());
        assert!(
            !root.join(".sterna").exists(),
            "a move creates nothing it did not need"
        );
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn the_root_is_read_from_either_spelling_and_defaults_to_here() {
        let words = |list: &[&str]| list.iter().map(|w| (*w).to_string()).collect::<Vec<_>>();
        assert_eq!(
            root_of(&words(&["doctor", "--root", "/p"])),
            PathBuf::from("/p")
        );
        assert_eq!(
            root_of(&words(&["config", "--root=/q", "x"])),
            PathBuf::from("/q")
        );
        assert_eq!(root_of(&words(&["config", "local"])), PathBuf::from("."));
    }
}
