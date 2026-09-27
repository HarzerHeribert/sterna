//! Signing in to a subscription: the gateway's `subscriptions connect`,
//! driven on a thread of its own so the session stays usable while a person
//! is in their browser, with its progress on a sheet that Esc hides and the
//! dock's "signing in" chip brings back.
//!
//! **Nothing opens a browser on a single click.** The gateway is always run
//! with `--no-browser`; the panel offers the link to copy, and opening it in
//! a browser is a row that asks first -- and is not offered at all over SSH,
//! where the browser would open on the wrong machine.
use super::*;
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};

/// How long a sign-in waits for the person before it gives up and says so.
const PATIENCE: Duration = Duration::from_secs(300);

/// What the screen holds of a sign-in running beside the session: its name,
/// the way to stop it, and the way to hand it a pasted address.
#[derive(Debug, Clone)]
pub(crate) struct Handle {
    pub(crate) label: String,
    pub(crate) cancel: Arc<AtomicBool>,
    pub(crate) pastes: mpsc::Sender<String>,
    /// The gateway's process, the leader of its own group.
    pub(crate) pid: u32,
}

/// The screen's hold on a running sign-in. Dropped with the screen -- the
/// session ending -- it takes the gateway's process group down with it, so
/// no login is left holding a callback port after Sterna has gone.
#[derive(Debug, Default)]
pub(crate) struct Running(pub(crate) Option<Handle>);

impl Drop for Running {
    fn drop(&mut self) {
        if let Some(handle) = self.0.take() {
            handle.cancel.store(true, Ordering::SeqCst);
            #[cfg(unix)]
            crate::tools::invoke::kill_group(handle.pid);
            // Windows has no group to take down; the sign-in's own thread
            // stops the child when it sees the cancel.
            #[cfg(not(unix))]
            let _ = handle.pid;
        }
    }
}

/// What the sign-in says as it goes.
pub(crate) enum Event {
    /// A line the chat keeps: the whole link or code, the outcome.
    Note(String),
    /// The panel, redrawn.
    Panel(Box<Panel>),
    /// It is over, one way or another.
    Done,
}

/// The name a subscription goes by on screen, never its account id.
pub(super) fn label(provider: &str) -> String {
    SUBSCRIPTIONS
        .iter()
        .find(|subscription| subscription.provider == provider)
        .map_or_else(
            || provider.to_string(),
            |subscription| subscription.label.to_string(),
        )
}

/// Starts the sign-in. With a screen it runs on a thread of its own and this
/// returns at once; in a script it runs here, and Ctrl-C cancels it.
pub(super) fn start(
    session: &Session<'_>,
    provider: &str,
    declared: Option<&str>,
    device_code: bool,
    again: String,
) {
    // No account named: the gateway connects, and once it works declares,
    // the provider's default one.
    let mut arguments = vec!["subscriptions", "connect", provider];
    if let Some(account) = declared {
        arguments.extend(["--entitlement", account]);
    }
    arguments.extend(["--json", "--no-browser"]);
    if device_code {
        arguments.push("--device-code");
    }
    let label = label(provider);
    let unreachable = |text: &str| show(session, Panel::text(format!("Sign in · {label}"), text));
    let Some(mut command) = session.gateway.control_command(&arguments) else {
        return unreachable("The inference gateway is not reachable.");
    };
    // Its own group, so a cancelled sign-in takes the broker's login (which
    // holds the provider's callback port) down with the gateway.
    #[cfg(unix)]
    std::os::unix::process::CommandExt::process_group(&mut command, 0);
    let Ok(mut child) = command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
    else {
        return unreachable("The inference gateway could not be started.");
    };
    let Some(stdout) = child.stdout.take() else {
        let _ = child.kill();
        return;
    };
    let stdin = child.stdin.take();
    let (lines, arrived) = mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            if lines.send(line).is_err() {
                break;
            }
        }
    });
    let panel = SignIn::new(&label, again, !super::super::ui::over_ssh());
    let cancel = Arc::new(AtomicBool::new(false));
    let (pastes, pasted) = mpsc::channel();
    match session.ui {
        Some(ui) => {
            ui.sign_in(Handle {
                label,
                cancel: Arc::clone(&cancel),
                pastes,
                pid: child.id(),
            });
            let updates = ui.updates();
            std::thread::spawn(move || {
                let mut panel = panel;
                drive(
                    child,
                    &arrived,
                    stdin,
                    &mut panel,
                    &|| cancel.load(Ordering::SeqCst),
                    &pasted,
                    &mut |event| {
                        let _ = updates.send(super::super::ui::Update::SignIn(event));
                    },
                );
            });
        }
        None => {
            // Ctrl-C cancels the sign-in, as it cancels a tool call.
            let token = crate::tools::invoke::CancellationToken::new();
            session.interrupt.arm(token.clone());
            let mut panel = panel;
            drive(
                child,
                &arrived,
                stdin,
                &mut panel,
                &|| token.is_cancelled(),
                &pasted,
                &mut |event| {
                    if let Event::Note(note) = event {
                        session_println!("{note}");
                    }
                },
            );
            if token.is_cancelled() {
                session.interrupt.consumed();
            }
        }
    }
}

