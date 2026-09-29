//! The only authority a cell has — map line 2463's in-program half.
//!
//! **Every one of these functions is the isolate's whole outside world.** The
//! registered tools go through [`crate::tools::invoke::run`] and therefore through
//! the merged sandbox; `keep`, `free` and `handles` touch nothing but the
//! handle table; `console` writes to a bounded buffer. There is no file, no
//! socket, no timer, no `require` and no dynamic import here, because a V8
//! context has none of those unless an embedder adds them and this module is
//! the only place that adds anything. `yieldNow` is the eighth function and
//! the one that runs no code: it stops the cell (`runtime-contract.md` §9.3).
//!
//! A refusal is a throw of `PermissionDenied` inside the model's own program
//! (`sandbox-grants.md` §1.4 and §5): catchable, final, and never a prompt.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use crate::bg::{self, RunOptions, WatchOptions};
use crate::events::batch::Batch;
use crate::events::{BatchStore, Event, EventId};
use crate::runtime::cell::{RESERVED_PREFIX, is_host_function};
use crate::runtime::excerpt::{self, SampledLine};
use crate::runtime::handles::HandleMeta;
use crate::runtime::isolate::DEFAULT_RESPONSE_BYTE_CAP;
use crate::runtime::marshal;
use crate::runtime::outcome::{
    CallRecord, Ended, PlanItem, PlanStatus, SourceEvidence, SourceRange,
};
use crate::runtime::preview::{
    ArrayValue, FileValue, PREVIEW_TOKEN_CAP, StringValue, Value, thousands,
};
use crate::runtime::state::{RecordedCall, RuntimeState, provenance};
use crate::sandbox::profile::PermissionDenied;
use crate::tools::invoke::{self, Args, ToolContext, ToolError, ToolResult};
use crate::tools::registry::{self, Tool};

mod agent;
mod ask;
mod console;
mod decide;
pub(super) mod helper;
use helper::helper_callback;
mod search;
use search::{build_glob, build_grep};
mod web;
use console::console_callback;
pub(crate) use decide::install_decide;
pub(crate) use web::install_web;

/// Declared once so the classes a refusal is thrown as exist before any cell
/// runs, and so the one JIT surface a code-over-objects runtime has no use
/// for is gone.
pub(crate) const BOOTSTRAP: &str = r#"
globalThis.PermissionDenied = class PermissionDenied extends Error {
  constructor(message, tool, path, rule) {
    super(message);
    this.name = "PermissionDenied";
    this.tool = tool;
    this.path = path;
    this.rule = rule;
  }
};
globalThis.ToolError = class ToolError extends Error {
  constructor(message) { super(message); this.name = "ToolError"; }
};
globalThis.Cancelled = class Cancelled extends Error {
  constructor(message, tool) { super(message); this.name = "Cancelled"; this.tool = tool; }
};
delete globalThis.WebAssembly;
delete globalThis.SharedArrayBuffer;
delete globalThis.Atomics;
"#;

/// The private symbol a tool-produced object is tagged with, so the binding
/// that ends up holding it can be given that call's provenance and its
/// already-built preview instead of being marshalled a second time.
///
/// A *private* symbol: invisible to `Object.keys`,
/// `Object.getOwnPropertySymbols` and `JSON.stringify`, so tagging a result
/// cannot change what a model's own program sees of it.
fn call_tag<'s>(scope: &mut v8::PinScope<'s, '_>) -> v8::Local<'s, v8::Private> {
    let name = v8::String::new(scope, "sterna.call").expect("a short literal is a valid string");
    v8::Private::for_api(scope, Some(name))
}

/// The private symbol the value `e()` answers with is tagged with, carrying
/// the number of the cell that minted it.
///
/// **This is what makes `runtime-contract.md` §1's two endings decidable
/// rather than declarable.** A cell yields when its promise fulfils with a
/// value carrying this tag for *this* cell, and returns otherwise, so a
/// program that calls `e()` itself and then returns something of its own
/// still returns: the marker is a value only the host can mint, and a marker
/// kept from an earlier cell does not answer for a later one. `v8::Private`
/// is API-only — there is no JavaScript operation that sets one — which is
/// the same property the preview tag above relies on.
///
/// **Only the host can mint it, and the host mints it on demand** for the
/// cell's own `__sterna_cell.e()`, so `return __sterna_cell.e()` yields by
/// construction — §1's one counterexample to "a top-level `return` ends the
/// task", reachable only by naming an internal the model is never shown, and
/// gaining no authority when it is.
fn end_tag<'s>(scope: &mut v8::PinScope<'s, '_>) -> v8::Local<'s, v8::Private> {
    let name = v8::String::new(scope, "sterna.end").expect("a short literal is a valid string");
    v8::Private::for_api(scope, Some(name))
}

/// Whether `value` is the marker [`fell_callback`] minted for cell `cell`.
pub(crate) fn is_end_marker(
    scope: &mut v8::PinScope,
    value: v8::Local<v8::Value>,
    cell: u64,
) -> bool {
    let Ok(object) = v8::Local::<v8::Object>::try_from(value) else {
        return false;
    };
    let tag = end_tag(scope);
    object
        .get_private(scope, tag)
        .and_then(|marker| marker.number_value(scope))
        .is_some_and(|minted| minted == cell as f64)
}

pub(crate) fn state(scope: &v8::PinScope) -> Rc<RuntimeState> {
    scope
        .get_slot::<Rc<RuntimeState>>()
        .expect("the runtime installs its state before any callback can run")
        .clone()
}

/// What the host keeps about a cell's ending beyond [`RuntimeState`]:
/// §9.4's trajectory, §9.3's yield request, and §9.2's response cap. One per
/// runtime, in the isolate's second slot; the trajectory and the request are
/// cleared by the isolate at the start of every cell and consumed when the
/// cell ends, and the cap is read when a cell returns a string.
pub(crate) struct CellTrace {
    calls: RefCell<Vec<CallRecord>>,
    yield_requested: Cell<bool>,
    yield_reason: RefCell<Option<String>>,
    /// What `answer(text)` said, when the cell said it. **The only way a
    /// cell ends the task**, so that ending is something a program states
    /// and never something the shape of a value implies.
    answer: RefCell<Option<String>>,
    /// The question `ask` put to the person, when the cell asked one. At most
    /// one per cell: `ask` ends the cell, so a second call cannot be reached.
    ask: RefCell<Option<crate::ask::Question>>,
    pub(crate) response_byte_cap: Cell<usize>,
    /// Whether this frame's capability results are being captured for a
    /// provider `tool_result`. Off for an authored cell, which pays nothing.
    capture_results: Cell<bool>,
    results: RefCell<Vec<String>>,
}

impl CellTrace {
    pub(crate) fn new() -> Rc<Self> {
        Rc::new(Self {
            calls: RefCell::new(Vec::new()),
            yield_requested: Cell::new(false),
            yield_reason: RefCell::new(None),
            answer: RefCell::new(None),
            ask: RefCell::new(None),
            response_byte_cap: Cell::new(DEFAULT_RESPONSE_BYTE_CAP),
            capture_results: Cell::new(false),
            results: RefCell::new(Vec::new()),
        })
    }

    pub(crate) fn begin_cell(&self) {
        self.calls.borrow_mut().clear();
        self.yield_requested.set(false);
        self.yield_reason.borrow_mut().take();
        self.answer.borrow_mut().take();
        self.ask.borrow_mut().take();
        self.results.borrow_mut().clear();
    }

    /// Turns capture on for the frame about to run. Scoped to that frame:
    /// [`Self::take_results`] drains it and the next `begin_cell` clears
    /// whatever a failed frame left behind.
    pub(crate) fn capture_results(&self, on: bool) {
        self.capture_results.set(on);
    }

    pub(crate) fn captures_results(&self) -> bool {
        self.capture_results.get()
    }

    pub(crate) fn record_result(&self, json: String) {
        self.results.borrow_mut().push(json);
    }

    /// This frame's typed results, in call order, taken once.
    pub(crate) fn take_results(&self) -> Vec<String> {
        std::mem::take(&mut *self.results.borrow_mut())
    }

    /// Every call that ran this cell, in order, taken once.
    pub(crate) fn take_calls(&self) -> Vec<CallRecord> {
        std::mem::take(&mut *self.calls.borrow_mut())
    }

    /// `Some(reason)` once per cell when `yieldNow` was called, with the
    /// reason it gave; `None` when it was not.
    pub(crate) fn take_yield(&self) -> Option<Option<String>> {
        if !self.yield_requested.replace(false) {
            return None;
        }
        Some(self.yield_reason.borrow_mut().take())
    }

    /// What this cell answered, taken once. `None` when it did not answer,
    /// which is every cell that was still working.
    pub(crate) fn take_answer(&self) -> Option<String> {
        self.answer.borrow_mut().take()
    }

    fn record_answer(&self, text: String) {
        *self.answer.borrow_mut() = Some(text);
    }

    /// The question this cell asked, taken once.
    pub(crate) fn take_ask(&self) -> Option<crate::ask::Question> {
        self.ask.borrow_mut().take()
    }

    fn record_ask(&self, question: crate::ask::Question) {
        *self.ask.borrow_mut() = Some(question);
    }

    pub(crate) fn record(&self, call: CallRecord) {
        self.calls.borrow_mut().push(call);
    }

    fn request_yield(&self, reason: Option<String>) {
        self.yield_requested.set(true);
        *self.yield_reason.borrow_mut() = reason;
    }
}

/// The instruction gate stopped a call: the cell ends here with a reason that
/// names the scope and says what did not happen (`InstructionContext::stop_reason`).
fn stop_for_instructions(scope: &mut v8::PinScope, before: &str, not_done: &str) {
    let reason = state(scope).instruction_stop_reason(before, not_done);
    trace(scope).request_yield(Some(reason));
    scope.terminate_execution();
}

fn trace(scope: &v8::PinScope) -> Rc<CellTrace> {
    scope
        .get_slot::<Rc<CellTrace>>()
        .expect("the runtime installs its trace before any callback can run")
        .clone()
}

fn js_string<'s>(scope: &mut v8::PinScope<'s, '_>, text: &str) -> v8::Local<'s, v8::Value> {
    v8::String::new(scope, text).map_or_else(|| v8::undefined(scope).into(), |string| string.into())
}

fn set_key(
    scope: &mut v8::PinScope,
    object: v8::Local<v8::Object>,
    key: &str,
    value: v8::Local<v8::Value>,
) {
    if let Some(key) = v8::String::new(scope, key) {
        object.set(scope, key.into(), value);
    }
}

/// The attributes every host function is installed with: not writable and
/// not deletable, so `free("grep")`, `grep = 1`, `delete globalThis.grep` and
/// `Object.defineProperty(globalThis, "grep", …)` all fail rather than
/// costing the task a tool it cannot get back. Not configurable is what
/// closes the `defineProperty` door: redefining a non-configurable property
/// is a `TypeError` by the language, not by a check this crate remembers to
/// make.
fn host_attributes() -> v8::PropertyAttribute {
    v8::PropertyAttribute::READ_ONLY | v8::PropertyAttribute::DONT_DELETE
}

/// [`set_key`] for something a program may not replace.
fn set_fixed_key(
    scope: &mut v8::PinScope,
    object: v8::Local<v8::Object>,
    key: &str,
    value: v8::Local<v8::Value>,
) {
    if let Some(key) = v8::String::new(scope, key) {
        object.define_own_property(scope, key.into(), value, host_attributes());
    }
}

/// Which host globals a context is given.
///
/// The invariant: **a helper's context holds only what its spec named, and
/// nothing that can cause an effect.** `little-helpers.md` makes the toolset
/// the safety boundary, which is true of the registered tools and false of
/// the host globals installed beside them — `bg.run` executes a command,
/// `send` messages another session, `mcp.call` reaches a server, and not one
/// of the three is in `registry::ALL`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostGlobals {
    /// Every global: an ordinary cell, and an ordinary subagent.
    Every,
    /// A helper's: exactly the tools its spec named, and no global that can
    /// cause an effect. The slice is the spec's own `tools`, so
    /// `little-helpers.md`'s claim — *a Scout cannot write because `write` is
    /// not in its registry subset* — is true of the binding and not only of
    /// the validator.
    Helper(&'static [&'static str]),
}

