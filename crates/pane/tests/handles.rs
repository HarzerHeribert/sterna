//! Acceptance tests for GH-PANE-61E-HANDLES against
//! `docs/product/pane/runtime-contract.md` §2 and §3.

use pane::runtime::handles::{HandleTable, render_table, render_table_delta};
use pane::runtime::preview::{
    ErrorValue, FileValue, PREVIEW_TOKEN_CAP, StackFrame, TABLE_TOKEN_CAP, TestReportValue, Value,
};

/// The class-C consumer link: a golden file over a table containing one of
/// every §3 type. Compared with `\r\n` normalised to `\n` on both sides so a
/// Windows checkout of the fixture cannot fail this for a reason that has
/// nothing to do with the renderer.
#[test]
fn the_handle_table_renders_byte_for_byte() {
    let mut table = HandleTable::new();
    table.declare(
        "arr",
        Value::array(vec![
            Value::Number(10.0),
            Value::Number(20.0),
            Value::Number(30.0),
            Value::Number(40.0),
            Value::Number(50.0),
        ]),
        1,
    );
    table.declare(
        "file",
        Value::File(FileValue {
            path: "src/lib.rs".into(),
            byte_len: 120,
            line_count: 10,
            mtime: "2026-09-05T00:00:00Z".into(),
            lines: vec!["fn main() {}".into(), "// second line".into()],
        }),
        1,
    );
    table.declare(
        "report",
        Value::TestReport(TestReportValue {
            passed: 3,
            failed: 1,
            skipped: 0,
            failing_names: vec!["test_x".into()],
            log: "SHOULD NEVER APPEAR".into(),
        }),
        1,
    );
    table.declare("str", Value::string("hello world"), 1);
    table.declare("num", Value::Number(42.0), 1);
    table.declare("flag", Value::Boolean(true), 1);
    table.declare("empty", Value::Null, 1);
    table.declare("missing", Value::Undefined, 1);
    table.declare(
        "obj",
        Value::object(vec![
            ("a".to_string(), Value::Number(1.0)),
            ("b".to_string(), Value::string("x")),
        ]),
        1,
    );
    table.declare(
        "err",
        Value::Error(ErrorValue {
            class: "TypeError".into(),
            message: "bad thing".into(),
            stack: vec![StackFrame {
                description: "cell 2, line 3".into(),
            }],
            ..ErrorValue::default()
        }),
        1,
    );

    let rendered = render_table(&table, PREVIEW_TOKEN_CAP, TABLE_TOKEN_CAP);
    let golden = include_str!("fixtures/handle_table.golden").replace("\r\n", "\n");
    assert_eq!(rendered, golden);

    // Every entry was declared this cell, so the first rendering after a
    // task starts is the full inventory byte for byte -- the roadmap's
    // *Observation delta* changes only what a REPEATED rendering shows.
    let (delta, stats) = render_table_delta(&table, PREVIEW_TOKEN_CAP, TABLE_TOKEN_CAP);
    assert_eq!(delta, golden);
    assert!(stats.full_inventory, "{stats:?}");
    assert_eq!(stats.rows_suppressed, 0);
    assert_eq!(stats.rows_rendered, table.len());
}

#[test]
fn a_file_preview_never_contains_the_file_contents() {
    let mut table = HandleTable::new();
    table.declare(
        "f",
        Value::File(FileValue {
            path: "notes.txt".into(),
            byte_len: 1000,
            line_count: 5,
            mtime: "2026-09-05T00:00:00Z".into(),
            lines: vec![
                "line one".into(),
                "line two".into(),
                "SECRET-CONTENTS-line-three".into(),
                "SECRET-CONTENTS-line-four".into(),
                "SECRET-CONTENTS-line-five".into(),
            ],
        }),
        1,
    );

    let rendered = render_table(&table, PREVIEW_TOKEN_CAP, TABLE_TOKEN_CAP);
    assert!(rendered.contains("line one"));
    assert!(rendered.contains("line two"));
    assert!(!rendered.contains("SECRET-CONTENTS"));
}

