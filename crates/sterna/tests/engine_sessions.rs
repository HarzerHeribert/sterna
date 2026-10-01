//! Sessions the host starts (`docs/engine.md`): several run at once, each in
//! its own folder, with no terminal anywhere, and the host adds up what they
//! used. Each check starts one host in a scratch world, starts its sessions
//! on the host port, and watches each one on its own session port.

#[path = "support/engine.rs"]
mod engine;

use engine::{
    Conn, Host, Provider, World, cell, ending, files_under, task_of, turn_ended, turn_of,
    with_usage,
};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const FOLDERS: [&str; 2] = ["alpha", "beta"];

fn task_for(name: &str) -> String {
    format!("Write {name}.txt in this folder.")
}

/// Which folder's task a request serves. The session puts its orientation
/// after the person's words in the same message.
fn folder_of(request: &Value) -> Option<&'static str> {
    let task = task_of(request);
    FOLDERS
        .into_iter()
        .find(|name| task.starts_with(&task_for(name)))
}

fn written_by(name: &str) -> String {
    format!("written by the {name} session")
}

/// The model's side of a folder's task: a cell that writes the folder's
/// file, then the answer.
fn reply(name: &str, turn: usize) -> String {
    if turn == 0 {
        cell(
            "write",
            &format!(
                "await write({{path: '{name}.txt', content: '{}'}});",
                written_by(name)
            ),
        )
    } else {
        ending(&format!("{name}.txt is written."))
    }
}

/// Starts a session in `root` with `task` on the host port; the answer is
/// its `{"id","listening","token"}`.
fn start(host: &Host, root: &Path, task: &str) -> Value {
    let started = host.ask(json!({"do":"start","root":root.to_str().unwrap(),"task":task}));
    assert!(
        started["id"].is_string(),
        "start names the session: {started}"
    );
    started
}

/// Reads a started session from its first event until its turn ends, and
/// returns the event that ended it. From the first event, so a turn that
/// ended before the check attached is still seen ending.
fn until_its_turn_ends(started: &Value) -> Value {
    let mut conn = Conn::session(started, "check");
    let events = conn.attach_from(1, turn_ended);
    let last = events.last().unwrap().clone();
    assert_eq!(
        last["activity"],
        "complete",
        "the task in {} ends complete: {:?}",
        started["id"],
        engine::kinds(&events)
    );
    last
}

fn unix_ms() -> u64 {
    let since = SystemTime::now().duration_since(UNIX_EPOCH).unwrap();
    u64::try_from(since.as_millis()).unwrap()
}

/// Two sessions in two folders, started on the host port with nobody at a
/// terminal, both finish their task; they ran at the same time, and each
/// wrote only inside its own folder.
#[test]
#[ignore = "plan goal 1"]
fn two_sessions_in_two_folders_run_at_once_without_a_terminal_and_each_writes_only_its_own() {
    // Every reply waits, so a turn is still open when the other task asks.
    const PACE: Duration = Duration::from_secs(3);
    let first_asked: Arc<Mutex<BTreeMap<&'static str, u64>>> = Arc::default();
    let record = Arc::clone(&first_asked);
    let provider = Provider::paced(move |request| {
        let Some(name) = folder_of(request) else {
            return (Duration::ZERO, ending("This request serves neither task."));
        };
        let turn = turn_of(request);
        if turn == 0 {
            record.lock().unwrap().entry(name).or_insert_with(unix_ms);
        }
        (PACE, reply(name, turn))
    });
    let world = World::new("two-folders", &provider);
    let alpha = world.folder("alpha");
    let beta = world.folder("beta");
    let host = world.host();

    let started_alpha = start(&host, &alpha, &task_for("alpha"));
    let started_beta = start(&host, &beta, &task_for("beta"));
    let ended = [
        until_its_turn_ends(&started_alpha),
        until_its_turn_ends(&started_beta),
    ];

    // Both tasks had asked the model before either turn ended. Had the host
    // run them one after the other, or started a task only once a client
    // attached, the second would first ask after the first had ended.
    let first_asked = first_asked.lock().unwrap().clone();
    assert_eq!(
        first_asked.len(),
        2,
        "both tasks reached the model: {first_asked:?}"
    );
    let last_to_ask = *first_asked.values().max().unwrap();
    let ended_at: Vec<u64> = ended
        .iter()
        .map(|event| {
            event["at"]
                .as_u64()
                .unwrap_or_else(|| panic!("an event says when it happened: {event}"))
        })
        .collect();
    let first_to_end = *ended_at.iter().min().unwrap();
    assert!(
        last_to_ask < first_to_end,
        "the two sessions ran at the same time: first requests at {first_asked:?}, \
         turns ended at {ended_at:?} (unix ms)"
    );

    assert_eq!(files_under(&alpha), ["alpha.txt"]);
    assert_eq!(files_under(&beta), ["beta.txt"]);
    for (root, name) in [(&alpha, "alpha"), (&beta, "beta")] {
        assert_eq!(
            std::fs::read_to_string(root.join(format!("{name}.txt"))).unwrap(),
            written_by(name)
        );
    }
    let allowed = [
        "alpha/",
        "beta/",
        "data/sterna/",
        "global-config/",
        "gateway-config/",
        "gateway-data/",
    ];
    let elsewhere: Vec<String> = files_under(&world.base)
        .into_iter()
        .filter(|path| !allowed.iter().any(|root| path.starts_with(root)))
        .collect();
    assert!(
        elsewhere.is_empty(),
        "nothing is written outside the two folders, the data folder, the settings root \
         and the gateway's roots: {elsewhere:?} under {}",
        world.base.display()
    );

    host.shutdown();
}

