//! Little helpers -- `docs/helpers.md`.
//!
//! A helper is a cheap model given a narrow toolset to answer one question on
//! demand: a secretary, not a delegate. It returns a value and it is gone.
//!
//! **The roster is data.** Adding a helper is a [`HelperSpec`] literal appended
//! to [`HELPERS`]; the guardrails are enforced here rather than by each spec,
//! so a new helper cannot introduce a new failure mode -- it can only choose
//! within a boundary [`validate`] already checks at startup.
//!
//! A helper is not a subagent. A subagent owns a goal and its effects persist;
//! a helper owns a question and leaves nothing behind. That is why
//! [`FORBIDDEN_TOOLS`] exists as a list a spec cannot hold rather than as a
//! sentence a model might ignore.

use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

use crate::contract::{Conversation, Message, Role};
use crate::decide::{self, Answer, Question};
use crate::tools::registry;
use crate::wire;

/// Tools no helper may hold, whatever its spec says.
///
/// A helper answers questions; it does not change the world. This is the
/// mutating third of `registry::ALL`, and [`validate`] refuses a spec naming
/// any of them -- so "a helper never writes" is a list it does not have rather
/// than a rule it might disregard.
pub const FORBIDDEN_TOOLS: [&str; 3] = ["write", "edit", "bash"];

/// What every helper is told on top of its own preamble.
///
/// **A helper is told to be quick; nothing caps it at being quick.** The
/// user's ruling of 2026-09-17: *"A little helper should be instructed to move
/// quick, but also not capped"*, and then *"if a helper returns nonsense that
/// can do more harm than good. So it should run as long as it needs."* A
/// global ceiling used to sit over every spec's own `max_turns`, and the loop
/// stopped a helper on that count **without ever telling it** — so the
/// roster's own preambles asked the model to report having "run out of
/// turns", which it had no way to observe. Both are gone; this replaced them,
/// and its second half is the part that matters: a helper that cannot answer
/// says so rather than inventing one.
pub const HELPER_HASTE: &str = "Answer in one pass if you can, and stop as soon as you have the answer. You are a side errand inside someone else's turn: they are waiting on you, so do not explore beyond the question you were asked.\n\nNothing counts your turns and nothing will cut you off mid-answer, so take the looks the question needs — and if you cannot answer it from what you can reach, say so plainly and name what is missing. Never guess to fill the gap: an answer the caller trusts and acts on is worse than no answer, and \"I could not determine X because Y\" is a useful result.";

/// Lets the ledger tell a helper's request from a task turn before the gateway
/// reads the body -- the same seam `supervisor.rs` uses for its look.
pub const PURPOSE_HEADER: (&str, &str) = ("x-glasshouse-purpose", "helper");

/// What a helper accepts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputKind {
    /// The user's request, for a helper that orients a task.
    Request,
    /// A blob the caller already holds.
    Text,
    /// A handle the caller already holds.
    Handle,
    /// A diff of the turn.
    Diff,
    /// Caller-supplied evidence about a claim, which may include a diff,
    /// current source, a contract, or verification observations.
    Evidence,
}

/// What a helper returns.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputKind {
    /// `file:line` spans -- cheap for the caller to verify.
    Spans,
    /// Shorter text, with the full text still in the caller's hands.
    Reduction,
    /// A judgement plus the evidence for it.
    Verdict,
    /// Lines in fixed forms the caller parses, never trusts.
    Checklist,
}

/// Where a helper may be invoked from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CallSite {
    /// The preflight hook, before the task model's first turn.
    Preflight,
    /// After a tool result.
    PostResult,
    /// Before a completion is accepted.
    CompletionGate,
    /// From inside a cell, by the model itself.
    Cell,
    /// Deriving the acceptance list from the request, before the first turn.
    Acceptance,
    /// A cell that did not parse, before its failure reaches the parent.
    ///
    /// Its own site rather than [`CallSite::PostResult`]: there is no result
    /// to be after, and `reduce.rs` finds the reducer by looking for that
    /// one, so a second helper wearing it would make a `find` ambiguous.
    ParseFailure,
}

/// One helper, entirely as data.
#[derive(Debug, Clone, Copy)]
pub struct HelperSpec {
    /// Called as `helper.<name>(…)`; also the key in the generated declarations.
    pub name: &'static str,
    /// One line, rendered into the declarations the caller sees.
    pub summary: &'static str,
    /// Present participle for the TUI lane: "scanning", "reducing", "checking".
    pub verb: &'static str,
    /// Its fixed instruction preamble -- the only prose a helper carries, and
    /// where the two contracts no toolset can express are stated.
    pub preamble: &'static str,
    /// Tool names, a subset of `registry::ALL`. Empty is legal and is the
    /// safest possible helper.
    pub tools: &'static [&'static str],
    /// Small on purpose: a helper answers, it does not compose.
    pub max_tokens: u32,
    /// How many turns this errand is shaped to take. `1` means one-shot,
    /// which is [`one_shot`]'s dispatch and needs no agent loop at all.
    ///
    /// **It bounds nothing.** Nothing passes it to the loop and nothing stops
    /// a helper on it: a helper is never told a budget, so a spec that asked
    /// the model to report having exhausted one was asking it to observe
    /// something it cannot see. It stays as the roster's own description of
    /// an errand's shape and as the one-shot dispatch.
    pub max_turns: u32,
    pub input: InputKind,
    pub output: OutputKind,
    pub call_sites: &'static [CallSite],
}

/// Find where something lives in this project, as spans the caller can open.
///
/// It holds the four reading tools and nothing else, so the worst it can do
/// is look in the wrong place -- and it looks for as long as the question
/// needs, because a scout that stops mid-search reports a hole as an absence.
pub const SCOUT: HelperSpec = HelperSpec {
    name: "find",
    summary: "Find where something lives in this project. Answers with file:line spans and Sterna appends those exact lines, read from disk: trust them rather than reopening the files. Cheaper than a broad search whose hits you would have to read.",
    verb: "scanning",
    preamble: "Never state a number you did not compute. You have a runtime: count and total in a \
        cell and report what it returned. A computed number is evidence; an estimated one \
        is a conclusion, and conclusions are not yours to draw.\n\
        \n\
        You find where something lives in this project. You can read, glob, grep and \
        fetch context, and you can change nothing. When the project's file listing follows \
        the question, start from it: pick the files whose paths fit, open them, and search \
        only for what the paths do not reveal.\n\
        \n\
        When your input is a scouting brief, answer its sections and nothing else: you are \
        scouting for the model that will act, so never attempt the request it quotes and \
        never report on work you did not do.\n\
        \n\
        Answer with spans only. For each: the path and the lines as `path/to/file.rs:120-148`, \
        the range that holds what was asked for -- the whole function or block, at most 80 lines -- \
        and only lines you actually opened, then one short sentence saying what is there in the \
        file's own words. The caller is shown exactly those lines, read from the file, so a \
        precise range is the whole of your answer's value. Name at most six spans; put the ones \
        that answer the question first. If nothing matches, say that in one line.\n\
        \n\
        Never state a cause. Never propose a fix. Never report anything you did not read \
        — you are returning evidence, and a wrong diagnosis the caller trusts is worse than no \
        diagnosis. Prose is never a substitute for a span: a caller who asked where something is \
        cannot open a paragraph about it. Name what you did not look at — the patterns you did \
        not run and the directories you skipped — as your last line.",
    // `rg` and `fd` rather than `grep` and `glob`: same purity, sharper search,
    // and a Scout's whole job is finding things. `grep` stays for the plain
    // regex case the model may already know.
    tools: &["read", "rg", "fd", "grep", "context"],
    max_tokens: 2048,
    max_turns: 8,
    input: InputKind::Request,
    output: OutputKind::Spans,
    call_sites: &[CallSite::Preflight, CallSite::Cell],
};

/// Dissect an exploring request in one toolless request (2026-09-23): the
/// brief carries the project's file listing and its instruction headings,
/// so the answer can only name paths that exist and takes no search loop.
/// Measured offline: recall of the files the acting model went on to read
/// equal to or better than [`SCOUT`]'s loop where names reveal the files, at
/// about a fifteenth of the tokens and a fifth of the time. Not in
/// [`HELPERS`]: it is never offered to a cell and serves one call site only
/// when `[helpers] scout_oneshot` picks it.
pub const DISSECTOR: HelperSpec = HelperSpec {
    name: "dissect",
    summary: "Dissect a request from the project's file listing, in one answer.",
    verb: "dissecting",
    preamble: "You are scouting for the model that will act. You cannot open files: you \
        have the request, the project's file listing and its instruction headings, and \
        nothing else. Answer the brief's sections and nothing else. Name only paths that \
        appear in the listing, never invent one, and never attempt the request or report \
        on work you did not do.",
    tools: &[],
    max_tokens: 640,
    max_turns: 1,
    input: InputKind::Request,
    output: OutputKind::Spans,
    call_sites: &[CallSite::Preflight],
};

