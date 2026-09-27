//! One thread owns the terminal and keys; the task thread only sends view state.
use std::cell::RefCell;
use std::collections::HashMap;
use std::io::{self, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crate::contract::{Conversation, ServedBy};
use crate::tui::{self, Activity, Notebook, ScreenState, SidebarVisibility};
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use crossterm::event::{DisableBracketedPaste, EnableBracketedPaste, MouseEventKind};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::{Terminal, backend::CrosstermBackend};

mod console_mode;
mod decision;
mod links;
mod terminal_input;

static ACTIVE: AtomicBool = AtomicBool::new(false);
static DRAWING: Mutex<()> = Mutex::new(());
thread_local! { static OUTPUT: RefCell<Option<mpsc::Sender<Update>>> = const { RefCell::new(None) }; }
thread_local! { static STARTUP: RefCell<Option<Vec<String>>> = const { RefCell::new(None) }; }

/// A handle on the terminal's update channel for a thread that finishes
/// after the task that started it (`after.rs`), or `None` outside a
/// terminal session.
pub(super) fn sender() -> Option<mpsc::Sender<Update>> {
    OUTPUT.with(|output| output.borrow().clone())
}

pub(super) fn output(message: String) {
    if super::output::active() {
        eprintln!("{message}");
        return;
    }
    OUTPUT.with(|output| {
        if let Some(sender) = output.borrow().as_ref() {
            let _ = sender.send(Update::Notice(message));
        } else if let Some(message) = STARTUP.with(|held| match held.borrow_mut().as_mut() {
            Some(notes) => {
                notes.push(message);
                None
            }
            None => Some(message),
        }) {
            println!("{message}");
        }
    });
}

/// A note a terminal session shows elsewhere (its footer), and so does not
/// repeat in the conversation; every other session prints it.
pub(super) fn detail(message: String) {
    if STARTUP.with(|held| held.borrow().is_none()) {
        output(message);
    }
}

/// Holds the notes a session says before its terminal UI exists, so they
/// open the conversation instead of flashing on the screen the UI replaces.
/// [`LiveUi::start`] takes them; a session that ends before it prints them
/// when this guard drops, so a refusal still arrives under its notes.
pub(super) struct StartupNotes;
impl StartupNotes {
    pub(super) fn hold() -> Self {
        STARTUP.with(|held| *held.borrow_mut() = Some(Vec::new()));
        Self
    }
}
impl Drop for StartupNotes {
    fn drop(&mut self) {
        for note in STARTUP
            .with(|held| held.borrow_mut().take())
            .unwrap_or_default()
        {
            println!("{note}");
        }
    }
}

/// **Ask for exactly the mouse reports the UI consumes.** `?1000` is
/// press/release, `?1002` is motion **while a button is held**, and `?1006`
/// is the SGR encoding all three are parsed from. `?1002` was deliberately
/// absent until 2026-09-18, when the user ruled that a plain click-and-drag
/// must select: a terminal offers its own selection only behind a modifier
/// while reporting is on, so Sterna draws the selection itself and needs to see
/// the drag. Still **not** `?1003` (any motion), which reports every pointer
/// movement over the window whether or not anything is pressed, and each
/// report no arm consumes is another chance for a read boundary to split one
/// into text (`terminal_input`). Crossterm has no command for this set, so
/// the bytes are written directly.
///
/// **On Windows these bytes are necessary and not sufficient**, which is why
/// [`enable_mouse_reporting`] exists. They reach the ConPTY emulator and tell
/// it to accept mouse input from the terminal outside it, but crossterm reads
/// Windows input as console records rather than as bytes, and a record only
/// reaches it once `ENABLE_MOUSE_INPUT` is set on the console handle — which
/// is all `EnableMouseCapture` does there (`is_ansi_code_supported` is `false`
/// on Windows, so it writes no `?1002`/`?1003` either). Without that call
/// Sterna's wheel did nothing on Windows at all.
const ENABLE_MOUSE_REPORTING: &[u8] = b"\x1b[?1000h\x1b[?1002h\x1b[?1003h\x1b[?1006h";
/// The matching resets, in the same order.
const DISABLE_MOUSE_REPORTING: &[u8] = b"\x1b[?1000l\x1b[?1002l\x1b[?1003l\x1b[?1006l";

/// The longest the screen goes without a frame while input keeps arriving.
/// A drag or a wheel delivers events faster than a full transcript re-render
/// takes, so the loop draws once per *batch* of input; this is the bound
/// that keeps a continuous stream from starving the screen entirely.
const FRAME: Duration = Duration::from_millis(50);

/// Request mouse reporting in both of the spellings a host can need.
fn enable_mouse_reporting() -> io::Result<()> {
    io::stdout().write_all(ENABLE_MOUSE_REPORTING)?;
    io::stdout().flush()?;
    #[cfg(windows)]
    execute!(io::stdout(), crossterm::event::EnableMouseCapture)?;
    Ok(())
}

/// The reverse, and it must run **before** `disable_raw_mode`: crossterm's
/// `DisableMouseCapture` restores the whole console input mode that was
/// captured when capture was enabled, and that snapshot was already raw — so
/// undoing it afterwards would hand the console straight back to raw mode.
fn disable_mouse_reporting() {
    #[cfg(windows)]
    let _ = execute!(io::stdout(), crossterm::event::DisableMouseCapture);
    let _ = io::stdout().write_all(DISABLE_MOUSE_REPORTING);
    let _ = io::stdout().flush();
}

/// Also called by the existing second-SIGINT exit path, which skips Drop.
pub(super) fn restore_terminal() {
    let _guard = super::lock(&DRAWING);
    if ACTIVE.swap(false, Ordering::SeqCst) {
        disable_mouse_reporting();
        console_mode::disable();
        let _ = disable_raw_mode();
        let _ = execute!(io::stdout(), DisableBracketedPaste);
        let _ = execute!(io::stdout(), LeaveAlternateScreen, crossterm::cursor::Show);
    }
}
struct Restore;
impl Drop for Restore {
    fn drop(&mut self) {
        restore_terminal();
    }
}

/// The next line of the session's stdin, `None` at end of input.
///
/// **One line per lock, rather than `stdin().lock().lines()`.** A lock held
/// for a whole read loop is not reentrant, and `/key` reads the key's own
/// line from inside the input that asked for it -- which would deadlock
/// against a loop still holding the lock it was called from.
pub(super) fn read_line() -> io::Result<Option<String>> {
    let mut line = String::new();
    if io::stdin().read_line(&mut line)? == 0 {
        return Ok(None);
    }
    line.truncate(line.trim_end_matches('\n').trim_end_matches('\r').len());
    Ok(Some(line))
}

pub(super) enum Update {
    Approval(crate::approval::Request),
    /// A question a cell put to the person, waiting on the session thread.
    Ask(crate::ask::Request),
    /// The approval gate's memory, sent once when the gate is made, so the
    /// Ask sheet can list and forget what was answered for the session.
    Memory(crate::approval::Memory),
    Snapshot(Box<(Conversation, Notebook, ServedBy, Activity)>),
    /// Open a form sheet. The terminal thread answers it on the form
    /// channel and on nothing else.
    Form(Box<tui::Form>),
    Model(String),
    /// Whether helpers run and what the subagents are, after a change.
    Tiers(bool, String),
    /// A chip offered first on the opening screen: its label, and what it types.
    Suggest(String, String),
    Delta(String),
    ToolDelta(String),
    /// Readable reasoning as it arrives (`wire::StreamDelta::Reasoning`).
    Reasoning(String),
    Mode(tui::Mode, bool),
    Effort(crate::wire::Effort),
    Panel(Box<tui::Panel>),
    Notice(String),
    /// Work behind the answer started (`true`) or ended, by lane name.
    Behind(&'static str, bool),
    Stop,
    /// The session loop has taken the oldest queued message and is running
    /// it; it is a task now and no longer waiting.
    Dequeued,
}
enum Input {
    Submit(String),
    Exit,
    Failed(String),
}

/// The person's two levers over a task already running, shared with the
/// session loop because that loop is inside a call and cannot read a
/// channel.
///
/// **They are separate levers, and the order matters.** The first Escape
/// raises `stop`, which the task loop reads at a cell boundary: the call in
/// flight finishes, its results are kept, and no further model turn is sent.
/// The second Escape raises `cancel`, which is [`Interrupter::raise`] by
/// another name -- the call in flight is cancelled where it stands. Asking
/// for the gentle one first is what makes the abrupt one safe to offer at
/// all: nothing is destroyed until someone has said so twice.
///
/// [`Interrupter::raise`]: super::Interrupter::raise
#[derive(Default)]
pub(super) struct Steer {
    stop: AtomicBool,
    cancel: AtomicBool,
}

impl Steer {
    /// The first Escape. Idempotent: pressing it twice before the boundary
    /// is read asks for the same thing.
    fn request_stop(&self) {
        self.stop.store(true, Ordering::SeqCst);
    }

    /// The second Escape.
    fn request_cancel(&self) {
        self.cancel.store(true, Ordering::SeqCst);
    }

    /// Read once by the task loop at a cell boundary, and lowered by the
    /// read: a stop ends the turn it was asked during and never the next
    /// one.
    pub(super) fn take_stop(&self) -> bool {
        self.stop.swap(false, Ordering::SeqCst)
    }

    /// Lowers both levers when a task starts: a lever pulled before it began
    /// was meant for a turn that has already ended.
    pub(super) fn clear(&self) {
        self.stop.store(false, Ordering::SeqCst);
        self.cancel.store(false, Ordering::SeqCst);
    }

    /// Read by the interrupt watcher, which owns the cancellation token.
    pub(super) fn take_cancel(&self) -> bool {
        self.cancel.swap(false, Ordering::SeqCst)
    }
}

/// The two channels the terminal thread answers on. A masked prompt's reply
/// has its own, so a secret cannot arrive where a message is expected.
struct Answers<'a> {
    inputs: &'a mpsc::Sender<Input>,
    secrets: &'a mpsc::Sender<Option<Vec<String>>>,
}

pub(super) struct LiveUi {
    handler_cancellations: Arc<Mutex<Vec<String>>>,
    steer: Arc<Steer>,
    updates: mpsc::Sender<Update>,
    inputs: mpsc::Receiver<Input>,
    /// Answers to [`Update::Form`], on their own channel: a secret
    /// must not be able to arrive as an `Input` and be taken for a message.
    secrets: mpsc::Receiver<Option<Vec<String>>>,
    thread: Option<JoinHandle<()>>,
}
impl LiveUi {
    /// Forwards suspended exact actions to the terminal owner. Closing the
    /// terminal drops pending requests and denies their waiting callbacks.
    pub(super) fn approval_gate(
        &self,
        ladder: crate::permissions::Ladder,
    ) -> crate::approval::Gate {
        let (gate, receiver) = crate::approval::Gate::channel(ladder);
        let updates = self.updates.clone();
        let _ = updates.send(Update::Memory(gate.memory()));
        thread::spawn(move || {
            for request in receiver {
                if updates.send(Update::Approval(request)).is_err() {
                    break;
                }
            }
        });
        gate
    }
    /// Forwards questions a cell asked to the terminal owner. Closing the
    /// terminal drops the pending question, which the session reads as
    /// nobody having answered -- never as a reason to wait.
    pub(super) fn ask_gate(&self) -> crate::ask::Gate {
        let (gate, receiver) = crate::ask::Gate::channel();
        let updates = self.updates.clone();
        thread::spawn(move || {
            for request in receiver {
                if updates.send(Update::Ask(request)).is_err() {
                    break;
                }
            }
        });
        gate
    }

    pub(super) fn start(
        mut state: ScreenState,
        conversation: Conversation,
        notebook: Notebook,
    ) -> Result<Self, String> {
        state.messages_seen = conversation.messages.len();
        for note in STARTUP
            .with(|held| held.borrow_mut().take())
            .unwrap_or_default()
        {
            state.note(note);
        }
        let (updates, receiver) = mpsc::channel();
        let (input_sender, inputs) = mpsc::channel();
        let (secret_sender, secrets) = mpsc::channel();
        let (ready_sender, ready) = mpsc::sync_channel(1);
        let handler_cancellations = Arc::new(Mutex::new(Vec::new()));
        let commands = handler_cancellations.clone();
        let steer = Arc::new(Steer::default());
        let levers = steer.clone();
        let thread = thread::spawn(move || {
            let result = run(
                state,
                conversation,
                notebook,
                receiver,
                Answers {
                    inputs: &input_sender,
                    secrets: &secret_sender,
                },
                ready_sender,
                commands,
                levers,
            );
            if let Err(error) = result {
                let _ = input_sender.send(Input::Failed(error.to_string()));
            }
        });
        ready
            .recv()
            .map_err(|_| "terminal thread exited during setup".to_string())??;
        OUTPUT.with(|slot| *slot.borrow_mut() = Some(updates.clone()));
        Ok(Self {
            handler_cancellations,
            steer,
            updates,
            inputs,
            secrets,
            thread: Some(thread),
        })
    }
    /// Opens a form sheet and blocks until it is answered: `Some` is every
    /// field's answer in order, `None` an Esc or a terminal that went away.
    /// **Nothing typed into it reaches the editor, the transcript or the
    /// input history** -- it comes back here and nowhere else.
    pub(super) fn form(&self, form: tui::Form) -> Option<Vec<String>> {
        // A paste nobody collected (a sign-in that ended first) must never
        // answer a form that asks for a key.
        while self.secrets.try_recv().is_ok() {}
        self.updates.send(Update::Form(Box::new(form))).ok()?;
        self.secrets.recv().ok().flatten()
    }
    /// What the person entered in a form the terminal opened on its own,
    /// such as a sign-in panel's paste row, if anything has arrived.
    pub(super) fn try_secret(&self) -> Option<String> {
        self.secrets.try_recv().ok().flatten()?.into_iter().next()
    }
    pub(super) fn handler_cancellations(&self) -> Vec<String> {
        std::mem::take(&mut *super::lock(&self.handler_cancellations))
    }
    pub(super) fn steer(&self) -> &Steer {
        &self.steer
    }
    /// A handle for the interrupt watcher, which outlives no borrow of this.
    pub(super) fn steer_handle(&self) -> Arc<Steer> {
        Arc::clone(&self.steer)
    }
    pub(super) fn next(&self) -> Result<Option<String>, String> {
        match self.inputs.recv() {
            Ok(Input::Submit(text)) => {
                // **The queue empties when this loop takes from it, not when
                // the screen guesses that it has.** Inferring it from the
                // working-to-idle edge missed a task that started in the
                // same breath the last one ended, and left a message
                // standing in the queue while it was already the task.
                let _ = self.updates.send(Update::Dequeued);
                Ok(Some(text))
            }
            Ok(Input::Exit) => Ok(None),
            Ok(Input::Failed(error)) => Err(error),
            Err(_) => Err("terminal input closed".into()),
        }
    }
    pub(super) fn publish(
        &self,
        transcript: &super::Transcript,
        served: &ServedBy,
        activity: Activity,
    ) {
        let _ = self.updates.send(Update::Snapshot(Box::new((
            transcript.conversation.clone(),
            transcript.notebook.clone(),
            served.clone(),
            activity,
        ))));
    }
    /// A publisher onto this terminal's channel that borrows nothing.
    pub(super) fn publisher(&self) -> Publisher {
        Publisher {
            updates: self.updates.clone(),
        }
    }
    pub(super) fn append_delta(&self, text: &str) {
        let _ = self.updates.send(Update::Delta(text.into()));
    }
    pub(super) fn tool_delta(&self, fragment: &str) {
        let _ = self.updates.send(Update::ToolDelta(fragment.into()));
    }
    pub(super) fn reasoning_delta(&self, text: &str) {
        let _ = self.updates.send(Update::Reasoning(text.into()));
    }
    pub(super) fn effort(&self, effort: crate::wire::Effort) {
        let _ = self.updates.send(Update::Effort(effort));
    }
    pub(super) fn mode(&self, mode: tui::Mode, pinned: bool) {
        let _ = self.updates.send(Update::Mode(mode, pinned));
    }
    pub(super) fn panel(&self, panel: tui::Panel) {
        let _ = self.updates.send(Update::Panel(Box::new(panel)));
    }
    pub(super) fn model(&self, model: &str) {
        let _ = self.updates.send(Update::Model(model.into()));
    }
    pub(super) fn suggest(&self, label: &str, types: &str) {
        let _ = self
            .updates
            .send(Update::Suggest(label.into(), types.into()));
    }

    pub(super) fn tiers(&self, helpers_on: bool, subagents: &str) {
        let _ = self
            .updates
            .send(Update::Tiers(helpers_on, subagents.into()));
    }
}
/// Publishes a snapshot while holding no borrow of the [`LiveUi`] it came from.
///
/// The invariant: a caller deeper in the stack than the session loop can draw
/// the screen. A cell blocks the task thread for as long as it runs, so
/// anything it wants shown while it runs -- a helper call in flight -- must
/// publish through a handle it owns; the channel is already `Send`-free and
/// cheap to clone, so this is that same channel without the borrow.
#[derive(Clone)]
pub(super) struct Publisher {
    updates: mpsc::Sender<Update>,
}
impl Publisher {
    pub(super) fn publish(
        &self,
        conversation: &Conversation,
        notebook: &Notebook,
        served: &ServedBy,
        activity: Activity,
    ) {
        let _ = self.updates.send(Update::Snapshot(Box::new((
            conversation.clone(),
            notebook.clone(),
            served.clone(),
            activity,
        ))));
    }
}

/// A publisher with no terminal thread behind it, paired with the receiving
/// end, so `session`'s tests can read what a publish would have drawn.
#[cfg(test)]
pub(super) fn test_publisher() -> (Publisher, mpsc::Receiver<Update>) {
    let (updates, receiver) = mpsc::channel();
    (Publisher { updates }, receiver)
}

/// The line a live session leaves in the terminal once its screen is gone.
static FAREWELL: Mutex<Option<String>> = Mutex::new(None);
/// What ended the live session, said with the farewell: a session that
/// ends the moment it starts must say what ended it.
static ENDED_BY: Mutex<Option<&'static str>> = Mutex::new(None);

fn ended_by(reason: &'static str) {
    *super::lock(&ENDED_BY) = Some(reason);
}

/// Prints `line` where the person will see it after the session: at once
/// without a live screen, and otherwise once [`LiveUi`] has restored the
/// terminal -- sent to the screen it would land on the alternate screen, or
/// on a screen that already stopped reading, and be lost either way.
pub(super) fn farewell(line: String) {
    if OUTPUT.with(|slot| slot.borrow().is_some()) {
        *super::lock(&FAREWELL) = Some(line);
    } else {
        output(line);
    }
}

impl Drop for LiveUi {
    fn drop(&mut self) {
        OUTPUT.with(|slot| *slot.borrow_mut() = None);
        let _ = self.updates.send(Update::Stop);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
        if let Some(line) = super::lock(&FAREWELL).take() {
            if let Some(reason) = super::lock(&ENDED_BY).take() {
                eprintln!("sterna: ended by {reason}");
            }
            eprintln!("{line}");
        }
    }
}

#[derive(Default)]
struct Editor {
    text: String,
    cursor: usize,
    selected: usize,
    history: Vec<String>,
    history_index: Option<usize>,
    draft: String,
}
impl Editor {
    fn previous(&self) -> usize {
        self.text[..self.cursor]
            .char_indices()
            .last()
            .map(|(i, _)| i)
            .unwrap_or(0)
    }
    fn next(&self) -> usize {
        self.text[self.cursor..]
            .chars()
            .next()
            .map(|c| self.cursor + c.len_utf8())
            .unwrap_or(self.cursor)
    }
    fn insert(&mut self, text: &str) {
        // Terminal transports may turn pasted LF into CR. Preserve either
        // newline convention as one LF, while stripping other controls.
        let mut text = text.chars().peekable();
        let mut normalized = String::with_capacity(text.size_hint().0);
        while let Some(c) = text.next() {
            match c {
                '\r' => {
                    if text.peek() == Some(&'\n') {
                        text.next();
                    }
                    normalized.push('\n');
                }
                '\n' | '\t' => normalized.push(c),
                c if !c.is_control() => normalized.push(c),
                _ => {}
            }
        }
        self.text.insert_str(self.cursor, &normalized);
        self.cursor += normalized.len();
        self.selected = 0;
    }
    fn recall(&mut self, older: bool) {
        if self.history.is_empty() {
            return;
        }
        let index = match (self.history_index, older) {
            (None, true) => {
                self.draft = self.text.clone();
                Some(self.history.len() - 1)
            }
            (Some(i), true) => Some(i.saturating_sub(1)),
            (Some(i), false) if i + 1 < self.history.len() => Some(i + 1),
            _ => None,
        };
        self.text = index
            .map(|i| self.history[i].clone())
            .unwrap_or_else(|| self.draft.clone());
        self.history_index = index;
        self.cursor = self.text.len();
        self.selected = 0;
    }
    fn take(&mut self) -> String {
        let text = std::mem::take(&mut self.text);
        if self.history.last() != Some(&text) {
            self.history.push(text.clone());
        }
        self.cursor = 0;
        self.selected = 0;
        self.history_index = None;
        self.draft.clear();
        text
    }
    fn key(&mut self, key: KeyEvent) -> bool {
        let control = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Char('p') if control => self.recall(true),
            KeyCode::Char('n') if control => self.recall(false),
            KeyCode::Char('a') if control => self.cursor = 0,
            KeyCode::Char('e') if control => self.cursor = self.text.len(),
            KeyCode::Char('u') if control => {
                self.text.drain(..self.cursor);
                self.cursor = 0;
            }
            KeyCode::Char('k') if control => {
                self.text.truncate(self.cursor);
            }
            KeyCode::Left => self.cursor = self.previous(),
            KeyCode::Right => self.cursor = self.next(),
            KeyCode::Home => {
                self.cursor = self.text[..self.cursor]
                    .rfind('\n')
                    .map(|i| i + 1)
                    .unwrap_or(0)
            }
            KeyCode::End => {
                self.cursor = self.text[self.cursor..]
                    .find('\n')
                    .map(|i| self.cursor + i)
                    .unwrap_or(self.text.len())
            }
            KeyCode::Backspace => {
                let prev = self.previous();
                self.text.drain(prev..self.cursor);
                self.cursor = prev;
                self.selected = 0;
            }
            KeyCode::Delete => {
                self.text.drain(self.cursor..self.next());
                self.selected = 0;
            }
            KeyCode::Up | KeyCode::Down if !tui::slash_matches(&self.text).is_empty() => {
                let count = tui::slash_matches(&self.text).len();
                self.selected = if key.code == KeyCode::Down {
                    (self.selected + 1) % count
                } else {
                    (self.selected + count - 1) % count
                };
            }
            KeyCode::Up => self.recall(true),
            KeyCode::Down => self.recall(false),
            KeyCode::Tab => {
                if let Some((name, _)) = tui::slash_matches(&self.text).get(self.selected) {
                    self.text = format!("{name} ");
                    self.cursor = self.text.len();
                    self.selected = 0;
                }
            }
            KeyCode::Enter
                if key
                    .modifiers
                    .intersects(KeyModifiers::ALT | KeyModifiers::SHIFT) =>
            {
                self.insert("\n")
            }
            KeyCode::Enter => {
                if let Some((name, _)) = tui::slash_matches(&self.text).get(self.selected) {
                    self.text = name.clone();
                    self.cursor = self.text.len();
                }
                return true;
            }
            KeyCode::Char(c) if !control && !key.modifiers.contains(KeyModifiers::ALT) => {
                self.insert(&c.to_string())
            }
            _ => {}
        }
        false
    }
}

