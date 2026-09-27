//! `/wizard` (also `/setup`): the first-start wizard -- sign in, a model for each workload,
//! and Jev -- and the one-line reminder a session prints while a step is
//! still missing.
//!
//! **Every step is read from what is true now, never from a flag saying it
//! was done**: signed in means the gateway holds a login or a key, models
//! means the three tiers are set, Jev means a TypeSafe account can answer
//! and decisions are on. So a step undone elsewhere reappears here, and a
//! step done by hand needs no wizard.

use super::controls;
use super::*;
use crate::spend::Tier;
use crate::tui::{Panel, PanelRow};

/// The strongest models Sterna is tuned for, most preferred first; the first
/// one the gateway serves becomes the main and subagent model. ChatGPT's
/// lead Claude's because its terms allow third-party tools
/// (docs/subscriptions.md).
const MAIN: &[&str] = &[
    "gpt-6-sol",
    "claude-opus-5-5",
    "claude-fable-5-1",
    "claude-opus-5",
    "gpt-6-astra",
    "gpt-5-6-sol",
    "claude-sonnet-5",
];

/// Fast, cheap models for helpers -- summaries, checks, reductions.
const HELPERS: &[&str] = &[
    "gpt-6-luna",
    "claude-haiku-4-5",
    "claude-sonnet-5",
    "gpt-5-6-luna",
];

/// Sterna's recommended settings, versioned. **An entry is never edited in
/// place**: a change is a new entry with the next `since`, and
/// [`VERSION`] moves to it -- so a person who saw version N is shown exactly
/// the entries after N, each with its reason, once.
pub(super) struct Recommended {
    pub key: &'static str,
    pub value: &'static str,
    pub since: u32,
    pub why: &'static str,
    /// Offered only when Jev can answer: decisions without a decision model
    /// would switch on nothing.
    pub needs_jev: bool,
    current: fn(&crate::config::SternaConfig) -> String,
}

pub(super) const RECOMMENDED: &[Recommended] = &[
    Recommended {
        key: "helpers.enabled",
        value: "true",
        since: 1,
        why: "helpers read long outputs on a fast model, so the main model's context stays small and cheap",
        needs_jev: false,
        current: |config| config.helpers.enabled.to_string(),
    },
    Recommended {
        key: "decisions.mode",
        value: "on",
        since: 1,
        why: "Jev answers quick questions -- is this command safe, is the task stuck -- so fewer of them reach you",
        needs_jev: true,
        current: |config| config.decisions.mode.as_str().to_string(),
    },
];

/// The newest `since` in [`RECOMMENDED`].
pub(super) const VERSION: u32 = 1;

/// The recommendations newer than `seen` whose value differs from this
/// configuration's, with the value it has now.
pub(super) fn changes_since(
    seen: u32,
    config: &crate::config::SternaConfig,
    jev: bool,
) -> Vec<(&'static Recommended, String)> {
    RECOMMENDED
        .iter()
        .filter(|entry| entry.since > seen && (jev || !entry.needs_jev))
        .map(|entry| (entry, (entry.current)(config)))
        .filter(|(entry, current)| current != entry.value)
        .collect()
}

/// A model for each workload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Picks {
    pub main: String,
    pub helpers: String,
    pub subagents: String,
}

/// The recommended picks among `served`, or `None` when nothing is served.
/// A preference matches a served id by its normalised prefix, so a dated
/// release (`claude-haiku-4-5-20251001`) answers for its family.
pub(super) fn recommend(served: &[String]) -> Option<Picks> {
    let first = |preferences: &[&str]| {
        preferences.iter().find_map(|preference| {
            served
                .iter()
                .find(|id| crate::models::normalise(id).starts_with(preference))
                .cloned()
        })
    };
    let main = first(MAIN).or_else(|| served.first().cloned())?;
    let helpers = first(HELPERS).unwrap_or_else(|| main.clone());
    Some(Picks {
        subagents: main.clone(),
        main,
        helpers,
    })
}

