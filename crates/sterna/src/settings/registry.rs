//! The one typed settings registry -- `docs/configuration.md`
//! §"Advanced `/config` and CLI counterpart": *one* registry defines every
//! supported key for file loading, the panel, the slash command and the CLI,
//! so a second permissive parser cannot grow beside it.
//!
//! Two rules hold this module together. The first is that an unknown key is
//! refused rather than carried: [`validate`] answers `Err` for anything
//! [`specs`] does not list, which is what keeps a typo out of a saved file
//! and out of the effective configuration. The second is that no range,
//! model rule or web constraint is restated here. A runtime key is checked by
//! building the smallest possible document that sets it and handing that to
//! [`crate::config::SternaConfig::parse_profile`] -- the same parser a session
//! start uses -- so the refusal sentence and the check it comes from cannot
//! drift apart.

use crate::config::SternaConfig;

/// What a value looks like once typed. Plain strings never need TOML quoting;
/// everything else is parsed from the word the user typed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// `true` or `false`.
    Bool,
    /// A signed integer; the range belongs to the runtime parser.
    Integer,
    /// A floating-point number; the range belongs to the runtime parser.
    Float,
    /// One of `choices`, and nothing else.
    Choice,
    /// One concrete model id, refused the way `config.toml` refuses one.
    Model,
    /// Free text, validated by whatever owns the key.
    Text,
    /// A list of strings: `a, b, c` or a TOML array literal.
    List,
}

/// One supported key, as every surface reads it.
#[derive(Debug, Clone, Copy)]
pub struct SettingSpec {
    /// The dotted key, exactly as it is spelled in the file.
    pub key: &'static str,
    /// The short name a panel row shows.
    pub label: &'static str,
    /// One sentence: what it does, and what it does not do.
    pub description: &'static str,
    /// How the written word becomes a value.
    pub kind: Kind,
    /// The complete set of valid words for [`Kind::Choice`], empty otherwise.
    pub choices: &'static [&'static str],
    /// Everyday setting (`/settings`' curated panel) rather than advanced.
    pub basic: bool,
    /// Whether a saved change needs a new session before it is in force.
    pub restart: bool,
}

/// Reasoning effort as the parent tier accepts it: `default` means *the
/// provider's own*, which is not the same as removing a saved override.
const EFFORT: &[&str] = &["auto", "low", "medium", "high", "xhigh", "max"];
/// A favourite's effort is a hard policy (`config.rs`): a slot never inherits
/// `auto`, so the curated word is absent here on purpose.
const HARD_EFFORT: &[&str] = &["low", "medium", "high", "xhigh", "max"];
/// `tui::Theme::ALL`, spelled for the registry. `tests/settings_store.rs`
/// asserts the two lists stay identical, so a new theme cannot appear in the
/// picker and be unsavable.
const THEMES: &[&str] = &[
    "neon",
    "amber",
    "ice",
    "mono",
    "violet",
    "cobalt",
    "mint",
    "rose",
    "amazon",
    "sun-conure",
    "hyacinth",
    "scarlet",
    "blue-gold",
    "green-wing",
    "military",
    "cockatoo",
    "arctic-tern",
];
const STATUS_LINES: &[&str] = &["full", "compact", "hidden"];
const SIDEBAR: &[&str] = &["auto", "show", "hide"];
const BACKGROUNDS: &[&str] = &crate::tui::background::Background::NAMES;
const STREAMS: &[&str] = &["actions", "code", "raw"];
const MOTIONS: &[&str] = &["full", "calm", "off"];
const AGENT_MODES: &[&str] = &["auto", "off", "pinned", "roster"];

/// The sandbox levels, spelled once in `permissions::Level::NAMES` so the
/// sheet and the settings row cannot disagree.
const SANDBOX_LEVELS: &[&str] = &crate::permissions::Level::NAMES;
const DECISION_MODES: &[&str] = &["off", "shadow", "on"];
const ASK_JEV: &[&str] = &["off", "weight", "decide"];

/// The top-level tables `SternaConfig` parses. A key under one of these is
/// validated by the runtime parser; everything else is owned here.
pub(crate) const RUNTIME_TABLES: [&str; 6] =
    ["limits", "agents", "model", "web", "decisions", "ask"];

/// Whether a key belongs to a runtime table, and so reaches `SternaConfig`.
pub fn is_runtime(key: &str) -> bool {
    key.split('.')
        .next()
        .is_some_and(|table| RUNTIME_TABLES.contains(&table))
}

/// Keys a project file may not set, whatever it says.
///
/// **A project document travels inside a repository.** Every other key layers
/// global-then-project because the person owns both files; these are the ones
/// where that assumption fails, because cloning a repository would otherwise
/// be enough to change them before the person has read a line of it. The
/// rule is the same boundary Claude Code draws by keeping
/// `--dangerously-skip-permissions` out of a settings file.
///
/// A short list, deliberately: this is a property of a key, not a dimension
/// of every key. `sandbox.level` is the one that matters most -- a project
/// that could set it could turn every question off.
const GLOBAL_ONLY: &[&str] = &[
    "sandbox.level",
    "sandbox.hosts",
    "sandbox.ecosystems",
    "wizard.seen",
];

