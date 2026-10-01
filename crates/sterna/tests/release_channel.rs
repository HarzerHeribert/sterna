//! `sterna update` against a release served from a local directory: the
//! archive is verified against `SHA256SUMS`, unpacked into a fresh version
//! directory, the pinned broker adopted through the new gateway, and
//! `current` repointed -- and a release whose archive does not match its sum
//! changes nothing.
#![cfg(unix)]

use std::io::{BufRead as _, BufReader, Write as _};
use std::net::TcpListener;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process::Command;

use sha2::Digest as _;
use sterna::update::{self, Source};

fn scratch(label: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("sterna-update-{label}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir.canonicalize().unwrap()
}

fn executable(path: &Path, text: &str) {
    std::fs::write(path, text).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

fn sha256(path: &Path) -> String {
    sha2::Sha256::digest(std::fs::read(path).unwrap())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn tar(dir: &Path, archive: &Path, entry: &str) {
    let status = Command::new("tar")
        .arg("czf")
        .arg(archive)
        .arg("-C")
        .arg(dir)
        .arg(entry)
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

/// A release `tag` under `<site>/dl/<tag>/`, with a broker pin whose asset
/// sits under `<site>/broker/v9.9.9/`. `tamper` lists a wrong sum.
fn publish(site: &Path, tag: &str, target: &str, adopt_log: &Path, tamper: bool) {
    let version = tag.trim_start_matches('v');
    let name = format!("sterna-{version}-{target}");
    let stage = site.join("stage").join(tag).join(&name);
    std::fs::create_dir_all(&stage).unwrap();
    executable(&stage.join("sterna"), "#!/bin/sh\necho sterna\n");
    executable(
        &stage.join("inference-gateway"),
        &format!(
            "#!/bin/sh\nprintf '%s\\n' \"$@\" >> \"{}\"\n",
            adopt_log.display()
        ),
    );

    let broker_stage = site.join("stage").join("broker");
    std::fs::create_dir_all(&broker_stage).unwrap();
    executable(&broker_stage.join("cli-proxy-api"), "#!/bin/sh\n");
    let broker_dir = site.join("broker").join("v9.9.9");
    std::fs::create_dir_all(&broker_dir).unwrap();
    let broker_asset = broker_dir.join("CLIProxyAPI_9.9.9_test.tar.gz");
    tar(&broker_stage, &broker_asset, "cli-proxy-api");
    std::fs::write(
        stage.join("cliproxyapi.toml"),
        format!(
            "repository = \"example/broker\"\nversion = \"9.9.9\"\n\n[assets]\n{target} = {{ name = \"CLIProxyAPI_9.9.9_test.tar.gz\", sha256 = \"{}\" }}\n",
            sha256(&broker_asset)
        ),
    )
    .unwrap();

    let dl = site.join("dl").join(tag);
    std::fs::create_dir_all(&dl).unwrap();
    let archive = dl.join(format!("{name}.tar.gz"));
    tar(stage.parent().unwrap(), &archive, &name);
    let sum = if tamper {
        "0".repeat(64)
    } else {
        sha256(&archive)
    };
    std::fs::write(dl.join("SHA256SUMS"), format!("{sum}  {name}.tar.gz\n")).unwrap();
    std::fs::write(site.join("api"), format!("[{{\"tag_name\":\"{tag}\"}}]")).unwrap();
}

#[test]
fn a_release_is_verified_installed_beside_the_running_one_and_its_broker_adopted() {
    let Some(target) = update::target() else {
        return;
    };
    let site = scratch("site");
    let root = scratch("root");
    let adopt_log = site.join("adopt.log");
    let base = serve(site.clone());
    let source = Source {
        releases_api: format!("{base}/api"),
        downloads: format!("{base}/dl"),
        broker_downloads: Some(format!("{base}/broker")),
        applications: None,
        launchers: None,
    };

    publish(&site, "v0.1.0-pre.9", target, &adopt_log, false);
    assert_eq!(update::latest_tag(&source).unwrap(), "v0.1.0-pre.9");
    let dest = update::install_release(&root, "v0.1.0-pre.9", target, &source).unwrap();

    assert!(dest.join("bin/sterna").is_file());
    assert_eq!(
        std::fs::read_link(root.join("current")).unwrap(),
        root.join("versions/v0.1.0-pre.9")
    );
    let adopted = std::fs::read_to_string(&adopt_log).unwrap();
    assert!(
        adopted.starts_with("subscriptions\nadopt-binary\n") && adopted.contains("cli-proxy-api"),
        "{adopted}"
    );
    assert_eq!(
        std::fs::read_to_string(root.join("broker-version")).unwrap(),
        "9.9.9"
    );

    // A release whose archive does not match its sum changes nothing.
    publish(&site, "v0.1.0-pre.10", target, &adopt_log, true);
    let refused = update::install_release(&root, "v0.1.0-pre.10", target, &source).unwrap_err();
    assert!(refused.contains("does not match its SHA-256"), "{refused}");
    assert!(!root.join("versions/v0.1.0-pre.10").exists());
    assert_eq!(
        std::fs::read_link(root.join("current")).unwrap(),
        root.join("versions/v0.1.0-pre.9")
    );
}

#[test]
fn the_newest_release_is_chosen_by_version_not_by_the_lists_order() {
    let site = scratch("order");
    std::fs::write(
        site.join("api"),
        r#"[{"tag_name":"v0.1.0-pre.9"},{"tag_name":"v0.1.0-pre.10"},{"tag_name":"v0.1.0-pre.1-204-gabc"}]"#,
    )
    .unwrap();
    let base = serve(site);
    let source = Source {
        releases_api: format!("{base}/api"),
        downloads: format!("{base}/dl"),
        broker_downloads: None,
        applications: None,
        launchers: None,
    };
    assert_eq!(update::latest_tag(&source).unwrap(), "v0.1.0-pre.10");
}
