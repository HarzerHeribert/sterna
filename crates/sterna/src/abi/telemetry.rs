//! Pure classifiers behind the interface-origin and recovery-cost telemetry
//! — `smarter-cheaper-roadmap.md`, *Interface-origin telemetry* and
//! *Recovery-cost attribution*.
//!
//! The invariant: **nothing here reads state or changes execution.** Every
//! function maps a record the kernel already produced to a label, so the
//! ledger can count by label without a second source of truth about what
//! ran. Each rule is stated on the function and pinned by
//! `tests/telemetry_taxonomy.rs`.

use crate::abi::lift::{self, Family};
use crate::runtime::outcome::{CallRecord, CellRecord, Ended};

/// Why one operation or frame failed, coarse enough to aggregate across
/// tasks and models.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum FailureKind {
    /// The program could not be run: parse, erasure, naming and protocol
    /// refusals, and an unresolvable name.
    Syntax,
    /// The sandbox or a rule refused the operation.
    Denial,
    /// An edit lost a race with the file it targeted.
    MutationConflict,
    /// A child process was killed by a signal and never exited.
    ProcessSignal,
    /// The runtime's wall clock, heap ceiling or a deadline ended it.
    Timeout,
    /// Cancellation from outside the program — nothing the program did.
    Infrastructure,
    /// A tool or command ran and reported failure.
    Command,
    /// Anything else the program threw.
    Runtime,
}

impl FailureKind {
    /// Every kind, in the order the result document lists them.
    pub const ALL: [FailureKind; 8] = [
        FailureKind::Syntax,
        FailureKind::Denial,
        FailureKind::MutationConflict,
        FailureKind::ProcessSignal,
        FailureKind::Timeout,
        FailureKind::Infrastructure,
        FailureKind::Command,
        FailureKind::Runtime,
    ];

    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Syntax => "syntax",
            Self::Denial => "denial",
            Self::MutationConflict => "mutation_conflict",
            Self::ProcessSignal => "process_signal",
            Self::Timeout => "timeout",
            Self::Infrastructure => "infrastructure",
            Self::Command => "command",
            Self::Runtime => "runtime",
        }
    }
}

/// The kind of one trajectory call's failure, or `None` for a call that
/// ended `Ok`.
///
/// Rules: `Denied` is a denial. `Threw{Cancelled}` is infrastructure. A
/// `ToolError` on `bash` with no exit code is a process signal, on `edit` a
/// mutation conflict, and on anything else a command failure. Any other
/// thrown class is read by [`classify_cell_error`] with an empty message.
#[must_use]
pub fn classify_call(call: &CallRecord) -> Option<FailureKind> {
    match &call.ended {
        Ended::Ok => None,
        Ended::Denied { .. } => Some(FailureKind::Denial),
        Ended::Threw { class } if class == "Cancelled" => Some(FailureKind::Infrastructure),
        Ended::Threw { class } if class == "ToolError" => Some(match call.tool.as_str() {
            "bash" if call.exit_code.is_none() => FailureKind::ProcessSignal,
            "edit" => FailureKind::MutationConflict,
            _ => FailureKind::Command,
        }),
        Ended::Threw { class } => Some(classify_cell_error(class, "")),
    }
}

const SYNTAX_CLASSES: [&str; 8] = [
    "SyntaxError",
    "TypeScriptNotErasable",
    "ReferenceError",
    "ReservedName",
    "ShadowsHostFunction",
    "ProtocolError",
    "CellEditError",
    "UndefinedName",
];

const MUTATION_CONFLICT_MARKERS: [&str; 4] = [
    "source version changed",
    "stale_hash",
    "ambiguous_match",
    "missing_match",
];

/// The kind of a cell's own throw, from its class and message.
///
/// Rules, in order: a class in {SyntaxError, TypeScriptNotErasable,
/// ReferenceError, ReservedName, ShadowsHostFunction, ProtocolError,
/// CellEditError, UndefinedName} is syntax; a message naming a source
/// version change, `stale_hash`, `ambiguous_match` or `missing_match` is a
/// mutation conflict; `RuntimeTerminated`, `RuntimeOutOfMemory` or a message
/// naming the wall clock or a deadline is a timeout; "killed by a signal" is
/// a process signal; `PermissionDenied` is a denial; `Cancelled` is
/// infrastructure; everything else is runtime.
#[must_use]
pub fn classify_cell_error(class: &str, message: &str) -> FailureKind {
    if SYNTAX_CLASSES.contains(&class) {
        return FailureKind::Syntax;
    }
    if MUTATION_CONFLICT_MARKERS
        .iter()
        .any(|marker| message.contains(marker))
    {
        return FailureKind::MutationConflict;
    }
    if matches!(class, "RuntimeTerminated" | "RuntimeOutOfMemory")
        || message.contains("wall clock")
        || message.contains("deadline")
    {
        return FailureKind::Timeout;
    }
    if message.contains("killed by a signal") {
        return FailureKind::ProcessSignal;
    }
    match class {
        "PermissionDenied" => FailureKind::Denial,
        "Cancelled" => FailureKind::Infrastructure,
        _ => FailureKind::Runtime,
    }
}

