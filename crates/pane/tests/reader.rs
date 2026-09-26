//! `helper.find` as the reader (2026-09-23): one toolless request over
//! Pane's own evidence, its spans served from disk, and the Scout's search
//! loop only when nothing it named verifies.
#![cfg(any(target_os = "macos", target_os = "linux"))]
use pane::config::HelpersConfig;
use pane::contract::SessionId;
use pane::runtime::isolate::Runtime;
use pane::sandbox::profile::Profile;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::sync::mpsc;
use std::time::Duration;

#[path = "support/sse.rs"]
mod sse;

/// A provider answering `replies` in order, sending each request it read.
fn provider(replies: Vec<&'static str>) -> (String, mpsc::Receiver<serde_json::Value>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let (sender, requests) = mpsc::channel();
    std::thread::spawn(move || {
        for (reply, incoming) in replies.into_iter().zip(listener.incoming()) {
            let mut stream = incoming.unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut length = 0;
            loop {
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                if line == "\r\n" {
                    break;
                }
                if let Some(n) = line.to_lowercase().strip_prefix("content-length:") {
                    length = n.trim().parse().unwrap();
                }
            }
            let mut body = vec![0; length];
            reader.read_exact(&mut body).unwrap();
            let request: serde_json::Value = serde_json::from_slice(&body).unwrap();
            let (content_type, text) = sse::response_for(&request, reply);
            sender.send(request).unwrap();
            write!(
                stream,
                "HTTP/1.1 200 OK\r\ncontent-type: {content_type}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{text}",
                text.len()
            )
            .unwrap();
        }
    });
    (url, requests)
}

fn git_repo(label: &str) -> std::path::PathBuf {
    let root = std::env::temp_dir().join(format!("pane-reader-{label}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(
        root.join("src/gate.rs"),
        "// the completion gate\nfn gate() {\n    hold_once();\n}\n",
    )
    .unwrap();
    std::fs::write(root.join("README.md"), "a project\n").unwrap();
    let git = |args: &[&str]| {
        std::process::Command::new("git")
            .arg("-C")
            .arg(&root)
            .args(args)
            .output()
            .unwrap();
    };
    git(&["init", "-q"]);
    git(&["add", "--", "src/gate.rs", "README.md"]);
    root
}

fn runtime(root: &std::path::Path) -> Runtime {
    Runtime::new(
        &Profile::compile(root, Some(r#"{"permissions":{"allow":["Read(**)"]}}"#)),
        &SessionId::new("reader"),
    )
    .with_helpers(HelpersConfig {
        model: Some("fixture-helper".into()),
        ..HelpersConfig::default()
    })
}

#[test]
fn find_serves_the_lines_it_names_in_one_toolless_request_and_falls_back_to_the_loop_only_when_nothing_verifies()
 {
    let root = git_repo("one");
    let (url, requests) = provider(vec![
        // One request: the finder names a real range.
        r#"{"role":"assistant","content":[{"type":"text","text":"src/gate.rs:2-4 — the gate"}],"usage":{"input_tokens":10,"output_tokens":5}}"#,
        // The fallback scenario: the finder names nothing real, the loop answers.
        r#"{"role":"assistant","content":[{"type":"text","text":"nowhere.rs:1-9 — guessed"}],"usage":{"input_tokens":10,"output_tokens":5}}"#,
        r#"{"role":"assistant","content":[{"type":"text","text":"src/gate.rs:3 — hold_once is called"}],"usage":{"input_tokens":10,"output_tokens":5}}"#,
    ]);
    let previous = std::env::var_os("ANTHROPIC_BASE_URL");
    // One test in this binary: nothing else reads the variable meanwhile.
    unsafe { std::env::set_var("ANTHROPIC_BASE_URL", &url) };

    let mut first = runtime(&root);
    let found =
        first.run_cell("console.log(await helper.find('where does the completion gate hold'));");
    drop(first);
    let asked = requests.recv_timeout(Duration::from_secs(10)).unwrap();
    let out = found.turn().stdout_tail.clone();
    assert!(out.contains(pane::excerpts::HEADING), "{out}");
    assert!(
        out.contains("3 |     hold_once();"),
        "the file's own line, from disk: {out}"
    );
    assert!(
        asked
            .get("tools")
            .is_none_or(|tools| tools.as_array().is_some_and(Vec::is_empty)),
        "the finder holds no tools: {asked}"
    );
    let body = asked.to_string();
    assert!(body.contains("Lines holding its words"), "{body}");
    assert!(
        body.contains("src/gate.rs:1:// the completion gate"),
        "the grep hits are served: {body}"
    );
    assert!(
        requests.recv_timeout(Duration::from_millis(300)).is_err(),
        "a verified answer asks nothing more"
    );

    let mut second = runtime(&root);
    let looked = second.run_cell("console.log(await helper.find('where is hold_once called'));");
    drop(second);
    let _finder = requests.recv_timeout(Duration::from_secs(10)).unwrap();
    let fallback = requests.recv_timeout(Duration::from_secs(10)).unwrap();
    unsafe {
        match previous {
            Some(value) => std::env::set_var("ANTHROPIC_BASE_URL", value),
            None => std::env::remove_var("ANTHROPIC_BASE_URL"),
        }
    }
    let out = looked.turn().stdout_tail.clone();
    assert!(
        fallback["tools"]
            .as_array()
            .is_some_and(|tools| !tools.is_empty()),
        "nothing verified, so the search loop ran: {fallback}"
    );
    assert!(out.contains("3 |     hold_once();"), "{out}");
    let _ = std::fs::remove_dir_all(root);
}
