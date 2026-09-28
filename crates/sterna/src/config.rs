//! Sterna runtime settings, layered by the native settings store at startup
//! (`docs/configuration.md`). A missing file means every default
//! the runtime limits already used before this package existed
//! (`runtime-contract.md` §7), so an absent file changes no
//! existing test.

use std::path::Path;

use crate::tools::registry;

/// `[limits]` -- the runtime constants `runtime-contract.md` §7 and the cell
/// limit, now loadable. Token spend is telemetry rather than a limit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    pub cell_wall_clock_s: u64,
    pub response_bytes: usize,
    /// A ceiling on cells **only when this person set one**, and `0` in the
    /// file means the same as absent (the user, 2026-09-17: "Limits are dumb
    /// for abstract tasks").
    ///
    /// It used to default to 120, and on 2026-09-17 that default ended a
    /// four-hour session at cell 120 of 120, mid-implementation, with code
    /// that did not compile — and cancelled that session's own `cargo test`
    /// job on the way out. What ends a task now is evidence that it has
    /// stopped producing anything: `progress::Stall`'s run of empty windows.
    pub cells: Option<u64>,
    /// Whether a terminal return is held once for the deterministic
    /// final-state contract check and the no-progress guard's findings
    /// (`smarter-cheaper-roadmap.md`, *Evidence-gated completion*). On by
    /// default; off is an ablation switch, not a product mode.
    pub evidence_gate: bool,
    /// The share of a **known** context window at which the conversation is
    /// swept once, as a percentage (the user, 2026-09-17: *"should be done in
    /// one deliberate sweep … because after that you pay less again per
    /// call"*).
    ///
    /// Not a ceiling and never ends anything: over it, older cell results
    /// lose the sections the newest one restates in full. An unknown window
    /// sweeps nothing, because there is no fraction to be over.
    pub compact_above_percent: u64,
    /// Keep this many of the newest cell results in full and collapse every
    /// older one to its first line (`prompt::keep_recent_results`); `0`
    /// keeps them all, and is the default: a collapse rewrites earlier turns,
    /// which restarts the provider's cache from that point, and history is
    /// append-only except for compaction (user, 2026-09-24).
    pub keep_results: usize,
    /// Show a long instruction document as its headings and their lines
    /// (`project::instructions::root_outlined`). Off: measured worse.
    pub instructions_outline: bool,
    /// Tell the model what a turn costs (`prompt::TURN_ECONOMY`), so it
    /// plans the task in fewer, whole-step cells. Off until measured.
    pub turn_economy: bool,
    /// `prompt::AUTONOMY_BLOCK` in the system prompt. Off until measured.
    pub autonomy_block: bool,
    /// `prompt::SCOPE_BLOCK` in the system prompt. Off until measured.
    pub scope_block: bool,
    /// `prompt::BATCH_NUDGE` at the end of every cell result. Off until
    /// measured.
    pub batch_nudge: bool,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            cell_wall_clock_s: 30,
            response_bytes: 16 * 1024,
            // No ceiling unless this person asks for one.
            cells: None,
            evidence_gate: true,
            // High on purpose. The user, 2026-09-17: "Compression at 252k
            // can be worth it but you pay with one call. 252k is not that
            // much by today's model though. I start compression at around
            // 800k in Claude code." A fraction carries that across models:
            // 85% of a 922k window is 784k, and 85% of a small one is small.
            compact_above_percent: 85,
            // Off since 2026-09-24: a collapse rewrites earlier turns, and
            // history is append-only except for compaction.
            keep_results: 0,
            // Measured and dropped the same day: with only headings the
            // model reread the document 30-47 times and found fewer facts.
            instructions_outline: false,
            turn_economy: false,
            autonomy_block: false,
            scope_block: false,
            batch_nudge: false,
        }
    }
}

/// `[decisions] mode` -- whether the decision model's hold reaches the task
/// model at all. `off` and an unset `model` are both "no request is ever
/// made"; `mode` only matters once a model is configured.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DecisionMode {
    Off,
    #[default]
    Shadow,
    On,
}

impl DecisionMode {
    pub fn parse(value: &str) -> Result<Self, String> {
        match value {
            "off" => Ok(Self::Off),
            "shadow" => Ok(Self::Shadow),
            "on" => Ok(Self::On),
            other => Err(format!(
                "config.toml: `[decisions] mode` must be \"off\", \"shadow\" or \"on\", not `{other}`"
            )),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Shadow => "shadow",
            Self::On => "on",
        }
    }
}

/// `[decisions]` -- the decision model's one intent question and the hold it
/// buys (`docs/decisions.md`). `model` has no default: unset means
/// decisions are off, said once at start.
#[derive(Debug, Clone, PartialEq)]
pub struct DecisionsConfig {
    pub model: Option<String>,
    pub mode: DecisionMode,
    /// Confidence at or above which a read-only intent holds an effectful
    /// cell or frame. `0.5..=1.0`.
    pub hold_above: f64,
    /// Confidence at or above which a `needs_exploration` complexity answer
    /// adds `preflight::SIGNAL_DECIDED_EXPLORATION` to `should_scout`'s
    /// signals (F2, map 2614/2615's paragraph). `0.5..=1.0`.
    pub scout_above: f64,
    /// The completion question's noul at or below which a claimed completion
    /// gets a `RequestNotSatisfied` finding (2616). `0.0..=0.5`.
    pub completion_no_below: f64,
    /// The completion question's noul at or above which the fresh checker is
    /// spared, when nothing else was found. `0.5..=1.0`.
    pub completion_yes_above: f64,
    /// A diff-hygiene `has_tests` noul at or below which the diff is read as
    /// missing tests for the behaviour it changes (2641). `0.0..=0.5`.
    pub hygiene_no_below: f64,
    /// A diff-hygiene `out_of_scope`/`debug_leftovers`/`deletes_tests`/
    /// `changes_signature` noul at or above which that question is decisive
    /// (2641). `0.5..=1.0`.
    pub hygiene_yes_above: f64,
    /// A `judge` acceptance item's noul at or above which the item counts as
    /// satisfied without the fresh checker (2642). `0.5..=1.0`.
    pub judge_yes_above: f64,
    /// A `judge` acceptance item's noul at or below which the item becomes a
    /// finding held once (2642). `0.0..=0.5`.
    pub judge_no_below: f64,
    /// The drift question's noul at or below which an effectful cell is held
    /// once, as not doing what the plan's current step says (2643).
    /// `0.0..=0.5`.
    pub drift_no_below: f64,
    /// Confidence at or above which a `read_only` intent proposes `explore`
    /// for one request, in `execute`, unpinned (2639). `0.5..=1.0`.

    /// A scout candidate's own relevance noul at or below which it is left
    /// out of what the Scout is served (2644). `0.0..=0.5`.
    pub scout_relevance_below: f64,
    /// A helper result's own noul at or below which its record carries the
    /// one line that it may not answer what was asked (2645) -- the same
    /// floor for the preflight Scout's own result and the completion gate's
    /// fresh checker. `0.0..=0.5`.
    pub helper_no_below: f64,
}

