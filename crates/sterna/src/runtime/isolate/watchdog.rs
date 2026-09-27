//! The cell watchdog and the epilogue budget (moved out of `isolate.rs` for
//! the Phase 59 size ratchet, 2026-09-13; nothing here is new).

use super::*;

/// The epilogue's whole budget, shared with the loops that spend it.
///
/// **The invariant: the epilogue costs one budget, not one budget per live
/// handle.** [`Runtime::refresh_previews`] and [`Runtime::largest_live_now`]
/// enter V8 once per name, and a read the epilogue watchdog stopped costs
/// about one [`TERMINATE_RETRY_INTERVAL`] before the next name starts the
/// clock again — so without something the loops poll, the epilogue's cost is
/// linear in the number of handles while [`EPILOGUE_WALL_CLOCK_LIMIT`] is a
/// constant, and enough handles run it past the hard deadline that poisons
/// the isolate. Both halves are here because they fail differently: the flag
/// is the watchdog's own verdict and needs no clock of this thread's, and
/// the deadline holds even if that thread has not been scheduled yet.
#[derive(Debug, Clone)]
pub(super) struct EpilogueBudget {
    pub(super) fired: Option<Arc<AtomicBool>>,
    pub(super) deadline: Instant,
}

impl EpilogueBudget {
    /// The budget of an epilogue watched by `watchdog`, starting now.
    pub(super) fn of(watchdog: &Watchdog) -> Self {
        Self {
            fired: Some(Arc::clone(&watchdog.fired)),
            deadline: Instant::now() + EPILOGUE_WALL_CLOCK_LIMIT,
        }
    }

    /// A cell answered without an epilogue at all — one that did not
    /// compile, or one a poisoned runtime refused before touching V8.
    pub(super) fn unwatched() -> Self {
        Self {
            fired: None,
            deadline: Instant::now() + EPILOGUE_WALL_CLOCK_LIMIT,
        }
    }

    /// Whether the epilogue has spent what it was given.
    pub(super) fn spent(&self) -> bool {
        self.fired
            .as_ref()
            .is_some_and(|fired| fired.load(Ordering::SeqCst))
            || Instant::now() >= self.deadline
    }
}

/// What a [`Watchdog`] did while its cell ran.
#[derive(Debug, Clone, Copy)]
pub(super) struct Disarmed {
    /// The wall-clock limit passed and the watchdog terminated the cell.
    pub(super) fired: bool,
    /// The cell went on running past [`HARD_DEADLINE_MULTIPLE`] × the limit
    /// while being terminated every [`TERMINATE_RETRY_INTERVAL`]. It stopped
    /// in the end — nothing downstream of `disarm` could run otherwise — but
    /// the isolate is no longer one this runtime will re-enter.
    pub(super) gave_up: bool,
}

/// The wall clock, as one thread per cell.
///
/// It holds a clone of the isolate's `IsolateHandle` — the same `Send` handle
/// the near-heap-limit callback uses — waits on a condition variable for the
/// cell to finish, and calls `terminate_execution` if the wait times out
/// first. The condition variable rather than a sleep loop is what makes
/// [`Watchdog::disarm`] return immediately for the overwhelming majority of
/// cells, which finish in milliseconds.
///
/// **It terminates until the cell stops, not once.** V8 observes a
/// termination request at its own interrupt checks, and there are stretches
/// with none: measured on this host, a cell allocating through
/// `Array.prototype.fill` sat inside `Builtin_ArrayPrototypeFill` and never
/// saw the single request the watchdog used to issue, so a 500 ms limit
/// became minutes and a 30 s one became 1:59 through the shipped binary.
/// Asking again every [`TERMINATE_RETRY_INTERVAL`] turns a lost request into
/// a 50 ms delay.
///
/// **And it never stops asking**, even past the hard deadline, because the
/// thread blocked inside V8 is the one that must return: giving up on the
/// request would be giving up on `run_cell` ever answering. What the hard
/// deadline does is record that this isolate stopped being stoppable, which
/// is what [`Runtime::poisoned`] then acts on.
pub(super) struct Watchdog {
    pub(super) done: Arc<(Mutex<bool>, Condvar)>,
    pub(super) fired: Arc<AtomicBool>,
    pub(super) gave_up: Arc<AtomicBool>,
    pub(super) thread: Option<std::thread::JoinHandle<()>>,
}

impl Watchdog {
    pub(super) fn arm(isolate: Option<v8::IsolateHandle>, limit: Duration) -> Self {
        Self::arm_cancellable(isolate, limit, None)
    }
    pub(super) fn arm_cancellable(
        isolate: Option<v8::IsolateHandle>,
        limit: Duration,
        token: Option<CancellationToken>,
    ) -> Self {
        Self::arm_pausing(isolate, limit, token, None)
    }

