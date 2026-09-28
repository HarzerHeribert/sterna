//! What one cell produced — `runtime-contract.md` §1's two endings, §5's
//! third, §4's rollout line, and §9's terminal response and trajectory.
//!
//! **A throw is a result, not an error.** [`CellOutcome::Threw`] carries the
//! same turn a yield would have carried — the elapsed time, the rendered
//! handle table, the captured stdout and the rollout record — and the
//! bindings the cell completed before throwing are in that table. Nothing
//! here is a `Result`, because none of the three endings is a failure of the
//! runtime.

use std::collections::BTreeMap;

use serde::ser::{SerializeMap, Serializer};
use serde::{Deserialize, Serialize};

use crate::runtime::handles::Provenance;
use crate::runtime::preview::{self, ErrorValue, Value};

/// Everything a cell hands back whatever way it ended.
#[derive(Debug, Clone, PartialEq)]
pub struct CellTurn {
    pub elapsed_ms: u64,
    /// The turn's whole rendering of the handle table, from
    /// `handles::render_table`. The isolate renders no handle itself.
    pub table: String,
    /// The last [`crate::runtime::preview::STDOUT_TOKEN_CAP`] tokens of the
    /// cell's `console` output.
    pub stdout_tail: String,
    /// How many tokens of `console` output were dropped ahead of
    /// [`stdout_tail`](Self::stdout_tail).
    pub stdout_dropped_tokens: usize,
    /// Why the cell yielded on purpose — `yieldNow(reason)`'s reason, or the
    /// response cap's sentence (`runtime-contract.md` §9.3, §9.2). `None` for
    /// a fall-off and for every ending that is not a yield. It rides the turn
    /// rather than [`CellOutcome::Yielded`] because callers outside this
    /// package match `Yielded { turn }` exhaustively.
    pub yield_reason: Option<String>,
    /// What the cell said with `answer(text)`, and the **only** thing that
    /// ends the person's task. `None` for every cell that was still working
    /// -- which is every cell that did not say otherwise.
    pub answer: Option<String>,
    /// The question the cell put to the person with `ask`, which ended it.
    /// The session answers it and feeds the answer back on the next turn;
    /// `None` is every cell that asked nothing.
    pub ask: Option<crate::ask::Question>,
    /// The one rollout line this cell owes — appended by the wiring package,
    /// never by this one.
    pub record: CellRecord,
    /// The model's own plan as it stood when the cell ended, newest write
    /// wins. Empty until a cell calls `todo.write`. It rides the turn rather
    /// than the record because it is task state the screen re-renders every
    /// cell, not a line the rollout owes.
    pub plan: Vec<PlanItem>,
    /// The canonical typed result of each capability call, in call order,
    /// captured only for a frame lowered from direct provider tool calls.
    ///
    /// It rides the turn because the turn is the frame's own value and dies
    /// with it: `CellTurn` is not `Serialize`, so this cannot reach the
    /// rollout, whose rule is programs and previews and never objects
    /// (`runtime-contract.md` §4). An authored cell captures nothing and this
    /// stays empty — the model already holds those results as live handles.
    pub capability_results: Vec<String>,
    /// What rendering `table` as a delta cost and saved this turn
    /// (`smarter-cheaper-roadmap.md`, *Observation delta*).
    pub observation: crate::runtime::observation::ObservationStats,
}

/// One item of the model's own plan — `todo.write`'s unit.
///
/// The invariant: **a status is one of three states and there is no fourth.**
/// A plan the model can put arbitrary text in the status of is a plan nothing
/// downstream can render or count, so an unknown status is refused at the
/// binding rather than stored and puzzled over later.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanItem {
    pub text: String,
    pub status: PlanStatus,
}

/// A plan item's state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlanStatus {
    Pending,
    Active,
    Done,
}

impl PlanStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            PlanStatus::Pending => "pending",
            PlanStatus::Active => "active",
            PlanStatus::Done => "done",
        }
    }

    /// The status named by `text`, or `None` for anything else. The three
    /// spellings are the whole vocabulary and the model is told them.
    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "pending" => Some(PlanStatus::Pending),
            "active" => Some(PlanStatus::Active),
            "done" => Some(PlanStatus::Done),
            _ => None,
        }
    }

    /// The mark this status carries in a rendered plan.
    pub fn mark(self) -> &'static str {
        match self {
            PlanStatus::Pending => "[ ]",
            PlanStatus::Active => "[~]",
            PlanStatus::Done => "[x]",
        }
    }
}

