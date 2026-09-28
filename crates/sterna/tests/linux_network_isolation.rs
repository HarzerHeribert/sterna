use sterna::sandbox::linux::{
    Regime, SocketFilterInstruction, socket_deny_filter, unix_socket_deny_filter,
};

// Small classic-BPF interpreter exercises both supported ABI policies on every
// CI host, including the arch and x32 branches that cannot execute on macOS.
fn evaluate(filter: &[SocketFilterInstruction], arch: u32, syscall: u32) -> u32 {
    evaluate_with(filter, arch, syscall, 0)
}

/// As [`evaluate`], with the call's first argument (`seccomp_data.args[0]`,
/// its low word at offset 16).
fn evaluate_with(filter: &[SocketFilterInstruction], arch: u32, syscall: u32, arg0: u32) -> u32 {
    let mut accumulator = 0;
    let mut pc = 0;
    loop {
        let i = filter[pc];
        match i.code {
            0x20 => {
                accumulator = match i.k {
                    0 => syscall,
                    4 => arch,
                    16 => arg0,
                    _ => panic!("bad load"),
                }
            }
            0x15 => pc += if accumulator == i.k { i.jt } else { i.jf } as usize,
            0x35 => pc += if accumulator >= i.k { i.jt } else { i.jf } as usize,
            0x06 => return i.k,
            _ => panic!("bad opcode"),
        }
        pc += 1;
    }
}

#[test]
fn policy_denies_network_and_compat_bypass_but_allows_file_io() {
    for (name, arch, calls, ordinary) in [
        (
            "x86_64",
            0xc000003e,
            vec![
                41, 53, 42, 49, 50, 43, 288, 44, 46, 307, 47, 299, 425, 426, 427, 438,
            ],
            vec![0, 1, 2, 3, 59, 102],
        ),
        (
            "aarch64",
            0xc00000b7,
            vec![
                198, 199, 203, 200, 201, 202, 242, 206, 211, 269, 212, 243, 425, 426, 427, 438,
            ],
            vec![63, 64, 56, 57, 221],
        ),
    ] {
        let filter = socket_deny_filter(name).unwrap();
        for syscall in calls {
            assert_eq!(
                evaluate(&filter, arch, syscall),
                0x00050001,
                "{name} {syscall}"
            );
        }
        for syscall in ordinary {
            assert_eq!(
                evaluate(&filter, arch, syscall),
                0x7fff0000,
                "{name} {syscall}"
            );
        }
        // i386 socketcall and ARM compat socketcall cannot enter the native map.
        assert_eq!(evaluate(&filter, 0x40000003, 102), 0x80000000);
        assert_eq!(evaluate(&filter, 0x40000028, 102), 0x80000000);
        assert_eq!(evaluate(&filter, arch, 0x40000029), 0x80000000);
    }
    assert!(socket_deny_filter("riscv64").is_none());
    assert!(!Regime::LandlockAndSeccomp { abi: 3 }.reaches_network());
    assert!(
        Regime::LandlockAndSeccomp { abi: 3 }
            .describe()
            .contains("seccomp")
    );
}

/// Inside the network namespace internet sockets are allowed -- they reach
/// only loopback and the relay -- and a Unix socket is refused, because a
/// socket file on disk reaches out of the namespace. `socketpair` reaches
/// only itself.
#[test]
fn the_namespaced_policy_refuses_unix_sockets_and_allows_the_rest() {
    const AF_UNIX: u32 = 1;
    const AF_INET: u32 = 2;
    const AF_INET6: u32 = 10;
    for (name, arch, socket, socketpair, connect) in [
        ("x86_64", 0xc000003e, 41, 53, 42),
        ("aarch64", 0xc00000b7, 198, 199, 203),
    ] {
        let filter = unix_socket_deny_filter(name).unwrap();
        assert_eq!(
            evaluate_with(&filter, arch, socket, AF_UNIX),
            0x00050001,
            "{name}"
        );
        for family in [AF_INET, AF_INET6] {
            assert_eq!(
                evaluate_with(&filter, arch, socket, family),
                0x7fff0000,
                "{name}"
            );
        }
        for call in [socketpair, connect] {
            assert_eq!(
                evaluate_with(&filter, arch, call, AF_UNIX),
                0x7fff0000,
                "{name} {call}"
            );
        }
        for call in [425, 426, 427, 438] {
            assert_eq!(evaluate(&filter, arch, call), 0x00050001, "{name} {call}");
        }
        assert_eq!(evaluate(&filter, 0x40000003, 102), 0x80000000);
    }
    assert!(unix_socket_deny_filter("riscv64").is_none());
}

