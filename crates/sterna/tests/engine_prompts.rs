//! A prompt -- an approval or a question -- is answered once, from any client
//! (`docs/engine.md`). Two clients attach to one session the host started:
//! both are shown the prompt, one answers it, both see it settled by that
//! one, and the other's answer to it is refused, to that client alone, and
//! changes nothing. The other client then answers the prompt that comes next,
//! so its refusal was about the settled prompt and never about who sent it.

#[path = "support/engine.rs"]
mod engine;

use engine::{Conn, Provider, World, cell, ending, files_under, turn_of};
use serde_json::{Value, json};

/// One attached client: every event it has read, and those no check has
/// taken yet. A check takes the first event it wants wherever it arrived, so
/// none depends on the order of events the contract leaves open -- a
/// `settled` and the next `prompt` it lets through, say.
struct Client {
    conn: Conn,
    seen: Vec<Value>,
    waiting: Vec<Value>,
}

impl Client {
    /// Connects to the session `started` names, as `name`, and attaches.
    fn attach(started: &Value, name: &str) -> Self {
        let mut conn = Conn::session(started, name);
        conn.attach();
        Self {
            conn,
            seen: Vec::new(),
            waiting: Vec::new(),
        }
    }

    fn send(&mut self, command: Value) {
        self.conn.send(command);
    }

    /// Answers `prompt`, the `prompt` object of a prompt event.
    fn answer(&mut self, prompt: &Value, answer: Value) {
        self.send(json!({"do":"answer","prompt":prompt["id"],"answer":answer}));
    }

    /// The first event not yet taken that satisfies `wanted`, read from the
    /// session if none has arrived yet.
    fn take(&mut self, wanted: impl Fn(&Value) -> bool) -> Value {
        if let Some(at) = self.waiting.iter().position(&wanted) {
            return self.waiting.remove(at);
        }
        let mut events = self.conn.until(&wanted);
        self.seen.extend(events.iter().cloned());
        let found = events.pop().unwrap();
        self.waiting.extend(events);
        found
    }

    /// The next prompt event.
    fn prompt(&mut self) -> Value {
        self.take(|event| event["kind"] == "prompt")
    }

    /// The event that settles `prompt`.
    fn settled(&mut self, prompt: &Value) -> Value {
        let id = prompt["id"].clone();
        self.take(|event| event["kind"] == "settled" && event["id"] == id)
    }

    /// The next refusal.
    fn refused(&mut self) -> Value {
        self.take(|event| event["kind"] == "refused")
    }

    /// The activity event that ends the turn under way.
    fn turn_ends(&mut self) -> Value {
        self.take(engine::turn_ended)
    }

    /// How many times this client was told `prompt` was settled.
    fn settlements(&self, prompt: &Value) -> usize {
        self.seen
            .iter()
            .filter(|event| event["kind"] == "settled" && event["id"] == prompt["id"])
            .count()
    }
}

/// Two calls that each wait for a person on Ask: the first writes `once.txt`,
/// the second `twice.txt` if its own answer allows it.
const TWO_WRITES: &str = r#"await write({path: "once.txt", content: "allowed once"});
let second = "refused";
try {
  await write({path: "twice.txt", content: "must not appear"});
  second = "written";
} catch (error) {}
answer("the second write was " + second);"#;

