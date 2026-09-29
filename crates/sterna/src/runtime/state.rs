//! What the host keeps while a task's cells run.
//!
//! One [`RuntimeState`] per [`crate::runtime::isolate::Runtime`], reachable
//! from every host callback through the isolate's slot. It holds the
//! session's own `Profile`, `Glasshouse` seam and `SessionId` — **cloned from
//! the session's, never compiled here**: nothing in `runtime/**` calls
//! `Profile::compile`, which is `sandbox-grants.md` §1.5's guarantee that a
//! program cannot widen the sandbox it runs in — plus the live handle table,
//! the cell's captured `console` output, and the calls whose results became
//! objects.

use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};

use crate::config::HelpersConfig;
use crate::contract::SessionId;
use crate::helpers::{HelperCall, HelperRecord};
use crate::project::source_context::SourceContext;
use crate::runtime::handles::{HandleMeta, HandleTable, Provenance};
use crate::runtime::instructions::{InstructionContext, PendingInstructions};
use crate::runtime::observation::ReductionStats;
use crate::runtime::outcome::{PlanItem, SourceEvidence};
use crate::runtime::preview::{self, Value};
use crate::sandbox::profile::{Access, Profile};
use crate::tools::invoke::CancellationToken;

/// Told when a helper call starts and again when it ends, with every call
/// this cell has made so far.
pub(crate) type HelperProgress = Rc<dyn Fn(&[HelperRecord])>;

thread_local! {
    static HELPER_PROGRESS: RefCell<Option<HelperProgress>> = const { RefCell::new(None) };
}

/// Installs the signal helper progress is reported through on this thread,
/// and answers with whatever it replaced.
///
/// **Thread-local, and it is the seam `session::ui`'s `OUTPUT` already is**: a
/// task's cells run on the session's own thread, so the terminal a running
/// helper must reach is the one belonging to the thread the call is made
/// from, and `runtime/**` names no terminal type of its own. A runtime built
/// without a session -- `agent.rs`'s subagents, every test -- finds none,
/// which is the absent case rather than a special one.
pub(crate) fn install_helper_progress(signal: Option<HelperProgress>) -> Option<HelperProgress> {
    HELPER_PROGRESS.with(|slot| std::mem::replace(&mut *slot.borrow_mut(), signal))
}

/// A `console` capture bounded ahead of rendering.
///
/// It keeps at most twice [`preview::STDOUT_TOKEN_CAP`]'s worth of characters
/// and drops from the front, so a program logging in a loop costs a bounded
/// amount of memory rather than a growing one, and `runtime-contract.md`
/// §3's "the rest is dropped with a count" is a number this already has.
#[derive(Debug, Default)]
pub(crate) struct ConsoleCapture {
    buffer: String,
    kept_chars: usize,
    dropped_chars: usize,
}

/// The characters [`preview::STDOUT_TOKEN_CAP`] tokens are worth, by the
/// `chars / 4` estimate the whole crate shares.
const KEEP_CHARS: usize = preview::STDOUT_TOKEN_CAP * 4;

/// Room the console's true-tail marker keeps for itself, so no context
/// promoted as visible can have its beginning cut off by the renderer that
/// writes that marker. Reserved whatever else is queued.
const CONTEXT_MARKER_RESERVE: usize = 256;

/// How many `CallSite::PostResult` reductions one task remembers, so a
/// repeated command is served rather than reduced again.
///
/// Small on purpose: this answers "did *this* task already reduce exactly
/// this output", which is a question about the last few tool calls, not a
/// cache of the session.
const REDUCTIONS_KEPT: usize = 16;

impl ConsoleCapture {
    pub(crate) fn write_line(&mut self, line: &str) {
        self.buffer.push_str(line);
        self.buffer.push('\n');
        self.kept_chars += line.chars().count() + 1;
        // Trim in one move per KEEP_CHARS appended rather than on every
        // write, so a program logging a million short lines pays O(1) per
        // line amortised.
        if self.kept_chars > 2 * KEEP_CHARS {
            self.trim_to(KEEP_CHARS);
        }
    }

    fn trim_to(&mut self, keep: usize) {
        if self.kept_chars <= keep {
            return;
        }
        let drop = self.kept_chars - keep;
        let byte = self
            .buffer
            .char_indices()
            .nth(drop)
            .map_or(self.buffer.len(), |(index, _)| index);
        self.buffer.drain(..byte);
        self.kept_chars -= drop;
        self.dropped_chars += drop;
    }

    /// The tail the turn shows, and how many tokens were dropped ahead of it.
    pub(crate) fn tail(&mut self) -> (String, usize) {
        self.trim_to(KEEP_CHARS);
        if self.dropped_chars == 0 {
            return (self.buffer.clone(), 0);
        }
        // The omission must be visible in stdout itself: callers that have
        // not yet plumbed the numeric field still cannot mistake a tail for
        // complete output. Recompute after reserving the marker because that
        // reservation itself may drop a few more characters.
        for _ in 0..2 {
            let marker = format!(
                "[console: ~{} tokens omitted before this true tail]\n",
                self.dropped_chars.div_ceil(4)
            );
            self.trim_to(KEEP_CHARS.saturating_sub(marker.chars().count()));
        }
        let dropped = self.dropped_chars.div_ceil(4);
        let marker = format!("[console: ~{dropped} tokens omitted before this true tail]\n");
        let mut shown = marker;
        shown.push_str(&self.buffer);
        debug_assert!(shown.chars().count() <= KEEP_CHARS);
        (shown, dropped)
    }

    pub(crate) fn clear(&mut self) {
        self.buffer.clear();
        self.kept_chars = 0;
        self.dropped_chars = 0;
    }
}

/// One tool call whose result became a live object, kept so the binding that
/// holds that object can be given its provenance and its already-built
/// preview instead of being marshalled a second time.
#[derive(Debug, Clone)]
pub(crate) struct RecordedCall {
    pub(crate) preview: Value,
    pub(crate) meta: HandleMeta,
}

/// A value a cell captured, in the order it was captured.
#[derive(Debug, Clone)]
pub(crate) struct Capture {
    pub(crate) name: String,
    pub(crate) value: Value,
    pub(crate) meta: HandleMeta,
}

/// The per-cell half of [`RuntimeState`], reset at the start of every cell.
#[derive(Debug, Default)]
pub(crate) struct CellState {
    pub(crate) console: ConsoleCapture,
    pub(crate) captures: Vec<Capture>,
    /// Names the model `keep`t this cell: they render in full every turn
    /// until redeclared (`handles::HandleTable::pin`).
    pub(crate) pinned: Vec<String>,
    /// Names the model's own `free` released during this cell. A capture of
    /// one is skipped: `runtime-contract.md` §2 makes `free` a lifetime
    /// event, and re-capturing at the end of the cell would undo it.
    pub(crate) freed: Vec<String>,
    /// Every helper call this cell completed, in call order — the one field
    /// `CellView.helpers` is built from.
    pub(crate) helpers: Vec<HelperRecord>,
    /// Helper calls **claimed** this cell, which is what the per-cell ceiling
    /// counts. A call in flight has claimed its slot and left no record yet,
    /// so counting the records instead would let a loop overrun the ceiling
    /// by whatever is outstanding.
    pub(crate) helper_calls: u32,
}

