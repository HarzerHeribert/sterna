//! The Linux applier: Landlock for filesystem rights and seccomp for sockets.
//! `confine` installs both before exec, with inheritance across descendants.
//! Bubblewrap argv construction remains available but is not the spawn path.
//!
//! Historical mount-view design: bubblewrap for the mount view, Landlock for the
//! per-path grants — map line 2455, specification
//! `docs/sandbox.md`.
//!
//! The invariant: **two primitives doing two jobs, and neither one is
//! described as doing the other's.** `bwrap` builds the view a process sees
//! — read-only everywhere, the project read-write, no network namespace —
//! and a Landlock ruleset applies per-path rights inside it. Conflating them
//! is the mistake §3 exists to prevent: the mount view alone is
//! directory-granular and cannot express a rule at all, and Landlock alone
//! leaves the network and the rest of the filesystem exactly where they
//! were.
//!
//! Landlock has no glob and no regex. `Read(**/*.rs)` becomes a read grant on
//! the enclosing directory, coarser than the pattern, with the extension
//! filter enforced by [`Profile::check`] alone — and on a kernel without
//! Landlock, the mount view is the whole enforcement. [`Regime`] is how a
//! session says which of those it is in rather than implying an exactness it
//! has not got.
//!
//! **`EXECUTE` is not part of a directory grant** — the 61D exec-roots
//! ruling, in Landlock's terms. No system root and not the project carries
//! it; [`LandlockRules::executable`] is the only place it is handed out, and
//! it names the one binary the caller resolved. [`LOADER_EXEC_ROOTS`] is the
//! exception the kernel forces and says why, and [`ExecScope`] is how the
//! ruleset states which grant it made.

use super::profile::{Access, Profile, SCRATCH_DIR};
use std::ffi::{OsStr, OsString};
use std::fmt;
use std::path::{Path, PathBuf};

/// The read-only roots a process needs before it can run at all: the loader,
/// the C library, the interpreter it was spawned as.
///
/// **Read and list, and not run.** These carried `EXECUTE` until the 61D
/// exec-roots ruling, which is what let a confined tool exec every binary
/// under `/usr/bin`; the grant that replaces it names one path and lives in
/// [`LandlockRules::executable`].
///
/// Not derived from `permissions`, and not derivable from it — no pattern
/// kind names an interpreter. None of these carries project or user data,
/// and §4's never-grantable set is disjoint from every one of them.
///
/// **`/proc`, `/sys` and `/dev` were here and are not any more.** A rule
/// beneath `/proc` grants `READ_FILE` on `/proc/<pid>/environ` for every
/// process of the same user, which is the whole environment of the harness
/// that spawned the tool — §4.2's credentials by another route, reachable by
/// no `permissions` pattern and narrowable by no `deny` entry. That is
/// harmless under a bubblewrap mount view, which unshares the PID namespace
/// and mounts a fresh `/proc`; it is not harmless in the regime this package
/// can actually apply, which is Landlock alone in the host's own PID
/// namespace (see [`regime`]). `/sys` goes for the same reason and `/dev` is
/// replaced by [`DEVICE_READS`], which names the devices instead of the tree
/// that contains the block devices too.
const SYSTEM_READ_ROOTS: [&str; 7] = ["/usr", "/bin", "/sbin", "/lib", "/lib64", "/etc", "/opt"];

/// Character devices a process may read, named one by one rather than as
/// `/dev`. Each is a device, not a file, and none carries project or user
/// data; a Landlock rule on a path that is not a directory covers that path
/// alone, so this is a grant on five files and not on the tree.
const DEVICE_READS: [&str; 5] = [
    "/dev/null",
    "/dev/zero",
    "/dev/random",
    "/dev/urandom",
    "/dev/tty",
];