impl HostGlobals {
    /// The globals a helper never holds, whatever else it was given.
    /// `helper` and `agent` are withheld for a different reason than the
    /// other four, and it is a lifetime one: `helpers-and-subagents.md` §11
    /// forbids the `Helper -> Helper` and `Helper -> Subagent` edges, and §19
    /// forbids a detached helper. `agent.run` was already refused to a helper
    /// by the subagent depth check, but a capability absent from the binding
    /// surface cannot be reached by a helper talked into trying, which is the
    /// standard `little-helpers.md` sets for the toolset.
    pub const WITHHELD_FROM_A_HELPER: [&'static str; 7] =
        ["bg", "mcp", "checks", "helper", "agent", "web", "decide"];

    /// Whether `global` is installed under this narrowing — the one predicate
    /// [`install`] and [`crate::prompt::render_runtime_for`] both read, so
    /// the block the model is shown cannot name a global its context lacks.
    #[must_use]
    pub fn installs(self, global: &str) -> bool {
        match self {
            Self::Every => true,
            Self::Helper(_) => !Self::WITHHELD_FROM_A_HELPER.contains(&global),
        }
    }

    /// [`Self::installs`] with the one global whose existence the session's
    /// configuration decides: `web` is bound and declared only when
    /// `[web]` names a domain or an endpoint (map 2658), so an unconfigured
    /// session holds no `web` and is told of none. The predicate
    /// [`install_web`] binds on and [`crate::prompt::render_runtime_reaching`]
    /// declares on.
    #[must_use]
    pub fn installs_with(self, global: &str, web_configured: bool) -> bool {
        self.installs_reaching(global, web_configured, true)
    }

    /// [`Self::installs`] with **every** global whose existence a session's
    /// configuration decides: `web` on `[web]` naming a domain or an endpoint,
    /// and `decide` on `[decisions]` naming a model. One predicate so the
    /// binding ([`install_decide`]) and the declaration cannot disagree about
    /// whether a global exists.
    ///
    /// [`Self::installs_with`] answers `true` for the decision half, which is
    /// what the Runtime block does today: a session with no `[decisions]
    /// model` binds no `decide` and is still told of it. Passing the real
    /// flag from `prompt::Reach` closes that, and is the one line left.
    #[must_use]
    pub fn installs_reaching(
        self,
        global: &str,
        web_configured: bool,
        decisions_configured: bool,
    ) -> bool {
        self.installs(global)
            && (global != "web" || web_configured)
            && (global != "decide" || decisions_configured)
    }

    /// Whether a **registered tool** is bound under this narrowing.
    ///
    /// Separate from [`Self::installs`] because a tool is admitted by the
    /// spec's own list while a host global is admitted by absence from
    /// [`Self::WITHHELD_FROM_A_HELPER`]. A helper that named no tool binds
    /// none, which is why `REDUCER` can reach nothing at all.
    #[must_use]
    pub fn binds_tool(self, tool: &str) -> bool {
        match self {
            Self::Every => true,
            Self::Helper(tools) => tools.contains(&tool),
        }
    }
}

/// Installs the host functions `globals` admits on the context's global
/// object.
pub(crate) fn install(scope: &mut v8::PinScope, globals: HostGlobals) {
    let context = scope.get_current_context();
    let global = context.global(scope);

    for tool in registry::ALL {
        let name = tool.name();
        if !globals.binds_tool(name) {
            continue;
        }
        let data = js_string(scope, name);
        let Some(function) = v8::Function::builder(tool_callback).data(data).build(scope) else {
            continue;
        };
        set_fixed_key(scope, global, name, function.into());
    }

    // The dialect spellings, bound to the same `tool_callback` with the same
    // registry name in their data slot — `tool-abi.md` §14. `Read` is not a
    // wrapper around `read`; the two names reach one callback, so acceptance
    // criterion 20's semantic equivalence is structural. The gate is asked of
    // the **registry** name, so an alias can never carry a capability a
    // helper's spec withheld.
    for (alias, tool) in crate::abi::dialect::tool_aliases() {
        if !globals.binds_tool(tool) {
            continue;
        }
        // The data slot carries the **alias**, not the registry name: it is
        // how the one callback learns which spelling it was reached by, and
        // therefore which parameter names to rename. Binding the registry
        // name here instead compiles, dispatches to the right capability,
        // and silently skips the rename.
        let data = js_string(scope, alias);
        let Some(function) = v8::Function::builder(tool_callback).data(data).build(scope) else {
            continue;
        };
        set_fixed_key(scope, global, alias, function.into());
    }

    if let Some(function) = v8::Function::builder(on_callback).build(scope) {
        set_fixed_key(scope, global, "on", function.into());
    }
    if let Some(function) = v8::Function::builder(off_callback).build(scope) {
        set_fixed_key(scope, global, "off", function.into());
    }

    // `web` is not bound here: whether it exists is the configuration's
    // decision, made when the broker arrives — see [`install_web`].
    // `decide` is not bound here either, for the same reason: `[decisions]
    // model` decides whether it exists — see [`install_decide`].

    if globals.installs("mcp") {
        let mcp = v8::Object::new(scope);
        if let Some(function) = v8::Function::builder(mcp_list_callback).build(scope) {
            set_fixed_key(scope, mcp, "list", function.into());
        }
        if let Some(function) = v8::Function::builder(mcp_call_callback).build(scope) {
            set_fixed_key(scope, mcp, "call", function.into());
        }
        set_fixed_key(scope, global, "mcp", mcp.into());
    }

    if let Some(function) = v8::Function::builder(keep_callback).build(scope) {
        set_fixed_key(scope, global, "keep", function.into());
    }
    if let Some(function) = v8::Function::builder(free_callback).build(scope) {
        set_fixed_key(scope, global, "free", function.into());
    }
    if let Some(function) = v8::Function::builder(handles_callback).build(scope) {
        set_fixed_key(scope, global, "handles", function.into());
    }
    if let Some(function) = v8::Function::builder(yield_now_callback).build(scope) {
        set_fixed_key(scope, global, "yieldNow", function.into());
    }
    if let Some(function) = v8::Function::builder(answer_callback).build(scope) {
        set_fixed_key(scope, global, "answer", function.into());
    }
    ask::install_ask(scope);

    // `events-contract.md` §5's three background-job entry points, on one
    // fixed object for the same reason every host function above is fixed: a
    // program that replaced `bg` would lose the only way it has to stop what
    // it started, and nothing could put it back.
    if globals.installs("bg") {
        let background = v8::Object::new(scope);
        if let Some(function) = v8::Function::builder(bg_run_callback).build(scope) {
            set_fixed_key(scope, background, "run", function.into());
        }
        if let Some(function) = v8::Function::builder(bg_watch_callback).build(scope) {
            set_fixed_key(scope, background, "watch", function.into());
        }
        if let Some(function) = v8::Function::builder(bg_cancel_callback).build(scope) {
            set_fixed_key(scope, background, "cancel", function.into());
        }
        set_fixed_key(scope, global, "bg", background.into());
    }

    if globals.installs("checks") {
        crate::runtime::checks::install(scope, global);
        crate::runtime::checks::install_aliases(scope, global);
    }

    // Little helpers (`little-helpers.md`, *Pulled*), installed from the
    // roster so appending a `HelperSpec` is the whole of adding a helper, and
    // only where `call_sites` says a cell may reach it.
    //
    // One shared `fn` item routed by the name in its own data slot, exactly
    // as the tools above are: `v8::Function::builder` takes a `fn` item, so a
    // closure per entry built in a loop coerces to a fn pointer and fails
    // inside the v8 crate, naming none of this code.
    let helper = v8::Object::new(scope);
    let install_helpers = globals.installs("helper");
    for spec in crate::helpers::HELPERS {
        if !install_helpers {
            break;
        }
        if !crate::prompt::declarations::callable_from_a_cell(spec) {
            continue;
        }
        let data = js_string(scope, spec.name);
        let Some(function) = v8::Function::builder(helper_callback)
            .data(data)
            .build(scope)
        else {
            continue;
        };
        set_fixed_key(scope, helper, spec.name, function.into());
    }
    if install_helpers {
        set_fixed_key(scope, global, "helper", helper.into());
    }

    // Subagents. Fixed for the same reason as `bg`: a program that replaced
    // `agent` could not stop what it started.
    if globals.installs("agent") {
        let agent = v8::Object::new(scope);
        if let Some(function) = v8::Function::builder(agent_run_callback).build(scope) {
            set_fixed_key(scope, agent, "run", function.into());
        }
        set_fixed_key(scope, global, "agent", agent.into());
    }

    // The model's own plan. Fixed like every other host object: a program
    // that replaced `todo` would leave the screen showing a checklist nothing
    // could update.
    let todo = v8::Object::new(scope);
    if let Some(function) = v8::Function::builder(todo_write_callback).build(scope) {
        set_fixed_key(scope, todo, "write", function.into());
    }
    if let Some(function) = v8::Function::builder(todo_read_callback).build(scope) {
        set_fixed_key(scope, todo, "read", function.into());
    }
    set_fixed_key(scope, global, "todo", todo.into());

    let console = v8::Object::new(scope);
    for method in ["log", "info", "warn", "error", "debug", "trace"] {
        if let Some(function) = v8::Function::builder(console_callback).build(scope) {
            set_key(scope, console, method, function.into());
        }
    }
    set_key(scope, global, "console", console.into());
    // The declared API is usable before the first event arrives. No handle
    // preview or event store is allocated until an actual batch is delivered.
    install_batch(scope);
}

/// The per-cell host object the generated wrapper takes as its parameter:
/// `s` captures a completed top-level binding, `e` records that the body ran
/// off its end. It is a parameter rather than a global so it is unreachable
/// the moment the cell's function returns.
pub(crate) fn host_object<'s>(scope: &mut v8::PinScope<'s, '_>) -> v8::Local<'s, v8::Object> {
    let object = v8::Object::new(scope);
    // Fixed for the same reason the globals are: a program that replaced `e`
    // would decide its own cell's ending, and one that replaced `s` would
    // decide what the table says it bound.
    if let Some(function) = v8::Function::builder(capture_callback).build(scope) {
        set_fixed_key(scope, object, "s", function.into());
    }
    if let Some(function) = v8::Function::builder(fell_callback).build(scope) {
        set_fixed_key(scope, object, "e", function.into());
    }
    object
}

// --- registered tools -------------------------------------------------

fn mcp_list_callback(
    scope: &mut v8::PinScope,
    _args: v8::FunctionCallbackArguments,
    mut retval: v8::ReturnValue,
) {
    let state = state(scope);
    // MCP executables are opaque, like bash: deliver applicable guidance
    // before the first discovery can start project code.
    if state.instruction_boundary("bash", &Args::new()) {
        // **A rule the harness could simply state does not cost a cell.**
        // Terminating here destroyed the program's control flow, its pending
        // `Promise.all` siblings and every local it had computed, to deliver a
        // sentence. The guarantee is only that this call does not run before the
        // instructions are read, and a throw the program can catch gives exactly
        // that -- while the gate stays closed for every later call in the cell,
        // so nothing slips past it. The text still reaches the model at the turn
        // boundary through `pending_instructions`, unchanged.
        stop_for_instructions(scope, "MCP discovery", "no server started");
        return;
    }
    let context = ToolContext {
        profile: &state.profile,
        session: &state.session,
    };
    let result = invoke::list_mcp(&context, &state.token.borrow(), &mut state.mcp.borrow_mut());
    record_mcp_call(scope, "mcp.list", &result);
    match result {
        Ok(descriptors) => {
            let json =
                serde_json::to_value(descriptors).expect("MCP descriptors contain JSON values");
            let value = json_to_v8(scope, &json);
            tag_mcp_result(scope, &state, "mcp.list", value, &json.to_string());
            retval.set(value);
        }
        Err(ToolError::Denied(denied)) => throw_denied(scope, &denied),
        Err(ToolError::Cancelled { tool }) => throw_cancelled(scope, &tool),
        Err(error) => throw_tool_error(scope, &error.to_string()),
    }
}

fn mcp_call_callback(
    scope: &mut v8::PinScope,
    args: v8::FunctionCallbackArguments,
    mut retval: v8::ReturnValue,
) {
    if !args.get(0).is_string() || !args.get(1).is_object() {
        throw_tool_error(
            scope,
            "mcp.call requires a discovered name and a JSON arguments object",
        );
        return;
    }
    let name = args.get(0).to_rust_string_lossy(scope);
    let Some(arguments) = v8::json::stringify(scope, args.get(1)) else {
        return;
    };
    let Ok(arguments) = serde_json::from_str(&arguments.to_rust_string_lossy(scope)) else {
        throw_tool_error(scope, "MCP arguments must be JSON");
        return;
    };
    let state = state(scope);
    if state.instruction_boundary("bash", &Args::new()) {
        stop_for_instructions(scope, "an MCP call", "the call did not run");
        return;
    }
    let context = ToolContext {
        profile: &state.profile,
        session: &state.session,
    };
    let result = {
        // An MCP server answering is the cell waiting, not the cell
        // computing (`RuntimeState::away_from_js`); the call has its own
        // timeout.
        let _away = state.away_from_js();
        invoke::run_mcp(
            &context,
            &state.token.borrow(),
            &mut state.mcp.borrow_mut(),
            &name,
            arguments,
        )
    };
    record_mcp_call(scope, "mcp.call", &result);
    match result {
        Ok(result) => {
            let (value, _) = build_structured_result(scope, &result);
            tag_mcp_result(scope, &state, &name, value, &result.stdout);
            retval.set(value);
        }
        Err(ToolError::Denied(denied)) => throw_denied(scope, &denied),
        Err(ToolError::Cancelled { tool }) => throw_cancelled(scope, &tool),
        Err(error) => throw_tool_error(scope, &error.to_string()),
    }
}

fn record_mcp_call<T>(scope: &mut v8::PinScope, name: &str, result: &Result<T, ToolError>) {
    let ended = match result {
        Ok(_) => Ended::Ok,
        Err(ToolError::Denied(d)) => Ended::Denied {
            rule: d.rule.clone(),
        },
        Err(ToolError::Cancelled { .. }) => Ended::Threw {
            class: "Cancelled".into(),
        },
        Err(_) => Ended::Threw {
            class: "ToolError".into(),
        },
    };
    // MCP arguments may contain credentials; the trajectory records status
    // without copying arbitrary argument values.
    trace(scope).record(CallRecord {
        tool: name.into(),
        args: std::collections::BTreeMap::new(),
        evidence: None,
        lifted_from: None,
        exit_code: None,
        repeat_of: None,
        error: result.as_ref().err().map(ToString::to_string),
        ended,
    });
}

fn tag_mcp_result(
    scope: &mut v8::PinScope,
    state: &Rc<RuntimeState>,
    name: &str,
    value: v8::Local<v8::Value>,
    payload: &str,
) {
    let preview = marshal::marshal(scope, value);
    let meta = HandleMeta {
        type_label: Some(
            if name.starts_with("web.") {
                "Web.Result"
            } else if name == "mcp.list" {
                "MCP.Tool[]"
            } else {
                "MCP.Result"
            }
            .into(),
        ),
        size_estimate: payload.len() as u64,
        provenance: Some(provenance(name, &Args::new(), payload, false)),
    };
    let id = state.record_call(RecordedCall { preview, meta });
    if let Ok(object) = v8::Local::<v8::Object>::try_from(value) {
        let tag = call_tag(scope);
        let marker = v8::Number::new(scope, id as f64);
        object.set_private(scope, tag, marker.into());
    }
}

/// Takes an exact line range from a capability's observed output.
///
/// `head` and `tail` are ranges, not summaries, so the result stays an exact
/// subset of what sterna actually read. Nothing is reworded, reordered or
/// elided within the range.
fn project(
    result: invoke::ToolResult,
    projection: crate::abi::lift::Projection,
) -> invoke::ToolResult {
    let lines: Vec<&str> = result.stdout.lines().collect();
    let kept: Vec<&str> = match projection {
        crate::abi::lift::Projection::Head(n) => lines.iter().take(n).copied().collect(),
        crate::abi::lift::Projection::Tail(n) => {
            let start = lines.len().saturating_sub(n);
            lines[start..].to_vec()
        }
    };
    let mut stdout = kept.join("\n");
    if !stdout.is_empty() && result.stdout.ends_with('\n') {
        stdout.push('\n');
    }
    invoke::ToolResult { stdout, ..result }
}

fn tool_callback(
    scope: &mut v8::PinScope,
    args: v8::FunctionCallbackArguments,
    mut retval: v8::ReturnValue,
) {
    let bound_name = args.data().to_rust_string_lossy(scope);
    // A dialect spelling resolves to its own capability and its own
    // parameter names, so `Read({file_path})` in a cell is the call
    // `Read({file_path})` is as a direct tool — `tool-abi.md` §14. The
    // rename below is the entire difference between the two spellings; the
    // dispatch beneath it is one path.
    let shape = crate::abi::dialect::lookup_any(&bound_name);
    let name = shape.map_or(bound_name.clone(), |shape| {
        shape.target.callee().to_string()
    });
    let Some(requested_tool) = registry::lookup(&name) else {
        // Unreachable through `install`, which binds only registered names,
        // and a refusal rather than a panic if it ever is reached.
        throw_denied(
            scope,
            &PermissionDenied {
                tool: name.clone(),
                path: String::new(),
                rule: format!("no tool named `{name}` is registered"),
            },
        );
        return;
    };

    // `bash("npm test")`, `read("src/a.rs")`: a bare string is the tool's
    // first argument, the way a model most often writes the call. Refusing
    // it cost a whole turn to learn the object spelling.
    let positional = requested_tool
        .args()
        .first()
        .filter(|_| shape.is_none() && args.get(0).is_string())
        .map(|arg| Args::new().with(arg.name(), args.get(0).to_rust_string_lossy(scope)));
    let mut call_args = match positional.map_or_else(|| read_arguments(scope, args.get(0)), Ok) {
        Ok(call_args) => call_args,
        Err(refusal) => {
            throw_tool_error(scope, &refusal);
            return;
        }
    };
    if let Some(shape) = shape {
        for param in shape.params {
            call_args = call_args.rename(param.provider, param.canonical);
        }
    }
    let state = state(scope);

    // Semantic command lifting (`semantic-command-lifting.md`). A command
    // whose meaning sterna can prove runs the stronger capability underneath
    // the shape the model wrote.
    //
    // The admission check comes first and is the safety rule: `bash` is
    // admitted by `admits_command` while `read` and `grep` are admitted by a
    // path check, and `sandbox-grants.md` §2 forbids conflating the two. A
    // lift that skipped it would let a project that refuses `rg` get search
    // anyway. So a lift only ever *substitutes* for a command the shell would
    // itself have run; anything else falls through to the ordinary path.
    let mut lifted_from: Option<String> = None;
    let mut projection: Option<crate::abi::lift::Projection> = None;
    let mut requested_tool = requested_tool;
    // What the result is marshalled as. A lift changes which capability runs;
    // it must not change the shape the caller was promised. `shell` promises a
    // process result, so a lifted `cat` still answers with `stdout` and
    // `exit_code` — the bytes are identical, because the capability runs the
    // same program. Returning a `File` instead would silently break every
    // caller that reads `result.stdout`.
    let marshal_as = requested_tool;
    if requested_tool.name() == "bash"
        && let Some(command) = call_args.get("command")
        && state.profile.admits_command(command).is_ok()
        && let Some(lift) = crate::abi::lift::recognize(command)
        && let Some(target) = registry::lookup(lift.capability)
    {
        let mut lifted = Args::new();
        for (name, value) in &lift.args {
            lifted = lifted.with(*name, value.clone());
        }
        lifted_from =
            crate::abi::lift::words::split(command).and_then(|words| words.first().cloned());
        projection = lift.projection;
        call_args = lifted;
        requested_tool = target;
    }
    let tool = if requested_tool.name() == "read" {
        call_args
            .get("path")
            .and_then(|path| {
                crate::project::source_context::infer_incomplete_target(
                    &state.profile,
                    std::path::Path::new(path),
                )
                .ok()
                .flatten()
            })
            .and_then(|symbol| {
                call_args = call_args.clone().with("symbol", symbol);
                registry::lookup("context")
            })
            .unwrap_or(requested_tool)
    } else {
        requested_tool
    };
    let implicit = tool.name() == "edit" && call_args.get("expected_sha256").is_none();
    if implicit && let Some(hash) = state.visible_source_hash(&call_args) {
        call_args = call_args.with("expected_sha256", hash);
    }
    // No visible version, or one the file has since moved past: the lines
    // themselves may still be ones the model saw, byte for byte.
    let mut bound_by_lines = false;
    if implicit
        && let Some(version) = state.seen_lines_cover(&call_args)
        && call_args.get("expected_sha256") != Some(version.as_str())
    {
        call_args = call_args.with("expected_sha256", version);
        bound_by_lines = true;
    }
    if tool.name() == "edit" && !state.source_version_is_visible(&call_args) && !bound_by_lines {
        // **The rule stands; the cell does not pay for it.** A version binds
        // when the model has actually read it, which is the next cell -- see
        // `edit_binding_gap`. Refusing the one call into the program gives
        // that guarantee exactly, and the message names which of the four
        // situations this is instead of telling a caller to do what it just
        // did.
        let gap = state.edit_binding_gap(&call_args);
        throw_tool_error(scope, &gap);
        return;
    }
    if state.instruction_boundary(tool.name(), &call_args) {
        // **Kept a termination, against the brief, on the evidence.** The
        // gate latches -- `Instructions::gate` returns true for every later
        // call once it is pending -- so a throw the program catches refuses
        // the rest of the cell anyway. Measured: a program that catches this
        // and writes again writes nothing. What a throw would cost is the
        // signal, turning a `Yielded` carrying its reason into a `Threw`
        // carrying an error, for no work recovered.
        stop_for_instructions(scope, &format!("`{}`", tool.name()), "the call did not run");
        return;
    }
    // Cloned rather than borrowed across the call: the call is the longest
    // thing this crate does, and a `RefCell` borrow held across it would
    // outlive every reason to hold it. Every clone names the same flag.
    let token = state.token.borrow().clone();
    let run = || {
        let context = ToolContext {
            profile: &state.profile,
            session: &state.session,
        };
        let gate = (!state.subagent.get())
            .then(|| state.approval_gate.borrow().clone())
            .flatten();
        // V8 termination does not unwind a Rust callback blocked on a reply,
        // and its query need not turn true before the callback returns. Poll
        // the watchdog's own flag as well as the user token. A gate entered
        // outside watched execution is closed rather than an unbounded wait.
        let isolate = scope.thread_safe_handle();
        let watchdog = state.watchdog_fired.borrow().clone();
        // The pause for a running child is taken *inside* `invoke`, around
        // the wait itself — not here. A hook (`glasshouse::run`) is also a
        // child of this call and has no bound of its own, so it must stay on
        // the cell's clock; that clock is all that ends a hook that hangs.
        invoke::run_traced_pausing(
            &context,
            &token,
            tool.name(),
            &call_args,
            gate.as_ref(),
            &|| {
                token.is_cancelled()
                    || watchdog
                        .as_ref()
                        .is_none_or(|fired| fired.load(std::sync::atomic::Ordering::SeqCst))
                    || isolate.is_execution_terminating()
            },
            Some(&state.host_clock),
        )
    };
    let check = call_args.get("command").filter(|line| {
        tool.name() == "bash" && lifted_from.is_none() && crate::verification::is_pure_check(line)
    });
    let traced = match check {
        Some(line) => crate::verification::ShellChecks::run_or_reuse(
            &state.shell_checks,
            &state.profile,
            line,
            run,
        ),
        None => run(),
    };

    // The lift's exact range is applied before anything reads the result,
    // so the trajectory, the repeat check and the handle all see the bytes
    // the program was actually given.
    let traced = invoke::Traced {
        outcome: traced.outcome.map(|result| match projection {
            Some(projection) => project(result, projection),
            None => result,
        }),
        checked: traced.checked,
    };

    // §9.4: recorded here because this is where every call funnels, and
    // recorded with the arguments `invoke` checked rather than the ones the
    // program wrote. Each class below is the class the matching throw
    // constructs, so the line says what the program could have caught, and
    // `error` is the message that throw carries.
    let (ended, error) = match &traced.outcome {
        Ok(result) => match call_failure(tool.name(), result) {
            Some(message) => (
                Ended::Threw {
                    class: "ToolError".to_string(),
                },
                Some(message),
            ),
            None => (Ended::Ok, None),
        },
        Err(ToolError::Denied(denied)) => (
            Ended::Denied {
                rule: denied.rule.clone(),
            },
            Some(denied.to_string()),
        ),
        Err(cancelled @ ToolError::Cancelled { .. }) => (
            Ended::Threw {
                class: "Cancelled".to_string(),
            },
            Some(cancelled.to_string()),
        ),
        Err(spawn @ ToolError::Spawn { .. }) => (
            Ended::Threw {
                class: "ToolError".to_string(),
            },
            Some(spawn.to_string()),
        ),
    };
    // A process result carries the child's exit status; an in-process call's
    // `Some(0)` is a convention and is not recorded as an observation.
    let exit_code = traced
        .outcome
        .as_ref()
        .ok()
        .filter(|result| result.confinement != invoke::Confinement::InProcess)
        .and_then(|result| result.exit_code);
    // A pure call that returned bytes an earlier call already returned is a
    // repeat. Decided after the call, by the digest: the call is never
    // skipped, because only running it can say the file did not change.
    let digest = traced.outcome.as_ref().ok().map(|result| {
        use sha2::{Digest, Sha256};
        format!("{:x}", Sha256::digest(result.stdout.as_bytes()))
    });
    let repeat = match (&digest, ended == Ended::Ok, tool.purity()) {
        (Some(digest), true, registry::Purity::Pure) => {
            state.observation_repeat(tool.name(), &traced.checked, digest)
        }
        _ => None,
    };
    // A `context` result carries the packed context itself, so one that does
    // not fit what is left of this turn's feedback budget is **narrowed to
    // what is left rather than refused**: the file was already opened, the
    // definition already found, and discarding that spends a whole round
    // trip to learn what is already in hand.
    //
    // The cell keeps running either way. A context nobody could deliver is
    // an omission on its own record -- carrying the two numbers, so the next
    // request can be aimed instead of repeated -- and never the end of a
    // turn that had eight other contexts in it.
    let evidence = traced
        .outcome
        .as_ref()
        .ok()
        .filter(|_| tool.name() == "context")
        .and_then(|result| {
            serde_json::from_str::<crate::project::source_context::SourceContext>(&result.stdout)
                .ok()
        })
        .map(|mut packed| {
            let budget = state.remaining_context_budget();
            let delivered = state.context_shown(&packed) || packed.narrow_to(budget);
            // Rebuilt from the narrowed context, never copied from the
            // tool's own payload: a record that named ranges the turn never
            // received would be describing a delivery that did not happen.
            let mut evidence = SourceEvidence {
                path: packed.path.clone(),
                sha256: packed.sha256.clone(),
                complete: packed.complete,
                ranges: context_ranges(&packed),
                omissions: packed.omissions.clone(),
            };
            // Certification rides on `complete`, which describes the
            // TARGET and which narrowing never touches -- only supporting
            // excerpts are shed, so a delivered context's target is
            // byte-exact exactly as it was before.
            //
            // **The queue's own answer decides, never the narrowing's.**
            // `narrow_to` measures against `remaining_context_budget` and the
            // queue measures again on the way in. Today the two agree
            // arithmetically; if they ever drift, the context is dropped, and
            // reading the answer rather than assuming it is what keeps that
            // drop loud instead of silent.
            let queued = delivered && state.note_source_context(&evidence, &packed);
            if queued {
                state.note_seen_lines(std::iter::once(&packed.target).chain(&packed.supporting));
            }
            if !queued {
                evidence.omissions.push(format!(
                    "not delivered to this turn's feedback: {budget} characters remained and this context renders {}; ask for it first in the next cell, or name a narrower symbol",
                    packed.render().chars().count()
                ));
            }
            evidence
        });
    let mut args = traced.checked.clone();
    if bound_by_lines {
        args.insert("bound".into(), "seen lines".into());
    }
    if let (Some(_), Ok(result)) = (check, &traced.outcome)
        && result.exit_code.is_some_and(|code| code != 0)
    {
        let output = format!("{}\n{}", result.stdout, result.stderr);
        let found = crate::verification::failure_locations(&output, state.profile.root(), 3);
        let attached = state.note_failure_locations(&found);
        if attached > 0 {
            args.insert("failure_locations".into(), attached.to_string());
        }
    }
    trace(scope).record(CallRecord {
        tool: tool.name().to_string(),
        args,
        evidence,
        lifted_from: lifted_from.clone(),
        exit_code,
        repeat_of: repeat.as_ref().map(|repeat| repeat.cell),
        error,
        ended,
    });

    if traced.outcome.is_ok() && state.instruction_file_written(tool.name(), &call_args) {
        trace(scope).request_yield(Some(format!(
            "`{}` completed and changed project instructions; execution stopped before any later call",
            tool.name()
        )));
        scope.terminate_execution();
        return;
    }

    match traced.outcome {
        Ok(result) => {
            let (value, call) = typed_result(
                scope,
                marshal_as,
                &call_args,
                &result,
                &state,
                repeat.as_ref(),
            );
            if let (Some(digest), Some(_)) = (&digest, call) {
                state.note_observation(tool.name(), &traced.checked, digest, call);
            }
            if call.is_some() {
                note_mutation(&state, tool.name(), &traced.checked, &result);
            }
            // The canonical typed result, captured as the model sees it, for a
            // frame lowered from direct provider calls. Stringifying the value
            // the isolate returns is what makes the provider result and the
            // cell result one observation rather than two encodings. Only a
            // call that minted a handle is captured: the session pairs these
            // with the calls that ended `ok`, in order, and a failed call's
            // `undefined` would shift every result after it.
            let trace = trace(scope);
            if trace.captures_results() && call.is_some() {
                let json = v8::json::stringify(scope, value)
                    .map(|json| json.to_rust_string_lossy(scope))
                    .unwrap_or_else(|| "null".to_string());
                trace.record_result(json);
            }
            retval.set(value);
        }
        Err(ToolError::Denied(denied)) => throw_denied(scope, &denied),
        Err(ToolError::Cancelled { tool }) => throw_cancelled(scope, &tool),
        Err(other) => throw_tool_error(scope, &other.to_string()),
    }
}

/// The ranges a packed context actually carries, target first.
///
/// Built from the context in hand rather than copied from the tool's
/// payload, so a narrowed delivery reports the excerpts that survived it.
/// The role spelling comes from the same `Serialize` the tool used, so the
/// two cannot drift apart.
fn context_ranges(packed: &crate::project::source_context::SourceContext) -> Vec<SourceRange> {
    std::iter::once(&packed.target)
        .chain(packed.supporting.iter())
        .map(|excerpt| SourceRange {
            path: excerpt.path.clone(),
            start: excerpt.range.start,
            end: excerpt.range.end,
            role: serde_json::to_value(&excerpt.role)
                .ok()
                .and_then(|role| role.as_str().map(str::to_owned))
                .unwrap_or_default(),
        })
        .collect()
}

/// Reads the call's single object argument into [`Args`], or the message the
/// refusal throws.
///
/// Every own property is passed through, including one the tool does not
/// declare: [`invoke::run`] refuses an undeclared argument, and dropping it
/// here would make a call that named the wrong argument look like it had
/// honoured it.
///
/// **An argument is a string or an array of strings, and anything else is
/// refused rather than stringified.** Every declared `ArgKind` is one of
/// those two, so a lossy `to_rust_string_lossy` on the rest could only ever
/// spell a JavaScript value the tool cannot use: a handle passed as `content`
/// wrote the nine characters `[object Object]` into a real file, with no
/// error anywhere.
fn read_arguments(scope: &mut v8::PinScope, value: v8::Local<v8::Value>) -> Result<Args, String> {
    let mut args = Args::new();
    if !value.is_object() {
        return Ok(args);
    }
    let Ok(object) = v8::Local::<v8::Object>::try_from(value) else {
        return Ok(args);
    };
    let Some(names) = object.get_own_property_names(scope, v8::GetPropertyNamesArgs::default())
    else {
        return Ok(args);
    };
    for index in 0..names.length() {
        let Some(key) = names.get_index(scope, index) else {
            continue;
        };
        let Some(given) = object.get(scope, key) else {
            continue;
        };
        if given.is_undefined() || given.is_null() {
            continue;
        }
        let key = key.to_rust_string_lossy(scope);
        if given.is_array() {
            let Ok(array) = v8::Local::<v8::Array>::try_from(given) else {
                return Err(format!(
                    "`{key}` accepts an array of strings; it could not be read"
                ));
            };
            let mut lines = Vec::with_capacity(array.length() as usize);
            for index in 0..array.length() {
                let line = array.get_index(scope, index).ok_or_else(|| {
                    format!("`{key}` accepts an array of strings; item {index} could not be read")
                })?;
                if !line.is_string() {
                    return Err(format!(
                        "`{key}` accepts an array of strings; item {index} is {}",
                        js_type_of(line)
                    ));
                }
                lines.push(line.to_rust_string_lossy(scope));
            }
            args = args.with_lines(key, lines);
        } else if given.is_string() {
            args = args.with(key, given.to_rust_string_lossy(scope));
        } else {
            return Err(argument_refusal(&key, given));
        }
    }
    Ok(args)
}

/// What the model is told about one argument it cannot pass.
///
/// It states the rule, never the argument's own contract, because an
/// *undeclared* argument reaches here too: telling the model that `offset`
/// "accepts a string" would imply quoting the value makes an argument exist
/// that the tool does not have. Naming the argument by name is enough.
fn argument_refusal(key: &str, given: v8::Local<v8::Value>) -> String {
    format!(
        "`{key}` was passed {}; a tool argument must be a string, or an array of strings",
        js_type_of(given)
    )
}

/// A rejected value named the way the model's own `typeof` would name it.
fn js_type_of(value: v8::Local<v8::Value>) -> &'static str {
    if value.is_array() {
        "an array"
    } else if value.is_function() {
        "a function"
    } else if value.is_number() {
        "a number"
    } else if value.is_boolean() {
        "a boolean"
    } else if value.is_object() {
        "an object"
    } else {
        "a value of another type"
    }
}