/// What every host callback can reach.
pub(crate) struct RuntimeState {
    pub(crate) handlers: Rc<crate::runtime::handlers::Handlers>,
    pub(crate) profile: Profile,
    /// Host-only suspension seam; absent in ordinary sessions and subagents.
    pub(crate) approval_gate: RefCell<Option<crate::approval::Gate>>,
    /// Why `ask` cannot be used in this runtime, or `None` when it can.
    ///
    /// Held as the refusal sentence rather than a flag so the callback throws
    /// the reason a person would want -- nobody at the keyboard, the setting
    /// off, or the request narrowed to `explore` -- instead of one word that
    /// covers three different situations.
    pub(crate) ask_refusal: RefCell<Option<String>>,
    /// How long this cell has spent inside host callbacks rather than
    /// executing JavaScript, which its watchdog subtracts from the
    /// wall-clock limit ([`crate::approval::WaitClock`]). Always present,
    /// because a `cargo test` is granted to every session while an approval
    /// gate is granted to almost none.
    pub(crate) host_clock: Arc<crate::approval::WaitClock>,
    /// The current watchdog's host-visible flag. V8's termination query may
    /// stay false until a blocked Rust callback returns to an interrupt check.
    pub(crate) watchdog_fired: RefCell<Option<Arc<AtomicBool>>>,
    pub(crate) mcp: RefCell<crate::tools::mcp::Mcp>,
    pub(crate) web: RefCell<Option<crate::web::WebBroker>>,
    /// The narrowing this runtime's context was built under, kept because
    /// the one global the configuration decides (`web`) is bound after
    /// construction, when the broker arrives — `Runtime::with_web_broker`.
    pub(crate) globals: crate::runtime::bindings::HostGlobals,
    /// Whether `web` has been bound, so a second broker does not bind twice.
    pub(crate) web_bound: std::cell::Cell<bool>,
    /// Whether `decide` has been bound, for the same reason as `web_bound`:
    /// the configuration decides whether the global exists, and it arrives
    /// after the context is built.
    pub(crate) decide_bound: std::cell::Cell<bool>,
    /// Plain `bash` checks that passed, and the tree they passed on.
    pub(crate) shell_checks: RefCell<crate::verification::ShellChecks>,
    pub(crate) agent_templates: crate::project::agents::Catalog,
    pub(crate) effective_config: RefCell<Option<crate::config::SternaConfig>>,
    pub(crate) session: SessionId,
    pub(crate) cell: std::cell::Cell<u64>,
    pub(crate) table: RefCell<HandleTable>,
    pub(crate) current: RefCell<CellState>,
    /// The token every tool call this runtime makes is cancellable through.
    /// It is replaceable because [`crate::runtime::isolate::Runtime::with_token`]
    /// is a builder over an already-constructed runtime, and a cell only ever
    /// reads it, so there is no path by which a program can reach it.
    pub(crate) token: RefCell<CancellationToken>,
    /// Every call whose result became a live object, by the id the object is
    /// tagged with.
    ///
    /// **Task-scoped, not cell-scoped.** A tag minted in cell *n* is read
    /// again when a later cell rebinds the object it names — by an
    /// assignment, by `keep`, or by the end-of-cell re-marshal — so a
    /// per-cell store would answer with whatever call happened to sit at the
    /// same position in the later cell, and the handle would be shown
    /// another call's preview and provenance. The map is cleared by
    /// [`RuntimeState::forget_calls`] when the task ends, which is the
    /// lifetime `runtime-contract.md` §2 gives a handle.
    calls: RefCell<HashMap<u64, RecordedCall>>,
    next_call: std::cell::Cell<u64>,
    /// The model's own plan, replaced whole by each `todo.write`.
    ///
    /// **Task-scoped, like [`calls`](Self::calls) and for the same reason**:
    /// a plan is the shape of the task in hand, so it is cleared with the
    /// task rather than carried into the next one.
    plan: RefCell<Vec<PlanItem>>,
    /// Whether this runtime belongs to a subagent, which may not start one.
    pub(crate) subagent: std::cell::Cell<bool>,
    /// What the parent task has left to spend, in tokens, refreshed each turn.
    /// `0` means unknown rather than exhausted -- a runtime nobody told is not
    /// a runtime that must refuse.
    pub(crate) budget_remaining: std::cell::Cell<u64>,
    /// The model the parent task is using, so a subagent inherits it rather
    /// than silently falling back to the compiled-in default.
    pub(crate) model: RefCell<String>,
    pub(crate) instructions: RefCell<InstructionContext>,
    /// `[helpers]` as the session read it. The default carries no `model`,
    /// which is helpers **off**: a runtime nobody configured spends nothing
    /// on the user's behalf.
    helpers: RefCell<HelpersConfig>,
    /// `[agents]` as the session read it: what a delegated goal runs on when
    /// the cell does not name a model.
    agents: RefCell<crate::config::AgentsConfig>,
    /// `[decisions]`, for the one question a *cell* may ask: `decide.choice`
    /// routes on `model`, and an unset model is why the global is not bound.
    decisions: RefCell<crate::config::DecisionsConfig>,
    /// What `CallSite::PostResult` has already reduced this task, keyed by
    /// the SHA-256 of the text it reduced.
    ///
    /// The invariant: **no value is reduced twice.** A cell is code, so the
    /// same command inside a loop is ordinary; without this, each identical
    /// result would buy an answer the task already holds. A hit is served
    /// from here and claims no helper call, so the ceiling is spent on
    /// distinct outputs only.
    ///
    /// Bounded by discarding the oldest. What is stored is the reduction, not
    /// the output -- one helper `max_tokens` each -- so a long task pays a
    /// small fixed cost rather than one that grows with it.
    reductions: RefCell<Vec<(String, String)>>,
    /// Filters this task has written, by the shape signature of the output
    /// each was written for.
    ///
    /// **This is the cache that pays.** [`reductions`](Self::reductions) is
    /// keyed by the digest of the exact bytes, so it answers "has this task
    /// already reduced *this* output" — true inside a loop and almost never
    /// otherwise, because two runs of one command differ in their counts. A
    /// filter is written for a *shape*, and two runs of one tool share one:
    /// so a filter written in an early cell answers every later run of the
    /// same tool for nothing.
    filters: RefCell<Vec<(String, String)>>,
    /// Versions whose exact editing context has crossed a completed cell
    /// boundary and therefore reached the model.
    visible_sources: RefCell<HashMap<PathBuf, String>>,
    /// Context produced in the cell currently running. It becomes visible at
    /// the next cell boundary, never earlier merely because code holds it.
    pending_sources: RefCell<Vec<(PathBuf, String)>>,
    /// Paths a context was queued for in the cell currently running WITHOUT
    /// its target whole -- an oversized definition delivered short, or a
    /// window where no boundary was found. They never certify a version.
    pending_incomplete: RefCell<Vec<PathBuf>>,
    /// The same, once they have crossed a cell boundary and therefore
    /// reached the model. Kept for one reason only: so a refused `edit` can
    /// say which of two things happened -- nothing was read, or something
    /// was and not enough of it. A complete context for the same path
    /// supersedes the entry, because then enough of it has been read.
    incomplete_sources: RefCell<HashSet<PathBuf>>,
    pending_context_output: RefCell<Vec<String>>,
    /// Every source context delivered this task, by the SHA-256 of its exact
    /// rendering, with the cell whose result carries it.
    ///
    /// **The conversation is append-only, so those bytes are still in front
    /// of the model.** A later context that renders the same bytes -- the
    /// rendering names path, symbol and file version, so a changed file
    /// never matches -- is delivered as its header and one line naming that
    /// cell, never as a second copy. Forgotten with the task, and when a
    /// checkpoint replaces the conversation: the one time earlier results
    /// leave the request.
    shown_contexts: RefCell<HashMap<String, u64>>,
    /// Every source line a delivered context showed the model, by path and
    /// line number, with its exact text -- the ledger an `edit` may bind to
    /// when no whole version is visible ([`Self::seen_lines_cover`]).
    seen_lines: RefCell<HashMap<PathBuf, BTreeMap<usize, String>>>,
    /// The same for the cell running now; they reach the model, and so the
    /// ledger, at the next cell boundary.
    pending_lines: RefCell<Vec<(PathBuf, usize, String)>>,
    /// Every pure observation this task has made, by tool, checked arguments
    /// and result digest, with the first call that made it.
    ///
    /// The invariant: **a repeat is decided by the bytes, never by skipping
    /// the call.** The call runs and the hash says whether the observation
    /// changed, so an unchanged file is reported as a repeat and a changed
    /// one is not. Task-scoped like [`calls`](Self::calls).
    observations: RefCell<HashMap<ObservationKey, Observed>>,
    /// The binding each recorded call's result was captured under, so a
    /// repeat can name the handle that already holds the same bytes.
    bindings: RefCell<HashMap<u64, String>>,
    repeated_observations: std::cell::Cell<usize>,
    /// The pushed reducer's work this task, reset with the handles.
    reduction: RefCell<ReductionStats>,
}

