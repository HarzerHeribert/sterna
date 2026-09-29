use std::fs;
use std::path::PathBuf;

use sterna::settings_commands::{cli, execute};

fn temp_root(name: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!(
        "sterna-settings-command-{name}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    fs::create_dir_all(&root).unwrap();
    root
}

fn strings(values: &[&str]) -> Vec<String> {
    values.iter().map(|value| (*value).into()).collect()
}

#[test]
fn local_is_default_and_concrete_model_ids_are_validated() {
    let root = temp_root("local");
    let output = execute(&root, &strings(&["model.parent", "fixture-model"])).unwrap();
    assert!(output.contains("Saved model.parent"));
    let local = std::path::Path::new(".sterna").join("config.toml");
    assert!(output.contains(&local.display().to_string()), "{output}");
    assert!(
        fs::read_to_string(root.join(".sterna/config.toml"))
            .unwrap()
            .contains("fixture-model")
    );
    let before = fs::read_to_string(root.join(".sterna/config.toml")).unwrap();
    assert!(
        execute(
            &root,
            &strings(&["model.parent", "model", "with", "spaces"])
        )
        .is_err()
    );
    assert_eq!(
        before,
        fs::read_to_string(root.join(".sterna/config.toml")).unwrap()
    );
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn reading_and_unsetting_report_the_effective_origin() {
    let root = temp_root("unset");
    execute(&root, &strings(&["ui.statusline", "compact"])).unwrap();
    let read = execute(&root, &strings(&["ui.statusline"])).unwrap();
    assert!(read.contains("ui.statusline = compact"));
    assert!(read.contains("local") || read.contains("project"));

    let unset = execute(&root, &strings(&["--unset", "ui.statusline"])).unwrap();
    assert!(unset.contains("Unset ui.statusline"));
    assert!(unset.contains("full"));
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn invalid_values_do_not_write_and_help_is_registry_backed() {
    let root = temp_root("invalid");
    let error = execute(&root, &strings(&["ui.statusline", "enormous"])).unwrap_err();
    assert!(error.contains("statusline") || error.contains("full"));
    assert!(!root.join(".sterna/config.toml").exists());

    let help = execute(&root, &strings(&["--help"])).unwrap();
    assert!(help.contains("ui.statusline"));
    assert!(help.contains("full|compact|"));
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn cli_removes_root_options_safely_wherever_they_appear() {
    let root = temp_root("cli-root");
    let output = cli(&strings(&[
        "config",
        "limits.evidence_gate",
        "--root",
        root.to_str().unwrap(),
        "true",
    ]))
    .unwrap();
    assert!(output.contains("Saved limits.evidence_gate"));
    assert!(root.join(".sterna/config.toml").exists());
    assert!(
        cli(&strings(&["config", "--root"]))
            .unwrap_err()
            .contains("requires a path")
    );
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn imports_preview_by_default_and_named_profiles_are_not_edited() {
    let root = temp_root("import");
    fs::create_dir_all(root.join(".glasshouse")).unwrap();
    fs::write(
        root.join(".glasshouse/pane.toml"),
        "[model]\nparent = 'legacy'\n",
    )
    .unwrap();

    let preview = execute(&root, &strings(&["import", "legacy"])).unwrap();
    assert!(preview.to_ascii_lowercase().contains("preview"));
    assert!(!root.join(".sterna/config.toml").exists());

    let profile = execute(
        &root,
        &strings(&["local", "--profile", "fast", "model.parent", "other"]),
    )
    .unwrap();
    assert!(profile.contains("read-only"));
    assert!(!root.join(".sterna/config.toml").exists());
    fs::remove_dir_all(root).unwrap();
}
