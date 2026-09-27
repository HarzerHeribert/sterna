//! Host-channel seam tests against the real V8 callback stack. Most of this
//! file is not interactive-approval acceptance: the shipped TUI does not
//! install the seam, and no decision here adds a permission or an OS sandbox
//! grant. The approval-hint tests at the end of the file are the exception --
//! they drive the shipped `sterna` binary in a real PTY, `--ask-approval` and
//! all, because the hint (F4, `decision-model.md`) is drawn by the live
//! terminal thread and nothing shorter exercises that seam.
use std::io::{BufRead, BufReader, Read as IoRead, Write as IoWrite};
use std::net::TcpListener;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use portable_pty::{CommandBuilder, PtySize, native_pty_system};
use sterna::approval::{Decision, Gate};
use sterna::contract::SessionId;
use sterna::runtime::isolate::{DEFAULT_HEAP_LIMIT_BYTES, Runtime};
use sterna::runtime::outcome::CellOutcome;
use sterna::runtime::preview::Value;
use sterna::sandbox::profile::Profile;
use sterna::tools::invoke::CancellationToken;

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let root = std::env::temp_dir().join(format!(
            "sterna-approval-boundary-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(root.join(".claude")).unwrap();
        Self(std::fs::canonicalize(root).unwrap())
    }

    fn runtime(&self, settings: Option<&str>) -> Runtime {
        Runtime::new(
            &Profile::compile(&self.0, settings),
            &SessionId::new("approval-boundary"),
        )
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn returned(outcome: &CellOutcome, expected: &str) {
    match outcome {
        CellOutcome::Returned { value, .. } => {
            assert_eq!(value, &Value::string(expected));
        }
        other => panic!("expected return {expected:?}, got {other:?}"),
    }
}

#[test]
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn once_resumes_the_suspended_call_without_replaying_an_earlier_effect() {
    let fixture = Fixture::new();
    let (gate, requests) = Gate::channel(sterna::permissions::Ladder::new(
        sterna::permissions::Rung::Manual,
    ));
    let earlier = fixture.0.join("earlier");
    let target = fixture.0.join("target");
    let observed_earlier = earlier.clone();
    let observed_target = target.clone();
    let responder = std::thread::spawn(move || {
        let first = requests.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!(first.action().tool(), "bash");
        assert!(!observed_earlier.exists());
        assert!(first.respond(Decision::AllowOnce));

        let second = requests.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!(second.action().tool(), "write");
        assert_eq!(std::fs::read_to_string(&observed_earlier).unwrap(), "x");
        assert!(!observed_target.exists(), "the pending call already ran");
        assert!(second.respond(Decision::AllowOnce));

        let third = requests.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!(third.action().tool(), "write");
        assert_eq!(std::fs::read_to_string(&observed_target).unwrap(), "one");
        assert!(third.respond(Decision::Deny));
        requests
    });
    let mut runtime = fixture
        .runtime(Some(
            r#"{"permissions":{"allow":["Bash(printf x >> earlier)"]}}"#,
        ))
        .with_approval_gate(gate.clone());
    let outcome = runtime.run_cell(
        r#"bash({command: "printf x >> earlier"});
           write({path: "target", content: "one"});
           try { write({path: "./target", content: "one"}); }
           catch (e) { return e.name; }
           return "unexpected";"#,
    );
    returned(&outcome, "PermissionDenied");
    assert_eq!(runtime.cell(), 1, "the runtime re-entered the cell");
    assert_eq!(outcome.turn().record.calls.len(), 3);
    assert_eq!(std::fs::read_to_string(earlier).unwrap(), "x");
    assert_eq!(std::fs::read_to_string(target).unwrap(), "one");
    assert!(gate.session_actions().is_empty());
    assert!(responder.join().unwrap().try_recv().is_err());
}

#[test]
fn session_decisions_match_all_canonical_arguments_and_summaries_hide_values() {
    let fixture = Fixture::new();
    let (gate, requests) = Gate::channel(sterna::permissions::Ladder::new(
        sterna::permissions::Rung::Manual,
    ));
    let root = fixture.0.clone();
    let responder = std::thread::spawn(move || {
        let first = requests.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!(
            first.action().arguments()["path"],
            root.join("target").to_string_lossy()
        );
        assert!(!format!("{:?}", first.action()).contains("secret-content"));
        assert!(first.respond(Decision::AllowForSession));

        let changed = requests.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!(changed.action().arguments()["content"], "different-content");
        assert!(changed.respond(Decision::Deny));
        requests
    });
    let mut runtime = fixture.runtime(None).with_approval_gate(gate.clone());
    let outcome = runtime.run_cell(
        r#"write({path: "target", content: "secret-content"});
           write({path: "./target", content: "secret-content"});
           try { write({path: "target", content: "different-content"}); }
           catch (e) { console.log(e.name); }
           write({path: "./target", content: "secret-content"});
           return "finished";"#,
    );
    returned(&outcome, "finished");
    assert_eq!(outcome.turn().record.calls.len(), 4);
    assert_eq!(
        std::fs::read_to_string(fixture.0.join("target")).unwrap(),
        "secret-content"
    );
    let summaries = gate.session_actions();
    assert_eq!(summaries.len(), 1);
    let fingerprint = summaries[0].strip_prefix("write · exact action ").unwrap();
    assert_eq!(fingerprint.len(), 12);
    assert!(fingerprint.bytes().all(|byte| byte.is_ascii_hexdigit()));
    assert!(!summaries[0].contains("secret-content"));
    assert!(!summaries[0].contains(fixture.0.to_string_lossy().as_ref()));
    assert!(responder.join().unwrap().try_recv().is_err());
}

#[test]
fn explicit_denies_never_grantable_actions_and_missing_grants_never_reach_the_gate() {
    let fixture = Fixture::new();
    let (gate, requests) = Gate::channel(sterna::permissions::Ladder::new(
        sterna::permissions::Rung::Manual,
    ));
    let mut runtime = fixture
        .runtime(Some(
            r#"{"permissions":{"allow":["Bash"],"deny":["Write(blocked)","Bash(echo *)"]}}"#,
        ))
        .with_approval_gate(gate);
    let outcome = runtime.run_cell(
        r#"const refusals = [];
           try { write({path: "blocked", content: "no"}); } catch (e) { refusals.push(e.name); }
           try { write({path: ".claude/settings.json", content: "no"}); } catch (e) { refusals.push(e.name); }
           try { read({path: "~/.ssh/id_ed25519"}); } catch (e) { refusals.push(e.name); }
           try { bash({command: "echo forbidden"}); } catch (e) { refusals.push(e.name); }
           try { bash({command: "bwrap true"}); } catch (e) { refusals.push(e.name); }
           try { read({path: "../missing-grant"}); } catch (e) { refusals.push(e.name); }
           return refusals.join(",");"#,
    );
    returned(&outcome, &["PermissionDenied"; 6].join(","));
    assert!(
        requests.try_recv().is_err(),
        "a refused call reached the host"
    );
    assert!(!fixture.0.join("blocked").exists());
    assert!(!fixture.0.join(".claude/settings.json").exists());
}

/// **The Auto rung runs what it promises to run.** With no `Bash(...)`
/// pattern at all, a command line that only reads runs without asking; a
/// line that writes is put to the person; neither is refused for being
/// absent from a list nobody wrote.
#[test]
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn auto_rung_runs_a_read_only_command_with_no_allow_patterns() {
    let fixture = Fixture::new();
    let (gate, requests) = Gate::channel(sterna::permissions::Ladder::new(
        sterna::permissions::Rung::Auto,
    ));
    let responder = std::thread::spawn(move || {
        let asked = requests.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!(asked.action().arguments()["command"], "touch made");
        assert!(asked.respond(Decision::AllowOnce));
        requests
    });
    let mut runtime = fixture
        .runtime(None)
        .with_approval_gate(gate.with_read_only(Vec::new()));
    let outcome = runtime.run_cell(
        r#"const said = [];
           try { bash({command: "ls -la && git log --oneline -3"}); said.push("ran"); }
           catch (e) { said.push(e.name); }
           try { bash({command: "touch made"}); said.push("ran"); }
           catch (e) { said.push(e.name); }
           return said.join(",");"#,
    );
    match &outcome {
        CellOutcome::Returned { value, .. } => assert!(
            !format!("{value:?}").contains("PermissionDenied"),
            "an unlisted command was refused on Auto: {value:?}"
        ),
        other => panic!("expected a return, got {other:?}"),
    }
    assert!(
        fixture.0.join("made").exists(),
        "the confirmed line did not run"
    );
    assert!(
        responder.join().unwrap().try_recv().is_err(),
        "the read-only line asked too"
    );
}

/// Nobody to ask keeps the list: an unlisted line is refused, exactly as it
/// was before the Auto rung learned to judge one.
#[test]
fn an_unattended_session_still_refuses_an_unlisted_command() {
    let fixture = Fixture::new();
    let (gate, requests) = Gate::channel(
        sterna::permissions::Ladder::new(sterna::permissions::Rung::Auto).unattended(),
    );
    let mut runtime = fixture.runtime(None).with_approval_gate(gate);
    let outcome = runtime.run_cell(
        r#"try { bash({command: "touch made"}); }
           catch (e) { return e.name; }
           return "unexpected";"#,
    );
    returned(&outcome, "PermissionDenied");
    assert!(requests.try_recv().is_err());
    assert!(!fixture.0.join("made").exists());
}

#[test]
fn an_unattached_runtime_stays_fail_closed_for_missing_grants() {
    let fixture = Fixture::new();
    let mut runtime = fixture.runtime(None);
    let outcome = runtime.run_cell(
        r#"try { bash({command: "echo no"}); }
           catch (e) { return e.name; }
           return "unexpected";"#,
    );
    returned(&outcome, "PermissionDenied");
}

#[test]
fn a_disconnected_host_and_a_dropped_request_deny_without_an_effect() {
    for drop_request in [false, true] {
        let fixture = Fixture::new();
        let (gate, requests) = Gate::channel(sterna::permissions::Ladder::new(
            sterna::permissions::Rung::Manual,
        ));
        let responder = std::thread::spawn(move || {
            if drop_request {
                drop(requests.recv_timeout(Duration::from_secs(5)).unwrap());
            }
        });
        let mut runtime = fixture.runtime(None).with_approval_gate(gate);
        let outcome = runtime.run_cell(
            r#"try { write({path: "target", content: "no"}); }
               catch (e) { return e.name; }
               return "unexpected";"#,
        );
        returned(&outcome, "PermissionDenied");
        responder.join().unwrap();
        assert!(!fixture.0.join("target").exists());
    }
}

/// A call cancelled while it waits is reported as cancelled -- the way a
/// running call is -- never as a refusal the model should work around, and
/// an answer arriving after it changes nothing.
#[test]
fn cancellation_cancels_the_pending_call_and_rejects_a_late_session_answer() {
    let fixture = Fixture::new();
    let (gate, requests) = Gate::channel(sterna::permissions::Ladder::new(
        sterna::permissions::Rung::Manual,
    ));
    let token = CancellationToken::new();
    let cancelled = token.clone();
    let (finished, completion) = mpsc::channel();
    let responder = std::thread::spawn(move || {
        let request = requests.recv_timeout(Duration::from_secs(5)).unwrap();
        assert!(request.is_pending());
        cancelled.cancel();
        completion.recv_timeout(Duration::from_secs(5)).unwrap();
        assert!(!request.is_pending());
        assert!(!request.respond(Decision::AllowForSession));
    });
    let mut runtime = fixture
        .runtime(None)
        .with_token(token)
        .with_approval_gate(gate.clone());
    let outcome = runtime.run_cell(
        r#"try { write({path: "target", content: "no"}); }
           catch (e) { return e.name; }
           return "unexpected";"#,
    );
    returned(&outcome, "Cancelled");
    finished.send(()).unwrap();
    responder.join().unwrap();
    assert!(gate.session_actions().is_empty());
    assert!(!fixture.0.join("target").exists());
}

#[test]
fn human_approval_wait_pauses_the_cell_clock_and_then_resumes_the_write() {
    let fixture = Fixture::new();
    let (gate, requests) = Gate::channel(sterna::permissions::Ladder::new(
        sterna::permissions::Rung::Manual,
    ));
    let responder = std::thread::spawn(move || {
        let request = requests.recv_timeout(Duration::from_secs(5)).unwrap();
        std::thread::sleep(Duration::from_millis(350));
        assert!(request.respond(Decision::AllowOnce));
    });
    let mut runtime = Runtime::with_limits(
        &Profile::compile(&fixture.0, None),
        &SessionId::new("approval-timeout"),
        DEFAULT_HEAP_LIMIT_BYTES,
        Duration::from_millis(100),
    )
    .with_approval_gate(gate);
    let started = Instant::now();
    let outcome =
        runtime.run_cell(r#"write({path: "target", content: "yes"}); return "approved";"#);
    responder.join().unwrap();
    assert!(started.elapsed() < Duration::from_secs(2));
    assert!(started.elapsed() >= Duration::from_millis(350));
    returned(&outcome, "approved");
    assert!(!runtime.poisoned());
    assert_eq!(
        std::fs::read_to_string(fixture.0.join("target")).unwrap(),
        "yes"
    );
}

#[test]
fn approval_resumes_remaining_compute_budget_instead_of_resetting_it() {
    let fixture = Fixture::new();
    let (gate, requests) = Gate::channel(sterna::permissions::Ladder::new(
        sterna::permissions::Rung::Manual,
    ));
    let responder = std::thread::spawn(move || {
        let request = requests.recv_timeout(Duration::from_secs(5)).unwrap();
        std::thread::sleep(Duration::from_millis(500));
        assert!(request.respond(Decision::AllowOnce));
    });
    let mut runtime = Runtime::with_limits(
        &Profile::compile(&fixture.0, None),
        &SessionId::new("approval-budget"),
        DEFAULT_HEAP_LIMIT_BYTES,
        Duration::from_millis(400),
    )
    .with_approval_gate(gate);
    let outcome = runtime.run_cell(
        r#"
        const before = Date.now(); while (Date.now() - before < 250) {}
        write({path: "target", content: "approved"});
        const after = Date.now(); while (Date.now() - after < 250) {}
        return "incorrectly reset the budget";
    "#,
    );
    responder.join().unwrap();
    assert!(
        format!("{outcome:?}").contains("RuntimeTimeout"),
        "{outcome:?}"
    );
    assert_eq!(
        std::fs::read_to_string(fixture.0.join("target")).unwrap(),
        "approved"
    );
    assert!(!runtime.poisoned());
}

#[test]
fn an_allow_once_is_consumed_even_when_the_execution_fails() {
    let fixture = Fixture::new();
    let (gate, requests) = Gate::channel(sterna::permissions::Ladder::new(
        sterna::permissions::Rung::Manual,
    ));
    let responder = std::thread::spawn(move || {
        let first = requests.recv_timeout(Duration::from_secs(5)).unwrap();
        let action = first.action().clone();
        assert!(first.respond(Decision::AllowOnce));
        let second = requests.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!(second.action(), &action);
        assert!(second.respond(Decision::Deny));
    });
    let mut runtime = fixture.runtime(None).with_approval_gate(gate);
    let outcome = runtime.run_cell(
        r#"try { write({path: ".", content: "cannot replace a directory"}); } catch (e) { console.log(e.name); }
           try { write({path: ".", content: "cannot replace a directory"}); }
           catch (e) { return e.name; }
           return "unexpected";"#,
    );
    returned(&outcome, "PermissionDenied");
    responder.join().unwrap();
}

#[test]
fn subagents_do_not_prompt_even_when_a_host_attaches_a_gate() {
    let fixture = Fixture::new();
    let (gate, requests) = Gate::channel(sterna::permissions::Ladder::new(
        sterna::permissions::Rung::Manual,
    ));
    let mut runtime = fixture.runtime(None).with_approval_gate(gate).as_subagent();
    let outcome = runtime.run_cell(
        r#"write({path: "target", content: "base profile grants this"});
           try { bash({command: "echo no grant"}); }
           catch (e) { return e.name; }
           return "unexpected";"#,
    );
    returned(&outcome, "PermissionDenied");
    assert!(requests.try_recv().is_err());
    assert!(fixture.0.join("target").exists());
}

#[test]
#[cfg(unix)]
fn a_symlink_retargeted_during_the_wait_invalidates_the_answer() {
    let fixture = Fixture::new();
    std::fs::write(fixture.0.join("original"), "original").unwrap();
    std::fs::write(fixture.0.join("other"), "other").unwrap();
    std::os::unix::fs::symlink("original", fixture.0.join("link")).unwrap();
    let (gate, requests) = Gate::channel(sterna::permissions::Ladder::new(
        sterna::permissions::Rung::Manual,
    ));
    let root = fixture.0.clone();
    let responder = std::thread::spawn(move || {
        let request = requests.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!(
            request.action().arguments()["path"],
            root.join("original").to_string_lossy()
        );
        std::fs::remove_file(root.join("link")).unwrap();
        std::os::unix::fs::symlink("other", root.join("link")).unwrap();
        assert!(request.respond(Decision::AllowOnce));
    });
    let mut runtime = fixture.runtime(None).with_approval_gate(gate);
    let outcome = runtime.run_cell(
        r#"try { write({path: "link", content: "no"}); }
           catch (e) { return e.name; }
           return "unexpected";"#,
    );
    returned(&outcome, "PermissionDenied");
    responder.join().unwrap();
    assert_eq!(
        std::fs::read_to_string(fixture.0.join("original")).unwrap(),
        "original"
    );
    assert_eq!(
        std::fs::read_to_string(fixture.0.join("other")).unwrap(),
        "other"
    );
}

#[test]
#[cfg(target_os = "linux")]
fn a_non_utf8_canonical_path_is_refused_before_a_lossy_key_can_be_approved() {
    use std::os::unix::ffi::OsStringExt;
    let fixture = Fixture::new();
    let target = fixture
        .0
        .join(std::ffi::OsString::from_vec(b"private-\xff".to_vec()));
    std::fs::write(&target, "untouched").unwrap();
    std::os::unix::fs::symlink(&target, fixture.0.join("link")).unwrap();
    let (gate, requests) = Gate::channel(sterna::permissions::Ladder::new(
        sterna::permissions::Rung::Manual,
    ));
    let mut runtime = fixture.runtime(None).with_approval_gate(gate);
    let outcome = runtime.run_cell(
        r#"try { write({path: "link", content: "no"}); }
           catch (e) { return e.name; }
           return "unexpected";"#,
    );
    returned(&outcome, "PermissionDenied");
    assert!(requests.try_recv().is_err());
    assert_eq!(std::fs::read_to_string(target).unwrap(), "untouched");
}

#[test]
fn remembered_actions_do_not_cross_roots_or_override_a_later_profiles_deny() {
    let first = Fixture::new();
    let second = Fixture::new();
    let (gate, requests) = Gate::channel(sterna::permissions::Ladder::new(
        sterna::permissions::Rung::Manual,
    ));
    let first_root = first.0.clone();
    let second_root = second.0.clone();
    let responder = std::thread::spawn(move || {
        let request = requests.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!(request.action().root(), first_root.to_string_lossy());
        assert!(request.respond(Decision::AllowForSession));
        let request = requests.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!(request.action().root(), second_root.to_string_lossy());
        assert!(request.respond(Decision::Deny));
        requests
    });
    let cell = r#"try { write({path: "target", content: "exact"}); }
                  catch (e) { return e.name; }
                  return "written";"#;
    let mut runtime = first.runtime(None).with_approval_gate(gate.clone());
    returned(&runtime.run_cell(cell), "written");
    let mut runtime = second.runtime(None).with_approval_gate(gate.clone());
    returned(&runtime.run_cell(cell), "PermissionDenied");
    let mut runtime = first
        .runtime(Some(r#"{"permissions":{"deny":["Write(target)"]}}"#))
        .with_approval_gate(gate);
    returned(&runtime.run_cell(cell), "PermissionDenied");
    assert!(!second.0.join("target").exists());
    assert!(responder.join().unwrap().try_recv().is_err());
}

#[test]
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn answering_the_gate_does_not_expand_the_os_sandbox() {
    let fixture = Fixture::new();
    let outside = Fixture::new();
    let private = outside.0.join("private");
    std::fs::write(&private, "private contents\n").unwrap();
    let command = format!(
        "if IFS= read -r contents < '{}'; then printf leaked; else printf confined; fi",
        private.display()
    );
    // The same command can read the file without confinement, so the negative
    // half below is an OS restriction, not a missing fixture or a bad command.
    let unconfined = std::process::Command::new("bash")
        .args(["-c", &command])
        .output()
        .unwrap();
    assert_eq!(String::from_utf8(unconfined.stdout).unwrap(), "leaked");
    let (gate, requests) = Gate::channel(sterna::permissions::Ladder::new(
        sterna::permissions::Rung::Manual,
    ));
    let responder = std::thread::spawn(move || {
        let request = requests.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!(request.action().tool(), "bash");
        assert!(request.respond(Decision::AllowOnce));
    });
    let mut runtime = fixture
        .runtime(Some(r#"{"permissions":{"allow":["Bash"]}}"#))
        .with_approval_gate(gate);
    let outcome = runtime.run_cell(&format!(
        "return bash({{command: {}}}).stdout;",
        serde_json::to_string(&command).unwrap()
    ));
    returned(&outcome, "confined");
    responder.join().unwrap();
}

// -- the approval hint (F4, decision-model.md) ---------------------------
//
// These four tests drive the shipped `sterna` binary in a real PTY with
// `--ask-approval`: the hint is drawn by the live terminal thread, and
// nothing shorter than the real TUI exercises `session::ui::run`'s render
// loop. A fake provider answers `/v1/messages` with one scripted `write`
// call and `/v1/systemone` with the scripted `fits` answers below, exactly
// as `tests/decisions.rs`'s `providers()` dispatches by path.

/// What the fake decision endpoint does with the one `fits` question a
/// pending approval asks.
enum HintReply {
    Answer(f64),
    Sleep(Duration),
}

/// The single key of a decision request's `questions` object, exactly as
/// `tests/decisions.rs::question_key` reads it.
fn question_key(body_text: &str) -> String {
    let value: serde_json::Value = serde_json::from_str(body_text).unwrap();
    value["questions"]
        .as_object()
        .and_then(|questions| questions.keys().next())
        .cloned()
        .unwrap_or_default()
}

/// A fake provider serving both `/v1/messages` (one scripted `write` cell,
/// streamed exactly as the live TUI requests it) and `/v1/systemone`. A
/// `[decisions]` model configured for these tests also asks the *existing*
/// `intent` question once before the first turn and would ask `satisfied`
/// once the task claims completion (`decision-model.md`) -- unrelated to the
/// approval hint under test, so both get a harmless, never-hold, never-fail
/// default answer here; only a request keyed `fits` draws from `hints`, in
/// order, and only those bodies are recorded. A `fits` request past the end
/// of `hints` gets a 500, which no test here needs but which keeps a bug
/// from hanging on an empty queue. Returns the base URL and the raw `fits`
/// request bodies, in arrival order.
fn hint_provider(hints: Vec<HintReply>) -> (String, Arc<Mutex<Vec<String>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let seen = Arc::new(Mutex::new(Vec::new()));
    let recorded = Arc::clone(&seen);
    thread::spawn(move || {
        let mut hints: std::collections::VecDeque<HintReply> = hints.into_iter().collect();
        loop {
            let Ok((mut stream, _)) = listener.accept() else {
                return;
            };
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
            let body_text = String::from_utf8_lossy(&body).into_owned();
            if path == "/v1/systemone" {
                let key = question_key(&body_text);
                if key == "fits" {
                    recorded.lock().unwrap().push(body_text);
                    match hints.pop_front() {
                        Some(HintReply::Answer(fits)) => {
                            let response = serde_json::json!({
                                "model": "fake-decider",
                                "answers": {"fits": {"type": "noul", "noul": fits}},
                                "usage": {"input_tokens": 10, "output_tokens": 4},
                            })
                            .to_string();
                            let _ = write!(
                                stream,
                                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{response}",
                                response.len()
                            );
                        }
                        Some(HintReply::Sleep(duration)) => {
                            thread::sleep(duration);
                            let response = "{}";
                            let _ = write!(
                                stream,
                                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{response}",
                                response.len()
                            );
                        }
                        None => {
                            let response = "{}";
                            let _ = write!(
                                stream,
                                "HTTP/1.1 500 Error\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{response}",
                                response.len()
                            );
                        }
                    }
                } else {
                    // The task's own `intent` (before the first turn) or
                    // `satisfied` (at the completion gate) question -- a
                    // harmless answer that never holds and never fails, so
                    // it cannot interact with the `fits` hint under test.
                    let response = match key.as_str() {
                        "intent" => serde_json::json!({
                            "model": "fake-decider",
                            "answers": {"intent": {
                                "type": "choice", "choice": "modify",
                                "probabilities": {"read_only": 0.0, "modify": 0.99, "run": 0.0, "other": 0.01},
                                "confidence": 0.99,
                            }},
                            "usage": {"input_tokens": 10, "output_tokens": 4},
                        }),
                        _ => serde_json::json!({
                            "model": "fake-decider",
                            "answers": {"satisfied": {"type": "noul", "noul": 0.5}},
                            "usage": {"input_tokens": 10, "output_tokens": 4},
                        }),
                    }
                    .to_string();
                    let _ = write!(
                        stream,
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{response}",
                        response.len()
                    );
                }
            } else {
                let request: serde_json::Value =
                    serde_json::from_str(&body_text).unwrap_or_default();
                let text =
                    "```sterna\nwrite({path: \"a.txt\", content: \"1\"});\nanswer(\"done\");\n```";
                let (mime, out) = if request["stream"] == true {
                    let events = [
                        serde_json::json!({"type":"message_start","message":{"role":"assistant","usage":{"input_tokens":10}}}),
                        serde_json::json!({"type":"content_block_delta","delta":{"type":"text_delta","text":text}}),
                        serde_json::json!({"type":"message_delta","usage":{"output_tokens":20}}),
                        serde_json::json!({"type":"message_stop"}),
                    ];
                    (
                        "text/event-stream",
                        events
                            .iter()
                            .map(|event| format!("data: {event}\n\n"))
                            .collect::<String>(),
                    )
                } else {
                    (
                        "application/json",
                        serde_json::json!({"role":"assistant","content":[{"type":"text","text":text}],"usage":{"input_tokens":10,"output_tokens":20}}).to_string(),
                    )
                };
                let _ = write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Type: {mime}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{out}",
                    out.len()
                );
            }
        }
    });
    (base, seen)
}

/// A live `sterna session --ask-approval` in a real PTY -- trimmed to what the
/// four tests below need: no resize, no mouse reports (`tests/tui_live.rs`'s
/// `App` covers those for the rest of the interactive surface).
struct LiveApp {
    _master: Box<dyn portable_pty::MasterPty + Send>,
    child: Box<dyn portable_pty::Child + Send + Sync>,
    input: Box<dyn IoWrite + Send>,
    output: mpsc::Receiver<Vec<u8>>,
    screen: vt100::Parser,
    root: PathBuf,
}
impl LiveApp {
    /// `config_toml` becomes `.sterna/config.toml`, exactly as
    /// `tests/decisions.rs::write_config` writes it -- empty means no
    /// `[decisions]` section at all, i.e. decisions off.
    fn start(base: &str, config_toml: &str) -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let root = std::env::temp_dir().join(format!(
            "sterna-approval-hint-live-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::SeqCst)
        ));
        std::fs::create_dir_all(root.join(".sterna")).unwrap();
        std::fs::write(root.join(".sterna/config.toml"), config_toml).unwrap();
        let pair = native_pty_system()
            .openpty(PtySize {
                rows: 30,
                cols: 80,
                pixel_width: 0,
                pixel_height: 0,
            })
            .unwrap();
        let mut command = CommandBuilder::new(env!("CARGO_BIN_EXE_sterna"));
        command.args(["session", "--root"]);
        command.arg(&root);
        command.args(["--model", "fixture-model"]);
        command.arg("--gateway");
        command.arg(root.join("no-gateway"));
        command.env("INFERENCE_GATEWAY_BIN", root.join("no-gateway"));
        command.arg("--ask-approval");
        command.env("ANTHROPIC_BASE_URL", base);
        command.env("XDG_CONFIG_HOME", root.join("global-config"));
        command.env_remove("ANTHROPIC_API_KEY");
        command.env_remove("ANTHROPIC_AUTH_TOKEN");
        command.env("TERM", "xterm-256color");
        let child = pair.slave.spawn_command(command).unwrap();
        drop(pair.slave);
        let mut reader = pair.master.try_clone_reader().unwrap();
        let input = pair.master.take_writer().unwrap();
        let (sender, output) = mpsc::channel();
        thread::spawn(move || {
            let mut buf = [0; 8192];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        if sender.send(buf[..n].to_vec()).is_err() {
                            break;
                        }
                    }
                }
            }
        });
        Self {
            _master: pair.master,
            child,
            input,
            output,
            screen: vt100::Parser::new(30, 80, 1000),
            root,
        }
    }
    fn send(&mut self, bytes: &[u8]) {
        self.input.write_all(bytes).unwrap();
        self.input.flush().unwrap();
    }
    /// Reply to `ESC[6n` the way a real terminal does -- `tests/tui_live.rs`
    /// §1's finding: crossterm blocks on this reply on Windows.
    fn answer_cursor_query(&mut self, bytes: &[u8]) {
        if bytes.windows(4).any(|window| window == b"\x1b[6n") {
            let _ = self.input.write_all(b"\x1b[1;1R");
            let _ = self.input.flush();
        }
    }
    fn pump_until(
        &mut self,
        deadline: Instant,
        predicate: impl Fn(&vt100::Screen) -> bool,
    ) -> bool {
        loop {
            if predicate(self.screen.screen()) {
                return true;
            }
            if Instant::now() >= deadline {
                return false;
            }
            if let Ok(bytes) = self.output.recv_timeout(Duration::from_millis(25)) {
                self.answer_cursor_query(&bytes);
                self.screen.process(&bytes);
            }
        }
    }
    fn contains(&mut self, needle: &str) {
        let deadline = Instant::now() + Duration::from_secs(10);
        assert!(
            self.pump_until(deadline, |screen| screen.contents().contains(needle)),
            "{needle:?} did not appear:\n{}",
            self.screen.screen().contents()
        );
    }
    /// Keeps pumping for `millis` regardless of content -- an absence
    /// assertion needs this, since `contains` stops at the first match and a
    /// screen that was never brought up to date could satisfy it by accident.
    fn settle(&mut self, millis: u64) {
        let deadline = Instant::now() + Duration::from_millis(millis);
        self.pump_until(deadline, |_| false);
    }
    fn screen_text(&self) -> String {
        self.screen.screen().contents()
    }
}
impl Drop for LiveApp {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

#[test]
fn a_hint_that_answers_in_time_is_shown_beside_the_confirmation() {
    let (base, decisions) = hint_provider(vec![HintReply::Answer(0.91)]);
    let mut app = LiveApp::start(
        &base,
        "[decisions]\nmodel = \"fake-decider\"\nmode = \"on\"\n",
    );
    app.contains("fixture-model");
    app.send(b"write a.txt for me\r");
    app.contains("APPROVE");
    // The hint is words, not a score.
    app.contains("looks like part of what you asked");
    assert_eq!(decisions.lock().unwrap().len(), 1);
    // A key counts once the prompt has been up, quietly, for half a second.
    app.settle(600);
    app.send(b"o");
    app.contains("done");
}

#[test]
fn a_decision_delayed_past_the_timeout_never_delays_or_marks_the_confirmation() {
    let (base, _decisions) = hint_provider(vec![HintReply::Sleep(Duration::from_secs(3))]);
    let mut app = LiveApp::start(
        &base,
        "[decisions]\nmodel = \"fake-decider\"\nmode = \"on\"\n",
    );
    app.contains("fixture-model");
    let sent = Instant::now();
    app.send(b"write a.txt for me\r");
    app.contains("APPROVE");
    assert!(
        sent.elapsed() < Duration::from_secs(2),
        "the confirmation waited on the decision model"
    );
    // decide.rs's own 2 s bound fires well before the fake's 3 s sleep ends;
    // by 2.5 s the request has failed and no hint was ever stored.
    app.settle(2_500);
    assert!(
        !app.screen_text().contains("Jev:"),
        "a failed or slow decision must never show a line:\n{}",
        app.screen_text()
    );
    app.send(b"o");
    app.contains("done");
}

#[test]
fn shadow_mode_records_the_hint_and_never_shows_the_line() {
    let (base, decisions) = hint_provider(vec![HintReply::Answer(0.91)]);
    let mut app = LiveApp::start(
        &base,
        "[decisions]\nmodel = \"fake-decider\"\nmode = \"shadow\"\n",
    );
    app.contains("fixture-model");
    app.send(b"write a.txt for me\r");
    app.contains("APPROVE");
    // No text to wait on distinguishes "recorded but not shown" from "not
    // asked yet", so this settles a fixed interval and checks both sides.
    app.settle(1_000);
    assert!(
        !app.screen_text().contains("Jev:"),
        "shadow must never show the line:\n{}",
        app.screen_text()
    );
    assert_eq!(
        decisions.lock().unwrap().len(),
        1,
        "shadow still asks and records the hint"
    );
    app.send(b"o");
    app.contains("done");
}

#[test]
fn no_model_means_no_approval_hint_request() {
    let (base, decisions) = hint_provider(vec![]);
    let mut app = LiveApp::start(&base, "");
    app.contains("fixture-model");
    app.send(b"write a.txt for me\r");
    app.contains("APPROVE");
    app.settle(1_000);
    assert!(!app.screen_text().contains("Jev:"));
    assert_eq!(
        decisions.lock().unwrap().len(),
        0,
        "no model means no request ever reaches /v1/systemone"
    );
    app.send(b"o");
    app.contains("done");
}

/// The ladder decides which calls reach a person at all, and a `full`
/// session installs no gate — so these run against the gate itself.
#[cfg(any(target_os = "macos", target_os = "linux"))]
mod ladder {
    use super::*;
    use sterna::permissions::{Ladder, Rung};

    /// `auto` runs ordinary work and asks about the rest — in one cell, so
    /// the two answers are the same session's.
    #[test]
    fn auto_runs_what_reads_and_asks_about_what_it_cannot_place() {
        let fixture = Fixture::new();
        let (gate, requests) = Gate::channel(Ladder::new(Rung::Auto));
        let responder = std::thread::spawn(move || {
            let asked = requests.recv_timeout(Duration::from_secs(5)).unwrap();
            let shown = asked.action().confirmation().text;
            assert!(shown.contains("deploy"), "the person sees the real call");
            assert!(asked.respond(Decision::AllowOnce));
            requests
        });
        let mut runtime = fixture
            .runtime(Some(r#"{"permissions":{"allow":["Bash"]}}"#))
            .with_approval_gate(gate.clone());
        let outcome = runtime.run_cell(
            r#"bash({command: "git status --short"});
               bash({command: "sh deploy"});
               return "both";"#,
        );
        returned(&outcome, "both");
        let left = responder.join().unwrap();
        assert!(
            left.try_recv().is_err(),
            "exactly one of the two calls reached the person"
        );
    }

    /// The rule the user named: a gate that answers differently on a retry
    /// teaches retrying. A denial is the session's answer for that exact
    /// call, and the second attempt is refused without asking again.
    #[test]
    fn a_refusal_is_the_sessions_answer_and_is_not_asked_a_second_time() {
        let fixture = Fixture::new();
        let (gate, requests) = Gate::channel(Ladder::new(Rung::Manual));
        let responder = std::thread::spawn(move || {
            let asked = requests.recv_timeout(Duration::from_secs(5)).unwrap();
            assert!(asked.respond(Decision::Deny));
            requests
        });
        let mut runtime = fixture
            .runtime(Some(r#"{"permissions":{"allow":["Bash"]}}"#))
            .with_approval_gate(gate.clone());
        let outcome = runtime.run_cell(
            r#"let denied = 0;
               for (const _ of [1, 2]) {
                 try { bash({command: "sh deploy"}); } catch (e) { denied += 1; }
               }
               return String(denied);"#,
        );
        returned(&outcome, "2");
        let left = responder.join().unwrap();
        assert!(
            left.try_recv().is_err(),
            "the second attempt was answered from the first, not asked again"
        );
    }

    /// Moving the rung while a cell is suspended in the gate binds the very
    /// next call of that same cell — which is why the rung is an atomic the
    /// gate reads per call, not an input the turn loop reads between turns.
    ///
    /// Deterministic by construction: the move happens on the responder
    /// thread, while the cell is stopped inside the first confirmation.
    #[test]
    fn a_rung_moved_while_a_cell_waits_binds_that_cells_next_call() {
        let fixture = Fixture::new();
        let ladder = Ladder::new(Rung::AcceptEdits);
        let (gate, requests) = Gate::channel(ladder.clone());
        let moved = ladder.clone();
        let responder = std::thread::spawn(move || {
            let asked = requests.recv_timeout(Duration::from_secs(5)).unwrap();
            // The person reaches for Shift-Tab while the confirmation is up.
            moved.set(Rung::Full);
            assert!(asked.respond(Decision::AllowOnce));
            requests
        });
        let mut runtime = fixture
            .runtime(Some(r#"{"permissions":{"allow":["Bash"]}}"#))
            .with_approval_gate(gate.clone());
        let outcome = runtime.run_cell(
            r#"bash({command: "sh one"});
               bash({command: "sh two"});
               return "both";"#,
        );
        returned(&outcome, "both");
        assert_eq!(ladder.rung(), Rung::Full);
        let left = responder.join().unwrap();
        assert!(
            left.try_recv().is_err(),
            "the second call ran under the new rung without asking"
        );
        let moves = ladder.drain_moves();
        assert_eq!(moves.len(), 1, "the move is recorded once");
        assert_eq!(
            (moves[0].from, moves[0].to),
            (Rung::AcceptEdits, Rung::Full)
        );
    }
}
