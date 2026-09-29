//! Binary-level proof that incomplete provider output cannot end a task
//! successfully, and that a turn cut off before anything ran, or refused for
//! the size it asked, is asked again with room that fits.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::process::Command;
use std::thread::JoinHandle;

/// A provider that answers each request in turn with one of `replies`
/// (status line, JSON body) and hands back the request bodies it received.
fn provider(replies: Vec<(&'static str, String)>) -> (String, JoinHandle<Vec<serde_json::Value>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base_url = format!("http://{}", listener.local_addr().unwrap());
    listener.set_nonblocking(true).unwrap();
    let handle = std::thread::spawn(move || {
        let mut bodies = Vec::new();
        for (status, reply) in replies {
            // A request that never comes -- a retry the session did not make
            // -- ends the script, so the test fails on what arrived instead
            // of waiting forever.
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
            let mut stream = loop {
                match listener.accept() {
                    Ok((stream, _)) => break stream,
                    Err(_) if std::time::Instant::now() < deadline => {
                        std::thread::sleep(std::time::Duration::from_millis(20));
                    }
                    Err(_) => return bodies,
                }
            };
            stream.set_nonblocking(false).unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut length = 0;
            loop {
                let mut line = String::new();
                assert_ne!(reader.read_line(&mut line).unwrap(), 0);
                if line == "\r\n" {
                    break;
                }
                if let Some((name, value)) = line.split_once(':')
                    && name.eq_ignore_ascii_case("content-length")
                {
                    length = value.trim().parse::<usize>().unwrap();
                }
            }
            let mut body = vec![0; length];
            reader.read_exact(&mut body).unwrap();
            bodies.push(serde_json::from_slice(&body).unwrap());
            write!(stream, "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", reply.len(), reply).unwrap();
        }
        // The endpoint closes after its last reply: a retry beyond the
        // scripted ones cannot reach a provider at all.
        bodies
    });
    (base_url, handle)
}

fn session(root: &std::path::Path, base_url: &str) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_sterna"))
        .args(["session", "--root"])
        .arg(root)
        .args(["--task", "Do the requested work", "--model", "fixture"])
        .env("ANTHROPIC_BASE_URL", base_url)
        .env("XDG_CONFIG_HOME", root.join("global-config"))
        .env_remove("ANTHROPIC_API_KEY")
        .env_remove("ANTHROPIC_AUTH_TOKEN")
        .output()
        .unwrap()
}

fn scratch(name: &str) -> std::path::PathBuf {
    let root = std::env::temp_dir().join(format!(
        "sterna-truthful-completion-{}-{name}",
        std::process::id()
    ));
    std::fs::create_dir_all(&root).unwrap();
    root
}

/// A reply cut off before its program finished is sent again once, with four
/// times the room (a model that publishes no limit starts at 32,768); cut off
/// again at the most room there is, the task fails with nothing run.
#[test]
fn max_token_prose_and_native_calls_fail_without_execution_after_one_larger_retry() {
    for with_call in [false, true] {
        let root = scratch(&with_call.to_string());
        let mut content = vec![serde_json::json!({
            "type": "text", "text": "I have started and will now",
        })];
        if with_call {
            content.push(serde_json::json!({
                "type": "tool_use", "id": "partial-call", "name": "execute_cell",
                "input": {"code": "await bash({command: 'printf unsafe > marker'}); answer('done');"},
            }));
        }
        let response = serde_json::json!({
            "role": "assistant", "stop_reason": "max_tokens", "content": content,
        })
        .to_string();
        let (base_url, served) = provider(vec![("200 OK", response.clone()), ("200 OK", response)]);
        let output = session(&root, &base_url);
        let bodies = served.join().unwrap();

        let asked: Vec<_> = bodies
            .iter()
            .map(|body| body["max_tokens"].clone())
            .collect();
        assert_eq!(
            asked,
            [32_768, 131_072],
            "one retry, with four times the room"
        );
        assert!(!output.status.success());
        let diagnostic = String::from_utf8_lossy(&output.stderr);
        assert!(diagnostic.contains("response incomplete"), "{diagnostic}");
        assert!(
            diagnostic.contains("I have started and will now"),
            "{diagnostic}"
        );
        assert!(!root.join("marker").exists());
        for entry in std::fs::read_dir(root.join(".sterna")).unwrap() {
            let path = entry.unwrap().path();
            if path
                .extension()
                .is_some_and(|extension| extension == "jsonl")
            {
                for line in std::fs::read_to_string(path).unwrap().lines() {
                    let row: serde_json::Value = serde_json::from_str(line).unwrap();
                    assert_ne!(row["kind"], "cell", "{row}");
                    assert_ne!(row["role"], "assistant", "{row}");
                }
            }
        }
        std::fs::remove_dir_all(root).unwrap();
    }
}

/// A provider that refuses the size asked for is asked for 8,192 instead,
/// and the task goes on.
#[test]
fn a_provider_refusing_the_output_size_is_asked_for_less() {
    let root = scratch("refused");
    let refusal = serde_json::json!({
        "type": "error",
        "error": {"type": "invalid_request_error", "message": "max_tokens: 32768 > 8192, which is the maximum allowed number of output tokens for fixture"},
    })
    .to_string();
    let answer = serde_json::json!({
        "role": "assistant", "stop_reason": "end_turn",
        "content": [{"type": "text", "text": "Nothing needed changing."}],
    })
    .to_string();
    let (base_url, served) = provider(vec![("400 Bad Request", refusal), ("200 OK", answer)]);
    let output = session(&root, &base_url);
    let bodies = served.join().unwrap();

    let asked: Vec<_> = bodies
        .iter()
        .map(|body| body["max_tokens"].clone())
        .collect();
    assert_eq!(asked, [32_768, 8_192]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    std::fs::remove_dir_all(root).unwrap();
}
