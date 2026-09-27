//! One bounded, read-only snapshot that orients a task before its first cell.

use crate::sandbox::profile::{Access, Profile};
use crate::tools::{invoke, registry};
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File};
use std::io::Read;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

const MAX_TREE: usize = 64;
const MAX_DETECTED: usize = 48;
const MAX_VISITED: usize = 4096;
const MAX_METADATA: u64 = 4096;
const MAX_OUTPUT: usize = 8192;
const SKIP_DIRS: &[&str] = &[
    ".git",
    ".sterna",
    "target",
    "node_modules",
    "build",
    "dist",
    ".next",
    ".venv",
];

/// Collect compact, deterministic environment and repository facts once at
/// task start. Every repository access is checked against `profile`; nothing
/// is created and no project executable or script is run.
pub fn collect(profile: &Profile) -> String {
    let root = profile.root();
    let shell = registry::lookup("bash")
        .and_then(|tool| tool.executable())
        .map(invoke::exec_grant);
    let shell_text = shell
        .as_ref()
        .filter(|grant| !grant.fell_back_to_roots)
        .map(|grant| grant.binary.display().to_string())
        .unwrap_or_else(|| "unknown (not resolved on PATH)".into());

    let (tree, detected) = scan_project(profile);
    let git = git_facts(profile);
    let executables = ["python3", "python", "git", "cargo", "node", "npm"]
        .into_iter()
        .map(|name| {
            invoke::resolve_program(name)
                .map(|path| format!("{name}={}", path.display()))
                .unwrap_or_else(|| format!("{name}=absent"))
        })
        .collect::<Vec<_>>()
        .join("; ");
    let tz = std::env::var("TZ")
        .ok()
        .filter(|value| !value.is_empty())
        .map(|value| {
            value
                .chars()
                .filter(|c| !c.is_control())
                .take(64)
                .collect::<String>()
        });
    let mut out = format!(
        "## Environment orientation\nplatform: {}/{}\ntask-start UTC: {}{}\nexecution shell: {}; version=unknown (not probed)\ncommon executables: {}\nproject root: {}\nshell cwd: reset to project root for every call; shell state does not persist between calls\nscratch suggestion: {} (not created)\n{}\ntop-level (sorted, max {}): {}\ndetected project files (sorted, max {}): {}",
        std::env::consts::OS,
        std::env::consts::ARCH,
        utc_now(),
        tz.map(|value| format!("; TZ={value}")).unwrap_or_default(),
        shell_text,
        executables,
        root.display(),
        root.join(".sterna").join("scratch").display(),
        git,
        MAX_TREE,
        tree,
        MAX_DETECTED,
        detected,
    );
    if out.len() > MAX_OUTPUT {
        out.truncate(floor_char_boundary(&out, MAX_OUTPUT - 24));
        out.push_str("\n… orientation truncated");
    }
    out
}

