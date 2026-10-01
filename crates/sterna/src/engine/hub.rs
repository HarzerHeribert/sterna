//! The engine's side of the seam: one thread that holds everything a client
//! attaches to and everything only the session may touch.
//!
//! **The session says what happened; the hub says it to every client.** The
//! session thread sends [`Update`]s, as it always has; the hub keeps the
//! state a client draws from (`wire::State`), numbers every event, keeps
//! them for a client that attaches later, and sends each to every attached
//! client. A client sends [`Command`]s; the hub alone holds the handles they
//! act on -- the approval waiting on an answer, the stop and cancel levers,
//! the level, the hosts, the sign-in -- so no client holds a piece of the
//! session, and a prompt is answered once, by whichever client answers
//! first.
//!
//! **The queue and the turn's state are the session's.** A message sent
//! while a turn runs waits here and goes when the turn ends, whichever
//! client sent it; whether the turn waits on a person is decided here from
//! the prompts that wait.

use std::collections::BTreeMap;
use std::sync::atomic::Ordering;
use std::sync::{Arc, mpsc};
use std::time::Duration;

use super::wire::{self, Answer, Asks, Command, Envelope, Event, Prompt, State};
use crate::contract::{Conversation, ServedBy};
use crate::session::ui::{Input, Steer};
use crate::tui::{self, Activity, Notebook, Stopper};

/// What the session thread tells the hub.
pub(crate) enum Update {
    Approval(crate::approval::Request),
    /// A question a cell put to the person, waiting on the session thread.
    Ask(crate::ask::Request),
    /// The approval gate's memory, sent once when the gate is made, so a
    /// client can list and forget what was answered for the session.
    Memory(crate::approval::Memory),
    /// The network proxy's live allowed list, sent once when the session
    /// starts one.
    Hosts(crate::sandbox::proxy::Allowed),
    /// The session's live level, which `set_level` moves.
    Level(crate::permissions::LiveLevel),
    /// The transcript and how the turn stands; `None` when a control has
    /// been answered, which leaves the turn's ending and its clock alone.
    Snapshot(Box<(Conversation, Notebook, ServedBy, Option<Activity>)>),
    /// A form, and where its answer goes: `None` when nobody answers.
    Form(Box<tui::Form>, mpsc::SyncSender<Option<Vec<String>>>),
    Model(String),
    /// What the subagents are, after a change.
    Tiers(String),
    /// A chip offered first on the opening screen: its label, and what it types.
    Suggest(String, String),
    /// Take away the opening chip that sends this.
    Unsuggest(String),
    /// A sign-in started beside the session: how to stop it and feed it.
    SignInStarted(crate::session::controls::sign_in::Handle),
    /// What a running sign-in says.
    SignIn(crate::session::controls::sign_in::Event),
    Delta(String),
    ToolDelta(String),
    /// Readable reasoning as it arrives (`wire::StreamDelta::Reasoning`).
    Reasoning(String),
    Effort(crate::wire::Effort),
    Panel(Box<tui::Panel>),
    Notice(String),
    Stop,
}

/// Everything that reaches the hub.
pub(crate) enum In {
    Update(Update),
    /// A client came: its number, its name, and where its lines go.
    Join {
        client: u64,
        name: String,
        sink: mpsc::Sender<Out>,
    },
    Leave {
        client: u64,
    },
    Command {
        client: u64,
        command: Command,
    },
    /// A line for one client alone.
    Tell {
        client: u64,
        event: Box<Event>,
    },
    /// The terminal cannot go on: the session ends with its reason.
    Failed(String),
}

/// What the hub sends one client.
#[derive(Debug, Clone)]
pub(crate) enum Out {
    Event(Arc<Envelope>),
    /// The session ended; nothing more will come.
    Close,
}

/// The hub's own sender, for the session thread and its helpers.
#[derive(Clone)]
pub(crate) struct Hub(mpsc::Sender<In>);

