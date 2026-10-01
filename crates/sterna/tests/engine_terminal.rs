//! The terminal is the engine's first client (`docs/engine.md`): what a
//! person does in it reaches the session through the seam, what it shows
//! arrives through the seam, and how a session reads is decided once. Each
//! check runs the real binary under a pseudo-terminal against a scripted
//! provider, and puts a second client on the same session's port.

#[path = "support/engine.rs"]
mod engine;
#[path = "support/terminal.rs"]
mod terminal;

use engine::{Conn, Provider, World, ending, turn_ended, turn_of};
use serde_json::{Value, json};
use std::time::Duration;
use terminal::Terminal;

/// A terminal session in `folder`, with no installed program reachable,
/// on a terminal wide enough that no path or line wraps.
fn terminal(world: &World, folder: &std::path::Path, args: &[&str], rows: u16) -> Terminal {
    let mut variables = world.variables();
    variables.push(("PATH".into(), String::new()));
    let mut term = Terminal::sized(folder, args, &variables, rows, 160);
    term.ready();
    term
}

/// The one running session's port: the terminal's own.
fn watch(world: &World, client: &str) -> Conn {
    let live = world.wait_live(1);
    assert_eq!(live.len(), 1, "one terminal, one running session: {live:?}");
    Conn::session(&live[0], client)
}

