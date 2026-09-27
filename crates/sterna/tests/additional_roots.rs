use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
#[cfg(not(target_os = "windows"))]
use sterna::sandbox::profile::Access;
use sterna::sandbox::profile::Profile;

static NEXT: AtomicU64 = AtomicU64::new(0);
struct Fixture(PathBuf);
impl Fixture {
    /// Under the build's scratch folder: the temp folders are writable
    /// places of every profile, and these tests are about what is not.
    fn new() -> Self {
        Self::under(std::path::Path::new(env!("CARGO_TARGET_TMPDIR")))
    }
    fn under(parent: &std::path::Path) -> Self {
        let path = parent.join(format!(
            "sterna-add-dir-{}-{}",
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
    // Without the added root it stays readable (reads are wide) and is
    // not writable.
    assert!(
        fixture
            .profile()
            .check("write", Access::Write, &fixture.0.join("extra/new"))
            .is_err()
    );
    for denied in [home.join(".ssh/id_rsa"), home.join(".config/credentials")] {
        assert!(profile.check("read", Access::Read, &denied).is_err());
        assert!(profile.check("write", Access::Write, &denied).is_err());
    }
    // `~/.gitconfig` stays unwritable: an additional root grants nothing
    // around it.
    let gitconfig = home.join(".gitconfig");
    assert!(profile.check("write", Access::Write, &gitconfig).is_err());
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
    // Reads are wide either way; the added root is what makes it writable,
    // and its sibling stays unwritable.
    assert!(
        profile
            .check("test", Access::Write, &fixture.0.join("extra/new"))
            .is_ok()
    );
    assert!(
        original
            .check("test", Access::Write, &fixture.0.join("extra/new"))
            .is_err()
    );
    assert!(
        profile
            .check("test", Access::Write, &fixture.0.join("outside/new"))
            .is_err()
    );
    assert!(
        original
            .check("test", Access::Read, &fixture.0.join("extra/new"))
            .is_ok()
    );
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
            .check("write", Access::Write, &fixture.0.join("extra/escape/new"))
            .is_err()
    );
}

#[test]
#[cfg(not(target_os = "windows"))]
fn macos_text_names_only_the_added_subtree_and_protects_its_configuration() {
    let fixture = Fixture::new();
    let profile = fixture.profile().with_additional_root("../extra").unwrap();
    let text = sterna::sandbox::macos::profile_text(&profile);
    assert!(text.contains(&format!(
        "(allow file-write* (subpath \"{}\"))",
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
    use sterna::contract::SessionId;
    use sterna::tools::invoke::{self, Args, ToolContext};
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
    let args = Args::new().with("command", "printf allowed > ../extra/result; cat ../extra/result; printf escaped > ../outside/escaped; printf forbidden > ../extra/.claude/settings.json");
    let result = invoke::run(&context, "bash", &args).unwrap();
    assert!(result.stdout.contains("allowed"));
    assert!(!fixture.0.join("outside/escaped").exists());
    assert_eq!(
        std::fs::read_to_string(fixture.0.join("extra/result")).unwrap(),
        "allowed"
    );
    assert!(!fixture.0.join("extra/.claude/settings.json").exists());
}

#[test]
#[cfg(not(target_os = "windows"))]
fn linux_grants_the_added_root_as_a_writable_place_and_nothing_beside_it() {
    let fixture = Fixture::new();
    let profile = Profile::compile(fixture.0.join("project"), None)
        .with_additional_root("../extra")
        .unwrap();
    let rules = sterna::sandbox::linux::landlock_rules(&profile);
    assert!(
        rules.read_write.contains(&fixture.0.join("extra")),
        "{rules:?}"
    );
    assert!(
        !rules.read_write.contains(&fixture.0.join("outside")),
        "{rules:?}"
    );
}
