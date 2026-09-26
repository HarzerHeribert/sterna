//! The V8 isolate a task's cells run in — `runtime-contract.md` §1, §2 and §5.
//!
//! **A [`Runtime`] cannot exist without the session's compiled `Profile`.**
//! [`Runtime::new`] takes one and there is no other constructor, no
//! `Default`, and no builder; the profile is cloned from the session's, never
//! compiled here, so `sandbox-grants.md` §1.5 — computed once, immutable for
//! the session — survives this layer intact.
//!
//! **One isolate, one context, every cell.** The context's global object *is*
//! the persistent scope: a cell's top-level bindings are copied onto it when
//! the cell ends, so cell *n + 1* reads them as free variables (§2), and
//! redeclaring a name in a later cell is a fresh function scope rather than a
//! `SyntaxError`. Nothing is evicted — the only three operations that shrink
//! the table are `HandleTable`'s own, and none of them is reachable from
//! rendering.
//!
//! **There is no event loop.** Every host function is synchronous, `await` on
//! a non-promise is legal, and the only microtasks are the promise jobs the
//! cell's own `async` wrapper enqueues; one explicit checkpoint drains them.
//! A cell that awaits something nothing can settle is answered, not hung.

use std::ffi::c_void;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, Once, PoisonError};
use std::time::{Duration, Instant};

use crate::contract::SessionId;
use crate::events::BatchStore;
use crate::events::batch::Batch;
use crate::runtime::bindings::{self, CellTrace};
use crate::runtime::cell::{self, CompiledCell, LINE_OFFSET};
use crate::runtime::handles::{self, HandleMeta};
use crate::runtime::marshal;
use crate::runtime::outcome::{
    CellOutcome, CellOutcomeKind, CellRecord, CellTurn, HandleRecord, TERMINAL_WALK_CAP, Terminal,
};
use crate::runtime::preview::{self, ErrorValue, PREVIEW_TOKEN_CAP, StackFrame, Value};
use crate::runtime::repair;
use crate::runtime::state::{HeapGuard, RuntimeState};
use crate::sandbox::profile::Profile;
use crate::tools::invoke::CancellationToken;

mod ask;
mod decide;
mod response;
mod returned;
mod watchdog;
mod web;
use watchdog::{EpilogueBudget, Watchdog, gave_up, timed_out};

/// The isolate's heap ceiling until `pane.toml` supplies one — 61F owns the
/// setting, `runtime-contract.md` §7 says so, and this is the default it
/// takes until then. Crossing it fails the **cell** with
/// `RuntimeOutOfMemory`; it never frees a handle (§2).
pub const DEFAULT_HEAP_LIMIT_BYTES: usize = 256 * 1024 * 1024;

/// How long one cell may occupy the session before the runtime stops it —
/// the same shape as [`DEFAULT_HEAP_LIMIT_BYTES`], and 61F owns the
/// configurable value.
///
/// It exists because `while (true) {}` allocates nothing, so the heap
/// ceiling never sees it: without a wall clock a cell that computes forever
/// takes the whole session with it and no later cell ever runs. Thirty
/// seconds is far longer than any cell measured here and far shorter than a
/// person waiting on a hung session.
pub const DEFAULT_CELL_WALL_CLOCK_LIMIT: Duration = Duration::from_secs(30);

// **It bounds the cell's own computing, not the work it waits for.** Time
// inside a host callback -- a granted `cargo test`, an MCP server, a helper,
// a person at a confirmation -- is subtracted (`RuntimeState::away_from_js`),
// because a build that takes four minutes is the task rather than a hang, and
// each of those waits carries its own bound already. Before that separation
// this limit reaped a running build at thirty seconds and answered the call
// `cancelled`.

/// The most bytes a returned string may be before the cell yields with the
/// cap as its reason instead of returning — `runtime-contract.md` §9.2's
/// response cap. A constant here for the same reason as the two above: 61F
/// owns the `pane.toml` setting. It is never a truncation: over it the task
/// continues and the model is told the size.
pub const DEFAULT_RESPONSE_BYTE_CAP: usize = 16 * 1024;

/// How many microtask checkpoints a cell gets before its promise is declared
/// unsettleable. One drains the whole queue, including jobs the queue's own
/// jobs enqueue; the rest are slack.
const MICROTASK_CHECKPOINTS: usize = 8;

/// How deep a stack V8 captures for a throw. Deeper than the three frames
/// `runtime-contract.md` §3 renders, so the host-frame filter in
/// [`thrown_error`] still has something to filter after the model's own
/// frames run out.
const STACK_TRACE_FRAME_LIMIT: i32 = 10;

/// How often a watchdog that has already fired asks again, until the cell
/// actually stops.
///
/// A termination request V8 does not observe is otherwise never re-issued:
/// measured on this host, `while (true) { const x = new Array(100).fill("y"); }`
/// ran for minutes against a 500 ms limit because the request landed while
/// the thread was inside `Builtin_ArrayPrototypeFill`, which checks no
/// interrupt. At this interval a lost request costs 50 ms rather than the
/// session.
const TERMINATE_RETRY_INTERVAL: Duration = Duration::from_millis(50);

/// The multiple of the wall-clock limit past which a cell that is *still*
/// running has ignored every termination the watchdog issued, and the
/// runtime stops trusting its isolate: the cell is answered as a
/// `RuntimeTimeout` naming both deadlines and [`Runtime::poisoned`] becomes
/// true, so no later cell of the task runs code in it.
///
/// `runtime-contract.md` §7 owns the wall clock itself. This multiplier and
/// [`TERMINATE_RETRY_INTERVAL`] are the watchdog's own mechanics and move to
/// `pane.toml` beside the limit when 61F takes it.
const HARD_DEADLINE_MULTIPLE: u32 = 3;

/// How long the bookkeeping *after* a cell gets before it is stopped in
/// turn.
///
/// `run_cell` re-reads every live handle when the cell ends, and reading a
/// binding runs the model's own getter or `Proxy` trap. With the cell's
/// watchdog already disarmed there, a cell that *finished* —
/// `const report = { get summary() { while (true) {} } };` — hung the whole
/// session with no `Watchdog` thread alive at all, measured through the
/// shipped binary and killed at 75 s. So the epilogue is watched too, on its
/// own clock rather than on what the cell left of its wall-clock limit: a
/// cell that used its whole budget would otherwise have every preview
/// terminated. Half a second is far more than sampling a handle table needs
/// and far less than a person waiting on a hung session; a read stopped here
/// keeps the preview it had, which is the trade `refresh_previews` was
/// already written for.
const EPILOGUE_WALL_CLOCK_LIMIT: Duration = Duration::from_millis(500);

/// The prefix every script a model authored is named with. A stack frame
/// from any other script is a host frame and is never shown (§5).
const CELL_SCRIPT_PREFIX: &str = "pane:cell:";

static V8_ONCE: Once = Once::new();

/// The V8 flags every isolate in this process is built under.
///
/// **A cell can always be stopped by a termination request, because no tier
/// that elides interrupt checks runs model code.** `terminate_execution` is
/// observed at V8's own interrupt checks, and TurboFan's code for an
/// allocating loop reaches none of them: measured on this host, with the
/// watchdog re-issuing the request every [`TERMINATE_RETRY_INTERVAL`],
/// `while (true) { const x = new Array(100).fill("y"); }` never stopped at
/// all, while `while (true) {}`, `while (true) { const x = new Array(100); }`,
/// a preallocated `a.fill("y")` and the same cell plus `Math.random()` each
/// stopped at ~305 ms against a 300 ms limit. Same binary, same cell, one
/// flag: no flag → never; `--no-maglev` → never; **`--no-turbofan` → 300 ms**;
/// `--jitless` → 311 ms.
///
/// **Maglev stays on**, so this gives up the top optimising tier and not the
/// JIT. A cell is a short program that calls tools, which that tier buys
/// almost nothing, and a cell that cannot be stopped is a session that cannot
/// be trusted — `runtime-contract.md` §2's wall clock is the whole reason
/// [`DEFAULT_CELL_WALL_CLOCK_LIMIT`] exists.
const V8_FLAGS: &str = "--no-turbofan";

/// [`initialize_v8`] for the one other place in this crate that runs a
/// model's JavaScript: [`super::reduce_run`]'s filter isolate.
///
/// It is the same initialisation and deliberately not a second one — the
/// flags above are set once per process, so a filter isolate built without
/// going through here would inherit whichever tier the first caller happened
/// to choose. `--no-turbofan` is the reason a filter can be stopped at all.
pub(crate) fn initialize_v8_for_filter() {
    initialize_v8();
}

fn initialize_v8() {
    V8_ONCE.call_once(|| {
        // Before the platform, because a flag read at initialisation is
        // ignored if it arrives after it.
        v8::V8::set_flags_from_string(V8_FLAGS);
        let platform = v8::new_default_platform(0, false).make_shared();
        v8::V8::initialize_platform(platform);
        v8::V8::initialize();
    });
}

/// The near-heap-limit callback's own state, behind the one `*mut c_void` V8
/// hands that callback: the [`HeapGuard`] the isolate is signalled through,
/// and how often the ceiling has been raised.
///
/// **A raise never refuses.** V8's rule is that answering the callback with
/// the limit it already has aborts the process, so "raise once per cell and
/// refuse after that" is a deliberate `FatalProcessOutOfMemory` for any cell
/// whose *single* allocation cannot be satisfied — measured:
/// `new Array(100000000).fill('y')` at the shipped 256 MiB ceiling killed the
/// process on three runs out of three, which takes the session with it. The
/// ceiling is instead enforced where it can be enforced without killing
/// anything: [`heap_grant`] lets the cell finish, [`Runtime::finish`] fails
/// any cell that crossed the ceiling, and
/// [`Runtime::restore_heap_limit`] puts the limit back before the next one,
/// so a raise is a cell's own emergency and never the task's new ceiling.
struct HeapWatch {
    guard: Rc<HeapGuard>,
    /// Cleared at the start of every cell by [`Runtime::run_cell`]; counts
    /// this cell's raises, which is what shrinks the next grant.
    raises_this_cell: AtomicU32,
    /// Raises since this runtime was built, which is what
    /// [`Runtime::heap_limit_raises`] answers with — the number that says
    /// how far the ceiling had to move.
    raises: AtomicU32,
    /// What [`Runtime::observe_heap_ceiling`] last saw the isolate holding.
    /// The callback cannot read the heap itself — it is handed a `*mut
    /// c_void` and two limits — so the runtime leaves the number here for
    /// [`heap_grant`] to floor its bound with.
    live_floor: AtomicUsize,
}

/// The smallest a raise ever grants. Never zero: a grant of nothing is the
/// current limit, and the current limit is the abort.
const HEAP_RAISE_FLOOR_BYTES: usize = 8 * 1024 * 1024;

/// How far one cell may push the ceiling out, as a multiple of the
/// configured one. Past it the grant drops to [`HEAP_RAISE_FLOOR_BYTES`],
/// which still is not `current_heap_limit` and so still is not an abort.
///
/// **It is a bound on growth, never below what the isolate already holds.**
/// V8 satisfies a large-object allocation without consulting
/// [`near_heap_limit`] at all, so a cell can leave a live heap many times
/// this multiple behind it with `raises = 0`; a later ordinary allocation
/// then asks for a raise the bound cannot give, and V8 answers a bound it
/// cannot reach with `FatalProcessOutOfMemory` — the process, not the cell.
/// Refusing there reclaims nothing, because the memory is already held. So
/// [`heap_grant`] floors this bound at twice
/// [`HeapWatch::live_floor`], the heap [`Runtime::observe_heap_ceiling`] last
/// measured. Measured on this host: a 16 MiB ceiling and a 240 MB array
/// aborted the process on the next cell's 4 MB allocation with the bound at
/// 16× alone, and answers `RuntimeOutOfMemory` with the floor.
const HEAP_RAISE_TOTAL_MULTIPLE: usize = 16;