/// A successful mutation makes its result the visible version of the path
/// at once, so the next `edit` binds to what sterna itself just wrote.
///
/// `edit` reports `after_sha256`; `write` hashes the checked text it wrote,
/// which is byte for byte what reached the file. Anything else is not a
/// mutation of a source version and registers nothing.
fn note_mutation(
    state: &Rc<RuntimeState>,
    tool: &str,
    checked: &invoke::CheckedArgs,
    result: &ToolResult,
) {
    let Some(path) = checked.get("path") else {
        return;
    };
    let sha256 = match tool {
        "edit" => serde_json::from_str::<serde_json::Value>(&result.stdout)
            .ok()
            .and_then(|edit| edit.get("after_sha256")?.as_str().map(str::to_owned)),
        "write" => checked
            .get("content")
            .or_else(|| checked.get("lines"))
            .map(|text| {
                use sha2::{Digest, Sha256};
                format!("{:x}", Sha256::digest(text.as_bytes()))
            }),
        _ => None,
    };
    if let Some(sha256) = sha256 {
        state.note_mutation(std::path::Path::new(path), &sha256);
    }
}

/// Builds the tool's declared result type, records the call, and tags the
/// object so the binding that holds it inherits the call's provenance.
/// Answers with the value and the id the call was recorded under, which is
/// `None` exactly when the call failed and no handle was minted.
fn typed_result<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    tool: &Tool,
    args: &Args,
    result: &ToolResult,
    state: &Rc<RuntimeState>,
    repeat: Option<&crate::runtime::state::Repeat>,
) -> (v8::Local<'s, v8::Value>, Option<u64>) {
    // Before the builders, because every one of them reads `stdout` and none
    // of them reads the exit code: a `read` of a missing file produced a
    // `File` handle of 0 bytes carrying the SHA-256 of the empty string, and
    // a model quoted it as the task's answer.
    if let Some(message) = call_failure(tool.name(), result) {
        throw_tool_error(scope, &message);
        // The exception is what the callback answers with; V8 discards a
        // return value once one is pending, and nothing below has run, so no
        // handle is minted. The actual child call was already recorded as a
        // `ToolError` throw by the common callback.
        return (v8::undefined(scope).into(), None);
    }

    let (value, preview, label) = match tool.name() {
        "read" => {
            let (value, preview) = build_file(scope, args, result);
            (value, preview, "File")
        }
        // **A pure search tool is typed as what it prints.** `rg` is spawned
        // with `--line-number --no-heading --color=never` (`invoke`'s
        // `Argv::SearchIn`), so its stdout is `path:line:text` — `grep -r
        // -n`'s own shape — and `fd` prints one path per line as `glob` does.
        // `abi::Router::present` already routes all four to `present_items`,
        // which needs an array and fell back to "not an array" for the two
        // that arrived as a process result.
        "grep" | "rg" => {
            let (value, preview) = build_grep(scope, args, result, state.profile.root());
            (value, preview, "Grep.Match[]")
        }
        "glob" | "fd" => {
            let (value, preview) = build_glob(scope, result, state.profile.root());
            (value, preview, "string[]")
        }
        "context" => {
            let (value, preview) = build_structured_result(scope, result);
            (value, preview, "Code.Context")
        }
        "edit" => {
            let (value, preview) = build_structured_result(scope, result);
            (value, preview, "Code.Edit")
        }
        // `jq`, `write` and `bash` keep the process shape: JSON, a path and a
        // command's output are none of them matches.
        //
        // **The failure reducer only ever sees a command line's output.**
        // `bash` is the only tool that runs one, and `REDUCER` reads build
        // and test output for its distinct failures; handed anything else it
        // spends a cheap model's tokens to answer that source text contains
        // no failures. Measured 2026-09-17 in a real session: four such
        // calls over `rg` results, 23,098 input tokens, 15.8 s, four answers
        // of "No failures."
        _ => {
            let reduction = if tool.name() == "bash" {
                crate::runtime::reduce::reduce_oversized(result, state)
            } else {
                crate::runtime::reduce::Reduction::NotAttempted
            };
            let (value, preview) = build_bash(scope, result, &reduction);
            (value, preview, "Bash.Result")
        }
    };

    // A repeat is said in the table header, where the model reads the type,
    // so it can see the repetition without re-reading the body.
    let type_label = match repeat {
        Some(repeat) => format!("{label}{}", repeat.label_suffix()),
        None => label.to_string(),
    };
    let meta = HandleMeta {
        type_label: Some(type_label),
        size_estimate: (result.stdout.len() + result.stderr.len()) as u64,
        provenance: Some(provenance(
            tool.name(),
            args,
            &result.stdout,
            tool.purity().may_rematerialise(),
        )),
    };
    let id = state.record_call(RecordedCall { preview, meta });
    if let Ok(object) = v8::Local::<v8::Object>::try_from(value) {
        let tag = call_tag(scope);
        // A `Number`, not an `Integer`: the id counts every call of the whole
        // task, and an `i32` would wrap where a task made two billion of them.
        let marker = v8::Number::new(scope, id as f64);
        object.set_private(scope, tag, marker.into());
    }
    (value, Some(id))
}

