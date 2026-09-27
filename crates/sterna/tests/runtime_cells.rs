//! Acceptance for GH-PANE-61E-ISOLATE against
//! `docs/runtime.md`, §2 and §5.
//!
//! **Every program here is assembled as a string by this file.** No model is
//! called anywhere in this package, and nothing that runs in the isolate came
//! from one — map line 2457 is not touched by these tests.
//!
//! The tests that spawn are gated to the platforms with a sandbox applier
//! that has ever executed (`sandbox-grants.md` §3): on Windows
//! `tools::invoke` refuses rather than spawning unconfined, which is correct
//! and would make a "the tool ran" assertion fail for a reason that has
//! nothing to do with this package.

use sterna::contract::SessionId;
use sterna::runtime::isolate::{DEFAULT_HEAP_LIMIT_BYTES, Runtime};
use sterna::runtime::outcome::{CellOutcome, HandleRecord};
use sterna::runtime::preview::{self, ErrorValue, Value};
use sterna::sandbox::profile::Profile;
// Gated like their only callers: the Windows cell compiles every target with
// warnings denied, and a helper whose callers are all `unix` tests is dead
// there (the cell's three reds on `a8766b2`).
#[cfg(unix)]
use std::path::Path;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};
#[cfg(any(target_os = "macos", target_os = "linux"))]
use sterna::tools::invoke::CancellationToken;
use sterna::web::{WebBroker, WebConfig, WebResponse, WebTransport};

static COUNTER: AtomicU64 = AtomicU64::new(0);

/// A throwaway project root with a `.claude/`, and a directory outside it.
struct Fixture {
    root: PathBuf,
    outside: PathBuf,
}

impl Fixture {
    fn new(label: &str) -> Self {
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let stem = format!("sterna-runtime-{}-{label}-{n}", std::process::id());
        let root = std::env::temp_dir().join(&stem);
        std::fs::create_dir_all(root.join(".claude")).unwrap();
        let outside = std::env::temp_dir().join(format!("{stem}-outside"));
        std::fs::create_dir_all(&outside).unwrap();
        Self { root, outside }
    }

    fn profile(&self) -> Profile {
        Profile::compile(&self.root, Some(&settings()))
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    fn profile_with(&self, settings: &str) -> Profile {
        Profile::compile(&self.root, Some(settings))
    }

    /// Every caller is a test gated to a host that can spawn (`unix`), so
    /// on Windows this helper is dead and `[workspace.lints.rust]` denies
    /// it -- the sterna Windows cell failed to compile this target at
    /// `b169254` before running one test.
    #[cfg(unix)]
    fn write(&self, path: &Path, contents: &str) -> PathBuf {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(path, contents).unwrap();
        path.to_path_buf()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
        let _ = std::fs::remove_dir_all(&self.outside);
    }
}

/// `Bash(echo*)` is argv admission and grants no file access at all
/// (`sandbox-grants.md` §2) — which is why it is safe in a fixture.
fn settings() -> String {
    r#"{"permissions":{"allow":["Bash(echo*)","Bash(cat*)"]}}"#.to_string()
}

fn runtime(fixture: &Fixture, session: &SessionId) -> Runtime {
    Runtime::new(&fixture.profile(), session)
}

fn returned(outcome: &CellOutcome) -> &Value {
    match outcome {
        CellOutcome::Returned { value, .. } => value,
        other => panic!("expected a return, got {other:?}"),
    }
}

fn returned_string(outcome: &CellOutcome) -> String {
    match returned(outcome) {
        Value::String(text) => text.head().to_string(),
        other => panic!("expected a string, got {other:?}"),
    }
}

fn threw(outcome: &CellOutcome) -> &ErrorValue {
    match outcome {
        CellOutcome::Threw { error, .. } => error,
        other => panic!("expected a throw, got {other:?}"),
    }
}

fn handle<'a>(outcome: &'a CellOutcome, name: &str) -> &'a HandleRecord {
    outcome
        .turn()
        .record
        .handles
        .iter()
        .find(|handle| handle.name == name)
        .unwrap_or_else(|| panic!("`{name}` is not a handle: {:?}", outcome.turn().record))
}

// --- §1 and §2: the persistent scope -----------------------------------

#[test]
fn a_top_level_binding_persists_into_the_next_cell_and_a_redeclaration_replaces_it() {
    let fixture = Fixture::new("scope");
    let session = SessionId::new("scope-session");
    let mut runtime = runtime(&fixture, &session);

    let first = runtime.run_cell("const hits = [1, 2, 3];\nconst other = \"kept\";\n");
    assert!(matches!(first, CellOutcome::Yielded { .. }), "{first:?}");
    assert!(runtime.is_live("hits"));
    assert!(runtime.is_live("other"));

    // Cell 2 reads cell 1's binding by the name the model itself wrote.
    let second = runtime.run_cell("return hits.length + other.length;\n");
    assert_eq!(returned(&second), &Value::Number(7.0));

    // Cell 3 redeclares: not a SyntaxError, and the table says where.
    let third = runtime.run_cell("const hits = [9];\n");
    assert!(matches!(third, CellOutcome::Yielded { .. }), "{third:?}");
    let rendered = runtime.render_handles();
    // The name field is padded to the widest live name in the table, so the
    // gap after `hits` depends on what else is live; match the row, not a gap.
    assert!(
        rendered
            .lines()
            .any(|line| line.starts_with("hits") && line.contains("Array  (replaced at cell 3)")),
        "{rendered}"
    );
    assert_eq!(runtime.handle_names(), vec!["other", "hits"]);

    // And the replacement is what cell 4 reads.
    let fourth = runtime.run_cell("return hits[0];\n");
    assert_eq!(returned(&fourth), &Value::Number(9.0));
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn a_named_runner_reexecutes_tools_across_cells_and_keeps_permission_checks() {
    let fixture = Fixture::new("named-runner");
    fixture.write(&fixture.root.join("verdict"), "first run\n");
    let profile = fixture.profile_with(r#"{"permissions":{"allow":["Read(**)","Bash(cat*)"]}}"#);
    let session = SessionId::new("named-runner-session");
    let mut runtime = Runtime::new(&profile, &session);
    let first = runtime.run_cell(
        r#"const verify = async (command = "cat verdict") => {
            const result = await bash({command});
            console.log(result.stdout);
            return {exit_code: result.exit_code};
        };
        await verify();"#,
    );
    assert!(matches!(first, CellOutcome::Yielded { .. }), "{first:?}");
    assert_eq!(first.turn().record.calls.len(), 1);
    assert!(first.turn().stdout_tail.contains("first run"), "{first:?}");

    fixture.write(&fixture.root.join("verdict"), "changed run\n");
    let second = runtime.run_cell("await verify();");
    assert!(matches!(second, CellOutcome::Yielded { .. }), "{second:?}");
    assert_eq!(second.turn().record.calls.len(), 1);
    assert!(
        second.turn().stdout_tail.contains("changed run"),
        "{second:?}"
    );
    assert!(!second.turn().stdout_tail.contains("first run"));

    let refused = runtime.run_cell("await verify(\"echo not-admitted\");");
    assert_eq!(threw(&refused).class, "PermissionDenied");
    assert!(runtime.is_live("verify"));
}

#[test]
fn structured_returns_continue_and_scalar_or_text_returns_end_the_task() {
    let fixture = Fixture::new("endings");
    let session = SessionId::new("endings-session");
    let mut runtime = runtime(&fixture, &session);

    let yielded = runtime.run_cell("const a = 1;\nconst b = a + 1;\n");
    match &yielded {
        CellOutcome::Yielded { turn } => {
            assert!(turn.table.contains("a  number"), "{}", turn.table);
            assert!(turn.table.contains("b  number"), "{}", turn.table);
        }
        other => panic!("expected a yield, got {other:?}"),
    }
    assert!(!yielded.ends_the_task());

    let returned_outcome = runtime.run_cell("return { total: a + b };\n");
    assert!(!returned_outcome.ends_the_task());
    match returned(&returned_outcome) {
        Value::Object(object) => {
            assert_eq!(object.key_count(), 1);
            assert_eq!(object.entries()[0].0, "total");
        }
        other => panic!("expected an object, got {other:?}"),
    }

    // A bare return is also notebook output rather than a final answer.
    let bare = runtime.run_cell("return;\n");
    assert!(!bare.ends_the_task());
    assert_eq!(returned(&bare), &Value::Undefined);

    // A returned string is notebook output too. Nothing a cell returns ends
    // the task; `answer(text)` is the only thing that does.
    let text = runtime.run_cell("return \"done\";\n");
    assert!(!text.ends_the_task());
    let said = runtime.run_cell("answer(\"done\");\nreturn { still: \"output\" };\n");
    assert!(said.ends_the_task());
    assert_eq!(said.answer(), Some("done"));
}

#[test]
fn a_throw_is_a_result_and_keeps_the_bindings_made_before_it() {
    let fixture = Fixture::new("throw");
    let session = SessionId::new("throw-session");
    let mut runtime = runtime(&fixture, &session);

    let outcome =
        runtime.run_cell("const before = 41;\nthrow new TypeError(\"boom\");\nconst after = 99;\n");
    let CellOutcome::Threw { error, turn } = &outcome else {
        panic!("expected a throw, got {outcome:?}");
    };
    assert_eq!(error.class, "TypeError");
    assert_eq!(error.message, "boom");
    // The model's own line 2, and the model's own column -- not the
    // wrapper's.
    assert_eq!(error.line, Some(2), "{error:?}");
    // A frame must exist for the quantifier below to mean anything: until
    // the isolate was asked to capture traces, `error.stack` was empty for
    // every throw and this assertion was vacuously true.
    assert!(!error.stack.is_empty(), "§5 promises frames: {error:?}");
    assert!(
        error
            .stack
            .iter()
            .all(|frame| frame.description.starts_with("cell ")),
        "a host frame reached the model: {error:?}"
    );

    // §5's third item: the bindings that completed are still there.
    assert!(runtime.is_live("before"), "{turn:?}");
    assert!(!runtime.is_live("after"));
    assert!(turn.table.contains("before  number"), "{}", turn.table);

    // And the next cell recovers in one line, which is the point of it.
    let recovered = runtime.run_cell("return before + 1;\n");
    assert_eq!(returned(&recovered), &Value::Number(42.0));
}

/// The lead's probe, `runtime-contract.md` §2: the value a binding holds when
/// the cell **ends** is what the next cell reads — a counter bumped in a loop
/// body, a `let` reassigned on a later line. A capture taken on the
/// declaration line alone would persist the first value, which a model would
/// meet in its first counter loop.
#[test]
fn a_binding_persists_with_the_value_it_holds_when_the_cell_ends() {
    let fixture = Fixture::new("final-value");
    let session = SessionId::new("final-value");
    let mut runtime = runtime(&fixture, &session);

    let first = runtime.run_cell(
        "let n = 0;\nfor (const i of [1, 2, 3]) n += i;\nlet x = 1;\nx = 2;\nconst box = { hits: 0 };\nbox.hits = 7;\n",
    );
    assert!(matches!(first, CellOutcome::Yielded { .. }), "{first:?}");

    let second = runtime.run_cell("return [n, x, box.hits].join(',');\n");
    assert_eq!(
        returned_string(&second),
        "6,2,7",
        "a binding must persist with the value it held when the cell ended: {second:?}"
    );
}

/// §2's "only three things become handles": a member assignment mutates an
/// object a binding already names and introduces no name of its own. The
/// parser's `get_identifier_name` answers a member expression with its
/// *property* name, so this used to compile a capture of a binding named
/// `hits` and throw `ReferenceError: hits is not defined` at the model.
#[test]
fn a_member_assignment_at_top_level_binds_nothing() {
    let fixture = Fixture::new("member");
    let session = SessionId::new("member-session");
    let mut runtime = runtime(&fixture, &session);

    let outcome = runtime.run_cell(
        "const box = { hits: 0 };\nconst arr = [0];\nbox.hits = 7;\narr[0] = 1;\nbox.nested = { deep: 0 };\nbox.nested.deep = 3;\n",
    );
    assert!(
        matches!(outcome, CellOutcome::Yielded { .. }),
        "a member assignment must not throw: {outcome:?}"
    );
    assert_eq!(
        runtime.handle_names(),
        vec!["box".to_string(), "arr".to_string()],
        "a member name became a handle"
    );

    let next = runtime.run_cell("return [box.hits, arr[0], box.nested.deep].join(',');\n");
    assert_eq!(returned_string(&next), "7,1,3", "{next:?}");
}

/// `runtime-contract.md` §5's third item, with the value §2 requires: the
/// bindings made before the throw persist, each holding what it held when the
/// cell ended, not what it held on the line it was declared.
#[test]
fn a_throw_keeps_the_latest_value_of_every_binding_made_before_it() {
    let fixture = Fixture::new("throw-latest");
    let session = SessionId::new("throw-latest");
    let mut runtime = runtime(&fixture, &session);

    let outcome = runtime.run_cell(
        "let n = 0;\nn = 5;\nconst seen = [];\nseen.push(\"one\");\nthrow new TypeError(\"boom\");\nlet never = 1;\n",
    );
    let CellOutcome::Threw { error, .. } = &outcome else {
        panic!("expected a throw, got {outcome:?}");
    };
    assert_eq!(error.class, "TypeError", "{error:?}");
    assert_eq!(error.line, Some(5), "the model's own line: {error:?}");
    assert!(!runtime.is_live("never"), "a line the throw never reached");

    let recovered = runtime.run_cell("return [n, seen.length].join(',');\n");
    assert_eq!(
        returned_string(&recovered),
        "5,1",
        "a throw must keep each binding's latest value: {recovered:?}"
    );
}

/// The other four shapes §2 calls a top-level binding. `class` keeps its own
/// block scope, so it is the one that cannot be re-read when the cell ends and
/// is captured where it is declared instead.
#[test]
fn destructuring_var_function_and_class_all_persist() {
    let fixture = Fixture::new("shapes");
    let session = SessionId::new("shapes-session");
    let mut runtime = runtime(&fixture, &session);

    let first = runtime.run_cell(
        "const { a, b: renamed } = { a: 1, b: 2 };\nconst [first, ...rest] = [3, 4, 5];\nvar counted = 0;\nfor (const n of rest) counted += n;\nfunction twice(n) { return n * 2; }\nclass Box { constructor(v) { this.v = v; } }\n",
    );
    assert!(matches!(first, CellOutcome::Yielded { .. }), "{first:?}");
    for name in ["a", "renamed", "first", "rest", "counted", "twice", "Box"] {
        assert!(runtime.is_live(name), "{name} did not become a handle");
    }

    let second = runtime.run_cell(
        "return [a, renamed, first, rest.length, counted, twice(3), new Box(6).v].join(',');\n",
    );
    // `counted` is 9 only because the loop body's mutation reached the next
    // cell; a declaration-line capture would answer 0.
    assert_eq!(returned_string(&second), "1,2,3,2,9,6,6", "{second:?}");
}

/// `free` is one of §2's three lifetime events, and the epilogue that re-reads
/// every binding must not undo one the cell performed on itself: the object
/// leaves the persistent scope as well as the table.
#[test]
fn a_name_freed_in_the_cell_that_declared_it_does_not_come_back() {
    let fixture = Fixture::new("free-same-cell");
    let session = SessionId::new("free-same-cell");
    let mut runtime = runtime(&fixture, &session);

    let first = runtime.run_cell("const temp = [1, 2, 3];\nfree(\"temp\");\n");
    assert!(matches!(first, CellOutcome::Yielded { .. }), "{first:?}");
    assert!(!runtime.is_live("temp"), "the table still lists it");

    let second = runtime.run_cell("return typeof temp;\n");
    assert_eq!(
        returned_string(&second),
        "undefined",
        "a freed binding was left on the persistent scope: {second:?}"
    );
}

/// A cell that ends with `return` ends the task with the value the expression
/// has *then* — the epilogue reads the bindings after it, and must not change
/// what the model returned.
#[test]
fn a_return_after_a_mutation_ends_the_task_with_the_bumped_value() {
    let fixture = Fixture::new("return-bumped");
    let session = SessionId::new("return-bumped");
    let mut runtime = runtime(&fixture, &session);

    let outcome = runtime.run_cell("let n = 1;\nfor (const i of [1, 2, 3]) n += i;\nreturn n;\n");
    assert_eq!(returned(&outcome), &Value::Number(7.0), "{outcome:?}");
}

/// The epilogue captures a name a second time, and the table must stay in
/// the order the model declared: a `class` cannot be re-read when the cell
/// ends, so a capture that removed and re-appended would sort it ahead of
/// every name that can be. Added at integration: the mutation
/// `remove-and-append` survived every other test.
#[test]
fn the_table_keeps_declaration_order_when_the_epilogue_recaptures_a_name() {
    let fixture = Fixture::new("declaration-order");
    let session = SessionId::new("declaration-order");
    let mut runtime = runtime(&fixture, &session);

    let outcome = runtime.run_cell("let n = 0;\nclass K {}\nn = 1;\n");
    assert!(
        matches!(outcome, CellOutcome::Yielded { .. }),
        "{outcome:?}"
    );
    assert_eq!(runtime.handle_names(), vec!["n", "K"]);
}

/// A cell that will not even compile is answered in the same turn slot, with
/// the model's own position.
#[test]
fn a_cell_that_does_not_compile_is_a_result_too() {
    let fixture = Fixture::new("syntax");
    let session = SessionId::new("syntax-session");
    let mut runtime = runtime(&fixture, &session);

    let outcome = runtime.run_cell("const a = 1;\nconst = ;\n");
    let CellOutcome::Threw { error, .. } = &outcome else {
        panic!("expected a throw, got {outcome:?}");
    };
    assert_eq!(error.class, "SyntaxError");
    assert_eq!(error.line, Some(2), "{error:?}");
}

// --- §2: nothing is ever evicted ---------------------------------------

#[test]
fn a_handle_is_never_freed_by_the_runtime() {
    let fixture = Fixture::new("evict");
    let session = SessionId::new("evict-session");
    let mut runtime = runtime(&fixture, &session);

    // Enough wide handles that the 2,048-token table cap must drop some
    // from the *rendering*.
    let mut program = String::new();
    for i in 0..40 {
        program.push_str(&format!("const h{i} = \"{}\";\n", "x".repeat(900)));
    }
    let outcome = runtime.run_cell(&program);
    let CellOutcome::Yielded { turn } = &outcome else {
        panic!("expected a yield, got {outcome:?}");
    };
    assert!(
        turn.table.contains("older handles not shown"),
        "the table was not over its cap, so this test proves nothing: {}",
        turn.table
    );
    assert!(
        preview::estimate_tokens(&turn.table) <= preview::TABLE_TOKEN_CAP,
        "the rendered table exceeded its own cap"
    );

    // Every one of them is still live, and every one is still addressable
    // from the next cell -- including the ones the rendering dropped.
    for i in 0..40 {
        assert!(runtime.is_live(&format!("h{i}")), "h{i} was freed");
    }
    let sum = runtime.run_cell("return h0.length + h39.length;\n");
    assert_eq!(returned(&sum), &Value::Number(1800.0));

    // The model's own `free` is one of the three things that can shrink it.
    let after_free = runtime.run_cell("free(\"h0\");\nreturn typeof h0;\n");
    assert_eq!(returned_string(&after_free), "undefined");
    assert!(!runtime.is_live("h0"));
    assert!(runtime.is_live("h1"));

    // And the task ending is the third.
    runtime.end_task();
    assert!(runtime.handle_names().is_empty());
}

#[test]
fn keep_names_a_value_the_model_never_bound() {
    let fixture = Fixture::new("keep");
    let session = SessionId::new("keep-session");
    let mut runtime = runtime(&fixture, &session);

    let outcome = runtime.run_cell("keep(\"chosen\", [1, 2, 3].map(n => n * 2));\n");
    assert!(
        matches!(outcome, CellOutcome::Yielded { .. }),
        "{outcome:?}"
    );
    assert!(runtime.is_live("chosen"));
    let next = runtime.run_cell("return chosen[2];\n");
    assert_eq!(returned(&next), &Value::Number(6.0));
}

// --- §3: the console tail ----------------------------------------------

#[test]
fn console_output_is_capped_to_the_last_512_tokens() {
    let fixture = Fixture::new("console");
    let session = SessionId::new("console-session");
    let mut runtime = runtime(&fixture, &session);

    let outcome = runtime.run_cell(
        "for (let i = 0; i < 3000; i++) { console.log(\"padding line number \" + i); }\n",
    );
    let turn = outcome.turn();
    assert!(
        preview::estimate_tokens(&turn.stdout_tail) <= preview::STDOUT_TOKEN_CAP,
        "the tail was {} tokens",
        preview::estimate_tokens(&turn.stdout_tail)
    );
    assert!(
        turn.stdout_dropped_tokens > 0,
        "3,000 lines fitted in 512 tokens, which cannot be right"
    );
    assert!(
        turn.stdout_tail.contains("padding line number 2999"),
        "the tail is not the end of the output"
    );
    assert!(
        !turn.stdout_tail.contains("padding line number 0\n"),
        "the tail still contains the beginning"
    );
}

// --- the tools ---------------------------------------------------------

#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn a_refused_call_throws_permission_denied_inside_the_program_and_is_catchable() {
    let fixture = Fixture::new("denied");
    let secret = fixture.write(&fixture.outside.join("secret.txt"), "OUTSIDE-SECRET\n");
    let session = SessionId::new("denied-session");
    let mut runtime = runtime(&fixture, &session);

    let program = format!(
        "try {{\n  await read({{ path: {path:?} }});\n  return \"no throw\";\n}} catch (e) {{\n  \
         return [e.name, e instanceof PermissionDenied, e.tool, e.rule.length > 0].join(\"|\");\n}}\n",
        path = secret.to_string_lossy()
    );
    let outcome = runtime.run_cell(&program);
    assert_eq!(
        returned_string(&outcome),
        "PermissionDenied|true|read|true",
        "{outcome:?}"
    );

    // Catchable and final: the turn continued, the task did not end on the
    // refusal itself, and no prompt was involved.
    let after = runtime.run_cell("const still = 1;\n");
    assert!(matches!(after, CellOutcome::Yielded { .. }), "{after:?}");

    // And the refused file's contents never entered the isolate.
    let rendered = runtime.render_handles();
    assert!(!rendered.contains("OUTSIDE-SECRET"), "{rendered}");
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn a_tool_result_is_a_live_object_the_program_computes_over() {
    let fixture = Fixture::new("live");
    let file = fixture.write(
        &fixture.root.join("notes.txt"),
        "alpha\nbeta\ngamma\ndelta\n",
    );
    let session = SessionId::new("live-session");
    let mut runtime = runtime(&fixture, &session);

    let program = format!(
        "const doc = await read({{ path: {path:?} }});\n",
        path = file.to_string_lossy()
    );
    let outcome = runtime.run_cell(&program);
    assert!(
        matches!(outcome, CellOutcome::Yielded { .. }),
        "{outcome:?}"
    );

    let rendered = runtime.render_handles();
    assert!(rendered.contains("doc  File"), "{rendered}");
    // §3: the preview shows the first two lines and never the contents.
    assert!(rendered.contains("alpha"), "{rendered}");
    assert!(!rendered.contains("gamma"), "{rendered}");

    // The payload is nevertheless there, in the isolate, for the program.
    let counted = runtime.run_cell("return doc.lines.filter(l => l.length === 5).length;\n");
    assert_eq!(returned(&counted), &Value::Number(3.0));
}

/// A guard whose condition is false performs no tool call at all: no child,
/// no effect, no hook. The paired positive half runs the *same* call with the
/// *same* grant and must succeed, so a profile that refused everything fails
/// here instead of passing quietly.
#[cfg(unix)]
#[test]
fn a_branch_not_taken_performs_no_tool_call() {
    let fixture = Fixture::new("branch");
    let session = SessionId::new("branch-session");
    let mut runtime = runtime(&fixture, &session);
    let marker = fixture.root.join("marker");

    let taken = |value: bool| {
        format!(
            "if ({value}) {{ await bash({{ command: \"echo made-it > marker\" }}); }}\nconst done{value} = 1;\n"
        )
    };

    let untaken = runtime.run_cell(&taken(false));
    assert!(
        !marker.exists(),
        "a branch that was not taken performed the call"
    );
    assert!(
        untaken.turn().record.calls.is_empty(),
        "a branch that was not taken recorded a call: {untaken:?}"
    );

    let taken = runtime.run_cell(&taken(true));
    assert!(
        marker.exists(),
        "the paired positive half did not run, so the negative proves nothing"
    );
    assert_eq!(taken.turn().record.calls.len(), 1, "{taken:?}");
}

/// Every tool result a program binds carries the recorded call that produced
/// it, and `pure` is the tool's own declaration rather than an inference.
#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn a_bound_tool_result_carries_its_calls_provenance() {
    let fixture = Fixture::new("provenance");
    let file = fixture.write(&fixture.root.join("one.txt"), "hello\n");
    let session = SessionId::new("provenance-session");
    let mut runtime = runtime(&fixture, &session);

    let program = format!(
        "const doc = await read({{ path: {path:?} }});\n",
        path = file.to_string_lossy()
    );
    let outcome = runtime.run_cell(&program);
    let record = &outcome.turn().record;
    let handle = record
        .handles
        .iter()
        .find(|handle| handle.name == "doc")
        .expect("doc is a handle");
    assert_eq!(handle.type_name, "File");
    let provenance = handle.provenance.as_ref().expect("a recorded call");
    assert_eq!(provenance.tool, "read");
    assert!(provenance.pure, "`read` declares itself pure");
    assert_eq!(provenance.sha256.len(), 64, "{provenance:?}");
    assert_eq!(
        provenance.args.get("path").map(String::as_str),
        Some(&*file.to_string_lossy())
    );
}

