//! Request modes through the built `sterna` binary (map lines 2637, 2638).
//!
//! Every test ignores the system prompt's mode line and has the scripted model
//! invoke the refused tool anyway: the prompt informs, the narrowed profile is
//! what refuses. The decisive assertion is always the filesystem — a refused
//! write left no file — with the refusal's rule text as the second half.

use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;

static COUNTER: AtomicU64 = AtomicU64::new(0);

fn scratch_dir(label: &str) -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "sterna-request-modes-{label}-{}-{n}",
        std::process::id()
    ));
    fs::create_dir_all(&dir).unwrap();
    dir
}

/// A loopback provider answering each request from its body.
fn start_provider<F>(turns: usize, answer: F) -> (String, Arc<Mutex<Vec<String>>>)
where
    F: Fn(&str) -> String + Send + 'static,
{
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let bodies = Arc::new(Mutex::new(Vec::new()));
    let seen = Arc::clone(&bodies);
    thread::spawn(move || {
        for _ in 0..turns {
            let Ok((stream, _)) = listener.accept() else {
                return;
            };
            answer_one(stream, &answer, &seen);
        }
    });
    (format!("http://127.0.0.1:{port}"), bodies)
}

fn answer_one<F: Fn(&str) -> String>(
    mut stream: TcpStream,
    answer: &F,
    bodies: &Mutex<Vec<String>>,
) {
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
    if reader.read_exact(&mut body).is_err() {
        return;
    }
    let body = String::from_utf8_lossy(&body).into_owned();
    let reply = answer(&body);
    bodies.lock().unwrap().push(body);
    let _ = stream.write_all(
        format!(
            "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
            reply.len()
        )
        .as_bytes(),
    );
    let _ = stream.write_all(reply.as_bytes());
    let _ = stream.flush();
}

fn cell_reply(code: &str) -> String {
    serde_json::json!({
        "role": "assistant",
        "content": [{"type": "text", "text": format!("```sterna\n{code}\n```")}],
    })
    .to_string()
}

/// A project whose profile admits every command line, so a refusal the tests
/// observe is the mode's.
fn project(label: &str) -> PathBuf {
    let root = scratch_dir(label);
    fs::create_dir_all(root.join(".sterna")).unwrap();
    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(
        root.join(".sterna/config.toml"),
        "[permissions]\nallow = [\"Bash\"]\n",
    )
    .unwrap();
    fs::write(root.join("src/lib.rs"), "pub fn existing() {}\n").unwrap();
    root
}

/// [`project`] plus `extra` appended to `.sterna/config.toml` -- the mode
/// proposal (2639) and the explore overlay's configured keys (2637) each
/// need `[decisions]` or `[modes.explore]` beside the existing
/// `[permissions]`.
fn project_with_config(label: &str, extra: &str) -> PathBuf {
    let root = project(label);
    let path = root.join(".sterna/config.toml");
    let mut config = fs::read_to_string(&path).unwrap();
    config.push_str(extra);
    fs::write(&path, config).unwrap();
    root
}

/// [`start_provider`] plus `/v1/systemone`'s intent question, answered every
/// time with the same scripted `choice`/`confidence` -- one task asks it
/// once, before the first turn (`decision-model.md` §2), and every test here
/// scripts one intent for the whole run.
fn start_provider_with_intent(
    cells: Vec<String>,
    intent_choice: &'static str,
    intent_confidence: f64,
) -> (String, Arc<Mutex<Vec<String>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let bodies = Arc::new(Mutex::new(Vec::new()));
    let seen = Arc::clone(&bodies);
    thread::spawn(move || {
        let mut cells = cells.into_iter();
        loop {
            let Ok((stream, _)) = listener.accept() else {
                return;
            };
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut request_line = String::new();
            if reader.read_line(&mut request_line).unwrap_or(0) == 0 {
                continue;
            }
            let path = request_line
                .split_whitespace()
                .nth(1)
                .unwrap_or_default()
                .to_string();
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
            if reader.read_exact(&mut body).is_err() {
                return;
            }
            let body = String::from_utf8_lossy(&body).into_owned();
            let mut stream = stream;
            let reply = if path == "/v1/systemone" {
                serde_json::json!({
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
                            "choice": "routine",
                            "probabilities": {"trivial": 0.0, "routine": 0.0, "needs_exploration": 0.0},
                            "confidence": 0.50,
                        },
                    },
                    "usage": {"input_tokens": 40, "output_tokens": 12},
                })
                .to_string()
            } else {
                seen.lock().unwrap().push(body);
                let Some(reply) = cells.next() else {
                    return;
                };
                reply
            };
            let _ = stream.write_all(
                format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
                    reply.len()
                )
                .as_bytes(),
            );
            let _ = stream.write_all(reply.as_bytes());
            let _ = stream.flush();
        }
    });
    (format!("http://127.0.0.1:{port}"), bodies)
}

