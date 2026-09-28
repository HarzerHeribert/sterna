//! Acceptance tests for map lines 2448 and 2450: `sterna::project::load`
//! writes nothing it reads, and `sterna::commands` offers every built-in and
//! every project command and skill honestly.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::SystemTime;
use sterna::commands::{self, BuiltIn, CommandSource, CommandStatus};
use sterna::project;

static COUNTER: AtomicU64 = AtomicU64::new(0);

/// A throwaway project directory, removed when the test finishes.
struct Fixture {
    root: PathBuf,
}

impl Fixture {
    fn new(label: &str) -> Self {
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "sterna-project-test-{}-{label}-{n}",
            std::process::id()
        ));
        std::fs::create_dir_all(&root).unwrap();
        Self { root }
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn write(root: &Path, relative: &str, content: &str) {
    let path = root.join(relative);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, content).unwrap();
}

fn write_full_project(root: &Path) {
    write(root, "CLAUDE.md", "# instructions\n");
    write(root, "AGENTS.md", "# agents\n");
    write(root, ".claude/settings.json", "{\"permissions\": {}}\n");
    write(root, ".claude/commands/deploy.md", "deploy the thing\n");
    write(root, ".claude/skills/reviewer/SKILL.md", "review things\n");
    write(root, ".mcp.json", "{\"servers\": {}}\n");
}

fn walk_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if file_type.is_dir() {
            walk_files(&path, out);
        } else if file_type.is_file() {
            out.push(path);
        }
    }
}

fn snapshot(dir: &Path) -> BTreeMap<PathBuf, (Vec<u8>, SystemTime)> {
    let mut files = Vec::new();
    walk_files(dir, &mut files);
    files
        .into_iter()
        .map(|path| {
            let bytes = std::fs::read(&path).unwrap();
            let mtime = std::fs::metadata(&path).unwrap().modified().unwrap();
            (path, (bytes, mtime))
        })
        .collect()
}

#[test]
fn the_loader_writes_nothing() {
    let fixture = Fixture::new("writes-nothing");
    write_full_project(&fixture.root);

    let before = snapshot(&fixture.root);
    let _config = project::load(&fixture.root);
    let after = snapshot(&fixture.root);

    assert_eq!(before, after);
}

#[test]
fn an_absent_claude_md_is_not_an_error() {
    let fixture = Fixture::new("absent-claude-md");
    // No CLAUDE.md, no AGENTS.md, no .claude/ directory at all.

    let config = project::load(&fixture.root);

    assert!(config.instructions.is_empty());
    assert_eq!(config.settings, None);
    assert_eq!(config.mcp, None);
    assert!(config.commands.is_empty());
    assert!(config.skills.is_empty());
}

#[test]
fn a_malformed_settings_json_is_carried_verbatim_not_repaired() {
    let fixture = Fixture::new("malformed-settings");
    let malformed = "{ \"permissions\": [not valid json,\n";
    write(&fixture.root, ".claude/settings.json", malformed);

    let config = project::load(&fixture.root);

    assert_eq!(config.settings.as_deref(), Some(malformed));
}

#[test]
fn settings_and_mcp_are_the_files_exact_bytes() {
    let fixture = Fixture::new("exact-bytes");
    let settings = "{\n  \"permissions\": { \"allow\": [\"Bash(git *)\"] }\n}\n";
    let mcp = "{\n  \"mcpServers\": {}\n}\n";
    write(&fixture.root, ".claude/settings.json", settings);
    write(&fixture.root, ".mcp.json", mcp);

    let config = project::load(&fixture.root);

    let settings_on_disk = std::fs::read(fixture.root.join(".claude/settings.json")).unwrap();
    let mcp_on_disk = std::fs::read(fixture.root.join(".mcp.json")).unwrap();
    assert_eq!(config.settings.unwrap().into_bytes(), settings_on_disk);
    assert_eq!(config.mcp.unwrap().into_bytes(), mcp_on_disk);
}

#[test]
fn every_project_command_and_skill_is_offered_by_name() {
    let fixture = Fixture::new("offered-by-name");
    write(&fixture.root, ".claude/commands/deploy.md", "deploy\n");
    write(&fixture.root, ".claude/commands/status.md", "status\n");
    write(
        &fixture.root,
        ".claude/skills/reviewer/SKILL.md",
        "review\n",
    );
    write(&fixture.root, ".claude/skills/planner/SKILL.md", "plan\n");

    let config = project::load(&fixture.root);
    let all = commands::all(&config);
    let names: Vec<&str> = all.iter().map(|c| c.name.as_str()).collect();

    for builtin in commands::BUILT_INS {
        assert!(
            names.contains(&builtin.name()),
            "missing built-in {}",
            builtin.name()
        );
    }
    for expected in ["deploy", "status", "reviewer", "planner"] {
        assert!(names.contains(&expected), "missing {expected}");
    }
}

