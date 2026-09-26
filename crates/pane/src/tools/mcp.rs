//! Project-owned stdio MCP clients. All children use the existing confinement
//! applier; a client's Drop kills its process group and reaps its child.
use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::PathBuf;
use std::process::Command;
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::{Duration, Instant};

use serde::Serialize;
use serde_json::{Value, json};

use crate::project::mcp::{self, Server};
use crate::sandbox::profile::{Access, PermissionDenied, Profile};
use crate::tools::invoke::{
    self, CancellationToken, Confinement, ExecGrant, ToolError, ToolResult,
};
use crate::tools::registry;

const FRAME_BYTES: usize = 8 * 1024 * 1024;
const TIMEOUT: Duration = Duration::from_secs(10);
const MAX_TOOLS: usize = 128;

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Descriptor {
    pub name: String,
    pub server: String,
    pub tool: String,
    pub description: String,
    pub input_schema: Value,
}

/// Runtime-scoped, lazy discovery: constructing a runtime never spawns.
#[derive(Default)]
pub struct Mcp {
    discovered: bool,
    clients: BTreeMap<String, Client>,
    tools: BTreeMap<String, Descriptor>,
    web: Option<Arc<crate::web::WebBroker>>,
}

fn failure(reason: &str) -> ToolError {
    ToolError::Spawn {
        tool: "mcp".into(),
        program: PathBuf::new(),
        error: reason.into(),
    }
}
fn denied() -> ToolError {
    PermissionDenied {
        tool: "mcp".into(),
        path: String::new(),
        rule: "MCP tool is not admitted by this session's project profile".into(),
    }
    .into()
}

impl Mcp {
    pub fn configure_web(&mut self, config: crate::web::WebConfig) -> Result<(), String> {
        self.with_web_broker(crate::web::WebBroker::new(config)?);
        Ok(())
    }

