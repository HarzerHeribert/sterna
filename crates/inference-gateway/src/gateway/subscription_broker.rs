//! Lifecycle boundary around one pinned CLIProxyAPI subscription sidecar.
//!
//! Glasshouse remains the public gateway and scheduler. This process is a
//! loopback-only protocol/authentication adapter for exactly one entitlement;
//! it never receives the disposable key given to a harness.

use std::ffi::{OsStr, OsString};
use std::fmt;
use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::net::{Ipv4Addr, SocketAddrV4, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use ureq::Agent;
use ureq::config::AutoHeaderValue;

use crate::routing::CredentialId;
use crate::secret::{REDACTED, SecretRef};

/// Explicit override for the pinned CLIProxyAPI executable.
pub const ENV_CLIPROXYAPI_BIN: &str = "INFERENCE_GATEWAY_CLIPROXYAPI_BIN";

/// Where one entitlement's broker keeps its state, and where its executable
/// lives.
///
/// The four directories [`RunningSubscriptionBroker::start`] needs, resolved
/// by whoever embeds this module. It is deliberately a plain carrier of
/// already-decided paths: this module owns the *lifecycle* of a sidecar, not
/// the layout of the host's data directory, and it must stay usable by a host
/// that has no `RuntimePaths` at all. `brokers_dir` and `entitlement_dir` are
/// created as private directories in that order, so `entitlement_dir` must be
/// under `brokers_dir` and `auth_dir` under `entitlement_dir`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BrokerPaths {
    /// Private state root shared by every entitlement's broker.
    pub brokers_dir: PathBuf,
    /// Stable state root for this one entitlement; its `instances`
    /// subdirectory holds each run's ephemeral serving directory.
    pub entitlement_dir: PathBuf,
    /// Stable OAuth state for this one entitlement, which outlives any one
    /// running sidecar.
    pub auth_dir: PathBuf,
    /// The host-managed pinned CLIProxyAPI executable, used when
    /// [`ENV_CLIPROXYAPI_BIN`] names nothing.
    pub executable: PathBuf,
}

const PROVIDER_NAME: &str = "cliproxyapi";
const CREDENTIAL_SERVICE: &str = "glasshouse-subscription-broker";
const API_KEY_BYTES: usize = 32;
const INSTANCE_ID_BYTES: usize = 16;
const READY_TIMEOUT: Duration = Duration::from_secs(5);
const READY_POLL: Duration = Duration::from_millis(20);
const READY_STABILITY: Duration = Duration::from_millis(250);
const MODEL_CATALOGUE_MAX_BYTES: u64 = 8 * 1024 * 1024;

/// The in-memory credential accepted only by one loopback sidecar.
///
/// There is deliberately no `Display`, serde implementation, clone, hash, or
/// public accessor. The crate-private accessor is the single seam a later
/// routing integration uses to construct the existing `UpstreamBackend`.
struct BrokerApiKey(String);

impl BrokerApiKey {
    fn generate() -> Result<Self> {
        let mut bytes = [0_u8; API_KEY_BYTES];
        getrandom::fill(&mut bytes)
            .context("could not read cryptographic randomness for the subscription broker")?;
        Ok(Self(hex::encode(bytes)))
    }

    fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for BrokerApiKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(REDACTED)
    }
}

/// How long [`RunningSubscriptionBroker::verify_credential`] may take: one
/// short completion, first token included.
const VERIFY_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);

/// A live one-entitlement CLIProxyAPI sidecar.
///
/// Dropping this value kills and reaps the process and removes its ephemeral
/// serving directory. The stable per-entitlement auth directory survives so
/// OAuth refresh state remains available to the next instance of that same
/// entitlement.
pub struct RunningSubscriptionBroker {
    child: Option<Child>,
    base_url: String,
    internal_key: BrokerApiKey,
    credential_id: CredentialId,
    auth_dir: PathBuf,
    instance_dir: PathBuf,
    executable_name: OsString,
    config_path: PathBuf,
}

