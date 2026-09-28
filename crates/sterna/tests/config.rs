//! `docs/configuration.md`: `.glasshouse/pane.toml`, loaded once
//! at session start. Absent means every default the runtime already used;
//! anything present is validated with one sentence per
//! refusal.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use sterna::config::{AgentsMode, CompletionStyle, PreflightScope, SternaConfig};
use sterna::wire::Effort;

fn unique() -> u64 {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    NEXT.fetch_add(1, Ordering::Relaxed)
}

fn scratch_dir(label: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "sterna-config-test-{}-{}-{}",
        label,
        std::process::id(),
        unique()
    ));
    fs::create_dir_all(&dir).unwrap();
    dir
}

/// Points the global scope at an empty directory for as long as it is held,
/// and puts the environment back afterwards. `XDG_CONFIG_HOME` is process
/// state, so every test that needs it takes the same lock.
struct NoGlobalConfig {
    previous: Option<std::ffi::OsString>,
    _lock: std::sync::MutexGuard<'static, ()>,
}

impl NoGlobalConfig {
    fn new(root: &Path) -> Self {
        static ENV: std::sync::Mutex<()> = std::sync::Mutex::new(());
        let lock = ENV.lock().unwrap_or_else(|error| error.into_inner());
        let empty = root.join("no-global-config");
        fs::create_dir_all(&empty).unwrap();
        let previous = std::env::var_os("XDG_CONFIG_HOME");
        // SAFETY: the lock above makes this the only thread touching the
        // environment, which is what `set_var`'s contract asks for.
        unsafe { std::env::set_var("XDG_CONFIG_HOME", &empty) };
        Self {
            previous,
            _lock: lock,
        }
    }
}

impl Drop for NoGlobalConfig {
    fn drop(&mut self) {
        // SAFETY: as above -- the lock is still held until this struct is gone.
        unsafe {
            match self.previous.take() {
                Some(value) => std::env::set_var("XDG_CONFIG_HOME", value),
                None => std::env::remove_var("XDG_CONFIG_HOME"),
            }
        }
    }
}

fn write_project_toml(root: &Path, text: &str) {
    let dir = root.join(".sterna");
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("config.toml"), text).unwrap();
}

fn write_legacy_toml(root: &Path, text: &str) {
    let dir = root.join(".glasshouse");
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("pane.toml"), text).unwrap();
}

/// What `load` answers when *nothing* is configured -- no project file and no
/// global one. The global scope is `$XDG_CONFIG_HOME/sterna/config.toml`, so a
/// test that only makes an empty project root reads the configuration of the
/// machine it runs on and fails the day a developer has any.
#[test]
fn absent_sterna_toml_means_the_defaults() {
    let root = scratch_dir("absent");
    let _guard = NoGlobalConfig::new(&root);
    let config = SternaConfig::load(&root).unwrap();

    assert_eq!(config, SternaConfig::default());
    assert_eq!(config.limits.cell_wall_clock_s, 30);
    assert_eq!(config.limits.response_bytes, 16384);
    assert_eq!(
        config.limits.cells, None,
        "no ceiling unless this person sets one: a task ends on evidence that it stopped \
         producing anything, not on a count of cells"
    );
    assert!(!config.helpers.preflight);
}

/// The supervisor is gone, and a file that still configures it loads: its
/// keys are read as unset, never refused, and the rest of the file stands.
#[test]
fn a_file_that_still_configures_the_supervisor_loads() {
    let root = scratch_dir("supervisor-retired");
    write_legacy_toml(
        &root,
        "[supervisor]\nenabled = true\nevery = 4\nmodel = \"watcher\"\n\n[helpers]\nmodel = \"helper-tier\"\n",
    );
    let config = SternaConfig::load(&root).expect("a retired table never stops Sterna");
    assert_eq!(config.helpers.model.as_deref(), Some("helper-tier"));
}

#[test]
fn helper_preflight_is_an_explicit_boolean_opt_in() {
    let root = scratch_dir("preflight-on");
    write_legacy_toml(
        &root,
        "[helpers]\nmodel = \"helper-tier\"\npreflight = true\n",
    );
    assert!(SternaConfig::load(&root).unwrap().helpers.preflight);

    let root = scratch_dir("preflight-not-boolean");
    write_legacy_toml(&root, "[helpers]\npreflight = \"sometimes\"\n");
    let error = SternaConfig::load(&root).unwrap_err();
    assert!(error.contains("preflight"), "{error}");
    assert_eq!(error.lines().count(), 1);
}

