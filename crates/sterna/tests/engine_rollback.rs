//! Two sessions may work in one folder (`docs/engine.md`), and neither can
//! undo the other's work. A rollback undoes its own session's newest cell
//! that changed files; when another session has changed one of those files
//! since, the rollback changes nothing and says which file and which session.

#[path = "support/engine.rs"]
mod engine;

use engine::{Conn, PATIENCE, Provider, World, cell, ending, task_of, turn_of};
use serde_json::{Value, json};
use std::time::Instant;

const FIRST: &str = "first session: write the shared file";
const SECOND: &str = "second session: change the shared file";

/// A cell that writes `content` to the shared file and ends the task.
fn writes(content: &str) -> String {
    format!(
        "await write({{path: \"shared.txt\", content: \"{content}\"}});\nanswer(\"the shared file is written\");"
    )
}

/// Reads `conn` until a notice, or a refusal of the rollback, that names both
/// `file` and `session`. Returns its words and every event read.
fn answer_naming(conn: &mut Conn, file: &str, session: &str) -> (String, Vec<Value>) {
    let deadline = Instant::now() + PATIENCE;
    let mut read = Vec::new();
    let mut said = Vec::new();
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            break;
        }
        let Some(event) = conn.recv(left) else { break };
        let words = match event["kind"].as_str() {
            Some("notice") => event["text"].as_str().map(str::to_string),
            Some("refused") if event["to"] == "rollback" => {
                event["reason"].as_str().map(str::to_string)
            }
            _ => None,
        };
        read.push(event);
        if let Some(words) = words {
            if words.contains(file) && names(&words, session) {
                return (words, read);
            }
            said.push(words);
        }
    }
    panic!(
        "the rollback was never answered naming {file} and session {session}; \
         it said {said:?} among {:?}",
        engine::kinds(&read)
    );
}

/// Whether `words` name `id` whole, and not as the start or end of a longer
/// id.
fn names(words: &str, id: &str) -> bool {
    let part = |c: char| c.is_alphanumeric() || c == '-' || c == '_';
    words.match_indices(id).any(|(at, _)| {
        !words[..at].chars().next_back().is_some_and(part)
            && !words[at + id.len()..].chars().next().is_some_and(part)
    })
}

/// The cells a record marks as rolled back: by the notebook's own flag, or by
/// the words a client is given for the cell -- its state, or a line saying
/// its changes were undone. A line that says a rollback was refused is not
/// such a mark.
fn rolled_back(record: &Value) -> Vec<Value> {
    let flagged = record["notebook"]["cells"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|cell| cell["rolled_back"] == true);
    let read = record["reading"]["cells"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|cell| {
            let words = |field: &str| {
                cell[field]
                    .as_str()
                    .unwrap_or("")
                    .to_lowercase()
                    .replace(['_', '-'], " ")
            };
            words("state").contains("rolled back")
                || ["changes were rolled back", "changes are undone"]
                    .iter()
                    .any(|said| words("line").contains(said))
        });
    flagged.chain(read).cloned().collect()
}

/// A writes the shared file, then B, in the same folder, changes it. A's
/// rollback would undo B's change, so it is refused: it names the file and
/// B, B's content stays, and A's cell is not marked as undone.
#[test]
#[ignore = "plan goal 7"]
fn a_rollback_over_another_sessions_change_is_refused_and_names_that_session() {
    let provider = Provider::start(|request| {
        let task = task_of(request);
        match turn_of(request) {
            0 if task.contains(FIRST) => cell("a", &writes("written by A")),
            0 if task.contains(SECOND) => cell("b", &writes("written by B")),
            _ => ending("done"),
        }
    });
    let world = World::new("rollback-shared", &provider);
    let folder = world.folder("project");
    let shared = folder.join("shared.txt");
    let host = world.host();
    let a = host.ask(json!({"do":"start","root":folder}));
    let b = host.ask(json!({"do":"start","root":folder}));
    assert_ne!(a["id"], b["id"], "two sessions: {a} {b}");
    let b_id = b["id"].as_str().expect("a started session has an id");

    let mut on_a = Conn::session(&a, "desktop");
    on_a.attach();
    on_a.send(json!({"do":"submit","text":FIRST}));
    assert_eq!(
        on_a.until_turn_ends().last().unwrap()["activity"],
        "complete"
    );
    assert_eq!(std::fs::read_to_string(&shared).unwrap(), "written by A");

    let mut on_b = Conn::session(&b, "desktop");
    on_b.attach();
    on_b.send(json!({"do":"submit","text":SECOND}));
    assert_eq!(
        on_b.until_turn_ends().last().unwrap()["activity"],
        "complete"
    );
    assert_eq!(std::fs::read_to_string(&shared).unwrap(), "written by B");

    // A's newest cell that changed files wrote the file B has changed since.
    // The rollback is sent from a client that attached just now, so every
    // notice it reads came after it attached.
    let mut undo = Conn::session(&a, "desktop");
    undo.attach();
    undo.send(json!({"do":"rollback"}));
    let (said, read) = answer_naming(&mut undo, "shared.txt", b_id);
    assert_eq!(
        std::fs::read_to_string(&shared).unwrap(),
        "written by B",
        "B's change stays: {said}"
    );

    // No record a client is given marks A's cell as undone.
    for record in read.iter().filter(|event| event["kind"] == "transcript") {
        assert_eq!(rolled_back(record), Vec::<Value>::new(), "{said}");
    }
    let mut later = Conn::session(&a, "later");
    let state = later.attach();
    // contract: the snapshot's state carries the record's `notebook` and
    // `reading` under a transcript's names, each cell's reading in
    // `reading.cells`, as `engine_terminal.rs` reads them.
    assert!(
        state["reading"]["cells"]
            .as_array()
            .is_some_and(|cells| !cells.is_empty()),
        "the snapshot reads A's cell: {state}"
    );
    assert_eq!(rolled_back(&state), Vec::<Value>::new(), "{said}");
}