/// What one raise adds: **all** of what is left of
/// [`HEAP_RAISE_TOTAL_MULTIPLE`], so the bound is reached on the first
/// callback rather than approached over several.
///
/// **The bound has to be reachable in one callback, because V8 offers about
/// two.** V8 invokes the near-heap-limit callback only while it is still
/// trying to satisfy an allocation and gives up after a handful of
/// last-resort collections, so a grant that merely *approaches* the bound
/// never arrives. Measured on this host: a cell that left a 152.8 MB live
/// heap behind a 32 MiB ceiling (`new Array(20000000).fill('y')`, which V8
/// satisfies from large-object space without ever calling this back) was
/// answered on the *next* cell's ordinary 4 MB allocation with two doubling
/// grants — 32 → 64 → 128 MiB, still under the live heap — and then
/// `Fatal JavaScript out of memory: Reached heap limit`, which is the
/// process, not the cell. Granting the remainder at once takes the limit to
/// 512 MiB on that first callback and the cell is answered with a value.
///
/// It does not weaken the ceiling and it does not raise the peak. The grant
/// was never what enforced the ceiling — [`Runtime::finish`] fails any cell
/// that crossed it, whether [`near_heap_limit`] reported the crossing or
/// [`Runtime::observe_heap_ceiling`] observed it, and
/// [`Runtime::restore_heap_limit`] puts the limit back before the next cell.
/// And the callback terminates the cell on the way in, so what a raise buys
/// is room to unwind; [`HEAP_RAISE_TOTAL_MULTIPLE`] is the same bound on how
/// far the limit can travel either way.
fn heap_grant(current_heap_limit: usize, initial_heap_limit: usize, live_floor: usize) -> usize {
    let ceiling = initial_heap_limit
        .saturating_mul(HEAP_RAISE_TOTAL_MULTIPLE)
        .max(live_floor.saturating_mul(2));
    if current_heap_limit >= ceiling {
        return HEAP_RAISE_FLOOR_BYTES;
    }
    (ceiling - current_heap_limit).max(HEAP_RAISE_FLOOR_BYTES)
}

/// V8 hands this callback a `*mut c_void` and nothing else, so the isolate it
/// must stop travels in the [`HeapWatch`] behind that pointer.
///
/// Every call raises, and every call terminates. The raise buys the
/// terminated cell room to reach the interrupt check where the termination
/// is observed; returning `current_heap_limit` instead — which is what
/// refusing a raise means to V8 — is a process abort, and this crate answers
/// an out-of-memory with a value. What keeps the ceiling a ceiling is the
/// termination this issues on the way in plus [`Runtime::finish`], which
/// fails any cell that crossed it, and [`Runtime::restore_heap_limit`],
/// which puts the limit back before the next cell — **not** the size of the
/// grant, which is a refusal V8 has no non-fatal way to express.
/// [`heap_grant`] bounds how far the limit can travel meanwhile
/// ([`HEAP_RAISE_TOTAL_MULTIPLE`]); it does not shrink, and its own doc
/// records the measurement that ruled shrinking out.
unsafe extern "C" fn near_heap_limit(
    data: *mut c_void,
    current_heap_limit: usize,
    initial_heap_limit: usize,
) -> usize {
    // SAFETY: `data` is the address of a `HeapWatch` the `Runtime` owns and
    // declares *after* its isolate, so the watch outlives every callback the
    // isolate can make.
    let watch = unsafe { &*(data as *const HeapWatch) };
    watch.guard.hit.store(true, Ordering::SeqCst);
    if let Some(isolate) = watch.guard.isolate.get() {
        isolate.terminate_execution();
    }
    watch.raises_this_cell.fetch_add(1, Ordering::SeqCst);
    watch.raises.fetch_add(1, Ordering::SeqCst);
    let live_floor = watch.live_floor.load(Ordering::SeqCst);
    current_heap_limit.saturating_add(heap_grant(
        current_heap_limit,
        initial_heap_limit,
        live_floor,
    ))
}

/// Every `ArrayBuffer` backing store in this isolate, counted and bounded.
///
/// An `ArrayBuffer`'s store is **external** memory, which V8's own heap
/// ceiling never sees: `new ArrayBuffer(1024 * 1024 * 1024)` against a 32 MiB
/// ceiling used to succeed with `raises = 0`, so anything model-authored had
/// an unmetered allocator beside a metered heap. V8 takes those stores from
/// the allocator its isolate was built with, which is this one, so this
/// comparison is where `runtime-contract.md` §2's ceiling reaches them: an
/// allocation that would cross it is **refused** rather than satisfied and
/// regretted, and [`Runtime::run_cell`] answers the cell with
/// `RuntimeOutOfMemory`.
///
/// **Nothing here calls back into V8**, which V8's allocator contract
/// forbids. It counts, it allocates, and it records that it refused; the
/// stopping is done by the runtime that reads [`ExternalMemory::refused`]
/// when the cell ends.
struct ExternalMemory {
    /// Bytes handed to V8 and not yet given back.
    live: AtomicUsize,
    /// The ceiling, which is the isolate's configured heap ceiling: one
    /// number for the model to reason about rather than two.
    limit: usize,
    /// Cleared at the start of every cell; set by a refusal that was
    /// **final** — see [`ExternalMemory::settle`].
    refused: AtomicBool,
    /// The size of the claim this refused most recently and has not yet seen
    /// satisfied, or zero. It is how a refusal V8 recovers from is told
    /// apart from one it does not; see [`ExternalMemory::settle`].
    pending: AtomicUsize,
}

/// V8's allocator interface carries no alignment, so a backing store is
/// aligned the way the `malloc`-based default allocator this replaces aligns
/// one.
const EXTERNAL_ALIGN: usize = 16;

impl ExternalMemory {
    /// The layout `len` bytes were allocated with, which is also the layout
    /// they must be freed with. `max(1)` because a zero-sized layout cannot
    /// be allocated and V8 does ask for empty buffers.
    fn layout(len: usize) -> Option<std::alloc::Layout> {
        std::alloc::Layout::from_size_align(len.max(1), EXTERNAL_ALIGN).ok()
    }

    /// Claims `len` against the ceiling, or refuses and records the size it
    /// refused.
    fn claim(&self, len: usize) -> bool {
        let mut live = self.live.load(Ordering::Acquire);
        loop {
            let next = live.saturating_add(len);
            if next > self.limit {
                self.pending.store(len.max(1), Ordering::SeqCst);
                return false;
            }
            match self
                .live
                .compare_exchange_weak(live, next, Ordering::AcqRel, Ordering::Acquire)
            {
                Ok(_) => {
                    self.settle(len);
                    return true;
                }
                Err(actual) => live = actual,
            }
        }
    }

    /// **A refusal V8 recovers from is a request for a collection, not a
    /// refusal.** [`ExternalMemory::release`] runs only when V8 frees a
    /// backing store, which needs a garbage collection, and V8 asks for one
    /// itself: an allocator that answers null makes `Heap::
    /// AllocateExternalBackingStore` collect and ask again *for the same
    /// size*, up to three more times, before it gives up and throws. Latching
    /// on the first null therefore refused a program that never held more
    /// than a megabyte at once — measured: a hundred 1 MiB `ArrayBuffer`s
    /// allocated and dropped in one cell, under a 32 MiB ceiling, answered
    /// `RuntimeOutOfMemory`, and an `ArrayBuffer` was effectively single-use
    /// up to the ceiling for the life of a task.
    ///
    /// So a claim that succeeds settles the refusal before it. The *same*
    /// size succeeding is V8's retry landing, and there was no refusal to
    /// report. A **different** size succeeding means the refused claim was
    /// never satisfied — the program went on to something smaller, which is
    /// exactly the `try { new ArrayBuffer(1 << 30) } catch {}` shape §2 does
    /// not let a cell walk away from — so that refusal is final and is
    /// latched here.
    fn settle(&self, len: usize) {
        let pending = self.pending.swap(0, Ordering::SeqCst);
        if pending != 0 && pending != len.max(1) {
            self.refused.store(true, Ordering::SeqCst);
        }
    }

    /// Whether this cell crossed the external ceiling: a refusal already
    /// final, or one still outstanding because nothing satisfied it.
    fn hit(&self) -> bool {
        self.refused.load(Ordering::SeqCst) || self.pending.load(Ordering::SeqCst) != 0
    }

    /// A refusal the previous cell was already answered with is not this
    /// cell's, and neither is a claim V8 abandoned when the cell ended.
    fn reset(&self) {
        self.refused.store(false, Ordering::SeqCst);
        self.pending.store(0, Ordering::SeqCst);
    }

    fn release(&self, len: usize) {
        self.live.fetch_sub(len, Ordering::AcqRel);
    }
}

fn external_alloc(memory: &ExternalMemory, len: usize, zeroed: bool) -> *mut c_void {
    if !memory.claim(len) {
        return std::ptr::null_mut();
    }
    let Some(layout) = ExternalMemory::layout(len) else {
        memory.release(len);
        return std::ptr::null_mut();
    };
    // SAFETY: the layout has a non-zero size and a power-of-two alignment.
    // The pointer goes straight back to V8, which returns it to
    // [`external_free`] with the same `len` and therefore the same layout.
    let ptr = unsafe {
        if zeroed {
            std::alloc::alloc_zeroed(layout)
        } else {
            std::alloc::alloc(layout)
        }
    };
    if ptr.is_null() {
        memory.release(len);
        return std::ptr::null_mut();
    }
    ptr.cast::<c_void>()
}

unsafe extern "C" fn external_allocate(memory: &ExternalMemory, len: usize) -> *mut c_void {
    external_alloc(memory, len, true)
}

unsafe extern "C" fn external_allocate_uninitialized(
    memory: &ExternalMemory,
    len: usize,
) -> *mut c_void {
    external_alloc(memory, len, false)
}

unsafe extern "C" fn external_free(memory: &ExternalMemory, data: *mut c_void, len: usize) {
    if data.is_null() {
        return;
    }
    if let Some(layout) = ExternalMemory::layout(len) {
        // SAFETY: `data` is what [`external_alloc`] returned for this `len`,
        // so this is the layout it was allocated with.
        unsafe { std::alloc::dealloc(data.cast::<u8>(), layout) };
    }
    memory.release(len);
}

unsafe extern "C" fn external_drop(memory: *const ExternalMemory) {
    // SAFETY: the pointer V8 holds is one `Arc::into_raw` produced, and V8
    // calls this exactly once, when the allocator it belongs to is destroyed.
    drop(unsafe { Arc::from_raw(memory) });
}

static EXTERNAL_VTABLE: v8::RustAllocatorVtable<ExternalMemory> = v8::RustAllocatorVtable {
    allocate: external_allocate,
    allocate_uninitialized: external_allocate_uninitialized,
    free: external_free,
    drop: external_drop,
};

/// One task's isolate.
///
/// Field order is drop order and is load-bearing: the context handle is
/// released while its isolate is alive, and the heap guard the near-heap-limit
/// callback points at outlives the isolate that could call it.
pub struct Runtime {
    context: v8::Global<v8::Context>,
    isolate: v8::OwnedIsolate,
    state: Rc<RuntimeState>,
    heap: Rc<HeapWatch>,
    /// The other half of the ceiling: what the isolate's `ArrayBuffer`
    /// allocator has handed out, and whether it refused.
    external: Arc<ExternalMemory>,
    /// The ceiling this runtime was configured with, kept because a raise
    /// moves the isolate's own and [`Runtime::restore_heap_limit`] puts this
    /// one back.
    heap_limit_bytes: usize,
    wall_clock_limit: Duration,
    /// Whether the isolate's own heap was over [`Runtime::heap_limit_bytes`]
    /// when the last cell ended, so that §2's ceiling fails **the cell that
    /// crosses it** and not every cell after it — see
    /// [`Runtime::observe_heap_ceiling`].
    heap_over_ceiling: bool,
    /// The cell that ran past [`HARD_DEADLINE_MULTIPLE`] × the wall-clock
    /// limit while being terminated every [`TERMINATE_RETRY_INTERVAL`], if
    /// one did, and which of the two things that run model code did it. Once
    /// it is set nothing in this type enters the isolate again — see
    /// [`Runtime::poisoned`].
    poisoned_by: Option<Poisoned>,
    syntax_failure: Option<repair::SyntaxFailure>,
}

impl Drop for Runtime {
    fn drop(&mut self) {
        self.state.handlers.clear();
    }
}

