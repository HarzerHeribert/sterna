//! Quitting a client with sessions still at work (`docs/engine.md`, the
//! host's `quit`): plan goal 8. A client that leaves mid-task and asks to
//! keep its session finds it again from the data folder alone, running or
//! finished, with its whole record; one that leaves without keeping it ends
//! the session, and the record of what it did stays on disk.
//!
//! Every check drives the built `sterna host` and the sessions it starts.
//! The provider is the scripted one in `support/engine.rs`, paced so the
//! task is still running when the client leaves; every folder and data
//! folder is a scratch directory of the check's own.

#[path = "support/engine.rs"]
mod engine;

use engine::{Conn, PATIENCE, Provider, World, cell, ending, task_of, turn_of};
use serde_json::{Value, json};
use std::path::Path;
use std::thread;
use std::time::{Duration, Instant};

/// The task each check's session is started with.
const TASK: &str = "take three steps, one at a time, then say how it went";

/// How long the provider takes over each reply: the task takes four of
/// them, so it runs for about eight seconds.
const PACE: Duration = Duration::from_secs(2);

/// Three cells and then an ending. Each cell returns `<word>-cell-<n>` and
/// the ending answers `<word> and finished`; all of them are put together
/// when the cell runs, so these words are in a record only because the cell
/// ran, never because its source was kept.
fn scripted(word: &'static str) -> Provider {
    Provider::paced(move |request| {
        if !task_of(request).contains(TASK) {
            return (Duration::ZERO, ending("not this check's task"));
        }
        let reply = match turn_of(request) {
            step @ 0..=2 => cell(
                &format!("step{}", step + 1),
                &format!("return [{word:?}, \"cell\", {}].join(\"-\");", step + 1),
            ),
            _ => cell(
                "end",
                &format!("answer([{word:?}, \"and\", \"finished\"].join(\" \"));"),
            ),
        };
        (PACE, reply)
    })
}

/// How many model requests this check's task has made so far.
fn asked(provider: &Provider) -> usize {
    provider
        .requests()
        .iter()
        .filter(|seen| task_of(&seen.body).contains(TASK))
        .count()
}

/// Waits for `done`, failing the check with `what` when it never holds.
fn wait_for(what: &str, done: impl Fn() -> bool) {
    let deadline = Instant::now() + PATIENCE;
    while !done() {
        assert!(Instant::now() < deadline, "{what}");
        thread::sleep(Duration::from_millis(100));
    }
}

/// An answer's `ok` value; an `error` answer fails the check.
fn ok(answer: Value, command: &str) -> Value {
    answer
        .get("ok")
        .cloned()
        .unwrap_or_else(|| panic!("the host refused {command}: {answer}"))
}

/// The host starts `TASK` in `root` for the client on `app`; the client
/// attaches to the session and waits until the first cell's result is in
/// the record. Returns the `start` answer and the attached connection.
fn start_and_see_the_first_cell(app: &mut Conn, root: &Path, word: &str) -> (Value, Conn) {
    let started = ok(
        app.request(json!({"do":"start","root":root.to_string_lossy(),"task":TASK})),
        "start",
    );
    let mut session = Conn::open(
        started["listening"].as_str().unwrap(),
        started["token"].as_str().unwrap(),
        "app",
    );
    let first = format!("{word}-cell-1");
    let snapshot = session.attach();
    if !snapshot["notebook"].to_string().contains(&first) {
        session.until(|event| {
            event["kind"] == "transcript" && event["notebook"].to_string().contains(&first)
        });
    }
    (started, session)
}