/// An approval is settled by the first client to answer it: the other sees
/// it settled, its own answer is refused, and the call ran once. The next
/// approval is the other client's to answer.
#[test]
fn an_approval_is_answered_once_and_the_other_client_cannot_answer_it_again() {
    let provider = Provider::start(|request| match turn_of(request) {
        0 => cell("gated", TWO_WRITES),
        _ => ending("finished"),
    });
    let world = World::new("prompts-approval", &provider);
    // `sandbox.level` is global only. On Ask every edit waits for a person.
    let global = world.base.join("global-config").join("sterna");
    std::fs::create_dir_all(&global).unwrap();
    std::fs::write(global.join("config.toml"), "[sandbox]\nlevel = \"ask\"\n").unwrap();
    let folder = world.folder("project");
    let host = world.host();
    let started = host.ask(json!({"do":"start","root":folder}));
    let mut one = Client::attach(&started, "one");
    let mut two = Client::attach(&started, "two");

    one.send(json!({"do":"submit","text":"write the two files"}));

    // Both clients are shown the one approval, and nothing is written yet.
    let first = one.prompt();
    assert_eq!(
        two.prompt(),
        first,
        "both clients see the same prompt event"
    );
    let first = first["prompt"].clone();
    assert_eq!(first["type"], "approval", "{first}");
    assert!(
        first["target"].as_str().unwrap_or("").contains("once.txt"),
        "the first approval is for the first write: {first}"
    );
    assert!(!folder.join("once.txt").exists());

    // One answers, and both see it settled by one, with that answer.
    one.answer(&first, json!({"approval":"allow_once"}));
    let settled = one.settled(&first);
    assert_eq!(
        two.settled(&first),
        settled,
        "both clients see the same settlement"
    );
    assert_eq!(settled["by"], "one", "{settled}");
    assert_eq!(
        settled["answer"],
        json!({"approval":"allow_once"}),
        "{settled}"
    );

    // The call ran, and the cell's next call waits for its own answer.
    let next = one.prompt();
    assert_eq!(two.prompt(), next, "both clients see the next prompt");
    let next = next["prompt"].clone();
    assert_ne!(next["id"], first["id"], "a new prompt has a new id: {next}");
    assert!(
        next["target"].as_str().unwrap_or("").contains("twice.txt"),
        "the next approval is for the second write: {next}"
    );
    assert_eq!(
        std::fs::read_to_string(folder.join("once.txt")).unwrap(),
        "allowed once"
    );

    // Two's answer to the settled prompt is refused. It is not taken for
    // the prompt that is waiting now either: that one is settled below by
    // two's own answer to it, and nothing more is written.
    two.answer(&first, json!({"approval":"allow_for_session"}));
    let refused = two.refused();
    assert_eq!(refused["to"], "answer", "{refused}");
    assert!(
        refused["reason"]
            .as_str()
            .is_some_and(|reason| !reason.trim().is_empty()),
        "a refusal says why: {refused}"
    );
    assert!(!folder.join("twice.txt").exists());

    // The waiting prompt is two's to answer.
    two.answer(&next, json!({"approval":"deny_once"}));
    let settled = two.settled(&next);
    assert_eq!(one.settled(&next), settled);
    assert_eq!(settled["by"], "two", "{settled}");
    assert_eq!(
        settled["answer"],
        json!({"approval":"deny_once"}),
        "{settled}"
    );

    assert_eq!(one.turn_ends()["activity"], "complete");
    two.turn_ends();
    assert!(
        one.seen.iter().all(|event| event["kind"] != "refused"),
        "a refusal goes only to the client it answers"
    );
    for client in [&one, &two] {
        assert_eq!(client.settlements(&first), 1, "{:?}", client.seen);
        assert_eq!(client.settlements(&next), 1, "{:?}", client.seen);
    }
    // The first write ran exactly once, and the second never ran.
    assert_eq!(
        std::fs::read_to_string(folder.join("once.txt")).unwrap(),
        "allowed once"
    );
    assert_eq!(files_under(&folder), ["once.txt"]);
}

