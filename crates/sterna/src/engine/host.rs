//! `sterna host`: the one process that keeps every folder and session in
//! view (`docs/engine.md`). It answers the list, starts each session it is
//! asked for as a process of its own (`sterna session --serve`), owns one
//! gateway those sessions share, adds up their usage, and keeps running
//! when a client quits and asks to keep its sessions.
//!
//! **A session the host starts dies with the host.** Its stdin is the
//! host's pipe, and a session started with `--serve` ends when that closes:
//! nothing is left running that nobody can find.

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command as Process, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use super::data::{self, Live};
use super::port::{read_line, same_token};
use super::wire::{self, Command, Envelope, Event};

/// How long a host with no session and no client waits before it leaves.
const IDLE: Duration = Duration::from_secs(300);

/// `sterna host [--background]`.
pub fn main(args: &[String]) -> Result<(), String> {
    let folder = data::folder().ok_or("sterna host: there is no data folder for this user")?;
    std::fs::create_dir_all(folder.join("logs")).map_err(|e| format!("sterna host: {e}"))?;
    if args.iter().any(|a| a == "--background") {
        return background(&folder);
    }
    if let Some(unknown) = args.first() {
        return Err(format!("sterna host: unknown option {unknown}"));
    }
    serve(&folder)
}

/// A host answering at `folder`'s `host.json`, if one does.
fn running(folder: &Path) -> Option<Value> {
    let text = std::fs::read_to_string(folder.join("host.json")).ok()?;
    let ready: Value = serde_json::from_str(&text).ok()?;
    let address = ready["listening"].as_str()?.parse().ok()?;
    TcpStream::connect_timeout(&address, Duration::from_millis(500)).ok()?;
    Some(ready)
}

/// Starts a host on its own, outside this process's group and console, if
/// none answers; prints the ready line of whichever host runs.
fn background(folder: &Path) -> Result<(), String> {
    if let Some(ready) = running(folder) {
        println!("{ready}");
        return Ok(());
    }
    let exe = std::env::current_exe().map_err(|e| format!("sterna host: {e}"))?;
    let mut command = Process::new(exe);
    command
        .arg("host")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(log(folder, "host")?);
    detach(&mut command);
    let mut child = command
        .spawn()
        .map_err(|e| format!("sterna host: could not start: {e}"))?;
    let stdout = child.stdout.take().ok_or("sterna host: no stdout")?;
    let mut line = String::new();
    BufReader::new(stdout)
        .read_line(&mut line)
        .map_err(|e| format!("sterna host: {e}"))?;
    let ready: Value = serde_json::from_str(line.trim())
        .map_err(|_| format!("sterna host: the host did not say it was ready: {line:?}"))?;
    println!("{ready}");
    Ok(())
}

/// A process that outlives the one that started it: its own session on
/// Unix, its own hidden console group on Windows.
pub(crate) fn detach(command: &mut Process) {
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    {
        use std::os::unix::process::CommandExt;
        // SAFETY: `setsid` is async-signal-safe and touches nothing of the
        // parent's; it is the only call made between fork and exec.
        unsafe {
            command.pre_exec(|| {
                libc::setsid();
                Ok(())
            });
        }
    }
    #[cfg(all(unix, not(any(target_os = "linux", target_os = "macos"))))]
    std::os::unix::process::CommandExt::process_group(command, 0);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NEW_PROCESS_GROUP | CREATE_NO_WINDOW);
    }
}

/// A log file in the data folder for a process with no terminal.
fn log(folder: &Path, name: &str) -> Result<std::fs::File, String> {
    let logs = folder.join("logs");
    std::fs::create_dir_all(&logs).map_err(|e| format!("sterna host: {e}"))?;
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(logs.join(format!("{name}.log")))
        .map_err(|e| format!("sterna host: {e}"))
}

/// A session this host started.
struct Started {
    root: String,
    process: Child,
    stdin: Option<ChildStdin>,
    listening: String,
    token: String,
}

/// What the host has heard from one session's port.
#[derive(Clone, Debug, Default)]
struct Heard {
    activity: String,
    since: u64,
    usage: wire::Usage,
    ended: bool,
}

#[derive(Default)]
struct Hosting {
    started: BTreeMap<String, Started>,
    /// In the order the host started them: what `usage` adds up.
    order: Vec<String>,
    heard: BTreeMap<String, Heard>,
    watching: std::collections::BTreeSet<String>,
    clients: usize,
    last_busy: Option<Instant>,
}