/// One pure observation's identity: the tool, its arguments as checked, and
/// the SHA-256 of what it returned.
type ObservationKey = (String, BTreeMap<String, String>, String);

/// Where an observation was first made.
#[derive(Debug, Clone, Copy)]
struct Observed {
    cell: u64,
    call: Option<u64>,
}

/// An earlier pure call whose observation the new one repeated byte for byte.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Repeat {
    /// The cell of the first call that made this observation.
    pub(crate) cell: u64,
    /// The handle that already holds the same bytes, when the first call's
    /// result was bound to a name.
    pub(crate) binding: Option<String>,
}

impl Repeat {
    /// The suffix a repeated handle's type label carries, so the table header
    /// says the repetition without the body being re-read.
    pub(crate) fn label_suffix(&self) -> String {
        match &self.binding {
            Some(binding) => format!(" (unchanged since cell {}, same as `{binding}`)", self.cell),
            None => format!(" (unchanged since cell {})", self.cell),
        }
    }
}

/// The key [`RuntimeState::shown_contexts`] is kept under.
fn rendering_key(rendered: &str) -> String {
    use sha2::{Digest, Sha256};
    format!("{:x}", Sha256::digest(rendered.as_bytes()))
}

/// Slots a pushed helper may never take, so the model's own `helper.*` calls
/// survive an automatic reduction that fired several times first.
const RESERVED_FOR_THE_MODEL: u32 = 2;

impl RuntimeState {
    /// Stops the cell's compute clock until the guard is dropped.
    ///
    /// **Every host callback that can outlast a keystroke takes one.** The
    /// cell is not computing while a child process builds, an MCP server
    /// answers or a helper thinks, and the wall-clock limit is there to stop
    /// a cell that computes forever. Without this the limit kills the work
    /// instead: the tool path polls the watchdog's own flag
    /// (`bindings::tool_callback`'s `stopped` closure), so at 30 seconds a
    /// `cargo test` was reaped mid-build and the call answered `cancelled`.
    ///
    /// The limit still catches `while (true) {}`, and still catches a loop
    /// that spams host calls -- that loop's own JavaScript keeps accruing
    /// between calls, so it dies later rather than never, in proportion to
    /// how much of its time is really its own.
    pub(crate) fn away_from_js(&self) -> crate::approval::Waiting {
        self.host_clock.pause()
    }

    /// The narrowing the context is built under — `Every` unless told.
    pub(crate) fn with_globals(mut self, globals: crate::runtime::bindings::HostGlobals) -> Self {
        self.globals = globals;
        self
    }

    pub(crate) fn new(profile: &Profile, session: &SessionId) -> Self {
        Self {
            handlers: crate::runtime::handlers::Handlers::new(),
            profile: profile.clone(),
            approval_gate: RefCell::new(None),
            ask_refusal: RefCell::new(Some(crate::ask::NOT_AVAILABLE.to_string())),
            watchdog_fired: RefCell::new(None),
            host_clock: Arc::new(crate::approval::WaitClock::default()),
            mcp: RefCell::new(crate::tools::mcp::Mcp::default()),
            web: RefCell::new(None),
            globals: crate::runtime::bindings::HostGlobals::Every,
            web_bound: std::cell::Cell::new(false),
            decide_bound: std::cell::Cell::new(false),
            agent_templates: crate::project::agents::Catalog::load(profile),
            effective_config: RefCell::new(None),
            shell_checks: RefCell::default(),
            session: session.clone(),
            cell: std::cell::Cell::new(0),
            table: RefCell::new(HandleTable::new()),
            current: RefCell::new(CellState::default()),
            token: RefCell::new(CancellationToken::new()),
            calls: RefCell::new(HashMap::new()),
            next_call: std::cell::Cell::new(0),
            plan: RefCell::new(Vec::new()),
            subagent: std::cell::Cell::new(false),
            budget_remaining: std::cell::Cell::new(0),
            model: RefCell::new(crate::wire::MODEL.to_string()),
            instructions: RefCell::new(InstructionContext::default()),
            helpers: RefCell::new(HelpersConfig::default()),
            agents: RefCell::new(crate::config::AgentsConfig::default()),
            decisions: RefCell::new(crate::config::DecisionsConfig::default()),
            reductions: RefCell::new(Vec::new()),
            filters: RefCell::new(Vec::new()),
            visible_sources: RefCell::new(HashMap::new()),
            pending_sources: RefCell::new(Vec::new()),
            pending_incomplete: RefCell::new(Vec::new()),
            incomplete_sources: RefCell::new(HashSet::new()),
            seen_lines: RefCell::default(),
            pending_lines: RefCell::default(),
            pending_context_output: RefCell::new(Vec::new()),
            shown_contexts: RefCell::new(HashMap::new()),
            observations: RefCell::new(HashMap::new()),
            bindings: RefCell::new(HashMap::new()),
            repeated_observations: std::cell::Cell::new(0),
            reduction: RefCell::new(ReductionStats::default()),
        }
    }

