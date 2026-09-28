//! Human-invoked session inspection and configuration. No model dispatch.
use super::*;
use crate::config::AgentsMode;
pub(super) mod sign_in;
mod subagents;
#[cfg(test)]
use crate::config::SternaConfig;
use crate::spend::Tier;
use crate::tui::{Panel, PanelRow, TierModels};

/// The cell ceiling as a person reads it. `None` is the default and means
/// there is none: a task ends on evidence that it has stopped producing
/// anything, not on a count of cells.
fn cell_limit(limits: &crate::config::Limits) -> String {
    match limits.cells {
        Some(cap) => format!("{cap} cells"),
        None => "none".to_string(),
    }
}

pub(super) fn show(session: &Session<'_>, panel: Panel) {
    if let Some(ui) = session.ui {
        ui.panel(panel);
    } else {
        session_println!(
            "{}\n{}",
            panel.title,
            panel
                .rows
                .iter()
                .map(|r| r.text.as_str())
                .collect::<Vec<_>>()
                .join("\n")
        );
    }
}

#[derive(serde::Deserialize)]
pub(super) struct Catalogue {
    version: u32,
    accounts: Vec<Account>,
}
#[derive(serde::Deserialize)]
struct Account {
    account: String,
    provider: Option<String>,
    models: Vec<String>,
    scope: String,
    selectable: Option<bool>,
    unavailable_reason: Option<String>,
    /// Whether this account holds a credential. `None` for one that is not
    /// connectable at all, such as a provider reached with an API key.
    #[serde(default)]
    authenticated: Option<bool>,
    /// The provider whose flow would connect it.
    #[serde(default)]
    connect_with: Option<String>,
    /// Whether it is in its pool; every account is unless taken out.
    #[serde(default)]
    pooled: Option<bool>,
}

pub(super) fn models(session: &Session<'_>) {
    models_at(session, Tier::Parent);
}

/// The model picker, open on `tier`'s section: `/models`, bare `/subagents`
/// and `/model helper` alike.
pub(super) fn models_at(session: &Session<'_>, tier: Tier) {
    let mut catalogue = session
        .gateway
        .run(&["entitlements", "--json", "--refresh"], None)
        .and_then(|bytes| serde_json::from_slice::<Catalogue>(&bytes).ok());
    let keys = api_keys(session);
    if let Some(catalogue) = &mut catalogue {
        for account in &mut catalogue.accounts {
            let missing_key = keys.iter().any(|key| {
                account.provider.as_deref() == Some(key.provider.as_str()) && key.source.is_none()
            });
            if account.authenticated == Some(false)
                || (account.authenticated.is_none() && missing_key)
            {
                account.selectable = Some(false);
                account.unavailable_reason = Some(
                    if account.authenticated == Some(false) {
                        "Connect this subscription in /login first."
                    } else {
                        "No credential for this provider; configure it in /login first."
                    }
                    .into(),
                );
            }
        }
    }
    // The gateway owns measurements too; standalone Sterna needs no Glasshouse process.
    let scores: std::collections::BTreeMap<String, f64> = crate::models::published(session.gateway)
        .into_iter()
        .filter_map(|(id, facts)| {
            facts
                .intelligence
                .filter(|n| n.is_finite())
                .map(|n| (id, n))
        })
        .collect();
    if catalogue.is_none() {
        show(session, unreachable_panel(session, "Models", "/model"));
        return;
    }
    let mut panel = model_panel(catalogue, tier_models(session)).with_intelligence(scores);
    if let Some(assignment) = panel.assignment.as_mut() {
        assignment.active = tier;
    }
    show(session, panel);
}

/// Words laid into lines no wider than `width`, continuation lines indented
/// under a `Why:`/`Fix:` label.
fn wrap_line(text: &str, width: usize) -> Vec<String> {
    let indent = if text.starts_with("Why:") || text.starts_with("Fix:") {
        "     "
    } else {
        ""
    };
    let mut lines: Vec<String> = Vec::new();
    let mut line = String::new();
    for word in text.split_whitespace() {
        if !line.is_empty() && line.chars().count() + 1 + word.chars().count() > width {
            lines.push(std::mem::replace(&mut line, indent.into()));
        }
        if !line.trim().is_empty() {
            line.push(' ');
        }
        line.push_str(word);
    }
    if !line.trim().is_empty() {
        lines.push(line);
    }
    lines
}

/// What a sign-in or model sheet says when the gateway gave no answer: what
/// the gateway is for, why it did not answer, what fixes it, and the one
/// thing to press after. A sheet with only "not reachable" on it was a dead
/// end on a fresh install.
fn unreachable_panel(session: &Session<'_>, title: &str, retry: &str) -> Panel {
    let why = session.gateway.why_unreachable();
    let fix = if why.contains("not installed") {
        "The Sterna installer puts it beside sterna; run the installer again, then try again."
    } else {
        "Fix what it said, then try again."
    };
    let why = why.replace('`', "");
    // A sheet row is one line; the reason and the fix are laid into as
    // many as they need rather than clipped mid-word at the edge.
    let mut rows = Vec::new();
    for paragraph in [
        "Sterna signs in and lists models through its inference gateway, and the gateway did not answer.".to_string(),
        String::new(),
        format!("Why: {why}"),
        format!("Fix: {fix}"),
        String::new(),
    ] {
        let lines = wrap_line(&paragraph, 70);
        if lines.is_empty() {
            rows.push(tui::PanelRow::info(String::new()));
        }
        rows.extend(lines.into_iter().map(tui::PanelRow::info));
    }
    // Asked again, the same sheet is redrawn with the time it was last
    // asked, so a try that failed again is seen to have tried.
    rows.push(tui::PanelRow::info(
        match crate::workbench::voice::local_hhmm() {
            Some(time) => format!("still not answering · {time}"),
            None => "still not answering".to_string(),
        },
    ));
    rows.push(tui::PanelRow::command("Try again", retry));
    let mut panel = Panel::rows(title, rows);
    panel.selected = panel.rows.len() - 1;
    panel
}

/// What each tier of this session runs on right now.
///
/// Helpers report `None` when they are off *for any reason* -- no model, or
/// `enabled = false` -- because from the panel's side those are one state:
/// no helper will run.
fn tier_models(session: &Session<'_>) -> TierModels {
    let config = session.config();
    TierModels {
        parent: session.model.borrow().clone(),
        helper: config
            .helpers
            .enabled
            .then(|| config.helpers.model.clone())
            .flatten(),
        subagent: match config.agents.mode {
            AgentsMode::Auto => None,
            AgentsMode::Off => Some("off".to_string()),
            AgentsMode::Pinned => config.agents.model.clone(),
            AgentsMode::Roster => Some("favourites".into()),
        },
    }
}

/// What the status line says of the helper and subagent tiers: helpers run
/// only when switched on **and** given a model, so `enabled` alone reads off.
pub(super) fn tier_status(config: &crate::config::SternaConfig) -> (bool, String) {
    let helpers_on = config.helpers.enabled && config.helpers.model.is_some();
    let subagents = match config.agents.mode {
        AgentsMode::Off => "off",
        AgentsMode::Auto => "inherits",
        AgentsMode::Pinned => "pinned",
        AgentsMode::Roster => "favourites",
    };
    (helpers_on, subagents.to_string())
}

/// Tells the screen the tiers changed, so the status line stops showing
/// what the session started with.
pub(super) fn publish_tiers(session: &Session<'_>) {
    if let Some(ui) = session.ui {
        let (helpers_on, subagents) = tier_status(&session.config());
        ui.tiers(helpers_on, &subagents);
    }
}

/// The settings store this session writes through: the project, and the
/// person's own settings folder when there is one.
pub(super) fn store(session: &Session<'_>) -> Result<crate::settings::Store, String> {
    crate::settings::Store::with_global(&session.project.root, session.settings_global.clone())
}

/// Where a choice made outside Settings is saved: globally (decision 6), or
/// the project when this machine has no user settings folder.
pub(super) fn home_scope(session: &Session<'_>) -> crate::settings::Scope {
    if session.settings_global.is_some() {
        crate::settings::Scope::Global
    } else {
        crate::settings::Scope::Local
    }
}

/// Saves a choice made outside Settings: globally (decision 6), and in the
/// project too for a key the project already sets, because there the
/// project's value wins and a global save alone would change nothing here.
/// A project that sets none of the keys is left untouched. A session running
/// a named profile saves into that profile, which lives in the project.
pub(super) fn save_home(
    session: &Session<'_>,
    edits: &[(String, Option<String>)],
    profile: Option<&str>,
) -> Result<crate::settings::Loaded, String> {
    use crate::settings::Scope;
    let store = store(session)?;
    if profile.is_some() {
        let snapshot = store.read(Scope::Local)?;
        return store.save_profile(Scope::Local, &snapshot, edits, profile);
    }
    let scope = home_scope(session);
    let snapshot = store.read(scope)?;
    let mut loaded = store.save(scope, &snapshot, edits)?;
    if scope == Scope::Global {
        let project = store.read(Scope::Local)?;
        let shadowed: Vec<_> = edits
            .iter()
            .filter(|(key, _)| crate::settings_session::value(&project.values, key).is_some())
            .cloned()
            .collect();
        if !shadowed.is_empty() {
            loaded = store.save(Scope::Local, &project, &shadowed)?;
        }
    }
    Ok(loaded)
}

