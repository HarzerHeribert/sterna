//! The plan request: one request that reads and writes only its plan.
//!
//! **It only narrows.** [`Narrowing`] is consulted by
//! [`Profile::check_request`](super::profile::Profile::check_request) and
//! [`Profile::admits_command`](super::profile::Profile::admits_command) only
//! after the profile's own never-grantable and `deny` decisions have admitted
//! a call, so everything the profile refuses stays refused and nothing here
//! can grant. `/plan <task>` runs one request this way; the request after it
//! is ordinary again and is handed the plan.

use super::profile::{command_segments, match_segment, resolve_pattern, skip_leading_redirects};
use std::path::Path;

/// What one request may do: everything the session's sandbox allows, or only
/// what a plan needs.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum RequestMode {
    /// The session's sandbox unchanged.
    #[default]
    Work,
    /// Reading tools and read-only shell commands, and one write:
    /// [`PLAN_FILE`]. No change executes.
    Plan,
}

impl RequestMode {
    pub fn name(self) -> &'static str {
        match self {
            Self::Work => "work",
            Self::Plan => "plan",
        }
    }
}

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

/// A compiled plan request, held by a narrowed profile.
#[derive(Debug, Clone)]
pub(crate) struct Narrowing {
    /// The plan file, resolved inside the project root.
    plan: Vec<String>,
}

impl Narrowing {
    /// `None` for [`RequestMode::Work`].
    pub(super) fn compile(mode: RequestMode, root: &Path, home: Option<&Path>) -> Option<Self> {
        match mode {
            RequestMode::Work => None,
            RequestMode::Plan => Some(Self {
                plan: resolve_pattern(root, home, PLAN_FILE),
            }),
        }
    }

    pub(super) fn mode(&self) -> RequestMode {
        RequestMode::Plan
    }

    /// The refusal for a write the profile already admitted, if this request
    /// refuses it. The plan's one entry is a file: equal, never an ancestor
    /// of the candidate, so `plan.md/x` is not the plan.
    pub(super) fn write_refusal(&self, candidate: &[String]) -> Option<String> {
        if self.plan.as_slice() == candidate {
            return None;
        }
        Some("plan: no change executes, so every write but the plan to `.sterna/scratch/plan.md` is refused; send the next message without /plan to make changes".to_string())
    }

    /// The refusal for a command line the profile already admitted, if this
    /// request refuses it. Matched on the words of each segment a shell would
    /// run, never on the line as one string.
    pub(super) fn command_refusal(&self, command_line: &str) -> Option<String> {
        command_reads_only(command_line, &[])
            .map(|why| format!("plan: {why}; the shell is read-only while planning"))
    }
}

/// Why this command line cannot be vouched for as reading only, or `None`
/// when every segment a shell would run reads. A plan request refuses what
/// this names ([`Narrowing::command_refusal`]).
///
/// `extra` is the caller's own list of admitted segment patterns, matched
/// before the built-in [`READ_ONLY_COMMANDS`].
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
