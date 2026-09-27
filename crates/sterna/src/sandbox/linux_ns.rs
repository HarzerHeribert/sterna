//! The Linux namespace view a confined command runs in, where the kernel
//! allows one: its own user, mount and network namespace.
//!
//! **Three things Landlock cannot do, done here.** Landlock's rules are
//! additive, so it cannot make `.git/hooks` or `.sterna` read-only inside a
//! writable project, and it has no network rule that means "only these
//! hosts". A mount namespace can: a read-only bind over each protected path,
//! an empty read-only filesystem over each secret. A network namespace can:
//! its only interface is loopback, and the only way out of it is a relay on
//! `127.0.0.1:<port>` that forwards to Sterna's proxy through a Unix socket
//! on disk, which is reachable across network namespaces. The proxy decides
//! which hosts a command may reach ([`super::proxy`]).
//!
//! **Where the kernel refuses unprivileged user namespaces, none of this
//! applies** and the command keeps the Landlock-only regime, with no network
//! at all. [`available`] decides once per process, by trying the whole
//! sequence in a throwaway child, so a session never learns mid-command
//! that a spawn cannot be set up.
//!
//! **Everything that runs after `fork` is async-signal-safe.** The paths,
//! the id maps and the socket address are prepared in the parent
//! ([`View::prepare`]); the child only makes system calls on them.

// Wired into the Linux applier in the next change; until then only the
// probe and the view are reached.
#![allow(dead_code)]

use std::ffi::CString;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

/// One read-only bind: the path, and the flags of the mount it sits on,
/// which a remount inside a user namespace must repeat or be refused.
struct ReadOnly {
    path: CString,
    locked: libc::c_ulong,
}

/// One secret: a directory gets an empty read-only filesystem over it, a
/// file gets `/dev/null`.
struct Hidden {
    path: CString,
    directory: bool,
}

/// Where the relay forwards to: Sterna's proxy, on a Unix socket, and the
/// port the command is told the proxy listens on.
struct Relay {
    port: u16,
    socket: libc::sockaddr_un,
    socket_len: libc::socklen_t,
}

/// Everything the child needs, prepared in the parent.
pub(crate) struct View {
    uid_map: Vec<u8>,
    gid_map: Vec<u8>,
    read_only: Vec<ReadOnly>,
    writable_inside: Vec<CString>,
    hidden: Vec<Hidden>,
    relay: Option<Relay>,
    dev_null: CString,
}

// SAFETY: `View` holds owned bytes and plain-data C structs only; nothing in
// it refers to thread-local or process-shared state.
unsafe impl Send for View {}
unsafe impl Sync for View {}

/// Paths inside a writable place that stay read-only for a command: code
/// written there runs later outside the sandbox (a git hook, a git config
/// alias) or widens the next session (Sterna's own settings).
pub(crate) fn protected_paths(writable: &[PathBuf]) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for root in writable {
        for name in [".git/hooks", ".git/config", ".sterna", ".claude"] {
            let path = root.join(name);
            if path.exists() {
                out.push(path);
            }
        }
        // A worktree's repository keeps its hooks and config at the top of
        // the common directory, not under `.git`.
        for name in ["hooks", "config"] {
            let path = root.join(name);
            if root.join("HEAD").is_file() && path.exists() {
                out.push(path);
            }
        }
    }
    out.sort();
    out.dedup();
    out
}