/// The roots that keep `EXECUTE` whatever binary is being confined: the ELF
/// interpreter's own.
///
/// **The kernel forces this and a narrower list cannot start a process.**
/// `execve` of a dynamically linked binary makes the kernel `open_exec` the
/// interpreter named in its `PT_INTERP`, and that open carries `FMODE_EXEC`,
/// which Landlock's file-open hook charges `LANDLOCK_ACCESS_FS_EXECUTE`. A
/// ruleset granting exec on the tool alone therefore refuses the tool.
///
/// Shared libraries are **not** why this list exists and would not need it:
/// `ld.so` opens them `O_RDONLY` and maps them `PROT_EXEC`, and Landlock
/// hooks the open rather than the mapping, so `READ_FILE` is all they take.
///
/// It is a directory list, which the ruling calls a list that is always
/// wrong — and it is admissible here for the reason `LOADER_READ_ROOTS` is
/// admissible on macOS: it is loader machinery, not a place tools live. The
/// binaries a confined process could reach through it are the interpreters,
/// and `/usr/bin` is not in it.
const LOADER_EXEC_ROOTS: [&str; 4] = ["/lib", "/lib64", "/usr/lib", "/usr/lib64"];

/// Landlock's filesystem access bits, ABI 1 through 3. Public so a test on
/// any host can assert what a read grant and a write grant are made of.
pub mod access {
    pub const EXECUTE: u64 = 1 << 0;
    pub const WRITE_FILE: u64 = 1 << 1;
    pub const READ_FILE: u64 = 1 << 2;
    pub const READ_DIR: u64 = 1 << 3;
    pub const REMOVE_DIR: u64 = 1 << 4;
    pub const REMOVE_FILE: u64 = 1 << 5;
    pub const MAKE_CHAR: u64 = 1 << 6;
    pub const MAKE_DIR: u64 = 1 << 7;
    pub const MAKE_REG: u64 = 1 << 8;
    pub const MAKE_SOCK: u64 = 1 << 9;
    pub const MAKE_FIFO: u64 = 1 << 10;
    pub const MAKE_BLOCK: u64 = 1 << 11;
    pub const MAKE_SYM: u64 = 1 << 12;
    pub const REFER: u64 = 1 << 13;
    pub const TRUNCATE: u64 = 1 << 14;

    /// What a read grant is: open and list. **Not run** — the 61D
    /// exec-roots ruling took `EXECUTE` out of every directory grant, and
    /// [`super::LandlockRules::executable`] is the only thing that hands it
    /// out. Never `WRITE_FILE`, never any of the `MAKE_*` bits.
    pub const READ: u64 = READ_FILE | READ_DIR;

    /// What an execute grant is: run, and the read the kernel's `open_exec`
    /// performs alongside it.
    pub const EXEC: u64 = EXECUTE | READ_FILE;

    /// The bits a rule on a **file** may carry.
    ///
    /// Landlock refuses a `PATH_BENEATH` rule with `EINVAL` when the
    /// descriptor is not a directory and `allowed_access` carries a
    /// directory-only right, and the refusal takes the whole ruleset with
    /// it — every confined spawn then fails to start. Measured: adding
    /// `/dev/null` to the read grants with [`READ`], which carries
    /// `READ_DIR`, turned both Linux execution tests into
    /// `Os { code: 22, kind: InvalidInput }` on a Landlock ABI 6 kernel.
    /// [`super::confine`] masks with this for any path that is not a
    /// directory.
    pub const FILE: u64 = EXECUTE | READ_FILE | WRITE_FILE | TRUNCATE;

    /// What a write grant is: everything ABI 3 knows how to hand out that a
    /// directory may have. `EXECUTE` is not in it, by way of [`READ`]: the
    /// project root is where model-authored files live and map line 2457
    /// says none of them runs.
    pub const READ_WRITE: u64 = READ
        | WRITE_FILE
        | REMOVE_DIR
        | REMOVE_FILE
        | MAKE_CHAR
        | MAKE_DIR
        | MAKE_REG
        | MAKE_SOCK
        | MAKE_FIFO
        | MAKE_BLOCK
        | MAKE_SYM
        | REFER
        | TRUNCATE;

    /// Every right the ruleset takes responsibility for.
    ///
    /// `EXECUTE` is in here **even though no directory grant carries it any
    /// more**, and that is the trap this constant exists to name: an access
    /// absent from `handled_access_fs` is one the ruleset does not restrict
    /// at all, so dropping it from here would not narrow exec — it would
    /// make exec unrestricted everywhere, silently, while every rule below
    /// still looked correct.
    pub const HANDLED: u64 = READ_WRITE | EXECUTE;
}