/// Reduce build output, logs and test results to their distinct failures.
///
/// `tools` is empty, so this helper cannot reach the filesystem at all -- the
/// caller passes the text in and still holds it, which is why a `Reduction`
/// needs no handle of its own.
pub const REDUCER: HelperSpec = HelperSpec {
    name: "reduce",
    summary: "Reduce a log or command output to its distinct failures, with file:line where the text names one.",
    verb: "reducing",
    preamble: "You reduce build output, logs and test results to their distinct failures — and \
        you do it by writing a filter, not by retyping what you found.\n\
        \n\
        You are shown a sample of the output, never the whole of it: its size, a histogram of \
        its line shapes with counts, its first and last lines verbatim, and every line the \
        caller requires you to keep. Read the histogram to see what the format is. You have no \
        tools and cannot look at anything else.\n\
        \n\
        Answer with one fence:\n\
        \n\
        ```sterna-filter\n\
        (text) => text.split('\\n').filter(line => /* keep it? */).join('\\n')\n\
        ```\n\
        \n\
        It is a JavaScript function taking the whole output as its only argument and returning \
        the lines to keep, joined by newlines. It runs against the real output, so it must cope \
        with every line shape the histogram showed you.\n\
        \n\
        **Your filter selects lines and never composes them.** Every line it returns must be a \
        line that was in the text, exactly as it stands there — not trimmed, not re-indented, \
        not reworded, not summarised, and not a count you wrote yourself. A returned line that \
        was not in the input is rejected however true it reads. Do not write a marker saying how \
        much you removed: that is counted for you, from the difference.\n\
        \n\
        A warning is not a failure: keep the lines that report an error. Keep every line the \
        caller listed as one that must be kept, and keep what makes each failure readable — the \
        name of what failed, the file and line where the text names one, the assertion or \
        message beneath it. Drop the rest. If there are no failures, keep the lines that say so.\n\
        \n\
        Outside the fence, in at most three short lines, say what a filter cannot: how many \
        distinct failures there are against how many lines carried one, and anything about the \
        shape of the run that a reader of the kept lines alone would miss. Name the shapes you \
        could not account for, and anything the sample showed you only a count of, as your \
        last line — you are reading a sample, so what you did not see is part of what you owe.\n\
        \n\
        Never state a cause. Never propose a fix. Never report anything the text does not say \
        — you are returning evidence, and a wrong diagnosis the caller trusts is worse than no \
        diagnosis. A file and line is never a substitute for a name: a caller who asked which \
        test failed cannot use a line number.",
    tools: &[],
    max_tokens: 1024,
    max_turns: 1,
    input: InputKind::Text,
    output: OutputKind::Reduction,
    call_sites: &[CallSite::PostResult, CallSite::Cell],
};

/// Check a claim against supplied evidence and readable files, and return the
/// evidence with the verdict.
///
/// `read` and `grep` only: it decides whether something holds, and a helper
/// that could also change it would be deciding about its own work.
pub const CHECKER: HelperSpec = HelperSpec {
    name: "check",
    summary: "Check a claim against supplied evidence and current files, and answer with a verdict plus the evidence for it.",
    verb: "checking",
    preamble: "Never state a number you did not compute. You have a runtime: count and total in a \
        cell and report what it returned. A computed number is evidence; an estimated one \
        is a conclusion, and conclusions are not yours to draw.\n\
        \n\
        You check whether a claim holds against the supplied evidence and current readable files. \
        The evidence may contain a diff, current source excerpts, a contract, or verification \
        observations. You can read and grep, and you can change nothing.\n\
        \n\
        Open with the verdict alone on the first line: `holds`, `does not hold`, or `cannot \
        tell`. Current source and its contract can establish a current-state claim without a \
        unified diff. An absent diff or baseline prevents only change-history claims such as \
        whether old tests were preserved. For a composite claim, use `cannot tell` if a material \
        part lacks evidence, but still identify which parts the evidence supports or refutes. \
        Under the verdict, give the evidence and nothing else. Cite source as \
        `path/to/file.rs:120` with the deciding text. Cite a verification observation by its check \
        name, exit code, `executed`, `reused`, `observed_at_ms`, and `reuse_scope`; do not turn it \
        into a file citation. A verdict with no evidence under it is not a verdict.\n\
        \n\
        `executed=true` means that observation came from an execution. `reused=true` means a \
        successful observation originally executed earlier in this request was reused because \
        its declared inputs and captured process environment were unchanged; it is valid for \
        that stated reuse scope but is not a fresh run during checker preparation. Do not demand \
        another run merely because an honest reusable observation was supplied. Do not infer \
        coverage beyond the named command, declared inputs, or stated reuse scope.\n\
        \n\
        Never propose a fix, never write the corrected code, and never report anything the \
        supplied evidence or the files do not say — you are returning evidence, and a wrong \
        verdict the caller trusts is worse than no verdict. Name the material evidence gaps, \
        and the unread files as your last line.",
    tools: &["read", "grep"],
    max_tokens: 2048,
    max_turns: 3,
    input: InputKind::Evidence,
    output: OutputKind::Verdict,
    call_sites: &[CallSite::CompletionGate, CallSite::Cell],
};

/// Turn a request into the acceptance items its completion is checked
/// against (`acceptance.rs`). Toolless and one-shot: it reads the request
/// and answers in fixed line forms, and everything it says is parsed and
/// then decided against the tree, never believed.
pub const ACCEPTANCE: HelperSpec = HelperSpec {
    name: "accept",
    summary: "Turn a request into a checklist of verifiable acceptance items, never a plan and never the work.",
    verb: "listing",
    preamble: crate::acceptance::DERIVE_PREAMBLE,
    tools: &[],
    max_tokens: 512,
    max_turns: 1,
    input: InputKind::Request,
    output: OutputKind::Checklist,
    call_sites: &[CallSite::Acceptance],
};

/// Repair a cell that failed to parse, and change nothing about what it
/// means.
///
/// `tools` is empty and one turn: the whole failed program is handed in and
/// the answer is one fence, so there is nothing to look up and nothing to
/// compose. The host applies the fence, re-parses, and runs the cell -- so
/// unlike every other helper, this one's answer changes what executes, which
/// is why its preamble spends most of its length on the one line it must not
/// cross.
pub const MENDER: HelperSpec = HelperSpec {
    name: "mend",
    summary: "Repair the syntax of a cell that failed to parse, changing nothing about what it means.",
    verb: "mending",
    preamble: "You repair the syntax of a program that failed to parse. You are shown the \
        parser's own error, the line it pointed at, and the whole program. It never ran, so \
        there is nothing to undo and nothing to be careful of except what the program means.\n\
        \n\
        Answer with one fence and nothing else:\n\
        \n\
        ```sterna-edit\n\
        {\"cell\":7,\"replace\":\"exact text occurring once\",\"with\":\"replacement\"}\n\
        ```\n\
        \n\
        `replace` must occur **exactly once** in the program exactly as it stands there — \
        copy it from what you were shown rather than retyping it, and take as much \
        surrounding text as you need to make it unique. `with` is what stands there instead. \
        Nothing else in the program changes.\n\
        \n\
        **Fix the syntax. Change nothing about what the program means.** You are not \
        improving this code and you are not its author: not a clearer name, not a tidier \
        line, not a call you would have written differently, and above all not a statement \
        removed to make the error go away. Deleting code parses beautifully and is the one \
        answer that is always wrong. If the only way you can see to make it parse is to take \
        something out, say so in one line instead of sending a fence.\n\
        \n\
        Most parse failures are punctuation: a quote that was never closed, a brace that was \
        never matched, an escape that meant one thing to the language you were writing and \
        another to the language you were writing it into. That last one is the common case \
        here — a string built in one language and embedded in another carries two levels of \
        escaping, and the fix is almost always to correct the levels rather than to rewrite \
        the text.\n\
        \n\
        Send one fence. If you cannot see the fix, or the only fix you can see would change \
        what the program does, say that in one line and send no fence — the program's author \
        gets it back and is better placed than you are.",
    tools: &[],
    max_tokens: 1024,
    max_turns: 1,
    input: InputKind::Text,
    output: OutputKind::Reduction,
    call_sites: &[CallSite::ParseFailure],
};

/// The roster. **This array is the whole extension point.**
pub const HELPERS: &[HelperSpec] = &[SCOUT, REDUCER, CHECKER, ACCEPTANCE, MENDER];

/// Find a helper by the name the model calls it with.
pub fn lookup(name: &str) -> Option<&'static HelperSpec> {
    HELPERS.iter().find(|spec| spec.name == name)
}