type Shared = Arc<Mutex<Hosting>>;

fn lock(shared: &Shared) -> std::sync::MutexGuard<'_, Hosting> {
    shared
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn serve(folder: &Path) -> Result<(), String> {
    let lock_file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(folder.join("host.lock"))
        .map_err(|e| format!("sterna host: {e}"))?;
    if lock_file.try_lock().is_err() {
        return Err("sterna host: a host is already running for this user".into());
    }
    let listener = TcpListener::bind("127.0.0.1:0").map_err(|e| format!("sterna host: {e}"))?;
    let token = data::token();
    let gateway = Gateway::start(folder)?;
    let ready = json!({
        "listening": listener.local_addr().map_err(|e| e.to_string())?.to_string(),
        "token": token,
        "pid": std::process::id(),
        "version": env!("CARGO_PKG_VERSION"),
        "protocol": wire::PROTOCOL,
    });
    data::write_private(&folder.join("host.json"), ready.to_string().as_bytes())
        .map_err(|e| format!("sterna host: {e}"))?;
    println!("{ready}");
    let _ = std::io::stdout().flush();

    let shared: Shared = Arc::new(Mutex::new(Hosting {
        last_busy: Some(Instant::now()),
        ..Hosting::default()
    }));
    let gateway = Arc::new(gateway);
    {
        let shared = Arc::clone(&shared);
        let folder = folder.to_path_buf();
        std::thread::spawn(move || keep_time(&shared, &folder));
    }
    for stream in listener.incoming() {
        let Ok(stream) = stream else {
            std::thread::sleep(Duration::from_millis(50));
            continue;
        };
        let (shared, token, gateway) = (Arc::clone(&shared), token.clone(), Arc::clone(&gateway));
        let folder = folder.to_path_buf();
        std::thread::spawn(move || client(stream, &token, &shared, &gateway, &folder));
    }
    drop(lock_file);
    Ok(())
}

/// Leaves when nothing has been running and nobody has been connected for
/// [`IDLE`]; reaps sessions that ended on their own.
fn keep_time(shared: &Shared, folder: &Path) {
    loop {
        std::thread::sleep(Duration::from_secs(1));
        let mut hosting = lock(shared);
        hosting
            .started
            .retain(|_, started| started.process.try_wait().ok().flatten().is_none());
        if !hosting.started.is_empty() || hosting.clients > 0 {
            hosting.last_busy = Some(Instant::now());
        } else if hosting.last_busy.is_some_and(|at| at.elapsed() > IDLE) {
            let _ = std::fs::remove_file(folder.join("host.json"));
            std::process::exit(0);
        }
    }
}

/// The gateway the host's sessions share: the one the environment names,
/// or one the host starts and owns.
struct Gateway {
    serving: Option<crate::gateway::Serving>,
}

impl Gateway {
    fn start(folder: &Path) -> Result<Self, String> {
        // The gateway installed beside this binary first: an app's bundle
        // holds both, and a desktop app is started with no PATH to find one.
        let gateway = crate::gateway::select(crate::gateway::installed().as_deref());
        let serving =
            crate::gateway::start_or_attach(&gateway, false, &folder.join("logs/gateway.log"))?;
        Ok(Self { serving })
    }

    /// What a session needs to reach it.
    fn environ(&self, command: &mut Process) {
        if let Some(serving) = &self.serving {
            command.env("ANTHROPIC_BASE_URL", serving.base_url());
            if let Some(token) = serving.token() {
                command.env("ANTHROPIC_AUTH_TOKEN", token);
            }
        }
    }
}