impl Runtime {
    /// Effective session configuration, retained for delegated child runtimes.
    pub fn with_config(self, config: crate::config::PaneConfig) -> Result<Self, String> {
        let runtime = self
            .with_helpers(config.helpers.clone())
            .with_agents(config.agents.clone())
            .with_decisions(config.decisions.clone())
            .with_web(config.web.clone())?;
        *runtime.state.effective_config.borrow_mut() = Some(config);
        Ok(runtime)
    }
    /// Installs a host-only confirmation seam for already admitted registered
    /// calls. A decision cannot override the compiled sandbox profile. The
    /// shipped session does not enable it; no cell can reach this builder.
    /// Waiting remains subject to the cell's wall-clock and cancellation limits.
    #[must_use]
    pub fn with_approval_gate(self, gate: crate::approval::Gate) -> Self {
        let clock = std::sync::Arc::clone(&self.state.host_clock);
        *self.state.approval_gate.borrow_mut() = Some(gate.with_wait_clock(clock));
        self
    }

    #[must_use]
    pub fn with_instruction_context(self) -> Self {
        self.state.enable_instruction_context();
        self
    }

    /// The `[helpers]` roster this runtime's cells may call. Not passing it
    /// leaves helpers off, which is the same answer an unset `[helpers]
    /// model` gives: a runtime nobody configured spends nothing.
    #[must_use]
    /// The default model a delegated goal runs on when the cell names none.
    pub fn with_agents(self, agents: crate::config::AgentsConfig) -> Self {
        if let Some(config) = self.state.effective_config.borrow_mut().as_mut() {
            config.agents = agents.clone();
        }
        self.state.set_agents(agents);
        self
    }

    pub fn with_helpers(self, helpers: crate::config::HelpersConfig) -> Self {
        if let Some(config) = self.state.effective_config.borrow_mut().as_mut() {
            config.helpers = helpers.clone();
        }
        self.state.set_helpers(helpers);
        self
    }

    /// Every helper call the cell that just ran made, in call order — the one
    /// field `CellView.helpers` is built from.
    pub fn helper_records(&self) -> Vec<crate::helpers::HelperRecord> {
        self.state.helper_records()
    }

    pub fn pending_instructions(
        &self,
    ) -> Option<crate::runtime::instructions::PendingInstructions> {
        self.state.pending_instructions()
    }

    pub fn acknowledge_instructions(&self) {
        self.state.acknowledge_instructions();
    }

    /// Builds a runtime for one task against the session's compiled profile.
    ///
    /// There is no constructor that does not take a [`Profile`], which is
    /// what makes "model-authored code never runs outside a sandbox" a
    /// property of the type rather than of a review:
    ///
    /// ```compile_fail
    /// use pane::runtime::isolate::Runtime;
    /// let _ = Runtime::new();
    /// ```
    ///
    /// and there is no `Default` to reach for either:
    ///
    /// ```compile_fail
    /// use pane::runtime::isolate::Runtime;
    /// let _: Runtime = Default::default();
    /// ```
    pub fn new(profile: &Profile, session: &SessionId) -> Self {
        Self::with_heap_limit(profile, session, DEFAULT_HEAP_LIMIT_BYTES)
    }

    /// [`Runtime::new`] with an explicit ceiling, so a test can reach the
    /// out-of-memory path without allocating 256 MiB.
    pub fn with_heap_limit(
        profile: &Profile,
        session: &SessionId,
        heap_limit_bytes: usize,
    ) -> Self {
        Self::with_limits(
            profile,
            session,
            heap_limit_bytes,
            DEFAULT_CELL_WALL_CLOCK_LIMIT,
        )
    }