/// One route for a model choice -- `/model`, the picker and setup alike:
/// the id is settled against what is served, saved, and put in force, the
/// session's own model and the screen's with it.
pub(super) fn use_model(session: &Session<'_>, tier: Tier, id: &str) -> Result<String, String> {
    if tier != Tier::Parent && matches!(id, "off" | "auto" | "inherit") {
        return assign_model(session, tier, id);
    }
    // Validate and persist before changing the live request model. A
    // rejected control word or malformed id therefore leaves both the file
    // and the running session unchanged.
    let accounts = super::startup::served_accounts(session.gateway);
    let model = super::startup::settle_model(id.to_string(), &accounts);
    if !crate::models::chat_capable(&model) {
        return Err(format!(
            "{model} does not answer a conversation; /models lists the ones that do"
        ));
    }
    // With a catalogue to hand, a model nobody serves is refused rather than
    // saved and left to fail on the next request.
    let served = super::startup::served_models(&accounts);
    if !served.is_empty() && !served.contains(&model) {
        return Err(format!(
            "no connected account serves {model}; /models lists what they do"
        ));
    }
    if tier != Tier::Parent {
        return assign_model(session, tier, &model);
    }
    assign_model(session, tier, &model)?;
    *session.model.borrow_mut() = model.clone();
    // The effort a person chose survives a model change: `xhigh` and `max`
    // have a wire form on every model, so taking the choice away would be
    // taking away a level that works.
    if let Some(ui) = session.ui {
        ui.model(&model);
    }
    Ok(format!("model changed to {model}"))
}

/// Assigns a model to one tier, and persists the two that outlive the session.
///
/// All three are saved globally ([`home_scope`]), under the active named
/// profile when selected. The effective configuration stays live and travels
/// to delegated agents as a snapshot.
///
/// SAFETY OF THE EDIT: the text is proved to load with [`SternaConfig::parse`]
/// **before** it replaces the file, so a rejected model name -- a path, a
/// glob, a registered tool's name -- fails with the config's own sentence and
/// leaves the file as it was. There is one validator, not two.
pub(super) fn assign_model(
    session: &Session<'_>,
    tier: Tier,
    value: &str,
) -> Result<String, String> {
    if tier == Tier::Subagents && matches!(value, "auto" | "inherit") {
        return Err("Implicit inheritance is disabled. Pick a concrete subagent model or configure favorite slots.".into());
    }
    let (section, key, key_removed) = match tier {
        Tier::Parent => ("model", "parent", false),
        Tier::Helpers => {
            if matches!(value, "auto" | "inherit") {
                return Err(format!(
                    "helper model must be `off` or a concrete model id, not `{value}`"
                ));
            }
            ("helpers", "model", value == "off")
        }
        Tier::Subagents => ("agents", "model", value == "off"),
    };
    let mut edits = vec![(
        format!("{section}.{key}"),
        if key_removed {
            None
        } else {
            Some(value.into())
        },
    )];
    if tier == Tier::Helpers {
        edits.push(("helpers.enabled".into(), Some((!key_removed).to_string())));
    }
    if tier == Tier::Subagents {
        let mode = match value {
            "off" => "off",
            _ => "pinned",
        };
        edits.push(("agents.mode".into(), Some(mode.into())));
    }
    let loaded = save_home(session, &edits, session.selected_profile.as_deref())?;
    // A live model choice must not activate unrelated preferences saved for restart.
    let mut live = session.config.borrow_mut();
    match tier {
        Tier::Parent => live.model.parent = loaded.config.model.parent,
        Tier::Helpers => {
            live.helpers.model = loaded.config.helpers.model;
            live.helpers.enabled = loaded.config.helpers.enabled;
        }
        Tier::Subagents => {
            live.agents.model = loaded.config.agents.model;
            live.agents.mode = loaded.config.agents.mode;
        }
    }
    drop(live);
    publish_tiers(session);
    Ok(match (tier, value) {
        (Tier::Helpers, "off") => "helpers off; no helper will run".to_string(),
        (Tier::Subagents, "off") => "subagents off; no subagent will run".to_string(),
        (tier, _) => format!("{} model set to {value}", tier.singular()),
    })
}

/// Connects a subscription account without leaving the session.
///
/// The gateway owns the credential from end to end. What crosses this boundary
/// is the authorization URL, a countdown and an outcome — never a token —
/// which is exactly what lets the flow be rendered here instead of handing the
/// terminal to a child process.
///
/// With no account named it lists the ones that could be connected, so
/// `/login` is discoverable on its own and not only from the model picker.
pub(super) fn login(session: &Session<'_>, argument: Option<&str>) {
    // `/login <account> device` asks for a device code instead of a link.
    let mut words = argument.unwrap_or_default().split_whitespace();
    let account = words.next();
    let rest: Vec<&str> = words.collect();
    let device_code = rest.contains(&"device");
    let accepted = rest.contains(&"anyway");
    let catalogue = session
        .gateway
        .run(&["entitlements", "--json"], None)
        .and_then(|bytes| serde_json::from_slice::<Catalogue>(&bytes).ok());

    let Some(catalogue) = catalogue else {
        show(session, unreachable_panel(session, "Sign in", "/login"));
        return;
    };

    let Some(account) = account else {
        show(session, sign_in_panel(&catalogue, &api_keys(session)));
        return;
    };
    // The wizard's second steps: which subscription, which provider's key.
    if account == "subscription" {
        show(session, subscription_panel(&catalogue));
        return;
    }
    if account == "key" {
        show(session, key_panel(&api_keys(session)));
        return;
    }
    // The three doors a person names by what they have, not by an account
    // table: a subscription is connected whether or not the gateway had it
    // declared -- the gateway declares it on the way in.
    if let Some(subscription) = subscription_named(account) {
        if let Some(warning) = warning_before(subscription, accepted) {
            show(session, warning_panel(subscription, warning));
            return;
        }
        let provider = subscription.provider;
        let declared = catalogue
            .accounts
            .iter()
            .find(|entry| entry.connect_with.as_deref() == Some(provider))
            .map(|entry| entry.account.clone());
        let again = format!("/login {}", argument.unwrap_or_default().trim());
        sign_in::start(session, provider, declared.as_deref(), device_code, again);
        return;
    }
    if account == "custom" {
        custom_endpoint(session);
        return;
    }

    let Some(entry) = catalogue
        .accounts
        .iter()
        .find(|entry| entry.account == account)
    else {
        show(
            session,
            Panel::text(
                "Connect an account",
                format!("`{account}` is not a configured account."),
            ),
        );
        return;
    };
    let Some(provider) = entry.connect_with.clone() else {
        show(
            session,
            Panel::text(
                "Connect an account",
                format!("`{account}` is not connected with a login flow."),
            ),
        );
        return;
    };

    let again = format!("/login {}", argument.unwrap_or_default().trim());
    sign_in::start(session, &provider, Some(account), device_code, again);
}

/// A subscription the broker can sign in to: how the wizard names it, the
/// plans it covers, the login flow, the words that name it in `/login`,
/// and -- where the provider's terms make it a risk -- what the person is
/// told before signing in. The warnings are the providers' own terms as
/// read 2026-09-25 (docs/subscriptions.md has the sources).
pub(super) struct Subscription {
    label: &'static str,
    plans: &'static str,
    provider: &'static str,
    words: &'static [&'static str],
    warning: Option<&'static str>,
}

pub(super) const SUBSCRIPTIONS: &[Subscription] = &[
    Subscription {
        label: "ChatGPT",
        plans: "Plus, Pro",
        provider: "openai",
        words: &["chatgpt", "openai", "codex"],
        warning: None,
    },
    Subscription {
        label: "Grok",
        plans: "SuperGrok, X Premium+",
        provider: "xai",
        words: &["grok", "xai"],
        warning: None,
    },
    Subscription {
        label: "Kimi",
        plans: "Kimi Code membership",
        provider: "kimi",
        words: &["kimi", "moonshot"],
        warning: Some(
            "Kimi allows its Kimi Code membership in third-party tools for personal use only, and its own guides use an API key rather than this sign-in.",
        ),
    },
    Subscription {
        label: "Claude",
        plans: "Pro, Max",
        provider: "anthropic",
        words: &["claude", "anthropic"],
        warning: Some(
            "Anthropic forbids using a Claude Pro or Max subscription outside its own apps and has blocked and suspended accounts for it. Sign in only if you accept that risk to your Claude account; an Anthropic API key is the allowed route.",
        ),
    },
    Subscription {
        label: "Gemini (Antigravity)",
        plans: "Google AI Pro, Ultra",
        provider: "google",
        words: &["gemini", "google", "antigravity"],
        warning: Some(
            "Google's terms forbid using Antigravity through third-party tools and it has suspended accounts for it, with a permanent ban on a second strike. A Google AI Studio key is the allowed route.",
        ),
    },
    Subscription {
        label: "Devin",
        plans: "Devin, Windsurf",
        provider: "devin",
        words: &["devin", "windsurf"],
        warning: Some(
            "Cognition's terms do not say whether third-party tools may use a Devin or Windsurf subscription, so the account could be restricted without warning.",
        ),
    },
    Subscription {
        label: "Muse Code",
        plans: "Meta",
        provider: "meta",
        words: &["muse", "meta"],
        warning: Some(
            "Meta has not said whether third-party tools may use a Muse Code plan, and on a data-sharing tier Meta may train on your code.",
        ),
    },
];

