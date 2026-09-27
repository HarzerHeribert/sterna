//! The wide sandbox on the Linux kernel: every file readable but the
//! secrets, writes in the writable places only, `.git/hooks` and `.sterna`
//! read-only inside them, and the network only through Sterna's proxy.
//!
//! The kernel test runs itself again with `HOME` pointing into a fixture, so
//! the secrets it plants and the home it refuses to write are its own and
//! never the machine's.
use std::path::{Path, PathBuf};

use sterna::sandbox::linux::{self, complement};

fn tree(dir: &Path) -> Vec<PathBuf> {
    let names: &[&str] = match dir.to_str().unwrap() {
        "/" => &["/usr", "/home", "/proc", "/etc"],
        "/home" => &["/home/me", "/home/other"],
        "/home/me" => &["/home/me/.ssh", "/home/me/code", "/home/me/.bashrc"],
        "/home/me/code" => &["/home/me/code/a", "/home/me/code/.env"],
        _ => &[],
    };
    names.iter().map(PathBuf::from).collect()
}

fn strings(paths: &[PathBuf]) -> Vec<&str> {
    paths.iter().map(|p| p.to_str().unwrap()).collect()
}

/// Everything but the excluded paths is granted, and the directories on the
/// way to them are only listed.
#[test]
fn the_complement_grants_around_what_is_excluded() {
    let excluded = [PathBuf::from("/home/me/.ssh"), PathBuf::from("/proc")];
    let c = complement(Path::new("/"), &excluded, &tree);
    assert_eq!(
        strings(&c.granted),
        [
            "/etc",
            "/home/me/.bashrc",
            "/home/me/code",
            "/home/other",
            "/usr"
        ]
    );
    assert_eq!(strings(&c.listed), ["/", "/home", "/home/me"]);
}

/// A start with nothing excluded beneath it is granted whole; a start inside
/// an excluded path grants nothing.
#[test]
fn the_complement_of_a_place_is_itself_or_nothing() {
    let excluded = [PathBuf::from("/home/me/.ssh")];
    let whole = complement(Path::new("/home/me/code"), &excluded, &tree);
    assert_eq!(strings(&whole.granted), ["/home/me/code"]);
    assert!(whole.listed.is_empty());
    let inside = complement(Path::new("/home/me/.ssh/keys"), &excluded, &tree);
    assert_eq!(inside, linux::Complement::default());
    let around = complement(
        Path::new("/home/me/code"),
        &[PathBuf::from("/home/me/code/.env")],
        &tree,
    );
    assert_eq!(strings(&around.granted), ["/home/me/code/a"]);
    assert_eq!(strings(&around.listed), ["/home/me/code"]);
}

// -- the kernel ----------------------------------------------------------

#[cfg(target_os = "linux")]
const INNER: &str = "STERNA_WIDE_SANDBOX_INNER";

