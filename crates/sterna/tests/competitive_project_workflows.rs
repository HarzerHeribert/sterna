use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use sterna::commands::{self, CommandStatus};
use sterna::project::{self, workflows};
use sterna::sandbox::profile::Profile;

static NEXT: AtomicU64 = AtomicU64::new(0);

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "sterna-workflows-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&path).unwrap();
        Self(path)
    }
    fn write(&self, path: &str, body: impl AsRef<[u8]>) {
        let path = self.0.join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, body).unwrap();
    }
    fn root(&self) -> &Path {
        &self.0
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn global_instructions_load_whole_and_do_not_require_project_read_grants() {
    let user = Fixture::new();
    assert_eq!(workflows::user_instructions_from(user.root()).unwrap(), "");
    user.write("AGENTS.md", "Use concise prose.\nKeep my spelling.\n");
    let loaded = workflows::user_instructions_from(user.root()).unwrap();
    assert!(loaded.contains("Use concise prose.\nKeep my spelling.\n"));
    assert!(loaded.contains("Global user instructions"));
}

#[test]
fn oversized_or_invalid_global_instructions_report_an_omission() {
    let user = Fixture::new();
    user.write("AGENTS.md", vec![b'x'; 65_537]);
    assert!(
        workflows::user_instructions_from(user.root())
            .unwrap_err()
            .contains("64 KiB")
    );
    user.write("AGENTS.md", [0xff]);
    assert!(
        workflows::user_instructions_from(user.root())
            .unwrap_err()
            .contains("UTF-8")
    );
}

#[test]
fn skill_invocation_keeps_complete_body_arguments_and_relative_reference_base() {
    let project_dir = Fixture::new();
    project_dir.write(
        ".claude/skills/review/SKILL.md",
        "Read references/rules.md.\nReview the code.\n",
    );
    let project = project::load(project_dir.root());
    let profile = Profile::compile(project_dir.root(), None);
    assert_eq!(
        commands::resolve(&project, "review").unwrap().status,
        CommandStatus::Available
    );
    let task = workflows::skill_task(
        &project,
        &profile,
        "review",
        "$(touch bad) `shell` $ARGUMENTS",
    )
    .unwrap();
    assert!(task.contains("Read references/rules.md.\nReview the code.\n"));
    // The base is named the way the platform spells a path: `.claude\skills\review` on Windows.
    let base = std::path::Path::new(".claude")
        .join("skills")
        .join("review")
        .display()
        .to_string();
    assert!(task.contains(&base), "{task}");
    assert!(task.contains("$(touch bad) `shell` $ARGUMENTS"));
    assert!(!project_dir.root().join("bad").exists());
}

#[test]
fn discovery_cannot_override_a_session_read_deny() {
    let dir = Fixture::new();
    dir.write(".claude/skills/review/SKILL.md", "private workflow");
    let project = project::load(dir.root());
    let profile = Profile::compile(
        dir.root(),
        Some(r#"{"permissions":{"deny":["Read(.claude/skills/**)"]}}"#),
    );
    assert!(workflows::skill_task(&project, &profile, "review", "").is_err());
}

#[test]
fn incomplete_skill_is_informational_and_never_expands_a_prefix() {
    let dir = Fixture::new();
    dir.write(".claude/skills/large/SKILL.md", vec![b'x'; 65_537]);
    std::fs::create_dir_all(dir.root().join(".claude/skills/empty")).unwrap();
    let project = project::load(dir.root());
    let profile = Profile::compile(dir.root(), None);
    for name in ["large", "empty"] {
        assert_eq!(
            commands::resolve(&project, name).unwrap().status,
            CommandStatus::Informational
        );
        assert!(workflows::skill_task(&project, &profile, name, "").is_err());
    }
}

#[test]
#[cfg(unix)]
fn symlink_escape_is_rejected_for_user_instructions_and_skill_invocation() {
    use std::os::unix::fs::symlink;
    let dir = Fixture::new();
    let outside = Fixture::new();
    outside.write("secret", "private");
    symlink(outside.root().join("secret"), dir.root().join("AGENTS.md")).unwrap();
    assert!(workflows::user_instructions_from(dir.root()).is_err());

    dir.write(".claude/skills/review/SKILL.md", "initial workflow");
    let project = project::load(dir.root());
    let profile = Profile::compile(dir.root(), None);
    let skill = dir.root().join(".claude/skills/review/SKILL.md");
    std::fs::remove_file(&skill).unwrap();
    symlink(outside.root().join("secret"), skill).unwrap();
    assert!(workflows::skill_task(&project, &profile, "review", "").is_err());
    assert!(!workflows::skill_available(&project, "review"));
}
