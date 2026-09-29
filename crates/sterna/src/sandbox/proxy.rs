//! The host-side HTTP proxy a sandboxed command reaches the network through.
//!
//! A sandboxed child gets `HTTPS_PROXY`, `HTTP_PROXY` and `ALL_PROXY` (and
//! their lowercase and npm forms) pointing here, and `NO_PROXY` for loopback
//! only. Package managers, git over https and tool installers all honour
//! those variables, so the one place a name is checked is this proxy: a host
//! on the [`Allowed`] list is tunnelled, anything else gets a `403` whose body
//! says what to change, and the refusal is recorded so the session can offer
//! to allow it.
//!
//! The check is on the *name* the client asked for, never on a resolved
//! address: allowing `crates.io` must not quietly allow whatever else shares
//! its CDN address, and an IP literal is refused unless it is listed exactly.
//!
//! Std only, one thread per connection. There is no async runtime in this
//! crate on purpose, and a proxy for one person's package installs sees tens
//! of connections, not thousands.

use base64::Engine as _;
use std::collections::BTreeSet;
use std::io::{self, Read, Write};
use std::net::{IpAddr, Shutdown, SocketAddr, TcpListener, TcpStream, ToSocketAddrs};
use std::path::Path;
#[cfg(unix)]
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::thread::JoinHandle;
use std::time::Duration;

/// One package ecosystem: the hosts its clients fetch from. The table is what
/// Settings › Sandbox shows as switches, so `label` and `clients` are copy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Ecosystem {
    /// The stable word saved in settings.
    pub name: &'static str,
    /// What the settings sheet shows.
    pub label: &'static str,
    pub hosts: &'static [&'static str],
    /// The tools that use these hosts, shown beside the label.
    pub clients: &'static str,
}

/// The default table. Every ecosystem is on by default; the hosts are the
/// registries and download hosts only, never a whole CDN or cloud domain.
pub const ECOSYSTEMS: &[Ecosystem] = &[
    Ecosystem {
        name: "rust",
        label: "Rust",
        hosts: &[
            "crates.io",
            "index.crates.io",
            "static.crates.io",
            "static.rust-lang.org",
        ],
        clients: "cargo, rustup, cargo-binstall",
    },
    Ecosystem {
        name: "javascript",
        label: "JavaScript",
        hosts: &["registry.npmjs.org", "registry.yarnpkg.com", "nodejs.org"],
        clients: "npm, pnpm, yarn, bun, corepack",
    },
    Ecosystem {
        name: "deno",
        label: "Deno, JSR",
        hosts: &["jsr.io", "deno.land", "dl.deno.land"],
        clients: "deno",
    },
    Ecosystem {
        name: "python",
        label: "Python",
        hosts: &["pypi.org", "files.pythonhosted.org"],
        clients: "pip, uv, poetry, pdm, pipx",
    },
    Ecosystem {
        name: "go",
        label: "Go",
        hosts: &["proxy.golang.org", "sum.golang.org"],
        clients: "go",
    },
    Ecosystem {
        name: "java",
        label: "Java, Kotlin",
        hosts: &[
            "repo.maven.apache.org",
            "repo1.maven.org",
            "plugins.gradle.org",
            "services.gradle.org",
        ],
        clients: "maven, gradle, sbt",
    },
    Ecosystem {
        name: "ruby",
        label: "Ruby",
        hosts: &["rubygems.org", "index.rubygems.org"],
        clients: "gem, bundler",
    },
    Ecosystem {
        name: "dotnet",
        label: ".NET",
        hosts: &["api.nuget.org"],
        clients: "dotnet, nuget",
    },
    Ecosystem {
        name: "php",
        label: "PHP",
        hosts: &["repo.packagist.org"],
        clients: "composer",
    },
    Ecosystem {
        name: "source",
        label: "Source hosts",
        hosts: &[
            "github.com",
            "codeload.github.com",
            "objects.githubusercontent.com",
            "raw.githubusercontent.com",
            "gitlab.com",
        ],
        clients: "git over https, tool installers",
    },
];

/// Whether a confined command on this machine can reach the proxy at all:
/// through the network namespace's relay on Linux, when the kernel lets
/// Sterna build one, and over loopback on macOS. Elsewhere commands have no
/// network and the proxy is not started.
#[cfg(target_os = "linux")]
pub fn reachable() -> bool {
    super::linux_ns::available()
}

