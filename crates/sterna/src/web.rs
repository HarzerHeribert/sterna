//! Host-owned, bounded web broker. This does not grant network access to shells.
//! Web content is untrusted data; callers must never promote it to instructions.
use crate::tools::invoke::CancellationToken;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::io::Read;
use std::net::IpAddr;
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};
use ureq::http::Uri;
use ureq::unversioned::resolver::{DefaultResolver, ResolvedSocketAddrs, Resolver};
use ureq::unversioned::transport::{DefaultConnector, NextTimeout};

pub mod search;

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct WebConfig {
    /// On by default. Which hosts `web.fetch` reaches without asking is not
    /// here: it is the session's one list of allowed hosts
    /// (`sandbox.hosts`, the ecosystems, and every host a person allowed),
    /// shared with the command proxy, and a fetch anywhere else asks the
    /// way leaving the sandbox does.
    pub enabled: bool,
    /// Never reached, whatever the allowed hosts say. Bare names match
    /// exactly; `*.example.org` matches subdomains only.
    pub deny_domains: Vec<String>,
    pub allow_http: bool,
    /// SearXNG-compatible JSON endpoint. No implicit search provider or credentials.
    pub search_endpoint: Option<String>,
    /// Which search provider answers `web.search`: `brave` (keyed) or
    /// `searxng` (the endpoint above, no key). Absent means `searxng` when an
    /// endpoint is set and no search otherwise (map 2657).
    pub search_provider: Option<String>,
    /// The **name** of the variable a keyed provider's key is read from —
    /// the process environment first, the gateway's credential file second.
    /// Never a value: a value here would be a key in a project file.
    pub search_key_var: Option<String>,
    pub max_response_bytes: usize,
    pub timeout_seconds: u64,
}

impl Default for WebConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            deny_domains: vec![],
            allow_http: false,
            search_endpoint: None,
            search_provider: None,
            search_key_var: None,
            max_response_bytes: 1_048_576,
            timeout_seconds: 20,
        }
    }
}

impl WebConfig {
    /// Whether `web.search` reaches anything: enabled, with a provider that
    /// has what it needs to be asked — an endpoint for `searxng`, a key
    /// variable's name for `brave` (the key itself is resolved when a query
    /// is made, and a missing key refuses by the variable's name).
    pub fn search_configured(&self) -> bool {
        self.enabled && search::SearchProvider::from_config(self).is_ok_and(|p| p.is_some())
    }

    /// One line for `/config`: how `web.fetch` reaches a host and who
    /// answers `web.search`, or `off`.
    pub fn describe(&self) -> String {
        if !self.configured() {
            return "off".to_string();
        }
        let fetch = "fetch reaches the allowed hosts and asks for others";
        let search = match search::SearchProvider::from_config(self) {
            Ok(Some(provider)) => format!("search via {}", provider.name()),
            _ => "no search".to_string(),
        };
        format!("{fetch} · {search}")
    }

    /// One word for the sidebar's `net:` field: the cell's shell never has a
    /// network, so the field names the host tools instead — `off` when the
    /// broker is, `web` when it is on.
    pub fn posture(&self) -> &'static str {
        if self.configured() { "web" } else { "off" }
    }

    /// Whether the `web` global exists at all — the one predicate
    /// `runtime::bindings::install_web` and the Runtime block both read
    /// (map 2658). `web.fetch` needs nothing more than the broker being on:
    /// a host outside the allowed list asks rather than refusing.
    pub fn configured(&self) -> bool {
        self.enabled
    }
}

/// Whether one host is reached without asking, answered for every hop of a
/// fetch: the first and each redirect.
pub type Reaches = Arc<dyn Fn(&str) -> bool + Send + Sync>;

