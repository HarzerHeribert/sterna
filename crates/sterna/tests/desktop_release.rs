//! The desktop app installs, runs and updates into a new version the way
//! the terminal's binaries do (plan goal 17): `install.sh --desktop` against
//! a release served from a local directory places the app beside the
//! version it ships with, and an update installs the next release's app
//! without overwriting the one that may be running, leaving the old version
//! whole.
#![cfg(unix)]

use std::io::{BufRead as _, BufReader, Write as _};
use std::net::TcpListener;
use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
use std::path::{Path, PathBuf};
use std::process::Command;

use sha2::Digest as _;
use sterna::update::{self, Source};

fn scratch(label: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("sterna-desktop-{label}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir.canonicalize().unwrap()
}

fn executable(path: &Path, text: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

fn sha256(path: &Path) -> String {
    sha2::Sha256::digest(std::fs::read(path).unwrap())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn tar(dir: &Path, archive: &Path, entries: &[&str]) {
    let status = Command::new("tar")
        .arg("czf")
        .arg(archive)
        .arg("-C")
        .arg(dir)
        .args(entries)
        .status()
        .unwrap();
    assert!(status.success());
}

/// Serves `<dir>/<path>` for every GET until the process ends.
fn serve(dir: PathBuf) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut line = String::new();
            let _ = reader.read_line(&mut line);
            loop {
                let mut header = String::new();
                if reader.read_line(&mut header).unwrap_or(0) <= 2 {
                    break;
                }
            }
            let path = line.split_whitespace().nth(1).unwrap_or("/");
            let path = path
                .split('?')
                .next()
                .unwrap_or(path)
                .trim_start_matches('/');
            let mut stream = stream;
            match std::fs::read(dir.join(path)) {
                Ok(body) => {
                    let _ = write!(
                        stream,
                        "HTTP/1.1 200 OK\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
                        body.len()
                    );
                    let _ = stream.write_all(&body);
                }
                Err(_) => {
                    let _ = write!(
                        stream,
                        "HTTP/1.1 404 Not Found\r\ncontent-length: 0\r\nconnection: close\r\n\r\n"
                    );
                }
            }
        }
    });
    base
}

/// The app as a release carries it, for this platform: what the person
/// opens, and the file whose output says which version it is.
fn app_entries() -> (&'static [&'static str], &'static str) {
    if cfg!(target_os = "macos") {
        (&["Sterna.app"], "Sterna.app/Contents/MacOS/Sterna")
    } else {
        (&["Sterna.AppImage", "sterna.png"], "Sterna.AppImage")
    }
}

/// A release `tag` under `<site>/dl/<tag>/`: the terminal's archive and the
/// desktop app's, both in its `SHA256SUMS`.
fn publish(site: &Path, tag: &str, target: &str) {
    let version = tag.trim_start_matches('v');
    let name = format!("sterna-{version}-{target}");
    let stage = site.join("stage").join(tag);
    executable(
        &stage.join(&name).join("sterna"),
        "#!/bin/sh\necho sterna\n",
    );
    executable(&stage.join(&name).join("inference-gateway"), "#!/bin/sh\n");

    let app = stage.join("desktop");
    let (entries, binary) = app_entries();
    executable(
        &app.join(binary),
        &format!("#!/bin/sh\necho \"Sterna {version}\"\n"),
    );
    if cfg!(target_os = "macos") {
        std::fs::write(
            app.join("Sterna.app/Contents/Info.plist"),
            format!("<plist><dict><key>CFBundleShortVersionString</key><string>{version}</string></dict></plist>\n"),
        )
        .unwrap();
    } else {
        std::fs::write(app.join("sterna.png"), b"\x89PNG fixture").unwrap();
    }

    let dl = site.join("dl").join(tag);
    std::fs::create_dir_all(&dl).unwrap();
    let archive = dl.join(format!("{name}.tar.gz"));
    tar(&stage, &archive, &[&name]);
    let desktop = format!("sterna-desktop-{version}-{target}.tar.gz");
    tar(&app, &dl.join(&desktop), entries);
    std::fs::write(
        dl.join("SHA256SUMS"),
        format!(
            "{}  {name}.tar.gz\n{}  {desktop}\n",
            sha256(&archive),
            sha256(&dl.join(&desktop))
        ),
    )
    .unwrap();
    std::fs::write(site.join("api"), format!("[{{\"tag_name\":\"{tag}\"}}]")).unwrap();
}

