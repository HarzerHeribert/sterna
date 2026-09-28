//! Sterna's own typed settings store -- `docs/configuration.md`.
//!
//! Three files, one precedence order, and one place that writes:
//!
//! ```text
//! built-in defaults -> global -> legacy -> project -> named profile -> CLI
//! ```
//!
//! *Global* is `$XDG_CONFIG_HOME/sterna/config.toml` (otherwise
//! `~/.config/sterna/config.toml`), resolved by the same
//! [`crate::project::workflows::user_directory`] that already selects global
//! instructions. *Project* is `<root>/.sterna/config.toml`, where `<root>` is
//! the folder Sterna started in or the explicit `--root`, Git repository or not.
//! *Legacy* is `<root>/.glasshouse/pane.toml`: still read, never written, and
//! visible in every load that uses it. The CLI layer is the caller's -- this
//! module stops at the profile.
//!
//! What this module will not do is as load-bearing as what it does:
//!
//! * It reads `.claude/settings.json` only when a person explicitly asks for
//!   an import, takes `permissions.allow`/`permissions.deny` from it and
//!   nothing else -- never hooks, environment or credentials -- and never
//!   writes that file back.
//! * It never widens a running sandbox. [`Store::permissions`] renders the
//!   native `[permissions]` tables for the existing profile compiler; a
//!   global denial survives every project overlay by union.
//! * It creates nothing while reading. A missing file is a missing file, and
//!   `mkdir` happens on the way to a write, never on the way to a read.
//! * It refuses a symbolic link on the path to a settings file, refuses an
//!   unknown key, and leaves every source document byte-identical when a
//!   validation, a conflict or a write fails.

pub mod registry;

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use toml_edit::{DocumentMut, InlineTable, Item, Table, Value};

use crate::config::{AgentsMode, CompletionStyle, SternaConfig};
use crate::sandbox::profile::Profile;

pub use registry::{
    Kind, SettingSpec, applies_now, check_value, hidden, live_command, permission_rule,
    shown_default, spec, specs, validate,
};

const LOCAL_DIR: &str = ".sterna";
const CONFIG_FILE: &str = "config.toml";
const LEGACY_DIR: &str = ".glasshouse";
const LEGACY_FILE: &str = "pane.toml";
const CLAUDE_DIR: &str = ".claude";
const CLAUDE_FILE: &str = "settings.json";
/// A settings file is a page of preferences, not a payload. The cap keeps a
/// pathological document out of memory and names itself in the refusal.
const MAX_BYTES: u64 = 256 * 1024;
/// The marker `import legacy` writes, after which the preserved legacy file
/// is no longer read and the native file is authoritative.
const MIGRATED: &str = "legacy.imported";
/// Said once, so every surface refuses the global scope in the same words.
const NO_GLOBAL: &str = "settings: this host has no user configuration directory (set XDG_CONFIG_HOME or HOME), so there is no global scope";

/// Which file an edit lands in. `Global` is this OS user's defaults, not a
/// machine-wide administrator setting; `Local` is this project folder alone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    Global,
    Local,
}

impl Scope {
    /// `global`, `local`, or `project` as the documented alias for `local`.
    pub fn parse(word: &str) -> Result<Self, String> {
        match word.trim() {
            "global" => Ok(Self::Global),
            "local" | "project" => Ok(Self::Local),
            other => Err(format!(
                "settings: scope must be `global` or `local` (`project` is an alias for `local`), not `{other}`"
            )),
        }
    }

    /// The word the CLI accepts.
    pub fn name(self) -> &'static str {
        match self {
            Self::Global => "global",
            Self::Local => "local",
        }
    }

    /// The word a panel tab shows.
    pub fn label(self) -> &'static str {
        match self {
            Self::Global => "Global",
            Self::Local => "Project",
        }
    }
}

/// One scope's file exactly as it was read: the bytes, and the values they
/// parse to. Saving takes one of these back, which is how an edit made
/// against a stale view is refused instead of overwriting the newer file.
#[derive(Debug, Clone)]
pub struct Snapshot {
    /// The document's verbatim text -- empty when the file does not exist.
    pub text: String,
    /// The same document as values; an empty table when the file is missing.
    pub values: toml::Value,
    /// Whether the file existed when this snapshot was taken.
    pub exists: bool,
    /// The file this snapshot came from, so a save cannot be handed the
    /// other scope's document by accident.
    pub path: PathBuf,
}

impl Default for Snapshot {
    fn default() -> Self {
        Self {
            text: String::new(),
            values: toml::Value::Table(toml::value::Table::new()),
            exists: false,
            path: PathBuf::new(),
        }
    }
}

/// The effective configuration, and the evidence for it.
#[derive(Debug, Clone)]
pub struct Loaded {
    /// The runtime configuration, parsed by `config.rs` and nothing else.
    pub config: SternaConfig,
    /// Every effective setting, runtime and presentation alike, as values.
    pub values: toml::Value,
    /// Dotted key -> where the effective value came from: `built-in`,
    /// `global`, `legacy`, `project`, `profile:<name>`, or `global+project`
    /// for a denial list both scopes contributed to.
    pub origins: BTreeMap<String, String>,
    /// Sentences a surface must show: a legacy fallback in use, an
    /// unavailable global directory, a profile masking the base scope.
    pub notices: Vec<String>,
    /// The named profile applied, if any.
    pub profile: Option<String>,
    /// Saved choices this version no longer offers -- what an upgrade leaves
    /// behind in a file. They were read as unset; a session removes them
    /// from the file it found them in.
    pub retired: Vec<Retired>,
}

/// One saved choice that is no longer a choice: where, which key, the word.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Retired {
    pub scope: Scope,
    pub key: String,
    pub word: String,
}

/// The two files, the project they belong to, and every verb over them.
#[derive(Debug, Clone)]
pub struct Store {
    root: PathBuf,
    global: Option<PathBuf>,
}

/// One pending change to a document: a full path (already including any
/// `profiles.<name>` prefix) and the typed value, or `None` to unset.
#[derive(Debug, Clone)]
struct Change {
    path: Vec<String>,
    value: Option<toml::Value>,
}

impl Store {
    /// The store for `root`, with the host-selected user configuration
    /// directory. A host with no such directory still gets a working local
    /// scope; only the global one is unavailable.
    pub fn new(root: &Path) -> Result<Self, String> {
        Self::with_global(root, crate::project::workflows::user_directory())
    }

    /// The same, with the global directory supplied -- what a test uses so
    /// that a run never reads or writes the developer's own settings.
    pub fn with_global(root: &Path, global_dir: Option<PathBuf>) -> Result<Self, String> {
        if root.as_os_str().is_empty() {
            return Err("settings: the project root is empty".to_string());
        }
        let anchored = if root.is_absolute() {
            root.to_path_buf()
        } else {
            std::env::current_dir()
                .map_err(|error| format!("settings: no working directory: {error}"))?
                .join(root)
        };
        let root = anchor_path(&anchored)?;
        let global = match global_dir {
            Some(dir) if dir.as_os_str().is_empty() => None,
            Some(dir) => Some(anchor_path(&dir)?),
            None => None,
        };
        Ok(Self { root, global })
    }

    /// The project folder these settings belong to.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Whether this host has a user configuration directory at all.
    pub fn global_available(&self) -> bool {
        self.global.is_some()
    }

    /// The global file, or `None` when the host offers no user directory.
    pub fn global_path(&self) -> Option<PathBuf> {
        self.global.as_ref().map(|dir| dir.join(CONFIG_FILE))
    }

    /// Where a scope's edits land. An unavailable global directory yields an
    /// empty path; [`Store::global_available`] is the question to ask first,
    /// and every read and write of that scope refuses in one sentence.
    pub fn path(&self, scope: Scope) -> PathBuf {
        match scope {
            Scope::Global => self.global_path().unwrap_or_default(),
            Scope::Local => self.root.join(LOCAL_DIR).join(CONFIG_FILE),
        }
    }

    /// The preserved pre-`.sterna` file. Read-only, always.
    pub fn legacy_path(&self) -> PathBuf {
        self.root.join(LEGACY_DIR).join(LEGACY_FILE)
    }

    /// `.claude/settings.json`, read only by an explicit import.
    pub fn claude_path(&self) -> PathBuf {
        self.root.join(CLAUDE_DIR).join(CLAUDE_FILE)
    }

    /// Loads every layer and merges them. `profile` selects a named overlay
    /// from the project (or legacy) document; the caller applies its own
    /// explicit CLI overrides on top of the result.
    pub fn load(&self, profile: Option<&str>) -> Result<Loaded, String> {
        let sources = self.read_sources()?;
        self.assemble(&sources, profile)
    }

