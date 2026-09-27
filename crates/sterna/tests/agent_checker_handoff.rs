//! A checker invoked inside the same cell that answers must be observed by a
//! later parent turn before that cell's candidate completion is accepted.
//!
//! A cell ends the task by calling `answer(text)`, so a cell that answers
//! *and* runs a checker has claimed completion before it could read what the
//! checker said. Every fixture here answers where it means to finish --
//! parent, checker and the later corrected turn alike.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use sterna::agent::AgentOptions;
use sterna::contract::SessionId;
use sterna::sandbox::profile::Profile;

static NEXT: AtomicU64 = AtomicU64::new(0);
static ENV_LOCK: Mutex<()> = Mutex::new(());

struct Fixture {
    root: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "sterna-checker-handoff-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(root.join(".sterna")).unwrap();
        std::fs::write(
            root.join(".sterna/config.toml"),
            "[helpers]\nacceptance_list = false\nmodel = \"checker-model\"\n\n[permissions]\nallow = [\"Read(**)\", \"Write(**)\"]\n",
        )
        .unwrap();
        Self { root }
    }

    fn profile(&self) -> Profile {
        Profile::compile(
            &self.root,
            Some(r#"{"permissions":{"allow":["Read(**)","Write(**)"]}}"#),
        )
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

#[path = "support/sse.rs"]
mod sse;

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
            let text = String::from_utf8_lossy(&body).into_owned();
            let request: serde_json::Value =
                serde_json::from_str(&text).unwrap_or(serde_json::Value::Null);
            recorded.lock().unwrap().push(text);

            // CHECKER holds tools, so its loop streams and is bounded by
            // silence rather than by duration (`wire::SIDE_ERRAND_SILENCE`).
            // Answer in whichever transport the request asked for.
            let (content_type, payload) = sse::response_for(&request, &reply.to_string());
            let _ = write!(
                stream,
                "HTTP/1.1 200 OK\r\ncontent-type: {content_type}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                payload.len(),
                payload
            );
        }
    });
    (format!("http://{address}"), requests)
}

#[test]
fn checker_and_candidate_are_handed_to_a_later_parent_turn() {
    let _environment = ENV_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    let fixture = Fixture::new();
    let candidate = "Everything is complete with no unresolved limitations.";
    let first_observation = "DOES NOT HOLD: required final verification is absent.";
    let second_observation = "CANNOT TELL: the draft exists, but sufficiency was not verified.";
    let (base_url, requests) = scripted_provider(vec![
        reply(
            "parent-first",
            &format!(
                "await write({{path: \"result.txt\", content: \"draft retained\"}}); \
                 await helper.check(\"Check required verification.\"); \
                 await helper.check(\"Check implementation sufficiency.\"); answer({candidate:?});"
            ),
        ),
        reply("checker-first", &format!("answer({first_observation:?});")),
        reply(
            "checker-second",
            &format!("answer({second_observation:?});"),
        ),
        reply(
            "parent-second",
            "const saved = await read({path: \"result.txt\"}); \
             answer(\"Checker could not verify the draft; \" + saved.text);",
        ),
    ]);
    // SAFETY: every test in this file holds the same lock for the whole wire
    // exchange, and the variable is removed before the guard is released.
    unsafe { std::env::set_var("ANTHROPIC_BASE_URL", base_url) };

    let result = sterna::agent::run(
        &fixture.profile(),
        &SessionId::new("checker-handoff"),
        "Implement and verify the requested change.",
        &AgentOptions {
            turns: Some(3),
            deadline: None,
            model: "parent-model".into(),
            effort: sterna::wire::Effort::default(),
        },
        &sterna::tools::invoke::CancellationToken::new(),
    );
    unsafe { std::env::remove_var("ANTHROPIC_BASE_URL") };

    assert_eq!(result.status, "returned", "{result:?}");
    assert_eq!(
        result.answer,
        "Checker could not verify the draft; draft retained"
    );
    assert_eq!(
        result.turns, 2,
        "the checker is not a duplicate parent turn"
    );
    assert_eq!(
        result.tokens, 36,
        "helper usage is not counted as parent usage"
    );
    assert_eq!(
        result.trajectory,
        ["write", "helper.check", "helper.check", "read"],
        "each actual operation is accounted once"
    );
    assert_eq!(
        std::fs::read_to_string(fixture.root.join("result.txt")).unwrap(),
        "draft retained",
        "the deferred completion must not roll back the cell's real edit"
    );

    let requests = requests.lock().unwrap();
    assert_eq!(requests.len(), 4, "two parent turns plus two checker calls");
    let later_parent = &requests[3];
    let handoff = &later_parent[later_parent
        .find("Candidate completion (deferred)")
        .expect("the later request carries the deterministic handoff")..];
    assert!(
        handoff.contains(candidate),
        "the returned candidate was hidden from the later parent turn: {later_parent}"
    );
    assert!(
        handoff.contains(first_observation) && handoff.contains(second_observation),
        "a checker outcome was hidden from the later parent turn: {later_parent}"
    );
    assert_eq!(
        handoff.matches(candidate).count(),
        1,
        "the candidate must be handed off exactly once"
    );
    assert_eq!(
        handoff.matches(first_observation).count(),
        1,
        "the first checker outcome must be handed off exactly once"
    );
    assert_eq!(
        handoff.matches(second_observation).count(),
        1,
        "the second checker outcome must be handed off exactly once"
    );
}