/// The record a snapshot holds, or what it lacks: every scripted cell's
/// result, in order, in `notebook.cells`, and the answer in the
/// conversation.
fn lacks(state: &Value, word: &str) -> Option<String> {
    // contract: a snapshot's `state` carries the record the way a
    // `transcript` event does, as `conversation` and `notebook`, and the
    // notebook's `cells` hold one entry per cell, in the order they ran.
    let Some(cells) = state["notebook"]["cells"].as_array() else {
        return Some(format!("no notebook.cells in the snapshot: {state}"));
    };
    let mut after = None;
    for step in 1..=3 {
        let result = format!("{word}-cell-{step}");
        let Some(at) = cells
            .iter()
            .position(|cell| cell.to_string().contains(&result))
        else {
            return Some(format!(
                "cell {step}'s result {result:?} is not in {cells:?}"
            ));
        };
        if after.is_some_and(|before| at <= before) {
            return Some(format!("cell {step} is out of order in {cells:?}"));
        }
        after = Some(at);
    }
    let answer = format!("{word} and finished");
    if !state["conversation"].to_string().contains(&answer) {
        return Some(format!(
            "the answer {answer:?} is not in the conversation: {}",
            state["conversation"]
        ));
    }
    None
}

/// Ends every session still running in the world when a check is over,
/// passed or not, through each session's own port. Declared after the host,
/// so it runs before the host is told to stop.
struct EndLive<'a>(&'a World);

impl Drop for EndLive<'_> {
    fn drop(&mut self) {
        for entry in self.0.live() {
            if let (Some(listening), Some(token)) =
                (entry["listening"].as_str(), entry["token"].as_str())
                && let Ok(mut session) = Conn::try_open(listening, token, "check")
            {
                session.send(json!({"do":"end"}));
            }
        }
    }
}

/// A client leaves mid-task and keeps its session. The task goes on with
/// nobody connected; a reopened client, knowing nothing but the data
/// folder's `host.json`, finds the session in the list still running, and
/// attaching shows the whole record: every cell and the answer.
#[test]
#[ignore = "plan goal 8"]
fn a_session_kept_when_its_client_quits_mid_task_runs_on_and_is_found_again_whole() {
    let provider = scripted("kept");
    let world = World::new("keep", &provider);
    let folder = world.folder("project");
    let host = world.host();
    let _end = EndLive(&world);
    let mut app = host.connect("app");
    let (started, session) = start_and_see_the_first_cell(&mut app, &folder, "kept");
    let id = started["id"].as_str().unwrap().to_string();
    assert!(
        asked(&provider) < 4,
        "the task had already reached its ending; the check needs it mid-way"
    );

    ok(app.request(json!({"do":"quit","keep":true})), "quit");
    drop(session);
    drop(app);
    // Nobody is connected now, and the task still asks for every reply.
    wait_for(
        "the kept session stopped working once its client left",
        || asked(&provider) >= 4,
    );

    // The reopened client: the host's address and token from the data
    // folder, as an app started again would find them.
    let path = world.data().join("host.json");
    let found: Value = serde_json::from_str(
        &std::fs::read_to_string(&path)
            .unwrap_or_else(|error| panic!("{}: {error}", path.display())),
    )
    .unwrap_or_else(|error| panic!("{} is not JSON: {error}", path.display()));
    let mut reopened = Conn::open(
        found["listening"].as_str().unwrap(),
        found["token"].as_str().unwrap(),
        "app",
    );
    let list = ok(reopened.request(json!({"do":"list"})), "list");
    let entry = list["folders"]
        .as_array()
        .into_iter()
        .flatten()
        .flat_map(|folder| folder["sessions"].as_array().into_iter().flatten())
        .find(|session| session["id"] == id.as_str())
        .unwrap_or_else(|| panic!("the kept session {id} is not in the list: {list}"))
        .clone();
    assert!(
        matches!(
            entry["live"]["state"].as_str(),
            Some("idle" | "thinking" | "writing" | "running" | "waiting")
        ),
        "the kept session is not running: {entry}"
    );

    let located = ok(reopened.request(json!({"do":"locate","id":id})), "locate");
    // Attached again until the turn under way has ended and the record is
    // whole, or patience runs out with what it still lacks.
    let deadline = Instant::now() + PATIENCE;
    loop {
        let mut session = Conn::open(
            located["listening"].as_str().unwrap(),
            located["token"].as_str().unwrap(),
            "app",
        );
        let state = session.attach();
        let Some(missing) = lacks(&state, "kept") else {
            break;
        };
        assert!(
            Instant::now() < deadline,
            "the kept session's record: {missing}"
        );
        drop(session);
        thread::sleep(Duration::from_millis(250));
    }
}