impl RunningSubscriptionBroker {
    /// Start one sidecar for `entitlement` using the override, when present,
    /// or the Glasshouse-managed pinned executable.
    pub fn start(paths: &BrokerPaths, entitlement: &str) -> Result<Self> {
        let executable = discover_executable(paths, std::env::var_os(ENV_CLIPROXYAPI_BIN))?;
        let mut last = None;
        for _ in 0..3 {
            match Self::start_with(paths, entitlement, &executable, READY_TIMEOUT, &[]) {
                Ok(running) => return Ok(running),
                Err(error) => last = Some(error),
            }
        }
        Err(last.expect("bounded startup attempted at least once"))
    }

    fn start_with(
        paths: &BrokerPaths,
        entitlement: &str,
        executable: &Path,
        timeout: Duration,
        child_env: &[(OsString, OsString)],
    ) -> Result<Self> {
        validate_entitlement(entitlement)?;
        let executable_name = diagnostic_name(executable);
        if !executable.is_file() {
            bail!(
                "CLIProxyAPI executable {:?} is not a regular file",
                executable_name
            );
        }

        let auth_dir = paths.auth_dir.clone();
        let instances_dir = paths.entitlement_dir.join("instances");
        for directory in [
            paths.brokers_dir.clone(),
            paths.entitlement_dir.clone(),
            auth_dir.clone(),
            instances_dir.clone(),
        ] {
            ensure_private_directory(&directory)?;
        }

        let instance_dir = instances_dir.join(format!("run-{}", random_hex(INSTANCE_ID_BYTES)?));
        ensure_private_directory(&instance_dir)?;
        let config_path = instance_dir.join("config.yaml");

        let reserved = TcpListener::bind(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0))
            .context("could not reserve a loopback port for CLIProxyAPI")?;
        let port = reserved
            .local_addr()
            .context("could not inspect the reserved CLIProxyAPI port")?
            .port();

        let internal_key = BrokerApiKey::generate()?;
        let config = render_config(port, &auth_dir, internal_key.expose())?;
        write_private_file(&config_path, config.as_bytes())?;
        let mut command = Command::new(executable);
        command
            .arg("-config")
            .arg(&config_path)
            .arg("-local-model")
            .current_dir(&instance_dir)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .env_clear();
        for (name, value) in child_env {
            command.env(name, value);
        }

        // **Held until the last instant before the child binds it.** A
        // reserved port is only reserved while the listener is open; between
        // the drop and the child's own bind, anything on this machine may
        // take it — and then the readiness probe below would be answered by
        // a stranger. The window cannot be closed from here (the child must
        // do the binding), so it is made as small as the syscall order
        // allows, and the probe's bearer key is what tells our own sidecar
        // from a stranger that happens to be listening.
        drop(reserved);
        let child = match command.spawn() {
            Ok(child) => child,
            Err(error) => {
                let _ = fs::remove_dir_all(&instance_dir);
                return Err(error).with_context(|| {
                    format!("could not start CLIProxyAPI executable {executable_name:?}")
                });
            }
        };

        let mut running = Self {
            child: Some(child),
            base_url: format!("http://127.0.0.1:{port}"),
            internal_key,
            credential_id: CredentialId::new(
                PROVIDER_NAME,
                SecretRef::OsCredential {
                    service: CREDENTIAL_SERVICE.to_owned(),
                    account: entitlement.to_owned(),
                },
            ),
            auth_dir,
            instance_dir,
            executable_name,
            config_path,
        };

        if let Err(error) = running.wait_until_ready(port, timeout) {
            running.terminate();
            return Err(error);
        }