    /// Host-only transport injection; remote credentials never come from JS.
    pub fn with_web_broker(&mut self, broker: crate::web::WebBroker) {
        self.web = Some(Arc::new(broker));
    }
    pub fn list(
        &mut self,
        profile: &Profile,
        token: &CancellationToken,
    ) -> Result<Vec<Descriptor>, ToolError> {
        if !self.discovered {
            // Keep discovery atomic: failure drops every child already started.
            let mut clients = BTreeMap::new();
            let mut tools = BTreeMap::new();
            let config = profile.root().join(".mcp.json");
            let raw = if config.exists() {
                let path = profile
                    .check("Read", Access::Read, &config)
                    .map_err(|_| denied())?;
                if !path.starts_with(profile.root()) {
                    return Err(denied());
                }
                let file = std::fs::File::open(path)
                    .map_err(|_| failure("cannot read MCP configuration"))?;
                let mut raw = String::new();
                file.take(1024 * 1024 + 1)
                    .read_to_string(&mut raw)
                    .map_err(|_| failure("cannot read MCP configuration"))?;
                Some(raw)
            } else {
                None
            };
            let servers = mcp::parse(raw.as_deref()).map_err(failure)?;
            for (name, server) in servers {
                if !profile.admits_mcp_server(&name) {
                    continue;
                }
                let mut client = if server.is_remote() {
                    Client::start_remote(
                        &server,
                        self.web.clone().ok_or_else(|| {
                            failure("remote MCP requires configured host web policy")
                        })?,
                        token,
                    )?
                } else {
                    Client::start(profile, &server, token)?
                };
                let protocol = if server.is_remote() {
                    "2025-03-26"
                } else {
                    "2024-11-05"
                };
                let initialized = client.rpc("initialize", json!({"protocolVersion":protocol, "capabilities":{}, "clientInfo":{"name":"pane", "version":env!("CARGO_PKG_VERSION")}}), token)?;
                if initialized.get("protocolVersion").and_then(Value::as_str) != Some(protocol)
                    || !initialized
                        .get("capabilities")
                        .is_some_and(Value::is_object)
                    || !initialized.get("serverInfo").is_some_and(Value::is_object)
                {
                    return Err(failure("invalid MCP initialize result"));
                }
                client.notify("notifications/initialized", json!({}), token)?;
                let mut cursor = None;
                let mut complete = false;
                let mut seen = std::collections::BTreeSet::new();
                for _ in 0..16 {
                    let params = cursor
                        .as_ref()
                        .map_or_else(|| json!({}), |cursor| json!({"cursor":cursor}));
                    let result = client.rpc("tools/list", params, token)?;
                    let entries = result
                        .get("tools")
                        .and_then(Value::as_array)
                        .ok_or_else(|| failure("invalid MCP tools/list result"))?;
                    for entry in entries {
                        let tool = entry
                            .get("name")
                            .and_then(Value::as_str)
                            .filter(|s| {
                                !s.is_empty() && s.len() <= 128 && !s.chars().any(char::is_control)
                            })
                            .ok_or_else(|| failure("invalid MCP tool name"))?;
                        if !seen.insert(tool.to_string()) {
                            return Err(failure("duplicate MCP tool name"));
                        }
                        if seen.len() > MAX_TOOLS {
                            return Err(failure("MCP discovery exceeds 128 tools per server"));
                        }
                        let schema = entry
                            .get("inputSchema")
                            .filter(|s| {
                                s.is_object()
                                    && s.get("type").and_then(Value::as_str) == Some("object")
                            })
                            .ok_or_else(|| failure("invalid MCP tool input schema"))?;
                        if schema.to_string().len() > 32 * 1024 {
                            return Err(failure("MCP tool schema exceeds 32 KiB"));
                        }
                        if registry::mcp_tool_is_absent(tool)
                            || !profile.admits_mcp_tool(&format!("mcp__{name}__{tool}"))
                        {
                            continue;
                        }
                        let descriptor = Descriptor {
                            name: registry::mcp_name(&name, tool),
                            server: name.clone(),
                            tool: tool.into(),
                            description: entry
                                .get("description")
                                .and_then(Value::as_str)
                                .unwrap_or_default()
                                .chars()
                                .take(2048)
                                .collect(),
                            input_schema: schema.clone(),
                        };
                        tools.insert(descriptor.name.clone(), descriptor);
                    }
                    match result.get("nextCursor") {
                        None | Some(Value::Null) => {
                            complete = true;
                            break;
                        }
                        Some(Value::String(next))
                            if next.len() <= 4096 && cursor.as_ref() != Some(next) =>
                        {
                            cursor = Some(next.clone())
                        }
                        _ => return Err(failure("invalid MCP pagination cursor")),
                    }
                }
                if !complete {
                    return Err(failure("MCP discovery exceeds 16 pages"));
                }
                clients.insert(name, client);
            }
            self.clients = clients;
            self.tools = tools;
            self.discovered = true;
        }
        Ok(self
            .tools
            .values()
            .filter(|d| profile.admits_mcp_tool(&format!("mcp__{}__{}", d.server, d.tool)))
            .cloned()
            .collect())
    }

    pub fn call(
        &mut self,
        profile: &Profile,
        token: &CancellationToken,
        name: &str,
        arguments: Value,
    ) -> Result<ToolResult, ToolError> {
        // Only tools already discovered can be called. Unknown names never spawn.
        let descriptor = self.tools.get(name).ok_or_else(denied)?;
        if registry::mcp_tool_is_absent(&descriptor.tool)
            || !profile.admits_mcp_tool(&format!("mcp__{}__{}", descriptor.server, descriptor.tool))
        {
            return Err(denied());
        }
        if !arguments.is_object() {
            return Err(failure("MCP arguments must be a JSON object"));
        }
        let client = self
            .clients
            .get_mut(&descriptor.server)
            .ok_or_else(|| failure("MCP server is unavailable"))?;
        let result = client.rpc(
            "tools/call",
            json!({"name":descriptor.tool, "arguments":arguments}),
            token,
        )?;
        if !result.get("content").is_some_and(Value::is_array)
            || result.get("isError").is_some_and(|v| !v.is_boolean())
        {
            client.stop();
            return Err(failure("invalid MCP tools/call result"));
        }
        // Tool failures are returned as data, preserving MCP's isError contract.
        Ok(ToolResult {
            modified: None,
            tool: name.into(),
            stdout: result.to_string(),
            stderr: String::new(),
            exit_code: Some(0),
            grant: client.grant.clone(),
            confinement: client.confinement,
        })
    }
}