fn build_structured_result<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    result: &ToolResult,
) -> (v8::Local<'s, v8::Value>, Value) {
    let parsed: serde_json::Value = serde_json::from_str(&result.stdout)
        .unwrap_or_else(|_| serde_json::json!({ "text": result.stdout }));
    let value = json_to_v8(scope, &parsed);
    let preview = match &parsed {
        serde_json::Value::Object(fields) => Value::object(
            fields
                .iter()
                .take(8)
                .map(|(key, value)| {
                    let preview = match value {
                        serde_json::Value::Null => Value::Null,
                        serde_json::Value::Bool(value) => Value::Boolean(*value),
                        serde_json::Value::Number(value) => {
                            Value::Number(value.as_f64().unwrap_or_default())
                        }
                        serde_json::Value::String(value) => Value::String(StringValue::sampled(
                            value.chars().count(),
                            value.chars().take(160).collect(),
                        )),
                        serde_json::Value::Array(values) => {
                            Value::Array(ArrayValue::sampled(values.len(), Vec::new(), None))
                        }
                        serde_json::Value::Object(values) => Value::object(
                            values
                                .keys()
                                .take(8)
                                .map(|key| {
                                    (
                                        key.clone(),
                                        Value::String(StringValue::sampled(1, "…".into())),
                                    )
                                })
                                .collect(),
                        ),
                    };
                    (key.clone(), preview)
                })
                .collect(),
        ),
        _ => Value::String(StringValue::sampled(
            result.stdout.chars().count(),
            result.stdout.chars().take(200).collect(),
        )),
    };
    (value, preview)
}

fn json_to_v8<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    value: &serde_json::Value,
) -> v8::Local<'s, v8::Value> {
    match value {
        serde_json::Value::Null => v8::null(scope).into(),
        serde_json::Value::Bool(value) => v8::Boolean::new(scope, *value).into(),
        serde_json::Value::Number(value) => {
            v8::Number::new(scope, value.as_f64().unwrap_or_default()).into()
        }
        serde_json::Value::String(value) => js_string(scope, value),
        serde_json::Value::Array(values) => {
            let array = v8::Array::new(scope, values.len() as i32);
            for (index, value) in values.iter().enumerate() {
                let value = json_to_v8(scope, value);
                array.set_index(scope, index as u32, value);
            }
            array.into()
        }
        serde_json::Value::Object(values) => {
            let object = v8::Object::new(scope);
            for (key, value) in values {
                let value = json_to_v8(scope, value);
                set_key(scope, object, key, value);
            }
            object.into()
        }
    }
}

