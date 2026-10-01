//! The bytes the model receives for Sterna's native cell contract. This module
//! renders prompts and cell feedback; provider call correlation stays typed
//! in [`crate::contract`].

pub mod declarations;

use crate::abi::{self, types};
use crate::contract::{Block, Conversation};
use crate::runtime::bindings::HostGlobals;
use crate::tools::registry::{Arg, Tool};

/// `model-contract.md` §2, verbatim. Compared byte for byte by
/// `prompt_bytes.rs::the_preamble_is_the_contracts_verbatim`.
pub const PREAMBLE: &str = concat!(
    "You are Sterna, a coding assistant. Answer conversational questions naturally.\n",
    "To act with tools, make exactly one `execute_cell` call in an assistant turn.\n",
    "Put every operation in that one TypeScript program; `execute_cell` is the only\n",
    "provider-native tool. Runtime tools are callable only inside its code.\n",
    "While you construct the call, none of THIS cell has executed. Code may await\n",
    "tools and branch on their actual returned values. Batch deterministic work\n",
    "when useful; stop at the next decision that needs unseen evidence. After\n",
    "submitting a cell, wait for its correlated result. Never invent output or\n",
    "infer success: only that result is runtime evidence.\n",
    "\n",
    "A cell is a program, and that is what earns it a turn. One cell can read\n",
    "several files, search the tree, edit, run the tests and branch on what comes\n",
    "back: every call is awaited, and every result is a live value the next line\n",
    "uses. So spend the turn on a whole step — gather what the step needs, act on\n",
    "it, and check the result in the same program — then yield when the next\n",
    "decision needs evidence that does not exist yet.\n",
    "\n",
    "  // one inspection cell: everything the next step is about to change\n",
    "  const [limits, callback, hits] = await Promise.all([\n",
    "    context({path: \"src/config.rs\", symbol: \"Limits\"}),\n",
    "    context({path: \"src/runtime/bindings.rs\", symbol: \"tool_callback\"}),\n",
    "    rg({pattern: \"cell_wall_clock|response_bytes\", path: \"src\"}),\n",
    "  ]);\n",
    "  return {omissions: limits.omissions, matched: hits.length};\n",
    "\n",
    "  // the next cell: the edits those results earned, and the check for them\n",
    "  await edit({path: \"src/config.rs\", old: OLD, replacement: REPLACEMENT});\n",
    "  const run = await bash({command: \"cargo test -p sterna --lib config\"});\n",
    "  const failures = run.stdout.split(\"\\n\").filter(line => line.includes(\"FAILED\"));\n",
    "  return {passed: run.exit_code === 0, failures};\n",
    "\n",
    "  // judge what the cell already holds, and branch on it, in the same turn\n",
    "  const diff = await bash({command: \"git diff --stat\"});\n",
    "  const call = await decide.choice(\n",
    "    \"Does this diff do more than rename a symbol?\",\n",
    "    {rename_only: \"every hunk renames one symbol\", wider: \"anything else\"},\n",
    "    diff.stdout);\n",
    "  if (call.choice === \"wider\" && call.confidence > 0.85) { /* inspect */ }\n",
    "\n",
    "`decide.choice` answers from inside the cell and costs no turn, so a\n",
    "judgement belongs in the step that needs it rather than in a turn of its\n",
    "own; the Runtime block below declares it when this session has it.\n",
    "\n",
    "Changing existing source has a rhythm worth knowing before you start: `edit`\n",
    "writes against lines a previous completed cell showed you — a `context`, or the\n",
    "lines Sterna attaches when a check fails. So when a change spans several files or\n",
    "symbols, fetch all of them in one cell and make every edit in the next: two\n",
    "turns for the whole batch, not two per file. After a failing check, edit the\n",
    "attached lines directly and rerun it in the same cell.\n",
    "\n",
    "Every cell carries a description: one short line, in the person's language,\n",
    "saying what it is for and why — not which functions it calls. It is the only\n",
    "account of your work the person sees while you run, and you read it back after\n",
    "compaction. Pass it as the `description` argument; in the fenced form it is the\n",
    "line immediately before the fence.\n",
    "\n",
    "A cell is validated before it runs. A parse error runs nothing and may offer\n",
    "`sterna-edit`; a return, yield, or throw stops later code. Tool results are live\n",
    "objects, but unseen fields are not model-visible: use declared fields and\n",
    "standard JavaScript, and bind your own values to names no declared tool or\n",
    "host global already has. Reuse live handles rather than repeating a read.\n",
    "For an existing source change,\n",
    "`context({path, symbol})` is the first source-reading tool and delivers its\n",
    "complete target automatically; `read` and whole-file prints are for files the\n",
    "step is not about to edit. Then `edit({path, old, replacement})` — the second\n",
    "argument is `replacement`, since `new` is a JavaScript keyword — and Sterna binds\n",
    "it to the latest observed source version. For file or script text containing\n",
    "`$`, quotes or heredocs, `write` and `edit` take line arrays:\n",
    "one double-quoted JavaScript string per logical line, and Sterna supplies the\n",
    "separators. Prefer compact structured summaries or bounded excerpts to broad\n",
    "prints. `glob` may return directories, so select a file before `read`. A\n",
    "`bash` result succeeded only when its `exit_code` says so.\n",
    "\n",
    "Bindings persist between cells of this user request; redeclaring replaces\n",
    "them. Each new user request starts a fresh runtime. Earlier requests are\n",
    "history, not unfinished work. Work on the current request, including its\n",
    "requested tests. Running off the end, `yieldNow(reason)` or a top-level\n",
    "`return` all give results and another turn: returning a value displays it\n",
    "as notebook output and finishes nothing. Return whatever you want to look\n",
    "at, as often as you like. A returned object is shown field by field as\n",
    "text -- an excerpt as its lines, an array of strings one per line -- within\n",
    "the return budget the usage line names; a field over its share is paged at\n",
    "a line and ends in one cursor line saying how to read on. Return what you\n",
    "need to read next, not everything you hold.\n",
    "\n",
    "The task ends only where you say it ends.\n",
    "`answer(text)` inside a cell ends the task with that text.\n",
    "A prose response with no `execute_cell` call ends the task as the answer.\n",
    "Use either only when the request is finished and the answer is grounded in\n",
    "observed results; do not use prose to announce work you still intend to\n",
    "perform.\n",
    "To interpret a file, inspect and yield first, then answer from the feedback.\n",
    "\n",
    "A thrown error carries its position and completed bindings. Continue from\n",
    "that state; failed or skipped calls did not succeed. PermissionDenied is\n",
    "final: code cannot widen the session's sandbox grant.",
);

/// One segment of [`PREAMBLE`] whose wording depends on the declared
/// interface. `cells` is the segment's exact bytes in the constant; a `None`
/// keeps it, `Some("")` drops it.
struct Variant {
    cells: &'static str,
    hybrid: Option<&'static str>,
    tools: Option<&'static str>,
}

