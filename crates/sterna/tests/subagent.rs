//! Phase 64 end to end: a subagent runs a real turn loop against a provider
//! and its answer comes back as an event, not as a blocking return.
//!
//! The provider here is a local socket answering a canned Messages reply, so
//! nothing in this file reaches a network or a model.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};
use sterna::agent::AgentOptions;
use sterna::bg;
use sterna::contract::SessionId;
use sterna::events::Kind;
use sterna::sandbox::profile::Profile;

static COUNTER: AtomicU64 = AtomicU64::new(0);
static ENV_LOCK: Mutex<()> = Mutex::new(());

struct Fixture {
    root: PathBuf,
    session: SessionId,
}

impl Fixture {
    fn new(label: &str) -> Self {
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "sterna-subagent-{}-{label}-{n}",
            std::process::id()
        ));
        std::fs::create_dir_all(root.join(".claude")).unwrap();
        Self {
            root,
            session: SessionId::new(format!("subagent-{label}-{n}")),
        }
    }

    fn profile(&self) -> Profile {
        Profile::compile(
            &self.root,
            Some(r#"{"permissions":{"allow":["Bash(echo*)"]}}"#),
        )
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        bg::shutdown(&self.session);
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// A provider that answers every request with the same assistant message.
fn start_provider(reply: &'static str, turns: usize) -> String {
    start_provider_sequence(vec![reply; turns])
}

fn start_provider_sequence(replies: Vec<&'static str>) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
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
                if let Some(rest) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                    length = rest.trim().parse().unwrap_or(0);
                }
            }
            let mut body = vec![0u8; length];
            let _ = reader.read_exact(&mut body);
            let payload = serde_json::json!({
                "role": "assistant",
                "content": [{"type": "text", "text": reply}],
                "usage": {"input_tokens": 11, "output_tokens": 7}
            })
            .to_string();
            let head = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
                payload.len()
            );
            let _ = stream.write_all(head.as_bytes());
            let _ = stream.write_all(payload.as_bytes());
            let _ = stream.flush();
        }
    });
    format!("http://127.0.0.1:{port}")
}

fn start_native_provider_sequence(replies: Vec<serde_json::Value>) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for payload in replies {
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
                if let Some(rest) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                    length = rest.trim().parse().unwrap_or(0);
                }
            }
            let mut body = vec![0; length];
            let _ = reader.read_exact(&mut body);
            let payload = payload.to_string();
            let head = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
                payload.len()
            );
            let _ = stream.write_all(head.as_bytes());
            let _ = stream.write_all(payload.as_bytes());
        }
    });
    format!("http://127.0.0.1:{port}")
}

#[test]
fn subagent_uses_native_cell_handoff_across_turns() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let fixture = Fixture::new("native");
    let reply = |id: &str, code: &str| {
        serde_json::json!({
            "role":"assistant", "content":[{"type":"tool_use","id":id,"name":"execute_cell","input":{"code":code}}],
            "usage":{"input_tokens":11,"output_tokens":7}
        })
    };
    let base = start_native_provider_sequence(vec![
        reply("first", "const computed = 42; console.log(computed);"),
        reply("second", "answer(`native ${computed}`);"),
    ]);
    unsafe {
        std::env::set_var("ANTHROPIC_BASE_URL", &base);
    }
    let _handle = bg::agent(
        &fixture.profile(),
        &fixture.session,
        "compute",
        &AgentOptions {
            turns: Some(4),
            deadline: None,
            model: "test-model".into(),
            effort: sterna::wire::Effort::default(),
        },
    );
    let events = wait_for_event(&fixture.session, Duration::from_secs(20));
    unsafe {
        std::env::remove_var("ANTHROPIC_BASE_URL");
    }
    let done = events
        .iter()
        .find(|event| matches!(event.kind, Kind::AgentDone { .. }))
        .unwrap();
    let result = bg::payload(&fixture.session, done.payload.as_str()).unwrap();
    assert_eq!(result.status, "returned");
    assert_eq!(result.stdout, "native 42");
}