/// Whether `key` may only be set in the global scope ([`GLOBAL_ONLY`]).
#[must_use]
pub fn is_global_only(key: &str) -> bool {
    GLOBAL_ONLY.contains(&key)
}

/// The session control that puts a saved key into force **in the session that
/// is already running**, or `None` when nothing can.
///
/// **A setting that only takes effect next time is not a setting, it is a
/// note to your future self.** Every command named here already existed and
/// already mutated the live session -- `session/controls.rs::command` has
/// answered `/effort`, `/mode`, `/permissions` and `/model` since long before
/// this function. What was missing was a caller: the settings panel wrote
/// TOML and stopped, so a person who changed their reasoning effort on the
/// panel was told to start a new session, while the same person typing
/// `/effort high` two lines lower changed it instantly. One of those two was
/// wrong, and it was not the slash command.
///
/// A key absent from this match genuinely cannot move mid-session -- a web
/// broker is built once -- and the panel says `next session` on that row rather than in a
/// sentence attached to every row.
///
/// **The model keys are here for agreement, not because the panel
/// reaches them this way.** A model row opens the navigator, and the
/// navigator already hands the loop the same `/model …` line this would; the
/// arms exist so [`applies_now`] and the thing that actually happens cannot
/// give a row two different answers.
#[must_use]
pub fn live_command(key: &str, value: Option<&str>) -> Option<String> {
    let value = value?;
    match key {
        "session.effort" => Some(format!("/effort {value}")),
        "sandbox.level" => Some(format!("/sandbox {value}")),
        "model.parent" => Some(format!("/model {value}")),
        "agents.model" => Some(format!("/model subagent {value}")),
        // `pinned` and `roster` need a model to name, and assigning one is
        // what turns them on; only the two that stand alone travel here.
        "agents.mode" if value == "auto" || value == "off" => {
            Some(format!("/model subagent {value}"))
        }
        _ => None,
    }
}

/// Every key [`live_command`] can answer for, whatever the value chosen.
/// `agents.mode` is on this list for its two standing values. `pinned` and
/// `roster` are reached by naming a model, and naming one emits the command
/// that puts it in force, so the row is honest either way.
const LIVE: &[&str] = &[
    "session.effort",
    "sandbox.level",
    "model.parent",
    "agents.model",
    "agents.mode",
];

/// Whether a saved `key` is in force the moment it is written.
///
/// Presentation keys are applied to the screen directly; everything else
/// needs [`live_command`] to reach the running session. This is what the
/// panel reads to decide whether a row wears a `next session` mark, so it is
/// deliberately a property of the *key* -- a row whose mark appeared and
/// disappeared as the cursor moved along its own values would be worse than
/// no mark at all.
#[must_use]
pub fn applies_now(key: &str) -> bool {
    key.starts_with("ui.") || LIVE.contains(&key)
}