/// One client connection: its hello, then one answer per command.
fn client(stream: TcpStream, token: &str, shared: &Shared, gateway: &Gateway, folder: &Path) {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(10)));
    let Ok(mut writer) = stream.try_clone() else {
        return;
    };
    let mut reader = BufReader::new(stream);
    let Some(first) = read_line(&mut reader) else {
        return;
    };
    let refuse = |writer: &mut TcpStream, reason: &str| {
        let _ = writeln!(writer, "{}", json!({"refused": {"reason": reason}}));
    };
    let hello = match serde_json::from_str::<wire::HelloLine>(first.trim()) {
        Ok(line) => line.hello,
        Err(_) => return refuse(&mut writer, "the first line must be a hello"),
    };
    if hello.protocol != wire::PROTOCOL {
        return refuse(&mut writer, "this host speaks protocol 1");
    }
    if !same_token(&hello.token, token) {
        return refuse(&mut writer, "wrong token");
    }
    let welcome =
        json!({"welcome": {"protocol": wire::PROTOCOL, "host": env!("CARGO_PKG_VERSION")}});
    if writeln!(writer, "{welcome}").is_err() {
        return;
    }
    let _ = reader.get_ref().set_read_timeout(None);
    lock(shared).clients += 1;
    while let Some(line) = read_line(&mut reader) {
        if line.trim().is_empty() {
            continue;
        }
        let answer = match serde_json::from_str::<Value>(line.trim()) {
            Ok(command) if command["do"] == "watch" => {
                let _ = writeln!(writer, "{}", json!({"ok": {}}));
                watch(&mut writer, shared);
                break;
            }
            Ok(command) if command["do"] == "shutdown" => {
                let _ = writeln!(writer, "{}", json!({"ok": {}}));
                shut_down(shared, folder);
            }
            Ok(command) => answer(&command, shared, gateway, folder),
            Err(error) => Err(format!("not a command: {error}")),
        };
        let line = match answer {
            Ok(ok) => json!({ "ok": ok }),
            Err(error) => json!({ "error": error }),
        };
        if writeln!(writer, "{line}")
            .and_then(|()| writer.flush())
            .is_err()
        {
            break;
        }
    }
    lock(shared).clients -= 1;
}

fn answer(
    command: &Value,
    shared: &Shared,
    gateway: &Gateway,
    folder: &Path,
) -> Result<Value, String> {
    match command["do"].as_str().unwrap_or("") {
        "list" => Ok(list(shared)),
        "start" => start(command, shared, gateway, folder),
        "locate" => {
            let id = command["id"].as_str().ok_or("locate needs an id")?;
            let live = Live::all()
                .into_iter()
                .find(|live| live.id == id && live.answers())
                .ok_or_else(|| format!("session {id} is not running"))?;
            Ok(json!({"id": live.id, "listening": live.listening, "token": live.token}))
        }
        "stop" => {
            let id = command["id"].as_str().ok_or("stop needs an id")?;
            end_session(shared, id);
            Ok(json!({}))
        }
        "usage" => Ok(usage(shared)),
        "quit" => {
            if command["keep"].as_bool() != Some(true) {
                let ids: Vec<String> = lock(shared).started.keys().cloned().collect();
                for id in ids {
                    end_session(shared, &id);
                }
            }
            Ok(json!({}))
        }
        other => Err(format!("the host takes no command {other:?}")),
    }
}

/// Starts a session in `root` as a process of its own, and gives it its
/// task through its own port.
fn start(
    command: &Value,
    shared: &Shared,
    gateway: &Gateway,
    folder: &Path,
) -> Result<Value, String> {
    let root = PathBuf::from(command["root"].as_str().ok_or("start needs a root")?);
    if !root.is_absolute() || !root.is_dir() {
        return Err(format!("{} is not a folder", root.display()));
    }
    let root = data::plain(&root);
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let mut process = Process::new(exe);
    process
        .arg("session")
        .arg("--serve")
        .arg("--root")
        .arg(&root);
    if let Some(model) = command["model"].as_str() {
        process.arg("--model").arg(model);
    }
    gateway.environ(&mut process);
    process
        .current_dir(&root)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(log(folder, "sessions")?);
    #[cfg(unix)]
    std::os::unix::process::CommandExt::process_group(&mut process, 0);
    let mut child = process
        .spawn()
        .map_err(|e| format!("could not start a session: {e}"))?;
    let stdout = child.stdout.take().ok_or("no stdout")?;
    // The ready line is the first that says where the session listens; the
    // rest of its stdout is read and dropped, so it never blocks on a full
    // pipe.
    let (said, heard) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut reader = BufReader::new(stdout);
        let mut line = String::new();
        let mut ready = false;
        while reader.read_line(&mut line).unwrap_or(0) > 0 {
            if !ready
                && let Ok(value) = serde_json::from_str::<Value>(line.trim())
                && value["listening"].is_string()
            {
                ready = true;
                let _ = said.send(value);
            }
            line.clear();
        }
    });
    let Ok(ready) = heard.recv_timeout(Duration::from_secs(60)) else {
        let _ = child.kill();
        let _ = child.wait();
        return Err("the session did not start".into());
    };
    let id = ready["id"].as_str().unwrap_or_default().to_string();
    let listening = ready["listening"].as_str().unwrap_or_default().to_string();
    let token = ready["token"].as_str().unwrap_or_default().to_string();
    let stdin = child.stdin.take();
    {
        let mut hosting = lock(shared);
        hosting.started.insert(
            id.clone(),
            Started {
                root: root.to_string_lossy().into_owned(),
                process: child,
                stdin,
                listening: listening.clone(),
                token: token.clone(),
            },
        );
        hosting.order.push(id.clone());
    }
    let task = command["task"].as_str().map(str::to_string);
    watch_session(shared, &id, &listening, &token, task);
    Ok(json!({"id": id, "listening": listening, "token": token}))
}