/// Dates every helper call still in flight from the frame it first appeared
/// in, and writes that wall clock into the copy of the notebook this thread
/// is about to draw.
///
/// The invariant: **a running call's elapsed comes from the clock, not from
/// its record.** The cell owns the task thread until the call returns, so
/// the record reaches the screen once, with `elapsed_ms` still zero, and
/// cannot be republished while it runs. `little-helpers.md` makes elapsed
/// text rather than animation precisely so it keeps counting under
/// `/motion off`, where the glyph is frozen and *is it alive* is the only
/// question left. Presentation only: this notebook is the terminal thread's
/// own clone, and a resolved record carries its real duration already.
fn tick_helper_clocks(notebook: &mut Notebook, since: &mut HashMap<(usize, usize), Instant>) {
    const PREFLIGHT: (usize, usize) = (usize::MAX, 0);
    since.retain(|(cell, call), _| {
        if (*cell, *call) == PREFLIGHT {
            return notebook
                .preflight
                .as_ref()
                .is_some_and(tui::helper_in_flight);
        }
        notebook
            .cells
            .get(*cell)
            .and_then(|cell| cell.helpers.get(*call))
            .is_some_and(tui::helper_in_flight)
    });
    if let Some(record) = notebook.preflight.as_mut()
        && tui::helper_in_flight(record)
    {
        let started = since.entry(PREFLIGHT).or_insert_with(Instant::now);
        record.outcome.elapsed_ms = started.elapsed().as_millis() as u64;
    }
    for (index, cell) in notebook.cells.iter_mut().enumerate() {
        for (call, record) in cell.helpers.iter_mut().enumerate() {
            if tui::helper_in_flight(record) {
                let started = since.entry((index, call)).or_insert_with(Instant::now);
                record.outcome.elapsed_ms = started.elapsed().as_millis() as u64;
            }
        }
    }
}

