//! Acceptance tests for GH-PANE-61E-PROMPT against
//! `docs/model-contract.md`.

use sterna::abi::Interface;
use sterna::contract::{Conversation, Message, Role, SessionId};
use sterna::prompt::{self, Budget, CellResult, ErrorSection, ExhaustedReason, Extracted};
use sterna::runtime::handles::{HandleMeta, HandleTable, render_table};
use sterna::runtime::isolate::Runtime;
use sterna::runtime::outcome::CellOutcome;
use sterna::runtime::preview::{
    ArrayValue, FileValue, PREVIEW_TOKEN_CAP, StringValue, TABLE_TOKEN_CAP, Value,
};
use sterna::sandbox::profile::Profile;
use sterna::tools::registry::{self, Tool};

/// §7's four messages, as a golden file. The handle table region is
/// `render_table`'s real bytes for a small fixture, not `model-contract.md`
/// §7's own repository-measured figures (`n=1195`, `inline cost ~30,565
/// tok`) -- those were measured on a real grep and this fixture is five
/// elements, so its `n=`, `inline cost` and `preview` figures are honest for
/// the fixture and illustrative of the shape, not §7's literal numbers.
///
/// Compared with `\r\n` normalised to `\n` on both sides, per the packet's
/// cross-platform requirement.
#[test]
fn the_worked_turn_renders_byte_for_byte() {
    let golden = include_str!("prompt/worked_turn.golden").replace("\r\n", "\n");
    let golden = golden.strip_suffix('\n').unwrap_or(&golden);
    let messages: Vec<&str> = golden.split("\n===MSG===\n").collect();
    assert_eq!(
        messages.len(),
        4,
        "golden file must hold exactly 4 messages"
    );
    let (task, cell_1_program, cell_1_result, cell_2_program) =
        (messages[0], messages[1], messages[2], messages[3]);

    assert_eq!(
        task,
        "Every file that names `IntegrationId` — how many are tests, and which\nproduction files would a new variant force me to touch?"
    );

    // Turn 1 assistant and turn 2 assistant are the model's own literal
    // output; this package's only claim on them is that `extract_program`
    // reads the program back out unchanged.
    assert_eq!(
        prompt::extract_program(cell_1_program),
        Extracted::Program(
            "const hits = await grep({ pattern: \"IntegrationId\", glob: \"crates/glasshouse/**/*.rs\" });\nconst adapter = await read({ path: \"crates/glasshouse/src/harness/mod.rs\" });"
                .to_string()
        )
    );
    assert_eq!(
        prompt::extract_program(cell_2_program),
        Extracted::Program(
            "const isTest = (m) => m.path.startsWith(\"crates/glasshouse/tests/\");\nconst inTests = hits.filter(isTest);\nconst prodFiles = new Set(hits.filter(m => !isTest(m)).map(m => m.path));\nreturn { total: hits.length, in_tests: inTests.length, prod_files: prodFiles.size };"
                .to_string()
        )
    );

    // The cell-1 result is this package's own rendering: `render_table`'s
    // real bytes for a fixture holding `hits` and `adapter`, declared with
    // the same `HandleMeta` the isolate would record for a real `grep`/
    // `read` call so the header carries §7's declared type names.
    let mut fixture = HandleTable::new();
    fixture.declare_with(
        "hits",
        Value::Array(ArrayValue::sampled(
            5,
            vec![
                Value::String(StringValue::sampled(
                    54,
                    "crates/glasshouse/tests/gateway_translate_effort.rs:29".to_string(),
                )),
                Value::String(StringValue::sampled(
                    55,
                    "crates/glasshouse/tests/gateway_translate_effort.rs:512".to_string(),
                )),
                Value::String(StringValue::sampled(
                    57,
                    "crates/glasshouse/tests/gateway_translate_responses.rs:35".to_string(),
                )),
            ],
            Some(Value::String(StringValue::sampled(
                36,
                "crates/sterna/src/tools/registry.rs:12".to_string(),
            ))),
        )),
        1,
        HandleMeta {
            type_label: Some("Grep.Match[]".into()),
            // Illustrative of a real grep's stdout size, not a measurement:
            // this fixture's own five elements are a few hundred bytes.
            size_estimate: 4_296,
            ..HandleMeta::default()
        },
    );
    fixture.declare(
        "adapter",
        Value::File(FileValue {
            path: "crates/glasshouse/src/harness/mod.rs".into(),
            byte_len: 63_979,
            line_count: 1_508,
            mtime: "2026-09-05T14:18:26Z".into(),
            lines: vec![
                "//! The contract every supported harness is reached through.".into(),
                "//!".into(),
            ],
        }),
        1,
    );
    let handle_table = render_table(&fixture, PREVIEW_TOKEN_CAP, TABLE_TOKEN_CAP);

    let result = CellResult {
        ask_answer: None,
        cell: 1,
        elapsed_ms: 412,
        description: None,
        error: None,
        yield_reason: None,
        output: None,
        handle_table: handle_table.clone(),
        stdout_tail: None,
        budget: Budget {
            turn_cap: 8_000,
            task_used: 3_412,
            task_cap: 400_000,
            cells_used: 1,
            cells_cap: Some(40),
            feedback: None,
        },
    };

    assert_eq!(prompt::render_result(&result), cell_1_result);
}

