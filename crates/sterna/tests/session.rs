//! Acceptance tests for `sterna session` (map lines 2444, 2446, 2447, 2448,
//! 2449, 2450 and 2457). Every test here drives the **built `sterna` binary** as a subprocess
//! (`env!("CARGO_BIN_EXE_sterna")`) -- the packet these prove exists precisely
//! because the six modules session.rs wires together were, until now,
//! correct and reachable from nothing but their own unit tests.
//!
//! No test reaches the real network or a real credential: the Anthropic
//! endpoint is a hand-rolled HTTP/1.1 server bound to `127.0.0.1:0`, and
//! every "glasshouse" is a shell script this file writes into its own temp
//! directory, exactly as `tests/seams.rs` and `tests/ruler_run.rs` already
//! do. 61D's sandbox is not built, so nothing model-authored may execute
//! here either.

use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::thread;

fn scratch_dir(label: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "sterna-session-test-{}-{}-{}",
        label,
        std::process::id(),
        unique()
    ));
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn unique() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    NEXT.fetch_add(1, Ordering::Relaxed)
}

/// Guards [`a_poisoned_inherited_global_config_never_reaches_the_session`]'s
/// own brief change of this test process's `XDG_CONFIG_HOME`, so no other
/// thread can read it mid-poison.
static ENV_LOCK: Mutex<()> = Mutex::new(());

/// Most session fixtures are not about model selection, so they name the
/// historical fixture model explicitly. A test that already persisted a
/// parent omits the CLI flag, preserving the production precedence rule.
///
/// The check reads the project and **the child's own isolated global
/// folder**, never this process's: every spawning helper in this file
/// isolates the child's global scope to `<root>/global-config`
/// (`GH-PANE-TEST-CONFIG-ISOLATION`), and a model choice is saved there
/// (decision 6). Checking the real global here would let a developer's or a
/// measurement's own `~/.config/sterna/config.toml` decide `persisted`,
/// while the isolated child sees no such thing -- exactly the mismatch that
/// made a real global config turn `--model` into a silently skipped flag
/// and the session into a "no parent model selected" refusal.
fn supply_test_model(command: &mut Command, root: &Path) {
    let persisted =
        sterna::settings::Store::with_global(root, Some(root.join("global-config").join("sterna")))
            .and_then(|store| store.load(None))
            .ok()
            .and_then(|loaded| loaded.config.model.parent)
            .is_some();
    if !persisted {
        command.arg("--model").arg(sterna::wire::MODEL);
    }
}

/// Unix only: the fakes are shell scripts. The Windows sterna cell runs every
/// other test in this file; a `.cmd` twin is the successor if one is wanted.
#[cfg(unix)]
fn write_script(dir: &Path, name: &str, body: &str) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let path = dir.join(name);
    fs::write(&path, body).unwrap();
    let mut perms = fs::metadata(&path).unwrap().permissions();
    perms.set_mode(0o755);
    fs::set_permissions(&path, perms).unwrap();
    path
}

/// Binds an ephemeral local port and drops the listener immediately, so any
/// connection to it is refused fast, locally, and without ever reaching a
/// real host -- the guard every test that must not send a request uses for
/// `ANTHROPIC_BASE_URL`.
fn refused_base_url() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    format!("http://127.0.0.1:{port}")
}

/// A minimal Anthropic Messages endpoint: for each reply in `replies`, in
/// order, accepts one connection, reads the request body up to its declared
/// `Content-Length`, records it, and answers with that reply's bytes as a
/// `200 application/json` response. Exits once every reply has been sent.
fn start_fake_provider(replies: Vec<String>) -> (String, Arc<Mutex<Vec<String>>>) {
    let turns = replies.len();
    let next = Mutex::new(0usize);
    start_answering_provider(turns, move |_body| {
        let mut index = next.lock().unwrap();
        let reply = replies[*index].clone();
        *index += 1;
        reply
    })
}

/// The same endpoint, answering each request from the **request body**.
///
/// A task's second turn is only meaningful if the model saw what the runtime
/// said in the first: a fixed list answers a request nobody looked at, and
/// would pass just as happily if the result block had been empty.
fn start_answering_provider<F>(turns: usize, answer: F) -> (String, Arc<Mutex<Vec<String>>>)
where
    F: Fn(&str) -> String + Send + 'static,
{
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let bodies = Arc::new(Mutex::new(Vec::new()));
    let bodies_thread = Arc::clone(&bodies);

    thread::spawn(move || {
        for _ in 0..turns {
            let Ok((stream, _)) = listener.accept() else {
                return;
            };
            handle_one_request(stream, &answer, &bodies_thread);
        }
    });

    (format!("http://127.0.0.1:{port}"), bodies)
}

#[path = "support/sse.rs"]
mod sse;

fn handle_one_request<F: Fn(&str) -> String>(
    mut stream: TcpStream,
    answer: &F,
    bodies: &Mutex<Vec<String>>,
) {
    let mut reader = BufReader::new(stream.try_clone().unwrap());
    let mut content_length = 0usize;
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).unwrap_or(0) == 0 {
            return;
        }
        if line == "\r\n" || line == "\n" {
            break;
        }
        if let Some(rest) = line.to_ascii_lowercase().strip_prefix("content-length:") {
            content_length = rest.trim().parse().unwrap_or(0);
        }
    }
    let mut body = vec![0u8; content_length];
    if reader.read_exact(&mut body).is_err() {
        return;
    }
    let body = String::from_utf8_lossy(&body).into_owned();
    let reply = answer(&body);
    let request: serde_json::Value = serde_json::from_str(&body).unwrap_or(serde_json::Value::Null);
    bodies.lock().unwrap().push(body);

    // **Answer in the transport the request asked for.** A narrowed helper
    // loop — SCOUT, CHECKER — streams, because its ceiling measures silence
    // rather than duration (`wire::SIDE_ERRAND_SILENCE`). A fixture that
    // always wrote JSON left such a caller reading a body with no
    // `message_stop` in it, which is how a resolved Scout went missing from
    // the notebook here.
    let (content_type, reply) = sse::response_for(&request, &reply);
    let response_body = reply.as_bytes();
    let response = format!(
        "HTTP/1.1 200 OK\r\ncontent-type: {content_type}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
        response_body.len()
    );
    let _ = stream.write_all(response.as_bytes());
    let _ = stream.write_all(response_body);
    let _ = stream.flush();
}

/// The same endpoint, answering with a **status** as well as a body.
///
/// A context overflow is a 400, not a reply, so a recovery test cannot be
/// written against a provider that only ever answers 200.
fn start_status_answering_provider<F>(turns: usize, answer: F) -> (String, Arc<Mutex<Vec<String>>>)
where
    F: Fn(&str) -> (u16, String) + Send + 'static,
{
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let bodies = Arc::new(Mutex::new(Vec::new()));
    let bodies_thread = Arc::clone(&bodies);

    thread::spawn(move || {
        for _ in 0..turns {
            let Ok((stream, _)) = listener.accept() else {
                return;
            };
            handle_one_request_with_status(stream, &answer, &bodies_thread);
        }
    });

    (format!("http://127.0.0.1:{port}"), bodies)
}

fn handle_one_request_with_status<F: Fn(&str) -> (u16, String)>(
    mut stream: TcpStream,
    answer: &F,
    bodies: &Mutex<Vec<String>>,
) {
    let mut reader = BufReader::new(stream.try_clone().unwrap());
    let mut content_length = 0usize;
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).unwrap_or(0) == 0 {
            return;
        }
        if line == "\r\n" || line == "\n" {
            break;
        }
        if let Some(rest) = line.to_ascii_lowercase().strip_prefix("content-length:") {
            content_length = rest.trim().parse().unwrap_or(0);
        }
    }
    let mut body = vec![0u8; content_length];
    if reader.read_exact(&mut body).is_err() {
        return;
    }
    let body = String::from_utf8_lossy(&body).into_owned();
    let (status, reply) = answer(&body);
    bodies.lock().unwrap().push(body);

    let response_body = reply.as_bytes();
    let response = format!(
        "HTTP/1.1 {status} X\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
        response_body.len()
    );
    let _ = stream.write_all(response.as_bytes());
    let _ = stream.write_all(response_body);
    let _ = stream.flush();
}

/// The body a provider sends when the conversation no longer fits.
fn too_long_body() -> String {
    serde_json::json!({
        "type": "error",
        "error": {
            "type": "invalid_request_error",
            "message": "prompt is too long: 250000 tokens > 200000 maximum"
        }
    })
    .to_string()
}

/// Builds a Messages-shaped response through `serde_json` rather than a
/// format string, so a reply carrying a quote or a newline (the shape
/// `nothing_the_model_returns_is_executed` needs) still serialises to valid
/// JSON.
fn assistant_reply(text: &str) -> String {
    serde_json::json!({
        "role": "assistant",
        "content": [{"type": "text", "text": text}],
    })
    .to_string()
}

fn native_cell_reply(id: &str, code: &str) -> String {
    serde_json::json!({
        "role": "assistant",
        "content": [{"type":"tool_use","id":id,"name":"execute_cell","input":{"code":code}}],
    })
    .to_string()
}

/// The ChatGPT backend's sticky routing (`x-codex-turn-state`): the token a
/// task's first response hands out goes back on every later request of that
/// task, and never into the next task, which gets its own.
#[test]
fn a_tasks_requests_echo_the_routing_token_its_first_response_gave() {
    let root = scratch_dir("turn-routing");
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
    let seen = Arc::new(Mutex::new(Vec::<Option<String>>::new()));
    let seen_thread = Arc::clone(&seen);
    let sessions = Arc::new(Mutex::new(Vec::<Option<String>>::new()));
    let sessions_thread = Arc::clone(&sessions);
    thread::spawn(move || {
        let replies = [
            (native_cell_reply("a", "const x = 1; return x;"), Some("T1")),
            (ending_reply(), None),
            (native_cell_reply("b", "const y = 2; return y;"), Some("T2")),
            (ending_reply(), None),
        ];
        for (reply, token) in replies {
            let Ok((mut stream, _)) = listener.accept() else {
                return;
            };
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let (mut length, mut routing, mut session) = (0usize, None, None);
            loop {
                let mut line = String::new();
                if reader.read_line(&mut line).unwrap_or(0) == 0 || line.trim().is_empty() {
                    break;
                }
                let lower = line.to_ascii_lowercase();
                if let Some(rest) = lower.strip_prefix("content-length:") {
                    length = rest.trim().parse().unwrap_or(0);
                }
                if lower.starts_with("x-codex-turn-state:") {
                    routing = Some(line.split_once(':').unwrap().1.trim().to_string());
                }
                if lower.starts_with("x-claude-code-session-id:") {
                    session = Some(line.split_once(':').unwrap().1.trim().to_string());
                }
            }
            let mut body = vec![0u8; length];
            let _ = reader.read_exact(&mut body);
            seen_thread.lock().unwrap().push(routing);
            sessions_thread.lock().unwrap().push(session);
            let header = token
                .map(|t| format!("x-codex-turn-state: {t}\r\n"))
                .unwrap_or_default();
            let response = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\n{header}connection: close\r\n\r\n{reply}",
                reply.len()
            );
            let _ = stream.write_all(response.as_bytes());
        }
    });
    let output = run_session_stdin(
        &root,
        &root.join("rollout.jsonl"),
        "turn-routing",
        &["first task", "second task"],
        &base,
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        *seen.lock().unwrap(),
        vec![None, Some("T1".to_string()), None, Some("T2".to_string())]
    );
    // The proxy keys its upstream prompt cache on this header; every request
    // names the one session so each lands where its prefix is cached.
    assert_eq!(
        *sessions.lock().unwrap(),
        vec![Some("turn-routing".to_string()); 4]
    );
}

/// A request without its message-level cache breakpoint: the marker moves to
/// the newest message every turn and is not part of the cached content, so a
/// prefix is compared without it.
fn without_cache_marks(request: &serde_json::Value) -> serde_json::Value {
    let mut request = request.clone();
    for message in request["messages"].as_array_mut().unwrap() {
        if let Some(blocks) = message["content"].as_array_mut() {
            for block in blocks {
                if let Some(block) = block.as_object_mut() {
                    block.remove("cache_control");
                }
            }
        }
    }
    request
}

/// The last block of a request's newest message.
fn newest_block(request: &serde_json::Value) -> &serde_json::Value {
    let message = request["messages"].as_array().unwrap().last().unwrap();
    message["content"].as_array().unwrap().last().unwrap()
}

fn reasoning_cell_reply(id: &str, code: &str) -> String {
    serde_json::json!({
        "role": "assistant",
        "content": [
            {"type":"thinking","thinking":format!("plan for {id}"),"signature":format!("enc-{id}")},
            {"type":"tool_use","id":id,"name":"execute_cell","input":{"code":code}}
        ],
    })
    .to_string()
}

/// History is append-only (user, 2026-09-24): every request re-sends the one
/// before it byte for byte -- the model's signed reasoning included -- and
/// only appends. That is what the provider's cache and the reasoning's own
/// validity both depend on.
#[test]
fn each_request_resends_the_last_one_unchanged_with_its_reasoning() {
    let root = scratch_dir("append-only-reasoning");
    let (url, bodies) = start_fake_provider(vec![
        reasoning_cell_reply("first", "const x = 1; return x;"),
        reasoning_cell_reply("second", "const y = 2; return y;"),
        reasoning_cell_reply("third", "answer(`done`);"),
    ]);
    let output = run_session(
        &root,
        &root.join("rollout.jsonl"),
        "append-only",
        "work",
        &url,
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let bodies = bodies.lock().unwrap();
    assert_eq!(bodies.len(), 3);
    for pair in bodies.windows(2) {
        let earlier: serde_json::Value = serde_json::from_str(&pair[0]).unwrap();
        let later: serde_json::Value = serde_json::from_str(&pair[1]).unwrap();
        assert_eq!(earlier["system"], later["system"]);
        assert_eq!(newest_block(&later)["cache_control"]["type"], "ephemeral");
        let (earlier, later) = (without_cache_marks(&earlier), without_cache_marks(&later));
        let (earlier, later) = (
            earlier["messages"].as_array().unwrap(),
            later["messages"].as_array().unwrap(),
        );
        assert!(later.len() > earlier.len());
        assert_eq!(
            earlier[..],
            later[..earlier.len()],
            "a request edited what the one before it sent"
        );
    }
    let last: serde_json::Value = serde_json::from_str(&bodies[2]).unwrap();
    let reasoning: Vec<&serde_json::Value> = last["messages"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|message| message["content"].as_array().into_iter().flatten())
        .filter(|block| block["type"] == "thinking")
        .collect();
    assert_eq!(
        reasoning.len(),
        2,
        "both earlier turns' reasoning goes back: {last}"
    );
    assert_eq!(reasoning[0]["signature"], "enc-first");
    assert_eq!(reasoning[1]["thinking"], "plan for second");
}

#[test]
fn native_cell_result_is_correlated_before_the_next_request() {
    let root = scratch_dir("native-handoff");
    let rollout = root.join("rollout.jsonl");
    let (base, bodies) = start_fake_provider(vec![
        native_cell_reply("call-read", "const x = 6 * 7; console.log(x);"),
        native_cell_reply("call-return", "answer(`done ${x}`);"),
    ]);
    let output = run_session(&root, &rollout, "native-handoff", "compute", &base);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let bodies = bodies.lock().unwrap();
    let second: serde_json::Value = serde_json::from_str(&bodies[1]).unwrap();
    let result = second["messages"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|message| message["content"].as_array().into_iter().flatten())
        .find(|block| block["type"] == "tool_result" && block["tool_use_id"] == "call-read")
        .expect("the exact correlated result must reach request two");
    assert!(result["content"].as_str().unwrap().contains("42"));
    let saved = rollout_lines(&rollout);
    assert!(
        saved
            .iter()
            .any(|line| line["blocks"][0]["type"] == "tool_use"
                && line["blocks"][0]["id"] == "call-read")
    );
    assert!(
        saved
            .iter()
            .any(|line| line["blocks"][0]["type"] == "tool_result"
                && line["blocks"][0]["tool_use_id"] == "call-read")
    );
    let call = saved
        .iter()
        .position(|line| line["blocks"][0]["id"] == "call-return")
        .unwrap();
    let result = saved
        .iter()
        .position(|line| line["blocks"][0]["tool_use_id"] == "call-return")
        .unwrap();
    let terminal = saved
        .iter()
        .rposition(|line| line["role"] == "assistant" && line["text"] == "done 42")
        .unwrap();
    assert!(call < result && result < terminal);
    assert!(
        saved[result]["blocks"][0]["content"]
            .as_str()
            .unwrap()
            .contains("## Return\ndone 42")
    );
    assert_eq!(cell_lines(&rollout).len(), 2);
}

#[test]
fn multiple_native_calls_are_all_rejected_without_execution() {
    let root = scratch_dir("native-multiple");
    let rollout = root.join("rollout.jsonl");
    let first = serde_json::json!({"role":"assistant","content":[
        {"type":"tool_use","id":"a","name":"execute_cell","input":{"code":"globalThis.bad = 1"}},
        {"type":"tool_use","id":"b","name":"other","input":{"code":"globalThis.worse = 1"}}
    ]})
    .to_string();
    let (base, bodies) =
        start_fake_provider(vec![first, native_cell_reply("finish", "answer('safe');")]);
    let output = run_session(&root, &rollout, "native-multiple", "do it", &base);
    assert!(output.status.success());
    assert_eq!(
        cell_lines(&rollout).len(),
        1,
        "only the final return may execute"
    );
    let second: serde_json::Value = serde_json::from_str(&bodies.lock().unwrap()[1]).unwrap();
    let results: Vec<_> = second["messages"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|message| message["content"].as_array().into_iter().flatten())
        .filter(|block| block["type"] == "tool_result")
        .collect();
    assert_eq!(results.len(), 2);
    assert_eq!(results[0]["tool_use_id"], "a");
    assert_eq!(results[1]["tool_use_id"], "b");
    assert!(results.iter().all(|result| result["is_error"] == true));
}

#[test]
fn one_long_native_call_can_run_multiple_runtime_tools() {
    let root = scratch_dir("native-long-tools");
    let rollout = root.join("rollout.jsonl");
    let padding = "// retained source\n".repeat(1200);
    let code = format!(
        "{padding}await write({{path:'made.txt',content:'native'}}); const f = await read({{path:'made.txt'}}); console.log(f.text);"
    );
    let (base, bodies) = start_fake_provider(vec![
        native_cell_reply("long-tools", &code),
        native_cell_reply("finish", "answer('done');"),
    ]);
    let output = run_session(&root, &rollout, "native-long-tools", "do it", &base);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(fs::read_to_string(root.join("made.txt")).unwrap(), "native");
    let cells = cell_lines(&rollout);
    assert_eq!(cells[0]["source"].as_str().unwrap(), code);
    assert_eq!(cells[0]["calls"].as_array().unwrap().len(), 2);
    let second: serde_json::Value = serde_json::from_str(&bodies.lock().unwrap()[1]).unwrap();
    assert!(second.to_string().contains("native"));
}

#[test]
fn malformed_native_input_and_runtime_throw_are_correlated_errors() {
    let root = scratch_dir("native-errors");
    let rollout = root.join("rollout.jsonl");
    let malformed = serde_json::json!({"role":"assistant","content":[
        {"type":"tool_use","id":"bad-input","name":"execute_cell","input":{"code":7}}
    ]})
    .to_string();
    let (base, bodies) = start_fake_provider(vec![
        malformed,
        native_cell_reply("throws", "throw new Error('boom')"),
        native_cell_reply("finish", "answer('done');"),
    ]);
    let output = run_session(&root, &rollout, "native-errors", "do it", &base);
    assert!(output.status.success());
    let bodies = bodies.lock().unwrap();
    for (index, id) in [(1, "bad-input"), (2, "throws")] {
        let body: serde_json::Value = serde_json::from_str(&bodies[index]).unwrap();
        let result = body["messages"]
            .as_array()
            .unwrap()
            .iter()
            .flat_map(|message| message["content"].as_array().into_iter().flatten())
            .find(|block| block["type"] == "tool_result" && block["tool_use_id"] == id)
            .unwrap();
        assert_eq!(result["is_error"], true);
    }
    assert_eq!(
        cell_lines(&rollout).len(),
        2,
        "invalid input must not run a cell"
    );
}

/// The same reply, plus a Messages `usage` object -- the shape a direct
/// provider always sends and the gateway tests never need, so this stays a
/// separate builder rather than a change to [`assistant_reply`] that every
/// other fixture in this file would inherit.
#[cfg(unix)] // its only callers are the two unix-gated usage tests; dead on Windows otherwise
fn assistant_reply_with_usage(text: &str, input_tokens: u64, output_tokens: u64) -> String {
    serde_json::json!({
        "role": "assistant",
        "content": [{"type": "text", "text": text}],
        "usage": {"input_tokens": input_tokens, "output_tokens": output_tokens},
    })
    .to_string()
}

fn run_session(
    root: &Path,
    rollout: &Path,
    session_id: &str,
    task: &str,
    base_url: &str,
) -> std::process::Output {
    run_session_with_gateway(root, rollout, session_id, task, base_url, None)
}

/// [`run_session`], plus the `inference-gateway` binary the entitlement,
/// subscription and routing-cost controls shell out to.
///
/// `base_url` is set to a loopback host, so the session is **hosted**: it
/// attaches rather than starting a gateway of its own, and its usage readout
/// goes to `glasshouse`, scoped to the project — which is why the usage-row
/// tests pass their fake as `--glasshouse`. The standalone case -- an unset
/// `ANTHROPIC_BASE_URL` and a gateway sterna starts itself -- is
/// [`a_session_runs_standalone_against_a_gateway_it_started`].
fn run_session_with_gateway(
    root: &Path,
    rollout: &Path,
    session_id: &str,
    task: &str,
    base_url: &str,
    gateway: Option<&Path>,
) -> std::process::Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_sterna"));
    command
        .arg("session")
        .arg("--root")
        .arg(root)
        .arg("--rollout")
        .arg(rollout)
        .arg("--session")
        .arg(session_id)
        .arg("--task")
        .arg(task)
        .env("ANTHROPIC_BASE_URL", base_url)
        .env("XDG_CONFIG_HOME", root.join("global-config"))
        .env_remove("ANTHROPIC_AUTH_TOKEN")
        .env_remove("ANTHROPIC_API_KEY");
    supply_test_model(&mut command, root);
    if let Some(gateway) = gateway {
        command.arg("--gateway").arg(gateway);
        // `--gateway` is ignored by a *hosted* session, which resolves the
        // binary its account controls run through `INFERENCE_GATEWAY_BIN`
        // and then `PATH`. A test that named a gateway means that one, not
        // whichever gateway the machine running the test has installed.
        command.env("INFERENCE_GATEWAY_BIN", gateway);
    }
    command.output().unwrap()
}

/// The reply that ends a task: a cell that calls `answer`.
///
/// **Every test whose own scripted reply is prose needs one.** A prose reply
/// is *answered*, not obeyed (`model-contract.md` §5): sterna sends back the
/// unchanged handle table and one line, and the task runs on until something
/// ends it. Before the session loop existed a turn was the whole run, and
/// these fixtures scripted one reply because one reply was all a run could
/// consume.
///
/// `answer(text)` is the whole of it: a cell that merely returns a value --
/// of any type, a string included -- is notebook output and buys another
/// turn, so a fixture that returns cannot end anything.
fn ending_reply() -> String {
    assistant_reply("```sterna\nanswer(\"done\");\n```")
}

/// [`ending_reply`], with a `usage` object attached.
#[cfg(unix)] // its only callers are the two unix-gated usage tests; dead on Windows otherwise
fn ending_reply_with_usage(input_tokens: u64, output_tokens: u64) -> String {
    assistant_reply_with_usage(
        "```sterna\nanswer(\"done\");\n```",
        input_tokens,
        output_tokens,
    )
}

/// The text of the last `user` message in a recorded request body -- what the
/// runtime told the model on the turn that request opened.
fn last_user_text(body: &str) -> String {
    let request: serde_json::Value = serde_json::from_str(body).unwrap();
    let messages = request["messages"].as_array().unwrap();
    messages
        .iter()
        .rev()
        .find(|message| message["role"] == "user")
        .expect("every request carries at least the task")["content"][0]["text"]
        .as_str()
        .unwrap()
        .to_string()
}

/// Every `cell` line in the rollout, in file order.
fn cell_lines(path: &Path) -> Vec<serde_json::Value> {
    rollout_lines(path)
        .into_iter()
        .filter(|line| line["kind"] == "cell")
        .collect()
}

fn rollout_lines(path: &Path) -> Vec<serde_json::Value> {
    fs::read_to_string(path)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

#[test]
fn the_binary_runs_a_turn_and_writes_a_rollout() {
    let root = scratch_dir("turn-root");
    let rollout = root.join("rollout.jsonl");
    let (base_url, bodies) =
        start_fake_provider(vec![assistant_reply("hi from the model"), ending_reply()]);

    let output = run_session(&root, &rollout, "sess-turn", "hello there", &base_url);

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(rollout.exists(), "the rollout file must be written");

    let lines = rollout_lines(&rollout);
    assert!(
        lines
            .iter()
            .any(|l| l["kind"] == "turn" && l["role"] == "user" && l["text"] == "hello there"),
        "no user turn in {lines:?}"
    );
    assert!(
        lines.iter().any(|l| l["kind"] == "turn"
            && l["role"] == "assistant"
            && l["text"] == "hi from the model"),
        "no assistant turn in {lines:?}"
    );

    // An ordinary answer ends naturally; it is not sent back for code conversion.
    assert_eq!(bodies.lock().unwrap().len(), 1);
}

#[test]
fn the_binary_resumes_an_existing_rollout_instead_of_starting_over() {
    let root = scratch_dir("resume-root");
    let rollout = root.join("rollout.jsonl");

    let (first_url, _first_bodies) =
        start_fake_provider(vec![assistant_reply("first reply"), ending_reply()]);
    let first = run_session(&root, &rollout, "sess-resume", "first message", &first_url);
    assert!(
        first.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&first.stderr)
    );

    let (second_url, second_bodies) =
        start_fake_provider(vec![assistant_reply("second reply"), ending_reply()]);
    let second = run_session(
        &root,
        &rollout,
        "sess-resume",
        "second message",
        &second_url,
    );
    assert!(
        second.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&second.stderr)
    );

    let bodies = second_bodies.lock().unwrap();
    assert_eq!(bodies.len(), 1);
    let request: serde_json::Value = serde_json::from_str(&bodies[0]).unwrap();
    let messages = request["messages"].as_array().unwrap();
    let texts: Vec<&str> = messages
        .iter()
        .map(|m| m["content"][0]["text"].as_str().unwrap())
        .collect();

    assert!(
        texts.contains(&"first message"),
        "second run's request must carry the first run's user turn: {texts:?}"
    );
    assert!(
        texts.contains(&"first reply"),
        "second run's request must carry the first run's assistant turn: {texts:?}"
    );
    assert!(
        texts.contains(&"second message"),
        "second run's request must also carry its own new turn: {texts:?}"
    );
}

#[test]
fn the_binary_loads_the_projects_own_instructions() {
    let root = scratch_dir("instructions-root");
    fs::write(
        root.join("CLAUDE.md"),
        "STERNA-SESSION-TEST-MARKER-loads-its-own-claude-md",
    )
    .unwrap();
    let rollout = root.join("rollout.jsonl");
    let (base_url, bodies) = start_fake_provider(vec![assistant_reply("ack"), ending_reply()]);

    let output = run_session(&root, &rollout, "sess-instructions", "hi", &base_url);
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let bodies = bodies.lock().unwrap();
    let request: serde_json::Value = serde_json::from_str(&bodies[0]).unwrap();
    let system = request["system"][0]["text"].as_str().unwrap();
    assert!(
        system.contains("STERNA-SESSION-TEST-MARKER-loads-its-own-claude-md"),
        "system prompt did not carry CLAUDE.md's content: {system}"
    );
}

#[test]
fn a_slash_command_is_answered_without_a_request() {
    let root = scratch_dir("slash-root");
    let rollout = root.join("rollout.jsonl");
    let base_url = refused_base_url();

    let output = run_session(&root, &rollout, "sess-slash", "/model", &base_url);

    assert!(
        output.status.success(),
        "a slash command must not fail even though the configured base URL refuses every connection: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    if rollout.exists() {
        let lines = rollout_lines(&rollout);
        assert!(
            lines.iter().all(|l| l["kind"] != "turn"),
            "a slash command must not be recorded as a turn: {lines:?}"
        );
    }
}

/// The default sandbox level, and what a session with nobody at the keyboard
/// does with an asking one.
///
/// A scripted run is the case this must not break: `sandboxed` is the
/// default, so every existing scripted invocation keeps working — the startup
/// line names the level, and says plainly that nobody is watching, so a log
/// read afterwards cannot be mistaken for a session where somebody answered.
#[test]
fn a_session_with_no_flag_starts_sandboxed_and_says_nobody_is_watching() {
    let root = scratch_dir("permissions-default");
    let rollout = root.join("rollout.jsonl");
    let base_url = refused_base_url();

    let output = run_session(&root, &rollout, "sess-rung", "/handles", &base_url);

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("sandbox: sandboxed — "),
        "sandboxed is the default level: {stdout}"
    );
    assert!(
        stdout.contains("nobody is watching, so leaving the sandbox is refused"),
        "a scripted session says so rather than pretending someone answered: {stdout}"
    );
    assert!(
        !stdout.contains("permissions: "),
        "the retired rung line is gone: {stdout}"
    );
}

/// `ask` confirms ordinary work, so it refuses to start where nobody can
/// answer, rather than stalling on its first call for ten minutes.
#[test]
fn sandbox_ask_refuses_a_scripted_session() {
    let root = scratch_dir("permissions-scripted");
    let rollout = root.join("rollout.jsonl");
    let base_url = refused_base_url();

    let output = std::process::Command::new(env!("CARGO_BIN_EXE_sterna"))
        .args([
            "session",
            "--root",
            root.to_str().unwrap(),
            "--rollout",
            rollout.to_str().unwrap(),
            "--session",
            "sess-manual",
            "--task",
            "/handles",
            "--sandbox",
            "ask",
        ])
        .env("ANTHROPIC_BASE_URL", &base_url)
        .output()
        .expect("sterna runs");

    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        combined.contains(
            "--sandbox ask requires an interactive terminal session; scripted calls cannot approve themselves"
        ),
        "ask cannot function unattended and says so: {combined}"
    );
    assert!(!output.status.success(), "{combined}");
}

#[test]
fn handles_command_reports_the_recorded_preview() {
    let root = scratch_dir("unbuilt-root");
    let rollout = root.join("rollout.jsonl");
    let base_url = refused_base_url();

    let output = run_session(&root, &rollout, "sess-unbuilt", "/handles", &base_url);

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("No handles recorded yet"),
        "the command must report available recorded handles: {stdout}"
    );
}