/// Opens a cell's inspection: **the one path `/cell <n>` and a click on that
/// cell's header both take**, so the two routes cannot drift into doing
/// different things (`tui::hit`: every click has a keyboard twin).
fn open_cell(state: &mut ScreenState, notebook: &Notebook, cell: usize) {
    state.inspection = tui::Inspection::open(cell, notebook);
    state.telemetry_open = false;
    if state.inspection.is_none() {
        state.note("No recorded cell at that number yet. Use /cells after an action.");
    }
}

/// Shift-Tab: one rung along the permission ladder, and the line that says
/// where it landed.
///
/// **It takes effect at once, task or no task.** The ladder is an atomic the
/// approval gate reads on the session thread, so a person who moves *down*
/// mid-task is asked about the very next call; moving *up* is their own act
/// and the ladder records it for the rollout.
fn rung_change(state: &mut ScreenState, scope: crate::settings::Scope) -> String {
    let mut to = state.permissions.rung().next();
    // **The key walks the rungs that ask; it cannot walk into the one that
    // does not.** Shift-Tab is one keystroke with no confirmation step, and
    // `full` is the rung where nothing is confirmed ever again -- reachable
    // in one press from `auto`, which is the default. A serious choice is
    // prevented structurally rather than apologised for afterwards, so this
    // key steps over it and `full` keeps the two routes that explain
    // themselves first: the Ask surface, which confirms, and
    // `/permissions full`, which is typed in full.
    //
    // A session *started* on `full` still leaves it here, because stepping
    // over a rung is not the same as being unable to leave one.
    if to == crate::permissions::Rung::Full {
        to = to.next();
    }
    // The new state, then what it means, then the way on: the notice every
    // route that moves the rung prints, and one more sentence.
    format!(
        "{} Shift-Tab again for the next.",
        crate::workbench::facts::set_rung(state, to, scope)
    )
}