/// The subscription a person's own word names.
fn subscription_named(word: &str) -> Option<&'static Subscription> {
    SUBSCRIPTIONS
        .iter()
        .find(|subscription| subscription.words.contains(&word))
}

/// The login flow a person's own word for a subscription names.
#[cfg(test)]
fn subscription_provider(word: &str) -> Option<&'static str> {
    subscription_named(word).map(|subscription| subscription.provider)
}

/// The warning a sign-in waits on: the subscription's own, until the person
/// has chosen to sign in anyway.
fn warning_before(subscription: &Subscription, accepted: bool) -> Option<&'static str> {
    subscription.warning.filter(|_| !accepted)
}

/// What a person is told before signing in to a subscription whose terms
/// make it a risk, with the choice to go on or back.
fn warning_panel(subscription: &Subscription, warning: &str) -> Panel {
    let word = subscription.words[0];
    // The whole warning, laid into lines, and the sheet opens on Back: going
    // on is the choice that has to be made on purpose.
    let mut rows: Vec<_> = wrap_line(&format!("⚠ {warning}"), 70)
        .into_iter()
        .map(tui::PanelRow::info)
        .collect();
    rows.push(tui::PanelRow::command(
        "Sign in anyway",
        format!("/login {word} anyway"),
    ));
    rows.push(tui::PanelRow::command("Back", "/login subscription"));
    let mut panel = Panel::rows(format!("Sign in › {}", subscription.label), rows);
    panel.selected = panel.rows.len() - 1;
    panel
}

/// `/login custom`: an endpoint's URL, what it speaks, and its key, asked one
/// at a time. The gateway declares the endpoint and files the key; nothing
/// here writes a configuration file or asks the person to.
fn custom_endpoint(session: &Session<'_>) {
    use crate::tui::form::{Field, Form, Kind, base_url, key_shape};
    const SPEAKS: [&str; 2] = ["OpenAI-compatible", "Anthropic Messages"];
    let mut form = Form::new(
        "Sign in › Your own endpoint",
        "Any OpenAI- or Anthropic-compatible URL: a local model, a company proxy, a new provider. Tab moves between the three.",
        vec![
            Field::new("Base URL", Kind::Text, "for example https://api.example.com/v1").checked(base_url),
            Field::new(
                "It speaks",
                Kind::Choice(SPEAKS.iter().map(|word| word.to_string()).collect()),
                "most endpoints are OpenAI-compatible",
            ),
            Field::new("API key", Kind::Secret, "paste it here, or leave it empty for a local model")
                .optional()
                .checked(key_shape),
        ],
    )
    .submit("connect");
    loop {
        let Some(answers) = fill(session, form.clone()) else {
            return;
        };
        let [url, speaks, key] = <[String; 3]>::try_from(answers).unwrap_or_default();
        let protocol = if speaks == SPEAKS[1] {
            "anthropic-messages"
        } else {
            "openai-chat"
        };
        let name = endpoint_name(&url);
        let added = session.gateway.run(
            &[
                "providers",
                "add",
                &name,
                "--base-url",
                &url,
                "--protocol",
                protocol,
                "--json",
            ],
            None,
        );
        if added.is_none() {
            form = form.with_error(0, format!("the gateway could not add {url}"));
            continue;
        }
        if key.is_empty() {
            show(
                session,
                Panel::text(
                    "Your own endpoint",
                    format!(
                        "Connected {name} ({url}) with no key. Pick one of its models with /models."
                    ),
                ),
            );
            return;
        }
        if crate::gateway::store_credential(session.gateway, &name, &key).is_none() {
            form = form.with_error(2, "the gateway did not store the key; try again");
            continue;
        }
        show(
            session,
            Panel::text(
                "Your own endpoint",
                format!("Connected {name} ({url}). Pick one of its models with /models."),
            ),
        );
        return;
    }
}

/// Asks through a form sheet; headless, one line per field that takes text,
/// and a choice takes its first word.
fn fill(session: &Session<'_>, form: crate::tui::Form) -> Option<Vec<String>> {
    if let Some(ui) = session.ui {
        return ui.form(form);
    }
    if let Some(warning) = &form.warning {
        eprintln!("sterna: {warning}");
    }
    let mut answers = Vec::new();
    for field in &form.fields {
        answers.push(match &field.kind {
            crate::tui::form::Kind::Choice(words) => words.first().cloned().unwrap_or_default(),
            _ => ui::read_line().ok().flatten()?.trim().to_string(),
        });
    }
    Some(answers)
}

/// What a person is told before giving a key to a provider whose route has
/// a catch, keyed by the gateway's provider name.
const KEY_WARNINGS: &[(&str, &str)] = &[(
    "gemini-openai",
    "This is Google's OpenAI-compatible endpoint, which Google still calls beta. Requests go to it as they are, past Sterna's own Gemini translation, so thinking and caching can behave differently than with `gemini`. Use a Google AI Studio key: Google's terms do not allow a Gemini or Antigravity subscription in third-party tools, and it has suspended accounts for it.",
)];

/// The form that takes one provider's API key.
fn key_form(provider: &str) -> crate::tui::Form {
    use crate::tui::form::{Field, Form, Kind, key_shape};
    let form = Form::new(
        format!("Sign in › API key · {provider}"),
        format!(
            "Paste your {provider} key below. The gateway keeps it in its credential file. Sterna never shows it, logs it or puts it in the conversation."
        ),
        vec![
            Field::new(
                "API key",
                Kind::Secret,
                "paste here: Cmd+V or Ctrl+Shift+V · Ctrl-R shows it while you type",
            )
            .checked(key_shape),
        ],
    );
    match KEY_WARNINGS.iter().find(|(name, _)| *name == provider) {
        Some((_, warning)) => form.warn(*warning),
        None => form,
    }
}

/// A short name for an endpoint, from its host: `api.together.xyz` is
/// `together`, `localhost:8000` is `localhost`; anything with no usable
/// label is `custom`.
fn endpoint_name(url: &str) -> String {
    let host = url
        .split("://")
        .nth(1)
        .unwrap_or(url)
        .split(['/', ':', '?'])
        .next()
        .unwrap_or("");
    let label = host
        .split('.')
        .find(|label| {
            !matches!(*label, "api" | "www" | "") && !label.chars().all(|c| c.is_ascii_digit())
        })
        .unwrap_or("");
    let name: String = label
        .to_ascii_lowercase()
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-')
        .collect();
    if name.is_empty() {
        "custom".to_string()
    } else {
        name
    }
}

/// The gateway's credential table. **Every session asks, hosted or not**: the
/// credentials belong to the gateway whoever started it, so a session
/// Glasshouse handed a gateway to sees and enters the same keys as one that
/// started its own. Empty is what a gateway that could not be asked leaves.
pub(super) fn api_keys(session: &Session<'_>) -> Vec<crate::gateway::CredentialRow> {
    crate::gateway::credentials(session.gateway).unwrap_or_default()
}

/// Says a startup once, and only for the one state a person must act on:
/// sterna started the gateway, the gateway names providers, and not one of
/// them has a credential -- so every turn would come back 503 until a key is
/// entered.
pub(super) fn announce_missing_credential(session: &Session<'_>, started_the_gateway: bool) {
    if session.config().agents.mode == AgentsMode::Auto {
        session_println!(
            "Subagents: legacy auto inheritance is disabled. Select a concrete model or configure favorite slots; Main will not be reused implicitly."
        );
    }
    const NOTICE: &str = "No provider credential is stored yet. Use /login to enter an API key \
                          or connect an account.";
    if !started_the_gateway || !crate::gateway::nothing_resolves(session.gateway) {
        return;
    }
    // A connected subscription is a credential the gateway holds too --
    // through its own login flow rather than a key, so `credentials list`
    // does not see it, and a session serving one must not be told nothing
    // is stored. Measured 2026-09-11 on three serving subscriptions.
    let connected = session
        .gateway
        .run(&["entitlements", "--json"], None)
        .and_then(|bytes| serde_json::from_slice::<Catalogue>(&bytes).ok())
        .is_some_and(|catalogue| {
            catalogue
                .accounts
                .iter()
                .any(|entry| entry.authenticated == Some(true))
        });
    if connected {
        return;
    }
    if session.ui.is_some() {
        session_println!("{NOTICE}");
    } else {
        // Not stdout: a session driven by a script has a caller reading its
        // answers there, and a notice is not an answer.
        eprintln!("{NOTICE}");
    }
}

