//! The structured task capsule derives its state from the trajectory alone.

use sterna::runtime::capsule::{Capsule, RENDER_CAP, State};
use sterna::runtime::outcome::{CallRecord, CellOutcomeKind, CellRecord, Ended};

fn call(tool: &str, args: &[(&str, &str)], ended: Ended, exit_code: Option<i32>) -> CallRecord {
    CallRecord {
        tool: tool.into(),
        args: args
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect(),
        evidence: None,
        lifted_from: None,
        exit_code,
        repeat_of: None,
        error: None,
        ended,
    }
}

fn cell(n: u64, calls: Vec<CallRecord>) -> CellRecord {
    CellRecord {
        cell: n,
        description: None,
        source: String::new(),
        outcome: CellOutcomeKind::Yielded,
        handles: Vec::new(),
        calls,
    }
}

#[test]
fn an_edit_then_a_passing_verification_is_verified_and_a_further_edit_is_not() {
    let mut capsule = Capsule::new("make the tests pass");
    capsule.observe_cell(
        &cell(
            1,
            vec![call(
                "edit",
                &[
                    ("path", "/app/src/lib.rs"),
                    ("after_sha256", "0123456789abcdef"),
                ],
                Ended::Ok,
                None,
            )],
        ),
        None,
        Some("tree-1"),
    );
    assert_eq!(capsule.state(), &State::InProgress);
    capsule.observe_cell(
        &cell(
            2,
            vec![call(
                "checks.run",
                &[
                    ("name", "tests"),
                    ("command", "cargo test"),
                    ("executed", "true"),
                    ("reused", "false"),
                ],
                Ended::Ok,
                Some(0),
            )],
        ),
        None,
        Some("tree-1"),
    );
    assert_eq!(capsule.state(), &State::Verified { cell: 2 });
    assert_eq!(capsule.checkpoint().unwrap().tree_digest, "tree-1");
    let rendered = capsule.render();
    assert!(rendered.contains("state: verified at cell 2"), "{rendered}");
    assert!(
        rendered.contains("edited /app/src/lib.rs (version 0123456789ab) (cell 1)"),
        "{rendered}"
    );
    assert!(
        rendered.contains("verified: cell 2 `cargo test` → exit 0"),
        "{rendered}"
    );

    capsule.observe_cell(
        &cell(
            3,
            vec![call(
                "write",
                &[("path", "/app/CHANGELOG.md")],
                Ended::Ok,
                None,
            )],
        ),
        None,
        Some("tree-2"),
    );
    assert_eq!(capsule.state(), &State::UnverifiedSince { cell: 3 });
    assert_eq!(capsule.checkpoint().unwrap().tree_digest, "tree-1");
    assert_eq!(capsule.to_json()["state"]["kind"], "unverified_since");
}

#[test]
fn a_denial_is_one_risk_however_often_it_repeats() {
    let mut capsule = Capsule::new("g");
    for n in 1..=4 {
        capsule.observe_cell(
            &cell(
                n,
                vec![call(
                    "bash",
                    &[("command", "curl http://x")],
                    Ended::Denied {
                        rule: "network".into(),
                    },
                    None,
                )],
            ),
            None,
            None,
        );
    }
    assert_eq!(capsule.risks().len(), 1);
    assert_eq!(capsule.risks()[0].since_cell, 1);
    assert_eq!(capsule.render().matches("denied").count(), 1);
}

#[test]
fn render_stays_under_the_bound_with_fifty_facts() {
    let mut capsule = Capsule::new("g".repeat(1_000));
    for n in 1..=50u64 {
        let path = format!("/app/{}/f{n}.c", "deep/".repeat(30));
        capsule.observe_cell(
            &cell(n, vec![call("edit", &[("path", &path)], Ended::Ok, None)]),
            Some(("RangeError", &"long message ".repeat(50))),
            None,
        );
    }
    let rendered = capsule.render();
    assert!(rendered.chars().count() <= RENDER_CAP, "{}", rendered.len());
    assert!(rendered.contains("(cell 50)"), "{rendered}");
    assert_eq!(capsule.to_json()["facts"].as_array().unwrap().len(), 8);
}

#[test]
fn salvage_marks_the_task_blocked() {
    let mut capsule = Capsule::new("g");
    capsule.salvage("provider returned 529 three times");
    assert!(matches!(capsule.state(), State::Blocked { reason } if reason.contains("529")));
    assert!(capsule.render().contains("blocked — provider returned 529"));
    assert!(
        capsule
            .risks()
            .iter()
            .any(|r| r.text.starts_with("cut off:"))
    );
}
