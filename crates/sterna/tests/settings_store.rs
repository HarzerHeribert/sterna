//! The settings store's contract, exercised through the public API only --
//! `docs/configuration.md`'s acceptance checklist.
//!
//! Every test supplies its own global directory through
//! [`Store::with_global`], so a run never reads or writes the developer's
//! `~/.config/sterna`, and never depends on `XDG_CONFIG_HOME` being anything in
//! particular. No test needs a Git repository: a project is a folder.

use std::path::{Path, PathBuf};

use sterna::settings::{Scope, Store, registry};

// -- scaffolding ---------------------------------------------------------

#[test]
fn absent_snapshots_are_scoped_and_detect_an_externally_created_empty_file() {
    let temp = Temp::new("empty-snapshot");
    let store = temp.store();
    let global = store.read(Scope::Global).unwrap();
    assert!(
        store
            .save(Scope::Local, &global, &[edit("ui.theme", "amber")])
            .is_err()
    );
    let local = store.read(Scope::Local).unwrap();
    write(&store.path(Scope::Local), "");
    assert!(
        store
            .save(Scope::Local, &local, &[edit("ui.theme", "amber")])
            .is_err()
    );
    assert_eq!(read(&store.path(Scope::Local)), "");
}

#[test]
fn unsupported_security_directives_cannot_be_partially_imported() {
    let temp = Temp::new("unsupported-permissions");
    let store = temp.store();
    write(
        &store.claude_path(),
        r#"{"permissions":{"allow":["Bash(*)"],"ask":["Bash(rm *)"]}}"#,
    );
    assert!(
        store
            .import("claude", false)
            .unwrap()
            .contains("permissions.ask")
    );
    assert!(store.import("claude", true).is_err());
    assert!(!store.path(Scope::Local).exists());
}

#[cfg(unix)]
#[test]
fn a_global_directory_swapped_for_a_symlink_is_refused() {
    let temp = Temp::new("global-swap");
    let store = temp.store();
    let snapshot = store.read(Scope::Global).unwrap();
    let moved = temp.path.join("moved-config");
    std::fs::rename(temp.global_dir(), &moved).unwrap();
    std::os::unix::fs::symlink(&moved, temp.global_dir()).unwrap();
    assert!(
        store
            .save(Scope::Global, &snapshot, &[edit("ui.theme", "amber")])
            .is_err()
    );
    assert!(!moved.join("config.toml").exists());
}

#[cfg(unix)]
#[test]
fn a_fifo_is_refused_without_waiting_for_a_writer() {
    use std::os::unix::ffi::OsStrExt;
    let temp = Temp::new("fifo");
    let store = temp.store();
    std::fs::create_dir_all(temp.root().join(".sterna")).unwrap();
    let path = std::ffi::CString::new(store.path(Scope::Local).as_os_str().as_bytes()).unwrap();
    assert_eq!(unsafe { libc::mkfifo(path.as_ptr(), 0o600) }, 0);
    assert!(store.load(None).is_err());
}

struct Temp {
    path: PathBuf,
}

impl Temp {
    fn new(tag: &str) -> Self {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "sterna-settings-{tag}-{}-{nanos}",
            std::process::id()
        ));
        std::fs::create_dir_all(path.join("project")).expect("project");
        std::fs::create_dir_all(path.join("config")).expect("config");
        // macOS resolves `/tmp` through a symbolic link; canonicalising here
        // keeps the store's own symlink refusal about the paths under test.
        let path = std::fs::canonicalize(&path).expect("canonicalise");
        Self { path }
    }

    fn root(&self) -> PathBuf {
        self.path.join("project")
    }

    fn global_dir(&self) -> PathBuf {
        self.path.join("config")
    }

    fn store(&self) -> Store {
        Store::with_global(&self.root(), Some(self.global_dir())).expect("store")
    }
}

impl Drop for Temp {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

fn write(path: &Path, text: &str) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("directory");
    }
    std::fs::write(path, text).expect("write");
}

fn read(path: &Path) -> String {
    std::fs::read_to_string(path).expect("read")
}

fn edit(key: &str, value: &str) -> (String, Option<String>) {
    (key.to_string(), Some(value.to_string()))
}

fn unset(key: &str) -> (String, Option<String>) {
    (key.to_string(), None)
}

fn string(values: &toml::Value, path: &str) -> Option<String> {
    let mut cursor = values;
    for part in path.split('.') {
        cursor = cursor.get(part)?;
    }
    cursor.as_str().map(str::to_string)
}

// -- layering ------------------------------------------------------------

/// The one key that does not layer, and why. Every other setting is
/// global-then-project because the person owns both files; a project
/// document travels inside a repository, so this one is refused where a
/// person would type it and dropped where a clone would ship it.
#[test]
fn the_sandbox_level_is_refused_in_the_project_scope_and_names_the_file_that_takes_it() {
    let temp = Temp::new("global-only-write");
    let store = temp.store();
    let local = store.read(Scope::Local).unwrap();
    let error = store
        .save(Scope::Local, &local, &[edit("sandbox.level", "full")])
        .expect_err("a project-scoped write of a global-only key is refused");
    assert!(
        error.contains("global setting only"),
        "the refusal says what the rule is: {error}"
    );
    assert!(
        error.contains("travels inside a repository"),
        "and why it exists: {error}"
    );
    assert!(
        error.contains(&store.path(Scope::Global).display().to_string()),
        "and where the setting does belong: {error}"
    );
    assert!(
        !store.path(Scope::Local).exists(),
        "a refused write touches no file"
    );
    // The same key in the scope that owns it is an ordinary save.
    let global = store.read(Scope::Global).unwrap();
    let loaded = store
        .save(Scope::Global, &global, &[edit("sandbox.level", "full")])
        .expect("the global scope takes it");
    assert_eq!(level(&loaded.values).as_deref(), Some("full"));
}

/// The enforcing half: a hand-written or cloned project document never goes
/// through `save`, so refusing the write alone would guard nothing. It is
/// dropped and announced -- not refused -- because a repository that cannot
/// disarm a reader should also not be able to stop them opening it.
#[test]
fn a_project_document_cannot_turn_off_confinement_and_the_load_says_so() {
    let temp = Temp::new("global-only-load");
    let store = temp.store();
    write(&store.path(Scope::Local), "[sandbox]\nlevel = \"full\"\n");
    let loaded = store.load(None).expect("the project file still loads");
    assert_eq!(
        level(&loaded.values),
        None,
        "a project document's copy never reaches the effective configuration"
    );
    assert!(
        loaded
            .notices
            .iter()
            .any(|notice| { notice.contains("sandbox.level") && notice.contains("ignored") }),
        "and the person is told it was ignored: {:?}",
        loaded.notices
    );
    // The project file is still usable for everything else it says.
    write(
        &store.path(Scope::Local),
        "[sandbox]\nlevel = \"full\"\n\n[ui]\ntheme = \"amber\"\n",
    );
    let loaded = store.load(None).expect("loads");
    assert_eq!(string(&loaded.values, "ui.theme").as_deref(), Some("amber"));
    assert_eq!(level(&loaded.values), None);
}

