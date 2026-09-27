//! The Linux applier: Landlock for filesystem rights, seccomp for sockets,
//! and -- where the kernel allows unprivileged user namespaces -- a private
//! mount and network namespace ([`super::linux_ns`]).
//!
//! **The sandbox is wide.** Every file is readable and every program may run,
//! except the secrets; writes reach the writable places
//! ([`Profile::writable_places`]). Landlock's rules are additive and cannot
//! subtract a path from a grant, so "everything but the secrets" is spelled
//! as its complement ([`complement`]): each directory on the way to a secret
//! may only be listed, and everything beside that way is granted whole.
//!
//! **Two regimes, and a session says which** ([`Regime`]). In the namespaced
//! one the secrets are also covered by empty mounts, `.git/hooks`,
//! `.git/config`, `.sterna` and `.claude` are read-only mounts inside the
//! writable places, and the network namespace's only way out is the relay to
//! Sterna's proxy. Without namespaces a command has Landlock and no network
//! at all, and the protected paths inside the project are guarded by Sterna's
//! own checks alone.

use super::profile::{Profile, SCRATCH_DIR};
use std::fmt;
use std::path::{Path, PathBuf};

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

    /// What a read grant is: open, list and run. The sandbox bounds what a
    /// command touches, not which programs it starts.
    pub const READ: u64 = READ_FILE | READ_DIR | EXECUTE;

    /// A directory on the way to a secret: its entries may be listed, and
    /// nothing beneath it is opened through this rule.
    pub const LIST: u64 = READ_DIR;

    /// The bits a rule on a **file** may carry. Landlock refuses a rule on a
    /// file that carries a directory-only right, and the refusal takes the
    /// whole ruleset with it, so [`super::confine`] masks with this for any
    /// path that is not a directory.
    pub const FILE: u64 = EXECUTE | READ_FILE | WRITE_FILE | TRUNCATE;

    /// What a write grant is: everything ABI 3 hands out.
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

    /// Every right the ruleset takes responsibility for. An access absent
    /// from here is one the ruleset does not restrict at all.
    pub const HANDLED: u64 = READ_WRITE;
}

/// Which enforcement this applier achieves on this host.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Regime {
    /// Landlock, seccomp, and a private user, mount and network namespace.
    Namespaced { abi: i32 },
    /// Landlock and seccomp in the host's namespaces: this kernel refuses
    /// unprivileged user namespaces.
    LandlockAndSeccomp { abi: i32 },
    /// No Landlock ABI 3 or no audited seccomp map: tools are not spawned.
    Unconfined,
}

impl Regime {
    /// The sentence the doctor and a session print.
    pub fn describe(self) -> String {
        match self {
            Regime::Namespaced { abi } => format!(
                "Landlock ABI {abi} in a private user, mount and network namespace: every file is readable except the secrets, which are also covered; \
                 writes reach the writable places, where .git/hooks, .git/config, .sterna and .claude stay read-only; \
                 the network is reachable only through Sterna's proxy."
            ),
            Regime::LandlockAndSeccomp { abi } => format!(
                "Landlock ABI {abi} with seccomp, without namespaces (this kernel refuses unprivileged user namespaces): \
                 every file is readable except the secrets and writes reach the writable places, but commands have no network, \
                 and .git/hooks, .sterna and .claude inside the project are protected by Sterna's own checks only."
            ),
            Regime::Unconfined => {
                "no Landlock ABI 3 or no supported seccomp architecture on this host, \
                 so Sterna refuses to spawn tools rather than run them unconfined."
                    .to_string()
            }
        }
    }

    /// Whether a command reaches any host at all (through the proxy).
    pub fn reaches_network(self) -> bool {
        matches!(self, Regime::Namespaced { .. })
    }
}

impl fmt::Display for Regime {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.describe())
    }
}

/// The per-path rights a Landlock ruleset grants, as a value, so a test on
/// any host can assert what the ruleset would say.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LandlockRules {
    /// Read, list and run: everything off the way to a secret.
    pub read: Vec<PathBuf>,
    /// List only: each directory on the way to a secret.
    pub list: Vec<PathBuf>,
    /// Read, write and run: the writable places (their own complement when a
    /// secret lies inside one) and the `/dev/null` sink.
    pub read_write: Vec<PathBuf>,
}

