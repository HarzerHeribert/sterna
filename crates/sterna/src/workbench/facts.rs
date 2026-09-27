//! The session facts a person changes most -- how often Sterna asks, the
//! work mode, the reasoning effort -- each with **one setter** that every
//! route reaches (a chip, a sheet row, a settings row, Shift-Tab and a typed
//! command) and **one name** every screen prints.
//!
//! Each setter changes the running session and saves the choice, so the
//! next session starts where this one was left. It is saved to the global
//! settings unless Settings is open on Project scope (decision 6: a project
//! file is written only when Project scope is chosen there). The mode and
//! the effort belong to the session thread, so every route reaches them as
//! the command that thread has always answered, and [`saving`] is where
//! that command is saved on its way out.
use crate::permissions::Rung;
use crate::settings::{Scope, Store};
use crate::tui::{Mode, ScreenState};
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

/// The rung, set now and saved; returns the notice every route prints.
/// Never asks is confirmed before it gets here (the Confirm sheet, which
/// opens on Cancel), whichever route asked for it.
pub fn set_rung(s: &mut ScreenState, rung: Rung, scope: Scope) -> String {
    s.permissions.set(rung);
    match persist(s, scope, "permissions.mode", rung.name()) {
        Ok(()) => rung.now(),
        Err(error) => format!("{} · for this session only: {error}", rung.now()),
    }
}

/// A command on its way to the session: a `/mode` or `/effort` that names
/// a value is saved here, once, whichever route produced it. `/mode auto`
/// saves nothing, because the file has no word for an unpinned mode.
pub fn saving(s: &ScreenState, command: &str, scope: Scope) {
    let words: Vec<_> = command.split_whitespace().collect();
    let _ = match words.as_slice() {
        ["/mode", word] => Mode::parse(word).map_or(Ok(()), |mode| {
            persist(s, scope, "session.mode", mode.setting())
        }),
        ["/effort", word] => Effort::parse(word).map_or(Ok(()), |effort| {
            persist(s, scope, "session.effort", effort.name())
        }),
        _ => Ok(()),
    };
}

/// The command that sets the work mode; `None` is Auto.
pub fn mode_command(mode: Option<Mode>) -> String {
    format!("/mode {}", mode.map_or("auto", |mode| mode.setting()))
}

/// The mode as the chip names it: `Build`, or `Build · auto` while it is
/// not pinned.
pub fn mode_word(s: &ScreenState) -> String {
    if s.mode_pinned {
        s.mode.label().to_string()
    } else {
        format!("{} · auto", s.mode.label())
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

/// A settings value as the screen names it: a rung's label, a mode's, and
/// `favourites` for the subagent roster.
pub fn shown(key: &str, value: &str) -> String {
    match key {
        "permissions.mode" => {
            Rung::parse(value).map_or_else(|| value.to_string(), |r| r.label().to_string())
        }
        "session.mode" => {
            Mode::parse(value).map_or_else(|| value.to_string(), |m| m.label().to_string())
        }
        "agents.mode" if value == "roster" => "favourites".to_string(),
        _ => value.to_string(),
    }
}