#[derive(Debug, Serialize)]
pub struct FetchResult {
    pub url: String,
    pub citation: String,
    pub status: u16,
    pub content_type: String,
    pub content: String,
    pub untrusted_content: bool,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct SearchHit {
    pub title: String,
    pub url: String,
    #[serde(default, alias = "content")]
    pub snippet: String,
}

#[derive(Debug, Serialize)]
pub struct SearchResult {
    pub query: String,
    /// Which provider answered: `brave` or `searxng`. In the rollout line,
    /// never the key.
    pub provider: String,
    pub results: Vec<SearchHit>,
    pub citations: Vec<String>,
    pub untrusted_content: bool,
}

/// A trusted transport seam for deterministic embedding/tests. Implementations
/// must bound reads, disable implicit redirects/proxies, and enforce DNS policy.
pub trait WebTransport: Send + Sync {
    fn get(&self, url: &str, max_bytes: usize, timeout: Duration) -> Result<WebResponse, String>;
    /// A GET carrying request headers — a keyed search provider's token.
    /// Without headers it is [`Self::get`]; with them, a transport that does
    /// not send headers refuses rather than sending the request bare, so a
    /// key is never silently dropped on the floor.
    fn get_with_headers(
        &self,
        url: &str,
        headers: &BTreeMap<String, String>,
        max_bytes: usize,
        timeout: Duration,
    ) -> Result<WebResponse, String> {
        if headers.is_empty() {
            return self.get(url, max_bytes, timeout);
        }
        Err("this transport sends no request headers".into())
    }
    fn post(
        &self,
        _url: &str,
        _headers: &BTreeMap<String, String>,
        _body: &[u8],
        _max_bytes: usize,
        _timeout: Duration,
    ) -> Result<WebPostResponse, String> {
        Err("HTTP POST is unsupported by this transport".into())
    }
}

pub struct WebPostResponse {
    pub status: u16,
    pub content_type: String,
    pub session_id: Option<String>,
    pub body: Vec<u8>,
}

pub struct WebResponse {
    pub status: u16,
    pub location: Option<String>,
    pub content_type: String,
    pub body: Vec<u8>,
}

pub struct WebBroker {
    config: WebConfig,
    transport: Arc<dyn WebTransport>,
    workers: Mutex<Vec<std::thread::JoinHandle<()>>>,
}

impl WebBroker {
    /// The configuration this broker was built from.
    pub fn config(&self) -> &WebConfig {
        &self.config
    }

    pub fn fetch_cancellable(
        &self,
        url: &str,
        token: &CancellationToken,
        reaches: Reaches,
    ) -> Result<FetchResult, String> {
        let url = url.to_owned();
        self.on_worker(token, move |broker, token| {
            broker.fetch_inner(&url, &token, &reaches)
        })
    }

    pub fn search_cancellable(
        &self,
        query: &str,
        token: &CancellationToken,
    ) -> Result<SearchResult, String> {
        let query = query.to_owned();
        self.on_worker(token, move |broker, token| {
            broker.search_inner(&query, &token)
        })
    }

    fn on_worker<T: Send + 'static>(
        &self,
        token: &CancellationToken,
        work: impl FnOnce(WebBroker, CancellationToken) -> Result<T, String> + Send + 'static,
    ) -> Result<T, String> {
        if token.is_cancelled() {
            return Err("web request cancelled before dispatch".into());
        }
        let mut workers = self
            .workers
            .lock()
            .map_err(|_| "web worker state unavailable")?;
        let mut pending = Vec::new();
        for worker in workers.drain(..) {
            if worker.is_finished() {
                let _ = worker.join();
            } else {
                pending.push(worker);
            }
        }
        *workers = pending;
        if workers.len() >= 8 {
            return Err(
                "web worker limit reached; cancelled in-flight requests are still finishing".into(),
            );
        }
        let mut config = self.config.clone();
        config.timeout_seconds = config.timeout_seconds.min(10);
        let broker = WebBroker {
            config,
            transport: self.transport.clone(),
            workers: Mutex::new(vec![]),
        };
        let request_token = token.clone();
        let (tx, rx) = mpsc::sync_channel(1);
        workers.push(std::thread::spawn(move || {
            let _ = tx.send(work(broker, request_token));
        }));
        drop(workers);
        loop {
            if token.is_cancelled() {
                return Err("web request cancelled; an in-flight request may finish, and will not be retried".into());
            }
            match rx.recv_timeout(Duration::from_millis(20)) {
                Ok(result) => {
                    return if token.is_cancelled() {
                        Err("web request cancelled".into())
                    } else {
                        result
                    };
                }
                Err(mpsc::RecvTimeoutError::Timeout) => (),
                Err(_) => return Err("web worker stopped".into()),
            }
        }
    }

