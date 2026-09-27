//! Acceptance for the container profile and the capability manifest —
//! `smarter-cheaper-roadmap.md`, *Benchmark container profile* and
//! *Capability/environment manifest*.
//!
//! Container mode is the explicit `--dangerously-bypass-os-sandbox`
//! acknowledgement read at the policy level: reads span the container, a
//! debugger is admissible, and everything else — writes, the sandbox
//! launchers, the never-grantable set, every `deny` pattern — holds exactly
//! as it does outside it. Nothing here spawns a process.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use sterna::manifest::{CommandPolicy, Manifest};
use sterna::sandbox::profile::{Access, PermissionDenied, Profile};

static COUNTER: AtomicU64 = AtomicU64::new(0);

/// A throwaway tree: `project/` is the root, `build/` its sibling, and
/// `build/secrets/` a subtree a `deny` can name. Removed when dropped.
struct Fixture {
    base: PathBuf,
}

impl Fixture {
    fn new(label: &str) -> Self {
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let base = scratch_base().join(format!(
            "sterna-container-profile-{}-{label}-{n}",
            std::process::id()
        ));
        std::fs::create_dir_all(base.join("project")).unwrap();
        std::fs::create_dir_all(base.join("build/secrets")).unwrap();
        std::fs::write(base.join("build/x.c"), "int main(void) { return 0; }\n").unwrap();
        std::fs::write(base.join("build/secrets/token"), "not-a-secret\n").unwrap();
        Self {
            base: std::fs::canonicalize(base).unwrap(),
        }
    }

    fn root(&self) -> PathBuf {
        self.base.join("project")
    }

    fn sibling(&self, tail: &str) -> PathBuf {
        self.base.join("build").join(tail)
    }

    /// The sibling as a pattern writes it: forward slashes, no escaping.
    fn sibling_pattern(&self) -> String {
        self.base.join("build").to_string_lossy().replace('\\', "/")
    }

    fn profile(&self, settings: &str) -> Profile {
        Profile::compile(self.root(), Some(settings))
    }

    fn container(&self, settings: &str) -> Profile {
        self.profile(settings).with_os_sandbox_bypass()
    }
}

/// Where a fixture tree goes: somewhere §4.3's `$HOME` rule does not cover, so
/// each test's refusal is decided by the rule it names. On Windows
/// `temp_dir()` is `%USERPROFILE%\AppData\Local\Temp`, under `$HOME`, so the
/// first candidate there that is outside the profile and creatable wins; on
/// Unix `temp_dir()` is already outside it.
fn scratch_base() -> PathBuf {
    if cfg!(windows) {
        let home: Vec<PathBuf> = ["HOME", "USERPROFILE"]
            .iter()
            .filter_map(std::env::var_os)
            .filter(|value| !value.is_empty())
            .filter_map(|value| std::fs::canonicalize(value).ok())
            .collect();
        let system_drive = std::env::var("SystemDrive").unwrap_or_else(|_| "C:".to_string());
        let candidates = [
            PathBuf::from(env!("CARGO_TARGET_TMPDIR")),
            PathBuf::from(format!("{system_drive}\\sterna-container-profile-tmp")),
        ];
        for candidate in candidates {
            if std::fs::create_dir_all(&candidate).is_err() {
                continue;
            }
            let Ok(resolved) = std::fs::canonicalize(&candidate) else {
                continue;
            };
            if !home.iter().any(|home| resolved.starts_with(home)) {
                return resolved;
            }
        }
    }
    std::env::temp_dir()
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.base);
    }
}

const BARE_BASH: &str = r#"{"permissions":{"allow":["Bash"]}}"#;

fn refusal<T: std::fmt::Debug>(result: Result<T, PermissionDenied>) -> PermissionDenied {
    result.expect_err("expected a refusal, got a grant")
}

#[cfg(unix)]
fn home() -> PathBuf {
    let value = std::env::var_os("HOME").expect("HOME is set on a Unix host");
    assert!(!value.is_empty(), "HOME is empty");
    PathBuf::from(value)
}