static SPECS: &[SettingSpec] = &[
    // -- the model tiers, the everyday half of `/settings` ----------------
    SettingSpec {
        key: "model.parent",
        label: "Main model",
        description: "Which model answers you. Applies from the next request, even during a turn; a request already in flight keeps the model it started on.",
        kind: Kind::Model,
        choices: &[],
        basic: true,
        restart: false,
    },
    SettingSpec {
        key: "session.effort",
        label: "Reasoning effort",
        description: "How hard the model thinks before answering. Higher is slower and costs more. `auto` lets the model choose: GPT models are asked for low effort (measured faster at equal results), and every other provider keeps its own setting.",
        kind: Kind::Choice,
        choices: EFFORT,
        basic: true,
        restart: false,
    },
    SettingSpec {
        key: "agents.mode",
        label: "Subagent mode",
        description: "Whether work can be handed to a subagent. Off refuses every spawn; pinned sends all of it to one model you name; roster picks from your favourites.",
        kind: Kind::Choice,
        choices: AGENT_MODES,
        basic: true,
        restart: false,
    },
    SettingSpec {
        key: "agents.model",
        label: "Subagent model",
        description: "The model a handed-off goal runs on. Naming one here turns subagent mode to pinned.",
        kind: Kind::Model,
        choices: &[],
        basic: true,
        restart: false,
    },
    SettingSpec {
        key: "agents.slots.quick.model",
        label: "Quick favorite",
        description: "Explicit favorite model; an empty slot never inherits Main. Provider/account routing remains gateway-owned.",
        kind: Kind::Model,
        choices: &[],
        basic: false,
        restart: true,
    },
    SettingSpec {
        key: "agents.slots.quick.effort",
        label: "Quick effort",
        description: "Captured effort for this favorite; a delegated call cannot raise it.",
        kind: Kind::Choice,
        choices: HARD_EFFORT,
        basic: false,
        restart: true,
    },
    SettingSpec {
        key: "agents.slots.balanced.model",
        label: "Balanced favorite",
        description: "Explicit favorite model; an empty slot never inherits Main. Provider/account routing remains gateway-owned.",
        kind: Kind::Model,
        choices: &[],
        basic: false,
        restart: true,
    },
    SettingSpec {
        key: "agents.slots.balanced.effort",
        label: "Balanced effort",
        description: "Captured effort for this favorite; a delegated call cannot raise it.",
        kind: Kind::Choice,
        choices: HARD_EFFORT,
        basic: false,
        restart: true,
    },
    SettingSpec {
        key: "agents.slots.deep.model",
        label: "Deep favorite",
        description: "Explicit favorite model; an empty slot never inherits Main. Provider/account routing remains gateway-owned.",
        kind: Kind::Model,
        choices: &[],
        basic: false,
        restart: true,
    },
    SettingSpec {
        key: "agents.slots.deep.effort",
        label: "Deep effort",
        description: "Captured effort for this favorite; a delegated call cannot raise it.",
        kind: Kind::Choice,
        choices: HARD_EFFORT,
        basic: false,
        restart: true,
    },
    SettingSpec {
        key: "agents.slots.heavy.model",
        label: "Heavy favorite",
        description: "Explicit favorite model; an empty slot never inherits Main. Provider/account routing remains gateway-owned.",
        kind: Kind::Model,
        choices: &[],
        basic: false,
        restart: true,
    },
    SettingSpec {
        key: "agents.slots.heavy.effort",
        label: "Heavy effort",
        description: "Captured effort for this favorite; a delegated call cannot raise it.",
        kind: Kind::Choice,
        choices: HARD_EFFORT,
        basic: false,
        restart: true,
    },
    // -- presentation, curated and Sterna-owned -----------------------------
    SettingSpec {
        key: "sandbox.level",
        label: "Sandbox",
        description: "How much runs without asking. Ask confirms every edit and command; Sandboxed runs everything inside the project and asks only to leave the sandbox; Full access runs without a sandbox and asks nothing. Global only.",
        kind: Kind::Choice,
        choices: SANDBOX_LEVELS,
        basic: true,
        restart: false,
    },
    SettingSpec {
        key: "sandbox.hosts",
        label: "Allowed hosts",
        description: "Hosts commands may reach through Sterna's proxy, beside the ecosystems switched on: `api.example.com`, or `*.example.com` for its subdomains. Global only.",
        kind: Kind::List,
        choices: &[],
        basic: false,
        restart: true,
    },
    SettingSpec {
        key: "sandbox.ecosystems",
        label: "Allowed ecosystems",
        description: "Which package registries and source hosts commands may reach: rust, javascript, deno, python, go, java, ruby, dotnet, php, source. Unset is all of them. Global only.",
        kind: Kind::List,
        choices: &[],
        basic: false,
        restart: true,
    },
    SettingSpec {
        key: "ui.theme",
        label: "Theme",
        description: "The accent colour. Your terminal's own background and transparency are left alone.",
        kind: Kind::Choice,
        choices: THEMES,
        basic: true,
        restart: false,
    },
    SettingSpec {
        key: "ui.background",
        label: "Background",
        description: "Whether your terminal's background is dark or light, so every colour stays readable on it. Auto asks the terminal when Sterna starts.",
        kind: Kind::Choice,
        choices: BACKGROUNDS,
        basic: true,
        restart: false,
    },
    SettingSpec {
        key: "ui.statusline",
        label: "Status line",
        description: "How much the bottom strip carries: everything, the controls only, or nothing.",
        kind: Kind::Choice,
        choices: STATUS_LINES,
        basic: true,
        restart: false,
    },
    SettingSpec {
        key: "ui.sidebar",
        label: "Sidebar",
        description: "The sidebar on the right. Auto shows it only when the terminal is wide enough to spare the columns.",
        kind: Kind::Choice,
        choices: SIDEBAR,
        basic: true,
        restart: false,
    },
    SettingSpec {
        key: "ui.motion",
        label: "Motion",
        description: "How much moves while Sterna works: full, calm (slower, no heartbeat) or off (nothing moves). Only what is changing ever moves.",
        kind: Kind::Choice,
        choices: MOTIONS,
        basic: true,
        restart: false,
    },
    SettingSpec {
        key: "ui.stream",
        label: "Streaming cell",
        description: "What you see while the model is still writing a cell: each action on its own row with a live character count, the code as it forms, or the raw protocol text.",
        kind: Kind::Choice,
        choices: STREAMS,
        basic: true,
        restart: false,
    },
    // -- advanced runtime keys --------------------------------------------
    SettingSpec {
        key: "decisions.reduce_returns",
        label: "Reduce returned logs",
        description: "Ask the decision model what kind of text a large returned field is, and shorten a log by rules before the model reads it. Needs a decisions model.",
        kind: Kind::Bool,
        choices: &[],
        basic: false,
        restart: true,
    },
    SettingSpec {
        key: "limits.reduce_above_tokens",
        label: "Reduce above tokens",
        description: "Estimated tokens above which command output and a returned log are shortened by rules before the model reads them.",
        kind: Kind::Integer,
        choices: &[],
        basic: false,
        restart: true,
    },
    SettingSpec {
        key: "decisions.mode",
        label: "Decisions",
        description: "`off` asks nothing. `shadow` asks and records what would hold. `on` holds a read-only request's effectful cell once.",
        kind: Kind::Choice,
        choices: DECISION_MODES,
        basic: false,
        restart: true,
    },
    SettingSpec {
        key: "decisions.model",
        label: "Decision model",
        description: "The model asked the intent question. Unset means decisions are off.",
        kind: Kind::Model,
        choices: &[],
        basic: false,
        restart: true,
    },
    SettingSpec {
        key: "ask.enabled",
        label: "Ask the person",
        description: "Whether a running program may put a question to you. Off in `explore`, and off with nobody at the keyboard.",
        kind: Kind::Bool,
        choices: &[],
        basic: false,
        restart: true,
    },
    SettingSpec {
        key: "ask.jev",
        label: "Ask weighting",
        description: "`off` asks nothing. `weight` shows the decision model's reading beside each choice. `decide` lets a confident enough answer stand in for you.",
        kind: Kind::Choice,
        choices: ASK_JEV,
        basic: false,
        restart: true,
    },
    SettingSpec {
        key: "ask.decide_above",
        label: "Ask decide confidence",
        description: "Confidence at or above which `ask.jev = decide` answers a question instead of putting it to you.",
        kind: Kind::Float,
        choices: &[],
        basic: false,
        restart: true,
    },
    SettingSpec {
        key: "decisions.hold_above",
        label: "Hold confidence",
        description: "Confidence at or above which a read-only intent holds an effectful cell or frame.",
        kind: Kind::Float,
        choices: &[],
        basic: false,
        restart: true,
    },
    SettingSpec {
        key: "decisions.completion_no_below",
        label: "Completion no threshold",
        description: "The completion question's confidence at or below which a claimed completion gets a not-satisfied finding.",
        kind: Kind::Float,
        choices: &[],
        basic: false,
        restart: true,
    },
    SettingSpec {
        key: "decisions.hygiene_no_below",
        label: "Hygiene no threshold",
        description: "A diff-hygiene `has_tests` confidence at or below which the diff is read as missing tests for the behaviour it changes.",
        kind: Kind::Float,
        choices: &[],
        basic: false,
        restart: true,
    },
    SettingSpec {
        key: "decisions.hygiene_yes_above",
        label: "Hygiene yes threshold",
        description: "A diff-hygiene confidence at or above which an out-of-scope, debug-leftover, deleted-test or changed-signature question is decisive.",
        kind: Kind::Float,
        choices: &[],
        basic: false,
        restart: true,
    },
    SettingSpec {
        key: "limits.cell_wall_clock_s",
        label: "Cell time limit",
        description: "Seconds one cell may run.",
        kind: Kind::Integer,
        choices: &[],
        basic: false,
        restart: true,
    },
    SettingSpec {
        key: "limits.response_bytes",
        label: "Response limit",
        description: "Bytes of tool response a cell may return.",
        kind: Kind::Integer,
        choices: &[],
        basic: false,
        restart: true,
    },
    SettingSpec {
        key: "limits.cells",
        label: "Cell budget",
        description: "Cells one task may run.",
        kind: Kind::Integer,
        choices: &[],
        basic: false,
        restart: true,
    },
    SettingSpec {
        key: "limits.evidence_gate",
        label: "Evidence gate",
        description: "Hold a terminal return once for the deterministic final-state check and the no-progress guard's findings. Off is an ablation switch.",
        kind: Kind::Bool,
        choices: &[],
        basic: false,
        restart: true,
    },
    SettingSpec {
        key: "limits.keep_results",
        label: "Keep results",
        description: "Keep this many of the newest cell results whole and collapse older ones to one line (their values stay bound). 0 keeps all.",
        kind: Kind::Integer,
        choices: &[],
        basic: false,
        restart: true,
    },
    SettingSpec {
        key: "limits.instructions_outline",
        label: "Outline long instructions",
        description: "Show an instruction document over 8 KB as its headings and lines; the model reads a section when its task reaches it.",
        kind: Kind::Bool,
        choices: &[],
        basic: false,
        restart: true,
    },
    SettingSpec {
        key: "limits.autonomy_block",
        label: "Autonomy block",
        description: "Tell the model to carry the request through without asking leave for reversible steps, and not to end on a plan or a promise.",
        kind: Kind::Bool,
        choices: &[],
        basic: false,
        restart: true,
    },
    SettingSpec {
        key: "limits.scope_block",
        label: "Scope block",
        description: "Tell the model to keep changes to what the request needs and report anything else as a follow-up.",
        kind: Kind::Bool,
        choices: &[],
        basic: false,
        restart: true,
    },
    SettingSpec {
        key: "limits.batch_nudge",
        label: "Batching nudge",
        description: "End every cell result with one line asking the model to fetch every independent item in its next cell.",
        kind: Kind::Bool,
        choices: &[],
        basic: false,
        restart: true,
    },
    SettingSpec {
        key: "limits.turn_economy",
        label: "Turn economy",
        description: "Tell the model that every turn re-sends the whole conversation, so it plans the task in fewer, whole-step cells.",
        kind: Kind::Bool,
        choices: &[],
        basic: false,
        restart: true,
    },
    SettingSpec {
        key: "web.enabled",
        label: "Web broker",
        description: "The host-owned web broker. This grants no network access to shells or tools.",
        kind: Kind::Bool,
        choices: &[],
        basic: false,
        restart: true,
    },
    SettingSpec {
        key: "web.allow_domains",
        label: "Allowed domains",
        description: "The domains web.fetch may reach; empty refuses every fetch. `*.example.org` matches subdomains only.",
        kind: Kind::List,
        choices: &[],
        basic: false,
        restart: true,
    },
    SettingSpec {
        key: "web.deny_domains",
        label: "Denied domains",
        description: "Deny wins over allow. Bare names match exactly.",
        kind: Kind::List,
        choices: &[],
        basic: false,
        restart: true,
    },
    SettingSpec {
        key: "web.allow_http",
        label: "Allow plain HTTP",
        description: "Permit `http://` as well as `https://`.",
        kind: Kind::Bool,
        choices: &[],
        basic: false,
        restart: true,
    },
    SettingSpec {
        key: "web.search_endpoint",
        label: "Search endpoint",
        description: "A SearXNG-compatible JSON endpoint. No credentials belong in this value.",
        kind: Kind::Text,
        choices: &[],
        basic: false,
        restart: true,
    },
    SettingSpec {
        key: "web.search_provider",
        label: "Search provider",
        description: "Who answers web.search: `brave` (keyed) or `searxng` (the endpoint above). Unset means searxng when an endpoint is set, else no search.",
        kind: Kind::Choice,
        choices: &["brave", "searxng"],
        basic: false,
        restart: true,
    },
    SettingSpec {
        key: "web.search_key_var",
        label: "Search key variable",
        description: "The NAME of the variable a keyed provider's key is read from — the environment first, then the gateway's credential file. Never the key itself.",
        kind: Kind::Text,
        choices: &[],
        basic: false,
        restart: true,
    },
    SettingSpec {
        key: "web.max_response_bytes",
        label: "Web response limit",
        description: "Bytes one fetched document may contribute.",
        kind: Kind::Integer,
        choices: &[],
        basic: false,
        restart: true,
    },
    SettingSpec {
        key: "web.timeout_seconds",
        label: "Web timeout",
        description: "Seconds one web request may take.",
        kind: Kind::Integer,
        choices: &[],
        basic: false,
        restart: true,
    },
    // -- native permissions, and the legacy migration marker ---------------
    SettingSpec {
        key: "permissions.allow",
        label: "Allowed patterns",
        description: "Native permission patterns, compiled by the existing profile compiler. Never widens a running sandbox.",
        kind: Kind::List,
        choices: &[],
        basic: false,
        restart: true,
    },
    SettingSpec {
        key: "wizard.seen",
        label: "Recommended settings seen",
        description: "The version of Sterna's recommended settings last shown to you; an update that changes them shows the difference once. /wizard runs the setup again.",
        kind: Kind::Integer,
        choices: &[],
        basic: false,
        restart: false,
    },
    SettingSpec {
        key: "permissions.deny",
        label: "Denied patterns",
        description: "Denials beat every allow, and a global denial survives every project overlay.",
        kind: Kind::List,
        choices: &[],
        basic: false,
        restart: true,
    },
    SettingSpec {
        key: "legacy.imported",
        label: "Legacy file imported",
        description: "Set by `import legacy`: `.glasshouse/pane.toml` is preserved but no longer read.",
        kind: Kind::Bool,
        choices: &[],
        basic: false,
        restart: true,
    },
];

