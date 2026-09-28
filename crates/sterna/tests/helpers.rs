//! Acceptance for the pulled half of `docs/helpers.md`:
//! `await helper.reduce(text)` from inside a cell.
//!
//! **No test here reaches a real provider.** The two that need a wire call
//! point `ANTHROPIC_BASE_URL` at a listener this file owns; the fail-closed
//! test points it at one that accepts nothing, so "no wire call was
//! attempted" is a count this file measures rather than a claim it makes.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use sterna::config::HelpersConfig;
use sterna::contract::SessionId;
use sterna::helpers::{CallSite, HelperSpec, REDUCER};
use sterna::prompt::declarations::callable_from_a_cell;
use sterna::runtime::bindings::HostGlobals;
use sterna::runtime::isolate::Runtime;
use sterna::runtime::outcome::CellOutcome;
use sterna::sandbox::profile::Profile;

#[path = "support/sse.rs"]
mod sse;

/// `ANTHROPIC_BASE_URL` is process-global, so the tests that set it are
/// serialised against each other exactly as `turns.rs` serialises its own.
static ENV_LOCK: Mutex<()> = Mutex::new(());
static COUNTER: AtomicU64 = AtomicU64::new(0);

struct Fixture {
    root: PathBuf,
}

impl Fixture {
    fn new(label: &str) -> Self {
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let root =
            std::env::temp_dir().join(format!("sterna-helpers-{}-{label}-{n}", std::process::id()));
        std::fs::create_dir_all(root.join(".claude")).unwrap();
        Self { root }
    }

    fn profile(&self) -> Profile {
        Profile::compile(&self.root, Some(r#"{"permissions":{"allow":[]}}"#))
    }

    /// A profile with a grant, for the `CallSite::PostResult` tests. Gated
    /// like its only callers: the Windows cell denies warnings and a helper
    /// whose callers are all `unix` tests is dead there.
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    fn profile_with(&self, settings: &str) -> Profile {
        Profile::compile(&self.root, Some(settings))
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// A listener that answers every request with one assistant message whose
/// only content is `text`, and counts what it was asked.
struct Provider {
    url: String,
    requests: Arc<AtomicUsize>,
    bodies: Arc<Mutex<Vec<serde_json::Value>>>,
}

fn provider(text: &str) -> Provider {
    provider_with_usage(
        text,
        serde_json::json!({"input_tokens": 10, "output_tokens": 5}),
    )
}

fn provider_with_usage(text: &str, usage: serde_json::Value) -> Provider {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let requests = Arc::new(AtomicUsize::new(0));
    let seen = requests.clone();
    let bodies = Arc::new(Mutex::new(Vec::new()));
    let captured = bodies.clone();
    let reply = text.to_string();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { return };
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut length = 0usize;
            loop {
                let mut line = String::new();
                if reader.read_line(&mut line).unwrap_or(0) == 0 {
                    return;
                }
                if line == "\r\n" || line == "\n" {
                    break;
                }
                if let Some(value) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                    length = value.trim().parse().unwrap();
                }
            }
            let mut body = vec![0; length];
            if reader.read_exact(&mut body).is_err() {
                return;
            }
            let request: serde_json::Value = serde_json::from_slice(&body).unwrap();
            captured.lock().unwrap().push(request.clone());
            seen.fetch_add(1, Ordering::SeqCst);
            let whole = serde_json::json!({
                "role": "assistant",
                "content": [{"type": "text", "text": reply}],
                "usage": usage
            })
            .to_string();
            let (content_type, payload) = sse::response_for(&request, &whole);
            let _ = write!(
                stream,
                "HTTP/1.1 200 OK\r\ncontent-type: {content_type}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                payload.len(),
                payload
            );
        }
    });
    Provider {
        url: format!("http://{address}"),
        requests,
        bodies,
    }
}

fn scripted_provider(payloads: Vec<serde_json::Value>) -> Provider {
    scripted_provider_with_delays(
        payloads
            .into_iter()
            .map(|payload| (payload, Duration::ZERO))
            .collect(),
    )
}

fn scripted_provider_with_delays(payloads: Vec<(serde_json::Value, Duration)>) -> Provider {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let requests = Arc::new(AtomicUsize::new(0));
    let seen = requests.clone();
    let bodies = Arc::new(Mutex::new(Vec::new()));
    let captured = bodies.clone();
    std::thread::spawn(move || {
        for (payload, delay) in payloads {
            let Ok((mut stream, _)) = listener.accept() else {
                return;
            };
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut length = 0usize;
            loop {
                let mut line = String::new();
                if reader.read_line(&mut line).unwrap_or(0) == 0 {
                    return;
                }
                if line == "\r\n" || line == "\n" {
                    break;
                }
                if let Some(value) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                    length = value.trim().parse().unwrap();
                }
            }
            let mut body = vec![0; length];
            if reader.read_exact(&mut body).is_err() {
                return;
            }
            let request: serde_json::Value = serde_json::from_slice(&body).unwrap();
            captured.lock().unwrap().push(request.clone());
            seen.fetch_add(1, Ordering::SeqCst);
            std::thread::sleep(delay);
            let (content_type, payload) = sse::response_for(&request, &payload.to_string());
            let _ = write!(
                stream,
                "HTTP/1.1 200 OK\r\ncontent-type: {content_type}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                payload.len(),
                payload
            );
        }
    });
    Provider {
        url: format!("http://{address}"),
        requests,
        bodies,
    }
}

fn configured(model: &str, calls_per_cell: u32) -> HelpersConfig {
    HelpersConfig {
        model: model.to_string().into(),
        enabled: true,
        calls_per_cell,
        ..HelpersConfig::default()
    }
}

fn threw(outcome: &CellOutcome) -> (String, String) {
    match outcome {
        CellOutcome::Threw { error, .. } => (error.class.clone(), error.message.clone()),
        other => panic!("expected a throw, got {other:?}"),
    }
}

fn returned_text(outcome: &CellOutcome) -> String {
    match outcome {
        CellOutcome::Returned { value, .. } => match value {
            sterna::runtime::preview::Value::String(text) => text.head().to_string(),
            other => panic!("expected a string, got {other:?}"),
        },
        other => panic!("expected a return, got {other:?}"),
    }
}

/// The fail-closed default: `[helpers] model` unset means no helper runs, and
/// the refusal is a `ToolError` the model's own program can catch.
#[test]
fn an_unconfigured_helper_refuses_and_makes_no_wire_call() {
    let _environment = ENV_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    let fixture = Fixture::new("unconfigured");
    let provider = provider("never asked");
    // SAFETY: the environment lock serialises every test in this file that
    // touches these variables, and no other thread here reads them.
    unsafe {
        std::env::set_var("ANTHROPIC_BASE_URL", &provider.url);
    }

    let mut runtime = Runtime::new(&fixture.profile(), &SessionId::new("helpers-unconfigured"));
    let outcome = runtime.run_cell(
        "try { await helper.reduce(\"a log line\"); answer(\"no refusal\"); }\n\
         catch (e) { return e.name + \": \" + e.message; }\n",
    );

    unsafe {
        std::env::remove_var("ANTHROPIC_BASE_URL");
    }

    let caught = returned_text(&outcome);
    assert!(
        caught.starts_with("ToolError: "),
        "the refusal must be a catchable ToolError, got {caught:?}"
    );
    assert!(
        caught.contains("[helpers] model"),
        "the refusal must name the configuration that is missing, got {caught:?}"
    );
    assert_eq!(
        provider.requests.load(Ordering::SeqCst),
        0,
        "an unconfigured helper must not reach the wire at all"
    );
    assert!(
        runtime.helper_records().is_empty(),
        "a call that never ran is not a helper record"
    );
}

/// The per-cell ceiling: `calls_per_cell` calls go through and the next one
/// is a refusal the program can catch, so a loop cannot spend without bound.
#[test]
fn the_cell_call_ceiling_refuses_the_call_after_it() {
    let _environment = ENV_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    let fixture = Fixture::new("ceiling");
    let provider = provider("error[E0433]: failed to resolve");
    unsafe {
        std::env::set_var("ANTHROPIC_BASE_URL", &provider.url);
    }

    let mut runtime = Runtime::new(&fixture.profile(), &SessionId::new("helpers-ceiling"))
        .with_helpers(configured("test-helper-model", 2));
    let outcome = runtime.run_cell(
        "let done = 0;\n\
         for (let i = 0; i < 3; i++) {\n\
         \x20 try { await helper.reduce(\"a log line\"); done++; }\n\
         \x20 catch (e) { return e.name + \" after \" + done + \": \" + e.message; }\n\
         }\n\
         answer(\"never refused\");\n",
    );

    unsafe {
        std::env::remove_var("ANTHROPIC_BASE_URL");
    }

    let caught = returned_text(&outcome);
    assert!(
        caught.starts_with("ToolError after 2: "),
        "the third call must be the catchable refusal, got {caught:?}"
    );
    assert!(
        caught.contains("2 helper call"),
        "the refusal must say what the ceiling was, got {caught:?}"
    );
    assert_eq!(
        provider.requests.load(Ordering::SeqCst),
        2,
        "the refused call must not reach the wire"
    );
    assert_eq!(
        runtime.helper_records().len(),
        2,
        "only the calls that ran are records"
    );
}

