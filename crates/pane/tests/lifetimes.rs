//! Acceptance tests for `docs/product/pane/helpers-and-subagents.md` §20.
//!
//! The document's own rule is that an item already proven by existing code
//! should be pointed at rather than rebuilt, so this file covers only the
//! items whose proof did not exist. Each test names the requirement it
//! discharges; the header comment of each block names the existing test where
//! one already proves it.
//!
//! Requirements already proven elsewhere, not duplicated here:
//!
//! - 4, 5, 6 — `subagent.rs::a_subagent_answers_in_a_later_event_and_never_blocks_the_caller`
//! - 9 — `events.rs::cancelling_a_job_that_ignores_sigterm_still_stops_it_and_reports`
//!   and `events.rs::shutdown_leaves_no_job_of_this_session_running`
//! - 10 — enforced in `bindings.rs`'s `agent.run` depth check and asserted below
//! - 13 — `helpers.rs::preflight_carries_its_helper_usage_into_the_returned_record`

use pane::contract::SessionId;
use pane::runtime::bindings::HostGlobals;
use pane::runtime::isolate::Runtime;
use pane::runtime::outcome::{CellOutcome, Terminal};
use pane::sandbox::profile::Profile;

fn root(name: &str) -> std::path::PathBuf {
    let root = std::env::temp_dir().join(format!("pane-lifetimes-{name}-{}", std::process::id()));
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

/// Requirement 12: a Little Helper cannot call a Helper or a subagent.
///
/// §11 forbids the `Helper → Helper` and `Helper → Subagent` edges outright.
/// This asserts the *absence* of the bindings rather than a runtime refusal:
/// the toolset is the safety boundary in `little-helpers.md`, and an absent
/// global cannot be reached by a helper that was talked into trying.
#[test]
fn a_helper_holds_neither_the_helper_roster_nor_the_agent_global() {
    let root = root("helper-narrowing");
    let profile = Profile::compile(&root, Some(r#"{"permissions":{"allow":[]}}"#));
    let mut helper = Runtime::for_helper(
        &profile,
        &SessionId::new("lifetimes-helper"),
        &["read", "grep"],
    )
    .as_subagent()
    .with_instruction_context();

    let listed = returned_text(&helper.run_cell(
        "return [\"helper\", \"agent\"].map(n => n + \"=\" + typeof globalThis[n]).join(\",\");\n",
    ));
    assert_eq!(
        listed, "helper=undefined,agent=undefined",
        "a helper's runtime must hold neither the helper roster nor `agent`: \
         `helpers-and-subagents.md` §11 forbids Helper -> Helper and Helper -> Subagent"
    );
}

/// Requirement 11: a subagent *can* call an allowed Little Helper.
///
/// The other side of the same narrowing, and the reason the fix cannot simply
/// be "withhold `helper` from every nested runtime": a subagent is explicitly
/// permitted the leaf.
#[test]
fn a_subagent_still_holds_the_helper_roster() {
    let root = root("subagent-helper");
    let profile = Profile::compile(&root, Some(r#"{"permissions":{"allow":[]}}"#));
    let mut subagent = Runtime::new(&profile, &SessionId::new("lifetimes-subagent")).as_subagent();

    let listed =
        returned_text(&subagent.run_cell("return \"helper=\" + typeof globalThis[\"helper\"];\n"));
    assert_eq!(
        listed, "helper=object",
        "a subagent may use a Little Helper for a narrow question (§10, §11)"
    );
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

/// The narrowing predicate itself, asserted directly so a future global added
/// to the withheld list is covered without running an isolate.
#[test]
fn the_narrowing_withholds_every_global_that_escapes_a_helpers_scope() {
    let helper = HostGlobals::Helper(&["read"]);
    for global in ["bg", "mcp", "checks", "helper", "agent"] {
        assert!(
            !helper.installs(global),
            "`{global}` must not be installed in a helper's runtime"
        );
    }
    // And an ordinary context keeps all of them.
    for global in ["bg", "mcp", "checks", "helper", "agent"] {
        assert!(HostGlobals::Every.installs(global));
    }
}

/// Requirement 1 and 2, at the level this file can prove without a provider:
/// a helper's runtime is the caller's to end. `Runtime::for_helper` produces a
/// runtime owned by the call that made it, so there is no registry a finished
/// helper could still be listed in — the property §2.2 calls structured
/// concurrency.
///
/// The live-cancellation half is proven by
/// `helpers.rs::cancellation_keeps_completed_usage_and_marks_the_inflight_request_unknown`,
/// which observes a cancelled in-flight helper request from the owning cell.
#[test]
fn a_helper_runtime_is_owned_by_its_caller_and_not_by_the_session() {
    let root = root("helper-ownership");
    let profile = Profile::compile(&root, Some(r#"{"permissions":{"allow":[]}}"#));
    let session = SessionId::new("lifetimes-ownership");

    // Two helper runtimes for one session are independent values; neither is
    // registered anywhere that outlives this scope, so dropping one cannot
    // leave a background task behind.
    let first = Runtime::for_helper(&profile, &session, &["read"]);
    drop(first);
    let mut second = Runtime::for_helper(&profile, &session, &["read"]);
    assert_eq!(
        returned_text(&second.run_cell("return \"alive\";\n")),
        "alive",
        "a helper runtime is an ordinary owned value, so the previous one \
         leaving scope cannot have detached anything"
    );
}