/// **No returned value finishes a subagent, not even a plain string.** The
/// string return is the shape that used to end one -- `return result.stdout`,
/// written to look at a command's output, was read as the final answer -- so
/// it is the value this guards with. The subagent goes on until it answers.
#[test]
fn a_returned_value_is_notebook_output_and_the_subagent_works_on() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let fixture = Fixture::new("structured-output");
    let reply = |id: &str, code: &str| {
        serde_json::json!({
            "role":"assistant", "content":[{"type":"tool_use","id":id,"name":"execute_cell","input":{"code":code}}],
            "usage":{"input_tokens":11,"output_tokens":7}
        })
    };
    let base = start_native_provider_sequence(vec![
        reply("inspect", "return {matchesCount: 0, sampleMatches: []};"),
        reply(
            "plain",
            "return \"a bare string, which used to end a subagent\";",
        ),
        reply(
            "answer",
            "answer(\"recommendations follow from the inspection\");",
        ),
    ]);
    unsafe {
        std::env::set_var("ANTHROPIC_BASE_URL", &base);
    }
    let _handle = bg::agent(
        &fixture.profile(),
        &fixture.session,
        "recommend changes",
        &AgentOptions {
            turns: Some(4),
            deadline: None,
            model: "test-model".into(),
            effort: sterna::wire::Effort::default(),
        },
    );
    let events = wait_for_event(&fixture.session, Duration::from_secs(20));
    unsafe {
        std::env::remove_var("ANTHROPIC_BASE_URL");
    }
    let done = events
        .iter()
        .find(|event| matches!(event.kind, Kind::AgentDone { .. }))
        .expect("a returned value must not finish the subagent");
    let result = bg::payload(&fixture.session, done.payload.as_str()).unwrap();
    assert_eq!(result.status, "returned");
    assert_eq!(
        result.stdout, "recommendations follow from the inspection",
        "the answer stands, not either returned value"
    );
}

