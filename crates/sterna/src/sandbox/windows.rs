//! The Windows applier: an **AppContainer**, entered at `CreateProcessW` —
//! map line 2455, specification `docs/sandbox.md`.
//!
//! The invariant: **the confinement is the spawn.** There is no function here
//! that decorates a `Command` and hands it back; stable `std` cannot attach a
//! token or a `PROC_THREAD_ATTRIBUTE` to a `Command` at all (`raw_attribute`
//! is nightly, rust-lang/rust#114854), so a decorating signature could only be
//! honest by lying. [`spawn`] performs the container creation, the ACL grant
//! and the `CreateProcessW` in one call, and it is the only expression in this
//! crate that starts a child on Windows. "There is no unconfined path" is
//! therefore a property of the control flow rather than a promise in a
//! comment.
//!
//! **The job object is not a sandbox and grants nothing.** Glasshouse already
//! creates one (`crates/glasshouse/src/pty/process.rs`, with
//! `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`) and `crates/glasshouse/src/pty/mod.rs`
//! says it outright — *"this is structure within the sanctioned harness API,
//! not a sandbox."* It is a **lifetime** primitive: it guarantees the process
//! tree dies with sterna, and [`spawn`] creates one for exactly that reason —
//! it is this platform's `process_group(0)`, the thing that lets a cancelled
//! `bash` call take its background jobs with it. The map line's phrase
//! "Windows job objects" must not be read as naming the grant mechanism,
//! because it does not.
//!
//! **One regime ships, and it is the AppContainer alone.** A
//! `WRITE_RESTRICTED` token removes the user's own write reach without
//! isolating anything, has no bearing on sockets, and would report a
//! confinement it cannot back; a regime built on it is not offered here, and
//! there is no variant of [`Regime`] for it, so it cannot reach a spawn.
//! Strengthening the container with a restricted primary token is a
//! successor, recorded in `sandbox-grants.md` §3.
//!
//! Not expressible: ACLs are per-object, so an extension-filtered glob gets
//! Linux's treatment — a directory-granular ACE, with the filter exact in
//! [`Profile::check`] alone. Case-insensitivity is the platform's, and
//! `Profile`'s matcher already makes the same decision on every host rather
//! than three different ones.
//!
//! **And the 61D exec-roots ruling is not expressible here either, which is
//! stated rather than worked around.** macOS names the resolved binary in a
//! `(literal …)` and Linux gives it a Landlock rule; the AppContainer model
//! has no equivalent, because a system binary is executable by
//! `ALL APPLICATION PACKAGES` through an ACE on the *binary*, and narrowing
//! that would mean rewriting the ACLs of files sterna does not own. It does
//! not. What this module does instead is **refuse**: a binary whose own ACL
//! does not already admit application packages cannot be loaded inside the
//! container, so [`spawn`] declines it by name rather than widening anything
//! to make it run. [`READ_RIGHTS`] and [`READ_WRITE_RIGHTS`] carry no
//! `FILE_EXECUTE`, so nothing model-authored runs (map line 2457).

use std::fmt;
use std::path::{Path, PathBuf};

use super::profile::{Access, Profile, SCRATCH_DIR};

/// Which enforcement this applier achieved.
///
/// Two variants, and the missing third is the point: a half-applied
/// confinement is a refusal here, never a value. [`spawn`] returns
/// [`Regime::AppContainer`] or it returns an error.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Regime {
    /// An AppContainer declaring no capabilities, entered through the
    /// `SECURITY_CAPABILITIES` attribute of a `STARTUPINFOEXW`. The project
    /// directory's ACL admits that container's SID and nothing else.
    AppContainer,
    /// Nothing could be applied, so nothing was spawned. Reported by
    /// [`regime`] as a host's answer; never carried by a running child.
    Unconfined,
}

impl Regime {
    /// The sentence a session prints at start-up.
    ///
    /// It states three things this platform is otherwise silent about: that
    /// the container SID is per user as well as per project, that the
    /// container has a **second** writable root by construction, and that the
    /// network claim depends on a service rather than on the access check.
    pub fn describe(self) -> String {
        match self {
            Regime::AppContainer => concat!(
                "an AppContainer declaring no capabilities, entered at CreateProcessW through a ",
                "SECURITY_CAPABILITIES attribute; the project directory's ACL admits that ",
                "container's SID alone. The container SID is derived from the project root and ",
                "this user's own SID together, so no other account on this machine can enter it. ",
                "ACLs are per-object: an extension-filtered pattern is enforced at directory ",
                "granularity here and exactly by Sterna's own pre-call check. There are two ",
                "writable roots on this platform and not one: the project root, and ",
                "%LOCALAPPDATA%\\Packages\\<container>\\, which Windows creates for the container ",
                "itself and which no rule in a settings document can remove. The network is not ",
                "removed by the access check: an AppContainer without internetClient is refused ",
                "by the Windows Filtering Platform, so that refusal holds only while the Windows ",
                "Firewall service is running -- sterna reports which, and does not assume it."
            )
            .to_string(),
            Regime::Unconfined => concat!(
                "no OS-level confinement: the AppContainer could not be created, so Sterna spawned ",
                "nothing. This is a refusal, not a degraded mode."
            )
            .to_string(),
        }
    }
}

impl fmt::Display for Regime {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.describe())
    }
}

/// Whether the network is actually removed, and by what.
///
/// **This is deliberately not a method on [`Regime`].** An AppContainer that
/// declares no `internetClient` capability is denied sockets by the Windows
/// Filtering Platform — a *service*, not the object-manager access check that
/// enforces every other grant here. A pure function of the regime value would
/// be claiming an enforcement it cannot see, which is the one thing a sandbox
/// report may not do, so the answer is measured instead.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NetworkIsolation {
    /// The Windows Firewall service reports itself running, so the missing
    /// `internetClient` capability is enforced.
    EnforcedByFirewall,
    /// The service is installed and is **not** running. The container has no
    /// network capability and nothing is enforcing that.
    NotEnforced,
    /// The service could not be queried. Not a claim in either direction.
    Unknown,
}

impl NetworkIsolation {
    pub fn as_str(self) -> &'static str {
        match self {
            NetworkIsolation::EnforcedByFirewall => {
                "removed (no internetClient capability; the Windows Firewall service is running)"
            }
            NetworkIsolation::NotEnforced => {
                "NOT removed: the container declares no internetClient capability, but the \
                 Windows Firewall service that enforces that is not running"
            }
            NetworkIsolation::Unknown => {
                "unknown: the Windows Firewall service could not be queried, so sterna does not \
                 claim the network is removed"
            }
        }
    }
}

impl fmt::Display for NetworkIsolation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

// The access mask, bit by bit, because a broad constant is how a right
// nobody chose ends up in an inheritable ACE on a person's project.
//
// **This block replaces `0x001F_01FF & !FILE_EXECUTE`, and the reason is a
// measured escape.** `FILE_ALL_ACCESS` minus one bit is not a decision, it is
// the absence of one: it carried `FILE_DELETE_CHILD`, which on a directory is
// the right to delete or rename a child **whose own DACL grants nothing** —
// exactly what the `.claude` carve-out is — and a confined child used it to
// rename `.claude` away, create a fresh one that inherited the root's write
// grant, and write the settings document the next session compiles from.
// Invariant 1 defeated by a `move`. (That escape had a second, independently
// sufficient cause and [`FILE_DELETE_CHILD`] names it.) It also carried
// `WRITE_DAC` and
// `WRITE_OWNER`, which are strictly worse: with either, a confined child can
// rewrite the ACL of every file in the project — including adding the
// `ALL APPLICATION PACKAGES` execute ACE that would make a file it wrote
// loadable inside the container.
//
// So each bit below is named and each one that survives says why. A bit that
// cannot state a reason is not in [`READ_WRITE_RIGHTS`].

/// `FILE_READ_DATA`, and `FILE_LIST_DIRECTORY` on a directory. Reading a
/// file, and listing the directory that holds it.
pub const FILE_READ_DATA: u32 = 0x0000_0001;

/// `FILE_WRITE_DATA`, and `FILE_ADD_FILE` on a directory. Writing a file, and
/// creating one.
pub const FILE_WRITE_DATA: u32 = 0x0000_0002;

/// `FILE_APPEND_DATA`, and `FILE_ADD_SUBDIRECTORY` on a directory. Appending,
/// and `mkdir`.
pub const FILE_APPEND_DATA: u32 = 0x0000_0004;

/// `FILE_READ_EA`. Part of an ordinary open; grants no data a read does not.
pub const FILE_READ_EA: u32 = 0x0000_0008;

/// `FILE_WRITE_EA`. Extended attributes on a file the container may already
/// rewrite entirely, so it adds no reach.
pub const FILE_WRITE_EA: u32 = 0x0000_0010;

/// `FILE_EXECUTE`, and `FILE_TRAVERSE` on a directory.
///
/// Withheld from both grants, and named here so a test on any host can say
/// which bit it is. The project tree is where model-authored files live and
/// map line 2457 says none of them executes. Traversal is unaffected in
/// practice: every token on Windows holds `SeChangeNotifyPrivilege`, which
/// bypasses the traverse check, and the caged child in
/// `tests/sandbox_windows_cage.rs` reads a file below the granted root to
/// prove it.
pub const FILE_EXECUTE: u32 = 0x0000_0020;

/// `FILE_DELETE_CHILD`.
///
/// **Withheld, and this is half of escape 3's fix.** On a directory it is the
/// right to delete or rename a child *without* any access to the child
/// itself, so an inheritable `FILE_DELETE_CHILD` on the project root reaches
/// straight past the `.claude` carve-out — the one object in the tree whose
/// DACL deliberately grants the container nothing but reads. Deleting an
/// ordinary project file still works, through [`DELETE`] on the file itself.
///
/// **The other half is that [`READ_RIGHTS`] carries no [`DELETE`]**, because
/// a rename needs `DELETE` on the object *or* `FILE_DELETE_CHILD` on its
/// parent and either one is enough. Measured on the Windows ARM64 VM,
/// 2026-09-09, one right handed back at a time against the shipping build: a
/// root at `0x0013_01DF` moved the carve-out aside, and so did a carve-out at
/// `0x0013_0089`, while `0x0013_019F` over `0x0012_0089` refused. Neither
/// withholding may be simplified away on the grounds that the other is doing
/// the work; `sandbox-grants.md` §3 carries the table.
pub const FILE_DELETE_CHILD: u32 = 0x0000_0040;

/// `FILE_READ_ATTRIBUTES`. Required by every open; `stat` is this bit.
pub const FILE_READ_ATTRIBUTES: u32 = 0x0000_0080;

/// `FILE_WRITE_ATTRIBUTES`. Timestamps and file attributes, which a build or
/// a copy inside the project sets. No access to another object follows from
/// it.
pub const FILE_WRITE_ATTRIBUTES: u32 = 0x0000_0100;

/// `DELETE`, on the object itself. Removing a file the container may already
/// truncate to nothing, and the right a rename needs — held **on the object**
/// rather than on its parent, which is the whole difference from
/// [`FILE_DELETE_CHILD`].
pub const DELETE: u32 = 0x0001_0000;

/// `READ_CONTROL`. Reading a DACL, which `icacls` and a diagnostic do.
pub const READ_CONTROL: u32 = 0x0002_0000;

/// `WRITE_DAC`.
///
/// **Withheld.** It is the right to rewrite an object's DACL, and it was
/// inheritable across the whole project. A confined child holding it can
/// grant itself anything the object's owner could — including the
/// `ALL APPLICATION PACKAGES` execute ACE that decides whether the container
/// can load a file the model wrote.
pub const WRITE_DAC: u32 = 0x0004_0000;