// --- the constructor ---------------------------------------------------

/// `runtime-contract.md`'s whole ordering claim, as a property of the type:
/// there is no way to reach a cell executor without the session's compiled
/// profile. The executable proof is the `compile_fail` doctest on
/// `Runtime::new`, which `cargo test -p sterna` runs; this test holds the
/// property that doctest depends on, so removing it fails here rather than
/// turning the doctest into a tautology.
#[test]
fn the_runtime_cannot_be_built_without_a_profile() {
    const SOURCES: [&str; 2] = [
        include_str!("../src/runtime/isolate.rs"),
        include_str!("../src/runtime/isolate/watchdog.rs"),
    ];
    let production = SOURCES
        .iter()
        .map(|source| {
            source
                .split_once("#[cfg(test)]")
                .map_or(*source, |(before, _)| before)
        })
        .collect::<Vec<_>>()
        .join("\n");
    // A builder over an already-built runtime (`fn with_x(self, …) -> Self`)
    // is not a constructor: it cannot produce a `Runtime` that does not
    // already exist, so it cannot produce one without a profile. Everything
    // that answers with `Self` from nothing must name one.
    let constructors: Vec<&str> = production
        .lines()
        .filter(|line| line.trim_start().starts_with("pub fn ") && line.contains("-> Self"))
        .filter(|line| !line.contains("(self,") && !line.contains("(self)"))
        .collect();
    assert!(
        !constructors.is_empty(),
        "no constructor was found at all, so this test would pass for the wrong reason"
    );
    for constructor in &constructors {
        assert!(
            constructor.contains("profile: &Profile"),
            "`{constructor}` builds a Runtime without a Profile"
        );
    }
    // And the builders are still seen, so the filter above cannot be what
    // hides a real constructor: every one of them consumes a `Runtime`.
    for builder in production
        .lines()
        .filter(|line| line.trim_start().starts_with("pub fn ") && line.contains("-> Self"))
        .filter(|line| line.contains("(self,") || line.contains("(self)"))
    {
        assert!(
            builder.contains("(self"),
            "`{builder}` was filtered out as a builder without taking a Runtime"
        );
    }
    assert!(
        !production.contains("impl Default for Runtime"),
        "a Default would be a second constructor with no profile"
    );
}

// --- §2's heap ceiling, and §1's absent event loop ---------------------

/// Crossing the ceiling fails the **cell**, names the five largest live
/// handles so the *model* can choose, and frees nothing. §2 is explicit that
/// a handle vanishing under a program that still names it is the failure
/// that would make the whole channel untrustworthy.
#[test]
fn the_heap_ceiling_fails_the_cell_and_frees_nothing() {
    let fixture = Fixture::new("heap");
    let session = SessionId::new("heap-session");
    let mut runtime = Runtime::with_heap_limit(&fixture.profile(), &session, 32 * 1024 * 1024);

    let first =
        runtime.run_cell("const big = \"b\".repeat(1_000_000);\nconst small = \"s\".repeat(10);\n");
    assert!(matches!(first, CellOutcome::Yielded { .. }), "{first:?}");

    let outcome = runtime
        .run_cell("const junk = [];\nwhile (true) { junk.push(new Array(200000).fill(7)); }\n");
    let CellOutcome::Threw { error, .. } = &outcome else {
        panic!("expected the cell to fail, got {outcome:?}");
    };
    assert_eq!(error.class, "RuntimeOutOfMemory", "{error:?}");
    assert!(
        error.message.contains("big"),
        "the error must name the largest live handles: {}",
        error.message
    );
    assert!(
        error.message.contains("nothing was freed"),
        "{}",
        error.message
    );

    // Nothing was evicted: every handle is still live and still named.
    for name in ["big", "small", "junk"] {
        assert!(runtime.is_live(name), "{name} was freed by the runtime");
    }

    // The model decides. Once it does, the isolate is usable again.
    let recovered = runtime.run_cell("free(\"junk\");\nreturn small.length;\n");
    assert_eq!(returned(&recovered), &Value::Number(10.0), "{recovered:?}");
}

/// There is no event loop, so a promise nothing can settle is answered
/// rather than waited on: a cell that hung would take the whole session with
/// it, and §1 gives a cell exactly two endings plus §5's third.
#[test]
fn an_unsettleable_await_is_answered_not_waited_on() {
    let fixture = Fixture::new("stall");
    let session = SessionId::new("stall-session");
    let mut runtime = runtime(&fixture, &session);

    let outcome = runtime.run_cell("const before = 1;\nawait new Promise(() => {});\n");
    let CellOutcome::Threw { error, .. } = &outcome else {
        panic!("expected a throw, got {outcome:?}");
    };
    assert_eq!(error.class, "RuntimeStalled", "{error:?}");
    // And §5 still holds: the binding made before it is live.
    assert!(runtime.is_live("before"));
}

/// A `then` chain of ordinary promises still resolves: the one microtask
/// checkpoint drains the queue, including jobs the queue's own jobs enqueue.
#[test]
fn a_promise_chain_settles_without_an_event_loop() {
    let fixture = Fixture::new("chain");
    let session = SessionId::new("chain-session");
    let mut runtime = runtime(&fixture, &session);

    let outcome = runtime.run_cell(
        "let n = 0;\nfor (let i = 0; i < 50; i++) { n = await Promise.resolve(n + 1); }\nreturn n;\n",
    );
    assert_eq!(returned(&outcome), &Value::Number(50.0), "{outcome:?}");
}

/// TypeScript that has no JavaScript to erase to is refused with a message
/// that says so, in the same turn slot a yield would have used.
#[test]
fn typescript_that_cannot_be_erased_is_a_result_with_a_reason() {
    let fixture = Fixture::new("erase");
    let session = SessionId::new("erase-session");
    let mut runtime = runtime(&fixture, &session);

    let outcome = runtime.run_cell("enum Colour { Red, Green }\n");
    let CellOutcome::Threw { error, .. } = &outcome else {
        panic!("expected a throw, got {outcome:?}");
    };
    assert_eq!(error.class, "TypeScriptNotErasable");
    assert!(error.message.contains("enum"), "{}", error.message);

    // Types that *can* be erased run untouched, at the model's own columns.
    let typed = runtime.run_cell(
        "interface Row { n: number }\nconst rows: Row[] = [{ n: 1 }, { n: 2 }];\nreturn rows.length;\n",
    );
    assert_eq!(returned(&typed), &Value::Number(2.0), "{typed:?}");
}

/// The isolate has no ambient authority: nothing an embedder did not add is
/// there, and the four tools plus the handle functions are all that was
/// added.
#[test]
fn the_isolate_has_no_ambient_authority() {
    let fixture = Fixture::new("ambient");
    let session = SessionId::new("ambient-session");
    let mut runtime = runtime(&fixture, &session);

    let absent = [
        "require",
        "process",
        "fetch",
        "setTimeout",
        "setInterval",
        "XMLHttpRequest",
        "WebAssembly",
        "Deno",
        "global",
        "importScripts",
        // Shared memory is the one door out of this isolate that is not a
        // capability but a *block*: `Atomics.wait` parks the thread inside
        // V8, where an interrupt from another thread may never land.
        "SharedArrayBuffer",
        "Atomics",
    ];
    let program = format!(
        "return [{}].filter(n => typeof globalThis[n] !== \"undefined\").join(\",\");\n",
        absent
            .iter()
            .map(|name| format!("{name:?}"))
            .collect::<Vec<_>>()
            .join(", ")
    );
    let outcome = runtime.run_cell(&program);
    assert_eq!(returned_string(&outcome), "", "{outcome:?}");

    // And exactly what this package adds is there.
    let present = runtime.run_cell(
        "return [\"grep\",\"read\",\"glob\",\"bash\",\"keep\",\"free\",\"handles\"]\n  .filter(n => typeof globalThis[n] === \"function\").length;\n",
    );
    assert_eq!(returned(&present), &Value::Number(7.0), "{present:?}");
}

/// `runtime-contract.md` §5: a throw carries "the source line and column
/// inside the model's own program" and "no stack from inside the runtime, no
/// host frames". A refusal is thrown from a native callback, so V8 captures
/// no stack for it at all (measured: `stack: []`); what the model gets is the
/// line and column of its own call, read from the message. Lead's test,
/// asserting that positively. The host-frame filter itself is unobservable
/// today — no throw in this runtime can carry a frame from outside a cell —
/// and its mutation is recorded as surviving by construction.
#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn a_refusal_carries_the_models_own_line_and_column_and_no_host_frame() {
    let fixture = Fixture::new("no-host-frame");
    let session = SessionId::new("no-host-frame");
    let mut runtime = runtime(&fixture, &session);

    let outcome = runtime.run_cell("const secret = await read({ path: \"/etc/passwd\" });\n");
    let CellOutcome::Threw { error, .. } = &outcome else {
        panic!("a refused read must throw: {outcome:?}");
    };
    assert_eq!(error.class, "PermissionDenied", "{error:?}");
    assert_eq!(
        (error.line, error.column),
        (Some(1), Some(21)),
        "the refusal must point at the model's own `read(` call: {error:?}"
    );
    // Not vacuous: a refusal is thrown from a native callback, and with the
    // isolate now capturing traces the model's own `read(` call is on it.
    assert!(
        !error.stack.is_empty(),
        "the refusal must carry the model's own frame: {error:?}"
    );
    for frame in &error.stack {
        let inside_program =
            frame.description.starts_with("cell 1,") || frame.description.contains("(cell 1,");
        assert!(
            inside_program,
            "a host frame reached the model: {:?} in {error:?}",
            frame.description
        );
    }
}

// --- GH-PANE-61E-ISOLATE-FIX: the verifier's eight findings -------------

/// A handle rebound in a later cell keeps the provenance and the preview of
/// the call that *made* it, not of whatever call sits at the same position in
/// the cell that rebound it.
///
/// The private tag used to carry an index into a per-cell vector, so `const c
/// = a` in a cell that read a different file showed the model that other
/// file's path, byte count and first line, and recorded its SHA-256 as `c`'s
/// provenance — which §4's resume would then have re-materialised `c` from.
#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn a_rebound_handle_keeps_its_own_previous_cells_provenance() {
    let fixture = Fixture::new("alias");
    let notes = fixture.write(&fixture.root.join("notes.txt"), "alpha\nbeta\ngamma\n");
    let tricky = fixture.write(&fixture.root.join("tricky.txt"), "call foo:12:bar here\n");
    let session = SessionId::new("alias-session");
    let mut runtime = runtime(&fixture, &session);

    let first = runtime.run_cell(&format!(
        "const a = await read({{ path: {path:?} }});\n",
        path = notes.to_string_lossy()
    ));
    assert!(matches!(first, CellOutcome::Yielded { .. }), "{first:?}");

    // Cell 2 makes a call of its own and rebinds cell 1's object.
    let second = runtime.run_cell(&format!(
        "const b = await read({{ path: {path:?} }});\nconst c = a;\n",
        path = tricky.to_string_lossy()
    ));
    assert!(matches!(second, CellOutcome::Yielded { .. }), "{second:?}");

    let c = handle(&second, "c");
    assert_eq!(c.type_name, "File");
    let provenance = c.provenance.as_ref().expect("a recorded call");
    assert_eq!(
        provenance.args.get("path").map(String::as_str),
        Some(&*notes.to_string_lossy()),
        "`c` was given another call's provenance: {provenance:?}"
    );
    assert!(c.preview.contains("alpha"), "{}", c.preview);
    assert!(
        !c.preview.contains("tricky"),
        "`c` was shown the other file's preview: {}",
        c.preview
    );
    // And `b`, which the same cell did bind to its own call, is unaffected.
    let b = handle(&second, "b");
    assert_eq!(
        b.provenance
            .as_ref()
            .and_then(|p| p.args.get("path"))
            .map(String::as_str),
        Some(&*tricky.to_string_lossy())
    );

    // The values themselves were never in doubt; this is the assertion that
    // says the preview now agrees with them.
    let third = runtime.run_cell("return [c === a, c.path === a.path].join(\",\");\n");
    assert_eq!(returned_string(&third), "true,true", "{third:?}");
}