#[cfg(target_os = "linux")]
#[test]
fn confined_socket_probe() {
    let Some(regime) = std::env::var_os("STERNA_TEST_SOCKET_PROBE") else {
        return;
    };
    let namespaced = regime == "namespaced";
    let opened = |family| {
        // SAFETY: socket has no pointer arguments; the descriptor, if any,
        // is closed before this returns.
        let fd = unsafe { libc::socket(family, libc::SOCK_STREAM, 0) };
        if fd >= 0 {
            unsafe { libc::close(fd) };
            return Ok(());
        }
        Err(std::io::Error::last_os_error().raw_os_error())
    };
    for family in [libc::AF_INET, libc::AF_INET6] {
        if namespaced {
            // Not refused by the filter; a kernel without IPv6 still
            // answers EAFNOSUPPORT for that family.
            assert_ne!(opened(family), Err(Some(libc::EPERM)), "{family}");
        } else {
            assert_eq!(opened(family), Err(Some(libc::EPERM)));
        }
    }
    assert_eq!(opened(libc::AF_UNIX), Err(Some(libc::EPERM)));
    let mut sockets = [-1; 2];
    // SAFETY: sockets points to two writable descriptors.
    let paired =
        unsafe { libc::socketpair(libc::AF_UNIX, libc::SOCK_STREAM, 0, sockets.as_mut_ptr()) };
    assert_eq!(paired == 0, namespaced);
    // SAFETY: forked child performs only socket/_exit, with no allocator access.
    let pid = unsafe { libc::fork() };
    assert!(pid >= 0);
    if pid == 0 {
        let denied = unsafe { libc::socket(libc::AF_UNIX, libc::SOCK_STREAM, 0) } == -1;
        unsafe { libc::_exit(if denied { 0 } else { 1 }) };
    }
    let mut status = 0;
    // SAFETY: valid child PID and writable status pointer.
    assert_eq!(unsafe { libc::waitpid(pid, &mut status, 0) }, pid);
    assert_eq!(status, 0);
}

#[cfg(target_os = "linux")]
#[test]
fn actual_confined_process_and_descendants_get_the_regimes_sockets() {
    use std::process::Command;
    use sterna::sandbox::{linux, profile::Profile};
    let regime = linux::regime();
    if regime == Regime::Unconfined {
        eprintln!("SKIP: requires Landlock ABI >=3 and supported seccomp architecture");
        return;
    }
    let root = std::env::current_dir().unwrap();
    let profile = Profile::compile(&root, None);
    let binary = std::env::current_exe().unwrap();
    let mut command = Command::new(&binary);
    command
        .args(["--exact", "confined_socket_probe", "--nocapture"])
        .env(
            "STERNA_TEST_SOCKET_PROBE",
            if regime.reaches_network() {
                "namespaced"
            } else {
                "isolated"
            },
        );
    assert!(linux::confine(&profile, &mut command).unwrap());
    let output = command.output().unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let mut command = Command::new("/bin/sh");
    command.args(["-c", "printf 'pipes still work' | cat"]);
    assert!(linux::confine(&profile, &mut command).unwrap());
    let output = command.output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(output.stdout, b"pipes still work");
}