    pub(crate) fn enable_instruction_context(&self) {
        self.instructions.borrow_mut().enable(&self.profile);
    }

    pub(crate) fn instruction_boundary(
        &self,
        tool: &str,
        args: &crate::tools::invoke::Args,
    ) -> bool {
        self.instructions
            .borrow_mut()
            .gate(&self.profile, tool, args)
    }

    pub(crate) fn pending_instructions(&self) -> Option<PendingInstructions> {
        self.instructions.borrow().pending()
    }

    pub(crate) fn seed_delivered_instructions(&self, system: &str) {
        self.instructions.borrow_mut().seed_delivered(system);
    }

    pub(crate) fn instructions_before_cell(&self, source: &str) -> bool {
        self.instructions
            .borrow_mut()
            .before_cell(&self.profile, source)
    }

    pub(crate) fn instruction_stop_reason(&self, before: &str, not_done: &str) -> String {
        self.instructions.borrow().stop_reason(before, not_done)
    }

    pub(crate) fn instruction_file_written(
        &self,
        tool: &str,
        args: &crate::tools::invoke::Args,
    ) -> bool {
        self.instructions
            .borrow_mut()
            .instruction_file_written(&self.profile, tool, args)
    }

    pub(crate) fn acknowledge_instructions(&self) {
        self.instructions.borrow_mut().acknowledge();
    }

    /// Replaces the plan whole — `todo.write`'s only effect.
    pub(crate) fn set_plan(&self, items: Vec<PlanItem>) {
        *self.plan.borrow_mut() = items;
    }

    /// The plan as it stands, for `todo.read` and for the turn the cell ends.
    pub(crate) fn plan(&self) -> Vec<PlanItem> {
        self.plan.borrow().clone()
    }

    /// The ordinal the next cell will take.
    ///
    /// Read before the frame runs so a lowered direct call can name its
    /// bindings deterministically; `begin_cell` is what actually advances it.
    pub(crate) fn next_cell(&self) -> u64 {
        self.cell.get() + u64::from(!self.handlers.running.get())
    }

    pub(crate) fn begin_cell(&self) -> u64 {
        self.visible_sources
            .borrow_mut()
            .extend(self.pending_sources.borrow_mut().drain(..));
        self.incomplete_sources
            .borrow_mut()
            .extend(self.pending_incomplete.borrow_mut().drain(..));
        {
            let mut seen = self.seen_lines.borrow_mut();
            for (path, line, text) in self.pending_lines.borrow_mut().drain(..) {
                seen.entry(path).or_default().insert(line, text);
            }
        }
        let cell = self.cell.get() + u64::from(!self.handlers.running.get());
        self.cell.set(cell);
        let mut current = self.current.borrow_mut();
        current.console.clear();
        current.captures.clear();
        current.freed.clear();
        current.pinned.clear();
        current.helpers.clear();
        current.helper_calls = 0;
        cell
    }

    pub(crate) fn set_helpers(&self, helpers: HelpersConfig) {
        *self.helpers.borrow_mut() = helpers;
    }

    /// `[helpers] reduce_above_tokens`: the estimated size above which a
    /// command result is worth a pushed reduction.
    pub(crate) fn reduce_above_tokens(&self) -> usize {
        self.helpers.borrow().reduce_above_tokens
    }

    /// Adds to this task's reduction figures.
    pub(crate) fn count_reduction(&self, count: impl FnOnce(&mut ReductionStats)) {
        count(&mut self.reduction.borrow_mut());
    }

    pub(crate) fn reduction_stats(&self) -> ReductionStats {
        *self.reduction.borrow()
    }

    /// Whether a pure call with these checked arguments already returned
    /// exactly these bytes this task, and if so which call did.
    pub(crate) fn observation_repeat(
        &self,
        tool: &str,
        args: &BTreeMap<String, String>,
        sha256: &str,
    ) -> Option<Repeat> {
        let key = (tool.to_string(), args.clone(), sha256.to_string());
        let observed = *self.observations.borrow().get(&key)?;
        self.repeated_observations
            .set(self.repeated_observations.get() + 1);
        Some(Repeat {
            cell: observed.cell,
            binding: observed
                .call
                .and_then(|id| self.bindings.borrow().get(&id).cloned()),
        })
    }

    /// Remembers a pure observation under the call that made it. The first
    /// call keeps the entry, so every later repeat points at it.
    pub(crate) fn note_observation(
        &self,
        tool: &str,
        args: &BTreeMap<String, String>,
        sha256: &str,
        call: Option<u64>,
    ) {
        self.observations
            .borrow_mut()
            .entry((tool.to_string(), args.clone(), sha256.to_string()))
            .or_insert(Observed {
                cell: self.cell.get(),
                call,
            });
    }

    /// The name a recorded call's result was bound under, for a later repeat
    /// to point at. The first binding wins: it is the one the model will
    /// still recognise.
    pub(crate) fn note_binding(&self, call: u64, name: &str) {
        self.bindings
            .borrow_mut()
            .entry(call)
            .or_insert_with(|| name.to_string());
    }

    pub(crate) fn repeated_observations(&self) -> usize {
        self.repeated_observations.get()
    }

    /// The model helpers run on, or the sentence saying why there is none.
    ///
    /// The invariant: **a helper never runs unasked.** `[helpers] model`
    /// unset is off, because a
    /// helper spends money on the user's behalf and the fail-closed direction
    /// is *not configured, not run*.
    /// Resolves a delegated goal only inside the explicitly configured assignment.
    #[cfg(test)]
    pub(crate) fn agent_model(&self, asked: Option<String>) -> Result<String, String> {
        self.agents
            .borrow()
            .select(asked.as_deref(), None)
            .map(|s| s.model)
    }
    pub(crate) fn agent_assignment(
        &self,
        model: Option<&str>,
        slot: Option<&str>,
        requested_effort: crate::wire::Effort,
        explicit_effort: bool,
    ) -> Result<(String, crate::wire::Effort), String> {
        let policy = self.agents.borrow();
        let selected = policy.select(model, slot)?;
        let effort = if policy.mode == crate::config::AgentsMode::Roster {
            if explicit_effort && requested_effort != selected.effort {
                return Err("effort is fixed by the selected favorite slot".into());
            }
            selected.effort
        } else {
            requested_effort
        };
        Ok((selected.model, effort))
    }

    /// The wall clock one subagent of this session gets, from `[agents]
    /// deadline_minutes`. `None` is no deadline.
    pub(crate) fn agent_deadline(&self) -> Option<std::time::Duration> {
        self.agents.borrow().deadline
    }

    /// `[decisions]` for this runtime. Held rather than read from
    /// `effective_config` so a narrowed child runtime that never received one
    /// answers "unconfigured" instead of inheriting a parent's model.
    pub(crate) fn set_decisions(&self, decisions: crate::config::DecisionsConfig) {
        *self.decisions.borrow_mut() = decisions;
    }

