use pane::contract::SessionId;
use pane::runtime::isolate::Runtime;
use pane::runtime::preview::{self, STDOUT_TOKEN_CAP};
use pane::sandbox::profile::Profile;

fn run(source: &str) -> pane::runtime::outcome::CellOutcome {
    let root = std::env::temp_dir();
    let profile = Profile::compile(&root, None);
    let mut runtime = Runtime::new(&profile, &SessionId::new("console-test"));
    runtime.run_cell(source)
}

#[test]
fn console_inspects_nested_data_without_invoking_getters_or_chasing_cycles() {
    let outcome = run("let getterRuns = 0;\n\
         const item = {name: 'roman', nested: {done: false}};\n\
         Object.defineProperty(item, 'danger', {enumerable: true, get() { getterRuns++; answer('ran'); }});\n\
         item.self = item;\n\
         console.log([item]);\n\
         console.log('getter runs after inspection:', getterRuns);\n");
    let output = &outcome.turn().stdout_tail;
    assert!(output.contains("\"name\": \"roman\""), "{output}");
    assert!(output.contains("\"nested\": {\"done\": false}"), "{output}");
    assert!(output.contains("\"danger\": [Getter]"), "{output}");
    assert!(output.contains("\"self\": [Circular]"), "{output}");
    assert!(
        output.contains("getter runs after inspection: 0"),
        "{output}"
    );
    assert!(!output.contains("[object Object]"), "{output}");
}

#[test]
fn console_structured_output_remains_bounded() {
    let outcome = run(
        "const many = Array.from({length: 5000}, (_, i) => ({i, text: 'x'.repeat(1000)}));\n\
         console.log(many);\n",
    );
    let turn = outcome.turn();
    assert!(
        preview::estimate_tokens(&turn.stdout_tail) <= STDOUT_TOKEN_CAP,
        "console tail was not capped: {} tokens",
        preview::estimate_tokens(&turn.stdout_tail)
    );
    assert!(turn.stdout_tail.contains("more") || turn.stdout_dropped_tokens > 0);
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn read_reports_the_admitted_files_actual_mtime_and_does_not_widen_the_root() {
    use pane::runtime::outcome::CellOutcome;
    use std::time::{SystemTime, UNIX_EPOCH};

    let stem = format!(
        "pane-mtime-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    );
    let root = std::env::temp_dir().join(&stem);
    let outside = std::env::temp_dir().join(format!("{stem}-outside"));
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("observed.txt"), "content").unwrap();
    std::fs::write(&outside, "secret").unwrap();
    let modified = std::fs::metadata(root.join("observed.txt"))
        .unwrap()
        .modified()
        .unwrap()
        .duration_since(UNIX_EPOCH)
        .unwrap();
    let expected = pane::events::Stamp::from_millis(modified.as_millis() as i64).to_string();
    let profile = Profile::compile(&root, Some(r#"{"permissions":{"allow":["Read"]}}"#));
    let mut runtime = Runtime::new(&profile, &SessionId::new("mtime-test"));
    let observed = runtime
        .run_cell("const file = await read({path: 'observed.txt'}); console.log(file.mtime);\n");
    assert_eq!(observed.turn().stdout_tail.trim(), expected);

    let denied = runtime.run_cell(&format!(
        "const escaped = await read({{path: {}}});\n",
        serde_json::to_string(&outside.to_string_lossy()).unwrap()
    ));
    assert!(
        matches!(denied, CellOutcome::Threw { ref error, .. } if error.class == "PermissionDenied"),
        "{denied:?}"
    );
    let _ = std::fs::remove_dir_all(root);
    let _ = std::fs::remove_file(outside);
}

#[test]
fn event_api_is_empty_but_usable_before_the_first_delivery() {
    let outcome = run(
        "console.log(batch.n, batch.where({kind: 'agent.done'}), batch.rest(), batch.ack([42]));",
    );
    let output = &outcome.turn().stdout_tail;
    assert_eq!(output, "0 [] [] {\"acked\": [], \"unknown\": [42]}\n");
    assert!(!outcome.turn().table.contains("Events.Batch"));
}
