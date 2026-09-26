//! Acceptance for `smarter-cheaper-roadmap.md`, *Tool outcome semantics*:
//! provider-native parallel calls are independent, so a frame lowered from
//! several of them isolates each call, and only a successful call leaves a
//! handle.
//!
//! Gated like `tests/abi.rs`: every test builds a runtime and runs a
//! capability, which on Windows would be refused rather than run unconfined.
#![cfg(any(target_os = "macos", target_os = "linux"))]

use pane::abi::{Dialect, lower};
use pane::contract::SessionId;
use pane::runtime::isolate::Runtime;
use pane::runtime::outcome::{CellOutcome, Ended};
use pane::sandbox::profile::Profile;
use serde_json::json;

/// A fixture tree with one source file, and a path outside it that every
/// profile refuses.
struct Fixture {
    root: std::path::PathBuf,
    outside: std::path::PathBuf,
}

impl Fixture {
    fn new(test: &str) -> Self {
        let stem = format!("pane-direct-frame-{test}-{}", std::process::id());
        let root = std::env::temp_dir().join(&stem);
        let outside = std::env::temp_dir().join(format!("{stem}-outside"));
        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_dir_all(&outside);
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(root.join("target.rs"), "fn marker() {}\n").unwrap();
        std::fs::write(outside.join("secret.txt"), "OUTSIDE\n").unwrap();
        Self { root, outside }
    }

    fn runtime(&self, session: &str, permissions: &str) -> Runtime {
        let profile = Profile::compile(&self.root, Some(permissions));
        Runtime::new(&profile, &SessionId::new(session))
    }

    fn target(&self) -> String {
        self.root.join("target.rs").to_string_lossy().into_owned()
    }

    fn secret(&self) -> String {
        self.outside
            .join("secret.txt")
            .to_string_lossy()
            .into_owned()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
        let _ = std::fs::remove_dir_all(&self.outside);
    }
}

const READ_ONLY: &str = r#"{"permissions":{"allow":[]}}"#;

fn read(id: &str, path: &str) -> (String, String, serde_json::Value) {
    (id.into(), "Read".into(), json!({"file_path": path}))
}

fn glob(id: &str) -> (String, String, serde_json::Value) {
    (id.into(), "Glob".into(), json!({"pattern": "*.rs"}))
}

/// The invariant the session relies on, stated as a test: `calls[i]` is the
/// i-th provider call, every call is recorded, and `capability_results`
/// carries one entry per call that succeeded, in order.
#[test]
fn a_denied_first_call_does_not_stop_the_second_and_leaves_no_handle() {
    let fixture = Fixture::new("first-denied");
    let mut runtime = fixture.runtime("first-denied", READ_ONLY);
    let calls = vec![read("a", &fixture.secret()), glob("b")];
    let lowered = lower(Dialect::Anthropic, &calls, runtime.next_cell()).unwrap();
    assert_eq!(lowered.calls[0].binding, "read_1_1");
    assert_eq!(lowered.calls[1].binding, "glob_1_2");

    let outcome = runtime.run_direct_frame(&lowered.source);
    assert!(
        matches!(outcome, CellOutcome::Yielded { .. }),
        "{outcome:?}"
    );
    let turn = outcome.turn();
    assert_eq!(turn.record.calls.len(), 2, "{outcome:?}");
    assert!(
        matches!(turn.record.calls[0].ended, Ended::Denied { .. }),
        "{:?}",
        turn.record.calls[0]
    );
    assert!(
        turn.record.calls[0]
            .error
            .as_deref()
            .is_some_and(|error| error.contains("PermissionDenied")),
        "{:?}",
        turn.record.calls[0]
    );
    assert_eq!(turn.record.calls[1].ended, Ended::Ok);
    assert!(turn.record.calls[1].error.is_none());
    assert_eq!(
        turn.capability_results.len(),
        1,
        "{:?}",
        turn.capability_results
    );
    assert!(turn.capability_results[0].contains("target.rs"));

    // The failed binding is not a live handle anywhere it could be seen.
    assert!(!runtime.is_live("read_1_1"));
    assert!(runtime.is_live("glob_1_2"));
    let names: Vec<&str> = turn
        .record
        .handles
        .iter()
        .map(|handle| handle.name.as_str())
        .collect();
    assert_eq!(names, vec!["glob_1_2"], "{:?}", turn.record.handles);
    assert!(!turn.table.contains("read_1_1"), "{}", turn.table);
    assert!(!runtime.render_handles().contains("read_1_1"));
    assert!(!runtime.handle_names().contains(&"read_1_1".to_string()));
}

