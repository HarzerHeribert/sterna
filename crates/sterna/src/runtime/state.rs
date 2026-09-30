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

use crate::contract::SessionId;
use crate::project::source_context::SourceContext;
use crate::runtime::handles::{HandleMeta, HandleTable, Provenance};
use crate::runtime::instructions::{InstructionContext, PendingInstructions};
use crate::runtime::observation::ReductionStats;
use crate::runtime::outcome::SourceEvidence;
use crate::runtime::preview::{self, Value};
use crate::sandbox::profile::{Access, Profile};
use crate::tools::invoke::CancellationToken;

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
    /// `decide.choice` calls **claimed** this cell, which is what the
    /// per-cell ceiling counts. A call in flight has claimed its slot, so a
    /// loop cannot overrun the ceiling by whatever is outstanding.
    pub(crate) decisions: u32,
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
    /// The session's one list of allowed hosts, live: what `web.fetch`
    /// reaches without asking, and where a host a person allows is added.
    /// `None` is a runtime nobody gave one, whose every fetch asks.
    pub(crate) hosts: RefCell<Option<crate::sandbox::proxy::Allowed>>,
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
    /// `[limits] reduce_above_tokens`: the estimated size above which a
    /// command result is shortened by the reduction rules.
    reduce_above_tokens: std::cell::Cell<usize>,
    /// `[agents]` as the session read it: what a delegated goal runs on when
    /// the cell does not name a model.
    agents: RefCell<crate::config::AgentsConfig>,
    /// `[decisions]`, for the one question a *cell* may ask: `decide.choice`
    /// routes on `model`, and an unset model is why the global is not bound.
    decisions: RefCell<crate::config::DecisionsConfig>,
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
    /// The bytes of the latest version of each file the model has a view of
    /// -- from a delivered `context` or its own `edit` or `write` -- so a
    /// change something else makes is shown as its changed lines and the
    /// view follows it ([`Self::follow_changed_sources`]).
    known_sources: RefCell<HashMap<PathBuf, crate::runtime::rewrites::Known>>,
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

/// How many `decide.choice` questions one cell may ask. A cell is code, so a
/// question sits inside a loop, and the refusal is what the program catches
/// instead of the loop running away.
pub(crate) const DECISIONS_PER_CELL: u32 = 8;

