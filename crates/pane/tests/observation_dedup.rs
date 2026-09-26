//! Acceptance for `smarter-cheaper-roadmap.md`, *Semantic read/search
//! lifting* (observation deduplication) and *Adaptive result reduction*
//! (pushed reduction routing).
//!
//! **No test here reaches a real provider.** The reduction tests point
//! `ANTHROPIC_BASE_URL` at a listener this file owns, copied from
//! `tests/helpers.rs`, and count what it was asked.
//!
//! Gated like `tests/lifting.rs`: every test runs a capability through a
//! real runtime, which on Windows would be refused rather than run
//! unconfined.
#![cfg(any(target_os = "macos", target_os = "linux"))]

use pane::config::HelpersConfig;
use pane::contract::SessionId;
use pane::runtime::isolate::Runtime;
use pane::runtime::outcome::{CellOutcome, Ended};
use pane::runtime::preview::Value;
use pane::sandbox::profile::Profile;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

#[path = "support/sse.rs"]
mod sse;

/// A grant that admits the shell command the lifting test writes, so the
/// only thing under test is whether the two spellings dedup against each
/// other.
const ADMITS: &str = r#"{"permissions":{"allow":["Bash","Read(**)"]}}"#;

fn fixture(name: &str) -> std::path::PathBuf {
    let root = std::env::temp_dir().join(format!(
        "pane-observation-dedup-{name}-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("notes.txt"), "alpha\nbeta\n").unwrap();
    root
}

fn runtime(root: &std::path::Path, permissions: &str, session: &str) -> Runtime {
    let profile = Profile::compile(root, Some(permissions));
    Runtime::new(&profile, &SessionId::new(session))
}

fn returned_text(outcome: &CellOutcome) -> String {
    match outcome {
        CellOutcome::Returned {
            value: Value::String(text),
            ..
        } => text.head().to_string(),
        other => panic!("expected a returned string, got {other:?}"),
    }
}

// --- observation deduplication -------------------------------------------

/// The same `read` twice: the second call runs, returns the same bytes, and
/// is recorded as a repeat of the first — in the trajectory and in the
/// table header, where the model can see it without re-reading the body.
#[test]
fn the_same_read_twice_is_a_repeat_the_table_and_the_trajectory_both_say() {
    let root = fixture("twice");
    let mut runtime = runtime(&root, ADMITS, "twice");
    let first = runtime.run_cell("const a = await read({path:'notes.txt'});");
    assert_eq!(first.turn().record.calls[0].ended, Ended::Ok, "{first:?}");
    assert_eq!(first.turn().record.calls[0].repeat_of, None);
    assert_eq!(runtime.observation_repeats(), 0);

    let second = runtime.run_cell("const b = await read({path:'notes.txt'});");
    let call = &second.turn().record.calls[0];
    assert_eq!(call.ended, Ended::Ok, "{second:?}");
    assert_eq!(call.repeat_of, Some(1), "{call:?}");
    assert_eq!(runtime.observation_repeats(), 1);
    let json = serde_json::to_string(call).unwrap();
    assert!(json.contains(r#""repeat_of":1"#), "{json}");

    let handle = second
        .turn()
        .record
        .handles
        .iter()
        .find(|handle| handle.name == "b")
        .unwrap_or_else(|| panic!("{:?}", second.turn().record.handles));
    assert_eq!(
        handle.type_name,
        "File (unchanged since cell 1, same as `a`)"
    );
    assert!(
        second
            .turn()
            .table
            .contains("unchanged since cell 1, same as `a`"),
        "{}",
        second.turn().table
    );
    // The first handle's label is untouched: it is the original observation.
    let original = second
        .turn()
        .record
        .handles
        .iter()
        .find(|handle| handle.name == "a")
        .unwrap();
    assert_eq!(original.type_name, "File");

    // Task end resets the count with the handles.
    runtime.end_task();
    assert_eq!(runtime.observation_repeats(), 0);
    let _ = std::fs::remove_dir_all(root);
}

/// The call is never skipped: after the file changes the same `read` is a
/// fresh observation, and nothing says otherwise.
#[test]
fn a_read_after_the_file_changed_is_not_a_repeat() {
    let root = fixture("changed");
    let mut runtime = runtime(&root, ADMITS, "changed");
    runtime.run_cell("const a = await read({path:'notes.txt'});");
    std::fs::write(root.join("notes.txt"), "alpha\nbeta\ngamma\n").unwrap();
    let second = runtime.run_cell("const b = await read({path:'notes.txt'});");
    let call = &second.turn().record.calls[0];
    assert_eq!(call.ended, Ended::Ok, "{second:?}");
    assert_eq!(call.repeat_of, None, "{call:?}");
    assert_eq!(runtime.observation_repeats(), 0);
    let handle = second
        .turn()
        .record
        .handles
        .iter()
        .find(|handle| handle.name == "b")
        .unwrap();
    assert_eq!(handle.type_name, "File");
    let text = returned_text(&runtime.run_cell("return b.text;"));
    assert!(
        text.contains("gamma"),
        "the fresh bytes reached the program"
    );
    let _ = std::fs::remove_dir_all(root);
}

/// A lifted `cat` and a direct `read` of the same file both reach `read`
/// with the same checked path, so they dedup against each other — in either
/// order.
#[test]
fn a_lifted_cat_and_a_direct_read_dedup_against_each_other() {
    let root = fixture("lifted");
    let path = root.join("notes.txt");
    let mut runtime = runtime(&root, ADMITS, "lifted");
    let command = format!("cat {}", path.to_string_lossy());
    let lifted = runtime.run_cell(&format!(
        "const viaShell = await shell({{ command: {} }});",
        serde_json::to_string(&command).unwrap()
    ));
    let first = &lifted.turn().record.calls[0];
    assert_eq!(first.tool, "read", "{lifted:?}");
    assert_eq!(first.lifted_from.as_deref(), Some("cat"));
    assert_eq!(first.repeat_of, None);

    let direct = runtime.run_cell(&format!(
        "const viaRead = await read({{ path: {:?} }});",
        path.to_string_lossy()
    ));
    let second = &direct.turn().record.calls[0];
    assert_eq!(second.tool, "read", "{direct:?}");
    assert_eq!(second.repeat_of, Some(1), "{second:?}");
    assert!(
        direct
            .turn()
            .table
            .contains("unchanged since cell 1, same as `viaShell`"),
        "{}",
        direct.turn().table
    );

    // And a second lifted `cat` repeats the first observation too.
    let again = runtime.run_cell(&format!(
        "const viaShellAgain = await shell({{ command: {} }});",
        serde_json::to_string(&command).unwrap()
    ));
    assert_eq!(again.turn().record.calls[0].repeat_of, Some(1), "{again:?}");
    assert_eq!(runtime.observation_repeats(), 2);
    let _ = std::fs::remove_dir_all(root);
}

/// An effectful call is never a repeat, however identical its output: the
/// second `bash` ran again and its result is its own observation.
#[test]
fn an_effectful_call_is_never_a_repeat() {
    let root = fixture("effectful");
    let mut runtime = runtime(&root, ADMITS, "effectful");
    let outcome = runtime.run_cell(
        "const a = await bash({command:\"printf same\"});\n\
         const b = await bash({command:\"printf same\"});",
    );
    let calls = &outcome.turn().record.calls;
    assert_eq!(calls.len(), 2, "{outcome:?}");
    assert_eq!(calls[1].repeat_of, None);
    assert_eq!(runtime.observation_repeats(), 0);
    let _ = std::fs::remove_dir_all(root);
}

// --- reduction routing ---------------------------------------------------
//
// The fake provider below is `tests/helpers.rs`'s listener, copied rather
// than shared: it answers every request with one assistant message and
// counts what it was asked.

/// `ANTHROPIC_BASE_URL` is process-global, so the tests that set it are
/// serialised against each other.
static ENV_LOCK: Mutex<()> = Mutex::new(());

struct Provider {
    url: String,
    requests: Arc<AtomicUsize>,
}

fn provider(text: &str) -> Provider {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let requests = Arc::new(AtomicUsize::new(0));
    let seen = requests.clone();
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
            seen.fetch_add(1, Ordering::SeqCst);
            let request: serde_json::Value =
                serde_json::from_slice(&body).unwrap_or(serde_json::Value::Null);
            let whole = serde_json::json!({
                "role": "assistant",
                "content": [{"type": "text", "text": reply}],
                "usage": {"input_tokens": 10, "output_tokens": 5}
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
    }
}

/// `printf` and brace expansion are bash builtins, so `bash` is the only
/// binary ever exec'd.
const PRINTF_ONLY: &str = r#"{"permissions":{"allow":["Bash(printf*)"]}}"#;

/// A reducer's answer in its new shape: one ```pane-filter``` fence holding
/// a function of the text, and prose beside it. The filter keeps the first
/// three lines, which is the must-keep list for an output whose every line
/// wears one failure shape.
fn reducer_answer(prose: &str) -> String {
    format!("```pane-filter\n(text) => text.split('\\n').slice(0, 3).join('\\n')\n```\n{prose}")
}

/// A threshold far below the default, so the trigger under test is the
/// configured one and not `STDOUT_TOKEN_CAP`.
const THRESHOLD: usize = 64;

fn helpers() -> HelpersConfig {
    HelpersConfig {
        model: Some("test-helper-model".to_string()),
        enabled: true,
        calls_per_cell: 8,
        reduce_above_tokens: THRESHOLD,
        ..HelpersConfig::default()
    }
}

fn report_program(command: &str) -> String {
    format!(
        "const r = await bash({{ command: {command:?} }});\n\
         return r.stdout.length + \"|\" + (r.reduced === undefined ? \"none\" : r.reduced);\n"
    )
}

fn reported(outcome: &CellOutcome) -> (usize, String) {
    let text = returned_text(outcome);
    let (length, reduction) = text
        .split_once('|')
        .unwrap_or_else(|| panic!("expected `<length>|<reduction>`, got {text:?}"));
    (length.parse().expect(length), reduction.to_string())
}

/// Well over the threshold and over the saving line: the reducer's own
/// `max_tokens` (1,024) plus half the threshold is about 4,300 characters.
fn large_command() -> String {
    r"printf 'error: boom %s\n' {1..600}".to_string()
}

/// A result above the configured threshold reaches the reducer once, and
/// the task's figures say so.
#[test]
fn a_result_above_the_configured_threshold_is_reduced_once_and_counted() {
    let _environment = ENV_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    let root = fixture("reduce-big");
    let provider = provider(&reducer_answer("3 distinct failures"));
    unsafe {
        std::env::set_var("ANTHROPIC_BASE_URL", &provider.url);
    }
    let mut runtime = runtime(&root, PRINTF_ONLY, "reduce-big").with_helpers(helpers());
    let outcome = runtime.run_cell(&report_program(&large_command()));
    unsafe {
        std::env::remove_var("ANTHROPIC_BASE_URL");
    }

    let (length, reduction) = reported(&outcome);
    assert!(
        length / 4 > THRESHOLD,
        "{length} chars is not over the threshold"
    );
    assert!(
        length / 4 < pane::runtime::preview::STDOUT_TOKEN_CAP,
        "the old cap must not be what fired: {length} chars"
    );
    // The answer arrives behind the line that says how much was left out.
    assert!(
        reduction.starts_with("[pane:reduction 600 lines / ")
            && reduction.ends_with("3 distinct failures"),
        "{reduction}"
    );
    assert_eq!(provider.requests.load(Ordering::SeqCst), 1);
    let stats = runtime.reduction_stats();
    assert_eq!(stats.attempted, 1, "{stats:?}");
    assert_eq!(stats.made, 1, "{stats:?}");
    assert_eq!(stats.failed, 0, "{stats:?}");
    assert_eq!(stats.cached, 0, "{stats:?}");
    assert_eq!(
        stats.filtered, 1,
        "the reduction came from a filter: {stats:?}"
    );
    // **`bytes_in` is what the reducer was handed, and that is no longer the
    // dump.** It is shown a sample — sizes, a histogram of line shapes, head,
    // tail and the lines it must keep — because a filter is written from the
    // *shape* of an output rather than from all of it. Measured here on a
    // 600-line log: 9,492 bytes of output became 1,865 of sample, and the
    // saving grows with the input because the sample's bulk does not.
    assert!(
        stats.bytes_in < length as u64 / 4,
        "the sample must be a fraction of the output it describes: \
         {} bytes of sample for {length} bytes of output",
        stats.bytes_in,
    );
    // `bytes_out` measures the answer the program received, lossiness line
    // included, so a served copy and a fresh one count the same value.
    assert_eq!(stats.bytes_out, reduction.len() as u64);

    runtime.end_task();
    assert_eq!(runtime.reduction_stats(), Default::default());
    let _ = std::fs::remove_dir_all(root);
}

/// The second identical output is served from the digest cache: one
/// request, and the figures count the hit as `cached`.
#[test]
fn an_identical_output_is_served_from_the_cache_and_counted() {
    let _environment = ENV_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    let root = fixture("reduce-cache");
    let provider = provider(&reducer_answer("3 distinct failures"));
    unsafe {
        std::env::set_var("ANTHROPIC_BASE_URL", &provider.url);
    }
    let mut runtime = runtime(&root, PRINTF_ONLY, "reduce-cache").with_helpers(helpers());
    let command = large_command();
    // Compared inside the program: a reduction carries its provenance and its
    // partiality now, and two of them joined exceed the preview head — which
    // would fail this test for the length of its own evidence.
    let outcome = runtime.run_cell(&format!(
        "const first = await bash({{ command: {command:?} }});\n\
         const second = await bash({{ command: {command:?} }});\n\
         return (first.reduced === second.reduced ? \"same\" : \"differs\")\n\
         \x20 + \"|\" + first.reduced.slice(0, 30)\n\
         \x20 + \"|\" + first.reduced.slice(-19);\n"
    ));
    unsafe {
        std::env::remove_var("ANTHROPIC_BASE_URL");
    }

    let text = returned_text(&outcome);
    let parts: Vec<&str> = text.split('|').collect();
    assert_eq!(
        parts[0], "same",
        "the served copy is the same answer: {text}"
    );
    assert!(parts[1].starts_with("[pane:reduction "), "{text}");
    assert_eq!(parts[2], "3 distinct failures", "{text}");
    assert_eq!(provider.requests.load(Ordering::SeqCst), 1);
    let stats = runtime.reduction_stats();
    assert_eq!(stats.attempted, 1, "{stats:?}");
    assert_eq!(stats.made, 1, "{stats:?}");
    assert_eq!(stats.cached, 1, "{stats:?}");
    // Both the fresh reduction and the served copy count the same value, so
    // the pair cannot disagree about one cache.
    assert_eq!(
        stats.bytes_out % 2,
        0,
        "the served copy counts what the fresh one did: {stats:?}"
    );
    assert!(stats.bytes_out > 0, "{stats:?}");
    let _ = std::fs::remove_dir_all(root);
}

/// Below the threshold nothing is attempted; and just above it, where the
/// reducer's own answer would cost about as much as it saves, nothing is
/// attempted either.
#[test]
fn a_result_below_the_threshold_or_the_saving_line_never_makes_a_request() {
    let _environment = ENV_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    let root = fixture("reduce-small");
    let provider = provider("never asked");
    unsafe {
        std::env::set_var("ANTHROPIC_BASE_URL", &provider.url);
    }
    let mut runtime = runtime(&root, PRINTF_ONLY, "reduce-small").with_helpers(helpers());
    let small = runtime.run_cell(&report_program(r"printf 'x %s\n' {1..20}"));
    let (small_length, small_reduction) = reported(&small);
    assert!(small_length / 4 < THRESHOLD, "{small_length}");
    assert_eq!(small_reduction, "none");

    // Over the threshold, but the expected saving is not positive: about
    // 125 tokens of output minus a 1,024-token answer is no saving at all.
    let middling = runtime.run_cell(&report_program(r"printf 'x %s\n' {1..100}"));
    let (middling_length, middling_reduction) = reported(&middling);
    assert!(middling_length / 4 > THRESHOLD, "{middling_length}");
    assert_eq!(middling_reduction, "none");
    unsafe {
        std::env::remove_var("ANTHROPIC_BASE_URL");
    }

    assert_eq!(provider.requests.load(Ordering::SeqCst), 0);
    assert_eq!(runtime.reduction_stats(), Default::default());
    assert!(runtime.helper_records().is_empty());
    let _ = std::fs::remove_dir_all(root);
}