/// The outer half: a fixture home and project, then this binary again with
/// `HOME` pointing at the fixture and the inner test selected.
#[test]
#[cfg(target_os = "linux")]
fn a_confined_command_reads_widely_and_writes_only_the_writable_places() {
    if std::env::var_os(INNER).is_some() {
        return inner();
    }
    if linux::regime() == linux::Regime::Unconfined {
        eprintln!("skipped: this kernel has no Landlock ABI 3");
        return;
    }
    let base = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("wide-sandbox-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let home = base.join("home");
    std::fs::create_dir_all(&home).unwrap();
    let state = std::env::temp_dir().join(format!("wide-sandbox-state-{}", std::process::id()));
    std::fs::create_dir_all(state.join("inference-gateway")).unwrap();
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "--nocapture",
            "a_confined_command_reads_widely_and_writes_only_the_writable_places",
        ])
        .env(INNER, "1")
        .env("HOME", std::fs::canonicalize(&home).unwrap())
        // The proxy under test connects directly; an outer proxy this
        // machine sits behind would be asked for `localhost` instead.
        .env_remove("HTTPS_PROXY")
        .env_remove("https_proxy")
        // Cargo names its own home for the processes it runs; the fixture's
        // `~/.cargo` is the toolchain home under test.
        .env_remove("CARGO_HOME")
        // The gateway's state -- a secret -- inside the temp folder, as CI's
        // keyring fixture puts it: the temp folder must stay writable.
        .env("XDG_STATE_HOME", &state)
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&base);
    let _ = std::fs::remove_dir_all(&state);
    assert!(
        output.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[cfg(target_os = "linux")]
fn inner() {
    use std::io::{Read, Write};
    use std::process::{Command, Stdio};
    use sterna::sandbox::profile::Profile;

    let home = PathBuf::from(std::env::var_os("HOME").unwrap());
    let root = home.join("code/project");
    for dir in [".git/hooks", ".sterna/scratch", ".claude"] {
        std::fs::create_dir_all(root.join(dir)).unwrap();
    }
    std::fs::write(root.join(".git/config"), "[core]\n").unwrap();
    std::fs::write(root.join(".sterna/config.toml"), "").unwrap();
    std::fs::create_dir_all(home.join(".ssh")).unwrap();
    std::fs::write(home.join(".ssh/id_test"), "SECRET-KEY").unwrap();
    std::fs::create_dir_all(home.join(".cargo/registry")).unwrap();
    std::fs::write(home.join(".cargo/credentials.toml"), "TOKEN").unwrap();
    std::fs::write(home.join("notes.txt"), "plain notes").unwrap();

    // A server on this machine's loopback, reachable only through the proxy.
    let server = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let server_port = server.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for stream in server.incoming().flatten() {
            let mut stream = stream;
            let _ = stream.write_all(b"hello from the host\n");
        }
    });
    let allowed = sterna::sandbox::proxy::Allowed::new(&[], &["localhost".to_string()]);
    let proxy = sterna::sandbox::proxy::Proxy::start(allowed).unwrap();
    let profile = Profile::compile(&root, None).with_proxy(sterna::sandbox::profile::ProxyRoute {
        port: proxy.port(),
        unix: proxy.unix_path().map(Path::to_path_buf),
        env: proxy.env(),
    });
    let namespaced = matches!(linux::regime(), linux::Regime::Namespaced { .. });

    let run = |line: &str| -> (bool, String) {
        let mut command = Command::new("/bin/bash");
        command
            .args(["-c", line])
            .current_dir(&root)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        assert!(linux::confine(&profile, &mut command).unwrap());
        let mut child = command.spawn().unwrap();
        let mut out = String::new();
        child
            .stdout
            .take()
            .unwrap()
            .read_to_string(&mut out)
            .unwrap();
        let status = child.wait().unwrap();
        (status.success(), out)
    };
    let p = |path: &Path| path.display().to_string();

    let (ok, out) = run(&format!("cat '{}'", p(&home.join("notes.txt"))));
    assert!(
        ok && out == "plain notes",
        "a plain home file is readable: {out}"
    );
    let (ok, _) = run("ls /usr/bin >/dev/null && /usr/bin/env true");
    assert!(ok, "system programs run");
    for secret in [
        home.join(".ssh/id_test"),
        home.join(".cargo/credentials.toml"),
    ] {
        // Refused by Landlock, or -- in the namespace -- covered by an empty
        // file: either way not one byte of it.
        let (_, out) = run(&format!("cat '{}'", p(&secret)));
        assert!(out.is_empty(), "{} was read: {out}", secret.display());
    }
    let (ok, _) = run(&format!("echo x > '{}'", p(&home.join("made"))));
    assert!(!ok, "the home folder was written");
    for place in [
        root.join("made"),
        home.join(".cargo/registry/made"),
        root.join(".sterna/scratch/made"),
    ] {
        let (ok, _) = run(&format!("echo x > '{}'", p(&place)));
        assert!(ok, "{} is a writable place", place.display());
    }
    let (ok, _) = run("t=$(mktemp) && echo x > \"$t\" && rm \"$t\"");
    assert!(ok, "the temp folder is writable");
    // The confiner's environment -- API keys among them -- stays unread:
    // the PID namespace hides it, and without one Landlock refuses a
    // sandboxed reader of an unsandboxed process -- except for root, whose
    // CAP_SYS_PTRACE passes that check, so as root only the namespace
    // protects it. (Measured: unprivileged, the read is refused with or
    // without a non-dumpable confiner.)
    let root_user = unsafe { libc::geteuid() } == 0;
    if namespaced || !root_user {
        let (ok, out) = run(&format!("cat /proc/{}/environ", std::process::id()));
        assert!(!ok && out.is_empty(), "the parent's environment was read");
    }

    if !namespaced {
        eprintln!("no user namespaces here: the protected paths and the proxy are not tested");
        return;
    }
    for protected in [
        root.join(".git/hooks/pre-commit"),
        root.join(".git/config"),
        root.join(".sterna/config.toml"),
        root.join(".claude/settings.json"),
    ] {
        let (ok, _) = run(&format!("echo x >> '{}'", p(&protected)));
        assert!(!ok, "{} was written", protected.display());
    }
    // Straight to the host's loopback: a different network namespace.
    let (ok, _) = run(&format!("exec 3<>/dev/tcp/127.0.0.1/{server_port}"));
    assert!(!ok, "the host's loopback was reached without the proxy");
    // Through the relay to the proxy, which tells an allowed host from a
    // refused one. (It tunnels to ports 443 and 80 only, which this test
    // cannot listen on; its own tests cover the tunnel.)
    let through = |host: &str| {
        run(&format!(
            "exec 3<>/dev/tcp/127.0.0.1/{port} && printf 'CONNECT {host}:{server_port} HTTP/1.1\\r\\nHost: {host}\\r\\n\\r\\n' >&3 && timeout 5 head -c 200 <&3",
            port = proxy.port()
        ))
    };
    let (_, out) = through("localhost");
    assert!(out.contains("ports 443 and 80 only"), "{out}");
    let (_, out) = through("example.com");
    assert!(
        out.contains("403") && !out.contains("ports 443 and 80"),
        "{out}"
    );
    assert!(proxy.refused().iter().any(|host| host == "example.com"));
    // A project that holds a secret -- here the home folder itself: a new
    // file lands directly in it, and the secret inside stays unreadable,
    // because the mounts cover it.
    let home_project = Profile::compile(&home, None);
    let run_home = |line: &str| -> (bool, String) {
        let mut command = Command::new("/bin/bash");
        command
            .args(["-c", line])
            .current_dir(&home)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        assert!(linux::confine(&home_project, &mut command).unwrap());
        let output = command.output().unwrap();
        (
            output.status.success(),
            String::from_utf8_lossy(&output.stdout).into_owned(),
        )
    };
    let (ok, _) = run_home("echo x > made-in-home");
    assert!(
        ok,
        "a file could not be created in a project that holds a secret"
    );
    let (_, out) = run_home("cat .ssh/id_test");
    assert!(
        out.is_empty(),
        "the secret inside the project was read: {out}"
    );
    // The relay holds nothing but its listener, however many descriptors
    // this process has open: one left behind -- `spawn`'s exec-status pipe
    // above 1024 -- kept the spawning thread waiting for ever.
    let held: Vec<std::fs::File> = (0..1100)
        .map(|_| std::fs::File::open("/dev/null").unwrap())
        .collect();
    let (sent, spawned) = std::sync::mpsc::channel();
    let profile_for_spawn = profile.clone();
    let root_for_spawn = root.clone();
    std::thread::spawn(move || {
        let mut command = Command::new("/bin/sleep");
        command
            .arg("30")
            .current_dir(&root_for_spawn)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        assert!(linux::confine(&profile_for_spawn, &mut command).unwrap());
        let _ = sent.send(command.spawn().unwrap());
    });
    let mut sleeper = spawned
        .recv_timeout(std::time::Duration::from_secs(10))
        .expect("the confined spawn never returned");
    drop(held);
    let children =
        std::fs::read_to_string(format!("/proc/{0}/task/{0}/children", sleeper.id())).unwrap();
    let relay = children.split_whitespace().next().expect("the relay runs");
    let fds = std::fs::read_dir(format!("/proc/{relay}/fd"))
        .unwrap()
        .count();
    assert_eq!(fds, 4, "the relay holds more than stdio and its listener");
    let _ = sleeper.kill();
    let _ = sleeper.wait();

    // A Unix socket on disk would reach out of the namespace; refused.
    let (ok, _) =
        run("python3 -c 'import socket; socket.socket(socket.AF_UNIX)' 2>/dev/null || exit 1");
    assert!(!ok, "a Unix socket was created");
}
