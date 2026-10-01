//! Setup without a terminal (`docs/engine.md`): a client sets the model,
//! the effort and the level, gives a key and signs in; every settings file
//! an earlier version wrote still loads; the desktop's own preferences come
//! back as they were left; and an account one install added is there for
//! the next. Plan goals 10, 11 and 12.

#[path = "support/engine.rs"]
mod engine;

use engine::{Conn, PATIENCE, Provider, World, ending};
#[cfg(unix)]
use serde_json::Value;
use serde_json::json;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// The person's global settings file in `world`.
fn global_settings(world: &World) -> PathBuf {
    world
        .base
        .join("global-config")
        .join("sterna")
        .join("config.toml")
}

/// A model, an effort and a level a client sets hold for the session and
/// are the person's settings from then on; the next request goes out on the
/// model it chose.
#[test]
fn the_model_the_effort_and_the_level_a_client_sets_hold_and_the_next_request_uses_them() {
    let provider = Provider::start(|_| ending("set"));
    let world = World::new("setup-controls", &provider);
    let folder = world.folder("project");
    let host = world.host();
    let started = host.ask(json!({"do":"start","root":folder.to_string_lossy()}));
    let mut client = Conn::session(&started, "desktop");
    client.attach();

    client.send(json!({"do":"control","line":"/model fixture-two"}));
    client.until(|e| e["kind"] == "facts" && e["facts"]["model"] == "fixture-two");
    client.send(json!({"do":"control","line":"/effort high"}));
    client.until(|e| e["kind"] == "facts" && e["facts"]["effort"] == "high");
    client.send(json!({"do":"set_level","level":"ask","save":true}));
    client.until(|e| e["kind"] == "facts" && e["facts"]["level"] == "ask");

    client.send(json!({"do":"submit","text":"one request, please"}));
    assert_eq!(
        client.until_turn_ends().last().unwrap()["activity"],
        "complete"
    );
    let requests = provider.requests();
    let last = requests.last().expect("the turn asked the model");
    assert_eq!(last.body["model"], "fixture-two", "{}", last.body);

    // Kept for the next session, where the person's settings are kept.
    let saved = host.ask(json!({"do":"settings"}));
    assert_eq!(saved["values"]["model.parent"], "fixture-two", "{saved}");
    assert_eq!(saved["values"]["sandbox.level"], "ask", "{saved}");
    host.ask(json!({"do":"set_setting","key":"session.effort","value":"high"}));
    let saved = host.ask(json!({"do":"settings"}));
    assert_eq!(saved["values"]["session.effort"], "high", "{saved}");
    // A value that is not one is refused, and nothing is saved.
    let mut conn = host.connect("desktop");
    let refused =
        conn.request(json!({"do":"set_setting","key":"sandbox.level","value":"wide open"}));
    assert!(refused.get("error").is_some(), "{refused}");
    let saved = host.ask(json!({"do":"settings"}));
    assert_eq!(saved["values"]["sandbox.level"], "ask", "{saved}");
}

/// A gateway that answers every control as an empty account list, records
/// the arguments it was run with, keeps what `credentials set` was handed on
/// stdin, and runs a sign-in that takes a pasted address.
#[cfg(unix)]
fn fake_gateway(world: &World) {
    use std::os::unix::fs::PermissionsExt;
    let base = world.base.display().to_string();
    let script = format!(
        r#"#!/bin/sh
echo "$@" >> '{base}/gateway-argv.txt'
case "$1 $2" in
  "credentials set")
    cat > "{base}/gateway-stdin-$$.txt"
    printf '%s\n' '{{"provider":"'"$3"'","variable":"OPENAI_API_KEY","stored_in":"credentials.toml","model_lists":[]}}'
    ;;
  "subscriptions connect")
    printf '%s\n' '{{"state":"opened","authorize_url":"https://example.invalid/sign-in"}}'
    read -r pasted
    printf '%s' "$pasted" > '{base}/gateway-pasted.txt'
    printf '%s\n' '{{"state":"connected","account":"me@example.com"}}'
    ;;
  *)
    printf '%s\n' '{{"version":1,"accounts":[]}}'
    ;;
esac
exit 0
"#
    );
    let path = world.base.join("no-gateway");
    std::fs::write(&path, script).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

/// Every file under `root`, `.sterna` folders and data folders included.
#[cfg(unix)]
fn every_file(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut folders = vec![root.to_path_buf()];
    while let Some(folder) = folders.pop() {
        for entry in std::fs::read_dir(&folder).into_iter().flatten().flatten() {
            let path = entry.path();
            if path.is_dir() {
                folders.push(path);
            } else {
                out.push(path);
            }
        }
    }
    out
}

