//! Acceptance for `smarter-cheaper-roadmap.md`, *Transactional mutation
//! composition*: sequential same-file edits and multi-hunk edits compile
//! into checked mutations, and only an external race is stale.
//!
//! Every tool here (`context`, `edit`, `write`) is in-process, so nothing
//! spawns and the file is not gated to a platform.

use sha2::{Digest, Sha256};
use sterna::contract::SessionId;
use sterna::runtime::isolate::Runtime;
use sterna::runtime::outcome::{CellOutcome, Ended};
use sterna::runtime::preview::Value;
use sterna::sandbox::profile::Profile;

fn fixture(label: &str) -> std::path::PathBuf {
    let root = std::env::temp_dir().join(format!(
        "sterna-mutation-composition-{label}-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("src")).unwrap();
    root
}

fn runtime(root: &std::path::Path, label: &str) -> Runtime {
    let profile = Profile::compile(root, None);
    Runtime::new(&profile, &SessionId::new(label))
}

fn returned_text(outcome: &CellOutcome) -> String {
    match outcome {
        CellOutcome::Returned {
            value: Value::String(text),
            ..
        } => text.head().to_string(),
        other => panic!("expected a returned string, got {other:?}"),
    }
}

fn hash(text: &str) -> String {
    format!("{:x}", Sha256::digest(text.as_bytes()))
}

/// The pilot's defect: the second edit of a file sterna itself had just edited
/// was refused as stale. After a successful `edit` the runtime registers
/// `after_sha256` at once, so the next edit in the same cell binds to it.
#[test]
fn two_sequential_edits_of_one_file_in_one_cell_both_apply() {
    let root = fixture("same-cell");
    let path = root.join("src/value.py");
    std::fs::write(&path, "alpha = 1\nbeta = 2\n").unwrap();
    let mut runtime = runtime(&root, "same-cell");
    runtime.run_cell("await context({path:'src/value.py'});");
    let changed = runtime.run_cell(
        "const first = await edit({path:'src/value.py', old:'alpha = 1', replacement:'alpha = 10'});\n\
         const second = await edit({path:'src/value.py', old:'beta = 2', replacement:'beta = 20'});\n\
         return first.after_sha256 === second.before_sha256 ? \"chained\" : \"unchained\";",
    );
    assert_eq!(returned_text(&changed), "chained", "{changed:?}");
    let calls = &changed.turn().record.calls;
    assert_eq!(calls.len(), 2, "{changed:?}");
    assert!(
        calls.iter().all(|call| call.ended == Ended::Ok),
        "{calls:?}"
    );
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "alpha = 10\nbeta = 20\n"
    );
    let _ = std::fs::remove_dir_all(root);
}

/// The same across cells: one `context`, then an edit in cell 2 and another
/// in cell 3 with no new `context` between them.
#[test]
fn an_edit_in_a_later_cell_binds_to_sternas_own_previous_edit() {
    let root = fixture("later-cell");
    let path = root.join("src/value.py");
    std::fs::write(&path, "alpha = 1\nbeta = 2\n").unwrap();
    let mut runtime = runtime(&root, "later-cell");
    runtime.run_cell("await context({path:'src/value.py'});");
    let first = runtime
        .run_cell("await edit({path:'src/value.py', old:'alpha = 1', replacement:'alpha = 10'});");
    assert_eq!(first.turn().record.calls[0].ended, Ended::Ok, "{first:?}");
    let second = runtime
        .run_cell("await edit({path:'src/value.py', old:'beta = 2', replacement:'beta = 20'});");
    assert_eq!(second.turn().record.calls[0].ended, Ended::Ok, "{second:?}");
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "alpha = 10\nbeta = 20\n"
    );
    assert_eq!(
        runtime.visible_version(&path).as_deref(),
        Some(hash("alpha = 10\nbeta = 20\n").as_str()),
        "the visible version is what sterna last wrote"
    );
    let _ = std::fs::remove_dir_all(root);
}

