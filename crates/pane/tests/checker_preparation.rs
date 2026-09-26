#![cfg(any(target_os = "macos", target_os = "linux"))]
use pane::config::HelpersConfig;
use pane::contract::SessionId;
use pane::runtime::isolate::Runtime;
use pane::sandbox::profile::Profile;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::time::{Duration, Instant};

#[path = "support/sse.rs"]
mod sse;

#[test]
fn checker_first_request_receives_actual_named_check_then_reuses_unchanged_evidence() {
    let root = std::env::temp_dir().join(format!("pane-checker-prepare-{}", std::process::id()));
    std::fs::create_dir_all(root.join(".glasshouse")).unwrap();
    std::fs::write(root.join("input.txt"), "FIRST-VERIFIED\n").unwrap();
    std::fs::write(root.join(".glasshouse/checks.toml"),"checker = [\"tests\"]\n[checks.tests]\ncommand = \"/bin/cat input.txt\"\ninputs = [\"input.txt\"]\nreuse = true\n").unwrap();
    std::fs::write(
        root.join("README.md"),
        "Original acceptance: preserve FIRST-VERIFIED.\n",
    )
    .unwrap();
    std::fs::create_dir_all(root.join("node_modules/noise")).unwrap();
    std::fs::write(
        root.join("node_modules/noise/hidden.txt"),
        "DO_NOT_SEED_GENERATED",
    )
    .unwrap();
    let profile = Profile::compile(
        &root,
        Some(r#"{"permissions":{"allow":["Read(**)","Bash(/bin/cat*)"]}}"#),
    );
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let worker = std::thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(20);
        let mut requests = Vec::new();
        while requests.len() < 4 && Instant::now() < deadline {
            let (mut stream, _) = match listener.accept() {
                Ok(pair) => pair,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(10));
                    continue;
                }
                Err(error) => panic!("{error}"),
            };
            stream.set_nonblocking(false).unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(3)))
                .unwrap();
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
            assert!(length < 1024 * 1024);
            let mut body = vec![0; length];
            reader.read_exact(&mut body).unwrap();
            let request = serde_json::from_slice::<serde_json::Value>(&body).unwrap();
            requests.push(request.clone());
            let whole = r#"{"role":"assistant","content":[{"type":"text","text":"holds\ninput.txt:1 FIRST-VERIFIED. Only the provided observation was checked."}],"usage":{"input_tokens":10,"output_tokens":5}}"#;
            let (content_type, reply) = sse::response_for(&request, whole);
            write!(stream,"HTTP/1.1 200 OK\r\ncontent-type: {content_type}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",reply.len(),reply).unwrap();
        }
        requests
    });
    let previous = std::env::var_os("ANTHROPIC_BASE_URL");
    // This integration-test process has one test. The provider thread never
    // reads process environment, and cleanup happens after runtime calls finish.
    unsafe {
        std::env::set_var("ANTHROPIC_BASE_URL", url);
    }
    let mut runtime =
        Runtime::new(&profile, &SessionId::new("checker-prepare")).with_helpers(HelpersConfig {
            model: Some("fixture-helper".into()),
            ..HelpersConfig::default()
        });
    let warmed = runtime
        .run_cell("const verified = await checks.run('tests', true); console.log(verified);");
    let scout = runtime.run_cell("const oriented = await helper.find('Find FIRST-VERIFIED acceptance'); console.log(oriented);");
    let first=runtime.run_cell("const first = await helper.check(JSON.stringify({authoritative_contract: 'input.txt must begin FIRST-VERIFIED', current_source: 'input.txt:1 FIRST-VERIFIED', configured_verification: verified, question: 'Check the current source against the original contract; report whether this named test observation was freshly executed and passed, and whether all original tests remain'})); console.log(first);");
    let second=runtime.run_cell("const second = await helper.check('Check the same original requirement with current evidence'); console.log(second);");
    let reducer = runtime.run_cell("const reduced = await helper.reduce('command: tests\\nexit code: 1\\nerror: REAL-FAILURE-SENTINEL'); console.log(reduced);");
    drop(runtime);
    unsafe {
        match previous {
            Some(value) => std::env::set_var("ANTHROPIC_BASE_URL", value),
            None => std::env::remove_var("ANTHROPIC_BASE_URL"),
        };
    }
    let requests = worker.join().unwrap();
    std::fs::remove_dir_all(root).unwrap();
    assert!(
        warmed.turn().stdout_tail.contains("\"executed\": true"),
        "{warmed:?}"
    );
    assert!(first.turn().stdout_tail.contains("holds"), "{first:?}");
    assert!(second.turn().stdout_tail.contains("holds"), "{second:?}");
    assert_eq!(requests.len(), 4);
    assert!(scout.turn().stdout_tail.contains("holds"), "{scout:?}");
    assert!(reducer.turn().stdout_tail.contains("holds"), "{reducer:?}");
    fn strings(value: &serde_json::Value, out: &mut String) {
        match value {
            serde_json::Value::String(s) => {
                out.push_str(s);
                out.push('\n');
            }
            serde_json::Value::Array(v) => {
                for x in v {
                    strings(x, out)
                }
            }
            serde_json::Value::Object(v) => {
                for x in v.values() {
                    strings(x, out)
                }
            }
            _ => {}
        }
    }
    let mut first_body = String::new();
    strings(&requests[1], &mut first_body);
    let mut second_body = String::new();
    strings(&requests[2], &mut second_body);
    assert!(first_body.contains("Check the current source against the original contract"));
    assert!(second_body.contains("Check the same original requirement with current evidence"));
    let mut scout_body = String::new();
    strings(&requests[0], &mut scout_body);
    assert!(
        scout_body.contains("Deterministic starting evidence"),
        "{scout_body}"
    );
    assert!(scout_body.contains("[Tree]"), "{scout_body}");
    assert!(!scout_body.contains("DO_NOT_SEED_GENERATED"));
    let mut reducer_body = String::new();
    strings(&requests[3], &mut reducer_body);
    assert!(reducer_body.contains("role: reducer"), "{reducer_body}");
    assert!(reducer_body.contains("REAL-FAILURE-SENTINEL"));
    for body in [&first_body, &second_body] {
        assert!(body.contains("FIRST-VERIFIED"));
        assert!(body.contains("\"exit_code\":0"), "{body}");
        assert!(body.contains("Original checker request"));
        assert!(body.contains("[Contract] README.md"), "{body}");
    }
    let host_evidence = first_body
        .find("Host verification observations")
        .expect("the actual first checker packet includes host evidence");
    let original_request = &first_body[..host_evidence];
    let prepared_observation = &first_body[host_evidence..];
    assert!(
        original_request.contains("\"executed\":true")
            && original_request.contains("\"reused\":false"),
        "the caller's proven fresh observation must survive into the first checker packet: {first_body}"
    );
    assert!(
        prepared_observation.contains("\"executed\":false")
            && prepared_observation.contains("\"reused\":true"),
        "host preparation must accurately label reuse: {first_body}"
    );
    assert!(
        prepared_observation.contains("originally executed earlier in this request"),
        "{first_body}"
    );
    assert!(
        first_body.contains("Current source and its contract can establish a current-state claim")
            && first_body.contains(
                "no unified-diff paths; change-history claims need other baseline evidence"
            ),
        "the first packet must permit current-state assessment while preserving the missing baseline limitation: {first_body}"
    );
    assert!(second_body.contains("\"executed\":false"), "{second_body}");
    assert!(second_body.contains("\"reused\":true"), "{second_body}");
}