        Ok(running)
    }

    fn wait_until_ready(&mut self, port: u16, timeout: Duration) -> Result<()> {
        let deadline = Instant::now() + timeout;
        let mut authenticated_once = false;
        loop {
            if let Some(status) = self
                .child
                .as_mut()
                .expect("a starting broker owns its child")
                .try_wait()
                .context("could not inspect the CLIProxyAPI process")?
            {
                bail!(
                    "CLIProxyAPI executable {:?} exited before readiness with {status}",
                    self.executable_name
                );
            }

            if let Ok(mut stream) = TcpStream::connect(SocketAddrV4::new(Ipv4Addr::LOCALHOST, port))
            {
                let _ = stream.set_read_timeout(Some(Duration::from_millis(200)));
                let request = format!(
                    "GET /v1/models HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nAuthorization: Bearer {}\r\nConnection: close\r\n\r\n",
                    self.internal_key.expose()
                );
                if stream.write_all(request.as_bytes()).is_ok() {
                    let mut response = Vec::with_capacity(4096);
                    if (&mut stream).take(4096).read_to_end(&mut response).is_ok()
                        && models_endpoint_is_ready(&response)
                    {
                        if authenticated_once {
                            return Ok(());
                        }
                        authenticated_once = true;
                        thread::sleep(READY_STABILITY.min(timeout));
                        continue;
                    }
                }
            }
            authenticated_once = false;
            if Instant::now() >= deadline {
                bail!(
                    "CLIProxyAPI executable {:?} did not become ready within the bounded startup timeout",
                    self.executable_name
                );
            }
            thread::sleep(READY_POLL.min(timeout));
        }
    }

    /// Loopback origin used to build the sidecar's existing gateway routes.
    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    /// Printable identity of the entitlement-specific internal credential.
    pub fn credential_id(&self) -> &CredentialId {
        &self.credential_id
    }

    pub fn provider_name(&self) -> &'static str {
        PROVIDER_NAME
    }

    /// The one narrow handoff into `UpstreamBackend` construction.
    ///
    /// This value must remain inside Glasshouse. It is public only because
    /// `gateway` is a library surface and the typed broker result has to
    /// supply both halves needed by the existing public backend constructor;
    /// callers must give it directly to the backend credential boundary.
    pub(super) fn internal_api_key(&self) -> &str {
        self.internal_key.expose()
    }

    /// Stable directory containing only this entitlement's OAuth material.
    pub fn auth_dir(&self) -> &Path {
        &self.auth_dir
    }

    /// Read this account's model catalogue through its authenticated sidecar.
    ///
    /// The bearer value never leaves this module, redirects are disabled so
    /// it cannot be forwarded to another origin, and every failure sentence
    /// is fixed text rather than an upstream response or transport rendering.
    pub fn model_catalogue_document(&self) -> Result<Vec<u8>> {
        let agent = Agent::new_with_config(
            Agent::config_builder()
                .http_status_as_error(false)
                .max_redirects(0)
                .accept_encoding(AutoHeaderValue::None)
                .timeout_connect(Some(crate::provider::discovery::CONNECT_TIMEOUT))
                .timeout_recv_response(Some(crate::provider::discovery::RESPONSE_TIMEOUT))
                .timeout_global(Some(crate::provider::discovery::TOTAL_TIMEOUT))
                .build(),
        );
        let url = format!("{}/v1/models", self.base_url);
        let mut response = agent
            .get(&url)
            .header(
                "authorization",
                format!("Bearer {}", self.internal_key.expose()),
            )
            .call()
            .map_err(|_| {
                anyhow::anyhow!("the subscription broker model catalogue did not answer")
            })?;
        if !response.status().is_success() {
            bail!("the subscription broker refused its model catalogue request");
        }
        response
            .body_mut()
            .with_config()
            .limit(MODEL_CATALOGUE_MAX_BYTES)
            .read_to_vec()
            .map_err(|_| {
                anyhow::anyhow!("the subscription broker model catalogue could not be read")
            })
    }

    /// One small completion for `model` through this sidecar: proof that
    /// the credential it holds is accepted, not merely present on disk. The
    /// error names the status only -- a refusal's body can carry account
    /// details, and this line is shown to the person.
    pub fn verify_credential(&self, model: &str) -> Result<()> {
        let agent = Agent::new_with_config(
            Agent::config_builder()
                .http_status_as_error(false)
                .max_redirects(0)
                .accept_encoding(AutoHeaderValue::None)
                .timeout_connect(Some(crate::provider::discovery::CONNECT_TIMEOUT))
                .timeout_global(Some(VERIFY_TIMEOUT))
                .build(),
        );
        // Written as text: the relay names no serialization crate
        // (`no_part_of_the_relay_deserializes_anything`), and a model id
        // from the catalogue needs no escaping once it is held to these.
        if model.is_empty()
            || !model
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_' | ':' | '/'))
        {
            bail!("the model id to try is not a plain identifier");
        }
        let body = format!(
            r#"{{"model":"{model}","max_tokens":16,"messages":[{{"role":"user","content":"Reply with ok."}}]}}"#
        );
        let url = format!("{}/v1/chat/completions", self.base_url);
        let response = agent
            .post(&url)
            .header(
                "authorization",
                format!("Bearer {}", self.internal_key.expose()),
            )
            .header("content-type", "application/json")
            .send(body.as_bytes())
            .map_err(|_| anyhow::anyhow!("the subscription broker did not answer"))?;
        let status = response.status().as_u16();
        if !(200..300).contains(&status) {
            bail!("{model} was refused with HTTP {status}");
        }
        Ok(())
    }

    fn terminate(&mut self) {
        if let Some(mut child) = self.child.take() {
            match child.try_wait() {
                Ok(Some(_)) => {}
                Ok(None) | Err(_) => {
                    let _ = child.kill();
                }
            }
            let _ = child.wait();
        }
        let _ = fs::remove_file(&self.config_path);
        let _ = fs::remove_dir_all(&self.instance_dir);
    }
}

