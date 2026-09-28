//! Binary-level canaries for the evidence gate and the no-progress guard
//! (`smarter-cheaper-roadmap.md`, *Evidence-gated completion*,
//! *Final-state contract checker*, *No-progress guard*): the three known
//! Terminal-Bench final-state mistakes and a planted repeat, replayed through
//! the built `sterna` binary against a loopback-only fake Messages provider.
//!
//! What is proved here is the session's behaviour, not the checker's rules —
//! those are `tests/final_state_contract.rs`. Every assertion below reads the
//! request bodies the binary actually sent and the machine result it wrote.
use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::process::Command;
use std::sync::{Arc, Mutex};

#[path = "support/sse.rs"]
mod sse;

fn root(label: &str) -> std::path::PathBuf {
    let root = std::env::temp_dir().join(format!(
        "sterna-gate-{label}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&root).unwrap();
    root
}

/// A fake provider answering each request in order and keeping every
/// request body, so a test can read what the model was actually shown.
fn providers(responses: Vec<Value>) -> (String, Arc<Mutex<Vec<String>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let bodies = Arc::new(Mutex::new(Vec::new()));
    let seen = Arc::clone(&bodies);
    std::thread::spawn(move || {
        for response in responses {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(std::time::Duration::from_secs(20)))
                .unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut length = 0;
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
            }
            let mut body = vec![0; length];
            reader.read_exact(&mut body).unwrap();
            seen.lock()
                .unwrap()
                .push(String::from_utf8_lossy(&body).into_owned());
            let request: serde_json::Value =
                serde_json::from_slice(&body).unwrap_or(serde_json::Value::Null);
            let (content_type, response) = sse::response_for(&request, &response.to_string());
            write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{response}", response.len()).unwrap();
        }
    });
    (url, bodies)
}

fn cell(id: &str, code: &str) -> Value {
    json!({"role": "assistant", "content": [{"type": "tool_use", "id": id, "name": "execute_cell", "input": {"code": code}}],
        "usage": {"input_tokens": 20, "output_tokens": 7}})
}

fn prose(text: &str) -> Value {
    json!({"role": "assistant", "content": [{"type": "text", "text": text}],
        "usage": {"input_tokens": 20, "output_tokens": 7}})
}

fn exec_json(root: &std::path::Path, endpoint: &str) -> Value {
    exec_json_task(root, endpoint, "finish the deliverable")
}

