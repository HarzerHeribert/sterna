//! Request modes: a per-request narrowing of the session's compiled profile —
//! map lines 2637 and 2638, ruling *Request modes* in `design-decisions.md`.
//!
//! **A mode only narrows.** [`Narrowing`] is consulted by
//! [`Profile::check_request`](super::profile::Profile::check_request) and
//! [`Profile::admits_command`](super::profile::Profile::admits_command) only
//! after the profile's own never-grantable, `deny` and `allow` decisions have
//! admitted a call, so everything the profile refuses stays refused in every
//! mode and nothing a mode says can grant. `Profile::check`, which the OS
//! appliers probe, never sees a mode.

use super::profile::{
    command_segments, covers, is_rooted, match_segment, resolve_pattern, skip_leading_redirects,
};
use std::path::Path;

/// The mode one request runs in. `Execute` is the session sandbox unchanged.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum RequestMode {
    #[default]
    Execute,
    /// Reading tools and read-only shell commands; writes only under
    /// [`ModeOverlay::writable`].
    Explore,
    /// What `Explore` reads, and one write: [`PLAN_FILE`]. No change executes.
    Plan,
}

impl RequestMode {
    pub fn name(self) -> &'static str {
        match self {
            Self::Execute => "execute",
            Self::Explore => "explore",
            Self::Plan => "plan",
        }
    }

    pub fn next(self) -> Self {
        match self {
            Self::Execute => Self::Explore,
            Self::Explore => Self::Plan,
            Self::Plan => Self::Execute,
        }
    }

    /// `build` is the settings registry's spelling of `execute`, and the
    /// word every screen shows; either is accepted, in any case.
    pub fn parse(word: &str) -> Option<Self> {
        match word.trim().to_ascii_lowercase().as_str() {
            "execute" | "build" => Some(Self::Execute),
            "explore" => Some(Self::Explore),
            "plan" => Some(Self::Plan),
            _ => None,
        }
    }

    /// The word a screen shows: the chip's, the sheet's and every notice's.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Execute => "Build",
            Self::Explore => "Explore",
            Self::Plan => "Plan",
        }
    }

    /// What choosing this mode does, in one sentence.
    #[must_use]
    pub fn sentence(self) -> &'static str {
        match self {
            Self::Execute => "Edits files and runs commands, inside the session's boundary.",
            Self::Explore => "Reads only. It can still write to its own scratch directory.",
            Self::Plan => "Reads, and writes one file: the plan. Nothing else changes.",
        }
    }

    /// The notice every route that sets the mode prints.
    #[must_use]
    pub fn now(self) -> String {
        format!(
            "Mode is now {} · {} Applies from the next request.",
            self.label(),
            self.sentence()
        )
    }

    /// The word the settings file stores.
    #[must_use]
    pub fn setting(self) -> &'static str {
        match self {
            Self::Execute => "build",
            other => other.name(),
        }
    }
}

/// Where `explore` may write before configuration adds to it.
pub const DEFAULT_WRITABLE: [&str; 1] = [".sterna/scratch/**"];

/// The one file `plan` may write, and the one the next request is handed.
pub const PLAN_FILE: &str = ".sterna/scratch/plan.md";

/// The plan a `plan` request left: [`PLAN_FILE`] as a regular file at exactly
/// `<root>/.sterna/scratch/plan.md`, never reached through a link, modified at or
/// after `since`. `None` when the request wrote nothing, so a plan request
/// that wrote no plan hands nothing on and a file a link points at is never
/// read. `since` is taken one second early: a file clock can trail the wall
/// clock, and a plan that old is still this request's.
pub fn written_plan(root: &Path, since: std::time::SystemTime) -> Option<String> {
    let path = root.join(PLAN_FILE);
    let metadata = std::fs::symlink_metadata(&path).ok()?;
    let since = since - std::time::Duration::from_secs(1);
    if !metadata.is_file() || metadata.modified().ok()? < since {
        return None;
    }
    if std::fs::canonicalize(&path).ok()? != std::fs::canonicalize(root).ok()?.join(PLAN_FILE) {
        return None;
    }
    let bytes = std::fs::read(&path).ok()?;
    Some(String::from_utf8_lossy(&bytes).into_owned())
}