/// Which paths the ruleset's `EXECUTE` rules name.
///
/// The two halves of the 61D exec-roots ruling as a value, so what the
/// applier reports says which one happened instead of leaving it to be
/// inferred from a path list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExecScope {
    /// The one binary the caller resolved, as a rule on that file. A sibling
    /// in the same directory is not executable through this ruleset.
    ResolvedBinary,
    /// [`SYSTEM_READ_ROOTS`], because the name could not be resolved and
    /// `execvp` has to search for it. Wider, and reported as such.
    DeclaredRoots,
    /// [`SYSTEM_READ_ROOTS`], the resolved binary **and the project root**,
    /// because the profile admits every command line.
    ///
    /// The invariant is the macOS applier's: **a grant that admits every
    /// command line grants those commands their binaries.** `bash` exists to
    /// exec other programs, so a shell that may exec only itself runs
    /// builtins and nothing else. It widens nothing for a profile that names
    /// its commands.
    RootsAndProject,
}

/// Which of the two grants `binary` earns.
///
/// An absolute path is one sterna resolved — `tools::invoke::exec_grant`
/// produces it with `canonicalize`, the only producer of the resolved case.
/// Anything relative is a bare name no `PATH_BENEATH` rule can name.
pub fn exec_scope(_profile: &Profile, _binary: &Path) -> ExecScope {
    // Every command line is admitted, so a confined command may start any
    // program under the roots: the sandbox bounds what it touches, not
    // which programs it runs.
    ExecScope::RootsAndProject
}

/// Which enforcement this applier achieved on this host.
///
/// The three regimes are genuinely different products, not degrees of the
/// same one, and §3 requires sterna to name which is in force at session
/// start.
///
/// **Two of the four are not reachable from [`regime`] in this package.**
/// Nothing here spawns `bwrap` — [`bwrap_argv`] builds an argv and the spawn
/// path that would run it is not this package's — so the mount view is never
/// installed, and a `regime()` that named it would report an enforcement
/// nobody applied. The two bubblewrap variants therefore describe what
/// [`available_regime`] found the host capable of, and become applicable
/// only when a caller executes that argv.
///
/// `abi` is the Landlock ABI the running kernel reports; the
/// specification asks for 3 or better, because `LANDLOCK_ACCESS_FS_TRUNCATE`
/// arrived there and a write grant without it is a write grant with a hole
/// in it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Regime {
    /// Landlock filesystem restrictions and inherited seccomp socket denial.
    LandlockAndSeccomp { abi: i32 },
    /// Both: the mount view removes the network and everything outside the
    /// project is read-only; Landlock applies per-directory rights inside.
    BubblewrapAndLandlock { abi: i32 },
    /// Bubblewrap alone. Directory-granularity is all there is, and the
    /// pattern's precision comes from [`Profile::check`] entirely.
    BubblewrapOnly,
    /// Landlock alone: per-path rights, but the process keeps the host's
    /// mount view and its network namespace.
    LandlockOnly { abi: i32 },
    /// Neither primitive is available. No OS-level confinement at all.
    Unconfined,
}

impl Regime {
    /// The sentence a session prints at start-up.
    pub fn describe(self) -> String {
        match self {
            Regime::LandlockAndSeccomp { abi } => format!(
                "Landlock ABI {abi} filesystem rules with inherited seccomp network denial: socket/socketpair, io_uring and pidfd_getfd are refused. Host mount/PID namespaces remain. Landlock rules are additive and cannot carve `.claude/` or `.sterna/` out of a writable project: Sterna's direct tool checks refuse those writes, but an arbitrary admitted process can modify them, including `.sterna/config.toml` settings used by a future session."
            ),
            Regime::BubblewrapAndLandlock { abi } => format!(
                "bubblewrap mount view (no network, read-only outside the project) with a Landlock ABI {abi} ruleset. \
                 Landlock has no glob: an extension-filtered pattern is enforced at directory granularity here and exactly by Sterna's own pre-call check."
            ),
            Regime::BubblewrapOnly => "bubblewrap mount view (no network, read-only outside the project) and no Landlock: \
                 this kernel offers no per-path rights, so directory granularity is the whole of the OS layer."
                .to_string(),
            Regime::LandlockOnly { abi } => format!(
                "Landlock ABI {abi} ruleset and no bubblewrap: per-path rights apply, but this process keeps the host's mount view and its network namespace, \
                 and `.claude/` and `.sterna/` are not write-protected by the OS layer — Landlock's rules are additive and cannot carve subdirectories out of a writable project, \
                 so Sterna's own pre-call check is the only thing refusing that write. \
                 `/proc` and `/sys` are not granted at all, because this process shares the host's PID namespace and a read of `/proc` there is a read of every \
                 same-user process's environment; a tool that needs either is refused rather than handed them."
            ),
            Regime::Unconfined => "no OS-level confinement: neither bubblewrap nor Landlock is available on this host, \
                 so Sterna's own pre-call check is the only thing between a tool and the filesystem."
                .to_string(),
        }
    }

