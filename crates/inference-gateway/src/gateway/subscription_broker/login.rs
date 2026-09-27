//! Signing a subscription account in through the broker's own login.
//!
//! The broker writes the credential it later reads, in its own format and
//! through its own TLS client, so a login it performed is one it can serve.
//! This runs that login as a child, keeps the browser for the gateway to
//! open, forwards a pasted callback address, and reports only what a person
//! must see: the sign-in link, a device code, success or failure. Every other
//! line the broker prints (its version, an SSH hint naming this machine's
//! public address, its logs) stays here.

use std::ffi::OsString;
use std::fs;
use std::path::PathBuf;
use std::process::{Child, ChildStdin, ChildStdout, Command, ExitStatus, Stdio};

use anyhow::{Context, Result};

use super::{
    BrokerPaths, ENV_CLIPROXYAPI_BIN, INSTANCE_ID_BYTES, diagnostic_name, discover_executable,
    ensure_private_directory, random_hex, render_config, validate_entitlement, write_private_file,
};
use crate::subscription::connect::Progress;

/// How a person signs in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Method {
    /// A link opened in a browser, or a callback address pasted back.
    Browser,
    /// A code entered on any device. OpenAI issues one when it is asked
    /// for; xAI's login (`-xai-login`) is always one, whatever it is called.
    DeviceCode,
}

/// The broker flag that signs `provider` in by `method`, or `None` when the
/// provider has no such flow.
#[must_use]
pub fn login_flag(provider: &str, method: Method) -> Option<&'static str> {
    match (provider, method) {
        ("anthropic", Method::Browser) => Some("-claude-login"),
        ("openai", Method::Browser) => Some("-codex-login"),
        ("openai", Method::DeviceCode) => Some("-codex-device-login"),
        ("google", Method::Browser) => Some("-antigravity-login"),
        ("kimi", Method::Browser) => Some("-kimi-login"),
        ("xai", Method::Browser) => Some("-xai-login"),
        ("devin", Method::Browser) => Some("-devin-login"),
        ("meta", Method::Browser) => Some("-meta-login"),
        _ => None,
    }
}

/// Environment a login needs from the parent: a home for the broker, and a
/// proxy or certificate store the network path may depend on. Nothing else.
const PASSED_ENVIRONMENT: &[&str] = &[
    "HOME",
    "HTTPS_PROXY",
    "https_proxy",
    "HTTP_PROXY",
    "http_proxy",
    "ALL_PROXY",
    "all_proxy",
    "NO_PROXY",
    "no_proxy",
    "SSL_CERT_FILE",
    "SSL_CERT_DIR",
];

/// A running broker login. Dropping it stops the child and removes its
/// private configuration; the credential it wrote stays.
pub struct BrokerLogin {
    child: Child,
    stdin: Option<ChildStdin>,
    stdout: Option<ChildStdout>,
    instance_dir: PathBuf,
}

