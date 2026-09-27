//! Named, confined verification commands. Reuse is explicit and scoped to a
//! request and declared file inputs; a reused observation is never a fresh run.
//!
//! [`ShellChecks`] applies the same rule to a plain `bash` check the model
//! writes: **a check that passed, and left the tree as it found it, is not run
//! again while the tree is byte-identical** -- the whole project, keyed by
//! [`crate::changes::Snapshot::tree_key`], stands in for declared inputs.
use std::collections::BTreeMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::sandbox::profile::{Access, Profile};
use crate::tools::invoke::{self, Args, CancellationToken, ToolContext};

const CONFIG_BYTES: u64 = 64 * 1024;
const FILE_BYTES: u64 = 1024 * 1024;
const SNAPSHOT_BYTES: usize = 16 * 1024 * 1024;
const NODES: usize = 4096;
const OUTPUT_CHARS: usize = 8000;

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct CheckConfig {
    #[serde(default)]
    pub checker: Vec<String>,
    #[serde(default)]
    pub checks: BTreeMap<String, CheckSpec>,
    /// The final-state contract, read by `crate::completion`. Optional so an
    /// existing `checks.toml` keeps parsing unchanged.
    #[serde(default)]
    pub contract: Option<ContractSpec>,
}

/// The `[contract]` table: what the finished tree must and must not hold.
/// Every key is optional; `fresh_verification` defaults to true so the gate
/// fails closed for a task that mutated files and never verified them.
#[derive(Debug, Clone, Deserialize, Serialize, Default, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ContractSpec {
    #[serde(default)]
    pub required: Vec<String>,
    #[serde(default)]
    pub forbidden: Vec<String>,
    /// `[contract.exclusive]` — directory to the only entries it may hold.
    #[serde(default)]
    pub exclusive: BTreeMap<String, Vec<String>>,
    #[serde(default)]
    pub coverage_tree: Option<String>,
    #[serde(default = "fresh_verification_default")]
    pub fresh_verification: bool,
}