    /// Takes every retired choice out of the file it was found in, so the
    /// next start is quiet and the file says what runs, and returns one
    /// sentence each for the person.
    pub fn remove_retired(&self, loaded: &Loaded) -> Vec<String> {
        loaded
            .retired
            .iter()
            .map(|retired| {
                // A value saved in a setting's old kind is rewritten in
                // place, in the file it was found in.
                if let Some(now) = registry::migrated_value(&retired.key, &retired.word) {
                    let written = self.read(retired.scope).and_then(|snapshot| {
                        self.save(
                            retired.scope,
                            &snapshot,
                            &[(retired.key.clone(), Some(now.to_string()))],
                        )
                    });
                    return registry::migration_notice(
                        &retired.key,
                        &retired.word,
                        now,
                        written.is_ok(),
                    );
                }
                // A retired permission word that chose a level is migrated,
                // globally and only where no level is saved yet: the file
                // keeps meaning what its owner chose. A project file's copy
                // is only removed, because the level is global only.
                let migrated = (retired.scope == Scope::Global)
                    .then(|| registry::migrated_level(&retired.key, &retired.word))
                    .flatten()
                    .filter(|_| {
                        crate::settings_session::value(&loaded.values, "sandbox.level").is_none()
                    })
                    // A saved rung beside `full_access` decided the old
                    // session's level (the rung won), so it decides this one.
                    .filter(|_| {
                        retired.key != "permissions.full_access"
                            || !loaded.retired.iter().any(|other| {
                                other.scope == Scope::Global
                                    && registry::migrated_level(&other.key, &other.word).is_some()
                                    && other.key == "permissions.mode"
                            })
                    });
                let mut changes = vec![(retired.key.clone(), None)];
                if let Some(level) = migrated {
                    changes.push(("sandbox.level".to_string(), Some(level.to_string())));
                }
                let removed = self
                    .read(retired.scope)
                    .and_then(|snapshot| self.save(retired.scope, &snapshot, &changes));
                if let Some(level) = migrated.filter(|_| removed.is_ok()) {
                    return format!(
                        "`{} = {}` is now `sandbox.level = \"{level}\"`; /sandbox changes it.",
                        retired.key, retired.word,
                    );
                }
                if let Some(instead) = registry::retired_key(&retired.key) {
                    return format!(
                        "`{} = {}` is no longer a setting{}; {instead}.",
                        retired.key,
                        retired.word,
                        if removed.is_ok() {
                            ", so it was removed from your settings"
                        } else {
                            ""
                        },
                    );
                }
                let now = retired
                    .key
                    .split('.')
                    .try_fold(&loaded.values, |value, part| value.get(part))
                    .and_then(toml::Value::as_str)
                    .map(str::to_string)
                    .or_else(|| shown_default(&retired.key))
                    .unwrap_or_else(|| "its default".into());
                format!(
                    "`{} = {}` is no longer a choice{}; Sterna uses `{now}`. /settings changes it.",
                    retired.key,
                    retired.word,
                    if removed.is_ok() {
                        ", so it was removed from your settings"
                    } else {
                        ""
                    },
                )
            })
            .collect()
    }

    /// One scope's file, exactly as it is on disk. A missing file is an empty
    /// snapshot, and nothing is created.
    pub fn read(&self, scope: Scope) -> Result<Snapshot, String> {
        if scope == Scope::Global && self.global.is_none() {
            return Err(NO_GLOBAL.to_string());
        }
        let path = self.path(scope);
        let text = match self.directory(scope, false)? {
            Some(directory) => directory.read(CONFIG_FILE, MAX_BYTES)?,
            None => None,
        };
        let exists = text.is_some();
        let text = text.unwrap_or_default();
        let values = if text.trim().is_empty() {
            toml::Value::Table(toml::value::Table::new())
        } else {
            toml::from_str::<toml::Value>(&text)
                .map_err(|error| format!("settings: {}: {error}", path.display()))?
        };
        Ok(Snapshot {
            text,
            values,
            exists,
            path,
        })
    }

    /// Applies `edits` to the base scope and returns the new effective
    /// configuration. `None` unsets a key; every key is registry-validated
    /// before anything is written, and nothing is written when any of them
    /// fails.
    pub fn save(
        &self,
        scope: Scope,
        snapshot: &Snapshot,
        edits: &[(String, Option<String>)],
    ) -> Result<Loaded, String> {
        self.save_profile(scope, snapshot, edits, None)
    }

    /// The same write, targeting a named profile's overlay instead of the
    /// base scope -- what `/models` needs when a profile is selected, and
    /// what `/settings` deliberately does not use for its Global/Project
    /// tabs. A profile overlays runtime keys only.
    pub fn save_profile(
        &self,
        scope: Scope,
        snapshot: &Snapshot,
        edits: &[(String, Option<String>)],
        profile: Option<&str>,
    ) -> Result<Loaded, String> {
        if let Some(name) = profile {
            check_profile_name(name)?;
            if scope == Scope::Global {
                return Err(format!(
                    "settings: named profiles live in {}; the global file has no [profiles] table yet",
                    self.path(Scope::Local).display()
                ));
            }
        }
        let mut changes = Vec::with_capacity(edits.len());
        for (key, value) in edits {
            // Said here rather than only at load, so a person who types the
            // write is told now instead of discovering later that their file
            // is being ignored.
            if scope == Scope::Local && registry::is_global_only(key) {
                return Err(format!(
                    "settings: `{key}` is a global setting only. A project file travels inside a \
                     repository, so a project-scoped `{key}` would let a clone change it before \
                     you had read a line of the code. Set it in `{}` instead.",
                    self.path(Scope::Global).display()
                ));
            }
            if profile.is_some() && !registry::is_runtime(key) {
                return Err(format!(
                    "settings: `{key}` is not a runtime setting; a profile overlays only \
                     [limits], [supervisor], [helpers], [agents], [model] and [web]"
                ));
            }
            let typed = match value {
                Some(text) => Some(registry::validate(key, text)?),
                // An unset still names a supported key: removing a key Sterna
                // does not have would quietly "succeed" on a typo.
                None => {
                    if registry::retired_key(key).is_none() {
                        registry::spec(key).ok_or_else(|| {
                            format!("settings: `{key}` is not a setting Sterna supports")
                        })?;
                    }
                    None
                }
            };
            let mut path: Vec<String> = Vec::new();
            if let Some(name) = profile {
                path.push("profiles".to_string());
                path.push(name.to_string());
            }
            path.extend(key.split('.').map(str::to_string));
            changes.push(Change { path, value: typed });
        }
        self.write(scope, snapshot, changes, profile, false)
    }

    /// The native `[permissions]` tables rendered as the `settings.json`
    /// document the existing profile compiler already reads, or `None` when
    /// no scope defines any. `.claude/settings.json` is never consulted here:
    /// a Claude permission reaches Sterna only through an explicit import.
    ///
    /// A project overlay may replace the allow list -- narrowing is a project
    /// decision -- but denials are unioned, so a global denial survives every
    /// overlay and every profile.
    pub fn permissions(&self) -> Result<Option<String>, String> {
        let loaded = self.load(None)?;
        let allow = string_list(&loaded.values, "permissions", "allow");
        let deny = string_list(&loaded.values, "permissions", "deny");
        if allow.is_none() && deny.is_none() {
            return Ok(None);
        }
        let allow = allow.unwrap_or_default();
        let deny = deny.unwrap_or_default();
        self.check_rules(allow.iter().chain(deny.iter()))?;
        let document = serde_json::json!({
            "permissions": { "allow": allow, "deny": deny }
        });
        let text = serde_json::to_string_pretty(&document)
            .map_err(|error| format!("settings: could not render permissions: {error}"))?;
        Ok(Some(text))
    }

    /// Imports from `claude` (`.claude/settings.json`, permissions only) or
    /// `legacy` (`.glasshouse/pane.toml`) into the project file.
    ///
    /// A dry run is the default and the only thing `apply = false` does is
    /// describe: no directory, no lock file and no document is created. Both
    /// sources are left byte-identical either way, and only non-conflicting
    /// keys are merged -- a value the project file already sets differently
    /// is reported and kept.
    pub fn import(&self, source: &str, apply: bool) -> Result<String, String> {
        match source.trim() {
            "claude" => self.import_claude(apply),
            "legacy" => self.import_legacy(apply),
            other => Err(format!(
                "settings: unknown import source `{other}`; use `claude` or `legacy`"
            )),
        }
    }

    // -- loading ---------------------------------------------------------

    fn read_sources(&self) -> Result<Sources, String> {
        let global = match (self.global.is_some(), self.directory(Scope::Global, false)) {
            (true, Ok(Some(directory))) => directory
                .read(CONFIG_FILE, MAX_BYTES)?
                .map(|text| (self.path(Scope::Global), text)),
            (true, Err(error)) => return Err(error),
            _ => None,
        };
        let local = match self.directory(Scope::Local, false)? {
            Some(directory) => directory
                .read(CONFIG_FILE, MAX_BYTES)?
                .map(|text| (self.path(Scope::Local), text)),
            None => None,
        };
        let legacy = match files::Directory::open(&self.root, &[LEGACY_DIR], false)? {
            Some(directory) => directory
                .read(LEGACY_FILE, MAX_BYTES)?
                .map(|text| (self.legacy_path(), text)),
            None => None,
        };
        Ok(Sources {
            global,
            local,
            legacy,
        })
    }