/// Every event this session has published by the time the subagent is done,
/// or everything seen before `within` runs out.
///
/// **It waits for the `AgentDone`, not for the first event of any kind.**
/// `bg::drain` takes what is there *now*, and a subagent publishes more than
/// one event: the first drain to come back non-empty may hold only what the
/// agent said on the way, with the answer still in flight. Returning that was
/// a race every caller here lost the same way — each of them goes on to
/// `.find(AgentDone)` — and it was invisible on macOS, where the events
/// happened to land in one drain, while `sterna (ubuntu-latest)` went red on
/// the deadline test (the sweep of 2026-09-18). Draining is destructive, so
/// what is taken has to be accumulated rather than re-read.
fn wait_for_event(session: &SessionId, within: Duration) -> Vec<sterna::events::Event> {
    let deadline = Instant::now() + within;
    let mut seen: Vec<sterna::events::Event> = Vec::new();
    loop {
        seen.extend(bg::drain(session));
        if seen
            .iter()
            .any(|event| matches!(event.kind, Kind::AgentDone { .. }))
        {
            return seen;
        }
        if Instant::now() >= deadline {
            return seen;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// The whole of Phase 64's first line: a handle at once, the work out of band,
/// and the answer arriving as an event rather than as a blocking return.
#[test]
fn a_subagent_answers_in_a_later_event_and_never_blocks_the_caller() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let fixture = Fixture::new("answers");
    let base_url = start_provider("```sterna\nanswer(\"the answer is 42\");\n```", 2);
    // SAFETY: `_guard` holds `ENV_LOCK` for this whole test.
    unsafe {
        std::env::set_var("ANTHROPIC_BASE_URL", &base_url);
    }

    let started = Instant::now();
    let handle = bg::agent(
        &fixture.profile(),
        &fixture.session,
        "what is six times seven",
        &AgentOptions {
            turns: Some(4),
            deadline: None,
            model: "test-model".to_string(),
            effort: sterna::wire::Effort::default(),
        },
    );
    let elapsed = started.elapsed();
    assert!(
        elapsed < Duration::from_millis(500),
        "agent.run blocked for {elapsed:?}; it must return before the subagent has done anything"
    );
    assert!(!handle.is_empty());

    // **The override is held until the event has arrived**, and that is not
    // tidiness. `wire::base_url` is read at request time on the subagent's own
    // thread, so unsetting it when `bg::agent` returns leaves a started
    // subagent pointing at the real provider — which is what happened, with a
    // 401 from api.anthropic.com to prove it.
    let events = wait_for_event(&fixture.session, Duration::from_secs(20));
    unsafe {
        std::env::remove_var("ANTHROPIC_BASE_URL");
    }
    let done = events
        .iter()
        .find(|event| matches!(event.kind, Kind::AgentDone { .. }))
        .expect("an agent.done event should have been delivered");
    assert_eq!(
        done.kind.as_str(),
        "agent.done",
        "a subagent must not arrive as a bg.done"
    );
    assert_eq!(done.source, format!("agent/{handle}"));

    let result = bg::payload(&fixture.session, done.payload.as_str())
        .expect("the completion's payload handle resolves");
    assert_eq!(result.stdout, "the answer is 42");
    assert_eq!(result.status, "returned");
}

/// A subagent that never answers is stopped by its own turn cap, and says so
/// rather than reporting an answer it does not have.
#[test]
fn a_subagent_that_never_answers_stops_at_its_turn_hint_and_keeps_its_work() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let fixture = Fixture::new("cap");
    // A program that always yields: it binds a name and runs off the end.
    // The prose beside it is the subagent's own last words, which are what
    // the parent must receive when the hint runs out.
    let base_url = start_provider(
        "Still narrowing it down; the parser is in config.rs.\n```sterna\nconst n = 1;\n```",
        8,
    );
    // SAFETY: `_guard` holds `ENV_LOCK` for this whole test.
    unsafe {
        std::env::set_var("ANTHROPIC_BASE_URL", &base_url);
    }
    bg::agent(
        &fixture.profile(),
        &fixture.session,
        "loop forever",
        &AgentOptions {
            turns: Some(2),
            deadline: None,
            model: "test-model".to_string(),
            effort: sterna::wire::Effort::default(),
        },
    );
    let events = wait_for_event(&fixture.session, Duration::from_secs(20));
    unsafe {
        std::env::remove_var("ANTHROPIC_BASE_URL");
    }

    let done = events
        .iter()
        .find(|event| matches!(event.kind, Kind::AgentDone { .. }))
        .expect("a capped subagent still completes");
    let result = bg::payload(&fixture.session, done.payload.as_str()).expect("resolves");
    assert_eq!(result.status, "turns", "{result:?}");
    // **The work survives the stop.** Until 2026-09-17 this answered "the
    // subagent used every turn it was given without returning" and dropped
    // everything the subagent had produced.
    assert!(
        result.stdout.contains("the parser is in config.rs"),
        "a subagent that stops early returns its own last words: {result:?}"
    );
    assert!(
        !result.stdout.contains("without returning"),
        "no placeholder may stand in for the work: {result:?}"
    );
}

/// No turn cap and no turn default: a subagent given no hint works past the
/// eight turns it used to be handed and the twenty-four it could never
/// exceed. The user, 2026-09-17: *"Limits are dumb for abstract tasks … what
/// if it needed 9 or 25. all for nothing?"*
#[test]
fn a_subagent_with_no_turn_hint_works_until_it_answers() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let fixture = Fixture::new("uncapped");
    let mut replies = vec!["```sterna\nconst n = 1;\n```"; 25];
    replies.push("```sterna\nanswer('the twenty-sixth turn answered');\n```");
    let base_url = start_provider_sequence(replies);
    // SAFETY: `_guard` holds `ENV_LOCK` for this whole test.
    unsafe {
        std::env::set_var("ANTHROPIC_BASE_URL", &base_url);
    }
    bg::agent(
        &fixture.profile(),
        &fixture.session,
        "take as long as it takes",
        &AgentOptions {
            turns: None,
            deadline: None,
            model: "test-model".to_string(),
            effort: sterna::wire::Effort::default(),
        },
    );
    let events = wait_for_event(&fixture.session, Duration::from_secs(60));
    unsafe {
        std::env::remove_var("ANTHROPIC_BASE_URL");
    }

    let done = events
        .iter()
        .find(|event| matches!(event.kind, Kind::AgentDone { .. }))
        .expect("an uncapped subagent completes when it answers");
    let result = bg::payload(&fixture.session, done.payload.as_str()).expect("resolves");
    assert_eq!(result.status, "returned", "{result:?}");
    assert!(
        result.stdout.contains("twenty-sixth turn answered"),
        "{result:?}"
    );
}