impl Default for DecisionsConfig {
    fn default() -> Self {
        Self {
            model: None,
            mode: DecisionMode::default(),
            hold_above: 0.85,
            scout_above: 0.85,
            completion_no_below: 0.10,
            completion_yes_above: 0.90,
            hygiene_no_below: 0.10,
            hygiene_yes_above: 0.90,
            judge_yes_above: 0.90,
            judge_no_below: 0.10,
            drift_no_below: 0.10,
            scout_relevance_below: 0.10,
            helper_no_below: 0.10,
        }
    }
}

/// `[ask] jev` -- how far the decision model gets to go on a question the
/// program put to the person.
///
/// `weight` is the default wherever a decision model exists: the person still
/// chooses, and Jev's reading of the request, the diff and the findings sits
/// beside each option as a number. `decide` lets a confident enough answer
/// stand in for the person; `off` never asks it at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AskJev {
    Off,
    #[default]
    Weight,
    Decide,
}

impl AskJev {
    pub fn parse(value: &str) -> Result<Self, String> {
        match value {
            "off" => Ok(Self::Off),
            "weight" => Ok(Self::Weight),
            "decide" => Ok(Self::Decide),
            other => Err(format!(
                "config.toml: `[ask] jev` must be \"off\", \"weight\" or \"decide\", not `{other}`"
            )),
        }
    }

    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Weight => "weight",
            Self::Decide => "decide",
        }
    }
}

/// `[ask]` -- whether a running program may put a question to the person, and
/// how much of the answer the decision model is allowed to supply.
///
/// Off by default: a question suspends the work until somebody looks at the
/// screen, so a session that never asked for the capability must not gain one
/// that can stall it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AskConfig {
    pub enabled: bool,
    pub jev: AskJev,
    /// Confidence at or above which `jev = "decide"` answers instead of the
    /// person. `0.5..=1.0`, the same span every other confidence takes.
    pub decide_above: f64,
}

impl Default for AskConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            jev: AskJev::default(),
            decide_above: 0.85,
        }
    }
}

/// The whole of `config.toml`. `project.rs`'s own invariant -- loading edits
/// `[helpers]` -- the little-helper tier (`docs/helpers.md`).
///
/// `model` has no default: unset means helpers are off, said once at
/// start. A helper spends money on the
/// user's behalf, so the fail-closed direction is *not configured, not run*.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HelpersConfig {
    pub model: Option<String>,
    /// Per-helper reasoning levels. These are hard defaults, individually
    /// overridable under `[helpers.effort]`.
    pub effort: HelperEfforts,
    /// Run the pushed Scout before the task model's first turn. Off by
    /// default: configured helpers remain callable without paying for a
    /// redundant repository scan on every request.
    pub preflight: bool,
    /// When `preflight` is on, whether every task pays for the Scout or only
    /// a task whose request carries an uncertainty signal
    /// (`smarter-cheaper-roadmap.md`, *Adaptive orchestration*).
    pub preflight_scope: PreflightScope,
    /// What the completion gate does once a task is accepted.
    pub completion: CompletionStyle,
    /// When the fresh independent checker reads the finished work behind
    /// the answer: the original request, the answer, the task's diff and its
    /// exact evidence, never the parent's rationale. One cheap request each
    /// time it runs; [`CompletionCheck::Auto`] runs it only after big work.
    pub completion_check: CompletionCheck,
    /// Derive an acceptance list from the request before the first turn and
    /// check it when the model claims completion (`acceptance.rs`). One
    /// cheap toolless request per task; the list is shown to the model.
    pub acceptance_list: bool,
    pub enabled: bool,
    /// The most helper calls one cell may make, so a loop cannot issue three
    /// hundred requests inside a single program.
    pub calls_per_cell: u32,
    /// Estimated tokens of a command result above which the pushed reducer
    /// is worth a cheap request. Below it the parent reads the output itself.
    pub reduce_above_tokens: usize,
    /// Ask the decision model what kind of text a large returned field is,
    /// and send a log to the reducer before the model reads it
    /// (`session/returned.rs`). Needs `[decisions] model` and a mode other
    /// than `off`; the whole value stays bound, so it acts in `shadow` too.
    pub reduce_returns: bool,
    /// Ask the decision model whether a returned value is enough to go on,
    /// and fetch the in-project files it names when it is not
    /// (`session/returned.rs`). Needs `[decisions] model`; in `shadow` mode
    /// the answer is recorded and nothing is fetched. Off until measured.
    pub prefetch_returns: bool,
    /// Dissect an exploring request in one toolless request over the
    /// project's file listing instead of the Scout's search loop
    /// (2026-09-23: ~15x cheaper and 4-8x faster at equal recall offline).
    /// Off until measured end to end.
    pub scout_oneshot: bool,
    /// Write `.sterna/learned.md` behind the answer of a task that had to
    /// search (`learned.rs`), and read it into the next task's prompt.
    pub learn: bool,
    /// Whether a settings file names `completion_check` / `learn` itself.
    /// A one-task run starts the work behind its answer only when it does
    /// (`session/after.rs::may_start`).
    pub completion_check_set: bool,
    pub learn_set: bool,
}

/// `[helpers] completion_check` -- when the checker reads the finished work.
/// A turn that changed nothing is never checked, whatever this says
/// (`session::after::wants_check`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CompletionCheck {
    /// After big work: a list the Scout wrote, or many files, many lines or
    /// many cells.
    #[default]
    Auto,
    /// After every answer that changed something.
    Always,
    Off,
}

impl CompletionCheck {
    pub const NAMES: [&'static str; 3] = ["auto", "always", "off"];

    pub fn parse(value: &str) -> Result<Self, String> {
        match value {
            "auto" => Ok(Self::Auto),
            "always" => Ok(Self::Always),
            "off" => Ok(Self::Off),
            other => Err(format!(
                "config.toml: `completion_check` must be \"auto\", \"always\" or \"off\", not `{other}`"
            )),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Always => "always",
            Self::Off => "off",
        }
    }
}

/// `[helpers] preflight_scope` -- which tasks the Scout runs for when
/// preflight is on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PreflightScope {
    /// Only when the request names a path that does not exist, a command the
    /// session cannot run, verification the session cannot see, or is long
    /// enough that a scan is expected to save parent attention.
    #[default]
    Auto,
    /// Every task, as before this key existed.
    Always,
}

impl PreflightScope {
    pub fn parse(value: &str) -> Result<Self, String> {
        match value {
            "auto" => Ok(Self::Auto),
            "always" => Ok(Self::Always),
            other => Err(format!(
                "config.toml: `preflight_scope` must be \"auto\" or \"always\", not `{other}`"
            )),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Always => "always",
        }
    }
}