/// `WRITE_OWNER`.
///
/// **Withheld.** Taking ownership implies [`WRITE_DAC`] whatever the DACL
/// says, so leaving it in would leave that bit in by another spelling.
pub const WRITE_OWNER: u32 = 0x0008_0000;

/// `SYNCHRONIZE`. Every synchronous file handle needs it.
pub const SYNCHRONIZE: u32 = 0x0010_0000;

/// The rights deliberately withheld from **both** grants, as one value a test
/// can assert against a mask without restating the reasoning.
pub const WITHHELD_RIGHTS: u32 = FILE_EXECUTE | FILE_DELETE_CHILD | WRITE_DAC | WRITE_OWNER;

/// Read, and nothing else. `0x0012_0089`.
///
/// This is each carve-out's whole grant, so every bit in it is a bit a program
/// holds on `.claude` and `.sterna`: it opens a settings document and reads it,
/// and there is no bit here through which it could change one.
///
/// **The absent [`DELETE`] is load-bearing and is half of escape 3's fix**,
/// because a rename needs it on the object when the parent withholds
/// [`FILE_DELETE_CHILD`]. Measured: a carve-out at `0x0013_0089` let a
/// confined child move `.claude` aside with the shipping root mask in place.
pub const READ_RIGHTS: u32 =
    FILE_READ_DATA | FILE_READ_EA | FILE_READ_ATTRIBUTES | READ_CONTROL | SYNCHRONIZE;

/// The above, plus writing, creating, appending and deleting the object
/// itself. `0x0013_019F`.
///
/// It is inheritable down the project root, so read it as the answer to
/// *"what may a confined program do to any file in this project"* — and note
/// which questions it cannot reach: it cannot execute anything
/// ([`FILE_EXECUTE`]), cannot rewrite any object's security
/// ([`WRITE_DAC`], [`WRITE_OWNER`]), and cannot touch a child whose own DACL
/// refuses it ([`FILE_DELETE_CHILD`]).
pub const READ_WRITE_RIGHTS: u32 = READ_RIGHTS
    | FILE_WRITE_DATA
    | FILE_APPEND_DATA
    | FILE_WRITE_EA
    | FILE_WRITE_ATTRIBUTES
    | DELETE;

/// Which access the AppContainer's capability SID is granted, and where.
///
/// A value, so the derivation is assertable on a host that cannot run a
/// single Win32 call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AclGrants {
    /// Directories whose ACL admits the capability SID for read. **Not for
    /// execute** — see [`READ_RIGHTS`].
    pub read_only: Vec<PathBuf>,
    /// Directories whose ACL admits it for read, write and delete. Not for
    /// execute either.
    pub read_write: Vec<PathBuf>,
    /// The binary these grants were derived for, recorded and not acted on.
    ///
    /// The 61D exec-roots ruling asks the OS to grant execution on this path
    /// and on nothing else. Windows cannot: system binaries are executable
    /// through an ACE on themselves for `ALL APPLICATION PACKAGES`, and sterna
    /// does not rewrite the ACLs of files it does not own. So this is the one
    /// platform where the narrow grant is *not* enforced; what [`spawn`] does
    /// instead is refuse a binary the container cannot load at all.
    pub executable: PathBuf,
    /// Whether the AppContainer declares `internetClient`. Always `false`:
    /// no `permissions` pattern names a host, a port or a protocol, so a
    /// network capability would have to be invented (§4.1).
    pub internet_client: bool,
}

/// Derives the ACL grants `profile` implies for a child about to exec
/// `binary`.
///
/// The project root, and nothing else. `binary` adds no ACE — see
/// [`AclGrants::executable`] for why this platform cannot honour the narrow
/// exec grant — so it can widen nothing here even in principle. Every system
/// directory an AppContainer needs is already readable by
/// `ALL APPLICATION PACKAGES`, so there is nothing to add for them and
/// nothing here that could be widened into them. `.claude/` and `.sterna/` are
/// carved back to read-only inside a writable root (§1.5), and both decisions are
/// [`Profile::check`]'s rather than this function's.
pub fn acl_grants(profile: &Profile, binary: &Path) -> AclGrants {
    let root = profile.root().to_path_buf();
    let mut read_only = Vec::new();
    let mut read_write = Vec::new();
    if grants(profile, Access::Write, &root) {
        read_write.push(root.clone());
        for protected in [root.join(".claude"), root.join(".sterna")] {
            if !grants(profile, Access::Write, &protected) {
                read_only.push(protected);
            }
        }
        // The scratchpad `.sterna/**`'s never rule exempts: its own protected
        // read-write grant inside the read-only `.sterna` carve-out.
        let scratch = root.join(SCRATCH_DIR);
        // A link or junction in its place is not the scratchpad; granting it
        // would rewrite the DACL of whatever it points at.
        let linked = std::fs::symlink_metadata(&scratch).is_ok_and(|m| m.file_type().is_symlink());
        if !linked && grants(profile, Access::Write, &scratch) {
            read_write.push(scratch);
        }
    } else if grants(profile, Access::Read, &root) {
        read_only.push(root);
    }
    AclGrants {
        read_only,
        read_write,
        executable: binary.to_path_buf(),
        internet_client: profile.grants_network(),
    }
}

/// The AppContainer profile name for a project root **and a user**.
///
/// **`user` is not decoration, it is the security property.** An AppContainer
/// SID is a pure function of the profile name, and the name used to be a hash
/// of the project path alone — so any local process that could guess or read
/// the path, including one running as a different account, could derive the
/// same SID, create the same container and inherit whatever the project ACL
/// grants it. Folding the current user's own SID in means the name is
/// derivable only by someone who already knows both, and the ACE
/// [`grant_project_acl`] writes admits a principal no other account can
/// become.
///
/// Stable across sessions for one user and one root, so a container is reused
/// rather than accumulating one profile per launch. It is not a secret and
/// carries no path: `CreateAppContainerProfile` caps the name at 64 UTF-16
/// code units, and a project path is routinely longer than that.
///
/// The `Glasshouse.Pane.` prefix predates the rename to Sterna and is kept on
/// purpose: the SID is derived from the name, so a new prefix would give every
/// project a new container and leave the old persistent profile and its
/// project ACE behind, since nothing here deletes either.
pub fn container_name(profile: &Profile, user: &str) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    let mut fold = |bytes: &[u8]| {
        for byte in bytes {
            hash ^= u64::from(*byte);
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
    };
    let root = profile.root().to_string_lossy();
    // Length-prefixed, so a root ending in the user's SID and a root that
    // does not cannot collide on the concatenation.
    fold(&(root.len() as u64).to_le_bytes());
    fold(root.as_bytes());
    fold(&(user.len() as u64).to_le_bytes());
    fold(user.as_bytes());
    format!("Glasshouse.Pane.{hash:016x}")
}

/// A file name no settings pattern is expected to spell, so the question put
/// to the profile is about the directory rather than about whichever file
/// happens to exist in it.
const PROBE: &str = ".sterna-sandbox-probe";

fn grants(profile: &Profile, access: Access, directory: &Path) -> bool {
    profile
        .check(access.as_str(), access, &directory.join(PROBE))
        .is_ok()
}

/// Which of the child's three standard handles is a pipe back to sterna, and
/// which is the null device.
///
/// The Windows spawn creates these by hand — `std` would create them, but
/// `std` is also what cannot attach the container, so the whole
/// `CreateProcessW` is this module's and the handles come with it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Pipes {
    pub stdin: bool,
    pub stdout: bool,
    pub stderr: bool,
}

/// Appends one `CreateProcessW` command-line argument, quoted the way the
/// C runtime's `CommandLineToArgvW` reads it back.
///
/// A pure function of UTF-16, so the escaping is assertable on any host —
/// which matters more here than usual: `CreateProcessW` takes one string and
/// the child re-splits it, so a quoting bug is an argument-injection bug, and
/// the argument that would be injected is a path the model chose.
///
/// The rule, which is `CommandLineToArgvW`'s and not an invention: a run of
/// backslashes is literal unless a `"` follows it, in which case the run is
/// doubled and the quote escaped; a trailing run inside quotes is doubled
/// because the closing quote follows it.
pub fn quote_argument(argument: &[u16], out: &mut Vec<u16>, force_quotes: bool) {
    const QUOTE: u16 = b'"' as u16;
    const BACKSLASH: u16 = b'\\' as u16;
    let quote = force_quotes
        || argument.is_empty()
        || argument
            .iter()
            .any(|unit| *unit == b' ' as u16 || *unit == b'\t' as u16);
    if quote {
        out.push(QUOTE);
    }
    let mut backslashes = 0usize;
    for unit in argument {
        if *unit == BACKSLASH {
            backslashes += 1;
        } else {
            if *unit == QUOTE {
                out.extend(std::iter::repeat_n(BACKSLASH, backslashes + 1));
            }
            backslashes = 0;
        }
        out.push(*unit);
    }
    if quote {
        out.extend(std::iter::repeat_n(BACKSLASH, backslashes));
        out.push(QUOTE);
    }
}

/// The whole `lpCommandLine`, NUL-terminated.
///
/// `program` is quoted unconditionally: it is a resolved absolute path and
/// `C:\Program Files\…` is the ordinary case, not the exotic one.
pub fn command_line(program: &[u16], arguments: &[Vec<u16>]) -> Vec<u16> {
    let mut line = Vec::new();
    quote_argument(program, &mut line, true);
    for argument in arguments {
        line.push(b' ' as u16);
        quote_argument(argument, &mut line, false);
    }
    line.push(0);
    line
}

/// How a child's `lpCommandLine` is assembled from its arguments.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineShape {
    /// Every argument quoted by `CommandLineToArgvW`'s rules — what every
    /// C-runtime program reads back: [`command_line`].
    Argv,
    /// `cmd.exe /d /s /c "<command>"`: the switches as arguments and the
    /// **last** argument verbatim between one pair of quotes:
    /// [`shell_command_line`], and the reason it exists.
    CmdTail,
}

/// The whole `lpCommandLine` for `cmd.exe`, NUL-terminated: the program
/// quoted, `switches` as they are, and `command` verbatim between one pair
/// of quotes.
///
/// **`cmd.exe` does not read its line by `CommandLineToArgvW`'s rules, and
/// that is why this is not [`command_line`].** With `/s`, cmd strips exactly
/// the first and the last quote after `/c` and hands everything between them
/// to its own parser, so the model's command reaches cmd byte for byte.
/// [`quote_argument`] would instead escape every `"` in it as `\"` and
/// double the backslashes before them — escapes cmd never removes, which
/// turns `findstr /c:"value = 10"` into a search for `\value`. A quote
/// inside the command is therefore *not* escaped here, and it cannot break
/// out of anything: cmd pairs the first quote with the last, wherever the
/// ones between fall, and there is no argv for a stray one to extend.
pub fn shell_command_line(program: &[u16], switches: &[Vec<u16>], command: &[u16]) -> Vec<u16> {
    const QUOTE: u16 = b'"' as u16;
    let mut line = Vec::new();
    quote_argument(program, &mut line, true);
    for switch in switches {
        line.push(b' ' as u16);
        quote_argument(switch, &mut line, false);
    }
    line.push(b' ' as u16);
    line.push(QUOTE);
    line.extend_from_slice(command);
    line.push(QUOTE);
    line.push(0);
    line
}

/// Uppercases the ASCII range of a UTF-16 environment variable name.
///
/// Windows compares variable names case-insensitively and sorts the
/// environment block by the folded name. Only ASCII is folded here, and that
/// is a stated limit rather than an oversight: every name this crate sets or
/// removes is ASCII, and folding the rest would need the platform's own
/// casing table, which is not available to a function that has to run on
/// three operating systems.
fn folded(name: &[u16]) -> Vec<u16> {
    name.iter()
        .map(|unit| {
            if (b'a' as u16..=b'z' as u16).contains(unit) {
                unit - 32
            } else {
                *unit
            }
        })
        .collect()
}