/// Every supported key, in panel order: everyday model and presentation rows
/// first, advanced rows after them.
pub fn specs() -> &'static [SettingSpec] {
    SPECS
}

/// One key's spec, or `None` for a key this build does not support.
pub fn spec(key: &str) -> Option<&'static SettingSpec> {
    SPECS.iter().find(|spec| spec.key == key)
}

/// Settings an upgrade removed outright, with where their job went. A saved
/// one is read as unset, taken out of its file and reported once, never
/// refused: the file was true of the version that wrote it.
const RETIRED_KEYS: &[(&str, &str)] = &[
    (
        "session.mode",
        "working modes are gone: /plan <task> plans one request, and every other request works as usual",
    ),
    (
        "modes.explore.writable",
        "Explore is gone: /plan <task> plans one request, reading only",
    ),
    (
        "modes.explore.commands",
        "Explore is gone: /plan <task> plans one request, reading only",
    ),
    (
        "permissions.mode",
        "how much runs without asking is `sandbox.level` now: /sandbox",
    ),
    (
        "decisions.mode_above",
        "requests are no longer switched to Explore: /plan <task> plans one request",
    ),
    (
        "decisions.command_runs_above",
        "the decision model no longer lets commands run: /sandbox sets how much runs without asking",
    ),
    (
        "permissions.full_access",
        "full access is `sandbox.level = \"full\"` now: /sandbox",
    ),
    ("ui.look", "the bird lives in the parrot themes now: /theme"),
    ("ui.voice", "Sterna speaks one plain voice with every theme"),
    (
        "ui.reduced_motion",
        "motion off freezes every animation the same way: /motion off",
    ),
    ("supervisor.enabled", SUPERVISOR_GONE),
    ("supervisor.every", SUPERVISOR_GONE),
    ("supervisor.model", SUPERVISOR_GONE),
    ("decisions.supervision_above", SUPERVISOR_GONE),
    (
        "limits.task_tokens",
        "spend is shown and never capped: nobody knows up front how much a task needs",
    ),
    ("decisions.scout_above", HELPERS_GONE),
    ("decisions.scout_relevance_below", HELPERS_GONE),
    ("decisions.helper_no_below", HELPERS_GONE),
    ("decisions.judge_yes_above", HELPERS_GONE),
    ("decisions.judge_no_below", HELPERS_GONE),
    ("decisions.completion_yes_above", HELPERS_GONE),
    (
        "decisions.drift_no_below",
        "the model's to-do list is gone, and the check of each cell against its current step with it: /plan <task> plans one request",
    ),
];

