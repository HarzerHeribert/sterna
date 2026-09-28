//! The standalone inference gateway: the process every provider request goes
//! through, and the binary the entitlement, subscription and routing-cost
//! controls ask.
//!
//! **Sterna links against no gateway crate.** It shells out to a sibling
//! executable, so the process boundary is the only contract between the two.
//!
//! **Start, attach or go direct is decided once** ([`select`] then
//! [`start_or_attach`]). A gateway was *handed over* when `ANTHROPIC_BASE_URL`
//! names one and either `ANTHROPIC_AUTH_TOKEN` came with it or the host is
//! loopback, and sterna attaches and starts nothing. Otherwise sterna
//! starts the gateway it was named (or
//! `inference-gateway` on `PATH`) and owns its whole lifetime. The one
//! fallback is loud, not silent: no gateway installed and none named means
//! the session talks to the provider directly and says so; a gateway named
//! by path that cannot be started is a startup refusal, because a request
//! that skipped a gateway the user asked for would also skip the entitlement
//! and cost controls that are the reason it exists.
//!
//! **The routing rule is one line**: `entitlements`, `subscriptions` and
//! `credentials` always run the gateway binary, because account, subscription
//! and credential state is the gateway's and belongs to no project, while
//! `routing-cost` runs the variant's own executable — Glasshouse, scoped to
//! this project, in a hosted session — because the usage a session ran up is
//! telemetry Glasshouse records for the project it supervises.

use std::io::Write;
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdout, Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Deserialize;

use crate::contract::ServedBy;

/// How this module reaches the gateway binary. `None` never attempts a
/// command: every control reports unreachable and [`Gateway::serve`] refuses.
#[derive(Debug, Clone)]
pub enum Gateway {
    /// Shells out to this executable. `PathBuf::from("inference-gateway")`
    /// lets the OS resolve it from `PATH` (`Command::new` maps to `execvp` on
    /// a bare name); a test passes its own fake script's path instead.
    Command { gateway: PathBuf },
    /// Attached to a gateway someone else started: the serving URL was handed
    /// over in the environment. Every control runs the gateway binary
    /// [`attached_gateway_binary`] finds, which is where that state lives
    /// whoever started the gateway.
    Attached,
}

impl Gateway {
    /// The command one control runs. `None` -- no gateway installed anywhere
    /// for an attached session -- is what every caller renders as
    /// unreachable.
    pub(crate) fn control_command(&self, args: &[&str]) -> Option<Command> {
        let mut command = match self {
            Self::Command { gateway } => Command::new(gateway),
            Self::Attached => Command::new(attached_gateway_binary()?),
        };
        command.args(args);
        Some(command)
    }

    /// The one definition of reachable: found, spawned, and exited 0.
    /// `stdin`, when given, is written and then dropped -- closing that end of
    /// the pipe -- so a child reading until EOF gets exactly one message.
    pub(crate) fn run(&self, args: &[&str], stdin: Option<&[u8]>) -> Option<Vec<u8>> {
        let mut command = self.control_command(args)?;
        command.stdout(Stdio::piped());
        command.stderr(Stdio::null());
        command.stdin(if stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        });

        let mut child = command.spawn().ok()?;
        if let Some(bytes) = stdin {
            child.stdin.take()?.write_all(bytes).ok()?;
        }
        let output = child.wait_with_output().ok()?;

