//! Binary-level canaries for the decision model's hold and its completion
//! question (`docs/decisions.md`): the built `sterna` binary
//! against a loopback fake that dispatches on the request path --
//! `/v1/messages` answers scripted cells in order (the `providers` fake from
//! `tests/evidence_gate.rs`, path-aware here since one task now makes up to
//! three kinds of request), `/v1/systemone` answers by the request's own
//! question key -- `"intent"` (asked once per task, before the first turn)
//! or `"satisfied"` (asked once per task, at the completion gate; 2616,
//! extended 2641/2642) -- each with a scripted answer, a non-2xx status, a
//! sleep past the 2 s bound, or (unscripted) a harmless default. A
//! `"satisfied"` request may carry the five diff-hygiene questions (2641)
//! and one `judge_<n>` per undecided acceptance item (2642) in the same
//! request; a test that scripts only `completion_answer` (the `satisfied`
//! key) gets every other key auto-filled with a neutral 0.50 noul
//! (`fill_unscripted_answers`), so a test written before 2641/2642 keeps
//! working without listing keys it does not care about.
use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

#[path = "support/sse.rs"]
mod sse;

fn root(label: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!(
        "sterna-decisions-{label}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&root).unwrap();
    root
}

fn write_config(root: &Path, text: &str) {
    let dir = root.join(".sterna");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("config.toml"), text).unwrap();
}

/// The sentences the gate noted beside the answer without holding it.
fn notes(result: &Value) -> Vec<String> {
    result["telemetry"]["completion"]["findings"]
        .as_array()
        .map(|notes| {
            notes
                .iter()
                .map(|note| note.as_str().unwrap_or("").to_string())
                .collect()
        })
        .unwrap_or_default()
}

/// What the decision endpoint does with one scripted question.
enum Decision {
    Answer(Value),
    Status(u16),
    Sleep(Duration),
}

/// Recorded request bodies (or, for the decision endpoint's headers, its raw
/// header block), shared with the fake server's own thread.
type Recorded = Arc<Mutex<Vec<String>>>;

/// Which request this package sent to `/v1/systemone`: `"intent"` for the
/// task-start request (which also carries the `kind` question in the same
/// `questions` map, so a plain "first key" read would see `intent` only by
/// luck of the sort), `"satisfied"` for the completion question (2616), or
/// `"field_shape"` for a large returned field.
fn question_key(body_text: &str) -> String {
    let value: Value = serde_json::from_str(body_text).unwrap();
    let questions = value["questions"].as_object().cloned().unwrap_or_default();
    if questions.contains_key("satisfied") {
        "satisfied".to_string()
    } else if questions.contains_key("field_shape") {
        "field_shape".to_string()
    } else {
        "intent".to_string()
    }
}

/// A fake provider dispatching on the request's own path: `/v1/messages`
/// answers `cells` in order. `/v1/systemone` answers by the request's own
/// question key: `intent`'s scripted answers in order (or a harmless
/// `read_only 0.94` default once the queue is empty), and `satisfied`'s
/// scripted answers in order (or a harmless `noul 0.50` default once its
/// queue is empty) -- most tests script at most one of each, since this
/// package asks the intent question once and the completion question once
/// per distinct diff claimed; a test that claims two different diffs (2616's
/// re-ask fix) scripts two `satisfied` answers. Every request's body is
/// kept, and the decision endpoint's own header blocks are kept alongside
/// its bodies.
fn providers(
    cells: Vec<Value>,
    intent: Vec<Decision>,
    completion: Vec<Decision>,
) -> (String, Recorded, Recorded, Recorded) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let message_bodies = Arc::new(Mutex::new(Vec::new()));
    let decision_bodies = Arc::new(Mutex::new(Vec::new()));
    let decision_headers = Arc::new(Mutex::new(Vec::new()));
    let seen_messages = Arc::clone(&message_bodies);
    let seen_decision_bodies = Arc::clone(&decision_bodies);
    let seen_decision_headers = Arc::clone(&decision_headers);
    std::thread::spawn(move || {
        let mut cells = cells.into_iter();
        let mut intent: std::collections::VecDeque<Decision> = intent.into_iter().collect();
        let mut completion: std::collections::VecDeque<Decision> = completion.into_iter().collect();
        loop {
            let Ok((mut stream, _)) = listener.accept() else {
                return;
            };
            stream
                .set_read_timeout(Some(Duration::from_secs(20)))
                .unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut request_line = String::new();
            if reader.read_line(&mut request_line).unwrap() == 0 {
                return;
            }
            let path = request_line
                .split_whitespace()
                .nth(1)
                .unwrap_or_default()
                .to_string();
            let mut length = 0;
            let mut header_block = String::new();
            loop {
                let mut line = String::new();
                if reader.read_line(&mut line).unwrap() == 0 {
                    return;
                }
                if line == "\r\n" {
                    break;
                }
                if let Some(value) = line.to_lowercase().strip_prefix("content-length:") {
                    length = value.trim().parse().unwrap();
                }
                header_block.push_str(&line);
            }
            let mut body = vec![0; length];
            reader.read_exact(&mut body).unwrap();
            let body_text = String::from_utf8_lossy(&body).into_owned();

            if path == "/v1/systemone" {
                let key = question_key(&body_text);
                seen_decision_bodies.lock().unwrap().push(body_text.clone());
                seen_decision_headers.lock().unwrap().push(header_block);
                let decision = match key.as_str() {
                    "intent" => intent
                        .pop_front()
                        .unwrap_or_else(|| Decision::Answer(decision_answer("read_only", 0.94))),
                    "satisfied" => completion
                        .pop_front()
                        .unwrap_or_else(|| Decision::Answer(completion_answer(0.50))),
                    // The field-shape question (`session/returned.rs`) is
                    // answered `log` at 0.90 for every field: the one test
                    // that asks it returns a log.
                    "field_shape" => Decision::Answer(field_shape_answer("log", 0.90)),
                    other => panic!("unexpected decision question key `{other}`"),
                };
                match decision {
                    Decision::Answer(mut value) => {
                        if key == "satisfied" {
                            fill_unscripted_answers(&mut value, &body_text);
                        }
                        let response = value.to_string();
                        let _ = write!(
                            stream,
                            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{response}",
                            response.len()
                        );
                    }
                    Decision::Status(status) => {
                        let response = "{}";
                        let _ = write!(
                            stream,
                            "HTTP/1.1 {status} Error\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{response}",
                            response.len()
                        );
                    }
                    // Answered from its own thread, so a slow decision holds
                    // only its own caller -- never the next request.
                    Decision::Sleep(duration) => {
                        std::thread::spawn(move || {
                            std::thread::sleep(duration);
                            let response = "{}";
                            let _ = write!(
                                stream,
                                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{response}",
                                response.len()
                            );
                        });
                    }
                }
            } else {
                let request: serde_json::Value =
                    serde_json::from_str(&body_text).unwrap_or(serde_json::Value::Null);
                seen_messages.lock().unwrap().push(body_text);
                let Some(response) = cells.next() else {
                    return;
                };
                let (content_type, response) = sse::response_for(&request, &response.to_string());
                let _ = write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{response}",
                    response.len()
                );
            }
        }
    });
    (url, message_bodies, decision_bodies, decision_headers)
}