struct Request {
    value: Value,
    reply: bool,
    token: CancellationToken,
}
struct Client {
    child: Option<invoke::ConfinedChild>,
    tx: Option<Sender<Request>>,
    rx: Receiver<Result<Value, &'static str>>,
    next_id: u64,
    grant: ExecGrant,
    confinement: Confinement,
    remote_active: bool,
    worker: Option<std::thread::JoinHandle<()>>,
}
impl Client {
    fn start_remote(
        server: &Server,
        broker: Arc<crate::web::WebBroker>,
        token: &CancellationToken,
    ) -> Result<Self, ToolError> {
        if token.is_cancelled() {
            return Err(ToolError::Cancelled { tool: "mcp".into() });
        }
        let url = server
            .url
            .clone()
            .ok_or_else(|| failure("remote MCP URL is missing"))?;
        broker
            .validate_url(&url)
            .map_err(|_| failure("remote MCP URL violates host web policy"))?;
        let headers = server.remote_headers().map_err(failure)?;
        let (tx, requests) = mpsc::channel::<Request>();
        let (responses, rx) = mpsc::sync_channel(1);
        let worker = std::thread::spawn(move || {
            let mut session_id = None;
            while let Ok(request) = requests.recv() {
                let result = remote_exchange(&broker, &url, &headers, &mut session_id, request);
                let failed = result.is_err();
                if responses.send(result).is_err() || failed {
                    break;
                }
            }
        });
        Ok(Self {
            child: None,
            tx: Some(tx),
            rx,
            next_id: 0,
            grant: ExecGrant {
                binary: PathBuf::new(),
                fell_back_to_roots: false,
            },
            confinement: Confinement::BrokeredNetwork,
            remote_active: true,
            worker: Some(worker),
        })
    }
    fn start(
        profile: &Profile,
        server: &Server,
        token: &CancellationToken,
    ) -> Result<Self, ToolError> {
        if token.is_cancelled() {
            return Err(ToolError::Cancelled { tool: "mcp".into() });
        }
        let executable = server
            .command
            .as_ref()
            .ok_or_else(|| failure("MCP executable is missing"))?;
        let candidate = if executable.contains(['/', '\\']) {
            profile
                .root()
                .join(executable)
                .to_string_lossy()
                .into_owned()
        } else {
            executable.clone()
        };
        let binary = invoke::resolve_program(&candidate)
            .ok_or_else(|| failure("MCP executable is unavailable"))?;
        let grant = ExecGrant {
            binary,
            fell_back_to_roots: false,
        };
        let mut command = Command::new(&grant.binary);
        command.args(&server.args).current_dir(profile.root());
        for (name, _) in std::env::vars_os() {
            if invoke::is_credential_variable(&name.to_string_lossy()) {
                command.env_remove(name);
            }
        }
        // Explicit project env is allowed; ambient provider credentials are not.
        command.envs(&server.env);
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            command.process_group(0);
        }
        if token.is_cancelled() {
            return Err(ToolError::Cancelled { tool: "mcp".into() });
        }
        // One call, because on Windows the confinement *is* the spawn: the
        // AppContainer is an argument to `CreateProcessW` and there is no
        // earlier moment at which a `Command` could carry it. The two
        // outcomes stay distinguishable — a sandbox refusal is `denied`, the
        // operating system declining to start an admitted program is not.
        let (mut child, confinement) = invoke::confined_spawn(
            profile,
            &grant.binary,
            "mcp",
            command,
            invoke::Pipes {
                stdin: true,
                stdout: true,
                stderr: false,
            },
            invoke::LineShape::Argv,
        )
        .map_err(|refusal| match refusal {
            invoke::SpawnRefusal::Denied(_) => denied(),
            invoke::SpawnRefusal::Failed(_) => failure("MCP server could not start"),
        })?;
        let input = child.take_stdin();
        let output = child.take_stdout();
        let (tx, requests) = mpsc::channel::<Request>();
        let (responses, rx) = mpsc::sync_channel(1);
        std::thread::spawn(move || {
            let (Some(mut input), Some(output)) = (input, output) else {
                return;
            };
            let mut output = BufReader::new(output);
            while let Ok(request) = requests.recv() {
                let result = exchange(&mut input, &mut output, request);
                let failed = result.is_err();
                if responses.send(result).is_err() || failed {
                    break;
                }
            }
        });
        Ok(Self {
            child: Some(child),
            tx: Some(tx),
            rx,
            next_id: 0,
            grant,
            confinement,
            remote_active: false,
            worker: None,
        })
    }
    fn rpc(
        &mut self,
        method: &str,
        params: Value,
        token: &CancellationToken,
    ) -> Result<Value, ToolError> {
        self.next_id += 1;
        self.send(Request { value: json!({"jsonrpc":"2.0", "id":self.next_id, "method":method, "params":params}), reply: true, token: token.clone() }, token)
    }
    fn notify(
        &mut self,
        method: &str,
        params: Value,
        token: &CancellationToken,
    ) -> Result<(), ToolError> {
        self.send(
            Request {
                value: json!({"jsonrpc":"2.0", "method":method, "params":params}),
                reply: false,
                token: token.clone(),
            },
            token,
        )
        .map(|_| ())
    }
    fn send(&mut self, request: Request, token: &CancellationToken) -> Result<Value, ToolError> {
        if self.child.is_none() && !self.remote_active {
            return Err(failure("MCP server is unavailable"));
        }
        if request.value.to_string().len() > FRAME_BYTES {
            return Err(failure("MCP request exceeds 8 MiB"));
        }
        if token.is_cancelled() {
            self.stop();
            return Err(ToolError::Cancelled { tool: "mcp".into() });
        }
        if self.tx.as_ref().is_none_or(|tx| tx.send(request).is_err()) {
            self.stop();
            return Err(failure("MCP server disconnected"));
        }
        let started = Instant::now();
        loop {
            if token.is_cancelled() {
                self.stop();
                return Err(ToolError::Cancelled { tool: "mcp".into() });
            }
            if started.elapsed() >= TIMEOUT {
                self.stop();
                return Err(failure("MCP request timed out"));
            }
            match self.rx.recv_timeout(Duration::from_millis(20)) {
                Ok(Ok(value)) => return Ok(value),
                Ok(Err(reason)) => {
                    self.stop();
                    return Err(failure(reason));
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(_) => {
                    self.stop();
                    return Err(failure("MCP server disconnected"));
                }
            }
        }
    }
    fn stop(&mut self) {
        self.remote_active = false;
        self.tx.take();
        if let Some(mut child) = self.child.take() {
            invoke::kill_and_reap(&mut child);
        }
    }
}