fn models_endpoint_is_ready(response: &[u8]) -> bool {
    let Ok(response) = std::str::from_utf8(response) else {
        return false;
    };
    let Some((head, body)) = response.split_once("\r\n\r\n") else {
        return false;
    };
    (head.starts_with("HTTP/1.1 200") || head.starts_with("HTTP/1.0 200"))
        && body.contains("\"data\":[{")
}

impl Drop for RunningSubscriptionBroker {
    fn drop(&mut self) {
        self.terminate();
    }
}

impl fmt::Debug for RunningSubscriptionBroker {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RunningSubscriptionBroker")
            .field("base_url", &self.base_url)
            .field("internal_key", &REDACTED)
            .field("credential_id", &self.credential_id.label())
            .field("auth_dir", &self.auth_dir)
            .field("executable", &self.executable_name)
            .finish_non_exhaustive()
    }
}

fn validate_entitlement(entitlement: &str) -> Result<()> {
    if entitlement.trim().is_empty() {
        bail!("a subscription broker entitlement name cannot be empty");
    }
    if entitlement.len() > 256 {
        bail!("a subscription broker entitlement name is too long");
    }
    Ok(())
}

fn discover_executable(paths: &BrokerPaths, override_path: Option<OsString>) -> Result<PathBuf> {
    if let Some(path) = override_path.filter(|value| !value.is_empty()) {
        let path = PathBuf::from(path);
        if path.is_file() {
            return Ok(path);
        }
        bail!(
            "CLIProxyAPI executable {:?} named by {ENV_CLIPROXYAPI_BIN} was not found",
            diagnostic_name(&path)
        );
    }

    let managed = paths.executable.clone();
    if managed.is_file() {
        return Ok(managed);
    }
    bail!(
        "CLIProxyAPI executable {:?} is absent from the gateway's managed tools directory; set {ENV_CLIPROXYAPI_BIN} to an explicit executable",
        diagnostic_name(&managed)
    )
}

fn diagnostic_name(path: &Path) -> OsString {
    path.file_name()
        .unwrap_or_else(|| OsStr::new("CLIProxyAPI"))
        .to_os_string()
}

fn random_hex(bytes: usize) -> Result<String> {
    let mut random = vec![0_u8; bytes];
    getrandom::fill(&mut random)
        .context("could not read cryptographic randomness for a subscription broker instance")?;
    Ok(hex::encode(random))
}