/// Every guardrail a spec could violate, checked once at startup.
///
/// This runs before any helper does, so a malformed roster is a refusal with
/// one sentence rather than a helper that quietly holds `bash`.
pub fn validate() -> Result<(), String> {
    for spec in HELPERS {
        if HELPERS
            .iter()
            .filter(|other| other.name == spec.name)
            .count()
            > 1
        {
            return Err(format!("helper `{}` is declared twice", spec.name));
        }
        check_spec(spec)?;
    }
    Ok(())
}

/// Every guardrail one spec must satisfy.
///
/// Separate from [`validate`] so a test can drive **this** predicate against a
/// rogue spec: `HELPERS` is a `const`, so a bad entry cannot be pushed into it,
/// and a test that re-implemented the check would pass with the check deleted.
pub fn check_spec(spec: &HelperSpec) -> Result<(), String> {
    if spec.name.is_empty() {
        return Err("a helper has no name".to_string());
    }
    if spec.preamble.trim().is_empty() {
        return Err(format!("helper `{}` has no preamble", spec.name));
    }
    if spec.call_sites.is_empty() {
        return Err(format!("helper `{}` can never be invoked", spec.name));
    }
    if spec.max_turns == 0 {
        return Err(format!(
            "helper `{}` asks for no turns at all; a helper that cannot take one turn \
             cannot answer",
            spec.name
        ));
    }
    for tool in spec.tools {
        if FORBIDDEN_TOOLS.contains(tool) {
            return Err(format!(
                "helper `{}` names the mutating tool `{tool}`; a helper answers questions \
                 and never changes the world",
                spec.name
            ));
        }
        if registry::lookup(tool).is_none() {
            return Err(format!(
                "helper `{}` names `{tool}`, which is not a registered tool",
                spec.name
            ));
        }
    }
    Ok(())
}

/// What one helper call produced.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct HelperOutcome {
    /// The helper's answer, or the failure sentence when `ok` is false.
    pub text: String,
    /// Whether the call produced an answer at all.
    ///
    /// A transport error and a refused status are **not** silently an empty
    /// answer: `supervisor.rs` shipped for weeks rendering a permanently
    /// failing look as a healthy one, and this field is why that cannot repeat
    /// here.
    pub ok: bool,
    /// The caller's cancellation token ended this call. Kept distinct from a
    /// provider or protocol failure so the session can consume exactly the
    /// interrupt this helper observed instead of leaking it into a later tool.
    #[serde(default)]
    pub cancelled: bool,
    pub elapsed_ms: u64,
}

/// Provider usage observed for one helper call. Counts and coverage travel
/// with the helper record so task totals can include helpers exactly once and
/// a missing usage row never masquerades as zero tokens.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct HelperUsage {
    /// False only when reading a record written before helper metering
    /// existed. Zero requests in such a record is unknown, not measured zero.
    pub coverage_known: bool,
    pub model: String,
    pub requests: u32,
    /// Successful provider responses received, including responses that
    /// omitted usage. Transport and HTTP failures remain `requests - responses`.
    pub responses: u32,
    pub reported_requests: u32,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_input_tokens: u64,
    pub cache_creation_input_tokens: u64,
    pub cache_read_reported_requests: u32,
    pub cache_creation_reported_requests: u32,
}

impl HelperUsage {
    pub fn known_tokens(&self) -> u64 {
        self.input_tokens
            .saturating_add(self.output_tokens)
            .saturating_add(self.cache_read_input_tokens)
            .saturating_add(self.cache_creation_input_tokens)
    }

    pub fn complete(&self) -> bool {
        self.coverage_known
            && self.reported_requests == self.requests
            && self.cache_read_reported_requests == self.reported_requests
            && self.cache_creation_reported_requests == self.reported_requests
    }

    fn begin_request(&mut self) {
        self.requests = self.requests.saturating_add(1);
    }

    fn record_response(&mut self, usage: Option<crate::wire::Usage>) {
        self.responses = self.responses.saturating_add(1);
        let Some(usage) = usage else {
            return;
        };
        self.reported_requests = self.reported_requests.saturating_add(1);
        self.input_tokens = self.input_tokens.saturating_add(usage.input_tokens);
        self.output_tokens = self.output_tokens.saturating_add(usage.output_tokens);
        if let Some(tokens) = usage.cache_read_input_tokens {
            self.cache_read_input_tokens = self.cache_read_input_tokens.saturating_add(tokens);
            self.cache_read_reported_requests = self.cache_read_reported_requests.saturating_add(1);
        }
        if let Some(tokens) = usage.cache_creation_input_tokens {
            self.cache_creation_input_tokens =
                self.cache_creation_input_tokens.saturating_add(tokens);
            self.cache_creation_reported_requests =
                self.cache_creation_reported_requests.saturating_add(1);
        }
    }
}

/// Shared only with the owned provider worker so a cancellation can retain
/// every earlier completed turn and name the currently unreported request.
#[derive(Debug, Clone)]
pub struct HelperUsageTracker(Arc<Mutex<HelperUsage>>);

impl HelperUsageTracker {
    fn new(model: &str) -> Self {
        Self(Arc::new(Mutex::new(HelperUsage {
            coverage_known: true,
            model: model.to_string(),
            ..HelperUsage::default()
        })))
    }

    pub(crate) fn begin_request(&self) {
        self.0.lock().expect("helper usage lock").begin_request();
    }

    pub(crate) fn record_response(&self, usage: Option<crate::wire::Usage>) {
        self.0
            .lock()
            .expect("helper usage lock")
            .record_response(usage);
    }

    fn snapshot(&self) -> HelperUsage {
        self.0.lock().expect("helper usage lock").clone()
    }
}

impl Default for HelperOutcome {
    /// A record read back from an older rollout that predates this field.
    fn default() -> Self {
        Self {
            text: String::new(),
            ok: false,
            cancelled: false,
            elapsed_ms: 0,
        }
    }
}

impl HelperOutcome {
    fn failed(reason: impl Into<String>, started: Instant) -> Self {
        Self {
            text: reason.into(),
            ok: false,
            cancelled: false,
            elapsed_ms: started.elapsed().as_millis() as u64,
        }
    }

    fn cancelled(started: Instant) -> Self {
        Self {
            text: "the helper was cancelled".into(),
            ok: false,
            cancelled: true,
            elapsed_ms: started.elapsed().as_millis() as u64,
        }
    }
}

/// One completed helper call, as the cell ledger and the TUI both read it.
///
/// Built by whatever invoked the helper, so the lane, the `HELPERS` inspector
/// section and the trajectory all render the same facts rather than three
/// nearly-equal shapes.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct HelperRecord {
    /// The spec's `name`, e.g. `reduce`. Owned because a `CellView` is
    /// persisted to the rollout and read back on `--resume`.
    pub helper: String,
    /// The spec's `verb`, for the lane while it runs.
    pub verb: String,
    /// A short, already-bounded description of what it was asked, for the
    /// lane and the inspector. Never the payload itself.
    pub asked: String,
    /// What came back.
    pub outcome: HelperOutcome,
    /// Turns actually taken; `1` for a one-shot helper.
    pub turns: u32,
    /// What the helper actually reached for, tool names in order.
    ///
    /// A helper reports numbers it says it computed. Without this the claim
    /// cannot be checked: the caller sees an answer and no trace of the work.
    /// Host preparation operations and omissions are prefixed `prepare`;
    /// a toolless helper has no subsequent model-driven tool operations.
    #[serde(default)]
    pub looked: Vec<String>,
    /// The helper model and every provider-reported token class, including
    /// per-class request coverage when a provider omitted usage.
    #[serde(default)]
    pub usage: HelperUsage,
}

impl HelperRecord {
    /// The lane's one-line summary once the call has resolved.
    pub fn lane_result(&self) -> String {
        if self.outcome.ok {
            format!("{} · {}ms", self.asked, self.outcome.elapsed_ms)
        } else {
            format!("{} · {}", self.outcome.text, self.asked)
        }
    }
}

/// What one helper call produced, and what it actually cost in turns.
///
/// The invariant: **`turns` is what the call took, never what it was
/// allowed.** A [`HelperRecord`] built from `spec.max_turns` renders a scout
/// that answered on its first turn as one that burned eight, and
/// `little-helpers.md`'s inspector section exists precisely so a bad call is
/// visible rather than hidden behind an `OK`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HelperCall {
    pub outcome: HelperOutcome,
    pub turns: u32,
    /// Tool names the loop reached for, in order; empty for `run_once`.
    pub looked: Vec<String>,
    pub usage: HelperUsage,
}