fn level(values: &toml::Value) -> Option<String> {
    string(values, "sandbox.level")
}

#[test]
fn global_defaults_are_overridden_by_the_project_with_visible_origins() {
    let temp = Temp::new("layering");
    let store = temp.store();
    write(
        &store.path(Scope::Global),
        "[model]\nparent = \"claude-opus-5\"\n\n[limits]\ncells = 12\n",
    );
    write(
        &store.path(Scope::Local),
        "[model]\nparent = \"claude-sonnet-5\"\n",
    );

    let loaded = store.load(None).expect("load");
    assert_eq!(
        loaded.config.model.parent.as_deref(),
        Some("claude-sonnet-5")
    );
    assert_eq!(loaded.config.limits.cells, Some(12));
    assert_eq!(loaded.origins["model.parent"], "project");
    assert_eq!(loaded.origins["limits.cells"], "global");
    // Curated presentation keys have a value before anyone has saved one.
    assert_eq!(loaded.origins["ui.theme"], "built-in");
    assert_eq!(
        string(&loaded.values, "ui.theme").as_deref(),
        Some(sterna::tui::Theme::natural().name())
    );
    assert_eq!(
        string(&loaded.values, "ui.statusline").as_deref(),
        Some("full")
    );
    assert_eq!(
        string(&loaded.values, "ui.sidebar").as_deref(),
        Some("auto")
    );
    // The sandbox level has no built-in row: it is present only where a
    // person saved one, which is how a retired permission word knows it may
    // still migrate.
    assert_eq!(level(&loaded.values), None);
    assert_eq!(
        string(&loaded.values, "session.effort").as_deref(),
        Some("auto")
    );
}

#[test]
fn a_named_profile_wins_over_both_scopes_and_says_so() {
    let temp = Temp::new("profile");
    let store = temp.store();
    write(
        &store.path(Scope::Local),
        "[model]\nparent = \"claude-opus-5\"\n\n[profiles.fast.model]\nparent = \"claude-haiku-4-5-20251001\"\n",
    );

    let base = store.load(None).expect("base");
    assert_eq!(base.config.model.parent.as_deref(), Some("claude-opus-5"));

    let fast = store.load(Some("fast")).expect("profile");
    assert_eq!(
        fast.config.model.parent.as_deref(),
        Some("claude-haiku-4-5-20251001")
    );
    assert_eq!(fast.origins["model.parent"], "profile:fast");
    assert!(fast.notices.iter().any(|notice| notice.contains("fast")));

    let missing = store.load(Some("nope")).expect_err("unknown profile");
    assert!(missing.contains("no profile named `nope`"), "{missing}");
}

#[test]
fn a_project_that_turns_subagents_off_drops_the_model_it_inherited() {
    let temp = Temp::new("agentsoff");
    let store = temp.store();
    // A lone `[agents] model` means `pinned`, here inherited from the user's
    // own defaults...
    write(
        &store.path(Scope::Global),
        "[agents]\nmodel = \"claude-haiku-4-5-20251001\"\n",
    );
    // ...and a project that refuses subagents drops it rather than composing
    // into `off` plus a pinned model, which is not a configuration at all.
    write(&store.path(Scope::Local), "[agents]\nmode = \"off\"\n");

    let loaded = store.load(None).expect("load");
    assert_eq!(loaded.config.agents.mode, sterna::config::AgentsMode::Off);
    assert!(loaded.config.agents.model.is_none());
    assert!(
        loaded
            .values
            .get("agents")
            .and_then(|table| table.get("model"))
            .is_none(),
        "{:?}",
        loaded.values.get("agents")
    );
    assert!(!loaded.origins.contains_key("agents.model"));

    // The inherited model stands on its own when nothing displaces it.
    std::fs::remove_file(store.path(Scope::Local)).expect("remove");
    let loaded = store.load(None).expect("load");
    assert_eq!(
        loaded.config.agents.mode,
        sterna::config::AgentsMode::Pinned
    );
    assert_eq!(
        loaded.config.agents.model.as_deref(),
        Some("claude-haiku-4-5-20251001")
    );
    // The row a panel shows says `pinned`, not the built-in default it
    // displaced, and attributes it to the file that caused it.
    assert_eq!(
        string(&loaded.values, "agents.mode").as_deref(),
        Some("pinned")
    );
    assert_eq!(loaded.origins["agents.mode"], "global");
}

#[test]
fn runtime_defaults_are_shown_before_anything_is_saved() {
    let temp = Temp::new("runtimedefaults");
    let store = temp.store();
    let loaded = store.load(None).expect("load");

    // A panel must never show "unset" for a default the runtime applies.
    assert_eq!(
        loaded.values["limits"]["reduce_above_tokens"].as_integer(),
        Some(sterna::config::REDUCE_ABOVE_TOKENS_DEFAULT as i64)
    );
    assert_eq!(
        loaded.values["decisions"]["reduce_returns"].as_bool(),
        Some(sterna::config::DecisionsConfig::default().reduce_returns)
    );
    assert_eq!(
        loaded.values["limits"]["cells"].as_integer(),
        // `0` is the file's spelling for the default, which is no ceiling.
        Some(sterna::config::Limits::default().cells.unwrap_or(0) as i64)
    );
    // The chips mark what runs: Ask weighting, the decision mode and each
    // favourite's effort are the runtime's own defaults, never a guess.
    assert_eq!(
        sterna::settings::registry::shown_default("ask.jev").as_deref(),
        Some(sterna::config::AskConfig::default().jev.as_str())
    );
    assert_eq!(
        sterna::settings::registry::shown_default("decisions.mode").as_deref(),
        Some(sterna::config::DecisionsConfig::default().mode.as_str())
    );
    assert_eq!(
        sterna::settings::registry::shown_default("agents.slots.deep.effort").as_deref(),
        Some(sterna::config::slot_effort("deep").name())
    );
    assert_eq!(loaded.origins["limits.reduce_above_tokens"], "built-in");
    assert_eq!(loaded.origins["limits.cells"], "built-in");
    // The model ids have no default: unset is what "off" means for agents,
    // and the panel must say so rather than invent one.
    assert!(loaded.values.get("model").is_none());
    assert!(!loaded.origins.contains_key("agents.model"));
}

