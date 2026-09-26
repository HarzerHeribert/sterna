#[cfg(not(target_os = "windows"))]
use pane::sandbox::profile::Access;
use pane::sandbox::profile::Profile;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT: AtomicU64 = AtomicU64::new(0);
struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        Self::under(&std::env::temp_dir())
    }
    fn under(parent: &std::path::Path) -> Self {
        let path = parent.join(format!(
            "pane-add-dir-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(path.join("project")).unwrap();
        std::fs::create_dir_all(path.join("extra")).unwrap();
        std::fs::create_dir_all(path.join("outside")).unwrap();
        Self(std::fs::canonicalize(path).unwrap())
    }
    fn profile(&self) -> Profile {
        Profile::compile(self.0.join("project"), None)
    }
}

#[test]
#[cfg(unix)]
fn host_selected_home_subtree_is_accessible_without_granting_home_or_credentials() {
    let Some(home) = std::env::var_os("HOME").map(PathBuf::from) else {
        return;
    };
    let fixture = Fixture::under(&home);
    let profile = fixture.profile().with_additional_root("../extra").unwrap();
    assert!(
        profile
            .check("read", Access::Read, &fixture.0.join("extra/readme"))
            .is_ok()
    );
    assert!(
        profile
            .check("write", Access::Write, &fixture.0.join("extra/new"))
            .is_ok()
    );
    assert!(!profile.executable_is_refused(&fixture.0.join("extra/script")));
    assert!(
        fixture
            .profile()
            .executable_is_refused(&fixture.0.join("extra/script"))
    );
    for denied in [home.join(".ssh/id_rsa"), home.join(".config/credentials")] {
        assert!(profile.check("read", Access::Read, &denied).is_err());
        assert!(profile.check("write", Access::Write, &denied).is_err());
        assert!(profile.executable_is_refused(&denied));
    }
    // `~/.gitconfig` keeps every half this test is about — an additional
    // root grants nothing around it, it is never writable and never
    // executable. Its *read* is a derived host grant since 2026-09-17,
    // because `git` will not start without it; that grant is pinned in
    // `sandbox_profile.rs`, and it is not something `--add-dir` can widen.
    let gitconfig = home.join(".gitconfig");
    assert!(profile.check("write", Access::Write, &gitconfig).is_err());
    assert!(profile.executable_is_refused(&gitconfig));
    assert!(fixture.profile().with_additional_root(&home).is_err());
    assert!(
        fixture
            .profile()
            .with_additional_root(home.join(".ssh"))
            .is_err()
    );
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
#[cfg(not(target_os = "windows"))]
fn explicit_root_is_canonical_relative_to_project_and_does_not_grant_its_sibling() {
    let fixture = Fixture::new();
    let original = fixture.profile();
    let profile = original.clone().with_additional_root("../extra").unwrap();
    assert_eq!(profile.additional_roots(), &[fixture.0.join("extra")]);
    for access in [Access::Read, Access::Write] {
        assert!(
            profile
                .check("test", access, &fixture.0.join("extra/new"))
                .is_ok()
        );
        assert!(
            original
                .check("test", access, &fixture.0.join("extra/new"))
                .is_err()
        );
        assert!(
            profile
                .check("test", access, &fixture.0.join("outside/new"))
                .is_err()
        );
    }
    assert!(
        profile
            .check(
                "test",
                Access::Read,
                &fixture.0.join("extra/.claude/AGENTS.md")
            )
            .is_ok()
    );
    assert!(
        profile
            .check(
                "test",
                Access::Write,
                &fixture.0.join("extra/.claude/settings.json")
            )
            .is_err()
    );
    assert!(
        profile
            .check(
                "test",
                Access::Write,
                &fixture.0.join("project/.claude/settings.json")
            )
            .is_err()
    );
}

#[test]
fn unavailable_roots_and_unrenderable_deny_combinations_refuse() {
    let fixture = Fixture::new();
    assert!(fixture.profile().with_additional_root("../absent").is_err());
    let profile = Profile::compile(
        fixture.0.join("project"),
        Some(r#"{"permissions":{"deny":["Read(private/**)"]}}"#),
    );
    assert!(profile.with_additional_root("../extra").is_err());
    assert!(
        fixture
            .profile()
            .with_additional_root(std::path::MAIN_SEPARATOR.to_string())
            .is_err()
    );
}

#[test]
#[cfg(windows)]
fn windows_refuses_instead_of_claiming_an_unimplemented_extra_acl_grant() {
    let fixture = Fixture::new();
    assert!(
        fixture
            .profile()
            .with_additional_root("../extra")
            .unwrap_err()
            .contains("AppContainer")
    );
}

#[test]
#[cfg(unix)]
fn symlinks_do_not_extend_the_added_subtree() {
    use std::os::unix::fs::symlink;
    let fixture = Fixture::new();
    symlink(fixture.0.join("extra"), fixture.0.join("alias")).unwrap();
    let profile = fixture.profile().with_additional_root("../alias").unwrap();
    assert_eq!(profile.additional_roots(), &[fixture.0.join("extra")]);
    symlink(fixture.0.join("outside"), fixture.0.join("extra/escape")).unwrap();
    assert!(
        profile
            .check("read", Access::Read, &fixture.0.join("extra/escape/secret"))
            .is_err()
    );
    assert!(
        profile
            .check("write", Access::Write, &fixture.0.join("extra/escape/new"))
            .is_err()
    );
}

#[test]
#[cfg(not(target_os = "windows"))]
fn macos_text_names_only_the_added_subtree_and_protects_its_configuration() {
    let fixture = Fixture::new();
    let profile = fixture.profile().with_additional_root("../extra").unwrap();
    let text = pane::sandbox::macos::profile_text(&profile, std::path::Path::new("/bin/cat"));
    assert!(text.contains(&format!(
        "(allow file-read* file-write* (subpath \"{}\"))",
        fixture.0.join("extra").display()
    )));
    assert!(text.contains(&format!(
        "(deny file-write* (subpath \"{}\"))",
        fixture.0.join("extra/.claude").display()
    )));
    assert!(!text.contains(&format!(
        "(subpath \"{}\")",
        fixture.0.join("outside").display()
    )));
}

#[test]
#[cfg(target_os = "macos")]
fn spawned_shell_can_use_extra_directory_but_cannot_escape_or_write_its_config() {
    use pane::contract::SessionId;
    use pane::tools::invoke::{self, Args, ToolContext};
    let fixture = Fixture::new();
    std::fs::create_dir_all(fixture.0.join("extra/.claude")).unwrap();
    std::fs::write(fixture.0.join("outside/secret"), "outside secret").unwrap();
    let profile = Profile::compile(
        fixture.0.join("project"),
        Some(r#"{"permissions":{"allow":["Bash"]}}"#),
    )
    .with_additional_root("../extra")
    .unwrap();
    let session = SessionId::new("extra-root-subprocess");
    let context = ToolContext {
        profile: &profile,
        session: &session,
    };
    let args = Args::new().with("command", "printf allowed > ../extra/result; cat ../extra/result; cat ../outside/secret; printf forbidden > ../extra/.claude/settings.json");
    let result = invoke::run(&context, "bash", &args).unwrap();
    assert!(result.stdout.contains("allowed"));
    assert!(!result.stdout.contains("outside secret"));
    assert_eq!(
        std::fs::read_to_string(fixture.0.join("extra/result")).unwrap(),
        "allowed"
    );
    assert!(!fixture.0.join("extra/.claude/settings.json").exists());
    // A positive unconfined baseline distinguishes sandbox refusal from a
    // fixture that was simply unreadable in the first place.
    assert_eq!(
        std::process::Command::new("/bin/cat")
            .arg(fixture.0.join("outside/secret"))
            .output()
            .unwrap()
            .stdout,
        b"outside secret"
    );
}

#[test]
#[cfg(not(target_os = "windows"))]
fn linux_broad_command_grant_includes_extra_executable_root_but_narrow_grant_does_not() {
    let fixture = Fixture::new();
    let broad = Profile::compile(
        fixture.0.join("project"),
        Some(r#"{"permissions":{"allow":["Bash"]}}"#),
    )
    .with_additional_root("../extra")
    .unwrap();
    let narrow = Profile::compile(
        fixture.0.join("project"),
        Some(r#"{"permissions":{"allow":["Bash(cat*)"]}}"#),
    )
    .with_additional_root("../extra")
    .unwrap();
    let binary = std::path::Path::new("/bin/cat");
    let broad_rules = pane::sandbox::linux::landlock_rules(&broad, binary);
    let narrow_rules = pane::sandbox::linux::landlock_rules(&narrow, binary);
    assert!(broad_rules.executable.contains(&fixture.0.join("extra")));
    assert!(!narrow_rules.executable.contains(&fixture.0.join("extra")));
    assert!(!broad_rules.executable.contains(&fixture.0.join("outside")));
}