/// The wall clock the person configured is what stops a subagent that never
/// answers — and it too keeps the work. `[agents] deadline_minutes` is absent
/// by default, so nothing here fires unless someone asked for it.
#[test]
fn a_configured_deadline_stops_a_subagent_and_keeps_its_work() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let fixture = Fixture::new("deadline");
    let base_url = start_provider(
        "Reading the config parser now.\n```sterna\nconst n = 1;\n```",
        200,
    );
    // SAFETY: `_guard` holds `ENV_LOCK` for this whole test.
    unsafe {
        std::env::set_var("ANTHROPIC_BASE_URL", &base_url);
    }
    bg::agent(
        &fixture.profile(),
        &fixture.session,
        "loop forever",
        &AgentOptions {
            turns: None,
            deadline: Some(Duration::from_millis(400)),
            model: "test-model".to_string(),
            effort: sterna::wire::Effort::default(),
        },
    );
    let events = wait_for_event(&fixture.session, Duration::from_secs(30));
    unsafe {
        std::env::remove_var("ANTHROPIC_BASE_URL");
    }

    let done = events
        .iter()
        .find(|event| matches!(event.kind, Kind::AgentDone { .. }))
        .expect("a subagent out of time still completes");
    let result = bg::payload(&fixture.session, done.payload.as_str()).expect("resolves");
    assert_eq!(
        result.status, "deadline",
        "time running out is its own stop, not a bare cancellation: {result:?}"
    );
    assert!(
        result.stdout.contains("Reading the config parser"),
        "{result:?}"
    );
    assert!(result.stderr.contains("ran out of time"), "{result:?}");
}

/// Measured 2026-09-17 (session `tlitep-13fv`): three subagents came back
/// `{status: "cancelled", stdout: "", stderr: ""}`, so the parent could not
/// tell an exhausted turn budget from a refusal and started the same doomed
/// subagent twice more. A subagent that stops early now says what it did.
#[test]
fn a_subagent_that_stopped_early_reports_its_turns_and_trajectory() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let fixture = Fixture::new("early");
    let base_url = start_provider("```sterna\nconst n = 1;\n```", 8);
    // SAFETY: `_guard` holds `ENV_LOCK` for this whole test.
    unsafe {
        std::env::set_var("ANTHROPIC_BASE_URL", &base_url);
    }
    bg::agent(
        &fixture.profile(),
        &fixture.session,
        "loop forever",
        &AgentOptions {
            turns: Some(2),
            deadline: None,
            model: "test-model".to_string(),
            effort: sterna::wire::Effort::default(),
        },
    );
    let events = wait_for_event(&fixture.session, Duration::from_secs(20));
    unsafe {
        std::env::remove_var("ANTHROPIC_BASE_URL");
    }

    let done = events
        .iter()
        .find(|event| matches!(event.kind, Kind::AgentDone { .. }))
        .expect("a capped subagent still completes");
    let result = bg::payload(&fixture.session, done.payload.as_str()).expect("resolves");
    assert_eq!(result.status, "turns", "{result:?}");
    assert!(
        result.stderr.contains("turn hint"),
        "the parent must be able to tell why it stopped: {result:?}"
    );
    assert!(
        result.stderr.contains("2 turns"),
        "the parent must be able to see how far it got: {result:?}"
    );
}