#[test]
fn a_missing_file_is_read_without_creating_anything() {
    let temp = Temp::new("missing");
    let store = temp.store();

    let loaded = store.load(None).expect("load");
    assert!(loaded.config.model.parent.is_none());
    let snapshot = store.read(Scope::Local).expect("read");
    assert!(!snapshot.exists);
    assert!(snapshot.text.is_empty());

    assert!(!temp.root().join(".sterna").exists());
    assert!(!store.path(Scope::Global).exists());
}

#[test]
fn a_host_without_a_user_directory_still_has_a_project_scope() {
    let temp = Temp::new("noglobal");
    let store = Store::with_global(&temp.root(), None).expect("store");
    write(
        &store.path(Scope::Local),
        "[model]\nparent = \"claude-haiku-4-5-20251001\"\n",
    );

    assert!(!store.global_available());
    assert_eq!(store.path(Scope::Global), PathBuf::new());
    let loaded = store.load(None).expect("load");
    assert_eq!(
        loaded.config.model.parent.as_deref(),
        Some("claude-haiku-4-5-20251001")
    );
    assert!(
        loaded
            .notices
            .iter()
            .any(|notice| notice.contains("global settings are unavailable"))
    );
    assert!(store.read(Scope::Global).is_err());
    let refused = store
        .save(
            Scope::Global,
            &store.read(Scope::Local).expect("read"),
            &[edit("ui.theme", "amber")],
        )
        .expect_err("no global scope");
    assert!(
        refused.contains("no user configuration directory"),
        "{refused}"
    );
}

// -- writing -------------------------------------------------------------

#[test]
fn saving_preserves_comments_and_unrelated_values() {
    let temp = Temp::new("comments");
    let store = temp.store();
    let path = store.path(Scope::Local);
    write(
        &path,
        "# the project's own note\n[limits]\n# how long one cell may run\ncell_wall_clock_s = 45\ncells = 12\n",
    );

    let snapshot = store.read(Scope::Local).expect("read");
    let loaded = store
        .save(
            Scope::Local,
            &snapshot,
            &[edit("limits.cells", "7"), edit("ui.theme", "amber")],
        )
        .expect("save");

    let text = read(&path);
    assert!(text.contains("# the project's own note"), "{text}");
    assert!(text.contains("# how long one cell may run"), "{text}");
    assert!(text.contains("cell_wall_clock_s = 45"), "{text}");
    assert!(text.contains("cells = 7"), "{text}");
    assert_eq!(loaded.config.limits.cells, Some(7));
    assert_eq!(loaded.config.limits.cell_wall_clock_s, 45);
    assert_eq!(string(&loaded.values, "ui.theme").as_deref(), Some("amber"));
    assert_eq!(loaded.origins["ui.theme"], "project");

    // The atomic write leaves neither a temporary nor a lock behind.
    let leftovers: Vec<String> = std::fs::read_dir(path.parent().expect("directory"))
        .expect("read_dir")
        .map(|entry| {
            entry
                .expect("entry")
                .file_name()
                .to_string_lossy()
                .to_string()
        })
        .filter(|name| name != "config.toml")
        .collect();
    assert!(leftovers.is_empty(), "{leftovers:?}");
}

#[test]
fn an_invalid_value_writes_nothing() {
    let temp = Temp::new("invalid");
    let store = temp.store();
    let path = store.path(Scope::Local);
    write(&path, "[limits]\ncells = 12\n");
    let snapshot = store.read(Scope::Local).expect("read");

    let error = store
        .save(Scope::Local, &snapshot, &[edit("limits.cells", "1001")])
        .expect_err("out of range");
    assert!(error.contains("between 1 and 1000"), "{error}");

    let unknown = store
        .save(Scope::Local, &snapshot, &[edit("limits.celss", "x")])
        .expect_err("unknown key");
    assert!(
        unknown.contains("is not a setting Sterna supports"),
        "{unknown}"
    );

    let unknown_unset = store
        .save(Scope::Local, &snapshot, &[unset("limits.celss")])
        .expect_err("unknown key");
    assert!(unknown_unset.contains("not a setting"), "{unknown_unset}");

    // Both the document and the directory are exactly as they were.
    assert_eq!(read(&path), "[limits]\ncells = 12\n");
    let leftovers: Vec<String> = std::fs::read_dir(path.parent().expect("directory"))
        .expect("read_dir")
        .map(|entry| {
            entry
                .expect("entry")
                .file_name()
                .to_string_lossy()
                .to_string()
        })
        .filter(|name| name != "config.toml")
        .collect();
    assert!(leftovers.is_empty(), "{leftovers:?}");
}

#[test]
fn an_inconsistent_model_and_mode_is_refused_before_the_write() {
    let temp = Temp::new("crosskey");
    let store = temp.store();
    let path = store.path(Scope::Local);
    let snapshot = store.read(Scope::Local).expect("read");

    let error = store
        .save(Scope::Local, &snapshot, &[edit("agents.mode", "pinned")])
        .expect_err("pinned needs a model");
    assert!(error.contains("requires `model`"), "{error}");
    assert!(!path.exists(), "a refused save created the file");

    let loaded = store
        .save(
            Scope::Local,
            &snapshot,
            &[
                edit("agents.mode", "pinned"),
                edit("agents.model", "claude-haiku-4-5-20251001"),
            ],
        )
        .expect("both keys together");
    assert_eq!(
        loaded.config.agents.model.as_deref(),
        Some("claude-haiku-4-5-20251001")
    );
}

#[test]
fn unsetting_a_key_falls_back_to_the_inherited_value() {
    let temp = Temp::new("unset");
    let store = temp.store();
    write(
        &store.path(Scope::Global),
        "[model]\nparent = \"claude-opus-5\"\n",
    );
    write(
        &store.path(Scope::Local),
        "[model]\nparent = \"claude-sonnet-5\"\n\n[web]\ntimeout_seconds = 30\n",
    );

    let snapshot = store.read(Scope::Local).expect("read");
    let loaded = store
        .save(
            Scope::Local,
            &snapshot,
            &[unset("model.parent"), unset("web.timeout_seconds")],
        )
        .expect("unset");

    assert_eq!(loaded.config.model.parent.as_deref(), Some("claude-opus-5"));
    assert_eq!(loaded.origins["model.parent"], "global");
    assert_eq!(
        loaded.config.web.timeout_seconds,
        sterna::web::WebConfig::default().timeout_seconds
    );
    let text = read(&store.path(Scope::Local));
    assert!(!text.contains("parent"), "{text}");
    // The emptied tables go with the keys that were in them.
    assert!(!text.contains("[web]"), "{text}");
}

