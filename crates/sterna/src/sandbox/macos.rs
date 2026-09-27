//! The macOS applier: a seatbelt profile generated from a compiled
//! [`Profile`] and entered before `exec` — map line 2455, specification
//! `docs/sandbox.md`.
//!
//! The invariant: **the generated text can only ever be narrower than the
//! profile it came from.** Every `allow` term here is either fixed platform
//! machinery (the loader's own paths), the project root, or the one binary
//! the caller has already resolved and is about to run; the project-root
//! terms are emitted only when [`Profile::check`] says the root is
//! reachable — so there is no input, at render time or at spawn time, that
//! adds a path the profile does not already permit. The text is a value:
//! [`profile_text`] produces it from a profile and a path with no process
//! anywhere in sight, which is what lets a test assert on it.
//!
//! **`process-exec*` names the binary, not the directories around it** — the
//! 61D exec-roots ruling. `EXECUTABLE_ROOTS` is what a name that could not
//! be resolved falls back to, and [`ExecScope`] is how the profile says
//! which of the two happened rather than leaving a reader to infer it.
//!
//! macOS is the one platform whose OS layer can express §2's pattern
//! language exactly — `(subpath …)` for a directory glob and `(regex …)`
//! for an extension filter. This applier does **not** reach that exactness
//! yet, and [`Regime`] says so rather than implying it; see that type's
//! documentation for the reason, which is a missing accessor on `Profile`
//! and not a property of seatbelt.

use super::profile::{Access, Profile, SCRATCH_DIR};
use std::fmt;
use std::path::{Path, PathBuf};

/// The **fallback** exec grant: where a tool may be executed from when sterna
/// could not resolve its name to a path and `execvp` has to search.
///
/// Not the ordinary case any more. When the caller hands [`profile_text`] a
/// resolved binary, the profile names that one path and none of these — the
/// 61D exec-roots ruling, whose argument is that a directory list permits
/// every binary a package manager ever put there and is a list that is
/// always wrong. These stay because a name `execvp` still has to find cannot
/// be written as a `(literal …)` at all, and a profile that named nothing
/// would refuse the search rather than bound it.
///
/// They are not derived from `permissions` and are not derivable from it:
/// `.claude/settings.json` has no pattern kind that names an interpreter. A
/// path here carries no user content; §4's never-grantable set is disjoint
/// from it, and `(deny default)` refuses everything else.
const EXECUTABLE_ROOTS: [&str; 6] = [
    "/usr/bin",
    "/bin",
    "/usr/sbin",
    "/sbin",
    "/usr/local/bin",
    "/opt/homebrew/bin",
];

/// The package prefixes [`EXECUTABLE_ROOTS`] are symlinks *into*, granted for
/// exec by [`ExecScope::RootsAndProject`] alone.
///
/// The invariant: **seatbelt matches the resolved path, so a root full of
/// symlinks grants nothing on its own.** Homebrew puts `git` at
/// `/opt/homebrew/bin/git` and the file at
/// `/opt/homebrew/Cellar/git/2.55.0/bin/git`; a `(subpath "/opt/homebrew/bin")`
/// term does not reach the second, which is why `git --version` exited 126
/// under a profile that named every command line as admitted. Both are
/// system-wide package prefixes, not user data — `$HOME` is never granted
/// here, so a binary under `~/.local/bin` stays unrunnable (§4.3 rule 3, and
/// the session says so).
const PACKAGE_PREFIXES: [&str; 2] = ["/opt/homebrew", "/usr/local"];

