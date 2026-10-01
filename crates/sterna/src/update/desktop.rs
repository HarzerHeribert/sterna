//! The desktop app, installed beside the release it ships with and updated
//! with it. A person who installed it (`install.sh --desktop`, `install.ps1`
//! with `STERNA_DESKTOP=1`) has a `desktop` marker in the install root, and
//! an update then places the next release's app as well -- in the new
//! version's directory, and where the person opens it, never over the one
//! that may be running.

use std::fs;
use std::path::{Path, PathBuf};

use super::{Install, Source, download, sha256_of, unpack};

/// The file in the install root that says the app is wanted; on macOS it
/// names the copy the person opens.
pub const MARKER: &str = "desktop";

/// Whether the person installed the app.
pub(super) fn wanted(root: &Path) -> bool {
    root.join(MARKER).is_file()
}

/// The install the desktop app's own copy of the engine belongs to, which
/// its path does not say: on Linux the app runs from a mounted image whose
/// file `APPIMAGE` names, and on macOS it is the copy in the Applications
/// folder the default install's marker names. Its sessions keep the app up
/// to date as a terminal's keep the terminal.
pub(super) fn install_of(exe: &Path) -> Option<Install> {
    let image = std::env::var_os("APPIMAGE").map(PathBuf::from);
    let root = home().map(|home| home.join(".local/lib/sterna"));
    install_from(exe, image.as_deref(), root.as_deref())
}

fn install_from(exe: &Path, image: Option<&Path>, root: Option<&Path>) -> Option<Install> {
    if let Some(image) = image {
        // <root>/versions/<tag>/Sterna.AppImage
        let image = image.canonicalize().ok()?;
        let version = image.parent()?;
        let versions = version.parent()?;
        if versions.file_name()? != "versions" {
            return None;
        }
        return Some(Install {
            root: versions.parent()?.to_path_buf(),
            tag: version.file_name()?.to_str()?.to_string(),
        });
    }
    // <bundle>/Contents/MacOS/sterna
    let macos = exe.parent()?;
    if macos.file_name()? != "MacOS" {
        return None;
    }
    let bundle = macos.parent()?.parent()?;
    let root = root?;
    let named = fs::read_to_string(root.join(MARKER)).ok()?;
    if Path::new(named.trim()).canonicalize().ok()? != bundle {
        return None;
    }
    let current = fs::read_link(root.join("current")).ok()?;
    Some(Install {
        root: root.to_path_buf(),
        tag: current.file_name()?.to_str()?.to_string(),
    })
}

/// What a release carries of the app for `target`, at the top of its
/// archive, and the archive's name.
fn layout(version: &str, target: &str) -> (&'static [&'static str], String) {
    if target.contains("apple") {
        (
            &["Sterna.app"],
            format!("sterna-desktop-{version}-{target}.tar.gz"),
        )
    } else if target.contains("windows") {
        (
            &["desktop"],
            format!("sterna-desktop-{version}-{target}.zip"),
        )
    } else {
        (
            &["Sterna.AppImage", "sterna.png"],
            format!("sterna-desktop-{version}-{target}.tar.gz"),
        )
    }
}

/// Places `tag`'s app in `dest`, its version directory, and where the
/// person opens it. A release that carries no app for this machine changes
/// nothing and is no error: the terminal's update goes ahead.
pub(super) fn install(
    root: &Path,
    dest: &Path,
    tag: &str,
    target: &str,
    source: &Source,
) -> Result<(), String> {
    let version = tag.trim_start_matches('v');
    let (entries, archive) = layout(version, target);
    if !dest.join(entries[0]).exists() {
        let work = root.join(format!(".desktop-{}", std::process::id()));
        let _ = fs::remove_dir_all(&work);
        fs::create_dir_all(&work).map_err(|e| format!("{}: {e}", work.display()))?;
        let fetched = fetch(&work, tag, &archive, source);
        let placed = match fetched {
            // No app in this release for this machine.
            Ok(false) => {
                let _ = fs::remove_dir_all(&work);
                return Ok(());
            }
            Ok(true) => entries.iter().try_for_each(|entry| {
                fs::rename(work.join(entry), dest.join(entry))
                    .map_err(|e| format!("{}: {e}", dest.join(entry).display()))
            }),
            Err(error) => Err(error),
        };
        let _ = fs::remove_dir_all(&work);
        placed?;
    }
    open_from(root, dest, target, source)
}

/// Downloads and verifies the app's archive and unpacks it into `work`;
/// `false` when the release has none.
fn fetch(work: &Path, tag: &str, archive: &str, source: &Source) -> Result<bool, String> {
    let base = format!("{}/{tag}", source.downloads);
    download(&format!("{base}/SHA256SUMS"), &work.join("SHA256SUMS"))?;
    let sums = fs::read_to_string(work.join("SHA256SUMS")).map_err(|e| e.to_string())?;
    let Some(want) = sums.lines().find_map(|line| {
        let (sum, name) = line.split_once(char::is_whitespace)?;
        (name.trim() == archive).then(|| sum.to_string())
    }) else {
        return Ok(false);
    };
    download(&format!("{base}/{archive}"), &work.join(archive))?;
    if sha256_of(&work.join(archive))? != want {
        return Err(format!("{archive} does not match its SHA-256; refusing it"));
    }
    unpack(&work.join(archive), work)?;
    Ok(true)
}