#[test]
fn a_subagent_can_amend_its_parse_failed_cell() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let fixture = Fixture::new("repair");
    let base = start_provider_sequence(vec![
        "```sterna\nanswer('repaired;\n```",
        "```sterna-edit\n{\"cell\":1,\"replace\":\"'repaired;\",\"with\":\"'repaired');\"}\n```",
    ]);
    // SAFETY: the environment lock is held until the child has finished.
    unsafe {
        std::env::set_var("ANTHROPIC_BASE_URL", base);
    }
    let result = sterna::agent::run(
        &fixture.profile(),
        &fixture.session,
        "answer",
        &AgentOptions {
            turns: Some(2),
            deadline: None,
            model: "test-model".into(),
            effort: sterna::wire::Effort::default(),
        },
        &sterna::tools::invoke::CancellationToken::new(),
    );
    unsafe {
        std::env::remove_var("ANTHROPIC_BASE_URL");
    }
    assert_eq!(result.status, "returned");
    assert_eq!(result.answer, "repaired");
    assert_eq!(result.turns, 2);
}

#[test]
fn subagent_plain_prose_is_its_result_without_a_marker_round_trip() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let fixture = Fixture::new("explicit-completion");
    let base = start_provider_sequence(vec![
        "I will calculate the answer next.",
        "The answer is 42.",
    ]);
    // SAFETY: serialized with the other environment-dependent tests.
    unsafe {
        std::env::set_var("ANTHROPIC_BASE_URL", base);
    }
    let result = sterna::agent::run(
        &fixture.profile(),
        &fixture.session,
        "Answer the question",
        &AgentOptions {
            turns: Some(2),
            deadline: None,
            model: "test-model".into(),
            effort: sterna::wire::Effort::default(),
        },
        &sterna::tools::invoke::CancellationToken::new(),
    );
    unsafe {
        std::env::remove_var("ANTHROPIC_BASE_URL");
    }
    assert_eq!(result.status, "returned");
    assert_eq!(result.answer, "I will calculate the answer next.");
    assert_eq!(result.turns, 1);
}

// --- A subagent is a session you can read and address ----------------------
//
// The user, 2026-09-17: *"Subagent behavior should also be like Claude code.
// In Claude code user can talk to subagent by selecting and jumping into its
// session in and out."* These four are the substance that an attach UI needs:
// a record written as the work happens, a message that reaches a running
// subagent, an honest answer for one that has already finished, and a look
// that names the way in.

/// A provider that answers slowly, so a test can watch a subagent *while* it
/// is working rather than racing its completion. Every request is recorded,
/// which is how the inbox test proves a message reached the conversation the
/// subagent actually sent.
fn start_recording_provider(
    replies: Vec<&'static str>,
    pause: Duration,
) -> (String, std::sync::Arc<Mutex<Vec<String>>>) {
    let seen = std::sync::Arc::new(Mutex::new(Vec::new()));
    let recorded = std::sync::Arc::clone(&seen);
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
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
                if let Some(rest) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                    length = rest.trim().parse().unwrap_or(0);
                }
            }
            let mut body = vec![0u8; length];
            let _ = reader.read_exact(&mut body);
            recorded
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .push(String::from_utf8_lossy(&body).into_owned());
            std::thread::sleep(pause);
            let payload = serde_json::json!({
                "role": "assistant",
                "content": [{"type": "text", "text": reply}],
                "usage": {"input_tokens": 11, "output_tokens": 7}
            })
            .to_string();
            let head = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
                payload.len()
            );
            let _ = stream.write_all(head.as_bytes());
            let _ = stream.write_all(payload.as_bytes());
            let _ = stream.flush();
        }
    });
    (format!("http://127.0.0.1:{port}"), seen)
}