/// The exact executable behind Homebrew's macOS Python launcher. Homebrew's
/// `bin/python3` is a Mach-O stub which starts the framework app executable;
/// Seatbelt therefore needs both literals. This is deliberately one known
/// layout, not executable-prefix discovery.
#[cfg(target_os = "macos")]
pub fn python_framework_companion(binary: &Path) -> Option<PathBuf> {
    use std::os::unix::fs::MetadataExt;

    let bin = binary.parent()?;
    let version = bin.parent()?;
    let versions = version.parent()?;
    let framework = versions.parent()?;
    let frameworks = framework.parent()?;
    let keg = frameworks.parent()?;
    let cellar = keg.parent()?.parent()?;
    let formula = keg.parent()?.file_name()?.to_str()?;
    if cellar != Path::new("/opt/homebrew/Cellar")
        || !(formula == "python" || formula.starts_with("python@"))
        || !binary.file_name()?.to_str()?.starts_with("python3")
        || versions.file_name()? != "Versions"
        || framework.file_name()? != "Python.framework"
        || frameworks.file_name()? != "Frameworks"
    {
        return None;
    }
    let binary_uid = std::fs::metadata(binary).ok()?.uid();
    let companion =
        std::fs::canonicalize(version.join("Resources/Python.app/Contents/MacOS/Python")).ok()?;
    let metadata = std::fs::metadata(&companion).ok()?;
    (companion.starts_with(version) && metadata.is_file() && metadata.uid() == binary_uid)
        .then_some(companion)
}

/// The loader's own reach. Read-only, and system-owned on every macOS
/// install: dyld resolves the shared cache and the executable's libraries
/// before the process gets to run a single instruction of its own.
const LOADER_READ_ROOTS: [&str; 10] = [
    "/usr",
    "/bin",
    "/sbin",
    "/etc",
    "/private/etc",
    "/System",
    "/Library",
    "/opt/homebrew",
    "/private/var/db/dyld",
    "/private/var/db/timezone",
];

/// Character devices a process may read. Each is a device, not a file, and
/// none of them carries project or user data.
const DEVICE_READS: [&str; 8] = [
    "/dev/null",
    "/dev/zero",
    "/dev/random",
    "/dev/urandom",
    "/dev/tty",
    "/dev/stdin",
    "/dev/stdout",
    "/dev/stderr",
];

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

/// Which paths the profile's `process-exec*` term names.
///
/// The two halves of the 61D exec-roots ruling as a value, so a session can
/// say which one is in force. `Regime` carries it and
/// [`Regime::describe`] prints it, because the ruling asks for the fallback
/// to be *visible* and a log line the caller never sees is not that.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExecScope {
    /// One `(literal …)` on the binary the caller resolved. The ordinary
    /// case, and the narrow one: a sibling in the same directory is not
    /// executable through this profile.
    ResolvedBinary,
    /// The six [`EXECUTABLE_ROOTS`], because the name could not be resolved
    /// and `execvp` has to search for it. Wider, and reported as such.
    DeclaredRoots,
    /// The [`EXECUTABLE_ROOTS`] **and the project root**, because the profile
    /// admits every command line.
    ///
    /// The invariant: **a grant that admits every command line grants those
    /// commands their binaries.** `bash` exists to exec other programs, so a
    /// shell confined to exec'ing only `/bin/bash` runs builtins and nothing
    /// else. Observed 2026-09-06 under `--yolo`: `ls /`, `git --version`,
    /// `python3 -c ...` and a project script each exited 126 while the
    /// startup line said every command line was admitted. It widens nothing
    /// for a profile that names its commands: `Bash(git*)` still admits only
    /// `git …`, and argv admission is untouched either way.
    RootsAndProject,
}

/// Which of the two grants `binary` earns.
///
/// An absolute path is one sterna resolved: `tools::invoke::exec_grant`
/// produces it with `canonicalize`, which is the only producer of the
/// resolved case and always yields an absolute path. Anything relative is a
/// bare name `execvp` still has to find, and there is no `(literal …)` that
/// names it. A path that is absolute but does not exist gets the literal
/// too, which is a refusal at exec time rather than a widening — the roots
/// would not have contained it either.
pub fn exec_scope(_profile: &Profile, _binary: &Path) -> ExecScope {
    // Every command line is admitted, so a confined command may start any
    // program under the roots: the sandbox bounds what it touches, not
    // which programs it runs.
    ExecScope::RootsAndProject
}