    /// [`Runtime::new`] for one helper's nested loop, and the whole of what
    /// makes a helper's toolset a capability boundary rather than a list.
    ///
    /// The invariant: **a helper's runtime holds only what its spec named,
    /// and nothing that can cause an effect.** `spec.tools` narrows
    /// `registry::ALL`; `bg`, `send` and `mcp` are not in it, so narrowing
    /// the toolset alone left a helper able to execute a command.
    ///
    /// `tools` is the spec's own list, and a name absent from it is a name
    /// this context does not bind — a helper that named no tool reaches
    /// nothing at all.
    pub fn for_helper(
        profile: &Profile,
        session: &SessionId,
        tools: &'static [&'static str],
    ) -> Self {
        Self::with_limits_and_globals(
            profile,
            session,
            DEFAULT_HEAP_LIMIT_BYTES,
            DEFAULT_CELL_WALL_CLOCK_LIMIT,
            bindings::HostGlobals::Helper(tools),
        )
    }

    /// Both ceilings explicitly, so a test can reach the timeout path
    /// without waiting out the default.
    pub fn with_limits(
        profile: &Profile,
        session: &SessionId,
        heap_limit_bytes: usize,
        wall_clock_limit: Duration,
    ) -> Self {
        Self::with_limits_and_globals(
            profile,
            session,
            heap_limit_bytes,
            wall_clock_limit,
            bindings::HostGlobals::Every,
        )
    }

    /// The one constructor. Every public one above reaches it, and `globals`
    /// is the only thing they disagree about.
    fn with_limits_and_globals(
        profile: &Profile,
        session: &SessionId,
        heap_limit_bytes: usize,
        wall_clock_limit: Duration,
        globals: bindings::HostGlobals,
    ) -> Self {
        initialize_v8();
        let external = Arc::new(ExternalMemory {
            live: AtomicUsize::new(0),
            limit: heap_limit_bytes,
            refused: AtomicBool::new(false),
            pending: AtomicUsize::new(0),
        });
        // SAFETY: the handle is an `Arc` reference this runtime keeps a
        // second one of, and `external_drop` gives exactly that reference
        // back when V8 destroys the allocator.
        let allocator = unsafe {
            v8::new_rust_allocator(Arc::into_raw(Arc::clone(&external)), &EXTERNAL_VTABLE)
        };
        let mut isolate = v8::Isolate::new(
            v8::CreateParams::default()
                .heap_limits(0, heap_limit_bytes)
                .array_buffer_allocator(allocator),
        );
        isolate.set_microtasks_policy(v8::MicrotasksPolicy::Explicit);
        // `Atomics.wait` blocks inside V8 where an interrupt may never land,
        // so the watchdog is not the answer to it: this line is, and measured
        // it is sufficient on its own — with both globals present, the
        // verifier's cell throws `TypeError: Atomics.wait cannot be called in
        // this context` at the model's own line in 665 µs. The bootstrap
        // deletes the two globals as well, because a single-threaded isolate
        // with no workers has nothing to share memory with and because
        // `the_isolate_has_no_ambient_authority` can then enumerate them.
        isolate.set_allow_atomics_wait(false);
        // Without this V8 captures no structured trace at all:
        // `v8::Exception::get_stack_trace` answers `None` for every throw, so
        // §5's "the top three in-program frames" was an empty list on every
        // error a model has ever been shown, and the two assertions that
        // looked like they checked it were quantifiers over that empty list.
        isolate.set_capture_stack_trace_for_uncaught_exceptions(true, STACK_TRACE_FRAME_LIMIT);

        let guard = HeapGuard::new();
        let _ = guard.isolate.set(isolate.thread_safe_handle());
        let heap = Rc::new(HeapWatch {
            guard,
            raises_this_cell: AtomicU32::new(0),
            raises: AtomicU32::new(0),
            live_floor: AtomicUsize::new(0),
        });
        isolate.add_near_heap_limit_callback(
            near_heap_limit,
            Rc::as_ptr(&heap).cast_mut().cast::<c_void>(),
        );

        let state = Rc::new(RuntimeState::new(profile, session).with_globals(globals));
        isolate.set_slot(state.clone());

        let context = {
            v8::scope!(let handle_scope, &mut isolate);
            let context = v8::Context::new(handle_scope, v8::ContextOptions::default());
            let scope = &mut v8::ContextScope::new(handle_scope, context);
            bindings::install(scope, globals);
            if let Some(source) = v8::String::new(scope, bindings::BOOTSTRAP)
                && let Some(script) = v8::Script::compile(scope, source, None)
            {
                script.run(scope);
            }
            v8::Global::new(scope, context)
        };

        Self {
            context,
            isolate,
            state,
            heap,
            external,
            heap_limit_bytes,
            wall_clock_limit,
            heap_over_ceiling: false,
            poisoned_by: None,
            syntax_failure: None,
        }
    }

    /// [`DEFAULT_RESPONSE_BYTE_CAP`] replaced, so a test can reach the
    /// over-cap yield without building sixteen kilobytes.
    #[must_use]
    pub fn with_response_byte_cap(self, bytes: usize) -> Self {
        let mut runtime = self;
        runtime.trace().response_byte_cap.set(bytes);
        runtime
    }

    /// §9's trajectory, yield request and response cap, in the isolate's
    /// second slot -- keyed by its type, beside the state rather than in it,
    /// and installed on first use so the constructor is unchanged.
    fn trace(&mut self) -> Rc<CellTrace> {
        if let Some(trace) = self.isolate.get_slot::<Rc<CellTrace>>() {
            return trace.clone();
        }
        let trace = CellTrace::new();
        self.isolate.set_slot(trace.clone());
        trace
    }

    /// The token every tool call this runtime makes becomes cancellable
    /// through — a builder, so [`Runtime::new`]'s signature is unchanged and
    /// a caller that has nothing to cancel goes on writing what it wrote.
    ///
    /// A cancelled call is `runtime-contract.md` §5's throw, class
    /// `Cancelled`, in the turn slot a yield would have used. Setting the
    /// token widens nothing and starts nothing: `tools::invoke` either
    /// returns before a child exists or kills one that does.
    #[must_use]
    pub fn with_token(self, token: CancellationToken) -> Self {
        // `let mut runtime = self` rather than `mut self` in the signature,
        // the same shape `with_response_byte_cap` above uses: it keeps this a
        // builder over an already-built runtime, which is the property
        // `runtime_cells::the_runtime_cannot_be_built_without_a_profile`
        // reads off this line.
        let mut runtime = self;
        runtime.set_token(token);
        runtime
    }

    /// The token the **next** cell's calls are cancellable through, replacing
    /// whatever [`with_token`](Self::with_token) or an earlier call installed.
    ///
    /// **A cancelled token has no way back, so a task hands each cell a fresh
    /// one.** [`CancellationToken::cancel`] is deliberately one-way — a token
    /// is one call's decision, not a reusable switch — so a session that kept
    /// one token for the whole task would answer every cell after the first
    /// cancellation with an instant `Cancelled` throw. This is how one Ctrl-C
    /// cancels one cell's calls; `session.rs` is its only caller.
    pub fn set_token(&mut self, token: CancellationToken) {
        *self.state.token.borrow_mut() = token;
    }

    /// The number of the cell that ran last; 0 before the first.
    pub fn cell(&self) -> u64 {
        self.state.cell.get()
    }

    /// Whether this runtime has stopped trusting its isolate.
    ///
    /// It is set by exactly one event: a cell that went on running past
    /// [`HARD_DEADLINE_MULTIPLE`] × the wall-clock limit while the watchdog
    /// terminated it every [`TERMINATE_RETRY_INTERVAL`]. A cell that ignored
    /// that many requests is running code the runtime cannot stop, so from
    /// then on [`Runtime::run_cell`] answers a throw naming the cell that did
    /// it **without entering the isolate**, and [`Runtime::end_task`] drops
    /// the handles without entering it either.
    pub fn poisoned(&self) -> bool {
        self.poisoned_by.is_some()
    }

    /// How many times the near-heap-limit callback has raised this isolate's
    /// ceiling since it was built — the number that says how far
    /// `DEFAULT_HEAP_LIMIT_BYTES` had to move for a cell to be stopped.
    /// Each raise is smaller than the last ([`heap_grant`]), so a handful of
    /// them is still a ceiling and not a first instalment.
    pub fn heap_limit_raises(&self) -> u32 {
        self.heap.raises.load(Ordering::SeqCst)
    }

    pub fn is_live(&self, name: &str) -> bool {
        self.state.table.borrow().is_live(name)
    }

    /// How many pure calls this task repeated byte for byte — the count
    /// behind every `repeat_of` in the trajectory. Reset at task end.
    pub fn observation_repeats(&self) -> usize {
        self.state.repeated_observations()
    }

    /// The pushed reducer's work this task. Reset at task end.
    pub fn reduction_stats(&self) -> crate::runtime::observation::ReductionStats {
        self.state.reduction_stats()
    }

    /// The version of `path` an implicit `edit` would bind to right now: the
    /// last complete context that reached the model, or the last version
    /// pane itself wrote.
    pub fn visible_version(&self, path: &std::path::Path) -> Option<String> {
        self.state.visible_version(path)
    }

    /// The model's own plan for this task, for the checkpoint a compaction
    /// writes. Task-scoped like the handles beside it.
    pub fn plan(&self) -> Vec<crate::runtime::outcome::PlanItem> {
        self.state.plan()
    }

    pub fn handle_names(&self) -> Vec<String> {
        self.state
            .table
            .borrow()
            .names()
            .into_iter()
            .map(str::to_string)
            .collect()
    }

    /// The turn's whole rendering of the handle table.
    pub fn render_handles(&self) -> String {
        handles::render_table(
            &self.state.table.borrow(),
            preview::PREVIEW_TOKEN_CAP,
            preview::TABLE_TOKEN_CAP,
        )
    }

    /// `events-contract.md` §4's delivery: the closed window's batch becomes
    /// the handle table's **last** row, named `batch`, and the one name this
    /// runtime declares in the model's own scope.
    ///
    /// Last is not a rule applied here — [`handles::HandleTable::declare_with`]
    /// appends, and every name the model bound was appended before this call,
    /// so the model's own bindings keep the order it made them in. §2's
    /// replacement rule is likewise the table's own: a second delivery frees
    /// the first and renders `batch  (replaced at cell N)`.
    ///
    /// Answers with the batch this delivery replaced, so the **session** can
    /// roll its unacked events into the next window (§3). The runtime owns no
    /// window and decides nothing about what carries forward; handing the old
    /// batch back rather than rolling it here is what keeps that true.
    pub fn deliver_batch(&mut self, batch: Batch) -> Option<Batch> {
        self.state
            .handlers
            .delivery
            .set(self.state.handlers.delivery.get() + 1);
        self.state.handlers.processed.set(false);
        let entry = batch.preview(PREVIEW_TOKEN_CAP);
        let cell = self.state.cell.get();
        let store = self.batch_store();
        let previous = store.replace(batch);

        // Delivery changes only Rust-owned state. The next program refreshes
        // the JS binding inside its watchdog and exception boundary: even a
        // seemingly ordinary global assignment can invoke a model's setter.

        self.state.table.borrow_mut().declare_rendered(
            "batch",
            Value::string(&entry),
            cell,
            HandleMeta {
                type_label: Some("Events.Batch".to_string()),
                size_estimate: entry.len() as u64,
                provenance: None,
            },
            entry,
        );
        previous
    }

    /// Runs each matching standing program at most once for this delivery.
    /// Call before composing model feedback. The remaining batch is the only
    /// delivery path, including after a partially acknowledged failure.
    pub fn run_handlers(&mut self) -> Vec<(String, CellOutcome)> {
        let handlers = self.state.handlers.clone();
        if handlers.processed.replace(true) {
            return Vec::new();
        }
        let Some(store) = self.isolate.get_slot::<Rc<BatchStore>>().cloned() else {
            return Vec::new();
        };
        let mut runs = Vec::new();
        let count = handlers.entries.borrow().len();
        for index in 0..count {
            let candidate = {
                let entries = handlers.entries.borrow();
                let h = &entries[index];
                let matches = h.registered < handlers.delivery.get()
                    && h.info.active
                    && store
                        .with(|batch| {
                            !batch
                                .where_(h.kind.as_deref(), h.source_filter.as_deref())
                                .is_empty()
                        })
                        .unwrap_or(false);
                if matches {
                    h.program.as_ref().map(|program| {
                        (
                            h.id.clone(),
                            h.info.name.clone(),
                            h.source.clone(),
                            program.clone(),
                        )
                    })
                } else {
                    None
                }
            };
            let Some((id, name, source, program)) = candidate else {
                continue;
            };
            handlers.running.set(true);
            let outcome = self.run_program(&source, Some(program));
            handlers.running.set(false);
            {
                let mut entries = handlers.entries.borrow_mut();
                entries[index].info.runs += 1;
                if store.with(|batch| batch.rest().is_empty()).unwrap_or(false) {
                    entries[index].info.drained += 1;
                }
            }
            let failed = if let CellOutcome::Threw { error, .. } = &outcome {
                Some(error.class.clone())
            } else {
                outcome
                    .turn()
                    .record
                    .calls
                    .iter()
                    .find_map(|call| match &call.ended {
                        crate::runtime::outcome::Ended::Denied { .. } => {
                            Some("PermissionDenied".into())
                        }
                        crate::runtime::outcome::Ended::Threw { class } if class == "Cancelled" => {
                            Some(class.clone())
                        }
                        _ => None,
                    })
            };
            if let Some(class) = failed {
                handlers.off_id(&id, Some(class));
            }
            store.with(|batch| batch.retain_unacked());
            runs.push((name, outcome));
        }
        // The next program refreshes the JS batch API inside its watchdog.
        // Updating only the host preview here cannot run a model's accessor.
        if store.with(|batch| batch.n == 0).unwrap_or(false) {
            self.state.table.borrow_mut().free("batch");
        } else if let Some(preview) = store.with(|batch| batch.preview(PREVIEW_TOKEN_CAP)) {
            self.state.table.borrow_mut().declare_rendered(
                "batch",
                Value::string(&preview),
                self.cell(),
                HandleMeta {
                    type_label: Some("Events.Batch".into()),
                    size_estimate: preview.len() as u64,
                    provenance: None,
                },
                preview,
            );
        }
        runs
    }

    pub fn handlers(&self) -> Vec<crate::runtime::handlers::HandlerInfo> {
        self.state
            .handlers
            .entries
            .borrow()
            .iter()
            .map(|h| h.info.clone())
            .collect()
    }
    pub fn off_handler(&mut self, name: &str) -> bool {
        self.state.handlers.off(name, None)
    }
    pub fn take_handler_notices(&mut self) -> Vec<String> {
        self.state.handlers.notices.take()
    }

    pub fn take_batch(&mut self) -> Option<Batch> {
        self.state.table.borrow_mut().free("batch");
        self.isolate.get_slot::<Rc<BatchStore>>()?.take()
    }

    pub fn batch_rolling_depth(&mut self) -> usize {
        self.isolate
            .get_slot::<Rc<BatchStore>>()
            .and_then(|store| store.with(|batch| batch.rolling_depth()))
            .unwrap_or(0)
    }

    pub fn batch_remaining(&mut self) -> usize {
        self.isolate
            .get_slot::<Rc<BatchStore>>()
            .and_then(|store| store.with(|batch| batch.rest().len()))
            .unwrap_or(0)
    }

    /// The live batch's Rust side, in the isolate's third slot — installed on
    /// first delivery, so a session that raises no event never allocates one
    /// and the constructor is unchanged.
    fn batch_store(&mut self) -> Rc<BatchStore> {
        if let Some(store) = self.isolate.get_slot::<Rc<BatchStore>>() {
            return store.clone();
        }
        let store = BatchStore::new();
        self.isolate.set_slot(store.clone());
        store
    }

    /// The task ending — `runtime-contract.md` §2's third and last lifetime
    /// event, and the only one this type performs itself.
    pub fn end_task(&mut self) {
        self.syntax_failure = None;
        self.state.handlers.clear();
        // A poisoned runtime's isolate ignored every termination the watchdog
        // issued, and [`Runtime::poisoned`] promises nothing re-enters it.
        // The handles go with the task either way: the table below is the
        // host's own record, and the persistent scope dies with the isolate.
        if !self.poisoned() {
            let names = self.handle_names();
            v8::scope!(let handle_scope, &mut self.isolate);
            let context = v8::Local::new(handle_scope, &self.context);
            let scope = &mut v8::ContextScope::new(handle_scope, context);
            let global = context.global(scope);
            for name in &names {
                if let Some(key) = v8::String::new(scope, name) {
                    global.delete(scope, key.into());
                }
            }
        }
        self.state.table.borrow_mut().end_task();
        self.state.forget_calls();
        crate::runtime::checks::clear(&mut self.isolate);
        // The batch is a handle, so the task ending frees it with the rest --
        // §2's third lifetime event applies to the one handle the runtime
        // declared exactly as it does to the ones the model did.
        if let Some(store) = self.isolate.get_slot::<Rc<BatchStore>>() {
            store.clear();
        }
    }

    /// The most recent parse failure, until another cell attempt or task end.
    pub fn syntax_failure(&self) -> Option<&repair::SyntaxFailure> {
        self.syntax_failure.as_ref()
    }

    /// The task's model and a legacy remaining-token hint. Pane sessions pass
    /// `0`, meaning unbounded/unknown, because task token spend is telemetry
    /// and never refuses a subagent. Called once per turn.
    pub fn set_task_context(&mut self, remaining: u64, model: &str) {
        self.state.budget_remaining.set(remaining);
        *self.state.model.borrow_mut() = model.to_string();
    }

    /// Marks this runtime as a subagent's, which is what refuses a nested
    /// `agent.run`.
    ///
    /// Phase 64's last line: a subagent has no subagent of its own. The depth
    /// is a flag rather than a counter because there is no second rung to
    /// count to — one level is the whole ladder, and a counter would invite a
    /// caller to raise it.
    pub fn as_subagent(self) -> Self {
        self.state.subagent.set(true);
        self
    }

    /// Every name the global object carries: the host functions, the
    /// JavaScript built-ins, and every live handle.
    ///
    /// Read from the isolate rather than kept as a list, because a list would
    /// be a second answer to a question the isolate already answers exactly —
    /// and a stale one would accuse a correct program.
    fn global_names(&mut self) -> std::collections::BTreeSet<String> {
        let mut names = std::collections::BTreeSet::new();
        if self.poisoned() {
            return names;
        }
        v8::scope!(let handle_scope, &mut self.isolate);
        let context = v8::Local::new(handle_scope, &self.context);
        let scope = &mut v8::ContextScope::new(handle_scope, context);
        let global = context.global(scope);
        // **`ALL_PROPERTIES`, not the default.** `GetPropertyNamesArgs`
        // defaults to `ONLY_ENUMERABLE`, and every JavaScript built-in on
        // `globalThis` -- `Set`, `JSON`, `Math`, `Promise` -- is
        // non-enumerable by specification. With the default this set omits
        // them and a correct `new Set(...)` is accused of using an undefined
        // name; the pane test suite caught exactly that within a minute.
        let args = v8::GetPropertyNamesArgsBuilder::new()
            .mode(v8::KeyCollectionMode::OwnOnly)
            .property_filter(v8::PropertyFilter::ALL_PROPERTIES | v8::PropertyFilter::SKIP_SYMBOLS)
            .index_filter(v8::IndexFilter::SkipIndices)
            .build();
        if let Some(array) = global.get_own_property_names(scope, args) {
            for index in 0..array.length() {
                if let Some(value) = array.get_index(scope, index) {
                    names.insert(value.to_rust_string_lossy(scope));
                }
            }
        }
        names
    }

    /// The first name the cell reads that nothing in the program binds and
    /// the global object does not carry, or `None`.
    fn first_undefined_name(&mut self, compiled: &cell::CompiledCell) -> Option<(String, u32)> {
        if compiled.free_names.is_empty() {
            return None;
        }
        let mut globals = self.global_names();
        // This host binding is refreshed under the execution watchdog, after
        // this compile-time check. Deleting it must not hide that declaration.
        globals.insert("batch".into());
        compiled
            .free_names
            .iter()
            .find(|(name, _)| !globals.contains(name))
            .cloned()
    }

    /// Runs one cell and answers with what it produced.
    ///
    /// Never a `Result`: a throw is a result (§5), a refusal is a throw
    /// (`sandbox-grants.md` §1.4), and a program that will not compile is a
    /// throw in the same turn slot. Nothing about a cell is an error of the
    /// runtime's.
    /// One cell the model sent, with one chance at having its punctuation
    /// repaired before the failure costs the parent a turn — see
    /// [`crate::runtime::repair::mend`] for what that is and why the single
    /// attempt is structural rather than counted.
    pub fn run_cell(&mut self, source: &str) -> CellOutcome {
        let outcome = self.run_program(source, None);
        let failed = self.syntax_failure.as_ref();
        let Some(mended) = repair::mend_after(&self.state, failed, &outcome) else {
            return outcome;
        };
        let mut repaired = self.run_program(&mended.amended, None);
        repair::announce(&mut repaired, &mended.note);
        repaired
    }

    /// Runs one frame lowered from direct provider tool calls.
    ///
    /// The only difference from [`Self::run_cell`] is that this frame's
    /// capability results are captured so each provider `tool_result` can
    /// carry one — there is no second executor and no second path
    /// (`tool-abi.md` §1). Capture is turned off again immediately, so an
    /// authored cell that follows is unaffected even if this frame threw.
    /// The ordinal the next cell will take, for a caller lowering direct
    /// provider calls into a frame whose binding names must be known before
    /// it runs.
    pub fn next_cell(&self) -> u64 {
        self.state.next_cell()
    }

    pub fn run_direct_frame(&mut self, source: &str) -> CellOutcome {
        self.trace().capture_results(true);
        let outcome = self.run_program(source, None);
        self.trace().capture_results(false);
        outcome
    }

    /// Frees every name in `names` whose value is `undefined`.
    ///
    /// The invariant: **only a successful direct call leaves a handle.** A
    /// lowered frame declares each call's binding ahead of the guarded call,
    /// so a call that threw leaves its name bound to `undefined`; that is not
    /// an observation, and a handle to it would be listed, rendered and
    /// resumable as if it were one. Called by `finish` for a direct frame,
    /// after the captures reach the table and before the record is built.
    fn free_undefined_direct_bindings(&mut self, names: &[String]) {
        let undefined: Vec<String> = names
            .iter()
            .filter(|name| matches!(self.state.table.borrow().get(name), Some(Value::Undefined)))
            .cloned()
            .collect();
        if undefined.is_empty() {
            return;
        }
        for name in &undefined {
            self.state.table.borrow_mut().free(name);
        }
        v8::scope!(let handle_scope, &mut self.isolate);
        let context = v8::Local::new(handle_scope, &self.context);
        let scope = &mut v8::ContextScope::new(handle_scope, context);
        let global = context.global(scope);
        for name in &undefined {
            if let Some(key) = v8::String::new(scope, name) {
                global.delete(scope, key.into());
            }
        }
    }

    fn run_program(
        &mut self,
        source: &str,
        saved: Option<v8::Global<v8::Function>>,
    ) -> CellOutcome {
        // Only a cell the model sent consumes the repair offer. A standing
        // handler arrives here too (`saved`), between the failed cell and the
        // model's `pane-edit`, and revoking the offer would answer the repair
        // pane itself asked for with "no syntax-failed cell is available".
        if saved.is_none() {
            self.syntax_failure = None;
        }
        let started = Instant::now();
        let cell = self.state.begin_cell();
        self.state.table.borrow_mut().begin_cell(cell);
        self.trace().begin_cell();
        // Before anything touches V8: an isolate that ignored every
        // termination is one this runtime no longer runs code in, and the
        // model is told which cell did it rather than being handed a hang.
        if let Some(by) = self.poisoned_by {
            return self.finish(
                cell,
                source,
                started,
                Ending::Threw(poisoned_error(by)),
                Stopped::none(),
                &EpilogueBudget::unwatched(),
            );
        }
        // A hit the previous cell did not consume — one raised while its own
        // previews were being taken — is not this cell's reason for stopping.
        self.heap.guard.take_hit();
        // Whatever the last cell had to be granted, this one starts at the
        // configured ceiling again — otherwise one bad cell would leave the
        // whole task running against a ceiling sixteen times the one §7
        // names.
        self.restore_heap_limit();
        self.external.reset();

        let compiled = match if saved.is_some() {
            Ok(CompiledCell {
                javascript: String::new(),
                declared: Vec::new(),
                script_name: String::new(),
                free_names: Vec::new(),
            })
        } else {
            cell::compile(source, cell)
        } {
            Ok(compiled) => compiled,
            Err(error) => {
                if let cell::CellError::Parse { line, column, .. } = &error {
                    self.syntax_failure = repair::SyntaxFailure::new(cell, source, *line, *column);
                }
                let value = cell::error_value(&error);
                return self.finish(
                    cell,
                    source,
                    started,
                    Ending::Threw(value),
                    Stopped::none(),
                    &EpilogueBudget::unwatched(),
                );
            }
        };

        // **Before anything runs.** A name the cell reads, binds nowhere and
        // the global object does not carry is a `ReferenceError` waiting to
        // happen -- and one that costs a whole turn to discover, since the
        // model only sees it in the next result. Reported here instead, with
        // the model's own line and column, in the turn that wrote it.
        if let Some((name, offset)) = self.first_undefined_name(&compiled) {
            let (line, column) = cell::line_and_column(source, offset);
            let value = cell::error_value(&cell::CellError::UndefinedName { name, line, column });
            return self.finish(
                cell,
                source,
                started,
                Ending::Threw(value),
                Stopped::none(),
                &EpilogueBudget::unwatched(),
            );
        }

        // **The command lines this program already spells out, judged
        // together before it runs.** Every one of them would otherwise meet
        // the `auto` rung one at a time, mid-cell, each paying its own wait
        // on the decision model. Nothing is granted and nobody is asked
        // here: the answers land in the gate's own memory, keyed by the same
        // exact line, so the calls that follow read them instead of asking
        // again. A computed line is not read out at all and meets the gate
        // as it always did.
        if saved.is_none()
            && !self.state.subagent.get()
            && let Some(gate) = self.state.approval_gate.borrow().as_ref()
        {
            let lines = crate::runtime::commands::literal_lines(source);
            if !lines.is_empty() {
                gate.prejudge(&lines);
            }
        }

        let watchdog = Watchdog::arm_pausing(
            self.heap.guard.isolate.get().cloned(),
            self.wall_clock_limit,
            self.state
                .handlers
                .running
                .get()
                .then(|| self.state.token.borrow().clone()),
            Some(std::sync::Arc::clone(&self.state.host_clock)),
        );
        *self.state.watchdog_fired.borrow_mut() = Some(Arc::clone(&watchdog.fired));
        let ending =
            if self.state.handlers.running.get() && self.state.token.borrow().is_cancelled() {
                Ending::Threw(plain_error("Cancelled", "the handler was cancelled"))
            } else {
                self.execute(&compiled, saved.as_ref())
            };
        self.state.watchdog_fired.borrow_mut().take();
        // Both halves are read here, before anything below allocates: taking
        // a preview can itself raise the heap callback, and a hit raised by
        // the runtime's own bookkeeping is not why the cell stopped.
        let disarmed = watchdog.disarm();
        let mut stopped = Stopped {
            timed_out: disarmed.fired,
            heap_hit: self.heap.guard.take_hit(),
            external_hit: self.external.hit(),
            heap_crossed: false,
            yielded: self.trace().take_yield(),
        };
        // Unconditionally, and it is not tidiness. Until a termination is
        // cancelled every V8 call below it bails out, so the previews would
        // stay the ones the declaration lines took and the out-of-memory list
        // would rank by them. Unconditional because the watchdog can also
        // fire in the instant between `execute` returning and `disarm` taking
        // the lock: the cell ends normally and a termination nobody asked for
        // is left pending on the isolate, which the *next* cell would run
        // into. A cancel with nothing pending is a no-op that returns false.
        self.isolate.cancel_terminate_execution();

        // Everything below re-enters the isolate and therefore runs the
        // model's own accessors, so it gets a watchdog of its own — see
        // [`EPILOGUE_WALL_CLOCK_LIMIT`]. `finish` is inside it because
        // `out_of_memory` sizes every live handle, and sizing a `Proxy` runs
        // its `ownKeys` trap. The [`EpilogueBudget`] is the other half of
        // that watchdog and the reason the deadline is one the *epilogue*
        // has rather than one every live handle has: without it the loops
        // below enter V8 once per name and pay `TERMINATE_RETRY_INTERVAL`
        // again for each, so thirty handles cost thirty times what one does.
        let epilogue = Watchdog::arm(
            self.heap.guard.isolate.get().cloned(),
            EPILOGUE_WALL_CLOCK_LIMIT,
        );
        *self.state.watchdog_fired.borrow_mut() = Some(Arc::clone(&epilogue.fired));
        let budget = EpilogueBudget::of(&epilogue);
        self.forget_freed();
        self.refresh_previews(&budget);
        // Once more, because the refresh above re-reads the model's own
        // values and a getter is the model's own code: one that calls
        // `yieldNow` or fills the heap there requests a termination after the
        // cancel above, and a request nobody cancels is honoured by the next
        // cell's first statement. The refresh already saw every effect of it
        // (the read answered with nothing); this is only the flag.
        self.isolate.cancel_terminate_execution();
        // After the previews and before the answer: the previews are the
        // model's own getters, so an allocation one of them makes belongs to
        // the cell whose table was being read, and reading the heap costs no
        // allocation and runs no code that a termination could be pending
        // against.
        stopped.heap_crossed = self.observe_heap_ceiling();
        // The cell did stop, or this line would not be running — but it took
        // more than the hard deadline of terminations to do it, so this is
        // the last cell that runs in this isolate. Reported ahead of the heap
        // hit: a ceiling that fired is recoverable and this is not.
        let outcome = if disarmed.gave_up {
            self.poisoned_by = Some(Poisoned {
                cell,
                cause: PoisonCause::Cell,
            });
            let error = gave_up(
                started.elapsed(),
                self.wall_clock_limit,
                self.wall_clock_limit.saturating_mul(HARD_DEADLINE_MULTIPLE),
            );
            self.finish(
                cell,
                source,
                started,
                Ending::Threw(error),
                Stopped::none(),
                &budget,
            )
        } else {
            self.finish(cell, source, started, ending, stopped, &budget)
        };
        let epilogue = epilogue.disarm();
        self.state.watchdog_fired.borrow_mut().take();
        // The epilogue's watchdog re-issues its termination until it is
        // disarmed, so this is the cancel that leaves the isolate clean for
        // the next cell.
        self.isolate.cancel_terminate_execution();
        // An epilogue that ran past its own hard deadline while being
        // terminated is the same event as a cell that did: an isolate this
        // runtime can no longer stop. This cell keeps the answer it earned —
        // the model's program did finish — and no later one enters the
        // isolate.
        if epilogue.gave_up {
            self.poisoned_by = Some(Poisoned {
                cell,
                cause: PoisonCause::Epilogue,
            });
        }
        outcome
    }

    /// Everything that happens inside the isolate, so every V8 handle is
    /// released before the turn is assembled.
    fn execute(
        &mut self,
        compiled: &CompiledCell,
        saved: Option<&v8::Global<v8::Function>>,
    ) -> Ending {
        let response_byte_cap = self.trace().response_byte_cap.get();
        let has_batch = self.isolate.get_slot::<Rc<BatchStore>>().is_some();
        v8::scope!(let handle_scope, &mut self.isolate);
        let context = v8::Local::new(handle_scope, &self.context);
        let scope = &mut v8::ContextScope::new(handle_scope, context);
        v8::tc_scope!(let try_catch, scope);
        if has_batch {
            // Filtering by a preceding handler changed n/rest. Refresh under
            // this program's watchdog because writing globalThis.batch can
            // invoke a setter installed by an earlier program.
            bindings::install_batch(try_catch);
            if try_catch.has_terminated() {
                return Ending::Terminated;
            }
            if try_catch.has_caught() {
                let exception = try_catch.exception();
                return Ending::Threw(caught_error(try_catch, exception));
            }
        }

        let wrapper = if let Some(saved) = saved {
            v8::Local::new(try_catch, saved)
        } else {
            let Some(source) = v8::String::new(try_catch, &compiled.javascript) else {
                return Ending::Threw(plain_error(
                    "RangeError",
                    "the cell is too large to compile",
                ));
            };
            let Some(name) = v8::String::new(try_catch, &compiled.script_name) else {
                return Ending::Threw(plain_error("RangeError", "the cell could not be named"));
            };
            let origin = v8::ScriptOrigin::new(
                try_catch,
                name.into(),
                0,
                0,
                false,
                -1,
                None,
                false,
                false,
                false,
                None,
            );

            let Some(script) = v8::Script::compile(try_catch, source, Some(&origin)) else {
                // A terminated cell has no exception to read, and reading one
                // while V8 is unwinding a termination is not safe.
                if try_catch.has_terminated() {
                    return Ending::Terminated;
                }
                let exception = try_catch.exception();
                return Ending::Threw(caught_error(try_catch, exception));
            };
            let Some(wrapper) = script.run(try_catch) else {
                // A terminated cell has no exception to read, and reading one
                // while V8 is unwinding a termination is not safe.
                if try_catch.has_terminated() {
                    return Ending::Terminated;
                }
                let exception = try_catch.exception();
                return Ending::Threw(caught_error(try_catch, exception));
            };
            let Ok(wrapper) = v8::Local::<v8::Function>::try_from(wrapper) else {
                return Ending::Threw(plain_error(
                    "TypeError",
                    "the cell did not compile to a callable",
                ));
            };

            wrapper
        };

        let host = bindings::host_object(try_catch);
        let receiver: v8::Local<v8::Value> = v8::undefined(try_catch).into();
        let Some(promise) = wrapper.call(try_catch, receiver, &[host.into()]) else {
            // A terminated cell has no exception to read, and reading one
            // while V8 is unwinding a termination is not safe.
            if try_catch.has_terminated() {
                return Ending::Terminated;
            }
            let exception = try_catch.exception();
            return Ending::Threw(caught_error(try_catch, exception));
        };

        for _ in 0..MICROTASK_CHECKPOINTS {
            try_catch.perform_microtask_checkpoint();
            if !matches!(promise_state(promise), Some(v8::PromiseState::Pending)) {
                break;
            }
        }

        if try_catch.has_terminated() {
            return Ending::Terminated;
        }

        let Ok(promise) = v8::Local::<v8::Promise>::try_from(promise) else {
            // An `async function` always answers with a promise; a value
            // here would mean the wrapper was not the one this module wrote.
            return Ending::Threw(plain_error(
                "TypeError",
                "the cell did not answer with a promise",
            ));
        };
        let ending = match promise.state() {
            v8::PromiseState::Fulfilled => {
                let value = promise.result(try_catch);
                // §1's two endings, decided by the value rather than by a
                // flag: the generated body ends `return __pane_cell.e()`, and
                // only the host can mint what that answers with.
                if self.state.handlers.running.get()
                    || bindings::is_end_marker(try_catch, value, self.state.cell.get())
                {
                    Ending::Yielded { reason: None }
                } else {
                    // Inside the watchdog on purpose: reading a result walks
                    // the model's own getters and proxy traps. A read that
                    // did not answer leaves its exception or termination on
                    // the `TryCatch`, and nothing may run the model's code
                    // again until that has been read off -- measured: a
                    // second read re-entered a getter the watchdog had just
                    // stopped, with no watchdog left to stop it.
                    match response::returned(try_catch, &self.state, response_byte_cap, value) {
                        Ok(ending) => ending,
                        Err(ReadFailed) => {
                            if try_catch.has_terminated() {
                                return Ending::Terminated;
                            }
                            let exception = try_catch.exception();
                            return Ending::Threw(caught_error(try_catch, exception));
                        }
                    }
                }
            }
            v8::PromiseState::Rejected => {
                let value = promise.result(try_catch);
                Ending::Threw(thrown_error(try_catch, value))
            }
            v8::PromiseState::Pending => Ending::Threw(plain_error(
                "RuntimeStalled",
                "the cell awaited a promise nothing can settle: pane's isolate has no timers, no \
                 sockets and no event loop, and every tool call is synchronous",
            )),
        };
        // Reading the result runs the model's getters, and a getter the
        // watchdog stopped would otherwise hand back a partial value as a
        // completed return: a termination raised while the ending was being
        // read is the ending.
        if try_catch.has_terminated() {
            return Ending::Terminated;
        }
        ending
    }

    /// Puts the isolate's heap limit back to the configured ceiling and
    /// starts this cell's raise count at zero.
    ///
    /// A raise is permanent as far as V8 is concerned, so without this the
    /// first cell to cross the ceiling would raise it for the rest of the
    /// task and §2's ceiling would be whatever the worst cell so far needed.
    /// `remove_near_heap_limit_callback` is V8's own way to restore a limit —
    /// it lowers it to the given value, or to the least the live heap allows
    /// if that is higher — and the callback goes straight back on.
    fn restore_heap_limit(&mut self) {
        if self.heap.raises_this_cell.swap(0, Ordering::SeqCst) == 0 {
            return;
        }
        let limit = self.heap_limit_bytes;
        let data = Rc::as_ptr(&self.heap).cast_mut().cast::<c_void>();
        self.isolate
            .remove_near_heap_limit_callback(near_heap_limit, limit);
        self.isolate
            .add_near_heap_limit_callback(near_heap_limit, data);
    }

    /// `free("name")` is one of §2's three lifetime events, and the epilogue
    /// that re-reads every binding for the value it ended with must not undo
    /// one the cell itself performed: a program that declares a name and then
    /// frees it would otherwise leave the object on the persistent scope,
    /// live to the next cell and absent from the table that is supposed to
    /// list everything live.
    fn forget_freed(&mut self) {
        let freed = self.state.current.borrow().freed.clone();
        if freed.is_empty() {
            return;
        }
        v8::scope!(let handle_scope, &mut self.isolate);
        let context = v8::Local::new(handle_scope, &self.context);
        let scope = &mut v8::ContextScope::new(handle_scope, context);
        let global = context.global(scope);
        for name in &freed {
            if let Some(key) = v8::String::new(scope, name) {
                global.delete(scope, key.into());
            }
        }
    }

    /// Takes every live capture's preview again from the value the name
    /// holds now that the cell has ended.
    ///
    /// `capture()` marshals where `s(…)` runs, which is the end of the
    /// declaration's line — so `const arr = []; arr.push(1,2,3,4,5)` showed
    /// the model `n=0` for an array of five, and the `RuntimeOutOfMemory`
    /// list ranked the array that filled the heap last, at `~0 B`, because
    /// it was empty when it was declared. §3's preview is of the handle, and
    /// §2's five largest are the five largest now.
    ///
    /// The persistent scope is where the value is read from: `capture()` has
    /// already put every captured name on the global object, so this needs no
    /// handle of its own and reads exactly what the next cell will see. The
    /// epilogue's own re-capture covers the same ground for a binding the
    /// `finally` can reach; this is what covers a `class`, a `keep`, and the
    /// two endings where the `finally` never runs at all.
    ///
    /// **Every live name, not only the ones this cell bound.** The captures
    /// alone are the cell that has just ended, so `const arr = []` in cell 1
    /// and `arr.push(1,2,3,4,5)` in cell 2 left the model reading `n=0` for an
    /// array of five for the rest of the task. A captured name is written back
    /// into `current.captures`, which [`Runtime::finish`] drains into the
    /// table; a live-but-uncaptured one goes straight to the table through
    /// [`handles::HandleTable::refresh`], which is the only write that does
    /// not reorder it or claim the model redeclared the name.
    ///
    /// **Bounded by one [`EpilogueBudget`], not by one per name.** Every name
    /// is a fresh entry into V8, so a read the epilogue watchdog stopped
    /// costs about one [`TERMINATE_RETRY_INTERVAL`] and the next name starts
    /// the clock again — against a fixed budget that is linear in the number
    /// of live handles. Measured before this loop polled the budget: thirty
    /// handles each carrying an ordinary 100 ms lazy accessor, at the shipped
    /// [`DEFAULT_CELL_WALL_CLOCK_LIMIT`], ran the epilogue past its hard
    /// deadline, poisoned the isolate and ended the task — for a program with
    /// no loop in it, reported to the model as `cell 1 yielded in 7881 ms`.
    fn refresh_previews(&mut self, budget: &EpilogueBudget) {
        let mut names: Vec<String> = self
            .state
            .table
            .borrow()
            .names()
            .into_iter()
            .map(str::to_string)
            .collect();
        let captured: Vec<String> = self
            .state
            .current
            .borrow()
            .captures
            .iter()
            .map(|capture| capture.name.clone())
            .collect();
        for name in &captured {
            if !names.contains(name) {
                names.push(name.clone());
            }
        }
        if names.is_empty() {
            return;
        }
        let state = self.state.clone();
        let mut refreshed: Vec<(String, Value, HandleMeta)> = Vec::with_capacity(names.len());
        {
            v8::scope!(let handle_scope, &mut self.isolate);
            let context = v8::Local::new(handle_scope, &self.context);
            let context_scope = &mut v8::ContextScope::new(handle_scope, context);
            // A getter read here is the model's own code and may throw. The
            // `TryCatch` is what keeps that exception from being left pending
            // on the isolate for the next cell's first statement to inherit;
            // it is reset when this scope ends, and the name keeps the
            // preview it had.
            v8::tc_scope!(let scope, context_scope);
            let global = context.global(scope);
            for name in names {
                // The epilogue's budget is spent by the epilogue, not by
                // each name in it: a read the watchdog stopped costs one
                // `TERMINATE_RETRY_INTERVAL`, and entering V8 for the next
                // name pays it again. A name not reached keeps the preview
                // it had, which is the same degradation a stopped read
                // already produces.
                if budget.spent() {
                    break;
                }
                let Some(key) = v8::String::new(scope, &name) else {
                    continue;
                };
                // A name the scope no longer has keeps the preview it had:
                // replacing it with `undefined` would report a handle the
                // table still lists as having lost its value.
                if global.has(scope, key.into()) != Some(true) {
                    continue;
                }
                let Some(value) = global.get(scope, key.into()) else {
                    continue;
                };
                let (preview, meta) = bindings::preview_of(scope, &state, value);
                refreshed.push((name, preview, meta));
            }
        }
        let mut current = state.current.borrow_mut();
        for (name, preview, meta) in refreshed {
            if let Some(capture) = current
                .captures
                .iter_mut()
                .find(|capture| capture.name == name)
            {
                capture.value = preview;
                capture.meta = meta;
            } else {
                // Live, and this cell did not bind it: the table's own entry
                // is the only copy there is.
                state.table.borrow_mut().refresh(&name, preview, meta);
            }
        }
    }

    /// Whether **this** cell took the isolate's heap over its configured
    /// ceiling, read off the isolate rather than off the callback.
    ///
    /// [`near_heap_limit`] answers a crossing V8 *reported*, and V8 does not
    /// always report one: an allocation V8 satisfies out of large-object
    /// space never calls the callback back at all. Measured on this host,
    /// `const big = new Array(12000000).fill('y')` under a 32 MiB ceiling
    /// yielded in 11 ms with `raises = 0` and a 96 MB heap — an ordinary
    /// success, with the oversized handle live and nothing said — and at
    /// 20 million elements the *next* ordinary allocation, two cells later,
    /// killed the process. `used_heap_size` is on the isolate's own thread,
    /// allocates nothing and runs no model code, so it is an observation the
    /// runtime can always make where the callback was silent.
    ///
    /// **Latched, because §2 fails the cell that *crosses* the ceiling.**
    /// Nothing is freed when a cell fails, so the heap stays over afterwards;
    /// reporting it again on every later cell would answer the model's own
    /// `free("big")` with the error it is answering, and there would be no
    /// way out of the task. The latch clears itself when the heap is next
    /// observed under the ceiling, so a second crossing is a second answer.
    fn observe_heap_ceiling(&mut self) -> bool {
        let used = self.isolate.get_heap_statistics().used_heap_size();
        // What the isolate is holding is also what [`heap_grant`] may not
        // refuse to accommodate; see [`HEAP_RAISE_TOTAL_MULTIPLE`].
        self.heap.live_floor.store(used, Ordering::SeqCst);
        let over = used >= self.heap_limit_bytes;
        let crossed = over && !self.heap_over_ceiling;
        self.heap_over_ceiling = over;
        crossed
    }

    /// Turns a cell's ending into the turn the session loop reads.
    fn finish(
        &mut self,
        cell: u64,
        source: &str,
        started: Instant,
        ending: Ending,
        stopped: Stopped,
        budget: &EpilogueBudget,
    ) -> CellOutcome {
        let captures = std::mem::take(&mut self.state.current.borrow_mut().captures);
        let direct = self.trace().captures_results();
        let mut declared_now: Vec<String> = Vec::new();
        for capture in captures {
            if direct {
                declared_now.push(capture.name.clone());
            }
            self.state.table.borrow_mut().declare_with(
                capture.name,
                capture.value,
                cell,
                capture.meta,
            );
        }
        // A `keep` pins after the declare, because a redeclaration frees the
        // old entry and the new one starts unpinned.
        let pinned = std::mem::take(&mut self.state.current.borrow_mut().pinned);
        for name in &pinned {
            self.state.table.borrow_mut().pin(name);
        }
        // Before the record and the table are rendered, so neither carries a
        // name whose call failed.
        if direct {
            self.free_undefined_direct_bindings(&declared_now);
        }

        // After the captures are in the table, because both of these read it:
        // the out-of-memory list names the five largest live handles, and a
        // timeout's own message promises the cell's bindings are still there.
        let ending = match ending {
            // Ahead of every other arm, and whatever the cell did next: §2
            // says a cell that crosses the ceiling fails, and an external
            // allocation the ceiling refused is one the cell may have caught
            // as an ordinary `RangeError` and gone on from.
            _ if stopped.external_hit => {
                Ending::Threw(self.out_of_memory(EXTERNAL_CEILING, budget))
            }
            // Likewise whatever the cell did next, and this is §2 read
            // strictly: *"when the isolate's heap crosses its configured
            // ceiling the cell fails with `RuntimeOutOfMemory`"*. Only a
            // *terminated* cell used to be answered that way, so a cell that
            // crossed the ceiling and finished anyway — which is exactly what
            // the raise exists to let it do — was an ordinary yield with the
            // oversized handle live and nothing said, having moved the
            // ceiling by up to `HEAP_RAISE_TOTAL_MULTIPLE` on the way.
            // `heap_crossed` beside `heap_hit` because the two answer
            // different questions: the flag is a crossing the callback
            // reported, and V8 reports only some of them.
            _ if stopped.heap_hit || stopped.heap_crossed => {
                Ending::Threw(self.out_of_memory(HEAP_CEILING, budget))
            }
            Ending::Terminated
                if self.state.handlers.running.get()
                    && self.state.token.borrow().is_cancelled()
                    && !stopped.timed_out =>
            {
                Ending::Threw(plain_error("Cancelled", "the handler was cancelled"))
            }
            Ending::Terminated if stopped.timed_out => {
                Ending::Threw(timed_out(started.elapsed(), self.wall_clock_limit))
            }
            // The two ceilings above win over the flag; the flag wins over
            // nothing else terminating — `yieldNow` is a yield, never an
            // error (§9.3).
            Ending::Terminated => match stopped.yielded {
                Some(reason) => Ending::Yielded { reason },
                // Nothing else in this crate terminates execution, so this
                // is reported as what it is rather than as one of the three.
                None => Ending::Threw(plain_error(
                    "RuntimeTerminated",
                    "the isolate was terminated before the cell finished",
                )),
            },
            other => other,
        };

        // The turn's table is the delta (`smarter-cheaper-roadmap.md`,
        // *Observation delta*): entries this cell changed in full, the rest
        // as one line each. `handles()` and the rollout keep the inventory.
        let (table, mut observation) = handles::render_table_delta(
            &self.state.table.borrow(),
            preview::PREVIEW_TOKEN_CAP,
            preview::TABLE_TOKEN_CAP,
        );
        observation.repeated_observations = self.state.repeated_observations();
        self.state.flush_source_context();
        let (stdout_tail, stdout_dropped_tokens) = self.state.current.borrow_mut().console.tail();
        let kind = match &ending {
            Ending::Yielded { .. } => CellOutcomeKind::Yielded,
            Ending::Returned(..) => CellOutcomeKind::Returned,
            Ending::Threw(_) | Ending::Terminated => CellOutcomeKind::Threw,
        };
        let yield_reason = match &ending {
            Ending::Yielded { reason } => reason.clone(),
            _ => None,
        };
        let calls = self.trace().take_calls();
        let answer = self.trace().take_answer();
        let asked = self.trace().take_ask();
        let record = CellRecord {
            cell,
            source: source.to_string(),
            // The runtime never sees the model's message, only its program,
            // so the descriptor is filled in by the layer that read the turn
            // (`session::run_cell`). A runtime-only caller records none.
            description: None,
            outcome: kind,
            handles: self
                .state
                .table
                .borrow()
                .rows(preview::PREVIEW_TOKEN_CAP)
                .into_iter()
                .map(|(name, type_name, preview, provenance)| HandleRecord {
                    name,
                    type_name,
                    preview,
                    provenance,
                })
                .collect(),
            calls,
        };
        let turn = CellTurn {
            elapsed_ms: started.elapsed().as_millis() as u64,
            table,
            stdout_tail,
            stdout_dropped_tokens,
            yield_reason,
            answer,
            ask: asked,
            record,
            plan: self.state.plan(),
            capability_results: self.trace().take_results(),
            observation,
        };

        match ending {
            Ending::Yielded { .. } => CellOutcome::Yielded { turn },
            Ending::Returned(value, terminal) => CellOutcome::Returned {
                value,
                terminal,
                turn,
            },
            Ending::Threw(error) => CellOutcome::Threw { error, turn },
            // Normalised above. An arm rather than an `unreachable!` so a
            // future ending cannot silently become a yield.
            Ending::Terminated => CellOutcome::Threw {
                error: plain_error(
                    "RuntimeTerminated",
                    "the isolate was terminated before the cell finished",
                ),
                turn,
            },
        }
    }

    /// The `n` largest live handles **as they are now**, largest first, each
    /// measured off the persistent scope at the moment the error is built.
    ///
    /// `HandleMeta::size_estimate` is taken where a handle is captured, so
    /// for a handle the *current* cell never bound it is the size that
    /// handle had in some earlier cell. That is the common shape here — a
    /// model declares `const acc = []` in one cell and fills it in the next
    /// — and it ranked the array that filled the heap last, at `~0 B`,
    /// behind a 44-character string. §2 promises "the five largest live
    /// handles by retained size" so that the model can choose what to free,
    /// and a ranking by stale sizes tells it to free the wrong thing.
    ///
    /// Measuring here rather than refreshing every cell keeps the cost on
    /// the path that has already lost: `marshal::size_estimate` is the
    /// deliberately shallow one written for this moment (it never walks an
    /// array's elements or asks an array for its property names), and it
    /// runs after `run_cell`'s unconditional `cancel_terminate_execution`,
    /// so V8 answers rather than bailing. A name the scope no longer holds
    /// keeps the size the table recorded for it.
    fn largest_live_now(&mut self, n: usize, budget: &EpilogueBudget) -> Vec<(String, u64)> {
        let mut sized: Vec<(String, u64)> = {
            let table = self.state.table.borrow();
            table
                .names()
                .into_iter()
                .map(|name| {
                    (
                        name.to_string(),
                        table.meta(name).map_or(0, |meta| meta.size_estimate),
                    )
                })
                .collect()
        };
        {
            v8::scope!(let handle_scope, &mut self.isolate);
            let context = v8::Local::new(handle_scope, &self.context);
            let context_scope = &mut v8::ContextScope::new(handle_scope, context);
            // Sizing runs `get_own_property_names`, which is a `Proxy`'s
            // `ownKeys` trap: the model's own code again, and it may throw.
            v8::tc_scope!(let scope, context_scope);
            let global = context.global(scope);
            for (name, size) in &mut sized {
                // The same budget [`Runtime::refresh_previews`] spends, and
                // for the same reason: a name not reached keeps the size the
                // table recorded for it.
                if budget.spent() {
                    break;
                }
                let Some(key) = v8::String::new(scope, name) else {
                    continue;
                };
                if global.has(scope, key.into()) != Some(true) {
                    continue;
                }
                let Some(value) = global.get(scope, key.into()) else {
                    continue;
                };
                *size = marshal::size_estimate(scope, value);
            }
        }
        sized.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        sized.truncate(n);
        sized
    }

    /// The `RuntimeOutOfMemory` preview: the five largest live handles, so
    /// the *model* can choose what to free. Nothing is evicted here — §2 is
    /// explicit that a handle vanishing under a program that still names it
    /// is the one failure that would make the channel untrustworthy.
    fn out_of_memory(&mut self, cause: &str, budget: &EpilogueBudget) -> ErrorValue {
        let largest = self.largest_live_now(5, budget);
        let listed = if largest.is_empty() {
            "no live handles".to_string()
        } else {
            largest
                .iter()
                .map(|(name, size)| format!("{name} (~{size} B)"))
                .collect::<Vec<_>>()
                .join(", ")
        };
        ErrorValue {
            class: "RuntimeOutOfMemory".to_string(),
            message: format!(
                "{cause}; nothing was freed. Largest live handles: {listed}. Call free(\"name\") \
                 on what you no longer need."
            ),
            line: None,
            column: None,
            stack: Vec::new(),
        }
    }
}

