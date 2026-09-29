//! The final-state contract checker and the evidence gate.
//!
//! The invariant: **a finding is a fact about the tree and the trajectory,
//! not an opinion about the narrative.** Every check here reads the project
//! root or the cell numbers the session hands it, and every sentence names
//! the path and what resolves it — three of twelve pilot trials ended
//! "complete" with a wrong filesystem state no one had looked at.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::Read;
use std::path::{Component, Path, PathBuf};

use crate::changes::ChangeKind;
use crate::verification::{CheckConfig, ContractSpec};

const CONFIG_BYTES: u64 = 64 * 1024;
/// The diff the fresh checker sees; the rest is named as omitted.
pub const DIFF_CAP: usize = 24 * 1024;
/// How many directory entries the coverage search will visit.
const WALK_CAP: usize = 20_000;
const ARTIFACT_EXTENSIONS: &[&str] = &[
    "o", "a", "so", "dylib", "obj", "exe", "out", "pyc", "class", "gcno", "gcda", "gcov",
];
const COVERAGE_EXTENSIONS: &[&str] = &["gcno", "gcda", "gcov"];
const SOURCE_EXTENSIONS: &[&str] = &["c", "cc", "cpp", "cxx"];
const SKIPPED_DIRS: &[&str] = &[".git", ".glasshouse", ".sterna", "node_modules", "target"];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Contract {
    pub required_paths: Vec<PathBuf>,
    pub forbidden_patterns: Vec<String>,
    pub exclusive_dirs: Vec<(PathBuf, Vec<String>)>,
    pub coverage_tree: Option<PathBuf>,
    /// Whether a verification that ran and was then overtaken by a mutation
    /// holds the completion (`StaleVerification`).
    pub require_fresh_verification: bool,
    /// Whether the project declares verification at all — named checks or an
    /// explicit `[contract]`. Only then is a mutating task with **no**
    /// verification a finding: a project with nothing configured has no
    /// applicable acceptance contract for a check, and holding every such
    /// completion would tax the common case for no evidence.
    pub verification_expected: bool,
}

impl Default for Contract {
    fn default() -> Self {
        Self {
            required_paths: Vec::new(),
            forbidden_patterns: Vec::new(),
            exclusive_dirs: Vec::new(),
            coverage_tree: None,
            require_fresh_verification: true,
            verification_expected: false,
        }
    }
}

/// The paths a task touched, relative to the root, from `changes`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TaskFiles {
    pub created: BTreeSet<PathBuf>,
    pub modified: BTreeSet<PathBuf>,
    pub deleted: BTreeSet<PathBuf>,
}

impl TaskFiles {
    /// Folds one cell's `Snapshot::changed_paths` in: a path created and
    /// later modified stays created; one created and then deleted is gone.
    pub fn observe(&mut self, changes: &[(PathBuf, ChangeKind)]) {
        for (path, kind) in changes {
            match kind {
                ChangeKind::Created => {
                    self.deleted.remove(path);
                    if !self.modified.contains(path) {
                        self.created.insert(path.clone());
                    }
                }
                ChangeKind::Modified => {
                    if !self.created.contains(path) {
                        self.modified.insert(path.clone());
                    }
                }
                ChangeKind::Deleted => {
                    if !self.created.remove(path) {
                        self.modified.remove(path);
                        self.deleted.insert(path.clone());
                    }
                }
            }
        }
    }

    pub fn is_empty(&self) -> bool {
        self.created.is_empty() && self.modified.is_empty() && self.deleted.is_empty()
    }

