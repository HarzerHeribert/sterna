//! Opening the app again once a newer release is in place (`app_restart`).
//!
//! The engine's `update` places the newest release beside the running one,
//! and where the person installed the app, the app with it
//! (`crates/sterna/src/update/desktop.rs`): on macOS as the bundle the
//! person opens, at the same path; on Linux as
//! `<install root>/current/Sterna.AppImage`; on Windows as
//! `<install root>\current\desktop\Sterna.exe`. This opens that copy as soon
//! as this process has gone; the caller exits right after. The sessions keep
//! running in the host, and the new app finds them there.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// The copy to open, by the platform's rule: the bundle this app runs from
/// on macOS; the install's current AppImage on Linux, else the one running
/// (`$APPIMAGE`); the install's current app on Windows, else this one.
/// `None` when there is no such copy.
pub fn target(
    os: &str,
    exe: &Path,
    install_root: Option<&Path>,
    appimage: Option<&Path>,
) -> Option<PathBuf> {
    match os {
        "macos" => exe
            .ancestors()
            .find(|dir| dir.extension().is_some_and(|ext| ext == "app"))
            .map(Path::to_path_buf),
        "linux" => install_root
            .map(|root| root.join("current").join("Sterna.AppImage"))
            .filter(|image| image.is_file())
            .or_else(|| appimage.map(Path::to_path_buf)),
        "windows" => install_root
            .map(|root| root.join("current").join("desktop").join("Sterna.exe"))
            .filter(|app| app.is_file())
            .or_else(|| Some(exe.to_path_buf())),
        _ => None,
    }
}

/// Starts what opens `target` once this process has gone, detached from it.
pub fn open_after_exit(os: &str, target: &Path) -> std::io::Result<()> {
    launcher(os, target, std::process::id())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map(drop)
}

/// A shell that waits for `pid` to end -- ten seconds at most -- and then
/// opens `target`: with `open` on macOS, which starts a bundle as the Dock
/// does, and by running it elsewhere.
#[cfg(unix)]
fn launcher(os: &str, target: &Path, pid: u32) -> Command {
    let open = if os == "macos" {
        "exec /usr/bin/open \"$2\""
    } else {
        "exec \"$2\""
    };
    let mut command = Command::new("/bin/sh");
    command
        .arg("-c")
        .arg(format!(
            "i=0; while kill -0 \"$1\" 2>/dev/null && [ $i -lt 50 ]; do sleep 0.2; i=$((i+1)); done; {open}"
        ))
        .arg("sh")
        .arg(pid.to_string())
        .arg(target);
    command
}

/// The new app itself, in a process group and console of its own: Windows
/// lets two copies run for the moment this one takes to exit.
#[cfg(windows)]
fn launcher(_os: &str, target: &Path, _pid: u32) -> Command {
    use std::os::windows::process::CommandExt;
    const DETACHED_PROCESS: u32 = 0x0000_0008;
    const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
    let mut command = Command::new(target);
    command.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP);
    command
}

#[cfg(test)]
mod tests {
    use super::target;
    use crate::Scratch;
    use std::path::Path;

    #[test]
    fn macos_opens_the_bundle_it_runs_from() {
        let exe = Path::new("/Users/a/Applications/Sterna.app/Contents/MacOS/Sterna");
        assert_eq!(
            target(
                "macos",
                exe,
                Some(Path::new("/Users/a/.local/lib/sterna")),
                None
            )
            .as_deref(),
            Some(Path::new("/Users/a/Applications/Sterna.app"))
        );
        assert_eq!(
            target(
                "macos",
                Path::new("/usr/local/bin/sterna-desktop"),
                None,
                None
            ),
            None
        );
    }

    #[test]
    fn linux_opens_the_current_appimage_else_the_running_one() {
        let scratch = Scratch::new("restart-linux");
        let root = scratch.0.join("sterna");
        let running = Path::new("/home/a/Downloads/Sterna.AppImage");
        let exe = Path::new("/tmp/.mount_SternaX/usr/bin/sterna-desktop");
        assert_eq!(
            target("linux", exe, Some(&root), Some(running)).as_deref(),
            Some(running)
        );
        assert_eq!(target("linux", exe, Some(&root), None), None);
        let current = root.join("current/Sterna.AppImage");
        std::fs::create_dir_all(current.parent().unwrap()).unwrap();
        std::fs::write(&current, b"").unwrap();
        assert_eq!(
            target("linux", exe, Some(&root), Some(running)),
            Some(current)
        );
    }

    #[test]
    fn windows_opens_the_current_app_else_itself() {
        let scratch = Scratch::new("restart-windows");
        let root = scratch.0.join("sterna");
        let exe = Path::new("/Programs/sterna/versions/v0.1.0-pre.30/desktop/Sterna.exe");
        assert_eq!(
            target("windows", exe, Some(&root), None).as_deref(),
            Some(exe)
        );
        let current = root.join("current").join("desktop").join("Sterna.exe");
        std::fs::create_dir_all(current.parent().unwrap()).unwrap();
        std::fs::write(&current, b"").unwrap();
        assert_eq!(target("windows", exe, Some(&root), None), Some(current));
    }

    /// The launcher waits for the app to go before it opens the new copy.
    #[cfg(unix)]
    #[test]
    fn the_new_copy_opens_only_after_the_old_one_has_gone() {
        use std::os::unix::fs::PermissionsExt;
        use std::time::{Duration, Instant};
        let scratch = Scratch::new("restart-wait");
        let opened = scratch.0.join("opened");
        let app = scratch.0.join("Sterna.AppImage");
        std::fs::write(&app, format!("#!/bin/sh\ntouch '{}'\n", opened.display())).unwrap();
        std::fs::set_permissions(&app, std::fs::Permissions::from_mode(0o755)).unwrap();
        let mut old = std::process::Command::new("/bin/sleep")
            .arg("0.6")
            .spawn()
            .unwrap();
        let started = Instant::now();
        let mut opener = super::launcher("linux", &app, old.id()).spawn().unwrap();
        std::thread::sleep(Duration::from_millis(300));
        assert!(!opened.exists(), "opened while the old copy still ran");
        old.wait().unwrap();
        opener.wait().unwrap();
        assert!(opened.exists(), "never opened");
        assert!(started.elapsed() >= Duration::from_millis(500));
    }
}
