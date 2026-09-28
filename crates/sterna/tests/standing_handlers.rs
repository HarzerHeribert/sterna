//! Standing programs run in the same live isolate, without a provider or replay.
use std::time::Duration;
use sterna::contract::SessionId;
use sterna::events::window::{Window, WindowConfig};
use sterna::events::{Event, PayloadRef, Priority, Stamp};
use sterna::runtime::isolate::{DEFAULT_HEAP_LIMIT_BYTES, Runtime};
use sterna::runtime::outcome::{CellOutcome, Ended};
use sterna::runtime::preview::Value;
use sterna::sandbox::profile::Profile;
use sterna::tools::invoke::CancellationToken;

fn runtime(limit: Duration) -> Runtime {
    Runtime::with_limits(
        &Profile::compile(std::env::temp_dir(), None),
        &SessionId::new("standing-tests"),
        DEFAULT_HEAP_LIMIT_BYTES,
        limit,
    )
}
fn batch(runtime: &mut Runtime) {
    let mut window = Window::new(WindowConfig::default());
    for (name, source) in [
        ("PostToolUse", "worker/noise"),
        ("PreToolUse", "worker/keep"),
    ] {
        window.accept(
            Event::pending_hook(
                name,
                source,
                Stamp::from_millis(0),
                PayloadRef::new("secret-payload"),
                Priority::Batch,
                "summary",
                name,
            ),
            Stamp::from_millis(0),
        );
    }
    runtime.deliver_batch(window.close_if_due(Stamp::from_millis(2000)).unwrap());
}
fn yielded(outcome: CellOutcome) {
    assert!(
        matches!(outcome, CellOutcome::Yielded { .. }),
        "{outcome:?}"
    );
}
fn register(runtime: &mut Runtime, pattern: &str, source: &str) {
    yielded(runtime.run_cell(&format!(
        "const noise = on({pattern}, {});",
        serde_json::to_string(source).unwrap()
    )));
}
fn returned_number(runtime: &mut Runtime, program: &str, expected: f64) {
    let result = runtime.run_cell(program);
    assert!(
        matches!(&result, CellOutcome::Returned { value: Value::Number(n), .. } if *n == expected),
        "{result:?}"
    );
}