    /// Whether the OS layer denies network access. Seccomp denies socket
    /// operations without removing the host network namespace; callers must
    /// not deliberately pass existing network descriptors to the child.
    pub fn removes_network(self) -> bool {
        matches!(
            self,
            Regime::BubblewrapAndLandlock { .. }
                | Regime::BubblewrapOnly
                | Regime::LandlockAndSeccomp { .. }
        )
    }
}

impl fmt::Display for Regime {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.describe())
    }
}

/// The per-path rights a Landlock ruleset grants, as a value.
///
/// Produced without a syscall so a test on any host can assert what the
/// ruleset would say, and so the derivation is reviewable separately from
/// the twenty lines of `unsafe` that install it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LandlockRules {
    /// Directories a process may read and list, and the character devices of
    /// [`DEVICE_READS`], which are files and are granted as themselves.
    /// **Not executable**: see [`access::READ`].
    pub read_only: Vec<PathBuf>,
    /// Directories a process may additionally write, create in and remove
    /// from, plus the literal `/dev/null` sink. File grants are masked to
    /// file-only rights when installed; no `/dev` directory is granted.
    /// Not executable either.
    pub read_write: Vec<PathBuf>,
    /// Everything carrying `LANDLOCK_ACCESS_FS_EXECUTE`: the resolved binary
    /// — or [`SYSTEM_READ_ROOTS`] when there was no path to name — followed
    /// by [`LOADER_EXEC_ROOTS`], which the kernel requires whichever of the
    /// two it is.
    pub executable: Vec<PathBuf>,
    /// Which of those two [`executable`](Self::executable) is.
    pub exec: ExecScope,
}

/// Derives the Landlock ruleset `profile` implies for a child about to exec
/// `binary`.
///
/// Every entry is a fixed system root, the project root, or the binary the
/// caller resolved, and the project root appears only where
/// [`Profile::check`] agrees it is reachable — so a settings document that
/// denies the root produces a ruleset with no write grant in it. `binary`
/// can only narrow: the widest thing it produces is the root list that used
/// to be unconditional.
///
/// **§1.5's `.claude` and `.sterna` write-denies are not in here, because Landlock cannot
/// express it.** A ruleset's rules are additive: an access beneath a granted
/// directory is allowed if *any* matching rule allows it, so a read-only rule
/// on either protected directory beneath a read-write rule on `<root>` removes nothing.
/// That was measured, not assumed —
/// `landlock_alone_does_not_enforce_the_dot_claude_carve_out_and_the_mount_view_does`
/// watched the write succeed on a Landlock ABI 8 kernel. On Linux the
/// carve-out therefore comes from [`bwrap_argv`]'s read-only bind, which is a
/// mount and does override, and from [`Profile::check`], which refuses the
/// write whatever the kernel would have allowed. [`Regime::LandlockOnly`]
/// says so in as many words rather than leaving a reader to assume the
/// ruleset covers it.
pub fn landlock_rules(profile: &Profile, binary: &Path) -> LandlockRules {
    landlock_rules_with_descendants(profile, binary, &[])
}