/// Where the helpers' jobs went when they were removed. Every `helpers.*`
/// key but the two [`MOVED_KEYS`] retires with this sentence.
const HELPERS_GONE: &str =
    "helpers are gone: the model reads what it needs itself, and a subagent takes delegated work";

/// Settings that kept their job and changed their table: the old key, and
/// the key its saved value is written under now.
const MOVED_KEYS: &[(&str, &str)] = &[
    ("helpers.reduce_above_tokens", "limits.reduce_above_tokens"),
    ("helpers.reduce_returns", "decisions.reduce_returns"),
];

/// Where the supervisor's job went when it was removed.
const SUPERVISOR_GONE: &str =
    "the supervisor is gone: a task that stops producing anything still ends on its own";

/// Keys Sterna writes for itself. They load like any other and are never
/// offered as a choice.
const HIDDEN: &[&str] = &["wizard.seen", "legacy.imported"];

/// Whether `key` is Sterna's own bookkeeping rather than a choice.
pub fn hidden(key: &str) -> bool {
    HIDDEN.contains(&key)
}

/// The word a setting that changed kind is saved as now, for the word it
/// was saved as before.
#[must_use]
pub fn migrated_value(key: &str, word: &str) -> Option<&'static str> {
    match (key, word) {
        // The effort the model chooses for itself was called `default`.
        ("session.effort", "default") => Some("auto"),
        _ => None,
    }
}