/// Why a `RuntimeOutOfMemory` was raised: the V8 heap filled, or an
/// `ArrayBuffer`'s backing store would have taken external memory over the
/// same ceiling. The model is told which, because only one of them is
/// answered by holding fewer objects.
const HEAP_CEILING: &str = "the isolate reached its heap ceiling";
const EXTERNAL_CEILING: &str = "an ArrayBuffer allocation would have taken this isolate's external memory over its ceiling \
     and was refused";

/// How a cell ended, before the turn around it is assembled.
enum Ending {
    /// A fall-off (`reason: None`), or a yield on purpose with `yieldNow`'s
    /// reason or the response cap's sentence (§9.3, §9.2).
    Yielded {
        reason: Option<String>,
    },
    Returned(Value, Terminal),
    Threw(ErrorValue),
    /// V8 stopped the cell. [`Stopped`] says which of the two ceilings did
    /// it, or that `yieldNow` asked, because the model is told a different
    /// thing by each.
    Terminated,
}

/// Which ceiling stopped the cell, read the instant it stopped.
///
/// Every flag is consumed once, before any of the bookkeeping that follows
/// a cell allocates: taking a preview can raise the heap callback itself,
/// and a hit raised there would report a timeout as an out-of-memory.
#[derive(Debug, Clone)]
struct Stopped {
    heap_hit: bool,
    /// The isolate's own heap was over the configured ceiling when the cell
    /// ended, whether or not [`near_heap_limit`] said so — see
    /// [`Runtime::observe_heap_ceiling`].
    heap_crossed: bool,
    /// The `ArrayBuffer` allocator refused an allocation that would have
    /// crossed the ceiling. Read rather than taken: it is cleared at the
    /// start of every cell, and unlike the heap hit it cannot be raised by
    /// the runtime's own bookkeeping, which allocates no backing stores.
    external_hit: bool,
    timed_out: bool,
    /// `Some` when `yieldNow` was called, with the reason it gave.
    yielded: Option<Option<String>>,
}