/// `[helpers.effort]` -- effort follows the work a helper does rather than
/// the model tier it happens to run on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HelperEfforts {
    pub find: crate::wire::Effort,
    pub reduce: crate::wire::Effort,
    pub check: crate::wire::Effort,
    /// The acceptance lister: fixed line forms from a request.
    pub accept: crate::wire::Effort,
    /// The mender: the punctuation of a cell that did not parse.
    pub mend: crate::wire::Effort,
}

impl HelperEfforts {
    pub fn for_helper(self, name: &str) -> Option<crate::wire::Effort> {
        match name {
            "find" => Some(self.find),
            "reduce" => Some(self.reduce),
            "check" => Some(self.check),
            "accept" => Some(self.accept),
            "mend" => Some(self.mend),
            // No silent provider-default fallback: a new helper must choose a
            // hard policy and become a config key before it can run.
            _ => None,
        }
    }
}

impl Default for HelperEfforts {
    fn default() -> Self {
        Self {
            // Lookup is mechanically verifiable; filtering needs more
            // discrimination; accepting or rejecting a claim is the most
            // consequential helper decision.
            find: crate::wire::Effort::Low,
            reduce: crate::wire::Effort::Medium,
            // Medium since 2026-09-23: the check is a note behind the
            // answer, and at high it took 85 s on a two-line fix.
            check: crate::wire::Effort::Medium,
            accept: crate::wire::Effort::Low,
            // A parse failure is punctuation, and the mender is shown the
            // parser's own verdict on where it is -- there is nothing to
            // deliberate about, and a model reasoning at length over a
            // missing brace is spending the turn this exists to save.
            mend: crate::wire::Effort::Low,
        }
    }
}

/// `[helpers] completion` -- what the gate says when a task is ACCEPTED.
///
/// A refusal is not affected: an unverified completion is reported either way.
/// This is only about the accepted case, where the honest default is silence —
/// a line printed after every task is a line nobody reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CompletionStyle {
    /// Say nothing on acceptance.
    #[default]
    Silent,
    /// One or two sentences recapping the session, and one suggested next
    /// prompt. Costs one cheap request per completed task.
    Recap,
}

impl CompletionStyle {
    pub fn parse(value: &str) -> Result<Self, String> {
        match value {
            "silent" => Ok(Self::Silent),
            "recap" => Ok(Self::Recap),
            other => Err(format!(
                "config.toml: `completion` must be \"silent\" or \"recap\", not `{other}`"
            )),
        }
    }
}

impl Default for HelpersConfig {
    fn default() -> Self {
        Self {
            model: None,
            effort: HelperEfforts::default(),
            preflight: false,
            preflight_scope: PreflightScope::Auto,
            completion: CompletionStyle::Silent,
            // Behind the answer since 2026-09-23 (`session/after.rs`), and
            // only after big work since it read a question it could not
            // settle: "cannot tell" after three turns, every time.
            completion_check: CompletionCheck::Auto,
            // Off: its derived items were the false alarms measured that day
            // (a prose "contains" item, a command the machine lacks).
            acceptance_list: false,
            enabled: true,
            calls_per_cell: 8,
            reduce_above_tokens: 2048,
            // On since 2026-09-23: Jev reads a field's shape at 97 % and the
            // whole value stays bound, so a shortened log loses nothing.
            reduce_returns: true,
            prefetch_returns: false,
            scout_oneshot: false,
            learn: true,
            completion_check_set: false,
            learn_set: false,
        }
    }
}

/// nothing -- holds here too: nothing in this module opens a path for writing.
///
/// `Eq` is not derived: [`DecisionsConfig::hold_above`] is an `f64`, and
/// nothing here needs `SternaConfig` as a map key or in a set.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct SternaConfig {
    pub limits: Limits,
    pub helpers: HelpersConfig,
    pub agents: AgentsConfig,
    pub model: ModelConfig,
    pub web: crate::web::WebConfig,
    pub decisions: DecisionsConfig,
    pub ask: AskConfig,
    pub wizard: WizardConfig,
}

/// `[wizard]` -- which version of Sterna's recommended settings the person
/// last saw, so an update that changes them shows the difference once
/// (`session/setup.rs`). Global only: it is about the person, not a project.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct WizardConfig {
    pub seen: u32,
}

/// `[model]` -- the parent tier, the one the person talks to.
///
/// It is here for the same reason `[helpers] model` and `[agents] model` are:
/// a session is three models, and the one you chose last should not be the
/// only one that forgets. There is deliberately no compiled-in fallback:
/// startup requires either this value or `--model`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ModelConfig {
    pub parent: Option<String>,
}

mod agents;
use agents::parse_agents;
pub use agents::{AgentSlot, AgentsConfig, AgentsMode, SLOT_NAMES, slot_effort};

/// One integer key's valid range, spelled once so the refusal sentence and
/// the check it comes from cannot drift apart.
struct Range {
    key: &'static str,
    min: i64,
    max: i64,
}

const CELL_WALL_CLOCK_S: Range = Range {
    key: "cell_wall_clock_s",
    min: 1,
    max: 600,
};
const RESPONSE_BYTES: Range = Range {
    key: "response_bytes",
    min: 1024,
    max: 1_048_576,
};
const CELLS: Range = Range {
    key: "cells",
    min: 1,
    max: 1000,
};
/// Below half a window a sweep removes little and rebuilds the cached prefix
/// for it; at 100 there is no room left to sweep into.
const COMPACT_ABOVE_PERCENT: Range = Range {
    key: "compact_above_percent",
    min: 50,
    max: 99,
};
/// `0` keeps every result; past a few dozen there is nothing left to save.
const KEEP_RESULTS: Range = Range {
    key: "keep_results",
    min: 0,
    max: 64,
};
const CALLS_PER_CELL: Range = Range {
    key: "calls_per_cell",
    min: 1,
    max: 64,
};
const REDUCE_ABOVE_TOKENS: Range = Range {
    key: "reduce_above_tokens",
    min: 256,
    max: 32_768,
};

impl Range {
    fn check(&self, value: i64) -> Result<i64, String> {
        if value < self.min || value > self.max {
            Err(format!(
                "config.toml: `{}` must be between {} and {}",
                self.key, self.min, self.max
            ))
        } else {
            Ok(value)
        }
    }
}

impl SternaConfig {
    /// Loads global and `<root>/.sterna/config.toml` settings. Missing files use defaults,
    /// never an error -- most projects have none.
    pub fn load(root: &Path) -> Result<Self, String> {
        Self::load_profile(root, None)
    }

    /// Select a named configuration overlay without modifying project defaults.
    pub fn load_profile(root: &Path, profile: Option<&str>) -> Result<Self, String> {
        Ok(crate::settings::Store::new(root)?.load(profile)?.config)
    }

