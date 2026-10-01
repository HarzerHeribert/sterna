//! Lines to and from the engine's loopback port.
//!
//! The engine speaks newline-delimited JSON over TCP on 127.0.0.1 (the host
//! and every session it starts). The UI cannot open a socket itself (its CSP
//! allows no network at all), so it asks for a connection here and gets an
//! id back. Each connection has two threads:
//!
//! - a reader, which emits every line it receives as the app event
//!   `engine-line` `{"id", "line"}`, and `engine-closed` `{"id"}` once,
//!   when the connection ends for any reason;
//! - a writer, which writes the lines `conn_send` queued, in the order they
//!   were sent. `conn_send` only queues, so a slow engine never stalls the
//!   window; a write that fails or stalls past [`WRITE_WITHIN`] closes the
//!   connection, which the UI hears as `engine-closed`.
//!
//! **Only addresses the engine's host announced are reached.** `conn_open`
//! refuses any other: the host's own (`host_start`'s ready line) and each
//! session port a host answer names (`{"ok":{…"listening":…}}`, the answer to
//! `start` and `locate`), which the reader notes before the UI sees the line.

use std::collections::{HashMap, HashSet};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{Shutdown, SocketAddr, TcpStream};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use serde::Serialize;
use serde_json::Value;
use tauri::{AppHandle, Emitter, Manager, State};

/// One line the engine sent.
const LINE_EVENT: &str = "engine-line";
/// The connection ended.
const CLOSED_EVENT: &str = "engine-closed";
/// The longest line accepted from the engine (its event log's own bound); a
/// longer one closes the connection rather than growing without bound.
const LINE_CAP: usize = 64 * 1024 * 1024;
const CONNECT_WITHIN: Duration = Duration::from_secs(3);
/// How long one write may wait for the engine to read.
const WRITE_WITHIN: Duration = Duration::from_secs(10);
const UNKNOWN: &str = "no such connection";

/// Every open connection, by id. Ids count up from 1 and are never reused.
pub struct Connections {
    next: AtomicU64,
    open: Mutex<HashMap<u64, Connection>>,
}

impl Default for Connections {
    fn default() -> Self {
        Self {
            next: AtomicU64::new(1),
            open: Mutex::new(HashMap::new()),
        }
    }
}

impl Connections {
    fn lock(&self) -> MutexGuard<'_, HashMap<u64, Connection>> {
        self.open.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// The addresses the engine's host announced: the only ones `conn_open`
/// connects to.
#[derive(Default)]
pub struct Announced(Mutex<HashSet<SocketAddr>>);

impl Announced {
    fn lock(&self) -> MutexGuard<'_, HashSet<SocketAddr>> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Notes `address` (as `listening` gives it) if it is a loopback one.
    pub fn add(&self, address: &str) {
        if let Ok(to) = loopback(address) {
            self.lock().insert(to);
        }
    }
}

/// The writing side of one connection.
struct Connection {
    /// Lines for the writer thread. Dropping it ends the thread, after it
    /// wrote what was already queued.
    lines: Sender<String>,
}

#[derive(Clone, Serialize)]
struct Line<'a> {
    id: u64,
    line: &'a str,
}

#[derive(Clone, Serialize)]
struct Closed {
    id: u64,
}

/// Connects to `address` (`"127.0.0.1:53211"`), which must be on this
/// computer and announced by the engine's host, and returns the
/// connection's id.
#[tauri::command]
pub async fn conn_open(app: AppHandle, address: String) -> Result<u64, String> {
    let to = loopback(&address)?;
    if !app.state::<Announced>().lock().contains(&to) {
        return Err(format!(
            "{to} was not given out by the engine's host, so the app did not connect to it."
        ));
    }
    let stream = tauri::async_runtime::spawn_blocking(move || {
        TcpStream::connect_timeout(&to, CONNECT_WITHIN)
    })
    .await
    .map_err(|e| format!("Could not connect to the engine at {to}: {e}"))?
    .map_err(|e| format!("Could not connect to the engine at {to}: {e}"))?;
    let failed = |e: std::io::Error| format!("Could not set up the connection to {to}: {e}");
    stream.set_nodelay(true).map_err(failed)?;
    stream
        .set_write_timeout(Some(WRITE_WITHIN))
        .map_err(failed)?;
    let reading = stream.try_clone().map_err(failed)?;

    let (lines, queued) = mpsc::channel();
    let connections = app.state::<Connections>();
    let id = connections.next.fetch_add(1, Ordering::Relaxed);
    connections.lock().insert(id, Connection { lines });
    std::thread::spawn(move || write_lines(stream, &queued));
    let reader_app = app.clone();
    std::thread::spawn(move || read_lines(&reader_app, id, reading));
    Ok(id)
}

/// Queues `line` for the engine; the writer adds the newline.
#[tauri::command]
pub fn conn_send(connections: State<'_, Connections>, id: u64, line: String) -> Result<(), String> {
    if line.contains('\n') {
        return Err("A line for the engine cannot contain a line break.".into());
    }
    let open = connections.lock();
    let connection = open.get(&id).ok_or(UNKNOWN)?;
    connection.lines.send(line).map_err(|_| UNKNOWN.to_string())
}

/// Forgets the connection. Lines already queued are still written; then
/// both halves are shut down and `engine-closed` follows. Closing an id that
/// is already gone is not an error.
#[tauri::command]
pub fn conn_close(connections: State<'_, Connections>, id: u64) -> Result<(), String> {
    connections.lock().remove(&id);
    Ok(())
}