        if output.status.success() {
            Some(output.stdout)
        } else {
            None
        }
    }

    /// Why the gateway gave no answer, in one sentence a person can act on:
    /// not installed, not startable, or ran and stopped with its own words.
    pub(crate) fn why_unreachable(&self) -> String {
        let Some(mut command) = self.control_command(&["entitlements", "--json"]) else {
            return "`inference-gateway` is not installed, or not on your PATH.".into();
        };
        // Its name, not its path: a path into a build or temp directory is
        // noise on a sheet meant to be read.
        let program = Path::new(command.get_program()).file_name().map_or_else(
            || "inference-gateway".into(),
            |name| name.to_string_lossy().into_owned(),
        );
        command
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped());
        match command.output() {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                format!("`{program}` is not installed, or not on your PATH.")
            }
            Err(error) => format!("`{program}` could not be started: {error}."),
            Ok(output) if !output.status.success() => {
                let said = String::from_utf8_lossy(&output.stderr);
                match said.lines().map(str::trim).find(|line| !line.is_empty()) {
                    Some(line) => format!("It ran and stopped: {line}"),
                    None => "It ran and stopped without saying why.".into(),
                }
            }
            Ok(_) => "It answered, but not with a list of accounts.".into(),
        }
    }

    /// Spawns `inference-gateway serve` and waits for its one ready line.
    ///
    /// **The read is blocking and on the caller's thread, deliberately.** The
    /// contract is that the gateway prints exactly one line as soon as it is
    /// listening, so the wait is short; doing it here rather than on a helper
    /// thread is what lets [`start_or_attach`] set the process environment
    /// while this process is still single-threaded.
    pub fn serve(&self, log: &Path) -> Result<Serving, ServeError> {
        let Gateway::Command { gateway } = self else {
            return Err(ServeError::Other(
                "sterna cannot start: no inference gateway is configured -- pass \
                 --gateway <path>, or set ANTHROPIC_BASE_URL to attach to one \
                 already serving"
                    .to_string(),
            ));
        };

        let mut command = Command::new(gateway);
        command.arg("serve").arg("--listen").arg("127.0.0.1:0");
        // Its own process group, so a Ctrl-C the terminal sends to Sterna's
        // group does not also end the gateway: sterna treats one Ctrl-C as
        // "cancel this call", and the next turn needs the gateway alive.
        // `Serving`'s `Drop` is what ends it.
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            command.process_group(0);
        }
        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            // Piped, never nulled: a refusal before the ready line is quoted
            // back to the user, and everything after it goes to `log`.
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| {
                let message = format!(
                    "sterna cannot start: could not run the inference gateway `{}` ({e}) -- \
                     install it, pass --gateway <path>, or set ANTHROPIC_BASE_URL to \
                     attach to one already serving",
                    gateway.display()
                );
                if e.kind() == std::io::ErrorKind::NotFound {
                    ServeError::NotInstalled(message)
                } else {
                    ServeError::Other(message)
                }
            })?;

        let stdout = child
            .stdout
            .take()
            .expect("stdout was piped by the spawn above");
        let mut reader = BufReader::new(stdout);
        let mut line = String::new();
        let ready = match reader.read_line(&mut line) {
            Ok(0) | Err(_) => None,
            Ok(_) => serde_json::from_str::<Ready>(line.trim()).ok(),
        };
        let mut stderr = child
            .stderr
            .take()
            .expect("stderr was piped by the spawn above");
        let Some(ready) = ready else {
            let _ = child.kill();
            let _ = child.wait();
            let mut said = String::new();
            let _ = stderr.read_to_string(&mut said);
            let said = said.trim();
            return Err(ServeError::Other(format!(
                "sterna cannot start: the inference gateway `{}` did not report a \
                 listening address on its first line of output{}",
                gateway.display(),
                if said.is_empty() {
                    String::new()
                } else {
                    format!("; it said: {said}")
                }
            )));
        };
        // The gateway's own diagnostics for the rest of the session, where a
        // failing session can be read back: the TUI owns the terminal from
        // here on, so they cannot go to Sterna's stderr.
        let log = log.to_path_buf();
        std::thread::spawn(move || {
            if let Ok(mut file) = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&log)
            {
                let _ = std::io::copy(&mut stderr, &mut file);
            }
        });

        Ok(Serving {
            child,
            _stdout: reader,
            base_url: ready.listening,
            token: ready.token.filter(|token| !token.is_empty()),
        })
    }
}

/// The file names a gateway executable can have on this platform.
#[cfg(windows)]
const GATEWAY_NAMES: &[&str] = &["inference-gateway.exe", "inference-gateway"];
#[cfg(not(windows))]
const GATEWAY_NAMES: &[&str] = &["inference-gateway"];

