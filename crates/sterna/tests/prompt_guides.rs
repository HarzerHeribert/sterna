//! The prompting guides' candidates (2026-09-24), each behind a `[limits]`
//! switch that is off by default: the autonomy and scope blocks live in the
//! system prompt from the first request, and the batching nudge ends each
//! new result without editing an earlier one.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};

fn root(name: &str, limits: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "sterna-guides-{name}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(dir.join(".sterna")).unwrap();
    std::fs::write(dir.join(".sterna/config.toml"), limits).unwrap();
    dir
}

fn cell(id: &str, code: &str) -> String {
    serde_json::json!({"role": "assistant", "content": [
        {"type": "tool_use", "id": id, "name": "execute_cell", "input": {"code": code}}
    ]})
    .to_string()
}

/// Answers each request with the next reply and keeps every request body.
fn provider(replies: Vec<String>) -> (String, Arc<Mutex<Vec<serde_json::Value>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
    let bodies = Arc::new(Mutex::new(Vec::new()));
    let kept = Arc::clone(&bodies);
    std::thread::spawn(move || {
        for reply in replies {
            let Ok((mut stream, _)) = listener.accept() else {
                return;
            };
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut length = 0usize;
            loop {
                let mut line = String::new();
                if reader.read_line(&mut line).unwrap_or(0) == 0 || line.trim().is_empty() {
                    break;
                }
                if let Some(rest) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                    length = rest.trim().parse().unwrap_or(0);
                }
            }
            let mut body = vec![0u8; length];
            let _ = reader.read_exact(&mut body);
            kept.lock()
                .unwrap()
                .push(serde_json::from_slice(&body).unwrap_or_default());
            let response = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{reply}",
                reply.len()
            );
            let _ = stream.write_all(response.as_bytes());
        }
    });
    (base, bodies)
}

fn run(root: &Path, base: &str) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_sterna"))
        .args(["session", "--root"])
        .arg(root)
        .args(["--task", "work", "--model", sterna::wire::MODEL])
        .env("ANTHROPIC_BASE_URL", base)
        .env("XDG_CONFIG_HOME", root.join("global-config"))
        .env("STERNA_DISABLE_AUTOUPDATE", "1")
        .env_remove("ANTHROPIC_AUTH_TOKEN")
        .env_remove("ANTHROPIC_API_KEY")
        .output()
        .unwrap()
}

/// A message as the provider caches it: a `cache_control` marker says where a
/// breakpoint sits, moves to the newest message each request, and is not
/// part of the cached content.
fn content(message: &serde_json::Value) -> serde_json::Value {
    let mut message = message.clone();
    if let Some(blocks) = message["content"].as_array_mut() {
        for block in blocks {
            if let Some(block) = block.as_object_mut() {
                block.remove("cache_control");
            }
        }
    }
    message
}

fn system(body: &serde_json::Value) -> String {
    body["system"][0]["text"]
        .as_str()
        .unwrap_or_default()
        .to_string()
}

#[test]
fn the_autonomy_and_scope_blocks_are_in_the_system_prompt_only_when_switched_on() {
    for (on, limits) in [
        (false, ""),
        (
            true,
            "[limits]\nautonomy_block = true\nscope_block = true\n",
        ),
    ] {
        let root = root("blocks", limits);
        let (base, bodies) = provider(vec![cell("a", "answer(`done`);")]);
        let output = run(&root, &base);
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let system = system(&bodies.lock().unwrap()[0]);
        assert_eq!(
            system.contains("## Carrying the request through"),
            on,
            "{system}"
        );
        assert_eq!(
            system.contains("## Scope\n\nThe request sets the scope."),
            on,
            "{system}"
        );
        let _ = std::fs::remove_dir_all(root);
    }
}

/// The nudge ends each new result, and a later request still re-sends the
/// earlier ones exactly as they were: it is appended, never edited in.
#[test]
fn the_batching_nudge_ends_each_new_result_and_edits_nothing_already_sent() {
    let root = root("nudge", "[limits]\nbatch_nudge = true\n");
    let (base, bodies) = provider(vec![
        cell("a", "const x = 1; return x;"),
        cell("b", "const y = 2; return y;"),
        cell("c", "answer(`done`);"),
    ]);
    let output = run(&root, &base);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let bodies = bodies.lock().unwrap();
    assert_eq!(bodies.len(), 3);
    for pair in bodies.windows(2) {
        let (earlier, later) = (
            pair[0]["messages"].as_array().unwrap(),
            pair[1]["messages"].as_array().unwrap(),
        );
        let unmarked =
            |messages: &[serde_json::Value]| messages.iter().map(content).collect::<Vec<_>>();
        assert_eq!(
            unmarked(earlier),
            unmarked(&later[..earlier.len()]),
            "a request edited what the one before it sent"
        );
        let result = later.last().unwrap().to_string();
        assert!(result.contains(sterna::prompt::BATCH_NUDGE), "{result}");
    }
    assert_eq!(
        bodies[2]
            .to_string()
            .matches("First privately list")
            .count(),
        2,
        "one nudge per result, none repeated"
    );
    let _ = std::fs::remove_dir_all(root);
}
