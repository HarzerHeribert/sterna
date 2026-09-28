//! The sidecars an earlier gateway left behind.
//!
//! A gateway that ends without its own shutdown -- a SIGKILL, a crash --
//! never drops its brokers, and each goes on running with parent 1, holding a
//! loopback port and the entitlement's OAuth refresh state beside whichever
//! broker the next gateway starts. Two were found alive a day after their
//! gateway, beside sixteen stale serving directories (2026-09-29).
//!
//! So a broker's start first stops the orphans of its own entitlement and
//! removes the serving directories nothing runs from. **Only an orphan is
//! stopped**: a process whose parent is 1 and whose `-config` names this
//! entitlement's own `instances/run-*` directory. A broker a live gateway
//! still owns has that gateway as its parent and is never touched, and a
//! directory younger than [`SETTLING`] may belong to a gateway starting this
//! very moment, so it is left for the next start.

use std::collections::BTreeSet;
use std::fs;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, SystemTime};

/// How old a serving directory with no process must be before it is removed:
/// the gap between a starting broker writing its config and its process
/// appearing in `ps` is milliseconds, and this is minutes past it.
const SETTLING: Duration = Duration::from_secs(60);

const RUN_PREFIX: &str = "run-";

/// Stops this entitlement's orphaned brokers and removes serving directories
/// no process runs from. Best effort: a host without `ps` reaps nothing.
pub(super) fn reap(instances_dir: &Path) {
    let Ok(output) = Command::new("ps")
        .args(["-A", "-o", "pid=,ppid=,command="])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
    else {
        return;
    };
    if !output.status.success() {
        return;
    }
    let listing = String::from_utf8_lossy(&output.stdout);
    let processes = parse(&listing);
    let prefix = format!("{}/{RUN_PREFIX}", instances_dir.display());
    for pid in orphaned(&processes, &prefix) {
        // SAFETY: `kill` takes two integers; the pid is one `ps` listed a
        // moment ago as an orphan running this entitlement's own config.
        unsafe { libc::kill(pid, libc::SIGTERM) };
    }
    let Ok(entries) = fs::read_dir(instances_dir) else {
        return;
    };
    let now = SystemTime::now();
    let names: Vec<String> = entries
        .filter_map(Result::ok)
        .filter(|entry| {
            entry
                .metadata()
                .and_then(|meta| meta.modified())
                .is_ok_and(|modified| now.duration_since(modified).unwrap_or_default() > SETTLING)
        })
        .filter_map(|entry| entry.file_name().into_string().ok())
        .collect();
    for name in unused(&processes, &prefix, &names) {
        let _ = fs::remove_dir_all(instances_dir.join(name));
    }
}

/// One `ps -o pid=,ppid=,command=` row.
struct Process<'a> {
    pid: libc::pid_t,
    ppid: libc::pid_t,
    command: &'a str,
}

/// The rows of a `ps -o pid=,ppid=,command=` listing. The command is the rest
/// of the line, spaces and all: the data directory is `Application Support`.
fn parse(listing: &str) -> Vec<Process<'_>> {
    listing
        .lines()
        .filter_map(|line| {
            let line = line.trim_start();
            let (pid, rest) = line.split_once(char::is_whitespace)?;
            let rest = rest.trim_start();
            let (ppid, command) = rest.split_once(char::is_whitespace)?;
            Some(Process {
                pid: pid.parse().ok()?,
                ppid: ppid.parse().ok()?,
                command: command.trim_start(),
            })
        })
        .collect()
}

/// Brokers of this entitlement whose gateway is gone.
fn orphaned(processes: &[Process<'_>], prefix: &str) -> Vec<libc::pid_t> {
    let config = format!(" -config {prefix}");
    processes
        .iter()
        .filter(|process| process.ppid == 1 && process.command.contains(&config))
        .map(|process| process.pid)
        .collect()
}

/// Serving directories among `names` that no process still running -- other
/// than an orphan, which is being stopped -- runs from.
fn unused<'n>(processes: &[Process<'_>], prefix: &str, names: &'n [String]) -> Vec<&'n str> {
    let in_use: BTreeSet<&str> = processes
        .iter()
        .filter(|process| process.ppid != 1)
        .filter_map(|process| {
            let at = process.command.find(prefix)? + prefix.len();
            let id = process.command[at..].split('/').next()?;
            Some(id)
        })
        .collect();
    names
        .iter()
        .filter_map(|name| {
            let id = name.strip_prefix(RUN_PREFIX)?;
            (!in_use.contains(id)).then_some(name.as_str())
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const PREFIX: &str = "/data/Application Support/brokers/entitlement-a/instances/run-";

    fn listing() -> String {
        [
            // An orphan of this entitlement: its gateway is gone.
            format!(" 8190     1 /tools/cliproxyapi -config {PREFIX}aaa/config.yaml -local-model"),
            // A broker a live gateway (pid 4000) still owns.
            format!(" 8200  4000 /tools/cliproxyapi -config {PREFIX}bbb/config.yaml -local-model"),
            // Another entitlement's orphan.
            " 8300     1 /tools/cliproxyapi -config /data/Application Support/brokers/entitlement-b/instances/run-ccc/config.yaml".to_string(),
            // Something else entirely that launchd owns.
            "  512     1 /usr/sbin/syslogd".to_string(),
        ]
        .join("\n")
    }

    #[test]
    fn only_this_entitlements_orphans_are_stopped() {
        let text = listing();
        assert_eq!(orphaned(&parse(&text), PREFIX), vec![8190]);
    }

    #[test]
    fn a_directory_a_live_broker_runs_from_is_kept_and_the_rest_go() {
        let text = listing();
        let names = [
            "run-aaa".to_string(),
            "run-bbb".to_string(),
            "run-ddd".to_string(),
            "login-eee".to_string(),
        ];
        assert_eq!(
            unused(&parse(&text), PREFIX, &names),
            vec!["run-aaa", "run-ddd"]
        );
    }
}
