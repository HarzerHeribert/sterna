//! The failure taxonomy, request causes, single-intent heuristic and shell
//! families, each rule pinned on a constructed record — no provider, no
//! isolate.
use std::collections::BTreeMap;

use sterna::abi::lift::Family;
use sterna::abi::telemetry::{
    FailureKind, RequestCause, classify_call, classify_cell_error, is_single_intent_cell,
    request_cause, shell_family,
};
use sterna::runtime::outcome::{CallRecord, CellOutcomeKind, CellRecord, Ended};

fn call(tool: &str, ended: Ended) -> CallRecord {
    CallRecord {
        tool: tool.into(),
        args: BTreeMap::new(),
        evidence: None,
        lifted_from: None,
        exit_code: None,
        repeat_of: None,
        error: None,
        ended,
    }
}

fn bash(command: &str, ended: Ended) -> CallRecord {
    let mut record = call("bash", ended);
    record.args.insert("command".into(), command.into());
    record
}

fn threw(class: &str) -> Ended {
    Ended::Threw {
        class: class.into(),
    }
}

fn frame(calls: Vec<CallRecord>) -> CellRecord {
    CellRecord {
        cell: 1,
        description: None,
        source: String::new(),
        outcome: CellOutcomeKind::Yielded,
        handles: Vec::new(),
        calls,
    }
}

// --- classify_call ---------------------------------------------------------

#[test]
fn an_ok_call_has_no_failure_kind() {
    assert_eq!(classify_call(&call("read", Ended::Ok)), None);
}

#[test]
fn a_denied_call_is_a_denial() {
    let denied = call(
        "bash",
        Ended::Denied {
            rule: "no allow".into(),
        },
    );
    assert_eq!(classify_call(&denied), Some(FailureKind::Denial));
}

#[test]
fn a_cancelled_call_is_infrastructure() {
    assert_eq!(
        classify_call(&call("read", threw("Cancelled"))),
        Some(FailureKind::Infrastructure)
    );
}

#[test]
fn a_bash_tool_error_without_an_exit_code_is_a_process_signal() {
    let killed = bash("cargo test", threw("ToolError"));
    assert_eq!(killed.exit_code, None);
    assert_eq!(classify_call(&killed), Some(FailureKind::ProcessSignal));
}

#[test]
fn a_bash_tool_error_with_an_exit_code_is_a_command_failure() {
    let mut failed = bash("cargo test", threw("ToolError"));
    failed.exit_code = Some(101);
    assert_eq!(classify_call(&failed), Some(FailureKind::Command));
}

#[test]
fn an_edit_tool_error_is_a_mutation_conflict() {
    assert_eq!(
        classify_call(&call("edit", threw("ToolError"))),
        Some(FailureKind::MutationConflict)
    );
}

#[test]
fn any_other_tool_error_is_a_command_failure() {
    for tool in ["read", "grep", "checks.run", "helper.scout", "agent.run"] {
        assert_eq!(
            classify_call(&call(tool, threw("ToolError"))),
            Some(FailureKind::Command),
            "{tool}"
        );
    }
}

#[test]
fn an_unfamiliar_thrown_class_falls_back_to_the_cell_error_rules() {
    assert_eq!(
        classify_call(&call("read", threw("PermissionDenied"))),
        Some(FailureKind::Denial)
    );
    assert_eq!(
        classify_call(&call("read", threw("SomethingElse"))),
        Some(FailureKind::Runtime)
    );
}

// --- classify_cell_error ---------------------------------------------------

#[test]
fn the_syntax_classes_are_syntax() {
    for class in [
        "SyntaxError",
        "TypeScriptNotErasable",
        "ReferenceError",
        "ReservedName",
        "ShadowsHostFunction",
        "ProtocolError",
        "CellEditError",
        "UndefinedName",
    ] {
        assert_eq!(
            classify_cell_error(class, "anything"),
            FailureKind::Syntax,
            "{class}"
        );
    }
}

#[test]
fn a_message_naming_a_source_race_is_a_mutation_conflict() {
    for message in [
        "The source version changed; refresh context before editing.",
        "edit failed: stale_hash",
        "edit failed: ambiguous_match",
        "edit failed: missing_match",
    ] {
        assert_eq!(
            classify_cell_error("ToolError", message),
            FailureKind::MutationConflict,
            "{message}"
        );
    }
}

#[test]
fn termination_memory_and_deadline_messages_are_timeouts() {
    assert_eq!(
        classify_cell_error("RuntimeTerminated", ""),
        FailureKind::Timeout
    );
    assert_eq!(
        classify_cell_error("RuntimeOutOfMemory", ""),
        FailureKind::Timeout
    );
    assert_eq!(
        classify_cell_error("Error", "the wall clock ended the cell"),
        FailureKind::Timeout
    );
    assert_eq!(
        classify_cell_error("Error", "deadline exceeded"),
        FailureKind::Timeout
    );
}

#[test]
fn a_signal_message_is_a_process_signal() {
    assert_eq!(
        classify_cell_error(
            "ToolError",
            "`cargo` was killed by a signal and never exited"
        ),
        FailureKind::ProcessSignal
    );
}

#[test]
fn denial_cancellation_and_the_rest() {
    assert_eq!(
        classify_cell_error("PermissionDenied", "rule"),
        FailureKind::Denial
    );
    assert_eq!(
        classify_cell_error("Cancelled", ""),
        FailureKind::Infrastructure
    );
    assert_eq!(
        classify_cell_error("TypeError", "x is not a function"),
        FailureKind::Runtime
    );
    assert_eq!(
        classify_cell_error("ToolError", "exit 1"),
        FailureKind::Runtime
    );
}

// --- request_cause ---------------------------------------------------------