/// Ends a running session through its own port, and closes the pipe it
/// would end on anyway.
fn end_session(shared: &Shared, id: &str) {
    let found = {
        let mut hosting = lock(shared);
        let stdin = hosting.started.get_mut(id).and_then(|s| s.stdin.take());
        drop(stdin);
        hosting
            .started
            .get(id)
            .map(|s| (s.listening.clone(), s.token.clone()))
    }
    .or_else(|| {
        Live::all()
            .into_iter()
            .find(|live| live.id == id)
            .map(|live| (live.listening, live.token))
    });
    if let Some((listening, token)) = found
        && let Ok(mut conn) = Line::open(&listening, &token, "host")
    {
        conn.send(&Command::End);
    }
}

fn shut_down(shared: &Shared, folder: &Path) -> ! {
    let ids: Vec<String> = lock(shared).started.keys().cloned().collect();
    for id in &ids {
        end_session(shared, id);
    }
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        let mut hosting = lock(shared);
        hosting
            .started
            .retain(|_, started| started.process.try_wait().ok().flatten().is_none());
        if hosting.started.is_empty() {
            break;
        }
        drop(hosting);
        std::thread::sleep(Duration::from_millis(50));
    }
    for started in lock(shared).started.values_mut() {
        let _ = started.process.kill();
    }
    let _ = std::fs::remove_file(folder.join("host.json"));
    std::process::exit(0);
}

/// Listens to one session's port for how it stands, and hands it `task`
/// once it is listening.
fn watch_session(shared: &Shared, id: &str, listening: &str, token: &str, task: Option<String>) {
    if !lock(shared).watching.insert(id.to_string()) {
        return;
    }
    let Ok(mut line) = Line::open(listening, token, "host") else {
        lock(shared).watching.remove(id);
        return;
    };
    // Where the session stands, then what changes: the host needs its
    // activity and usage, never its whole history.
    line.send(&Command::Attach { from: None });
    if let Some(text) = task {
        line.send(&Command::Submit {
            text,
            images: Vec::new(),
        });
    }
    let (shared, id) = (Arc::clone(shared), id.to_string());
    std::thread::spawn(move || {
        while let Some(envelope) = line.next() {
            let mut hosting = lock(&shared);
            let heard = hosting.heard.entry(id.clone()).or_default();
            match envelope.event {
                Event::Activity { activity, since } => {
                    heard.activity = live_word(activity).into();
                    heard.since = since;
                }
                Event::Snapshot { state } => {
                    heard.activity = live_word(state.activity).into();
                    heard.since = state.since;
                    heard.usage = state.usage;
                }
                Event::Usage { usage } => heard.usage = usage,
                Event::Ended { .. } => heard.ended = true,
                _ => {}
            }
        }
        let mut hosting = lock(&shared);
        hosting.heard.entry(id.clone()).or_default().ended = true;
        hosting.watching.remove(&id);
    });
}

/// How a running session stands, in the list's words.
fn live_word(activity: crate::tui::Activity) -> &'static str {
    use crate::tui::Activity;
    match activity {
        Activity::Thinking | Activity::Compacting | Activity::Waiting => "thinking",
        Activity::Streaming => "writing",
        Activity::Executing | Activity::Searching => "running",
        Activity::AwaitingYou => "waiting",
        _ => "idle",
    }
}