/// One row per provider that declares a key, after the accounts: where the
/// key resolves from now, and `/key <provider>` to enter one.
fn key_rows(keys: &[crate::gateway::CredentialRow]) -> Vec<tui::PanelRow> {
    let mut keys: Vec<_> = keys.iter().collect();
    keys.sort_by_key(|row| row.provider.to_lowercase());
    keys.into_iter()
        .map(|row| {
            let state = match (row.source.as_deref(), row.native_store.as_deref()) {
                (Some("file"), _) => "stored in the gateway's credential file".to_string(),
                (Some("native"), _) => "stored in the native store".to_string(),
                (Some("environment"), _) => "stored in the environment".to_string(),
                (Some(source), _) => format!("stored in {source}"),
                (None, Some("refused")) => {
                    "a Keychain item exists that this build may not read; enter it again"
                        .to_string()
                }
                (None, _) => "not set".to_string(),
            };
            tui::PanelRow::command(
                format!("{} · API key · {state}", row.provider),
                format!("/key {}", row.provider),
            )
        })
        .collect()
}

/// The subscriptions signed in, by the wizard's names for them.
pub(super) fn connected_subscriptions(catalogue: &Catalogue) -> Vec<String> {
    catalogue
        .accounts
        .iter()
        .filter(|entry| entry.connect_with.is_some() && entry.authenticated == Some(true))
        .map(|entry| {
            SUBSCRIPTIONS
                .iter()
                .find(|subscription| entry.connect_with.as_deref() == Some(subscription.provider))
                .map_or(entry.account.clone(), |subscription| {
                    subscription.label.to_string()
                })
        })
        .collect()
}

/// The gateway's account catalogue, or `None` when it cannot be asked.
pub(super) fn catalogue(session: &Session<'_>) -> Option<Catalogue> {
    session
        .gateway
        .run(&["entitlements", "--json"], None)
        .and_then(|bytes| serde_json::from_slice::<Catalogue>(&bytes).ok())
}

/// Saves settings edits to one scope's file (never a named profile: the
/// wizard's settings are not runtime overlays), proved to load before the
/// file is replaced.
pub(super) fn save_settings(
    session: &Session<'_>,
    scope: crate::settings::Scope,
    edits: &[(String, Option<String>)],
) -> Result<crate::settings::Loaded, String> {
    let store = store(session)?;
    let snapshot = store.read(scope)?;
    store.save_profile(scope, &snapshot, edits, None)
}

/// `/login`, the wizard's first step: three ways in, in the order a person
/// reaches for them. Each row opens the next step; nothing asks for a file.
fn sign_in_panel(catalogue: &Catalogue, keys: &[crate::gateway::CredentialRow]) -> Panel {
    let row = |text: String, command: &str| tui::PanelRow::opens(text, command);
    let connected = connected_subscriptions(catalogue);
    let stored = keys.iter().filter(|key| key.source.is_some()).count();
    let subscription = if connected.is_empty() {
        "Sign in with a subscription · ChatGPT, Grok, Claude, Gemini …".to_string()
    } else {
        format!(
            "Sign in with a subscription · {} connected",
            connected.join(", ")
        )
    };
    let key = match stored {
        0 => "Sign in with an API key · Anthropic, OpenAI, OpenRouter …".to_string(),
        1 => "Sign in with an API key · 1 key stored".to_string(),
        n => format!("Sign in with an API key · {n} keys stored"),
    };
    Panel::rows(
        "Sign in",
        vec![
            row(subscription, "/login subscription"),
            row(key, "/login key"),
            row(
                "Custom endpoint · any OpenAI- or Anthropic-compatible URL".into(),
                "/login custom",
            ),
        ],
    )
}

/// The subscription step: ChatGPT and Claude always, whether or not an
/// account is declared -- signing in declares it -- with each declared
/// account listed by name, and any other account a login flow connects.
fn subscription_panel(catalogue: &Catalogue) -> Panel {
    let row = |text: String, command: String| tui::PanelRow::opens(text, command);
    let state = |entry: &Account| {
        if entry.authenticated == Some(true) {
            "connected"
        } else {
            "sign in"
        }
    };
    let mut rows = Vec::new();
    for subscription in SUBSCRIPTIONS {
        let declared: Vec<&Account> = catalogue
            .accounts
            .iter()
            .filter(|entry| entry.connect_with.as_deref() == Some(subscription.provider))
            .collect();
        let risk = match subscription.warning {
            Some(_) => " · ⚠ read first",
            None => "",
        };
        if declared.is_empty() {
            rows.push(row(
                format!("{} · {}{risk}", subscription.label, subscription.plans),
                format!("/login {}", subscription.words[0]),
            ));
        }
        // By the name it was declared with, and with the offer's warning;
        // the gateway's scope word is not the person's.
        for entry in declared {
            rows.push(row(
                format!(
                    "{} · {}{risk} · {}",
                    subscription.label,
                    entry.account,
                    state(entry)
                ),
                format!("/login {}", entry.account),
            ));
        }
    }
    let known: Vec<&str> = SUBSCRIPTIONS.iter().map(|s| s.provider).collect();
    for entry in catalogue.accounts.iter().filter(|entry| {
        entry
            .connect_with
            .as_deref()
            .is_some_and(|provider| !known.contains(&provider))
    }) {
        rows.push(row(
            format!("{} · {} · {}", entry.account, entry.scope, state(entry)),
            format!("/login {}", entry.account),
        ));
    }
    Panel::rows("Sign in › Subscription", rows)
}

/// The API-key step: every provider the gateway knows a key for.
fn key_panel(keys: &[crate::gateway::CredentialRow]) -> Panel {
    let mut rows = key_rows(keys);
    if rows.is_empty() {
        rows.push(tui::PanelRow::info(
            "The gateway names no provider that takes a key.",
        ));
    }
    Panel::rows("Sign in › API key", rows)
}

/// `/key <provider>`: takes an API key without echoing it and hands it to the
/// gateway to store.
///
/// **The value is read, passed to one child's stdin, and dropped.** It is
/// never put in the editor, the conversation, the rollout, a panel or a log
/// -- the panel this ends with names the *variable*, never the key.
pub(super) fn key(session: &Session<'_>, provider: Option<&str>) {
    // Bare, it is the list of providers that take a key.
    let Some(provider) = provider.filter(|value| !value.is_empty()) else {
        show(session, key_panel(&api_keys(session)));
        return;
    };
    let mut form = key_form(provider);
    loop {
        // A form closed without a key changed nothing, and says nothing.
        let Some(value) = fill(session, form).and_then(|answers| answers.into_iter().next()) else {
            return;
        };
        if value.is_empty() {
            session_println!("No key was pasted, so nothing was stored for {provider}.");
            return;
        }
        if let Some(variable) = crate::gateway::store_credential(session.gateway, provider, &value)
        {
            session_println!("Stored the {variable} for {provider} in the gateway.");
            return;
        }
        // Refused: the same form again, saying so, rather than a sentence
        // that sends the person to a shell.
        form = key_form(provider).with_error(
            0,
            "the gateway did not store this key; check it is the whole key and paste it again",
        );
    }
}

fn model_panel(catalogue: Option<Catalogue>, tiers: TierModels) -> Panel {
    let title = "Models".to_string();
    match catalogue {
        Some(catalogue) if catalogue.version == 1 => Panel::models(
            title,
            catalogue
                .accounts
                .into_iter()
                .map(|account| {
                    // An account that could be connected and is not is the row
                    // a person most wants to act on, so it says so and offers
                    // the flow rather than sitting empty.
                    let connect = match (account.authenticated, &account.connect_with) {
                        (Some(false), Some(provider)) => Some(provider.clone()),
                        _ => None,
                    };
                    let unavailable_reason = match (&connect, account.unavailable_reason) {
                        // Enter on a model does nothing; the account's own
                        // sign-in row above it is the way in.
                        (Some(_), _) => Some("Not connected · sign in above".into()),
                        (None, existing) => existing,
                    };
                    // A subscription is a member of its provider's pool,
                    // listed with its plan and last usage reading.
                    let pooled = account
                        .connect_with
                        .is_some()
                        .then(|| account.pooled.unwrap_or(true));
                    let note = super::usage::latest_summary(&account.account);
                    tui::ModelGroup {
                        provider: account.provider.unwrap_or_else(|| "native harness".into()),
                        account: account.account,
                        scope: account.scope,
                        models: account.models,
                        selectable: if account.authenticated == Some(false) {
                            Some(false)
                        } else {
                            account.selectable
                        },
                        unavailable_reason,
                        connect,
                        pooled,
                        note,
                    }
                })
                .collect(),
            tiers,
        ),
        _ => Panel::text(
            title,
            "Catalogue unavailable: no gateway answered. Use /model <id>.",
        ),
    }
}