/// A `write` makes its bytes the visible version, so a following `edit`
/// needs no `context` — in the same cell and in a later one.
#[test]
fn a_written_file_can_be_edited_without_a_context() {
    let root = fixture("write-then-edit");
    let path = root.join("src/new.py");
    let mut runtime = runtime(&root, "write-then-edit");
    let same_cell = runtime.run_cell(
        "await write({path:'src/new.py', content:'value = 1\\n'});\n\
         await edit({path:'src/new.py', old:'value = 1', replacement:'value = 2'});",
    );
    let calls = &same_cell.turn().record.calls;
    assert_eq!(calls.len(), 2, "{same_cell:?}");
    assert_eq!(calls[1].tool, "edit");
    assert_eq!(calls[1].ended, Ended::Ok, "{same_cell:?}");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "value = 2\n");

    let later = runtime
        .run_cell("await edit({path:'src/new.py', old:'value = 2', replacement:'value = 3'});");
    assert_eq!(later.turn().record.calls[0].ended, Ended::Ok, "{later:?}");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "value = 3\n");

    // `lines` is the other spelling of a write, and it is hashed as written.
    let lines = runtime.run_cell(
        "await write({path:'src/lines.py', lines:['a = 1', 'b = 2']});\n\
         await edit({path:'src/lines.py', old:'b = 2', replacement:'b = 3'});",
    );
    assert_eq!(lines.turn().record.calls[1].ended, Ended::Ok, "{lines:?}");
    assert_eq!(
        std::fs::read_to_string(root.join("src/lines.py")).unwrap(),
        "a = 1\nb = 3\n"
    );
    let _ = std::fs::remove_dir_all(root);
}

/// Only Sterna's own mutation moves the visible version: a change something
/// else made between two of Sterna's edits is still a stale refusal, and the
/// external bytes are left exactly as they were.
#[test]
fn an_external_change_between_sternas_edits_is_still_stale() {
    let root = fixture("external");
    let path = root.join("src/value.py");
    std::fs::write(&path, "value = 1\n").unwrap();
    let mut runtime = runtime(&root, "external");
    runtime.run_cell("await context({path:'src/value.py'});");
    let first = runtime
        .run_cell("await edit({path:'src/value.py', old:'value = 1', replacement:'value = 2'});");
    assert_eq!(first.turn().record.calls[0].ended, Ended::Ok, "{first:?}");

    std::fs::write(&path, "value = 99\n").unwrap();
    let refused = runtime.run_cell(
        "try { await edit({path:'src/value.py', old:'value = 2', replacement:'value = 3'}); answer(\"applied\"); }\n\
         catch (e) { return e.message; }",
    );
    let message = returned_text(&refused);
    assert!(message.contains("The source version changed"), "{message}");
    assert_eq!(
        refused.turn().record.calls[0].ended,
        Ended::Threw {
            class: "ToolError".into()
        }
    );
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "value = 99\n");
    let _ = std::fs::remove_dir_all(root);
}

// --- atomic multi-hunk edits --------------------------------------------

const THREE: &str = "one\ntwo\nthree\nfour\nfive\n";