/// The two provider controls selected for one helper invocation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HelperRoute<'a> {
    pub model: &'a str,
    pub effort: wire::Effort,
    /// This call's ceiling on the whole response, when the work itself
    /// decides it rather than the roster. `None` means [`HelperSpec::max_tokens`].
    ///
    /// Only the reducer sets it, and [`reduction_cap`] says why: the answer a
    /// reduction owes is a fraction of what it is reducing, and a constant is
    /// the wrong shape at both ends of the range.
    pub cap: Option<u32>,
}

impl<'a> HelperRoute<'a> {
    /// A route that takes the spec's own declared cap.
    #[must_use]
    pub fn new(model: &'a str, effort: wire::Effort) -> Self {
        Self {
            model,
            effort,
            cap: None,
        }
    }

    /// The ceiling this call will actually carry.
    #[must_use]
    pub fn cap_for(&self, spec: &HelperSpec) -> u32 {
        self.cap.unwrap_or(spec.max_tokens)
    }
}

/// How much answer a reduction of `input_tokens` is allowed.
///
/// **The invariant is not "at most 1,024" -- it is that a reduction stays a
/// fraction of what it replaces.** A constant expressed that cheaply and got
/// both ends wrong: a dense failure log could not be represented at all, and
/// paid for a request that returned a refusal.
///
/// Measured on real `cargo test` failures, 2026-09-19, against the text a
/// faithful reduction has to carry -- the result line plus every distinct
/// failure with its assertion:
///
/// | log | input | irreducible core | core / input |
/// |---|---|---|---|
/// | 100 terse failures | ~6,644 tok | ~1,505 tok | 0.23 |
/// | 30 verbose failures | ~3,722 tok | ~455 tok | 0.12 |
///
/// [`REDUCTION_DIVISOR`] is 4 because the worst measured shape needs 0.23 of
/// its own size and a divisor of 6 does not fit it. The ceiling is where
/// summarising stops being the answer: above it the honest reduction is
/// partial, the provenance line says so, and the remedy is to narrow the
/// command. The floor is a safety net that the `reduce_above_tokens`
/// threshold means is rarely reached.
#[must_use]
pub fn reduction_cap(input_tokens: usize) -> u32 {
    let scaled = u32::try_from(input_tokens / REDUCTION_DIVISOR as usize).unwrap_or(u32::MAX);
    scaled.clamp(REDUCTION_FLOOR, REDUCTION_CEILING)
}

/// The fraction of its input a reduction may spend on its answer.
pub const REDUCTION_DIVISOR: u32 = 4;
/// The smallest answer a reduction is ever given.
pub const REDUCTION_FLOOR: u32 = 512;
/// The largest, past which a reduction is honestly partial rather than longer.
pub const REDUCTION_CEILING: u32 = 4096;

/// Run any helper in the roster: the one entry point a caller uses.
///
/// Dispatches on the spec rather than on its name, so a new `HelperSpec`
/// needs no call-site edit. A toolless one-turn spec is [`run_once`]; a spec
/// holding tools or asking for more than one turn needs the agent loop and
/// goes to [`run_with_tools`].
pub fn run(
    spec: &HelperSpec,
    route: HelperRoute<'_>,
    input: &str,
    profile: &crate::sandbox::profile::Profile,
    session: &crate::contract::SessionId,
    token: &crate::tools::invoke::CancellationToken,
) -> HelperCall {
    let started = Instant::now();
    let prepared = crate::helper_context::HelperRole::from_helper_name(spec.name)
        .map(|role| crate::helper_context::prepare(role, input, profile, token));
    // The appended original is bounded by the **role's** bound, and a Reducer
    // has none: truncating the log it was called on would make it answer about
    // the part that survived. A question is bounded, and when it is, both ends
    // are kept rather than a prefix.
    let role = crate::helper_context::HelperRole::from_helper_name(spec.name);
    let request = prepared.as_ref().map(|packet| {
        let carried = match role.and_then(crate::helper_context::payload_bound) {
            // A bounded role's input is a question, and a question is cut at
            // both ends rather than compressed.
            Some(bound) => crate::helper_context::bounded_payload(input, bound),
            // An unbounded role's input is the work, and it arrives whole in
            // meaning: runs of structurally identical lines collapse to one
            // shape and an exact count, so nothing distinct is lost and the
            // bulk that was only repetition never has to be paid for. Six
            // thousand identical probe lines are one observation, not six
            // thousand.
            None => {
                let collapsed = crate::helper_context::collapse_runs(input);
                if collapsed.lines_out < collapsed.lines_in {
                    format!(
                        "{}\n[{} lines of repetition collapsed to {}; every distinct line is \
                         present and each run states its exact count]",
                        collapsed.text, collapsed.lines_in, collapsed.lines_out
                    )
                } else {
                    collapsed.text
                }
            }
        };
        format!("{}\n\nOriginal helper request:\n{carried}", packet.rendered)
    });
    let mut call = run_unprepared(
        spec,
        route,
        request.as_deref().unwrap_or(input),
        profile,
        session,
        token,
    );
    if let Some(packet) = prepared {
        let mut operations: Vec<String> = packet
            .operations
            .into_iter()
            .map(|operation| format!("prepare: {} {}", operation.action, operation.subject))
            .collect();
        operations.extend(packet.omissions.into_iter().map(|omission| {
            format!(
                "prepare omitted: {} ({})",
                omission.subject, omission.reason
            )
        }));
        operations.append(&mut call.looked);
        call.looked = operations;
    }
    call.outcome.elapsed_ms = started.elapsed().as_millis() as u64;
    call
}

fn run_unprepared(
    spec: &HelperSpec,
    route: HelperRoute<'_>,
    input: &str,
    profile: &crate::sandbox::profile::Profile,
    session: &crate::contract::SessionId,
    token: &crate::tools::invoke::CancellationToken,
) -> HelperCall {
    let started = Instant::now();
    if token.is_cancelled() {
        return HelperCall {
            outcome: HelperOutcome::cancelled(started),
            turns: 0,
            looked: Vec::new(),
            usage: HelperUsage {
                coverage_known: true,
                model: route.model.to_string(),
                ..HelperUsage::default()
            },
        };
    }
    if one_shot(spec) {
        let cap = route.cap_for(spec);
        let spec = *spec;
        let model = route.model.to_string();
        let effort = route.effort;
        let input = input.to_string();
        let usage = HelperUsageTracker::new(&model);
        usage.begin_request();
        let worker_usage = usage.clone();
        match wait_for_helper(token, move || {
            run_once_metered(&spec, &model, effort, cap, &input, &worker_usage)
        }) {
            HelperWait::Returned(call) => call,
            HelperWait::Cancelled => HelperCall {
                outcome: HelperOutcome::cancelled(started),
                turns: 0,
                looked: Vec::new(),
                usage: usage.snapshot(),
            },
            HelperWait::Panicked => HelperCall {
                outcome: HelperOutcome::failed(
                    format!("`{}` panicked while running", spec.name),
                    started,
                ),
                turns: 0,
                looked: Vec::new(),
                usage: usage.snapshot(),
            },
        }
    } else {
        run_with_tools(spec, route, input, profile, session, token)
    }
}

/// How often a blocked helper checks whether its caller cancelled it.
const HELPER_CANCEL_POLL: Duration = Duration::from_millis(20);

enum HelperWait<T> {
    Returned(T),
    Cancelled,
    Panicked,
}

/// Run an owned helper operation away from the caller's V8 isolate while the
/// caller remains able to observe cancellation. A cancelled provider request
/// may finish on this worker, but it owns every value it retained and remains
/// bounded by the wire timeout; its result is discarded.
fn wait_for_helper<T, F>(
    token: &crate::tools::invoke::CancellationToken,
    operation: F,
) -> HelperWait<T>
where
    T: Send + 'static,
    F: FnOnce() -> T + Send + 'static,
{
    let (sender, receiver) = mpsc::sync_channel(1);
    std::thread::spawn(move || {
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(operation));
        let _ = sender.send(result);
    });
    loop {
        if token.is_cancelled() {
            return HelperWait::Cancelled;
        }
        match receiver.recv_timeout(HELPER_CANCEL_POLL) {
            Ok(Ok(value)) => return HelperWait::Returned(value),
            Ok(Err(_)) | Err(mpsc::RecvTimeoutError::Disconnected) => {
                return HelperWait::Panicked;
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
        }
    }
}

/// Whether one wire call serves this spec: nothing to call a tool with, and
/// one turn to do it in.
fn one_shot(spec: &HelperSpec) -> bool {
    spec.tools.is_empty() && spec.max_turns == 1
}

/// Run a one-turn, toolless helper: the spec's preamble as the system block,
/// the caller's input as the one user message, the purpose header set.
///
/// A helper holding tools or asking for more than one turn needs the agent
/// loop and is not this function's job; [`validate`] permits such a spec and
/// callers dispatch on `max_turns`.
pub fn run_once(spec: &HelperSpec, route: HelperRoute<'_>, input: &str) -> HelperOutcome {
    let usage = HelperUsageTracker::new(route.model);
    usage.begin_request();
    let cap = route.cap_for(spec);
    run_once_metered(spec, route.model, route.effort, cap, input, &usage).outcome
}