/// What the placed app says it is, run as the person's system runs it.
fn runs(app: &Path) -> String {
    let output = Command::new(app).output().unwrap();
    assert!(output.status.success(), "{} runs", app.display());
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

#[test]
fn the_desktop_app_installs_runs_and_updates_into_a_new_version_beside_the_old() {
    let Some(target) = update::target() else {
        return;
    };
    if !matches!(target, "aarch64-apple-darwin" | "x86_64-unknown-linux-gnu") {
        // The desktop app ships for macOS and Linux first.
        return;
    }
    let site = scratch("site");
    let home = scratch("home");
    let root = home.join("lib/sterna");
    let apps = home.join("Applications");
    let data = home.join("share");
    let base = serve(site.clone());

    publish(&site, "v0.9.0", target);
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../sites/public/install.sh");
    let installed = Command::new("sh")
        .arg(&script)
        .arg("--desktop")
        .env("STERNA_RELEASES_API", format!("{base}/api"))
        .env("STERNA_RELEASE_DOWNLOADS", format!("{base}/dl"))
        .env("STERNA_HOME", &root)
        .env("STERNA_BIN_DIR", home.join("bin"))
        .env("STERNA_APPLICATIONS", &apps)
        .env("XDG_DATA_HOME", &data)
        .env("HOME", &home)
        .output()
        .unwrap();
    assert!(
        installed.status.success(),
        "install.sh --desktop: {}",
        String::from_utf8_lossy(&installed.stderr)
    );

    // The app is where the person opens it, and runs.
    let opened = if cfg!(target_os = "macos") {
        apps.join("Sterna.app/Contents/MacOS/Sterna")
    } else {
        root.join("current/Sterna.AppImage")
    };
    assert_eq!(runs(&opened), "Sterna 0.9.0");
    if cfg!(target_os = "linux") {
        let entry = std::fs::read_to_string(data.join("applications/sterna.desktop")).unwrap();
        assert!(
            entry.contains(&format!(
                "Exec={}",
                root.join("current/Sterna.AppImage").display()
            )),
            "{entry}"
        );
    }
    if cfg!(target_os = "macos") {
        // The app's own sessions find this install through the copy it names.
        assert_eq!(
            std::fs::read_to_string(root.join("desktop"))
                .unwrap()
                .trim(),
            apps.join("Sterna.app").display().to_string()
        );
    }
    let first_inode = std::fs::metadata(&opened).unwrap().ino();

    // The next release: the app is updated with the terminal's binaries.
    publish(&site, "v0.10.0", target);
    let source = Source {
        releases_api: format!("{base}/api"),
        downloads: format!("{base}/dl"),
        broker_downloads: None,
        applications: Some(apps.clone()),
        launchers: Some(data.join("applications")),
    };
    update::install_release(&root, "v0.10.0", target, &source).unwrap();
    assert_eq!(runs(&opened), "Sterna 0.10.0");
    assert_ne!(
        std::fs::metadata(&opened).unwrap().ino(),
        first_inode,
        "the running app was replaced, never written over"
    );
    // The version it updated from is whole.
    let kept = root.join("versions/v0.9.0").join(app_entries().1);
    assert_eq!(runs(&kept), "Sterna 0.9.0");
    assert_eq!(
        std::fs::read_link(root.join("current")).unwrap(),
        root.join("versions/v0.10.0")
    );
}