/// What the lane and the `/cell` inspector read: one record per call, naming
/// the helper and a bounded `asked` that is **not** the payload.
#[test]
fn a_helper_call_leaves_a_record_that_names_it_and_not_its_payload() {
    let _environment = ENV_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    let fixture = Fixture::new("record");
    let provider = provider("error[E0433]: failed to resolve `foo`");
    unsafe {
        std::env::set_var("ANTHROPIC_BASE_URL", &provider.url);
    }

    let mut runtime = Runtime::new(&fixture.profile(), &SessionId::new("helpers-record"))
        .with_helpers(configured("test-helper-model", 8));
    // Four distinct lines, one of them a phrase no summary may echo.
    let outcome = runtime.run_cell(
        "const log = [\"warning: unused\", \"SECRET-PAYLOAD-MARKER\", \"error: boom\", \"done\"]\n\
         \x20 .join(\"\\n\");\n\
         return await helper.reduce(log);\n",
    );

    unsafe {
        std::env::remove_var("ANTHROPIC_BASE_URL");
    }

    assert_eq!(
        returned_text(&outcome),
        "error[E0433]: failed to resolve `foo`"
    );
    let records = runtime.helper_records();
    assert_eq!(records.len(), 1, "{records:?}");
    let record = &records[0];
    assert_eq!(record.helper, "reduce");
    assert_eq!(record.verb, "reducing");
    assert_eq!(record.turns, 1);
    assert!(record.outcome.ok, "{record:?}");
    assert_eq!(record.asked, "4 lines", "{record:?}");
    assert_eq!(record.usage.model, "test-helper-model");
    assert_eq!(record.usage.requests, 1);
    assert_eq!(record.usage.reported_requests, 1);
    assert_eq!(record.usage.known_tokens(), 15);
    assert!(
        !record.usage.complete(),
        "omitted cache fields are unknown rather than measured zero: {record:?}"
    );
    assert!(
        !record.asked.contains("SECRET-PAYLOAD-MARKER"),
        "`asked` must describe the payload, never carry it: {record:?}"
    );
}

#[test]
fn a_one_shot_helper_records_every_reported_token_class() {
    let _environment = ENV_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    let fixture = Fixture::new("usage-complete");
    let provider = provider_with_usage(
        "one failure",
        serde_json::json!({
            "input_tokens": 10,
            "output_tokens": 5,
            "cache_read_input_tokens": 70,
            "cache_creation_input_tokens": 20
        }),
    );
    unsafe { std::env::set_var("ANTHROPIC_BASE_URL", &provider.url) };
    let mut helpers = configured("test-helper-model", 8);
    helpers.effort.reduce = sterna::wire::Effort::High;
    let mut runtime = Runtime::new(
        &fixture.profile(),
        &SessionId::new("helpers-usage-complete"),
    )
    .with_helpers(helpers);

    let outcome = runtime.run_cell("return await helper.reduce(\"a log line\");\n");
    unsafe { std::env::remove_var("ANTHROPIC_BASE_URL") };

    assert_eq!(returned_text(&outcome), "one failure");
    let bodies = provider.bodies.lock().unwrap();
    assert_eq!(bodies.len(), 1);
    assert_eq!(bodies[0]["output_config"]["effort"], "high");
    // **The declared cap is the cap.** This used to assert `32_769 +
    // REDUCER.max_tokens`, on the reading that a reasoning budget must leave
    // the reducer's response allowance intact -- which made the 1,024 tokens
    // that exist to keep a reduction terse into a floor under 33,793, and put
    // 4,227 output tokens on the wire against 77 lines of input. A budget
    // must be at least 1,024 and strictly below `max_tokens`, so none is
    // expressible inside this cap and none is asked for; the effort word
    // still rides along.
    assert_eq!(bodies[0]["max_tokens"], REDUCER.max_tokens);
    assert!(
        bodies[0]
            .get("thinking")
            .is_none_or(serde_json::Value::is_null),
        "no thinking budget fits inside a {}-token answer cap: {}",
        REDUCER.max_tokens,
        bodies[0]
    );
    let records = runtime.helper_records();
    let usage = &records[0].usage;
    assert_eq!(usage.model, "test-helper-model");
    assert_eq!(usage.requests, 1);
    assert_eq!(usage.reported_requests, 1);
    assert_eq!(usage.input_tokens, 10);
    assert_eq!(usage.output_tokens, 5);
    assert_eq!(usage.cache_read_input_tokens, 70);
    assert_eq!(usage.cache_creation_input_tokens, 20);
    assert_eq!(usage.known_tokens(), 105);
    assert!(usage.complete());
}

#[test]
fn a_helper_with_no_usage_object_records_unknown_coverage_not_zero_usage() {
    let _environment = ENV_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    let fixture = Fixture::new("usage-missing");
    let provider = provider_with_usage("one failure", serde_json::Value::Null);
    unsafe { std::env::set_var("ANTHROPIC_BASE_URL", &provider.url) };
    let mut runtime = Runtime::new(&fixture.profile(), &SessionId::new("helpers-usage-missing"))
        .with_helpers(configured("test-helper-model", 8));

    let outcome = runtime.run_cell("return await helper.reduce(\"a log line\");\n");
    unsafe { std::env::remove_var("ANTHROPIC_BASE_URL") };

    assert_eq!(returned_text(&outcome), "one failure");
    let records = runtime.helper_records();
    let usage = &records[0].usage;
    assert!(usage.coverage_known);
    assert_eq!(usage.requests, 1);
    assert_eq!(usage.reported_requests, 0);
    assert_eq!(usage.known_tokens(), 0, "unknown usage invents no tokens");
    assert!(!usage.complete());
}

#[test]
fn a_multiturn_helper_sums_each_response_once_with_cache_coverage() {
    const TWO_TURN: HelperSpec = HelperSpec {
        name: "two_turn_test",
        summary: "test helper",
        verb: "testing",
        preamble: "Use a cell, then return.",
        tools: &[],
        max_tokens: 128,
        max_turns: 2,
        input: sterna::helpers::InputKind::Text,
        output: sterna::helpers::OutputKind::Reduction,
        call_sites: &[CallSite::Cell],
    };
    let _environment = ENV_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    let fixture = Fixture::new("usage-multiturn");
    let provider = scripted_provider(vec![
        serde_json::json!({
            "role": "assistant",
            "content": [{
                "type": "tool_use", "id": "cell-1", "name": "execute_cell",
                "input": {"code": "const observed = 1; console.log(observed);"}
            }],
            "usage": {"input_tokens": 10, "output_tokens": 5,
                "cache_read_input_tokens": 70, "cache_creation_input_tokens": 20}
        }),
        serde_json::json!({
            "role": "assistant",
            "content": [{
                "type": "tool_use", "id": "cell-2", "name": "execute_cell",
                "input": {"code": "answer(\"found\");"}
            }],
            "usage": {"input_tokens": 11, "output_tokens": 6,
                "cache_read_input_tokens": 71, "cache_creation_input_tokens": 21}
        }),
    ]);
    unsafe { std::env::set_var("ANTHROPIC_BASE_URL", &provider.url) };

    let call = sterna::helpers::run(
        &TWO_TURN,
        sterna::helpers::HelperRoute {
            model: "test-helper-model",
            effort: sterna::wire::Effort::Medium,
            cap: None,
        },
        "inspect this",
        &fixture.profile(),
        &SessionId::new("helpers-usage-multiturn"),
        &sterna::tools::invoke::CancellationToken::new(),
    );
    unsafe { std::env::remove_var("ANTHROPIC_BASE_URL") };

    assert!(call.outcome.ok, "{call:?}");
    assert_eq!(call.outcome.text, "found");
    assert_eq!(call.turns, 2);
    assert_eq!(call.usage.model, "test-helper-model");
    assert_eq!(call.usage.requests, 2);
    assert_eq!(call.usage.reported_requests, 2);
    assert_eq!(call.usage.input_tokens, 21);
    assert_eq!(call.usage.output_tokens, 11);
    assert_eq!(call.usage.cache_read_input_tokens, 141);
    assert_eq!(call.usage.cache_creation_input_tokens, 41);
    assert_eq!(call.usage.known_tokens(), 214);
    assert!(call.usage.complete());
}

/// **A helper is cut off for going silent, never for taking its time — on
/// the loop as well as on the one-shot errand.**
///
/// The narrowed loop used to hand `wire::SIDE_ERRAND_TIMEOUT` to a
/// non-streamed request, where it became a `timeout_global`: a whole-answer
/// ceiling, which is the quantity `wire::SIDE_ERRAND_SILENCE`'s own doc
/// comment says cannot distinguish a model that is thinking from a socket
/// that has died. Measured 2026-09-19: `CHECKER` died at exactly 120s with
/// its answer still arriving, and the quality miss it ran to catch shipped.
///
/// The ceiling itself is 120s of silence and no test can spend it, so what is
/// asserted here is the thing that decides it — the request the loop actually
/// puts on the wire. Three properties, each killed by a different way of
/// getting this wrong:
///
/// * `stream` is true, so the bound is the gap between events (revert the
///   `agent.rs` branch and this fails);
/// * `max_tokens` is the model's own, not the spec's. A spec's figure means
///   *this answer is short*, which is true of a one-shot errand and false of
///   a turn in a loop, where it would stop a response mid-`tool_use` and
///   throw away a call the provider had already finished;
/// * the request still declares tools, because a narrowed loop is narrowed to
///   a toolset and not to prose.
#[test]
fn a_narrowed_loops_turn_is_streamed_with_the_models_own_allowance() {
    const LOOPING: HelperSpec = HelperSpec {
        name: "streamed_loop_test",
        summary: "test helper",
        verb: "testing",
        preamble: "Use a cell, then return.",
        tools: &[],
        // Deliberately tiny, and deliberately not what reaches the wire.
        max_tokens: 128,
        max_turns: 2,
        input: sterna::helpers::InputKind::Text,
        output: sterna::helpers::OutputKind::Reduction,
        call_sites: &[CallSite::Cell],
    };
    let _environment = ENV_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    let fixture = Fixture::new("narrowed-streams");
    let provider = scripted_provider(vec![
        serde_json::json!({
            "role": "assistant",
            "content": [{
                "type": "tool_use", "id": "cell-1", "name": "execute_cell",
                "input": {"code": "console.log(1);"}
            }]
        }),
        serde_json::json!({
            "role": "assistant",
            "content": [{
                "type": "tool_use", "id": "cell-2", "name": "execute_cell",
                "input": {"code": "answer(\"done\");"}
            }]
        }),
    ]);
    unsafe { std::env::set_var("ANTHROPIC_BASE_URL", &provider.url) };

    let call = sterna::helpers::run(
        &LOOPING,
        sterna::helpers::HelperRoute {
            model: "test-helper-model",
            effort: sterna::wire::Effort::Medium,
            cap: None,
        },
        "inspect this",
        &fixture.profile(),
        &SessionId::new("helpers-narrowed-streams"),
        &sterna::tools::invoke::CancellationToken::new(),
    );
    unsafe { std::env::remove_var("ANTHROPIC_BASE_URL") };

    assert!(call.outcome.ok, "{call:?}");
    assert_eq!(call.outcome.text, "done");

    let bodies = provider.bodies.lock().unwrap().clone();
    assert_eq!(bodies.len(), 2, "both turns should have reached the wire");
    let allowed = sterna::wire::max_tokens_for("test-helper-model");
    assert_ne!(
        allowed, LOOPING.max_tokens,
        "this test cannot tell the two allowances apart unless they differ"
    );
    for (turn, body) in bodies.iter().enumerate() {
        assert_eq!(
            body.get("stream").and_then(serde_json::Value::as_bool),
            Some(true),
            "turn {turn} was not streamed, so its ceiling measures duration: {body}"
        );
        // `Allowance::Model` is the model's own maximum with the reasoning
        // budget added *on top*, since thinking is space the provider needs
        // beside the answer. Under a spec's `Allowance::Capped(128)` the
        // budget would not fit at all (`wire::THINKING_MIN_BUDGET` is 1,024)
        // and `max_tokens` would be the 128 itself, so this arithmetic is
        // what tells the two allowances apart.
        let asked = body
            .get("max_tokens")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or_default();
        let budget = body
            .get("thinking")
            .and_then(|thinking| thinking.get("budget_tokens"))
            .and_then(serde_json::Value::as_u64)
            .unwrap_or_default();
        assert_eq!(
            asked - budget,
            u64::from(allowed),
            "turn {turn} left the answer a cap that is not the model's own \
             (asked {asked}, of which {budget} is reasoning): {body}"
        );
        assert!(
            body.get("tools")
                .and_then(serde_json::Value::as_array)
                .is_some_and(|tools| !tools.is_empty()),
            "turn {turn} declared no tool, so the loop cannot act: {body}"
        );
    }
}