// `hold_above` is pinned above every confidence these tests script so the
// pre-existing hold (2616, decision-model.md §3) never fires here and only
// the mode proposal (2639) is under test.
const DECISIONS_ON: &str =
    "[decisions]\nmodel = \"jev-latest\"\nmode = \"on\"\nmode_above = 0.85\nhold_above = 0.99\n";
const DECISIONS_SHADOW: &str = "[decisions]\nmodel = \"jev-latest\"\nmode = \"shadow\"\nmode_above = 0.85\nhold_above = 0.99\n";

/// `sterna session` with `args`, `inputs` piped one per line.
fn run(root: &Path, args: &[&str], inputs: &[&str], base_url: &str) -> std::process::Output {
    let rollout = scratch_dir("rollout").join("rollout.jsonl");
    let mut command = Command::new(env!("CARGO_BIN_EXE_sterna"));
    command
        .arg("session")
        .arg("--root")
        .arg(root)
        .arg("--rollout")
        .arg(&rollout)
        .arg("--session")
        .arg(format!(
            "sess-mode-{}",
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ))
        .arg("--model")
        .arg(sterna::wire::MODEL)
        .args(args)
        .env("ANTHROPIC_BASE_URL", base_url)
        .env("XDG_CONFIG_HOME", scratch_dir("global-config"))
        .env_remove("ANTHROPIC_AUTH_TOKEN")
        .env_remove("ANTHROPIC_API_KEY")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command.spawn().unwrap();
    {
        let stdin = child.stdin.as_mut().unwrap();
        for line in inputs {
            writeln!(stdin, "{line}").unwrap();
        }
    }
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    output
}

/// One cell that attempts a write, and returns what happened as a string.
fn attempt_write(file: &str) -> String {
    format!(
        "let out;\ntry {{ await write({{ path: \"{file}\", content: \"changed\" }}); out = \"wrote\"; }} catch (e) {{ out = \"refused: \" + e.message; }}\nanswer(out);"
    )
}

#[test]
fn explore_refuses_a_write_the_model_attempts_despite_the_prompt() {
    let root = project("explore-write");
    let (base_url, bodies) = start_provider(1, |_| cell_reply(&attempt_write("src/new.rs")));
    let output = run(&root, &["--mode", "explore"], &["add a module"], &base_url);
    let stdout = String::from_utf8_lossy(&output.stdout);

    assert!(
        !root.join("src/new.rs").exists(),
        "explore wrote outside its globs: {stdout}"
    );
    assert!(
        stdout.contains("mode explore: writes only under"),
        "{stdout}"
    );
    let bodies = bodies.lock().unwrap();
    assert!(
        bodies[0].contains("Request mode: explore"),
        "the prompt did not name the mode"
    );
}

/// The control half: the same scripted write lands in `execute`, so the
/// refusal above is the mode's and not a broken fixture.
#[test]
fn execute_runs_the_same_write() {
    let root = project("execute-write");
    let (base_url, bodies) = start_provider(1, |_| cell_reply(&attempt_write("src/new.rs")));
    run(&root, &[], &["add a module"], &base_url);
    assert_eq!(
        fs::read_to_string(root.join("src/new.rs")).unwrap(),
        "changed"
    );
    assert!(!bodies.lock().unwrap()[0].contains("Request mode:"));
}

#[test]
fn plan_reads_and_refuses_a_write() {
    let root = project("plan");
    let code = "const file = await read({ path: \"src/lib.rs\" });\nlet out = \"read:\" + file.preview;\ntry { await write({ path: \"PLAN.md\", content: \"changed\" }); out += \"|wrote\"; } catch (e) { out += \"|refused: \" + e.message; }\nanswer(out);";
    let (base_url, _) = start_provider(1, move |_| cell_reply(code));
    let output = run(&root, &["--plan"], &["plan the change"], &base_url);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        !root.join("PLAN.md").exists(),
        "plan executed a change: {stdout}"
    );
    assert!(stdout.contains("existing"), "plan could not read: {stdout}");
    assert!(stdout.contains("mode plan: no change executes"), "{stdout}");
}