    /// Called on the MCP worker. Cancellation cannot roll back an in-flight action.
    pub fn post_json_cancellable(
        &self,
        url: &str,
        headers: &BTreeMap<String, String>,
        body: &[u8],
        token: &CancellationToken,
    ) -> Result<WebPostResponse, String> {
        if token.is_cancelled() {
            return Err("remote MCP cancelled before dispatch".into());
        }
        if !self.config.enabled {
            return Err("remote MCP requires [web].enabled".into());
        }
        self.validate_url(url)?;
        if body.len() > self.config.max_response_bytes {
            return Err("web POST request exceeds configured byte limit".into());
        }
        for (name, value) in headers {
            let lower = name.to_ascii_lowercase();
            if name.is_empty()
                || name.len() > 128
                || !name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
                || matches!(
                    lower.as_str(),
                    "host" | "content-length" | "transfer-encoding" | "connection" | "upgrade"
                )
                || lower.starts_with("proxy-")
                || value.len() > 16384
                || !value.bytes().all(|b| (0x20..=0x7e).contains(&b))
            {
                return Err("invalid or forbidden web POST header".into());
            }
        }
        if token.is_cancelled() {
            return Err("remote MCP cancelled before dispatch".into());
        }
        let response = self.transport.post(
            url,
            headers,
            body,
            self.config.max_response_bytes,
            Duration::from_secs(self.config.timeout_seconds.min(10)),
        )?;
        if !(200..300).contains(&response.status) {
            return Err(format!(
                "remote MCP returned HTTP {}; redirects are not followed",
                response.status
            ));
        }
        if response.body.len() > self.config.max_response_bytes {
            return Err("web POST response exceeds configured byte limit".into());
        }
        Ok(response)
    }
    pub fn new(config: WebConfig) -> Result<Self, String> {
        let agent = ureq::Agent::with_parts(
            ureq::Agent::config_builder()
                .proxy(None)
                .max_redirects(0)
                .http_status_as_error(false)
                .timeout_global(Some(Duration::from_secs(
                    config.timeout_seconds.clamp(1, 60),
                )))
                .build(),
            DefaultConnector::default(),
            PublicResolver,
        );
        Self::with_transport(config, Box::new(HttpTransport(agent)))
    }