#[test]
fn an_edit_against_a_stale_snapshot_is_refused() {
    let temp = Temp::new("conflict");
    let store = temp.store();
    let path = store.path(Scope::Local);
    write(&path, "[limits]\ncells = 12\n");
    let snapshot = store.read(Scope::Local).expect("read");

    // Another editor saves first.
    write(&path, "[limits]\ncells = 30\n");

    let error = store
        .save(Scope::Local, &snapshot, &[edit("limits.cells", "7")])
        .expect_err("stale");
    assert!(error.contains("changed since it was read"), "{error}");
    assert_eq!(read(&path), "[limits]\ncells = 30\n");
}

#[test]
fn a_snapshot_from_the_other_scope_is_refused() {
    let temp = Temp::new("scopemix");
    let store = temp.store();
    write(&store.path(Scope::Global), "[limits]\ncells = 12\n");
    let global = store.read(Scope::Global).expect("read");

    let error = store
        .save(Scope::Local, &global, &[edit("limits.cells", "7")])
        .expect_err("wrong scope");
    assert!(error.contains("was read from"), "{error}");
}

#[test]
fn a_profile_save_targets_the_overlay_and_leaves_the_base_alone() {
    let temp = Temp::new("saveprofile");
    let store = temp.store();
    write(
        &store.path(Scope::Local),
        "[model]\nparent = \"claude-opus-5\"\n",
    );
    let snapshot = store.read(Scope::Local).expect("read");

    let loaded = store
        .save_profile(
            Scope::Local,
            &snapshot,
            &[edit("model.parent", "claude-haiku-4-5-20251001")],
            Some("fast"),
        )
        .expect("save profile");
    assert_eq!(
        loaded.config.model.parent.as_deref(),
        Some("claude-haiku-4-5-20251001")
    );
    assert_eq!(loaded.origins["model.parent"], "profile:fast");
    assert_eq!(
        store
            .load(None)
            .expect("base")
            .config
            .model
            .parent
            .as_deref(),
        Some("claude-opus-5")
    );

    let snapshot = store.read(Scope::Local).expect("read");
    let refused = store
        .save_profile(
            Scope::Local,
            &snapshot,
            &[edit("ui.theme", "amber")],
            Some("fast"),
        )
        .expect_err("presentation is not a runtime overlay");
    assert!(refused.contains("overlays only"), "{refused}");

    let refused = store
        .save_profile(
            Scope::Global,
            &snapshot,
            &[edit("limits.cells", "7")],
            Some("fast"),
        )
        .expect_err("no global profiles");
    assert!(refused.contains("named profiles live in"), "{refused}");
}

// -- path safety ---------------------------------------------------------

/// Plant a symbolic link, or say why this host cannot. Windows grants
/// symlink creation only to an elevated or Developer-Mode account (error
/// 1314, `ERROR_PRIVILEGE_NOT_HELD`), and a test that cannot plant the link
/// has nothing to refuse: it prints `skipped:` and ends, and the privileged
/// CI cell keeps the check. Any other failure is still a failure.
fn plant_link(target: &std::path::Path, link: &std::path::Path, directory: bool) -> bool {
    #[cfg(unix)]
    {
        let _ = directory;
        std::os::unix::fs::symlink(target, link).expect("symlink");
        true
    }
    #[cfg(windows)]
    {
        let made = if directory {
            std::os::windows::fs::symlink_dir(target, link)
        } else {
            std::os::windows::fs::symlink_file(target, link)
        };
        match made {
            Ok(()) => true,
            Err(error) if error.raw_os_error() == Some(1314) => {
                println!(
                    "skipped: this account may not create symbolic links (os error 1314); \
                     the privileged CI cell keeps the check"
                );
                false
            }
            Err(error) => panic!("symlink: {error}"),
        }
    }
}

#[test]
fn a_symbolic_link_on_the_settings_path_is_refused() {
    let temp = Temp::new("symlink");
    let store = temp.store();
    let elsewhere = temp.path.join("elsewhere");
    std::fs::create_dir_all(&elsewhere).expect("elsewhere");
    if !plant_link(&elsewhere, &temp.root().join(".sterna"), true) {
        return;
    }

    let error = store.read(Scope::Local).expect_err("linked directory");
    assert!(error.contains("symbolic link"), "{error}");
    assert!(store.load(None).is_err());
}

#[test]
fn a_symbolic_link_in_place_of_the_file_is_refused() {
    let temp = Temp::new("symlinkfile");
    let store = temp.store();
    let target = temp.path.join("stolen.toml");
    write(&target, "[limits]\ncells = 3\n");
    std::fs::create_dir_all(temp.root().join(".sterna")).expect("directory");
    if !plant_link(&target, &store.path(Scope::Local), false) {
        return;
    }

    let error = store.read(Scope::Local).expect_err("linked file");
    assert!(error.contains("symbolic link"), "{error}");
    assert_eq!(read(&target), "[limits]\ncells = 3\n");
}

// -- permissions ---------------------------------------------------------

#[test]
fn native_permissions_render_for_the_compiler_with_global_denials_surviving() {
    let temp = Temp::new("permissions");
    let store = temp.store();
    assert!(store.permissions().expect("none").is_none());

    write(
        &store.path(Scope::Global),
        "[permissions]\nallow = [\"Read(src/**)\"]\ndeny = [\"Read(secrets/**)\"]\n",
    );
    write(
        &store.path(Scope::Local),
        "[permissions]\nallow = [\"Read(docs/**)\"]\ndeny = [\"Bash(rm)\"]\n",
    );

    let json = store.permissions().expect("permissions").expect("some");
    let document: serde_json::Value = serde_json::from_str(&json).expect("json");
    let allow = document["permissions"]["allow"].as_array().expect("allow");
    let deny = document["permissions"]["deny"].as_array().expect("deny");
    // A project may narrow the allow list it inherits...
    assert_eq!(allow.len(), 1);
    assert_eq!(allow[0], "Read(docs/**)");
    // ...and may not drop a denial it inherits.
    let deny: Vec<&str> = deny.iter().filter_map(serde_json::Value::as_str).collect();
    assert!(deny.contains(&"Read(secrets/**)"), "{deny:?}");
    assert!(deny.contains(&"Bash(rm)"), "{deny:?}");

    let loaded = store.load(None).expect("load");
    assert_eq!(loaded.origins["permissions.deny"], "global+project");
}