/// What a folder's reply says it cost. Distinct for every folder and every
/// reply, so a figure that misses a reply, counts one twice or takes the
/// other session's reads differently from the right one.
fn cost(name: &str, turn: usize) -> (u64, u64) {
    let (input, output) = if name == "alpha" {
        (1_000, 100)
    } else {
        (40_000, 4_000)
    };
    let nth = turn as u64 + 1;
    (input * nth, output * nth)
}

/// Input and output tokens the provider's replies said, summed per folder.
type Said = Arc<Mutex<BTreeMap<&'static str, (u64, u64)>>>;

/// A `usage` answer reduced to what this check compares: each session's
/// `[id, input, output]`, ordered by id, and the total's `[input, output]`.
fn tally(usage: &Value) -> Value {
    let mut sessions: Vec<Value> = usage["sessions"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|session| {
            json!([
                session["id"],
                session["input_tokens"],
                session["output_tokens"]
            ])
        })
        .collect();
    sessions.sort_by_key(|session| session[0].to_string());
    json!({
        "sessions": sessions,
        "total": [usage["total"]["input_tokens"], usage["total"]["output_tokens"]],
    })
}

/// Two sessions' requests appear in the host's usage: each session's tokens
/// are what its own replies said, and the total is their sum.
#[test]
#[ignore = "plan goal 9"]
fn the_hosts_usage_counts_each_sessions_replies_and_totals_their_sum() {
    let said: Said = Arc::default();
    let record = Arc::clone(&said);
    let provider = Provider::start(move |request| {
        let Some(name) = folder_of(request) else {
            return with_usage(&ending("This request serves neither task."), 0, 0);
        };
        let turn = turn_of(request);
        let (input, output) = cost(name, turn);
        let mut said = record.lock().unwrap();
        let sum = said.entry(name).or_default();
        *sum = (sum.0 + input, sum.1 + output);
        with_usage(&reply(name, turn), input, output)
    });
    let world = World::new("usage-sum", &provider);
    let alpha = world.folder("alpha");
    let beta = world.folder("beta");
    let host = world.host();

    let started_alpha = start(&host, &alpha, &task_for("alpha"));
    let started_beta = start(&host, &beta, &task_for("beta"));
    until_its_turn_ends(&started_alpha);
    until_its_turn_ends(&started_beta);

    let said = said.lock().unwrap().clone();
    assert_eq!(said.len(), 2, "both tasks reached the model: {said:?}");
    let (alpha_in, alpha_out) = said["alpha"];
    let (beta_in, beta_out) = said["beta"];
    let wanted = tally(&json!({
        "sessions": [
            {"id": started_alpha["id"], "input_tokens": alpha_in, "output_tokens": alpha_out},
            {"id": started_beta["id"], "input_tokens": beta_in, "output_tokens": beta_out},
        ],
        "total": {"input_tokens": alpha_in + beta_in, "output_tokens": alpha_out + beta_out},
    }));

    // The host may count a session's last reply a moment after the session's
    // own port said the turn ended.
    let deadline = Instant::now() + engine::PATIENCE;
    let mut usage = host.ask(json!({"do":"usage"}));
    while tally(&usage) != wanted && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(100));
        usage = host.ask(json!({"do":"usage"}));
    }
    assert_eq!(
        tally(&usage),
        wanted,
        "each session's usage is what its replies said and the total is their sum: {usage}"
    );

    host.shutdown();
}