    pub fn parse_profile(text: &str, selected: Option<&str>) -> Result<Self, String> {
        let mut value: toml::Value =
            toml::from_str(text).map_err(|error| format!("config.toml: {error}"))?;
        let table = value.as_table_mut().ok_or("config.toml must be a table")?;
        let profiles = table.remove("profiles");
        if let Some(profiles) = &profiles {
            let profiles = profiles
                .as_table()
                .ok_or("config.toml: [profiles] must be a table")?;
            for (name, overlay) in profiles {
                if name.is_empty()
                    || !name
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b))
                    || !overlay.is_table()
                {
                    return Err(
                        "config.toml: profiles must be named tables (letters, digits, _ or -)"
                            .into(),
                    );
                }
            }
        }
        if let Some(name) = selected {
            let overlay = profiles
                .as_ref()
                .and_then(|profiles| profiles.get(name))
                .ok_or_else(|| format!("config.toml: no profile named `{name}`"))?;
            if let Some(agents) = overlay.get("agents").and_then(toml::Value::as_table)
                && matches!(
                    agents.get("mode").and_then(toml::Value::as_str),
                    Some("off" | "auto")
                )
                && !agents.contains_key("model")
                && let Some(base_agents) =
                    value.get_mut("agents").and_then(toml::Value::as_table_mut)
            {
                base_agents.remove("model");
            }
            merge_tables(&mut value, overlay);
        }
        Self::parse_base(&toml::to_string(&value).map_err(|error| format!("config.toml: {error}"))?)
    }

    /// Parses without touching the filesystem.
    ///
    /// Public so that a caller about to **write** `config.toml` can prove the
    /// text it is about to save loads — the same check `/permissions` makes
    /// by compiling a profile before saving `settings.json`. Reading stays
    /// this module's only filesystem verb; the write belongs to the command
    /// that made the edit.
    pub fn parse(text: &str) -> Result<Self, String> {
        Self::parse_profile(text, None)
    }

    fn parse_base(text: &str) -> Result<Self, String> {
        let value: toml::Value = toml::from_str(text).map_err(|e| format!("config.toml: {e}"))?;
        let table = value.as_table().ok_or_else(|| {
            "config.toml: must be a table of [limits], [helpers] and [agents]".to_string()
        })?;

        for key in table.keys() {
            if ![
                "limits",
                "helpers",
                "agents",
                "model",
                "web",
                "decisions",
                "ask",
                "wizard",
            ]
            .contains(&key.as_str())
            {
                return Err(format!(
                    "config.toml: unknown table `[{key}]`; only [limits], [helpers], \
                     [agents], [model], [web], [decisions], [ask] and [wizard] are recognised"
                ));
            }
        }

        let limits = match table.get("limits") {
            Some(value) => parse_limits(value)?,
            None => Limits::default(),
        };

        let helpers = match table.get("helpers") {
            Some(value) => parse_helpers(value)?,
            None => HelpersConfig::default(),
        };

        let agents = match table.get("agents") {
            Some(value) => parse_agents(value)?,
            None => AgentsConfig::default(),
        };

        let model = match table.get("model") {
            Some(value) => parse_model(value)?,
            None => ModelConfig::default(),
        };

        let web = match table.get("web") {
            Some(value) => value
                .clone()
                .try_into::<crate::web::WebConfig>()
                .map_err(|error| format!("config.toml: [web]: {error}"))?,
            None => crate::web::WebConfig::default(),
        };
        crate::web::WebBroker::new(web.clone())?;

        let decisions = match table.get("decisions") {
            Some(value) => parse_decisions(value)?,
            None => DecisionsConfig::default(),
        };

        let ask = match table.get("ask") {
            Some(value) => parse_ask(value)?,
            None => AskConfig::default(),
        };
        let wizard = match table.get("wizard") {
            Some(value) => {
                let table = table_of(value, "wizard")?;
                if let Some(key) = table.keys().find(|key| key.as_str() != "seen") {
                    return Err(format!("config.toml: unknown key `{key}` in [wizard]"));
                }
                let seen = match table.get("seen") {
                    None => 0,
                    Some(value) => value
                        .as_integer()
                        .and_then(|seen| u32::try_from(seen).ok())
                        .ok_or("config.toml: `[wizard] seen` must be a whole number")?,
                };
                WizardConfig { seen }
            }
            None => WizardConfig::default(),
        };

        Ok(Self {
            limits,
            helpers,
            agents,
            model,
            web,
            decisions,
            ask,
            wizard,
        })
    }
}

fn merge_tables(base: &mut toml::Value, overlay: &toml::Value) {
    if let (Some(base), Some(overlay)) = (base.as_table_mut(), overlay.as_table()) {
        for (key, value) in overlay {
            match base.get_mut(key) {
                Some(existing) if existing.is_table() && value.is_table() => {
                    merge_tables(existing, value)
                }
                _ => {
                    base.insert(key.clone(), value.clone());
                }
            }
        }
    }
}

/// `[agents]` -- an explicit mode plus a model for `pinned`.
///
/// A legacy table containing only `model` is interpreted as `pinned`, so an
/// existing project keeps the behaviour it selected before modes existed.
/// `[model] parent` -- one optional key, refused the same way the other two
/// tiers' model names are, so one validator covers all three.
fn parse_model(value: &toml::Value) -> Result<ModelConfig, String> {
    let table = table_of(value, "model")?;
    for key in table.keys() {
        if key != "parent" {
            return Err(format!(
                "config.toml: unknown key `{key}` in [model]; only `parent` is recognised"
            ));
        }
    }
    let parent = match table.get("parent") {
        None => None,
        Some(value) => {
            let text = value
                .as_str()
                .ok_or_else(|| "config.toml: `parent` must be a string".to_string())?;
            validate_parent_model(text)?;
            Some(text.to_string())
        }
    };
    Ok(ModelConfig { parent })
}

fn table_of<'a>(value: &'a toml::Value, name: &str) -> Result<&'a toml::value::Table, String> {
    value
        .as_table()
        .ok_or_else(|| format!("config.toml: [{name}] must be a table"))
}

fn int_field(table: &toml::value::Table, key: &str) -> Result<Option<i64>, String> {
    match table.get(key) {
        None => Ok(None),
        Some(value) => value
            .as_integer()
            .map(Some)
            .ok_or_else(|| format!("config.toml: `{key}` must be an integer")),
    }
}

