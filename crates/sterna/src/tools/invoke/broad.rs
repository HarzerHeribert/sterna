//! What a broad search covers, and what it leaves alone.
//!
//! **One place, because every answer here is the same answer.** A search
//! aimed at the project root means the project, not the trees the project
//! generates: git's internals, Sterna's own rollout, and whatever the
//! project's `.gitignore` calls generated. A search aimed *into* one of
//! those means that one, and reads it whole. Split out of `invoke.rs` on
//! 2026-09-20 for the Phase 59 size ratchet, when routing `grep` through
//! ripgrep pushed the file over the ceiling; nothing here is new but
//! `ignored_directories` and `ripgrep_is_installed`.

use super::*;

/// Directory names a broad `grep` skips, taken from the project's own
/// `.gitignore` and from nowhere else.
///
/// **A generated tree is the project's own answer, not a list this file
/// invented.** `grep` has no notion of an ignore file, so a search of a
/// checkout holding a model download and two virtual environments read
/// 9.2 GB to find nothing -- measured at 143 seconds, twice in one session
/// on 2026-09-20 -- while `rg` beside it in the same roster had skipped
/// those directories all along.
///
/// **Only the search root's own `.gitignore`, and only its directory rules.**
/// A gitignore pattern with no slash in it matches at every depth, which is
/// exactly what `--exclude-dir` does, so those rules transfer without
/// changing meaning. Everything else is left alone:
///
/// - a rule from a *subdirectory's* `.gitignore` applies only under that
///   subdirectory, and `--exclude-dir` matches a bare name at any depth --
///   measured: `--exclude-dir=sub/models` excludes nothing at all, so the
///   scope cannot be expressed. Applying it anyway would hide a sibling
///   `models/` that git tracks, and the result shape here is a match list
///   that carries no stderr, so nothing could tell the caller it had
///   happened. A silent miss is the defect this function exists to remove,
///   not a cheaper spelling of it;
/// - a rule naming a file rather than a directory, for the same reason;
/// - a rule with a glob in it, which `--exclude-dir` would read as its own
///   pattern language rather than git's.
///
/// `rg` remains the tool that skips ignored files properly, positionally and
/// at every level, and its summary says so.
pub(super) fn ignored_directories(search_root: &Path) -> Vec<String> {
    let Ok(text) = std::fs::read_to_string(search_root.join(".gitignore")) else {
        return Vec::new();
    };
    let mut names: Vec<String> = text
        .lines()
        .map(str::trim)
        .filter(|rule| !rule.is_empty() && !rule.starts_with('#') && !rule.starts_with('!'))
        // A trailing `/` is git's own spelling of "this is a directory", and
        // it is the only one that says so without guessing.
        .filter(|rule| rule.ends_with('/'))
        .map(|rule| {
            rule.trim_end_matches('/')
                .trim_start_matches('/')
                .to_string()
        })
        .filter(|name| {
            !name.is_empty()
                && !name.contains('/')
                && !name.contains('*')
                && !name.contains('?')
                && !name.contains('[')
        })
        .collect();
    names.sort();
    names.dedup();
    names
}

/// Whether a search rooted at `search_root` is a broad one — not deliberately
/// aimed inside `.sterna` or `.git` — and so omits the generated trees: `.git`
/// is pruned from the walk and the rollout is filtered from the output.
pub(super) fn is_broad_search(project: &Path, search_root: &Path) -> bool {
    !explicitly_roots_component(project, search_root, ".sterna")
        && !explicitly_roots_component(project, search_root, ".git")
}

/// Whether a caller deliberately rooted search inside a generated hidden
/// tree. Direct targeting is an opt-in; a project-root search remains broad.
pub(super) fn explicitly_roots_component(project: &Path, search_root: &Path, wanted: &str) -> bool {
    search_root
        .strip_prefix(project)
        .ok()
        .is_some_and(|relative| contains_component(relative, wanted))
}

pub(super) fn contains_component(path: &Path, wanted: &str) -> bool {
    path.components()
        .any(|component| component.as_os_str() == wanted)
}

/// Generated paths omitted from broad discovery: git's internals and Sterna's
/// own state (see [`is_sterna_artifact`]). Naming `.sterna` in the pattern or
/// the path still reaches it.
pub(super) fn is_search_artifact(relative: &Path) -> bool {
    contains_component(relative, ".git") || is_sterna_artifact(relative)
}

pub(super) fn is_sterna_artifact(relative: &Path) -> bool {
    let components: Vec<_> = relative
        .components()
        .filter_map(|component| component.as_os_str().to_str())
        .collect();
    // What Sterna generates under `.sterna/`: the session transcripts and logs
    // in `sessions/`, and the older transcripts and logs written beside the
    // configuration. What a person keeps there -- `config.toml`,
    // `learned.md`, `scratch/` -- stays findable.
    components.windows(2).any(|pair| {
        pair[0] == ".sterna"
            && (pair[1] == "sessions" || pair[1].ends_with(".jsonl") || pair[1].ends_with(".log"))
    })
}

/// The filename prefix from one `grep -r -n` result. This mirrors the
/// runtime match parser: a colon belongs to the path unless the following
/// non-empty field is an ASCII line number.
pub(super) fn grep_match_path(line: &str) -> Option<&str> {
    let mut start = 0usize;
    while let Some(offset) = line[start..].find(':') {
        let colon = start + offset;
        let rest = &line[colon + 1..];
        if let Some(next) = rest.find(':') {
            let digits = &rest[..next];
            if !digits.is_empty() && digits.bytes().all(|byte| byte.is_ascii_digit()) {
                return Some(&line[..colon]);
            }
        }
        start = colon + 1;
        if start >= line.len() {
            break;
        }
    }
    None
}

pub(super) fn filter_grep_artifacts(project: &Path, output: &str) -> String {
    output
        .lines()
        .filter(|line| {
            grep_match_path(line)
                .and_then(|path| Path::new(path).strip_prefix(project).ok())
                .is_none_or(|relative| !is_search_artifact(relative))
        })
        .map(|line| format!("{line}\n"))
        .collect()
}

/// Whether ripgrep is on this machine, answered once.
///
/// **Once, because the answer decides which binary a search runs on and a
/// search happens many times a cell.** `PATH` does not change under a running
/// session, and resolving it per call would put a directory walk in front of
/// the tool whose whole point here is that it is fast.
pub(super) fn ripgrep_is_installed() -> bool {
    static PRESENT: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *PRESENT.get_or_init(|| resolve_program("rg").is_some())
}
