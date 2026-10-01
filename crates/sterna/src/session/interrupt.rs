//! Ctrl-C and SIGTERM as the process receives them: the handlers, the count
//! they keep, and the watcher that turns it into the session's decision.
//! What an interrupt does to the call in flight is [`Interrupter`]'s; how a
//! signal ends the session is here.

use super::{Interrupter, resume, ui};
use crate::bg;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

/// A second Ctrl-C inside this window ends the session; a later one starts a
/// new pair. Two seconds is long enough that a person who meant "again"
/// reaches it and short enough that an interrupt an hour ago is not half of
/// today's.
pub(super) const DOUBLE_INTERRUPT_WINDOW: Duration = Duration::from_secs(2);

/// How often the watcher asks whether the handler fired -- the same 20 ms
/// `tools::invoke` polls its child with, so a Ctrl-C costs at most two polls.
const INTERRUPT_POLL: Duration = Duration::from_millis(20);

/// How many Ctrl-C have arrived since [`watch`] last looked: counted by the
/// signal handler and by the keyboard's own Ctrl-C.
///
/// **A handler may do exactly one async-signal-safe thing, and this is it.**
/// Everything the interrupt means -- which token to cancel, whether it is the
/// second of a pair, whether a rollout line is half written -- is decided by
/// [`watch`] on an ordinary thread, where locks and allocation are legal.
/// It is a count, not a flag: two presses read in one go -- a quick double
/// tap, or a terminal that sends both at once -- land between two of the
/// watcher's looks, and a flag raised twice reads as once.
pub(crate) static INTERRUPT: AtomicUsize = AtomicUsize::new(0);
static TERMINATE: AtomicBool = AtomicBool::new(false);

/// Installs the process's SIGINT handler. Unix: `signal(2)`, whose BSD
/// semantics on both platforms sterna ships for leave the handler installed
/// across deliveries, so a second Ctrl-C reaches the same function.
///
/// `libc` is not a dependency of this crate on macOS and this is two lines of
/// declaration, so the handler is declared rather than depended on -- the same
/// choice `sandbox::macos` makes for `sandbox_init`.
#[cfg(unix)]
pub(super) fn install_interrupt_handler() {
    /// `SIGINT` on every unix sterna ships for.
    const SIGINT: i32 = 2;

    unsafe extern "C" {
        fn signal(sig: i32, handler: usize) -> usize;
    }

    extern "C" fn on_interrupt(_sig: i32) {
        INTERRUPT.fetch_add(1, Ordering::SeqCst);
    }

    extern "C" fn on_terminate(_sig: i32) {
        TERMINATE.store(true, Ordering::SeqCst);
    }
    unsafe {
        signal(SIGINT, on_interrupt as *const () as usize);
        signal(15, on_terminate as *const () as usize);
    };
}

/// The Windows half: the console's Ctrl-C routine sets the identical flag.
///
/// It runs on a thread of the console's own making rather than on top of the
/// interrupted one, and returning `TRUE` says the event was handled -- which
/// is what stops the default handler ending the process before [`watch`] has
/// decided whether this was the first Ctrl-C or the second.
#[cfg(windows)]
pub(super) fn install_interrupt_handler() {
    use windows_sys::Win32::Foundation::TRUE;
    use windows_sys::Win32::System::Console::{
        CTRL_BREAK_EVENT, CTRL_C_EVENT, SetConsoleCtrlHandler,
    };
    use windows_sys::core::BOOL;

    unsafe extern "system" fn on_interrupt(event: u32) -> BOOL {
        if event == CTRL_C_EVENT || event == CTRL_BREAK_EVENT {
            INTERRUPT.fetch_add(1, Ordering::SeqCst);
        }
        TRUE
    }

    unsafe { SetConsoleCtrlHandler(Some(on_interrupt), TRUE) };
}

/// Turns the count into the session's decision, forever.
///
/// It is a thread because there is nowhere else to poll from: a task spends
/// its whole life inside `send_turn` or inside `run_cell`, and neither
/// returns to the loop while the call a Ctrl-C is meant to stop is running.
pub(super) fn watch(state: &Interrupter, steer: Option<Arc<ui::Steer>>) -> ! {
    let mut first: Option<Instant> = None;
    loop {
        std::thread::sleep(INTERRUPT_POLL);
        if TERMINATE.swap(false, Ordering::SeqCst) {
            state.end_after_signal(143, "termination requested; ending the session");
        }
        // The second Escape reaches the token here, because this thread is
        // the one that owns it -- but it stays out of the double-interrupt
        // window above. Escape is the lever that must never end the
        // session: a person pressing it twice is asking for their call
        // back, not for their session to go away.
        if steer.as_ref().is_some_and(|steer| steer.take_cancel()) {
            state.by_signal.store(false, Ordering::SeqCst);
            state.raise();
            continue;
        }
        let presses = INTERRUPT.swap(0, Ordering::SeqCst);
        if presses == 0 {
            continue;
        }
        let now = Instant::now();
        if presses > 1
            || first.is_some_and(|earlier| now.duration_since(earlier) <= DOUBLE_INTERRUPT_WINDOW)
        {
            state.end_the_session();
        }
        first = Some(now);
        state.by_signal.store(true, Ordering::SeqCst);
        state.raise();
    }
}

