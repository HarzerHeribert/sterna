//! The no-progress guard notices an identical retry once, and the verified
//! checkpoint knows when later work disturbed it.

use sterna::progress::{CheckpointStatus, Checkpoints, Guard, NO_PROGRESS_NOTICE, fingerprint};
use sterna::runtime::outcome::{CallRecord, CellOutcomeKind, CellRecord, Ended};

fn frame(cell: u64, command: &str) -> CellRecord {
    CellRecord {
        cell,
        description: None,
        source: format!("await bash({{command: '{command}'}}) // cell {cell}"),
        outcome: CellOutcomeKind::Threw,
        handles: Vec::new(),
        calls: vec![CallRecord {
            tool: "bash".into(),
            args: [("command".to_string(), command.to_string())]
                .into_iter()
                .collect(),
            evidence: None,
            lifted_from: None,
            exit_code: None,
            repeat_of: None,
            error: None,
            ended: Ended::Denied {
                rule: "network".into(),
            },
        }],
    }
}

const ERROR: Option<(&str, &str)> = Some(("ToolError", "bash: network access denied by rule"));

#[test]
fn a_planted_three_repeat_loop_is_noticed_on_the_second_repeat_and_only_once() {
    let mut guard = Guard::new(2);
    let notices: Vec<Option<String>> = (1..=3)
        .map(|cell| guard.observe(fingerprint(&frame(cell, "curl http://x"), ERROR, Some("t"))))
        .collect();
    assert!(notices[0].is_none());
    assert_eq!(notices[1].as_deref(), Some(NO_PROGRESS_NOTICE));
    assert!(notices[2].is_none());
    assert_eq!(guard.notices(), 1);
}

#[test]
fn a_retry_with_different_arguments_is_progress() {
    let mut guard = Guard::new(2);
    assert!(
        guard
            .observe(fingerprint(&frame(1, "curl http://x"), ERROR, Some("t")))
            .is_none()
    );
    assert!(
        guard
            .observe(fingerprint(&frame(2, "curl http://y"), ERROR, Some("t")))
            .is_none()
    );
    assert_eq!(guard.notices(), 0);
}

#[test]
fn a_retry_after_the_tree_changed_is_progress() {
    let mut guard = Guard::new(2);
    assert!(
        guard
            .observe(fingerprint(&frame(1, "curl http://x"), ERROR, Some("t1")))
            .is_none()
    );
    assert!(
        guard
            .observe(fingerprint(&frame(2, "curl http://x"), ERROR, Some("t2")))
            .is_none()
    );
    assert_eq!(guard.notices(), 0);
}

#[test]
fn the_checkpoint_moves_none_verified_disturbed_and_back_to_verified() {
    let mut checkpoints = Checkpoints::default();
    assert_eq!(checkpoints.status(), CheckpointStatus::None);
    checkpoints.note_mutation(1, "a");
    assert_eq!(checkpoints.status(), CheckpointStatus::None);
    checkpoints.note_verification(2, "a", true);
    assert_eq!(checkpoints.status(), CheckpointStatus::Verified { cell: 2 });
    checkpoints.note_mutation(3, "b");
    assert_eq!(
        checkpoints.status(),
        CheckpointStatus::Disturbed {
            verified_cell: 2,
            mutation_cell: 3
        }
    );
    checkpoints.note_verification(4, "b", false);
    assert!(matches!(
        checkpoints.status(),
        CheckpointStatus::Disturbed { .. }
    ));
    checkpoints.note_verification(5, "b", true);
    assert_eq!(checkpoints.status(), CheckpointStatus::Verified { cell: 5 });
}