/// A question is settled by the first client to answer it: the other sees it
/// settled, its own answer is refused, and the model is told the first
/// answer and never the refused one. The next question is the other client's
/// to answer.
#[test]
fn a_question_is_answered_once_and_the_other_client_cannot_answer_it_again() {
    // The choices are built inside the cell, so a choice's whole name reaches
    // the model only as an answer.
    let provider = Provider::start(|request| match turn_of(request) {
        0 => cell(
            "lantern",
            r#"ask("Which lantern?", ["copper", "slate"].map((name) => name + "-lantern"));"#,
        ),
        1 => cell(
            "wick",
            r#"ask("Which wick?", ["short", "long"].map((name) => name + "-wick"));"#,
        ),
        _ => ending("lit"),
    });
    let world = World::new("prompts-question", &provider);
    let folder = world.folder("project");
    // Asking is off unless the settings turn it on; no decision model answers
    // for the person.
    let settings = folder.join(".sterna").join("config.toml");
    let mut config = std::fs::read_to_string(&settings).unwrap();
    config.push_str("\n[ask]\nenabled = true\njev = \"off\"\n");
    std::fs::write(&settings, config).unwrap();
    let host = world.host();
    let started = host.ask(json!({"do":"start","root":folder}));
    let mut one = Client::attach(&started, "one");
    let mut two = Client::attach(&started, "two");

    one.send(json!({"do":"submit","text":"pick a lantern and a wick"}));

    // Both clients are shown the one question.
    let first = one.prompt();
    assert_eq!(
        two.prompt(),
        first,
        "both clients see the same prompt event"
    );
    let first = first["prompt"].clone();
    assert_eq!(first["type"], "question", "{first}");
    assert_eq!(first["question"], "Which lantern?", "{first}");
    assert_eq!(
        first["choices"],
        json!(["copper-lantern", "slate-lantern"]),
        "{first}"
    );

    // One answers, and both see it settled by one, with that answer.
    one.answer(&first, json!({"choice":"slate-lantern"}));
    let settled = one.settled(&first);
    assert_eq!(
        two.settled(&first),
        settled,
        "both clients see the same settlement"
    );
    assert_eq!(settled["by"], "one", "{settled}");
    assert_eq!(
        settled["answer"],
        json!({"choice":"slate-lantern"}),
        "{settled}"
    );

    // The answer reached the model, whose next cell asks the next question.
    let next = one.prompt();
    assert_eq!(two.prompt(), next, "both clients see the next prompt");
    let next = next["prompt"].clone();
    assert_ne!(next["id"], first["id"], "a new prompt has a new id: {next}");
    assert_eq!(next["question"], "Which wick?", "{next}");

    // Two's answer to the settled question is refused. It is not taken for
    // the question that is waiting now either: that one is settled below by
    // two's own answer to it.
    two.answer(&first, json!({"choice":"copper-lantern"}));
    let refused = two.refused();
    assert_eq!(refused["to"], "answer", "{refused}");
    assert!(
        refused["reason"]
            .as_str()
            .is_some_and(|reason| !reason.trim().is_empty()),
        "a refusal says why: {refused}"
    );

    // The waiting question is two's to answer.
    two.answer(&next, json!({"choice":"long-wick"}));
    let settled = two.settled(&next);
    assert_eq!(one.settled(&next), settled);
    assert_eq!(settled["by"], "two", "{settled}");
    assert_eq!(
        settled["answer"],
        json!({"choice":"long-wick"}),
        "{settled}"
    );

    assert_eq!(one.turn_ends()["activity"], "complete");
    two.turn_ends();
    assert!(
        one.seen.iter().all(|event| event["kind"] != "refused"),
        "a refusal goes only to the client it answers"
    );
    for client in [&one, &two] {
        assert_eq!(client.settlements(&first), 1, "{:?}", client.seen);
        assert_eq!(client.settlements(&next), 1, "{:?}", client.seen);
    }

    // What the model was told: each question's one answer, and never the
    // refused one.
    let requests = provider.requests();
    let sent = |turn: usize| -> String {
        requests
            .iter()
            .find(|seen| turn_of(&seen.body) == turn)
            .unwrap_or_else(|| panic!("no request followed answer {turn}"))
            .body
            .to_string()
    };
    assert!(sent(1).contains("slate-lantern"), "{}", sent(1));
    assert!(sent(2).contains("long-wick"), "{}", sent(2));
    for seen in &requests {
        assert!(
            !seen.body.to_string().contains("copper-lantern"),
            "the refused answer reached the model: {}",
            seen.body
        );
    }
}
