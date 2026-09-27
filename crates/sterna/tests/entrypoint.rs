//! Actual-process coverage for Sterna's discoverable top-level entry point.

use std::path::{Path, PathBuf};
use std::process::Command;

struct Scratch(PathBuf);

impl Scratch {
    fn new(label: &str) -> Self {
        static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let path = std::env::temp_dir().join(format!(
            "sterna-entrypoint-{label}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::SeqCst)
        ));
        std::fs::create_dir_all(&path).unwrap();
        std::fs::create_dir_all(path.join(".glasshouse")).unwrap();
        std::fs::write(
            path.join(".glasshouse/pane.toml"),
            format!("[model]\nparent = {:?}\n", sterna::wire::MODEL),
        )
        .unwrap();
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }

    /// The session resolved its rollout here: the folder it names sessions
    /// in exists. (A session nobody asked anything in leaves no file in it.)
    fn has_session_folder(&self) -> bool {
        self.path().join(".sterna/sessions").is_dir()
    }

    fn has_session_rollout(&self) -> bool {
        std::fs::read_dir(self.path().join(".sterna/sessions")).is_ok_and(|entries| {
            entries.filter_map(Result::ok).any(|entry| {
                entry
                    .path()
                    .extension()
                    .is_some_and(|extension| extension == "jsonl")
                    && entry.path().is_file()
            })
        })
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn sterna() -> Command {
    static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let config_home = std::env::temp_dir().join(format!(
        "sterna-entrypoint-config-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, std::sync::atomic::Ordering::SeqCst)
    ));
    let mut command = Command::new(env!("CARGO_BIN_EXE_sterna"));
    command
        .env_remove("ANTHROPIC_API_KEY")
        .env_remove("ANTHROPIC_AUTH_TOKEN")
        // Every spawn gets its own empty global-config root, never the
        // developer's real `~/.config/sterna/config.toml`.
        .env("XDG_CONFIG_HOME", config_home)
        // **Set, so these tests stay about the entry point.** A session with
        // no base URL starts an `inference-gateway` and refuses when it
        // cannot (`gateway::start_or_attach`); a set URL is the attach half,
        // and needs no binary. Port 1 is never dialled -- no input here is a
        // turn -- and could only refuse locally if it were.
        .env("ANTHROPIC_BASE_URL", "http://127.0.0.1:1")
        // Lifecycle reporting degrades when Glasshouse is absent. Keeping it
        // absent makes these tests independent of the developer's install --
        // and proves a session needs no `glasshouse` on `PATH` at all.
        .env("PATH", "");
    command
}

#[test]
fn top_level_help_is_real_usage_on_stdout() {
    for flag in ["--help", "-h"] {
        let output = sterna().arg(flag).output().unwrap();
        assert!(output.status.success(), "{flag}: {:?}", output.status);
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(stdout.contains("Usage:"), "{flag}: {stdout}");
        assert!(stdout.contains("sterna session --root <path>"));
        assert!(stdout.contains("sterna ruler run"));
        assert!(stdout.contains("starts a session in the current project"));
        assert!(output.stderr.is_empty(), "{flag} wrote to stderr");
    }
}

#[test]
fn unknown_commands_and_options_fail_instead_of_echoing_stdin() {
    for argument in ["does-not-exist", "--does-not-exist"] {
        let output = sterna().arg(argument).output().unwrap();
        assert_eq!(output.status.code(), Some(2), "{argument}");
        assert!(output.stdout.is_empty(), "{argument} wrote to stdout");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stderr.contains("unknown"), "{argument}: {stderr}");
        assert!(stderr.contains(argument), "{argument}: {stderr}");
        assert!(stderr.contains("sterna --help"), "{argument}: {stderr}");
    }
}

#[test]
fn bare_sterna_without_a_model_refuses_instead_of_choosing_one() {
    let root = Scratch::new("unconfigured");
    std::fs::remove_file(root.path().join(".glasshouse/pane.toml")).unwrap();
    let output = sterna().current_dir(root.path()).output().unwrap();
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("no parent model selected"), "{stderr}");
    assert!(!root.has_session_rollout());
}

#[test]
fn bare_sterna_runs_the_ordinary_session_in_its_current_directory() {
    let root = Scratch::new("bare");
    let output = sterna().current_dir(root.path()).output().unwrap();
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        root.has_session_folder(),
        "bare sterna did not use the session's default rollout path"
    );
    assert!(
        !root.has_session_rollout(),
        "a session nobody asked anything in is not kept"
    );
}

#[test]
fn explicit_session_version_and_ruler_dispatch_remain_available() {
    let root = Scratch::new("session");
    let session = sterna()
        .args(["session", "--root"])
        .arg(root.path())
        .output()
        .unwrap();
    assert!(session.status.success());
    assert!(root.has_session_folder());

    let version = sterna().arg("--version").output().unwrap();
    assert!(version.status.success());
    assert_eq!(
        String::from_utf8_lossy(&version.stdout),
        format!("sterna {}\n", env!("CARGO_PKG_VERSION"))
    );

    let ruler = sterna().arg("ruler").output().unwrap();
    assert!(!ruler.status.success());
    assert!(String::from_utf8_lossy(&ruler.stderr).contains("usage: sterna ruler run [flags]"));
}