pub fn landlock_rules_with_descendants(
    profile: &Profile,
    binary: &Path,
    descendants: &[PathBuf],
) -> LandlockRules {
    let root = profile.root().to_path_buf();
    let mut read_only: Vec<PathBuf> = SYSTEM_READ_ROOTS
        .iter()
        .chain(DEVICE_READS.iter())
        .map(PathBuf::from)
        .collect();
    // Git and ordinary command redirection open this sink read/write. Grant
    // the one character device, never its containing directory or siblings.
    let mut read_write = vec![PathBuf::from("/dev/null")];
    read_write.extend(profile.additional_roots().iter().cloned());
    if grants(profile, Access::Write, &root) {
        read_write.push(root.clone());
    } else if grants(profile, Access::Read, &root) {
        read_only.push(root);
    }
    // The toolchain a build reads, and the repository a worktree belongs to
    // — the same subtrees `Profile::check` answers from, so the ruleset and
    // the pre-call check cannot disagree. Landlock has no deny term, so the
    // credential carve-out inside a toolchain home is enforced by the
    // in-process check alone; `Regime` says the OS layer is directory-
    // granular for exactly this class of gap.
    read_only.extend(profile.toolchain_roots().map(Path::to_path_buf));
    read_only.extend(profile.toolchain_read_files().map(Path::to_path_buf));
    read_write.extend(profile.repository_dirs().map(Path::to_path_buf));
    let exec = exec_scope(profile, binary);
    let mut executable: Vec<PathBuf> = match exec {
        ExecScope::ResolvedBinary => vec![binary.to_path_buf()],
        ExecScope::DeclaredRoots => SYSTEM_READ_ROOTS.iter().map(PathBuf::from).collect(),
        // A superset of both, never a replacement: the shell sterna resolved
        // may live outside the system roots, and the project's own scripts
        // are the case the widening exists for.
        ExecScope::RootsAndProject => {
            let mut paths: Vec<PathBuf> = SYSTEM_READ_ROOTS.iter().map(PathBuf::from).collect();
            if binary.is_absolute() {
                paths.push(binary.to_path_buf());
            }
            paths.push(profile.root().to_path_buf());
            paths.extend(profile.additional_roots().iter().cloned());
            paths
        }
    };
    executable.extend(LOADER_EXEC_ROOTS.iter().map(PathBuf::from));
    // A compiler driver execs the next link in its own chain, and only where
    // the scope is already a root list; see the macOS applier's term for the
    // reasoning and for why the resolved arm keeps its single path.
    if !matches!(exec, ExecScope::ResolvedBinary) {
        executable.extend(profile.toolchain_roots().map(Path::to_path_buf));
    }
    executable.extend(
        descendants
            .iter()
            .filter(|path| path.as_path() != binary)
            .cloned(),
    );
    LandlockRules {
        read_only,
        read_write,
        executable,
        exec,
    }
}

/// The bubblewrap argv that builds the mount view for `profile`, running
/// `program` with `args` inside it.
///
/// `--unshare-all` is what removes the network namespace, and it is §4.1's
/// enforcement on this platform: there is no pattern a settings document
/// could carry that would take this flag off, because nothing about
/// `profile` is consulted for it.
///
/// Bind order is the whole of the policy and is not cosmetic: `/` read-only
/// first, then the project read-write over it, then `.claude` and `.sterna`
/// read-only over *that*. bwrap applies binds in argument order, so reversing any pair
/// widens the result. That last bind is the **only** OS-level enforcement of
/// §1.5 on Linux — see [`landlock_rules`] for why the ruleset cannot do it.
pub fn bwrap_argv(profile: &Profile, program: &OsStr, args: &[OsString]) -> Vec<OsString> {
    let root = profile.root();
    let mut argv: Vec<OsString> = [
        "bwrap",
        "--unshare-all",
        "--die-with-parent",
        "--new-session",
    ]
    .iter()
    .map(OsString::from)
    .collect();
    bind(&mut argv, "--ro-bind", Path::new("/"));
    argv.push(OsString::from("--proc"));
    argv.push(OsString::from("/proc"));
    argv.push(OsString::from("--dev"));
    argv.push(OsString::from("/dev"));
    if grants(profile, Access::Write, root) {
        bind(&mut argv, "--bind", root);
        for protected in [root.join(".claude"), root.join(".sterna")] {
            if !grants(profile, Access::Write, &protected) {
                bind(&mut argv, "--ro-bind", &protected);
            }
        }
        // The scratchpad `.sterna/**`'s never rule exempts, read-write over the
        // read-only `.sterna` bind. bwrap cannot bind a missing source, so an
        // absent scratch directory stays read-only in this view.
        let scratch = root.join(SCRATCH_DIR);
        let linked = std::fs::symlink_metadata(&scratch).is_ok_and(|m| m.file_type().is_symlink());
        if scratch.is_dir() && !linked && grants(profile, Access::Write, &scratch) {
            bind(&mut argv, "--bind", &scratch);
        }
    }
    argv.push(OsString::from("--"));
    argv.push(program.to_os_string());
    argv.extend(args.iter().cloned());
    argv
}

