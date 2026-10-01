//! The clients a session opens with: its terminal, or a port with nobody at
//! a terminal (`--serve`), or none at all (a scripted run). Either way the
//! session reaches them only through its seam (`engine`).

use std::io::{self, IsTerminal};
use std::path::PathBuf;

use super::{SessionArgs, Transcript, controls, startup, ui};
use crate::config::SternaConfig;
use crate::sandbox::profile::Profile;
use crate::tui;

/// What the session's clients are first shown of it.
pub(super) struct Seed<'a> {
    pub(super) session: &'a str,
    pub(super) started_on: Option<String>,
    pub(super) level: &'a crate::permissions::LiveLevel,
    pub(super) effort: crate::wire::Effort,
    pub(super) profile: &'a Profile,
    pub(super) config: &'a SternaConfig,
    pub(super) values: &'a toml::Value,
    pub(super) served_models: Vec<String>,
}

/// The folder the session runs in, resolved, and its clients: the terminal
/// when there is one, the port alone when served, and none for a script.
pub(super) fn open(
    args: &SessionArgs,
    transcript: &Transcript,
    seed: Seed<'_>,
) -> Result<(Option<ui::LiveUi>, PathBuf), String> {
    let folder = std::fs::canonicalize(&args.root).unwrap_or_else(|_| args.root.clone());
    let opening = || ui::Opening {
        session: seed.session.to_string(),
        root: folder.clone(),
        facts: crate::engine::wire::Facts {
            model: seed.started_on.clone(),
            effort: seed.effort.name().into(),
            level: seed.level.level().name().into(),
            root: folder.to_string_lossy().into_owned(),
            project: Some(startup::project_name(&args.root)),
            subagents: Some(controls::tier_status(seed.config)),
            ..crate::engine::wire::Facts::default()
        },
        saves: crate::engine::hub::Saves {
            root: Some(args.root.clone()),
            global: crate::project::workflows::user_directory(),
        },
        suggestions: crate::workbench::voice::project_suggestions(&args.root),
    };
    let conversation = transcript.conversation.clone();
    let notebook = transcript.notebook.clone();
    if args.serve {
        return Ok((
            Some(ui::LiveUi::serve(conversation, notebook, opening())?),
            folder,
        ));
    }
    if args.task.is_some() || !io::stdin().is_terminal() || !io::stdout().is_terminal() {
        return Ok((None, folder));
    }
    let mut state = tui::ScreenState {
        model: seed.started_on.clone(),
        level: seed.level.clone(),
        effort: seed.effort,
        settings_root: Some(args.root.clone()),
        settings_global: crate::project::workflows::user_directory(),
        settings_profile: args.profile.clone(),
        compact: true,
        pretty: true,
        project: Some(startup::project_name(&args.root)),
        // The mechanism, for the surface that explains the boundary. It is
        // no longer the status line's words: `3p/1c` is a path-rule count
        // and a command-pattern count, and nothing on a status line could
        // ever have said so.
        sandbox: Some(format!(
            "{} path rules · {} pre-approved commands",
            seed.profile.rule_count(),
            seed.profile.pre_approved().len(),
        )),
        subagents: Some(controls::tier_status(seed.config)),
        // The third half, which until 2026-09-19 no surface carried: a rung
        // and a grant are two choices, and whether Sterna confines what it
        // spawns is the one that decided whether `cargo test` could link.
        confinement: Some(
            crate::tools::invoke::Confinement::for_session(seed.profile)
                .map_or("no-applier", crate::tools::invoke::Confinement::short)
                .to_string(),
        ),
        // The shell never has a network; the field names the host tools
        // that do (map 2657, design §8).
        network: Some(seed.config.web.posture().into()),
        ..tui::ScreenState::default()
    };
    crate::settings_session::presentation(&mut state, seed.values);
    state.settings_models = seed.served_models.clone();
    state.local_hour = crate::workbench::voice::local_hour();
    if let Some(root) = state.settings_root.clone() {
        state.suggestions = crate::workbench::voice::project_suggestions(&root);
    }
    Ok((
        Some(ui::LiveUi::start(state, conversation, notebook, opening())?),
        folder,
    ))
}