/// The sign-in's own loop: the gateway's lines become the panel and the
/// chat's notes; a pasted address goes to the gateway; a cancel, or five
/// minutes with no answer, stops it and says which.
fn drive(
    mut child: Child,
    lines: &mpsc::Receiver<String>,
    mut stdin: Option<ChildStdin>,
    sign_in: &mut SignIn,
    cancelled: &dyn Fn() -> bool,
    pastes: &mpsc::Receiver<String>,
    emit: &mut dyn FnMut(Event),
) {
    let started = Instant::now();
    emit(Event::Panel(Box::new(sign_in.render())));
    loop {
        match lines.recv_timeout(Duration::from_millis(200)) {
            Ok(line) => {
                if let Some(progress) = SignInProgress::read(&line) {
                    emit(Event::Note(sign_in.apply(progress)));
                    emit(Event::Panel(Box::new(sign_in.render())));
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                let stop = if cancelled() {
                    Some(State::Cancelled)
                } else if started.elapsed() > PATIENCE && sign_in.state == State::Waiting {
                    Some(State::TimedOut)
                } else {
                    None
                };
                if let Some(state) = stop {
                    #[cfg(unix)]
                    crate::tools::invoke::kill_group(child.id());
                    let _ = child.kill();
                    emit(Event::Note(sign_in.stop(state)));
                    emit(Event::Panel(Box::new(sign_in.render())));
                    break;
                }
                if let Ok(pasted) = pastes.try_recv()
                    && let Some(pipe) = stdin.as_mut()
                    && writeln!(pipe, "{}", pasted.trim())
                        .and_then(|()| pipe.flush())
                        .is_ok()
                {
                    sign_in.pasted = true;
                    emit(Event::Panel(Box::new(sign_in.render())));
                }
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                if sign_in.state == State::Waiting {
                    emit(Event::Note(sign_in.apply(SignInProgress::Failed(
                        "the gateway ended the sign-in without an answer".into(),
                    ))));
                    emit(Event::Panel(Box::new(sign_in.render())));
                }
                break;
            }
        }
    }
    drop(stdin);
    let _ = child.wait();
    emit(Event::Done);
}

/// One progress line the gateway's `subscriptions connect --json` writes.
/// Unknown shapes are dropped rather than printed raw: this is another
/// program's output and the panel is not a place to echo bytes nobody
/// recognised.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum SignInProgress {
    Opened { link: String },
    DeviceCode { link: String, code: String },
    Connected(Option<String>),
    Failed(String),
}

impl SignInProgress {
    pub(super) fn read(line: &str) -> Option<Self> {
        let value: serde_json::Value = serde_json::from_str(line).ok()?;
        let text = |key: &str| value.get(key).and_then(serde_json::Value::as_str);
        Some(match text("state")? {
            "opened" => Self::Opened {
                link: text("authorize_url")?.to_owned(),
            },
            "device_code" => Self::DeviceCode {
                link: text("verification_url")?.to_owned(),
                code: text("user_code")?.to_owned(),
            },
            "connected" => Self::Connected(text("account").map(str::to_owned)),
            "failed" => Self::Failed(text("reason").unwrap_or("").to_owned()),
            _ => return None,
        })
    }
}

/// Where a sign-in stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(super) enum State {
    #[default]
    Waiting,
    Connected,
    Failed,
    Cancelled,
    TimedOut,
}

