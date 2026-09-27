//! Real-process regressions for cell deadlines, task cancellation and SIGTERM.
#![cfg(unix)]
use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use sterna::contract::SessionId;
use sterna::runtime::isolate::{DEFAULT_HEAP_LIMIT_BYTES, Runtime};
use sterna::runtime::outcome::CellOutcome;
use sterna::runtime::preview::Value;
use sterna::sandbox::profile::Profile;

struct Fixture {
    root: PathBuf,
}
impl Fixture {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let root = std::env::temp_dir().join(format!(
            "sterna-cutoff-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&root).unwrap();
        Self {
            root: fs::canonicalize(root).unwrap(),
        }
    }
    fn profile(&self) -> Profile {
        Profile::compile(&self.root, Some(&serde_json::json!({"permissions":{"allow":[
            format!("Read({}/**)",self.root.display()),format!("Write({}/**)",self.root.display()),"Bash".to_string()
        ]}}).to_string()))
    }
    fn command(&self, base: &str) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_sterna"));
        command
            .args(["session", "--root"])
            .arg(&self.root)
            .arg("--model")
            .arg(sterna::wire::MODEL)
            .args([
                "--task",
                "Verify cutoff",
                "--session",
                "cutoff",
                "--yolo",
                "--rollout",
            ])
            .arg(self.root.join("rollout.jsonl"))
            .env("ANTHROPIC_BASE_URL", base)
            .env("XDG_CONFIG_HOME", self.root.join("global-config"))
            .env_remove("ANTHROPIC_AUTH_TOKEN")
            .env_remove("ANTHROPIC_API_KEY")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped());
        command
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn wait_started(f: &Fixture, child: &mut Child) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        if f.root.join("started").exists() {
            return;
        }
        if let Some(status) = child.try_wait().unwrap() {
            panic!("Sterna exited before its child started: {status}");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let _ = child.kill();
    let _ = child.wait();
    panic!("child never started");
}
fn wait_exit(child: &mut Child) -> std::process::ExitStatus {
    let deadline = Instant::now() + Duration::from_secs(12);
    while Instant::now() < deadline {
        if let Some(status) = child.try_wait().unwrap() {
            return status;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let _ = child.kill();
    let _ = child.wait();
    panic!("Sterna did not finish within the bound");
}
fn code(source: &str) -> String {
    serde_json::json!({"role":"assistant","content":[{"type":"tool_use","id":"cell","name":"execute_cell","input":{"code":source}}]}).to_string()
}
fn provider(first: String) -> (String, Arc<Mutex<Vec<String>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let address = format!("http://{}", listener.local_addr().unwrap());
    let bodies = Arc::new(Mutex::new(Vec::new()));
    let captured = bodies.clone();
    std::thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(20);
        while Instant::now() < deadline {
            let Ok((mut stream, _)) = listener.accept() else {
                std::thread::sleep(Duration::from_millis(5));
                continue;
            };
            stream.set_nonblocking(false).unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
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
                if let Some(n) = line.to_lowercase().strip_prefix("content-length:") {
                    length = n.trim().parse::<usize>().unwrap();
                }
            }
            let mut body = vec![0; length];
            reader.read_exact(&mut body).unwrap();
            let mut captured = captured.lock().unwrap();
            let index = captured.len();
            captured.push(String::from_utf8(body).unwrap());
            drop(captured);
            let response = if index == 0 {
                first.clone()
            } else {
                code("answer('recovered');")
            };
            let _ = write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                response.len(),
                response
            );
        }
    });
    (address, bodies)
}

/// The wall-clock limit bounds the cell's own computing, so a granted child
/// that outlives it is waited for rather than reaped.
///
/// **This expectation genuinely changed on 2026-09-18** (the user: "ein Rust
/// compile step für glasshouse oder blast radius dauert manchmal mehrere
/// Minuten"). It read `foreground_deadline_kills_late_write_and_remains_
/// recoverable` and pinned the opposite: a `sleep 3` under a one-second limit
/// was killed mid-run and the call answered `cancelled`, because the tool
/// path polls the watchdog's own flag. A build is the work, not a hang.
///
/// What it still pins: the child's output reaches the cell, the write lands
/// *before* the cell ends rather than after it, and the isolate survives. The
/// orphaned-child invariant this test used to carry now belongs to
/// cancellation, where it is pinned by the sibling below -- a person stopping
/// a task still kills its children.
#[test]
fn a_child_that_outlives_the_compute_limit_is_waited_for_not_reaped() {
    let command = "echo $$ > started; sleep 3; printf late > marker";
    let f = Fixture::new();
    let mut runtime = Runtime::with_limits(
        &f.profile(),
        &SessionId::new("deadline"),
        DEFAULT_HEAP_LIMIT_BYTES,
        Duration::from_secs(1),
    );
    let began = Instant::now();
    let outcome = runtime.run_cell(&format!(
        "const r = await bash({{command:{}}}); return r.exit_code;",
        serde_json::to_string(command).unwrap()
    ));
    assert!(f.root.join("started").exists(), "fixture did not run");
    assert!(
        matches!(
            &outcome,
            CellOutcome::Returned {
                value: Value::Number(code),
                ..
            } if *code == 0.0
        ),
        "a three-second build under a one-second compute limit must finish: {outcome:?}"
    );
    assert!(
        began.elapsed() >= Duration::from_secs(3),
        "the cell did not actually wait for its child"
    );
    assert!(
        f.root.join("marker").exists(),
        "the child's own write never landed"
    );
    assert!(!runtime.poisoned(), "waiting poisoned the isolate");
    assert!(matches!(
        runtime.run_cell("return 7;"),
        CellOutcome::Returned {
            value: Value::Number(7.0),
            ..
        }
    ));
}