    /// The model a cell's own question goes to, or why there is none.
    ///
    /// `mode` is deliberately not consulted: `off` silences the *harness's*
    /// gates, which can hold a cell or narrow a grant. A question the program
    /// asked itself changes nothing it did not already choose to branch on,
    /// so the model naming it is the whole of the configuration here.
    pub(crate) fn decision_model(&self) -> Result<String, String> {
        self.decisions.borrow().model.clone().ok_or_else(|| {
            "the decision model is not configured: set `[decisions] model` in .sterna/config.toml".to_string()
        })
    }

    /// Whether `decide` exists for this runtime at all — the predicate the
    /// binding and the declaration both answer to.
    pub(crate) fn decisions_configured(&self) -> bool {
        self.decisions.borrow().model.is_some()
    }

    pub(crate) fn set_agents(&self, agents: crate::config::AgentsConfig) {
        *self.agents.borrow_mut() = agents;
    }

    pub(crate) fn helper_route(
        &self,
        helper: &str,
    ) -> Result<(String, crate::wire::Effort), String> {
        let helpers = self.helpers.borrow();
        if !helpers.enabled {
            return Err(
                "helpers are off: `[helpers] enabled` is false in .sterna/config.toml".to_string(),
            );
        }
        let model = helpers.model.clone().ok_or_else(|| {
            "helpers are not configured: set `[helpers] model` in .sterna/config.toml".to_string()
        })?;
        let effort = helpers.effort.for_helper(helper).ok_or_else(|| {
            format!("helper `{helper}` has no reasoning effort policy in .sterna/config.toml")
        })?;
        Ok((model, effort))
    }

    /// Takes one of this cell's helper-call slots, or says the ceiling is
    /// reached. `little-helpers.md`'s *cost is bounded per cell*: a program
    /// is code, so a helper call sits inside a loop, and the refusal is what
    /// the model catches instead of the loop running away.
    pub(crate) fn claim_helper_call(&self) -> Result<(), String> {
        self.claim_helper_slot(0)
    }

    /// A slot claimed by a helper the MODEL DID NOT ASK FOR — today the
    /// post-result reduction.
    ///
    /// The invariant: **a pushed helper never starves a pulled one.** Both
    /// spend the same per-cell budget, so an automatic reduction firing on
    /// several oversized results could leave a model that then reaches for
    /// `helper.find` refused for a call it never made. `reserved` slots stay
    /// for the model's own calls.
    pub(crate) fn claim_pushed_helper_call(&self) -> Result<(), String> {
        self.claim_helper_slot(RESERVED_FOR_THE_MODEL)
    }

    fn claim_helper_slot(&self, reserved: u32) -> Result<(), String> {
        let ceiling = self.helpers.borrow().calls_per_cell;
        // Reserve only what there is room to reserve. At a ceiling of one or
        // two there is nothing to protect — the model is refused either way —
        // so a pushed call still gets its single slot rather than the feature
        // silently turning itself off on a small budget.
        let available = if ceiling == 0 {
            0
        } else {
            ceiling.saturating_sub(reserved).max(1)
        };
        let mut current = self.current.borrow_mut();
        if current.helper_calls >= available {
            // Names the ceiling and the key that sets it, and does not tell
            // the program what to do about it. This refusal throws into the
            // cell rather than ending it, so the control flow is the
            // program's own; a message that said "yield" was the ceiling
            // dictating a round trip it had no business dictating.
            return Err(format!(
                "this cell has spent its {ceiling} helper call(s); `[helpers] calls_per_cell` sets that ceiling and accepts up to 64"
            ));
        }
        current.helper_calls += 1;
        Ok(())
    }

    /// A helper call starting: the record is kept with its outcome unfilled
    /// and the progress signal fires, so the lane can show the call **while
    /// it is in flight**. Answers with the slot [`finish_helper`] resolves.
    ///
    /// The invariant: a call is visible from the moment it starts. Its wire
    /// call blocks this thread, so a record kept only on return can never be
    /// rendered as running -- which is the whole of `little-helpers.md`'s
    /// lane. `record.outcome` and `record.turns` are what the call has
    /// produced so far, which at the start is nothing: leave them at their
    /// defaults rather than at the spec's ceiling.
    ///
    /// [`finish_helper`]: RuntimeState::finish_helper
    pub(crate) fn begin_helper(&self, record: HelperRecord) -> usize {
        let slot = {
            let mut current = self.current.borrow_mut();
            current.helpers.push(record);
            current.helpers.len() - 1
        };
        self.report_helper_progress();
        slot
    }

    /// The call in `slot` resolving: what came back and the turns it actually
    /// took replace the unfilled ones, and the progress signal fires again.
    ///
    /// It takes the whole [`HelperCall`] rather than its outcome because
    /// `turns` is only known once the call has answered, and a record left at
    /// the turns it was allowed is exactly what the inspector section exists
    /// to make visible.
    pub(crate) fn finish_helper(&self, slot: usize, call: HelperCall) {
        if let Some(record) = self.current.borrow_mut().helpers.get_mut(slot) {
            record.outcome = call.outcome;
            record.turns = call.turns;
            record.looked = call.looked;
            record.usage = call.usage;
        }
        self.report_helper_progress();
    }

    /// Hands this cell's calls to the installed signal, if there is one. The
    /// signal is cloned out before it runs, so it may install another.
    fn report_helper_progress(&self) {
        let Some(signal) = HELPER_PROGRESS.with(|slot| slot.borrow().clone()) else {
            return;
        };
        let records = self.current.borrow().helpers.clone();
        signal(&records);
    }

    /// The reduction already made for text with this digest, if there is one.
    ///
    /// Answering from here is what makes the second identical result free:
    /// it is read before [`claim_helper_call`] so a hit spends neither a
    /// request nor a slot of the cell's ceiling.
    ///
    /// [`claim_helper_call`]: RuntimeState::claim_helper_call
    pub(crate) fn reduction_of(&self, digest: &str) -> Option<String> {
        self.reductions
            .borrow()
            .iter()
            .find(|(seen, _)| seen == digest)
            .map(|(_, reduction)| reduction.clone())
    }

    /// Keeps one reduction against the digest of the text it reduced,
    /// discarding the oldest once [`REDUCTIONS_KEPT`] are held.
    pub(crate) fn remember_reduction(&self, digest: String, reduction: String) {
        let mut reductions = self.reductions.borrow_mut();
        if reductions.len() >= REDUCTIONS_KEPT {
            reductions.remove(0);
        }
        reductions.push((digest, reduction));
    }

    /// The filter this task already wrote for output of this shape.
    ///
    /// Read before the economics test and before any slot is claimed, so a
    /// hit costs neither a request nor a wait -- it is one `run_filter` over
    /// the new text, and the provenance line is recomputed from *that* text
    /// rather than served with the filter.
    pub(crate) fn filter_of(&self, signature: &str) -> Option<String> {
        self.filters
            .borrow()
            .iter()
            .find(|(seen, _)| seen == signature)
            .map(|(_, filter)| filter.clone())
    }

    /// Keeps one filter against the shape signature it was written for.
    pub(crate) fn remember_filter(&self, signature: String, filter: String) {
        let mut filters = self.filters.borrow_mut();
        if filters.len() >= REDUCTIONS_KEPT {
            filters.remove(0);
        }
        filters.push((signature, filter));
    }

