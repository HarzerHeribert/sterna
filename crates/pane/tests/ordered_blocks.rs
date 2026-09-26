use pane::contract::SessionId;
use pane::prompt::{
    COMPLETE_MARKER, Extracted, MAX_PANE_BLOCKS, MAX_PROGRAM_BYTES, completion_text,
    extract_program,
};
use pane::runtime::cell;
use pane::runtime::isolate::Runtime;
use pane::runtime::outcome::CellOutcome;
use pane::runtime::preview::Value;
use pane::sandbox::profile::Profile;

#[test]
fn complete_pane_blocks_compose_in_message_order_with_a_safe_boundary() {
    let extracted = extract_program(
        "first\n```pane\nconst value = 40 // trailing comment\n```\nmiddle\n```pane\nreturn value + 2;\n```",
    );
    assert_eq!(
        extracted,
        Extracted::Program("const value = 40 // trailing comment\n;\nreturn value + 2;".into())
    );
}

#[test]
fn the_whole_composition_is_preflighted_and_keeps_cross_block_references() {
    let Extracted::Program(source) = extract_program(
        "```pane\nconst earlier = 41;\n```\n```pane\nconst later = earlier + 1;\nreturn later;\n```",
    ) else {
        panic!("expected a program");
    };
    let compiled = cell::compile(&source, 1).expect("the combined cell compiles");
    assert_eq!(compiled.declared, vec!["earlier", "later"]);

    let Extracted::Program(invalid) =
        extract_program("```pane\nconst wouldWrite = true;\n```\n```pane\nconst = ;\n```")
    else {
        panic!("expected one combined program");
    };
    assert!(cell::compile(&invalid, 1).is_err());
}

