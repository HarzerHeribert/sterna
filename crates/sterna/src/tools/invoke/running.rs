//! A child its caller is waiting on, held as a value: what
//! [`super::spawn_confined`] holds between the spawn and the result, so a
//! caller that stops waiting can hand the wait on instead of killing the work.
//!
//! **A foreground command is bounded by being handed back, never by being
//! killed.** A cell waits on a command for at most its own wall clock
//! (`limits.cell_wall_clock_s`); a command still running then carries on as
//! a background job ([`crate::bg::adopt`]) and the model decides whether to
//! wait for it again or stop it. Measured 2026-09-30: with no bound at all, a
//! Python probe that never ended held two SWE-bench sessions for forty
//! minutes each, the cell's clock paused throughout, and no model turn came
//! in between.

use std::process::ExitStatus;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use super::{
    CANCEL_POLL, ConfinedChild, Confinement, ExecGrant, ToolError, ToolResult, collect,
    kill_and_reap, process,
};

/// How long a caller waits on a child before handing it on, and whom it
/// hands it to. `to` answers with the name the child now goes by.
pub(crate) struct HandOver<'a> {
    pub after: Duration,
    pub to: &'a dyn Fn(Running) -> String,
}

/// A spawned child and the two threads reading its pipes.
pub(crate) struct Running {
    child: ConfinedChild,
    stdout: JoinHandle<Vec<u8>>,
    stderr: JoinHandle<Vec<u8>>,
    tool: String,
    grant: ExecGrant,
    confinement: Confinement,
    started: Instant,
    /// Set once the child has exited while its pipes were still open --
    /// a descendant that left the process group can hold them.
    status: Option<ExitStatus>,
}

/// Where a wait left the child.
pub(crate) enum Wait {
    /// It ended, or the caller stopped it: the call's own answer.
    Ended(Result<ToolResult, ToolError>),
    /// Still running when the caller stopped waiting.
    Running(Running),
}

impl Running {
    pub(super) fn new(
        child: ConfinedChild,
        stdout: JoinHandle<Vec<u8>>,
        stderr: JoinHandle<Vec<u8>>,
        tool: &str,
        grant: ExecGrant,
        confinement: Confinement,
    ) -> Self {
        Self {
            child,
            stdout,
            stderr,
            tool: tool.to_string(),
            grant,
            confinement,
            started: Instant::now(),
            status: None,
        }
    }

    /// How long the child has been running.
    pub(crate) fn elapsed(&self) -> Duration {
        self.started.elapsed()
    }

    /// Waits until the child has ended and its pipes are read, however long
    /// that takes, unless `stopped` says to kill it.
    pub(crate) fn finish(self, stopped: &dyn Fn() -> bool) -> Result<ToolResult, ToolError> {
        match self.wait(stopped, None) {
            Wait::Ended(outcome) => outcome,
            // Unreachable: with no `until` the wait only ends one way.
            Wait::Running(running) => running.finish(stopped),
        }
    }

    /// Waits until the child has ended and its pipes are read, `stopped`
    /// says to kill it, or `until` passes and it is handed back running.
    pub(super) fn wait(mut self, stopped: &dyn Fn() -> bool, until: Option<Instant>) -> Wait {
        let patient = |now: Instant| until.is_none_or(|until| now < until);
        while self.status.is_none() {
            if stopped() {
                kill_and_reap(&mut self.child);
                return Wait::Ended(Err(self.cancelled()));
            }
            match process::try_complete(&mut self.child) {
                Ok(Some(status)) => self.status = Some(status),
                Ok(None) if patient(Instant::now()) => std::thread::sleep(CANCEL_POLL),
                Ok(None) => return Wait::Running(self),
                // The wait itself failing leaves a running child nothing here
                // can observe again, so it is killed on the way out. Reporting
                // it as `Spawn` is not new lumping: `output()` raised the same
                // variant for its own wait and read failures.
                Err(error) => {
                    kill_and_reap(&mut self.child);
                    return Wait::Ended(Err(ToolError::Spawn {
                        tool: self.tool.clone(),
                        program: self.grant.binary.clone(),
                        error: error.to_string(),
                    }));
                }
            }
        }
        // Descendants in the owned group have now been stopped. Keep the final
        // pipe drain cancellable too; a process that deliberately escaped the
        // group must not hold this host callback indefinitely through its pipe.
        while !self.stdout.is_finished() || !self.stderr.is_finished() {
            if stopped() {
                return Wait::Ended(Err(self.cancelled()));
            }
            if !patient(Instant::now()) {
                return Wait::Running(self);
            }
            std::thread::sleep(CANCEL_POLL);
        }
        Wait::Ended(Ok(ToolResult {
            modified: None,
            exit_code: self.status.and_then(|status| status.code()),
            tool: self.tool,
            stdout: collect(self.stdout),
            stderr: collect(self.stderr),
            grant: self.grant,
            confinement: self.confinement,
        }))
    }

    fn cancelled(&self) -> ToolError {
        ToolError::Cancelled {
            tool: self.tool.clone(),
        }
    }
}