/// The constant against §2's preamble block, embedded here so the two can
/// never drift without this test noticing.
///
/// The contract is the four-space-indented block that opens `You are Sterna,`,
/// not the whole of §2: the section also carries unindented prose *about* the
/// preamble (Phase 61H's migration note is one), and 61H's first rule is that
/// the shipped prompt must not gain explanatory material. So the region is
/// anchored on that line and ended at the last indented line, and every line
/// between the two must itself be indented or empty — commentary inside the
/// block fails here rather than being silently dropped from the comparison.
/// The region ends at §2.1, whose own indented blocks are the variants'.
#[test]
fn the_preamble_is_the_contracts_verbatim() {
    let contract = include_str!("../../../docs/model-contract.md");
    let section = contract
        .split("## 2. The system preamble, verbatim")
        .nth(1)
        .unwrap()
        .split("### 2.1 Interface variants")
        .next()
        .unwrap();

    let lines: Vec<&str> = section.lines().collect();
    let start = lines
        .iter()
        .position(|line| line.starts_with("    You are Sterna,"))
        .expect("§2 must hold an indented preamble opening `    You are Sterna,`");
    let end = lines
        .iter()
        .rposition(|line| line.starts_with("    "))
        .expect("§2's preamble must hold at least one indented line")
        + 1;
    let block = &lines[start..end];
    for line in block {
        assert!(
            line.is_empty() || line.starts_with("    "),
            "§2's preamble block holds an unindented line, which the preamble \
             constant cannot carry: {line:?}"
        );
    }

    let expected = block
        .iter()
        .map(|line| line.strip_prefix("    ").unwrap_or(line))
        .collect::<Vec<_>>()
        .join("\n");
    assert_eq!(prompt::PREAMBLE, expected);
}

/// Every four-space-indented block in a §2.1 part, with the indent removed.
fn indented_blocks(part: &str) -> Vec<String> {
    let mut blocks = Vec::new();
    let mut current: Vec<&str> = Vec::new();
    for line in part.lines() {
        if let Some(text) = line.strip_prefix("    ") {
            current.push(text);
        } else if !current.is_empty() {
            blocks.push(current.join("\n"));
            current.clear();
        }
    }
    if !current.is_empty() {
        blocks.push(current.join("\n"));
    }
    blocks
}

/// §2.1 against `preamble_for`, by the same indented-block technique: every
/// replacement sentence the contract spells out for a variant is a verbatim
/// slice of that variant's rendered preamble, and the cells variant is §2's
/// block untouched.
#[test]
fn the_interface_variants_are_the_contracts_verbatim() {
    let contract = include_str!("../../../docs/model-contract.md");
    let section = contract
        .split("### 2.1 Interface variants")
        .nth(1)
        .expect("§2.1 must exist")
        .split("## 3.")
        .next()
        .unwrap();
    let (hybrid_part, tools_part) = section
        .split_once("**Tools.**")
        .expect("§2.1 must hold a **Tools.** part");
    let hybrid_part = hybrid_part
        .split("**Hybrid.**")
        .nth(1)
        .expect("§2.1 must hold a **Hybrid.** part");

    let hybrid = prompt::preamble_for(Interface::Hybrid);
    let hybrid_blocks = indented_blocks(hybrid_part);
    assert!(
        hybrid_blocks.len() >= 2,
        "§2.1's hybrid part must spell its replacements as indented blocks"
    );
    for block in &hybrid_blocks {
        assert!(
            hybrid.contains(block.as_str()),
            "§2.1's hybrid block is not in the rendered hybrid preamble:\n{block}\n---\n{hybrid}"
        );
    }

    let tools = prompt::preamble_for(Interface::Tools);
    let tools_blocks = indented_blocks(tools_part);
    assert!(
        tools_blocks.len() >= 3,
        "§2.1's tools part must spell its replacements as indented blocks"
    );
    for block in &tools_blocks {
        assert!(
            tools.contains(block.as_str()),
            "§2.1's tools block is not in the rendered tools preamble:\n{block}\n---\n{tools}"
        );
    }

    assert_eq!(prompt::preamble_for(Interface::Cells), prompt::PREAMBLE);
}

