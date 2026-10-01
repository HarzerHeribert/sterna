//! The session facts a person changes most -- the sandbox level and the
//! reasoning effort -- each with **one setter** that every route reaches (a
//! chip, a sheet row, a settings row and a typed command) and **one name**
//! every screen prints.
//!
//! Each setter changes the running session and saves the choice, so the
//! next session starts where this one was left. The level is saved to the
//! global settings only: a project file must never be able to lower it. The
//! effort is saved to the global settings unless Settings is open on Project
//! scope (decision 6). The effort belongs to the session thread, so every
//! route reaches it as the command that thread has always answered, and
//! [`saving`] is where that command is saved on its way out.
use crate::permissions::Level;
use crate::settings::{Scope, Store};
use crate::tui::ScreenState;
use crate::wire::Effort;

/// Saves one fact to `scope`. A session with no project root or no user
/// settings folder (a test, a bare terminal) changes the running session
/// only.
fn persist(s: &ScreenState, scope: Scope, key: &str, value: &str) -> Result<(), String> {
    let Some(root) = s.settings_root.as_deref() else {
        return Ok(());
    };
    if scope == Scope::Global && s.settings_global.is_none() {
        return Ok(());
    }
    let store = Store::with_global(root, s.settings_global.clone())?;
    let snapshot = store.read(scope)?;
    store
        .save(
            scope,
            &snapshot,
            &[(key.to_string(), Some(value.to_string()))],
        )
        .map(|_| ())
}

/// A global list setting as the file holds it, `None` when it is unset or
/// cannot be read.
pub fn global_list(s: &ScreenState, key: &str) -> Option<Vec<String>> {
    let root = s.settings_root.as_deref()?;
    s.settings_global.as_ref()?;
    let store = Store::with_global(root, s.settings_global.clone()).ok()?;
    let snapshot = store.read(Scope::Global).ok()?;
    crate::settings_session::value(&snapshot.values, key)
        .and_then(toml::Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(toml::Value::as_str)
                .map(str::to_string)
                .collect()
        })
}

/// Saves a whole list to the global settings: an empty one is saved as
/// empty, which for `sandbox.ecosystems` means none rather than all.
pub fn save_list(s: &ScreenState, key: &str, values: &[String]) -> Result<(), String> {
    let array = toml::Value::Array(values.iter().cloned().map(toml::Value::String).collect());
    persist(s, Scope::Global, key, &array.to_string())
}

/// "Always allow": the hosts join the global `sandbox.hosts`, once each.
/// The live proxy already has them (the gate added them); this is the part
/// that outlives the session. Returns the notice.
pub fn keep_hosts(s: &ScreenState, hosts: &[String]) -> String {
    let mut kept = global_list(s, "sandbox.hosts").unwrap_or_default();
    for host in hosts {
        if !kept.iter().any(|h| h.eq_ignore_ascii_case(host)) {
            kept.push(host.clone());
        }
    }
    let names = hosts.join(", ");
    let is = if hosts.len() == 1 { "is" } else { "are" };
    match save_list(s, "sandbox.hosts", &kept) {
        Ok(()) => format!("{names} {is} allowed in every session from now on."),
        Err(error) => format!("{names} {is} allowed for this session only: {error}"),
    }
}

/// The level, set now and saved globally; returns the notice every route
/// prints. Full access is confirmed before it gets here (the Confirm sheet,
/// which opens on Cancel), whichever route asked for it.
pub fn set_level(s: &mut ScreenState, level: Level) -> String {
    s.set_level(level);
    match persist(s, Scope::Global, "sandbox.level", level.name()) {
        Ok(()) => level.now(),
        Err(error) => format!("{} · for this session only: {error}", level.now()),
    }
}

/// A command on its way to the session: an `/effort` that names a value is
/// saved here, once, whichever route produced it.
pub fn saving(s: &ScreenState, command: &str, scope: Scope) {
    let words: Vec<_> = command.split_whitespace().collect();
    if let ["/effort", word] = words.as_slice()
        && let Some(effort) = Effort::parse(word)
    {
        let _ = persist(s, scope, "session.effort", effort.name());
    }
}

/// The level as the chip names it.
pub fn level_word(s: &ScreenState) -> String {
    match s.level.level() {
        Level::Full => "▲ Full access".to_string(),
        level => format!("◼ {}", level.label()),
    }
}

/// The effort as the model chip carries it: `auto`, or `auto (low)` when
/// the model is sent something else.
pub fn effort_short(s: &ScreenState) -> String {
    let sent = s.effort.sent_for(s.model.as_deref().unwrap_or(""));
    if sent == s.effort {
        s.effort.name().to_string()
    } else {
        format!("{} ({})", s.effort.name(), sent.name())
    }
}

/// The effort as the chip names it, with what is actually sent when that
/// differs: `effort auto (low)` on a model that has its own default.
pub fn effort_word(s: &ScreenState) -> String {
    let sent = s.effort.sent_for(s.model.as_deref().unwrap_or(""));
    if sent == s.effort {
        format!("effort {}", s.effort.name())
    } else {
        format!("effort {} ({})", s.effort.name(), sent.name())
    }
}

/// A settings value as the screen names it: a level's label, and
/// `favourites` for the subagent roster.
pub fn shown(key: &str, value: &str) -> String {
    match key {
        "sandbox.level" => {
            Level::parse(value).map_or_else(|| value.to_string(), |l| l.label().to_string())
        }
        "agents.mode" if value == "roster" => "favourites".to_string(),
        // **A choice says what it does.** The files keep their words; the
        // screen shows what each one means.
        "decisions.mode" if value == "shadow" => "watch only".to_string(),
        "ask.jev" => match value {
            "off" => "ask me",
            "weight" => "show Jev's guess",
            "decide" => "Jev answers when sure",
            other => other,
        }
        .to_string(),
        _ => value.to_string(),
    }
}