/// The table [`preamble_for`] applies — `model-contract.md` §2.1.
///
/// The invariant: **every sentence the three preambles share is one copy in
/// [`PREAMBLE`].** A variant is a replacement over that constant, never a
/// second constant, so the shared guidance cannot drift between interfaces;
/// `prompt::tests::every_variant_segment_occurs_exactly_once` fails when a
/// segment stops matching.
const VARIANTS: &[Variant] = &[
    // A request that declares no `execute_cell` runs no cells, so the
    // descriptor sentence would name an argument it has no call to put on.
    Variant {
        cells: "Every cell carries a description: one short line, in the person's language,\n\
                saying what it is for and why — not which functions it calls. It is the only\n\
                account of your work the person sees while you run, and you read it back after\n\
                compaction. Pass it as the `description` argument; in the fenced form it is the\n\
                line immediately before the fence.\n\n",
        hybrid: None,
        tools: Some(""),
    },
    Variant {
        cells: "To act with tools, make exactly one `execute_cell` call in an assistant turn.\n\
                Put every operation in that one TypeScript program; `execute_cell` is the only\n\
                provider-native tool. Runtime tools are callable only inside its code.\n",
        hybrid: Some(
            "To act, call a familiar tool directly for one independent operation, or make\n\
             exactly one `execute_cell` call for dependent, branching, looped or batched\n\
             work. Inside a cell the same tools are typed async functions with the same\n\
             arguments and results.\n",
        ),
        tools: Some(
            "To act, call the familiar tools directly; each call's result is runtime\n\
             evidence.\n",
        ),
    },
    Variant {
        cells: "While you construct the call, none of THIS cell has executed. Code may await\n\
                tools and branch on their actual returned values. Batch deterministic work\n\
                when useful; stop at the next decision that needs unseen evidence. After\n\
                submitting a cell, wait for its correlated result. Never invent output or\n\
                infer success: only that result is runtime evidence.",
        hybrid: None,
        tools: Some(
            "Stop at the next decision that needs unseen evidence. After each call, wait\n\
             for its correlated result. Never invent output or infer success: only that\n\
             result is runtime evidence.",
        ),
    },
    Variant {
        cells: "A cell is validated before it runs. A parse error runs nothing and may offer\n\
                `sterna-edit`; a return, yield, or throw stops later code. Tool results are live\n\
                objects, but unseen fields are not model-visible: use declared fields and\n\
                standard JavaScript, and bind your own values to names no declared tool or\n\
                host global already has. Reuse live handles rather than repeating a read.\n\
                For an existing source change,\n\
                `context({path, symbol})` is the first source-reading tool and delivers its\n\
                complete target automatically; `read` and whole-file prints are for files the\n\
                step is not about to edit. Then `edit({path, old, replacement})` — the second\n\
                argument is `replacement`, since `new` is a JavaScript keyword — and Sterna binds\n\
                it to the latest observed source version. For file or script text containing\n\
                `$`, quotes or heredocs, `write` and `edit` take line arrays:\n\
                one double-quoted JavaScript string per logical line, and Sterna supplies the\n\
                separators. Prefer compact structured summaries or bounded excerpts to broad\n\
                prints. `glob` may return directories, so select a file before `read`. A\n\
                `bash` result succeeded only when its `exit_code` says so.",
        hybrid: None,
        tools: Some(
            "Sterna binds an edit to the latest observed source version. A command result\n\
             succeeded only when its `exit_code` says so.",
        ),
    },
    Variant {
        // `answer` is a host function inside a cell, so a request that
        // declares no `execute_cell` has nowhere to call it.
        cells: "`answer(text)` inside a cell ends the task with that text.\n",
        hybrid: None,
        tools: Some(""),
    },
    Variant {
        cells: "A prose response with no `execute_cell` call ends the task as the answer.\n",
        hybrid: Some("A prose response with no tool call ends the task as the answer.\n"),
        tools: Some("A prose response with no tool call ends the task as the answer.\n"),
    },
    // The chaining paragraph and its worked cells: what a turn buys. The
    // examples carry their own indentation, so these segments are `concat!`
    // forms rather than line-continuations, which would strip it. A request
    // that declares no `execute_cell` has no cell to fill, so `Tools` drops
    // each of them with its separator; `Hybrid` keeps them, because a cell is
    // exactly where hybrid's dependent work belongs.
    Variant {
        cells: concat!(
            "A cell is a program, and that is what earns it a turn. One cell can read\n",
            "several files, search the tree, edit, run the tests and branch on what comes\n",
            "back: every call is awaited, and every result is a live value the next line\n",
            "uses. So spend the turn on a whole step — gather what the step needs, act on\n",
            "it, and check the result in the same program — then yield when the next\n",
            "decision needs evidence that does not exist yet.\n",
            "\n",
            "  // one inspection cell: everything the next step is about to change\n",
            "  const [limits, callback, hits] = await Promise.all([\n",
            "    context({path: \"src/config.rs\", symbol: \"Limits\"}),\n",
            "    context({path: \"src/runtime/bindings.rs\", symbol: \"tool_callback\"}),\n",
            "    rg({pattern: \"cell_wall_clock|response_bytes\", path: \"src\"}),\n",
            "  ]);\n",
            "  return {omissions: limits.omissions, matched: hits.length};\n",
            "\n",
            "  // the next cell: the edits those results earned, and the check for them\n",
            "  await edit({path: \"src/config.rs\", old: OLD, replacement: REPLACEMENT});\n",
            "  const run = await bash({command: \"cargo test -p sterna --lib config\"});\n",
            "  const failures = run.stdout.split(\"\\n\").filter(line => line.includes(\"FAILED\"));\n",
            "  return {passed: run.exit_code === 0, failures};\n",
            "\n",
            "  // judge what the cell already holds, and branch on it, in the same turn\n",
            "  const diff = await bash({command: \"git diff --stat\"});\n",
            "  const call = await decide.choice(\n",
            "    \"Does this diff do more than rename a symbol?\",\n",
            "    {rename_only: \"every hunk renames one symbol\", wider: \"anything else\"},\n",
            "    diff.stdout);\n",
            "  if (call.choice === \"wider\" && call.confidence > 0.85) { /* inspect */ }\n",
            "\n",
        ),
        hybrid: None,
        tools: Some(""),
    },
    Variant {
        cells: concat!(
            "`decide.choice` answers from inside the cell and costs no turn, so a\n",
            "judgement belongs in the step that needs it rather than in a turn of its\n",
            "own; the Runtime block below declares it when this session has it.\n",
            "\n",
        ),
        hybrid: Some(concat!(
            "`decide.choice` answers from inside the cell and costs no turn, so a\n",
            "judgement belongs in the step that needs it rather than in a turn of its\n",
            "own; the Runtime block below declares it when this session has it. A direct\n",
            "call spends a whole turn on one operation, which suits an independent step\n",
            "whose result needs nothing further this turn; dependent, branching or\n",
            "repeated work is what a cell is for.\n",
            "\n",
        )),
        tools: Some(""),
    },
    // The two-turn rhythm `edit` imposes. `Tools` states the version rule in
    // its own mechanics replacement and has no cell to batch into.
    Variant {
        cells: concat!(
            "Changing existing source has a rhythm worth knowing before you start: `edit`\n",
            "writes against lines a previous completed cell showed you — a `context`, or the\n",
            "lines Sterna attaches when a check fails. So when a change spans several files or\n",
            "symbols, fetch all of them in one cell and make every edit in the next: two\n",
            "turns for the whole batch, not two per file. After a failing check, edit the\n",
            "attached lines directly and rerun it in the same cell.\n",
            "\n",
        ),
        hybrid: None,
        tools: Some(""),
    },
];

