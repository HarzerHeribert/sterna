//! Acceptance for `decide.choice` — the decision model asked by the running
//! program rather than by the harness (the user, 2026-09-17: *"Same for jev —
//! things which are near code but need eval"*).
//!
//! **No test here reaches a real provider.** Each one points
//! `ANTHROPIC_BASE_URL` at a loopback fake this file owns, answering
//! `/v1/systemone` with a `choice` for whatever key the request named.

use serde_json::Value as Json;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use sterna::config::DecisionsConfig;
use sterna::contract::SessionId;
use sterna::runtime::isolate::Runtime;
use sterna::runtime::outcome::CellOutcome;
use sterna::runtime::preview::Value;
use sterna::sandbox::profile::Profile;

/// `ANTHROPIC_BASE_URL` is process-global, so every test that sets it is
/// serialised against the others in this file.
static ENV_LOCK: Mutex<()> = Mutex::new(());
static COUNTER: AtomicU64 = AtomicU64::new(0);

struct Fixture {
    root: PathBuf,
}

impl Fixture {
    fn new(label: &str) -> Self {
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "sterna-cell-judgements-{label}-{}-{n}",
            std::process::id()
        ));
        std::fs::create_dir_all(root.join(".claude")).unwrap();
        Self { root }
    }

    fn profile(&self) -> Profile {
        Profile::compile(&self.root, Some(r#"{"permissions":{"allow":[]}}"#))
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// A loopback fake answering every `/v1/systemone` question with `choice` at
/// `confidence`, and counting the requests so a test can prove how many were
/// made — or how few.
fn fake(choice: &str, confidence: f64) -> (String, Arc<AtomicUsize>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let hits = Arc::new(AtomicUsize::new(0));
    let seen = hits.clone();
    let choice = choice.to_string();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { return };
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut request_line = String::new();
            if reader.read_line(&mut request_line).unwrap_or(0) == 0 {
                return;
            }
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
                    length = value.trim().parse().unwrap_or(0);
                }
            }
            let mut body = vec![0; length];
            if reader.read_exact(&mut body).is_err() {
                return;
            }
            seen.fetch_add(1, Ordering::SeqCst);
            let request: Json = serde_json::from_slice(&body).unwrap();
            let answers: serde_json::Map<String, Json> = request["questions"]
                .as_object()
                .map(|map| map.keys().cloned().collect::<Vec<_>>())
                .unwrap_or_default()
                .into_iter()
                .map(|key| {
                    (
                        key,
                        serde_json::json!({
                            "type": "choice",
                            "choice": choice,
                            "probabilities": {choice.clone(): confidence},
                            "confidence": confidence,
                        }),
                    )
                })
                .collect();
            let payload = serde_json::json!({
                "model": "jev-latest",
                "answers": answers,
                "usage": {"input_tokens": 20, "output_tokens": 5},
            })
            .to_string();
            let _ = write!(
                stream,
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{payload}",
                payload.len()
            );
        }
    });
    (url, hits)
}

fn decisions(model: Option<&str>) -> DecisionsConfig {
    DecisionsConfig {
        model: model.map(str::to_string),
        ..DecisionsConfig::default()
    }
}

fn returned_string(outcome: &CellOutcome) -> String {
    match outcome {
        CellOutcome::Returned { value, .. } => match value {
            Value::String(text) => text.head().to_string(),
            other => panic!("expected a string, got {other:?}"),
        },
        other => panic!("expected a return, got {other:?}"),
    }
}

/// The chain the declaration advertises: hold some evidence, ask for a
/// judgement about it, branch on the answer — one cell, one turn.
#[test]
fn a_cell_asks_for_a_judgement_and_branches_on_the_answer() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    let (url, hits) = fake("wider", 0.91);
    unsafe {
        std::env::set_var("ANTHROPIC_BASE_URL", &url);
    }
    let fixture = Fixture::new("branch");
    let session = SessionId::new("judgement-session");
    let mut runtime =
        Runtime::new(&fixture.profile(), &session).with_decisions(decisions(Some("jev-latest")));

    let outcome = runtime.run_cell(
        "const call = await decide.choice(\n\
         \x20 \"Does this diff do more than rename a symbol?\",\n\
         \x20 {rename_only: \"every hunk renames one symbol\", wider: \"anything else changed\"},\n\
         \x20 \"-  let old = 1;\\n+  let renamed = 1;\\n+  fs.remove(path);\");\n\
         if (call.choice === \"wider\" && call.confidence > 0.8) { return \"look closer\"; }\n\
         return \"rename only\";\n",
    );

    assert_eq!(returned_string(&outcome), "look closer");
    assert_eq!(
        hits.load(Ordering::SeqCst),
        1,
        "one question, one request — the judgement costs no turn and no second call"
    );
    unsafe {
        std::env::remove_var("ANTHROPIC_BASE_URL");
    }
}