fn run_once_metered(
    spec: &HelperSpec,
    model: &str,
    effort: wire::Effort,
    cap: u32,
    input: &str,
    usage: &HelperUsageTracker,
) -> HelperCall {
    let started = Instant::now();
    debug_assert!(spec.tools.is_empty() && spec.max_turns == 1);

    let conversation = Conversation {
        system: spec.preamble.to_string(),
        messages: vec![Message::text(Role::User, input)],
    };
    // Streamed, so the ceiling on this call can measure silence rather than
    // duration: a reasoning helper that is working keeps the socket busy with
    // `ping` and `thinking` events, and only a dead one goes quiet
    // (`wire::SIDE_ERRAND_SILENCE`).
    let outcome = match wire::send_errand_streaming(
        &conversation,
        model,
        effort,
        cap,
        Some(PURPOSE_HEADER),
    ) {
        Ok(turn) => {
            usage.record_response(turn.usage);
            let text: String = turn
                .message
                .content
                .iter()
                .map(crate::contract::Block::text)
                .collect::<Vec<_>>()
                .join("");
            if text.trim().is_empty() {
                HelperOutcome::failed("the helper returned nothing", started)
            } else {
                HelperOutcome {
                    text,
                    ok: true,
                    cancelled: false,
                    elapsed_ms: started.elapsed().as_millis() as u64,
                }
            }
        }
        // The error already says the request failed.
        Err(err) => HelperOutcome::failed(err.to_string(), started),
    };
    HelperCall {
        outcome,
        turns: 1,
        looked: Vec::new(),
        usage: usage.snapshot(),
    }
}

/// Run a helper that holds tools, through the subagent loop with its toolset
/// narrowed to `spec.tools`, its instructions replaced by the preamble, and
/// its model set to the tier.
///
/// The invariant: **only a returned answer is an answer.** The loop can also
/// end out of turns, cancelled or failed, and every one of those is `ok:
/// false` with the reason in the text -- a helper that stopped without an
/// answer must not read as a healthy short one. This is the invariant that
/// preserves the signal now that nothing counts a helper's turns: only a
/// `returned` status with non-empty text is an answer.
pub fn run_with_tools(
    spec: &HelperSpec,
    route: HelperRoute<'_>,
    input: &str,
    profile: &crate::sandbox::profile::Profile,
    session: &crate::contract::SessionId,
    token: &crate::tools::invoke::CancellationToken,
) -> HelperCall {
    let started = Instant::now();
    let options = crate::agent::AgentOptions {
        // **Nothing terminates a helper on a count.** The user, 2026-09-17:
        // *"how does it know if it runs out of turns? And if a helper returns
        // nonsense that can do more harm than good. So it should run as long
        // as it needs."* What bounds this call is real: every provider
        // request on this narrowed path carries `wire::SIDE_ERRAND_TIMEOUT`
        // (a one-shot errand streams instead, and ends on silence), and the
        // caller's cancellation token — the person's `/stop` — ends the
        // wait. See
        // `spec.max_turns`, which is now a declaration and not a limit.
        turns: None,
        model: route.model.to_string(),
        effort: route.effort,
        deadline: None,
    };
    let narrowed = crate::agent::Narrowed {
        tools: spec.tools,
        instructions: spec.preamble,
    };
    let usage = HelperUsageTracker::new(route.model);
    let worker_usage = usage.clone();
    // **On an owned thread, always.** `run_narrowed` builds a Runtime, which is
    // a second V8 isolate, and this function is reached from a host callback
    // while the caller's isolate is borrowed. Owning every input also lets the
    // caller stop waiting when cancelled; the provider request may finish on
    // this thread under its hard ceiling, but it retains no session borrow and
    // the post-response token check starts no late tool.
    let profile = profile.clone();
    let session = session.clone();
    let input = input.to_string();
    let worker_token = token.clone();
    let result = match wait_for_helper(token, move || {
        crate::agent::run_narrowed_metered(
            &profile,
            &session,
            &input,
            &options,
            &worker_token,
            crate::agent::NarrowedRun::helper(&narrowed, &worker_usage),
        )
    }) {
        HelperWait::Returned(result) => result,
        HelperWait::Cancelled => {
            return HelperCall {
                outcome: HelperOutcome::cancelled(started),
                turns: 0,
                looked: Vec::new(),
                usage: usage.snapshot(),
            };
        }
        HelperWait::Panicked => {
            return HelperCall {
                outcome: HelperOutcome::failed(
                    format!("`{}` panicked while running", spec.name),
                    started,
                ),
                turns: 0,
                looked: Vec::new(),
                usage: usage.snapshot(),
            };
        }
    };

    let answer = result.answer.trim();
    let outcome = if result.status != "returned" {
        HelperOutcome::failed(
            format!(
                "the call ended `{}`: {}",
                result.status,
                if answer.is_empty() {
                    "no answer"
                } else {
                    answer
                }
            ),
            started,
        )
    } else if answer.is_empty() {
        HelperOutcome::failed("the helper returned nothing", started)
    } else {
        HelperOutcome {
            text: result.answer.clone(),
            ok: true,
            cancelled: false,
            elapsed_ms: started.elapsed().as_millis() as u64,
        }
    };
    HelperCall {
        outcome,
        turns: u32::try_from(result.turns).unwrap_or(u32::MAX),
        looked: result.trajectory,
        usage: usage.snapshot(),
    }
}

// ---------------------------------------------------------------------
// The three call sites that are not a cell.
//
// Each is a thin entry point rather than a second runtime: they all reach
// `run`, so a helper invoked automatically is the same helper the model can
// call, under the same guardrails.
// ---------------------------------------------------------------------

/// SCOUT at `CallSite::Preflight` -- once per task, after the request arrives
/// and before the model's first turn.
///
/// `input` is the scouting brief `crate::preflight::scouting_brief` built,
/// never the raw request: the brief is what keeps the scout from attempting
/// the task, and the record's `asked` is the request quoted inside it.
///
/// Returns the record, or `None` when no spec serves this site. **A failed
/// preflight is never fatal**: the task runs with the static orientation,
/// exactly as it does today.
pub fn preflight(
    input: &str,
    route: HelperRoute<'_>,
    profile: &crate::sandbox::profile::Profile,
    session: &crate::contract::SessionId,
    token: &crate::tools::invoke::CancellationToken,
    mut progress: impl FnMut(&HelperRecord),
) -> Option<HelperRecord> {
    let spec = HELPERS
        .iter()
        .find(|spec| spec.call_sites.contains(&CallSite::Preflight))?;
    let mut record = HelperRecord {
        helper: spec.name.to_string(),
        verb: spec.verb.to_string(),
        asked: bounded_ask(crate::preflight::request_in(input).unwrap_or(input)),
        ..HelperRecord::default()
    };
    progress(&record);
    let call = run(spec, route, input, profile, session, token);
    record.outcome = call.outcome;
    record.turns = call.turns;
    record.looked = call.looked;
    record.usage = call.usage;
    progress(&record);
    Some(record)
}

/// ACCEPTANCE at `CallSite::Acceptance` -- once per task, before the first
/// turn: the request in, the acceptance lines out.
pub fn acceptance_list(
    request: &str,
    route: HelperRoute<'_>,
    profile: &crate::sandbox::profile::Profile,
    session: &crate::contract::SessionId,
    token: &crate::tools::invoke::CancellationToken,
) -> Option<HelperRecord> {
    let spec = HELPERS
        .iter()
        .find(|spec| spec.call_sites.contains(&CallSite::Acceptance))?;
    let call = run(spec, route, request, profile, session, token);
    Some(HelperRecord {
        helper: spec.name.to_string(),
        verb: spec.verb.to_string(),
        asked: bounded_ask(request),
        outcome: call.outcome,
        turns: call.turns,
        looked: call.looked,
        usage: call.usage,
    })
}

/// CHECKER at `CallSite::CompletionGate` -- before a completion is accepted
/// -- plus the result judge (2645) on the checker's own return: the same
/// one `noul` the preflight Scout's own result is judged with, never
/// withholding, truncating or rerunning the checker's findings. `judge:
/// None` is byte-identical to this call with no judge at all. The second
/// element of the pair is the judge's own `(noul, latency_ms)`, for a
/// caller's telemetry -- `None` under [`judge_outcome`]'s own conditions.
pub fn check_completion_judged(
    evidence: &str,
    route: HelperRoute<'_>,
    context: HelperContext<'_>,
    judge: Option<HelperJudge<'_>>,
) -> Option<(HelperRecord, Option<(f64, u64)>)> {
    let spec = HELPERS
        .iter()
        .find(|spec| spec.call_sites.contains(&CallSite::CompletionGate))?;
    let (call, judged) = run_judged(spec, route, evidence, context, judge);
    Some((
        HelperRecord {
            helper: spec.name.to_string(),
            verb: spec.verb.to_string(),
            asked: bounded_ask(evidence),
            outcome: call.outcome,
            turns: call.turns,
            looked: call.looked,
            usage: call.usage,
        },
        judged,
    ))
}