/// An `abstract` member erases to nothing, so the subclass's method is the
/// one that runs. The eraser used to leave the member's bare key behind,
/// which is a field declaration: the base constructor created `area` as an
/// own property initialised `undefined`, it shadowed `Sq`'s prototype method,
/// and the program threw `TypeError: this.area is not a function` from inside
/// the base class — nowhere near the cause, on a program `tsc` runs.
#[test]
fn an_abstract_member_erases_to_nothing() {
    let fixture = Fixture::new("abstract");
    let session = SessionId::new("abstract-session");
    let mut runtime = runtime(&fixture, &session);

    let outcome = runtime.run_cell(
        "abstract class Shape {\n  abstract area(): number;\n  describe(): string { return \
         \"area=\" + this.area(); }\n}\nclass Sq extends Shape {\n  constructor(n: number) { \
         super(); }\n  area(): number { return 42; }\n}\nconst s = new Sq(6);\nreturn \
         s.describe();\n",
    );
    assert_eq!(returned_string(&outcome), "area=42", "{outcome:?}");
}

/// A cell that computes forever is stopped at the wall clock, answered as a
/// `RuntimeTimeout` throw, and the session goes on.
///
/// `while (true) {}` allocates nothing, so the heap ceiling never sees it:
/// before the watchdog `run_cell` simply did not return, and every later cell
/// of the task was unreachable.
#[test]
fn a_cell_that_never_yields_is_answered_as_a_timeout_and_the_next_cell_runs() {
    let fixture = Fixture::new("timeout");
    let session = SessionId::new("timeout-session");
    let limit = Duration::from_millis(500);
    let mut runtime = Runtime::with_limits(
        &fixture.profile(),
        &session,
        DEFAULT_HEAP_LIMIT_BYTES,
        limit,
    );

    let started = Instant::now();
    let outcome = runtime.run_cell("const before = 41;\nwhile (true) {}\n");
    let elapsed = started.elapsed();

    let error = threw(&outcome);
    assert_eq!(error.class, "RuntimeTimeout", "{error:?}");
    assert!(
        error.message.contains("wall-clock limit"),
        "{}",
        error.message
    );
    assert!(
        elapsed < limit + Duration::from_secs(2),
        "the cell took {elapsed:?} against a {limit:?} limit"
    );

    // §5: the binding the cell completed before it was stopped is live, and
    // nothing was freed.
    assert!(runtime.is_live("before"), "{}", runtime.render_handles());

    // And the isolate is warm: the next cell runs, and reads that binding.
    let after = runtime.run_cell("return before + 1;\n");
    assert_eq!(returned(&after), &Value::Number(42.0), "{after:?}");
}

/// `Atomics.wait` parks the thread *inside* V8, where an interrupt from the
/// watchdog's thread may never land — so it is closed at the door instead:
/// the isolate is built with `set_allow_atomics_wait(false)` and the
/// bootstrap deletes both globals, which is what this cell reaches.
#[test]
fn atomics_wait_cannot_block_the_session() {
    let fixture = Fixture::new("atomics");
    let session = SessionId::new("atomics-session");
    let limit = Duration::from_millis(500);
    let mut runtime = Runtime::with_limits(
        &fixture.profile(),
        &session,
        DEFAULT_HEAP_LIMIT_BYTES,
        limit,
    );

    let started = Instant::now();
    let outcome =
        runtime.run_cell("Atomics.wait(new Int32Array(new SharedArrayBuffer(8)), 0, 0);\n");
    let elapsed = started.elapsed();

    let error = threw(&outcome);
    assert_eq!(error.class, "ReferenceError", "{error:?}");
    // Answered, not waited on — and not even by the watchdog: the name is
    // gone, so the cell throws in microseconds.
    assert!(elapsed < limit, "the cell took {elapsed:?}");

    let after = runtime.run_cell("return 7;\n");
    assert_eq!(returned(&after), &Value::Number(7.0), "{after:?}");
}

/// The seven host functions are the cell's whole authority, and no door
/// replaces, removes or redefines one. The compile-time refusal covers a
/// declaration; these are the five ways round it the verifier executed.
#[test]
fn no_door_shadows_deletes_or_redefines_a_host_function() {
    let fixture = Fixture::new("doors");
    let session = SessionId::new("doors-session");
    let mut runtime = runtime(&fixture, &session);

    // 1. `keep` — the documented one, and the one a model tidying its table
    //    by a tool's name would reach without trying to break anything.
    let kept = runtime.run_cell(
        "try { keep(\"read\", function fake(){ return \"NOT A TOOL\"; }); return \"no throw\"; } \
         catch (e) { return e.name; }\n",
    );
    assert_eq!(returned_string(&kept), "ToolError", "{kept:?}");

    // 2. `free`.
    let freed = runtime
        .run_cell("try { free(\"grep\"); return \"no throw\"; } catch (e) { return e.name; }\n");
    assert_eq!(returned_string(&freed), "ToolError", "{freed:?}");

    // 3. the private host object, reached by name or through `arguments`.
    let assigned = runtime.run_cell(
        "try { __sterna_cell.s(\"read\", function(){ return \"SHADOWED\"; }); return \"no throw\"; \
         } catch (e) { return e.name; }\n",
    );
    assert_eq!(returned_string(&assigned), "ToolError", "{assigned:?}");

    // 4. `defineProperty` — refused by the language, because the property is
    //    not configurable.
    let defined = runtime.run_cell(
        "try { Object.defineProperty(globalThis, \"grep\", { value: () => \"FORGED\", \
         configurable: true }); return \"no throw\"; } catch (e) { return e.name; }\n",
    );
    assert_eq!(returned_string(&defined), "TypeError", "{defined:?}");

    // 5. plain assignment and `delete`, both silent failures in sloppy mode.
    //    The assignment is inside a function so that it is a *run-time*
    //    write: `bash = 1` at top level is a binding, and `cell::compile`
    //    refuses that one earlier and by name.
    let refused = runtime.run_cell("bash = 1;\n");
    assert_eq!(threw(&refused).class, "ShadowsHostFunction", "{refused:?}");
    let written = runtime.run_cell(
        "(function(){ bash = 1; })();\nconst gone = delete globalThis.handles;\nreturn [typeof \
         bash, gone].join(\",\");\n",
    );
    assert_eq!(returned_string(&written), "function,false", "{written:?}");

    // Every one of the seven is still the function this package installed,
    // in a later cell, which is the property the guard exists for.
    let survived = runtime.run_cell(
        "return [\"grep\",\"read\",\"glob\",\"bash\",\"keep\",\"free\",\"handles\"]\n  .filter(n \
         => typeof globalThis[n] === \"function\").length;\n",
    );
    assert_eq!(returned(&survived), &Value::Number(7.0), "{survived:?}");
    // And none of the refused writes left a handle behind.
    assert_eq!(runtime.handle_names(), vec!["gone"], "{survived:?}");
}

/// **An answer inside a branch is the answer.** `answer` ends the cell where
/// it is called, so the lines after it never run and cannot overwrite what it
/// said -- the shape a model actually writes when it chooses between two
/// answers, and the shape that silently took the wrong one while `answer`
/// merely recorded the text and returned.
#[test]
fn an_answer_ends_the_cell_so_a_later_one_cannot_overwrite_it() {
    let fixture = Fixture::new("answer-branch");
    let session = SessionId::new("answer-branch-session");
    let mut runtime = runtime(&fixture, &session);

    let branched = runtime.run_cell(
        "const wider = true;\nif (wider) { answer(\"look closer\"); }\nanswer(\"rename only\");\n",
    );
    assert!(branched.ends_the_task());
    assert_eq!(branched.answer(), Some("look closer"));

    // And nothing after the call runs at all, not merely nothing that
    // answers: a write placed below it never reaches the disk.
    let planted = fixture.root.join("after-the-answer.txt");
    let stopped = runtime.run_cell(&format!(
        "answer(\"done\");\nawait write({{path: {:?}, content: \"reached\"}});\n",
        planted.to_string_lossy()
    ));
    assert_eq!(stopped.answer(), Some("done"));
    assert!(
        !planted.exists(),
        "a write below the answer ran: {}",
        planted.display()
    );
}

/// Whether a cell returned or yielded is decided by the value its promise
/// fulfils with, not by a flag the program can set.
///
/// `__sterna_cell.e()` used to set that flag, so one line of the model's own
/// program turned its `return` into a yield.
#[test]
fn a_forged_epilogue_does_not_turn_a_return_into_a_yield() {
    let fixture = Fixture::new("epilogue");
    let session = SessionId::new("epilogue-session");
    let mut runtime = runtime(&fixture, &session);

    let named = runtime.run_cell("__sterna_cell.e();\nreturn \"THIS SHOULD END THE TASK\";\n");
    assert!(
        matches!(named, CellOutcome::Returned { .. }),
        "the forged epilogue turned a return into a yield: {named:?}"
    );
    assert_eq!(returned_string(&named), "THIS SHOULD END THE TASK");

    // The same object reached without naming it: `arguments[0]` is the host
    // object, and it buys the same nothing.
    let through_arguments = runtime.run_cell("arguments[0].e();\nreturn \"ALSO THE END\";\n");
    assert!(
        matches!(through_arguments, CellOutcome::Returned { .. }),
        "{through_arguments:?}"
    );
    assert_eq!(returned_string(&through_arguments), "ALSO THE END");

    // A marker minted in an earlier cell does not answer for a later one.
    let stashed = runtime.run_cell("const marker = __sterna_cell.e();\n");
    assert!(
        matches!(stashed, CellOutcome::Yielded { .. }),
        "{stashed:?}"
    );
    let replayed = runtime.run_cell("return marker;\n");
    assert!(
        matches!(replayed, CellOutcome::Returned { .. }),
        "an earlier cell's marker yielded a later one: {replayed:?}"
    );

    // And the ordinary fall-off still yields.
    let fell = runtime.run_cell("const ordinary = 1;\n");
    assert!(matches!(fell, CellOutcome::Yielded { .. }), "{fell:?}");
}

/// §3's preview is of the handle, so it describes the value the cell ended
/// with rather than the one its declaration line saw. `const arr = []` then
/// `arr.push(1,2,3,4,5)` told the model `n=0` for an array of five.
#[test]
fn a_handles_preview_describes_the_value_the_cell_ended_with() {
    let fixture = Fixture::new("preview");
    let session = SessionId::new("preview-session");
    let mut runtime = runtime(&fixture, &session);

    let outcome = runtime.run_cell("const arr = [];\narr.push(1, 2, 3, 4, 5);\n");
    assert!(
        handle(&outcome, "arr").preview.starts_with("n=5"),
        "{}",
        handle(&outcome, "arr").preview
    );

    // A `class` keeps its own block scope, so the epilogue cannot re-read it
    // and the end-of-cell re-marshal is the only thing that can.
    let kept = runtime.run_cell(
        "class Box { }\nconst box = new Box();\nkeep(\"held\", box);\nbox.a = 1;\nbox.b = 2;\n",
    );
    assert!(
        handle(&kept, "held").preview.contains("\"a\""),
        "{}",
        handle(&kept, "held").preview
    );
    assert!(
        handle(&kept, "held").preview.contains("\"b\""),
        "{}",
        handle(&kept, "held").preview
    );
}

/// §2's one recovery mechanism: the `RuntimeOutOfMemory` error lists the five
/// largest live handles so the *model* can choose what to free. It ranked by
/// the sizes taken on each handle's declaration line, so the array that
/// filled the heap was ranked last, at `~0 B`.
#[test]
fn the_out_of_memory_list_names_the_handle_that_filled_the_heap() {
    let fixture = Fixture::new("oom-rank");
    let session = SessionId::new("oom-rank-session");
    let mut runtime = Runtime::with_heap_limit(&fixture.profile(), &session, 32 * 1024 * 1024);

    let first = runtime.run_cell("const modest = \"m\".repeat(100_000);\n");
    assert!(matches!(first, CellOutcome::Yielded { .. }), "{first:?}");

    let outcome =
        runtime.run_cell("const filler = [];\nwhile (true) { filler.push(new Array(200000)); }\n");
    let error = threw(&outcome);
    assert_eq!(error.class, "RuntimeOutOfMemory", "{error:?}");
    assert!(
        error.message.contains("Largest live handles: filler ("),
        "the handle that filled the heap must be ranked first: {}",
        error.message
    );
    assert!(error.message.contains("modest"), "{}", error.message);

    // Nothing was evicted, and the model can still act on what it was told.
    let recovered = runtime.run_cell("free(\"filler\");\nreturn modest.length;\n");
    assert_eq!(
        returned(&recovered),
        &Value::Number(100_000.0),
        "{recovered:?}"
    );
}

/// `declare` states that something exists elsewhere. It emits no code, so it
/// erases to nothing and binds nothing: the first two shapes used to reach V8
/// with the keyword intact and throw `SyntaxError` on valid TypeScript, and
/// the third generated a capture of a name that does not exist and threw
/// `ReferenceError` at a column past the end of the model's own line.
#[test]
fn declare_erases_to_nothing_and_binds_nothing() {
    let fixture = Fixture::new("declare");
    let session = SessionId::new("declare-session");
    let mut runtime = runtime(&fixture, &session);

    for source in [
        "declare const missing: number;\n",
        "declare class Z { n: number }\n",
        "declare function foo(a: number): void;\n",
    ] {
        let outcome = runtime.run_cell(source);
        assert!(
            matches!(outcome, CellOutcome::Yielded { .. }),
            "{source:?} -> {outcome:?}"
        );
        assert!(
            outcome.turn().record.handles.is_empty(),
            "{source:?} made a handle: {:?}",
            outcome.turn().record.handles
        );
    }
    assert!(runtime.handle_names().is_empty());
}

/// §3's drop note tells a model to call `handles()` "for the full list" when
/// the table is over budget, and the model then decides what to `free` from
/// what it is shown. Captures are drained into the table when the cell ends,
/// so the list was one cell stale and did not contain what the model had just
/// bound.
#[test]
fn handles_mid_cell_includes_the_current_cells_bindings() {
    let fixture = Fixture::new("mid-cell");
    let session = SessionId::new("mid-cell-session");
    let mut runtime = runtime(&fixture, &session);

    let first = runtime.run_cell("const earlier = 1;\n");
    assert!(matches!(first, CellOutcome::Yielded { .. }), "{first:?}");

    let outcome = runtime.run_cell("const a = 1;\nconst b = 2;\nreturn handles().join(\",\");\n");
    assert_eq!(returned_string(&outcome), "earlier,a,b", "{outcome:?}");

    // And a name this cell freed is in neither half of the answer.
    let after = runtime.run_cell("const c = 3;\nfree(\"c\");\nreturn handles().join(\",\");\n");
    assert_eq!(returned_string(&after), "earlier,a,b", "{after:?}");
}

/// Whether ripgrep is on `PATH`, which is what decides whether a `grep` call
/// is served by ripgrep or by the host's own `grep` — `tools::invoke` asks the
/// same question and substitutes the binary, so a test that cares which one
/// answered has to ask it too rather than guess from the platform.
///
/// Gated to match its one caller: unused is an error here.
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn ripgrep_on_path() -> bool {
    std::env::var_os("PATH")
        .map(|path| std::env::split_paths(&path).any(|directory| directory.join("rg").is_file()))
        .unwrap_or(false)
}

/// `grep -r` prints lines that are not located matches — `Binary file …
/// matches` is the routine one. Attributing them to the searched path with
/// `line: 0` made them indistinguishable from a hit at the top of a file, so
/// §6's own worked cell (`new Set(hits.map(m => m.path)).size`) counted the
/// searched *directory* as a file.
#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn a_grep_line_that_is_not_a_match_has_no_line_number() {
    let fixture = Fixture::new("grep-binary");
    fixture.write(&fixture.root.join("hit.txt"), "NEEDLE here\n");
    std::fs::write(
        fixture.root.join("bin.dat"),
        b"NEEDLE\x00\x01\x02\x00binary\n",
    )
    .unwrap();
    let session = SessionId::new("grep-binary-session");
    let mut runtime = runtime(&fixture, &session);

    let outcome = runtime.run_cell(&format!(
        "const hits = await grep({{ pattern: \"NEEDLE\", path: {path:?} }});\nconst located = \
         hits.filter(m => m.line !== null);\nreturn [hits.length > located.length, \
         located.length, new Set(located.map(m => m.path)).size].join(\",\");\n",
        path = fixture.root.to_string_lossy()
    ));
    // **Whether an unlocated line is printed at all is a fact about the
    // BACKEND, not about the operating system**, and keying it on the OS is
    // why this test was red on every developer machine and green on both CI
    // cells. `checked_call` hands a `grep` call to ripgrep wherever ripgrep
    // is installed, and ripgrep skips a binary file with a note on *stderr*;
    // so does GNU grep 3.5 and later. Only BSD grep, on a macOS with no
    // ripgrep, puts the notice on stdout, and only there is there a line for
    // the filter to remove. Every backend checks the half that matters on
    // all of them: the located match is exactly one, in exactly one file.
    let expected = if cfg!(target_os = "macos") && !ripgrep_on_path() {
        "true,1,1"
    } else {
        "false,1,1"
    };
    assert_eq!(
        returned_string(&outcome),
        expected,
        "a line grep printed that is not a located match must be filterable: {outcome:?}"
    );
}

