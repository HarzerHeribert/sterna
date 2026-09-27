//! Binary-level proof that incomplete provider output cannot end a task successfully.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::process::Command;

#[test]
fn max_token_prose_and_native_calls_fail_without_execution_or_retry() {
    for with_call in [false, true] {
        let root = std::env::temp_dir().join(format!(
            "sterna-truthful-completion-{}-{with_call}",
            std::process::id()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base_url = format!("http://{}", listener.local_addr().unwrap());
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
        let provider = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
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
            reader.read_exact(&mut vec![0; length]).unwrap();
            write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", response.len(), response).unwrap();
            // The endpoint closes after one request: an accidental retry cannot
            // produce the original incomplete-response diagnostic below.
        });
        let output = Command::new(env!("CARGO_BIN_EXE_sterna"))
            .args(["session", "--root"])
            .arg(&root)
            .args([
                "--task",
                "Do the requested work",
                "--model",
                "fixture",
                "--yolo",
            ])
            .env("ANTHROPIC_BASE_URL", base_url)
            .env("XDG_CONFIG_HOME", root.join("global-config"))
            .env_remove("ANTHROPIC_API_KEY")
            .env_remove("ANTHROPIC_AUTH_TOKEN")
            .output()
            .unwrap();
        provider.join().unwrap();

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