/// [`PREAMBLE`] as the declared interface needs it — `model-contract.md`
/// §2.1. `Cells` is the constant verbatim; `Hybrid` describes both routes;
/// `Tools` drops the cell mechanics a request without `execute_cell` cannot
/// use. No variant says or implies that `execute_cell` is the only native
/// tool, because in hybrid mode it is not.
pub fn preamble_for(interface: abi::Interface) -> String {
    let mut text = PREAMBLE.to_string();
    for variant in VARIANTS {
        let replacement = match interface {
            abi::Interface::Cells => None,
            abi::Interface::Hybrid => variant.hybrid,
            abi::Interface::Tools => variant.tools,
        };
        if let Some(replacement) = replacement {
            text = text.replacen(variant.cells, replacement, 1);
        }
    }
    text
}

/// Request-only context; keeps user text and saved conversation unchanged.
/// A new runtime is created per user request, not per inference turn.
///
/// **A request never edits a message an earlier request already sent.** That
/// is what lets a provider serve the conversation from its prompt cache: the
/// prefix is the same bytes it was last turn, so only the new tail is paid
/// for. [`project_runtime_history`] honours it by construction (it changes
/// only the message that just stopped being the newest), and this function
/// used to break it: it appended a `[Sterna task boundary: this is the current
/// user request …]` block to the current task's message, so the *previous*
/// task's message silently lost that block the moment a second task began.
///
/// Measured 2026-09-17 by rendering four turns across a task boundary: within
/// one task the requests shared every message but the frontier (a prefix of 8
/// of 11), and at the boundary they shared **nothing** — the first divergence
/// was message 0, so the whole conversation was re-processed uncached. The
/// block said what [`PREAMBLE`] already says in the cached system block
/// ("Each new user request starts a fresh runtime. Earlier requests are
/// history, not unfinished work. Work on the current request"), and after an
/// overflow [`checkpoint`] names the task under *## The task* as well. A
/// duplicate that costs the whole cache is not worth its bytes, so it is
/// gone; `the_request_never_edits_a_message_it_already_sent` pins the
/// guarantee that replaced it.
///
/// **Nor, since 2026-09-24, does it project the previous result to its
/// historical form**: the result goes back exactly as the model read it,
/// because the model's reasoning after it is bound to those bytes (a changed
/// prefix makes Claude refuse the thinking block) and history is append-only
/// except for compaction (the user's ruling). The stale `## Handles` and
/// `## Task` sections that stay behind are what the proactive sweep
/// ([`compact_conversation`]) trims once the context is large enough to care.
pub fn with_task_context(conversation: &Conversation, model: &str, _task: &str) -> Conversation {
    let mut request = conversation.clone();
    request.system.push_str(&format!(
        "\n\nYou are Sterna, a coding assistant. Configured request model: {}. This is the requested model, not independently verified backend identity. Do not infer a different identity from previous replies or project paths.",
        serde_json::to_string(model).expect("model name serializes")
    ));
    request
}

/// Request-only projection of trusted runtime state. Historical stdout,
/// errors, yield reasons and repair hints were rendered separately from the
/// snapshots, so text that resembles a section header is never reinterpreted.
///
/// **It changes only the message that just stopped being the newest**, and
/// that is why it is cheap against a prompt cache: every earlier message was
/// already projected by the previous request and comes back byte for byte.
/// Measured over six turns, the common prefix between consecutive requests is
/// every message but the last three — the projected frontier, the new
/// assistant turn and its result, of which the last two are new text anyway.
pub fn project_runtime_history(conversation: &mut Conversation, active_from: usize) {
    let latest = conversation
        .messages
        .iter()
        .enumerate()
        .rev()
        .find_map(|(i, m)| (i >= active_from && m.historical.is_some()).then_some(i));
    for (i, message) in conversation.messages.iter_mut().enumerate() {
        if Some(i) != latest
            && let Some(history) = &message.historical
        {
            // Keep native result correlation intact. A multi-block message
            // has no unambiguous per-block historical projection, so retain
            // it conservatively rather than orphaning any call id.
            if message.content.len() == 1 {
                match &mut message.content[0] {
                    Block::Text(text) | Block::ToolResult { content: text, .. } => {
                        *text = history.clone()
                    }
                    Block::ToolUse { .. }
                    | Block::Image { .. }
                    | Block::Thinking { .. }
                    | Block::RedactedThinking { .. } => {}
                }
            }
        }
    }
}

/// Collapses every cell result but the newest `keep` to its first line (the
/// `[cell N yielded in …]` header) plus one sentence saying its values are
/// still bound. `0` keeps everything.
///
/// **The boundary moves in steps of [`COLLAPSE_STEP`] results**, so the
/// request prefix changes once every few turns rather than on every turn:
/// each collapse re-sends the history uncached once, then it is cached
/// again. Measured offline 2026-09-23 over six recorded tasks: keeping two
/// results would have saved 24 % of the parent's input, one 34 %.
pub fn keep_recent_results(conversation: &mut Conversation, keep: usize) {
    if keep == 0 {
        return;
    }
    let results: Vec<usize> = conversation
        .messages
        .iter()
        .enumerate()
        .filter(|(_, m)| m.historical.is_some() && m.content.len() == 1)
        .map(|(i, _)| i)
        .collect();
    let collapse = results.len().saturating_sub(keep) / COLLAPSE_STEP * COLLAPSE_STEP;
    for &i in &results[..collapse] {
        if let Block::Text(text) | Block::ToolResult { content: text, .. } =
            &mut conversation.messages[i].content[0]
        {
            let head = text.lines().next().unwrap_or("").to_string();
            *text = format!("{head}\n{COLLAPSED}");
        }
    }
}

/// The turn-economy line (`[limits] turn_economy`): what a turn costs, said
/// once, so the plan is made in whole steps rather than discovered in small
/// ones.
pub const TURN_ECONOMY: &str = "\n\n## Turns are the expensive unit\n\nEvery turn re-sends \
    this whole conversation, so a task done in four turns costs about half of one done in \
    eight. Plan the task as few cells as it honestly needs: each cell does a whole step -- \
    read everything the step needs at once, make the edits, run the check, and branch on its \
    result in the same program -- and yield only when the next decision needs evidence this \
    cell cannot produce.";

/// The autonomy block (`[limits] autonomy_block`), after the Fable 5.1 and
/// GPT-6 guides: carry a request through without asking leave for its
/// reversible steps, and do not end a turn on a plan or a promise.
pub const AUTONOMY_BLOCK: &str = "\n\n## Carrying the request through\n\nYou are working \
    autonomously: the person is not watching in real time, so asking \"Shall I...?\" or \"Want me \
    to...?\" in an answer blocks the work. For reversible steps that follow from the request, \
    proceed without asking; use `ask` only for a destructive action or a change of scope the \
    person must decide. When the person is describing a problem, asking a question or thinking \
    aloud rather than asking for a change, your assessment is the deliverable: report it and \
    stop. Before you answer, read your last paragraph: if it is a plan, a list of next steps or \
    a promise about work not yet done, do that work now in another cell. Answer only when the \
    request is complete or blocked on input only the person can give.";