#[test]
fn cancellation_keeps_completed_usage_and_marks_the_inflight_request_unknown() {
    const TWO_TURN: HelperSpec = HelperSpec {
        name: "two_turn_cancel_test",
        summary: "test helper",
        verb: "testing",
        preamble: "Use a cell, then return.",
        tools: &[],
        max_tokens: 128,
        max_turns: 2,
        input: sterna::helpers::InputKind::Text,
        output: sterna::helpers::OutputKind::Reduction,
        call_sites: &[CallSite::Cell],
    };
    let _environment = ENV_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    let fixture = Fixture::new("usage-cancelled");
    let provider = scripted_provider_with_delays(vec![
        (
            serde_json::json!({
                "role": "assistant",
                "content": [{
                    "type": "tool_use", "id": "cell-1", "name": "execute_cell",
                    "input": {"code": "const observed = 1; console.log(observed);"}
                }],
                "usage": {"input_tokens": 10, "output_tokens": 5,
                    "cache_read_input_tokens": 70, "cache_creation_input_tokens": 20}
            }),
            Duration::ZERO,
        ),
        (
            serde_json::json!({
                "role": "assistant",
                "content": [{"type": "text", "text": "late"}],
                "usage": {"input_tokens": 999, "output_tokens": 999,
                    "cache_read_input_tokens": 999, "cache_creation_input_tokens": 999}
            }),
            Duration::from_millis(400),
        ),
    ]);
    unsafe { std::env::set_var("ANTHROPIC_BASE_URL", &provider.url) };

    let token = sterna::tools::invoke::CancellationToken::new();
    let cancel = token.clone();
    let requests = provider.requests.clone();
    let canceller = std::thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(2);
        while requests.load(Ordering::SeqCst) < 2 && Instant::now() < deadline {
            std::thread::yield_now();
        }
        assert_eq!(requests.load(Ordering::SeqCst), 2);
        cancel.cancel();
    });
    let started = Instant::now();
    let call = sterna::helpers::run(
        &TWO_TURN,
        sterna::helpers::HelperRoute {
            model: "test-helper-model",
            effort: sterna::wire::Effort::Medium,
            cap: None,
        },
        "inspect this",
        &fixture.profile(),
        &SessionId::new("helpers-usage-cancelled"),
        &token,
    );
    canceller.join().unwrap();
    unsafe { std::env::remove_var("ANTHROPIC_BASE_URL") };

    assert!(call.outcome.cancelled, "{call:?}");
    assert!(started.elapsed() < Duration::from_millis(300));
    assert_eq!(call.usage.requests, 2, "one completed and one in flight");
    assert_eq!(call.usage.reported_requests, 1);
    assert_eq!(call.usage.known_tokens(), 105);
    assert_eq!(call.usage.cache_read_reported_requests, 1);
    assert_eq!(call.usage.cache_creation_reported_requests, 1);
    assert!(!call.usage.complete());
}

#[test]
fn preflight_carries_its_helper_usage_into_the_returned_record() {
    let _environment = ENV_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    let fixture = Fixture::new("usage-preflight");
    let provider = provider_with_usage(
        "src/lib.rs:1 — entry point\nNot checked: other files",
        serde_json::json!({
            "input_tokens": 10,
            "output_tokens": 5,
            "cache_read_input_tokens": 70,
            "cache_creation_input_tokens": 20
        }),
    );
    unsafe { std::env::set_var("ANTHROPIC_BASE_URL", &provider.url) };

    // The preflight is handed the scouting brief, never the raw request;
    // the record still says what was asked.
    let brief = sterna::preflight::scouting_brief(
        "Find the repository entry point",
        &sterna::manifest::Manifest::default(),
    );
    let record = sterna::helpers::preflight(
        &brief,
        sterna::helpers::HelperRoute {
            model: "test-helper-model",
            effort: sterna::wire::Effort::Low,
            cap: None,
        },
        &fixture.profile(),
        &SessionId::new("helpers-usage-preflight"),
        &sterna::tools::invoke::CancellationToken::new(),
        |_| {},
    )
    .expect("the roster has a preflight helper");
    unsafe { std::env::remove_var("ANTHROPIC_BASE_URL") };

    assert!(record.outcome.ok, "{record:?}");
    assert_eq!(record.asked, "Find the repository entry point");
    assert_eq!(record.usage.model, "test-helper-model");
    assert_eq!(record.usage.requests, 1);
    assert_eq!(record.usage.reported_requests, 1);
    assert_eq!(record.usage.known_tokens(), 105);
    assert!(record.usage.complete());
}

#[test]
fn a_historical_helper_record_deserializes_with_unknown_usage_coverage() {
    let record: sterna::helpers::HelperRecord = serde_json::from_value(serde_json::json!({
        "helper": "reduce",
        "verb": "reducing",
        "asked": "old rollout",
        "outcome": {"text": "done", "ok": true, "cancelled": false, "elapsed_ms": 1},
        "turns": 1,
        "looked": []
    }))
    .unwrap();

    assert!(!record.usage.coverage_known);
    assert_eq!(record.usage.known_tokens(), 0);
    assert!(!record.usage.complete());
}

/// A helper that could not answer is a throw, never a reduction that looks
/// healthy.
/// **Nothing stops a helper on a count.** Its spec says `max_turns: 2`, the
/// old global ceiling was 8, and this one takes twelve turns and answers.
/// The user, 2026-09-17: *"if a helper returns nonsense that can do more harm
/// than good. So it should run as long as it needs."*
#[test]
fn a_helper_runs_past_every_former_ceiling_and_answers() {
    const PATIENT: HelperSpec = HelperSpec {
        name: "patient_test",
        summary: "test helper",
        verb: "testing",
        // The spec's own number is a description of the errand's shape now,
        // not a budget: twelve turns run under a spec that says two.
        preamble: "Use a cell, then return.",
        tools: &[],
        max_tokens: 128,
        max_turns: 2,
        input: sterna::helpers::InputKind::Text,
        output: sterna::helpers::OutputKind::Reduction,
        call_sites: &[CallSite::Cell],
    };
    let _environment = ENV_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    let fixture = Fixture::new("patient");
    let mut payloads: Vec<serde_json::Value> = (0..11)
        .map(|n| {
            serde_json::json!({
                "role": "assistant",
                "content": [{
                    "type": "tool_use", "id": format!("cell-{n}"), "name": "execute_cell",
                    "input": {"code": "const looked = 1;"}
                }]
            })
        })
        .collect();
    payloads.push(serde_json::json!({
        "role": "assistant",
        "content": [{
            "type": "tool_use", "id": "cell-last", "name": "execute_cell",
            "input": {"code": "answer(\"the twelfth turn answered\");"}
        }]
    }));
    let provider = scripted_provider(payloads);
    unsafe { std::env::set_var("ANTHROPIC_BASE_URL", &provider.url) };

    let call = sterna::helpers::run(
        &PATIENT,
        sterna::helpers::HelperRoute {
            model: "test-helper-model",
            effort: sterna::wire::Effort::Medium,
            cap: None,
        },
        "take as long as you need",
        &fixture.profile(),
        &SessionId::new("helpers-patient"),
        &sterna::tools::invoke::CancellationToken::new(),
    );
    unsafe { std::env::remove_var("ANTHROPIC_BASE_URL") };

    assert!(call.outcome.ok, "{call:?}");
    assert_eq!(call.outcome.text, "the twelfth turn answered");
    assert_eq!(call.turns, 12, "twelve turns under a spec that says two");
}