#[cfg(target_os = "macos")]
pub fn reachable() -> bool {
    true
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub fn reachable() -> bool {
    false
}

/// Whether `host` is a host name this module will store: letters, digits,
/// dots and hyphens, optionally led by `*.` for "any subdomain of". No
/// scheme, port, path or spaces — those are what a pasted URL brings, and a
/// silent partial match on one would allow the wrong thing. One trailing dot
/// (the fully qualified form) is accepted.
pub fn valid_host(host: &str) -> bool {
    let bare = host.strip_suffix('.').unwrap_or(host);
    let bare = bare.strip_prefix("*.").unwrap_or(bare);
    if bare.is_empty() || bare.len() > 253 {
        return false;
    }
    bare.split('.').all(|label| {
        !label.is_empty()
            && label.len() <= 63
            && !label.starts_with('-')
            && !label.ends_with('-')
            && label
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-')
    })
}

/// Lowercase, one trailing dot dropped, IPv6 brackets removed.
fn normalize(host: &str) -> String {
    let host = host.trim();
    let host = host
        .strip_prefix('[')
        .and_then(|h| h.strip_suffix(']'))
        .unwrap_or(host);
    host.strip_suffix('.').unwrap_or(host).to_ascii_lowercase()
}

#[derive(Debug, Default)]
struct HostSet {
    exact: BTreeSet<String>,
    /// `*.example.com` stored as `example.com`.
    suffixes: BTreeSet<String>,
}

impl HostSet {
    fn insert(&mut self, host: &str) -> bool {
        if !valid_host(host) {
            return false;
        }
        let host = normalize(host);
        match host.strip_prefix("*.") {
            Some(suffix) => self.suffixes.insert(suffix.to_string()),
            None => self.exact.insert(host),
        };
        true
    }
}

/// The hosts a sandboxed command may reach. Cloning shares the set, so the
/// running [`Proxy`] sees a host [`Allowed::add`]ed mid-session on its next
/// connection without a restart.
#[derive(Debug, Clone, Default)]
pub struct Allowed {
    inner: Arc<RwLock<HostSet>>,
}

impl Allowed {
    /// The hosts of every ecosystem named in `ecosystems` (unknown names are
    /// skipped: a settings file from a newer build must still load) plus the
    /// valid entries of `extra`.
    pub fn new(ecosystems: &[String], extra: &[String]) -> Self {
        let mut set = HostSet::default();
        for eco in ECOSYSTEMS
            .iter()
            .filter(|e| ecosystems.iter().any(|n| n == e.name))
        {
            for host in eco.hosts {
                set.insert(host);
            }
        }
        for host in extra {
            set.insert(host);
        }
        Self {
            inner: Arc::new(RwLock::new(set)),
        }
    }

    /// Every ecosystem on, no extra hosts.
    pub fn defaults() -> Self {
        let names: Vec<String> = ECOSYSTEMS.iter().map(|e| e.name.to_string()).collect();
        Self::new(&names, &[])
    }

    /// Whether a connection to `host` goes through. Exact, case-insensitive
    /// name match; a `*.` entry matches strictly deeper names only. An IP
    /// literal matches only an identical exact entry, never a wildcard.
    pub fn permits(&self, host: &str) -> bool {
        let host = normalize(host);
        if host.is_empty() {
            return false;
        }
        let set = self.inner.read().unwrap_or_else(|e| e.into_inner());
        if set.exact.contains(&host) {
            return true;
        }
        if host.parse::<IpAddr>().is_ok() {
            return false;
        }
        set.suffixes.iter().any(|suffix| {
            host.len() > suffix.len() + 1
                && host.ends_with(suffix.as_str())
                && host.as_bytes()[host.len() - suffix.len() - 1] == b'.'
        })
    }

    /// Allow `host` from now on. Returns `false`, and changes nothing, when
    /// the string is not a [`valid_host`].
    pub fn add(&self, host: &str) -> bool {
        self.inner
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .insert(host)
    }

    /// Stop allowing `host`, written as it was added (`*.name` for a
    /// wildcard). Returns whether it was allowed. A connection already open
    /// finishes; the next one is refused.
    pub fn remove(&self, host: &str) -> bool {
        let host = normalize(host);
        let mut set = self.inner.write().unwrap_or_else(|e| e.into_inner());
        match host.strip_prefix("*.") {
            Some(suffix) => set.suffixes.remove(suffix),
            None => set.exact.remove(&host),
        }
    }

    /// Every allowed entry, sorted, wildcards written `*.name`.
    pub fn hosts(&self) -> Vec<String> {
        let set = self.inner.read().unwrap_or_else(|e| e.into_inner());
        let mut all: Vec<String> = set.exact.iter().cloned().collect();
        all.extend(set.suffixes.iter().map(|s| format!("*.{s}")));
        all.sort();
        all
    }
}

/// How the proxy reaches an allowed host. A test's `resolve` maps a name to a
/// local fake upstream; production resolves the name itself.
type Resolver = Arc<dyn Fn(&str, u16) -> Option<SocketAddr> + Send + Sync>;

#[derive(Clone, Default)]
struct Route {
    /// An outer proxy the host itself sits behind, read once at start.
    outer: Option<OuterProxy>,
    resolve: Option<Resolver>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct OuterProxy {
    /// `host:port`.
    addr: String,
    /// A ready `Proxy-Authorization` value when the URL carried credentials.
    auth: Option<String>,
}

impl OuterProxy {
    /// Read `HTTPS_PROXY`/`https_proxy`. Only a plain `http://` proxy (or a
    /// bare `host:port`) is chained through; anything else is ignored, and
    /// the proxy connects directly.
    fn from_env() -> Option<Self> {
        ["HTTPS_PROXY", "https_proxy"]
            .iter()
            .filter_map(|k| std::env::var(k).ok())
            .find(|v| !v.trim().is_empty())
            .and_then(|v| Self::parse(&v))
    }

    fn parse(url: &str) -> Option<Self> {
        let url = url.trim();
        let rest = match url.split_once("://") {
            Some((scheme, rest)) if scheme.eq_ignore_ascii_case("http") => rest,
            Some(_) => return None,
            None => url,
        };
        let authority = rest.split('/').next().unwrap_or("");
        let (userinfo, hostport) = match authority.rsplit_once('@') {
            Some((u, h)) => (Some(u), h),
            None => (None, authority),
        };
        if hostport.is_empty() {
            return None;
        }
        let addr = if hostport.rsplit_once(':').is_some_and(|(_, p)| {
            !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()) && !hostport.ends_with(']')
        }) {
            hostport.to_string()
        } else {
            format!("{hostport}:80")
        };
        let auth = userinfo.map(|u| {
            let decoded = percent_decode(u);
            format!(
                "Basic {}",
                base64::engine::general_purpose::STANDARD.encode(decoded)
            )
        });
        Some(Self { addr, auth })
    }
}

fn percent_decode(s: &str) -> Vec<u8> {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let escaped = (bytes[i] == b'%')
            .then(|| s.get(i + 1..i + 3))
            .flatten()
            .and_then(|h| u8::from_str_radix(h, 16).ok());
        if let Some(b) = escaped {
            out.push(b);
            i += 3;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    out
}

/// Distinct refused hosts, most recent last.
#[derive(Debug, Default)]
struct Refusals(Mutex<Vec<String>>);

impl Refusals {
    fn record(&self, host: &str) {
        let mut list = self.0.lock().unwrap_or_else(|e| e.into_inner());
        list.retain(|h| h != host);
        list.push(host.to_string());
    }
}

/// A request head is bounded: nothing a package manager sends comes close.
const MAX_HEAD: usize = 16 * 1024;
/// How long a client may take to send its request head.
const HEAD_TIMEOUT: Duration = Duration::from_secs(30);
/// How long a tunnel may sit with no bytes in one direction before it is
/// closed. Long enough for a slow registry, short enough that a dead
/// connection does not hold a thread forever.
const IDLE_TIMEOUT: Duration = Duration::from_secs(300);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(20);

struct Shared {
    allowed: Allowed,
    refused: Refusals,
    route: Route,
    stop: AtomicBool,
}

/// The running proxy. Dropping it stops accepting new connections and
/// removes the socket directory; a tunnel already open finishes on its own.
pub struct Proxy {
    shared: Arc<Shared>,
    port: u16,
    #[cfg(unix)]
    /// The socket's path; its parent is the private directory.
    unix: Option<(PathBuf, JoinHandle<()>)>,
    tcp: Option<JoinHandle<()>>,
}

impl Proxy {
    /// Bind `127.0.0.1:0` (and, on unix, a socket in a fresh private temp
    /// directory) and start accepting. An outer proxy in this process's own
    /// `HTTPS_PROXY` is read here, once, and chained through.
    pub fn start(allowed: Allowed) -> io::Result<Proxy> {
        Self::start_with(
            allowed,
            Route {
                outer: OuterProxy::from_env(),
                resolve: None,
            },
        )
    }

    fn start_with(allowed: Allowed, route: Route) -> io::Result<Proxy> {
        let shared = Arc::new(Shared {
            allowed,
            refused: Refusals::default(),
            route,
            stop: AtomicBool::new(false),
        });
        let listener = TcpListener::bind(("127.0.0.1", 0))?;
        let port = listener.local_addr()?.port();
        #[cfg(unix)]
        let unix = Some(unix_listener::start(&shared)?);
        let tcp = {
            let shared = Arc::clone(&shared);
            std::thread::Builder::new()
                .name("sandbox-proxy".into())
                .spawn(move || {
                    for stream in listener.incoming() {
                        if shared.stop.load(Ordering::SeqCst) {
                            break;
                        }
                        if let Ok(stream) = stream {
                            spawn_client(&shared, Client::Tcp(stream));
                        }
                    }
                })?
        };
        Ok(Proxy {
            shared,
            port,
            #[cfg(unix)]
            unix,
            tcp: Some(tcp),
        })
    }

    pub fn port(&self) -> u16 {
        self.port
    }

    /// The Unix-domain socket's path, for a child in its own network namespace that
    /// cannot see the host's loopback. `None` off unix.
    pub fn unix_path(&self) -> Option<&Path> {
        #[cfg(unix)]
        {
            self.unix.as_ref().map(|(socket, _)| socket.as_path())
        }
        #[cfg(not(unix))]
        {
            None
        }
    }

    /// The environment a sandboxed child gets: every spelling of the proxy
    /// variable its tools might read, and loopback exempted.
    pub fn env(&self) -> Vec<(String, String)> {
        let url = format!("http://127.0.0.1:{}", self.port);
        let mut env: Vec<(String, String)> = [
            "HTTPS_PROXY",
            "HTTP_PROXY",
            "ALL_PROXY",
            "https_proxy",
            "http_proxy",
            "all_proxy",
            "npm_config_proxy",
            "npm_config_https_proxy",
        ]
        .iter()
        .map(|k| (k.to_string(), url.clone()))
        .collect();
        let no_proxy = "localhost,127.0.0.1,::1".to_string();
        env.push(("NO_PROXY".into(), no_proxy.clone()));
        env.push(("no_proxy".into(), no_proxy));
        env
    }

    /// The live list this proxy checks: a host added to it is let through
    /// on the next connection.
    pub fn allowed(&self) -> Allowed {
        self.shared.allowed.clone()
    }

    /// Distinct hosts refused since start, most recent last.
    pub fn refused(&self) -> Vec<String> {
        self.shared
            .refused
            .0
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// [`Proxy::refused`], clearing the list.
    pub fn take_refused(&self) -> Vec<String> {
        std::mem::take(
            &mut *self
                .shared
                .refused
                .0
                .lock()
                .unwrap_or_else(|e| e.into_inner()),
        )
    }
}

impl Drop for Proxy {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::SeqCst);
        // `accept` has no timeout; one connection of our own wakes it to see
        // the flag. A wake-up that cannot connect -- a sandbox around this
        // process refusing it, a descriptor limit -- leaves that thread in
        // `accept` for good, so it is joined only when it was woken and is
        // otherwise left to end with the process. Measured 2026-09-29: under
        // a seatbelt refusing local-socket connects, every session hung at
        // exit in the unconditional join.
        if TcpStream::connect(("127.0.0.1", self.port)).is_ok()
            && let Some(handle) = self.tcp.take()
        {
            let _ = handle.join();
        }
        #[cfg(unix)]
        if let Some((socket, handle)) = self.unix.take() {
            if std::os::unix::net::UnixStream::connect(&socket).is_ok() {
                let _ = handle.join();
            }
            if let Some(dir) = socket.parent() {
                let _ = std::fs::remove_dir_all(dir);
            }
        }
    }
}

#[cfg(unix)]
mod unix_listener {
    use super::{Client, Shared, spawn_client};
    use std::io;
    use std::os::unix::fs::DirBuilderExt as _;
    use std::os::unix::net::UnixListener;
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::thread::JoinHandle;

    const SOCKET: &str = "proxy.sock";

    /// A fresh `0700` directory, so only this user can reach the socket, and
    /// a short name, because a socket path is limited to about 104 bytes.
    pub(super) fn start(shared: &Arc<Shared>) -> io::Result<(PathBuf, JoinHandle<()>)> {
        static SEQ: AtomicU32 = AtomicU32::new(0);
        let dir = loop {
            let candidate = std::env::temp_dir().join(format!(
                "sterna-px-{}-{}",
                std::process::id(),
                SEQ.fetch_add(1, Ordering::Relaxed)
            ));
            match std::fs::DirBuilder::new().mode(0o700).create(&candidate) {
                Ok(()) => break candidate,
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(e) => return Err(e),
            }
        };
        let socket = dir.join(SOCKET);
        let listener = match UnixListener::bind(&socket) {
            Ok(l) => l,
            Err(e) => {
                let _ = std::fs::remove_dir_all(&dir);
                return Err(e);
            }
        };
        let shared = Arc::clone(shared);
        let handle = std::thread::Builder::new()
            .name("sandbox-proxy-unix".into())
            .spawn(move || {
                for stream in listener.incoming() {
                    if shared.stop.load(Ordering::SeqCst) {
                        break;
                    }
                    if let Ok(stream) = stream {
                        spawn_client(&shared, Client::Unix(stream));
                    }
                }
            });
        match handle {
            Ok(h) => Ok((socket, h)),
            Err(e) => {
                let _ = std::fs::remove_dir_all(&dir);
                Err(e)
            }
        }
    }
}

/// The client side of one connection, over TCP or a Unix socket.
enum Client {
    Tcp(TcpStream),
    #[cfg(unix)]
    Unix(std::os::unix::net::UnixStream),
}

impl Client {
    fn try_clone(&self) -> io::Result<Client> {
        match self {
            Client::Tcp(s) => s.try_clone().map(Client::Tcp),
            #[cfg(unix)]
            Client::Unix(s) => s.try_clone().map(Client::Unix),
        }
    }

    fn set_read_timeout(&self, t: Option<Duration>) -> io::Result<()> {
        match self {
            Client::Tcp(s) => s.set_read_timeout(t),
            #[cfg(unix)]
            Client::Unix(s) => s.set_read_timeout(t),
        }
    }

    fn shutdown(&self, how: Shutdown) {
        let _ = match self {
            Client::Tcp(s) => s.shutdown(how),
            #[cfg(unix)]
            Client::Unix(s) => s.shutdown(how),
        };
    }
}

impl Read for Client {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match self {
            Client::Tcp(s) => s.read(buf),
            #[cfg(unix)]
            Client::Unix(s) => s.read(buf),
        }
    }
}