/// The reason the limit exists: `while (true) {}` allocates nothing, so the
/// heap ceiling never sees it. Pausing the clock for host work must not
/// weaken this.
#[test]
fn a_loop_that_only_computes_still_dies_at_the_compute_limit() {
    let f = Fixture::new();
    let mut runtime = Runtime::with_limits(
        &f.profile(),
        &SessionId::new("runaway"),
        DEFAULT_HEAP_LIMIT_BYTES,
        Duration::from_secs(1),
    );
    let began = Instant::now();
    let outcome = runtime.run_cell("while (true) {}");
    assert!(
        matches!(&outcome, CellOutcome::Threw { error, .. } if error.class == "RuntimeTimeout"),
        "{outcome:?}"
    );
    assert!(
        began.elapsed() < Duration::from_secs(10),
        "the runaway loop was not stopped near its limit"
    );
}

/// A child that hangs still ends the cell -- through its own bound, which is
/// what the cell now relies on instead of its compute clock.
#[test]
fn a_child_that_hangs_ends_by_its_own_timeout_and_says_so() {
    let f = Fixture::new();
    let mut runtime = Runtime::with_limits(
        &f.profile(),
        &SessionId::new("hung-child"),
        DEFAULT_HEAP_LIMIT_BYTES,
        Duration::from_secs(1),
    );
    let began = Instant::now();
    let outcome = runtime.run_cell("return await bash({command:'sleep 30', timeout: 1200});");
    assert!(
        began.elapsed() < Duration::from_secs(20),
        "the cell waited past the child's own bound: {outcome:?}"
    );
    let rendered = format!("{outcome:?}");
    assert!(
        rendered.contains("1200") || rendered.to_lowercase().contains("timeout"),
        "the ending must name the bound that ended it: {rendered}"
    );
}

#[test]
fn task_interrupt_kills_late_write_then_recovers_without_poisoning() {
    let f = Fixture::new();
    let (base, bodies) = provider(code(
        "await bash({command:'echo $$ > started; sleep 3; printf late > marker'});",
    ));
    let mut child = f.command(&base).spawn().unwrap();
    wait_started(&f, &mut child);
    assert!(
        Command::new("kill")
            .args(["-INT", &child.id().to_string()])
            .status()
            .unwrap()
            .success()
    );
    assert!(wait_exit(&mut child).success());
    assert_eq!(bodies.lock().unwrap().len(), 2);
    std::thread::sleep(Duration::from_secs(3));
    assert!(!f.root.join("marker").exists());
    let rollout = fs::read_to_string(f.root.join("rollout.jsonl")).unwrap();
    assert!(rollout.contains("Cancelled"));
}

#[test]
fn sigterm_cancels_owned_foreground_group_before_exit() {
    let f = Fixture::new();
    let (base, bodies) = provider(code(
        "await bash({command:'echo $$ > started; sleep 3; printf late > marker'});",
    ));
    let mut child = f.command(&base).spawn().unwrap();
    wait_started(&f, &mut child);
    let began = Instant::now();
    assert!(
        Command::new("kill")
            .args(["-TERM", &child.id().to_string()])
            .status()
            .unwrap()
            .success()
    );
    assert_eq!(wait_exit(&mut child).code(), Some(143));
    assert!(began.elapsed() < Duration::from_secs(2));
    assert_eq!(
        bodies.lock().unwrap().len(),
        1,
        "termination started another provider request"
    );
    std::thread::sleep(Duration::from_secs(3));
    assert!(!f.root.join("marker").exists());
}

#[test]
fn successful_foreground_exit_stops_remaining_writers_with_inherited_or_closed_stdio() {
    for command in [
        "echo $$ > started; (sleep 3; printf late > marker) & printf foreground; exit 7",
        "echo $$ > started; (sleep 3; printf late > marker) >/dev/null 2>&1 & printf foreground; exit 7",
    ] {
        let f = Fixture::new();
        let mut runtime = Runtime::with_limits(
            &f.profile(),
            &SessionId::new("detached"),
            DEFAULT_HEAP_LIMIT_BYTES,
            Duration::from_secs(2),
        );
        let outcome = runtime.run_cell(&format!("const result = await bash({{command:{}}}); return result.stdout + ':' + result.exit_code;", serde_json::to_string(command).unwrap()));
        assert!(
            matches!(&outcome, CellOutcome::Returned { value: Value::String(text), .. } if text.head() == "foreground:7"),
            "{outcome:?}"
        );
        assert!(f.root.join("started").exists());
        std::thread::sleep(Duration::from_secs(3));
        assert!(
            !f.root.join("marker").exists(),
            "successful foreground call leaked a writer"
        );
    }
}