/// The ruleset `profile` implies, reading directories on this host.
pub fn landlock_rules(profile: &Profile) -> LandlockRules {
    landlock_rules_over(profile, &entries)
}

/// The ruleset `profile` implies, with `children` listing a directory's
/// entries; injected so the derivation is testable on any host.
pub fn landlock_rules_over(
    profile: &Profile,
    children: &dyn Fn(&Path) -> Vec<PathBuf>,
) -> LandlockRules {
    let secrets = profile.secret_paths();
    let reads = complement(Path::new("/"), &secrets, children);
    let mut list = reads.listed;
    let mut read_write = vec![PathBuf::from("/dev/null")];
    for place in profile.writable_places() {
        // A write grant never reaches into a secret: a place under one is
        // not granted, and a place holding one is granted as its own
        // complement.
        if secrets.iter().any(|secret| place.starts_with(secret)) {
            continue;
        }
        if secrets.iter().any(|secret| secret.starts_with(&place)) {
            let inside = complement(&place, &secrets, children);
            read_write.extend(inside.granted);
            list.extend(inside.listed);
        } else {
            read_write.push(place);
        }
    }
    list.sort();
    list.dedup();
    LandlockRules {
        read: reads.granted,
        list,
        read_write,
    }
}

/// What [`complement`] grants: whole subtrees, and directories that may only
/// be listed.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Complement {
    pub granted: Vec<PathBuf>,
    pub listed: Vec<PathBuf>,
}

/// The paths a grant on `start` covers so that everything beneath it is
/// reachable except `excluded` and what lies beneath each: each directory on
/// the way to an excluded path is listed rather than granted, every sibling
/// off that way is granted whole, and the excluded path itself is not.
///
/// An entry that is a symbolic link is granted as itself, which grants
/// nothing beyond the link: Landlock follows it to its target, which is
/// covered, or not, by its own place in the walk.
pub fn complement(
    start: &Path,
    excluded: &[PathBuf],
    children: &dyn Fn(&Path) -> Vec<PathBuf>,
) -> Complement {
    let mut out = Complement::default();
    if excluded.iter().any(|path| start.starts_with(path)) {
        return out;
    }
    if excluded.iter().any(|path| path.starts_with(start)) {
        walk(start, excluded, children, &mut out);
    } else {
        out.granted.push(start.to_path_buf());
    }
    out.granted.sort();
    out.granted.dedup();
    out.listed.sort();
    out.listed.dedup();
    out
}

fn walk(
    dir: &Path,
    excluded: &[PathBuf],
    children: &dyn Fn(&Path) -> Vec<PathBuf>,
    out: &mut Complement,
) {
    out.listed.push(dir.to_path_buf());
    for child in children(dir) {
        if excluded.iter().any(|path| path == &child) {
            continue;
        }
        if excluded.iter().any(|path| path.starts_with(&child)) {
            walk(&child, excluded, children, out);
        } else {
            out.granted.push(child);
        }
    }
}

