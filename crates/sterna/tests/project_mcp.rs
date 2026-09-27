//! Hermetic stdio protocol fixtures: bash builtins only, no service or model.
#![cfg(any(target_os = "macos", target_os = "linux"))]
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use serde_json::json;
use sterna::contract::SessionId;
use sterna::runtime::isolate::Runtime;
use sterna::runtime::outcome::CellOutcome;
use sterna::runtime::preview::Value;
use sterna::sandbox::profile::Profile;
use sterna::tools::invoke::CancellationToken;
use sterna::tools::mcp::Mcp;
use sterna::tools::registry::mcp_name;

static NEXT: AtomicU64 = AtomicU64::new(0);
struct Fixture {
    root: PathBuf,
}
impl Fixture {
    fn new(mode: &str) -> Self {
        let root = std::env::temp_dir().join(format!(
            "sterna-mcp-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&root).unwrap();
        let server = r#"printf '%s' "$$" > server.pid
printf 'private-server-diagnostic\n' >&2
while IFS= read -r request; do
  case "$request" in
    *'"method":"initialize"'*)
      if [[ "$MODE" == bad_init ]]; then printf '%s\n' 'private-malformed-secret'; continue; fi
      printf '%s\n' '{"jsonrpc":"2.0","id":1,"result":{"protocolVersion":"2024-11-05","capabilities":{"tools":{}},"serverInfo":{"name":"fake","version":"1"}}}' ;;
    *'"method":"notifications/initialized"'*) printf yes > initialized ;;
    *'"method":"tools/list"'*)
      [[ -f initialized ]] || exit 2
      if [[ "$MODE" == bad_list ]]; then printf '%s\n' '{"jsonrpc":"2.0","id":2,"result":{"tools":"bad"}}'; continue; fi
      printf '%s\n' '{"jsonrpc":"2.0","id":2,"result":{"tools":[{"name":"echo","description":"Return nested input","inputSchema":{"type":"object","properties":{"nested":{"type":"object"}}}},{"name":"Web_Fetch","inputSchema":{"type":"object"}},{"name":"websearch","inputSchema":{"type":"object"}},{"name":"forbidden","inputSchema":{"type":"object"}}]}}' ;;
    *'"method":"tools/call"'*)
      printf called > called
      if [[ "$MODE" == stall ]]; then while :; do :; done; fi
      if [[ "$MODE" == bad_call ]]; then printf '%s\n' '{"jsonrpc":"2.0","id":999,"result":{}}'; continue; fi
      if [[ "$MODE" == oversized ]]; then printf '%s' '{"jsonrpc":"2.0","id":3,"result":{"content":[{"type":"text","text":"'; printf '%9000000s' ''; printf '%s\n' '"}]}}'; continue; fi
      if [[ "$MODE" == huge ]]; then
        IFS= read -r payload < payload
        printf '{"jsonrpc":"2.0","id":3,"result":{"content":[{"type":"text","text":"%s"}]}}\n' "$payload"
      elif [[ "$MODE" == error ]]; then
        printf '%s\n' '{"jsonrpc":"2.0","id":3,"result":{"content":[{"type":"text","text":"declined"}],"isError":true}}'
      else
        [[ "$request" == *'"nested":{"flag":true,"numbers":[1,2]}'* ]] || exit 3
        printf '%s\n' '{"jsonrpc":"2.0","id":3,"result":{"content":[{"type":"text","text":"ok"}],"structuredContent":{"accepted":true}}}'
      fi ;;
  esac