/// Waits until `f` holds, or panics with `what`.
fn until(within: Duration, what: &str, mut f: impl FnMut() -> bool) {
    let deadline = Instant::now() + within;
    while Instant::now() < deadline {
        if f() {
            return;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    panic!("timed out waiting for {what}");
}

fn rollout_lines(path: &std::path::Path) -> Vec<serde_json::Value> {
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect()
}

#[test]
fn a_running_subagents_rollout_is_readable_while_it_works() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let fixture = Fixture::new("record");
    let mut replies = vec!["Reading the parser.\n```sterna\nconst n = 1;\n```"; 5];
    replies.push("```sterna\nanswer('done reading');\n```");
    let (base_url, _seen) = start_recording_provider(replies, Duration::from_millis(150));
    // SAFETY: `_guard` holds `ENV_LOCK` for this whole test.
    unsafe {
        std::env::set_var("ANTHROPIC_BASE_URL", &base_url);
    }
    let handle = bg::agent(
        &fixture.profile(),
        &fixture.session,
        "read the parser",
        &AgentOptions {
            turns: None,
            deadline: None,
            model: "test-model".to_string(),
            effort: sterna::wire::Effort::default(),
        },
    );
    let record = sterna::agent::AgentRollout::for_job(&fixture.root, &fixture.session, &handle);
    // Beside the parent's file, in a folder named for it -- and out of the
    // folder `--sessions` lists, because nothing resumes a subagent.
    assert!(
        record.path.ends_with(format!(
            "{}.agents/{handle}.jsonl",
            fixture.session.as_str()
        )),
        "{:?}",
        record.path
    );

    // **Mid-run, not after.** The subagent is still working here: the
    // provider pauses between replies and the run is not drained yet.
    until(
        Duration::from_secs(20),
        "the record to carry two turns",
        || {
            rollout_lines(&record.path)
                .iter()
                .filter(|line| line["kind"] == "turn")
                .count()
                >= 2
        },
    );
    let mid = rollout_lines(&record.path);
    assert_eq!(mid[0]["kind"], "system", "the system block comes first");
    let turns: Vec<&serde_json::Value> = mid.iter().filter(|l| l["kind"] == "turn").collect();
    assert_eq!(turns[0]["role"], "user");
    assert!(
        turns[0]["text"]
            .as_str()
            .unwrap()
            .contains("read the parser"),
        "the task the cell asked for is the first turn: {:?}",
        turns[0]
    );
    assert_eq!(turns[1]["role"], "assistant");
    assert!(
        bg::progress(&fixture.session, &handle)
            .expect("a subagent reports progress")
            .running,
        "this assertion is the point of the test: the record was readable while it ran"
    );

    let _ = wait_for_event(&fixture.session, Duration::from_secs(30));
    unsafe {
        std::env::remove_var("ANTHROPIC_BASE_URL");
    }
    let done = rollout_lines(&record.path);
    let numbers: Vec<u64> = done
        .iter()
        .filter(|line| line["kind"] == "turn")
        .map(|line| line["turn"].as_u64().unwrap())
        .collect();
    assert!(
        numbers.windows(2).all(|pair| pair[0] < pair[1]),
        "turns are recorded in order: {numbers:?}"
    );
    assert!(
        done.iter().any(|line| line["kind"] == "cell"),
        "the cells it ran are in the record too"
    );
}

#[test]
fn a_person_can_tell_a_running_subagent_something() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let fixture = Fixture::new("tell");
    let mut replies = vec!["Still reading.\n```sterna\nconst n = 1;\n```"; 6];
    replies.push("```sterna\nanswer('stopped reading');\n```");
    let (base_url, seen) = start_recording_provider(replies, Duration::from_millis(150));
    // SAFETY: `_guard` holds `ENV_LOCK` for this whole test.
    unsafe {
        std::env::set_var("ANTHROPIC_BASE_URL", &base_url);
    }
    let handle = bg::agent(
        &fixture.profile(),
        &fixture.session,
        "read everything",
        &AgentOptions {
            turns: None,
            deadline: None,
            model: "test-model".to_string(),
            effort: sterna::wire::Effort::default(),
        },
    );
    until(Duration::from_secs(20), "the subagent's first turn", || {
        bg::progress(&fixture.session, &handle).is_some_and(|look| look.turns >= 1)
    });
    assert_eq!(
        bg::tell(&fixture.session, &handle, "stop reading and summarise"),
        Ok(sterna::bg::Delivery::Queued),
    );

    let _ = wait_for_event(&fixture.session, Duration::from_secs(30));
    unsafe {
        std::env::remove_var("ANTHROPIC_BASE_URL");
    }
    let record = sterna::agent::AgentRollout::for_job(&fixture.root, &fixture.session, &handle);
    let said: Vec<String> = rollout_lines(&record.path)
        .iter()
        .filter(|line| line["kind"] == "turn" && line["role"] == "user")
        .filter_map(|line| line["text"].as_str().map(str::to_owned))
        .collect();
    assert!(
        said.iter()
            .any(|text| text.contains("stop reading and summarise")),
        "what the person said is part of the subagent's record: {said:?}"
    );
    // **It reached the conversation, not just the file.** The provider saw
    // it, which is the only proof that the subagent was actually told.
    let requests = seen.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    assert!(
        requests
            .iter()
            .any(|body| body.contains("stop reading and summarise")),
        "the message is in the request the subagent sent next"
    );
}

