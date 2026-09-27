//! What a session starts on, and what a person reads when a request fails.
//!
//! The parent model is the person's choice: `--model`, then `[model] parent`.
//! Without one a terminal session opens the model picker and a script is
//! refused, so nothing is ever spent against a model nobody chose. A name the
//! gateway's catalogue does not list is read as a family word and becomes
//! that family's newest served model, and the chat says so.
use super::*;

/// One account `inference-gateway entitlements --json` lists.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Deserialize)]
pub(crate) struct ServedAccount {
    pub account: String,
    #[serde(default)]
    pub models: Vec<String>,
    #[serde(default)]
    pub selectable: Option<bool>,
    #[serde(default)]
    pub authenticated: Option<bool>,
    #[serde(default)]
    pub connect_with: Option<String>,
    #[serde(default)]
    pub provider: Option<String>,
}

/// The provider whose account answers the decision model's questions.
pub(crate) const DECISIONS_PROVIDER: &str = "typesafe";

/// The decision model a session gets when `[decisions] model` is unset: Jev,
/// when a served account is TypeSafe's (the gateway routes `/v1/systemone`
/// to it). `None` when a model is configured, when the mode is `off`, or when
/// no account could answer -- then decisions stay off, said once at start.
pub(super) fn default_decisions_model(
    decisions: &crate::config::DecisionsConfig,
    accounts: &[ServedAccount],
) -> Option<&'static str> {
    (decisions.model.is_none()
        && decisions.mode != crate::config::DecisionMode::Off
        && accounts.iter().any(|a| {
            a.provider.as_deref() == Some(DECISIONS_PROVIDER) && a.selectable != Some(false)
        }))
    .then_some(crate::decide::DEFAULT_MODEL)
}

#[derive(serde::Deserialize)]
struct Listing {
    #[serde(default)]
    accounts: Vec<ServedAccount>,
}

/// The level this session starts on, and whether it has anybody to ask.
///
/// `--sandbox`, then the saved `sandbox.level`, then `sandboxed`. The saved
/// value is global only: a project document's copy is dropped where the file
/// is read (`settings.rs`), because cloning a repository must never be
/// enough to lower it.
///
/// `ask` confirms the edits and commands every task makes, so with nobody at
/// the keyboard its first call would wait and be refused; it is refused at
/// startup instead. `full` is refused where Sterna has no unconfined applier,
/// because spawning anyway would be the one unconfined path it exists not to
/// have.
pub(super) fn level(
    args: &SessionArgs,
    values: &toml::Value,
) -> Result<crate::permissions::LiveLevel, String> {
    use crate::permissions::Level;
    let level = args
        .sandbox
        .or_else(|| {
            crate::settings_session::value(values, "sandbox.level")
                .and_then(toml::Value::as_str)
                .and_then(Level::parse)
        })
        .unwrap_or_default();
    if level == Level::Full
        && !cfg!(any(
            target_os = "linux",
            target_os = "windows",
            target_os = "macos"
        ))
    {
        return Err("--sandbox full is supported on macOS, Linux and Windows; this platform has no unconfined applier".into());
    }
    let attended = args.task.is_none() && io::stdin().is_terminal() && io::stdout().is_terminal();
    if level.needs_a_person() && !attended {
        return Err(
            "--sandbox ask requires an interactive terminal session; scripted calls cannot approve themselves"
                .into(),
        );
    }
    let live = crate::permissions::LiveLevel::new(level);
    Ok(if attended { live } else { live.unattended() })
}

/// The gate every session gets: it decides per call whether the level sends
/// it to a person, and refuses what it would have asked when nobody is
/// there. The approval hint (F4, decision-model.md) is attached once, here,
/// exactly like `[decisions]` is read once at session start.
pub(super) fn approval_gate(
    level: &crate::permissions::LiveLevel,
    config: &crate::config::SternaConfig,
    profile: &crate::sandbox::profile::Profile,
    interactive: Option<&ui::LiveUi>,
) -> crate::approval::Gate {
    let decisions = config.decisions.clone();
    let gate = match interactive {
        Some(ui) => ui.approval_gate(level.clone()),
        // Nobody reads this channel: every question on an unattended level
        // is refused before it is sent.
        None => crate::approval::Gate::channel(level.clone()).0,
    };
    gate.with_decisions(decisions.model, decisions.mode)
        .with_pre_approved(profile.pre_approved().to_vec())
}