/// `preflight_scope` defaults to `auto` -- direct execution is the fast path
/// and a scout runs from a signal -- and `always` is the pre-roadmap
/// behaviour, spelled out.
#[test]
fn helper_preflight_scope_defaults_to_auto_and_parses_always() {
    let root = scratch_dir("preflight-scope-default");
    assert_eq!(
        SternaConfig::load(&root).unwrap().helpers.preflight_scope,
        PreflightScope::Auto
    );

    let root = scratch_dir("preflight-scope-always");
    write_legacy_toml(
        &root,
        "[helpers]\nmodel = \"helper-tier\"\npreflight = true\npreflight_scope = \"always\"\n",
    );
    assert_eq!(
        SternaConfig::load(&root).unwrap().helpers.preflight_scope,
        PreflightScope::Always
    );

    // A word that is not a choice -- what an upgrade leaves behind -- does
    // not stop Sterna: it runs on the default and names what it dropped.
    let root = scratch_dir("preflight-scope-unknown");
    write_project_toml(&root, "[helpers]\npreflight_scope = \"sometimes\"\n");
    assert_eq!(
        SternaConfig::load(&root).unwrap().helpers.preflight_scope,
        PreflightScope::default()
    );
    assert_retired(&root, "helpers.preflight_scope", "sometimes");
}

/// The settings store reports `key = word` as a choice it dropped.
fn assert_retired(root: &Path, key: &str, word: &str) {
    let loaded = sterna::settings::Store::new(root)
        .unwrap()
        .load(None)
        .unwrap();
    assert!(
        loaded
            .retired
            .iter()
            .any(|retired| retired.key == key && retired.word == word),
        "{key} = {word} was not reported: {:?}",
        loaded.retired
    );
}

#[test]
fn helper_effort_has_role_defaults_and_accepts_partial_hard_overrides() {
    let defaults = SternaConfig::default().helpers.effort;
    assert_eq!(defaults.find, Effort::Low);
    assert_eq!(defaults.reduce, Effort::Medium);
    assert_eq!(defaults.check, Effort::Medium);

    let configured =
        SternaConfig::parse("[helpers.effort]\nfind = \"medium\"\ncheck = \"xhigh\"\n")
            .unwrap()
            .helpers
            .effort;
    assert_eq!(configured.find, Effort::Medium);
    assert_eq!(configured.reduce, Effort::Medium);
    assert_eq!(configured.check, Effort::Xhigh);
    assert_eq!(configured.for_helper("find"), Some(Effort::Medium));
    assert_eq!(configured.for_helper("unknown"), None);

    // A helper's effort is a hard value: `auto`, the model's own choice,
    // is refused, and the retired word `default` is no word at all.
    let error = SternaConfig::parse("[helpers.effort]\nreduce = \"auto\"\n").unwrap_err();
    assert!(error.contains("hard value"), "{error}");
    assert!(SternaConfig::parse("[helpers.effort]\nreduce = \"default\"\n").is_err());
}

/// A saved token cap is retired: read as unset, never refused, and it
/// configures no cap.
#[test]
fn a_saved_task_token_cap_loads_and_caps_nothing() {
    let root = scratch_dir("legacy-task-tokens");
    write_legacy_toml(&root, "[limits]\ntask_tokens = 1000\n");

    let config = SternaConfig::load(&root).unwrap();
    assert_eq!(config.limits, SternaConfig::default().limits);
}

#[test]
fn a_limit_outside_its_range_is_refused_with_one_sentence() {
    let root = scratch_dir("range");
    write_legacy_toml(&root, "[limits]\ncell_wall_clock_s = 0\n");

    let err = SternaConfig::load(&root).unwrap_err();
    assert!(err.contains("cell_wall_clock_s"), "{err}");
    assert!(err.contains('1') && err.contains("600"), "{err}");
    assert_eq!(err.lines().count(), 1, "refused with one sentence: {err}");
}