fn exec_json_task(root: &std::path::Path, endpoint: &str, task: &str) -> Value {
    let output = Command::new(env!("CARGO_BIN_EXE_sterna"))
        .args(["exec", task, "--output-format", "json", "--root"])
        .arg(root)
        .args(["--model", "test/model"])
        .env("ANTHROPIC_BASE_URL", endpoint)
        .env("ANTHROPIC_API_KEY", "test-only")
        .env("XDG_CONFIG_HOME", root.join("global-config"))
        .env_remove("ANTHROPIC_AUTH_TOKEN")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

/// The polyglot mistake: the deliverable is written and a compiled binary is
/// left beside it. The first `return` is held with the finding; the same
/// return again finishes with the completion recorded unverified.
#[test]
fn a_stray_binary_beside_the_deliverable_holds_the_return_once_and_is_recorded_unverified() {
    let root = root("stray-binary");
    let (endpoint, bodies) = providers(vec![
        cell(
            "c1",
            "await write({path: \"polyglot/main.py.c\", content: \"int main(){return 0;}\\n\"});\n\
             await write({path: \"polyglot/cmain\", content: \"\\u007fELF compiled test binary\"});\n\
             answer(\"done\");",
        ),
        cell("c2", "answer(\"done\");"),
    ]);
    let result = exec_json(&root, &endpoint);
    let completion = &result["telemetry"]["completion"];
    assert_eq!(completion["claimed"], true, "{completion}");
    assert_eq!(completion["verified"], false, "{completion}");
    assert_eq!(completion["deferred"], 1, "{completion}");
    let findings = completion["findings"].as_array().unwrap();
    assert!(
        findings
            .iter()
            .any(|f| f.as_str().unwrap().contains("cmain")),
        "{findings:?}"
    );
    let bodies = bodies.lock().unwrap();
    assert_eq!(bodies.len(), 2, "one held return, then the finish");
    assert!(
        bodies[1].contains("Candidate completion (deferred)") && bodies[1].contains("cmain"),
        "the second request carries the candidate and the finding: {}",
        bodies[1]
    );
    assert_eq!(result["answer"], "done");
    let _ = std::fs::remove_dir_all(root);
}

/// The gcov mistake: coverage data written away from the instrumented source.
#[test]
fn coverage_data_outside_the_source_tree_is_a_finding_before_completion() {
    let root = root("coverage-tree");
    std::fs::create_dir_all(root.join("sqlite/src")).unwrap();
    std::fs::write(
        root.join("sqlite/src/btree.c"),
        "int btree(void){return 1;}\n",
    )
    .unwrap();
    let (endpoint, bodies) = providers(vec![
        cell(
            "c1",
            "await write({path: \"sqlite-gcov-build/btree.gcno\", content: \"gcno\"});\n\
             await write({path: \"sqlite/src/btree.c\", content: \"int btree(void){return 2;}\\n\"});\n\
             answer(\"built with coverage\");",
        ),
        cell("c2", "answer(\"built with coverage\");"),
    ]);
    let result = exec_json(&root, &endpoint);
    let completion = &result["telemetry"]["completion"];
    assert_eq!(completion["verified"], false, "{completion}");
    let findings = completion["findings"].as_array().unwrap();
    assert!(
        findings
            .iter()
            .any(|f| f.as_str().unwrap().contains("btree.gcno")),
        "{findings:?}"
    );
    assert_eq!(bodies.lock().unwrap().len(), 2);
    let _ = std::fs::remove_dir_all(root);
}

/// A project that declares verification: a mutation with no check run is a
/// note beside the answer, never a hold (2026-09-23, *lanes, not gates*: a
/// stale or missing check is not a fact that the work is wrong); without a
/// declared check the same task completes with no note at all.
#[test]
fn a_declared_check_that_never_ran_is_a_note_and_never_holds() {
    let noted = root("declared-check");
    std::fs::create_dir_all(noted.join(".sterna")).unwrap();
    std::fs::write(
        noted.join(".sterna/checks.toml"),
        "[checks.tests]\ncommand = \"true\"\n",
    )
    .unwrap();
    let (endpoint, bodies) = providers(vec![cell(
        "c1",
        "await write({path: \"src/lib.rs\", content: \"pub fn x() {}\\n\"});\nanswer(\"done\");",
    )]);
    let result = exec_json(&noted, &endpoint);
    assert_eq!(bodies.lock().unwrap().len(), 1, "no held turn");
    assert_eq!(result["telemetry"]["completion"]["deferred"], 0);
    let notes = &result["telemetry"]["after_answer"]["notes"];
    assert!(
        notes.as_array().is_some_and(|n| n.iter().any(|n| n
            .as_str()
            .unwrap_or("")
            .contains("Run a verification before finishing"))),
        "{notes}"
    );
    let _ = std::fs::remove_dir_all(noted);

    let plain = root("undeclared-check");
    let (endpoint, bodies) = providers(vec![cell(
        "c1",
        "await write({path: \"src/lib.rs\", content: \"pub fn x() {}\\n\"});\nanswer(\"done\");",
    )]);
    let result = exec_json(&plain, &endpoint);
    assert_eq!(result["telemetry"]["completion"]["verified"], true);
    assert_eq!(result["telemetry"]["completion"]["deferred"], 0);
    assert!(
        result["telemetry"]["after_answer"]["notes"]
            .as_array()
            .is_none_or(Vec::is_empty),
        "{}",
        result["telemetry"]["after_answer"]
    );
    assert_eq!(bodies.lock().unwrap().len(), 1);
    let _ = std::fs::remove_dir_all(plain);
}

/// A fact holds the answer: the task's last verification exited non-zero and
/// nothing changed after it. The claim is held once with the command and its
/// exit; the same claim again finishes with the completion unverified.
#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn a_failed_last_verification_holds_the_answer_once_then_records_it_unverified() {
    let root = root("failed-verification");
    std::fs::create_dir_all(root.join(".sterna")).unwrap();
    std::fs::write(
        root.join(".sterna/checks.toml"),
        "[checks.tests]\ncommand = \"/usr/bin/false\"\n",
    )
    .unwrap();
    std::fs::write(
        root.join(".sterna/config.toml"),
        "[permissions]\nallow = [\"Bash(/usr/bin/false)\"]\n",
    )
    .unwrap();
    let (endpoint, bodies) = providers(vec![
        cell(
            "c1",
            "const run = await checks.run('tests');\nreturn {exit: run.exit_code};",
        ),
        prose("Done: the tests were run."),
        prose("Done: the tests were run."),
    ]);
    let result = exec_json(&root, &endpoint);
    let bodies = bodies.lock().unwrap();
    assert_eq!(
        bodies.len(),
        3,
        "first turn, held claim, same claim again: {result}"
    );
    assert!(
        bodies[2].contains("exited 1, and nothing changed after it"),
        "the held claim names the failed verification: {}",
        bodies[2]
    );
    let completion = &result["telemetry"]["completion"];
    assert_eq!(completion["verified"], false, "{completion}");
    assert_eq!(completion["deferred"], 1, "{completion}");
    let _ = std::fs::remove_dir_all(root);
}

