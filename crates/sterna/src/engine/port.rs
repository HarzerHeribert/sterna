//! The session port: loopback TCP, one JSON object per line, a token in the
//! client's first line (`docs/engine.md`). The same on every platform, and
//! the gateway's own pattern: an ephemeral port on 127.0.0.1 and a secret
//! only the user's own files hold.

use std::io::{BufRead, BufReader, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::Arc;
use std::time::Duration;

use super::hub::{Hub, Out};
use super::wire::{self, Command, Event, HelloLine};

/// The longest line a client may send: a hello, a message, a pasted key.
const MAX_LINE: usize = 4 * 1024 * 1024;

/// A port that is listening, and the token a client must say.
#[derive(Clone, Debug)]
pub(crate) struct Port {
    pub(crate) listening: SocketAddr,
    pub(crate) token: String,
}

/// Opens the port for `session` and serves every client that says the token.
pub(crate) fn open(hub: &Hub, session: &str) -> std::io::Result<Port> {
    let listener = TcpListener::bind("127.0.0.1:0")?;
    let port = Port {
        listening: listener.local_addr()?,
        token: super::data::token(),
    };
    let token = Arc::new(port.token.clone());
    let session = Arc::new(session.to_string());
    let hub = hub.clone();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(stream) = stream else { continue };
            let (hub, token, session) = (hub.clone(), Arc::clone(&token), Arc::clone(&session));
            std::thread::spawn(move || serve(stream, &hub, &token, &session));
        }
    });
    Ok(port)
}

/// Whether `given` is `token`, in time that does not depend on where they
/// differ.
pub(crate) fn same_token(given: &str, token: &str) -> bool {
    let (a, b) = (given.as_bytes(), token.as_bytes());
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// Reads one line of at most [`MAX_LINE`] bytes; `None` at the end.
pub(crate) fn read_line(reader: &mut impl BufRead) -> Option<String> {
    let mut line = Vec::new();
    let read = std::io::Read::take(&mut *reader, MAX_LINE as u64 + 1)
        .read_until(b'\n', &mut line)
        .ok()?;
    if read == 0 || line.len() > MAX_LINE {
        return None;
    }
    String::from_utf8(line).ok()
}

/// One client: the hello, then its commands in and the session's lines out.
fn serve(stream: TcpStream, hub: &Hub, token: &str, session: &str) {
    let _ = stream.set_nodelay(true);
    let _ = stream.set_read_timeout(Some(Duration::from_secs(10)));
    let Ok(mut writer) = stream.try_clone() else {
        return;
    };
    let mut reader = BufReader::new(stream);
    let refuse = |writer: &mut TcpStream, reason: &str| {
        let line = serde_json::json!({"refused": {"reason": reason}});
        let _ = writeln!(writer, "{line}");
    };
    let Some(first) = read_line(&mut reader) else {
        return;
    };
    let hello = match serde_json::from_str::<HelloLine>(first.trim()) {
        Ok(line) => line.hello,
        Err(_) => return refuse(&mut writer, "the first line must be a hello"),
    };
    if hello.protocol != wire::PROTOCOL {
        return refuse(
            &mut writer,
            &format!("this session speaks protocol {}", wire::PROTOCOL),
        );
    }
    if !same_token(&hello.token, token) {
        return refuse(&mut writer, "wrong token");
    }
    let welcome = serde_json::json!({"welcome": {"protocol": wire::PROTOCOL, "session": session}});
    if writeln!(writer, "{welcome}").is_err() {
        return;
    }
    let _ = reader.get_ref().set_read_timeout(None);

    let joined = super::client::join(hub, hello.client.trim());
    let client = joined.link.clone();
    let events = joined.events;
    std::thread::spawn(move || {
        for out in events {
            let line = match out {
                Out::Event(envelope) => match serde_json::to_string(&*envelope) {
                    Ok(line) => line,
                    Err(_) => continue,
                },
                Out::Close => break,
            };
            if writeln!(writer, "{line}")
                .and_then(|()| writer.flush())
                .is_err()
            {
                break;
            }
        }
        let _ = writer.shutdown(std::net::Shutdown::Both);
    });
    while let Some(line) = read_line(&mut reader) {
        if line.trim().is_empty() {
            continue;
        }
        match serde_json::from_str::<Command>(line.trim()) {
            Ok(command) => {
                if !client.send(command) {
                    break;
                }
            }
            Err(error) => {
                let to = serde_json::from_str::<serde_json::Value>(line.trim())
                    .ok()
                    .and_then(|v| v["do"].as_str().map(str::to_string))
                    .unwrap_or_default();
                client.refused(&to, &format!("not a command this session takes: {error}"));
            }
        }
    }
    client.leave();
}

impl super::client::Link {
    /// Says to this client alone that a line it sent was not taken.
    pub(crate) fn refused(&self, to: &str, reason: &str) {
        self.tell(Event::Refused {
            to: to.into(),
            reason: reason.into(),
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_token_matches_only_itself() {
        assert!(same_token("abc123", "abc123"));
        assert!(!same_token("abc124", "abc123"));
        assert!(!same_token("abc12", "abc123"));
        assert!(!same_token("", "abc123"));
    }

    #[test]
    fn a_line_longer_than_the_limit_ends_the_reading() {
        let long = vec![b'x'; MAX_LINE + 10];
        let mut reader = std::io::Cursor::new(long);
        assert_eq!(read_line(&mut reader), None);
        let mut reader = std::io::Cursor::new(b"{\"do\":\"end\"}\n".to_vec());
        assert_eq!(
            read_line(&mut reader).as_deref(),
            Some("{\"do\":\"end\"}\n")
        );
    }
}