/// Where the person opens the app from: `~/Applications` on macOS, which
/// takes a copy put in place by renames; a menu entry on Linux, which opens
/// whatever `current` points at; the Start menu's shortcut on Windows does
/// the same, and is the installer's.
fn open_from(root: &Path, dest: &Path, target: &str, source: &Source) -> Result<(), String> {
    if target.contains("apple") {
        let apps = source
            .applications
            .clone()
            .or_else(|| home().map(|home| home.join("Applications")))
            .ok_or("there is no Applications folder to put the app in")?;
        fs::create_dir_all(&apps).map_err(|e| format!("{}: {e}", apps.display()))?;
        let fresh = apps.join(format!(".Sterna.app.{}", std::process::id()));
        let _ = fs::remove_dir_all(&fresh);
        copy_bundle(&dest.join("Sterna.app"), &fresh)?;
        let placed = apps.join("Sterna.app");
        let gone = apps.join(format!(".Sterna.app.old.{}", std::process::id()));
        if placed.exists() {
            fs::rename(&placed, &gone).map_err(|e| format!("{}: {e}", placed.display()))?;
        }
        fs::rename(&fresh, &placed).map_err(|e| format!("{}: {e}", placed.display()))?;
        // A copy that may still be running: its files go, it keeps running.
        let _ = fs::remove_dir_all(&gone);
        let marker = root.join(MARKER);
        return fs::write(&marker, format!("{}\n", placed.display()))
            .map_err(|e| format!("{}: {e}", marker.display()));
    }
    if target.contains("linux") {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let image = dest.join("Sterna.AppImage");
            fs::set_permissions(&image, fs::Permissions::from_mode(0o755))
                .map_err(|e| format!("{}: {e}", image.display()))?;
        }
        let launchers = source
            .launchers
            .clone()
            .or_else(|| home().map(|home| home.join(".local/share/applications")))
            .ok_or("there is no menu folder to put the app in")?;
        fs::create_dir_all(&launchers).map_err(|e| format!("{}: {e}", launchers.display()))?;
        let current = root.join("current");
        let entry = format!(
            "[Desktop Entry]\nType=Application\nName=Sterna\nComment=Watch and answer your coding sessions\nExec={} %U\nIcon={}\nCategories=Development;\nTerminal=false\n",
            current.join("Sterna.AppImage").display(),
            current.join("sterna.png").display(),
        );
        let path = launchers.join("sterna.desktop");
        let fresh = launchers.join(format!(".sterna.desktop.{}", std::process::id()));
        fs::write(&fresh, entry).map_err(|e| format!("{}: {e}", fresh.display()))?;
        return fs::rename(&fresh, &path).map_err(|e| format!("{}: {e}", path.display()));
    }
    Ok(())
}

fn home() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
}

/// Copies an app bundle whole: its links stay links, its modes stay.
fn copy_bundle(from: &Path, to: &Path) -> Result<(), String> {
    let status = std::process::Command::new("cp")
        .arg("-R")
        .arg(from)
        .arg(to)
        .status()
        .map_err(|e| format!("cp: {e}"))?;
    status
        .success()
        .then_some(())
        .ok_or_else(|| format!("{} could not be copied", from.display()))
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    fn scratch(label: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "sterna-desktop-unit-{label}-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir.canonicalize().unwrap()
    }

    #[test]
    fn the_apps_own_engine_finds_the_install_it_came_with() {
        let base = scratch("of");
        let root = base.join("lib/sterna");
        let version = root.join("versions/v0.9.0");
        fs::create_dir_all(&version).unwrap();
        std::os::unix::fs::symlink(&version, root.join("current")).unwrap();
        let bundle = base.join("Applications/Sterna.app");
        let exe = bundle.join("Contents/MacOS/sterna");
        fs::create_dir_all(exe.parent().unwrap()).unwrap();
        fs::write(&exe, "").unwrap();
        let found = |root: &Path| install_from(&exe, None, Some(root));

        // Not installed as the app: nothing to update.
        assert_eq!(found(&root), None);
        // The marker names another copy: not this install's.
        fs::write(root.join(MARKER), format!("{}\n", base.display())).unwrap();
        assert_eq!(found(&root), None);
        fs::write(root.join(MARKER), format!("{}\n", bundle.display())).unwrap();
        let install = found(&root).unwrap();
        assert_eq!(
            (install.root.as_path(), install.tag.as_str()),
            (root.as_path(), "v0.9.0")
        );

        // A Linux image is placed in its version directory.
        let image = version.join("Sterna.AppImage");
        fs::write(&image, "").unwrap();
        let mounted = Path::new("/tmp/.mount_Sterna/usr/bin/sterna");
        let install = install_from(mounted, Some(&image), None).unwrap();
        assert_eq!(
            (install.root.as_path(), install.tag.as_str()),
            (root.as_path(), "v0.9.0")
        );
        assert_eq!(install_from(mounted, Some(&exe), None), None);
        let _ = fs::remove_dir_all(&base);
    }
}