impl View {
    /// Prepares the view for one command. `hidden` are the secrets to cover,
    /// `protected` the paths to make read-only, `scratch` the writable
    /// subtrees inside a protected path (`.sterna/scratch`), and `relay` the
    /// proxy's port and socket, when commands may reach any host at all.
    pub(crate) fn prepare(
        hidden: &[PathBuf],
        protected: &[PathBuf],
        scratch: &[PathBuf],
        relay: Option<(u16, &Path)>,
    ) -> std::io::Result<Self> {
        // SAFETY: `getuid`/`getgid` read the caller's own ids and never fail.
        let (uid, gid) = unsafe { (libc::getuid(), libc::getgid()) };
        let read_only = protected
            .iter()
            .filter(|path| path.exists())
            .map(|path| {
                Ok(ReadOnly {
                    path: c_path(path)?,
                    locked: locked_flags(path),
                })
            })
            .collect::<std::io::Result<Vec<_>>>()?;
        let writable_inside = scratch
            .iter()
            .filter(|path| path.is_dir())
            .map(|path| c_path(path))
            .collect::<std::io::Result<Vec<_>>>()?;
        let hidden = hidden
            .iter()
            .filter_map(|path| {
                let meta = std::fs::symlink_metadata(path).ok()?;
                if meta.file_type().is_symlink() {
                    return None;
                }
                Some(c_path(path).map(|c| Hidden {
                    path: c,
                    directory: meta.is_dir(),
                }))
            })
            .collect::<std::io::Result<Vec<_>>>()?;
        let relay = relay.map(|(port, socket)| {
            unix_address(socket).map(|(address, len)| Relay {
                port,
                socket: address,
                socket_len: len,
            })
        });
        let relay = relay.transpose()?;
        Ok(Self {
            uid_map: format!("{uid} {uid} 1\n").into_bytes(),
            gid_map: format!("{gid} {gid} 1\n").into_bytes(),
            read_only,
            writable_inside,
            hidden,
            relay,
            dev_null: CString::new("/dev/null").expect("no interior NUL"),
        })
    }

    /// Enters the view. Called in the forked child before `exec`, before
    /// Landlock and seccomp; async-signal-safe.
    ///
    /// # Safety
    /// Only in a single-threaded child between `fork` and `exec`.
    pub(crate) unsafe fn enter(&self) -> std::io::Result<()> {
        // SAFETY: every call below is a system call on memory this struct
        // owns, valid for the duration of the call, in a single-threaded
        // child; none allocates.
        unsafe {
            check(libc::unshare(
                libc::CLONE_NEWUSER | libc::CLONE_NEWNS | libc::CLONE_NEWNET,
            ))?;
            write_file(c"/proc/self/setgroups", b"deny")?;
            write_file(c"/proc/self/uid_map", &self.uid_map)?;
            write_file(c"/proc/self/gid_map", &self.gid_map)?;
            check(libc::mount(
                std::ptr::null(),
                c"/".as_ptr(),
                std::ptr::null(),
                libc::MS_REC | libc::MS_PRIVATE,
                std::ptr::null(),
            ))?;
            for protected in &self.read_only {
                bind(&protected.path, &protected.path)?;
            }
            // Before the remount below, so the scratch bind keeps the
            // writable mount it was made from.
            for inside in &self.writable_inside {
                bind(inside, inside)?;
            }
            for protected in &self.read_only {
                check(libc::mount(
                    std::ptr::null(),
                    protected.path.as_ptr(),
                    std::ptr::null(),
                    libc::MS_BIND | libc::MS_REMOUNT | libc::MS_RDONLY | protected.locked,
                    std::ptr::null(),
                ))?;
            }
            for hidden in &self.hidden {
                if hidden.directory {
                    check(libc::mount(
                        c"tmpfs".as_ptr(),
                        hidden.path.as_ptr(),
                        c"tmpfs".as_ptr(),
                        libc::MS_RDONLY | libc::MS_NOSUID | libc::MS_NODEV | libc::MS_NOEXEC,
                        c"mode=0,size=4k".as_ptr().cast(),
                    ))?;
                } else {
                    bind(&self.dev_null, &hidden.path)?;
                }
            }
            loopback_up()?;
            if let Some(relay) = &self.relay {
                start_relay(relay, &self.dev_null)?;
            }
        }
        Ok(())
    }
}

/// Whether this process may build the view: tried once, in a throwaway
/// child, with every step [`View::enter`] takes on a scratch directory.
pub fn available() -> bool {
    static AVAILABLE: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *AVAILABLE.get_or_init(probe)
}