/// The level this session starts on, and what it means in one clause.
///
/// **A session with nobody at the keyboard says so**: what the level would
/// have asked about is refused there, and a person reading a log afterwards
/// must be able to tell that from a session where they answered.
pub(super) fn sandbox_line(level: &crate::permissions::LiveLevel) -> String {
    let now = level.level();
    let unattended = if level.is_unattended() && now != crate::permissions::Level::Full {
        " — nobody is watching, so leaving the sandbox is refused"
    } else {
        ""
    };
    format!("sandbox: {} — {}{unattended}", now.name(), now.sentence())
}

/// The proxy confined commands reach the network through, when this
/// platform can route them to it: macOS always, Linux where a command can
/// have its own network namespace ([`crate::sandbox::linux_ns::available`]).
/// Elsewhere commands have no network at all, and nothing is started.
///
/// The allowed hosts are the ecosystems `sandbox.ecosystems` names (all of
/// them when unset), `sandbox.hosts`, and each `--allow-host`.
pub(super) fn start_proxy(
    args: &SessionArgs,
    values: &toml::Value,
) -> Option<crate::sandbox::proxy::Proxy> {
    if !crate::sandbox::proxy::reachable() {
        return None;
    }
    let list = |key: &str| -> Option<Vec<String>> {
        crate::settings_session::value(values, key)
            .and_then(toml::Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(toml::Value::as_str)
                    .map(str::to_string)
                    .collect()
            })
    };
    let ecosystems = list("sandbox.ecosystems").unwrap_or_else(|| {
        crate::sandbox::proxy::ECOSYSTEMS
            .iter()
            .map(|ecosystem| ecosystem.name.to_string())
            .collect()
    });
    let mut hosts = list("sandbox.hosts").unwrap_or_default();
    hosts.extend(args.allow_hosts.iter().cloned());
    for host in &hosts {
        if !crate::sandbox::proxy::valid_host(host) {
            session_println!("sandbox: `{host}` is not a host name, so it was not allowed");
        }
    }
    let allowed = crate::sandbox::proxy::Allowed::new(&ecosystems, &hosts);
    match crate::sandbox::proxy::Proxy::start(allowed) {
        Ok(proxy) => Some(proxy),
        Err(error) => {
            session_println!(
                "sandbox: the network proxy could not start ({error}); commands have no network"
            );
            None
        }
    }
}

/// The startup line naming who is watching this session.
///
/// **Off is worth one line; on is worth one too.** A session that cannot say
/// who is watching it reads exactly like a session nobody is watching --
/// measured 2026-09-17 (session `tlitep-13fv`), where `supervisor: off (no
/// model)` scrolled past at startup and sixty cells of reading without an
/// edit then ran to the cell cap.
pub(super) fn supervisor_line(
    supervisor: &crate::config::SupervisorConfig,
    decisions: &crate::config::DecisionsConfig,
) -> String {
    if !supervisor.enabled {
        return "supervisor: off (disabled)".to_string();
    }
    let classifier = decisions
        .model
        .as_deref()
        .filter(|_| decisions.mode != crate::config::DecisionMode::Off);
    let every = supervisor.every;
    match (classifier, supervisor.model.as_deref()) {
        (Some(classifier), Some(model)) => format!(
            "supervisor: {classifier} decides, {model} writes the nudge, looking every {every} cell(s)"
        ),
        (Some(classifier), None) => {
            format!("supervisor: {classifier} decides, looking every {every} cell(s)")
        }
        (None, Some(model)) => format!("supervisor: {model}, looking every {every} cell(s)"),
        (None, None) => "supervisor: off (no model)".to_string(),
    }
}

/// The gateway's cached account listing; empty when there is no gateway.
pub(super) fn served_accounts(gateway: &Gateway) -> Vec<ServedAccount> {
    gateway
        .run(&["entitlements", "--json"], None)
        .and_then(|bytes| serde_json::from_slice::<Listing>(&bytes).ok())
        .map(|listing| listing.accounts)
        .unwrap_or_default()
}

