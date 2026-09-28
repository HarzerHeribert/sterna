//! The macOS applier: a seatbelt profile generated from a compiled
//! [`Profile`] and entered before `exec` (`docs/sandbox.md`, per platform).
//!
//! **The sandbox is wide.** Every file is readable and every program may run;
//! the secrets are taken back after the grants, because seatbelt takes the
//! last matching term. Writes reach the writable places
//! ([`Profile::writable_places`]), where `.git/hooks`, `.git/config`,
//! `.sterna` (but its scratchpad) and `.claude` stay read-only. The network
//! reaches this machine only, where Sterna's proxy listens and decides which
//! hosts a command may reach.
//!
//! The text is a value: [`profile_text`] produces it from a profile with no
//! process anywhere in sight, which is what lets a test assert on it.

use super::profile::{Access, Profile, SCRATCH_DIR};
use std::path::Path;

/// The Mach services a confined tool may look up, by `global-name`.
///
/// **Empty, and that is the measured base set.** A previous revision emitted
/// a blanket `(allow mach-lookup)`, which reaches every Mach service on the
/// machine including `securityd` — the Keychain, which §4.2 says is never
/// grantable on any platform. A file-only sandbox does not satisfy that
/// clause, because securityd does the keychain read on the caller's behalf
/// and the file rules never see it.
///
/// Nothing in scope needed it: with no `mach-lookup` term emitted at all,
/// `cat`, `grep`, `ls`, `sed`, `wc`, `cp`, `head`, `awk`, `find`, `sh`,
/// `env`, `xxd`, `diff` and `tar` each ran under this profile and read the
/// project. The one measured degradation is name resolution — `id` prints
/// `uid=501` where it would otherwise print `uid=501(eneas)`, because
/// `getpwuid` reaches opendirectoryd over Mach — and no tool failed for it.
///
/// A name is added here only where a tool in scope is *shown* to need it,
/// bisected the way the file roots were, with the demonstration recorded in
/// the package that adds it. `securityd`, `com.apple.SecurityServer` and
/// every other keychain endpoint are excluded by §4.2 whatever a tool wants.
const MACH_SERVICES: [&str; 0] = [];

/// Character devices a process may write. `/dev/dtracehelper` is opened for
/// write by the loader itself on every exec; refusing it costs a denial
/// record on every spawn and buys nothing.
const DEVICE_WRITES: [&str; 5] = [
    "/dev/null",
    "/dev/dtracehelper",
    "/dev/tty",
    "/dev/stdout",
    "/dev/stderr",
];

/// A file name no settings pattern is expected to spell, used to ask the
/// profile about a directory rather than about a file that happens to exist.
const WRITE_PROBE: &str = ".sterna-sandbox-write-probe";

/// The sentence the doctor and a session print.
pub fn describe(profile: &Profile) -> String {
    let network = if profile.grants_network() {
        "the network only through Sterna's proxy"
    } else {
        "no network"
    };
    format!(
        "seatbelt: every file is readable except the secrets, writes reach the writable places \
         (.git/hooks, .git/config, .sterna and .claude stay read-only), {network}, no Mach service."
    )
}