fn probe() -> bool {
    let Ok(scratch) = tempdir() else {
        return false;
    };
    let protected = scratch.join("protected");
    let inside = protected.join("inside");
    let secret = scratch.join("secret");
    let ok = std::fs::create_dir_all(&inside).is_ok() && std::fs::create_dir_all(&secret).is_ok();
    let result = ok
        && View::prepare(&[secret], std::slice::from_ref(&protected), &[inside], None)
            .ok()
            .is_some_and(|view| {
                let mut command = std::process::Command::new("/bin/sh");
                command
                    .args(["-c", "exit 0"])
                    .stdin(std::process::Stdio::null())
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null());
                use std::os::unix::process::CommandExt;
                // SAFETY: `enter` is async-signal-safe and runs in the child.
                unsafe {
                    command.pre_exec(move || view.enter());
                }
                command.status().is_ok_and(|status| status.success())
            });
    let _ = std::fs::remove_dir_all(&scratch);
    result
}

fn tempdir() -> std::io::Result<PathBuf> {
    let base = std::env::temp_dir();
    for attempt in 0..16u32 {
        let path = base.join(format!(".sterna-ns-probe-{}-{attempt}", std::process::id()));
        match std::fs::create_dir(&path) {
            Ok(()) => return Ok(path),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
    }
    Err(std::io::Error::from(std::io::ErrorKind::AlreadyExists))
}

fn c_path(path: &Path) -> std::io::Result<CString> {
    CString::new(path.as_os_str().as_bytes())
        .map_err(|_| std::io::Error::from(std::io::ErrorKind::InvalidInput))
}

/// The flags of the mount `path` lives on that a remount inside a user
/// namespace may not drop.
fn locked_flags(path: &Path) -> libc::c_ulong {
    let Ok(c) = c_path(path) else { return 0 };
    // SAFETY: `c` is a valid NUL-terminated path and `stat` is written by
    // the kernel before it is read.
    let mut stat: libc::statvfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::statvfs(c.as_ptr(), &mut stat) } != 0 {
        return 0;
    }
    let flag = stat.f_flag;
    let mut locked = 0;
    for (st, ms) in [
        (libc::ST_NOSUID, libc::MS_NOSUID),
        (libc::ST_NODEV, libc::MS_NODEV),
        (libc::ST_NOEXEC, libc::MS_NOEXEC),
        (libc::ST_NOATIME, libc::MS_NOATIME),
        (libc::ST_NODIRATIME, libc::MS_NODIRATIME),
        (libc::ST_RELATIME, libc::MS_RELATIME),
    ] {
        if flag & st != 0 {
            locked |= ms;
        }
    }
    locked
}

fn unix_address(path: &Path) -> std::io::Result<(libc::sockaddr_un, libc::socklen_t)> {
    // SAFETY: an all-zero `sockaddr_un` is a valid empty address.
    let mut address: libc::sockaddr_un = unsafe { std::mem::zeroed() };
    address.sun_family = libc::AF_UNIX as libc::sa_family_t;
    let bytes = path.as_os_str().as_bytes();
    if bytes.len() >= address.sun_path.len() {
        return Err(std::io::Error::from(std::io::ErrorKind::InvalidInput));
    }
    for (slot, byte) in address.sun_path.iter_mut().zip(bytes) {
        *slot = *byte as libc::c_char;
    }
    let len = std::mem::size_of::<libc::sa_family_t>() + bytes.len() + 1;
    Ok((address, len as libc::socklen_t))
}

fn check(result: libc::c_int) -> std::io::Result<()> {
    if result < 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(())
    }
}