/// **Only a returned answer is an answer.** A helper that stops without
/// returning is `ok: false` with the reason in its text, which is what keeps
/// a silent stop from reading as a healthy short answer — the invariant that
/// carries the signal now that no count does.
#[test]
fn a_helper_that_stops_without_returning_is_not_a_healthy_answer() {
    const YIELDING: HelperSpec = HelperSpec {
        name: "yielding_test",
        summary: "test helper",
        verb: "testing",
        preamble: "Use a cell, then return.",
        tools: &[],
        max_tokens: 128,
        max_turns: 2,
        input: sterna::helpers::InputKind::Text,
        output: sterna::helpers::OutputKind::Reduction,
        call_sites: &[CallSite::Cell],
    };
    let _environment = ENV_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    let fixture = Fixture::new("yielding");
    // One reply, then the provider is spent: the next request fails, which is
    // a stop the helper did not choose.
    let provider = scripted_provider(vec![serde_json::json!({
        "role": "assistant",
        "content": [{
            "type": "tool_use", "id": "cell-1", "name": "execute_cell",
            "input": {"code": "const partial = 1;"}
        }]
    })]);
    unsafe { std::env::set_var("ANTHROPIC_BASE_URL", &provider.url) };

    let call = sterna::helpers::run(
        &YIELDING,
        sterna::helpers::HelperRoute {
            model: "test-helper-model",
            effort: sterna::wire::Effort::Medium,
            cap: None,
        },
        "answer this",
        &fixture.profile(),
        &SessionId::new("helpers-yielding"),
        &sterna::tools::invoke::CancellationToken::new(),
    );
    unsafe { std::env::remove_var("ANTHROPIC_BASE_URL") };

    assert!(
        !call.outcome.ok,
        "a helper that never returned must not read as answered: {call:?}"
    );
    assert!(
        call.outcome.text.contains("the call ended"),
        "the caller is told how it ended: {call:?}"
    );
}

#[test]
fn a_failed_helper_call_throws_and_is_recorded_as_failed() {
    let _environment = ENV_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    let fixture = Fixture::new("failed");
    // Nothing is listening on this port, so the request cannot complete.
    unsafe {
        std::env::set_var("ANTHROPIC_BASE_URL", "http://127.0.0.1:9");
    }

    let mut runtime = Runtime::new(&fixture.profile(), &SessionId::new("helpers-failed"))
        .with_helpers(configured("test-helper-model", 8));
    let outcome = runtime.run_cell("return await helper.reduce(\"a log line\");\n");

    unsafe {
        std::env::remove_var("ANTHROPIC_BASE_URL");
    }

    let (class, message) = threw(&outcome);
    assert_eq!(class, "ToolError", "{message}");
    let records = runtime.helper_records();
    assert_eq!(records.len(), 1, "{records:?}");
    assert!(
        !records[0].outcome.ok,
        "a failed call must never be recorded as a healthy one: {records:?}"
    );
}

/// The roster declares itself: appending a `HelperSpec` is what puts a helper
/// in front of the model, with no second place to edit — and `call_sites` is
/// what decides which helpers a cell is told about at all.
/// The pilot's scouts, handed the raw request, tried to be the parent and
/// reported that they could not run commands. The preamble now says what a
/// scouting brief is for, and the toolset still cannot change anything.
#[test]
fn the_scout_preamble_says_it_scouts_for_the_actor_and_names_no_mutating_tool() {
    let scout = sterna::helpers::SCOUT;
    assert!(
        scout
            .preamble
            .contains("scouting for the model that will act"),
        "{}",
        scout.preamble
    );
    assert!(
        scout.preamble.contains("never attempt the request"),
        "{}",
        scout.preamble
    );
    for tool in sterna::helpers::FORBIDDEN_TOOLS {
        assert!(!scout.tools.contains(&tool), "SCOUT holds `{tool}`");
        assert!(
            !scout.preamble.contains(&format!("`{tool}`")),
            "the preamble names `{tool}`: {}",
            scout.preamble
        );
    }
    assert!(!scout.preamble.contains("bash"), "{}", scout.preamble);
}

#[test]
fn every_helper_in_the_roster_is_declared_to_the_model() {
    let runtime = sterna::prompt::render_runtime();
    assert!(runtime.contains("declare const helper: {"), "{runtime}");
    for spec in sterna::helpers::HELPERS {
        let declared =
            runtime.contains(&format!("  {}(text: string): Promise<string>;", spec.name));
        if callable_from_a_cell(spec) {
            assert!(declared, "`{}` is in the roster and undeclared", spec.name);
            assert!(
                runtime.contains(spec.summary),
                "`{}`'s summary is not what the model is shown",
                spec.name
            );
        } else {
            assert!(
                !declared,
                "`{}` may not be called from a cell, so a cell must not be told it exists",
                spec.name
            );
        }
    }
    assert!(
        sterna::prompt::declarations::declares_global("helper"),
        "the isolate binds `helper` and the enumeration test reads this"
    );
}

/// `[helpers] enabled = false` must refuse **even with a model configured**.
///
/// The gate this pins fails OPEN when deleted: a user who turned helpers off
/// but left `model` set would have them run and spend, silently. That is the
/// direction that costs money, so it gets its own test rather than riding on
/// the unconfigured case.
#[test]
fn helpers_disabled_with_a_model_configured_still_refuse_and_never_reach_the_wire() {
    let _environment = ENV_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    let fixture = Fixture::new("disabled");
    let provider = provider("never asked");
    // SAFETY: the environment lock serialises every test in this file that
    // touches these variables, and no other thread here reads them.
    unsafe {
        std::env::set_var("ANTHROPIC_BASE_URL", &provider.url);
    }

    let mut runtime = Runtime::new(&fixture.profile(), &SessionId::new("helpers-disabled"))
        .with_helpers(HelpersConfig {
            model: "a-real-model".to_string().into(),
            enabled: false,
            calls_per_cell: 8,
            ..HelpersConfig::default()
        });
    let outcome = runtime.run_cell(
        "try { await helper.reduce(\"a log line\"); answer(\"no refusal\"); }\n\
         catch (e) { return e.name + \": \" + e.message; }\n",
    );

    unsafe {
        std::env::remove_var("ANTHROPIC_BASE_URL");
    }

    let caught = returned_text(&outcome);
    assert!(
        caught.starts_with("ToolError: "),
        "a disabled helper must refuse catchably, got {caught:?}"
    );
    assert!(
        caught.contains("enabled"),
        "the refusal must name `[helpers] enabled`, got {caught:?}"
    );
    assert_eq!(
        provider.requests.load(Ordering::SeqCst),
        0,
        "a disabled helper must not reach the wire even with a model set"
    );
}

/// The `helper` global carries **exactly** the roster entries a cell may
/// call: every one of them, and nothing else.
///
/// The declaration is generated from `HELPERS` and the bindings are installed
/// from `HELPERS`, so this is the test that fails the moment those two drift.
/// A declared helper that is not installed would hand the model a `TypeError`
/// where the declaration promises a catchable `ToolError`; an installed
/// helper that is not declared would be reachable from a cell the spec says
/// may not reach it. Both directions are the same equality, so it is asserted
/// as a set rather than as a loop that only checks one of them.
#[test]
fn every_declared_helper_is_actually_installed() {
    let fixture = Fixture::new("declared-installed");
    let mut runtime = Runtime::new(&fixture.profile(), &SessionId::new("helpers-declared"));

    let mut expected: Vec<&str> = sterna::helpers::HELPERS
        .iter()
        .filter(|spec| callable_from_a_cell(spec))
        .map(|spec| spec.name)
        .collect();
    expected.sort_unstable();
    assert!(
        !expected.is_empty(),
        "the roster must offer a cell at least one helper, or this proves nothing"
    );

    let installed = returned_text(
        &runtime.run_cell("return Object.getOwnPropertyNames(helper).sort().join(\",\");\n"),
    );
    assert_eq!(
        installed,
        expected.join(","),
        "the `helper` global must carry exactly the helpers a cell may call"
    );

    for name in expected {
        let kind = returned_text(&runtime.run_cell(&format!("return typeof helper[{name:?}];")));
        assert_eq!(
            kind, "function",
            "`helper.{name}` is declared to the model but is {kind} on the global"
        );
    }
}

/// A helper whose `call_sites` exclude `Cell` reaches neither the global nor
/// the declaration — `little-helpers.md`'s "where it may be invoked from".
///
/// `HELPERS` is a `const`, so no test can append a rogue entry to the shipped
/// roster and watch it be filtered out, and every entry today names `Cell`.
/// The seam is the predicate, driven with a legal spec naming every call site
/// *except* `Cell` — and because a predicate nothing consults filters nothing,
/// the second half reads `install` and pins that its roster loop is gated on
/// that same function, the way `session.rs` pins its startup validation.
#[test]
fn a_helper_that_may_not_be_called_from_a_cell_is_not_installed() {
    let preflight_only = HelperSpec {
        call_sites: &[
            CallSite::Preflight,
            CallSite::PostResult,
            CallSite::CompletionGate,
        ],
        ..REDUCER
    };
    assert!(
        sterna::helpers::check_spec(&preflight_only).is_ok(),
        "the rogue must be a legal spec, or its exclusion proves nothing"
    );
    assert!(
        !callable_from_a_cell(&preflight_only),
        "a spec whose call sites exclude `Cell` must be neither installed nor declared"
    );
    assert!(
        callable_from_a_cell(&HelperSpec {
            call_sites: &[CallSite::Preflight, CallSite::Cell],
            ..REDUCER
        }),
        "a spec naming `Cell` among its call sites must pass the same filter"
    );

    const BINDINGS: &str = include_str!("../src/runtime/bindings.rs");
    let loop_body = BINDINGS
        .split_once("for spec in crate::helpers::HELPERS {")
        .expect("`install` must bind the `helper` global from the roster itself")
        .1
        .split_once("\n    }")
        .expect("that loop must still close at one level inside `install`")
        .0;
    assert!(
        loop_body.contains("callable_from_a_cell"),
        "`install` binds every roster entry without consulting `call_sites`, so a \
         preflight-only helper would be on the global: {loop_body}"
    );
}

/// `helper` may not be shadowed by a cell's own binding.
///
/// The protected set is a list, and a test that iterates that list cannot
/// notice a name missing from it — which is exactly how `write`, `context` and
/// `edit` went unprotected until `9fdd763`. So `helper` is asserted by name.
#[test]
fn a_cell_may_not_shadow_the_helper_global() {
    let fixture = Fixture::new("shadow-helper");
    let mut runtime = Runtime::new(&fixture.profile(), &SessionId::new("helpers-shadow"));
    let outcome = runtime.run_cell("const marker = 2;\nconst helper = 1;\n");
    let (class, message) = threw(&outcome);
    assert_eq!(
        class, "ShadowsHostFunction",
        "declaring `helper` must be refused at compile time, got {message:?}"
    );
    assert!(
        !runtime.is_live("marker"),
        "the cell must be refused before it runs, but `marker` survived"
    );
}