fn parse_limits(value: &toml::Value) -> Result<Limits, String> {
    let table = table_of(value, "limits")?;
    let defaults = Limits::default();

    for key in table.keys() {
        if ![
            "cell_wall_clock_s",
            "response_bytes",
            // Accepted as a no-op so an existing project does not stop
            // starting when token caps are removed. New sessions account for
            // spend but never use this value to control execution.
            "task_tokens",
            "cells",
            "evidence_gate",
            "compact_above_percent",
            "keep_results",
            "instructions_outline",
            "turn_economy",
            "autonomy_block",
            "scope_block",
            "batch_nudge",
        ]
        .contains(&key.as_str())
        {
            return Err(format!("config.toml: unknown key `{key}` in [limits]"));
        }
    }
    let evidence_gate = match table.get("evidence_gate") {
        None => defaults.evidence_gate,
        Some(value) => value
            .as_bool()
            .ok_or_else(|| "config.toml: `evidence_gate` must be true or false".to_string())?,
    };

    let cell_wall_clock_s = match int_field(table, "cell_wall_clock_s")? {
        Some(v) => u64::try_from(CELL_WALL_CLOCK_S.check(v)?).expect("range is non-negative"),
        None => defaults.cell_wall_clock_s,
    };
    let response_bytes = match int_field(table, "response_bytes")? {
        Some(v) => usize::try_from(RESPONSE_BYTES.check(v)?).expect("range is non-negative"),
        None => defaults.response_bytes,
    };
    if let Some(value) = table.get("task_tokens")
        && !value.is_integer()
    {
        return Err("config.toml: `task_tokens` must be an integer".into());
    }
    let compact_above_percent = match int_field(table, "compact_above_percent")? {
        Some(v) => u64::try_from(COMPACT_ABOVE_PERCENT.check(v)?).expect("range is non-negative"),
        None => defaults.compact_above_percent,
    };
    let cells = match int_field(table, "cells")? {
        // Zero is the explicit "no ceiling", as `[agents] deadline_minutes`
        // spells the same intent; absent leaves the default, which is none.
        Some(0) => None,
        Some(v) => Some(u64::try_from(CELLS.check(v)?).expect("range is non-negative")),
        None => defaults.cells,
    };

    let keep_results = match int_field(table, "keep_results")? {
        Some(v) => usize::try_from(KEEP_RESULTS.check(v)?).expect("range is non-negative"),
        None => defaults.keep_results,
    };
    let instructions_outline = match table.get("instructions_outline") {
        None => defaults.instructions_outline,
        Some(value) => value.as_bool().ok_or_else(|| {
            "config.toml: `instructions_outline` must be true or false".to_string()
        })?,
    };

    let turn_economy = match table.get("turn_economy") {
        None => defaults.turn_economy,
        Some(value) => value
            .as_bool()
            .ok_or_else(|| "config.toml: `turn_economy` must be true or false".to_string())?,
    };
    let switch = |key: &str, default: bool| -> Result<bool, String> {
        match table.get(key) {
            None => Ok(default),
            Some(value) => value
                .as_bool()
                .ok_or_else(|| format!("config.toml: `{key}` must be true or false")),
        }
    };
    let autonomy_block = switch("autonomy_block", defaults.autonomy_block)?;
    let scope_block = switch("scope_block", defaults.scope_block)?;
    let batch_nudge = switch("batch_nudge", defaults.batch_nudge)?;

    Ok(Limits {
        turn_economy,
        autonomy_block,
        scope_block,
        batch_nudge,
        cell_wall_clock_s,
        response_bytes,
        cells,
        evidence_gate,
        compact_above_percent,
        keep_results,
        instructions_outline,
    })
}

const HOLD_ABOVE_MIN: f64 = 0.5;
const HOLD_ABOVE_MAX: f64 = 1.0;
const SCOUT_ABOVE_MIN: f64 = 0.5;
const SCOUT_ABOVE_MAX: f64 = 1.0;
const COMPLETION_NO_BELOW_MIN: f64 = 0.0;
const COMPLETION_NO_BELOW_MAX: f64 = 0.5;
const COMPLETION_YES_ABOVE_MIN: f64 = 0.5;
const COMPLETION_YES_ABOVE_MAX: f64 = 1.0;
const HYGIENE_NO_BELOW_MIN: f64 = 0.0;
const HYGIENE_NO_BELOW_MAX: f64 = 0.5;
const HYGIENE_YES_ABOVE_MIN: f64 = 0.5;
const HYGIENE_YES_ABOVE_MAX: f64 = 1.0;
const JUDGE_YES_ABOVE_MIN: f64 = 0.5;
const JUDGE_YES_ABOVE_MAX: f64 = 1.0;
const JUDGE_NO_BELOW_MIN: f64 = 0.0;
const JUDGE_NO_BELOW_MAX: f64 = 0.5;
const DRIFT_NO_BELOW_MIN: f64 = 0.0;
const DRIFT_NO_BELOW_MAX: f64 = 0.5;
const SCOUT_RELEVANCE_BELOW_MIN: f64 = 0.0;
const SCOUT_RELEVANCE_BELOW_MAX: f64 = 0.5;
const HELPER_NO_BELOW_MIN: f64 = 0.0;
const HELPER_NO_BELOW_MAX: f64 = 0.5;

const DECIDE_ABOVE_MIN: f64 = 0.5;
const DECIDE_ABOVE_MAX: f64 = 1.0;

/// `[ask]`, refused key by key like every other table: an unknown key is a
/// typo the person meant to have an effect, not a setting to ignore.
fn parse_ask(value: &toml::Value) -> Result<AskConfig, String> {
    let table = table_of(value, "ask")?;
    let defaults = AskConfig::default();

    for key in table.keys() {
        if !["enabled", "jev", "decide_above"].contains(&key.as_str()) {
            return Err(format!("config.toml: unknown key `{key}` in [ask]"));
        }
    }

    let enabled = match table.get("enabled") {
        None => defaults.enabled,
        Some(value) => value
            .as_bool()
            .ok_or_else(|| "config.toml: `[ask] enabled` must be a boolean".to_string())?,
    };
    let jev = match table.get("jev") {
        None => defaults.jev,
        Some(value) => AskJev::parse(
            value
                .as_str()
                .ok_or_else(|| "config.toml: `[ask] jev` must be a string".to_string())?,
        )?,
    };
    let decide_above = match table.get("decide_above") {
        None => defaults.decide_above,
        Some(value) => {
            let number = value
                .as_float()
                .or_else(|| value.as_integer().map(|v| v as f64))
                .ok_or_else(|| "config.toml: `decide_above` must be a number".to_string())?;
            if !(DECIDE_ABOVE_MIN..=DECIDE_ABOVE_MAX).contains(&number) {
                return Err(format!(
                    "config.toml: `decide_above` must be between {DECIDE_ABOVE_MIN} and {DECIDE_ABOVE_MAX}"
                ));
            }
            number
        }
    };

    Ok(AskConfig {
        enabled,
        jev,
        decide_above,
    })
}