/// A directory's entries on this host, or none when it cannot be read.
pub fn entries(dir: &Path) -> Vec<PathBuf> {
    std::fs::read_dir(dir)
        .map(|entries| entries.flatten().map(|entry| entry.path()).collect())
        .unwrap_or_default()
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

/// What [`confine`] applies on this host.
#[cfg(target_os = "linux")]
pub fn regime() -> Regime {
    let abi = landlock_abi();
    if abi < 3 || !seccomp_supported_arch() {
        Regime::Unconfined
    } else if super::linux_ns::available() {
        Regime::Namespaced { abi }
    } else {
        Regime::LandlockAndSeccomp { abi }
    }
}

/// Installs `profile`'s sandbox on `command`, to take effect in the child
/// between `fork` and `exec`: the namespace view where [`regime`] has one,
/// then the Landlock ruleset, then the seccomp filter.
///
/// Every path handle and the view are prepared in the parent; the child only
/// makes system calls.
///
/// Returns `Ok(false)` -- and installs nothing -- on a kernel below ABI 3,
/// because a ruleset that silently drops `TRUNCATE` is a write grant with a
/// hole in it.
#[cfg(target_os = "linux")]
pub fn confine(profile: &Profile, command: &mut std::process::Command) -> std::io::Result<bool> {
    use std::os::fd::{AsRawFd, OwnedFd};
    use std::os::unix::fs::OpenOptionsExt;
    use std::os::unix::process::CommandExt;

    let regime = regime();
    let view = match regime {
        Regime::Unconfined => return Ok(false),
        Regime::Namespaced { .. } => Some(super::linux_ns::View::prepare(
            &profile.secret_paths(),
            &profile.protected_paths(),
            &[profile.root().join(SCRATCH_DIR)],
            profile
                .proxy()
                .and_then(|route| route.unix.as_deref().map(|unix| (route.port, unix))),
        )?),
        Regime::LandlockAndSeccomp { .. } => None,
    };
    let filter = if view.is_some() {
        unix_socket_deny_filter(std::env::consts::ARCH)
    } else {
        socket_deny_filter(std::env::consts::ARCH)
    }
    .ok_or_else(|| std::io::Error::from_raw_os_error(libc::ENOTSUP))?;
    let rules = landlock_rules(profile);
    let mut handles: Vec<(OwnedFd, u64)> = Vec::new();
    for (paths, rights) in [
        (&rules.read, access::READ),
        (&rules.list, access::LIST),
        (&rules.read_write, access::READ_WRITE),
    ] {
        for path in paths {
            // A path that vanished since it was listed grants nothing.
            let Ok(file) = std::fs::OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_PATH | libc::O_CLOEXEC)
                .open(path)
            else {
                continue;
            };
            let directory = file.metadata().map(|meta| meta.is_dir()).unwrap_or(true);
            let rights = if directory {
                rights
            } else {
                rights & access::FILE
            };
            if rights != 0 {
                handles.push((OwnedFd::from(file), rights));
            }
        }
    }
    let handled = access::HANDLED;
    // SAFETY: `pre_exec` runs in the forked child before `exec`. It performs
    // system calls on memory and descriptors prepared in the parent and
    // allocates nothing.
    unsafe {
        command.pre_exec(move || {
            if let Some(view) = &view {
                view.enter()?;
            }
            restrict(handled, &handles)?;
            install_socket_filter(&filter)
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

/// The filter inside the network namespace, where the only interface is
/// loopback and the only way out is the relay to Sterna's proxy: internet
/// sockets are allowed, because nothing they reach leaves the namespace.
/// A Unix socket is refused, because a socket file on disk is reachable
/// across network namespaces -- the path out that bypasses the proxy.
/// `socketpair` stays allowed: its two ends reach only each other. io_uring
/// and `pidfd_getfd` are refused as in [`socket_deny_filter`].
pub fn unix_socket_deny_filter(arch: &str) -> Option<Vec<SocketFilterInstruction>> {
    let (audit_arch, socket): (u32, u32) = match arch {
        "x86_64" => (0xc000_003e, 41),
        "aarch64" => (0xc000_00b7, 198),
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
    for syscall in [425, 426, 427, 438] {
        filter.push(instruction(0x15, 0, 1, syscall));
        filter.push(instruction(0x06, 0, 0, 0x0005_0001)); // RET ERRNO EPERM
    }
    filter.push(instruction(0x15, 0, 3, socket)); // JEQ socket, else allow
    filter.push(instruction(0x20, 0, 0, 16)); // LD W ABS seccomp_data.args[0]
    filter.push(instruction(0x15, 0, 1, libc_af_unix())); // JEQ AF_UNIX
    filter.push(instruction(0x06, 0, 0, 0x0005_0001)); // RET ERRNO EPERM
    filter.push(instruction(0x06, 0, 0, 0x7fff_0000)); // RET ALLOW
    Some(filter)
}

/// `AF_UNIX`, spelled here so the filter builds on every host.
const fn libc_af_unix() -> u32 {
    1
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
