//! Acceptance for `smarter-cheaper-roadmap.md`, *Semantic read/search
//! lifting* (observation deduplication) and *Adaptive result reduction*
//! (pushed reduction routing).
//!
//! **No test here reaches a real provider.** The reduction tests point
//! `ANTHROPIC_BASE_URL` at a listener this file owns, copied from
//! `tests/helpers.rs`, and count what it was asked.
//!
//! Gated like `tests/lifting.rs`: every test runs a capability through a
//! real runtime, which on Windows would be refused rather than run
//! unconfined.
#![cfg(any(target_os = "macos", target_os = "linux"))]

use sterna::contract::SessionId;
use sterna::runtime::isolate::Runtime;
use sterna::runtime::outcome::{CellOutcome, Ended};
use sterna::runtime::preview::Value;
use sterna::sandbox::profile::Profile;

/// A grant that admits the shell command the lifting test writes, so the
/// only thing under test is whether the two spellings dedup against each
/// other.
const ADMITS: &str = r#"{"permissions":{"allow":["Bash","Read(**)"]}}"#;

fn fixture(name: &str) -> std::path::PathBuf {
    let root = std::env::temp_dir().join(format!(
        "sterna-observation-dedup-{name}-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("notes.txt"), "alpha\nbeta\n").unwrap();
    root
}

fn runtime(root: &std::path::Path, permissions: &str, session: &str) -> Runtime {
    let profile = Profile::compile(root, Some(permissions));
    Runtime::new(&profile, &SessionId::new(session))
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

// --- observation deduplication -------------------------------------------

/// The same `read` twice: the second call runs, returns the same bytes, and
/// is recorded as a repeat of the first — in the trajectory and in the
/// table header, where the model can see it without re-reading the body.
#[test]
fn the_same_read_twice_is_a_repeat_the_table_and_the_trajectory_both_say() {
    let root = fixture("twice");
    let mut runtime = runtime(&root, ADMITS, "twice");
    let first = runtime.run_cell("const a = await read({path:'notes.txt'});");
    assert_eq!(first.turn().record.calls[0].ended, Ended::Ok, "{first:?}");
    assert_eq!(first.turn().record.calls[0].repeat_of, None);
    assert_eq!(runtime.observation_repeats(), 0);

    let second = runtime.run_cell("const b = await read({path:'notes.txt'});");
    let call = &second.turn().record.calls[0];
    assert_eq!(call.ended, Ended::Ok, "{second:?}");
    assert_eq!(call.repeat_of, Some(1), "{call:?}");
    assert_eq!(runtime.observation_repeats(), 1);
    let json = serde_json::to_string(call).unwrap();
    assert!(json.contains(r#""repeat_of":1"#), "{json}");

    let handle = second
        .turn()
        .record
        .handles
        .iter()
        .find(|handle| handle.name == "b")
        .unwrap_or_else(|| panic!("{:?}", second.turn().record.handles));
    assert_eq!(
        handle.type_name,
        "File (unchanged since cell 1, same as `a`)"
    );
    assert!(
        second
            .turn()
            .table
            .contains("unchanged since cell 1, same as `a`"),
        "{}",
        second.turn().table
    );
    // The first handle's label is untouched: it is the original observation.
    let original = second
        .turn()
        .record
        .handles
        .iter()
        .find(|handle| handle.name == "a")
        .unwrap();
    assert_eq!(original.type_name, "File");

    // Task end resets the count with the handles.
    runtime.end_task();
    assert_eq!(runtime.observation_repeats(), 0);
    let _ = std::fs::remove_dir_all(root);
}

/// The call is never skipped: after the file changes the same `read` is a
/// fresh observation, and nothing says otherwise.
#[test]
fn a_read_after_the_file_changed_is_not_a_repeat() {
    let root = fixture("changed");
    let mut runtime = runtime(&root, ADMITS, "changed");
    runtime.run_cell("const a = await read({path:'notes.txt'});");
    std::fs::write(root.join("notes.txt"), "alpha\nbeta\ngamma\n").unwrap();
    let second = runtime.run_cell("const b = await read({path:'notes.txt'});");
    let call = &second.turn().record.calls[0];
    assert_eq!(call.ended, Ended::Ok, "{second:?}");
    assert_eq!(call.repeat_of, None, "{call:?}");
    assert_eq!(runtime.observation_repeats(), 0);
    let handle = second
        .turn()
        .record
        .handles
        .iter()
        .find(|handle| handle.name == "b")
        .unwrap();
    assert_eq!(handle.type_name, "File");
    let text = returned_text(&runtime.run_cell("return b.text;"));
    assert!(
        text.contains("gamma"),
        "the fresh bytes reached the program"
    );
    let _ = std::fs::remove_dir_all(root);
}

/// A lifted `cat` and a direct `read` of the same file both reach `read`
/// with the same checked path, so they dedup against each other — in either
/// order.
#[test]
fn a_lifted_cat_and_a_direct_read_dedup_against_each_other() {
    let root = fixture("lifted");
    let path = root.join("notes.txt");
    let mut runtime = runtime(&root, ADMITS, "lifted");
    let command = format!("cat {}", path.to_string_lossy());
    let lifted = runtime.run_cell(&format!(
        "const viaShell = await shell({{ command: {} }});",
        serde_json::to_string(&command).unwrap()
    ));
    let first = &lifted.turn().record.calls[0];
    assert_eq!(first.tool, "read", "{lifted:?}");
    assert_eq!(first.lifted_from.as_deref(), Some("cat"));
    assert_eq!(first.repeat_of, None);

    let direct = runtime.run_cell(&format!(
        "const viaRead = await read({{ path: {:?} }});",
        path.to_string_lossy()
    ));
    let second = &direct.turn().record.calls[0];
    assert_eq!(second.tool, "read", "{direct:?}");
    assert_eq!(second.repeat_of, Some(1), "{second:?}");
    assert!(
        direct
            .turn()
            .table
            .contains("unchanged since cell 1, same as `viaShell`"),
        "{}",
        direct.turn().table
    );

    // And a second lifted `cat` repeats the first observation too.
    let again = runtime.run_cell(&format!(
        "const viaShellAgain = await shell({{ command: {} }});",
        serde_json::to_string(&command).unwrap()
    ));
    assert_eq!(again.turn().record.calls[0].repeat_of, Some(1), "{again:?}");
    assert_eq!(runtime.observation_repeats(), 2);
    let _ = std::fs::remove_dir_all(root);
}

/// An effectful call is never a repeat, however identical its output: the
/// second `bash` ran again and its result is its own observation.
#[test]
fn an_effectful_call_is_never_a_repeat() {
    let root = fixture("effectful");
    let mut runtime = runtime(&root, ADMITS, "effectful");
    let outcome = runtime.run_cell(
        "const a = await bash({command:\"printf same\"});\n\
         const b = await bash({command:\"printf same\"});",
    );
    let calls = &outcome.turn().record.calls;
    assert_eq!(calls.len(), 2, "{outcome:?}");
    assert_eq!(calls[1].repeat_of, None);
    assert_eq!(runtime.observation_repeats(), 0);
    let _ = std::fs::remove_dir_all(root);
}

// --- reduction by rules ----------------------------------------------------

/// `printf` and brace expansion are bash builtins, so `bash` is the only
/// binary ever exec'd.
const PRINTF_ONLY: &str = r#"{"permissions":{"allow":["Bash(printf*)"]}}"#;

/// A threshold far below the default, so the trigger under test is the
/// configured one and not `STDOUT_TOKEN_CAP`.
const THRESHOLD: usize = 64;

/// A runtime whose `[limits] reduce_above_tokens` is [`THRESHOLD`].
fn reducing(root: &std::path::Path, session: &str) -> Runtime {
    let mut config = sterna::config::SternaConfig::default();
    config.limits.reduce_above_tokens = THRESHOLD;
    runtime(root, PRINTF_ONLY, session)
        .with_config(config)
        .expect("the configuration applies")
}

fn report_program(command: &str) -> String {
    format!(
        "const r = await bash({{ command: {command:?} }});\n\
         return r.stdout.length + \"|\" + (r.reduced === undefined ? \"none\" : r.reduced);\n"
    )
}

fn reported(outcome: &CellOutcome) -> (usize, String) {
    let text = returned_text(outcome);
    let (length, reduction) = text
        .split_once('|')
        .unwrap_or_else(|| panic!("expected `<length>|<reduction>`, got {text:?}"));
    (length.parse().expect(length), reduction.to_string())
}

/// Six hundred passing test lines and one failure: well over the threshold,
/// and exactly what the passing-test rule drops.
const PASSING_LOG: &str = r"printf 'test case_%s ... ok\n' {1..600}";

/// A result above the configured threshold is shortened by the rules, with
/// the line that says what was left out, and the task's figures say so.
#[test]
fn a_result_above_the_configured_threshold_is_ruled_and_counted() {
    let root = fixture("reduce-big");
    let mut runtime = reducing(&root, "reduce-big");
    let outcome = runtime.run_cell(&report_program(PASSING_LOG));

    let (length, reduction) = reported(&outcome);
    assert!(
        length / 4 > THRESHOLD,
        "{length} chars is not over the threshold"
    );
    assert!(
        length / 4 < sterna::runtime::preview::STDOUT_TOKEN_CAP,
        "the old cap must not be what fired: {length} chars"
    );
    assert!(
        reduction.starts_with("[sterna:reduction 600 lines / "),
        "the reduction leads with what it left out: {reduction}"
    );
    assert!(reduction.contains("rules: "), "{reduction}");
    let stats = runtime.reduction_stats();
    assert_eq!(stats.ruled, 1, "{stats:?}");
    assert_eq!(stats.bytes_in, length as u64, "{stats:?}");
    assert_eq!(stats.bytes_out, reduction.len() as u64, "{stats:?}");

    runtime.end_task();
    assert_eq!(runtime.reduction_stats(), Default::default());
    let _ = std::fs::remove_dir_all(root);
}

/// Below the threshold nothing is reduced; above it, an output no rule
/// recognises is left as it is.
#[test]
fn a_small_result_or_one_no_rule_recognises_is_left_whole() {
    let root = fixture("reduce-small");
    let mut runtime = reducing(&root, "reduce-small");
    let small = runtime.run_cell(&report_program(r"printf 'x %s\n' {1..20}"));
    let (small_length, small_reduction) = reported(&small);
    assert!(small_length / 4 < THRESHOLD, "{small_length}");
    assert_eq!(small_reduction, "none");

    let unrecognised = runtime.run_cell(&report_program(r"printf 'x %s\n' {1..200}"));
    let (unrecognised_length, unrecognised_reduction) = reported(&unrecognised);
    assert!(unrecognised_length / 4 > THRESHOLD, "{unrecognised_length}");
    assert_eq!(unrecognised_reduction, "none");

    assert_eq!(runtime.reduction_stats(), Default::default());
    let _ = std::fs::remove_dir_all(root);
}
