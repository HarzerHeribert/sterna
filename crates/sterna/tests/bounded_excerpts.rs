use serde_json::Value;
use std::sync::atomic::{AtomicU64, Ordering};
use sterna::contract::SessionId;
use sterna::runtime::isolate::Runtime;
use sterna::runtime::outcome::CellOutcome;
use sterna::runtime::preview::{self, STDOUT_TOKEN_CAP};
use sterna::sandbox::profile::Profile;

static NEXT: AtomicU64 = AtomicU64::new(0);

struct Fixture {
    root: std::path::PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "sterna-bounded-excerpt-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&root).unwrap();
        Self { root }
    }

    fn runtime(&self) -> Runtime {
        let profile = Profile::compile(&self.root, Some(r#"{"permissions":{"allow":["Read"]}}"#));
        Runtime::new(&profile, &SessionId::new("bounded-excerpt-test"))
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn json_from(outcome: &CellOutcome) -> Value {
    serde_json::from_str(outcome.turn().stdout_tail.trim()).unwrap_or_else(|error| {
        panic!(
            "console output was not one JSON value: {error}: {:?}",
            outcome.turn().stdout_tail
        )
    })
}

#[test]
fn excerpts_cover_start_middle_end_and_name_the_next_call() {
    let fixture = Fixture::new();
    let contents = (1..=75)
        .map(|line| format!("row {line} — λ"))
        .collect::<Vec<_>>()
        .join("\n");
    std::fs::write(fixture.root.join("source.rs"), contents).unwrap();
    let mut runtime = fixture.runtime();

    for (start, lines, expected_end, expected_next) in [
        (1, 4, 4, Some(5)),
        (31, 3, 33, Some(34)),
        (73, 20, 75, None),
    ] {
        let outcome = runtime.run_cell(&format!(
            "const file = await read({{path: 'source.rs'}});\n\
             console.log(JSON.stringify(file.excerpt({{start: {start}, lines: {lines}}})));\n"
        ));
        let value = json_from(&outcome);
        assert_eq!(value["start"], start);
        assert_eq!(value["end"], expected_end);
        assert_eq!(
            value["next"],
            expected_next.map_or(Value::Null, Value::from)
        );
        let text = value["text"].as_str().unwrap();
        assert!(
            text.starts_with(&format!("[lines {start}-{expected_end} of 75]\n")),
            "{text}"
        );
        match expected_next {
            Some(next) => assert!(
                text.contains(&format!(
                    "[next: call .excerpt({{start: {next}, lines: {lines}}}) on this File]"
                )),
                "{text}"
            ),
            None => assert!(text.ends_with("[end of file]\n"), "{text}"),
        }
        assert!(text.contains('λ'));
        assert!(text.chars().count() <= 24 * 1024);
    }
}

#[test]
fn a_350_line_modest_source_fits_the_default_page() {
    let fixture = Fixture::new();
    let contents = (1..=350)
        .map(|line| format!("const value_{line} = {line}; // bounded source line λ"))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        (10_000..=20_000).contains(&contents.len()),
        "{}",
        contents.len()
    );
    std::fs::write(fixture.root.join("modest.ts"), contents).unwrap();
    let mut runtime = fixture.runtime();
    let outcome = runtime.run_cell(
        "const file = await read({path: 'modest.ts'});\n\
         console.log(file.excerpt().text);\n",
    );
    let shown = &outcome.turn().stdout_tail;
    assert!(shown.starts_with("[lines 1-350 of 350]\n"), "{shown}");
    assert!(shown.contains("1 | const value_1 = 1;"), "{shown}");
    assert!(shown.contains("350 | const value_350 = 350;"), "{shown}");
    assert!(shown.contains("[end of file]"), "{shown}");
    assert!(!shown.contains("tokens omitted"), "{shown}");
    assert_eq!(outcome.turn().stdout_dropped_tokens, 0);
}

#[test]
fn several_ordinary_outputs_are_retained_together() {
    let fixture = Fixture::new();
    let mut runtime = fixture.runtime();
    let outcome = runtime.run_cell(
        "console.log('FIRST-BEGIN|' + 'a'.repeat(7000) + '|FIRST-END');\n\
         console.log('SECOND-BEGIN|' + '界'.repeat(7000) + '|SECOND-END');\n\
         console.log('THIRD-SUMMARY');\n",
    );
    let shown = &outcome.turn().stdout_tail;
    for marker in [
        "FIRST-BEGIN|",
        "|FIRST-END",
        "SECOND-BEGIN|",
        "|SECOND-END",
        "THIRD-SUMMARY",
    ] {
        assert!(shown.contains(marker), "missing {marker}: {shown}");
    }
    assert_eq!(outcome.turn().stdout_dropped_tokens, 0);
    assert!(preview::estimate_tokens(shown) <= STDOUT_TOKEN_CAP);
}

#[test]
fn continuation_is_monotonic_and_the_line_request_is_capped() {
    let fixture = Fixture::new();
    let contents = (1..=450)
        .map(|n| format!("line {n} {}", "payload ".repeat(12)))
        .collect::<Vec<_>>()
        .join("\n");
    std::fs::write(fixture.root.join("many.txt"), contents).unwrap();
    let mut runtime = fixture.runtime();
    let outcome = runtime.run_cell(
        "const file = await read({path: 'many.txt'});\n\
         const seen = []; let cursor = 1;\n\
         while (cursor !== null) {\n\
           const part = file.excerpt({start: cursor, lines: 10000});\n\
           if (part.truncatedLines !== 0) throw new Error('ordinary line split between pages');\n\
           seen.push([part.start, part.end, part.next, part.text.length]);\n\
           if (part.next !== null && part.next <= cursor) throw new Error('non-monotonic');\n\
           cursor = part.next;\n\
         }\n\
         console.log(JSON.stringify(seen));\n",
    );
    let seen = json_from(&outcome);
    let rows = seen.as_array().unwrap();
    assert!(
        rows.len() > 2,
        "the output cap should require continuation: {rows:?}"
    );
    assert_eq!(rows.first().unwrap()[0], 1);
    assert_eq!(rows.last().unwrap()[1], 450);
    for pair in rows.windows(2) {
        assert_eq!(pair[0][2], pair[1][0]);
        assert!(pair[0][2].as_u64().unwrap() > pair[0][0].as_u64().unwrap());
    }
    assert!(rows.iter().all(|row| row[3].as_u64().unwrap() <= 24 * 1024));
}

#[test]
fn a_long_unicode_line_is_bounded_and_names_its_exact_slice_continuation() {
    let fixture = Fixture::new();
    std::fs::write(
        fixture.root.join("long.txt"),
        format!("{}\nafter\n", "🙂λ".repeat(6_000)),
    )
    .unwrap();
    let mut runtime = fixture.runtime();
    let outcome = runtime.run_cell(
        "const file = await read({path: 'long.txt'});\n\
         console.log(file.excerpt({start: 1, lines: 2}).text);\n",
    );
    let text = &outcome.turn().stdout_tail;
    assert!(
        text.chars().count() <= 24 * 1024,
        "{}",
        text.chars().count()
    );
    assert!(
        text.contains("line continues on this File at .lines[0].slice("),
        "{text}"
    );
    assert!(text.contains('🙂') && text.contains('λ'));

    let metadata = runtime.run_cell(
        "const file = await read({path: 'long.txt'});\n\
         const e = file.excerpt({start: 1, lines: 2});\n\
         const match = e.text.match(/\\.lines\\[0\\]\\.slice\\((\\d+), (\\d+)\\)/);\n\
         const start = Number(match[1]), end = Number(match[2]);\n\
         const piece = file.lines[0].slice(start, end);\n\
         const rendered = e.text.split('\\n')[1];\n\
         const shown = rendered.slice('1 | '.length, rendered.indexOf(' … [line continues'));
         console.log(JSON.stringify({next: e.next, truncatedLines: e.truncatedLines, bounded: end > start && end - start <= 1600, immediate: shown.length === start && piece === file.lines[0].slice(shown.length, end) && piece.length > 0}));\n",
    );
    let value = json_from(&metadata);
    assert!(value["truncatedLines"].as_u64().unwrap() >= 1);
    assert_eq!(value["next"], Value::Null);
    assert_eq!(value["bounded"], true);
    assert_eq!(value["immediate"], true);
}

#[test]
fn astral_excerpt_survives_the_canonical_console_call_exactly() {
    let fixture = Fixture::new();
    // The rendered excerpt is well below 1,800 scalars but above 1,800
    // UTF-16 units, which caught the console's former unit-count mismatch.
    let line = format!("mixed:{}:done", "🙂λ".repeat(600));
    std::fs::write(fixture.root.join("astral.txt"), &line).unwrap();
    let mut runtime = fixture.runtime();
    let outcome = runtime.run_cell(
        "const file = await read({path: 'astral.txt'});\n\
         console.log(file.excerpt({start: 1, lines: 1}).text);\n",
    );
    let expected = format!("[lines 1-1 of 1]\n1 | {line}\n[end of file]\n\n");
    assert_eq!(outcome.turn().stdout_tail, expected);
    assert!(outcome.turn().stdout_tail.starts_with("[lines 1-1 of 1]"));
}

#[test]
fn invalid_options_throw_and_out_of_range_is_explicit() {
    let fixture = Fixture::new();
    std::fs::write(fixture.root.join("tiny.txt"), "one\ntwo\n").unwrap();
    let mut runtime = fixture.runtime();
    for options in ["{start: 0}", "{lines: -1}", "{start: 1.5}", "'bad'"] {
        let outcome = runtime.run_cell(&format!(
            "const file = await read({{path: 'tiny.txt'}}); file.excerpt({options});"
        ));
        assert!(
            matches!(outcome, CellOutcome::Threw { ref error, .. } if error.class == "ToolError"),
            "{options}: {outcome:?}"
        );
    }
    let outcome = runtime.run_cell(
        "const file = await read({path: 'tiny.txt'}); console.log(JSON.stringify(file.excerpt({start: 99, lines: 2})));",
    );
    let value = json_from(&outcome);
    assert_eq!(value["start"], 99);
    assert_eq!(value["end"], Value::Null);
    assert_eq!(value["next"], Value::Null);
    assert_eq!(value["text"], "[lines 99-0 of 2]\n[end of file]\n");
}

#[test]
fn excerpt_adds_no_filesystem_authority() {
    let fixture = Fixture::new();
    std::fs::write(fixture.root.join("inside.txt"), "inside\n").unwrap();
    let outside = fixture.root.with_extension("outside");
    std::fs::write(&outside, "secret\n").unwrap();
    let mut runtime = fixture.runtime();
    let outcome = runtime.run_cell(&format!(
        "console.log(typeof globalThis.excerpt);\n\
         const escaped = await read({{path: {}}});\n",
        serde_json::to_string(&outside.to_string_lossy()).unwrap()
    ));
    assert_eq!(outcome.turn().stdout_tail, "undefined\n");
    assert!(
        matches!(outcome, CellOutcome::Threw { ref error, .. } if error.class == "PermissionDenied"),
        "{outcome:?}"
    );
    let _ = std::fs::remove_file(outside);
}

#[test]
fn console_truncation_is_visible_and_keeps_the_true_unicode_tail() {
    let fixture = Fixture::new();
    let mut runtime = fixture.runtime();
    let one = runtime.run_cell("console.log('BEGIN-' + '🙂'.repeat(30000) + '-TRUE-END');\n");
    let shown = &one.turn().stdout_tail;
    assert!(
        shown.contains("UTF-16 units omitted; showing true suffix"),
        "{shown}"
    );
    assert!(shown.ends_with("-TRUE-END\n"), "{shown}");
    assert!(!shown.contains("BEGIN-"), "{shown}");
    assert!(preview::estimate_tokens(shown) <= STDOUT_TOKEN_CAP);

    let many = runtime.run_cell(
        "for (let i = 0; i < 5000; i++) console.log('line-' + i + '-' + '界'.repeat(20));\n",
    );
    let turn = many.turn();
    assert!(turn.stdout_dropped_tokens > 0);
    assert!(
        turn.stdout_tail.starts_with("[console: ~"),
        "{}",
        turn.stdout_tail
    );
    assert!(
        turn.stdout_tail
            .contains("tokens omitted before this true tail")
    );
    assert!(
        turn.stdout_tail.contains("line-4999-"),
        "{}",
        turn.stdout_tail
    );
    assert!(preview::estimate_tokens(&turn.stdout_tail) <= STDOUT_TOKEN_CAP);
}

#[test]
fn structured_readme_helper_and_command_evidence_fit_without_leaf_truncation() {
    let fixture = Fixture::new();
    let readme = (1..=32)
        .map(|line| {
            format!(
                "README-{line:02} | installation and verification evidence {}",
                "r".repeat(28)
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    assert!((1_500..4_000).contains(&readme.len()), "{}", readme.len());
    std::fs::write(fixture.root.join("README.md"), &readme).unwrap();
    let helper = format!("HELPER-BEGIN|{}|HELPER-END", "h".repeat(700));
    let stdout = format!("STDOUT-BEGIN|{}|STDOUT-END", "s".repeat(700));
    let mut runtime = fixture.runtime();

    let outcome = runtime.run_cell(&format!(
        "const readme = await read({{path: 'README.md'}});\n\
         console.log({{readme: readme.excerpt(), helper: {{answer: {helper}}}, command: {{stdout: {stdout}, stderr: ''}}}});\n",
        helper = serde_json::to_string(&helper).unwrap(),
        stdout = serde_json::to_string(&stdout).unwrap(),
    ));
    let shown = &outcome.turn().stdout_tail;
    let value = json_from(&outcome);
    let numbered = readme
        .lines()
        .enumerate()
        .map(|(index, line)| format!("{:>2} | {line}\n", index + 1))
        .collect::<String>();
    let expected_excerpt = format!("[lines 1-32 of 32]\n{numbered}[end of file]\n");
    assert_eq!(value["readme"]["text"], expected_excerpt, "{shown}");
    assert_eq!(value["readme"]["start"], 1);
    assert_eq!(value["readme"]["end"], 32);
    assert_eq!(value["readme"]["lineCount"], 32);
    assert_eq!(value["readme"]["next"], Value::Null);
    assert_eq!(value["readme"]["truncatedLines"], 0);
    assert_eq!(value["helper"]["answer"], helper, "{shown}");
    assert_eq!(value["command"]["stdout"], stdout, "{shown}");
    assert_eq!(value["command"]["stderr"], "", "{shown}");
    assert!(!shown.contains("omitted; showing true suffix"), "{shown}");
    assert_eq!(outcome.turn().stdout_dropped_tokens, 0);
    assert!(preview::estimate_tokens(shown) <= STDOUT_TOKEN_CAP);
}

#[test]
fn hostile_nested_console_values_are_bounded_with_a_true_omission_marker() {
    let fixture = Fixture::new();
    let mut runtime = fixture.runtime();
    let outcome = runtime.run_cell(
        "console.log({payload: 'HOSTILE-BEGIN|' + '🙂'.repeat(40000) + '|HOSTILE-TRUE-END', after: 'unvisited'});\n",
    );
    let shown = &outcome.turn().stdout_tail;

    assert!(shown.contains("omitted; showing true suffix"), "{shown}");
    assert!(shown.contains("HOSTILE-TRUE-END"), "{shown}");
    assert!(!shown.contains("HOSTILE-BEGIN"), "{shown}");
    assert!(
        shown.contains("1 more"),
        "unvisited member was not named: {shown}"
    );
    assert!(shown.chars().count() <= 24 * 1024 + 1, "{}", shown.len());
    assert!(preview::estimate_tokens(shown) <= STDOUT_TOKEN_CAP);
}

#[test]
fn structured_console_inspection_does_not_invoke_getters_or_proxy_traps() {
    let fixture = Fixture::new();
    let mut runtime = fixture.runtime();
    let outcome = runtime.run_cell(
        "(() => {\n\
           let consoleHits = 0;\n\
           const hostileProxy = new Proxy({}, {ownKeys() { consoleHits++; throw new Error('trap'); }});\n\
           const evidence = {get secret() { consoleHits++; throw new Error('getter'); }, hostileProxy};\n\
           console.log(evidence);\n\
           console.log('console-hits=' + consoleHits);\n\
         })();\n",
    );
    let shown = &outcome.turn().stdout_tail;

    assert!(shown.contains("[Getter]"), "{shown}");
    assert!(shown.contains("[Proxy]"), "{shown}");
    assert!(shown.contains("console-hits=0"), "{shown}");
}

#[test]
fn a_key_that_exhausts_the_budget_does_not_descend_into_its_object_value() {
    let fixture = Fixture::new();
    let mut runtime = fixture.runtime();
    let outcome = runtime.run_cell(
        "const nested = {shouldNeverBeInspected: 'value'};\n\
         const evidence = {[('k'.repeat(30000))]: nested};\n\
         console.log(evidence);\n",
    );
    let shown = &outcome.turn().stdout_tail;

    assert!(shown.contains("1 more"), "{shown}");
    assert!(!shown.contains("shouldNeverBeInspected"), "{shown}");
    assert!(preview::estimate_tokens(shown) <= STDOUT_TOKEN_CAP);
}

/// `read("modest.ts")` is how a model most often writes the call; the bare
/// string is the tool's first argument, not a refusal that costs a turn.
#[test]
fn a_bare_string_is_the_first_argument() {
    let fixture = Fixture::new();
    std::fs::write(fixture.root.join("short.txt"), "one line\n").unwrap();
    let mut runtime = fixture.runtime();
    let outcome = runtime.run_cell(
        "const file = await read('short.txt');\n\
         console.log(file.excerpt().text);\n",
    );
    let shown = &outcome.turn().stdout_tail;
    assert!(shown.contains("1 | one line"), "{shown}");
}
