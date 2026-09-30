//! `sterna session`: the run that wires the six merged 61C modules together.
//! Each of them is correct and tested in isolation; this module is the only
//! place any of them is called from `main` rather than from its own tests --
//! see the packet's OBJECTIVE for why that gap, not missing code, is what
//! this module exists to close.

pub mod output;
mod ui;

use std::cell::{Cell, Ref, RefCell};
use std::io::{self, IsTerminal};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant, SystemTime};

use clap::Parser;
use ratatui::Terminal;
use ratatui::backend::TestBackend;

use crate::bg;
use crate::commands::{self, CommandSource, CommandStatus};
use crate::config::SternaConfig;
use crate::contract::{Block, Conversation, Message, ProjectConfig, Role, ServedBy, SessionId};
use crate::events::batch::Batch;
use crate::events::window::{Window, WindowConfig};
use crate::gateway::{self, Gateway};
use crate::memory::LocalMemory;
use crate::project;
use crate::prompt::{self, Budget, CellResult, ErrorSection, Extracted};
use crate::rollout::{self, Rollout};
use crate::runtime::isolate::{DEFAULT_HEAP_LIMIT_BYTES, Runtime};
use crate::runtime::outcome::{CellOutcome, CellRecord, Ended};
use crate::runtime::preview;
use crate::sandbox::modes::{self, RequestMode};
use crate::sandbox::profile::Profile;
use crate::session::context::{
    context_cap, estimate_context, record_request, return_budget, send_task_turn_recovering,
    sweep_if_due,
};
use crate::telemetry::RequestMeasurement;
use crate::tools::invoke::{self, Args, ToolContext, ToolError};
use crate::tools::registry;
use crate::tui::{self, CellError, CellView, ContextTokens, Counted, Notebook, TaskTokens};
use crate::wire;

const REQUEST_MEASUREMENT_CAP: usize = 64;

macro_rules! session_println {
    ($($arg:tt)*) => { ui::output(format!($($arg)*)) };
}

mod args;
mod ask;
mod cell_view;
mod context;
mod controls;
mod ending;
use ending::delivered_the_interrupt;
mod interrupt;
use interrupt::{DOUBLE_INTERRUPT_WINDOW, INTERRUPT, install_interrupt_handler, watch};
mod native;
mod notices;
mod resume;
mod returned;
mod setup;
mod splash;
mod startup;
mod system;
use system::{estimate_request_tokens, estimate_task_request_tokens};
mod task;
mod test_check;
mod usage;

pub use system::{MANIFEST_PROBE, session_facts, session_facts_with, system_manifest};
use system::{apply_decision_hold, build_system_prompt, task_decision};
use task::{Observed, TaskSpend, TaskState, partial_effects};

/// The longest a turn waits for an open event window to close before it is
/// composed without one — `events-contract.md` §2's own 2,000 ms deadline
/// plus room for the drain that follows it.
///
/// It is a ceiling, not a delay: an **empty** window can never close and is
/// answered at once, which is every turn of every session that raised no
/// event. Only a window already holding an event is waited on, and only until
/// §2's deadline closes it.
const EVENT_WAIT: Duration = Duration::from_millis(2_500);

/// How often that wait looks again. Short enough that a window closes within
/// a frame of its deadline, long enough that waiting costs nothing.
const EVENT_POLL: Duration = Duration::from_millis(25);

/// Ordinary prose cannot silently finish unfinished action work.
const NO_PROGRAM: &str = prompt::CONTINUE_WORK;
const TWO_BLOCKS: &str = "Mixed or multiple sterna-edit blocks are ambiguous; send one repair or ordinary Sterna code. Nothing ran.";

/// The class a cancelled call throws with (`bindings.rs`'s `Cancelled`), read
/// off the cell's own trajectory so the session knows a Ctrl-C was delivered.
const CANCELLED: &str = "Cancelled";

/// The keyboard's end of the cancellation facility.
///
/// **A Ctrl-C cancels the call in flight; it never terminates the isolate.**
/// JavaScript is stopped by the wall-clock watchdog (`runtime-contract.md`
/// §7's limits) and by nothing else here: a second terminator racing the
/// watchdog's own is how a runtime that could be stopped becomes one that
/// cannot. So an interrupt raised while a cell is only computing is not lost
/// and not applied either -- [`pending`](Self::pending) stays raised until a
/// call actually ends `Cancelled`, which makes the *next* call the one it
/// cancels.
struct Interrupter {
    /// The session whose background board [`end_the_session`](Self::end_the_session)
    /// has to take with it -- a value `run` already holds when it builds this,
    /// rather than a boxed shutdown callback, whose only effect would be to
    /// hide one call to a module this file already imports.
    session: SessionId,
    /// The token of the cell now running, replaced before every cell so one
    /// Ctrl-C cannot cancel every later call.
    token: Mutex<invoke::CancellationToken>,
    /// Raised when an interrupt has been seen and not yet consumed by a call
    /// that ended `Cancelled`.
    pending: AtomicBool,
    /// Raised once the second Ctrl-C has decided to exit, and never lowered.
    /// It pins [`pending`](Self::pending) raised, so every call started
    /// during the reap grace is cancelled before it spawns a child.
    ending: AtomicBool,
    /// Raised when the last interrupt came from Ctrl-C rather than from
    /// the person's second Escape, so a stopped turn says which it was.
    by_signal: AtomicBool,
    /// Held for the length of every rollout write. The second-Ctrl-C exit
    /// takes it too, which is the whole of "the rollout's current line is
    /// complete": `Rollout` writes one whole line per call, so waiting for
    /// this lock is waiting for that call to return.
    writing: Mutex<()>,
}

/// A lock that a panic elsewhere cannot turn into a second failure: the data
/// behind both mutexes is a token and a unit, neither of which a panic can
/// leave inconsistent.
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

impl Interrupter {
    fn new(session: SessionId) -> Self {
        Self {
            session,
            token: Mutex::new(invoke::CancellationToken::new()),
            pending: AtomicBool::new(false),
            ending: AtomicBool::new(false),
            by_signal: AtomicBool::new(false),
            writing: Mutex::new(()),
        }
    }

    /// Publishes the token the cell about to run will make its calls through,
    /// cancelling it on the spot if an interrupt is still pending.
    ///
    /// The check and the store happen under one lock, so there is no window
    /// in which [`raise`](Self::raise) cancels the token being replaced and
    /// the replacement escapes uncancelled.
    fn arm(&self, token: invoke::CancellationToken) {
        let mut slot = lock(&self.token);
        if self.pending.load(Ordering::SeqCst) {
            token.cancel();
        }
        *slot = token;
    }

    /// One interrupt: cancel the call in flight and stay raised.
    fn raise(&self) {
        let slot = lock(&self.token);
        self.pending.store(true, Ordering::SeqCst);
        slot.cancel();
    }

    /// Who raised the interrupt a cancelled turn ended on.
    fn raised_by(&self) -> tui::Stopper {
        if self.by_signal.load(Ordering::SeqCst) {
            tui::Stopper::Interrupt
        } else {
            tui::Stopper::You
        }
    }

    /// A call ended `Cancelled`, so the interrupt that asked for it has been
    /// delivered and later cells start clean -- **unless the session is
    /// already ending**, in which case there are no later cells and lowering
    /// the flag would let one start a child the exit then orphans.
    fn consumed(&self) {
        if !self.ending.load(Ordering::SeqCst) {
            self.pending.store(false, Ordering::SeqCst);
        }
    }

    /// A new task starts with nothing pending -- unless the session is
    /// ending, for the same reason [`consumed`](Self::consumed) keeps it.
    fn start_clean(&self) {
        INTERRUPT.store(0, Ordering::SeqCst);
        self.consumed();
    }

    fn writing(&self) -> MutexGuard<'_, ()> {
        lock(&self.writing)
    }
}

/// The two rollout writes in this module, and every one of them goes through
/// one of these -- which is what makes [`Interrupter::end_the_session`]'s
/// "never a half line" a property of the code rather than of the timing.
fn write_turn(
    interrupt: &Interrupter,
    rollout: &mut Rollout,
    role: Role,
    text: &str,
) -> io::Result<()> {
    let _line = interrupt.writing();
    rollout.record_turn(role, text)?;
    output::message(&Message::text(role, text));
    Ok(())
}