/// Turns mouse reporting on or off, sets the state the status line draws
/// from, and says what just happened.
///
/// **What capture actually takes, and what it leaves.** Sterna asks for
/// [`ENABLE_MOUSE_REPORTING`] — `?1000` press/release and the `?1006` SGR
/// encoding — and deliberately **not** `?1002`/`?1003` motion tracking, so a
/// pointer dragged across the window is never reported to Sterna. What the
/// terminal does with that drag is then the terminal's own decision: most
/// keep Shift for native selection whatever the application asked for, and
/// several select on an unmodified drag too once no motion is requested. That
/// is a terminal's behaviour, not a promise this program can make, which is
/// the whole reason for the explicit release below.
///
/// **Released, the terminal owns the pointer again and no click reaches
/// Sterna** — including a click on the marker that would take it back, which is
/// why both routes are keys: `/mouse` and Ctrl-G.
fn set_mouse_capture(state: &mut tui::ScreenState, on: bool) {
    if on {
        if enable_mouse_reporting().is_err() {
            state.note("This terminal did not accept the mouse-mode change.");
            return;
        }
    } else {
        disable_mouse_reporting();
    }
    state.mouse_off = !on;
    state.note(if on {
        "Mouse on: click to open, drag to select. /mouse or Ctrl-G hands it back."
    } else {
        "Mouse released: the terminal owns the pointer. /mouse or Ctrl-G takes it back."
    });
}

