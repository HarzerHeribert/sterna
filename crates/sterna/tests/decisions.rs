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
    std::fs::write(dir.join("config.toml"), pinned(text)).unwrap();
}

/// `text` with the `[helpers]` defaults these tests were written against,
/// for every key the test does not set itself: the acceptance list on, and
/// neither the checker behind the answer nor the learned-notes writer, whose
/// extra requests (defaults since 2026-09-23) would shift every scripted
/// answer. A test about one of them sets it and is left alone.
fn pinned(text: &str) -> String {
    let set = |key: &str| {
        text.lines()
            .any(|line| line.trim_start().starts_with(&format!("{key} =")))
    };
    let mut keys = String::new();
    for (key, value) in [
        ("acceptance_list", "true"),
        ("completion_check", "false"),
        ("learn", "false"),
    ] {
        if !set(key) {
            keys.push_str(&format!("{key} = {value}\n"));
        }
    }
    if text.lines().any(|line| line.trim() == "[helpers]") {
        text.replacen("[helpers]\n", &format!("[helpers]\n{keys}"), 1)
    } else {
        format!("{text}\n[helpers]\n{keys}")
    }
}

/// The sentences the gate noted beside the answer without holding it.
fn notes(result: &Value) -> Vec<String> {
    result["telemetry"]["after_answer"]["notes"]
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

/// Which of the three requests this package ever sends to `/v1/systemone`:
/// `"intent"` for the task-start request (which now also carries the
/// `complexity` question in the same `questions` map -- F2 -- so a plain
/// "first key" read would see `complexity` first once `BTreeMap` sorts them),
/// `"satisfied"` for the completion question (2616), or `"drift"` for the
/// per-cell question asked before an effectful cell runs (2643).
fn question_key(body_text: &str) -> String {
    let value: Value = serde_json::from_str(body_text).unwrap();
    let questions = value["questions"].as_object().cloned().unwrap_or_default();
    if questions.contains_key("satisfied") {
        "satisfied".to_string()
    } else if questions.contains_key("supervision") {
        "supervision".to_string()
    } else if questions.contains_key("drift") {
        "drift".to_string()
    } else if questions.contains_key("judge") {
        "judge".to_string()
    } else if questions.contains_key("field_shape") {
        "field_shape".to_string()
    } else if questions.contains_key("enough") {
        "enough".to_string()
    } else if !questions.is_empty() && questions.keys().all(|key| key.parse::<usize>().is_ok()) {
        // A Scout ranking request (2644): one `noul` per candidate, keyed by
        // the candidate's own index, so its key set is never one of the
        // fixed names above.
        "rank".to_string()
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
    providers_full(cells, intent, completion, vec![], vec![], vec![], vec![])
}

/// [`providers`] plus a scripted queue for the supervision question
/// (`supervisor.md` §3): the decision model's own answer about a trajectory.
/// A `/v1/messages` request carrying the supervisor's preamble is answered
/// with one canned line and never consumes a scripted cell, so a test can
/// count prose looks without its cell script shifting under it.
fn providers_with_supervision(
    cells: Vec<Value>,
    supervision: Vec<Decision>,
) -> (String, Recorded, Recorded, Recorded) {
    providers_full(cells, vec![], vec![], vec![], vec![], vec![], supervision)
}

/// [`providers`] plus a fourth scripted queue for the drift question's own
/// key (2643) -- most tests never ask it, so [`providers`] stays the
/// three-argument call every existing test already uses.
fn providers_with_drift(
    cells: Vec<Value>,
    intent: Vec<Decision>,
    completion: Vec<Decision>,
    drift: Vec<Decision>,
) -> (String, Recorded, Recorded, Recorded) {
    providers_full(cells, intent, completion, drift, vec![], vec![], vec![])
}

/// [`providers`] plus scripted queues for the Scout's own ranking question
/// (2644, numeric candidate-index keys) and a judge question (2645, either
/// call site the judge reaches). An unscripted judge question defaults to a
/// confident yes (`checker_judge_answer(0.94)`), so a test scripting only
/// `rank` (or neither) keeps seeing a helper's own result byte-identical to
/// before this feature landed.
fn providers_with_rank_and_judge(
    cells: Vec<Value>,
    intent: Vec<Decision>,
    completion: Vec<Decision>,
    rank: Vec<Decision>,
    judge: Vec<Decision>,
) -> (String, Recorded, Recorded, Recorded) {
    providers_full(cells, intent, completion, vec![], judge, rank, vec![])
}

#[allow(clippy::too_many_arguments)]
fn providers_full(
    cells: Vec<Value>,
    intent: Vec<Decision>,
    completion: Vec<Decision>,
    drift: Vec<Decision>,
    judge: Vec<Decision>,
    rank: Vec<Decision>,
    supervision: Vec<Decision>,
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
        let mut drift: std::collections::VecDeque<Decision> = drift.into_iter().collect();
        let mut judge: std::collections::VecDeque<Decision> = judge.into_iter().collect();
        let mut rank: std::collections::VecDeque<Decision> = rank.into_iter().collect();
        let mut supervision: std::collections::VecDeque<Decision> =
            supervision.into_iter().collect();
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
                    "drift" => drift
                        .pop_front()
                        .unwrap_or_else(|| Decision::Answer(drift_answer(0.50))),
                    "judge" => judge
                        .pop_front()
                        .unwrap_or_else(|| Decision::Answer(checker_judge_answer(0.94))),
                    "rank" => rank
                        .pop_front()
                        .unwrap_or_else(|| Decision::Answer(rank_answer(&[("0", 0.94)]))),
                    "supervision" => supervision.pop_front().unwrap_or_else(|| {
                        Decision::Answer(supervision_answer("making_progress", 0.94))
                    }),
                    // The field-shape question (`session/returned.rs`) is
                    // answered `log` at 0.90 for every field: the one test
                    // that asks it returns a log.
                    "field_shape" => Decision::Answer(field_shape_answer("log", 0.90)),
                    // The enough question (`session/returned.rs::enrich`) is
                    // answered from the request itself: a request that asks
                    // to read everything is never enough, any other is.
                    "enough" => {
                        Decision::Answer(enough_answer(if body_text.contains("read everything") {
                            0.20
                        } else {
                            0.90
                        }))
                    }
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
                let is_look = body_text.contains(SUPERVISOR_MARKER);
                let request: serde_json::Value =
                    serde_json::from_str(&body_text).unwrap_or(serde_json::Value::Null);
                seen_messages.lock().unwrap().push(body_text);
                let response = if is_look {
                    json!({"role": "assistant", "content": [{"type": "text", "text": LOOK_LINE}],
                        "usage": {"input_tokens": 20, "output_tokens": 7}})
                } else {
                    let Some(response) = cells.next() else {
                        return;
                    };
                    response
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

/// The intent answer alone, with a harmless default `complexity` answer
/// (`routine` at a confidence under every `scout_above` a test configures) so
/// every existing hold/override test -- which scripts only the intent choice
/// it cares about -- keeps working now that `decide()` requires an answer for
/// every question it asked, complexity included.
fn decision_answer(choice: &str, confidence: f64) -> Value {
    decision_answer_with_complexity(choice, confidence, "routine", 0.50)
}

/// Both answers from the one task-start request, for a test that scripts the
/// complexity question itself (`preflight::SIGNAL_DECIDED_EXPLORATION`, F2).
fn decision_answer_with_complexity(
    intent_choice: &str,
    intent_confidence: f64,
    complexity_choice: &str,
    complexity_confidence: f64,
) -> Value {
    json!({
        "model": "jev-latest",
        "answers": {
            "intent": {
                "type": "choice",
                "choice": intent_choice,
                "probabilities": {"read_only": intent_confidence, "modify": 0.0, "run": 0.0, "other": 0.0},
                "confidence": intent_confidence,
            },
            "complexity": {
                "type": "choice",
                "choice": complexity_choice,
                "probabilities": {"trivial": 0.0, "routine": 0.0, "needs_exploration": 0.0},
                "confidence": complexity_confidence,
            },
            // The kind question (2026-09-23) rides the same request; an
            // inert answer here, under `KIND_ABOVE`, so every older test
            // keeps its shape. `decision_answer_with_kind` overrides it.
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

/// The enough question's answer: one noul.
fn enough_answer(noul: f64) -> Value {
    json!({
        "model": "jev-latest",
        "answers": {
            "enough": {
                "type": "noul",
                "noul": noul,
            }
        },
        "usage": {"input_tokens": 30, "output_tokens": 8},
    })
}

/// All three task-start answers, for a test that scripts the kind question
/// (2026-09-23): intent and kind as given, complexity `routine` at 0.50.
fn decision_answer_with_kind(
    intent_choice: &str,
    intent_confidence: f64,
    kind_choice: &str,
    kind_confidence: f64,
) -> Value {
    let mut value =
        decision_answer_with_complexity(intent_choice, intent_confidence, "routine", 0.50);
    value["answers"]["kind"] = json!({
        "type": "choice",
        "choice": kind_choice,
        "probabilities": {"explore": 0.0, "fix": 0.0, "implement": 0.0, "question": 0.0, "run": 0.0},
        "confidence": kind_confidence,
    });
    value
}

/// The first words of `supervisor.rs`'s prose preambles -- both the old
/// deciding one and the phrasing one start with it, which is exactly what a
/// test counting *prose looks* wants to match.
const SUPERVISOR_MARKER: &str = "You watch a coding agent's trajectory";

/// What the scripted supervisor model writes when it is asked for a line.
const LOOK_LINE: &str = "you are re-reading the same files; make the edit";

/// The supervision question's answer alone (`supervisor.md` §3): one choice
/// over the four criteria, with the confidence the threshold is read against.
fn supervision_answer(choice: &str, confidence: f64) -> Value {
    json!({
        "model": "jev-latest",
        "answers": {
            "supervision": {
                "type": "choice",
                "choice": choice,
                "probabilities": {
                    "making_progress": 0.0,
                    "repeating_a_failing_call": 0.0,
                    "looping_over_the_same_reads": 0.0,
                    "stopped_without_returning": 0.0,
                },
                "confidence": confidence,
            }
        },
        "usage": {"input_tokens": 30, "output_tokens": 8},
    })
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

/// The drift question's answer alone (2643) -- always a single-key request,
/// unlike `satisfied`'s hygiene and judge additions.
fn drift_answer(noul: f64) -> Value {
    json!({
        "model": "jev-latest",
        "answers": {
            "drift": {
                "type": "noul",
                "noul": noul,
            }
        },
        "usage": {"input_tokens": 30, "output_tokens": 8},
    })
}

/// The completion gate's fresh-checker judge question's answer alone
/// (2645) -- always a single-key request over `{asked, result}`, keyed
/// `"judge"` exactly as the preflight Scout's own result-judge is (the same
/// one `noul` both call sites are judged with).
fn checker_judge_answer(noul: f64) -> Value {
    json!({
        "model": "jev-latest",
        "answers": {
            "judge": {
                "type": "noul",
                "noul": noul,
            }
        },
        "usage": {"input_tokens": 20, "output_tokens": 5},
    })
}

/// A Scout ranking request's answer (2644): one `noul` per candidate index
/// key -- unlike every other question in this file, the key set depends on
/// how many candidates the ranking named, so this builds the map from
/// `(key, noul)` pairs instead of naming one fixed key.
fn rank_answer(scores: &[(&str, f64)]) -> Value {
    let answers: serde_json::Map<String, Value> = scores
        .iter()
        .map(|(key, noul)| ((*key).to_string(), json!({"type": "noul", "noul": noul})))
        .collect();
    json!({
        "model": "jev-latest",
        "answers": Value::Object(answers),
        "usage": {"input_tokens": 20, "output_tokens": 5},
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

/// A completion answer that also scripts one `judge_<n>` noul per entry in
/// `judge`, in order (2642).
fn completion_answer_with_judge(noul: f64, judge: &[f64]) -> Value {
    let mut answers = serde_json::Map::new();
    answers.insert(
        "satisfied".to_string(),
        json!({"type": "noul", "noul": noul}),
    );
    for (index, value) in judge.iter().enumerate() {
        answers.insert(
            format!("judge_{index}"),
            json!({"type": "noul", "noul": value}),
        );
    }
    json!({
        "model": "jev-latest",
        "answers": Value::Object(answers),
        "usage": {"input_tokens": 40, "output_tokens": 12},
    })
}

/// `mode_above = 1.0` keeps the mode proposal (map 2639) out of these tests:
/// with the default 0.85, a confident `read_only` intent would narrow the
/// request to `explore`, where an effectful cell is refused by the profile
/// before the hold could fire -- `tests/request_modes.rs` proves that path;
/// this file proves the hold itself.
const DECISIONS_ON: &str =
    "[decisions]\nmodel = \"jev-latest\"\nmode = \"on\"\nhold_above = 0.85\nmode_above = 1.0\n";
const DECISIONS_SHADOW: &str =
    "[decisions]\nmodel = \"jev-latest\"\nmode = \"shadow\"\nhold_above = 0.85\n";
const DECISIONS_ON_WITH_CHECKER: &str = "[decisions]\nmodel = \"jev-latest\"\nmode = \"on\"\nhold_above = 0.85\n[helpers]\nmodel = \"helper-tier\"\ncompletion_check = true\n";

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

// -- the drift question (2643) -------------------------------------------

const DRIFT_PLAN_CELL: &str = "todo.write([{text: \"write a.txt\", status: \"active\"}]);";
const DRIFT_EFFECT_CELL: &str =
    "await write({path: \"a.txt\", content: \"1\"});\nanswer(\"done\");";

#[test]
fn a_confident_drift_no_holds_the_cell_once_then_lets_it_run() {
    let root = root("drift-hold");
    write_config(&root, DECISIONS_ON);
    let (endpoint, messages, _decisions, _headers) = providers_with_drift(
        vec![
            cell("c1", DRIFT_PLAN_CELL),
            cell("c2", DRIFT_EFFECT_CELL),
            cell("c3", DRIFT_EFFECT_CELL),
        ],
        vec![Decision::Answer(decision_answer("modify", 0.99))],
        vec![],
        // Two confident nos: the second is what a broken once rule would act
        // on (holding the re-issued cell again, forever) -- `exec_bounded`'s
        // own bound turns that into a fast, clean failure rather than a hang.
        vec![
            Decision::Answer(drift_answer(0.05)),
            Decision::Answer(drift_answer(0.05)),
        ],
    );
    let result = exec_bounded(&root, &endpoint, "write the file", None)
        .expect("the once rule keeps the task moving");
    let messages = messages.lock().unwrap();
    assert_eq!(
        messages.len(),
        3,
        "the plan cell, the held effect cell, then the re-issue that runs"
    );
    assert!(
        messages[2].contains("this cell may not do what the plan's current step says")
            && messages[2].contains("write a.txt"),
        "the drift block reaches the model's next turn, naming the step: {}",
        messages[2]
    );
    assert!(root.join("a.txt").exists(), "the re-issued cell ran");
    assert_eq!(result["answer"], "done");
    let telemetry = &result["telemetry"]["decisions"]["drift"];
    assert_eq!(
        telemetry["asked"], 2,
        "asked again on the re-issue, but the once rule still wins: {telemetry}"
    );
    assert_eq!(telemetry["held"], 1, "{telemetry}");
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn an_in_between_drift_answer_runs_the_cell() {
    let root = root("drift-in-between");
    write_config(&root, DECISIONS_ON);
    let (endpoint, messages, _decisions, _headers) = providers_with_drift(
        vec![cell("c1", DRIFT_PLAN_CELL), cell("c2", DRIFT_EFFECT_CELL)],
        vec![Decision::Answer(decision_answer("modify", 0.99))],
        vec![],
        vec![Decision::Answer(drift_answer(0.50))],
    );
    let result = exec_bounded(&root, &endpoint, "write the file", None).expect("no hold, no hang");
    let messages = messages.lock().unwrap();
    assert_eq!(
        messages.len(),
        2,
        "the plan cell, then the effect cell running unheld"
    );
    assert!(
        !messages[1].contains("this cell may not do what the plan's current step says"),
        "{}",
        messages[1]
    );
    assert!(root.join("a.txt").exists());
    let telemetry = &result["telemetry"]["decisions"]["drift"];
    assert_eq!(telemetry["asked"], 1, "{telemetry}");
    assert_eq!(telemetry["held"], 0, "{telemetry}");
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn a_plan_with_no_active_step_asks_nothing() {
    let root = root("drift-no-plan");
    write_config(&root, DECISIONS_ON);
    let (endpoint, messages, decisions, _headers) = providers_with_drift(
        vec![cell("c1", DRIFT_EFFECT_CELL)],
        vec![Decision::Answer(decision_answer("modify", 0.99))],
        vec![],
        vec![],
    );
    let result = exec_bounded(&root, &endpoint, "write the file", None).expect("no hold, no hang");
    assert_eq!(messages.lock().unwrap().len(), 1, "nothing is ever held");
    assert!(root.join("a.txt").exists());
    let bodies = decisions.lock().unwrap();
    assert!(
        bodies.iter().all(|body| !body.contains("\"drift\"")),
        "no active plan step means the drift question is never asked: {bodies:?}"
    );
    let telemetry = &result["telemetry"]["decisions"]["drift"];
    assert_eq!(telemetry["asked"], 0, "{telemetry}");
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn shadow_counts_would_drift_and_runs_the_cell() {
    let root = root("drift-shadow");
    write_config(&root, DECISIONS_SHADOW);
    let (endpoint, messages, _decisions, _headers) = providers_with_drift(
        vec![cell("c1", DRIFT_PLAN_CELL), cell("c2", DRIFT_EFFECT_CELL)],
        vec![Decision::Answer(decision_answer("modify", 0.99))],
        vec![],
        vec![Decision::Answer(drift_answer(0.05))],
    );
    let result =
        exec_bounded(&root, &endpoint, "write the file", None).expect("shadow never holds");
    let messages = messages.lock().unwrap();
    assert_eq!(messages.len(), 2, "shadow runs the cell as today");
    assert!(
        !messages[1].contains("this cell may not do what the plan's current step says"),
        "shadow never reaches the model: {}",
        messages[1]
    );
    assert!(root.join("a.txt").exists(), "shadow still writes the file");
    let telemetry = &result["telemetry"]["decisions"]["drift"];
    assert_eq!(telemetry["would_hold"], 1, "{telemetry}");
    assert_eq!(telemetry["held"], 0, "{telemetry}");
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn a_slow_drift_decision_runs_the_cell_and_counts_drift_failed() {
    let root = root("drift-timeout");
    write_config(&root, DECISIONS_ON);
    let (endpoint, messages, _decisions, _headers) = providers_with_drift(
        vec![cell("c1", DRIFT_PLAN_CELL), cell("c2", DRIFT_EFFECT_CELL)],
        vec![Decision::Answer(decision_answer("modify", 0.99))],
        vec![],
        vec![Decision::Sleep(Duration::from_secs(3))],
    );
    let result = exec_bounded(&root, &endpoint, "write the file", None)
        .expect("a failed decision runs the cell, not a hang");
    let messages = messages.lock().unwrap();
    assert_eq!(
        messages.len(),
        2,
        "the cell runs unheld once the request times out"
    );
    assert!(root.join("a.txt").exists());
    let telemetry = &result["telemetry"]["decisions"]["drift"];
    assert_eq!(telemetry["failed"], 1, "{telemetry}");
    assert_eq!(telemetry["held"], 0, "{telemetry}");
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
        3,
        "one request, all three questions: {body}"
    );
    assert_eq!(body["questions"]["kind"]["type"], "choice");
    assert!(
        body["questions"]["kind"]["criteria"]["explore"].is_string(),
        "{body}"
    );
    assert_eq!(body["questions"]["complexity"]["type"], "choice");
    assert!(
        body["questions"]["complexity"]["criteria"]["needs_exploration"].is_string(),
        "{body}"
    );
    let _ = std::fs::remove_dir_all(root);
}

// -- the preflight signal (F2, map 2614/2615's paragraph) -----------------

const DECISIONS_ON_WITH_PREFLIGHT: &str = "[decisions]\nmodel = \"jev-latest\"\nmode = \"on\"\n\
     [helpers]\nmodel = \"helper-tier\"\npreflight = true\nacceptance_list = false\n";
const DECISIONS_SHADOW_WITH_PREFLIGHT: &str = "[decisions]\nmodel = \"jev-latest\"\nmode = \"shadow\"\n\
     [helpers]\nmodel = \"helper-tier\"\npreflight = true\nacceptance_list = false\n";

const DECISIONS_ON_ONESHOT: &str = "[decisions]\nmodel = \"jev-latest\"\nmode = \"on\"\n\
     [helpers]\nmodel = \"helper-tier\"\nacceptance_list = false\nscout_oneshot = true\n";

/// With `scout_oneshot`, a confident explore is dissected in one toolless
/// request over the project's file listing: the Scout's request carries the
/// tracked files and offers no tool, and its dissection reaches the turn.
#[test]
fn an_explore_request_is_dissected_in_one_request_over_the_file_listing() {
    const DISSECTION: &str = "## Tasks\n1. Read the setup — every step is known\n\n## Files\nscripts/setup.sh — task 1, the setup\n\n## Verify\nbash -n scripts/setup.sh\n\n## Needs\n(none)\n\n## Skip\n(none found)\n";
    let root = root("kind-explore-oneshot");
    write_config(&root, DECISIONS_ON_ONESHOT);
    std::fs::write(root.join("README.md"), "# Demo\n").unwrap();
    std::fs::create_dir_all(root.join("scripts")).unwrap();
    std::fs::write(root.join("scripts/setup.sh"), "#!/bin/sh\n").unwrap();
    let git = |args: &[&str]| {
        std::process::Command::new("git")
            .arg("-C")
            .arg(&root)
            .args(args)
            .output()
            .expect("git runs")
    };
    git(&["init", "-q"]);
    git(&["add", "README.md", "scripts/setup.sh"]);
    let (endpoint, messages, _decisions, _headers) = providers(
        vec![prose(DISSECTION), cell("c1", "answer(\"done\");")],
        vec![Decision::Answer(decision_answer_with_kind(
            "read_only",
            0.94,
            "explore",
            0.90,
        ))],
        vec![],
    );
    let result = exec_bounded(&root, &endpoint, NO_SIGNAL_TASK, None).expect("the task finishes");
    let messages = messages.lock().unwrap();
    assert_eq!(
        messages.len(),
        2,
        "the dissection, then the task's own turn"
    );
    let scout = &messages[0];
    assert!(scout.contains("## Project files"), "{scout}");
    assert!(
        scout.contains("scripts/setup.sh") && scout.contains("README.md"),
        "{scout}"
    );
    assert!(
        !scout.contains("\"tools\""),
        "one request, no tool offered: {scout}"
    );
    assert!(messages[1].contains("1. Read the setup"), "{}", messages[1]);
    assert!(
        messages[1].contains("## Served in full (1)")
            && messages[1].contains("### scripts/setup.sh"),
        "the file the dissection named reaches the turn: {}",
        messages[1]
    );
    assert_eq!(
        result["telemetry"]["decisions"]["scout_brief"],
        "dissection"
    );
}

const DECISIONS_ON_WITH_HELPERS: &str = "[decisions]\nmodel = \"jev-latest\"\nmode = \"on\"\n\
     [helpers]\nmodel = \"helper-tier\"\nacceptance_list = false\n";

/// A harmless scout answer: five headings, each `(none found)` -- content
/// does not matter to these tests, only that the scout was asked at all.
fn scout_prose() -> Value {
    prose(
        "## Constraints\n(none found)\n\n## Files\n(none found)\n\n## Tests\n(none found)\n\n\
         ## Capabilities\n(none found)\n\n## Risks\n(none found)\n",
    )
}

/// A request with no deterministic preflight signal: no missing path, no
/// absent executable, no verification word, under 80 words.
const NO_SIGNAL_TASK: &str = "please give me a hand with something here";

#[test]
fn a_needs_exploration_answer_above_threshold_starts_the_scout_with_the_signal_named() {
    let root = root("scout-signal-above");
    write_config(&root, DECISIONS_ON_WITH_PREFLIGHT);
    let (endpoint, messages, _decisions, _headers) = providers(
        vec![scout_prose(), cell("c1", "answer(\"done\");")],
        vec![Decision::Answer(decision_answer_with_complexity(
            "read_only",
            0.94,
            "needs_exploration",
            0.90,
        ))],
        vec![],
    );
    let result = exec_bounded(&root, &endpoint, NO_SIGNAL_TASK, None)
        .expect("the scout runs, then the task's own turn");
    assert_eq!(
        messages.lock().unwrap().len(),
        2,
        "no deterministic signal alone would have run the scout"
    );
    let telemetry = &result["telemetry"]["decisions"];
    assert_eq!(telemetry["scout_signal"], true, "{telemetry}");
    assert_eq!(telemetry["would_scout"], false, "{telemetry}");
    assert_eq!(
        telemetry["complexity"]["choice"], "needs_exploration",
        "{telemetry}"
    );
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn a_needs_exploration_answer_below_threshold_does_not_start_the_scout() {
    let root = root("scout-signal-below");
    write_config(&root, DECISIONS_ON_WITH_PREFLIGHT);
    let (endpoint, messages, _decisions, _headers) = providers(
        vec![cell("c1", "answer(\"done\");")],
        vec![Decision::Answer(decision_answer_with_complexity(
            "read_only",
            0.94,
            "needs_exploration",
            0.80,
        ))],
        vec![],
    );
    let result = exec_bounded(&root, &endpoint, NO_SIGNAL_TASK, None)
        .expect("no signal at all, no scout, no hang");
    assert_eq!(
        messages.lock().unwrap().len(),
        1,
        "below scout_above, the model's answer adds no reason to scout"
    );
    let telemetry = &result["telemetry"]["decisions"];
    assert_eq!(telemetry["scout_signal"], false, "{telemetry}");
    assert_eq!(telemetry["would_scout"], false, "{telemetry}");
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn a_trivial_answer_never_suppresses_a_deterministic_signal() {
    let root = root("scout-signal-trivial");
    write_config(&root, DECISIONS_ON_WITH_PREFLIGHT);
    let (endpoint, messages, _decisions, _headers) = providers(
        vec![scout_prose(), cell("c1", "answer(\"done\");")],
        vec![Decision::Answer(decision_answer_with_complexity(
            "read_only",
            0.94,
            "trivial",
            0.99,
        ))],
        vec![],
    );
    let result = exec_bounded(
        &root,
        &endpoint,
        "Rename the entry function in src/missing.rs quickly",
        None,
    )
    .expect("the missing path alone runs the scout");
    assert_eq!(
        messages.lock().unwrap().len(),
        2,
        "a missing path is its own deterministic signal, trivial or not"
    );
    let telemetry = &result["telemetry"]["decisions"];
    assert_eq!(
        telemetry["scout_signal"], false,
        "trivial never contributes the decided signal: {telemetry}"
    );
    assert_eq!(telemetry["complexity"]["choice"], "trivial", "{telemetry}");
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn shadow_records_would_scout_and_never_starts_the_scout() {
    let root = root("scout-signal-shadow");
    write_config(&root, DECISIONS_SHADOW_WITH_PREFLIGHT);
    let (endpoint, messages, _decisions, _headers) = providers(
        vec![cell("c1", "answer(\"done\");")],
        vec![Decision::Answer(decision_answer_with_complexity(
            "read_only",
            0.94,
            "needs_exploration",
            0.90,
        ))],
        vec![],
    );
    let result = exec_bounded(&root, &endpoint, NO_SIGNAL_TASK, None)
        .expect("shadow never scouts, never hangs");
    assert_eq!(
        messages.lock().unwrap().len(),
        1,
        "shadow records the answer and changes nothing"
    );
    let telemetry = &result["telemetry"]["decisions"];
    assert_eq!(telemetry["would_scout"], true, "{telemetry}");
    assert_eq!(telemetry["scout_signal"], false, "{telemetry}");
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn a_failed_decision_leaves_preflight_exactly_as_it_is_today() {
    let root = root("scout-signal-failed");
    write_config(&root, DECISIONS_ON_WITH_PREFLIGHT);
    let (endpoint, messages, _decisions, _headers) = providers(
        vec![cell("c1", "answer(\"done\");")],
        vec![Decision::Status(500)],
        vec![],
    );
    let result = exec_bounded(&root, &endpoint, NO_SIGNAL_TASK, None)
        .expect("a failed decision leaves preflight alone, no hang");
    assert_eq!(
        messages.lock().unwrap().len(),
        1,
        "no deterministic signal and no decision to add one"
    );
    let telemetry = &result["telemetry"]["decisions"];
    assert_eq!(telemetry["failed"], 1, "{telemetry}");
    assert!(telemetry["complexity"].is_null(), "{telemetry}");
    assert_eq!(telemetry["scout_signal"], false, "{telemetry}");
    assert_eq!(telemetry["would_scout"], false, "{telemetry}");
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
        result["telemetry"]["after_answer"]["notes"][0]
            .as_str()
            .is_some_and(|note| note.contains("Run a verification")),
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
fn a_confident_yes_spares_the_fresh_checker_when_nothing_else_is_found() {
    let root = root("completion-yes");
    write_config(&root, DECISIONS_ON_WITH_CHECKER);
    let (endpoint, messages, decisions, _headers) = providers(
        vec![cell("c1", "answer(\"done\");")],
        vec![],
        vec![Decision::Answer(completion_answer(0.94))],
    );
    let result = exec_bounded(&root, &endpoint, "fix the bug", None)
        .expect("a spared checker still finishes the task");
    assert_eq!(
        messages.lock().unwrap().len(),
        1,
        "no second request reaches /v1/messages for the checker"
    );
    assert_eq!(result["telemetry"]["completion"]["verified"], true);
    let telemetry = &result["telemetry"]["decisions"]["completion"];
    assert_eq!(telemetry["noul"], 0.94, "{telemetry}");
    assert_eq!(telemetry["checker_skipped"], "decision 0.94", "{telemetry}");
    assert_eq!(
        decisions.lock().unwrap().len(),
        2,
        "the intent question, and the completion question"
    );
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn an_undecided_answer_runs_the_checker_as_today() {
    let root = root("completion-undecided");
    write_config(&root, DECISIONS_ON_WITH_CHECKER);
    let (endpoint, messages, _decisions, _headers) = providers(
        vec![
            cell("c1", "answer(\"done\");"),
            prose("The change holds; nothing more is needed."),
        ],
        vec![],
        vec![Decision::Answer(completion_answer(0.55))],
    );
    let result = exec_bounded(&root, &endpoint, "fix the bug", None)
        .expect("the checker runs and the task still finishes");
    assert_eq!(
        messages.lock().unwrap().len(),
        2,
        "the task turn, then the checker's own request"
    );
    assert_eq!(result["telemetry"]["completion"]["verified"], true);
    let telemetry = &result["telemetry"]["decisions"]["completion"];
    assert_eq!(telemetry["noul"], 0.55, "{telemetry}");
    assert!(telemetry["checker_skipped"].is_null(), "{telemetry}");
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn a_yes_never_removes_a_mechanical_finding() {
    let root = root("completion-yes-with-finding");
    write_config(&root, DECISIONS_ON_WITH_CHECKER);
    write_checks_toml(&root, "[contract]\nrequired = [\"missing.txt\"]\n");
    let (endpoint, messages, _decisions, _headers) = providers(
        vec![
            cell("c1", "answer(\"done\");"),
            cell("c2", "answer(\"done\");"),
            prose("holds\nnothing more is needed."),
        ],
        vec![],
        vec![Decision::Answer(completion_answer(0.94))],
    );
    let result = exec_bounded(&root, &endpoint, "fix the bug", None)
        .expect("a confident yes does not remove the required-path finding");
    assert_eq!(
        messages.lock().unwrap().len(),
        3,
        "held once, the same claim finishes unverified, then the checker behind the answer"
    );
    assert_eq!(
        result["telemetry"]["after_answer"]["checks"][0]["verdict"], "holds",
        "{result}"
    );
    let telemetry = &result["telemetry"]["decisions"]["completion"];
    assert_eq!(telemetry["noul"], 0.94, "{telemetry}");
    assert!(telemetry["checker_skipped"].is_null(), "{telemetry}");
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

// -- the fresh checker, behind the answer (2026-09-23) -----------------------

/// The checker runs after the answer and its verdict is a note for the
/// person: a "does not hold" costs the model no turn and never reaches it,
/// and nothing judges the checker's own result any more (2645's judge sat on
/// a hold that no longer exists).
#[test]
fn a_checker_that_says_does_not_hold_is_a_note_behind_the_answer_and_costs_no_turn() {
    let root = root("checker-after-no");
    write_config(&root, DECISIONS_ON_WITH_CHECKER);
    let (endpoint, messages, decisions, _headers) = providers(
        vec![
            cell("c1", "answer(\"done\");"),
            prose("does not hold\nthe diff misses the retry path"),
        ],
        vec![],
        vec![Decision::Answer(completion_answer(0.55))],
    );
    let result = exec_bounded(&root, &endpoint, "fix the bug", None)
        .expect("a flagged checker never holds the task");
    let bodies = messages.lock().unwrap();
    assert_eq!(
        bodies.len(),
        2,
        "the task turn, then the checker's own request"
    );
    assert!(
        !bodies[0].contains("the diff misses the retry path"),
        "the checker's words never reach the model"
    );
    let check = &result["telemetry"]["after_answer"]["checks"][0];
    assert_eq!(check["verdict"], "does not hold", "{result}");
    assert!(
        check["text"]
            .as_str()
            .unwrap_or("")
            .contains("the diff misses the retry path"),
        "{check}"
    );
    assert_eq!(result["telemetry"]["completion"]["deferred"], 0);
    assert_eq!(
        decisions.lock().unwrap().len(),
        2,
        "the intent question and the completion question -- no judge on the checker"
    );
    let _ = std::fs::remove_dir_all(root);
}

/// A checker that finds the answer holds leaves one quiet verdict, and the
/// task's own figures are those of a run with no checker at all.
#[test]
fn a_checker_that_says_holds_leaves_one_verdict_and_nothing_else() {
    let root = root("checker-after-yes");
    write_config(&root, DECISIONS_ON_WITH_CHECKER);
    let (endpoint, messages, _decisions, _headers) = providers(
        vec![
            cell("c1", "answer(\"done\");"),
            prose("holds\nthe answer does what was asked"),
        ],
        vec![],
        vec![Decision::Answer(completion_answer(0.55))],
    );
    let result = exec_bounded(&root, &endpoint, "fix the bug", None)
        .expect("a checker behind the answer finishes");
    assert_eq!(messages.lock().unwrap().len(), 2);
    let checks = &result["telemetry"]["after_answer"]["checks"];
    assert_eq!(checks.as_array().map(Vec::len), Some(1), "{checks}");
    assert_eq!(checks[0]["verdict"], "holds", "{checks}");
    assert_eq!(result["telemetry"]["completion"]["verified"], true);
    assert_eq!(result["telemetry"]["completion"]["deferred"], 0);
    let _ = std::fs::remove_dir_all(root);
}

/// `decisions.helpers` counts a Scout ranking (2644) and the judge it also
/// runs on the Scout's own returned result (2645, unconditional whenever a
/// decision model is configured and `mode` is not `off`) -- the checker
/// path's own check is proven separately above, with `completion_check`
/// off here so only the Scout's self-check contributes to `checked`.
#[test]
fn decisions_helpers_telemetry_counts_the_scouts_ranking_and_its_own_judge() {
    let root = root("helpers-ranking-telemetry");
    write_config(&root, DECISIONS_ON_WITH_PREFLIGHT);
    std::fs::write(
        root.join("retry.rs"),
        "fn handle_timeout() { /* timeout retry code lives here */ }\n",
    )
    .unwrap();
    let (endpoint, _messages, _decisions, _headers) = providers_with_rank_and_judge(
        vec![scout_prose(), cell("c1", "answer(\"done\");")],
        vec![Decision::Answer(decision_answer_with_complexity(
            "read_only",
            0.94,
            "needs_exploration",
            0.90,
        ))],
        vec![],
        vec![Decision::Answer(rank_answer(&[("0", 0.94)]))],
        vec![Decision::Answer(checker_judge_answer(0.9))],
    );
    let result = exec_bounded(&root, &endpoint, "please find the timeout retry code", None)
        .expect("the scout runs, ranked and judged, then the task's own turn");
    let helpers_telemetry = &result["telemetry"]["decisions"]["helpers"];
    assert_eq!(helpers_telemetry["ranked"], 1, "{helpers_telemetry}");
    assert_eq!(helpers_telemetry["skipped"], 0, "{helpers_telemetry}");
    assert_eq!(
        helpers_telemetry["checked"], 1,
        "the scout's own result is judged too (2645): {helpers_telemetry}"
    );
    assert_eq!(helpers_telemetry["flagged"], 0, "{helpers_telemetry}");
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

const DECISIONS_ON_WITH_LISTER: &str = "[decisions]\nmodel = \"jev-latest\"\nmode = \"on\"\n\
     [helpers]\nmodel = \"helper-tier\"\ncompletion_check = true\n";
/// No `completion_check`, so a judge finding that does not spare the checker
/// (a confident no, or shadow mode never sparing it) does not also pay for
/// one -- these tests are about the judge decision alone.
const DECISIONS_ON_WITH_LISTER_NO_CHECKER: &str =
    "[decisions]\nmodel = \"jev-latest\"\nmode = \"on\"\n[helpers]\nmodel = \"helper-tier\"\n";
const DECISIONS_SHADOW_WITH_LISTER: &str = "[decisions]\nmodel = \"jev-latest\"\nmode = \"shadow\"\n\
     [helpers]\nmodel = \"helper-tier\"\n";

#[test]
fn a_judge_item_answered_yes_is_satisfied_without_the_checker() {
    let root = root("judge-yes");
    write_config(&root, DECISIONS_ON_WITH_LISTER);
    let (endpoint, messages, _decisions, _headers) = providers(
        vec![
            prose("judge: the tone is friendly"),
            cell("c1", "answer(\"done\");"),
        ],
        vec![],
        vec![Decision::Answer(completion_answer_with_judge(
            0.94,
            &[0.95],
        ))],
    );
    let result = exec_bounded(&root, &endpoint, "make sure the tone is friendly", None)
        .expect("a satisfied judge item spares the checker too");
    assert_eq!(
        messages.lock().unwrap().len(),
        2,
        "the lister, then the first turn -- no checker, no hold"
    );
    assert_eq!(result["telemetry"]["completion"]["verified"], true);
    let acceptance = &result["telemetry"]["acceptance"];
    assert_eq!(
        acceptance["judged"], 0,
        "the judge item is now met: {acceptance}"
    );
    assert_eq!(acceptance["met"], 1, "{acceptance}");
    let telemetry = &result["telemetry"]["decisions"]["completion"];
    assert_eq!(telemetry["judged"]["yes"], 1, "{telemetry}");
    assert_eq!(telemetry["judged"]["undecided"], 0, "{telemetry}");
    assert!(telemetry["checker_skipped"].is_string(), "{telemetry}");
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn a_judge_item_answered_no_is_a_note_naming_the_item_and_never_holds() {
    let root = root("judge-no");
    write_config(&root, DECISIONS_ON_WITH_LISTER_NO_CHECKER);
    let (endpoint, messages, _decisions, _headers) = providers(
        vec![
            prose("judge: the tone is friendly"),
            cell("c1", "answer(\"done\");"),
        ],
        vec![],
        vec![Decision::Answer(completion_answer_with_judge(
            0.94,
            &[0.05],
        ))],
    );
    let result = exec_bounded(&root, &endpoint, "make sure the tone is friendly", None)
        .expect("a not-satisfied judge item is a note");
    let messages = messages.lock().unwrap();
    assert_eq!(
        messages.len(),
        2,
        "the lister, the first turn -- no held turn"
    );
    assert!(
        notes(&result)
            .iter()
            .any(|n| n.contains("the tone is friendly")),
        "the note names the item: {result}"
    );
    assert_eq!(result["telemetry"]["completion"]["deferred"], 0);
    let telemetry = &result["telemetry"]["decisions"]["completion"];
    assert_eq!(telemetry["judged"]["no"], 1, "{telemetry}");
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn a_judge_item_answered_undecided_runs_the_checker_as_today() {
    let root = root("judge-undecided");
    write_config(&root, DECISIONS_ON_WITH_LISTER);
    let (endpoint, messages, _decisions, _headers) = providers(
        vec![
            prose("judge: the tone is friendly"),
            cell("c1", "answer(\"done\");"),
            prose("The change holds; nothing more is needed."),
        ],
        vec![],
        vec![Decision::Answer(completion_answer_with_judge(0.94, &[0.5]))],
    );
    let result = exec_bounded(&root, &endpoint, "make sure the tone is friendly", None)
        .expect("an undecided judge item runs the checker and still finishes");
    assert_eq!(
        messages.lock().unwrap().len(),
        3,
        "the lister, the first turn, then the checker's own request"
    );
    assert_eq!(result["telemetry"]["completion"]["verified"], true);
    let telemetry = &result["telemetry"]["decisions"]["completion"];
    assert_eq!(telemetry["judged"]["undecided"], 1, "{telemetry}");
    assert!(telemetry["checker_skipped"].is_null(), "{telemetry}");
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn shadow_records_judged_and_changes_nothing() {
    let root = root("judge-shadow");
    write_config(&root, DECISIONS_SHADOW_WITH_LISTER);
    let (endpoint, messages, _decisions, _headers) = providers(
        vec![
            prose("judge: the tone is friendly"),
            cell("c1", "answer(\"done\");"),
        ],
        vec![],
        vec![Decision::Answer(completion_answer_with_judge(
            0.94,
            &[0.95],
        ))],
    );
    let result = exec_bounded(&root, &endpoint, "make sure the tone is friendly", None)
        .expect("shadow never decides a judge item");
    assert_eq!(
        messages.lock().unwrap().len(),
        2,
        "shadow finishes on the first claim, no checker skip recorded as a hold"
    );
    assert_eq!(result["telemetry"]["completion"]["verified"], true);
    let acceptance = &result["telemetry"]["acceptance"];
    assert_eq!(
        acceptance["judged"], 1,
        "shadow never mutates the acceptance list's own status: {acceptance}"
    );
    let telemetry = &result["telemetry"]["decisions"]["completion"];
    assert_eq!(telemetry["judged"]["yes"], 1, "{telemetry}");
    let _ = std::fs::remove_dir_all(root);
}

// --- supervisor.md §3: the decision model decides, the LLM phrases --------

/// Both models configured, a look after every cell.
const SUPERVISED_BY_BOTH: &str = "[decisions]\nmodel = \"jev-latest\"\nmode = \"on\"\n\
     [supervisor]\nevery = 1\nmodel = \"helper-tier\"\n";

/// The decision model alone: nothing is configured that could write a
/// sentence, so a nudge must carry the criterion's own words.
const SUPERVISED_BY_THE_DECISION_MODEL: &str =
    "[decisions]\nmodel = \"jev-latest\"\nmode = \"on\"\n[supervisor]\nevery = 1\n";

/// One cell that yields (so a look has a next turn to head) and one that
/// ends the task.
fn looping_then_done() -> Vec<Value> {
    vec![cell("c1", "const x = 1;"), cell("c2", "answer(\"done\");")]
}

fn prose_looks(messages: &[String]) -> usize {
    messages
        .iter()
        .filter(|body| body.contains(SUPERVISOR_MARKER))
        .count()
}

/// The saving this layering exists for: a trajectory the decision model
/// calls progress costs one typed question and **no prose request at all**.
#[test]
fn a_progress_answer_buys_no_prose_look() {
    let root = root("supervision-progress");
    write_config(&root, SUPERVISED_BY_BOTH);
    let (endpoint, messages, decisions, _headers) = providers_with_supervision(
        looping_then_done(),
        vec![Decision::Answer(supervision_answer(
            "making_progress",
            0.99,
        ))],
    );
    let result =
        exec_bounded(&root, &endpoint, "keep going", None).expect("the task finishes normally");
    assert_eq!(result["answer"], "done");
    let messages = messages.lock().unwrap();
    assert_eq!(
        prose_looks(&messages),
        0,
        "a confident `making_progress` buys no sentence: {messages:?}"
    );
    assert!(
        decisions
            .lock()
            .unwrap()
            .iter()
            .any(|body| body.contains("\"supervision\"")),
        "the question was asked"
    );
    let _ = std::fs::remove_dir_all(root);
}

/// And the other half: a confident loop buys exactly one sentence, and that
/// sentence heads the next turn.
#[test]
fn a_confident_loop_buys_exactly_one_line_and_nudges_with_it() {
    let root = root("supervision-loop");
    write_config(&root, SUPERVISED_BY_BOTH);
    let (endpoint, messages, _decisions, _headers) = providers_with_supervision(
        looping_then_done(),
        vec![Decision::Answer(supervision_answer(
            "looping_over_the_same_reads",
            0.95,
        ))],
    );
    let result =
        exec_bounded(&root, &endpoint, "keep going", None).expect("a nudge never ends a task");
    assert_eq!(result["answer"], "done");
    let messages = messages.lock().unwrap();
    assert_eq!(
        prose_looks(&messages),
        1,
        "one sentence, bought only after the decision model said yes: {messages:?}"
    );
    assert!(
        messages
            .iter()
            .any(|body| !body.contains(SUPERVISOR_MARKER) && body.contains(LOOK_LINE)),
        "the written line heads the next task turn: {messages:?}"
    );
    let _ = std::fs::remove_dir_all(root);
}

/// **The supervisor nudges and no longer ends a task** (the user,
/// 2026-09-19: everything that makes the harness work against itself goes).
///
/// Three consecutive looks that all decided to intervene used to end the
/// task. Three *model opinions* is not evidence, and the criteria the
/// supervisor matches on — `looping_over_the_same_reads` among them —
/// describe exactly what a careful re-read looks like, so a supervisor with
/// a wrong prior ended real work and the model had no appeal. Measured
/// across three benchmark runs the same day, its nudge fired four, two and
/// five times and was ignored every time with no consequence: too weak to
/// help and strong enough to kill.
///
/// The deterministic stall is the ender now, and it reads the trajectory
/// rather than an opinion of it.
#[test]
fn three_verdicts_in_a_row_nudge_and_never_end_the_task() {
    let root = root("supervision-ends");
    write_config(&root, SUPERVISED_BY_THE_DECISION_MODEL);
    let never_returns: Vec<Value> = (0..12)
        .map(|n| cell(&format!("c{n}"), "const x = 1;"))
        .collect();
    let (endpoint, messages, _decisions, _headers) = providers_with_supervision(
        never_returns,
        (0..3)
            .map(|_| Decision::Answer(supervision_answer("repeating_a_failing_call", 0.95)))
            .collect(),
    );
    let _ = exec_bounded(&root, &endpoint, "keep going", None);
    let messages = messages.lock().unwrap();
    assert!(
        messages.len() > 5,
        "three verdicts no longer stop the task at the third look: {}",
        messages.len()
    );
    assert!(
        !messages
            .iter()
            .any(|body| body.contains("The supervisor has said")),
        "no count of verdicts may end a task: {messages:?}"
    );
    let _ = std::fs::remove_dir_all(root);
}

/// No model configured that could write a sentence: the nudge still fires,
/// carrying the criterion's own words. A worse sentence, never a lost
/// intervention.
#[test]
fn without_a_supervisor_model_the_criterion_is_the_nudge() {
    let root = root("supervision-criterion");
    write_config(&root, SUPERVISED_BY_THE_DECISION_MODEL);
    let (endpoint, messages, _decisions, _headers) = providers_with_supervision(
        looping_then_done(),
        vec![Decision::Answer(supervision_answer(
            "looping_over_the_same_reads",
            0.95,
        ))],
    );
    let result = exec_bounded(&root, &endpoint, "keep going", None).expect("the task finishes");
    assert_eq!(result["answer"], "done");
    let messages = messages.lock().unwrap();
    assert_eq!(prose_looks(&messages), 0, "there is no model to ask");
    assert!(
        messages
            .iter()
            .any(|body| body.contains("the same files are being read again")),
        "the criterion's own words nudge instead: {messages:?}"
    );
    let _ = std::fs::remove_dir_all(root);
}

/// §3's rule, unchanged by the layering: a question that could not be
/// answered is a failed look — never a nudge, and never a prose request
/// bought on a guess.
#[test]
fn an_unanswerable_supervision_question_never_nudges() {
    let root = root("supervision-failed");
    write_config(&root, SUPERVISED_BY_BOTH);
    let (endpoint, messages, _decisions, _headers) =
        providers_with_supervision(looping_then_done(), vec![Decision::Status(500)]);
    let result = exec_bounded(&root, &endpoint, "keep going", None)
        .expect("a failed look leaves the task running");
    assert_eq!(result["answer"], "done");
    let messages = messages.lock().unwrap();
    assert_eq!(prose_looks(&messages), 0, "{messages:?}");
    assert!(
        !messages.iter().any(|body| body.contains("supervisor: ")),
        "a look that could not be made says nothing to the model: {messages:?}"
    );
    let _ = std::fs::remove_dir_all(root);
}

/// A large returned field the decision model reads as a log goes to the
/// reducer before the model sees it (`session/returned.rs`): the rules rung
/// drops the passing lines with no helper request, the failure stays, and
/// the lossiness line says what went. Since 2026-09-23 `shadow` reduces
/// too: the whole value stays bound, so a shortened log stops nothing.
#[test]
fn a_large_returned_log_is_reduced_when_the_decision_model_reads_it_as_one() {
    const LOG_CELL: &str = "const lines = [];\nfor (let i = 0; i < 1500; i++) lines.push(`test case_${i} ... ok`);\nlines.push(\"test the_one_that_matters ... FAILED\");\nlines.push(\"test result: FAILED. 1500 passed; 1 failed\");\nreturn { run: lines.join(\"\\n\"), n: 1 };";
    for (label, decisions_toml, reduced) in [
        ("reduce-on", DECISIONS_ON, true),
        ("reduce-shadow", DECISIONS_SHADOW, true),
    ] {
        let root = root(label);
        write_config(
            &root,
            // `acceptance_list` off: that helper's own `/v1/messages` request
            // would otherwise consume the scripted first cell.
            &format!(
                "{decisions_toml}[helpers]\nmodel = \"helper-tier\"\nreduce_returns = true\nacceptance_list = false\n"
            ),
        );
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

/// A return that names in-project files the program does not hold is
/// enriched with them when the decision model says the return is not
/// enough (`session/returned.rs::enrich`): each arrives as a numbered block
/// under `### [prefetched] path`, read-only and within the return budget,
/// and the ledger says which. When the answer is enough, or in `shadow`
/// mode, nothing is fetched and the ledger says that instead.
#[test]
fn a_return_that_names_files_is_enriched_when_the_decision_model_says_it_is_not_enough() {
    const LISTING_CELL: &str = "return { listing: [\"README.md\", \"scripts/setup.sh\", \"missing.txt\"].join(\"\\n\"), n: 3 };";
    for (label, decisions_toml, request, fetched) in [
        (
            "prefetch-on",
            DECISIONS_ON,
            "read everything and tell me how to start",
            true,
        ),
        ("prefetch-enough", DECISIONS_ON, "list the files", false),
        (
            "prefetch-shadow",
            DECISIONS_SHADOW,
            "read everything and tell me how to start",
            false,
        ),
    ] {
        let root = root(label);
        write_config(
            &root,
            &format!(
                "{decisions_toml}[helpers]\nprefetch_returns = true\nacceptance_list = false\n"
            ),
        );
        std::fs::write(
            root.join("README.md"),
            "# Demo\n\nRun scripts/setup.sh first.\n",
        )
        .unwrap();
        std::fs::create_dir_all(root.join("scripts")).unwrap();
        std::fs::write(
            root.join("scripts/setup.sh"),
            "#!/bin/sh\necho setting up\n",
        )
        .unwrap();
        let (endpoint, messages, decisions, _headers) = providers(
            vec![cell("c1", LISTING_CELL), cell("c2", "answer(\"done\");")],
            vec![],
            vec![],
        );
        let result = exec_bounded(&root, &endpoint, request, None).expect("the task finishes");
        let messages = messages.lock().unwrap();
        assert_eq!(messages.len(), 2, "{label}: the return buys one more turn");
        let feedback = &messages[1];
        assert!(feedback.contains("### listing"), "{label}: {feedback}");
        if fetched {
            assert!(
                feedback.contains("### [prefetched] README.md"),
                "{label}: {feedback}"
            );
            assert!(feedback.contains("1 | # Demo"), "{label}: {feedback}");
            assert!(
                feedback.contains("### [prefetched] scripts/setup.sh"),
                "{label}: {feedback}"
            );
            assert!(
                feedback.contains("2 | echo setting up"),
                "{label}: {feedback}"
            );
            assert!(
                feedback.contains("prefetched, not held") && feedback.contains("[end of file]"),
                "{label}: {feedback}"
            );
        } else {
            assert!(
                !feedback.contains("[prefetched]"),
                "{label}: nothing fetched: {feedback}"
            );
        }
        assert!(
            !feedback.contains("[prefetched] missing.txt"),
            "{label}: {feedback}"
        );
        let asked: Vec<String> = decisions
            .lock()
            .unwrap()
            .iter()
            .filter(|body| body.contains("\"enough\""))
            .cloned()
            .collect();
        assert_eq!(
            asked.len(),
            1,
            "{label}: one enough question for the one return"
        );
        assert!(
            asked[0].contains("\"candidates\":[\"README.md\",\"scripts/setup.sh\"]"),
            "{label}: the state names the candidates that exist: {}",
            asked[0]
        );
        let prefetch = &result["telemetry"]["decisions"]["prefetch"];
        assert_eq!(prefetch[0]["cell"], 1, "{label}: {prefetch}");
        assert_eq!(
            prefetch[0]["prefetched"],
            if fetched {
                json!(["README.md", "scripts/setup.sh"])
            } else {
                json!([])
            },
            "{label}: {prefetch}"
        );
        assert_eq!(
            prefetch[0]["would_prefetch"],
            label == "prefetch-shadow",
            "{label}: {prefetch}"
        );
        let _ = std::fs::remove_dir_all(root);
    }
}

/// A confident `explore` kind (2026-09-23) lowers the task's effort to
/// `low` when the person chose none, runs the Scout with the dissection
/// brief even with no other signal, and the Scout's tasks, files and needs
/// reach the system prompt. In `shadow` mode the same answer is recorded as
/// what would have happened and nothing changes.
#[test]
fn an_explore_request_lowers_effort_and_briefs_the_scout_to_dissect() {
    const DISSECTION: &str = "## Tasks\n1. Find the entry point — the command that starts the server is known\n2. Read the setup — every step scripts/setup.sh takes is listed\n\n## Files\nREADME.md:1 — task 1, the quick start\nscripts/setup.sh:1 — task 2, the setup script\n\n## Verify\nbash -n scripts/setup.sh\n\n## Needs\nwhich backend the person runs on\n\n## Skip\n(none found)\n";
    for (label, config, acting) in [
        // No `preflight = true`: a confident explore runs the dissection on
        // its own -- the A/B of 2026-09-23 ran three arms with the Scout
        // silently off because this arm set it.
        ("kind-explore-on", DECISIONS_ON_WITH_HELPERS, true),
        (
            "kind-explore-shadow",
            DECISIONS_SHADOW_WITH_PREFLIGHT,
            false,
        ),
    ] {
        let root = root(label);
        write_config(&root, config);
        std::fs::write(root.join("README.md"), "# Demo\n").unwrap();
        std::fs::create_dir_all(root.join("scripts")).unwrap();
        std::fs::write(root.join("scripts/setup.sh"), "#!/bin/sh\n").unwrap();
        let mut cells = vec![cell("c1", "answer(\"done\");")];
        if acting {
            cells.insert(0, prose(DISSECTION));
        }
        let (endpoint, messages, _decisions, _headers) = providers(
            cells,
            vec![Decision::Answer(decision_answer_with_kind(
                "read_only",
                0.94,
                "explore",
                0.90,
            ))],
            vec![],
        );
        let result =
            exec_bounded(&root, &endpoint, NO_SIGNAL_TASK, None).expect("the task finishes");
        let messages = messages.lock().unwrap();
        let telemetry = &result["telemetry"]["decisions"];
        assert_eq!(
            telemetry["kind"]["choice"], "explore",
            "{label}: {telemetry}"
        );
        if acting {
            assert_eq!(
                messages.len(),
                2,
                "{label}: the Scout, then the task's own turn"
            );
            let scout = &messages[0];
            assert!(
                scout.contains("## Tasks") && scout.contains("under 300 words"),
                "{label}: {scout}"
            );
            let turn = &messages[1];
            assert!(
                turn.contains("\"effort\":\"low\""),
                "{label}: the turn carries low effort: {turn}"
            );
            assert!(
                turn.contains("## Tasks") && turn.contains("1. Find the entry point"),
                "{label}: {turn}"
            );
            assert!(
                turn.contains("## Needs") && turn.contains("which backend"),
                "{label}: {turn}"
            );
            assert!(turn.contains("scout (dissection)"), "{label}: {turn}");
            assert_eq!(telemetry["effort"]["set"], "low", "{label}: {telemetry}");
            assert_eq!(
                telemetry["scout_brief"], "dissection",
                "{label}: {telemetry}"
            );
        } else {
            assert_eq!(messages.len(), 1, "{label}: no signal, no Scout, one turn");
            assert!(
                !messages[0].contains("\"effort\""),
                "{label}: shadow sets nothing: {}",
                messages[0]
            );
            assert_eq!(
                telemetry["effort"]["would_set"], "low",
                "{label}: {telemetry}"
            );
            assert_eq!(telemetry["would_dissect"], true, "{label}: {telemetry}");
            assert!(telemetry["scout_brief"].is_null(), "{label}: {telemetry}");
        }
        let _ = std::fs::remove_dir_all(root);
    }
}