impl Write for Client {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        match self {
            Client::Tcp(s) => s.write(buf),
            #[cfg(unix)]
            Client::Unix(s) => s.write(buf),
        }
    }
    fn flush(&mut self) -> io::Result<()> {
        match self {
            Client::Tcp(s) => s.flush(),
            #[cfg(unix)]
            Client::Unix(s) => s.flush(),
        }
    }
}

fn spawn_client(shared: &Arc<Shared>, client: Client) {
    let shared = Arc::clone(shared);
    let _ = std::thread::Builder::new()
        .name("sandbox-proxy-conn".into())
        .spawn(move || {
            let _ = serve(&shared, client);
        });
}

/// Read up to the blank line ending a request or response head. Returns the
/// head and whatever body bytes arrived with it.
fn read_head(stream: &mut impl Read) -> io::Result<(Vec<u8>, Vec<u8>)> {
    let mut buf = Vec::with_capacity(1024);
    let mut chunk = [0u8; 4096];
    loop {
        if let Some(end) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            let rest = buf.split_off(end + 4);
            return Ok((buf, rest));
        }
        if buf.len() > MAX_HEAD {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "request head too large",
            ));
        }
        let n = stream.read(&mut chunk)?;
        if n == 0 {
            return Err(io::ErrorKind::UnexpectedEof.into());
        }
        buf.extend_from_slice(&chunk[..n]);
    }
}

