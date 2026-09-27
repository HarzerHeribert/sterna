//! Resolves a slash command name to what it *is*, never to its effect: this
//! module runs nothing and spawns nothing. It validates skill documents for
//! discovery, but never interprets their contents as code. It decides, for a given
//! `contract::ProjectConfig` and name, which of a built-in, a project
//! command, or a skill answers, and whether the built-in's subsystem exists
//! yet.

use crate::contract::ProjectConfig;

/// The names sterna offers regardless of the project.
///
/// The first seven are map line 2450's, in the order it lists them. `exit` is
/// Sterna's own eighth: the key handler has always acted on `/exit`, but it was
/// in no list, so the `/` menu never offered it and it read as a command sterna
/// did not have. Being absent from a menu is how a command that works comes
/// to look broken.
pub const BUILT_INS: [BuiltIn; 12] = [
    BuiltIn::Model,
    BuiltIn::Models,
    // `login` was the same defect as `exit` one line down, and outlasted its
    // fix: the variant existed and the key handler answered it, and it was
    // in no list, so the `/` menu never offered a way to connect an account.
    BuiltIn::Login,
    // The first-start wizard: sign in, models per workload, Jev.
    BuiltIn::Setup,
    BuiltIn::Usage,
    BuiltIn::Entitlements,
    BuiltIn::Handles,
    BuiltIn::Supervisor,
    BuiltIn::Rollback,
    BuiltIn::Budget,
    BuiltIn::Memory,
    BuiltIn::Exit,
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BuiltIn {
    Model,
    Models,
    Login,
    /// Sign in, pick a model per workload and set up Jev (`session/setup.rs`).
    Setup,
    /// What each subscription has used of its limits (`session/usage.rs`).
    Usage,
    Entitlements,
    Handles,
    Supervisor,
    Rollback,
    Budget,
    Memory,
    /// End the session. It has always worked; it was not in this list, so the
    /// `/` menu did not offer it and it read as a command sterna did not have.
    Exit,
}

impl BuiltIn {
    /// The command's name, without the leading `/` -- the same shape as a
    /// project command's key, so the two compare directly.
    pub fn name(self) -> &'static str {
        match self {
            BuiltIn::Model => "model",
            BuiltIn::Models => "models",
            BuiltIn::Login => "login",
            BuiltIn::Setup => "wizard",
            BuiltIn::Usage => "usage",
            BuiltIn::Entitlements => "entitlements",
            BuiltIn::Handles => "handles",
            BuiltIn::Supervisor => "supervisor",
            BuiltIn::Rollback => "rollback",
            BuiltIn::Budget => "budget",
            BuiltIn::Memory => "memory",
            BuiltIn::Exit => "exit",
        }
    }
}

/// Where a resolved command's answer came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommandSource {
    BuiltIn(BuiltIn),
    ProjectCommand,
    ProjectSkill,
}

/// Whether a resolved entry executes or is discovery metadata only.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommandStatus {
    Available,
    /// Discoverable project metadata that Sterna can describe but does not
    /// execute. This prevents discovery from looking like acceptance.
    Informational,
}

/// A slash command by name, and what would happen if it were invoked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedCommand {
    pub name: String,
    pub source: CommandSource,
    pub status: CommandStatus,
}

/// Every command sterna offers for `project`: the built-ins, then every
/// project command and skill not shadowed by one.
///
/// **A built-in's name is reserved and always wins a collision.** A
/// project's own commands and skills are the project's own untrusted text,
/// and `/rollback` or `/budget` naming safety-relevant behaviour must not be
/// silently replaced by a same-named file the project happens to ship. Where
/// a project command and a skill share a name that no built-in claims, the
/// command wins: `.claude/commands/<name>.md` is written to be a command,
/// where a skill of the same name is a reusable directory workflow, so the
/// more specific source takes it. Both rules are one
/// precedence order: built-in, then project command, then skill.
pub fn all(project: &ProjectConfig) -> Vec<ResolvedCommand> {
    let mut out: Vec<ResolvedCommand> = BUILT_INS
        .iter()
        .map(|&builtin| resolved_builtin(builtin))
        .collect();

    for name in project.commands.keys() {
        if BUILT_INS.iter().any(|b| b.name() == name) {
            continue;
        }
        out.push(ResolvedCommand {
            name: name.clone(),
            source: CommandSource::ProjectCommand,
            status: CommandStatus::Available,
        });
    }

    for name in project.skills.keys() {
        if BUILT_INS.iter().any(|b| b.name() == name) {
            continue;
        }
        if project.commands.contains_key(name) {
            continue;
        }
        out.push(ResolvedCommand {
            name: name.clone(),
            source: CommandSource::ProjectSkill,
            status: skill_status(project, name),
        });
    }

    out
}

/// Resolves one name against `project`, following the same precedence as
/// [`all`]: built-in, then project command, then skill.
pub fn resolve(project: &ProjectConfig, name: &str) -> Option<ResolvedCommand> {
    if let Some(builtin) = BUILT_INS.iter().find(|b| b.name() == name) {
        return Some(resolved_builtin(*builtin));
    }
    if project.commands.contains_key(name) {
        return Some(ResolvedCommand {
            name: name.to_string(),
            source: CommandSource::ProjectCommand,
            status: CommandStatus::Available,
        });
    }
    if project.skills.contains_key(name) {
        return Some(ResolvedCommand {
            name: name.to_string(),
            source: CommandSource::ProjectSkill,
            status: skill_status(project, name),
        });
    }
    None
}

fn resolved_builtin(builtin: BuiltIn) -> ResolvedCommand {
    ResolvedCommand {
        name: builtin.name().to_string(),
        source: CommandSource::BuiltIn(builtin),
        status: CommandStatus::Available,
    }
}

fn skill_status(project: &ProjectConfig, name: &str) -> CommandStatus {
    if crate::project::workflows::skill_available(project, name) {
        CommandStatus::Available
    } else {
        CommandStatus::Informational
    }
}