/// What is done, read from the gateway and the configuration.
struct Progress {
    /// The subscriptions and providers signed in, by name; empty is not yet.
    signed_in: Vec<String>,
    /// The three tiers, when every one is set.
    models: Option<Picks>,
    /// A TypeSafe account can answer.
    jev_key: bool,
    /// Decisions are on and name a model.
    jev_on: bool,
    served: Vec<String>,
}

impl Progress {
    fn done(&self) -> usize {
        usize::from(!self.signed_in.is_empty())
            + usize::from(self.models.is_some())
            + usize::from(self.jev_key && self.jev_on)
    }
}

fn progress(session: &Session<'_>) -> Progress {
    let catalogue = controls::catalogue(session);
    let keys = controls::api_keys(session);
    let mut signed_in: Vec<String> = catalogue
        .as_ref()
        .map(controls::connected_subscriptions)
        .unwrap_or_default();
    signed_in.extend(
        keys.iter()
            .filter(|key| key.source.is_some() && key.provider != startup::DECISIONS_PROVIDER)
            .map(|key| key.provider.clone()),
    );
    let jev_key = keys
        .iter()
        .any(|key| key.provider == startup::DECISIONS_PROVIDER && key.source.is_some());
    let config = session.config();
    let models = match (
        config.model.parent.clone(),
        config
            .helpers
            .enabled
            .then(|| config.helpers.model.clone())
            .flatten(),
        (config.agents.mode == crate::config::AgentsMode::Pinned)
            .then(|| config.agents.model.clone())
            .flatten(),
    ) {
        (Some(main), Some(helpers), Some(subagents)) => Some(Picks {
            main,
            helpers,
            subagents,
        }),
        _ => None,
    };
    let jev_on = config.decisions.mode == crate::config::DecisionMode::On
        && config.decisions.model.is_some();
    let served = startup::served_accounts(session.gateway)
        .into_iter()
        .filter(|account| account.selectable != Some(false))
        .flat_map(|account| account.models)
        .collect();
    Progress {
        signed_in,
        models,
        jev_key,
        jev_on,
        served,
    }
}

const STEPS: usize = 3;

/// The wizard's overview: each step, whether it is done, and Enter to do it.
fn overview(progress: &Progress) -> Panel {
    let row = |text: String, command: &str| PanelRow::opens(text, command);
    let mark = |done: bool| if done { "✓" } else { "○" };
    let signed = if progress.signed_in.is_empty() {
        "a subscription or an API key".to_string()
    } else {
        progress.signed_in.join(", ")
    };
    let models = match (&progress.models, recommend(&progress.served)) {
        (Some(picks), _) => format!(
            "main {} · helpers {} · subagents {}",
            picks.main, picks.helpers, picks.subagents
        ),
        (None, Some(_)) => "recommended picks ready".to_string(),
        (None, None) => "after you sign in".to_string(),
    };
    let jev = match (progress.jev_key, progress.jev_on) {
        (true, true) => "on".to_string(),
        (true, false) => "key stored · turn decisions on".to_string(),
        (false, _) => "needs a TypeSafe key".to_string(),
    };
    Panel::rows(
        format!("Setup · {} of {STEPS} done", progress.done()),
        vec![
            row(
                format!(
                    "{} 1 Sign in · {signed}",
                    mark(!progress.signed_in.is_empty())
                ),
                "/login",
            ),
            row(
                format!(
                    "{} 2 Models for each workload · {models}",
                    mark(progress.models.is_some())
                ),
                "/wizard models",
            ),
            row(
                format!(
                    "{} 3 Jev, the decision model · {jev}",
                    mark(progress.jev_key && progress.jev_on)
                ),
                "/wizard jev",
            ),
        ],
    )
}