/// Every model an account that can serve lists: selectable, and not known
/// to be logged out. Sorted and without duplicates.
pub(crate) fn served_models(accounts: &[ServedAccount]) -> Vec<String> {
    let mut models: Vec<String> = accounts
        .iter()
        .filter(|a| a.selectable != Some(false) && a.authenticated != Some(false))
        .flat_map(|a| a.models.iter().cloned())
        .collect();
    models.sort();
    models.dedup();
    models
}

/// The models a subagent may be sent to, with what the gateway publishes for
/// each -- resolved once at session start, because the accounts a session
/// serves cannot change while it runs and the figures are a subprocess away.
pub(super) fn subagent_roster(
    gateway: &Gateway,
    accounts: &[ServedAccount],
) -> Vec<crate::models::RosterModel> {
    // One ask, two readers: the roster in the system block, and the two real
    // limits -- the context window and the model's own output maximum --
    // which `wire` and the context meter read through
    // `crate::models::limits_for` rather than by holding the session.
    let published = crate::models::published(gateway);
    crate::models::remember(&published);
    crate::models::measure(&served_models(accounts), &published)
}

/// The project's folder name as a person knows it; a relative root such as
/// `.` names nothing.
pub(super) fn project_name(root: &std::path::Path) -> String {
    let root = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    root.file_name()
        .unwrap_or(root.as_os_str())
        .to_string_lossy()
        .into_owned()
}

/// The model the person asked for, if any. `None` only for a terminal
/// session, which opens the picker; a scripted one has nobody to pick.
pub(super) fn requested_model(
    cli: Option<&str>,
    config: &SternaConfig,
    interactive: bool,
) -> Result<Option<String>, String> {
    let Some(model) = cli
        .map(str::to_string)
        .or_else(|| config.model.parent.clone())
    else {
        return if interactive {
            Ok(None)
        } else {
            Err("sterna cannot start: no parent model selected; pass `--model <id>` or set `[model] parent` in .sterna/config.toml (or global Sterna config)".to_string())
        };
    };
    crate::config::validate_parent_model(&model)
        .map_err(|reason| format!("sterna cannot start: {reason}"))?;
    Ok(Some(model))
}

/// `name` as the gateway will serve it. A listed id is kept. Otherwise the
/// family word is resolved, first among accounts that hold a login and then
/// among every account, and a resolution is said in the chat.
pub(super) fn settle_model(name: String, accounts: &[ServedAccount]) -> String {
    let all = served_models(accounts);
    if all.contains(&name) {
        return name;
    }
    let connected: Vec<String> = accounts
        .iter()
        .filter(|a| a.authenticated == Some(true))
        .flat_map(|a| a.models.iter().cloned())
        .collect();
    match resolve_family(&name, &connected).or_else(|| resolve_family(&name, &all)) {
        Some(resolved) => {
            session_println!("model: `{name}` resolved to {resolved}");
            resolved
        }
        None => name,
    }
}

/// The newest model in `served` whose id carries `word` as one of its
/// words (`opus` in `claude-opus-5`), when every such model is one family.
/// `None` for no match, or for a word several families share (`claude`,
/// `gpt`): guessing between families would choose for the person.
pub(crate) fn resolve_family(word: &str, served: &[String]) -> Option<String> {
    let word = word.to_ascii_lowercase();
    let matches: Vec<&String> = served
        .iter()
        .filter(|id| words(id).contains(&word))
        .collect();
    let family = words(matches.first()?);
    if matches.iter().any(|id| words(id) != family) {
        return None;
    }
    // A provider-prefixed id (`anthropic/claude-opus-5`) loses a tie to the
    // account's own spelling of the same version.
    matches
        .into_iter()
        .max_by_key(|id| (version(id), !id.contains('/')))
        .cloned()
}

/// An id's words, lowercased, without its provider prefix or its numbers.
fn words(id: &str) -> Vec<String> {
    segments(id)
        .filter(|part| !part.chars().all(|c| c.is_ascii_digit()))
        .map(str::to_ascii_lowercase)
        .collect()
}