fn parse_decisions(value: &toml::Value) -> Result<DecisionsConfig, String> {
    let table = table_of(value, "decisions")?;
    let defaults = DecisionsConfig::default();

    for key in table.keys() {
        if ![
            "model",
            "mode",
            "hold_above",
            "scout_above",
            "completion_no_below",
            "completion_yes_above",
            "hygiene_no_below",
            "hygiene_yes_above",
            "judge_yes_above",
            "judge_no_below",
            "drift_no_below",
            "scout_relevance_below",
            "helper_no_below",
        ]
        .contains(&key.as_str())
        {
            return Err(format!("config.toml: unknown key `{key}` in [decisions]"));
        }
    }

    let model = match table.get("model") {
        None => None,
        Some(value) => {
            let text = value
                .as_str()
                .ok_or_else(|| "config.toml: `[decisions] model` must be a string".to_string())?;
            check_names_no_tool_path_or_grant("model", text)?;
            Some(text.to_string())
        }
    };
    let mode = match table.get("mode") {
        None => defaults.mode,
        Some(value) => DecisionMode::parse(
            value
                .as_str()
                .ok_or_else(|| "config.toml: `mode` must be a string".to_string())?,
        )?,
    };
    let hold_above = match table.get("hold_above") {
        None => defaults.hold_above,
        Some(value) => {
            let number = value
                .as_float()
                .or_else(|| value.as_integer().map(|v| v as f64))
                .ok_or_else(|| "config.toml: `hold_above` must be a number".to_string())?;
            if !(HOLD_ABOVE_MIN..=HOLD_ABOVE_MAX).contains(&number) {
                return Err(format!(
                    "config.toml: `hold_above` must be between {HOLD_ABOVE_MIN} and {HOLD_ABOVE_MAX}"
                ));
            }
            number
        }
    };
    let scout_above = match table.get("scout_above") {
        None => defaults.scout_above,
        Some(value) => {
            let number = value
                .as_float()
                .or_else(|| value.as_integer().map(|v| v as f64))
                .ok_or_else(|| "config.toml: `scout_above` must be a number".to_string())?;
            if !(SCOUT_ABOVE_MIN..=SCOUT_ABOVE_MAX).contains(&number) {
                return Err(format!(
                    "config.toml: `scout_above` must be between {SCOUT_ABOVE_MIN} and {SCOUT_ABOVE_MAX}"
                ));
            }
            number
        }
    };
    let completion_no_below = match table.get("completion_no_below") {
        None => defaults.completion_no_below,
        Some(value) => {
            let number = value
                .as_float()
                .or_else(|| value.as_integer().map(|v| v as f64))
                .ok_or_else(|| "config.toml: `completion_no_below` must be a number".to_string())?;
            if !(COMPLETION_NO_BELOW_MIN..=COMPLETION_NO_BELOW_MAX).contains(&number) {
                return Err(format!(
                    "config.toml: `completion_no_below` must be between {COMPLETION_NO_BELOW_MIN} and {COMPLETION_NO_BELOW_MAX}"
                ));
            }
            number
        }
    };
    let completion_yes_above = match table.get("completion_yes_above") {
        None => defaults.completion_yes_above,
        Some(value) => {
            let number = value
                .as_float()
                .or_else(|| value.as_integer().map(|v| v as f64))
                .ok_or_else(|| {
                    "config.toml: `completion_yes_above` must be a number".to_string()
                })?;
            if !(COMPLETION_YES_ABOVE_MIN..=COMPLETION_YES_ABOVE_MAX).contains(&number) {
                return Err(format!(
                    "config.toml: `completion_yes_above` must be between {COMPLETION_YES_ABOVE_MIN} and {COMPLETION_YES_ABOVE_MAX}"
                ));
            }
            number
        }
    };

    let hygiene_no_below = match table.get("hygiene_no_below") {
        None => defaults.hygiene_no_below,
        Some(value) => {
            let number = value
                .as_float()
                .or_else(|| value.as_integer().map(|v| v as f64))
                .ok_or_else(|| "config.toml: `hygiene_no_below` must be a number".to_string())?;
            if !(HYGIENE_NO_BELOW_MIN..=HYGIENE_NO_BELOW_MAX).contains(&number) {
                return Err(format!(
                    "config.toml: `hygiene_no_below` must be between {HYGIENE_NO_BELOW_MIN} and {HYGIENE_NO_BELOW_MAX}"
                ));
            }
            number
        }
    };
    let hygiene_yes_above = match table.get("hygiene_yes_above") {
        None => defaults.hygiene_yes_above,
        Some(value) => {
            let number = value
                .as_float()
                .or_else(|| value.as_integer().map(|v| v as f64))
                .ok_or_else(|| "config.toml: `hygiene_yes_above` must be a number".to_string())?;
            if !(HYGIENE_YES_ABOVE_MIN..=HYGIENE_YES_ABOVE_MAX).contains(&number) {
                return Err(format!(
                    "config.toml: `hygiene_yes_above` must be between {HYGIENE_YES_ABOVE_MIN} and {HYGIENE_YES_ABOVE_MAX}"
                ));
            }
            number
        }
    };
    let judge_yes_above = match table.get("judge_yes_above") {
        None => defaults.judge_yes_above,
        Some(value) => {
            let number = value
                .as_float()
                .or_else(|| value.as_integer().map(|v| v as f64))
                .ok_or_else(|| "config.toml: `judge_yes_above` must be a number".to_string())?;
            if !(JUDGE_YES_ABOVE_MIN..=JUDGE_YES_ABOVE_MAX).contains(&number) {
                return Err(format!(
                    "config.toml: `judge_yes_above` must be between {JUDGE_YES_ABOVE_MIN} and {JUDGE_YES_ABOVE_MAX}"
                ));
            }
            number
        }
    };
    let judge_no_below = match table.get("judge_no_below") {
        None => defaults.judge_no_below,
        Some(value) => {
            let number = value
                .as_float()
                .or_else(|| value.as_integer().map(|v| v as f64))
                .ok_or_else(|| "config.toml: `judge_no_below` must be a number".to_string())?;
            if !(JUDGE_NO_BELOW_MIN..=JUDGE_NO_BELOW_MAX).contains(&number) {
                return Err(format!(
                    "config.toml: `judge_no_below` must be between {JUDGE_NO_BELOW_MIN} and {JUDGE_NO_BELOW_MAX}"
                ));
            }
            number
        }
    };

    let drift_no_below = match table.get("drift_no_below") {
        None => defaults.drift_no_below,
        Some(value) => {
            let number = value
                .as_float()
                .or_else(|| value.as_integer().map(|v| v as f64))
                .ok_or_else(|| "config.toml: `drift_no_below` must be a number".to_string())?;
            if !(DRIFT_NO_BELOW_MIN..=DRIFT_NO_BELOW_MAX).contains(&number) {
                return Err(format!(
                    "config.toml: `drift_no_below` must be between {DRIFT_NO_BELOW_MIN} and {DRIFT_NO_BELOW_MAX}"
                ));
            }
            number
        }
    };

    let scout_relevance_below = match table.get("scout_relevance_below") {
        None => defaults.scout_relevance_below,
        Some(value) => {
            let number = value
                .as_float()
                .or_else(|| value.as_integer().map(|v| v as f64))
                .ok_or_else(|| {
                    "config.toml: `scout_relevance_below` must be a number".to_string()
                })?;
            if !(SCOUT_RELEVANCE_BELOW_MIN..=SCOUT_RELEVANCE_BELOW_MAX).contains(&number) {
                return Err(format!(
                    "config.toml: `scout_relevance_below` must be between {SCOUT_RELEVANCE_BELOW_MIN} and {SCOUT_RELEVANCE_BELOW_MAX}"
                ));
            }
            number
        }
    };

    let helper_no_below = match table.get("helper_no_below") {
        None => defaults.helper_no_below,
        Some(value) => {
            let number = value
                .as_float()
                .or_else(|| value.as_integer().map(|v| v as f64))
                .ok_or_else(|| "config.toml: `helper_no_below` must be a number".to_string())?;
            if !(HELPER_NO_BELOW_MIN..=HELPER_NO_BELOW_MAX).contains(&number) {
                return Err(format!(
                    "config.toml: `helper_no_below` must be between {HELPER_NO_BELOW_MIN} and {HELPER_NO_BELOW_MAX}"
                ));
            }
            number
        }
    };

    Ok(DecisionsConfig {
        model,
        mode,
        hold_above,
        scout_above,
        completion_no_below,
        completion_yes_above,
        hygiene_no_below,
        hygiene_yes_above,
        judge_yes_above,
        judge_no_below,
        drift_no_below,
        scout_relevance_below,
        helper_no_below,
    })
}

