use std::io::{BufReader, prelude::*};
use std::net::TcpListener;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use pane::config::HelpersConfig;
use pane::contract::SessionId;
use pane::runtime::isolate::Runtime;
use pane::runtime::outcome::CellOutcome;
use pane::runtime::repair::{SOURCE_BYTE_CAP, SyntaxFailure};
use pane::sandbox::profile::Profile;

#[path = "support/sse.rs"]
mod sse;

/// `ANTHROPIC_BASE_URL` is process-global, so the tests that set it are
/// serialised against each other.
static ENV_LOCK: Mutex<()> = Mutex::new(());

static NEXT: AtomicU64 = AtomicU64::new(0);

struct Fixture(PathBuf);

impl Fixture {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "pane-cell-repair-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(path.join(".claude")).unwrap();
        Self(path)
    }

    fn runtime(&self) -> Runtime {
        Runtime::new(
            &Profile::compile(&self.0, None),
            &SessionId::new("cell-repair"),
        )
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn exact_edit_requires_current_cell_one_nonempty_match_and_a_change() {
    let failure = SyntaxFailure::new(7, "const x = nope;\nconst y = nope;\n", 1, 0).unwrap();
    assert!(
        failure
            .apply(r#"{"cell":6,"replace":"x","with":"z"}"#)
            .is_err()
    );
    assert!(
        failure
            .apply(r#"{"cell":7,"replace":"","with":"z"}"#)
            .is_err()
    );
    assert!(
        failure
            .apply(r#"{"cell":7,"replace":"missing","with":"z"}"#)
            .is_err()
    );
    assert!(
        failure
            .apply(r#"{"cell":7,"replace":"nope","with":"yes"}"#)
            .is_err()
    );
    assert!(
        failure
            .apply(r#"{"cell":7,"replace":"x","with":"x"}"#)
            .is_err()
    );
    assert!(
        failure
            .apply(r#"{"cell":7,"replace":"x","with":"z","extra":1}"#)
            .is_err()
    );
    assert_eq!(
        failure
            .apply(r#"{"cell":7,"replace":"const x = nope;","with":"const x = yes;"}"#)
            .unwrap(),
        "const x = yes;\nconst y = nope;\n"
    );
}

#[test]
fn repair_input_and_result_are_bounded() {
    assert!(SyntaxFailure::new(1, &"x".repeat(SOURCE_BYTE_CAP + 1), 1, 0).is_none());
    let failure = SyntaxFailure::new(1, "x", 1, 0).unwrap();
    let json = format!(
        "{{\"cell\":1,\"replace\":\"x\",\"with\":\"{}\"}}",
        "y".repeat(SOURCE_BYTE_CAP)
    );
    assert!(failure.apply(&json).is_err());
    let failure =
        SyntaxFailure::new(1, &format!("x{}", "a".repeat(SOURCE_BYTE_CAP - 1)), 1, 0).unwrap();
    assert!(
        failure
            .apply(r#"{"cell":1,"replace":"x","with":"bb"}"#)
            .is_err()
    );
}

#[test]
fn only_parser_failure_is_eligible_and_any_next_run_consumes_it() {
    let fixture = Fixture::new();
    let mut runtime = fixture.runtime();

    runtime.run_cell("const = ;");
    let failure = runtime.syntax_failure().expect("parse failure is eligible");
    assert_eq!(failure.cell, 1);
    assert!(failure.hint().contains("Nothing in cell 1 ran"));

    let thrown = runtime.run_cell("globalThis.touched = 1; throw new SyntaxError('runtime');");
    assert!(
        matches!(thrown, pane::runtime::outcome::CellOutcome::Threw { ref error, .. } if error.class == "SyntaxError")
    );
    assert!(runtime.syntax_failure().is_none());

    runtime.run_cell("const = ;");
    assert_eq!(runtime.syntax_failure().unwrap().cell, 3);
    runtime.run_cell("const ok = 1;");
    assert!(runtime.syntax_failure().is_none());
}

#[test]
fn a_corrected_parse_failure_becomes_the_new_target_and_task_end_clears_it() {
    let fixture = Fixture::new();
    let mut runtime = fixture.runtime();

    runtime.run_cell("const = ;");
    let amended = runtime
        .syntax_failure()
        .unwrap()
        .apply(r#"{"cell":1,"replace":"const = ;","with":"let = ;"}"#)
        .unwrap();
    runtime.run_cell(&amended);
    assert_eq!(runtime.syntax_failure().unwrap().cell, 2);

    runtime.end_task();
    assert!(runtime.syntax_failure().is_none());
}

#[test]
fn overlapping_matches_are_ambiguous_and_replacements_are_literal_unicode() {
    let failure = SyntaxFailure::new(1, "界界界", 1, 0).unwrap();
    assert!(
        failure
            .apply(r#"{"cell":1,"replace":"界界","with":"x"}"#)
            .is_err()
    );
    let failure = SyntaxFailure::new(1, "const 世界 = 'bad;", 1, 0).unwrap();
    assert_eq!(
        failure
            .apply(r#"{"cell":1,"replace":"'bad;","with":"'$&\\n';"}"#)
            .unwrap(),
        "const 世界 = '$&\\n';"
    );
}

#[test]
fn the_hint_quotes_the_failing_line_with_a_caret_and_numbers_from_the_error() {
    let source = "const a = 1;\nconst b = 2;\nconst bad = \"unterminated\nconst c = 3;\nconst d = 4;\nconst e = 5;\n";
    // Line 3, column 12: the opening quote of the string that never closes.
    let failure = SyntaxFailure::new(4, source, 3, 12).unwrap();
    let hint = failure.hint();
    assert!(
        hint.contains("const bad = \"unterminated"),
        "the failing line is not quoted:\n{hint}"
    );
    assert!(
        hint.contains("3 | const bad"),
        "the gutter does not carry the error's own line number:\n{hint}"
    );
    assert!(hint.contains('^'), "no caret marks the column:\n{hint}");
    assert!(
        hint.contains("const a = 1;") && hint.contains("const c = 3;"),
        "the surrounding lines are missing:\n{hint}"
    );
    assert!(
        !hint.contains("const e = 5;"),
        "the excerpt is unbounded:\n{hint}"
    );
    assert!(
        hint.contains("```pane-edit"),
        "the repair fence was lost:\n{hint}"
    );
    // The caret sits under the column the `## Error` block names, counting
    // characters from the start of the line exactly as `line_and_column` does.
    let caret = hint
        .lines()
        .find(|line| line.contains('^'))
        .expect("a caret line");
    let gutter = caret.find('|').expect("a gutter") + 2;
    assert_eq!(
        caret[gutter..].chars().take_while(|c| *c == ' ').count(),
        12,
        "the caret is not at column 12:\n{hint}"
    );
}

#[test]
fn a_position_the_source_does_not_have_quotes_nothing_and_still_offers_the_fence() {
    for (line, column) in [(0, 0), (99, 3)] {
        let failure = SyntaxFailure::new(2, "const a = 1;\n", line, column).unwrap();
        let hint = failure.hint();
        assert!(
            !hint.contains('^'),
            "a caret was drawn for line {line}:\n{hint}"
        );
        assert!(
            hint.contains("Nothing in cell 2 ran.") && hint.contains("```pane-edit"),
            "the fence was lost for line {line}:\n{hint}"
        );
    }
    // A line longer than the quoted width is clipped rather than reprinted.
    let long = format!("const x = \"{}\";\n", "a".repeat(400));
    let hint = SyntaxFailure::new(1, &long, 1, 5).unwrap().hint();
    assert!(hint.contains('…'), "a long line was not clipped:\n{hint}");
    assert!(
        hint.lines().all(|line| line.chars().count() < 260),
        "a clipped line is still too wide:\n{hint}"
    );
}

#[test]
fn a_real_parse_failure_hands_back_the_line_the_model_wrote() {
    let fixture = Fixture::new();
    let mut runtime = fixture.runtime();

    runtime.run_cell(
        "const one = 1;\nconst two = 2;\nconst broken = \"no closing quote\nconst four = 4;\n",
    );
    let hint = runtime
        .syntax_failure()
        .expect("parse failure is eligible")
        .hint();
    assert!(
        hint.contains("const broken = \"no closing quote"),
        "the model cannot see the line it wrote:\n{hint}"
    );
    assert!(
        hint.contains("```pane-edit"),
        "the repair fence was lost:\n{hint}"
    );
}

// --- the mender -------------------------------------------------------
//
// The fixture is the failure watched live on 2026-09-19: a model writing
// Rust source, which carries its own escapes, through a JavaScript string.
// One level of escaping is lost and `\u{whole_set}` becomes a code-point
// escape whose digits are not hex.

/// The program as the model sent it, which does not parse.
const STACKED: &str = "const rust = \"let s = \\\"\\u{whole_set}\\\";\";\nawait write({path: \"out.rs\", content: rust});\n";

/// The one fence that repairs it: the lost backslash, put back.
const STACKED_FENCE: &str = r#"{"cell":1,"replace":"\\u{whole_set}","with":"\\\\u{whole_set}"}"#;

#[test]
fn a_stacked_escape_mend_is_accepted_and_names_what_it_changed() {
    let failure = SyntaxFailure::new(1, STACKED, 1, 26).unwrap();
    let mended = failure.mend(STACKED_FENCE).expect("the repair is accepted");
    assert!(
        mended.amended.contains(r"\\u{whole_set}"),
        "the escape is now literal: {}",
        mended.amended
    );
    assert!(
        mended.amended.contains("await write("),
        "the rest of the program is untouched: {}",
        mended.amended
    );
    // The parent is told a size, and it is a punctuation-sized one.
    assert!(
        mended.before <= 16 && mended.after <= 16,
        "a stacked-escape repair is small: {} -> {}",
        mended.before,
        mended.after
    );
}

#[test]
fn a_mend_may_not_delete_a_statement() {
    let failure = SyntaxFailure::new(1, STACKED, 1, 26).unwrap();
    let deletion =
        r#"{"cell":1,"replace":"await write({path: \"out.rs\", content: rust});\n","with":""}"#;
    let refused = failure.mend(deletion).expect_err("a deletion is refused");
    assert!(
        refused.contains("shorter"),
        "the refusal names the shortening: {refused}"
    );
    // The same edit is still available to the parent, which authored the
    // program and may change it however it likes.
    assert!(
        failure.apply(deletion).is_ok(),
        "only a mend is bounded; an authored pane-edit is not"
    );
}

#[test]
fn a_mend_that_only_shortens_a_little_is_accepted() {
    let failure = SyntaxFailure::new(1, "const x = ((1);\n", 1, 13).unwrap();
    let mended = failure
        .mend(r#"{"cell":1,"replace":"((1)","with":"(1)"}"#)
        .expect("one character removed is a repair, not a deletion");
    assert_eq!(mended.amended, "const x = (1);\n");
}

#[test]
fn the_brief_carries_the_error_the_quoted_line_and_the_whole_program() {
    let failure = SyntaxFailure::new(7, STACKED, 1, 26).unwrap();
    let brief = failure.brief("SyntaxError", "Invalid Unicode escape sequence");
    assert!(brief.contains("SyntaxError: Invalid Unicode escape sequence"));
    assert!(brief.contains("line 1, column 26"));
    assert!(
        brief.contains("1 | const rust"),
        "the line is quoted: {brief}"
    );
    assert!(
        brief.contains("await write("),
        "the whole program is shown, because `replace` must be unique in it"
    );
    assert!(brief.contains("fence for cell 7"));
}

/// A listener answering every request with one assistant message, counting
/// what it was asked — the count is how "one attempt" is proved.
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
            let request: serde_json::Value = serde_json::from_slice(&body).unwrap();
            seen.fetch_add(1, Ordering::SeqCst);
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

fn fenced(json: &str) -> String {
    format!("```pane-edit\n{json}\n```")
}

fn helpers() -> HelpersConfig {
    HelpersConfig {
        model: "test-mender".to_string().into(),
        enabled: true,
        ..HelpersConfig::default()
    }
}

/// Runs one cell against a provider that answers every helper request with
/// `reply`, and returns the outcome and how many requests were made.
fn cell_with_mender(source: &str, reply: &str) -> (CellOutcome, usize, String) {
    let fixture = Fixture::new();
    let provider = provider(reply);
    let guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    // SAFETY: serialised by ENV_LOCK, as every test in this crate that
    // points the wire at a listener does.
    unsafe {
        std::env::set_var("ANTHROPIC_BASE_URL", &provider.url);
    }
    let mut runtime = fixture.runtime().with_helpers(helpers());
    let outcome = runtime.run_cell(source);
    unsafe {
        std::env::remove_var("ANTHROPIC_BASE_URL");
    }
    drop(guard);
    let stdout = match &outcome {
        CellOutcome::Returned { turn, .. } | CellOutcome::Yielded { turn } => {
            turn.stdout_tail.clone()
        }
        CellOutcome::Threw { turn, .. } => turn.stdout_tail.clone(),
    };
    (outcome, provider.requests.load(Ordering::SeqCst), stdout)
}

#[test]
fn a_cell_that_did_not_parse_is_mended_and_runs_and_the_parent_is_told() {
    let (outcome, requests, stdout) = cell_with_mender(STACKED, &fenced(STACKED_FENCE));
    assert_eq!(requests, 1, "one mend, one request");
    assert!(
        !matches!(outcome, CellOutcome::Threw { .. }),
        "the repaired cell ran: {outcome:?}"
    );
    assert!(
        stdout.contains("[pane:mend"),
        "the parent is told a helper repaired its program: {stdout:?}"
    );
    assert!(
        stdout.contains("did not parse"),
        "and what the failure was: {stdout:?}"
    );
}

#[test]
fn a_mend_is_attempted_once_and_a_second_failure_is_the_parents() {
    // A fence that applies but still does not parse: the amended cell fails,
    // and nothing tries again.
    let useless = r#"{"cell":1,"replace":"const rust","with":"const  rust"}"#;
    let (outcome, requests, _) = cell_with_mender(STACKED, &fenced(useless));
    assert_eq!(requests, 1, "one attempt, never a loop");
    assert!(
        matches!(outcome, CellOutcome::Threw { .. }),
        "the parent gets the failure it would have got anyway"
    );
}

#[test]
fn a_cell_that_threw_at_runtime_is_never_sent_to_the_mender() {
    let (outcome, requests, _) =
        cell_with_mender("throw new Error('real');\n", &fenced(STACKED_FENCE));
    assert_eq!(requests, 0, "a runtime throw is a result, not a typo");
    assert!(matches!(outcome, CellOutcome::Threw { .. }));
}

#[test]
fn a_mend_that_breaks_the_bounds_is_refused_and_costs_the_cell_nothing() {
    let deletion =
        r#"{"cell":1,"replace":"await write({path: \"out.rs\", content: rust});\n","with":""}"#;
    let (outcome, requests, stdout) = cell_with_mender(STACKED, &fenced(deletion));
    assert_eq!(requests, 1);
    assert!(
        matches!(outcome, CellOutcome::Threw { .. }),
        "a refused mend leaves today's behaviour exactly as it was"
    );
    assert!(
        !stdout.contains("[pane:mend"),
        "and claims no repair: {stdout:?}"
    );
}
