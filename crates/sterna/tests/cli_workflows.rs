//! CLI routing and offline diagnostics do not require a provider or gateway.
#[path = "../src/cli_workflows.rs"]
#[allow(dead_code)]
mod cli_workflows;

use std::process::{Command, Stdio};

fn strings(args: &[&str]) -> Vec<String> {
    args.iter().map(|arg| (*arg).into()).collect()
}

#[test]
fn print_and_exec_preserve_task_boundaries_and_session_options() {
    for input in [
        strings(&["-p", "fix this\nthen test", "--model", "provider/model"]),
        strings(&["exec", "fix this\nthen test", "--model", "provider/model"]),
    ] {
        let args = cli_workflows::prepare(&input).unwrap().unwrap();
        let value = |key: &str| {
            args.iter()
                .position(|arg| arg == key)
                .map(|i| args[i + 1].as_str())
        };
        assert_eq!(value("--task"), Some("fix this\nthen test"));
        assert_eq!(value("--model"), Some("provider/model"));
        assert_eq!(value("--root"), Some("."));
    }
}

#[test]
fn history_and_continue_route_to_existing_session_implementation() {
    assert_eq!(
        cli_workflows::prepare(&strings(&["--continue"])).unwrap(),
        Some(strings(&["--resume=", "--root", "."]))
    );
    assert_eq!(
        cli_workflows::prepare(&strings(&["--resume", "session-id", "--root=/repo"])).unwrap(),
        Some(strings(&["--resume", "session-id", "--root=/repo"]))
    );
    assert_eq!(
        cli_workflows::prepare(&strings(&["--sessions"])).unwrap(),
        Some(strings(&["--sessions", "--root", "."]))
    );
    assert_eq!(cli_workflows::prepare(&strings(&["typo"])).unwrap(), None);
    assert!(cli_workflows::prepare(&strings(&["-p"])).is_err());
}

#[test]
fn exec_rejects_empty_stdin_instead_of_starting_an_interactive_session() {
    let root = std::env::temp_dir().join(format!(
        "sterna-cli-workflows-empty-stdin-{}",
        std::process::id()
    ));
    let output = Command::new(env!("CARGO_BIN_EXE_sterna"))
        .arg("exec")
        .stdin(Stdio::null())
        .env("XDG_CONFIG_HOME", root.join("global-config"))
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&output.stderr).contains("empty task"));
}