/// The scope block (`[limits] scope_block`), after the Fable 5.1 guide's
/// "keep changes and tests to what the task asks for".
pub const SCOPE_BLOCK: &str = "\n\n## Scope\n\nThe request sets the scope. If, while working \
    or testing, you find a pre-existing bug, a performance concern or behaviour the request does \
    not mention, do not fix, optimise or extend it in this change unless the requested behaviour \
    cannot work without it; name it as a follow-up in your answer. Where the request is \
    ambiguous, implement the reading its wording and the surrounding code most directly \
    support, and say which reading you took. Verify however you like; scratch checks need not be \
    kept. Add permanent tests only where the request asks for them or the repository keeps tests \
    for this kind of change. This is about extras only: do every part the request asks for.";

/// The batching nudge (`[limits] batch_nudge`), after the Fable 5.1 guide:
/// one line at the end of each new result -- never an edit of an earlier
/// one -- so it is read at the moment the next cell is planned.
pub const BATCH_NUDGE: &str = "First privately list what you need next; then fetch every item \
    that does not depend on another's result in the next cell.";

/// How many results the collapse boundary advances at a time.
pub const COLLAPSE_STEP: usize = 3;
/// What a collapsed result says in place of its output.
pub const COLLAPSED: &str = "[output collapsed to keep the context small; every value this cell \
    bound is still live in the runtime -- return it again if you need it]";

/// Why the preamble is being replaced, and **it is always something observed
/// rather than something counted** (the user, 2026-09-17: "Limits are dumb for
/// abstract tasks").
///
/// Each variant carries the fact behind it, because the sentence the model
/// reads on its last turn is the only place that fact ever appears: a task
/// ended for a reason it cannot see is a task that ends twice the same way.
/// A ceiling the person set is still a reason — it is theirs, not ours.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExhaustedReason {
    /// The person configured `[limits] cells` and the task reached it.
    CellLimit { cap: u64 },
    /// `windows` whole stall windows in a row: no tree change, no new fact,
    /// no verification result moved, after being told so.
    Stalled { windows: u32, cells: u32 },
    /// `turns` messages in a row that ran no program at all.
    ///
    /// **This is not a budget on thinking.** A turn carrying no program runs
    /// no cell, so it writes no record — and with no record there is nothing
    /// for the stall guard to observe. It is the
    /// only evidence a prose-only task ever produces, which is why it is the
    /// one thing still counted anywhere in this loop.
    NoProgram { turns: u32 },
}

/// The one sentence that replaces the preamble when the task is exhausted —
/// §6's last paragraph, naming the reason. The only permitted action is a
/// top-level returned string.
///
/// One sentence, always, ending in what the model may still do: this is the
/// last thing it reads before the task closes, and a paragraph here competes
/// with the answer it is being asked for.
#[must_use]
pub fn exhausted_preamble(reason: &ExhaustedReason) -> String {
    match reason {
        ExhaustedReason::CellLimit { cap } => format!(
            "The cell limit this project set ({cap}) is reached; the only action this turn may \
             take is returning a final answer string at top level."
        ),
        ExhaustedReason::NoProgram { turns } => format!(
            "No program has run for {turns} turns; the only action this turn may take is \
             returning a final answer string at top level."
        ),
        ExhaustedReason::Stalled { windows, cells } => format!(
            "Nothing has changed for {cells} cells across {windows} notices — every call, every \
             result and the tree were already seen; the only action this turn may take is \
             returning a final answer string at top level."
        ),
    }
}

/// What this particular session is, as the model needs to know it.
///
/// **The invariant: every field here is a fact the model cannot derive from
/// the preamble and would otherwise discover by failing.** A model that does
/// not know the tool set has no writer spends its turns asking `bash` to do
/// it and reading `PermissionDenied`; a model that does not know which
/// commands are admitted cannot tell a refusal it caused from one the person
/// configured. Observed 2026-09-06: a session spent six cells rediscovering
/// exactly these two facts and then returned a stub.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionFacts {
    /// The project root every relative path resolves against.
    pub root: String,
    /// `Write`/`Edit` globs the compiled profile actually holds.
    pub writable: Vec<String>,
    /// Whether the sandbox grants network access.
    pub network: bool,
    /// Which tool definitions the request declares, so the preamble and the
    /// *This session* block describe the routes the model actually has.
    pub interface: abi::Interface,
    /// The rendered `## Environment` block ([`crate::manifest::Manifest::render`]),
    /// when the session collected one.
    pub manifest: Option<String>,
}

/// §1's *This session* block: the facts above, rendered.
///
/// Pure in its argument, so the golden test that pins the binary's system
/// bytes can build the same string without running a session.
/// The line telling a plan request what it may do, or `None` for an ordinary
/// request. Information only: the narrowed profile is what refuses, so a
/// model that ignores this line is refused all the same.
pub fn request_mode_line(mode: crate::sandbox::modes::RequestMode) -> Option<String> {
    use crate::sandbox::modes::{PLAN_FILE, READ_ONLY_COMMANDS, RequestMode};
    match mode {
        RequestMode::Work => None,
        RequestMode::Plan => Some(format!(
            "\nThis is a plan request. Reading tools run; write the plan to {PLAN_FILE} and answer with a short summary; every other write is refused; bash runs only read-only commands ({}) with no redirect into a file; MCP tools are refused. A refusal names the plan; do not retry it.\n",
            READ_ONLY_COMMANDS.join(", ")
        )),
    }
}

/// How much of a plan file one request carries.
pub const PLAN_SECTION_BYTES: usize = 16 * 1024;

/// The system-block section handing a `plan` request's file to the next
/// request: at most [`PLAN_SECTION_BYTES`] of it, cut at a line boundary.
pub fn plan_section(plan: &str) -> String {
    let file = crate::sandbox::modes::PLAN_FILE;
    let body = if plan.len() <= PLAN_SECTION_BYTES {
        plan.trim_end().to_string()
    } else {
        let mut cut = PLAN_SECTION_BYTES;
        while !plan.is_char_boundary(cut) {
            cut -= 1;
        }
        let kept = plan[..cut]
            .rfind('\n')
            .map_or(&plan[..cut], |end| &plan[..end]);
        format!("{kept}\n… [truncated]")
    };
    format!("\n## Plan ({file})\n{body}\n")
}

pub fn render_session_facts(facts: &SessionFacts) -> String {
    let writable = if facts.writable.is_empty() {
        "nothing is writable".to_string()
    } else {
        format!(
            "writable: {} (deny rules still apply)",
            facts.writable.join(", ")
        )
    };
    let whole_set = match facts.interface {
        abi::Interface::Cells => "The tools above are the whole set.",
        abi::Interface::Hybrid => {
            "The tools above are the whole set; the familiar ones are callable directly or\n\
             inside a cell."
        }
        abi::Interface::Tools => "The familiar tools above are the whole set, called directly.",
    };
    format!(
        "## This session\n\nThe project root is {root}. Relative paths resolve against it.\n\n\
         {whole_set} To change existing source, call `context` with\n\
         its target symbol, let that result reach the next turn, then call `edit` with the\n\
         exact old text and replacement. Use `write` for new files or deliberate whole-file\n\
         rewrites. File objects retain their bytes; do not print broad contents.\n\n\
         Sandbox: {writable}; every file is readable except secrets; every command line\n\
         runs unless a deny rule refuses it; network: {network}. Anything outside that\n\
         throws PermissionDenied, which is final — no cell widens a grant, so a refusal\n\
         means choose another route or say plainly that the grant forbids it.",
        root = facts.root,
        network = if facts.network { "yes" } else { "no" },
    )
}