/// A planted repeat: the same failing call twice with an unchanged tree gets
/// one notice at the head of the next feedback, and it is counted.
#[test]
fn an_identical_failing_cell_repeated_is_noticed_once_and_counted() {
    let root = root("no-progress");
    let repeat = "const missing = await read({path: \"missing.txt\"});";
    let (endpoint, bodies) = providers(vec![
        cell("c1", repeat),
        cell("c2", repeat),
        cell("c3", "answer(\"gave up\");"),
    ]);
    let result = exec_json(&root, &endpoint);
    assert_eq!(result["telemetry"]["progress"]["no_progress_notices"], 1);
    let bodies = bodies.lock().unwrap();
    assert_eq!(bodies.len(), 3);
    assert!(
        !bodies[1].contains("repeated without progress"),
        "the first failure is not a repeat: {}",
        bodies[1]
    );
    assert!(
        bodies[2].contains("repeated without progress"),
        "the second identical failure is noticed: {}",
        bodies[2]
    );
    assert_eq!(result["answer"], "gave up");
    let _ = std::fs::remove_dir_all(root);
}

/// The capsule reaches the parent as a `## Task` block once it has something
/// to say, and rides the result as telemetry.
#[test]
fn the_task_capsule_reaches_the_feedback_and_the_result() {
    let root = root("capsule");
    let (endpoint, bodies) = providers(vec![
        cell(
            "c1",
            "await write({path: \"notes.txt\", content: \"hello\\n\"});",
        ),
        cell("c2", "answer(\"done\");"),
    ]);
    let result = exec_json(&root, &endpoint);
    let capsule = &result["telemetry"]["capsule"];
    assert!(capsule.is_object(), "{result}");
    assert_eq!(capsule["goal"], "finish the deliverable", "{capsule}");
    let bodies = bodies.lock().unwrap();
    assert!(
        bodies[1].contains("## Task") && bodies[1].contains("notes.txt"),
        "{}",
        bodies[1]
    );
    let _ = std::fs::remove_dir_all(root);
}

