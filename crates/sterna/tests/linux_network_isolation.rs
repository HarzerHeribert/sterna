use sterna::sandbox::linux::{Regime, SocketFilterInstruction, socket_deny_filter};

// Small classic-BPF interpreter exercises both supported ABI policies on every
// CI host, including the arch and x32 branches that cannot execute on macOS.
fn evaluate(filter: &[SocketFilterInstruction], arch: u32, syscall: u32) -> u32 {
    let mut accumulator = 0;
    let mut pc = 0;
    loop {
        let i = filter[pc];
        match i.code {
            0x20 => {
                accumulator = match i.k {
                    0 => syscall,
                    4 => arch,
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
    assert!(Regime::LandlockAndSeccomp { abi: 3 }.removes_network());
    assert!(
        Regime::LandlockAndSeccomp { abi: 3 }
            .describe()
            .contains("seccomp")
    );
}

#[cfg(target_os = "linux")]
#[test]
fn confined_socket_probe() {
    if std::env::var_os("STERNA_TEST_SOCKET_PROBE").is_none() {
        return;
    }
    for family in [libc::AF_INET, libc::AF_INET6, libc::AF_UNIX] {
        // SAFETY: socket has no pointer arguments and returns no fd on denial.
        assert_eq!(unsafe { libc::socket(family, libc::SOCK_STREAM, 0) }, -1);
        assert_eq!(
            std::io::Error::last_os_error().raw_os_error(),
            Some(libc::EPERM)
        );
    }
    let mut sockets = [-1; 2];
    // SAFETY: sockets points to two writable descriptors.
    assert_eq!(
        unsafe { libc::socketpair(libc::AF_UNIX, libc::SOCK_STREAM, 0, sockets.as_mut_ptr()) },
        -1
    );
    assert_eq!(
        std::io::Error::last_os_error().raw_os_error(),
        Some(libc::EPERM)
    );
    // SAFETY: forked child performs only socket/_exit, with no allocator access.
    let pid = unsafe { libc::fork() };
    assert!(pid >= 0);
    if pid == 0 {
        let denied = unsafe { libc::socket(libc::AF_INET, libc::SOCK_STREAM, 0) } == -1;
        unsafe { libc::_exit(if denied { 0 } else { 1 }) };
    }
    let mut status = 0;
    // SAFETY: valid child PID and writable status pointer.
    assert_eq!(unsafe { libc::waitpid(pid, &mut status, 0) }, pid);
    assert_eq!(status, 0);
}

#[cfg(target_os = "linux")]
#[test]
fn actual_confined_process_and_descendants_cannot_open_sockets() {
    use std::process::Command;
    use sterna::sandbox::{linux, profile::Profile};
    if linux::landlock_abi() < 3 || !linux::seccomp_supported_arch() {
        eprintln!("SKIP: requires Landlock ABI >=3 and supported seccomp architecture");
        return;
    }
    let root = std::env::current_dir().unwrap();
    let settings = serde_json::json!({"permissions":{"allow":["Bash", format!("Read({}/**)", root.display()), format!("Write({}/**)", root.display())]}}).to_string();
    let profile = Profile::compile(&root, Some(&settings));
    let binary = std::env::current_exe().unwrap();
    let mut command = Command::new(&binary);
    command
        .args(["--exact", "confined_socket_probe", "--nocapture"])
        .env("STERNA_TEST_SOCKET_PROBE", "1");
    assert!(linux::confine(&profile, &binary, &mut command).unwrap());
    let output = command.output().unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let shell = std::fs::canonicalize("/bin/sh").unwrap();
    let mut command = Command::new(&shell);
    command.args(["-c", "printf 'pipes still work' | cat"]);
    assert!(linux::confine(&profile, &shell, &mut command).unwrap());
    let output = command.output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(output.stdout, b"pipes still work");
}
