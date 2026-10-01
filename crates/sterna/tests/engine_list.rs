//! The one list of folders and sessions (`docs/engine.md`, "Where to find
//! them" and "The host port"): plan goal 5. A session started in the
//! terminal is in the list the host keeps; the list is the same after the
//! host restarts; and the terminal's own listing still keeps to the folder
//! it runs in, however many folders the list holds.
//!
//! Every check drives the built `sterna` binary: a terminal session under a
//! pseudo-terminal, `sterna host`, or `sterna --sessions` as a plain
//! process. The provider is the scripted one in `support/engine.rs`, and
//! every folder and data folder is a scratch directory of the check's own.

#[path = "support/engine.rs"]
mod engine;
#[path = "support/terminal.rs"]
mod terminal;

use engine::{Conn, Host, Provider, World, cell, rollout_ids, turn_ended};
use serde_json::{Value, json};
use std::path::Path;
use std::thread;
use std::time::Duration;
use terminal::Terminal;

/// What every task in this file ends with. It is put together when the cell
/// runs, so these words are on the screen or in the record only once the
/// answer was given, never because the cell's own source is drawn.
const ANSWER: &str = "listed and kept";

fn answering() -> Provider {
    Provider::start(|_| cell("end", r#"answer(["listed", "and", "kept"].join(" "));"#))
}

/// A gap longer than a second between two uses, so a list that keeps its
/// times in whole seconds still tells them apart.
fn later() {
    thread::sleep(Duration::from_millis(1500));
}

/// Whether a folder the list names is `folder`. Both sides are resolved, so
/// a list that keeps the path another way (without Windows' `\\?\`, say) is
/// not a different folder.
fn same_folder(listed: &Value, folder: &Path) -> bool {
    listed["root"]
        .as_str()
        .is_some_and(|root| engine::same_path(Path::new(root), folder))
}

/// The list's entry for `folder`; fails the check with the list when there
/// is none.
fn folder_in<'a>(list: &'a Value, folder: &Path) -> &'a Value {
    list["folders"]
        .as_array()
        .unwrap_or_else(|| panic!("the list has no folders array: {list}"))
        .iter()
        .find(|entry| same_folder(entry, folder))
        .unwrap_or_else(|| panic!("{} is not in the list: {list}", folder.display()))
}

/// The ids of a folder's sessions, in the order the list gives them.
fn ids_of(entry: &Value) -> Vec<String> {
    entry["sessions"]
        .as_array()
        .unwrap_or_else(|| panic!("a folder without a sessions array: {entry}"))
        .iter()
        .map(|session| {
            session["id"]
                .as_str()
                .unwrap_or_else(|| panic!("a session without an id: {session}"))
                .to_string()
        })
        .collect()
}

/// The list with every `live` field emptied: what a restart may change.
fn without_live(list: &Value) -> Value {
    let mut list = list.clone();
    for folder in list["folders"].as_array_mut().into_iter().flatten() {
        for session in folder["sessions"].as_array_mut().into_iter().flatten() {
            session["live"] = Value::Null;
        }
    }
    list
}

/// The host starts a session in `root`; a client attaches, sends `task`,
/// and waits for the turn to end. Returns the session's id.
fn finish_task(host: &Host, root: &Path, task: &str) -> String {
    let started = host.ask(json!({"do":"start","root":root.to_string_lossy()}));
    let id = started["id"]
        .as_str()
        .unwrap_or_else(|| panic!("start named no session: {started}"))
        .to_string();
    let mut session = Conn::open(
        started["listening"].as_str().unwrap(),
        started["token"].as_str().unwrap(),
        "app",
    );
    // Attached before the task is sent, so the turn's end is an event this
    // client sees rather than one that may have passed already.
    session.attach();
    session.send(json!({"do":"submit","text":task}));
    let events = session.until(turn_ended);
    assert_eq!(
        events.last().unwrap()["activity"],
        "complete",
        "the task in {} did not end complete",
        root.display()
    );
    id
}

/// What `sterna --sessions` prints, run as a person would in `folder`.
fn listing(world: &World, folder: &Path) -> String {
    let output = world
        .sterna()
        .arg("--sessions")
        .current_dir(folder)
        .output()
        .expect("sterna --sessions runs");
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    assert!(
        output.status.success(),
        "sterna --sessions in {} failed: {stdout}{}",
        folder.display(),
        String::from_utf8_lossy(&output.stderr)
    );
    stdout
}

