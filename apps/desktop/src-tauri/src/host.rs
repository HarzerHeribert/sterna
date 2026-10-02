//! Starting the engine: `sterna host --background` makes sure one host runs
//! for this user (starting it on its own, outside the app's process group,
//! if none answers) and prints that host's ready line, then exits. The line
//! is everything the UI needs to connect: `{"listening", "token", "pid",
//! "version", "protocol"}`.
//!
//! `host_start`, step by step:
//!
//! 1. Choose the engine ([`engine::choose`]): the command-line install
//!    unless the bundled engine is newer; `STERNA_BIN` instead, in a debug
//!    build only.
//! 2. A bundled engine on a DMG or AppImage mount is copied into the app's
//!    data folder and run from there ([`engine::prepare`]); a bundled engine
//!    is given its gateway in `INFERENCE_GATEWAY_BIN` unless that is set.
//! 3. Run `<engine> host --background` and read its ready line. If that
//!    engine cannot start a host, the other one is tried, once.
//! 4. If the host that answered is older than the engine chosen and runs no
//!    session, it is told to shut down and started again with that engine,
//!    once ([`renew`]). The ready line of the host that runs comes back.

use std::collections::VecDeque;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::path::Path;
use std::process::{Child, ChildStderr, Command, ExitStatus, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use tauri::{AppHandle, Manager};

use crate::conn::Announced;
use crate::engine::{self, Engine, Kind, Places};

/// How long the engine has to print its ready line.
const READY_WITHIN: Duration = Duration::from_secs(30);
/// How long a starter that failed gets to exit and finish its stderr.
const SETTLE_WITHIN: Duration = Duration::from_secs(3);
/// How long an old host gets to leave after `shutdown`.
const LEAVE_WITHIN: Duration = Duration::from_secs(5);
/// How long one answer from a host may take.
const ANSWER_WITHIN: Duration = Duration::from_secs(5);
/// How much of the engine's stderr an error carries.
const STDERR_LINES: usize = 20;
/// The longest stderr line kept, in bytes; the rest is cut.
const STDERR_LINE_BYTES: usize = 2000;

/// Makes sure the engine's host runs and returns its ready line as JSON.
#[tauri::command]
pub async fn host_start(app: AppHandle) -> Result<Value, String> {
    let places = Places::of(&app);
    let ready = tauri::async_runtime::spawn_blocking(move || start(&places))
        .await
        .map_err(|e| format!("The engine could not be started: {e}"))??;
    if let Some(listening) = ready["listening"].as_str() {
        app.state::<Announced>().add(listening);
    }
    Ok(ready)
}

fn start(places: &Places) -> Result<Value, String> {
    let mut failures = Vec::new();
    for mut engine in engine::choose(places)? {
        if let Err(failure) = engine::prepare(&mut engine, places) {
            failures.push(failure);
            continue;
        }
        match launch(engine.command(), &engine.shown(), READY_WITHIN) {
            Ok(ready) => {
                let host_json = places.data_folder.as_ref().map(|dir| dir.join("host.json"));
                return renew(ready, &mut engine, host_json.as_deref());
            }
            Err(failure) => failures.push(failure),
        }
    }
    Err(failures.join("\n\n"))
}

/// Replaces a host older than `engine` that runs no session: tells it to
/// shut down, waits for its `host.json` to go (or [`LEAVE_WITHIN`]), and
/// starts the host again with `engine`. Once, never in a loop; any other
/// host's ready line comes back as it is.
fn renew(ready: Value, engine: &mut Engine, host_json: Option<&Path>) -> Result<Value, String> {
    if engine.kind == Kind::Override {
        return Ok(ready);
    }
    let Some(running) = ready["version"].as_str().and_then(engine::parse_version) else {
        return Ok(ready);
    };
    let Some(ours) = engine.version() else {
        return Ok(ready);
    };
    if running >= ours || !shut_down_if_idle(&ready) {
        return Ok(ready);
    }
    let until = Instant::now() + LEAVE_WITHIN;
    while host_json.is_some_and(Path::exists) && Instant::now() < until {
        std::thread::sleep(Duration::from_millis(50));
    }
    launch(engine.command(), &engine.shown(), READY_WITHIN)
}

/// Asks the host for its list; if no session in it is live, tells it to
/// shut down. True when it was told.
fn shut_down_if_idle(ready: &Value) -> bool {
    let talk = || -> Option<bool> {
        let to: SocketAddr = ready["listening"].as_str()?.parse().ok()?;
        let stream = TcpStream::connect_timeout(&to, ANSWER_WITHIN).ok()?;
        stream.set_read_timeout(Some(ANSWER_WITHIN)).ok()?;
        let mut writer = stream.try_clone().ok()?;
        let mut reader = BufReader::new(stream);
        let mut ask = |line: Value| -> Option<Value> {
            writeln!(writer, "{line}").ok()?;
            let mut answer = String::new();
            reader.read_line(&mut answer).ok()?;
            serde_json::from_str(answer.trim()).ok()
        };
        let hello = json!({"hello": {
            "token": ready["token"].as_str()?,
            "protocol": ready["protocol"].as_u64().unwrap_or(1),
            "client": "desktop",
        }});
        ask(hello)?.get("welcome")?;
        let list = ask(json!({"do": "list"}))?;
        if !runs_no_session(list.get("ok")?) {
            return Some(false);
        }
        ask(json!({"do": "shutdown"}));
        Some(true)
    };
    talk().unwrap_or(false)
}

/// No session in a `list` answer has a `live` state.
fn runs_no_session(list: &Value) -> bool {
    list["folders"].as_array().is_some_and(|folders| {
        folders.iter().all(|folder| {
            folder["sessions"]
                .as_array()
                .is_none_or(|sessions| sessions.iter().all(|session| session["live"].is_null()))
        })
    })
}

/// What went wrong before a ready line arrived.
enum Failure {
    Spawn(std::io::Error),
    Read(std::io::Error),
    NoLine,
    NotReady(String),
    Timeout(Duration),
}

/// Runs `command host --background` and waits up to `within` for its line.
fn launch(mut command: Command, shown: &str, within: Duration) -> Result<Value, String> {
    command
        .args(["host", "--background"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command
        .spawn()
        .map_err(|e| explain(shown, &Failure::Spawn(e), None, ""))?;
    let tail = child.stderr.take().map(Tail::collect);
    let first = child.stdout.take().map(first_line);

    let outcome = match first.map(|line| line.recv_timeout(within)) {
        Some(Ok(Ok(line))) if line.is_empty() => Err(Failure::NoLine),
        Some(Ok(Ok(line))) => match serde_json::from_str::<Value>(line.trim()) {
            Ok(ready) if ready.is_object() => Ok(ready),
            _ => Err(Failure::NotReady(line)),
        },
        Some(Ok(Err(e))) => Err(Failure::Read(e)),
        Some(Err(mpsc::RecvTimeoutError::Timeout)) => Err(Failure::Timeout(within)),
        Some(Err(mpsc::RecvTimeoutError::Disconnected)) | None => Err(Failure::NoLine),
    };
    match outcome {
        Ok(ready) => {
            reap(child);
            Ok(ready)
        }
        Err(failure) => {
            // Only a starter that never answered is stopped: it is the one
            // process this app started, and nothing is waiting on it.
            if matches!(failure, Failure::Timeout(_)) {
                let _ = child.kill();
            }
            let status = settle(&mut child);
            let stderr = tail.map(|tail| tail.text()).unwrap_or_default();
            reap(child);
            Err(explain(shown, &failure, status, &stderr))
        }
    }
}

/// The first stdout line, read on a thread so the caller can time out.
///
/// After the first line the thread keeps reading to the end and throws the
/// rest away, so an engine that goes on writing never blocks on a full pipe
/// or dies on a closed one.
fn first_line(stdout: impl Read + Send + 'static) -> Receiver<std::io::Result<String>> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut reader = BufReader::new(stdout);
        let mut line = String::new();
        let _ = tx.send(reader.read_line(&mut line).map(|_| line));
        let _ = std::io::copy(&mut reader, &mut std::io::sink());
    });
    rx
}

/// The last [`STDERR_LINES`] lines the engine wrote to stderr.
struct Tail {
    lines: Arc<Mutex<VecDeque<String>>>,
    done: Receiver<()>,
}

impl Tail {
    fn collect(stderr: ChildStderr) -> Self {
        let lines = Arc::new(Mutex::new(VecDeque::new()));
        let (tx, done) = mpsc::channel();
        let kept = Arc::clone(&lines);
        std::thread::spawn(move || {
            for line in BufReader::new(stderr).split(b'\n') {
                let Ok(mut line) = line else { break };
                line.truncate(STDERR_LINE_BYTES);
                let line = String::from_utf8_lossy(&line).trim_end().to_string();
                let mut kept = kept.lock().unwrap_or_else(PoisonError::into_inner);
                if kept.len() == STDERR_LINES {
                    kept.pop_front();
                }
                kept.push_back(line);
            }
            let _ = tx.send(());
        });
        Self { lines, done }
    }

    /// What was written, once stderr ended or [`SETTLE_WITHIN`] passed.
    fn text(self) -> String {
        let _ = self.done.recv_timeout(SETTLE_WITHIN);
        let lines = self.lines.lock().unwrap_or_else(PoisonError::into_inner);
        let text: Vec<&str> = lines.iter().map(String::as_str).collect();
        text.join("\n").trim().to_string()
    }
}

/// The starter's exit status, if it exits within [`SETTLE_WITHIN`].
fn settle(child: &mut Child) -> Option<ExitStatus> {
    let until = Instant::now() + SETTLE_WITHIN;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Some(status),
            Ok(None) if Instant::now() < until => std::thread::sleep(Duration::from_millis(50)),
            _ => return None,
        }
    }
}