#[test]
fn three_hunks_apply_as_one_mutation() {
    let root = fixture("hunks");
    let path = root.join("src/list.txt");
    std::fs::write(&path, THREE).unwrap();
    let mut runtime = runtime(&root, "hunks");
    runtime.run_cell("await context({path:'src/list.txt'});");
    let changed = runtime.run_cell(
        "const r = await edit({path:'src/list.txt', olds:['one', 'three', 'five'], replacements:['1', '3', '5']});\n\
         return r.hunks.length + \"|\" + r.hunks.map(h => h.start).join(\",\") + \"|\" + r.changed_lines.start;",
    );
    assert_eq!(returned_text(&changed), "3|1,3,5|1", "{changed:?}");
    assert_eq!(changed.turn().record.calls[0].ended, Ended::Ok);
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "1\ntwo\n3\nfour\n5\n"
    );
    // The trajectory keeps the hunks as items, not as one joined string.
    let args = &changed.turn().record.calls[0].args;
    assert_eq!(
        args.get("olds").map(String::as_str),
        Some(r#"["one","three","five"]"#)
    );
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn a_hunk_that_does_not_match_leaves_the_file_byte_identical() {
    let root = fixture("missing-hunk");
    let path = root.join("src/list.txt");
    std::fs::write(&path, THREE).unwrap();
    let mut runtime = runtime(&root, "missing-hunk");
    runtime.run_cell("await context({path:'src/list.txt'});");
    let refused = runtime.run_cell(
        "try { await edit({path:'src/list.txt', olds:['one', 'absent', 'five'], replacements:['1', '?', '5']}); answer(\"applied\"); }\n\
         catch (e) { return e.message; }",
    );
    let message = returned_text(&refused);
    assert!(message.contains("hunk 1 (missing_match)"), "{message}");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), THREE);
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn overlapping_hunks_are_refused_without_writing() {
    let root = fixture("overlap");
    let path = root.join("src/list.txt");
    std::fs::write(&path, THREE).unwrap();
    let mut runtime = runtime(&root, "overlap");
    runtime.run_cell("await context({path:'src/list.txt'});");
    let refused = runtime.run_cell(
        "try { await edit({path:'src/list.txt', olds:['two\\nthree', 'three\\nfour'], replacements:['a', 'b']}); answer(\"applied\"); }\n\
         catch (e) { return e.message; }",
    );
    let message = returned_text(&refused);
    assert!(message.contains("overlapping_hunks"), "{message}");
    assert!(message.contains("hunks 0 and 1"), "{message}");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), THREE);
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn the_single_and_multi_forms_cannot_be_mixed_and_counts_must_pair() {
    let root = fixture("mixed");
    let path = root.join("src/list.txt");
    std::fs::write(&path, THREE).unwrap();
    let mut runtime = runtime(&root, "mixed");
    runtime.run_cell("await context({path:'src/list.txt'});");
    let mixed = runtime.run_cell(
        "try { await edit({path:'src/list.txt', old:'one', replacement:'1', olds:['two'], replacements:['2']}); answer(\"applied\"); }\n\
         catch (e) { return e.rule; }",
    );
    assert!(returned_text(&mixed).contains("not both"), "{mixed:?}");
    let mismatch = runtime.run_cell(
        "try { await edit({path:'src/list.txt', olds:['one', 'two'], replacements:['1']}); answer(\"applied\"); }\n\
         catch (e) { return e.message; }",
    );
    assert!(
        returned_text(&mismatch).contains("2 hunk(s) and replacements has 1"),
        "{mismatch:?}"
    );
    assert_eq!(std::fs::read_to_string(&path).unwrap(), THREE);
    let _ = std::fs::remove_dir_all(root);
}

/// A direct `Edit` with `old_strings` reaches the same kernel: lowered into
/// `edit({olds, replacements})`, run as a frame, applied atomically.
#[test]
fn a_direct_multi_hunk_edit_runs_through_the_same_kernel() {
    let root = fixture("direct");
    let path = root.join("src/list.txt");
    std::fs::write(&path, THREE).unwrap();
    let mut runtime = runtime(&root, "direct");
    runtime.run_cell("await context({path:'src/list.txt'});");
    let calls = vec![(
        "e".to_string(),
        "Edit".to_string(),
        serde_json::json!({
            "file_path": "src/list.txt",
            "old_strings": ["two", "four"],
            "new_strings": ["2", "4"]
        }),
    )];
    let lowered =
        sterna::abi::lower(sterna::abi::Dialect::Anthropic, &calls, runtime.next_cell()).unwrap();
    let outcome = runtime.run_direct_frame(&lowered.source);
    let record = &outcome.turn().record;
    assert_eq!(record.calls.len(), 1, "{outcome:?}");
    assert_eq!(record.calls[0].tool, "edit");
    assert_eq!(record.calls[0].ended, Ended::Ok, "{outcome:?}");
    assert_eq!(outcome.turn().capability_results.len(), 1);
    assert!(
        outcome.turn().capability_results[0].contains("\"hunks\":[{"),
        "{}",
        outcome.turn().capability_results[0]
    );
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "one\n2\nthree\n4\nfive\n"
    );
    let _ = std::fs::remove_dir_all(root);
}
