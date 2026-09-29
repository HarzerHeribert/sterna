use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use sterna::contract::SessionId;
use sterna::runtime::isolate::Runtime;
use sterna::runtime::repair::{SOURCE_BYTE_CAP, SyntaxFailure};
use sterna::sandbox::profile::Profile;

static NEXT: AtomicU64 = AtomicU64::new(0);

struct Fixture(PathBuf);

impl Fixture {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "sterna-cell-repair-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(path.join(".claude")).unwrap();
        Self(path)
    }

    fn runtime(&self) -> Runtime {
        Runtime::new(
            &Profile::compile(&self.0, None),
            &SessionId::new("cell-repair"),
        )
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn exact_edit_requires_current_cell_one_nonempty_match_and_a_change() {
    let failure = SyntaxFailure::new(7, "const x = nope;\nconst y = nope;\n", 1, 0).unwrap();
    assert!(
        failure
            .apply(r#"{"cell":6,"replace":"x","with":"z"}"#)
            .is_err()
    );
    assert!(
        failure
            .apply(r#"{"cell":7,"replace":"","with":"z"}"#)
            .is_err()
    );
    assert!(
        failure
            .apply(r#"{"cell":7,"replace":"missing","with":"z"}"#)
            .is_err()
    );
    assert!(
        failure
            .apply(r#"{"cell":7,"replace":"nope","with":"yes"}"#)
            .is_err()
    );
    assert!(
        failure
            .apply(r#"{"cell":7,"replace":"x","with":"x"}"#)
            .is_err()
    );
    assert!(
        failure
            .apply(r#"{"cell":7,"replace":"x","with":"z","extra":1}"#)
            .is_err()
    );
    assert_eq!(
        failure
            .apply(r#"{"cell":7,"replace":"const x = nope;","with":"const x = yes;"}"#)
            .unwrap(),
        "const x = yes;\nconst y = nope;\n"
    );
}

#[test]
fn repair_input_and_result_are_bounded() {
    assert!(SyntaxFailure::new(1, &"x".repeat(SOURCE_BYTE_CAP + 1), 1, 0).is_none());
    let failure = SyntaxFailure::new(1, "x", 1, 0).unwrap();
    let json = format!(
        "{{\"cell\":1,\"replace\":\"x\",\"with\":\"{}\"}}",
        "y".repeat(SOURCE_BYTE_CAP)
    );
    assert!(failure.apply(&json).is_err());
    let failure =
        SyntaxFailure::new(1, &format!("x{}", "a".repeat(SOURCE_BYTE_CAP - 1)), 1, 0).unwrap();
    assert!(
        failure
            .apply(r#"{"cell":1,"replace":"x","with":"bb"}"#)
            .is_err()
    );
}

#[test]
fn only_parser_failure_is_eligible_and_any_next_run_consumes_it() {
    let fixture = Fixture::new();
    let mut runtime = fixture.runtime();

    runtime.run_cell("const = ;");
    let failure = runtime.syntax_failure().expect("parse failure is eligible");
    assert_eq!(failure.cell, 1);
    assert!(failure.hint().contains("Nothing in cell 1 ran"));

    let thrown = runtime.run_cell("globalThis.touched = 1; throw new SyntaxError('runtime');");
    assert!(
        matches!(thrown, sterna::runtime::outcome::CellOutcome::Threw { ref error, .. } if error.class == "SyntaxError")
    );
    assert!(runtime.syntax_failure().is_none());

    runtime.run_cell("const = ;");
    assert_eq!(runtime.syntax_failure().unwrap().cell, 3);
    runtime.run_cell("const ok = 1;");
    assert!(runtime.syntax_failure().is_none());
}

#[test]
fn a_corrected_parse_failure_becomes_the_new_target_and_task_end_clears_it() {
    let fixture = Fixture::new();
    let mut runtime = fixture.runtime();

    runtime.run_cell("const = ;");
    let amended = runtime
        .syntax_failure()
        .unwrap()
        .apply(r#"{"cell":1,"replace":"const = ;","with":"let = ;"}"#)
        .unwrap();
    runtime.run_cell(&amended);
    assert_eq!(runtime.syntax_failure().unwrap().cell, 2);

    runtime.end_task();
    assert!(runtime.syntax_failure().is_none());
}

#[test]
fn overlapping_matches_are_ambiguous_and_replacements_are_literal_unicode() {
    let failure = SyntaxFailure::new(1, "界界界", 1, 0).unwrap();
    assert!(
        failure
            .apply(r#"{"cell":1,"replace":"界界","with":"x"}"#)
            .is_err()
    );
    let failure = SyntaxFailure::new(1, "const 世界 = 'bad;", 1, 0).unwrap();
    assert_eq!(
        failure
            .apply(r#"{"cell":1,"replace":"'bad;","with":"'$&\\n';"}"#)
            .unwrap(),
        "const 世界 = '$&\\n';"
    );
}

#[test]
fn the_hint_quotes_the_failing_line_with_a_caret_and_numbers_from_the_error() {
    let source = "const a = 1;\nconst b = 2;\nconst bad = \"unterminated\nconst c = 3;\nconst d = 4;\nconst e = 5;\n";
    // Line 3, column 12: the opening quote of the string that never closes.
    let failure = SyntaxFailure::new(4, source, 3, 12).unwrap();
    let hint = failure.hint();
    assert!(
        hint.contains("const bad = \"unterminated"),
        "the failing line is not quoted:\n{hint}"
    );
    assert!(
        hint.contains("3 | const bad"),
        "the gutter does not carry the error's own line number:\n{hint}"
    );
    assert!(hint.contains('^'), "no caret marks the column:\n{hint}");
    assert!(
        hint.contains("const a = 1;") && hint.contains("const c = 3;"),
        "the surrounding lines are missing:\n{hint}"
    );
    assert!(
        !hint.contains("const e = 5;"),
        "the excerpt is unbounded:\n{hint}"
    );
    assert!(
        hint.contains("```sterna-edit"),
        "the repair fence was lost:\n{hint}"
    );
    // The caret sits under the column the `## Error` block names, counting
    // characters from the start of the line exactly as `line_and_column` does.
    let caret = hint
        .lines()
        .find(|line| line.contains('^'))
        .expect("a caret line");
    let gutter = caret.find('|').expect("a gutter") + 2;
    assert_eq!(
        caret[gutter..].chars().take_while(|c| *c == ' ').count(),
        12,
        "the caret is not at column 12:\n{hint}"
    );
}

#[test]
fn a_position_the_source_does_not_have_quotes_nothing_and_still_offers_the_fence() {
    for (line, column) in [(0, 0), (99, 3)] {
        let failure = SyntaxFailure::new(2, "const a = 1;\n", line, column).unwrap();
        let hint = failure.hint();
        assert!(
            !hint.contains('^'),
            "a caret was drawn for line {line}:\n{hint}"
        );
        assert!(
            hint.contains("Nothing in cell 2 ran.") && hint.contains("```sterna-edit"),
            "the fence was lost for line {line}:\n{hint}"
        );
    }
    // A line longer than the quoted width is clipped rather than reprinted.
    let long = format!("const x = \"{}\";\n", "a".repeat(400));
    let hint = SyntaxFailure::new(1, &long, 1, 5).unwrap().hint();
    assert!(hint.contains('…'), "a long line was not clipped:\n{hint}");
    assert!(
        hint.lines().all(|line| line.chars().count() < 260),
        "a clipped line is still too wide:\n{hint}"
    );
}

#[test]
fn a_real_parse_failure_hands_back_the_line_the_model_wrote() {
    let fixture = Fixture::new();
    let mut runtime = fixture.runtime();

    runtime.run_cell(
        "const one = 1;\nconst two = 2;\nconst broken = \"no closing quote\nconst four = 4;\n",
    );
    let hint = runtime
        .syntax_failure()
        .expect("parse failure is eligible")
        .hint();
    assert!(
        hint.contains("const broken = \"no closing quote"),
        "the model cannot see the line it wrote:\n{hint}"
    );
    assert!(
        hint.contains("```sterna-edit"),
        "the repair fence was lost:\n{hint}"
    );
}