#[allow(clippy::too_many_arguments)]
fn run(
    mut state: ScreenState,
    mut conversation: Conversation,
    mut notebook: Notebook,
    updates: mpsc::Receiver<Update>,
    answers: Answers<'_>,
    ready: mpsc::SyncSender<Result<(), String>>,
    handler_cancellations: Arc<Mutex<Vec<String>>>,
    steer: Arc<Steer>,
) -> io::Result<()> {
    let setup = (|| {
        let _guard = super::lock(&DRAWING);
        enable_raw_mode()?;
        let console = console_mode::select();
        ACTIVE.store(true, Ordering::SeqCst);
        execute!(io::stdout(), EnterAlternateScreen, EnableBracketedPaste)?;
        enable_mouse_reporting()?;
        Terminal::new(CrosstermBackend::new(io::stdout())).map(|terminal| (terminal, console))
    })();
    let _restore = Restore;
    let (mut terminal, console) = match setup {
        Ok(ready_terminal) => {
            let _ = ready.send(Ok(()));
            ready_terminal
        }
        Err(error) => {
            let _ = ready.send(Err(error.to_string()));
            return Err(error);
        }
    };
    let mut input = terminal_input::TerminalInput::new(console);
    let mut editor = Editor::default();
    let mut served = ServedBy::default();
    let mut busy = false;
    // A prompt sent while idle, shown at once: the session records it only
    // after its preflight (the decision, the Scout, the acceptance lister),
    // and until then the snapshots it sends do not hold it yet.
    let mut sending: Option<Sending> = None;
    // Whether the previous pass through the loop was working, so the two
    // edges -- a task starting and a task ending -- can be told from the
    // many passes that are neither.
    let mut was_busy = false;
    let started = Instant::now();
    state.activity = Activity::Starting;
    let mut dirty = true;
    let mut last_drawn = Instant::now();
    let mut last_tick = Instant::now();
    let mut task_started: Option<Instant> = None;
    let mut helper_clocks: HashMap<(usize, usize), Instant> = HashMap::new();
    let mut previous_rows = 0usize;
    let mut viewport_height = 10usize;
    // Where the left button went down, so a release knows whether the
    // gesture was a click or a drag.
    let mut workbench = crate::workbench::Workbench::default();
    // When the position indicator came up, so it can go down again after
    // `tui::SCROLL_INDICATOR_LINGER` without a timer of its own.
    let mut last_scroll: Option<Instant> = None;
    // The decision prompts -- approvals, a question, the words behind
    // "another way" -- answered on purpose (`decision.rs`).
    let mut prompts = decision::Prompts::default();
    // Where the open form was drawn, so a click reaches its fields.
    let mut form_hits: Vec<(ratatui::layout::Rect, crate::workbench::FormHit)> = Vec::new();
    loop {
        if !ACTIVE.load(Ordering::SeqCst) {
            break;
        }
        if prompts.retain_pending() {
            dirty = true;
        }
        for update in updates.try_iter() {
            dirty = true;
            match update {
                Update::Approval(request) => {
                    prompts.push_approval(request);
                    state.inspection = None;
                }
                Update::Ask(request) => {
                    prompts.ask(request);
                    state.inspection = None;
                }
                Update::Memory(memory) => state.memory = Some(memory),
                Update::Snapshot(snapshot) => {
                    let (c, n, s, activity) = *snapshot;
                    let completed = n
                        .cells
                        .iter()
                        .filter(|cell| cell.execution.is_some())
                        .count();
                    let previous = notebook
                        .cells
                        .iter()
                        .filter(|cell| cell.execution.is_some())
                        .count();
                    if completed > previous
                        && !state.reduced_motion
                        && n.cells.last().is_some_and(|cell| {
                            cell.error.is_none()
                                && cell.execution.as_deref().is_some_and(|calls| {
                                    !calls.contains(" · failed") && !calls.contains(" · denied")
                                })
                        })
                    {
                        state.completion_tick = Some(0);
                    }
                    refresh_handler_panel(
                        workbench.panel_mut("Standing handlers"),
                        &notebook.handlers,
                        &n.handlers,
                    );
                    workbench
                        .turning_off
                        .retain(|name| n.handlers.iter().any(|h| h.name == *name && h.active));
                    conversation = c;
                    keep_sending(&mut conversation, &mut sending, activity);
                    state.messages_seen = conversation.messages.len();
                    notebook = n;
                    if s.is_known() {
                        served = s;
                    }
                    state.activity = activity;
                    state.streaming_text = None;
                    state.streaming_tool_input = None;
                    state.streaming_reasoning = None;
                    if served.is_known() {
                        state.connected = Some(true);
                    }
                    if matches!(
                        activity,
                        Activity::Idle | Activity::Complete | Activity::Failed
                    ) {
                        // Cancellation may finish the waiting callback before
                        // the user answers. Remove stale confirmations then.
                        prompts.clear_approvals();
                        if let Some(start) = task_started.take() {
                            state.pulse.elapsed_ms = start.elapsed().as_millis() as u64;
                        }
                    } else if task_started.is_none() {
                        task_started = Some(Instant::now());
                    }
                    busy = matches!(
                        activity,
                        Activity::Thinking
                            | Activity::Streaming
                            | Activity::Executing
                            | Activity::Searching
                            | Activity::Waiting
                            | Activity::Compacting
                    );
                }
                Update::Delta(text) => {
                    state.pulse.receive(text.len());
                    state
                        .streaming_text
                        .get_or_insert_with(String::new)
                        .push_str(&text);
                    state.activity = Activity::Streaming;
                    busy = true;
                }
                Update::Reasoning(text) => {
                    state.pulse.receive(text.len());
                    state
                        .streaming_reasoning
                        .get_or_insert_with(String::new)
                        .push_str(&text);
                    busy = true;
                }
                Update::ToolDelta(fragment) => {
                    state.pulse.receive(fragment.len());
                    state
                        .streaming_tool_input
                        .get_or_insert_with(String::new)
                        .push_str(&fragment);
                    state.activity = Activity::Streaming;
                    busy = true;
                }
                Update::Model(model) => state.model = Some(model),
                Update::Suggest(label, types) => {
                    if state.suggestions.is_empty() {
                        state.suggestions = crate::workbench::voice::suggestions(None, 0, false);
                    }
                    state.suggestions.retain(|(_, said)| *said != types);
                    state.suggestions.insert(0, (label, types));
                }
                Update::Tiers(helpers_on, subagents) => {
                    state.helpers_on = helpers_on;
                    state.subagents = Some(subagents);
                }
                Update::Mode(mode, pinned) => {
                    state.mode = mode;
                    state.mode_pinned = pinned;
                }
                Update::Effort(effort) => state.effort = effort,
                // The screen's inbox: the workbench opens it as a sheet on the
                // next frame, as the child of the row that asked for it.
                Update::Panel(panel) => state.panel = Some(*panel),
                Update::Notice(message) => {
                    workbench.notice = message.lines().next().unwrap_or("").to_owned();
                    state.note(message);
                    state.landed_note();
                }
                Update::Behind(lane, running) => state.lane(lane, running),
                // The form draws over the sheet that opened it, which is
                // still there when the form is done or put back.
                Update::Form(form) => {
                    state.form = Some(*form);
                    state.inspection = None;
                }
                Update::Dequeued => {
                    if !state.queued.is_empty() {
                        state.queued.remove(0);
                    }
                }
                Update::Stop => return Ok(()),
            }
        }
        // A stop that was asked for has been answered by the task ending;
        // the next Escape starts the ladder again from its gentle rung.
        if !busy && was_busy && state.stopping {
            state.stopping = false;
            dirty = true;
        }
        was_busy = busy;
        if state.activity == Activity::Starting && started.elapsed() >= Duration::from_millis(350) {
            state.activity = Activity::Idle;
            dirty = true;
        }
        // Frames are owed only while something changes state; an idle
        // screen draws its one heartbeat, or nothing (`tui/look.rs`).
        let moving = busy || state.activity == Activity::Starting || state.settling_or_behind();
        let period = if moving {
            Some(state.frame_period())
        } else {
            state.heartbeat()
        };
        if period.is_some_and(|period| last_tick.elapsed() >= period) {
            if !state.reduced_motion {
                state.animation_frame = state.animation_frame.wrapping_add(1);
            }
            state.advance_landing();
            if let Some(start) = task_started {
                state.pulse.elapsed_ms = start.elapsed().as_millis() as u64;
            }
            state.completion_tick = state
                .completion_tick
                .and_then(|tick| (tick < 5).then_some(tick + 1));
            last_tick = Instant::now();
            dirty = true;
        }
        // A notice that has had its time on the dock's edge is cleared by
        // the next frame, and nothing else would draw one.
        if workbench.notice_expired() {
            dirty = true;
        }
        if state.scrolling
            && last_scroll.is_some_and(|at| at.elapsed() >= tui::SCROLL_INDICATOR_LINGER)
        {
            state.scrolling = false;
            last_scroll = None;
            dirty = true;
        }
        // **One frame per batch of input, not one per event.** Rendering
        // the transcript costs more than the gap between two events of a
        // drag or a wheel flick, so drawing on each one puts the screen
        // behind the hand and leaves a flicked wheel scrolling after it
        // stopped -- the motion a person sees is the queue draining. While
        // more input is already waiting, consume it and draw once.
        let waiting = input.queued() || event::poll(Duration::ZERO)?;
        if dirty && (!waiting || last_drawn.elapsed() >= FRAME) {
            tick_helper_clocks(&mut notebook, &mut helper_clocks);
            state.input = editor.text.clone();
            state.cursor = Some(editor.cursor);
            state.completion_selected = editor.selected;
            let _guard = super::lock(&DRAWING);
            if !ACTIVE.load(Ordering::SeqCst) {
                break;
            }
            let size = terminal.size()?;
            let area = ratatui::layout::Rect::new(0, 0, size.width, size.height);
            let regions = crate::workbench::layout(area, &state);
            viewport_height = usize::from(regions.transcript.height).max(1);
            workbench.absorb_panel(&mut state);
            let document = crate::workbench::Document::build(
                &conversation,
                &notebook,
                &state,
                &workbench,
                usize::from(regions.transcript.width),
            );
            let rows = document.rows.len();
            workbench.anchor_document(&document, &mut state, viewport_height);
            previous_rows = rows;
            if let Some(inspection) = state.inspection.as_mut() {
                inspection.clamp(
                    &conversation,
                    &notebook,
                    regions.transcript.width,
                    regions.transcript.height,
                );
            }
            terminal.draw(|frame| {
                crate::workbench::render(
                    frame,
                    &conversation,
                    &notebook,
                    &state,
                    &served,
                    &mut workbench,
                );
                form_hits = match state.form.as_ref() {
                    Some(form) => crate::workbench::render_form(frame, form, state.theme),
                    None => Vec::new(),
                };
                prompts.draw(frame, state.theme);
            })?;
            io::stdout().flush()?;
            dirty = false;
            last_drawn = Instant::now();
        }
        if !input.queued() && !event::poll(Duration::from_millis(if moving { 40 } else { 100 }))? {
            continue;
        }
        let Some(input_event) = input.read()? else {
            continue;
        };
        // The first thing the person does ends the opening: whatever the
        // session had already said about itself is its card, and anything
        // it says from here is a notice about what they just did.
        if state.startup_notes.is_none() {
            state.startup_notes = Some(state.history.iter().take_while(|n| n.after == 0).count());
        }
        // Security prompts retain priority; no local control can answer them.
        // All ordinary pointer and local-panel events go to the new reducer.
        // Ctrl-C over a selection copies it, even with a prompt up.
        let copying = matches!(&input_event, Event::Key(key)
            if key.code == KeyCode::Char('c')
                && key.modifiers.contains(KeyModifiers::CONTROL))
            && state
                .selection
                .is_some_and(|selection| !selection.is_empty());
        if prompts.active() && !copying {
            let done = match &input_event {
                Event::Key(key) if key.kind != KeyEventKind::Release => prompts.key(*key),
                Event::Paste(text) => prompts.paste(text),
                Event::Mouse(mouse) => match mouse.kind {
                    MouseEventKind::Up(crossterm::event::MouseButton::Left) => {
                        prompts.click(mouse.column, mouse.row)
                    }
                    MouseEventKind::ScrollUp => prompts.wheel(true),
                    MouseEventKind::ScrollDown => prompts.wheel(false),
                    _ => decision::Done::Nothing,
                },
                Event::Resize(_, _) => decision::Done::Redraw,
                _ => decision::Done::Nothing,
            };
            match done {
                decision::Done::Nothing => {}
                decision::Done::Redraw => dirty = true,
                decision::Done::Interrupt => {
                    super::INTERRUPT.store(true, Ordering::SeqCst);
                    dirty = true;
                }
            }
            continue;
        }
        if state.form.is_none() {
            state.input = editor.text.clone();
            state.cursor = Some(editor.cursor);
            if matches!(&input_event, Event::Mouse(mouse) if matches!(mouse.kind, MouseEventKind::ScrollUp | MouseEventKind::ScrollDown))
            {
                last_scroll = Some(Instant::now());
            }
            match workbench.event(&input_event, &mut state, &notebook, busy) {
                crate::workbench::Effect::Insert(command) => {
                    editor.text = command;
                    editor.cursor = editor.text.len();
                    editor.selected = 0;
                    dirty = true;
                    continue;
                }
                crate::workbench::Effect::OpenPath(path) => {
                    workbench.notice = if links::show(std::path::Path::new(&path)) {
                        "Opened file."
                    } else {
                        "No application could open this file."
                    }
                    .into();
                    dirty = true;
                    continue;
                }
                crate::workbench::Effect::Pass => {}
                crate::workbench::Effect::Consumed => {
                    dirty = true;
                    continue;
                }
                crate::workbench::Effect::Ignored => continue,
                crate::workbench::Effect::OpenLink(link) => {
                    say(
                        &mut workbench,
                        if links::open(&link) {
                            "Opened in the browser."
                        } else {
                            "No browser available here; copy the link instead."
                        },
                    );
                    dirty = true;
                    continue;
                }
                crate::workbench::Effect::PasteCallback => {
                    state.form = Some(paste_callback_form());
                    dirty = true;
                    continue;
                }
                crate::workbench::Effect::HandlerOff(name) => {
                    super::lock(&handler_cancellations).push(name.clone());
                    say(&mut workbench, format!("Turning off {name}…"));
                    dirty = true;
                    continue;
                }
                crate::workbench::Effect::Cursor(offset) => {
                    editor.cursor = editor
                        .text
                        .floor_char_boundary(offset.min(editor.text.len()));
                    dirty = true;
                    continue;
                }
                crate::workbench::Effect::Copy(text) => {
                    links::copy(&text);
                    let said = if workbench.is_local() {
                        "Copied."
                    } else {
                        "Copied selection."
                    };
                    say(&mut workbench, said);
                    dirty = true;
                    continue;
                }
                crate::workbench::Effect::Command(command) => {
                    if workbench.local_command(command.trim(), &mut state, &notebook) {
                        // A control that acts on this screen is answered by
                        // this screen. Sending it to the model would spend a
                        // request to be told the command is unknown.
                    } else if !busy {
                        busy = true;
                        workbench.sent(&command, &state);
                        let _ = answers.inputs.send(Input::Submit(command));
                    } else {
                        say(
                            &mut workbench,
                            "Finish the current turn before this action.",
                        );
                    }
                    dirty = true;
                    continue;
                }
            }
        }
        match input_event {
            Event::Resize(_, _) => {
                dirty = true;
            }
            Event::Mouse(mouse) => {
                if let Some(form) = state.form.as_mut() {
                    if mouse.kind == MouseEventKind::Up(crossterm::event::MouseButton::Left) {
                        let hit = form_hits
                            .iter()
                            .rev()
                            .find(|(r, _)| crate::workbench::contains(*r, mouse.column, mouse.row));
                        match hit.map(|(_, hit)| *hit) {
                            Some(crate::workbench::FormHit::Field(i)) => form.focus = i,
                            Some(crate::workbench::FormHit::Word(i, word)) => {
                                form.choose_word(i, word)
                            }
                            Some(crate::workbench::FormHit::Submit) => {
                                if form.enter() {
                                    let given = state.form.take().map(tui::Form::take);
                                    let _ = answers.secrets.send(given);
                                }
                            }
                            Some(crate::workbench::FormHit::Back) => {
                                state.form = None;
                                let _ = answers.secrets.send(None);
                            }
                            None => {}
                        }
                        dirty = true;
                    }
                    continue;
                }
                let up = mouse.kind == MouseEventKind::ScrollUp;
                if up || mouse.kind == MouseEventKind::ScrollDown {
                    state.scrolling = true;
                    last_scroll = Some(Instant::now());
                    if let Some(inspection) = state.inspection.as_mut() {
                        inspection.scroll = if up {
                            inspection.scroll.saturating_sub(3)
                        } else {
                            inspection.scroll.saturating_add(3)
                        };
                    } else if !workbench.is_local() && !state.telemetry_open {
                        state.scrollback = if up {
                            state
                                .scrollback
                                .saturating_add(3)
                                .min(previous_rows.saturating_sub(viewport_height))
                        } else {
                            state.scrollback.saturating_sub(3)
                        };
                    }
                    dirty = true;
                }
            }
            Event::Paste(text) => {
                // Pasting is how most keys are entered, so the masked prompt
                // takes a paste before anything else can.
                if let Some(form) = state.form.as_mut() {
                    form.push(&text);
                } else {
                    editor.insert(&text);
                }
                dirty = true;
            }
            Event::Key(key) if key.kind != KeyEventKind::Release => {
                dirty = true;
                // **Modal, and first.** While a masked prompt is open every
                // key belongs to it: none reaches the editor, the panel, the
                // inspector or the input history.
                if let Some(form) = state.form.as_mut() {
                    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
                    match key.code {
                        KeyCode::Enter => {
                            if form.enter() {
                                let answers_given = state.form.take().map(tui::Form::take);
                                let _ = answers.secrets.send(answers_given);
                            }
                        }
                        KeyCode::Esc => {
                            state.form = None;
                            let _ = answers.secrets.send(None);
                        }
                        KeyCode::Tab | KeyCode::Down => form.move_focus(true),
                        KeyCode::BackTab | KeyCode::Up => form.move_focus(false),
                        KeyCode::Left => form.choose(false),
                        KeyCode::Right => form.choose(true),
                        KeyCode::Char('u') if ctrl => form.clear(),
                        KeyCode::Char('r') if ctrl => form.reveal(),
                        KeyCode::Backspace => form.backspace(),
                        KeyCode::Char(c)
                            if !key
                                .modifiers
                                .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
                        {
                            form.push(&c.to_string());
                        }
                        _ => {}
                    }
                    continue;
                }
                if let Some(inspection) = state.inspection.as_mut() {
                    let handled = match key.code {
                        KeyCode::Esc => {
                            state.inspection = None;
                            true
                        }
                        KeyCode::Left => {
                            inspection.adjacent(false, &notebook);
                            true
                        }
                        KeyCode::Right => {
                            inspection.adjacent(true, &notebook);
                            true
                        }
                        KeyCode::Up => {
                            inspection.scroll = inspection.scroll.saturating_sub(1);
                            true
                        }
                        KeyCode::Down => {
                            inspection.scroll = inspection.scroll.saturating_add(1);
                            true
                        }
                        KeyCode::PageUp => {
                            inspection.scroll = inspection
                                .scroll
                                .saturating_sub(viewport_height.saturating_sub(3));
                            true
                        }
                        KeyCode::PageDown => {
                            inspection.scroll = inspection
                                .scroll
                                .saturating_add(viewport_height.saturating_sub(3));
                            true
                        }
                        KeyCode::Home => {
                            inspection.scroll = 0;
                            true
                        }
                        KeyCode::End => {
                            inspection.scroll = usize::MAX;
                            true
                        }
                        _ => false,
                    };
                    if handled {
                        continue;
                    }
                }
                if state.telemetry_open && !workbench.is_local() {
                    match key.code {
                        KeyCode::Esc => {
                            state.telemetry_open = false;
                            continue;
                        }
                        KeyCode::Up => {
                            state.telemetry_selected = Some(
                                state
                                    .telemetry_selected
                                    .unwrap_or(notebook.requests.len().saturating_sub(1))
                                    .saturating_sub(1),
                            );
                            continue;
                        }
                        KeyCode::Down => {
                            state.telemetry_selected = state
                                .telemetry_selected
                                .and_then(|i| (i + 1 < notebook.requests.len()).then_some(i + 1));
                            continue;
                        }
                        _ => {}
                    }
                }
                if key.code == KeyCode::BackTab {
                    // The permission rung, not the request mode: this is the
                    // one a person reaches for constantly, and — unlike a
                    // request mode, which is a new request — it must move
                    // *while* a task runs, because that is when someone
                    // notices they are on the wrong rung. The request mode
                    // keeps `/mode` and its own sidebar field.
                    state.notice = Some(rung_change(&mut state, workbench.scope()));
                    continue;
                }
                // **Escape, and only while a task runs.** Every panel,
                // modal and inspection above this point takes its own
                // Escape and `continue`s, so reaching here means the
                // composer is what the keyboard is pointed at -- and an
                // Escape into an idle composer has never meant anything, so
                // nothing is taken away by giving it a meaning here.
                //
                // The first press stops at the next cell boundary and the
                // second cancels the call in flight; `state.stopping` is
                // what tells them apart, and the task's end lowers it.
                if key.code == KeyCode::Esc && key.modifiers.is_empty() {
                    if !busy {
                        if state.queued.pop().is_some() {
                            state.note(if state.queued.is_empty() {
                                "Queue cleared.".to_string()
                            } else {
                                format!(
                                    "Took the last queued message back; {} still queued.",
                                    state.queued.len()
                                )
                            });
                        }
                        continue;
                    }
                    if state.stopping {
                        steer.request_cancel();
                        state.note("Cancelling the call in flight.");
                    } else {
                        state.stopping = true;
                        steer.request_stop();
                        state.note(
                            "Stopping after this cell · Esc again cancels the call in flight",
                        );
                    }
                    dirty = true;
                    continue;
                }
                if key.modifiers.contains(KeyModifiers::CONTROL) {
                    match key.code {
                        KeyCode::Char('c') => {
                            if !busy && !editor.text.is_empty() {
                                editor.text.clear();
                                editor.cursor = 0;
                            } else {
                                super::INTERRUPT.store(true, Ordering::SeqCst);
                            }
                            continue;
                        }
                        KeyCode::Char('t') => {
                            state.telemetry_open = !state.telemetry_open;
                            state.inspection = None;
                            workbench.close_all();
                            continue;
                        }
                        KeyCode::Char('o') => {
                            state.compact = !state.compact;
                            continue;
                        }
                        KeyCode::Char('b') => {
                            state.sidebar = match state.sidebar {
                                SidebarVisibility::Hidden => SidebarVisibility::Shown,
                                _ => SidebarVisibility::Hidden,
                            };
                            continue;
                        }
                        KeyCode::Char('f') => {
                            state.fullscreen = !state.fullscreen;
                            continue;
                        }
                        // Give the pointer back to the terminal, and take it
                        // again. Ctrl-G because Ctrl-E, Ctrl-A, Ctrl-U and
                        // Ctrl-K are the composer's, and Ctrl-S and Ctrl-Q
                        // are the terminal's own flow control.
                        KeyCode::Char('g') => {
                            let off = !state.mouse_off;
                            set_mouse_capture(&mut state, !off);
                            continue;
                        }
                        KeyCode::Home => {
                            state.scrollback = previous_rows.saturating_sub(viewport_height);
                            continue;
                        }
                        KeyCode::End => {
                            state.scrollback = 0;
                            continue;
                        }
                        KeyCode::Char('d') if !busy && editor.text.is_empty() => {
                            ended_by("Ctrl-D on an empty prompt");
                            let _ = answers.inputs.send(Input::Exit);
                            return Ok(());
                        }
                        _ => {}
                    }
                }
                match key.code {
                    KeyCode::PageUp => {
                        state.scrollback = state
                            .scrollback
                            .saturating_add(viewport_height.saturating_sub(2))
                            .min(previous_rows.saturating_sub(viewport_height));
                        continue;
                    }
                    KeyCode::PageDown => {
                        state.scrollback = state
                            .scrollback
                            .saturating_sub(viewport_height.saturating_sub(2));
                        continue;
                    }
                    _ => {}
                }
                // **An Enter with more input already behind it is a
                // newline, not a send.** A person who presses Enter to send
                // has nothing queued after it -- they are waiting to see what
                // happens. A multi-line payload arriving as plain keystrokes
                // does, because the rest of it is already in the buffer. That
                // is the only case this rewrite fires in, and it is the case
                // that was silently costing whole tasks: a four-line prompt
                // typed into the pty without bracketed-paste markers sent its
                // first line as the entire task and kept the rest as a draft
                // nobody was told about (measured 2026-09-19).
                //
                // A real paste never reaches here -- `Event::Paste` inserts
                // it whole -- so this is the unbracketed path alone, and
                // `ALT` is the modifier the editor already reads as "insert
                // a newline", so the behaviour is one the composer has.
                //
                // **"Already behind it" means a keystroke, not a console
                // record**, and asking the terminal directly got that wrong
                // on Windows: crossterm emits a release record after every
                // press, so a poll taken the instant Enter was read is
                // answered by Enter's own release, every Enter became a
                // newline, and nothing a Windows user typed was ever sent.
                // It cost the whole `tui_live` target on that cell.
                // `typing_waiting` asks the resolved queue instead.
                let key = if key.code == KeyCode::Enter
                    && key.modifiers.is_empty()
                    && input.typing_waiting()?
                {
                    KeyEvent::new(KeyCode::Enter, KeyModifiers::ALT)
                } else {
                    key
                };
                if editor.key(key) && !editor.text.trim().is_empty() {
                    if workbench.local_command(editor.text.trim(), &mut state, &notebook) {
                        editor.take();
                        dirty = true;
                        continue;
                    }
                    if matches!(
                        editor.text.split_whitespace().next(),
                        Some("/cell" | "/cells" | "/chat")
                    ) {
                        let text = editor.take();
                        let mut words = text.split_whitespace();
                        let command = words.next().unwrap_or_default();
                        if command == "/chat" {
                            state.inspection = None;
                            state.telemetry_open = false;
                        } else {
                            let cell = match words.next() {
                                Some(value) => value.parse::<usize>().unwrap_or(0),
                                None => tui::Inspection::latest(&notebook).unwrap_or(0),
                            };
                            open_cell(&mut state, &notebook, cell);
                            if state.inspection.is_some()
                                && let Some(line) = &notebook.decision
                            {
                                state.note(line.clone());
                            }
                        }
                        continue;
                    }
                    if editor.text.trim() == "/telemetry" {
                        editor.take();
                        state.telemetry_open = !state.telemetry_open;
                        workbench.close_all();
                        state.notice = None;
                        continue;
                    }
                    // Sits with the other argument-less presentation toggles
                    // rather than with `/sidebar`, so it still works while a
                    // task runs -- watching a long stream fill the screen is
                    // the case this command exists for.
                    if editor.text.trim() == "/mouse" {
                        editor.take();
                        let off = !state.mouse_off;
                        set_mouse_capture(&mut state, !off);
                        continue;
                    }
                    if editor.text.trim() == "/fullscreen" {
                        editor.take();
                        state.fullscreen = !state.fullscreen;
                        state.notice = None;
                        continue;
                    }
                    if editor.text.split_whitespace().next() == Some("/handlers") {
                        let text = editor.take();
                        let parts: Vec<_> = text.split_whitespace().collect();
                        match parts.as_slice() {
                            ["/handlers"] => {
                                state.panel = Some(tui::handlers_panel(&notebook.handlers));
                            }
                            ["/handlers", "off", name] => {
                                if busy
                                    && notebook
                                        .handlers
                                        .iter()
                                        .any(|h| h.name == *name && h.active)
                                {
                                    super::lock(&handler_cancellations).push((*name).to_string());
                                    state.note(format!(
                                        "handler {name}: cancellation queued for the next cell boundary"
                                    ));
                                } else {
                                    state.note(format!(
                                        "handler {name}: no active handler with that name"
                                    ));
                                }
                            }
                            _ => state.note("Use /handlers or /handlers off <name>"),
                        }
                        continue;
                    }
                    // **Submitting while a task runs queues, it does not
                    // refuse.** `LiveUi::next` blocks on this same channel
                    // and the session loop reaches it the moment the task
                    // ends, so a message sent now is simply the next one --
                    // no new plumbing, and nothing to re-press. What it used
                    // to do instead was keep the draft and say so in a
                    // notice, which asked the person to watch for an ending
                    // they had already stopped watching for.
                    //
                    // A slash command is not queued: those are this
                    // terminal's own controls and several of them mean
                    // nothing between tasks, so they keep saying what they
                    // have always said.
                    if busy {
                        let text = editor.text.trim().to_string();
                        if text.starts_with('/') {
                            state.notice = Some(
                                "Working. Your draft is kept; Ctrl-C interrupts tools; twice exits."
                                    .into(),
                            );
                            continue;
                        }
                        let text = editor.take();
                        if answers.inputs.send(Input::Submit(text.clone())).is_err() {
                            return Ok(());
                        }
                        state.queued.push(text);
                        state.notice = Some(
                            "Queued for when this turn ends · Esc takes the last one back".into(),
                        );
                        dirty = true;
                        continue;
                    }
                    let text = editor.take();
                    state.scrollback = 0;
                    state.notice = None;
                    if text.trim() == "/exit" {
                        ended_by("/exit");
                        let _ = answers.inputs.send(Input::Exit);
                        return Ok(());
                    }
                    if text.split_whitespace().next() == Some("/statusline") {
                        let word = text.split_whitespace().nth(1).unwrap_or("");
                        let said = match crate::settings_session::save_status(&mut state,word) {
                            Ok(())=>"Status line saved for this project. Selected profile overrides still apply.".into(),
                            Err(error)=>error,
                        };
                        state.note(said);
                        continue;
                    }
                    if text.split_whitespace().next() == Some("/sidebar") {
                        state.sidebar = match text.split_whitespace().nth(1) {
                            Some("hide") => SidebarVisibility::Hidden,
                            Some("show") => SidebarVisibility::Shown,
                            _ => SidebarVisibility::Auto,
                        };
                        state.note("Sidebar: /sidebar auto|show|hide · Ctrl-B toggles");
                        continue;
                    }
                    busy = true;
                    task_started = Some(Instant::now());
                    state.pulse = tui::Pulse::default();
                    state.activity = Activity::Thinking;
                    if !text.trim_start().starts_with('/') && !text.trim().is_empty() {
                        sending = Some(Sending {
                            text: text.clone(),
                            recorded_at: conversation.messages.len(),
                        });
                        keep_sending(&mut conversation, &mut sending, Activity::Thinking);
                        dirty = true;
                    }
                    workbench.sent(&text, &state);
                    if answers.inputs.send(Input::Submit(text)).is_err() {
                        return Ok(());
                    }
                }
            }
            _ => {}
        }
    }
    Ok(())
}