/// **A tool-holding helper is callable, and is still a capability boundary.**
///
/// Replaces the guard that kept SCOUT and CHECKER off the cell path while two
/// hazards were open: a nested V8 isolate on the borrowed thread (now closed —
/// `run_with_tools` runs the loop on its own thread, the way `bg::serve_once`
/// does), and a runtime that bound every tool regardless of the spec (now
/// closed — `HostGlobals::Helper` carries the spec's own list).
///
/// Stated over the roster rather than by naming today's specs, so a spec added
/// later is held to the same two properties: it is reachable, and reaching it
/// grants nothing it did not name.
#[test]
fn a_tool_holding_helper_is_callable_and_grants_only_what_it_named() {
    let fixture = Fixture::new("tool-holding-callable");
    let mut runtime = Runtime::new(&fixture.profile(), &SessionId::new("helpers-tool-holding"));

    let mut checked = 0;
    for spec in sterna::helpers::HELPERS {
        if spec.tools.is_empty() {
            continue;
        }
        checked += 1;

        if spec.call_sites.contains(&CallSite::Cell) {
            let kind = returned_text(
                &runtime.run_cell(&format!("return typeof helper[{:?}];", spec.name)),
            );
            assert_eq!(
                kind, "function",
                "`{}` declares CallSite::Cell but is not on the global",
                spec.name
            );
        }

        // The boundary itself: whatever it holds, it cannot reach outside the
        // isolate. Driven through the real narrowed constructor, so this fails
        // if a spec's toolset ever admits a mutating tool.
        for forbidden in sterna::helpers::FORBIDDEN_TOOLS {
            assert!(
                !spec.tools.contains(&forbidden),
                "`{}` names the mutating tool `{forbidden}`",
                spec.name
            );
        }
    }
    assert!(
        checked >= 2,
        "the roster should carry the tool-holding specs this guards; checked {checked}"
    );
}

/// **A helper's runtime holds nothing that can cause an effect.**
///
/// `little-helpers.md` makes the toolset the safety boundary, and that is
/// true of the registered tools and false of the host globals installed
/// beside them: `bg.run` executes a command and `mcp.call` reaches a
/// server. Neither is a tool, so narrowing
/// `spec.tools` never touched them and a Scout could have shelled out.
///
/// The runtime is built the way `helpers::run_with_tools` builds one, through
/// `agent::run_narrowed`. The second half is what stops the first passing for
/// the wrong reason: an ordinary cell still holds all three, so a name that
/// does not exist would fail here rather than read as a narrowing.
#[test]
fn a_helpers_runtime_holds_no_global_that_can_cause_an_effect() {
    // Every name that reaches outside the isolate: the three host globals AND
    // the three mutating tools. The tools are the half that matters most --
    // `bash` executes, and a helper runs `.as_subagent()`, which skips the
    // approval gate entirely.
    const PROGRAM: &str = "return [\"bg\", \"mcp\", \"bash\", \"write\", \"edit\"]\n\
         \x20 .map(n => n + \"=\" + typeof globalThis[n]).join(\",\");\n";
    let fixture = Fixture::new("narrowed-globals");

    // A Scout-shaped toolset: read-only tools only, exactly as its spec names.
    let mut helper = Runtime::for_helper(
        &fixture.profile(),
        &SessionId::new("helpers-narrowed"),
        &["read", "grep"],
    )
    .as_subagent()
    .with_instruction_context();
    assert_eq!(
        returned_text(&helper.run_cell(PROGRAM)),
        "bg=undefined,mcp=undefined,bash=undefined,write=undefined,edit=undefined",
        "a helper's runtime must bind nothing that reaches outside the isolate"
    );
    assert_eq!(
        returned_text(
            &helper.run_cell(
                "return [\"read\", \"grep\"].map(n => typeof globalThis[n]).join(\",\");\n"
            )
        ),
        "function,function",
        "and it must still bind the tools its spec did name"
    );

    let mut cell = Runtime::new(&fixture.profile(), &SessionId::new("helpers-ordinary-cell"));
    assert_eq!(
        returned_text(&cell.run_cell(PROGRAM)),
        "bg=object,mcp=object,bash=function,write=function,edit=function",
        "an ordinary cell keeps every host global and tool it had"
    );
}

/// The declaration matches what is installed.
///
/// Telling a helper about a global its context does not bind buys a
/// `TypeError` on a name the system block promised, where the point of the
/// narrowing is that the capability is simply absent.
#[test]
fn a_helper_is_never_declared_a_global_its_runtime_does_not_hold() {
    let every = sterna::prompt::render_runtime();
    let helper = sterna::prompt::render_runtime_for(HostGlobals::Helper(&["read", "grep"]));
    for head in ["declare const bg: {", "declare const mcp: {"] {
        assert!(every.contains(head), "`{head}` is what a cell is shown");
        assert!(
            !helper.contains(head),
            "a helper is told about `{head}`, which its runtime does not bind"
        );
    }
    assert!(
        helper.contains("declare function keep("),
        "the narrowed block must still declare what a helper does hold"
    );
}

/// The production caller is what asks for the narrowing.
///
/// A helper's loop cannot be run from here without a wire call, so the seam is
/// pinned by reading `agent::run_narrowed`, exactly as this file pins
/// `install`'s roster loop: the branch that decides, the runtime it builds,
/// and the declaration it renders from the same value.
#[test]
fn the_narrowed_loop_is_what_asks_for_a_narrowed_runtime() {
    const AGENT: &str = include_str!("../src/agent.rs");
    let production = AGENT
        .split_once("#[cfg(test)]")
        .map_or(AGENT, |(before, _)| before);
    // The decisive expressions, not their surrounding shape: the narrowing is
    // derived from the SPEC'S OWN toolset, and the same value reaches both the
    // runtime and the system block — so a helper can never be told about a
    // capability it does not hold, nor hold one it was not told about.
    for named in [
        "HostGlobals::Helper(narrowed.tools)",
        "Runtime::for_helper(profile, session, tools)",
        "prompt::render_system_for(&instructions, &tools, &facts, globals)",
    ] {
        assert!(
            production.contains(named),
            "`run_narrowed` no longer carries `{named}`, so a helper's loop may hold \
             globals its spec never named"
        );
    }
}

// --- CallSite::PostResult -----------------------------------------------
//
// The pushed half nothing asks for: an oversized command result is reduced
// by the host, without the model spending a turn to request it. Every test
// below spawns a `bash`, and each is gated to macOS and Linux because those
// are the hosts where `bash` and its brace expansion are certainly present.
// Windows has had an applier since 2026-09-09 and no longer refuses, so the
// gate is now about the runner's own tools rather than about confinement;
// widening it is a named successor in `sandbox-grants.md` §7.

/// The whole grant these tests need. `printf` and brace expansion are both
/// bash builtins, so `bash` is the only binary that is ever exec'd.
#[cfg(any(target_os = "macos", target_os = "linux"))]
const PRINTF_ONLY: &str = r#"{"permissions":{"allow":["Bash(printf*)"]}}"#;

/// A reducer's answer in the shape the reducer now answers in: one
/// ```sterna-filter``` fence holding a function of the text, and prose beside
/// it.
///
/// The filter keeps the first three lines, which is exactly the must-keep
/// list `reduce_sample::must_keep` produces for an output whose every line
/// wears one failure shape — so it passes validation without the fixture
/// having to know what the marker was.
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn reducer_answer(prose: &str) -> String {
    format!("```sterna-filter\n(text) => text.split('\\n').slice(0, 3).join('\\n')\n```\n{prose}")
}

/// A command line whose output is comfortably over
/// `preview::STDOUT_TOKEN_CAP`, so the automatic reduction's own trigger is
/// what fires rather than a number this file chose.
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn oversized_command(marker: &str) -> String {
    format!(r#"printf '{marker} %s\n' {{1..4000}}"#)
}

/// `const r = await bash(…); return r.stdout.length + "|" + <the reduction>`.
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn report_program(command: &str) -> String {
    format!(
        "const r = await bash({{ command: {command:?} }});\n\
         return r.stdout.length + \"|\" + (r.reduced === undefined ? \"none\" : r.reduced);\n"
    )
}

/// Splits `"<length>|<reduction-or-none>"` back into the two facts.
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn reported(outcome: &CellOutcome) -> (usize, String) {
    let text = returned_text(outcome);
    let (length, reduction) = text
        .split_once('|')
        .unwrap_or_else(|| panic!("expected `<length>|<reduction>`, got {text:?}"));
    (length.parse().expect(length), reduction.to_string())
}

/// **A result the model could have printed whole is left alone.**
///
/// The reduction fires without being asked for, so the case that matters
/// most is the one where it must not fire at all: an ordinary command result
/// carries no `reduced`, leaves no record, and reaches no wire.
#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn a_small_command_result_is_untouched_and_costs_no_helper_call() {
    let _environment = ENV_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    let fixture = Fixture::new("post-result-small");
    let provider = provider("never asked");
    unsafe {
        std::env::set_var("ANTHROPIC_BASE_URL", &provider.url);
    }

    let mut runtime = Runtime::new(
        &fixture.profile_with(PRINTF_ONLY),
        &SessionId::new("post-result-small"),
    )
    .with_helpers(configured("test-helper-model", 8));
    let outcome = runtime.run_cell(&report_program(r"printf 'error: boom\n'"));

    unsafe {
        std::env::remove_var("ANTHROPIC_BASE_URL");
    }

    let (length, reduction) = reported(&outcome);
    assert_eq!(length, "error: boom\n".len(), "{outcome:?}");
    assert_eq!(
        reduction, "none",
        "a result under the cap is not worth a request"
    );
    assert_eq!(
        provider.requests.load(Ordering::SeqCst),
        0,
        "a small result must not reach the wire at all"
    );
    assert!(
        runtime.helper_records().is_empty(),
        "a call that never ran is not a helper record: {:?}",
        runtime.helper_records()
    );
}