/// Why a tool call cannot become a result — `runtime-contract.md` §9.1's *a
/// failed call cannot itself become an answer*.
///
/// **`grep` and `glob` keep exit 1**, which is "no matches" and is an empty
/// array rather than a failure; exit 2 and above is a bad pattern or an
/// unreadable root and throws like everything else. **`bash` is never refused
/// for its exit code**: that code is part of its declared result and the
/// program reads it.
///
/// **An absent exit code is a failure for every tool, `bash` included.** It
/// means the child died of a signal, which is not an exit status and is not a
/// success; the early return that used to be here made a SIGKILLed call a
/// typed result built from partial output and recorded it `Ended::Ok`.
fn call_failure(tool: &str, result: &ToolResult) -> Option<String> {
    let Some(code) = result.exit_code else {
        return Some(format!(
            "`{tool}` was killed by a signal and never exited, so any output it produced is \
             partial"
        ));
    };
    let tolerated = match tool {
        "bash" => return None,
        // `rg` exits 1 for "no match", exactly as `grep` does: an empty
        // result is an answer, not a failure.
        "grep" | "glob" | "rg" => code <= 1,
        _ => code == 0,
    };
    if tolerated {
        return None;
    }
    Some(format!(
        "`{tool}` failed with exit {code}: {}",
        stderr_reason(&result.stderr)
    ))
}

/// One line, bounded like a preview: the model needs the reason, not the
/// child's whole diagnostic. A first line that only announces one
/// (`rg: regex parse error:`) gets the last line, which states it.
fn stderr_reason(stderr: &str) -> String {
    let lines: Vec<&str> = stderr
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();
    let detail = match lines.as_slice() {
        [] => "the child wrote nothing to stderr".into(),
        [first, .., last] if first.ends_with(':') => format!("{first} {last}"),
        [first, ..] => (*first).to_string(),
    };
    detail.chars().take(200).collect()
}

fn build_file<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    args: &Args,
    result: &ToolResult,
) -> (v8::Local<'s, v8::Value>, Value) {
    let path = args.get("path").unwrap_or_default().to_string();
    let lines: Vec<&str> = result.stdout.lines().collect();

    let object = v8::Object::new(scope);
    let path_value = js_string(scope, &path);
    set_key(scope, object, "path", path_value);
    let bytes = v8::Number::new(scope, result.stdout.len() as f64);
    set_key(scope, object, "bytes", bytes.into());
    let count = v8::Number::new(scope, lines.len() as f64);
    set_key(scope, object, "lineCount", count.into());
    let text = js_string(scope, &result.stdout);
    set_key(scope, object, "text", text);
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(result.stdout.as_bytes());
    let digest = js_string(scope, &format!("{digest:x}"));
    set_key(scope, object, "sha256", digest);
    let array = v8::Array::new(scope, lines.len() as i32);
    for (index, line) in lines.iter().enumerate() {
        let line = js_string(scope, line);
        array.set_index(scope, index as u32, line);
    }
    set_key(scope, object, "lines", array.into());
    if let Some(function) = v8::Function::builder(file_excerpt_callback)
        .data(array.into())
        .build(scope)
    {
        set_fixed_key(scope, object, "excerpt", function.into());
    }
    // Content and admitted metadata arrive together from the tool layer.
    let mtime = result.modified.clone().unwrap_or_else(|| "unknown".into());
    let mtime_value = js_string(scope, &mtime);
    set_key(scope, object, "mtime", mtime_value);

    let preview = Value::File(FileValue {
        path,
        byte_len: result.stdout.len() as u64,
        line_count: lines.len() as u64,
        mtime,
        lines: lines.iter().take(2).map(|line| line.to_string()).collect(),
    });
    (object.into(), preview)
}

fn positive_integer_option(
    scope: &mut v8::PinScope,
    options: v8::Local<v8::Value>,
    name: &str,
    default: usize,
) -> Option<usize> {
    if options.is_undefined() || options.is_null() {
        return Some(default);
    }
    let Ok(object) = v8::Local::<v8::Object>::try_from(options) else {
        throw_tool_error(scope, "File.excerpt expects one options object");
        return None;
    };
    let key = v8::String::new(scope, name)?;
    let value = object.get(scope, key.into())?;
    if value.is_undefined() || value.is_null() {
        return Some(default);
    }
    let Some(number) = value.number_value(scope) else {
        throw_tool_error(
            scope,
            &format!("File.excerpt `{name}` must be a positive integer"),
        );
        return None;
    };
    if !number.is_finite() || number < 1.0 || number.fract() != 0.0 || number > usize::MAX as f64 {
        throw_tool_error(
            scope,
            &format!("File.excerpt `{name}` must be a positive integer"),
        );
        return None;
    }
    Some(number as usize)
}

fn sample_line(scope: &v8::PinScope, line: v8::Local<v8::String>) -> SampledLine {
    let total = line.length();
    let mut units = vec![0u16; total.min(excerpt::MAX_LINE_UNITS)];
    line.write_v2(scope, 0, &mut units, v8::WriteFlags::empty());
    if total > units.len()
        && units
            .last()
            .is_some_and(|unit| (0xD800..=0xDBFF).contains(unit))
    {
        units.pop();
    }
    let consumed_units = units.len();
    SampledLine {
        text: String::from_utf16_lossy(&units),
        consumed_units,
        omitted_units: total.saturating_sub(consumed_units),
    }
}

fn file_excerpt_callback(
    scope: &mut v8::PinScope,
    args: v8::FunctionCallbackArguments,
    mut retval: v8::ReturnValue,
) {
    let Some(start) = positive_integer_option(scope, args.get(0), "start", 1) else {
        return;
    };
    let Some(lines_requested) =
        positive_integer_option(scope, args.get(0), "lines", excerpt::DEFAULT_LINES)
    else {
        return;
    };
    let Ok(lines) = v8::Local::<v8::Array>::try_from(args.data()) else {
        throw_tool_error(scope, "File.excerpt is detached from its read result");
        return;
    };
    let line_count = lines.length() as usize;
    let count = lines_requested.min(excerpt::MAX_LINES);
    let mut samples = Vec::with_capacity(count);
    let first = start.saturating_sub(1).min(line_count);
    let last = first.saturating_add(count).min(line_count);
    for index in first..last {
        let Some(value) = lines.get_index(scope, index as u32) else {
            break;
        };
        let Ok(line) = v8::Local::<v8::String>::try_from(value) else {
            break;
        };
        samples.push(sample_line(scope, line));
    }
    let excerpt = excerpt::render(line_count, start, lines_requested, samples);
    let object = v8::Object::new(scope);
    let text = js_string(scope, &excerpt.text);
    set_key(scope, object, "text", text);
    for (name, value) in [
        ("start", excerpt.start),
        ("lineCount", excerpt.line_count),
        ("truncatedLines", excerpt.truncated_lines),
    ] {
        let number = v8::Number::new(scope, value as f64);
        set_key(scope, object, name, number.into());
    }
    match excerpt.end {
        Some(value) => {
            let number = v8::Number::new(scope, value as f64);
            set_key(scope, object, "end", number.into());
        }
        None => {
            let null = v8::null(scope);
            set_key(scope, object, "end", null.into());
        }
    }
    match excerpt.next {
        Some(value) => {
            let number = v8::Number::new(scope, value as f64);
            set_key(scope, object, "next", number.into());
        }
        None => {
            let null = v8::null(scope);
            set_key(scope, object, "next", null.into());
        }
    }
    retval.set(object.into());
}

fn build_bash<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    result: &ToolResult,
    reduction: &crate::runtime::reduce::Reduction,
) -> (v8::Local<'s, v8::Value>, Value) {
    let object = v8::Object::new(scope);
    let stdout = js_string(scope, &result.stdout);
    set_key(scope, object, "stdout", stdout);
    let stderr = js_string(scope, &result.stderr);
    set_key(scope, object, "stderr", stderr);
    match result.exit_code {
        Some(code) => {
            let code = v8::Integer::new(scope, code);
            set_key(scope, object, "exit_code", code.into());
        }
        None => {
            let null = v8::null(scope);
            set_key(scope, object, "exit_code", null.into());
        }
    }
    let mut entries = vec![
        ("stdout".to_string(), Value::string(&result.stdout)),
        ("stderr".to_string(), Value::string(&result.stderr)),
        (
            "exit_code".to_string(),
            result
                .exit_code
                .map_or(Value::Null, |code| Value::Number(f64::from(code))),
        ),
    ];
    // `reduced` when one was made, `reduction_error` when one was attempted
    // and failed, and neither when none was needed. Three states, because an
    // absent key alone cannot say which of the other two happened.
    match reduction {
        crate::runtime::reduce::Reduction::NotAttempted => {}
        crate::runtime::reduce::Reduction::Made(reduced) => {
            let value = js_string(scope, reduced);
            set_key(scope, object, "reduced", value);
            entries.push(("reduced".to_string(), Value::string(reduced)));
        }
        crate::runtime::reduce::Reduction::Failed(why) => {
            let value = js_string(scope, why);
            set_key(scope, object, "reduction_error", value);
            entries.push(("reduction_error".to_string(), Value::string(why)));
        }
    }
    (object.into(), Value::object(entries))
}

// --- the handle functions ----------------------------------------------

fn keep_callback(
    scope: &mut v8::PinScope,
    args: v8::FunctionCallbackArguments,
    _retval: v8::ReturnValue,
) {
    let name = args.get(0).to_rust_string_lossy(scope);
    if !is_identifier(&name) {
        throw_tool_error(
            scope,
            &format!("keep(\"{name}\") is not a name a program could have bound"),
        );
        return;
    }
    capture(scope, &name, args.get(1), false);
    state(scope).note_pin(&name);
}

fn free_callback(
    scope: &mut v8::PinScope,
    args: v8::FunctionCallbackArguments,
    _retval: v8::ReturnValue,
) {
    let name = args.get(0).to_rust_string_lossy(scope);
    if refuse_host_name(scope, &name, "freed") {
        return;
    }
    let state = state(scope);
    state.table.borrow_mut().free(&name);
    state.note_free(&name);
    let context = scope.get_current_context();
    let global = context.global(scope);
    if let Some(key) = v8::String::new(scope, &name) {
        global.delete(scope, key.into());
    }
}

/// Every name the model can address right now — `runtime-contract.md` §3's
/// drop note promises this is "the full list", and the list a program is
/// shown when it asks mid-cell has to include what that cell has just bound.
///
/// The table alone is one cell stale: captures are drained into it when the
/// cell ends. So the current cell's captures are appended, newest last by
/// construction, and a name `free` released this cell is in neither.
fn handles_callback(
    scope: &mut v8::PinScope,
    _args: v8::FunctionCallbackArguments,
    mut retval: v8::ReturnValue,
) {
    let state = state(scope);
    let mut names: Vec<String> = state
        .table
        .borrow()
        .names()
        .into_iter()
        .map(str::to_string)
        .collect();
    for capture in &state.current.borrow().captures {
        if !names.contains(&capture.name) {
            names.push(capture.name.clone());
        }
    }
    let array = v8::Array::new(scope, names.len() as i32);
    for (index, name) in names.iter().enumerate() {
        let value = js_string(scope, name);
        array.set_index(scope, index as u32, value);
    }
    retval.set(array.into());
}

/// `s(name, value)` from a declaration line, and `s(name, value, true)` from
/// the generated epilogue.
///
/// The third argument is what tells the two apart, and it is load-bearing for
/// `free`: a name the model freed and then bound again in the same cell must
/// come back (that is a binding the program still names), while the
/// epilogue's blind re-read of every `late` name must **not** resurrect one
/// the model freed after declaring it. Only [`cell::compile`] writes either
/// call, and a program that reaches this callback by hand reaches it through
/// `keep`, which passes no third argument.
fn capture_callback(
    scope: &mut v8::PinScope,
    args: v8::FunctionCallbackArguments,
    _retval: v8::ReturnValue,
) {
    let name = args.get(0).to_rust_string_lossy(scope);
    let late = args.get(2).is_true();
    capture(scope, &name, args.get(1), late);
}

/// The most characters of a `yieldNow` reason the result block carries —
/// `runtime-contract.md` §3's preview cap, by the crate's own four-characters-
/// a-token estimate.
const REASON_CHARS: usize = PREVIEW_TOKEN_CAP * 4;

/// `yieldNow(reason?)` — `runtime-contract.md` §9.3: the cell ends in the
/// yield slot at once, from wherever it was called.
///
/// **It runs no code and touches no state but the cell's own flag.** The
/// mechanism is `terminate_execution`, the one way V8 offers to stop a
/// running program from inside a callback that no `try`/`catch` in the
/// program can intercept; the isolate reads the flag the instant the cell
/// stops and answers with a yield rather than the `RuntimeTerminated` a
/// termination nobody asked for would be. The heap ceiling and the wall
/// clock are read first and win, so a cell that hit either while also asking
/// to yield is told the truth about why it stopped.
fn yield_now_callback(
    scope: &mut v8::PinScope,
    args: v8::FunctionCallbackArguments,
    _retval: v8::ReturnValue,
) {
    let given = args.get(0);
    let reason = if given.is_undefined() || given.is_null() {
        None
    } else {
        Some(bound_reason(&given.to_rust_string_lossy(scope)))
    };
    trace(scope).request_yield(reason);
    scope.terminate_execution();
    // `terminate_execution` requests an interrupt, and V8 services one at a
    // stack check -- a function entry or a loop back-edge -- never on the
    // return from an API callback. Measured: straight-line statements after
    // `yieldNow()` ran to the end of the cell, which then fell off normally.
    // Entering this loop is the stack check: the pending termination is
    // serviced before its first iteration completes and unwinds through here
    // and out of the program, so nothing after the call ever runs. It cannot
    // spin -- the termination is already requested -- and were it somehow
    // not honoured, the watchdog would end the cell as a timeout rather than
    // let it hang.
    if let Some(source) = v8::String::new(scope, "for (;;) {}")
        && let Some(script) = v8::Script::compile(scope, source, None)
    {
        script.run(scope);
    }
}