fn respond(client: &mut Client, status: &str, body: &str) -> io::Result<()> {
    let msg = format!(
        "HTTP/1.1 {status}\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    client.write_all(msg.as_bytes())?;
    client.flush()
}

/// Split `host:port`, `[v6]:port` or a bare host (`default` port).
fn split_host_port(authority: &str, default: u16) -> Option<(String, u16)> {
    if let Some(rest) = authority.strip_prefix('[') {
        let (host, after) = rest.split_once(']')?;
        let port = match after.strip_prefix(':') {
            Some(p) => p.parse().ok()?,
            None if after.is_empty() => default,
            None => return None,
        };
        return Some((host.to_string(), port));
    }
    match authority.rsplit_once(':') {
        Some((host, port)) => Some((host.to_string(), port.parse().ok()?)),
        None => Some((authority.to_string(), default)),
    }
}

/// What one request asks for.
struct Target {
    host: String,
    port: u16,
    /// For absolute-form requests: the origin-form path to forward.
    path: Option<String>,
}

fn parse_target(method: &str, target: &str) -> Option<Target> {
    if method.eq_ignore_ascii_case("CONNECT") {
        let (host, port) = split_host_port(target, 443)?;
        return Some(Target {
            host,
            port,
            path: None,
        });
    }
    let (scheme, rest) = target.split_once("://")?;
    if !scheme.eq_ignore_ascii_case("http") {
        return None;
    }
    let (authority, path) = match rest.find(['/', '?']) {
        Some(i) if rest.as_bytes()[i] == b'/' => (&rest[..i], rest[i..].to_string()),
        Some(i) => (&rest[..i], format!("/{}", &rest[i..])),
        None => (rest, "/".to_string()),
    };
    let authority = authority.rsplit_once('@').map_or(authority, |(_, h)| h);
    let (host, port) = split_host_port(authority, 80)?;
    Some(Target {
        host,
        port,
        path: Some(path),
    })
}

fn refusal_body(host: &str) -> String {
    format!(
        "Sterna's sandbox does not allow {host}. To reach it, run the command again with `outside` naming {host} and why: the person can allow the host. It can also be added in Settings › Sandbox › Allowed hosts.\n"
    )
}

fn serve(shared: &Shared, mut client: Client) -> io::Result<()> {
    client.set_read_timeout(Some(HEAD_TIMEOUT))?;
    let (head, early_body) = match read_head(&mut client) {
        Ok(h) => h,
        Err(e) if e.kind() == io::ErrorKind::InvalidData => {
            return respond(
                &mut client,
                "431 Request Header Fields Too Large",
                "The request head is too large.\n",
            );
        }
        Err(e) => return Err(e),
    };
    let head = String::from_utf8_lossy(&head).into_owned();
    let mut lines = head.split("\r\n");
    let request_line = lines.next().unwrap_or("");
    let mut parts = request_line.split_whitespace();
    let (Some(method), Some(target), Some(version)) = (parts.next(), parts.next(), parts.next())
    else {
        return respond(&mut client, "400 Bad Request", "Malformed request line.\n");
    };
    let Some(Target { host, port, path }) = parse_target(method, target) else {
        return respond(
            &mut client,
            "400 Bad Request",
            "The proxy takes CONNECT host:port or an absolute http:// URL.\n",
        );
    };
    let host = normalize(&host);
    if !shared.allowed.permits(&host) {
        shared.refused.record(&host);
        return respond(&mut client, "403 Forbidden", &refusal_body(&host));
    }
    if port != 443 && port != 80 {
        return respond(
            &mut client,
            "403 Forbidden",
            &format!(
                "Sterna's sandbox allows ports 443 and 80 only, and {host} was asked for on port {port}.\n"
            ),
        );
    }

    match path {
        None => {
            let mut upstream = match open_tunnel(&shared.route, &host, port) {
                Ok(u) => u,
                Err(e) => return respond(&mut client, "502 Bad Gateway", &unreachable(&host, &e)),
            };
            client.write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")?;
            client.flush()?;
            if !early_body.is_empty() {
                upstream.write_all(&early_body)?;
            }
            splice(client, upstream)
        }
        Some(path) => {
            // One request per connection: with `Connection: close` forced, a
            // later request on the same socket cannot reach a different host
            // through this already-vetted upstream.
            let mut out = String::new();
            let via_outer = shared.route.resolve.is_none() && shared.route.outer.is_some();
            let (mut upstream, line) = if via_outer {
                let outer = shared.route.outer.as_ref().expect("checked above");
                let stream = match connect_addr(&outer.addr) {
                    Ok(s) => s,
                    Err(e) => {
                        return respond(&mut client, "502 Bad Gateway", &unreachable(&host, &e));
                    }
                };
                if let Some(auth) = &outer.auth {
                    out.push_str(&format!("Proxy-Authorization: {auth}\r\n"));
                }
                (stream, format!("{method} {target} {version}\r\n"))
            } else {
                match connect_direct(&shared.route, &host, port) {
                    Ok(s) => (s, format!("{method} {path} {version}\r\n")),
                    Err(e) => {
                        return respond(&mut client, "502 Bad Gateway", &unreachable(&host, &e));
                    }
                }
            };
            let mut rewritten = line;
            for header in lines.filter(|l| !l.is_empty()) {
                let name = header.split(':').next().unwrap_or("").trim();
                if ["proxy-connection", "proxy-authorization", "connection"]
                    .iter()
                    .any(|h| name.eq_ignore_ascii_case(h))
                {
                    continue;
                }
                rewritten.push_str(header);
                rewritten.push_str("\r\n");
            }
            rewritten.push_str(&out);
            rewritten.push_str("Connection: close\r\n\r\n");
            upstream.write_all(rewritten.as_bytes())?;
            upstream.write_all(&early_body)?;
            splice(client, upstream)
        }
    }
}

fn unreachable(host: &str, err: &io::Error) -> String {
    format!("Could not reach {host}: {err}.\n")
}

fn connect_addr(addr: &str) -> io::Result<TcpStream> {
    let mut last = io::Error::new(io::ErrorKind::NotFound, "no address");
    for sa in addr.to_socket_addrs()? {
        match TcpStream::connect_timeout(&sa, CONNECT_TIMEOUT) {
            Ok(s) => return Ok(s),
            Err(e) => last = e,
        }
    }
    Err(last)
}

fn connect_direct(route: &Route, host: &str, port: u16) -> io::Result<TcpStream> {
    if let Some(resolve) = &route.resolve {
        let addr = resolve(host, port)
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no address"))?;
        return TcpStream::connect_timeout(&addr, CONNECT_TIMEOUT);
    }
    let authority = if host.contains(':') {
        format!("[{host}]:{port}")
    } else {
        format!("{host}:{port}")
    };
    connect_addr(&authority)
}

/// A byte stream to `host:port`: through the outer proxy's own CONNECT when
/// there is one, directly otherwise.
fn open_tunnel(route: &Route, host: &str, port: u16) -> io::Result<TcpStream> {
    let outer = match (&route.resolve, &route.outer) {
        (None, Some(outer)) => outer,
        _ => return connect_direct(route, host, port),
    };
    let mut stream = connect_addr(&outer.addr)?;
    let authority = if host.contains(':') {
        format!("[{host}]:{port}")
    } else {
        format!("{host}:{port}")
    };
    let mut req = format!("CONNECT {authority} HTTP/1.1\r\nHost: {authority}\r\n");
    if let Some(auth) = &outer.auth {
        req.push_str(&format!("Proxy-Authorization: {auth}\r\n"));
    }
    req.push_str("\r\n");
    stream.write_all(req.as_bytes())?;
    stream.set_read_timeout(Some(CONNECT_TIMEOUT))?;
    let (head, rest) = read_head(&mut stream)?;
    stream.set_read_timeout(None)?;
    let status = String::from_utf8_lossy(&head);
    let status_line = status.lines().next().unwrap_or("");
    if status_line.split_whitespace().nth(1) != Some("200") {
        return Err(io::Error::other(format!(
            "the outer proxy answered {status_line}"
        )));
    }
    if !rest.is_empty() {
        // A proxy that sent bytes before we spoke is not one we can splice.
        return Err(io::Error::other("the outer proxy sent unexpected data"));
    }
    Ok(stream)
}

/// Copy both ways until either side closes or idles out.
fn splice(client: Client, upstream: TcpStream) -> io::Result<()> {
    client.set_read_timeout(Some(IDLE_TIMEOUT))?;
    upstream.set_read_timeout(Some(IDLE_TIMEOUT))?;
    let mut client_read = client.try_clone()?;
    let mut upstream_write = upstream.try_clone()?;
    let up = std::thread::Builder::new()
        .name("sandbox-proxy-up".into())
        .spawn(move || {
            let result = io::copy(&mut client_read, &mut upstream_write);
            let _ = upstream_write.shutdown(Shutdown::Write);
            result
        })?;
    let mut client_write = client;
    let mut upstream_read = upstream;
    let down = io::copy(&mut upstream_read, &mut client_write);
    client_write.shutdown(Shutdown::Write);
    if down.is_err() {
        // The server side is gone; do not leave the upload thread waiting on
        // an idle client.
        client_write.shutdown(Shutdown::Both);
        let _ = upstream_read.shutdown(Shutdown::Both);
    }
    let _ = up.join();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader};

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    /// A wake-up that cannot connect does not hold the drop -- and with it
    /// the process -- open: the accept thread it could not wake is left.
    #[cfg(unix)]
    #[test]
    fn a_wake_up_that_cannot_connect_does_not_hold_the_drop() {
        let proxy = Proxy::start(Allowed::defaults()).expect("the proxy starts");
        let socket = proxy.unix_path().expect("a socket on unix").to_path_buf();
        std::fs::remove_file(&socket).expect("the socket file is removed");
        let (done, finished) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            drop(proxy);
            let _ = done.send(());
        });
        assert!(
            finished
                .recv_timeout(std::time::Duration::from_secs(5))
                .is_ok(),
            "dropping the proxy hung on a thread it could not wake"
        );
    }

    #[test]
    fn the_table_is_ten_ecosystems_of_valid_hosts_with_unique_names() {
        assert_eq!(ECOSYSTEMS.len(), 10);
        let names: BTreeSet<_> = ECOSYSTEMS.iter().map(|e| e.name).collect();
        assert_eq!(names.len(), ECOSYSTEMS.len());
        for eco in ECOSYSTEMS {
            assert!(!eco.hosts.is_empty(), "{}", eco.name);
            for host in eco.hosts {
                assert!(valid_host(host), "{host}");
                assert!(!host.starts_with("*."), "{host}");
            }
        }
    }

    #[test]
    fn valid_host_refuses_urls_ports_paths_and_spaces() {
        for ok in ["example.com", "*.example.com", "a-b.c1.io", "Example.COM."] {
            assert!(valid_host(ok), "{ok}");
        }
        for bad in [
            "",
            "https://example.com",
            "example.com:443",
            "example.com/x",
            "exa mple.com",
            "*.",
            "a..b",
            "-a.com",
            "*example.com",
            "a.*.com",
            "::1",
        ] {
            assert!(!valid_host(bad), "{bad}");
        }
    }

    #[test]
    fn permits_matches_names_exactly_and_ignores_case_and_a_trailing_dot() {
        let allowed = Allowed::defaults();
        assert!(allowed.permits("crates.io"));
        assert!(allowed.permits("CRATES.IO"));
        assert!(allowed.permits("crates.io."));
        assert!(!allowed.permits("evil.crates.io"));
        assert!(!allowed.permits("crates.io.evil.com"));
        assert!(!allowed.permits("example.com"));
        assert!(!allowed.permits(""));
    }

    #[test]
    fn a_wildcard_matches_subdomains_but_not_the_name_itself() {
        let allowed = Allowed::new(&[], &s(&["*.example.com"]));
        assert!(allowed.permits("a.example.com"));
        assert!(allowed.permits("a.b.EXAMPLE.com"));
        assert!(!allowed.permits("example.com"));
        assert!(!allowed.permits("badexample.com"));
        assert_eq!(allowed.hosts(), s(&["*.example.com"]));
    }

    #[test]
    fn ip_literals_are_refused_unless_listed_exactly() {
        let allowed = Allowed::new(&[], &s(&["*.0.1", "10.0.0.2"]));
        assert!(!allowed.permits("127.0.0.1"));
        assert!(!allowed.permits("::1"));
        assert!(!allowed.permits("[::1]"));
        assert!(allowed.permits("10.0.0.2"));
    }

    #[test]
    fn a_disabled_ecosystem_is_refused_and_unknown_names_are_skipped() {
        let allowed = Allowed::new(&s(&["rust", "no-such-thing"]), &s(&["bad host"]));
        assert!(allowed.permits("static.crates.io"));
        assert!(!allowed.permits("registry.npmjs.org"));
        assert!(!allowed.permits("pypi.org"));
        assert_eq!(allowed.hosts().len(), 4);
    }

    #[test]
    fn add_takes_effect_at_once_through_every_clone() {
        let allowed = Allowed::new(&[], &[]);
        let seen_by_proxy = allowed.clone();
        assert!(!seen_by_proxy.permits("late.example"));
        assert!(allowed.add("Late.Example"));
        assert!(seen_by_proxy.permits("late.example"));
        assert!(!allowed.add("http://nope"));
        assert_eq!(seen_by_proxy.hosts(), s(&["late.example"]));
    }

    /// Removing is exact: a wildcard entry goes only by its `*.` spelling,
    /// and a name it never held is not an error.
    #[test]
    fn remove_takes_one_entry_away_through_every_clone() {
        let allowed = Allowed::new(&[], &s(&["a.example", "*.b.example"]));
        let seen_by_proxy = allowed.clone();
        assert!(!allowed.remove("b.example"));
        assert!(seen_by_proxy.permits("x.b.example"));
        assert!(allowed.remove("*.B.example"));
        assert!(!seen_by_proxy.permits("x.b.example"));
        assert!(allowed.remove("A.example."));
        assert!(!seen_by_proxy.permits("a.example"));
        assert!(seen_by_proxy.hosts().is_empty());
    }

    #[test]
    fn the_outer_proxy_url_is_parsed_with_and_without_credentials() {
        assert_eq!(
            OuterProxy::parse("http://127.0.0.1:3128"),
            Some(OuterProxy {
                addr: "127.0.0.1:3128".into(),
                auth: None
            })
        );
        let with = OuterProxy::parse("http://us%40er:pw@proxy.corp:8080/").unwrap();
        assert_eq!(with.addr, "proxy.corp:8080");
        assert_eq!(
            with.auth.as_deref(),
            Some(
                format!(
                    "Basic {}",
                    base64::engine::general_purpose::STANDARD.encode("us@er:pw")
                )
                .as_str()
            )
        );
        assert_eq!(
            OuterProxy::parse("proxy.corp").unwrap().addr,
            "proxy.corp:80"
        );
        assert_eq!(OuterProxy::parse("socks5://proxy:1080"), None);
    }

    /// A fake upstream on loopback: echoes bytes when `echo`, otherwise reads
    /// one request head and answers with its request line in the body.
    fn fake_upstream(echo: bool) -> SocketAddr {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let addr = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { continue };
                std::thread::spawn(move || {
                    if echo {
                        let mut buf = [0u8; 1024];
                        while let Ok(n) = stream.read(&mut buf) {
                            if n == 0 || stream.write_all(&buf[..n]).is_err() {
                                break;
                            }
                        }
                    } else if let Ok((head, _)) = read_head(&mut stream) {
                        let head = String::from_utf8_lossy(&head).into_owned();
                        let first = head.lines().next().unwrap_or("").to_string();
                        let body = format!("saw: {first}");
                        let _ = write!(
                            stream,
                            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n{body}",
                            body.len()
                        );
                    }
                });
            }
        });
        addr
    }

    /// A proxy whose allowed names all resolve to `upstream`, with no outer
    /// proxy whatever this process's environment says.
    fn test_proxy(allowed: Allowed, upstream: SocketAddr) -> Proxy {
        Proxy::start_with(
            allowed,
            Route {
                outer: None,
                resolve: Some(Arc::new(move |_, _| Some(upstream))),
            },
        )
        .unwrap()
    }

    fn connect(proxy: &Proxy) -> TcpStream {
        let stream = TcpStream::connect(("127.0.0.1", proxy.port())).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        stream
    }

    fn read_all(stream: &mut TcpStream) -> String {
        let mut out = String::new();
        let _ = stream.read_to_string(&mut out);
        out
    }

    #[test]
    fn connect_to_an_allowed_name_tunnels_bytes_both_ways() {
        let proxy = test_proxy(
            Allowed::new(&[], &s(&["allowed.test"])),
            fake_upstream(true),
        );
        let mut stream = connect(&proxy);
        stream
            .write_all(b"CONNECT allowed.test:443 HTTP/1.1\r\nHost: allowed.test:443\r\n\r\n")
            .unwrap();
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        let mut status = String::new();
        reader.read_line(&mut status).unwrap();
        assert_eq!(status, "HTTP/1.1 200 Connection Established\r\n");
        let mut blank = String::new();
        reader.read_line(&mut blank).unwrap();
        assert_eq!(blank, "\r\n");
        stream.write_all(b"ping through the tunnel").unwrap();
        let mut got = [0u8; 23];
        reader.read_exact(&mut got).unwrap();
        assert_eq!(&got, b"ping through the tunnel");
        assert!(proxy.refused().is_empty());
    }

    #[test]
    fn connect_to_a_refused_name_gets_403_and_is_recorded() {
        let proxy = test_proxy(
            Allowed::new(&[], &s(&["allowed.test"])),
            fake_upstream(true),
        );
        for host in ["first.test", "Blocked.Test", "first.test"] {
            let mut stream = connect(&proxy);
            write!(stream, "CONNECT {host}:443 HTTP/1.1\r\n\r\n").unwrap();
            let reply = read_all(&mut stream);
            assert!(reply.starts_with("HTTP/1.1 403 Forbidden\r\n"), "{reply}");
            assert!(
                reply.contains(&format!(
                    "Sterna's sandbox does not allow {}.",
                    host.to_ascii_lowercase()
                )),
                "{reply}"
            );
        }
        assert_eq!(proxy.refused(), s(&["blocked.test", "first.test"]));
        assert_eq!(proxy.take_refused().len(), 2);
        assert!(proxy.refused().is_empty());
    }

    #[test]
    fn a_port_other_than_80_or_443_is_refused() {
        let proxy = test_proxy(
            Allowed::new(&[], &s(&["allowed.test"])),
            fake_upstream(true),
        );
        let mut stream = connect(&proxy);
        stream
            .write_all(b"CONNECT allowed.test:22 HTTP/1.1\r\n\r\n")
            .unwrap();
        let reply = read_all(&mut stream);
        assert!(reply.starts_with("HTTP/1.1 403 Forbidden\r\n"), "{reply}");
        assert!(reply.contains("port 22"), "{reply}");
        assert!(proxy.refused().is_empty());
    }

    #[test]
    fn an_absolute_form_get_is_forwarded_in_origin_form() {
        let proxy = test_proxy(
            Allowed::new(&[], &s(&["allowed.test"])),
            fake_upstream(false),
        );
        let mut stream = connect(&proxy);
        stream
            .write_all(
                b"GET http://allowed.test/pkg/index?x=1 HTTP/1.1\r\nHost: allowed.test\r\nProxy-Connection: keep-alive\r\n\r\n",
            )
            .unwrap();
        let reply = read_all(&mut stream);
        assert!(reply.starts_with("HTTP/1.1 200 OK\r\n"), "{reply}");
        assert!(
            reply.ends_with("saw: GET /pkg/index?x=1 HTTP/1.1"),
            "{reply}"
        );

        let mut stream = connect(&proxy);
        stream
            .write_all(b"GET http://other.test/ HTTP/1.1\r\n\r\n")
            .unwrap();
        assert!(read_all(&mut stream).starts_with("HTTP/1.1 403"));
        assert_eq!(proxy.refused(), s(&["other.test"]));
    }

    #[test]
    fn env_points_every_proxy_variable_here_and_exempts_loopback() {
        let proxy = test_proxy(Allowed::defaults(), fake_upstream(true));
        let env = proxy.env();
        let url = format!("http://127.0.0.1:{}", proxy.port());
        for key in [
            "HTTPS_PROXY",
            "HTTP_PROXY",
            "ALL_PROXY",
            "https_proxy",
            "http_proxy",
            "all_proxy",
            "npm_config_proxy",
            "npm_config_https_proxy",
        ] {
            assert!(env.contains(&(key.to_string(), url.clone())), "{key}");
        }
        assert!(env.contains(&("NO_PROXY".into(), "localhost,127.0.0.1,::1".into())));
    }

    #[cfg(unix)]
    #[test]
    fn the_unix_socket_serves_and_is_removed_on_drop() {
        use std::os::unix::fs::PermissionsExt as _;
        use std::os::unix::net::UnixStream;
        let proxy = test_proxy(Allowed::new(&[], &[]), fake_upstream(true));
        let socket = proxy.unix_path().unwrap().to_path_buf();
        let dir = socket.parent().unwrap().to_path_buf();
        let mode = std::fs::metadata(&dir).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o700);
        let mut stream = UnixStream::connect(&socket).unwrap();
        stream
            .write_all(b"CONNECT nope.test:443 HTTP/1.1\r\n\r\n")
            .unwrap();
        let mut reply = String::new();
        stream.read_to_string(&mut reply).unwrap();
        assert!(reply.starts_with("HTTP/1.1 403"), "{reply}");
        drop(proxy);
        assert!(!dir.exists());
    }
}