#[test]
fn scripted_session_defers_a_same_cell_candidate_until_after_checker_observation() {
    let _environment = ENV_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    let fixture = Fixture::new();
    let candidate = "Everything is complete with no unresolved limitations.";
    let observation = "CANNOT TELL: the requested verification was not demonstrated.";
    let corrected = "The edit is retained; verification remains unresolved.";
    let (base_url, requests) = scripted_provider(vec![
        reply(
            "session-first",
            &format!(
                "await write({{path: \"session-result.txt\", content: \"retained\"}}); \
                 await helper.check(\"Check the final claim.\"); answer({candidate:?});"
            ),
        ),
        reply("session-checker", &format!("answer({observation:?});")),
        reply("session-second", &format!("answer({corrected:?});")),
    ]);
    let rollout = fixture.root.join("rollout.jsonl");
    let output = Command::new(env!("CARGO_BIN_EXE_sterna"))
        .arg("session")
        .arg("--root")
        .arg(&fixture.root)
        .arg("--rollout")
        .arg(&rollout)
        .arg("--session")
        .arg("checker-session-path")
        .arg("--model")
        .arg(sterna::wire::MODEL)
        .arg("--task")
        .arg("Implement the change and check it before completing.")
        .env("ANTHROPIC_BASE_URL", base_url)
        .env("XDG_CONFIG_HOME", fixture.root.join("global-config"))
        .env_remove("ANTHROPIC_AUTH_TOKEN")
        .env_remove("ANTHROPIC_API_KEY")
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "session failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        std::fs::read_to_string(fixture.root.join("session-result.txt")).unwrap(),
        "retained",
        "the guarded return must retain edits from its cell"
    );
    let requests = requests.lock().unwrap();
    assert_eq!(requests.len(), 3, "two task turns and one checker request");
    let later_parent = &requests[2];
    let handoff = &later_parent[later_parent
        .find("Candidate completion (deferred)")
        .expect("the interactive session sent the completion guard observation")..];
    assert!(
        handoff.contains(candidate),
        "candidate missing: {later_parent}"
    );
    assert!(
        handoff.contains(observation),
        "checker observation missing: {later_parent}"
    );
    let rollout = std::fs::read_to_string(rollout).unwrap();
    assert!(
        rollout.contains(corrected),
        "the corrected later completion was not recorded: {rollout}"
    );
    assert_eq!(
        rollout.matches("\"helper\":\"check\"").count(),
        1,
        "the cell-local checker record must enter persisted accounting once: {rollout}"
    );
}