/// The gateway binary an attached session's controls run: the one
/// `INFERENCE_GATEWAY_BIN` names, else one installed beside this executable,
/// else the first on `PATH`.
///
/// **It is resolved here rather than left to `execvp` because "no gateway
/// anywhere" has to be distinguishable from "the gateway refused".** A bare
/// name handed to `Command::new` would fail at spawn either way, and the
/// controls would report a reachable gateway that said no.
fn attached_gateway_binary() -> Option<PathBuf> {
    if let Some(named) = std::env::var_os("INFERENCE_GATEWAY_BIN").filter(|value| !value.is_empty())
    {
        return Some(PathBuf::from(named));
    }
    std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(Path::to_path_buf))
        .into_iter()
        .chain(std::env::split_paths(
            &std::env::var_os("PATH").unwrap_or_default(),
        ))
        .flat_map(|dir| GATEWAY_NAMES.iter().map(move |name| dir.join(name)))
        .find(|candidate| candidate.is_file())
}

/// Why [`Gateway::serve`] could not start a gateway. The distinction exists
/// for exactly one caller: `session::run` treats an executable that is not
/// installed as "talk to the provider directly" when nobody named one with
/// `--gateway`, and every other failure as the refusal it is.
#[derive(Debug)]
pub enum ServeError {
    /// The executable could not be found at all.
    NotInstalled(String),
    Other(String),
}

impl std::fmt::Display for ServeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotInstalled(message) | Self::Other(message) => f.write_str(message),
        }
    }
}

/// The one line `inference-gateway serve` prints when it is listening.
#[derive(Debug, Deserialize)]
struct Ready {
    listening: String,
    #[serde(default)]
    token: Option<String>,
}

/// A gateway this process started, and which dies with it.
///
/// **Holding this value is what keeps the gateway alive**, so a caller binds
/// it for as long as the session lasts. [`Drop`] kills the child rather than
/// only closing its stdin: stdin closing is the gateway's own shutdown signal
/// and both are sent, but only the kill is not contingent on the child
/// reading.
pub struct Serving {
    child: Child,
    /// Held, not read. The gateway prints one line and then keeps this pipe
    /// open; dropping the reader would close Sterna's end of it.
    _stdout: BufReader<ChildStdout>,
    base_url: String,
    token: Option<String>,
}

impl Serving {
    /// The URL this gateway is listening on -- what `ANTHROPIC_BASE_URL` is
    /// set to for the rest of the process.
    #[must_use]
    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    /// The bearer the gateway minted for this session, when it minted one.
    #[must_use]
    pub fn token(&self) -> Option<&str> {
        self.token.as_deref()
    }
}