/// **An oversized result is reduced, and the full output is still there.**
///
/// Three facts in one, because they are one path: the handle table offers
/// `reduced` as a key the next turn can read, the program still holds every
/// byte `stdout` carried, and one `HelperRecord` says what the lane and
/// `/cell` show. A summary that replaced the output would make the helper
/// the only witness to it; a reduction nothing names would never be read.
#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn an_oversized_command_result_is_reduced_and_the_full_output_remains() {
    let _environment = ENV_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    let fixture = Fixture::new("post-result-big");
    let provider = provider(&reducer_answer("3 distinct failures"));
    unsafe {
        std::env::set_var("ANTHROPIC_BASE_URL", &provider.url);
    }

    let mut runtime = Runtime::new(
        &fixture.profile_with(PRINTF_ONLY),
        &SessionId::new("post-result-big"),
    )
    .with_helpers(configured("test-helper-model", 8));
    let command = oversized_command("error: boom");
    let first = runtime.run_cell(&format!(
        "const r = await bash({{ command: {command:?} }});\n"
    ));

    // The turn the model actually gets: the reduction is a key it can read,
    // never text injected into its context — `little-helpers.md`'s "output
    // never injects itself".
    let table = match &first {
        CellOutcome::Yielded { turn } => turn.table.clone(),
        other => panic!("expected a yield, got {other:?}"),
    };
    assert!(
        table.contains("\"reduced\": string"),
        "the next turn must be told the reduction is there to read: {table}"
    );

    let records = runtime.helper_records();
    assert_eq!(records.len(), 1, "{records:?}");
    let record = &records[0];
    assert_eq!(record.helper, "reduce", "{record:?}");
    assert_eq!(record.verb, "reducing", "{record:?}");
    assert!(record.outcome.ok, "{record:?}");
    assert_eq!(record.asked, "4,000 lines", "{record:?}");
    assert_eq!(record.usage.requests, 1);
    assert_eq!(record.usage.reported_requests, 1);
    assert_eq!(record.usage.known_tokens(), 15);

    let (length, reduction) = reported(&runtime.run_cell(
        "return r.stdout.length + \"|\" + (r.reduced === undefined ? \"none\" : r.reduced);\n",
    ));

    unsafe {
        std::env::remove_var("ANTHROPIC_BASE_URL");
    }

    assert!(
        length / 4 > sterna::runtime::preview::STDOUT_TOKEN_CAP,
        "the full output must still be there, and over the cap: {length} chars"
    );
    assert!(
        reduction.ends_with("3 distinct failures"),
        "the reduction must reach the program as `reduced`: {reduction}"
    );
    // The reduction states its own lossiness, so a reader can tell three
    // failures out of three from three out of two hundred without going and
    // looking. A summary that looks complete is never re-checked.
    assert!(
        reduction.starts_with("[sterna:reduction 4,000 lines / 66,893 bytes → "),
        "the reduction must lead with what it left out: {reduction}"
    );
    assert!(
        reduction.contains("`stdout` and `stderr` on this result are complete and unchanged"),
        "the reduction must name where the whole output still is: {reduction}"
    );
    assert_eq!(
        provider.requests.load(Ordering::SeqCst),
        1,
        "one oversized result is one reduction"
    );
}

/// **The deterministic rung spends no request at all.**
///
/// Four thousand passing test lines are the ordinary shape of the output
/// that trips the threshold, and every one of them is already counted by the
/// `test result:` line beneath it. A rule removes them for free; a model
/// removing them costs a request, a wait, and -- measured on 2026-09-19 --
/// up to 4,227 output tokens against 77 lines of input.
///
/// Its opposite is `an_oversized_command_result_is_reduced_and_the_full_output_remains`
/// above: four thousand distinct `error: boom N` lines, which no rule may
/// touch, still reach the helper and still cost exactly one request.
#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn rules_alone_bring_a_test_log_under_the_threshold_and_no_request_is_made() {
    let _environment = ENV_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    let fixture = Fixture::new("post-result-ruled");
    let provider = provider("a reduction nobody should need");
    unsafe {
        std::env::set_var("ANTHROPIC_BASE_URL", &provider.url);
    }

    let mut runtime = Runtime::new(
        &fixture.profile_with(PRINTF_ONLY),
        &SessionId::new("post-result-ruled"),
    )
    .with_helpers(configured("test-helper-model", 8));
    let command = r#"printf 'test suite::case_%s ... ok\n' {1..4000}"#;
    let (length, reduction) = reported(&runtime.run_cell(&report_program(command)));

    unsafe {
        std::env::remove_var("ANTHROPIC_BASE_URL");
    }

    assert_eq!(
        provider.requests.load(Ordering::SeqCst),
        0,
        "a rule removed every line; no cheap-model token may be spent"
    );
    assert!(
        runtime.helper_records().is_empty(),
        "no helper ran: {:?}",
        runtime.helper_records()
    );
    assert!(
        length / 4 > sterna::runtime::preview::STDOUT_TOKEN_CAP,
        "the exact output must still be there, and over the cap: {length} chars"
    );
    assert!(
        reduction.contains("4000 passing or ignored test lines removed"),
        "the reduction must say what it removed and how much: {reduction}"
    );
    // The rung's own elision markers and the lossiness line answer different
    // questions -- which rule dropped what, and how big the whole thing was
    // -- so both belong, and neither repeats the other's number.
    assert!(
        reduction.starts_with("[sterna:reduction 4,000 lines / "),
        "a rules-only reduction states its sizes too: {reduction}"
    );
    assert!(
        reduction.contains("rules only, no model: passing-test-lines"),
        "and says no model was spent, naming the rules that fired: {reduction}"
    );
    assert_eq!(
        reduction.matches("[sterna:reduction").count(),
        1,
        "exactly one lossiness line, never one per rung: {reduction}"
    );
}

/// **Do not reduce the same value twice.**
///
/// A cell is code, so the same command inside a loop is the ordinary case.
/// The second identical result is served the reduction the first one paid
/// for: one request, one record, and both results carry it.
#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn the_same_output_is_never_reduced_twice() {
    let _environment = ENV_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    let fixture = Fixture::new("post-result-twice");
    let provider = provider(&reducer_answer("3 distinct failures"));
    unsafe {
        std::env::set_var("ANTHROPIC_BASE_URL", &provider.url);
    }

    let mut runtime = Runtime::new(
        &fixture.profile_with(PRINTF_ONLY),
        &SessionId::new("post-result-twice"),
    )
    .with_helpers(configured("test-helper-model", 8));
    let command = oversized_command("error: boom");
    // Compared inside the program rather than by joining both into one
    // returned string: a reduction now carries its provenance and its
    // partiality, and two of them joined exceed the preview head — which
    // would fail this test for the length of its own evidence.
    let outcome = runtime.run_cell(&format!(
        "const first = await bash({{ command: {command:?} }});\n\
         const second = await bash({{ command: {command:?} }});\n\
         return (first.reduced === second.reduced ? \"same\" : \"differs\")\n\
         \x20 + \"|\" + first.reduced.slice(0, 40);\n"
    ));

    unsafe {
        std::env::remove_var("ANTHROPIC_BASE_URL");
    }

    // Both halves carry the same served reduction, lossiness line included —
    // a cached reduction is the same answer, so it says the same thing about
    // itself.
    let text = returned_text(&outcome);
    let (same, head) = text.split_once("|").expect("a verdict and a head");
    assert_eq!(
        same, "same",
        "the second identical result must carry the very same reduction: {text}"
    );
    assert!(
        head.starts_with("[sterna:reduction "),
        "the served reduction keeps its lossiness line: {text}"
    );
    assert_eq!(
        provider.requests.load(Ordering::SeqCst),
        1,
        "the second identical result must not be reduced again"
    );
    assert_eq!(
        runtime.helper_records().len(),
        1,
        "a served reduction is not a second call"
    );
}

/// **With helpers unconfigured, an oversized result behaves exactly as
/// today.**
///
/// This is the gate that fails OPEN when it is deleted: a user who never
/// configured a helper would have one run and spend on every large command
/// result, silently.
#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn an_oversized_result_is_untouched_when_helpers_are_unconfigured() {
    let _environment = ENV_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    let fixture = Fixture::new("post-result-off");
    let provider = provider("never asked");
    unsafe {
        std::env::set_var("ANTHROPIC_BASE_URL", &provider.url);
    }

    // No `with_helpers`: the default carries no model, which is helpers off.
    let mut runtime = Runtime::new(
        &fixture.profile_with(PRINTF_ONLY),
        &SessionId::new("post-result-off"),
    );
    let outcome = runtime.run_cell(&report_program(&oversized_command("error: boom")));

    unsafe {
        std::env::remove_var("ANTHROPIC_BASE_URL");
    }

    let (length, reduction) = reported(&outcome);
    assert!(
        length / 4 > sterna::runtime::preview::STDOUT_TOKEN_CAP,
        "the fixture must be over the cap, got {length} chars"
    );
    assert_eq!(
        reduction, "none",
        "an unconfigured helper must leave the result exactly as it was"
    );
    assert_eq!(
        provider.requests.load(Ordering::SeqCst),
        0,
        "an unconfigured helper must not reach the wire at all"
    );
    assert!(
        runtime.helper_records().is_empty(),
        "a call that never ran is not a helper record"
    );
}

