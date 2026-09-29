use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use sterna::project::orientation;
use sterna::sandbox::profile::Profile;

static NEXT: AtomicU64 = AtomicU64::new(0);

struct Fixture {
    root: PathBuf,
}
impl Fixture {
    fn new(label: &str) -> Self {
        let root = std::env::temp_dir().join(format!(
            "sterna-orientation-{}-{label}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&root).unwrap();
        Self { root }
    }
    fn profile(&self, permissions: &str) -> Profile {
        Profile::compile(&self.root, Some(permissions))
    }
    /// The root as a settings pattern writes it, exactly as
    /// `sandbox_profile.rs`'s fixture does: see [`pattern_path`].
    fn pattern_root(&self) -> String {
        pattern_path(&self.root)
    }
}

/// `path` as a settings pattern writes it: forward slashes.
///
/// The separator is the escaping, not a preference. A settings document is
/// JSON, and a Windows path interpolated into one carries `\U`, `\A`, `\T`
/// … — invalid JSON escapes — so the document fails to parse and compiles to
/// no rules at all, which is a grant rather than the denial the test asked
/// for. `Profile` folds `\` to `/` on every host, so the forward-slashed
/// spelling names the same path everywhere.
fn pattern_path(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

#[test]
fn facts_name_real_environment_and_sorted_bounded_project_files() {
    let fixture = Fixture::new("facts");
    std::fs::write(fixture.root.join("Cargo.toml"), "[package]\n").unwrap();
    std::fs::write(fixture.root.join("README.md"), "hello\n").unwrap();
    std::fs::create_dir(fixture.root.join("src")).unwrap();
    std::fs::write(fixture.root.join("src/main.rs"), "fn main() {}\n").unwrap();
    for (dir, file) in [("zeta", "package.json"), ("alpha", "pyproject.toml")] {
        std::fs::create_dir(fixture.root.join(dir)).unwrap();
        std::fs::write(fixture.root.join(dir).join(file), "fixture\n").unwrap();
    }
    for i in 0..100 {
        std::fs::write(fixture.root.join(format!("item-{i:03}")), "x").unwrap();
    }
    let profile = fixture.profile(r#"{"permissions":{}}"#);
    let text = orientation::collect(&profile);
    assert!(
        text.contains(&format!(
            "platform: {}/{}",
            std::env::consts::OS,
            std::env::consts::ARCH
        )),
        "{text}"
    );
    assert!(
        text.contains("task-start UTC: 20") && text.contains('T') && text.contains('Z'),
        "{text}"
    );
    assert!(
        text.contains(&format!("project root: {}", profile.root().display())),
        "{text}"
    );
    assert!(
        text.contains("execution shell:") && text.contains("shell cwd: reset to project root"),
        "{text}"
    );
    assert!(text.contains("common executables: python3="), "{text}");
    assert!(text.contains(
        "detected project files (sorted, max 48): Cargo.toml, README.md, alpha/pyproject.toml, src/main.rs, zeta/package.json"
    ), "{text}");
    assert!(
        text.find("alpha/pyproject.toml") < text.find("zeta/package.json"),
        "detected paths were not sorted independently of creation order: {text}"
    );
    assert!(
        text.contains("… +"),
        "the large tree was not visibly bounded: {text}"
    );
    assert!(text.len() <= 8192);
    assert!(!fixture.root.join(".sterna/scratch").exists());
}

#[test]
fn linked_checkout_reports_identity_branch_and_common_dir() {
    let fixture = Fixture::new("linked");
    let git_dir = fixture.root.join("metadata/worktrees/topic");
    let common = fixture.root.join("metadata");
    std::fs::create_dir_all(&git_dir).unwrap();
    std::fs::write(
        fixture.root.join(".git"),
        "gitdir: metadata/worktrees/topic\n",
    )
    .unwrap();
    std::fs::write(
        git_dir.join("HEAD"),
        "ref: refs/heads/feature/orientation\n",
    )
    .unwrap();
    std::fs::write(git_dir.join("commondir"), "../..\n").unwrap();
    let text = orientation::collect(&fixture.profile(r#"{"permissions":{}}"#));
    assert!(
        text.contains("git: checkout; identity=linked-worktree:topic"),
        "{text}"
    );
    assert!(text.contains("branch=feature/orientation"), "{text}");
    assert!(
        text.contains(&format!(
            "common-dir={}",
            std::fs::canonicalize(common).unwrap().display()
        )),
        "{text}"
    );
}

#[test]
fn denied_git_metadata_is_reported_unknown_without_reading_it() {
    let fixture = Fixture::new("denied");
    std::fs::create_dir(fixture.root.join(".git")).unwrap();
    std::fs::write(
        fixture.root.join(".git/HEAD"),
        "ref: refs/heads/secret-name\n",
    )
    .unwrap();
    let root = fixture.pattern_root();
    let settings =
        format!(r#"{{"permissions":{{"deny":["Read({root}/.git)","Read({root}/.git/**)"]}}}}"#);
    let text = orientation::collect(&fixture.profile(&settings));
    assert!(text.contains("git: unknown/refused"), "{text}");
    assert!(!text.contains("secret-name"), "{text}");
}

#[test]
fn denied_commondir_is_not_reported_as_the_git_directory() {
    let fixture = Fixture::new("denied-commondir");
    let git_dir = fixture.root.join("metadata/worktrees/topic");
    std::fs::create_dir_all(&git_dir).unwrap();
    std::fs::write(
        fixture.root.join(".git"),
        "gitdir: metadata/worktrees/topic\n",
    )
    .unwrap();
    std::fs::write(git_dir.join("HEAD"), "ref: refs/heads/visible\n").unwrap();
    std::fs::write(git_dir.join("commondir"), "../..\n").unwrap();
    let denied = git_dir.join("commondir");
    let settings = format!(
        r#"{{"permissions":{{"deny":["Read({})"]}}}}"#,
        pattern_path(&denied)
    );
    let text = orientation::collect(&fixture.profile(&settings));
    assert!(text.contains("branch=visible"), "{text}");
    assert!(text.contains("common-dir=unknown/refused"), "{text}");
    assert!(
        !text.contains(&format!("common-dir={}", git_dir.display())),
        "{text}"
    );
}

#[test]
fn huge_top_level_tree_has_an_observable_scan_ceiling() {
    let fixture = Fixture::new("scan-ceiling");
    for i in 0..4_100 {
        std::fs::write(fixture.root.join(format!("entry-{i:04}")), "x").unwrap();
    }
    let text = orientation::collect(&fixture.profile(r#"{"permissions":{}}"#));
    assert!(text.contains("scan limit reached"), "{text}");
    assert!(text.len() <= 8192, "{}", text.len());
}

#[test]
#[cfg(unix)]
fn symlink_escape_is_not_followed_or_reported_as_project_content() {
    let fixture = Fixture::new("symlink");
    let outside = fixture.root.with_extension("outside");
    std::fs::create_dir_all(&outside).unwrap();
    std::fs::write(outside.join("Cargo.toml"), "secret marker\n").unwrap();
    std::os::unix::fs::symlink(&outside, fixture.root.join("escaped")).unwrap();
    let text = orientation::collect(&fixture.profile(r#"{"permissions":{}}"#));
    // Reads outside the project are granted, so the link itself is a
    // project entry and may be named; what orientation guarantees is that
    // it is never walked: not shown as a directory, and nothing behind it
    // is detected as a project file.
    assert!(!text.contains("escaped/"), "{text}");
    assert!(!text.contains("Cargo.toml"), "{text}");
    assert!(!text.contains("secret marker"), "{text}");
    let _ = std::fs::remove_dir_all(outside);
}

/// The project's MCP servers the grant admits are named, without starting
/// any, so the model knows there are tools behind `mcp.list()`; a server
/// the grant does not admit is not named, and no server means no line.
#[test]
fn admitted_mcp_servers_are_named_and_nothing_else() {
    let fixture = Fixture::new("mcp-names");
    std::fs::write(
        fixture.root.join(".mcp.json"),
        r#"{"mcpServers": {
            "alpha": {"command": "alpha-server"},
            "beta": {"command": "beta-server"}
        }}"#,
    )
    .unwrap();
    let facts =
        orientation::collect(&fixture.profile(r#"{"permissions":{"allow":["mcp__alpha__*"]}}"#));
    assert!(
        facts.contains("MCP servers (their tools and schemas come from `mcp.list()`): alpha\n")
            || facts
                .ends_with("MCP servers (their tools and schemas come from `mcp.list()`): alpha"),
        "{facts}"
    );
    assert!(!facts.contains("beta"), "{facts}");

    let none = orientation::collect(&fixture.profile(r#"{"permissions":{"allow":[]}}"#));
    assert!(!none.contains("MCP servers"), "{none}");
}