#[test]
fn duplicate_top_level_bindings_are_preserved_for_the_cell_transform() {
    let Extracted::Program(source) = extract_program(
        "```pane\nconst tally = 1;\n```\n```pane\nconst tally = 2;\nreturn tally;\n```",
    ) else {
        panic!("expected one combined program");
    };
    let compiled = cell::compile(&source, 1).expect("REPL-style declarations compile");
    // The capture list is one handle per name; the source still contains both
    // declarations and its existing const-to-var rewrite makes the latter win.
    assert_eq!(compiled.declared, vec!["tally"]);
    assert_eq!(source.matches("const tally").count(), 2);

    let root = std::env::temp_dir().join(format!("pane-ordered-duplicate-{}", std::process::id()));
    std::fs::create_dir_all(root.join(".claude")).unwrap();
    let profile = Profile::compile(&root, None);
    let session = SessionId::new("ordered-duplicate");
    let mut runtime = Runtime::new(&profile, &session);
    assert!(matches!(
        runtime.run_cell(&source),
        CellOutcome::Returned {
            value: Value::Number(2.0),
            ..
        }
    ));
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn ordered_blocks_execute_as_one_cell_and_stop_at_return_or_throw() {
    let root = std::env::temp_dir().join(format!("pane-ordered-blocks-{}", std::process::id()));
    std::fs::create_dir_all(root.join(".claude")).unwrap();
    let profile = Profile::compile(&root, None);
    let session = SessionId::new("ordered-blocks");

    let mut runtime = Runtime::new(&profile, &session);
    let Extracted::Program(source) = extract_program(
        "```pane\nconst base = 40;\n```\n```pane\nreturn base + 2;\n```\n```pane\nthrow new Error('must not run');\n```",
    ) else {
        panic!("expected one combined program");
    };
    let returned = runtime.run_cell(&source);
    assert!(matches!(
        returned,
        CellOutcome::Returned {
            value: Value::Number(42.0),
            ..
        }
    ));

    let mut runtime = Runtime::new(&profile, &session);
    let Extracted::Program(source) = extract_program(
        "```pane\nconst reached = 1;\n```\n```pane\nthrow new Error('stop');\n```\n```pane\nconst skipped = 2;\n```",
    ) else {
        panic!("expected one combined program");
    };
    let threw = runtime.run_cell(&source);
    assert!(matches!(threw, CellOutcome::Threw { .. }));
    assert!(runtime.is_live("reached"));
    assert!(!runtime.is_live("skipped"));

    let _ = std::fs::remove_dir_all(root);
}

#[cfg(unix)]
#[test]
fn preflight_return_and_throw_prevent_later_admitted_writes() {
    let root = std::env::temp_dir().join(format!("pane-ordered-effects-{}", std::process::id()));
    std::fs::create_dir_all(root.join(".claude")).unwrap();
    let profile = Profile::compile(&root, Some(r#"{"permissions":{"allow":["Bash(echo*)"]}}"#));
    let session = SessionId::new("ordered-effects");

    // Prove the exact capability used by the negative cases is admitted and
    // can make an observable write in this fixture.
    let mut runtime = Runtime::new(&profile, &session);
    let control =
        runtime.run_cell("const control = await bash({command: \"echo yes > control-marker\"});\n");
    assert!(root.join("control-marker").exists(), "{control:?}");

    let mut runtime = Runtime::new(&profile, &session);
    let Extracted::Program(source) = extract_program(
        "```pane\nconst later = await bash({command: \"echo bad > syntax-marker\"});\n```\n```pane\nconst = ;\n```",
    ) else {
        panic!("expected one combined program");
    };
    let syntax = runtime.run_cell(&source);
    assert!(matches!(syntax, CellOutcome::Threw { .. }), "{syntax:?}");
    assert!(!root.join("syntax-marker").exists());
    assert!(syntax.turn().record.calls.is_empty(), "{syntax:?}");

    let mut runtime = Runtime::new(&profile, &session);
    let Extracted::Program(source) = extract_program(
        "```pane\nreturn 7;\n```\n```pane\nconst later = await bash({command: \"echo bad > return-marker\"});\n```",
    ) else {
        panic!("expected one combined program");
    };
    let returned = runtime.run_cell(&source);
    assert!(matches!(returned, CellOutcome::Returned { .. }));
    assert!(!root.join("return-marker").exists());
    assert!(returned.turn().record.calls.is_empty(), "{returned:?}");

    let mut runtime = Runtime::new(&profile, &session);
    let Extracted::Program(source) = extract_program(
        "```pane\nthrow new Error('stop');\n```\n```pane\nconst later = await bash({command: \"echo bad > throw-marker\"});\n```",
    ) else {
        panic!("expected one combined program");
    };
    let threw = runtime.run_cell(&source);
    assert!(matches!(threw, CellOutcome::Threw { .. }));
    assert!(!root.join("throw-marker").exists());
    assert!(threw.turn().record.calls.is_empty(), "{threw:?}");

    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn edits_are_single_and_exclusive() {
    assert!(matches!(
        extract_program("```pane-edit\n{}\n```\n```pane\nreturn 1;\n```"),
        Extracted::TwoBlocks
    ));
    assert!(matches!(
        extract_program("```pane-edit\n{}\n```\n```pane-edit\n{}\n```"),
        Extracted::TwoBlocks
    ));
    assert_eq!(
        extract_program("```pane-edit\n{\"cell\":1}\n```"),
        Extracted::Edit("{\"cell\":1}".into())
    );
}

#[test]
fn incomplete_and_malformed_attempts_never_expose_an_executable_prefix() {
    for text in [
        "```pane\nconst partial = true;",
        "```pane-edit\n{}",
        "```pane extra\nreturn 1;\n```",
        "<php-pane>return 1;</php-pane>",
    ] {
        assert!(
            matches!(extract_program(text), Extracted::Invalid(_)),
            "{text}"
        );
    }
    assert_eq!(
        extract_program("```ts\nconst example = true;\n```\n```bash\necho example\n```"),
        Extracted::Prose
    );
    assert_eq!(
        extract_program("```html\n<php-pane>an example</php-pane>\n```"),
        Extracted::Prose
    );
}

#[test]
fn block_count_and_combined_source_are_bounded() {
    let too_many = "```pane\n;\n```\n".repeat(MAX_PANE_BLOCKS + 1);
    assert!(matches!(extract_program(&too_many), Extracted::Invalid(_)));

    let too_large = format!("```pane\n{}\n```", "x".repeat(MAX_PROGRAM_BYTES + 1));
    assert!(matches!(extract_program(&too_large), Extracted::Invalid(_)));

    let first = "x".repeat(MAX_PROGRAM_BYTES - "\n;\n".len() - 1);
    let exactly = format!("```pane\n{first}\n```\n```pane\ny\n```");
    assert!(matches!(extract_program(&exactly), Extracted::Program(_)));
    let over_with_separator = format!("```pane\n{first}\n```\n```pane\nyy\n```");
    assert!(matches!(
        extract_program(&over_with_separator),
        Extracted::Invalid(_)
    ));

    let oversized_edit = format!("```pane-edit\n{}\n```", "x".repeat(MAX_PROGRAM_BYTES + 1));
    assert!(matches!(
        extract_program(&oversized_edit),
        Extracted::Invalid(_)
    ));
}

#[test]
fn natural_prose_completes_and_the_legacy_marker_is_still_hidden() {
    assert_eq!(
        completion_text(&format!("Here is the answer.\n{COMPLETE_MARKER}")),
        Some("Here is the answer.".into())
    );
    assert_eq!(completion_text(COMPLETE_MARKER), None);
    assert_eq!(completion_text(&format!(" \n{COMPLETE_MARKER}")), None);
    assert_eq!(
        completion_text("ordinary prose"),
        Some("ordinary prose".into())
    );
    assert_eq!(
        completion_text(&format!("{COMPLETE_MARKER}\nmore prose")),
        None
    );
    assert_eq!(
        completion_text(&format!("```md\n{COMPLETE_MARKER}\n```")),
        None
    );
    assert_eq!(
        completion_text(&format!("```pane\nreturn 1;\n```\n{COMPLETE_MARKER}")),
        None
    );
    assert_eq!(
        completion_text(&format!("```pane\nconst partial = 1;\n{COMPLETE_MARKER}")),
        None
    );
}