pub(super) fn command(
    name: &str,
    argument: Option<&str>,
    session: &Session<'_>,
    transcript: &mut Transcript,
) -> bool {
    match name {
        // Bare, it is the picker's Subagents section: the one place the
        // favourites and the pinned model are chosen.
        "subagents" if argument.is_none_or(|a| a.trim().is_empty()) => {
            models_at(session, Tier::Subagents);
        }
        "subagents" => match subagents::assign(session, argument.unwrap_or_default()) {
            Ok(message) => session_println!("{message}"),
            Err(error) => session_println!("ERROR: {error}"),
        },
        "handlers" => {
            if argument.is_some() {
                session_println!("No active task; handlers are released when its task ends.");
            }
            show(session, tui::handlers_panel(&transcript.notebook.handlers));
        }
        "effort" => {
            if let Some(value) = argument {
                if let Some(effort) = wire::Effort::parse(value) {
                    session.effort.set(effort);
                    if let Some(ui) = session.ui {
                        ui.effort(effort);
                    }
                    session_println!("{}", effort.now());
                } else {
                    session_println!("Use /effort default|low|medium|high|xhigh|max");
                }
            } else {
                const LADDER: [&str; 6] = ["default", "low", "medium", "high", "xhigh", "max"];
                let rows = LADDER
                    .iter()
                    .map(|value| PanelRow::command(value.to_string(), format!("/effort {value}")))
                    .collect();
                show(
                    session,
                    Panel {
                        title: format!("Effort · current {}", session.effort.get().name()),
                        rows,
                        // It opens on the effort in force, like every sheet.
                        selected: LADDER
                            .iter()
                            .position(|value| *value == session.effort.get().name())
                            .unwrap_or(0),
                        ..Panel::default()
                    },
                );
            }
        }
        // `/sandbox <level>` typed where no screen caught it first: the
        // screen's own route saves and confirms Full access, this one moves
        // the running session only.
        "sandbox" => {
            let word = argument.map(str::trim).unwrap_or_default();
            match (
                crate::permissions::Level::parse(word),
                session.level.as_ref(),
            ) {
                (Some(level), Some(live)) => {
                    live.set(level);
                    session_println!("{}", level.now());
                }
                (Some(_), None) => session_println!("this session has no live sandbox level"),
                (None, live) => session_println!(
                    "Sandbox: {} · use /sandbox ask|sandboxed|full",
                    live.map_or("unknown", |live| live.level().label())
                ),
            }
        }
        "handles" => {
            let table = transcript
                .notebook
                .cells
                .last()
                .and_then(|cell| cell.table.as_deref())
                .unwrap_or("No handles recorded yet.");
            show(session, Panel::text("Last handle preview", table));
        }
        "budget" => {
            let used = transcript
                .notebook
                .tokens
                .as_ref()
                .map(|tokens| tokens.used)
                .unwrap_or(0);
            show(
                session,
                Panel::text(
                    "Task spend",
                    format!(
                        "Last task: {used} cumulative tokens\nToken spend is telemetry and has no cap.\nCell limit: {}\nConfigure runtime limits in .sterna/config.toml for the next session.",
                        cell_limit(&session.config().limits)
                    ),
                ),
            );
        }
        "context" => {
            let c = &transcript.conversation;
            let estimated = estimate_request_tokens(c, &session.model.borrow());
            let bytes: usize = c.messages.iter().map(|m| message_text(m).len()).sum();
            let measured = measured_context(transcript.notebook.context);
            show(
                session,
                Panel::text(
                    "Context",
                    format!(
                        "{} messages · {} cells\n{}\nSystem: {} bytes\nMessages: {} bytes\nNext request: ~{} tokens (estimate)\nTask spend: cumulative telemetry, no cap\nContext is retained in the rollout; no model call was made.",
                        c.messages.len(),
                        transcript.notebook.cells.len(),
                        measured,
                        c.system.len(),
                        bytes,
                        estimated
                    ),
                ),
            );
        }
        "config" => {
            let args = argument
                .unwrap_or("")
                .split_whitespace()
                .map(str::to_owned)
                .collect::<Vec<_>>();
            match crate::settings_commands::execute(&session.project.root, &args) {
                Ok(text) => show(
                    session,
                    Panel::text(
                        "Sterna configuration",
                        format!(
                            "{text}\nRuntime changes require a new session; live sandbox unchanged."
                        ),
                    ),
                ),
                Err(error) => session_println!("ERROR: {error}"),
            }
        }
        "settings" => show(
            session,
            Panel::text(
                "Settings",
                "Open /settings in an interactive terminal. CLI: sterna config --help",
            ),
        ),
        // This folder's sessions, as a sheet: choosing one ends this session
        // and starts that one here.
        "resume" => show(
            session,
            super::resume::panel(&session.project.root, session.interrupt.session.as_str()),
        ),
        "status" => {
            // The same facts the chips show, named the way they name them.
            let config = session.config();
            let (helpers_on, subagents) = tier_status(&config);
            let model = session.model.borrow().clone();
            let effort = session.effort.get();
            let sent = effort.sent_for(&model);
            let mut lines = vec![
                format!("Model: {model}"),
                match session
                    .level
                    .as_ref()
                    .map(crate::permissions::LiveLevel::level)
                {
                    Some(level) => format!("Sandbox: {} · {}", level.label(), level.sentence()),
                    None => "Sandbox: unknown".to_string(),
                },
                if sent == effort {
                    format!("Effort: {}", effort.name())
                } else {
                    format!("Effort: {} (sent as {})", effort.name(), sent.name())
                },
                format!(
                    "Helpers: {}",
                    match (helpers_on, config.helpers.model.as_deref()) {
                        (true, Some(helper)) => format!("on · {helper}"),
                        _ if config.helpers.enabled => "on, but no helper model chosen".into(),
                        _ => "off".into(),
                    }
                ),
                format!("Subagents: {subagents}"),
                format!("Project: {}", session.project.root.display()),
                format!(
                    "Rules: {} · {} · commands reach {}",
                    counted(session.profile.rule_count(), "path rule"),
                    counted(session.profile.pre_approved().len(), "pre-approved command"),
                    if session.profile.grants_network() {
                        "any host"
                    } else {
                        "allowed hosts only"
                    }
                ),
                format!("Web: {}", config.web.describe()),
                format!(
                    "Cell limit: {} · {} seconds each · response {} bytes",
                    cell_limit(&config.limits),
                    config.limits.cell_wall_clock_s,
                    config.limits.response_bytes
                ),
                format!(
                    "Helper effort: find {} · reduce {} · check {}",
                    config.helpers.effort.find.name(),
                    config.helpers.effort.reduce.name(),
                    config.helpers.effort.check.name()
                ),
            ];
            lines.push("Change any of these in Settings (F2).".into());
            drop(config);
            show(session, Panel::text("Session", lines.join("\n")));
        }
        "rollback" => rollback(session, argument, &mut transcript.notebook),
        "permissions" => match permissions(session, argument) {
            Ok(text) => show(session, Panel::text("Permissions", text)),
            Err(error) => session_println!("ERROR: {error}"),
        },
        "models" | "entitlements" => {
            models(session);
        }
        // Each of these can finish a setup step, so the opening chip that
        // leads back into setup is recomputed after it.
        "login" => {
            login(session, argument);
            super::setup::offer(session);
        }
        "wizard" | "setup" => {
            super::setup::command(session, argument);
            super::setup::offer(session);
        }
        "pool" => pool(session, argument),
        "usage" => {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(0));
            show(
                session,
                super::usage::panel(super::usage::read(session).as_ref(), now),
            );
        }
        "key" => {
            key(session, argument);
            super::setup::offer(session);
        }
        _ => return false,
    }
    true
}

/// `/pool <account> include|exclude`: take a subscription account into its
/// provider's pool or out of it (the gateway reads it on its next request),
/// then show the picker again so the row's mark moves under the cursor.
fn pool(session: &Session<'_>, argument: Option<&str>) {
    let mut words = argument.unwrap_or_default().split_whitespace();
    let (Some(account), Some(action @ ("include" | "exclude"))) = (words.next(), words.next())
    else {
        session_println!("Use /pool <account> include|exclude");
        return;
    };
    let flag = if action == "include" {
        "--include"
    } else {
        "--exclude"
    };
    match session.gateway.run(
        &["subscriptions", "pool", "--entitlement", account, flag],
        None,
    ) {
        Some(_) => {
            session_println!(
                "{account}: {} its pool",
                if action == "include" {
                    "back in"
                } else {
                    "taken out of"
                }
            );
            models(session);
        }
        None => session_println!("ERROR: the gateway could not change {account}'s pool"),
    }
}

/// "1 path rule" or "3 path rules".
fn counted(count: usize, thing: &str) -> String {
    format!("{count} {thing}{}", if count == 1 { "" } else { "s" })
}