fn list(shared: &Shared) -> Value {
    let running: BTreeMap<String, Live> = Live::all()
        .into_iter()
        .filter(Live::answers)
        .map(|live| (live.id.clone(), live))
        .collect();
    for live in running.values() {
        watch_session(shared, &live.id, &live.listening, &live.token, None);
    }
    let hosting = lock(shared);
    let list = super::list::read();
    let folders: Vec<Value> = list
        .folders
        .iter()
        .map(|folder| {
            let sessions: Vec<Value> = folder
                .sessions
                .iter()
                .map(|session| {
                    let ended = hosting.heard.get(&session.id).is_some_and(|heard| heard.ended);
                    let live = running.get(&session.id).filter(|_| !ended).map(|_| {
                        let heard = hosting.heard.get(&session.id).cloned().unwrap_or_default();
                        json!({
                            "state": if heard.activity.is_empty() { "idle" } else { heard.activity.as_str() },
                            "since": heard.since,
                        })
                    });
                    json!({
                        "id": session.id,
                        "title": session.title,
                        "last_used": session.last_used,
                        "live": live,
                    })
                })
                .collect();
            json!({"root": folder.root, "last_used": folder.last_used, "sessions": sessions})
        })
        .collect();
    json!({ "folders": folders })
}

fn usage(shared: &Shared) -> Value {
    let hosting = lock(shared);
    let mut total = wire::Usage::default();
    let sessions: Vec<Value> = hosting
        .order
        .iter()
        .map(|id| {
            let usage = hosting.heard.get(id).map(|h| h.usage).unwrap_or_default();
            total.input_tokens += usage.input_tokens;
            total.output_tokens += usage.output_tokens;
            total.reasoned_tokens += usage.reasoned_tokens;
            total.requests += usage.requests;
            json!({
                "id": id,
                "root": hosting.started.get(id).map(|s| s.root.clone()),
                "input_tokens": usage.input_tokens,
                "output_tokens": usage.output_tokens,
                "requests": usage.requests,
            })
        })
        .collect();
    json!({
        "sessions": sessions,
        "total": {
            "input_tokens": total.input_tokens,
            "output_tokens": total.output_tokens,
            "requests": total.requests,
        },
    })
}

/// Sends the list, and again each time it changes, until the client goes.
fn watch(writer: &mut TcpStream, shared: &Shared) {
    // A client that has gone is noticed by its end of the connection
    // closing, whether or not the list changes.
    let gone = Arc::new(std::sync::atomic::AtomicBool::new(false));
    if let Ok(reading) = writer.try_clone() {
        let gone = Arc::clone(&gone);
        std::thread::spawn(move || {
            let mut reader = BufReader::new(reading);
            while read_line(&mut reader).is_some() {}
            gone.store(true, std::sync::atomic::Ordering::SeqCst);
        });
    }
    let mut last = Value::Null;
    loop {
        if gone.load(std::sync::atomic::Ordering::SeqCst) {
            return;
        }
        let now = list(shared);
        if now != last {
            if writeln!(writer, "{}", json!({ "list": now })).is_err() {
                return;
            }
            last = now;
        }
        std::thread::sleep(Duration::from_millis(300));
    }
}

/// A connection to a session's port, as the host holds one.
struct Line {
    reader: BufReader<TcpStream>,
    writer: TcpStream,
}

impl Line {
    fn open(listening: &str, token: &str, client: &str) -> Result<Self, String> {
        let address = listening.parse().map_err(|_| "no address".to_string())?;
        let stream = TcpStream::connect_timeout(&address, Duration::from_secs(2))
            .map_err(|e| e.to_string())?;
        let mut line = Self {
            reader: BufReader::new(stream.try_clone().map_err(|e| e.to_string())?),
            writer: stream,
        };
        let hello =
            json!({"hello": {"token": token, "protocol": wire::PROTOCOL, "client": client}});
        writeln!(line.writer, "{hello}").map_err(|e| e.to_string())?;
        // A port that takes the connection and never answers is not a
        // session: a stale entry whose port another program now holds.
        let _ = line
            .reader
            .get_ref()
            .set_read_timeout(Some(Duration::from_secs(5)));
        let welcome = read_line(&mut line.reader).ok_or("no welcome")?;
        if !welcome.contains("\"welcome\"") {
            return Err(welcome);
        }
        let _ = line.reader.get_ref().set_read_timeout(None);
        Ok(line)
    }

    fn send(&mut self, command: &Command) {
        if let Ok(text) = serde_json::to_string(command) {
            let _ = writeln!(self.writer, "{text}");
        }
    }

    fn next(&mut self) -> Option<Envelope> {
        loop {
            let line = read_line(&mut self.reader)?;
            if let Ok(envelope) = serde_json::from_str::<Envelope>(line.trim()) {
                return Some(envelope);
            }
        }
    }
}
