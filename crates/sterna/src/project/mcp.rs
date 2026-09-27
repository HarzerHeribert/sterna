//! Bounded parsing of the project's existing `.mcp.json` bytes.
use std::collections::BTreeMap;

use serde::Deserialize;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Config {
    mcp_servers: BTreeMap<String, Server>,
}

/// Explicit transport configuration. No Debug: argv, headers and env may contain secrets.
#[derive(Deserialize)]
pub struct Server {
    #[serde(rename = "type", default)]
    transport: Option<String>,
    pub command: Option<String>,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    pub url: Option<String>,
    #[serde(default)]
    pub headers: BTreeMap<String, String>,
}

impl Server {
    pub fn is_remote(&self) -> bool {
        matches!(self.transport.as_deref(), Some("http" | "streamable-http"))
    }

    /// Expand only explicit server.env values, never the process environment.
    pub fn remote_headers(&self) -> Result<BTreeMap<String, String>, &'static str> {
        let mut result = BTreeMap::new();
        for (name, value) in &self.headers {
            let mut expanded = String::new();
            let mut rest = value.as_str();
            while let Some((before, tail)) = rest.split_once("${") {
                expanded.push_str(before);
                let (variable, after) =
                    tail.split_once('}').ok_or("invalid MCP header variable")?;
                expanded.push_str(
                    self.env
                        .get(variable)
                        .ok_or("MCP header variable is absent from explicit server env")?,
                );
                rest = after;
            }
            expanded.push_str(rest);
            if expanded.len() > 16384 || !expanded.bytes().all(|b| (0x20..=0x7e).contains(&b)) {
                return Err("invalid MCP header value");
            }
            result.insert(name.to_ascii_lowercase(), expanded);
        }
        Ok(result)
    }
}

/// Parse without returning source text or serde diagnostics to the model.
pub fn parse(raw: Option<&str>) -> Result<BTreeMap<String, Server>, &'static str> {
    let Some(raw) = raw else {
        return Ok(BTreeMap::new());
    };
    if raw.len() > 1024 * 1024 {
        return Err("MCP configuration exceeds 1 MiB");
    }
    let config: Config = serde_json::from_str(raw).map_err(|_| "invalid MCP configuration")?;
    if config.mcp_servers.len() > 16 {
        return Err("MCP configuration exceeds 16 servers");
    }
    let mut servers = BTreeMap::new();
    for (name, server) in config.mcp_servers {
        if server.transport.as_deref() == Some("sse") {
            return Err(
                "legacy MCP SSE transport is unsupported; configure Streamable HTTP with type http",
            );
        }
        if !matches!(
            server.transport.as_deref(),
            None | Some("stdio" | "http" | "streamable-http")
        ) {
            continue;
        }
        if name.is_empty()
            || name.len() > 128
            || name.contains("__")
            || !name
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
            || (!server.is_remote()
                && server
                    .command
                    .as_ref()
                    .is_none_or(|s| s.is_empty() || s.len() > 4096 || s.contains('\0')))
            || (server.is_remote()
                && (server.command.is_some()
                    || server
                        .url
                        .as_ref()
                        .is_none_or(|url| url.is_empty() || url.len() > 8192)))
            || server.headers.len() > 64
            || server.headers.keys().any(|name| {
                name.is_empty()
                    || name.len() > 128
                    || !name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
                    || matches!(
                        name.to_ascii_lowercase().as_str(),
                        "host"
                            | "content-length"
                            | "transfer-encoding"
                            | "connection"
                            | "accept"
                            | "content-type"
                            | "mcp-session-id"
                            | "mcp-protocol-version"
                    )
            })
            || server.args.len() > 64
            || server.env.len() > 64
            || server
                .args
                .iter()
                .any(|s| s.len() > 16 * 1024 || s.contains('\0'))
            || server.env.iter().any(|(k, v)| {
                k.is_empty()
                    || k.contains(['=', '\0'])
                    || k.len() > 256
                    || v.contains('\0')
                    || v.len() > 16 * 1024
            })
        {
            return Err("invalid MCP server configuration");
        }
        server.remote_headers()?;
        servers.insert(name, server);
    }
    Ok(servers)
}