/// # Safety
/// Async-signal-safe; `path` must be NUL-terminated.
unsafe fn write_file(path: &std::ffi::CStr, bytes: &[u8]) -> std::io::Result<()> {
    // SAFETY: the caller's contract; the buffer outlives the call.
    unsafe {
        let fd = libc::open(path.as_ptr(), libc::O_WRONLY | libc::O_CLOEXEC);
        if fd < 0 {
            return Err(std::io::Error::last_os_error());
        }
        let written = libc::write(fd, bytes.as_ptr().cast(), bytes.len());
        let error = std::io::Error::last_os_error();
        libc::close(fd);
        if written != bytes.len() as isize {
            return Err(error);
        }
    }
    Ok(())
}

/// # Safety
/// Async-signal-safe; both paths NUL-terminated.
unsafe fn bind(source: &CString, target: &CString) -> std::io::Result<()> {
    // SAFETY: the caller's contract.
    check(unsafe {
        libc::mount(
            source.as_ptr(),
            target.as_ptr(),
            std::ptr::null(),
            libc::MS_BIND | libc::MS_REC,
            std::ptr::null(),
        )
    })
}

/// `struct ifreq` as the two flag ioctls read it: a name and a short.
#[repr(C)]
struct InterfaceFlags {
    name: [u8; 16],
    flags: libc::c_short,
    _pad: [u8; 22],
}

/// Brings `lo` up in the new network namespace, where it starts down.
///
/// # Safety
/// Async-signal-safe.
unsafe fn loopback_up() -> std::io::Result<()> {
    const SIOCGIFFLAGS: libc::c_ulong = 0x8913;
    const SIOCSIFFLAGS: libc::c_ulong = 0x8914;
    // SAFETY: a datagram socket used only for two ioctls on a struct this
    // function owns, then closed.
    unsafe {
        let fd = libc::socket(libc::AF_INET, libc::SOCK_DGRAM | libc::SOCK_CLOEXEC, 0);
        if fd < 0 {
            return Err(std::io::Error::last_os_error());
        }
        let mut request = InterfaceFlags {
            name: [0; 16],
            flags: 0,
            _pad: [0; 22],
        };
        request.name[..2].copy_from_slice(b"lo");
        let result = if libc::ioctl(fd, SIOCGIFFLAGS as _, &mut request) == 0 {
            request.flags |= (libc::IFF_UP | libc::IFF_RUNNING) as libc::c_short;
            libc::ioctl(fd, SIOCSIFFLAGS as _, &request)
        } else {
            -1
        };
        let error = std::io::Error::last_os_error();
        libc::close(fd);
        if result != 0 {
            return Err(error);
        }
    }
    Ok(())
}

/// Starts the relay: a process inside the namespace listening on
/// `127.0.0.1:<port>` that hands each connection to the proxy's socket.
///
/// It is forked before Landlock and seccomp are installed, holds no
/// descriptor but its listening socket, never execs, and dies with the
/// command (`PR_SET_PDEATHSIG`).
///
/// # Safety
/// Async-signal-safe, in the single-threaded pre-exec child.
unsafe fn start_relay(relay: &Relay, dev_null: &CString) -> std::io::Result<()> {
    // SAFETY: plain system calls on owned memory; the forked relay only
    // calls async-signal-safe functions and `_exit`s.
    unsafe {
        let listener = libc::socket(libc::AF_INET, libc::SOCK_STREAM | libc::SOCK_CLOEXEC, 0);
        if listener < 0 {
            return Err(std::io::Error::last_os_error());
        }
        let one: libc::c_int = 1;
        libc::setsockopt(
            listener,
            libc::SOL_SOCKET,
            libc::SO_REUSEADDR,
            (&one as *const libc::c_int).cast(),
            std::mem::size_of::<libc::c_int>() as libc::socklen_t,
        );
        let mut address: libc::sockaddr_in = std::mem::zeroed();
        address.sin_family = libc::AF_INET as libc::sa_family_t;
        address.sin_port = relay.port.to_be();
        address.sin_addr.s_addr = u32::from_ne_bytes([127, 0, 0, 1]);
        if libc::bind(
            listener,
            (&address as *const libc::sockaddr_in).cast(),
            std::mem::size_of::<libc::sockaddr_in>() as libc::socklen_t,
        ) != 0
            || libc::listen(listener, 64) != 0
        {
            let error = std::io::Error::last_os_error();
            libc::close(listener);
            return Err(error);
        }
        let parent = libc::getpid();
        match libc::fork() {
            -1 => {
                let error = std::io::Error::last_os_error();
                libc::close(listener);
                Err(error)
            }
            0 => {
                // The relay.
                libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL);
                if libc::getppid() != parent {
                    libc::_exit(0);
                }
                quiet(dev_null, listener);
                libc::signal(libc::SIGCHLD, libc::SIG_IGN);
                relay_loop(listener, relay)
            }
            _ => {
                libc::close(listener);
                Ok(())
            }
        }
    }
}