/// The recap `[helpers] completion = "recap"` asks for: one or two sentences
/// on what the session did, and one suggested next prompt.
///
/// A one-shot toolless call by construction -- it summarises what already
/// happened and must not go looking for more.
pub const RECAP_PREAMBLE: &str = "You close out a coding session. In at most two sentences, say what was actually done — \
     from the transcript only, never inferred. Then, on a new line beginning `Next:`, suggest \
     one specific next prompt the user could send. Never invent work that did not happen, and \
     never claim something succeeded that the transcript does not show succeeding.";

/// One short description of an input, bounded, never the payload itself.
pub(crate) fn bounded_ask(input: &str) -> String {
    let line = input.lines().next().unwrap_or("").trim();
    if line.chars().count() <= 60 {
        line.to_string()
    } else {
        format!("{}…", line.chars().take(59).collect::<String>())
    }
}

// ---------------------------------------------------------------------
// Ranking a Scout's candidates and judging a helper's own result
// (map 2644, 2645; `docs/decisions.md`).
//
// Both ask the decision model one bounded question and never withhold or
// rerun a helper's own call: ranking orders and floors what the Scout is
// served before it starts, and judging appends one line to a returned
// record after it finishes. A failed, slow or absent decision leaves
// either exactly as it is today.
// ---------------------------------------------------------------------

/// How many lines of a candidate's head the ranking question sees.
const RANK_HEAD_LINES: usize = 40;

/// The most one ranking request's `state` may hold. Heads are dropped from
/// the end first, then whole candidates, until the state fits -- never a
/// question sent about a file `state` does not carry.
const RANK_STATE_BYTES: usize = 64 * 1024;

/// How many of a scout's ranked, floor-passing candidates the brief names.
const MAX_RANKED_SCOUT_FILES: usize = 24;

/// One file offered to the ranking question before the Scout's own request
/// is built. `head` is what the question sees -- never the whole file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScoutCandidate {
    pub path: String,
    pub head: String,
}

/// The decision model and floor for one ranking request.
#[derive(Debug, Clone, Copy)]
pub struct ScoutRankRoute<'a> {
    pub model: &'a str,
    pub floor: f64,
    /// `mode = on`: the ranking changes what the Scout is served.
    /// `mode = shadow`: it is only asked and counted -- today's order runs.
    pub apply: bool,
}

/// What ranking one Scout call's candidates produced.
#[derive(Debug, Clone, PartialEq)]
pub struct ScoutRanking {
    /// Every candidate the ranking question actually answered.
    pub ranked: u32,
    /// The paths that cleared the floor, in descending relevance, capped at
    /// [`MAX_RANKED_SCOUT_FILES`].
    pub kept: Vec<(String, f64)>,
    /// Candidates the floor excluded.
    pub skipped: u32,
    /// Candidates dropped before the request was even sent, to keep `state`
    /// under [`RANK_STATE_BYTES`].
    pub dropped: u32,
    /// Candidates that cleared the floor but sat past
    /// [`MAX_RANKED_SCOUT_FILES`], so the brief never names them.
    ///
    /// **A bound that fires is counted where the Scout reads.** Without this
    /// the brief lists twenty-four files and reads as though those were all
    /// that qualified, and the Scout stops looking.
    pub past_cap: u32,
    pub latency_ms: u64,
}

impl ScoutRanking {
    /// The Scouting record's own line: `ranked N, skipped M, top: a.rs 0.94,
    /// b.rs 0.81` (`little-helpers.md`).
    pub fn note(&self) -> String {
        let mut line = format!("ranked {}, skipped {}", self.ranked, self.skipped);
        if self.dropped > 0 {
            line.push_str(&format!(", {} not sent (request bound)", self.dropped));
        }
        if self.past_cap > 0 {
            line.push_str(&format!(", {} past the brief's limit", self.past_cap));
        }
        if !self.kept.is_empty() {
            let top: Vec<String> = self
                .kept
                .iter()
                .take(3)
                .map(|(path, score)| format!("{path} {score:.2}"))
                .collect();
            line.push_str(&format!(", top: {}", top.join(", ")));
        }
        line
    }
}

/// The first [`RANK_HEAD_LINES`] lines of `text`.
fn bounded_head(text: &str) -> String {
    text.lines()
        .take(RANK_HEAD_LINES)
        .collect::<Vec<_>>()
        .join("\n")
}

/// Builds the ranking request's `state`, dropping heads and then whole
/// candidates from the end until it fits [`RANK_STATE_BYTES`]. Returns the
/// state, the original indices it still names, and how many candidates were
/// dropped entirely.
fn bounded_candidate_state(
    request: &str,
    candidates: &[ScoutCandidate],
) -> (serde_json::Value, Vec<usize>, u32) {
    let mut heads: Vec<String> = candidates
        .iter()
        .map(|candidate| bounded_head(&candidate.head))
        .collect();
    let mut included = vec![true; candidates.len()];
    let mut dropped = 0u32;

    let build = |heads: &[String], included: &[bool]| -> serde_json::Value {
        let files: Vec<serde_json::Value> = candidates
            .iter()
            .zip(heads)
            .zip(included)
            .filter(|&(_, &keep)| keep)
            .map(|((candidate, head), _)| serde_json::json!({"path": candidate.path, "head": head}))
            .collect();
        serde_json::json!({"request": request, "files": files})
    };
    let size = |value: &serde_json::Value| {
        serde_json::to_vec(value)
            .map(|bytes| bytes.len())
            .unwrap_or(usize::MAX)
    };

    let mut index = candidates.len();
    while size(&build(&heads, &included)) > RANK_STATE_BYTES && index > 0 {
        index -= 1;
        heads[index].clear();
    }
    let mut index = candidates.len();
    while size(&build(&heads, &included)) > RANK_STATE_BYTES && index > 0 {
        index -= 1;
        if included[index] {
            included[index] = false;
            dropped += 1;
        }
    }
    let value = build(&heads, &included);
    let kept_indices: Vec<usize> = included
        .iter()
        .enumerate()
        .filter(|&(_, &keep)| keep)
        .map(|(i, _)| i)
        .collect();
    (value, kept_indices, dropped)
}

/// Asks one `noul` per candidate in one request, keyed by its index in
/// `candidates`, and answers with the ranking -- or `None` on any transport,
/// status, timeout or parse failure, in which case the caller keeps its
/// candidates in whatever order it already had them (fail-open, same as
/// every other decision in this package).
pub fn rank_scout_candidates(
    request: &str,
    candidates: &[ScoutCandidate],
    route: ScoutRankRoute<'_>,
) -> Option<ScoutRanking> {
    if candidates.is_empty() {
        return None;
    }
    let (state, kept_indices, dropped) = bounded_candidate_state(request, candidates);
    if kept_indices.is_empty() {
        return None;
    }
    let questions: Vec<(String, Question)> = kept_indices
        .iter()
        .map(|&index| {
            (
                index.to_string(),
                Question::Noul {
                    instructions: format!("file {index} is relevant to the request"),
                },
            )
        })
        .collect();
    let answers = decide::decide(route.model, state, &questions).ok()?;
    let mut scored: Vec<(String, f64)> = Vec::new();
    let mut latency_ms = 0u64;
    for decision in answers.decisions {
        latency_ms = decision.latency_ms;
        let Answer::Noul(score) = decision.answer else {
            continue;
        };
        let Ok(index) = decision.key.parse::<usize>() else {
            continue;
        };
        let Some(candidate) = candidates.get(index) else {
            continue;
        };
        scored.push((candidate.path.clone(), score));
    }
    if scored.is_empty() {
        return None;
    }
    scored.sort_by(|a, b| b.1.total_cmp(&a.1));
    let ranked = scored.len() as u32;
    let skipped = scored
        .iter()
        .filter(|(_, score)| *score < route.floor)
        .count() as u32;
    let passing: Vec<(String, f64)> = scored
        .into_iter()
        .filter(|(_, score)| *score >= route.floor)
        .collect();
    let past_cap = passing.len().saturating_sub(MAX_RANKED_SCOUT_FILES) as u32;
    let kept = passing.into_iter().take(MAX_RANKED_SCOUT_FILES).collect();
    Some(ScoutRanking {
        ranked,
        kept,
        skipped,
        dropped,
        past_cap,
        latency_ms,
    })
}