fn write_message(
    interrupt: &Interrupter,
    rollout: &mut Rollout,
    message: &Message,
) -> io::Result<()> {
    let _line = interrupt.writing();
    rollout.record_message(message)?;
    output::message(message);
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn write_cell(
    interrupt: &Interrupter,
    rollout: &mut Rollout,
    record: &CellRecord,
    origin: crate::abi::Origin,
    error: Option<(&str, &str)>,
    observation: crate::runtime::observation::ObservationStats,
    reduction: crate::runtime::observation::ReductionStats,
    single_intent: bool,
) -> io::Result<()> {
    let _line = interrupt.writing();
    rollout.record_cell(record)?;
    output::cell_frame(record, origin, error, observation, reduction, single_intent);
    Ok(())
}

pub use args::SessionArgs;

/// Parses `args` (everything after `sterna session`) and runs it.
pub fn dispatch(args: &[String]) -> Result<(), String> {
    let parsed = SessionArgs::try_parse_from(
        std::iter::once("sterna session".to_string()).chain(args.iter().cloned()),
    )
    .map_err(|e| e.to_string())?;
    let machine_output = output::Output::start(parsed.output_format);
    if parsed.output_format != output::Format::Text && (parsed.task.is_none() || parsed.sessions) {
        let result = Err("--output-format json/stream-json requires --task (or sterna exec) and cannot be combined with --sessions".into());
        machine_output.finish(&result)?;
        return result;
    }
    let mut moved = crate::relocate::at_start(Some(&parsed.root));
    if parsed.sessions {
        moved.iter().for_each(|line| eprintln!("{line}"));
        return resume::print_listing(&parsed.root);
    }
    let result = resume::each(parsed, |args| run(args, &mut moved));
    // A start that ended before its notes were said -- a cancelled picker, a
    // refused `--resume`, an image that would not load -- still says what it
    // moved: the next start finds nothing left to move and never would.
    moved.drain(..).for_each(|line| eprintln!("{line}"));
    machine_output.finish(&result)?;
    result
}

fn message_text(message: &Message) -> String {
    message
        .content
        .iter()
        .filter_map(|block| match block {
            Block::Text(text) => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("")
}

/// The conversation and, beside it, everything the notebook column knows
/// about it that the messages themselves do not say: which cell threw, which
/// returned, what its handle table looked like as it ended.
///
/// **They travel together because they are indexed together.** A cell's view
/// is found by the same ordinal the screen numbers the cell with, so a
/// conversation that grew without its notebook -- a resumed session, whose
/// cells came back from the rollout file -- would hang every later view under
/// the wrong cell.
#[derive(Clone)]
struct Transcript {
    conversation: Conversation,
    notebook: Notebook,
    /// Provider-only reset point after context overflow. The visible and
    /// persisted conversation remains complete.
    provider_checkpoint: Option<String>,
    provider_start: usize,
}

/// Pipe output is static; the interactive terminal is owned by `ui::LiveUi`.
fn render(
    transcript: &Transcript,
    served_by: &ServedBy,
    session: &Session<'_>,
    activity: tui::Activity,
) {
    if let Some(ui) = session.ui {
        ui.publish(transcript, served_by, activity);
    } else {
        startup::render_as_lines(transcript, served_by);
    }
}

/// Runs `session`, in the order the packet's OBJECTIVE fixes: load the
/// project, resume or start the rollout, `SessionStart`, then one input (or
/// stdin's, one per line) at a time until the input source is exhausted.
fn run(mut args: SessionArgs, moved: &mut Vec<String>) -> Result<Option<String>, String> {
    resume::choose(&mut args);
    if args.images.len() > 4 {
        return Err("at most four image attachments are accepted per task".into());
    }
    let images = args
        .images
        .iter()
        .map(|path| crate::images::load(&args.root, path))
        .collect::<Result<Vec<_>, _>>()?;
    // First, so a `--resume <id>` refusal is about which session to open
    // rather than arriving under two lines of startup notes. Said at the end
    // too: a crash or a closed terminal pane never reaches `/exit`.
    let (session_id, rollout_path) = resume::resolve_session(&args)?;
    output::session(session_id.as_str());
    wire::set_cache_key(session_id.as_str());
    let terminal = args.task.is_none() && io::stdin().is_terminal() && io::stdout().is_terminal();
    // Notes said before the terminal UI exists open its conversation rather
    // than flashing on the screen the UI replaces.
    let _startup_notes = terminal.then(ui::StartupNotes::hold);
    moved.drain(..).for_each(|line| session_println!("{line}"));
    let mut project = project::load(&args.root);
    let settings_store = crate::settings::Store::new(&args.root)?;
    let mut loaded_settings = settings_store.load(args.profile.as_deref())?;
    for notice in &loaded_settings.notices {
        session_println!("settings: {notice}");
    }
    let retired = settings_store.remove_retired(&loaded_settings);
    for notice in &retired {
        session_println!("settings: {notice}");
    }
    // A retired word may have been migrated into the file just now
    // (`permissions.full_access` into `sandbox.level`): this session runs on
    // what it announced, not on what the file said a moment ago.
    if !retired.is_empty() {
        loaded_settings = settings_store.load(args.profile.as_deref())?;
    }
    if project.settings.is_some() {
        session_println!(
            "settings: Claude permissions are not used implicitly. /config import claude previews an explicit import; the original file is never changed."
        );
    }
    project.settings = settings_store.permissions()?;
    // Shared and mutable because `/model <tier> <id>` changes it mid-session:
    // the next cell's runtime must be built from the choice just made, not
    // from what the file said at startup.
    let config = RefCell::new(loaded_settings.config);
    let initial_effort = crate::settings_session::value(&loaded_settings.values, "session.effort")
        .and_then(toml::Value::as_str)
        .and_then(wire::Effort::parse)
        .unwrap_or_default();

    // After the settings load, because `sandbox.level` is one of the two
    // places that set it.
    let level = startup::level(&args, &loaded_settings.values)?;
    // An explicit `--model` wins, then the model this project was last left
    // on. There is no compiled-in request-model fallback: starting without a
    // concrete choice would make Sterna silently spend against a model the
    // person did not select, so a terminal opens the picker instead.
    let requested = startup::requested_model(args.model.as_deref(), &config.borrow(), terminal)?;
    let gateway = gateway::select(args.gateway.as_deref());
    let accounts = startup::served_accounts(&gateway);
    // Jev is the default decision model wherever the gateway can route it:
    // an unset `[decisions] model` with a TypeSafe account served means
    // `jev-latest`, so every decision question is asked rather than inert.
    // Only for a gateway Sterna starts itself, whose listing is the binary and
    // configuration that will serve it; a hosted session's listing describes
    // this machine's gateway, not the one it was handed. An explicit model,
    // or `mode = "off"`, is left exactly as written.
    let default_decisions = matches!(gateway, gateway::Gateway::Command { .. })
        .then(|| startup::default_decisions_model(&config.borrow().decisions, &accounts))
        .flatten();
    if let Some(model) = default_decisions {
        config.borrow_mut().decisions.model = Some(model.to_string());
    }
    if terminal {
        notices::at_start(&gateway);
    }
    if config.borrow().decisions.model.is_none() && !terminal {
        session_println!("decisions: off (no model)");
    } else if let Some(model) = default_decisions.filter(|_| !terminal) {
        session_println!("decisions: {model} (the gateway serves a TypeSafe account)");
    }
    // A terminal draws the level live on the top bar instead, so the line
    // cannot go stale the moment the level changes.
    if !terminal {
        session_println!("{}", startup::sandbox_line(&level));
    }

    // `sandbox-grants.md` §1.5: computed once, at session start, immutable
    // for the session's life. Reloading a persisted configuration must never
    // let a program widen its own sandbox during the running session.
    let mut profile = compile_profile_once(&project);
    for directory in &args.additional_dirs {
        profile = profile.with_additional_root(directory)?;
    }
    if level.level() == crate::permissions::Level::Full {
        // Said plainly, and said with what still holds: a line that only
        // shouts teaches a person to stop reading it.
        session_println!(
            "sandbox: full access — Sterna applies no OS confinement to the children it spawns; this machine is the boundary. The deny patterns and the never-grantable set are unchanged."
        );
    }

    let roster = startup::subagent_roster(&gateway, &accounts);
    let started_on = requested.map(|model| startup::settle_model(model, &accounts));
    // Held for the whole session: dropping it kills the gateway sterna started.
    // `None` means sterna attached to one already serving, or runs direct.
    // Before the interrupt thread and the live UI on purpose -- it writes the
    // process environment, which is only sound while single-threaded. A slow
    // start flies the tern until the live UI takes the screen (`splash.rs`).
    let (_serving, splash_frame) = splash::start_gateway(
        terminal.then_some(&loaded_settings.values),
        &gateway,
        args.gateway.is_some(),
        &rollout_path.with_extension("gateway.log"),
    )?;
    // After the gateway, which writes the process environment and so must
    // start while this process is still single-threaded; the proxy is the
    // first thread of its own.
    let network = startup::network(&args, &loaded_settings.values);
    profile = startup::route_through(profile, network.proxy.as_deref());
    // Collected once the profile is final: the manifest reports the grants
    // in force, and the bypass and the proxy applied above change what it
    // says.
    let manifest = system_manifest(&profile, &config.borrow());
    if crate::verification::load(&profile).is_ok_and(|checks| !checks.checker.is_empty()) {
        session_println!("{}", crate::verification::CHECKER_GONE);
    }

    let resuming = rollout_path.exists();
    let (conversation, provider_checkpoint, provider_start) = if resuming {
        let conversation = rollout::resume(&rollout_path)
            .map_err(|e| format!("could not resume {}: {e}", rollout_path.display()))?;
        let checkpoint = rollout::resume_checkpoint(&rollout_path).map_err(|e| {
            format!(
                "could not resume checkpoint {}: {e}",
                rollout_path.display()
            )
        })?;
        let (provider_checkpoint, provider_start) = checkpoint
            .map(|(text, start)| (Some(text), start))
            .unwrap_or((None, 0));
        (conversation, provider_checkpoint, provider_start)
    } else {
        (
            Conversation {
                system: build_system_prompt(
                    &config.borrow().limits,
                    &config.borrow().web,
                    &config.borrow().agents,
                    &config.borrow().decisions,
                    &roster,
                    &profile,
                    args.interface.unwrap_or_default(),
                    &manifest,
                ),
                messages: Vec::new(),
            },
            None,
            0,
        )
    };

    let mut rollout = Rollout::create(&rollout_path, session_id.clone(), &conversation.system)
        .map_err(|e| format!("could not open {}: {e}", rollout_path.display()))?;

    // The live stream sits beside the rollout and shares its stem, so a tool
    // that found one has found the other. On by default: an observability
    // surface nobody knows to ask for is one nobody uses, and the cost is one
    // short append per transition against a rollout that is far larger.
    let observe = crate::observe::Observer::beside(&rollout_path, session_id.as_str());

    // A resumed conversation's cells are not replayed (`runtime-contract.md`
    // §4), so the notebook starts empty and pads: an earlier cell renders
    // with no view of its own rather than with the next task's.
    let mut notebook = Notebook::default();
    if resuming {
        for (ordinal, view) in rollout::resume_views(&rollout_path)
            .map_err(|e| format!("could not resume views {}: {e}", rollout_path.display()))?
        {
            notebook.set(ordinal, view);
        }
    }
    let mut transcript = Transcript {
        conversation,
        notebook,
        provider_checkpoint,
        provider_start,
    };

    observe.session_begin(&rollout_path, &args.root);

    // The notes live beside the rollout, so a project's notes travel with
    // the session that made them.
    let memory = LocalMemory::new(
        rollout_path
            .parent()
            .map(std::path::Path::to_path_buf)
            .unwrap_or_else(|| args.root.clone()),
    );

    // Installed before the first task and never again: from here on a Ctrl-C
    // cancels the call in flight rather than killing the process mid-line.
    let interrupt = Arc::new(Interrupter::new(session_id.clone()));
    install_interrupt_handler();

    drop(splash_frame);
    let interactive =
        if args.task.is_none() && io::stdin().is_terminal() && io::stdout().is_terminal() {
            Some(ui::LiveUi::start(
                {
                    let mut state = tui::ScreenState {
                        model: started_on.clone(),
                        level: level.clone(),
                        effort: initial_effort,
                        settings_root: Some(args.root.clone()),
                        settings_global: crate::project::workflows::user_directory(),
                        settings_profile: args.profile.clone(),
                        compact: true,
                        pretty: true,
                        project: Some(startup::project_name(&args.root)),
                        // The mechanism, for the surface that explains the
                        // boundary. It is no longer the status line's words:
                        // `3p/1c` is a path-rule count and a command-pattern
                        // count, and nothing on a status line could ever have
                        // said so.
                        sandbox: Some(format!(
                            "{} path rules · {} pre-approved commands",
                            profile.rule_count(),
                            profile.pre_approved().len(),
                        )),
                        subagents: Some(controls::tier_status(&config.borrow())),
                        // The third half, which until 2026-09-19 no surface
                        // carried: a rung and a grant are two choices, and
                        // whether Sterna confines what it spawns is the one
                        // that decided whether `cargo test` could link.
                        confinement: Some(
                            crate::tools::invoke::Confinement::for_session(&profile)
                                .map_or("no-applier", crate::tools::invoke::Confinement::short)
                                .to_string(),
                        ),
                        // The shell never has a network; the field names the
                        // host tools that do (map 2657, design §8).
                        network: Some(config.borrow().web.posture().into()),
                        ..tui::ScreenState::default()
                    };
                    crate::settings_session::presentation(&mut state, &loaded_settings.values);
                    state.settings_models = startup::served_models(&accounts);
                    state.local_hour = crate::workbench::voice::local_hour();
                    if let Some(root) = state.settings_root.clone() {
                        state.suggestions = crate::workbench::voice::project_suggestions(&root);
                    }
                    state
                },
                transcript.conversation.clone(),
                transcript.notebook.clone(),
            )?)
        } else {
            None
        };

    // **Spawned after the terminal exists, because it reads the terminal's
    // Escape lever as well as the signal handler's flag.** The handler is
    // installed above and its flag is an atomic, so a Ctrl-C struck in the
    // moment between the two is not lost -- it is read by the first poll.
    let watched = Arc::clone(&interrupt);
    let levers = interactive.as_ref().map(ui::LiveUi::steer_handle);
    std::thread::spawn(move || watch(&watched, levers));

    // Every session gets the gate: it decides per call whether the level
    // sends it to a person (`permissions::judge`), and refuses what nobody
    // is there to answer.
    let approval_gate = Some(startup::approval_gate(
        &level,
        &config.borrow(),
        &profile,
        interactive.as_ref(),
        &network,
    ));
    let ask_gate = interactive.as_ref().map(ui::LiveUi::ask_gate);
    let session = Session {
        selected_profile: args.profile.clone(),
        pending_images: RefCell::new(images),
        observe: observe.clone(),
        approval_gate,
        ask_gate,
        level: Some(level.clone()),
        window: RefCell::new(Window::new(WindowConfig::default())),
        roster,
        project: &project,
        config: &config,
        profile: &profile,
        gateway: &gateway,
        id: &session_id,
        memory: &memory,
        interrupt: &interrupt,
        ui: interactive.as_ref(),
        model: RefCell::new(started_on.clone().unwrap_or_default()),
        context_window: started_on.clone().zip(args.context_window_tokens),
        mode: Cell::new(RequestMode::Work),
        effort: Cell::new(initial_effort),
        routing: Default::default(),
        interface: Cell::new(args.interface.unwrap_or_default()),
        manifest,
        rollbacks: RefCell::new(Vec::new()),
        rollback_pending: Cell::new(None),
        plan: RefCell::new(None),
        requests: std::cell::Cell::new(0),
        settings_global: crate::project::workflows::user_directory(),
    };
    output::interface(session.interface.get(), session.dialect());
    controls::announce_missing_credential(&session, _serving.is_some());
    setup::at_start(&session, started_on.is_none());
    resume::offer(&args, &session, &session_id);
    let outcome = drive(&args, &session, &mut transcript, &mut rollout)
        .map_err(|message| startup::explain_failure(&message, &session));
    startup::refused_hosts(network.proxy.as_deref(), interactive.is_some());
    // §5 again, and this one is the promise `session::run` itself makes: an
    // input that failed mid-task left `run_task` by `?` without reaching its
    // own shutdown, and a job of that task must not outlive the session
    // either.
    bg::shutdown(&session_id);
    interrupt::leave_the_exit_to_the_signal(&session.interrupt.ending);
    let next = resume::at_end();

    outcome.map(|()| next)
}

/// Everything one session holds for its whole life, gathered so a per-input
/// function takes the session rather than five of its parts.
///
/// **`profile` is a borrow, and that is `sandbox-grants.md` §1.5.** The one
/// `Profile` `run` compiled is the only one any input can be answered
/// against; there is no owned field here that a later call could replace.
struct Session<'a> {
    selected_profile: Option<String>,
    pending_images: RefCell<Vec<Block>>,
    approval_gate: Option<crate::approval::Gate>,
    /// Where a question a cell asked goes. `None` whenever nobody is at the
    /// keyboard, which is what makes `ask` throw rather than wait.
    ask_gate: Option<crate::ask::Gate>,
    /// The live sandbox level. `None` only for the constructed sessions in
    /// tests that never ask anybody anything.
    level: Option<crate::permissions::LiveLevel>,
    window: RefCell<Window>,
    ui: Option<&'a ui::LiveUi>,
    model: RefCell<String>,
    /// Capacity explicitly associated with the startup model. A `/model`
    /// switch cannot silently reuse it for a different model.
    context_window: Option<(String, u64)>,
    /// What the request now running may do: `Plan` for the one request
    /// `/plan` started, `Work` otherwise.
    mode: Cell<RequestMode>,
    effort: Cell<wire::Effort>,
    /// This task's sticky routing to the provider's cache (cleared per task).
    routing: std::sync::Arc<wire::TurnRouting>,
    /// The entry points this session shows the parent model.
    interface: Cell<crate::abi::Interface>,
    /// The environment manifest collected once at session start from the
    /// compiled profile; rendered into the system block.
    manifest: crate::manifest::Manifest,
    /// The subagent roster, resolved once beside the manifest (`startup`).
    roster: Vec<crate::models::RosterModel>,
    project: &'a ProjectConfig,
    config: &'a RefCell<SternaConfig>,
    /// The keyboard's end of the cancellation facility: the SIGINT handler's
    /// flag, the token of the cell now running, and the lock every rollout
    /// write is taken under. A task publishes each cell's fresh token to it
    /// and asks it to forget the interrupt a cancelled call has delivered.
    interrupt: &'a Interrupter,
    profile: &'a Profile,
    /// The live event stream, for a program watching this session while it
    /// works (`observe.rs`). `Observer::none()` is a session nobody is
    /// watching and every emit on it is a no-op.
    observe: crate::observe::Observer,
    /// Entitlements, subscriptions and routing cost -- the controls that
    /// moved off Glasshouse and onto the standalone gateway.
    gateway: &'a Gateway,
    id: &'a SessionId,
    memory: &'a LocalMemory,
    /// Exact before/after snapshots for cells that changed project files.
    rollbacks: RefCell<Vec<RollbackCheckpoint>>,
    /// Stack length previewed by the last bare `/rollback`.
    rollback_pending: Cell<Option<usize>>,
    /// The plan the last `plan` request wrote, handed to the next request
    /// that is not `plan` and forgotten there.
    plan: RefCell<Option<String>>,
    /// Requests this session has started, for the decision model's context.
    requests: std::cell::Cell<u32>,
    /// The person's own settings folder, where a model, a setup pick or Jev
    /// is saved (decision 6). `None` -- a machine with no home, a test that
    /// names none -- saves to the project instead.
    settings_global: Option<std::path::PathBuf>,
}

impl Session<'_> {
    /// The project's configuration as it stands now.
    ///
    /// A borrow rather than a field, because a tier assignment replaces it
    /// mid-session; hold the `Ref` no longer than one statement.
    fn config(&self) -> Ref<'_, SternaConfig> {
        self.config.borrow()
    }

    /// The entry points this session declares to the parent model.
    ///
    /// The dialect follows the **active** request model, so a `/model` switch
    /// to another family changes which spellings are shown without changing a
    /// capability, an execution path or a lifetime
    /// (`tool-abi.md` §5, `helpers-and-subagents.md` §17).
    fn surface(&self) -> wire::Surface {
        wire::Surface::Acting {
            interface: self.interface.get(),
            dialect: self.dialect(),
        }
    }

    fn dialect(&self) -> crate::abi::Dialect {
        crate::abi::Dialect::for_model(&self.model.borrow())
    }
}

struct RollbackCheckpoint {
    before: crate::changes::Snapshot,
    after: crate::changes::Snapshot,
    /// The cell whose change this is, marked when it is rolled back.
    cell: usize,
}