/// How a cell ended.
#[derive(Debug, Clone, PartialEq)]
pub enum CellOutcome {
    /// It ran off the end, or asked to hand back. The model gets the table
    /// and another turn, and the isolate stays warm (§1, §9.3).
    Yielded { turn: CellTurn },
    /// It executed a top-level `return`. Text and scalars end the task; a
    /// structured value is bounded notebook output (§1, §9.2).
    Returned {
        value: Value,
        terminal: Terminal,
        turn: CellTurn,
    },
    /// It threw. The turn slot a yield would have used carries the error
    /// instead, and the bindings made before the throw are in `turn.table`
    /// (§5).
    Threw { error: ErrorValue, turn: CellTurn },
}

/// A top-level return rendered at the isolate boundary. Text is a terminal
/// response; anything else is notebook output for the next turn, kept whole
/// here and **paged, never cut, when it is rendered** ([`Terminal::render_within`]).
///
/// The invariant, ruled 2026-09-23 after session tls9up-7rz spent fourteen
/// cells re-squeezing data it already held: **a returned value reaches the
/// model as the value, within a budget the usage line names.** A field over
/// its share of that budget is shown up to a line boundary and followed by
/// one cursor line saying how to read on; no field degrades to its type
/// name, and no return is cut at a byte count. The only bound the isolate
/// still applies is [`TERMINAL_WALK_CAP`], a memory limit on the walk, and a
/// field the walk stopped inside says so in the same cursor position.
#[derive(Debug, Clone, PartialEq)]
pub enum Terminal {
    /// A returned string: the response, verbatim.
    Text(String),
    /// A returned array, number, boolean, `null`, `Map` or `Set`: its JSON
    /// with values. `cut` when the walk stopped at [`TERMINAL_WALK_CAP`].
    Json { text: String, cut: bool },
    /// A returned object: its top-level fields kept apart, in the order the
    /// program wrote them, so each can be rendered as what it is and paged
    /// on its own.
    Fields(Vec<ReturnedField>),
}

/// One top-level field of a returned object.
#[derive(Debug, Clone, PartialEq)]
pub struct ReturnedField {
    pub name: String,
    pub body: FieldBody,
    /// `false` when the walk stopped inside this field at
    /// [`TERMINAL_WALK_CAP`]; the rendering says so where a cursor line
    /// would go.
    pub whole: bool,
}

/// What a field holds, read for how it is best shown.
#[derive(Debug, Clone, PartialEq)]
pub enum FieldBody {
    /// A string: shown as its text, block-wise. A `File.excerpt` lands here
    /// and is paged by its own line numbers.
    Text(String),
    /// An array of strings: one per line, unquoted.
    Lines(Vec<String>),
    /// Anything else: its JSON, pretty-printed when it is long enough to
    /// page.
    Json(String),
}

/// The memory bound on reading a returned value out of the isolate: bytes of
/// rendered text per return. It is not a context budget -- that is
/// [`Terminal::render_within`]'s argument -- and a value under it is never
/// shortened here.
pub const TERMINAL_WALK_CAP: usize = 1024 * 1024;

/// The smallest share a field is given when a return is paged, in estimated
/// tokens: enough for an excerpt's header and a screen of lines, so a field
/// squeezed by its neighbours still shows what it is.
pub const FIELD_FLOOR_TOKENS: usize = 600;

/// The name prefix of a field the session prefetched rather than the program
/// returned (`session/returned.rs`); its cursor line names `read`.
pub const PREFETCHED_MARK: &str = "[prefetched]";

/// A compact JSON object no longer than this is rendered on one line as the
/// program wrote it -- `{"matches":3,"files":2}` reads better than three
/// headed sections.
const ONE_LINE_JSON: usize = 240;

/// Compact JSON longer than this is pretty-printed so it has line
/// boundaries to page on.
const PRETTY_JSON_ABOVE: usize = 160;

/// What [`Terminal::render_within`] produced, and what it cost.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rendered {
    /// The `## Output` body.
    pub text: String,
    /// Its estimated tokens.
    pub tokens: usize,
    /// The fields that were paged, by name, in order.
    pub paged: Vec<String>,
}

impl Terminal {
    /// The value whole: the response as the person reads it and the rollout
    /// keeps it. No budget applies here; the model's feedback goes through
    /// [`Self::render_within`].
    pub fn render(&self) -> String {
        self.render_within(usize::MAX).text
    }