/// The section appended to the Scout's brief so it reads the ranked
/// candidates before it looks for anything itself. Only used when
/// [`ScoutRankRoute::apply`] is set -- see [`preflight_judged`].
fn scout_ranking_section(ranking: &ScoutRanking) -> String {
    let mut section = String::from("\n\n## Candidate files, ranked by relevance\n");
    for (path, score) in &ranking.kept {
        section.push_str(&format!("{path} ({score:.2})\n"));
    }
    if ranking.skipped > 0 {
        section.push_str(&format!(
            "({} candidate file(s) skipped below the relevance floor)\n",
            ranking.skipped
        ));
    }
    // What this list is not: the bounds above it, named. A Scout told only
    // what ranked well reads the list as the whole field and stops looking.
    if ranking.past_cap > 0 {
        section.push_str(&format!(
            "({} further file(s) cleared the floor but are not listed here: this brief names at most {MAX_RANKED_SCOUT_FILES})\n",
            ranking.past_cap
        ));
    }
    if ranking.dropped > 0 {
        section.push_str(&format!(
            "({} candidate file(s) were never ranked: the ranking request could not carry them)\n",
            ranking.dropped
        ));
    }
    section.push_str("Read these first, in this order, before searching further.\n");
    section
}

/// The candidates [`rank_scout_candidates`] ranks for a Scout call: the
/// distinct paths [`crate::helper_context::prepare`]'s own task-term walk
/// actually matched (`EvidenceKind::Match`), in the order it found them --
/// never a separate directory walk, and never any model work inside
/// `helper_context.rs` itself, which does none by design. A file the walk
/// never matched is never offered to the ranking question at all, whatever
/// its place in the walk's own directory order.
///
/// Each kept path's head is re-read here, bounded the same way
/// [`bounded_head`] bounds it elsewhere -- `prepare`'s own evidence carries
/// only the matched line, not a head -- from `profile`'s root; an
/// unreadable path is skipped rather than failing the caller. Every path
/// came from `prepare`'s own walk, so it already cleared that walk's
/// permission and root checks; nothing here reaches further than the Scout
/// would reach on its own.
pub fn scout_candidates_from_evidence(
    evidence: &[crate::helper_context::Evidence],
    profile: &crate::sandbox::profile::Profile,
    max_files: usize,
) -> Vec<ScoutCandidate> {
    let mut seen = std::collections::HashSet::new();
    let mut candidates = Vec::new();
    for item in evidence {
        if item.kind != crate::helper_context::EvidenceKind::Match {
            continue;
        }
        let Some((path, _line)) = item.subject.rsplit_once(':') else {
            continue;
        };
        if !seen.insert(path.to_string()) {
            continue;
        }
        if candidates.len() >= max_files {
            break;
        }
        let Ok(text) = std::fs::read_to_string(profile.root().join(path)) else {
            continue;
        };
        candidates.push(ScoutCandidate {
            path: path.to_string(),
            head: bounded_head(&text),
        });
    }
    candidates
}

/// The immutable per-call context every helper entry point needs, bundled
/// so [`run_judged`] and [`preflight_judged`] stay under clippy's argument
/// ceiling without changing [`run`]'s own signature -- `runtime/bindings.rs`
/// and existing tests call `run` exactly as it is today.
#[derive(Clone, Copy)]
pub struct HelperContext<'a> {
    pub profile: &'a crate::sandbox::profile::Profile,
    pub session: &'a crate::contract::SessionId,
    pub token: &'a crate::tools::invoke::CancellationToken,
}

/// The decision model and floor for one helper-result judge question.
#[derive(Debug, Clone, Copy)]
pub struct HelperJudge<'a> {
    pub model: &'a str,
    pub floor: f64,
    /// `mode = on`: a confident no is written into the outcome's text.
    /// `mode = shadow`: the question is still asked, never written.
    pub apply: bool,
}

/// Asks whether `outcome`'s text answers `asked`, and on a confident no
/// under `judge.apply`, appends one line to `outcome.text` -- never
/// replacing, truncating or rerunning it. Returns the noul answer and the
/// question's own latency, or `None` when there was nothing to judge (the
/// call already failed or was cancelled) or the question itself failed,
/// timed out or came back unparseable.
fn judge_outcome(
    asked: &str,
    outcome: &mut HelperOutcome,
    judge: HelperJudge<'_>,
) -> Option<(f64, u64)> {
    if !outcome.ok {
        return None;
    }
    let state = serde_json::json!({"asked": asked, "result": outcome.text});
    let questions = [(
        "judge".to_string(),
        Question::Noul {
            instructions: "the result answers what was asked".to_string(),
        },
    )];
    let answers = decide::decide(judge.model, state, &questions).ok()?;
    let decision = answers.decisions.into_iter().find(|d| d.key == "judge")?;
    let latency_ms = decision.latency_ms;
    let Answer::Noul(noul) = decision.answer else {
        return None;
    };
    if judge.apply && noul <= judge.floor {
        outcome.text.push_str(&format!(
            "\n\ndecision: this result may not answer what was asked ({noul:.2})"
        ));
    }
    Some((noul, latency_ms))
}

/// [`run`] plus one judge question on what it returned (2645). `judge:
/// None` is byte-identical to [`run`] -- no model configured, `mode = off`,
/// or the caller chooses not to ask. Never withholds or reruns the call:
/// the judge only reads `call.outcome` after it is already decided. The
/// second element is the judge's own `(noul, latency_ms)`, for a caller's
/// telemetry -- `None` under the same conditions as [`judge_outcome`].
pub fn run_judged(
    spec: &HelperSpec,
    route: HelperRoute<'_>,
    input: &str,
    context: HelperContext<'_>,
    judge: Option<HelperJudge<'_>>,
) -> (HelperCall, Option<(f64, u64)>) {
    let mut call = run(
        spec,
        route,
        input,
        context.profile,
        context.session,
        context.token,
    );
    let judged = judge.and_then(|judge| judge_outcome(input, &mut call.outcome, judge));
    (call, judged)
}

/// What [`preflight_judged`] answers with: the same record [`preflight`]
/// returns, plus the ranking that shaped what the Scout was served (`None`
/// when nothing was ranked) and the judge's own `(noul, latency_ms)` on the
/// Scout's returned result (`None` under [`judge_outcome`]'s own
/// conditions).
pub struct PreflightJudged {
    pub record: HelperRecord,
    pub ranking: Option<ScoutRanking>,
    pub judge: Option<(f64, u64)>,
}