/// Builds the child's `lpEnvironment` block from the inherited environment
/// and the `Command`'s own modifications.
///
/// A pure function, so the credential scrub `invoke` performs by name is
/// assertable on any host — including the case that matters, which is that a
/// removal actually removes and does not merely fail to add.
///
/// The block is sorted by folded name because `CreateProcessW` documents that
/// it must be, and it is double-NUL terminated because an empty block is
/// still two units long.
pub fn environment_block(
    inherited: impl IntoIterator<Item = (Vec<u16>, Vec<u16>)>,
    changes: impl IntoIterator<Item = (Vec<u16>, Option<Vec<u16>>)>,
) -> Vec<u16> {
    let mut table: std::collections::BTreeMap<Vec<u16>, (Vec<u16>, Vec<u16>)> =
        std::collections::BTreeMap::new();
    for (name, value) in inherited {
        if name.is_empty() {
            continue;
        }
        table.insert(folded(&name), (name, value));
    }
    for (name, value) in changes {
        if name.is_empty() {
            continue;
        }
        let key = folded(&name);
        match value {
            Some(value) => {
                table.insert(key, (name, value));
            }
            None => {
                table.remove(&key);
            }
        }
    }
    let mut block = Vec::new();
    for (name, value) in table.into_values() {
        block.extend_from_slice(&name);
        block.push(b'=' as u16);
        block.extend_from_slice(&value);
        block.push(0);
    }
    block.push(0);
    block
}

/// Why [`spawn`] did not produce a child.
///
/// The two are different in kind and the caller reports them differently. A
/// [`SpawnError::NotConfinable`] is the sandbox's own answer -- no user SID,
/// no container, an image the container cannot load, an ACL that would not
/// take the grant -- and it reaches a program as `PermissionDenied`, which it
/// can catch (§1.4). A [`SpawnError::NotStarted`] is `CreateProcessW`
/// declining a program the container was already ready for, which is the
/// operating system's answer and not a permission decision. Collapsing them
/// would report a missing binary as a sandbox refusal.
///
/// None leaves a child behind. `NotStarted` is the only one that reaches
/// `CreateProcessW` at all, and it is the case where that call returned zero;
/// `NotPrepared` is the explicit bypass's failure before that call, where no
/// container was asked for.
#[derive(Debug)]
pub enum SpawnError {
    NotConfinable(std::io::Error),
    NotStarted(std::io::Error),
    /// The explicit bypass's own pre-`CreateProcessW` failure — a relative
    /// program, a pipe, the job or the attribute list — where no container
    /// was asked for, so "the AppContainer could not be entered" would be
    /// false. Never a permission decision.
    NotPrepared(std::io::Error),
}

impl fmt::Display for SpawnError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SpawnError::NotConfinable(error) => write!(
                f,
                "the AppContainer could not be entered, so nothing was spawned: {error} \
                 (docs/sandbox.md, per platform)"
            ),
            SpawnError::NotStarted(error) => error.fmt(f),
            SpawnError::NotPrepared(error) => write!(
                f,
                "the unconfined child could not be prepared (its pipes, job or command line), \
                 so nothing was spawned: {error}"
            ),
        }
    }
}

/// Crate-private on purpose: the unconfined `CreateProcessW` is reachable
/// only from `tools::invoke::spawn_bypassed`'s Windows arm, behind
/// `Profile::os_sandbox_bypassed()`.
#[cfg(target_os = "windows")]
pub(crate) use platform::spawn_unconfined;
#[cfg(target_os = "windows")]
pub use platform::{
    Ace, AppContainer, ContainedChild, container_aces, container_masks, current_user_sid,
    grant_project_acl, image_admits_app_containers, network_isolation, regime, spawn,
};