#[test]
fn batch_accessors_are_bounded_only_when_the_next_program_runs() {
    const PROBE: &str = "STERNA_HANDLER_ACCESSOR_PROBE";
    if let Ok(mode) = std::env::var(PROBE) {
        let cancelled = mode == "cancel";
        let throwing = mode.starts_with("throw");
        let ordinary = mode.ends_with("cell");
        let mut r = runtime(if cancelled {
            Duration::from_secs(5)
        } else {
            Duration::from_millis(150)
        });
        let setter = if throwing {
            "throw new Error('bad setter');"
        } else {
            "for (;;) {}"
        };
        register(
            &mut r,
            "{}",
            &format!(
                "Object.defineProperty(globalThis, 'batch', {{configurable:true, set(v) {{ {setter} }} }});"
            ),
        );
        batch(&mut r);
        let first = r.run_handlers();
        assert_eq!(first.len(), 1);
        assert!(matches!(first[0].1, CellOutcome::Yielded { .. }));
        let delivered = std::time::Instant::now();
        batch(&mut r);
        assert!(delivered.elapsed() < Duration::from_secs(1));
        let canceller = cancelled.then(|| {
            let token = CancellationToken::new();
            r.set_token(token.clone());
            std::thread::spawn(move || {
                std::thread::sleep(Duration::from_millis(40));
                token.cancel();
            })
        });
        let started = std::time::Instant::now();
        let outcome = if ordinary {
            r.off_handler("noise");
            r.run_cell("return batch.n;")
        } else {
            let mut runs = r.run_handlers();
            assert_eq!(runs.len(), 1);
            assert!(!r.handlers()[0].active);
            runs.remove(0).1
        };
        if let Some(canceller) = canceller {
            canceller.join().unwrap();
        }
        assert!(started.elapsed() < Duration::from_secs(2));
        let expected = if cancelled {
            "Cancelled"
        } else if throwing {
            "Error"
        } else {
            "RuntimeTimeout"
        };
        assert!(
            matches!(&outcome, CellOutcome::Threw { error, .. } if error.class == expected),
            "{outcome:?}"
        );
        return;
    }

    // A regression must fail the test, not hang the entire suite inside V8.
    for mode in [
        "loop-handler",
        "loop-cell",
        "throw-handler",
        "throw-cell",
        "cancel",
    ] {
        let mut child = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "batch_accessors_are_bounded_only_when_the_next_program_runs",
                "--nocapture",
            ])
            .env(PROBE, mode)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(8);
        while child.try_wait().unwrap().is_none() {
            if std::time::Instant::now() >= deadline {
                child.kill().unwrap();
                let output = child.wait_with_output().unwrap();
                panic!(
                    "{mode} escaped the runtime deadline: {}",
                    String::from_utf8_lossy(&output.stderr)
                );
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        let output = child.wait_with_output().unwrap();
        assert!(
            output.status.success(),
            "{mode}: {}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

#[test]
fn delivery_keeps_batch_visible_to_preflight_after_the_global_was_deleted() {
    let mut r = runtime(Duration::from_secs(2));
    returned_number(&mut r, "return batch.n;", 0.0);
    yielded(r.run_cell("delete globalThis.batch;"));
    batch(&mut r);
    returned_number(&mut r, "return batch.n;", 2.0);
}

#[test]
fn no_event_bookkeeping_does_not_create_a_delivery_or_refresh_the_global() {
    let mut r = runtime(Duration::from_secs(2));
    returned_number(&mut r, "return batch.n;", 0.0);
    yielded(r.run_cell("globalThis.batchWrites = 0; Object.defineProperty(globalThis, 'batch', {configurable: true, set(v) { globalThis.batchWrites++; }});"));
    assert!(r.take_batch().is_none());
    assert_eq!(r.batch_rolling_depth(), 0);
    assert_eq!(r.batch_remaining(), 0);
    assert!(r.run_handlers().is_empty());
    assert!(!r.handle_names().iter().any(|name| name == "batch"));
    returned_number(&mut r, "return globalThis.batchWrites;", 0.0);
    batch(&mut r);
    returned_number(&mut r, "return globalThis.batchWrites;", 1.0);
}

#[test]
fn getter_reentry_cannot_register_a_sixty_fifth_handler() {
    let mut r = runtime(Duration::from_secs(2));
    let outcome = r.run_cell(
        "for(let i=0;i<63;i++) on({}, ''); on({get kind(){on({}, ''); return 'hook.*';}}, '');",
    );
    assert!(
        matches!(&outcome, CellOutcome::Threw { error, .. } if error.class == "HandlerLimit"),
        "{outcome:?}"
    );
    assert_eq!(r.handlers().len(), 64);
    batch(&mut r);
    assert_eq!(r.run_handlers().len(), 64);
    assert_eq!(r.cell(), 1);
    let outcome = r.run_cell("on({}, '');");
    assert!(
        matches!(&outcome, CellOutcome::Threw { error, .. } if error.class == "HandlerLimit"),
        "{outcome:?}"
    );
    assert_eq!(r.handlers().len(), 64);
}

#[test]
fn a_first_binding_equal_to_the_generated_id_cannot_be_renamed_by_an_alias() {
    let mut r = runtime(Duration::from_secs(2));
    yielded(r.run_cell("const handler1 = on({}, ''); const alias = handler1;"));
    assert_eq!(r.handlers()[0].name, "handler1");
    yielded(r.run_cell("const later = alias;"));
    assert_eq!(r.handlers()[0].name, "handler1");
    assert!(!r.off_handler("alias"));
    assert!(!r.off_handler("later"));
    assert!(r.off_handler("handler1"));
    assert!(!r.handlers()[0].active);
}

#[test]
fn future_matches_share_scope_filter_ack_and_do_not_replay_or_charge_cells() {
    let mut r = runtime(Duration::from_secs(2));
    yielded(r.run_cell("let count = 0;"));
    batch(&mut r);
    register(
        &mut r,
        r#"{kind: "hook.*", source: "worker/noise"}"#,
        "count += 1; batch.ack(batch.where({source: 'worker/noise'}).map(e => e.id));",
    );
    // Registration itself does not run the program or acknowledge the existing batch.
    assert_eq!(r.batch_remaining(), 2);
    assert!(
        r.run_handlers().is_empty(),
        "registration ran against the existing batch"
    );
    batch(&mut r);
    let runs = r.run_handlers();
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].0, "noise");
    assert_eq!(r.cell(), 2, "handler charged an ordinary cell");
    assert_eq!(r.batch_remaining(), 1);
    assert!(!r.render_handles().contains("worker/noise"));
    assert!(!r.render_handles().contains("count +="));
    assert!(r.run_handlers().is_empty(), "same batch was replayed");
    returned_number(&mut r, "return count;", 1.0);
    batch(&mut r);
    assert_eq!(r.run_handlers().len(), 1);
    returned_number(&mut r, "return count;", 2.0);
    yielded(r.run_cell("off(noise); off(noise);"));
    assert!(!r.handlers()[0].active);
    batch(&mut r);
    assert!(r.run_handlers().is_empty());
}

#[test]
fn no_match_does_nothing_and_fully_drained_batch_has_no_model_handle() {
    let mut r = runtime(Duration::from_secs(2));
    register(
        &mut r,
        r#"{kind:"message"}"#,
        "throw new Error('must not run');",
    );
    batch(&mut r);
    assert!(r.run_handlers().is_empty());
    assert!(r.off_handler("noise"));
    assert!(r.off_handler("noise"));
    assert!(!r.off_handler("missing"));
    yielded(r.run_cell("const drain = on({}, 'batch.ack(batch.rest().map(e => e.id));');"));
    batch(&mut r);
    assert_eq!(r.run_handlers().len(), 1);
    assert_eq!(r.batch_remaining(), 0);
    assert!(!r.handle_names().iter().any(|n| n == "batch"));
    assert_eq!(r.handlers()[1].drained, 1);
    returned_number(&mut r, "return batch.n + batch.where({}).length;", 0.0);
}

#[test]
fn partial_throw_disables_once_preserves_unacked_and_bounds_notice() {
    let mut r = runtime(Duration::from_secs(2));
    register(
        &mut r,
        "{}",
        "batch.ack([batch.rest()[0].id]); const e = new Error('secret payload'); e.name = 'Oops\\nsecret'.repeat(500); throw e;",
    );
    batch(&mut r);
    assert_eq!(r.run_handlers().len(), 1);
    assert_eq!(r.batch_remaining(), 1);
    assert!(!r.handlers()[0].active);
    let notices = r.take_handler_notices();
    assert_eq!(notices.len(), 1);
    assert!(!notices[0].contains('\n'));
    assert!(notices[0].len() < 120);
    assert!(!notices[0].contains("secret payload"));
    assert!(r.take_handler_notices().is_empty());
    batch(&mut r);
    assert!(r.run_handlers().is_empty());
    assert_eq!(r.handlers()[0].runs, 1);
}

#[test]
fn nested_registration_is_refused_and_task_end_frees_program_values() {
    let mut r = runtime(Duration::from_secs(2));
    register(&mut r, "{}", "on({}, 'batch.ack([])');");
    batch(&mut r);
    let runs = r.run_handlers();
    assert!(
        matches!(&runs[0].1, CellOutcome::Threw { error, .. } if error.class == "HandlerNesting")
    );
    assert_eq!(r.handlers().len(), 1);
    assert_eq!(r.batch_remaining(), 2);
    r.end_task();
    assert!(r.handlers().is_empty());
    assert_eq!(r.batch_remaining(), 0);
    assert!(r.handle_names().is_empty());
}

#[test]
fn timeout_and_cpu_cancellation_disable_with_no_retry_and_runtime_recovers() {
    let mut r = runtime(Duration::from_millis(100));
    register(&mut r, "{}", "for (;;) {}");
    batch(&mut r);
    let runs = r.run_handlers();
    assert!(
        matches!(&runs[0].1, CellOutcome::Threw { error, .. } if error.class == "RuntimeTimeout"),
        "{:?}",
        runs[0]
    );
    assert!(!r.handlers()[0].active);
    returned_number(&mut r, "return 7;", 7.0);

    let mut r = runtime(Duration::from_secs(5));
    register(&mut r, "{}", "for (;;) {}");
    batch(&mut r);
    let token = CancellationToken::new();
    r.set_token(token.clone());
    let canceller = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(40));
        token.cancel();
    });
    let start = std::time::Instant::now();
    let runs = r.run_handlers();
    canceller.join().unwrap();
    assert!(start.elapsed() < Duration::from_secs(2));
    assert!(
        matches!(&runs[0].1, CellOutcome::Threw { error, .. } if error.class == "Cancelled"),
        "{:?}",
        runs[0]
    );
    assert!(!r.handlers()[0].active);
    r.set_token(CancellationToken::new());
    returned_number(&mut r, "return 8;", 8.0);
}