    /// The directory a scope's file lives in, opened one component at a time
    /// and never through a symbolic link. `None` means it does not exist and
    /// we are not on our way to a write, which is how reading creates nothing.
    fn directory(&self, scope: Scope, create: bool) -> Result<Option<files::Directory>, String> {
        match scope {
            Scope::Local => files::Directory::open(&self.root, &[LOCAL_DIR], create),
            Scope::Global => {
                let base = self.global.as_ref().ok_or_else(|| NO_GLOBAL.to_string())?;
                files::Directory::open(base, &[], create)
            }
        }
    }

    fn assemble(&self, sources: &Sources, profile: Option<&str>) -> Result<Loaded, String> {
        let mut notices = Vec::new();
        let global = match &sources.global {
            Some((path, text)) => parse_document(path, text, Scope::Global)?,
            None => Parsed::default(),
        };
        if self.global.is_none() {
            notices.push(
                "No user configuration directory (XDG_CONFIG_HOME or HOME); global settings are unavailable."
                    .to_string(),
            );
        }
        let local = match &sources.local {
            Some((path, text)) => parse_document(path, text, Scope::Local)?,
            None => Parsed::default(),
        };
        let migrated = matches!(local.flat.get(MIGRATED), Some(toml::Value::Boolean(true)));

        let legacy = match (&sources.legacy, migrated) {
            (Some((path, _)), true) => {
                notices.push(format!(
                    "`{}` is preserved but no longer read: it was imported into `{}`.",
                    path.display(),
                    self.path(Scope::Local).display()
                ));
                Parsed::default()
            }
            (Some((path, text)), false) => {
                let parsed = parse_document(path, text, Scope::Local)?;
                // This file is preserved, never rewritten, so a retired choice
                // in it is said rather than removed.
                for (key, word) in &parsed.retired {
                    notices.push(match registry::migrated_value(key, word) {
                        Some(now) => format!(
                            "{} (in `{}`, which is preserved as it is)",
                            registry::migration_notice(key, word, now, false),
                            path.display()
                        ),
                        None => format!(
                            "`{key} = {word}` in `{}` is no longer a choice; Sterna uses its default.",
                            path.display()
                        ),
                    });
                }
                let mut conflicts: Vec<String> = Vec::new();
                for (key, value) in &parsed.flat {
                    if local.flat.get(key).is_some_and(|saved| saved != value) {
                        conflicts.push(key.clone());
                    }
                }
                if !conflicts.is_empty() {
                    return Err(format!(
                        "settings: `{}` and `{}` disagree about {}. Sterna will not guess which runtime configuration you meant: run `sterna config import legacy` to see the merge, then `--apply` it. The legacy file is preserved either way.",
                        path.display(),
                        self.path(Scope::Local).display(),
                        conflicts
                            .iter()
                            .map(|key| format!("`{key}`"))
                            .collect::<Vec<_>>()
                            .join(", ")
                    ));
                }
                notices.push(format!(
                    "`{}` is a read-only legacy fallback; `sterna config import legacy` migrates it to `{}`.",
                    path.display(),
                    self.path(Scope::Local).display()
                ));
                parsed
            }
            (None, _) => Parsed::default(),
        };

        let mut map: BTreeMap<String, toml::Value> = BTreeMap::new();
        let mut origins: BTreeMap<String, String> = BTreeMap::new();
        for (key, value) in defaults() {
            map.insert(key.to_string(), value);
            origins.insert(key.to_string(), "built-in".to_string());
        }
        for (label, parsed) in [
            ("global", &global),
            ("legacy", &legacy),
            ("project", &local),
        ] {
            // `config.rs`' rule for a profile overlay, which a lower scope
            // needs just as much: a layer that says `mode = "off"` (or
            // `"auto"`) and names no model drops the model it inherited,
            // rather than composing into a configuration -- pinned to a model
            // while refusing every spawn -- that no file asked for and the
            // parser rightly refuses.
            let clears_model = parsed
                .flat
                .get("agents.mode")
                .and_then(toml::Value::as_str)
                .is_some_and(|mode| matches!(mode, "off" | "auto" | "roster"))
                && !parsed.flat.contains_key("agents.model");
            if clears_model {
                map.remove("agents.model");
                origins.remove("agents.model");
            }
            for (key, value) in &parsed.flat {
                // The enforcing half of the same rule: a hand-written or
                // cloned project document never reaches `save`, so refusing
                // the write alone would guard nothing. Ignored rather than
                // refused -- a repository that cannot disarm a reader should
                // also not be able to stop them opening it.
                if label != "global" && registry::is_global_only(key) {
                    notices.push(format!(
                        "`{key}` is a global setting only and was ignored in `{}`; set it in `{}`.",
                        match label {
                            "legacy" => self.legacy_path(),
                            _ => self.path(Scope::Local),
                        }
                        .display(),
                        self.path(Scope::Global).display()
                    ));
                    continue;
                }
                if key == "permissions.deny" {
                    let merged = union_lists(map.get(key), value);
                    let origin = match origins.get(key) {
                        Some(previous) if previous != label => format!("{previous}+{label}"),
                        Some(previous) => previous.clone(),
                        None => label.to_string(),
                    };
                    map.insert(key.clone(), merged);
                    origins.insert(key.clone(), origin);
                    continue;
                }
                map.insert(key.clone(), value.clone());
                origins.insert(key.clone(), label.to_string());
            }
        }

        let permission_patterns: Vec<String> = ["permissions.allow", "permissions.deny"]
            .into_iter()
            .filter_map(|key| map.get(key).and_then(toml::Value::as_array))
            .flatten()
            .filter_map(toml::Value::as_str)
            .map(str::to_owned)
            .collect();
        self.check_rules(permission_patterns.iter())?;

        // The runtime half is parsed by `config.rs`, profile overlay and all:
        // one parser owns every range, model rule and mode combination.
        //
        // Built-in defaults are shown to a panel but never written into that
        // document. `[agents]` infers `pinned` from a lone `model`, so a
        // default `mode = "auto"` standing beside an inherited model would
        // turn an existing project's configuration into a contradiction it
        // never wrote.
        let mut document = nest(map.iter().filter(|(key, _)| {
            registry::is_runtime(key.as_str())
                && origins.get(key.as_str()).map(String::as_str) != Some("built-in")
        }));
        let overlay = match profile {
            None => None,
            Some(name) => {
                check_profile_name(name)?;
                let from_local = local.profiles.get(name);
                let from_legacy = legacy.profiles.get(name);
                let overlay = match (from_local, from_legacy) {
                    (Some(_), Some(_)) => {
                        return Err(format!(
                            "settings: profile `{name}` is defined in both `{}` and `{}`; migrate the legacy file before selecting it",
                            self.path(Scope::Local).display(),
                            self.legacy_path().display()
                        ));
                    }
                    (Some(overlay), None) | (None, Some(overlay)) => overlay,
                    (None, None) => {
                        return Err(format!("settings: no profile named `{name}`"));
                    }
                };
                if let Some(table) = document.as_table_mut() {
                    let mut profiles = toml::value::Table::new();
                    profiles.insert(name.to_string(), nest(overlay.iter()));
                    table.insert("profiles".to_string(), toml::Value::Table(profiles));
                }
                Some((name.to_string(), overlay.clone()))
            }
        };
        let text = toml::to_string(&document).map_err(|error| {
            format!("settings: could not render the effective document: {error}")
        })?;
        let config = SternaConfig::parse_profile(&text, profile).map_err(|error| {
            format!(
                "settings: {}",
                error.strip_prefix("config.toml: ").unwrap_or(&error)
            )
        })?;

        if let Some((name, overlay)) = &overlay {
            // `config.rs`' own rule, mirrored so `values` and `config` cannot
            // disagree: a profile that turns subagents off or back to `auto`
            // drops the base scope's pinned model rather than colliding.
            let clears_model = overlay
                .get("agents.mode")
                .and_then(toml::Value::as_str)
                .is_some_and(|mode| matches!(mode, "off" | "auto" | "roster"))
                && !overlay.contains_key("agents.model");
            if clears_model {
                map.remove("agents.model");
                origins.remove("agents.model");
            }
            for (key, value) in overlay {
                map.insert(key.clone(), value.clone());
                origins.insert(key.clone(), format!("profile:{name}"));
            }
            notices.push(format!(
                "Profile `{name}` is applied over the saved scopes; edits in the Global and Project tabs do not change it."
            ));
        }

        // `[agents] model` without a mode *is* `pinned` (`config.rs`), so the
        // row a panel shows says so rather than the default it displaced.
        if map.contains_key("agents.model")
            && origins.get("agents.mode").map(String::as_str) == Some("built-in")
        {
            map.insert(
                "agents.mode".to_string(),
                toml::Value::String("pinned".to_string()),
            );
            if let Some(origin) = origins.get("agents.model").cloned() {
                origins.insert("agents.mode".to_string(), origin);
            }
        }

        let retired: Vec<Retired> = [(Scope::Global, &global), (Scope::Local, &local)]
            .into_iter()
            .flat_map(|(scope, parsed)| {
                parsed.retired.iter().map(move |(key, word)| Retired {
                    scope,
                    key: key.clone(),
                    word: word.clone(),
                })
            })
            .collect();
        Ok(Loaded {
            config,
            values: nest(map.iter()),
            origins,
            notices,
            profile: profile.map(str::to_string),
            retired,
        })
    }