/// `answer(text)` — the one way a cell ends the person's task
/// (`runtime-contract.md` §9.2). It records the text and returns; the
/// program keeps running to its own end, so a cell may answer and still
/// tidy up after itself.
///
/// **It is a statement, not an inference.** Before this existed the task
/// ended when a cell returned a value that was not an array or an object,
/// which made `return someBashResult.stdout` -- a thing written to *look*
/// at output -- publish that output as the final answer and stop the
/// session. Nothing about a value's type says the work is done; only the
/// program can.
///
/// **It ends the cell where it is called**, the way [`yield_now_callback`]
/// does and for the same reason: a program that answers inside a branch --
/// `if (confident) { answer("look closer"); }` followed by
/// `answer("rename only");` -- means the first one. Recording the text and
/// running on let the second
/// overwrite it -- an intention overruled by the shape of the program, which
/// is the defect this whole contract exists to remove.
fn answer_callback(
    scope: &mut v8::PinScope,
    args: v8::FunctionCallbackArguments,
    _retval: v8::ReturnValue,
) {
    let given = args.get(0);
    if given.is_undefined() || given.is_null() {
        return;
    }
    let trace = trace(scope);
    trace.record_answer(given.to_rust_string_lossy(scope));
    // Deliberate, so the isolate answers with a yield rather than the
    // `RuntimeTerminated` a termination nobody asked for would be. The task
    // is over either way -- `ends_the_task` reads the answer, not this -- but
    // a cell that stopped on purpose must not read as one that was killed.
    trace.request_yield(None);
    scope.terminate_execution();
    // The same stack check `yield_now_callback` documents: V8 services a
    // termination at a loop back-edge, never on the return from an API
    // callback, so entering this loop is what stops the lines after the call.
    if let Some(source) = v8::String::new(scope, "for (;;) {}")
        && let Some(script) = v8::Script::compile(scope, source, None)
    {
        script.run(scope);
    }
}

/// One line, at most [`REASON_CHARS`] characters, cut on a character
/// boundary and saying so — the reason is a line of the result block and
/// must neither forge a second line nor cost the turn more than a preview.
fn bound_reason(reason: &str) -> String {
    let one_line = reason.lines().collect::<Vec<_>>().join(" ");
    let total = one_line.chars().count();
    if total <= REASON_CHARS {
        return one_line;
    }
    let head: String = one_line.chars().take(REASON_CHARS).collect();
    format!(
        "{head}…(reason cut at {} of {} characters)",
        thousands(REASON_CHARS as u64),
        thousands(total as u64)
    )
}

/// The generated epilogue's `return __sterna_cell.e()`: it answers with a
/// fresh object carrying [`end_tag`] for the cell that is running, and the
/// isolate reads the cell's ending off the value its promise fulfils with.
///
/// Nothing is recorded on the host side, which is the point — the flag this
/// replaced could be set by one line of the model's own program, and the
/// cell then yielded where §1 says it returns.
fn fell_callback(
    scope: &mut v8::PinScope,
    _args: v8::FunctionCallbackArguments,
    mut retval: v8::ReturnValue,
) {
    let cell = state(scope).cell.get();
    let marker = v8::Object::new(scope);
    let tag = end_tag(scope);
    let minted = v8::Number::new(scope, cell as f64);
    marker.set_private(scope, tag, minted.into());
    retval.set(marker.into());
}

/// The one path by which a value becomes a handle: it is put on the
/// persistent scope so the next cell can name it, and its preview is
/// recorded so this turn can show it.
///
/// **Every write to the persistent scope goes through here** — `keep`, the
/// compiled declaration-line captures and the epilogue's re-reads alike —
/// which is why the host-function guard lives here rather than only in
/// `cell::compile`. The compile-time refusal is the good early message for a
/// binding a program declared; this is the one that holds for a name it
/// assembled at run time.
///
/// **The write is checked, and a refused write is a throw.** Until it was,
/// a frozen `globalThis` put the name in the handle table, rendered it to the
/// model this turn, and left it `undefined` the next — the one failure
/// `runtime-contract.md` §2 says would make the whole channel untrustworthy,
/// reached by one line of defensive tidiness in the model's own program.
///
/// `late` marks the generated epilogue's re-read of a name, which is the one
/// caller that must not un-free anything: see [`capture_callback`].
fn capture(scope: &mut v8::PinScope, name: &str, value: v8::Local<v8::Value>, late: bool) {
    if refuse_host_name(scope, name, "bound") {
        return;
    }
    let state = state(scope);
    let context = scope.get_current_context();
    let global = context.global(scope);
    let Some(key) = v8::String::new(scope, name).map(v8::Local::<v8::Value>::from) else {
        throw_unbindable(scope, name);
        return;
    };
    global.set(scope, key, value);
    // Checked by reading the binding back, not by `set`'s own answer.
    // `v8::Object::set` performs a **sloppy-mode** store: on a frozen
    // `globalThis` it silently does nothing and still answers `Some(true)`
    // (measured — afterwards `Object.isExtensible(globalThis)` is false, the
    // name is not an own property, and `typeof globalThis.x` is
    // `"undefined"`). §2's question is only ever "will this name be there
    // next cell", and reading it back is that question exactly — a
    // non-writable property that kept its old value and a setter that stored
    // somewhere else both answer it correctly, and neither is visible in a
    // boolean.
    let bound = global
        .get(scope, key)
        .is_some_and(|read| read.same_value(value));
    if !bound {
        throw_unbindable(scope, name);
        return;
    }
    if !late {
        // §2 gives a handle three ways to die and `free` is one of them —
        // but a name the same cell binds *again* after freeing it is a
        // binding the program still names, and `Runtime::forget_freed` would
        // otherwise delete it off the persistent scope after the cell. This
        // is the cheapest recovery `RuntimeOutOfMemory`'s own message invites
        // ("call free(\"name\") on what you no longer need"), so it has to
        // leave the model holding the summary it kept.
        state
            .current
            .borrow_mut()
            .freed
            .retain(|freed| freed != name);
    }
    if !late
        && let Some(id) = handler_name(scope, value)
        && let Some(h) = state
            .handlers
            .entries
            .borrow_mut()
            .iter_mut()
            .find(|h| h.id == id && !h.name_bound)
    {
        h.info.name = name.to_string();
        h.name_bound = true;
    }
    let (preview, meta) = preview_of(scope, &state, value);
    if let Some(id) = recorded_call_id(scope, value) {
        state.note_binding(id, name);
    }
    state.capture(name, preview, meta);
}

/// A tool-produced object keeps the preview and provenance its call already
/// built; anything else is marshalled now.
pub(crate) fn preview_of(
    scope: &mut v8::PinScope,
    state: &Rc<RuntimeState>,
    value: v8::Local<v8::Value>,
) -> (Value, HandleMeta) {
    if let Some(name) = handler_name(scope, value) {
        let active = state
            .handlers
            .entries
            .borrow()
            .iter()
            .any(|h| h.id == name && h.info.active);
        return (
            Value::string(if active { "active" } else { "stale" }),
            HandleMeta {
                type_label: Some("Handler".into()),
                size_estimate: 0,
                provenance: None,
            },
        );
    }
    if let Some(call) = recorded_call(scope, state, value) {
        return (call.preview, call.meta);
    }
    let preview = marshal::marshal(scope, value);
    let meta = HandleMeta {
        type_label: None,
        size_estimate: marshal::size_estimate(scope, value),
        provenance: None,
    };
    (preview, meta)
}

/// The call a tool-produced object was tagged with, when `value` is one;
/// `None` for anything the program built itself. This is the whole of
/// "handle-aware": a reader that asks here before it reads a property can
/// answer with the call's preview and never touch the payload (§4).
pub(crate) fn recorded_call(
    scope: &mut v8::PinScope,
    state: &Rc<RuntimeState>,
    value: v8::Local<v8::Value>,
) -> Option<RecordedCall> {
    state.recorded(recorded_call_id(scope, value)?)
}

/// The id a tool-produced object was tagged with, or `None` for a value the
/// program built itself.
fn recorded_call_id(scope: &mut v8::PinScope, value: v8::Local<v8::Value>) -> Option<u64> {
    let object = v8::Local::<v8::Object>::try_from(value).ok()?;
    let tag = call_tag(scope);
    let id = object.get_private(scope, tag)?.number_value(scope)?;
    if !id.is_finite() || id < 1.0 {
        return None;
    }
    Some(id as u64)
}

fn is_identifier(name: &str) -> bool {
    let mut chars = name.chars();
    chars
        .next()
        .is_some_and(|first| first.is_alphabetic() || first == '_' || first == '$')
        && chars.all(|c| c.is_alphanumeric() || c == '_' || c == '$')
}

// --- the batch, and the background jobs that fill it --------------------

fn batch_store(scope: &v8::PinScope) -> Option<Rc<BatchStore>> {
    scope.get_slot::<Rc<BatchStore>>().cloned()
}

/// One event of the live batch, copied out of the `RefCell` before anything
/// re-enters V8.
///
/// Owned rather than borrowed on purpose: building a JS object calls back
/// into the isolate, and a `RefCell` borrow of the batch held across that
/// call would be a borrow live while a callback that also reads the batch
/// could run.
struct EventRow {
    id: EventId,
    kind: String,
    source: String,
    at: String,
    summary: String,
    age: u32,
    payload: String,
}

fn row(event: &Event, age: u32) -> EventRow {
    EventRow {
        id: event.id,
        kind: event.kind.as_str(),
        source: event.source.clone(),
        at: event.at.to_string(),
        summary: event.summary.clone(),
        age,
        payload: event.payload.as_str().to_string(),
    }
}

/// Every event of the live batch that `pick` selects, with its age.
fn selected(scope: &v8::PinScope, pick: impl FnOnce(&Batch) -> Vec<&Event>) -> Vec<EventRow> {
    let Some(store) = batch_store(scope) else {
        return Vec::new();
    };
    store
        .with(|batch| {
            let ages: std::collections::HashMap<EventId, u32> = batch
                .events()
                .into_iter()
                .map(|(event, age)| (event.id, age))
                .collect();
            pick(&*batch)
                .into_iter()
                .map(|event| row(event, ages.get(&event.id).copied().unwrap_or(0)))
                .collect()
        })
        .unwrap_or_default()
}

fn events_array<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    rows: &[EventRow],
) -> v8::Local<'s, v8::Value> {
    let array = v8::Array::new(scope, rows.len() as i32);
    for (index, row) in rows.iter().enumerate() {
        let object = v8::Object::new(scope);
        let id = v8::Number::new(scope, row.id as f64);
        set_fixed_key(scope, object, "id", id.into());
        for (key, text) in [
            ("kind", &row.kind),
            ("source", &row.source),
            ("at", &row.at),
            ("summary", &row.summary),
        ] {
            let value = js_string(scope, text);
            set_fixed_key(scope, object, key, value);
        }
        let age = v8::Number::new(scope, f64::from(row.age));
        set_fixed_key(scope, object, "age", age.into());
        // §1: a payload is materialised on first access, and §3 never
        // previews one. A method rather than a property is what makes that
        // true of this object too -- reading the event costs nothing until
        // the program asks.
        let data = js_string(scope, &row.payload);
        if let Some(function) = v8::Function::builder(payload_callback)
            .data(data)
            .build(scope)
        {
            set_fixed_key(scope, object, "payload", function.into());
        }
        array.set_index(scope, index as u32, object.into());
    }
    array.into()
}

/// `batch.where({kind, source})` -- both optional, `hook.*` matched by
/// prefix, exactly as `Batch::where_` decides.
fn batch_where_callback(
    scope: &mut v8::PinScope,
    args: v8::FunctionCallbackArguments,
    mut retval: v8::ReturnValue,
) {
    let (kind, source) = read_filter(scope, args.get(0));
    let rows = selected(scope, |batch| {
        batch.where_(kind.as_deref(), source.as_deref())
    });
    let array = events_array(scope, &rows);
    retval.set(array);
}

/// `batch.rest()` -- this batch's events not yet acked.
fn batch_rest_callback(
    scope: &mut v8::PinScope,
    _args: v8::FunctionCallbackArguments,
    mut retval: v8::ReturnValue,
) {
    let rows = selected(scope, Batch::rest);
    let array = events_array(scope, &rows);
    retval.set(array);
}

/// `batch.ack(ids)` -- an id this batch does not hold comes back in
/// `unknown` rather than being silently dropped, which is `Batch::ack`'s own
/// decision and not a second one here.
fn batch_ack_callback(
    scope: &mut v8::PinScope,
    args: v8::FunctionCallbackArguments,
    mut retval: v8::ReturnValue,
) {
    let ids = read_ids(scope, args.get(0));
    let acked = batch_store(scope)
        .and_then(|store| store.with(|batch| batch.ack(&ids)))
        .unwrap_or_else(|| crate::events::batch::Acked {
            acked: Vec::new(),
            unknown: ids,
        });
    let object = v8::Object::new(scope);
    for (key, list) in [("acked", &acked.acked), ("unknown", &acked.unknown)] {
        let array = v8::Array::new(scope, list.len() as i32);
        for (index, id) in list.iter().enumerate() {
            let value = v8::Number::new(scope, *id as f64);
            array.set_index(scope, index as u32, value.into());
        }
        set_fixed_key(scope, object, key, array.into());
    }
    retval.set(object.into());
}