/// Handles scripted, live-composer, and piped input through the same task
/// and command dispatch.
fn drive(
    args: &SessionArgs,
    session: &Session<'_>,
    transcript: &mut Transcript,
    rollout: &mut Rollout,
) -> Result<(), String> {
    if let Some(task) = &args.task {
        return process_input(task, session, transcript, rollout).result();
    }

    if let Some(ui) = session.ui {
        while let Some(input) = ui.next(&|| setup::offer(session))? {
            // A turn has already said how it ended; a control reports no
            // turn at all, so the turn's clock and its ending stay as they
            // were.
            let ran = process_input(&input, session, transcript, rollout);
            if let Err(message) = ran.outcome() {
                session_println!("ERROR: {}", startup::explain_failure(message, session));
            }
            // A control chosen as the turn ended, after its last request.
            let late = ui.steer().take_controls();
            for command in &late {
                answer_control(command, session, transcript);
            }
            if matches!(ran, Ran::Control(_)) || !late.is_empty() {
                ui.control_done(transcript);
            }
        }
        return Ok(());
    }

    while let Some(line) = ui::read_line().map_err(|e| e.to_string())? {
        // **A failed input ends that input, not the session.** A REPL that
        // exits on the first upstream error loses the whole conversation to
        // one 400 or one dropped connection, which is what a person watching
        // reads as "it crashed"; `--task` above still propagates, because a
        // scripted one-shot has nobody to report to but its exit code.
        // Observed 2026-09-06: one empty message made a gateway answer 400
        // and the session ended mid-task.
        if let Err(message) = process_input(&line, session, transcript, rollout).outcome() {
            session_println!("{}", startup::explain_failure(message, session));
        }
    }
    Ok(())
}

/// What one input was: a model turn, or a control answered locally.
enum Ran {
    Turn(Result<(), String>),
    Control(Result<(), String>),
}

impl Ran {
    fn outcome(&self) -> &Result<(), String> {
        match self {
            Self::Turn(result) | Self::Control(result) => result,
        }
    }
    fn result(self) -> Result<(), String> {
        match self {
            Self::Turn(result) | Self::Control(result) => result,
        }
    }
}

/// One input: a slash command answered locally, or a **task** run to its end.
/// A slash command -- resolved or not -- never reaches [`wire::send_turn`];
/// only text that is not a slash command does.
///
/// **A slash command is answered between tasks, never inside one.** The one
/// [`Runtime`] a task owns lives inside [`run_task`] and is dropped when the
/// task ends, so there is no code path on which a command could reach it.
fn process_input(
    input: &str,
    session: &Session<'_>,
    transcript: &mut Transcript,
    rollout: &mut Rollout,
) -> Ran {
    // **Blank input is not a turn.** A message with no content is not a
    // message: the Anthropic shape requires content, tool calls or reasoning
    // blocks, and a gateway that enforces it answers 400 and the task dies.
    // A bare Enter is the commonest keystroke in a REPL, so this guard is
    // what stops it ending the session. Observed 2026-09-06 at `messages.0`
    // and again at `messages.13`.
    if input.trim().is_empty() {
        return Ran::Control(Ok(()));
    }
    if let Some(rest) = input.strip_prefix('/') {
        let (name, argument) = split_command(rest);
        if !is_session_control(name)
            && let Some(resolved) = commands::resolve(session.project, name)
            && resolved.source == CommandSource::ProjectSkill
            && resolved.status == CommandStatus::Available
        {
            return match crate::project::workflows::skill_task(
                session.project,
                session.profile,
                name,
                argument.unwrap_or(""),
            ) {
                Ok(task) => Ran::Turn(run_task(&task, session, transcript, rollout)),
                Err(error) => Ran::Control(Err(error)),
            };
        }
        if !is_session_control(name)
            && let Some(resolved) = commands::resolve(session.project, name)
            && resolved.source == CommandSource::ProjectCommand
            && resolved.status == CommandStatus::Available
            && let Some(body) = session.project.commands.get(name)
        {
            let task = project_command_task(name, body, argument);
            return Ran::Turn(run_task(&task, session, transcript, rollout));
        }
        // `/plan <task>`: one request that reads and writes only its plan.
        // The next request works as usual and is handed the plan.
        if name == "plan" {
            let Some(task) = argument.filter(|task| !task.trim().is_empty()) else {
                session_println!(
                    "Use /plan <what to plan>: that one request reads, writes only {}, and hands the plan to your next message.",
                    modes::PLAN_FILE
                );
                return Ran::Control(Ok(()));
            };
            session.mode.set(RequestMode::Plan);
            return Ran::Turn(run_task(task, session, transcript, rollout));
        }
        answer_command(rest, name, argument, session, transcript);
        // The screen hears of a control through `control_done`, which keeps
        // the turn's ending; only the line-mode transcript is drawn here.
        if session.ui.is_none() {
            startup::render_as_lines(transcript, &ServedBy::default());
        }
        return Ran::Control(Ok(()));
    }
    Ran::Turn(run_task(input, session, transcript, rollout))
}

/// One control the screen passed on while a turn ran: `/model`, `/sandbox`
/// or `/effort` with its argument.
fn answer_control(command: &str, session: &Session<'_>, transcript: &mut Transcript) {
    let rest = command.trim().trim_start_matches('/');
    let (name, argument) = split_command(rest);
    answer_command(rest, name, argument, session, transcript);
}

fn is_session_control(name: &str) -> bool {
    matches!(
        name,
        "" | "help"
            | "tool"
            | "model"
            | "models"
            | "effort"
            | "plan"
            | "sandbox"
            | "context"
            | "status"
            | "config"
            | "permissions"
            | "login"
            | "pool"
            | "key"
            | "rollback"
            | "memory"
    )
}

fn project_command_task(name: &str, body: &str, argument: Option<&str>) -> String {
    let mut task = format!("Project command /{name}:\n\n{body}");
    if let Some(argument) = argument.filter(|value| !value.is_empty()) {
        task.push_str("\n\nArguments supplied by the user:\n");
        task.push_str(argument);
    }
    task
}

/// What one assistant message asked the session to do.
struct Step {
    /// The next user message, or `None` when the task is over: a top-level
    /// `return` is answered with nothing at all, because nothing further is
    /// asked of the model (`runtime-contract.md` §1).
    answer: Option<String>,
    /// Same feedback without its replaceable state snapshots.
    historical: Option<String>,
    native_result: Option<Message>,
    /// The task's terminal response (`runtime-contract.md` §9.2): rendered
    /// and kept as the assistant's own turn, with no request after it.
    response: Option<String>,
    /// Whether the message carried no program (§5's prose).
    ///
    /// **Nothing counts these any more.** Three prose turns in a row used to
    /// end the task, and a model reasoning its way toward a hard decision in
    /// prose is indistinguishable, to a counter, from a model stuck; the
    /// stall guard judges that on what the cells produced.
    prose: bool,
    /// The cell this turn ran -- `None` for prose and for two blocks,
    /// neither of which ran a cell at all.
    record: Option<CellRecord>,
    rollback: Option<(crate::changes::Snapshot, crate::changes::Snapshot)>,
    view: CellView,
}

/// Runs one task to its end: every turn's program goes to this task's own
/// isolate and every outcome comes back as the next user message, with no
/// person in the loop, until a terminal return, the cell cap or the task
/// cell limit ends it. The model is directed to return final answers as strings.
///
/// **One [`Runtime`] per task, built from the session's one compiled
/// [`Profile`].** `sandbox-grants.md` §1.5 is that the profile is computed
/// once at session start; this borrows it and compiles nothing, so a second
/// task cannot widen the first's grants and a program cannot widen its own.
fn run_task(
    task: &str,
    session: &Session<'_>,
    transcript: &mut Transcript,
    rollout: &mut Rollout,
) -> Result<(), String> {
    if session.model.borrow().is_empty() {
        let refused = "No model selected yet. Pick one with /model.";
        if let Some(ui) = session.ui {
            ui.publish(transcript, &ServedBy::default(), tui::Activity::Failed);
        }
        return Err(refused.into());
    }
    transcript.notebook.handlers.clear();
    // Moves made while the last task ran reach the file here, at the boundary:
    // the UI thread that made them writes nothing itself.
    rollout.record_moves(session.level.as_ref());
    let (mode, started) = (session.mode.get(), std::time::SystemTime::now());
    session.observe.task_begin(
        task,
        mode.name(),
        session
            .level
            .as_ref()
            .map_or("unset", |level| level.level().name()),
        &session.model.borrow(),
    );
    let ended = run_task_inner(task, session, transcript, rollout);
    session.observe.task_end(match &ended {
        Ok(None) => "answered",
        Ok(Some(_)) => "stopped",
        Err(reason) => reason.as_str(),
    });
    let activity = match &ended {
        Ok(None) => tui::Activity::Complete,
        Ok(Some(by)) => tui::Activity::Stopped(*by),
        Err(_) => tui::Activity::Failed,
    };
    let result = ended.map(|_| ());
    rollout.record_moves(session.level.as_ref());
    // A plan is one request: the next one works as usual.
    session.mode.set(RequestMode::Work);
    if mode == RequestMode::Plan
        && let Some(plan) = modes::written_plan(session.profile.root(), started)
    {
        session_println!(
            "plan written: {} ({} lines)",
            modes::PLAN_FILE,
            plan.lines().count()
        );
        session.plan.replace(Some(plan));
    }
    transcript.notebook.handlers.clear();
    if session.ui.is_some() {
        notices::at_task_end();
    }
    if let Some(ui) = session.ui {
        ui.publish(transcript, &ServedBy::default(), activity);
    }
    result
}