fn parse_helpers(value: &toml::Value) -> Result<HelpersConfig, String> {
    let table = table_of(value, "helpers")?;
    let defaults = HelpersConfig::default();

    for key in table.keys() {
        if ![
            "model",
            "effort",
            "enabled",
            "preflight",
            "preflight_scope",
            "calls_per_cell",
            "completion",
            "completion_check",
            "acceptance_list",
            "reduce_above_tokens",
            "reduce_returns",
            "prefetch_returns",
            "scout_oneshot",
            "learn",
        ]
        .contains(&key.as_str())
        {
            return Err(format!("config.toml: unknown key `{key}` in [helpers]"));
        }
    }

    let model = match table.get("model") {
        None => None,
        Some(value) => {
            let text = value
                .as_str()
                .ok_or_else(|| "config.toml: `model` must be a string".to_string())?;
            validate_concrete_model("[helpers] model", text)?;
            Some(text.to_string())
        }
    };
    let effort = match table.get("effort") {
        None => defaults.effort,
        Some(value) => parse_helper_efforts(value)?,
    };
    let enabled = match table.get("enabled") {
        None => defaults.enabled,
        Some(value) => value
            .as_bool()
            .ok_or_else(|| "config.toml: `enabled` must be true or false".to_string())?,
    };
    let preflight = match table.get("preflight") {
        None => defaults.preflight,
        Some(value) => value
            .as_bool()
            .ok_or_else(|| "config.toml: `preflight` must be true or false".to_string())?,
    };
    let calls_per_cell = match int_field(table, "calls_per_cell")? {
        Some(v) => u32::try_from(CALLS_PER_CELL.check(v)?).expect("range is non-negative"),
        None => defaults.calls_per_cell,
    };

    let completion = match table.get("completion") {
        None => defaults.completion,
        Some(value) => CompletionStyle::parse(
            value
                .as_str()
                .ok_or_else(|| "config.toml: `completion` must be a string".to_string())?,
        )?,
    };
    let preflight_scope = match table.get("preflight_scope") {
        None => defaults.preflight_scope,
        Some(value) => PreflightScope::parse(
            value
                .as_str()
                .ok_or_else(|| "config.toml: `preflight_scope` must be a string".to_string())?,
        )?,
    };
    let acceptance_list = match table.get("acceptance_list") {
        None => defaults.acceptance_list,
        Some(value) => value
            .as_bool()
            .ok_or_else(|| "config.toml: `acceptance_list` must be true or false".to_string())?,
    };
    let completion_check = match table.get("completion_check") {
        None => defaults.completion_check,
        Some(value) => CompletionCheck::parse(value.as_str().ok_or_else(|| {
            "config.toml: `completion_check` must be \"auto\", \"always\" or \"off\"".to_string()
        })?)?,
    };
    let reduce_above_tokens = match int_field(table, "reduce_above_tokens")? {
        Some(v) => usize::try_from(REDUCE_ABOVE_TOKENS.check(v)?).expect("range is non-negative"),
        None => defaults.reduce_above_tokens,
    };

    let reduce_returns = match table.get("reduce_returns") {
        None => defaults.reduce_returns,
        Some(value) => value
            .as_bool()
            .ok_or_else(|| "config.toml: `reduce_returns` must be true or false".to_string())?,
    };

    let prefetch_returns = match table.get("prefetch_returns") {
        None => defaults.prefetch_returns,
        Some(value) => value
            .as_bool()
            .ok_or_else(|| "config.toml: `prefetch_returns` must be true or false".to_string())?,
    };
    let scout_oneshot = match table.get("scout_oneshot") {
        None => defaults.scout_oneshot,
        Some(value) => value
            .as_bool()
            .ok_or_else(|| "config.toml: `scout_oneshot` must be true or false".to_string())?,
    };
    let learn = match table.get("learn") {
        None => defaults.learn,
        Some(value) => value
            .as_bool()
            .ok_or_else(|| "config.toml: `learn` must be true or false".to_string())?,
    };

    Ok(HelpersConfig {
        model,
        effort,
        preflight,
        preflight_scope,
        completion,
        completion_check,
        acceptance_list,
        enabled,
        calls_per_cell,
        reduce_above_tokens,
        reduce_returns,
        prefetch_returns,
        scout_oneshot,
        learn,
        completion_check_set: table.contains_key("completion_check"),
        learn_set: table.contains_key("learn"),
    })
}

fn parse_helper_efforts(value: &toml::Value) -> Result<HelperEfforts, String> {
    let table = table_of(value, "helpers.effort")?;
    for key in table.keys() {
        if !["find", "reduce", "check", "accept", "mend"].contains(&key.as_str()) {
            return Err(format!(
                "config.toml: unknown key `{key}` in [helpers.effort]; only `find`, `reduce`, `check`, `accept` and `mend` are recognised"
            ));
        }
    }
    let defaults = HelperEfforts::default();
    let read = |name: &str, default| match table.get(name) {
        None => Ok(default),
        Some(value) => {
            let word = value.as_str().ok_or_else(|| {
                format!("config.toml: `[helpers.effort] {name}` must be a string")
            })?;
            let effort = crate::wire::Effort::parse(word).ok_or_else(|| {
                format!(
                    "config.toml: `[helpers.effort] {name}` must be low, medium, high, xhigh or max, not `{word}`"
                )
            })?;
            if effort == crate::wire::Effort::Default {
                return Err(format!(
                    "config.toml: `[helpers.effort] {name}` must be a hard value: low, medium, high, xhigh or max"
                ));
            }
            Ok(effort)
        }
    };
    Ok(HelperEfforts {
        find: read("find", defaults.find)?,
        reduce: read("reduce", defaults.reduce)?,
        accept: read("accept", defaults.accept)?,
        check: read("check", defaults.check)?,
        mend: read("mend", defaults.mend)?,
    })
}