/// §1's system block: the interface's preamble, one declaration per tool in
/// `tools`' order, this session's own facts, its manifest when it has one,
/// then the project's own instructions.
pub fn render_system(instructions: &str, tools: &[&Tool], facts: &SessionFacts) -> String {
    render_system_for(instructions, tools, facts, HostGlobals::Every)
}

/// [`render_system`] for a context whose host globals are narrowed.
///
/// The invariant: **the Runtime block declares what the context actually
/// binds.** A context told about `bg` it does not hold gets a `TypeError` on
/// a name the system block promised, where the point of the narrowing is that
/// the capability is absent and the refusal is clean.
pub fn render_system_for(
    instructions: &str,
    tools: &[&Tool],
    facts: &SessionFacts,
    globals: HostGlobals,
) -> String {
    render_system_reaching(instructions, tools, facts, globals, Reach::default())
}

/// What this session reaches beyond the fixed table: the `[web]` policy, and
/// the models a subagent may be sent to.
///
/// One struct rather than a parameter per global, because each of these is
/// the same kind of fact -- a global whose declaration depends on this
/// session's configuration -- and a caller that has neither says so once.
#[derive(Clone, Copy, Default)]
pub struct Reach<'a> {
    /// `None` is an unconfigured session, which binds no `web` and is told of
    /// none.
    pub web: Option<&'a declarations::WebReach>,
    /// `None` renders the table's generic `agent` text: no roster is claimed
    /// where the session has not resolved one.
    pub agents: Option<&'a declarations::AgentRoster>,
    /// Whether `[decisions]` names a model, which is the one predicate
    /// `decide` is bound on.
    ///
    /// **Declared exactly where it is bound.** The runtime installs `decide`
    /// only for a session whose `[decisions]` names a model, so a session
    /// without one must not be told the global exists — a promise the model
    /// would spend a cell discovering is false.
    pub decisions: bool,
}

impl<'a> Reach<'a> {
    /// The reach of a session that only configured `[web]`.
    #[must_use]
    pub fn webbed(web: Option<&'a declarations::WebReach>) -> Self {
        Self {
            web,
            agents: None,
            decisions: false,
        }
    }
}

/// [`render_system_for`] with what this session reaches: the Runtime block
/// declares `web` only when `web` is configured, and then says which domains
/// it may name (map 2656, 2658), and declares `agent` with the models this
/// session's gateway serves.
pub fn render_system_reaching(
    instructions: &str,
    tools: &[&Tool],
    facts: &SessionFacts,
    globals: HostGlobals,
    reach: Reach<'_>,
) -> String {
    let rendered: Vec<String> = tools.iter().map(|tool| render_declaration(tool)).collect();
    let mut system = format!(
        "{}\n\n## Tools\n\n{}\n\n## Runtime\n\n{}\n\n{}\n\n{}",
        preamble_for(facts.interface),
        rendered.join("\n\n"),
        render_runtime_reaching(globals, reach),
        render_abi_for(globals, facts.interface),
        render_session_facts(facts)
    );
    // The manifest is its own block, directly after *This session*: both
    // describe what this particular session can do, and the manifest is the
    // finer of the two.
    if let Some(manifest) = &facts.manifest {
        system.push_str("\n\n");
        system.push_str(manifest);
    }
    system.push_str("\n\n");
    system.push_str(instructions);
    system
}

/// §1's *Familiar tools* block: the ABI's own types and declarations.
///
/// The invariant is the Runtime block's, for the same reason: **a name the
/// isolate binds is a name this block declares.** `runtime_cells.rs`'s
/// enumeration test fails on a bound-but-undeclared global, and every
/// dialect spelling is one.
///
/// What is deliberately *not* here is any explanation of how a cell reaches a
/// capability. `tool-abi.md` §23 and the `improvement-register.md` prompt
/// boundary both say a rule expressible as a type does not belong in prose,
/// and the completeness fields in [`types::PRELUDE`] are that rule.
pub fn render_abi_for(globals: HostGlobals, interface: abi::Interface) -> String {
    let mut declarations = Vec::new();
    for dialect in abi::dialect::ALL {
        for shape in dialect.shapes() {
            let bound = match shape.target {
                abi::Target::Tool(name) => globals.binds_tool(name),
                abi::Target::HostCall(_) => globals.installs("checks"),
            };
            if !bound
                || declarations
                    .iter()
                    .any(|(name, _)| *name == shape.provider_name)
            {
                continue;
            }
            declarations.push((
                shape.provider_name,
                format!("{}\n// {}", shape.declaration(), shape.description),
            ));
        }
    }
    if declarations.is_empty() {
        return String::new();
    }
    let bodies: Vec<String> = declarations.into_iter().map(|(_, body)| body).collect();
    // Cells mode has no direct call to prefer: its guidance asks for a whole
    // step per turn, as the preamble does, instead of the smallest cell.
    let guidance = match interface {
        abi::Interface::Cells => types::CELLS_GUIDANCE,
        abi::Interface::Hybrid | abi::Interface::Tools => types::GUIDANCE,
    };
    format!(
        "## Familiar tools\n\n{}\n\n{}\n\n{}\n\n{}",
        types::PRELUDE,
        bodies.join("\n\n"),
        guidance,
        types::REPORTING
    )
}

/// §1's *Runtime* block: every host global that is not a tool.
///
/// The invariant is [`declarations::Binding`]'s: a name the isolate binds is
/// a name this block declares. It is rendered from the same table the
/// enumeration test checks, so the two cannot drift.
pub fn render_runtime() -> String {
    render_runtime_for(HostGlobals::Every)
}