done
"#;
        std::fs::write(root.join("server.sh"), server).unwrap();
        std::fs::write(root.join("payload"), format!("{}\n", "x".repeat(400_000))).unwrap();
        std::fs::write(root.join(".mcp.json"), json!({"mcpServers":{"demo":{"command":"/bin/bash", "args":["server.sh"], "env":{"MODE":mode}}}}).to_string()).unwrap();
        Self { root }
    }
    fn profile(&self) -> Profile {
        Profile::compile(
            &self.root,
            Some(r#"{"permissions":{"allow":["mcp__demo__*"],"deny":["mcp__demo__forbidden"]}}"#),
        )
    }
    fn pid(&self) -> String {
        std::fs::read_to_string(self.root.join("server.pid")).unwrap()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}
fn alive(pid: &str) -> bool {
    std::process::Command::new("/bin/kill")
        .args(["-0", pid])
        .stderr(std::process::Stdio::null())
        .status()
        .unwrap()
        .success()
}
fn arguments() -> serde_json::Value {
    json!({"nested":{"flag":true,"numbers":[1,2]}})
}

#[test]
fn discovery_call_permissions_and_process_teardown() {
    let fixture = Fixture::new("normal");
    let profile = fixture.profile();
    let token = CancellationToken::new();
    let mut mcp = Mcp::default();
    let tools = mcp.list(&profile, &token).unwrap();
    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0].name, mcp_name("demo", "echo"));
    assert_eq!(
        tools[0].input_schema["properties"]["nested"]["type"],
        "object"
    );
    let pid = fixture.pid();
    assert!(alive(&pid));
    let denied_profile = Profile::compile(&fixture.root, None);
    assert!(
        mcp.call(&denied_profile, &token, &tools[0].name, arguments())
            .unwrap_err()
            .denied()
            .is_some()
    );
    assert!(!fixture.root.join("called").exists());
    assert!(
        mcp.call(&profile, &token, &mcp_name("demo", "forbidden"), json!({}))
            .unwrap_err()
            .denied()
            .is_some()
    );
    let result = mcp
        .call(&profile, &token, &tools[0].name, arguments())
        .unwrap();
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&result.stdout).unwrap()["structuredContent"]["accepted"],
        true
    );
    drop(mcp);
    assert!(!alive(&pid), "runtime-owned child was not reaped");
}

#[test]
fn no_grant_never_spawns_and_exact_grant_can_discover() {
    let fixture = Fixture::new("normal");
    let token = CancellationToken::new();
    let mut mcp = Mcp::default();
    assert!(
        mcp.list(&Profile::compile(&fixture.root, None), &token)
            .unwrap()
            .is_empty()
    );
    assert!(!fixture.root.join("server.pid").exists());
    let profile = Profile::compile(
        &fixture.root,
        Some(r#"{"permissions":{"allow":["mcp__demo__echo"]}}"#),
    );
    assert!(profile.admits_mcp_server("demo"));
    assert!(!profile.admits_mcp_server("other"));
    assert_eq!(Mcp::default().list(&profile, &token).unwrap().len(), 1);
    let denied = Profile::compile(
        &fixture.root,
        Some(r#"{"permissions":{"allow":["mcp__demo__echo"],"deny":["mcp__DEMO__*"]}}"#),
    );
    assert!(!denied.admits_mcp_server("demo"));
}

#[test]
fn malformed_discovery_and_calls_fail_without_echoing_server_secrets() {
    for mode in ["bad_init", "bad_list", "bad_call", "oversized"] {
        let fixture = Fixture::new(mode);
        let profile = fixture.profile();
        let token = CancellationToken::new();
        let mut mcp = Mcp::default();
        let error = match mcp.list(&profile, &token) {
            Err(error) => error,
            Ok(tools) => mcp
                .call(&profile, &token, &tools[0].name, arguments())
                .unwrap_err(),
        };
        assert!(!error.to_string().contains("private"));
        assert!(!alive(&fixture.pid()), "malformed server leaked in {mode}");
    }
}

#[test]
fn cancellation_stops_an_unresponsive_stdio_server() {
    let fixture = Fixture::new("stall");
    let profile = fixture.profile();
    let token = CancellationToken::new();
    let mut mcp = Mcp::default();
    let tools = mcp.list(&profile, &token).unwrap();
    let cancel = token.clone();
    let thread = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(100));
        cancel.cancel();
    });
    let start = Instant::now();
    assert!(matches!(
        mcp.call(&profile, &token, &tools[0].name, arguments()),
        Err(sterna::tools::invoke::ToolError::Cancelled { .. })
    ));
    thread.join().unwrap();
    assert!(start.elapsed() < Duration::from_secs(2));
    assert!(!alive(&fixture.pid()));
}

#[test]
fn runtime_keeps_large_results_as_bounded_effectful_handles() {
    let fixture = Fixture::new("huge");
    let mut runtime = Runtime::new(&fixture.profile(), &SessionId::new("mcp"));
    let outcome =
        runtime.run_cell("const tools = mcp.list(); const result = mcp.call(tools[0].name, {});");
    assert!(
        matches!(outcome, CellOutcome::Yielded { .. }),
        "{outcome:?}"
    );
    assert!(runtime.render_handles().len() < 4096);
    let handle = outcome
        .turn()
        .record
        .handles
        .iter()
        .find(|h| h.name == "result")
        .unwrap();
    assert_eq!(handle.type_name, "MCP.Result");
    assert!(!handle.provenance.as_ref().unwrap().pure);
    let next = runtime.run_cell("return result.content[0].text.length;");
    assert!(
        matches!(
            next,
            CellOutcome::Returned {
                value: Value::Number(400_000.0),
                ..
            }
        ),
        "{next:?}"
    );
    let pid = fixture.pid();
    drop(runtime);
    assert!(!alive(&pid));
}