impl RuntimeState {
    /// Stops the cell's compute clock until the guard is dropped.
    ///
    /// **Every host callback that can outlast a keystroke takes one.** The
    /// cell is not computing while a child process builds, an MCP server
    /// answers or a person decides, and the wall-clock limit is there to stop
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
            hosts: RefCell::new(None),
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
            subagent: std::cell::Cell::new(false),
            budget_remaining: std::cell::Cell::new(0),
            model: RefCell::new(crate::wire::MODEL.to_string()),
            instructions: RefCell::new(InstructionContext::default()),
            reduce_above_tokens: std::cell::Cell::new(crate::config::REDUCE_ABOVE_TOKENS_DEFAULT),
            agents: RefCell::new(crate::config::AgentsConfig::default()),
            decisions: RefCell::new(crate::config::DecisionsConfig::default()),
            visible_sources: RefCell::new(HashMap::new()),
            pending_sources: RefCell::new(Vec::new()),
            pending_incomplete: RefCell::new(Vec::new()),
            incomplete_sources: RefCell::new(HashSet::new()),
            seen_lines: RefCell::default(),
            pending_lines: RefCell::default(),
            pending_context_output: RefCell::new(Vec::new()),
            shown_contexts: RefCell::new(HashMap::new()),
            known_sources: RefCell::new(HashMap::new()),
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
        // A change made while no cell ran -- the person's editor, a job that
        // finished -- is found before this cell can trip over it, and
        // arrives with this cell's result.
        self.follow_changed_sources();
        let mut current = self.current.borrow_mut();
        current.console.clear();
        current.captures.clear();
        current.freed.clear();
        current.pinned.clear();
        current.decisions = 0;
        cell
    }

    pub(crate) fn set_reduce_above_tokens(&self, tokens: usize) {
        self.reduce_above_tokens.set(tokens);
    }

    /// `[limits] reduce_above_tokens`: the estimated size above which a
    /// command result is shortened by the reduction rules.
    pub(crate) fn reduce_above_tokens(&self) -> usize {
        self.reduce_above_tokens.get()
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

    /// Takes one of this cell's `decide.choice` slots, or says the ceiling is
    /// reached. The refusal throws into the cell rather than ending it, so
    /// the control flow stays the program's own.
    pub(crate) fn claim_decision(&self) -> Result<(), String> {
        let mut current = self.current.borrow_mut();
        if current.decisions >= DECISIONS_PER_CELL {
            return Err(format!(
                "this cell has asked its {DECISIONS_PER_CELL} decide.choice questions"
            ));
        }
        current.decisions += 1;
        Ok(())
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
        self.visible_sources.borrow_mut().clear();
        self.pending_sources.borrow_mut().clear();
        self.pending_incomplete.borrow_mut().clear();
        self.incomplete_sources.borrow_mut().clear();
        self.seen_lines.borrow_mut().clear();
        self.pending_lines.borrow_mut().clear();
        self.pending_context_output.borrow_mut().clear();
        self.shown_contexts.borrow_mut().clear();
        self.known_sources.borrow_mut().clear();
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
        self.remember_source(&path, &evidence.sha256);
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
                "`edit` did not run: the current version of `{named}` is in this turn's feedback and you have not read it yet; it binds from the next cell, so make the edit there with no new `context`"
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
    ///
    /// None while a newer version of the path waits in this turn's feedback
    /// -- a change shown with this cell, or a context of the new bytes: the
    /// file already is that version, so binding the older one could only be
    /// refused as stale, and the refusal would send the model to read again
    /// what it is about to be shown.
    pub(crate) fn visible_source_hash(&self, args: &crate::tools::invoke::Args) -> Option<String> {
        let path = self.absolute_source_path(Path::new(args.get("path")?));
        let visible = self.visible_sources.borrow().get(&path).cloned()?;
        let superseded = self
            .pending_sources
            .borrow()
            .iter()
            .any(|(pending, sha)| pending == &path && !sha.eq_ignore_ascii_case(&visible));
        (!superseded).then_some(visible)
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
        self.remember_source(&path, sha256);
        self.visible_sources
            .borrow_mut()
            .insert(path, sha256.to_ascii_lowercase());
    }

    /// Records the bytes of the version of `path` the model now has, when
    /// the file on disk still is that version. One that already moved on is
    /// forgotten: the model's view of it is stale, and an `edit` says so.
    fn remember_source(&self, path: &Path, sha256: &str) {
        match crate::runtime::rewrites::Known::read(path) {
            Some(known) if known.sha256.eq_ignore_ascii_case(sha256) => {
                self.known_sources
                    .borrow_mut()
                    .insert(path.to_path_buf(), known);
            }
            _ => {
                self.known_sources.borrow_mut().remove(path);
            }
        }
    }

    /// Every change to a file the model has a view of that did not come
    /// from its own `edit` or `write` -- a formatter it ran, a background
    /// job, the person's editor -- delivered with this cell's result, and
    /// the view moved to follow it.
    ///
    /// Run at the end of every cell, before the cell's source context is
    /// written, so a change arrives with the cell whose command made it. A
    /// change small enough to show is shown line by line, and then an
    /// `edit` binds to the new version with no new `context`: the version
    /// the model edits against moves when it had the old one whole, and
    /// the lines it has seen are renumbered through the change. One too
    /// large to show is named in one line and the view stays where it was,
    /// so an `edit` is refused as stale and says to read it again.
    pub(crate) fn follow_changed_sources(&self) {
        use crate::runtime::rewrites::{self, Known};
        const HEADING: &str = "## Changed on disk since you read it\n\
            Every changed line is below, and the version you edit against follows it: \
            no new `context` is needed.\n";
        let known: Vec<(PathBuf, Known)> = self
            .known_sources
            .borrow()
            .iter()
            .map(|(path, known)| (path.clone(), known.clone()))
            .collect();
        let mut shown = String::new();
        for (path, before) in known {
            if before.looks_unchanged(&path) {
                continue;
            }
            let label = path
                .strip_prefix(self.profile.root())
                .unwrap_or(&path)
                .display()
                .to_string();
            let Some(now) = Known::read(&path) else {
                self.known_sources.borrow_mut().remove(&path);
                self.seen_lines.borrow_mut().remove(&path);
                shown.push_str(&format!("### {label}: deleted, or no longer text\n"));
                continue;
            };
            if now.sha256 == before.sha256 {
                self.known_sources.borrow_mut().insert(path, now);
                continue;
            }
            let room = self
                .remaining_context_budget()
                .saturating_sub(HEADING.len() + shown.chars().count());
            let versions = format!("{} -> {}", &before.sha256[..12], &now.sha256[..12]);
            let too_many = |changed: String| {
                format!(
                    "### {label} ({versions}): {changed} lines changed, too many to show here; \
                     call `context` for what you need before editing it\n"
                )
            };
            match rewrites::delta(&before.text, &now.text, rewrites::MAX_CHANGES) {
                Some(delta) => {
                    let part = if delta.changed == 0 {
                        format!(
                            "### {label} ({versions}): line endings or the final newline changed; no line's text did\n"
                        )
                    } else {
                        format!("### {label} ({versions})\n{}", delta.rendered)
                    };
                    if part.chars().count() <= room {
                        shown.push_str(&part);
                        self.follow_source(&path, &before, now, &delta);
                        continue;
                    }
                    shown.push_str(&too_many(delta.changed.to_string()));
                }
                None => shown.push_str(&too_many(format!("more than {}", rewrites::MAX_CHANGES))),
            }
            // Not followed: the line numbers no longer hold, the version the
            // model read stays the one an `edit` is checked against, and the
            // new bytes are recorded so the change is reported once.
            self.seen_lines.borrow_mut().remove(&path);
            self.pending_lines
                .borrow_mut()
                .retain(|(pending, _, _)| pending != &path);
            self.known_sources.borrow_mut().insert(path, now);
        }
        if !shown.is_empty() {
            self.pending_context_output
                .borrow_mut()
                .push(format!("{HEADING}{shown}"));
        }
    }

    /// Moves the model's view of `path` from `before` to `now` through a
    /// change it is being shown whole.
    fn follow_source(
        &self,
        path: &Path,
        before: &crate::runtime::rewrites::Known,
        now: crate::runtime::rewrites::Known,
        delta: &crate::runtime::rewrites::Delta,
    ) {
        let had_whole =
            self.visible_sources
                .borrow()
                .get(path)
                .is_some_and(|sha| sha.eq_ignore_ascii_case(&before.sha256))
                || self.pending_sources.borrow().iter().any(|(pending, sha)| {
                    pending == path && sha.eq_ignore_ascii_case(&before.sha256)
                });
        if had_whole {
            self.pending_sources
                .borrow_mut()
                .push((path.to_path_buf(), now.sha256.clone()));
        }
        let moved = |line: usize| {
            line.checked_sub(1)
                .and_then(|index| delta.map.get(index).copied().flatten())
                .map(|index| index + 1)
        };
        if let Some(seen) = self.seen_lines.borrow_mut().get_mut(path) {
            *seen = std::mem::take(seen)
                .into_iter()
                .filter_map(|(line, text)| moved(line).map(|line| (line, text)))
                .collect();
        }
        let mut pending = self.pending_lines.borrow_mut();
        pending.retain_mut(|(pending, line, _)| {
            pending != path
                || match moved(*line) {
                    Some(to) => {
                        *line = to;
                        true
                    }
                    None => false,
                }
        });
        for (line, text) in &delta.added {
            pending.push((path.to_path_buf(), line + 1, text.clone()));
        }
        drop(pending);
        self.known_sources
            .borrow_mut()
            .insert(path.to_path_buf(), now);
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

    /// A cell asks `decide.choice` at most [`DECISIONS_PER_CELL`] times, and
    /// the next cell starts with the full allowance again.
    #[test]
    fn a_cell_asks_decide_choice_a_bounded_number_of_times() {
        let state = state();
        for _ in 0..DECISIONS_PER_CELL {
            assert!(state.claim_decision().is_ok());
        }
        assert!(state.claim_decision().is_err());
        state.begin_cell();
        assert!(state.claim_decision().is_ok());
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
