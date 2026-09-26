//! Host settings coordination. Disk changes never widen a running sandbox.
use crate::{
    settings::{Scope, Snapshot, Store},
    settings_ui::{Action, Row, SettingsPanel},
    tui,
};
use std::path::{Path, PathBuf};

pub(crate) fn value<'a>(values: &'a toml::Value, key: &str) -> Option<&'a toml::Value> {
    key.split('.')
        .try_fold(values, |table, name| table.get(name))
}
fn display(value: &toml::Value) -> String {
    value
        .as_str()
        .map(str::to_owned)
        .unwrap_or_else(|| value.to_string())
}
pub(crate) fn presentation(state: &mut tui::ScreenState, values: &toml::Value) {
    let word = |key| value(values, key).and_then(toml::Value::as_str);
    state.theme = word("ui.theme")
        .and_then(tui::Theme::parse)
        .unwrap_or_else(tui::Theme::natural);
    state.status_line = match word("ui.statusline") {
        Some("compact") => tui::StatusLine::Compact,
        Some("hide" | "hidden") => tui::StatusLine::Hidden,
        _ => tui::StatusLine::Full,
    };
    state.sidebar = match word("ui.sidebar") {
        Some("show") => tui::SidebarVisibility::Shown,
        Some("hide") => tui::SidebarVisibility::Hidden,
        _ => tui::SidebarVisibility::Auto,
    };
    // Off when either key says so: `ui.reduced_motion` predates the level.
    let reduced = value(values, "ui.reduced_motion")
        .and_then(toml::Value::as_bool)
        .unwrap_or(false);
    state.set_motion(if reduced {
        tui::Motion::Off
    } else {
        word("ui.motion")
            .and_then(tui::Motion::parse)
            .unwrap_or_default()
    });
    state.voice = word("ui.voice")
        .and_then(tui::Voice::parse)
        .unwrap_or_default();
    state.stream = word("ui.stream")
        .and_then(tui::Stream::parse)
        .unwrap_or_default();
    state.truecolor = crate::workbench::plumage::truecolor();
}
pub(crate) struct Editor {
    pub panel: SettingsPanel,
    root: PathBuf,
    scope: usize,
    profile: Option<String>,
    status_only: bool,
    snapshot: Snapshot,
    models: Vec<String>,
}
impl Editor {
    pub fn open(state: &tui::ScreenState, status_only: bool) -> Result<Self, String> {
        Self::new(
            state
                .settings_root
                .clone()
                .ok_or("settings unavailable without a project root")?,
            1,
            state.settings_profile.clone(),
            status_only,
            state.settings_models.clone(),
        )
    }
    fn new(
        root: PathBuf,
        scope: usize,
        profile: Option<String>,
        status_only: bool,
        models: Vec<String>,
    ) -> Result<Self, String> {
        let store = Store::new(&root)?;
        let selected = if scope == 0 {
            Scope::Global
        } else {
            Scope::Local
        };
        let snapshot = store.read(selected)?;
        let loaded = store.load(profile.as_deref())?;
        let rows = crate::settings::specs()
            .iter()
            .filter(|s| {
                if status_only {
                    s.key == "ui.statusline"
                } else {
                    s.basic
                }
            })
            .map(|s| {
                let saved = value(&snapshot.values, s.key);
                let effective = value(&loaded.values, s.key);
                let shown = saved
                    .or(effective)
                    .map(display)
                    .unwrap_or_else(|| "unset".into());
                let mut choices = s.choices.iter().map(|x| x.to_string()).collect::<Vec<_>>();
                if choices.is_empty()
                    && (effective.is_some_and(toml::Value::is_bool)
                        || matches!(s.key, "helpers.enabled" | "ui.reduced_motion"))
                {
                    choices = vec!["false".into(), "true".into()];
                }
                if s.key.ends_with(".model") || s.key == "model.parent" {
                    choices = models.clone();
                    if !choices.contains(&shown) && shown != "unset" {
                        choices.insert(0, shown.clone());
                    }
                }
                Row {
                    key: s.key.into(),
                    label: s.label.into(),
                    description: format!(
                        "{} · Saved: {} · Effective: {} ({}){}",
                        s.description,
                        saved.map(display).unwrap_or_else(|| "inherited".into()),
                        effective.map(display).unwrap_or_else(|| "unset".into()),
                        loaded
                            .origins
                            .get(s.key)
                            .map(String::as_str)
                            .unwrap_or("built-in"),
                        if s.key.starts_with("ui.") {
                            ""
                        } else {
                            " · New session required"
                        }
                    ),
                    value: shown,
                    origin: if saved.is_some() {
                        if scope == 0 { "global" } else { "project" }.into()
                    } else {
                        loaded
                            .origins
                            .get(s.key)
                            .cloned()
                            .unwrap_or_else(|| "built-in".into())
                    },
                    choices,
                    restart: !s.key.starts_with("ui."),
                }
            })
            .collect();
        let mut panel = SettingsPanel::new(scope, store.path(selected).display().to_string(), rows);
        if profile.is_some() {
            panel.notice("Editing base scope; the selected profile may override these values. Runtime changes require a new session.".into());
        } else if !loaded.notices.is_empty() {
            panel.notice(loaded.notices.join(" · "));
        }
        Ok(Self {
            panel,
            root,
            scope,
            profile,
            status_only,
            snapshot,
            models,
        })
    }
    /// True closes the editor. Failed writes keep staged edits on screen.
    pub fn key(&mut self, key: crossterm::event::KeyEvent, state: &mut tui::ScreenState) -> bool {
        match self.panel.key(key) {
            Action::Cancel => true,
            Action::None => false,
            Action::SwitchScope(scope) => {
                match Self::new(
                    self.root.clone(),
                    scope,
                    self.profile.clone(),
                    self.status_only,
                    self.models.clone(),
                ) {
                    Ok(next) => *self = next,
                    Err(error) => self.panel.notice(error),
                }
                false
            }
            Action::Apply => {
                let result = (|| {
                    let store = Store::new(&self.root)?;
                    store.save(
                        if self.scope == 0 {
                            Scope::Global
                        } else {
                            Scope::Local
                        },
                        &self.snapshot,
                        &self.panel.edits(),
                    )?;
                    store.load(self.profile.as_deref())
                })();
                match result {
                    Ok(loaded) => {
                        presentation(state, &loaded.values);
                        state.note("Settings saved. Presentation is active; runtime and permission changes require a new session. CLI/profile overrides still apply.");
                        true
                    }
                    Err(error) => {
                        self.panel.notice(error);
                        false
                    }
                }
            }
        }
    }
}
pub(crate) fn save_status(state: &mut tui::ScreenState, word: &str) -> Result<(), String> {
    let store = Store::new(
        state
            .settings_root
            .as_deref()
            .ok_or("settings root unavailable")?,
    )?;
    let snapshot = store.read(Scope::Local)?;
    store.save(
        Scope::Local,
        &snapshot,
        &[(
            "ui.statusline".into(),
            Some(if word == "hidden" { "hide" } else { word }.into()),
        )],
    )?;
    let loaded = store.load(state.settings_profile.as_deref())?;
    presentation(state, &loaded.values);
    Ok(())
}