/// The rollback preview: what the rollback would change, a danger row that
/// confirms it, and Cancel -- where the sheet opens, and what Esc does.
fn rollback_panel(preview: &str, cell: Option<usize>) -> Panel {
    let cell = cell.map_or_else(
        || "the latest cell".to_string(),
        |cell| format!("cell {cell:03}"),
    );
    let mut panel = Panel::text(
        format!("Rollback › {cell}"),
        format!(
            "These files go back to how they were before {cell}. This cannot be undone.\n{preview}"
        ),
    );
    // Cancel comes first, where the sheet opens.
    panel.selected = panel.rows.len();
    panel
        .rows
        .push(PanelRow::command("Cancel", "/rollback cancel"));
    panel.rows.push(PanelRow::danger(
        format!("Roll back {cell}"),
        crate::workbench::Action::Command("/rollback confirm".into()),
    ));
    panel.back = Some(crate::workbench::Action::Command("/rollback cancel".into()));
    panel
}

fn rollback(session: &Session<'_>, argument: Option<&str>, notebook: &mut tui::Notebook) {
    let count = session.rollbacks.borrow().len();
    let Some(last) = session
        .rollbacks
        .borrow()
        .last()
        .map(|checkpoint| checkpoint.before.rollback_plan(&checkpoint.after))
    else {
        session_println!("/rollback: no file-changing cell is available in this session");
        return;
    };
    let plan = match last {
        Ok(plan) => plan,
        Err(error) => {
            session_println!("/rollback unavailable: {error}");
            return;
        }
    };

    match argument.filter(|value| !value.is_empty()) {
        None => {
            session.rollback_pending.set(Some(count));
            let cell = session.rollbacks.borrow().last().map(|last| last.cell);
            show(session, rollback_panel(&plan.preview(), cell));
        }
        Some("cancel") => {
            session.rollback_pending.set(None);
            session_println!("Rollback cancelled; no files were changed.");
        }
        Some("confirm") => {
            if let Err(message) =
                rollback_confirmation(session.ui.is_some(), session.rollback_pending.get(), count)
            {
                session_println!("{message}");
                return;
            }
            match plan.apply(session.profile) {
                Ok(()) => {
                    // The cell says its change was undone, and the notice is
                    // one line; the preview of what came back follows it.
                    let cell = session.rollbacks.borrow_mut().pop().map(|c| c.cell);
                    if let Some(view) =
                        cell.and_then(|cell| notebook.cells.get_mut(cell.checked_sub(1)?))
                    {
                        view.rolled_back = true;
                    }
                    session.rollback_pending.set(None);
                    session_println!(
                        "Rolled back cell {} · its changes are undone\n{}",
                        cell.map_or_else(|| "?".to_string(), |cell| format!("{cell:03}")),
                        plan.preview()
                    );
                }
                Err(error) => {
                    session.rollback_pending.set(None);
                    session_println!("Rollback refused: {error}");
                }
            }
        }
        Some(_) => session_println!("Use /rollback, /rollback confirm, or /rollback cancel"),
    }
}

fn rollback_confirmation(
    interactive: bool,
    previewed: Option<usize>,
    current: usize,
) -> Result<(), &'static str> {
    if !interactive {
        return Err(
            "/rollback confirm is refused outside an interactive TUI; no files were changed",
        );
    }
    if previewed != Some(current) {
        return Err("/rollback confirm requires a current preview; run /rollback first");
    }
    Ok(())
}

fn permissions(session: &Session<'_>, argument: Option<&str>) -> Result<String, String> {
    let saved = crate::settings_session::permissions(&session.project.root, argument)?;
    Ok(format!(
        "Effective current session (immutable): {} path rules · {} pre-approved commands · {} MCP patterns\n{saved}",
        session.profile.rule_count(),
        session.profile.pre_approved().len(),
        session.profile.mcp_tool_count()
    ))
}

/// `/context`'s line for the request just sent: its size, against the
/// window when the window is known, and who counted it.
fn measured_context(context: Option<crate::tui::ContextTokens>) -> String {
    let Some(context) = context else {
        return "Current request context: no request yet".into();
    };
    match context.cap {
        Some(cap) => format!(
            "Current request context: {}/{} tokens ({}%), {}",
            context.used,
            cap,
            context.used.min(cap).saturating_mul(100) / cap.max(1),
            context.counted.by()
        ),
        None => format!(
            "Current request context: {} tokens, {}; the model's window size is not known",
            context.used,
            context.counted.by()
        ),
    }
}

#[cfg(test)]
pub(super) mod tests {

    #[test]
    fn the_context_line_says_who_counted_and_what_is_not_known() {
        use crate::tui::{ContextTokens, Counted};
        let line = super::measured_context(Some(ContextTokens {
            used: 12,
            cap: None,
            cap_source: crate::models::WindowSource::Unknown,
            counted: Counted::Gateway,
        }));
        assert_eq!(
            line,
            "Current request context: 12 tokens, counted by the provider; the model's window size is not known"
        );
        let line = super::measured_context(Some(ContextTokens {
            used: 50,
            cap: Some(200),
            cap_source: crate::models::WindowSource::Unknown,
            counted: Counted::Estimated,
        }));
        assert_eq!(
            line,
            "Current request context: 50/200 tokens (25%), estimated"
        );
    }

    #[test]
    fn only_the_gemini_relay_key_is_warned_about_and_the_warning_names_the_terms() {
        let warned = super::key_form("gemini-openai").warning.unwrap();
        assert!(warned.contains("subscription"), "{warned}");
        assert!(
            warned.contains("past Sterna's own Gemini translation"),
            "{warned}"
        );
        assert!(super::key_form("gemini").warning.is_none());
        assert!(super::key_form("anthropic").warning.is_none());
    }

    #[test]
    fn the_status_line_reports_the_tiers_as_they_are_set_now() {
        let mut config = crate::config::SternaConfig::default();
        config.helpers.enabled = true;
        config.agents.mode = AgentsMode::Pinned;
        // Enabled without a model runs nothing, so the status must not say on.
        assert_eq!(tier_status(&config), (false, "pinned".to_string()));
        config.helpers.model = Some("gpt-5.4-mini".to_string());
        config.agents.mode = AgentsMode::Roster;
        assert_eq!(tier_status(&config), (true, "favourites".to_string()));
    }