#[test]
fn runtime_functions_are_not_advertised_as_provider_native_tools() {
    assert!(prompt::PREAMBLE.contains("provider-native tool"));
    assert!(prompt::PREAMBLE.contains("Runtime tools are callable only inside its code"));
}

#[test]
fn historical_projection_preserves_native_result_correlation() {
    let older = Message::runtime_tool_result("call-old", "live old", true, "historical old");
    let newest = Message::runtime_tool_result("call-new", "live new", false, "historical new");
    let mut conversation = Conversation {
        system: String::new(),
        messages: vec![older, newest],
    };

    prompt::project_runtime_history(&mut conversation, 0);
    assert_eq!(
        conversation.messages[0].content,
        vec![sterna::contract::Block::ToolResult {
            tool_use_id: "call-old".into(),
            content: "historical old".into(),
            is_error: true,
        }]
    );
    assert_eq!(
        conversation.messages[1].content,
        vec![sterna::contract::Block::ToolResult {
            tool_use_id: "call-new".into(),
            content: "live new".into(),
            is_error: false,
        }]
    );
}

#[test]
fn every_registered_tool_has_exactly_one_declaration_and_no_other_does() {
    // The declarations table's keys equal `registry::names()` exactly, so
    // the two cannot silently drift.
    let mut declared: Vec<&str> = prompt::declarations::ENTRIES
        .iter()
        .map(|entry| entry.name)
        .collect();
    let mut registered = registry::names();
    declared.sort_unstable();
    registered.sort_unstable();
    assert_eq!(declared, registered);

    let tools: Vec<&Tool> = registry::ALL.iter().collect();
    let system = prompt::render_system(
        "",
        &tools,
        &prompt::SessionFacts {
            root: "/tmp/x".to_string(),
            writable: Vec::new(),
            network: false,
            interface: Interface::Cells,
            manifest: None,
        },
    );

    // Scoped to the `## Tools` section: the `## Runtime` block declares the
    // host bindings that are not tools, and counting over the whole system
    // block would read those as tool declarations.
    let tools_start =
        system.find("## Tools\n\n").expect("a `## Tools` section") + "## Tools\n\n".len();
    let tools_end = system[tools_start..]
        .find("\n\n## Runtime\n\n")
        .expect("a `## Runtime` section after the tools")
        + tools_start;
    let tools_section = &system[tools_start..tools_end];
    assert_eq!(
        tools_section.matches("declare function ").count(),
        registry::ALL.len(),
        "one declaration per registered tool"
    );
    for name in registry::names() {
        assert_eq!(
            system.matches(&format!("declare function {name}(")).count(),
            1,
            "`{name}` must render exactly one declaration"
        );
    }
    assert_eq!(
        system.matches("// @callers program").count(),
        registry::ALL.len(),
        "every declaration ends with its own @callers line"
    );
}

#[test]
fn the_prompt_teaches_the_literal_multiline_content_form() {
    let system = prompt::render_system(
        "",
        &registry::ALL.iter().collect::<Vec<_>>(),
        &prompt::SessionFacts {
            root: "/tmp/x".to_string(),
            writable: Vec::new(),
            network: false,
            interface: Interface::Cells,
            manifest: None,
        },
    );

    assert!(
        system.contains("content?: string; lines?: string[]"),
        "{system}"
    );
    assert!(system.contains("oldLines?: string[]"), "{system}");
    assert!(system.contains("replacementLines?: string[]"), "{system}");
    assert!(system.contains("each item is one logical line"), "{system}");
    assert!(system.contains("never template literals"), "{system}");
    assert!(system.contains("caller's $WORKTREE"), "{system}");
    assert!(system.contains("${BASH_SOURCE[0]}"), "{system}");
    assert!(system.contains("$WORKTREE"), "{system}");
    assert!(
        system.contains("one double-quoted JavaScript string per logical line"),
        "{system}"
    );
}