/// The shape the first hybrid Terminal-Bench trial showed on 2026-09-13: the
/// model returns a structured value (notebook output, not terminal) and then
/// finishes in prose. The gate covers that prose completion the same way:
/// held once with the finding, then recorded unverified.
#[test]
fn a_prose_completion_after_a_stray_binary_is_held_once_and_recorded_unverified() {
    let root = root("prose-stray-binary");
    let (endpoint, bodies) = providers(vec![
        cell(
            "c1",
            "await write({path: \"polyglot/main.py.c\", content: \"int main(){return 0;}\\n\"});\n\
             await write({path: \"polyglot/cmain\", content: \"\\u007fELF compiled test binary\"});\n\
             return {ok: true};",
        ),
        prose("Done: the deliverable is written."),
        prose("Done: the deliverable is written."),
    ]);
    let result = exec_json(&root, &endpoint);
    let completion = &result["telemetry"]["completion"];
    assert_eq!(completion["claimed"], true, "{completion}");
    assert_eq!(completion["verified"], false, "{completion}");
    assert_eq!(completion["deferred"], 1, "{completion}");
    assert!(
        completion["findings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|f| f.as_str().unwrap().contains("cmain")),
        "{completion}"
    );
    let bodies = bodies.lock().unwrap();
    assert_eq!(
        bodies.len(),
        3,
        "the structured return, one held prose completion, then the finish"
    );
    assert!(
        bodies[2].contains("Candidate completion (deferred)") && bodies[2].contains("cmain"),
        "the held prose completion reaches the model with the finding: {}",
        bodies[2]
    );
    assert_eq!(result["answer"], "Done: the deliverable is written.");
    let _ = std::fs::remove_dir_all(root);
}

/// A prose completion with nothing to find completes at once, and the
/// completion is now recorded (it was absent for prose before 2026-09-13).
#[test]
fn a_prose_completion_with_nothing_to_find_completes_verified_in_one_turn() {
    let root = root("prose-clean");
    let (endpoint, bodies) = providers(vec![
        cell(
            "c1",
            "await write({path: \"notes.txt\", content: \"hello\\n\"});",
        ),
        prose("Done: notes written."),
    ]);
    let result = exec_json(&root, &endpoint);
    let completion = &result["telemetry"]["completion"];
    assert_eq!(completion["claimed"], true, "{completion}");
    assert_eq!(completion["verified"], true, "{completion}");
    assert_eq!(completion["deferred"], 0, "{completion}");
    assert_eq!(bodies.lock().unwrap().len(), 2);
    assert_eq!(result["answer"], "Done: notes written.");
    let _ = std::fs::remove_dir_all(root);
}

/// The request-derived acceptance list (`acceptance.rs`), opted into: the
/// lister answers first, its items are shown to the model before its first
/// turn, and an item the finished tree does not meet is a note beside the
/// answer with what was observed -- never a hold (2026-09-23: derived items
/// were the measured false alarms).
#[test]
fn an_unmet_acceptance_item_is_a_note_with_what_was_observed_and_never_holds() {
    let root = root("acceptance");
    std::fs::create_dir_all(root.join(".sterna")).unwrap();
    std::fs::write(
        root.join(".sterna/config.toml"),
        "[helpers]\nmodel = \"test/helper\"\npreflight = false\nacceptance_list = true\n\
         completion_check = \"off\"\nlearn = false\n",
    )
    .unwrap();
    let (endpoint, bodies) = providers(vec![
        // The lister's answer: two file items and one judge item.
        prose(
            "file: out/a.txt exists\nfile: out/b.txt exists\njudge: the files are named as requested",
        ),
        cell(
            "c1",
            "await write({path: \"out/a.txt\", content: \"a\\n\"});\nreturn {ok: true};",
        ),
        prose("Done: both files are written."),
    ]);
    // Four words or more: a shorter request needs no repository and gets no
    // lister, the same rule as the preflight.
    let result = exec_json_task(&root, &endpoint, "write out/a.txt and out/b.txt for me");
    let bodies = bodies.lock().unwrap();
    assert_eq!(
        bodies.len(),
        3,
        "lister, first turn, the answer -- no held turn"
    );
    assert!(
        bodies[0].contains("acceptance items"),
        "the first request is the lister's: {}",
        &bodies[0][..bodies[0].len().min(600)]
    );
    assert!(
        bodies[1].contains("## Acceptance list") && bodies[1].contains("file `out/b.txt` exists"),
        "the model sees the list before its first turn: {}",
        bodies[1]
    );
    let notes: Vec<String> = result["telemetry"]["after_answer"]["notes"]
        .as_array()
        .map(|n| {
            n.iter()
                .map(|n| n.as_str().unwrap_or("").to_string())
                .collect()
        })
        .unwrap_or_default();
    assert!(
        notes
            .iter()
            .any(|n| n.contains("Acceptance item not met: file `out/b.txt` exists — absent")),
        "the note names the unmet item: {notes:?}"
    );
    assert!(
        !notes
            .iter()
            .any(|n| n.contains("out/a.txt` exists — absent")),
        "the met item is not a note: {notes:?}"
    );
    let completion = &result["telemetry"]["completion"];
    assert_eq!(completion["deferred"], 0, "{completion}");
    let acceptance = &result["telemetry"]["acceptance"];
    assert_eq!(acceptance["items"], 3, "{acceptance}");
    assert_eq!(acceptance["met"], 1, "{acceptance}");
    assert_eq!(acceptance["unmet"], 1, "{acceptance}");
    assert_eq!(acceptance["judged"], 1, "{acceptance}");
    let _ = std::fs::remove_dir_all(root);
}

/// The stall notice (`progress::Stall`): six cells in a row producing nothing
/// this task has not already seen get one notice at the head of the next
/// feedback, counted, and the task goes on.
///
/// **Seven reads, six repeats.** The first read of a file is new information
/// however many times the model reads it afterwards, so a run of identical
/// frames starts counting on the second. The variable name differs on every
/// cell here and changes nothing: `progress::fingerprint` reads the calls,
/// their arguments and the tree, never the model's source.
#[test]
fn six_repeated_cells_get_one_stall_notice_and_the_task_continues() {
    let root = root("stall");
    let mut responses = vec![cell(
        "c1",
        "await write({path: \"notes.txt\", content: \"hello\\n\"});",
    )];
    for i in 0..7 {
        responses.push(cell(
            &format!("r{i}"),
            &format!("const look{i} = await read({{path: \"notes.txt\"}});"),
        ));
    }
    responses.push(prose("Done: read it seven times."));
    let (endpoint, bodies) = providers(responses);
    let result = exec_json(&root, &endpoint);
    let bodies = bodies.lock().unwrap();
    assert_eq!(bodies.len(), 9);
    assert!(
        !bodies[7].contains("No progress for"),
        "the first read was new, so five repeats are not yet a stall: {}",
        bodies[7]
    );
    assert!(
        bodies[8].contains("No progress for 6 cells"),
        "the sixth repeat is noticed: {}",
        bodies[8]
    );
    assert_eq!(result["telemetry"]["progress"]["stall_notices"], 1);
    assert_eq!(result["telemetry"]["progress"]["no_progress_notices"], 0);
    assert_eq!(result["answer"], "Done: read it seven times.");
    let _ = std::fs::remove_dir_all(root);
}