    #[test]
    fn a_fresh_install_offers_both_subscriptions_keys_and_a_custom_endpoint() {
        // What a new machine's gateway answers: no account declared at all.
        let catalogue: Catalogue = serde_json::from_str(r#"{"version":1,"accounts":[]}"#).unwrap();
        let commands = |panel: &Panel| -> Vec<String> {
            panel
                .rows
                .iter()
                .filter_map(|row| row.command_line().map(str::to_string))
                .collect()
        };
        // Step one: three ways in.
        assert_eq!(
            commands(&sign_in_panel(&catalogue, &[])),
            ["/login subscription", "/login key", "/login custom"]
        );
        // Step two: every subscription the broker signs in to, though
        // nothing is declared.
        assert_eq!(
            commands(&subscription_panel(&catalogue)),
            [
                "/login chatgpt",
                "/login grok",
                "/login kimi",
                "/login claude",
                "/login gemini",
                "/login devin",
                "/login muse"
            ]
        );
        assert_eq!(subscription_provider("chatgpt"), Some("openai"));
        assert_eq!(subscription_provider("claude"), Some("anthropic"));
        assert_eq!(subscription_provider("antigravity"), Some("google"));
    }

    /// A model on an account that is not connected says so and points at
    /// the account's own sign-in row; Enter on the model does nothing, so
    /// it is not offered.
    #[test]
    fn a_model_on_an_unconnected_account_points_at_its_sign_in() {
        let catalogue: Catalogue = serde_json::from_str(
            r#"{"version":1,"accounts":[{"account":"openai-plus","provider":"openai","models":["m"],"scope":"subscription","selectable":true,"unavailable_reason":null,"authenticated":false,"connect_with":"openai"}]}"#,
        )
        .unwrap();
        let panel = model_panel(Some(catalogue), TierModels::default());
        let reasons = format!("{panel:?}");
        assert!(
            reasons.contains("Not connected · sign in above"),
            "{reasons}"
        );
        assert!(!reasons.contains("press enter"), "{reasons}");
    }

    /// A declared subscription is listed by the name it was declared with
    /// and keeps the offer's warning; the gateway's scope word is not shown.
    #[test]
    fn a_declared_subscription_keeps_its_words_and_its_warning() {
        let catalogue: Catalogue = serde_json::from_str(
            r#"{"version":1,"accounts":[{"account":"me@example.com","provider":"anthropic","models":[],"scope":"user","selectable":true,"unavailable_reason":null,"authenticated":true,"connect_with":"anthropic"}]}"#,
        )
        .unwrap();
        let panel = subscription_panel(&catalogue);
        let claude = panel
            .rows
            .iter()
            .find(|row| row.text.starts_with("Claude"))
            .unwrap();
        assert_eq!(
            claude.text,
            "Claude · me@example.com · ⚠ read first · connected"
        );
        assert_eq!(claude.command_line(), Some("/login me@example.com"));
    }

    #[test]
    fn a_subscription_whose_terms_forbid_it_is_warned_about_before_signing_in() {
        let claude = subscription_named("claude").unwrap();
        let panel = warning_panel(claude, claude.warning.unwrap());
        assert!(
            panel.rows[0].text.contains("Anthropic forbids"),
            "{:?}",
            panel.rows[0].text
        );
        let commands: Vec<_> = panel
            .rows
            .iter()
            .filter_map(tui::PanelRow::command_line)
            .collect();
        assert_eq!(commands, ["/login claude anyway", "/login subscription"]);
        assert!(warning_before(claude, false).is_some(), "asked first");
        assert!(warning_before(claude, true).is_none(), "`anyway` signs in");
        // The ones whose providers allow third-party tools go straight to
        // the sign-in.
        for word in ["chatgpt", "grok"] {
            assert!(
                subscription_named(word).unwrap().warning.is_none(),
                "{word}"
            );
        }
        for word in ["claude", "gemini", "kimi", "devin", "muse"] {
            assert!(
                subscription_named(word).unwrap().warning.is_some(),
                "{word}"
            );
        }
    }

    /// A reflexive Enter on the rollback preview cancels: the sheet opens on
    /// Cancel, confirming is a danger row, and Esc cancels too.
    #[test]
    fn the_rollback_preview_starts_on_cancel() {
        let panel = rollback_panel("a.txt: restored", Some(1));
        assert_eq!(panel.title, "Rollback › cell 001");
        let cancel = panel.rows.iter().position(|row| row.text == "Cancel");
        let roll = panel
            .rows
            .iter()
            .position(|row| row.text == "Roll back cell 001");
        assert!(cancel < roll, "Cancel comes first");
        assert_eq!(
            panel.rows[panel.selected].command_line(),
            Some("/rollback cancel")
        );
        let confirm = panel
            .rows
            .iter()
            .find(|row| row.text == "Roll back cell 001")
            .unwrap();
        assert_eq!(confirm.kind, crate::workbench::ItemKind::Danger);
        assert_eq!(
            panel.back,
            Some(crate::workbench::Action::Command("/rollback cancel".into()))
        );
    }

    #[test]
    fn a_custom_endpoint_is_named_after_its_host() {
        assert_eq!(endpoint_name("https://api.together.xyz/v1"), "together");
        assert_eq!(endpoint_name("http://localhost:8000/v1"), "localhost");
        assert_eq!(endpoint_name("http://127.0.0.1:4000"), "custom");
        assert_eq!(endpoint_name("https://openrouter.ai/api/v1"), "openrouter");
    }

    use super::*;

    #[test]
    fn model_catalogue_groups_by_provider_then_account_and_preserves_model_ids() {
        let catalogue = serde_json::from_value(serde_json::json!({
            "version": 1,
            "accounts": [
                {"account":"a-account", "provider":"z-provider", "scope":"declared", "models":["shared/id"]},
                {"account":"z-account", "provider":"a-provider", "scope":"declared", "models":["shared/id", "B/model", "shared/id", "bad id", ""]},
                {"account":"b-account", "provider":"a-provider", "scope":"declared", "models":["shared/id"]}
            ]
        })).unwrap();
        let panel = model_panel(Some(catalogue), TierModels::default());
        let (groups, _) = panel
            .catalogue()
            .expect("a model panel carries its catalogue");
        let routes: Vec<_> = groups
            .iter()
            .map(|g| format!("{} · {} · {}", g.provider, g.account, g.scope))
            .collect();
        assert_eq!(
            routes,
            [
                "a-provider · b-account · declared",
                "a-provider · z-account · declared",
                "z-provider · a-account · declared"
            ]
        );
        // Whitespace ids and empty ids are dropped; the rest are kept exactly,
        // sorted and without duplicates.
        assert_eq!(groups[1].models, ["B/model", "shared/id"]);
    }

    #[test]
    fn permission_edits_preserve_other_settings_and_reject_invalid_grants() {
        let root = std::env::temp_dir().join(format!("sterna-permissions-{}", std::process::id()));
        fs::create_dir_all(root.join(".sterna")).unwrap();
        let path = root.join(".sterna/config.toml");
        fs::write(
            &path,
            "# keep this comment\n[ui]\ntheme='amber'\n[permissions]\nallow=[]\ndeny=['Bash(rm *)']\n",
        )
        .unwrap();
        let project = ProjectConfig {
            root: root.clone(),
            ..ProjectConfig::default()
        };
        let config = SternaConfig::default();
        let profile = Profile::compile(
            &root,
            crate::settings::Store::new(&root)
                .unwrap()
                .permissions()
                .unwrap()
                .as_deref(),
        );
        let gateway = crate::gateway::Gateway::Command {
            gateway: root.join("absent-gateway"),
        };
        let id = SessionId::new("permission-test");
        let memory = LocalMemory::new(&root);
        let interrupt = Interrupter::new(id.clone());
        let session = Session {
            observe: crate::observe::Observer::none(),
            selected_profile: None,
            pending_images: RefCell::new(Vec::new()),
            approval_gate: None,
            ask_gate: None,
            level: None,
            window: RefCell::new(crate::events::window::Window::new(Default::default())),
            roster: Vec::new(),
            ui: None,
            model: RefCell::new("test".into()),
            context_window: None,
            interface: Cell::new(crate::abi::Interface::default()),
            manifest: crate::manifest::Manifest::default(),
            mode: Cell::new(crate::sandbox::modes::RequestMode::Work),
            effort: Cell::new(wire::Effort::Default),
            routing: Default::default(),
            project: &project,
            config: &RefCell::new(config),
            interrupt: &interrupt,
            profile: &profile,
            gateway: &gateway,
            id: &id,
            memory: &memory,
            rollbacks: RefCell::new(Vec::new()),
            rollback_pending: Cell::new(None),
            plan: RefCell::new(None),
            requests: std::cell::Cell::new(0),
            settings_global: Some(root.join("user-settings")),
        };
        permissions(&session, Some("allow Read(**)")).unwrap();
        let saved = fs::read_to_string(&path).unwrap();
        let parsed: toml::Value = toml::from_str(&saved).unwrap();
        assert_eq!(parsed["ui"]["theme"].as_str(), Some("amber"));
        assert!(saved.contains("# keep this comment"));
        assert_eq!(
            parsed["permissions"]["deny"],
            toml::Value::Array(vec![toml::Value::String("Bash(rm *)".into())])
        );
        assert!(permissions(&session, Some("allow NotAGrant(foo)")).is_err());
        assert_eq!(fs::read_to_string(&path).unwrap(), saved);
        permissions(&session, Some("remove Read(**)")).unwrap();
        let parsed: toml::Value = toml::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(parsed["permissions"]["allow"], toml::Value::Array(vec![]));
        fs::remove_dir_all(root).unwrap();
    }

    /// Builds a session rooted at `root` and runs `body` against it.
    pub(in crate::session) fn with_session(
        root: &std::path::Path,
        body: impl FnOnce(&Session<'_>),
    ) {
        with_selected_session(root, None, body)
    }

    fn with_selected_session(
        root: &std::path::Path,
        selected: Option<&str>,
        body: impl FnOnce(&Session<'_>),
    ) {
        let project = ProjectConfig {
            root: root.to_path_buf(),
            ..ProjectConfig::default()
        };
        let config =
            RefCell::new(SternaConfig::load_profile(root, selected).expect("the fixture parses"));
        let profile = Profile::compile(root, None);
        let gateway = crate::gateway::Gateway::Command {
            gateway: root.join("absent-gateway"),
        };
        let id = SessionId::new("tier-test");
        let memory = LocalMemory::new(root);
        let interrupt = Interrupter::new(id.clone());
        let session = Session {
            observe: crate::observe::Observer::none(),
            selected_profile: selected.map(str::to_string),
            pending_images: RefCell::new(Vec::new()),
            approval_gate: None,
            ask_gate: None,
            level: None,
            window: RefCell::new(crate::events::window::Window::new(Default::default())),
            roster: Vec::new(),
            ui: None,
            model: RefCell::new("opus-5".into()),
            context_window: None,
            interface: Cell::new(crate::abi::Interface::default()),
            manifest: crate::manifest::Manifest::default(),
            mode: Cell::new(crate::sandbox::modes::RequestMode::Work),
            effort: Cell::new(wire::Effort::Default),
            routing: Default::default(),
            project: &project,
            config: &config,
            interrupt: &interrupt,
            profile: &profile,
            gateway: &gateway,
            id: &id,
            memory: &memory,
            rollbacks: RefCell::new(Vec::new()),
            rollback_pending: Cell::new(None),
            plan: RefCell::new(None),
            requests: std::cell::Cell::new(0),
            settings_global: Some(root.join("user-settings")),
        };
        body(&session);
    }

    /// A subscription's warning is read whole before signing in, and the
    /// sheet opens on Back: going on is chosen on purpose.
    #[test]
    fn a_subscription_warning_is_whole_and_starts_on_back() {
        let (claude, warning) = SUBSCRIPTIONS
            .iter()
            .find_map(|subscription| subscription.warning.map(|w| (subscription, w)))
            .expect("one subscription carries a warning");
        let panel = warning_panel(claude, warning);
        let said: Vec<String> = panel
            .rows
            .iter()
            .filter(|row| row.action.is_none())
            .map(|row| row.text.clone())
            .collect();
        assert_eq!(
            said.join(" ").split_whitespace().collect::<Vec<_>>(),
            format!("⚠ {warning}")
                .split_whitespace()
                .collect::<Vec<_>>()
        );
        assert_eq!(panel.rows[panel.selected].text, "Back");
    }

    /// The providers that take a key are listed by name.
    #[test]
    fn the_key_list_is_in_name_order() {
        let row = |provider: &str| crate::gateway::CredentialRow {
            provider: provider.into(),
            variable: None,
            source: None,
            native_store: None,
        };
        let panel = key_panel(&[row("openai"), row("Anthropic"), row("groq")]);
        let names: Vec<_> = panel
            .rows
            .iter()
            .map(|row| row.text.split(' ').next().unwrap_or_default().to_string())
            .collect();
        assert_eq!(names, ["Anthropic", "groq", "openai"]);
    }

    /// A model that cannot hold a conversation is refused on every route
    /// and changes nothing.
    #[test]
    fn a_model_that_cannot_converse_is_refused() {
        let root = std::env::temp_dir().join(format!("sterna-chat-only-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        with_session(&root, |session| {
            let refused = use_model(session, Tier::Parent, "gpt-image-1").unwrap_err();
            assert!(
                refused.contains("does not answer a conversation"),
                "{refused}"
            );
            assert!(use_model(session, Tier::Helpers, "whisper-1").is_err());
            assert_eq!(*session.model.borrow(), "opus-5");
            assert!(!root.join("user-settings").join("config.toml").exists());
        });
        fs::remove_dir_all(&root).unwrap();
    }

    /// A tier assignment takes effect now and survives the session, and an
    /// unrelated setting in the same file is not collateral damage.
    #[test]
    fn assigning_a_tier_is_live_persisted_and_reversible() {
        let root = std::env::temp_dir().join(format!("sterna-tier-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join(".sterna")).unwrap();
        let file = root.join(".sterna").join("config.toml");
        fs::write(
            &file,
            "[limits]\ncells = 42\n\n[helpers]\nenabled = false\n",
        )
        .unwrap();

        // What the next session loads: the project and the person's own
        // settings, where a model choice is saved.
        let load = || {
            crate::settings::Store::with_global(&root, Some(root.join("user-settings")))
                .unwrap()
                .load(None)
                .unwrap()
                .config
        };
        with_session(&root, |session| {
            assert_eq!(tier_models(session).parent, "opus-5");
            assert_eq!(tier_models(session).helper, None, "helpers ship off");

            assign_model(session, Tier::Helpers, "gpt-5.6-luna").unwrap();
            // Live, with no restart -- the next cell's runtime is built from
            // this.
            assert_eq!(tier_models(session).helper.as_deref(), Some("gpt-5.6-luna"));
            // Persisted, because `agent.rs` loads the settings itself when a
            // delegated goal starts.
            let saved = load();
            assert_eq!(saved.helpers.model.as_deref(), Some("gpt-5.6-luna"));
            // Choosing a model IS the opt-in, so an earlier `enabled = false`
            // does not silently swallow it: the project set it, so it is
            // changed there, where it wins.
            assert!(saved.helpers.enabled);
            assert!(
                fs::read_to_string(&file)
                    .unwrap()
                    .contains("enabled = true")
            );
            // And an unrelated setting survived the edit.
            assert_eq!(saved.limits.cells, Some(42));

            assign_model(session, Tier::Subagents, "claude-sonnet-5").unwrap();
            let saved = load();
            assert_eq!(saved.agents.mode, AgentsMode::Pinned);
            assert_eq!(saved.agents.model.as_deref(), Some("claude-sonnet-5"));

            assign_model(session, Tier::Subagents, "off").unwrap();
            let saved = load();
            assert_eq!(saved.agents.mode, AgentsMode::Off);
            assert_eq!(saved.agents.model, None);
            assert_eq!(tier_models(session).subagent.as_deref(), Some("off"));

            // Reversible, which is what makes the panel safe to press.
            assign_model(session, Tier::Helpers, "off").unwrap();
            assert_eq!(tier_models(session).helper, None);
            assert_eq!(load().helpers.model, None);
            assert!(assign_model(session, Tier::Subagents, "inherit").is_err());
            assert_eq!(tier_models(session).subagent.as_deref(), Some("off"));
            assert_eq!(load().agents.mode, AgentsMode::Off);

            // A value the config refuses fails with the config's own sentence
            // and leaves the file byte-identical: one validator, not two.
            let before = fs::read_to_string(&file).unwrap();
            assert!(assign_model(session, Tier::Helpers, "../etc/passwd").is_err());
            assert_eq!(fs::read_to_string(&file).unwrap(), before);

            // The parent is remembered too: the tier a person changes most
            // was the only one that used to forget.
            assign_model(session, Tier::Parent, "claude-opus-4-8").unwrap();
            assert_eq!(load().model.parent.as_deref(), Some("claude-opus-4-8"));
            let before = fs::read_to_string(&file).unwrap();
            assert!(assign_model(session, Tier::Parent, "auto").is_err());
            assert_eq!(fs::read_to_string(&file).unwrap(), before);
        });
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn tier_changes_preserve_selected_profile_and_leave_base_settings_unchanged() {
        let root = std::env::temp_dir().join(format!("sterna-profile-tier-{}", std::process::id()));
        fs::create_dir_all(root.join(".sterna")).unwrap();
        let file = root.join(".sterna/config.toml");
        let text = "[model]\nparent='base-parent'\n[limits]\ncells=42\n[helpers]\nmodel='base-helper'\nenabled=true\n[agents]\nmodel='base-agent'\n[profiles.review.limits]\ncells=17\n[profiles.review.web]\nenabled=true\n[profiles.review.helpers]\nmodel='review-helper'\n";
        fs::write(&file, text).unwrap();
        // Loaded, not parsed: the claim below is that the tier changes left
        // the base settings alone, and `load` is what the assertion re-reads.
        // Parsing only the project text made the pair differ by whatever the
        // developer's own global configuration contributes, so the test read
        // the machine it ran on.
        let base = SternaConfig::load(&root).unwrap();
        with_selected_session(&root, Some("review"), |session| {
            assign_model(session, Tier::Helpers, "changed-helper").unwrap();
            assert_eq!(session.config().limits.cells, Some(17));
            assert!(session.config().web.enabled);
            assert_eq!(
                session.config().helpers.model.as_deref(),
                Some("changed-helper")
            );
            assign_model(session, Tier::Helpers, "off").unwrap();
            assert!(!session.config().helpers.enabled);
            assign_model(session, Tier::Subagents, "off").unwrap();
            assert_eq!(session.config().agents.mode, crate::config::AgentsMode::Off);
            assert!(assign_model(session, Tier::Subagents, "auto").is_err());
            assert_eq!(session.config().agents.mode, crate::config::AgentsMode::Off);
            assign_model(session, Tier::Parent, "review-parent").unwrap();
            assert_eq!(
                session.config().model.parent.as_deref(),
                Some("review-parent")
            );
            assert_eq!(SternaConfig::load(&root).unwrap(), base);
            let reloaded = SternaConfig::load_profile(&root, Some("review")).unwrap();
            let live = session.config().clone();
            assert_eq!(reloaded, live);
        });
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn live_assignment_does_not_activate_unrelated_restart_preferences() {
        let root = std::env::temp_dir().join(format!("sterna-pending-live-{}", std::process::id()));
        fs::create_dir_all(root.join(".sterna")).unwrap();
        let file = root.join(".sterna/config.toml");
        fs::write(&file, "[limits]\ncells=17\n").unwrap();
        with_session(&root, |session| {
            fs::write(&file, "[limits]\ncells=42\n[helpers]\npreflight=true\n").unwrap();
            assign_model(session, Tier::Helpers, "explicit-helper").unwrap();
            assert_eq!(session.config().limits.cells, Some(17));
            assert!(!session.config().helpers.preflight);
            assert_eq!(
                session.config().helpers.model.as_deref(),
                Some("explicit-helper")
            );
            subagents::assign(session, "quick explicit-agent low").unwrap();
            assert_eq!(session.config().limits.cells, Some(17));
            assert!(!session.config().helpers.preflight);
            assert_eq!(
                session.config().agents.slots["quick"].model,
                "explicit-agent"
            );
            assert_eq!(SternaConfig::load(&root).unwrap().limits.cells, Some(42));
        });
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn rollback_confirmation_is_interactive_and_bound_to_the_previewed_checkpoint() {
        assert!(rollback_confirmation(false, Some(1), 1).is_err());
        assert!(rollback_confirmation(true, None, 1).is_err());
        assert!(rollback_confirmation(true, Some(1), 2).is_err());
        assert_eq!(rollback_confirmation(true, Some(2), 2), Ok(()));
    }
}