#[test]
fn a_test_report_preview_never_contains_the_log() {
    let mut table = HandleTable::new();
    table.declare(
        "report",
        Value::TestReport(TestReportValue {
            passed: 2,
            failed: 1,
            skipped: 0,
            failing_names: vec!["test_thing".into()],
            log: "SECRET-LOG-CONTENTS".into(),
        }),
        1,
    );

    let rendered = render_table(&table, PREVIEW_TOKEN_CAP, TABLE_TOKEN_CAP);
    assert!(rendered.contains("test_thing"));
    assert!(!rendered.contains("SECRET-LOG-CONTENTS"));
}

#[test]
fn a_preview_over_the_cap_drops_elements_before_it_cuts_a_string() {
    let long = "x".repeat(50);
    let items: Vec<Value> = (0..8).map(|_| Value::string(&long)).collect();
    let mut table = HandleTable::new();
    table.declare("arr", Value::array(items), 1);

    // A 40-token cap fits two 50-char elements but not four.
    let rendered = render_table(&table, 40, TABLE_TOKEN_CAP);

    assert!(rendered.contains("n=8"));
    assert!(rendered.contains(&format!("[0] \"{long}\"")));
    assert!(rendered.contains(&format!("[7] \"{long}\"")));
    // Dropped by count, not by cutting the string short:
    assert!(!rendered.contains("[1]"));
    assert!(!rendered.contains("[2]"));
    assert!(!rendered.contains("[3]"));
}

#[test]
fn a_table_over_the_cap_drops_renderings_and_frees_nothing() {
    let mut table = HandleTable::new();
    let long = "y".repeat(100);
    for i in 0..5u64 {
        table.declare(format!("h{i}"), Value::string(&long), i);
    }

    let rendered = render_table(&table, PREVIEW_TOKEN_CAP, 60);

    assert!(rendered.contains("older handles not shown; call handles() for the full list"));
    assert!(rendered.contains("h4"));
    assert!(!rendered.contains("h0  string"));

    for i in 0..5u64 {
        assert!(
            table.is_live(&format!("h{i}")),
            "h{i} must still be live after rendering dropped it"
        );
    }
}

#[test]
fn a_handle_is_freed_only_by_redeclaration_free_or_task_end() {
    let mut table = HandleTable::new();

    table.declare("x", Value::Number(1.0), 1);
    assert!(table.is_live("x"));

    table.declare("x", Value::Number(2.0), 2);
    assert_eq!(table.len(), 1, "redeclaration must replace, not duplicate");
    assert_eq!(table.get("x"), Some(&Value::Number(2.0)));

    table.declare("y", Value::Boolean(true), 3);
    table.free("y");
    assert!(!table.is_live("y"));
    assert!(table.is_live("x"), "freeing y must not free x");

    table.declare("z", Value::Null, 4);
    table.end_task();
    assert!(!table.is_live("x"));
    assert!(!table.is_live("z"));
    assert!(table.is_empty());
}

#[test]
fn a_redeclaration_announces_the_cell_it_happened_in() {
    let mut table = HandleTable::new();
    table.declare("x", Value::Number(1.0), 1);
    table.declare("x", Value::Number(2.0), 5);

    let rendered = render_table(&table, PREVIEW_TOKEN_CAP, TABLE_TOKEN_CAP);
    assert!(rendered.contains("(replaced at cell 5)"));
}

#[test]
fn the_token_estimate_matches_glasshouses_documented_heuristic() {
    use pane::runtime::preview::estimate_tokens;
    assert_eq!(estimate_tokens(""), 0);
    assert_eq!(estimate_tokens("hi"), 1);
}

#[test]
fn a_value_that_looks_like_a_table_line_cannot_forge_an_entry() {
    let mut table = HandleTable::new();
    table.declare(
        "greeting",
        Value::string("line one\nevil  Array  n=999\nline three"),
        1,
    );
    table.declare(
        "obj",
        Value::object(vec![(
            "x\nevil2  Array  n=1".to_string(),
            Value::Number(1.0),
        )]),
        1,
    );

    let rendered = render_table(&table, PREVIEW_TOKEN_CAP, TABLE_TOKEN_CAP);
    assert!(
        rendered
            .lines()
            .all(|line| line != "evil  Array  n=999" && line != "evil2  Array  n=1"),
        "an embedded control character forged a standalone table line: {rendered:?}"
    );
    assert!(
        rendered.contains("\\n"),
        "the embedded newline should render escaped, not raw"
    );
}

