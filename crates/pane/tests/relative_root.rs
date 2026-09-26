//! Two of the three tests here need a real sandbox and a real working
//! directory, so they are Unix-only — and so are the imports and the
//! `RestoreCwd` guard only they use, because `-D warnings` makes an unused
//! import an error on Windows. They are gated together, in one block, so the
//! helper cannot drift out of step with what uses it.
//!
//! The third, `missing_relative_root_is_anchored_without_falling_back_to_cwd`,
//! is deliberately NOT gated: it asserts how a relative root is anchored, which
//! is precisely the behaviour Windows spells differently, so it is the one test
//! in this file that the Windows cell most needs to run.

use pane::sandbox::profile::Profile;
use std::sync::Mutex;

#[cfg(any(target_os = "macos", target_os = "linux"))]
use {
    pane::contract::SessionId,
    pane::sandbox::profile::Access,
    pane::tools::invoke::{self, Args, ToolContext},
    std::path::{Path, PathBuf},
    std::sync::MutexGuard,
};

static CWD: Mutex<()> = Mutex::new(());

#[cfg(any(target_os = "macos", target_os = "linux"))]
struct RestoreCwd {
    original: PathBuf,
    _lock: MutexGuard<'static, ()>,
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
impl RestoreCwd {
    fn enter(path: &Path) -> Self {
        let lock = CWD.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let original = std::env::current_dir().unwrap();
        std::env::set_current_dir(path).unwrap();
        Self {
            original,
            _lock: lock,
        }
    }
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
impl Drop for RestoreCwd {
    fn drop(&mut self) {
        std::env::set_current_dir(&self.original).unwrap();
    }
}

#[test]
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn dot_root_is_anchored_and_real_tools_do_not_fail_with_enoent() {
    let root = std::env::temp_dir().join(format!("pane-relative-root-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("roman.py"), "value = 'XIV'\n").unwrap();
    let _cwd = RestoreCwd::enter(&root);

    let settings = r#"{"permissions":{"allow":["Read(**)","Bash"]}}"#;
    let profile = Profile::compile(".", Some(settings));
    assert_eq!(profile.root(), std::fs::canonicalize(&root).unwrap());
    assert!(profile.root().is_absolute());
    let session = SessionId::new("relative-root-regression");
    let context = ToolContext {
        profile: &profile,
        session: &session,
    };

    let read = invoke::run(&context, "read", &Args::new().with("path", "roman.py"))
        .expect("read under --root . must start /bin/cat and read the project");
    assert_eq!(read.stdout, "value = 'XIV'\n");

    let glob = invoke::run(&context, "glob", &Args::new().with("pattern", "**/*"))
        .expect("glob under --root . must traverse the project root");
    assert!(
        glob.stdout.lines().any(|path| Path::new(path)
            .file_name()
            .is_some_and(|name| name == "roman.py")),
        "{glob:?}"
    );

    let bash = invoke::run(&context, "bash", &Args::new().with("command", "pwd"))
        .expect("bash under --root . must have a valid confined current directory");
    assert_eq!(Path::new(bash.stdout.trim()), profile.root());
}

#[test]
fn missing_relative_root_is_anchored_without_falling_back_to_cwd() {
    let _lock = CWD.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let cwd = std::env::current_dir().unwrap();
    // The anchor is the *canonical* cwd, because `Profile::compile` resolves
    // the longest existing prefix of the root it was handed. On Windows that
    // is `\\?\D:\…`, on macOS `/var/…` becomes `/private/var/…`; asking for
    // the raw cwd asserted one host's spelling of the answer rather than the
    // anchoring this test is named for.
    let anchor = std::fs::canonicalize(&cwd).unwrap();
    let missing = format!("pane-no-such-root-{}", std::process::id());
    let profile = Profile::compile(&missing, None);
    assert_eq!(profile.root(), anchor.join(&missing));
    assert!(profile.root().is_absolute());
    // Both spellings of "it did not fall back to the working directory": the
    // one the caller holds, and the one the anchoring actually produced — on
    // Windows only the second can fail.
    assert_ne!(profile.root(), cwd);
    assert_ne!(profile.root(), anchor);
}

#[test]
#[cfg(unix)]
fn drive_relative_spelling_is_not_mistaken_for_a_windows_absolute_root() {
    let _lock = CWD.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let cwd = std::env::current_dir().unwrap();
    let profile = Profile::compile(
        "C:project",
        Some(r#"{"permissions":{"allow":["Read(**)","Bash","mcp__demo__*"]}}"#),
    );
    assert_ne!(profile.root(), cwd);
    assert!(
        profile
            .diagnostics()
            .iter()
            .any(|line| line.contains("drive-relative")),
        "{:?}",
        profile.diagnostics()
    );
    let refusal = profile
        .check("Read", Access::Read, Path::new("anything"))
        .expect_err("an ambiguous root must not receive the implicit root grant");
    assert!(refusal.rule.contains("drive-relative"), "{refusal:?}");
    assert!(profile.admits_command("pwd").is_err());
    assert!(!profile.admits_mcp_tool("mcp__demo__read"));
}