/// Everything a person does in the terminal -- a message, an answer to an
/// approval, a message queued behind a turn and taken back, a stop, a
/// control -- arrives at the session as the seam's own commands: a second
/// client watching the same session sees each one happen, and an answer the
/// terminal gave is settled by the terminal.
#[test]
fn what_a_person_does_in_the_terminal_reaches_a_second_client_through_the_seam() {
    // The terminal keeps one conversation across its messages, so each reply
    // is chosen by the newest message that asks for it.
    let provider = Provider::paced(|request| {
        if engine::replies_since(request, "write the file") == Some(0) {
            return (
                Duration::ZERO,
                engine::cell(
                    "w",
                    r#"write({path: "asked.txt", content: "once"}); return "written";"#,
                ),
            );
        }
        if engine::replies_since(request, "take your time") == Some(0) {
            return (Duration::from_secs(4), engine::cell("slow", r#"return 1;"#));
        }
        (Duration::ZERO, ending("done"))
    });
    let world = World::new("terminal-actions", &provider);
    let folder = world.folder("project");
    let mut term = terminal(&world, &folder, &["--sandbox", "ask"], 40);
    let mut watcher = watch(&world, "watcher");
    watcher.attach();

    term.say("write the file");
    let events = watcher.until(|event| event["kind"] == "prompt");
    let prompt = events.last().unwrap()["prompt"].clone();
    assert_eq!(prompt["type"], "approval", "{prompt}");
    term.contains("APPROVE");
    term.settle(700);
    term.send(b"o");
    let events = watcher.until(|event| event["kind"] == "settled");
    let settled = events.last().unwrap();
    assert_eq!(settled["id"], prompt["id"], "{settled}");
    assert_eq!(
        settled["by"], "terminal",
        "the terminal's answer is the terminal's: {settled}"
    );
    watcher.until_turn_ends();
    assert_eq!(
        std::fs::read_to_string(folder.join("asked.txt")).unwrap(),
        "once"
    );

    // A turn that takes its time: a message typed behind it waits in the
    // session's queue, Esc takes it back, and the next Esc stops the turn.
    term.say("take your time");
    watcher.until(|event| event["kind"] == "activity" && event["activity"] == "thinking");
    term.say("and then this");
    let queued = watcher.until(|event| event["kind"] == "queue");
    assert_eq!(
        queued.last().unwrap()["items"],
        json!(["and then this"]),
        "the queue is the session's"
    );
    term.send(b"\x1b");
    let emptied = watcher.until(|event| event["kind"] == "queue");
    assert_eq!(emptied.last().unwrap()["items"], json!([]));
    term.send(b"\x1b");
    let ended = watcher.until(turn_ended);
    assert_eq!(ended.last().unwrap()["activity"], "stopped");

    // The message taken back is in the composer again; sent, it is a turn
    // of its own.
    term.send(b"\r");
    let resent = watcher.until(|event| {
        event["kind"] == "transcript"
            && event["conversation"]["messages"]
                .as_array()
                .is_some_and(|messages| {
                    messages
                        .iter()
                        .any(|m| m["role"] == "user" && m.to_string().contains("and then this"))
                })
    });
    assert!(!resent.is_empty());
    watcher.until(turn_ended);

    term.say("/effort high");
    let facts =
        watcher.until(|event| event["kind"] == "facts" && event["facts"]["effort"] == "high");
    assert!(!facts.is_empty());
}

/// The replies for a turn a client watches in full: readable reasoning,
/// prose, and one cell that writes a file, prints and answers -- so the
/// answer's facts are that cell's own.
fn whole_turn() -> Provider {
    Provider::paced(|request| {
        match turn_of(request) {
        0 => (
            Duration::from_millis(1500),
            json!({
                "role": "assistant",
                "content": [
                    {"type":"thinking","thinking":"Notes first, then the answer.","signature":"sig-1"},
                    {"type":"text","text":"Writing the notes now."},
                    {"type":"tool_use","id":"n","name":"execute_cell","input":{"code":
                        "write({path: \"notes.txt\", content: \"hello\\n\"}); console.log([\"wrote\", \"notes\"].join(\" \")); answer(\"The notes are written.\");"}},
                ],
                "usage": {"input_tokens": 10, "output_tokens": 5},
            })
            .to_string(),
        ),
        _ => (Duration::ZERO, ending("The notes are written.")),
    }
    })
}

/// Everything the terminal shows of a turn arrives through the seam, live --
/// the reasoning and its clock, the prose as it comes, the cell as it is
/// written, the running cell's clock, its diff and its output -- and a client
/// that attaches afterwards from the first event receives exactly the lines
/// a client watching live received.
#[test]
fn the_terminal_and_a_second_client_receive_the_same_live_session() {
    let provider = whole_turn();
    let world = World::new("terminal-live", &provider);
    let folder = world.folder("project");
    let mut term = terminal(&world, &folder, &[], 50);
    let mut early = watch(&world, "early");
    early.send(json!({"do":"attach","from":1}));

    term.say("make notes");
    let live = early.until(turn_ended);
    let last_seq = live.last().unwrap()["seq"].as_u64().unwrap();

    let of = |kind: &str| -> Vec<&Value> { live.iter().filter(|e| e["kind"] == kind).collect() };
    let joined = |kind: &str| -> String {
        of(kind)
            .iter()
            .filter_map(|e| e["text"].as_str())
            .collect::<String>()
    };
    let thinking = live
        .iter()
        .find(|e| e["kind"] == "activity" && e["activity"] == "thinking")
        .expect("the reasoning row's clock starts from an event");
    assert!(thinking["since"].as_u64().is_some(), "{thinking}");
    assert!(
        joined("reasoning").contains("Notes first"),
        "{:?}",
        engine::kinds(&live)
    );
    // The reasoning arrives while the turn is still thinking: before the
    // answer streams or the cell runs.
    let first_reasoning = live.iter().position(|e| e["kind"] == "reasoning").unwrap();
    let first_after = live
        .iter()
        .position(|e| {
            e["kind"] == "activity"
                && matches!(e["activity"].as_str(), Some("streaming" | "executing"))
        })
        .expect("the turn streams and runs its cell");
    assert!(first_reasoning < first_after, "{:?}", engine::kinds(&live));
    assert!(joined("delta").contains("Writing the notes now."));
    assert!(joined("tool_delta").contains("notes.txt"));
    let running = live
        .iter()
        .find(|e| e["kind"] == "activity" && e["activity"] == "executing")
        .expect("a running cell's clock starts from an event");
    assert!(running["since"].as_u64().is_some(), "{running}");
    let record = of("transcript")
        .into_iter()
        .rev()
        .find(|e| e["notebook"]["cells"][0]["changes"].is_string())
        .expect("a transcript carries the cell's diff");
    let cell = &record["notebook"]["cells"][0];
    assert!(
        cell["changes"].as_str().unwrap().contains("notes.txt"),
        "{cell}"
    );
    assert!(
        cell["stdout"]
            .as_str()
            .unwrap_or("")
            .contains("wrote notes"),
        "{cell}"
    );
    assert_eq!(live.last().unwrap()["activity"], "complete");

    // The terminal shows what the client received: the prose, and the
    // cell's output on the card's Full output tab, where a person opens it.
    term.contains("Writing the notes now.");
    term.click_on("Full output");
    term.contains("wrote notes");

    // A client that comes later, from the first event, receives the same lines.
    let mut late = watch(&world, "late");
    let replayed = late.attach_from(1, |event| event["seq"].as_u64() == Some(last_seq));
    let first = live[0]["seq"].as_u64().unwrap();
    let replayed: Vec<&Value> = replayed
        .iter()
        .filter(|e| e["seq"].as_u64().is_some_and(|seq| seq >= first))
        .collect();
    let live: Vec<&Value> = live.iter().collect();
    assert_eq!(
        replayed, live,
        "a late client from the first event receives what a live one did"
    );
}

/// How a session reads is decided once: the state on a cell's card, the line
/// under it and the facts under the answer arrive as words, the terminal
/// draws exactly those words, and a client attaching later is given the same.
#[test]
fn a_cells_state_its_line_and_the_answers_facts_read_the_same_in_every_client() {
    let provider = whole_turn();
    let world = World::new("terminal-reading", &provider);
    let folder = world.folder("project");
    let mut term = terminal(&world, &folder, &[], 50);
    let mut watcher = watch(&world, "watcher");
    watcher.attach();

    term.say("make notes");
    let events = watcher.until(turn_ended);
    let reading = events
        .iter()
        .rev()
        .find(|e| e["kind"] == "transcript")
        .expect("the turn's record arrives with its reading")["reading"]
        .clone();
    let cell = reading["cells"]
        .as_array()
        .and_then(|cells| cells.iter().find(|c| c["cell"] == 1))
        .unwrap_or_else(|| panic!("the first cell has a reading: {reading}"))
        .clone();
    let state = cell["state"].as_str().unwrap();
    let line = cell["line"].as_str().unwrap();
    let facts = reading["answer"]["facts"].as_str().unwrap();
    for words in [state, line, facts] {
        assert!(!words.trim().is_empty(), "{reading}");
        term.contains(words);
    }

    let mut later = watch(&world, "later");
    assert_eq!(later.attach()["reading"], reading);
}
