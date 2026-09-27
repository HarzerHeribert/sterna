//! Observe exit without reaping, so the owned process-group id cannot be
//! recycled before remaining descendants are killed.
use std::io;
use std::process::ExitStatus;

use super::ConfinedChild;

pub(super) fn try_complete(child: &mut ConfinedChild) -> io::Result<Option<ExitStatus>> {
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    {
        if !exited(child.id())? {
            return Ok(None);
        }
        // WNOWAIT kept the leader waitable and reserves its pid/group id.
        // Its recorded exit status is unchanged by signalling the descendants.
        super::kill_group(child.id());
        child.wait().map(Some)
    }
    // Windows reaches here, and needs no part of the ordering above: a
    // `ContainedChild` holds an open handle to the process, so the kernel
    // keeps the object alive and the id cannot be recycled while sterna can
    // still name it. What it does need is the *other* half — the descendants
    // have to be stopped **here**, at the moment the leader is seen to have
    // exited, and not at `Drop`.
    //
    // Without it the completion loop can spin for ever on a child that has
    // already exited. The job's stdout write end is inherited by everything
    // the child started, so a surviving grandchild holds the pipe open, the
    // drain thread never sees EOF, and nothing between here and cancellation
    // terminates the job — `ContainedChild::drop` does, but the drop is
    // *after* the loop this function is called from. Unix has always killed
    // the group at exactly this point, and this is the same act with this
    // platform's primitive.
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        let Some(status) = child.try_wait()? else {
            return Ok(None);
        };
        // Discarded for the reason `kill_and_reap` discards its own: a job
        // whose last member has already exited is not an error, it is the
        // race this call exists to be indifferent to.
        let _ = child.kill();
        Ok(Some(status))
    }
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn exited(pid: u32) -> io::Result<bool> {
    let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
    let result = unsafe {
        libc::waitid(
            libc::P_PID,
            pid,
            &mut info,
            libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
        )
    };
    observed(result, info.si_signo != 0)
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn observed(result: i32, exited: bool) -> io::Result<bool> {
    if result == 0 {
        return Ok(exited);
    }
    let error = io::Error::last_os_error();
    if error.kind() == io::ErrorKind::Interrupted {
        Ok(false)
    } else {
        Err(error)
    }
}