/// Which enforcement this applier actually achieved, so a coarser regime is
/// stated rather than implied (§3's closing sentence, applied to macOS).
///
/// Seatbelt can express every pattern in §2's table. **This applier does not
/// reach that**, and the gap is what it renders rather than anything about
/// the operating system: only the project root reaches the profile text, so
/// a rule naming a path outside it — §3's own `(literal "/etc/passwd")` —
/// is not rendered, and neither is a `deny` rule naming a path inside it.
/// The first case makes the OS layer *narrower* than the profile, which is
/// safe and merely inconvenient; the second makes it *wider*, and the
/// in-process [`Profile::check`] is what holds the line there.
/// [`Regime::path_rules`] carries the count so a session can say how many
/// rules went unrendered instead of claiming an exactness it has not got.
///
/// `Profile::rules` now enumerates them, so rendering the rest is possible
/// and is a package of its own: each rendered rule changes what a confined
/// process may touch, in both directions, and that is a decision with its
/// own acceptance rather than a comment repair.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Regime {
    /// `(deny default)` with the project root granted and §1.5's `.claude`
    /// write-deny rendered. `path_rules` is how many `Read`/`Write`/`Edit`
    /// rules the profile compiled, every one of which is enforced by
    /// [`Profile::check`] and none of which reaches the OS layer. `exec` is
    /// which paths `process-exec*` names.
    ProjectRootOnly { path_rules: usize, exec: ExecScope },
    ExplicitRoots {
        path_rules: usize,
        exec: ExecScope,
        additional: usize,
    },
}

impl Regime {
    /// The sentence a session prints at start-up. It names the coarseness,
    /// because §3 requires sterna to state it rather than imply exactness.
    pub fn describe(self) -> String {
        if let Regime::ExplicitRoots {
            path_rules,
            exec,
            additional,
        } = self
        {
            return Regime::ProjectRootOnly { path_rules, exec }.describe().replacen(
                "the project root the only readable and writable root",
                &format!("the project root and {additional} explicit additional readable and writable root(s), each with a .claude write deny"),
                1,
            );
        }
        match self {
            Regime::ProjectRootOnly { path_rules, exec } => format!(
                "seatbelt: deny by default, the project root the only readable and writable root, no network, no Mach service. \
                 File metadata stays readable filesystem-wide: existence, size, mode and a symlink's target, never a file's contents. \
                 The OS layer is directory-granular; {path_rules} path rule(s) from `.claude/settings.json` are enforced by Sterna's own pre-call check alone. \
                 {}",
                match exec {
                    ExecScope::ResolvedBinary =>
                        "Execution is granted on the one resolved binary and on nothing else, so a sibling in its directory cannot be run.",
                    ExecScope::DeclaredRoots =>
                        "The program name could not be resolved to a path, so execution fell back to the declared executable roots \
                         and is bounded by those directories rather than by one binary.",
                    ExecScope::RootsAndProject =>
                        "Every command line is admitted, so execution is granted on the declared executable roots and on the project \
                         root — the commands this profile admits can reach their own binaries. Argv admission is unchanged.",
                }
            ),
            Regime::ExplicitRoots { .. } => unreachable!("handled above"),
        }
    }
}

impl fmt::Display for Regime {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.describe())
    }
}

/// The regime [`profile_text`] achieves for `profile` and `binary`.
pub fn regime(profile: &Profile, binary: &Path) -> Regime {
    if !profile.additional_roots().is_empty() {
        return Regime::ExplicitRoots {
            path_rules: profile.rule_count(),
            exec: exec_scope(profile, binary),
            additional: profile.additional_roots().len(),
        };
    }
    Regime::ProjectRootOnly {
        path_rules: profile.rule_count(),
        exec: exec_scope(profile, binary),
    }
}