/// Waits for the starter on a thread of its own, so it never lingers as a
/// zombie; it is never stopped. `--background` exits right after its line.
fn reap(mut child: Child) {
    std::thread::spawn(move || {
        let _ = child.wait();
    });
}

/// A failure in plain words, with the end of the engine's stderr.
fn explain(shown: &str, failure: &Failure, status: Option<ExitStatus>, stderr: &str) -> String {
    let ended = status.map_or(String::new(), |status| format!(" ({status})"));
    let headline = match failure {
        Failure::Spawn(e) => format!("The engine ({shown}) could not be started: {e}."),
        Failure::Read(e) => format!("The engine's output could not be read: {e}."),
        Failure::NoLine => {
            format!("The engine ({shown}) exited{ended} without saying it was ready.")
        }
        Failure::NotReady(line) => {
            let line: String = line.trim().chars().take(300).collect();
            format!("The engine ({shown}) printed something other than its ready line: {line}")
        }
        Failure::Timeout(within) => format!(
            "The engine ({shown}) did not say it was ready within {} seconds, so it was stopped.",
            within.as_secs_f32()
        ),
    };
    if stderr.is_empty() {
        format!("{headline} It wrote nothing to stderr.")
    } else {
        format!("{headline} It wrote:\n{stderr}")
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::Scratch;
    use std::net::TcpListener;
    use std::path::PathBuf;
    use std::thread::JoinHandle;

    /// `sh -c <script>`; `launch` adds `host --background`, which become
    /// the script's `$0` and `$1`.
    fn shell(script: &str) -> Command {
        let mut command = Command::new("sh");
        command.args(["-c", script]);
        command
    }

    fn run(script: &str) -> Result<Value, String> {
        launch(shell(script), "test engine", Duration::from_secs(10))
    }

    #[test]
    fn the_ready_line_comes_back_as_json() {
        let ready = run(r#"echo '{"listening":"127.0.0.1:5","token":"t"}'; echo more"#);
        assert_eq!(ready.unwrap()["listening"], "127.0.0.1:5");
    }

    #[test]
    fn the_arguments_are_host_background() {
        let ready = run(r#"printf '{"args":"%s %s"}\n' "$0" "$1""#);
        assert_eq!(ready.unwrap()["args"], "host --background");
    }

    #[test]
    fn an_exit_without_a_line_names_the_status_and_stderr() {
        let error = run("echo first >&2; echo second >&2; exit 3").unwrap_err();
        assert!(
            error.contains("exited (exit status: 3) without saying it was ready"),
            "{error}"
        );
        assert!(error.ends_with("It wrote:\nfirst\nsecond"), "{error}");
    }

    #[test]
    fn a_line_that_is_not_a_json_object_is_refused() {
        let error = run("echo '[1]'").unwrap_err();
        assert!(error.contains("other than its ready line: [1]"), "{error}");
        assert!(error.ends_with("It wrote nothing to stderr."), "{error}");
    }

    #[test]
    fn only_the_last_lines_of_stderr_are_kept() {
        let error = run("i=0; while [ $i -lt 30 ]; do echo line$i >&2; i=$((i+1)); done; exit 1")
            .unwrap_err();
        let (_, wrote) = error.split_once("It wrote:\n").unwrap();
        let expected: Vec<String> = (10..30).map(|i| format!("line{i}")).collect();
        assert_eq!(wrote.lines().collect::<Vec<_>>(), expected);
    }

    #[test]
    fn a_silent_engine_is_stopped_at_the_deadline() {
        let started = Instant::now();
        let error = launch(
            shell("exec sleep 30"),
            "test engine",
            Duration::from_millis(300),
        )
        .unwrap_err();
        assert!(
            error.contains("did not say it was ready within 0.3 seconds"),
            "{error}"
        );
        assert!(started.elapsed() < Duration::from_secs(10));
    }

    #[test]
    fn a_host_is_idle_only_when_no_session_is_live() {
        let idle = json!({"folders": [
            {"root": "/a", "sessions": [{"id": "1", "live": null}]},
            {"root": "/b", "sessions": []},
        ]});
        assert!(runs_no_session(&idle));
        let busy = json!({"folders": [{"root": "/a", "sessions": [
            {"id": "1", "live": null},
            {"id": "2", "live": {"state": "idle", "since": 1}},
        ]}]});
        assert!(!runs_no_session(&busy));
        assert!(!runs_no_session(&json!({})));
    }

    /// A host port that answers a hello, a `list` with `list`, and a
    /// `shutdown` by removing `host_json`; returns what it was asked.
    fn fake_host(list: Value, host_json: PathBuf) -> (String, JoinHandle<Vec<String>>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap().to_string();
        let heard = std::thread::spawn(move || {
            // Never asked within the deadline is an empty answer, which the
            // test's own assertion reports, never a test that waits forever.
            listener.set_nonblocking(true).unwrap();
            let deadline = Instant::now() + Duration::from_secs(20);
            let stream = loop {
                match listener.accept() {
                    Ok((stream, _)) => break stream,
                    Err(e)
                        if e.kind() == std::io::ErrorKind::WouldBlock
                            && Instant::now() < deadline =>
                    {
                        std::thread::sleep(Duration::from_millis(20));
                    }
                    Err(_) => return Vec::new(),
                }
            };
            stream.set_nonblocking(false).unwrap();
            let mut writer = stream.try_clone().unwrap();
            let mut heard = Vec::new();
            for line in BufReader::new(stream).lines() {
                let Ok(line) = line else { break };
                let command: Value = serde_json::from_str(&line).unwrap();
                let asked = command["do"].as_str().unwrap_or("hello").to_string();
                let answer = match asked.as_str() {
                    "hello" => json!({"welcome": {"protocol": 1, "host": "0.1.0"}}),
                    "list" => json!({"ok": list}),
                    _ => {
                        let _ = std::fs::remove_file(&host_json);
                        json!({"ok": {}})
                    }
                };
                heard.push(asked);
                writeln!(writer, "{answer}").unwrap();
            }
            heard
        });
        (address, heard)
    }

    /// An engine of version 0.2.0 whose host says it is 0.2.0, token `new`.
    fn newer_engine(scratch: &Scratch) -> Engine {
        use std::os::unix::fs::PermissionsExt;
        let path = scratch.0.join("sterna");
        let script = "#!/bin/sh\nif [ \"$1\" = --version ]; then echo 'sterna 0.2.0'; exit 0; fi\n\
            echo '{\"listening\":\"127.0.0.1:1\",\"token\":\"new\",\"version\":\"0.2.0\",\"protocol\":1}'\n";
        std::fs::write(&path, script).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        engine::wait_until_runnable(&path, &["--version"]);
        Engine::new(Kind::Installed, path)
    }

    fn old_ready(address: &str, version: &str) -> Value {
        json!({"listening": address, "token": "old", "version": version, "protocol": 1})
    }

    #[test]
    fn an_older_idle_host_is_replaced_once() {
        let scratch = Scratch::new("renew-idle");
        let host_json = scratch.0.join("host.json");
        std::fs::write(&host_json, "{}").unwrap();
        let idle = json!({"folders": [{"root": "/a", "sessions": [{"id": "1", "live": null}]}]});
        let (address, heard) = fake_host(idle, host_json.clone());
        let mut engine = newer_engine(&scratch);
        let ready = renew(old_ready(&address, "0.1.0"), &mut engine, Some(&host_json)).unwrap();
        assert_eq!(ready["token"], "new");
        assert_eq!(heard.join().unwrap(), ["hello", "list", "shutdown"]);
    }

    #[test]
    fn an_older_host_with_a_live_session_is_kept() {
        let scratch = Scratch::new("renew-busy");
        let host_json = scratch.0.join("host.json");
        let busy = json!({"folders": [{"root": "/a", "sessions": [{"id": "1", "live": {"state": "idle"}}]}]});
        let (address, heard) = fake_host(busy, host_json.clone());
        let mut engine = newer_engine(&scratch);
        let ready = renew(old_ready(&address, "0.1.0"), &mut engine, Some(&host_json)).unwrap();
        assert_eq!(ready["token"], "old");
        assert_eq!(heard.join().unwrap(), ["hello", "list"]);
    }

    #[test]
    fn a_host_as_new_as_the_engine_is_left_alone() {
        let scratch = Scratch::new("renew-same");
        let host_json = scratch.0.join("host.json");
        let mut engine = newer_engine(&scratch);
        for (version, kind) in [
            ("0.2.0", Kind::Installed),
            ("0.3.0", Kind::Installed),
            ("not a version", Kind::Installed),
            ("0.0.1", Kind::Override),
        ] {
            let idle = json!({"folders": []});
            let (address, heard) = fake_host(idle, host_json.clone());
            engine.kind = kind;
            let ready = renew(old_ready(&address, version), &mut engine, Some(&host_json)).unwrap();
            assert_eq!(ready["token"], "old", "{version}");
            // The host was never asked anything: this connection is the first.
            drop(TcpStream::connect(&address).unwrap());
            assert!(heard.join().unwrap().is_empty(), "{version}");
        }
    }

    #[test]
    fn a_missing_program_says_it_could_not_start() {
        let error = launch(
            Command::new("/nonexistent/sterna"),
            "/nonexistent/sterna",
            Duration::from_secs(1),
        )
        .unwrap_err();
        assert!(
            error.starts_with("The engine (/nonexistent/sterna) could not be started"),
            "{error}"
        );
    }
}