#[test]
fn the_first_request_is_implementation() {
    assert_eq!(request_cause(None, false), RequestCause::Implementation);
    assert_eq!(request_cause(None, true), RequestCause::Implementation);
}

#[test]
fn a_request_after_a_failed_frame_is_repair() {
    let previous = frame(vec![call("read", Ended::Ok)]);
    assert_eq!(request_cause(Some(&previous), true), RequestCause::Repair);
}

#[test]
fn a_request_after_a_read_only_frame_is_exploration() {
    let previous = frame(
        ["read", "grep", "rg", "glob", "fd", "jq", "context"]
            .into_iter()
            .map(|tool| call(tool, Ended::Ok))
            .collect(),
    );
    assert_eq!(
        request_cause(Some(&previous), false),
        RequestCause::Exploration
    );
    let observing_shell = frame(vec![
        bash("git status", Ended::Ok),
        bash("ls src", Ended::Ok),
    ]);
    assert_eq!(
        request_cause(Some(&observing_shell), false),
        RequestCause::Exploration
    );
}

#[test]
fn a_request_after_checks_or_a_verification_command_is_verification() {
    let checks = frame(vec![call("read", Ended::Ok), call("checks.run", Ended::Ok)]);
    assert_eq!(
        request_cause(Some(&checks), false),
        RequestCause::Verification
    );
    let cargo = frame(vec![bash("cargo test -p sterna", Ended::Ok)]);
    assert_eq!(
        request_cause(Some(&cargo), false),
        RequestCause::Verification
    );
}

#[test]
fn a_request_after_a_frame_that_changed_things_is_implementation() {
    let edit = frame(vec![call("read", Ended::Ok), call("edit", Ended::Ok)]);
    assert_eq!(
        request_cause(Some(&edit), false),
        RequestCause::Implementation
    );
    let arbitrary_shell = frame(vec![bash("make release", Ended::Ok)]);
    assert_eq!(
        request_cause(Some(&arbitrary_shell), false),
        RequestCause::Implementation
    );
    let no_calls = frame(Vec::new());
    assert_eq!(
        request_cause(Some(&no_calls), false),
        RequestCause::Implementation
    );
}

// --- is_single_intent_cell -------------------------------------------------

#[test]
fn one_awaited_call_is_single_intent() {
    let one = vec![call("read", Ended::Ok)];
    assert!(is_single_intent_cell(
        "const f = await read({ path: 'src/lib.rs' });",
        &one
    ));
    assert!(is_single_intent_cell(
        "// look at the entry point\n\nreturn await read({ path: 'src/lib.rs' })\n",
        &one
    ));
    assert!(is_single_intent_cell(
        "/* the file\n   we need */ await grep({ pattern: 'fn main', path: 'src' });",
        &one
    ));
    assert!(is_single_intent_cell(
        "await bash({ command: 'echo http://example.org' });",
        &one
    ));
}

#[test]
fn a_program_is_not_single_intent() {
    let one = vec![call("read", Ended::Ok)];
    let two = vec![call("read", Ended::Ok), call("grep", Ended::Ok)];
    // Two calls ran.
    assert!(!is_single_intent_cell("await read({ path: 'a' });", &two));
    // Two statements.
    assert!(!is_single_intent_cell(
        "const f = await read({ path: 'a' }); console.log(f.text);",
        &one
    ));
    // Two awaits.
    assert!(!is_single_intent_cell(
        "await read({ path: 'a' }) && await read({ path: 'b' })",
        &one
    ));
    // Control flow and closures.
    for source in [
        "if (x) await read({ path: 'a' })",
        "for (const p of ps) await read({ path: p })",
        "while (true) await read({ path: 'a' })",
        "try { await read({ path: 'a' }) } catch (e) {}",
        "const t = (await read({ path: 'a' })).text.split('\\n').map(l => l.trim())",
    ] {
        assert!(!is_single_intent_cell(source, &one), "{source}");
    }
    // Nothing awaited, nothing at all.
    assert!(!is_single_intent_cell("read({ path: 'a' })", &one));
    assert!(!is_single_intent_cell("// only a comment\n", &one));
    assert!(!is_single_intent_cell("", &Vec::new()));
}

// --- shell_family ----------------------------------------------------------

#[test]
fn a_bash_call_is_classified_by_its_checked_command() {
    assert_eq!(
        shell_family(&bash("rg IntegrationId src", Ended::Ok)),
        Some(Family::Search)
    );
    assert_eq!(
        shell_family(&bash("cat README.md", Ended::Ok)),
        Some(Family::Read)
    );
    assert_eq!(shell_family(&bash("ls -la", Ended::Ok)), Some(Family::List));
    assert_eq!(
        shell_family(&bash("git status", Ended::Ok)),
        Some(Family::RepositoryState)
    );
    assert_eq!(
        shell_family(&bash("cargo test", Ended::Ok)),
        Some(Family::Verification)
    );
    assert_eq!(shell_family(&bash("make release", Ended::Ok)), None);
    assert_eq!(shell_family(&call("bash", Ended::Ok)), None);
}

#[test]
fn a_lifted_call_is_classified_by_the_program_it_was_lifted_from() {
    let mut lifted = call("rg", Ended::Ok);
    lifted.lifted_from = Some("rg".into());
    lifted.args.insert("pattern".into(), "IntegrationId".into());
    assert_eq!(shell_family(&lifted), Some(Family::Search));
    let mut read = call("read", Ended::Ok);
    read.lifted_from = Some("cat".into());
    assert_eq!(shell_family(&read), Some(Family::Read));
}

#[test]
fn a_call_that_is_not_shell_shaped_has_no_family() {
    assert_eq!(shell_family(&call("read", Ended::Ok)), None);
    assert_eq!(shell_family(&call("checks.run", Ended::Ok)), None);
}
