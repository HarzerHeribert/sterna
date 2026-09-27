//! Local observation of project files before and after a Sterna cell.
//!
//! Nothing produced here belongs in the model conversation.
//!
//! **Detecting a change and rendering it are two different jobs with two
//! different costs, and only the second one needs the file's bytes.** A
//! snapshot therefore records what every readable file *is* -- its length and
//! its modification time -- and reads contents only for files small enough to
//! diff and restore. Detection is complete for a tree of any size; a file too
//! large to keep is still watched, and a change to it is reported by name and
//! size rather than disappearing.
//!
//! Measured on 2026-09-17, before this split: the capture read every file's
//! bytes under a 16 MiB total budget, and this repository is 929 files and
//! 25.55 MiB with `.git`, `target` and `.sterna` excluded -- so every capture
//! here overran the budget and declared itself incomplete. In one 120-cell
//! session, 112 of 123 views printed "capture incomplete" while files were
//! being written throughout. A budget that an ordinary repository exceeds is
//! not a guard, it is a blindfold.
//!
//! What remains best-effort is the *walk*: a directory that cannot be read, or
//! a tree past [`MAX_FILES`] or [`MAX_DEPTH`], leaves paths unseen, and an
//! incomplete walk never turns an unseen path into a creation or a deletion.
//! Content that was not kept is a separate, quieter fact: it costs diff text
//! and rollback for that one file, never detection.

use crate::sandbox::profile::{Access, Profile};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// A guard against a pathological tree, not against an ordinary one: a source
/// checkout is hundreds or thousands of files, and exceeding this says so.
const MAX_FILES: usize = 200_000;
const MAX_DEPTH: usize = 64;

/// The largest file whose bytes are kept, so it can be diffed and restored.
/// A larger file is still watched by length and modification time.
const MAX_DIFF_FILE_BYTES: u64 = 1024 * 1024;

/// How many bytes of file content one snapshot keeps.
///
/// This bounds **memory**, which is the only thing keeping bytes costs, and
/// the arithmetic is worth stating: `session.rs` clones the before and after
/// snapshots to hold a rollback, and a task holds its starting snapshot, so
/// roughly four copies can be alive at once. Exceeding it costs diff text for
/// the files reached last -- each named at diff time -- and never costs
/// detection, so the failure mode is verbosity, not blindness.
const MAX_DIFF_CACHE_BYTES: u64 = 32 * 1024 * 1024;

const MAX_OUTPUT_BYTES: usize = 64 * 1024;

const EXCLUDED: &[&str] = &[
    ".git",
    ".glasshouse",
    ".sterna",
    "node_modules",
    "target",
    ".cache",
    ".next",
    ".venv",
    "__pycache__",
];

mod git;

/// What one path was, at the moment a snapshot was taken.
///
/// `bytes` is present only when the file was small enough to keep and the
/// snapshot had a reason to keep it; its absence costs diff text for that one
/// path and never costs detection, because `len` and `modified` are always
/// recorded.
#[derive(Clone, Debug, PartialEq, Eq)]
struct FileState {
    len: u64,
    modified: Option<SystemTime>,
    digest: Option<[u8; 32]>,
    bytes: Option<Vec<u8>>,
}

/// One path a snapshot has something to say about: its state, or its absence,
/// plus git's own two letters when git is what named it.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Observed {
    /// `None` means the path is not in the working tree.
    state: Option<FileState>,
    /// git's status letters, for the one question the filesystem cannot
    /// answer: whether a path that differs from `HEAD` was ever in `HEAD`.
    code: Option<[u8; 2]>,
}

/// Where a snapshot's knowledge comes from, which decides what its silence
/// means.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
enum Origin {
    /// Asked of git: `files` holds every path differing from `HEAD`, and a
    /// path that is absent from it is identical to its `HEAD` blob.
    Derived,
    /// Walked: `files` holds every readable file, and a path absent from it
    /// was not seen.
    #[default]
    Scanned,
}

/// A point-in-time view of the project's files.
///
/// Derived from git where the project is a repository, walked otherwise. In
/// neither case are file contents read speculatively: see the module header.
#[derive(Clone, Debug, Default)]
pub struct Snapshot {
    root: PathBuf,
    origin: Origin,
    files: BTreeMap<PathBuf, Observed>,
    complete: bool,
    notes: Vec<String>,
}

/// How one path differs between two snapshots.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ChangeKind {
    Created,
    Modified,
    Deleted,
}

/// A checked reversal of one observed cell change.
///
/// The plan contains only paths whose state changed between the cell's
/// before/after snapshots. Applying it proves each of those paths still holds
/// what the cell left there, and refuses the whole reversal otherwise.
#[derive(Clone, Debug)]
pub struct RollbackPlan {
    expected: BTreeMap<PathBuf, Option<FileState>>,
    operations: Vec<RollbackOperation>,
}

#[derive(Clone, Debug)]
enum RollbackOperation {
    Write { path: PathBuf, bytes: Vec<u8> },
    Remove { path: PathBuf },
}

impl Snapshot {
    /// Captures the project: from git where it can be, by walking where it
    /// cannot.
    pub fn capture(profile: &Profile) -> Self {
        Self::capture_with_limits(
            profile,
            MAX_FILES,
            MAX_DIFF_CACHE_BYTES,
            MAX_DIFF_FILE_BYTES,
        )
    }

    /// One key for everything a check could read in this project, or `None`
    /// when it cannot be proven: `HEAD`, plus each path that differs from it
    /// with its content digest. Only a complete git-derived capture whose
    /// every changed file was digested qualifies -- a walked tree or a file
    /// too large to hash could change without changing the key.
    #[must_use]
    pub fn tree_key(profile: &Profile) -> Option<String> {
        let snapshot = Self::capture(profile);
        let hashed = snapshot.files.values().all(|observed| {
            observed
                .state
                .as_ref()
                .is_none_or(|state| state.digest.is_some())
        });
        if !snapshot.complete || snapshot.origin != Origin::Derived || !hashed {
            return None;
        }
        let head = git::head(profile.root())?;
        Some(format!("{head}:{}", snapshot.digest()))
    }

