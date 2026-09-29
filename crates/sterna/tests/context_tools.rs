use sterna::contract::SessionId;
use sterna::runtime::isolate::Runtime;
use sterna::runtime::outcome::{CellOutcome, Ended};
use sterna::sandbox::profile::Profile;

fn fixture(label: &str) -> std::path::PathBuf {
    let root = std::env::temp_dir().join(format!(
        "sterna-context-tool-{label}-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("src")).unwrap();
    root
}

#[test]
fn context_and_edit_form_a_versioned_visible_edit_loop() {
    let root = fixture("success");
    let path = root.join("src/limits.py");
    std::fs::write(&path, "def clamp(value):\n    return value\n").unwrap();
    let profile = Profile::compile(&root, None);
    let mut runtime = Runtime::new(&profile, &SessionId::new("context-edit"));

    let first = runtime.run_cell(&format!(
        "const ctx = await context({{path:{path:?}, symbol:\"clamp\"}});\nconsole.log(`context-symbol=${{ctx.symbol}}`);"
    ));
    assert!(first.turn().stdout_tail.contains("def clamp(value)"));
    assert!(first.turn().stdout_tail.contains("context-symbol=clamp"));
    assert!(first.turn().stdout_tail.contains("version:"));
    let call = &first.turn().record.calls[0];
    assert_eq!(call.tool, "context");
    assert_eq!(call.ended, Ended::Ok);
    let evidence = call.evidence.as_ref().expect("context records visibility");
    assert!(!first.turn().stdout_tail.contains(&evidence.sha256));
    assert_eq!(evidence.path, "src/limits.py");
    assert!(evidence.complete);
    assert_eq!(evidence.ranges[0].start, 1);
    assert_eq!(evidence.ranges[0].end, 2);

    let second = runtime.run_cell(&format!(
        "const changed = await edit({{path:{path:?}, old:\"    return value\", replacement:\"    return Math.max(0, value)\"}});\nreturn changed.after_sha256;"
    ));
    assert!(matches!(second, CellOutcome::Returned { .. }), "{second:?}");
    assert_eq!(second.turn().record.calls[0].tool, "edit");
    assert_eq!(second.turn().record.calls[0].ended, Ended::Ok);
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "def clamp(value):\n    return Math.max(0, value)\n"
    );
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn context_cannot_be_used_for_a_semantic_edit_before_it_reaches_the_model() {
    let root = fixture("same-cell");
    let path = root.join("src/value.py");
    std::fs::write(&path, "value = 1\n").unwrap();
    let profile = Profile::compile(&root, None);
    let mut runtime = Runtime::new(&profile, &SessionId::new("same-cell"));
    // The rule is unchanged -- a version binds when the model has actually
    // read it, which is the next cell. What changed is that enforcing it
    // costs the call and not the cell: the refusal is the program's to catch,
    // and it names which of the four situations this is instead of telling a
    // caller to do the thing it has just done.
    let marker = root.join("reached.txt");
    let result = runtime.run_cell(&format!(
        "const ctx = await context({{path:{path:?}}});\n\
         try {{\n\
           await edit({{path:{path:?}, old:\"value = 1\", replacement:\"value = 2\"}});\n\
           throw new Error(\"the edit must not have run\");\n\
         }} catch (e) {{\n\
           const m = String(e.message ?? e);\n\
           if (!m.includes(\"in this turn's feedback\")) throw new Error(\"wrong refusal: \" + m);\n\
         }}\n\
         await write({{path:{marker:?}, content:\"reached\"}});"
    ));
    assert!(
        !matches!(result, CellOutcome::Threw { .. }),
        "the refusal is catchable and the cell runs past it: {result:?}"
    );
    assert!(
        marker.exists(),
        "the program kept its control flow after the refusal"
    );
    assert_eq!(result.turn().record.calls[0].tool, "context");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "value = 1\n");
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn an_incomplete_context_binds_the_lines_it_showed_and_refuses_the_rest() {
    let root = fixture("incomplete-binding");
    let path = root.join("src/big.py");
    // One definition past the 24,000-byte target cap: delivered short, so no
    // whole version binds -- but the lines it did show do.
    let body: String = (0..1_200)
        .map(|i| format!("    line_{i} = \"padding padding padding\"\n"))
        .collect();
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, format!("def target():\n{body}")).unwrap();
    let profile = Profile::compile(&root, None);
    let mut runtime = Runtime::new(&profile, &SessionId::new("incomplete"));

    let first = runtime.run_cell(&format!(
        "const ctx = await context({{path:{path:?}, symbol:'target'}});"
    ));
    assert!(
        !matches!(first, CellOutcome::Threw { .. }),
        "an oversized definition is delivered, not thrown: {first:?}"
    );

    // A line past what was delivered: refused, and the refusal says why.
    let marker = root.join("reached.txt");
    let unseen = runtime.run_cell(&format!(
        "try {{\n\
           await edit({{path:{path:?}, old:\"line_1199 =\", replacement:\"line_x =\"}});\n\
           throw new Error(\"the edit must not have run\");\n\
         }} catch (e) {{\n\
           const m = String(e.message ?? e);\n\
           if (!m.includes(\"without its target whole\")) throw new Error(\"wrong refusal: \" + m);\n\
         }}\n\
         await write({{path:{marker:?}, content:\"reached\"}});"
    ));
    assert!(
        !matches!(unseen, CellOutcome::Threw { .. }),
        "the refusal is catchable: {unseen:?}"
    );
    assert!(marker.exists(), "the cell ran past the refusal");
    assert!(
        std::fs::read_to_string(&path)
            .unwrap()
            .contains("line_1199 =")
    );

    // A line it showed: bound line by line, and marked so in the record.
    let seen = runtime.run_cell(&format!(
        "await edit({{path:{path:?}, old:\"line_0 =\", replacement:\"line_zero =\"}});"
    ));
    assert!(!matches!(seen, CellOutcome::Threw { .. }), "{seen:?}");
    let call = &seen.turn().record.calls[0];
    assert_eq!(
        call.args.get("bound").map(String::as_str),
        Some("seen lines")
    );
    assert!(
        std::fs::read_to_string(&path)
            .unwrap()
            .contains("line_zero =")
    );
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn a_seen_line_is_still_editable_after_something_else_changed_the_file() {
    let root = fixture("moved-on");
    let path = root.join("src/value.py");
    std::fs::write(&path, "value = 1\nother = 1\n").unwrap();
    let profile = Profile::compile(&root, None);
    let mut runtime = Runtime::new(&profile, &SessionId::new("moved-on"));
    runtime.run_cell(&format!("await context({{path:{path:?}}});"));
    // A formatter, a second tool: the other line moves on, this one does not.
    std::fs::write(&path, "value = 1\nother = 2\n").unwrap();

    // The changed line was never seen in its new form: refused.
    let stale = runtime.run_cell(&format!(
        "await edit({{path:{path:?}, old:\"other = 2\", replacement:\"other = 9\"}});"
    ));
    assert!(matches!(stale, CellOutcome::Threw { .. }), "{stale:?}");
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "value = 1\nother = 2\n"
    );

    // The unchanged line was seen byte for byte: bound line by line.
    let edited = runtime.run_cell(&format!(
        "await edit({{path:{path:?}, old:\"value = 1\", replacement:\"value = 3\"}});"
    ));
    assert!(!matches!(edited, CellOutcome::Threw { .. }), "{edited:?}");
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "value = 3\nother = 2\n"
    );
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn an_edit_from_stale_visible_context_throws_and_writes_nothing() {
    let root = fixture("stale");
    let path = root.join("src/value.py");
    std::fs::write(&path, "value = 1\n").unwrap();
    let profile = Profile::compile(&root, None);
    let mut runtime = Runtime::new(&profile, &SessionId::new("stale-edit"));
    runtime.run_cell(&format!("const ctx = await context({{path:{path:?}}});"));
    std::fs::write(&path, "value = 2\n").unwrap();

    let result = runtime.run_cell(&format!(
        "await edit({{path:{path:?}, expected_sha256:ctx.sha256, old:\"value = 1\", replacement:\"value = 3\"}});"
    ));
    assert!(matches!(result, CellOutcome::Threw { .. }), "{result:?}");
    assert_eq!(
        result.turn().record.calls[0].ended,
        Ended::Threw {
            class: "ToolError".into()
        }
    );
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "value = 2\n");
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn broad_read_of_one_large_stub_is_promoted_to_visible_context() {
    let root = fixture("promoted-read");
    let path = root.join("src/value.py");
    let source = format!(
        "def value():\n    raise NotImplementedError('stub')\n{}",
        "# far padding\n".repeat(2_000)
    );
    std::fs::write(&path, source).unwrap();
    let profile = Profile::compile(&root, None);
    let mut runtime = Runtime::new(&profile, &SessionId::new("promoted-read"));

    let first = runtime.run_cell(&format!(
        "const source = await read({{path:{path:?}}});\nconsole.log(source.text);"
    ));
    assert_eq!(first.turn().record.calls[0].tool, "context");
    assert!(
        first.turn().record.calls[0]
            .evidence
            .as_ref()
            .is_some_and(|evidence| evidence.complete),
        "{first:?}"
    );
    assert!(first.turn().stdout_tail.contains("symbol: value"));
    assert!(!first.turn().stdout_tail.contains("far padding"));

    let second = runtime.run_cell(&format!(
        "await edit({{path:{path:?}, old:\"    raise NotImplementedError('stub')\", replacement:\"    return 1\"}});"
    ));
    assert_eq!(second.turn().record.calls[0].tool, "edit");
    assert!(std::fs::read_to_string(&path).unwrap().contains("return 1"));
    let _ = std::fs::remove_dir_all(root);
}

#[cfg(unix)]
#[test]
fn related_sources_can_be_inspected_together_then_edited_and_verified_together() {
    let root = fixture("batch");
    std::fs::write(root.join("src/a.py"), "value = 1\n").unwrap();
    std::fs::write(root.join("src/b.py"), "value = 2\n").unwrap();
    let profile = Profile::compile(
        &root,
        Some(r#"{"permissions":{"allow":["Read(**)","Write(**)","Bash"]}}"#),
    );
    let mut runtime = Runtime::new(&profile, &SessionId::new("batch"));
    let inspected = runtime
        .run_cell("const a = context({path:'src/a.py'}); const b = context({path:'src/b.py'});");
    assert_eq!(inspected.turn().record.calls.len(), 2, "{inspected:?}");
    assert!(inspected.turn().stdout_tail.contains("value = 1"));
    assert!(inspected.turn().stdout_tail.contains("value = 2"));
    let changed = runtime.run_cell(r#"
        edit({path:'src/a.py', old:'value = 1', replacement:'value = 10'});
        edit({path:'src/b.py', old:'value = 2', replacement:'value = 20'});
        write({path:'check.sh', lines:['test "$(< src/a.py)" = "value = 10" && test "$(< src/b.py)" = "value = 20"']});
        const verified = bash({command:'source check.sh'});
        if (verified.exit_code !== 0) throw new Error("verification failed");
        return verified.exit_code;
    "#);
    assert!(
        matches!(changed, CellOutcome::Returned { .. }),
        "{changed:?}"
    );
    assert_eq!(changed.turn().record.calls.len(), 4, "{changed:?}");
    assert!(
        changed
            .turn()
            .record
            .calls
            .iter()
            .all(|c| c.ended == Ended::Ok),
        "{changed:?}"
    );
    assert_eq!(
        std::fs::read_to_string(root.join("src/a.py")).unwrap(),
        "value = 10\n"
    );
    assert_eq!(
        std::fs::read_to_string(root.join("src/b.py")).unwrap(),
        "value = 20\n"
    );
    let _ = std::fs::remove_dir_all(root);
}

/// The same contract where the command tool is `cmd.exe`: two edits, one
/// verification command, and the verification's own exit code returned.
/// `check.cmd` stands where `check.sh` stood, and its `for /f` loop is the
/// exact-line test that `test "$(< f)" = …` is on Unix.
///
/// **Not `findstr /x`, and that is measured rather than taste.** On the
/// Windows ARM64 VM, 2026-09-11, inside the cage:
/// `echo value = 10| findstr /x /c:"value = 10"` matched, and
/// `findstr /x /c:"value = 10" src\a.py` did not, against a file whose
/// contents `type` printed as exactly `value = 10`. `findstr` ends a line at
/// `\r\n`, and Sterna writes `\n`, so the whole file is one line ending in a
/// byte the exact-match test then fails on. The `echo` half is what says the
/// argument arrived intact — the quoting `windows::shell_command_line`
/// performs is not what fails here.
#[cfg(windows)]
#[test]
fn related_sources_can_be_inspected_together_then_edited_and_verified_together() {
    let root = fixture("batch");
    std::fs::write(root.join("src/a.py"), "value = 1\n").unwrap();
    std::fs::write(root.join("src/b.py"), "value = 2\n").unwrap();
    let profile = Profile::compile(
        &root,
        Some(r#"{"permissions":{"allow":["Read(**)","Write(**)","Bash"]}}"#),
    );
    let mut runtime = Runtime::new(&profile, &SessionId::new("batch"));
    let inspected = runtime
        .run_cell("const a = context({path:'src/a.py'}); const b = context({path:'src/b.py'});");
    assert_eq!(inspected.turn().record.calls.len(), 2, "{inspected:?}");
    assert!(inspected.turn().stdout_tail.contains("value = 1"));
    assert!(inspected.turn().stdout_tail.contains("value = 2"));
    let changed = runtime.run_cell(r#"
        edit({path:'src/a.py', old:'value = 1', replacement:'value = 10'});
        edit({path:'src/b.py', old:'value = 2', replacement:'value = 20'});
        write({path:'check.cmd', lines:['@echo off', 'for /f "usebackq delims=" %%L in ("src\\a.py") do if not "%%L"=="value = 10" exit /b 1', 'for /f "usebackq delims=" %%L in ("src\\b.py") do if not "%%L"=="value = 20" exit /b 1', 'exit /b 0']});
        const verified = bash({command:'check.cmd'});
        if (verified.exit_code !== 0) throw new Error("verification failed: " + JSON.stringify(verified));
        return verified.exit_code;
    "#);
    assert!(
        matches!(changed, CellOutcome::Returned { .. }),
        "{changed:?}"
    );
    assert_eq!(changed.turn().record.calls.len(), 4, "{changed:?}");
    assert!(
        changed
            .turn()
            .record
            .calls
            .iter()
            .all(|c| c.ended == Ended::Ok),
        "{changed:?}"
    );
    assert_eq!(
        std::fs::read_to_string(root.join("src/a.py")).unwrap(),
        "value = 10\n"
    );
    assert_eq!(
        std::fs::read_to_string(root.join("src/b.py")).unwrap(),
        "value = 20\n"
    );
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn refreshing_changed_source_replaces_the_implicit_edit_version() {
    let root = fixture("refresh");
    let path = root.join("src/value.py");
    std::fs::write(&path, "value = 1\n").unwrap();
    let profile = Profile::compile(&root, None);
    let mut runtime = Runtime::new(&profile, &SessionId::new("refresh"));
    runtime.run_cell("context({path:'src/value.py'});");
    runtime.run_cell("edit({path:'src/value.py', old:'value = 1', replacement:'value = 2'});");
    runtime.run_cell("context({path:'src/value.py'});");
    let changed =
        runtime.run_cell("edit({path:'src/value.py', old:'value = 2', replacement:'value = 3'});");
    assert_eq!(changed.turn().record.calls.len(), 1, "{changed:?}");
    assert_eq!(
        changed.turn().record.calls[0].ended,
        Ended::Ok,
        "{changed:?}"
    );
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "value = 3\n");
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn a_full_context_batch_preserves_whole_evidence_and_does_not_certify_overflow() {
    let root = fixture("batch-cap");
    for name in ["a", "b", "c"] {
        let body = format!(
            "# {name}-BEGIN\n{}# {name}-END\nvalue = 1\n",
            format!("# {}\n", "x".repeat(120)).repeat(100)
        );
        std::fs::write(root.join(format!("src/{name}.py")), body).unwrap();
    }
    let profile = Profile::compile(&root, None);
    let mut runtime = Runtime::new(&profile, &SessionId::new("batch-cap"));
    let inspected = runtime.run_cell("console.log('z'.repeat(100000)); const a = await context({path:'src/a.py'}); const b = await context({path:'src/b.py'}); const c = await context({path:'src/c.py'});");
    assert_eq!(inspected.turn().record.calls.len(), 3, "{inspected:?}");
    for marker in ["a-BEGIN", "a-END", "b-BEGIN", "b-END"] {
        assert!(
            inspected.turn().stdout_tail.contains(marker),
            "lost {marker}"
        );
    }
    assert!(!inspected.turn().stdout_tail.contains("c-BEGIN"));
    // The read happened, so its record survives even though the turn had no
    // room to echo it -- and it carries both numbers, so the next request
    // can be aimed rather than repeated verbatim.
    let overflowed = inspected.turn().record.calls[2]
        .evidence
        .as_ref()
        .expect("an undelivered context keeps its record");
    let note = overflowed
        .omissions
        .iter()
        .find(|note| note.contains("not delivered"))
        .unwrap_or_else(|| panic!("no undelivered note: {:?}", overflowed.omissions));
    assert!(
        note.contains("characters remained") && note.contains("renders"),
        "both numbers: {note}"
    );
    let feedback = sterna::prompt::CellResult {
        ask_answer: None,
        cell: 1,
        elapsed_ms: inspected.turn().elapsed_ms,
        description: None,
        error: None,
        yield_reason: inspected.turn().yield_reason.clone(),
        output: None,
        handle_table: inspected.turn().table.clone(),
        stdout_tail: Some(inspected.turn().stdout_tail.clone()),
        budget: sterna::prompt::Budget {
            turn_cap: 8192,
            task_used: 0,
            task_cap: 0,
            cells_used: 1,
            cells_cap: Some(40),
            feedback: None,
        },
    };
    use sterna::contract::{Block, Conversation, Message, Role};
    let mut call = Message::text(Role::Assistant, "");
    call.content = vec![Block::ToolUse {
        id: "batch".into(),
        name: "execute_cell".into(),
        input: serde_json::json!({"code":"context batch"}),
    }];
    let mut conversation = Conversation {
        system: String::new(),
        messages: vec![
            Message::text(Role::User, "edit these files"),
            call,
            Message::runtime_tool_result(
                "batch",
                sterna::prompt::render_result(&feedback),
                false,
                sterna::prompt::render_result_history(&feedback),
            ),
        ],
    };
    for older in [false, true] {
        if older {
            conversation
                .messages
                .push(Message::text(Role::Assistant, "a later inspection"));
            conversation
                .messages
                .push(Message::runtime("later result", "later history"));
        }
        let projected =
            sterna::prompt::with_task_context(&conversation, "test-model", "edit these files");
        let wire: serde_json::Value = serde_json::from_slice(&sterna::wire::request_body_on_model(
            &projected,
            "test-model",
        ))
        .unwrap();
        let delivered = wire["messages"][2]["content"][0]["content"]
            .as_str()
            .unwrap();
        for marker in ["a-BEGIN", "a-END", "b-BEGIN", "b-END"] {
            assert!(
                delivered.contains(marker),
                "provider lost {marker}, older={older}"
            );
        }
        assert!(!delivered.contains("c-BEGIN"));
        assert!(!delivered.contains("c-END"));
    }

    assert!(inspected.turn().stdout_dropped_tokens > 0);
    let refused = runtime
        .run_cell("await edit({path:'src/c.py', old:'value = 1', replacement:'value = 2'});");
    assert!(refused.turn().record.calls.is_empty(), "{refused:?}");
    assert!(
        std::fs::read_to_string(root.join("src/c.py"))
            .unwrap()
            .ends_with("value = 1\n")
    );
    runtime.run_cell("await context({path:'src/c.py'});");
    let changed = runtime
        .run_cell("await edit({path:'src/c.py', old:'value = 1', replacement:'value = 2'});");
    assert_eq!(changed.turn().record.calls[0].ended, Ended::Ok);
    let _ = std::fs::remove_dir_all(root);
}

/// Measured 2026-09-19: a benchmark run lost whole cells to `context` throwing
/// on a guessed symbol name, and every sibling call in the same `Promise.all`
/// died with it. A miss is an answer now, so the batch survives it.
#[test]
fn a_guessed_symbol_does_not_take_its_sibling_context_calls_down() {
    let root = fixture("symbol-miss-batch");
    let padding = "# padding padding padding padding\n".repeat(600);
    std::fs::write(
        root.join("src/good.py"),
        format!("def kept(value):\n    return value\n{padding}"),
    )
    .unwrap();
    std::fs::write(
        root.join("src/other.py"),
        format!("def present(value):\n    return value\n{padding}"),
    )
    .unwrap();
    let profile = Profile::compile(&root, None);
    let mut runtime = Runtime::new(&profile, &SessionId::new("miss-batch"));

    let cell = runtime.run_cell(
        "const [good, missed] = await Promise.all([\n\
           context({path:'src/good.py', symbol:'kept'}),\n\
           context({path:'src/other.py', symbol:'no_such_name'}),\n\
         ]);\n\
         console.log(`good=${good.complete} missed=${missed.complete}`);\n\
         console.log(`asked=${missed.symbol}`);\n\
         console.log(`outline-has-present=${missed.text.includes('present')}`);",
    );

    let turn = cell.turn();
    assert_eq!(turn.record.calls.len(), 2, "both calls ran: {cell:?}");
    assert!(
        turn.record.calls.iter().all(|call| call.ended == Ended::Ok),
        "neither call throws: {:?}",
        turn.record.calls
    );
    assert!(
        turn.stdout_tail.contains("good=true missed=false"),
        "the sibling survived and the miss is honest: {}",
        turn.stdout_tail
    );
    assert!(
        turn.stdout_tail.contains("asked=no_such_name"),
        "the miss keeps the name that was asked for: {}",
        turn.stdout_tail
    );
    assert!(
        turn.stdout_tail.contains("outline-has-present=true"),
        "the miss names what the file does define: {}",
        turn.stdout_tail
    );
    let _ = std::fs::remove_dir_all(root);
}

/// A batch that outgrows the turn's feedback budget narrows what is left
/// instead of ending the cell: the reads already happened, and the calls
/// after the one that did not fit still run.
#[test]
fn a_context_batch_past_the_feedback_budget_narrows_and_the_cell_runs_on() {
    let root = fixture("budget-runs-on");
    for name in ["a", "b", "c"] {
        let body = format!(
            "# {name}-BEGIN\n{}# {name}-END\nvalue = 1\n",
            format!("# {}\n", "x".repeat(120)).repeat(100)
        );
        std::fs::write(root.join(format!("src/{name}.py")), body).unwrap();
    }
    let profile = Profile::compile(&root, None);
    let mut runtime = Runtime::new(&profile, &SessionId::new("budget-runs-on"));

    let cell = runtime.run_cell(
        "await context({path:'src/a.py'});\nawait context({path:'src/b.py'});\nawait context({path:'src/c.py'});\nconsole.log('reached-the-end');",
    );
    let turn = cell.turn();

    assert!(
        turn.stdout_tail.contains("reached-the-end"),
        "the cell must run past a context the budget could not hold: {:?}",
        turn.yield_reason
    );
    assert!(
        !turn
            .yield_reason
            .as_deref()
            .is_some_and(|why| why.contains("request it in the next cell")),
        "the old refusal ended the cell; it must not: {:?}",
        turn.yield_reason
    );
    assert_eq!(turn.record.calls.len(), 3, "{:?}", turn.record.calls);
    for call in &turn.record.calls {
        assert_eq!(call.tool, "context");
        assert!(
            call.evidence.is_some(),
            "every context keeps its record, delivered or not: {call:?}"
        );
    }
    let _ = std::fs::remove_dir_all(root);
}

/// The conversation is append-only, so a context whose exact bytes an earlier
/// result carries is pointed back to rather than printed again -- and the
/// pointer still binds an edit, because the model has those bytes. A changed
/// file renders different bytes and is printed in full.
#[test]
fn a_context_already_shown_is_pointed_back_to_and_a_changed_one_printed_again() {
    let root = fixture("shown-before");
    let path = root.join("src/value.py");
    std::fs::write(&path, "value = 1\n").unwrap();
    let profile = Profile::compile(&root, None);
    let mut runtime = Runtime::new(&profile, &SessionId::new("shown-before"));

    let first = runtime.run_cell("await context({path:'src/value.py'});");
    assert!(first.turn().stdout_tail.contains("value = 1"), "{first:?}");
    let again = runtime.run_cell("await context({path:'src/value.py'});");
    let again = &again.turn().stdout_tail;
    assert!(
        again.contains("unchanged: identical to the context in cell 1's result"),
        "{again}"
    );
    assert!(again.contains("path: src/value.py"), "{again}");
    assert!(!again.contains("value = 1"), "{again}");

    std::fs::write(&path, "value = 2\n").unwrap();
    let changed = runtime.run_cell("await context({path:'src/value.py'});");
    assert!(
        changed.turn().stdout_tail.contains("value = 2"),
        "{changed:?}"
    );
    assert!(
        !changed.turn().stdout_tail.contains("unchanged:"),
        "{changed:?}"
    );

    std::fs::write(&path, "value = 1\n").unwrap();
    let back = runtime.run_cell("await context({path:'src/value.py'});");
    assert!(
        back.turn()
            .stdout_tail
            .contains("identical to the context in cell 1's result"),
        "{back:?}"
    );
    let edited = runtime
        .run_cell("await edit({path:'src/value.py', old:'value = 1', replacement:'value = 3'});");
    assert_eq!(edited.turn().record.calls[0].ended, Ended::Ok, "{edited:?}");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "value = 3\n");
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn the_same_context_twice_in_one_cell_is_printed_once() {
    let root = fixture("shown-same-cell");
    std::fs::write(root.join("src/value.py"), "value = 1\n").unwrap();
    let profile = Profile::compile(&root, None);
    let mut runtime = Runtime::new(&profile, &SessionId::new("shown-same-cell"));

    let cell = runtime
        .run_cell("await context({path:'src/value.py'});\nawait context({path:'src/value.py'});");
    let stdout = &cell.turn().stdout_tail;
    assert_eq!(stdout.matches("value = 1").count(), 1, "{stdout}");
    assert!(
        stdout.contains("identical to the context earlier in this result"),
        "{stdout}"
    );
    let _ = std::fs::remove_dir_all(root);
}

/// A pointer is one line, so a context already shown is delivered even when
/// the turn has no room left for it in full.
#[test]
fn a_context_already_shown_needs_no_room_in_a_full_turn() {
    let root = fixture("shown-full-turn");
    for name in ["a", "b", "c"] {
        let body = format!(
            "# {name}-BEGIN\n{}# {name}-END\nvalue = 1\n",
            format!("# {}\n", "x".repeat(120)).repeat(100)
        );
        std::fs::write(root.join(format!("src/{name}.py")), body).unwrap();
    }
    let profile = Profile::compile(&root, None);
    let mut runtime = Runtime::new(&profile, &SessionId::new("shown-full-turn"));

    runtime.run_cell("await context({path:'src/c.py'});");
    let cell = runtime.run_cell(
        "await context({path:'src/a.py'});\nawait context({path:'src/b.py'});\nawait context({path:'src/c.py'});",
    );
    let turn = cell.turn();
    assert!(
        turn.stdout_tail
            .contains("identical to the context in cell 1's result"),
        "{:?}",
        turn.record.calls
    );
    let c = turn.record.calls[2].evidence.as_ref().expect("evidence");
    assert!(
        !c.omissions.iter().any(|o| o.contains("not delivered")),
        "{c:?}"
    );
    let _ = std::fs::remove_dir_all(root);
}

/// A checkpoint takes every earlier result out of the request, so nothing
/// can be pointed back to after one.
#[test]
fn after_a_checkpoint_every_context_is_printed_in_full_again() {
    let root = fixture("shown-checkpoint");
    std::fs::write(root.join("src/value.py"), "value = 1\n").unwrap();
    let profile = Profile::compile(&root, None);
    let mut runtime = Runtime::new(&profile, &SessionId::new("shown-checkpoint"));

    runtime.run_cell("await context({path:'src/value.py'});");
    runtime.forget_shown_contexts();
    let after = runtime.run_cell("await context({path:'src/value.py'});");
    assert!(after.turn().stdout_tail.contains("value = 1"), "{after:?}");
    assert!(
        !after.turn().stdout_tail.contains("unchanged:"),
        "{after:?}"
    );
    let _ = std::fs::remove_dir_all(root);
}

/// A command that rewrites a file the model has read -- a formatter -- is
/// delivered as its changed lines with that cell's result, and the next
/// `edit` binds to the new bytes with no new `context`.
#[test]
fn a_file_a_command_rewrote_arrives_as_its_changes_and_an_edit_needs_no_new_context() {
    let root = fixture("rewritten-by-command");
    let path = root.join("src/fmt.py");
    std::fs::write(&path, "def f():\n    x = 1\n    y = 2\n    return x + y\n").unwrap();
    let profile = Profile::compile(&root, None);
    let mut runtime = Runtime::new(&profile, &SessionId::new("rewritten-by-command"));

    runtime.run_cell("await context({path:'src/fmt.py'});");
    let formatted = runtime.run_cell(
        "await bash({command: \"printf 'def f():\\\\n    x = 1\\\\n    y = 22\\\\n    return x + y\\\\n' > src/fmt.py\"});",
    );
    let shown = &formatted.turn().stdout_tail;
    assert!(
        shown.contains("## Changed on disk since you read it"),
        "{shown}"
    );
    assert!(shown.contains("### src/fmt.py ("), "{shown}");
    assert!(
        shown.contains("-    3 |     y = 2\n+    3 |     y = 22\n"),
        "{shown}"
    );
    assert!(
        !shown.contains("x = 1"),
        "unchanged lines are not repeated: {shown}"
    );

    let edited = runtime
        .run_cell("await edit({path:'src/fmt.py', old:'    y = 22', replacement:'    y = 3'});");
    assert_eq!(edited.turn().record.calls[0].ended, Ended::Ok, "{edited:?}");
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "def f():\n    x = 1\n    y = 3\n    return x + y\n"
    );
    let _ = std::fs::remove_dir_all(root);
}

/// A change made while no cell ran -- the person's editor -- arrives with
/// the next cell's result, whatever that cell does.
#[test]
fn a_change_made_between_cells_arrives_with_the_next_result() {
    let root = fixture("rewritten-between");
    let path = root.join("src/value.py");
    std::fs::write(&path, "a = 1\nb = 1\n").unwrap();
    let profile = Profile::compile(&root, None);
    let mut runtime = Runtime::new(&profile, &SessionId::new("rewritten-between"));

    runtime.run_cell("await context({path:'src/value.py'});");
    std::fs::write(&path, "a = 1\nb = 2\n").unwrap();
    let next = runtime.run_cell("return 1;");
    let shown = &next.turn().stdout_tail;
    assert!(
        shown.contains("-    2 | b = 1\n+    2 | b = 2\n"),
        "{shown}"
    );
    let again = runtime.run_cell("return 2;");
    assert!(
        !again.turn().stdout_tail.contains("Changed on disk"),
        "a change is reported once: {again:?}"
    );
    let edited =
        runtime.run_cell("await edit({path:'src/value.py', old:'b = 2', replacement:'b = 3'});");
    assert_eq!(edited.turn().record.calls[0].ended, Ended::Ok, "{edited:?}");
    let _ = std::fs::remove_dir_all(root);
}

/// A rewrite too large to show is named in one line, and the view does not
/// follow it: an `edit` is refused as stale until the file is read again.
#[test]
fn a_rewrite_too_large_to_show_is_named_and_the_view_stays_where_it_was() {
    let root = fixture("rewritten-whole");
    let path = root.join("src/many.py");
    let before: String = (0..300).map(|i| format!("v{i} = {i}\n")).collect();
    let after: String = (0..300).map(|i| format!("w{i} = {i}\n")).collect();
    std::fs::write(&path, &before).unwrap();
    let profile = Profile::compile(&root, None);
    let mut runtime = Runtime::new(&profile, &SessionId::new("rewritten-whole"));

    runtime.run_cell("await context({path:'src/many.py'});");
    std::fs::write(&path, &after).unwrap();
    let next = runtime.run_cell("return 1;");
    let shown = &next.turn().stdout_tail;
    assert!(
        shown.contains("more than 200 lines changed, too many to show here"),
        "{shown}"
    );
    assert!(!shown.contains("+    1 | w0 = 0"), "{shown}");
    let stale = runtime.run_cell(
        "try { await edit({path:'src/many.py', old:'w5 = 5', replacement:'w5 = 6'}); return 'edited'; } catch (e) { return 'refused'; }",
    );
    assert!(
        format!("{stale:?}").contains("head: \"refused\""),
        "{stale:?}"
    );
    assert_eq!(std::fs::read_to_string(&path).unwrap(), after);
    let _ = std::fs::remove_dir_all(root);
}

/// A file the model saw only part of: the lines it saw are renumbered
/// through the change and stay editable, and the change certifies nothing
/// it did not see.
#[test]
fn seen_lines_follow_a_change_and_unseen_lines_stay_unbound() {
    let root = fixture("rewritten-partial");
    let path = root.join("notes.txt");
    let mut lines: Vec<String> = (0..400)
        .map(|i| format!("filler line {i:03} with enough text to make the file long"))
        .collect();
    lines[200] = "marker = 1".to_string();
    std::fs::write(&path, lines.join("\n") + "\n").unwrap();
    let profile = Profile::compile(&root, None);
    let mut runtime = Runtime::new(&profile, &SessionId::new("rewritten-partial"));

    let read = runtime.run_cell("await context({path:'notes.txt', symbol:'marker'});");
    assert!(
        read.turn().stdout_tail.contains("complete: false"),
        "{read:?}"
    );
    lines.splice(
        0..0,
        [
            "new 1".to_string(),
            "new 2".to_string(),
            "new 3".to_string(),
        ],
    );
    std::fs::write(&path, lines.join("\n") + "\n").unwrap();
    let next = runtime.run_cell("return 1;");
    assert!(
        next.turn().stdout_tail.contains("+    1 | new 1\n"),
        "{next:?}"
    );

    // Before any edit of its own: a successful `edit` makes the version it
    // wrote visible whole, which would bind every line after it.
    let unseen = runtime.run_cell(
        "try { await edit({path:'notes.txt', old:'filler line 390 with', replacement:'changed'}); return 'edited'; } catch (e) { return 'refused'; }",
    );
    assert!(
        format!("{unseen:?}").contains("head: \"refused\""),
        "{unseen:?}"
    );
    assert!(
        std::fs::read_to_string(&path)
            .unwrap()
            .contains("filler line 390 with")
    );
    let seen = runtime
        .run_cell("await edit({path:'notes.txt', old:'marker = 1', replacement:'marker = 2'});");
    assert_eq!(seen.turn().record.calls[0].ended, Ended::Ok, "{seen:?}");
    let _ = std::fs::remove_dir_all(root);
}

/// A change made between cells is found before the next cell runs, so an
/// edit in that cell is told the change is in this turn's feedback --
/// never sent to read the file again.
#[test]
fn an_edit_right_after_an_outside_change_is_told_to_wait_one_cell_not_to_reread() {
    let root = fixture("rewritten-then-edited");
    let path = root.join("src/value.py");
    std::fs::write(&path, "a = 1\nb = 1\n").unwrap();
    let profile = Profile::compile(&root, None);
    let mut runtime = Runtime::new(&profile, &SessionId::new("rewritten-then-edited"));

    runtime.run_cell("await context({path:'src/value.py'});");
    std::fs::write(&path, "a = 1\nb = 2\n").unwrap();
    let early = runtime.run_cell(
        "try { await edit({path:'src/value.py', old:'b = 2', replacement:'b = 3'}); return 'edited'; } catch (e) { return String(e.message); }",
    );
    let said = format!("{early:?}");
    assert!(said.contains("in this turn's feedback"), "{said}");
    assert!(
        early.turn().stdout_tail.contains("+    2 | b = 2\n"),
        "{early:?}"
    );
    let edited =
        runtime.run_cell("await edit({path:'src/value.py', old:'b = 2', replacement:'b = 3'});");
    assert_eq!(edited.turn().record.calls[0].ended, Ended::Ok, "{edited:?}");
    let _ = std::fs::remove_dir_all(root);
}

/// A change to line endings or the final newline alone is said as that,
/// not as an empty diff.
#[test]
fn a_line_ending_change_is_named_as_one() {
    let root = fixture("rewritten-endings");
    let path = root.join("src/value.py");
    std::fs::write(&path, "a = 1\nb = 1\n").unwrap();
    let profile = Profile::compile(&root, None);
    let mut runtime = Runtime::new(&profile, &SessionId::new("rewritten-endings"));

    runtime.run_cell("await context({path:'src/value.py'});");
    std::fs::write(&path, "a = 1\nb = 1").unwrap();
    let next = runtime.run_cell("return 1;");
    assert!(
        next.turn()
            .stdout_tail
            .contains("line endings or the final newline changed; no line's text did"),
        "{next:?}"
    );
    let _ = std::fs::remove_dir_all(root);
}

/// Two candidate fixes tried against one check in one cell: each is written,
/// the check runs, the file is put back before the next, and nothing stays
/// changed.
#[cfg(unix)]
#[test]
fn speculate_tries_each_candidate_against_the_check_and_leaves_the_file_as_it_was() {
    let root = fixture("speculate");
    let path = root.join("src/value.py");
    std::fs::write(&path, "value = 1\nother = 1\n").unwrap();
    let profile = Profile::compile(&root, None);
    let mut runtime = Runtime::new(&profile, &SessionId::new("speculate"));

    let tried = runtime.run_cell(
        "const r = await speculate(\"grep -q 'value = 3' src/value.py\", [\n\
           {name: 'two', edits: [{path: 'src/value.py', old: 'value = 1', replacement: 'value = 2'}]},\n\
           {name: 'three', edits: [{path: 'src/value.py', old: 'value = 1', replacement: 'value = 3'}]},\n\
           {name: 'missing', edits: [{path: 'src/value.py', old: 'value = 9', replacement: 'value = 3'}]},\n\
         ]);\n\
         return r.map(t => `${t.name}:${t.applied}:${t.exit_code}:${t.error === null ? '' : t.error}`).join('|');",
    );
    let said = format!("{tried:?}");
    assert!(said.contains("two:true:1:"), "{said}");
    assert!(said.contains("three:true:0:"), "{said}");
    assert!(
        said.contains("missing:false:null:an `old` in `src/value.py` occurs 0 times"),
        "{said}"
    );
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "value = 1\nother = 1\n"
    );
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn speculate_refuses_what_is_not_a_list_of_candidates() {
    let root = fixture("speculate-args");
    let profile = Profile::compile(&root, None);
    let mut runtime = Runtime::new(&profile, &SessionId::new("speculate-args"));
    let refused = runtime.run_cell(
        "try { await speculate('true', []); return 'ran'; } catch (e) { return e.name + ':' + e.message; }",
    );
    let said = format!("{refused:?}");
    assert!(said.contains("ToolError:"), "{said}");
    assert!(
        said.contains("`candidates` is an array of 1 to 4"),
        "{said}"
    );
    let _ = std::fs::remove_dir_all(root);
}