impl Stopped {
    /// A cell that never reached V8 — one that did not compile.
    fn none() -> Self {
        Self {
            heap_hit: false,
            heap_crossed: false,
            external_hit: false,
            timed_out: false,
            yielded: None,
        }
    }
}

/// A read of the model's value that did not answer: the getter or trap it
/// ran threw, or was stopped. The exception is on the `TryCatch` and the
/// reader stops at once, because the next read would run that code again.
struct ReadFailed;

/// Which cell cost the isolate its trust, and which of the two things that
/// run model code was still running when the hard deadline passed.
///
/// **The distinction is the model's to know.** A cell that ignores every
/// termination is the model's own program; an epilogue that does is pane
/// re-reading the handle table, which runs the getters and proxy traps the
/// program defined but is pane's own loop and pane's own deadline. Telling
/// the model `cell N did not stop` for the second case says something untrue
/// about its program — measured through the shipped binary, on a cell that
/// had already been reported to it as `cell 1 yielded in 7895 ms`.
#[derive(Debug, Clone, Copy)]
struct Poisoned {
    cell: u64,
    cause: PoisonCause,
}

/// See [`Poisoned`].
#[derive(Debug, Clone, Copy)]
enum PoisonCause {
    /// The cell's own program went on running past the hard deadline.
    Cell,
    /// The cell finished; reading its handles afterwards did not.
    Epilogue,
}