/// The mirror image: a denial in the second call leaves the first call's
/// handle live, and a later authored cell can compute over it.
#[test]
fn a_denied_second_call_leaves_the_first_calls_handle_live() {
    let fixture = Fixture::new("second-denied");
    let mut runtime = fixture.runtime("second-denied", READ_ONLY);
    let calls = vec![read("a", &fixture.target()), read("b", &fixture.secret())];
    let lowered = lower(Dialect::Anthropic, &calls, runtime.next_cell()).unwrap();

    let outcome = runtime.run_direct_frame(&lowered.source);
    assert!(
        matches!(outcome, CellOutcome::Yielded { .. }),
        "{outcome:?}"
    );
    let turn = outcome.turn();
    assert_eq!(turn.record.calls.len(), 2);
    assert_eq!(turn.record.calls[0].ended, Ended::Ok);
    assert!(matches!(turn.record.calls[1].ended, Ended::Denied { .. }));
    assert_eq!(turn.capability_results.len(), 1);
    assert!(runtime.is_live("read_1_1"));
    assert!(!runtime.is_live("read_1_2"));

    let later =
        runtime.run_cell("return read_1_1.text.includes(\"marker\") ? \"held\" : \"lost\";");
    match later {
        CellOutcome::Returned {
            terminal: pane::runtime::outcome::Terminal::Text(text),
            ..
        } => assert_eq!(text, "held"),
        other => panic!("{other:?}"),
    }
}

/// One call is not isolated: its failure throws, so the session answers the
/// one `tool_result` as an error, exactly as before.
#[test]
fn a_single_failing_call_still_throws() {
    let fixture = Fixture::new("single");
    let mut runtime = fixture.runtime("single", READ_ONLY);
    let calls = vec![read("a", &fixture.secret())];
    let lowered = lower(Dialect::Anthropic, &calls, runtime.next_cell()).unwrap();
    assert!(lowered.source.starts_with("const read_1_1 = await read("));

    let outcome = runtime.run_direct_frame(&lowered.source);
    assert!(matches!(outcome, CellOutcome::Threw { .. }), "{outcome:?}");
    let turn = outcome.turn();
    assert_eq!(turn.record.calls.len(), 1);
    assert!(matches!(turn.record.calls[0].ended, Ended::Denied { .. }));
    assert!(turn.capability_results.is_empty());
    assert!(!runtime.is_live("read_1_1"));
}

/// A call that ran and failed inside a frame records the message it threw,
/// so the provider result can carry it although the frame did not throw.
#[test]
fn a_thrown_call_in_a_frame_records_its_message_and_the_frame_continues() {
    let fixture = Fixture::new("threw");
    let mut runtime = fixture.runtime("threw", READ_ONLY);
    // Reading a directory is a call that runs and fails, not a denial.
    let calls = vec![read("a", &fixture.root.to_string_lossy()), glob("b")];
    let lowered = lower(Dialect::Anthropic, &calls, runtime.next_cell()).unwrap();

    let outcome = runtime.run_direct_frame(&lowered.source);
    assert!(
        matches!(outcome, CellOutcome::Yielded { .. }),
        "{outcome:?}"
    );
    let turn = outcome.turn();
    assert_eq!(turn.record.calls.len(), 2, "{outcome:?}");
    assert_eq!(
        turn.record.calls[0].ended,
        Ended::Threw {
            class: "ToolError".into()
        }
    );
    assert!(
        turn.record.calls[0]
            .error
            .as_deref()
            .is_some_and(|error| error.contains("`read` failed")),
        "{:?}",
        turn.record.calls[0]
    );
    assert_eq!(turn.record.calls[1].ended, Ended::Ok);
    assert_eq!(turn.capability_results.len(), 1);
    assert!(!runtime.is_live("read_1_1"));
    assert!(runtime.is_live("glob_1_2"));
}

/// A process result records the child's exit status; an in-process call
/// records none, because its `0` is a convention rather than an observation.
#[test]
fn a_process_result_records_its_exit_code_and_an_in_process_call_does_not() {
    let fixture = Fixture::new("exit-code");
    let mut runtime = fixture.runtime("exit-code", r#"{"permissions":{"allow":["Bash(exit*)"]}}"#);
    let outcome = runtime.run_cell(&format!(
        "await bash({{command:\"exit 7\"}});\n\
         await glob({{pattern:\"*.rs\"}});\n\
         await read({{path:{:?}}});",
        fixture.target()
    ));
    let calls = &outcome.turn().record.calls;
    assert_eq!(calls.len(), 3, "{outcome:?}");
    assert_eq!(calls[0].tool, "bash");
    assert_eq!(calls[0].exit_code, Some(7));
    assert_eq!(calls[0].ended, Ended::Ok);
    assert_eq!(calls[1].tool, "glob");
    assert_eq!(calls[1].exit_code, None);
    assert_eq!(calls[2].tool, "read");
    assert_eq!(calls[2].exit_code, Some(0));
    let json = serde_json::to_string(&calls[0]).unwrap();
    assert!(json.contains(r#""exit_code":7"#), "{json}");
}