/// A key given on the host and a key given in a session's own form reach
/// the gateway on its stdin, and nowhere else: not its arguments, not a
/// record, an event either client was sent, a log, or a model request.
#[cfg(unix)]
#[test]
fn a_key_from_a_client_reaches_the_gateway_on_its_stdin_and_nowhere_else() {
    const HOST_KEY: &str = "sk-fixture-goal-ten-host-0123456789abcdef"; // glasshouse:not-a-secret
    const FORM_KEY: &str = "sk-fixture-goal-ten-form-fedcba9876543210"; // glasshouse:not-a-secret
    let provider = Provider::start(|_| ending("done"));
    let world = World::new("setup-keys", &provider);
    fake_gateway(&world);
    let folder = world.folder("project");
    let host = world.host();
    host.ask(json!({"do":"set_key","provider":"openai","key":HOST_KEY}));

    let started = host.ask(json!({"do":"start","root":folder.to_string_lossy()}));
    let mut client = Conn::session(&started, "desktop");
    client.attach();
    let mut watcher = Conn::session(&started, "watcher");
    watcher.attach();
    let mut seen: Vec<Value> = Vec::new();

    client.send(json!({"do":"control","line":"/key openai"}));
    let asked = client.until(|e| e["kind"] == "prompt" && e["prompt"]["type"] == "form");
    let id = asked.last().unwrap()["prompt"]["id"].clone();
    seen.extend(asked);
    client.send(json!({"do":"answer","prompt":id,"answer":{"form":[FORM_KEY]}}));
    seen.extend(client.until(|e| e["kind"] == "settled" && e["id"] == id));
    client.send(json!({"do":"submit","text":"carry on"}));
    seen.extend(client.until_turn_ends());
    seen.extend(watcher.until(engine::turn_ended));

    // The gateway was handed both, on stdin.
    let handed: String = every_file(&world.base)
        .into_iter()
        .filter(|path| {
            path.file_name()
                .is_some_and(|name| name.to_string_lossy().starts_with("gateway-stdin-"))
        })
        .map(|path| std::fs::read_to_string(path).unwrap())
        .collect();
    assert!(
        handed.contains(HOST_KEY),
        "the host's key reached the gateway"
    );
    assert!(
        handed.contains(FORM_KEY),
        "the form's key reached the gateway"
    );

    // And nowhere else.
    let argv = std::fs::read_to_string(world.base.join("gateway-argv.txt")).unwrap();
    for key in [HOST_KEY, FORM_KEY] {
        assert!(
            !argv.contains(key),
            "a key was put on the gateway's command line"
        );
        for event in &seen {
            assert!(
                !event.to_string().contains(key),
                "a key was in an event: {event}"
            );
        }
        for file in every_file(&world.base) {
            let name = file.file_name().unwrap().to_string_lossy().into_owned();
            if name.starts_with("gateway-stdin-") {
                continue;
            }
            let bytes = std::fs::read(&file).unwrap_or_default();
            assert!(
                !String::from_utf8_lossy(&bytes).contains(key),
                "a key was written to {}",
                file.display()
            );
        }
        for request in provider.seen.lock().unwrap().iter() {
            assert!(
                !request.body.to_string().contains(key),
                "a key was in a model request"
            );
        }
    }
}

/// A sign-in runs from the host: its progress arrives on the connection
/// that asked for it, the address a browser ended on goes back to the
/// gateway, and the sign-in says it connected.
#[cfg(unix)]
#[test]
fn a_sign_in_runs_from_a_client_and_takes_the_pasted_address() {
    const ADDRESS: &str = "http://localhost:1455/auth/callback?code=fixture";
    let provider = Provider::start(|_| ending("done"));
    let world = World::new("setup-sign-in", &provider);
    fake_gateway(&world);
    let host = world.host();
    let mut conn = host.connect("desktop");
    let answer = conn.request(json!({"do":"sign_in","provider":"openai"}));
    assert!(answer.get("ok").is_some(), "{answer}");
    let opened = loop {
        let line = conn.recv(PATIENCE).expect("the sign-in says how it stands");
        if line["sign_in"]["state"] == "opened" {
            break line;
        }
    };
    assert_eq!(
        opened["sign_in"]["authorize_url"],
        "https://example.invalid/sign-in"
    );
    conn.send(json!({"paste":ADDRESS}));
    let done = loop {
        let line = conn.recv(PATIENCE).expect("the sign-in ends");
        if line.get("done").is_some() {
            break line;
        }
    };
    assert_eq!(done["done"]["connected"], true, "{done}");
    assert_eq!(
        std::fs::read_to_string(world.base.join("gateway-pasted.txt")).unwrap(),
        ADDRESS
    );
    let argv = std::fs::read_to_string(world.base.join("gateway-argv.txt")).unwrap();
    assert!(
        argv.lines()
            .any(|line| line.starts_with("subscriptions connect openai")
                && line.contains("--no-browser")),
        "the gateway was told not to open a browser: {argv}"
    );
}

