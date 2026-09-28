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
use editor::Editor;
use ratatui::{Terminal, backend::CrosstermBackend};

mod console_mode;
mod decision;
mod editor;
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

/// Whether this screen is reached over SSH, where a browser or a file
/// viewer would open on the wrong machine.
pub(super) fn over_ssh() -> bool {
    ["SSH_CONNECTION", "SSH_TTY"]
        .iter()
        .any(|name| std::env::var_os(name).is_some_and(|value| !value.is_empty()))
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

/// Asks the terminal to tell Shift-Enter from Enter (the kitty keyboard
/// protocol's first flag). A terminal without the protocol ignores the
/// request, and Alt-Enter stays the newline everywhere. Windows reads
/// console records, which carry the modifiers already.
#[cfg(not(windows))]
fn push_keyboard_protocol() {
    let _ = execute!(
        io::stdout(),
        crossterm::event::PushKeyboardEnhancementFlags(
            crossterm::event::KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES
        )
    );
}
#[cfg(windows)]
fn push_keyboard_protocol() {}
#[cfg(not(windows))]
fn pop_keyboard_protocol() {
    let _ = execute!(io::stdout(), crossterm::event::PopKeyboardEnhancementFlags);
}
#[cfg(windows)]
fn pop_keyboard_protocol() {}

/// Also called by the existing second-SIGINT exit path, which skips Drop.
pub(super) fn restore_terminal() {
    let _guard = super::lock(&DRAWING);
    if ACTIVE.swap(false, Ordering::SeqCst) {
        disable_mouse_reporting();
        console_mode::disable();
        let _ = disable_raw_mode();
        let _ = execute!(io::stdout(), DisableBracketedPaste);
        pop_keyboard_protocol();
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

fn next_input(
    inputs: &mpsc::Receiver<Input>,
    changed: &dyn Fn(),
) -> Result<Option<String>, String> {
    loop {
        match inputs.recv() {
            Ok(Input::Submit(text)) => return Ok(Some(text)),
            Ok(Input::Exit) => return Ok(None),
            Ok(Input::Failed(error)) => return Err(error),
            Ok(Input::Changed) => changed(),
            Err(_) => return Err("terminal input closed".into()),
        }
    }
}

pub(super) enum Update {
    Approval(crate::approval::Request),
    /// A question a cell put to the person, waiting on the session thread.
    Ask(crate::ask::Request),
    /// The approval gate's memory, sent once when the gate is made, so the
    /// Ask sheet can list and forget what was answered for the session.
    Memory(crate::approval::Memory),
    /// The network proxy's live allowed list, sent once when the session
    /// starts one, so the hosts sheet changes what commands reach now.
    Hosts(crate::sandbox::proxy::Allowed),
    /// The transcript and how the turn stands; `None` when a control has
    /// been answered, which leaves the turn's ending and its clock alone.
    Snapshot(Box<(Conversation, Notebook, ServedBy, Option<Activity>)>),
    /// Open a form sheet. The terminal thread answers it on the form
    /// channel and on nothing else.
    Form(Box<tui::Form>),
    Model(String),
    /// Whether helpers run and what the subagents are, after a change.
    Tiers(bool, String),
    /// A chip offered first on the opening screen: its label, and what it types.
    Suggest(String, String),
    /// Take away the opening chip that sends this.
    Unsuggest(String),
    /// A sign-in started beside the session: how to stop it and feed it.
    SignInStarted(super::controls::sign_in::Handle),
    /// What a running sign-in says.
    SignIn(super::controls::sign_in::Event),
    Delta(String),
    ToolDelta(String),
    /// Readable reasoning as it arrives (`wire::StreamDelta::Reasoning`).
    Reasoning(String),
    Effort(crate::wire::Effort),
    Panel(Box<tui::Panel>),
    Notice(String),
    /// Work behind the answer started (`true`) or ended, by lane name.
    Behind(&'static str, bool),
    Stop,
}
enum Input {
    Submit(String),
    Exit,
    Failed(String),
    /// Something the session reads changed on its own -- a sign-in that
    /// finished in the background -- so what the opening screen offers is
    /// worked out again.
    Changed,
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
    /// A stop asked for, and by whom: the first Escape, or Ctrl-C.
    stop: Mutex<Option<tui::Stopper>>,
    cancel: AtomicBool,
    /// A model, mode or effort chosen while the turn runs, applied where
    /// its next request begins (decision 9).
    controls: Mutex<Vec<String>>,
}

impl Steer {
    /// The first Escape, or a Ctrl-C. Idempotent: pressing it twice before
    /// the boundary is read asks for the same thing.
    fn request_stop(&self, by: tui::Stopper) {
        *super::lock(&self.stop) = Some(by);
    }

    /// A control for the turn's next request.
    fn request_control(&self, command: String) {
        super::lock(&self.controls).push(command);
    }

    /// The controls chosen since the last request, oldest first.
    pub(super) fn take_controls(&self) -> Vec<String> {
        std::mem::take(&mut *super::lock(&self.controls))
    }

    /// The second Escape.
    fn request_cancel(&self) {
        self.cancel.store(true, Ordering::SeqCst);
    }

    /// Read once by the task loop at a cell boundary, and lowered by the
    /// read: a stop ends the turn it was asked during and never the next
    /// one.
    pub(super) fn take_stop(&self) -> Option<tui::Stopper> {
        super::lock(&self.stop).take()
    }

    /// Lowers both levers when a task starts: a lever pulled before it began
    /// was meant for a turn that has already ended. A control chosen
    /// between turns is kept: it is for this one.
    pub(super) fn clear(&self) {
        *super::lock(&self.stop) = None;
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
        level: crate::permissions::LiveLevel,
    ) -> crate::approval::Gate {
        let (gate, receiver) = crate::approval::Gate::channel(level);
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
    /// Shares the proxy's live allowed list with the screen.
    pub(super) fn share_hosts(&self, allowed: crate::sandbox::proxy::Allowed) {
        let _ = self.updates.send(Update::Hosts(allowed));
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
        // An answer nobody collected must never answer a form that asks
        // for a key.
        while self.secrets.try_recv().is_ok() {}
        self.updates.send(Update::Form(Box::new(form))).ok()?;
        self.secrets.recv().ok().flatten()
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
    /// The next thing typed; `changed` runs, on the session's thread, for
    /// each change that arrives before it.
    pub(super) fn next(&self, changed: &dyn Fn()) -> Result<Option<String>, String> {
        next_input(&self.inputs, changed)
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
            Some(activity),
        ))));
    }
    /// A control has been answered: the screen takes the transcript as it
    /// now stands and stops waiting, and no turn is reported.
    pub(super) fn control_done(&self, transcript: &super::Transcript) {
        let _ = self.updates.send(Update::Snapshot(Box::new((
            transcript.conversation.clone(),
            transcript.notebook.clone(),
            ServedBy::default(),
            None,
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
    /// Hands the screen a sign-in that now runs beside the session.
    pub(super) fn sign_in(&self, handle: super::controls::sign_in::Handle) {
        let _ = self.updates.send(Update::SignInStarted(handle));
    }
    /// A sender for a thread that reports to the screen on its own.
    pub(super) fn updates(&self) -> mpsc::Sender<Update> {
        self.updates.clone()
    }
    /// Takes the opening chip that sends `types` away.
    pub(super) fn unsuggest(&self, types: &str) {
        let _ = self.updates.send(Update::Unsuggest(types.into()));
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
            Some(activity),
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
        // Asked before the key reader starts, so its replies are read here.
        crate::tui::background::ask();
        let console = console_mode::select();
        ACTIVE.store(true, Ordering::SeqCst);
        execute!(io::stdout(), EnterAlternateScreen, EnableBracketedPaste)?;
        push_keyboard_protocol();
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
    if state.background == crate::tui::background::Background::Auto {
        state.light = crate::tui::background::detected();
    }
    let mut input = terminal_input::TerminalInput::new(console);
    let mut editor = Editor::default();
    editor.root = state.settings_root.clone();
    let mut served = ServedBy::default();
    let mut busy = false;
    // A sign-in running beside the session: its handle, its latest panel,
    // whether that panel has been shown, and whether the open form is the
    // one that takes its pasted address.
    let mut sign_in = super::controls::sign_in::Running::default();
    let mut sign_in_panel: Option<tui::Panel> = None;
    let mut sign_in_shown = false;
    let mut paste_form = false;
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
    let mut clock = Clock::default();
    // When an idle Ctrl-C armed the quit: a second one within the window
    // ends the session, and the notice that says so goes when it lapses.
    let mut quit_armed: Option<Instant> = None;
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
                }
                Update::Ask(request) => {
                    prompts.ask(request);
                }
                Update::Memory(memory) => state.memory = Some(memory),
                Update::Hosts(allowed) => state.allowed = Some(allowed),
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
                    // A cell that ran may have made or removed files.
                    if completed > previous {
                        crate::tui::forget_paths();
                    }
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
                    keep_sending(
                        &mut conversation,
                        &mut sending,
                        activity.unwrap_or(Activity::Idle),
                    );
                    state.messages_seen = conversation.messages.len();
                    notebook = n;
                    if s.is_known() {
                        served = s;
                    }
                    if served.is_known() {
                        state.connected = Some(true);
                    }
                    // A control answered: nothing waits any more, and the
                    // last turn's ending, clock and pulse stay as they were.
                    let Some(activity) = activity else {
                        busy = false;
                        continue;
                    };
                    state.activity = activity;
                    state.streaming_text = None;
                    state.streaming_tool_input = None;
                    state.streaming_reasoning = None;
                    if !activity.working() {
                        // The turn may have made or removed files.
                        crate::tui::forget_paths();
                        // Cancellation may finish the waiting callback before
                        // the user answers. Remove stale confirmations then.
                        prompts.clear_approvals();
                        if let Some(ms) = clock.stop() {
                            state.pulse.elapsed_ms = ms;
                        }
                    } else if clock.started.is_none() {
                        // A turn started by a project command or skill,
                        // which the screen sent as a control.
                        clock.start();
                        state.pulse = tui::Pulse::default();
                    }
                    busy = activity.working();
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
                Update::Unsuggest(types) => {
                    state.suggestions.retain(|(_, said)| *said != types);
                }
                // A new sign-in replaces one still running: the older one
                // is cancelled rather than left holding its callback port.
                Update::SignInStarted(handle) => {
                    state.signing_in = Some(handle.label.clone());
                    // Replacing the hold cancels the older one.
                    sign_in = super::controls::sign_in::Running(Some(handle));
                    sign_in_shown = false;
                }
                Update::SignIn(super::controls::sign_in::Event::Note(message)) => {
                    workbench.notice = message.lines().next().unwrap_or("").to_owned();
                    state.note(message);
                    state.landed_note();
                }
                // The panel opens once; after Esc it is kept, and the dock's
                // "signing in" chip brings it back.
                Update::SignIn(super::controls::sign_in::Event::Panel(panel)) => {
                    if let Some(open) = workbench.panel_mut(&panel.title) {
                        *open = (*panel).clone();
                    } else if state
                        .panel
                        .as_ref()
                        .is_some_and(|pending| pending.title == panel.title)
                        || !sign_in_shown
                    {
                        // Not drawn yet, or never shown: this one opens.
                        state.panel = Some((*panel).clone());
                        sign_in_shown = true;
                    }
                    sign_in_panel = Some(*panel);
                }
                Update::SignIn(super::controls::sign_in::Event::Done) => {
                    // Over on its own: nothing left to stop, and a setup
                    // step it may have finished is counted again.
                    sign_in.0 = None;
                    state.signing_in = None;
                    let _ = answers.inputs.send(Input::Changed);
                }
                Update::Tiers(helpers_on, subagents) => {
                    state.helpers_on = helpers_on;
                    state.subagents = Some(subagents);
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
                }
                Update::Stop => return Ok(()),
            }
        }
        // An approval or a question on screen mid-turn: the card and the dock
        // say the turn waits for the person, and its clock stands still.
        if clock.hold(busy && prompts.active(), &mut state.activity) {
            dirty = true;
        }
        // The session is free: the oldest held message is the next turn.
        if !busy && !state.queued.is_empty() {
            let text = state.queued.remove(0);
            if state.queued.is_empty() {
                unqueue_notice(&mut state);
            }
            let Ok(pending) = submit(
                text,
                &mut state,
                &mut clock,
                &mut workbench,
                answers.inputs,
                conversation.messages.len(),
            ) else {
                return Ok(());
            };
            busy = true;
            sending = pending;
            keep_sending(&mut conversation, &mut sending, Activity::Thinking);
            dirty = true;
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
            if let Some(ms) = clock.elapsed_ms() {
                state.pulse.elapsed_ms = ms;
            }
            state.completion_tick = state
                .completion_tick
                .and_then(|tick| (tick < 5).then_some(tick + 1));
            last_tick = Instant::now();
            dirty = true;
        }
        // A notice that has had its time on the dock's edge is cleared by
        // the next frame, and nothing else would draw one.
        if quit_armed.is_some_and(|at| at.elapsed() >= super::DOUBLE_INTERRUPT_WINDOW) {
            quit_armed = None;
            if state.notice.as_deref() == Some(crate::workbench::voice::QUIT_ARMED) {
                state.notice = None;
            }
            dirty = true;
        }
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
            state.completions = editor.completions().rows();
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
                decision::Done::KeepHosts(hosts) => {
                    let notice = crate::workbench::facts::keep_hosts(&state, &hosts);
                    state.note(notice);
                    dirty = true;
                }
                // The same Ctrl-C as over a running turn: it stops the
                // turn, and says so.
                decision::Done::Interrupt => {
                    super::INTERRUPT.fetch_add(1, Ordering::SeqCst);
                    state.stopping = true;
                    steer.request_stop(tui::Stopper::Interrupt);
                    state.note(crate::workbench::voice::CTRL_C_STOPPING);
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
            let turn = busy && state.activity.working();
            let mut effect = workbench.event(&input_event, &mut state, &notebook, turn);
            // A popup row: a path completes in place; a command runs.
            if let crate::workbench::Effect::Completion(index) = effect {
                editor.selected = index;
                effect = match editor.complete(index) {
                    Some(true) => crate::workbench::Effect::Command(editor.take()),
                    _ => crate::workbench::Effect::Consumed,
                };
            }
            match effect {
                crate::workbench::Effect::Insert(command) => {
                    editor.replace(command);
                    dirty = true;
                    continue;
                }
                crate::workbench::Effect::Draft(message) => {
                    editor.replace(if editor.text.trim().is_empty() {
                        message
                    } else {
                        format!("{}\n\n{message}", editor.text.trim_end())
                    });
                    dirty = true;
                    continue;
                }
                crate::workbench::Effect::PopupMove(down) => {
                    editor.move_selection(down);
                    dirty = true;
                    continue;
                }
                crate::workbench::Effect::OpenPath(path) => {
                    let file = std::path::Path::new(&path);
                    let named = state
                        .settings_root
                        .as_deref()
                        .and_then(|root| file.strip_prefix(root).ok())
                        .unwrap_or(file)
                        .display()
                        .to_string();
                    workbench.notice = match links::show(file) {
                        links::Shown::Opened => "Opened file.".into(),
                        links::Shown::OverSsh => {
                            links::copy(&path);
                            "Can't open files over SSH · path copied".into()
                        }
                        links::Shown::Missing => format!("{named} no longer exists"),
                        links::Shown::NoOpener => format!("Nothing here opens {named}"),
                    };
                    dirty = true;
                    continue;
                }
                crate::workbench::Effect::Pass => {}
                // A popup row was taken above, before this match.
                crate::workbench::Effect::Consumed | crate::workbench::Effect::Completion(_) => {
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
                    if sign_in.0.is_some() {
                        state.form = Some(paste_callback_form());
                        paste_form = true;
                    } else {
                        say(&mut workbench, "No sign-in is waiting for an address.");
                    }
                    dirty = true;
                    continue;
                }
                // The resume sheet chose another session: this one ends as
                // `/exit` ends it, and the chosen one starts in its place.
                crate::workbench::Effect::Resume(id) => {
                    super::resume::switch_to(id);
                    if busy {
                        steer.request_stop(tui::Stopper::You);
                        steer.request_cancel();
                    }
                    let _ = answers.inputs.send(Input::Exit);
                    return Ok(());
                }
                crate::workbench::Effect::CancelSignIn => {
                    if let Some(handle) = &sign_in.0 {
                        handle
                            .cancel
                            .store(true, std::sync::atomic::Ordering::SeqCst);
                        say(
                            &mut workbench,
                            format!("Cancelling the sign-in to {}…", handle.label),
                        );
                    }
                    dirty = true;
                    continue;
                }
                crate::workbench::Effect::ReopenSignIn => {
                    if let Some(panel) = &sign_in_panel {
                        state.panel = Some(panel.clone());
                    }
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
                    if command.trim() == "/exit" {
                        ended_by("/exit");
                        if busy {
                            steer.request_stop(tui::Stopper::You);
                            steer.request_cancel();
                        }
                        let _ = answers.inputs.send(Input::Exit);
                        return Ok(());
                    }
                    if workbench.local_command(command.trim(), &mut state, &notebook) {
                        // A control that acts on this screen is answered by
                        // this screen. Sending it to the model would spend a
                        // request to be told the command is unknown.
                    } else if !(busy && state.activity.working()) {
                        busy = true;
                        workbench.sent(&command, &state);
                        let _ = answers.inputs.send(Input::Submit(command));
                    } else if crate::workbench::mid_turn(&command) {
                        workbench.sent(&command, &state);
                        steer.request_control(command);
                        say(&mut workbench, crate::workbench::voice::NEXT_REQUEST);
                    } else {
                        say(&mut workbench, crate::workbench::voice::BETWEEN_TURNS);
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
                                    answer_form(
                                        given,
                                        &mut paste_form,
                                        sign_in.0.as_ref(),
                                        answers.secrets,
                                    );
                                }
                            }
                            Some(crate::workbench::FormHit::Back) => {
                                state.form = None;
                                answer_form(
                                    None,
                                    &mut paste_form,
                                    sign_in.0.as_ref(),
                                    answers.secrets,
                                );
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
                    if !workbench.is_local() && !state.telemetry_open {
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
                                answer_form(
                                    answers_given,
                                    &mut paste_form,
                                    sign_in.0.as_ref(),
                                    answers.secrets,
                                );
                            }
                        }
                        KeyCode::Esc => {
                            state.form = None;
                            answer_form(None, &mut paste_form, sign_in.0.as_ref(), answers.secrets);
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
                // **Escape, and only while a task runs.** Every panel,
                // modal and sheet above this point takes its own
                // Escape and `continue`s, so reaching here means the
                // composer is what the keyboard is pointed at -- and an
                // Escape into an idle composer has never meant anything, so
                // nothing is taken away by giving it a meaning here.
                //
                // The first press stops at the next cell boundary and the
                // second cancels the call in flight; `state.stopping` is
                // what tells them apart, and the task's end lowers it.
                if key.code == KeyCode::Esc && key.modifiers.is_empty() {
                    // An open popup is put away first; with it away, a
                    // second Escape takes back the word it was for.
                    if editor.dismiss() {
                        continue;
                    }
                    if !busy && editor.dismissed() {
                        editor.drop_popup_word();
                        continue;
                    }
                    // A message still in the queue is taken back first, into
                    // the composer, before Escape means stop.
                    if let Some(taken) = state.queued.pop() {
                        editor.text = if editor.text.trim().is_empty() {
                            taken
                        } else {
                            format!("{taken}\n{}", editor.text)
                        };
                        editor.cursor = editor.text.len();
                        if state.queued.is_empty() {
                            unqueue_notice(&mut state);
                            state.note("Took the queued message back into the composer.");
                        } else {
                            state.note(format!(
                                "Took the last queued message back; {} still queued.",
                                state.queued.len()
                            ));
                        }
                        dirty = true;
                        continue;
                    }
                    if !busy {
                        // Idle with nothing typed, Escape leaves fullscreen.
                        if state.fullscreen && editor.text.is_empty() {
                            workbench.local_command("/fullscreen", &mut state, &notebook);
                            dirty = true;
                        }
                        continue;
                    }
                    if state.stopping {
                        steer.request_cancel();
                        state.note("Cancelling the call in flight.");
                    } else {
                        state.stopping = true;
                        steer.request_stop(tui::Stopper::You);
                        state.note(
                            "Stopping after this cell · Esc again cancels the call in flight",
                        );
                    }
                    dirty = true;
                    continue;
                }
                if key.modifiers.contains(KeyModifiers::CONTROL) {
                    match key.code {
                        // **One Ctrl-C, one meaning**, in this order. A
                        // selection was copied before this, and a pending
                        // approval took the key before that. A turn is
                        // interrupted; a draft is cleared, and Ctrl-Z brings
                        // it back; an empty composer arms the quit.
                        KeyCode::Char('c') => {
                            if busy && state.activity.working() {
                                super::INTERRUPT.fetch_add(1, Ordering::SeqCst);
                                state.stopping = true;
                                steer.request_stop(tui::Stopper::Interrupt);
                                state.note(crate::workbench::voice::CTRL_C_STOPPING);
                            } else if !editor.text.is_empty() {
                                editor.clear();
                                state.note(crate::workbench::voice::DRAFT_CLEARED);
                            } else {
                                super::INTERRUPT.fetch_add(1, Ordering::SeqCst);
                                state.notice = Some(crate::workbench::voice::QUIT_ARMED.into());
                                quit_armed = Some(Instant::now());
                            }
                            dirty = true;
                            continue;
                        }
                        KeyCode::Char('t') => {
                            state.telemetry_open = !state.telemetry_open;
                            workbench.close_all();
                            continue;
                        }
                        KeyCode::Char('o') => {
                            state.compact = !state.compact;
                            continue;
                        }
                        // The same route as `/sidebar`: saved, noted, and
                        // one undo away.
                        KeyCode::Char('b') => {
                            let word = match state.sidebar {
                                SidebarVisibility::Hidden => "show",
                                _ => "hide",
                            };
                            workbench.local_command(
                                &format!("/sidebar {word}"),
                                &mut state,
                                &notebook,
                            );
                            continue;
                        }
                        // The same route as `/fullscreen`, and said the same way.
                        KeyCode::Char('f') => {
                            workbench.local_command("/fullscreen", &mut state, &notebook);
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
                        KeyCode::Char('d') if editor.text.is_empty() => {
                            state.note(crate::workbench::voice::CTRL_D_BUSY);
                            continue;
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
                        workbench.local_command("/fullscreen", &mut state, &notebook);
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
                    // /exit is honoured mid-turn: the turn is stopped and the
                    // session ends when it has.
                    if editor.text.trim() == "/exit" {
                        editor.take();
                        ended_by("/exit");
                        if busy {
                            steer.request_stop(tui::Stopper::You);
                            steer.request_cancel();
                        }
                        let _ = answers.inputs.send(Input::Exit);
                        return Ok(());
                    }
                    let turn = busy && state.activity.working();
                    // A slash command mid-turn is a control: a model, mode
                    // or effort applies from the turn's next request, and
                    // the rest wait for the turn to end.
                    if turn && editor.text.trim_start().starts_with('/') {
                        let text = editor.text.trim().to_string();
                        if crate::workbench::mid_turn(&text) {
                            editor.take();
                            workbench.sent(&text, &state);
                            steer.request_control(text);
                            state.notice = Some(crate::workbench::voice::NEXT_REQUEST.into());
                        } else {
                            state.notice = Some(crate::workbench::voice::BETWEEN_TURNS.into());
                        }
                        dirty = true;
                        continue;
                    }
                    // **A message sent while the session is busy is held in
                    // Sterna's queue** and sent when it is free, so Escape
                    // can take it back until then (decision 8).
                    if busy && !editor.text.trim_start().starts_with('/') {
                        state.queued.push(editor.take());
                        state.notice = Some(crate::workbench::voice::QUEUED.into());
                        dirty = true;
                        continue;
                    }
                    let text = editor.take();
                    state.scrollback = 0;
                    state.notice = None;
                    let Ok(pending) = submit(
                        text,
                        &mut state,
                        &mut clock,
                        &mut workbench,
                        answers.inputs,
                        conversation.messages.len(),
                    ) else {
                        return Ok(());
                    };
                    busy = true;
                    if pending.is_some() {
                        sending = pending;
                        keep_sending(&mut conversation, &mut sending, Activity::Thinking);
                        dirty = true;
                    }
                }
            }
            _ => {}
        }
    }
    Ok(())
}

/// Sends one input to the session. A message starts a turn and its clock,
/// and comes back as the line the screen shows until the session records
/// it; a slash command is a control and starts neither. `Err` when the
/// session has gone.
fn submit(
    text: String,
    state: &mut ScreenState,
    clock: &mut Clock,
    workbench: &mut crate::workbench::Workbench,
    inputs: &mpsc::Sender<Input>,
    recorded_at: usize,
) -> Result<Option<Sending>, ()> {
    let message = !text.trim_start().starts_with('/') && !text.trim().is_empty();
    let pending = message.then(|| {
        clock.start();
        state.pulse = tui::Pulse::default();
        state.activity = Activity::Thinking;
        Sending {
            text: text.clone(),
            recorded_at,
        }
    });
    workbench.sent(&text, state);
    inputs.send(Input::Submit(text)).map_err(|_| ())?;
    Ok(pending)
}

/// The queue notice goes when the queue is empty.
fn unqueue_notice(state: &mut ScreenState) {
    if state.notice.as_deref() == Some(crate::workbench::voice::QUEUED) {
        state.notice = None;
    }
}

/// A turn's clock: when it started, and how long it has stood still waiting
/// on the person's answer, which is not time the turn spent working.
#[derive(Default)]
struct Clock {
    started: Option<Instant>,
    /// When the current wait on the person began.
    waiting: Option<Instant>,
    /// The waits already over.
    held: Duration,
    /// What the turn was doing when the wait began.
    resumed: Activity,
}

impl Clock {
    fn start(&mut self) {
        *self = Self {
            started: Some(Instant::now()),
            ..Self::default()
        };
    }
    fn elapsed_ms(&self) -> Option<u64> {
        let held = self.held + self.waiting.map_or(Duration::ZERO, |at| at.elapsed());
        self.started
            .map(|start| start.elapsed().saturating_sub(held).as_millis() as u64)
    }
    /// The turn ended: its working time, and the clock is put away.
    fn stop(&mut self) -> Option<u64> {
        let ms = self.elapsed_ms();
        *self = Self::default();
        ms
    }
    /// Holds the clock while the turn waits on the person and shows it as
    /// waiting for them; lets it run again, and restores what the turn was
    /// doing, once they have answered. True when the screen changed.
    fn hold(&mut self, waiting: bool, activity: &mut Activity) -> bool {
        match (waiting, self.waiting) {
            (true, None) => {
                self.waiting = Some(Instant::now());
                self.resumed = *activity;
                *activity = Activity::AwaitingYou;
                true
            }
            // A snapshot mid-wait said what the turn is doing underneath.
            (true, Some(_)) if *activity != Activity::AwaitingYou => {
                self.resumed = *activity;
                *activity = Activity::AwaitingYou;
                true
            }
            (false, Some(at)) => {
                self.held += at.elapsed();
                self.waiting = None;
                if *activity == Activity::AwaitingYou {
                    *activity = self.resumed;
                }
                true
            }
            _ => false,
        }
    }
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

/// A form's answer, to whoever asked: the sign-in running beside the session
/// for a pasted address, else the session thread waiting on the form.
fn answer_form(
    given: Option<Vec<String>>,
    paste_form: &mut bool,
    sign_in: Option<&super::controls::sign_in::Handle>,
    secrets: &mpsc::Sender<Option<Vec<String>>>,
) {
    if std::mem::take(paste_form) {
        if let (Some(address), Some(handle)) =
            (given.and_then(|given| given.into_iter().next()), sign_in)
        {
            let _ = handle.pastes.send(address);
        }
        return;
    }
    let _ = secrets.send(given);
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
    /// A change that arrives while the session waits is acted on where
    /// the session is, and the wait goes on to what is typed next.
    #[test]
    fn a_change_is_acted_on_and_the_wait_goes_on() {
        let (send, inputs) = std::sync::mpsc::channel();
        send.send(super::Input::Changed).unwrap();
        send.send(super::Input::Changed).unwrap();
        send.send(super::Input::Submit("next".into())).unwrap();
        let seen = std::cell::Cell::new(0);
        let next = super::next_input(&inputs, &|| seen.set(seen.get() + 1));
        assert_eq!(next, Ok(Some("next".into())));
        assert_eq!(seen.get(), 2);
    }
    /// While the turn waits on the person the card says so and the clock
    /// stands still; once they answer, the turn is what it was and the
    /// clock runs again without the wait in it.
    #[test]
    fn the_clock_stands_still_while_the_turn_waits_on_you() {
        use super::{Activity, Clock};
        let mut clock = Clock::default();
        clock.start();
        let mut activity = Activity::Executing;
        assert!(clock.hold(true, &mut activity));
        assert_eq!(activity, Activity::AwaitingYou);
        std::thread::sleep(std::time::Duration::from_millis(120));
        assert!(
            clock.elapsed_ms().unwrap() < 100,
            "the wait on the person was counted as work"
        );
        assert!(clock.hold(false, &mut activity));
        assert_eq!(activity, Activity::Executing);
        assert!(clock.elapsed_ms().unwrap() < 100);
        assert!(!clock.hold(false, &mut activity), "nothing changed");
    }

    #[test]
    fn a_stop_is_read_once_so_it_ends_the_turn_it_was_asked_during() {
        let steer = super::Steer::default();
        assert!(steer.take_stop().is_none(), "nothing was asked for");
        steer.request_stop(crate::tui::Stopper::You);
        steer.request_stop(crate::tui::Stopper::You);
        assert_eq!(
            steer.take_stop(),
            Some(crate::tui::Stopper::You),
            "the stop never reached the task loop"
        );
        assert!(
            steer.take_stop().is_none(),
            "the stop survived its own turn and would end the next one"
        );
    }

    #[test]
    fn the_two_escapes_are_separate_levers() {
        let steer = super::Steer::default();
        steer.request_stop(crate::tui::Stopper::You);
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
}