/// An id's version numbers in order; an eight-digit date stamp is not one.
fn version(id: &str) -> Vec<u32> {
    segments(id)
        .filter(|part| part.len() < 8 && part.chars().all(|c| c.is_ascii_digit()))
        .filter_map(|part| part.parse().ok())
        .collect()
}

fn segments(id: &str) -> impl Iterator<Item = &str> {
    id.rsplit('/')
        .next()
        .unwrap_or(id)
        .split(['-', '.', '_'])
        .filter(|part| !part.is_empty())
}

/// A failed request's message, preceded by the one thing a person can do
/// about it when Sterna recognises the failure.
pub(super) fn explain_failure(message: &str, session: &Session<'_>) -> String {
    let model = session.model.borrow().clone();
    match advice(
        message,
        &model,
        || served_accounts(session.gateway),
        session.ui.is_some(),
    ) {
        Some(advice) => format!("{advice}\n{message}"),
        None => message.to_string(),
    }
}

/// What to do about `message`, or `None` when it is not a failure Sterna
/// recognises. `accounts` is read only for a login failure.
pub(crate) fn advice(
    message: &str,
    model: &str,
    accounts: impl FnOnce() -> Vec<ServedAccount>,
    interactive: bool,
) -> Option<String> {
    if message.contains("auth_unavailable") || message.contains("http status: 401") {
        let accounts = accounts();
        let serving = accounts
            .iter()
            .find(|a| a.connect_with.is_some() && a.models.iter().any(|m| m == model));
        return Some(match serving {
            Some(a) if interactive => format!(
                "The `{}` login is no longer valid. Type /login {} to sign in again.",
                a.account, a.account
            ),
            Some(a) => format!(
                "The `{}` login is no longer valid. Sign in again with: inference-gateway subscriptions connect --entitlement {} {}",
                a.account,
                a.account,
                a.connect_with.as_deref().unwrap_or_default()
            ),
            None => {
                "The account serving this model has no valid credential. /login lists the accounts."
                    .to_string()
            }
        });
    }
    // The endpoint did not answer at all: nothing about a cell, the model or
    // a login's validity is known yet.
    if message.contains("request failed: io:") {
        return Some(if interactive {
            "Couldn't reach the model's endpoint · check /login".to_string()
        } else {
            "Couldn't reach the model's endpoint · check the gateway and your login".to_string()
        });
    }
    if message.contains("unknown provider for model") {
        return Some(if interactive {
            format!("`{model}` is not a model the gateway serves. /model lists the ones it does.")
        } else {
            format!(
                "`{model}` is not a model the gateway serves. `inference-gateway entitlements` lists the ones it does."
            )
        });
    }
    None
}

/// Every acceptance test below, and any real pipe, takes this path. Draws
/// through the identical `tui::render` a live terminal uses, into an
/// in-memory buffer exactly as `tui.rs`'s own tests do, then prints each
/// non-blank row as a line of text -- so the conversation column and the
/// sidebar's content (including its honest "not connected" collapse) reach
/// stdout rather than a dropped `TestBackend`.
///
/// **The buffer is sized to the notebook rather than fixed.** A pipe has no
/// scrollback, so a height chosen once would silently drop the newest cell
/// exactly when a task had run long enough to be worth reading; the doubling
/// is the room a wrapped table line takes.
pub(super) fn render_as_lines(transcript: &Transcript, served_by: &ServedBy) {
    if output::active() {
        return;
    }
    let handles = empty_handles();
    let rows = tui::notebook_height(&transcript.conversation, &handles, &transcript.notebook);
    let height = (rows * 2 + 8).clamp(40, 2_000) as u16;
    let backend = TestBackend::new(100, height);
    let mut terminal = Terminal::new(backend).expect("an in-memory backend never fails to init");
    let _ = terminal.draw(|frame| {
        tui::render(
            frame,
            &transcript.conversation,
            served_by,
            &handles,
            &transcript.notebook,
        )
    });
    let buffer = terminal.backend().buffer();
    for y in 0..buffer.area.height {
        let mut line = String::new();
        for x in 0..buffer.area.width {
            if let Some(cell) = buffer.cell((x, y)) {
                line.push_str(cell.symbol());
            }
        }
        let line = line.trim_end();
        if !line.is_empty() {
            println!("{line}");
        }
    }
}
