use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use sterna::contract::SessionId;
use sterna::sandbox::profile::Profile;
use sterna::tools::invoke::{self, Args, ToolContext};

static COUNTER: AtomicU64 = AtomicU64::new(0);

struct Fixture {
    root: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "sterna-glob-dogfood-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        for relative in [
            "top.txt",
            "nested/inner.rs",
            "nested/deeper/last.md",
            ".sterna/root-secret.txt",
            "nested/.sterna/nested-secret.txt",
        ] {
            let path = root.join(relative);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, relative).unwrap();
        }
        let root = std::fs::canonicalize(root).unwrap();
        Self { root }
    }

    fn glob(&self, profile: &Profile, pattern: &str, path: Option<&Path>) -> Vec<PathBuf> {
        let session = SessionId::new("glob-dogfood");
        let mut args = Args::new().with("pattern", pattern);
        if let Some(path) = path {
            args = args.with("path", path.to_string_lossy());
        }
        invoke::run(
            &ToolContext {
                profile,
                session: &session,
            },
            "glob",
            &args,
        )
        .unwrap()
        .stdout
        .lines()
        .map(PathBuf::from)
        .collect()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

#[test]
fn glob_matches_project_relative_paths_and_honours_nested_roots() {
    let fixture = Fixture::new();
    let profile = Profile::compile(&fixture.root, None);

    let direct = fixture.glob(&profile, "*", None);
    assert!(direct.contains(&fixture.root.join("top.txt")), "{direct:?}");
    assert!(direct.contains(&fixture.root.join("nested")), "{direct:?}");
    assert!(
        !direct.contains(&fixture.root.join("nested/inner.rs")),
        "{direct:?}"
    );

    let recursive = fixture.glob(&profile, "**/*", None);
    assert!(
        recursive.contains(&fixture.root.join("nested/inner.rs")),
        "{recursive:?}"
    );
    assert!(
        recursive.contains(&fixture.root.join("nested/deeper/last.md")),
        "{recursive:?}"
    );

    let sterna_tree = fixture.glob(&profile, ".sterna/**", None);
    assert!(
        sterna_tree.contains(&fixture.root.join(".sterna")),
        "{sterna_tree:?}"
    );
    assert!(
        sterna_tree.contains(&fixture.root.join(".sterna/root-secret.txt")),
        "{sterna_tree:?}"
    );
    assert!(
        !sterna_tree.contains(&fixture.root.join("nested/.sterna")),
        "{sterna_tree:?}"
    );

    let nested_sterna_files = fixture.glob(&profile, "**/.sterna/*", None);
    assert!(
        nested_sterna_files.contains(&fixture.root.join("nested/.sterna/nested-secret.txt")),
        "{nested_sterna_files:?}"
    );

    let nested = fixture.glob(&profile, "*", Some(&fixture.root.join("nested")));
    assert!(
        nested.contains(&fixture.root.join("nested/inner.rs")),
        "{nested:?}"
    );
    assert!(
        nested.contains(&fixture.root.join("nested/deeper")),
        "{nested:?}"
    );
    assert!(
        !nested.contains(&fixture.root.join("top.txt")),
        "{nested:?}"
    );
}

#[test]
fn denied_dot_directories_are_neither_returned_nor_traversed() {
    let fixture = Fixture::new();
    let profile = Profile::compile(
        &fixture.root,
        Some(r#"{"permissions":{"deny":["Read(.sterna/**)","Read(**/.sterna/**)"]}}"#),
    );

    let recursive = fixture.glob(&profile, "**/*", None);
    assert!(
        recursive.contains(&fixture.root.join("top.txt")),
        "{recursive:?}"
    );
    assert!(
        recursive.contains(&fixture.root.join("nested/inner.rs")),
        "{recursive:?}"
    );
    assert!(
        recursive
            .iter()
            .all(|path| !path.components().any(|part| part.as_os_str() == ".sterna")),
        "a denied dot-directory leaked through glob: {recursive:?}"
    );
    assert!(fixture.glob(&profile, ".sterna/**", None).is_empty());
    assert!(fixture.glob(&profile, "**/.sterna/*", None).is_empty());
}

#[test]
fn question_mark_matches_one_unicode_character() {
    let fixture = Fixture::new();
    std::fs::write(fixture.root.join("é.txt"), "unicode").unwrap();
    let profile = Profile::compile(&fixture.root, None);
    assert_eq!(
        fixture.glob(&profile, "?.txt", None),
        vec![fixture.root.join("é.txt")]
    );
}