/// Runs the task; `Ok(Some(by))` when it was stopped before it finished.
fn run_task_inner(
    task: &str,
    session: &Session<'_>,
    transcript: &mut Transcript,
    rollout: &mut Rollout,
) -> Result<Option<tui::Stopper>, String> {
    // A stop or an interrupt raised before this task began belongs to no
    // task: an Escape whose turn ended before its cell boundary, a Ctrl-C at
    // an idle prompt. Left raised, it ended the next task on its first
    // check -- measured 56 ms after the person's message was sent.
    session.interrupt.start_clean();
    if let Some(ui) = session.ui {
        ui.steer().clear();
    }
    let mut budget = TaskSpend::new(session.config().limits.cells);
    session.routing.clear();
    system::keep_session_system(session, transcript);
    let task_context = system::task_lines(session);
    let has_history = !transcript.conversation.messages.is_empty();
    let (decision, decision_failures, pending_decision) = task_decision(task, session, has_history);
    let effort_lease = system::EffortLease::for_kind(session, decision.as_ref());
    {
        let _line = session.interrupt.writing();
        rollout
            .record_context(&transcript.conversation.system)
            .map_err(|e| format!("could not record task context: {e}"))?;
    }

    // A checkpoint describes one runtime's live handles. A later task has a
    // fresh runtime, but must not resend the oversized history that forced
    // the checkpoint. Replace only the provider summary with a truthful
    // empty-runtime boundary and retain the complete visible conversation.
    if transcript.provider_checkpoint.is_some() {
        transcript.provider_checkpoint = Some(format!(
            "Earlier conversation was omitted because it no longer fit. This is a new task with a fresh runtime: no prior handles are live and nothing from the earlier task should be continued.\n\n## The task\n{}",
            task.trim()
        ));
        transcript.provider_start = transcript.conversation.messages.len();
    }
    let mut user_message = Message::text(Role::User, task);
    system::carry_task_context(&mut user_message, task_context);
    user_message
        .content
        .extend(session.pending_images.borrow_mut().drain(..));
    write_message(session.interrupt, rollout, &user_message)
        .map_err(|e| format!("could not record the user turn: {e}"))?;
    transcript.conversation.messages.push(user_message);

    // This request's profile: a plan request narrows it, and Full access
    // lifts Sterna's own confinement. Built once per request from the live
    // level, so a level changed mid-task applies from the next request.
    let request_profile = request_profile(session);
    let mut runtime = Runtime::with_limits(
        &request_profile,
        session.id,
        DEFAULT_HEAP_LIMIT_BYTES,
        Duration::from_secs(session.config().limits.cell_wall_clock_s),
    )
    .with_response_byte_cap(session.config().limits.response_bytes)
    .with_instruction_context()
    .with_delivered_instructions(&held_text(&transcript.conversation))
    .with_config(session.config().clone())?;
    // The approval hint's own `asked`/`failed` counts are session-wide (the
    // gate outlives one task); this task's telemetry reports the delta from
    // where they stood before this task's cells ran.
    let approval_hint_baseline = session
        .approval_gate
        .as_ref()
        .map_or((0, 0), crate::approval::Gate::hint_counts);
    if let Some(gate) = &session.approval_gate {
        runtime = runtime.with_approval_gate(gate.clone().with_task(task.to_string()));
    }
    // Decided once per request, because all three of its inputs can change
    // between requests: the terminal, `[ask] enabled`, and the narrowing.
    runtime = runtime.with_ask(ask::refusal(session));
    // `events-contract.md` §2: one window is always open, from session start
    // or from the moment the previous batch was delivered. It is per task
    // because the isolate the batch is bound in is, and §5's jobs are
    // cancelled with it below.
    let mut window = session.window.borrow_mut();
    transcript.notebook.batches_delivered = 0;
    let mut final_turn = false;
    let mut terminal_failure = None;
    let mut incomplete;
    let mut stopped_by_request = None;
    // The deliberate sweep's two pieces of memory: where the conversation
    // stood when it was last swept, so a session that sits above the
    // fraction does not rewrite the provider's cached prefix every turn; and
    // whether the last cell threw, which is the deterministic reading of
    // whether this is a settled moment to take one.
    let mut swept_at_messages: Option<usize> = None;
    let mut last_cell_threw = false;
    let mut task_state = TaskState::new(task, &request_profile, &session.config())
        .with_session(session.id)
        .with_decision(decision, decision_failures)
        .with_kind_effects(&effort_lease);
    task_state.pending_decision = pending_decision;
    output::decisions(task_state.decisions_telemetry(&session.config().decisions));

    loop {
        // **The cell boundary, which is where a requested stop is honoured.**
        // Every iteration of this loop is one model turn, so reading the
        // lever here is exactly "the call in flight finished, and no further
        // turn is sent". Nothing is thrown away: the cells that ran are in
        // the notebook and the rollout, and the task ends the way a finished
        // one does rather than as a failure, because a person choosing to
        // stop is not an error.
        if let Some(by) = session.ui.and_then(|ui| ui.steer().take_stop()) {
            incomplete = false;
            stopped_by_request = Some(by);
            break;
        }
        // A model, mode or effort chosen while the turn runs applies from
        // its next request (decision 9), and this is where that begins.
        if let Some(ui) = session.ui {
            for command in ui.steer().take_controls() {
                answer_control(&command, session, transcript);
            }
        }
        let since = SystemTime::now();
        let requested_model = session.model.borrow().clone();
        let estimate =
            estimate_task_request_tokens(&transcript.conversation, &requested_model, task);
        let (cap, cap_source) = context_cap(session, &requested_model);
        estimate_context(&mut transcript.notebook, estimate, cap, cap_source);
        // One deliberate sweep near the top of a known window, never a
        // trickle and never mid-repair (the user, 2026-09-17).
        if let Some(notice) = sweep_if_due(
            &mut transcript.conversation,
            estimate,
            cap,
            cap_source,
            session.config().limits.compact_above_percent,
            last_cell_threw,
            &mut swept_at_messages,
        ) {
            session_println!("{notice}");
        }
        if let Some(ui) = session.ui {
            ui.publish(transcript, &ServedBy::default(), tui::Activity::Thinking);
        }
        let cause = task_state.next_cause();
        let (turn, elapsed_ms) =
            match send_task_turn_recovering(transcript, session, &runtime, task, rollout, cause) {
                Ok(sent) => sent,
                // The second Escape: the turn was given up on, which is the
                // person stopping the task, not the task failing.
                Err(error) if error.contains(wire::CANCELLED_TURN) => {
                    session.interrupt.consumed();
                    incomplete = false;
                    stopped_by_request = Some(session.interrupt.raised_by());
                    break;
                }
                Err(error) => {
                    task_state.salvage(&error);
                    return Err(error);
                }
            };
        task_state.settle_decision(session, false);
        let request_cell = tui::cell_ordinal(&transcript.conversation, &transcript.notebook) + 1;
        let served = gateway::served_by(session.gateway, since);
        record_request(
            &mut transcript.notebook,
            RequestMeasurement::from_response(
                request_cell,
                requested_model,
                elapsed_ms,
                // Project routing rows are not correlated to this request.
                ServedBy::default(),
                turn.usage.as_ref(),
            ),
        );
        let assistant_text = message_text(&turn.message);
        // **An empty reply is never appended.** A message with no content is
        // not a message, and appending one poisons the conversation for the
        // whole task: every later request replays it, and a gateway that
        // enforces the shape answers 400 to all of them, so one empty reply
        // becomes a task that can no longer make any request at all. Ending
        // here loses this turn; appending loses the session. Observed
        // 2026-09-06: `messages.13` empty, then 400 on every retry.
        if assistant_text.trim().is_empty()
            && !turn
                .message
                .content
                .iter()
                .any(|block| matches!(block, Block::ToolUse { .. }))
        {
            return Err(
                "the model returned an empty reply; the task ends here rather than repeating it \
                 on every later request"
                    .to_string(),
            );
        }
        let assistant_message = turn.message;
        write_message(session.interrupt, rollout, &assistant_message)
            .map_err(|e| format!("could not record the assistant turn: {e}"))?;
        transcript
            .conversation
            .messages
            .push(assistant_message.clone());

        budget.add(&served, turn.usage.as_ref(), estimate);

        // A fresh token for this cell, published to the watcher before the
        // cell can make a call: one Ctrl-C is one cell's cancellation, and
        // `arm` cancels this one on the spot if an earlier interrupt is still
        // pending, so an interrupt raised while nothing was in flight is
        // spent on the next call rather than lost.
        let cell_token = invoke::CancellationToken::new();
        session.interrupt.arm(cell_token.clone());
        runtime.set_token(cell_token);
        // What a subagent inherits and is measured against — Phase 64. Set
        // per turn rather than once, because both change during a task.
        // Zero means "unbounded/unknown" to the subagent binding. The parent
        // accounts for nested work, but cumulative token spend never refuses
        // an agent call or ends the task.
        runtime.set_task_context(0, &session.model.borrow());

        // `events-contract.md` §4: the window that was open while this turn
        // was being answered closes here and its batch is bound into the
        // model's scope -- before the cell runs, so the handle table this
        // cell's own result carries has the `batch` row last and the model
        // sees the events on the very next turn. **No event ever gets a turn
        // of its own** (line 2481): a turn is composed for a user message,
        // and a batch rides the one that was already going to happen.
        if let Some(previous) = runtime.take_batch() {
            window.carry_forward(previous.roll());
        }
        if let Some(batch) = next_batch(&mut window, session.id, EVENT_WAIT) {
            runtime.deliver_batch(batch);
            for (name, outcome) in runtime.run_handlers() {
                let _line = session.interrupt.writing();
                rollout
                    .record_handler(&name, &outcome.turn().record)
                    .map_err(|e| format!("could not record the handler run: {e}"))?;
            }
            if runtime.batch_remaining() > 0 {
                transcript.notebook.batches_delivered += 1;
            }
        }
        transcript.notebook.inbox_depth = window.depth() + runtime.batch_rolling_depth();
        let ordinal = tui::cell_ordinal(&transcript.conversation, &transcript.notebook);
        if let Some(ui) = session.ui {
            ui.publish(transcript, &served, tui::Activity::Executing);
        }
        budget.begin_turn(return_budget(&transcript.notebook, &session.model.borrow()));
        let step = act_on(
            &assistant_message,
            &mut runtime,
            &mut budget,
            rollout,
            session.interrupt,
            &request_profile,
            session.dialect(),
            session,
            &mut task_state,
        );
        let mut step = step?;
        // The approval hint answers on its own thread (approval.rs), so this
        // cell's own confirmation may still be pending when the cell returns;
        // syncing here just means a hint born mid-cell is visible by the next
        // one, never that it is required before this cell can be observed.
        if let Some(gate) = &session.approval_gate {
            let (asked, failed) = gate.hint_counts();
            task_state.approval_hints = asked.saturating_sub(approval_hint_baseline.0);
            task_state.approval_hint_failures = failed.saturating_sub(approval_hint_baseline.1);
        }
        transcript.notebook.handlers = runtime.handlers();
        transcript.notebook.inbox_depth = window.depth() + runtime.batch_rolling_depth();
        transcript.notebook.decision = task_state.decision_line(&session.config().decisions);
        let mut observed = Observed::default();
        if let Some(record) = &step.record {
            let error = step
                .view
                .error
                .as_ref()
                .map(|error| (error.class.as_str(), error.message.as_str()));
            let snapshots = step
                .rollback
                .as_ref()
                .map(|(before, after)| (before, after));
            observed = task_state.observe(record, error, snapshots);
            step.view.capsule = Some(task_state.capsule.to_json());
        }
        task_state.previous_failed = step.view.error.is_some();
        let mut notice_lines = runtime.take_handler_notices();
        notice_lines.extend(observed.notices);
        if session.config().limits.batch_nudge {
            notice_lines.push(prompt::BATCH_NUDGE.to_string());
        }
        let notices = notice_lines.join("\n");
        if !notices.is_empty() {
            if let Some(answer) = &mut step.answer {
                answer.push_str(&format!("\n{notices}"));
            }
            if let Some(history) = &mut step.historical {
                history.push_str(&format!("\n{notices}"));
            }
            if let Some(result) = &mut step.native_result {
                for block in &mut result.content {
                    if let Block::ToolResult { content, .. } = block {
                        content.push_str(&format!("\n{notices}"));
                    }
                }
            }
        }
        // The `## Task` block rides the live feedback only: it is state the
        // next request replaces, so the historical projection never carries
        // it (the same rule as `## Handles`).
        if let Some(block) = observed.capsule_block {
            if let Some(answer) = &mut step.answer {
                answer.push_str("\n\n");
                answer.push_str(&block);
            }
            if let Some(result) = &mut step.native_result {
                for content in &mut result.content {
                    if let Block::ToolResult { content, .. } = content {
                        content.push_str("\n\n");
                        content.push_str(&block);
                    }
                }
            }
        }
        if let Some((before, after)) = step.rollback.take() {
            session.rollbacks.borrow_mut().push(RollbackCheckpoint {
                before,
                after,
                cell: ordinal,
            });
            session.rollback_pending.set(None);
        }
        // Instructions the gate raised ride this cell's result, never the
        // system prompt: history is append-only, and a prompt edited mid-task
        // made every later request miss the provider's cache
        // (`runtime::instructions`).
        let instruction_boundary = runtime.pending_instructions();
        if let Some(pending) = &instruction_boundary {
            let delivery = format!("\n\n{}", pending.text.trim());
            if let Some(answer) = &mut step.answer {
                answer.push_str(&delivery);
            }
            if let Some(history) = &mut step.historical {
                history.push_str(&delivery);
            }
            if let Some(result) = &mut step.native_result {
                for block in &mut result.content {
                    if let Block::ToolResult { content, .. } = block {
                        content.push_str(&delivery);
                    }
                }
                if let Some(history) = &mut result.historical {
                    history.push_str(&delivery);
                }
            }
        }
        // A prose turn runs no cell, so `task_state.observe` never sees it and
        // the stall would sit at zero however long a model talked. It is
        // observed here instead, fingerprinted by what it said: saying
        // something new is progress, saying it again is not. This is what
        // replaced the count of prose turns -- a count cannot tell a model
        // reasoning toward a hard decision from a model stuck.
        if step.prose {
            let said = step
                .response
                .as_deref()
                .or(step.answer.as_deref())
                .unwrap_or_default();
            task_state
                .stall
                .observe(&crate::progress::prose_fingerprint(said));
        }
        if let Some(record) = step.record.take() {
            if delivered_the_interrupt(&record) {
                session.interrupt.consumed();
            }
            last_cell_threw = record.outcome == crate::runtime::outcome::CellOutcomeKind::Threw;
        }

        let poisoned = runtime.poisoned();
        if poisoned {
            let cause = step
                .view
                .error
                .as_ref()
                .map(|error| format!("{}: {}", error.class, error.message))
                .unwrap_or_else(|| {
                    "the cell or its handle inspection exceeded the runtime's hard stop deadline"
                        .into()
                });
            terminal_failure = Some(format!(
                "The task is incomplete: the runtime is poisoned and cannot execute further work. {cause}"
            ));
            step.response = None;
        }
        // The flag is read before this turn decorates it, so the turn that
        // carries the exhausted preamble is sent, answered and only then
        // ends the task -- §6's required final string needs that turn to
        // actually happen.
        let completed = step.answer.is_none();
        let stop = completed || final_turn || poisoned;
        incomplete = poisoned || (stop && !completed);
        // Why a task ends lives in `ending.rs`, which is also where the
        // reasoning for each ender is written down.
        let exhausted = ending::exhausted(
            &ending::Ending {
                cap: session.config().limits.cells,
                cap_reached: budget.cell_limit_reached(),
                stalled_windows: task_state.stall.stalled_windows(),
            },
            crate::progress::DEFAULT_STALL_LIMIT,
            crate::progress::DEFAULT_STALL_WINDOW,
        );
        if !stop && let Some(reason) = exhausted {
            ending::announce(
                &reason,
                &mut step.answer,
                &mut step.historical,
                step.native_result.as_mut(),
            );
            final_turn = true;
        }

        step.view.answered = step.answer.is_some();
        transcript.notebook.tokens = budget.tokens();
        transcript.notebook.set(ordinal, step.view.clone());
        {
            let _line = session.interrupt.writing();
            rollout
                .record_view(ordinal, &step.view)
                .map_err(|e| format!("could not record the cell view: {e}"))?;
        }

        if let Some(result) = step.native_result.take() {
            transcript.conversation.messages.push(result.clone());
            write_message(session.interrupt, rollout, &result)
                .map_err(|e| format!("could not record the native cell result: {e}"))?;
        } else if let Some(answer) = &step.answer {
            transcript
                .conversation
                .messages
                .push(match &step.historical {
                    Some(history) => Message::runtime(answer, history),
                    None => Message::text(Role::User, answer),
                });
            {
                let _line = session.interrupt.writing();
                rollout
                    .record_feedback(Role::User, answer, step.historical.as_deref())
                    .map_err(|e| format!("could not record the runtime's answer: {e}"))?;
            }
        }

        if let Some(pending) = instruction_boundary {
            if pending.fatal {
                runtime.end_task();
                render(transcript, &served, session, tui::Activity::Failed);
                return Err(format!(
                    "Directory instructions could not be loaded completely. {}",
                    pending.text
                ));
            }
            // This acknowledges delivery, never execution or a permission change.
            runtime.acknowledge_instructions();
        }

        // A native call must be paired before any later assistant content,
        // including the synthetic terminal display for a top-level return.
        if let Some(response) = &step.response {
            transcript
                .conversation
                .messages
                .push(Message::text(Role::Assistant, response));
            write_turn(session.interrupt, rollout, Role::Assistant, response)
                .map_err(|e| format!("could not record the terminal response: {e}"))?;
        }

        render(
            transcript,
            &served,
            session,
            if incomplete {
                tui::Activity::Failed
            } else if stop {
                tui::Activity::Complete
            } else {
                tui::Activity::Thinking
            },
        );

        if stop {
            break;
        }
    }

    if let Some(previous) = runtime.take_batch() {
        window.carry_forward(previous.roll());
    }
    transcript.notebook.inbox_depth = window.depth();
    runtime.end_task();
    transcript.notebook.handlers.clear();
    // §5: a background job outlives no task. Every live job is cancelled
    // through `bg::cancel`'s ladder -- `invoke`'s own group kill -- and its
    // thread is joined, so nothing this task started is still running when
    // the isolate that could have read its result is gone.
    bg::shutdown(session.id);
    // A one-shot run waits for a shadow decision so its result records it; a
    // person at the terminal is never held for one.
    task_state.settle_decision(session, session.ui.is_none());
    if let Some(by) = stopped_by_request {
        // Said to the model as well as to the person. A turn that simply
        // stops leaves the next one reading a transcript whose last cell
        // had no answer, and a model reading that guesses -- usually that
        // its work was wrong. It was not; it was interrupted.
        let said = "The person stopped this turn. Work already done stands; do not redo it. \
                    Wait for what they say next.";
        write_turn(session.interrupt, rollout, Role::User, said)
            .map_err(|e| format!("could not record the stop: {e}"))?;
        render(
            transcript,
            &ServedBy::default(),
            session,
            tui::Activity::Stopped(by),
        );
    }
    if incomplete {
        let reason = terminal_failure.unwrap_or_else(|| {
            "Stopped before the work was confirmed done · say what to do next, or ask it to continue."
                .into()
        });
        task_state.salvage(&reason);
        Err(reason)
    } else {
        output::capsule(task_state.capsule.to_json());
        Ok(stopped_by_request)
    }
}

/// §4's delivery decision for one turn: the batch this turn carries, or
/// `None`.
///
/// Three states, and the third is the whole of §4's *"a turn with an empty
/// batch and no user input does not happen: the runtime waits"*:
///
/// - the window is **closed** (an interrupt, or its deadline has passed):
///   its batch is delivered now;
/// - the window is **open with events in it**: this waits, because
///   delivering now would give the events that arrived first a turn of their
///   own and leave the rest for the next one — which is exactly what line
///   2481 forbids. It waits only until §2's deadline closes the window, and
///   never past `budget`;
/// - the window is **empty**: there is nothing to deliver and nothing to wait
///   for — an empty window has no deadline and can never close — so this
///   answers `None` at once and the turn happens because a user message asked
///   for it. **A batch never composes a turn**; that is why an empty one
///   cannot.
///
/// `budget` is a ceiling on the second state, so a clock that jumps backwards
/// or a window whose deadline is misconfigured costs a bounded wait rather
/// than a session that never sends another turn.
fn next_batch(window: &mut Window, session: &SessionId, budget: Duration) -> Option<Batch> {
    let started = Instant::now();
    loop {
        for event in bg::drain(session) {
            window.accept(event, crate::events::now());
        }
        if let Some(batch) = window.close_if_due(crate::events::now()) {
            return Some(batch);
        }
        if window.is_empty() || started.elapsed() >= budget {
            return None;
        }
        std::thread::sleep(EVENT_POLL);
    }
}