/// A prompt the screen shows before the session has recorded it.
struct Sending {
    text: String,
    /// The conversation's length when it was sent; a longer one holds it.
    recorded_at: usize,
}

/// Shows a sent prompt until the session's own copy arrives: appended to a
/// snapshot that does not hold it yet, dropped once one does or the task
/// ends without it.
fn keep_sending(
    conversation: &mut Conversation,
    sending: &mut Option<Sending>,
    activity: Activity,
) {
    let Some(pending) = sending.as_ref() else {
        return;
    };
    let ended = matches!(
        activity,
        Activity::Idle | Activity::Complete | Activity::Failed
    );
    if ended || conversation.messages.len() > pending.recorded_at {
        *sending = None;
        return;
    }
    conversation.messages.push(crate::contract::Message::text(
        crate::contract::Role::User,
        pending.text.as_str(),
    ));
}

/// The panel is an open view of task state, not a copy frozen at `/handlers`.
///
/// The rows carry the handler's name as their id, so the sheet keeps its
/// focus on the same handler through counter and status changes.
fn refresh_handler_panel(
    panel: Option<&mut tui::Panel>,
    before: &[crate::runtime::handlers::HandlerInfo],
    after: &[crate::runtime::handlers::HandlerInfo],
) {
    let Some(held) = panel else { return };
    let _ = before;
    *held = tui::handlers_panel(after);
}