/// **The failure reducer only ever sees a command line's output** — the
/// defect a real session measured on 2026-09-17.
///
/// `REDUCER` reads build and test output for its distinct failures. Handed a
/// pure search tool's results it answers that there were none, truthfully and
/// uselessly: four such calls in one session, over `rg` results, cost 23,098
/// input tokens and 15.8 s for four answers of "No failures." `bash` is the
/// only tool that runs a command line, so it is the only one whose result
/// reaches the reducer; `jq` prints JSON and is the tool that still lands in
/// the process arm beside it (`rg` and `fd` are typed as matches and paths
/// now, and never arrive here at all).
#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn only_a_command_results_output_is_ever_reduced() {
    let _environment = ENV_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    let fixture = Fixture::new("post-result-jq");
    let provider = provider(&reducer_answer("3 distinct failures"));
    unsafe {
        std::env::set_var("ANTHROPIC_BASE_URL", &provider.url);
    }

    // Comfortably over `[helpers] reduce_above_tokens` once jq pretty-prints
    // it, so the trigger is the reduction's own and not a number chosen here.
    let document = fixture.root.join("big.json");
    let items: Vec<String> = (0..4000).map(|n| format!("error: boom {n}")).collect();
    std::fs::write(
        &document,
        serde_json::to_string(&items).expect("an array of strings serialises"),
    )
    .unwrap();

    let mut runtime = Runtime::new(
        &fixture.profile_with(PRINTF_ONLY),
        &SessionId::new("post-result-jq"),
    )
    .with_helpers(configured("test-helper-model", 8));

    let command = oversized_command("error: boom");
    let outcome = runtime.run_cell(&format!(
        "const j = await jq({{ filter: \".\", path: {document:?} }});\n\
         const b = await bash({{ command: {command:?} }});\n\
         return (j.stdout.length > 12000) + \"|\" + (j.reduced === undefined) + \"|\" + \
         (b.reduced === undefined ? \"none\" : b.reduced);\n",
        document = document.to_string_lossy()
    ));

    unsafe {
        std::env::remove_var("ANTHROPIC_BASE_URL");
    }

    let CellOutcome::Returned { value, .. } = &outcome else {
        panic!("expected a return, got {outcome:?}");
    };
    let sterna::runtime::preview::Value::String(text) = value else {
        panic!("expected a string, got {value:?}");
    };
    // The shape under test is *which* results carry a reduction at all, so
    // this reads the two flags and the presence of bash's reduction rather
    // than its exact text — which now leads with its own lossiness line.
    let head = text.head();
    assert!(
        head.starts_with("true|true|[sterna:reduction ") && head.ends_with("3 distinct failures"),
        "jq's output is oversized and carries no reduction, while bash's still does: {outcome:?}"
    );

    let records = runtime.helper_records();
    assert_eq!(
        records.len(),
        1,
        "exactly one reduction, and it is the command's: {records:?}"
    );
    assert_eq!(records[0].asked, "4,000 lines", "{records:?}");
    assert_eq!(
        provider.requests.load(Ordering::SeqCst),
        1,
        "the cheap model is asked once, about the command's output"
    );
}

/// **The per-cell ceiling still applies, and reaching it degrades rather
/// than throwing.**
///
/// A reduction the model did not ask for must never be the thing that fails
/// its program: the second oversized result simply arrives without
/// `reduced`, which is today's behaviour.
#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn the_cell_ceiling_bounds_reductions_nobody_asked_for() {
    let _environment = ENV_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    let fixture = Fixture::new("post-result-ceiling");
    let provider = provider(&reducer_answer("3 distinct failures"));
    unsafe {
        std::env::set_var("ANTHROPIC_BASE_URL", &provider.url);
    }

    let mut runtime = Runtime::new(
        &fixture.profile_with(PRINTF_ONLY),
        &SessionId::new("post-result-ceiling"),
    )
    .with_helpers(configured("test-helper-model", 1));
    // Two *different* oversized outputs, so the second is a fresh value and
    // the ceiling is the only thing that can stop it.
    let first = oversized_command("error: boom");
    let second = oversized_command("error: other");
    let outcome = runtime.run_cell(&format!(
        "const a = await bash({{ command: {first:?} }});\n\
         const b = await bash({{ command: {second:?} }});\n\
         return (a.reduced === undefined ? \"none\" : a.reduced)\n\
         \x20 + \"|\" + (b.reduced === undefined ? \"none\" : b.reduced);\n"
    ));

    unsafe {
        std::env::remove_var("ANTHROPIC_BASE_URL");
    }

    let text = returned_text(&outcome);
    assert!(
        text.starts_with("[sterna:reduction ") && text.ends_with("3 distinct failures|none"),
        "the call past the ceiling must be skipped, not thrown: {text}"
    );
    assert_eq!(
        provider.requests.load(Ordering::SeqCst),
        1,
        "the refused reduction must not reach the wire"
    );
    assert_eq!(
        runtime.helper_records().len(),
        1,
        "only the reduction that ran is a record"
    );
}

/// **A reduction nobody asked for never starves a call the model made.**
///
/// Both halves spend one per-cell budget, so an automatic reduction firing on
/// several oversized results could leave the model refused for a `helper.*`
/// call it did make. Slots are reserved for the pulled half.
///
/// Gated like its five siblings above, and for the same reason: it spends
/// `PRINTF_ONLY` and `oversized_command`, which are a bash grant and a brace
/// expansion. Those are gated to the two Unix hosts, so leaving this test
/// ungated does not make it run on Windows — it makes the file fail to
/// compile there, since `-D warnings` is the least of it once the names have
/// gone. It was added after the other five and simply lost the attribute.
#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn a_pushed_reduction_leaves_slots_for_the_models_own_calls() {
    let _environment = ENV_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    let fixture = Fixture::new("pushed-vs-pulled");
    let provider = provider("reduced");
    // SAFETY: the environment lock serialises every test in this file.
    unsafe {
        std::env::set_var("ANTHROPIC_BASE_URL", &provider.url);
    }

    // The default ceiling, so the reservation has room to bite.
    let mut runtime = Runtime::new(
        &fixture.profile_with(PRINTF_ONLY),
        &SessionId::new("pushed-vs-pulled"),
    )
    .with_helpers(configured("test-helper-model", 8));

    // NINE distinct oversized results against a ceiling of eight: without the
    // reservation the pushed half consumes every slot and the model's own call
    // is refused for a call it did make. Seven would not discriminate — the
    // model's call would still fit — and a mutation proved that version green.
    let mut program = String::new();
    for i in 0..9 {
        let command = oversized_command(&format!("error: boom {i}"));
        program.push_str(&format!("await bash({{ command: {command:?} }});\n"));
    }
    // Then a call the MODEL makes. It must still be served.
    program.push_str("return await helper.reduce(\"a log the model asked about\");\n");

    let outcome = runtime.run_cell(&program);

    unsafe {
        std::env::remove_var("ANTHROPIC_BASE_URL");
    }

    assert_eq!(
        returned_text(&outcome),
        "reduced",
        "the model's own helper call must survive the pushed reductions"
    );
}

/// **A partial reduction says which part is missing, because nothing else
/// can.**
///
/// A provider that truncates sets `stop_reason: "max_tokens"` and the call
/// fails outright — that answer never becomes `reduced`. The dangerous case
/// is the one this covers: a reduction that reads exactly like a complete
/// one and is not. Four thousand lines carry a failure marker here and the
/// reducer is shown three of them, because a must-keep list of four thousand
/// is the whole input and then no filter can shrink anything. So the model
/// could not have kept what it never saw, and the count of what it did not
/// see is the fact a reader needs — computed here, never estimated.
#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn a_partial_reduction_counts_the_marked_lines_the_reducer_never_saw() {
    let _environment = ENV_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    let fixture = Fixture::new("post-result-crowded");
    // 4,000 `error:` lines are ~16.7k tokens, so the scaled cap lands on its
    // 4,096 ceiling; 4,000 output tokens is 98% of it.
    let provider = provider_with_usage(
        &reducer_answer("3 distinct failures"),
        serde_json::json!({"input_tokens": 10, "output_tokens": 4000}),
    );
    unsafe {
        std::env::set_var("ANTHROPIC_BASE_URL", &provider.url);
    }

    let mut runtime = Runtime::new(
        &fixture.profile_with(PRINTF_ONLY),
        &SessionId::new("post-result-crowded"),
    )
    .with_helpers(configured("test-helper-model", 8));
    let command = oversized_command("error: boom");
    runtime.run_cell(&format!(
        "const r = await bash({{ command: {command:?} }});\n"
    ));
    let (_, reduction) = reported(&runtime.run_cell(
        "return r.stdout.length + \"|\" + (r.reduced === undefined ? \"none\" : r.reduced);\n",
    ));

    unsafe {
        std::env::remove_var("ANTHROPIC_BASE_URL");
    }

    assert!(
        reduction.contains("3,997 further marked lines"),
        "a partial reduction must count exactly what the reducer never saw: {reduction}"
    );
    assert!(
        reduction.contains("read this as partial"),
        "and say plainly that it is partial: {reduction}"
    );
    assert!(
        reduction.ends_with("3 distinct failures"),
        "the answer itself still arrives: {reduction}"
    );
}

/// **A roomy reduction does not cry wolf.**
///
/// The sibling of the test above, and the reason it is a separate one: a
/// partial-answer warning on every reduction would be noise, and noise is
/// how a real warning stops being read.
#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn a_reduction_with_room_to_spare_carries_no_warning() {
    let _environment = ENV_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    let fixture = Fixture::new("post-result-roomy");
    let provider = provider(&reducer_answer("3 distinct failures"));
    unsafe {
        std::env::set_var("ANTHROPIC_BASE_URL", &provider.url);
    }

    let mut runtime = Runtime::new(
        &fixture.profile_with(PRINTF_ONLY),
        &SessionId::new("post-result-roomy"),
    )
    .with_helpers(configured("test-helper-model", 8));
    // Nothing marked, so nothing was withheld from the reducer and the
    // selection is far under its budget: there is no partiality to report.
    let command = oversized_command("routine line");
    runtime.run_cell(&format!(
        "const r = await bash({{ command: {command:?} }});\n"
    ));
    let (_, reduction) = reported(&runtime.run_cell(
        "return r.stdout.length + \"|\" + (r.reduced === undefined ? \"none\" : r.reduced);\n",
    ));

    unsafe {
        std::env::remove_var("ANTHROPIC_BASE_URL");
    }

    assert!(
        !reduction.contains("read this as partial"),
        "a three-line selection with nothing withheld is not partial: {reduction}"
    );
    assert!(
        reduction.starts_with("[sterna:reduction "),
        "it still states its sizes: {reduction}"
    );
}