    /// The value for the model, within `budget` estimated tokens.
    ///
    /// Under the budget every field is whole. Over it, each field gets a
    /// share by water-filling -- small fields whole, large ones levelled --
    /// with [`FIELD_FLOOR_TOKENS`] as every field's floor, and a field over
    /// its share is shown to a line boundary and followed by one cursor
    /// line naming the rest and how to reach it.
    pub fn render_within(&self, budget: usize) -> Rendered {
        match self {
            Terminal::Text(text) => Rendered {
                tokens: preview::estimate_tokens(text),
                text: text.clone(),
                paged: Vec::new(),
            },
            Terminal::Json { text, cut } => {
                let field = ReturnedField {
                    name: String::new(),
                    body: FieldBody::Json(text.clone()),
                    whole: !cut,
                };
                let paged = page_fields(std::slice::from_ref(&field), budget);
                let (text, tokens) = paged[0].clone();
                Rendered {
                    text,
                    tokens,
                    paged: Vec::new(),
                }
            }
            Terminal::Fields(fields) => {
                if let Some(line) = one_line_object(fields) {
                    return Rendered {
                        tokens: preview::estimate_tokens(&line),
                        text: line,
                        paged: Vec::new(),
                    };
                }
                let paged = page_fields(fields, budget);
                let mut out = String::new();
                let mut tokens = 0;
                let mut names = Vec::new();
                for (field, (body, cost)) in fields.iter().zip(paged) {
                    if !out.is_empty() {
                        out.push('\n');
                    }
                    if body.contains('\n') || field.body_is_block() {
                        out.push_str(&format!("### {}\n{body}", field.name));
                    } else {
                        out.push_str(&format!("{}: {body}", field.name));
                    }
                    tokens += cost;
                    if cost < field.tokens() {
                        names.push(field.name.clone());
                    }
                }
                Rendered {
                    text: out,
                    tokens,
                    paged: names,
                }
            }
        }
    }
}

impl ReturnedField {
    /// The field's body as text, whole: an excerpt as its block, lines one
    /// per line, JSON pretty when it is long enough to page.
    pub fn text(&self) -> String {
        match &self.body {
            FieldBody::Text(text) => text.clone(),
            FieldBody::Lines(lines) => lines.join("\n"),
            FieldBody::Json(json) => pretty_json(json),
        }
    }

    fn tokens(&self) -> usize {
        preview::estimate_tokens(&self.text())
    }

    fn body_is_block(&self) -> bool {
        matches!(&self.body, FieldBody::Lines(lines) if lines.len() > 1)
    }

    /// The one line that follows a paged field: how many lines are not
    /// shown and how to reach them, in the shape the field itself suggests.
    fn cursor_line(&self, shown_lines: usize, total_lines: usize, shown_text: &str) -> String {
        let rest = total_lines.saturating_sub(shown_lines);
        match &self.body {
            // A prefetched file is not held: `read` is the way to the rest.
            FieldBody::Text(_) if self.name.starts_with(PREFETCHED_MARK) => format!(
                "[+{} lines not shown · read({{path: {:?}}}) holds the whole file]",
                preview::thousands(rest as u64),
                self.name[PREFETCHED_MARK.len()..].trim()
            ),
            FieldBody::Text(text) if is_excerpt(text) => {
                // The last numbered line kept is where the next page starts.
                let last = shown_text
                    .lines()
                    .rev()
                    .find_map(excerpt_line_number)
                    .unwrap_or(0);
                let end = excerpt_end(text).unwrap_or(last + rest);
                let remaining = end.saturating_sub(last);
                format!(
                    "[+{} lines not shown · call .excerpt({{start: {}, lines: {}}}) on the same File]",
                    preview::thousands(remaining as u64),
                    last + 1,
                    remaining.max(1)
                )
            }
            FieldBody::Text(_) => format!(
                "[+{} lines not shown · return this field alone, or a slice of it, to read on]",
                preview::thousands(rest as u64)
            ),
            FieldBody::Lines(_) => format!(
                "[+{} of {} entries not shown · .slice({shown_lines}) shows the rest]",
                preview::thousands(rest as u64),
                preview::thousands(total_lines as u64)
            ),
            FieldBody::Json(_) => format!(
                "[+{} lines of JSON not shown · return a narrower value to read on]",
                preview::thousands(rest as u64)
            ),
        }
    }
}