/// [`render_runtime`] for a narrowed context: the same table, filtered by the
/// predicate `bindings::install` itself binds on. `web` is declared in its
/// table form here — this is the complete table, not a session's block; a
/// session renders through [`render_runtime_reaching`].
pub fn render_runtime_for(globals: HostGlobals) -> String {
    declarations::RUNTIME
        .iter()
        .filter(|binding| globals.installs(binding.global))
        .map(|binding| binding.declaration.to_string())
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// A session's Runtime block: the table filtered by the narrowing **and by
/// the configuration** — `web` appears only when `[web]` names a domain or
/// an endpoint, rendered with what it reaches, on the same predicate
/// `Runtime::with_web_broker` binds it on
/// (`HostGlobals::installs_with`; map 2658).
pub fn render_runtime_reaching(globals: HostGlobals, reach: Reach<'_>) -> String {
    declarations::RUNTIME
        .iter()
        .filter(|binding| {
            globals.installs_reaching(binding.global, reach.web.is_some(), reach.decisions)
        })
        .map(|binding| match binding.global {
            "web" => reach.web.map_or_else(
                || binding.declaration.to_string(),
                declarations::web_declaration,
            ),
            "agent" => reach.agents.map_or_else(
                || binding.declaration.to_string(),
                declarations::agent_declaration,
            ),
            _ => binding.declaration.to_string(),
        })
        .collect::<Vec<_>>()
        .join("\n\n")
}

fn render_declaration(tool: &Tool) -> String {
    let entry = declarations::lookup(tool.name())
        .unwrap_or_else(|| panic!("no declarations entry for tool `{}`", tool.name()));
    format!(
        "declare function {name}(a: {params}): Promise<{ret}>;\n// {summary} {purity}\n// @callers program",
        name = tool.name(),
        params = render_params(tool.name(), tool.args()),
        ret = entry.return_type,
        summary = entry.summary,
        purity = declarations::purity_clause(tool.purity()),
    )
}

fn render_params(tool: &str, args: &[Arg]) -> String {
    if args.is_empty() {
        return "{}".to_string();
    }
    let mut fields: Vec<String> = args
        .iter()
        .map(|arg| {
            let optional_mark = if arg.is_required() { "" } else { "?" };
            let value_type =
                declarations::param_type(tool, arg.name()).unwrap_or(match arg.kind() {
                    crate::tools::registry::ArgKind::Lines
                    | crate::tools::registry::ArgKind::Texts => "string[]",
                    _ => "string",
                });
            format!("{}{optional_mark}: {value_type}", arg.name())
        })
        .collect();
    fields.extend(
        declarations::runtime_options(tool)
            .iter()
            .map(|field| field.to_string()),
    );
    format!("{{{}}}", fields.join("; "))
}

/// One cell's outcome, as this module's own plain input — the wiring
/// package adapts the isolate's real outcome to it. `handle_table` is
/// already-rendered text; this module never builds one.
pub struct CellResult {
    pub cell: u64,
    pub elapsed_ms: u64,
    /// The one line the model wrote about what this cell was for, echoed back
    /// to it in the result's head so it survives compaction
    /// ([`compact_result`] keeps everything before the first section) and the
    /// model keeps a running account of what it has already done. The user,
    /// 2026-09-17: *"Stays in context might be beneficial."*
    pub description: Option<String>,
    pub error: Option<ErrorSection>,
    /// Why the cell yielded on purpose — `runtime-contract.md` §9.3's one
    /// line under the cell line. Never rendered beside an error: a throw is
    /// not a yield, whatever else the caller filled in.
    pub yield_reason: Option<String>,
    /// Bounded structured value returned for notebook inspection. This is
    /// shown to the model and user, then the task continues.
    pub output: Option<String>,
    pub handle_table: String,
    pub stdout_tail: Option<String>,
    pub budget: Budget,
    /// The person's answer to the question this cell asked, rendered by
    /// [`crate::ask::Answer::rendered`]. `None` for every cell that asked
    /// nothing, which is almost all of them.
    pub ask_answer: Option<String>,
}

/// §6's `## Error` section: the class, the message, where in the model's own
/// program it happened, and up to three in-program frames.
pub struct ErrorSection {
    pub class: String,
    pub message: String,
    /// The line and column inside the model's own program, when the runtime
    /// attributed the throw to one. Absent, no position line is written —
    /// never `line 0, column 0`, which names a place that does not exist.
    pub position: Option<(u64, u64)>,
    pub frames: Vec<String>,
}

impl ErrorSection {
    /// [`position`](Self::position) from a runtime error's own line and
    /// column, which is where the doc comment above is enforced.
    ///
    /// `line.zip(column)` alone is not enough: it distinguishes *absent*
    /// from *present*, and V8 reports a stack overflow as
    /// present-and-**zero**, so `function f(n) { return f(n + 1); } f(0)` —
    /// an ordinary bug in a model-written traversal — was rendered as
    /// `line 0, column 0`. There is no line 0 in anyone's program.
    pub fn position_of(line: Option<u32>, column: Option<u32>) -> Option<(u64, u64)> {
        line.zip(column)
            .filter(|&(line, column)| line != 0 || column != 0)
            .map(|(line, column)| (u64::from(line), u64::from(column)))
    }
}

/// The usage figures §6 reports. `task_cap` is retained for compatibility
/// with callers constructing this value, but task spend is never capped and
/// the renderer does not expose the field.
pub struct Budget {
    pub turn_cap: u64,
    pub task_used: u64,
    pub task_cap: u64,
    pub cells_used: u64,
    /// The ceiling this person set, or `None` — the usage line then shows the
    /// count alone, because a denominator nobody chose is a fiction.
    pub cells_cap: Option<u64>,
    /// The return budget this turn's `## Output` was rendered within, and
    /// what the return cost, when a session is keeping the figure. `None`
    /// leaves the usage line as it was.
    pub feedback: Option<ReturnUsage>,
}

/// §6's return figures: the budget a returned value is rendered within and
/// what this cell's return actually took of it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReturnUsage {
    /// Estimated tokens a returned value may fill this turn.
    pub budget: u64,
    /// This cell's rendered return in estimated tokens, and the fields that
    /// were paged to fit -- `None` when the cell returned nothing.
    pub this_return: Option<(u64, Vec<String>)>,
}

/// The return budget when the window is unknown, in estimated tokens.
pub const RETURN_BUDGET_UNKNOWN: u64 = 8_000;
/// The least a return may fill however full the window: a floor below which
/// paging would show a header and nothing else. The window's own overflow
/// path -- the sweep -- is the hard stop, not this figure.
pub const RETURN_BUDGET_FLOOR: u64 = 4_000;
/// The most a return may fill in one turn; above it the next page is a
/// better read than a longer one.
pub const RETURN_BUDGET_CEILING: u64 = 24_000;
/// The share of the room left in the window a return may take.
pub const RETURN_BUDGET_SHARE: u64 = 4;

/// How much of the window a returned value may fill this turn: a quarter of
/// the room left after the turn's own output cap, between
/// [`RETURN_BUDGET_FLOOR`] and [`RETURN_BUDGET_CEILING`], and
/// [`RETURN_BUDGET_UNKNOWN`] when nobody measured the window.
///
/// Ruled 2026-09-23 in place of a byte cap: a return is what the model asked
/// to read, so the budget follows the room rather than a number chosen
/// before the task, and the model is told the figure on every usage line.
#[must_use]
pub fn return_budget(used: Option<u64>, window: Option<u64>, turn_output_cap: u64) -> u64 {
    match used.zip(window) {
        Some((used, window)) => {
            let room = window.saturating_sub(used).saturating_sub(turn_output_cap);
            (room / RETURN_BUDGET_SHARE).clamp(RETURN_BUDGET_FLOOR, RETURN_BUDGET_CEILING)
        }
        None => RETURN_BUDGET_UNKNOWN,
    }
}

/// §6's user message: the yield/throw line, then `## Handles`, `## Error`,
/// `## stdout` and `## Usage`, in that order, each omitted when there is
/// nothing to say — except `## Handles`, which is never omitted and writes
/// `(none)` for an empty table.
pub fn render_result(result: &CellResult) -> String {
    render_result_with_state(result, true)
}

pub fn render_result_history(result: &CellResult) -> String {
    render_result_with_state(result, false)
}

