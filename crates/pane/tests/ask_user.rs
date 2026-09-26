//! `ask(question, choices)`: one question a running program puts to the
//! person, and the three ways it can be refused before anyone sees it.
//!
//! The invariant these tests exist for is the second one below: **a session
//! with nobody at the keyboard throws at the call and the task carries on.**
//! A question that could block would let a program stall a session it was
//! asked to finish, so the refusal is synchronous, catchable, and leaves the
//! rest of the cell running.

use pane::ask::{Answer, AnsweredBy, Question, Weights};
use pane::config::{AskJev, PaneConfig};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

static NEXT: AtomicU64 = AtomicU64::new(0);

struct Fixture {
    root: PathBuf,
}

impl Fixture {
    fn new(config: &str) -> Self {
        let root = std::env::temp_dir().join(format!(
            "pane-ask-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(root.join(".pane")).unwrap();
        std::fs::write(root.join(".pane/config.toml"), config).unwrap();
        Self { root }
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn reply(id: &str, code: &str) -> serde_json::Value {
    serde_json::json!({
        "role": "assistant",
        "content": [{
            "type": "tool_use",
            "id": id,
            "name": "execute_cell",
            "input": {"code": code}
        }],
        "usage": {"input_tokens": 11, "output_tokens": 7}
    })
}

fn scripted_provider(replies: Vec<serde_json::Value>) -> (String, Arc<Mutex<Vec<String>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let requests = Arc::new(Mutex::new(Vec::new()));
    let recorded = Arc::clone(&requests);
    std::thread::spawn(move || {
        for reply in replies {
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
            recorded
                .lock()
                .unwrap()
                .push(String::from_utf8_lossy(&body).into_owned());
            let payload = reply.to_string();
            let _ = write!(
                stream,
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                payload.len(),
                payload
            );
        }
    });
    (format!("http://{address}"), requests)
}

/// Runs a scripted `--task` session, which is never interactive.
fn run(fixture: &Fixture, replies: Vec<serde_json::Value>) -> (std::process::Output, Vec<String>) {
    let (base_url, requests) = scripted_provider(replies);
    let output = Command::new(env!("CARGO_BIN_EXE_pane"))
        .arg("session")
        .arg("--root")
        .arg(&fixture.root)
        .arg("--rollout")
        .arg(fixture.root.join("rollout.jsonl"))
        .arg("--session")
        .arg("ask-session")
        .arg("--model")
        .arg(pane::wire::MODEL)
        .arg("--task")
        .arg("Decide what to do and finish.")
        .env("ANTHROPIC_BASE_URL", base_url)
        .env("XDG_CONFIG_HOME", fixture.root.join("global-config"))
        .env_remove("ANTHROPIC_AUTH_TOKEN")
        .env_remove("ANTHROPIC_API_KEY")
        .output()
        .unwrap();
    let recorded = requests.lock().unwrap().clone();
    (output, recorded)
}

const WRITABLE: &str =
    "[helpers]\nacceptance_list = false\n\n[permissions]\nallow = [\"Read(**)\", \"Write(**)\"]\n";

/// **The invariant.** With nobody at the keyboard `ask` throws where it was
/// called, the program catches it, and everything after the catch still runs.
/// Nothing waits, and the session ends on its own.
#[test]
fn a_session_with_nobody_at_the_keyboard_throws_and_the_task_carries_on() {
    let fixture = Fixture::new(&format!("{WRITABLE}\n[ask]\nenabled = true\n"));
    let code = "let caught = \"nothing thrown\";\n\
                try { ask(\"Which way?\", [\"left\", \"right\"]); }\n\
                catch (error) { caught = error.message; }\n\
                await write({path: \"caught.txt\", content: caught});\n\
                await write({path: \"after.txt\", content: \"the cell kept running\"});\n\
                answer(\"decided without asking\");";
    let (output, requests) = run(&fixture, vec![reply("ask-refused", code)]);

    assert!(
        output.status.success(),
        "session failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let caught = std::fs::read_to_string(fixture.root.join("caught.txt")).unwrap();
    assert!(
        caught.contains("no one is at this session to ask"),
        "the refusal must say nobody is there: {caught}"
    );
    assert_eq!(
        std::fs::read_to_string(fixture.root.join("after.txt")).unwrap(),
        "the cell kept running",
        "a refused ask must not end the cell"
    );
    assert_eq!(
        requests.len(),
        1,
        "the task finished in its own turn rather than waiting for anybody"
    );
}

/// A question is refused for its shape before it reaches anybody, and that
/// refusal is catchable like any other.
#[test]
fn a_question_that_offers_one_answer_is_refused_at_the_call() {
    let fixture = Fixture::new(&format!("{WRITABLE}\n[ask]\nenabled = true\n"));
    let code = "let caught = \"nothing thrown\";\n\
                try { ask(\"Which way?\", [\"only one\"]); }\n\
                catch (error) { caught = error.message; }\n\
                await write({path: \"caught.txt\", content: caught});\n\
                answer(\"done\");";
    let (output, _) = run(&fixture, vec![reply("ask-bounds", code)]);
    assert!(output.status.success());
    let caught = std::fs::read_to_string(fixture.root.join("caught.txt")).unwrap();
    assert!(
        caught.contains("at least two choices") || caught.contains("no one is at this session"),
        "a one-choice question is refused: {caught}"
    );
}

/// `ask` is a host function on the persistent scope, so a program cannot
/// bind the name and answer its own questions.
#[test]
fn ask_is_a_host_function_a_program_cannot_replace() {
    let fixture = Fixture::new(&format!("{WRITABLE}\n[ask]\nenabled = true\n"));
    let code = "const ask = () => \"mine\";\nanswer(\"shadowed\");";
    let (output, _) = run(&fixture, vec![reply("ask-shadow", code)]);
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        combined.contains("ShadowsHostFunction") || combined.contains("host function"),
        "binding `ask` must be refused: {combined}"
    );
}

/// The model is told the capability exists, and told that it throws.
#[test]
fn the_runtime_block_declares_ask_and_says_it_can_throw() {
    let fixture = Fixture::new(&format!("{WRITABLE}\n[ask]\nenabled = true\n"));
    let (_, requests) = run(&fixture, vec![reply("ask-decl", "answer(\"done\");")]);
    let first = requests.first().expect("one request was made");
    assert!(
        first.contains("declare function ask(question: string, choices: string[])"),
        "the declaration must reach the model"
    );
    assert!(
        first.contains("catch it and decide for yourself"),
        "the model must be told the call can throw and what to do then"
    );
}

// -- the decision the feature makes ---------------------------------------

fn weights(confidence: f64) -> Weights {
    Weights {
        probabilities: vec![1.0 - confidence, confidence],
        choice: "right".to_string(),
        confidence,
    }
}

/// **`decide` answers at the bar, `weight` never answers at all.** The bar is
/// the confidence a person said they would accept, so an answer landing
/// exactly on it is one they already agreed to.
#[test]
fn jev_answers_for_the_person_only_in_decide_mode_and_only_at_or_above_the_bar() {
    assert!(
        weights(0.85).decides(AskJev::Decide, 0.85),
        "exactly at the bar is good enough"
    );
    assert!(weights(0.90).decides(AskJev::Decide, 0.85));
    assert!(
        !weights(0.84).decides(AskJev::Decide, 0.85),
        "below the bar the person is asked"
    );
    assert!(
        !weights(1.0).decides(AskJev::Weight, 0.85),
        "weighting explains the choice; it never takes it"
    );
    assert!(!weights(1.0).decides(AskJev::Off, 0.85));
}

/// A model's answer never reads as the person's, so a program branching on
/// it can tell the difference.
#[test]
fn the_observation_names_who_answered() {
    let person = Answer {
        choice: Some("left".into()),
        by: AnsweredBy::Person,
    };
    assert!(person.rendered().contains("the person chose"));

    let decided = Answer {
        choice: Some("left".into()),
        by: AnsweredBy::Decision { confidence: 0.91 },
    };
    let rendered = decided.rendered();
    assert!(
        rendered.contains("decision model chose at 0.91"),
        "{rendered}"
    );
    assert!(!rendered.contains("the person chose"), "{rendered}");

    assert!(Answer::dismissed().rendered().contains("decide yourself"));
}

/// A question is bounded where it is built, so the panel, the callback and
/// the observation cannot disagree about what one is.
#[test]
fn a_question_is_bounded_where_it_is_built() {
    assert!(Question::new("Which?", vec!["a".into(), "b".into()]).is_ok());
    assert!(Question::new("Which?", vec!["a".into()]).is_err());
    let ten: Vec<String> = (0..10).map(|n| n.to_string()).collect();
    assert!(Question::new("Which?", ten).is_err());
    assert!(Question::new("", vec!["a".into(), "b".into()]).is_err());
}

// -- configuration ---------------------------------------------------------

/// Off unless asked for: a capability that can suspend work is never one a
/// session gains by saying nothing.
#[test]
fn asking_is_off_until_the_project_turns_it_on() {
    let defaults = PaneConfig::parse("").unwrap().ask;
    assert!(!defaults.enabled);
    assert_eq!(defaults.jev, AskJev::Weight);
    assert!((defaults.decide_above - 0.85).abs() < f64::EPSILON);

    let configured =
        PaneConfig::parse("[ask]\nenabled = true\njev = \"decide\"\ndecide_above = 0.95\n")
            .unwrap()
            .ask;
    assert!(configured.enabled);
    assert_eq!(configured.jev, AskJev::Decide);
    assert!((configured.decide_above - 0.95).abs() < f64::EPSILON);
}

#[test]
fn an_unknown_ask_key_or_an_out_of_range_bar_is_refused_by_name() {
    let unknown = PaneConfig::parse("[ask]\nenabldd = true\n").unwrap_err();
    assert!(unknown.contains("enabldd"), "{unknown}");

    let low = PaneConfig::parse("[ask]\ndecide_above = 0.2\n").unwrap_err();
    assert!(low.contains("decide_above"), "{low}");
    let high = PaneConfig::parse("[ask]\ndecide_above = 1.5\n").unwrap_err();
    assert!(high.contains("decide_above"), "{high}");

    let word = PaneConfig::parse("[ask]\njev = \"sometimes\"\n").unwrap_err();
    assert!(word.contains("sometimes"), "{word}");
}