    pub(super) fn arm_pausing(
        isolate: Option<v8::IsolateHandle>,
        limit: Duration,
        token: Option<CancellationToken>,
        wait_clock: Option<Arc<crate::approval::WaitClock>>,
    ) -> Self {
        let done = Arc::new((Mutex::new(false), Condvar::new()));
        let fired = Arc::new(AtomicBool::new(false));
        let gave_up = Arc::new(AtomicBool::new(false));
        let armed = Instant::now();
        let paused_at_start = wait_clock
            .as_ref()
            .map(|clock| clock.elapsed())
            .unwrap_or_default();
        let hard = limit.saturating_mul(HARD_DEADLINE_MULTIPLE);
        let thread = isolate.map(|isolate| {
            let done = Arc::clone(&done);
            let fired = Arc::clone(&fired);
            let gave_up = Arc::clone(&gave_up);
            std::thread::spawn(move || {
                let elapsed = || {
                    let paused = wait_clock
                        .as_ref()
                        .map(|clock| clock.elapsed())
                        .unwrap_or_default();
                    armed
                        .elapsed()
                        .saturating_sub(paused.saturating_sub(paused_at_start))
                };
                let (lock, finished) = &*done;
                let mut guard = lock.lock().unwrap_or_else(PoisonError::into_inner);
                loop {
                    if *guard {
                        return;
                    }
                    if token.as_ref().is_some_and(CancellationToken::is_cancelled)
                        || elapsed() >= limit
                    {
                        break;
                    }
                    let wait = if token.is_some() || wait_clock.is_some() {
                        Duration::from_millis(20).min(limit.saturating_sub(elapsed()))
                    } else {
                        limit.saturating_sub(elapsed())
                    };
                    let (next, _) = finished
                        .wait_timeout_while(guard, wait, |done| !*done)
                        .unwrap_or_else(PoisonError::into_inner);
                    guard = next;
                }
                // Ordered before the terminate so the flag is visible to
                // `disarm`, which cannot run until this thread releases the
                // lock it is still holding.
                fired.store(elapsed() >= limit, Ordering::SeqCst);
                isolate.terminate_execution();
                loop {
                    let (next, _) = finished
                        .wait_timeout_while(guard, TERMINATE_RETRY_INTERVAL, |done| !*done)
                        .unwrap_or_else(PoisonError::into_inner);
                    guard = next;
                    // Read before the cell's own ending is: a cell that
                    // stopped at last, but only after the hard deadline, is
                    // exactly the one this flag is about. Checking it after
                    // the `return` below would answer "false" for every cell
                    // that eventually stopped, which is all of them.
                    if elapsed() >= hard {
                        gave_up.store(true, Ordering::SeqCst);
                    }
                    if *guard {
                        return;
                    }
                    isolate.terminate_execution();
                }
            })
        });
        Self {
            done,
            fired,
            gave_up,
            thread,
        }
    }

    /// Stops the watch and answers what it did.
    pub(super) fn disarm(mut self) -> Disarmed {
        self.stop();
        Disarmed {
            fired: self.fired.load(Ordering::SeqCst),
            gave_up: self.gave_up.load(Ordering::SeqCst),
        }
    }

    /// Tells the thread the cell finished and waits for it to notice.
    /// Idempotent, so [`Watchdog::disarm`] and the `Drop` below can both run.
    pub(super) fn stop(&mut self) {
        {
            let (lock, finished) = &*self.done;
            *lock.lock().unwrap_or_else(PoisonError::into_inner) = true;
            finished.notify_all();
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// A panic between arming and disarming would otherwise leave a thread that
/// terminates whatever the isolate is running when its deadline arrives —
/// which, by then, is a later cell.
impl Drop for Watchdog {
    fn drop(&mut self) {
        self.stop();
    }
}

/// The `RuntimeTimeout` a stopped cell is answered with.
///
/// It is a throw and not a runtime error for the reason §5 gives for every
/// other one: the bindings the cell completed are in the table, the session
/// is intact, and the model gets the next turn to decide what to do about it.
pub(super) fn timed_out(elapsed: Duration, limit: Duration) -> ErrorValue {
    ErrorValue {
        class: "RuntimeTimeout".to_string(),
        message: format!(
            "the cell ran for {} ms without finishing and was stopped at Sterna's wall-clock limit \
             of {} ms; nothing was freed and the bindings it completed are still live",
            elapsed.as_millis(),
            limit.as_millis()
        ),
        line: None,
        column: None,
        stack: Vec::new(),
    }
}

/// The `RuntimeTimeout` a cell that ignored the whole ladder of
/// terminations is answered with — the wall-clock limit it passed, and the
/// hard deadline past which this isolate stopped being trusted.
///
/// Still a throw, and still `RuntimeTimeout`: from the model's side this is
/// the same event as [`timed_out`], only worse, and §5's shape does not
/// change because the host lost confidence in its isolate.
pub(super) fn gave_up(elapsed: Duration, limit: Duration, hard: Duration) -> ErrorValue {
    ErrorValue {
        class: "RuntimeTimeout".to_string(),
        message: format!(
            "the cell ran for {} ms, ignoring every termination sterna issued from its wall-clock \
             limit of {} ms onwards and past its hard deadline of {} ms; nothing was freed, and \
             no later cell runs in this isolate",
            elapsed.as_millis(),
            limit.as_millis(),
            hard.as_millis()
        ),
        line: None,
        column: None,
        stack: Vec::new(),
    }
}