/// The models step: Sterna's recommended pick for each workload, or the
/// picker to choose each one.
fn models_panel(progress: &Progress) -> Panel {
    let Some(picks) = recommend(&progress.served) else {
        return Panel::rows(
            "Setup › Models",
            vec![PanelRow::opens(
                "Sign in first: the models come from what you sign in to",
                "/login",
            )],
        );
    };
    Panel::rows(
        "Setup › Models",
        vec![
            PanelRow::command(
                format!(
                    "Use Sterna's picks · main {} · helpers {} · subagents {}",
                    picks.main, picks.helpers, picks.subagents
                ),
                "/wizard models apply",
            ),
            PanelRow::info(
                "Sets these three models for every project, and helpers run on theirs. Jev is step 3.",
            ),
            PanelRow::opens("Choose each one myself", "/models"),
        ],
    )
}

/// The Jev step: what it is for, then the key and the switch.
fn jev_panel(progress: &Progress) -> Panel {
    let mut rows = vec![PanelRow::info(
        "Jev answers Sterna's quick decisions: whether a command in auto mode is safe to run without asking, whether a long task is stuck, what kind of task a request is. It is a TypeSafe model and needs a TypeSafe API key.",
    )];
    if !progress.jev_key {
        rows.push(PanelRow::opens(
            "Add a TypeSafe API key",
            format!("/key {}", startup::DECISIONS_PROVIDER),
        ));
    } else if !progress.jev_on {
        rows.push(PanelRow::command(
            "Turn decisions on with Jev",
            "/wizard jev on",
        ));
    } else {
        rows.push(PanelRow::info("Jev is on"));
    }
    Panel::rows("Setup › Jev", rows)
}

/// `/wizard [models [apply] | jev [on] | update [apply|keep]]`.
pub(super) fn command(session: &Session<'_>, argument: Option<&str>) {
    let words: Vec<&str> = argument.unwrap_or_default().split_whitespace().collect();
    match words.as_slice() {
        ["update", "apply"] => {
            let jev = progress(session).jev_key;
            apply_recommended(session, jev);
            controls::show(session, overview(&progress(session)));
        }
        ["update"] => {
            let jev = progress(session).jev_key;
            let seen = session.config().wizard.seen;
            let changes = changes_since(seen, &session.config(), jev);
            if changes.is_empty() {
                record_seen(session);
                controls::show(session, overview(&progress(session)));
            } else {
                controls::show(session, update_panel(&changes));
            }
        }
        ["update", "keep"] => {
            record_seen(session);
            controls::show(session, overview(&progress(session)));
        }
        ["models", "apply"] => apply_models(session),
        ["models"] => controls::show(session, models_panel(&progress(session))),
        ["jev", "on"] => jev_on(session),
        ["jev"] => controls::show(session, jev_panel(&progress(session))),
        _ => controls::show(session, overview(&progress(session))),
    }
}

/// Applies every recommendation that differs now, and records that this
/// version was seen.
fn apply_recommended(session: &Session<'_>, jev: bool) {
    let edits: Vec<(String, Option<String>)> = changes_since(0, &session.config(), jev)
        .into_iter()
        .map(|(entry, _)| (entry.key.to_string(), Some(entry.value.to_string())))
        .collect();
    if !edits.is_empty() {
        match controls::save_home(session, &edits, None) {
            Ok(loaded) => {
                let mut live = session.config.borrow_mut();
                live.helpers.enabled = loaded.config.helpers.enabled;
                live.decisions.mode = loaded.config.decisions.mode;
                drop(live);
                controls::publish_tiers(session);
            }
            Err(error) => session_println!("ERROR: {error}"),
        }
    }
    record_seen(session);
}

/// Records that the person has seen this version of the recommendations.
fn record_seen(session: &Session<'_>) {
    let edits = [("wizard.seen".to_string(), Some(VERSION.to_string()))];
    match controls::save_settings(session, crate::settings::Scope::Global, &edits) {
        Ok(_) => session.config.borrow_mut().wizard.seen = VERSION,
        Err(error) => session_println!("ERROR: {error}"),
    }
}