#[allow(clippy::too_many_arguments)]
fn act_on(
    assistant: &Message,
    runtime: &mut Runtime,
    budget: &mut TaskSpend,
    rollout: &mut Rollout,
    interrupt: &Interrupter,
    profile: &Profile,
    dialect: crate::abi::Dialect,
    session: &Session<'_>,
    task_state: &mut TaskState,
) -> Result<Step, String> {
    let assistant_text = message_text(assistant);
    let calls: Vec<_> = assistant
        .content
        .iter()
        .filter_map(|block| match block {
            Block::ToolUse { id, name, input } => Some((id, name, input)),
            _ => None,
        })
        .collect();
    // Direct familiar-tool calls: every call is a dialect spelling, so the
    // turn lowers into one frame of the same TypeScript a model could have
    // written and runs through the same executor (`tool-abi.md` §1, §18).
    // A turn mixing `execute_cell` with direct tools is refused, because the
    // two would be one frame whose ordering nothing states.
    let direct = !calls.is_empty()
        && calls.iter().all(|(_, name, _)| *name != "execute_cell")
        && calls
            .iter()
            .all(|(_, name, _)| dialect.lookup(name).is_some());
    let lowered = if direct {
        let requested: Vec<(String, String, serde_json::Value)> = calls
            .iter()
            .map(|(id, name, input)| ((*id).clone(), (*name).clone(), (*input).clone()))
            .collect();
        match crate::abi::lower(dialect, &requested, runtime.next_cell()) {
            Ok(lowered) => Some(lowered),
            Err(error) => {
                // A decode refusal is per call and names what was wrong, so
                // the next turn can repair it without re-reading the schema.
                let message = error.message();
                let content = calls
                    .iter()
                    .map(|(id, _, _)| Block::ToolResult {
                        tool_use_id: (*id).clone(),
                        content: message.clone(),
                        is_error: true,
                    })
                    .collect();
                return Ok(Step {
                    answer: Some(message.clone()),
                    historical: None,
                    native_result: Some(Message {
                        role: Role::User,
                        content,
                        historical: None,
                    }),
                    response: None,
                    prose: true,
                    record: None,
                    rollback: None,
                    view: CellView {
                        error: Some(CellError {
                            class: error.kind.as_str().into(),
                            message,
                            line: None,
                            column: None,
                        }),
                        ..CellView::default()
                    },
                });
            }
        }
    } else {
        None
    };

    if lowered.is_none()
        && (calls.len() > 1 || calls.first().is_some_and(|call| call.1 != "execute_cell"))
    {
        let explanation = if calls.len() > 1 {
            "ProtocolError: exactly one execute_cell call is allowed; nothing ran."
        } else {
            "ProtocolError: unknown tool call; nothing ran."
        };
        let content = calls
            .iter()
            .map(|(id, _, _)| Block::ToolResult {
                tool_use_id: (*id).clone(),
                content: explanation.to_string(),
                is_error: true,
            })
            .collect();
        return Ok(Step {
            answer: Some(explanation.to_string()),
            historical: None,
            native_result: Some(Message {
                role: Role::User,
                content,
                historical: None,
            }),
            response: None,
            prose: true,
            record: None,
            rollback: None,
            view: CellView {
                error: Some(CellError {
                    class: "ProtocolError".into(),
                    message: explanation.into(),
                    line: None,
                    column: None,
                }),
                ..CellView::default()
            },
        });
    }
    let native = lowered.is_none().then(|| calls.first().copied()).flatten();
    let description = native::descriptor(lowered.is_some(), native, &assistant_text);
    let (source, repaired_from) = if let Some(lowered) = &lowered {
        (lowered.source.clone(), None)
    } else if let Some((id, _, input)) = native {
        let Some(source) = native::cell_source(input) else {
            let explanation = native::MALFORMED_CELL_INPUT;
            return Ok(Step {
                answer: Some(explanation.into()),
                historical: None,
                native_result: Some(Message::tool_result(id.clone(), explanation, true)),
                response: None,
                prose: true,
                record: None,
                rollback: None,
                view: CellView {
                    error: Some(CellError {
                        class: "ProtocolError".into(),
                        message: explanation.into(),
                        line: None,
                        column: None,
                    }),
                    ..CellView::default()
                },
            });
        };
        (source.to_string(), None)
    } else {
        match prompt::extract_program(&assistant_text) {
            Extracted::Program(source) => (source, None),
            Extracted::Edit(json) => {
                let patched = runtime
                    .syntax_failure()
                    .ok_or_else(|| "No syntax-failed cell is available in this task.".to_string())
                    .and_then(|failed| {
                        failed
                            .apply(&json)
                            .map(|source| (source, Some(failed.cell)))
                    });
                match patched {
                    Ok(patched) => patched,
                    Err(error) => {
                        let hint = runtime
                            .syntax_failure()
                            .map(|failed| failed.hint())
                            .unwrap_or_default();
                        return Ok(Step {
                            answer: Some(format!("CellEditError: {error} Nothing ran.\n{hint}")),
                            historical: None,
                            native_result: None,
                            response: None,
                            prose: true,
                            record: None,
                            rollback: None,
                            view: CellView {
                                error: Some(CellError {
                                    class: "CellEditError".into(),
                                    message: error,
                                    line: None,
                                    column: None,
                                }),
                                ..CellView::default()
                            },
                        });
                    }
                }
            }
            // §5: the task does not advance and the cell counter does not move.
            // The screen still shows the table, because it is still what the
            // isolate holds -- an output region saying `(no outputs)` beside live
            // handles would be the screen disagreeing with the message sent in
            // the same breath.
            // A prose answer is a completion claim, gated like a `return`.
            Extracted::Prose if let Some(candidate) = prompt::completion_text(&assistant_text) => {
                return Ok(task_state.prose_completion(&candidate, profile, session));
            }
            Extracted::Invalid(error) => {
                return Ok(Step {
                    answer: Some(format!("ProtocolError: {error} Nothing ran.")),
                    historical: None,
                    native_result: None,
                    response: None,
                    prose: true,
                    record: None,
                    rollback: None,
                    view: CellView {
                        error: Some(CellError {
                            class: "ProtocolError".into(),
                            message: error,
                            line: None,
                            column: None,
                        }),
                        ..CellView::default()
                    },
                });
            }
            Extracted::Prose => {
                let table = runtime.render_handles();
                return Ok(Step {
                    answer: Some(unchanged_table(&table)),
                    historical: Some(NO_PROGRAM.to_string()),
                    native_result: None,
                    response: None,
                    prose: true,
                    record: None,
                    rollback: None,
                    view: CellView {
                        table: Some(table),
                        ..CellView::default()
                    },
                });
            }
            Extracted::TwoBlocks => {
                return Ok(Step {
                    answer: Some(TWO_BLOCKS.to_string()),
                    historical: None,
                    native_result: None,
                    response: None,
                    prose: true,
                    record: None,
                    rollback: None,
                    view: CellView {
                        table: Some(runtime.render_handles()),
                        ..CellView::default()
                    },
                });
            }
        }
    };

    // The decision hold (`decision-model.md`): a read-only request above the
    // configured confidence holds the first effectful cell or frame once.
    // Moved to `system.rs` for the size ratchet; nothing about the shape
    // changed.
    if let Some(step) = apply_decision_hold(
        session,
        task_state,
        runtime,
        lowered.as_ref(),
        &source,
        &calls,
    ) {
        return Ok(step);
    }

    // The pre-cell seam, and the one an observer is told about before
    // anything runs: the source is parsed, the certain command lines are
    // already known, and no side effect has happened yet. A line the program
    // will assemble at runtime is deliberately absent and arrives as its own
    // `command.judge` when it is made.
    // The runtime's own number, not a count kept here: the span in this
    // stream is then the same integer as the `cell` on the rollout's line,
    // so a reader correlating the two files never has to translate.
    let submitted = runtime.next_cell() as usize;
    session.observe.cell_submit(
        submitted,
        &source,
        description.as_deref(),
        &crate::runtime::commands::literal_lines(&source),
    );
    let before = crate::changes::Snapshot::capture(profile);
    let outcome = if lowered.is_some() {
        runtime.run_direct_frame(&source)
    } else {
        runtime.run_cell(&source)
    };
    let after = crate::changes::Snapshot::capture(profile);
    let changes = before.diff(&after);
    session.observe.cell_end(
        submitted,
        match outcome.turn().record.outcome {
            crate::runtime::outcome::CellOutcomeKind::Yielded => "yielded",
            crate::runtime::outcome::CellOutcomeKind::Returned => "returned",
            crate::runtime::outcome::CellOutcomeKind::Threw => "threw",
        },
        outcome.turn().record.calls.len(),
    );
    let rollback = changes.is_some().then(|| (before.clone(), after.clone()));
    budget.cells_used = budget.cells_used.saturating_add(1);
    let turn = outcome.turn();
    // The runtime records the program; only this layer saw the message the
    // program came in, so the descriptor is attached here and everything
    // downstream — the rollout, the view, the result — reads it from the
    // one record.
    let mut record = turn.record.clone();
    record.description = description.clone();
    let record = record;
    let origin = if lowered.is_some() {
        crate::abi::Origin::DirectTool
    } else {
        crate::abi::Origin::AuthoredCell
    };
    let thrown = match &outcome {
        CellOutcome::Threw { error, .. } => Some((error.class.as_str(), error.message.as_str())),
        _ => None,
    };
    let reduction = budget.reduction_delta(runtime.reduction_stats());
    write_cell(
        interrupt,
        rollout,
        &record,
        origin,
        thrown,
        turn.observation,
        reduction,
        lowered.is_none()
            && crate::abi::telemetry::is_single_intent_cell(&record.source, &record.calls),
    )
    .map_err(|e| format!("could not record the cell: {e}"))?;

    let mut view = CellView {
        description: description.clone(),
        executed_source: (native.is_some() || repaired_from.is_some() || lowered.is_some())
            .then(|| source.clone()),
        origin: if lowered.is_some() {
            crate::abi::Origin::DirectTool
        } else {
            crate::abi::Origin::AuthoredCell
        },
        repaired_from,
        changes,
        call_count: Some(record.calls.len()),
        execution: Some(cell_view::execution(&record)),
        table: Some(turn.table.clone()),
        stdout: (!turn.stdout_tail.is_empty()).then(|| turn.stdout_tail.clone()),
        ..CellView::default()
    };
    // **The question is answered before the turn is assembled.** The cell
    // that asked is already over, so nothing is suspended while a person
    // reads; what waits is this one function, and it waits at most
    // `ask::MAX_ASK_WAIT` before answering itself that nobody chose.
    let ask_answer = turn.ask.clone().map(|question| {
        let answer = ask::resolve(question.clone(), &after, session, task_state);
        view.asked = Some(answer.row(&question.question));
        answer.rendered()
    });
    let mut result = CellResult {
        cell: turn.record.cell,
        elapsed_ms: turn.elapsed_ms,
        description: description.clone(),
        error: None,
        yield_reason: None,
        output: None,
        ask_answer,
        handle_table: turn.table.clone(),
        stdout_tail: (!turn.stdout_tail.is_empty()).then(|| turn.stdout_tail.clone()),
        budget: budget.line(&session.model.borrow()),
    };

    let mut response = None;
    // **The task ends where the cell said it ends, and nowhere else.** The
    // text is `answer(text)`'s, never a returned value: a return is written
    // to look at something, and reading one as a final answer is what once
    // published a `sed` dump as the session's conclusion. A throw answers
    // nothing -- `ends_the_task` says so -- so a claim that did not survive
    // its own cell cannot end the task either.
    if outcome.ends_the_task()
        && let Some(text) = outcome.answer().map(str::to_string)
    {
        // The evidence gate: fresh contradictory evidence overrides `done`
        // (`smarter-cheaper-roadmap.md`, *Evidence-gated completion*). The
        // candidate is kept as notebook output and the findings reach the
        // next turn.
        if let Some(gate) = task_state.gate(&text, turn.record.cell, &before, &after, session) {
            view.output = Some(gate.clone());
            result.output = Some(gate);
        } else {
            view.returned = Some(text.clone());
            response = Some(text);
        }
    }
    match &outcome {
        // §9.2: what is rendered is a string verbatim and any other value as
        // its JSON -- never `marshal`'s sample. Every one of them is notebook
        // output for the next turn; none of them is an ending.
        CellOutcome::Returned { terminal, .. } if response.is_none() => {
            let text = returned::show(session, runtime, task_state, budget, terminal, &mut result);
            view.output = Some(text);
        }
        CellOutcome::Returned { .. } => {}
        CellOutcome::Threw { error, .. } => {
            view.error = Some(CellError {
                class: error.class.clone(),
                message: error.message.clone(),
                line: error.line,
                column: error.column,
            });
            result.error = Some(ErrorSection {
                class: error.class.clone(),
                message: error.message.clone(),
                position: ErrorSection::position_of(error.line, error.column),
                frames: error
                    .stack
                    .iter()
                    .map(|frame| frame.description.clone())
                    .collect(),
            });
        }
        // §9.3: a yield on purpose says why, under the cell line and beside
        // the table on the screen -- never in the error region.
        CellOutcome::Yielded { turn } => {
            view.yield_reason = turn.yield_reason.clone();
            result.yield_reason = turn.yield_reason.clone();
        }
    }
    // §1: the task ends with a `return` and nothing further is asked of the
    // model; a yield and a throw are answered. The outcome's own predicate
    // decides, so there is no second reading of §1 here to drift from it.
    // Partial effects stay explicit (`smarter-cheaper-roadmap.md`, *Tool
    // outcome semantics*): a thrown cell names the effectful calls that
    // completed before the throw, so one late refusal cannot read as
    // "nothing happened" and cost a turn redoing work that persisted.
    let effects = partial_effects(&record, matches!(outcome, CellOutcome::Threw { .. }));
    let feedback = |mut answer: String| {
        if let Some(effects) = &effects {
            answer.push_str("\n\n## Effects\n");
            answer.push_str(effects);
        }
        if let Some(failed) = runtime.syntax_failure() {
            answer.push_str("\n\n");
            answer.push_str(&failed.hint());
        }
        answer
    };
    let answer = response
        .is_none()
        .then(|| feedback(prompt::render_result(&result)));
    let historical = response
        .is_none()
        .then(|| feedback(prompt::render_result_history(&result)));

    // One provider `tool_result` per direct call, carrying that call's own
    // canonical typed result. The frame's trajectory decides which calls ran:
    // a throw stops the frame, so every later call reports that it did not
    // run rather than silently returning nothing (`tool-abi.md` §20's
    // requested-versus-executed).
    let direct_result = lowered.as_ref().map(|lowered| {
        let turn = outcome.turn();
        let mut produced = turn.capability_results.iter();
        let router = crate::abi::Router::default();
        let content = lowered
            .calls
            .iter()
            .enumerate()
            .map(|(index, call)| {
                let ended = record.calls.get(index).map(|call| &call.ended);
                match ended {
                    Some(crate::runtime::outcome::Ended::Ok) => {
                        let value = produced
                            .next()
                            .and_then(|json| serde_json::from_str(json).ok())
                            .unwrap_or(serde_json::Value::Null);
                        let presented = router.present(call.capability, &call.binding, &value);
                        Block::ToolResult {
                            tool_use_id: call.id.clone(),
                            content: crate::abi::encode_result(&presented).to_string(),
                            is_error: false,
                        }
                    }
                    Some(crate::runtime::outcome::Ended::Denied { rule }) => Block::ToolResult {
                        tool_use_id: call.id.clone(),
                        content: format!("PermissionDenied: {rule}"),
                        is_error: true,
                    },
                    Some(crate::runtime::outcome::Ended::Threw { class }) => Block::ToolResult {
                        tool_use_id: call.id.clone(),
                        // An isolated multi-call frame does not throw, so the
                        // call's own recorded message is the truthful text;
                        // the frame's error is the fallback for a lone call.
                        content: format!(
                            "{class}: {}",
                            record
                                .calls
                                .get(index)
                                .and_then(|call| call.error.clone())
                                .unwrap_or_else(|| cell_error_text(&outcome))
                        ),
                        is_error: true,
                    },
                    None => Block::ToolResult {
                        tool_use_id: call.id.clone(),
                        content: "This call did not run: an earlier call in the same turn \
                                  stopped it."
                            .to_string(),
                        is_error: true,
                    },
                }
            })
            .collect();
        Message {
            role: Role::User,
            content,
            historical: None,
        }
    });

    let native_result = native.map(|(id, _, _)| {
        let with_return = |mut text: String| {
            if let Some(value) = &response {
                text.push_str("\n\n## Return\n");
                text.push_str(value);
            }
            text
        };
        let full = feedback(with_return(prompt::render_result(&result)));
        let history = feedback(with_return(prompt::render_result_history(&result)));
        Message::runtime_tool_result(
            id.clone(),
            full,
            matches!(outcome, CellOutcome::Threw { .. }),
            history,
        )
    });
    Ok(Step {
        answer,
        historical,
        native_result: direct_result.or(native_result),
        response,
        prose: false,
        record: Some(record),
        rollback,
        view,
    })
}