/// Points stdin, stdout and stderr at `/dev/null` and closes every other
/// descriptor but `keep`, so the relay holds none of the command's pipes
/// open and its caller sees end-of-file when the command ends.
///
/// # Safety
/// Async-signal-safe.
unsafe fn quiet(dev_null: &CString, keep: libc::c_int) {
    // SAFETY: descriptor operations on this process's own table.
    unsafe {
        let null = libc::open(dev_null.as_ptr(), libc::O_RDWR);
        if null >= 0 {
            for fd in 0..3 {
                libc::dup2(null, fd);
            }
            if null > 2 {
                libc::close(null);
            }
        }
        for fd in 3..1024 {
            if fd != keep {
                libc::close(fd);
            }
        }
    }
}

/// # Safety
/// Async-signal-safe; never returns.
unsafe fn relay_loop(listener: libc::c_int, relay: &Relay) -> ! {
    // SAFETY: plain system calls; every path ends in `_exit`.
    unsafe {
        loop {
            let client = libc::accept4(
                listener,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                libc::SOCK_CLOEXEC,
            );
            if client < 0 {
                if *libc::__errno_location() == libc::EINTR {
                    continue;
                }
                libc::_exit(0);
            }
            let relay_pid = libc::getpid();
            match libc::fork() {
                0 => {
                    libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL);
                    if libc::getppid() != relay_pid {
                        libc::_exit(0);
                    }
                    libc::close(listener);
                    let upstream =
                        libc::socket(libc::AF_UNIX, libc::SOCK_STREAM | libc::SOCK_CLOEXEC, 0);
                    if upstream < 0
                        || libc::connect(
                            upstream,
                            (&relay.socket as *const libc::sockaddr_un).cast(),
                            relay.socket_len,
                        ) != 0
                    {
                        libc::_exit(1);
                    }
                    pump(client, upstream);
                    libc::_exit(0);
                }
                _ => {
                    libc::close(client);
                }
            }
        }
    }
}

/// Copies bytes both ways until both sides have closed.
///
/// # Safety
/// Async-signal-safe.
unsafe fn pump(a: libc::c_int, b: libc::c_int) {
    let mut buffer = [0u8; 16 * 1024];
    let mut open = [true, true];
    // SAFETY: reads and writes into a stack buffer this function owns.
    unsafe {
        while open[0] || open[1] {
            let mut fds = [
                libc::pollfd {
                    fd: if open[0] { a } else { -1 },
                    events: libc::POLLIN,
                    revents: 0,
                },
                libc::pollfd {
                    fd: if open[1] { b } else { -1 },
                    events: libc::POLLIN,
                    revents: 0,
                },
            ];
            if libc::poll(fds.as_mut_ptr(), 2, -1) < 0 {
                if *libc::__errno_location() == libc::EINTR {
                    continue;
                }
                return;
            }
            for (index, (from, to)) in [(a, b), (b, a)].into_iter().enumerate() {
                if !open[index] || fds[index].revents == 0 {
                    continue;
                }
                let read = libc::read(from, buffer.as_mut_ptr().cast(), buffer.len());
                if read <= 0 {
                    open[index] = false;
                    libc::shutdown(to, libc::SHUT_WR);
                    continue;
                }
                let mut sent = 0isize;
                while sent < read {
                    let wrote = libc::write(
                        to,
                        buffer.as_ptr().offset(sent).cast(),
                        (read - sent) as usize,
                    );
                    if wrote <= 0 {
                        return;
                    }
                    sent += wrote;
                }
            }
        }
    }
}