    pub fn with_transport(
        config: WebConfig,
        transport: Box<dyn WebTransport>,
    ) -> Result<Self, String> {
        if !(1..=8_388_608).contains(&config.max_response_bytes) {
            return Err("web.max_response_bytes must be between 1 and 8388608".into());
        }
        if !(1..=60).contains(&config.timeout_seconds) {
            return Err("web.timeout_seconds must be between 1 and 60".into());
        }
        for pattern in &config.deny_domains {
            let name = pattern.strip_prefix("*.").unwrap_or(pattern);
            if name.is_empty()
                || name.starts_with('.')
                || name.ends_with('.')
                || !name
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'-')
            {
                return Err(format!("invalid web domain pattern: {pattern}"));
            }
        }
        let broker = Self {
            config,
            transport: Arc::from(transport),
            workers: Mutex::new(vec![]),
        };
        if let Some(endpoint) = &broker.config.search_endpoint {
            broker.validate_url(endpoint)?;
        }
        search::SearchProvider::from_config(&broker.config)?;
        Ok(broker)
    }

    /// Scheme, host, private-address and deny-list checks. Whether the host
    /// is reached without asking is the caller's [`Reaches`], not this.
    pub fn validate_url(&self, url: &str) -> Result<Uri, String> {
        if url.len() > 8192 || url.contains(['\\', '\r', '\n', '\t', '#']) {
            return Err("invalid web URL (use an absolute URL without a fragment)".into());
        }
        let uri: Uri = url.parse().map_err(|_| "invalid web URL".to_string())?;
        match uri.scheme_str() {
            Some("https") => (),
            Some("http") if self.config.allow_http => (),
            _ => return Err("web URL must use HTTPS (HTTP requires web.allow_http)".into()),
        }
        let authority = uri.authority().ok_or("web URL requires a host")?;
        if authority.as_str().contains('@') {
            return Err("credentials in web URLs are forbidden".into());
        }
        let host = uri
            .host()
            .ok_or("web URL requires a host")?
            .trim_matches(['[', ']'])
            .to_ascii_lowercase();
        if host.ends_with('.')
            || host.contains('%')
            || host == "localhost"
            || host.ends_with(".localhost")
            || host.ends_with(".local")
            || host.ends_with(".internal")
            || host.parse::<IpAddr>().is_ok_and(|ip| !public_ip(ip))
        {
            return Err("web access to local/private destinations is forbidden".into());
        }
        if self
            .config
            .deny_domains
            .iter()
            .any(|p| domain_matches(p, &host))
        {
            return Err(format!("web domain denied: {host}"));
        }
        Ok(uri)
    }

    /// The host a fetch of `url` would reach first, after every check but
    /// whether it is allowed: what a caller asks a person about.
    pub fn host_of(&self, url: &str) -> Result<String, String> {
        let uri = self.validate_url(url)?;
        Ok(host_name(&uri))
    }

    pub fn fetch(&self, url: &str, reaches: &Reaches) -> Result<FetchResult, String> {
        self.fetch_inner(url, &CancellationToken::new(), reaches)
    }

    /// The model's fetch. Every hop's host answers to `reaches`: the first
    /// one was asked about before this was called, and a redirect to a host
    /// nobody allowed is refused with the URL to fetch, so asking for it
    /// is the model's next call rather than a question mid-redirect.
    fn fetch_inner(
        &self,
        url: &str,
        token: &CancellationToken,
        reaches: &Reaches,
    ) -> Result<FetchResult, String> {
        if !self.config.enabled {
            return Err("web tools are off; `web.enabled` turns them on".into());
        }
        let mut current = url.to_owned();
        let deadline = Instant::now() + Duration::from_secs(self.config.timeout_seconds);
        for hop in 0..=5 {
            if token.is_cancelled() {
                return Err("web request cancelled before dispatch".into());
            }
            let uri = self.validate_url(&current)?;
            let host = host_name(&uri);
            if !reaches(&host) {
                return Err(if hop == 0 {
                    format!("web.fetch refused: {host} is not an allowed host")
                } else {
                    format!(
                        "web.fetch refused: the page redirected to {host}, which is not an allowed \
                         host; fetch {current} to ask for it"
                    )
                });
            }
            let remaining = deadline
                .checked_duration_since(Instant::now())
                .filter(|d| !d.is_zero())
                .ok_or("web request timed out")?;
            if token.is_cancelled() {
                return Err("web request cancelled before dispatch".into());
            }
            let response =
                self.transport
                    .get(&current, self.config.max_response_bytes, remaining)?;
            if response.body.len() > self.config.max_response_bytes {
                return Err("web response exceeds configured byte limit".into());
            }
            if matches!(response.status, 301 | 302 | 303 | 307 | 308) {
                if hop == 5 {
                    return Err("web redirect limit exceeded".into());
                }
                let location = response.location.ok_or("web redirect missing Location")?;
                current = redirect_url(&uri, &location)?;
                continue;
            }
            if !(200..300).contains(&response.status) {
                return Err(format!("web request returned HTTP {}", response.status));
            }
            let media = response.content_type.split(';').next().unwrap_or("").trim();
            if !media.is_empty()
                && !media.starts_with("text/")
                && !matches!(
                    media,
                    "application/json" | "application/xml" | "application/xhtml+xml"
                )
            {
                return Err(format!(
                    "unsupported web content type: {media}; fetch accepts text, HTML, JSON and XML"
                ));
            }
            let content = String::from_utf8(response.body)
                .map_err(|_| "web response is not UTF-8 text".to_string())?;
            return Ok(FetchResult {
                citation: current.clone(),
                url: current,
                status: response.status,
                content_type: response.content_type,
                content,
                untrusted_content: true,
            });
        }
        unreachable!()
    }

    pub fn search(&self, query: &str) -> Result<SearchResult, String> {
        self.search_inner(query, &CancellationToken::new())
    }

    fn search_inner(&self, query: &str, token: &CancellationToken) -> Result<SearchResult, String> {
        if !self.config.enabled {
            return Err("web tools are off; `web.enabled` turns them on".into());
        }
        if query.trim().is_empty() || query.len() > 4096 {
            return Err("web search query must be 1..4096 bytes".into());
        }
        search::run(self, query, token)
    }

    /// One GET to a destination the user configured, with request headers:
    /// the URL validated like every other, the status and the byte bound
    /// checked, no redirect followed — a search endpoint that redirects is a
    /// misconfiguration, not a hop to take with a key in the headers.
    fn get_endpoint(
        &self,
        url: &str,
        headers: &BTreeMap<String, String>,
        token: &CancellationToken,
    ) -> Result<WebResponse, String> {
        if token.is_cancelled() {
            return Err("web request cancelled before dispatch".into());
        }
        self.validate_url(url)?;
        let response = self.transport.get_with_headers(
            url,
            headers,
            self.config.max_response_bytes,
            Duration::from_secs(self.config.timeout_seconds),
        )?;
        if response.body.len() > self.config.max_response_bytes {
            return Err("web response exceeds configured byte limit".into());
        }
        if !(200..300).contains(&response.status) {
            return Err(format!(
                "web search returned HTTP {}; redirects are not followed",
                response.status
            ));
        }
        Ok(response)
    }
}