fn scan_project(profile: &Profile) -> (String, String) {
    let root = profile.root();
    if let Err(denied) = profile.check("Read", Access::Read, root) {
        return (
            format!("refused ({})", denied.rule),
            "unknown (root refused)".into(),
        );
    }
    let Ok(entries) = fs::read_dir(root) else {
        return (
            "unknown (root unreadable)".into(),
            "unknown (root unreadable)".into(),
        );
    };
    let mut names = BTreeSet::new();
    let mut detected = BTreeSet::new();
    let mut candidates = BTreeMap::new();
    let mut names_total = 0usize;
    let mut detected_total = 0usize;
    let mut refused = 0usize;
    let mut tree_cutoff = false;
    for (index, entry) in entries.flatten().enumerate() {
        if index == MAX_VISITED {
            tree_cutoff = true;
            break;
        }
        let path = entry.path();
        let Some(name) = path
            .file_name()
            .and_then(|name| name.to_str())
            .and_then(safe_name)
        else {
            continue;
        };
        if profile.check("Read", Access::Read, &path).is_err() {
            refused += 1;
            continue;
        }
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        let shown = if kind.is_dir() {
            format!("{name}/")
        } else {
            name.clone()
        };
        names_total += 1;
        insert_bounded(&mut names, shown, MAX_TREE);
        if kind.is_file() && interesting(&name) {
            detected_total += 1;
            insert_bounded(&mut detected, name.clone(), MAX_DETECTED);
        }
        if kind.is_dir() && !kind.is_symlink() && !SKIP_DIRS.contains(&name.as_str()) {
            candidates.insert(name, path);
            if candidates.len() > MAX_TREE {
                candidates.pop_last();
            }
        }
    }
    let mut traversal_cutoff = false;
    let mut child_visited = 0usize;
    'directories: for (prefix, dir) in candidates {
        let Ok(children) = fs::read_dir(&dir) else {
            continue;
        };
        for child in children.flatten() {
            if child_visited == MAX_VISITED {
                traversal_cutoff = true;
                break 'directories;
            }
            child_visited += 1;
            let path = child.path();
            if profile.check("Read", Access::Read, &path).is_err() {
                continue;
            }
            let Ok(kind) = child.file_type() else {
                continue;
            };
            if kind.is_file() && !kind.is_symlink() {
                let Some(name) = path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .and_then(safe_name)
                else {
                    continue;
                };
                if interesting(&name) {
                    detected_total += 1;
                    insert_bounded(&mut detected, format!("{prefix}/{name}"), MAX_DETECTED);
                }
            }
        }
    }
    (
        render_set(
            names,
            names_total,
            refused,
            tree_cutoff.then_some("scan limit reached"),
        ),
        render_set(
            detected,
            detected_total,
            0,
            traversal_cutoff.then_some("traversal limit reached"),
        ),
    )
}

fn interesting(name: &str) -> bool {
    matches!(
        name,
        "Cargo.toml"
            | "Cargo.lock"
            | "package.json"
            | "pyproject.toml"
            | "requirements.txt"
            | "go.mod"
            | "Makefile"
            | "justfile"
            | "README"
            | "README.md"
            | "AGENTS.md"
            | "CLAUDE.md"
            | "pytest.ini"
            | "tox.ini"
            | "vitest.config.ts"
            | "jest.config.js"
            | "tsconfig.json"
            | "main.rs"
            | "main.py"
            | "index.ts"
            | "index.js"
    ) || name.ends_with(".csproj")
        || name.ends_with(".sln")
}

fn insert_bounded(values: &mut BTreeSet<String>, value: String, limit: usize) {
    values.insert(value);
    if values.len() > limit {
        values.pop_last();
    }
}

fn render_set(
    values: BTreeSet<String>,
    total: usize,
    refused: usize,
    note: Option<&str>,
) -> String {
    if values.is_empty() {
        let mut notes = Vec::new();
        if refused > 0 {
            notes.push(format!("{refused} refused"));
        }
        if let Some(note) = note {
            notes.push(note.to_string());
        }
        return if notes.is_empty() {
            "(none)".into()
        } else {
            notes.join("; ")
        };
    }
    let mut shown: Vec<String> = values.into_iter().collect();
    if total > shown.len() {
        shown.push(format!("… +{} more", total - shown.len()));
    }
    if refused > 0 {
        shown.push(format!("… {refused} refused"));
    }
    if let Some(note) = note {
        shown.push(format!("… {note}"));
    }
    shown.join(", ")
}

