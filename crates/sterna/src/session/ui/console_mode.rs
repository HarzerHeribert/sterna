//! The Windows console's input mode: ask for raw terminal input.
//!
//! **The invariant: where the console grants `ENABLE_VIRTUAL_TERMINAL_INPUT`,
//! keys, modified keys and SGR mouse reports reach Sterna as the console's
//! translated events, independent of the keyboard layout; where it refuses,
//! the console-record path with its AltGr rule runs as before; and the flag
//! never outlives the session.** Measured on the ARM64 VM, 2026-09-17: with
//! the flag, a click is a mouse record and a German-layout `[` is a plain key;
//! without it, a report's characters carry the layout's modifiers. What the
//! flag does not change: ConPTY consumes bracketed-paste markers under both
//! modes (design-decisions.md, *Bracketed paste does not reach the app
//! through ConPTY*). crossterm's raw mode clears the line, echo and
//! processed-input bits and knows nothing of this one, so it is set here
//! after raw mode and cleared here before raw mode is restored.

use std::io;
#[cfg(windows)]
use std::sync::atomic::{AtomicBool, Ordering};

#[cfg(windows)]
use windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE;
#[cfg(windows)]
use windows_sys::Win32::System::Console::{
    CONSOLE_MODE, ENABLE_VIRTUAL_TERMINAL_INPUT, GetConsoleMode, GetStdHandle, STD_INPUT_HANDLE,
    SetConsoleMode,
};

use super::terminal_input::Console;

/// The console this session reads through: raw terminal input where the
/// console grants it, the host's own reading of the input otherwise. Called
/// after raw mode is on; a refusal or an error keeps the host's console.
pub(super) fn select() -> Console {
    match enable() {
        Ok(true) => Console::VtInput,
        Ok(false) | Err(_) => Console::host(),
    }
}

/// Whether this process set the flag, so that only a flag it set is cleared.
#[cfg(windows)]
static ENABLED: AtomicBool = AtomicBool::new(false);

/// Ask the console for raw terminal input. `Ok(true)` when it was granted,
/// `Ok(false)` when the console refused the flag (an older conhost) and the
/// record path stays; an error when there is no console at all. Off Windows
/// the terminal already sends its own bytes, so there is nothing to ask for.
#[cfg(not(windows))]
fn enable() -> io::Result<bool> {
    Ok(false)
}

#[cfg(windows)]
fn enable() -> io::Result<bool> {
    let handle = unsafe { GetStdHandle(STD_INPUT_HANDLE) };
    if handle == INVALID_HANDLE_VALUE || handle.is_null() {
        return Err(io::Error::last_os_error());
    }
    let mut mode: CONSOLE_MODE = 0;
    if unsafe { GetConsoleMode(handle, &mut mode) } == 0 {
        return Err(io::Error::last_os_error());
    }
    if mode & ENABLE_VIRTUAL_TERMINAL_INPUT != 0 {
        ENABLED.store(false, Ordering::SeqCst);
        return Ok(true);
    }
    let granted = unsafe { SetConsoleMode(handle, mode | ENABLE_VIRTUAL_TERMINAL_INPUT) } != 0;
    ENABLED.store(granted, Ordering::SeqCst);
    Ok(granted)
}

/// Clear the flag this process set, leaving every other bit as it is now.
/// Idempotent, and a no-op where `enable` did not set it.
#[cfg(not(windows))]
pub(super) fn disable() {}

#[cfg(windows)]
pub(super) fn disable() {
    if !ENABLED.swap(false, Ordering::SeqCst) {
        return;
    }
    let handle = unsafe { GetStdHandle(STD_INPUT_HANDLE) };
    if handle == INVALID_HANDLE_VALUE || handle.is_null() {
        return;
    }
    let mut mode: CONSOLE_MODE = 0;
    if unsafe { GetConsoleMode(handle, &mut mode) } == 0 {
        return;
    }
    unsafe { SetConsoleMode(handle, mode & !ENABLE_VIRTUAL_TERMINAL_INPUT) };
}