    /// Captures by walking, whatever the project is.
    ///
    /// The tests reach for this to exercise the fallback and its limits on a
    /// fixture that may well sit inside a repository.
    #[cfg(test)]
    fn capture_by_walking(profile: &Profile) -> Self {
        Self::walk(
            profile,
            MAX_FILES,
            MAX_DIFF_CACHE_BYTES,
            MAX_DIFF_FILE_BYTES,
        )
    }

    fn capture_with_limits(
        profile: &Profile,
        max_files: usize,
        cache_budget: u64,
        max_file_bytes: u64,
    ) -> Self {
        match Self::from_git(profile, max_file_bytes) {
            Some(snapshot) => snapshot,
            None => Self::walk(profile, max_files, cache_budget, max_file_bytes),
        }
    }

    /// The derived capture: git names the paths, and only those paths are
    /// touched.
    ///
    /// Bytes are kept for a path that already differed from `HEAD`, because
    /// nothing else can reproduce what it held. A path git calls clean needs
    /// no bytes kept at all -- its `HEAD` blob *is* its content, and
    /// [`git::head_blob`] fetches it later if it turns out to have changed.
    fn from_git(profile: &Profile, max_file_bytes: u64) -> Option<Self> {
        let root = profile.root();
        // One question, one subprocess: a project that is not a repository
        // fails at `rev-parse` inside this call and falls through to the walk.
        let entries = git::dirty(root)?;
        let mut snapshot = Self {
            root: root.to_path_buf(),
            origin: Origin::Derived,
            complete: true,
            ..Self::default()
        };
        for entry in entries {
            let observed = if entry.deleted() {
                Observed {
                    state: None,
                    code: Some(entry.code),
                }
            } else {
                let absolute = root.join(&entry.path);
                let Ok(resolved) = profile.check("read", Access::Read, &absolute) else {
                    continue;
                };
                match Self::state_of(&resolved, max_file_bytes, true) {
                    Some(state) => Observed {
                        state: Some(state),
                        code: Some(entry.code),
                    },
                    None => Observed {
                        state: None,
                        code: Some(entry.code),
                    },
                }
            };
            snapshot.files.insert(entry.path, observed);
        }
        Some(snapshot)
    }

    /// The fallback capture: every readable file's length and modification
    /// time, with no total-byte budget, because nothing is being read to find
    /// out whether it changed.
    fn walk(profile: &Profile, max_files: usize, cache_budget: u64, max_file_bytes: u64) -> Self {
        let mut snapshot = Self {
            root: profile.root().to_path_buf(),
            origin: Origin::Scanned,
            complete: true,
            ..Self::default()
        };
        let mut remaining = cache_budget;
        let mut visited = 0usize;
        snapshot.visit(
            profile,
            profile.root(),
            0,
            (max_files, max_file_bytes),
            &mut remaining,
            &mut visited,
        );
        snapshot
    }

    /// Reads one path's state. `keep` asks for its bytes as well, which is
    /// refused above `max_file_bytes` -- the file is still stated, by length
    /// and modification time, so a change to it is still seen.
    fn state_of(resolved: &Path, max_file_bytes: u64, keep: bool) -> Option<FileState> {
        let metadata = fs::metadata(resolved).ok()?;
        if !metadata.is_file() {
            return None;
        }
        let len = metadata.len();
        let modified = metadata.modified().ok();
        if !keep || len > max_file_bytes {
            return Some(FileState {
                len,
                modified,
                digest: None,
                bytes: None,
            });
        }
        let read = fs::File::open(resolved).and_then(|file| {
            let mut bytes = Vec::with_capacity(len as usize);
            file.take(len + 1).read_to_end(&mut bytes)?;
            Ok(bytes)
        });
        match read {
            Ok(bytes) if bytes.len() as u64 == len => {
                let digest: [u8; 32] = Sha256::digest(&bytes).into();
                Some(FileState {
                    len,
                    modified,
                    digest: Some(digest),
                    bytes: Some(bytes),
                })
            }
            // The file changed under the read, or could not be read: its
            // metadata still stands, and the diff will say the content is
            // unavailable rather than pretend it saw it.
            _ => Some(FileState {
                len,
                modified,
                digest: None,
                bytes: None,
            }),
        }
    }

    fn visit(
        &mut self,
        profile: &Profile,
        directory: &Path,
        depth: usize,
        (max_files, max_file_bytes): (usize, u64),
        remaining: &mut u64,
        visited: &mut usize,
    ) {
        if depth > MAX_DEPTH {
            self.incomplete("directory-depth limit reached");
            return;
        }
        let entries = match fs::read_dir(directory) {
            Ok(entries) => entries,
            Err(_) => {
                self.incomplete("a directory could not be read");
                return;
            }
        };
        let mut paths: Vec<PathBuf> = entries.filter_map(Result::ok).map(|e| e.path()).collect();
        paths.sort();
        for path in paths {
            if *visited >= max_files {
                self.incomplete("path-count limit reached");
                break;
            }
            *visited += 1;
            let relative = match path.strip_prefix(profile.root()) {
                Ok(path) => path.to_path_buf(),
                Err(_) => continue,
            };
            let metadata = match fs::symlink_metadata(&path) {
                Ok(metadata) => metadata,
                Err(_) => {
                    self.incomplete("a path changed while it was scanned");
                    continue;
                }
            };
            if metadata.file_type().is_symlink() {
                continue;
            }
            if metadata.is_dir() {
                if relative
                    .components()
                    .next_back()
                    .and_then(|part| part.as_os_str().to_str())
                    .is_some_and(|name| EXCLUDED.contains(&name))
                {
                    continue;
                }
                // Walking names is local bookkeeping; only opening a file is
                // gated. Otherwise an allowed `Read(src/**)` could be hidden
                // behind a root directory that was not itself granted.
                self.visit(
                    profile,
                    &path,
                    depth + 1,
                    (max_files, max_file_bytes),
                    remaining,
                    visited,
                );
                continue;
            }
            if !metadata.is_file() {
                continue;
            }
            let Ok(resolved) = profile.check("read", Access::Read, &path) else {
                continue;
            };
            // Bytes are kept while the cache allows; past it a file is still
            // watched, which is the whole point of separating the two.
            let keep = metadata.len() <= max_file_bytes && metadata.len() <= *remaining;
            let Some(state) = Self::state_of(&resolved, max_file_bytes, keep) else {
                continue;
            };
            if let Some(bytes) = &state.bytes {
                *remaining = remaining.saturating_sub(bytes.len() as u64);
            }
            self.files.insert(
                relative,
                Observed {
                    state: Some(state),
                    code: None,
                },
            );
        }
    }

