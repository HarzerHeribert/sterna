//! Host settings coordination. Disk changes never widen a running sandbox.
use crate::{
    settings::{Scope, Store},
    tui,
};
use std::path::Path;

pub(crate) fn value<'a>(values: &'a toml::Value, key: &str) -> Option<&'a toml::Value> {
    key.split('.')
        .try_fold(values, |table, name| table.get(name))
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
    state.stream = word("ui.stream")
        .and_then(tui::Stream::parse)
        .unwrap_or_default();
    state.truecolor = crate::workbench::plumage::truecolor();
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
