//! Acceptance tests for a subagent's lifetime (`docs/subagents.md`).
//!
//! Requirements already proven elsewhere, not duplicated here:
//!
//! - `subagent.rs::a_subagent_answers_in_a_later_event_and_never_blocks_the_caller`
//! - `events.rs::cancelling_a_job_that_ignores_sigterm_still_stops_it_and_reports`
//!   and `events.rs::shutdown_leaves_no_job_of_this_session_running`
//! - the `agent.run` depth check in `bindings.rs`, asserted below

use sterna::contract::SessionId;
use sterna::runtime::isolate::Runtime;
use sterna::runtime::outcome::{CellOutcome, Terminal};
use sterna::sandbox::profile::Profile;

fn root(name: &str) -> std::path::PathBuf {
    let root = std::env::temp_dir().join(format!("sterna-lifetimes-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    root
}

fn returned_text(outcome: &CellOutcome) -> String {
    match outcome {
        CellOutcome::Returned {
            terminal: Terminal::Text(text),
            ..
        } => text.clone(),
        other => panic!("the program returns a string: {other:?}"),
    }
}

/// Requirement 10: a subagent cannot start another subagent.
///
/// Enforced in `bindings.rs`'s `agent.run` by the depth flag rather than by
/// narrowing, because an ordinary subagent is otherwise a full runtime. The
/// refusal is a value, not a panic (`sandbox-grants.md` §1.4).
#[test]
fn a_subagent_may_not_start_a_subagent() {
    let root = root("no-nested-agent");
    let profile = Profile::compile(&root, Some(r#"{"permissions":{"allow":[]}}"#));
    let mut subagent = Runtime::new(&profile, &SessionId::new("lifetimes-nested")).as_subagent();

    let outcome = subagent.run_cell(
        "try { agent.run(\"do a thing\", {turns: 1}); return \"started\"; }\n\
         catch (e) { return \"refused: \" + e.message; }\n",
    );
    let text = returned_text(&outcome);
    assert!(
        text.starts_with("refused:"),
        "a subagent that starts a subagent must be refused: {text}"
    );
    assert!(
        text.contains("may not start a subagent"),
        "and the refusal must say why: {text}"
    );
}