    // -- writing ---------------------------------------------------------

    fn write(
        &self,
        scope: Scope,
        snapshot: &Snapshot,
        changes: Vec<Change>,
        profile: Option<&str>,
        migrating: bool,
    ) -> Result<Loaded, String> {
        if scope == Scope::Global && self.global.is_none() {
            return Err(NO_GLOBAL.to_string());
        }
        let path = self.path(scope);
        // A snapshot from the other tab would silently move an edit between
        // scopes, which is the one thing the tabs promise not to -- and a
        // default-constructed snapshot would claim a base document nobody
        // read. Both are refused, with no exception for an empty one.
        if snapshot.path != path {
            return Err(format!(
                "settings: this snapshot was read from `{}`, not `{}`; read the scope you are saving",
                snapshot.path.display(),
                path.display()
            ));
        }
        if changes.is_empty() {
            return self.load(profile);
        }
        if scope == Scope::Local && !migrating {
            self.refuse_pending_migration()?;
        }

        // Optimistic concurrency, before anything on disk is touched.
        let current = match self.directory(scope, false)? {
            Some(directory) => directory.read(CONFIG_FILE, MAX_BYTES)?,
            None => None,
        };
        if current.is_some() != snapshot.exists
            || current.as_deref().unwrap_or_default() != snapshot.text
        {
            return Err(format!(
                "settings: `{}` changed since it was read; reload and reapply the edit",
                path.display()
            ));
        }

        let candidate = apply_changes(&snapshot.text, &changes)?;
        // Validate the whole effective configuration -- not just the edited
        // keys -- with the candidate standing in for its scope. A failure
        // here has touched no file at all.
        let mut sources = self.read_sources()?;
        let slot = match scope {
            Scope::Global => &mut sources.global,
            Scope::Local => &mut sources.local,
        };
        *slot = Some((path.clone(), candidate.clone()));
        self.assemble(&sources, None)?;
        if let Some(name) = profile {
            self.assemble(&sources, Some(name))?;
        }

        // Every step from here on is relative to one held directory
        // descriptor: the conflict re-check, the lock, the temporary and the
        // rename all name the directory Sterna opened, not a path another
        // process can re-point between two calls.
        let directory = self
            .directory(scope, true)?
            .ok_or_else(|| format!("settings: could not open `{}`", path.display()))?;
        let _lock = directory.lock(CONFIG_FILE)?;
        // Re-check under the lock: another writer may have won the race
        // between the check above and this line.
        let current = directory.read(CONFIG_FILE, MAX_BYTES)?;
        if current.is_some() != snapshot.exists
            || current.as_deref().unwrap_or_default() != snapshot.text
        {
            return Err(format!(
                "settings: `{}` changed since it was read; reload and reapply the edit",
                path.display()
            ));
        }
        directory.write(CONFIG_FILE, &candidate)?;
        self.load(profile)
    }

    fn refuse_pending_migration(&self) -> Result<(), String> {
        let legacy = self.legacy_path();
        let present = match files::Directory::open(&self.root, &[LEGACY_DIR], false)? {
            Some(directory) => directory.read(LEGACY_FILE, MAX_BYTES)?.is_some(),
            None => false,
        };
        if !present {
            return Ok(());
        }
        let local = self.read(Scope::Local)?;
        let migrated = local
            .values
            .get("legacy")
            .and_then(|table| table.get("imported"))
            .and_then(toml::Value::as_bool)
            .unwrap_or(false);
        if migrated {
            return Ok(());
        }
        Err(format!(
            "settings: `{}` still holds this project's settings. Run `sterna config import legacy` to preview the migration and `--apply` it; saving into `{}` first would leave two files disagreeing about the same project.",
            legacy.display(),
            self.path(Scope::Local).display()
        ))
    }

    // -- imports ---------------------------------------------------------

    fn import_claude(&self, apply: bool) -> Result<String, String> {
        let path = self.claude_path();
        let source = match files::Directory::open(&self.root, &[CLAUDE_DIR], false)? {
            Some(directory) => directory.read(CLAUDE_FILE, MAX_BYTES)?,
            None => None,
        };
        let Some(text) = source else {
            return Err(format!(
                "settings: nothing to import: `{}` does not exist",
                path.display()
            ));
        };
        let document: serde_json::Value = serde_json::from_str(&text)
            .map_err(|error| format!("settings: {}: {error}", path.display()))?;
        let object = document
            .as_object()
            .ok_or_else(|| format!("settings: {}: not a JSON object", path.display()))?;

        let mut ignored: Vec<String> = Vec::new();
        let mut rejected: Vec<String> = Vec::new();
        let mut notes: Vec<String> = Vec::new();
        for key in object.keys() {
            if key != "permissions" {
                ignored.push(key.clone());
            }
        }
        let permissions = object
            .get("permissions")
            .and_then(serde_json::Value::as_object);
        if object.contains_key("permissions") && permissions.is_none() {
            rejected.push("`permissions` is not an object".into());
        }
        let mut source_allow: Vec<String> = Vec::new();
        let mut source_deny: Vec<String> = Vec::new();
        if let Some(permissions) = permissions {
            for (key, value) in permissions {
                let target = match key.as_str() {
                    "allow" => &mut source_allow,
                    "deny" => &mut source_deny,
                    other => {
                        rejected.push(format!(
                            "unsupported security directive `permissions.{other}`"
                        ));
                        continue;
                    }
                };
                let Some(entries) = value.as_array() else {
                    rejected.push(format!("`permissions.{key}` is not an array"));
                    continue;
                };
                for entry in entries {
                    match entry.as_str() {
                        Some(pattern) => target.push(pattern.to_string()),
                        None => rejected.push(format!(
                            "a non-string entry in `permissions.{key}` grants nothing"
                        )),
                    }
                }
            }
        }
        let keep = |patterns: Vec<String>, rejected: &mut Vec<String>| -> Vec<String> {
            patterns
                .into_iter()
                .filter(|pattern| match self.check_rules(std::iter::once(pattern)) {
                    Ok(()) => true,
                    Err(reason) => {
                        rejected.push(reason);
                        false
                    }
                })
                .collect()
        };
        let source_deny = keep(source_deny, &mut rejected);
        let source_allow = keep(source_allow, &mut rejected);

        let snapshot = self.read(Scope::Local)?;
        let existing_allow =
            string_list(&snapshot.values, "permissions", "allow").unwrap_or_default();
        let existing_deny =
            string_list(&snapshot.values, "permissions", "deny").unwrap_or_default();
        let effective_deny: BTreeSet<String> = match self.load(None) {
            Ok(loaded) => string_list(&loaded.values, "permissions", "deny")
                .unwrap_or_default()
                .into_iter()
                .collect(),
            // A project that cannot load yet still deserves a preview; the
            // project file's own denials are the ones that matter here.
            Err(_) => existing_deny.iter().cloned().collect(),
        };

        let mut allow = existing_allow.clone();
        let mut deny = existing_deny.clone();
        for pattern in &source_deny {
            if !deny.contains(pattern) {
                deny.push(pattern.clone());
            }
        }
        for pattern in &source_allow {
            if source_deny.contains(pattern) {
                notes.push(format!(
                    "`{pattern}` is denied by the same document; a denial beats every allow, so it was not imported as one"
                ));
                continue;
            }
            if effective_deny.contains(pattern) || deny.contains(pattern) {
                notes.push(format!(
                    "`{pattern}` is already denied by your settings; the allow was not imported"
                ));
                continue;
            }
            if allow.contains(pattern) {
                continue;
            }
            allow.push(pattern.clone());
        }

        let mut changes = Vec::new();
        if allow != existing_allow {
            changes.push(Change {
                path: vec!["permissions".into(), "allow".into()],
                value: Some(list_value(&allow)),
            });
        }
        if deny != existing_deny {
            changes.push(Change {
                path: vec!["permissions".into(), "deny".into()],
                value: Some(list_value(&deny)),
            });
        }

        let mut report = String::new();
        report.push_str(&format!(
            "Import permissions from `{}` into `{}`{}\n",
            path.display(),
            self.path(Scope::Local).display(),
            if apply {
                ""
            } else {
                " (preview; nothing was written)"
            }
        ));
        let added_allow: Vec<&String> = allow
            .iter()
            .filter(|p| !existing_allow.contains(p))
            .collect();
        let added_deny: Vec<&String> = deny.iter().filter(|p| !existing_deny.contains(p)).collect();
        push_list(&mut report, "permissions.allow +=", &added_allow);
        push_list(&mut report, "permissions.deny  +=", &added_deny);
        if !ignored.is_empty() {
            report.push_str(&format!(
                "  never imported: {} (hooks, environment and credentials stay where they are)\n",
                ignored.join(", ")
            ));
        }
        for note in &notes {
            report.push_str(&format!("  note: {note}\n"));
        }
        for reason in &rejected {
            report.push_str(&format!("  rejected: {reason}\n"));
        }
        if apply && !rejected.is_empty() {
            // A rejected rule is a rule that would have granted or -- worse --
            // denied something and does not. Applying the rest would quietly
            // ship a permission set nobody wrote, so the import stops and the
            // document stays exactly as it is.
            report.push_str(
                "Refused: nothing was written. Fix or remove the rejected entries above and import again.\n",
            );
            return Err(report);
        }
        if changes.is_empty() {
            report.push_str("  nothing to import; the project file already says this.\n");
            return Ok(report);
        }
        if !apply {
            report.push_str("Run the same command with `--apply` to write it.\n");
            return Ok(report);
        }
        self.write(Scope::Local, &snapshot, changes, None, false)?;
        report.push_str(&format!(
            "Written. `{}` was not modified, and no permission is in force until a new session compiles it.\n",
            path.display()
        ));
        Ok(report)
    }