/// What changed in Sterna's recommended settings since the person last saw
/// them: each difference, why, and the choice to take them or keep theirs.
fn update_panel(changes: &[(&Recommended, String)]) -> Panel {
    let mut rows: Vec<PanelRow> = changes
        .iter()
        .map(|(entry, current)| {
            PanelRow::info(format!(
                "{}: {current} → {} · {}",
                entry.key, entry.value, entry.why
            ))
        })
        .collect();
    rows.push(PanelRow::command(
        "Apply Sterna's new settings",
        "/wizard update apply",
    ));
    rows.push(PanelRow::command("Keep mine", "/wizard update keep"));
    Panel::rows("Sterna's recommended settings changed", rows)
}

fn apply_models(session: &Session<'_>) {
    let Some(picks) = recommend(&progress(session).served) else {
        controls::show(session, models_panel(&progress(session)));
        return;
    };
    if let Err(error) = apply_picks(session, &picks) {
        session_println!("ERROR: {error}");
        return;
    }
    controls::show(session, overview(&progress(session)));
}

/// Sets the three models the way `/model` does: saved for every project and
/// in force now -- the header, the sidebar and /status name the main one at
/// once. Nothing else is switched on: helpers run on the helper model they
/// are given, and Jev stays step 3.
fn apply_picks(session: &Session<'_>, picks: &Picks) -> Result<(), String> {
    for (tier, model) in [
        (Tier::Parent, &picks.main),
        (Tier::Helpers, &picks.helpers),
        (Tier::Subagents, &picks.subagents),
    ] {
        controls::use_model(session, tier, model)?;
    }
    Ok(())
}

fn jev_on(session: &Session<'_>) {
    let edits = [
        ("decisions.mode".to_string(), Some("on".to_string())),
        (
            "decisions.model".to_string(),
            Some(crate::decide::DEFAULT_MODEL.to_string()),
        ),
    ];
    match controls::save_home(session, &edits, None) {
        Ok(loaded) => {
            let mut live = session.config.borrow_mut();
            live.decisions.mode = loaded.config.decisions.mode;
            live.decisions.model = loaded.config.decisions.model;
        }
        Err(error) => {
            session_println!("ERROR: {error}");
            return;
        }
    }
    controls::show(session, overview(&progress(session)));
}

/// At the start of a terminal session: the wizard itself when no model is
/// chosen yet, otherwise one line naming what is still missing.
pub(super) fn at_start(session: &Session<'_>, no_model: bool) {
    if session.ui.is_none() {
        if no_model {
            session_println!("No model selected yet: pick one, or type /model <id>.");
        }
        return;
    }
    let progress = progress(session);
    // Never a sheet at start: a sheet would take the first keys the person
    // types. The way into setup is a chip on the opening card.
    if no_model {
        session_println!("No model selected yet: finish setup, or type /model <id>.");
    }
    // After an update that changed the recommendations: one line, never a
    // panel -- a panel at start would take the first keys the person types.
    let seen = session.config().wizard.seen;
    if seen < VERSION && !changes_since(seen, &session.config(), progress.jev_key).is_empty() {
        session_println!(
            "Sterna's recommended settings changed · /wizard update shows what and why"
        );
        return;
    }
    offer_for(session, &progress);
}

/// The opening screen's way back into setup, recomputed after anything that
/// can finish a step -- a sign-in, a key, a model, the wizard itself -- and
/// taken away once every step is done.
pub(super) fn offer(session: &Session<'_>) {
    if session.ui.is_some() {
        offer_for(session, &progress(session));
    }
}