fn bind(argv: &mut Vec<OsString>, flag: &str, source: &Path) {
    argv.push(OsString::from(flag));
    argv.push(source.as_os_str().to_os_string());
    argv.push(source.as_os_str().to_os_string());
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

/// Whether `bwrap` is on `PATH`.
///
/// Reading `PATH` decides only whether a primitive is *available*, and its
/// absence makes the reported regime coarser. There is no value of `PATH`
/// that adds a grant, which is why consulting the environment is admissible
/// here and nowhere else in this module.
pub fn bubblewrap_available() -> bool {
    let Some(path) = std::env::var_os("PATH") else {
        return false;
    };
    std::env::split_paths(&path).any(|dir| dir.join("bwrap").is_file())
}

#[cfg(target_os = "linux")]
mod sys {
    /// `landlock_create_ruleset(NULL, 0, LANDLOCK_CREATE_RULESET_VERSION)`
    /// returns the ABI the running kernel implements.
    pub const CREATE_RULESET_VERSION: u32 = 1;
    pub const RULE_PATH_BENEATH: u32 = 1;

    #[repr(C)]
    pub struct RulesetAttr {
        pub handled_access_fs: u64,
    }

    /// `struct landlock_path_beneath_attr` is declared packed in the kernel
    /// headers; a Rust mirror that lets the compiler insert the natural
    /// four bytes of tail padding is a different structure and the syscall
    /// rejects it.
    #[repr(C, packed)]
    pub struct PathBeneathAttr {
        pub allowed_access: u64,
        pub parent_fd: i32,
    }
}

/// The Landlock ABI this kernel implements, or a negative errno.
#[cfg(target_os = "linux")]
pub fn landlock_abi() -> i32 {
    // SAFETY: the documented ABI query — a null attribute pointer with a
    // zero size is what `LANDLOCK_CREATE_RULESET_VERSION` requires, and it
    // creates no ruleset and no file descriptor.
    let abi = unsafe {
        libc::syscall(
            libc::SYS_landlock_create_ruleset,
            std::ptr::null::<sys::RulesetAttr>(),
            0usize,
            sys::CREATE_RULESET_VERSION,
        )
    };
    if abi < 0 { -1 } else { abi as i32 }
}

/// What [`confine`] actually applies on this host.
///
/// Landlock plus seccomp, or no supported confinement. Bubblewrap is absent
/// even where `bwrap` is installed: this package never spawns it, so a
/// regime naming the mount view would claim an enforcement that was not
/// installed — and [`Regime::removes_network`] would answer `true` for a
/// network nothing removed, which is §4.1 reported as enforced when it is
/// not. [`available_regime`] answers the other question, under a name that
/// says which one it is.
#[cfg(target_os = "linux")]
pub fn regime() -> Regime {
    let abi = landlock_abi();
    if abi >= 3 && seccomp_supported_arch() {
        Regime::LandlockAndSeccomp { abi }
    } else {
        Regime::Unconfined
    }
}

/// What this host *could* enforce if a caller composed both primitives —
/// the mount view from [`bwrap_argv`] around the ruleset [`confine`]
/// installs.
///
/// A capability report, not a claim about any process: nothing here has been
/// applied, and a session that prints this must say so. It exists so the
/// spawn path that will run the argv can ask the question, and so the answer
/// stops being confused with [`regime`]'s.
#[cfg(target_os = "linux")]
pub fn available_regime() -> Regime {
    let abi = landlock_abi();
    match (bubblewrap_available(), abi >= 3) {
        (true, true) => Regime::BubblewrapAndLandlock { abi },
        (true, false) => Regime::BubblewrapOnly,
        (false, true) => Regime::LandlockOnly { abi },
        (false, false) => Regime::Unconfined,
    }
}

/// Installs `profile`'s Landlock ruleset and socket-denial seccomp filter on `command`, to take effect in the
/// child between `fork` and `exec`.
///
/// The ruleset is still derived here rather than accepted as an argument,
/// which is requirement 5 made structural: the only thing a caller may hand
/// in is the path it is about to run, and every value of it produces a
/// ruleset at least as narrow as the roots-based one.
///
/// Every path handle is opened in the parent. The child does three syscalls
/// and closes a descriptor; it allocates nothing.
///
/// Returns `Ok(false)` — and installs nothing — on a kernel below ABI 3,
/// because a ruleset that silently drops `TRUNCATE` is a write grant with a
/// hole in it and pretending otherwise is exactly what [`Regime`] exists to
/// stop.
#[cfg(target_os = "linux")]
pub fn confine(
    profile: &Profile,
    binary: &Path,
    command: &mut std::process::Command,
) -> std::io::Result<bool> {
    confine_with_descendants(profile, binary, &[], command)
}

#[cfg(target_os = "linux")]
pub fn confine_with_descendants(
    profile: &Profile,
    binary: &Path,
    descendants: &[PathBuf],
    command: &mut std::process::Command,
) -> std::io::Result<bool> {
    use std::os::fd::{AsRawFd, OwnedFd};
    use std::os::unix::fs::OpenOptionsExt;
    use std::os::unix::process::CommandExt;

    let abi = landlock_abi();
    if abi < 3 || !seccomp_supported_arch() {
        return Ok(false);
    }
    let network_filter = socket_deny_filter(std::env::consts::ARCH)
        .ok_or_else(|| std::io::Error::from_raw_os_error(libc::ENOTSUP))?;
    let rules = landlock_rules_with_descendants(profile, binary, descendants);
    let mut handles: Vec<(OwnedFd, u64)> = Vec::new();
    for (paths, rights) in [
        (&rules.read_only, access::READ),
        (&rules.read_write, access::READ_WRITE),
        // Landlock's rules union along a path's ancestors, so this adds
        // `EXECUTE` to a binary the roots above already made readable
        // without adding it to that binary's siblings.
        (&rules.executable, access::EXEC),
    ] {
        for path in paths {
            // A system root a given distribution does not have is not an
            // error: the ruleset simply grants nothing beneath it.
            let Ok(file) = std::fs::OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_PATH | libc::O_CLOEXEC)
                .open(path)
            else {
                continue;
            };
            // A rule on a file may not carry a directory-only right, and
            // the kernel refuses the whole ruleset rather than that one
            // rule if it does — see [`access::FILE`]. `DEVICE_READS` are
            // files.
            let directory = file.metadata().map(|meta| meta.is_dir()).unwrap_or(true);
            let rights = if directory {
                rights
            } else {
                rights & access::FILE
            };
            handles.push((OwnedFd::from(file), rights));
        }
    }
    let handled = access::HANDLED;
    // SAFETY: `pre_exec` runs in the forked child before `exec`. It performs
    // syscalls on descriptors opened in the parent and allocates nothing.
    unsafe {
        command.pre_exec(move || {
            restrict(handled, &handles)?;
            install_socket_filter(&network_filter)
        });
    }
    return Ok(true);

    fn restrict(handled: u64, handles: &[(OwnedFd, u64)]) -> std::io::Result<()> {
        let attr = sys::RulesetAttr {
            handled_access_fs: handled,
        };
        // SAFETY: `attr` outlives the call and its size is the one the
        // kernel is told to read.
        let ruleset = unsafe {
            libc::syscall(
                libc::SYS_landlock_create_ruleset,
                &attr as *const sys::RulesetAttr,
                std::mem::size_of::<sys::RulesetAttr>(),
                0u32,
            )
        };
        if ruleset < 0 {
            return Err(std::io::Error::last_os_error());
        }
        let ruleset = ruleset as libc::c_int;
        for (handle, rights) in handles {
            let rule = sys::PathBeneathAttr {
                allowed_access: *rights & handled,
                parent_fd: handle.as_raw_fd(),
            };
            // SAFETY: `rule` matches the kernel's packed layout and lives
            // across the call; `handle` is an open `O_PATH` descriptor.
            let added = unsafe {
                libc::syscall(
                    libc::SYS_landlock_add_rule,
                    ruleset,
                    sys::RULE_PATH_BENEATH,
                    &rule as *const sys::PathBeneathAttr,
                    0u32,
                )
            };
            if added < 0 {
                let error = std::io::Error::last_os_error();
                // SAFETY: `ruleset` is the descriptor the call above returned.
                unsafe { libc::close(ruleset) };
                return Err(error);
            }
        }
        // `no_new_privs` first: without it `landlock_restrict_self` refuses,
        // and with it a set-uid binary cannot hand the rights back.
        // SAFETY: `prctl` with these arguments reads no memory.
        if unsafe { libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) } != 0 {
            let error = std::io::Error::last_os_error();
            // SAFETY: as above.
            unsafe { libc::close(ruleset) };
            return Err(error);
        }
        // SAFETY: `ruleset` is a live ruleset descriptor; the call consumes
        // no memory and applies to the calling thread only, which after
        // `fork` is the whole child.
        let applied = unsafe { libc::syscall(libc::SYS_landlock_restrict_self, ruleset, 0u32) };
        let error = std::io::Error::last_os_error();
        // SAFETY: as above.
        unsafe { libc::close(ruleset) };
        if applied < 0 {
            return Err(error);
        }
        Ok(())
    }
}