impl Drop for WebBroker {
    fn drop(&mut self) {
        let workers = self
            .workers
            .get_mut()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        for worker in workers.drain(..) {
            let _ = worker.join();
        }
    }
}

/// A URI's host as the policy compares it: lower case, IPv6 brackets off.
fn host_name(uri: &Uri) -> String {
    uri.host()
        .unwrap_or_default()
        .trim_matches(['[', ']'])
        .to_ascii_lowercase()
}

fn domain_matches(pattern: &str, host: &str) -> bool {
    let pattern = pattern.to_ascii_lowercase();
    if let Some(suffix) = pattern.strip_prefix("*.") {
        host.len() > suffix.len() + 1 && host.ends_with(&format!(".{suffix}"))
    } else {
        host == pattern
    }
}

fn encode(value: &str) -> String {
    value
        .bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() || b"-._~".contains(&b) {
                (b as char).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect()
}

fn redirect_url(base: &Uri, location: &str) -> Result<String, String> {
    if location.starts_with("https://") || location.starts_with("http://") {
        return Ok(location.into());
    }
    if location.starts_with("//") {
        return Ok(format!(
            "{}:{location}",
            base.scheme_str().unwrap_or("https")
        ));
    }
    if location.contains(':') || location.is_empty() {
        return Err("unsupported web redirect URL".into());
    }
    let origin = format!(
        "{}://{}",
        base.scheme_str().unwrap_or("https"),
        base.authority().ok_or("missing host")?
    );
    if location.starts_with('/') {
        return Ok(format!("{origin}{location}"));
    }
    if location.starts_with('?') {
        return Ok(format!("{origin}{}{location}", base.path()));
    }
    let directory = base
        .path()
        .rsplit_once('/')
        .map(|(dir, _)| dir)
        .unwrap_or("");
    Ok(format!("{origin}{directory}/{location}"))
}

/// Conservative global-unicast filter, including mapped IPv4 and translation ranges.
pub fn public_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v) => {
            let [a, b, c, _] = v.octets();
            !(v.is_private()
                || v.is_loopback()
                || v.is_link_local()
                || v.is_broadcast()
                || v.is_documentation()
                || a == 0
                || a >= 224
                || (a == 100 && (64..=127).contains(&b))
                || (a == 192 && b == 0 && c == 0)
                || (a == 198 && (b == 18 || b == 19)))
        }
        IpAddr::V6(v) => {
            if let Some(v4) = v.to_ipv4_mapped() {
                return public_ip(IpAddr::V4(v4));
            }
            let s = v.segments();
            // Only ordinary IPv6 global unicast; exclude documentation, Teredo,
            // 6to4, benchmarking and transition networks that embed IPv4 targets.
            (s[0] & 0xe000) == 0x2000
                && !(s[0] == 0x2001 && (s[1] < 0x0200 || s[1] == 0x0db8))
                && s[0] != 0x2002
                && !(s[0] == 0x3fff && s[1] < 0x1000)
        }
    }
}