/// The cancellation facility the session layer consumes: a token the runtime
/// holds, set from another thread while a call is in flight. A cancelled call
/// is §5's throw in the turn slot a yield would have used, class `Cancelled`,
/// and the session is intact afterwards.
///
/// The command is built out of `bash` builtins on purpose: the seatbelt names
/// one resolved binary in `process-exec*` (the 61D exec-roots ruling), so a
/// confined `bash` cannot exec `/bin/sleep` at all.
#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn a_token_set_during_a_call_cancels_it_and_the_cell_is_answered_as_a_throw() {
    let fixture = Fixture::new("cancel");
    let session = SessionId::new("cancel-session");
    let token = CancellationToken::new();
    let mut runtime = Runtime::new(
        &fixture.profile_with(r#"{"permissions":{"allow":["Bash(while*)","Bash(do*)"]}}"#),
        &session,
    )
    .with_token(token.clone());

    let setter = token.clone();
    let canceller = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(200));
        setter.cancel();
    });

    let started = Instant::now();
    let outcome = runtime.run_cell(
        "const before = 1;\nconst out = await bash({ command: \"while :; do :; done\" });\nreturn \
         out.stdout;\n",
    );
    let elapsed = started.elapsed();
    canceller.join().unwrap();

    let error = threw(&outcome);
    assert_eq!(error.class, "Cancelled", "{error:?}");
    assert!(
        elapsed < Duration::from_secs(5),
        "the call ran for {elapsed:?} against a child that never exits"
    );
    // §5: the binding made before the throw is live, and the turn carries it.
    assert!(runtime.is_live("before"), "{}", runtime.render_handles());
    assert_eq!(handle(&outcome, "before").preview, "1");
}

// --- §9: ending a task from inside the program -------------------------

/// `yieldNow(reason?)` ends the cell in the yield slot at once, from inside
/// a nested guard, before or after an `await`, and no `try`/`catch` in the
/// program intercepts it: the bindings made before it are live, the ones
/// after it never ran, and the next cell finds the isolate warm. It is not
/// an error, and the reason rides the turn.
#[test]
fn yield_now_ends_the_cell_in_the_yield_slot_and_is_not_an_error() {
    let fixture = Fixture::new("yield-now");
    let session = SessionId::new("yield-now-session");
    let mut runtime = runtime(&fixture, &session);

    let outcome = runtime.run_cell(
        "const before = 1;\nif (before === 1) {\n  if (true) { try { yieldNow(\"why\"); } catch (e) \
         { keep(\"caught\", e.name); } }\n}\nconst after = 2;\n",
    );
    let CellOutcome::Yielded { turn } = &outcome else {
        panic!("yieldNow must yield, got {outcome:?}");
    };
    assert_eq!(turn.yield_reason.as_deref(), Some("why"));
    assert_eq!(
        turn.record.outcome,
        sterna::runtime::outcome::CellOutcomeKind::Yielded
    );
    assert!(!outcome.ends_the_task());
    assert!(runtime.is_live("before"), "{}", turn.table);
    assert!(!runtime.is_live("after"), "a statement after yieldNow ran");
    assert!(
        !runtime.is_live("caught"),
        "a try/catch in the program intercepted the yield: {}",
        turn.table
    );

    // After an `await`, from a microtask, the same.
    let later =
        runtime.run_cell("const x = await 5;\nif (x === 5) { yieldNow(); }\nconst never = 1;\n");
    let CellOutcome::Yielded { turn } = &later else {
        panic!("yieldNow after an await must yield, got {later:?}");
    };
    assert_eq!(turn.yield_reason, None);
    assert!(runtime.is_live("x"), "{}", turn.table);
    assert!(!runtime.is_live("never"));

    // And nothing leaked into the next cell.
    let next = runtime.run_cell("return before + x;\n");
    assert_eq!(returned(&next), &Value::Number(6.0), "{next:?}");
}

/// The free rung of §9.2's ladder: an over-sized response that is mostly
/// padding is rendered with the padding gone, rather than refused.
///
/// No model is asked and nothing is spent — the user, 2026-09-18: *"Erst
/// deterministisch Müll entsorgen"*. The value the program returned is
/// untouched; only the view is shortened, and the note says so.
#[test]
fn a_padded_response_is_normalised_rather_than_refused() {
    let fixture = Fixture::new("response-padding");
    let session = SessionId::new("response-padding-session");
    let mut runtime = runtime(&fixture, &session);
    let cap = sterna::runtime::isolate::DEFAULT_RESPONSE_BYTE_CAP;

    // Just over the cap, and all of the excess is trailing whitespace.
    let padded = runtime.run_cell(&format!(
        "return \"line\" + \" \".repeat({}) + \"\\n\\n\\n\\n\" + \"end\";\n",
        cap
    ));
    let CellOutcome::Returned { terminal, .. } = &padded else {
        panic!("padding alone must not cost the response: {padded:?}");
    };
    let sterna::runtime::outcome::Terminal::Text(text) = terminal else {
        panic!("{terminal:?}");
    };
    assert!(text.starts_with("line\n\nend"), "{text:?}");
    assert!(
        text.contains("whitespace") && text.contains("unchanged"),
        "the shortening must be disclosed: {text:?}"
    );
    assert!(
        text.len() <= cap,
        "the view is still bounded: {}",
        text.len()
    );
}

/// §9.2: a returned string over the response cap is not a return. The cell
/// yields with the size and the cap as its reason, the task continues, and
/// a string at the cap returns verbatim and in full. Bytes, not characters.
///
/// **The reason's wording changed on 2026-09-18** and the change is the
/// point: it now says the bindings are still live and what to do instead,
/// because a cap that only states a size leaves the model to guess whether
/// its value survived. A response of pure content still yields — there is
/// nothing to remove — which is what this test covers; the rung that removes
/// provable noise is `a_padded_response_is_normalised_rather_than_refused`.
#[test]
fn a_response_over_the_cap_yields_with_the_cap_as_its_reason() {
    let fixture = Fixture::new("response-cap");
    let session = SessionId::new("response-cap-session");
    let mut runtime = runtime(&fixture, &session);
    let cap = sterna::runtime::isolate::DEFAULT_RESPONSE_BYTE_CAP;
    assert_eq!(cap, 16_384);

    let over = runtime.run_cell(&format!("return \"x\".repeat({});\n", cap + 1));
    let CellOutcome::Yielded { turn } = &over else {
        panic!("a response over the cap must yield, got {over:?}");
    };
    let reason = turn.yield_reason.as_deref().unwrap_or_default();
    assert!(
        reason.starts_with("the response is 16,385 bytes, over the cap of 16,384 bytes"),
        "{reason}"
    );
    assert!(
        reason.contains("still live") && reason.contains("next cell"),
        "the model must be told its value survived and what to do: {reason}"
    );
    assert_eq!(
        turn.record.outcome,
        sterna::runtime::outcome::CellOutcomeKind::Yielded
    );
    assert!(!over.ends_the_task());

    let at = runtime.run_cell(&format!("return \"x\".repeat({cap});\n"));
    let CellOutcome::Returned { terminal, .. } = &at else {
        panic!("a response at the cap returns, got {at:?}");
    };
    assert_eq!(
        terminal,
        &sterna::runtime::outcome::Terminal::Text("x".repeat(cap))
    );

    // Two-byte characters count twice: 8,193 of them are over, 8,192 are not.
    let wide_over = runtime.run_cell("return \"é\".repeat(8193);\n");
    assert!(
        matches!(&wide_over, CellOutcome::Yielded { turn } if turn.yield_reason.as_deref().is_some_and(|r| r.starts_with("the response is 16,386 bytes"))),
        "{wide_over:?}"
    );
    let wide_at = runtime.run_cell("return \"é\".repeat(8192);\n");
    assert!(
        matches!(&wide_at, CellOutcome::Returned { terminal: sterna::runtime::outcome::Terminal::Text(text), .. } if text.len() == cap),
        "{wide_at:?}"
    );
}

/// §9.1 and §9.4: a guard that did not hold executed nothing, fired no hook
/// and appears nowhere in the trajectory -- the trajectory names only the
/// call that ran.
#[cfg(unix)]
#[test]
fn a_branch_not_taken_performs_no_call() {
    let fixture = Fixture::new("untaken");
    let session = SessionId::new("untaken-session");
    let mut runtime = runtime(&fixture, &session);

    let outcome = runtime.run_cell(
        "const ran = await bash({ command: \"echo ran\" });\nif (ran.stdout === \"never\") { await \
         bash({ command: \"echo untaken\" }); }\n",
    );
    assert!(
        matches!(outcome, CellOutcome::Yielded { .. }),
        "{outcome:?}"
    );

    let calls = &outcome.turn().record.calls;
    assert_eq!(
        calls.len(),
        1,
        "the trajectory names only the call that ran: {calls:?}"
    );
    assert_eq!(calls[0].tool, "bash");
    assert_eq!(
        calls[0].args.get("command").map(String::as_str),
        Some("echo ran")
    );
    assert_eq!(calls[0].ended, sterna::runtime::outcome::Ended::Ok);
}

/// §9.4: every call that ran, in order, with its arguments **as checked**
/// -- the resolved path, never the program's spelling -- and how it ended:
/// ok, or denied with the deciding rule. A refused call is in the
/// trajectory too, because it was attempted.
#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn a_cells_trajectory_names_every_call_that_ran_as_checked() {
    let fixture = Fixture::new("trajectory");
    let file = fixture.write(&fixture.root.join("sub").join("one.txt"), "hello\n");
    let session = SessionId::new("trajectory-session");
    let mut runtime = runtime(&fixture, &session);

    // The program spells the path with a `..` in it; the record carries the
    // path the child was given.
    let spelled = format!("{}/sub/../sub/one.txt", fixture.root.display());
    let program = format!(
        "const doc = await read({{ path: {spelled:?} }});\nconst out = await bash({{ command: \
         \"echo two\" }});\nlet refused = \"\";\ntry {{ await bash({{ command: \"rm -rf /\" }}); \
         }} catch (e) {{ refused = e.name; }}\n"
    );
    let outcome = runtime.run_cell(&program);
    assert!(
        matches!(outcome, CellOutcome::Yielded { .. }),
        "{outcome:?}"
    );
    assert!(
        handle(&outcome, "refused")
            .preview
            .contains("\"PermissionDenied\""),
        "the refused call was caught inside the program: {:?}",
        handle(&outcome, "refused")
    );

    let calls = &outcome.turn().record.calls;
    let tools: Vec<&str> = calls.iter().map(|call| call.tool.as_str()).collect();
    assert_eq!(tools, vec!["read", "bash", "bash"], "{calls:?}");

    let resolved = file.canonicalize().unwrap();
    assert_eq!(
        calls[0].args.get("path").map(String::as_str),
        Some(resolved.to_string_lossy().as_ref()),
        "the path is recorded as checked, not as spelled: {calls:?}"
    );
    assert_ne!(
        calls[0].args.get("path").map(String::as_str),
        Some(spelled.as_str())
    );
    assert_eq!(calls[0].ended, sterna::runtime::outcome::Ended::Ok);

    assert_eq!(
        calls[1].args.get("command").map(String::as_str),
        Some("echo two")
    );
    assert_eq!(calls[1].ended, sterna::runtime::outcome::Ended::Ok);

    assert!(
        matches!(&calls[2].ended, sterna::runtime::outcome::Ended::Denied { rule } if !rule.is_empty()),
        "{calls:?}"
    );
    assert!(
        calls[2].args.is_empty(),
        "a spelling the profile refused is not recorded as admitted: {calls:?}"
    );
}

/// The two windows after `execute` in which the model's own code still runs
/// -- the end-of-cell preview refresh, and the walk that reads a returned
/// value -- neither let a termination reach the next cell nor pass a stopped
/// read off as a completed return.
#[test]
fn a_getter_that_yields_during_the_refresh_terminates_nothing_later() {
    let fixture = Fixture::new("late-getter");
    let session = SessionId::new("late-getter-session");
    let mut runtime = runtime(&fixture, &session);

    // The declaration line's capture reads `y`, so the cell yields there;
    // the refresh after the cell reads `y` again and asks a second time.
    let outcome = runtime
        .run_cell("const x = { get y() { yieldNow(\"late\"); return 1; } };\nconst z = 2;\n");
    assert!(
        matches!(outcome, CellOutcome::Yielded { .. }),
        "{outcome:?}"
    );

    // The next cell runs to its own ending rather than into a termination
    // nobody asked for.
    let next = runtime.run_cell("return 41 + 1;\n");
    assert_eq!(returned(&next), &Value::Number(42.0), "{next:?}");
}

#[test]
fn a_result_whose_getter_never_returns_is_a_timeout_not_a_result() {
    let fixture = Fixture::new("result-getter");
    let session = SessionId::new("result-getter-session");
    let limit = Duration::from_millis(500);
    let mut runtime = Runtime::with_limits(
        &fixture.profile(),
        &session,
        DEFAULT_HEAP_LIMIT_BYTES,
        limit,
    );

    let started = Instant::now();
    let outcome = runtime.run_cell("return { get spin() { while (true) {} } };\n");
    let elapsed = started.elapsed();
    let error = threw(&outcome);
    assert_eq!(error.class, "RuntimeTimeout", "{outcome:?}");
    assert!(!outcome.ends_the_task(), "a stopped read became a return");
    assert!(
        elapsed < limit + Duration::from_secs(2),
        "the read took {elapsed:?} against a {limit:?} limit"
    );

    let after = runtime.run_cell("return 7;\n");
    assert_eq!(returned(&after), &Value::Number(7.0), "{after:?}");

    // A getter that throws while the result is read is the cell's throw,
    // at the model's own line, and the task goes on.
    let thrown = runtime.run_cell("return { get boom() { throw new TypeError(\"no\"); } };\n");
    let error = threw(&thrown);
    assert_eq!(error.class, "TypeError", "{thrown:?}");
    assert_eq!(error.message, "no");
    assert_eq!(error.line, Some(1), "{error:?}");
    assert!(!thrown.ends_the_task());
}

// ---- GH-PANE-61E-ISOLATE-FIX-2 ----
//
// The second independent verifier's findings 1-6, plus the lead's `read` of a
// missing file, each with the cell that demonstrated it. Every one was executed
// against the shipped defaults before it was written down: the first two cells
// below ran for 1:59 and reached about 7 GB of resident memory against a 30 s
// wall clock and a 256 MiB ceiling.

/// The wall clock stops a cell whose loop body *allocates*, not only one that
/// spins — `runtime-contract.md` §2's `RuntimeTimeout`.
///
/// A termination request is observed at V8's own interrupt checks, and
/// TurboFan's code for this loop reaches none — so re-issuing the request
/// every `TERMINATE_RETRY_INTERVAL` did nothing for it either, and the hard
/// deadline could not rescue it because the thread blocked inside V8 is the
/// thread that must return. `isolate::V8_FLAGS` is what makes the request
/// observable; this test is what says so. Measured before that flag, 300 ms
/// limit, killed externally at 6 s:
///
/// | cell | outcome |
/// |---|---|
/// | `while (true) {}` | `RuntimeTimeout` at 311 ms |
/// | `while (true) { const x = new Array(100); }` | `RuntimeTimeout` at 310 ms |
/// | `const a = new Array(100); while (true) { a.fill("y"); }` | `RuntimeTimeout` at 303 ms |
/// | `while (true) { const x = new Array(100).fill("y"); Math.random(); }` | `RuntimeTimeout` at 308 ms |
/// | `while (true) { const x = new Array(100).fill("y"); }` | **never** |
/// | `while (true) { const x = new Array(100); x.fill("y"); }` | **never** |
/// | `while (true) { new Array(100).fill("y"); }` | **never** |
///
/// The old regression (`a_cell_that_never_yields_…`, whose `while (true) {}`
/// allocates nothing) sat on the safe side of a threshold it could not see.
#[test]
fn a_cell_that_allocates_forever_is_answered_as_a_timeout_within_the_grace() {
    let fixture = Fixture::new("fill-timeout");
    let session = SessionId::new("fill-timeout");
    let limit = Duration::from_millis(500);
    let mut runtime = Runtime::with_limits(
        &fixture.profile(),
        &session,
        DEFAULT_HEAP_LIMIT_BYTES,
        limit,
    );

    for width in [100u32, 1000] {
        let started = Instant::now();
        let outcome = runtime.run_cell(&format!(
            "const before = {width};\nwhile (true) {{ const x = new Array({width}).fill(\"y\"); }}\n"
        ));
        let elapsed = started.elapsed();

        let error = threw(&outcome);
        assert_eq!(error.class, "RuntimeTimeout", "fill({width}): {error:?}");
        assert!(
            elapsed < limit + Duration::from_secs(2),
            "fill({width}) took {elapsed:?} against a {limit:?} limit"
        );
        // Stopped by an ordinary re-issued termination, well inside the hard
        // deadline: the isolate is still trusted.
        assert!(
            !runtime.poisoned(),
            "fill({width}) took the hard deadline to stop: {error:?}"
        );

        // §5: the binding made before the loop is live, and the next cell
        // reads it -- whatever the loop body was allocating.
        assert!(runtime.is_live("before"), "{}", runtime.render_handles());
        let after = runtime.run_cell("return before + 1;\n");
        assert_eq!(
            returned(&after),
            &Value::Number(f64::from(width + 1)),
            "fill({width}): {after:?}"
        );
    }
}

/// The heap ceiling is a ceiling: a raise buys the terminated cell room to
/// unwind, and each raise is smaller than the last, so the ceiling holds
/// rather than becoming a first instalment.
///
/// `near_heap_limit` used to add another `initial_heap_limit` on every
/// callback, so a cell that ignored its termination was handed a new ceiling
/// as fast as it could fill the last one — which is how 256 MiB became about
/// 7 GB of resident memory. Answering the second callback with the limit it
/// already had fixed that and cost the process instead (V8 aborts on it), so
/// what bounds the growth now is the shrinking grant.
#[test]
fn a_cell_that_fills_the_heap_is_answered_at_the_configured_ceiling() {
    let fixture = Fixture::new("fill-heap");
    let session = SessionId::new("fill-heap");
    let mut runtime = Runtime::with_limits(
        &fixture.profile(),
        &session,
        32 * 1024 * 1024,
        Duration::from_secs(20),
    );

    let first = runtime.run_cell("const rows = [];\n");
    assert!(matches!(first, CellOutcome::Yielded { .. }), "{first:?}");
    assert_eq!(
        runtime.heap_limit_raises(),
        0,
        "nothing has hit the ceiling"
    );

    let started = Instant::now();
    let outcome = runtime.run_cell("while (true) { rows.push(new Array(1000).fill(\"y\")); }\n");
    let elapsed = started.elapsed();

    let error = threw(&outcome);
    assert_eq!(error.class, "RuntimeOutOfMemory", "{error:?}");
    assert!(
        runtime.heap_limit_raises() <= 4,
        "the ceiling was raised {} times; a handful of shrinking grants is the whole allowance",
        runtime.heap_limit_raises()
    );
    // The ceiling, not the wall clock: answered long before the 20 s limit.
    assert!(
        elapsed < Duration::from_secs(10),
        "the cell took {elapsed:?} to reach a 32 MiB ceiling"
    );

    // Nothing was evicted, and the model can act on what it was told.
    assert!(runtime.is_live("rows"), "{}", runtime.render_handles());
    let recovered = runtime.run_cell("free(\"rows\");\nreturn 7;\n");
    assert_eq!(returned(&recovered), &Value::Number(7.0), "{recovered:?}");
}