fn git_facts(profile: &Profile) -> String {
    let marker = profile.root().join(".git");
    let marker = match profile.check("Read", Access::Read, &marker) {
        Ok(path) => path,
        Err(_) => return "git: unknown/refused".into(),
    };
    let (git_dir, identity) = if marker.is_dir() {
        (marker.clone(), "main-worktree".to_string())
    } else if marker.is_file() {
        let MetadataRead::Content(text) = read_bounded(profile, &marker) else {
            return "git: unknown/refused".into();
        };
        let Some(raw) = text.trim().strip_prefix("gitdir:") else {
            return "git: unknown (invalid .git file)".into();
        };
        let candidate = marker.parent().unwrap_or(profile.root()).join(raw.trim());
        let Ok(path) = profile.check("Read", Access::Read, &candidate) else {
            return "git: checkout; metadata refused".into();
        };
        let identity = path
            .file_name()
            .and_then(|name| name.to_str())
            .and_then(safe_name)
            .unwrap_or_else(|| "linked".into());
        (path, format!("linked-worktree:{identity}"))
    } else {
        return "git: no checkout".into();
    };
    let branch = match read_bounded(profile, &git_dir.join("HEAD")) {
        MetadataRead::Content(head) => parse_head(&head),
        MetadataRead::Missing | MetadataRead::Refused => "unknown/refused".into(),
    };
    let common = match read_bounded(profile, &git_dir.join("commondir")) {
        MetadataRead::Missing => Some(git_dir.clone()),
        MetadataRead::Content(value) => Some(git_dir.join(value.trim())),
        MetadataRead::Refused => None,
    };
    let common = common
        .and_then(|path| profile.check("Read", Access::Read, &path).ok())
        .map(|path| path.display().to_string())
        .unwrap_or_else(|| "unknown/refused".into());
    format!("git: checkout; identity={identity}; branch={branch}; common-dir={common}")
}

fn parse_head(head: &str) -> String {
    let value = head.trim();
    if let Some(branch) = value.strip_prefix("ref: refs/heads/")
        && !branch.is_empty()
        && branch
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "/._-".contains(c))
    {
        return branch.chars().take(160).collect();
    }
    if value.len() >= 7 && value.chars().all(|c| c.is_ascii_hexdigit()) {
        return format!("detached:{}", value.chars().take(12).collect::<String>());
    }
    "unknown/refused".into()
}

enum MetadataRead {
    Missing,
    Refused,
    Content(String),
}

fn read_bounded(profile: &Profile, path: &Path) -> MetadataRead {
    let path = match profile.check("Read", Access::Read, path) {
        Ok(path) => path,
        Err(_) => return MetadataRead::Refused,
    };
    let metadata = match fs::metadata(&path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return MetadataRead::Missing,
        Err(_) => return MetadataRead::Refused,
    };
    if !metadata.is_file() || metadata.len() > MAX_METADATA {
        return MetadataRead::Refused;
    }
    let file = match File::open(path) {
        Ok(file) => file,
        Err(_) => return MetadataRead::Refused,
    };
    let mut bytes = Vec::new();
    if file.take(MAX_METADATA + 1).read_to_end(&mut bytes).is_err()
        || bytes.len() as u64 > MAX_METADATA
    {
        return MetadataRead::Refused;
    }
    String::from_utf8(bytes)
        .map(MetadataRead::Content)
        .unwrap_or(MetadataRead::Refused)
}

fn utc_now() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let days = (secs / 86_400) as i64;
    let day_secs = secs % 86_400;
    let (year, month, day) = civil_from_days(days);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        day_secs / 3600,
        day_secs / 60 % 60,
        day_secs % 60
    )
}

fn civil_from_days(days_since_epoch: i64) -> (i64, i64, i64) {
    let z = days_since_epoch + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let mut year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = mp + if mp < 10 { 3 } else { -9 };
    year += i64::from(month <= 2);
    (year, month, day)
}

fn floor_char_boundary(text: &str, mut at: usize) -> usize {
    at = at.min(text.len());
    while !text.is_char_boundary(at) {
        at -= 1;
    }
    at
}

fn safe_name(name: &str) -> Option<String> {
    (!name.is_empty() && name.chars().all(|c| !c.is_control()))
        .then(|| name.chars().take(160).collect())
}
