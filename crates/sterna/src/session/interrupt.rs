//! Ctrl-C and SIGTERM as the process receives them: the handlers, the count
//! they keep, and the watcher that turns it into the session's decision.
//! What an interrupt does to the call in flight is [`Interrupter`]'s.

use super::{Interrupter, ui};
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
pub(super) static INTERRUPT: AtomicUsize = AtomicUsize::new(0);
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