#[test]
fn a_result_block_omits_empty_sections_and_writes_none_for_an_empty_table() {
    let empty = CellResult {
        ask_answer: None,
        cell: 1,
        elapsed_ms: 5,
        description: None,
        error: None,
        yield_reason: None,
        output: None,
        handle_table: String::new(),
        stdout_tail: None,
        budget: Budget {
            turn_cap: 8_000,
            task_used: 0,
            task_cap: 400_000,
            cells_used: 0,
            cells_cap: Some(40),
            feedback: None,
        },
    };
    let rendered = prompt::render_result(&empty);
    assert!(rendered.contains("## Handles\n(none)"));
    assert!(!rendered.contains("## Error"));
    assert!(!rendered.contains("## stdout"));
    assert!(rendered.contains("## Usage"));

    let with_stdout = CellResult {
        ask_answer: None,
        cell: 2,
        elapsed_ms: 5,
        description: None,
        error: None,
        yield_reason: None,
        output: None,
        handle_table: "x  number\n1".to_string(),
        stdout_tail: Some("hello".to_string()),
        budget: Budget {
            turn_cap: 8_000,
            task_used: 1,
            task_cap: 400_000,
            cells_used: 1,
            cells_cap: Some(40),
            feedback: None,
        },
    };
    let rendered = prompt::render_result(&with_stdout);
    assert!(rendered.contains("## stdout\nhello"));
    assert!(!rendered.contains("## Error"));

    let with_error = CellResult {
        ask_answer: None,
        cell: 3,
        elapsed_ms: 5,
        description: None,
        error: Some(ErrorSection {
            class: "TypeError".to_string(),
            message: "bad thing".to_string(),
            position: Some((2, 4)),
            frames: vec!["cell 3, line 2".to_string()],
        }),
        yield_reason: None,
        output: None,
        handle_table: String::new(),
        stdout_tail: None,
        budget: Budget {
            turn_cap: 8_000,
            task_used: 1,
            task_cap: 400_000,
            cells_used: 1,
            cells_cap: Some(40),
            feedback: None,
        },
    };
    let rendered = prompt::render_result(&with_error);
    assert!(rendered.starts_with("[cell 3 threw in 5 ms]"));
    assert!(rendered.contains("## Error\nTypeError: bad thing"));
    assert!(rendered.contains("line 2, column 4"));
    assert!(rendered.contains("at cell 3, line 2"));

    // Section order: Handles, Error, stdout, Usage.
    let handles_at = rendered.find("## Handles").unwrap();
    let error_at = rendered.find("## Error").unwrap();
    let budget_at = rendered.find("## Usage").unwrap();
    assert!(handles_at < error_at);
    assert!(error_at < budget_at);
}

#[test]
fn task_spend_has_no_cap_warning_and_limit_preambles_are_one_sentence() {
    let below = CellResult {
        ask_answer: None,
        cell: 1,
        elapsed_ms: 1,
        description: None,
        error: None,
        yield_reason: None,
        output: None,
        handle_table: String::new(),
        stdout_tail: None,
        budget: Budget {
            turn_cap: 8_000,
            task_used: 359_999,
            task_cap: 400_000,
            cells_used: 1,
            cells_cap: Some(40),
            feedback: None,
        },
    };
    assert!(!prompt::render_result(&below).contains("finish or return"));

    let at_ninety = CellResult {
        ask_answer: None,
        cell: 1,
        elapsed_ms: 1,
        description: None,
        error: None,
        yield_reason: None,
        output: None,
        handle_table: String::new(),
        stdout_tail: None,
        budget: Budget {
            turn_cap: 8_000,
            task_used: 360_000,
            task_cap: 400_000,
            cells_used: 1,
            cells_cap: Some(40),
            feedback: None,
        },
    };
    let rendered = prompt::render_result(&at_ninety);
    assert!(rendered.contains("turn output cap 8,000 · task spent 360,000 · cells 1/40"));
    assert!(!rendered.contains("400,000"));
    assert!(!rendered.contains("finish or return"));

    // Every way a task can end says so in one sentence, names what was
    // observed, and ends in the one thing the model may still do.
    for reason in [
        ExhaustedReason::CellLimit { cap: 120 },
        ExhaustedReason::Stalled {
            windows: 3,
            cells: 18,
        },
        ExhaustedReason::NoProgram { turns: 6 },
    ] {
        let sentence = prompt::exhausted_preamble(&reason);
        assert!(!sentence.contains('\n'));
        assert_eq!(sentence.matches('.').count(), 1, "{sentence}");
        assert!(sentence.contains("return"), "{sentence}");
    }
    // The reason is the fact, not a number the model has to interpret.
    assert!(
        prompt::exhausted_preamble(&ExhaustedReason::CellLimit { cap: 40 })
            .contains("this project set (40)")
    );
}