/// The status a shell reports for a process ended by SIGINT.
const INTERRUPTED_EXIT: i32 = 130;

/// How long the second Ctrl-C gives the cancelled call to kill and reap its
/// own child before exiting anyway: twelve of `invoke`'s 20 ms polls, spent
/// holding the rollout's write lock so the task loop cannot start another
/// call inside it. See [`Interrupter::end_the_session`].
const REAP_GRACE: Duration = Duration::from_millis(250);

impl Interrupter {
    /// The second Ctrl-C, and the only place in `sterna` that exits from a
    /// thread other than the main one.
    ///
    /// **It cancels before it exits, and that is not decoration.**
    /// `std::process::exit` does not touch this process's children, so an
    /// exit taken with a call in flight reparents the confined child to
    /// `init` and leaves it there. Measured, before this function did
    /// anything but exit: one `bash` spinning at 87% of a core, for ever.
    /// Cancelling hands that child to `invoke::kill_and_reap`, which kills
    /// *and* reaps it.
    ///
    /// **Then it takes [`writing`](Self::writing) and holds it across the
    /// grace, and that ordering is the rest of the fix.** Taking the lock
    /// waits for the rollout line in flight to finish, which is the
    /// whole-line guarantee. *Holding* it stops the task loop at its next
    /// write -- `act_on`'s cell line is the very next thing after the
    /// cancelled call returns -- so the loop cannot answer the cell, ask for
    /// another turn and start another cell inside the grace. It did exactly
    /// that when the grace was an unguarded sleep, spawning a *fresh*
    /// spinning child for the same exit to orphan.
    ///
    /// **Then it takes the background board with it, which is the same
    /// defect a second time**: `raise` cancels the foreground call's token
    /// and nothing else, and a job runs on a thread of its own under a token
    /// of its own. Measured before this call existed: a job's `bash` on
    /// `ppid 1` at 99% of a core, twenty seconds after `sterna` exited 130.
    /// It goes *after* the lock, because holding it is what stops the loop
    /// starting a fresh `bg.run` for the exit to orphan, and *before* the
    /// sleep, because the grace is what the cancelled children are reaped
    /// in. The grace it passes is [`REAP_GRACE`] rather than `bg`'s own ten
    /// seconds, and `shutdown_within` detaches what has not stopped by then:
    /// a Ctrl-C that waits for an unkillable job would be a worse defect
    /// than the orphan this closes.
    ///
    /// [`REAP_GRACE`] is bounded because a Ctrl-C that hangs is not a Ctrl-C:
    /// after it, the exit proceeds whatever the child is doing.
    fn end_the_session(&self) -> ! {
        self.end_after_signal(INTERRUPTED_EXIT, "interrupted twice; ending the session")
    }

    fn end_after_signal(&self, exit: i32, message: &str) -> ! {
        self.ending.store(true, Ordering::SeqCst);
        self.raise();
        let _line = self.writing();
        bg::shutdown_within(&self.session, REAP_GRACE);
        std::thread::sleep(REAP_GRACE);
        ui::restore_terminal();
        // The exit below runs no destructor: the entry that says where this
        // session listens goes now, or it would point at nothing.
        crate::engine::data::withdraw_all();
        eprintln!("sterna: {message}");
        // Every way out says how to come back, Ctrl-C included.
        resume::goodbye()
            .into_iter()
            .for_each(|line| eprintln!("{line}"));
        std::process::exit(exit);
    }
}

/// The session's own thread, once its loop is over: a signal that is ending
/// the session owns the exit -- its status, its last lines -- so this thread
/// waits for it rather than racing it to an exit of its own.
///
/// **Found on Windows.** Ending by a signal restores the terminal, which
/// ends the screen's thread; the session's thread then read its input
/// closed and exited 1 while the watcher was still saying goodbye, before
/// its exit with 130.
pub(super) fn leave_the_exit_to_the_signal(ending: &AtomicBool) {
    while ending.load(Ordering::SeqCst) {
        std::thread::park();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_signal_ending_the_session_keeps_the_exit_to_itself() {
        static ENDING: AtomicBool = AtomicBool::new(false);
        // Nothing ending: the session's thread goes on at once.
        leave_the_exit_to_the_signal(&ENDING);
        ENDING.store(true, Ordering::SeqCst);
        let waiting = std::thread::spawn(|| leave_the_exit_to_the_signal(&ENDING));
        std::thread::sleep(Duration::from_millis(200));
        waiting.thread().unpark();
        std::thread::sleep(Duration::from_millis(50));
        assert!(
            !waiting.is_finished(),
            "the session's thread raced the signal"
        );
    }
}