    /// Every helper call the cell that just ran completed, in call order.
    pub(crate) fn helper_records(&self) -> Vec<HelperRecord> {
        self.current.borrow().helpers.clone()
    }

    /// Records a call whose result became a live object, and answers with the
    /// **task-scoped** id the object is tagged with. Ids start at 1, so a
    /// zero read back from a tag is not a call.
    pub(crate) fn record_call(&self, call: RecordedCall) -> u64 {
        let id = self.next_call.get() + 1;
        self.next_call.set(id);
        self.calls.borrow_mut().insert(id, call);
        id
    }

    pub(crate) fn recorded(&self, id: u64) -> Option<RecordedCall> {
        self.calls.borrow().get(&id).cloned()
    }

    /// The task ending. Every handle is gone, so every call recorded for one
    /// is too.
    pub(crate) fn forget_calls(&self) {
        self.calls.borrow_mut().clear();
        self.next_call.set(0);
        // The plan is the shape of the task that just ended, so it goes with
        // it: a next task inheriting the last one's checklist would be
        // reporting work it never did.
        self.plan.borrow_mut().clear();
        // Reductions describe results that were held behind the handles this
        // just dropped, so they end with them.
        self.reductions.borrow_mut().clear();
        self.filters.borrow_mut().clear();
        self.visible_sources.borrow_mut().clear();
        self.pending_sources.borrow_mut().clear();
        self.pending_incomplete.borrow_mut().clear();
        self.incomplete_sources.borrow_mut().clear();
        self.seen_lines.borrow_mut().clear();
        self.pending_lines.borrow_mut().clear();
        self.pending_context_output.borrow_mut().clear();
        self.shown_contexts.borrow_mut().clear();
        self.observations.borrow_mut().clear();
        self.bindings.borrow_mut().clear();
        self.repeated_observations.set(0);
        *self.reduction.borrow_mut() = ReductionStats::default();
    }

    /// How many characters this turn's feedback budget still holds for
    /// source context.
    ///
    /// Asked before a context is queued, so one that does not fit can be
    /// narrowed to what is left rather than refused. A caller that only
    /// learns "it did not fit" has to spend a round trip discovering by how
    /// much; this is that number, given before the decision instead of
    /// after it.
    pub(crate) fn remaining_context_budget(&self) -> usize {
        let output = self.pending_context_output.borrow();
        let used: usize = output.iter().map(|s| s.chars().count() + 1).sum();
        KEEP_CHARS
            .saturating_sub(CONTEXT_MARKER_RESERVE)
            .saturating_sub(used)
            // The newline `flush_source_context` writes after this one.
            .saturating_sub(1)
    }

    /// Queue a context that fits the remaining feedback budget, answering
    /// whether it did.
    ///
    /// Callers narrow to [`Self::remaining_context_budget`] first, so a
    /// refusal here means the bare target alone is larger than what is left
    /// -- the one case there is nothing to deliver.
    ///
    /// A context whose exact rendering an earlier result already carries is
    /// queued as [`SourceContext::render_shown`] instead: it still certifies
    /// its version for `edit`, because the model has those bytes in front of
    /// it.
    pub(crate) fn note_source_context(
        &self,
        evidence: &SourceEvidence,
        packed: &SourceContext,
    ) -> bool {
        let full = packed.render();
        let key = rendering_key(&full);
        let shown = self.shown_contexts.borrow().get(&key).copied();
        let text = match shown {
            Some(cell) if cell == self.cell.get() => packed.render_shown("earlier in this result"),
            Some(cell) => packed.render_shown(&format!("in cell {cell}'s result")),
            None => full,
        };
        let mut output = self.pending_context_output.borrow_mut();
        let used: usize = output.iter().map(|s| s.chars().count() + 1).sum();
        if used + text.chars().count() + 1 > KEEP_CHARS.saturating_sub(CONTEXT_MARKER_RESERVE) {
            return false;
        }
        let path = self.absolute_source_path(Path::new(&evidence.path));
        if evidence.complete {
            self.incomplete_sources.borrow_mut().remove(&path);
            self.pending_sources
                .borrow_mut()
                .push((path, evidence.sha256.clone()));
        } else {
            self.pending_incomplete.borrow_mut().push(path);
        }
        output.push(text);
        if shown.is_none() {
            self.shown_contexts
                .borrow_mut()
                .insert(key, self.cell.get());
        }
        true
    }

    /// Whether this context's exact rendering already reached the model this
    /// task, so it needs no room beyond one line.
    pub(crate) fn context_shown(&self, packed: &SourceContext) -> bool {
        self.shown_contexts
            .borrow()
            .contains_key(&rendering_key(&packed.render()))
    }

    /// Forgets which contexts the conversation carries, for when it stops
    /// carrying them.
    pub(crate) fn forget_shown_contexts(&self) {
        self.shown_contexts.take();
    }

    /// Why an `edit` is not yet bound to a version of its path, in terms the
    /// program can act on rather than the rule restated.
    ///
    /// **The cross-turn requirement is not a formality and is not loosened
    /// here.** `begin_cell` is what moves a context from pending to visible,
    /// and `flush_source_context` writes the text after the cell's own
    /// output -- so a version becomes bindable exactly when the model has
    /// actually read it. An edit in the same cell as its context would bind
    /// to bytes nobody had seen. What was wrong was the message: it told a
    /// caller to do the thing it had just done, without saying which of four
    /// situations it was in.
    pub(crate) fn edit_binding_gap(&self, args: &crate::tools::invoke::Args) -> String {
        let Some(named) = args.get("path") else {
            return "`edit` did not run: it names no `path` to bind a version to".into();
        };
        let path = self.absolute_source_path(Path::new(named));
        if self
            .pending_sources
            .borrow()
            .iter()
            .any(|(p, _)| p == &path)
        {
            return format!(
                "`edit` did not run: a context for `{named}` is in this turn's feedback and you have not read it yet; it binds from the next cell, so make the edit there"
            );
        }
        if self.pending_incomplete.borrow().contains(&path)
            || self.incomplete_sources.borrow().contains(&path)
        {
            return format!(
                "`edit` did not run: the context for `{named}` reached you without its target whole, so no version binds to it; name an inner symbol that fits, read that result, and edit in the next cell"
            );
        }
        if self.visible_sources.borrow().contains_key(&path) {
            return format!(
                "`edit` did not run: the version of `{named}` you read is not the one this edit names; call `context` for its current bytes and edit in the next cell"
            );
        }
        format!(
            "`edit` did not run: nothing has shown you `{named}`'s current bytes; call `context` with the target symbol, read its result, and edit in the next cell"
        )
    }