#[test]
fn tool_error_remains_inspectable_data() {
    let fixture = Fixture::new("error");
    let mut runtime = Runtime::new(&fixture.profile(), &SessionId::new("mcp-error"));
    let result =
        runtime.run_cell("const tools = mcp.list(); return mcp.call(tools[0].name, {}).isError;");
    assert!(
        matches!(
            result,
            CellOutcome::Returned {
                value: Value::Boolean(true),
                ..
            }
        ),
        "{result:?}"
    );
}

#[test]
fn encoded_names_cannot_collide_and_invalid_configuration_is_redacted() {
    assert_ne!(mcp_name("a-b", "x"), mcp_name("a_2db", "x"));
    assert_ne!(mcp_name("a", "b__c"), mcp_name("a__b", "c"));
    assert!(
        sterna::project::mcp::parse(Some("private config secret"))
            .err()
            .unwrap()
            .contains("invalid")
    );
    let remote = r#"{"mcpServers":{"remote":{"type":"http","url":"https://invalid.test"}}}"#;
    assert!(sterna::project::mcp::parse(Some(remote)).unwrap()["remote"].is_remote());
}

#[test]
fn runtime_mcp_gates_discovery_refuses_a_denied_tool_and_never_records_argument_values() {
    let fixture = Fixture::new("huge");
    let server = fixture.root.join("server.sh");
    std::fs::write(
        &server,
        std::fs::read_to_string(&server)
            .unwrap()
            .replace("Return nested input", &"d".repeat(4000)),
    )
    .unwrap();
    std::fs::create_dir_all(fixture.root.join("nested")).unwrap();
    std::fs::write(fixture.root.join("nested/AGENTS.md"), "nested MCP policy").unwrap();
    let mut runtime =
        Runtime::new(&fixture.profile(), &SessionId::new("mcp-gate")).with_instruction_context();
    let blocked = runtime.run_cell("const tools = mcp.list();");
    assert!(matches!(blocked, CellOutcome::Yielded { .. }));
    assert!(
        runtime
            .pending_instructions()
            .unwrap()
            .text
            .contains("nested MCP policy")
    );
    assert!(!fixture.root.join("server.pid").exists());
    assert!(!fixture.root.join("called").exists());
    assert!(blocked.turn().record.calls.is_empty());
    runtime.acknowledge_instructions();
    let listed = runtime.run_cell("const tools = mcp.list();");
    assert!(matches!(listed, CellOutcome::Yielded { .. }), "{listed:?}");
    assert_eq!(listed.turn().record.calls.len(), 1);
    assert_eq!(listed.turn().record.calls[0].tool, "mcp.list");

    // The advertised-but-denied tool must have no server effect, while its
    // refusal is still catchable.
    let refused = runtime.run_cell("try { mcp.call('mcp__demo__forbidden', {password:'private-argument-secret'}); } catch (error) { console.log(error.name); }"); // glasshouse:not-a-secret
    assert!(refused.turn().stdout_tail.contains("PermissionDenied"));
    assert!(!fixture.root.join("called").exists());

    let successful = runtime
        .run_cell("const result = mcp.call(tools[0].name, {password:'private-argument-secret'});"); // glasshouse:not-a-secret
    assert!(
        matches!(successful, CellOutcome::Yielded { .. }),
        "{successful:?}"
    );
    assert!(fixture.root.join("called").exists());
    for outcome in [&listed, &refused, &successful] {
        let record = serde_json::to_string(&outcome.turn().record.calls).unwrap();
        assert!(!record.contains("private-argument-secret"));
        assert!(
            outcome
                .turn()
                .record
                .calls
                .iter()
                .all(|call| call.args.is_empty())
        );
    }
    let full = runtime.run_cell("return result.content[0].text.length;");
    assert!(matches!(
        full,
        CellOutcome::Returned {
            value: Value::Number(400_000.0),
            ..
        }
    ));
}