/// [`preflight`] plus the candidate ranking (2644) and the result judge
/// (2645) on the same call. `rank: None` or `judge: None` leaves that half
/// exactly as [`preflight`] behaves.
pub fn preflight_judged(
    input: &str,
    route: HelperRoute<'_>,
    context: HelperContext<'_>,
    rank: Option<(&[ScoutCandidate], ScoutRankRoute<'_>)>,
    judge: Option<HelperJudge<'_>>,
    oneshot: bool,
    mut progress: impl FnMut(&HelperRecord),
) -> Option<PreflightJudged> {
    let spec = if oneshot {
        &DISSECTOR
    } else {
        HELPERS
            .iter()
            .find(|spec| spec.call_sites.contains(&CallSite::Preflight))?
    };
    let request = crate::preflight::request_in(input).unwrap_or(input);

    let ranking = rank.and_then(|(candidates, rank_route)| {
        rank_scout_candidates(request, candidates, rank_route)
    });
    let effective_input = match (&ranking, rank) {
        (Some(ranking), Some((_, rank_route))) if rank_route.apply => {
            format!("{input}{}", scout_ranking_section(ranking))
        }
        _ => input.to_string(),
    };

    let mut record = HelperRecord {
        helper: spec.name.to_string(),
        verb: spec.verb.to_string(),
        asked: bounded_ask(request),
        ..HelperRecord::default()
    };
    progress(&record);
    let (call, judged) = run_judged(spec, route, &effective_input, context, judge);
    record.outcome = call.outcome;
    record.turns = call.turns;
    record.looked = call.looked;
    record.usage = call.usage;
    progress(&record);
    Some(PreflightJudged {
        record,
        ranking,
        judge: judged,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_shipped_roster_is_valid() {
        validate().expect("the roster must pass its own guardrails");
    }

    #[test]
    fn a_spec_naming_a_mutating_tool_is_refused() {
        // Driven through the production predicate, so deleting the guard fails
        // this test rather than leaving it green.
        /// One `'static` toolset per forbidden tool. Kept honest by the
        /// assertion below rather than by matching it up by eye.
        const ROGUE: [(&str, &[&str]); 3] = [
            ("write", &["write"]),
            ("edit", &["edit"]),
            ("bash", &["bash"]),
        ];

        assert_eq!(
            ROGUE.map(|(name, _)| name),
            FORBIDDEN_TOOLS,
            "this test must cover every forbidden tool"
        );

        for (tool, toolset) in ROGUE {
            assert!(
                registry::lookup(tool).is_some(),
                "`{tool}` must be a real tool, or the refusal guards nothing"
            );
            let rogue = HelperSpec {
                tools: toolset,
                ..REDUCER
            };
            let refused =
                check_spec(&rogue).expect_err("a helper holding a mutating tool must be refused");
            assert!(refused.contains(tool), "{refused}");
            assert!(refused.contains("never changes the world"), "{refused}");
        }
    }

    #[test]
    fn a_spec_naming_an_unregistered_tool_is_refused() {
        let rogue = HelperSpec {
            tools: &["telepathy"],
            ..REDUCER
        };
        let refused = check_spec(&rogue).expect_err("an unknown tool must be refused");
        assert!(refused.contains("not a registered tool"), "{refused}");
    }

    #[test]
    fn a_spec_with_no_call_site_or_no_turn_at_all_is_refused() {
        let unreachable = HelperSpec {
            call_sites: &[],
            ..REDUCER
        };
        assert!(
            check_spec(&unreachable)
                .expect_err("a helper nothing can invoke must be refused")
                .contains("never be invoked")
        );

        // Zero turns is the only turn count a spec cannot have: it is
        // incoherent, not merely large. Nothing refuses a spec for asking
        // for many — the user's ruling of 2026-09-17.
        let silent = HelperSpec {
            max_turns: 0,
            ..REDUCER
        };
        assert!(
            check_spec(&silent)
                .expect_err("a helper that cannot take a turn must be refused")
                .contains("no turns at all")
        );
        let patient = HelperSpec {
            max_turns: 64,
            ..REDUCER
        };
        assert!(
            check_spec(&patient).is_ok(),
            "a helper spec may declare as many turns as its errand needs"
        );
    }

    /// The three `little-helpers.md` starts with, by the names the model
    /// calls them by. `lookup` is the production path a binding uses, so a
    /// spec that is written but not appended to `HELPERS` fails here.
    #[test]
    fn the_roster_holds_the_three_the_spec_starts_with() {
        for name in ["find", "reduce", "check"] {
            assert!(
                lookup(name).is_some(),
                "`{name}` must be in the roster the model is offered"
            );
        }
        // The acceptance lister (2026-09-14) is the fourth: pushed, never
        // offered to a cell.
        assert!(lookup("accept").is_some());
        assert!(!ACCEPTANCE.call_sites.contains(&CallSite::Cell));
        // The mender (2026-09-19) is the fifth, and the first that repairs
        // rather than reports: it is reached only from a parse failure, where
        // nothing ran and so nothing can be undone.
        assert!(lookup("mend").is_some());
        assert_eq!(
            HELPERS.len(),
            5,
            "the roster is the whole extension point; nothing else may be in it"
        );
    }

    /// `run` routes on the spec rather than on the name: a toolless one-turn
    /// spec is one wire call, and anything else needs the agent loop. Driven
    /// through the production predicate, because a test that re-derived the
    /// condition would stay green with the branch deleted.
    #[test]
    fn each_spec_is_routed_to_the_runtime_that_can_serve_it() {
        assert!(
            one_shot(&REDUCER),
            "the reducer holds nothing and takes one turn"
        );
        for spec in [&SCOUT, &CHECKER] {
            assert!(
                !one_shot(spec),
                "`{}` holds tools, so it needs the agent loop",
                spec.name
            );
        }
    }

    #[test]
    fn the_reducer_holds_no_tools_and_takes_one_turn() {
        assert!(REDUCER.tools.is_empty(), "the reducer must reach nothing");
        assert_eq!(REDUCER.max_turns, 1);
    }

    /// `little-helpers.md`: the two contracts no toolset can express stay in
    /// **each** helper's preamble. Asserted over the whole roster rather than
    /// per spec, so a helper appended tomorrow cannot ship without them.
    #[test]
    fn every_helper_states_the_two_contracts_in_its_own_preamble() {
        for spec in HELPERS {
            // **The mender is the one helper that proposes a change, because
            // that is its whole job.** The three phrases below bind a helper
            // that REPORTS: a wrong diagnosis the caller trusts is worse than
            // no diagnosis, so it returns evidence and never a fix. A
            // repairing helper needs the other discipline instead, asserted
            // below -- exempting it from a sentence it cannot honestly say is
            // not a hole, because the sentence it must say is stricter.
            if spec.name == MENDER.name {
                for phrase in [
                    "Change nothing about what the program means",
                    "you are not its author",
                    "Deleting code parses beautifully",
                ] {
                    assert!(
                        spec.preamble.contains(phrase),
                        "`{}` must say `{phrase}`: it changes code, so it says what it may \
                         never change",
                        spec.name
                    );
                }
                continue;
            }
            for phrase in [
                "you are returning evidence",
                "Never propose a fix",
                "as your last line",
            ] {
                assert!(
                    spec.preamble.contains(phrase),
                    "`{}` must say `{phrase}`: evidence never conclusions, and say what you \
                     did not look at",
                    spec.name
                );
            }
        }
    }

    /// The toolsets are driven through the production predicate: re-listing
    /// them here would pass with `check_spec` deleted, and `check_spec` is
    /// what refuses a mutating or unregistered name.
    #[test]
    fn the_scout_and_the_checker_hold_only_registered_read_only_tools() {
        for spec in [&SCOUT, &CHECKER] {
            check_spec(spec).unwrap_or_else(|refused| panic!("{refused}"));
            assert!(
                !spec.tools.is_empty(),
                "`{}` reads the project, so an empty toolset would make it a one-shot",
                spec.name
            );
        }
    }

    #[test]
    fn the_reducer_forbids_conclusions_in_its_own_preamble() {
        // The one contract no toolset can express, so it must be in the prose.
        assert!(REDUCER.preamble.contains("Never state a cause"));
        assert!(REDUCER.preamble.contains("evidence"));
    }

    #[test]
    fn a_waiting_helper_returns_promptly_when_its_caller_cancels() {
        let token = crate::tools::invoke::CancellationToken::new();
        let canceller = token.clone();
        let (release, held) = mpsc::channel::<()>();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(30));
            canceller.cancel();
        });

        let started = Instant::now();
        // The operation would hold for three seconds; a cancellation that
        // returns in well under one proves it did not wait for it. The bound
        // is a second rather than a few poll intervals because a starved CI
        // runner (macOS, run 34585283591) took over 250ms just to schedule the
        // cancelling thread, and that is not what this test is about.
        let result = wait_for_helper(&token, move || {
            let _ = held.recv_timeout(Duration::from_secs(3));
            7
        });
        drop(release);

        assert!(matches!(result, HelperWait::Cancelled));
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "cancellation waited for the owned helper operation"
        );
    }

    /// **A reduction is always a fraction of what it replaces.**
    ///
    /// The property, not three spot checks: across every size that can reach
    /// the reducer, the allowance leaves a real saving and never falls below
    /// the floor. The economics test in `reduce_oversized` spends a request
    /// only when `tokens - allowance` clears half the threshold, and that is
    /// guaranteed here rather than hoped for.
    #[test]
    fn a_reduction_is_always_a_fraction_of_what_it_replaces() {
        // The threshold is the smallest input that reaches the reducer at
        // all; above it, every decade up to a very large log.
        for input in [2_049usize, 4_000, 6_644, 16_000, 50_000, 500_000] {
            let cap = reduction_cap(input);
            assert!(
                cap >= REDUCTION_FLOOR,
                "{input} tokens gave {cap}, below the floor"
            );
            assert!(
                cap <= REDUCTION_CEILING,
                "{input} tokens gave {cap}, above the ceiling"
            );
            assert!(
                (cap as usize) < input,
                "{input} tokens gave {cap}: a reduction that big replaces nothing"
            );
        }
    }

    /// The floor and the ceiling each bind at their own end, and the middle
    /// of the range is neither -- so a change to one clamp cannot silently
    /// swallow the whole function.
    #[test]
    fn the_floor_and_the_ceiling_each_bind_at_their_own_end() {
        assert_eq!(reduction_cap(0), REDUCTION_FLOOR, "the floor binds below");
        assert_eq!(
            reduction_cap(1_000_000),
            REDUCTION_CEILING,
            "the ceiling binds above"
        );
        // Measured 2026-09-19: 100 terse `cargo test` failures are ~6,644
        // tokens and need ~1,505 to name every distinct one. The constant
        // 1,024 this replaced could not carry that and returned a refusal.
        let measured = reduction_cap(6_644);
        assert_eq!(measured, 1_661, "the divisor is what the middle uses");
        assert!(
            measured > 1_505,
            "the measured irreducible core must fit: {measured}"
        );
    }
}