    /// Records every line of `excerpts` -- a context just queued for this
    /// turn's feedback, target and supporting excerpts alike -- as shown.
    pub(crate) fn note_seen_lines<'a>(
        &self,
        excerpts: impl IntoIterator<Item = &'a crate::project::source_context::SourceExcerpt>,
    ) {
        let mut pending = self.pending_lines.borrow_mut();
        for excerpt in excerpts {
            let path = self.absolute_source_path(Path::new(&excerpt.path));
            for (offset, line) in excerpt.text.lines().enumerate() {
                pending.push((path.clone(), excerpt.range.start + offset, line.to_string()));
            }
        }
    }

    /// The current version of an `edit`'s file, when **every line the edit
    /// touches is a line the model was shown and is byte-identical now** --
    /// the same guarantee a visible version gives, held line by line.
    /// Each `old` must occur exactly once; a line never shown, or shown and
    /// since changed, answers `None` and the edit is refused as before.
    pub(crate) fn seen_lines_cover(&self, args: &crate::tools::invoke::Args) -> Option<String> {
        let path = self.absolute_source_path(Path::new(args.get("path")?));
        let seen = self.seen_lines.borrow();
        let seen = seen.get(&path)?;
        let bytes = std::fs::read(&path).ok()?;
        let text = std::str::from_utf8(&bytes).ok()?;
        let joined = |items: &[String]| format!("{}\n", items.join("\n"));
        let olds: Vec<String> = match (args.get("old"), args.items("oldLines"), args.items("olds"))
        {
            (Some(old), None, None) => vec![old.to_string()],
            (None, Some(lines), None) => vec![joined(lines)],
            (None, None, Some(olds)) => olds.to_vec(),
            _ => return None,
        };
        let lines: Vec<&str> = text.lines().collect();
        for old in olds.iter().filter(|old| !old.is_empty()) {
            let mut found = text.match_indices(old.as_str());
            let (at, _) = found.next()?;
            if found.next().is_some() {
                return None;
            }
            let first = text[..at].matches('\n').count() + 1;
            let last = first + old.trim_end_matches('\n').matches('\n').count();
            for number in first..=last {
                if seen.get(&number).map(String::as_str) != lines.get(number - 1).copied() {
                    return None;
                }
            }
        }
        use sha2::{Digest, Sha256};
        Some(format!("{:x}", Sha256::digest(&bytes)))
    }

    /// Delivers the lines around each place a failing check named, with this
    /// cell's feedback, and records them as shown -- so the fix can be the
    /// next cell's edit instead of a request spent reading the failure.
    /// Answers how many were delivered; one that does not fit the budget is
    /// left out, never cut short.
    pub(crate) fn note_failure_locations(&self, locations: &[(PathBuf, usize)]) -> usize {
        const AROUND: usize = 4;
        let mut delivered = 0;
        for (path, line) in locations {
            let Ok(text) = std::fs::read_to_string(path) else {
                continue;
            };
            let lines: Vec<&str> = text.lines().collect();
            let first = line.saturating_sub(AROUND).max(1);
            let last = (line + AROUND).min(lines.len());
            if first > last {
                continue;
            }
            let shown = path.strip_prefix(self.profile.root()).unwrap_or(path);
            let mut rendered = format!(
                "## Failure location (attached by Sterna)\n{}:{line}\n",
                shown.display()
            );
            for number in first..=last {
                rendered.push_str(&format!("{number:>5} | {}\n", lines[number - 1]));
            }
            if rendered.chars().count() + 1 > self.remaining_context_budget() {
                continue;
            }
            self.pending_context_output.borrow_mut().push(rendered);
            let mut pending = self.pending_lines.borrow_mut();
            for number in first..=last {
                pending.push((path.clone(), number, lines[number - 1].to_string()));
            }
            delivered += 1;
        }
        delivered
    }

    /// Appends the complete batch after model-authored output. Contexts that
    /// did not fit were not queued or certified as visible.
    pub(crate) fn flush_source_context(&self) {
        for text in self.pending_context_output.borrow_mut().drain(..) {
            self.current.borrow_mut().console.write_line(&text);
        }
    }

    pub(crate) fn source_version_is_visible(&self, args: &crate::tools::invoke::Args) -> bool {
        let (Some(path), Some(hash)) = (args.get("path"), args.get("expected_sha256")) else {
            return false;
        };
        let path = self.absolute_source_path(Path::new(path));
        self.visible_sources
            .borrow()
            .get(&path)
            .is_some_and(|seen| seen == &hash.to_ascii_lowercase())
    }

    /// The latest complete version of this path that reached the model.
    /// Refreshing after an edit replaces the old version instead of making
    /// every future implicit edit ambiguous. The writer still checks disk's
    /// actual hash, so an external change remains a stale-version refusal.
    pub(crate) fn visible_source_hash(&self, args: &crate::tools::invoke::Args) -> Option<String> {
        let path = self.absolute_source_path(Path::new(args.get("path")?));
        self.visible_sources.borrow().get(&path).cloned()
    }

    /// The version of `path` an implicit `edit` would bind to right now.
    pub(crate) fn visible_version(&self, path: &Path) -> Option<String> {
        let path = self.absolute_source_path(path);
        self.visible_sources.borrow().get(&path).cloned()
    }

    /// A successful `edit` or `write` made `sha256` the version on disk, and
    /// the model holds the result that says so — so it is visible at once,
    /// in this cell and every later one, and the next edit binds to it.
    ///
    /// The invariant: **only Sterna's own mutation moves the visible version.**
    /// The writer still compares the disk hash, so a change something else
    /// made between two of Sterna's edits stays a stale refusal. A context of
    /// the same path still pending from earlier in this cell is dropped: it
    /// describes bytes the mutation just replaced.
    pub(crate) fn note_mutation(&self, path: &Path, sha256: &str) {
        let path = self.absolute_source_path(path);
        self.pending_sources
            .borrow_mut()
            .retain(|(pending, _)| pending != &path);
        // Line numbers below the change may have moved; the new version is
        // visible whole, so nothing is lost by forgetting them.
        self.seen_lines.borrow_mut().remove(&path);
        self.pending_lines
            .borrow_mut()
            .retain(|(pending, _, _)| pending != &path);
        self.visible_sources
            .borrow_mut()
            .insert(path, sha256.to_ascii_lowercase());
    }

    fn absolute_source_path(&self, path: &Path) -> PathBuf {
        self.profile
            .check("edit", Access::Read, path)
            .unwrap_or_else(|_| {
                if path.is_absolute() {
                    path.to_path_buf()
                } else {
                    self.profile.root().join(path)
                }
            })
    }

    /// A name is captured twice in the ordinary case — once where the
    /// binding is made, once by the epilogue that reads the value it ended
    /// with — so the second capture **overwrites in place**. Removing and
    /// re-appending would order the table by the epilogue instead of by the
    /// model's own declarations, and a name the epilogue cannot read (a
    /// `class`) would then sort ahead of every name it can.
    pub(crate) fn capture(&self, name: &str, value: Value, meta: HandleMeta) {
        let mut current = self.current.borrow_mut();
        if current.freed.iter().any(|freed| freed == name) {
            return;
        }
        if let Some(existing) = current
            .captures
            .iter_mut()
            .find(|existing| existing.name == name)
        {
            existing.value = value;
            existing.meta = meta;
            return;
        }
        current.captures.push(Capture {
            name: name.to_string(),
            value,
            meta,
        });
    }

    /// A `keep` is an explicit pin: the model asked to see this value again.
    pub(crate) fn note_pin(&self, name: &str) {
        let mut current = self.current.borrow_mut();
        if !current.pinned.iter().any(|pinned| pinned == name) {
            current.pinned.push(name.to_string());
        }
    }

    pub(crate) fn note_free(&self, name: &str) {
        let mut current = self.current.borrow_mut();
        current.captures.retain(|existing| existing.name != name);
        if !current.freed.iter().any(|freed| freed == name) {
            current.freed.push(name.to_string());
        }
    }
}