#[derive(Debug)]
struct PublicResolver;
impl Resolver for PublicResolver {
    fn resolve(
        &self,
        uri: &Uri,
        config: &ureq::config::Config,
        timeout: NextTimeout,
    ) -> Result<ResolvedSocketAddrs, ureq::Error> {
        let resolved = DefaultResolver::default().resolve(uri, config, timeout)?;
        // Check the exact addresses handed to the connector: no second DNS
        // resolution and therefore no validation/use DNS rebinding race.
        if resolved.iter().any(|addr| !public_ip(addr.ip())) {
            return Err(ureq::Error::Other(
                std::io::Error::new(
                    std::io::ErrorKind::PermissionDenied,
                    "web DNS resolved to a local/private destination",
                )
                .into(),
            ));
        }
        Ok(resolved)
    }
}

struct HttpTransport(ureq::Agent);
impl WebTransport for HttpTransport {
    fn post(
        &self,
        url: &str,
        headers: &BTreeMap<String, String>,
        body: &[u8],
        max_bytes: usize,
        timeout: Duration,
    ) -> Result<WebPostResponse, String> {
        let mut request = self
            .0
            .post(url)
            .header("User-Agent", "Sterna-MCP/1.0")
            .header("Accept", "application/json, text/event-stream")
            .header("Content-Type", "application/json");
        for (name, value) in headers {
            request = request.header(name.as_str(), value.as_str());
        }
        let mut response = request
            .config()
            .timeout_global(Some(timeout))
            .build()
            .send(body)
            .map_err(|_| "remote MCP HTTP request failed".to_string())?;
        let status = response.status().as_u16();
        let content_type = response
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_owned();
        let session_id = response
            .headers()
            .get("mcp-session-id")
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned);
        let mut body = Vec::new();
        if (200..300).contains(&status) {
            response
                .body_mut()
                .as_reader()
                .take(max_bytes as u64 + 1)
                .read_to_end(&mut body)
                .map_err(|_| "remote MCP HTTP body read failed".to_string())?;
        }
        Ok(WebPostResponse {
            status,
            content_type,
            session_id,
            body,
        })
    }
    fn get(&self, url: &str, max_bytes: usize, timeout: Duration) -> Result<WebResponse, String> {
        self.get_with_headers(url, &BTreeMap::new(), max_bytes, timeout)
    }
    fn get_with_headers(
        &self,
        url: &str,
        headers: &BTreeMap<String, String>,
        max_bytes: usize,
        timeout: Duration,
    ) -> Result<WebResponse, String> {
        let mut request = self
            .0
            .get(url)
            .header("User-Agent", "Sterna-Web/1.0")
            .header(
                "Accept",
                "text/html, text/plain, application/json, application/xml",
            );
        for (name, value) in headers {
            request = request.header(name.as_str(), value.as_str());
        }
        let mut response = request
            .config()
            .timeout_global(Some(timeout))
            .build()
            .call()
            .map_err(|e| format!("web request failed: {e}"))?;
        let status = response.status().as_u16();
        let location = response
            .headers()
            .get("location")
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned);
        let content_type = response
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_owned();
        let mut body = Vec::new();
        if !(300..400).contains(&status) {
            response
                .body_mut()
                .as_reader()
                .take(max_bytes as u64 + 1)
                .read_to_end(&mut body)
                .map_err(|e| format!("web body read failed: {e}"))?;
        }
        Ok(WebResponse {
            status,
            location,
            content_type,
            body,
        })
    }
}
