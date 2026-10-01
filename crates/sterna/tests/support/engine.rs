//! What the engine's checks share: a scripted provider that answers every
//! session at once, the host, and a client that speaks the seam's lines
//! (`docs/engine.md`). Nothing here reaches a real network or a real
//! credential: the provider is a hand-rolled HTTP/1.1 server on
//! `127.0.0.1:0`, and every folder, data folder and settings root is a
//! scratch directory of the test's own.
#![allow(dead_code)]

#[path = "sse.rs"]
mod sse;

use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

/// How long a check waits for something the engine should do in well under
/// a second. Generous, because a CI cell builds and runs everything at once.
pub const PATIENCE: Duration = Duration::from_secs(60);

pub fn scratch(label: &str) -> PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!(
        "sterna-engine-{label}-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&dir).unwrap();
    // A canonical path: macOS reaches the temp folder through a symlink, and
    // a session names its folder by the path it resolved.
    plain(&dir)
}

/// `path` resolved, and on Windows without the `\\?\` a resolved drive
/// path carries, so it is the path a person and a terminal use.
pub fn plain(path: &Path) -> PathBuf {
    let resolved = path.canonicalize().unwrap();
    #[cfg(windows)]
    {
        let text = resolved.to_string_lossy().into_owned();
        if let Some(rest) = text.strip_prefix(r"\\?\")
            && rest.chars().nth(1) == Some(':')
        {
            return PathBuf::from(rest);
        }
    }
    resolved
}

/// Whether two folders are one, however each is spelled.
pub fn same_path(a: &Path, b: &Path) -> bool {
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => a == b,
    }
}

/// The sessions a folder's own record holds: the names of the rollout files
/// under `.sterna/sessions`, leaving out the event logs beside them.
pub fn rollout_ids(folder: &Path) -> Vec<String> {
    // contract: a session's id in the list is its record file's name here,
    // the id `sterna --sessions` prints and `--resume` takes, whether the
    // terminal or the host started it.
    let mut ids: Vec<String> = std::fs::read_dir(folder.join(".sterna").join("sessions"))
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name().to_string_lossy().into_owned();
            let id = name.strip_suffix(".jsonl")?;
            (!id.contains('.')).then(|| id.to_string())
        })
        .collect();
    ids.sort();
    ids
}

/// How many replies the model has given since the last user message that
/// holds `words`; `None` when no user message holds them. What a provider
/// answers by when a session keeps one conversation across many messages.
pub fn replies_since(request: &Value, words: &str) -> Option<usize> {
    let messages = request["messages"].as_array()?;
    let at = messages
        .iter()
        .rposition(|m| m["role"] == "user" && text_of(&m["content"]).contains(words))?;
    Some(
        messages[at..]
            .iter()
            .filter(|m| m["role"] == "assistant")
            .count(),
    )
}

/// One request the provider was sent: its path and its body.
#[derive(Clone, Debug)]
pub struct Seen {
    pub path: String,
    pub body: Value,
}

/// A Messages endpoint that answers every request, on its own thread, for
/// as long as the test runs: `answer` sees the request and says what to send
/// back, and after how long.
pub struct Provider {
    pub url: String,
    pub seen: Arc<Mutex<Vec<Seen>>>,
}

impl Provider {
    pub fn start<F>(answer: F) -> Self
    where
        F: Fn(&Value) -> String + Send + Sync + 'static,
    {
        Self::paced(move |request| (Duration::ZERO, answer(request)))
    }

    /// The same, with a wait before each answer: a turn that is still
    /// running when the test acts.
    pub fn paced<F>(answer: F) -> Self
    where
        F: Fn(&Value) -> (Duration, String) + Send + Sync + 'static,
    {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let seen = Arc::new(Mutex::new(Vec::new()));
        let answer = Arc::new(answer);
        let record = Arc::clone(&seen);
        thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(stream) = stream else { return };
                let answer = Arc::clone(&answer);
                let record = Arc::clone(&record);
                thread::spawn(move || serve_one(stream, &*answer, &record));
            }
        });
        Self { url, seen }
    }

    /// Every model request so far (the `messages` route), in arrival order.
    pub fn requests(&self) -> Vec<Seen> {
        self.seen
            .lock()
            .unwrap()
            .iter()
            .filter(|seen| seen.path.contains("messages"))
            .cloned()
            .collect()
    }
}