#[test]
fn an_unknown_key_is_refused() {
    let root = scratch_dir("unknown-key");
    write_legacy_toml(&root, "[limits]\nbogus = 1\n");
    let err = SternaConfig::load(&root).unwrap_err();
    assert!(err.contains("bogus"), "{err}");

    let root = scratch_dir("unknown-table");
    write_legacy_toml(&root, "[nope]\nx = 1\n");
    let err = SternaConfig::load(&root).unwrap_err();
    assert!(err.contains("nope"), "{err}");
}

#[test]
fn sterna_toml_names_no_tool_path_or_grant() {
    let root = scratch_dir("tool-name");
    write_legacy_toml(&root, "[decisions]\nmodel = \"grep\"\n");
    let err = SternaConfig::load(&root).unwrap_err();
    assert!(err.contains("names no tool, path or grant"), "{err}");

    let root = scratch_dir("path-like");
    write_legacy_toml(&root, "[decisions]\nmodel = \"../etc/passwd\"\n");
    let err = SternaConfig::load(&root).unwrap_err();
    assert!(err.contains("names no tool, path or grant"), "{err}");

    let namespaced = SternaConfig::parse(
        "[model]\nparent = \"vendor/model-302\"\n\n[helpers]\nmodel = \"vendor/helper-1\"\n",
    )
    .expect("provider-qualified catalogue ids are model ids, not paths");
    assert_eq!(namespaced.model.parent.as_deref(), Some("vendor/model-302"));
}

/// `[helpers] completion` -- the only thing that decides whether an accepted
/// task says anything at all. The default is silence: a line printed after
/// every task is a line nobody reads, so the recap is opt-in and its key
/// takes exactly two values.
#[test]
fn completion_parses_both_styles_and_defaults_to_silent() {
    let root = scratch_dir("completion-absent");
    let config = SternaConfig::load(&root).unwrap();
    assert_eq!(
        config.helpers.completion,
        CompletionStyle::Silent,
        "an absent key is silence, not a recap nobody asked for"
    );

    let root = scratch_dir("completion-silent");
    write_legacy_toml(&root, "[helpers]\ncompletion = \"silent\"\n");
    assert_eq!(
        SternaConfig::load(&root).unwrap().helpers.completion,
        CompletionStyle::Silent
    );

    let root = scratch_dir("completion-recap");
    write_legacy_toml(&root, "[helpers]\ncompletion = \"recap\"\n");
    assert_eq!(
        SternaConfig::load(&root).unwrap().helpers.completion,
        CompletionStyle::Recap
    );
}

/// A third completion style is not a choice: Sterna runs on the default and
/// names what it dropped. A value that is not even a word is still refused.
#[test]
fn a_third_completion_style_is_dropped_and_a_non_word_refused() {
    let root = scratch_dir("completion-bogus");
    write_project_toml(&root, "[helpers]\ncompletion = \"chatty\"\n");
    assert_eq!(
        SternaConfig::load(&root).unwrap().helpers.completion,
        CompletionStyle::default()
    );
    assert_retired(&root, "helpers.completion", "chatty");

    let root = scratch_dir("completion-not-a-string");
    write_legacy_toml(&root, "[helpers]\ncompletion = true\n");
    let err = SternaConfig::load(&root).unwrap_err();
    assert!(err.contains("completion"), "{err}");
    assert_eq!(err.lines().count(), 1, "refused with one sentence: {err}");
}