/// The one-time sentence for a value [`migrated_value`] rewrote: what it
/// is now, and what to do to keep the old behaviour where it differs.
#[must_use]
pub fn migration_notice(key: &str, word: &str, now: &str, written: bool) -> String {
    let what = if written {
        format!("`{key} = {word}` is now `\"{now}\"`")
    } else {
        format!("`{key} = {word}` is read as `\"{now}\"`")
    };
    match (key, now) {
        ("session.effort", "auto") => {
            format!(
                "{what}: the same setting under a clearer name. The model chooses its own effort."
            )
        }
        _ => format!("{what}; /settings changes it."),
    }
}

/// The `sandbox.level` a retired permission word means, when the retired
/// key carried one: the old rungs that asked become `ask`, the ones that did
/// not become `sandboxed`, and `full_access = true` is `full`. `None` for
/// every other key and for a `full_access = false`, which chose nothing.
#[must_use]
pub fn migrated_level(key: &str, word: &str) -> Option<&'static str> {
    match (key, word.trim().to_ascii_lowercase().as_str()) {
        ("permissions.mode", "manual" | "accept-edits" | "accept_edits" | "acceptedits") => {
            Some("ask")
        }
        ("permissions.mode", "auto" | "full") => Some("sandboxed"),
        ("permissions.full_access", "true") => Some("full"),
        _ => None,
    }
}

/// `key` split into the profile overlay it sits in, if any, and the key
/// inside it: `profiles.review.limits.cells` is `(Some("review"),
/// "limits.cells")`.
#[must_use]
pub fn in_profile(key: &str) -> (Option<&str>, &str) {
    match key
        .strip_prefix("profiles.")
        .and_then(|rest| rest.split_once('.'))
    {
        Some((name, inner)) => (Some(name), inner),
        None => (None, key),
    }
}

/// Where a removed setting's job went, if `key` is one -- in a profile
/// overlay as much as at the top of a file.
pub fn retired_key(key: &str) -> Option<&'static str> {
    let (_, key) = in_profile(key);
    if key.starts_with("helpers.") {
        return Some(HELPERS_GONE);
    }
    RETIRED_KEYS
        .iter()
        .find(|(retired, _)| *retired == key)
        .map(|(_, instead)| *instead)
}

/// The key a moved setting is saved under now, in the same profile overlay
/// when it sat in one.
#[must_use]
pub fn moved_key(key: &str) -> Option<String> {
    let (profile, inner) = in_profile(key);
    let (_, now) = MOVED_KEYS.iter().find(|(old, _)| *old == inner)?;
    Some(match profile {
        Some(name) => format!("profiles.{name}.{now}"),
        None => (*now).to_string(),
    })
}

/// Turns the word a person typed into the value that will be written.
///
/// Strings are written as typed -- no TOML quoting -- and every other kind is
/// parsed: `true`/`false`, an integer, or a list written either `a, b` or
/// `["a", "b"]`. Nothing here executes or expands a value.
pub fn validate(key: &str, value: &str) -> Result<toml::Value, String> {
    let spec = spec(key).ok_or_else(|| unknown_key(key))?;
    let typed =
        match spec.kind {
            Kind::Bool => match value.trim() {
                "true" => toml::Value::Boolean(true),
                "false" => toml::Value::Boolean(false),
                other => {
                    return Err(format!(
                        "settings: `{key}` must be `true` or `false`, not `{other}`"
                    ));
                }
            },
            Kind::Integer => {
                let cleaned = value.trim().replace('_', "");
                toml::Value::Integer(cleaned.parse::<i64>().map_err(|_| {
                    format!("settings: `{key}` must be a whole number, not `{value}`")
                })?)
            }
            Kind::Float => toml::Value::Float(
                value
                    .trim()
                    .parse::<f64>()
                    .map_err(|_| format!("settings: `{key}` must be a number, not `{value}`"))?,
            ),
            Kind::List => parse_list(key, value)?,
            Kind::Choice => {
                let word = normalise_choice(key, value.trim());
                toml::Value::String(word)
            }
            Kind::Model | Kind::Text => toml::Value::String(value.trim().to_string()),
        };
    check_value(key, &typed)?;
    Ok(typed)
}