/// `address` as a socket address, if it is one on this computer.
fn loopback(address: &str) -> Result<SocketAddr, String> {
    let to: SocketAddr = address
        .trim()
        .parse()
        .map_err(|_| format!("{address} is not an address and port such as 127.0.0.1:53211."))?;
    if !to.ip().to_canonical().is_loopback() {
        return Err(format!(
            "{to} is not on this computer; the app connects only to loopback addresses."
        ));
    }
    Ok(to)
}

fn write_lines(mut stream: TcpStream, queued: &Receiver<String>) {
    for line in queued {
        let mut bytes = line.into_bytes();
        bytes.push(b'\n');
        if stream
            .write_all(&bytes)
            .and_then(|()| stream.flush())
            .is_err()
        {
            break;
        }
    }
    let _ = stream.shutdown(Shutdown::Both);
}

fn read_lines(app: &AppHandle, id: u64, stream: TcpStream) {
    let mut reader = BufReader::with_capacity(64 * 1024, stream);
    let mut line = Vec::new();
    let announced = app.state::<Announced>();
    while next_line(&mut reader, &mut line, LINE_CAP) {
        let text = String::from_utf8_lossy(&line);
        if text.contains("\"listening\"") {
            for address in announced_in(&text) {
                announced.add(&address);
            }
        }
        let _ = app.emit(LINE_EVENT, Line { id, line: &text });
    }
    app.state::<Connections>().lock().remove(&id);
    let _ = reader.get_ref().shutdown(Shutdown::Both);
    let _ = app.emit(CLOSED_EVENT, Closed { id });
}

/// Every `listening` address inside the `ok` of a host answer
/// (`{"ok":{"id","listening","token"}}`); nothing from any other line.
fn announced_in(line: &str) -> Vec<String> {
    fn collect(value: &Value, found: &mut Vec<String>) {
        match value {
            Value::Object(fields) => {
                for (key, value) in fields {
                    match (key.as_str(), value) {
                        ("listening", Value::String(address)) => found.push(address.clone()),
                        _ => collect(value, found),
                    }
                }
            }
            Value::Array(items) => items.iter().for_each(|item| collect(item, found)),
            _ => {}
        }
    }
    let mut found = Vec::new();
    if let Ok(Value::Object(answer)) = serde_json::from_str::<Value>(line)
        && let Some(ok) = answer.get("ok")
    {
        collect(ok, &mut found);
    }
    found
}

/// Reads the next line into `line`, without its `\n` (or `\r\n`); a last
/// line the stream ended without a newline counts too. False at the end of
/// the stream, on an error, and for a line longer than `cap` bytes.
fn next_line(reader: &mut impl BufRead, line: &mut Vec<u8>, cap: usize) -> bool {
    line.clear();
    let limit = u64::try_from(cap).map_or(u64::MAX, |cap| cap.saturating_add(1));
    match reader.take(limit).read_until(b'\n', line) {
        Ok(0) | Err(_) => return false,
        Ok(_) => {}
    }
    if line.last() == Some(&b'\n') {
        line.pop();
        if line.last() == Some(&b'\r') {
            line.pop();
        }
        true
    } else {
        line.len() <= cap
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(input: &str, cap: usize) -> Vec<String> {
        let mut reader = BufReader::new(input.as_bytes());
        let mut line = Vec::new();
        let mut read = Vec::new();
        while next_line(&mut reader, &mut line, cap) {
            read.push(String::from_utf8(line.clone()).unwrap());
        }
        read
    }

    #[test]
    fn lines_lose_their_line_endings() {
        assert_eq!(lines("a\nb\r\n\nlast", 100), ["a", "b", "", "last"]);
    }

    #[test]
    fn a_line_over_the_cap_ends_the_stream() {
        assert_eq!(lines("abcd\nabcde\nnever\n", 4), ["abcd"]);
        assert_eq!(lines("abcde", 4), Vec::<String>::new());
    }

    #[test]
    fn host_answers_announce_their_listening_addresses() {
        let start = r#"{"ok":{"id":"s1","listening":"127.0.0.1:5000","token":"t"}}"#;
        assert_eq!(announced_in(start), ["127.0.0.1:5000"]);
        let nested = r#"{"ok":{"sessions":[{"listening":"127.0.0.1:1"},{"listening":"[::1]:2"}]}}"#;
        assert_eq!(announced_in(nested), ["127.0.0.1:1", "[::1]:2"]);
        for other in [
            r#"{"seq":3,"kind":"notice","text":"\"listening\":\"127.0.0.1:9\""}"#,
            r#"{"error":"no","listening":"127.0.0.1:9"}"#,
            r#"{"listening":"127.0.0.1:9"}"#,
            r#"{"ok":{"listening":9}}"#,
            r#"not json "listening""#,
        ] {
            assert!(announced_in(other).is_empty(), "{other}");
        }
    }

    #[test]
    fn only_announced_loopback_addresses_are_kept() {
        let announced = Announced::default();
        announced.add("127.0.0.1:5000");
        announced.add("10.0.0.1:5000");
        announced.add("nonsense");
        let kept: Vec<SocketAddr> = announced.lock().iter().copied().collect();
        assert_eq!(kept, ["127.0.0.1:5000".parse::<SocketAddr>().unwrap()]);
    }

    #[test]
    fn only_loopback_addresses_are_accepted() {
        for ok in [
            "127.0.0.1:53211",
            " 127.0.0.1:1 ",
            "[::1]:9",
            "[::ffff:127.0.0.1]:9",
        ] {
            assert!(loopback(ok).is_ok(), "{ok}");
        }
        for refused in ["10.0.0.1:80", "0.0.0.0:80", "[::]:80", "192.168.1.2:53211"] {
            let error = loopback(refused).unwrap_err();
            assert!(error.contains("is not on this computer"), "{error}");
        }
        let error = loopback("localhost:80").unwrap_err();
        assert!(error.contains("is not an address and port"), "{error}");
    }
}