#[test]
fn container_mode_is_the_bypass_fact_and_nothing_else_sets_it() {
    let fixture = Fixture::new("mode");
    assert!(!fixture.profile(BARE_BASH).container_mode());
    assert!(fixture.container(BARE_BASH).container_mode());
    // The yolo-shaped document alone does not turn it on: it is a host
    // construction step, not a pattern.
    assert!(
        !fixture
            .profile(r#"{"permissions":{"allow":["Bash","Read(/**)","Write(/**)"]}}"#)
            .container_mode()
    );
}

#[test]
fn a_debugger_is_admitted_only_in_container_mode_and_a_launcher_never() {
    let fixture = Fixture::new("debugger");
    let plain = fixture.profile(BARE_BASH);
    let container = fixture.container(BARE_BASH);

    let denied = refusal(plain.admits_command("gdb ./a.out"));
    assert!(
        denied.rule.contains("never grantable 5"),
        "{:?}",
        denied.rule
    );
    assert!(denied.rule.contains("`gdb`"), "{:?}", denied.rule);
    container
        .admits_command("gdb ./a.out")
        .expect("container mode admits a debugger: the container is the boundary");
    container
        .admits_command("strace -f ./a.out")
        .expect("every debugger name is admitted in container mode");

    for launcher in [
        "bwrap --dev-bind / / /bin/sh",
        "sandbox-exec -p '(version 1)(allow default)' /bin/sh",
        "sh -c \"bubblewrap --ro-bind / / /bin/sh\"",
    ] {
        for (label, profile) in [("plain", &plain), ("container", &container)] {
            let denied = refusal(profile.admits_command(launcher));
            assert!(
                denied.rule.contains("never grantable 5"),
                "{label}: {launcher:?} was refused for the wrong reason: {:?}",
                denied.rule
            );
        }
    }

    // Outside container mode the refusal is the one it always was, byte for
    // byte, so the existing pins keep holding.
    assert_eq!(
        refusal(plain.admits_command("sudo gdb --pid 4242")).rule,
        "`gdb` re-enters the sandbox launcher or attaches a debugger and is never grantable by any pattern (docs/sandbox.md, never grantable 5)"
    );
}

#[test]
fn the_never_grantable_table_drops_the_debuggers_in_container_mode_only() {
    let fixture = Fixture::new("table");
    let plain = fixture.profile(BARE_BASH).never_grantable_commands();
    let container = fixture.container(BARE_BASH).never_grantable_commands();
    assert_eq!(
        plain,
        vec![
            "sandbox-exec",
            "bwrap",
            "bubblewrap",
            "lldb",
            "gdb",
            "strace",
            "ltrace",
            "dtrace",
            "dtruss",
            "windbg",
            "x64dbg",
            "vsjitdebugger.exe",
        ]
    );
    assert_eq!(container, vec!["sandbox-exec", "bwrap", "bubblewrap"]);
}

#[test]
fn a_sibling_of_the_root_is_readable_in_container_mode_and_never_writable() {
    let fixture = Fixture::new("sibling");
    let plain = fixture.profile(BARE_BASH);
    let container = fixture.container(BARE_BASH);
    let source = fixture.sibling("x.c");

    let denied = refusal(plain.check("Read", Access::Read, &source));
    assert_eq!(
        denied.rule,
        "no grant covers this path; the project root is the only readable root"
    );
    let granted = container
        .check("Read", Access::Read, &source)
        .expect("container mode reads span the container");
    assert_eq!(granted, source);

    let target = fixture.sibling("out.o");
    refusal(plain.check("Write", Access::Write, &target));
    let denied = refusal(container.check("Write", Access::Write, &target));
    assert!(
        denied.rule.contains("only writable roots") && denied.rule.contains("container mode"),
        "the container-mode write refusal names the asymmetry: {:?}",
        denied.rule
    );
    // And the root itself is still granted both ways in both modes.
    for profile in [&plain, &container] {
        profile
            .check("Read", Access::Read, &fixture.root().join("src/main.rs"))
            .unwrap();
        profile
            .check("Write", Access::Write, &fixture.root().join("src/main.rs"))
            .unwrap();
    }
}

#[test]
#[cfg(unix)]
fn a_credential_store_stays_refused_for_read_in_container_mode() {
    let fixture = Fixture::new("home");
    let container = fixture.container(BARE_BASH);
    let key = home().join(".ssh/id_ed25519");
    let denied = refusal(container.check("Read", Access::Read, &key));
    assert!(
        denied.rule.contains("`~/.ssh`") && denied.rule.contains("never grantable 3"),
        "the refusal names the never-grantable rule that decided it: {:?}",
        denied.rule
    );
    // The whole of `$HOME` outside the project, not only the five names.
    let dotfile = home().join("scratch-that-is-not-the-project.txt");
    let denied = refusal(container.check("Read", Access::Read, &dotfile));
    assert!(
        denied.rule.contains("never grantable 3"),
        "{:?}",
        denied.rule
    );
}

#[test]
fn a_configured_deny_pattern_refuses_a_container_read() {
    let fixture = Fixture::new("deny");
    let settings = format!(
        r#"{{"permissions":{{"allow":["Bash"],"deny":["Read({}/secrets/**)"]}}}}"#,
        fixture.sibling_pattern()
    );
    let container = fixture.container(&settings);
    let denied = refusal(container.check("Read", Access::Read, &fixture.sibling("secrets/token")));
    assert!(
        denied.rule.contains("permissions.deny") && denied.rule.contains("secrets/**"),
        "the deny entry is quoted back: {:?}",
        denied.rule
    );
    // The sibling outside the denied subtree is still readable.
    container
        .check("Read", Access::Read, &fixture.sibling("x.c"))
        .unwrap();
}

#[test]
fn the_dot_sterna_write_refusal_names_where_scratch_files_go() {
    let fixture = Fixture::new("scratch");
    let profile = fixture.profile(BARE_BASH);
    let denied = refusal(profile.check(
        "Write",
        Access::Write,
        &fixture.root().join(".sterna/scratch.txt"),
    ));
    assert!(
        denied
            .rule
            .ends_with("; write scratch files elsewhere under the project root"),
        "{:?}",
        denied.rule
    );
    assert!(denied.rule.contains("`.sterna/**`"), "{:?}", denied.rule);
    // `.claude/**` keeps its own sentence: it is a widening hazard, not a
    // scratch directory.
    let denied = refusal(profile.check(
        "Write",
        Access::Write,
        &fixture.root().join(".claude/settings.json"),
    ));
    assert!(denied.rule.contains("invariant 5"), "{:?}", denied.rule);
    assert!(!denied.rule.contains("scratch"), "{:?}", denied.rule);
}

#[test]
#[cfg(not(target_os = "windows"))]
fn the_dot_sterna_refusal_under_an_additional_root_names_the_affordance_too() {
    let fixture = Fixture::new("scratch-extra");
    let profile = fixture
        .profile(BARE_BASH)
        .with_additional_root("../build")
        .unwrap();
    let denied = refusal(profile.check("Write", Access::Write, &fixture.sibling(".sterna/x")));
    assert!(
        denied
            .rule
            .ends_with("; write scratch files elsewhere under the project root"),
        "{:?}",
        denied.rule
    );
    let denied = refusal(profile.check("Write", Access::Write, &fixture.sibling(".claude/x")));
    assert!(!denied.rule.contains("scratch"), "{:?}", denied.rule);
}

const ABSENT: &str = "sterna-container-profile-no-such-executable";

#[test]
fn the_manifest_reports_the_mode_the_table_and_an_absent_executable() {
    let fixture = Fixture::new("manifest");
    let settings = format!(
        r#"{{"permissions":{{"allow":["Bash(cargo test*)"],"deny":["Read({}/secrets/**)"]}}}}"#,
        fixture.sibling_pattern()
    );
    let root = fixture.root().display().to_string();

    let plain = Manifest::collect(&fixture.profile(&settings), &[ABSENT]);
    assert!(!plain.container_mode);
    assert_eq!(plain.root, root);
    assert_eq!(plain.readable_roots, vec![root.clone()]);
    assert_eq!(plain.writable_roots, vec![root.clone()]);
    assert_eq!(
        plain.reserved_paths,
        vec![
            fixture.root().join(".sterna").display().to_string(),
            fixture.root().join(".claude").display().to_string(),
        ]
    );
    assert_eq!(
        plain.denied_patterns,
        vec![format!("Read({}/secrets/**)", fixture.sibling_pattern())]
    );
    assert_eq!(
        plain.commands,
        CommandPolicy::Patterns(vec!["Bash(cargo test*)".to_string()])
    );
    assert_eq!(plain.never_grantable_commands.len(), 12);
    assert!(plain.never_grantable_commands.contains(&"gdb".to_string()));
    assert_eq!(plain.executables.len(), 1);
    assert_eq!(plain.executables[0].name, ABSENT);
    assert_eq!(plain.executables[0].path, None);
    assert!(!plain.network);
    assert!(plain.unavailable.is_empty());

    let container = Manifest::collect(&fixture.container(&settings), &[ABSENT]);
    assert!(container.container_mode);
    assert_eq!(
        container.readable_roots,
        vec![
            root.clone(),
            "/ (container mode: everything except credential stores and denied patterns)"
                .to_string()
        ]
    );
    assert_eq!(container.writable_roots, vec![root]);
    assert_eq!(
        container.never_grantable_commands,
        vec!["sandbox-exec", "bwrap", "bubblewrap"]
    );
    assert!(container.render().contains("Container mode"));
    assert!(!plain.render().contains("Container mode"));
}

#[test]
fn the_manifest_states_the_command_policy_the_profile_holds() {
    let fixture = Fixture::new("commands");
    assert_eq!(
        Manifest::collect(&fixture.profile(BARE_BASH), &[]).commands,
        CommandPolicy::All
    );
    assert_eq!(
        Manifest::collect(&fixture.profile(r#"{"permissions":{}}"#), &[]).commands,
        CommandPolicy::None
    );
}

#[test]
fn the_manifest_resolves_a_present_executable_to_a_path() {
    let fixture = Fixture::new("present");
    let name = if cfg!(windows) { "cmd.exe" } else { "sh" };
    let manifest = Manifest::collect(&fixture.profile(BARE_BASH), &[name, ABSENT]);
    assert_eq!(manifest.executables.len(), 2);
    let present = &manifest.executables[0];
    assert_eq!(present.name, name);
    let path = present
        .path
        .as_deref()
        .unwrap_or_else(|| panic!("`{name}` is on PATH on every host: {manifest:?}"));
    assert!(Path::new(path).is_absolute(), "{path}");
    assert_eq!(manifest.executables[1].path, None);
}

#[test]
#[cfg(not(target_os = "windows"))]
fn the_manifest_names_both_roots_and_their_reserved_paths() {
    let fixture = Fixture::new("roots");
    let profile = fixture
        .container(BARE_BASH)
        .with_additional_root("../build")
        .unwrap();
    let manifest = Manifest::collect(&profile, &[]);
    let root = fixture.root().display().to_string();
    let extra = fixture.base.join("build").display().to_string();
    assert_eq!(
        manifest.readable_roots,
        vec![
            root.clone(),
            extra.clone(),
            "/ (container mode: everything except credential stores and denied patterns)"
                .to_string()
        ]
    );
    assert_eq!(manifest.writable_roots, vec![root.clone(), extra.clone()]);
    assert_eq!(
        manifest.reserved_paths,
        vec![
            format!("{root}/.sterna"),
            format!("{root}/.claude"),
            format!("{extra}/.sterna"),
            format!("{extra}/.claude"),
        ]
    );
    let rendered = manifest.render();
    assert!(
        rendered.contains(&format!("Readable roots: {root}, {extra}, /")),
        "{rendered}"
    );
    assert!(
        rendered.contains(&format!("Writable roots: {root}, {extra}")),
        "{rendered}"
    );
}
