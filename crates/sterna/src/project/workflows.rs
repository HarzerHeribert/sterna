//! Explicitly invoked reusable workflows and host-selected user instructions.

use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};

use crate::contract::ProjectConfig;
use crate::sandbox::profile::{Access, Profile};

const MAX_DOCUMENT_BYTES: u64 = 64 * 1024;

fn read_document(path: &Path) -> Result<String, String> {
    let file = File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut bytes = Vec::new();
    file.take(MAX_DOCUMENT_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| format!("{}: {e}", path.display()))?;
    if bytes.len() as u64 > MAX_DOCUMENT_BYTES {
        return Err(format!("{}: exceeds 64 KiB document limit", path.display()));
    }
    String::from_utf8(bytes).map_err(|_| format!("{}: document is not UTF-8", path.display()))
}

/// The folder's name under [`config_home`].
pub const USER_DIRECTORY: &str = "sterna";

/// User-owned configuration, selected by the host environment, never by a
/// project document or a model tool. This does not grant tools home access.
pub fn user_directory() -> Option<PathBuf> {
    config_home().map(|home| home.join(USER_DIRECTORY))
}

/// `$XDG_CONFIG_HOME` when it is set and absolute, otherwise `~/.config`:
/// the folder every user configuration directory is resolved under.
pub fn config_home() -> Option<PathBuf> {
    std::env::var_os("XDG_CONFIG_HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .or_else(|| {
            std::env::var_os("HOME")
                .or_else(|| std::env::var_os("USERPROFILE"))
                .map(PathBuf::from)
                .filter(|path| path.is_absolute())
                .map(|path| path.join(".config"))
        })
}

/// Complete global instructions, with explicit diagnostics instead of a
/// silently truncated policy. Missing instructions are normal.
pub fn user_instructions_from(directory: &Path) -> Result<String, String> {
    let path = directory.join("AGENTS.md");
    match std::fs::symlink_metadata(&path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(String::new()),
        Err(error) => return Err(format!("{}: {error}", path.display())),
        Ok(_) => {}
    }
    let directory = std::fs::canonicalize(directory).map_err(|e| e.to_string())?;
    let resolved = std::fs::canonicalize(&path).map_err(|e| e.to_string())?;
    if !resolved.starts_with(directory) {
        return Err(
            "global instruction document resolves outside the user configuration directory".into(),
        );
    }
    let body = read_document(&resolved)?;
    Ok(format!(
        "## Global user instructions\n\nUser configuration: {}. These preferences apply across projects; project instructions provide more specific local guidance.\n\n{}\n",
        resolved.display(),
        body
    ))
}

pub fn user_instructions() -> String {
    let Some(directory) = user_directory() else {
        return String::new();
    };
    match user_instructions_from(&directory) {
        Ok(text) => text,
        Err(error) => format!(
            "## Global user instruction load failure\n\n{error}\nDo not assume the missing user instructions were loaded. Tell the user about this omission.\n"
        ),
    }
}

/// Discovery only. Invocation rechecks the immutable session profile.
pub fn skill_available(project: &ProjectConfig, name: &str) -> bool {
    let Some(directory) = project.skills.get(name) else {
        return false;
    };
    let Ok(root) = std::fs::canonicalize(&project.root) else {
        return false;
    };
    let Ok(path) = std::fs::canonicalize(directory.join("SKILL.md")) else {
        return false;
    };
    path.starts_with(root) && read_document(&path).is_ok()
}

/// Expands a skill into an ordinary user task. It gains no permissions and
/// executes no shell interpolation, frontmatter, or embedded commands.
pub fn skill_task(
    project: &ProjectConfig,
    profile: &Profile,
    name: &str,
    arguments: &str,
) -> Result<String, String> {
    let directory = project
        .skills
        .get(name)
        .ok_or_else(|| format!("unknown skill: {name}"))?;
    let path = profile
        .check("skill", Access::Read, &directory.join("SKILL.md"))
        .map_err(|error| error.to_string())?;
    if !path.starts_with(profile.root()) {
        return Err("skill document resolves outside the project root".into());
    }
    let body = read_document(&path)?;
    Ok(format!(
        "Run project skill /{name}.\nSkill source: {}\nResolve relative references against {}. Read referenced instructions through the normal tools before acting. This workflow does not grant extra tool or filesystem permissions.\n\n{body}\n\nUser arguments (literal text):\n{arguments}",
        path.display(),
        path.parent().unwrap_or(profile.root()).display()
    ))
}
