use sterna::prompt::{self, declarations};
use sterna::tools::registry;

#[test]
fn preamble_states_the_observation_and_batching_decisions() {
    let text = prompt::PREAMBLE;
    for rule in [
        "unseen fields are not model-visible",
        "Reuse live handles rather than repeating a read",
        "`context({path, symbol})` is the first source-reading tool",
        "`edit({path, old, replacement})`",
        "Batch deterministic work",
        "wait for its correlated result",
        "Never invent output or\ninfer success",
        "`glob` may return directories",
        "`exit_code`",
    ] {
        assert!(text.contains(rule), "missing guidance: {rule}");
    }
    assert!(text.contains("Answer conversational questions naturally"));
    assert!(text.contains("provider-native tool"));
}

/// The preamble teaches what a turn buys, not only what a cell must avoid.
///
/// Measured 2026-09-17 in two real sessions on one hard task (`tlitep-13fv`,
/// `tlj14m-24r`, 120 cells each): **1.6 and 1.98 tool calls per cell**, with
/// 20 cells making no call at all. The instruction to batch was present, as a
/// subordinate clause among roughly fifteen prohibitions; a cell was being
/// used as a single tool call, and each one cost a whole request and result.
/// These four pins are the shape that replaced it.
#[test]
fn the_preamble_teaches_what_a_turn_buys() {
    let text = prompt::PREAMBLE;

    // 1. What a cell is, before what it must not do.
    assert!(
        text.contains("A cell is a program, and that is what earns it a turn"),
        "the preamble must say what a turn buys: {text}"
    );

    // 2. Worked cells, not only prose about them — the batched inspection,
    //    the edit-and-check, and the judgement the cell branches on.
    for worked in [
        "await Promise.all",
        "const failures = await helper.reduce(run.stdout);",
        "const call = await decide.choice(",
        "if (call.choice === \"wider\" && call.confidence > 0.85)",
    ] {
        assert!(text.contains(worked), "missing worked example: {worked}");
    }

    // 3. A helper and a judgement cost no turn, said where it is useful.
    assert!(text.contains("cost no\nturn"), "{text}");

    // 4. The rhythm `edit` imposes, stated before the model can trip it
    //    (`runtime/bindings.rs` terminates a cell whose edit has no
    //    correlated context from a previous cell; it fired 5 times in the
    //    second measured session).
    assert!(
        text.contains("turns for the whole batch, not two per file"),
        "the preamble must price the context-then-edit rhythm: {text}"
    );
}

#[test]
fn declarations_cover_tool_specific_decisions() {
    let mut names: Vec<_> = declarations::ENTRIES
        .iter()
        .map(|entry| entry.name)
        .collect();
    let mut registered = registry::names();
    names.sort_unstable();
    registered.sort_unstable();
    assert_eq!(names, registered);
    assert!(
        declarations::lookup("glob")
            .unwrap()
            .summary
            .contains("may include directories")
    );
    assert!(
        declarations::lookup("bash")
            .unwrap()
            .summary
            .contains("inspect `exit_code`")
    );
    assert!(
        declarations::lookup("context")
            .unwrap()
            .summary
            .contains("complete target")
    );
    assert!(
        declarations::lookup("edit")
            .unwrap()
            .summary
            .contains("Stale")
    );

    let runtime = declarations::RUNTIME
        .iter()
        .map(|binding| binding.declaration)
        .collect::<Vec<_>>()
        .join("\n");
    assert!(runtime.contains("compact structured summary"));
    assert!(runtime.contains("Use background work only"));
    assert!(runtime.contains("Use it only when the question is separable"));
}