    fn incomplete(&mut self, note: &str) {
        self.complete = false;
        if !self.notes.iter().any(|held| held == note) {
            self.notes.push(note.to_string());
        }
    }

    /// Whether a path this snapshot does not mention is known to be unchanged.
    ///
    /// True for a derived snapshot, where silence means "identical to `HEAD`";
    /// false for a walked one that hit a limit, where silence means "not
    /// seen".
    fn silence_is_knowledge(&self) -> bool {
        self.origin == Origin::Derived || self.complete
    }

    /// The paths whose state differs between `self` and `after`, relative to
    /// the root, sorted.
    ///
    /// Only paths one of the two snapshots mentions are considered, which for
    /// a derived snapshot is exactly the set git named and for a walked one is
    /// every file it saw.
    pub fn changed_paths(&self, after: &Self) -> Vec<(PathBuf, ChangeKind)> {
        let mut paths: Vec<&PathBuf> = self.files.keys().chain(after.files.keys()).collect();
        paths.sort();
        paths.dedup();
        let mut out = Vec::new();
        for path in paths {
            if let Some(kind) = self.kind_of(after, path) {
                out.push((path.clone(), kind));
            }
        }
        out
    }

    /// How `path` differs between the two snapshots, or `None`.
    fn kind_of(&self, after: &Self, path: &Path) -> Option<ChangeKind> {
        let before = self.files.get(path);
        let now = after.files.get(path);
        match (before, now) {
            (Some(before), Some(now)) => match (&before.state, &now.state) {
                (Some(a), Some(b)) if changed(a, b) => Some(ChangeKind::Modified),
                (Some(_), Some(_)) => None,
                (Some(_), None) => Some(ChangeKind::Deleted),
                (None, Some(_)) => Some(ChangeKind::Created),
                (None, None) => None,
            },
            // Mentioned after and not before: it now differs from what it was,
            // and the only snapshot that can say what it *was* is the one that
            // stayed silent -- which is knowledge only for a derived snapshot.
            (None, Some(now)) => {
                if !self.silence_is_knowledge() {
                    return None;
                }
                match &now.state {
                    None => Some(ChangeKind::Deleted),
                    Some(_) if now.code.is_some_and(Observed::untracked) => {
                        Some(ChangeKind::Created)
                    }
                    // A walked snapshot that saw everything and did not see
                    // this path is watching it appear.
                    Some(_) if self.origin == Origin::Scanned => Some(ChangeKind::Created),
                    Some(_) => Some(ChangeKind::Modified),
                }
            }
            // Mentioned before and not after: for a derived snapshot the cell
            // put it back to `HEAD`, which is a change; for a walked one it is
            // a deletion, and only a complete later walk may say so.
            (Some(before), None) => {
                if !after.silence_is_knowledge() {
                    return None;
                }
                match after.origin {
                    Origin::Derived => match before.state {
                        Some(_) => Some(ChangeKind::Modified),
                        None => Some(ChangeKind::Created),
                    },
                    Origin::Scanned => before.state.as_ref().map(|_| ChangeKind::Deleted),
                }
            }
            (None, None) => None,
        }
    }

    /// The file's state at this snapshot with its bytes filled in where they
    /// can be had: from what was kept, or -- for a path git called clean --
    /// from its `HEAD` blob, which is what that file held.
    fn resolved(&self, path: &Path, limit: u64) -> Option<FileState> {
        match self.files.get(path) {
            // What was kept, and nothing else. **The file is never read
            // now to fill in what a snapshot did not keep**: the disk holds
            // the *current* bytes, and this may be a before-state, so doing
            // so would render a file's diff against itself.
            Some(observed) => observed.state.clone(),
            None if self.origin == Origin::Derived => {
                let bytes = git::head_blob(&self.root, path, limit)?;
                Some(FileState {
                    len: bytes.len() as u64,
                    modified: None,
                    digest: Some(Sha256::digest(&bytes).into()),
                    bytes: Some(bytes),
                })
            }
            None => None,
        }
    }

    /// Renders observed changes and any coverage that was missing. `None`
    /// means nothing changed and nothing was hidden.
    pub fn diff(&self, after: &Self) -> Option<String> {
        let mut out = String::new();
        for (path, kind) in self.changed_paths(after) {
            let before = self.resolved(&path, MAX_DIFF_FILE_BYTES);
            let now = after.resolved(&path, MAX_DIFF_FILE_BYTES);
            match (kind, before, now) {
                (ChangeKind::Created, _, Some(state)) => render_addition(&mut out, &path, &state),
                (ChangeKind::Deleted, Some(state), _) => render_deletion(&mut out, &path, &state),
                (ChangeKind::Modified, Some(before), Some(now)) => {
                    render_change(&mut out, &path, &before, &now)
                }
                // Named, with what is known about it, rather than dropped:
                // the whole objection to a bound that decided what a person
                // was allowed to hear about (2026-09-17).
                (kind, before, now) => {
                    let verb = match kind {
                        ChangeKind::Created => "added",
                        ChangeKind::Deleted => "deleted",
                        ChangeKind::Modified => "changed",
                    };
                    let size = now
                        .or(before)
                        .map(|state| format!(" ({} bytes)", state.len))
                        .unwrap_or_default();
                    out.push_str(&format!(
                        "Content unavailable for {verb} file: {}{size}\n",
                        label(&path)
                    ));
                }
            }
        }
        let mut notes = self.notes.clone();
        for note in &after.notes {
            if !notes.contains(note) {
                notes.push(note.clone());
            }
        }
        if !notes.is_empty() {
            if out.is_empty() {
                // **Never a claim of absence from an incomplete scan.** A file
                // the walk never reached is invisible here rather than
                // unchanged, and a reader told "no changes" would take the
                // stronger of the two readings (2026-09-17, the dogfooding
                // run).
                out.push_str(
                    "No change observed in what was captured; the capture is incomplete, \
                     so a change outside it would not appear here.\n",
                );
            }
            out.push_str("\n[change capture incomplete: ");
            out.push_str(&notes.join("; "));
            out.push_str("]\n");
        }
        if out.is_empty() {
            return None;
        }
        if out.len() > MAX_OUTPUT_BYTES {
            let mut end = MAX_OUTPUT_BYTES;
            while !out.is_char_boundary(end) {
                end -= 1;
            }
            out.truncate(end);
            out.push_str("\n[change output truncated]\n");
        }
        Some(out)
    }