/// What every cell after [`gave_up`] is answered with, naming the cell that
/// cost the isolate its trust and what was running at the time. Built
/// without touching V8, which is the whole promise of [`Runtime::poisoned`].
fn poisoned_error(by: Poisoned) -> ErrorValue {
    let cell = by.cell;
    let message = match by.cause {
        PoisonCause::Cell => format!(
            "cell {cell} did not stop when pane terminated it, so this isolate is no longer \
             trusted and no later cell runs in it; the task is over"
        ),
        PoisonCause::Epilogue => format!(
            "cell {cell} finished, but reading its handles afterwards did not stop when pane \
             terminated it — a handle is read by running the getters and proxy traps the program \
             defined — so this isolate is no longer trusted and no later cell runs in it; the \
             task is over"
        ),
    };
    ErrorValue {
        class: "RuntimePoisoned".to_string(),
        message,
        line: None,
        column: None,
        stack: Vec::new(),
    }
}

fn plain_error(class: &str, message: &str) -> ErrorValue {
    ErrorValue {
        class: class.to_string(),
        message: message.to_string(),
        line: None,
        column: None,
        stack: Vec::new(),
    }
}

fn promise_state(value: v8::Local<v8::Value>) -> Option<v8::PromiseState> {
    v8::Local::<v8::Promise>::try_from(value)
        .ok()
        .map(|promise| promise.state())
}