/// Commands whose every admitted spelling reads. Arguments that turn one of
/// them into a writer or a launcher are refused by [`refused_argument`].
pub const READ_ONLY_COMMANDS: [&str; 20] = [
    "ls", "cat", "head", "tail", "wc", "grep", "rg", "find", "stat", "file", "git", "du", "df",
    "ps", "env", "which", "pwd", "echo", "date", "uname",
];

/// `rev-parse` joined the list on 2026-09-18: it resolves names and prints
/// them, writes nothing, and a session that cannot run it cannot find out
/// where its own repository is (measured in a real session, which ran
/// `git status --short && git rev-parse --show-toplevel && pwd`).
const GIT_READ_ONLY: [&str; 7] = [
    "status",
    "log",
    "diff",
    "show",
    "blame",
    "ls-files",
    "rev-parse",
];

/// The configurable half of a mode: extra writable globs for `explore` and
/// extra read-only command patterns for both narrowing modes.
#[derive(Debug, Clone)]
pub struct ModeOverlay {
    writable: Vec<String>,
    commands: Vec<String>,
}

impl Default for ModeOverlay {
    fn default() -> Self {
        Self::new(Vec::new(), Vec::new())
    }
}

impl ModeOverlay {
    /// [`DEFAULT_WRITABLE`] followed by `writable`; `commands` are `Bash`-style
    /// segment patterns (`cargo metadata*`) added to [`READ_ONLY_COMMANDS`].
    pub fn new(writable: Vec<String>, commands: Vec<String>) -> Self {
        let mut all: Vec<String> = DEFAULT_WRITABLE.iter().map(|s| s.to_string()).collect();
        all.extend(writable);
        Self {
            writable: all,
            commands,
        }
    }

    pub fn writable(&self) -> &[String] {
        &self.writable
    }
}

/// A compiled mode, held by a narrowed profile.
#[derive(Debug, Clone)]
pub(crate) struct Narrowing {
    mode: RequestMode,
    /// `(as written, resolved glob)`, every one inside the project root.
    writable: Vec<(String, Vec<String>)>,
    commands: Vec<String>,
}

impl Narrowing {
    /// `None` for `Execute`, with a diagnostic per dropped glob. A writable
    /// glob that is absolute or resolves outside the root is dropped: project
    /// isolation is the profile's, and a mode cannot name a path beyond it.
    pub(super) fn compile(
        mode: RequestMode,
        overlay: &ModeOverlay,
        root: &Path,
        home: Option<&Path>,
        root_spelling: &[String],
    ) -> (Option<Self>, Vec<String>) {
        let mut diagnostics = Vec::new();
        let writable = match mode {
            RequestMode::Execute => return (None, diagnostics),
            // The file, not the subtree: `plan` writes its plan and nothing else.
            RequestMode::Plan => vec![(
                PLAN_FILE.to_string(),
                resolve_pattern(root, home, PLAN_FILE),
            )],
            RequestMode::Explore => overlay
                .writable
                .iter()
                .filter_map(|written| {
                    let glob = resolve_pattern(root, home, written);
                    if is_rooted(&written.replace('\\', "/"))
                        || glob.is_empty()
                        || !glob.starts_with(root_spelling)
                    {
                        diagnostics.push(format!(
                            "mode explore: `{written}` is not a project-relative glob inside the root; it makes nothing writable"
                        ));
                        return None;
                    }
                    Some((written.clone(), glob))
                })
                .collect(),
        };
        let narrowing = Self {
            mode,
            writable,
            commands: overlay.commands.clone(),
        };
        (Some(narrowing), diagnostics)
    }

    pub(super) fn mode(&self) -> RequestMode {
        self.mode
    }

    /// The refusal for a write the profile already admitted, if this mode
    /// refuses it.
    pub(super) fn write_refusal(&self, candidate: &[String]) -> Option<String> {
        // `plan`'s one entry is a file: equal, never an ancestor of the
        // candidate, so `plan.md/x` is not the plan.
        if self.writable.iter().any(|(_, glob)| match self.mode {
            RequestMode::Plan => glob.as_slice() == candidate,
            _ => covers(glob, candidate, false),
        }) {
            return None;
        }
        Some(match self.mode {
            RequestMode::Plan => {
                "mode plan: no change executes, so every write but the plan to `.sterna/scratch/plan.md` is refused; `/mode execute` lifts it from the next request".to_string()
            }
            _ => {
                let under: Vec<String> = self
                    .writable
                    .iter()
                    .map(|(written, _)| format!("`{written}`"))
                    .collect();
                let under = if under.is_empty() {
                    "nothing".to_string()
                } else {
                    under.join(", ")
                };
                format!(
                    "mode explore: writes only under {under}; `/mode execute` lifts it from the next request"
                )
            }
        })
    }