/// What the parent was doing when it made a request, read from the frame
/// that preceded it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub enum RequestCause {
    /// The first request, and any request after a frame that changed things.
    #[default]
    Implementation,
    /// After a frame that only observed.
    Exploration,
    /// After a frame that ran checks or a verification command.
    Verification,
    /// After a frame that failed.
    Repair,
}

impl RequestCause {
    /// Every cause, in the order the result document lists them.
    pub const ALL: [RequestCause; 4] = [
        RequestCause::Implementation,
        RequestCause::Exploration,
        RequestCause::Verification,
        RequestCause::Repair,
    ];

    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Implementation => "implementation",
            Self::Exploration => "exploration",
            Self::Verification => "verification",
            Self::Repair => "repair",
        }
    }
}

/// Tools that observe and never change anything, by registry name.
const READ_ONLY_TOOLS: [&str; 7] = ["read", "grep", "rg", "glob", "fd", "jq", "context"];

/// Whether one call only observed: a read-only tool, or a `bash` whose
/// checked command classifies as search, read, list or repository state.
fn is_read_only(call: &CallRecord) -> bool {
    if READ_ONLY_TOOLS.contains(&call.tool.as_str()) {
        return true;
    }
    call.tool == "bash"
        && matches!(
            shell_family(call),
            Some(Family::Search | Family::Read | Family::List | Family::RepositoryState)
        )
}

/// Whether one call verified: `checks.run`, or a `bash` whose checked
/// command classifies as verification.
fn is_verification(call: &CallRecord) -> bool {
    call.tool == "checks.run"
        || (call.tool == "bash" && shell_family(call) == Some(Family::Verification))
}

/// The cause of the request that follows `previous`.
///
/// Rules, in order: no previous frame is implementation; `previous_failed`
/// is repair; a frame whose calls all only observed (at least one call) is
/// exploration; a frame with any `checks.run` or verification-family `bash`
/// call is verification; everything else is implementation.
#[must_use]
pub fn request_cause(previous: Option<&CellRecord>, previous_failed: bool) -> RequestCause {
    let Some(previous) = previous else {
        return RequestCause::Implementation;
    };
    if previous_failed {
        return RequestCause::Repair;
    }
    if !previous.calls.is_empty() && previous.calls.iter().all(is_read_only) {
        return RequestCause::Exploration;
    }
    if previous.calls.iter().any(is_verification) {
        return RequestCause::Verification;
    }
    RequestCause::Implementation
}

/// Whether an authored cell did one thing — the shape a direct tool call
/// would have expressed as well.
///
/// A heuristic, and deliberately conservative: exactly one call ran, and the
/// source without comments and blank lines is one statement — one `await`,
/// no `;` before its last non-blank character, and no `if`, `for`, `while`,
/// `try` or `=>`. A false negative costs one uncounted cell; a false
/// positive would misreport a program as a call, so anything doubtful is
/// `false`.
#[must_use]
pub fn is_single_intent_cell(source: &str, calls: &[CallRecord]) -> bool {
    if calls.len() != 1 {
        return false;
    }
    let stripped = strip_comments(source);
    let body = stripped.trim_end_matches(|c: char| c.is_whitespace() || c == ';');
    if body.trim().is_empty() || body.contains(';') || body.contains("=>") {
        return false;
    }
    let mut awaits = 0usize;
    for word in body.split(|c: char| !(c.is_alphanumeric() || c == '_' || c == '$')) {
        match word {
            "await" => awaits += 1,
            "if" | "for" | "while" | "try" => return false,
            _ => {}
        }
    }
    awaits == 1
}

/// The source without `//` and `/* */` comments and without blank lines.
/// String literals are not parsed; a comment marker inside one is a false
/// negative for [`is_single_intent_cell`], never a false positive.
fn strip_comments(source: &str) -> String {
    let mut out = String::with_capacity(source.len());
    let mut rest = source;
    while let Some(start) = rest.find("/*") {
        out.push_str(&rest[..start]);
        match rest[start + 2..].find("*/") {
            Some(end) => rest = &rest[start + 2 + end + 2..],
            None => {
                rest = "";
                break;
            }
        }
    }
    out.push_str(rest);
    out.lines()
        .map(strip_line_comment)
        .filter(|line| !line.trim().is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

/// The line before its first `//` that does not follow a `:`, so a URL in
/// a string is not mistaken for a comment.
fn strip_line_comment(line: &str) -> &str {
    let mut from = 0;
    while let Some(at) = line[from..].find("//") {
        let index = from + at;
        if index == 0 || !line[..index].ends_with(':') {
            return &line[..index];
        }
        from = index + 2;
    }
    line
}

/// The family of a shell-shaped call, for the lifting ledger.
///
/// A `bash` call classifies its checked `command`; a lifted call, whose
/// checked arguments are the capability's, classifies the program word the
/// kernel kept in `lifted_from`. Anything else is not shell-shaped.
#[must_use]
pub fn shell_family(call: &CallRecord) -> Option<Family> {
    if call.tool != "bash" && call.lifted_from.is_none() {
        return None;
    }
    if let Some(command) = call.args.get("command") {
        return lift::classify(command);
    }
    call.lifted_from.as_deref().and_then(lift::classify)
}