/// SECURITY / ISOLATION: `config.toml` can name no tool, a path or a grant --
/// those are the sandbox's (`sandbox-grants.md`) and stay there. A path
/// separator, a glob character, or a registered tool's own name refuses the
/// value with one sentence naming the key.
fn check_names_no_tool_path_or_grant(key: &str, value: &str) -> Result<(), String> {
    // Gateway catalogues legitimately use provider-qualified ids such as
    // `vendor/model-302`. A slash alone therefore cannot mean "path". Path
    // roots and traversal segments can, and no concrete model id needs a
    // backslash, glob, or permission-expression parenthesis.
    let slash_segments: Vec<_> = value.split('/').collect();
    let bytes = value.as_bytes();
    let windows_drive = bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':';
    let looks_like_a_path_or_glob = value.starts_with('/')
        || value.starts_with("~/")
        || windows_drive
        || value.contains('\\')
        || value.contains('*')
        || value.contains('?')
        || value.contains('(')
        || value.contains(')')
        || slash_segments
            .iter()
            .any(|segment| segment.is_empty() || matches!(*segment, "." | ".."));
    let names_a_tool = registry::names().contains(&value);
    if looks_like_a_path_or_glob || names_a_tool {
        return Err(format!("config.toml: `{key}` names no tool, path or grant"));
    }
    Ok(())
}

/// Validates a model used by the parent. Mode words are controls, never
/// concrete request model identifiers.
pub fn validate_parent_model(value: &str) -> Result<(), String> {
    validate_concrete_model("[model] parent", value)
}

fn validate_concrete_model(key: &str, value: &str) -> Result<(), String> {
    if value.is_empty() || value.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Err(format!(
            "config.toml: `{key}` must be one concrete model id"
        ));
    }
    if matches!(value, "auto" | "off" | "inherit") {
        return Err(format!(
            "config.toml: `{key}` must be a concrete model id, not `{value}`"
        ));
    }
    check_names_no_tool_path_or_grant(key, value)
}

#[cfg(test)]
mod completion_threshold_tests {
    use super::*;

    #[test]
    fn the_completion_thresholds_default_without_a_decisions_table() {
        let defaults = DecisionsConfig::default();
        assert_eq!(defaults.completion_no_below, 0.10);
        assert_eq!(defaults.completion_yes_above, 0.90);
        let config = SternaConfig::parse_profile("", None).unwrap();
        assert_eq!(config.decisions.completion_no_below, 0.10);
        assert_eq!(config.decisions.completion_yes_above, 0.90);
    }

    #[test]
    fn the_completion_thresholds_parse_and_are_refused_out_of_range() {
        let config = SternaConfig::parse_profile(
            "[decisions]\nmodel = \"jev-latest\"\ncompletion_no_below = 0.2\ncompletion_yes_above = 0.8\n",
            None,
        )
        .unwrap();
        assert_eq!(config.decisions.completion_no_below, 0.2);
        assert_eq!(config.decisions.completion_yes_above, 0.8);

        let error = SternaConfig::parse_profile("[decisions]\ncompletion_no_below = 0.6\n", None)
            .unwrap_err();
        assert!(error.contains("completion_no_below"), "{error}");

        let error = SternaConfig::parse_profile("[decisions]\ncompletion_yes_above = 0.4\n", None)
            .unwrap_err();
        assert!(error.contains("completion_yes_above"), "{error}");
    }

    #[test]
    fn the_hygiene_and_judge_thresholds_default_and_are_refused_out_of_range() {
        let defaults = DecisionsConfig::default();
        assert_eq!(defaults.hygiene_no_below, 0.10);
        assert_eq!(defaults.hygiene_yes_above, 0.90);
        assert_eq!(defaults.judge_yes_above, 0.90);
        assert_eq!(defaults.judge_no_below, 0.10);

        let config = SternaConfig::parse_profile(
            "[decisions]\nmodel = \"jev-latest\"\nhygiene_no_below = 0.2\nhygiene_yes_above = 0.8\n\
             judge_yes_above = 0.8\njudge_no_below = 0.2\n",
            None,
        )
        .unwrap();
        assert_eq!(config.decisions.hygiene_no_below, 0.2);
        assert_eq!(config.decisions.hygiene_yes_above, 0.8);
        assert_eq!(config.decisions.judge_yes_above, 0.8);
        assert_eq!(config.decisions.judge_no_below, 0.2);

        let error =
            SternaConfig::parse_profile("[decisions]\nhygiene_no_below = 0.6\n", None).unwrap_err();
        assert!(error.contains("hygiene_no_below"), "{error}");
        let error = SternaConfig::parse_profile("[decisions]\nhygiene_yes_above = 0.4\n", None)
            .unwrap_err();
        assert!(error.contains("hygiene_yes_above"), "{error}");
        let error =
            SternaConfig::parse_profile("[decisions]\njudge_yes_above = 0.4\n", None).unwrap_err();
        assert!(error.contains("judge_yes_above"), "{error}");
        let error =
            SternaConfig::parse_profile("[decisions]\njudge_no_below = 0.6\n", None).unwrap_err();
        assert!(error.contains("judge_no_below"), "{error}");
    }

    #[test]
    fn the_drift_threshold_defaults_and_is_refused_out_of_range() {
        let defaults = DecisionsConfig::default();
        assert_eq!(defaults.drift_no_below, 0.10);

        let config = SternaConfig::parse_profile(
            "[decisions]\nmodel = \"jev-latest\"\ndrift_no_below = 0.2\n",
            None,
        )
        .unwrap();
        assert_eq!(config.decisions.drift_no_below, 0.2);

        let error =
            SternaConfig::parse_profile("[decisions]\ndrift_no_below = 0.6\n", None).unwrap_err();
        assert!(error.contains("drift_no_below"), "{error}");
    }

    /// 2644/2645: the Scout's relevance floor and the helper judge's floor
    /// are `[decisions]` keys now, refused out of range like every other
    /// floor in this table.
    #[test]
    fn the_helper_floors_default_and_are_refused_out_of_range() {
        let defaults = DecisionsConfig::default();
        assert_eq!(defaults.scout_relevance_below, 0.10);
        assert_eq!(defaults.helper_no_below, 0.10);

        let config = SternaConfig::parse_profile(
            "[decisions]\nmodel = \"jev-latest\"\nscout_relevance_below = 0.2\n\
             helper_no_below = 0.3\n",
            None,
        )
        .unwrap();
        assert_eq!(config.decisions.scout_relevance_below, 0.2);
        assert_eq!(config.decisions.helper_no_below, 0.3);

        let error = SternaConfig::parse_profile("[decisions]\nscout_relevance_below = 0.6\n", None)
            .unwrap_err();
        assert!(error.contains("scout_relevance_below"), "{error}");
        let error =
            SternaConfig::parse_profile("[decisions]\nhelper_no_below = 0.6\n", None).unwrap_err();
        assert!(error.contains("helper_no_below"), "{error}");
    }
}