/// Every settings file an earlier version wrote still loads: a host reads
/// it, a session starts on it and runs a task, and a second session after
/// the first has taken out what retired starts on it again.
#[test]
fn every_settings_file_an_earlier_version_wrote_still_loads() {
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("settings");
    let mut files: Vec<PathBuf> = std::fs::read_dir(&fixtures)
        .unwrap()
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "toml"))
        .collect();
    files.sort();
    assert!(files.len() >= 5, "the fixtures are there: {files:?}");
    for file in files {
        let provider = Provider::start(|_| ending("loaded"));
        let world = World::new("setup-old-settings", &provider);
        let settings = global_settings(&world);
        std::fs::create_dir_all(settings.parent().unwrap()).unwrap();
        std::fs::copy(&file, &settings).unwrap();
        let folder = world.folder("project");
        let host = world.host();
        host.ask(json!({"do":"settings"}));
        for round in ["first", "second"] {
            let started = host.ask(json!({"do":"start","root":folder.to_string_lossy()}));
            let mut client = Conn::session(&started, "desktop");
            let state = client.attach();
            assert!(
                state["facts"]["level"].is_string(),
                "{}: the {round} session reads its level: {state}",
                file.display()
            );
            client.send(json!({"do":"submit","text":"does it load?"}));
            assert_eq!(
                client.until_turn_ends().last().unwrap()["activity"],
                "complete",
                "{}: the {round} session ran its task",
                file.display()
            );
            client.send(json!({"do":"end"}));
        }
        host.shutdown();
    }
}

/// The desktop's own preferences come back exactly as they were left, after
/// the host has restarted.
#[test]
fn the_desktops_preferences_read_back_unchanged_after_the_host_restarts() {
    let provider = Provider::start(|_| ending("done"));
    let world = World::new("setup-preferences", &provider);
    let host = world.host();
    assert_eq!(host.ask(json!({"do":"preferences"})), json!({}));
    let preferences = json!({
        "theme": "hyacinth",
        "appearance": "dark",
        "motion": "calm",
        "card": {"bird": true, "so_far": false, "context": true},
        "notify": ["ask", "done"],
        "window": {"width": 1480.5, "title": "Sterna · ünïcode ✓"},
    });
    host.ask(json!({"do":"set_preferences","preferences":preferences}));
    host.shutdown();
    let host = world.host();
    assert_eq!(host.ask(json!({"do":"preferences"})), preferences);
}

/// The gateway binary `cargo test -p sterna` runs beside: built here when it
/// was not, so this file never depends on the order things were built in.
fn real_gateway() -> PathBuf {
    let name = format!("inference-gateway{}", std::env::consts::EXE_SUFFIX);
    let candidate = Path::new(env!("CARGO_BIN_EXE_sterna")).with_file_name(&name);
    if !candidate.exists() {
        let status = std::process::Command::new(env!("CARGO"))
            .args([
                "build",
                "-p",
                "inference-gateway",
                "--bin",
                "inference-gateway",
            ])
            .current_dir(Path::new(env!("CARGO_MANIFEST_DIR")).join("../.."))
            .status()
            .expect("cargo runs");
        assert!(status.success(), "the gateway builds");
    }
    candidate
}

/// An account one install of the gateway added is there for another
/// install -- a reinstall in a new folder, an app's bundled copy -- because
/// what it keeps lives in the person's data folder, not beside a binary.
#[test]
fn an_account_one_install_added_is_there_for_the_next() {
    const KEY: &str = "sk-fixture-goal-twelve-0123456789abcdef"; // glasshouse:not-a-secret
    let provider = Provider::start(|_| ending("done"));
    let world = World::new("setup-accounts", &provider);
    let gateway = real_gateway();
    let first = world
        .base
        .join("first-install")
        .join(gateway.file_name().unwrap());
    std::fs::create_dir_all(first.parent().unwrap()).unwrap();
    std::fs::copy(&gateway, &first).unwrap();
    // The host's own gateway is another copy, at the path the world names.
    let second = world
        .base
        .join(format!("no-gateway{}", std::env::consts::EXE_SUFFIX));
    std::fs::copy(&gateway, &second).unwrap();

    let mut command = std::process::Command::new(&first);
    world.environ(&mut command);
    let mut child = command
        .args(["credentials", "set", "openai", "--json"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    use std::io::Write;
    child
        .stdin
        .take()
        .unwrap()
        .write_all(KEY.as_bytes())
        .unwrap();
    assert!(
        child.wait().unwrap().success(),
        "the first install took the key"
    );

    let host = world.host();
    let deadline = Instant::now() + PATIENCE;
    loop {
        let accounts = host.ask(json!({"do":"accounts"}));
        if accounts["accounts"]
            .as_array()
            .is_some_and(|accounts| accounts.iter().any(|a| a["provider"] == "openai"))
        {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the second install never saw the first's account: {accounts}"
        );
        std::thread::sleep(Duration::from_millis(200));
    }
}