/// Everything the model already holds as text -- the system prompt and every
/// message -- which is what the instruction gate must not deliver again.
fn held_text(conversation: &Conversation) -> String {
    let mut text = conversation.system.clone();
    for message in &conversation.messages {
        for block in &message.content {
            if let Block::Text(body) | Block::ToolResult { content: body, .. } = block {
                text.push('\n');
                text.push_str(body);
            }
        }
    }
    text
}

/// The message of a frame that threw, for a direct call's own error result.
fn cell_error_text(outcome: &CellOutcome) -> String {
    match outcome {
        CellOutcome::Threw { error, .. } => error.message.clone(),
        _ => "the call did not return".to_string(),
    }
}

/// §5's answer to a message that carried no program: the handle table
/// unchanged, and one line saying so. `(none)` rather than an empty section,
/// the same rule [`prompt::render_result`] keeps for the same table.
fn unchanged_table(table: &str) -> String {
    let shown = if table.is_empty() { "(none)" } else { table };
    format!("## Handles\n{shown}\n\n{NO_PROGRAM}")
}

// Measure only the successful request, excluding context recovery and UI work.
fn timed_send_task_turn(
    conversation: &Conversation,
    session: &Session<'_>,
    task: &str,
    cause: crate::abi::telemetry::RequestCause,
) -> Result<(wire::Turn, u64), wire::WireError> {
    let start = Instant::now();
    output::parent_request_started_with(&session.model.borrow(), cause);
    let turn = send_task_turn(conversation, session, task)?;
    let elapsed = start.elapsed().as_millis().min(u128::from(u64::MAX)) as u64;
    Ok((turn, elapsed))
}

/// A turn cut off before anything in it could run is sent again with more
/// room, and a provider that refused the room asked for is asked for less:
/// neither changed the conversation. Each changes the model's room for the
/// rest of the session and has a floor or a ceiling, so this ends.
fn send_task_turn(
    conversation: &Conversation,
    session: &Session<'_>,
    task: &str,
) -> Result<wire::Turn, wire::WireError> {
    loop {
        let sent = send_task_turn_once(conversation, session, task);
        let model = session.model.borrow().clone();
        let again = match &sent {
            Err(wire::WireError::IncompleteResponse { .. }) => wire::raise_output_room(&model),
            Err(error) if wire::refused_size(error) => wire::lower_output_room(&model),
            _ => false,
        };
        if !again {
            return sent;
        }
    }
}

fn send_task_turn_once(
    conversation: &Conversation,
    session: &Session<'_>,
    task: &str,
) -> Result<wire::Turn, wire::WireError> {
    let model = session.model.borrow();
    let mut request = prompt::with_task_context(conversation, &model, task);
    prompt::keep_recent_results(&mut request, session.config().limits.keep_results);
    let conversation = &request;
    let surface = session.surface();
    // A reasoning summary only where a person watches the work live, and
    // there only now and then (`wire::SUMMARY_EVERY`).
    session.routing.plan_summary(session.ui.is_some());
    if let Some(ui) = session.ui {
        wire::send_turn_streaming_cancellable(
            conversation.clone(),
            model.clone(),
            system::turn_effort(session.effort.get(), &model),
            surface,
            Some(session.routing.clone()),
            &|| session.interrupt.pending.load(Ordering::SeqCst),
            &mut |delta| match delta {
                wire::StreamDelta::Text(text) => ui.append_delta(&text),
                // Root's UI integration replaces these no-ops with
                // `tool_delta`; neither fragment belongs in conversation.
                wire::StreamDelta::ToolInput(fragment) => ui.tool_delta(&fragment),
                wire::StreamDelta::ToolReady(_) => {}
                wire::StreamDelta::Reasoning(text) => ui.reasoning_delta(&text),
            },
        )
    } else {
        wire::send_turn_bounded_routed(
            conversation,
            &model,
            system::turn_effort(session.effort.get(), &model),
            None,
            None,
            surface,
            Some(&session.routing),
        )
    }
}

/// Splits a slash command's name from whatever follows it -- `/memory a
/// note` is a name and an argument, `/model` is a name and nothing. Empty
/// input (a bare `/`) yields an empty name, which [`answer_command`] treats
/// the same as `/help`.
fn split_command(rest: &str) -> (&str, Option<&str>) {
    match rest.split_once(char::is_whitespace) {
        Some((name, argument)) => (name, Some(argument.trim())),
        None => (rest, None),
    }
}

/// Answers a slash command. `commands::resolve` only ever *decides* what a
/// command is; acting on one is this function's job. `/memory` is the only
/// built-in with an action beyond naming itself, and a bare `/` or `/help`
/// is this package's chosen way to reach map line 2450's other half: the
/// full list `commands::all` has decided since it was written, and that
/// nothing before this package ever printed.
///
/// **`/memory` is where map line 2446 reaches the binary.** The seam and its
/// local fallback were built and tested by `GH-PANE-61C-SEAMS`, and then
/// nothing called them: `commands` was scoped to decide and never to act, and
/// no package was given the acting half. A capability nothing invokes is not
/// a capability, whatever its tests say.
fn answer_command(
    rest: &str,
    name: &str,
    argument: Option<&str>,
    session: &Session<'_>,
    transcript: &mut Transcript,
) {
    if controls::command(name, argument, session, transcript) {
        return;
    }
    // A bare `/` or `/help` lists rather than resolves, so `commands::all`
    // has a caller and the binary actually *offers* what 2450 names.
    if name == "model" {
        if let Some(argument) = argument.filter(|value| !value.is_empty()) {
            // A session is two models. `/model <id>` stays what it always
            // was -- the parent's -- and a leading tier word assigns the
            // other.
            // The first sentence is the one a mistyped slug has always got;
            // the second is the tiers it can now also name.
            const USAGE: &str = "/model expects one model name\n\
                /model parent|subagent <id> assigns one tier\n\
                /model subagent auto|off (inherit is an alias for auto)";
            // A tier word alone opens the picker on that tier, rather than
            // naming the Main model `subagent`.
            if let Some(tier) = crate::spend::Tier::parse(argument.trim()) {
                controls::models_at(session, tier);
                return;
            }
            let (tier, model) = match argument.split_once(char::is_whitespace) {
                Some((word, rest)) => match crate::spend::Tier::parse(word) {
                    Some(tier) => (tier, rest.trim()),
                    None => {
                        session_println!("{USAGE}");
                        return;
                    }
                },
                None => (crate::spend::Tier::Parent, argument),
            };
            if model.is_empty() || model.chars().any(char::is_whitespace) {
                session_println!("{USAGE}");
                return;
            }
            match controls::use_model(session, tier, model) {
                Ok(outcome) => session_println!("{outcome}"),
                Err(reason) if tier == crate::spend::Tier::Parent => {
                    session_println!("model unchanged: {reason}");
                }
                Err(reason) => session_println!("{reason}"),
            }
            setup::offer(session);
        } else {
            controls::models(session);
        }
        return;
    }
    if name.is_empty() || name == "help" {
        offer_commands(session);
        return;
    }

    // `/tool` is answered before `commands::resolve` is consulted, because
    // it is not a project command: it carries its own arguments on the same
    // line, and a resolver keyed on a bare name would look up
    // `tool read path=…` and answer "unknown".
    // `tool_invocation` reads the **whole** line, not the split-off name:
    // `/tool read path=…` carries its arguments after the command word, and
    // a resolver keyed on the bare name would look up `tool` and lose them.
    if let Some(call) = tool_invocation(rest) {
        answer_tool(call, session);
        return;
    }
    match commands::resolve(session.project, name) {
        Some(resolved) => match resolved.status {
            CommandStatus::Available => {
                if name == "memory" {
                    answer_memory(session.memory, argument);
                } else {
                    let description = match resolved.source {
                        CommandSource::ProjectSkill => "project skill",
                        CommandSource::ProjectCommand => "project command",
                        CommandSource::BuiltIn(_) => "command",
                    };
                    session_println!(
                        "/{name}: {description} found; running it is not supported yet"
                    );
                }
            }
            CommandStatus::Informational => {
                let description = match resolved.source {
                    CommandSource::ProjectSkill => "project skill",
                    CommandSource::ProjectCommand => "project command",
                    CommandSource::BuiltIn(_) => "command",
                };
                session_println!(
                    "/{name}: informational {description}; Sterna does not execute this entry"
                );
            }
        },
        None => session_println!("/{name}: unknown command"),
    }
}

/// Prints every command `commands::all` names, in its own order -- the
/// built-ins, then the project's own commands and skills. `all` had a
/// production caller nowhere before this package; this is that caller.
fn offer_commands(session: &Session<'_>) {
    // A click puts the command in the composer, ready for its argument.
    let row = |name: String, help: &str| {
        let text = format!("{name} · {help}");
        tui::PanelRow::run(text, crate::workbench::Action::Insert(format!("{name} ")))
    };
    let mut rows: Vec<tui::PanelRow> = tui::slash_matches("/")
        .into_iter()
        .map(|(name, help)| row(name, help))
        .collect();
    for command in commands::all(session.project) {
        let name = format!("/{}", command.name);
        if !rows.iter().any(|r| r.text.starts_with(&format!("{name} "))) {
            rows.push(row(name, "a project command"));
        }
    }
    controls::show(session, tui::Panel::rows("Commands", rows));
}

/// Reads memory and the latest checkpoint through Glasshouse's MCP surface,
/// falling back to the local store when nothing answers — map line 2446. A
/// non-empty `argument` is a note to save instead: `/memory <text>` is this
/// package's chosen writer, and it always lands in the local store, the only
/// store `sterna` itself owns -- Glasshouse's own memory tool is written to by
/// Glasshouse's own harness, not by a second writer invented here.
///
/// A read prints what it found and says plainly when that was nothing.
fn answer_memory(memory: &LocalMemory, argument: Option<&str>) {
    if let Some(text) = argument.filter(|text| !text.is_empty()) {
        match memory.add(text) {
            Ok(()) => session_println!("/memory: saved"),
            Err(e) => session_println!("/memory: could not save: {e}"),
        }
        return;
    }

    let notes: Vec<String> = memory
        .search("")
        .into_iter()
        .map(|note| note.text)
        .collect();
    if notes.is_empty() {
        session_println!("/memory: no notes");
    } else {
        for note in &notes {
            session_println!("/memory: {note}");
        }
    }
    match memory.latest() {
        Some(checkpoint) => session_println!("/memory checkpoint: {checkpoint}"),
        None => session_println!("/memory checkpoint: none"),
    }
}

/// The profile one request runs under: the session's, narrowed for a plan
/// request, and without Sterna's own confinement on Full access.
fn request_profile(session: &Session<'_>) -> Profile {
    let profile = session.profile.clone().narrowed_to(session.mode.get());
    match session
        .level
        .as_ref()
        .map(crate::permissions::LiveLevel::level)
    {
        Some(crate::permissions::Level::Full) => profile.with_os_sandbox_bypass(),
        _ => profile,
    }
}

/// The one place a session compiles its [`Profile`], and the one place that
/// prints the sandbox notice.
///
/// **The notice is the observation, not a courtesy.** `sandbox-grants.md`
/// §1.5's "computed once, at session start" is otherwise a property of the
/// call graph that no test can see; because this is the only expression in
/// `sterna session` that produces a `Profile` and it prints as it does so, a
/// second compilation would print a second line, and
/// `tests/tools.rs::the_profile_is_built_once_per_session` counts them.
fn compile_profile_once(project: &ProjectConfig) -> Profile {
    let profile = Profile::from_project(project);
    // A terminal's footer already shows the rule counts and network.
    ui::detail(format!(
        "sandbox: profile compiled once for this session -- {} path rule(s), {} pre-approved \
         command(s), network: {}",
        profile.rule_count(),
        profile.pre_approved().len(),
        if profile.grants_network() {
            "yes"
        } else {
            "no"
        },
    ));
    // Derived grants, said out loud: a person should be able to see that a
    // build can read its toolchain without reading the source for it.
    let toolchain: Vec<String> = profile
        .toolchain_roots()
        .map(|path| path.display().to_string())
        .collect();
    if !toolchain.is_empty() {
        ui::detail(format!(
            "sandbox: the toolchain is readable and runnable -- {} (read only; a registry \
             credential file inside one is not)",
            toolchain.join(", ")
        ));
    }
    let repository: Vec<String> = profile
        .repository_dirs()
        .map(|path| path.display().to_string())
        .collect();
    if !repository.is_empty() {
        ui::detail(format!(
            "sandbox: this root is a git worktree, so its repository is readable and writable -- {}",
            repository.join(", ")
        ));
    }
    for diagnostic in profile.diagnostics() {
        session_println!("sandbox: {diagnostic}");
    }
    profile
}