/// The sign-in panel: what to do next, and every way to do it.
#[derive(Debug, Default)]
pub(super) struct SignIn {
    label: String,
    link: Option<String>,
    device: Option<(String, String)>,
    pasted: bool,
    outcome: Option<String>,
    state: State,
    /// The command that starts it again.
    again: String,
    /// Whether a browser can open on this screen: not over SSH.
    browser: bool,
}

impl SignIn {
    pub(super) fn new(label: &str, again: String, browser: bool) -> Self {
        Self {
            label: label.to_owned(),
            again,
            browser,
            ..Self::default()
        }
    }

    /// Records `progress` and returns the line the chat keeps for it: the
    /// whole link or code, so it can be read and selected after the panel is
    /// gone.
    pub(super) fn apply(&mut self, progress: SignInProgress) -> String {
        let label = self.label.clone();
        match progress {
            SignInProgress::Opened { link } => {
                let note = format!("Sign-in link for {label}:\n{link}");
                self.link = Some(link);
                note
            }
            SignInProgress::DeviceCode { link, code } => {
                let note = format!("Sign in to {label} with the code {code} at:\n{link}");
                self.device = Some((link, code));
                note
            }
            SignInProgress::Connected(account) => {
                let said = match account {
                    Some(account) => format!("{label} is connected as {account}."),
                    None => format!("{label} is connected."),
                };
                self.outcome = Some(said.clone());
                self.state = State::Connected;
                said
            }
            SignInProgress::Failed(reason) => {
                self.outcome = Some(format!("failed: {reason}"));
                self.state = State::Failed;
                format!("ERROR: signing in to {label} failed: {reason}")
            }
        }
    }

    /// Stops a sign-in that is still waiting, and says why.
    fn stop(&mut self, state: State) -> String {
        self.state = state;
        let said = match state {
            State::TimedOut => format!(
                "No answer from {} after {} minutes; the sign-in stopped.",
                self.label,
                PATIENCE.as_secs() / 60
            ),
            _ => format!("Sign-in to {} cancelled.", self.label),
        };
        self.outcome = Some(said.clone());
        said
    }