#[test]
fn doctor_json_reports_invalid_config_without_exposing_its_contents() {
    let root = std::env::temp_dir().join(format!(
        "sterna-doctor-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(root.join(".glasshouse")).unwrap();
    std::fs::write(
        root.join(".glasshouse/pane.toml"),
        "SECRET_SHOULD_NOT_APPEAR [ invalid",
    )
    .unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_sterna"))
        .args(["doctor", "--json", "--root"])
        .arg(&root)
        .env("ANTHROPIC_BASE_URL", "https://SECRET_ENDPOINT.invalid")
        .env("XDG_CONFIG_HOME", root.join("global-config"))
        .output()
        .unwrap();
    let stdout = String::from_utf8(output.stdout).unwrap();
    let report: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(report["schema_version"], 1);
    assert_eq!(report["ok"], false);
    assert!(
        report["checks"]
            .as_array()
            .unwrap()
            .iter()
            .any(|check| check["name"] == "config" && check["status"] == "error")
    );
    assert!(!stdout.contains("SECRET_"));
    assert!(root.join(".glasshouse/pane.toml").exists());
    std::fs::remove_dir_all(root).unwrap();
}

/// The trap a real session fell into on 2026-09-19: five `[helpers]` keys
/// set, saved without complaint, reported `ok` by `doctor`, and inert all
/// session — the only place the truth appeared was inside the system
/// prompt, which a person never reads.
#[test]
fn doctor_warns_about_a_feature_enabled_without_its_prerequisite() {
    let root = std::env::temp_dir().join(format!(
        "sterna-doctor-inert-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(root.join(".sterna")).unwrap();
    // Exactly the benchmark's configuration: helpers on, no helper model.
    std::fs::write(
        root.join(".sterna/config.toml"),
        "[model]\nparent = \"gpt-5.6-sol\"\n\n[helpers]\nenabled = true\npreflight = true\n",
    )
    .unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_sterna"))
        .args(["doctor", "--json", "--root"])
        .arg(&root)
        .env("XDG_CONFIG_HOME", root.join("global-config"))
        .output()
        .unwrap();
    let stdout = String::from_utf8(output.stdout).unwrap();
    let report: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    let settings = report["checks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|check| check["name"] == "settings")
        .expect("doctor reports a settings check");
    assert_eq!(settings["status"], "warning", "{stdout}");
    let detail = settings["detail"].as_str().unwrap();
    assert!(detail.contains("[helpers]"), "{detail}");
    assert!(
        detail.contains("helpers.model"),
        "the warning must name the key that fixes it: {detail}"
    );
    std::fs::remove_dir_all(root).unwrap();
}

/// The same check stays quiet when nothing is inert, so it is a signal
/// rather than a standing complaint.
#[test]
fn doctor_settings_check_is_ok_when_every_feature_has_what_it_needs() {
    let root = std::env::temp_dir().join(format!(
        "sterna-doctor-ok-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(root.join(".sterna")).unwrap();
    std::fs::write(
        root.join(".sterna/config.toml"),
        "[model]\nparent = \"gpt-5.6-sol\"\n",
    )
    .unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_sterna"))
        .args(["doctor", "--json", "--root"])
        .arg(&root)
        .env("XDG_CONFIG_HOME", root.join("global-config"))
        .output()
        .unwrap();
    let stdout = String::from_utf8(output.stdout).unwrap();
    let report: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    let settings = report["checks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|check| check["name"] == "settings")
        .expect("doctor reports a settings check");
    assert_eq!(settings["status"], "ok", "{stdout}");
    std::fs::remove_dir_all(root).unwrap();
}

/// **`sterna doctor` answers for the configuration in hand, not for the
/// build.** It said "Seatbelt backend compiled" until 2026-09-19 -- true on
/// every machine whatever anyone had configured -- and a person who had set
/// the rung and the grant read it as confirmation that they had full access.
#[test]
fn doctor_names_the_confinement_this_configuration_would_apply() {
    let root = std::env::temp_dir().join(format!(
        "sterna-doctor-confinement-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let global = root.join("global-config");
    std::fs::create_dir_all(global.join("sterna")).unwrap();
    std::fs::create_dir_all(root.join(".sterna")).unwrap();
    std::fs::write(
        root.join(".sterna/config.toml"),
        "[model]\nparent = \"gpt-5.6-sol\"\n",
    )
    .unwrap();
    let detail = |global_text: &str| {
        std::fs::write(global.join("sterna/config.toml"), global_text).unwrap();
        let output = Command::new(env!("CARGO_BIN_EXE_sterna"))
            .args(["doctor", "--json", "--root"])
            .arg(&root)
            .env("XDG_CONFIG_HOME", &global)
            .output()
            .unwrap();
        let stdout = String::from_utf8(output.stdout).unwrap();
        let report: serde_json::Value = serde_json::from_str(&stdout).unwrap();
        let check = report["checks"]
            .as_array()
            .unwrap()
            .iter()
            .find(|check| check["name"] == "sandbox")
            .expect("doctor reports a sandbox check")
            .clone();
        (
            check["status"].as_str().unwrap().to_string(),
            check["detail"].as_str().unwrap().to_string(),
        )
    };

    let (status, confined) = detail("");
    assert_eq!(status, "ok", "{confined}");
    assert!(
        confined.contains("confines every child this configuration spawns"),
        "the confined case answers for the configuration: {confined}"
    );

    let (status, open) = detail("[permissions]\nfull_access = true\n");
    assert_eq!(status, "warning", "{open}");
    assert!(
        open.contains("no OS confinement"),
        "the unconfined case says so plainly: {open}"
    );
    assert!(
        open.contains("full_access"),
        "and names the setting that chose it: {open}"
    );
    assert!(
        open.contains("never-grantable"),
        "and what still refuses: {open}"
    );
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn doctor_missing_root_is_a_structured_failure() {
    let root =
        std::env::temp_dir().join(format!("sterna-nonexistent-doctor-{}", std::process::id()));
    let output = Command::new(env!("CARGO_BIN_EXE_sterna"))
        .args(["doctor", "--json", "--root"])
        .env("XDG_CONFIG_HOME", root.join("global-config"))
        .arg(root)
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["checks"][0]["name"], "project");
    assert_eq!(report["checks"][0]["status"], "error");
}