    /// SHA-256 over the sorted (path, content digest) pairs, as hex. Two
    /// snapshots with the same digest hold the same observed file set, so a
    /// later capture can be compared to a verified one without keeping it.
    ///
    /// Modification times are deliberately not hashed: a file restored to its
    /// former contents is the same tree, and a digest that moved with its
    /// timestamp would call that a change.
    pub fn digest(&self) -> String {
        let mut hash = Sha256::new();
        for (path, observed) in &self.files {
            hash.update(label(path).as_bytes());
            hash.update([0]);
            match observed.state.as_ref() {
                Some(state) => match state.digest {
                    Some(digest) => hash.update(digest),
                    None => hash.update(state.len.to_le_bytes()),
                },
                None => hash.update(b"absent"),
            }
            hash.update([0]);
        }
        format!("{:x}", hash.finalize())
    }

    /// Builds an exact rollback for the transition from `self` to `after`.
    ///
    /// A path whose former contents cannot be produced -- too large to have
    /// been kept, and not recoverable from `HEAD` -- refuses the plan by name
    /// rather than reversing part of a cell.
    pub fn rollback_plan(&self, after: &Self) -> Result<RollbackPlan, String> {
        if !self.complete || !after.complete {
            let mut notes = self.notes.clone();
            for note in &after.notes {
                if !notes.contains(note) {
                    notes.push(note.clone());
                }
            }
            return Err(format!(
                "change capture was incomplete{}",
                if notes.is_empty() {
                    String::new()
                } else {
                    format!(": {}", notes.join("; "))
                }
            ));
        }

        let mut expected = BTreeMap::new();
        let mut operations = Vec::new();
        for (path, _) in self.changed_paths(after) {
            let before = self.resolved(&path, MAX_DIFF_FILE_BYTES);
            expected.insert(path.clone(), after.resolved(&path, MAX_DIFF_FILE_BYTES));
            match before {
                Some(state) => {
                    let bytes = state
                        .bytes
                        .clone()
                        .ok_or_else(|| format!("content was not captured for {}", label(&path)))?;
                    operations.push(RollbackOperation::Write { path, bytes });
                }
                None => operations.push(RollbackOperation::Remove { path }),
            }
        }
        if operations.is_empty() {
            return Err("the checkpoint contains no observed file changes".into());
        }
        Ok(RollbackPlan {
            expected,
            operations,
        })
    }
}

impl Observed {
    /// git's letters for a path that is not in `HEAD`.
    fn untracked(code: [u8; 2]) -> bool {
        code == *b"??" || code[0] == b'A'
    }
}