/// The paths a read grant covers so that everything is readable except
/// `excluded` and what lies beneath each.
///
/// Landlock's rules are additive and cannot subtract a path from a granted
/// directory, so "everything but the secrets" is spelled as its complement:
/// each directory on the way to an excluded path is walked, every sibling
/// off that way is granted whole, and the excluded path itself is not. A
/// directory on the way is returned in `listed` rather than granted, so its
/// entries can be listed and nothing beneath it is readable by that rule.
///
/// `children` lists a directory's entries; injected so the walk is testable
/// on any host. An entry that is a symbolic link is granted as itself,
/// which grants nothing beyond the link: its target is covered, or not, by
/// its own place in the walk.
pub(crate) fn complement(
    excluded: &[PathBuf],
    children: &dyn Fn(&Path) -> Vec<PathBuf>,
) -> Complement {
    let mut out = Complement::default();
    walk(Path::new("/"), excluded, children, &mut out);
    out.granted.sort();
    out.granted.dedup();
    out.listed.sort();
    out.listed.dedup();
    out
}

/// What [`complement`] grants: whole subtrees, and directories that may only
/// be listed.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct Complement {
    pub(crate) granted: Vec<PathBuf>,
    pub(crate) listed: Vec<PathBuf>,
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
pub(crate) fn entries(dir: &Path) -> Vec<PathBuf> {
    std::fs::read_dir(dir)
        .map(|entries| entries.flatten().map(|entry| entry.path()).collect())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tree(dir: &Path) -> Vec<PathBuf> {
        let names: &[&str] = match dir.to_str().unwrap() {
            "/" => &["/usr", "/home", "/proc", "/etc"],
            "/home" => &["/home/me", "/home/other"],
            "/home/me" => &["/home/me/.ssh", "/home/me/code", "/home/me/.bashrc"],
            _ => &[],
        };
        names.iter().map(PathBuf::from).collect()
    }

    /// Everything but the excluded paths is granted, and the directories on
    /// the way to them are only listed.
    #[test]
    fn the_complement_grants_around_what_is_excluded() {
        let excluded = [PathBuf::from("/home/me/.ssh"), PathBuf::from("/proc")];
        let c = complement(&excluded, &tree);
        let granted: Vec<&str> = c.granted.iter().map(|p| p.to_str().unwrap()).collect();
        assert_eq!(
            granted,
            [
                "/etc",
                "/home/me/.bashrc",
                "/home/me/code",
                "/home/other",
                "/usr"
            ]
        );
        let listed: Vec<&str> = c.listed.iter().map(|p| p.to_str().unwrap()).collect();
        assert_eq!(listed, ["/", "/home", "/home/me"]);
        assert!(!c.granted.iter().any(|p| p.starts_with("/proc")));
    }

    #[test]
    fn protected_paths_name_hooks_config_and_sternas_own() {
        let root = std::env::temp_dir().join(format!("sterna-ns-protect-{}", std::process::id()));
        std::fs::create_dir_all(root.join(".git/hooks")).unwrap();
        std::fs::write(root.join(".git/config"), "").unwrap();
        std::fs::create_dir_all(root.join(".sterna")).unwrap();
        let paths = protected_paths(std::slice::from_ref(&root));
        let _ = std::fs::remove_dir_all(&root);
        assert_eq!(
            paths,
            [
                root.join(".git/config"),
                root.join(".git/hooks"),
                root.join(".sterna")
            ]
        );
    }
}