impl BrokerLogin {
    /// Starts the broker's login for `entitlement`, writing into its auth
    /// directory. The broker never opens a browser itself (`-no-browser`).
    pub fn start(paths: &BrokerPaths, entitlement: &str, flag: &str) -> Result<Self> {
        validate_entitlement(entitlement)?;
        let executable = discover_executable(paths, std::env::var_os(ENV_CLIPROXYAPI_BIN))?;
        let instances_dir = paths.entitlement_dir.join("instances");
        for directory in [
            &paths.brokers_dir,
            &paths.entitlement_dir,
            &paths.auth_dir,
            &instances_dir,
        ] {
            ensure_private_directory(directory)?;
        }
        // A login the gateway was killed during leaves its private config
        // behind; an entitlement signs in once at a time, so any is stale.
        for entry in fs::read_dir(&instances_dir)?.flatten() {
            if entry.file_name().to_string_lossy().starts_with("login-") {
                let _ = fs::remove_dir_all(entry.path());
            }
        }
        let instance_dir = instances_dir.join(format!("login-{}", random_hex(INSTANCE_ID_BYTES)?));
        ensure_private_directory(&instance_dir)?;
        let config_path = instance_dir.join("config.yaml");
        // A login serves nothing: the port and the key only make the file valid.
        let config = render_config(0, &paths.auth_dir, &random_hex(INSTANCE_ID_BYTES)?)?;
        write_private_file(&config_path, config.as_bytes())?;

        let mut command = Command::new(&executable);
        command
            .arg("-config")
            .arg(&config_path)
            .arg(flag)
            .arg("-no-browser")
            .current_dir(&instance_dir)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .env_clear();
        for name in PASSED_ENVIRONMENT {
            if let Some(value) = std::env::var_os(name) {
                command.env(name, value);
            }
        }
        let mut child = match command.spawn() {
            Ok(child) => child,
            Err(error) => {
                let _ = fs::remove_dir_all(&instance_dir);
                return Err(error).with_context(|| {
                    format!(
                        "could not start CLIProxyAPI executable {:?}",
                        diagnostic_name(&executable)
                    )
                });
            }
        };
        Ok(Self {
            stdin: child.stdin.take(),
            stdout: child.stdout.take(),
            child,
            instance_dir,
        })
    }

    /// Where a pasted callback address goes. The broker reads one after its
    /// first fifteen seconds; a line written earlier waits in the pipe.
    pub fn take_stdin(&mut self) -> Option<ChildStdin> {
        self.stdin.take()
    }

    /// The broker's output, for [`LoginOutput::read`].
    pub fn take_stdout(&mut self) -> Option<ChildStdout> {
        self.stdout.take()
    }

    /// Waits for the login to end.
    pub fn wait(&mut self) -> Result<ExitStatus> {
        self.child
            .wait()
            .context("could not wait for the CLIProxyAPI login")
    }
}

impl Drop for BrokerLogin {
    fn drop(&mut self) {
        if matches!(self.child.try_wait(), Ok(None)) {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
        let _ = fs::remove_dir_all(&self.instance_dir);
    }
}

/// The line the broker prints once the credential is on disk.
const SAVED: &str = "Authentication saved to";

/// Reads the broker's login output, one line at a time.
#[derive(Debug, Default)]
pub struct LoginOutput {
    awaiting_link: bool,
    /// xAI names its device link on the line after its own heading.
    awaiting_device_link: bool,
    device_link: Option<String>,
    failure: Option<String>,
}

impl LoginOutput {
    /// What `line` tells a person, or `None` for a line nobody needs.
    ///
    /// Success is the line naming the saved credential, not the broker's
    /// "authentication successful", which it also prints before saving.
    pub fn read(&mut self, line: &str) -> Option<Progress> {
        let line = line.trim();
        if line.starts_with("Visit the following URL") {
            self.awaiting_link = true;
            return None;
        }
        if self.awaiting_link && line.starts_with("https://") {
            self.awaiting_link = false;
            return Some(Progress::Opened {
                authorize_url: line.to_owned(),
                browser_opened: false,
            });
        }
        if let Some(link) = line.strip_prefix("Codex device URL:") {
            self.device_link = Some(link.trim().to_owned());
            return None;
        }
        // xAI: "To authenticate, please visit:", the link on a line of its
        // own, then "Then enter this code: ABCD".
        if line.starts_with("To authenticate, please visit") {
            self.awaiting_device_link = true;
            return None;
        }
        if self.awaiting_device_link && line.starts_with("https://") {
            self.awaiting_device_link = false;
            self.device_link = Some(line.to_owned());
            return None;
        }
        if let Some(code) = line
            .strip_prefix("Codex device code:")
            .or_else(|| line.strip_prefix("Then enter this code:"))
        {
            return Some(Progress::DeviceCode {
                verification_url: self.device_link.clone()?,
                user_code: code.trim().to_owned(),
            });
        }
        // Anywhere in the line: the paste prompt has no line ending, so the
        // broker's next line arrives joined to it.
        if let Some(at) = line.find(SAVED) {
            return Some(Progress::Connected {
                account: account_label(line[at + SAVED.len()..].trim()),
            });
        }
        let lower = line.to_ascii_lowercase();
        if self.failure.is_none()
            && (lower.contains("authentication failed") || lower.contains("[error"))
        {
            self.failure = Some(failure_reason(line));
        }
        None
    }