/// A small object on one line, as the program wrote it, or `None` when any
/// field is a block or the line would be long.
fn one_line_object(fields: &[ReturnedField]) -> Option<String> {
    let mut out = String::from("{");
    for (index, field) in fields.iter().enumerate() {
        if !field.whole {
            return None;
        }
        let value = match &field.body {
            FieldBody::Text(text) => {
                if text.contains('\n') {
                    return None;
                }
                serde_json::Value::String(text.clone()).to_string()
            }
            FieldBody::Lines(lines) => {
                if lines.iter().any(|line| line.contains('\n')) {
                    return None;
                }
                serde_json::Value::Array(
                    lines
                        .iter()
                        .map(|line| serde_json::Value::String(line.clone()))
                        .collect(),
                )
                .to_string()
            }
            FieldBody::Json(json) => json.clone(),
        };
        if index > 0 {
            out.push(',');
        }
        out.push_str(&serde_json::Value::String(field.name.clone()).to_string());
        out.push(':');
        out.push_str(&value);
        if out.len() > ONE_LINE_JSON {
            return None;
        }
    }
    out.push('}');
    Some(out)
}

/// Compact JSON pretty-printed once it is long enough to need line
/// boundaries; text that is not JSON (a walk that stopped) stays as it is.
fn pretty_json(json: &str) -> String {
    if json.len() <= PRETTY_JSON_ABOVE {
        return json.to_string();
    }
    serde_json::from_str::<serde_json::Value>(json)
        .ok()
        .and_then(|value| serde_json::to_string_pretty(&value).ok())
        .unwrap_or_else(|| json.to_string())
}

fn is_excerpt(text: &str) -> bool {
    text.starts_with("[lines ")
}

/// `  12 | text` → 12.
fn excerpt_line_number(line: &str) -> Option<usize> {
    let (number, _) = line.split_once(" | ")?;
    number.trim().parse().ok()
}

/// `[lines 1-400 of 1200]` → 400.
fn excerpt_end(text: &str) -> Option<usize> {
    let header = text.lines().next()?;
    let range = header.strip_prefix("[lines ")?;
    let (range, _) = range.split_once(" of ")?;
    let (_, end) = range.split_once('-')?;
    end.parse().ok()
}

/// Each field's rendering and cost within `budget`: whole when the sum fits,
/// otherwise water-filled with [`FIELD_FLOOR_TOKENS`] as the floor.
fn page_fields(fields: &[ReturnedField], budget: usize) -> Vec<(String, usize)> {
    let texts: Vec<String> = fields.iter().map(ReturnedField::text).collect();
    let sizes: Vec<usize> = texts.iter().map(|t| preview::estimate_tokens(t)).collect();
    let total: usize = sizes.iter().sum();
    let shares: Vec<usize> = if total <= budget {
        sizes.clone()
    } else {
        water_fill(&sizes, budget)
    };
    fields
        .iter()
        .zip(texts)
        .zip(shares)
        .map(|((field, text), share)| {
            let mut out = if share >= preview::estimate_tokens(&text) {
                text
            } else {
                page(field, &text, share)
            };
            if !field.whole {
                out.push_str(&format!(
                    "\n[the walk of this value stopped at {} bytes · return a narrower view of it]",
                    preview::thousands(TERMINAL_WALK_CAP as u64)
                ));
            }
            let tokens = preview::estimate_tokens(&out);
            (out, tokens)
        })
        .collect()
}

/// Shares that sum to about `budget`: fields that fit under the level are
/// whole, the rest are levelled, and none goes under the floor (or its own
/// size). Smallest first, so a small field is never squeezed by a large one.
fn water_fill(sizes: &[usize], budget: usize) -> Vec<usize> {
    let mut order: Vec<usize> = (0..sizes.len()).collect();
    order.sort_by_key(|&i| sizes[i]);
    let mut shares = vec![0; sizes.len()];
    let mut remaining = budget;
    for (rank, &i) in order.iter().enumerate() {
        let left = sizes.len() - rank;
        let level = remaining / left;
        let share = if sizes[i] <= level {
            sizes[i]
        } else {
            level.max(FIELD_FLOOR_TOKENS.min(sizes[i]))
        };
        shares[i] = share;
        remaining = remaining.saturating_sub(share);
    }
    shares
}

