use serde_json::{Value, json};
use std::collections::{BTreeMap, VecDeque};
use std::path::PathBuf;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicU64, Ordering},
};
use std::time::Duration;
use sterna::project::mcp::parse;
use sterna::sandbox::profile::Profile;
use sterna::tools::{
    invoke::{CancellationToken, Confinement},
    mcp::Mcp,
};
use sterna::web::{WebBroker, WebConfig, WebPostResponse, WebResponse, WebTransport};

type Calls = Arc<Mutex<Vec<(String, BTreeMap<String, String>, Value)>>>;
struct Fake {
    replies: Mutex<VecDeque<WebPostResponse>>,
    calls: Calls,
}
impl WebTransport for Fake {
    fn get(&self, _: &str, _: usize, _: Duration) -> Result<WebResponse, String> {
        Err("unexpected GET".into())
    }
    fn post(
        &self,
        url: &str,
        headers: &BTreeMap<String, String>,
        body: &[u8],
        _: usize,
        _: Duration,
    ) -> Result<WebPostResponse, String> {
        self.calls.lock().unwrap().push((
            url.into(),
            headers.clone(),
            serde_json::from_slice(body).unwrap(),
        ));
        self.replies
            .lock()
            .unwrap()
            .pop_front()
            .ok_or("unexpected POST".into())
    }
}
fn response(status: u16, media: &str, body: String, session: Option<&str>) -> WebPostResponse {
    WebPostResponse {
        status,
        content_type: media.into(),
        session_id: session.map(str::to_owned),
        body: body.into_bytes(),
    }
}
fn rpc(id: u64, result: Value) -> WebPostResponse {
    response(
        200,
        "application/json",
        json!({"jsonrpc":"2.0", "id":id,"result":result}).to_string(),
        None,
    )
}
fn initialization() -> WebPostResponse {
    let mut reply = rpc(
        1,
        json!({"protocolVersion":"2025-03-26","capabilities":{"tools":{}},"serverInfo":{"name":"fixture","version":"1"}}),
    );
    reply.session_id = Some("session-private-123".into());
    reply
}
fn listed() -> WebPostResponse {
    rpc(
        2,
        json!({"tools":[{"name":"echo","description":"Echo input","inputSchema":{"type":"object"}},{"name":"forbidden","inputSchema":{"type":"object"}}]}),
    )
}
fn configured(replies: Vec<WebPostResponse>, config: WebConfig) -> (Mcp, Calls) {
    let calls = Arc::new(Mutex::new(vec![]));
    let broker = WebBroker::with_transport(
        config,
        Box::new(Fake {
            replies: Mutex::new(replies.into()),
            calls: calls.clone(),
        }),
    )
    .unwrap();
    let mut mcp = Mcp::default();
    mcp.with_web_broker(broker);
    (mcp, calls)
}
fn enabled() -> WebConfig {
    WebConfig {
        enabled: true,
        ..WebConfig::default()
    }
}
static NEXT: AtomicU64 = AtomicU64::new(0);
struct Fixture(PathBuf);
impl Fixture {
    fn new(url: &str) -> Self {
        let root = std::env::temp_dir().join(format!(
            "sterna-remote-mcp-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join(".mcp.json"), json!({"mcpServers":{"remote":{"type":"http","url":url,"headers":{"Authorization":"Bearer ${TOKEN}"},"env":{"TOKEN":"explicit-fixture-secret"}}}}).to_string()).unwrap();
        Self(root)
    }
    fn profile(&self) -> Profile {
        Profile::compile(
            &self.0,
            Some(
                r#"{"permissions":{"allow":["mcp__remote__*"],"deny":["mcp__remote__forbidden"]}}"#,
            ),
        )
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn initialize_discover_call_session_headers_and_domain_policy() {
    let fixture = Fixture::new("https://mcp.example.com/tools");
    let (mut mcp, calls) = configured(
        vec![
            initialization(),
            response(202, "", String::new(), None),
            listed(),
            rpc(3, json!({"content":[{"type":"text","text":"hello"}]})),
        ],
        enabled(),
    );
    let token = CancellationToken::new();
    let tools = mcp.list(&fixture.profile(), &token).unwrap();
    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0].tool, "echo");
    let result = mcp
        .call(
            &fixture.profile(),
            &token,
            &tools[0].name,
            json!({"text":"hello"}),
        )
        .unwrap();
    assert_eq!(result.confinement, Confinement::BrokeredNetwork);
    assert!(result.stdout.contains("hello"));
    assert!(!result.stdout.contains("secret"));
    let calls = calls.lock().unwrap();
    assert_eq!(calls.len(), 4);
    assert!(!calls[0].1.contains_key("mcp-session-id"));
    for (_, headers, _) in calls.iter().skip(1) {
        assert_eq!(headers["mcp-session-id"], "session-private-123");
    }
    for (url, headers, _) in calls.iter() {
        assert_eq!(url, "https://mcp.example.com/tools");
        assert_eq!(headers["authorization"], "Bearer explicit-fixture-secret");
    }
}

#[test]
fn finite_sse_notifications_then_response_are_supported() {
    let fixture = Fixture::new("https://mcp.example.com");
    let event = format!(
        "event: message\r\ndata: {}\r\n\r\n: heartbeat\r\ndata: {}\r\n\r\n",
        json!({"jsonrpc":"2.0","method":"notifications/message","params":{}}),
        json!({"jsonrpc":"2.0","id":2,"result":{"tools":[{"name":"echo","inputSchema":{"type":"object"}}]}})
    );
    let (mut mcp, _) = configured(
        vec![
            initialization(),
            response(202, "", String::new(), None),
            response(200, "text/event-stream", event, None),
        ],
        enabled(),
    );
    assert_eq!(
        mcp.list(&fixture.profile(), &CancellationToken::new())
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn disabled_denied_private_and_ungranted_servers_make_no_requests() {
    for (url, config) in [
        ("https://mcp.example.com", WebConfig::default()),
        (
            "https://mcp.example.com",
            WebConfig {
                deny_domains: vec!["mcp.example.com".into()],
                ..enabled()
            },
        ),
        ("https://127.0.0.1/", enabled()),
    ] {
        let fixture = Fixture::new(url);
        let (mut mcp, calls) = configured(vec![], config);
        assert!(
            mcp.list(&fixture.profile(), &CancellationToken::new())
                .is_err()
        );
        assert!(calls.lock().unwrap().is_empty());
    }
    let fixture = Fixture::new("https://mcp.example.com");
    let (mut mcp, calls) = configured(vec![], enabled());
    assert!(
        mcp.list(
            &Profile::compile(&fixture.0, None),
            &CancellationToken::new()
        )
        .unwrap()
        .is_empty()
    );
    assert!(calls.lock().unwrap().is_empty());
}

#[test]
fn redirects_oversize_bad_ids_and_server_requests_fail_without_replay() {
    for reply in [
        response(307, "text/html", "redirect".into(), None),
        response(200, "application/json", "x".repeat(1_048_577), None),
        rpc(999, json!({})),
        response(
            200,
            "application/json",
            json!({"jsonrpc":"2.0","id":99,"method":"sampling/createMessage"}).to_string(),
            None,
        ),
    ] {
        let fixture = Fixture::new("https://mcp.example.com");
        let (mut mcp, calls) = configured(vec![reply], enabled());
        let error = mcp
            .list(&fixture.profile(), &CancellationToken::new())
            .err()
            .unwrap()
            .to_string();
        assert!(!error.contains("explicit-fixture-secret"));
        assert_eq!(calls.lock().unwrap().len(), 1);
    }
}

#[test]
fn remote_config_requires_explicit_env_and_rejects_protocol_header_override() {
    assert!(parse(Some(r#"{"mcpServers":{"r":{"type":"http","url":"https://example.com","headers":{"Authorization":"Bearer ${PATH}"}}}}"#)).is_err());
    assert!(
        parse(Some(
            r#"{"mcpServers":{"r":{"type":"sse","url":"https://example.com"}}}"#
        ))
        .err()
        .unwrap()
        .contains("legacy")
    );
    assert!(parse(Some(r#"{"mcpServers":{"r":{"type":"http","url":"https://example.com","headers":{"Mcp-Session-Id":"spoof"}}}}"#)).is_err());
}

#[test]
fn cancellation_before_discovery_never_posts() {
    let fixture = Fixture::new("https://mcp.example.com");
    let (mut mcp, calls) = configured(vec![], enabled());
    let token = CancellationToken::new();
    token.cancel();
    assert!(mcp.list(&fixture.profile(), &token).is_err());
    assert!(calls.lock().unwrap().is_empty());
}

#[test]
fn cancellation_after_initialize_does_not_dispatch_queued_notification() {
    struct CancelInitialize {
        token: CancellationToken,
        calls: Arc<Mutex<usize>>,
    }
    impl WebTransport for CancelInitialize {
        fn get(&self, _: &str, _: usize, _: Duration) -> Result<WebResponse, String> {
            Err("unexpected GET".into())
        }
        fn post(
            &self,
            _: &str,
            _: &BTreeMap<String, String>,
            _: &[u8],
            _: usize,
            timeout: Duration,
        ) -> Result<WebPostResponse, String> {
            assert!(timeout <= Duration::from_secs(10));
            *self.calls.lock().unwrap() += 1;
            self.token.cancel();
            Ok(initialization())
        }
    }
    let token = CancellationToken::new();
    let calls = Arc::new(Mutex::new(0));
    let broker = WebBroker::with_transport(
        enabled(),
        Box::new(CancelInitialize {
            token: token.clone(),
            calls: calls.clone(),
        }),
    )
    .unwrap();
    let fixture = Fixture::new("https://mcp.example.com");
    let mut mcp = Mcp::default();
    mcp.with_web_broker(broker);
    assert!(mcp.list(&fixture.profile(), &token).is_err());
    drop(mcp);
    assert_eq!(*calls.lock().unwrap(), 1);
}
