//! A child agent receives stable environment/root context and newly applicable
//! nested policy before its write is allowed to execute.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use sterna::agent::AgentOptions;
use sterna::bg;
use sterna::contract::SessionId;
use sterna::events::Kind;
use sterna::sandbox::profile::Profile;

static ENV_LOCK: Mutex<()> = Mutex::new(());
static NEXT: AtomicUsize = AtomicUsize::new(0);

struct Fixture {
    root: PathBuf,
    session: SessionId,
}
impl Fixture {
    fn new() -> Self {
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "sterna-subagent-orientation-{}-{n}",
            std::process::id()
        ));
        std::fs::create_dir_all(root.join("nested")).unwrap();
        std::fs::write(root.join("AGENTS.md"), "ROOT_AGENTS_GUIDANCE").unwrap();
        std::fs::write(root.join("CLAUDE.md"), "ROOT_CLAUDE_GUIDANCE").unwrap();
        std::fs::write(root.join("nested/AGENTS.md"), "NESTED_WRITE_GUIDANCE").unwrap();
        Self {
            root,
            session: SessionId::new(format!("subagent-orientation-{n}")),
        }
    }
    fn profile(&self) -> Profile {
        Profile::compile(
            &self.root,
            Some(r#"{"permissions":{"allow":["Read(**)","Write(**)"]}}"#),
        )
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        bg::shutdown(&self.session);
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn provider(target: PathBuf) -> (String, Arc<Mutex<Vec<serde_json::Value>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let bodies = Arc::new(Mutex::new(Vec::new()));
    let seen = bodies.clone();
    std::thread::spawn(move || {
        for turn in 0..2 {
            let (mut stream, _) = listener.accept().unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut length = 0usize;
            loop {
                let mut line = String::new();
                if reader.read_line(&mut line).unwrap() == 0 {
                    return;
                }
                if line == "\r\n" || line == "\n" {
                    break;
                }
                if let Some(value) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                    length = value.trim().parse().unwrap();
                }
            }
            let mut body = vec![0; length];
            reader.read_exact(&mut body).unwrap();
            let request: serde_json::Value = serde_json::from_slice(&body).unwrap();
            assert!(
                !target.exists(),
                "nested write ran before provider received its policy"
            );
            seen.lock().unwrap().push(request);
            let (id, content) = if turn == 0 {
                (
                    "discover-policy",
                    "await write({path: 'nested/result.txt', content: 'too early'});",
                )
            } else {
                (
                    "write-after-policy",
                    "await write({path: 'nested/result.txt', content: 'verified'}); answer('done');",
                )
            };
            let payload = serde_json::json!({
                "role":"assistant",
                "content":[{"type":"tool_use","id":id,"name":"execute_cell","input":{"code":content}}],
                "usage":{"input_tokens":10,"output_tokens":5}
            }).to_string();
            write!(stream, "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}", payload.len(), payload).unwrap();
        }
    });
    (format!("http://{address}"), bodies)
}

#[test]
fn child_receives_root_orientation_and_nested_policy_before_nested_write() {
    let _environment = ENV_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    let fixture = Fixture::new();
    let target = fixture.root.join("nested/result.txt");
    let (url, bodies) = provider(target.clone());
    unsafe {
        std::env::set_var("ANTHROPIC_BASE_URL", &url);
    }
    let _handle = bg::agent(
        &fixture.profile(),
        &fixture.session,
        "write nested/result.txt after following every applicable instruction",
        &AgentOptions {
            turns: Some(4),
            deadline: None,
            model: "test-model".into(),
            effort: sterna::wire::Effort::Default,
        },
    );
    let deadline = Instant::now() + Duration::from_secs(20);
    let events = loop {
        let events = bg::drain(&fixture.session);
        if events
            .iter()
            .any(|event| matches!(event.kind, Kind::AgentDone { .. }))
        {
            break events;
        }
        assert!(Instant::now() < deadline, "subagent did not finish");
        std::thread::sleep(Duration::from_millis(20));
    };
    unsafe {
        std::env::remove_var("ANTHROPIC_BASE_URL");
    }

    let bodies = bodies.lock().unwrap();
    assert_eq!(bodies.len(), 2);
    let first_system = bodies[0]["system"][0]["text"].as_str().unwrap();
    assert!(
        first_system.contains("ROOT_AGENTS_GUIDANCE"),
        "{first_system}"
    );
    assert!(
        first_system.contains("ROOT_CLAUDE_GUIDANCE"),
        "{first_system}"
    );
    assert!(
        first_system.contains("## Environment orientation"),
        "{first_system}"
    );
    assert!(
        first_system.contains(&format!(
            "project root: {}",
            fixture.profile().root().display()
        )),
        "{first_system}"
    );
    assert!(!first_system.contains("NESTED_WRITE_GUIDANCE"));

    let second = bodies[1].to_string();
    assert!(second.contains("## Environment orientation"), "{second}");
    assert!(second.contains("ROOT_AGENTS_GUIDANCE"), "{second}");
    assert!(second.contains("ROOT_CLAUDE_GUIDANCE"), "{second}");
    assert!(
        second.contains("## Newly applicable project instructions"),
        "{second}"
    );
    assert!(second.contains("NESTED_WRITE_GUIDANCE"), "{second}");
    assert!(second.contains("did not run"), "{second}");
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "verified");
    assert!(
        events
            .iter()
            .any(|event| matches!(event.kind, Kind::AgentDone { .. }))
    );
}
