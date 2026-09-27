use sterna::contract::SessionId;
// Gated like its only callers: every test that builds a runtime here is
// macOS/Linux, and on Windows an unused import is an error under
// `[workspace.lints.rust]`'s denied warnings.
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
#[cfg(any(target_os = "macos", target_os = "linux"))]
use sterna::runtime::isolate::Runtime;
use sterna::sandbox::profile::Profile;
use sterna::tools::invoke::{CancellationToken, ToolContext};
use sterna::verification::Verification;
static NEXT_FIXTURE: AtomicU64 = AtomicU64::new(0);

struct Fixture {
    root: PathBuf,
    profile: Profile,
}
impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "sterna-checks-{}-{}",
            std::process::id(),
            NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(root.join(".sterna")).unwrap();
        std::fs::write(root.join("input.txt"), "first\n").unwrap();
        std::fs::write(root.join(".sterna/checks.toml"), "checker = [\"tests\"]\n[checks.tests]\ncommand = \"/bin/cat input.txt\"\ninputs = [\"input.txt\"]\nreuse = true\n").unwrap();
        let profile = Profile::compile(
            &root,
            Some(
                r#"{"permissions":{"allow":["Read(**)","Bash(/bin/cat*)","Bash(/usr/bin/false)"]}}"#,
            ),
        );
        Self { root, profile }
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn named_checks_reuse_only_successful_unchanged_declared_inputs_and_force_is_fresh() {
    let f = Fixture::new();
    let session = SessionId::new("named-checks");
    let ctx = ToolContext {
        profile: &f.profile,
        session: &session,
    };
    let token = CancellationToken::new();
    let mut checks = Verification::default();
    let first = checks.run("tests", false, &ctx, &token).unwrap();
    assert!(first.executed && !first.reused);
    assert_eq!(first.stdout, "first\n");
    assert_eq!(first.exit_code, Some(0));
    let cached = checks.run("tests", false, &ctx, &token).unwrap();
    assert!(!cached.executed && cached.reused);
    assert_eq!(cached.observed_at_ms, first.observed_at_ms);
    std::fs::write(f.root.join("input.txt"), "second\n").unwrap();
    let changed = checks.run("tests", false, &ctx, &token).unwrap();
    assert!(changed.executed && !changed.reused);
    assert_eq!(changed.stdout, "second\n");
    assert!(checks.run("tests", true, &ctx, &token).unwrap().executed);
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn failed_checks_are_never_reused_and_configuration_is_not_a_permission_grant() {
    let f = Fixture::new();
    let session = SessionId::new("named-checks");
    let ctx = ToolContext {
        profile: &f.profile,
        session: &session,
    };
    let token = CancellationToken::new();
    let mut checks = Verification::default();
    std::fs::write(
        f.root.join(".sterna/checks.toml"),
        "[checks.tests]\ncommand = \"/usr/bin/false\"\ninputs = [\"input.txt\"]\nreuse = true\n",
    )
    .unwrap();
    for _ in 0..2 {
        let result = checks.run("tests", false, &ctx, &token).unwrap();
        assert_eq!(result.exit_code, Some(1));
        assert!(result.executed && !result.reused);
    }
    std::fs::write(
        f.root.join(".sterna/checks.toml"),
        "[checks.tests]\ncommand = \"touch forbidden\"\n",
    )
    .unwrap();
    assert!(checks.run("tests", false, &ctx, &token).is_err());
    assert!(!f.root.join("forbidden").exists());
}

#[test]
fn missing_inputs_and_oversized_config_never_establish_reusable_evidence() {
    let f = Fixture::new();
    std::fs::write(
        f.root.join(".sterna/checks.toml"),
        "[checks.tests]\ncommand = \"/bin/cat input.txt\"\nreuse = true\n",
    )
    .unwrap();
    assert!(
        sterna::verification::load(&f.profile)
            .unwrap_err()
            .contains("explicit inputs")
    );
    std::fs::write(f.root.join(".sterna/checks.toml"), "#".repeat(65537)).unwrap();
    assert!(
        sterna::verification::load(&f.profile)
            .unwrap_err()
            .contains("bounded read")
    );
}

#[test]
fn cancelled_checks_do_not_execute() {
    let f = Fixture::new();
    let session = SessionId::new("named-checks");
    let ctx = ToolContext {
        profile: &f.profile,
        session: &session,
    };
    let token = CancellationToken::new();
    token.cancel();
    assert!(
        Verification::default()
            .run("tests", false, &ctx, &token)
            .unwrap_err()
            .contains("cancelled")
    );
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn named_check_binding_persists_within_request_but_is_absent_from_helpers() {
    let f = Fixture::new();
    let session = SessionId::new("named-checks");
    let mut runtime = Runtime::new(&f.profile, &session);
    let first =
        runtime.run_cell("const observed = await checks.run('tests'); console.log(observed);");
    assert!(
        first.turn().stdout_tail.contains("\"executed\": true"),
        "{first:?}"
    );
    let second = runtime.run_cell("console.log(await checks.run('tests'));");
    assert!(
        second.turn().stdout_tail.contains("\"reused\": true"),
        "{second:?}"
    );
    runtime.end_task();
    let after_end = runtime.run_cell("console.log(await checks.run('tests')); ");
    assert!(
        after_end.turn().stdout_tail.contains("\"executed\": true"),
        "{after_end:?}"
    );
    drop(runtime);
    let mut helper = Runtime::for_helper(&f.profile, &session, &["read"]);
    let absent = helper.run_cell("console.log(typeof globalThis.checks);");
    assert_eq!(absent.turn().stdout_tail.trim(), "undefined");
    drop(helper);
    let mut fresh = Runtime::new(&f.profile, &session);
    assert!(
        fresh
            .run_cell("console.log(await checks.run('tests'));")
            .turn()
            .stdout_tail
            .contains("\"executed\": true")
    );
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn an_incomplete_large_dependency_snapshot_disables_reuse() {
    let f = Fixture::new();
    std::fs::create_dir(f.root.join("sources")).unwrap();
    std::fs::write(f.root.join("sources/huge.txt"), vec![b'x'; 1024 * 1024 + 1]).unwrap();
    std::fs::write(
        f.root.join(".sterna/checks.toml"),
        "[checks.tests]\ncommand = \"/bin/cat input.txt\"\ninputs = [\"sources\"]\nreuse = true\n",
    )
    .unwrap();
    let session = SessionId::new("bounded-checks");
    let ctx = ToolContext {
        profile: &f.profile,
        session: &session,
    };
    let mut checks = Verification::default();
    for _ in 0..2 {
        let result = checks
            .run("tests", false, &ctx, &CancellationToken::new())
            .unwrap();
        assert_eq!(result.exit_code, Some(0));
        assert!(result.executed && !result.reused);
        assert!(result.reuse_scope.contains("not reusable"));
    }
}