    fn import_legacy(&self, apply: bool) -> Result<String, String> {
        let path = self.legacy_path();
        let source = match files::Directory::open(&self.root, &[LEGACY_DIR], false)? {
            Some(directory) => directory.read(LEGACY_FILE, MAX_BYTES)?,
            None => None,
        };
        let Some(text) = source else {
            return Err(format!(
                "settings: nothing to import: `{}` does not exist",
                path.display()
            ));
        };
        let legacy = parse_document(&path, &text, Scope::Local)?;
        let snapshot = self.read(Scope::Local)?;
        let local = parse_document(&snapshot.path, &snapshot.text, Scope::Local)?;

        let mut changes = Vec::new();
        let mut imported: Vec<String> = Vec::new();
        let mut kept: Vec<String> = Vec::new();
        for (key, value) in &legacy.flat {
            if key == MIGRATED {
                continue;
            }
            match local.flat.get(key) {
                Some(existing) if existing == value => {}
                Some(existing) => kept.push(format!(
                    "`{key}`: the project file keeps {}, the legacy file's {} was not imported",
                    render(existing),
                    render(value)
                )),
                None => {
                    imported.push(format!("{key} = {}", render(value)));
                    changes.push(Change {
                        path: key.split('.').map(str::to_string).collect(),
                        value: Some(value.clone()),
                    });
                }
            }
        }
        for (name, overlay) in &legacy.profiles {
            if local.profiles.contains_key(name) {
                kept.push(format!(
                    "profile `{name}` already exists in the project file; the legacy one was not imported"
                ));
                continue;
            }
            for (key, value) in overlay {
                imported.push(format!("profiles.{name}.{key} = {}", render(value)));
                let mut path = vec!["profiles".to_string(), name.clone()];
                path.extend(key.split('.').map(str::to_string));
                changes.push(Change {
                    path,
                    value: Some(value.clone()),
                });
            }
        }
        changes.push(Change {
            path: MIGRATED.split('.').map(str::to_string).collect(),
            value: Some(toml::Value::Boolean(true)),
        });

        let mut report = String::new();
        report.push_str(&format!(
            "Migrate `{}` into `{}`{}\n",
            path.display(),
            self.path(Scope::Local).display(),
            if apply {
                ""
            } else {
                " (preview; nothing was written)"
            }
        ));
        let imported_refs: Vec<&String> = imported.iter().collect();
        push_list(&mut report, "import:", &imported_refs);
        for line in &kept {
            report.push_str(&format!("  conflict: {line}\n"));
        }
        report.push_str(&format!(
            "  set: {MIGRATED} = true -- the legacy file is preserved and no longer read.\n"
        ));
        if !apply {
            report.push_str("Run the same command with `--apply` to write it.\n");
            return Ok(report);
        }
        self.write(Scope::Local, &snapshot, changes, None, true)?;
        report.push_str(&format!(
            "Written. `{}` was not modified.\n",
            path.display()
        ));
        Ok(report)
    }

    // -- helpers ---------------------------------------------------------

    /// Compiles patterns the way a session will, so a rule that grants
    /// nothing is refused at the point a person can still fix it.
    fn check_rules<'a>(&self, patterns: impl Iterator<Item = &'a String>) -> Result<(), String> {
        let patterns: Vec<&String> = patterns.collect();
        if patterns.is_empty() {
            return Ok(());
        }
        for pattern in &patterns {
            registry::permission_rule(pattern.as_str())?;
        }
        let document = serde_json::json!({ "permissions": { "allow": patterns } });
        let profile = Profile::compile(&self.root, Some(&document.to_string()));
        for diagnostic in profile.diagnostics() {
            if let Some(pattern) = patterns
                .iter()
                .find(|pattern| diagnostic.contains(pattern.as_str()))
            {
                return Err(format!("settings: `{pattern}`: {diagnostic}"));
            }
        }
        Ok(())
    }
}

#[derive(Debug, Default)]
struct Sources {
    global: Option<(PathBuf, String)>,
    local: Option<(PathBuf, String)>,
    legacy: Option<(PathBuf, String)>,
}

/// One document's leaves, and its named profiles' leaves.
#[derive(Debug, Default, Clone)]
struct Parsed {
    flat: BTreeMap<String, toml::Value>,
    profiles: BTreeMap<String, BTreeMap<String, toml::Value>>,
    /// Keys whose saved word is a choice this version no longer has, with it.
    retired: Vec<(String, String)>,
}

fn parse_document(path: &Path, text: &str, scope: Scope) -> Result<Parsed, String> {
    let value: toml::Value =
        toml::from_str(text).map_err(|error| format!("settings: {}: {error}", path.display()))?;
    let table = value
        .as_table()
        .ok_or_else(|| format!("settings: {}: must be a table", path.display()))?;
    let mut parsed = Parsed::default();
    for (key, value) in table {
        if key == "profiles" {
            if scope == Scope::Global {
                return Err(format!(
                    "settings: {}: named profiles are not supported in the global file; keep [profiles] in the project file",
                    path.display()
                ));
            }
            let profiles = value.as_table().ok_or_else(|| {
                format!("settings: {}: [profiles] must be a table", path.display())
            })?;
            for (name, overlay) in profiles {
                check_profile_name(name)
                    .map_err(|error| format!("{error} ({})", path.display()))?;
                let overlay = overlay.as_table().ok_or_else(|| {
                    format!(
                        "settings: {}: profile `{name}` must be a table",
                        path.display()
                    )
                })?;
                let mut flat = BTreeMap::new();
                flatten("", &toml::Value::Table(overlay.clone()), &mut flat);
                for (key, value) in &flat {
                    if !registry::is_runtime(key) {
                        return Err(format!(
                            "settings: {}: profile `{name}` may only override runtime keys, not `{key}`",
                            path.display()
                        ));
                    }
                    registry::check_value(key, value).map_err(|error| {
                        format!("{error} ({}, profile `{name}`)", path.display())
                    })?;
                }
                parsed.profiles.insert(name.clone(), flat);
            }
            continue;
        }
        flatten(key, value, &mut parsed.flat);
    }
    // **A choice an upgrade retired does not stop Sterna.** The file said
    // something that was true of the version that wrote it; refusing to
    // start over it punishes the person for updating. It is read as unset
    // and reported. Every other invalid value still refuses -- a malformed
    // permission must never quietly become the default.
    let retired: Vec<(String, String)> = parsed
        .flat
        .iter()
        .filter_map(|(key, value)| {
            let saved = value
                .as_str()
                .map_or_else(|| value.to_string(), str::to_string);
            if registry::retired_key(key).is_some()
                || registry::migrated_value(key, &saved).is_some()
            {
                return Some((key.clone(), saved));
            }
            let spec = registry::spec(key)?;
            let word = value.as_str()?;
            (spec.kind == registry::Kind::Choice && !spec.choices.contains(&word))
                .then(|| (key.clone(), word.to_string()))
        })
        .collect();
    for (key, word) in &retired {
        match registry::migrated_value(key, word) {
            // Read as what it means now, before any file is rewritten.
            Some(now) => {
                parsed
                    .flat
                    .insert(key.clone(), toml::Value::String(now.to_string()));
            }
            None => {
                parsed.flat.remove(key);
            }
        }
    }
    parsed.retired = retired;
    for (key, value) in &parsed.flat {
        registry::check_value(key, value)
            .map_err(|error| format!("{error} ({})", path.display()))?;
        if scope == Scope::Global && key.starts_with("legacy.") {
            return Err(format!(
                "settings: {}: `{key}` describes one project's migration and belongs in its project file",
                path.display()
            ));
        }
    }
    Ok(parsed)
}