#[test]
fn a_permission_rule_that_grants_nothing_is_refused_with_a_reason() {
    let temp = Temp::new("badrule");
    let store = temp.store();
    let snapshot = store.read(Scope::Local).expect("read");

    let network = store
        .save(
            Scope::Local,
            &snapshot,
            &[edit("permissions.allow", "WebFetch(https://example.org)")],
        )
        .expect_err("network is never a pattern");
    assert!(
        network.contains("network reach is never a permission pattern"),
        "{network}"
    );

    let unknown = store
        .save(
            Scope::Local,
            &snapshot,
            &[edit("permissions.allow", "Sudo(rm)")],
        )
        .expect_err("unknown kind");
    assert!(
        unknown.contains("not a permission pattern kind"),
        "{unknown}"
    );

    let escaping = store
        .save(
            Scope::Local,
            &snapshot,
            &[edit("permissions.allow", "Read(../**)")],
        )
        .expect_err("outside the project");
    assert!(escaping.contains("outside the project root"), "{escaping}");

    assert!(!store.path(Scope::Local).exists());
}

// -- imports -------------------------------------------------------------

#[test]
fn importing_claude_permissions_is_explicit_and_takes_nothing_else() {
    let temp = Temp::new("importclaude");
    let store = temp.store();
    let source = temp.root().join(".claude").join("settings.json");
    let original = r#"{
  "apiKeyHelper": "/bin/echo secret",
  "env": { "TOKEN": "shh" },
  "hooks": { "PreToolUse": [] },
  "permissions": {
    "allow": ["Read(docs/**)", "Bash(git status)", "WebFetch(https://example.org)", "Read(.env)"],
    "deny": ["Read(.env)"],
    "ask": ["Read(other/**)"]
  }
}
"#;
    write(&source, original);

    let preview = store.import("claude", false).expect("preview");
    assert!(
        preview.contains("preview; nothing was written"),
        "{preview}"
    );
    assert!(preview.contains("Read(docs/**)"), "{preview}");
    assert!(preview.contains("apiKeyHelper"), "{preview}");
    assert!(preview.contains("env"), "{preview}");
    assert!(preview.contains("hooks"), "{preview}");
    assert!(preview.contains("rejected"), "{preview}");
    assert!(preview.contains("WebFetch"), "{preview}");
    assert!(!store.path(Scope::Local).exists(), "a preview wrote a file");

    // A rule that grants nothing would make the imported permission set a
    // different one from the document a person read, so the apply refuses
    // rather than importing "the rest".
    let refused = store
        .import("claude", true)
        .expect_err("a rejected rule stops the apply");
    assert!(
        refused.contains("Refused: nothing was written"),
        "{refused}"
    );
    assert!(
        !store.path(Scope::Local).exists(),
        "a refused import wrote a file"
    );

    let original = original
        .replace("\"WebFetch(https://example.org)\", ", "")
        .replace(",\n    \"ask\": [\"Read(other/**)\"]", "");
    write(&source, &original);
    let applied = store.import("claude", true).expect("apply");
    assert!(applied.contains("Written."), "{applied}");
    assert_eq!(read(&source), original, "the source document was modified");

    let json = store.permissions().expect("permissions").expect("some");
    let document: serde_json::Value = serde_json::from_str(&json).expect("json");
    let allow: Vec<String> = document["permissions"]["allow"]
        .as_array()
        .expect("allow")
        .iter()
        .filter_map(|value| value.as_str().map(str::to_string))
        .collect();
    let deny: Vec<String> = document["permissions"]["deny"]
        .as_array()
        .expect("deny")
        .iter()
        .filter_map(|value| value.as_str().map(str::to_string))
        .collect();
    assert!(allow.contains(&"Read(docs/**)".to_string()), "{allow:?}");
    assert!(allow.contains(&"Bash(git status)".to_string()), "{allow:?}");
    // Denied in the same document: a denial beats every allow, both here and
    // in the compiler, so it is not imported as one.
    assert!(!allow.contains(&"Read(.env)".to_string()), "{allow:?}");
    assert!(deny.contains(&"Read(.env)".to_string()), "{deny:?}");
    // Nothing that is not a permission crossed the boundary.
    let text = read(&store.path(Scope::Local));
    assert!(!text.contains("TOKEN"), "{text}");
    assert!(!text.contains("hooks"), "{text}");
    assert!(!text.contains("apiKeyHelper"), "{text}");

    // A second import has nothing left to do and still writes nothing.
    let again = store.import("claude", false).expect("second preview");
    assert!(again.contains("nothing to import"), "{again}");
}

#[test]
fn the_legacy_file_is_a_visible_fallback_until_it_is_migrated() {
    let temp = Temp::new("legacy");
    let store = temp.store();
    let legacy = store.legacy_path();
    let original = "[model]\nparent = \"claude-opus-5\"\n\n[limits]\ncells = 9\n";
    write(&legacy, original);

    // Legacy alone: it is still read, and the notice says where from.
    let loaded = store.load(None).expect("legacy fallback");
    assert_eq!(loaded.config.limits.cells, Some(9));
    assert_eq!(loaded.origins["limits.cells"], "legacy");
    assert!(
        loaded
            .notices
            .iter()
            .any(|notice| notice.contains("read-only legacy fallback")),
        "{:?}",
        loaded.notices
    );

    // A save into the project file would leave two files disagreeing about
    // one project, so it asks for the migration instead.
    let snapshot = store.read(Scope::Local).expect("read");
    let refused = store
        .save(Scope::Local, &snapshot, &[edit("ui.theme", "amber")])
        .expect_err("migration first");
    assert!(refused.contains("import legacy"), "{refused}");
    assert!(!store.path(Scope::Local).exists());

    // Two files that disagree are a diagnostic, never a guess.
    write(&store.path(Scope::Local), "[limits]\ncells = 5\n");
    let error = store.load(None).expect_err("disagreement");
    assert!(error.contains("limits.cells"), "{error}");
    assert!(error.contains("import legacy"), "{error}");

    let preview = store.import("legacy", false).expect("preview");
    assert!(
        preview.contains("preview; nothing was written"),
        "{preview}"
    );
    assert!(preview.contains("conflict"), "{preview}");
    assert!(preview.contains("model.parent"), "{preview}");
    assert_eq!(read(&store.path(Scope::Local)), "[limits]\ncells = 5\n");

    let applied = store.import("legacy", true).expect("apply");
    assert!(applied.contains("Written."), "{applied}");
    assert_eq!(read(&legacy), original, "the legacy file was modified");

    // Provenance: the migration is recorded, so the preserved file is no
    // longer read and the two can never conflict again.
    let loaded = store.load(None).expect("after migration");
    assert_eq!(loaded.config.model.parent.as_deref(), Some("claude-opus-5"));
    assert_eq!(loaded.origins["model.parent"], "project");
    assert_eq!(loaded.config.limits.cells, Some(5));
    assert!(
        loaded
            .notices
            .iter()
            .any(|notice| notice.contains("no longer read")),
        "{:?}",
        loaded.notices
    );

    // And ordinary saving works again.
    let snapshot = store.read(Scope::Local).expect("read");
    let loaded = store
        .save(Scope::Local, &snapshot, &[edit("ui.theme", "amber")])
        .expect("save after migration");
    assert_eq!(string(&loaded.values, "ui.theme").as_deref(), Some("amber"));
}