/// `/mode execute` between requests lifts the narrowing from the next one.
#[test]
fn slash_mode_execute_restores_writes_from_the_next_request() {
    let root = project("mode-switch");
    let (base_url, _) = start_provider(2, |body| {
        if body.contains("second request") {
            cell_reply(&attempt_write("src/second.rs"))
        } else {
            cell_reply(&attempt_write("src/first.rs"))
        }
    });
    let output = run(
        &root,
        &["--mode", "explore"],
        &["first request", "/mode execute", "second request"],
        &base_url,
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(!root.join("src/first.rs").exists(), "{stdout}");
    assert!(
        root.join("src/second.rs").exists(),
        "/mode execute did not lift the narrowing: {stdout}"
    );
}

/// The shell stays read-only in explore: a redirect and `rm` are refused, a
/// read-only command runs.
#[cfg(unix)]
#[test]
fn explore_bash_runs_read_only_commands_and_refuses_writers() {
    let root = project("explore-bash");
    fs::write(root.join("victim.txt"), "keep").unwrap();
    let code = "const listed = await bash({ command: \"ls src\" });\nlet out = \"ls:\" + listed.stdout;\nfor (const command of [\"echo x > made.txt\", \"rm victim.txt\"]) {\n  try { await bash({ command }); out += \"|ran \" + command; } catch (e) { out += \"|refused: \" + e.message; }\n}\nanswer(out);";
    let (base_url, _) = start_provider(1, move |_| cell_reply(code));
    let output = run(&root, &["--mode", "explore"], &["look around"], &base_url);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(!root.join("made.txt").exists(), "{stdout}");
    assert!(root.join("victim.txt").exists(), "{stdout}");
    assert!(
        stdout.contains("lib.rs"),
        "a read-only command did not run: {stdout}"
    );
    assert!(stdout.contains("writes through a redirect"), "{stdout}");
    assert!(
        stdout.contains("`rm` is not a read-only command"),
        "{stdout}"
    );
}

/// `/tool` is the direct frame a person types; it takes the same narrowing.
#[test]
fn a_direct_tool_frame_is_refused_by_the_same_rule() {
    let root = project("explore-tool");
    let (base_url, _) = start_provider(0, |_| String::new());
    let output = run(
        &root,
        &["--mode", "explore"],
        &["/tool write path=src/direct.rs content=changed"],
        &base_url,
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(!root.join("src/direct.rs").exists(), "{stdout}");
    assert!(
        stdout.contains("mode explore: writes only under"),
        "{stdout}"
    );
}

/// `explore` writes its scratchpad — the default writable glob, carved out of
/// `.sterna/**` — and is refused one directory over.
#[test]
fn explore_writes_the_scratchpad_and_is_refused_outside_it() {
    let root = project("explore-scratch");
    let code = format!(
        "{}\n{}",
        attempt_write(".sterna/scratch/notes.md").replace("answer(out);", "let first = out;"),
        attempt_write("src/x.rs").replace("answer(out);", "answer(first + \"|\" + out);")
    );
    let (base_url, _) = start_provider(1, move |_| cell_reply(&code));
    let output = run(&root, &["--mode", "explore"], &["take notes"], &base_url);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(
        fs::read_to_string(root.join(".sterna/scratch/notes.md"))
            .ok()
            .as_deref(),
        Some("changed"),
        "explore could not write its scratchpad: {stdout}"
    );
    assert!(!root.join("src/x.rs").exists(), "{stdout}");
    assert!(
        stdout.contains("mode explore: writes only under"),
        "{stdout}"
    );
}

fn write_all(files: &[&str]) -> String {
    let mut code = String::from("let out = \"\";\n");
    for file in files {
        code.push_str(&format!(
            "try {{ await write({{ path: \"{file}\", content: \"step one\\nstep two\\n\" }}); out += \"|wrote {file}\"; }} catch (e) {{ out += \"|refused {file}: \" + e.message; }}\n"
        ));
    }
    code.push_str("answer(out);");
    code
}

/// `plan` writes `.sterna/scratch/plan.md` and nothing else, and says so.
#[test]
fn plan_writes_its_plan_file_and_is_refused_every_other_write() {
    let root = project("plan-file");
    let code = write_all(&[
        ".sterna/scratch/plan.md",
        ".sterna/scratch/notes.md",
        "PLAN.md",
    ]);
    let (base_url, bodies) = start_provider(1, move |_| cell_reply(&code));
    let output = run(&root, &["--plan"], &["plan the change"], &base_url);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(root.join(".sterna/scratch/plan.md").exists(), "{stdout}");
    assert!(!root.join(".sterna/scratch/notes.md").exists(), "{stdout}");
    assert!(!root.join("PLAN.md").exists(), "{stdout}");
    assert!(stdout.contains("mode plan: no change executes"), "{stdout}");
    assert!(
        stdout.contains("plan written: .sterna/scratch/plan.md (2 lines)"),
        "{stdout}"
    );
    assert!(bodies.lock().unwrap()[0].contains("write the plan to .sterna/scratch/plan.md"));
}

const PLAN_SECTION: &str = "## Plan (.sterna/scratch/plan.md)";

/// The plan a `plan` request wrote is carried by the next request once, and
/// not by the one after it.
#[test]
fn a_written_plan_reaches_the_next_request_once() {
    let root = project("plan-carry");
    let (base_url, bodies) = start_provider(3, |body| {
        if body.contains("plan the change") && !body.contains("carry it out") {
            cell_reply(&write_all(&[".sterna/scratch/plan.md"]))
        } else {
            cell_reply("answer(\"done\");")
        }
    });
    let output = run(
        &root,
        &["--plan"],
        &[
            "plan the change",
            "/mode execute",
            "carry it out",
            "and again",
        ],
        &base_url,
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let bodies = bodies.lock().unwrap();
    assert_eq!(bodies.len(), 3, "{stdout}");
    assert!(!bodies[0].contains(PLAN_SECTION));
    assert_eq!(
        bodies[1].matches(PLAN_SECTION).count(),
        1,
        "the next request did not carry the plan once: {stdout}"
    );
    assert!(bodies[1].contains("step one"));
    // The plan rides in the next task's own message, so a later request
    // still holds it once, as history -- never as a fresh copy.
    assert_eq!(
        bodies[2].matches(PLAN_SECTION).count(),
        1,
        "the plan was carried past the next request"
    );
}

/// A plan request that writes no plan hands nothing on.
#[test]
fn a_plan_request_that_writes_nothing_carries_nothing() {
    let root = project("plan-nothing");
    let (base_url, bodies) = start_provider(2, |_| cell_reply("answer(\"no plan\");"));
    let output = run(
        &root,
        &["--plan"],
        &["plan the change", "/mode execute", "carry it out"],
        &base_url,
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let bodies = bodies.lock().unwrap();
    assert_eq!(bodies.len(), 2, "{stdout}");
    assert!(!bodies[1].contains(PLAN_SECTION), "{stdout}");
    assert!(!stdout.contains("plan written"), "{stdout}");
}

// -- the mode proposed from the intent (2639) ----------------------------

/// A confident `read_only` intent in `execute`, unpinned, proposes `explore`
/// for that one request: the scripted write is refused and the applied line
/// names the mode and the confidence.
#[test]
fn a_confident_read_only_intent_proposes_explore_for_one_request() {
    let root = project_with_config("mode-proposal-applied", DECISIONS_ON);
    let (base_url, _) = start_provider_with_intent(
        vec![cell_reply(&attempt_write("src/new.rs"))],
        "read_only",
        0.97,
    );
    let output = run(&root, &[], &["add a module"], &base_url);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(!root.join("src/new.rs").exists(), "{stdout}");
    assert!(
        stdout.contains("mode explore: writes only under"),
        "{stdout}"
    );
    assert!(
        stdout
            .contains("decision: explore for this request (read_only 0.97); /mode execute to pin"),
        "{stdout}"
    );
    // The advice is the person's; the classifier's raw answer is not.
    assert!(!stdout.contains("decision: intent"), "{stdout}");
}

/// Between 0.5 and `mode_above`, Sterna offers `/mode explore` with one line
/// and runs the request in the session's own mode, unchanged.
#[test]
fn a_read_only_intent_below_mode_above_offers_explore_and_runs_as_today() {
    let root = project_with_config("mode-proposal-below", DECISIONS_ON);
    let (base_url, _) = start_provider_with_intent(
        vec![cell_reply(&attempt_write("src/new.rs"))],
        "read_only",
        0.70,
    );
    let output = run(&root, &[], &["add a module"], &base_url);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(root.join("src/new.rs").exists(), "{stdout}");
    assert!(
        stdout.contains("decision: read_only 0.70 below mode_above; /mode explore to pin"),
        "{stdout}"
    );
}

/// `/mode execute` pins: a later confident intent never proposes.
#[test]
fn mode_execute_pins_and_a_confident_intent_never_proposes() {
    let root = project_with_config("mode-proposal-pinned", DECISIONS_ON);
    let (base_url, _) = start_provider_with_intent(
        vec![cell_reply(&attempt_write("src/new.rs"))],
        "read_only",
        0.97,
    );
    let output = run(&root, &[], &["/mode execute", "add a module"], &base_url);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(root.join("src/new.rs").exists(), "{stdout}");
    assert!(
        !stdout.contains("decision: explore for this request"),
        "{stdout}"
    );
}

/// `/mode auto` clears the pin: the same confident intent proposes again.
#[test]
fn mode_auto_unpins_and_a_confident_intent_proposes_again() {
    let root = project_with_config("mode-proposal-unpinned", DECISIONS_ON);
    let (base_url, _) = start_provider_with_intent(
        vec![cell_reply(&attempt_write("src/new.rs"))],
        "read_only",
        0.97,
    );
    let output = run(
        &root,
        &[],
        &["/mode execute", "/mode auto", "add a module"],
        &base_url,
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(!root.join("src/new.rs").exists(), "{stdout}");
    assert!(
        stdout
            .contains("decision: explore for this request (read_only 0.97); /mode execute to pin"),
        "{stdout}"
    );
}

/// `mode = shadow` runs the request as today and only counts
/// `mode_proposal.would_apply` -- never narrows, never prints the applied
/// line.
#[test]
fn shadow_mode_runs_as_today_and_counts_would_apply() {
    let root = project_with_config("mode-proposal-shadow", DECISIONS_SHADOW);
    let (base_url, _) =
        start_provider_with_intent(vec![cell_reply("answer(\"done\");")], "read_only", 0.97);
    let out_path = root.join("stdout.json");
    let stdout_file = std::fs::File::create(&out_path).unwrap();
    let status = Command::new(env!("CARGO_BIN_EXE_sterna"))
        .args(["exec", "add a module", "--output-format", "json", "--root"])
        .arg(&root)
        .args(["--model", sterna::wire::MODEL])
        .env("ANTHROPIC_BASE_URL", &base_url)
        .env("XDG_CONFIG_HOME", root.join("global-config"))
        .env_remove("ANTHROPIC_AUTH_TOKEN")
        .env_remove("ANTHROPIC_API_KEY")
        .stdout(stdout_file)
        .stderr(Stdio::null())
        .status()
        .unwrap();
    assert!(status.success());
    let result: serde_json::Value = serde_json::from_slice(&fs::read(&out_path).unwrap()).unwrap();
    let mode_proposal = &result["telemetry"]["decisions"]["mode_proposal"];
    assert_eq!(mode_proposal["would_apply"], true, "{mode_proposal}");
    assert_eq!(mode_proposal["applied"], false, "{mode_proposal}");
    assert_eq!(mode_proposal["proposed"], true, "{mode_proposal}");
}

/// `[modes.explore] writable = [...]` and `commands = [...]` reach the
/// overlay: a configured documentation glob is writable in `explore` and a
/// configured command pattern runs read-only there (2637's open half).
#[cfg(unix)]
#[test]
fn configured_explore_writable_and_commands_reach_the_overlay() {
    let root = project_with_config(
        "explore-configured-overlay",
        "[modes.explore]\nwritable = [\"docs/**\"]\ncommands = [\"cargo metadata*\"]\n",
    );
    fs::create_dir_all(root.join("docs")).unwrap();
    let code = format!(
        "{}\n{}\nlet third;\ntry {{ await bash({{ command: \"cargo metadata\" }}); third = \"ran\"; }} catch (e) {{ third = \"refused: \" + e.message; }}\nanswer(first + \"|\" + second + \"|\" + third);",
        attempt_write("docs/notes.md").replace("answer(out);", "let first = out;"),
        attempt_write("src/x.rs").replace("answer(out);", "let second = out;"),
    );
    let (base_url, _) = start_provider(1, move |_| cell_reply(&code));
    let output = run(&root, &["--mode", "explore"], &["take notes"], &base_url);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(
        fs::read_to_string(root.join("docs/notes.md"))
            .ok()
            .as_deref(),
        Some("changed"),
        "the configured doc glob was not writable: {stdout}"
    );
    assert!(!root.join("src/x.rs").exists(), "{stdout}");
    assert!(stdout.contains("|ran"), "{stdout}");
}