impl Drop for Serving {
    fn drop(&mut self) {
        // Closing stdin is the contract's polite shutdown, and the gateway
        // needs a moment to honour it: its own exit is what terminates the
        // subscription brokers it started. A kill that followed the close
        // at once left three broker sidecars orphaned on every session end
        // (measured 2026-09-11). The kill stays as the bound for a gateway
        // that never reads its stdin.
        drop(self.child.stdin.take());
        let deadline = std::time::Instant::now() + GATEWAY_SHUTDOWN_GRACE;
        while std::time::Instant::now() < deadline {
            match self.child.try_wait() {
                Ok(Some(_)) => return,
                Ok(None) => std::thread::sleep(std::time::Duration::from_millis(25)),
                Err(_) => break,
            }
        }
        // The whole process group, not just the gateway: it was started as
        // the leader of its own group, so every subscription sidecar it
        // spawned is in it too. Killing only the gateway left each sidecar
        // running with parent 1 whenever the gateway's own shutdown outlasted
        // the grace (measured 2026-09-24: sixteen CLIProxyAPI processes left
        // from one evening's ruler runs).
        #[cfg(unix)]
        {
            // SAFETY: `killpg` takes two integers; the id is the group this
            // child leads (`process_group(0)` in `serve`).
            unsafe { libc::killpg(self.child.id() as libc::pid_t, libc::SIGKILL) };
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// How long a gateway gets to leave on its own after its stdin closes
/// before it is killed. Long enough for it to stop its brokers; short
/// enough that a session's end never feels like a hang.
const GATEWAY_SHUTDOWN_GRACE: std::time::Duration = std::time::Duration::from_secs(3);

/// Attaches to a gateway that is already serving, or starts one.
///
/// Whether the environment hands this session a gateway to attach to: a
/// base URL, and with it either the bearer the gateway minted or a loopback
/// host. A base URL alone is not enough — Claude Code exports one into every
/// child it starts, and a sterna run from such a shell would otherwise attach
/// to a proxy it was never given a token for and never start its own.
#[must_use]
pub fn handed_a_gateway() -> bool {
    let Ok(url) = std::env::var("ANTHROPIC_BASE_URL") else {
        return false;
    };
    if url.is_empty() {
        return false;
    }
    if std::env::var("ANTHROPIC_AUTH_TOKEN").is_ok_and(|token| !token.is_empty()) {
        return true;
    }
    let host = url
        .split_once("://")
        .map_or(url.as_str(), |(_, rest)| rest)
        .split(['/', '?'])
        .next()
        .unwrap_or_default()
        .trim_start_matches('[')
        .rsplit_once(':')
        .map_or_else(|| "", |(host, _)| host)
        .trim_end_matches(']');
    let host = if host.is_empty() {
        url.split_once("://")
            .map_or(url.as_str(), |(_, rest)| rest)
            .split(['/', '?', ':'])
            .next()
            .unwrap_or_default()
    } else {
        host
    };
    host == "localhost" || host == "::1" || host.starts_with("127.")
}

/// The handle a session runs its controls through, decided once from the
/// environment and the flag: handed a gateway, the session is attached to it;
/// otherwise the gateway named, or `inference-gateway` on `PATH`.
#[must_use]
pub fn select(named: Option<&Path>) -> Gateway {
    if handed_a_gateway() {
        return Gateway::Attached;
    }
    Gateway::Command {
        gateway: named.map_or_else(|| PathBuf::from("inference-gateway"), Path::to_path_buf),
    }
}

/// `Ok(None)` is an attached session or a direct one; `Ok(Some)` owns the
/// gateway it started, which [`point_environment_at`] then hands to
/// [`crate::wire`]. `named` says whether the user asked for this gateway by
/// path, which is what turns "not installed" from a notice into a refusal.
pub fn start_or_attach(
    gateway: &Gateway,
    named: bool,
    log: &Path,
) -> Result<Option<Serving>, String> {
    if matches!(gateway, Gateway::Attached) {
        return Ok(None);
    }

    let serving = match gateway.serve(log) {
        Ok(serving) => serving,
        // Not installed, and nobody asked for it by path: the session talks
        // to the provider directly, exactly as sterna did before the gateway
        // existed, and says so once. A named `--gateway` that fails stays the
        // refusal it is.
        Err(ServeError::NotInstalled(_)) if !named => {
            eprintln!(
                "sterna: no `inference-gateway` on PATH -- talking to the provider directly \
                 (install it, or pass --gateway <path>)"
            );
            return Ok(None);
        }
        Err(error) => return Err(error.to_string()),
    };
    Ok(Some(serving))
}

/// Points [`crate::wire::base_url`] and [`crate::wire`]'s credential header at
/// the gateway sterna started, through the environment.
///
/// # Safety of the environment write
///
/// `std::env::set_var` is unsound beside another thread reading the
/// environment. The only caller is `session::run`, after the start's splash
/// thread has been joined and before it spawns the interrupt watcher or
/// starts the live UI, and [`Gateway::serve`] itself starts no thread -- so
/// this process is single-threaded at the write.
pub fn point_environment_at(serving: &Serving) {
    // SAFETY: single-threaded at this point -- see the doc comment above.
    unsafe {
        std::env::set_var("ANTHROPIC_BASE_URL", serving.base_url());
        if let Some(token) = serving.token() {
            std::env::set_var("ANTHROPIC_AUTH_TOKEN", token);
        }
    }
}

// ---------------------------------------------------------------------
// The credentials the gateway holds, and the one way sterna adds to them
// ---------------------------------------------------------------------

/// One row of `inference-gateway credentials list --json`: a provider that
/// declares a credential variable, and where that variable resolves from now.
/// `source` is `None` when nothing resolves it -- the state `/key` exists to
/// leave.
#[derive(Debug, Clone, Deserialize)]
pub struct CredentialRow {
    pub provider: String,
    #[serde(default)]
    pub variable: Option<String>,
    #[serde(default)]
    pub source: Option<String>,
    /// `present`, `absent`, `refused` or `unavailable`. `refused` is the one
    /// a person must act on: a Keychain item exists that this build may not
    /// read, so the key has to be entered again.
    #[serde(default)]
    pub native_store: Option<String>,
}

#[derive(Debug, Deserialize)]
struct CredentialList {
    #[serde(default)]
    providers: Vec<CredentialRow>,
}

/// The gateway's credential table, or `None` when it cannot be asked -- not
/// installed, not reachable, or too old to have the subcommand. Absent is not
/// empty: a caller must not read "could not ask" as "nothing is stored".
#[must_use]
pub fn credentials(gateway: &Gateway) -> Option<Vec<CredentialRow>> {
    let stdout = gateway.run(&["credentials", "list", "--json"], None)?;
    serde_json::from_slice::<CredentialList>(&stdout)
        .ok()
        .map(|list| list.providers)
}

/// Whether the gateway answered, named at least one provider, and resolves a
/// credential for none of them -- the only state worth a startup notice.
#[must_use]
pub fn nothing_resolves(gateway: &Gateway) -> bool {
    credentials(gateway)
        .is_some_and(|rows| !rows.is_empty() && rows.iter().all(|row| row.source.is_none()))
}

/// Hands `key` to the gateway to store, and reports the variable it was
/// stored under. `None` is a gateway that refused or could not be run.
///
/// **The key travels on the child's stdin and never in `args`.** A command
/// line is readable by every process on the machine; a pipe is not.
#[must_use]
pub fn store_credential(gateway: &Gateway, provider: &str, key: &str) -> Option<String> {
    #[derive(Deserialize)]
    struct Stored {
        #[serde(default)]
        variable: Option<String>,
    }
    let stdout = gateway.run(
        &["credentials", "set", provider, "--json"],
        Some(key.as_bytes()),
    )?;
    Some(
        serde_json::from_slice::<Stored>(&stdout)
            .ok()
            .and_then(|stored| stored.variable)
            .unwrap_or_else(|| "API key".to_string()),
    )
}

// ---------------------------------------------------------------------
// Which entitlement served each request, and what it cost
// ---------------------------------------------------------------------

/// One row of `inference-gateway routing-cost --json`. Only the columns this
/// module needs are declared; every other key is ignored by `serde_json`
/// without any attribute here.
#[derive(Debug, Deserialize)]
struct ObservationRow {
    #[serde(default)]
    provider: Option<String>,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    route: Option<String>,
    #[serde(default)]
    quota_context: Option<String>,
    #[serde(default)]
    input_tokens: Option<u64>,
    #[serde(default)]
    output_tokens: Option<u64>,
    #[serde(default)]
    cached_input_tokens: Option<u64>,
}

/// Fills a [`ServedBy`] from `inference-gateway routing-cost --json --since
/// <since>`. **The row used is the last model observation printed** (excluding
/// local context-firewall bookkeeping): rows arrive ascending by
/// `observed_at`, and the last row at or after `since` is the one closest to
/// the request this call is answering for.
///
/// Absent is not zero: no gateway, a launch failure, a non-zero exit, or an
/// empty window all produce [`ServedBy::default`], whose `is_known` is
/// `false` -- never a `ServedBy` with token fields defaulted to zero.
pub fn served_by(gateway: &Gateway, since: SystemTime) -> ServedBy {
    let since_secs = unix_secs(since).to_string();
    let Some(stdout) = gateway.run(&["routing-cost", "--json", "--since", &since_secs], None)
    else {
        return ServedBy::default();
    };

    let text = String::from_utf8_lossy(&stdout);
    let row = text
        .lines()
        .filter_map(|line| serde_json::from_str::<ObservationRow>(line.trim()).ok())
        // Tool/firewall bookkeeping is not a model request or an entitlement.
        .rfind(|row| {
            !(row.provider.as_deref() == Some("glasshouse")
                && row.model.as_deref() == Some("context-firewall"))
        });

    match row {
        Some(row) => ServedBy {
            provider: row.provider,
            model: row.model,
            route: row.route,
            quota_context: row.quota_context,
            input_tokens: row.input_tokens,
            output_tokens: row.output_tokens,
            cached_input_tokens: row.cached_input_tokens,
        },
        None => ServedBy::default(),
    }
}

fn unix_secs(t: SystemTime) -> i64 {
    match t.duration_since(UNIX_EPOCH) {
        Ok(d) => d.as_secs() as i64,
        Err(e) => -(e.duration().as_secs() as i64),
    }
}