    /// The first failure the broker reported, if it reported one.
    pub fn failure(&self) -> Option<&str> {
        self.failure.as_deref()
    }
}

/// A saved credential's label: its file name without the provider prefix
/// and the extension (`claude-me@example.com.json` is `me@example.com`).
fn account_label(path: &str) -> Option<String> {
    let name = std::path::Path::new(path).file_name()?.to_string_lossy();
    let stem = name.strip_suffix(".json").unwrap_or(&name);
    let label = stem.split_once('-').map_or(stem, |(_, rest)| rest);
    (!label.is_empty()).then(|| label.to_owned())
}

/// A failure as a person reads it: without the log prefix, and without the
/// provider's response body, which may echo the code back.
fn failure_reason(line: &str) -> String {
    let message = line.rsplit("] ").next().unwrap_or(line).trim();
    let message = match message.find("status ") {
        Some(at) => {
            let digits = message[at + 7..]
                .find(|c: char| !c.is_ascii_digit())
                .map_or(message.len(), |end| at + 7 + end);
            &message[..digits]
        }
        None => message,
    };
    message.chars().take(300).collect()
}

/// Whether a browser can be opened for the person at this terminal: not over
/// SSH, where it would open on the wrong screen, and on Linux only with a
/// display.
pub fn browser_available(variable: impl Fn(&str) -> Option<OsString>) -> bool {
    let set = |name: &str| variable(name).is_some_and(|value| !value.is_empty());
    if set("SSH_CONNECTION") || set("SSH_TTY") {
        return false;
    }
    if cfg!(target_os = "linux") {
        return set("DISPLAY") || set("WAYLAND_DISPLAY");
    }
    cfg!(any(target_os = "macos", windows))
}

/// Opens an `https` link with the system's own launcher (on macOS
/// `/usr/bin/open`, never whatever a terminal put first on `PATH`), so it
/// lands in the default browser. Anything but `https` is refused.
pub fn open_in_browser(url: &str) -> bool {
    if !url.starts_with("https://") {
        return false;
    }
    let mut command = if cfg!(target_os = "macos") {
        let mut command = Command::new("/usr/bin/open");
        command.arg(url);
        command
    } else if cfg!(windows) {
        let mut command = Command::new("rundll32.exe");
        command.arg("url.dll,FileProtocolHandler").arg(url);
        command
    } else {
        let mut command = Command::new("xdg-open");
        command.arg(url);
        command
    };
    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// xAI's login is a device flow: its link on the line after its own
    /// heading, then the code. Both reach the person.
    #[test]
    fn xai_device_flow_yields_a_link_and_a_code() {
        let mut output = LoginOutput::default();
        let said: Vec<_> = [
            "To authenticate, please visit:",
            "https://accounts.x.ai/oauth2/device?user_code=ABCD",
            "Then enter this code: ABCD",
        ]
        .into_iter()
        .filter_map(|line| output.read(line))
        .collect();
        assert_eq!(
            said,
            [Progress::DeviceCode {
                verification_url: "https://accounts.x.ai/oauth2/device?user_code=ABCD".into(),
                user_code: "ABCD".into(),
            }]
        );
    }

    /// The broker's own no-browser Claude login output, captured 2026-09-14
    /// from CLIProxyAPI 7.2.153 with the challenge and state shortened.
    const CLAUDE_LOGIN: &str = "CLIProxyAPI Version: 7.2.153, Commit: 934fb792
[2026-09-14 23:54:53] [--------] [info ] [main.go:572] CLIProxyAPI Version: 7.2.153
To authenticate from a remote machine, an SSH tunnel may be required.
  ssh -L 54545:127.0.0.1:54545 root@203.0.113.7 -p 22
Visit the following URL to continue authentication:
https://claude.ai/oauth/authorize?client_id=9d1c250a-e61b-44d9-88ed-5944d1962f5e&code=true&code_challenge=abc&code_challenge_method=S256&redirect_uri=http%3A%2F%2Flocalhost%3A54545%2Fcallback&response_type=code&scope=user%3Aprofile&state=xyz
Waiting for Claude authentication callback...
Paste the Claude callback URL (or press Enter to keep waiting): Authentication saved to /auth/claude-me@example.com.json
Claude authentication successful!";

    fn read_all(text: &str) -> (Vec<Progress>, LoginOutput) {
        let mut output = LoginOutput::default();
        let progress = text.lines().filter_map(|line| output.read(line)).collect();
        (progress, output)
    }

    /// The whole link, exactly as printed, then success naming the account;
    /// the SSH hint with this machine's address and the logs never cross.
    #[test]
    fn the_link_and_the_saved_account_are_all_that_crosses() {
        let (progress, output) = read_all(CLAUDE_LOGIN);
        let link = CLAUDE_LOGIN.lines().nth(5).unwrap();
        assert_eq!(
            progress,
            vec![
                Progress::Opened {
                    authorize_url: link.to_owned(),
                    browser_opened: false,
                },
                Progress::Connected {
                    account: Some("me@example.com".into()),
                },
            ]
        );
        assert_eq!(output.failure(), None);
        let rendered = serde_json::to_string(&progress).unwrap();
        assert!(!rendered.contains("203.0.113.7"), "{rendered}");
    }

    /// A link is read only after the line that introduces it.
    #[test]
    fn a_stray_link_is_not_a_sign_in_link() {
        let (progress, _) = read_all(
            "https://github.com/router-for-me/CLIProxyAPI/releases\nWaiting for Claude authentication callback...",
        );
        assert!(progress.is_empty(), "{progress:?}");
    }

    #[test]
    fn a_device_code_carries_its_link() {
        let (progress, _) = read_all(
            "Starting Codex device authentication...\nCodex device URL: https://auth.openai.com/codex/device\nCodex device code: ABCD-EFGH",
        );
        assert_eq!(
            progress,
            vec![Progress::DeviceCode {
                verification_url: "https://auth.openai.com/codex/device".into(),
                user_code: "ABCD-EFGH".into(),
            }]
        );
    }

    /// A failure keeps its status and loses the provider's body.
    #[test]
    fn a_failure_is_reported_without_the_response_body() {
        let (progress, output) = read_all(
            "[2026-09-14] [x] [error] [anthropic.go:9] Token exchange failed: token exchange failed with status 400: {\"code\":\"secret-echo\"}\nClaude authentication failed: boom",
        );
        assert!(progress.is_empty());
        assert_eq!(
            output.failure(),
            Some("Token exchange failed: token exchange failed with status 400")
        );
    }

    #[test]
    fn each_provider_names_its_login_and_only_openai_has_a_device_code() {
        assert_eq!(
            login_flag("anthropic", Method::Browser),
            Some("-claude-login")
        );
        assert_eq!(login_flag("openai", Method::Browser), Some("-codex-login"));
        assert_eq!(
            login_flag("openai", Method::DeviceCode),
            Some("-codex-device-login")
        );
        assert_eq!(
            login_flag("google", Method::Browser),
            Some("-antigravity-login")
        );
        assert_eq!(login_flag("anthropic", Method::DeviceCode), None);
    }

    /// Over SSH a browser would open on the wrong screen, so none is opened.
    #[test]
    fn no_browser_is_opened_over_ssh() {
        let over_ssh = |name: &str| (name == "SSH_CONNECTION").then(|| OsString::from("1 2 3 4"));
        assert!(!browser_available(over_ssh));
        let local = |name: &str| (name == "DISPLAY").then(|| OsString::from(":0"));
        assert_eq!(
            browser_available(local),
            cfg!(any(target_os = "macos", target_os = "linux", windows))
        );
        assert!(!open_in_browser("file:///etc/passwd"));
    }
}