fn serve_one(
    mut stream: TcpStream,
    answer: &(dyn Fn(&Value) -> (Duration, String) + Send + Sync),
    seen: &Mutex<Vec<Seen>>,
) {
    let mut reader = BufReader::new(stream.try_clone().unwrap());
    let mut request_line = String::new();
    if reader.read_line(&mut request_line).unwrap_or(0) == 0 {
        return;
    }
    let path = request_line
        .split_whitespace()
        .nth(1)
        .unwrap_or("")
        .to_string();
    let mut length = 0usize;
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).unwrap_or(0) == 0 {
            return;
        }
        if line.trim().is_empty() {
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
    let request: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    seen.lock().unwrap().push(Seen {
        path: path.clone(),
        body: request.clone(),
    });
    let (wait, whole) = if path.contains("messages") {
        answer(&request)
    } else {
        (Duration::ZERO, json!({"data": []}).to_string())
    };
    thread::sleep(wait);
    let (content_type, reply) = sse::response_for(&request, &whole);
    let head = format!(
        "HTTP/1.1 200 OK\r\ncontent-type: {content_type}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
        reply.len()
    );
    let _ = stream.write_all(head.as_bytes());
    let _ = stream.write_all(reply.as_bytes());
    let _ = stream.flush();
}

/// The text of a request's first user message: which task it serves.
pub fn task_of(request: &Value) -> String {
    request["messages"]
        .as_array()
        .and_then(|messages| messages.iter().find(|m| m["role"] == "user"))
        .map(|message| text_of(&message["content"]))
        .unwrap_or_default()
}

/// How many replies the model already gave in this request's conversation:
/// zero on a task's first request.
pub fn turn_of(request: &Value) -> usize {
    request["messages"].as_array().map_or(0, |messages| {
        messages.iter().filter(|m| m["role"] == "assistant").count()
    })
}

