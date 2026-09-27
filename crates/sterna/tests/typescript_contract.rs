//! The advertised TypeScript contract, end to end: every construct
//! `ERASABLE_CONSTRUCTS` names compiles, erases in place (no line changes
//! width — `runtime-contract.md` §5) and runs in a real isolate; every
//! construct `NOT_ERASABLE_CONSTRUCTS` names is refused **before** execution
//! with a diagnostic at the model's own line that names the construct and
//! what to write instead.
//!
//! No program here calls a tool, so nothing spawns and nothing is gated to
//! a platform.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use sterna::contract::SessionId;
use sterna::runtime::cell::{self, ERASABLE_CONSTRUCTS, NOT_ERASABLE_CONSTRUCTS};
use sterna::runtime::isolate::Runtime;
use sterna::runtime::outcome::CellOutcome;
use sterna::runtime::preview::Value;
use sterna::sandbox::profile::Profile;

static COUNTER: AtomicU64 = AtomicU64::new(0);

/// A throwaway project root with a `.claude/`, removed on drop.
struct Fixture {
    root: PathBuf,
}

impl Fixture {
    fn new(label: &str) -> Self {
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "sterna-ts-contract-{}-{label}-{n}",
            std::process::id()
        ));
        std::fs::create_dir_all(root.join(".claude")).unwrap();
        Self { root }
    }

    fn runtime(&self, session: &str) -> Runtime {
        let profile = Profile::compile(
            &self.root,
            Some(r#"{"permissions":{"allow":["Bash(echo*)"]}}"#),
        );
        Runtime::new(&profile, &SessionId::new(session))
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// The model's own lines back out of the generated program: the one-line
/// prologue dropped, the epilogue dropped, and each line's capture splice
/// (always appended at the line's end) cut off again.
fn body_of(javascript: &str) -> String {
    javascript
        .lines()
        .skip(cell::LINE_OFFSET as usize)
        .take_while(|line| *line != ";return __sterna_cell.e();")
        .map(|line| line.split(";__sterna_cell.s(").next().unwrap_or(line))
        .collect::<Vec<_>>()
        .join("\n")
}

fn returned_string(outcome: &CellOutcome) -> String {
    match outcome {
        CellOutcome::Returned {
            value: Value::String(text),
            ..
        } => text.head().to_string(),
        other => panic!("expected a returned string, got {other:?}"),
    }
}

/// Item 2 of the contract: each advertised construct compiles, keeps every
/// line's width, and runs to the marker.
#[test]
fn every_erasable_construct_compiles_keeps_its_columns_and_runs() {
    let fixture = Fixture::new("erasable");
    for (construct, program) in ERASABLE_CONSTRUCTS {
        let compiled =
            cell::compile(program, 1).unwrap_or_else(|error| panic!("{construct}: {error:?}"));
        let body = body_of(&compiled.javascript);
        assert_eq!(
            program.lines().count(),
            body.lines().count(),
            "{construct}: the erased body has a different number of lines"
        );
        for (original, erased) in program.lines().zip(body.lines()) {
            assert_eq!(
                original.chars().count(),
                erased.chars().count(),
                "{construct}: a line changed width: {original:?} -> {erased:?}"
            );
        }
        // A fresh runtime per row: a string return ends the task, and one
        // row's bindings must not be what makes the next row pass.
        let mut runtime = fixture.runtime(&format!("erasable-{construct}"));
        let outcome = runtime.run_cell(program);
        assert!(
            matches!(&outcome, CellOutcome::Returned { .. }),
            "{construct}: {outcome:?}"
        );
        assert_eq!(returned_string(&outcome), "ok", "{construct}");
    }
}

/// Item 3: each refused construct is a `TypeScriptNotErasable` throw in
/// the model's own turn slot, before anything ran, at the model's own line,
/// naming the construct and the alternative.
#[test]
fn every_refused_construct_is_named_before_execution_with_an_alternative() {
    let fixture = Fixture::new("refused");
    for (construct, example, alternative) in NOT_ERASABLE_CONSTRUCTS {
        let mut runtime = fixture.runtime(&format!("refused-{construct}"));
        let outcome = runtime.run_cell(&format!("const ran = 1;\n{example}\n"));
        let CellOutcome::Threw { error, .. } = &outcome else {
            panic!("{construct}: expected a throw, got {outcome:?}");
        };
        assert_eq!(error.class, "TypeScriptNotErasable", "{construct}");
        assert!(
            error.message.contains(construct),
            "{construct}: {}",
            error.message
        );
        assert!(
            error.message.contains(alternative),
            "{construct}: {}",
            error.message
        );
        assert_eq!(error.line, Some(2), "{construct}: {error:?}");
        // Nothing ran: line 1's binding was never made.
        assert!(!runtime.is_live("ran"), "{construct}: the cell ran");
    }
}

/// Item 4: the Terminal-Bench pilot's failing shape, pinned by name. The
/// free-name scan read `"active" as const` as a reference to a type named
/// `const` and refused the cell with `ReferenceError: \`const\` is not
/// defined` before it ran.
#[test]
fn as_const_in_an_object_literal_compiles_and_runs() {
    let program = "const todos = [\n  {text: \"Read the brief\", status: \"active\" as const},\n  {text: \"Run the tests\", status: \"pending\" as const},\n];\nreturn JSON.stringify(todos);\n";
    let compiled = cell::compile(program, 1).unwrap();
    assert!(
        compiled.free_names.iter().all(|(name, _)| name != "const"),
        "{:?}",
        compiled.free_names
    );
    for (original, erased) in program.lines().zip(body_of(&compiled.javascript).lines()) {
        assert_eq!(
            original.chars().count(),
            erased.chars().count(),
            "a line changed width: {original:?} -> {erased:?}"
        );
    }

    let fixture = Fixture::new("as-const");
    let mut runtime = fixture.runtime("as-const");
    let outcome = runtime.run_cell(program);
    assert_eq!(
        returned_string(&outcome),
        "[{\"text\":\"Read the brief\",\"status\":\"active\"},{\"text\":\"Run the tests\",\"status\":\"pending\"}]",
        "{outcome:?}"
    );
}

/// Item 1's other direction, at the isolate: a type-only declaration binds
/// nothing at value level, so reading its name is still the `ReferenceError`
/// the scan exists to report — in the turn that wrote it, at its own line.
#[test]
fn a_type_only_name_is_still_undefined_at_value_level() {
    let fixture = Fixture::new("type-only");
    let mut runtime = fixture.runtime("type-only");
    let outcome = runtime.run_cell("type T = string;\nreturn T;\n");
    let CellOutcome::Threw { error, .. } = &outcome else {
        panic!("expected a throw, got {outcome:?}");
    };
    assert_eq!(error.class, "ReferenceError");
    assert!(
        error.message.starts_with("`T` is not defined"),
        "{}",
        error.message
    );
    assert_eq!(error.line, Some(2), "{error:?}");
}