/// `text` to a line boundary within `share` tokens, at least one line, then
/// the field's cursor line.
fn page(field: &ReturnedField, text: &str, share: usize) -> String {
    let lines: Vec<&str> = text.lines().collect();
    let total = lines.len();
    let mut kept = 0;
    let mut chars = 0;
    for line in &lines {
        // Counted as the joined text will be estimated, so the share is met
        // in the same units the usage line reports.
        let after = chars + line.chars().count() + 1;
        if kept > 0 && after.div_ceil(4) > share {
            break;
        }
        chars = after;
        kept += 1;
    }
    // An excerpt's `[next: …]` footer describes the whole block, so it goes
    // with the lines it was written for; the cursor line replaces it.
    let shown: Vec<&str> = lines[..kept]
        .iter()
        .copied()
        .filter(|line| !line.starts_with("[next: call .excerpt("))
        .collect();
    let mut out = shown.join("\n");
    out.push('\n');
    out.push_str(&field.cursor_line(kept, total, &out));
    out
}

impl CellOutcome {
    pub fn turn(&self) -> &CellTurn {
        match self {
            CellOutcome::Yielded { turn }
            | CellOutcome::Returned { turn, .. }
            | CellOutcome::Threw { turn, .. } => turn,
        }
    }

    pub fn kind(&self) -> CellOutcomeKind {
        match self {
            CellOutcome::Yielded { .. } => CellOutcomeKind::Yielded,
            CellOutcome::Returned { .. } => CellOutcomeKind::Returned,
            CellOutcome::Threw { .. } => CellOutcomeKind::Threw,
        }
    }

    /// Whether the task is over, and it is over **only because the cell
    /// said so** with `answer(text)`.
    ///
    /// It used to be read off the returned value's type -- an array or an
    /// object was notebook output, anything else was the final answer. That
    /// made `return result.stdout`, written to look at a command's output,
    /// publish that output as the answer and end the session; a returned
    /// value's shape says nothing about whether the work is finished. A
    /// throw never ends a task however it answered: something failed after
    /// the claim, and the claim has not survived it.
    #[must_use]
    pub fn ends_the_task(&self) -> bool {
        match self {
            CellOutcome::Returned { turn, .. } | CellOutcome::Yielded { turn } => {
                turn.answer.is_some()
            }
            CellOutcome::Threw { .. } => false,
        }
    }

    /// What the cell answered, when it did.
    #[must_use]
    pub fn answer(&self) -> Option<&str> {
        match self {
            CellOutcome::Returned { turn, .. } | CellOutcome::Yielded { turn } => {
                turn.answer.as_deref()
            }
            CellOutcome::Threw { .. } => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum CellOutcomeKind {
    Yielded,
    Returned,
    Threw,
}

/// One rollout line — `runtime-contract.md` §4, whose shape this struct is.
///
/// It records the model's **program** and the handles' **previews**, never a
/// live object and never a payload: a resumed session rebuilds nothing by
/// re-running a cell, so there is nothing here for it to re-run.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CellRecord {
    pub cell: u64,
    /// The model's own TypeScript, as it wrote it — never the erased or
    /// wrapped JavaScript, which is Sterna's spelling and not the model's.
    pub source: String,
    /// The one line the model wrote about what this cell is for, in the
    /// person's language — `docs/workbench.md`.
    ///
    /// **It is the model's stated intention, not a record of what ran.**
    /// `calls` is the record; when the two disagree the trajectory is the
    /// truth. Absent for a cell whose model said nothing and for
    /// every rollout row written before this field existed, and absent is
    /// never an error: the notebook falls back to the cell's first source
    /// line, which is what it drew before.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub outcome: CellOutcomeKind,
    pub handles: Vec<HandleRecord>,
    /// §9.4's trajectory: every call that actually ran in this cell, in
    /// order. An untaken branch ran nothing and records nothing; the answer
    /// itself is on the `turn` line, never here.
    pub calls: Vec<CallRecord>,
}

/// One live handle as the rollout records it.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct HandleRecord {
    pub name: String,
    #[serde(rename = "type")]
    pub type_name: String,
    pub preview: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provenance: Option<Provenance>,
}

/// One call of the trajectory — `runtime-contract.md` §9.4.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CallRecord {
    /// The registry name.
    pub tool: String,
    /// The arguments **as checked**: a path is the resolved path the child
    /// was given, never the program's spelling. A refused call carries only
    /// what was admitted before the refusing argument.
    pub args: BTreeMap<String, String>,
    /// Exact source material made visible by an inspection call before a
    /// semantic edit. Absent for ordinary calls and older rollout rows.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub evidence: Option<SourceEvidence>,
    /// The program the model actually wrote, when sterna proved the command's
    /// meaning and ran a capability instead.
    ///
    /// One word, never the command line: `semantic-command-lifting.md` asks
    /// the ledger to explain that a shell-shaped request was lifted without
    /// persisting its payload, and `tool` already names what really ran.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lifted_from: Option<String>,
    /// The child's exit status, for a call that ran a process (`bash`,
    /// `checks.run`, a search or a read). Absent for an in-process call,
    /// whose `Some(0)` is a convention rather than an observation.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    /// The cell of an earlier pure call with the same tool, the same checked
    /// arguments and a byte-identical result. The call still ran — the file
    /// may have changed — and the hash is what decided it had not.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repeat_of: Option<u64>,
    /// The one-line message a failed call threw, so a frame that isolates
    /// each call can still answer the provider with what went wrong. Absent
    /// for a call that ended `ok`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    pub ended: Ended,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceEvidence {
    pub path: String,
    pub sha256: String,
    pub complete: bool,
    pub ranges: Vec<SourceRange>,
    pub omissions: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceRange {
    pub path: String,
    pub start: usize,
    pub end: usize,
    pub role: String,
}

/// How one call ended: `"ok"`, `{"threw": "<class>"}` or
/// `{"denied": "<rule>"}` on the line. A cancelled call is a throw (§9.1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Ended {
    Ok,
    Threw { class: String },
    Denied { rule: String },
}