fn render_result_with_state(result: &CellResult, include_state: bool) -> String {
    let verb = if result.error.is_some() {
        "threw"
    } else {
        "yielded"
    };
    let mut out = format!("[cell {} {verb} in {} ms]", result.cell, result.elapsed_ms);
    // The historical form only, before the first section header: what the
    // model said this cell was for outlives the handles and the output it
    // produced there. The live result never repeats it -- the model's own
    // call carries it just above, and the echo was 1 % of Sterna's fresh
    // input over 30 SWE-bench tasks (2026-09-30).
    if !include_state && let Some(description) = &result.description {
        out.push('\n');
        out.push_str(description);
    }
    if result.error.is_none()
        && let Some(reason) = &result.yield_reason
    {
        out.push('\n');
        out.push_str(reason);
    }

    if include_state {
        out.push_str("\n\n## Handles\n");
        if result.handle_table.is_empty() {
            out.push_str("(none)");
        } else {
            out.push_str(&result.handle_table);
        }
    }

    if let Some(error) = &result.error {
        out.push_str("\n\n## Error\n");
        out.push_str(&format!(
            "{}: {}",
            error.class,
            bounded_error_message(&error.message)
        ));
        if let Some((line, column)) = error.position {
            // Once, not twice: a first frame of `cell 6, line 1, column 47`
            // under its own copy read as a second location to look at.
            let at = format!("line {line}, column {column}");
            if !error
                .frames
                .first()
                .is_some_and(|frame| frame.ends_with(&at) || frame.contains(&format!("{at})")))
            {
                out.push_str(&format!("\n{at}"));
            }
        }
        for frame in error.frames.iter().take(3) {
            out.push_str(&format!("\n  at {frame}"));
        }
    }

    // Before the output: the answer is why this turn exists, and
    // a model reading top to bottom should meet it before the bookkeeping.
    if let Some(answer) = &result.ask_answer {
        out.push_str("\n\n## Answer\n");
        out.push_str(answer);
    }

    if let Some(output) = &result.output {
        out.push_str("\n\n## Output\n");
        out.push_str(output);
    }

    if let Some(stdout) = &result.stdout_tail {
        out.push_str("\n\n## stdout\n");
        out.push_str(stdout);
    }

    if include_state {
        out.push_str("\n\n## Usage\n");
        out.push_str(&render_usage_line(&result.budget));
    }

    out
}

/// The most of a thrown message `## Error` shows, in estimated tokens. Every
/// other section of a cell's result has a bound (the handle table, stdout,
/// the return budget); a message is whatever the program threw, and a
/// command's whole log thrown as one would reach the model unbounded.
pub const ERROR_MESSAGE_TOKENS: usize = 2_000;

/// `message` whole when it fits [`ERROR_MESSAGE_TOKENS`]; otherwise its start
/// and its end -- a failing command's reason is usually the last thing it
/// printed -- around one line saying how much is missing and how to get it.
fn bounded_error_message(message: &str) -> String {
    let length = message.chars().count();
    let room = ERROR_MESSAGE_TOKENS * 4;
    if length <= room {
        return message.to_string();
    }
    let head = room * 3 / 4;
    let tail = room - head;
    let start: String = message.chars().take(head).collect();
    let end: String = message.chars().skip(length - tail).collect();
    format!(
        "{start}\n[… {} characters of this message not shown · catch the error and return the part you need …]\n{end}",
        thousands((length - head - tail) as u64)
    )
}

fn render_usage_line(budget: &Budget) -> String {
    let cells = match budget.cells_cap {
        Some(cap) => format!("{}/{}", thousands(budget.cells_used), thousands(cap)),
        None => thousands(budget.cells_used),
    };
    let mut line = format!(
        "turn output cap {} · task spent {} · cells {cells}",
        thousands(budget.turn_cap),
        thousands(budget.task_used),
    );
    if let Some(usage) = &budget.feedback {
        line.push_str(&format!(" · return budget {}", thousands(usage.budget)));
        if let Some((tokens, paged)) = &usage.this_return {
            line.push_str(&format!(" · this return {}", thousands(*tokens)));
            if !paged.is_empty() {
                line.push_str(&format!(" (paged: {})", paged.join(", ")));
            }
        }
    }
    line
}

/// `n` with a comma every three digits from the right — the usage line's
/// own formatting, `model-contract.md` §6's `8,000` / `3,412` / `400,000`.
fn thousands(n: u64) -> String {
    let digits = n.to_string();
    let bytes = digits.as_bytes();
    let mut out = String::with_capacity(bytes.len() + bytes.len() / 3);
    for (i, byte) in bytes.iter().enumerate() {
        if i > 0 && (bytes.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(*byte as char);
    }
    out
}

mod protocol;
pub use protocol::{
    Extracted, MAX_DESCRIPTION_BYTES, MAX_PROGRAM_BYTES, MAX_STERNA_BLOCKS, bound_description,
    completion_text, descriptor_of, extract_program,
};

/// Feedback for a reply that announced work but supplied no action or completion.
pub const CONTINUE_WORK: &str = "No executable action or usable answer was supplied. Continue with one Sterna cell, or send the final answer as prose without a cell.";

// --- compaction: what a past turn still has to say ---------------------

/// What one pass of [`compact_conversation`] removed.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Compaction {
    /// Messages this pass shortened.
    pub messages: usize,
    /// Bytes removed from them.
    pub bytes: usize,
}

impl Compaction {
    pub fn is_empty(&self) -> bool {
        self.messages == 0
    }
}

/// The sections of a cell result that the **newest** result restates in full,
/// and which are therefore redundant in every older one.
///
/// The invariant: **a section listed here is rendered complete every turn, so
/// an older copy tells the model nothing the latest message does not.**
/// `## Handles` is the whole live table, `## Usage` and `## Budget` the
/// current figures — each is a snapshot of now, not a record of then.
/// `## Error` and `## stdout` are the opposite: they belong to the cell that
/// produced them and appear nowhere else, so they are never dropped.
const SUPERSEDED_SECTIONS: [&str; 3] = ["## Handles", "## Usage", "## Budget"];

/// Removes the superseded sections from one rendered cell result.
///
/// Pure over its input, so the claim "this is lossless" is checked by
/// comparing against a freshly rendered result rather than by inspection.
pub fn compact_result(rendered: &str) -> String {
    let mut out = String::with_capacity(rendered.len());
    // Everything before the first section header is the cell line and its
    // yield reason, which name what happened and are kept.
    let mut parts = rendered.split("\n\n## ");
    if let Some(head) = parts.next() {
        out.push_str(head);
    }
    for section in parts {
        let name = section.split('\n').next().unwrap_or_default();
        if SUPERSEDED_SECTIONS
            .iter()
            .any(|superseded| superseded.trim_start_matches("## ") == name)
        {
            continue;
        }
        out.push_str("\n\n## ");
        out.push_str(section);
    }
    out
}

/// Whether `text` is a message sterna rendered rather than one a person typed.
///
/// Only these are compacted: a person's own words are never edited, however
/// long the conversation gets.
pub fn is_rendered_result(text: &str) -> bool {
    text.starts_with("[cell ") || text.starts_with("## Handles")
}