#[test]
fn a_message_to_a_finished_subagent_is_undelivered_rather_than_an_error() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let fixture = Fixture::new("late");
    let base_url = start_provider("```sterna\nanswer('answered at once');\n```", 1);
    // SAFETY: `_guard` holds `ENV_LOCK` for this whole test.
    unsafe {
        std::env::set_var("ANTHROPIC_BASE_URL", &base_url);
    }
    let handle = bg::agent(
        &fixture.profile(),
        &fixture.session,
        "answer",
        &AgentOptions {
            turns: None,
            deadline: None,
            model: "test-model".to_string(),
            effort: sterna::wire::Effort::default(),
        },
    );
    let _ = wait_for_event(&fixture.session, Duration::from_secs(20));
    unsafe {
        std::env::remove_var("ANTHROPIC_BASE_URL");
    }
    until(Duration::from_secs(10), "the job to be finished", || {
        bg::progress(&fixture.session, &handle).is_some_and(|look| !look.running)
    });
    assert_eq!(
        bg::tell(&fixture.session, &handle, "one more thing"),
        Ok(sterna::bg::Delivery::Undelivered),
        "racing the work and losing is an outcome, not a mistake"
    );
    assert_eq!(
        bg::tell(&fixture.session, "job404", "anyone there?"),
        Ok(sterna::bg::Delivery::Undelivered),
    );
    assert!(
        bg::tell(&fixture.session, &handle, "").is_err(),
        "an empty message is refused rather than queued"
    );
}

#[test]
fn a_look_names_the_record_and_whether_it_still_listens() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let fixture = Fixture::new("wayin");
    let (base_url, _seen) = start_recording_provider(
        vec!["```sterna\nanswer('answered');\n```"],
        Duration::from_millis(250),
    );
    // SAFETY: `_guard` holds `ENV_LOCK` for this whole test.
    unsafe {
        std::env::set_var("ANTHROPIC_BASE_URL", &base_url);
    }
    let handle = bg::agent(
        &fixture.profile(),
        &fixture.session,
        "answer",
        &AgentOptions {
            turns: None,
            deadline: None,
            model: "test-model".to_string(),
            effort: sterna::wire::Effort::default(),
        },
    );
    let running = bg::progress(&fixture.session, &handle).expect("a subagent reports progress");
    // The record hangs off the *profile's* root, which `Profile::compile`
    // canonicalises -- on macOS `/var` is a symlink to `/private/var`, so an
    // expectation built from the raw temp path names the same file by another
    // name.
    let root = std::fs::canonicalize(&fixture.root).unwrap();
    assert_eq!(
        running.rollout,
        Some(sterna::agent::AgentRollout::for_job(&root, &fixture.session, &handle).path),
        "the look names the record a person would open"
    );
    assert!(
        running.takes_messages,
        "a running subagent can be told things"
    );

    let _ = wait_for_event(&fixture.session, Duration::from_secs(20));
    unsafe {
        std::env::remove_var("ANTHROPIC_BASE_URL");
    }
    until(Duration::from_secs(10), "the job to be finished", || {
        bg::progress(&fixture.session, &handle).is_some_and(|look| !look.running)
    });
    let ended = bg::progress(&fixture.session, &handle).expect("progress outlives the work");
    assert!(
        !ended.takes_messages,
        "a finished subagent is honest that nothing would hear a message"
    );
    assert!(ended.rollout.is_some(), "its record is still there to read");
}