fn payload_callback(
    scope: &mut v8::PinScope,
    args: v8::FunctionCallbackArguments,
    mut retval: v8::ReturnValue,
) {
    let id = args.data().to_rust_string_lossy(scope);
    let state = state(scope);
    let session = state.session.clone();
    let Some(result) = bg::payload(&session, &id) else {
        let null = v8::null(scope);
        retval.set(null.into());
        return;
    };
    let object = v8::Object::new(scope);
    let status = js_string(scope, &result.status);
    set_fixed_key(scope, object, "status", status);
    // Built one at a time rather than over an array of callbacks: rusty_v8
    // takes a zero-sized *fn item*, and an array coerces both arms to a fn
    // pointer, which fails a `const` size check inside the binding rather
    // than at this line.
    let data = js_string(scope, &id);
    if let Some(function) = v8::Function::builder(stdout_callback)
        .data(data)
        .build(scope)
    {
        set_fixed_key(scope, object, "stdout", function.into());
    }
    let data = js_string(scope, &id);
    if let Some(function) = v8::Function::builder(stderr_callback)
        .data(data)
        .build(scope)
    {
        set_fixed_key(scope, object, "stderr", function.into());
    }
    retval.set(object.into());
}

fn stdout_callback(
    scope: &mut v8::PinScope,
    args: v8::FunctionCallbackArguments,
    mut retval: v8::ReturnValue,
) {
    let text = stream(scope, &args, |result| result.stdout);
    let value = js_string(scope, &text);
    retval.set(value);
}

fn stderr_callback(
    scope: &mut v8::PinScope,
    args: v8::FunctionCallbackArguments,
    mut retval: v8::ReturnValue,
) {
    let text = stream(scope, &args, |result| result.stderr);
    let value = js_string(scope, &text);
    retval.set(value);
}

fn stream(
    scope: &mut v8::PinScope,
    args: &v8::FunctionCallbackArguments,
    pick: impl FnOnce(bg::JobResult) -> String,
) -> String {
    let id = args.data().to_rust_string_lossy(scope);
    let session = state(scope).session.clone();
    bg::payload(&session, &id).map(pick).unwrap_or_default()
}

// --- agent.run ---------------------------------------------------------

/// Phase 64's `agent.run(task, {turns, model})`.
///
/// **Both refusals happen before a handle exists**, which is `bg.run`'s own
/// rule and matters for the same reason: a program that catches the exception
/// is holding nothing, and there is no started subagent to stop.
///
/// The refusal is depth — a subagent may not start a subagent, and the runtime
/// knows which it is. `budget_remaining` is a compatibility hint for direct
/// runtime callers; Sterna sessions set it to zero (unbounded/unknown), so token
/// spend never refuses an agent call.
fn agent_run_callback(
    scope: &mut v8::PinScope,
    args: v8::FunctionCallbackArguments,
    mut retval: v8::ReturnValue,
) {
    let task = args.get(0).to_rust_string_lossy(scope);
    if task.trim().is_empty() {
        throw_tool_error(scope, "agent.run needs a task to work on");
        return;
    }
    // No default and no ceiling: absent `turns` means "until the work is
    // done" (`agent::AgentOptions`), and the wall clock is what ends a
    // subagent that never gets there.
    let turns = read_millis(scope, args.get(1), "turns");
    let asked_model = read_option(scope, args.get(1), "model");
    let asked_slot = read_option(scope, args.get(1), "slot");
    let asked_profile = read_option(scope, args.get(1), "profile");
    let asked_effort = read_option(scope, args.get(1), "effort");

    let state = state(scope);
    if state.subagent.get() {
        throw_denied(
            scope,
            &PermissionDenied {
                tool: "agent".to_string(),
                path: String::new(),
                rule: "a subagent may not start a subagent; answer the question you were given"
                    .to_string(),
            },
        );
        return;
    }

    let template = match asked_profile
        .as_deref()
        .map(|name| state.agent_templates.resolve(name))
        .transpose()
    {
        Ok(template) => template,
        Err(error) => {
            throw_tool_error(scope, &error);
            return;
        }
    };
    let effort = match asked_effort.as_deref() {
        Some(value) => match crate::wire::Effort::parse(value) {
            Some(effort) => effort,
            None => {
                throw_tool_error(
                    scope,
                    "agent effort must be auto, low, medium, high, xhigh, or max",
                );
                return;
            }
        },
        None => template
            .and_then(|template| template.effort)
            .unwrap_or_default(),
    };
    let task = template.map_or(task.clone(), |template| template.task(&task));
    let asked_model = asked_model.or_else(|| template.and_then(|template| template.model.clone()));
    let selected_model = state.agent_assignment(
        asked_model.as_deref(),
        asked_slot.as_deref(),
        effort,
        asked_effort.is_some(),
    );
    let (model, effort) = match selected_model {
        Ok(assignment) => assignment,
        Err(rule) => {
            throw_denied(
                scope,
                &PermissionDenied {
                    tool: "agent".to_string(),
                    path: String::new(),
                    rule,
                },
            );
            return;
        }
    };

    // Nonzero remains supported for direct runtime callers. Normal Sterna
    // sessions pass zero, so cumulative task spend cannot reach this guard.
    let remaining = state.budget_remaining.get();
    if remaining > 0 && remaining < MINIMUM_AGENT_BUDGET {
        throw_denied(
            scope,
            &PermissionDenied {
                tool: "agent".to_string(),
                path: String::new(),
                rule: format!(
                    "the task has {remaining} token(s) left and a subagent needs at least \
                     {MINIMUM_AGENT_BUDGET}; finish or return"
                ),
            },
        );
        return;
    }

    let options = crate::agent::AgentOptions {
        turns,
        model,
        effort,
        deadline: state.agent_deadline(),
    };
    let handle = crate::bg::agent_with_config(
        &state.profile,
        &state.session,
        &task,
        &options,
        state.effective_config.borrow().as_ref(),
    );
    trace(scope).record(CallRecord {
        tool: "agent.run".into(),
        args: [("source".into(), format!("agent/{handle}"))]
            .into_iter()
            .collect(),
        evidence: None,
        lifted_from: None,
        exit_code: None,
        repeat_of: None,
        error: None,
        ended: Ended::Ok,
    });
    let object = agent::agent_object(scope, &handle);
    retval.set(object);
}

// --- helper.<name> -----------------------------------------------------

/// What a task must have left before a subagent may start. One ordinary
/// turn's ceiling, which is the smallest amount that could produce an answer
/// rather than a truncation.
const MINIMUM_AGENT_BUDGET: u64 = crate::wire::MAX_TOKENS as u64;

// --- todo.write, todo.read ---------------------------------------------

/// `todo.write(items)` — the model's own plan, replaced whole.
///
/// The invariant: **what this stores is renderable.** Every item has text and
/// one of three statuses, checked here, because the plan is shown to the
/// person and counted in the turn line; an item that is neither would be a
/// row nothing can draw. A malformed write throws and changes nothing, so a
/// program that catches it still holds the plan it had.
fn todo_write_callback(
    scope: &mut v8::PinScope,
    args: v8::FunctionCallbackArguments,
    _retval: v8::ReturnValue,
) {
    let Ok(array) = v8::Local::<v8::Array>::try_from(args.get(0)) else {
        throw_tool_error(scope, "todo.write takes an array of {text, status}");
        return;
    };
    let mut items = Vec::with_capacity(array.length() as usize);
    for index in 0..array.length() {
        let Some(entry) = array.get_index(scope, index) else {
            continue;
        };
        let Ok(object) = v8::Local::<v8::Object>::try_from(entry) else {
            throw_tool_error(
                scope,
                &format!("todo.write item {index} is not an object with text and status"),
            );
            return;
        };
        let text = read_object_key(scope, object, "text").unwrap_or_default();
        if text.trim().is_empty() {
            throw_tool_error(scope, &format!("todo.write item {index} has no text"));
            return;
        }
        let status_text = read_object_key(scope, object, "status")
            .unwrap_or_else(|| PlanStatus::Pending.as_str().to_string());
        let Some(status) = PlanStatus::parse(&status_text) else {
            throw_tool_error(
                scope,
                &format!(
                    "todo.write item {index}: status {status_text:?} is not one of pending, \
                     active, done"
                ),
            );
            return;
        };
        items.push(PlanItem { text, status });
    }
    state(scope).set_plan(items);
}

/// `todo.read()` — the plan as it stands, as plain objects.
fn todo_read_callback(
    scope: &mut v8::PinScope,
    _args: v8::FunctionCallbackArguments,
    mut retval: v8::ReturnValue,
) {
    let plan = state(scope).plan();
    let array = v8::Array::new(scope, plan.len() as i32);
    for (index, item) in plan.iter().enumerate() {
        let object = v8::Object::new(scope);
        let text = js_string(scope, &item.text);
        set_key(scope, object, "text", text);
        let status = js_string(scope, item.status.as_str());
        set_key(scope, object, "status", status);
        array.set_index(scope, index as u32, object.into());
    }
    retval.set(array.into());
}

/// One string property of an object, or `None` when it is absent or is not a
/// string. Distinct from [`read_option`], which reads a property off an
/// options argument that may itself be absent.
fn read_object_key(
    scope: &mut v8::PinScope,
    object: v8::Local<v8::Object>,
    key: &str,
) -> Option<String> {
    let key = v8::String::new(scope, key)?;
    let value = object.get(scope, key.into())?;
    if value.is_null_or_undefined() {
        return None;
    }
    Some(value.to_rust_string_lossy(scope))
}

// --- bg.run, bg.watch, bg.cancel ---------------------------------------

/// §5's `bg.run`. The refusal is `Profile::admits_command`'s own and it
/// happens inside [`bg::run`] **before** a handle exists, so a program that
/// catches this exception is holding nothing.
fn bg_run_callback(
    scope: &mut v8::PinScope,
    args: v8::FunctionCallbackArguments,
    mut retval: v8::ReturnValue,
) {
    let command = args.get(0).to_rust_string_lossy(scope);
    let options = RunOptions {
        cwd: read_option(scope, args.get(1), "cwd"),
        env: read_option(scope, args.get(1), "env"),
        timeout_ms: read_millis(scope, args.get(1), "timeout"),
    };
    let state = state(scope);
    let instruction_args = Args::new().with("command", &command);
    if state.instruction_boundary("bg.run", &instruction_args) {
        stop_for_instructions(scope, "`bg.run`", "the job did not start");
        return;
    }
    match bg::run(&state.profile, &state.session, &command, &options) {
        Ok(handle) => {
            let object = job_object(scope, &handle);
            retval.set(object);
        }
        Err(denied) => throw_denied(scope, &denied),
    }
}

/// §5's `bg.watch`. `every` defaults to a second, which is the smallest
/// cadence a shell command can be run at without the polling itself being
/// the load.
///
/// **The default is not the bound.** A program that names its own `every` is
/// answered by `bg::watch`'s floor, which refuses a cadence under it rather
/// than clamping silently — the enforcement is there and not here so that no
/// caller of the module can get under it.
fn bg_watch_callback(
    scope: &mut v8::PinScope,
    args: v8::FunctionCallbackArguments,
    mut retval: v8::ReturnValue,
) {
    let command = args.get(0).to_rust_string_lossy(scope);
    let options = WatchOptions {
        every_ms: read_millis(scope, args.get(1), "every").unwrap_or(DEFAULT_WATCH_EVERY_MS),
        until: read_option(scope, args.get(1), "until"),
        timeout_ms: read_millis(scope, args.get(1), "timeout"),
    };
    let state = state(scope);
    let instruction_args = Args::new().with("command", &command);
    if state.instruction_boundary("bg.watch", &instruction_args) {
        stop_for_instructions(scope, "`bg.watch`", "the watcher did not start");
        return;
    }
    match bg::watch(&state.profile, &state.session, &command, &options) {
        Ok(handle) => {
            let object = job_object(scope, &handle);
            retval.set(object);
        }
        Err(denied) => throw_denied(scope, &denied),
    }
}

/// How often `bg.watch` runs its command when the model named no cadence.
const DEFAULT_WATCH_EVERY_MS: u64 = 1_000;

/// §5's `bg.cancel(handle)`: idempotent, and it takes either the object
/// `bg.run` answered with or the bare id off it, because a model that kept
/// only `job.id` should not have to reconstruct the object.
fn bg_cancel_callback(
    scope: &mut v8::PinScope,
    args: v8::FunctionCallbackArguments,
    _retval: v8::ReturnValue,
) {
    let given = args.get(0);
    let id = match v8::Local::<v8::Object>::try_from(given) {
        Ok(object) => v8::String::new(scope, "id")
            .and_then(|key| object.get(scope, key.into()))
            .map(|value| value.to_rust_string_lossy(scope))
            .unwrap_or_default(),
        Err(_) => given.to_rust_string_lossy(scope),
    };
    if id.is_empty() {
        return;
    }
    let session = state(scope).session.clone();
    bg::cancel(&session, &id);
}

fn job_object<'s>(scope: &mut v8::PinScope<'s, '_>, handle: &str) -> v8::Local<'s, v8::Value> {
    let object = v8::Object::new(scope);
    let id = js_string(scope, handle);
    set_fixed_key(scope, object, "id", id);
    let source = js_string(scope, &format!("bg/{handle}"));
    set_fixed_key(scope, object, "source", source);
    object.into()
}

/// One string property of an options object, or `None` when the object, the
/// property or its value is absent.
fn read_option(scope: &mut v8::PinScope, value: v8::Local<v8::Value>, key: &str) -> Option<String> {
    let object = v8::Local::<v8::Object>::try_from(value).ok()?;
    let key = v8::String::new(scope, key)?;
    let given = object.get(scope, key.into())?;
    if given.is_undefined() || given.is_null() {
        return None;
    }
    Some(given.to_rust_string_lossy(scope))
}

/// One millisecond count off an options object. A value that is not a finite
/// non-negative number is `None` rather than zero: a zero deadline would
/// cancel the job it was meant to bound.
fn read_millis(scope: &mut v8::PinScope, value: v8::Local<v8::Value>, key: &str) -> Option<u64> {
    let object = v8::Local::<v8::Object>::try_from(value).ok()?;
    let key = v8::String::new(scope, key)?;
    let given = object.get(scope, key.into())?;
    let number = given.number_value(scope)?;
    (number.is_finite() && number >= 1.0).then_some(number as u64)
}

