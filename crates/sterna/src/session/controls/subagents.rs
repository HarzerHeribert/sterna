//! Human-only favorite edits. Persist and validate the whole assignment atomically.
use super::*;
use crate::settings::Store;

pub(super) fn assign(session: &Session<'_>, argument: &str) -> Result<String, String> {
    let store = super::store(session)?;
    let edits = edits(&store, session.selected_profile.as_deref(), argument)?;
    let loaded = super::save_home(session, &edits, session.selected_profile.as_deref())?;
    let mode = loaded.config.agents.mode.name();
    let count = loaded.config.agents.slots.len();
    session.config.borrow_mut().agents = loaded.config.agents;
    super::publish_tiers(session);
    Ok(format!(
        "Subagents: {mode} · {count} configured favorites · next launch uses this assignment; in-flight jobs unchanged"
    ))
}

#[cfg(test)]
fn edit(
    store: &Store,
    profile: Option<&str>,
    argument: &str,
) -> Result<crate::settings::Loaded, String> {
    use crate::settings::Scope;
    let edits = edits(store, profile, argument)?;
    let snapshot = store.read(Scope::Local)?;
    store.save_profile(Scope::Local, &snapshot, &edits, profile)
}

/// What `/subagents …` changes, as settings edits.
fn edits(
    store: &Store,
    profile: Option<&str>,
    argument: &str,
) -> Result<Vec<(String, Option<String>)>, String> {
    let words: Vec<_> = argument.split_whitespace().collect();
    let mut edits = Vec::new();
    match words.as_slice() {
        ["on" | "roster"] => {
            edits.push(("agents.model".into(), None));
            edits.push(("agents.mode".into(), Some("roster".into())));
        }
        ["off"] => {
            edits.push(("agents.model".into(), None));
            edits.push(("agents.mode".into(), Some("off".into())));
        }
        [slot, model] | [slot, model, _] if crate::config::SLOT_NAMES.contains(slot) => {
            let key = format!("agents.slots.{slot}");
            if *model == "off" {
                if words.len() != 2 { return Err("Removing a favorite takes no effort value.".into()); }
                edits.push((format!("{key}.model"), None));
                edits.push((format!("{key}.effort"), None));
                let old = store.load(profile)?;
                if old.config.agents.mode == AgentsMode::Roster && old.config.agents.slots.len() <= 1 {
                    edits.push(("agents.mode".into(), Some("off".into())));
                }
            } else {
                edits.push((format!("{key}.model"), Some((*model).into())));
                if let Some(effort) = words.get(2) {
                    edits.push((format!("{key}.effort"), Some((*effort).into())));
                }
            }
        }
        _ => return Err("Use /subagents on|off, or /subagents quick|balanced|deep|heavy MODEL [EFFORT]. Use MODEL=off to remove a favorite. Filling a slot never enables delegation.".into()),
    }
    Ok(edits)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::Scope;
    struct Temp(std::path::PathBuf);
    impl Temp {
        fn path(&self) -> &std::path::Path {
            &self.0
        }
    }
    impl Drop for Temp {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    fn tempdir() -> Temp {
        static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let path = std::env::temp_dir().join(format!(
            "sterna-favorites-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&path).unwrap();
        Temp(path)
    }
    #[test]
    fn slots_save_without_enabling_and_enable_only_explicitly() {
        let root = tempdir();
        let store = Store::with_global(root.path(), None).unwrap();
        assert!(edit(&store, None, "on").is_err());
        let loaded = edit(&store, None, "quick fixture-fast low").unwrap();
        assert_eq!(loaded.config.agents.mode, AgentsMode::Off);
        assert_eq!(loaded.config.agents.slots["quick"].model, "fixture-fast");
        let loaded = edit(&store, None, "on").unwrap();
        assert_eq!(loaded.config.agents.mode, AgentsMode::Roster);
        let loaded = edit(&store, None, "quick off").unwrap();
        assert_eq!(loaded.config.agents.mode, AgentsMode::Off);
        assert!(loaded.config.agents.slots.is_empty());
    }
    #[test]
    fn invalid_favorite_cannot_change_a_saved_roster() {
        let root = tempdir();
        let store = Store::with_global(root.path(), None).unwrap();
        edit(&store, None, "deep fixture-deep high").unwrap();
        let path = store.path(Scope::Local);
        let before = std::fs::read(&path).unwrap();
        assert!(edit(&store, None, "deep fixture-next default").is_err());
        assert!(edit(&store, None, "any fixture-next low").is_err());
        assert_eq!(before, std::fs::read(&path).unwrap());
    }
    #[test]
    fn favorite_model_and_effort_are_isolated_in_named_profile() {
        let root = tempdir();
        let store = Store::with_global(root.path(), None).unwrap();
        let loaded = edit(&store, Some("review"), "balanced fixture-review medium").unwrap();
        assert_eq!(
            loaded.config.agents.slots["balanced"].model,
            "fixture-review"
        );
        assert!(store.load(None).unwrap().config.agents.slots.is_empty());
    }
}
