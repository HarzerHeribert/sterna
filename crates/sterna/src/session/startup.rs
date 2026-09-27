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

/// How far this session's children reach: how wide the admission profile is
/// compiled, and whether Sterna confines what it spawns.
///
/// Two independent facts rather than one flag, because they answer different
/// questions and two of the three spellings that set them
/// (`--yolo`, `--dangerously-bypass-os-sandbox`) name exactly one each.
pub(super) struct Reach {
    /// The project root and every command line are admitted.
    pub yolo: bool,
    /// Sterna installs no OS confinement of its own on the children it spawns.
    pub unconfined: bool,
}

/// The reach `args` asks for, refused rather than approximated.
///
/// `--full-access` is the one name for all three halves — this pair and the
/// `full` rung [`ladder`] resolves — and the older spellings keep meaning
/// exactly the half they always meant.
///
/// **Every platform Sterna has an unconfined applier for is accepted.** This
/// was Linux and Windows only, on the reasoning that an externally isolated
/// container or CI runner is the boundary there and nothing outside the
/// seatbelt is one on macOS. That reasoning described a benchmark runner and
/// not a person: on a development machine the machine itself is the boundary
/// its owner has already chosen, and refusing them the mode only moved the
/// work to a tool with no admission checks at all (user ruling 2026-09-18).
/// What does not move is what `Profile::check` refuses: §4's never-grantable
/// set is enforced in this process, before any child is spawned, and is
/// identical on every rung and every platform.
pub(super) fn reach(args: &SessionArgs, values: &toml::Value) -> Result<Reach, String> {
    let stored = full_access_setting(values);
    let reach = Reach {
        yolo: args.yolo || args.full_access || stored,
        unconfined: args.dangerously_bypass_os_sandbox || args.full_access || stored,
    };
    if reach.unconfined && !reach.yolo {
        return Err("--dangerously-bypass-os-sandbox requires --yolo so both the admission profile and OS confinement choice are explicit (or pass --full-access, which is both)".into());
    }
    if reach.unconfined
        && !cfg!(any(
            target_os = "linux",
            target_os = "windows",
            target_os = "macos"
        ))
    {
        return Err("--full-access is supported on macOS, Linux and Windows; this platform has no unconfined applier, and a bypass that spawned anyway would be the one unconfined path Sterna exists not to have".into());
    }
    Ok(reach)
}

/// `[permissions] full_access`, the stored spelling of `--full-access`.
///
/// **Global scope only, enforced where the file is read** (`settings.rs`
/// drops it from a project or legacy document and says so). This function is
/// therefore free to read the assembled value: by the time it gets here, a
/// project document's copy is already gone.
///
/// It is the whole flag rather than its unconfined third, because splitting
/// it would rebuild in a settings file exactly the three-separate-choices
/// trap `--full-access` was created to remove: on 2026-09-19 a person set
/// the rung and the grant, left the confinement, and a session spent twelve
/// cells hunting a linker it was never going to be allowed to run.
fn full_access_setting(values: &toml::Value) -> bool {
    crate::settings_session::value(values, "permissions.full_access")
        .and_then(toml::Value::as_bool)
        .unwrap_or(false)
}

/// Which rung a session starts on, and whether it has anybody to ask.
///
/// The flag, then the saved rung, then the default — one place decides it.
/// `--ask-approval` is the alias for the strictest rung and is folded in
/// here; naming both with different rungs is ambiguous and is refused rather
/// than silently resolved.
///
/// A rung that confirms edits or every command line cannot function with
/// nobody at the keyboard: its first call would wait ten minutes and then be
/// denied, so it is refused at startup instead. `auto` asks rarely enough to
/// degrade instead, which is what keeps every scripted run working exactly
/// as it does today.
pub(super) fn ladder(
    args: &SessionArgs,
    values: &toml::Value,
) -> Result<crate::permissions::Ladder, String> {
    use crate::permissions::Rung;
    if args.ask_approval && args.permissions.is_some_and(|rung| rung != Rung::Manual) {
        return Err(
            "--ask-approval is the alias for --permissions manual; naming both with different rungs is ambiguous"
                .into(),
        );
    }
    // `--full-access` names this rung as one of its three halves, so naming
    // a different one beside it is the same ambiguity and gets the same
    // refusal rather than a silent winner.
    if args.full_access && args.permissions.is_some_and(|rung| rung != Rung::Full) {
        return Err(
            "--full-access starts on the `full` rung; naming --permissions with a different rung beside it is ambiguous"
                .into(),
        );
    }
    let rung = args
        .permissions
        .or_else(|| args.full_access.then_some(Rung::Full))
        .or_else(|| args.ask_approval.then_some(Rung::Manual))
        .or_else(|| {
            crate::settings_session::value(values, "permissions.mode")
                .and_then(toml::Value::as_str)
                .and_then(Rung::parse)
        })
        // Below a named rung on purpose. `full_access` is a stored default,
        // and a stored default loses to any explicit `permissions.mode` --
        // including a project file's, which is the layering the rest of this
        // configuration already has. Only the flag-beside-flag case above is
        // ambiguous enough to refuse.
        .or_else(|| full_access_setting(values).then_some(Rung::Full))
        .unwrap_or_default();
    let attended = args.task.is_none() && io::stdin().is_terminal() && io::stdout().is_terminal();
    if rung.needs_a_person() && !attended {
        return Err(format!(
            "--permissions {} requires an interactive terminal session; scripted calls cannot approve themselves",
            rung.name()
        ));
    }
    let ladder = crate::permissions::Ladder::new(rung);
    Ok(if attended {
        ladder
    } else {
        ladder.unattended()
    })
}

/// The gate the ladder's asking rungs need, and nothing for the one that
/// never asks.
///
/// `full` asks nothing, so it needs no gate at all and pays nothing for one.
/// Every other rung installs the gate and decides per call whether it
/// reaches a person (`permissions::judge`). The approval hint (F4,
/// decision-model.md): the gate is session-scoped and outlives any one task,
/// so its model and mode are attached once, here, exactly like
/// `[decisions]` is read once at session start.
pub(super) fn approval_gate(
    ladder: &crate::permissions::Ladder,
    config: &crate::config::SternaConfig,
    interactive: Option<&ui::LiveUi>,
) -> Option<crate::approval::Gate> {
    // A terminal session gets its gate whatever rung it starts on: the rung
    // moves mid-session, and a session started on Never asks and stepped
    // down to Every call must ask from the next call on.
    let decisions = config.decisions.clone();
    interactive
        .map(|ui| ui.approval_gate(ladder.clone()))
        .map(|gate| {
            gate.with_decisions(
                decisions.model,
                decisions.mode,
                decisions.command_runs_above,
            )
        })
        .map(|gate| gate.with_read_only(config.modes.explore.commands.clone()))
}

/// The rung this session starts on, and what it means in one clause.
///
/// **A session with nobody at the keyboard says so.** An asking rung there
/// runs what it would have confirmed — the profile is still the boundary —
/// and a person reading a log afterwards must be able to tell that from a
/// session where they answered.
pub(super) fn permissions_line(ladder: &crate::permissions::Ladder) -> String {
    let rung = ladder.rung();
    // The same sentence the Ask sheet shows, so the promise made here is
    // the one the gate keeps.
    let what = rung.sentence();
    let unattended = if ladder.is_unattended() && rung.ever_asks() {
        " — no terminal to ask at, so what would be confirmed runs"
    } else {
        ""
    };
    format!("permissions: {} — {what}{unattended}", rung.name())
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