/// The provenance of one call, assembled where both the tool's declaration
/// and the call's own arguments are in hand.
pub(crate) fn provenance(
    tool: &str,
    args: &crate::tools::invoke::Args,
    stdout: &str,
    pure: bool,
) -> Provenance {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(stdout.as_bytes());
    Provenance {
        tool: tool.to_string(),
        args: args
            .names()
            .map(|name| {
                (
                    name.to_string(),
                    args.get(name).unwrap_or_default().to_string(),
                )
            })
            .collect(),
        sha256: format!("{digest:x}"),
        pure,
    }
}

/// Signals an out-of-memory across V8's near-heap-limit callback, which is
/// handed a `*mut c_void` and nothing else.
///
/// [`AtomicBool`] and [`OnceLock`] rather than a `RefCell`: the callback runs
/// inside a garbage collection, where a `RefCell` this crate might already
/// have borrowed would panic.
pub(crate) struct HeapGuard {
    pub(crate) hit: AtomicBool,
    pub(crate) isolate: OnceLock<v8::IsolateHandle>,
}

impl HeapGuard {
    pub(crate) fn new() -> Rc<Self> {
        Rc::new(Self {
            hit: AtomicBool::new(false),
            isolate: OnceLock::new(),
        })
    }

    pub(crate) fn take_hit(&self) -> bool {
        self.hit.swap(false, Ordering::SeqCst)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::helpers::HelperOutcome;

    #[test]
    fn console_output_is_bounded_and_says_how_much_it_dropped() {
        let mut console = ConsoleCapture::default();
        for i in 0..2_000 {
            console.write_line(&format!("line {i} with some padding to make it wide"));
        }
        let (tail, dropped) = console.tail();
        assert!(
            preview::estimate_tokens(&tail) <= preview::STDOUT_TOKEN_CAP,
            "tail was {} tokens",
            preview::estimate_tokens(&tail)
        );
        assert!(dropped > 0, "nothing was reported dropped");
        // The tail is the *end* of the output, which is what a model needs.
        assert!(tail.contains("line 1999"), "{tail}");
        assert!(!tail.contains("line 0 "), "{tail}");
    }

    fn state() -> RuntimeState {
        RuntimeState::new(
            &Profile::compile(
                std::env::temp_dir(),
                Some(r#"{"permissions":{"allow":[]}}"#),
            ),
            &SessionId::new("progress"),
        )
    }

    #[test]
    fn agent_modes_resolve_auto_off_and_pinned_without_spawning() {
        let state = state();
        *state.model.borrow_mut() = "parent-model".into();

        state.set_agents(crate::config::AgentsConfig::default());
        assert!(state.agent_model(None).is_err());
        assert!(state.agent_model(Some("cell-model".into())).is_err());

        state.set_agents(crate::config::AgentsConfig {
            mode: crate::config::AgentsMode::Pinned,
            model: Some("pinned-model".into()),
            deadline: None,
            ..Default::default()
        });
        assert_eq!(state.agent_model(None).unwrap(), "pinned-model");
        assert!(state.agent_model(Some("cell-model".into())).is_err());

        state.set_agents(crate::config::AgentsConfig {
            mode: crate::config::AgentsMode::Off,
            model: None,
            deadline: None,
            ..Default::default()
        });
        assert!(state.agent_model(None).is_err());
        assert!(
            state.agent_model(Some("cell-model".into())).is_err(),
            "off refuses even an explicit model"
        );
    }

    fn asked(name: &str) -> HelperRecord {
        HelperRecord {
            helper: name.to_string(),
            verb: "reducing".to_string(),
            asked: "4118 lines".to_string(),
            ..HelperRecord::default()
        }
    }

    /// The lane exists to say a helper is running, so the signal must arrive
    /// **before** the answer does: one call, two reports, the first carrying a
    /// record nothing has resolved yet.
    #[test]
    fn a_helper_call_reports_its_start_and_then_its_end() {
        let seen: Rc<RefCell<Vec<Vec<HelperRecord>>>> = Rc::new(RefCell::new(Vec::new()));
        let recorder = Rc::clone(&seen);
        let previous = install_helper_progress(Some(Rc::new(move |records: &[HelperRecord]| {
            recorder.borrow_mut().push(records.to_vec());
        })));

        let state = state();
        let slot = state.begin_helper(asked("reduce"));
        state.finish_helper(
            slot,
            HelperCall {
                outcome: HelperOutcome {
                    text: "3 distinct root failures".to_string(),
                    ok: true,
                    cancelled: false,
                    elapsed_ms: 1_100,
                },
                turns: 1,
                looked: Vec::new(),
                usage: crate::helpers::HelperUsage::default(),
            },
        );
        install_helper_progress(previous);

        let seen = seen.borrow();
        assert_eq!(seen.len(), 2, "one call must report a start and an end");
        assert_eq!(seen[0].len(), 1);
        assert!(
            !seen[0][0].outcome.ok && seen[0][0].outcome.text.is_empty(),
            "the first report must carry an unresolved call: {:?}",
            seen[0][0].outcome
        );
        assert_eq!(seen[0][0].helper, "reduce");
        assert!(seen[1][0].outcome.ok, "the second report must be resolved");
        assert_eq!(seen[1][0].outcome.text, "3 distinct root failures");
        assert_eq!(state.helper_records().len(), 1, "one call, one record");
    }

    /// Every runtime built without a session -- `agent.rs`'s subagents and
    /// every test -- finds no signal, and that is the ordinary case rather
    /// than a special one.
    #[test]
    fn a_helper_call_with_no_terminal_installed_still_records() {
        let previous = install_helper_progress(None);
        let state = state();
        let slot = state.begin_helper(asked("reduce"));
        state.finish_helper(
            slot,
            HelperCall {
                outcome: HelperOutcome {
                    text: "nothing failed".to_string(),
                    ok: true,
                    cancelled: false,
                    elapsed_ms: 40,
                },
                turns: 1,
                looked: Vec::new(),
                usage: crate::helpers::HelperUsage::default(),
            },
        );
        install_helper_progress(previous);
        let records = state.helper_records();
        assert_eq!(records.len(), 1);
        assert!(records[0].outcome.ok);
    }

    #[test]
    fn short_console_output_is_kept_whole_with_nothing_dropped() {
        let mut console = ConsoleCapture::default();
        console.write_line("hello");
        let (tail, dropped) = console.tail();
        assert_eq!(tail, "hello\n");
        assert_eq!(dropped, 0);
    }
}