/// Native ABIs with an audited syscall map. Compat ABIs are killed by the
/// filter's architecture guard; x32 syscall numbers are killed separately.
pub fn seccomp_supported_arch() -> bool {
    matches!(std::env::consts::ARCH, "x86_64" | "aarch64")
}

/// Linux classic BPF instruction layout; portable so policy tests run on macOS.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SocketFilterInstruction {
    pub code: u16,
    pub jt: u8,
    pub jf: u8,
    pub k: u32,
}

/// Fail-closed network policy. All new sockets (including Unix sockets) are
/// denied to prevent local network proxies and SCM_RIGHTS descriptor receipt.
/// io_uring is denied because its socket operations bypass syscall filtering.
/// pidfd_getfd cannot import another process's existing network descriptor.
/// This does not revoke descriptors deliberately inherited from the parent.
pub fn socket_deny_filter(arch: &str) -> Option<Vec<SocketFilterInstruction>> {
    let (audit_arch, socket_calls): (u32, &[u32]) = match arch {
        "x86_64" => (
            0xc000_003e,
            &[41, 53, 42, 49, 50, 43, 288, 44, 46, 307, 47, 299],
        ),
        "aarch64" => (
            0xc000_00b7,
            &[198, 199, 203, 200, 201, 202, 242, 206, 211, 269, 212, 243],
        ),
        _ => return None,
    };
    let instruction = |code, jt, jf, k| SocketFilterInstruction { code, jt, jf, k };
    let mut filter = vec![
        instruction(0x20, 0, 0, 4),           // LD W ABS seccomp_data.arch
        instruction(0x15, 1, 0, audit_arch),  // JEQ native arch, skip kill
        instruction(0x06, 0, 0, 0x8000_0000), // RET KILL_PROCESS
        instruction(0x20, 0, 0, 0),           // LD W ABS seccomp_data.nr
        instruction(0x35, 0, 1, 0x4000_0000), // JGE x32 bit / invalid syscall
        instruction(0x06, 0, 0, 0x8000_0000),
    ];
    // Syscall IDs 425..427 and 438 are shared by the two supported ABIs.
    for syscall in socket_calls.iter().copied().chain([425, 426, 427, 438]) {
        filter.push(instruction(0x15, 0, 1, syscall));
        filter.push(instruction(0x06, 0, 0, 0x0005_0001)); // RET ERRNO EPERM
    }
    filter.push(instruction(0x06, 0, 0, 0x7fff_0000)); // RET ALLOW
    Some(filter)
}

#[cfg(target_os = "linux")]
fn install_socket_filter(filter: &[SocketFilterInstruction]) -> std::io::Result<()> {
    #[repr(C)]
    struct Program {
        len: u16,
        filter: *const SocketFilterInstruction,
    }
    let program = Program {
        len: filter.len() as u16,
        filter: filter.as_ptr(),
    };
    // no_new_privs was installed by restrict(). The pre-exec child is single
    // threaded; seccomp filters are inherited by clone/fork and survive exec.
    // SAFETY: Program and instructions have Linux sock_fprog/sock_filter ABI
    // layout and remain live throughout this synchronous kernel copy.
    let result = unsafe { libc::prctl(libc::PR_SET_SECCOMP, 2, &program as *const Program, 0, 0) };
    if result != 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}