/// Renders the seatbelt profile for `profile`, executing `binary`, as text.
///
/// Deterministic in its two arguments: the same profile and the same binary
/// render the same bytes on every call, and nothing in the environment or in
/// the argv about to be spawned reaches this function. `binary` is the one
/// thing the invocation contributes and it can only ever *narrow* the
/// result — it replaces six directory grants with one path — which is how
/// invariant §1.1 survives contact with the process-spawning half of the
/// sandbox now that a caller hands something in.
pub fn profile_text(profile: &Profile, binary: &Path) -> String {
    profile_text_with_descendants(profile, binary, &[])
}

/// Renders the profile with exact binaries named by command segments that
/// already passed the profile's Bash admission. Each descendant remains a
/// literal; admitting Python does not grant its directory or a sibling.
pub fn profile_text_with_descendants(
    profile: &Profile,
    binary: &Path,
    descendants: &[PathBuf],
) -> String {
    let root = display(profile.root());
    let mut out = String::new();
    out.push_str("(version 1)\n");
    out.push_str("(deny default)\n");

    // Metadata, and only metadata, filesystem-wide. Resolving any path at
    // all requires stat-ing its parents, so a deny-default profile that
    // withholds this cannot exec. It discloses existence, size and mode —
    // and the *target* of a symlink, because `readlink(2)` is a metadata
    // operation to seatbelt and its result is the link's own contents. It
    // discloses no file's data: reading `~/.ssh` is refused, and reading a
    // symlink that points into it returns the path and not the key.
    // `Regime::describe` says so, because §3 requires a coarseness to be
    // stated rather than left for a reader to discover.
    out.push_str("(allow file-read-metadata)\n");
    // `/` itself is read by the loader on every exec.
    out.push_str("(allow file-read* (literal \"/\"))\n");

    // The 61D exec-roots ruling: the one binary the caller resolved, or the
    // declared roots when there is no path to name. `(literal …)` is a
    // single file and not a subtree, so a sibling in the same directory is
    // not reachable through this term.
    let exec = exec_scope(profile, binary);
    out.push_str("(allow process-exec*");
    match exec {
        ExecScope::ResolvedBinary => {
            out.push_str(&format!(" (literal {})", quote(&display(binary))));
        }
        ExecScope::DeclaredRoots => {
            for path in EXECUTABLE_ROOTS {
                out.push_str(&format!(" (subpath {})", quote(path)));
            }
        }
        ExecScope::RootsAndProject => {
            // A superset of the resolved arm, never a replacement: the shell
            // sterna resolved may itself live outside the roots — Homebrew's
            // `bash` canonicalises into `/opt/homebrew/Cellar/...` — and
            // dropping its literal refuses the exec of the very binary the
            // grant is about.
            // A bare name nothing resolved is never written: seatbelt
            // would read it against its own working directory.
            if binary.is_absolute() {
                out.push_str(&format!(" (literal {})", quote(&display(binary))));
            }
            for path in EXECUTABLE_ROOTS.iter().chain(PACKAGE_PREFIXES.iter()) {
                out.push_str(&format!(" (subpath {})", quote(path)));
            }
            out.push_str(&format!(" (subpath {})", quote(&root)));
            for extra in profile.additional_roots() {
                out.push_str(&format!(" (subpath {})", quote(&display(extra))));
            }
        }
    }
    // The toolchain's own binaries, and only where the scope is already a
    // root list. A compiler driver is a chain — `~/.cargo/bin/cargo` is a
    // rustup shim that execs `~/.rustup/toolchains/<t>/bin/cargo`, which
    // execs `rustc` — so a root-based grant that omits it stops at the first
    // link, which is what a `cargo test` inside a cell hit on 2026-09-17.
    // **Never in the resolved arm**: a profile that names its commands buys
    // exec on the one binary sterna resolved, and the 61D ruling exists to keep
    // it that way. A chain under a named-command profile is the descendants
    // mechanism's job, not this term's.
    if !matches!(exec, ExecScope::ResolvedBinary) {
        for path in profile.toolchain_roots() {
            out.push_str(&format!(" (subpath {})", quote(&display(path))));
        }
    }
    for descendant in descendants {
        if descendant != binary {
            out.push_str(&format!(" (literal {})", quote(&display(descendant))));
        }
    }
    out.push_str(")\n");

    // A process may read the one file it is about to become, and nothing
    // else gains a byte. `process-exec*` says the exec is permitted; the
    // image still has to be mapped, and a resolved binary outside
    // `LOADER_READ_ROOTS` — `~/.cargo/bin/cargo`, a `~/.local/bin/bash` —
    // is named by no read term this profile otherwise emits. The term is a
    // `(literal …)` on that one path and must never become a `(subpath …)`
    // on its directory: §4.3's `$HOME` rule survives only because the
    // binary's own bytes are granted and its neighbours are not. Emitted in
    // the resolved arm alone — the fallback's roots are read roots already.
    if matches!(exec, ExecScope::ResolvedBinary)
        || (matches!(exec, ExecScope::RootsAndProject) && binary.is_absolute())
    {
        out.push_str(&format!(
            "(allow file-read* (literal {}))\n",
            quote(&display(binary))
        ));
    }
    for descendant in descendants {
        if descendant != binary {
            out.push_str(&format!(
                "(allow file-read* (literal {}))\n",
                quote(&display(descendant))
            ));
        }
    }
    out.push_str("(allow process-fork)\n");
    out.push_str("(allow signal (target self))\n");
    out.push_str("(allow sysctl-read)\n");

    // §4.2. Enumerated, never blanket: an unfiltered `mach-lookup` reaches
    // securityd and answers keychain queries authoritatively. The term is
    // omitted entirely while `MACH_SERVICES` is empty, so the profile has no
    // `mach-lookup` line for a reader to mistake for a narrow one.
    let mut services = String::new();
    for name in MACH_SERVICES {
        services.push_str(&format!(" (global-name {})", quote(name)));
    }
    if !services.is_empty() {
        out.push_str(&format!("(allow mach-lookup{services})\n"));
    }

    out.push_str("(allow file-read*");
    for path in LOADER_READ_ROOTS {
        out.push_str(&format!(" (subpath {})", quote(path)));
    }
    for path in DEVICE_READS {
        out.push_str(&format!(" (literal {})", quote(path)));
    }
    out.push_str(")\n");
    out.push_str("(allow file-write-data");
    for path in DEVICE_WRITES {
        out.push_str(&format!(" (literal {})", quote(path)));
    }
    out.push_str(")\n");
    out.push_str("(allow file-ioctl (literal \"/dev/tty\") (literal \"/dev/dtracehelper\"))\n");

    // The profile decides, not this function. A settings document whose
    // `deny` list covers the project root produces no grant here at all,
    // which is the only reason these two lines are conditional.
    if profile
        .check("read", Access::Read, &profile.root().join(READ_PROBE))
        .is_ok()
    {
        out.push_str(&format!("(allow file-read* (subpath {}))\n", quote(&root)));
    }
    if profile
        .check("write", Access::Write, &profile.root().join(WRITE_PROBE))
        .is_ok()
    {
        out.push_str(&format!("(allow file-write* (subpath {}))\n", quote(&root)));
    }

    // The toolchain a build reads, and the repository a worktree belongs to.
    // Both are the profile's own decision — `Profile::check` answers from the
    // same subtrees — so the two layers cannot disagree about them.
    for path in profile.toolchain_roots() {
        out.push_str(&format!(
            "(allow file-read* (subpath {}))\n",
            quote(&display(path))
        ));
    }
    // Single files, as `(literal …)`: `git` will not start without its
    // global configuration, and granting the file grants nothing around it.
    for path in profile.toolchain_read_files() {
        out.push_str(&format!(
            "(allow file-read* (literal {}))\n",
            quote(&display(path))
        ));
    }
    // After the subtree that contains them, because seatbelt takes the last
    // matching term: a registry token is not part of the toolchain read.
    for path in profile.toolchain_credentials() {
        out.push_str(&format!(
            "(deny file-read* (literal {}))\n",
            quote(&display(&path))
        ));
    }
    for path in profile.repository_dirs() {
        out.push_str(&format!(
            "(allow file-read* file-write* (subpath {}))\n",
            quote(&display(path))
        ));
    }

    // Explicit host roots are canonical subtrees, admitted only when no
    // configured deny glob would need an approximate OS translation.
    for extra in profile.additional_roots() {
        let path = quote(&display(extra));
        out.push_str(&format!(
            "(allow file-read* file-write* (subpath {path}))\n"
        ));
        out.push_str(&format!(
            "(deny file-write* (subpath {}))\n",
            quote(&display(&extra.join(".claude")))
        ));
        out.push_str(&format!(
            "(deny file-write* (subpath {}))\n",
            quote(&display(&extra.join(".sterna")))
        ));
    }

    // §1.5: `.claude/` lives inside the writable root, so a program that
    // could write it could widen the profile it was derived from. Emitted
    // when — and only when — the profile agrees it is unwritable, so the two
    // layers cannot disagree about it.
    let dot_claude = profile.root().join(".claude");
    out.push_str(&format!(
        "(deny file-write* (subpath {}))\n",
        quote(&display(&profile.root().join(".sterna")))
    ));
    // The scratchpad the `.sterna/**` never rule exempts, after the deny because
    // seatbelt takes the last matching term. Seatbelt matches the resolved
    // path, so a link under it is judged where it points, as the profile is.
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
    if profile
        .check("write", Access::Write, &dot_claude.join(WRITE_PROBE))
        .is_err()
    {
        out.push_str(&format!(
            "(deny file-write* (subpath {}))\n",
            quote(&display(&dot_claude))
        ));
    }

    // §4.1. Unconditional in practice, because `grants_network` is: no
    // `permissions` pattern names a host, a port or a protocol, so there is
    // nothing a document could say that would reach this branch.
    if !profile.grants_network() {
        out.push_str("(deny network*)\n");
    }
    out
}