fn offer_for(session: &Session<'_>, progress: &Progress) {
    let done = progress.done();
    let Some(ui) = session.ui else {
        if done < STEPS {
            session_println!("Setup: {done} of {STEPS} done · /wizard finishes it");
        }
        return;
    };
    if done < STEPS {
        let left = STEPS - done;
        // On the opening screen it is the first thing to press, not a note
        // that also rode the dock and the card.
        ui.suggest(
            &format!(
                "finish setup · {left} step{} left",
                if left == 1 { "" } else { "s" }
            ),
            "/wizard",
        );
    } else {
        ui.unsuggest("/wizard");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Sterna's picks are saved for every project and are in force now:
    /// the project folder is left alone and the session answers on the
    /// main pick at once. Nothing else is switched on.
    #[test]
    fn sternas_picks_are_saved_globally_and_in_force_now() {
        let root = std::env::temp_dir().join(format!("sterna-picks-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        super::super::controls::tests::with_session(&root, |session| {
            let picks = Picks {
                main: "claude-opus-4-8".into(),
                helpers: "gpt-5.6-luna".into(),
                subagents: "claude-sonnet-5".into(),
            };
            apply_picks(session, &picks).unwrap();
            let store =
                crate::settings::Store::with_global(&root, Some(root.join("user-settings")))
                    .unwrap();
            let global = store.read(crate::settings::Scope::Global).unwrap();
            assert_eq!(
                global.values["model"]["parent"].as_str(),
                Some("claude-opus-4-8")
            );
            assert!(
                !root.join(".sterna").join("config.toml").exists(),
                "setup writes nothing into the project"
            );
            assert_eq!(*session.model.borrow(), "claude-opus-4-8");
            assert_ne!(
                session.config().decisions.mode,
                crate::config::DecisionMode::On,
                "Jev stays step 3"
            );
        });
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn sterna_picks_its_tuned_models_from_what_is_served() {
        let served = |ids: &[&str]| ids.iter().map(|id| (*id).to_string()).collect::<Vec<_>>();
        assert_eq!(
            recommend(&served(&["gpt-6-luna", "gpt-6-sol", "claude-opus-5-5"])),
            Some(Picks {
                main: "gpt-6-sol".into(),
                helpers: "gpt-6-luna".into(),
                subagents: "gpt-6-sol".into(),
            }),
            "ChatGPT leads; helpers get the fast model"
        );
        let claude = recommend(&served(&["claude-haiku-4-5-20251001", "claude-opus-5-5"])).unwrap();
        assert_eq!(claude.main, "claude-opus-5-5");
        assert_eq!(
            claude.helpers, "claude-haiku-4-5-20251001",
            "a dated release answers for its family"
        );
        let unknown = recommend(&served(&["some-local-model"])).unwrap();
        assert_eq!(unknown.main, "some-local-model");
        assert_eq!(unknown.helpers, "some-local-model");
        assert_eq!(recommend(&[]), None);
    }

    #[test]
    fn an_update_shows_only_the_recommendations_newer_than_the_ones_seen() {
        let mut config = crate::config::SternaConfig::default();
        config.helpers.enabled = false;
        let changed = changes_since(0, &config, false);
        let keys: Vec<&str> = changed.iter().map(|(entry, _)| entry.key).collect();
        assert_eq!(keys, ["helpers.enabled"], "decisions only with Jev");
        assert_eq!(
            changed[0].1, "false",
            "the person's own value is shown beside Sterna's"
        );
        assert_eq!(
            changes_since(0, &config, true).len(),
            2,
            "with Jev, decisions are recommended too"
        );
        assert!(
            changes_since(VERSION, &config, true).is_empty(),
            "seen once, asked once"
        );
        config.helpers.enabled = true;
        assert!(
            changes_since(0, &config, false).is_empty(),
            "already Sterna's value: nothing to show"
        );
        // Every entry is dated no later than the version it ships in.
        assert!(RECOMMENDED.iter().all(|entry| entry.since <= VERSION));
        assert_eq!(
            RECOMMENDED.iter().map(|entry| entry.since).max(),
            Some(VERSION)
        );
    }
}