/// The same check, for a value that is already typed -- a file being loaded,
/// or an entry being imported. [`validate`] is this plus the parse.
pub fn check_value(key: &str, value: &toml::Value) -> Result<(), String> {
    let spec = spec(key).ok_or_else(|| unknown_key(key))?;
    match spec.kind {
        Kind::Bool => {
            if !value.is_bool() {
                return Err(format!("settings: `{key}` must be `true` or `false`"));
            }
        }
        Kind::Integer => {
            if !value.is_integer() {
                return Err(format!("settings: `{key}` must be a whole number"));
            }
        }
        Kind::Float => {
            if !(value.is_float() || value.is_integer()) {
                return Err(format!("settings: `{key}` must be a number"));
            }
        }
        Kind::Choice => {
            let word = value
                .as_str()
                .ok_or_else(|| format!("settings: `{key}` must be one of {}", choices(spec)))?;
            if !spec.choices.contains(&word) {
                return Err(format!(
                    "settings: `{key}` must be one of {}, not `{word}`",
                    choices(spec)
                ));
            }
        }
        Kind::Model | Kind::Text => {
            let text = value
                .as_str()
                .ok_or_else(|| format!("settings: `{key}` must be text"))?;
            if text.is_empty() {
                return Err(format!(
                    "settings: `{key}` cannot be empty; remove the override instead"
                ));
            }
            if text.chars().any(char::is_control) {
                return Err(format!("settings: `{key}` must be one line of text"));
            }
        }
        Kind::List => {
            let items = value
                .as_array()
                .ok_or_else(|| format!("settings: `{key}` must be a list"))?;
            for item in items {
                let entry = item
                    .as_str()
                    .ok_or_else(|| format!("settings: every entry of `{key}` must be text"))?;
                if entry.chars().any(char::is_control) {
                    return Err(format!("settings: `{key}` entries must be one line each"));
                }
                if key.starts_with("permissions.") {
                    permission_rule(entry)?;
                }
            }
        }
    }
    // Ranges, model ids, domain patterns and endpoint URLs are the runtime
    // parser's, never restated here. `Choice` keys are excluded because a
    // single-key document is not always a valid configuration on its own:
    // `agents.mode = "pinned"` needs the model that a second edit supplies.
    if is_runtime(key) && spec.kind != Kind::Choice {
        probe(key, value)?;
    }
    Ok(())
}

/// Whether a permission pattern is one the profile compiler understands.
///
/// This is the lexical half -- the kind and its argument. The half that needs
/// a project root (a project-relative pattern escaping the root) belongs to
/// [`crate::sandbox::profile::Profile::compile`], and the store runs it there.
pub fn permission_rule(pattern: &str) -> Result<(), String> {
    let trimmed = pattern.trim();
    if trimmed.is_empty() {
        return Err("settings: an empty permission pattern grants nothing".to_string());
    }
    let (name, argument) = match trimmed.split_once('(') {
        Some((name, rest)) => match rest.strip_suffix(')') {
            Some(argument) => (name.trim(), Some(argument)),
            None => {
                return Err(format!(
                    "settings: `{trimmed}` is missing its closing parenthesis"
                ));
            }
        },
        None => (trimmed, None),
    };
    match name {
        "Read" | "Write" | "Edit" => match argument.map(str::trim) {
            Some(argument) if !argument.is_empty() => Ok(()),
            _ => Err(format!(
                "settings: `{trimmed}` names no path; a bare `{name}` grants nothing"
            )),
        },
        "Bash" => Ok(()),
        "WebFetch" | "WebSearch" => Err(format!(
            "settings: `{trimmed}` grants nothing: network reach is never a permission pattern \
             (docs/sandbox.md, never grantable 1); configure `web.*` instead"
        )),
        other if other.starts_with("mcp__") => {
            if argument.is_some() {
                return Err(format!(
                    "settings: `{trimmed}` takes no argument; name the MCP tool alone"
                ));
            }
            Ok(())
        }
        other => Err(format!(
            "settings: `{other}` is not a permission pattern kind Sterna understands; \
             use Read, Write, Edit, Bash or mcp__server__tool"
        )),
    }
}

/// The valid words for a choice key, as an error sentence spells them.
pub fn choices(spec: &SettingSpec) -> String {
    spec.choices
        .iter()
        .map(|word| format!("`{word}`"))
        .collect::<Vec<_>>()
        .join(", ")
}

fn normalise_choice(key: &str, word: &str) -> String {
    match (key, word) {
        // `/statusline`'s alias.
        ("ui.statusline", "hide") => "hidden".to_string(),
        _ => word.to_string(),
    }
}