/// A client leaves mid-task without keeping its session. The session ends:
/// its live entry goes, its port takes no one, and nothing more is asked of
/// the model. What it did stays on disk: its record file holds the task and
/// the first cell's result, and the folder's own listing still offers it.
#[test]
#[ignore = "plan goal 8"]
fn a_session_not_kept_when_its_client_quits_ends_and_its_record_stays_on_disk() {
    let provider = scripted("dropped");
    let world = World::new("drop", &provider);
    let folder = world.folder("project");
    let host = world.host();
    let _end = EndLive(&world);
    let mut app = host.connect("app");
    let (started, session) = start_and_see_the_first_cell(&mut app, &folder, "dropped");
    let id = started["id"].as_str().unwrap().to_string();
    assert!(
        world.live().iter().any(|entry| entry["id"] == id.as_str()),
        "the running session has no live entry: {:?}",
        world.live()
    );

    ok(app.request(json!({"do":"quit","keep":false})), "quit");
    drop(session);
    drop(app);
    wait_for("the session's live entry never went away", || {
        !world.live().iter().any(|entry| entry["id"] == id.as_str())
    });
    wait_for("the ended session still answers on its port", || {
        Conn::try_open(
            started["listening"].as_str().unwrap(),
            started["token"].as_str().unwrap(),
            "app",
        )
        .is_err()
    });
    // Long enough for two more replies, had the session gone on.
    let ended_at = asked(&provider);
    thread::sleep(PACE * 2 + Duration::from_millis(500));
    assert_eq!(
        asked(&provider),
        ended_at,
        "the session went on asking the model after it ended"
    );
    assert!(
        ended_at < 4,
        "the task reached its ending; it was not ended mid-way"
    );

    // contract: a session the host starts keeps its record where a terminal
    // session does, `<root>/.sterna/sessions/<id>.jsonl`, under the id the
    // host gave it -- the id `sterna --sessions` prints and `--resume` takes.
    let rollout = folder
        .join(".sterna")
        .join("sessions")
        .join(format!("{id}.jsonl"));
    let record = std::fs::read_to_string(&rollout)
        .unwrap_or_else(|error| panic!("the record {}: {error}", rollout.display()));
    assert!(record.contains(TASK), "the record lost the task:\n{record}");
    assert!(
        record.contains("dropped-cell-1"),
        "the record lost the first cell's result:\n{record}"
    );
    let output = world
        .sterna()
        .arg("--sessions")
        .current_dir(&folder)
        .output()
        .expect("sterna --sessions runs");
    let listing = String::from_utf8_lossy(&output.stdout);
    assert!(
        listing.split_whitespace().any(|word| word == id),
        "the ended session is not offered in its folder:\n{listing}{}",
        String::from_utf8_lossy(&output.stderr)
    );

    // A host still running after the client left lists the session as one
    // that is not running, and still lists it.
    if let Ok(text) = std::fs::read_to_string(world.data().join("host.json"))
        && let Ok(found) = serde_json::from_str::<Value>(&text)
        && let (Some(listening), Some(token)) =
            (found["listening"].as_str(), found["token"].as_str())
        && let Ok(mut conn) = Conn::try_open(listening, token, "app")
    {
        let list = ok(conn.request(json!({"do":"list"})), "list");
        let entry = list["folders"]
            .as_array()
            .into_iter()
            .flatten()
            .flat_map(|folder| folder["sessions"].as_array().into_iter().flatten())
            .find(|session| session["id"] == id.as_str())
            .unwrap_or_else(|| panic!("the ended session {id} left the list: {list}"));
        assert!(
            entry["live"].is_null(),
            "the ended session is listed as running: {entry}"
        );
    }
}