/// A cell that ignores every termination for the hard deadline costs the
/// isolate its trust: the cell is answered as a `RuntimeTimeout` naming both
/// deadlines, and no later cell of the task runs code in it.
///
/// The cell is one `Array.prototype.fill` of 30 million elements — a single
/// uninterruptible builtin call, measured at ~34 ms on this host, against a
/// 1 ms limit and therefore a 3 ms hard deadline. That is the shape the
/// watchdog cannot interrupt at all, which is exactly the case the deadline
/// exists for; a loop of small fills is stopped by the re-issued termination
/// long before it (see the timeout test above). The limit is 1 ms rather than
/// the wall clock's own scale so the margin is ~34x: a machine that could run
/// the fill inside 3 ms would fail the `poisoned()` assertion loudly rather
/// than quietly proving nothing.
#[test]
fn a_runtime_that_could_not_stop_a_cell_is_poisoned_and_says_so() {
    let fixture = Fixture::new("poison");
    let session = SessionId::new("poison");
    let limit = Duration::from_millis(1);
    let mut runtime = Runtime::with_limits(&fixture.profile(), &session, 1024 * 1024 * 1024, limit);

    let outcome = runtime.run_cell("const wall = new Array(30_000_000).fill(\"y\");\n");
    let error = threw(&outcome);
    assert_eq!(error.class, "RuntimeTimeout", "{error:?}");
    assert!(
        error.message.contains("hard deadline of 3 ms"),
        "both deadlines must be named: {}",
        error.message
    );
    assert!(
        error.message.contains("wall-clock limit of 1 ms"),
        "both deadlines must be named: {}",
        error.message
    );
    assert!(runtime.poisoned(), "{error:?}");

    // Every later cell is answered without the isolate being entered, and the
    // answer names the cell that did it.
    let refused = runtime.run_cell("return 1;\n");
    let refusal = threw(&refused);
    assert_eq!(refusal.class, "RuntimePoisoned", "{refusal:?}");
    assert!(refusal.message.contains("cell 1"), "{}", refusal.message);
    // And it names what did not stop. This cell's own program did not, and
    // the message says so; the epilogue's poisoning is a different sentence
    // (`isolate::tests::the_epilogue_and_the_cell_are_not_the_same_poisoning`)
    // because telling the model its program did not stop when what ran on is
    // Sterna's own handle-table read is a false statement about its program.
    assert!(
        refusal
            .message
            .contains("cell 1 did not stop when sterna terminated it"),
        "{}",
        refusal.message
    );
    assert_eq!(refused.turn().record.cell, 2, "{refused:?}");

    // And ending the task does not enter it either.
    runtime.end_task();
    assert!(runtime.handle_names().is_empty());
}

/// §3's preview is of the handle **as it now is**, across a cell boundary.
///
/// `refresh_previews` iterated the captures of the cell that had just ended,
/// so a handle declared in an earlier cell was in the *table* and not in the
/// captures, and its preview and size were never taken again for the rest of
/// the task: `const arr = []` in cell 1 and `arr.push(1, 2, 3, 4, 5)` in cell 2
/// left the model reading `n=0` for an array of five while
/// `return arr.length` answered 5. Splitting work across cells is what this
/// runtime is for, so the fixed case was the rarer one.
#[test]
fn a_handle_declared_earlier_shows_the_value_it_now_has() {
    let fixture = Fixture::new("refresh-earlier");
    let session = SessionId::new("refresh-earlier");
    let mut runtime = runtime(&fixture, &session);

    let first = runtime.run_cell("const arr = [];\nconst tag = \"kept\";\n");
    assert!(matches!(first, CellOutcome::Yielded { .. }), "{first:?}");
    assert!(first.turn().table.contains("n=0"), "{}", first.turn().table);

    // Cell 2 binds nothing: `arr` is mutated, not declared.
    let second = runtime.run_cell("arr.push(1, 2, 3, 4, 5);\n");
    assert!(matches!(second, CellOutcome::Yielded { .. }), "{second:?}");
    assert!(
        second.turn().table.contains("n=5"),
        "the table must show the array as it is now: {}",
        second.turn().table
    );
    assert_eq!(handle(&second, "arr").type_name, "Array");

    // Refreshing must not reorder the table or claim a redeclaration.
    assert_eq!(runtime.handle_names(), vec!["arr", "tag"]);
    assert!(
        !second.turn().table.contains("replaced at cell"),
        "nothing was redeclared: {}",
        second.turn().table
    );

    // And the value itself is unchanged by any of it.
    let third = runtime.run_cell("return arr.length;\n");
    assert_eq!(returned(&third), &Value::Number(5.0), "{third:?}");
}

/// §2's one recovery mechanism, across a cell boundary: the
/// `RuntimeOutOfMemory` list ranks by the size a handle has **now**.
///
/// `HandleMeta::size_estimate` is taken where a handle is captured, so a
/// handle the failing cell never bound carried the size it had in the cell
/// that declared it: the array that filled a 32 MiB heap was reported at
/// `~0 B` and ranked behind a 44-character string, telling the model to free
/// exactly the wrong thing. Splitting work across cells is what this runtime
/// is for, so this is the common shape rather than the rare one.
#[test]
fn the_out_of_memory_ranking_counts_a_handle_from_an_earlier_cell() {
    let fixture = Fixture::new("oom-earlier");
    let session = SessionId::new("oom-earlier");
    let mut runtime = Runtime::with_limits(
        &fixture.profile(),
        &session,
        32 * 1024 * 1024,
        Duration::from_secs(20),
    );

    let first = runtime.run_cell(
        "const acc = [];\nconst decoy = \"a much longer string than the empty array is\";\n",
    );
    assert!(matches!(first, CellOutcome::Yielded { .. }), "{first:?}");

    let outcome = runtime.run_cell("while (true) { acc.push(new Array(10000).fill(\"x\")); }\n");
    let error = threw(&outcome);
    assert_eq!(error.class, "RuntimeOutOfMemory", "{error:?}");
    assert!(
        error.message.contains("Largest live handles: acc ("),
        "the handle that filled the heap was declared a cell earlier and must still rank first: \
         {}",
        error.message
    );
    assert!(error.message.contains("decoy"), "{}", error.message);
}

/// §2's lifetime rule from the other side: `free("x")` and then a *rebinding*
/// of `x` in the same cell leaves `x` live with the new value.
///
/// `Runtime::forget_freed` deletes every name the cell freed off the
/// persistent scope after the cell, which is right for declare-then-free and
/// destroyed the binding for free-then-declare — the cheapest recovery the
/// `RuntimeOutOfMemory` message itself invites ("call free(\"name\") on what
/// you no longer need"), performed in one cell, lost both the object and its
/// summary.
#[test]
fn free_then_rebind_in_one_cell_keeps_the_new_binding() {
    let fixture = Fixture::new("free-rebind");
    let session = SessionId::new("free-rebind");
    let mut runtime = runtime(&fixture, &session);

    let first = runtime.run_cell("const big = [1, 2, 3];\nconst w = 0;\n");
    assert!(matches!(first, CellOutcome::Yielded { .. }), "{first:?}");

    // A redeclaration after the free.
    let second = runtime.run_cell("free(\"big\");\nconst big = [1];\n");
    assert!(matches!(second, CellOutcome::Yielded { .. }), "{second:?}");
    assert!(
        runtime.is_live("big"),
        "the rebinding vanished: {}",
        runtime.render_handles()
    );

    // A `keep` after the free, and `handles()` mid-cell sees it.
    let third = runtime.run_cell(
        "free(\"w\");\nkeep(\"w\", 42);\nconsole.log(\"LIVE:\" + handles().join(\",\"));\n",
    );
    assert!(matches!(third, CellOutcome::Yielded { .. }), "{third:?}");
    let logged = &third.turn().stdout_tail;
    assert!(
        logged.contains("LIVE:") && logged.contains('w'),
        "handles() must list the name the cell just re-kept: {logged}"
    );
    assert!(runtime.is_live("w"), "{}", runtime.render_handles());

    // And the next cell reads both, which is the whole claim.
    let fourth = runtime.run_cell("return JSON.stringify([big, w]);\n");
    assert_eq!(returned_string(&fourth), "[[1],42]", "{fourth:?}");
}

/// A write to the persistent scope the scope refuses is a throw the model
/// sees, not a handle that vanishes between turns.
///
/// `capture()` discarded `global.set`'s result, so on a frozen `globalThis`
/// the name went into the handle table, was rendered to the model this turn,
/// and was `undefined` the next — §2's "a handle vanishing under a program
/// that still names it is the failure that would make the whole channel
/// untrustworthy", reached by one line of defensive tidiness.
#[test]
fn a_refused_scope_write_is_a_throw_not_a_vanished_handle() {
    let fixture = Fixture::new("frozen-scope");
    let session = SessionId::new("frozen-scope");

    {
        let mut runtime = runtime(&fixture, &session);
        let outcome = runtime.run_cell("Object.freeze(globalThis);\nconst x = 5;\n");
        let error = threw(&outcome);
        assert_eq!(error.class, "TypeError", "{error:?}");
        assert!(
            error.message.contains("`x` could not be bound"),
            "the throw must name the binding: {}",
            error.message
        );
        assert!(
            !runtime.is_live("x"),
            "a handle the scope refused reached the table: {}",
            runtime.render_handles()
        );
    }

    // The quieter variant: a pre-existing non-writable own property.
    let mut runtime = runtime(&fixture, &session);
    let outcome = runtime.run_cell(
        "Object.defineProperty(globalThis, \"y\", { value: 1, writable: false, configurable: \
         false });\nconst y = 99;\n",
    );
    let error = threw(&outcome);
    assert_eq!(error.class, "TypeError", "{error:?}");
    assert!(
        error.message.contains("`y` could not be bound"),
        "{}",
        error.message
    );
    assert!(!runtime.is_live("y"), "{}", runtime.render_handles());
}

/// §5's "the top three in-program frames", which were an empty list on every
/// error a model has ever been shown.
///
/// `thrown_error` reads frames from `v8::Exception::get_stack_trace`, which
/// yields a structured trace only when the isolate has been asked to capture
/// one — and nothing asked. Both existing assertions about frames are
/// quantifiers over the frame list, so both were vacuously true; this is the
/// one that says a frame exists.
#[test]
fn a_nested_throw_carries_its_in_program_frames() {
    let fixture = Fixture::new("frames");
    let session = SessionId::new("frames");
    let mut runtime = runtime(&fixture, &session);

    let outcome = runtime.run_cell(
        "function inner() { throw new Error(\"boom\"); }\nfunction outer() { return inner(); \
         }\nfunction top() { return outer(); }\ntop();\n",
    );
    let error = threw(&outcome);
    assert_eq!(error.class, "Error", "{error:?}");
    assert!(
        !error.stack.is_empty(),
        "§5 promises the top in-program frames: {error:?}"
    );
    assert!(
        error.stack[0].description.starts_with("inner ("),
        "the innermost frame is the model's own `inner`: {:?}",
        error.stack
    );
    // And still only the model's own program: no host frame, ever.
    for frame in &error.stack {
        assert!(
            frame.description.contains("cell 1,"),
            "a host frame reached the model: {frame:?}"
        );
    }
}

/// A compile-time refusal points at the declaration it refused.
///
/// `CellError::ShadowsHostFunction` and `CellError::ReservedName` carried no
/// span, so `compile_error_value` produced `line: None` and the model was
/// shown `line 0, column 0` — a position no program has — on a program it can
/// write on its first turn.
#[test]
fn a_compile_time_refusal_carries_the_span_of_the_declaration() {
    let fixture = Fixture::new("refusal-span");
    let session = SessionId::new("refusal-span");
    let mut runtime = runtime(&fixture, &session);

    let outcome = runtime.run_cell("const read = 1;\n");
    let error = threw(&outcome);
    assert_eq!(error.class, "ShadowsHostFunction", "{error:?}");
    assert_eq!((error.line, error.column), (Some(1), Some(0)), "{error:?}");

    // The span is the declaration's own, not the program's start.
    let later = runtime.run_cell("const a = 1;\nconst b = 2;\n  const __sterna_x = 3;\n");
    let error = threw(&later);
    assert_eq!(error.class, "ReservedName", "{error:?}");
    assert_eq!((error.line, error.column), (Some(3), Some(2)), "{error:?}");
}

/// **Every registered tool's name is protected, not a hand-copied four of
/// them.** `write`, `context` and `edit` were missing from the guard's list,
/// so `const write = 1;` compiled, the whole cell ran, and the model was told
/// afterwards that its `globalThis` was frozen — the wrong cause, a turn late.
#[test]
fn a_binding_may_not_take_any_registered_tools_name() {
    let fixture = Fixture::new("shadow-registry");
    let session = SessionId::new("shadow-registry");
    let mut runtime = runtime(&fixture, &session);

    // The marker is declared *first*, so a cell that ran before being refused
    // leaves it live and a cell refused at compile time cannot.
    let outcome = runtime.run_cell("const marker = 2;\nconst write = 1;\n");
    let error = threw(&outcome);
    assert_eq!(error.class, "ShadowsHostFunction", "{error:?}");
    assert_eq!((error.line, error.column), (Some(2), Some(0)), "{error:?}");
    assert!(
        !runtime.is_live("marker"),
        "the cell ran before it was refused"
    );

    for name in sterna::tools::registry::names() {
        let refused = runtime.run_cell(&format!("const {name} = 1;\n"));
        assert_eq!(
            threw(&refused).class,
            "ShadowsHostFunction",
            "`{name}` may be shadowed: {refused:?}"
        );
    }
}

/// `runtime-contract.md` §9.1: a failed call cannot itself become an answer.
///
/// Every builder in `bindings.rs` reads the child's `stdout` and none of them
/// read its exit code, so `read` of a missing file answered with a `File`
/// handle — `0 B`, `0 lines`, the SHA-256 of the empty string as its
/// provenance — and `"all good: " + f.text` became the task's terminal
/// response while `cat` had exited 1 with its message on stderr.
#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn a_read_of_a_missing_file_throws_and_never_becomes_a_result() {
    let fixture = Fixture::new("read-missing");
    let session = SessionId::new("read-missing-session");
    let mut runtime = runtime(&fixture, &session);

    let outcome = runtime.run_cell(&format!(
        "const f = await read({{ path: {path:?} }});\nreturn \"all good: \" + f.text;\n",
        path = fixture.root.join("missing.txt").to_string_lossy()
    ));
    let error = threw(&outcome);
    assert_eq!(error.class, "ToolError", "{error:?}");
    assert!(
        error.message.contains("`read` failed with exit 1"),
        "the throw must name the tool and the child's status: {}",
        error.message
    );
    // The model's own line, so §5's position is one its program has.
    assert_eq!(error.line, Some(1), "{error:?}");
    // And no handle was minted for a call that did not produce one.
    assert!(
        !runtime.is_live("f"),
        "a failed call became a handle: {}",
        runtime.render_handles()
    );

    // A read that succeeds is untouched by the check.
    let notes = fixture.write(&fixture.root.join("notes.txt"), "alpha\nbeta\n");
    let ok = runtime.run_cell(&format!(
        "const g = await read({{ path: {path:?} }});\nreturn g.lines.length;\n",
        path = notes.to_string_lossy()
    ));
    assert_eq!(returned(&ok), &Value::Number(2.0), "{ok:?}");
}

/// The live failure this API closes: `${...}` in a template literal is a
/// JavaScript expression, but in a line-array string it reaches disk as shell
/// syntax. `$WORKTREE` is likewise data, not an expansion by Sterna.
#[test]
fn a_cell_writes_literal_multiline_scripts_without_template_interpolation() {
    let fixture = Fixture::new("literal-script-lines");
    let session = SessionId::new("literal-script-lines-session");
    let mut runtime = runtime(&fixture, &session);

    let outcome = runtime.run_cell(
        r##"await write({
  path: "run.sh",
  lines: [
    "#!/usr/bin/env bash",
    'source="${BASH_SOURCE[0]}"',
    'printf "%s\\n" "$WORKTREE"'
  ]
});
return "written";
"##,
    );
    assert_eq!(returned_string(&outcome), "written", "{outcome:?}");
    assert_eq!(
        std::fs::read_to_string(fixture.root.join("run.sh")).unwrap(),
        "#!/usr/bin/env bash\nsource=\"${BASH_SOURCE[0]}\"\nprintf \"%s\\n\" \"$WORKTREE\"\n"
    );
}

/// The one exception, and its boundary: for `grep` and `glob`, exit 1 is "no
/// matches" and stays an empty array; exit 2 and above is a real failure and
/// throws like `read`.
#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn grep_with_no_match_is_an_empty_array_and_a_bad_pattern_throws() {
    let fixture = Fixture::new("grep-exit");
    fixture.write(&fixture.root.join("hit.txt"), "alpha\n");
    let session = SessionId::new("grep-exit-session");
    let mut runtime = runtime(&fixture, &session);

    // Exit 1: no matches. Still a result, and still an empty array.
    let empty = runtime.run_cell(&format!(
        "const none = await grep({{ pattern: \"NOTHINGMATCHESTHIS\", path: {path:?} \
         }});\nreturn none.length;\n",
        path = fixture.root.to_string_lossy()
    ));
    assert_eq!(returned(&empty), &Value::Number(0.0), "{empty:?}");

    // Exit 2: a pattern grep cannot compile. A failure, and a throw.
    let bad = runtime.run_cell(&format!(
        "const hits = await grep({{ pattern: \"a\\\\\", path: {path:?} }});\nreturn hits.length;\n",
        path = fixture.root.to_string_lossy()
    ));
    let error = threw(&bad);
    assert_eq!(error.class, "ToolError", "{error:?}");
    assert!(
        error.message.contains("`grep` failed with exit"),
        "{}",
        error.message
    );
}

