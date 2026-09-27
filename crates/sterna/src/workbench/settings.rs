//! Direct local preferences over Sterna's existing typed, conflict-aware store.
use crate::settings::{Kind, Loaded, Scope, SettingSpec, Snapshot, Store};
use crate::tui::ScreenState;
use std::path::PathBuf;

pub const CATEGORIES: [&str; 6] = [
    "Everyday",
    "Display",
    "Little helpers",
    "Models & accounts",
    "Subagents",
    "Advanced",
];

/// The first category, and the only one chosen by how often a person reaches
/// for the thing rather than by which table it lives under.
///
/// **A settings surface is split by frequency of use, not by taxonomy.** What
/// was here was three keys called "Workspace", while the model you talk to
/// sat two categories away under "Models & accounts" and whether helpers run
/// at all sat under another. Someone opening settings wants the five things
/// they change; every one of those five is on this list, and everything else
/// is still exactly one Tab away.
const EVERYDAY: [&str; 7] = [
    "model.parent",
    "session.effort",
    "session.mode",
    "permissions.mode",
    "helpers.enabled",
    "ui.theme",
    "ui.motion",
];
pub struct Preferences {
    pub scope: Scope,
    pub category: usize,
    pub selected: usize,
    pub query: String,
    pub editing: Option<(String, String)>,
    pub notice: String,
    /// The control the last save owes the running session, waiting for the
    /// reducer to hand it back to the loop. Drained by [`Self::take_live`];
    /// a second save before that drain replaces it, because the later choice
    /// is the one the person is looking at.
    live: Option<String>,
    pub snapshot: Snapshot,
    pub loaded: Loaded,
    pub path: PathBuf,
    root: PathBuf,
    global: Option<PathBuf>,
    profile: Option<String>,
    undo: Option<Vec<(String, Option<String>)>>,
}
impl Preferences {
    pub fn open(s: &ScreenState) -> Result<Self, String> {
        Self::with_global(s, crate::project::workflows::user_directory())
    }
    /// Tests and embedded hosts can supply a global directory without process-wide environment edits.
    pub fn with_global(s: &ScreenState, global: Option<PathBuf>) -> Result<Self, String> {
        let root = s
            .settings_root
            .clone()
            .ok_or("Settings require a project root.")?;
        let store = Store::with_global(&root, global.clone())?;
        Ok(Self {
            scope: Scope::Local,
            category: 0,
            selected: 0,
            query: String::new(),
            editing: None,
            notice: String::new(),
            live: None,
            snapshot: store.read(Scope::Local)?,
            loaded: store.load(s.settings_profile.as_deref())?,
            path: store.path(Scope::Local).to_path_buf(),
            root,
            global,
            profile: s.settings_profile.clone(),
            undo: None,
        })
    }
    pub fn rows(&self) -> Vec<&'static SettingSpec> {
        let mut found: Vec<&'static SettingSpec> = crate::settings::specs()
            .iter()
            .filter(|spec| {
                if !self.query.is_empty() {
                    let q = self.query.to_lowercase();
                    return format!("{} {} {}", spec.key, spec.label, spec.description)
                        .to_lowercase()
                        .contains(&q);
                }
                let k = spec.key;
                match self.category {
                    0 => EVERYDAY.contains(&k),
                    1 => k.starts_with("ui."),
                    2 => [
                        "helpers.enabled",
                        "helpers.completion",
                        "helpers.preflight",
                        "helpers.completion_check",
                        "helpers.learn",
                        "helpers.effort.find",
                        "helpers.effort.reduce",
                        "helpers.effort.check",
                    ]
                    .contains(&k),
                    3 => ["model.parent", "helpers.model"].contains(&k),
                    4 => {
                        k == "agents.mode"
                            || k == "agents.model"
                            || (k.starts_with("agents.slots.") && k.ends_with(".model"))
                    }
                    // A key the parser still accepts so an existing project
                    // starts, and that does nothing. Offering it is offering
                    // a decision with no consequence.
                    _ => !spec.basic && spec.key != "limits.task_tokens",
                }
            })
            .collect();
        // The everyday list is in the order a person reaches for the things,
        // which is not the order the registry happens to declare them in.
        if self.query.is_empty() && self.category == 0 {
            found.sort_by_key(|spec| {
                EVERYDAY
                    .iter()
                    .position(|k| *k == spec.key)
                    .unwrap_or(usize::MAX)
            });
        }
        found
    }
    /// What the session is actually using for `key`, as a word.
    ///
    /// With nothing configured this falls back to what the runtime would do
    /// anyway, so a row reads `auto` rather than `unset` while a session is
    /// demonstrably running on `auto`. The fallback is the panel's alone --
    /// see [`crate::settings::shown_default`] for why it may not be the
    /// loader's.
    pub fn effective(&self, key: &str) -> String {
        match get(&self.loaded.values, key) {
            Some(value) => show(Some(value)),
            None => crate::settings::shown_default(key).unwrap_or_else(|| "unset".into()),
        }
    }
    pub fn saved(&self, key: &str) -> Option<String> {
        get(&self.snapshot.values, key).map(|v| show(Some(v)))
    }
    pub fn origin(&self, key: &str) -> &str {
        self.loaded
            .origins
            .get(key)
            .map(String::as_str)
            .unwrap_or("built-in")
    }
    pub fn choices(spec: &SettingSpec) -> Vec<String> {
        if spec.key == "agents.mode" {
            return vec!["off".into(), "pinned".into(), "roster".into()];
        }
        if spec.kind == Kind::Bool {
            return vec!["false".into(), "true".into()];
        }
        spec.choices.iter().map(|s| s.to_string()).collect()
    }
    pub fn save(
        &mut self,
        key: &str,
        value: Option<String>,
        s: &mut ScreenState,
    ) -> Result<(), String> {
        let store = Store::with_global(&self.root, self.global.clone())?;
        let mut edits = vec![(key.to_string(), value.clone())];
        if key == "agents.model" && value.is_some() {
            edits.push(("agents.mode".into(), Some("pinned".into())));
        }
        if key == "agents.mode" && value.as_deref().is_some_and(|v| v != "pinned") {
            edits.push(("agents.model".into(), None));
        }
        if key.starts_with("agents.slots.") && key.ends_with(".model") && value.is_none() {
            edits.push((key.replace(".model", ".effort"), None));
            if self.loaded.config.agents.mode == crate::config::AgentsMode::Roster
                && self.loaded.config.agents.slots.len() == 1
            {
                edits.push(("agents.mode".into(), Some("off".into())));
            }
        }
        let previous = edits
            .iter()
            .map(|(key, _)| (key.clone(), self.saved(key)))
            .collect();
        store.save(self.scope, &self.snapshot, &edits)?;
        self.undo = Some(previous);
        self.reload()?;
        // Apply only the edited presentation field: an unrelated save must
        // not erase a live /motion or /sidebar override.
        let mut resolved = ScreenState::default();
        crate::settings_session::presentation(&mut resolved, &self.loaded.values);
        match key {
            "ui.theme" => s.theme = resolved.theme,
            "ui.reduced_motion" | "ui.motion" => s.set_motion(resolved.motion),
            "ui.statusline" => s.status_line = resolved.status_line,
            "ui.sidebar" => s.sidebar = resolved.sidebar,
            "ui.stream" => s.stream = resolved.stream,
            _ => {}
        }
        // **The saved choice reaches the session that is running, not only
        // the next one.** Everything this can answer for is answered now; a
        // key it cannot answer for says so in its own words rather than
        // handing every row the same apology.
        self.live = crate::settings::live_command(key, value.as_deref());
        self.notice = if self.live.is_some() || key.starts_with("ui.") {
            format!(
                "{} is now {}. Ctrl-Z undoes it.",
                label(key),
                show(get(&self.snapshot.values, key))
            )
        } else {
            format!(
                "{} is saved. This session keeps what it started with; the next one takes it.",
                label(key)
            )
        };
        if self.profile.is_some() {
            self.notice
                .push_str(" · the selected profile may override this scope");
        }
        Ok(())
    }

    /// Hands the reducer the control this save owes the running session.
    pub fn take_live(&mut self) -> Option<String> {
        self.live.take()
    }
    pub fn cycle(&mut self, forward: bool, s: &mut ScreenState) -> Result<(), String> {
        let Some(spec) = self.rows().get(self.selected).copied() else {
            return Ok(());
        };
        let choices = Self::choices(spec);
        if choices.is_empty() {
            self.editing = Some((spec.key.into(), self.effective(spec.key)));
            return Ok(());
        }
        let n = choices.len();
        let old = choices
            .iter()
            .position(|v| *v == self.effective(spec.key))
            .unwrap_or(0);
        let i = if forward {
            (old + 1) % n
        } else {
            (old + n - 1) % n
        };
        // Safety-relevant choices need an explicit field confirmation, never arrow rollover.
        if spec.key.starts_with("permissions.") || spec.key == "agents.mode" {
            self.editing = Some((spec.key.into(), choices[i].clone()));
            return Ok(());
        }
        self.save(spec.key, Some(choices[i].clone()), s)
    }
    pub fn undo(&mut self, s: &mut ScreenState) -> Result<(), String> {
        if let Some(previous) = self.undo.clone() {
            let store = Store::with_global(&self.root, self.global.clone())?;
            store.save(self.scope, &self.snapshot, &previous)?;
            self.reload()?;
            let mut resolved = ScreenState::default();
            crate::settings_session::presentation(&mut resolved, &self.loaded.values);
            for (key, _) in &previous {
                match key.as_str() {
                    "ui.theme" => s.theme = resolved.theme,
                    "ui.reduced_motion" | "ui.motion" => s.set_motion(resolved.motion),
                    "ui.statusline" => s.status_line = resolved.status_line,
                    "ui.sidebar" => s.sidebar = resolved.sidebar,
                    "ui.stream" => s.stream = resolved.stream,
                    _ => {}
                }
            }
            // An undo that only rewrote the file would be a worse lie than
            // the save was: the row would show the old value while the
            // session went on using the new one. Whatever the save put into
            // force, the undo takes back out, reading the value that won
            // after the reload rather than the one that was written -- a
            // restored-to-inherited key falls back to its inherited value,
            // and that is the value the session must be told about.
            let live = previous.iter().find_map(|(key, _)| {
                crate::settings::live_command(key, Some(&self.effective(key)))
            });
            self.live = live;
            self.undo = None;
            self.notice = "Restored.".into();
        }
        Ok(())
    }
    pub fn switch_scope(&mut self) -> Result<(), String> {
        let next = if self.scope == Scope::Local {
            Scope::Global
        } else {
            Scope::Local
        };
        let store = Store::with_global(&self.root, self.global.clone())?;
        let snapshot = store.read(next)?;
        self.scope = next;
        self.snapshot = snapshot;
        self.path = store.path(next).to_path_buf();
        self.undo = None;
        self.editing = None;
        self.notice.clear();
        Ok(())
    }
    fn reload(&mut self) -> Result<(), String> {
        let store = Store::with_global(&self.root, self.global.clone())?;
        self.snapshot = store.read(self.scope)?;
        self.loaded = store.load(self.profile.as_deref())?;
        Ok(())
    }
}
fn get<'a>(v: &'a toml::Value, key: &str) -> Option<&'a toml::Value> {
    key.split('.').try_fold(v, |v, key| v.get(key))
}
fn show(v: Option<&toml::Value>) -> String {
    v.map(|v| {
        v.as_str()
            .map(str::to_owned)
            .unwrap_or_else(|| v.to_string())
    })
    .unwrap_or_else(|| "unset".into())
}

/// The panel's own label for a key, so a notice reads the way the row does.
/// A key with no spec is its own best name.
fn label(key: &str) -> &str {
    crate::settings::specs()
        .iter()
        .find(|spec| spec.key == key)
        .map_or(key, |spec| spec.label)
}