impl Hub {
    pub(crate) fn send(&self, update: Update) -> Result<(), ()> {
        self.0.send(In::Update(update)).map_err(|_| ())
    }
    pub(crate) fn sender(&self) -> mpsc::Sender<In> {
        self.0.clone()
    }
}

/// A client's numbers come from one counter for the whole process, so a
/// client that reconnects is never mistaken for the one before it.
pub(crate) fn next_client() -> u64 {
    use std::sync::atomic::AtomicU64;
    static NEXT: AtomicU64 = AtomicU64::new(1);
    NEXT.fetch_add(1, Ordering::Relaxed)
}

/// Where a level or a host the person allowed is saved: the project root
/// and the user's settings folder, as the session resolved them.
#[derive(Clone, Debug, Default)]
pub(crate) struct Saves {
    pub(crate) root: Option<std::path::PathBuf>,
    pub(crate) global: Option<std::path::PathBuf>,
}

/// Starts the hub's thread for one session, with what a client sees
/// before the session has said anything.
pub(crate) fn start(
    state: State,
    steer: Arc<Steer>,
    inputs: mpsc::Sender<Input>,
    saves: Saves,
) -> (Hub, std::thread::JoinHandle<()>) {
    let (sender, receiver) = mpsc::channel();
    let mut hub = Engine {
        seq: 0,
        log: Vec::new(),
        clients: BTreeMap::new(),
        state,
        steer,
        inputs,
        saves,
        level: None,
        memory: None,
        allowed: None,
        sign_in: crate::session::controls::sign_in::Running::default(),
        pending: BTreeMap::new(),
        next_prompt: 0,
        busy: false,
        sending: None,
        owner: None,
        resumed: Activity::Idle,
        shown_hints: BTreeMap::new(),
    };
    let thread = std::thread::spawn(move || hub.run(&receiver));
    (Hub(sender), thread)
}

/// One prompt waiting on an answer, with the handle that answers it.
enum Pending {
    Approval(crate::approval::Request),
    Ask(crate::ask::Request),
    Form(mpsc::SyncSender<Option<Vec<String>>>, Option<u64>),
}

struct Client {
    name: String,
    sink: mpsc::Sender<Out>,
    attached: bool,
}

/// A message shown before the session has recorded it.
struct Sending {
    text: String,
    /// The conversation's length when it was sent; a longer one holds it.
    recorded_at: usize,
}

struct Engine {
    seq: u64,
    log: Vec<Arc<Envelope>>,
    clients: BTreeMap<u64, Client>,
    state: State,
    steer: Arc<Steer>,
    inputs: mpsc::Sender<Input>,
    saves: Saves,
    level: Option<crate::permissions::LiveLevel>,
    memory: Option<crate::approval::Memory>,
    allowed: Option<crate::sandbox::proxy::Allowed>,
    sign_in: crate::session::controls::sign_in::Running,
    pending: BTreeMap<u64, Pending>,
    next_prompt: u64,
    /// A turn or a control is under way: the session is not reading input.
    busy: bool,
    sending: Option<Sending>,
    /// The client whose input the session is answering: a sheet or a form
    /// the session builds in reply goes to it alone.
    owner: Option<u64>,
    /// What the turn was doing when it began to wait on a person.
    resumed: Activity,
    /// The decision model's reading of each approval, once sent.
    shown_hints: BTreeMap<u64, f64>,
}