/// **A provider that truncates produces no `reduced` at all.**
///
/// The hard case, kept beside the soft one so the two are read together:
/// `stop_reason: "max_tokens"` is a failed call, and a failed call carries
/// `reduction_error` with the remedy rather than a half-summary the parent
/// might trust.
#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn a_truncated_reduction_is_a_failure_and_never_reaches_the_program_as_an_answer() {
    let _environment = ENV_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    let fixture = Fixture::new("post-result-truncated");
    let provider = scripted_provider(vec![serde_json::json!({
        "role": "assistant",
        "content": [{"type": "text", "text": "3 distinct fail"}],
        "stop_reason": "max_tokens",
        "usage": {"input_tokens": 10, "output_tokens": 4096}
    })]);
    unsafe {
        std::env::set_var("ANTHROPIC_BASE_URL", &provider.url);
    }

    let mut runtime = Runtime::new(
        &fixture.profile_with(PRINTF_ONLY),
        &SessionId::new("post-result-truncated"),
    )
    .with_helpers(configured("test-helper-model", 8));
    let command = oversized_command("error: boom");
    runtime.run_cell(&format!(
        "const r = await bash({{ command: {command:?} }});\n"
    ));
    let answer = returned_text(&runtime.run_cell(
        "return (r.reduced === undefined ? \"none\" : \"reduced\")\n\
         \x20 + \"|\" + (r.reduction_error === undefined ? \"none\" : r.reduction_error);\n",
    ));

    unsafe {
        std::env::remove_var("ANTHROPIC_BASE_URL");
    }

    let (reduced, error) = answer.split_once('|').expect("two fields");
    assert_eq!(
        reduced, "none",
        "a truncated answer must never arrive as `reduced`: {answer}"
    );
    assert!(
        error.contains("`stdout` and `stderr` are complete and unchanged"),
        "the failure names where the whole output still is: {answer}"
    );
}

/// **A filter is written once and answers every later run of the same tool.**
///
/// This is the cache that pays, and the reason the key is a shape and not a
/// digest. `reduction_of` is keyed on the SHA-256 of the exact bytes, so it
/// hits only when a command produced byte-identical output twice — true
/// inside a loop and almost never otherwise, because two runs of one tool
/// differ in their counts. A filter written for `error: boom 1..4000` is
/// correct for `error: boom 1..3900`, and `Shapes::signature` deliberately
/// drops the counts so the two share a key.
///
/// Measured here: three oversized results of one shape, one request.
#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn a_filter_written_for_one_shape_answers_every_later_run_of_it() {
    let _environment = ENV_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    let fixture = Fixture::new("post-result-shape");
    let provider = provider(&reducer_answer("3 distinct failures"));
    unsafe {
        std::env::set_var("ANTHROPIC_BASE_URL", &provider.url);
    }

    let mut runtime = Runtime::new(
        &fixture.profile_with(PRINTF_ONLY),
        &SessionId::new("post-result-shape"),
    )
    .with_helpers(configured("test-helper-model", 8));
    // Three different outputs — different byte counts, different digests —
    // wearing one shape.
    let outcome = runtime.run_cell(
        "const a = await bash({ command: \"printf 'error: boom %s\\\\n' {1..4000}\" });\n\
         const b = await bash({ command: \"printf 'error: boom %s\\\\n' {1..3900}\" });\n\
         const c = await bash({ command: \"printf 'error: boom %s\\\\n' {1..3800}\" });\n\
         return [a, b, c].map(r => r.reduced === undefined ? \"none\" : r.reduced.split(\"\\n\")[0]).join(\"@\");\n",
    );

    unsafe {
        std::env::remove_var("ANTHROPIC_BASE_URL");
    }

    // **The provenance is recomputed per application, never cached with the
    // filter.** A reused filter runs against a *different* log, so a served
    // line saying "4,000 lines → 3" about the 3,900-line run is precisely the
    // silent wrong answer this package exists to prevent — and it is the one
    // mistake a cache makes by default.
    let text = returned_text(&outcome);
    let lines: Vec<&str> = text.split('@').collect();
    assert_eq!(lines.len(), 3, "{text}");
    for (index, expected) in ["4,000 lines", "3,900 lines", "3,800 lines"]
        .into_iter()
        .enumerate()
    {
        assert!(
            lines[index].starts_with(&format!("[sterna:reduction {expected} / ")),
            "reduction {index} must state its own output's size, not an earlier one's: {}",
            lines[index],
        );
    }
    assert_eq!(
        provider.requests.load(Ordering::SeqCst),
        1,
        "one filter answers all three; the later two spend no request"
    );
    let stats = runtime.reduction_stats();
    assert_eq!(stats.filtered, 1, "{stats:?}");
    assert_eq!(stats.filter_reused, 2, "{stats:?}");
    assert_eq!(
        stats.cached, 0,
        "the digest cache cannot help here — three distinct outputs: {stats:?}"
    );
}

/// **The evidence and the reading of it are never the same text.**
///
/// A filter's output is evidence: every line occurred in the output, and the
/// validator is what makes that true. The prose beside it is the model's
/// reading, which is the half a filter cannot produce and the half that can
/// be wrong. A reader who cannot tell them apart has the worse of both, so
/// the prose is last and behind a marker naming whose words it is.
#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn a_reductions_evidence_and_its_prose_are_marked_apart() {
    let _environment = ENV_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    let fixture = Fixture::new("post-result-prose");
    let provider = provider(&reducer_answer(
        "two hundred failures, three distinct shapes",
    ));
    unsafe {
        std::env::set_var("ANTHROPIC_BASE_URL", &provider.url);
    }

    let mut runtime = Runtime::new(
        &fixture.profile_with(PRINTF_ONLY),
        &SessionId::new("post-result-prose"),
    )
    .with_helpers(configured("test-helper-model", 8));
    let command = oversized_command("error: boom");
    runtime.run_cell(&format!(
        "const r = await bash({{ command: {command:?} }});\n"
    ));
    let (_, reduction) = reported(&runtime.run_cell(
        "return r.stdout.length + \"|\" + (r.reduced === undefined ? \"none\" : r.reduced);\n",
    ));

    unsafe {
        std::env::remove_var("ANTHROPIC_BASE_URL");
    }

    let (evidence, prose) = reduction
        .split_once("[sterna:reduction notes, the reducer's own words]")
        .unwrap_or_else(|| panic!("the prose must be marked as the model's: {reduction}"));
    assert!(
        prose.trim() == "two hundred failures, three distinct shapes",
        "the model's own words come last: {prose:?}"
    );
    // Every line of the evidence half really occurred in the output.
    for line in evidence.lines().skip(1).filter(|line| !line.is_empty()) {
        assert!(
            line.starts_with("error: boom "),
            "the evidence half holds selected lines and nothing else: {line:?}"
        );
    }
}

/// **A filter that composes a line is refused, and the caller ends exactly
/// where it does today.**
///
/// The guarantee is that a reduction cannot say something the output did
/// not. So a filter returning a line nobody printed is rejected, the retry
/// is given the reason, and a second refusal leaves the parent with the
/// same `reduction_error` a failed helper has always produced — never a
/// composed line wearing evidence's clothes.
#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn a_filter_that_writes_its_own_line_is_refused_and_nothing_is_lost() {
    let _environment = ENV_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    let fixture = Fixture::new("post-result-composed");
    let provider =
        provider("```sterna-filter\n(text) => '1 test failed out of 4000'\n```\nmy own summary");
    unsafe {
        std::env::set_var("ANTHROPIC_BASE_URL", &provider.url);
    }

    let mut runtime = Runtime::new(
        &fixture.profile_with(PRINTF_ONLY),
        &SessionId::new("post-result-composed"),
    )
    .with_helpers(configured("test-helper-model", 8));
    let command = oversized_command("error: boom");
    runtime.run_cell(&format!(
        "const r = await bash({{ command: {command:?} }});\n"
    ));
    let outcome = runtime.run_cell(
        "return (r.reduced === undefined ? \"no-reduced\" : r.reduced)\n\
         \x20 + \"|\" + (r.reduction_error === undefined ? \"none\" : r.reduction_error)\n\
         \x20 + \"|\" + r.stdout.length;\n",
    );

    unsafe {
        std::env::remove_var("ANTHROPIC_BASE_URL");
    }

    let text = returned_text(&outcome);
    let parts: Vec<&str> = text.splitn(3, '|').collect();
    assert_eq!(
        parts[0], "no-reduced",
        "a composed line never reaches the program as a reduction: {text}"
    );
    assert!(
        parts[1].contains("does not occur in the input"),
        "and the parent is told why it got none: {text}"
    );
    assert!(
        parts[2].parse::<usize>().expect(parts[2]) > 40_000,
        "the exact output is untouched and still there: {text}"
    );
    assert_eq!(
        provider.requests.load(Ordering::SeqCst),
        2,
        "one retry, carrying the reason, and then it stops"
    );
    let stats = runtime.reduction_stats();
    assert_eq!(stats.filtered, 0, "{stats:?}");
    assert_eq!(stats.filter_rejected, 1, "{stats:?}");
    assert_eq!(stats.failed, 1, "{stats:?}");
}

/// A helper whose request never reached anyone says it failed once.
#[test]
fn a_failed_helper_request_says_it_failed_once() {
    let _environment = ENV_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    drop(listener);
    unsafe { std::env::set_var("ANTHROPIC_BASE_URL", &url) };
    let outcome = sterna::helpers::run_once(
        &REDUCER,
        sterna::helpers::HelperRoute {
            model: "test-helper-model",
            effort: sterna::wire::Effort::Medium,
            cap: None,
        },
        "a log line",
    );
    unsafe { std::env::remove_var("ANTHROPIC_BASE_URL") };
    assert!(!outcome.ok, "{outcome:?}");
    assert_eq!(
        outcome.text.matches("request failed").count(),
        1,
        "{}",
        outcome.text
    );
}
