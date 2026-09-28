//! The plan request through the built `sterna` binary: `/plan <task>` runs
//! one request that reads, writes only `.sterna/scratch/plan.md` and runs
//! only read-only commands; the request after it works as usual.
//!
//! Every test ignores the task's plan line and has the scripted model invoke
//! the refused tool anyway: the prompt informs, the narrowed profile is what
//! refuses. The decisive assertion is always the filesystem — a refused
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

/// A project whose `Bash` allow pre-approves every command line, so no
/// question interrupts a piped run and a refusal the tests observe is the
/// plan's.
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

const PLAN_REFUSAL: &str = "plan: no change executes";
const PLAN_LINE: &str = "This is a plan request.";

#[test]
fn plan_refuses_a_write_the_model_attempts_despite_the_prompt() {
    let root = project("plan-write");
    let (base_url, bodies) = start_provider(1, |_| cell_reply(&attempt_write("src/new.rs")));
    let output = run(&root, &[], &["/plan add a module"], &base_url);
    let stdout = String::from_utf8_lossy(&output.stdout);

    assert!(
        !root.join("src/new.rs").exists(),
        "plan wrote outside its plan file: {stdout}"
    );
    assert!(stdout.contains(PLAN_REFUSAL), "{stdout}");
    let bodies = bodies.lock().unwrap();
    assert!(
        bodies[0].contains(PLAN_LINE),
        "the request did not say it plans"
    );
}

/// The control half: the same scripted write lands in an ordinary request,
/// so the refusal above is the plan's and not a broken fixture.
#[test]
fn an_ordinary_request_runs_the_same_write() {
    let root = project("work-write");
    let (base_url, bodies) = start_provider(1, |_| cell_reply(&attempt_write("src/new.rs")));
    run(&root, &[], &["add a module"], &base_url);
    assert_eq!(
        fs::read_to_string(root.join("src/new.rs")).unwrap(),
        "changed"
    );
    assert!(!bodies.lock().unwrap()[0].contains(PLAN_LINE));
}

#[test]
fn plan_reads_and_refuses_a_write() {
    let root = project("plan");
    let code = "const file = await read({ path: \"src/lib.rs\" });\nlet out = \"read:\" + file.preview;\ntry { await write({ path: \"PLAN.md\", content: \"changed\" }); out += \"|wrote\"; } catch (e) { out += \"|refused: \" + e.message; }\nanswer(out);";
    let (base_url, _) = start_provider(1, move |_| cell_reply(code));
    let output = run(&root, &[], &["/plan plan the change"], &base_url);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        !root.join("PLAN.md").exists(),
        "plan executed a change: {stdout}"
    );
    assert!(stdout.contains("existing"), "plan could not read: {stdout}");
    assert!(stdout.contains(PLAN_REFUSAL), "{stdout}");
}

/// A plan is one request: the request after it writes again.
#[test]
fn the_request_after_a_plan_writes_again() {
    let root = project("plan-then-work");
    let (base_url, _) = start_provider(2, |body| {
        if body.contains("second request") {
            cell_reply(&attempt_write("src/second.rs"))
        } else {
            cell_reply(&attempt_write("src/first.rs"))
        }
    });
    let output = run(
        &root,
        &[],
        &["/plan first request", "second request"],
        &base_url,
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(!root.join("src/first.rs").exists(), "{stdout}");
    assert!(
        root.join("src/second.rs").exists(),
        "the plan's narrowing outlived its request: {stdout}"
    );
}

/// `/plan` without a task runs nothing and says how to use it.
#[test]
fn plan_without_a_task_says_how_to_use_it() {
    let root = project("plan-empty");
    let (base_url, bodies) = start_provider(0, |_| String::new());
    let output = run(&root, &[], &["/plan"], &base_url);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("Use /plan <what to plan>"), "{stdout}");
    assert!(bodies.lock().unwrap().is_empty(), "{stdout}");
}

/// The shell stays read-only while planning: a redirect and `rm` are
/// refused, a read-only command runs.
#[cfg(unix)]
#[test]
fn plan_bash_runs_read_only_commands_and_refuses_writers() {
    let root = project("plan-bash");
    fs::write(root.join("victim.txt"), "keep").unwrap();
    let code = "const listed = await bash({ command: \"ls src\" });\nlet out = \"ls:\" + listed.stdout;\nfor (const command of [\"echo x > made.txt\", \"rm victim.txt\"]) {\n  try { await bash({ command }); out += \"|ran \" + command; } catch (e) { out += \"|refused: \" + e.message; }\n}\nanswer(out);";
    let (base_url, _) = start_provider(1, move |_| cell_reply(code));
    let output = run(&root, &[], &["/plan look around"], &base_url);
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
    assert!(
        stdout.contains("the shell is read-only while planning"),
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

/// `plan` writes `.sterna/scratch/plan.md` and nothing else -- not even the
/// rest of the scratch folder -- and says so.
#[test]
fn plan_writes_its_plan_file_and_is_refused_every_other_write() {
    let root = project("plan-file");
    let code = write_all(&[
        ".sterna/scratch/plan.md",
        ".sterna/scratch/notes.md",
        "PLAN.md",
    ]);
    let (base_url, bodies) = start_provider(1, move |_| cell_reply(&code));
    let output = run(&root, &[], &["/plan plan the change"], &base_url);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(root.join(".sterna/scratch/plan.md").exists(), "{stdout}");
    assert!(!root.join(".sterna/scratch/notes.md").exists(), "{stdout}");
    assert!(!root.join("PLAN.md").exists(), "{stdout}");
    assert!(stdout.contains(PLAN_REFUSAL), "{stdout}");
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
        &[],
        &["/plan plan the change", "carry it out", "and again"],
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
    // The plan line rides in the plan request's own message: the next
    // request holds it once, as history, and never adds its own.
    assert_eq!(
        bodies[1].matches(PLAN_LINE).count(),
        1,
        "the request after a plan still planned: {stdout}"
    );
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
        &[],
        &["/plan plan the change", "carry it out"],
        &base_url,
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let bodies = bodies.lock().unwrap();
    assert_eq!(bodies.len(), 2, "{stdout}");
    assert!(!bodies[1].contains(PLAN_SECTION), "{stdout}");
    assert!(!stdout.contains("plan written"), "{stdout}");
}
