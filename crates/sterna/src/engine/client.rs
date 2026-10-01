//! A client's side of the seam, in process: what the terminal holds of a
//! session. Nothing here is a handle into the session -- a [`Link`] sends
//! commands, and a prompt is the data the session sent and the link to
//! answer it with.

use std::sync::mpsc;

use super::hub::{Hub, In, Out};
use super::wire::{self, Answer, Asks, Choice, Command, Prompt};

/// Sends one client's commands to its session. Cloned freely: every clone
/// speaks as the same client.
#[derive(Clone, Debug)]
pub struct Link {
    client: u64,
    hub: mpsc::Sender<In>,
}

impl Link {
    /// `false` when the session has gone.
    pub fn send(&self, command: Command) -> bool {
        self.hub
            .send(In::Command {
                client: self.client,
                command,
            })
            .is_ok()
    }

    /// The client cannot go on; the session ends with this reason.
    pub(crate) fn failed(&self, reason: String) {
        let _ = self.hub.send(In::Failed(reason));
    }

    /// The client has gone.
    pub(crate) fn leave(&self) {
        let _ = self.hub.send(In::Leave {
            client: self.client,
        });
    }

    /// A line for this client alone.
    pub(crate) fn tell(&self, event: wire::Event) {
        let _ = self.hub.send(In::Tell {
            client: self.client,
            event: Box::new(event),
        });
    }
}

impl std::fmt::Debug for In {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            In::Update(_) => "update",
            In::Join { .. } => "join",
            In::Leave { .. } => "leave",
            In::Command { .. } => "command",
            In::Tell { .. } => "tell",
            In::Failed(_) => "failed",
        })
    }
}

/// A link whose commands are kept rather than sent: a screen with no
/// session behind it, as a test draws one, and what it would have asked.
#[must_use]
pub fn recording() -> (Link, Recording) {
    let (hub, received) = mpsc::channel();
    (Link { client: 0, hub }, Recording(received))
}

/// What a [`recording`] link was asked to send.
pub struct Recording(mpsc::Receiver<In>);

impl Recording {
    /// The commands sent since the last look, oldest first.
    #[must_use]
    pub fn commands(&self) -> Vec<Command> {
        self.0
            .try_iter()
            .filter_map(|sent| match sent {
                In::Command { command, .. } => Some(command),
                _ => None,
            })
            .collect()
    }
}

/// A client joined to a session in this process: its link, and the events
/// meant for it.
pub(crate) struct Joined {
    pub(crate) link: Link,
    pub(crate) events: mpsc::Receiver<Out>,
}

/// Joins `hub` as `name`. Nothing arrives until the client attaches.
pub(crate) fn join(hub: &Hub, name: &str) -> Joined {
    let client = super::hub::next_client();
    let (sink, events) = mpsc::channel();
    let sender = hub.sender();
    let _ = sender.send(In::Join {
        client,
        name: name.to_string(),
        sink,
    });
    Joined {
        link: Link {
            client,
            hub: sender,
        },
        events,
    }
}

/// An approval as a client holds it: the call, why it asks, and how to
/// answer it.
#[derive(Debug)]
pub struct Approval {
    pub id: u64,
    action: crate::approval::Action,
    reason: Option<String>,
    hosts: Vec<String>,
    leaves_sandbox: bool,
    fits: Option<f64>,
    link: Link,
}

impl Approval {
    pub fn action(&self) -> &crate::approval::Action {
        &self.action
    }
    pub fn reason(&self) -> Option<&str> {
        self.reason.as_deref()
    }
    pub fn hosts(&self) -> &[String] {
        &self.hosts
    }
    pub fn leaves_sandbox(&self) -> bool {
        self.leaves_sandbox
    }
    /// The decision model's reading, once it has arrived.
    pub fn hint_line(&self) -> Option<crate::approval::Hint> {
        self.fits
            .map(|fits| crate::approval::Hint { fits, asked_ms: 0 })
    }
    pub fn hint(&mut self, fits: f64) {
        self.fits = Some(fits);
    }
    /// Answers it; `false` when the session has gone.
    pub fn respond(self, decision: crate::approval::Decision) -> bool {
        use crate::approval::Decision;
        let answer = match decision {
            Decision::AllowOnce => Answer::Approval(Choice::AllowOnce),
            Decision::AllowForSession => Answer::Approval(Choice::AllowForSession),
            Decision::Deny => Answer::Approval(Choice::Deny),
            Decision::DenyOnce => Answer::Approval(Choice::DenyOnce),
            Decision::Cancel => Answer::Approval(Choice::Cancel),
            Decision::Redirect(words) => Answer::Redirect(words),
            Decision::AllowHostSession => Answer::Approval(Choice::AllowHostSession),
            Decision::AllowHostAlways => Answer::Approval(Choice::AllowHostAlways),
        };
        self.link.send(Command::Answer {
            prompt: self.id,
            answer,
        })
    }
}