    /// The refusal for a command line the profile already admitted, if this
    /// mode refuses it. Matched on the words of each segment a shell would
    /// run, never on the line as one string.
    pub(super) fn command_refusal(&self, command_line: &str) -> Option<String> {
        let mode = self.mode.name();
        command_reads_only(command_line, &self.commands)
            .map(|why| format!("mode {mode}: {why}; the shell is read-only"))
    }
}

/// Why this command line cannot be vouched for as reading only, or `None`
/// when every segment a shell would run reads.
///
/// **Two callers, one reader.** A narrowing mode refuses what this names
/// ([`Narrowing::command_refusal`]); the permission ladder asks the person
/// about it instead ([`crate::permissions::judge_command`]). The two differ
/// in what they do with the answer and not in the answer, so they share the
/// reader rather than keeping two lists that could drift.
///
/// `extra` is the caller's own list of admitted segment patterns
/// (`cargo metadata*`), matched before the built-in [`READ_ONLY_COMMANDS`].
///
/// **On Windows the line is `cmd.exe`'s, and it is screened before it is
/// read** ([`cmd_line_unreadable`]): a construct only `cmd.exe` has is a
/// reason to ask, never a reason to guess.
pub(crate) fn command_reads_only(command_line: &str, extra: &[String]) -> Option<String> {
    if cfg!(windows)
        && let Some(why) = cmd_line_unreadable(command_line)
    {
        return Some(why);
    }
    {
        let refuse = Some;
        for segment in command_segments(command_line) {
            if segment.contains("<(") || segment.contains(">(") {
                return refuse(format!("`{segment}` runs a process substitution"));
            }
            let words: Vec<&str> = segment.split_whitespace().collect();
            let mut plain = Vec::with_capacity(words.len());
            let mut index = 0;
            while index < words.len() {
                match redirect(words[index], words.get(index + 1).copied()) {
                    Redirect::None => {
                        plain.push(words[index]);
                        index += 1;
                    }
                    Redirect::Harmless { operand_follows } => {
                        index += if operand_follows { 2 } else { 1 }
                    }
                    Redirect::Writes => {
                        return refuse(format!("`{segment}` writes through a redirect"));
                    }
                }
            }
            if extra
                .iter()
                .any(|pattern| match_segment(pattern, skip_leading_redirects(&segment), false))
            {
                continue;
            }
            let Some((name, args)) = plain.split_first() else {
                continue;
            };
            let name = *name;
            if name.contains('=') {
                return refuse(format!(
                    "`{segment}` sets a variable in front of the command, which hides it"
                ));
            }
            if name.chars().any(|c| {
                matches!(
                    c,
                    '$' | '`'
                        | '\''
                        | '"'
                        | '\\'
                        | '/'
                        | '*'
                        | '?'
                        | '['
                        | ']'
                        | '{'
                        | '}'
                        | '('
                        | ')'
                )
            }) {
                return refuse(format!("`{name}` does not name a read-only command"));
            }
            if !READ_ONLY_COMMANDS.contains(&name) {
                return refuse(format!("`{name}` is not a read-only command"));
            }
            if let Some(argument) = refused_argument(name, args) {
                return refuse(format!(
                    "`{name} {argument}` can write or run another program"
                ));
            }
        }
        None
    }
}