/// The top sheet's notice when one is open, else the dock's.
fn say(workbench: &mut crate::workbench::Workbench, text: impl Into<String>) {
    let text = text.into();
    match workbench.top_mut() {
        Some(layer) => layer.sheet.notice = text,
        None => workbench.notice = text,
    }
}

/// The form that takes the address a browser ended on after signing in on
/// another device.
fn paste_callback_form() -> tui::Form {
    tui::Form::new(
        "Finish signing in",
        "Signing in on another device? Paste the address your browser ended on after you signed in.",
        vec![tui::form::Field::new(
            "Callback address",
            tui::form::Kind::Text,
            "the whole address from the browser's bar, starting with http",
        )],
    )
    .submit("finish")
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_stop_is_read_once_so_it_ends_the_turn_it_was_asked_during() {
        let steer = super::Steer::default();
        assert!(!steer.take_stop(), "nothing was asked for");
        steer.request_stop();
        steer.request_stop();
        assert!(steer.take_stop(), "the stop never reached the task loop");
        assert!(
            !steer.take_stop(),
            "the stop survived its own turn and would end the next one"
        );
    }

    #[test]
    fn the_two_escapes_are_separate_levers() {
        let steer = super::Steer::default();
        steer.request_stop();
        assert!(
            !steer.take_cancel(),
            "the gentle rung cancelled the call in flight"
        );
        steer.request_cancel();
        assert!(steer.take_cancel());
        assert!(!steer.take_cancel(), "one press cancelled twice");
    }

    use super::*;

    #[test]
    fn a_sent_prompt_shows_until_the_session_records_it() {
        use crate::contract::{Message, Role};
        let mut sending = Some(Sending {
            text: "build a todo app".into(),
            recorded_at: 0,
        });
        // A preflight snapshot without the prompt still shows it.
        let mut early = Conversation::default();
        keep_sending(&mut early, &mut sending, Activity::Thinking);
        assert_eq!(early.messages.len(), 1);
        assert!(sending.is_some());
        // The session's own copy arrives: shown once, not twice.
        let mut recorded = Conversation::default();
        recorded
            .messages
            .push(Message::text(Role::User, "build a todo app"));
        keep_sending(&mut recorded, &mut sending, Activity::Thinking);
        assert_eq!(recorded.messages.len(), 1);
        assert!(sending.is_none());
    }

    /// A call in flight is dated from the frame it first appeared in, so its
    /// lane's seconds keep counting while the cell that made it blocks the
    /// task thread. A call that has resolved keeps what it actually took.
    #[test]
    fn a_running_helper_is_timed_by_the_clock_and_a_resolved_one_by_its_record() {
        use crate::helpers::{HelperOutcome, HelperRecord};
        let running = HelperRecord {
            helper: "reduce".into(),
            verb: "reducing".into(),
            asked: "cargo build log".into(),
            ..HelperRecord::default()
        };
        let resolved = HelperRecord {
            outcome: HelperOutcome {
                text: "3 distinct root failures".into(),
                ok: true,
                cancelled: false,
                elapsed_ms: 120,
            },
            ..running.clone()
        };
        let mut notebook = Notebook::default();
        // `set` numbers cells from one; this is the notebook's first cell.
        notebook.set(
            1,
            tui::CellView {
                helpers: vec![running, resolved],
                ..tui::CellView::default()
            },
        );
        let mut clocks = HashMap::new();
        clocks.insert((0, 0), Instant::now() - Duration::from_millis(1500));
        clocks.insert((0, 1), Instant::now());

        tick_helper_clocks(&mut notebook, &mut clocks);

        let helpers = &notebook.cells[0].helpers;
        assert!(
            helpers[0].outcome.elapsed_ms >= 1500,
            "a running call's elapsed comes from the clock: {}",
            helpers[0].outcome.elapsed_ms
        );
        assert_eq!(
            helpers[1].outcome.elapsed_ms, 120,
            "a resolved call keeps the duration it actually took"
        );
        assert!(
            !clocks.contains_key(&(0, 1)),
            "a call that resolved no longer holds a clock"
        );
    }

    /// The open handler panel is the task's state, not a copy: runs, a
    /// handler going stale and the list emptying all reach it, a stale one
    /// can no longer be turned off, and every row keeps its handler's name
    /// as its id so the sheet's focus stays on it.
    #[test]
    fn an_open_handler_panel_tracks_runs_disable_cancel_and_clear() {
        use crate::runtime::handlers::HandlerInfo;
        let mut before = vec![
            HandlerInfo {
                name: "first".into(),
                runs: 0,
                drained: 0,
                error: None,
                active: true,
            },
            HandlerInfo {
                name: "second".into(),
                runs: 0,
                drained: 0,
                error: None,
                active: true,
            },
        ];
        let mut panel = tui::handlers_panel(&before);
        for phase in 0..4 {
            let mut after = before.clone();
            match phase {
                0 => {
                    after[1].runs = 1;
                    after[1].drained = 1;
                }
                1 => {
                    after[1].active = false;
                    after[1].error = Some("RuntimeTimeout".into());
                }
                2 => {
                    after[0].active = false;
                }
                _ => after.clear(),
            }
            refresh_handler_panel(Some(&mut panel), &before, &after);
            if phase < 3 {
                assert_eq!(panel.rows[2].id.as_deref(), Some("handler:second"));
            }
            if phase == 0 {
                assert!(panel.rows[2].text.contains("1 runs · 1 drained"));
            }
            if phase == 1 {
                assert!(panel.rows[2].text.contains("stale"));
                assert!(panel.rows[2].text.contains("RuntimeTimeout"));
                assert!(!panel.rows[2].acts());
            }
            if phase == 2 {
                assert!(panel.rows[1].text.contains("stale"));
                assert!(!panel.rows[1].acts());
            }
            if phase == 3 {
                assert_eq!(panel.rows.len(), 1);
                assert!(panel.rows[0].text.contains("No handlers in this task"));
            }
            before = after;
        }
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    #[test]
    fn editing_preserves_unicode_boundaries_and_multiline_paste() {
        let mut editor = Editor::default();
        editor.insert("a界\nb");
        editor.key(key(KeyCode::Left));
        editor.key(key(KeyCode::Backspace));
        assert_eq!(editor.text, "a界b");
        editor.key(key(KeyCode::Backspace));
        assert_eq!(editor.text, "ab");
        editor.key(KeyEvent::new(KeyCode::Enter, KeyModifiers::ALT));
        assert_eq!(editor.text, "a\nb");
    }
    #[test]
    fn paste_normalizes_terminal_newlines_without_admitting_controls() {
        let mut editor = Editor::default();
        editor.insert("first\rsecond\r\nthird\nfourth\tcolumn\x00\x1bfinal");
        assert_eq!(editor.text, "first\nsecond\nthird\nfourth\tcolumnfinal");
        assert_eq!(editor.cursor, editor.text.len());
    }
    #[test]
    fn selection_changes_what_tab_completes() {
        let mut editor = Editor::default();
        editor.insert("/");
        editor.key(key(KeyCode::Down));
        editor.key(key(KeyCode::Tab));
        assert_eq!(editor.text, "/models ");
        assert_eq!(editor.cursor, editor.text.len());
    }
    #[test]
    fn history_restores_an_unsent_draft() {
        let mut editor = Editor::default();
        editor.insert("sent");
        editor.take();
        editor.insert("draft");
        editor.recall(true);
        assert_eq!(editor.text, "sent");
        editor.recall(false);
        assert_eq!(editor.text, "draft");
    }

    /// Shift-Tab walks the permission ladder and wraps, and it does not
    /// touch the request mode.
    #[test]
    fn shift_tab_moves_the_rung_and_leaves_the_request_mode_alone() {
        let mut state = ScreenState {
            permissions: crate::permissions::Ladder::new(crate::permissions::Rung::Manual),
            ..ScreenState::default()
        };
        let mode_before = state.mode;
        let mut seen = Vec::new();
        for _ in 0..3 {
            let line = rung_change(&mut state, crate::settings::Scope::Global);
            // Where it landed, and what that does -- the notice every route
            // prints, so this one and the Ask surface cannot drift.
            let rung = state.permissions.rung();
            assert!(
                line.starts_with(&rung.now()),
                "it says where it landed and what that means: {line}"
            );
            seen.push(state.permissions.rung());
        }
        assert_eq!(
            seen,
            vec![
                crate::permissions::Rung::AcceptEdits,
                crate::permissions::Rung::Auto,
                // Not `Full`: the key steps over the rung that stops asking.
                crate::permissions::Rung::Manual,
            ],
            "one rung per press, wrapping to where it started"
        );
        assert_eq!(state.mode, mode_before, "the request mode is untouched");
        // Three moves, not four: the step over Never asks is one move, so
        // the gate never reads that rung, not even for an instant.
        assert_eq!(
            state.permissions.drain_moves().len(),
            3,
            "every move is recorded for the rollout"
        );
    }
}