    pub(super) fn render(&self) -> Panel {
        use crate::workbench::Action;
        let info = |text: &str| tui::PanelRow::info(text);
        let waiting = self.state == State::Waiting;
        let mut rows = Vec::new();
        if let (Some(link), true) = (&self.link, waiting) {
            rows.push(info(if self.browser {
                "Open the sign-in page in a browser, or copy the link."
            } else {
                "Copy the sign-in link and open it in a browser on any device."
            }));
            if self.browser {
                rows.push(tui::PanelRow::run(
                    "open the sign-in page in your browser",
                    Action::AskOpenLink(link.clone()),
                ));
            }
            rows.push(tui::PanelRow::run(
                "copy the sign-in link",
                Action::Copy(link.clone()),
            ));
            rows.push(tui::PanelRow::open(
                "signed in somewhere else? paste the address the browser ended on",
                Action::PasteCallback,
            ));
        }
        if let (Some((link, code)), true) = (&self.device, waiting) {
            rows.push(info(&format!("On any device, open {link}")));
            rows.push(info(&format!("and enter the code {code}")));
            rows.push(tui::PanelRow::run(
                "copy the code",
                Action::Copy(code.clone()),
            ));
            if self.browser {
                rows.push(tui::PanelRow::run(
                    "open the page in your browser",
                    Action::AskOpenLink(link.clone()),
                ));
            }
        }
        if self.pasted && waiting {
            rows.push(info("pasted; finishing the sign-in…"));
        }
        rows.push(info(self.outcome.as_deref().unwrap_or(
            "waiting for the sign-in, then one request to check it works…",
        )));
        match self.state {
            State::Waiting => rows.push(tui::PanelRow::run(
                "cancel sign-in",
                Action::CancelSignIn,
            )),
            // The browser's last page is the broker's local callback, which
            // has already closed by the time a person looks at it.
            State::Connected => rows.push(info(
                "The browser tab may say it cannot connect — that is expected once the sign-in has finished; you can close it.",
            )),
            State::Failed | State::Cancelled | State::TimedOut => rows.push(
                tui::PanelRow::run("start again", Action::Command(self.again.clone())),
            ),
        }
        let mut panel = Panel::rows(format!("Sign in · {}", self.label), rows);
        panel.selected = panel.rows.iter().position(tui::PanelRow::acts).unwrap_or(0);
        panel
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workbench::Action;

    /// The gateway's sign-in lines become progress; the link and code arrive
    /// whole, and a line of another shape is dropped.
    #[test]
    fn sign_in_progress_reads_the_gateway_lines_whole() {
        let link = "https://claude.ai/oauth/authorize?client_id=x&scope=user%3Aprofile&state=s";
        assert_eq!(
            SignInProgress::read(&format!(
                r#"{{"state":"opened","authorize_url":"{link}","browser_opened":false}}"#
            )),
            Some(SignInProgress::Opened { link: link.into() })
        );
        assert_eq!(
            SignInProgress::read(
                r#"{"state":"device_code","verification_url":"https://auth.openai.com/codex/device","user_code":"ABCD-EFGH"}"#
            ),
            Some(SignInProgress::DeviceCode {
                link: "https://auth.openai.com/codex/device".into(),
                code: "ABCD-EFGH".into()
            })
        );
        assert_eq!(
            SignInProgress::read(r#"{"state":"connected","account":"me@example.com"}"#),
            Some(SignInProgress::Connected(Some("me@example.com".into())))
        );
        assert_eq!(SignInProgress::read("waiting for the browser"), None);
    }

    /// The panel offers every way through with the whole link behind each
    /// row: opening a browser asks first, copying does not, and a pasted
    /// address finishes it. The chat keeps the link. It is named by the
    /// subscription, and while it waits it can be cancelled.
    #[test]
    fn the_sign_in_panel_asks_before_a_browser_and_can_be_cancelled() {
        let link = "https://claude.ai/oauth/authorize?client_id=x&scope=user%3Aprofile&state=s";
        let mut sign_in = SignIn::new("Claude", "/login claude".into(), true);
        let note = sign_in.apply(SignInProgress::Opened { link: link.into() });
        assert_eq!(note, format!("Sign-in link for Claude:\n{link}"));
        let panel = sign_in.render();
        assert_eq!(panel.title, "Sign in · Claude");
        let actions: Vec<_> = panel
            .rows
            .iter()
            .filter_map(|row| row.action.clone())
            .collect();
        assert_eq!(
            actions,
            vec![
                Action::AskOpenLink(link.into()),
                Action::Copy(link.into()),
                Action::PasteCallback,
                Action::CancelSignIn,
            ]
        );
        assert_eq!(
            sign_in.apply(SignInProgress::Failed("status 400".into())),
            "ERROR: signing in to Claude failed: status 400"
        );
        let failed = sign_in.render();
        assert!(
            failed
                .rows
                .iter()
                .any(|row| row.text == "failed: status 400")
        );
        assert_eq!(
            failed.rows[failed.selected].action,
            Some(Action::Command("/login claude".into())),
            "a failed sign-in offers to start again"
        );
    }

    /// Over SSH a browser would open on the wrong machine: the open row is
    /// not offered and the panel starts on copying the link.
    #[test]
    fn over_ssh_the_sign_in_starts_on_copying_the_link() {
        let link = "https://x.ai/device";
        let mut sign_in = SignIn::new("Grok", "/login grok".into(), false);
        sign_in.apply(SignInProgress::Opened { link: link.into() });
        let panel = sign_in.render();
        assert!(
            panel
                .rows
                .iter()
                .all(|row| !matches!(row.action, Some(Action::AskOpenLink(_))))
        );
        assert_eq!(
            panel.rows[panel.selected].action,
            Some(Action::Copy(link.into()))
        );
    }

    /// A cancelled or timed-out sign-in says so and offers to start again.
    #[test]
    fn a_stopped_sign_in_says_why_and_starts_again() {
        let mut sign_in = SignIn::new("Grok", "/login grok".into(), true);
        assert_eq!(sign_in.stop(State::Cancelled), "Sign-in to Grok cancelled.");
        let panel = sign_in.render();
        assert!(
            panel
                .rows
                .iter()
                .all(|row| row.action != Some(Action::CancelSignIn))
        );
        assert_eq!(
            panel.rows[panel.selected].action,
            Some(Action::Command("/login grok".into()))
        );
        let mut sign_in = SignIn::new("Grok", "/login grok".into(), true);
        assert!(sign_in.stop(State::TimedOut).contains("after 5 minutes"));
    }
}