fn remote_exchange(
    broker: &crate::web::WebBroker,
    url: &str,
    configured_headers: &BTreeMap<String, String>,
    session_id: &mut Option<String>,
    request: Request,
) -> Result<Value, &'static str> {
    if request.token.is_cancelled() {
        return Err("remote MCP cancelled before dispatch");
    }
    let mut headers = configured_headers.clone();
    if let Some(id) = session_id.as_ref() {
        headers.insert("mcp-session-id".into(), id.clone());
    }
    headers.insert("mcp-protocol-version".into(), "2025-03-26".into());
    let body = serde_json::to_vec(&request.value).map_err(|_| "invalid MCP request")?;
    let response = broker.post_json_cancellable(url, &headers, &body, &request.token).map_err(
        |_| "remote MCP request failed or was denied; no redirect or automatic retry was performed",
    )?;
    let initialize = request.value.get("method").and_then(Value::as_str) == Some("initialize");
    if initialize && let Some(id) = response.session_id {
        if id.is_empty() || id.len() > 4096 || !id.bytes().all(|b| (0x21..=0x7e).contains(&b)) {
            return Err("invalid MCP session ID");
        }
        *session_id = Some(id);
    }
    if !request.reply {
        return if response.status == 202 && response.body.is_empty() {
            Ok(Value::Null)
        } else {
            Err("remote MCP notification must return empty HTTP 202")
        };
    }
    let media = response.content_type.split(';').next().unwrap_or("").trim();
    let messages = match media {
        "application/json" => {
            vec![serde_json::from_slice(&response.body).map_err(|_| "malformed remote MCP JSON")?]
        }
        "text/event-stream" => parse_sse(&response.body)?,
        _ => return Err("remote MCP response must be JSON or finite SSE"),
    };
    let mut answer = None;
    for value in messages {
        let batch = if let Value::Array(values) = value {
            values
        } else {
            vec![value]
        };
        for value in batch {
            if value.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
                return Err("invalid MCP JSON-RPC version");
            }
            if value.get("method").is_some() {
                if value.get("id").is_some() {
                    return Err("MCP server requests are unsupported");
                }
                continue;
            }
            if value.get("id") != request.value.get("id") {
                return Err("MCP reply id mismatch");
            }
            if value.get("error").is_some() {
                return Err("MCP server returned a protocol error");
            }
            if answer.is_some() {
                return Err("duplicate MCP reply");
            }
            answer = Some(
                value
                    .get("result")
                    .cloned()
                    .ok_or("MCP result is missing")?,
            );
        }
    }
    answer.ok_or("remote MCP stream ended without a matching response")
}