fn cell(id: &str, code: &str) -> Value {
    json!({"role": "assistant", "content": [{"type": "tool_use", "id": id, "name": "execute_cell", "input": {"code": code}}],
        "usage": {"input_tokens": 20, "output_tokens": 7}})
}

/// A native `Write` tool call -- the shape a `--interface tools` turn sends
/// instead of `execute_cell` (`abi::dialect`'s `Write` shape).
fn direct_write(id: &str, path: &str, content: &str) -> Value {
    json!({"role": "assistant", "content": [{"type": "tool_use", "id": id, "name": "Write", "input": {"file_path": path, "content": content}}],
        "usage": {"input_tokens": 20, "output_tokens": 7}})
}

/// A native `Read` tool call -- pure, never held.
fn direct_read(id: &str, path: &str) -> Value {
    json!({"role": "assistant", "content": [{"type": "tool_use", "id": id, "name": "Read", "input": {"file_path": path}}],
        "usage": {"input_tokens": 20, "output_tokens": 7}})
}

/// A plain-text completion. A direct tool call never ends the task itself --
/// unlike `return` inside a cell -- so a direct-frame scenario needs one of
/// these to finish.
fn prose(text: &str) -> Value {
    json!({"role": "assistant", "content": [{"type": "text", "text": text}],
        "usage": {"input_tokens": 20, "output_tokens": 7}})
}

/// The intent answer, with an inert `kind` answer (under `KIND_ABOVE`) so
/// every hold/override test -- which scripts only the intent choice it
/// cares about -- answers every question the request asked.
fn decision_answer(intent_choice: &str, intent_confidence: f64) -> Value {
    json!({
        "model": "jev-latest",
        "answers": {
            "intent": {
                "type": "choice",
                "choice": intent_choice,
                "probabilities": {"read_only": intent_confidence, "modify": 0.0, "run": 0.0, "other": 0.0},
                "confidence": intent_confidence,
            },
            // The kind question (2026-09-23) rides the same request;
            // `decision_answer_with_kind` overrides it.
            "kind": {
                "type": "choice",
                "choice": "fix",
                "probabilities": {"explore": 0.0, "fix": 0.5, "implement": 0.0, "question": 0.0, "run": 0.0},
                "confidence": 0.5,
            }
        },
        "usage": {"input_tokens": 40, "output_tokens": 12},
    })
}

/// The field-shape question's answer: one choice over the five shapes.
fn field_shape_answer(choice: &str, confidence: f64) -> Value {
    json!({
        "model": "jev-latest",
        "answers": {
            "field_shape": {
                "type": "choice",
                "choice": choice,
                "probabilities": {"log": confidence, "listing": 0.0, "source": 0.0, "prose": 0.0, "data": 0.0},
                "confidence": confidence,
            }
        },
        "usage": {"input_tokens": 40, "output_tokens": 12},
    })
}

/// Both task-start answers, for a test that scripts the kind question
/// (2026-09-23).
fn decision_answer_with_kind(
    intent_choice: &str,
    intent_confidence: f64,
    kind_choice: &str,
    kind_confidence: f64,
) -> Value {
    let mut value = decision_answer(intent_choice, intent_confidence);
    value["answers"]["kind"] = json!({
        "type": "choice",
        "choice": kind_choice,
        "probabilities": {"explore": 0.0, "fix": 0.0, "implement": 0.0, "question": 0.0, "run": 0.0},
        "confidence": kind_confidence,
    });
    value
}

fn completion_answer(noul: f64) -> Value {
    json!({
        "model": "jev-latest",
        "answers": {
            "satisfied": {
                "type": "noul",
                "noul": noul,
            }
        },
        "usage": {"input_tokens": 40, "output_tokens": 12},
    })
}

/// Fills a neutral 0.50 noul answer for every question the request asked
/// that the scripted response did not name -- so a test scripting only
/// `satisfied` still answers whatever hygiene (2641) or judge (2642)
/// questions the same request added, without needing to enumerate them. A
/// noul of 0.50 sits strictly between every `*_no_below` and `*_yes_above`
/// default, so an unscripted key is always undecided: no finding, no
/// acceptance item settled.
fn fill_unscripted_answers(value: &mut Value, body_text: &str) {
    let body: Value = serde_json::from_str(body_text).unwrap();
    let Some(questions) = body["questions"].as_object() else {
        return;
    };
    let answers = value["answers"]
        .as_object_mut()
        .expect("a scripted answer body carries an `answers` object");
    for key in questions.keys() {
        answers
            .entry(key.clone())
            .or_insert_with(|| json!({"type": "noul", "noul": 0.5}));
    }
}

/// A completion answer that also scripts the five hygiene nouls (2641), for
/// a test asserting on a specific hygiene question.
fn completion_answer_with_hygiene(noul: f64, hygiene: [f64; 5]) -> Value {
    let keys = [
        "has_tests",
        "out_of_scope",
        "debug_leftovers",
        "deletes_tests",
        "changes_signature",
    ];
    let mut answers = serde_json::Map::new();
    answers.insert(
        "satisfied".to_string(),
        json!({"type": "noul", "noul": noul}),
    );
    for (key, value) in keys.iter().zip(hygiene.iter()) {
        answers.insert((*key).to_string(), json!({"type": "noul", "noul": value}));
    }
    json!({
        "model": "jev-latest",
        "answers": Value::Object(answers),
        "usage": {"input_tokens": 40, "output_tokens": 12},
    })
}