fn text_of(content: &Value) -> String {
    match content {
        Value::String(text) => text.clone(),
        Value::Array(blocks) => blocks
            .iter()
            .filter_map(|block| block["text"].as_str())
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

/// A reply that runs `code` as one cell.
pub fn cell(id: &str, code: &str) -> String {
    used(
        json!({
            "role": "assistant",
            "content": [{"type":"tool_use","id":id,"name":"execute_cell","input":{"code":code}}],
        }),
        10,
        5,
    )
}

/// A reply that ends the task with `text` as its answer.
pub fn ending(text: &str) -> String {
    cell(
        "end",
        &format!("answer({});", serde_json::to_string(text).unwrap()),
    )
}

/// A reply of prose and then a cell, so a client sees prose arrive before the
/// cell is written.
pub fn prose_then_cell(prose: &str, id: &str, code: &str) -> String {
    used(
        json!({
            "role": "assistant",
            "content": [
                {"type":"text","text":prose},
                {"type":"tool_use","id":id,"name":"execute_cell","input":{"code":code}},
            ],
        }),
        10,
        5,
    )
}

/// `reply` with its usage set: what the provider says the request cost.
pub fn with_usage(reply: &str, input: u64, output: u64) -> String {
    let mut value: Value = serde_json::from_str(reply).unwrap();
    value["usage"] = json!({"input_tokens": input, "output_tokens": output});
    value.to_string()
}

fn used(mut value: Value, input: u64, output: u64) -> String {
    value["usage"] = json!({"input_tokens": input, "output_tokens": output});
    value.to_string()
}

/// One check's world: its scratch root, the user's data folder and settings
/// root inside it, and the provider every session it starts talks to.
pub struct World {
    pub base: PathBuf,
    pub provider: String,
}

impl World {
    pub fn new(label: &str, provider: &Provider) -> Self {
        Self {
            base: scratch(label),
            provider: provider.url.clone(),
        }
    }

    /// The engine's data folder, `$XDG_DATA_HOME/sterna`.
    pub fn data(&self) -> PathBuf {
        self.base.join("data").join("sterna")
    }

    /// A project folder, with the fixture model as its model.
    pub fn folder(&self, name: &str) -> PathBuf {
        let root = self.base.join(name);
        std::fs::create_dir_all(root.join(".sterna")).unwrap();
        std::fs::write(
            root.join(".sterna/config.toml"),
            "[model]\nparent = \"fixture-model\"\n",
        )
        .unwrap();
        plain(&root)
    }

    /// `sterna` with this world's environment: its data folder, its settings
    /// root, its provider, and no credential or installed gateway of the
    /// developer's.
    pub fn sterna(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_sterna"));
        self.environ(&mut command);
        command
    }

    pub fn environ(&self, command: &mut Command) {
        command
            .env("XDG_DATA_HOME", self.base.join("data"))
            .env("XDG_CONFIG_HOME", self.base.join("global-config"))
            .env("ANTHROPIC_BASE_URL", &self.provider)
            .env("INFERENCE_GATEWAY_BIN", self.base.join("no-gateway"))
            .env(
                "INFERENCE_GATEWAY_CONFIG",
                self.base.join("gateway-config").join("gateway.toml"),
            )
            .env("INFERENCE_GATEWAY_DATA_DIR", self.base.join("gateway-data"))
            .env_remove("ANTHROPIC_API_KEY")
            .env_remove("ANTHROPIC_AUTH_TOKEN")
            .env_remove("COLORTERM");
    }

    /// The variables [`World::environ`] sets, for a launcher that is not a
    /// `Command` (a pseudo-terminal's).
    pub fn variables(&self) -> Vec<(String, String)> {
        let path = |p: PathBuf| p.to_string_lossy().into_owned();
        vec![
            ("XDG_DATA_HOME".into(), path(self.base.join("data"))),
            (
                "XDG_CONFIG_HOME".into(),
                path(self.base.join("global-config")),
            ),
            ("ANTHROPIC_BASE_URL".into(), self.provider.clone()),
            (
                "INFERENCE_GATEWAY_BIN".into(),
                path(self.base.join("no-gateway")),
            ),
            (
                "INFERENCE_GATEWAY_CONFIG".into(),
                path(self.base.join("gateway-config").join("gateway.toml")),
            ),
            (
                "INFERENCE_GATEWAY_DATA_DIR".into(),
                path(self.base.join("gateway-data")),
            ),
        ]
    }

    /// Starts a host in this world and waits for its ready line.
    pub fn host(&self) -> Host {
        let mut command = self.sterna();
        command
            .arg("host")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit());
        let mut child = command.spawn().expect("sterna host starts");
        let stdout = child.stdout.take().unwrap();
        let (lines, ready) = std::sync::mpsc::channel();
        thread::spawn(move || {
            let mut reader = BufReader::new(stdout);
            let mut line = String::new();
            let _ = reader.read_line(&mut line);
            let _ = lines.send(line);
            // Drain the rest, so a host that prints never blocks on a full pipe.
            let mut rest = String::new();
            while reader.read_line(&mut rest).unwrap_or(0) > 0 {
                rest.clear();
            }
        });
        let line = ready
            .recv_timeout(PATIENCE)
            .expect("sterna host printed its ready line");
        let ready: Value = serde_json::from_str(line.trim())
            .unwrap_or_else(|_| panic!("the host's ready line is JSON: {line:?}"));
        Host {
            child: Some(child),
            listening: ready["listening"].as_str().unwrap().to_string(),
            token: ready["token"].as_str().unwrap().to_string(),
            ready,
        }
    }

    /// The `live/<id>.json` entries of every session running in this world.
    pub fn live(&self) -> Vec<Value> {
        let mut entries: Vec<Value> = std::fs::read_dir(self.data().join("live"))
            .into_iter()
            .flatten()
            .flatten()
            .filter(|entry| {
                let name = entry.file_name().to_string_lossy().into_owned();
                name.ends_with(".json") && !name.starts_with('.')
            })
            .filter_map(|entry| std::fs::read_to_string(entry.path()).ok())
            .filter_map(|text| serde_json::from_str(&text).ok())
            .collect();
        entries.sort_by_key(|entry| entry["started"].as_u64().unwrap_or(0));
        entries
    }

    /// Waits until a running session's entry appears, and returns it.
    pub fn wait_live(&self, count: usize) -> Vec<Value> {
        let deadline = Instant::now() + PATIENCE;
        loop {
            let live = self.live();
            if live.len() >= count {
                return live;
            }
            assert!(
                Instant::now() < deadline,
                "{count} running session(s) never appeared in {}",
                self.data().join("live").display()
            );
            thread::sleep(Duration::from_millis(50));
        }
    }
}

/// A host this test started. Dropping it ends it, by its own handle.
pub struct Host {
    child: Option<Child>,
    pub listening: String,
    pub token: String,
    pub ready: Value,
}

impl Host {
    pub fn connect(&self, client: &str) -> Conn {
        Conn::open(&self.listening, &self.token, client)
    }

    /// One command on a connection of its own, and its answer's `ok` value;
    /// an `error` answer fails the check with what the host said.
    pub fn ask(&self, command: Value) -> Value {
        let mut conn = self.connect("check");
        let answer = conn.request(command.clone());
        match answer.get("ok") {
            Some(ok) => ok.clone(),
            None => panic!("the host refused {command}: {answer}"),
        }
    }

    /// Asks the host to end, and waits for it to exit.
    pub fn shutdown(mut self) {
        let _ = Conn::try_open(&self.listening, &self.token, "check")
            .map(|mut conn| conn.request(json!({"do":"shutdown"})));
        if let Some(mut child) = self.child.take() {
            let deadline = Instant::now() + PATIENCE;
            while child.try_wait().ok().flatten().is_none() {
                if Instant::now() > deadline {
                    let _ = child.kill();
                    panic!("the host did not exit after shutdown");
                }
                thread::sleep(Duration::from_millis(50));
            }
        }
    }

    /// The host process ends without being asked: what a crash looks like.
    pub fn kill(mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

impl Drop for Host {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = Conn::try_open(&self.listening, &self.token, "check")
                .map(|mut conn| conn.request(json!({"do":"shutdown"})));
            thread::sleep(Duration::from_millis(200));
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

/// One client connection, past its hello.
pub struct Conn {
    reader: BufReader<TcpStream>,
    writer: TcpStream,
    pub welcome: Value,
}

impl Conn {
    pub fn open(listening: &str, token: &str, client: &str) -> Self {
        Self::try_open(listening, token, client)
            .unwrap_or_else(|answer| panic!("the port at {listening} refused {client}: {answer}"))
    }

    /// Connects and says hello; `Err` is the refusal, or why there was none.
    pub fn try_open(listening: &str, token: &str, client: &str) -> Result<Self, Value> {
        let stream = TcpStream::connect(listening).map_err(|error| json!(error.to_string()))?;
        let mut conn = Self {
            reader: BufReader::new(stream.try_clone().unwrap()),
            writer: stream,
            welcome: Value::Null,
        };
        conn.send(json!({"hello":{"token":token,"protocol":1,"client":client}}));
        let answer = conn
            .recv(PATIENCE)
            .ok_or_else(|| json!("no answer to the hello"))?;
        if answer.get("welcome").is_none() {
            return Err(answer);
        }
        conn.welcome = answer;
        Ok(conn)
    }

    /// Connects to the session a live entry names.
    pub fn session(entry: &Value, client: &str) -> Self {
        Self::open(
            entry["listening"].as_str().unwrap(),
            entry["token"].as_str().unwrap(),
            client,
        )
    }

    pub fn send(&mut self, value: Value) {
        let mut line = value.to_string();
        line.push('\n');
        self.writer.write_all(line.as_bytes()).unwrap();
        self.writer.flush().unwrap();
    }

    /// The next line, or `None` when nothing arrived in `timeout` or the
    /// connection closed.
    pub fn recv(&mut self, timeout: Duration) -> Option<Value> {
        self.reader
            .get_ref()
            .set_read_timeout(Some(timeout))
            .unwrap();
        let mut line = String::new();
        match self.reader.read_line(&mut line) {
            Ok(0) | Err(_) => None,
            Ok(_) => Some(
                serde_json::from_str(line.trim())
                    .unwrap_or_else(|_| panic!("every line on the seam is JSON: {line:?}")),
            ),
        }
    }

    /// One command and the one line that answers it.
    pub fn request(&mut self, value: Value) -> Value {
        self.send(value.clone());
        self.recv(PATIENCE)
            .unwrap_or_else(|| panic!("no answer to {value}"))
    }

    /// Reads events until one satisfies `wanted`; returns every event read,
    /// the matching one last. Fails the check with the kinds it did see.
    pub fn until(&mut self, wanted: impl Fn(&Value) -> bool) -> Vec<Value> {
        let deadline = Instant::now() + PATIENCE;
        let mut seen = Vec::new();
        while Instant::now() < deadline {
            let left = deadline
                .saturating_duration_since(Instant::now())
                .max(Duration::from_millis(1));
            let Some(event) = self.recv(left) else {
                break;
            };
            let done = wanted(&event);
            seen.push(event);
            if done {
                return seen;
            }
        }
        let kinds: Vec<String> = seen
            .iter()
            .map(|event| event["kind"].as_str().unwrap_or("?").to_string())
            .collect();
        panic!("the awaited event never came; saw {kinds:?}");
    }

    /// Attaches and returns the snapshot.
    pub fn attach(&mut self) -> Value {
        self.send(json!({"do":"attach"}));
        let events = self.until(|event| event["kind"] == "snapshot");
        events.last().unwrap()["state"].clone()
    }

    /// Attaches from `seq`, and returns every event up to and including the
    /// first one `wanted` matches.
    pub fn attach_from(&mut self, seq: u64, wanted: impl Fn(&Value) -> bool) -> Vec<Value> {
        self.send(json!({"do":"attach","from":seq}));
        self.until(wanted)
    }

    /// Reads until the turn under way ends, and returns every event read.
    pub fn until_turn_ends(&mut self) -> Vec<Value> {
        self.until(turn_ended)
    }
}

/// Whether `event` says a turn ended.
pub fn turn_ended(event: &Value) -> bool {
    event["kind"] == "activity"
        && matches!(
            event["activity"].as_str(),
            Some("complete" | "failed" | "stopped" | "interrupted")
        )
}

/// The `kind` of every event, in order.
pub fn kinds(events: &[Value]) -> Vec<String> {
    events
        .iter()
        .map(|event| event["kind"].as_str().unwrap_or("?").to_string())
        .collect()
}

/// Every file under `root`, relative to it, leaving out Sterna's own folder.
pub fn files_under(root: &Path) -> Vec<String> {
    fn walk(base: &Path, dir: &Path, out: &mut Vec<String>) {
        for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
            let path = entry.path();
            let relative = path
                .strip_prefix(base)
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/");
            if relative == ".sterna" {
                continue;
            }
            if path.is_dir() {
                walk(base, &path, out);
            } else {
                out.push(relative);
            }
        }
    }
    let mut out = Vec::new();
    walk(root, root, &mut out);
    out.sort();
    out
}