#[test]
fn ordered_sterna_blocks_are_a_program_and_a_ts_block_is_prose() {
    assert_eq!(
        prompt::extract_program("```sterna\nconst x = 1;\n```"),
        Extracted::Program("const x = 1;".to_string())
    );
    assert_eq!(
        prompt::extract_program(
            "here you go\n```sterna\nconst x = 1;\n```\nand also\n```sterna\nconst y = 2;\n```"
        ),
        Extracted::Program("const x = 1;\n;\nconst y = 2;".into())
    );
    assert_eq!(
        prompt::extract_program("just some prose, no code"),
        Extracted::Prose
    );
    assert_eq!(
        prompt::extract_program("```ts\nconst x: number = 1;\n```"),
        Extracted::Prose
    );
}

/// The source scan: this module has no type for a provider-native call block
/// and never renders a live value's payload itself.
#[test]
fn the_prompt_module_never_names_a_tool_use_block() {
    let mod_rs = include_str!("../src/prompt/mod.rs");
    let declarations_rs = include_str!("../src/prompt/declarations.rs");
    for (label, source) in [("mod.rs", mod_rs), ("declarations.rs", declarations_rs)] {
        assert!(!source.contains("tool_use"), "{label} names tool_use");
        assert!(!source.contains("tool_result"), "{label} names tool_result");
        assert!(
            !source.contains("runtime::preview"),
            "{label} names runtime::preview"
        );
    }
}

/// Addendum 4: a throw the runtime could not attribute to a line of the
/// model's program gets no position line at all -- never `line 0, column 0`,
/// which names a place that does not exist. An attributed one still does.
#[test]
fn an_unattributed_throw_omits_the_position_line() {
    let result = |position: Option<(u64, u64)>| CellResult {
        ask_answer: None,
        cell: 3,
        elapsed_ms: 5,
        description: None,
        error: Some(ErrorSection {
            class: "RuntimeTimeout".to_string(),
            message: "the cell ran for 30,000 ms".to_string(),
            position,
            frames: vec!["cell 3, line 2".to_string()],
        }),
        yield_reason: None,
        output: None,
        handle_table: String::new(),
        stdout_tail: None,
        budget: Budget {
            turn_cap: 8_000,
            task_used: 1,
            task_cap: 400_000,
            cells_used: 1,
            cells_cap: Some(40),
            feedback: None,
        },
    };

    let unattributed = prompt::render_result(&result(None));
    assert!(
        unattributed
            .contains("## Error\nRuntimeTimeout: the cell ran for 30,000 ms\n  at cell 3, line 2"),
        "{unattributed}"
    );
    assert!(!unattributed.contains("line 0, column 0"), "{unattributed}");
    assert!(!unattributed.contains(", column "), "{unattributed}");

    let attributed = prompt::render_result(&result(Some((2, 4))));
    assert!(
        attributed.contains("## Error\nRuntimeTimeout: the cell ran for 30,000 ms\nline 2, column 4\n  at cell 3, line 2"),
        "{attributed}"
    );
}

/// Session tm3hb2-1k3n: `line 1, column 47` above `at cell 6, line 1,
/// column 47` read as two places. A frame that already names the position
/// is the only place it is said.
#[test]
fn a_position_the_first_frame_names_is_said_once() {
    let rendered = prompt::render_result(&CellResult {
        ask_answer: None,
        cell: 6,
        description: None,
        elapsed_ms: 26,
        yield_reason: None,
        handle_table: String::new(),
        output: None,
        error: Some(ErrorSection {
            class: "ToolError".into(),
            message: "`rg` failed with exit 2: rg: regex parse error: error: unclosed group".into(),
            position: Some((1, 47)),
            frames: vec!["cell 6, line 1, column 47".into()],
        }),
        stdout_tail: None,
        budget: Budget {
            turn_cap: 8_000,
            task_used: 1,
            task_cap: 400_000,
            cells_used: 1,
            cells_cap: Some(40),
            feedback: None,
        },
    });
    assert_eq!(
        rendered.matches("line 1, column 47").count(),
        1,
        "{rendered}"
    );
    assert!(
        rendered.contains("unclosed group\n  at cell 6, line 1, column 47"),
        "{rendered}"
    );
}