/// A file name no settings pattern is expected to spell, used to ask the
/// profile about a directory rather than about a file that happens to exist.
const READ_PROBE: &str = ".sterna-sandbox-read-probe";
const WRITE_PROBE: &str = ".sterna-sandbox-write-probe";

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

/// Applies `profile`'s seatbelt profile to `command`, which is about to
/// exec `binary`, to take effect in the child between `fork` and `exec`.
///
/// The text is still rendered here rather than accepted as an argument, and
/// that is requirement 5: the only thing a caller may hand in is the path it
/// is about to run, and every value of it produces a profile at least as
/// narrow as the roots-based one. There is no parameter through which a
/// caller could pass policy.
///
/// The `CString` is built before the fork. Everything the child does after
/// that is one call into libSystem with a pointer that already exists.
#[cfg(target_os = "macos")]
pub fn confine(
    profile: &Profile,
    binary: &Path,
    command: &mut std::process::Command,
) -> std::io::Result<()> {
    confine_with_descendants(profile, binary, &[], command)
}

#[cfg(target_os = "macos")]
pub fn confine_with_descendants(
    profile: &Profile,
    binary: &Path,
    descendants: &[PathBuf],
    command: &mut std::process::Command,
) -> std::io::Result<()> {
    use std::ffi::{CString, c_char};
    use std::os::unix::process::CommandExt;

    let text = CString::new(profile_text_with_descendants(profile, binary, descendants))
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
