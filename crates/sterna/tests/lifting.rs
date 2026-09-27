//! Acceptance tests for `docs/tools.md`.
//!
//! Every test here goes through the real runtime, because the claim is about
//! what actually executes: a command the model wrote as shell either becomes
//! the stronger capability or runs exactly as written, and the trajectory says
//! which. The recognizer's own grammar is proven in its unit tests; this file
//! proves the wiring, the safety rule and the fallbacks.
//!
//! Gated to macOS and Linux like the rest of Sterna's executing tests: on
//! Windows `tools::invoke` refuses to spawn rather than running unconfined,
//! so "the capability ran" cannot be asserted there for reasons that have
//! nothing to do with lifting.
#![cfg(any(target_os = "macos", target_os = "linux"))]

use sterna::contract::SessionId;
use sterna::runtime::isolate::Runtime;
use sterna::runtime::outcome::{CellOutcome, CellRecord};
use sterna::sandbox::profile::Profile;

/// A grant that admits the shell commands these tests write, so the only
/// thing under test is whether sterna lifted them.
const ADMITS: &str = r#"{"permissions":{"allow":["Bash","Read(**)"]}}"#;

fn fixture(name: &str) -> std::path::PathBuf {
    let root = std::env::temp_dir().join(format!("sterna-lifting-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(
        root.join("target.rs"),
        "one SessionManager\ntwo\nthree\nfour\nfive\n",
    )
    .unwrap();
    root
}

fn record(outcome: &CellOutcome) -> &CellRecord {
    match outcome {
        CellOutcome::Yielded { turn }
        | CellOutcome::Returned { turn, .. }
        | CellOutcome::Threw { turn, .. } => &turn.record,
    }
}

/// Runs one shell-shaped call and returns the trajectory it produced.
fn shell(root: &std::path::Path, permissions: &str, command: &str) -> (String, Option<String>) {
    let profile = Profile::compile(root, Some(permissions));
    let mut runtime = Runtime::new(&profile, &SessionId::new("lifting-session"));
    let source = format!(
        "await shell({{ command: {} }});\n",
        serde_json::to_string(command).unwrap()
    );
    let outcome = runtime.run_cell(&source);
    let record = record(&outcome);
    let call = record
        .calls
        .first()
        .unwrap_or_else(|| panic!("a call should have run: {outcome:?}"));
    (call.tool.clone(), call.lifted_from.clone())
}

#[test]
fn a_search_command_becomes_the_search_capability() {
    let root = fixture("search");
    let command = format!("rg SessionManager {}", root.to_string_lossy());
    let (tool, lifted_from) = shell(&root, ADMITS, &command);
    assert_eq!(tool, "rg", "the search capability ran, not an opaque shell");
    assert_eq!(
        lifted_from.as_deref(),
        Some("rg"),
        "the ledger says what the model actually wrote"
    );
}

#[test]
fn the_other_search_spelling_reaches_a_search_capability_too() {
    let root = fixture("grep");
    let command = format!("grep -rn SessionManager {}", root.to_string_lossy());
    let (tool, lifted_from) = shell(&root, ADMITS, &command);
    assert_eq!(tool, "grep");
    assert_eq!(lifted_from.as_deref(), Some("grep"));
}

#[test]
fn a_read_command_becomes_the_read_capability() {
    let root = fixture("read");
    let command = format!("cat {}", root.join("target.rs").to_string_lossy());
    let (tool, lifted_from) = shell(&root, ADMITS, &command);
    assert_eq!(tool, "read");
    assert_eq!(lifted_from.as_deref(), Some("cat"));
}

/// The range must survive the lift exactly: `head -n 2` is the first two
/// lines, not the whole file and not a summary of it.
#[test]
fn a_range_read_keeps_exactly_the_range_that_was_asked_for() {
    let root = fixture("range");
    let profile = Profile::compile(&root, Some(ADMITS));
    let mut runtime = Runtime::new(&profile, &SessionId::new("lifting-range"));
    let command = format!("head -n 2 {}", root.join("target.rs").to_string_lossy());
    // `shell` promises a process result, and a lift must not change that: the
    // range is exact *and* `stdout` still means stdout.
    let source = format!(
        "const r = await shell({{ command: {} }});\nreturn r.stdout;\n",
        serde_json::to_string(&command).unwrap()
    );
    let outcome = runtime.run_cell(&source);
    let CellOutcome::Returned {
        terminal: sterna::runtime::outcome::Terminal::Text(text),
        ..
    } = &outcome
    else {
        panic!("the program returns the text it read: {outcome:?}");
    };
    assert_eq!(
        text, "one SessionManager\ntwo\n",
        "an exact range, and nothing beyond it"
    );
    assert_eq!(record(&outcome).calls[0].tool, "read");
}

/// The safety rule. `bash` is admitted by command-line admission while `read`
/// is admitted by a path check, and conflating the two would let a project
/// that refuses a command get its result anyway.
#[test]
fn a_command_the_grant_refuses_is_never_lifted_into_one_that_would_be_allowed() {
    let root = fixture("refused");
    let command = format!("cat {}", root.join("target.rs").to_string_lossy());
    // Read is granted; the shell command is not admitted at all.
    let refuses_shell = r#"{"permissions":{"allow":["Read(**)"]}}"#;
    let profile = Profile::compile(&root, Some(refuses_shell));
    let mut runtime = Runtime::new(&profile, &SessionId::new("lifting-refused"));
    let source = format!(
        "try {{ await shell({{ command: {} }}); return \"ran\"; }}\n\
         catch (e) {{ return \"refused\"; }}\n",
        serde_json::to_string(&command).unwrap()
    );
    let outcome = runtime.run_cell(&source);
    let record = record(&outcome);
    assert_eq!(
        record.calls[0].tool, "bash",
        "an unadmitted command stays the command the model wrote, so the \
         shell's own refusal is what answers it"
    );
    assert!(record.calls[0].lifted_from.is_none());
}

#[test]
fn an_unsupported_flag_falls_back_instead_of_being_approximated() {
    let root = fixture("flag");
    let command = format!("rg -i SessionManager {}", root.to_string_lossy());
    let (tool, lifted_from) = shell(&root, ADMITS, &command);
    assert_eq!(
        tool, "bash",
        "case-insensitivity changes which lines match, so the command runs as written"
    );
    assert!(lifted_from.is_none());
}

#[test]
fn compound_shell_is_never_reinterpreted() {
    let root = fixture("compound");
    for command in [
        format!("rg SessionManager {} | head -n 1", root.to_string_lossy()),
        format!(
            "cat {} && echo done",
            root.join("target.rs").to_string_lossy()
        ),
        format!(
            "cat {} > /dev/null",
            root.join("target.rs").to_string_lossy()
        ),
    ] {
        let (tool, _) = shell(&root, ADMITS, &command);
        assert_eq!(tool, "bash", "`{command}` is a program, not one command");
    }
}

#[test]
fn a_mutation_is_left_alone() {
    let root = fixture("mutation");
    let command = format!("rm {}", root.join("target.rs").to_string_lossy());
    let (tool, lifted_from) = shell(&root, ADMITS, &command);
    assert_eq!(tool, "bash");
    assert!(lifted_from.is_none());
}

/// The same shell-shaped request reaches the same capability whether the model
/// sent it directly or wrote it inside a cell — `semantic-command-lifting.md`,
/// *Direct calls and Cells remain semantically identical*.
#[test]
fn a_direct_call_and_a_cell_call_lift_identically() {
    let root = fixture("equivalence");
    let command = format!("rg SessionManager {}", root.to_string_lossy());
    let profile = Profile::compile(&root, Some(ADMITS));

    // Direct: lowered from a provider tool call.
    let calls = vec![(
        "call-1".to_string(),
        "shell".to_string(),
        serde_json::json!({"command": command}),
    )];
    let lowered = sterna::abi::lower(sterna::abi::Dialect::OpenAi, &calls, 1).unwrap();
    let mut direct = Runtime::new(&profile, &SessionId::new("lift-direct"));
    let direct_outcome = direct.run_direct_frame(&lowered.source);

    // Authored: the model wrote the same call itself.
    let mut authored = Runtime::new(&profile, &SessionId::new("lift-cell"));
    let authored_outcome = authored.run_cell(&format!(
        "await shell({{ command: {} }});\n",
        serde_json::to_string(&command).unwrap()
    ));

    let direct_call = &record(&direct_outcome).calls[0];
    let authored_call = &record(&authored_outcome).calls[0];
    assert_eq!(direct_call.tool, "rg");
    assert_eq!(direct_call.tool, authored_call.tool);
    assert_eq!(direct_call.args, authored_call.args);
    assert_eq!(direct_call.lifted_from, authored_call.lifted_from);
}

/// A lift substitutes the capability, so the arguments recorded are the
/// capability's checked ones — the path resolved, not the spelling written.
#[test]
fn a_lifted_call_records_the_capabilitys_own_checked_arguments() {
    let root = fixture("checked");
    let command = format!("rg SessionManager {}", root.to_string_lossy());
    let profile = Profile::compile(&root, Some(ADMITS));
    let mut runtime = Runtime::new(&profile, &SessionId::new("lift-checked"));
    let outcome = runtime.run_cell(&format!(
        "await shell({{ command: {} }});\n",
        serde_json::to_string(&command).unwrap()
    ));
    let call = &record(&outcome).calls[0];
    assert_eq!(
        call.args.get("pattern").map(String::as_str),
        Some("SessionManager")
    );
    assert!(
        call.args.contains_key("path"),
        "the search root is a checked path: {:?}",
        call.args
    );
    assert!(
        !call.args.contains_key("command"),
        "the opaque command line is not what ran: {:?}",
        call.args
    );
}