/// The question and its answer are in the cell's record, so a person reading
/// the transcript sees what was asked and what came back.
#[test]
fn the_question_and_its_answer_reach_the_cells_record() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    let (url, _hits) = fake("rename_only", 0.77);
    unsafe {
        std::env::set_var("ANTHROPIC_BASE_URL", &url);
    }
    let fixture = Fixture::new("record");
    let session = SessionId::new("record-session");
    let mut runtime =
        Runtime::new(&fixture.profile(), &session).with_decisions(decisions(Some("jev-latest")));

    let outcome = runtime.run_cell(
        "return (await decide.choice(\"Is this a rename?\",\n\
         \x20 {rename_only: \"only a rename\", wider: \"more than a rename\"}, \"a diff\")).choice;\n",
    );

    assert_eq!(returned_string(&outcome), "rename_only", "what came back");
    let (CellOutcome::Returned { turn, .. }
    | CellOutcome::Threw { turn, .. }
    | CellOutcome::Yielded { turn }) = &outcome;
    let judgement = turn
        .record
        .calls
        .iter()
        .find(|call| call.tool == "decide.choice")
        .expect("the judgement is in the cell's record");
    assert!(
        judgement
            .args
            .get("asked")
            .is_some_and(|asked| asked.contains("Is this a rename?")),
        "the record says what was asked: {:?}",
        judgement.args
    );
    assert!(matches!(
        judgement.ended,
        sterna::runtime::outcome::Ended::Ok
    ));
    unsafe {
        std::env::remove_var("ANTHROPIC_BASE_URL");
    }
}

/// An unconfigured session holds no `decide` at all — the same answer `web`
/// gives when `[web]` names nothing.
#[test]
fn a_session_with_no_decision_model_binds_nothing_and_is_told_of_nothing() {
    let fixture = Fixture::new("unconfigured");
    let session = SessionId::new("unconfigured-session");
    let mut runtime = Runtime::new(&fixture.profile(), &session).with_decisions(decisions(None));

    let outcome = runtime.run_cell("return typeof decide;\n");
    assert_eq!(
        returned_string(&outcome),
        "undefined",
        "an unset `[decisions] model` binds no `decide`"
    );

    // And it is told of nothing, which is the other half of this test's name:
    // the Runtime block is rendered through the same predicate the binding
    // answers to, so a session without a decision model sees no `decide`.
    // (Until `Reach` carried `decisions`, this assertion read the other way
    // and pinned the gap rather than the intent.)
    let block = sterna::prompt::render_runtime_reaching(
        sterna::runtime::bindings::HostGlobals::Every,
        sterna::prompt::Reach::webbed(None),
    );
    assert!(
        !block.contains("declare const decide:"),
        "an unconfigured session was told about `decide`: {block}"
    );
}

/// A failed question throws catchably. Nothing here can return text that
/// reads like a judgement when no judgement was made.
#[test]
fn a_question_that_fails_throws_rather_than_answering() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    // A port nobody is listening on: the request cannot complete.
    let closed = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", closed.local_addr().unwrap());
    drop(closed);
    unsafe {
        std::env::set_var("ANTHROPIC_BASE_URL", &url);
    }
    let fixture = Fixture::new("failed");
    let session = SessionId::new("failed-session");
    let mut runtime =
        Runtime::new(&fixture.profile(), &session).with_decisions(decisions(Some("jev-latest")));

    let outcome = runtime.run_cell(
        "try {\n\
         \x20 await decide.choice(\"Is this a rename?\", {a: \"one\", b: \"two\"}, \"x\");\n\
         \x20 return \"answered\";\n\
         } catch (error) { return `caught:${error.name}`; }\n",
    );
    assert_eq!(
        returned_string(&outcome),
        "caught:ToolError",
        "a failed question is catchable, never an answer"
    );
    unsafe {
        std::env::remove_var("ANTHROPIC_BASE_URL");
    }
}

/// A cell asks a bounded number of questions, so a loop cannot keep buying
/// judgements.
#[test]
fn a_cell_asks_a_bounded_number_of_judgements() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    let (url, hits) = fake("a", 0.9);
    unsafe {
        std::env::set_var("ANTHROPIC_BASE_URL", &url);
    }
    let fixture = Fixture::new("allowance");
    let session = SessionId::new("allowance-session");
    let mut runtime =
        Runtime::new(&fixture.profile(), &session).with_decisions(decisions(Some("jev-latest")));

    let outcome = runtime.run_cell(
        "let made = 0;\n\
         try {\n\
         \x20 for (let i = 0; i < 12; i++) {\n\
         \x20   await decide.choice(\"q\", {a: \"one\", b: \"two\"}, \"x\");\n\
         \x20   made++;\n\
         \x20 }\n\
         \x20 return `all:${made}`;\n\
         } catch (error) { return `stopped:${made}`; }\n",
    );
    assert_eq!(
        returned_string(&outcome),
        "stopped:8",
        "a cell asks at most eight questions"
    );
    assert_eq!(
        hits.load(Ordering::SeqCst),
        8,
        "the refusal happens before the request, so nothing was spent on the ninth"
    );
    unsafe {
        std::env::remove_var("ANTHROPIC_BASE_URL");
    }
}