/// The rest of a `/tool …` line, or `None` for any other slash command.
fn tool_invocation(name: &str) -> Option<&str> {
    match name.strip_prefix("tool") {
        Some("") => Some(""),
        Some(rest) if rest.starts_with(char::is_whitespace) => Some(rest.trim()),
        _ => None,
    }
}

/// Parses `<tool> [name=value …]` into a call.
///
/// A value runs to the next `name=` token or to the end of the line, so
/// `command=echo hello` is one command line rather than two arguments. That
/// is the whole grammar: this is a person's entry point, and the model has
/// none — nothing an assistant returns reaches [`invoke::run`] in this
/// package (map line 2457).
fn parse_tool_line(rest: &str) -> Option<(String, Args)> {
    let mut tokens = rest.split_whitespace();
    let tool = tokens.next()?.to_string();
    let mut args = Args::new();
    let mut current: Option<(String, String)> = None;
    for token in tokens {
        match token.split_once('=') {
            Some((name, value)) if !name.is_empty() => {
                if let Some((name, value)) = current.take() {
                    args = args.with(name, value);
                }
                current = Some((name.to_string(), value.to_string()));
            }
            _ => match current.as_mut() {
                Some((_, value)) => {
                    value.push(' ');
                    value.push_str(token);
                }
                None => return None,
            },
        }
    }
    if let Some((name, value)) = current {
        args = args.with(name, value);
    }
    Some((tool, args))
}

/// Answers `/tool …` — **the sandbox's first production caller**, and map
/// line 2455's "every tool runs confined" reaching the binary.
///
/// A refusal is printed and the session continues: `sandbox-grants.md` §1.4
/// is that a refusal is a value, so nothing here prompts, escalates, retries
/// or returns an error to the caller.
fn answer_tool(rest: &str, session: &Session<'_>) {
    let Some((tool, args)) = parse_tool_line(rest) else {
        session_println!(
            "/tool <name> [arg=value ...]; registered: {}",
            registry::names().join(", ")
        );
        return;
    };
    let ctx = ToolContext {
        profile: &request_profile(session),
        session: session.id,
    };
    match invoke::run(&ctx, &tool, &args) {
        Ok(result) => {
            session_println!("{}{}", result.stdout, result.stderr);
            session_println!(
                "/tool {tool}: exit {} · {}",
                result
                    .exit_code
                    .map(|code| code.to_string())
                    .unwrap_or_else(|| "signal".to_string()),
                result.confinement.plainly()
            );
        }
        Err(ToolError::Denied(denied)) => session_println!("{denied}"),
        Err(error) => session_println!("/tool {tool}: {error}"),
    }
}

#[cfg(test)]
mod tests {

    use super::*;

    /// Instructions are delivered in cell results now, so the next task's
    /// gate must see the conversation as well as the prompt, or it delivers
    /// the same document again and stops a cell for it.
    #[test]
    fn the_instruction_gate_is_seeded_with_the_conversation_and_the_prompt() {
        let conversation = Conversation {
            system: "SYSTEM PROMPT".into(),
            messages: vec![
                Message::text(Role::User, "THE TASK"),
                Message::runtime_tool_result("call", "DELIVERED IN A RESULT", false, "history"),
                Message::runtime("DELIVERED IN FEEDBACK", "history"),
            ],
        };
        let held = held_text(&conversation);
        for part in [
            "SYSTEM PROMPT",
            "THE TASK",
            "DELIVERED IN A RESULT",
            "DELIVERED IN FEEDBACK",
        ] {
            assert!(held.contains(part), "{part} missing from {held}");
        }
    }

    /// `sterna` started in a project names that project, not `.`.
    #[test]
    fn the_project_is_named_by_its_folder_not_by_a_relative_root() {
        let name = startup::project_name(std::path::Path::new("."));
        assert_eq!(name, "sterna", "run from the crate directory: {name}");
    }

    #[test]
    fn startup_model_requires_a_concrete_cli_or_persisted_choice() {
        let empty = SternaConfig::default();
        let error = startup::requested_model(None, &empty, false).unwrap_err();
        assert!(error.contains("--model <id>"), "{error}");
        assert_eq!(
            startup::requested_model(None, &empty, true),
            Ok(None),
            "a terminal session opens the picker instead of refusing"
        );

        for mode in ["auto", "off", "inherit"] {
            assert!(
                startup::requested_model(Some(mode), &empty, true).is_err(),
                "accepted {mode}"
            );
        }

        let persisted = SternaConfig::parse("[model]\nparent = \"persisted-model\"\n").unwrap();
        assert_eq!(
            startup::requested_model(None, &persisted, false).unwrap(),
            Some("persisted-model".to_string())
        );
        assert_eq!(
            startup::requested_model(Some("cli-model"), &persisted, false).unwrap(),
            Some("cli-model".to_string()),
            "the CLI must take precedence"
        );
    }

    fn served(ids: &[&str]) -> Vec<String> {
        ids.iter().map(|id| id.to_string()).collect()
    }

    /// A family word becomes that family's newest served model: `opus` is
    /// `claude-opus-5`, not the first or the dated `4-5` spelling.
    #[test]
    fn a_family_word_resolves_to_its_newest_served_model() {
        let catalogue = served(&[
            "claude-opus-4-7",
            "claude-opus-5",
            "claude-opus-4-5-20251101",
            "claude-opus-4-8",
            "claude-fable-5",
            "claude-fable-5-1",
            "claude-3-5-haiku-20241022",
            "claude-haiku-4-5-20251001",
            "gpt-5.6-sol",
            "gpt-5.6-luna",
        ]);
        let resolve = |word| startup::resolve_family(word, &catalogue);
        assert_eq!(resolve("opus").as_deref(), Some("claude-opus-5"));
        assert_eq!(resolve("Fable").as_deref(), Some("claude-fable-5-1"));
        assert_eq!(
            resolve("haiku").as_deref(),
            Some("claude-haiku-4-5-20251001")
        );
        assert_eq!(resolve("sol").as_deref(), Some("gpt-5.6-sol"));
        assert_eq!(resolve("claude"), None, "several families share the word");
        assert_eq!(resolve("gpt"), None, "several families share the word");
        assert_eq!(resolve("mistral"), None);
        assert_eq!(
            startup::resolve_family(
                "opus",
                &served(&["anthropic/claude-opus-5", "claude-opus-5"])
            )
            .as_deref(),
            Some("claude-opus-5"),
            "the account's own spelling wins a tie"
        );
    }

    /// Jev is the decision model by default exactly when a served account is
    /// TypeSafe's; a configured model and `mode = "off"` are left alone.
    #[test]
    fn jev_is_the_default_decision_model_when_the_gateway_serves_typesafe() {
        let typesafe = startup::ServedAccount {
            account: "typesafe".into(),
            provider: Some("typesafe".into()),
            ..Default::default()
        };
        let chatgpt = startup::ServedAccount {
            account: "chatgpt".into(),
            provider: Some("openai".into()),
            ..Default::default()
        };
        let unset = crate::config::DecisionsConfig::default();
        assert_eq!(
            startup::default_decisions_model(&unset, &[chatgpt.clone(), typesafe.clone()]),
            Some("jev-latest")
        );
        assert_eq!(startup::default_decisions_model(&unset, &[chatgpt]), None);
        let off = crate::config::DecisionsConfig {
            mode: crate::config::DecisionMode::Off,
            ..Default::default()
        };
        assert_eq!(
            startup::default_decisions_model(&off, std::slice::from_ref(&typesafe)),
            None
        );
        let chosen = crate::config::DecisionsConfig {
            model: Some("jev-1.13.0".into()),
            ..Default::default()
        };
        assert_eq!(startup::default_decisions_model(&chosen, &[typesafe]), None);
    }

    /// A listed id is kept, and a family word is settled among accounts that
    /// hold a login before accounts that do not.
    #[test]
    fn settling_keeps_a_listed_id_and_prefers_logged_in_accounts() {
        let account = |name: &str, authenticated, models: &[&str]| startup::ServedAccount {
            account: name.into(),
            models: served(models),
            authenticated,
            ..Default::default()
        };
        let accounts = vec![
            account("router", None, &["vendor/claude-opus-6"]),
            account(
                "claude-max",
                Some(true),
                &["claude-opus-5", "claude-sonnet-5"],
            ),
        ];
        assert_eq!(
            startup::settle_model("claude-sonnet-5".into(), &accounts),
            "claude-sonnet-5"
        );
        assert_eq!(
            startup::settle_model("opus".into(), &accounts),
            "claude-opus-5"
        );
        assert_eq!(startup::settle_model("claude".into(), &accounts), "claude");
    }

    /// A dead subscription login names the account and the way back in; an
    /// unknown model points at the list; anything else gets no advice.
    #[test]
    fn a_recognised_request_failure_says_what_to_do() {
        let dead_login = r#"request failed: http status: 503 — {"type":"error","error":{"type":"api_error","message":"auth_unavailable: no auth available (providers=claude, model=claude-opus-5)"}}"#;
        let accounts = || {
            vec![startup::ServedAccount {
                account: "claude-max".into(),
                models: served(&["claude-opus-5"]),
                authenticated: Some(true),
                connect_with: Some("anthropic".into()),
                ..Default::default()
            }]
        };
        assert_eq!(
            startup::advice(dead_login, "claude-opus-5", accounts, true).as_deref(),
            Some(
                "The `claude-max` login is no longer valid. Type /login claude-max to sign in again."
            )
        );
        let scripted = startup::advice(dead_login, "claude-opus-5", accounts, false).unwrap();
        assert!(
            scripted.ends_with(
                "inference-gateway subscriptions connect --entitlement claude-max anthropic"
            ),
            "{scripted}"
        );
        let unknown = r#"request failed: http status: 400 — {"error":{"message":"unknown provider for model opus"}}"#;
        assert_eq!(
            startup::advice(unknown, "opus", Vec::new, true).as_deref(),
            Some("`opus` is not a model the gateway serves. /model lists the ones it does.")
        );
        assert_eq!(
            startup::advice(
                "request failed: http status: 529",
                "claude-opus-5",
                Vec::new,
                true
            ),
            None
        );
    }