/// `{kind, source}`, both optional.
fn read_filter(
    scope: &mut v8::PinScope,
    value: v8::Local<v8::Value>,
) -> (Option<String>, Option<String>) {
    (
        read_option(scope, value, "kind"),
        read_option(scope, value, "source"),
    )
}

/// An array of event ids. A value that is not a finite id is skipped here
/// rather than becoming `0`, which is not an id any window ever assigns.
fn read_ids(scope: &mut v8::PinScope, value: v8::Local<v8::Value>) -> Vec<EventId> {
    let Ok(array) = v8::Local::<v8::Array>::try_from(value) else {
        return Vec::new();
    };
    let mut ids = Vec::with_capacity(array.length() as usize);
    for index in 0..array.length() {
        if let Some(item) = array.get_index(scope, index)
            && let Some(number) = item.number_value(scope)
            && number.is_finite()
            && number >= 1.0
        {
            ids.push(number as EventId);
        }
    }
    ids
}

/// §4's delivery, in the isolate: the `batch` name, and the three methods
/// §3 gives it.
///
/// Bound with an ordinary write rather than [`set_fixed_key`], because §2's
/// replacement rule applies to this handle exactly as it does to one the
/// model bound itself -- each delivery replaces the last.
pub(crate) fn install_batch(scope: &mut v8::PinScope) {
    let context = scope.get_current_context();
    let global = context.global(scope);
    let object = v8::Object::new(scope);
    let n = batch_store(scope)
        .and_then(|store| store.with(|batch| batch.n))
        .unwrap_or(0);
    let count = v8::Number::new(scope, n as f64);
    set_fixed_key(scope, object, "n", count.into());
    if let Some(function) = v8::Function::builder(batch_where_callback).build(scope) {
        // `where` is a reserved word in JavaScript nowhere -- it is not a
        // keyword at all -- and a property name could carry it even if it
        // were, so the model writes `batch.where(...)` and the Rust name
        // stays `where_`.
        set_fixed_key(scope, object, "where", function.into());
    }
    if let Some(function) = v8::Function::builder(batch_ack_callback).build(scope) {
        set_fixed_key(scope, object, "ack", function.into());
    }
    if let Some(function) = v8::Function::builder(batch_rest_callback).build(scope) {
        set_fixed_key(scope, object, "rest", function.into());
    }
    set_key(scope, global, "batch", object.into());
}

// --- console -----------------------------------------------------------

fn throw_denied(scope: &mut v8::PinScope, denied: &PermissionDenied) {
    // The error's name already says `PermissionDenied`; a message that said
    // it again printed it twice wherever the error is shown.
    let text = denied.to_string();
    let message = js_string(
        scope,
        text.strip_prefix("PermissionDenied: ").unwrap_or(&text),
    );
    let tool = js_string(scope, &denied.tool);
    let path = js_string(scope, &denied.path);
    let rule = js_string(scope, &denied.rule);
    match construct(scope, "PermissionDenied", &[message, tool, path, rule]) {
        Some(error) => {
            scope.throw_exception(error);
        }
        None => throw_plain(scope, &denied.to_string()),
    }
}

/// Refuses a write or a free that names a host function or the runtime's own
/// `__sterna_` prefix, and answers whether it did.
///
/// A `ToolError` throw rather than a silent no-op: a model that lost `grep`
/// silently would have no way to learn it, and `runtime-contract.md` §5 makes
/// a throw the shape a refusal already has.
fn refuse_host_name(scope: &mut v8::PinScope, name: &str, verb: &str) -> bool {
    if is_host_function(name) {
        throw_tool_error(
            scope,
            &format!(
                "`{name}` is a host function and may not be {verb}; it would replace it on the \
                 persistent scope for the rest of the task and nothing could put it back"
            ),
        );
        return true;
    }
    if name.starts_with(RESERVED_PREFIX) {
        throw_tool_error(
            scope,
            &format!("`{name}` starts with `{RESERVED_PREFIX}`, which the runtime reserves"),
        );
        return true;
    }
    false
}

/// A write to the persistent scope the scope itself refused.
///
/// A `TypeError`, because that is what assigning to a frozen or non-writable
/// property throws in strict-mode JavaScript, and because the model's
/// question is about its own `globalThis`, not about a tool.
fn throw_unbindable(scope: &mut v8::PinScope, name: &str) {
    let message = format!(
        "`{name}` could not be bound on the persistent scope, so it would not be there next \
         cell; is `globalThis` frozen, or is `{name}` a non-writable property of it?"
    );
    let Some(text) = v8::String::new(scope, &message) else {
        return;
    };
    let error = v8::Exception::type_error(scope, text);
    scope.throw_exception(error);
}

fn throw_cancelled(scope: &mut v8::PinScope, tool: &str) {
    let message = js_string(
        scope,
        &ToolError::Cancelled {
            tool: tool.to_string(),
        }
        .to_string(),
    );
    let named = js_string(scope, tool);
    match construct(scope, "Cancelled", &[message, named]) {
        Some(error) => {
            scope.throw_exception(error);
        }
        None => throw_plain(scope, "the call was cancelled"),
    }
}

pub(crate) fn throw_tool_error(scope: &mut v8::PinScope, message: &str) {
    let text = js_string(scope, message);
    match construct(scope, "ToolError", &[text]) {
        Some(error) => {
            scope.throw_exception(error);
        }
        None => throw_plain(scope, message),
    }
}

/// The last resort when the bootstrap's own classes are not reachable: a
/// plain `Error`, so a refusal is never silently swallowed.
fn throw_plain(scope: &mut v8::PinScope, message: &str) {
    let Some(text) = v8::String::new(scope, message) else {
        return;
    };
    let error = v8::Exception::error(scope, text);
    scope.throw_exception(error);
}

fn construct<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    class: &str,
    args: &[v8::Local<'s, v8::Value>],
) -> Option<v8::Local<'s, v8::Value>> {
    let context = scope.get_current_context();
    let global = context.global(scope);
    let key = v8::String::new(scope, class)?;
    let found = global.get(scope, key.into())?;
    let constructor = v8::Local::<v8::Function>::try_from(found).ok()?;
    constructor.new_instance(scope, args).map(Into::into)
}

fn handler_tag<'s>(scope: &mut v8::PinScope<'s, '_>) -> v8::Local<'s, v8::Private> {
    let name = v8::String::new(scope, "sterna.handler").unwrap();
    v8::Private::for_api(scope, Some(name))
}
fn handler_name(scope: &mut v8::PinScope, value: v8::Local<v8::Value>) -> Option<String> {
    let object = v8::Local::<v8::Object>::try_from(value).ok()?;
    let tag = handler_tag(scope);
    let name = object.get_private(scope, tag)?;
    name.is_string().then(|| name.to_rust_string_lossy(scope))
}
fn handler_error(scope: &mut v8::PinScope, class: &str, message: &str) {
    let text = v8::String::new(scope, message).unwrap();
    let error = v8::Exception::error(scope, text);
    if let Ok(object) = v8::Local::<v8::Object>::try_from(error) {
        let name = js_string(scope, class);
        set_key(scope, object, "name", name);
    }
    scope.throw_exception(error);
}
fn on_callback(
    scope: &mut v8::PinScope,
    args: v8::FunctionCallbackArguments,
    mut retval: v8::ReturnValue,
) {
    let state = state(scope);
    if state.handlers.running.get() {
        handler_error(
            scope,
            "HandlerNesting",
            "a handler cannot register a handler",
        );
        return;
    }
    if state.handlers.entries.borrow().len() >= 64 {
        handler_error(
            scope,
            "HandlerLimit",
            "a task may register at most 64 handlers",
        );
        return;
    }
    if !args.get(1).is_string() {
        handler_error(
            scope,
            "TypeError",
            "on(pattern, program) requires a TypeScript program string",
        );
        return;
    }
    let (kind, source_filter) = read_filter(scope, args.get(0));
    let source = args.get(1).to_rust_string_lossy(scope);
    if source.len() > 65536 {
        handler_error(
            scope,
            "HandlerLimit",
            "a handler program may contain at most 65536 bytes",
        );
        return;
    }
    let compiled = match crate::runtime::cell::compile(&source, state.cell.get()) {
        Ok(compiled) => compiled,
        Err(_) => {
            handler_error(
                scope,
                "SyntaxError",
                "the handler program could not be compiled",
            );
            return;
        }
    };
    let Some(javascript) = v8::String::new(scope, &compiled.javascript) else {
        return;
    };
    let Some(script) = v8::Script::compile(scope, javascript, None) else {
        return;
    };
    let Some(value) = script.run(scope) else {
        return;
    };
    let Ok(program) = v8::Local::<v8::Function>::try_from(value) else {
        return;
    };
    // Pattern getters and wrapper construction can reenter JavaScript. Never
    // hold a RefCell borrow across them, and recheck at the insertion boundary.
    if state.handlers.entries.borrow().len() >= 64 {
        handler_error(
            scope,
            "HandlerLimit",
            "a task may register at most 64 handlers",
        );
        return;
    }
    let name = format!("handler{}", state.handlers.entries.borrow().len() + 1);
    state
        .handlers
        .entries
        .borrow_mut()
        .push(crate::runtime::handlers::Handler {
            registered: state.handlers.delivery.get(),
            id: name.clone(),
            name_bound: false,
            info: crate::runtime::handlers::HandlerInfo {
                name: name.clone(),
                runs: 0,
                drained: 0,
                error: None,
                active: true,
            },
            kind,
            source_filter,
            source,
            program: Some(v8::Global::new(scope, program)),
        });
    let handle = v8::Object::new(scope);
    let tag = handler_tag(scope);
    let value = js_string(scope, &name);
    handle.set_private(scope, tag, value);
    set_fixed_key(scope, handle, "name", value);
    retval.set(handle.into());
}
fn off_callback(
    scope: &mut v8::PinScope,
    args: v8::FunctionCallbackArguments,
    _retval: v8::ReturnValue,
) {
    if let Some(name) = handler_name(scope, args.get(0)) {
        state(scope).handlers.off_id(&name, None);
    } else {
        handler_error(scope, "TypeError", "off expects a Handler handle");
    }
}

#[cfg(test)]
mod tests {
    use super::search::{match_line, parse_match};
    use super::*;

    /// Session tm3hb2-1k3n: a bad `rg` pattern reached the model as
    /// "rg: regex parse error:" and nothing else.
    #[test]
    fn a_diagnosis_announced_on_one_line_keeps_the_line_that_states_it() {
        let rg = "rg: regex parse error:\n    (?:a|b(\n       ^\nerror: unclosed group\n";
        assert_eq!(
            stderr_reason(rg),
            "rg: regex parse error: error: unclosed group"
        );
        let missing = "rg: docs/x.md: No such file or directory (os error 2)\nsecond line\n";
        assert_eq!(
            stderr_reason(missing),
            "rg: docs/x.md: No such file or directory (os error 2)"
        );
        assert_eq!(stderr_reason("\n  \n"), "the child wrote nothing to stderr");
    }

    #[test]
    fn a_grep_line_splits_on_the_first_colon_that_precedes_a_line_number() {
        let found = parse_match("crates/sterna/src/lib.rs:12:pub mod runtime;", "root");
        assert_eq!(found.path, "crates/sterna/src/lib.rs");
        assert_eq!(found.line, Some(12));
        assert_eq!(found.text, "pub mod runtime;");
    }

    #[test]
    fn a_path_with_a_colon_in_it_does_not_move_the_split() {
        let found = parse_match("C:/tmp/a.rs:7:let x = 1;", "root");
        assert_eq!(found.path, "C:/tmp/a.rs");
        assert_eq!(found.line, Some(7));
        assert_eq!(found.text, "let x = 1;");
    }

    #[test]
    fn a_line_without_a_filename_takes_the_requested_path() {
        let found = parse_match("9:hit", "/tmp/one.txt");
        assert_eq!(found.path, "/tmp/one.txt");
        assert_eq!(found.line, Some(9));
        assert_eq!(found.text, "hit");
    }

    /// `grep -r` prints this routinely, and it is not a match: giving it
    /// line 0 made `new Set(hits.map(m => m.path))` — §6's own worked cell —
    /// count the searched directory as a file.
    #[test]
    fn a_line_that_is_not_a_located_match_has_no_line_number() {
        let found = parse_match("Binary file /tmp/root/bin.dat matches", "/tmp/root");
        assert_eq!(found.line, None);
        assert_eq!(found.path, "/tmp/root");
        assert_eq!(found.text, "Binary file /tmp/root/bin.dat matches");
        // And it renders as what grep said, not as a located hit.
        assert_eq!(match_line(&found), "Binary file /tmp/root/bin.dat matches");
    }

    #[test]
    fn a_yield_reason_is_one_line_and_bounded_on_a_character_boundary() {
        assert_eq!(bound_reason("two\nlines"), "two lines");
        let long: String = "é".repeat(REASON_CHARS + 5);
        let bounded = bound_reason(&long);
        assert!(bounded.starts_with(&"é".repeat(REASON_CHARS)), "{bounded}");
        assert!(
            bounded.contains("reason cut at 1,024 of 1,029 characters"),
            "{bounded}"
        );
        assert!(
            !bounded.contains(&"é".repeat(REASON_CHARS + 1)),
            "{bounded}"
        );
    }

    #[test]
    fn only_a_name_a_program_could_have_bound_is_a_handle_name() {
        assert!(is_identifier("hits"));
        assert!(is_identifier("_a$1"));
        assert!(!is_identifier(""));
        assert!(!is_identifier("1a"));
        assert!(!is_identifier("a b"));
        assert!(!is_identifier("a.b"));
    }
}