/// The three-tier plumbing: a frontier parent, a cheap helper, and a
/// separately chosen model for delegated goals.
///
/// Without `[agents] model` a subagent inherits the parent's model, so a
/// session driven by a frontier model pays frontier rates for every goal it
/// hands off unless the model remembers to name a cheaper one each time.
#[test]
fn agents_take_their_own_model_from_configuration() {
    let root = std::env::temp_dir().join(format!("sterna-agents-config-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join(".glasshouse")).unwrap();
    std::fs::write(
        root.join(".glasshouse/pane.toml"),
        "[helpers]\nmodel = \"gpt-5.6-luna\"\n\n[agents]\nmodel = \"claude-sonnet-5\"\n",
    )
    .unwrap();

    let config = sterna::config::SternaConfig::load(&root).expect("the file parses");
    assert_eq!(config.helpers.model.as_deref(), Some("gpt-5.6-luna"));
    assert_eq!(config.agents.model.as_deref(), Some("claude-sonnet-5"));
}

/// A project that configures nothing gets no agent default, which is the
/// previous behaviour exactly: the subagent inherits the parent.
#[test]
fn an_unconfigured_project_names_no_agent_model() {
    let root = std::env::temp_dir().join(format!("sterna-agents-none-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let config = sterna::config::SternaConfig::load(&root).expect("a missing file is the default");
    assert_eq!(config.agents.model, None);
}

/// A typo in the table is a startup error, not a silently ignored preference.
#[test]
fn an_unknown_agents_key_is_refused_by_name() {
    let root = std::env::temp_dir().join(format!("sterna-agents-typo-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join(".glasshouse")).unwrap();
    std::fs::write(
        root.join(".glasshouse/pane.toml"),
        "[agents]\nmodle = \"claude-sonnet-5\"\n",
    )
    .unwrap();
    let error = sterna::config::SternaConfig::load(&root).unwrap_err();
    assert!(error.contains("modle"), "{error}");
}

#[test]
fn agent_modes_parse_and_legacy_model_means_pinned() {
    let automatic = SternaConfig::parse("[agents]\nmode = \"auto\"\n").unwrap();
    assert_eq!(automatic.agents.mode, AgentsMode::Auto);
    assert_eq!(automatic.agents.model, None);

    let off = SternaConfig::parse("[agents]\nmode = \"off\"\n").unwrap();
    assert_eq!(off.agents.mode, AgentsMode::Off);
    assert_eq!(off.agents.model, None);

    let pinned =
        SternaConfig::parse("[agents]\nmode = \"pinned\"\nmodel = \"claude-sonnet-5\"\n").unwrap();
    assert_eq!(pinned.agents.mode, AgentsMode::Pinned);
    assert_eq!(pinned.agents.model.as_deref(), Some("claude-sonnet-5"));

    let legacy = SternaConfig::parse("[agents]\nmodel = \"legacy-model\"\n").unwrap();
    assert_eq!(legacy.agents.mode, AgentsMode::Pinned);
    assert_eq!(legacy.agents.model.as_deref(), Some("legacy-model"));
}

#[test]
fn contradictory_or_incomplete_agent_modes_are_refused() {
    for text in [
        "[agents]\nmode = \"pinned\"\n",
        "[agents]\nmode = \"auto\"\nmodel = \"some-model\"\n",
        "[agents]\nmode = \"off\"\nmodel = \"some-model\"\n",
        "[agents]\nmode = \"inherit\"\n",
    ] {
        assert!(SternaConfig::parse(text).is_err(), "accepted: {text}");
    }
}

#[test]
fn parent_and_helper_model_fields_require_concrete_ids() {
    for mode in ["auto", "off", "inherit"] {
        let parent = format!("[model]\nparent = \"{mode}\"\n");
        assert!(
            SternaConfig::parse(&parent).is_err(),
            "parent accepted {mode}"
        );

        let helper = format!("[helpers]\nmodel = \"{mode}\"\n");
        assert!(
            SternaConfig::parse(&helper).is_err(),
            "helper accepted {mode}"
        );
    }
}

/// `[helpers] acceptance_list` and `[helpers.effort] accept` (2026-09-14): on
/// by default with helpers, off by config, and the lister's effort is a hard
/// value like every helper's.
#[test]
fn the_acceptance_list_and_its_effort_are_configurable() {
    let config = sterna::config::SternaConfig::parse(
        "[helpers]\nmodel = \"m\"\nacceptance_list = true\n[helpers.effort]\naccept = \"medium\"\n",
    )
    .unwrap();
    assert!(config.helpers.acceptance_list);
    assert_eq!(
        config.helpers.effort.for_helper("accept"),
        Some(sterna::wire::Effort::Medium)
    );
    let defaults = sterna::config::SternaConfig::parse("[helpers]\nmodel = \"m\"\n").unwrap();
    assert!(
        !defaults.helpers.acceptance_list,
        "off by default since 2026-09-23: its derived items were the measured false alarms"
    );
    assert!(
        defaults.helpers.completion_check == sterna::config::CompletionCheck::Auto
            && defaults.helpers.learn,
        "the checker behind the answer runs after big work, and the learned notes are on, by default"
    );
    assert_eq!(
        defaults.helpers.effort.for_helper("accept"),
        Some(sterna::wire::Effort::Low)
    );
    let refused = sterna::config::SternaConfig::parse("[helpers]\nacceptance_list = \"yes\"\n");
    assert!(refused.is_err());
}
