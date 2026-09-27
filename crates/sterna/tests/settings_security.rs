//! Regression coverage for project-local settings protection and the active
//! Linux Landlock limitation. These assertions stay separate from the general
//! sandbox suite so adding sterna-native settings cannot accidentally inherit
//! coverage that mentions only another harness's directory.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use sterna::sandbox::profile::{Access, Profile};
use sterna::sandbox::{linux, windows};

static COUNTER: AtomicU64 = AtomicU64::new(0);

struct Fixture(PathBuf);

impl Fixture {
    fn new() -> Self {
        let sequence = COUNTER.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "sterna-settings-security-{}-{sequence}",
            std::process::id(),
        ));
        std::fs::create_dir_all(root.join(".claude")).unwrap();
        std::fs::create_dir_all(root.join(".sterna")).unwrap();
        std::fs::write(root.join(".sterna/config.toml"), "model = \"fixture\"\n").unwrap();
        Self(root)
    }

    fn profile(&self) -> Profile {
        let root = self.0.to_string_lossy().replace('\\', "/");
        Profile::compile(
            &self.0,
            Some(&format!(
                r#"{{"permissions":{{"allow":["Read({root}/**)","Edit({root}/**)","Bash(*)"]}}}}"#
            )),
        )
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn sterna_settings_are_carved_out_of_linux_and_windows_project_write_grants() {
    let fixture = Fixture::new();
    let profile = fixture.profile();
    let root = profile.root();

    assert!(
        profile
            .check("write", Access::Write, &root.join("ordinary.txt"))
            .is_ok()
    );
    assert!(
        profile
            .check("write", Access::Write, &root.join(".sterna/config.toml"))
            .is_err()
    );

    let argv: Vec<String> = linux::bwrap_argv(&profile, "/bin/sh".as_ref(), &[])
        .into_iter()
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect();
    let project = root.to_string_lossy();
    let sterna = root.join(".sterna").to_string_lossy().into_owned();
    let bind_at = |flag: &str, path: &str| {
        argv.windows(3)
            .position(|words| words[0] == flag && words[1] == path && words[2] == path)
    };
    let project_at = bind_at("--bind", &project).expect("writable project bind");
    let sterna_at = bind_at("--ro-bind", &sterna).expect("read-only .sterna bind");
    assert!(
        project_at < sterna_at,
        "the later bind must narrow the project view: {argv:?}"
    );

    let grants = windows::acl_grants(&profile, Path::new(r"C:\Windows\System32\cmd.exe"));
    assert!(
        grants.read_write.contains(&root.to_path_buf()),
        "{grants:?}"
    );
    assert!(
        grants.read_only.contains(&root.join(".sterna")),
        "{grants:?}"
    );
}

#[cfg(target_os = "macos")]
#[test]
fn macos_refuses_sterna_settings_writes_by_tools_and_admitted_shells() {
    use std::process::{Command, Stdio};
    use sterna::sandbox::macos;

    let fixture = Fixture::new();
    let profile = fixture.profile();
    let config = profile.root().join(".sterna/config.toml");

    // The tool boundary refuses the operation before any process is started.
    assert!(profile.check("write", Access::Write, &config).is_err());

    // Bash(*) deliberately admits an arbitrary command line. The native OS
    // boundary must independently refuse the same write even if that check is
    // bypassed by a future caller.
    let shell = std::fs::canonicalize("/bin/bash").unwrap();
    let mut command = Command::new(&shell);
    command
        .arg("-c")
        .arg("printf compromised > .sterna/config.toml")
        .current_dir(profile.root())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    macos::confine(&profile, &shell, &mut command).unwrap();
    let output = command.output().expect("confined bash starts");
    assert!(
        !output.status.success(),
        "the admitted shell rewrote settings: {output:?}"
    );
    assert_eq!(
        std::fs::read_to_string(config).unwrap(),
        "model = \"fixture\"\n"
    );
}

#[cfg(target_os = "linux")]
#[test]
fn landlock_only_refuses_direct_sterna_writes_but_cannot_stop_an_admitted_shell() {
    use std::process::{Command, Stdio};

    if linux::landlock_abi() < 3 || !linux::seccomp_supported_arch() {
        eprintln!(
            "skipped: active Linux confinement requires Landlock ABI 3 and audited seccomp architecture"
        );
        return;
    }

    let fixture = Fixture::new();
    let profile = fixture.profile();
    let config = profile.root().join(".sterna/config.toml");

    // Sterna's direct tool boundary remains closed.
    assert!(profile.check("write", Access::Write, &config).is_err());

    // The active Linux spawn path installs Landlock plus seccomp, not the
    // bubblewrap mount view. Landlock grants are additive, so the writable
    // project rule also reaches `.sterna` for an arbitrary admitted process.
    let shell = std::fs::canonicalize("/bin/bash").unwrap();
    let mut command = Command::new(&shell);
    command
        .arg("-c")
        .arg("printf compromised > .sterna/config.toml")
        .current_dir(profile.root())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    assert!(linux::confine(&profile, &shell, &mut command).unwrap());
    let output = command.output().expect("confined bash starts");
    assert!(
        output.status.success(),
        "the documented Landlock limitation changed: {output:?}"
    );
    assert_eq!(std::fs::read_to_string(config).unwrap(), "compromised");

    let warning = linux::regime().describe();
    assert!(warning.contains("arbitrary admitted process"), "{warning}");
    assert!(warning.contains(".sterna/config.toml"), "{warning}");
    assert!(warning.contains("future session"), "{warning}");
}