#[cfg(target_os = "windows")]
mod platform {
    use super::{
        AclGrants, LineShape, Pipes, READ_RIGHTS, READ_WRITE_RIGHTS, Regime, SpawnError,
        acl_grants, command_line, container_name, environment_block, shell_command_line,
    };
    use crate::sandbox::profile::{Access, Profile};
    use std::ffi::c_void;
    use std::fs::File;
    use std::io;
    use std::os::windows::ffi::{OsStrExt, OsStringExt};
    use std::os::windows::io::FromRawHandle;
    use std::os::windows::process::ExitStatusExt;
    use std::path::Path;
    use std::process::{Command, ExitStatus};
    use windows_sys::Win32::Foundation::{
        CloseHandle, ERROR_ALREADY_EXISTS, GENERIC_READ, GENERIC_WRITE, HANDLE,
        HANDLE_FLAG_INHERIT, INVALID_HANDLE_VALUE, LocalFree, SetHandleInformation, WAIT_OBJECT_0,
    };
    use windows_sys::Win32::Security::Authorization::{
        ConvertSidToStringSidW, ConvertStringSidToSidW, EXPLICIT_ACCESS_W, GetNamedSecurityInfoW,
        NO_MULTIPLE_TRUSTEE, SE_FILE_OBJECT, SET_ACCESS, SetEntriesInAclW, SetNamedSecurityInfoW,
        TRUSTEE_IS_SID, TRUSTEE_IS_WELL_KNOWN_GROUP, TRUSTEE_W,
    };
    use windows_sys::Win32::Security::Isolation::{
        CreateAppContainerProfile, DeriveAppContainerSidFromAppContainerName,
    };
    use windows_sys::Win32::Security::{
        ACE_HEADER, ACL, ACL_REVISION, ACL_SIZE_INFORMATION, AclSizeInformation, AddAce,
        DACL_SECURITY_INFORMATION, EqualSid, GetAce, GetAclInformation, GetTokenInformation,
        INHERIT_ONLY_ACE, INHERITED_ACE, InitializeAcl, PROTECTED_DACL_SECURITY_INFORMATION,
        PSECURITY_DESCRIPTOR, PSID, SECURITY_ATTRIBUTES, SECURITY_CAPABILITIES,
        SUB_CONTAINERS_AND_OBJECTS_INHERIT, TOKEN_QUERY, TOKEN_USER, TokenUser,
    };
    use windows_sys::Win32::Storage::FileSystem::{
        CreateFileW, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
    };
    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
        JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
        SetInformationJobObject, TerminateJobObject,
    };
    use windows_sys::Win32::System::Pipes::CreatePipe;
    use windows_sys::Win32::System::Services::{
        CloseServiceHandle, OpenSCManagerW, OpenServiceW, QueryServiceStatus, SC_MANAGER_CONNECT,
        SERVICE_QUERY_STATUS, SERVICE_RUNNING, SERVICE_STATUS,
    };
    use windows_sys::Win32::System::Threading::{
        CREATE_NO_WINDOW, CREATE_SUSPENDED, CREATE_UNICODE_ENVIRONMENT, CreateProcessW,
        DeleteProcThreadAttributeList, EXTENDED_STARTUPINFO_PRESENT, GetCurrentProcess,
        GetExitCodeProcess, InitializeProcThreadAttributeList, LPPROC_THREAD_ATTRIBUTE_LIST,
        OpenProcessToken, PROC_THREAD_ATTRIBUTE_HANDLE_LIST,
        PROC_THREAD_ATTRIBUTE_SECURITY_CAPABILITIES, PROCESS_INFORMATION, ResumeThread,
        STARTF_USESTDHANDLES, STARTUPINFOEXW, TerminateProcess, UpdateProcThreadAttribute,
        WaitForSingleObject,
    };

    /// `ACCESS_ALLOWED_ACE_TYPE` and `ACCESS_DENIED_ACE_TYPE`. Named locally
    /// rather than by pulling in `Win32_System_SystemServices` for two bytes.
    const ALLOWED_ACE: u8 = 0;
    const DENIED_ACE: u8 = 1;

    /// `ALL APPLICATION PACKAGES` and `ALL RESTRICTED APPLICATION PACKAGES`.
    ///
    /// An image an AppContainer can load carries an execute ACE for one of
    /// these on the file itself. They are the two well-known package groups
    /// every AppContainer is a member of, and the reason a system binary runs
    /// in a container without sterna touching its ACL.
    const APPLICATION_PACKAGE_SIDS: [&str; 2] = ["S-1-15-2-1", "S-1-15-2-2"];

    /// Every execute-bearing right an allow ACE could carry.
    ///
    /// `FILE_EXECUTE`, `GENERIC_EXECUTE`, `GENERIC_ALL`. `FILE_ALL_ACCESS`
    /// contains `FILE_EXECUTE` already, so it needs no separate bit.
    const EXECUTE_BITS: u32 = 0x0000_0020 | 0x2000_0000 | 0x1000_0000;

    /// A `HANDLE` closed exactly once, on every path out including an early
    /// error.
    ///
    /// Hand-written rather than `OwnedHandle` because half the handles here
    /// are handed to `CreateProcessW` and must then *not* be closed by the
    /// parent's own bookkeeping, and [`Owned::release`] is how that is said
    /// out loud.
    struct Owned(HANDLE);

    impl Owned {
        fn new(handle: HANDLE) -> io::Result<Self> {
            if handle.is_null() || handle == INVALID_HANDLE_VALUE {
                return Err(io::Error::last_os_error());
            }
            Ok(Self(handle))
        }

        fn raw(&self) -> HANDLE {
            self.0
        }

        /// Gives the handle up: the caller owns it now, and `Drop` will not
        /// close it.
        fn release(mut self) -> HANDLE {
            std::mem::replace(&mut self.0, std::ptr::null_mut())
        }
    }

    impl Drop for Owned {
        fn drop(&mut self) {
            if !self.0.is_null() {
                // SAFETY: `self.0` is a live handle this type owns, closed
                // once because `release` nulls the field.
                unsafe { CloseHandle(self.0) };
            }
        }
    }

    /// A SID allocated by a Win32 call and released with `LocalFree`.
    struct OwnedSid(PSID);

    impl Drop for OwnedSid {
        fn drop(&mut self) {
            if !self.0.is_null() {
                // SAFETY: the SID came from `ConvertStringSidToSidW` or
                // `CreateAppContainerProfile`, whose documented release is
                // `LocalFree`/`FreeSid`.
                unsafe { LocalFree(self.0) };
            }
        }
    }

    fn wide(text: &str) -> Vec<u16> {
        std::ffi::OsStr::new(text)
            .encode_wide()
            .chain(std::iter::once(0))
            .collect()
    }

    fn wide_path(path: &Path) -> Vec<u16> {
        path.as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect()
    }

    /// This process's own user SID, in `S-1-5-21-…` string form.
    ///
    /// **A failure here is a refusal, not a fallback.** The name it feeds is
    /// what stops another account deriving this container's SID
    /// ([`container_name`]); a spawn that could not read it would be a spawn
    /// into a container anyone can enter.
    pub fn current_user_sid() -> io::Result<String> {
        let mut token: HANDLE = std::ptr::null_mut();
        // SAFETY: `GetCurrentProcess` is a pseudo-handle needing no close and
        // `token` is a live out-parameter.
        let opened = unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) };
        if opened == 0 {
            return Err(io::Error::last_os_error());
        }
        let token = Owned(token);
        let mut needed: u32 = 0;
        // SAFETY: a null buffer with a zero length is the documented way to
        // ask `GetTokenInformation` for the size it needs.
        unsafe {
            GetTokenInformation(token.raw(), TokenUser, std::ptr::null_mut(), 0, &mut needed);
        }
        if needed == 0 {
            return Err(io::Error::last_os_error());
        }
        let mut buffer = vec![0u8; needed as usize];
        // SAFETY: the buffer is `needed` bytes long, which is the size the
        // call just asked for.
        let read = unsafe {
            GetTokenInformation(
                token.raw(),
                TokenUser,
                buffer.as_mut_ptr().cast::<c_void>(),
                needed,
                &mut needed,
            )
        };
        if read == 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: the buffer holds a `TOKEN_USER` whose `User.Sid` points
        // into it; both live until the end of this function.
        let sid = unsafe { (*buffer.as_ptr().cast::<TOKEN_USER>()).User.Sid };
        sid_to_string(sid)
    }

    fn sid_to_string(sid: PSID) -> io::Result<String> {
        let mut text: *mut u16 = std::ptr::null_mut();
        // SAFETY: `sid` is a valid SID for the life of the call and `text` is
        // a live out-parameter the call allocates.
        let converted = unsafe { ConvertSidToStringSidW(sid, &mut text) };
        if converted == 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: the call returned a NUL-terminated wide string.
        let length = unsafe {
            let mut length = 0usize;
            while *text.add(length) != 0 {
                length += 1;
            }
            length
        };
        // SAFETY: `text` points at `length` valid units.
        let slice = unsafe { std::slice::from_raw_parts(text, length) };
        let owned = std::ffi::OsString::from_wide(slice)
            .to_string_lossy()
            .into_owned();
        // SAFETY: `text` was allocated by `ConvertSidToStringSidW`.
        unsafe { LocalFree(text.cast()) };
        Ok(owned)
    }

    /// An AppContainer profile and its capability SID.
    ///
    /// Created with **no** capabilities. `internetClient` is the one that
    /// would give the process a network, and §4.1 is why it is absent rather
    /// than present and disabled.
    pub struct AppContainer {
        sid: PSID,
        name: String,
    }

    impl AppContainer {
        /// Creates — or reuses — the container named `name`.
        ///
        /// The profile is **persistent**: a registry entry and a directory
        /// under `%LOCALAPPDATA%\Packages` that outlive the session.
        /// `sandbox-grants.md` §3 records that as a permanent consequence of
        /// having run rather than pretending it is cleaned up, because
        /// `DeleteAppContainerProfile` is called nowhere in this crate and
        /// deleting a container a concurrent session is using would be worse
        /// than leaving it.
        pub fn create(name: &str) -> io::Result<Self> {
            let wide_name = wide(name);
            let mut sid: PSID = std::ptr::null_mut();
            // SAFETY: `wide_name` outlives the call; a null capability
            // pointer with a zero count is the documented way to ask for a
            // container with no capabilities at all.
            let created = unsafe {
                CreateAppContainerProfile(
                    wide_name.as_ptr(),
                    wide_name.as_ptr(),
                    wide_name.as_ptr(),
                    std::ptr::null(),
                    0,
                    &mut sid,
                )
            };
            if created < 0 {
                // `HRESULT_FROM_WIN32(ERROR_ALREADY_EXISTS)` is the ordinary
                // second launch, not a failure: the profile is per project
                // root and per user and outlives the session that made it.
                let already = (created as u32) == (0x8007_0000 | ERROR_ALREADY_EXISTS);
                if !already {
                    return Err(io::Error::from_raw_os_error(created));
                }
                // SAFETY: `wide_name` outlives the call and `sid` is a live
                // out-parameter.
                let derived = unsafe {
                    DeriveAppContainerSidFromAppContainerName(wide_name.as_ptr(), &mut sid)
                };
                if derived < 0 {
                    return Err(io::Error::from_raw_os_error(derived));
                }
            }
            Ok(Self {
                sid,
                name: name.to_string(),
            })
        }

        /// The capability SID, for the `SECURITY_CAPABILITIES` a
        /// `STARTUPINFOEXW` carries.
        pub fn sid(&self) -> PSID {
            self.sid
        }

        /// The profile name, which is also the directory name under
        /// `%LOCALAPPDATA%\Packages`.
        pub fn name(&self) -> &str {
            &self.name
        }
    }

    impl Drop for AppContainer {
        fn drop(&mut self) {
            if !self.sid.is_null() {
                // SAFETY: the SID was allocated by the container APIs, which
                // document `FreeSid`'s `LocalFree` as its release.
                unsafe { LocalFree(self.sid) };
            }
        }
    }

    /// What this host can enforce for `profile`, **creating nothing**.
    ///
    /// `DeriveAppContainerSidFromAppContainerName` computes the SID for a name
    /// and creates no profile, which answers the availability question
    /// without writing to the developer's machine: a function whose name
    /// promises a reading must not perform a write. It is the weaker probe,
    /// and that is stated rather than glossed — it establishes that this host
    /// has the AppContainer API and that the name derives, not that a profile
    /// can be created. Creation belongs to [`spawn`], which needs the
    /// container.
    pub fn regime(profile: &Profile) -> Regime {
        let Ok(user) = current_user_sid() else {
            return Regime::Unconfined;
        };
        let name = wide(&container_name(profile, &user));
        let mut sid: PSID = std::ptr::null_mut();
        // SAFETY: `name` outlives the call and `sid` is a live out-parameter;
        // the call allocates a SID and nothing else.
        let derived = unsafe { DeriveAppContainerSidFromAppContainerName(name.as_ptr(), &mut sid) };
        if derived < 0 {
            return Regime::Unconfined;
        }
        drop(OwnedSid(sid));
        Regime::AppContainer
    }

    /// Whether the service that enforces an AppContainer's missing
    /// `internetClient` is running.
    ///
    /// `MpsSvc` is the Windows Firewall service. It hosts the Windows
    /// Filtering Platform filters that refuse a capability-less container's
    /// sockets; the object-manager access check that enforces every file
    /// grant here has no opinion about a socket at all.
    pub fn network_isolation() -> super::NetworkIsolation {
        use super::NetworkIsolation;
        // SAFETY: both name pointers are null, which asks for the local
        // machine's active database.
        let manager =
            unsafe { OpenSCManagerW(std::ptr::null(), std::ptr::null(), SC_MANAGER_CONNECT) };
        if manager.is_null() {
            return NetworkIsolation::Unknown;
        }
        let name = wide("MpsSvc");
        // SAFETY: `manager` is open and `name` outlives the call.
        let service = unsafe { OpenServiceW(manager, name.as_ptr(), SERVICE_QUERY_STATUS) };
        if service.is_null() {
            // SAFETY: `manager` was opened above and is closed once.
            unsafe { CloseServiceHandle(manager) };
            return NetworkIsolation::Unknown;
        }
        let mut status: SERVICE_STATUS = unsafe { std::mem::zeroed() };
        // SAFETY: `service` is open and `status` is a live out-parameter.
        let queried = unsafe { QueryServiceStatus(service, &mut status) };
        // SAFETY: both handles were opened above and are closed once each.
        unsafe {
            CloseServiceHandle(service);
            CloseServiceHandle(manager);
        }
        if queried == 0 {
            return NetworkIsolation::Unknown;
        }
        if status.dwCurrentState == SERVICE_RUNNING {
            NetworkIsolation::EnforcedByFirewall
        } else {
            NetworkIsolation::NotEnforced
        }
    }

    /// The allow and deny masks `sid` holds on `path`, from its effective
    /// DACL.
    ///
    /// Inherited ACEs included, because they are what an access check sees.
    /// A NULL DACL means "everyone, everything", which is reported as an
    /// all-bits allow rather than as an absence.
    fn masks_for(path: &Path, sid: PSID) -> io::Result<(u32, u32)> {
        let path = wide_path(path);
        let mut dacl: *mut ACL = std::ptr::null_mut();
        let mut descriptor: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
        // SAFETY: `path` outlives the call; the out-pointers are live and the
        // descriptor owns the ACL it hands back.
        let read = unsafe {
            GetNamedSecurityInfoW(
                path.as_ptr(),
                SE_FILE_OBJECT,
                DACL_SECURITY_INFORMATION,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                &mut dacl,
                std::ptr::null_mut(),
                &mut descriptor,
            )
        };
        if read != 0 {
            return Err(io::Error::from_raw_os_error(read as i32));
        }
        let masks = if dacl.is_null() {
            (u32::MAX, 0)
        } else {
            summed(&walk_dacl(dacl, sid))
        };
        // SAFETY: `descriptor` was allocated by the call above.
        unsafe { LocalFree(descriptor) };
        Ok(masks)
    }

    /// The allow and deny masks a list of ACEs adds up to, **as the access
    /// check would read them**: the first ACE that decides a bit decides it,
    /// so a right already denied is not later allowed and vice versa.
    fn summed(aces: &[Ace]) -> (u32, u32) {
        let mut allow = 0u32;
        let mut deny = 0u32;
        for ace in aces {
            let undecided = !(allow | deny);
            if ace.allow {
                allow |= ace.mask & undecided;
            } else {
                deny |= ace.mask & undecided;
            }
        }
        (allow, deny)
    }

    /// One ACE naming the SID that was asked about, in the order the access
    /// check will read them.
    ///
    /// Order is the whole reason this is a list and not two masks: an
    /// access check stops at the first ACE that decides, so a DENY *after*
    /// an ALLOW carrying the same right decides nothing. A pair of summed
    /// masks cannot tell those two ACLs apart, and the difference between
    /// them is whether a program can write into `.claude`.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct Ace {
        pub allow: bool,
        pub inherited: bool,
        pub mask: u32,
    }

    /// The ACEs naming this profile's container on `path`, in ACL order.
    ///
    /// Derives the SID from the name and creates nothing, so asking does not
    /// write to the machine.
    pub fn container_aces(profile: &Profile, path: &Path) -> io::Result<Vec<Ace>> {
        let user = current_user_sid()?;
        let name = wide(&container_name(profile, &user));
        let mut sid: PSID = std::ptr::null_mut();
        // SAFETY: `name` outlives the call and `sid` is a live out-parameter.
        let derived = unsafe { DeriveAppContainerSidFromAppContainerName(name.as_ptr(), &mut sid) };
        if derived < 0 {
            return Err(io::Error::from_raw_os_error(derived));
        }
        let sid = OwnedSid(sid);
        aces_for(path, sid.0)
    }

    fn aces_for(path: &Path, sid: PSID) -> io::Result<Vec<Ace>> {
        let path = wide_path(path);
        let mut dacl: *mut ACL = std::ptr::null_mut();
        let mut descriptor: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
        // SAFETY: `path` outlives the call; the out-pointers are live and the
        // descriptor owns the ACL it hands back.
        let read = unsafe {
            GetNamedSecurityInfoW(
                path.as_ptr(),
                SE_FILE_OBJECT,
                DACL_SECURITY_INFORMATION,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                &mut dacl,
                std::ptr::null_mut(),
                &mut descriptor,
            )
        };
        if read != 0 {
            return Err(io::Error::from_raw_os_error(read as i32));
        }
        let aces = if dacl.is_null() {
            // A NULL DACL is "everyone, everything" -- reported as one
            // inherited-looking allow rather than as an absence, so a caller
            // cannot read it as "no grant".
            vec![Ace {
                allow: true,
                inherited: false,
                mask: u32::MAX,
            }]
        } else {
            walk_dacl(dacl, sid)
        };
        // SAFETY: `descriptor` was allocated by the call above.
        unsafe { LocalFree(descriptor) };
        Ok(aces)
    }

    /// The ACEs a DACL holds for one SID, in order.
    ///
    /// `INHERIT_ONLY_ACE` entries are skipped: they apply to children of this
    /// object and grant nothing on the object itself, so counting one would
    /// report a grant that does not exist.
    fn walk_dacl(dacl: *const ACL, sid: PSID) -> Vec<Ace> {
        let mut information: ACL_SIZE_INFORMATION = unsafe { std::mem::zeroed() };
        // SAFETY: `dacl` is a valid ACL and the buffer's size is its own.
        let read = unsafe {
            GetAclInformation(
                dacl,
                (&raw mut information).cast::<c_void>(),
                std::mem::size_of::<ACL_SIZE_INFORMATION>() as u32,
                AclSizeInformation,
            )
        };
        if read == 0 {
            return Vec::new();
        }
        let mut aces = Vec::new();
        for index in 0..information.AceCount {
            let mut ace: *mut c_void = std::ptr::null_mut();
            // SAFETY: `index` is below the count the call above reported.
            if unsafe { GetAce(dacl, index, &mut ace) } == 0 {
                continue;
            }
            // SAFETY: `ace` points at an ACE whose first field is its header.
            let header = unsafe { *ace.cast::<ACE_HEADER>() };
            if u32::from(header.AceFlags) & INHERIT_ONLY_ACE != 0 {
                continue;
            }
            if header.AceType != ALLOWED_ACE && header.AceType != DENIED_ACE {
                continue;
            }
            // An allow and a deny ACE share a layout: the header, a `u32`
            // mask, and the SID beginning at the next `u32`.
            // SAFETY: `ace` points at an ACE of at least that size, which its
            // own type guarantees.
            let (mask, ace_sid) = unsafe {
                let base = ace.cast::<u8>();
                let mask = *base.add(std::mem::size_of::<ACE_HEADER>()).cast::<u32>();
                let ace_sid = base
                    .add(std::mem::size_of::<ACE_HEADER>() + std::mem::size_of::<u32>())
                    .cast::<c_void>();
                (mask, ace_sid)
            };
            // SAFETY: both are valid SIDs for the life of the call.
            if unsafe { EqualSid(ace_sid, sid) } == 0 {
                continue;
            }
            aces.push(Ace {
                allow: header.AceType == ALLOWED_ACE,
                inherited: u32::from(header.AceFlags) & INHERITED_ACE != 0,
                mask,
            });
        }
        aces
    }

    /// The allow and deny masks this profile's container holds on `path`.
    ///
    /// For a report, and for the test that has to say **which ACEs were
    /// applied where** rather than only that a child was refused. It derives
    /// the SID from the name and creates nothing, so asking the question
    /// does not write to the machine.
    pub fn container_masks(profile: &Profile, path: &Path) -> io::Result<(u32, u32)> {
        let user = current_user_sid()?;
        let name = wide(&container_name(profile, &user));
        let mut sid: PSID = std::ptr::null_mut();
        // SAFETY: `name` outlives the call and `sid` is a live out-parameter.
        let derived = unsafe { DeriveAppContainerSidFromAppContainerName(name.as_ptr(), &mut sid) };
        if derived < 0 {
            return Err(io::Error::from_raw_os_error(derived));
        }
        let sid = OwnedSid(sid);
        masks_for(path, sid.0)
    }

    /// Whether an AppContainer could load `image` at all.
    ///
    /// **The ruling this answers**: sterna does not write ACEs onto files
    /// outside the project, so a binary whose own ACL does not admit
    /// application packages cannot be made runnable — it is refused by name
    /// instead. A tool installed by a package manager that strips the
    /// inherited `ALL APPLICATION PACKAGES` ACE is exactly that case, and the
    /// diagnostic in `tests/sandbox_windows_cage.rs` names which binaries on
    /// a given machine those are.
    ///
    /// A deny ACE for the same group wins, which is why both masks are read.
    pub fn image_admits_app_containers(image: &Path) -> io::Result<bool> {
        for text in APPLICATION_PACKAGE_SIDS {
            let text = wide(text);
            let mut sid: PSID = std::ptr::null_mut();
            // SAFETY: `text` outlives the call and `sid` is a live
            // out-parameter the call allocates.
            if unsafe { ConvertStringSidToSidW(text.as_ptr(), &mut sid) } == 0 {
                return Err(io::Error::last_os_error());
            }
            let sid = OwnedSid(sid);
            let (allow, deny) = masks_for(image, sid.0)?;
            if allow & EXECUTE_BITS != 0 && deny & EXECUTE_BITS == 0 {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// Extends the project directory's ACL to admit `container`'s SID, and
    /// admits nothing else.
    ///
    /// The grants come from [`acl_grants`], so the profile decides which
    /// directories appear and whether the writable one is writable.
    ///
    /// **Extends rather than replaces.** A null `OldAcl` handed to
    /// `SetEntriesInAclW` does not merge — it builds an ACL from the supplied
    /// entries alone — so the existing DACL is read with
    /// [`GetNamedSecurityInfoW`] and handed back as `OldAcl`. Nothing here
    /// ever removes an ACE belonging to another principal: the developer,
    /// `SYSTEM` and `Administrators` keep exactly what they had.
    ///
    /// **The `.claude` and `.sterna` carve-outs are absences of grant, not DENYs, and that
    /// is a measurement rather than a preference.** Measured on the Windows
    /// ARM64 VM, 2026-09-09: with `.claude` carrying, in ACL order, an
    /// explicit DENY of the write bits for the container SID, then an
    /// explicit ALLOW of the read bits, then the root's inherited ALLOW of
    /// the read-write bits — `icacls` confirming that order — a confined
    /// child ran `mkdir` inside `.claude` and **succeeded**, while the same
    /// child's `mkdir` outside the project was refused with
    /// *"Access is denied."* An AppContainer's access check is a **grant**
    /// check: the package SID has to be granted the access, and a DENY naming
    /// it decides nothing. So the only carve-out that works is one where the
    /// container is never granted the right in the first place, and that
    /// means blocking the root's inheritable grant from reaching `.claude`.
    ///
    /// **What that costs, stated rather than left implicit.** A carve-out
    /// nested inside a granted root is written with
    /// `PROTECTED_DACL_SECURITY_INFORMATION`, which stops inheritance at that
    /// directory. Every inherited ACE is copied into it first — that is what
    /// passing the effective DACL as `OldAcl` does — so no principal loses
    /// access, but `.claude`'s permissions stop tracking the project root's
    /// from then on. `sandbox-grants.md` §3 records it as a permanent
    /// consequence of having run, because it is one.
    ///
    /// **Idempotent, and that is load-bearing rather than tidy.** This runs
    /// on every spawn, and re-writing an ACE that is already there would both
    /// grow the DACL of a person's project directory and re-walk their whole
    /// tree — measured at 1.01s for 10,000 files against a 47ms skip. So
    /// every granted path's current masks are read first and the call returns
    /// having written nothing when all of them already carry exactly the
    /// intended grant.
    ///
    /// **That skip is one decision for the whole call, not one per path, and
    /// the carve-out's own grant is what makes it safe.** See the phases in
    /// the body: an interrupted propagation leaves the root looking done, and
    /// the carve-out looking unfinished is the only evidence that it is not.
    pub fn grant_project_acl(
        profile: &Profile,
        binary: &Path,
        container: &AppContainer,
    ) -> io::Result<()> {
        let grants: AclGrants = acl_grants(profile, binary);
        let mut targets: Vec<(&Path, u32, bool)> = Vec::new();
        for (paths, rights) in [
            (&grants.read_write, READ_WRITE_RIGHTS),
            (&grants.read_only, READ_RIGHTS),
        ] {
            for path in paths {
                // A carve-out is one that sits inside a root this same call
                // grants more broadly. It is the only case that needs the
                // inheritance stopped; a whole project granted read-only
                // inherits nothing of Sterna's making and is left merging.
                let nested = grants
                    .read_write
                    .iter()
                    .any(|granted| path.starts_with(granted) && path != granted);
                targets.push((path.as_path(), rights, nested));
            }
        }
        for (path, _, nested) in &targets {
            // **A carve-out has to exist to be carved out**, and on a
            // fresh project either protected directory may not. Skipping a missing one
            // -- which is what the Linux applier does with a system root
            // a distribution lacks -- would be a fail-open here and not a
            // harmless absence: the project root is writable, so a
            // program could create the protected directory itself and write the
            // settings document its *next* session is compiled from,
            // which is invariant 5 defeated by a `mkdir`. So the
            // directory is created, and a failure to create it is a
            // refusal rather than a spawn without the carve-out.
            //
            // `sandbox-grants.md` §3 records protected directories left in a
            // project that had none as a consequence of having run.
            if *nested && !path.exists() {
                std::fs::create_dir_all(path)?;
            }
        }

        // **Exactly these rights and no others, on every path** -- not "at
        // least these". A superset is the defect this check closes twice
        // over: on a carve-out the superset *is* the write reach being
        // carved out, and on the project root a superset is what a machine
        // that ran an earlier build carries, because that build's mask held
        // `WRITE_DAC`, `WRITE_OWNER` and `FILE_DELETE_CHILD`. A `>=` test
        // would leave every one of them in place for ever.
        //
        // It is one decision for the whole call rather than one per path,
        // and that is the completion witness below.
        let mut applied = true;
        for (path, rights, _) in &targets {
            let (allow, deny) = masks_for(path, container.sid())?;
            if allow != *rights || deny != 0 {
                applied = false;
            }
        }
        if applied {
            return Ok(());
        }

        // **The carve-out's own grant is the completion witness, and the
        // three phases below are what make it one.**
        //
        // `SetNamedSecurityInfoW` writes the named object's DACL and *then*
        // walks the tree propagating it, so a call interrupted part way
        // leaves the root carrying the final mask over a tree that does not.
        // Reading the root's mask therefore cannot tell a finished
        // propagation from an abandoned one, and the skip above would make
        // the abandoned one permanent -- half the project unreachable to the
        // container until somebody deleted the ACE by hand.
        //
        // So the carve-out is written twice: closed before the root
        // propagates, and granted its reads only after every root has
        // returned. The `.claude` grant existing at all is then proof that
        // the propagation it follows ran to the end. Measured on the Windows
        // ARM64 VM, 2026-09-09: propagating over a 10,000-file project cost
        // 1.01s and the skip cost 47ms, so paying a second full propagation
        // for the witness was not an option -- but `.claude` is a directory
        // with a settings document in it, and writing it twice costs
        // nothing.
        //
        // **Phase 1 also is escape 2's fix**, unchanged in substance.
        // Windows inheritance is a copy performed when the ACE is written,
        // so granting the root first *put* the read-write ACE inside
        // `.claude`, and the carve-out then had to remove it -- which
        // `SetEntriesInAclW`'s `REVOKE_ACCESS` cannot do, because it does not
        // modify an inherited ACE. Measured on the same host: a confined
        // child's `mkdir .claude\evil` returned exit 0 and the directory
        // landed. A protected DACL is not propagated into, so writing the
        // carve-out first means the root's grant never reaches it;
        // [`without_sid`] is the second half, removing an ACE a previous
        // build already propagated.
        for (path, _, nested) in &targets {
            if *nested {
                write_acl(path, container, None, true)?;
            }
        }
        for (path, rights, nested) in &targets {
            if !*nested {
                write_acl(path, container, Some(*rights), false)?;
            }
        }
        for (path, rights, nested) in &targets {
            if *nested {
                write_acl(path, container, Some(*rights), true)?;
            }
        }
        Ok(())
    }

    /// Writes one path's DACL: the container's ACEs replaced by exactly
    /// `rights`, every other principal's left alone.
    ///
    /// `rights` of `None` means the container appears in the DACL nowhere at
    /// all, which is the closed half of the completion witness in
    /// [`grant_project_acl`] and is only ever asked of a carve-out.
    ///
    /// `nested` is what makes it a carve-out: **every** ACE naming the
    /// container is stripped from the DACL by hand, and inheritance is
    /// stopped, so the root's broader grant can neither survive in it nor
    /// reach back into it.
    ///
    /// **Why by hand.** The obvious spelling — hand `SetEntriesInAclW` a
    /// `REVOKE_ACCESS` entry for the container and let it drop the ACEs — was
    /// what shipped, and it does not work: that function merges *explicit*
    /// entries and copies an inherited ACE through unchanged. The root's
    /// grant arrives in `.claude` as an inherited ACE, so the revoke passed
    /// over exactly the ACE it existed to remove, the protected DACL then
    /// froze it in place, and a confined child's `mkdir .claude\evil`
    /// returned exit 0 on the VM. [`without_sid`] removes an ACE by not
    /// copying it, which has no such exception.
    fn write_acl(
        path: &Path,
        container: &AppContainer,
        rights: Option<u32>,
        nested: bool,
    ) -> io::Result<()> {
        debug_assert!(
            rights.is_some() || nested,
            "only a carve-out is ever written without the container in its DACL"
        );
        let mut path_units = wide_path(path);
        let mut old_dacl: *mut ACL = std::ptr::null_mut();
        let mut descriptor: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
        // SAFETY: `path_units` outlives the call; the two out-pointers are
        // live, and the descriptor owns the ACL it hands back.
        let read = unsafe {
            GetNamedSecurityInfoW(
                path_units.as_ptr(),
                SE_FILE_OBJECT,
                DACL_SECURITY_INFORMATION,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                &mut old_dacl,
                std::ptr::null_mut(),
                &mut descriptor,
            )
        };
        if read != 0 {
            return Err(io::Error::from_raw_os_error(read as i32));
        }
        let descriptor = OwnedLocal(descriptor);

        // The carve-out starts from a DACL the container appears nowhere in,
        // so the only ACE naming it afterwards is the one added below.
        let stripped = if nested {
            Some(without_sid(old_dacl, container.sid())?)
        } else {
            None
        };
        let base = stripped
            .as_ref()
            .map_or(old_dacl, |acl| acl.as_ptr().cast_mut());
        // `SET_ACCESS`, not `GRANT_ACCESS`: a grant is OR'd into the
        // trustee's existing explicit ACE, so a project last caged by a build
        // whose mask still held `WRITE_DAC` would keep it for ever. A set
        // replaces the trustee's ACEs with this one.
        let merged = match rights {
            Some(rights) => Some(merge(&[entry(container, rights, SET_ACCESS)], base)?),
            None => None,
        };
        let acl: *const c_void = match &merged {
            Some(acl) => acl.0.cast(),
            None => base.cast(),
        };

        let information = if nested {
            DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION
        } else {
            DACL_SECURITY_INFORMATION
        };
        // SAFETY: `path_units` and the ACL outlive the call. Unprotected for
        // an ordinary grant, so the inherited ACEs that give the developer
        // their own project stay where they were; protected for a carve-out,
        // where the ACEs just copied out of `old_dacl` are what keeps every
        // other principal's access unchanged.
        let written = unsafe {
            SetNamedSecurityInfoW(
                path_units.as_mut_ptr(),
                SE_FILE_OBJECT,
                information,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                acl.cast_mut().cast(),
                std::ptr::null_mut(),
            )
        };
        drop((merged, stripped, descriptor));
        if written != 0 {
            return Err(io::Error::from_raw_os_error(written as i32));
        }
        Ok(())
    }

    /// An ACL this crate allocated, aligned the way an `ACL` must be.
    ///
    /// A `Vec<u8>` is not: `ACL` and every ACE in it begin on a `DWORD`
    /// boundary, and the global allocator promises a `u8` buffer nothing
    /// stronger than byte alignment. The `Vec<u32>` is the alignment, said in
    /// the type rather than assumed.
    struct OwnedAcl(Vec<u32>);

    impl OwnedAcl {
        fn as_ptr(&self) -> *const ACL {
            self.0.as_ptr().cast()
        }

        fn as_mut_ptr(&mut self) -> *mut ACL {
            self.0.as_mut_ptr().cast()
        }
    }

    /// `old` with every ACE naming `sid` left out, and every surviving ACE
    /// made explicit.
    ///
    /// **Removing by not copying is the whole point.** `SetEntriesInAclW`'s
    /// `REVOKE_ACCESS` merges against the *explicit* entries of an ACL and
    /// copies an inherited ACE through untouched, which is how the project
    /// root's inheritable read-write grant survived inside the `.claude`
    /// carve-out and let a confined child write there. A copy that skips the
    /// SID has no such exception, and no dependence on an undocumented
    /// ordering between a revoke and a grant.
    ///
    /// **Everyone else keeps exactly what they had, and gets it explicitly.**
    /// The caller protects this DACL from inheritance, so an ACE left
    /// carrying `INHERITED_ACE` would claim a parentage it no longer has —
    /// `icacls` would print it as `(I)` on an object nothing propagates into,
    /// and a later recalculation could drop it. The flag is cleared as each
    /// ACE is copied; the *inheritance* flags are not, so files created inside
    /// the carve-out still inherit the developer's, `SYSTEM`'s and
    /// `Administrators`' access exactly as before.
    fn without_sid(old: *const ACL, sid: PSID) -> io::Result<OwnedAcl> {
        /// `AceFlags` is the second byte of every `ACE_HEADER`.
        const FLAGS_OFFSET: usize = 1;
        /// `AddAce`'s "append at the end" index.
        const AT_THE_END: u32 = u32::MAX;
        /// `INHERITED_ACE`, in the width `AceFlags` actually is.
        const INHERITED: u8 = 0x10;
        const _: () = assert!(INHERITED as u32 == INHERITED_ACE);

        let mut information: ACL_SIZE_INFORMATION = unsafe { std::mem::zeroed() };
        // An ACL this function only ever removes from cannot need more room
        // than the one it copies, and a null `old` means "everyone,
        // everything" -- which becomes an empty ACL here, denying all,
        // because a carve-out inheriting a NULL DACL is the one case where
        // erring wide would be erring open.
        let capacity = if old.is_null() {
            std::mem::size_of::<ACL>() as u32
        } else {
            // SAFETY: `old` is a valid ACL and the buffer's size is its own.
            let read = unsafe {
                GetAclInformation(
                    old,
                    (&raw mut information).cast::<c_void>(),
                    std::mem::size_of::<ACL_SIZE_INFORMATION>() as u32,
                    AclSizeInformation,
                )
            };
            if read == 0 {
                return Err(io::Error::last_os_error());
            }
            information.AclBytesInUse + information.AclBytesFree
        }
        .max(std::mem::size_of::<ACL>() as u32);

        let mut acl = OwnedAcl(vec![0u32; capacity.div_ceil(4) as usize]);
        // SAFETY: the buffer is `capacity` bytes, `DWORD`-aligned by its
        // element type.
        if unsafe { InitializeAcl(acl.as_mut_ptr(), capacity, ACL_REVISION) } == 0 {
            return Err(io::Error::last_os_error());
        }
        if old.is_null() {
            return Ok(acl);
        }

        for index in 0..information.AceCount {
            let mut ace: *mut c_void = std::ptr::null_mut();
            // SAFETY: `index` is below the count the call above reported.
            if unsafe { GetAce(old, index, &mut ace) } == 0 {
                return Err(io::Error::last_os_error());
            }
            // SAFETY: `ace` points at an ACE whose first field is its header.
            let header = unsafe { *ace.cast::<ACE_HEADER>() };
            if names_sid(ace, header.AceType, sid) {
                continue;
            }
            // SAFETY: the header's own `AceSize` is the length of the ACE.
            let bytes =
                unsafe { std::slice::from_raw_parts(ace.cast::<u8>(), header.AceSize as usize) };
            let mut copy = bytes.to_vec();
            copy[FLAGS_OFFSET] &= !INHERITED;
            // SAFETY: `acl` is an initialised ACL with room for what it was
            // copied from, and `copy` is one whole ACE of its own stated
            // length.
            let added = unsafe {
                AddAce(
                    acl.as_mut_ptr(),
                    ACL_REVISION,
                    AT_THE_END,
                    copy.as_ptr().cast::<c_void>(),
                    copy.len() as u32,
                )
            };
            if added == 0 {
                return Err(io::Error::last_os_error());
            }
        }
        Ok(acl)
    }

    /// Whether an allow or deny ACE names `sid`.
    ///
    /// Any other ACE type is copied through: an audit or an object ACE grants
    /// no access, and dropping one would change a machine's auditing while
    /// claiming to change a sandbox.
    fn names_sid(ace: *const c_void, kind: u8, sid: PSID) -> bool {
        if kind != ALLOWED_ACE && kind != DENIED_ACE {
            return false;
        }
        // An allow and a deny ACE share a layout: the header, a `u32` mask,
        // and the SID beginning at the next `u32`.
        // SAFETY: `ace` points at an ACE of at least that size, which its own
        // type guarantees.
        let ace_sid = unsafe {
            ace.cast::<u8>()
                .add(std::mem::size_of::<ACE_HEADER>() + std::mem::size_of::<u32>())
                .cast::<c_void>()
        };
        // SAFETY: both are valid SIDs for the life of the call.
        unsafe { EqualSid(ace_sid.cast_mut(), sid) != 0 }
    }

    /// `SetEntriesInAclW`, with the allocation it returns owned.
    fn merge(entries: &[EXPLICIT_ACCESS_W], old: *mut ACL) -> io::Result<OwnedLocal> {
        let mut acl: *mut ACL = std::ptr::null_mut();
        // SAFETY: the entry array's address and count agree, and `old` is a
        // DACL read from the object — passing it is what makes this a merge
        // instead of a replacement.
        let built =
            unsafe { SetEntriesInAclW(entries.len() as u32, entries.as_ptr(), old, &mut acl) };
        if built != 0 {
            return Err(io::Error::from_raw_os_error(built as i32));
        }
        Ok(OwnedLocal(acl.cast()))
    }

    /// Anything a security API allocated and `LocalFree` releases.
    struct OwnedLocal(PSECURITY_DESCRIPTOR);

    impl Drop for OwnedLocal {
        fn drop(&mut self) {
            if !self.0.is_null() {
                // SAFETY: allocated by `GetNamedSecurityInfoW` or
                // `SetEntriesInAclW`, and freed once.
                unsafe { LocalFree(self.0) };
            }
        }
    }

    /// One `EXPLICIT_ACCESS_W` for `container`'s SID, inheritable down the
    /// tree.
    fn entry(container: &AppContainer, rights: u32, mode: i32) -> EXPLICIT_ACCESS_W {
        EXPLICIT_ACCESS_W {
            grfAccessPermissions: rights,
            grfAccessMode: mode,
            grfInheritance: SUB_CONTAINERS_AND_OBJECTS_INHERIT,
            Trustee: TRUSTEE_W {
                pMultipleTrustee: std::ptr::null_mut(),
                MultipleTrusteeOperation: NO_MULTIPLE_TRUSTEE,
                TrusteeForm: TRUSTEE_IS_SID,
                TrusteeType: TRUSTEE_IS_WELL_KNOWN_GROUP,
                ptstrName: container.sid().cast(),
            },
        }
    }

    /// A child that exists only because a container was entered first.
    ///
    /// It is not a `std::process::Child` and cannot be made into one: nothing
    /// in stable `std` builds a `Child` from a handle, which is the same
    /// reason [`spawn`] exists. The surface below is the subset
    /// `tools::invoke` actually uses, and no more.
    pub struct ContainedChild {
        process: HANDLE,
        job: HANDLE,
        pid: u32,
        /// Which container it entered, for the report.
        pub container: String,
        pub stdin: Option<File>,
        pub stdout: Option<File>,
        pub stderr: Option<File>,
    }

    // SAFETY: the process and job handles are kernel objects this value owns
    // alone and closes in `Drop`. A kernel handle is valid on every thread of
    // the process and nothing here is shared, so moving the value to another
    // thread -- a foreground command handed over to a job (`bg::adopt`) -- is
    // sound. It stays `!Sync`.
    unsafe impl Send for ContainedChild {}

    impl ContainedChild {
        pub fn id(&self) -> u32 {
            self.pid
        }

        pub fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
            // SAFETY: `self.process` is the handle `CreateProcessW` returned
            // and is live until `Drop`.
            let waited = unsafe { WaitForSingleObject(self.process, 0) };
            if waited != WAIT_OBJECT_0 {
                return Ok(None);
            }
            self.status().map(Some)
        }

        pub fn wait(&mut self) -> io::Result<ExitStatus> {
            // SAFETY: as above; `INFINITE` is `u32::MAX`.
            unsafe { WaitForSingleObject(self.process, u32::MAX) };
            self.status()
        }

        fn status(&self) -> io::Result<ExitStatus> {
            let mut code: u32 = 0;
            // SAFETY: `self.process` is live and `code` is a live
            // out-parameter.
            if unsafe { GetExitCodeProcess(self.process, &mut code) } == 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(ExitStatus::from_raw(code))
        }

        /// Kills the child **and everything it started**.
        ///
        /// This is the job object's whole purpose and this platform's
        /// `killpg`: a `bash` call that started a server and was cancelled
        /// must not leave the server running. Terminating the job reaches
        /// every process assigned to it, which is exactly what this call
        /// created.
        pub fn kill(&mut self) -> io::Result<()> {
            // SAFETY: `self.job` is the job this spawn created and owns.
            if unsafe { TerminateJobObject(self.job, 1) } == 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        }
    }

    /// **Dropping one kills it, and that is a deliberate difference from
    /// unix.** The job carries `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`, so
    /// closing the last handle to it takes the child and everything it
    /// started with it; a `std::process::Child` dropped on unix leaves the
    /// process running. The Windows behaviour is the one this crate wants —
    /// `spawn_confined`'s contract is that a call leaves nothing behind — and
    /// it is the same flag, for the same reason, that
    /// `crates/glasshouse/src/pty/process.rs` sets on its own job.
    impl Drop for ContainedChild {
        fn drop(&mut self) {
            // SAFETY: both handles are this type's own, closed once each.
            unsafe {
                CloseHandle(self.process);
                CloseHandle(self.job);
            }
        }
    }

    /// A refusal that names a program spelled relative to *Sterna's* current
    /// directory.
    ///
    /// **Escape 1, in one sentence:** `CreateProcessW` completes a partial
    /// `lpApplicationName` from the calling process's current drive and
    /// directory, and Sterna's current directory during a session is the
    /// project root — which invariant 3 makes writable. So a program that
    /// wrote `<project>\grep` and then called a tool whose program did not
    /// resolve on `PATH` had sterna execute the file it had just written.
    ///
    /// The fallback that produced that spelling cannot be salvaged on this
    /// platform, and that is a property of the API rather than a policy:
    /// `spawn` passes `lpApplicationName`, so `CreateProcessW` performs no
    /// search of its own. There is no "let the loader look it up, bounded by
    /// the applier's executable roots" here — the AppContainer has no
    /// executable roots (§3) — so an unresolved name has exactly one meaning
    /// and it is the wrong one.
    fn relative_program(image: &Path) -> io::Error {
        io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!(
                "`{}` is not an absolute path, and CreateProcessW would complete it from Sterna's \
                 own current directory -- which is the project root, and writable. A tool whose \
                 program does not resolve on PATH is refused rather than resolved against a \
                 directory the program can write (docs/sandbox.md, per platform)",
                image.display()
            ),
        )
    }

    /// A refusal that names a program the running profile would let the
    /// program itself write.
    ///
    /// The general shape of escape 1, rather than the one spelling that was
    /// measured: whether the model can *reach* a binary is a question the
    /// profile already answers, so it is asked instead of re-derived. A
    /// binary anywhere the profile grants `Write` — the project root by
    /// invariant 3, and any other writable path a settings document names —
    /// is a binary the model could have authored, and map line 2457 says
    /// nothing model-authored runs.
    ///
    /// It is deliberately stricter than the escape required, and the cost is
    /// stated rather than discovered: a project-local MCP server executable
    /// is refused on Windows. That is the direction §4 asks a doubt to fall.
    fn writable_program(image: &Path) -> io::Error {
        io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!(
                "`{}` sits where this profile grants write, so a program could have authored it; \
                 sterna does not execute a binary its own sandbox would let the model write (docs/sandbox.md, per platform)",
                image.display()
            ),
        )
    }

    /// A refusal that names the binary the container could not load.
    fn cannot_load(image: &Path) -> io::Error {
        io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!(
                "`{}` carries no execute ACE for ALL APPLICATION PACKAGES, so an AppContainer \
                 cannot load it. sterna does not write ACEs onto files outside the project to make \
                 one runnable (docs/sandbox.md, per platform), so this tool is refused rather than spawned \
                 with less confinement",
                image.display()
            ),
        )
    }

    /// Confines and spawns, in one call, because on this platform they are
    /// one act.
    ///
    /// The order is the contract: the user's SID, then the container name it
    /// derives, then the container, then the refusal for an image the
    /// container cannot load, then the project ACL, and only then
    /// `CreateProcessW`. Every one of those is a `?`, so there is no
    /// expression below that starts a child without all of them having
    /// succeeded.
    ///
    /// `command` is read for its program, arguments, environment changes and
    /// current directory — the stdio it may carry is ignored, because the
    /// handles are created here and `pipes` says which. `shape` says how the
    /// arguments become the one string `CreateProcessW` takes: quoted for a
    /// program that reads it back by `CommandLineToArgvW`'s rules, or
    /// [`shell_command_line`]'s way for `cmd.exe`, which reads its own.
    pub fn spawn(
        profile: &Profile,
        binary: &Path,
        command: &Command,
        pipes: Pipes,
        shape: LineShape,
    ) -> Result<ContainedChild, SpawnError> {
        // Everything down to `CreateProcessW` is the confinement, so every
        // failure above that line is `NotConfinable` and leaves no process
        // anywhere.
        use SpawnError::{NotConfinable, NotStarted};
        // **The program is decided before anything else, and both questions
        // are the sandbox's own** -- so both are `NotConfinable`, which is
        // what reaches a program as a catchable `PermissionDenied` (§1.4).
        //
        // Measured on the Windows ARM64 VM, 2026-09-09: with only the
        // `is_file()` check below, a bare relative name reached
        // `CreateProcessW`, which completed it from Sterna's own current
        // directory -- the project root -- and started a file the model had
        // written there. Both checks are above every Win32 call in this
        // function, so a refused program creates no container, writes no ACE
        // and touches nothing.
        if !binary.is_absolute() {
            return Err(NotConfinable(relative_program(binary)));
        }
        if profile.check("exec", Access::Write, binary).is_ok() {
            return Err(NotConfinable(writable_program(binary)));
        }
        // A binary that is absolute, unwritable and simply not there is the
        // operating system's answer and not a permission decision, which is
        // why it is asked after the two above and reported as a different
        // kind. Without this the ACL read below fails with
        // `ERROR_FILE_NOT_FOUND` and the whole spawn is reported as *"the
        // AppContainer could not be entered"* — measured on the Windows ARM64
        // VM, 2026-09-09, where a machine with no `cat.exe` on `PATH` told a
        // program its sandbox had failed when its only problem was a missing
        // tool. `CreateProcessW` is given `lpApplicationName`, so it performs
        // no search of its own: an unresolved name could never start.
        if !binary.is_file() {
            return Err(NotStarted(not_an_executable(binary)));
        }
        let user = current_user_sid().map_err(NotConfinable)?;
        let container =
            AppContainer::create(&container_name(profile, &user)).map_err(NotConfinable)?;
        if !image_admits_app_containers(binary).map_err(NotConfinable)? {
            return Err(NotConfinable(cannot_load(binary)));
        }
        grant_project_acl(profile, binary, &container).map_err(NotConfinable)?;
        start(profile, binary, command, pipes, shape, Some(&container))
    }

    /// The explicit host-selected bypass: the same process, pipes, job and
    /// command line as [`spawn`], entered into **no** container.
    ///
    /// **The invariant: this is reachable from exactly one place**,
    /// `tools::invoke::spawn_bypassed`'s Windows arm behind
    /// `Profile::os_sandbox_bypassed()`, which `--dangerously-bypass-os-sandbox
    /// --yolo` sets and nothing else does; a confinement failure in [`spawn`]
    /// never falls back here. The two questions the sandbox asks of a
    /// program before it creates anything — a relative name, a writable
    /// image — are the sandbox's and are not asked; the program must still
    /// be absolute (`CreateProcessW` is given `lpApplicationName` and
    /// searches nothing) and present, as on the confined path.
    pub(crate) fn spawn_unconfined(
        profile: &Profile,
        binary: &Path,
        command: &Command,
        pipes: Pipes,
        shape: LineShape,
    ) -> Result<ContainedChild, SpawnError> {
        use SpawnError::{NotPrepared, NotStarted};
        if !binary.is_absolute() {
            return Err(NotPrepared(relative_program(binary)));
        }
        if !binary.is_file() {
            return Err(NotStarted(not_an_executable(binary)));
        }
        start(profile, binary, command, pipes, shape, None)
    }

    fn not_an_executable(binary: &Path) -> io::Error {
        io::Error::new(
            io::ErrorKind::NotFound,
            format!(
                "`{}` is not an executable file on this machine",
                binary.display()
            ),
        )
    }

    /// Everything from the pipes to the resumed thread, shared by the
    /// confined and the unconfined spawn. `container` is the one difference:
    /// with it the child enters the AppContainer at `CreateProcessW`; without
    /// it the attribute list carries only the handle list, and the report
    /// names the absence.
    fn start(
        profile: &Profile,
        binary: &Path,
        command: &Command,
        pipes: Pipes,
        shape: LineShape,
        container: Option<&AppContainer>,
    ) -> Result<ContainedChild, SpawnError> {
        use SpawnError::{NotConfinable, NotPrepared, NotStarted};
        // A failure before `CreateProcessW` is the confinement's when a
        // container was asked for, and the bypass's own when none was.
        let prepare = |error: io::Error| {
            if container.is_some() {
                NotConfinable(error)
            } else {
                NotPrepared(error)
            }
        };
        // Inheritable by construction; the parent's own end of each pipe has
        // its inherit flag cleared below, so the child receives one end and
        // never the other.
        let inheritable = SECURITY_ATTRIBUTES {
            nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: std::ptr::null_mut(),
            bInheritHandle: 1,
        };

        let (stdin_child, stdin_parent) = if pipes.stdin {
            let (read, write) = pipe(&inheritable).map_err(prepare)?;
            clear_inherit(write.raw()).map_err(prepare)?;
            (read, Some(write))
        } else {
            (
                null_device(&inheritable, GENERIC_READ).map_err(prepare)?,
                None,
            )
        };
        let (stdout_child, stdout_parent) = if pipes.stdout {
            let (read, write) = pipe(&inheritable).map_err(prepare)?;
            clear_inherit(read.raw()).map_err(prepare)?;
            (write, Some(read))
        } else {
            (
                null_device(&inheritable, GENERIC_WRITE).map_err(prepare)?,
                None,
            )
        };
        let (stderr_child, stderr_parent) = if pipes.stderr {
            let (read, write) = pipe(&inheritable).map_err(prepare)?;
            clear_inherit(read.raw()).map_err(prepare)?;
            (write, Some(read))
        } else {
            (
                null_device(&inheritable, GENERIC_WRITE).map_err(prepare)?,
                None,
            )
        };

        // The job is created before the process and the process is created
        // suspended, so there is no window in which a child could start and
        // fork before it is assigned.
        // SAFETY: both arguments are null, which asks for an unnamed job with
        // default security.
        let job = Owned::new(unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) })
            .map_err(prepare)?;
        let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { std::mem::zeroed() };
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        // SAFETY: the structure and its declared length agree.
        let limited = unsafe {
            SetInformationJobObject(
                job.raw(),
                JobObjectExtendedLimitInformation,
                (&raw const limits).cast::<c_void>(),
                std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            )
        };
        if limited == 0 {
            return Err(prepare(io::Error::last_os_error()));
        }

        let application = wide_path(&conventional(binary));
        let arguments: Vec<Vec<u16>> = command
            .get_args()
            .map(|argument| argument.encode_wide().collect())
            .collect();
        let program = &application[..application.len() - 1];
        let mut line = match (shape, arguments.split_last()) {
            (LineShape::CmdTail, Some((tail, switches))) => {
                shell_command_line(program, switches, tail)
            }
            (LineShape::Argv, _) | (LineShape::CmdTail, None) => command_line(program, &arguments),
        };
        let block = environment_block(
            std::env::vars_os().map(|(name, value)| {
                (
                    name.encode_wide().collect::<Vec<u16>>(),
                    value.encode_wide().collect::<Vec<u16>>(),
                )
            }),
            command.get_envs().map(|(name, value)| {
                (
                    name.encode_wide().collect::<Vec<u16>>(),
                    value.map(|value| value.encode_wide().collect::<Vec<u16>>()),
                )
            }),
        );
        let directory = wide_path(&conventional(
            command.get_current_dir().unwrap_or_else(|| profile.root()),
        ));

        // Two attributes: the container the child enters, and the exact set
        // of handles it may inherit. The second is not tidiness — with
        // `bInheritHandles` true and no list, every inheritable handle this
        // process holds would cross into a sandboxed child — and it is set
        // on the unconfined path too, for the same reason.
        let mut capabilities = container.map(|container| SECURITY_CAPABILITIES {
            AppContainerSid: container.sid(),
            Capabilities: std::ptr::null_mut(),
            CapabilityCount: 0,
            Reserved: 0,
        });
        let mut handles = [stdin_child.raw(), stdout_child.raw(), stderr_child.raw()];
        let mut attributes =
            AttributeList::new(if capabilities.is_some() { 2 } else { 1 }).map_err(prepare)?;
        if let Some(capabilities) = capabilities.as_mut() {
            attributes
                .set(
                    PROC_THREAD_ATTRIBUTE_SECURITY_CAPABILITIES as usize,
                    (&raw mut *capabilities).cast::<c_void>(),
                    std::mem::size_of::<SECURITY_CAPABILITIES>(),
                )
                .map_err(prepare)?;
        }
        attributes
            .set(
                PROC_THREAD_ATTRIBUTE_HANDLE_LIST as usize,
                handles.as_mut_ptr().cast::<c_void>(),
                std::mem::size_of_val(&handles),
            )
            .map_err(prepare)?;

        let mut startup: STARTUPINFOEXW = unsafe { std::mem::zeroed() };
        startup.StartupInfo.cb = std::mem::size_of::<STARTUPINFOEXW>() as u32;
        startup.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
        startup.StartupInfo.hStdInput = stdin_child.raw();
        startup.StartupInfo.hStdOutput = stdout_child.raw();
        startup.StartupInfo.hStdError = stderr_child.raw();
        startup.lpAttributeList = attributes.raw();

        let mut information: PROCESS_INFORMATION = unsafe { std::mem::zeroed() };
        // `CREATE_NO_WINDOW` rather than std's default of inheriting this
        // process's console: a tool call must reach sterna through the pipes
        // above and nowhere else, and a child holding the terminal could
        // write over a TUI that is drawing on it. Without the flag a child
        // with no inherited console allocates a window of its own, which is
        // worse again.
        //
        // SAFETY: every pointer outlives the call. `lpCommandLine` is
        // writable, as `CreateProcessW` requires. `bInheritHandles` is true
        // and the attribute list restricts inheritance to the three handles
        // above -- without that list, every inheritable handle this process
        // holds would cross into a sandboxed child.
        let created = unsafe {
            CreateProcessW(
                application.as_ptr(),
                line.as_mut_ptr(),
                std::ptr::null(),
                std::ptr::null(),
                1,
                EXTENDED_STARTUPINFO_PRESENT
                    | CREATE_UNICODE_ENVIRONMENT
                    | CREATE_SUSPENDED
                    | CREATE_NO_WINDOW,
                block.as_ptr().cast::<c_void>(),
                directory.as_ptr(),
                (&raw const startup).cast(),
                &mut information,
            )
        };
        if created == 0 {
            // The container was ready and the operating system still would
            // not start this program. That is not a permission decision and
            // is not reported as one.
            return Err(NotStarted(io::Error::last_os_error()));
        }
        let process = Owned(information.hProcess);
        let thread = Owned(information.hThread);
        // The child ends are the child's now. Dropping them here closes this
        // process's copies, which is what makes a pipe read see EOF when the
        // child exits.
        drop((stdin_child, stdout_child, stderr_child));

        // SAFETY: `process` and `job` are live handles this function owns.
        if unsafe { AssignProcessToJobObject(job.raw(), process.raw()) } == 0 {
            let failure = io::Error::last_os_error();
            // The process is **not** in the job -- that is what just failed --
            // so terminating the job would reach nothing and leave a
            // suspended child alive for ever. The process itself is what has
            // to be stopped, and it is still suspended, so it has run no
            // instruction of its own.
            // SAFETY: `process` is the handle `CreateProcessW` returned.
            unsafe { TerminateProcess(process.raw(), 1) };
            return Err(prepare(failure));
        }
        // SAFETY: `thread` is the child's suspended primary thread.
        if unsafe { ResumeThread(thread.raw()) } == u32::MAX {
            let failure = io::Error::last_os_error();
            // In the job now, so terminating the job reaches it and anything
            // it could conceivably have started.
            // SAFETY: `job` is the job this call created and owns.
            unsafe { TerminateJobObject(job.raw(), 1) };
            return Err(prepare(failure));
        }

        Ok(ContainedChild {
            process: process.release(),
            job: job.release(),
            pid: information.dwProcessId,
            container: container.map_or_else(
                || "none (dangerously unconfined)".to_string(),
                |container| container.name().to_string(),
            ),
            // SAFETY: each handle is a pipe end this process owns and is
            // giving to the `File`, which closes it exactly once.
            stdin: stdin_parent.map(|end| unsafe { File::from_raw_handle(end.release().cast()) }),
            stdout: stdout_parent.map(|end| unsafe { File::from_raw_handle(end.release().cast()) }),
            stderr: stderr_parent.map(|end| unsafe { File::from_raw_handle(end.release().cast()) }),
        })
    }

    /// The spelling Windows' own command interpreters accept.
    ///
    /// `Profile::compile` canonicalizes the project root and
    /// `tools::invoke::resolve_program` canonicalizes the binary, and on
    /// Windows `canonicalize` returns the **verbatim** form,
    /// `\\?\C:\…`. `CreateProcessW` takes it happily and then `cmd.exe`
    /// prints *"UNC paths are not supported. Defaulting to Windows
    /// directory"* and starts the child in `C:\Windows` — measured on the
    /// Windows ARM64 VM, 2026-09-09, with the confined child reporting it on
    /// its own stderr. A tool whose current directory is not the project is a
    /// tool every relative path lies to.
    ///
    /// So the prefix is dropped for the two strings Windows itself consumes,
    /// and only where dropping it is lossless: a plain drive path shorter
    /// than `MAX_PATH`. A longer path, or a verbatim UNC one, keeps the
    /// prefix — being unable to name a path at all is worse than a cwd a
    /// shell complains about, and neither widens anything.
    fn conventional(path: &Path) -> std::path::PathBuf {
        const MAX_PATH: usize = 260;
        let text = path.as_os_str().to_string_lossy();
        let Some(rest) = text.strip_prefix(r"\\?\") else {
            return path.to_path_buf();
        };
        let drive = rest.as_bytes();
        let plain = drive.len() > 2
            && drive[0].is_ascii_alphabetic()
            && drive[1] == b':'
            && drive[2] == b'\\';
        if !plain || rest.len() >= MAX_PATH {
            return path.to_path_buf();
        }
        std::path::PathBuf::from(rest.to_string())
    }

    fn pipe(attributes: &SECURITY_ATTRIBUTES) -> io::Result<(Owned, Owned)> {
        let mut read: HANDLE = std::ptr::null_mut();
        let mut write: HANDLE = std::ptr::null_mut();
        // SAFETY: both out-parameters are live and `attributes` outlives the
        // call; a zero size asks for the system default buffer.
        if unsafe { CreatePipe(&mut read, &mut write, attributes, 0) } == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok((Owned(read), Owned(write)))
    }

    fn clear_inherit(handle: HANDLE) -> io::Result<()> {
        // SAFETY: `handle` is live; clearing the inherit flag is what keeps
        // Sterna's own end of a pipe out of the child.
        if unsafe { SetHandleInformation(handle, HANDLE_FLAG_INHERIT, 0) } == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    /// An inheritable handle on `NUL`, for a stream that is not a pipe.
    ///
    /// A fresh handle per stream rather than one duplicated three ways: the
    /// handle list attribute takes a set, and two entries naming the same
    /// handle is not a set.
    fn null_device(attributes: &SECURITY_ATTRIBUTES, access: u32) -> io::Result<Owned> {
        let name = wide("NUL");
        // SAFETY: `name` and `attributes` outlive the call.
        let handle = unsafe {
            CreateFileW(
                name.as_ptr(),
                access,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                attributes,
                OPEN_EXISTING,
                0,
                std::ptr::null_mut(),
            )
        };
        Owned::new(handle)
    }

    /// A `PROC_THREAD_ATTRIBUTE_LIST`, sized by the call that fills it.
    ///
    /// `InitializeProcThreadAttributeList` is called twice on purpose: once
    /// with a null buffer to learn the size, once to build the list in it.
    /// The buffer outlives every `UpdateProcThreadAttribute` and the
    /// `CreateProcessW` that reads it, because it is owned by this value and
    /// the value is alive across both.
    struct AttributeList {
        buffer: Vec<u8>,
        initialised: bool,
    }

    impl AttributeList {
        fn new(count: u32) -> io::Result<Self> {
            let mut size: usize = 0;
            // SAFETY: a null list pointer is the documented way to ask for
            // the size; this call is expected to fail with
            // `ERROR_INSUFFICIENT_BUFFER`.
            unsafe {
                InitializeProcThreadAttributeList(std::ptr::null_mut(), count, 0, &mut size);
            }
            if size == 0 {
                return Err(io::Error::last_os_error());
            }
            let mut list = Self {
                buffer: vec![0u8; size],
                initialised: false,
            };
            // SAFETY: the buffer is exactly the size the call above asked
            // for.
            let built =
                unsafe { InitializeProcThreadAttributeList(list.raw(), count, 0, &mut size) };
            if built == 0 {
                return Err(io::Error::last_os_error());
            }
            list.initialised = true;
            Ok(list)
        }

        fn raw(&mut self) -> LPPROC_THREAD_ATTRIBUTE_LIST {
            self.buffer.as_mut_ptr().cast::<c_void>()
        }

        fn set(&mut self, attribute: usize, value: *mut c_void, size: usize) -> io::Result<()> {
            // SAFETY: the list is initialised, `value` points at `size` bytes
            // that outlive the `CreateProcessW` below, and the two optional
            // out-parameters are null as documented.
            let updated = unsafe {
                UpdateProcThreadAttribute(
                    self.raw(),
                    0,
                    attribute,
                    value,
                    size,
                    std::ptr::null_mut(),
                    std::ptr::null(),
                )
            };
            if updated == 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        }
    }

    impl Drop for AttributeList {
        fn drop(&mut self) {
            if self.initialised {
                // SAFETY: the list was initialised by `new` and is deleted
                // once.
                unsafe { DeleteProcThreadAttributeList(self.raw()) };
            }
        }
    }
}
