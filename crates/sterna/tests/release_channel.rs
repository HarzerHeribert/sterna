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

use serde_json::json;
use sha2::Digest as _;
use sterna::update::{self, Source};

#[path = "support/engine.rs"]
mod engine;

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

/// The host's `update` command, which the desktop app sends when it opens:
/// a copy run from a build tree says why it does not update itself, and an
/// install one release behind is told so and moved to the newest release
/// beside the version it runs from.
#[test]
fn the_host_tells_an_install_a_newer_release_is_out_and_moves_it_there() {
    let Some(target) = update::target() else {
        return;
    };
    let provider = engine::Provider::start(|_| engine::ending("unused"));
    let world = engine::World::new("host-update", &provider);
    let check = json!({"do":"update","check":true});

    let host = world.host();
    let fixed = host.ask(check.clone());
    assert_eq!(fixed["updates"], false, "{fixed}");
    assert!(
        fixed["why"].as_str().is_some_and(|why| !why.is_empty()),
        "{fixed}"
    );
    host.shutdown();

    // This build placed as v0.9.0 of an install, and v0.10.0 published.
    let root = world.base.join("install");
    let running = root.join("versions/v0.9.0/bin");
    std::fs::create_dir_all(&running).unwrap();
    std::fs::copy(env!("CARGO_BIN_EXE_sterna"), running.join("sterna")).unwrap();
    std::os::unix::fs::symlink(root.join("versions/v0.9.0"), root.join("current")).unwrap();
    let site = scratch("host-site");
    publish(&site, "v0.10.0", target, &site.join("adopted.log"), false);
    std::fs::write(
        site.join("api"),
        r#"[{"tag_name":"v0.9.0"},{"tag_name":"v0.10.0"}]"#,
    )
    .unwrap();
    let base = serve(site);
    let host = world.host_from(
        &running.join("sterna"),
        &[
            ("STERNA_UPDATE_API", format!("{base}/api")),
            ("STERNA_UPDATE_DOWNLOADS", format!("{base}/dl")),
            ("STERNA_UPDATE_BROKER_DOWNLOADS", format!("{base}/broker")),
        ],
    );

    let standing = host.ask(check.clone());
    assert_eq!(
        (
            &standing["installed"],
            &standing["latest"],
            &standing["available"],
            &standing["automatic"]
        ),
        (
            &json!("v0.9.0"),
            &json!("v0.10.0"),
            &json!(true),
            &json!(true)
        ),
        "{standing}"
    );
    let moved = host.ask(json!({"do":"update"}));
    assert_eq!(moved["installed"], "v0.10.0", "{moved}");
    assert_eq!(
        std::fs::read_link(root.join("current")).unwrap(),
        root.join("versions/v0.10.0")
    );
    assert!(
        running.join("sterna").is_file(),
        "the version the host runs from is kept whole"
    );
    // Automatic checks turned off: the app's own check on opening asks
    // nothing of the network, and a person's Check now still does.
    host.shutdown();
    let off = world.host_from(
        &running.join("sterna"),
        &[
            (
                "STERNA_UPDATE_API",
                "http://127.0.0.1:9/unreachable".to_string(),
            ),
            ("STERNA_DISABLE_AUTOUPDATE", "1".to_string()),
        ],
    );
    let quiet = off.ask(json!({"do":"update","check":true,"automatic":true}));
    assert_eq!(
        (
            &quiet["automatic"],
            &quiet["available"],
            quiet.get("latest")
        ),
        (&json!(false), &json!(false), None),
        "{quiet}"
    );
    let mut asked = off.connect("check");
    let refused = asked.request(json!({"do":"update","check":true}));
    assert!(
        refused["error"].is_string(),
        "Check now asks the network: {refused}"
    );
    off.shutdown();
    let host = world.host_from(
        &running.join("sterna"),
        &[
            ("STERNA_UPDATE_API", format!("{base}/api")),
            ("STERNA_UPDATE_DOWNLOADS", format!("{base}/dl")),
        ],
    );
    let after = host.ask(check);
    assert_eq!(
        (&after["installed"], &after["available"]),
        (&json!("v0.10.0"), &json!(false)),
        "{after}"
    );
    host.shutdown();
}