/// `runtime-contract.md` §9.3: a yield's reason is one line directly under
/// the cell line, before `## Handles` -- and it is never rendered with
/// `threw`, because a throw is not a yield whatever else was filled in.
#[test]
fn a_yield_reason_is_one_line_under_the_cell_line() {
    let result = |error: Option<ErrorSection>| CellResult {
        ask_answer: None,
        cell: 3,
        elapsed_ms: 5,
        description: None,
        error,
        yield_reason: Some("the tests did not run; the target is missing".to_string()),
        output: None,
        handle_table: "x  number\n1".to_string(),
        stdout_tail: None,
        budget: Budget {
            turn_cap: 8_000,
            task_used: 1,
            task_cap: 400_000,
            cells_used: 1,
            cells_cap: Some(40),
            feedback: None,
        },
    };

    let yielded = prompt::render_result(&result(None));
    assert!(
        yielded.starts_with(
            "[cell 3 yielded in 5 ms]\nthe tests did not run; the target is missing\n\n## Handles\nx  number"
        ),
        "{yielded}"
    );
    assert!(!yielded.contains("## Error"), "{yielded}");

    let threw = prompt::render_result(&result(Some(ErrorSection {
        class: "TypeError".to_string(),
        message: "bad thing".to_string(),
        position: None,
        frames: Vec::new(),
    })));
    assert!(
        threw.starts_with("[cell 3 threw in 5 ms]\n\n## Handles"),
        "{threw}"
    );
    assert!(!threw.contains("the tests did not run"), "{threw}");
}