const DECISIONS_ON: &str =
    "[decisions]\nmodel = \"jev-latest\"\nmode = \"on\"\nhold_above = 0.85\n";
const DECISIONS_SHADOW: &str =
    "[decisions]\nmodel = \"jev-latest\"\nmode = \"shadow\"\nhold_above = 0.85\n";
/// A task cell that changes a file before it answers: the checker reads
/// work, and a turn that changed nothing is never checked.
const WRITES_THEN_ANSWERS: &str =
    "await write({path:'fix.txt',content:'fixed'});\nanswer(\"done\");";

fn write_checks_toml(root: &Path, text: &str) {
    let dir = root.join(".sterna");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("checks.toml"), text).unwrap();
}

/// Runs `sterna exec` against `endpoint`, killing it and answering `None` if it
/// has not exited within `timeout` -- the once rule's own mutation (dropping
/// `effect_holds == 0`) makes a held cell hold forever, since a held cell is
/// charged no cell of the budget; this is what turns that hang into a fast,
/// clean test failure instead of blocking the suite.
fn exec_bounded(root: &Path, endpoint: &str, task: &str, interface: Option<&str>) -> Option<Value> {
    let out_path = root.join("stdout.json");
    let stdout_file = std::fs::File::create(&out_path).unwrap();
    let mut args = vec![
        "exec".to_string(),
        task.to_string(),
        "--output-format".to_string(),
        "json".to_string(),
        "--root".to_string(),
        root.display().to_string(),
        "--model".to_string(),
        "test/model".to_string(),
    ];
    if let Some(interface) = interface {
        args.push("--interface".to_string());
        args.push(interface.to_string());
    }
    let mut child = Command::new(env!("CARGO_BIN_EXE_sterna"))
        .args(&args)
        .env("ANTHROPIC_BASE_URL", endpoint)
        .env("ANTHROPIC_API_KEY", "test-only")
        .env("XDG_CONFIG_HOME", root.join("global-config"))
        .env_remove("ANTHROPIC_AUTH_TOKEN")
        .stdout(stdout_file)
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let started = Instant::now();
    loop {
        if child.try_wait().unwrap().is_some() {
            let bytes = std::fs::read(&out_path).unwrap();
            return serde_json::from_slice(&bytes).ok();
        }
        if started.elapsed() > Duration::from_secs(10) {
            let _ = child.kill();
            let _ = child.wait();
            return None;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn a_read_only_request_holds_the_first_effectful_cell_once_then_lets_it_run() {
    let root = root("hold-once");
    write_config(&root, DECISIONS_ON);
    let held = "await write({path: \"a.txt\", content: \"1\"});\nanswer(\"done\");";
    let (endpoint, messages, decisions, _headers) = providers(
        vec![cell("c1", held), cell("c2", held)],
        vec![Decision::Answer(decision_answer("read_only", 0.94))],
        vec![],
    );
    let result = exec_bounded(&root, &endpoint, "read the file for me", None)
        .expect("the once rule keeps the task moving");
    let messages = messages.lock().unwrap();
    assert_eq!(messages.len(), 2, "held, then the re-issue that runs");
    assert!(
        messages[1].contains("## Held (decision)"),
        "the held block reaches the model's next turn: {}",
        messages[1]
    );
    assert_eq!(
        decisions.lock().unwrap().len(),
        2,
        "the intent question once, and the completion question once when the task finishes"
    );
    assert!(root.join("a.txt").exists(), "the re-issued cell ran");
    assert_eq!(result["answer"], "done");
    let telemetry = &result["telemetry"]["decisions"];
    assert_eq!(telemetry["holds"], 1, "{telemetry}");
    assert_eq!(telemetry["overrides"], 1, "{telemetry}");
    assert_eq!(telemetry["intent"]["choice"], "read_only", "{telemetry}");
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn a_confidence_below_hold_above_never_holds() {
    let root = root("below-threshold");
    write_config(&root, DECISIONS_ON);
    let (endpoint, messages, _decisions, _headers) = providers(
        vec![cell(
            "c1",
            "await write({path: \"a.txt\", content: \"1\"});\nanswer(\"done\");",
        )],
        vec![Decision::Answer(decision_answer("read_only", 0.80))],
        vec![],
    );
    let result =
        exec_bounded(&root, &endpoint, "read the file for me", None).expect("no hold, no hang");
    assert_eq!(messages.lock().unwrap().len(), 1, "nothing is ever held");
    assert!(root.join("a.txt").exists());
    let telemetry = &result["telemetry"]["decisions"];
    assert_eq!(telemetry["would_hold"], 0, "{telemetry}");
    assert_eq!(telemetry["holds"], 0, "{telemetry}");
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn shadow_records_the_would_be_hold_and_writes_the_file() {
    let root = root("shadow");
    write_config(&root, DECISIONS_SHADOW);
    let (endpoint, messages, _decisions, _headers) = providers(
        vec![cell(
            "c1",
            "await write({path: \"a.txt\", content: \"1\"});\nanswer(\"done\");",
        )],
        vec![Decision::Answer(decision_answer("read_only", 0.94))],
        vec![],
    );
    let result =
        exec_bounded(&root, &endpoint, "read the file for me", None).expect("shadow never holds");
    let messages = messages.lock().unwrap();
    assert_eq!(messages.len(), 1, "shadow runs the cell as today");
    assert!(
        !messages[0].contains("## Held (decision)"),
        "shadow never reaches the model: {}",
        messages[0]
    );
    assert!(root.join("a.txt").exists(), "shadow still writes the file");
    let telemetry = &result["telemetry"]["decisions"];
    assert_eq!(telemetry["would_hold"], 1, "{telemetry}");
    assert_eq!(telemetry["holds"], 0, "{telemetry}");
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn a_modify_intent_or_a_pure_cell_is_never_held() {
    let modify = root("modify-intent");
    write_config(&modify, DECISIONS_ON);
    let (endpoint, messages, _decisions, _headers) = providers(
        vec![cell(
            "c1",
            "await write({path: \"a.txt\", content: \"1\"});\nanswer(\"done\");",
        )],
        vec![Decision::Answer(decision_answer("modify", 0.99))],
        vec![],
    );
    exec_bounded(&modify, &endpoint, "edit the file for me", None).expect("modify never holds");
    assert_eq!(messages.lock().unwrap().len(), 1);
    assert!(modify.join("a.txt").exists());
    let _ = std::fs::remove_dir_all(modify);

    let pure = root("pure-cell");
    write_config(&pure, DECISIONS_ON);
    std::fs::write(pure.join("notes.txt"), "hi\n").unwrap();
    let (endpoint, messages, _decisions, _headers) = providers(
        vec![cell(
            "c1",
            "const seen = await read({path: \"notes.txt\"});\nanswer(seen.text);",
        )],
        vec![Decision::Answer(decision_answer("read_only", 0.94))],
        vec![],
    );
    let result = exec_bounded(&pure, &endpoint, "read notes.txt for me", None)
        .expect("a pure cell never holds");
    assert_eq!(messages.lock().unwrap().len(), 1);
    assert_eq!(result["answer"], "hi\n");
    let _ = std::fs::remove_dir_all(pure);
}

#[test]
fn a_direct_tool_frame_is_held_by_the_same_rule() {
    let root = root("direct-frame");
    write_config(&root, DECISIONS_ON);
    let target = root.join("a.txt");
    let (endpoint, messages, _decisions, _headers) = providers(
        vec![
            direct_write("t1", target.to_str().unwrap(), "1"),
            direct_write("t2", target.to_str().unwrap(), "1"),
            prose("Done: a.txt is written."),
        ],
        vec![Decision::Answer(decision_answer("read_only", 0.94))],
        vec![],
    );
    let result = exec_bounded(&root, &endpoint, "write a.txt for me", Some("tools"))
        .expect("the once rule keeps a direct frame moving too");
    let messages = messages.lock().unwrap();
    assert_eq!(
        messages.len(),
        3,
        "held, the re-issued call that runs, then the finishing prose"
    );
    assert!(
        messages[1].contains("## Held (decision)"),
        "the tool_result carries the held block: {}",
        messages[1]
    );
    assert!(
        messages[2].contains("wrote 1 bytes"),
        "the re-issued call actually ran, not held again: {}",
        messages[2]
    );
    assert!(target.exists(), "the re-issued Write call ran");
    assert_eq!(result["answer"], "Done: a.txt is written.");
    let telemetry = &result["telemetry"]["decisions"];
    assert_eq!(telemetry["holds"], 1, "{telemetry}");
    let _ = std::fs::remove_dir_all(&root);

    let read_root = root.with_file_name(format!(
        "{}-read",
        root.file_name().unwrap().to_string_lossy()
    ));
    std::fs::create_dir_all(&read_root).unwrap();
    write_config(&read_root, DECISIONS_ON);
    std::fs::write(read_root.join("notes.txt"), "hi\n").unwrap();
    let (endpoint, messages, _decisions, _headers) = providers(
        vec![
            direct_read("t1", read_root.join("notes.txt").to_str().unwrap()),
            prose("Done: notes.txt says hi."),
        ],
        vec![Decision::Answer(decision_answer("read_only", 0.94))],
        vec![],
    );
    let result = exec_bounded(
        &read_root,
        &endpoint,
        "read notes.txt for me",
        Some("tools"),
    )
    .expect("a Read frame is never held");
    let messages = messages.lock().unwrap();
    assert_eq!(messages.len(), 2, "a Read frame is never held");
    assert!(
        !messages[1].contains("## Held (decision)"),
        "{}",
        messages[1]
    );
    assert_eq!(result["answer"], "Done: notes.txt says hi.");
    let _ = std::fs::remove_dir_all(read_root);
}

#[test]
fn a_failed_or_slow_decision_leaves_the_task_as_it_is() {
    let failed = root("decision-500");
    write_config(&failed, DECISIONS_ON);
    let (endpoint, messages, _decisions, _headers) = providers(
        vec![cell(
            "c1",
            "await write({path: \"a.txt\", content: \"1\"});\nanswer(\"done\");",
        )],
        vec![Decision::Status(500)],
        vec![],
    );
    let result = exec_bounded(&failed, &endpoint, "read the file for me", None)
        .expect("a failed decision leaves the task alone");
    assert_eq!(
        messages.lock().unwrap().len(),
        1,
        "no hold on a failed decision"
    );
    assert!(failed.join("a.txt").exists());
    let telemetry = &result["telemetry"]["decisions"];
    assert_eq!(telemetry["failed"], 1, "{telemetry}");
    assert!(telemetry["intent"].is_null(), "{telemetry}");
    let _ = std::fs::remove_dir_all(failed);

    let slow = root("decision-slow");
    write_config(&slow, DECISIONS_ON);
    let (endpoint, messages, _decisions, _headers) = providers(
        vec![cell(
            "c1",
            "await write({path: \"a.txt\", content: \"1\"});\nanswer(\"done\");",
        )],
        vec![Decision::Sleep(Duration::from_secs(3))],
        vec![],
    );
    let result = exec_bounded(&slow, &endpoint, "read the file for me", None)
        .expect("a slow decision times out rather than hanging the task");
    assert_eq!(messages.lock().unwrap().len(), 1);
    assert!(slow.join("a.txt").exists());
    assert_eq!(result["telemetry"]["decisions"]["failed"], 1);
    let _ = std::fs::remove_dir_all(slow);
}

#[test]
fn a_shadow_task_never_waits_for_its_decision_before_the_first_turn() {
    // Shadow records the answer and acts on none of it, so a slow Jev must
    // not hold the first turn (the user, 2026-09-23: "just wait, but not --
    // and measure it"). The cell stamps when it ran; the decision takes 3 s.
    let root = root("decision-shadow-late");
    write_config(&root, DECISIONS_SHADOW);
    let (endpoint, messages, _decisions, _headers) = providers(
        vec![cell(
            "c1",
            "await write({path: \"ran.txt\", content: String(Date.now())});\nanswer(\"done\");",
        )],
        vec![Decision::Sleep(Duration::from_secs(3))],
        vec![],
    );
    let started = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis();
    let result = exec_bounded(&root, &endpoint, "read the file for me", None)
        .expect("a slow shadow decision never holds the task");
    assert_eq!(messages.lock().unwrap().len(), 1);
    let ran: u128 = std::fs::read_to_string(root.join("ran.txt"))
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    assert!(
        ran - started < 2_500,
        "the first turn waited {} ms for a decision shadow never acts on",
        ran - started
    );
    // Measured, not dropped: the answer was still waited for at the end.
    assert_eq!(result["telemetry"]["decisions"]["failed"], 1, "{result}");
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn no_model_means_no_request_and_no_thread() {
    let root = root("no-model");
    let (endpoint, messages, decisions, _headers) = providers(
        vec![cell(
            "c1",
            "await write({path: \"a.txt\", content: \"1\"});\nanswer(\"done\");",
        )],
        vec![],
        vec![],
    );
    let result =
        exec_bounded(&root, &endpoint, "read the file for me", None).expect("nothing to hang on");
    assert_eq!(messages.lock().unwrap().len(), 1);
    assert_eq!(
        decisions.lock().unwrap().len(),
        0,
        "no request ever reaches /v1/systemone"
    );
    assert!(result["telemetry"]["decisions"].is_null(), "{result}");
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn the_decision_request_carries_purpose_model_and_the_intent_question() {
    let root = root("wire-shape");
    write_config(&root, DECISIONS_ON);
    let (endpoint, _messages, decisions, headers) = providers(
        vec![cell(
            "c1",
            "await write({path: \"a.txt\", content: \"1\"});\nanswer(\"done\");",
        )],
        vec![Decision::Answer(decision_answer("read_only", 0.94))],
        vec![],
    );
    exec_bounded(&root, &endpoint, "read the file for me", None).expect("nothing to hang on");
    let headers = headers.lock().unwrap();
    assert_eq!(
        headers.len(),
        1,
        "the effectful cell is held, so the completion question is never reached"
    );
    let header_text = headers[0].to_lowercase();
    assert!(
        header_text.contains("x-glasshouse-purpose: decision"),
        "{header_text}"
    );
    assert!(
        header_text.contains("x-glasshouse-model: jev-latest"),
        "{header_text}"
    );
    let bodies = decisions.lock().unwrap();
    let body: Value = serde_json::from_str(&bodies[0]).unwrap();
    assert_eq!(body["model"], "jev-latest");
    assert_eq!(body["questions"]["intent"]["type"], "choice");
    assert!(body["questions"]["intent"]["criteria"]["read_only"].is_string());
    assert_eq!(
        body["questions"].as_object().unwrap().len(),
        2,
        "one request, both questions: {body}"
    );
    assert_eq!(body["questions"]["kind"]["type"], "choice");
    assert!(
        body["questions"]["kind"]["criteria"]["explore"].is_string(),
        "{body}"
    );
    let _ = std::fs::remove_dir_all(root);
}

// -- the completion question (2616) --------------------------------------

/// A confident no is the decision model's reading, not a fact: it is a note
/// beside the answer and never holds it (2026-09-23, *lanes, not gates*).
#[test]
fn a_confident_no_is_a_note_beside_the_answer_and_never_holds() {
    let root = root("completion-no");
    write_config(&root, DECISIONS_ON);
    let (endpoint, messages, decisions, _headers) = providers(
        vec![cell("c1", "answer(\"done\");")],
        vec![],
        vec![Decision::Answer(completion_answer(0.06))],
    );
    let result =
        exec_bounded(&root, &endpoint, "fix the bug", None).expect("a noted completion finishes");
    let messages = messages.lock().unwrap();
    assert_eq!(messages.len(), 1, "no held turn");
    assert!(
        // The diff is empty (this task never wrote a file), so the
        // answer-state addendum to 2616 is what asked the question.
        notes(&result).iter().any(
            |n| n.contains("the decision model reads the answer as not satisfying the request")
        ),
        "{result}"
    );
    assert_eq!(result["telemetry"]["completion"]["deferred"], 0);
    let telemetry = &result["telemetry"]["decisions"]["completion"];
    assert_eq!(telemetry["noul"], 0.06, "{telemetry}");
    assert_eq!(telemetry["finding_added"], true, "{telemetry}");
    assert_eq!(telemetry["state"], "answer", "{telemetry}");
    assert_eq!(
        decisions.lock().unwrap().len(),
        2,
        "the intent question once, and the completion question once for the unchanged diff -- not twice"
    );
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn a_changed_diff_is_asked_again_and_a_fixed_task_verifies() {
    let root = root("completion-changed-diff");
    write_config(&root, DECISIONS_ON);
    // A fact holds the first claim (the contract's `b.txt` is missing), so
    // the task claims twice over two different diffs; the decision model's
    // own no is only a note and would hold nothing.
    write_checks_toml(&root, "[contract]\nrequired = [\"b.txt\"]\n");
    let (endpoint, messages, decisions, _headers) = providers(
        vec![
            cell(
                "c1",
                "await write({path: \"a.txt\", content: \"1\"});\nanswer(\"done\");",
            ),
            cell(
                "c2",
                "await write({path: \"b.txt\", content: \"1\"});\nanswer(\"done\");",
            ),
        ],
        vec![Decision::Answer(decision_answer("modify", 0.99))],
        vec![
            Decision::Answer(completion_answer(0.05)),
            Decision::Answer(completion_answer(0.95)),
        ],
    );
    let result = exec_bounded(&root, &endpoint, "fix the bug", None)
        .expect("a fixed task verifies once the diff changes");
    let messages = messages.lock().unwrap();
    assert_eq!(
        messages.len(),
        2,
        "held on the first diff, then a second cell whose diff has changed"
    );
    assert!(messages[1].contains("b.txt"), "{}", messages[1]);
    assert!(
        !messages[1].contains("the decision model reads the diff as not satisfying the request"),
        "the model's reading is never what holds: {}",
        messages[1]
    );
    // The fixed diff stands: no second hold. It is not *verified* -- no
    // check ever ran, and that note rides beside the answer.
    assert_eq!(result["telemetry"]["completion"]["deferred"], 1, "{result}");
    assert_eq!(
        result["telemetry"]["completion"]["verified"], false,
        "{result}"
    );
    assert!(
        notes(&result)
            .iter()
            .any(|note| note.contains("Run a verification")),
        "{result}"
    );
    let telemetry = &result["telemetry"]["decisions"]["completion"];
    assert_eq!(telemetry["noul"], 0.95, "{telemetry}");
    assert_eq!(telemetry["finding_added"], false, "{telemetry}");
    let bodies = decisions.lock().unwrap();
    assert_eq!(
        bodies.len(),
        3,
        "the intent question once, and the completion question twice -- once per distinct diff"
    );
    let satisfied: Vec<Value> = bodies
        .iter()
        .filter(|body| body.contains("\"satisfied\""))
        .map(|body| serde_json::from_str(body).unwrap())
        .collect();
    assert_eq!(satisfied.len(), 2, "{bodies:?}");
    assert_ne!(
        satisfied[0]["state"]["diff"], satisfied[1]["state"]["diff"],
        "the second question is asked about the changed diff, not the cached one: {satisfied:?}"
    );
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn a_yes_never_removes_a_mechanical_finding() {
    let root = root("completion-yes-with-finding");
    write_config(&root, DECISIONS_ON);
    write_checks_toml(&root, "[contract]\nrequired = [\"missing.txt\"]\n");
    let (endpoint, messages, _decisions, _headers) = providers(
        vec![
            cell("c1", WRITES_THEN_ANSWERS),
            cell("c2", "answer(\"done\");"),
        ],
        vec![Decision::Answer(decision_answer("modify", 0.94))],
        vec![Decision::Answer(completion_answer(0.94))],
    );
    let result = exec_bounded(&root, &endpoint, "fix the bug", None)
        .expect("a confident yes does not remove the required-path finding");
    assert_eq!(
        messages.lock().unwrap().len(),
        2,
        "held once, then the same claim finishes unverified"
    );
    let telemetry = &result["telemetry"]["decisions"]["completion"];
    assert_eq!(telemetry["noul"], 0.94, "{telemetry}");
    assert_eq!(telemetry["finding_added"], false, "{telemetry}");
    let completion = &result["telemetry"]["completion"];
    assert_eq!(completion["verified"], false, "{completion}");
    assert!(
        completion["findings"][0]
            .as_str()
            .unwrap()
            .contains("missing.txt"),
        "{completion}"
    );
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn shadow_records_the_completion_answer_and_changes_nothing() {
    let root = root("completion-shadow");
    write_config(&root, DECISIONS_SHADOW);
    let (endpoint, messages, _decisions, _headers) = providers(
        vec![cell("c1", "answer(\"done\");")],
        vec![],
        vec![Decision::Answer(completion_answer(0.06))],
    );
    let result = exec_bounded(&root, &endpoint, "fix the bug", None)
        .expect("shadow never holds the completion");
    assert_eq!(
        messages.lock().unwrap().len(),
        1,
        "shadow finishes on the first claim"
    );
    assert_eq!(result["telemetry"]["completion"]["verified"], true);
    let telemetry = &result["telemetry"]["decisions"]["completion"];
    assert_eq!(telemetry["noul"], 0.06, "{telemetry}");
    assert_eq!(telemetry["finding_added"], false, "{telemetry}");
    assert!(telemetry["checker_skipped"].is_null(), "{telemetry}");
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn a_failed_or_slow_completion_decision_leaves_the_gate_as_it_is() {
    let failed = root("completion-500");
    write_config(&failed, DECISIONS_ON);
    let (endpoint, messages, _decisions, _headers) = providers(
        vec![cell("c1", "answer(\"done\");")],
        vec![],
        vec![Decision::Status(500)],
    );
    let result = exec_bounded(&failed, &endpoint, "fix the bug", None)
        .expect("a failed completion decision leaves the gate alone");
    assert_eq!(messages.lock().unwrap().len(), 1);
    assert_eq!(result["telemetry"]["completion"]["verified"], true);
    let telemetry = &result["telemetry"]["decisions"];
    assert_eq!(telemetry["failed"], 1, "{telemetry}");
    assert!(telemetry["completion"].is_null(), "{telemetry}");
    let _ = std::fs::remove_dir_all(failed);

    let slow = root("completion-slow");
    write_config(&slow, DECISIONS_ON);
    let (endpoint, messages, _decisions, _headers) = providers(
        vec![cell("c1", "answer(\"done\");")],
        vec![],
        vec![Decision::Sleep(Duration::from_secs(3))],
    );
    let result = exec_bounded(&slow, &endpoint, "fix the bug", None)
        .expect("a slow completion decision times out rather than hanging the task");
    assert_eq!(messages.lock().unwrap().len(), 1);
    assert_eq!(result["telemetry"]["completion"]["verified"], true);
    assert_eq!(result["telemetry"]["decisions"]["failed"], 1);
    let _ = std::fs::remove_dir_all(slow);
}

#[test]
fn a_large_diff_is_cut_at_a_hunk_boundary_and_still_asked() {
    let root = root("completion-large-diff");
    write_config(&root, DECISIONS_SHADOW);
    let mut code = String::new();
    for i in 0..40 {
        code.push_str(&format!(
            "await write({{path: \"f{i}.txt\", content: \"{}\"}});\n",
            "x".repeat(2_000)
        ));
    }
    code.push_str("answer(\"done\");");
    let (endpoint, messages, decisions, _headers) = providers(
        vec![cell("c1", &code)],
        // A non-read-only intent, so the non-empty diff this cell produces
        // is what the completion question is asked about, not the answer
        // state a read-only intent (the fake's unscripted default) would
        // force (2641/2642's addendum to 2616).
        vec![Decision::Answer(decision_answer("modify", 0.99))],
        vec![Decision::Answer(completion_answer(0.50))],
    );
    let result = exec_bounded(&root, &endpoint, "write many files", None)
        .expect("a large diff still gets a completion question");
    assert_eq!(messages.lock().unwrap().len(), 1, "shadow never holds");
    let bodies = decisions.lock().unwrap();
    assert_eq!(
        bodies.len(),
        2,
        "the intent question, then the completion question"
    );
    let satisfied: Value = serde_json::from_str(&bodies[1]).unwrap();
    assert_eq!(satisfied["state"]["diff_truncated"], true, "{satisfied}");
    let diff = satisfied["state"]["diff"].as_str().unwrap();
    assert!(
        diff.len() <= sterna::decide::DIFF_STATE_BYTES,
        "bounded to {}: got {}",
        sterna::decide::DIFF_STATE_BYTES,
        diff.len()
    );
    let telemetry = &result["telemetry"]["decisions"]["completion"];
    assert_eq!(telemetry["truncated"], true, "{telemetry}");
    let _ = std::fs::remove_dir_all(root);
}

// -- diff hygiene (2641) --------------------------------------------------

#[test]
fn a_confident_out_of_scope_yes_is_a_note_with_the_reason_and_never_holds() {
    let root = root("hygiene-out-of-scope");
    write_config(&root, DECISIONS_ON);
    let held = "await write({path: \"a.txt\", content: \"1\"});\nanswer(\"done\");";
    let (endpoint, messages, decisions, _headers) = providers(
        vec![cell("c1", held)],
        vec![Decision::Answer(decision_answer("modify", 0.99))],
        vec![Decision::Answer(completion_answer_with_hygiene(
            0.94,
            [0.5, 0.95, 0.5, 0.5, 0.5],
        ))],
    );
    let result =
        exec_bounded(&root, &endpoint, "fix the bug", None).expect("a hygiene note never holds");
    let messages = messages.lock().unwrap();
    assert_eq!(messages.len(), 1, "no held turn");
    assert!(
        notes(&result)
            .iter()
            .any(|n| n.contains("the diff changes files the request did not ask about")),
        "{result}"
    );
    let telemetry = &result["telemetry"]["decisions"]["completion"];
    assert_eq!(telemetry["hygiene_findings"], 1, "{telemetry}");
    assert_eq!(telemetry["hygiene"]["out_of_scope"], 0.95, "{telemetry}");
    assert_eq!(telemetry["state"], "diff", "{telemetry}");
    assert_eq!(
        decisions.lock().unwrap().len(),
        2,
        "the intent question once, and the completion question once for the unchanged diff -- not twice"
    );
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn an_undecided_has_tests_answer_adds_no_finding() {
    let root = root("hygiene-undecided");
    write_config(&root, DECISIONS_ON);
    let (endpoint, messages, _decisions, _headers) = providers(
        vec![cell(
            "c1",
            "await write({path: \"a.txt\", content: \"1\"});\nanswer(\"done\");",
        )],
        vec![Decision::Answer(decision_answer("modify", 0.99))],
        vec![Decision::Answer(completion_answer_with_hygiene(
            0.94,
            [0.5, 0.5, 0.5, 0.5, 0.5],
        ))],
    );
    let result = exec_bounded(&root, &endpoint, "fix the bug", None).expect("no finding, no hold");
    assert_eq!(
        messages.lock().unwrap().len(),
        1,
        "an undecided hygiene answer never holds"
    );
    assert_eq!(result["telemetry"]["completion"]["verified"], true);
    let telemetry = &result["telemetry"]["decisions"]["completion"];
    assert_eq!(telemetry["hygiene_findings"], 0, "{telemetry}");
    assert_eq!(telemetry["hygiene"]["has_tests"], 0.5, "{telemetry}");
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn shadow_records_the_five_hygiene_nouls_and_adds_no_finding() {
    let root = root("hygiene-shadow");
    write_config(&root, DECISIONS_SHADOW);
    let (endpoint, messages, _decisions, _headers) = providers(
        vec![cell(
            "c1",
            "await write({path: \"a.txt\", content: \"1\"});\nanswer(\"done\");",
        )],
        vec![Decision::Answer(decision_answer("modify", 0.99))],
        vec![Decision::Answer(completion_answer_with_hygiene(
            0.06,
            [0.05, 0.95, 0.95, 0.95, 0.95],
        ))],
    );
    let result = exec_bounded(&root, &endpoint, "fix the bug", None).expect("shadow never holds");
    assert_eq!(
        messages.lock().unwrap().len(),
        1,
        "shadow finishes on the first claim"
    );
    assert_eq!(result["telemetry"]["completion"]["verified"], true);
    let telemetry = &result["telemetry"]["decisions"]["completion"];
    assert_eq!(telemetry["hygiene_findings"], 0, "{telemetry}");
    assert_eq!(telemetry["hygiene"]["has_tests"], 0.05, "{telemetry}");
    assert_eq!(telemetry["hygiene"]["out_of_scope"], 0.95, "{telemetry}");
    let _ = std::fs::remove_dir_all(root);
}

// -- the empty-diff / read-only answer state (2641's addendum to 2616) ----

#[test]
fn an_answer_only_task_with_an_empty_diff_is_asked_over_the_answer_state() {
    let root = root("answer-state-verified");
    write_config(&root, DECISIONS_ON);
    let (endpoint, messages, decisions, _headers) = providers(
        vec![cell("c1", "answer(\"done\");")],
        vec![],
        vec![Decision::Answer(completion_answer(0.94))],
    );
    let result = exec_bounded(&root, &endpoint, "read the file for me", None)
        .expect("an answer-state completion verifies");
    assert_eq!(
        messages.lock().unwrap().len(),
        1,
        "a confident yes never holds"
    );
    assert_eq!(result["telemetry"]["completion"]["verified"], true);
    let bodies = decisions.lock().unwrap();
    let satisfied: Value = serde_json::from_str(&bodies[1]).unwrap();
    assert_eq!(satisfied["state"]["answer"], "done", "{satisfied}");
    assert!(
        satisfied["state"].get("diff").is_none(),
        "the answer state carries no diff key: {satisfied}"
    );
    let telemetry = &result["telemetry"]["decisions"]["completion"];
    assert_eq!(telemetry["state"], "answer", "{telemetry}");
    assert!(telemetry["hygiene"].is_null(), "{telemetry}");
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn a_confident_no_over_the_answer_state_is_a_note_and_never_holds() {
    let root = root("answer-state-no");
    write_config(&root, DECISIONS_ON);
    let (endpoint, messages, _decisions, _headers) = providers(
        vec![cell("c1", "answer(\"done\");")],
        vec![],
        vec![Decision::Answer(completion_answer(0.05))],
    );
    let result = exec_bounded(&root, &endpoint, "read the file for me", None)
        .expect("a confident no over the answer state is a note");
    let messages = messages.lock().unwrap();
    assert_eq!(messages.len(), 1, "no held turn");
    assert!(
        notes(&result).iter().any(
            |n| n.contains("the decision model reads the answer as not satisfying the request")
        ),
        "{result}"
    );
    assert_eq!(result["telemetry"]["completion"]["deferred"], 0);
    let telemetry = &result["telemetry"]["decisions"]["completion"];
    assert_eq!(telemetry["state"], "answer", "{telemetry}");
    assert_eq!(telemetry["finding_added"], true, "{telemetry}");
    let _ = std::fs::remove_dir_all(root);
}

// -- judge items (2642) ----------------------------------------------------

/// A large returned field the decision model reads as a log is shortened by
/// the rules before the model sees it (`session/returned.rs`): the passing
/// lines go, the failure stays, and the lossiness line says what went. Since 2026-09-23 `shadow` reduces
/// too: the whole value stays bound, so a shortened log stops nothing.
#[test]
fn a_large_returned_log_is_reduced_when_the_decision_model_reads_it_as_one() {
    const LOG_CELL: &str = "const lines = [];\nfor (let i = 0; i < 1500; i++) lines.push(`test case_${i} ... ok`);\nlines.push(\"test the_one_that_matters ... FAILED\");\nlines.push(\"test result: FAILED. 1500 passed; 1 failed\");\nreturn { run: lines.join(\"\\n\"), n: 1 };";
    for (label, decisions_toml, reduced) in [
        ("reduce-on", DECISIONS_ON, true),
        ("reduce-shadow", DECISIONS_SHADOW, true),
    ] {
        let root = root(label);
        write_config(&root, decisions_toml);
        let (endpoint, messages, decisions, _headers) = providers(
            vec![cell("c1", LOG_CELL), cell("c2", "answer(\"done\");")],
            vec![],
            vec![],
        );
        let result = exec_bounded(
            &root,
            &endpoint,
            "run the tests and tell me what failed",
            None,
        )
        .expect("the task finishes");
        let messages = messages.lock().unwrap();
        assert_eq!(messages.len(), 2, "{label}: the return buys one more turn");
        let feedback = &messages[1];
        assert!(feedback.contains("### run"), "{label}: {feedback}");
        assert!(
            feedback.contains("test the_one_that_matters ... FAILED"),
            "{label}: the failure always reaches the model: {feedback}"
        );
        if reduced {
            assert!(
                feedback.contains("[sterna:reduction"),
                "{label}: the lossiness line says what went: {feedback}"
            );
            assert!(
                !feedback.contains("test case_1400 ... ok"),
                "{label}: the passing lines went: {feedback}"
            );
            assert!(
                feedback.contains("still live in your bindings"),
                "{label}: {feedback}"
            );
        } else {
            assert!(
                !feedback.contains("[sterna:reduction"),
                "{label}: shadow reduces nothing: {feedback}"
            );
            assert!(
                feedback.contains("test case_1400 ... ok") || feedback.contains("lines not shown"),
                "{label}: the field is shown or paged as it stands: {feedback}"
            );
        }
        let asked: Vec<String> = decisions
            .lock()
            .unwrap()
            .iter()
            .filter(|body| body.contains("\"field_shape\""))
            .cloned()
            .collect();
        assert_eq!(
            asked.len(),
            1,
            "{label}: one shape question for the one large field"
        );
        assert!(
            asked[0].contains("\"field\":\"run\"") && asked[0].contains("line_shapes"),
            "{label}: the state carries the field and its histogram: {}",
            asked[0]
        );
        let shapes = &result["telemetry"]["decisions"]["field_shapes"];
        assert_eq!(shapes[0]["field"], "run", "{label}: {shapes}");
        assert_eq!(shapes[0]["choice"], "log", "{label}: {shapes}");
        assert_eq!(shapes[0]["reduced"], reduced, "{label}: {shapes}");
        assert!(shapes[0]["would_reduce"].is_null(), "{label}: {shapes}");
        let _ = std::fs::remove_dir_all(root);
    }
}

/// A confident `explore` kind (2026-09-23) lowers the task's effort to
/// `low` when the person chose none. In `shadow` mode the same answer is
/// recorded as what would have happened and nothing changes.
#[test]
fn an_explore_request_lowers_effort() {
    for (label, config, acting) in [
        ("kind-explore-on", DECISIONS_ON, true),
        ("kind-explore-shadow", DECISIONS_SHADOW, false),
    ] {
        let root = root(label);
        write_config(&root, config);
        let (endpoint, messages, _decisions, _headers) = providers(
            vec![cell("c1", "answer(\"done\");")],
            vec![Decision::Answer(decision_answer_with_kind(
                "read_only",
                0.94,
                "explore",
                0.90,
            ))],
            vec![],
        );
        let result = exec_bounded(&root, &endpoint, "how does the setup work here", None)
            .expect("the task finishes");
        let messages = messages.lock().unwrap();
        let telemetry = &result["telemetry"]["decisions"];
        assert_eq!(
            telemetry["kind"]["choice"], "explore",
            "{label}: {telemetry}"
        );
        assert_eq!(messages.len(), 1, "{label}: one turn");
        if acting {
            assert!(
                messages[0].contains("\"effort\":\"low\""),
                "{label}: the turn carries low effort: {}",
                messages[0]
            );
            assert_eq!(telemetry["effort"]["set"], "low", "{label}: {telemetry}");
        } else {
            assert!(
                !messages[0].contains("\"effort\""),
                "{label}: shadow sets nothing: {}",
                messages[0]
            );
            assert_eq!(
                telemetry["effort"]["would_set"], "low",
                "{label}: {telemetry}"
            );
        }
        let _ = std::fs::remove_dir_all(root);
    }
}