/// Why a `cmd.exe` command line is not read here at all, or `None` when it
/// holds nothing this scan would misread.
///
/// **`cmd.exe` is not `sh`, so the reader below cannot simply be pointed at
/// its line.** What the two agree about is narrow and it is exactly what is
/// left after this screen: plain words, and the operators `&`, `&&`, `||`,
/// `|` and a newline, all of which [`command_segments`] already splits a
/// command of its own out of. Everything `cmd.exe` spells differently is
/// screened out instead of guessed at — `^` escapes the next character,
/// `%VAR%` and delayed `!VAR!` expand to text nobody here has seen, `(`…`)`
/// groups commands, `<` and `>` redirect by rules that are not the POSIX
/// ones (`NUL`, not `/dev/null`), and `"` quotes by rules this deliberately
/// quote-blind scan does not track. `$` and a backtick are ordinary
/// characters to `cmd.exe` and substitutions to the reader below, which is
/// the same disagreement from the other side.
///
/// The answer is a reason to **ask**, never to refuse and never to run: a
/// reader that cannot place a line has learned that it cannot place it. This
/// is what keeps Windows no more permissive than POSIX — it can only add
/// questions, never remove one.
pub(crate) fn cmd_line_unreadable(command_line: &str) -> Option<String> {
    const UNREADABLE: [(char, &str); 10] = [
        ('^', "escapes the next character"),
        ('%', "expands an environment variable"),
        ('!', "can expand a variable under delayed expansion"),
        ('"', "quotes, and this scan does not track quoting"),
        ('(', "groups commands"),
        (')', "groups commands"),
        ('<', "redirects input"),
        ('>', "redirects output"),
        (
            '$',
            "is a plain character to cmd.exe and a substitution to this scan",
        ),
        (
            '`',
            "is a plain character to cmd.exe and a substitution to this scan",
        ),
    ];
    UNREADABLE
        .iter()
        .find(|(character, _)| command_line.contains(*character))
        .map(|(character, what)| {
            format!("`{character}` {what} on cmd.exe, whose line this check does not parse")
        })
}

enum Redirect {
    None,
    Harmless { operand_follows: bool },
    Writes,
}

/// Whether `word` is an output redirect, and whether it can write a file.
/// Only a descriptor duplicate (`2>&1`) and `/dev/null` are harmless; any
/// other word holding `>` is treated as writing, which is the refusing
/// direction for a word this scan cannot place.
fn redirect(word: &str, next: Option<&str>) -> Redirect {
    if word.contains("<>") {
        return Redirect::Writes;
    }
    if !word.contains('>') {
        return Redirect::None;
    }
    let rest = word
        .strip_prefix('&')
        .unwrap_or_else(|| word.trim_start_matches(|c: char| c.is_ascii_digit()));
    let Some(target) = rest.strip_prefix(">>").or_else(|| rest.strip_prefix('>')) else {
        return Redirect::Writes;
    };
    if target == "/dev/null" {
        return Redirect::Harmless {
            operand_follows: false,
        };
    }
    if !rest.starts_with(">>")
        && let Some(fd) = target.strip_prefix('&')
        && !fd.is_empty()
        && fd.chars().all(|c| c.is_ascii_digit())
    {
        return Redirect::Harmless {
            operand_follows: false,
        };
    }
    if target.is_empty() && next == Some("/dev/null") {
        return Redirect::Harmless {
            operand_follows: true,
        };
    }
    Redirect::Writes
}

/// The argument that makes a read-only command write, run a program, or
/// hide one, if any.
fn refused_argument<'a>(name: &str, args: &[&'a str]) -> Option<&'a str> {
    let first = args.first().copied();
    match name {
        "git" => match first {
            Some(sub) if GIT_READ_ONLY.contains(&sub) => args.iter().copied().find(|arg| {
                arg.starts_with("--output") || arg.starts_with("--ext-diff") || *arg == "--textconv"
            }),
            Some(other) => Some(other),
            None => None,
        },
        "find" => args.iter().copied().find(|arg| {
            matches!(
                *arg,
                "-delete"
                    | "-exec"
                    | "-execdir"
                    | "-ok"
                    | "-okdir"
                    | "-fprint"
                    | "-fprint0"
                    | "-fprintf"
                    | "-fls"
            )
        }),
        "rg" => args.iter().copied().find(|arg| arg.starts_with("--pre")),
        "file" => args
            .iter()
            .copied()
            .find(|arg| *arg == "-C" || *arg == "--compile"),
        "date" => args.iter().copied().find(|arg| {
            *arg == "-s"
                || arg.starts_with("--set")
                || !(arg.starts_with('+') || arg.starts_with('-'))
        }),
        "env" => first,
        _ => None,
    }
}