/// §6's position line, for the throw a model-written traversal produces most
/// often — end to end, from the cell to the rendered block.
///
/// `ErrorSection::position`'s own doc comment says a position is *"never
/// `line 0, column 0`, which names a place that does not exist"*, and the
/// guard that was installed for it distinguished absent from present. V8
/// reports a stack overflow as present-and-zero, so the model was handed
/// `line 0, column 0` under the message and again on each of ten frames.
#[test]
fn a_stack_overflow_renders_no_position_line_and_no_zero_frames() {
    let root = std::env::temp_dir().join(format!("sterna-prompt-overflow-{}", std::process::id()));
    std::fs::create_dir_all(root.join(".claude")).unwrap();
    let profile = Profile::compile(&root, Some(r#"{"permissions":{"allow":[]}}"#));
    let mut runtime = Runtime::new(&profile, &SessionId::new("overflow"));
    let outcome = runtime.run_cell("function f(n) { return f(n + 1); }\nf(0);\n");
    let _ = std::fs::remove_dir_all(&root);

    let CellOutcome::Threw { error, turn } = &outcome else {
        panic!("expected a throw, got {outcome:?}");
    };
    assert_eq!(error.class, "RangeError", "{error:?}");

    // Assembled exactly as `session.rs` assembles it for the same throw.
    let result = CellResult {
        ask_answer: None,
        cell: turn.record.cell,
        elapsed_ms: turn.elapsed_ms,
        description: None,
        error: Some(ErrorSection {
            class: error.class.clone(),
            message: error.message.clone(),
            position: ErrorSection::position_of(error.line, error.column),
            frames: error
                .stack
                .iter()
                .map(|frame| frame.description.clone())
                .collect(),
        }),
        yield_reason: None,
        output: None,
        handle_table: turn.table.clone(),
        stdout_tail: None,
        budget: Budget {
            turn_cap: 4_000,
            task_used: 1_000,
            task_cap: 100_000,
            cells_used: 1,
            cells_cap: Some(40),
            feedback: None,
        },
    };

    let rendered = prompt::render_result(&result);
    assert!(
        rendered.contains("RangeError: Maximum call stack size exceeded"),
        "{rendered}"
    );
    assert!(!rendered.contains("line 0"), "{rendered}");
    assert!(!rendered.contains("column 0"), "{rendered}");

    // And the guard itself, because the isolate now drops a `(0, 0)` frame
    // before the section is built: without this the rendering above passes
    // whether or not `position_of` filters, and the section builder is the
    // last line of defence for any *other* producer that reports a position
    // present-and-zero (a compile error's own, for one).
    assert_eq!(ErrorSection::position_of(Some(0), Some(0)), None);
    assert_eq!(ErrorSection::position_of(Some(2), Some(4)), Some((2, 4)));
    assert_eq!(ErrorSection::position_of(Some(1), Some(0)), Some((1, 0)));
    assert_eq!(ErrorSection::position_of(None, Some(0)), None);
}

// --- compaction --------------------------------------------------------

fn sample_result(cell: u64) -> String {
    prompt::render_result(&CellResult {
        ask_answer: None,
        cell,
        elapsed_ms: 12,
        description: None,
        error: None,
        yield_reason: None,
        output: None,
        handle_table: "hits  Array  120 rows · preview 8 tok".to_string(),
        stdout_tail: Some("the cell printed this".to_string()),
        budget: Budget {
            turn_cap: 8_000,
            task_used: 3_412,
            task_cap: 400_000,
            cells_used: cell,
            cells_cap: Some(40),
            feedback: None,
        },
    })
}

/// The lossless claim, checked rather than asserted: every section compaction
/// removes is present, in full, in the newest result.
#[test]
fn compaction_removes_only_what_the_newest_result_restates() {
    let old = sample_result(1);
    let newest = sample_result(2);

    let compacted = prompt::compact_result(&old);
    assert!(compacted.len() < old.len(), "nothing was removed");

    for section in ["## Handles", "## Usage"] {
        assert!(
            !compacted.contains(section),
            "`{section}` survived compaction: {compacted}"
        );
        assert!(
            newest.contains(section),
            "`{section}` was dropped but the newest result does not carry it either"
        );
    }
    // What belongs to that cell alone is kept.
    assert!(compacted.starts_with("[cell 1 yielded"), "{compacted}");
    assert!(
        compacted.contains("## stdout\nthe cell printed this"),
        "{compacted}"
    );
}

/// An error belongs to the cell that threw and appears nowhere else, so it
/// survives however old the turn is.
#[test]
fn compaction_never_drops_an_error() {
    let rendered = prompt::render_result(&CellResult {
        ask_answer: None,
        cell: 1,
        elapsed_ms: 1,
        description: None,
        error: Some(ErrorSection {
            class: "TypeError".to_string(),
            message: "hits.filter is not a function".to_string(),
            position: Some((3, 11)),
            frames: Vec::new(),
        }),
        yield_reason: None,
        output: None,
        handle_table: "hits  Array".to_string(),
        stdout_tail: None,
        budget: Budget {
            turn_cap: 8_000,
            task_used: 1,
            task_cap: 400_000,
            cells_used: 1,
            cells_cap: Some(40),
            feedback: None,
        },
    });
    let compacted = prompt::compact_result(&rendered);
    assert!(
        compacted.contains("TypeError: hits.filter is not a function"),
        "{compacted}"
    );
    assert!(compacted.contains("line 3, column 11"), "{compacted}");
    assert!(!compacted.contains("## Handles"), "{compacted}");
}

/// Two things are never touched: the newest rendered result, and anything a
/// person typed.
#[test]
fn compaction_spares_the_newest_result_and_every_word_a_person_wrote() {
    let person = "find every caller of IntegrationId and summarise them";
    let mut conversation = Conversation {
        system: "sys".to_string(),
        messages: vec![
            Message::text(Role::User, person),
            Message::text(Role::Assistant, "```sterna\nconst a = 1;\n```"),
            Message::text(Role::User, sample_result(1)),
            Message::text(Role::Assistant, "```sterna\nconst b = 2;\n```"),
            Message::text(Role::User, sample_result(2)),
        ],
    };
    let report = prompt::compact_conversation(&mut conversation);
    assert_eq!(
        report.messages, 1,
        "exactly the one older result was compacted"
    );

    let text = |index: usize| conversation.messages[index].content[0].text().to_string();
    assert_eq!(text(0), person, "a person's own words were edited");
    assert!(
        !text(2).contains("## Handles"),
        "the older result kept its table"
    );
    assert!(
        text(4).contains("## Handles"),
        "the newest result was compacted"
    );
    assert!(
        text(1).contains("const a = 1"),
        "an assistant turn was edited"
    );
}

/// The checkpoint's job is to say the objects survived; a model told only
/// "the conversation was dropped" would re-run everything it still holds.
#[test]
fn the_checkpoint_names_the_task_and_the_live_handles() {
    let text = prompt::checkpoint(
        "summarise every caller",
        &["hits".to_string(), "files".to_string()],
        Some("http status: 400 — prompt is too long"),
    );
    assert!(text.contains("summarise every caller"), "{text}");
    assert!(text.contains("hits, files"), "{text}");
    assert!(
        text.contains("every handle below is live"),
        "the checkpoint does not say the objects survived: {text}"
    );
    assert!(text.contains("prompt is too long"), "{text}");
}

#[test]
fn repair_fences_are_data_and_cannot_mix_with_executable_code() {
    let edit = r#"{"cell":1,"replace":"broken","with":"fixed"}"#;
    assert_eq!(
        prompt::extract_program(&format!("```sterna-edit\n{edit}\n```")),
        Extracted::Edit(edit.into())
    );
    assert_eq!(
        prompt::extract_program(&format!(
            "```sterna-edit\n{edit}\n```\n```sterna\nreturn 1;\n```"
        )),
        Extracted::TwoBlocks
    );
    assert_eq!(
        prompt::extract_program(&format!(
            "```sterna-edit\n{edit}\n```\n```sterna-edit\n{edit}\n```"
        )),
        Extracted::TwoBlocks
    );
    assert_eq!(
        prompt::extract_program(&format!("```sterna-edit\n{edit}")),
        Extracted::Invalid("unfinished sterna-edit block".into())
    );
    assert_eq!(
        prompt::extract_program("```json\n{}\n```"),
        Extracted::Prose
    );
}

/// The user, 2026-09-17: *"Stays in context might be beneficial."* The
/// descriptor is in the model's own message -- the line before the fence, or
/// the call's `description` -- which compaction never edits, so after
/// compaction the model still holds a line of account for every cell it ran.
/// The result does not repeat it (2026-09-30: the echo was 1 % of Sterna's
/// fresh input over 30 SWE-bench tasks).
#[test]
fn every_descriptor_survives_compaction_when_the_handles_do_not() {
    let described = |cell: u64, description: &str| {
        prompt::render_result(&CellResult {
            ask_answer: None,
            cell,
            elapsed_ms: 12,
            description: Some(description.to_string()),
            error: None,
            yield_reason: None,
            output: None,
            handle_table: "hits  Array  120 rows · preview 8 tok".to_string(),
            stdout_tail: Some("the cell printed this".to_string()),
            budget: Budget {
                turn_cap: 8_000,
                task_used: 3_412,
                task_cap: 400_000,
                cells_used: cell,
                cells_cap: Some(40),
                feedback: None,
            },
        })
    };
    let first = "Reading the ssh design to find what I have to change.";
    let second = "Adding the ssh module and its escape probes.";
    let mut conversation = Conversation {
        system: "sys".to_string(),
        messages: vec![
            Message::text(Role::User, "implement ssh.run"),
            Message::text(
                Role::Assistant,
                format!("{first}\n```sterna\nconst a = 1;\n```"),
            ),
            Message::text(Role::User, described(1, first)),
            Message::text(
                Role::Assistant,
                format!("{second}\n```sterna\nconst b = 2;\n```"),
            ),
            Message::text(Role::User, described(2, second)),
        ],
    };
    let text = |conversation: &Conversation, index: usize| {
        conversation.messages[index].content[0].text().to_string()
    };
    assert!(
        !text(&conversation, 2).contains(first),
        "the result repeated the description its cell already carries: {}",
        text(&conversation, 2)
    );
    prompt::compact_conversation(&mut conversation);

    assert!(
        !text(&conversation, 2).contains("## Handles"),
        "the older result kept its table, so nothing was compacted"
    );
    assert!(
        text(&conversation, 1).contains(first),
        "compaction lost what the cell was for: {}",
        text(&conversation, 1)
    );
    assert!(
        text(&conversation, 3).contains(second),
        "{}",
        text(&conversation, 3)
    );
}

/// A cell whose model said nothing renders exactly as it did before the
/// descriptor existed: absent is not an empty line, and not an error.
#[test]
fn a_result_without_a_descriptor_is_byte_identical_to_the_old_head() {
    let rendered = sample_result(4);
    assert!(
        rendered.starts_with("[cell 4 yielded in 12 ms]\n\n## Handles"),
        "{rendered}"
    );
}