#[test]
fn the_binary_with_no_arguments_starts_a_session_in_its_current_directory() {
    let root = scratch_dir("bare-entrypoint-root");
    fs::create_dir_all(root.join(".sterna")).unwrap();
    fs::write(
        root.join(".sterna/config.toml"),
        format!("[model]\nparent = {:?}\n", sterna::wire::MODEL),
    )
    .unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_sterna"))
        .current_dir(&root)
        .env("ANTHROPIC_BASE_URL", refused_base_url())
        .env("XDG_CONFIG_HOME", root.join("global-config"))
        .env_remove("ANTHROPIC_AUTH_TOKEN")
        .env_remove("ANTHROPIC_API_KEY")
        // A developer install must not receive this fixture's lifecycle
        // events. Missing Glasshouse is the session seam's normal fail-soft
        // path.
        .env("PATH", "")
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    // Per-session rollouts: the id is generated, so the folder is found
    // rather than the file named. That a bare `sterna` uses cwd as its root
    // is still the point; and EOF invents no turn, so nobody asked anything
    // and the session is not kept.
    let kept: Vec<_> = fs::read_dir(root.join(".sterna/sessions"))
        .expect("bare sterna did not use cwd as --root .")
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.extension()
                .is_some_and(|extension| extension == "jsonl")
        })
        .collect();
    assert!(kept.is_empty(), "EOF must not invent a turn: {kept:?}");
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn nothing_the_model_returns_is_executed() {
    let root = scratch_dir("no-execute-root");
    let rollout = root.join("rollout.jsonl");
    let sentinel = root.join("sentinel-should-not-exist");
    let malicious = format!("```sh\ntouch {}\n```", sentinel.display());
    let (base_url, _bodies) =
        start_fake_provider(vec![assistant_reply(&malicious), ending_reply()]);

    let output = run_session(&root, &rollout, "sess-no-execute", "please help", &base_url);

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        !sentinel.exists(),
        "the model's text must never be executed: {} was created",
        sentinel.display()
    );
}

/// The other half of 2446, and the replacement for
/// `the_binary_falls_back_to_the_local_store_when_glasshouse_is_absent`,
/// whose assertion (`stdout.contains("/memory")`) was satisfied by
/// `"/memory: no notes"` -- the string reporting that the fallback found
/// nothing. This writes a note through one invocation's `/memory <text>` and
/// asserts a second invocation, with Glasshouse still absent both times,
/// reads that exact note back: a message saying nothing was found cannot
/// satisfy this, only the note's own text can.
#[test]
fn a_note_written_through_the_binary_is_read_back_by_a_later_run() {
    let root = scratch_dir("memory-roundtrip-root");
    let rollout = root.join("rollout.jsonl");

    let write = run_session(
        &root,
        &rollout,
        "memory-write",
        "/memory STERNA-WROTE-THIS-NOTE",
        &refused_base_url(),
    );
    assert!(
        write.status.success(),
        "an absent glasshouse must not fail the session; stderr:\n{}",
        String::from_utf8_lossy(&write.stderr)
    );

    let read = run_session(
        &root,
        &rollout,
        "memory-read",
        "/memory",
        &refused_base_url(),
    );
    assert!(
        read.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&read.stderr)
    );
    let stdout = String::from_utf8_lossy(&read.stdout);
    assert!(
        stdout.contains("STERNA-WROTE-THIS-NOTE"),
        "a note written through the binary was not read back by a later run:\n{stdout}"
    );
}

/// 2449: the shipped binary must show something, not build a `TestBackend`
/// and drop it. `run_session` always captures stdout as a pipe, which is
/// exactly the non-tty path every real pipe takes, so a regression back to
/// the dropped `TestBackend` fails this the same way it would fail a user
/// piping the binary's output anywhere.
#[test]
fn a_piped_session_prints_the_models_reply() {
    let root = scratch_dir("print-reply-root");
    let rollout = root.join("rollout.jsonl");
    let (base_url, _bodies) = start_fake_provider(vec![
        assistant_reply("STERNA-PRINTED-REPLY-MARKER"),
        ending_reply(),
    ]);

    let output = run_session(&root, &rollout, "sess-print-reply", "hello", &base_url);

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        !stdout.is_empty(),
        "a piped session must print something to stdout"
    );
    assert!(
        stdout.contains("STERNA-PRINTED-REPLY-MARKER"),
        "the assistant's reply never reached stdout:\n{stdout}"
    );
}

/// 2449's other clause: the sidebar's content reaches stdout too, including
/// its honest collapse when Glasshouse is absent. Before this package,
/// stdout was zero bytes and this assertion could not have passed on any
/// string; it is not satisfied by an empty capture, only by the sidebar's
/// own collapsed text actually being printed.
#[test]
fn a_piped_session_prints_the_sidebar_content() {
    let root = scratch_dir("print-sidebar-root");
    let rollout = root.join("rollout.jsonl");
    let (base_url, _bodies) = start_fake_provider(vec![assistant_reply("ack"), ending_reply()]);

    let output = run_session(&root, &rollout, "sess-print-sidebar", "hello", &base_url);

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("gateway not connected"),
        "the sidebar's collapsed content never reached stdout:\n{stdout}"
    );
}

/// 2450: `commands::all` decided the full list from the day it was written;
/// this is the first test asserting the binary actually offers it, with a
/// project command and a project skill both present so neither source is
/// standing in for the other.
#[test]
fn the_command_list_is_offered_by_the_binary() {
    let root = scratch_dir("command-list-root");
    fs::create_dir_all(root.join(".claude").join("commands")).unwrap();
    fs::write(
        root.join(".claude").join("commands").join("deploy.md"),
        "deploy the project",
    )
    .unwrap();
    fs::create_dir_all(root.join(".claude").join("skills").join("reviewer")).unwrap();

    let rollout = root.join("rollout.jsonl");

    let output = run_session(
        &root,
        &rollout,
        "sess-command-list",
        "/help",
        &refused_base_url(),
    );

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("deploy"),
        "the project command never appeared in the offered list:\n{stdout}"
    );
    assert!(
        stdout.contains("reviewer"),
        "the project skill never appeared in the offered list:\n{stdout}"
    );
}

#[test]
fn a_project_command_runs_through_the_normal_task_prompt() {
    let root = scratch_dir("project-command-run");
    fs::create_dir_all(root.join(".claude").join("commands")).unwrap();
    fs::write(
        root.join(".claude").join("commands").join("deploy.md"),
        "Inspect the release manifest before deploying.",
    )
    .unwrap();
    let rollout = root.join("rollout.jsonl");
    let (base_url, bodies) = start_fake_provider(vec![ending_reply()]);

    let output = run_session(
        &root,
        &rollout,
        "project-command-run",
        "/deploy staging",
        &base_url,
    );

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let requests = bodies.lock().unwrap();
    assert_eq!(requests.len(), 1);
    let task = last_user_text(&requests[0]);
    assert!(task.contains("Project command /deploy:"), "{task}");
    assert!(
        task.contains("Inspect the release manifest before deploying."),
        "{task}"
    );
    assert!(
        task.contains("Arguments supplied by the user:\nstaging"),
        "{task}"
    );
}

/// 2450's uncovered branch: `commands::resolve`'s `ProjectSkill` arm, which
/// no test in the crate exercised even though it is the branch the binary
/// itself uses for a bare `/<skill-name>`.
#[test]
fn a_project_skill_resolves_by_name() {
    let root = scratch_dir("skill-resolve-root");
    fs::create_dir_all(root.join(".claude").join("skills").join("reviewer")).unwrap();

    let rollout = root.join("rollout.jsonl");

    let output = run_session(
        &root,
        &rollout,
        "sess-skill-resolve",
        "/reviewer",
        &refused_base_url(),
    );

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("informational project skill")
            && stdout.contains("does not execute this entry"),
        "resolving a bare project skill by name never reached the binary's ProjectSkill branch:\n{stdout}"
    );
}

#[test]
fn supervisor_and_permissions_report_effective_session_state() {
    let root = scratch_dir("control-state");
    fs::create_dir_all(root.join(".sterna")).unwrap();
    fs::write(
        root.join(".sterna/config.toml"),
        "[permissions]\nallow = [\"Bash(cargo *)\"]\n",
    )
    .unwrap();
    let rollout = root.join("rollout.jsonl");

    let supervisor = run_session(
        &root,
        &rollout,
        "control-supervisor",
        "/supervisor",
        &refused_base_url(),
    );
    assert!(supervisor.status.success());
    let stdout = String::from_utf8_lossy(&supervisor.stdout);
    assert!(stdout.contains("Supervisor"), "{stdout}");
    assert!(stdout.contains("State: off"), "{stdout}");
    assert!(stdout.contains("Cadence: every 4 cells"), "{stdout}");

    let permissions = run_session(
        &root,
        &root.join("permissions-rollout.jsonl"),
        "control-permissions",
        "/permissions",
        &refused_base_url(),
    );
    assert!(permissions.status.success());
    let stdout = String::from_utf8_lossy(&permissions.stdout);
    assert!(
        stdout.contains("Effective current session (immutable)"),
        "{stdout}"
    );
    assert!(
        stdout.contains("Persisted next-session settings"),
        "{stdout}"
    );
    assert!(stdout.contains("Bash(cargo *)"), "{stdout}");
}

#[test]
fn rollback_previews_the_exact_checkpoint_and_headless_confirmation_refuses() {
    let root = scratch_dir("rollback-control");
    let rollout = root.join("rollout.jsonl");
    let (base_url, _bodies) = start_fake_provider(vec![
        native_cell_reply(
            "make-file",
            "await write({path: 'created-by-cell.txt', content: 'cell'});",
        ),
        native_cell_reply("finish", "answer('done');"),
    ]);

    let output = run_session_stdin(
        &root,
        &rollout,
        "rollback-control",
        &["make a file", "/rollback", "/rollback confirm"],
        &base_url,
    );

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("remove created-by-cell.txt"), "{stdout}");
    assert!(
        stdout.contains("refused outside an interactive TUI"),
        "{stdout}"
    );
    assert_eq!(
        fs::read_to_string(root.join("created-by-cell.txt")).unwrap(),
        "cell"
    );
}

/// **2462, and the package's whole point: the model acts by returning a
/// TypeScript program that calls tools by name on live objects.**
///
/// Two cells, `model-contract.md` §7's own worked turn with its paths adapted
/// to a fixture tree: cell 1 greps and reads, cell 2 computes over the array
/// the grep produced and returns. Nothing between them is a person -- the
/// binary sends the second turn itself.
///
/// The provider answers from the **request body** rather than from a list, so
/// cell 2 is only sent because the runtime's own result block reached the
/// model naming `hits`. A fixed list would pass with an empty result block.
///
/// **2465 is the marker assertion.** `harness.rs` line 5 is never in a
/// message: `adapter` is a live `File` handle whose preview is a path, a size
/// and its first two lines, and there is no code path that writes a payload
/// into the conversation.
///
/// Unix only, and the reason is the runtime's, not this file's: on Windows a
/// tool call refuses before spawning, so cell 1 would throw `PermissionDenied`
/// and `hits` would never bind. That refusal is correct and is the runtime's
/// own to test; what this test needs is a host where a program's tool call
/// actually runs.
#[cfg(unix)]
#[test]
fn a_scripted_two_cell_task_runs_through_the_binary_and_returns() {
    let root = scratch_dir("two-cell-root");
    fs::create_dir_all(root.join("src")).unwrap();
    fs::create_dir_all(root.join("tests")).unwrap();
    fs::write(
        root.join("src").join("lib.rs"),
        "use crate::IntegrationId;\npub struct IntegrationId;\n",
    )
    .unwrap();
    fs::write(
        root.join("src").join("harness.rs"),
        "// IntegrationId lives here\n// two\n// three\n// four\n// PAYLOAD-MARKER-NEVER-IN-A-MESSAGE\n",
    )
    .unwrap();
    fs::write(
        root.join("tests").join("it.rs"),
        "use sterna::IntegrationId;\n",
    )
    .unwrap();

    // Outside the fixture tree on purpose: a rollout inside it would be one
    // more file the model's own `grep` reads, and the counts it returns would
    // then depend on the session's own record of asking for them.
    let rollout = scratch_dir("two-cell-rollout").join("rollout.jsonl");

    let cell_one = format!(
        "```sterna\nconst hits = await grep({{ pattern: \"IntegrationId\", path: \"{root}\" }});\nconst adapter = await read({{ path: \"{root}/src/harness.rs\" }});\n```",
        root = root.display()
    );
    let cell_two = "```sterna\nconst isTest = (m) => m.path.includes(\"/tests/\");\nconst inTests = hits.filter(isTest);\nconst prodFiles = new Set(hits.filter(m => !isTest(m)).map(m => m.path));\nreturn { total: hits.length, in_tests: inTests.length, prod_files: prodFiles.size };\n```";

    let (base_url, bodies) = start_answering_provider(3, move |body| {
        if body.contains("## Output") {
            assistant_reply("Two files match; one is a test and one is production.")
        } else if body.contains("## Handles") && body.contains("hits") {
            assistant_reply(cell_two)
        } else {
            assistant_reply(&cell_one)
        }
    });

    let output = run_session(
        &root,
        &rollout,
        "sess-two-cell",
        "How many files name that type, and how many are tests?",
        &base_url,
    );
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let cells = cell_lines(&rollout);
    assert_eq!(cells.len(), 2, "two cells ran: {cells:?}");
    assert_eq!(cells[0]["cell"], 1);
    assert_eq!(cells[0]["outcome"], "yielded");
    assert_eq!(cells[1]["cell"], 2);
    assert_eq!(cells[1]["outcome"], "returned");

    let hits = cells[0]["handles"]
        .as_array()
        .unwrap()
        .iter()
        .find(|handle| handle["name"] == "hits")
        .unwrap_or_else(|| panic!("the grep result never became a handle: {cells:?}"));
    assert_eq!(hits["provenance"]["tool"], "grep");
    let preview = hits["preview"].as_str().unwrap();
    assert!(
        preview.contains("n=4"),
        "the four matches must be countable through the handle: {preview}"
    );

    let bodies = bodies.lock().unwrap();
    assert_eq!(
        bodies.len(),
        3,
        "structured output bought a final prose turn"
    );
    let result_block = last_user_text(&bodies[1]);
    assert!(
        result_block.starts_with("[cell 1 yielded in"),
        "the second turn opened with the first cell's result: {result_block}"
    );
    assert!(
        !bodies[1].contains("PAYLOAD-MARKER-NEVER-IN-A-MESSAGE"),
        "a handle's payload reached the conversation: {}",
        bodies[1]
    );
    assert!(last_user_text(&bodies[2]).contains("## Output"));

    let stdout = String::from_utf8_lossy(&output.stdout);
    for key in ["\"total\"", "\"in_tests\"", "\"prod_files\""] {
        assert!(
            stdout.contains(key),
            "the returned value's preview must name {key}:\n{stdout}"
        );
    }
}

/// §5: a message with no `sterna` block is prose. The task does not advance,
/// **the cell counter does not move**, and the answer is the unchanged handle
/// table and one line.
///
/// The prose here contains a ```` ```ts ```` block, which is the case §5 names
/// outright: a model writing about TypeScript emits those constantly, and a
/// parser that ran them would run the model's explanations.
#[test]
fn a_prose_answer_with_example_code_ends_without_running_a_cell() {
    let root = scratch_dir("prose-root");
    let rollout = root.join("rollout.jsonl");

    let (base_url, bodies) = start_fake_provider(vec![
        assistant_reply("Here is how I would do it:\n\n```ts\nconst x = 1;\n```\n\nShall I?"),
        ending_reply(),
    ]);

    let output = run_session(&root, &rollout, "sess-prose", "count them", &base_url);
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    assert!(
        cell_lines(&rollout).is_empty(),
        "explanatory code must never execute"
    );
    let bodies = bodies.lock().unwrap();
    assert_eq!(bodies.len(), 1, "a prose answer needs no repair request");
    let saved = std::fs::read_to_string(&rollout).unwrap();
    assert!(!saved.contains("no program ran; send one sterna block"));
    assert!(saved.contains("Shall I?"));
}

/// Blocks are one program, with one feedback exchange and cross-block bindings.
#[test]
fn ordered_sterna_blocks_run_in_one_cell() {
    let root = scratch_dir("two-blocks-root");
    let rollout = root.join("rollout.jsonl");
    let (base_url, bodies) = start_fake_provider(vec![
        assistant_reply("```sterna\nconst a = 1;\n```\n\n```sterna\nconst b = a + 2;\n```"),
        assistant_reply("```sterna\nanswer(`${b}`);\n```"),
    ]);
    let output = run_session(&root, &rollout, "ordered", "do the thing", &base_url);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let cells = cell_lines(&rollout);
    assert_eq!(cells.len(), 2);
    assert!(
        cells[0]["source"]
            .as_str()
            .unwrap()
            .contains("const b = a + 2;")
    );
    assert_eq!(bodies.lock().unwrap().len(), 2);
    assert!(String::from_utf8_lossy(&output.stdout).contains(" 3"));
}