#[test]
fn an_unknown_import_source_is_refused() {
    let temp = Temp::new("importsource");
    let store = temp.store();
    let error = store.import("vscode", false).expect_err("unknown source");
    assert!(error.contains("use `claude` or `legacy`"), "{error}");
    let missing = store.import("claude", false).expect_err("missing source");
    assert!(missing.contains("does not exist"), "{missing}");
}

// -- the registry --------------------------------------------------------

#[test]
fn every_choice_the_registry_offers_validates() {
    for spec in registry::specs() {
        for choice in spec.choices {
            registry::validate(spec.key, choice)
                .unwrap_or_else(|error| panic!("{}: {choice}: {error}", spec.key));
        }
    }
    assert!(registry::spec("ui.theme").is_some());
    assert!(registry::spec("nonsense").is_none());
    assert!(registry::validate("nonsense", "x").is_err());
    // The picker's themes and the registry's are one list.
    let themes: Vec<&str> = sterna::tui::Theme::ALL
        .iter()
        .map(|theme| theme.name())
        .collect();
    assert_eq!(
        registry::spec("ui.theme").expect("theme").choices,
        &themes[..]
    );
}

#[test]
fn typed_values_are_parsed_without_toml_quoting() {
    assert_eq!(
        registry::validate("limits.evidence_gate", "true").expect("bool"),
        toml::Value::Boolean(true)
    );
    assert_eq!(
        registry::validate("limits.cells", "12").expect("integer"),
        toml::Value::Integer(12)
    );
    assert_eq!(
        registry::validate("web.allow_domains", "example.org, *.example.net").expect("list"),
        toml::Value::Array(vec![
            toml::Value::String("example.org".into()),
            toml::Value::String("*.example.net".into()),
        ])
    );
    assert_eq!(
        registry::validate("web.allow_domains", "[\"example.org\"]").expect("array literal"),
        toml::Value::Array(vec![toml::Value::String("example.org".into())])
    );
    // The runtime parser owns the ranges, the model rules and the domains.
    assert!(registry::validate("decisions.completion_no_below", "0.6").is_err());
    assert_eq!(
        registry::validate("decisions.completion_no_below", "0.2").expect("in range"),
        toml::Value::Float(0.2)
    );
    assert!(registry::validate("decisions.hygiene_no_below", "0.6").is_err());
    assert!(registry::validate("decisions.hygiene_yes_above", "0.4").is_err());
    // `0` is the file's spelling for "no ceiling", so it validates; a figure
    // outside the range and a word still do not.
    assert_eq!(
        registry::validate("limits.cells", "0").expect("no ceiling"),
        toml::Value::Integer(0)
    );
    assert!(registry::validate("limits.cells", "1001").is_err());
    assert!(registry::validate("limits.cells", "many").is_err());
    assert!(registry::validate("web.allow_domains", "not a domain").is_err());
    assert!(registry::validate("model.parent", "/etc/passwd").is_err());
    assert!(registry::validate("agents.slots.quick.effort", "auto").is_err());
    assert!(registry::validate("ui.theme", "chartreuse").is_err());
    // Documented aliases are normalised to the spelling that is written.
    assert_eq!(
        registry::validate("ui.statusline", "hide").expect("alias"),
        toml::Value::String("hidden".into())
    );
}

#[test]
fn a_file_with_an_unknown_key_is_refused_by_name() {
    let temp = Temp::new("unknownfile");
    let store = temp.store();
    write(&store.path(Scope::Local), "[limits]\ncelsl = 12\n");
    let error = store.load(None).expect_err("unknown key");
    assert!(error.contains("limits.celsl"), "{error}");

    write(&store.path(Scope::Local), "[nonsense]\nvalue = 1\n");
    let error = store.load(None).expect_err("unknown table");
    assert!(error.contains("nonsense.value"), "{error}");
}

/// The permission redesign retired the working modes, the Explore overlay,
/// the mode proposal and the decision model's command vouching. None is a
/// setting any more, and a saved copy is read as unset, never refused.
#[test]
fn the_retired_mode_and_permission_keys_are_no_longer_settings() {
    for key in [
        "session.mode",
        "modes.explore.writable",
        "modes.explore.commands",
        "permissions.mode",
        "permissions.full_access",
        "decisions.mode_above",
        "decisions.command_runs_above",
    ] {
        assert!(registry::spec(key).is_none(), "{key} is still a setting");
        assert!(registry::retired_key(key).is_some(), "{key} is not retired");
        assert!(registry::validate(key, "x").is_err(), "{key} validates");
    }
    assert!(registry::spec("sandbox.level").is_some());
    assert_eq!(
        registry::validate("sandbox.level", "ask").expect("a level"),
        toml::Value::String("ask".into())
    );
    assert!(registry::validate("sandbox.level", "manual").is_err());

    let temp = Temp::new("retired-modes");
    let store = temp.store();
    write(
        &store.path(Scope::Local),
        "[session]\nmode = \"explore\"\n\n[modes.explore]\nwritable = [\"docs/**\"]\n\n\
         [decisions]\nmode_above = 0.9\ncommand_runs_above = 0.95\n",
    );
    let loaded = store.load(None).expect("a retired key never stops a load");
    let mut keys: Vec<&str> = loaded.retired.iter().map(|r| r.key.as_str()).collect();
    keys.sort_unstable();
    assert_eq!(
        keys,
        [
            "decisions.command_runs_above",
            "decisions.mode_above",
            "modes.explore.writable",
            "session.mode",
        ]
    );
}

/// The one mapping of an old permission word onto a level.
#[test]
fn a_retired_permission_word_names_the_level_it_meant() {
    for (key, word, level) in [
        ("permissions.mode", "manual", Some("ask")),
        ("permissions.mode", "accept-edits", Some("ask")),
        ("permissions.mode", "accept_edits", Some("ask")),
        ("permissions.mode", "acceptEdits", Some("ask")),
        ("permissions.mode", "auto", Some("sandboxed")),
        ("permissions.mode", "full", Some("sandboxed")),
        ("permissions.full_access", "true", Some("full")),
        ("permissions.full_access", "false", None),
        ("session.mode", "explore", None),
    ] {
        assert_eq!(registry::migrated_level(key, word), level, "{key} = {word}");
    }
}