impl Engine {
    fn run(&mut self, receiver: &mpsc::Receiver<In>) {
        loop {
            match receiver.recv_timeout(Duration::from_millis(100)) {
                Ok(In::Update(Update::Stop)) => break,
                Ok(In::Update(update)) => self.update(update),
                Ok(In::Join { client, name, sink }) => {
                    self.clients.insert(
                        client,
                        Client {
                            name,
                            sink,
                            attached: false,
                        },
                    );
                }
                Ok(In::Leave { client }) => {
                    self.clients.remove(&client);
                }
                Ok(In::Command { client, command }) => self.command(client, command),
                Ok(In::Tell { client, event }) => self.tell(client, *event),
                Ok(In::Failed(reason)) => {
                    let _ = self.inputs.send(Input::Failed(reason));
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            }
            self.tick();
        }
        self.emit(Event::Ended {
            reason: "the session ended".into(),
        });
        for client in self.clients.values() {
            let _ = client.sink.send(Out::Close);
        }
    }

    // -- out --------------------------------------------------------------

    /// Numbers `event`, keeps it, and sends it to every attached client.
    fn emit(&mut self, event: Event) {
        self.seq += 1;
        self.state.seq = self.seq;
        let envelope = Arc::new(Envelope {
            seq: self.seq,
            at: wire::now_ms(),
            event,
        });
        self.log.push(Arc::clone(&envelope));
        self.clients.retain(|_, client| {
            !client.attached || client.sink.send(Out::Event(Arc::clone(&envelope))).is_ok()
        });
    }

    /// A line for one client only, outside the session's sequence.
    fn tell(&mut self, client: u64, event: Event) {
        let envelope = Arc::new(Envelope {
            seq: 0,
            at: wire::now_ms(),
            event,
        });
        if let Some(found) = self.clients.get(&client)
            && found.sink.send(Out::Event(envelope)).is_err()
        {
            self.clients.remove(&client);
        }
    }

    /// A line for the client whose input this answers, or for every client
    /// when nobody's is.
    fn for_owner(&mut self, event: Event) {
        match self.owner.filter(|owner| self.clients.contains_key(owner)) {
            Some(owner) => self.tell(owner, event),
            None => self.emit(event),
        }
    }

    fn refuse(&mut self, client: u64, to: &str, reason: impl Into<String>) {
        self.tell(
            client,
            Event::Refused {
                to: to.into(),
                reason: reason.into(),
            },
        );
    }

    fn snapshot(&self) -> State {
        let mut state = self.state.clone();
        state.prompts = self
            .pending
            .iter()
            .filter(|(_, pending)| !matches!(pending, Pending::Form(..)))
            .filter_map(|(id, _)| self.prompt_of(*id))
            .collect();
        state
    }

    /// The prompt as it was raised: kept beside the handle for a client
    /// that attaches while it waits.
    fn prompt_of(&self, id: u64) -> Option<Prompt> {
        self.log
            .iter()
            .rev()
            .find_map(|envelope| match &envelope.event {
                Event::Prompt { prompt } if prompt.id == id => Some(prompt.clone()),
                _ => None,
            })
    }

    fn set_activity(&mut self, activity: Activity) {
        if self.state.activity == activity {
            return;
        }
        self.state.activity = activity;
        self.state.since = wire::now_ms();
        let since = self.state.since;
        self.emit(Event::Activity { activity, since });
    }

    fn facts(&mut self) {
        let facts = self.state.facts.clone();
        self.emit(Event::Facts { facts });
    }

    fn queue(&mut self) {
        let items = self.state.queue.clone();
        self.emit(Event::Queue { items });
    }

    // -- the session ------------------------------------------------------

    fn update(&mut self, update: Update) {
        match update {
            Update::Approval(request) => {
                let id = self.prompt_id();
                let action = request.action();
                let confirmation = action.confirmation();
                let asks = Asks::Approval {
                    tool: action.tool().into(),
                    root: action.root().into(),
                    arguments: action.arguments().clone(),
                    label: action.label(),
                    target: action.target(),
                    confirmation: confirmation.text,
                    complete: confirmation.complete,
                    reason: request.reason().map(str::to_string),
                    hosts: request.hosts().to_vec(),
                    leaves_sandbox: request.leaves_sandbox(),
                    fits: request.hint_line().map(|hint| hint.fits),
                };
                if let Some(fits) = request.hint_line().map(|hint| hint.fits) {
                    self.shown_hints.insert(id, fits);
                }
                self.pending.insert(id, Pending::Approval(request));
                self.emit(Event::Prompt {
                    prompt: Prompt { id, asks },
                });
                self.hold();
            }
            Update::Ask(request) => {
                let id = self.prompt_id();
                let question = request.question();
                let asks = Asks::Question {
                    question: question.question.clone(),
                    choices: question.choices.clone(),
                    weights: request.weights().map(|w| w.probabilities.clone()),
                    guess: request.weights().map(|w| w.choice.clone()),
                };
                self.pending.insert(id, Pending::Ask(request));
                self.emit(Event::Prompt {
                    prompt: Prompt { id, asks },
                });
                self.hold();
            }
            Update::Form(form, reply) => {
                let id = self.prompt_id();
                let owner = self.owner;
                self.pending.insert(id, Pending::Form(reply, owner));
                self.for_owner(Event::Prompt {
                    prompt: Prompt {
                        id,
                        asks: Asks::Form { form },
                    },
                });
            }
            Update::Memory(memory) => {
                self.memory = Some(memory);
                self.memory_changed();
            }
            Update::Hosts(allowed) => {
                self.allowed = Some(allowed);
                self.hosts_changed();
            }
            Update::Level(level) => {
                self.state.facts.level = level.level().name().into();
                self.level = Some(level);
            }
            Update::Snapshot(snapshot) => self.record(*snapshot),
            Update::Delta(text) => {
                self.state
                    .streaming
                    .text
                    .get_or_insert_with(String::new)
                    .push_str(&text);
                self.emit(Event::Delta { text });
                self.streaming();
            }
            Update::ToolDelta(text) => {
                self.state
                    .streaming
                    .tool
                    .get_or_insert_with(String::new)
                    .push_str(&text);
                self.emit(Event::ToolDelta { text });
                self.streaming();
            }
            Update::Reasoning(text) => {
                self.state
                    .streaming
                    .reasoning
                    .get_or_insert_with(String::new)
                    .push_str(&text);
                self.emit(Event::Reasoning { text });
            }
            Update::Model(model) => {
                self.state.facts.model = Some(model);
                self.facts();
            }
            Update::Effort(effort) => {
                self.state.facts.effort = effort.name().into();
                self.facts();
            }
            Update::Tiers(subagents) => {
                self.state.facts.subagents = Some(subagents);
                self.facts();
            }
            Update::Suggest(label, types) => {
                self.state.suggestions.retain(|(_, said)| *said != types);
                self.state
                    .suggestions
                    .insert(0, (label.clone(), types.clone()));
                self.emit(Event::Suggest { label, types });
            }
            Update::Unsuggest(types) => {
                self.state.suggestions.retain(|(_, said)| *said != types);
                self.emit(Event::Unsuggest { types });
            }
            Update::SignInStarted(handle) => {
                let label = handle.label.clone();
                self.state.sign_in = Some(label.clone());
                // A new sign-in replaces one still running, which is cancelled.
                self.sign_in = crate::session::controls::sign_in::Running(Some(handle));
                self.for_owner(Event::SignIn {
                    sign_in: wire::SignIn::Started { label },
                });
            }
            Update::SignIn(event) => {
                use crate::session::controls::sign_in::Event as Said;
                let sign_in = match event {
                    Said::Note(text) => {
                        self.state.notes.push(text.clone());
                        wire::SignIn::Note { text }
                    }
                    Said::Panel(panel) => wire::SignIn::Panel { panel },
                    Said::Done => {
                        // Over on its own: nothing is left to stop, and a
                        // setup step it may have finished is counted again.
                        self.sign_in.0 = None;
                        self.state.sign_in = None;
                        let _ = self.inputs.send(Input::Changed);
                        wire::SignIn::Done
                    }
                };
                self.for_owner(Event::SignIn { sign_in });
            }
            Update::Panel(panel) => self.for_owner(Event::Panel { panel }),
            Update::Notice(text) => {
                self.state.notes.push(text.clone());
                self.emit(Event::Notice { text });
            }
            Update::Stop => {}
        }
    }

    fn prompt_id(&mut self) -> u64 {
        self.next_prompt += 1;
        self.next_prompt
    }

    /// The record changed: the clients get it with its reading, the turn's
    /// state follows it, and a turn that ended takes the next queued
    /// message.
    fn record(&mut self, (conversation, notebook, served, activity): RecordParts) {
        let mut conversation = conversation;
        self.keep_sending(&mut conversation, activity.unwrap_or(Activity::Idle));
        self.state.conversation = conversation;
        self.state.notebook = notebook;
        if served.is_known() {
            self.state.served = served.clone();
        }
        self.state.reading = super::reading::of(&self.state.notebook);
        self.state.streaming = wire::Streaming::default();
        let usage = self.usage();
        if usage != self.state.usage {
            self.state.usage = usage;
            self.emit(Event::Usage { usage });
        }
        self.emit(Event::Transcript {
            conversation: Box::new(self.state.conversation.clone()),
            notebook: Box::new(self.state.notebook.clone()),
            served,
            activity,
            reading: self.state.reading.clone(),
        });
        match activity {
            None => self.busy = false,
            Some(activity) => {
                self.busy = activity.working();
                if !self.busy {
                    // A turn that ended takes the prompts nobody answered.
                    self.drop_settled();
                }
                if self.state.activity == Activity::AwaitingYou && self.waiting_on_a_person() {
                    self.resumed = activity;
                } else {
                    self.set_activity(activity);
                }
            }
        }
        if !self.busy {
            self.owner = None;
            if !self.state.queue.is_empty() {
                let text = self.state.queue.remove(0);
                self.queue();
                self.start(None, text);
            }
        }
    }

    /// The session's own count of what its requests used.
    fn usage(&self) -> wire::Usage {
        let requests = &self.state.notebook.requests;
        wire::Usage {
            input_tokens: requests.iter().map(|r| r.input_tokens.unwrap_or(0)).sum(),
            output_tokens: requests.iter().map(|r| r.output_tokens.unwrap_or(0)).sum(),
            reasoned_tokens: self
                .state
                .notebook
                .tokens
                .as_ref()
                .map_or(0, |tokens| tokens.reasoned),
            requests: requests.len() as u64,
        }
    }

    /// The first piece of an answer: the turn is streaming.
    fn streaming(&mut self) {
        if matches!(self.state.activity, Activity::Thinking) {
            self.set_activity(Activity::Streaming);
        }
    }

    /// Shows a sent message until the session's own copy arrives.
    fn keep_sending(&mut self, conversation: &mut Conversation, activity: Activity) {
        let Some(pending) = self.sending.as_ref() else {
            return;
        };
        let ended = matches!(
            activity,
            Activity::Idle | Activity::Complete | Activity::Failed
        );
        if ended || conversation.messages.len() > pending.recorded_at {
            self.sending = None;
            return;
        }
        conversation.messages.push(crate::contract::Message::text(
            crate::contract::Role::User,
            pending.text.as_str(),
        ));
    }

    fn waiting_on_a_person(&self) -> bool {
        self.pending
            .values()
            .any(|pending| matches!(pending, Pending::Approval(_) | Pending::Ask(_)))
    }

    /// A prompt that waits holds the turn: it waits for you.
    fn hold(&mut self) {
        if self.busy && self.waiting_on_a_person() && self.state.activity != Activity::AwaitingYou {
            self.resumed = self.state.activity;
            self.set_activity(Activity::AwaitingYou);
        }
    }

    /// No prompt waits any more: the turn goes on as it was.
    fn release(&mut self) {
        if self.state.activity == Activity::AwaitingYou && !self.waiting_on_a_person() {
            let resumed = self.resumed;
            self.set_activity(resumed);
        }
    }

    // -- clients ----------------------------------------------------------

    fn command(&mut self, client: u64, command: Command) {
        let name = self
            .clients
            .get(&client)
            .map(|c| c.name.clone())
            .unwrap_or_default();
        match command {
            Command::Attach { from } => self.attach(client, from),
            Command::Submit { text, images } => {
                if !images.is_empty() {
                    return self.refuse(client, "submit", "this session takes no images yet");
                }
                self.submit(client, text);
            }
            Command::Control { line } => {
                let line = if line.trim_start().starts_with('/') {
                    line
                } else {
                    format!("/{}", line.trim_start())
                };
                self.submit(client, line);
            }
            Command::TakeBack => {
                if self.state.queue.pop().is_some() {
                    self.queue();
                } else {
                    self.refuse(client, "take_back", "nothing is queued");
                }
            }
            Command::Stop { by } => {
                let by: Stopper = by.unwrap_or(wire::StoppedBy::You).into();
                if by == Stopper::Interrupt {
                    crate::session::interrupt::INTERRUPT.fetch_add(1, Ordering::SeqCst);
                }
                if self.busy {
                    self.steer.request_stop(by);
                } else if by == Stopper::You {
                    self.refuse(client, "stop", "no turn is running");
                }
            }
            Command::Cancel => self.steer.request_cancel(),
            Command::Answer { prompt, answer } => self.answer(client, &name, prompt, answer),
            Command::SetLevel { level, save } => {
                let Some(parsed) = crate::permissions::Level::parse(&level) else {
                    return self.refuse(client, "set_level", format!("there is no level {level}"));
                };
                if let Some(live) = &self.level {
                    live.set(parsed);
                }
                if save {
                    self.save("sandbox.level", parsed.name());
                }
                self.state.facts.level = parsed.name().into();
                self.facts();
            }
            Command::Forget { id } => {
                if let Some(memory) = &self.memory {
                    memory.forget(&id);
                }
                self.memory_changed();
            }
            Command::Host { host, allow } => {
                if let Some(allowed) = &self.allowed {
                    if allow {
                        allowed.add(&host);
                    } else {
                        allowed.remove(&host);
                    }
                }
                self.hosts_changed();
            }
            Command::SignInPaste { text } => match &self.sign_in.0 {
                Some(handle) => {
                    let _ = handle.pastes.send(text);
                }
                None => self.refuse(client, "sign_in_paste", "no sign-in is waiting"),
            },
            Command::SignInCancel => match &self.sign_in.0 {
                Some(handle) => handle.cancel.store(true, Ordering::SeqCst),
                None => self.refuse(client, "sign_in_cancel", "no sign-in is running"),
            },
            Command::Rollback => {
                if self.busy {
                    return self.refuse(client, "rollback", "a turn is running");
                }
                self.start(Some(client), "/rollback now".into());
            }
            Command::Resume { id } => {
                crate::session::resume::switch_to(id);
                self.end();
            }
            Command::End => self.end(),
        }
    }

    fn attach(&mut self, client: u64, from: Option<u64>) {
        let Some(found) = self.clients.get_mut(&client) else {
            return;
        };
        found.attached = true;
        match from {
            Some(from) => {
                let sink = found.sink.clone();
                for envelope in self.log.iter().filter(|e| e.seq >= from) {
                    if sink.send(Out::Event(Arc::clone(envelope))).is_err() {
                        break;
                    }
                }
            }
            None => {
                let state = Box::new(self.snapshot());
                self.tell(client, Event::Snapshot { state });
            }
        }
    }

    /// A message or a command. A message sent while a turn runs waits in the
    /// queue; a control mid-turn applies from the turn's next request, and
    /// any other command waits for the turn to end.
    fn submit(&mut self, client: u64, text: String) {
        let command = text.trim_start().starts_with('/');
        if !self.busy {
            return self.start(Some(client), text);
        }
        if !command {
            self.state.queue.push(text);
            return self.queue();
        }
        if self.state.activity.working() && crate::workbench::mid_turn(text.trim()) {
            self.steer.request_control(text.trim().to_string());
        } else {
            self.refuse(
                client,
                "submit",
                crate::workbench::voice::BETWEEN_TURNS.to_string(),
            );
        }
    }

    /// The session takes `text` now.
    fn start(&mut self, client: Option<u64>, text: String) {
        if text.trim().is_empty() {
            return;
        }
        self.owner = client;
        self.busy = true;
        let message = !text.trim_start().starts_with('/');
        if message {
            self.sending = Some(Sending {
                text: text.clone(),
                recorded_at: self.state.conversation.messages.len(),
            });
        }
        if self.inputs.send(Input::Submit(text)).is_err() {
            self.busy = false;
            return;
        }
        if message {
            // Shown at once, before the session has recorded it.
            let mut conversation = self.state.conversation.clone();
            self.keep_sending(&mut conversation, Activity::Thinking);
            self.state.conversation = conversation;
            self.state.streaming = wire::Streaming::default();
            self.emit(Event::Transcript {
                conversation: Box::new(self.state.conversation.clone()),
                notebook: Box::new(self.state.notebook.clone()),
                served: ServedBy::default(),
                activity: Some(Activity::Thinking),
                reading: self.state.reading.clone(),
            });
            self.set_activity(Activity::Thinking);
        }
    }

    fn end(&mut self) {
        if self.busy {
            self.steer.request_stop(Stopper::You);
            self.steer.request_cancel();
        }
        let _ = self.inputs.send(Input::Exit);
    }

    fn answer(&mut self, client: u64, name: &str, id: u64, answer: Answer) {
        let Some(pending) = self.pending.remove(&id) else {
            let reason = if id > 0 && id <= self.next_prompt {
                "it was already answered"
            } else {
                "there is no such prompt"
            };
            return self.refuse(client, "answer", reason);
        };
        let settled = match (pending, answer) {
            (Pending::Approval(request), answer) => {
                use crate::approval::Decision;
                let hosts = request.hosts().to_vec();
                let decision = match &answer {
                    Answer::Approval(choice) => match choice {
                        wire::Choice::AllowOnce => Decision::AllowOnce,
                        wire::Choice::AllowForSession => Decision::AllowForSession,
                        wire::Choice::Deny => Decision::Deny,
                        wire::Choice::DenyOnce => Decision::DenyOnce,
                        wire::Choice::Cancel => Decision::Cancel,
                        wire::Choice::AllowHostSession if !hosts.is_empty() => {
                            Decision::AllowHostSession
                        }
                        wire::Choice::AllowHostAlways if !hosts.is_empty() => {
                            Decision::AllowHostAlways
                        }
                        _ => {
                            self.pending.insert(id, Pending::Approval(request));
                            return self.refuse(client, "answer", "this call refused no host");
                        }
                    },
                    Answer::Redirect(words) => Decision::Redirect(words.trim().to_string()),
                    Answer::Dismiss => Decision::DenyOnce,
                    _ => {
                        self.pending.insert(id, Pending::Approval(request));
                        return self.refuse(client, "answer", "an approval takes an approval");
                    }
                };
                if decision == Decision::AllowHostAlways {
                    self.keep_hosts(&hosts);
                }
                request.respond(decision);
                answer
            }
            (Pending::Ask(request), answer) => {
                let reply = match &answer {
                    Answer::Choice(choice) if request.question().choices.contains(choice) => {
                        crate::ask::Answer {
                            choice: Some(choice.clone()),
                            by: crate::ask::AnsweredBy::Person,
                        }
                    }
                    Answer::Dismiss => crate::ask::Answer::dismissed(),
                    _ => {
                        self.pending.insert(id, Pending::Ask(request));
                        return self.refuse(client, "answer", "that is not one of the choices");
                    }
                };
                request.respond(reply);
                answer
            }
            (Pending::Form(reply, owner), answer) => {
                let given = match answer {
                    Answer::Form(values) => Some(values),
                    Answer::Dismiss => None,
                    _ => {
                        self.pending.insert(id, Pending::Form(reply, owner));
                        return self.refuse(client, "answer", "a form takes its fields");
                    }
                };
                let _ = reply.send(given);
                // A form's values travel only in the answer.
                Answer::Form(Vec::new())
            }
        };
        self.emit(Event::Settled {
            id,
            by: name.to_string(),
            answer: settled,
        });
        self.release();
    }

    // -- what is shared with the session's own threads -------------------

    /// Prompts the session stopped waiting on: settled by the session.
    fn drop_settled(&mut self) {
        let gone: Vec<u64> = self
            .pending
            .iter()
            .filter(|(_, pending)| match pending {
                Pending::Approval(request) => !request.is_pending(),
                Pending::Ask(_) => !self.busy,
                Pending::Form(..) => false,
            })
            .map(|(id, _)| *id)
            .collect();
        for id in gone {
            self.pending.remove(&id);
            self.emit(Event::Settled {
                id,
                by: "session".into(),
                answer: Answer::Dismiss,
            });
        }
        self.release();
    }

    fn tick(&mut self) {
        if self
            .pending
            .values()
            .any(|p| matches!(p, Pending::Approval(r) if !r.is_pending()))
        {
            self.drop_settled();
        }
        let hints: Vec<(u64, f64)> = self
            .pending
            .iter()
            .filter_map(|(id, pending)| match pending {
                Pending::Approval(request) => request.hint_line().map(|hint| (*id, hint.fits)),
                _ => None,
            })
            .filter(|(id, _)| !self.shown_hints.contains_key(id))
            .collect();
        for (id, fits) in hints {
            self.shown_hints.insert(id, fits);
            self.emit(Event::Hint { id, fits });
        }
        self.memory_changed();
        self.hosts_changed();
    }

    fn memory_changed(&mut self) {
        let Some(memory) = &self.memory else { return };
        let entries: Vec<wire::Remembered> = memory.entries().into_iter().map(Into::into).collect();
        if entries != self.state.memory || self.log.is_empty() {
            self.state.memory = entries.clone();
            self.emit(Event::Memory { entries });
        }
    }

    fn hosts_changed(&mut self) {
        let Some(allowed) = &self.allowed else { return };
        let hosts = allowed.hosts();
        if self.state.hosts.as_ref() != Some(&hosts) {
            self.state.hosts = Some(hosts.clone());
            self.emit(Event::Hosts { hosts });
        }
    }

    /// Saves one global setting where the session's settings live.
    fn save(&mut self, key: &str, value: &str) {
        let (Some(root), Some(global)) = (self.saves.root.clone(), self.saves.global.clone())
        else {
            return;
        };
        let saved = crate::settings::Store::with_global(&root, Some(global)).and_then(|store| {
            let snapshot = store.read(crate::settings::Scope::Global)?;
            store
                .save(
                    crate::settings::Scope::Global,
                    &snapshot,
                    &[(key.to_string(), Some(value.to_string()))],
                )
                .map(|_| ())
        });
        if let Err(error) = saved {
            let text = format!("{key} is set for this session only: {error}");
            self.state.notes.push(text.clone());
            self.emit(Event::Notice { text });
        }
    }

    /// "Always allow": the hosts join the global `sandbox.hosts`.
    fn keep_hosts(&mut self, hosts: &[String]) {
        let (Some(root), Some(global)) = (self.saves.root.clone(), self.saves.global.clone())
        else {
            return;
        };
        let Ok(store) = crate::settings::Store::with_global(&root, Some(global)) else {
            return;
        };
        let mut kept: Vec<String> = store
            .read(crate::settings::Scope::Global)
            .ok()
            .and_then(|snapshot| {
                crate::settings_session::value(&snapshot.values, "sandbox.hosts")
                    .and_then(toml::Value::as_array)
                    .map(|items| {
                        items
                            .iter()
                            .filter_map(toml::Value::as_str)
                            .map(str::to_string)
                            .collect()
                    })
            })
            .unwrap_or_default();
        for host in hosts {
            if !kept.iter().any(|h| h.eq_ignore_ascii_case(host)) {
                kept.push(host.clone());
            }
        }
        let array = toml::Value::Array(kept.into_iter().map(toml::Value::String).collect());
        self.save("sandbox.hosts", &array.to_string());
    }
}

type RecordParts = (Conversation, Notebook, ServedBy, Option<Activity>);