impl Serialize for Ended {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Ended::Ok => serializer.serialize_str("ok"),
            Ended::Threw { class } => {
                let mut map = serializer.serialize_map(Some(1))?;
                map.serialize_entry("threw", class)?;
                map.end()
            }
            Ended::Denied { rule } => {
                let mut map = serializer.serialize_map(Some(1))?;
                map.serialize_entry("denied", rule)?;
                map.end()
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    #[test]
    fn a_rollout_line_serialises_to_the_contracts_own_shape() {
        let record = CellRecord {
            cell: 4,
            source: "const hits = await grep({});\n".into(),
            description: None,
            outcome: CellOutcomeKind::Yielded,
            handles: vec![HandleRecord {
                name: "hits".into(),
                type_name: "Grep.Match[]".into(),
                preview: "n=1195".into(),
                provenance: Some(Provenance {
                    tool: "grep".into(),
                    args: BTreeMap::from([("pattern".to_string(), "IntegrationId".to_string())]),
                    sha256: "9f2c".into(),
                    pure: true,
                }),
            }],
            calls: vec![
                CallRecord {
                    tool: "grep".into(),
                    args: BTreeMap::from([("path".to_string(), "/tmp/root".to_string())]),
                    evidence: None,
                    lifted_from: None,
                    exit_code: None,
                    repeat_of: None,
                    error: None,
                    ended: Ended::Ok,
                },
                CallRecord {
                    tool: "bash".into(),
                    args: BTreeMap::new(),
                    evidence: None,
                    lifted_from: None,
                    exit_code: None,
                    repeat_of: None,
                    error: None,
                    ended: Ended::Denied {
                        rule: "no allow".into(),
                    },
                },
                CallRecord {
                    tool: "read".into(),
                    args: BTreeMap::new(),
                    evidence: None,
                    lifted_from: None,
                    exit_code: None,
                    repeat_of: None,
                    error: None,
                    ended: Ended::Threw {
                        class: "Cancelled".into(),
                    },
                },
            ],
        };
        let json = serde_json::to_string(&record).unwrap();
        assert!(json.contains(r#""cell":4"#), "{json}");
        assert!(json.contains(r#""outcome":"yielded""#), "{json}");
        assert!(json.contains(r#""type":"Grep.Match[]""#), "{json}");
        assert!(json.contains(r#""pure":true"#), "{json}");
        assert!(
            json.contains(r#""calls":[{"tool":"grep","args":{"path":"/tmp/root"},"ended":"ok"}"#),
            "{json}"
        );
        assert!(json.contains(r#""ended":{"denied":"no allow"}"#), "{json}");
        assert!(json.contains(r#""ended":{"threw":"Cancelled"}"#), "{json}");
        // The optional per-call facts are absent from a line that has none.
        for absent in ["exit_code", "repeat_of", "\"error\""] {
            assert!(!json.contains(absent), "{absent} leaked into {json}");
        }
    }

    /// The three optional facts appear only when set, so an older rollout
    /// row and a row for a plain in-process call read the same.
    #[test]
    fn a_call_line_carries_its_exit_code_repeat_and_error_only_when_set() {
        let call = CallRecord {
            tool: "bash".into(),
            args: BTreeMap::new(),
            evidence: None,
            lifted_from: None,
            exit_code: Some(7),
            repeat_of: Some(3),
            error: Some("`read` failed with exit 1: no such file".into()),
            ended: Ended::Ok,
        };
        let json = serde_json::to_string(&call).unwrap();
        assert!(json.contains(r#""exit_code":7"#), "{json}");
        assert!(json.contains(r#""repeat_of":3"#), "{json}");
        assert!(json.contains(r#""error":"`read` failed"#), "{json}");
    }

    fn turn() -> CellTurn {
        CellTurn {
            elapsed_ms: 0,
            table: String::new(),
            stdout_tail: String::new(),
            stdout_dropped_tokens: 0,
            yield_reason: None,
            answer: None,
            ask: None,
            record: CellRecord {
                cell: 1,
                source: String::new(),
                description: None,
                outcome: CellOutcomeKind::Yielded,
                handles: Vec::new(),
                calls: Vec::new(),
            },
            plan: Vec::new(),
            capability_results: Vec::new(),
            observation: Default::default(),
        }
    }

    /// **No returned value ends the task, whatever its type.** A cell ends
    /// it only by saying so with `answer(text)`, and a throw ends nothing
    /// even when the cell answered before it failed.
    #[test]
    fn only_an_answer_ends_the_task_and_no_returned_value_does() {
        let answered = || CellTurn {
            answer: Some("done".into()),
            ..turn()
        };
        for value in [
            Value::Null,
            Value::Number(3.0),
            Value::string("done"),
            Value::object(vec![("a".to_string(), Value::Number(1.0))]),
        ] {
            let terminal = Terminal::Json {
                text: "…".into(),
                cut: false,
            };
            assert!(
                !CellOutcome::Returned {
                    value: value.clone(),
                    terminal: terminal.clone(),
                    turn: turn(),
                }
                .ends_the_task(),
                "a returned {value:?} must not end the task"
            );
            assert!(
                CellOutcome::Returned {
                    value,
                    terminal,
                    turn: answered(),
                }
                .ends_the_task()
            );
        }
        assert!(!CellOutcome::Yielded { turn: turn() }.ends_the_task());
        assert!(CellOutcome::Yielded { turn: answered() }.ends_the_task());
        assert_eq!(
            CellOutcome::Yielded { turn: answered() }.answer(),
            Some("done")
        );
        assert!(
            !CellOutcome::Threw {
                error: ErrorValue::default(),
                turn: answered(),
            }
            .ends_the_task(),
            "a throw after an answer has not survived its own cell"
        );
    }

    fn excerpt(lines: usize) -> String {
        let mut text = format!("[lines 1-{lines} of {lines}]\n");
        for n in 1..=lines {
            text.push_str(&format!(
                "{n:>4} | line {n} of the file, with enough words to cost tokens\n"
            ));
        }
        text.push_str("[end of file]\n");
        text
    }

    fn field(name: &str, body: FieldBody) -> ReturnedField {
        ReturnedField {
            name: name.into(),
            body,
            whole: true,
        }
    }

    #[test]
    fn a_small_object_renders_on_one_line_as_the_program_wrote_it() {
        let terminal = Terminal::Fields(vec![
            field("matches", FieldBody::Json("3".into())),
            field("files", FieldBody::Json("2".into())),
            field(
                "names",
                FieldBody::Lines(vec!["a.rs".into(), "b.rs".into()]),
            ),
        ]);
        assert_eq!(
            terminal.render(),
            r#"{"matches":3,"files":2,"names":["a.rs","b.rs"]}"#
        );
        assert_eq!(Terminal::Text("verbatim".into()).render(), "verbatim");
    }

    #[test]
    fn a_return_under_the_budget_arrives_whole_field_by_field() {
        // Cell 4 of session tls9up-7rz, the shape that was cut to 2 KB: an
        // excerpt beside a count. At the ceiling it arrives entire.
        let terminal = Terminal::Fields(vec![
            field("readme", FieldBody::Text(excerpt(240))),
            field("count", FieldBody::Json("240".into())),
        ]);
        let rendered = terminal.render_within(24_000);
        assert!(rendered.paged.is_empty(), "{:?}", rendered.paged);
        assert!(
            rendered
                .text
                .starts_with("### readme\n[lines 1-240 of 240]\n"),
            "{}",
            rendered.text
        );
        assert!(
            rendered.text.contains(" 240 | line 240 of the file"),
            "{}",
            rendered.text
        );
        assert!(
            rendered.text.contains("[end of file]\n\ncount: 240"),
            "{}",
            rendered.text
        );
        assert!(
            !rendered.text.contains("string"),
            "no field degrades to its type name"
        );
        assert!(rendered.tokens > 3_000, "{}", rendered.tokens);
    }

    #[test]
    fn a_field_over_its_share_is_paged_at_a_line_with_a_cursor() {
        let terminal = Terminal::Fields(vec![
            field("readme", FieldBody::Text(excerpt(400))),
            field("count", FieldBody::Json("400".into())),
        ]);
        let rendered = terminal.render_within(1_000);
        assert_eq!(rendered.paged, vec!["readme".to_string()]);
        assert!(rendered.tokens <= 1_100, "{}", rendered.tokens);
        // Whole lines only, then the one cursor line naming the next start.
        let body = rendered.text.strip_prefix("### readme\n").expect("heading");
        let last_numbered = body
            .lines()
            .rfind(|line| line.contains(" | line "))
            .expect("some lines kept");
        let last: usize = last_numbered
            .split(" | ")
            .next()
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        assert!(last > 1 && last < 400, "{last}");
        assert!(
            last_numbered.ends_with("cost tokens"),
            "cut at a line: {last_numbered}"
        );
        let cursor = format!(
            "[+{} lines not shown · call .excerpt({{start: {}, lines: {}}}) on the same File]",
            400 - last,
            last + 1,
            400 - last
        );
        assert!(rendered.text.contains(&cursor), "{}", rendered.text);
        assert!(
            !rendered.text.contains("[next: call"),
            "the footer went with its lines"
        );
        // The small field beside it is untouched.
        assert!(rendered.text.ends_with("\ncount: 400"), "{}", rendered.text);
    }

    #[test]
    fn every_field_keeps_its_floor_and_its_own_cursor_shape() {
        let lines: Vec<String> = (0..500).map(|n| format!("entry number {n}")).collect();
        let json = serde_json::to_string(
            &(0..300)
                .map(|n| serde_json::json!({"n": n, "name": format!("item {n}")}))
                .collect::<Vec<_>>(),
        )
        .unwrap();
        let terminal = Terminal::Fields(vec![
            field("prose", FieldBody::Text("a\n".repeat(2_000))),
            field("entries", FieldBody::Lines(lines)),
            field("data", FieldBody::Json(json)),
        ]);
        let rendered = terminal.render_within(1_500);
        assert_eq!(rendered.paged, vec!["prose", "entries", "data"]);
        for name in ["prose", "entries", "data"] {
            let section = rendered
                .text
                .split(&format!("### {name}\n"))
                .nth(1)
                .unwrap()
                .split("\n### ")
                .next()
                .unwrap();
            // The floor, less the one line the cut may stop short by: at a
            // 1,500 budget three fields levelled without a floor would get
            // 500 each, and this is what says they get 600.
            assert!(
                preview::estimate_tokens(section) >= FIELD_FLOOR_TOKENS - 20,
                "{name} squeezed under its floor: {} tokens",
                preview::estimate_tokens(section)
            );
        }
        assert!(
            rendered
                .text
                .contains("lines not shown · return this field alone"),
            "{}",
            rendered.text
        );
        assert!(
            rendered.text.contains("of 500 entries not shown · .slice("),
            "{}",
            rendered.text
        );
        assert!(
            rendered.text.contains("lines of JSON not shown"),
            "{}",
            rendered.text
        );
        // Pretty JSON has a line to page on; the first kept line is its bracket.
        assert!(
            rendered.text.contains("### data\n[\n  {"),
            "{}",
            rendered.text
        );
    }

    #[test]
    fn a_walk_that_stopped_says_so_instead_of_pretending() {
        let terminal = Terminal::Fields(vec![ReturnedField {
            name: "big".into(),
            body: FieldBody::Text("x".repeat(100)),
            whole: false,
        }]);
        let rendered = terminal.render();
        assert!(
            rendered.contains("stopped at 1,048,576 bytes"),
            "{rendered}"
        );
        let bare = Terminal::Json {
            text: "[1,2".into(),
            cut: true,
        };
        assert!(bare.render().contains("stopped at"), "{}", bare.render());
    }
}