/// A global rung is migrated onto `sandbox.level`, removed from its file,
/// and reported once in the sentence that names the new command.
#[test]
fn a_global_permission_rung_migrates_to_a_sandbox_level() {
    let temp = Temp::new("migrate-rung");
    let store = temp.store();
    write(
        &store.path(Scope::Global),
        "[permissions]\nmode = \"manual\"\n",
    );
    let loaded = store.load(None).expect("loads");
    let notices = store.remove_retired(&loaded);
    assert_eq!(
        notices,
        ["`permissions.mode = manual` is now `sandbox.level = \"ask\"`; /sandbox changes it."]
    );
    let after = store.load(None).expect("loads");
    assert_eq!(level(&after.values).as_deref(), Some("ask"));
    assert!(after.retired.is_empty(), "{:?}", after.retired);
    assert!(!read(&store.path(Scope::Global)).contains("mode"));
}

/// A setting that kept its job and changed its table is read under its new
/// key, written there in its own file, and said once.
#[test]
fn a_moved_setting_is_read_under_its_new_key_and_rewritten_once() {
    let temp = Temp::new("migrate-moved");
    let store = temp.store();
    write(
        &store.path(Scope::Global),
        "[helpers]\nreduce_above_tokens = 4096\nreduce_returns = false\n",
    );
    let loaded = store.load(None).expect("loads");
    assert_eq!(loaded.config.limits.reduce_above_tokens, 4096);
    assert!(!loaded.config.decisions.reduce_returns);
    let notices = store.remove_retired(&loaded);
    assert!(
        notices.contains(
            &"`helpers.reduce_above_tokens = 4096` is `limits.reduce_above_tokens` now, and your settings were updated; /settings changes it."
                .to_string()
        ),
        "{notices:?}"
    );
    let text = read(&store.path(Scope::Global));
    assert!(!text.contains("[helpers]"), "{text}");
    assert!(text.contains("reduce_above_tokens = 4096"), "{text}");
    assert!(text.contains("reduce_returns = false"), "{text}");
    let after = store.load(None).expect("loads");
    assert!(after.retired.is_empty(), "said once: {:?}", after.retired);
    assert_eq!(after.config.limits.reduce_above_tokens, 4096);
    assert!(!after.config.decisions.reduce_returns);
}

/// A moved setting never overwrites a value its new key already holds in
/// the same file: that is the newer choice.
#[test]
fn a_moved_setting_leaves_a_value_already_at_its_new_key() {
    let temp = Temp::new("migrate-moved-taken");
    let store = temp.store();
    write(
        &store.path(Scope::Local),
        "[helpers]\nreduce_returns = false\n\n[decisions]\nreduce_returns = true\n",
    );
    let loaded = store.load(None).expect("loads");
    assert!(loaded.config.decisions.reduce_returns, "the new key wins");
    store.remove_retired(&loaded);
    let text = read(&store.path(Scope::Local));
    assert!(!text.contains("[helpers]"), "{text}");
    assert!(text.contains("reduce_returns = true"), "{text}");
    assert!(!text.contains("reduce_returns = false"), "{text}");
}

/// Every other helper setting, and the decision thresholds that tuned the
/// helpers, is read as unset, removed from its file and said once; none of
/// them stops Sterna from starting.
#[test]
fn a_retired_helper_setting_is_removed_and_said_once() {
    let temp = Temp::new("migrate-helpers");
    let store = temp.store();
    write(
        &store.path(Scope::Local),
        "[helpers]\nenabled = true\nmodel = \"gpt-5.6-luna\"\n\n[helpers.effort]\nfind = \"low\"\n\n[decisions]\nscout_above = 0.8\n",
    );
    let loaded = store.load(None).expect("a file naming helpers still loads");
    let notices = store.remove_retired(&loaded);
    for key in [
        "helpers.enabled",
        "helpers.model",
        "helpers.effort.find",
        "decisions.scout_above",
    ] {
        assert!(
            notices
                .iter()
                .any(|notice| notice.starts_with(&format!("`{key} = "))
                    && notice
                        .contains("is no longer a setting, so it was removed from your settings")
                    && notice.contains("helpers are gone")),
            "{key}: {notices:?}"
        );
    }
    let text = read(&store.path(Scope::Local));
    assert!(!text.contains("helpers"), "{text}");
    assert!(!text.contains("scout_above"), "{text}");
    let after = store.load(None).expect("loads");
    assert!(after.retired.is_empty(), "said once: {:?}", after.retired);
}

/// A profile overlay that names helper settings still loads: a moved key
/// applies under its new name inside the overlay, a retired one is dropped,
/// and both are taken out of the overlay once. The base file setting the new
/// key does not stop the overlay's own value from moving: only the overlay
/// holding it already would.
#[test]
fn a_profile_overlay_with_helper_settings_still_loads() {
    let temp = Temp::new("migrate-profile");
    let store = temp.store();
    write(
        &store.path(Scope::Local),
        "[limits]\ncells = 40\n\n[decisions]\nreduce_returns = true\n\n[profiles.review.limits]\ncells = 12\n\n[profiles.review.helpers]\nmodel = \"gpt-5.6-luna\"\nreduce_returns = false\n",
    );
    let loaded = store.load(Some("review")).expect("the overlay loads");
    assert_eq!(loaded.config.limits.cells, Some(12));
    assert!(!loaded.config.decisions.reduce_returns);
    let notices = store.remove_retired(&loaded);
    assert_eq!(notices.len(), 2, "{notices:?}");
    assert!(
        notices.iter().any(|notice| notice.starts_with(
            "`profiles.review.helpers.reduce_returns = false` is `profiles.review.decisions.reduce_returns` now"
        )),
        "{notices:?}"
    );
    let text = read(&store.path(Scope::Local));
    assert!(!text.contains("helpers"), "{text}");
    assert!(text.contains("reduce_returns = false"), "{text}");
    let after = store.load(Some("review")).expect("loads");
    assert!(after.retired.is_empty(), "said once: {:?}", after.retired);
    assert!(!after.config.decisions.reduce_returns);
    assert_eq!(after.config.limits.cells, Some(12));
    // Moved inside the overlay, not out of it: without the profile the
    // base file's value still stands.
    let base = store.load(None).expect("loads");
    assert!(base.config.decisions.reduce_returns, "{text}");
}