/// Whether `name`'s binary resolves on this host, so a test that needs a
/// real `rg` or `fd` reports its absence instead of failing for it —
/// `tools.rs::installed`'s rule, spelled here because that helper is private
/// to its own file.
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn resolves(name: &str) -> bool {
    let tool =
        sterna::tools::registry::lookup(name).unwrap_or_else(|| panic!("`{name}` registered"));
    let executable = tool
        .executable()
        .unwrap_or_else(|| panic!("`{name}` is a program"));
    !sterna::tools::invoke::exec_grant(executable).fell_back_to_roots
}

/// **A pure search tool is typed as what it prints.** `rg` prints
/// `path:line:text` — `grep -r -n`'s own shape — so it returns `grep`'s
/// matches and not a process result the program has to parse out of
/// `stdout`.
#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn ripgrep_returns_the_matches_grep_returns() {
    if !resolves("rg") {
        eprintln!("skipped: ripgrep is not installed here");
        return;
    }
    let fixture = Fixture::new("rg-typed");
    fixture.write(&fixture.root.join("hit.txt"), "alpha\nNEEDLE here\nomega\n");
    let session = SessionId::new("rg-typed-session");
    let mut runtime = runtime(&fixture, &session);

    let found = runtime.run_cell(&format!(
        "const hits = await rg({{ pattern: \"NEEDLE\", path: {path:?} }});\n\
         return hits.length + \"|\" + typeof hits[0].path + \"|\" + hits[0].line + \"|\" + \
         hits[0].text + \"|\" + (hits[0].stdout === undefined);\n",
        path = fixture.root.to_string_lossy()
    ));
    assert_eq!(
        returned_string(&found),
        "1|string|2|NEEDLE here|true",
        "an rg result is a Grep.Match[], not a process result: {found:?}"
    );

    // Exit 1 is "no matches" for `rg` exactly as for `grep`: an empty array,
    // not a throw (`bindings.rs::call_failure`).
    let empty = runtime.run_cell(&format!(
        "const none = await rg({{ pattern: \"NOTHINGMATCHESTHIS\", path: {path:?} }});\n\
         return none.length;\n",
        path = fixture.root.to_string_lossy()
    ));
    assert_eq!(returned(&empty), &Value::Number(0.0), "{empty:?}");
}

/// `fd` prints one path per line, which is `glob`'s shape, so it returns a
/// string array.
#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn fd_returns_a_string_array_of_paths() {
    if !resolves("fd") {
        eprintln!("skipped: fd is not installed here");
        return;
    }
    let fixture = Fixture::new("fd-typed");
    fixture.write(&fixture.root.join("marker-one.txt"), "a\n");
    let session = SessionId::new("fd-typed-session");
    let mut runtime = runtime(&fixture, &session);

    let found = runtime.run_cell(&format!(
        "const paths = await fd({{ pattern: \"marker-one\", path: {path:?} }});\n\
         return Array.isArray(paths) + \"|\" + paths.length + \"|\" + \
         paths[0].endsWith(\"marker-one.txt\");\n",
        path = fixture.root.to_string_lossy()
    ));
    assert_eq!(
        returned_string(&found),
        "true|1|true",
        "an fd result is a string[], not a process result: {found:?}"
    );
}

// --- the epilogue: the model's own code, after `execute` ----------------

/// §2's wall clock covers the **whole** of `run_cell`, not only `execute`.
///
/// `run_cell` used to disarm the watchdog and *then* re-read every live
/// handle off the persistent scope — and reading a binding runs the model's
/// own getter or `Proxy` trap. A cell that *finished* therefore hung the
/// session forever: measured through the shipped binary with
/// `const report = { get summary() { while (true) {} } };`, killed at 75 s,
/// with no `Watchdog` thread in the process at all. Each shape below binds
/// (or `keep`s) a value whose accessor never returns, which is what makes
/// the hang the refresh's rather than the cell's.
#[test]
fn an_accessor_that_never_returns_is_stopped_after_the_cell_as_well() {
    let fixture = Fixture::new("epilogue-getter");
    let limit = Duration::from_millis(500);
    let shapes = [
        "const o = { get spin() { while (true) {} } };\n",
        "const o = { get spin() { while (true) { const x = new Array(100).fill('y'); } } };\n",
        "keep('o', { get spin() { while (true) {} } });\n",
        "const view = new Proxy({}, { get(){ while (true) {} }, ownKeys(){ return [\"a\"]; }, \
         getOwnPropertyDescriptor(){ return { enumerable: true, configurable: true }; } });\n",
    ];

    for (index, shape) in shapes.iter().enumerate() {
        let session = SessionId::new(format!("epilogue-getter-{index}"));
        let mut runtime = Runtime::with_limits(
            &fixture.profile(),
            &session,
            DEFAULT_HEAP_LIMIT_BYTES,
            limit,
        );

        let started = Instant::now();
        let outcome = runtime.run_cell(shape);
        let elapsed = started.elapsed();
        assert!(
            elapsed < limit + Duration::from_secs(2),
            "shape {index} took {elapsed:?} against a {limit:?} limit: {outcome:?}"
        );

        // And the session is still a session: the next cell runs.
        let started = Instant::now();
        let next = runtime.run_cell("return 7;\n");
        let elapsed = started.elapsed();
        assert!(
            elapsed < limit + Duration::from_secs(2),
            "shape {index}'s next cell took {elapsed:?}: {next:?}"
        );
        assert_eq!(returned(&next), &Value::Number(7.0), "shape {index}");
    }
}

/// The same hazard one cell later, which is the cost of reading every live
/// name rather than only this cell's captures: cell 2 binds nothing and adds
/// a spinning getter to a handle cell 1 declared.
#[test]
fn a_getter_a_later_cell_adds_to_an_older_handle_is_stopped_too() {
    let fixture = Fixture::new("epilogue-define");
    let session = SessionId::new("epilogue-define");
    let limit = Duration::from_millis(500);
    let mut runtime = Runtime::with_limits(
        &fixture.profile(),
        &session,
        DEFAULT_HEAP_LIMIT_BYTES,
        limit,
    );

    let first = runtime.run_cell("const o = { n: 1 };\n");
    assert!(matches!(first, CellOutcome::Yielded { .. }), "{first:?}");

    let started = Instant::now();
    let second = runtime.run_cell(
        "Object.defineProperty(o, \"spin\", { get(){ while (true) {} }, enumerable: true });\n",
    );
    let elapsed = started.elapsed();
    assert!(
        elapsed < limit + Duration::from_secs(2),
        "the refresh of an older handle took {elapsed:?}: {second:?}"
    );

    let third = runtime.run_cell("return 7;\n");
    assert_eq!(returned(&third), &Value::Number(7.0), "{third:?}");
}

/// §2's ceiling, for the allocation that cannot be satisfied at all.
///
/// The raise used to be once per cell, and V8's own rule is that answering
/// the near-heap-limit callback with the *current* limit aborts the process:
/// `new Array(100000000).fill('y')` at the shipped 256 MiB ceiling printed
/// `Fatal JavaScript out of memory: Reached heap limit` and killed the
/// process, deterministically. Many small allocations survive it — the
/// termination the first callback requested lands between the two — which is
/// why the ceiling's own test never saw this.
#[test]
fn a_single_allocation_over_the_ceiling_is_answered_and_not_a_process_abort() {
    let fixture = Fixture::new("single-alloc");
    let session = SessionId::new("single-alloc");
    let mut runtime = Runtime::with_limits(
        &fixture.profile(),
        &session,
        DEFAULT_HEAP_LIMIT_BYTES,
        Duration::from_secs(30),
    );

    let outcome = runtime.run_cell("const big = new Array(100000000).fill('y');\n");
    let error = threw(&outcome);
    assert_eq!(error.class, "RuntimeOutOfMemory", "{outcome:?}");

    // The whole point of answering rather than aborting: the task goes on.
    let after = runtime.run_cell("free(\"big\");\nreturn 7;\n");
    assert_eq!(returned(&after), &Value::Number(7.0), "{after:?}");
}

/// §2's ceiling for **external** memory: an `ArrayBuffer`'s backing store is
/// not V8 heap, so `near_heap_limit` never sees it and a 1 GiB buffer
/// against a 32 MiB ceiling used to yield with the handle live — an
/// unmetered allocator for anything model-authored.
#[test]
fn an_array_buffer_over_the_ceiling_is_out_of_memory() {
    let fixture = Fixture::new("external-memory");
    let session = SessionId::new("external-memory");
    let mut runtime = Runtime::with_limits(
        &fixture.profile(),
        &session,
        32 * 1024 * 1024,
        Duration::from_secs(20),
    );

    // A buffer inside the ceiling is ordinary work and stays ordinary work.
    let ok =
        runtime.run_cell("const small = new ArrayBuffer(1024 * 1024);\nreturn small.byteLength;\n");
    assert_eq!(returned(&ok), &Value::Number(1024.0 * 1024.0), "{ok:?}");

    let outcome = runtime.run_cell("const b = new ArrayBuffer(1024 * 1024 * 1024);\n");
    let error = threw(&outcome);
    assert_eq!(error.class, "RuntimeOutOfMemory", "{outcome:?}");

    let after = runtime.run_cell("return 7;\n");
    assert_eq!(returned(&after), &Value::Number(7.0), "{after:?}");
}

/// A ceiling the cell crossed and *survived* used to be dropped on the
/// floor: `finish` converted a heap hit into `RuntimeOutOfMemory` only under
/// `Ending::Terminated`, so `const big = new Array(50000000).fill('y')` at
/// the shipped 256 MiB ceiling was an ordinary yield — `raises = 1`, the
/// oversized handle live, nothing said. §2 says the cell fails, and it now
/// does whether or not the termination the callback requested stopped it.
#[test]
fn a_ceiling_the_cell_survived_still_fails_the_cell() {
    let fixture = Fixture::new("survived-ceiling");
    let session = SessionId::new("survived-ceiling");
    let mut runtime = Runtime::with_limits(
        &fixture.profile(),
        &session,
        DEFAULT_HEAP_LIMIT_BYTES,
        Duration::from_secs(30),
    );

    let outcome = runtime.run_cell("const big = new Array(50000000).fill('y');\n");
    let error = threw(&outcome);
    assert_eq!(error.class, "RuntimeOutOfMemory", "{outcome:?}");
    // Nothing was evicted: §2's recovery is the model's to perform.
    assert!(runtime.is_live("big"), "{}", runtime.render_handles());

    let freed = runtime.run_cell("free(\"big\");\nreturn 7;\n");
    assert_eq!(returned(&freed), &Value::Number(7.0), "{freed:?}");
}

/// A raise is permanent as far as V8 is concerned, so the ceiling has to be
/// put back: without `Runtime::restore_heap_limit` the first cell to cross it
/// would leave the whole task running against whatever that cell needed —
/// up to sixteen times the configured ceiling — and §2's ceiling would be a
/// high-water mark rather than a setting.
///
/// What this asserts is what a test can see from outside the isolate: the
/// callback is still installed and still fires after a cell has been granted
/// a raise. V8 restores a limit only as far as the *live* heap allows
/// (`remove_near_heap_limit_callback`'s own rule), so the exact number the
/// limit returns to is V8's and not this crate's; dropping the re-arm
/// altogether leaves no ceiling at all, which is what the second crossing
/// here catches.
#[test]
fn the_ceiling_still_fires_after_a_cell_has_been_granted_a_raise() {
    let fixture = Fixture::new("restore-ceiling");
    let session = SessionId::new("restore-ceiling");
    let mut runtime = Runtime::with_limits(
        &fixture.profile(),
        &session,
        32 * 1024 * 1024,
        Duration::from_secs(20),
    );

    let first = runtime.run_cell("const rows = [];\n");
    assert!(matches!(first, CellOutcome::Yielded { .. }), "{first:?}");
    let crossed = runtime.run_cell("while (true) { rows.push(new Array(1000).fill(\"y\")); }\n");
    assert_eq!(threw(&crossed).class, "RuntimeOutOfMemory", "{crossed:?}");
    let raised_once = runtime.heap_limit_raises();
    assert!(raised_once >= 1, "the ceiling was never raised");

    let recovered = runtime.run_cell("free(\"rows\");\nreturn 7;\n");
    assert_eq!(returned(&recovered), &Value::Number(7.0), "{recovered:?}");

    let again = runtime
        .run_cell("const more = [];\nwhile (true) { more.push(new Array(1000).fill(\"y\")); }\n");
    assert_eq!(threw(&again).class, "RuntimeOutOfMemory", "{again:?}");
    assert!(
        runtime.heap_limit_raises() > raised_once,
        "the near-heap-limit callback did not survive the restore: {} raises both times",
        raised_once
    );
}

/// §3's preview, §2's ranking and §9.2's result, for the two collections
/// that have no own enumerable properties at all.
///
/// All three walkers read own string keys, and a `Map`/`Set` has none: the
/// table showed an empty preview whatever the collection held, the
/// out-of-memory list ranked the `Map` that filled the heap at `~32 B`
/// behind a 16-byte counter, and `return new Set([1,2,3])` wrote `{}` as the
/// task's answer with `cut: false`.
#[test]
fn a_map_and_a_set_are_previewed_ranked_and_returned_by_what_they_hold() {
    let fixture = Fixture::new("collections");
    let session = SessionId::new("collections");
    let mut runtime = runtime(&fixture, &session);

    let first =
        runtime.run_cell("const s = new Set([1, 2, 3]);\nconst m = new Map([[\"a\", 1]]);\n");
    assert!(matches!(first, CellOutcome::Yielded { .. }), "{first:?}");
    let table = runtime.render_handles();
    assert!(
        table
            .lines()
            .any(|line| line.starts_with("s") && line.contains("Set")),
        "a Set renders as a Set: {table}"
    );
    assert!(
        table.contains("size=3"),
        "a Set's preview carries its size: {table}"
    );
    assert!(
        table.contains("size=1"),
        "a Map's preview carries its size: {table}"
    );

    // §9.2's result: a Set is an array and a Map is an object.
    let set = runtime.run_cell("return new Set([1, 2, 3]);\n");
    let CellOutcome::Returned { terminal, .. } = &set else {
        panic!("expected a return, got {set:?}");
    };
    assert_eq!(terminal.render(), "[1,2,3]", "{set:?}");

    let map_session = SessionId::new("collections-map");
    let mut map_runtime = Runtime::new(&fixture.profile(), &map_session);
    let map = map_runtime.run_cell("return new Map([[\"a\", 1], [\"b\", 2]]);\n");
    let CellOutcome::Returned { terminal, .. } = &map else {
        panic!("expected a return, got {map:?}");
    };
    assert_eq!(terminal.render(), "{\"a\":1,\"b\":2}", "{map:?}");
}

/// Cell 4 of session tls9up-7rz, through the real isolate: an excerpt, a
/// count and a few lines returned together arrive as those three things,
/// whole, within the return budget -- not as 2 KiB of JSON and a type
/// preview (ruled 2026-09-23).
#[cfg(unix)]
#[test]
fn a_returned_object_keeps_its_fields_apart_and_an_excerpt_arrives_whole() {
    let fixture = Fixture::new("fields");
    let body: String = (1..=240)
        .map(|n| format!("line {n} of the readme, with enough words to be worth reading\n"))
        .collect();
    let file = fixture.write(&fixture.root.join("README.md"), &body);
    let session = SessionId::new("fields-session");
    let mut runtime = runtime(&fixture, &session);
    let program = format!(
        "const doc = await read({{ path: {path:?} }});\n\
         return {{ readme: doc.excerpt({{ start: 1, lines: 240 }}), count: doc.lineCount, \
         head: doc.lines.slice(0, 2) }};\n",
        path = file.to_string_lossy()
    );
    let outcome = runtime.run_cell(&program);
    let CellOutcome::Returned { terminal, .. } = &outcome else {
        panic!("expected a return, got {outcome:?}");
    };
    let sterna::runtime::outcome::Terminal::Fields(fields) = terminal else {
        panic!("an object keeps its fields apart: {terminal:?}");
    };
    assert_eq!(
        fields.iter().map(|f| f.name.as_str()).collect::<Vec<_>>(),
        ["readme", "count", "head"]
    );
    assert!(fields.iter().all(|f| f.whole), "{fields:?}");
    let rendered = terminal.render_within(24_000);
    assert!(rendered.paged.is_empty(), "{:?}", rendered.paged);
    assert!(
        rendered
            .text
            .starts_with("### readme\n[lines 1-240 of 240]\n  1 | line 1 of the readme"),
        "{}",
        rendered.text
    );
    assert!(
        rendered.text.contains("\n240 | line 240 of the readme"),
        "{}",
        rendered.text
    );
    assert!(
        rendered
            .text
            .contains("[end of file]\n\ncount: 240\n### head\nline 1 of the readme"),
        "{}",
        rendered.text
    );
    assert!(
        rendered.tokens > 3_000 && rendered.tokens < 6_000,
        "{}",
        rendered.tokens
    );

    // Squeezed, the excerpt is paged at a line and the cursor names the
    // next start; the count beside it is untouched.
    let paged = terminal.render_within(1_000);
    assert_eq!(paged.paged, vec!["readme".to_string()]);
    assert!(
        paged
            .text
            .contains("lines not shown · call .excerpt({start: "),
        "{}",
        paged.text
    );
    assert!(paged.text.contains("\ncount: 240\n"), "{}", paged.text);
}

/// The ranking half of the same defect, across a cell boundary: the `Map`
/// that filled a 32 MiB heap must be the handle the model is told to free.
#[test]
fn the_out_of_memory_ranking_counts_what_a_map_holds() {
    let fixture = Fixture::new("map-ranking");
    let session = SessionId::new("map-ranking");
    let mut runtime = Runtime::with_limits(
        &fixture.profile(),
        &session,
        32 * 1024 * 1024,
        Duration::from_secs(20),
    );

    let first = runtime.run_cell("const m = new Map();\n");
    assert!(matches!(first, CellOutcome::Yielded { .. }), "{first:?}");

    let outcome = runtime
        .run_cell("let i = 0;\nwhile (true) { m.set(i, new Array(1000).fill('y')); i++; }\n");
    let error = threw(&outcome);
    assert_eq!(error.class, "RuntimeOutOfMemory", "{error:?}");
    assert!(
        error.message.contains("Largest live handles: m ("),
        "the Map that filled the heap must rank first: {}",
        error.message
    );
    let sized = error
        .message
        .split("Largest live handles: m (~")
        .nth(1)
        .and_then(|rest| rest.split(" B)").next())
        .and_then(|digits| digits.parse::<u64>().ok())
        .unwrap_or_else(|| panic!("no size for `m`: {}", error.message));
    assert!(
        sized > 1024 * 1024,
        "the Map filled a 32 MiB heap and was ranked at {sized} B: {}",
        error.message
    );
}