#[test]
fn built_in_controls_with_existing_subsystems_are_available() {
    let fixture = Fixture::new("available-controls");
    let config = project::load(&fixture.root);

    let handles = commands::resolve(&config, "handles").unwrap();
    assert_eq!(handles.source, CommandSource::BuiltIn(BuiltIn::Handles));
    assert_eq!(handles.status, CommandStatus::Available);

    let rollback = commands::resolve(&config, "rollback").unwrap();
    assert_eq!(rollback.status, CommandStatus::Available);

    let model = commands::resolve(&config, "model").unwrap();
    assert_eq!(model.status, CommandStatus::Available);
}

#[test]
fn a_project_command_or_skill_never_shadows_a_built_in() {
    let fixture = Fixture::new("collision-builtin");
    // A project ships its own "model" command and its own "rollback" skill --
    // both names a built-in already claims.
    write(
        &fixture.root,
        ".claude/commands/model.md",
        "not the real /model\n",
    );
    write(
        &fixture.root,
        ".claude/skills/rollback/SKILL.md",
        "not the real /rollback\n",
    );

    let config = project::load(&fixture.root);

    let model = commands::resolve(&config, "model").unwrap();
    assert_eq!(model.source, CommandSource::BuiltIn(BuiltIn::Model));

    let rollback = commands::resolve(&config, "rollback").unwrap();
    assert_eq!(rollback.source, CommandSource::BuiltIn(BuiltIn::Rollback));

    // The built-in wins the single "model" and "rollback" slot in the
    // combined listing too -- the project's files do not add a second entry.
    let all = commands::all(&config);
    assert_eq!(all.iter().filter(|c| c.name == "model").count(), 1);
    assert_eq!(all.iter().filter(|c| c.name == "rollback").count(), 1);
}

#[test]
fn a_project_command_beats_a_same_named_skill() {
    let fixture = Fixture::new("collision-command-skill");
    write(
        &fixture.root,
        ".claude/commands/reviewer.md",
        "the command\n",
    );
    write(
        &fixture.root,
        ".claude/skills/reviewer/SKILL.md",
        "the skill\n",
    );

    let config = project::load(&fixture.root);

    let resolved = commands::resolve(&config, "reviewer").unwrap();
    assert_eq!(resolved.source, CommandSource::ProjectCommand);
    assert_eq!(resolved.status, CommandStatus::Available);

    let all = commands::all(&config);
    assert_eq!(all.iter().filter(|c| c.name == "reviewer").count(), 1);
}

#[test]
fn a_discovered_complete_skill_is_available() {
    let fixture = Fixture::new("informational-skill");
    write(
        &fixture.root,
        ".claude/skills/reviewer/SKILL.md",
        "review\n",
    );
    let config = project::load(&fixture.root);
    let resolved = commands::resolve(&config, "reviewer").unwrap();
    assert_eq!(resolved.source, CommandSource::ProjectSkill);
    assert_eq!(resolved.status, CommandStatus::Available);
}

#[cfg(unix)]
#[test]
fn a_symlink_out_of_the_project_root_is_not_followed() {
    let fixture = Fixture::new("symlink-escape");
    let outside = Fixture::new("symlink-escape-outside");
    write(&outside.root, "secret.md", "do not read this\n");
    write(&outside.root, "extra-skill/SKILL.md", "outside skill\n");

    std::fs::create_dir_all(fixture.root.join(".claude/commands")).unwrap();
    std::fs::create_dir_all(fixture.root.join(".claude/skills")).unwrap();
    std::os::unix::fs::symlink(
        outside.root.join("secret.md"),
        fixture.root.join(".claude/commands/leaked.md"),
    )
    .unwrap();
    std::os::unix::fs::symlink(
        outside.root.join("extra-skill"),
        fixture.root.join(".claude/skills/leaked_skill"),
    )
    .unwrap();

    let config = project::load(&fixture.root);

    assert!(!config.commands.contains_key("leaked"));
    assert!(!config.skills.contains_key("leaked_skill"));
}