/// The threshold that checked each cell against the model's to-do list went
/// with the list: removed from its file and said once, and Sterna still
/// starts.
#[test]
fn the_drift_threshold_is_retired_with_the_to_do_list() {
    let temp = Temp::new("migrate-drift");
    let store = temp.store();
    write(
        &store.path(Scope::Global),
        "[decisions]
drift_no_below = 0.2
hold_above = 0.9
",
    );
    let loaded = store
        .load(None)
        .expect("a file naming the drift threshold still loads");
    let notices = store.remove_retired(&loaded);
    assert!(
        notices.iter().any(
            |notice| notice.starts_with("`decisions.drift_no_below = 0.2`")
                && notice.contains("to-do list is gone")
        ),
        "{notices:?}"
    );
    let text = read(&store.path(Scope::Global));
    assert!(!text.contains("drift_no_below"), "{text}");
    assert!(text.contains("hold_above = 0.9"), "{text}");
    assert!(store.load(None).expect("loads").retired.is_empty());
}

/// `full_access = true` alone becomes `full`; beside a rung, the rung
/// decided the old session's level, so it decides the new one.
#[test]
fn full_access_migrates_to_full_unless_a_rung_beside_it_decided() {
    let temp = Temp::new("migrate-full");
    let store = temp.store();
    write(
        &store.path(Scope::Global),
        "[permissions]\nfull_access = true\n",
    );
    let loaded = store.load(None).expect("loads");
    store.remove_retired(&loaded);
    assert_eq!(
        level(&store.load(None).unwrap().values).as_deref(),
        Some("full")
    );

    let temp = Temp::new("migrate-rung-wins");
    let store = temp.store();
    write(
        &store.path(Scope::Global),
        "[permissions]\nmode = \"auto\"\nfull_access = true\n",
    );
    let loaded = store.load(None).expect("loads");
    let notices = store.remove_retired(&loaded);
    assert_eq!(
        level(&store.load(None).unwrap().values).as_deref(),
        Some("sandboxed"),
        "{notices:?}"
    );
    assert!(
        notices.iter().any(|notice| notice.starts_with(
            "`permissions.full_access = true` is no longer a setting, so it was removed from your settings; "
        )),
        "{notices:?}"
    );
}

/// A level the person already saved is never overwritten by an old word,
/// and a project file's copy is only removed: the level is global only.
#[test]
fn a_retired_rung_never_overrides_a_saved_level_or_migrates_from_a_project() {
    let temp = Temp::new("migrate-saved");
    let store = temp.store();
    write(
        &store.path(Scope::Global),
        "[permissions]\nmode = \"manual\"\n\n[sandbox]\nlevel = \"full\"\n",
    );
    let loaded = store.load(None).expect("loads");
    let notices = store.remove_retired(&loaded);
    assert_eq!(
        notices,
        [format!(
            "`permissions.mode = manual` is no longer a setting, so it was removed from your settings; {}.",
            registry::retired_key("permissions.mode").unwrap()
        )]
    );
    assert_eq!(
        level(&store.load(None).unwrap().values).as_deref(),
        Some("full")
    );

    let temp = Temp::new("migrate-project");
    let store = temp.store();
    write(
        &store.path(Scope::Local),
        "[permissions]\nmode = \"manual\"\n",
    );
    let loaded = store.load(None).expect("loads");
    let notices = store.remove_retired(&loaded);
    assert!(
        notices[0].contains("is no longer a setting, so it was removed"),
        "{notices:?}"
    );
    let after = store.load(None).expect("loads");
    assert_eq!(level(&after.values), None);
    assert!(after.retired.is_empty());
    assert!(
        !store.path(Scope::Global).exists()
            || !read(&store.path(Scope::Global)).contains("sandbox")
    );
}

#[test]
fn the_global_file_holds_no_project_only_keys() {
    let temp = Temp::new("globalkeys");
    let store = temp.store();
    write(
        &store.path(Scope::Global),
        "[profiles.fast.model]\nparent = \"claude-opus-5\"\n",
    );
    let error = store.load(None).expect_err("no global profiles");
    assert!(
        error.contains("named profiles are not supported"),
        "{error}"
    );

    write(&store.path(Scope::Global), "[legacy]\nimported = true\n");
    let error = store.load(None).expect_err("project-only key");
    assert!(error.contains("belongs in its project file"), "{error}");
}

#[test]
fn scopes_parse_the_words_the_command_line_accepts() {
    assert_eq!(Scope::parse("global").expect("global"), Scope::Global);
    assert_eq!(Scope::parse("local").expect("local"), Scope::Local);
    assert_eq!(Scope::parse("project").expect("alias"), Scope::Local);
    let error = Scope::parse("machine").expect_err("unknown scope");
    assert!(error.contains("`global` or `local`"), "{error}");
    assert_eq!(Scope::Local.label(), "Project");
    assert_eq!(Scope::Global.name(), "global");
}

/// The supervisor is gone. A settings file that still configures it loads,
/// the keys come out of the file they were in, and the notice says once
/// where the job went.
#[test]
fn the_supervisors_settings_are_retired_with_one_notice() {
    let temp = Temp::new("retire-supervisor");
    let store = temp.store();
    write(
        &store.path(Scope::Global),
        "[supervisor]\nenabled = true\nevery = 4\nmodel = \"watcher\"\n\n[decisions]\nsupervision_above = 0.9\n",
    );
    let loaded = store.load(None).expect("a retired key never stops a load");
    let notices = store.remove_retired(&loaded);
    assert_eq!(notices.len(), 4, "{notices:?}");
    assert!(
        notices
            .iter()
            .all(|notice| notice.contains("the supervisor is gone")),
        "{notices:?}"
    );
    let saved = read(&store.path(Scope::Global));
    assert!(!saved.contains("supervis"), "{saved}");
    let after = store.load(None).expect("loads");
    assert!(after.retired.is_empty(), "said once: {:?}", after.retired);
}

/// The effort the model chooses for itself was saved as `default`; it is
/// `auto` now. A saved word is read as `auto`, rewritten in its file, and
/// said once.
#[test]
fn a_saved_default_effort_becomes_auto() {
    let temp = Temp::new("migrate-effort");
    let store = temp.store();
    write(
        &store.path(Scope::Global),
        "[session]\neffort = \"default\"\n",
    );
    let loaded = store.load(None).expect("loads");
    assert_eq!(
        string(&loaded.values, "session.effort").as_deref(),
        Some("auto")
    );
    let notices = store.remove_retired(&loaded);
    assert_eq!(
        notices,
        [
            "`session.effort = default` is now `\"auto\"`: the same setting under a clearer name. The model chooses its own effort."
        ]
    );
    let saved = read(&store.path(Scope::Global));
    assert!(!saved.contains("default"), "{saved}");
    let after = store.load(None).expect("loads");
    assert!(after.retired.is_empty(), "said once: {:?}", after.retired);
}