fn check_profile_name(name: &str) -> Result<(), String> {
    if name.is_empty()
        || !name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b))
    {
        return Err(format!(
            "settings: profile names are letters, digits, `_` or `-`, not `{name}`"
        ));
    }
    Ok(())
}

fn flatten(prefix: &str, value: &toml::Value, out: &mut BTreeMap<String, toml::Value>) {
    match value {
        toml::Value::Table(table) => {
            for (key, value) in table {
                let key = if prefix.is_empty() {
                    key.clone()
                } else {
                    format!("{prefix}.{key}")
                };
                flatten(&key, value, out);
            }
        }
        leaf => {
            out.insert(prefix.to_string(), leaf.clone());
        }
    }
}

fn nest<'a>(entries: impl Iterator<Item = (&'a String, &'a toml::Value)>) -> toml::Value {
    let mut root = toml::value::Table::new();
    for (key, value) in entries {
        let mut parts = key.split('.').peekable();
        let mut cursor = &mut root;
        while let Some(part) = parts.next() {
            if parts.peek().is_none() {
                cursor.insert(part.to_string(), value.clone());
                break;
            }
            let entry = cursor
                .entry(part.to_string())
                .or_insert_with(|| toml::Value::Table(toml::value::Table::new()));
            match entry {
                toml::Value::Table(table) => cursor = table,
                _ => break,
            }
        }
    }
    toml::Value::Table(root)
}

/// Every key that has a value before anyone has saved one -- the curated
/// presentation keys, and each runtime default `config.rs` already applies.
///
/// A panel that showed "unset" for `helpers.enabled` would be describing a
/// runtime that does not exist: the default is `true` whether or not a file
/// says so. These are read from the same `Default` implementations the
/// session uses, so the panel cannot drift from the runtime. Keys with no
/// default -- the three model ids -- are absent until something sets them,
/// which is exactly what "unset means off" means for helpers and agents.
fn defaults() -> Vec<(&'static str, toml::Value)> {
    let limits = crate::config::Limits::default();
    let supervisor = crate::config::SupervisorConfig::default();
    let helpers = crate::config::HelpersConfig::default();
    let agents = crate::config::AgentsConfig::default();
    let web = crate::web::WebConfig::default();
    let decisions = crate::config::DecisionsConfig::default();
    let count = |value: u64| toml::Value::Integer(i64::try_from(value).unwrap_or(i64::MAX));
    let word = |value: &str| toml::Value::String(value.to_string());
    vec![
        ("ui.theme", word(crate::tui::Theme::natural().name())),
        ("ui.statusline", word("full")),
        ("ui.sidebar", word("auto")),
        ("ui.background", word("auto")),
        ("ui.motion", word(crate::tui::Motion::default().name())),
        ("ui.stream", word(crate::tui::Stream::default().name())),
        (
            "session.effort",
            word(crate::wire::Effort::default().name()),
        ),
        ("limits.cell_wall_clock_s", count(limits.cell_wall_clock_s)),
        ("limits.response_bytes", count(limits.response_bytes as u64)),
        // `0` is this file's spelling for "no ceiling", the same as absent,
        // so the row round-trips through the parser unchanged.
        ("limits.cells", count(limits.cells.unwrap_or(0))),
        (
            "supervisor.enabled",
            toml::Value::Boolean(supervisor.enabled),
        ),
        ("supervisor.every", count(u64::from(supervisor.every))),
        ("helpers.enabled", toml::Value::Boolean(helpers.enabled)),
        ("helpers.preflight", toml::Value::Boolean(helpers.preflight)),
        (
            "helpers.calls_per_cell",
            count(u64::from(helpers.calls_per_cell)),
        ),
        (
            "helpers.completion",
            word(match helpers.completion {
                CompletionStyle::Silent => "silent",
                CompletionStyle::Recap => "recap",
            }),
        ),
        ("helpers.effort.find", word(helpers.effort.find.name())),
        ("helpers.effort.reduce", word(helpers.effort.reduce.name())),
        ("helpers.effort.check", word(helpers.effort.check.name())),
        (
            "agents.mode",
            word(match agents.mode {
                AgentsMode::Auto => "auto",
                AgentsMode::Off => "off",
                AgentsMode::Pinned => "pinned",
                AgentsMode::Roster => "roster",
            }),
        ),
        ("web.enabled", toml::Value::Boolean(web.enabled)),
        ("web.allow_http", toml::Value::Boolean(web.allow_http)),
        ("web.allow_domains", toml::Value::Array(Vec::new())),
        ("web.deny_domains", toml::Value::Array(Vec::new())),
        (
            "web.max_response_bytes",
            count(web.max_response_bytes as u64),
        ),
        ("web.timeout_seconds", count(web.timeout_seconds)),
        ("decisions.mode", word(decisions.mode.as_str())),
        (
            "decisions.hold_above",
            toml::Value::Float(decisions.hold_above),
        ),
        (
            "decisions.completion_no_below",
            toml::Value::Float(decisions.completion_no_below),
        ),
        (
            "decisions.completion_yes_above",
            toml::Value::Float(decisions.completion_yes_above),
        ),
    ]
}

fn union_lists(existing: Option<&toml::Value>, addition: &toml::Value) -> toml::Value {
    let mut items: Vec<toml::Value> = existing
        .and_then(toml::Value::as_array)
        .cloned()
        .unwrap_or_default();
    for value in addition.as_array().cloned().unwrap_or_default() {
        if !items.contains(&value) {
            items.push(value);
        }
    }
    toml::Value::Array(items)
}

fn string_list(values: &toml::Value, table: &str, key: &str) -> Option<Vec<String>> {
    let entries = values.get(table)?.get(key)?.as_array()?;
    Some(
        entries
            .iter()
            .filter_map(toml::Value::as_str)
            .map(str::to_string)
            .collect(),
    )
}

fn list_value(items: &[String]) -> toml::Value {
    toml::Value::Array(
        items
            .iter()
            .map(|item| toml::Value::String(item.clone()))
            .collect(),
    )
}

fn render(value: &toml::Value) -> String {
    match value {
        toml::Value::String(text) => format!("`{text}`"),
        other => format!("`{other}`"),
    }
}

fn push_list(report: &mut String, label: &str, items: &[&String]) {
    if items.is_empty() {
        return;
    }
    for item in items {
        report.push_str(&format!("  {label} {item}\n"));
    }
}

// -- documents -----------------------------------------------------------

/// Applies changes to a document's text, preserving every comment, key order
/// and unrelated value `toml_edit` can preserve.
fn apply_changes(text: &str, changes: &[Change]) -> Result<String, String> {
    let mut document: DocumentMut = text
        .parse()
        .map_err(|error| format!("settings: the file is not valid TOML: {error}"))?;
    for change in changes {
        let path: Vec<&str> = change.path.iter().map(String::as_str).collect();
        match &change.value {
            Some(value) => set_in_table(document.as_table_mut(), &path, edit_value(value))?,
            None => {
                remove_in_table(document.as_table_mut(), &path);
            }
        }
    }
    Ok(document.to_string())
}

fn edit_value(value: &toml::Value) -> Value {
    match value {
        toml::Value::String(text) => Value::from(text.as_str()),
        toml::Value::Integer(number) => Value::from(*number),
        toml::Value::Float(number) => Value::from(*number),
        toml::Value::Boolean(flag) => Value::from(*flag),
        toml::Value::Array(items) => {
            let mut array = toml_edit::Array::new();
            for item in items {
                array.push(edit_value(item));
            }
            Value::Array(array)
        }
        // Datetimes and tables are not values any registry key produces.
        other => Value::from(other.to_string()),
    }
}

fn set_in_table(table: &mut Table, path: &[&str], value: Value) -> Result<(), String> {
    let (head, rest) = path.split_first().expect("a change names at least one key");
    let head = *head;
    if rest.is_empty() {
        match table.get_mut(head) {
            // In place, so the key keeps the comment written above it.
            Some(Item::Value(existing)) => {
                let mut value = value;
                *value.decor_mut() = existing.decor().clone();
                *existing = value;
            }
            Some(_) => {
                return Err(format!(
                    "settings: `{head}` is a table in this file, not a value"
                ));
            }
            None => {
                table.insert(head, Item::Value(value));
            }
        }
        return Ok(());
    }
    if table.get(head).is_none() {
        let mut fresh = Table::new();
        fresh.set_implicit(true);
        table.insert(head, Item::Table(fresh));
    }
    match table.get_mut(head).expect("just inserted") {
        Item::Table(child) => set_in_table(child, rest, value),
        Item::Value(Value::InlineTable(child)) => set_in_inline(child, rest, value),
        _ => Err(format!(
            "settings: `{head}` is not a table in this file; Sterna will not overwrite it"
        )),
    }
}