/// §5: a throw is a result. It fills the turn slot a yield would have used,
/// carries the class, the message and the position inside the model's own
/// program, and **the turn is not retried** -- the session sends the next one
/// and the task keeps going.
#[test]
fn a_cell_that_throws_is_answered_and_the_session_continues() {
    let root = scratch_dir("throw-root");
    let rollout = root.join("rollout.jsonl");

    let (base_url, bodies) = start_fake_provider(vec![
        assistant_reply(
            "```sterna\nconst before = 1;\nthrow new ReferenceError(\"fixture\");\n```",
        ),
        ending_reply(),
    ]);

    let output = run_session(&root, &rollout, "sess-throw", "do the thing", &base_url);
    assert!(
        output.status.success(),
        "a throw must not fail the session; stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let cells = cell_lines(&rollout);
    assert_eq!(cells.len(), 2, "{cells:?}");
    assert_eq!(cells[0]["outcome"], "threw");
    // How the *program* ended, which is no longer how the *task* ended: the
    // second cell answered and then ran off its end, so it yielded.
    assert_eq!(cells[1]["outcome"], "yielded");

    let bodies = bodies.lock().unwrap();
    assert_eq!(bodies.len(), 2, "the session continued after the throw");
    let answer = last_user_text(&bodies[1]);
    assert!(
        answer.starts_with("[cell 1 threw in"),
        "the throw fills the turn slot a yield would have used: {answer}"
    );
    assert!(
        answer.contains("## Error\nReferenceError:"),
        "the error section carries the class and the message: {answer}"
    );
    assert!(
        answer.contains("before"),
        "the binding made before the throw must still be in the table: {answer}"
    );
    assert_eq!(
        bodies[1].matches("throw new ReferenceError").count(),
        1,
        "the turn is never retried: the throwing program appears once, as the \
         assistant message that sent it"
    );

    // §9.1: a cell that threw did not return, so no assistant `turn` line
    // follows its cell line -- the runtime's own answer does.
    let lines = rollout_lines(&rollout);
    let threw_at = lines
        .iter()
        .position(|line| line["kind"] == "cell" && line["outcome"] == "threw")
        .unwrap();
    let feedback = lines[threw_at + 1..]
        .iter()
        .find(|line| line["kind"] == "turn")
        .expect("a throw must be followed by runtime feedback");
    assert_eq!(feedback["role"], "user", "{lines:?}");
}

/// The system block the binary builds for a session in `root` that reached no
/// gateway: `render_system_reaching`'s bytes, with the session's own subagent
/// roster — empty of models, because nothing served any, and `Off` because
/// that is what `[agents]` defaults to now: delegation is never inherited,
/// it is chosen (`docs/workbench.md`, *Delegation policy*).
///
/// One spelling for both byte-equality tests, and it renders through the same
/// function the binary does: a second hand-built string here would be the
/// drift those tests exist to catch.
fn expected_system_block(root: &std::path::Path) -> String {
    let mut profile = sterna::sandbox::profile::Profile::compile(root, None);
    // The binary starts the proxy wherever a command can reach it, and the
    // prompt's network fact follows; the route's own values are not shown.
    if sterna::sandbox::proxy::reachable() {
        profile = profile.with_proxy(sterna::sandbox::profile::ProxyRoute {
            port: 0,
            unix: None,
            env: Vec::new(),
        });
    }
    let config = sterna::config::SternaConfig::default();
    let manifest = sterna::session::system_manifest(&profile, &config);
    let agents = sterna::prompt::declarations::AgentRoster {
        posture: sterna::prompt::declarations::AgentsPosture::Off,
        models: Vec::new(),
    };
    sterna::prompt::render_system_reaching(
        &sterna::project::instructions::root(&profile),
        &sterna::tools::registry::ALL.iter().collect::<Vec<_>>(),
        &sterna::session::session_facts_with(
            &profile,
            sterna::abi::Interface::default(),
            &manifest,
        ),
        sterna::runtime::bindings::HostGlobals::Every,
        sterna::prompt::Reach {
            web: None,
            agents: Some(&agents),
            // Derived from the same config the binary reads, not spelled a
            // second time: a default session has helpers enabled with no
            // model, so its roster carries its own refusal.
            helpers: Some(config.helpers.model.is_some() && config.helpers.enabled),
            decisions: false,
        },
    )
}

/// REQUIRED BEHAVIOR 6, and `model-contract.md` §1: the system block the
/// binary sends is `prompt::render_system`'s bytes for the same inputs -- the
/// preamble, one declaration per registered tool, then the project's own
/// instructions.
///
/// **Byte equality, not `contains`.** The system block is what the provider's
/// prompt cache holds for the whole task; a second spelling of it here that
/// merely carried the same words would break the cache and would make §8's
/// gateway comparison a comparison of two prompts.
#[test]
fn the_system_block_is_render_systems_own_bytes() {
    let root = scratch_dir("system-bytes-root");
    fs::write(root.join("CLAUDE.md"), "PROJECT-INSTRUCTION-ONE").unwrap();
    let rollout = root.join("rollout.jsonl");
    let absent = root.join("no-such-glasshouse");
    let (base_url, bodies) = start_fake_provider(vec![ending_reply()]);

    // The gateway is absent as well as Glasshouse: the system block's subagent
    // roster is whatever gateway serves, and a byte-equality test must not
    // depend on which models the machine running it happens to have.
    let output = run_session_with_gateway(
        &root,
        &rollout,
        "sess-system-bytes",
        "hi",
        &base_url,
        Some(&absent),
    );
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    // The facts are built from a profile compiled exactly as the binary
    // compiles it, through `session_facts` itself: a second spelling here
    // would be the very drift this test exists to catch.
    let expected = expected_system_block(&root);

    // Read the task-start snapshot from its audit row: time is sampled by
    // the subprocess, not regenerated by this test after the task finishes.
    let saved = sterna::rollout::resume(&rollout).unwrap().system;
    let (prefix, orientation) = saved.split_once("\n\n## Environment orientation").unwrap();
    assert_eq!(prefix, expected);
    assert!(orientation.contains("task-start UTC:"));
    let expected = saved;
    let bodies = bodies.lock().unwrap();
    let request: serde_json::Value = serde_json::from_str(&bodies[0]).unwrap();
    let expected = sterna::prompt::with_task_context(
        &sterna::contract::Conversation {
            system: expected,
            messages: Vec::new(),
        },
        sterna::wire::MODEL,
        "hi",
    );
    assert_eq!(
        request["system"][0]["text"].as_str().unwrap(),
        expected.system
    );
}

/// §6's cell limit, and the one sentence that replaces the preamble when the
/// limit is reached.
///
/// **The loop ends after that turn whatever the model does**, so the cap is
/// asserted by the provider running out of scripted turns: a loop that kept
/// going would open a forty-second connection to a listener that has already
/// exited, and the run would fail rather than succeed.
///
/// The final turn's program still runs. It has to: `exhausted_preamble` says
/// the only permitted action is a top-level `return`, and a return is a
/// program -- refusing to run it would make the sentence unfollowable.
#[test]
fn the_cell_cap_replaces_the_preamble_and_ends_the_task_after_one_more_turn() {
    let root = scratch_dir("cell-cap-root");
    let rollout = root.join("rollout.jsonl");
    // There is no default cap since 2026-09-17; a ceiling exists only when
    // this person sets one, and this test pins that ceiling's mechanics.
    //
    // Twelve, not forty: these scripted cells run `const x = 1;` forever, which
    // changes no file, records no fact and verifies nothing, so the stall
    // ender reaches them at eighteen cells. A ceiling has to be under that to
    // be the thing this test is measuring.
    std::fs::create_dir_all(root.join(".sterna")).unwrap();
    std::fs::write(root.join(".sterna/config.toml"), "[limits]\ncells = 12\n").unwrap();

    // 12 is the configured cap; the thirteenth turn is the final-answer turn.
    let turns = 13;
    let replies = (0..turns)
        .map(|_| assistant_reply("```sterna\nconst x = 1;\n```"))
        .collect();
    let (base_url, bodies) = start_fake_provider(replies);

    let output = run_session(&root, &rollout, "sess-cell-cap", "keep going", &base_url);
    assert!(
        !output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let bodies = bodies.lock().unwrap();
    assert_eq!(
        bodies.len(),
        turns,
        "the task must stop one turn after the cap, not run on"
    );
    assert!(
        !last_user_text(&bodies[turns - 2]).starts_with("The cell limit this project set"),
        "the turn before the cap carries the ordinary result block"
    );
    assert!(
        last_user_text(&bodies[turns - 1])
            .starts_with("The cell limit this project set (12) is reached"),
        "the final-answer turn opens with the one sentence that replaces \
         the preamble: {}",
        last_user_text(&bodies[turns - 1])
    );
}

/// A fake `inference-gateway` whose `routing-cost --json` answers with one
/// observation row, and which is silent for every other subcommand.
/// `once_only` makes it answer the **first** call and nothing after it.
#[cfg(unix)]
fn write_routing_cost(dir: &Path, name: &str, once_only: bool) -> PathBuf {
    let state = dir.join(format!("{name}.seen"));
    let row = r#"{"provider":"anthropic","model":"claude-sonnet-5","quota_context":"pro-plan","input_tokens":100,"output_tokens":20}"#;
    let guard = if once_only {
        format!(
            "[ -f {state} ] && exit 0\ntouch {state}\n",
            state = state.display()
        )
    } else {
        String::new()
    };
    let body = format!(
        "#!/bin/sh\nif [ \"$1\" = \"--scope\" ]; then shift 2; fi\ncase \"$1\" in\n  routing-cost)\n{guard}    echo '{row}'\n    ;;\nesac\nexit 0\n"
    );
    write_script(dir, name, &body)
}

/// §6: the task's token figure is the gateway's own usage row when there is
/// one -- "read from the gateway's own usage row rather than estimated".
///
/// 120 is `100 + 20`, the row's own two figures. The estimate for this
/// conversation is several hundred tokens, so a usage line reading `task
/// spent 120` cannot have been produced by the fallback.
#[cfg(unix)]
#[test]
fn a_gateway_reported_turn_is_counted_from_the_usage_row_not_estimated() {
    let root = scratch_dir("budget-gateway-root");
    let rollout = root.join("rollout.jsonl");
    let gateway = write_routing_cost(&root, "fake_routing_cost.sh", false);

    let (base_url, bodies) = start_fake_provider(vec![
        assistant_reply("```sterna\nconst x = 1;\n```"),
        ending_reply(),
    ]);

    let output = run_session_with_gateway(
        &root,
        &rollout,
        "sess-budget-gateway",
        "count them",
        &base_url,
        Some(&gateway),
    );
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let bodies = bodies.lock().unwrap();
    let result_block = last_user_text(&bodies[1]);
    assert!(
        result_block.contains("· task spent 120 · cells 1"),
        "the usage line must carry the gateway's own figures: {result_block}"
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("spent 240 · reported"),
        "two reported turns total 240 in the sidebar:\n{stdout}"
    );
    assert!(
        stdout.contains("spent 240 · reported"),
        "the sidebar must say the figure was reported, not estimated:\n{stdout}"
    );
}

/// The other half of §6's rule, and the reason the sidebar has a provenance
/// line at all: **a total that mixes a measurement with a heuristic says so.**
///
/// This gateway meters the first turn and not the second, so the total is one
/// reported figure plus one estimate. Labelling that `gateway-reported` would
/// be the honesty failure 2449 forbids -- a number that looks measured and is
/// not -- and labelling it `estimated` would understate a figure that is
/// partly real.
#[cfg(unix)]
#[test]
fn a_turn_the_gateway_never_metered_is_labelled_rather_than_averaged() {
    let root = scratch_dir("budget-mixed-root");
    let rollout = root.join("rollout.jsonl");
    let gateway = write_routing_cost(&root, "fake_routing_cost_once.sh", true);

    let (base_url, _bodies) = start_fake_provider(vec![
        assistant_reply("```sterna\nconst x = 1;\n```"),
        ending_reply(),
    ]);

    let output = run_session_with_gateway(
        &root,
        &rollout,
        "sess-budget-mixed",
        "count them",
        &base_url,
        Some(&gateway),
    );
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("part estimated"),
        "a total built from both sources must say so:\n{stdout}"
    );
}

// --- runtime-contract.md §9: ending a task from inside the program -------

/// §9.2 through the binary: **`answer(text)` ends the task and a returned
/// value never does**, whatever its type.
///
/// The first program returns the very string the second one answers with.
/// The return buys another turn and nothing else -- if it ended the task the
/// provider would have been asked once, and the screen would carry the text
/// anyway, so the request count is what separates the two. After the answer
/// the rollout's last `turn` line is the assistant's, carrying the text
/// verbatim, and the third reply is scripted so that a request sent after
/// the ending would be served and counted rather than fail on the
/// connection.
///
/// A cell that answers and then runs off its end is recorded as a `yielded`
/// program: how the program stopped and whether the task is over are
/// separate facts now, and the returning cell before it is the contrast.
#[test]
fn an_answer_ends_the_task_and_a_returned_string_does_not() {
    let root = scratch_dir("terminal-string-root");
    let rollout = root.join("rollout.jsonl");
    let answer = "Three files name it; two are tests.\nThe third is src/lib.rs.";
    let quoted = serde_json::to_string(answer).unwrap();

    let (base_url, bodies) = start_fake_provider(vec![
        assistant_reply(&format!("```sterna\nreturn {quoted};\n```")),
        assistant_reply(&format!("```sterna\nanswer({quoted});\n```")),
        assistant_reply("```sterna\nanswer(\"NEVER REQUESTED\");\n```"),
    ]);

    let output = run_session(
        &root,
        &rollout,
        "sess-terminal-string",
        "count them",
        &base_url,
    );
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let cells = cell_lines(&rollout);
    assert_eq!(
        cells.len(),
        2,
        "the returned string bought a turn: {cells:?}"
    );
    assert_eq!(
        cells[0]["outcome"], "returned",
        "the first program returned the answer text and did not end the task"
    );
    assert_eq!(cells[1]["outcome"], "yielded", "{cells:?}");

    let lines = rollout_lines(&rollout);
    let terminal = lines
        .iter()
        .rev()
        .find(|line| line["kind"] == "turn")
        .unwrap();
    assert_eq!(terminal["role"], "assistant", "{lines:?}");
    assert_eq!(terminal["text"], answer, "the response is kept verbatim");

    let bodies = bodies.lock().unwrap();
    assert_eq!(
        bodies.len(),
        2,
        "one request for each program, and none after the answer"
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains(" Three files name it; two are tests."),
        "the reply must be on the screen as the assistant's turn:\n{stdout}"
    );
}

/// §9.1: a program whose only call throws, and which then returns a
/// sentence anyway, is answered with the throw. `Threw` never ends the task:
/// no assistant `turn` line follows the throw's cell line, the runtime's
/// answer does, and the session sends the next turn.
#[test]
fn a_throw_never_becomes_a_terminal_response() {
    let root = scratch_dir("throw-terminal-root");
    let rollout = root.join("rollout.jsonl");

    let (base_url, bodies) = start_fake_provider(vec![
        assistant_reply(
            "```sterna\nconst before = 1;\nthrow new ReferenceError(\"fixture\");\nanswer(\"CONFIDENT SENTENCE\");\n```",
        ),
        ending_reply(),
    ]);

    let output = run_session(
        &root,
        &rollout,
        "sess-throw-terminal",
        "do the thing",
        &base_url,
    );
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let lines = rollout_lines(&rollout);
    let threw_at = lines
        .iter()
        .position(|line| line["kind"] == "cell" && line["outcome"] == "threw")
        .unwrap_or_else(|| panic!("the throw's cell line is missing: {lines:?}"));
    let feedback = lines[threw_at + 1..]
        .iter()
        .find(|line| line["kind"] == "turn")
        .expect("a throw must be followed by runtime feedback");
    assert_eq!(
        feedback["role"], "user",
        "the line after a throw is the runtime's answer, never an assistant turn: {lines:?}"
    );
    assert!(
        !lines.iter().any(|line| {
            line["kind"] == "turn"
                && line["role"] == "assistant"
                && line["text"] == "CONFIDENT SENTENCE"
        }),
        "the sentence after a throw became a terminal response: {lines:?}"
    );

    let bodies = bodies.lock().unwrap();
    assert_eq!(bodies.len(), 2, "the session continued after the throw");
    let answer = last_user_text(&bodies[1]);
    assert!(answer.starts_with("[cell 1 threw in"), "{answer}");
    assert!(!answer.contains("CONFIDENT SENTENCE"), "{answer}");
}

/// A structured return is notebook output with values and triggers another
/// inference turn. This is the live Gemini failure: it returned a diagnostic
/// object while gathering evidence and Sterna used to end the whole task.
#[test]
fn a_returned_object_is_output_and_the_task_continues() {
    let root = scratch_dir("terminal-json-root");
    let rollout = root.join("rollout.jsonl");
    let (base_url, bodies) = start_fake_provider(vec![
        assistant_reply(
            "```sterna\nreturn { matches: 3, files: 2, names: [\"a.rs\", \"b.rs\"] };\n```",
        ),
        assistant_reply("The evidence supports two concise recommendations."),
    ]);

    let output = run_session(
        &root,
        &rollout,
        "sess-terminal-json",
        "count them",
        &base_url,
    );
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let bodies = bodies.lock().unwrap();
    assert_eq!(bodies.len(), 2, "structured output must buy another turn");
    let feedback = last_user_text(&bodies[1]);
    assert!(feedback.contains("## Output"), "{feedback}");
    assert!(
        feedback.contains(r#"{"matches":3,"files":2,"names":["a.rs","b.rs"]}"#),
        "{feedback}"
    );
    let lines = rollout_lines(&rollout);
    let final_answer = lines
        .iter()
        .rev()
        .find(|line| line["kind"] == "turn" && line["role"] == "assistant")
        .expect("final assistant turn");
    assert_eq!(
        final_answer["text"],
        "The evidence supports two concise recommendations."
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("\"matches\": 3"),
        "the values, not their types, reach the screen:\n{stdout}"
    );

    // A return over the old 2 KiB cap arrives whole: 3,000 characters of
    // `€` beside a number are under any return budget, so the model reads
    // the value and not a type-only preview of it (ruled 2026-09-23).
    let root = scratch_dir("terminal-json-whole-root");
    let rollout = root.join("rollout.jsonl");
    let (base_url, bodies) = start_fake_provider(vec![
        assistant_reply("```sterna\nreturn { b: \"€\".repeat(3000), n: 1 };\n```"),
        assistant_reply("done"),
    ]);
    let output = run_session(
        &root,
        &rollout,
        "sess-terminal-json-whole",
        "count them",
        &base_url,
    );
    assert!(output.status.success());
    let bodies = bodies.lock().unwrap();
    assert_eq!(bodies.len(), 2);
    let feedback = last_user_text(&bodies[1]);
    let text = feedback
        .split("## Output\n")
        .nth(1)
        .expect("output section")
        .to_string();
    let first = text.lines().next().expect("the field line");
    assert!(first.starts_with("b: "), "{text}");
    assert_eq!(
        first.chars().filter(|c| *c == '€').count(),
        3000,
        "every character reaches the model: {text}"
    );
    assert!(text.contains("\nn: 1\n"), "{text}");
    assert!(!text.contains("cut at"), "{text}");
    assert!(!text.contains("string"), "no type-only preview: {text}");
    assert!(
        feedback.contains("· return budget ") && feedback.contains("· this return "),
        "the usage line names the budget and the cost: {feedback}"
    );
}

/// **Nothing counts cells any more** (the user, 2026-09-17: "Limits are dumb
/// for abstract tasks"). A task whose cells keep producing something runs as
/// long as the work takes, past the 120 that used to be the default ceiling
/// and would have ended this session at cell 120 of 120 — which is what it did
/// to a real four-hour run that morning, mid-implementation.
///
/// The cells here write a different file each time, so they make progress and
/// the stall ender never reaches them: what is being proved is that no *count*
/// of work ends a task, not that a stalled one runs forever.
#[test]
fn no_count_of_cells_ends_a_task_that_keeps_producing_something() {
    let root = scratch_dir("no-cell-cap-root");
    let rollout = root.join("rollout.jsonl");

    // Past the old 120-cell default, then a final answer.
    let cells = 130;
    let mut replies: Vec<String> = (0..cells)
        .map(|n| {
            assistant_reply(&format!(
                "```sterna\nwrite({{path: \"note-{n}.txt\", content: \"{n}\"}});\n```"
            ))
        })
        .collect();
    replies.push(ending_reply());
    let (base_url, bodies) = start_fake_provider(replies);

    let output = run_session(&root, &rollout, "sess-no-cell-cap", "keep going", &base_url);
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let bodies = bodies.lock().unwrap();
    assert_eq!(
        bodies.len(),
        cells + 1,
        "every scripted turn ran: no ceiling ended the task"
    );
    for body in bodies.iter() {
        let text = last_user_text(body);
        assert!(
            !text.starts_with("The cell limit"),
            "no cell limit exists to be reached: {text}"
        );
    }
    // The usage line shows the count with no denominator, because a
    // denominator nobody chose is a fiction.
    let mid = last_user_text(&bodies[cells - 1]);
    assert!(mid.contains("· cells "), "{mid}");
    assert!(
        !mid.contains(&format!("cells {}/", cells - 1)),
        "an uncapped task shows the count alone: {mid}"
    );
}

/// An investigation that only ever reads is work, and the harness has no
/// business ranking it below writing.
///
/// **The defect this pins.** Progress used to mean a tree change, a new
/// capsule fact or a verification result — *wrote a file or ran a test*. A
/// read-only task sets none of the three, so eighteen cells of reading were
/// ended as a stall while working perfectly. Every cell here reads a
/// different file and changes nothing; twenty-four are scripted, well past
/// the old eighteen, so a task ended on the old definition would be served
/// and counted.
#[test]
fn an_investigation_that_only_ever_reads_is_never_ended_as_a_stall() {
    let root = scratch_dir("read-only-investigation");
    let rollout = root.join("rollout.jsonl");
    for i in 0..24 {
        fs::write(root.join(format!("f{i}.txt")), format!("file {i}\n")).unwrap();
    }
    fs::create_dir_all(root.join(".sterna")).unwrap();
    fs::write(
        root.join(".sterna/config.toml"),
        "[permissions]\nallow = [\"Read(**)\"]\n",
    )
    .unwrap();

    let mut replies: Vec<String> = (0..24)
        .map(|i| {
            assistant_reply(&format!(
                "```sterna\nconst seen{i} = await read({{path: \"f{i}.txt\"}});\n```"
            ))
        })
        .collect();
    replies.push(ending_reply());
    let (base_url, bodies) = start_fake_provider(replies);

    let output = run_session(&root, &rollout, "sess-read-only", "look around", &base_url);
    assert!(
        output.status.success(),
        "a task that keeps reading new files must finish on its own terms; \
         stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let bodies = bodies.lock().unwrap();
    assert_eq!(
        bodies.len(),
        25,
        "every scripted read ran, then the model's own ending"
    );
    for (index, body) in bodies.iter().enumerate() {
        let text = last_user_text(body);
        assert!(
            !text.starts_with("Nothing has changed"),
            "turn {index} read a file it had not read: {text}"
        );
        assert!(
            !text.contains("No progress for"),
            "turn {index} was not a repeat: {text}"
        );
    }
}

/// The ender that needs no model: whole windows in which nothing changed.
///
/// These cells run the same no-op forever — no file, no fact, no verification
/// — which is the one thing that must still end a task without a person
/// watching it. Three stall windows of six repeats is the patience; the
/// twentieth turn is the final-answer turn the preamble asks for.
#[test]
fn a_task_that_stops_producing_anything_ends_on_the_stall_with_its_reason() {
    let root = scratch_dir("stall-end-root");
    let rollout = root.join("rollout.jsonl");

    // Far more than the ender needs, so a loop that ran on would be served.
    let replies = (0..40)
        .map(|_| assistant_reply("```sterna\nconst x = 1;\n```"))
        .collect();
    let (base_url, bodies) = start_fake_provider(replies);

    let output = run_session(&root, &rollout, "sess-stall-end", "go nowhere", &base_url);
    assert!(
        !output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let bodies = bodies.lock().unwrap();
    assert_eq!(
        bodies.len(),
        20,
        "the first cell was new, then three windows of six repeats, then one \
         turn to answer"
    );
    let last = last_user_text(&bodies[19]);
    assert!(
        last.starts_with("Nothing has changed for 18 cells across 3 notices"),
        "the ending sentence names what was observed: {last}"
    );
    assert!(
        !last_user_text(&bodies[18]).starts_with("Nothing has changed"),
        "the window before it ends nothing"
    );
}

/// A turn that runs no program is observed by the stall like any other, and
/// nothing counts turns any more (the user, 2026-09-19: everything that makes
/// the harness work against itself goes).
///
/// **A prose turn writes no cell record**, so it used to be invisible to both
/// enders and a count of six was the patch. It is fingerprinted by what it
/// said instead: saying something new is progress, saying the same thing
/// again is not — which is the distinction a count could never draw between a
/// model reasoning toward a hard decision and a model stuck.
///
/// Here every reply is byte-identical, so the first is new and the rest are
/// repeats: three windows of six, the exhausted preamble, then one more turn
/// whatever the model does. A program in between resets the streak.
#[test]
fn repeated_malformed_executable_replies_are_bounded() {
    let prose = || assistant_reply("<php-sterna>read({path: 'roman.py'});</php-sterna>");

    let root = scratch_dir("prose-cap-root");
    let rollout = root.join("rollout.jsonl");
    // Twenty, so a loop that ran on would be served and counted.
    let (base_url, bodies) = start_fake_provider((0..24).map(|_| prose()).collect());
    let output = run_session(&root, &rollout, "sess-prose-cap", "count them", &base_url);
    assert!(
        !output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let bodies = bodies.lock().unwrap();
    assert_eq!(
        bodies.len(),
        20,
        "the first reply was new, then three windows of six repeats, then one \
         turn to answer"
    );
    for (index, body) in bodies.iter().enumerate().take(19) {
        assert!(
            !last_user_text(body).starts_with("Nothing has changed"),
            "prose turn {index} ends nothing: {}",
            last_user_text(body)
        );
    }
    assert!(
        last_user_text(&bodies[19]).starts_with("Nothing has changed for 18"),
        "the eighteenth repeat carries the exhausted preamble: {}",
        last_user_text(&bodies[19])
    );
    drop(bodies);

    let root = scratch_dir("prose-reset-root");
    let rollout = root.join("rollout.jsonl");
    let (base_url, bodies) = start_fake_provider(vec![
        prose(),
        prose(),
        assistant_reply("```sterna\nconst x = 1;\n```"),
        prose(),
        prose(),
        ending_reply(),
    ]);
    let output = run_session(&root, &rollout, "sess-prose-reset", "count them", &base_url);
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let bodies = bodies.lock().unwrap();
    assert_eq!(bodies.len(), 6, "a program resets the streak");
    for body in bodies.iter() {
        assert!(
            !last_user_text(body).starts_with("Nothing has changed"),
            "{}",
            last_user_text(body)
        );
    }
}

/// Addendum 1: the usage line's turn output cap is the `max_tokens` the request
/// actually carries -- one constant, read from the wire -- so the model is
/// told the figure that binds it.
#[test]
fn the_usage_line_names_the_max_tokens_actually_sent() {
    let root = scratch_dir("turn-cap-root");
    let rollout = root.join("rollout.jsonl");
    let (base_url, bodies) = start_fake_provider(vec![
        assistant_reply("```sterna\nconst x = 1;\n```"),
        ending_reply(),
    ]);
    let output = run_session(&root, &rollout, "sess-turn-cap", "count them", &base_url);
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let bodies = bodies.lock().unwrap();
    let request: serde_json::Value = serde_json::from_str(&bodies[1]).unwrap();
    let sent = request["max_tokens"].as_u64().unwrap();
    // **Not a second constant.** Since per-model limits landed, a turn asks
    // for what the gateway publishes as this model's own maximum output, so
    // the figure depends on the shipped index and on which gateway answers —
    // and `wire::MAX_TOKENS` is only the fallback for a model nothing
    // publishes one for. What this test owns is the agreement: the usage
    // line the model reads names the number the request actually carried.
    assert!(
        sent >= u64::from(sterna::wire::MAX_TOKENS),
        "a published maximum below the fallback would make every turn smaller \
         than it used to be: {sent}"
    );
    let result_block = last_user_text(&bodies[1]);
    let named = result_block
        .split("turn output cap ")
        .nth(1)
        .and_then(|rest| rest.split(' ').next())
        .map(|figure| figure.replace(',', ""))
        .and_then(|figure| figure.parse::<u64>().ok())
        .unwrap_or_else(|| panic!("the usage line states a turn output cap: {result_block}"));
    assert_eq!(
        named, sent,
        "the usage line names the figure actually sent: {result_block}"
    );
}

// --- runtime-contract.md §6 addendum: a direct provider's own `usage` -----

/// §6, direct-provider path: with no gateway data at all, a Messages
/// response's own `usage` object is counted as reported, not estimated.
///
/// 30 is `20 + 10`, one reply's own two figures; 60 is both replies'. The
/// estimate for this conversation is a different figure entirely (several
/// hundred tokens, as the gateway test's own comment notes), so a usage
/// line reading `task spent 30` cannot have come from the fallback.
#[cfg(unix)]
#[test]
fn a_direct_providers_usage_is_counted_as_reported_not_estimated() {
    let root = scratch_dir("budget-direct-root");
    let rollout = root.join("rollout.jsonl");

    let (base_url, bodies) = start_fake_provider(vec![
        assistant_reply_with_usage("```sterna\nconst x = 1;\n```", 20, 10),
        ending_reply_with_usage(20, 10),
    ]);

    let output = run_session(
        &root,
        &rollout,
        "sess-budget-direct",
        "count them",
        &base_url,
    );
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let bodies = bodies.lock().unwrap();
    let result_block = last_user_text(&bodies[1]);
    // The cap is the model's own published maximum and belongs to
    // `the_usage_line_names_the_max_tokens_actually_sent`; what this test
    // owns is the spend beside it, which must be the figure the provider
    // reported rather than an estimate.
    assert!(
        result_block.contains("· task spent 30 · cells 1"),
        "the usage line must carry the response's own usage: {result_block}"
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("spent 60 · reported"),
        "two reported turns total 60 in the sidebar:\n{stdout}"
    );
    assert!(
        stdout.contains("spent 60 · reported"),
        "the sidebar must say the figure was reported, not estimated, when a \
         direct provider's own usage is all there is:\n{stdout}"
    );
}

/// Cumulative spend is an observation, not control flow. Crossing the former
/// 400k default on the first response must neither inject a final-answer
/// preamble nor stop the task after the following cell.
#[cfg(unix)]
#[test]
fn reported_token_spend_never_caps_the_task() {
    let root = scratch_dir("uncapped-token-spend-root");
    let rollout = root.join("rollout.jsonl");

    let (base_url, bodies) = start_fake_provider(vec![
        assistant_reply_with_usage("```sterna\nconst first = 1;\n```", 450_000, 10),
        assistant_reply_with_usage("```sterna\nconst second = first + 1;\n```", 20, 10),
        ending_reply_with_usage(20, 10),
    ]);

    let output = run_session(
        &root,
        &rollout,
        "sess-uncapped-token-spend",
        "keep working until you can return",
        &base_url,
    );
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let bodies = bodies.lock().unwrap();
    assert_eq!(bodies.len(), 3, "the task must continue past 400k spent");
    assert!(
        !last_user_text(&bodies[1]).contains("only action this turn"),
        "spend must not alter the next request: {}",
        last_user_text(&bodies[1])
    );
    assert!(
        last_user_text(&bodies[2]).contains("task spent 450,040"),
        "later turns still receive truthful cumulative spend: {}",
        last_user_text(&bodies[2])
    );
}

/// The precedence half of the §6 addendum, which neither gateway test above
/// can see because their replies carry no `usage`: when the gateway's own row
/// and the response's `usage` both report a turn, the gateway's figures are
/// the ones counted. The row says 100 + 20 per turn; the replies say 20 + 10.
/// Written by the lead at integration, because a mutation preferring the
/// response's `usage` would otherwise survive every test in this file.
#[cfg(unix)]
#[test]
fn the_gateways_row_wins_over_the_responses_usage_when_both_report() {
    let root = scratch_dir("budget-gateway-over-usage-root");
    let rollout = root.join("rollout.jsonl");
    let gateway = write_routing_cost(&root, "fake_routing_cost.sh", false);

    let (base_url, bodies) = start_fake_provider(vec![
        assistant_reply_with_usage("```sterna\nconst x = 1;\n```", 20, 10),
        ending_reply_with_usage(20, 10),
    ]);

    let output = run_session_with_gateway(
        &root,
        &rollout,
        "sess-budget-gateway-over-usage",
        "count them",
        &base_url,
        Some(&gateway),
    );
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let bodies = bodies.lock().unwrap();
    let result_block = last_user_text(&bodies[1]);
    assert!(
        result_block.contains("· task spent 120 · cells 1"),
        "the gateway's row (120) must win over the response's usage (30): {result_block}"
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("spent 240 · reported"),
        "two gateway-reported turns total 240, not the responses' 60:\n{stdout}"
    );
}

// --- docs/supervisor.md: the supervisor's look --------------

/// The system prompt every look's request carries -- §3, verbatim's own
/// first sentence, distinctive enough that no ordinary task turn ever
/// contains it.
const SUPERVISOR_SYSTEM_MARKER: &str = "You watch a coding agent's trajectory";

fn is_supervisor_request(body: &str) -> bool {
    let request: serde_json::Value = serde_json::from_str(body).unwrap();
    request["system"][0]["text"]
        .as_str()
        .unwrap_or("")
        .contains(SUPERVISOR_SYSTEM_MARKER)
}

fn looping_cell_reply() -> String {
    assistant_reply("```sterna\nconst x = 1;\n```")
}

fn write_supervisor_sterna_toml(root: &Path, every: u32, extra: &str) {
    fs::create_dir_all(root.join(".sterna")).unwrap();
    fs::write(
        root.join(".sterna/config.toml"),
        format!("[supervisor]\nevery = {every}\nmodel = \"claude-sonnet-5\"\n{extra}"),
    )
    .unwrap();
}

/// §5, the acceptance test itself: a scripted provider answers the same
/// program three turns running, `every = 3` batches exactly those three
/// cells into one look, and the scripted supervisor model says `intervene`
/// on the trajectory that shows the repeat -- the nudge heads the very next
/// user message, the turn after the third (and second-repeated) cell,
/// within two turns of it.
///
/// The mutation this test kills: the cadence off by one. A look fired after
/// two cells instead of three would see only the first repeat, and one fired
/// after four would miss the window this test asserts on.
#[test]
fn a_planted_three_turn_loop_is_nudged_within_two_turns() {
    let root = scratch_dir("supervisor-loop-root");
    write_supervisor_sterna_toml(&root, 3, "");
    let rollout = root.join("rollout.jsonl");

    let task_count = Mutex::new(0usize);
    let (base_url, bodies) = start_answering_provider(5, move |body| {
        if is_supervisor_request(body) {
            return assistant_reply(
                r#"{"intervene": true, "reason": "the same program three times"}"#,
            );
        }
        let mut count = task_count.lock().unwrap();
        *count += 1;
        if *count <= 3 {
            looping_cell_reply()
        } else {
            ending_reply()
        }
    });

    let output = run_session(
        &root,
        &rollout,
        "sess-supervisor-loop",
        "keep going",
        &base_url,
    );
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let bodies = bodies.lock().unwrap();
    assert_eq!(bodies.len(), 5, "3 task turns, 1 look, 1 final task turn");
    let task_bodies: Vec<&String> = bodies
        .iter()
        .filter(|body| !is_supervisor_request(body))
        .collect();
    assert_eq!(
        task_bodies.len(),
        4,
        "the look is not a task turn: {bodies:?}"
    );
    assert_eq!(
        bodies
            .iter()
            .filter(|body| is_supervisor_request(body))
            .count(),
        1,
        "exactly one look for the three planted cells: {bodies:?}"
    );

    let fourth_turn_answer = last_user_text(task_bodies[3]);
    assert!(
        fourth_turn_answer.starts_with("supervisor: the same program three times"),
        "the nudge must head the turn after the third cell: {fourth_turn_answer}"
    );

    let lines = rollout_lines(&rollout);
    assert!(
        lines.iter().any(|l| l["kind"] == "turn"
            && l["role"] == "user"
            && l["text"]
                .as_str()
                .unwrap_or("")
                .starts_with("supervisor: the same program three times")),
        "the nudge must be recorded as a user turn: {lines:?}"
    );

    // §5's other half, and the lead's second mutation: `enabled = false`
    // sends no supervisor request at all, however the cadence would
    // otherwise trigger.
    let root = scratch_dir("supervisor-off-root");
    write_supervisor_sterna_toml(&root, 3, "enabled = false\n");
    let rollout = root.join("rollout.jsonl");

    let task_count = Mutex::new(0usize);
    let (base_url, bodies) = start_answering_provider(4, move |body| {
        assert!(
            !is_supervisor_request(body),
            "enabled = false must never send a supervisor request"
        );
        let mut count = task_count.lock().unwrap();
        *count += 1;
        if *count <= 3 {
            looping_cell_reply()
        } else {
            ending_reply()
        }
    });

    let output = run_session(
        &root,
        &rollout,
        "sess-supervisor-off",
        "keep going",
        &base_url,
    );
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let bodies = bodies.lock().unwrap();
    assert_eq!(
        bodies.len(),
        4,
        "no supervisor request was inserted: {bodies:?}"
    );
    for body in bodies.iter() {
        assert!(
            !last_user_text(body).starts_with("supervisor:"),
            "enabled = false must never nudge: {}",
            last_user_text(body)
        );
    }
}

/// REQUIRED BEHAVIOR 2: an unparseable look answer is not a nudge, and is
/// shown as such -- it folds into the ordinary "looked, no nudge" outcome
/// rather than silently becoming an intervention. The lead's mutation:
/// unparseable treated as `intervene`.
#[test]
fn an_unparseable_supervisor_answer_is_not_a_nudge() {
    let root = scratch_dir("supervisor-unparseable-root");
    write_supervisor_sterna_toml(&root, 1, "");
    let rollout = root.join("rollout.jsonl");

    let task_count = Mutex::new(0usize);
    let (base_url, bodies) = start_answering_provider(3, move |body| {
        if is_supervisor_request(body) {
            return assistant_reply("not json at all");
        }
        let mut count = task_count.lock().unwrap();
        *count += 1;
        if *count == 1 {
            looping_cell_reply()
        } else {
            ending_reply()
        }
    });

    let output = run_session(
        &root,
        &rollout,
        "sess-supervisor-unparseable",
        "keep going",
        &base_url,
    );
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let bodies = bodies.lock().unwrap();
    assert_eq!(bodies.len(), 3, "task turn, look, task turn: {bodies:?}");
    assert!(is_supervisor_request(&bodies[1]), "{bodies:?}");
    assert!(
        !last_user_text(&bodies[2]).starts_with("supervisor:"),
        "an unparseable answer must never become a nudge: {}",
        last_user_text(&bodies[2])
    );

    let cells = cell_lines(&rollout);
    assert_eq!(cells.len(), 2, "{cells:?}");
}

/// `supervisor.md` §3: "Anything unparseable is *not intervene* and is
/// recorded **as such**." A look that never produced an answer -- a transport
/// error, or a reply that does not parse -- used to be recorded as the healthy
/// `looked, no nudge`, so a supervisor whose model id or endpoint is wrong
/// spent a metered request every `every` cells and read as working. The
/// recorded status now names the failure; it still never nudges.
#[test]
fn a_failed_supervisor_look_is_recorded_as_failed_not_as_no_nudge() {
    let root = scratch_dir("supervisor-failed-look-root");
    write_supervisor_sterna_toml(&root, 1, "");
    let rollout = root.join("rollout.jsonl");

    let task_count = Mutex::new(0usize);
    let (base_url, bodies) = start_answering_provider(3, move |body| {
        if is_supervisor_request(body) {
            return assistant_reply("not json at all");
        }
        let mut count = task_count.lock().unwrap();
        *count += 1;
        if *count == 1 {
            looping_cell_reply()
        } else {
            ending_reply()
        }
    });

    let output = run_session_stdin(
        &root,
        &rollout,
        "sess-supervisor-failed-look",
        &["keep going", "/supervisor"],
        &base_url,
    );
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);

    assert!(
        stdout.contains("Latest: look failed: unparseable"),
        "a failed look must be recorded as failed, and name its cause:\n{stdout}"
    );
    assert!(
        !stdout.contains("Latest: looked; no nudge"),
        "a failed look must not be recorded as a healthy look:\n{stdout}"
    );

    // The safety half of §3, unchanged: a look that produced no answer never
    // becomes a nudge.
    let bodies = bodies.lock().unwrap();
    assert_eq!(bodies.len(), 3, "task turn, look, task turn: {bodies:?}");
    assert!(is_supervisor_request(&bodies[1]), "{bodies:?}");
    assert!(
        !last_user_text(&bodies[2]).starts_with("supervisor:"),
        "a failed look must never become a nudge: {}",
        last_user_text(&bodies[2])
    );
}

/// §3: the look's request carries `x-glasshouse-purpose: supervisor`, so the
/// ledger can tell it apart from a task turn before the gateway reads the
/// header itself.
#[test]
fn the_look_carries_the_purpose_header() {
    let root = scratch_dir("supervisor-header-root");
    write_supervisor_sterna_toml(&root, 1, "");
    let rollout = root.join("rollout.jsonl");

    let task_count = Mutex::new(0usize);
    let (base_url, captured) = start_capturing_provider(3, move |body| {
        if is_supervisor_request(body) {
            return assistant_reply(r#"{"intervene": false, "reason": "fine"}"#);
        }
        let mut count = task_count.lock().unwrap();
        *count += 1;
        if *count == 1 {
            looping_cell_reply()
        } else {
            ending_reply()
        }
    });

    let output = run_session(
        &root,
        &rollout,
        "sess-supervisor-header",
        "count them",
        &base_url,
    );
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let captured = captured.lock().unwrap();
    let look = captured
        .iter()
        .find(|(_, body)| is_supervisor_request(body))
        .expect("the look's own request must have been sent");
    assert_eq!(
        look.0.get("x-glasshouse-purpose").map(String::as_str),
        Some("supervisor"),
        "the look must carry the purpose header: {:?}",
        look.0
    );
}

/// The addendum (lead, 07:12): the look must name `[supervisor] model` --
/// map line 2469's "with a cheaper model" clause, the one part of it this
/// package had left the task's own model standing in for. Every ordinary
/// task turn still carries `wire::MODEL`; only the look's own request names
/// the configured, deliberately distinct id.
#[test]
fn the_look_names_the_supervisors_model_and_the_turns_name_the_tasks() {
    let root = scratch_dir("supervisor-model-root");
    fs::create_dir_all(root.join(".sterna")).unwrap();
    fs::write(
        root.join(".sterna/config.toml"),
        "[supervisor]\nevery = 1\nmodel = \"cheap-model-for-the-test\"\n",
    )
    .unwrap();
    let rollout = root.join("rollout.jsonl");

    let task_count = Mutex::new(0usize);
    let (base_url, bodies) = start_answering_provider(3, move |body| {
        if is_supervisor_request(body) {
            return assistant_reply(r#"{"intervene": false, "reason": "fine"}"#);
        }
        let mut count = task_count.lock().unwrap();
        *count += 1;
        if *count == 1 {
            looping_cell_reply()
        } else {
            ending_reply()
        }
    });

    let output = run_session(
        &root,
        &rollout,
        "sess-supervisor-model",
        "count them",
        &base_url,
    );
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let bodies = bodies.lock().unwrap();
    assert_eq!(bodies.len(), 3, "task turn, look, task turn: {bodies:?}");

    for (index, body) in bodies.iter().enumerate() {
        let request: serde_json::Value = serde_json::from_str(body).unwrap();
        let model = request["model"].as_str().unwrap();
        if is_supervisor_request(body) {
            assert_eq!(
                model, "cheap-model-for-the-test",
                "the look must name the configured model: request {index}: {body}"
            );
        } else {
            assert_eq!(
                model,
                sterna::wire::MODEL,
                "every task turn must still name the task's own model: request {index}: {body}"
            );
        }
    }
}

/// REQUIRED BEHAVIOR 4: the four limits actually bind the runtime and the
/// cell limit, loaded from `config.toml` rather than the built-in
/// default of 40.
#[test]
fn a_loaded_cell_limit_ends_the_task() {
    let root = scratch_dir("loaded-cell-limit-root");
    fs::create_dir_all(root.join(".sterna")).unwrap();
    fs::write(root.join(".sterna/config.toml"), "[limits]\ncells = 2\n").unwrap();
    let rollout = root.join("rollout.jsonl");

    // Three scripted turns: the third is the final-answer turn.
    let turns = 3;
    let replies = (0..turns)
        .map(|_| assistant_reply("```sterna\nconst x = 1;\n```"))
        .collect();
    let (base_url, bodies) = start_fake_provider(replies);

    let output = run_session(
        &root,
        &rollout,
        "sess-loaded-cell-limit",
        "keep going",
        &base_url,
    );
    assert!(
        !output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let bodies = bodies.lock().unwrap();
    assert_eq!(
        bodies.len(),
        turns,
        "the task must stop one turn after the loaded cap, not run on"
    );
    assert!(
        last_user_text(&bodies[turns - 1])
            .starts_with("The cell limit this project set (2) is reached"),
        "a `cells = 2` config.toml must end the task after two cells: {}",
        last_user_text(&bodies[turns - 1])
    );
}

/// One captured request: its headers (lower-cased names) and its body.
type CapturedRequest = (std::collections::HashMap<String, String>, String);

/// The same minimal endpoint as `start_answering_provider`, but also records
/// each request's headers alongside its body -- only
/// `the_look_carries_the_purpose_header` above needs a header, and nothing
/// before this heading reads one, so this is an addition rather than a
/// change to the helper `sterna-61e-usage` also builds on.
fn start_capturing_provider<F>(
    turns: usize,
    answer: F,
) -> (String, Arc<Mutex<Vec<CapturedRequest>>>)
where
    F: Fn(&str) -> String + Send + 'static,
{
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let captured = Arc::new(Mutex::new(Vec::new()));
    let captured_thread = Arc::clone(&captured);

    thread::spawn(move || {
        for _ in 0..turns {
            let Ok((mut stream, _)) = listener.accept() else {
                return;
            };
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut headers = std::collections::HashMap::new();
            loop {
                let mut line = String::new();
                if reader.read_line(&mut line).unwrap_or(0) == 0 {
                    break;
                }
                if line == "\r\n" || line == "\n" {
                    break;
                }
                if let Some((name, value)) = line.trim_end().split_once(':') {
                    headers.insert(name.trim().to_ascii_lowercase(), value.trim().to_string());
                }
            }
            let content_length = headers
                .get("content-length")
                .and_then(|v| v.parse().ok())
                .unwrap_or(0usize);
            let mut body = vec![0u8; content_length];
            if reader.read_exact(&mut body).is_err() {
                continue;
            }
            let body = String::from_utf8_lossy(&body).into_owned();
            let reply = answer(&body);
            captured_thread.lock().unwrap().push((headers, body));

            let response_body = reply.as_bytes();
            let response = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
                response_body.len()
            );
            let _ = stream.write_all(response.as_bytes());
            let _ = stream.write_all(response_body);
            let _ = stream.flush();
        }
    });

    (format!("http://127.0.0.1:{port}"), captured)
}

// --- the keyboard's end of the cancellation facility (GH-PANE-SIGINT) ----

/// SIGINT during a session, and the three things it must do: cancel the tool
/// call in flight, leave a cell that is only computing alone, and end the
/// session on a second Ctrl-C without cutting a rollout line in half.
///
/// Unix only, because the signal is: the Windows half is
/// `SetConsoleCtrlHandler`, which cannot be raised from a test the way
/// `kill -INT` can. Every helper below is inside the module so nothing here
/// is dead code on the Windows cell.
#[cfg(unix)]
mod interrupts {
    use super::*;
    use std::process::{Child, Stdio};
    use std::sync::mpsc;
    use std::time::{Duration, Instant};

    fn captured_request(mut stream: &TcpStream) -> (String, String) {
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        let mut headers = String::new();
        let mut content_length = 0usize;
        loop {
            let mut line = String::new();
            if reader.read_line(&mut line).unwrap_or(0) == 0 {
                break;
            }
            if line == "\r\n" || line == "\n" {
                break;
            }
            if let Some(rest) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                content_length = rest.trim().parse().unwrap_or(0);
            }
            headers.push_str(&line.to_ascii_lowercase());
        }
        let mut body = vec![0; content_length];
        reader.read_exact(&mut body).unwrap();
        // Keep the socket borrowed until the complete request has arrived.
        let _ = &mut stream;
        (headers, String::from_utf8(body).unwrap())
    }

    fn answer(mut stream: TcpStream, body: &str) {
        let _ = write!(
            stream,
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
    }

    type CapturedRequests = Arc<Mutex<Vec<(String, String)>>>;

    /// First answer asks for a cell helper; the helper and the task turn after
    /// it are then held independently. Releasing the cancelled helper while
    /// the task turn remains held makes any late fourth request observable.
    fn held_cell_helper_provider() -> (
        String,
        CapturedRequests,
        mpsc::Receiver<()>,
        mpsc::Sender<()>,
        mpsc::Receiver<()>,
        mpsc::Sender<()>,
    ) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let requests = Arc::new(Mutex::new(Vec::new()));
        let captured = Arc::clone(&requests);
        let (helper_seen_tx, helper_seen) = mpsc::channel();
        let (helper_release, helper_held) = mpsc::channel();
        let (task_seen_tx, task_seen) = mpsc::channel();
        let (task_release, task_held) = mpsc::channel();

        thread::spawn(move || {
            let (first, _) = listener.accept().unwrap();
            let request = captured_request(&first);
            captured.lock().unwrap().push(request);
            answer(
                first,
                &assistant_reply(
                    "```sterna\nconst found = await helper.find(\"find the needle\");\nreturn found;\n```",
                ),
            );

            let (helper, _) = listener.accept().unwrap();
            let request = captured_request(&helper);
            captured.lock().unwrap().push(request);
            helper_seen_tx.send(()).unwrap();
            thread::spawn(move || {
                if helper_held.recv().is_ok() {
                    answer(
                        helper,
                        &assistant_reply(
                            "```sterna\nconst late = await read({ path: \"late.txt\" });\n```",
                        ),
                    );
                }
            });

            let (task, _) = listener.accept().unwrap();
            let request = captured_request(&task);
            captured.lock().unwrap().push(request);
            task_seen_tx.send(()).unwrap();
            thread::spawn(move || {
                if task_held.recv().is_ok() {
                    answer(task, &ending_reply());
                }
            });

            listener.set_nonblocking(true).unwrap();
            let deadline = Instant::now() + Duration::from_millis(700);
            while Instant::now() < deadline {
                match listener.accept() {
                    Ok((late, _)) => {
                        let request = captured_request(&late);
                        captured.lock().unwrap().push(request);
                        answer(late, &ending_reply());
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(10));
                    }
                    Err(_) => break,
                }
            }
        });

        (
            base,
            requests,
            helper_seen,
            helper_release,
            task_seen,
            task_release,
        )
    }

    /// Hold pushed preflight first and the ordinary task request second. A
    /// third request can only come from the cancelled Scout accepting its late
    /// tool-bearing response.
    fn held_preflight_provider() -> (
        String,
        CapturedRequests,
        mpsc::Receiver<()>,
        mpsc::Sender<()>,
        mpsc::Receiver<()>,
        mpsc::Sender<()>,
    ) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let requests = Arc::new(Mutex::new(Vec::new()));
        let captured = Arc::clone(&requests);
        let (helper_seen_tx, helper_seen) = mpsc::channel();
        let (helper_release, helper_held) = mpsc::channel();
        let (task_seen_tx, task_seen) = mpsc::channel();
        let (task_release, task_held) = mpsc::channel();

        thread::spawn(move || {
            let (helper, _) = listener.accept().unwrap();
            let request = captured_request(&helper);
            captured.lock().unwrap().push(request);
            helper_seen_tx.send(()).unwrap();
            thread::spawn(move || {
                if helper_held.recv().is_ok() {
                    answer(
                        helper,
                        &assistant_reply(
                            "```sterna\nconst late = await read({ path: \"late.txt\" });\n```",
                        ),
                    );
                }
            });

            let (task, _) = listener.accept().unwrap();
            let request = captured_request(&task);
            captured.lock().unwrap().push(request);
            task_seen_tx.send(()).unwrap();
            thread::spawn(move || {
                if task_held.recv().is_ok() {
                    answer(task, &ending_reply());
                }
            });

            listener.set_nonblocking(true).unwrap();
            let deadline = Instant::now() + Duration::from_millis(700);
            while Instant::now() < deadline {
                match listener.accept() {
                    Ok((late, _)) => {
                        let request = captured_request(&late);
                        captured.lock().unwrap().push(request);
                        answer(late, &ending_reply());
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(10));
                    }
                    Err(_) => break,
                }
            }
        });

        (
            base,
            requests,
            helper_seen,
            helper_release,
            task_seen,
            task_release,
        )
    }

    /// A command that never ends and writes a marker the moment it starts.
    ///
    /// **`bash` builtins only, on purpose.** The seatbelt names one resolved
    /// binary in `process-exec*` (the 61D exec-roots ruling), so a confined
    /// `bash` cannot exec `/bin/sleep` at all -- the same reason
    /// `runtime_cells.rs`'s own cancellation test spins rather than sleeps.
    /// The marker is what lets a test signal *after* the child exists rather
    /// than after a guessed delay.
    ///
    /// **`label` makes the command line itself unique**, so
    /// [`no_spinner_survives`] can look for one test's child in `ps` while the
    /// other tests in this binary are running their own beside it.
    fn spins_and_marks(label: &str) -> String {
        format!("while :; do echo go > marker-{label}; done")
    }

    fn marker_of(root: &Path, label: &str) -> PathBuf {
        root.join(format!("marker-{label}"))
    }

    /// A cell whose one statement is that call, so the cell's ending is the
    /// call's ending.
    fn spinning_cell(label: &str) -> String {
        assistant_reply(&format!(
            "```sterna\nconst out = await bash({{ command: \"{}\" }});\nreturn out.stdout;\n```",
            spins_and_marks(label)
        ))
    }

    /// No confined child of this test is still running.
    ///
    /// `std::process::exit` does not touch a process's children, so the
    /// second-Ctrl-C path is the one place `sterna` could reparent a spinning
    /// `bash` to `init` and leave it there. It did -- one child at 87% of a
    /// core, for ever -- until `Interrupter::end_the_session` learned to
    /// cancel and then hold the rollout's write lock across the reap grace.
    /// This is a regression test, not a tidiness check.
    ///
    /// **`ps -A -ww`, and the width flag is load-bearing**: macOS cuts
    /// `-o command` at the terminal width, and a confined `bash`'s command
    /// line carries an absolute interpreter path before the needle -- so a
    /// truncated listing reads exactly like "nothing survived". The row is
    /// printed with `pid` and `ppid` because an orphan's `ppid 1` is what
    /// names the defect.
    fn no_spinner_survives(label: &str) {
        let needle = spins_and_marks(label);
        let listing = Command::new("ps")
            .args(["-A", "-ww", "-o", "pid,ppid,stat,%cpu,command"])
            .output()
            .expect("ps runs");
        let listing = String::from_utf8_lossy(&listing.stdout);
        let survivors: Vec<&str> = listing
            .lines()
            .filter(|line| line.contains(&needle))
            .collect();
        assert!(
            survivors.is_empty(),
            "the exit left a confined child running: {survivors:?}"
        );
    }

    /// A cell that only computes: no call, no handle from a tool, and long
    /// enough that a signal sent when the turn was answered lands inside it.
    fn computing_cell() -> String {
        assistant_reply(
            "```sterna\nlet n = 0;\nfor (let i = 0; i < 50000000; i++) { n = (n + i) % 1000003; \
             }\nconst spun = n;\n```",
        )
    }

    /// The grants [`spins_and_marks`] needs, and nothing else: three command
    /// prefixes, no path rule of any kind (`sandbox-grants.md` §2 -- argv
    /// admission grants no file access).
    fn grant_the_spin(root: &Path) {
        fs::create_dir_all(root.join(".sterna")).unwrap();
        fs::write(
            root.join(".sterna/config.toml"),
            // `trap` is here for the stubborn-job test below, which needs a
            // job that ignores every catchable signal; it grants no file
            // access either (`sandbox-grants.md` §2).
            "[permissions]\nallow = [\"Bash(while*)\", \"Bash(do*)\", \"Bash(echo*)\", \"Bash(trap*)\"]\n",
        )
        .unwrap();
    }

    /// [`run_session`], but spawned rather than waited on, because these
    /// tests need the pid while it runs.
    ///
    /// stdout is discarded rather than piped: the session redraws the whole
    /// notebook every turn, and a pipe nobody drains while the test waits for
    /// a marker would fill and stop the very process being signalled.
    fn spawn_session(root: &Path, rollout: &Path, task: &str, base_url: &str) -> Child {
        Command::new(env!("CARGO_BIN_EXE_sterna"))
            .arg("session")
            .arg("--root")
            .arg(root)
            .arg("--rollout")
            .arg(rollout)
            .arg("--session")
            .arg("sess-interrupt")
            .arg("--model")
            .arg(sterna::wire::MODEL)
            .arg("--task")
            .arg(task)
            .env("ANTHROPIC_BASE_URL", base_url)
            .env("XDG_CONFIG_HOME", root.join("global-config"))
            .env_remove("ANTHROPIC_AUTH_TOKEN")
            .env_remove("ANTHROPIC_API_KEY")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .expect("sterna starts")
    }

    fn wait_for_marker(marker: &Path, child: &mut Child) {
        let deadline = Instant::now() + Duration::from_secs(20);
        while Instant::now() < deadline {
            if marker.exists() {
                return;
            }
            thread::sleep(Duration::from_millis(10));
        }
        let _ = child.kill();
        let _ = child.wait();
        panic!(
            "the confined child never started: {} absent",
            marker.display()
        );
    }

    fn wait_for_turns(bodies: &Arc<Mutex<Vec<String>>>, count: usize, child: &mut Child) {
        let deadline = Instant::now() + Duration::from_secs(20);
        while Instant::now() < deadline {
            if bodies.lock().unwrap().len() >= count {
                return;
            }
            thread::sleep(Duration::from_millis(10));
        }
        let _ = child.kill();
        let _ = child.wait();
        panic!("the session never asked for turn {count}");
    }

    fn send_interrupt(child: &Child) {
        let status = Command::new("kill")
            .arg("-INT")
            .arg(child.id().to_string())
            .status()
            .expect("kill runs");
        assert!(status.success(), "kill -INT {} failed", child.id());
    }

    /// How one call of a cell's recorded trajectory ended -- `{"threw":
    /// "Cancelled"}` on the line (`runtime-contract.md` §9.4).
    fn call_endings(cell: &serde_json::Value) -> Vec<serde_json::Value> {
        cell["calls"]
            .as_array()
            .expect("every cell line carries a trajectory")
            .iter()
            .map(|call| call["ended"].clone())
            .collect()
    }

    fn outcomes(rollout: &Path) -> Vec<String> {
        cell_lines(rollout)
            .iter()
            .map(|cell| cell["outcome"].as_str().unwrap().to_string())
            .collect()
    }

    /// Ctrl-C with a call in flight: the call ends as §5's `Cancelled` throw,
    /// the cell is answered, the model is asked for another turn, and the
    /// session goes on to end normally -- against a child that would
    /// otherwise never exit.
    #[test]
    fn a_sigint_during_a_tool_call_cancels_it_and_the_session_continues() {
        let root = scratch_dir("sigint-call");
        grant_the_spin(&root);
        let rollout = root.join("rollout.jsonl");
        let marker = marker_of(&root, "call");
        let (base_url, bodies) = start_fake_provider(vec![
            spinning_cell("call"),
            assistant_reply("```sterna\nanswer(\"done\");\n```"),
        ]);

        let started = Instant::now();
        let mut child = spawn_session(&root, &rollout, "spin for me", &base_url);
        wait_for_marker(&marker, &mut child);
        send_interrupt(&child);
        let output = child.wait_with_output().unwrap();
        let elapsed = started.elapsed();

        assert!(
            output.status.success(),
            "status {:?}, stderr: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            elapsed < Duration::from_secs(25),
            "the session ran for {elapsed:?} against a child that never exits"
        );

        let cells = cell_lines(&rollout);
        assert_eq!(cells[0]["outcome"], "threw", "{cells:?}");
        assert_eq!(
            call_endings(&cells[0]),
            vec![serde_json::json!({"threw": "Cancelled"})],
            "{cells:?}"
        );

        // The second turn was requested, and what it carried is §5's error
        // section naming the class -- so the model was told the call was
        // cancelled rather than being asked to guess.
        let bodies = bodies.lock().unwrap();
        assert_eq!(bodies.len(), 2, "the session did not ask for another turn");
        let answer = last_user_text(&bodies[1]);
        assert!(answer.contains("## Error"), "{answer}");
        assert!(answer.contains("Cancelled"), "{answer}");
    }

    #[test]
    fn a_sigint_cancels_a_cell_helper_without_leaking_or_running_its_late_reply() {
        let root = scratch_dir("sigint-helper");
        write_helpers_sterna_toml(&root, "helper-tier");
        fs::write(root.join("late.txt"), "a late helper must not read this\n").unwrap();
        let rollout = root.join("rollout.jsonl");
        let (base, requests, helper_seen, helper_release, task_seen, task_release) =
            held_cell_helper_provider();
        // Fewer than four words bypasses pushed preflight; this test reaches
        // the explicit helper call in the first task-model cell.
        let child = spawn_session(&root, &rollout, "find this", &base);
        helper_seen
            .recv_timeout(Duration::from_secs(10))
            .expect("the cell helper request starts");

        let interrupted = Instant::now();
        send_interrupt(&child);
        task_seen
            .recv_timeout(Duration::from_secs(2))
            .expect("the cancelled helper returns control to the task promptly");
        assert!(
            interrupted.elapsed() < Duration::from_secs(2),
            "helper cancellation stayed blocked on the provider"
        );

        helper_release.send(()).unwrap();
        thread::sleep(Duration::from_millis(300));
        {
            let requests = requests.lock().unwrap();
            assert_eq!(
                requests.len(),
                3,
                "the cancelled helper executed its late read and requested another turn"
            );
            assert!(
                requests[1].0.contains("x-glasshouse-purpose: helper"),
                "tool-holding helper request lost its routing identity: {}",
                requests[1].0
            );
            let helper_body: serde_json::Value = serde_json::from_str(&requests[1].1).unwrap();
            assert_eq!(helper_body["model"], "helper-tier");
            assert!(
                !requests[0].0.contains("x-glasshouse-purpose: helper")
                    && !requests[2].0.contains("x-glasshouse-purpose: helper"),
                "ordinary task traffic was stamped as helper traffic"
            );
        }

        task_release.send(()).unwrap();
        let output = child.wait_with_output().unwrap();
        assert!(
            output.status.success(),
            "stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let cells = cell_lines(&rollout);
        assert_eq!(
            call_endings(&cells[0]),
            vec![serde_json::json!({"threw":"Cancelled"})]
        );
        // The last cell is `ending_reply`: it answers and runs off its end,
        // so the program yielded even though the task is over.
        assert_eq!(outcomes(&rollout), vec!["threw", "yielded"]);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_sigint_cancels_preflight_without_leaking_into_the_task() {
        let root = scratch_dir("sigint-preflight");
        write_helpers_sterna_toml(&root, "helper-tier");
        fs::write(root.join("late.txt"), "a late Scout must not read this\n").unwrap();
        let rollout = root.join("rollout.jsonl");
        let (base, requests, helper_seen, helper_release, task_seen, task_release) =
            held_preflight_provider();
        let child = spawn_session(
            &root,
            &rollout,
            "find where the cancellation token is used",
            &base,
        );
        helper_seen
            .recv_timeout(Duration::from_secs(10))
            .expect("preflight starts");

        let interrupted = Instant::now();
        send_interrupt(&child);
        task_seen
            .recv_timeout(Duration::from_secs(2))
            .expect("cancelled preflight gives control to the task promptly");
        assert!(interrupted.elapsed() < Duration::from_secs(2));

        helper_release.send(()).unwrap();
        thread::sleep(Duration::from_millis(300));
        {
            let requests = requests.lock().unwrap();
            assert_eq!(
                requests.len(),
                2,
                "cancelled preflight executed a late read or cancellation reached the task"
            );
            assert!(requests[0].0.contains("x-glasshouse-purpose: helper"));
            let helper_body: serde_json::Value = serde_json::from_str(&requests[0].1).unwrap();
            assert_eq!(helper_body["model"], "helper-tier");
            assert!(!requests[1].0.contains("x-glasshouse-purpose: helper"));
        }

        task_release.send(()).unwrap();
        let output = child.wait_with_output().unwrap();
        assert!(
            output.status.success(),
            "stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(outcomes(&rollout), vec!["yielded"]);
        fs::remove_dir_all(root).unwrap();
    }

    /// A second Ctrl-C inside two seconds ends the session with the status a
    /// shell reports for an interrupted process, and the rollout it leaves
    /// behind is whole: every line parses, including the last.
    #[test]
    fn a_second_sigint_within_two_seconds_ends_the_session_with_exit_130() {
        let root = scratch_dir("sigint-twice");
        grant_the_spin(&root);
        let rollout = root.join("rollout.jsonl");
        let marker = marker_of(&root, "twice");
        let (base_url, _bodies) = start_fake_provider(vec![
            spinning_cell("twice"),
            spinning_cell("twice"),
            spinning_cell("twice"),
        ]);

        let mut child = spawn_session(&root, &rollout, "spin twice", &base_url);
        wait_for_marker(&marker, &mut child);
        send_interrupt(&child);
        thread::sleep(Duration::from_millis(200));
        send_interrupt(&child);
        let output = child.wait_with_output().unwrap();

        assert_eq!(
            output.status.code(),
            Some(130),
            "stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            String::from_utf8_lossy(&output.stderr).contains("interrupted twice"),
            "stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        );

        // The exit cancelled before it took the writing lock, so the call in
        // flight killed and reaped its own child on the way out.
        no_spinner_survives("twice");

        // `rollout_lines` parses every line and panics on one that does not,
        // so this is the whole-last-line assertion: an exit taken in the
        // middle of a write would leave a fragment here.
        let lines = rollout_lines(&rollout);
        assert!(
            lines.len() >= 2,
            "the session exited before it recorded anything: {lines:?}"
        );
    }

    /// Ctrl-C with no call in flight does not end the cell: JavaScript is
    /// stopped by the wall-clock watchdog and never by the interrupt, so a
    /// cell that is only computing runs to its own end. The interrupt is not
    /// lost either -- the **next** cell's call is what it cancels.
    #[test]
    fn a_sigint_with_no_call_in_flight_does_not_end_the_cell() {
        let root = scratch_dir("sigint-compute");
        grant_the_spin(&root);
        let rollout = root.join("rollout.jsonl");
        let (base_url, bodies) = start_fake_provider(vec![
            computing_cell(),
            spinning_cell("compute"),
            assistant_reply("```sterna\nanswer(\"done\");\n```"),
        ]);

        let mut child = spawn_session(&root, &rollout, "compute then spin", &base_url);
        // The first turn has been answered, so the computing cell is running
        // or about to.
        wait_for_turns(&bodies, 1, &mut child);
        send_interrupt(&child);
        let output = child.wait_with_output().unwrap();

        assert!(
            output.status.success(),
            "status {:?}, stderr: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        );

        let cells = cell_lines(&rollout);
        assert_eq!(
            outcomes(&rollout),
            vec!["yielded", "threw", "yielded"],
            "the computing cell was ended by the signal: {cells:?}"
        );
        assert!(
            call_endings(&cells[0]).is_empty(),
            "the computing cell made a call: {cells:?}"
        );
        assert_eq!(
            call_endings(&cells[1]),
            vec![serde_json::json!({"threw": "Cancelled"})],
            "the interrupt was dropped instead of spent on the next call: {cells:?}"
        );
    }

    /// The other side of the window, and the reason "within two seconds" is a
    /// claim rather than a decoration: two Ctrl-Cs **far enough apart** are
    /// two first interrupts, each cancelling one call, and the session
    /// survives both to end normally.
    ///
    /// Without this, widening [`DOUBLE_INTERRUPT_WINDOW`] to any larger value
    /// changes no observable behaviour the test above watches -- it sends its
    /// pair 200 ms apart, which is inside every window a mutation would
    /// choose. The gap here is 3.5 s against a 2 s window, so the margin
    /// absorbs a loaded machine's scheduling without reaching the boundary.
    #[test]
    fn two_sigints_more_than_two_seconds_apart_do_not_end_the_session() {
        let root = scratch_dir("sigint-apart");
        grant_the_spin(&root);
        let rollout = root.join("rollout.jsonl");
        let marker = marker_of(&root, "apart");
        let (base_url, _bodies) = start_fake_provider(vec![
            spinning_cell("apart"),
            spinning_cell("apart"),
            assistant_reply("```sterna\nanswer(\"done\");\n```"),
        ]);

        let mut child = spawn_session(&root, &rollout, "spin, wait, spin", &base_url);
        wait_for_marker(&marker, &mut child);
        send_interrupt(&child);
        // The second cell is spinning by now, and stays so until this lands.
        thread::sleep(Duration::from_millis(3500));
        send_interrupt(&child);
        let output = child.wait_with_output().unwrap();

        let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
        assert!(
            output.status.success(),
            "status {:?}, stderr: {stderr}",
            output.status
        );
        assert!(
            !stderr.contains("interrupted twice"),
            "the second interrupt was paired with one 3.5 s older: {stderr}"
        );
        assert_eq!(
            outcomes(&rollout),
            vec!["threw", "threw", "yielded"],
            "each interrupt must have cancelled one call of its own"
        );
    }

    // --- GH-PANE-BG-EXIT-AND-COST: what an exit must take with it ---------
    //
    // The three tests above watch the foreground child of a call in flight.
    // The three below watch the **background board**, which was added after
    // `end_the_session` was written and was never wired into its fix: §5's
    // "a background job outlives no session" has to be true of every exit
    // this binary can take, not only of the tidy ones.

    /// A cell that starts a background job and then **yields**.
    ///
    /// It must not `return`: a top-level return ends the task, and
    /// `run_task`'s own `bg::shutdown` would take the job with it before any
    /// signal arrived -- a different exit path, tested separately below.
    fn background_cell(label: &str) -> String {
        assistant_reply(&format!(
            "```sterna\nbg.run(\"{}\");\nconst started = 1;\n```",
            spins_and_marks(label)
        ))
    }

    /// Waits, bounded, for a path to appear, and answers whether it did.
    /// Every wait in these tests is bounded: a job that never starts must
    /// fail a test rather than hang one.
    fn waits_for(path: &Path) -> bool {
        let deadline = Instant::now() + Duration::from_secs(20);
        while Instant::now() < deadline {
            if path.exists() {
                return true;
            }
            thread::sleep(Duration::from_millis(10));
        }
        path.exists()
    }

    /// **The second Ctrl-C takes the background jobs with it.**
    ///
    /// `std::process::exit` does not touch a process's children, so before
    /// `end_the_session` learned to shut the board down this left the job's
    /// `bash` on `ppid 1` spinning at 99% of a core after `sterna` had exited
    /// 130 -- the same defect the foreground child's fix closed, in the same
    /// function, for the half of it that did not exist yet.
    ///
    /// The exit is timed as well as asserted: a shutdown that waited for a
    /// job that would not stop would be a worse defect than the orphan.
    #[test]
    fn a_second_sigint_takes_the_background_jobs_with_it() {
        let root = scratch_dir("sigint-bg");
        grant_the_spin(&root);
        let rollout = root.join("rollout.jsonl");
        let job = marker_of(&root, "bgjob");
        let foreground = marker_of(&root, "bgfg");
        let (base_url, _bodies) = start_fake_provider(vec![
            background_cell("bgjob"),
            spinning_cell("bgfg"),
            spinning_cell("bgfg"),
            spinning_cell("bgfg"),
        ]);

        let mut child = spawn_session(&root, &rollout, "start a job, then spin", &base_url);
        // Both processes exist before the first signal: the job's, started by
        // the first cell, and the call's, started by the second.
        wait_for_marker(&job, &mut child);
        wait_for_marker(&foreground, &mut child);
        send_interrupt(&child);
        thread::sleep(Duration::from_millis(200));
        send_interrupt(&child);
        let asked = Instant::now();
        let output = child.wait_with_output().unwrap();
        let exit_took = asked.elapsed();

        assert_eq!(
            output.status.code(),
            Some(130),
            "stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        no_spinner_survives("bgfg");
        no_spinner_survives("bgjob");
        assert!(
            exit_took < Duration::from_secs(5),
            "the second Ctrl-C took {exit_took:?} to end the session; a Ctrl-C that waits is not \
             a Ctrl-C"
        );
    }

    /// The tidy exit: a task's top-level `return` reaches `run_task`'s own
    /// `bg::shutdown`, and nothing it started is left running.
    ///
    /// The second turn is not answered until the job's own process exists,
    /// so the assertion cannot hold vacuously by racing the job's start.
    #[test]
    fn a_task_that_returns_takes_its_background_job_with_it() {
        let root = scratch_dir("bg-return");
        grant_the_spin(&root);
        let rollout = root.join("rollout.jsonl");
        let job = marker_of(&root, "bgret");
        let gate = job.clone();
        let replies = [background_cell("bgret"), ending_reply()];
        let next = Mutex::new(0usize);
        let (base_url, _bodies) = start_answering_provider(2, move |_body| {
            let mut index = next.lock().unwrap();
            if *index > 0 {
                assert!(waits_for(&gate), "the background job never started");
            }
            let reply = replies[*index].clone();
            *index += 1;
            reply
        });

        let output = run_session(
            &root,
            &rollout,
            "sess-bg-return",
            "start a job, then return",
            &base_url,
        );

        assert!(
            output.status.success(),
            "status {:?}, stderr: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(job.exists(), "the background job never started");
        no_spinner_survives("bgret");
    }

    /// The untidy one: a task that fails mid-flight leaves `run_task` by `?`
    /// without reaching its own shutdown, so `session::run`'s is what has to
    /// catch the job -- which is the promise that call was added for.
    ///
    /// The failure is a reply that is not a Messages response at all, gated
    /// on the job's marker so the job is running when the task dies.
    #[test]
    fn a_task_that_fails_mid_flight_takes_its_background_job_with_it() {
        let root = scratch_dir("bg-fail");
        grant_the_spin(&root);
        let rollout = root.join("rollout.jsonl");
        let job = marker_of(&root, "bgfail");
        let gate = job.clone();
        let replies = [
            background_cell("bgfail"),
            "this is not a Messages response".to_string(),
        ];
        let next = Mutex::new(0usize);
        let (base_url, _bodies) = start_answering_provider(2, move |_body| {
            let mut index = next.lock().unwrap();
            if *index > 0 {
                assert!(waits_for(&gate), "the background job never started");
            }
            let reply = replies[*index].clone();
            *index += 1;
            reply
        });

        let output = run_session(
            &root,
            &rollout,
            "sess-bg-fail",
            "start a job, then fail",
            &base_url,
        );

        assert!(
            !output.status.success(),
            "the unparseable reply was accepted: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(job.exists(), "the background job never started");
        no_spinner_survives("bgfail");
    }

    /// A job that ignores every catchable signal, so only the ladder's second
    /// rung stops it -- `invoke`'s `killpg(SIGKILL)` on the group the call
    /// created.
    fn stubborn_spin(label: &str) -> String {
        format!("trap '' TERM INT HUP; {}", spins_and_marks(label))
    }

    /// A cell that starts ten of them and yields.
    fn ten_stubborn_jobs(label: &str) -> String {
        assistant_reply(&format!(
            "```sterna\nfor (let i = 0; i < 10; i++) {{ bg.run(\"{}\"); }}\nconst started = 1;\n```",
            stubborn_spin(label)
        ))
    }

    /// **The other half of the Blocker: the exit must stay prompt.**
    ///
    /// Killing the board is only half a fix. `bg::shutdown`'s own grace is
    /// ten seconds, and an exit that spent it — or one settle per job — would
    /// have made a double Ctrl-C worse than the orphan it closes, which is
    /// why `end_the_session` passes its own reap grace and why the whole
    /// shutdown is bounded by one grace rather than by one per job. Ten jobs
    /// that ignore `TERM`, `INT` and `HUP`, and the exit is still measured in
    /// hundreds of milliseconds.
    #[test]
    fn ten_signal_ignoring_jobs_do_not_hold_the_exit() {
        let root = scratch_dir("sigint-stubborn");
        grant_the_spin(&root);
        let rollout = root.join("rollout.jsonl");
        let job = marker_of(&root, "bgstub");
        let foreground = marker_of(&root, "bgstubfg");
        let (base_url, _bodies) = start_fake_provider(vec![
            ten_stubborn_jobs("bgstub"),
            spinning_cell("bgstubfg"),
            spinning_cell("bgstubfg"),
            spinning_cell("bgstubfg"),
        ]);

        let mut child = spawn_session(&root, &rollout, "start ten jobs, then spin", &base_url);
        wait_for_marker(&job, &mut child);
        wait_for_marker(&foreground, &mut child);
        send_interrupt(&child);
        thread::sleep(Duration::from_millis(200));
        send_interrupt(&child);
        let asked = Instant::now();
        let output = child.wait_with_output().unwrap();
        let exit_took = asked.elapsed();

        assert_eq!(
            output.status.code(),
            Some(130),
            "stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        // Bounded by `REAP_GRACE` for the board and `REAP_GRACE` again for
        // the foreground child, plus the settles the board's own grace caps:
        // two seconds is a generous ceiling that a per-job grace would still
        // blow through, and ten seconds is what `bg`'s own grace would cost.
        assert!(
            exit_took < Duration::from_secs(2),
            "ten stubborn jobs held the exit for {exit_took:?}"
        );
        no_spinner_survives("bgstub");
        no_spinner_survives("bgstubfg");
    }
}

// ---------------------------------------------------------------------
// The three ways a session used to die, and the flag that opens the grant
// (the primary's fixes of 2026-09-06, from a real run against a strict
// gateway). Each test reproduces the failure through the built binary.
// ---------------------------------------------------------------------

/// Drives the binary as a REPL rather than with `--task`: `inputs` are piped
/// one per line, exactly as a person types them.
fn run_session_stdin(
    root: &Path,
    rollout: &Path,
    session_id: &str,
    inputs: &[&str],
    base_url: &str,
) -> std::process::Output {
    use std::process::Stdio;
    let mut command = Command::new(env!("CARGO_BIN_EXE_sterna"));
    command
        .arg("session")
        .arg("--root")
        .arg(root)
        .arg("--rollout")
        .arg(rollout)
        .arg("--session")
        .arg(session_id)
        .env("ANTHROPIC_BASE_URL", base_url)
        .env("XDG_CONFIG_HOME", root.join("global-config"))
        .env_remove("ANTHROPIC_AUTH_TOKEN")
        .env_remove("ANTHROPIC_API_KEY")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    supply_test_model(&mut command, root);
    let mut child = command.spawn().unwrap();
    {
        let stdin = child.stdin.as_mut().unwrap();
        for line in inputs {
            writeln!(stdin, "{line}").unwrap();
        }
    }
    child.wait_with_output().unwrap()
}

/// A blank line is the commonest keystroke in a REPL, and it used to compose
/// a message with no content — which a gateway enforcing the Messages shape
/// answers `400` to, killing the task. It must not reach the provider at all.
#[test]
fn a_blank_input_is_not_a_turn_and_never_reaches_the_provider() {
    let root = scratch_dir("blank-input-root");
    let rollout = root.join("rollout.jsonl");
    // One reply, because exactly one of the four inputs is a turn.
    let (base_url, bodies) = start_fake_provider(vec![ending_reply()]);

    let output = run_session_stdin(
        &root,
        &rollout,
        "sess-blank",
        &["", "   ", "\t", "hi"],
        &base_url,
    );
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let bodies = bodies.lock().unwrap();
    assert_eq!(bodies.len(), 1, "only `hi` is a turn; three blanks are not");
    let request: serde_json::Value = serde_json::from_str(&bodies[0]).unwrap();
    let messages = request["messages"].as_array().unwrap();
    for message in messages {
        let text: String = message["content"]
            .as_array()
            .unwrap()
            .iter()
            .map(|block| block["text"].as_str().unwrap_or_default())
            .collect();
        assert!(
            !text.trim().is_empty(),
            "no message may be empty; got {message}"
        );
    }
}

/// An empty reply used to be appended to the conversation and replayed on
/// every later request, so one of them turned the whole task into a stream of
/// `400`s. It must end that task instead — and the REPL must survive it, or a
/// person loses the session to one bad turn.
#[test]
fn an_empty_reply_ends_its_task_without_ending_the_session() {
    let root = scratch_dir("empty-reply-root");
    let rollout = root.join("rollout.jsonl");
    // Turn one is answered with an empty message; turn two, a fresh task, is
    // answered normally. Two requests prove the REPL lived through the first.
    let (base_url, bodies) = start_fake_provider(vec![assistant_reply(""), ending_reply()]);

    let output = run_session_stdin(
        &root,
        &rollout,
        "sess-empty-reply",
        &["first task", "second task"],
        &base_url,
    );
    assert!(
        output.status.success(),
        "the session must survive an empty reply; stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("empty reply"),
        "the person is told why the task ended; stdout: {stdout}"
    );

    let bodies = bodies.lock().unwrap();
    assert_eq!(bodies.len(), 2, "the second task was still attempted");
    // The decisive assertion: the empty reply is nowhere in the second
    // request. Appending it is what poisoned every later turn.
    let second: serde_json::Value = serde_json::from_str(&bodies[1]).unwrap();
    for message in second["messages"].as_array().unwrap() {
        let text: String = message["content"]
            .as_array()
            .unwrap()
            .iter()
            .map(|block| block["text"].as_str().unwrap_or_default())
            .collect();
        assert!(
            !text.trim().is_empty(),
            "the empty assistant turn must not be replayed; got {message}"
        );
    }
}

/// The model is told the grant in the same breath as the session starts: a
/// grant it cannot see is a grant it plans around by failing. Every command
/// line runs inside the sandbox by default, and the system block says so.
#[test]
fn the_system_block_says_every_command_line_runs_inside_the_sandbox() {
    let root = scratch_dir("sandbox-grant-root");
    fs::write(root.join("CLAUDE.md"), "PROJECT").unwrap();
    let rollout = root.join("rollout.jsonl");
    let (base_url, bodies) = start_fake_provider(vec![ending_reply()]);

    let output = run_session_stdin(&root, &rollout, "sess-grant", &["go"], &base_url);
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let bodies = bodies.lock().unwrap();
    let request: serde_json::Value = serde_json::from_str(&bodies[0]).unwrap();
    let system = request["system"][0]["text"].as_str().unwrap();
    assert!(
        system.contains("every command line\nruns unless a deny rule refuses it"),
        "system block must state the command grant; got:\n{system}"
    );
    assert!(
        system.contains("every file is readable except secrets"),
        "system block must state the read grant; got:\n{system}"
    );
    assert!(
        !system.contains("no command may be run at all"),
        "the retired no-grant sentence is gone; got:\n{system}"
    );
    assert!(
        system.contains("To change existing source, call `context` with"),
        "the model must be told how a file is changed; got:\n{system}"
    );
}

/// The sandbox level is the one permission flag. The flags it replaced are
/// refused by the parser rather than quietly accepted and ignored.
#[test]
fn the_retired_permission_flags_are_refused() {
    let root = scratch_dir("retired-permission-flags");
    for flag in [
        &["--yolo"][..],
        &["--full-access"],
        &["--dangerously-bypass-os-sandbox"],
        &["--ask-approval"],
        &["--permissions", "manual"],
        &["--mode", "explore"],
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_sterna"))
            .arg("session")
            .arg("--root")
            .arg(&root)
            .arg("--task")
            .arg("go")
            .args(flag)
            .env("XDG_CONFIG_HOME", root.join("global-config"))
            .output()
            .unwrap();
        assert!(!output.status.success(), "{flag:?} was accepted");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.contains(&format!("unexpected argument '{}'", flag[0])),
            "{flag:?}: {stderr}"
        );
    }
}

/// The platform gate on `full`: every platform Sterna has an unconfined
/// applier for accepts it and goes on to its ordinary startup. macOS is one
/// of them since 2026-09-18 — a development machine is the boundary its
/// owner has already chosen, and refusing them the level only moved the work
/// somewhere with no admission checks at all.
#[test]
fn sandbox_full_is_accepted_on_every_platform_with_an_unconfined_applier() {
    let root = scratch_dir("sandbox-full-platform");
    let output = Command::new(env!("CARGO_BIN_EXE_sterna"))
        .arg("session")
        .arg("--root")
        .arg(&root)
        .arg("--task")
        .arg("go")
        .arg("--model")
        .arg("fixture-model")
        .arg("--gateway")
        .arg(root.join("no-gateway"))
        .args(["--sandbox", "full"])
        .env("XDG_CONFIG_HOME", root.join("global-config"))
        .env("INFERENCE_GATEWAY_BIN", root.join("no-gateway"))
        // A closed loopback port: the task fails at its first request, after
        // startup has accepted the flags, and never reaches a real provider.
        .env("ANTHROPIC_BASE_URL", "http://127.0.0.1:1")
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    if cfg!(any(
        target_os = "linux",
        target_os = "windows",
        target_os = "macos"
    )) {
        assert!(
            !stderr.contains("is supported on"),
            "full is accepted on this platform: {stderr}"
        );
        assert!(
            stdout.contains("sandbox: full — "),
            "startup went on to name the level: {stdout}{stderr}"
        );
    } else {
        assert!(!output.status.success());
        assert!(
            stderr.contains("--sandbox full is supported on macOS, Linux and Windows"),
            "full is refused by platform here: {stderr}"
        );
    }
}

/// The startup lines a `full` session prints, whichever route chose it.
fn full_access_lines(said: &str) {
    assert!(
        said.contains("sandbox: full — No sandbox, nothing asks."),
        "the level line names full: {said}"
    );
    assert!(
        !said.contains("nobody is watching"),
        "full asks nothing, so an unattended full session has nothing refused for want of a person: {said}"
    );
    // The half that does not move, in the same line, by name.
    assert!(
        said.contains(
            "sandbox: full access — Sterna applies no OS confinement to the children it spawns; this machine is the boundary. The deny patterns and the never-grantable set are unchanged."
        ),
        "the unconfined half is announced with what still refuses: {said}"
    );
}

fn run_full_access_session(root: &Path, global: &Path, flag: bool) -> String {
    let mut command = Command::new(env!("CARGO_BIN_EXE_sterna"));
    command
        .arg("session")
        .arg("--root")
        .arg(root)
        .arg("--task")
        .arg("go")
        .arg("--model")
        .arg("fixture-model")
        .arg("--gateway")
        .arg(root.join("no-gateway"))
        .env("XDG_CONFIG_HOME", global)
        .env("INFERENCE_GATEWAY_BIN", root.join("no-gateway"))
        .env("ANTHROPIC_BASE_URL", "http://127.0.0.1:1");
    if flag {
        command.args(["--sandbox", "full"]);
    }
    let output = command.output().unwrap();
    format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

/// `--sandbox full` is one flag for both halves — nothing asks, and Sterna
/// confines nothing it spawns — and it says so in one line a person can act
/// on, naming what still refuses rather than only shouting.
#[test]
#[cfg(any(target_os = "linux", target_os = "windows", target_os = "macos"))]
fn sandbox_full_says_what_still_holds() {
    let root = scratch_dir("sandbox-full-flag");
    let said = run_full_access_session(&root, &root.join("global-config"), true);
    full_access_lines(&said);
}

/// **The same level, set once instead of retyped.** A saved global
/// `sandbox.level = "full"` reaches a session exactly as the flag does, and
/// is asserted against the flag's own observable rather than a parallel
/// assertion that could drift from it.
#[test]
#[cfg(any(target_os = "linux", target_os = "windows", target_os = "macos"))]
fn the_saved_sandbox_level_reaches_a_session_exactly_as_the_flag_does() {
    let root = scratch_dir("sandbox-level-setting");
    let global = root.join("global-config");
    fs::create_dir_all(global.join("sterna")).unwrap();
    fs::write(
        global.join("sterna/config.toml"),
        "[sandbox]\nlevel = \"full\"\n",
    )
    .unwrap();
    let said = run_full_access_session(&root, &global, false);
    full_access_lines(&said);
}

/// `sandbox.level` is global only: a project file travels inside a
/// repository, so its copy is ignored with a notice and the session stays on
/// the default level.
#[test]
fn a_project_files_sandbox_level_is_ignored_with_a_notice() {
    let root = scratch_dir("sandbox-level-project");
    fs::create_dir_all(root.join(".sterna")).unwrap();
    fs::write(
        root.join(".sterna/config.toml"),
        "[sandbox]\nlevel = \"full\"\n",
    )
    .unwrap();
    let said = run_full_access_session(&root, &root.join("global-config"), false);
    assert!(
        said.contains("sandbox: sandboxed — "),
        "the project's level did not apply: {said}"
    );
    assert!(
        !said.contains("sandbox: full access"),
        "a clone cannot switch confinement off: {said}"
    );
    assert!(
        said.contains("`sandbox.level` is a global setting only and was ignored"),
        "the person is told why: {said}"
    );
}

/// A saved settings file is never broken: the retired
/// `permissions.full_access = true` is migrated to `sandbox.level = "full"`
/// in the global file on first load, with one notice that says what to do.
#[test]
#[cfg(any(target_os = "linux", target_os = "windows", target_os = "macos"))]
fn a_saved_full_access_grant_is_migrated_to_the_sandbox_level() {
    let root = scratch_dir("full-access-migrated");
    let global = root.join("global-config");
    fs::create_dir_all(global.join("sterna")).unwrap();
    fs::write(
        global.join("sterna/config.toml"),
        "[permissions]\nfull_access = true\n",
    )
    .unwrap();
    let said = run_full_access_session(&root, &global, false);
    assert!(
        said.contains(
            "`permissions.full_access = true` is now `sandbox.level = \"full\"`; /sandbox changes it."
        ),
        "the migration is announced: {said}"
    );
    // The session that migrated runs on what it announced.
    full_access_lines(&said);
    let saved = fs::read_to_string(global.join("sterna/config.toml")).unwrap();
    assert!(
        !saved.contains("full_access"),
        "the old word is gone: {saved}"
    );
    let again = run_full_access_session(&root, &global, false);
    full_access_lines(&again);
    assert!(
        !again.contains("is now `sandbox.level"),
        "the notice is one-time: {again}"
    );
}

// --- /model actually selects the model ---------------------------------

/// The defect: `/model <slug>` resolved, printed `/model (BuiltIn(Model))`
/// and changed nothing, so the request still named the compiled-in default.
/// Observed 2026-09-06 while driving a real gateway — the run looked like it
/// had switched model and had not.
#[test]
fn slash_model_changes_the_slug_the_next_request_carries() {
    let root = scratch_dir("model-switch-root");
    let rollout = root.join("rollout.jsonl");
    let (base_url, bodies) = start_fake_provider(vec![ending_reply()]);

    let output = run_session_stdin(
        &root,
        &rollout,
        "sess-model-switch",
        &["/model deepseek-v4-flash", "do the thing"],
        &base_url,
    );
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let bodies = bodies.lock().unwrap();
    let request: serde_json::Value = serde_json::from_str(&bodies[0]).unwrap();
    assert_eq!(
        request["model"].as_str().unwrap(),
        "deepseek-v4-flash",
        "the request carried the default rather than the slug `/model` was given"
    );
    assert_ne!(request["model"].as_str().unwrap(), sterna::wire::MODEL);
}

/// A slash command answers between tasks, so `/model` alone must name what
/// is active without sending anything at all.
///
/// **The catalogue is this test's own**, because the tier lines are rendered
/// beside one: reading it from whatever binary happens to be installed made
/// the assertion depend on the developer's machine.
#[cfg(unix)]
#[test]
fn model_picker_names_the_active_slug_without_calling_the_provider() {
    let root = scratch_dir("model-report-root");
    let rollout = root.join("rollout.jsonl");
    let record = root.join("gateway-argv.txt");
    let (base_url, bodies) = start_fake_provider(vec![ending_reply()]);
    // Handed a loopback base URL, the session is hosted: its catalogue is the
    // gateway's, so the fake is what `INFERENCE_GATEWAY_BIN` names and nothing
    // on the developer's PATH can answer instead.
    let gateway = write_fake_gateway(&root, "fake_gateway.sh", &record, &base_url, "unused");

    let output = run_session_stdin_hosted(
        &root,
        &rollout,
        "sess-model-report",
        &["/model"],
        &base_url,
        &gateway,
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains(&format!("parent {}", sterna::wire::MODEL)),
        "`/model` did not name the active slug: {stdout}"
    );
    // And the two tiers a person would otherwise never learn they had.
    assert!(
        // Delegation is off until it is chosen, so that is what the tier
        // line says now (`workbench.md`, *Delegation policy*).
        stdout.contains("helper off") && stdout.contains("subagent off"),
        "`/model` named only the parent tier: {stdout}"
    );
    assert!(
        bodies.lock().unwrap().is_empty(),
        "`/model` reached the provider"
    );
}

/// The model you chose last is the model the next session starts on.
///
/// Without this every session begins on the built-in default, so a person
/// re-picks their model each time and the choice never means anything.
#[test]
fn a_project_starts_on_the_model_it_was_last_left_on() {
    let root = scratch_dir("model-remembered-root");
    std::fs::create_dir_all(root.join(".sterna")).unwrap();
    let rollout = root.join("rollout.jsonl");

    let (base_url, _bodies) = start_fake_provider(vec![ending_reply()]);
    let output = run_session_stdin(
        &root,
        &rollout,
        "sess-model-remember",
        &["/model claude-opus-4-8"],
        &base_url,
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("model changed to claude-opus-4-8"),
        "{stdout}"
    );
    // Saved for every project (decision 6), in the store's own spelling of
    // the person's settings folder; the project gets no file for it.
    let global = sterna::settings::Store::with_global(
        &root,
        Some(root.join("global-config").join("sterna")),
    )
    .unwrap()
    .path(sterna::settings::Scope::Global);
    let saved = std::fs::read_to_string(&global).unwrap();
    assert!(
        saved.contains("claude-opus-4-8"),
        "the choice was not written: {saved}"
    );
    assert!(
        !root.join(".sterna/config.toml").exists(),
        "a model choice wrote into the project"
    );

    // A second session, told nothing on its command line, starts there.
    let (second_url, bodies) = start_fake_provider(vec![ending_reply()]);
    run_session_stdin(
        &root,
        &rollout,
        "sess-model-remember-2",
        &["do the thing"],
        &second_url,
    );
    let bodies = bodies.lock().unwrap();
    let request: serde_json::Value = serde_json::from_str(&bodies[0]).unwrap();
    assert_eq!(
        request["model"], "claude-opus-4-8",
        "a fresh session ignored the remembered model"
    );
    assert_ne!(
        request["model"].as_str().unwrap(),
        sterna::wire::MODEL,
        "the fixture must differ from the default, or it proves nothing"
    );
}

/// A slug carrying a space is a typo, not a model, and taking it would send
/// a request that can only 404 — so it is refused and the active slug stands.
#[test]
fn a_model_slug_with_a_space_is_refused_and_the_active_slug_stands() {
    let root = scratch_dir("model-refuse-root");
    let rollout = root.join("rollout.jsonl");
    let (base_url, bodies) = start_fake_provider(vec![ending_reply()]);

    let output = run_session_stdin(
        &root,
        &rollout,
        "sess-model-refuse",
        &["/model claude sonnet 5", "do the thing"],
        &base_url,
    );
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("/model expects one model name"),
        "no refusal printed: {}",
        String::from_utf8_lossy(&output.stdout)
    );
    let bodies = bodies.lock().unwrap();
    let request: serde_json::Value = serde_json::from_str(&bodies[0]).unwrap();
    assert_eq!(
        request["model"].as_str().unwrap(),
        sterna::wire::MODEL,
        "a refused slug still changed the model"
    );
}

#[test]
fn untaken_tool_branches_are_not_reported_as_executed_calls() {
    let root = scratch_dir("untaken-branch");
    let rollout = root.join("rollout.jsonl");
    let (base, _) = start_fake_provider(vec![assistant_reply(
        "```sterna\nif (false) await bash({command: 'never-run'});\nanswer('done');\n```",
    )]);
    let output = run_session(&root, &rollout, "untaken", "do it", &base);
    assert!(
        output.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let text = String::from_utf8_lossy(&output.stdout);
    assert!(text.contains("No tool calls ran in this cell."));
    assert!(!text.contains("└─ bash"));
}

// --- a conversation that no longer fits --------------------------------

/// Rung one, and the whole point of it: the retry after an overflow carries a
/// **smaller** request, and the task goes on. Before this, a conversation
/// that outgrew the window ended the task -- the one failure every long task
/// is guaranteed to reach.
#[test]
fn an_overflow_checkpoints_the_already_projected_request_once() {
    let root = scratch_dir("overflow-compact-root");
    let rollout = root.join("rollout.jsonl");
    let turn = std::sync::atomic::AtomicUsize::new(0);
    // Two cells first, so there is an older result for compaction to drop;
    // the third request overflows, the fourth is the retry.
    let (base_url, bodies) = start_status_answering_provider(4, move |_body| {
        let n = turn.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        match n {
            0 | 1 => (200, assistant_reply("```sterna\nconst a = 1;\n```")),
            2 => (400, too_long_body()),
            _ => (200, assistant_reply("```sterna\nanswer(`${a}`);\n```")),
        }
    });

    let output = run_session(&root, &rollout, "sess-overflow", "do it", &base_url);
    assert!(
        output.status.success(),
        "the task did not survive the overflow. stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let bodies = bodies.lock().unwrap();
    assert_eq!(bodies.len(), 4, "expected a retry after the overflow");
    // **What the checkpoint rung promises is that the conversation is
    // replaced, not that the bytes always fall.** With two one-line cells the
    // checkpoint — which names the task, the plan and every live handle — can
    // be longer than the two tiny results it stands in for; this test used to
    // read as a size win only because the request that overflowed carried a
    // per-request "[Sterna task boundary]" block that the retry did not, and
    // that block is gone. So pin the mechanism: the retry says the
    // conversation was dropped, and the dropped results are not in it.
    assert!(
        bodies[3].contains("no longer fit"),
        "the retry did not carry the checkpoint: {}",
        bodies[3]
    );
    assert!(
        bodies[2].contains("[cell 1 yielded"),
        "the request that overflowed should still carry the older result: {}",
        bodies[2]
    );
    assert!(
        !bodies[3].contains("[cell 1 yielded"),
        "the retry still carried the result the checkpoint replaced: {}",
        bodies[3]
    );
    assert!(
        String::from_utf8_lossy(&output.stdout)
            .contains("provider context was replaced by a checkpoint"),
        "the compaction was not reported: {}",
        String::from_utf8_lossy(&output.stdout)
    );
    let resumed = sterna::rollout::resume(&rollout).unwrap();
    assert_eq!(
        resumed.messages.len(),
        7,
        "resume must preserve visible pre-checkpoint turns"
    );
    assert!(
        !resumed.messages.iter().any(|message| {
            message.content[0]
                .text()
                .contains("every handle below is live")
        }),
        "provider checkpoint metadata is not visible chat"
    );
    assert!(
        fs::read_to_string(&rollout).unwrap().contains("[cell 1"),
        "original evidence must remain in the append-only file"
    );
    assert_eq!(
        resumed.messages.last().unwrap().content[0].text(),
        "1",
        "the live binding survives without replay"
    );
}

#[test]
fn task_after_overflow_keeps_small_provider_context_without_stale_handles() {
    let root = scratch_dir("overflow-new-task");
    let rollout = root.join("rollout.jsonl");
    let turn = std::sync::atomic::AtomicUsize::new(0);
    let (base, bodies) = start_status_answering_provider(5, move |_| {
        let n = turn.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        match n {
            0 | 1 => (200, assistant_reply("```sterna\nconst oldHandle = 1;\n```")),
            2 => (400, too_long_body()),
            _ => (200, ending_reply()),
        }
    });
    let output = run_session_stdin(
        &root,
        &rollout,
        "overflow-new-task",
        &["old oversized task", "fresh task"],
        &base,
    );
    assert!(output.status.success());
    let bodies = bodies.lock().unwrap();
    let request: serde_json::Value = serde_json::from_str(&bodies[4]).unwrap();
    let encoded = request["messages"].to_string();
    assert!(encoded.contains("fresh task"));
    assert!(
        !encoded.contains("oldHandle"),
        "a fresh runtime must not inherit prior task handles"
    );
    assert!(encoded.contains("no prior handles are live"));
}

/// Rung two: with nothing redundant to drop -- the very first request of a
/// task -- the conversation is replaced by a checkpoint, and the retry says
/// so. This is the rung a text harness cannot take, because its results are
/// its transcript.
#[test]
fn an_overflow_with_nothing_to_compact_falls_back_to_a_checkpoint() {
    let root = scratch_dir("overflow-checkpoint-root");
    let rollout = root.join("rollout.jsonl");
    let turn = std::sync::atomic::AtomicUsize::new(0);
    let (base_url, bodies) = start_status_answering_provider(2, move |_body| {
        let n = turn.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        if n == 0 {
            (400, too_long_body())
        } else {
            (200, ending_reply())
        }
    });

    let output = run_session(
        &root,
        &rollout,
        "sess-overflow-cp",
        "summarise every caller",
        &base_url,
    );
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let bodies = bodies.lock().unwrap();
    assert_eq!(bodies.len(), 2, "expected one retry");
    let retry: serde_json::Value = serde_json::from_str(&bodies[1]).unwrap();
    let messages = retry["messages"].as_array().unwrap();
    assert_eq!(
        messages.len(),
        1,
        "the checkpoint did not replace the conversation"
    );
    let text = messages[0]["content"][0]["text"].as_str().unwrap();
    assert!(
        text.contains("every handle below is live"),
        "the checkpoint does not tell the model its objects survived: {text}"
    );
    assert!(
        text.contains("summarise every caller"),
        "the checkpoint lost the task: {text}"
    );
}

/// An ordinary 400 is not an overflow and must not be retried as one: a
/// malformed request retried unchanged is a loop, and retried after a
/// checkpoint has thrown away a conversation for nothing.
#[test]
fn a_plain_bad_request_is_reported_rather_than_compacted() {
    let root = scratch_dir("plain-400-root");
    let rollout = root.join("rollout.jsonl");
    let (base_url, bodies) = start_status_answering_provider(1, move |_body| {
        (
            400,
            serde_json::json!({"type":"error","error":{"message":"model: unknown field"}})
                .to_string(),
        )
    });

    let output = run_session(&root, &rollout, "sess-plain-400", "do it", &base_url);
    assert_eq!(
        bodies.lock().unwrap().len(),
        1,
        "a plain 400 was retried as though it were an overflow"
    );
    let all = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        all.contains("unknown field"),
        "the real error was not reported: {all}"
    );
}

#[test]
fn new_user_requests_get_truthful_model_and_runtime_boundaries() {
    let root = scratch_dir("task-boundary-root");
    let rollout = root.join("rollout.jsonl");
    let (base_url, bodies) = start_fake_provider(vec![
        assistant_reply("```sterna\nconst previous = 42;\n```"),
        assistant_reply("```sterna\nanswer(`${previous}`);\n```"),
        assistant_reply(
            "```sterna\nanswer(handles().includes('previous') ? 'leaked' : 'fresh runtime');\n```",
        ),
    ]);
    let output = run_session_stdin(
        &root,
        &rollout,
        "boundaries",
        &[
            "/model deepseek-v4-flash",
            "remember a number",
            "what are you?",
        ],
        &base_url,
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("fresh runtime"));
    let bodies = bodies.lock().unwrap();
    assert_eq!(bodies.len(), 3);
    for body in bodies.iter() {
        let request: serde_json::Value = serde_json::from_str(body).unwrap();
        assert_eq!(request["model"], "deepseek-v4-flash");
        let system = request["system"][0]["text"].as_str().unwrap();
        assert!(system.contains("You are Sterna"));
        assert!(system.contains("Configured request model: \"deepseek-v4-flash\""));
        // The runtime boundary is stated once, in the system block every
        // request shares, rather than appended to whichever user message is
        // current. Saying it per request meant un-saying it on the previous
        // one, which invalidated the whole cached prefix at every task
        // boundary (`prompt::with_task_context`).
        assert!(
            system.contains("Each new user request starts a fresh runtime"),
            "the boundary contract is in the system block: {system}"
        );
        assert!(
            system.contains("Earlier requests are\nhistory, not unfinished work."),
            "and it says what that means for earlier requests: {system}"
        );
    }
    let last: serde_json::Value = serde_json::from_str(&bodies[2]).unwrap();
    let current = last["messages"].as_array().unwrap().last().unwrap();
    assert_eq!(current["content"][0]["text"], "what are you?");
    assert!(
        current["content"].as_array().unwrap().len() == 1,
        "the person's own message reaches the provider as they typed it: {current}"
    );
    let saved = std::fs::read_to_string(&rollout).unwrap();
    assert!(
        !saved.contains("Sterna task boundary"),
        "request metadata must not rewrite the user's saved text"
    );
}

#[test]
fn prompt_write_grants_do_not_include_denied_path_components() {
    let root = scratch_dir("prompt-grants-root");
    let profile = sterna::sandbox::profile::Profile::compile(
        &root,
        Some(r#"{"permissions":{"allow":["Write(src/**)"],"deny":["Write(secrets/**)"]}}"#),
    );
    let facts = sterna::session::session_facts(&profile);
    // `session_facts` sorts and dedups only to make the list deterministic
    // and unique; the two entries are joined into a sentence for the model
    // (`render_session_facts`), so their relative ORDER carries no meaning.
    // Sorting a Windows `\\?\C:\...` path against `Write(src/**)` lands
    // differently than sorting a Unix `/tmp/...` path against it (`\` sorts
    // after `W`, `/` sorts before it), so an order-sensitive assertion here
    // was really pinned to the host's path spelling, not to the contract.
    let mut actual = facts.writable.clone();
    actual.sort();
    let mut expected = vec![
        profile.root().display().to_string(),
        "Write(src/**)".to_string(),
    ];
    expected.sort();
    assert_eq!(
        actual, expected,
        "the root is writable whether or not a rule says so, regardless of listing order"
    );
    let text = sterna::prompt::render_session_facts(&facts);
    assert!(text.contains("deny rules still apply"));
    assert!(
        !text.contains("secrets/"),
        "a denied path component is never listed as writable: {text}"
    );
}

/// One system block, two renderings of the same grant, and until 2026-09-18
/// they disagreed: the `Sandbox:` line listed the write-`allow` rules and so
/// said **nothing is writable** for an ordinary session, directly above the
/// `## Environment` block naming the project root as a writable root. A
/// dogfooding session read the restrictive half as authoritative and declined
/// to do any work in a directory it was allowed to write.
///
/// `Profile::check` is the authority both now read through.
#[test]
fn the_sandbox_line_and_the_environment_block_agree_about_what_is_writable() {
    let root = scratch_dir("writable-agreement-root");
    // No write rules at all: the case where the two derivations diverged.
    let profile = sterna::sandbox::profile::Profile::compile(&root, None);
    assert!(
        profile
            .check(
                "write",
                sterna::sandbox::profile::Access::Write,
                &root.join("new.txt"),
            )
            .is_ok(),
        "the authority admits a write under the project root"
    );

    let facts = sterna::session::session_facts(&profile);
    let sandbox_line = sterna::prompt::render_session_facts(&facts);
    let environment = sterna::manifest::Manifest::collect(&profile, &[]).render();

    assert!(
        !sandbox_line.contains("nothing is writable"),
        "a root `check` admits is not nothing: {sandbox_line}"
    );
    let shown = profile.root().display().to_string();
    assert!(
        sandbox_line.contains(&shown) && environment.contains(&shown),
        "both name the same writable root\n--- sandbox\n{sandbox_line}\n--- environment\n{environment}"
    );
}

#[test]
fn an_identity_answer_after_a_completed_task_does_not_trigger_more_execution() {
    let root = scratch_dir("natural-followup-root");
    let rollout = root.join("rollout.jsonl");
    let (base_url, bodies) = start_fake_provider(vec![
        assistant_reply("```sterna\nconst result = 42; answer('Task complete.');\n```"),
        assistant_reply(
            "I am Sterna, a coding assistant. The requested model is deepseek-v4-flash.",
        ),
        assistant_reply("```sterna\nthrow new Error('unwanted extra execution');\n```"),
    ]);
    let output = run_session_stdin(
        &root,
        &rollout,
        "natural-followup",
        &[
            "/model deepseek-v4-flash",
            "finish the task",
            "what are you?",
        ],
        &base_url,
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(bodies.lock().unwrap().len(), 2);
    assert_eq!(
        cell_lines(&rollout).len(),
        1,
        "identity answer must not run another cell"
    );
    let saved = std::fs::read_to_string(&rollout).unwrap();
    assert!(!saved.contains("unwanted extra execution"));
    assert!(!saved.contains("no program ran"));
}

#[test]
#[cfg(unix)]
fn shell_changes_survive_a_cell_error_but_never_enter_model_context() {
    let root = scratch_dir("local-diff");
    fs::create_dir_all(root.join(".sterna")).unwrap();
    fs::write(
        root.join(".sterna/config.toml"),
        "[permissions]\nallow = [\"Read(**)\", \"Write(**)\", \"Bash(echo*)\"]\n",
    )
    .unwrap();
    fs::write(root.join("example.txt"), "LOCAL_DIFF_OLD_SENTINEL\n").unwrap();
    let rollout = root.join("rollout.jsonl");
    let (base, bodies) = start_fake_provider(vec![
        assistant_reply(
            "```sterna\nawait bash({command: 'echo replacement > example.txt'});\nthrow new Error('after write');\n```",
        ),
        assistant_reply("The script failed after writing."),
    ]);
    let output = run_session(&root, &rollout, "diff", "update the file", &base);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let shown = String::from_utf8_lossy(&output.stdout);
    assert!(shown.contains("CHANGES OBSERVED"), "{shown}");
    assert!(shown.contains("-LOCAL_DIFF_OLD_SENTINEL"), "{shown}");
    assert!(shown.contains("+replacement"), "{shown}");
    for body in bodies.lock().unwrap().iter() {
        assert!(!body.contains("LOCAL_DIFF_OLD_SENTINEL"));
        assert!(!body.contains("CHANGES OBSERVED"));
    }
    let persisted = rollout_lines(&rollout);
    assert!(
        persisted.iter().any(|line| line["kind"] == "view"
            && line["view"]["changes"]
                .as_str()
                .is_some_and(|diff| diff.contains("LOCAL_DIFF_OLD_SENTINEL"))),
        "the display-only local diff must survive resume"
    );
    assert!(
        !persisted
            .iter()
            .filter(|line| line["kind"] == "turn" || line["kind"] == "cell")
            .any(|line| line.to_string().contains("LOCAL_DIFF_OLD_SENTINEL")),
        "model/protocol rows must not contain the local diff"
    );
}

#[test]
fn syntax_failed_cell_can_be_repaired_without_repeating_the_program() {
    let root = scratch_dir("cell-repair");
    let rollout = root.join("rollout.jsonl");
    let source = "const repaired = 'REPAIRED;\nanswer(repaired);";
    let edit =
        serde_json::json!({"cell":1,"replace":"'REPAIRED;","with":"'REPAIRED';"}).to_string();
    let (base, bodies) = start_fake_provider(vec![
        assistant_reply(&format!("```sterna\n{source}\n```")),
        assistant_reply(&format!("```sterna-edit\n{edit}\n```")),
    ]);
    let output = run_session(&root, &rollout, "repair", "answer", &base);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let cells = cell_lines(&rollout);
    assert_eq!(cells.len(), 2);
    assert_eq!(cells[0]["source"], source);
    assert_eq!(cells[0]["outcome"], "threw");
    assert_eq!(
        cells[1]["source"],
        "const repaired = 'REPAIRED';\nanswer(repaired);"
    );
    assert_eq!(cells[1]["outcome"], "yielded");
    assert!(String::from_utf8_lossy(&output.stdout).contains("REPAIRED"));
    let bodies = bodies.lock().unwrap();
    assert_eq!(bodies.len(), 2);
    let feedback = last_user_text(&bodies[1]);
    assert!(feedback.contains("sterna-edit"), "{feedback}");
    assert!(
        !feedback.contains(source),
        "repair feedback must not duplicate full source"
    );
}

#[test]
fn invalid_edits_keep_the_parse_target_and_do_not_create_cells() {
    let root = scratch_dir("repair-reject");
    let rollout = root.join("rollout.jsonl");
    let reply = |cell| {
        assistant_reply(&format!(
            "```sterna-edit\n{}\n```",
            serde_json::json!({"cell":cell,"replace":"'done;","with":"'done';"})
        ))
    };
    let (base, bodies) = start_fake_provider(vec![
        assistant_reply("```sterna\nconst d = 'done;\nanswer(d);\n```"),
        reply(99),
        reply(1),
    ]);
    let output = run_session(&root, &rollout, "repair-reject", "answer", &base);
    assert!(output.status.success());
    let cells = cell_lines(&rollout);
    assert_eq!(cells.len(), 2);
    assert_eq!(cells[1]["cell"], 2);
    assert_eq!(cells[1]["outcome"], "yielded");
    let bodies = bodies.lock().unwrap();
    assert!(last_user_text(&bodies[2]).contains("CellEditError"));
}

#[test]
fn runtime_syntax_error_never_offers_a_replay_and_invalid_edits_are_bounded() {
    let root = scratch_dir("repair-runtime-error");
    let rollout = root.join("rollout.jsonl");
    let edit = assistant_reply(
        "```sterna-edit\n{\"cell\":1,\"replace\":\"throw\",\"with\":\"return\"}\n```",
    );
    // One program, then edits that run nothing. Twenty-four replies are
    // scripted so a loop that ran past the bound would be served and counted.
    let mut replies = vec![assistant_reply(
        "```sterna\nconst before = 1; throw new SyntaxError('runtime');\n```",
    )];
    replies.extend(std::iter::repeat_n(edit, 24));
    let (base, bodies) = start_fake_provider(replies);
    let output = run_session(&root, &rollout, "repair-runtime", "answer", &base);
    assert!(!output.status.success());
    assert_eq!(cell_lines(&rollout).len(), 1);
    let bodies = bodies.lock().unwrap();
    assert_eq!(
        bodies.len(),
        21,
        "the program and the first edit are each new, then three windows of six \
         identical edits that run nothing, then the final-answer turn"
    );
    assert!(!last_user_text(&bodies[1]).contains("sterna-edit"));
    assert!(last_user_text(&bodies[2]).contains("No syntax-failed cell"));
}

#[test]
fn prose_without_a_native_call_is_final_and_never_executes_its_example() {
    let root = scratch_dir("completion-regression");
    fs::create_dir_all(root.join(".sterna")).unwrap();
    fs::write(
        root.join(".sterna/config.toml"),
        "[permissions]\nallow = [\"Read(**)\", \"Write(**)\"]\n",
    )
    .unwrap();
    let rollout = root.join("rollout.jsonl");
    let (base, bodies) = start_fake_provider(vec![
        assistant_reply(
            "```sterna\nawait write({path:'implementation.txt', content:'fixed'});\n```",
        ),
        assistant_reply(
            "Let me create the regression tests now.\n```bash\ntouch MUST_NOT_EXECUTE\n```",
        ),
        assistant_reply(
            "```sterna\nawait write({path:'regression.txt', content:'retained regression fixture'});\nanswer('Implementation and regression fixture written.');\n```",
        ),
    ]);
    let output = run_session(
        &root,
        &rollout,
        "completion-regression",
        "Implement the fix and add regression tests.",
        &base,
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        fs::read_to_string(root.join("implementation.txt")).unwrap(),
        "fixed"
    );
    assert!(!root.join("regression.txt").exists());
    assert!(!root.join("MUST_NOT_EXECUTE").exists());
    assert_eq!(
        cell_lines(&rollout).len(),
        1,
        "the Bash example is prose, never a cell"
    );
    let requests = bodies.lock().unwrap();
    assert_eq!(requests.len(), 2);
    assert!(String::from_utf8_lossy(&output.stdout).contains("Let me create"));
}

#[test]
fn unmarked_prose_is_a_natural_one_request_answer() {
    let root = scratch_dir("natural-prose");
    let rollout = root.join("rollout.jsonl");
    let (base, bodies) = start_fake_provider(vec![assistant_reply("Implemented and tested.")]);
    let output = run_session(
        &root,
        &rollout,
        "unfinished",
        "Make the requested change",
        &base,
    );
    assert!(output.status.success());
    assert_eq!(bodies.lock().unwrap().len(), 1);
    assert!(String::from_utf8_lossy(&output.stdout).contains("Implemented and tested."));
    assert!(cell_lines(&rollout).is_empty());
}

#[test]
fn a_prose_answer_is_displayed_naturally_without_a_cell() {
    let root = scratch_dir("explicit-prose");
    let rollout = root.join("rollout.jsonl");
    let (base, bodies) = start_fake_provider(vec![assistant_reply("I am Sterna.")]);
    let output = run_session(&root, &rollout, "explicit-prose", "What are you?", &base);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(bodies.lock().unwrap().len(), 1);
    assert!(cell_lines(&rollout).is_empty());
    let shown = String::from_utf8_lossy(&output.stdout);
    assert!(shown.contains("I am Sterna."));
    assert!(
        fs::read_to_string(rollout)
            .unwrap()
            .contains("I am Sterna."),
        "raw evidence retains the actual response"
    );
}

#[test]
fn outgoing_history_sends_each_result_as_the_model_read_it() {
    let root = scratch_dir("state-history");
    let rollout = root.join("rollout.jsonl");
    let (base, bodies) = start_fake_provider(vec![
        assistant_reply(
            "```sterna\nconst saved = 1; console.log('observation\\n\\n## Handles\\nliteral stdout heading'); throw new Error('preserved failure');\n```",
        ),
        assistant_reply("```sterna\nconst saved = 2; const fresh = 3;\n```"),
        assistant_reply("```sterna\nanswer(`${saved + fresh}`);\n```"),
    ]);
    let output = run_session(&root, &rollout, "state-history", "Complete the task", &base);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let requests = bodies.lock().unwrap();
    assert_eq!(requests.len(), 3);
    let request: serde_json::Value = serde_json::from_str(&requests[2]).unwrap();
    let old = request["messages"][2]["content"][0]["text"]
        .as_str()
        .unwrap();
    assert!(old.contains("preserved failure"));
    assert!(old.contains("observation\n\n## Handles\nliteral stdout heading"));
    assert!(
        old.contains("## Usage"),
        "an earlier result goes back as the model read it: {old}"
    );
    assert!(last_user_text(&requests[2]).contains("fresh"));
    let evidence = fs::read_to_string(rollout).unwrap();
    assert!(
        evidence.contains("## Usage"),
        "full original feedback stays in the rollout"
    );
}

#[test]
fn task_orientation_and_instructions_refresh_without_widening_live_permissions() {
    let root = scratch_dir("task-orientation-refresh");
    fs::write(root.join("CLAUDE.md"), "GUIDANCE_VERSION_ONE").unwrap();
    fs::write(root.join("AGENTS.md"), "ROOT_AGENTS_GUIDANCE").unwrap();
    fs::write(root.join("Cargo.toml"), "[package]\nname='fixture'\n").unwrap();
    let changed_root = root.clone();
    let (base, bodies) = start_answering_provider(2, move |body| {
        if last_user_text(body) == "first task" {
            fs::write(changed_root.join("CLAUDE.md"), "GUIDANCE_VERSION_TWO").unwrap();
            fs::create_dir_all(changed_root.join(".claude")).unwrap();
            fs::write(
                changed_root.join(".claude/settings.json"),
                r#"{"permissions":{"allow":["Write(widened/**)"]}}"#,
            )
            .unwrap();
            fs::create_dir_all(changed_root.join(".sterna")).unwrap();
            fs::write(
                changed_root.join(".sterna/config.toml"),
                "[permissions]\nallow = [\"Write(widened/**)\"]\n",
            )
            .unwrap();
        }
        ending_reply()
    });
    let log = root.join("rollout.jsonl");
    let output = run_session_stdin(
        &root,
        &log,
        "orientation",
        &["first task", "second task"],
        &base,
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let bodies = bodies.lock().unwrap();
    let first: serde_json::Value = serde_json::from_str(&bodies[0]).unwrap();
    let second: serde_json::Value = serde_json::from_str(&bodies[1]).unwrap();
    let first = first["system"][0]["text"].as_str().unwrap();
    let second = second["system"][0]["text"].as_str().unwrap();
    assert!(first.contains("GUIDANCE_VERSION_ONE"));
    assert!(second.contains("GUIDANCE_VERSION_TWO"));
    assert!(!second.contains("GUIDANCE_VERSION_ONE"));
    for system in [first, second] {
        assert!(system.contains("ROOT_AGENTS_GUIDANCE"));
        assert!(system.contains("CLAUDE.md") && system.contains("AGENTS.md"));
        assert!(system.contains("## Environment orientation"));
        assert!(system.contains(std::env::consts::OS));
        assert!(system.contains("Cargo.toml"));
        assert!(
            !system.contains("widened"),
            "guidance reload widened frozen permissions: {system}"
        );
    }
    // The grant the model is told is the one frozen at session start.
    let sandbox_line = |system: &str| {
        let start = system.find("Sandbox: ").expect("a Sandbox: line");
        let end = start
            + system[start..]
                .find("network:")
                .expect("its network clause");
        system[start..end].to_string()
    };
    assert_eq!(
        sandbox_line(first),
        sandbox_line(second),
        "guidance reload changed the stated grant"
    );
    let resumed = sterna::rollout::resume(&log).unwrap();
    assert!(resumed.system.contains("GUIDANCE_VERSION_TWO"));
    assert!(!resumed.system.contains("GUIDANCE_VERSION_ONE"));
}

/// A later task in the same session re-sends the earlier conversation as an
/// exact prefix -- the system block included, though the clock moved on --
/// under one cache key, so the provider reads it from its cache.
#[test]
fn a_second_task_resends_the_first_as_an_unchanged_prefix() {
    let root = scratch_dir("stable-session-prefix");
    fs::write(root.join("CLAUDE.md"), "STABLE_GUIDANCE").unwrap();
    let (base, bodies) = start_answering_provider(2, move |body| {
        if last_user_text(body) == "first task" {
            std::thread::sleep(std::time::Duration::from_millis(1100));
        }
        ending_reply()
    });
    let output = run_session_stdin(
        &root,
        &root.join("rollout.jsonl"),
        "stable-prefix",
        &["first task", "second task"],
        &base,
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let bodies = bodies.lock().unwrap();
    let first: serde_json::Value = serde_json::from_str(&bodies[0]).unwrap();
    let second: serde_json::Value = serde_json::from_str(&bodies[1]).unwrap();
    assert_eq!(first["system"], second["system"]);
    let (earlier, later) = (without_cache_marks(&first), without_cache_marks(&second));
    let (earlier, later) = (
        earlier["messages"].as_array().unwrap(),
        later["messages"].as_array().unwrap(),
    );
    assert_eq!(
        earlier[..],
        later[..earlier.len()],
        "the first task's messages are not a prefix of the second's"
    );
    assert_eq!(first["metadata"]["user_id"], "stable-prefix");
    assert_eq!(second["metadata"], first["metadata"]);
}

#[test]
fn resumed_tasks_use_current_instructions_instead_of_the_saved_system_prompt() {
    let root = scratch_dir("resumed-guidance");
    fs::write(root.join("AGENTS.md"), "RESUME_OLD_GUIDANCE").unwrap();
    let log = root.join("rollout.jsonl");
    let (url, _) = start_fake_provider(vec![ending_reply()]);
    assert!(
        run_session(&root, &log, "first", "first", &url)
            .status
            .success()
    );
    fs::write(root.join("AGENTS.md"), "RESUME_NEW_GUIDANCE").unwrap();
    let (url, bodies) = start_fake_provider(vec![ending_reply()]);
    assert!(
        run_session(&root, &log, "second", "second", &url)
            .status
            .success()
    );
    let bodies = bodies.lock().unwrap();
    let request: serde_json::Value = serde_json::from_str(&bodies[0]).unwrap();
    let system = request["system"][0]["text"].as_str().unwrap();
    assert!(system.contains("RESUME_NEW_GUIDANCE"));
    assert!(!system.contains("RESUME_OLD_GUIDANCE"));
}

#[test]
fn environment_snapshot_does_not_change_between_inferences_in_one_task() {
    let root = scratch_dir("stable-task-context");
    let (url, bodies) = start_fake_provider(vec![
        native_cell_reply("first", "const x = 1;"),
        native_cell_reply("second", "answer(`${x}`);"),
    ]);
    let output = run_session(
        &root,
        &root.join("rollout.jsonl"),
        "stable-context",
        "work",
        &url,
    );
    assert!(output.status.success());
    let bodies = bodies.lock().unwrap();
    assert_eq!(bodies.len(), 2);
    let first: serde_json::Value = serde_json::from_str(&bodies[0]).unwrap();
    let second: serde_json::Value = serde_json::from_str(&bodies[1]).unwrap();
    assert_eq!(first["system"], second["system"]);
}

#[test]
fn nested_instructions_reach_the_provider_before_a_write_can_execute() {
    let root = scratch_dir("nested-provider-boundary");
    fs::create_dir_all(root.join("nested")).unwrap();
    fs::create_dir_all(root.join(".sterna")).unwrap();
    fs::write(
        root.join(".sterna/config.toml"),
        "[permissions]\nallow = [\"Read(**)\", \"Write(**)\"]\n",
    )
    .unwrap();
    fs::write(root.join("AGENTS.md"), "ROOT_GUIDANCE").unwrap();
    fs::write(
        root.join("nested/AGENTS.md"),
        "NESTED_GUIDANCE: write the word verified.",
    )
    .unwrap();
    let target = root.join("nested/result.txt");
    let checked_target = target.clone();
    let step = std::sync::atomic::AtomicUsize::new(0);
    let (url, bodies) = start_answering_provider(2, move |body| {
        let request: serde_json::Value = serde_json::from_str(body).unwrap();
        let system = request["system"][0]["text"].as_str().unwrap();
        assert!(
            !checked_target.exists(),
            "write happened before guidance delivery"
        );
        match step.fetch_add(1, std::sync::atomic::Ordering::SeqCst) {
            0 => {
                assert!(system.contains("ROOT_GUIDANCE"));
                assert!(!system.contains("NESTED_GUIDANCE:"));
                native_cell_reply(
                    "blocked",
                    "await write({path: 'nested/result.txt', content: 'too early'}); answer('wrong');",
                )
            }
            _ => {
                assert!(system.contains("NESTED_GUIDANCE: write the word verified."));
                let result = request["messages"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .flat_map(|m| m["content"].as_array().into_iter().flatten())
                    .find(|b| b["type"] == "tool_result" && b["tool_use_id"] == "blocked")
                    .unwrap();
                assert!(result["content"].as_str().unwrap().contains("did not run"));
                native_cell_reply(
                    "allowed",
                    "await write({path: 'nested/result.txt', content: 'verified'}); answer('done');",
                )
            }
        }
    });
    let log = root.join("rollout.jsonl");
    let output = run_session(&root, &log, "nested-instructions", "write result", &url);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(bodies.lock().unwrap().len(), 2);
    assert_eq!(fs::read_to_string(target).unwrap(), "verified");
    let resumed = sterna::rollout::resume(&log).unwrap();
    assert!(resumed.system.contains("NESTED_GUIDANCE:"));
    let rows = rollout_lines(&log);
    let calls: usize = rows
        .iter()
        .filter(|r| r["kind"] == "cell")
        .map(|r| r["calls"].as_array().map_or(0, Vec::len))
        .sum();
    assert_eq!(calls, 1, "only the post-guidance write should be recorded");
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn standing_handler_drains_a_future_batch_without_an_extra_model_request() {
    let root = scratch_dir("standing-handler-drain");
    fs::create_dir_all(root.join(".sterna")).unwrap();
    fs::write(
        root.join(".sterna/config.toml"),
        "[permissions]\nallow = [\"Bash(echo*)\"]\n",
    )
    .unwrap();
    let rollout = root.join("rollout.jsonl");
    let turn = std::sync::atomic::AtomicUsize::new(0);
    let (url, requests) = start_answering_provider(2, move |_| {
        match turn.fetch_add(1, std::sync::atomic::Ordering::SeqCst) {
            0 => assistant_reply(
                "```sterna\nlet seen = 0; const noise = on({kind:'bg.done'}, 'seen += batch.rest().length; batch.ack(batch.rest().map(e => e.id));'); const job = bg.run('echo handled');\n```",
            ),
            _ => {
                // Let the real background process finish while the model request is in flight.
                thread::sleep(std::time::Duration::from_millis(800));
                assistant_reply("```sterna\nanswer(`seen=${seen}; remaining=${batch.n}`);\n```")
            }
        }
    });
    let output = run_session(
        &root,
        &rollout,
        "standing-handler-drain",
        "handle background noise",
        &url,
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        requests.lock().unwrap().len(),
        2,
        "a drained batch generated inference"
    );
    let lines: Vec<serde_json::Value> = fs::read_to_string(&rollout)
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    let cells: Vec<_> = lines
        .iter()
        .filter(|l| l["kind"] == "cell" && l.get("handler").is_none())
        .collect();
    let handlers: Vec<_> = lines.iter().filter(|l| l["handler"] == "noise").collect();
    assert_eq!(handlers.len(), 1, "{lines:?}");
    assert_eq!(cells.len(), 2);
    assert_eq!(cells[1]["cell"], 2);
    assert!(
        lines.iter().any(|l| l["text"] == "seen=1; remaining=0"),
        "{lines:?}"
    );
    fs::remove_dir_all(root).unwrap();
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn disabled_handler_notice_reaches_the_first_preview_once_before_next_inference() {
    let root = scratch_dir("standing-handler-notice");
    fs::create_dir_all(root.join(".sterna")).unwrap();
    fs::write(
        root.join(".sterna/config.toml"),
        "[permissions]\nallow = [\"Bash(echo*)\"]\n",
    )
    .unwrap();
    let rollout = root.join("rollout.jsonl");
    let turn = std::sync::atomic::AtomicUsize::new(0);
    let (url, requests) = start_answering_provider(3, move |_| {
        match turn.fetch_add(1, std::sync::atomic::Ordering::SeqCst) {
            0 => assistant_reply(
                "```sterna\nconst broken = on({kind:'bg.done'}, 'throw new Error(\"private error body\");'); const job = bg.run('echo notice');\n```",
            ),
            1 => {
                thread::sleep(std::time::Duration::from_millis(800));
                assistant_reply("```sterna\nconst continued = 1;\n```")
            }
            _ => assistant_reply("```sterna\nanswer('done');\n```"),
        }
    });
    let output = run_session(
        &root,
        &rollout,
        "standing-handler-notice",
        "surface handler failure",
        &url,
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let requests = requests.lock().unwrap();
    assert!(!requests[1].contains("handler broken disabled: Error"));
    let last: serde_json::Value = serde_json::from_str(&requests[2]).unwrap();
    let messages = last["messages"].as_array().unwrap();
    let feedback = messages.last().unwrap().to_string();
    assert_eq!(
        feedback.matches("handler broken disabled: Error").count(),
        1,
        "{last}"
    );
    assert!(
        feedback.contains("Events.Batch"),
        "notice did not share the first batch preview: {last}"
    );
    assert!(!feedback.contains("private error body"));
    let handlers = fs::read_to_string(&rollout)
        .unwrap()
        .lines()
        .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
        .filter(|l| l["handler"] == "broken")
        .count();
    assert_eq!(handlers, 1, "disabled handler retried on next delivery");
    fs::remove_dir_all(root).unwrap();
}

// ---------------------------------------------------------------------
// Preflight -- SCOUT once per task, before the model's first turn.
// `little-helpers.md`, *Pushed -- the preflight hook*.
// ---------------------------------------------------------------------

/// `[helpers] model` is the whole configuration a preflight needs: unset is
/// off, exactly as `[supervisor] model` unset is.
fn write_helpers_sterna_toml(root: &Path, model: &str) {
    fs::create_dir_all(root.join(".sterna")).unwrap();
    fs::write(
        root.join(".sterna/config.toml"),
        format!("[helpers]\nacceptance_list = false\nmodel = \"{model}\"\npreflight = true\npreflight_scope = \"always\"\n"),
    )
    .unwrap();
}

/// The system block of one recorded request body.
fn request_system(body: &str) -> String {
    let request: serde_json::Value = serde_json::from_str(body).unwrap();
    request["system"][0]["text"].as_str().unwrap().to_string()
}

/// The task's own message as sent: the request, then the task context Sterna
/// carries after it (mode, plan, preflight, acceptance), which the system
/// prompt no longer holds so it stays the same from task to task.
fn request_task_message(body: &str) -> String {
    let request: serde_json::Value = serde_json::from_str(body).unwrap();
    let task = request["messages"]
        .as_array()
        .unwrap()
        .iter()
        .rfind(|message| message["role"] == "user")
        .unwrap();
    task["content"]
        .as_array()
        .map(|blocks| {
            blocks
                .iter()
                .filter_map(|b| b["text"].as_str())
                .collect::<Vec<_>>()
                .join("\n\n")
        })
        .unwrap_or_else(|| task["content"].as_str().unwrap_or_default().to_string())
}

/// **With helpers unconfigured nothing changes at all.** No request is sent
/// on the user's behalf and the system block is the one
/// `the_system_block_is_render_systems_own_bytes` pins, byte for byte up to
/// the orientation snapshot this test does not regenerate.
///
/// The mutation this kills: a preflight that runs whenever the roster has a
/// `Preflight` spec. `helpers::preflight` itself checks no configuration --
/// it cannot, it is handed a model -- so the fail-closed direction lives at
/// this call site and nowhere else.
#[test]
fn preflight_does_not_fire_with_helpers_unconfigured() {
    let root = scratch_dir("preflight-off-root");
    fs::write(root.join("CLAUDE.md"), "PROJECT-INSTRUCTION-ONE").unwrap();
    let rollout = root.join("rollout.jsonl");
    let absent = root.join("no-such-glasshouse");
    let (base_url, bodies) = start_fake_provider(vec![ending_reply()]);

    // Absent gateway as well: this test also compares the system block byte
    // for byte, and the subagent roster is the gateway's answer.
    let output = run_session_with_gateway(
        &root,
        &rollout,
        "sess-preflight-off",
        "where is the retry budget applied",
        &base_url,
        Some(&absent),
    );
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let bodies = bodies.lock().unwrap();
    assert_eq!(
        bodies.len(),
        1,
        "an unconfigured helper must send no request of its own"
    );

    let expected = expected_system_block(&root);
    let saved = sterna::rollout::resume(&rollout).unwrap().system;
    let (prefix, orientation) = saved.split_once("\n\n## Environment orientation").unwrap();
    assert_eq!(prefix, expected);
    assert!(
        !orientation.contains("## Request (verbatim"),
        "no preflight block may be appended: {orientation}"
    );
}

#[test]
fn a_configured_helper_model_does_not_enable_preflight_by_itself() {
    let root = scratch_dir("preflight-default-off-root");
    fs::create_dir_all(root.join(".sterna")).unwrap();
    fs::write(
        root.join(".sterna/config.toml"),
        "[helpers]\nacceptance_list = false\nmodel = \"helper-tier\"\n",
    )
    .unwrap();
    let rollout = root.join("rollout.jsonl");
    let (base_url, bodies) = start_fake_provider(vec![ending_reply()]);

    let output = run_session(
        &root,
        &rollout,
        "sess-preflight-default-off",
        "find where the retry budget is applied",
        &base_url,
    );
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let bodies = bodies.lock().unwrap();
    assert_eq!(
        bodies.len(),
        1,
        "a helper model alone must keep helpers demand-driven"
    );
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&bodies[0]).unwrap()["model"],
        sterna::wire::MODEL
    );
    fs::remove_dir_all(root).unwrap();
}

/// The measured rule of `little-helpers.md`: **the verbatim request is never
/// replaced.** A paraphrase that adds a clause becomes the criterion the
/// model solves for, so the request is the first section and is the bytes the
/// user typed; the scout's reading is advisory under it, what it named is
/// served, and its own report stays out of the prompt behind a one-line
/// record.
#[test]
fn preflight_serves_the_scouts_files_under_the_verbatim_request() {
    let root = scratch_dir("preflight-on-root");
    write_helpers_sterna_toml(&root, "helper-tier");
    fs::write(root.join("haystack.rs"), "// the needle is on this line\n").unwrap();
    let rollout = root.join("rollout.jsonl");
    let task = "find the needle, and do not change the colour";
    let (base_url, bodies) = start_fake_provider(vec![
        assistant_reply("```sterna\nanswer(\"haystack.rs:1 where the needle is\");\n```"),
        ending_reply(),
    ]);

    let output = run_session(&root, &rollout, "sess-preflight-on", task, &base_url);
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let rendered = String::from_utf8_lossy(&output.stdout);
    assert!(
        rendered.contains("PREFLIGHT · SCOUT") && rendered.contains("haystack.rs"),
        "the resolved Scout must survive into the task's later/final notebook frame: {rendered}"
    );

    let bodies = bodies.lock().unwrap();
    assert_eq!(
        bodies.len(),
        2,
        "one scout request, then the task's own turn"
    );
    assert!(
        request_system(&bodies[0]).contains("Answer with spans only"),
        "the first request must be the scout's, carrying its preamble"
    );

    let system = request_task_message(&bodies[1]);
    assert!(
        system.contains(&format!("## Request (verbatim, authoritative)\n{task}")),
        "the request must appear unmodified and first: {system}"
    );
    assert!(
        system.contains("## Served in full (1)"),
        "the file the scout named must be served: {system}"
    );
    assert!(
        system.contains("// the needle is on this line"),
        "served means its contents, not its path: {system}"
    );
    assert!(
        system.contains("## Scouting record"),
        "the record is one line, and it is present: {system}"
    );
    assert!(
        !system.contains("Could not determine"),
        "the scout's uncertainty is not a section: {system}"
    );
}

/// **A failed preflight is never fatal.** The scout's reply is empty, which
/// its own loop reports as a failure; the task then runs on exactly as it
/// does with helpers off, and gets the next scripted reply.
///
/// Before the preflight existed this run failed: the empty reply was the
/// *task's* first turn, and an empty reply ends a task with an error.
#[test]
fn a_failed_preflight_still_runs_the_task() {
    let root = scratch_dir("preflight-failed-root");
    write_helpers_sterna_toml(&root, "helper-tier");
    let rollout = root.join("rollout.jsonl");
    let (base_url, bodies) = start_fake_provider(vec![assistant_reply(""), ending_reply()]);

    let output = run_session(
        &root,
        &rollout,
        "sess-preflight-failed",
        "find the needle, and do not change the colour",
        &base_url,
    );
    assert!(
        output.status.success(),
        "a failed preflight must leave the session runnable; stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let bodies = bodies.lock().unwrap();
    assert_eq!(bodies.len(), 2, "the scout tried once, the task ran once");
    let system = request_task_message(&bodies[1]);
    assert!(
        !system.contains("## Request (verbatim"),
        "a failed preflight appends nothing: {system}"
    );
}

// ---------------------------------------------------------------------
// Standalone: sterna, the inference gateway it starts, and no Glasshouse
// anywhere in the serving path (the milestone of 2026-09-10).
// ---------------------------------------------------------------------

/// A fake `inference-gateway`.
///
/// `serve` prints the one ready line the contract fixes -- pointing at
/// `listening`, this file's own fake provider -- and then stays alive until
/// its stdin closes, as a real gateway does. Do not replace the read loop with
/// `sleep`: that makes every unrelated caller pay the production three-second
/// emergency kill grace. The dedicated shutdown tests provide stubborn
/// children when that fallback is the behavior under test. **Every**
/// invocation, `serve` included, appends its own argv to `record`, so a test
/// can prove both what was asked of the gateway and what was not.
#[cfg(unix)]
fn write_fake_gateway(
    dir: &Path,
    name: &str,
    record: &Path,
    listening: &str,
    token: &str,
) -> PathBuf {
    const SCRIPT: &str = r#"#!/bin/sh
echo "$@" >> "@RECORD@"
case "$1" in
  serve)
    echo '{"listening":"@LISTENING@","token":"@TOKEN@"}'
    while read -r _line; do :; done
    ;;
  entitlements)
    echo '{"version":1,"accounts":[{"account":"work@example.com","provider":"anthropic","models":["claude-opus-5"],"scope":"user","selectable":true,"authenticated":'"${FAKE_GATEWAY_AUTHENTICATED:-false}"',"connect_with":"anthropic"}]}'
    ;;
  subscriptions)
    echo '{"state":"connected","account":"work@example.com"}'
    ;;
  credentials)
    case "$2" in
      list)
        echo '{"version":1,"providers":[{"provider":"anthropic","variable":"ANTHROPIC_API_KEY","source":null,"native_store":"absent"}]}'
        ;;
      set)
        cat > "@RECORD@.stdin"
        echo '{"provider":"'"$3"'","variable":"ANTHROPIC_API_KEY","stored_in":"/dev/null/credentials.toml"}'
        ;;
    esac
    ;;
esac
exit 0
"#;
    let body = SCRIPT
        .replace("@RECORD@", &record.display().to_string())
        .replace("@LISTENING@", listening)
        .replace("@TOKEN@", token);
    write_script(dir, name, &body)
}

/// A provider endpoint that records each request's **headers** as well as its
/// body.
///
/// The bearer a started gateway minted exists nowhere but on the wire: no
/// other fixture in this file can see whether it arrived.
#[cfg(unix)]
#[allow(clippy::type_complexity)]
fn start_header_recording_provider(
    replies: Vec<String>,
) -> (String, Arc<Mutex<Vec<(Vec<String>, String)>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let seen: Arc<Mutex<Vec<(Vec<String>, String)>>> = Arc::new(Mutex::new(Vec::new()));
    let seen_thread = Arc::clone(&seen);

    thread::spawn(move || {
        for reply in replies {
            let Ok((mut stream, _)) = listener.accept() else {
                return;
            };
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut headers: Vec<String> = Vec::new();
            let mut content_length = 0usize;
            loop {
                let mut line = String::new();
                if reader.read_line(&mut line).unwrap_or(0) == 0 {
                    return;
                }
                if line == "\r\n" || line == "\n" {
                    break;
                }
                if let Some(rest) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                    content_length = rest.trim().parse().unwrap_or(0);
                }
                headers.push(line.trim_end().to_string());
            }
            let mut body = vec![0u8; content_length];
            if reader.read_exact(&mut body).is_err() {
                return;
            }
            seen_thread
                .lock()
                .unwrap()
                .push((headers, String::from_utf8_lossy(&body).into_owned()));

            let bytes = reply.as_bytes();
            let head = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
                bytes.len()
            );
            let _ = stream.write_all(head.as_bytes());
            let _ = stream.write_all(bytes);
            let _ = stream.flush();
        }
    });

    (format!("http://127.0.0.1:{port}"), seen)
}

/// [`run_session_stdin`], driving the gateway rather than Glasshouse.
#[cfg(unix)]
fn run_session_stdin_with_gateway(
    root: &Path,
    rollout: &Path,
    session_id: &str,
    inputs: &[&str],
    base_url: Option<&str>,
    gateway: &Path,
) -> std::process::Output {
    use std::process::Stdio;
    let mut command = Command::new(env!("CARGO_BIN_EXE_sterna"));
    command
        .arg("session")
        .arg("--root")
        .arg(root)
        .arg("--rollout")
        .arg(rollout)
        .arg("--session")
        .arg(session_id)
        .arg("--gateway")
        .arg(gateway)
        .env_remove("ANTHROPIC_BASE_URL")
        .env_remove("ANTHROPIC_AUTH_TOKEN")
        .env_remove("ANTHROPIC_API_KEY")
        .env("XDG_CONFIG_HOME", root.join("global-config"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    supply_test_model(&mut command, root);
    if let Some(base_url) = base_url {
        command.env("ANTHROPIC_BASE_URL", base_url);
    }
    let mut child = command.spawn().unwrap();
    {
        let stdin = child.stdin.as_mut().unwrap();
        for line in inputs {
            writeln!(stdin, "{line}").unwrap();
        }
    }
    child.wait_with_output().unwrap()
}

/// [`run_session_stdin`] for a **hosted** session whose account controls must
/// reach a gateway of this test's own choosing.
///
/// `PATH` is emptied of everything but the system directories and
/// `INFERENCE_GATEWAY_BIN` names the fake, so the gateway a hosted control
/// resolves is never whichever one the developer has installed.
#[cfg(unix)]
fn run_session_stdin_hosted(
    root: &Path,
    rollout: &Path,
    session_id: &str,
    inputs: &[&str],
    base_url: &str,
    gateway_bin: &Path,
) -> std::process::Output {
    use std::process::Stdio;
    let mut command = Command::new(env!("CARGO_BIN_EXE_sterna"));
    command
        .arg("session")
        .arg("--root")
        .arg(root)
        .arg("--rollout")
        .arg(rollout)
        .arg("--session")
        .arg(session_id)
        .env("ANTHROPIC_BASE_URL", base_url)
        .env_remove("ANTHROPIC_AUTH_TOKEN")
        .env_remove("ANTHROPIC_API_KEY")
        .env("XDG_CONFIG_HOME", root.join("global-config"))
        .env("PATH", "/usr/bin:/bin")
        .env("INFERENCE_GATEWAY_BIN", gateway_bin)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    supply_test_model(&mut command, root);
    let mut child = command.spawn().unwrap();
    {
        let stdin = child.stdin.as_mut().unwrap();
        for line in inputs {
            writeln!(stdin, "{line}").unwrap();
        }
    }
    child.wait_with_output().unwrap()
}

/// **The standalone proof.** No `ANTHROPIC_BASE_URL`, no `--glasshouse`, and a
/// `PATH` carrying no `glasshouse`: sterna starts the gateway itself, reads its
/// ready line, sends the turn to the URL that line named, and carries the
/// bearer that line minted.
///
/// Every one of those four is asserted, because any one of them passing alone
/// would still leave sterna dependent on something it was not handed.
#[cfg(unix)]
#[test]
fn a_session_runs_standalone_against_a_gateway_it_started() {
    let root = scratch_dir("standalone-gateway-root");
    let rollout = root.join("rollout.jsonl");
    let record = root.join("gateway-argv.txt");
    let (provider_url, requests) = start_header_recording_provider(vec![ending_reply()]);
    let gateway = write_fake_gateway(
        &root,
        "fake_gateway.sh",
        &record,
        &provider_url,
        "gw-bearer-42",
    );

    let output = Command::new(env!("CARGO_BIN_EXE_sterna"))
        .arg("session")
        .arg("--root")
        .arg(&root)
        .arg("--rollout")
        .arg(&rollout)
        .arg("--session")
        .arg("sess-standalone")
        .arg("--model")
        .arg(sterna::wire::MODEL)
        .arg("--task")
        .arg("hi")
        .arg("--gateway")
        .arg(&gateway)
        // Nothing tells sterna where to send a request, or with what.
        .env_remove("ANTHROPIC_BASE_URL")
        .env_remove("ANTHROPIC_AUTH_TOKEN")
        .env_remove("ANTHROPIC_API_KEY")
        .env("XDG_CONFIG_HOME", root.join("global-config"))
        // No `glasshouse` is reachable, and none is passed: a session that
        // needed one could not finish from here.
        .env("PATH", "/usr/bin:/bin")
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "a session with no Glasshouse must still run: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        !root.join(".glasshouse").exists(),
        "the session must not have required Glasshouse project state"
    );

    let seen = fs::read_to_string(&record).unwrap();
    assert!(
        seen.lines().any(|line| line.starts_with("serve ")),
        "sterna must have started the gateway itself: {seen}"
    );

    let requests = requests.lock().unwrap();
    assert_eq!(
        requests.len(),
        1,
        "the turn must reach the gateway sterna started, not a provider"
    );
    let (headers, body) = &requests[0];
    assert!(
        headers
            .iter()
            .any(|header| header.eq_ignore_ascii_case("authorization: Bearer gw-bearer-42")),
        "the bearer from the ready line must be on the request: {headers:?}"
    );
    let request: serde_json::Value = serde_json::from_str(body).unwrap();
    assert_eq!(
        request["messages"][0]["content"][0]["text"], "hi",
        "the request the gateway received must be this session's turn"
    );
}

/// **The standalone proof, with the shipped gateway.** The test above pins
/// the protocol against a fake; this one runs the real `inference-gateway`
/// built beside `sterna`, configured through `INFERENCE_GATEWAY_CONFIG` at a
/// provider this test controls, with no `glasshouse` on `PATH` and nothing
/// in the environment saying where to send a request. The provider must see
/// the account's own credential — which only the gateway holds — and the
/// turn must run to completion.
#[cfg(unix)]
#[test]
fn a_session_completes_a_turn_through_the_real_gateway_with_no_glasshouse() {
    const PROVIDER_KEY: &str = "provider-key-only-the-gateway-holds";

    let gateway = real_gateway_binary();
    let root = scratch_dir("real-gateway-root");
    let rollout = root.join("rollout.jsonl");
    let (provider_url, requests) = start_header_recording_provider(vec![ending_reply()]);
    let config = root.join("gateway.toml");
    fs::write(
        &config,
        format!(
            r#"
[providers.fixture]
base_url = "{provider_url}"
protocol = "anthropic-messages"
credential_env = ["STERNA_E2E_PROVIDER_KEY"]

[accounts.local]
kind = "api-key"
provider = "fixture"
credential = {{ env = "STERNA_E2E_PROVIDER_KEY" }}
"#
        ),
    )
    .unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_sterna"))
        .arg("session")
        .arg("--root")
        .arg(&root)
        .arg("--rollout")
        .arg(&rollout)
        .arg("--session")
        .arg("sess-real-gateway")
        .arg("--model")
        .arg(sterna::wire::MODEL)
        .arg("--task")
        .arg("hi")
        .arg("--gateway")
        .arg(&gateway)
        .env_remove("ANTHROPIC_BASE_URL")
        .env_remove("ANTHROPIC_AUTH_TOKEN")
        .env_remove("ANTHROPIC_API_KEY")
        .env("XDG_CONFIG_HOME", root.join("global-config"))
        // No `glasshouse` anywhere; the gateway reads this test's catalogue
        // and keeps its state under this test's root, never the user's.
        .env("PATH", "/usr/bin:/bin")
        .env("INFERENCE_GATEWAY_CONFIG", &config)
        .env("INFERENCE_GATEWAY_DATA_DIR", root.join("gateway-data"))
        .env("STERNA_E2E_PROVIDER_KEY", PROVIDER_KEY)
        .output()
        .unwrap();

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "a session with the real gateway and no Glasshouse must complete: {stderr}"
    );
    assert!(
        !root.join(".glasshouse").exists(),
        "the session must not have required Glasshouse project state"
    );

    let requests = requests.lock().unwrap();
    assert_eq!(
        requests.len(),
        1,
        "the turn must reach the provider through the gateway sterna started"
    );
    let (headers, body) = &requests[0];
    assert!(
        headers.iter().any(|header| header.ends_with(PROVIDER_KEY)),
        "the provider must be given the account's own credential by the gateway: {headers:?}"
    );
    assert!(
        headers
            .iter()
            .filter(|header| header.to_ascii_lowercase().starts_with("authorization:"))
            .all(|header| header.ends_with(PROVIDER_KEY)),
        "Sterna's bearer for the gateway must never reach the provider: {headers:?}"
    );
    let request: serde_json::Value = serde_json::from_str(body).unwrap();
    assert_eq!(
        request["messages"][0]["content"][0]["text"], "hi",
        "the request the provider received must be this session's turn"
    );
}

/// The `inference-gateway` binary next to `sterna`'s own — built here when a
/// `cargo test -p sterna` did not build it, so this file never depends on the
/// order somebody ran the workspace in.
#[cfg(unix)]
fn real_gateway_binary() -> PathBuf {
    let candidate = Path::new(env!("CARGO_BIN_EXE_sterna")).with_file_name("inference-gateway");
    if !candidate.exists() {
        let status = Command::new(env!("CARGO"))
            .args([
                "build",
                "-p",
                "inference-gateway",
                "--bin",
                "inference-gateway",
            ])
            .current_dir(Path::new(env!("CARGO_MANIFEST_DIR")).join("../.."))
            .status()
            .expect("cargo runs");
        assert!(
            status.success(),
            "building inference-gateway for the standalone proof"
        );
    }
    assert!(
        candidate.exists(),
        "no inference-gateway beside sterna at {}",
        candidate.display()
    );
    candidate
}

/// A **hosted** sterna -- Glasshouse started a gateway and passed its URL --
/// attaches to it and starts nothing.
///
/// The fake gateway's `serve` would report a *dead* endpoint, so starting it
/// would fail the turn as well as the argv assertion: this cannot pass by
/// accident.
#[cfg(unix)]
#[test]
fn a_session_handed_a_base_url_attaches_rather_than_starting_a_gateway() {
    let root = scratch_dir("attach-gateway-root");
    let rollout = root.join("rollout.jsonl");
    let record = root.join("gateway-argv.txt");
    let (base_url, bodies) = start_fake_provider(vec![ending_reply()]);
    let dead = refused_base_url();
    let gateway = write_fake_gateway(&root, "fake_gateway.sh", &record, &dead, "unused");

    let output = run_session_with_gateway(
        &root,
        &rollout,
        "sess-attach",
        "hi",
        &base_url,
        Some(&gateway),
    );
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let seen = fs::read_to_string(&record).unwrap_or_default();
    assert!(
        !seen.lines().any(|line| line.starts_with("serve")),
        "a sterna handed a base URL must not start a second gateway: {seen}"
    );
    assert_eq!(
        bodies.lock().unwrap().len(),
        1,
        "the turn must go to the URL sterna was handed"
    );
}

/// `/model` and `/login` reach the **gateway** binary, and neither carries
/// `--scope`: the gateway has no projects, and a flag it does not accept
/// would fail the call rather than be ignored.
#[cfg(unix)]
#[test]
fn the_model_and_login_controls_reach_the_gateway_without_a_scope() {
    let root = scratch_dir("gateway-controls-root");
    let rollout = root.join("rollout.jsonl");
    let record = root.join("gateway-argv.txt");
    let base_url = refused_base_url();
    let gateway = write_fake_gateway(&root, "fake_gateway.sh", &record, &base_url, "unused");

    // Standalone: nothing handed sterna a gateway, so it starts this one and
    // every control goes to it.
    let output = run_session_stdin_with_gateway(
        &root,
        &rollout,
        "sess-gateway-controls",
        &["/model", "/login work@example.com"],
        None,
        &gateway,
    );
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let seen = fs::read_to_string(&record).unwrap();
    let lines: Vec<&str> = seen.lines().collect();
    assert!(
        lines.contains(&"entitlements --json --refresh"),
        "/model must refresh entitlements through the gateway: {seen}"
    );
    assert!(
        lines.contains(&"entitlements --json"),
        "/login must read entitlements through the gateway: {seen}"
    );
    assert!(
        lines.contains(
            &"subscriptions connect anthropic --entitlement work@example.com --json --no-browser"
        ),
        "/login must run the connect flow through the gateway, which opens no browser itself: {seen}"
    );
    assert!(
        !seen.contains("--scope"),
        "`--scope` is a Glasshouse project concept and must not be sent: {seen}"
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("claude-opus-5"),
        "the model panel must be built from the gateway's catalogue:\n{stdout}"
    );
    assert!(
        stdout.contains("connected as work@example.com"),
        "the login flow's own progress must reach the panel:\n{stdout}"
    );
}

/// **A key entered in the session exists in exactly one place afterwards.**
/// It reaches the gateway on the child's stdin -- never in `argv`, which any
/// process on the machine can read -- and nowhere a later reader could find
/// it: not the session's answers, not its diagnostics, not the rollout it
/// would replay from.
#[cfg(unix)]
#[test]
fn a_key_typed_at_the_prompt_reaches_the_gateway_on_stdin_and_nothing_else() {
    const KEY: &str = "sk-test-secret-value";
    let root = scratch_dir("gateway-key-entry-root");
    let rollout = root.join("rollout.jsonl");
    let record = root.join("gateway-argv.txt");
    let base_url = refused_base_url();
    let gateway = write_fake_gateway(&root, "fake_gateway.sh", &record, &base_url, "unused");

    let output = run_session_stdin_with_gateway(
        &root,
        &rollout,
        "sess-key-entry",
        &["/key anthropic", KEY],
        None,
        &gateway,
    );
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let seen = fs::read_to_string(&record).unwrap();
    assert!(
        seen.lines()
            .any(|line| line == "credentials set anthropic --json"),
        "/key must store through the gateway's own command: {seen}"
    );
    assert_eq!(
        fs::read_to_string(root.join("gateway-argv.txt.stdin")).unwrap(),
        KEY,
        "the gateway must receive the key itself, on stdin"
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let replayed = fs::read_to_string(&rollout).unwrap_or_default();
    for (place, text) in [
        ("stdout", stdout.as_ref()),
        ("stderr", stderr.as_ref()),
        ("the rollout", replayed.as_str()),
    ] {
        assert!(
            !text.contains(KEY),
            "the key must never reach {place}:\n{text}"
        );
    }
    assert!(
        stdout.contains("Stored the ANTHROPIC_API_KEY for anthropic in the gateway."),
        "the variable it was stored under is what the session reports:\n{stdout}"
    );
}

/// `/login` is where a person goes to be able to send a turn, so it lists
/// both ways of becoming able to: the accounts a flow connects, and then the
/// providers whose API key the gateway holds -- or does not.
#[cfg(unix)]
#[test]
fn the_login_panel_lists_api_key_rows_after_the_accounts() {
    let root = scratch_dir("gateway-key-panel-root");
    let rollout = root.join("rollout.jsonl");
    let record = root.join("gateway-argv.txt");
    let base_url = refused_base_url();
    let gateway = write_fake_gateway(&root, "fake_gateway.sh", &record, &base_url, "unused");

    let output = run_session_stdin_with_gateway(
        &root,
        &rollout,
        "sess-key-panel",
        // The wizard's two steps: the subscription accounts, then the keys.
        &["/login subscription", "/login key"],
        None,
        &gateway,
    );
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    let Some(account) = stdout.find("Claude · work@example.com · ⚠ read first · sign in")
    else {
        panic!("the account row must still be listed:\n{stdout}");
    };
    let Some(key) = stdout.find("anthropic · API key · not set") else {
        panic!("the key row must be listed:\n{stdout}");
    };
    assert!(
        account < key,
        "the key rows come after the accounts:\n{stdout}"
    );
}

/// A hosted session's keys are the gateway's keys: `/login` lists the API-key
/// rows and `/key` stores one, through the gateway binary rather than through
/// Glasshouse — and the key itself still exists in exactly one place
/// afterwards, the gateway's own stdin.
#[cfg(unix)]
#[test]
fn a_hosted_session_enters_a_key_through_the_gateway_binary() {
    const KEY: &str = "a-key-entered-in-a-hosted-session-and-kept-by-the-gateway";
    let root = scratch_dir("hosted-key-entry-root");
    let rollout = root.join("rollout.jsonl");
    let record = root.join("gateway-argv.txt");
    let base_url = refused_base_url();
    let gateway = write_fake_gateway(&root, "fake_gateway.sh", &record, &base_url, "unused");

    let output = run_session_stdin_hosted(
        &root,
        &rollout,
        "sess-hosted-key",
        &["/login key", "/key anthropic", KEY],
        &base_url,
        &gateway,
    );
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("anthropic · API key · not set"),
        "a hosted /login must list the gateway's key rows:\n{stdout}"
    );
    let seen = fs::read_to_string(&record).unwrap();
    assert!(
        seen.lines()
            .any(|line| line == "credentials set anthropic --json"),
        "/key must store through the gateway's own command: {seen}"
    );
    assert_eq!(
        fs::read_to_string(root.join("gateway-argv.txt.stdin")).unwrap(),
        KEY,
        "the gateway must receive the key itself, on stdin"
    );
    assert!(
        stdout.contains("Stored the ANTHROPIC_API_KEY for anthropic in the gateway."),
        "the variable it was stored under is what the session reports:\n{stdout}"
    );

    let stderr = String::from_utf8_lossy(&output.stderr);
    let replayed = fs::read_to_string(&rollout).unwrap_or_default();
    for (place, text) in [
        ("stdout", stdout.as_ref()),
        ("stderr", stderr.as_ref()),
        ("the rollout", replayed.as_str()),
    ] {
        assert!(
            !text.contains(KEY),
            "the key must never reach {place}:\n{text}"
        );
    }
}

/// **Attached** account controls: handed a gateway (a base URL with a
/// loopback host), `/model`, `/login` and the
/// key rows go to the **gateway binary** — the accounts, subscriptions and
/// credentials are the gateway's wherever it was started — and none of them
/// carries `--scope`, which the gateway does not accept.
#[cfg(unix)]
#[test]
fn the_model_and_login_controls_of_a_hosted_session_reach_the_gateway_binary() {
    let root = scratch_dir("hosted-controls-root");
    let rollout = root.join("rollout.jsonl");
    let gateway_record = root.join("gateway-argv.txt");
    let base_url = refused_base_url();
    let gateway = write_fake_gateway(&root, "fake_gateway.sh", &gateway_record, &base_url, "x");

    let output = run_session_stdin_hosted(
        &root,
        &rollout,
        "sess-hosted-controls",
        &["/model", "/login", "/login work@example.com"],
        &base_url,
        &gateway,
    );
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let seen = fs::read_to_string(&gateway_record).unwrap();
    assert!(
        seen.lines()
            .any(|line| line == "entitlements --json --refresh"),
        "/model must refresh entitlements through the gateway binary: {seen}"
    );
    assert!(
        seen.lines().any(|line| line == "credentials list --json"),
        "/login must read the key rows through the gateway binary: {seen}"
    );
    assert!(
        seen.lines()
            .any(|line| line
                == "subscriptions connect anthropic --entitlement work@example.com --json --no-browser"),
        "/login must run the connect flow through the gateway binary, which opens no browser itself: {seen}"
    );
    assert!(
        !seen.contains("--scope"),
        "the gateway has no projects to scope a control to: {seen}"
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("claude-opus-5"),
        "the model panel must be built from the gateway's catalogue:\n{stdout}"
    );
    assert!(
        stdout.contains("connected as work@example.com"),
        "the login flow's own progress must reach the panel:\n{stdout}"
    );
}

/// What an attached session **spent** is read from the gateway it is attached
/// to: the usage readout runs the gateway binary's `routing-cost`, like every
/// other control, and carries no `--scope`.
#[cfg(unix)]
#[test]
fn the_usage_rows_of_an_attached_session_come_from_the_gateway_binary() {
    let root = scratch_dir("hosted-usage-root");
    let rollout = root.join("rollout.jsonl");
    let gateway_record = root.join("gateway-argv.txt");
    let (base_url, bodies) = start_fake_provider(vec![ending_reply()]);
    let gateway = write_fake_gateway(&root, "fake_gateway.sh", &gateway_record, &base_url, "x");

    let output = Command::new(env!("CARGO_BIN_EXE_sterna"))
        .arg("session")
        .arg("--root")
        .arg(&root)
        .arg("--rollout")
        .arg(&rollout)
        .arg("--session")
        .arg("sess-hosted-usage")
        .arg("--model")
        .arg(sterna::wire::MODEL)
        .arg("--task")
        .arg("hi")
        .env("ANTHROPIC_BASE_URL", &base_url)
        .env_remove("ANTHROPIC_AUTH_TOKEN")
        .env_remove("ANTHROPIC_API_KEY")
        .env("XDG_CONFIG_HOME", root.join("global-config"))
        .env("PATH", "/usr/bin:/bin")
        .env("INFERENCE_GATEWAY_BIN", &gateway)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(bodies.lock().unwrap().len(), 1, "the turn must have run");

    let seen = fs::read_to_string(&gateway_record).unwrap_or_default();
    assert!(
        seen.lines()
            .any(|line| line.starts_with("routing-cost --json --since")),
        "the usage readout must reach the gateway binary: {seen}"
    );
    assert!(
        !seen.contains("--scope"),
        "the gateway has no projects to scope a readout to: {seen}"
    );
}

/// The resolution the hosted account controls do, at its failing end: nothing
/// named in `INFERENCE_GATEWAY_BIN`, nothing beside the executable, nothing on
/// `PATH`. The control says the gateway is not reachable rather than reporting
/// an empty catalogue as though the gateway had answered with one.
///
/// **The binary is copied into this test's own directory first**: the build
/// directory `CARGO_BIN_EXE_sterna` points into has an `inference-gateway` beside
/// `sterna` (see [`real_gateway_binary`]), which is exactly what the second step
/// of the resolution finds.
#[cfg(unix)]
#[test]
fn a_hosted_session_with_no_gateway_anywhere_says_it_is_not_reachable() {
    use std::process::Stdio;
    let root = scratch_dir("hosted-no-gateway-root");
    let rollout = root.join("rollout.jsonl");
    let base_url = refused_base_url();
    let sterna = root.join("sterna");
    fs::copy(env!("CARGO_BIN_EXE_sterna"), &sterna).unwrap();

    let mut child = Command::new(&sterna)
        .arg("session")
        .arg("--root")
        .arg(&root)
        .arg("--rollout")
        .arg(&rollout)
        .arg("--session")
        .arg("sess-hosted-no-gateway")
        .arg("--model")
        .arg(sterna::wire::MODEL)
        .env("ANTHROPIC_BASE_URL", &base_url)
        .env_remove("ANTHROPIC_AUTH_TOKEN")
        .env_remove("ANTHROPIC_API_KEY")
        .env_remove("INFERENCE_GATEWAY_BIN")
        .env("PATH", "/usr/bin:/bin")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    writeln!(child.stdin.as_mut().unwrap(), "/login").unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("did not answer")
            && stdout.contains("not installed, or not on your PATH")
            && stdout.contains("Try again"),
        "no gateway resolves anywhere, and the sheet must say why and what next:\n{stdout}"
    );
}

/// The other two steps of that resolution, and their order: with nothing in
/// `INFERENCE_GATEWAY_BIN`, a hosted control runs the gateway installed beside
/// the binary, and falls back to the first one on `PATH`.
///
/// **The order is the point.** An install ships `sterna` and `inference-gateway`
/// together, and a session must ask the gateway it was installed with rather
/// than whichever older one a shell happens to resolve first.
#[cfg(unix)]
#[test]
fn a_hosted_session_prefers_the_gateway_beside_the_binary_to_the_one_on_path() {
    use std::process::Stdio;
    let root = scratch_dir("hosted-gateway-resolution-root");
    let base_url = refused_base_url();
    let bin = root.join("bin");
    let on_path = root.join("path");
    fs::create_dir_all(&bin).unwrap();
    fs::create_dir_all(&on_path).unwrap();
    let sterna = bin.join("sterna");
    fs::copy(env!("CARGO_BIN_EXE_sterna"), &sterna).unwrap();
    let path_record = root.join("on-path-argv.txt");
    write_fake_gateway(
        &on_path,
        "inference-gateway",
        &path_record,
        &base_url,
        "unused",
    );

    let login = |session: &str| {
        let mut child = Command::new(&sterna)
            .arg("session")
            .arg("--root")
            .arg(&root)
            .arg("--rollout")
            .arg(root.join(format!("{session}.jsonl")))
            .arg("--session")
            .arg(session)
            .arg("--model")
            .arg(sterna::wire::MODEL)
            .env("ANTHROPIC_BASE_URL", &base_url)
            .env_remove("ANTHROPIC_AUTH_TOKEN")
            .env_remove("ANTHROPIC_API_KEY")
            .env_remove("INFERENCE_GATEWAY_BIN")
            .env("PATH", format!("{}:/usr/bin:/bin", on_path.display()))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        writeln!(child.stdin.as_mut().unwrap(), "/login subscription").unwrap();
        let output = child.wait_with_output().unwrap();
        assert!(
            output.status.success(),
            "stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).into_owned()
    };

    let stdout = login("sess-gateway-on-path");
    assert!(
        stdout.contains("Claude · work@example.com · ⚠ read first · sign in"),
        "the gateway on PATH must have answered the catalogue:\n{stdout}"
    );
    let asked_on_path = fs::read_to_string(&path_record).unwrap();
    assert!(
        asked_on_path.contains("entitlements --json"),
        "the gateway on PATH is what a hosted control resolves: {asked_on_path}"
    );

    // And now one beside the binary, which must take it instead.
    let beside_record = root.join("beside-argv.txt");
    write_fake_gateway(
        &bin,
        "inference-gateway",
        &beside_record,
        &base_url,
        "unused",
    );
    let stdout = login("sess-gateway-beside");
    assert!(
        stdout.contains("Claude · work@example.com · ⚠ read first · sign in"),
        "the gateway beside the binary must have answered:\n{stdout}"
    );
    assert!(
        fs::read_to_string(&beside_record)
            .unwrap()
            .contains("entitlements --json"),
        "a gateway installed beside the binary is the one a hosted control asks"
    );
    assert_eq!(
        fs::read_to_string(&path_record).unwrap(),
        asked_on_path,
        "and the one on PATH must not have been asked a second time"
    );
}

/// A base URL with neither a bearer nor a loopback host is not a gateway
/// handed to this session — it is what Claude Code exports into every child
/// — so sterna starts its own gateway and points itself at that instead.
#[cfg(unix)]
#[test]
fn a_base_url_without_a_token_or_a_loopback_host_does_not_count_as_a_handed_gateway() {
    let root = scratch_dir("inherited-base-url-root");
    let rollout = root.join("rollout.jsonl");
    let record = root.join("gateway-argv.txt");
    let (provider_url, requests) = start_header_recording_provider(vec![ending_reply()]);
    let gateway = write_fake_gateway(&root, "fake_gateway.sh", &record, &provider_url, "gw-7");

    let output = run_session_with_gateway(
        &root,
        &rollout,
        "sess-inherited-url",
        "hi",
        "http://proxy.invalid:9",
        Some(&gateway),
    );
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let seen = fs::read_to_string(&record).unwrap_or_default();
    assert!(
        seen.lines().any(|line| line.starts_with("serve ")),
        "sterna must start its own gateway rather than attach to an inherited URL: {seen}"
    );
    assert_eq!(
        requests.lock().unwrap().len(),
        1,
        "the turn went to the gateway sterna started, not to the inherited URL"
    );
}

/// A gateway that refuses before its ready line says why, in Sterna's own
/// refusal: the real binary given a configuration it cannot read exits with
/// its words on stderr, and they come back. (An account whose credential
/// resolves nowhere is no longer a refusal: since 2026-09-11 the gateway
/// listens and waits for a key — `a_key_entered_in_the_session_…` above.)
#[cfg(unix)]
#[test]
fn a_gateway_that_refuses_to_serve_is_quoted_in_sternas_refusal() {
    let gateway = real_gateway_binary();
    let root = scratch_dir("gateway-refusal-root");
    let rollout = root.join("rollout.jsonl");
    let config = root.join("gateway.toml");
    fs::write(&config, "[providers.fixture\nthis is not toml\n").unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_sterna"))
        .arg("session")
        .arg("--root")
        .arg(&root)
        .arg("--rollout")
        .arg(&rollout)
        .arg("--session")
        .arg("sess-gateway-refusal")
        .arg("--model")
        .arg(sterna::wire::MODEL)
        .arg("--task")
        .arg("hi")
        .arg("--gateway")
        .arg(&gateway)
        .env_remove("ANTHROPIC_BASE_URL")
        .env_remove("ANTHROPIC_AUTH_TOKEN")
        .env_remove("ANTHROPIC_API_KEY")
        .env_remove("STERNA_E2E_UNSET_VAR")
        .env("XDG_CONFIG_HOME", root.join("global-config"))
        .env("PATH", "/usr/bin:/bin")
        .env("INFERENCE_GATEWAY_CONFIG", &config)
        .env("INFERENCE_GATEWAY_DATA_DIR", root.join("gateway-data"))
        .output()
        .unwrap();
    assert!(
        !output.status.success(),
        "a gateway that cannot serve is a refusal"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("did not report a listening address") && stderr.contains("it said:"),
        "the refusal must carry the gateway's own words: {stderr}"
    );
    assert!(
        stderr.contains("gateway.toml"),
        "the gateway's words must name the file it could not read: {stderr}"
    );
}

/// A gateway **named by path** that cannot be started is a startup refusal,
/// naming the binary: a turn that skipped a gateway the user asked for would
/// skip every entitlement and cost control it exists to apply. Only an
/// unnamed, uninstalled gateway is the direct-mode case, and that is a notice.
#[test]
fn a_gateway_that_cannot_be_started_refuses_the_session_by_name() {
    let root = scratch_dir("gateway-missing-root");
    let rollout = root.join("rollout.jsonl");
    let missing = root.join("no-such-inference-gateway");

    let output = Command::new(env!("CARGO_BIN_EXE_sterna"))
        .arg("session")
        .arg("--root")
        .arg(&root)
        .arg("--rollout")
        .arg(&rollout)
        .arg("--session")
        .arg("sess-gateway-missing")
        .arg("--model")
        .arg(sterna::wire::MODEL)
        .arg("--task")
        .arg("hi")
        .arg("--gateway")
        .arg(&missing)
        .env_remove("ANTHROPIC_BASE_URL")
        .env_remove("ANTHROPIC_AUTH_TOKEN")
        .env_remove("ANTHROPIC_API_KEY")
        .env("XDG_CONFIG_HOME", root.join("global-config"))
        .output()
        .unwrap();

    assert!(
        !output.status.success(),
        "a session with no gateway must not start: {}",
        String::from_utf8_lossy(&output.stdout)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("sterna cannot start: could not run the inference gateway"),
        "the refusal must be one sentence a person can act on: {stderr}"
    );
    assert!(
        stderr.contains(&missing.display().to_string()),
        "the refusal must name the binary it could not run: {stderr}"
    );
    assert!(
        !rollout.exists(),
        "a refused session must not have opened a rollout"
    );
}

/// The user's own case (2026-09-11): no key anywhere, the session starts
/// anyway, the key entered at `/key` is stored by the gateway sterna spawned,
/// and the next turn completes through it. Stdin mode stands in for the
/// masked prompt: the line after `/key fixture` is the key.
#[cfg(unix)]
#[test]
fn a_key_entered_in_the_session_is_stored_by_the_gateway_and_the_next_turn_completes() {
    use std::io::Write as _;
    const ENTERED_KEY: &str = "entered-at-the-prompt-never-in-the-environment";

    let gateway = real_gateway_binary();
    let root = scratch_dir("real-gateway-key-entry");
    let rollout = root.join("rollout.jsonl");
    let (provider_url, requests) = start_header_recording_provider(vec![ending_reply()]);
    let config = root.join("gateway.toml");
    fs::write(
        &config,
        format!(
            r#"
[providers.fixture]
base_url = "{provider_url}"
protocol = "anthropic-messages"
credential_env = ["STERNA_E2E_ENTERED_KEY"]
"#
        ),
    )
    .unwrap();
    let data_dir = root.join("gateway-data");

    let mut child = Command::new(env!("CARGO_BIN_EXE_sterna"))
        .arg("session")
        .arg("--root")
        .arg(&root)
        .arg("--rollout")
        .arg(&rollout)
        .arg("--session")
        .arg("sess-key-entry")
        .arg("--model")
        .arg(sterna::wire::MODEL)
        .arg("--gateway")
        .arg(&gateway)
        .env_remove("ANTHROPIC_BASE_URL")
        .env_remove("ANTHROPIC_AUTH_TOKEN")
        .env_remove("ANTHROPIC_API_KEY")
        .env_remove("STERNA_E2E_ENTERED_KEY")
        .env("XDG_CONFIG_HOME", root.join("global-config"))
        .env("PATH", "/usr/bin:/bin")
        .env("INFERENCE_GATEWAY_CONFIG", &config)
        .env("INFERENCE_GATEWAY_DATA_DIR", &data_dir)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    stdin
        .write_all(format!("/key fixture\n{ENTERED_KEY}\nhi\n").as_bytes())
        .unwrap();
    drop(stdin);
    let output = child.wait_with_output().unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "the session must complete: {stdout}\n{stderr}"
    );
    assert!(
        stderr.contains("No provider credential is stored yet"),
        "a session that spawned a keyless gateway says so once: {stderr}"
    );

    let requests = requests.lock().unwrap();
    assert_eq!(
        requests.len(),
        1,
        "the turn after the key was entered reached the provider: {stdout}\n{stderr}"
    );
    let (headers, _) = &requests[0];
    assert!(
        headers.iter().any(|header| header.ends_with(ENTERED_KEY)),
        "the provider was given the entered key by the gateway: {headers:?}"
    );

    let credentials = data_dir.join("credentials.toml");
    let stored =
        fs::read_to_string(&credentials).expect("the gateway stored the key in its own file");
    assert!(stored.contains("STERNA_E2E_ENTERED_KEY"), "{stored}");
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mode = fs::metadata(&credentials).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "owner-only, was {mode:o}");
    }
    for (name, text) in [
        ("stdout", stdout.to_string()),
        ("stderr", stderr.to_string()),
        ("rollout", fs::read_to_string(&rollout).unwrap_or_default()),
        (
            "gateway log",
            fs::read_to_string(rollout.with_extension("gateway.log")).unwrap_or_default(),
        ),
    ] {
        assert!(
            !text.contains(ENTERED_KEY),
            "the key leaked into {name}: {text}"
        );
    }
}

/// The start-up notice is for a gateway with nothing to serve. A connected
/// subscription is something to serve even though no provider key is stored,
/// so it silences the notice — measured 2026-09-11 on three serving
/// subscriptions that were told nothing was stored.
#[cfg(unix)]
#[test]
fn a_connected_subscription_silences_the_missing_credential_notice() {
    let root = scratch_dir("notice-connected-root");
    let record = root.join("gateway.record");
    let gateway = write_fake_gateway(
        &root,
        "inference-gateway",
        &record,
        "http://127.0.0.1:9",
        "fake-token",
    );
    let stderr_with = |authenticated: &str| -> String {
        let output = Command::new(env!("CARGO_BIN_EXE_sterna"))
            .arg("session")
            .arg("--root")
            .arg(&root)
            .arg("--rollout")
            .arg(root.join(format!("rollout-{authenticated}.jsonl")))
            .arg("--session")
            .arg(format!("sess-notice-{authenticated}"))
            .arg("--model")
            .arg(sterna::wire::MODEL)
            .arg("--gateway")
            .arg(&gateway)
            .env_remove("ANTHROPIC_BASE_URL")
            .env_remove("ANTHROPIC_AUTH_TOKEN")
            .env_remove("ANTHROPIC_API_KEY")
            .env("XDG_CONFIG_HOME", root.join("global-config"))
            .env("FAKE_GATEWAY_AUTHENTICATED", authenticated)
            .stdin(std::process::Stdio::null())
            .output()
            .unwrap();
        String::from_utf8_lossy(&output.stderr).to_string()
    };
    assert!(
        stderr_with("false").contains("No provider credential is stored yet"),
        "no key stored and nothing connected: the notice"
    );
    assert!(
        !stderr_with("true").contains("No provider credential is stored yet"),
        "a connected subscription serves, so no notice"
    );
}

/// Ending a session ends its gateway the polite way first: stdin closes,
/// the gateway gets a moment to shut down — which is what terminates the
/// subscription brokers it started — and only then is it killed. Measured
/// 2026-09-11: a kill without the wait left three broker sidecars orphaned
/// on every session exit.
#[cfg(unix)]
#[test]
fn ending_a_session_lets_its_gateway_shut_down_before_it_is_killed() {
    let root = scratch_dir("gateway-polite-shutdown");
    let marker = root.join("gateway-stopped-cleanly");
    let script = format!(
        "#!/bin/sh\ncase \"$1\" in\n  serve)\n    echo '{{\"listening\":\"http://127.0.0.1:9\",\"token\":\"t\"}}'\n    while read -r _line; do :; done\n    echo stopped > \"{}\"\n    exit 0\n    ;;\n  credentials)\n    echo '{{\"version\":1,\"providers\":[]}}'\n    ;;\n  entitlements)\n    echo '{{\"version\":1,\"accounts\":[]}}'\n    ;;\nesac\nexit 0\n",
        marker.display()
    );
    let gateway = write_script(&root, "inference-gateway", &script);
    let output = Command::new(env!("CARGO_BIN_EXE_sterna"))
        .arg("session")
        .arg("--root")
        .arg(&root)
        .arg("--rollout")
        .arg(root.join("rollout.jsonl"))
        .arg("--session")
        .arg("sess-polite-shutdown")
        .arg("--model")
        .arg(sterna::wire::MODEL)
        .arg("--gateway")
        .arg(&gateway)
        .env_remove("ANTHROPIC_BASE_URL")
        .env_remove("ANTHROPIC_AUTH_TOKEN")
        .env_remove("ANTHROPIC_API_KEY")
        .env("XDG_CONFIG_HOME", root.join("global-config"))
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        marker.exists(),
        "the gateway must have read EOF and exited on its own before sterna killed it"
    );
}

/// `GH-PANE-TEST-CONFIG-ISOLATION`: the developer's own
/// `~/.config/sterna/config.toml` is whatever `XDG_CONFIG_HOME` names in the
/// *test process's own* environment, since nothing here is guaranteed to run
/// in a container. `run_session_with_gateway`'s `.env("XDG_CONFIG_HOME", ...)`
/// exists to keep a spawned session from ever seeing it; this proves that
/// rather than assuming it, by planting a bogus parent model only at the
/// *inherited* location and this test's own model only at the location the
/// helper points the child to.
///
/// This does not go through [`run_session`]: that helper calls
/// [`supply_test_model`], which takes [`ENV_LOCK`] itself, and this test must
/// hold the lock across its own poisoning window -- a nested lock on the same
/// thread would deadlock. Skipping it costs nothing here: a fresh scratch
/// root has no project config and no CLI `--model` is passed, so resolution
/// is left entirely to whichever global scope the spawned child reads --
/// exactly what `supply_test_model` would have arranged anyway.
#[test]
fn a_poisoned_inherited_global_config_never_reaches_the_session() {
    // Held for the whole poisoning window, so no concurrent
    // `supply_test_model` call (which reads this same variable) can observe
    // the poisoned value and wrongly conclude its own root has a persisted
    // model.
    let _environment = ENV_LOCK.lock().unwrap_or_else(|error| error.into_inner());

    let root = scratch_dir("global-isolation");
    let rollout = root.join("rollout.jsonl");

    // What the developer's shell would already have exported, and so what
    // this test's own process inherits -- the exact hazard the isolation
    // helper exists to shut out.
    let inherited = scratch_dir("global-isolation-inherited");
    fs::create_dir_all(inherited.join("sterna")).unwrap();
    fs::write(
        inherited.join("sterna/config.toml"),
        "[model]\nparent = \"bogus-model-that-must-not-be-used\"\n",
    )
    .unwrap();

    // The location the fixture's own helper points the child's
    // `XDG_CONFIG_HOME` at -- the correct global scope for this session,
    // carrying this test's own model rather than the inherited one.
    fs::create_dir_all(root.join("global-config/sterna")).unwrap();
    fs::write(
        root.join("global-config/sterna/config.toml"),
        format!("[model]\nparent = {:?}\n", sterna::wire::MODEL),
    )
    .unwrap();

    let previous = std::env::var_os("XDG_CONFIG_HOME");
    // Safety: `ENV_LOCK`, held for this whole function, serialises every
    // access to this variable across the test binary.
    unsafe { std::env::set_var("XDG_CONFIG_HOME", &inherited) };
    let (base_url, bodies) = start_fake_provider(vec![ending_reply()]);
    let output = Command::new(env!("CARGO_BIN_EXE_sterna"))
        .arg("session")
        .arg("--root")
        .arg(&root)
        .arg("--rollout")
        .arg(&rollout)
        .arg("--session")
        .arg("sess-global-isolation")
        .arg("--task")
        .arg("hi")
        .env("ANTHROPIC_BASE_URL", &base_url)
        .env_remove("ANTHROPIC_AUTH_TOKEN")
        .env_remove("ANTHROPIC_API_KEY")
        // The line under test: without it, the child inherits this
        // process's own (poisoned) `XDG_CONFIG_HOME` instead.
        .env("XDG_CONFIG_HOME", root.join("global-config"))
        .output()
        .unwrap();
    match previous {
        Some(value) => unsafe { std::env::set_var("XDG_CONFIG_HOME", value) },
        None => unsafe { std::env::remove_var("XDG_CONFIG_HOME") },
    }

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let bodies = bodies.lock().unwrap();
    let request: serde_json::Value = serde_json::from_str(&bodies[0]).unwrap();
    assert_eq!(
        request["model"],
        sterna::wire::MODEL,
        "the session must use this fixture's own global config, never the one \
         inherited from the process environment: {request}"
    );
}

/// The live event stream: a session announces itself, its task and each cell
/// **before** the work happens, so a program tailing the file can say what is
/// happening rather than only what happened.
///
/// The rollout answers the second question and cannot answer the first: its
/// `cell` line is written when the cell is over. This asserts the ordering
/// that makes the difference — `cell.submit` lands before the command inside
/// that cell has run, which is the whole reason the file exists.
#[test]
fn the_event_stream_announces_a_cell_before_it_runs() {
    let root = scratch_dir("observe-root");
    let rollout = root.join("rollout.jsonl");
    let marker = root.join("the-cell-ran");
    // The command line is scenery for this test -- only the event stream's
    // ordering is under test -- so it must actually run on the interpreter
    // this host's `bash` tool answers to: `cmd.exe` has no `touch`, and a
    // Windows path's backslashes must reach the sterna-script string literal
    // escaped (`{:?}`) rather than interpolated raw, or the parser eats them
    // as (mostly unrecognized) escapes of its own. Same fix as
    // `instruction_boundary.rs`'s `create_file_cell`.
    let touch_command = if cfg!(windows) {
        format!("type nul > \"{}\"", marker.display())
    } else {
        format!("touch {}", marker.display())
    };
    let (base_url, _bodies) = start_fake_provider(vec![
        assistant_reply(&format!(
            "```sterna\nawait bash({{ command: {touch_command:?} }});\nreturn 1;\n```",
        )),
        assistant_reply("done"),
        ending_reply(),
    ]);

    let output = run_session(&root, &rollout, "sess-observe", "go", &base_url);
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let events = root.join("rollout.events.jsonl");
    let text = fs::read_to_string(&events)
        .unwrap_or_else(|e| panic!("no stream beside the rollout at {}: {e}", events.display()));
    let lines: Vec<serde_json::Value> = text
        .lines()
        .map(|line| serde_json::from_str(line).expect("every line is one JSON object"))
        .collect();
    let kinds: Vec<&str> = lines
        .iter()
        .map(|line| line["kind"].as_str().expect("a kind"))
        .collect();

    for expected in [
        "session.begin",
        "task.begin",
        "cell.submit",
        "cell.end",
        "task.end",
    ] {
        assert!(kinds.contains(&expected), "no {expected} in {kinds:?}");
    }

    // The ordering that matters: submitted before run, ended after.
    let submit = kinds.iter().position(|k| *k == "cell.submit").unwrap();
    let end = kinds.iter().position(|k| *k == "cell.end").unwrap();
    let task_begin = kinds.iter().position(|k| *k == "task.begin").unwrap();
    assert!(task_begin < submit, "the task opens before its first cell");
    assert!(submit < end, "a cell is announced before it is finished");

    // `cell.submit` carries what a reader at that seam would decide on: the
    // source, and the command line the program spells out.
    let submitted = &lines[submit];
    assert_eq!(
        submitted["span"], lines[end]["span"],
        "one span, two halves"
    );
    let commands = submitted["commands"]
        .as_array()
        .expect("certain command lines");
    assert!(
        commands
            .iter()
            .any(|c| c.as_str() == Some(touch_command.as_str())),
        "the literal command line is named before it runs: {commands:?}"
    );
    assert!(
        submitted["program"]
            .as_str()
            .is_some_and(|s| s.contains("await bash(")),
        "the whole program is carried for a reader that may refuse it"
    );
    // Every line keeps the envelope's own `source`: the program is spelled
    // `program` precisely so it cannot displace it, which it once did.
    for (line, kind) in lines.iter().zip(&kinds) {
        assert_eq!(
            line["source"].as_str(),
            Some("session/sess-observe"),
            "{kind} lost the envelope's source"
        );
    }

    // And the span number is the rollout's own cell number, so a reader
    // correlating the two files never has to translate between them.
    let rollout_text = fs::read_to_string(&rollout).unwrap();
    let cell_number = rollout_text
        .lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .find(|line| line["kind"] == "cell")
        .and_then(|line| line["cell"].as_u64())
        .expect("the rollout recorded a cell");
    assert_eq!(
        submitted["cell"].as_u64(),
        Some(cell_number),
        "the stream's cell number is the rollout's"
    );
}

/// The main model's effort, as one task's first request asks for it.
fn first_request_effort(config: Option<&str>, model_flag: bool) -> serde_json::Value {
    let root = scratch_dir("turn-effort");
    if let Some(config) = config {
        fs::create_dir_all(root.join(".sterna")).unwrap();
        fs::write(root.join(".sterna/config.toml"), config).unwrap();
    }
    let (base, bodies) = start_answering_provider(1, |_| ending_reply());
    let mut command = Command::new(env!("CARGO_BIN_EXE_sterna"));
    command
        .arg("session")
        .arg("--root")
        .arg(&root)
        .arg("--task")
        .arg("go")
        .env("ANTHROPIC_BASE_URL", &base)
        .env("XDG_CONFIG_HOME", root.join("global-config"))
        .env_remove("ANTHROPIC_AUTH_TOKEN")
        .env_remove("ANTHROPIC_API_KEY");
    if model_flag {
        command.arg("--model").arg(sterna::wire::MODEL);
    }
    let output = command.output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let bodies = bodies.lock().unwrap();
    let first: serde_json::Value = serde_json::from_str(&bodies[0]).unwrap();
    first["output_config"]["effort"].clone()
}

/// Left at `default`, a GPT main model asks for `low` -- what the ruler's
/// `low` arm measured faster at equal results; a chosen effort is sent as
/// chosen, and a Claude model keeps the provider's own setting.
#[test]
fn a_gpt_main_model_left_at_default_effort_asks_for_low() {
    let gpt = "[model]\nparent = \"gpt-6-sol\"\n";
    assert_eq!(first_request_effort(Some(gpt), false), "low");
    let chosen = format!("{gpt}[session]\neffort = \"high\"\n");
    assert_eq!(first_request_effort(Some(&chosen), false), "high");
    assert!(first_request_effort(None, true).is_null());
}

/// How many requests of one task went to the helper model, when the task
/// writes a file.
fn helper_requests(helpers: &str) -> usize {
    helper_requests_after(helpers, writing_reply())
}

/// A task that writes one file and answers.
fn writing_reply() -> String {
    assistant_reply(
        "```sterna\nawait write({path:'made.txt',content:'made'});\nanswer(\"done\");\n```",
    )
}

/// How many requests of one task went to the helper model, for one reply.
fn helper_requests_after(helpers: &str, reply: String) -> usize {
    let root = scratch_dir("one-task-behind");
    fs::create_dir_all(root.join(".sterna")).unwrap();
    fs::write(
        root.join(".sterna/config.toml"),
        format!("[helpers]\nacceptance_list = false\nmodel = \"helper-tier\"\n{helpers}"),
    )
    .unwrap();
    let (base, bodies) = start_answering_provider(2, move |_| reply.clone());
    let output = run_session(
        &root,
        &root.join("rollout.jsonl"),
        "one-task-behind",
        "go",
        &base,
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let bodies = bodies.lock().unwrap();
    bodies
        .iter()
        .filter(|body| {
            serde_json::from_str::<serde_json::Value>(body).unwrap()["model"] == "helper-tier"
        })
        .count()
}

/// A one-task run exits when its task is done, so work behind the answer
/// would hold the exit: the completion check starts only when the person
/// wrote `completion_check` themselves.
#[test]
fn a_one_task_run_starts_the_completion_check_only_when_the_person_set_it() {
    assert_eq!(helper_requests(""), 0, "an unset check held a one-task run");
    assert_eq!(helper_requests("completion_check = \"always\"\n"), 1);
}

/// A turn that changed nothing leaves nothing in the files to check: the
/// checker spent its turns finding that out and said it could not tell.
/// It is never asked, even when the person chose `always`.
#[test]
fn a_turn_that_changed_nothing_is_never_checked() {
    assert_eq!(
        helper_requests_after("completion_check = \"always\"\n", ending_reply()),
        0,
        "a read-only answer was checked"
    );
}

/// An upgrade retired `ui.stream = "quiet"`, and a sterna that refused to start
/// over it punished the person for updating. It starts, says what changed,
/// and takes the word out of the file so the next start is quiet.
#[test]
fn a_choice_an_upgrade_retired_is_removed_and_sterna_starts() {
    let root = scratch_dir("retired-choice");
    fs::create_dir_all(root.join(".sterna")).unwrap();
    let config = root.join(".sterna/config.toml");
    fs::write(&config, "[ui]\nstream = \"quiet\"\ntheme = \"amber\"\n").unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_sterna"))
        .arg("session")
        .arg("--root")
        .arg(&root)
        .arg("--rollout")
        .arg(root.join("rollout.jsonl"))
        .arg("--model")
        .arg(sterna::wire::MODEL)
        .env("ANTHROPIC_BASE_URL", refused_base_url())
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "a retired choice stopped sterna:\n{stdout}\n{stderr}"
    );
    assert!(
        stdout.contains("`ui.stream = quiet` is no longer a choice")
            && stdout.contains("Sterna uses `actions`"),
        "{stdout}"
    );
    let saved = fs::read_to_string(&config).unwrap();
    assert!(!saved.contains("quiet"), "{saved}");
    assert!(
        saved.contains("amber"),
        "the rest of the file is kept: {saved}"
    );
}

/// `ui.look` and `/bird` are gone -- the parrots are themes. A file that
/// still says `look = "bird"` starts, is told where the bird went, and
/// loses the line so the next start is quiet.
#[test]
fn a_saved_look_is_removed_and_sterna_starts() {
    let root = scratch_dir("retired-look");
    fs::create_dir_all(root.join(".sterna")).unwrap();
    let config = root.join(".sterna/config.toml");
    fs::write(&config, "[ui]\nlook = \"bird\"\ntheme = \"amber\"\n").unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_sterna"))
        .arg("session")
        .arg("--root")
        .arg(&root)
        .arg("--rollout")
        .arg(root.join("rollout.jsonl"))
        .arg("--model")
        .arg(sterna::wire::MODEL)
        .env("ANTHROPIC_BASE_URL", refused_base_url())
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "a saved look stopped sterna:\n{stdout}\n{stderr}"
    );
    assert!(
        stdout.contains("`ui.look = bird` is no longer a setting")
            && stdout.contains("parrot themes"),
        "{stdout}"
    );
    let saved = fs::read_to_string(&config).unwrap();
    assert!(!saved.contains("look"), "{saved}");
    assert!(
        saved.contains("amber"),
        "the rest of the file is kept: {saved}"
    );
}

/// `ui.voice` is gone -- Sterna speaks one plain voice. A file that still
/// says `voice = "playful"` starts, is told once, and loses the line so the
/// next start is quiet.
#[test]
fn a_saved_voice_is_removed_and_sterna_starts() {
    let root = scratch_dir("retired-voice");
    fs::create_dir_all(root.join(".sterna")).unwrap();
    let config = root.join(".sterna/config.toml");
    fs::write(&config, "[ui]\nvoice = \"playful\"\ntheme = \"amber\"\n").unwrap();
    let run = || {
        Command::new(env!("CARGO_BIN_EXE_sterna"))
            .arg("session")
            .arg("--root")
            .arg(&root)
            .arg("--rollout")
            .arg(root.join("rollout.jsonl"))
            .arg("--model")
            .arg(sterna::wire::MODEL)
            .env("ANTHROPIC_BASE_URL", refused_base_url())
            .stdin(std::process::Stdio::null())
            .output()
            .unwrap()
    };
    let output = run();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "a saved voice stopped sterna:\n{stdout}\n{stderr}"
    );
    assert!(
        stdout.contains("`ui.voice = playful` is no longer a setting")
            && stdout.contains("one plain voice"),
        "{stdout}"
    );
    let saved = fs::read_to_string(&config).unwrap();
    assert!(!saved.contains("voice"), "{saved}");
    assert!(
        saved.contains("amber"),
        "the rest of the file is kept: {saved}"
    );
    let again = run();
    let stdout = String::from_utf8_lossy(&again.stdout);
    assert!(again.status.success(), "{stdout}");
    assert!(!stdout.contains("ui.voice"), "told only once: {stdout}");
}

/// `ui.reduced_motion` is gone -- motion off is the one switch. A file that
/// still says `reduced_motion = true` starts, is told once what does the
/// same now, and loses the line.
#[test]
fn a_saved_reduced_motion_is_removed_and_says_what_replaces_it() {
    let root = scratch_dir("retired-reduced-motion");
    fs::create_dir_all(root.join(".sterna")).unwrap();
    let config = root.join(".sterna/config.toml");
    fs::write(&config, "[ui]\nreduced_motion = true\ntheme = \"amber\"\n").unwrap();
    let run = || {
        Command::new(env!("CARGO_BIN_EXE_sterna"))
            .arg("session")
            .arg("--root")
            .arg(&root)
            .arg("--rollout")
            .arg(root.join("rollout.jsonl"))
            .arg("--model")
            .arg(sterna::wire::MODEL)
            .env("ANTHROPIC_BASE_URL", refused_base_url())
            .stdin(std::process::Stdio::null())
            .output()
            .unwrap()
    };
    let output = run();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "a saved reduced_motion stopped sterna:\n{stdout}\n{stderr}"
    );
    assert!(
        stdout.contains("`ui.reduced_motion = true` is no longer a setting")
            && stdout.contains("/motion off"),
        "{stdout}"
    );
    let saved = fs::read_to_string(&config).unwrap();
    assert!(!saved.contains("reduced_motion"), "{saved}");
    assert!(
        saved.contains("amber"),
        "the rest of the file is kept: {saved}"
    );
    let again = run();
    let stdout = String::from_utf8_lossy(&again.stdout);
    assert!(again.status.success(), "{stdout}");
    assert!(
        !stdout.contains("reduced_motion"),
        "told only once: {stdout}"
    );
}

/// `/login custom` is one form: the URL, what it speaks, and a key. The
/// gateway is told to add the endpoint and is handed the key on stdin --
/// never in argv, where any process could read it.
#[cfg(unix)]
#[test]
fn a_custom_endpoint_is_one_form_and_its_key_goes_on_stdin() {
    const KEY: &str = "sk-custom-secret-value";
    let root = scratch_dir("custom-endpoint-root");
    let rollout = root.join("rollout.jsonl");
    let record = root.join("gateway-argv.txt");
    let base_url = refused_base_url();
    let gateway = write_fake_gateway(&root, "fake_gateway.sh", &record, &base_url, "unused");
    let output = run_session_stdin_with_gateway(
        &root,
        &rollout,
        "sess-custom-endpoint",
        &["/login custom", "https://api.example.com/v1", KEY],
        None,
        &gateway,
    );
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let seen = fs::read_to_string(&record).unwrap();
    assert!(
        seen.lines().any(|line| line
            == "providers add example --base-url https://api.example.com/v1 --protocol openai-chat --json"),
        "{seen}"
    );
    assert!(
        seen.lines()
            .any(|line| line == "credentials set example --json"),
        "{seen}"
    );
    assert!(!seen.contains(KEY), "the key reached argv: {seen}");
    assert_eq!(
        fs::read_to_string(root.join("gateway-argv.txt.stdin")).unwrap(),
        KEY
    );
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("Connected example"),
        "{}",
        String::from_utf8_lossy(&output.stdout)
    );
}