fn parse_list(key: &str, value: &str) -> Result<toml::Value, String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Ok(toml::Value::Array(Vec::new()));
    }
    if trimmed.starts_with('[') {
        let parsed: toml::Value = toml::from_str(&format!("value = {trimmed}"))
            .map_err(|error| format!("settings: `{key}` is not a valid list: {error}"))?;
        let items = parsed
            .get("value")
            .and_then(toml::Value::as_array)
            .ok_or_else(|| format!("settings: `{key}` must be a list"))?;
        return Ok(toml::Value::Array(items.clone()));
    }
    Ok(toml::Value::Array(
        trimmed
            .split(',')
            .map(str::trim)
            .filter(|entry| !entry.is_empty())
            .map(|entry| toml::Value::String(entry.to_string()))
            .collect(),
    ))
}

/// Builds the smallest document that sets `key` and parses it exactly as a
/// session start would, so a range lives in one place.
fn probe(key: &str, value: &toml::Value) -> Result<(), String> {
    let mut document = toml::value::Table::new();
    nest(&mut document, key, value.clone());
    let text = toml::to_string(&toml::Value::Table(document))
        .map_err(|error| format!("settings: `{key}`: {error}"))?;
    SternaConfig::parse_profile(&text, None)
        .map(|_| ())
        .map_err(|error| {
            format!(
                "settings: {}",
                error.strip_prefix("config.toml: ").unwrap_or(&error)
            )
        })
}

fn nest(table: &mut toml::value::Table, key: &str, value: toml::Value) {
    let mut parts = key.split('.').peekable();
    let mut cursor = table;
    while let Some(part) = parts.next() {
        if parts.peek().is_none() {
            cursor.insert(part.to_string(), value);
            return;
        }
        let entry = cursor
            .entry(part.to_string())
            .or_insert_with(|| toml::Value::Table(toml::value::Table::new()));
        cursor = match entry {
            toml::Value::Table(table) => table,
            _ => return,
        };
    }
}

fn unknown_key(key: &str) -> String {
    let leaf = key.rsplit('.').next().unwrap_or(key);
    let near: Vec<&str> = SPECS
        .iter()
        .map(|spec| spec.key)
        .filter(|candidate| {
            candidate.rsplit('.').next() == Some(leaf) || candidate.starts_with(key)
        })
        .take(3)
        .collect();
    if near.is_empty() {
        format!("settings: `{key}` is not a setting Sterna supports")
    } else {
        format!(
            "settings: `{key}` is not a setting Sterna supports; did you mean {}?",
            near.iter()
                .map(|key| format!("`{key}`"))
                .collect::<Vec<_>>()
                .join(" or ")
        )
    }
}

/// The value the runtime would use for `key` when nothing has set it — for
/// the panel to show, and **for nothing else**.
///
/// **This is a display concern and it must never become a configured one.**
/// The first attempt at removing the panel's `unset` rows put these into the
/// effective configuration instead, and two security tests caught it inside
/// a minute: an absent `permissions.full_access` and a present `false` are
/// the same to a reader and very different to the loader, which drops a
/// project document's copy of that key precisely by noticing it is there.
///
/// So: the loader keeps answering "nothing set this", and the panel answers
/// "and this is what happens when nothing does".
#[must_use]
pub fn shown_default(key: &str) -> Option<String> {
    let decisions = crate::config::DecisionsConfig::default();
    let ask = crate::config::AskConfig::default();
    Some(match key {
        "sandbox.level" => crate::permissions::Level::default().name().to_string(),
        "sandbox.hosts" => "none".into(),
        "sandbox.ecosystems" => "all".into(),
        "wizard.seen" => "0".into(),
        "permissions.allow" | "permissions.deny" => "none".into(),
        "limits.evidence_gate" => crate::config::Limits::default().evidence_gate.to_string(),
        "limits.keep_results" => crate::config::Limits::default().keep_results.to_string(),
        "limits.turn_economy" => crate::config::Limits::default().turn_economy.to_string(),
        "limits.autonomy_block" => crate::config::Limits::default().autonomy_block.to_string(),
        "limits.scope_block" => crate::config::Limits::default().scope_block.to_string(),
        "limits.batch_nudge" => crate::config::Limits::default().batch_nudge.to_string(),
        "limits.instructions_outline" => crate::config::Limits::default()
            .instructions_outline
            .to_string(),
        "limits.reduce_above_tokens" => crate::config::REDUCE_ABOVE_TOKENS_DEFAULT.to_string(),
        "decisions.reduce_returns" => decisions.reduce_returns.to_string(),
        "ask.enabled" => ask.enabled.to_string(),
        // The runtime's own defaults, so the ● marks what actually runs.
        "ask.jev" => ask.jev.as_str().into(),
        "decisions.mode" => decisions.mode.as_str().into(),
        "agents.slots.quick.effort" => crate::config::slot_effort("quick").name().into(),
        "agents.slots.balanced.effort" => crate::config::slot_effort("balanced").name().into(),
        "agents.slots.deep.effort" => crate::config::slot_effort("deep").name().into(),
        "agents.slots.heavy.effort" => crate::config::slot_effort("heavy").name().into(),
        "ask.decide_above" => ask.decide_above.to_string(),
        "decisions.hygiene_no_below" => decisions.hygiene_no_below.to_string(),
        "decisions.hygiene_yes_above" => decisions.hygiene_yes_above.to_string(),
        _ => return None,
    })
}