fn set_in_inline(table: &mut InlineTable, path: &[&str], value: Value) -> Result<(), String> {
    let (head, rest) = path.split_first().expect("a change names at least one key");
    let head = *head;
    if rest.is_empty() {
        table.insert(head, value);
        return Ok(());
    }
    if table.get(head).is_none() {
        table.insert(head, Value::InlineTable(InlineTable::new()));
    }
    match table.get_mut(head).expect("just inserted") {
        Value::InlineTable(child) => set_in_inline(child, rest, value),
        _ => Err(format!(
            "settings: `{head}` is not a table in this file; Sterna will not overwrite it"
        )),
    }
}

/// Removes a key, and any table the removal emptied -- an unset leaves no
/// `[helpers]` header standing over nothing.
fn remove_in_table(table: &mut Table, path: &[&str]) -> bool {
    let (head, rest) = path.split_first().expect("a change names at least one key");
    let head = *head;
    if rest.is_empty() {
        return table.remove(head).is_some();
    }
    let emptied = match table.get_mut(head) {
        Some(Item::Table(child)) => {
            let removed = remove_in_table(child, rest);
            removed && child.is_empty()
        }
        Some(Item::Value(Value::InlineTable(child))) => {
            let removed = remove_in_inline(child, rest);
            removed && child.is_empty()
        }
        _ => false,
    };
    if emptied {
        table.remove(head);
    }
    emptied
}

fn remove_in_inline(table: &mut InlineTable, path: &[&str]) -> bool {
    let (head, rest) = path.split_first().expect("a change names at least one key");
    let head = *head;
    if rest.is_empty() {
        return table.remove(head).is_some();
    }
    match table.get_mut(head) {
        Some(Value::InlineTable(child)) => {
            let removed = remove_in_inline(child, rest);
            if removed && child.is_empty() {
                table.remove(head);
            }
            removed
        }
        _ => false,
    }
}

// -- the filesystem ------------------------------------------------------

/// Resolve the existing ancestor once, retaining missing suffixes. In
/// particular macOS's /var alias must be resolved before no-follow traversal.
fn anchor_path(path: &Path) -> Result<PathBuf, String> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .map_err(|e| format!("settings: {e}"))?
            .join(path)
    };
    let mut ancestor = absolute.as_path();
    let mut suffix = Vec::new();
    loop {
        match std::fs::canonicalize(ancestor) {
            Ok(mut resolved) => {
                for name in suffix.iter().rev() {
                    resolved.push(name);
                }
                return Ok(resolved);
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let name = ancestor
                    .file_name()
                    .ok_or_else(|| format!("settings: {}: {error}", absolute.display()))?;
                suffix.push(name.to_os_string());
                ancestor = ancestor
                    .parent()
                    .ok_or_else(|| format!("settings: {}: {error}", absolute.display()))?;
            }
            Err(error) => return Err(format!("settings: {}: {error}", absolute.display())),
        }
    }
}

/// Reading and writing a settings file, relative to a directory descriptor
/// Sterna holds open.
///
/// A settings directory sits inside a project a tool may write, so the
/// pathname a check looked at and the pathname a write later opens are not
/// guaranteed to be the same object: between the two, `.sterna` can become a
/// symbolic link to somewhere else entirely. Re-checking the pathname does
/// not close that window -- only naming the object does. So every component
/// is opened once, with `O_NOFOLLOW`, and the temporary, the lock, the
/// conflict re-read and the rename are all `*at` calls against the
/// descriptor that opening produced.
///
/// Hosts without `openat` fall back to the pathname form below, which is the
/// weaker check it can make.
#[cfg(any(target_os = "linux", target_os = "macos"))]
mod files {
    use std::ffi::CString;
    use std::io::{Read, Write};
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
    use std::os::unix::ffi::OsStrExt;
    use std::path::{Path, PathBuf};
    use std::time::Duration;

    /// One open directory: the descriptor, and the path only so a refusal can
    /// name the file a person would look for.
    pub struct Directory {
        fd: OwnedFd,
        path: PathBuf,
    }

    /// The lock a writer holds, released by dropping it -- through the same
    /// descriptor, so it cannot unlink a file that took the name later.
    pub struct Lock {
        fd: OwnedFd,
        name: String,
    }

    fn name_of(segment: &str) -> Result<CString, String> {
        CString::new(segment)
            .map_err(|_| format!("settings: `{segment}` is not a usable file name"))
    }

    fn symlink_refusal(path: &Path) -> String {
        format!(
            "settings: `{}` is a symbolic link; Sterna reads and writes settings only through real paths",
            path.display()
        )
    }

    impl Directory {
        /// Opens `base` (a path Sterna resolved itself) and then each segment,
        /// refusing a symbolic link at every step. `create` makes the missing
        /// segments, which is the only way a settings directory is ever made.
        pub fn open(base: &Path, segments: &[&str], create: bool) -> Result<Option<Self>, String> {
            let root = c"/";
            let fd = unsafe {
                libc::open(
                    root.as_ptr(),
                    libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC,
                )
            };
            if fd < 0 {
                let error = std::io::Error::last_os_error();
                if error.kind() == std::io::ErrorKind::NotFound {
                    return Ok(None);
                }
                return Err(format!("settings: {}: {error}", base.display()));
            }
            let mut current = unsafe { OwnedFd::from_raw_fd(fd) };
            let mut path = PathBuf::from("/");
            let base_parts = base.components().filter_map(|part| match part {
                std::path::Component::Normal(name) => Some(name),
                _ => None,
            });
            for segment in base_parts.chain(segments.iter().map(std::ffi::OsStr::new)) {
                path.push(segment);
                let name = CString::new(segment.as_bytes())
                    .map_err(|_| format!("settings: `{}` is not a usable path", path.display()))?;
                if create {
                    let made = unsafe { libc::mkdirat(current.as_raw_fd(), name.as_ptr(), 0o700) };
                    if made < 0 {
                        let error = std::io::Error::last_os_error();
                        if error.kind() != std::io::ErrorKind::AlreadyExists {
                            return Err(format!("settings: {}: {error}", path.display()));
                        }
                    }
                }
                let fd = unsafe {
                    libc::openat(
                        current.as_raw_fd(),
                        name.as_ptr(),
                        libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                    )
                };
                if fd < 0 {
                    let error = std::io::Error::last_os_error();
                    return match error.raw_os_error() {
                        Some(libc::ELOOP) => Err(symlink_refusal(&path)),
                        Some(libc::ENOTDIR) => Err(format!(
                            "settings: `{}` is a symbolic link or not a directory",
                            path.display()
                        )),
                        _ if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
                        _ => Err(format!("settings: {}: {error}", path.display())),
                    };
                }
                current = unsafe { OwnedFd::from_raw_fd(fd) };
            }
            Ok(Some(Self { fd: current, path }))
        }

        /// The file's path, for a sentence a person reads.
        pub fn display(&self, name: &str) -> PathBuf {
            self.path.join(name)
        }

        /// Reads one file in this directory. A missing file is `None`; a
        /// symbolic link, a directory and an oversized document are refusals.
        pub fn read(&self, name: &str, max: u64) -> Result<Option<String>, String> {
            let target = name_of(name)?;
            // `O_NONBLOCK` so that a FIFO left in place of a settings file
            // is a refusal rather than a session that never starts: opening a
            // pipe with no writer blocks forever otherwise. A regular file --
            // the only thing the check below accepts -- is unaffected by it.
            let fd = unsafe {
                libc::openat(
                    self.fd.as_raw_fd(),
                    target.as_ptr(),
                    libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC,
                )
            };
            if fd < 0 {
                let error = std::io::Error::last_os_error();
                return match error.raw_os_error() {
                    Some(libc::ELOOP) => Err(symlink_refusal(&self.display(name))),
                    _ if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
                    _ => Err(format!(
                        "settings: {}: {error}",
                        self.display(name).display()
                    )),
                };
            }
            let file = unsafe { std::fs::File::from_raw_fd(fd) };
            let metadata = file
                .metadata()
                .map_err(|error| format!("settings: {}: {error}", self.display(name).display()))?;
            if !metadata.is_file() {
                return Err(format!(
                    "settings: `{}` is not a regular file",
                    self.display(name).display()
                ));
            }
            if metadata.len() > max {
                return Err(format!(
                    "settings: {}: exceeds the {} KiB settings document limit",
                    self.display(name).display(),
                    max / 1024
                ));
            }
            // The length is bounded again while reading, not only by the
            // metadata above: a file that grows between the two is still read
            // to a known ceiling, and the refusal is the same sentence.
            let mut text = String::new();
            file.take(max + 1)
                .read_to_string(&mut text)
                .map_err(|error| format!("settings: {}: {error}", self.display(name).display()))?;
            if text.len() as u64 > max {
                return Err(format!(
                    "settings: {}: exceeds the {} KiB settings document limit",
                    self.display(name).display(),
                    max / 1024
                ));
            }
            Ok(Some(text))
        }