/// A question a cell asked, as a client holds it.
#[derive(Debug)]
pub struct Question {
    pub id: u64,
    question: crate::ask::Question,
    weights: Option<crate::ask::Weights>,
    link: Link,
}

impl Question {
    pub fn question(&self) -> &crate::ask::Question {
        &self.question
    }
    pub fn weights(&self) -> Option<&crate::ask::Weights> {
        self.weights.as_ref()
    }
    pub fn respond(self, answer: crate::ask::Answer) -> bool {
        let answer = match answer.choice {
            Some(choice) => Answer::Choice(choice),
            None => Answer::Dismiss,
        };
        self.link.send(Command::Answer {
            prompt: self.id,
            answer,
        })
    }
}

/// A prompt as a client draws it.
pub enum Raised {
    Approval(Approval),
    Question(Question),
    Form(u64, Box<crate::tui::Form>),
}

/// The prompt `prompt`, answerable through `link`.
#[must_use]
pub fn raised(prompt: Prompt, link: &Link) -> Raised {
    let id = prompt.id;
    match prompt.asks {
        Asks::Approval {
            tool,
            root,
            arguments,
            reason,
            hosts,
            leaves_sandbox,
            fits,
            ..
        } => Raised::Approval(Approval {
            id,
            action: crate::approval::Action::new(&tool, std::path::Path::new(&root), arguments),
            reason,
            hosts,
            leaves_sandbox,
            fits,
            link: link.clone(),
        }),
        Asks::Question {
            question,
            choices,
            weights,
            guess,
        } => Raised::Question(Question {
            id,
            question: crate::ask::Question { question, choices },
            weights: weights.zip(guess).map(|(probabilities, choice)| {
                let confidence = probabilities.iter().copied().fold(0.0, f64::max);
                crate::ask::Weights {
                    probabilities,
                    choice,
                    confidence,
                }
            }),
            link: link.clone(),
        }),
        Asks::Form { form } => Raised::Form(id, form),
    }
}

/// What was answered for the session, as a client lists it, and the way to
/// forget an answer.
#[derive(Clone, Debug)]
pub struct Memory {
    pub entries: Vec<wire::Remembered>,
    link: Link,
}

impl Memory {
    #[must_use]
    pub fn new(entries: Vec<wire::Remembered>, link: Link) -> Self {
        Self { entries, link }
    }
    #[must_use]
    pub fn entries(&self) -> Vec<wire::Remembered> {
        self.entries.clone()
    }
    pub fn forget(&self, id: &str) {
        self.link.send(Command::Forget { id: id.to_string() });
    }
}

/// The hosts a command may reach now, as a client sees them, and the way
/// to let one through or stop letting it.
#[derive(Clone, Debug)]
pub struct Hosts {
    pub hosts: Vec<String>,
    link: Link,
}

impl Hosts {
    #[must_use]
    pub fn new(hosts: Vec<String>, link: Link) -> Self {
        Self { hosts, link }
    }
    pub fn add(&self, host: &str) -> bool {
        self.link.send(Command::Host {
            host: host.to_string(),
            allow: true,
        })
    }
    pub fn remove(&self, host: &str) -> bool {
        self.link.send(Command::Host {
            host: host.to_string(),
            allow: false,
        })
    }
    #[must_use]
    pub fn hosts(&self) -> Vec<String> {
        self.hosts.clone()
    }
}