/// The sidecar's configuration. `passthrough-headers` is on so the ChatGPT
/// backend's `x-codex-turn-state` sticky-routing token reaches the harness,
/// which echoes it for the rest of its turn -- without it every request is
/// routed alone and mostly misses the provider's cache. CLIProxyAPI's own
/// filter (`FilterUpstreamHeaders`) still drops `Set-Cookie`, framing and
/// encoding headers.
fn render_config(port: u16, auth_dir: &Path, internal_key: &str) -> Result<String> {
    let auth_dir = auth_dir
        .to_str()
        .context("the subscription broker auth directory is not valid UTF-8")?;
    let auth_dir = yaml_double_quoted(auth_dir);
    let internal_key = yaml_double_quoted(internal_key);

    Ok(format!(
        concat!(
            "host: \"127.0.0.1\"\n",
            "port: {port}\n",
            "tls:\n",
            "  enable: false\n",
            "remote-management:\n",
            "  allow-remote: false\n",
            "  secret-key: \"\"\n",
            "  disable-control-panel: true\n",
            "  disable-auto-update-panel: true\n",
            "auth-dir: {auth_dir}\n",
            "api-keys: [{internal_key}]\n",
            "debug: false\n",
            "pprof:\n",
            "  enable: false\n",
            "  addr: \"127.0.0.1:0\"\n",
            "plugins:\n",
            "  enabled: false\n",
            "  dir: \"plugins-disabled\"\n",
            "  configs: {{}}\n",
            "commercial-mode: true\n",
            "request-log: false\n",
            "logging-to-file: false\n",
            "logs-max-total-size-mb: 0\n",
            "error-logs-max-files: 0\n",
            "usage-statistics-enabled: false\n",
            "request-retry: 0\n",
            "max-retry-credentials: 1\n",
            "max-retry-interval: 0\n",
            "disable-claude-cloak-mode: true\n",
            "quota-exceeded:\n",
            "  switch-project: false\n",
            "  switch-preview-model: false\n",
            "  antigravity-credits: false\n",
            "routing:\n",
            "  strategy: \"fill-first\"\n",
            "  session-affinity: false\n",
            "passthrough-headers: true\n",
            "save-cooldown-status: false\n",
            "ws-auth: true\n",
            "nonstream-keepalive-interval: 0\n",
            "streaming:\n",
            "  keepalive-seconds: 0\n",
            "  bootstrap-retries: 0\n"
        ),
        port = port,
        auth_dir = auth_dir,
        internal_key = internal_key,
    ))
}

/// Encode one string as a YAML double-quoted scalar without bringing a body
/// parser into the gateway relay module.
fn yaml_double_quoted(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('"');
    for character in value.chars() {
        match character {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            control if control.is_control() => {
                use std::fmt::Write as _;
                write!(&mut out, "\\u{:04x}", control as u32)
                    .expect("writing into a String cannot fail");
            }
            ordinary => out.push(ordinary),
        }
    }
    out.push('"');
    out
}

fn ensure_private_directory(path: &Path) -> Result<()> {
    fs::create_dir_all(path).context("could not create a private subscription broker directory")?;
    let metadata = fs::symlink_metadata(path)
        .context("could not inspect a private subscription broker directory")?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        bail!("a subscription broker private directory is not a real directory");
    }
    set_owner_only_directory(path)?;
    Ok(())
}

fn write_private_file(path: &Path, contents: &[u8]) -> Result<()> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options
        .open(path)
        .context("could not create the private CLIProxyAPI bootstrap config")?;
    file.write_all(contents)
        .context("could not write the private CLIProxyAPI bootstrap config")?;
    file.sync_all()
        .context("could not finish the private CLIProxyAPI bootstrap config")?;
    Ok(())
}

#[cfg(unix)]
fn set_owner_only_directory(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
        .context("could not make a subscription broker directory owner-only")
}

#[cfg(not(unix))]
fn set_owner_only_directory(_path: &Path) -> Result<()> {
    Ok(())
}

pub mod login;

#[cfg(test)]
mod tests;