/// Finite POST response SSE only: no legacy endpoint negotiation, GET stream,
/// reconnection, event replay or server-initiated request handling.
fn parse_sse(body: &[u8]) -> Result<Vec<Value>, &'static str> {
    let text = std::str::from_utf8(body).map_err(|_| "MCP SSE is not UTF-8")?;
    let mut messages = Vec::new();
    let mut data = String::new();
    for line in text.lines().chain(std::iter::once("")) {
        if line.is_empty() {
            if !data.is_empty() {
                if messages.len() == 64 {
                    return Err("MCP SSE exceeds 64 events");
                }
                messages.push(serde_json::from_str(&data).map_err(|_| "malformed MCP SSE event")?);
                data.clear();
            }
        } else if let Some(value) = line.strip_prefix("data:") {
            data.push_str(value.strip_prefix(' ').unwrap_or(value));
            data.push('\n');
        } else if line == "data" {
            data.push('\n');
        }
    }
    Ok(messages)
}
impl Drop for Client {
    fn drop(&mut self) {
        self.stop();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

fn exchange(
    input: &mut impl Write,
    output: &mut impl BufRead,
    request: Request,
) -> Result<Value, &'static str> {
    serde_json::to_writer(&mut *input, &request.value).map_err(|_| "MCP write failed")?;
    input
        .write_all(b"\n")
        .and_then(|()| input.flush())
        .map_err(|_| "MCP write failed")?;
    if !request.reply {
        return Ok(Value::Null);
    }
    for _ in 0..64 {
        let mut line = Vec::new();
        (&mut *output)
            .take(FRAME_BYTES as u64 + 1)
            .read_until(b'\n', &mut line)
            .map_err(|_| "MCP read failed")?;
        if line.len() > FRAME_BYTES {
            return Err("MCP reply exceeds 8 MiB");
        }
        if line.last() != Some(&b'\n') {
            return Err("MCP server disconnected");
        }
        let value: Value = serde_json::from_slice(&line).map_err(|_| "malformed MCP reply")?;
        if value.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
            return Err("invalid MCP JSON-RPC version");
        }
        if value.get("method").is_some() {
            if value.get("id").is_some() {
                return Err("MCP server requests are unsupported");
            }
            continue;
        }
        if value.get("id") != request.value.get("id") {
            return Err("MCP reply id mismatch");
        }
        if value.get("error").is_some() {
            return Err("MCP server returned a protocol error");
        }
        return value
            .get("result")
            .filter(|r| r.is_object())
            .cloned()
            .ok_or("invalid MCP reply result");
    }
    Err("MCP notification limit exceeded")
}