        /// Writes through a temporary file in this same directory and renames
        /// it over the target, so a reader sees either the old document or the
        /// new one and never half of either. The temporary is created
        /// exclusively, never followed, and removed on every failure path.
        pub fn write(&self, name: &str, text: &str) -> Result<(), String> {
            let target = name_of(name)?;
            let display = self.display(name);
            // Keep the mode the file already had, so a deliberately tightened
            // (or group-readable) settings file stays as its owner set it.
            let mut mode: libc::mode_t = 0o600;
            let mut status: libc::stat = unsafe { std::mem::zeroed() };
            let found = unsafe {
                libc::fstatat(
                    self.fd.as_raw_fd(),
                    target.as_ptr(),
                    &mut status,
                    libc::AT_SYMLINK_NOFOLLOW,
                )
            };
            if found == 0 {
                if (status.st_mode & libc::S_IFMT) != libc::S_IFREG {
                    return Err(format!(
                        "settings: `{}` is not a regular file; Sterna will not replace it",
                        display.display()
                    ));
                }
                mode = status.st_mode & 0o777;
            }

            let temporary = format!(
                ".{name}.tmp{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|since| since.as_nanos())
                    .unwrap_or_default()
            );
            let scratch = name_of(&temporary)?;
            let fd = unsafe {
                libc::openat(
                    self.fd.as_raw_fd(),
                    scratch.as_ptr(),
                    libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_CLOEXEC,
                    libc::c_uint::from(0o600_u16),
                )
            };
            if fd < 0 {
                let error = std::io::Error::last_os_error();
                return Err(format!(
                    "settings: {}: {error}",
                    self.display(&temporary).display()
                ));
            }
            let mut file = unsafe { std::fs::File::from_raw_fd(fd) };
            let written = file
                .write_all(text.as_bytes())
                .and_then(|()| {
                    // `fchmod` on the descriptor, not the name: the file this
                    // permission lands on is the one just created.
                    if unsafe { libc::fchmod(file.as_raw_fd(), mode) } < 0 {
                        return Err(std::io::Error::last_os_error());
                    }
                    Ok(())
                })
                .and_then(|()| file.sync_all());
            drop(file);
            if let Err(error) = written {
                self.discard(&scratch);
                return Err(format!(
                    "settings: {}: {error}",
                    self.display(&temporary).display()
                ));
            }
            let renamed = unsafe {
                libc::renameat(
                    self.fd.as_raw_fd(),
                    scratch.as_ptr(),
                    self.fd.as_raw_fd(),
                    target.as_ptr(),
                )
            };
            if renamed < 0 {
                let error = std::io::Error::last_os_error();
                self.discard(&scratch);
                return Err(format!("settings: {}: {error}", display.display()));
            }
            // The rename is what has to survive a crash, so the directory
            // entry is flushed too. A failure here is not a lost document.
            unsafe { libc::fsync(self.fd.as_raw_fd()) };
            Ok(())
        }

        fn discard(&self, name: &CString) {
            unsafe { libc::unlinkat(self.fd.as_raw_fd(), name.as_ptr(), 0) };
        }

        /// One writer at a time, across processes. A held lock is never
        /// broken by age: a lock file that outlives its writer is a thing a
        /// person removes knowingly, and the refusal says which file and how.
        pub fn lock(&self, name: &str) -> Result<Lock, String> {
            let lock_name = format!(".{name}.lock");
            let path = name_of(&lock_name)?;
            for attempt in 0..100 {
                let fd = unsafe {
                    libc::openat(
                        self.fd.as_raw_fd(),
                        path.as_ptr(),
                        libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_CLOEXEC,
                        libc::c_uint::from(0o600_u16),
                    )
                };
                if fd >= 0 {
                    let mut file = unsafe { std::fs::File::from_raw_fd(fd) };
                    let _ = write!(file, "{}", std::process::id());
                    let _ = file.sync_all();
                    drop(file);
                    return Ok(Lock {
                        fd: self
                            .fd
                            .try_clone()
                            .map_err(|error| format!("settings: {error}"))?,
                        name: lock_name,
                    });
                }
                let error = std::io::Error::last_os_error();
                if error.kind() != std::io::ErrorKind::AlreadyExists {
                    return Err(format!(
                        "settings: {}: {error}",
                        self.display(&lock_name).display()
                    ));
                }
                if attempt + 1 < 100 {
                    std::thread::sleep(Duration::from_millis(20));
                }
            }
            Err(format!(
                "settings: another editor is writing `{}` and has held it for two seconds. \
                 Nothing was written. If no other Sterna is running, remove `{}` and try again.",
                self.display(name).display(),
                self.display(&lock_name).display()
            ))
        }
    }

    impl Drop for Lock {
        fn drop(&mut self) {
            if let Ok(name) = CString::new(self.name.as_str()) {
                unsafe { libc::unlinkat(self.fd.as_raw_fd(), name.as_ptr(), 0) };
            }
        }
    }
}

/// The pathname form, for hosts without `openat`: the same verbs, and the
/// same refusals, checked by name rather than by descriptor.
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
mod files {
    use std::io::Write;
    use std::path::{Path, PathBuf};
    use std::time::Duration;

    pub struct Directory {
        path: PathBuf,
    }

    pub struct Lock {
        path: PathBuf,
    }

    fn symlink_refusal(path: &Path) -> String {
        format!(
            "settings: `{}` is a symbolic link; Sterna reads and writes settings only through real paths",
            path.display()
        )
    }

    fn refuse_link(path: &Path) -> Result<bool, String> {
        match std::fs::symlink_metadata(path) {
            Ok(metadata) if metadata.file_type().is_symlink() => Err(symlink_refusal(path)),
            Ok(_) => Ok(true),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(error) => Err(format!("settings: {}: {error}", path.display())),
        }
    }

    impl Directory {
        pub fn open(base: &Path, segments: &[&str], create: bool) -> Result<Option<Self>, String> {
            let mut path = base.to_path_buf();
            if create {
                std::fs::create_dir_all(base)
                    .map_err(|error| format!("settings: {}: {error}", base.display()))?;
            }
            for segment in segments {
                path.push(segment);
                if create && !refuse_link(&path)? {
                    std::fs::create_dir_all(&path)
                        .map_err(|error| format!("settings: {}: {error}", path.display()))?;
                }
                if !refuse_link(&path)? {
                    return Ok(None);
                }
            }
            if !refuse_link(&path)? {
                return Ok(None);
            }
            Ok(Some(Self { path }))
        }

        pub fn display(&self, name: &str) -> PathBuf {
            self.path.join(name)
        }

        pub fn read(&self, name: &str, max: u64) -> Result<Option<String>, String> {
            let path = self.display(name);
            let metadata = match std::fs::symlink_metadata(&path) {
                Ok(metadata) => metadata,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
                Err(error) => return Err(format!("settings: {}: {error}", path.display())),
            };
            if metadata.file_type().is_symlink() {
                return Err(symlink_refusal(&path));
            }
            if !metadata.is_file() {
                return Err(format!(
                    "settings: `{}` is not a regular file",
                    path.display()
                ));
            }
            if metadata.len() > max {
                return Err(format!(
                    "settings: {}: exceeds the {} KiB settings document limit",
                    path.display(),
                    max / 1024
                ));
            }
            std::fs::read_to_string(&path)
                .map(Some)
                .map_err(|error| format!("settings: {}: {error}", path.display()))
        }

        pub fn write(&self, name: &str, text: &str) -> Result<(), String> {
            let path = self.display(name);
            refuse_link(&path)?;
            let temporary = self.display(&format!(
                ".{name}.tmp{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|since| since.as_nanos())
                    .unwrap_or_default()
            ));
            let write = || -> std::io::Result<()> {
                let mut file = std::fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(&temporary)?;
                file.write_all(text.as_bytes())?;
                file.sync_all()
            };
            if let Err(error) = write() {
                let _ = std::fs::remove_file(&temporary);
                return Err(format!("settings: {}: {error}", temporary.display()));
            }
            if let Err(error) = std::fs::rename(&temporary, &path) {
                let _ = std::fs::remove_file(&temporary);
                return Err(format!("settings: {}: {error}", path.display()));
            }
            Ok(())
        }

        pub fn lock(&self, name: &str) -> Result<Lock, String> {
            let path = self.display(&format!(".{name}.lock"));
            for attempt in 0..100 {
                match std::fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(&path)
                {
                    Ok(mut file) => {
                        let _ = write!(file, "{}", std::process::id());
                        return Ok(Lock { path });
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                        if attempt + 1 < 100 {
                            std::thread::sleep(Duration::from_millis(20));
                        }
                    }
                    Err(error) => {
                        return Err(format!("settings: {}: {error}", path.display()));
                    }
                }
            }
            Err(format!(
                "settings: another editor is writing `{}` and has held it for two seconds. \
                 Nothing was written. If no other Sterna is running, remove `{}` and try again.",
                self.display(name).display(),
                path.display()
            ))
        }
    }

    impl Drop for Lock {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}