    /// A fixture tree and a profile that admits reading it.
    fn abi_fixture(name: &str) -> (std::path::PathBuf, Profile) {
        let root = std::env::temp_dir().join(format!("sterna-admit-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("target.rs"), "fn admitted() {}\n").unwrap();
        let profile = Profile::compile(&root, Some(r#"{"permissions":{"allow":[]}}"#));
        (root, profile)
    }

    fn direct_call(id: &str, name: &str, input: serde_json::Value) -> Message {
        Message {
            role: Role::Assistant,
            content: vec![Block::ToolUse {
                id: id.into(),
                name: name.into(),
                input,
            }],
            historical: None,
        }
    }

    fn act(
        assistant: &Message,
        root: &std::path::Path,
        profile: &Profile,
        dialect: crate::abi::Dialect,
    ) -> Step {
        let session = SessionId::new("admit");
        let mut runtime = Runtime::new(profile, &session);
        let mut budget = TaskSpend::new(Some(40));
        let mut rollout =
            Rollout::create(&root.join("rollout.jsonl"), session.clone(), "system").unwrap();
        let interrupt = Interrupter::new(session.clone());
        let project = ProjectConfig {
            root: root.to_path_buf(),
            ..ProjectConfig::default()
        };
        let config = RefCell::new(SternaConfig::default());
        let gateway = crate::gateway::Gateway::Command {
            gateway: root.join("absent-gateway"),
        };
        let memory = LocalMemory::new(root);
        let live = Session {
            selected_profile: None,
            pending_images: RefCell::new(Vec::new()),
            observe: crate::observe::Observer::none(),
            approval_gate: None,
            ask_gate: None,
            level: None,
            window: RefCell::new(crate::events::window::Window::new(Default::default())),
            roster: Vec::new(),
            ui: None,
            model: RefCell::new("test".into()),
            context_window: None,
            interface: Cell::new(crate::abi::Interface::default()),
            manifest: crate::manifest::Manifest::default(),
            mode: Cell::new(RequestMode::Work),
            effort: Cell::new(wire::Effort::Auto),
            routing: Default::default(),
            project: &project,
            config: &config,
            interrupt: &interrupt,
            profile,
            gateway: &gateway,
            id: &session,
            memory: &memory,
            rollbacks: RefCell::new(Vec::new()),
            rollback_pending: Cell::new(None),
            plan: RefCell::new(None),
            requests: std::cell::Cell::new(0),
            settings_global: None,
        };
        let mut task_state = TaskState::new("admit", profile, &config.borrow());
        act_on(
            assistant,
            &mut runtime,
            &mut budget,
            &mut rollout,
            &interrupt,
            profile,
            dialect,
            &live,
            &mut task_state,
        )
        .expect("the turn is acted on")
    }

    fn result_blocks(step: &Step) -> Vec<(String, String, bool)> {
        step.native_result
            .as_ref()
            .expect("a direct call is answered with tool results")
            .content
            .iter()
            .map(|block| match block {
                Block::ToolResult {
                    tool_use_id,
                    content,
                    is_error,
                } => (tool_use_id.clone(), content.clone(), *is_error),
                other => panic!("a direct call answers with tool results only: {other:?}"),
            })
            .collect()
    }

    /// The turn a hybrid session exists for: the model emits its familiar
    /// tool, and the tool actually runs and answers.
    #[test]
    fn a_direct_provider_call_runs_and_answers_with_a_typed_result() {
        let (root, profile) = abi_fixture("runs");
        let target = root.join("target.rs");
        let assistant = direct_call(
            "call-1",
            "Read",
            serde_json::json!({"file_path": target.to_string_lossy()}),
        );
        let step = act(&assistant, &root, &profile, crate::abi::Dialect::Anthropic);

        let blocks = result_blocks(&step);
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].0, "call-1", "the result correlates to the call");
        assert!(!blocks[0].2, "the read succeeded: {blocks:?}");

        let value: serde_json::Value = serde_json::from_str(&blocks[0].1).unwrap();
        assert_eq!(value["source"], serde_json::json!("exact"));
        assert_eq!(value["complete"], serde_json::json!(true));
        assert!(
            value["text"].as_str().unwrap().contains("fn admitted()"),
            "the result carries what was actually read: {value}"
        );

        // The capability ran through the ordinary trajectory, so the ledger
        // records it exactly as it records a cell's own call.
        let record = step.record.expect("a direct frame records its cell");
        assert_eq!(record.calls.len(), 1);
        assert_eq!(record.calls[0].tool, "read");

        // The screen must not present Sterna's own spelling of the call as
        // source the model wrote.
        assert_eq!(step.view.origin, crate::abi::Origin::DirectTool);
    }

    /// The other half of the same rule: a real cell is still the model's own.
    #[test]
    fn an_authored_cell_is_still_reported_as_the_models_own_source() {
        let (root, profile) = abi_fixture("authored");
        let assistant = direct_call(
            "call-1",
            "execute_cell",
            serde_json::json!({"code": "return \"done\";"}),
        );
        let step = act(&assistant, &root, &profile, crate::abi::Dialect::Anthropic);
        assert_eq!(step.view.origin, crate::abi::Origin::AuthoredCell);
    }

    /// Both channels land in one field, and the rollout carries it.
    ///
    /// The user, 2026-09-17: *"A model should deliver a descriptor for a cell
    /// to make user facing communication of inner working and thought process
    /// visible."* Measured behind it: 121 `tool_use` blocks and 2 text blocks
    /// across the 123 assistant turns of session `tlitep-13fv`.
    #[test]
    fn a_cells_descriptor_reaches_its_record_from_either_channel() {
        let (root, profile) = abi_fixture("described-native");
        let native = direct_call(
            "call-1",
            "execute_cell",
            serde_json::json!({
                "code": "return \"done\";",
                "description": "Answering from what I already read.",
            }),
        );
        let step = act(&native, &root, &profile, crate::abi::Dialect::Anthropic);
        assert_eq!(
            step.record
                .as_ref()
                .and_then(|record| record.description.as_deref()),
            Some("Answering from what I already read.")
        );
        assert_eq!(
            step.view.description.as_deref(),
            Some("Answering from what I already read."),
            "the screen reads the same field the rollout does"
        );

        let (root, profile) = abi_fixture("described-fence");
        let fenced = Message::text(
            Role::Assistant,
            "Answering from what I already read.\n```sterna\nreturn \"done\";\n```",
        );
        let step = act(&fenced, &root, &profile, crate::abi::Dialect::Anthropic);
        assert_eq!(
            step.record
                .as_ref()
                .and_then(|record| record.description.as_deref()),
            Some("Answering from what I already read."),
            "the fence channel's line is the same field"
        );
    }

    /// A model that says nothing still runs: the schema asks for the line,
    /// the parser does not require it, and nothing downstream reports an
    /// error where a sentence would have been.
    #[test]
    fn a_native_call_without_a_description_still_runs() {
        let (root, profile) = abi_fixture("undescribed");
        let assistant = direct_call(
            "call-1",
            "execute_cell",
            serde_json::json!({"code": "return \"done\";"}),
        );
        let step = act(&assistant, &root, &profile, crate::abi::Dialect::Anthropic);
        let record = step.record.expect("the cell ran");
        assert_eq!(record.description, None);
        assert!(step.view.error.is_none(), "{:?}", step.view.error);
    }

    /// Several independent familiar calls in one turn become one frame, and
    /// each still gets its own correlated answer.
    #[test]
    fn independent_direct_calls_answer_individually_from_one_frame() {
        let (root, profile) = abi_fixture("fused");
        let target = root.join("target.rs");
        let assistant = Message {
            role: Role::Assistant,
            content: vec![
                Block::ToolUse {
                    id: "a".into(),
                    name: "Read".into(),
                    input: serde_json::json!({"file_path": target.to_string_lossy()}),
                },
                Block::ToolUse {
                    id: "b".into(),
                    name: "Glob".into(),
                    input: serde_json::json!({"pattern": "*.rs"}),
                },
            ],
            historical: None,
        };
        let step = act(&assistant, &root, &profile, crate::abi::Dialect::Anthropic);

        let blocks = result_blocks(&step);
        assert_eq!(blocks.len(), 2, "{blocks:?}");
        assert_eq!(blocks[0].0, "a");
        assert_eq!(blocks[1].0, "b");
        assert!(!blocks[0].2 && !blocks[1].2, "{blocks:?}");
        let record = step.record.expect("one frame");
        assert_eq!(record.calls.len(), 2, "one frame ran both calls");
    }

    /// A malformed familiar call is refused per call, naming what was wrong,
    /// and nothing runs.
    #[test]
    fn a_malformed_direct_call_is_refused_by_name_and_runs_nothing() {
        let (root, profile) = abi_fixture("malformed");
        let assistant = direct_call("call-1", "Read", serde_json::json!({"path": "target.rs"}));
        let step = act(&assistant, &root, &profile, crate::abi::Dialect::Anthropic);

        let blocks = result_blocks(&step);
        assert_eq!(blocks.len(), 1);
        assert!(blocks[0].2, "an undeclared parameter is an error");
        assert!(
            blocks[0].1.contains("file_path"),
            "the refusal names the declared parameter: {}",
            blocks[0].1
        );
        assert!(step.record.is_none(), "nothing ran, so no cell is recorded");
    }

    /// The OpenAI façade reaches the same capability as the Anthropic one.
    #[test]
    fn the_other_dialect_reaches_the_same_capability() {
        let (root, profile) = abi_fixture("dialect");
        let assistant = direct_call("call-1", "shell", serde_json::json!({"command": "true"}));
        let step = act(&assistant, &root, &profile, crate::abi::Dialect::OpenAi);
        let record = step.record.expect("a direct frame records its cell");
        assert_eq!(record.calls.len(), 1);
        assert_eq!(record.calls[0].tool, "bash");
    }

    /// A turn mixing a cell with direct tools is refused rather than guessed
    /// at: the two would be one frame whose ordering nothing states.
    #[test]
    fn a_turn_mixing_a_cell_with_direct_tools_runs_nothing() {
        let (root, profile) = abi_fixture("mixed");
        let assistant = Message {
            role: Role::Assistant,
            content: vec![
                Block::ToolUse {
                    id: "a".into(),
                    name: "execute_cell".into(),
                    input: serde_json::json!({"code": "return 1;"}),
                },
                Block::ToolUse {
                    id: "b".into(),
                    name: "Read".into(),
                    input: serde_json::json!({"file_path": "target.rs"}),
                },
            ],
            historical: None,
        };
        let step = act(&assistant, &root, &profile, crate::abi::Dialect::Anthropic);
        assert!(step.record.is_none(), "nothing ran");
        let blocks = result_blocks(&step);
        assert!(blocks.iter().all(|(_, _, is_error)| *is_error));
    }

    #[test]
    fn only_a_tool_line_is_a_tool_invocation() {
        assert_eq!(tool_invocation("tool read path=x"), Some("read path=x"));
        assert_eq!(tool_invocation("tool"), Some(""));
        assert_eq!(tool_invocation("tooling"), None);
        assert_eq!(tool_invocation("memory"), None);
    }

    #[test]
    fn task_spend_counts_cache_reads_and_creation_without_inventing_them() {
        let usage = wire::Usage {
            input_tokens: 10,
            output_tokens: 5,
            cache_read_input_tokens: Some(70),
            cache_creation_input_tokens: Some(20),
        };
        let mut direct = TaskSpend::new(Some(10));
        direct.add(&ServedBy::default(), Some(&usage), 999);
        assert_eq!(direct.used(), 105);

        let mut gateway = TaskSpend::new(Some(10));
        gateway.add(
            &ServedBy {
                input_tokens: Some(3),
                output_tokens: Some(4),
                cached_input_tokens: Some(80),
                ..ServedBy::default()
            },
            Some(&usage),
            999,
        );
        assert_eq!(gateway.used(), 107);

        let mut absent = TaskSpend::new(Some(10));
        absent.add(
            &ServedBy::default(),
            Some(&wire::Usage {
                input_tokens: 3,
                output_tokens: 4,
                cache_read_input_tokens: None,
                cache_creation_input_tokens: None,
            }),
            999,
        );
        assert_eq!(absent.used(), 7);
    }

    #[test]
    fn request_context_replaces_the_previous_request_and_preserves_its_window() {
        let mut notebook = Notebook {
            context: Some(ContextTokens {
                used: 9,
                cap: Some(1_048_576),
                cap_source: crate::models::WindowSource::Observed,
                counted: Counted::Estimated,
            }),
            ..Notebook::default()
        };
        for (input, cached) in [(100, 20), (250, 50)] {
            record_request(
                &mut notebook,
                RequestMeasurement::from_response(
                    1,
                    "gemini-3.8-flash-high".into(),
                    10,
                    ServedBy::default(),
                    Some(&wire::Usage {
                        input_tokens: input,
                        output_tokens: 999,
                        cache_read_input_tokens: Some(cached),
                        cache_creation_input_tokens: None,
                    }),
                ),
            );
        }
        assert_eq!(notebook.requests.len(), 2);
        assert_eq!(
            notebook.context,
            Some(ContextTokens {
                used: 300,
                cap: Some(1_048_576),
                // The window's provenance survives a new request exactly as
                // the figure does: a reading replaces the count, never what
                // the meter is allowed to claim about the cap.
                cap_source: crate::models::WindowSource::Observed,
                counted: Counted::Gateway,
            })
        );
    }

    #[test]
    fn a_value_runs_to_the_next_name_equals_token() {
        let (tool, args) = parse_tool_line("bash command=echo hello world").unwrap();
        assert_eq!(tool, "bash");
        assert_eq!(args.get("command"), Some("echo hello world"));
    }

    /// One `bg.done`, the only kind §5 produces.
    fn bg_done(source: &str) -> crate::events::Event {
        use crate::events::{Event, Kind, PayloadRef, Priority};
        Event::pending(
            Kind::BgDone {
                emission: source.to_string(),
            },
            source,
            crate::events::now(),
            PayloadRef::new(format!("{source}#exit")),
            Priority::Batch,
            "a job finished",
        )
    }

    /// `events-contract.md` §4: **"a turn with an empty batch and no user
    /// input does not happen: the runtime waits."**
    ///
    /// An empty window is the case where there is nothing to wait *for*: it
    /// has no deadline and can never close, so no batch is ever delivered and
    /// the turn that follows happens because a user message asked for it. A
    /// batch never composes a turn, which is why an empty one cannot.
    ///
    /// **This is also what keeps the test from hanging**, and it is the
    /// property being asserted rather than a convenience: the budget is
    /// thirty seconds and is never reached, because an empty window is
    /// answered on the first pass. A `next_batch` that waited out its budget
    /// here would fail this test rather than slow it down.
    #[test]
    fn an_empty_window_delivers_nothing_and_waits_for_nothing() {
        let session = SessionId::new("next-batch-empty");
        let mut window = Window::new(WindowConfig::default());
        let started = Instant::now();
        assert!(
            next_batch(&mut window, &session, Duration::from_secs(30)).is_none(),
            "an empty window produced a batch"
        );
        assert!(
            started.elapsed() < Duration::from_millis(200),
            "an empty window was waited on for {:?}; it can never close",
            started.elapsed()
        );
        bg::shutdown(&session);
    }

    /// Line 2481's first clause: **an event does not get a turn of its own
    /// while a batch window is open.** A window holding an event that has not
    /// reached §2's deadline is waited on, not delivered — and the wait is
    /// bounded by the budget, which is the only reason this test terminates
    /// if the deadline logic is ever wrong.
    #[test]
    fn an_open_window_is_waited_on_rather_than_delivered_in_pieces() {
        let session = SessionId::new("next-batch-open");
        let mut window = Window::new(WindowConfig::default());
        window.accept(bg_done("bg/job1"), crate::events::now());
        let started = Instant::now();
        assert!(
            next_batch(&mut window, &session, Duration::from_millis(200)).is_none(),
            "an open window was delivered before §2's deadline closed it"
        );
        assert!(
            started.elapsed() >= Duration::from_millis(200),
            "the open window was not waited on at all: {:?}",
            started.elapsed()
        );
        bg::shutdown(&session);
    }

    /// And the other side of it: once the window is due, **both** events
    /// arrive as one batch — the second never became a turn of its own.
    #[test]
    fn a_due_window_delivers_every_event_it_holds_as_one_batch() {
        use crate::events::Stamp;
        let session = SessionId::new("next-batch-due");
        let mut window = Window::new(WindowConfig::default());
        let long_ago = Stamp::from_millis(crate::events::now().as_millis() - 3_000);
        window.accept(bg_done("bg/job1"), long_ago);
        window.accept(bg_done("bg/job2"), long_ago);
        let batch = next_batch(&mut window, &session, Duration::from_millis(200))
            .expect("a window past its deadline closes");
        assert_eq!(batch.n, 2, "the events were split across two deliveries");
        bg::shutdown(&session);
    }

    #[test]
    fn two_arguments_are_kept_apart() {
        let (tool, args) = parse_tool_line("grep pattern=fn path=/tmp").unwrap();
        assert_eq!(tool, "grep");
        assert_eq!(args.get("pattern"), Some("fn"));
        assert_eq!(args.get("path"), Some("/tmp"));
    }

    #[test]
    fn partial_effects_name_only_completed_effectful_calls_of_a_thrown_cell() {
        use crate::runtime::outcome::{CallRecord, CellOutcomeKind};
        use std::collections::BTreeMap;
        let call = |tool: &str, key: &str, value: &str, ended: Ended| CallRecord {
            tool: tool.into(),
            args: BTreeMap::from([(key.to_string(), value.to_string())]),
            evidence: None,
            lifted_from: None,
            exit_code: None,
            repeat_of: None,
            error: None,
            ended,
        };
        let record = CellRecord {
            cell: 3,
            source: String::new(),
            description: None,
            outcome: CellOutcomeKind::Threw,
            handles: Vec::new(),
            calls: vec![
                call("read", "path", "/p/a.py", Ended::Ok),
                call("edit", "path", "/p/a.py", Ended::Ok),
                call("bash", "command", "make all", Ended::Ok),
                call(
                    "rg",
                    "path",
                    "/build",
                    Ended::Denied {
                        rule: "no grant".into(),
                    },
                ),
            ],
        };
        let effects = partial_effects(&record, true).unwrap();
        assert!(effects.contains("edit /p/a.py"), "{effects}");
        assert!(effects.contains("bash `make all`"), "{effects}");
        assert!(!effects.contains("read"), "{effects}");
        assert!(!effects.contains("rg"), "{effects}");
        assert!(partial_effects(&record, false).is_none());
        let reads_only = CellRecord {
            calls: vec![call("read", "path", "/p/a.py", Ended::Ok)],
            ..record
        };
        assert!(partial_effects(&reads_only, true).is_none());
    }
}