#[test]
fn caught_permission_refusal_disables_a_handler_without_widening_the_profile() {
    // Every command line is admitted unless a deny pattern refuses it.
    let mut r = Runtime::with_limits(
        &Profile::compile(
            std::env::temp_dir(),
            Some(r#"{"permissions":{"deny":["Bash(refused-command*)"]}}"#),
        ),
        &SessionId::new("standing-tests"),
        DEFAULT_HEAP_LIMIT_BYTES,
        Duration::from_secs(2),
    );
    register(
        &mut r,
        "{}",
        "try { await bash({command: 'refused-command'}); } catch {};",
    );
    batch(&mut r);
    let runs = r.run_handlers();
    assert!(
        runs[0]
            .1
            .turn()
            .record
            .calls
            .iter()
            .any(|c| matches!(c.ended, Ended::Denied { .. }))
    );
    assert_eq!(r.handlers()[0].error.as_deref(), Some("PermissionDenied"));
    assert_eq!(r.batch_remaining(), 2);
}

/// A handler's reported lifecycle carries its state and its error's class,
/// never its source or a payload.
#[test]
fn handler_lifecycle_is_reported_without_source_or_payloads() {
    let mut r = runtime(Duration::from_secs(2));
    register(&mut r, "{}", "throw new Error('private source');");
    assert!(r.handlers().last().unwrap().active);
    batch(&mut r);
    r.run_handlers();
    let info = r.handlers().last().unwrap().clone();
    assert!(!info.active);
    assert!(info.error.as_deref().unwrap_or_default().contains("Error"));
    assert!(!format!("{info:?}").contains("private source"), "{info:?}");
}

#[cfg(unix)]
#[test]
fn handler_tools_run_as_calls_and_rollout_does_not_restore_handlers() {
    let root = std::env::temp_dir().join(format!("sterna-standing-hooks-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("input.txt"), "test\n").unwrap();
    let session = SessionId::new("handler-hook-path");
    let mut r = Runtime::new(&Profile::compile(&root, None), &session);
    let source =
        "const file = await read({path: 'input.txt'}); batch.ack(batch.rest().map(e => e.id));";
    register(&mut r, "{}", source);
    batch(&mut r);
    let runs = r.run_handlers();
    assert_eq!(runs.len(), 1);
    assert!(
        matches!(runs[0].1, CellOutcome::Yielded { .. }),
        "{:?}",
        runs[0]
    );
    assert_eq!(runs[0].1.turn().record.calls.len(), 1, "{:?}", runs[0]);
    let path = root.join("rollout.jsonl");
    let mut rollout = sterna::rollout::Rollout::create(&path, session, "system").unwrap();
    rollout
        .record_handler(&runs[0].0, &runs[0].1.turn().record)
        .unwrap();
    drop(rollout);
    let text = std::fs::read_to_string(&path).unwrap();
    let record: serde_json::Value = serde_json::from_str(text.lines().last().unwrap()).unwrap();
    assert_eq!(record["handler"], "noise");
    assert_eq!(record["source"], source);
    assert_eq!(record["cell"], 1);
    assert_eq!(record["calls"].as_array().unwrap().len(), 1);
    assert!(!record.to_string().contains("secret-payload"));
    r.end_task();
    assert!(r.handlers().is_empty());
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn sidebar_count_is_live_and_narrow_terminal_stays_without_sidebar() {
    use ratatui::{Terminal, backend::TestBackend};
    use sterna::tui::{Notebook, ScreenState};
    let mut r = runtime(Duration::from_secs(2));
    register(&mut r, "{}", "batch.ack([]);");
    let mut notebook = Notebook {
        handlers: r.handlers(),
        ..Notebook::default()
    };
    let render = |width, notebook: &Notebook| {
        let mut terminal = Terminal::new(TestBackend::new(width, 40)).unwrap();
        terminal
            .draw(|frame| {
                sterna::tui::render_screen(
                    frame,
                    &sterna::contract::Conversation::default(),
                    &sterna::contract::ServedBy::default(),
                    &sterna::runtime::handles::HandleTable::new(),
                    notebook,
                    &ScreenState::default(),
                )
            })
            .unwrap();
        terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>()
    };
    assert!(render(140, &notebook).contains("handlers 1"));
    assert!(!render(80, &notebook).contains("handlers 1"));
    r.off_handler("noise");
    notebook.handlers = r.handlers();
    // The count is live, and a count of none is shown by the line going away:
    // the readout draws a handler row only when one is standing, so "handlers"
    // vanishing is what proves the `off` reached the sidebar. Asserted against
    // a render that is otherwise intact, so a broken draw cannot pass as an
    // absent handler.
    let after = render(140, &notebook);
    assert!(
        !after.contains("handlers 1"),
        "the handler is still counted"
    );
    assert!(
        !after.contains("handlers"),
        "no handler row is drawn for none"
    );
}

#[test]
fn off_uses_the_handle_identity_even_when_a_binding_matches_another_handlers_id() {
    let mut r = runtime(Duration::from_secs(2));
    yielded(r.run_cell("const handler2 = on({}, 'batch.ack([]);'); const second = on({}, 'batch.ack([]);'); off(second);"));
    let handlers = r.handlers();
    assert!(handlers[0].active);
    assert!(!handlers[1].active);
    batch(&mut r);
    assert_eq!(r.run_handlers().len(), 1);
}

#[test]
fn handler_heap_limit_disables_without_charging_an_ordinary_cell() {
    let mut r = Runtime::with_limits(
        &Profile::compile(std::env::temp_dir(), None),
        &SessionId::new("handler-heap-limit"),
        8 * 1024 * 1024,
        Duration::from_secs(2),
    );
    register(
        &mut r,
        "{}",
        "const huge = new Uint8Array(32 * 1024 * 1024);",
    );
    batch(&mut r);
    let runs = r.run_handlers();
    assert!(
        matches!(&runs[0].1, CellOutcome::Threw { error, .. } if error.class == "RuntimeOutOfMemory"),
        "{:?}",
        runs[0]
    );
    assert!(!r.handlers()[0].active);
    assert_eq!(r.cell(), 1);
    assert_eq!(r.batch_remaining(), 2);
}

#[test]
fn saved_handler_rollout_is_inspectable_but_resume_replays_nothing() {
    let root = std::env::temp_dir().join(format!("sterna-handler-resume-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    let path = root.join("rollout.jsonl");
    let mut r = runtime(Duration::from_secs(2));
    register(&mut r, "{}", "batch.ack(batch.rest().map(e => e.id));");
    batch(&mut r);
    let runs = r.run_handlers();
    let mut rollout =
        sterna::rollout::Rollout::create(&path, SessionId::new("handler-resume"), "system")
            .unwrap();
    rollout
        .record_handler(&runs[0].0, &runs[0].1.turn().record)
        .unwrap();
    drop(rollout);
    let resumed = sterna::rollout::resume(&path).unwrap();
    assert!(resumed.messages.is_empty());
    assert!(sterna::rollout::resume_views(&path).unwrap().is_empty());
    let restored = runtime(Duration::from_secs(2));
    assert!(restored.handlers().is_empty());
    assert!(restored.handle_names().is_empty());
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn later_handlers_see_the_filtered_batch_count() {
    let mut r = runtime(Duration::from_secs(2));
    yielded(r.run_cell("const first = on({source:'worker/noise'}, 'batch.ack(batch.where({source: \"worker/noise\"}).map(e => e.id));'); const second = on({source:'worker/keep'}, 'if (batch.n !== 1) throw new Error(\"stale count\"); batch.ack(batch.rest().map(e => e.id));');"));
    batch(&mut r);
    let runs = r.run_handlers();
    assert_eq!(runs.len(), 2);
    assert!(
        runs.iter()
            .all(|(_, outcome)| matches!(outcome, CellOutcome::Yielded { .. })),
        "{runs:?}"
    );
    assert_eq!(r.batch_remaining(), 0);
    assert_eq!(r.cell(), 1);
}

#[test]
fn a_standing_handler_run_does_not_revoke_the_repair_offer() {
    let mut r = runtime(Duration::from_secs(2));
    register(&mut r, "{}", "globalThis.ran = (globalThis.ran ?? 0) + 1;");
    let outcome = r.run_cell("const = ;");
    assert!(matches!(&outcome, CellOutcome::Threw { .. }), "{outcome:?}");
    let offered = r
        .syntax_failure()
        .expect("a parse failure offers a repair")
        .cell;
    batch(&mut r);
    assert_eq!(r.run_handlers().len(), 1);
    assert_eq!(
        r.syntax_failure().map(|failure| failure.cell),
        Some(offered),
        "a standing handler run revoked the repair offer"
    );
}