/// Renders the seatbelt profile for `profile` as text. Deterministic: the
/// same profile renders the same bytes on every call.
pub fn profile_text(profile: &Profile) -> String {
    let mut out = String::new();
    out.push_str("(version 1)\n");
    out.push_str("(deny default)\n");
    out.push_str("(allow file-read*)\n");
    out.push_str("(allow process-exec*)\n");
    out.push_str("(allow process-fork)\n");
    out.push_str("(allow signal (target same-sandbox))\n");
    out.push_str("(allow sysctl-read)\n");

    // §4.2. Enumerated, never blanket: an unfiltered `mach-lookup` reaches
    // securityd and answers keychain queries authoritatively. The term is
    // omitted entirely while `MACH_SERVICES` is empty.
    let mut services = String::new();
    for name in MACH_SERVICES {
        services.push_str(&format!(" (global-name {})", quote(name)));
    }
    if !services.is_empty() {
        out.push_str(&format!("(allow mach-lookup{services})\n"));
    }
    out.push_str("(allow file-write-data");
    for path in DEVICE_WRITES {
        out.push_str(&format!(" (literal {})", quote(path)));
    }
    out.push_str(")\n");
    out.push_str("(allow file-ioctl (literal \"/dev/tty\") (literal \"/dev/dtracehelper\"))\n");

    for place in profile.writable_places() {
        out.push_str(&format!(
            "(allow file-write* (subpath {}))\n",
            quote(&display(&place))
        ));
    }
    // What stays read-only inside them: code written there runs later
    // outside the sandbox, or widens the next session.
    for path in profile.protected_paths() {
        out.push_str(&format!(
            "(deny file-write* (subpath {}))\n",
            quote(&display(&path))
        ));
    }
    // The scratchpad the `.sterna/**` rule exempts, after the deny, and only
    // where it is the scratchpad: a link in its place is judged where it
    // points, as the profile judges it.
    let scratch = profile.root().join(SCRATCH_DIR);
    if profile
        .check("write", Access::Write, &scratch.join(WRITE_PROBE))
        .is_ok_and(|resolved| resolved.starts_with(&scratch))
    {
        out.push_str(&format!(
            "(allow file-write* (subpath {}))\n",
            quote(&display(&scratch))
        ));
    }

    // The secrets, after every grant: never readable, never writable.
    for path in profile.secret_paths() {
        out.push_str(&format!(
            "(deny file-read* file-write* (subpath {}))\n",
            quote(&display(&path))
        ));
    }

    // Network: this machine only, where the proxy listens and a dev server
    // may run; the proxy decides which hosts a command reaches.
    out.push_str("(deny network*)\n");
    if profile.grants_network() {
        out.push_str("(allow network-bind (local ip \"localhost:*\"))\n");
        out.push_str("(allow network-inbound (local ip \"localhost:*\"))\n");
        out.push_str("(allow network-outbound (remote ip \"localhost:*\"))\n");
    }
    out
}

/// Quotes a path for a seatbelt profile literal.
///
/// Backslash and double quote are the only two characters the profile
/// language's string syntax reserves, and escaping them here is what stops a
/// project directory whose name contains one from closing the term early and
/// being read as more profile — the injection this whole file would
/// otherwise be one `mkdir` away from.
fn quote(path: &str) -> String {
    let mut out = String::with_capacity(path.len() + 2);
    out.push('"');
    for ch in path.chars() {
        if ch == '"' || ch == '\\' {
            out.push('\\');
        }
        out.push(ch);
    }
    out.push('"');
    out
}

fn display(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

/// Applies `profile`'s seatbelt profile to `command`, to take effect in the
/// child between `fork` and `exec`. The `CString` is built before the fork;
/// the child makes one call into libSystem with a pointer that already
/// exists.
#[cfg(target_os = "macos")]
pub fn confine(profile: &Profile, command: &mut std::process::Command) -> std::io::Result<()> {
    use std::ffi::{CString, c_char};
    use std::os::unix::process::CommandExt;

    let text = CString::new(profile_text(profile))
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidInput, error))?;
    // SAFETY: `pre_exec` runs in the forked child before `exec`. The only
    // call it makes is `sandbox_init` on a `CString` allocated in the parent,
    // so nothing here allocates and nothing touches this process's state.
    unsafe {
        command.pre_exec(move || {
            let mut error: *mut c_char = std::ptr::null_mut();
            let applied = sandbox_init(text.as_ptr(), 0, &mut error);
            if applied != 0 {
                return Err(std::io::Error::other("sandbox_init refused the profile"));
            }
            Ok(())
        });
    }
    Ok(())
}

// libSystem's seatbelt entry point. `flags` is `0`, which is what makes the
// first argument a profile *string* rather than the name of one of Apple's
// built-in profiles.
#[cfg(target_os = "macos")]
unsafe extern "C" {
    fn sandbox_init(
        profile: *const std::ffi::c_char,
        flags: u64,
        errorbuf: *mut *mut std::ffi::c_char,
    ) -> i32;
}