pub(crate) fn permissions(root: &Path, argument: Option<&str>) -> Result<String, String> {
    let store = Store::new(root)?;
    if let Some(argument) = argument.filter(|a| !a.trim().is_empty()) {
        let (action, rule) = argument
            .split_once(' ')
            .ok_or("Use /permissions allow|remove <rule>")?;
        if !matches!(action, "allow" | "remove") || rule.trim().is_empty() {
            return Err("Use /permissions allow|remove <rule>".into());
        }
        let snapshot = store.read(Scope::Local)?;
        let mut rules = value(&snapshot.values, "permissions.allow")
            .and_then(toml::Value::as_array)
            .cloned()
            .unwrap_or_default();
        let rule = toml::Value::String(rule.trim().into());
        if action == "allow" {
            if !rules.contains(&rule) {
                rules.push(rule);
            }
        } else {
            rules.retain(|r| r != &rule);
        }
        store.save(
            Scope::Local,
            &snapshot,
            &[(
                "permissions.allow".into(),
                Some(toml::Value::Array(rules).to_string()),
            )],
        )?;
    }
    Ok(format!(
        "Persisted next-session settings\n{}\n{}\nPersisted edits apply to the next session only. Running sandbox unchanged.\nClaude settings: /config import claude (preview), then /config import claude --apply",
        store.path(Scope::Local).display(),
        store
            .permissions()?
            .unwrap_or_else(|| "No native permission rules.".into())
    ))
}
