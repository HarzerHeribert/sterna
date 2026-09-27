//! Direct local preferences over Sterna's existing typed, conflict-aware store.
use super::Action;
use super::sheet::{Field, Item, Sheet};
use crate::settings::{Kind, Loaded, Scope, SettingSpec, Snapshot, Store};
use crate::tui::ScreenState;
use std::collections::BTreeMap;
use std::path::PathBuf;

pub const CATEGORIES: [&str; 7] = [
    "Everyday",
    "Display",
    "Little helpers",
    "Models & accounts",
    "Subagents",
    "Advanced",
    "Tuning",
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
const HELPERS: [&str; 8] = [
    "helpers.enabled",
    "helpers.completion",
    "helpers.preflight",
    "helpers.completion_check",
    "helpers.learn",
    "helpers.effort.find",
    "helpers.effort.reduce",
    "helpers.effort.check",
];
const MODELS: [&str; 2] = ["model.parent", "helpers.model"];

/// The category a key is listed under. Everyday repeats keys on purpose;
/// no other category repeats one, and Advanced holds only what no other
/// category does.
pub(super) fn category_of(spec: &SettingSpec) -> usize {
    let k = spec.key;
    if k.starts_with("ui.") {
        1
    } else if HELPERS.contains(&k) {
        2
    } else if MODELS.contains(&k) {
        3
    } else if k.starts_with("agents.") && !k.ends_with(".effort") {
        4
    } else if EVERYDAY.contains(&k) {
        0
    } else if spec.kind == Kind::Float {
        // Confidence thresholds: numbers a person tunes, rarely, together.
        6
    } else {
        5
    }
}
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
    /// What the last save replaced, for the session's one undo list.
    /// Drained by [`Self::take_change`].
    change: Option<super::Change>,
    /// What the running session is using now, read from the screen before
    /// every build: a value a chip, Shift-Tab or a command set shows here at
    /// once, whatever the files say.
    observed: BTreeMap<&'static str, String>,
    pub snapshot: Snapshot,
    pub loaded: Loaded,
    pub path: PathBuf,
    root: PathBuf,
    global: Option<PathBuf>,
    profile: Option<String>,
}
impl Preferences {
    pub fn open(s: &ScreenState) -> Result<Self, String> {
        Self::with_global(s, s.settings_global.clone())
    }
    /// Tests and embedded hosts can supply a global directory without process-wide environment edits.
    ///
    /// It opens on Global (decision 6): the project file is written only
    /// when Project is chosen here. With no user settings folder it opens
    /// on the project, the only file there is.
    pub fn with_global(s: &ScreenState, global: Option<PathBuf>) -> Result<Self, String> {
        let root = s
            .settings_root
            .clone()
            .ok_or("Settings require a project root.")?;
        let store = Store::with_global(&root, global.clone())?;
        let scope = if global.is_some() {
            Scope::Global
        } else {
            Scope::Local
        };
        let mut preferences = Self {
            scope,
            category: 0,
            selected: 0,
            query: String::new(),
            editing: None,
            notice: String::new(),
            live: None,
            change: None,
            observed: BTreeMap::new(),
            snapshot: store.read(scope)?,
            loaded: store.load(s.settings_profile.as_deref())?,
            path: store.path(scope).to_path_buf(),
            root,
            global,
            profile: s.settings_profile.clone(),
        };
        preferences.observe(s);
        Ok(preferences)
    }
    /// Reads what the running session uses now for the keys it holds live.
    pub fn observe(&mut self, s: &ScreenState) {
        use crate::tui::{SidebarVisibility, StatusLine};
        let mut observed = BTreeMap::new();
        observed.insert("session.effort", s.effort.name().to_string());
        observed.insert("session.mode", s.mode.setting().to_string());
        observed.insert("permissions.mode", s.permissions.rung().name().to_string());
        observed.insert("ui.theme", s.theme.name().to_string());
        observed.insert("ui.motion", s.motion.name().to_string());
        observed.insert("ui.stream", s.stream.name().to_string());
        observed.insert(
            "ui.sidebar",
            match s.sidebar {
                SidebarVisibility::Shown => "show",
                SidebarVisibility::Hidden => "hide",
                SidebarVisibility::Auto => "auto",
            }
            .to_string(),
        );
        observed.insert(
            "ui.statusline",
            match s.status_line {
                StatusLine::Full => "full",
                StatusLine::Compact => "compact",
                StatusLine::Hidden => "hidden",
            }
            .to_string(),
        );
        if let Some(model) = &s.model {
            observed.insert("model.parent", model.clone());
        }
        self.observed = observed;
    }
    pub fn rows(&self) -> Vec<&'static SettingSpec> {
        let mut found: Vec<&'static SettingSpec> = crate::settings::specs()
            .iter()
            // A key the parser still accepts so an existing project starts,
            // and that does nothing, is not offered; nor is Sterna's own
            // bookkeeping.
            .filter(|spec| spec.key != "limits.task_tokens" && !crate::settings::hidden(spec.key))
            .filter(|spec| {
                if !self.query.is_empty() {
                    let q = self.query.to_lowercase();
                    return format!("{} {} {}", spec.key, spec.label, spec.description)
                        .to_lowercase()
                        .contains(&q);
                }
                match self.category {
                    0 => EVERYDAY.contains(&spec.key),
                    // The favourites and the pinned model are chosen in the
                    // picker; here is only whether subagents run at all.
                    4 => spec.key == "agents.mode",
                    category => category_of(spec) == category,
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
    /// What the session is actually using for `key`, as a word: the running
    /// session's value where it holds one live, else the files'.
    ///
    /// With nothing configured this falls back to what the runtime would do
    /// anyway, so a row reads `auto` rather than `unset` while a session is
    /// demonstrably running on `auto`. The fallback is the panel's alone --
    /// see [`crate::settings::shown_default`] for why it may not be the
    /// loader's.
    pub fn effective(&self, key: &str) -> String {
        match self.observed.get(key) {
            Some(value) => value.clone(),
            None => self.filed(key),
        }
    }
    /// What the files say for `key`, merged, or Sterna's own default.
    pub fn filed(&self, key: &str) -> String {
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
        let before = self.effective(key);
        let previous = edits
            .iter()
            .map(|(key, _)| (key.clone(), self.saved(key)))
            .collect();
        store
            .save(self.scope, &self.snapshot, &edits)
            .map_err(|error| in_words(key, &error))?;
        self.change = Some(super::Change {
            was: format!("{} {}", label(key), word(key, &before)),
            back: Action::Restore(self.scope == Scope::Global, previous),
        });
        self.reload()?;
        apply(s, &self.loaded.values, &[key]);
        // **The saved choice reaches the session that is running, not only
        // the next one.** Everything this can answer for is answered now; a
        // key it cannot answer for says so in its own words rather than
        // handing every row the same apology.
        self.live = crate::settings::live_command(key, value.as_deref());
        self.notice = if self.live.is_some() || key.starts_with("ui.") {
            format!(
                "{} is now {}.",
                label(key),
                word(key, &show(get(&self.snapshot.values, key)))
            )
        } else {
            format!(
                "{} is saved. This session keeps what it started with; the next one takes it.",
                label(key)
            )
        };
        // Saved where it does not win: the row said so before, and the
        // notice says so now rather than announcing a change that is not.
        if self.scope == Scope::Global && self.origin(key) == "project" {
            self.notice = format!(
                "{} is saved globally; this project sets {}, which wins here.",
                label(key),
                word(key, &self.filed(key))
            );
        }
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
    /// Hands the reducer what the last save replaced, for the undo list.
    pub fn take_change(&mut self) -> Option<super::Change> {
        self.change.take()
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
        self.editing = None;
        self.notice.clear();
        Ok(())
    }
    /// Reads the files again after something outside this sheet saved to
    /// them -- a chip, Shift-Tab, a typed command, an undo.
    pub fn refresh(&mut self) {
        if let Err(error) = self.reload() {
            self.notice = error;
        }
    }
    fn reload(&mut self) -> Result<(), String> {
        let store = Store::with_global(&self.root, self.global.clone())?;
        self.snapshot = store.read(self.scope)?;
        self.loaded = store.load(self.profile.as_deref())?;
        Ok(())
    }
}

/// Puts back what a save replaced: the file, the screen and the running
/// session. Returns the command the session is owed for a key it holds
/// live, reading the value that won after the restore -- a key restored to
/// inherited falls back to its inherited value, and that is the value the
/// session must be told about.
pub fn restore(
    s: &mut ScreenState,
    global: bool,
    keys: &[(String, Option<String>)],
) -> Result<Option<String>, String> {
    let root = s
        .settings_root
        .clone()
        .ok_or("Settings require a project root.")?;
    let store = Store::with_global(&root, s.settings_global.clone())?;
    let scope = if global { Scope::Global } else { Scope::Local };
    let snapshot = store.read(scope)?;
    store.save(scope, &snapshot, keys)?;
    let loaded = store.load(s.settings_profile.as_deref())?;
    let names: Vec<&str> = keys.iter().map(|(key, _)| key.as_str()).collect();
    apply(s, &loaded.values, &names);
    let now = |key: &str| match get(&loaded.values, key) {
        Some(value) => show(Some(value)),
        None => crate::settings::shown_default(key).unwrap_or_default(),
    };
    // The rung is this screen's to set; everything else the session owns.
    if names.contains(&"permissions.mode")
        && let Some(rung) = crate::permissions::Rung::parse(&now("permissions.mode"))
    {
        s.permissions.set(rung);
    }
    Ok(names
        .iter()
        .filter(|key| **key != "permissions.mode")
        .find_map(|key| crate::settings::live_command(key, Some(&now(key)))))
}

/// Puts the saved presentation keys in force on the screen. Only the keys
/// named: an unrelated save must not erase a live /motion or /sidebar.
fn apply(s: &mut ScreenState, values: &toml::Value, keys: &[&str]) {
    let mut resolved = ScreenState::default();
    crate::settings_session::presentation(&mut resolved, values);
    for key in keys {
        match *key {
            "ui.theme" => s.theme = resolved.theme,
            "ui.motion" => s.set_motion(resolved.motion),
            "ui.statusline" => s.status_line = resolved.status_line,
            "ui.sidebar" => s.sidebar = resolved.sidebar,
            "ui.stream" => s.stream = resolved.stream,
            _ => {}
        }
    }
}

/// A value as the sheet says it: a rung's label, On for `true`.
fn word(key: &str, value: &str) -> String {
    human_value(&super::facts::shown(key, value)).to_string()
}

/// A refusal from the store, in the row's words: the label rather than the
/// dotted key, and without the store's own prefix.
fn in_words(key: &str, error: &str) -> String {
    let error = error.strip_prefix("settings: ").unwrap_or(error);
    error.replace(&format!("`{key}`"), label(key))
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

/// The settings sheet's rows: one per setting in the category or the search.
///
/// **The sheet owns the section and the search.** The category and query
/// the rows are read through are copied from it on every build, so a Tab, a
/// click on a section chip and a typed letter all reach the same list.
pub(super) fn items(sheet: &mut Sheet, p: &mut Preferences, s: &ScreenState) -> Vec<Item> {
    if sheet.sections.is_empty() {
        sheet.sections = CATEGORIES.iter().map(|c| (*c).to_string()).collect();
        sheet.section = p.category.min(CATEGORIES.len() - 1);
        sheet.query = Some(p.query.clone());
    }
    p.observe(s);
    p.category = sheet.section.min(CATEGORIES.len() - 1);
    p.query = sheet.query.clone().unwrap_or_default();
    sheet.title = "Settings".into();
    sheet.crumbs = vec![CATEGORIES[p.category].to_string()];
    sheet.tools = vec![
        (
            Scope::Global.label().to_string(),
            Action::Scope(true),
            p.scope == Scope::Global,
        ),
        (
            Scope::Local.label().to_string(),
            Action::Scope(false),
            p.scope == Scope::Local,
        ),
    ];
    sheet.total = Some(
        crate::settings::specs()
            .iter()
            .filter(|spec| spec.key != "limits.task_tokens" && !crate::settings::hidden(spec.key))
            .count(),
    );
    if sheet.notice.is_empty() && !p.notice.is_empty() {
        sheet.notice = std::mem::take(&mut p.notice);
    }
    let focused = sheet.items.get(sheet.focus).map(|item| item.id.clone());
    let rows = p.rows();
    // Where a save lands, named beside the scope chips: a Global label must
    // never be able to conceal a Project write. Most choices apply at once;
    // the rows that wait for the next session say so themselves.
    let mut items = vec![
        Item::info(format!(
            "{} · choices save themselves; most apply now",
            saved_in(&p.path, p.scope)
        ))
        .tone(super::Tone::Muted),
    ];
    for (i, spec) in rows.iter().enumerate() {
        let id = format!("setting:{}", spec.key);
        let effective = p.effective(spec.key);
        let when = if crate::settings::applies_now(spec.key) {
            ""
        } else {
            " · next session"
        };
        let mut detail = format!("{}{when}", spec.description);
        // Editing Global under a project that sets the key changes nothing
        // here, and the row says so before the save rather than after.
        if p.scope == Scope::Global && p.origin(spec.key) == "project" {
            detail = format!(
                "This project sets {}; a Global value applies to other projects · {detail}",
                word(spec.key, &p.filed(spec.key))
            );
        }
        let is_focused = focused.as_deref() == Some(id.as_str());
        if is_focused {
            detail.push_str(" · ");
            detail.push_str(&whose(p, spec.key, &effective));
        }
        let options = Preferences::choices(spec);
        let editing = p
            .editing
            .as_ref()
            .filter(|(key, _)| key == spec.key)
            .map(|(_, value)| value.clone());
        let disabled =
            if p.scope == Scope::Local && crate::settings::registry::is_global_only(spec.key) {
                Some("Global only · F6 switches to Global".to_string())
            } else if let Some(slot) = spec
                .key
                .strip_prefix("agents.slots.")
                .and_then(|rest| rest.strip_suffix(".effort"))
                && p.effective(&format!("agents.slots.{slot}.model")) == "unset"
            {
                Some("Choose this favourite's model first".to_string())
            } else {
                None
            };
        let item = if let Some(value) = editing {
            Item::field(
                id,
                spec.label,
                Field {
                    cursor: value.len(),
                    text: value,
                    secret: false,
                    fresh: true,
                    list: spec.kind == Kind::List,
                },
            )
            .act(Action::Setting(i, None))
            .detail(format!("{} · Enter saves · Esc cancels", kind_words(spec)))
        } else if spec.kind == Kind::Model {
            let shown = if effective == "unset" {
                "choose a model".to_string()
            } else {
                effective.clone()
            };
            Item::open(
                id,
                format!("{} · {shown}", spec.label),
                Action::Setting(i, None),
            )
            .detail(detail)
        } else if options.is_empty() {
            let shown = if effective == "unset" {
                "not set".to_string()
            } else {
                effective.clone()
            };
            Item::open(
                id,
                format!("{} · {shown}", spec.label),
                Action::Setting(i, None),
            )
            .detail(detail)
        } else {
            let current = options.iter().position(|v| *v == effective);
            let values = options
                .iter()
                .map(|v| (word(spec.key, v), Action::Setting(i, Some(v.clone()))))
                .collect();
            // Helpers switched on with no model to run them on do nothing,
            // and the row says so instead of a bare On.
            if spec.key == "helpers.enabled"
                && effective == "true"
                && p.effective("helpers.model") == "unset"
            {
                detail = format!("On, but no helper model chosen · {detail}");
            }
            Item::value(id, spec.label, values, current).detail(detail)
        };
        items.push(item.disabled(disabled));
        if spec.key == "helpers.enabled"
            && effective == "true"
            && p.effective("helpers.model") == "unset"
        {
            items.push(
                Item::run(
                    "setting:helpers.enabled:choose",
                    "choose a helper model",
                    Action::SettingsAt(3),
                )
                .inline(),
            );
        }
        // The way back to Sterna's own value, where the focus is and only
        // when this scope holds a value to remove.
        if is_focused && p.saved(spec.key).is_some() {
            items.push(
                Item::run(
                    format!("setting:{}:default", spec.key),
                    "Use default",
                    Action::UseDefault(i),
                )
                .inline(),
            );
        }
    }
    if p.category == 4 && p.query.is_empty() {
        items.push(Item::open(
            "setting:agents:picker",
            "Favourites and the pinned model",
            Action::Command("/subagents".into()),
        ));
    }
    if rows.is_empty() {
        items.push(super::sheet::Item::info(format!(
            "No setting matches \"{}\". Backspace removes a letter; Esc clears the search.",
            p.query
        )));
    }
    items
}

/// What a field takes, in words.
fn kind_words(spec: &SettingSpec) -> &'static str {
    match spec.kind {
        Kind::Integer => "a whole number",
        Kind::Float => "a number",
        Kind::List => "a list, one per line or separated by commas",
        _ => "text",
    }
}

/// Where a row's value comes from, in the words a person uses for it --
/// never the dotted key, which nobody types here.
fn whose(p: &Preferences, key: &str, effective: &str) -> String {
    if effective == "unset" {
        return "not set · Sterna uses its own default".to_string();
    }
    let shown = word(key, effective);
    let filed = p.filed(key);
    if filed != effective {
        return format!(
            "{shown} · in force now, not saved; the settings say {}",
            word(key, &filed)
        );
    }
    match p.origin(key) {
        "built-in" => format!("{shown} · Sterna's own default"),
        "global" => format!("{shown} · set in your global settings"),
        "project" => format!("{shown} · set in this project's settings"),
        other => format!("{shown} · from {other}"),
    }
}

/// A setting's value as the sheet says it: a switch is On or Off.
pub(super) fn human_value(value: &str) -> &str {
    match value {
        "true" => "On",
        "false" => "Off",
        other => other,
    }
}

/// Where a choice is saved, in the words a person uses for it -- never a
/// temporary directory's full path.
fn saved_in(path: &std::path::Path, scope: Scope) -> String {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let shown = match home
        .as_deref()
        .and_then(|home| path.strip_prefix(home).ok())
    {
        Some(rest) => format!("~/{}", rest.display()),
        None => path
            .iter()
            .rev()
            .take(2)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect::<PathBuf>()
            .display()
            .to_string(),
    };
    match scope {
        Scope::Global => format!("Your settings, for every project · {shown}"),
        _ => format!("This project only · {shown}"),
    }
}