/// The error a `TryCatch` was holding — a compile failure or a synchronous
/// throw before the cell's first `await`. The exception is read out of the
/// `TryCatch` by the caller so this takes an ordinary scope.
fn caught_error(scope: &mut v8::PinScope, exception: Option<v8::Local<v8::Value>>) -> ErrorValue {
    match exception {
        Some(exception) => thrown_error(scope, exception),
        None => plain_error("Error", "the cell failed without an exception"),
    }
}

/// An exception, read for exactly what `runtime-contract.md` §5 lists: the
/// class, the message, the position inside the model's own program, and only
/// the frames that are inside it.
fn thrown_error(scope: &mut v8::PinScope, exception: v8::Local<v8::Value>) -> ErrorValue {
    let mut error = marshal::error_of(scope, exception);

    if let Some(trace) = v8::Exception::get_stack_trace(scope, exception) {
        for index in 0..trace.get_frame_count() {
            let Some(frame) = trace.get_frame(scope, index) else {
                continue;
            };
            let script = frame
                .get_script_name(scope)
                .map(|name| name.to_rust_string_lossy(scope))
                .unwrap_or_default();
            if !script.starts_with(CELL_SCRIPT_PREFIX) {
                // A host frame, and §5 says the model never sees one.
                continue;
            }
            let line = (frame.get_line_number() as u32).saturating_sub(LINE_OFFSET);
            let column = (frame.get_column() as u32).saturating_sub(1);
            // V8 reports a stack overflow's frames as present-and-zero, and
            // `ErrorSection::position` is explicit that `line 0, column 0`
            // names a place that does not exist. `function f(n) { return
            // f(n + 1); } f(0)` was rendered to the model as ten frames of
            // it, which is the position a model-written traversal is most
            // likely to be handed wrongly.
            if line == 0 && column == 0 {
                continue;
            }
            if error.line.is_none() {
                error.line = Some(line);
                error.column = Some(column);
            }
            let function = frame
                .get_function_name(scope)
                .map(|name| name.to_rust_string_lossy(scope))
                .unwrap_or_default();
            let cell = script.trim_start_matches(CELL_SCRIPT_PREFIX);
            error.stack.push(StackFrame {
                description: if function.is_empty() {
                    format!("cell {cell}, line {line}, column {column}")
                } else {
                    format!("{function} (cell {cell}, line {line}, column {column})")
                },
            });
        }
    }

    if error.line.is_none()
        && let Some(message) = v8::Exception::create_message(scope, exception).into()
    {
        let message: v8::Local<v8::Message> = message;
        let script = message
            .get_script_resource_name(scope)
            .map(|name| name.to_rust_string_lossy(scope))
            .unwrap_or_default();
        if script.starts_with(CELL_SCRIPT_PREFIX)
            && let Some(line) = message.get_line_number(scope)
        {
            error.line = Some((line as u32).saturating_sub(LINE_OFFSET));
            error.column = Some(message.get_start_column() as u32);
        }
    }

    error
}

/// The one property of this module that is a property of its *source*: no
/// expression under `runtime/` compiles a sandbox profile, so the profile a
/// cell runs against can only be the one the session compiled at start-up
/// (`sandbox-grants.md` §1.5).
#[cfg(test)]
mod tests {
    use super::{PoisonCause, Poisoned, poisoned_error};

    const ISOLATE_SOURCE: &str = include_str!("isolate.rs");
    const BINDINGS_SOURCE: &str = include_str!("bindings.rs");
    const STATE_SOURCE: &str = include_str!("state.rs");
    const CELL_SOURCE: &str = include_str!("cell.rs");
    const MARSHAL_SOURCE: &str = include_str!("marshal.rs");
    const WATCHDOG_SOURCE: &str = include_str!("isolate/watchdog.rs");
    const CONSOLE_SOURCE: &str = include_str!("bindings/console.rs");
    // Every successor of a split, or the split is how a forbidden call gets
    // into the runtime. `bindings/{agent,decide,web}.rs` and
    // `isolate/{decide,web}.rs` were extracted on 2026-09-17 and
    // `bindings/helper.rs` on 2026-09-18, and none of them was added here
    // until now: for that window the scan below was reading a shrinking
    // fraction of the module it claims to cover.
    const AGENT_SOURCE: &str = include_str!("bindings/agent.rs");
    const BINDINGS_DECIDE_SOURCE: &str = include_str!("bindings/decide.rs");
    const BINDINGS_WEB_SOURCE: &str = include_str!("bindings/web.rs");
    const HELPER_SOURCE: &str = include_str!("bindings/helper.rs");
    const SEARCH_SOURCE: &str = include_str!("bindings/search.rs");
    const ISOLATE_DECIDE_SOURCE: &str = include_str!("isolate/decide.rs");
    const ISOLATE_WEB_SOURCE: &str = include_str!("isolate/web.rs");
    const RESPONSE_SOURCE: &str = include_str!("isolate/response.rs");

    const SOURCES: [(&str, &str); 15] = [
        ("isolate.rs", ISOLATE_SOURCE),
        ("bindings.rs", BINDINGS_SOURCE),
        ("state.rs", STATE_SOURCE),
        ("cell.rs", CELL_SOURCE),
        ("marshal.rs", MARSHAL_SOURCE),
        ("isolate/watchdog.rs", WATCHDOG_SOURCE),
        ("bindings/console.rs", CONSOLE_SOURCE),
        ("bindings/agent.rs", AGENT_SOURCE),
        ("bindings/decide.rs", BINDINGS_DECIDE_SOURCE),
        ("bindings/web.rs", BINDINGS_WEB_SOURCE),
        ("bindings/helper.rs", HELPER_SOURCE),
        ("bindings/search.rs", SEARCH_SOURCE),
        ("isolate/decide.rs", ISOLATE_DECIDE_SOURCE),
        ("isolate/web.rs", ISOLATE_WEB_SOURCE),
        ("isolate/response.rs", RESPONSE_SOURCE),
    ];

    /// The production half of a file: everything before its first
    /// `#[cfg(test)]`, with comment lines dropped so a sentence *about* a
    /// forbidden call is not mistaken for one.
    fn production(source: &str) -> String {
        source
            .split_once("#[cfg(test)]")
            .map_or(source, |(before, _)| before)
            .lines()
            .filter(|line| {
                let trimmed = line.trim_start();
                !trimmed.starts_with("//")
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    // A scan that scanned nothing would pass every assertion below, so each
    // file's production half is checked to still contain the item that
    // makes it that file.
    //
    // The epilogue that could not be stopped is not the cell that could
    // not be stopped, and the model is not told it was. `a0186fa` had one
    // message for both, so a task ended by pane's own handle-table read —
    // thirty ordinary lazy accessors, a program with no loop in it, reported
    // to the model one turn earlier as `cell 1 yielded in 7895 ms` — was
    // explained to the model as `cell 1 did not stop when pane terminated
    // it`. That is a false statement about the model's own program, and it
    // is the half of Blocker 1 that a bounded budget does not by itself
    // repair.
    //
    // Documents no item: the test these two paragraphs were written for is
    // gone, and they are kept as a note rather than as a doc comment that
    // documents whatever happens to follow them.

    /// **The pre-judgement happens before the program runs, or it buys
    /// nothing.** Asked after `execute`, every command line would already
    /// have met the gate one at a time, which is the cost this exists to
    /// remove. Provable only by reading: both calls are on paths a unit test
    /// cannot reach without a decision model and a terminal to ask at.
    #[test]
    fn the_command_lines_are_judged_before_the_cell_runs() {
        let judged = ISOLATE_SOURCE
            .find("gate.prejudge(")
            .expect("the cell loop must pre-judge the lines its source spells out");
        let ran = ISOLATE_SOURCE
            .find("self.execute(&compiled, saved.as_ref())")
            .expect("the cell loop must still run the compiled cell through `execute`");
        assert!(
            judged < ran,
            "the lines must be judged before the program runs"
        );
        assert!(
            ISOLATE_SOURCE[judged.saturating_sub(400)..judged].contains("literal_lines("),
            "what is judged must be the lines read out of this cell's own source"
        );
    }

    #[test]
    fn the_epilogue_and_the_cell_are_not_the_same_poisoning() {
        let by_cell = poisoned_error(Poisoned {
            cell: 1,
            cause: PoisonCause::Cell,
        });
        let by_epilogue = poisoned_error(Poisoned {
            cell: 1,
            cause: PoisonCause::Epilogue,
        });
        assert_eq!(by_cell.class, by_epilogue.class);
        assert_ne!(
            by_cell.message, by_epilogue.message,
            "the two are told apart or the model is told something untrue"
        );
        assert!(
            by_cell.message.contains("cell 1 did not stop"),
            "{}",
            by_cell.message
        );
        assert!(
            !by_epilogue.message.contains("cell 1 did not stop"),
            "the cell did stop; what did not was the read of its handles: {}",
            by_epilogue.message
        );
        assert!(
            by_epilogue.message.contains("reading its handles"),
            "{}",
            by_epilogue.message
        );
    }

    #[test]
    fn the_scan_has_something_to_scan() {
        for (name, source) in SOURCES {
            let production = production(source);
            assert!(
                production.len() > 500,
                "{name}'s production half is only {} bytes",
                production.len()
            );
        }
        assert!(production(ISOLATE_SOURCE).contains("pub fn run_cell"));
        assert!(production(BINDINGS_SOURCE).contains("invoke::run"));
        assert!(production(HELPER_SOURCE).contains("fn helper_callback"));
        assert!(production(RESPONSE_SOURCE).contains("fn returned"));
        assert!(production(CELL_SOURCE).contains("fn compile"));
    }

    #[test]
    fn nothing_in_the_runtime_compiles_a_profile() {
        for (name, source) in SOURCES {
            assert!(
                !production(source).contains("Profile::compile"),
                "{name} compiles a profile; sandbox-grants.md §1.5 says the session's is the only \
                 one"
            );
        }
    }

    /// The other half of the same claim: the only child-spawning path out of
    /// this module is `invoke::run`, which is confined before it spawns.
    #[test]
    fn the_runtime_spawns_nothing_of_its_own() {
        for (name, source) in SOURCES {
            let production = production(source);
            for forbidden in [
                "Command::new",
                "std::fs::",
                "TcpStream",
                "std::net",
                "File::open",
            ] {
                assert!(
                    !production.contains(forbidden),
                    "{name} reaches for `{forbidden}`; every effect this package has goes through \
                     tools::invoke::run"
                );
            }
        }
    }
}