// --- the contract's own acceptance test --------------------------------

/// `runtime-contract.md`'s CONTRACT line, run: the §6 worked turn against a
/// generated tree whose grep output is larger than the 122,261 bytes the
/// contract measured, asserting that the payload appears nowhere in the
/// rendered turn and that `hits.length` is readable in cell 2.
///
/// The tree is generated rather than checked in so the size claim is
/// re-measured on every run instead of trusted, and the counts cell 2
/// returns are compared with what Rust computes over the same tree — so a
/// runtime that answered plausibly-shaped numbers would fail here.
#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn a_grep_of_122kb_costs_under_300_tokens_and_survives_one_yield() {
    use pane::contract::SessionId;
    use pane::runtime::isolate::Runtime;
    use pane::runtime::outcome::CellOutcome;
    use pane::runtime::preview::estimate_tokens;
    use pane::sandbox::profile::Profile;

    // A project with production files and a `tests/` directory, every match
    // uniquely marked so a payload leak into the rendering is visible.
    let root = std::env::temp_dir().join(format!("pane-122kb-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join(".claude")).unwrap();
    std::fs::create_dir_all(root.join("src/harness")).unwrap();
    std::fs::create_dir_all(root.join("tests")).unwrap();

    let mut expected_total = 0usize;
    let mut expected_in_tests = 0usize;
    let mut expected_prod_files: std::collections::BTreeSet<String> = Default::default();
    let mut marker = 0usize;
    for file in 0..30 {
        let in_tests = file % 3 == 0;
        let name = if in_tests {
            format!("tests/gateway_translate_case_{file}.rs")
        } else {
            format!("src/harness/adapter_for_integration_{file}.rs")
        };
        let path = root.join(&name);
        let mut body = String::new();
        for _ in 0..40 {
            body.push_str(&format!(
                "    let profile = LaunchProfile::native(IntegrationId::ClaudeCode); // unique-marker-{marker} padding padding padding padding\n"
            ));
            marker += 1;
            expected_total += 1;
            if in_tests {
                expected_in_tests += 1;
            }
        }
        if !in_tests {
            expected_prod_files.insert(path.to_string_lossy().to_string());
        }
        std::fs::write(&path, body).unwrap();
    }

    // The adapter file §6 reads: big, and its first two lines are the only
    // ones a preview may ever show.
    let adapter = root.join("src/harness/mod.rs");
    let mut adapter_body =
        String::from("//! The contract every supported harness is reached through.\n//!\n");
    for i in 0..1500 {
        adapter_body.push_str(&format!(
            "// SECRET-BODY-LINE {i} padding padding padding\n"
        ));
    }
    std::fs::write(&adapter, &adapter_body).unwrap();

    let profile = Profile::compile(&root, Some(r#"{"permissions":{"allow":[]}}"#));
    let session = SessionId::new("acceptance-122kb");
    let mut runtime = Runtime::new(&profile, &session);

    // Cell 1 is §6's own two lines, with this tree's paths.
    let cell_one = format!(
        "const hits = await grep({{ pattern: \"IntegrationId\", path: {root:?} }});\n\
         const adapter = await read({{ path: {adapter:?} }});\n",
        root = root.to_string_lossy(),
        adapter = adapter.to_string_lossy()
    );
    let first = runtime.run_cell(&cell_one);
    let CellOutcome::Yielded { turn } = &first else {
        panic!("cell 1 falls off the end, so it yields: {first:?}");
    };

    // The grep really is larger than the contract's measurement, so the
    // ratio this test claims is a ratio and not a rounding.
    let raw = std::process::Command::new("grep")
        .args(["-r", "-n", "-e", "IntegrationId", "--"])
        .arg(&root)
        .output()
        .expect("grep runs on this host");
    assert!(
        raw.stdout.len() > 122_261,
        "the fixture's grep output is only {} bytes, so it does not test the claim",
        raw.stdout.len()
    );

    // What the model is shown. The name field is padded to the widest live
    // name in the table (`handles.rs`'s `render_entry`), so "hits" carries
    // more trailing spaces than "adapter" -- checked by prefix, not by an
    // exact gap width.
    let rendered = &turn.table;
    let hits_line = rendered.lines().next().unwrap_or_default();
    assert!(
        hits_line.starts_with("hits") && hits_line.contains("Grep.Match[]"),
        "the handle carries its declared type: {rendered}"
    );
    assert!(rendered.contains("adapter  File"), "{rendered}");
    assert!(
        rendered.contains(&format!("n={expected_total}")),
        "the preview states the length it never showed: {rendered}"
    );
    println!(
        "122KB acceptance: {} bytes of grep output rendered as {} tokens",
        raw.stdout.len(),
        estimate_tokens(rendered)
    );
    assert!(
        estimate_tokens(rendered) < 300,
        "the rendered turn cost {} tokens for {} bytes of grep output",
        estimate_tokens(rendered),
        raw.stdout.len()
    );

    // Nothing beyond [0] [1] [2] and [last] is in the rendering, and none of
    // the file's own body is.
    let shown: Vec<&str> = rendered
        .lines()
        .filter(|line| line.trim_start().starts_with('['))
        .collect();
    assert_eq!(shown.len(), 4, "{rendered}");
    for middle in [100usize, 600, 900] {
        assert!(
            !rendered.contains(&format!("unique-marker-{middle} ")),
            "a hit the preview does not index reached the rendering: {rendered}"
        );
    }
    assert!(
        !rendered.contains("SECRET-BODY-LINE"),
        "the file's contents reached the preview: {rendered}"
    );
    assert!(
        rendered.contains("The contract every supported harness is reached through"),
        "the File preview shows its first line: {rendered}"
    );

    // Cell 2: the handle survived the yield, and the program computes over
    // the payload the model was never shown.
    let cell_two = format!(
        "const isTest = (m) => m.path.startsWith({tests:?});\n\
         const inTests = hits.filter(isTest);\n\
         const prodFiles = new Set(hits.filter(m => !isTest(m)).map(m => m.path));\n\
         return {{ total: hits.length, in_tests: inTests.length, prod_files: prodFiles.size }};\n",
        // Hit paths are root-relative (`bindings/search.rs`).
        tests = "tests/"
    );
    let second = runtime.run_cell(&cell_two);
    assert!(
        !second.ends_the_task(),
        "structured output continues the task"
    );
    let CellOutcome::Returned { value, .. } = &second else {
        panic!("expected a return: {second:?}");
    };
    let Value::Object(object) = value else {
        panic!("expected an object: {value:?}");
    };
    let number = |key: &str| -> f64 {
        match object.entries().iter().find(|(name, _)| name == key) {
            Some((_, Value::Number(n))) => *n,
            other => panic!("{key} is not a number: {other:?}"),
        }
    };
    assert_eq!(number("total"), expected_total as f64);
    assert_eq!(number("in_tests"), expected_in_tests as f64);
    assert_eq!(number("prod_files"), expected_prod_files.len() as f64);

    let _ = std::fs::remove_dir_all(&root);
}

// --- GH-PANE-61G-DELIVERY-AND-BG: §4's batch row ------------------------

use pane::events::batch::Batch;
use pane::events::window::{Window, WindowConfig};
use pane::events::{Event, Kind, PayloadRef, Priority, Stamp};
use pane::runtime::handles::HandleMeta;

/// One closed batch, built the way the session builds one — through a window
/// — so the entry this table renders is the batch's own
/// `events-contract.md` §3 preview and not a string a test wrote.
fn closed_batch(source: &str) -> Batch {
    let mut window = Window::new(WindowConfig::default());
    window.accept(
        Event::pending(
            Kind::BgDone {
                emission: "exit".into(),
            },
            format!("bg/{source}"),
            Stamp::from_millis(0),
            PayloadRef::new(format!("{source}#exit")),
            Priority::Batch,
            "the build finished",
        ),
        Stamp::from_millis(0),
    );
    window
        .close_if_due(Stamp::from_millis(3_000))
        .expect("a window whose deadline has passed closes")
}

fn batch_meta(entry: &str) -> HandleMeta {
    HandleMeta {
        type_label: Some("Events.Batch".to_string()),
        size_estimate: entry.len() as u64,
        provenance: None,
    }
}

/// `events-contract.md` §4: "exactly one row, named `batch`, **always
/// last**, so the model's own bindings keep the order it made them in" — and
/// the row is the batch's §3 preview, not §3-of-`runtime-contract`'s `string`
/// rule applied to it.
#[test]
fn the_batch_row_is_the_tables_last_and_carries_its_own_preview() {
    let mut table = HandleTable::new();
    table.declare("hits", Value::array(vec![Value::Number(1.0)]), 1);
    table.declare("adapter", Value::string("codex"), 1);
    table.declare("report", Value::Number(3.0), 1);

    let batch = closed_batch("job1");
    let entry = batch.preview(PREVIEW_TOKEN_CAP);
    table.declare_rendered(
        "batch",
        Value::string(&entry),
        1,
        batch_meta(&entry),
        entry.clone(),
    );

    assert_eq!(table.names(), vec!["hits", "adapter", "report", "batch"]);
    let rendered = render_table(&table, PREVIEW_TOKEN_CAP, TABLE_TOKEN_CAP);
    let batch_at = rendered
        .find("Events.Batch")
        .expect("the batch row is in the table");
    for name in ["hits", "adapter", "report"] {
        let at = rendered
            .find(name)
            .unwrap_or_else(|| panic!("`{name}` is not in the table: {rendered}"));
        assert!(
            at < batch_at,
            "`{name}` was rendered after the batch row, so the batch is not last:\n{rendered}"
        );
    }
    assert!(
        rendered.trim_end().ends_with(entry.trim_end()),
        "the table does not end with the batch's own preview:\n{rendered}"
    );
    // Its own §3 preview, verbatim -- not `len=` and the first 200 characters
    // of it, which is what `runtime-contract.md` §3's `string` rule would
    // have produced. (The `adapter` row above *is* a string and does carry
    // `len=`, which is why this asks the batch's own entry and not the whole
    // table.)
    let batch_entry = &rendered[batch_at..];
    assert!(batch_entry.contains("the build finished"), "{batch_entry}");
    assert!(batch_entry.contains("bg.done"), "{batch_entry}");
    assert!(
        !batch_entry.contains("len="),
        "the batch was rendered by the `string` type rule rather than by its own \
         events-contract.md §3 preview:\n{batch_entry}"
    );
}

/// `runtime-contract.md` §2 through §4's "each delivery replaces and frees
/// the previous batch, rendered `batch  (replaced at cell 5)`".
#[test]
fn a_second_batch_replaces_and_frees_the_first() {
    let mut table = HandleTable::new();
    let first = closed_batch("job1").preview(PREVIEW_TOKEN_CAP);
    table.declare_rendered("batch", Value::string(&first), 1, batch_meta(&first), first);
    let second = closed_batch("job2").preview(PREVIEW_TOKEN_CAP);
    table.declare_rendered(
        "batch",
        Value::string(&second),
        5,
        batch_meta(&second),
        second,
    );

    assert_eq!(
        table.names(),
        vec!["batch"],
        "the first batch was not freed; two live handles now answer to one name"
    );
    let rendered = render_table(&table, PREVIEW_TOKEN_CAP, TABLE_TOKEN_CAP);
    assert_eq!(
        rendered.matches("Events.Batch").count(),
        1,
        "two batch rows were rendered:\n{rendered}"
    );
    assert!(
        rendered.contains("(replaced at cell 5)"),
        "the replacement was not announced:\n{rendered}"
    );
    assert!(rendered.contains("bg/job2"), "{rendered}");
    assert!(!rendered.contains("bg/job1"), "{rendered}");
}
