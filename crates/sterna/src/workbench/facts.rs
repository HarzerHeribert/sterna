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

/// The level, set now and saved globally; returns the notice every route
/// prints. Full access is confirmed before it gets here (the Confirm sheet,
/// which opens on Cancel), whichever route asked for it.
pub fn set_level(s: &mut ScreenState, level: Level) -> String {
    s.level.set(level);
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

/// The effort as the chip names it, with what is actually sent when that
/// differs: `effort default (low)` on a model that has its own default.
pub fn effort_word(s: &ScreenState) -> String {
    let sent = s.effort.sent_for(s.model.as_deref().unwrap_or(""));
    if sent == s.effort {
        format!("effort {}", s.effort.name())
    } else {
        format!("effort {} ({})", s.effort.name(), sent.name())
    }
}

/// The next effort on the ladder, from the stored value -- never from the
/// value a model is sent, or a first step could land where it started.
pub fn next_effort(effort: Effort) -> Effort {
    match effort {
        Effort::Default => Effort::Low,
        Effort::Low => Effort::Medium,
        Effort::Medium => Effort::High,
        Effort::High => Effort::Xhigh,
        Effort::Xhigh => Effort::Max,
        Effort::Max => Effort::Default,
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
        _ => value.to_string(),
    }
}