/// §5's position, for the throw a model-written traversal produces most
/// often. V8 reports a stack overflow as present-and-zero, so
/// `error.line.zip(error.column)` was `Some((0, 0))` and the model was
/// handed `line 0, column 0` — a place `ErrorSection::position`'s own doc
/// comment says is never written — three times over.
#[test]
fn a_stack_overflow_names_no_position_and_no_zero_frame() {
    let fixture = Fixture::new("overflow");
    let session = SessionId::new("overflow");
    let mut runtime = runtime(&fixture, &session);

    let outcome = runtime.run_cell("function f(n) { return f(n + 1); }\nf(0);\n");
    let error = threw(&outcome);
    assert_eq!(error.class, "RangeError", "{error:?}");
    assert!(
        !matches!((error.line, error.column), (Some(0), Some(0))),
        "(0, 0) is not a place: {error:?}"
    );
    for frame in &error.stack {
        assert!(
            !frame.description.contains("line 0, column 0"),
            "a frame naming a place that does not exist: {error:?}"
        );
    }

    // And the session goes on.
    let after = runtime.run_cell("return 7;\n");
    assert_eq!(returned(&after), &Value::Number(7.0), "{after:?}");
}

// --- GH-PANE-61G-DELIVERY-AND-BG: §4's delivery into the model's scope ---

use sterna::events::batch::Batch;
use sterna::events::window::{Window, WindowConfig};
use sterna::events::{Event, Kind, PayloadRef, Priority, Stamp};

/// One closed batch carrying `n` `bg.done` events, built through a window so
/// what the runtime is handed is what a session would hand it.
fn closed_batch(n: u64) -> Batch {
    let mut window = Window::new(WindowConfig::default());
    for i in 0..n {
        window.accept(
            Event::pending(
                Kind::BgDone {
                    emission: format!("exit-{i}"),
                },
                format!("bg/job{i}"),
                Stamp::from_millis(0),
                PayloadRef::new(format!("job{i}#exit")),
                Priority::Batch,
                format!("job{i} finished"),
            ),
            Stamp::from_millis(0),
        );
    }
    window
        .close_if_due(Stamp::from_millis(3_000))
        .expect("a window whose deadline has passed closes")
}

/// `events-contract.md` §4: the batch extends the handle table with exactly
/// one row, named `batch`, **always last** — after three names the model
/// itself bound — and a second delivery replaces and frees the first.
#[test]
fn a_delivered_batch_is_the_tables_last_row_and_the_next_one_replaces_it() {
    let fixture = Fixture::new("delivery");
    let session = SessionId::new("delivery-session");
    let mut runtime = runtime(&fixture, &session);

    let cell = runtime.run_cell("const one = 1;\nconst two = 2;\nconst three = 3;\n");
    assert!(matches!(cell, CellOutcome::Yielded { .. }), "{cell:?}");
    assert_eq!(runtime.handle_names(), vec!["one", "two", "three"]);

    assert!(
        runtime.deliver_batch(closed_batch(2)).is_none(),
        "the first delivery replaced a batch that never existed"
    );
    assert_eq!(
        runtime.handle_names(),
        vec!["one", "two", "three", "batch"],
        "the batch row is not last, so the model's own bindings lost the order it made them in"
    );

    // A second delivery frees the first and says where.
    let previous = runtime
        .deliver_batch(closed_batch(1))
        .expect("the second delivery hands back the batch it replaced");
    assert_eq!(
        previous.n, 2,
        "the wrong batch was handed back to be rolled"
    );
    assert_eq!(runtime.handle_names(), vec!["one", "two", "three", "batch"]);
    let rendered = runtime.render_handles();
    assert_eq!(
        rendered.matches("Events.Batch").count(),
        1,
        "two batch rows are live:\n{rendered}"
    );
    assert!(
        rendered.contains("(replaced at cell 1)"),
        "the replacement was not announced:\n{rendered}"
    );
}

/// §4's three methods, called from the model's own program: `batch.where`,
/// `batch.ack` and `batch.rest` are `Batch::where_`, `ack` and `rest` behind
/// a binding, and the batch is the one name the runtime declares in a scope
/// the model otherwise owns entirely.
#[test]
fn the_model_calls_where_ack_and_rest_on_the_batch_it_was_given() {
    let fixture = Fixture::new("batch-api");
    let session = SessionId::new("batch-api-session");
    let mut runtime = runtime(&fixture, &session);
    runtime.deliver_batch(closed_batch(3));

    let seen = runtime.run_cell(
        "const all = batch.where({});\n\
         const mine = batch.where({source: \"bg/job1\"});\n\
         return `${batch.n}/${all.length}/${mine.length}/${all[0].kind}`;\n",
    );
    assert_eq!(returned_string(&seen), "3/3/1/bg.done");

    // `ack` takes the ids the model itself read off the events, and `rest` is
    // what it did not ack. An id no batch holds comes back as unknown rather
    // than being silently dropped.
    let acked = runtime.run_cell(
        "const acked = batch.ack([batch.where({})[0].id, 9999]);\n\
         return `${acked.acked.length}/${acked.unknown[0]}/${batch.rest().length}`;\n",
    );
    assert_eq!(returned_string(&acked), "1/9999/2");
}

/// §5's refusal, seen from inside the model's own program: a `bg.run` outside
/// the grant is a `PermissionDenied` **at the call**, and the handle table
/// has no new row afterwards.
#[test]
fn a_bg_run_outside_the_grant_throws_before_any_handle_exists() {
    let fixture = Fixture::new("bg-denied");
    let session = SessionId::new("bg-denied-session");
    let mut runtime = runtime(&fixture, &session);

    let before = runtime.handle_names();
    let outcome = runtime.run_cell("const job = bg.run(\"curl https://example.com\");\n");
    let error = threw(&outcome);
    assert_eq!(error.class, "PermissionDenied", "{error:?}");
    assert_eq!(
        runtime.handle_names(),
        before,
        "a refused bg.run left a handle behind"
    );
    assert!(!runtime.is_live("job"), "the refusal minted a job handle");
}

/// `bg` is the isolate's own, like every other host function: a program
/// cannot replace it and so cannot lose the only way it has to stop what it
/// started.
#[test]
fn bg_is_a_host_object_a_program_cannot_replace() {
    let fixture = Fixture::new("bg-fixed");
    let session = SessionId::new("bg-fixed-session");
    let mut runtime = runtime(&fixture, &session);

    let shape =
        runtime.run_cell("return `${typeof bg.run}/${typeof bg.watch}/${typeof bg.cancel}`;\n");
    assert_eq!(returned_string(&shape), "function/function/function");

    // A sloppy-mode store on a non-writable property does nothing, and
    // `delete` of a non-deletable one answers false; either way `bg` is still
    // the host's.
    let kept = runtime.run_cell(
        "try { globalThis.bg = 1; } catch (e) {}\n\
         const gone = delete globalThis.bg;\n\
         return `${gone}/${typeof bg.run}`;\n",
    );
    assert_eq!(returned_string(&kept), "false/function");
}

/// `runtime-contract.md` §2's third lifetime event reaches the batch too: the
/// task ending frees it with every other handle.
#[test]
fn ending_the_task_frees_the_batch_with_the_rest() {
    let fixture = Fixture::new("batch-end");
    let session = SessionId::new("batch-end-session");
    let mut runtime = runtime(&fixture, &session);
    runtime.run_cell("const kept = 1;\n");
    runtime.deliver_batch(closed_batch(1));
    assert!(runtime.is_live("batch"));

    runtime.end_task();
    assert!(
        runtime.handle_names().is_empty(),
        "{:?}",
        runtime.handle_names()
    );
    assert!(!runtime.is_live("batch"));
}

// --- the epilogue's budget is the epilogue's, not each handle's --------

/// Thirty handles, each an ordinary lazy accessor doing 100 ms of honest
/// work and returning a number — a program with no loop in it — at the
/// **shipped** `DEFAULT_CELL_WALL_CLOCK_LIMIT`, and the next cell runs.
///
/// `Runtime::refresh_previews` enters V8 once per name, so a read the
/// epilogue watchdog stopped cost about one `TERMINATE_RETRY_INTERVAL` and
/// the next name started the clock again: the epilogue's cost was linear in
/// the number of live handles while its budget was a constant. Measured at
/// `a0186fa` through this shape and through the shipped binary: cell 1 was
/// reported to the model as `yielded in 7881 ms`, and cell 2 and every cell
/// after it threw `RuntimePoisoned` — the task over, for a program that had
/// finished, with a diagnostic blaming the program for Sterna's own epilogue.
///
/// The wall clock is the shipped one on purpose: a shortened limit is what
/// let this through, because it makes the *cell* the thing that stops.
#[test]
fn thirty_lazy_accessors_do_not_cost_the_isolate_its_trust() {
    let fixture = Fixture::new("thirty-accessors");
    let session = SessionId::new("thirty-accessors");
    let mut runtime = runtime(&fixture, &session);

    let mut source = String::new();
    for index in 0..30 {
        source.push_str(&format!(
            "const h{index} = {{ get total() {{ const t = Date.now(); while (Date.now() - t < \
             100) {{}} return {index}; }} }};\n"
        ));
    }
    let first = runtime.run_cell(&source);
    assert!(
        matches!(first, CellOutcome::Yielded { .. }),
        "the program finished; it must be answered as having finished: {first:?}"
    );

    // The whole of the claim: the epilogue is bounded by one budget, so the
    // isolate is still one this runtime enters.
    let second = runtime.run_cell("1 + 1;\n");
    assert!(
        matches!(second, CellOutcome::Yielded { .. }),
        "the next cell must run: {second:?}"
    );
    assert!(
        second.turn().elapsed_ms < 3_000,
        "the epilogue's cost must be the epilogue's, not thirty handles': {} ms",
        second.turn().elapsed_ms
    );
    let third = runtime.run_cell("return 7;\n");
    assert_eq!(returned(&third), &Value::Number(7.0), "{third:?}");
}

// --- §2's ceiling, observed rather than reported -----------------------

/// A crossing the near-heap-limit callback never reports is still a
/// crossing, and it fails **that** cell.
///
/// V8 satisfies a large-object allocation without ever invoking the
/// callback, so `finish`'s `stopped.heap_hit` — a crossing the callback
/// *reported* — is false for it. Measured at `a0186fa`: `new
/// Array(12000000).fill('y')`, a 96 MB array under a 32 MiB ceiling, yielded
/// in 11 ms with `raises = 0`, the oversized handle live and nothing said,
/// and two cells later an ordinary 4 MB allocation killed the process. §2
/// says the cell fails.
#[test]
fn a_crossing_the_callback_never_reported_still_fails_that_cell() {
    let fixture = Fixture::new("observed-crossing");
    let session = SessionId::new("observed-crossing");
    let mut runtime = Runtime::with_limits(
        &fixture.profile(),
        &session,
        32 * 1024 * 1024,
        Duration::from_secs(30),
    );

    let outcome = runtime.run_cell("const big = new Array(12000000).fill('y');\n");
    let error = threw(&outcome);
    assert_eq!(error.class, "RuntimeOutOfMemory", "{outcome:?}");
    assert_eq!(
        runtime.heap_limit_raises(),
        0,
        "V8 never reported this crossing; the runtime observed it, and that is the whole point \
         of the test"
    );
    assert!(
        error.message.contains("big"),
        "the model is told what to free: {}",
        error.message
    );
    // Nothing is evicted (§2), and the ceiling fails the cell that crossed
    // it rather than every cell after it — the model still has a task.
    assert!(runtime.is_live("big"), "{}", runtime.render_handles());
    let after = runtime.run_cell("return 7;\n");
    assert_eq!(returned(&after), &Value::Number(7.0), "{after:?}");
}

/// The same crossing, one cell later: a heap left far above the ceiling used
/// to kill the **process** on the next ordinary allocation.
///
/// Measured at `a0186fa`, deterministically: a 32 MiB ceiling,
/// `new Array(20000000).fill('y')` yielding at 19 ms with `raises = 0`, then
/// `11 + 11`, then a 4 MB allocation — two `Mark-Compact … last resort`
/// collections at 152.8 MB and `Fatal JavaScript out of memory: Reached heap
/// limit`, exit 133. The heap was at 4.8× the ceiling, *inside*
/// `HEAP_RAISE_TOTAL_MULTIPLE`, because the raise the third cell asked for
/// was granted in doublings from the configured ceiling and never reached
/// the live heap.
#[test]
fn a_heap_over_the_ceiling_does_not_kill_the_process_on_a_later_cell() {
    let fixture = Fixture::new("no-abort");
    let session = SessionId::new("no-abort");
    let mut runtime = Runtime::with_limits(
        &fixture.profile(),
        &session,
        32 * 1024 * 1024,
        Duration::from_secs(30),
    );

    let first = runtime.run_cell("const big = new Array(20000000).fill('y');\n");
    assert_eq!(threw(&first).class, "RuntimeOutOfMemory", "{first:?}");
    let second = runtime.run_cell("11 + 11;\n");
    assert!(matches!(second, CellOutcome::Yielded { .. }), "{second:?}");
    // The cell that used to abort the process. Whatever it is answered with,
    // it is answered.
    let third = runtime.run_cell("const half = new Array(1048576).fill('z');\n");
    assert!(
        matches!(
            third,
            CellOutcome::Yielded { .. } | CellOutcome::Threw { .. }
        ),
        "{third:?}"
    );
    let fourth = runtime.run_cell("return 7;\n");
    assert_eq!(returned(&fourth), &Value::Number(7.0), "{fourth:?}");

    // The same shape where the live heap is past the raise bound itself: a
    // 240 MB array under a 16 MiB ceiling is 15x `HEAP_RAISE_TOTAL_MULTIPLE`'s
    // configured multiple, so the grant reaches the bound and the bound is
    // still under the heap. `heap_grant` floors the bound at twice what
    // `observe_heap_ceiling` last measured, and without that floor this cell
    // takes the process with it.
    let fixture = Fixture::new("no-abort-past-bound");
    let session = SessionId::new("no-abort-past-bound");
    let mut runtime = Runtime::with_limits(
        &fixture.profile(),
        &session,
        16 * 1024 * 1024,
        Duration::from_secs(30),
    );
    let first = runtime.run_cell("const big = new Array(30000000).fill('y');\n");
    assert_eq!(threw(&first).class, "RuntimeOutOfMemory", "{first:?}");
    let second = runtime.run_cell("11 + 11;\n");
    assert!(matches!(second, CellOutcome::Yielded { .. }), "{second:?}");
    let third = runtime.run_cell("const half = new Array(1048576).fill('z');\n");
    assert!(
        matches!(
            third,
            CellOutcome::Yielded { .. } | CellOutcome::Threw { .. }
        ),
        "{third:?}"
    );
}

/// An over-ceiling allocation made by a **preview getter** is the model's
/// own code and is answered in the cell whose table was being read.
///
/// Measured at `a0186fa`: `const o = { get big() { return new
/// Array(20000000).fill('y'); } };` under an 8 MiB ceiling — a 160 MB
/// allocation made by the runtime's own epilogue — yielded in 54 ms with
/// nothing said, and re-allocated on every later cell's refresh.
#[test]
fn an_over_ceiling_allocation_inside_a_preview_getter_is_answered() {
    let fixture = Fixture::new("getter-ceiling");
    let session = SessionId::new("getter-ceiling");
    let mut runtime = Runtime::with_limits(
        &fixture.profile(),
        &session,
        8 * 1024 * 1024,
        Duration::from_secs(10),
    );

    let outcome =
        runtime.run_cell("const o = { get big() { return new Array(20000000).fill('y'); } };\n");
    assert_eq!(threw(&outcome).class, "RuntimeOutOfMemory", "{outcome:?}");
    let after = runtime.run_cell("return 9;\n");
    assert_eq!(returned(&after), &Value::Number(9.0), "{after:?}");
}

/// A hundred one-megabyte buffers allocated and dropped in one cell is a
/// program that never holds more than a megabyte, and it is not a refusal.
///
/// `ExternalMemory::release` runs only when V8 frees a backing store, which
/// needs a collection — and V8 asks for one itself, retrying the *same size*
/// after collecting when the allocator answers null. Latching on that first
/// null made an `ArrayBuffer` single-use up to the ceiling for the life of a
/// task: measured at `a0186fa`, this cell answered `RuntimeOutOfMemory`, and
/// so did four consecutive cells each allocating and dropping 24 MiB.
///
/// The second half is the other direction, and it is why the fix is a
/// `settle` and not a deletion: a refusal nothing satisfied still fails the
/// cell, whatever the program did with the `RangeError` afterwards.
#[test]
fn a_buffer_allocated_and_dropped_a_hundred_times_is_not_a_refusal() {
    let fixture = Fixture::new("external-live");
    let session = SessionId::new("external-live");
    let mut runtime = Runtime::with_limits(
        &fixture.profile(),
        &session,
        32 * 1024 * 1024,
        Duration::from_secs(30),
    );

    let outcome = runtime.run_cell(
        "let n = 0;\nfor (let i = 0; i < 100; i++) { const b = new ArrayBuffer(1024 * 1024); n += \
         b.byteLength; }\nn;\n",
    );
    assert!(
        matches!(outcome, CellOutcome::Yielded { .. }),
        "live external memory never exceeded 1 MiB under a 32 MiB ceiling: {outcome:?}"
    );

    let refused = runtime.run_cell("const huge = new ArrayBuffer(1024 * 1024 * 1024);\n");
    let error = threw(&refused);
    assert_eq!(error.class, "RuntimeOutOfMemory", "{refused:?}");
    assert!(
        error.message.contains("ArrayBuffer"),
        "the model is told which ceiling: {}",
        error.message
    );

    // And a refusal the cell caught is still the cell's, which is the
    // property `settle` had to keep while it stopped latching on a retry.
    let caught = runtime.run_cell(
        "let caught = false;\ntry { const h = new ArrayBuffer(1024 * 1024 * 1024); } catch (e) { \
         caught = true; }\nconst tiny = new ArrayBuffer(16);\n",
    );
    assert_eq!(threw(&caught).class, "RuntimeOutOfMemory", "{caught:?}");
}

// --- every binding the isolate installs is a binding the model is told about