    fn present(&self) -> impl Iterator<Item = &PathBuf> {
        self.created.iter().chain(self.modified.iter())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FindingKind {
    RequiredMissing,
    ForbiddenPresent,
    ExclusiveDirExtra,
    UnexpectedArtifact,
    CoverageOutsideTree,
    StaleVerification,
    NoVerification,
    /// An item of the request-derived acceptance list was not met.
    AcceptanceUnmet,
    /// Test files that use a definition the task changed, and that no
    /// command of the task ran (`session::test_check::related_tests`).
    RelatedTestsUnrun,
    /// The decision model reads the task's diff or answer as not satisfying
    /// the request, at or below `[decisions] completion_no_below` (2616).
    RequestNotSatisfied,
    /// One of the five diff-hygiene questions answered decisively against
    /// the diff: no test for the changed behaviour, a file outside the
    /// request's scope, a debugging leftover, a deleted test, or a changed
    /// public signature (2641).
    HygieneIssue,
    /// An acceptance list's `judge` item the decision model reads as not
    /// satisfied, at or below `[decisions] judge_no_below` (2642).
    JudgeNotSatisfied,
    /// The task's last verification command exited non-zero and nothing
    /// changed after it: the answer stands on a check that failed.
    VerificationFailed,
}

impl FindingKind {
    /// Whether this finding holds the answer back once, or only rides along
    /// as a note. **Only a fact holds**: a verification that failed, or the
    /// project's own declared contract broken (`.sterna/checks.toml`).
    /// A cheap model's reading, a derived acceptance item or a stale check is
    /// a note -- measured 2026-09-23: 11 of 11 findings of those kinds held
    /// an attempt that then passed its own test.
    #[must_use]
    pub fn holds(self) -> bool {
        matches!(
            self,
            Self::RequiredMissing
                | Self::ForbiddenPresent
                | Self::ExclusiveDirExtra
                | Self::UnexpectedArtifact
                | Self::CoverageOutsideTree
                | Self::VerificationFailed
                | Self::RelatedTestsUnrun
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Finding {
    pub kind: FindingKind,
    pub path: Option<PathBuf>,
    pub sentence: String,
}

/// Reads `.sterna/checks.toml`'s `[contract]` table. A missing file or
/// a file without the table is the default contract; a path that leaves the
/// root is refused because a contract cannot require anything outside it.
pub fn load_contract(root: &Path) -> Result<Contract, String> {
    let path = root.join(".sterna/checks.toml");
    if !path.is_file() {
        return Ok(Contract::default());
    }
    let mut bytes = Vec::new();
    fs::File::open(&path)
        .map_err(|e| format!("checks.toml: {e}"))?
        .take(CONFIG_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| format!("checks.toml: {e}"))?;
    if bytes.len() as u64 > CONFIG_BYTES {
        return Err("checks.toml: exceeds the bounded read limit".into());
    }
    let config: CheckConfig =
        toml::from_str(std::str::from_utf8(&bytes).map_err(|e| format!("checks.toml: {e}"))?)
            .map_err(|e| format!("checks.toml: {e}"))?;
    let checks_declared = !config.checks.is_empty();
    match config.contract {
        None => Ok(Contract {
            verification_expected: checks_declared,
            ..Contract::default()
        }),
        Some(spec) => {
            let mut contract = contract_from_spec(spec)?;
            contract.verification_expected = true;
            Ok(contract)
        }
    }
}

fn contract_from_spec(spec: ContractSpec) -> Result<Contract, String> {
    let relative = |text: &str| -> Result<PathBuf, String> {
        let path = PathBuf::from(text);
        if text.trim().is_empty()
            || path.is_absolute()
            || path
                .components()
                .any(|c| !matches!(c, Component::Normal(_) | Component::CurDir))
        {
            return Err(format!(
                "checks.toml: [contract] path `{text}` must be relative and stay inside the root"
            ));
        }
        Ok(path)
    };
    Ok(Contract {
        required_paths: spec
            .required
            .iter()
            .map(|p| relative(p))
            .collect::<Result<_, _>>()?,
        forbidden_patterns: spec.forbidden,
        exclusive_dirs: spec
            .exclusive
            .into_iter()
            .map(|(dir, allowed)| relative(&dir).map(|dir| (dir, allowed)))
            .collect::<Result<_, _>>()?,
        coverage_tree: spec.coverage_tree.as_deref().map(relative).transpose()?,
        require_fresh_verification: spec.fresh_verification,
        verification_expected: true,
    })
}

/// Every finding about the finished tree, contract-driven and mechanical.
pub fn check(
    contract: &Contract,
    root: &Path,
    files: &TaskFiles,
    last_verification_cell: Option<u64>,
    last_mutation_cell: Option<u64>,
) -> Vec<Finding> {
    let mut findings = Vec::new();
    for required in &contract.required_paths {
        if !under(root, required).exists() {
            findings.push(Finding {
                kind: FindingKind::RequiredMissing,
                path: Some(under(root, required)),
                sentence: format!(
                    "Create {}; the task requires it as a deliverable and it does not exist.",
                    under(root, required).display()
                ),
            });
        }
    }
    for pattern in &contract.forbidden_patterns {
        for path in files.present() {
            let text = slashed(path);
            let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if glob_match(pattern, &text) || (!pattern.contains('/') && glob_match(pattern, name)) {
                findings.push(Finding {
                    kind: FindingKind::ForbiddenPresent,
                    path: Some(under(root, path)),
                    sentence: format!(
                        "Remove {}; it matches the forbidden pattern `{pattern}`.",
                        under(root, path).display()
                    ),
                });
            }
        }
    }
    for (dir, allowed) in &contract.exclusive_dirs {
        let Ok(entries) = fs::read_dir(under(root, dir)) else {
            continue;
        };
        let mut names: Vec<String> = entries
            .filter_map(Result::ok)
            .filter_map(|e| e.file_name().to_str().map(str::to_owned))
            .collect();
        names.sort();
        for name in names {
            if !allowed.iter().any(|a| a == &name) {
                findings.push(Finding {
                    kind: FindingKind::ExclusiveDirExtra,
                    path: Some(under(root, dir).join(&name)),
                    sentence: format!(
                        "Remove {} or name it in the contract; {} may contain only {}.",
                        under(root, dir).join(&name).display(),
                        under(root, dir).display(),
                        allowed.join(", ")
                    ),
                });
            }
        }
    }

    let mut flagged: BTreeSet<PathBuf> = BTreeSet::new();
    let sources = lazy_sources(root, files);
    for path in &files.created {
        if !is_coverage(path) {
            continue;
        }
        let dir = path.parent().unwrap_or(Path::new(""));
        let outside = match &contract.coverage_tree {
            Some(tree) => (!path.starts_with(tree)).then(|| under(root, tree)),
            None => None,
        };
        let elsewhere = coverage_source_dir(path, &sources)
            .filter(|source_dir| source_dir != dir)
            .map(|source_dir| under(root, &source_dir));
        if let Some(expected) = outside.or(elsewhere) {
            flagged.insert(path.clone());
            findings.push(Finding {
                kind: FindingKind::CoverageOutsideTree,
                path: Some(under(root, path)),
                sentence: format!(
                    "Move {} under {}, where the verifier looks for coverage data, or build with coverage there.",
                    under(root, path).display(),
                    expected.display()
                ),
            });
        }
    }
    let deliverable_dirs: BTreeSet<&Path> = files
        .present()
        .filter(|p| !is_artifact(root, p))
        .filter_map(|p| p.parent())
        .collect();
    for path in &files.created {
        if flagged.contains(path)
            || contract.required_paths.iter().any(|r| r == path)
            || !is_artifact(root, path)
        {
            continue;
        }
        let dir = path.parent().unwrap_or(Path::new(""));
        if !deliverable_dirs.contains(dir) {
            continue;
        }
        let sibling = files
            .present()
            .find(|p| p.parent() == Some(dir) && !is_artifact(root, p))
            .map(|p| under(root, p));
        findings.push(Finding {
            kind: FindingKind::UnexpectedArtifact,
            path: Some(under(root, path)),
            sentence: format!(
                "Remove {} or name it as a deliverable; it is a compiled artifact beside {}.",
                under(root, path).display(),
                sibling.map_or_else(
                    || "task-edited files".to_string(),
                    |p| p.display().to_string()
                )
            ),
        });
    }

    let mutated = last_mutation_cell.is_some() || !files.is_empty();
    if contract.require_fresh_verification && mutated {
        match (last_verification_cell, last_mutation_cell) {
            (Some(verified), Some(mutation)) if mutation > verified => {
                findings.push(Finding {
                    kind: FindingKind::StaleVerification,
                    path: None,
                    sentence: format!(
                        "Re-run the verification before finishing; files changed in cell {mutation} after the last verification in cell {verified}."
                    ),
                });
            }
            (None, _) if contract.verification_expected => {
                findings.push(Finding {
                    kind: FindingKind::NoVerification,
                    path: None,
                    sentence: match last_mutation_cell {
                        Some(cell) => format!(
                            "Run a verification before finishing; files changed in cell {cell} and no check has run."
                        ),
                        None => "Run a verification before finishing; files changed and no check has run.".into(),
                    },
                });
            }
            _ => {}
        }
    }
    findings
}

/// What an independent checker is shown: the request, the diff, the exact
/// facts and the findings — never the parent's narrative, which is the
/// thing it exists to disbelieve.
pub fn fresh_checker_evidence(
    task: &str,
    answer: &str,
    diff: &str,
    facts: &[String],
    findings: &[Finding],
    judge: &[String],
) -> String {
    let mut out = String::from("## Original request\n");
    out.push_str(task);
    // The answer is what the person reads: a claim in it that the work does
    // not bear out is exactly what a second look is for.
    out.push_str("\n\n## The answer\n");
    if answer.trim().is_empty() {
        out.push_str("(no answer text)\n");
    } else {
        out.push_str(answer.trim_end());
        out.push('\n');
    }
    out.push_str("\n## Diff\n");
    if diff.chars().count() > DIFF_CAP {
        let kept: String = diff.chars().take(DIFF_CAP).collect();
        out.push_str(&kept);
        out.push_str(&format!(
            "\n[diff truncated: {} characters omitted]\n",
            diff.chars().count() - DIFF_CAP
        ));
    } else if diff.trim().is_empty() {
        out.push_str("(no observed file changes)\n");
    } else {
        out.push_str(diff);
        if !diff.ends_with('\n') {
            out.push('\n');
        }
    }
    out.push_str("\n## Facts\n");
    if facts.is_empty() {
        out.push_str("(none recorded)\n");
    }
    for fact in facts {
        out.push_str(&format!("- {fact}\n"));
    }
    out.push_str("\n## Final-state findings\n");
    if findings.is_empty() {
        out.push_str("(none)\n");
    }
    for finding in findings {
        out.push_str(&format!("- {}\n", finding.sentence));
    }
    if !judge.is_empty() {
        out.push_str("\n## Acceptance items to judge\n");
        for item in judge {
            out.push_str(&format!("- {item}\n"));
        }
    }
    out.push_str("\nDoes the answer hold, and does the current state satisfy the original request");
    if judge.is_empty() {
        out.push_str("?\n");
    } else {
        out.push_str(", and does each acceptance item above hold? Name any that does not.\n");
    }
    out
}

/// What the work behind an answer amounted to, for [`wants_check`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Work {
    /// Files the task created, modified or deleted.
    pub files: usize,
    /// Lines added plus lines removed in the task's diff.
    pub lines: usize,
    /// Cells the task ran.
    pub cells: u64,
    /// Whether the Scout wrote the task's acceptance list.
    pub scout_list: bool,
}

/// Big work, for [`crate::config::CompletionCheck::Auto`]. Starting points,
/// not measurements: the check's cost is on screen so they can be tuned.
pub const BIG_FILES: usize = 4;
pub const BIG_LINES: usize = 150;
pub const BIG_CELLS: u64 = 10;

/// Whether the checker reads this answer. **A turn that changed nothing is
/// never checked**: a question or a read-only answer leaves nothing in the
/// files to check, and the checker spent its three turns finding that out
/// before it said it could not tell.
#[must_use]
pub fn wants_check(check: crate::config::CompletionCheck, work: &Work) -> bool {
    use crate::config::CompletionCheck;
    if work.files == 0 {
        return false;
    }
    match check {
        CompletionCheck::Off => false,
        CompletionCheck::Always => true,
        CompletionCheck::Auto => {
            work.scout_list
                || work.files >= BIG_FILES
                || work.lines >= BIG_LINES
                || work.cells >= BIG_CELLS
        }
    }
}

/// Lines added plus lines removed in a unified diff, its file headers aside.
#[must_use]
pub fn changed_lines(diff: &str) -> usize {
    diff.lines()
        .filter(|line| {
            (line.starts_with('+') && !line.starts_with("+++"))
                || (line.starts_with('-') && !line.starts_with("---"))
        })
        .count()
}

/// `relative` under `root`, spelled with the platform's one separator. A
/// contract key is written with `/`, and on Windows `Path::join` keeps it
/// verbatim beside a native root, so a finding would name
/// `C:\t\sqlite-gcov-build/btree.gcno`; rebuilding the key from its
/// components (which split on either separator there) spells it natively.
/// Where `/` is already the separator this is exactly `root.join(relative)`.
fn under(root: &Path, relative: &Path) -> PathBuf {
    if std::path::MAIN_SEPARATOR == '/' {
        return root.join(relative);
    }
    root.join(relative.components().collect::<PathBuf>())
}

fn slashed(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

fn extension(path: &Path) -> String {
    path.extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase)
        .unwrap_or_default()
}

fn is_coverage(path: &Path) -> bool {
    COVERAGE_EXTENSIONS.contains(&extension(path).as_str())
}

/// A compiled artifact by extension, or by the first bytes of the file:
/// ELF, Mach-O (thin and fat, either byte order) and PE.
fn is_artifact(root: &Path, relative: &Path) -> bool {
    if ARTIFACT_EXTENSIONS.contains(&extension(relative).as_str()) {
        return true;
    }
    let Ok(mut file) = fs::File::open(root.join(relative)) else {
        return false;
    };
    let mut magic = [0u8; 4];
    let Ok(read) = file.read(&mut magic) else {
        return false;
    };
    read >= 2
        && (&magic[..2] == b"MZ"
            || read == 4
                && matches!(
                    magic,
                    [0x7f, b'E', b'L', b'F']
                        | [0xfe, 0xed, 0xfa, 0xce]
                        | [0xfe, 0xed, 0xfa, 0xcf]
                        | [0xce, 0xfa, 0xed, 0xfe]
                        | [0xcf, 0xfa, 0xed, 0xfe]
                        | [0xca, 0xfe, 0xba, 0xbe]
                ))
}

/// Source files under the root by name, walked only when a coverage file
/// was created; bounded and skipping the directories `changes` skips.
fn lazy_sources(root: &Path, files: &TaskFiles) -> BTreeMap<String, Vec<PathBuf>> {
    let mut sources: BTreeMap<String, Vec<PathBuf>> = BTreeMap::new();
    if !files.created.iter().any(|p| is_coverage(p)) {
        return sources;
    }
    let mut pending = vec![root.to_path_buf()];
    let mut visited = 0;
    while let Some(dir) = pending.pop() {
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.filter_map(Result::ok) {
            visited += 1;
            if visited > WALK_CAP {
                return sources;
            }
            let path = entry.path();
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            if kind.is_symlink() {
                continue;
            }
            let name = entry.file_name().to_string_lossy().into_owned();
            if kind.is_dir() {
                if !SKIPPED_DIRS.contains(&name.as_str()) {
                    pending.push(path);
                }
            } else if kind.is_file() && SOURCE_EXTENSIONS.contains(&extension(&path).as_str()) {
                let relative = path.strip_prefix(root).unwrap_or(&path).to_path_buf();
                sources.entry(name).or_default().push(relative);
            }
        }
    }
    sources
}

/// The directory of the source a coverage file was generated for:
/// `btree.gcno` → `btree.c`; `btree.c.gcov` → `btree.c`.
fn coverage_source_dir(
    coverage: &Path,
    sources: &BTreeMap<String, Vec<PathBuf>>,
) -> Option<PathBuf> {
    let stem = coverage.file_stem()?.to_str()?;
    let candidates: Vec<String> =
        if SOURCE_EXTENSIONS.contains(&extension(Path::new(stem)).as_str()) {
            vec![stem.to_string()]
        } else {
            SOURCE_EXTENSIONS
                .iter()
                .map(|ext| format!("{stem}.{ext}"))
                .collect()
        };
    candidates
        .iter()
        .filter_map(|name| sources.get(name))
        .flat_map(|paths| paths.iter())
        .min()
        .map(|source| source.parent().unwrap_or(Path::new("")).to_path_buf())
}

/// `*` within one segment, `**` across segments, `?` one character.
fn glob_match(pattern: &str, value: &str) -> bool {
    fn matches(pattern: &[u8], value: &[u8], memo: &mut BTreeMap<(usize, usize), bool>) -> bool {
        let key = (pattern.len(), value.len());
        if let Some(answer) = memo.get(&key) {
            return *answer;
        }
        let answer = if pattern.is_empty() {
            value.is_empty()
        } else if pattern.starts_with(b"**") {
            matches(&pattern[2..], value, memo)
                || (!value.is_empty() && matches(pattern, &value[1..], memo))
        } else if pattern[0] == b'*' {
            matches(&pattern[1..], value, memo)
                || (!value.is_empty() && value[0] != b'/' && matches(pattern, &value[1..], memo))
        } else if pattern[0] == b'?' {
            !value.is_empty() && value[0] != b'/' && matches(&pattern[1..], &value[1..], memo)
        } else {
            !value.is_empty() && pattern[0] == value[0] && matches(&pattern[1..], &value[1..], memo)
        };
        memo.insert(key, answer);
        answer
    }
    matches(pattern.as_bytes(), value.as_bytes(), &mut BTreeMap::new())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_contract_table_parses_and_defaults() {
        let config: CheckConfig = toml::from_str(
            "[contract]\nrequired = [\"polyglot/main.py.c\"]\nforbidden = [\"**/*.o\"]\ncoverage_tree = \"sqlite\"\n[contract.exclusive]\n\"polyglot\" = [\"main.py.c\"]\n",
        )
        .unwrap();
        let contract = contract_from_spec(config.contract.unwrap()).unwrap();
        assert_eq!(
            contract.required_paths,
            vec![PathBuf::from("polyglot/main.py.c")]
        );
        assert_eq!(contract.forbidden_patterns, vec!["**/*.o".to_string()]);
        assert_eq!(
            contract.exclusive_dirs,
            vec![(PathBuf::from("polyglot"), vec!["main.py.c".to_string()])]
        );
        assert_eq!(contract.coverage_tree, Some(PathBuf::from("sqlite")));
        assert!(contract.require_fresh_verification);
        let config: CheckConfig =
            toml::from_str("[contract]\nfresh_verification = false\n").unwrap();
        assert!(
            !contract_from_spec(config.contract.unwrap())
                .unwrap()
                .require_fresh_verification
        );
        assert!(toml::from_str::<CheckConfig>("[contract]\nbogus = 1\n").is_err());
        let config: CheckConfig = toml::from_str("[contract]\nrequired = [\"../x\"]\n").unwrap();
        assert!(contract_from_spec(config.contract.unwrap()).is_err());
    }

    #[test]
    fn task_files_fold_a_created_then_deleted_path_away() {
        let mut files = TaskFiles::default();
        files.observe(&[(PathBuf::from("a"), ChangeKind::Created)]);
        files.observe(&[(PathBuf::from("a"), ChangeKind::Modified)]);
        assert!(files.created.contains(Path::new("a")) && files.modified.is_empty());
        files.observe(&[(PathBuf::from("a"), ChangeKind::Deleted)]);
        assert!(files.is_empty());
        files.observe(&[(PathBuf::from("b"), ChangeKind::Modified)]);
        files.observe(&[(PathBuf::from("b"), ChangeKind::Deleted)]);
        assert!(files.deleted.contains(Path::new("b")) && files.modified.is_empty());
    }

    #[test]
    fn globs_match_segments_and_bare_names() {
        assert!(glob_match("**/*.o", "build/x/y.o"));
        assert!(glob_match("*.o", "y.o"));
        assert!(!glob_match("*.o", "build/y.o"));
        assert!(glob_match("build/**", "build/a/b"));
        assert!(glob_match("cmai?", "cmain"));
    }
}