fn fresh_verification_default() -> bool {
    true
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CheckSpec {
    pub command: String,
    #[serde(default)]
    pub inputs: Vec<String>,
    #[serde(default)]
    pub reuse: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct CheckResult {
    pub name: String,
    pub command: String,
    pub stdout: String,
    pub stderr: String,
    pub exit_code: Option<i32>,
    pub observed_at_ms: u64,
    pub executed: bool,
    pub reused: bool,
    pub reuse_scope: String,
}

/// What a reused check's `stderr` ends with, so the model knows it was not
/// run and how to run it anyway.
pub const REUSED_NOTE: &str = "[sterna: not run again -- this exact check passed earlier in this session on byte-identical files, so its result is repeated. Change a file, or append `&& true`, to run it anyway.]";

/// Whether `command` is checks and nothing else: `&&`-joined simple commands
/// (no pipe, redirection, substitution or environment prefix), each one a
/// test, build, lint or format check. A formatter that writes is not a check,
/// and a check that writes anyway is caught after it runs: it changed the
/// tree, so its result is never kept.
#[must_use]
pub fn is_pure_check(command: &str) -> bool {
    use crate::abi::lift::{self, Family};
    let Some(parts) = and_parts(command) else {
        return false;
    };
    parts.iter().all(|part| {
        let Some(words) = lift::words::split(part) else {
            return false;
        };
        let words: Vec<&str> = words.iter().map(String::as_str).collect();
        matches!(lift::classify(part), Some(Family::Verification))
            || match words.as_slice() {
                ["cargo", "fmt", rest @ ..] => rest.contains(&"--check"),
                ["go", "test" | "vet", ..] | ["mypy", ..] => true,
                ["ruff", "check", rest @ ..] => !rest.iter().any(|w| w.starts_with("--fix")),
                _ => false,
            }
    })
}

/// `command` split on each `&&` outside quotes; `None` for an empty part.
fn and_parts(command: &str) -> Option<Vec<&str>> {
    let bytes = command.as_bytes();
    let (mut parts, mut start, mut quote) = (Vec::new(), 0, None);
    let mut index = 0;
    while index < bytes.len() {
        match (quote, bytes[index]) {
            (None, b'\'' | b'"') => quote = Some(bytes[index]),
            (Some(open), byte) if byte == open => quote = None,
            (None, b'&') if bytes.get(index + 1) == Some(&b'&') => {
                parts.push(command[start..index].trim());
                start = index + 2;
                index += 1;
            }
            _ => {}
        }
        index += 1;
    }
    parts.push(command[start..].trim());
    parts.iter().all(|part| !part.is_empty()).then_some(parts)
}

/// The project files and lines a failing check's output names, in the order
/// they first appear, at most `limit`: `path:line` (Rust, pytest, most
/// tools) and Python's `File "path", line N`. A path outside `root`, or one
/// that is not a file, is not a location in this project and is skipped.
#[must_use]
pub fn failure_locations(output: &str, root: &Path, limit: usize) -> Vec<(PathBuf, usize)> {
    let root = std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
    let mut found: Vec<(PathBuf, usize)> = Vec::new();
    let mut consider = |path: &str, line: &str| {
        let Ok(line) = line.parse::<usize>() else {
            return;
        };
        let candidate = root.join(path);
        let Ok(resolved) = std::fs::canonicalize(&candidate) else {
            return;
        };
        if line > 0
            && resolved.starts_with(&root)
            && resolved.is_file()
            && !found.iter().any(|(p, l)| *p == resolved && *l == line)
        {
            found.push((resolved, line));
        }
    };
    for text_line in output.lines() {
        if let Some(rest) = text_line.trim_start().strip_prefix("File \"")
            && let Some((path, after)) = rest.split_once('"')
            && let Some(number) = after.trim_start_matches(", line ").split(',').next()
        {
            consider(path, number.trim());
        }
        for token in text_line.split(|c: char| c.is_whitespace() || "()'\"`,".contains(c)) {
            let mut parts = token.split(':');
            if let (Some(path), Some(number)) = (parts.next(), parts.next())
                && path.contains('.')
                && !path.is_empty()
            {
                consider(path, number);
            }
        }
    }
    found.truncate(limit);
    found
}

/// Passing plain-`bash` checks, by command line, with the tree they passed on.
#[derive(Default)]
pub struct ShellChecks {
    passed: BTreeMap<String, (String, invoke::ToolResult)>,
}

impl ShellChecks {
    /// Runs `line` through `run`, or repeats its last pass when the tree is
    /// the one it passed on. A pass is kept only when it exited 0 and the
    /// tree before and after it had the same key; anything else forgets it.
    /// No key -- not a repository, or a change that cannot be hashed -- means
    /// it always runs.
    pub(crate) fn run_or_reuse(
        checks: &std::cell::RefCell<Self>,
        profile: &Profile,
        line: &str,
        run: impl FnOnce() -> invoke::Traced,
    ) -> invoke::Traced {
        let before = crate::changes::Snapshot::tree_key(profile);
        let kept = checks.borrow().passed.get(line).cloned();
        if let (Some(tree), Some((passed_on, mut result))) = (&before, kept)
            && *tree == passed_on
            && profile.admits_command(line).is_ok()
        {
            if !result.stderr.is_empty() && !result.stderr.ends_with('\n') {
                result.stderr.push('\n');
            }
            result.stderr.push_str(REUSED_NOTE);
            let checked = BTreeMap::from([
                ("command".to_string(), line.to_string()),
                ("reused".to_string(), "true".to_string()),
            ]);
            return invoke::Traced {
                outcome: Ok(result),
                checked,
            };
        }
        let traced = run();
        let passed = traced
            .outcome
            .as_ref()
            .ok()
            .filter(|result| result.exit_code == Some(0));
        let after = crate::changes::Snapshot::tree_key(profile);
        let mut checks = checks.borrow_mut();
        match (passed, before, after) {
            (Some(result), Some(before), Some(after)) if before == after => {
                checks
                    .passed
                    .insert(line.to_string(), (after, result.clone()));
            }
            _ => {
                checks.passed.remove(line);
            }
        }
        traced
    }
}

#[derive(Default)]
pub struct Verification {
    successes: BTreeMap<String, (String, CheckResult)>,
}

fn bounded_read(profile: &Profile, path: &Path, limit: u64) -> Result<Vec<u8>, String> {
    let path = profile
        .check("checks", Access::Read, path)
        .map_err(|e| e.to_string())?;
    if !std::fs::metadata(&path)
        .map_err(|e| e.to_string())?
        .is_file()
    {
        return Err("verification input must be an ordinary file".into());
    }
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NONBLOCK | libc::O_NOFOLLOW);
    }
    let file = options.open(path).map_err(|e| e.to_string())?;
    if !file.metadata().map_err(|e| e.to_string())?.is_file() {
        return Err("verification input must be an ordinary file".into());
    }
    let mut bytes = Vec::new();
    file.take(limit + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() as u64 > limit {
        return Err("verification input exceeds the bounded read limit".into());
    }
    Ok(bytes)
}

pub fn load(profile: &Profile) -> Result<CheckConfig, String> {
    let path = profile.root().join(".sterna/checks.toml");
    let admitted = profile
        .check("checks", Access::Read, &path)
        .map_err(|e| e.to_string())?;
    if !admitted.exists() {
        return Ok(CheckConfig::default());
    }
    let bytes = bounded_read(profile, &path, CONFIG_BYTES)?;
    let config: CheckConfig =
        toml::from_str(std::str::from_utf8(&bytes).map_err(|e| e.to_string())?)
            .map_err(|e| format!("checks.toml: {e}"))?;
    if config.checks.len() > 16 || config.checker.len() > 4 {
        return Err("checks.toml: at most 16 named checks and four checker checks".into());
    }
    for (name, spec) in &config.checks {
        if name.is_empty()
            || name.len() > 64
            || !name
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
            || spec.command.trim().is_empty()
            || spec.command.len() > 4096
            || spec.inputs.len() > 32
        {
            return Err("checks.toml: invalid name, command or input count".into());
        }
        if spec.reuse && spec.inputs.is_empty() {
            return Err(format!(
                "checks.toml: reusable check `{name}` requires explicit inputs"
            ));
        }
    }
    if config
        .checker
        .iter()
        .any(|name| !config.checks.contains_key(name))
    {
        return Err("checks.toml: checker names an undefined check".into());
    }
    Ok(config)
}

fn fingerprint(profile: &Profile, spec: &CheckSpec, token: &CancellationToken) -> Option<String> {
    if !spec.reuse || spec.inputs.is_empty() {
        return None;
    }
    let mut hash = Sha256::new();
    hash.update(serde_json::to_vec(spec).ok()?);
    hash.update(profile.root().as_os_str().as_encoded_bytes());
    let mut env: Vec<_> = std::env::vars_os().collect();
    env.sort();
    for (key, value) in env {
        hash.update(key.as_encoded_bytes());
        hash.update([0]);
        hash.update(value.as_encoded_bytes());
        hash.update([0]);
    }
    let mut stack: Vec<PathBuf> = spec.inputs.iter().map(|p| profile.root().join(p)).collect();
    let mut nodes = 0;
    let mut bytes = 0;
    while let Some(path) = stack.pop() {
        nodes += 1;
        if nodes > NODES || token.is_cancelled() {
            return None;
        }
        let admitted = profile.check("checks", Access::Read, &path).ok()?;
        let metadata = std::fs::symlink_metadata(&path).ok()?;
        // A dependency outside the declared tree or a special file cannot
        // support a trustworthy reuse key. Fresh execution remains available.
        if metadata.file_type().is_symlink() {
            return None;
        }
        hash.update(admitted.as_os_str().as_encoded_bytes());
        hash.update([0]);
        if metadata.is_dir() {
            let mut children = Vec::new();
            for entry in std::fs::read_dir(admitted).ok()? {
                if nodes + stack.len() + children.len() >= NODES {
                    return None;
                }
                children.push(entry.ok()?.path());
            }
            children.sort();
            stack.extend(children);
        } else if metadata.is_file() {
            let content = bounded_read(profile, &path, FILE_BYTES).ok()?;
            bytes += content.len();
            if bytes > SNAPSHOT_BYTES {
                return None;
            }
            hash.update(content.len().to_le_bytes());
            hash.update(content);
        } else {
            return None;
        }
    }
    Some(format!("{:x}", hash.finalize()))
}

fn output_tail(text: String) -> String {
    let count = text.chars().count();
    if count <= OUTPUT_CHARS {
        return text;
    }
    let tail: String = text.chars().skip(count - OUTPUT_CHARS).collect();
    format!(
        "[checks: {} characters omitted; original output tail]\n{tail}",
        count - OUTPUT_CHARS
    )
}

impl Verification {
    pub fn run(
        &mut self,
        name: &str,
        force: bool,
        ctx: &ToolContext<'_>,
        token: &CancellationToken,
    ) -> Result<CheckResult, String> {
        if token.is_cancelled() {
            return Err("verification cancelled before execution".into());
        }
        let config = load(ctx.profile)?;
        let spec = config.checks.get(name).ok_or_else(|| {
            format!(
                "No check `{name}` in .sterna/checks.toml; define its command before running it"
            )
        })?;
        // Recheck admission even for a cache hit; a result is no authority.
        ctx.profile
            .admits_command(&spec.command)
            .map_err(|e| e.to_string())?;
        let before = fingerprint(ctx.profile, spec, token);
        if !force
            && let Some(key) = &before
            && let Some((saved, result)) = self.successes.get(name)
            && key == saved
        {
            if token.is_cancelled() {
                return Err("verification cancelled before reuse".into());
            }
            let mut result = result.clone();
            result.executed = false;
            result.reused = true;
            return Ok(result);
        }
        self.successes.remove(name);
        let result = invoke::run_cancellable(
            ctx,
            token,
            "bash",
            &Args::new().with("command", &spec.command),
        )
        .map_err(|e| e.to_string())?;
        let after = fingerprint(ctx.profile, spec, token);
        if token.is_cancelled() {
            return Err("verification cancelled during execution".into());
        }
        let observation = CheckResult {
            name: name.into(),
            command: spec.command.clone(),
            stdout: output_tail(result.stdout),
            stderr: output_tail(result.stderr),
            exit_code: result.exit_code,
            observed_at_ms: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis() as u64,
            executed: true,
            reused: false,
            reuse_scope: if before.is_some() && before == after {
                "declared input contents and process environment; external dependencies and nondeterminism require force=true".into()
            } else {
                "not reusable: no complete unchanged declared-input snapshot".into()
            },
        };
        if observation.exit_code == Some(0)
            && let Some(key) = before
            && Some(&key) == after.as_ref()
        {
            self.successes
                .insert(name.into(), (key, observation.clone()));
        }
        Ok(observation)
    }
}

#[cfg(test)]
mod tests {
    use super::is_pure_check;

    #[test]
    fn only_a_line_of_checks_is_a_check() {
        for line in [
            "cargo test -p sterna --test project",
            "cargo fmt --all -- --check && cargo clippy -p sterna",
            "cargo test -p sterna --test project && cargo test -p sterna --test 'tui_look'",
            "pytest -q tests/test_tally.py",
            "ruff check src",
        ] {
            assert!(is_pure_check(line), "{line}");
        }
        for line in [
            "cargo fmt --all",
            "cargo fmt --all && cargo fmt --all -- --check",
            "cargo test && true",
            "cargo test | tail -5",
            "cargo test > log.txt",
            "CARGO_TARGET_DIR=/tmp/t cargo test",
            "ruff check --fix src",
            "git diff --check",
            "cargo test &&",
            "scripts/check.sh",
        ] {
            assert!(!is_pure_check(line), "{line}");
        }
    }
}