/// Drops every superseded section from every rendered result **except the
/// most recent one**, which is the copy the others are redundant against.
///
/// Lossless by construction, and that is why it is the first thing tried: it
/// removes only text the conversation still carries somewhere else. When it
/// is not enough, [`checkpoint`] is the next rung and it is not lossless.
pub fn compact_conversation(conversation: &mut Conversation) -> Compaction {
    let last_rendered = conversation.messages.iter().rposition(|message| {
        message.content.iter().any(|block| match block {
            Block::Text(text) => is_rendered_result(text),
            Block::ToolResult { content, .. } => is_rendered_result(content),
            Block::ToolUse { .. }
            | Block::Image { .. }
            | Block::Thinking { .. }
            | Block::RedactedThinking { .. } => false,
        })
    });
    let Some(last_rendered) = last_rendered else {
        return Compaction::default();
    };

    let mut report = Compaction::default();
    for (index, message) in conversation.messages.iter_mut().enumerate() {
        if index == last_rendered {
            continue;
        }
        for block in message.content.iter_mut() {
            let text = match block {
                Block::Text(text) | Block::ToolResult { content: text, .. } => text,
                Block::ToolUse { .. }
                | Block::Image { .. }
                | Block::Thinking { .. }
                | Block::RedactedThinking { .. } => continue,
            };
            if !is_rendered_result(text) {
                continue;
            }
            let compacted = compact_result(text);
            if compacted.len() < text.len() {
                report.messages += 1;
                report.bytes += text.len() - compacted.len();
                *text = compacted;
            }
        }
    }
    if report.messages > 0 {
        drop_reasoning(conversation);
    }
    report
}

/// Removes every reasoning block. Compaction rewrote earlier turns, and
/// reasoning is valid only after the exact bytes it was produced on: sent
/// back after an edit it is refused (Claude) or wasted, so after compaction
/// the model reasons afresh, as it did before reasoning was kept at all.
pub fn drop_reasoning(conversation: &mut Conversation) {
    for message in &mut conversation.messages {
        message.content.retain(|block| {
            !matches!(
                block,
                Block::Thinking { .. } | Block::RedactedThinking { .. }
            )
        });
    }
}

/// The second rung: what one task hands itself when its conversation has to
/// be thrown away.
///
/// The invariant, and the reason sterna can do this at all: **the working set
/// survives the compaction.** A text harness's tool results *are* the
/// transcript, so discarding the transcript discards them; Sterna's live
/// objects are in the isolate and are re-listed in the next cell's handle
/// table, so what is lost here is the narration and not the work. The
/// checkpoint therefore names the handles rather than describing them — the
/// table that follows it is authoritative and complete.
///
/// Its three parts are what the task was, what went wrong last, and what it
/// still holds.
pub fn checkpoint(task: &str, live_handles: &[String], last_error: Option<&str>) -> String {
    let mut out = String::new();
    out.push_str(
        "The conversation before this point was dropped because it no longer fit. **Your \
         objects are untouched** — every handle below is live, and the handle table in the \
         next result is complete. Nothing needs re-reading or re-running.\n\n",
    );
    out.push_str("## The task\n");
    out.push_str(task.trim());

    if let Some(error) = last_error {
        out.push_str("\n\n## What went wrong last\n");
        out.push_str(error.trim());
    }

    out.push_str("\n\n## What you still hold\n");
    if live_handles.is_empty() {
        out.push_str("Nothing yet.");
    } else {
        out.push_str(&live_handles.join(", "));
    }
    out
}

#[cfg(test)]
mod tests {

    #[test]
    fn older_results_collapse_in_steps_and_the_newest_stay_whole() {
        let mut conversation = Conversation {
            system: String::new(),
            messages: (1..=7)
                .map(|n| {
                    crate::contract::Message::runtime(
                        format!("[cell {n} yielded]\nbig output {n}"),
                        format!("[cell {n} yielded]\nbig output {n}"),
                    )
                })
                .collect(),
        };
        keep_recent_results(&mut conversation, 2);
        let texts: Vec<String> = conversation
            .messages
            .iter()
            .map(|m| match &m.content[0] {
                Block::Text(text) => text.clone(),
                _ => String::new(),
            })
            .collect();
        // Seven results, two kept: five over, collapsed in a step of three.
        for (index, text) in texts.iter().enumerate() {
            let collapsed = text.ends_with(COLLAPSED);
            assert_eq!(collapsed, index < 3, "result {}: {text}", index + 1);
            assert!(text.starts_with(&format!("[cell {} yielded]", index + 1)));
        }
        let before = conversation.clone();
        keep_recent_results(&mut conversation, 0);
        assert_eq!(conversation.messages.len(), before.messages.len());
    }
    use super::*;

    /// The table's segments are byte-exact slices of the constant, each
    /// occurring once, so a replacement can neither miss nor double.
    #[test]
    fn every_variant_segment_occurs_exactly_once() {
        for variant in VARIANTS {
            assert_eq!(
                PREAMBLE.matches(variant.cells).count(),
                1,
                "segment is not a unique slice of PREAMBLE: {:?}",
                variant.cells
            );
        }
        assert_eq!(preamble_for(abi::Interface::Cells), PREAMBLE);
        assert_ne!(preamble_for(abi::Interface::Hybrid), PREAMBLE);
        assert_ne!(preamble_for(abi::Interface::Tools), PREAMBLE);
    }

    #[test]
    fn a_thrown_message_keeps_its_start_and_its_end_within_the_bound() {
        assert_eq!(
            bounded_error_message("exit 1: no such file"),
            "exit 1: no such file"
        );
        let log = format!(
            "cargo test failed\n{}error[E0425]: cannot find value `x`",
            "   Compiling crate v1.0.0\n".repeat(20_000)
        );
        let shown = bounded_error_message(&log);
        assert!(
            preview_tokens(&shown) <= ERROR_MESSAGE_TOKENS + 40,
            "{} tokens",
            preview_tokens(&shown)
        );
        assert!(shown.starts_with("cargo test failed\n"), "the start stays");
        assert!(
            shown.ends_with("error[E0425]: cannot find value `x`"),
            "the end, where the reason is, stays"
        );
        assert!(shown.contains("characters of this message not shown"));
    }

    /// The documented `chars / 4` estimate: this module may not name the
    /// runtime's preview module, which `prompt_bytes.rs` scans for.
    fn preview_tokens(text: &str) -> usize {
        text.chars().count().div_ceil(4)
    }

    #[test]
    fn cells_mode_asks_for_whole_steps_and_hybrid_keeps_direct_calls() {
        let cells = render_abi_for(HostGlobals::Every, abi::Interface::Cells);
        assert!(cells.contains(types::CELLS_GUIDANCE));
        assert!(
            !cells.contains("smallest cell"),
            "cells mode must not ask for small cells"
        );
        let hybrid = render_abi_for(HostGlobals::Every, abi::Interface::Hybrid);
        assert!(hybrid.contains(types::GUIDANCE));
    }

    #[test]
    fn a_long_plan_is_cut_at_a_line_boundary_within_the_bound() {
        let short = plan_section("step one\nstep two\n");
        assert_eq!(
            short,
            "\n## Plan (.sterna/scratch/plan.md)\nstep one\nstep two\n"
        );
        let line = "é".repeat(99) + "\n";
        let long = line.repeat(PLAN_SECTION_BYTES / line.len() + 10);
        let section = plan_section(&long);
        let body = section
            .strip_prefix("\n## Plan (.sterna/scratch/plan.md)\n")
            .unwrap()
            .strip_suffix("\n… [truncated]\n")
            .expect("a cut plan says so");
        assert!(body.len() <= PLAN_SECTION_BYTES);
        assert!(body.lines().all(|kept| kept == line.trim_end()));
    }
}