/// Whether `id` stands as a word of its own in `text`: one id may begin
/// with another (the same second, a longer process number).
fn names(text: &str, id: &str) -> bool {
    text.split_whitespace().any(|word| word == id)
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

/// A session a person ran in the terminal, with no host running at the
/// time, is in the list the host gives once one starts: its folder, and the
/// session under it by the id its record file carries. The terminal has
/// quit, so the session is not running.
#[test]
fn a_session_run_in_the_terminal_is_in_the_hosts_list_after_the_terminal_quits() {
    let provider = answering();
    let world = World::new("list-terminal", &provider);
    let folder = world.folder("a");
    let mut variables = world.variables();
    // As `tui_live.rs` starts a bare session: no installed program on PATH
    // receives this session's lifecycle events.
    variables.push(("PATH".into(), String::new()));
    let mut terminal = Terminal::start(&folder, &[], &variables, 100);
    terminal.ready();
    terminal.say("keep a note of where this session lives");
    terminal.contains(ANSWER);
    terminal.settle(500);
    // Ctrl-D on an empty composer between turns quits. Pressed again if the
    // turn was still closing when the first one arrived.
    let mut quit = false;
    for _ in 0..3 {
        terminal.send(b"\x04");
        if terminal.exited_within(5_000) {
            quit = true;
            break;
        }
    }
    assert!(quit, "Ctrl-D did not quit:\n{}", terminal.contents());
    let recorded = rollout_ids(&folder);
    assert_eq!(recorded.len(), 1, "one session, one record: {recorded:?}");

    let host = world.host();
    let _end = EndLive(&world);
    let list = host.ask(json!({"do":"list"}));
    let folders = list["folders"].as_array().unwrap();
    assert_eq!(folders.len(), 1, "one folder was used: {list}");
    let entry = folder_in(&list, &folder);
    assert_eq!(ids_of(entry), recorded, "the terminal's session: {list}");
    assert!(
        entry["sessions"][0]["live"].is_null(),
        "the terminal quit, so its session is not running: {list}"
    );
}

/// The list is kept, not rebuilt from what happens to be running: after the
/// host shuts down and a new one starts, it is the same list, folders and
/// sessions newest first, with only whether each is running changed.
///
/// West is used, then east, then west again. Newest first is west, east;
/// the folders' names sort the other way, and so does the order each was
/// first used in, so neither can stand in for when each was last used.
#[test]
fn the_list_is_the_same_after_the_host_restarts_with_folders_and_sessions_newest_first() {
    let provider = answering();
    let world = World::new("list-restart", &provider);
    let west = world.folder("west");
    let east = world.folder("east");
    let host = world.host();
    let _end = EndLive(&world);
    let first = finish_task(&host, &west, "the first task, in the west folder");
    later();
    let second = finish_task(&host, &east, "a task in the east folder");
    later();
    let third = finish_task(&host, &west, "a later task, in the west folder again");

    let before = host.ask(json!({"do":"list"}));
    let folders = before["folders"].as_array().unwrap();
    assert_eq!(folders.len(), 2, "two folders were used: {before}");
    assert!(
        same_folder(&folders[0], &west) && same_folder(&folders[1], &east),
        "the folder used last comes first: {before}"
    );
    assert_eq!(
        ids_of(&folders[0]),
        [third, first],
        "the session used last comes first: {before}"
    );
    assert_eq!(ids_of(&folders[1]), [second], "{before}");

    host.shutdown();
    let host = world.host();
    let after = host.ask(json!({"do":"list"}));
    assert_eq!(
        without_live(&after),
        without_live(&before),
        "the list changed across a restart"
    );
}

/// The list holds every folder, and `sterna --sessions` -- the listing
/// `/resume` and `--resume` offer from -- still shows only the folder it
/// runs in: folder A's session and not folder B's, and the reverse in B.
#[test]
fn the_terminals_listing_keeps_to_its_own_folder_while_the_list_holds_both() {
    let provider = answering();
    let world = World::new("list-folders", &provider);
    let a = world.folder("a");
    let b = world.folder("b");
    let host = world.host();
    let _end = EndLive(&world);
    let in_a = finish_task(&host, &a, "a task in folder A");
    let in_b = finish_task(&host, &b, "a task in folder B");
    // The one list has both, so what follows is the terminal keeping to its
    // folder and not a list that never held the other one.
    let list = host.ask(json!({"do":"list"}));
    assert_eq!(ids_of(folder_in(&list, &a)), [in_a.as_str()], "{list}");
    assert_eq!(ids_of(folder_in(&list, &b)), [in_b.as_str()], "{list}");

    // contract: as in `rollout_ids`, the id the host gave a session is the
    // one its folder's listing prints.
    let from_a = listing(&world, &a);
    assert!(
        names(&from_a, &in_a),
        "A's session is not offered in A:\n{from_a}"
    );
    assert!(
        !names(&from_a, &in_b),
        "B's session is offered in A:\n{from_a}"
    );
    let from_b = listing(&world, &b);
    assert!(
        names(&from_b, &in_b),
        "B's session is not offered in B:\n{from_b}"
    );
    assert!(
        !names(&from_b, &in_a),
        "A's session is offered in B:\n{from_b}"
    );
}