/// The defect this pins, observed 2026-09-06: `bg.run`, `bg.watch`,
/// `bg.cancel`, `keep`, `free` and `handles` were bound in the isolate and
/// shipped for a whole sub-phase while the system block declared only the
/// four tools, so 61G's background jobs and monitors were unreachable by the
/// only caller they have.
///
/// It enumerates the **real** globals out of a real isolate rather than
/// trusting a list: a host binding is installed non-writable and
/// non-configurable, which is exactly what distinguishes it from a language
/// built-in, so a binding added without a declaration fails here.
#[test]
fn every_host_global_is_declared_to_the_model() {
    let fixture = Fixture::new("declared");
    let session = SessionId::new("declared-session");
    let mut runtime = runtime(&fixture, &session);

    let outcome = runtime.run_cell(
        "const names = Object.getOwnPropertyNames(globalThis).filter(n => {\n\
         \x20 const d = Object.getOwnPropertyDescriptor(globalThis, n);\n\
         \x20 return d && d.writable === false && d.configurable === false;\n\
         });\n\
         return names.join(\",\");\n",
    );
    let listed = returned_string(&outcome);
    let installed: Vec<&str> = listed
        .split(',')
        .filter(|name| !name.is_empty())
        .filter(|name| !sterna::prompt::declarations::LANGUAGE_CONSTANTS.contains(name))
        .collect();

    assert!(
        installed.contains(&"bg"),
        "the enumeration found no host bindings at all, so it proves nothing: {listed:?}"
    );
    let undeclared: Vec<&&str> = installed
        .iter()
        .filter(|name| !sterna::prompt::declarations::declares_global(name))
        .collect();
    assert!(
        undeclared.is_empty(),
        "these globals are bound in the isolate and declared nowhere in the system block, so \
         the model cannot use them: {undeclared:?}"
    );
}

/// The `Runtime` block names each of them, so the declaration table being
/// complete is not the same claim as the prompt carrying it.
#[test]
fn the_runtime_block_carries_every_non_tool_binding() {
    let block = sterna::prompt::render_runtime();
    for binding in sterna::prompt::declarations::RUNTIME {
        assert!(
            block.contains(binding.declaration),
            "`{}` is declared in the table but absent from the rendered block",
            binding.global
        );
    }
    // Each member by its rendered form, so a declaration that lost one --
    // `bg` without `cancel` leaves a program unable to stop what it started --
    // fails here rather than at the model.
    for fragment in [
        "declare const bg: {",
        "run(command: string",
        "watch(command: string",
        "cancel(job: Job | string)",
        "declare const batch: {",
        "where(query: {kind?: string; source?: string})",
        "ack(ids: number[])",
        "rest(): Event[]",
        "declare function handles(): string[];",
        "declare function keep(",
        "declare function free(",
    ] {
        assert!(
            block.contains(fragment),
            "the runtime block never declares `{fragment}`"
        );
    }
}

// --- todo: the model's own plan ----------------------------------------

/// A plan is only worth writing if it is still there next cell, and only
/// worth having if the person and the next turn can both see it.
#[test]
fn a_plan_written_in_one_cell_is_readable_in_the_next_and_rides_the_turn() {
    let fixture = Fixture::new("plan");
    let session = SessionId::new("plan-session");
    let mut runtime = runtime(&fixture, &session);

    let first = runtime.run_cell(
        "todo.write([\n\
         \x20 {text: \"read the file\", status: \"done\"},\n\
         \x20 {text: \"write the patch\", status: \"active\"},\n\
         \x20 {text: \"run the tests\", status: \"pending\"},\n\
         ]);\n",
    );
    let CellOutcome::Yielded { turn } = &first else {
        panic!("expected a yield, got {first:?}");
    };
    assert_eq!(turn.plan.len(), 3, "the plan did not ride the turn");
    assert_eq!(turn.plan[1].text, "write the patch");
    assert_eq!(turn.plan[1].status.as_str(), "active");

    let second = runtime.run_cell(
        "const plan = todo.read();\nreturn plan.map(i => i.status + \":\" + i.text).join(\"|\");\n",
    );
    assert_eq!(
        returned_string(&second),
        "done:read the file|active:write the patch|pending:run the tests",
        "the plan did not survive into the next cell"
    );
}

/// A status outside the three is refused **and changes nothing**: a program
/// that catches the throw still holds the plan it had, rather than a
/// half-written one.
#[test]
fn an_unknown_status_is_refused_and_leaves_the_previous_plan_standing() {
    let fixture = Fixture::new("plan-refuse");
    let session = SessionId::new("plan-refuse-session");
    let mut runtime = runtime(&fixture, &session);

    runtime.run_cell("todo.write([{text: \"the only step\", status: \"active\"}]);\n");
    let outcome = runtime.run_cell(
        "let refused = \"no\";\n\
         try { todo.write([{text: \"a\", status: \"done\"}, {text: \"b\", status: \"blocked\"}]); }\n\
         catch (e) { refused = String(e.message || e); }\n\
         const plan = todo.read();\n\
         return refused + \" // \" + plan.length + \" // \" + plan[0].text;\n",
    );
    let answer = returned_string(&outcome);
    assert!(
        answer.contains("blocked"),
        "the refusal did not name the bad status: {answer}"
    );
    assert!(
        answer.ends_with("// 1 // the only step"),
        "a refused write changed the plan: {answer}"
    );
}

/// The plan is the shape of the task in hand, so it goes when the task does —
/// a next task inheriting the last one's checklist would report work it never
/// did.
#[test]
fn the_plan_is_cleared_when_the_task_ends() {
    let fixture = Fixture::new("plan-task");
    let session = SessionId::new("plan-task-session");
    let mut runtime = runtime(&fixture, &session);

    runtime.run_cell("todo.write([{text: \"first task step\", status: \"active\"}]);\n");
    runtime.end_task();
    let outcome = runtime.run_cell("return String(todo.read().length);\n");
    assert_eq!(returned_string(&outcome), "0");
}

/// `## Plan` reaches the model's own result message, which is what keeps a
/// long task on its checklist; an empty plan writes no section at all.
#[test]
fn the_result_message_carries_the_plan_and_omits_it_when_empty() {
    use sterna::prompt::{Budget, CellResult};
    use sterna::runtime::outcome::{PlanItem, PlanStatus};

    let base = |plan: Vec<PlanItem>| CellResult {
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
            task_used: 1,
            task_cap: 400_000,
            cells_used: 1,
            cells_cap: Some(40),
            feedback: None,
        },
        plan,
    };

    let rendered = sterna::prompt::render_result(&base(vec![
        PlanItem {
            text: "read the file".to_string(),
            status: PlanStatus::Done,
        },
        PlanItem {
            text: "write the patch".to_string(),
            status: PlanStatus::Active,
        },
    ]));
    assert!(
        rendered.contains("## Plan\n[x] read the file\n[~] write the patch"),
        "{rendered}"
    );
    assert!(!sterna::prompt::render_result(&base(Vec::new())).contains("## Plan"));
}

/// The end of the chain, through the real sandbox: a script in the project
/// actually runs. Before the exec grant followed the command grant this
/// exited 126 — the shell was admitted and could exec nothing but itself.
#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn a_project_script_runs_when_every_command_line_is_admitted() {
    let fixture = Fixture::new("script-exec");
    let script = fixture.root.join("say.sh");
    fixture.write(&script, "#!/bin/sh\necho ran-inside-the-sandbox\n");
    std::fs::set_permissions(
        &script,
        <std::fs::Permissions as std::os::unix::fs::PermissionsExt>::from_mode(0o755),
    )
    .unwrap();
    let session = SessionId::new("script-exec-session");
    let root = fixture.root.to_string_lossy().replace('\\', "/");
    let profile = fixture.profile_with(&format!(
        r#"{{"permissions":{{"allow":["Read({root}/**)","Write({root}/**)","Bash"]}}}}"#
    ));
    let mut runtime = Runtime::new(&profile, &session);

    let outcome = runtime.run_cell(
        "const r = await bash({command: \"./say.sh\"});\nreturn r.stdout.trim() + \" exit=\" + r.exit_code;\n",
    );
    assert_eq!(
        returned_string(&outcome),
        "ran-inside-the-sandbox exit=0",
        "a project script could not be executed"
    );
}

// --- a name that is not defined, caught before the cell runs ------------

/// The turn this saves: a typo used to cost a whole round trip — the cell
/// ran, V8 threw `ReferenceError`, and the model only read it in the next
/// result. Now nothing runs and the answer names the line.
#[test]
fn an_undefined_name_is_reported_before_the_cell_runs() {
    let fixture = Fixture::new("undefined-name");
    let session = SessionId::new("undefined-name-session");
    let mut runtime = runtime(&fixture, &session);

    let outcome = runtime.run_cell("const n = 1;\nreturn totl + n;\n");
    let error = threw(&outcome);
    assert_eq!(error.class, "ReferenceError");
    assert!(error.message.contains("`totl` is not defined"), "{error:?}");
    assert!(
        error.message.contains("use `write`/`edit` line arrays"),
        "the deterministic recovery path must be in the same error: {error:?}"
    );
    assert_eq!(error.line, Some(2), "the model's own line");
    assert!(
        outcome.turn().record.calls.is_empty(),
        "the cell ran despite naming something undefined"
    );
}

/// The three shapes that are legal and must never be accused, because a false
/// positive has the model "fix" code that was right.
#[test]
fn legal_free_names_are_never_accused() {
    let fixture = Fixture::new("free-names-ok");
    let session = SessionId::new("free-names-ok-session");
    let mut runtime = runtime(&fixture, &session);

    // A non-enumerable built-in. `Set`, `JSON` and `Math` are non-enumerable
    // on `globalThis` by specification, and the first version of this check
    // enumerated only enumerable properties and accused every one of them.
    let built_ins = runtime.run_cell(
        "const s = new Set([1, 2, 2]);\nreturn JSON.stringify([s.size, Math.max(1, 2)]);\n",
    );
    assert_eq!(returned_string(&built_ins), "[2,2]");

    // A handle bound by an earlier cell.
    runtime.run_cell("const kept = [1, 2, 3];\n");
    let across = runtime.run_cell("return String(kept.length);\n");
    assert_eq!(returned_string(&across), "3");

    // `typeof x` is the one place an undefined name is legal, and it is how
    // a cell asks whether a handle it freed is really gone.
    let probe = runtime.run_cell("return typeof neverBoundAnywhere;\n");
    assert_eq!(returned_string(&probe), "undefined");

    // A host function, and a name declared later in the same cell.
    let hoisted =
        runtime.run_cell("const r = later();\nfunction later() { return \"ok\"; }\nreturn r;\n");
    assert_eq!(returned_string(&hoisted), "ok");
}

// --- subagents: Phase 64 ------------------------------------------------

/// The two refusals Phase 64 names, and both happen **before a handle
/// exists**: a program that catches either is holding nothing, and there is no
/// started subagent to stop.
#[test]
fn a_subagent_may_not_start_a_subagent() {
    let fixture = Fixture::new("agent-depth");
    let session = SessionId::new("agent-depth-session");
    let mut parent = runtime(&fixture, &session).with_agents(
        sterna::config::SternaConfig::parse("[agents]\nmodel='claude-sonnet-5'")
            .unwrap()
            .agents,
    );

    // The parent may ask: it gets a handle, and the handle names its source.
    let started = parent
        .run_cell("const a = agent.run(\"count the files\");\nreturn a.id + \" \" + a.source;\n");
    let answer = returned_string(&started);
    assert!(
        answer.contains("agent/"),
        "no agent handle came back: {answer}"
    );
    assert_eq!(started.turn().record.calls[0].tool, "agent.run");
    assert!(started.turn().record.calls[0].args["source"].starts_with("agent/"));

    // A subagent's own runtime may not.
    let mut child = Runtime::new(&fixture.profile(), &session).as_subagent();
    let refused = child.run_cell(
        "try { agent.run(\"and again\"); return \"no throw\"; }\ncatch (e) { return e.name + \": \" + e.message; }\n",
    );
    let text = returned_string(&refused);
    assert!(text.starts_with("PermissionDenied"), "{text}");
    assert!(text.contains("may not start a subagent"), "{text}");
}

/// **A parent can look in on a running subagent**, which nothing let it do
/// before (user, 2026-09-17: *"A parent model should check on a subagent from
/// some time. But even Claude Code does not do that."*). The look is a board
/// read: no provider request, no waiting, and `null` for a handle that has
/// none.
#[test]
fn a_parent_can_look_in_on_a_running_subagent() {
    let fixture = Fixture::new("agent-progress");
    let session = SessionId::new("agent-progress-session");
    let mut runtime = runtime(&fixture, &session).with_agents(
        sterna::config::SternaConfig::parse("[agents]\nmodel='claude-sonnet-5'")
            .unwrap()
            .agents,
    );
    runtime.set_task_context(400_000, "claude-sonnet-5");

    let looked = runtime.run_cell(
        "const job = agent.run(\"anything\");\n\
         const seen = job.progress();\n\
         return JSON.stringify({\n\
           turns: typeof seen.turns,\n\
           calls: Array.isArray(seen.calls),\n\
           elapsed: typeof seen.elapsed_ms,\n\
           running: seen.running,\n\
         });\n",
    );
    let seen = returned_string(&looked);
    assert!(seen.contains("\"turns\":\"number\""), "{seen}");
    assert!(seen.contains("\"calls\":true"), "{seen}");
    assert!(seen.contains("\"elapsed\":\"number\""), "{seen}");
    assert!(seen.contains("\"running\":true"), "{seen}");

    // A command job is not a subagent and has no turns to report.
    let none = runtime.run_cell(
        "const job = agent.run(\"another\");\n\
         bg.cancel(job);\n\
         return typeof job.progress();\n",
    );
    assert_eq!(returned_string(&none), "object");

    sterna::bg::shutdown(&session);
}

/// A task that cannot pay for a subagent is refused one, rather than starting
/// one it would have to kill halfway — which spends the tokens and produces
/// nothing.
#[test]
fn a_budget_that_cannot_pay_refuses_the_subagent_before_it_starts() {
    let fixture = Fixture::new("agent-budget");
    let session = SessionId::new("agent-budget-session");
    let mut runtime = runtime(&fixture, &session).with_agents(
        sterna::config::SternaConfig::parse("[agents]\nmodel='claude-sonnet-5'")
            .unwrap()
            .agents,
    );

    // A budget below one turn's ceiling.
    runtime.set_task_context(10, "claude-sonnet-5");
    let refused = runtime.run_cell(
        "try { agent.run(\"anything\"); return \"no throw\"; }\ncatch (e) { return e.message; }\n",
    );
    let text = returned_string(&refused);
    assert!(text.contains("10 token(s) left"), "{text}");

    // Plenty, and it starts. `0` means unknown, which is not a refusal.
    runtime.set_task_context(400_000, "claude-sonnet-5");
    let started = runtime.run_cell("return agent.run(\"anything\").source;\n");
    assert!(returned_string(&started).starts_with("agent/"));
}

/// An explicit assignment is carried into the job without imposing a turn cap.
#[test]
fn a_subagent_job_carries_its_explicit_model() {
    use sterna::agent::AgentOptions;
    let asked = AgentOptions {
        turns: None,
        model: "chosen-model".to_string(),
        effort: sterna::wire::Effort::default(),
        deadline: None,
    };
    assert_eq!(asked.model, "chosen-model");
    assert!(
        asked.turns.is_none(),
        "an unbounded job runs until it answers"
    );
}

// --- `web` exists only when configured, and a fetch is one rollout line

/// A transport that answers every GET with one fixed document and remembers
/// what it was asked, so the broker's own policy and the runtime's record are
/// what is measured, not the network.
struct FixedTransport {
    asked: std::sync::Mutex<Vec<String>>,
}

impl WebTransport for FixedTransport {
    fn get(&self, url: &str, _: usize, _: Duration) -> Result<WebResponse, String> {
        self.asked.lock().unwrap().push(url.to_string());
        Ok(WebResponse {
            status: 200,
            location: None,
            content_type: "text/plain; charset=utf-8".into(),
            body: b"hello from the fixture".to_vec(),
        })
    }
}

fn web_broker(config: WebConfig) -> WebBroker {
    WebBroker::with_transport(
        config,
        Box::new(FixedTransport {
            asked: std::sync::Mutex::new(Vec::new()),
        }),
    )
    .unwrap()
}

/// **Map 2658 and 2656, through a real isolate.** Before any broker, and with
/// a broker whose configuration allows nothing, `web` is not a name the cell
/// holds; once `[web]` allows a domain it is; and a fetch leaves one call
/// record naming the URL, the status, the size and the type of the answer —
/// the rollout line the transcript renders.
#[test]
fn web_is_bound_only_when_configured_and_a_fetch_is_one_rollout_line() {
    let fixture = Fixture::new("web-bound");
    let session = SessionId::new("web-bound-session");

    let mut bare = runtime(&fixture, &session);
    assert_eq!(
        returned_string(&bare.run_cell("return typeof web;")),
        "undefined",
        "a runtime nobody configured holds a `web`"
    );

    let unconfigured = WebConfig {
        enabled: true,
        ..WebConfig::default()
    };
    let mut still_bare = runtime(&fixture, &session).with_web_broker(web_broker(unconfigured));
    assert_eq!(
        returned_string(&still_bare.run_cell("return typeof web;")),
        "undefined",
        "enabled with nothing allowed is not configured, so `web` must not exist"
    );

    let configured = WebConfig {
        enabled: true,
        allow_domains: vec!["example.com".into()],
        ..WebConfig::default()
    };
    let mut runtime = runtime(&fixture, &session).with_web_broker(web_broker(configured));
    assert_eq!(
        returned_string(&runtime.run_cell("return typeof web;")),
        "object"
    );
    let outcome = runtime
        .run_cell("const page = web.fetch(\"https://example.com/docs\");\nreturn page.content;");
    assert_eq!(returned_string(&outcome), "hello from the fixture");
    let calls = &outcome.turn().record.calls;
    assert_eq!(calls.len(), 1, "{calls:?}");
    let call = &calls[0];
    assert_eq!(call.tool, "web.fetch");
    assert_eq!(call.args["url"], "https://example.com/docs");
    assert_eq!(call.args["status"], "200");
    assert_eq!(call.args["bytes"], "22");
    assert!(
        call.args["content_type"].starts_with("text/plain"),
        "{:?}",
        call.args
    );
    assert!(call.error.is_none(), "{call:?}");

    // A refused fetch is still one line, with the URL and the refusal.
    let refused = runtime
        .run_cell("try { web.fetch(\"https://elsewhere.org/\"); } catch (e) { return String(e); }");
    let calls = &refused.turn().record.calls;
    assert_eq!(calls.len(), 1, "{calls:?}");
    assert_eq!(calls[0].args["url"], "https://elsewhere.org/");
    assert!(
        calls[0]
            .error
            .as_deref()
            .is_some_and(|e| e.contains("denied")),
        "{:?}",
        calls[0]
    );
}
