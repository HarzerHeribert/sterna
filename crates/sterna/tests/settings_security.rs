//! Regression coverage for project-local settings protection and the active
//! Linux Landlock limitation. These assertions stay separate from the general
//! sandbox suite so adding sterna-native settings cannot accidentally inherit
//! coverage that mentions only another harness's directory.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use sterna::sandbox::profile::{Access, Profile};
use sterna::sandbox::windows;

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
fn sterna_settings_are_carved_out_of_every_project_write_grant() {
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

    // Every OS layer is handed `.sterna` as a path that stays read-only
    // inside the writable project.
    assert!(profile.protected_paths().contains(&root.join(".sterna")));

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
    macos::confine(&profile, &mut command).unwrap();
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

/// In the namespaced regime `.sterna` is a read-only mount, so an admitted
/// shell cannot rewrite the settings a future session reads. Without user
/// namespaces Landlock's additive rules cannot carve it out, and the regime
/// says so.
#[cfg(target_os = "linux")]
#[test]
fn linux_refuses_an_admitted_shell_sterna_settings_writes_where_it_can_and_says_where_not() {
    use std::process::{Command, Stdio};
    use sterna::sandbox::linux;

    let regime = linux::regime();
    if regime == linux::Regime::Unconfined {
        eprintln!("skipped: this kernel has no Landlock ABI 3");
        return;
    }
    let fixture = Fixture::new();
    let profile = fixture.profile();
    let config = profile.root().join(".sterna/config.toml");
    assert!(profile.check("write", Access::Write, &config).is_err());

    let mut command = Command::new("/bin/bash");
    command
        .arg("-c")
        .arg("printf compromised > .sterna/config.toml")
        .current_dir(profile.root())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    assert!(linux::confine(&profile, &mut command).unwrap());
    let output = command.output().expect("confined bash starts");
    match regime {
        linux::Regime::Namespaced { .. } => {
            assert!(!output.status.success(), "{output:?}");
            assert_eq!(
                std::fs::read_to_string(config).unwrap(),
                "model = \"fixture\"\n"
            );
        }
        _ => {
            let said = regime.describe();
            assert!(said.contains(".sterna"), "{said}");
            assert!(said.contains("Sterna's own checks only"), "{said}");
        }
    }
}