impl RollbackPlan {
    /// Exact affected paths, suitable for a confirmation panel.
    pub fn preview(&self) -> String {
        self.operations
            .iter()
            .map(|operation| match operation {
                RollbackOperation::Write { path, .. } => {
                    format!("restore {}", label(path))
                }
                RollbackOperation::Remove { path } => format!("remove {}", label(path)),
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Applies the plan after proving every affected path still holds what the
    /// cell left there.
    ///
    /// The proof reads the paths themselves rather than re-capturing the
    /// project: it is the same question asked of less of the disk, and it does
    /// not depend on how the capture was sourced.
    pub fn apply(&self, profile: &Profile) -> Result<(), String> {
        for (path, expected) in &self.expected {
            let written = profile.root().join(path);
            let current = profile
                .check("read", Access::Read, &written)
                .ok()
                .and_then(|resolved| Snapshot::state_of(&resolved, MAX_DIFF_FILE_BYTES, true));
            match (expected, &current) {
                (Some(expected), Some(current)) if !changed(expected, current) => {}
                (None, None) => {}
                (None, Some(_)) => {
                    return Err(format!(
                        "{} appeared after the checkpoint; nothing was changed",
                        label(path)
                    ));
                }
                _ => {
                    return Err(format!(
                        "{} changed after the checkpoint; nothing was changed",
                        label(path)
                    ));
                }
            }
            if expected.is_none() && fs::symlink_metadata(&written).is_ok() {
                return Err(format!(
                    "{} appeared after the checkpoint; nothing was changed",
                    label(path)
                ));
            }
            let mut prefix = profile.root().to_path_buf();
            for component in path.components() {
                prefix.push(component);
                if fs::symlink_metadata(&prefix)
                    .is_ok_and(|metadata| metadata.file_type().is_symlink())
                {
                    return Err(format!(
                        "{} now crosses a symlink; nothing was changed",
                        label(path)
                    ));
                }
            }
        }

        // Resolve every destination through the profile before changing the
        // first byte. A denied path therefore cannot leave a partial rollback.
        let targets =
            self.operations
                .iter()
                .map(|operation| {
                    let path = match operation {
                        RollbackOperation::Write { path, .. }
                        | RollbackOperation::Remove { path } => path,
                    };
                    profile
                        .check("rollback", Access::Write, &profile.root().join(path))
                        .map_err(|denied| denied.to_string())
                })
                .collect::<Result<Vec<_>, _>>()?;

        for (operation, target) in self.operations.iter().zip(targets) {
            match operation {
                RollbackOperation::Write { path, bytes } => {
                    if let Some(parent) = target.parent() {
                        fs::create_dir_all(parent).map_err(|error| {
                            format!("could not create {}: {error}", parent.display())
                        })?;
                    }
                    fs::write(&target, bytes)
                        .map_err(|error| format!("could not restore {}: {error}", label(path)))?;
                }
                RollbackOperation::Remove { path } => match fs::remove_file(&target) {
                    Ok(()) => {}
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                    Err(error) => {
                        return Err(format!("could not remove {}: {error}", label(path)));
                    }
                },
            }
        }
        Ok(())
    }
}

/// Whether two states of one path differ.
///
/// A content digest decides it when both sides have one. Otherwise length and
/// modification time do -- which is what a file watcher uses, and what lets a
/// file too large to read still be watched.
fn changed(before: &FileState, after: &FileState) -> bool {
    match (before.digest, after.digest) {
        (Some(a), Some(b)) => a != b,
        _ => before.len != after.len || before.modified != after.modified,
    }
}

fn label(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

fn text(state: &FileState) -> Option<&str> {
    let bytes = state.bytes.as_deref()?;
    if bytes.contains(&0) {
        return None;
    }
    std::str::from_utf8(bytes).ok()
}

fn render_addition(out: &mut String, path: &Path, state: &FileState) {
    let name = label(path);
    match text(state) {
        Some(body) => render_hunk(out, "/dev/null", &format!("b/{name}"), "", body),
        None if state.bytes.is_none() => out.push_str(&format!(
            "Content unavailable for added file: {name} ({} bytes)\n",
            state.len
        )),
        None => out.push_str(&format!(
            "Binary file added: {name} ({} bytes)\n",
            state.len
        )),
    }
}

fn render_deletion(out: &mut String, path: &Path, state: &FileState) {
    let name = label(path);
    match text(state) {
        Some(body) => render_hunk(out, &format!("a/{name}"), "/dev/null", body, ""),
        None if state.bytes.is_none() => out.push_str(&format!(
            "Content unavailable for deleted file: {name} ({} bytes)\n",
            state.len
        )),
        None => out.push_str(&format!(
            "Binary file deleted: {name} ({} bytes)\n",
            state.len
        )),
    }
}

fn render_change(out: &mut String, path: &Path, before: &FileState, after: &FileState) {
    let name = label(path);
    match (text(before), text(after)) {
        (Some(old), Some(new)) => {
            render_hunk(out, &format!("a/{name}"), &format!("b/{name}"), old, new)
        }
        _ if before.bytes.is_none() || after.bytes.is_none() => out.push_str(&format!(
            "Content unavailable for changed file: {name} ({} -> {} bytes)\n",
            before.len, after.len
        )),
        _ => out.push_str(&format!(
            "Binary file changed: {name} ({} -> {} bytes)\n",
            before.len, after.len
        )),
    }
}

fn render_hunk(out: &mut String, old_name: &str, new_name: &str, old: &str, new: &str) {
    let old_lines: Vec<&str> = old.lines().collect();
    let new_lines: Vec<&str> = new.lines().collect();
    if old_lines == new_lines && old != new {
        out.push_str(&format!("--- {old_name}\n+++ {new_name}\n"));
        out.push_str("@@ end-of-file newline @@\n");
        out.push_str(if old.ends_with('\n') == new.ends_with('\n') {
            "Line endings changed (CRLF/LF)\n"
        } else if old.ends_with('\n') {
            "-newline present\n+no newline\n"
        } else {
            "-no newline\n+newline present\n"
        });
        return;
    }
    let prefix = old_lines
        .iter()
        .zip(&new_lines)
        .take_while(|(a, b)| a == b)
        .count();
    let suffix = old_lines[prefix..]
        .iter()
        .rev()
        .zip(new_lines[prefix..].iter().rev())
        .take_while(|(a, b)| a == b)
        .count();
    let old_end = old_lines.len().saturating_sub(suffix);
    let new_end = new_lines.len().saturating_sub(suffix);
    out.push_str(&format!("--- {old_name}\n+++ {new_name}\n"));
    out.push_str(&format!(
        "@@ -{},{} +{},{} @@\n",
        prefix + 1,
        old_end.saturating_sub(prefix),
        prefix + 1,
        new_end.saturating_sub(prefix)
    ));
    for line in &old_lines[prefix..old_end] {
        out.push('-');
        out.push_str(line);
        out.push('\n');
    }
    for line in &new_lines[prefix..new_end] {
        out.push('+');
        out.push_str(line);
        out.push('\n');
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn fixture(name: &str) -> (PathBuf, Profile) {
        let root = std::env::temp_dir().join(format!(
            "sterna-changes-{name}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&root).unwrap();
        let profile =
            Profile::compile(&root, Some(r#"{"permissions":{"allow":["Read","Write"]}}"#));
        (root, profile)
    }

    #[test]
    fn reports_only_changes_after_the_baseline() {
        let (root, profile) = fixture("baseline");
        fs::write(root.join("already-dirty.txt"), "held\n").unwrap();
        fs::write(root.join("changed.txt"), "before\n").unwrap();
        fs::write(root.join("deleted.txt"), "gone\n").unwrap();
        let before = Snapshot::capture_by_walking(&profile);
        fs::write(root.join("changed.txt"), "after\n").unwrap();
        fs::write(root.join("added.txt"), "new\n").unwrap();
        fs::remove_file(root.join("deleted.txt")).unwrap();
        let rendered = before
            .diff(&Snapshot::capture_by_walking(&profile))
            .unwrap();
        assert!(rendered.contains("+++ b/added.txt"), "{rendered}");
        assert!(
            rendered.contains("-before") && rendered.contains("+after"),
            "{rendered}"
        );
        assert!(rendered.contains("--- a/deleted.txt"), "{rendered}");
        assert!(!rendered.contains("already-dirty"), "{rendered}");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn changed_paths_name_each_kind_and_the_digest_tracks_content() {
        let (root, profile) = fixture("changed-paths");
        fs::write(root.join("held.txt"), "same\n").unwrap();
        fs::write(root.join("changed.txt"), "before\n").unwrap();
        fs::write(root.join("deleted.txt"), "gone\n").unwrap();
        let before = Snapshot::capture_by_walking(&profile);
        let unchanged = Snapshot::capture_by_walking(&profile);
        assert_eq!(before.digest(), unchanged.digest());
        assert!(before.changed_paths(&unchanged).is_empty());
        fs::write(root.join("changed.txt"), "after\n").unwrap();
        fs::write(root.join("added.txt"), "new\n").unwrap();
        fs::remove_file(root.join("deleted.txt")).unwrap();
        let after = Snapshot::capture_by_walking(&profile);
        assert_eq!(
            before.changed_paths(&after),
            vec![
                (PathBuf::from("added.txt"), ChangeKind::Created),
                (PathBuf::from("changed.txt"), ChangeKind::Modified),
                (PathBuf::from("deleted.txt"), ChangeKind::Deleted),
            ]
        );
        assert_ne!(before.digest(), after.digest());
        fs::write(root.join("changed.txt"), "before\n").unwrap();
        fs::remove_file(root.join("added.txt")).unwrap();
        fs::write(root.join("deleted.txt"), "gone\n").unwrap();
        assert_eq!(
            before.digest(),
            Snapshot::capture_by_walking(&profile).digest()
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn changed_paths_honour_incomplete_captures() {
        let (root, profile) = fixture("changed-paths-incomplete");
        fs::write(root.join("a.txt"), "a").unwrap();
        fs::write(root.join("b.txt"), "b").unwrap();
        let before = Snapshot::capture_with_limits(&profile, 1, 10, 10);
        fs::write(root.join("0-added.txt"), "new").unwrap();
        fs::remove_file(root.join("b.txt")).unwrap();
        let after = Snapshot::capture_with_limits(&profile, 1, 10, 10);
        assert!(before.changed_paths(&after).is_empty());
        assert!(
            before
                .changed_paths(&Snapshot::capture_by_walking(&profile))
                .iter()
                .all(|(_, kind)| *kind != ChangeKind::Created)
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn exclusions_and_symlinks_are_not_captured() {
        let (root, profile) = fixture("excluded");
        for directory in [".git", ".glasshouse", ".sterna", "node_modules", "target"] {
            fs::create_dir_all(root.join(directory)).unwrap();
            fs::write(root.join(directory).join("noise"), "before").unwrap();
        }
        #[cfg(unix)]
        std::os::unix::fs::symlink("/etc/passwd", root.join("escape")).unwrap();
        let before = Snapshot::capture_by_walking(&profile);
        for directory in [".git", ".glasshouse", ".sterna", "node_modules", "target"] {
            fs::write(root.join(directory).join("noise"), "after").unwrap();
        }
        assert!(
            before
                .diff(&Snapshot::capture_by_walking(&profile))
                .is_none()
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn limits_are_explicit_and_do_not_invent_deletions() {
        let (root, profile) = fixture("limits");
        fs::write(root.join("a.txt"), "a").unwrap();
        fs::write(root.join("b.txt"), "b").unwrap();
        let before = Snapshot::capture_with_limits(&profile, 10, 10, 10);
        fs::remove_file(root.join("a.txt")).unwrap();
        fs::write(root.join("0-added.txt"), "new").unwrap();
        let after = Snapshot::capture_with_limits(&profile, 1, 10, 10);
        let rendered = before.diff(&after).unwrap();
        assert!(rendered.contains("capture incomplete"), "{rendered}");
        assert!(rendered.contains("0-added.txt"), "{rendered}");
        assert!(!rendered.contains("deleted"), "{rendered}");
        assert!(!rendered.contains("--- a/a.txt"), "{rendered}");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn an_incomplete_baseline_does_not_invent_additions() {
        let (root, profile) = fixture("incomplete-before");
        fs::write(root.join("a.txt"), "a").unwrap();
        fs::write(root.join("b.txt"), "b").unwrap();
        let before = Snapshot::capture_with_limits(&profile, 1, 10, 10);
        let rendered = before
            .diff(&Snapshot::capture_by_walking(&profile))
            .unwrap();
        assert!(rendered.contains("No change observed in what was captured"));
        assert!(!rendered.contains("+++"), "{rendered}");
        assert!(!rendered.contains("added file"), "{rendered}");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_file_too_large_to_diff_is_still_watched_and_its_change_named() {
        // Before this split the file was read or not read, and not reading it
        // meant not seeing it: a same-length edit to an oversized file was
        // invisible, disclosed only as "capture incomplete". It is now watched
        // by length and modification time like any other file.
        let (root, profile) = fixture("oversized-mutation");
        let path = root.join("large.bin");
        let mut bytes = vec![b'a'; MAX_DIFF_FILE_BYTES as usize + 1];
        fs::write(&path, &bytes).unwrap();
        let before = Snapshot::capture_by_walking(&profile);
        bytes[0] = b'b';
        fs::write(&path, bytes).unwrap();
        bump_mtime(&path);
        let after = Snapshot::capture_by_walking(&profile);

        assert_eq!(
            before.changed_paths(&after),
            vec![(PathBuf::from("large.bin"), ChangeKind::Modified)]
        );
        let rendered = before.diff(&after).unwrap();
        assert!(
            rendered.contains("Content unavailable for changed file: large.bin"),
            "{rendered}"
        );
        assert!(rendered.contains("bytes)"), "{rendered}");
        assert!(!rendered.contains("capture incomplete"), "{rendered}");
        // Its bytes were never kept, so reversing it is refused by name
        // rather than half-done.
        let error = before.rollback_plan(&after).unwrap_err();
        assert!(error.contains("content was not captured"), "{error}");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn an_entirely_omitted_file_discloses_missing_coverage() {
        let (root, profile) = fixture("omitted-mutation");
        fs::write(root.join("a.txt"), "held").unwrap();
        fs::write(root.join("b.txt"), "before").unwrap();
        let before = Snapshot::capture_with_limits(&profile, 1, 100, 100);
        fs::write(root.join("b.txt"), "after").unwrap();
        let after = Snapshot::capture_with_limits(&profile, 1, 100, 100);

        let rendered = before.diff(&after).unwrap();
        assert!(rendered.contains("No change observed in what was captured"));
        assert!(!rendered.contains("+++"), "{rendered}");
        assert!(!rendered.contains("---"), "{rendered}");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn complete_unchanged_snapshots_remain_quiet() {
        let (root, profile) = fixture("unchanged");
        fs::write(root.join("held.txt"), "unchanged\n").unwrap();
        let before = Snapshot::capture_by_walking(&profile);
        let after = Snapshot::capture_by_walking(&profile);
        assert!(before.complete && after.complete);
        assert!(before.diff(&after).is_none());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn binary_changes_are_summarized() {
        let (root, profile) = fixture("binary");
        fs::write(root.join("blob.bin"), [0, 1, 2]).unwrap();
        let before = Snapshot::capture_by_walking(&profile);
        fs::write(root.join("blob.bin"), [0, 1, 3]).unwrap();
        let rendered = before
            .diff(&Snapshot::capture_by_walking(&profile))
            .unwrap();
        assert!(
            rendered.contains("Binary file changed: blob.bin"),
            "{rendered}"
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn an_end_of_file_newline_change_is_visible() {
        let (root, profile) = fixture("newline");
        fs::write(root.join("line.txt"), "same").unwrap();
        let before = Snapshot::capture_by_walking(&profile);
        fs::write(root.join("line.txt"), "same\n").unwrap();
        let rendered = before
            .diff(&Snapshot::capture_by_walking(&profile))
            .unwrap();
        assert!(rendered.contains("end-of-file newline"), "{rendered}");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn rollback_restores_a_modified_file() {
        let (root, profile) = fixture("rollback-modified");
        fs::write(root.join("changed.txt"), "before\n").unwrap();
        let before = Snapshot::capture_by_walking(&profile);
        fs::write(root.join("changed.txt"), "after\n").unwrap();
        let after = Snapshot::capture_by_walking(&profile);

        let plan = before.rollback_plan(&after).unwrap();
        assert_eq!(plan.preview(), "restore changed.txt");
        plan.apply(&profile).unwrap();

        assert_eq!(
            fs::read_to_string(root.join("changed.txt")).unwrap(),
            "before\n"
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn rollback_removes_a_file_created_by_the_cell() {
        let (root, profile) = fixture("rollback-created");
        let before = Snapshot::capture_by_walking(&profile);
        fs::write(root.join("created.txt"), "cell\n").unwrap();
        let after = Snapshot::capture_by_walking(&profile);

        let plan = before.rollback_plan(&after).unwrap();
        assert_eq!(plan.preview(), "remove created.txt");
        plan.apply(&profile).unwrap();

        assert!(!root.join("created.txt").exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn rollback_recreates_a_file_deleted_by_the_cell() {
        let (root, profile) = fixture("rollback-deleted");
        fs::write(root.join("deleted.txt"), "original\n").unwrap();
        let before = Snapshot::capture_by_walking(&profile);
        fs::remove_file(root.join("deleted.txt")).unwrap();
        let after = Snapshot::capture_by_walking(&profile);

        before
            .rollback_plan(&after)
            .unwrap()
            .apply(&profile)
            .unwrap();

        assert_eq!(
            fs::read_to_string(root.join("deleted.txt")).unwrap(),
            "original\n"
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn rollback_preserves_unrelated_files_and_refuses_later_edits() {
        let (root, profile) = fixture("rollback-unrelated");
        fs::write(root.join("changed.txt"), "before\n").unwrap();
        let before = Snapshot::capture_by_walking(&profile);
        fs::write(root.join("changed.txt"), "after\n").unwrap();
        let after = Snapshot::capture_by_walking(&profile);
        let plan = before.rollback_plan(&after).unwrap();

        fs::write(root.join("unrelated.txt"), "user\n").unwrap();
        plan.apply(&profile).unwrap();
        assert_eq!(
            fs::read_to_string(root.join("unrelated.txt")).unwrap(),
            "user\n"
        );

        fs::write(root.join("changed.txt"), "newer user edit\n").unwrap();
        let error = plan.apply(&profile).unwrap_err();
        assert!(error.contains("changed after the checkpoint"), "{error}");
        assert_eq!(
            fs::read_to_string(root.join("changed.txt")).unwrap(),
            "newer user edit\n"
        );
        fs::remove_dir_all(root).unwrap();
    }

    /// Moves a file's modification time forward so a test never depends on
    /// the filesystem's timestamp granularity.
    fn bump_mtime(path: &Path) {
        let handle = fs::File::options().write(true).open(path).unwrap();
        let later = SystemTime::now() + std::time::Duration::from_secs(2);
        handle
            .set_times(fs::FileTimes::new().set_modified(later))
            .unwrap();
    }

    #[test]
    fn a_tree_larger_than_any_content_budget_is_captured_completely() {
        // 18 MiB over a 1 KiB content budget: the old capture read bytes to
        // decide what changed, so a tree this size declared itself incomplete
        // and said nothing about its own changes. Measured on the real repo
        // that provoked this: 929 files, 25.55 MiB, incomplete every time.
        let (root, profile) = fixture("large-tree");
        for index in 0..20 {
            fs::write(
                root.join(format!("bulk-{index:02}.txt")),
                vec![b'x'; 900 * 1024],
            )
            .unwrap();
        }
        let before = Snapshot::capture_with_limits(&profile, MAX_FILES, 1024, MAX_DIFF_FILE_BYTES);
        assert!(
            before.complete,
            "a metadata walk has no byte budget to exceed"
        );

        fs::write(root.join("bulk-07.txt"), vec![b'y'; 900 * 1024]).unwrap();
        bump_mtime(&root.join("bulk-07.txt"));
        let after = Snapshot::capture_with_limits(&profile, MAX_FILES, 1024, MAX_DIFF_FILE_BYTES);
        assert!(after.complete);
        assert_eq!(
            before.changed_paths(&after),
            vec![(PathBuf::from("bulk-07.txt"), ChangeKind::Modified)]
        );
        let rendered = before.diff(&after).unwrap();
        assert!(!rendered.contains("capture incomplete"), "{rendered}");
        assert!(rendered.contains("bulk-07.txt"), "{rendered}");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_change_that_keeps_the_length_is_seen() {
        let (root, profile) = fixture("same-length");
        fs::write(root.join("same.txt"), "aaaa").unwrap();
        let before = Snapshot::capture_by_walking(&profile);
        fs::write(root.join("same.txt"), "bbbb").unwrap();
        let after = Snapshot::capture_by_walking(&profile);
        assert_eq!(
            before.changed_paths(&after),
            vec![(PathBuf::from("same.txt"), ChangeKind::Modified)]
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn without_content_a_state_is_compared_by_length_and_time() {
        // The decisive unit: a file too large to have been read is compared on
        // what a snapshot always has. Dropping either half of this pair makes
        // a same-length edit invisible again.
        let epoch = SystemTime::UNIX_EPOCH;
        let state = |len: u64, at: SystemTime| FileState {
            len,
            modified: Some(at),
            digest: None,
            bytes: None,
        };
        let later = epoch + std::time::Duration::from_secs(1);
        assert!(!changed(&state(4, epoch), &state(4, epoch)));
        assert!(changed(&state(4, epoch), &state(5, epoch)));
        assert!(
            changed(&state(4, epoch), &state(4, later)),
            "a same-length edit moves the modification time and nothing else"
        );
    }

    #[test]
    fn nothing_changed_is_said_differently_from_nothing_seen() {
        let (root, profile) = fixture("quiet-versus-blind");
        fs::write(root.join("a.txt"), "a").unwrap();
        fs::write(root.join("b.txt"), "b").unwrap();
        let complete = Snapshot::capture_by_walking(&profile);
        assert!(
            complete
                .diff(&Snapshot::capture_by_walking(&profile))
                .is_none()
        );

        let blind = Snapshot::capture_with_limits(&profile, 1, 1024, MAX_DIFF_FILE_BYTES);
        let rendered = blind.diff(&Snapshot::capture_by_walking(&profile)).unwrap();
        assert!(rendered.contains("capture is incomplete"), "{rendered}");
        fs::remove_dir_all(root).unwrap();
    }

    /// A git fixture with one committed file, or `None` where git will not run.
    fn repository(name: &str) -> Option<(PathBuf, Profile)> {
        let (root, profile) = fixture(name);
        let git = |args: &[&str]| {
            std::process::Command::new("git")
                .args(args)
                .current_dir(&root)
                .output()
                .ok()
                .is_some_and(|out| out.status.success())
        };
        fs::write(root.join("tracked.txt"), "committed\n").unwrap();
        let ready = git(&["init", "--quiet"])
            && git(&["config", "user.email", "sterna@example.invalid"])
            && git(&["config", "user.name", "sterna"])
            && git(&["add", "tracked.txt"])
            && git(&["commit", "--quiet", "-m", "base"]);
        if !ready {
            println!("skipped: git is not usable here");
            let _ = fs::remove_dir_all(&root);
            return None;
        }
        Some((root, profile))
    }

    #[test]
    fn a_committed_file_is_diffed_against_head_without_having_been_read_first() {
        // The point of deriving: the baseline held no bytes for this file --
        // git called it clean -- and the diff is still exact, because a clean
        // file's content is its HEAD blob.
        let Some((root, profile)) = repository("git-clean-edit") else {
            return;
        };
        let before = Snapshot::capture(&profile);
        assert!(
            !before.files.contains_key(Path::new("tracked.txt")),
            "a clean file costs the baseline nothing"
        );

        fs::write(root.join("tracked.txt"), "edited by the cell\n").unwrap();
        let after = Snapshot::capture(&profile);
        assert_eq!(
            before.changed_paths(&after),
            vec![(PathBuf::from("tracked.txt"), ChangeKind::Modified)]
        );
        let rendered = before.diff(&after).unwrap();
        assert!(rendered.contains("-committed"), "{rendered}");
        assert!(rendered.contains("+edited by the cell"), "{rendered}");

        before
            .rollback_plan(&after)
            .unwrap()
            .apply(&profile)
            .unwrap();
        assert_eq!(
            fs::read_to_string(root.join("tracked.txt")).unwrap(),
            "committed\n"
        );
        fs::remove_dir_all(root).unwrap();
    }

    /// **The same directory reached by another spelling is the same
    /// directory.** git answers `rev-parse --show-toplevel` in the path it
    /// resolved, and a session's root is the path it was given. When the two
    /// spellings differ, `dirty` used to drop every entry and report
    /// `Some(vec![])` -- "nothing changed", for a tree that had just been
    /// edited, with nothing in the answer to say so.
    ///
    /// Measured on the sweep of 2026-09-19: both git-backed tests failed on
    /// `windows-latest` with `left: []`, deterministically, the rerun-alone
    /// included. The runner's `TEMP` is an 8.3 short name and git prints the
    /// long one. A symlink is the same mismatch on a host that has them.
    #[cfg(unix)]
    #[test]
    fn a_root_spelled_another_way_still_sees_what_git_sees() {
        let Some((root, _)) = repository("git-spelling") else {
            return;
        };
        let link = root.with_extension("link");
        let _ = fs::remove_file(&link);
        std::os::unix::fs::symlink(&root, &link).unwrap();
        let profile =
            Profile::compile(&link, Some(r#"{"permissions":{"allow":["Read","Write"]}}"#));

        let before = Snapshot::capture(&profile);
        fs::write(link.join("tracked.txt"), "edited through the other name\n").unwrap();
        let after = Snapshot::capture(&profile);
        assert_eq!(
            before.changed_paths(&after),
            vec![(PathBuf::from("tracked.txt"), ChangeKind::Modified)],
            "a root spelled another way reported no changes at all"
        );

        let _ = fs::remove_file(&link);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_created_file_and_a_deleted_one_are_told_apart_by_what_git_knows() {
        let Some((root, profile)) = repository("git-kinds") else {
            return;
        };
        let before = Snapshot::capture(&profile);
        fs::write(root.join("new.txt"), "fresh\n").unwrap();
        fs::remove_file(root.join("tracked.txt")).unwrap();
        let after = Snapshot::capture(&profile);
        assert_eq!(
            before.changed_paths(&after),
            vec![
                (PathBuf::from("new.txt"), ChangeKind::Created),
                (PathBuf::from("tracked.txt"), ChangeKind::Deleted),
            ]
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_file_written_and_deleted_within_one_cell_is_not_seen() {
        // The known gap of deriving from state rather than from actions: both
        // snapshots agree the path does not exist. Closing it needs the cell's
        // own tool records, which `session.rs` holds and does not yet pass in.
        let Some((root, profile)) = repository("git-transient") else {
            return;
        };
        let before = Snapshot::capture(&profile);
        fs::write(root.join("scratch.tmp"), "transient\n").unwrap();
        fs::remove_file(root.join("scratch.tmp")).unwrap();
        let after = Snapshot::capture(&profile);
        assert!(before.changed_paths(&after).is_empty());
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn rollback_refuses_a_symlink_that_replaced_a_deleted_path() {
        let (root, profile) = fixture("rollback-symlink");
        fs::write(root.join("deleted.txt"), "original\n").unwrap();
        fs::write(root.join("unrelated.txt"), "user\n").unwrap();
        let before = Snapshot::capture_by_walking(&profile);
        fs::remove_file(root.join("deleted.txt")).unwrap();
        let after = Snapshot::capture_by_walking(&profile);
        let plan = before.rollback_plan(&after).unwrap();
        std::os::unix::fs::symlink("unrelated.txt", root.join("deleted.txt")).unwrap();

        let error = plan.apply(&profile).unwrap_err();
        assert!(error.contains("appeared after the checkpoint"), "{error}");
        assert_eq!(
            fs::read_to_string(root.join("unrelated.txt")).unwrap(),
            "user\n"
        );
        fs::remove_dir_all(root).unwrap();
    }
}
