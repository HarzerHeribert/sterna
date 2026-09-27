//! The shape of a large returned field, read by the decision model before
//! the value is rendered, and the reducer for the one shape where less is
//! more.
//!
//! The user's reading (2026-09-23): *sometimes reduction is enrichment, by
//! not flooding context with what nobody needs -- maybe Jev can decide
//! that.* So it does. For every field at or over `[helpers]
//! reduce_above_tokens`, Jev is asked what kind of text it is (`decide::
//! field_shape`, 2 s, one question). A `log` goes to the reducer's ladder --
//! rules first, then this task's filter cache, then one cheap request -- and
//! what comes back is the failures with a lossiness line saying what was
//! removed; every other shape is paged as it stands. No answer, a refused
//! reduction or `shadow` mode leaves the field exactly as `render_within`
//! would have shown it, so Sterna without Jev is dumber here and never broken.

use super::*;
use crate::runtime::outcome::{CellTurn, FieldBody, ReturnedField, Terminal};
use crate::sandbox::profile::Access;
use std::path::{Path, PathBuf};

/// A returned value shown to the model and the screen: its large fields'
/// shapes asked for and a log reduced ([`shape`]), what it points at fetched
/// when it is not enough ([`enrich`]), then rendered within this turn's return budget (`TaskSpend::render_return`), with the usage line
/// carrying the figures. The screen shows the same text the model reads.
#[expect(
    clippy::too_many_arguments,
    reason = "the one place a returned value meets the session, the task, the budget and the profile"
)]
pub(super) fn show(
    session: &Session<'_>,
    runtime: &Runtime,
    task_state: &mut TaskState,
    budget: &mut TaskSpend,
    profile: &Profile,
    turn: &CellTurn,
    terminal: &Terminal,
    result: &mut CellResult,
) -> String {
    let shaped = shape(session, runtime, task_state, terminal);
    let return_budget = budget.return_budget as usize;
    let enriched = enrich(session, task_state, profile, turn, return_budget, shaped);
    let text = budget.render_return(&enriched);
    result.budget.feedback = Some(budget.return_usage());
    result.output = Some(text.clone());
    text
}

/// The confidence at or above which a `log` answer sends the field to the
/// reducer. Below it the field is paged: a reduction is lossy on purpose,
/// and an uncertain reading is not a reason to lose anything.
const REDUCE_ABOVE: f64 = 0.6;

/// The terminal as the model will read it: each large field's shape asked
/// for, and a log reduced when the answer and the mode allow it.
pub(super) fn shape(
    session: &Session<'_>,
    runtime: &Runtime,
    task_state: &mut TaskState,
    terminal: &Terminal,
) -> Terminal {
    let config = session.config();
    let Terminal::Fields(fields) = terminal else {
        return terminal.clone();
    };
    if !config.helpers.reduce_returns {
        return terminal.clone();
    }
    let Some(model) = config.decisions.model.clone() else {
        return terminal.clone();
    };
    if config.decisions.mode == crate::config::DecisionMode::Off {
        return terminal.clone();
    }
    let threshold = config.helpers.reduce_above_tokens;
    // Acts in `shadow` too since 2026-09-23: a log shortened with the whole
    // value still bound stops nothing, so it is advice-shaped, not a gate.
    // Every large field's question at once: each waits up to Jev's timeout,
    // and asked in turn six fields held the turn for twelve seconds
    // (2026-09-23) -- no Escape reaches a call like that.
    let answers: Vec<Option<Result<crate::decide::FieldShape, crate::decide::DecideError>>> =
        std::thread::scope(|scope| {
            let asked: Vec<_> = fields
                .iter()
                .map(|field| {
                    let text = field.text();
                    let large = crate::runtime::preview::estimate_tokens(&text) >= threshold;
                    let model = model.as_str();
                    let name = field.name.as_str();
                    large.then(|| {
                        scope.spawn(move || crate::decide::field_shape(model, name, &text))
                    })
                })
                .collect();
            asked
                .into_iter()
                .map(|handle| handle.map(|h| h.join().expect("a shape question does not panic")))
                .collect()
        });
    let shaped = fields
        .iter()
        .zip(answers)
        .map(|(field, answer)| match answer {
            None => field.clone(),
            Some(answer) => shape_field(answer, runtime, task_state, field),
        })
        .collect();
    Terminal::Fields(shaped)
}

fn shape_field(
    answer: Result<crate::decide::FieldShape, crate::decide::DecideError>,
    runtime: &Runtime,
    task_state: &mut TaskState,
    field: &ReturnedField,
) -> ReturnedField {
    let text = field.text();
    let answer = match answer {
        Ok(answer) => answer,
        Err(error) => {
            task_state.field_shapes.push(serde_json::json!({
                "field": field.name,
                "failed": error.to_string(),
            }));
            return field.clone();
        }
    };
    let wants_reduction =
        answer.choice == crate::decide::FIELD_LOG && answer.confidence >= REDUCE_ABOVE;
    let reduced = wants_reduction
        .then(|| runtime.reduce_returned(&text))
        .flatten();
    task_state.field_shapes.push(serde_json::json!({
        "field": field.name,
        "choice": answer.choice,
        "confidence": answer.confidence,
        "latency_ms": answer.latency_ms,
        "reduced": reduced.is_some(),
    }));
    match reduced {
        Some(reduced) => ReturnedField {
            name: field.name.clone(),
            body: FieldBody::Text(format!(
                "{reduced}\n[the whole value is still live in your bindings; return a slice of it to see more]"
            )),
            whole: field.whole,
        },
        None => field.clone(),
    }
}

// --- enough to go on, or fetch what the return points at -------------------

/// The `enough` noul at or below which the return is read as not enough and
/// what it names is fetched. Above it nothing is fetched: a prefetch spends
/// window on files the model may not want, and an uncertain no is not a
/// reason to spend it.
const ENOUGH_BELOW: f64 = 0.4;
/// The most files one return prefetches.
const PREFETCH_FILES: usize = 3;
/// The most estimated tokens one prefetched file may take, whatever the room.
const PREFETCH_FILE_TOKENS: usize = 6_000;
/// The least room left in the return budget for a prefetch to be worth
/// asking about: under this a file would arrive as a header and a cursor.
const PREFETCH_MIN_ROOM: usize = 1_500;
/// The most bytes of a candidate file read at all.
const PREFETCH_MAX_BYTES: u64 = 1024 * 1024;
use crate::runtime::outcome::PREFETCHED_MARK as PREFETCHED;

/// The return with what it points at fetched behind it, when the decision
/// model says the return alone is not enough: the files the return names,
/// read-only, in-project, within what is left of the return budget, each as
/// a numbered block under `### [prefetched] path`. Never a helper turn, and
/// never a handle -- the model reads a prefetched file and `read`s it itself
/// when it wants to hold it.
pub(super) fn enrich(
    session: &Session<'_>,
    task_state: &mut TaskState,
    profile: &Profile,
    turn: &CellTurn,
    return_budget: usize,
    terminal: Terminal,
) -> Terminal {
    let config = session.config();
    if !config.helpers.prefetch_returns {
        return terminal;
    }
    let Some(model) = config.decisions.model.clone() else {
        return terminal;
    };
    if config.decisions.mode == crate::config::DecisionMode::Off {
        return terminal;
    }
    let Terminal::Fields(mut fields) = terminal else {
        return terminal;
    };
    let rendered = Terminal::Fields(fields.clone()).render_within(return_budget);
    let room = return_budget.saturating_sub(rendered.tokens);
    let candidates = candidate_paths(&rendered.text, profile, &turn.table);
    if candidates.is_empty() || room < PREFETCH_MIN_ROOM {
        return Terminal::Fields(fields);
    }
    let names: Vec<String> = candidates
        .iter()
        .map(|(relative, _)| relative.clone())
        .collect();
    let glance: Vec<crate::decide::FieldGlance> = fields
        .iter()
        .map(|field| {
            let text = field.text();
            crate::decide::FieldGlance {
                name: field.name.clone(),
                tokens: crate::runtime::preview::estimate_tokens(&text),
                head: text.lines().take(4).collect::<Vec<_>>().join("\n"),
            }
        })
        .collect();
    let step = turn
        .plan
        .iter()
        .find(|item| item.status == crate::runtime::outcome::PlanStatus::Active)
        .map(|item| item.text.clone());
    let answer =
        match crate::decide::enough(&model, &task_state.task, step.as_deref(), &glance, &names) {
            Ok(answer) => answer,
            Err(error) => {
                task_state.prefetches.push(serde_json::json!({
                    "cell": turn.record.cell,
                    "candidates": names,
                    "failed": error.to_string(),
                }));
                return Terminal::Fields(fields);
            }
        };
    let wants = answer.noul <= ENOUGH_BELOW;
    let acting = config.decisions.mode == crate::config::DecisionMode::On;
    let mut fetched = Vec::new();
    if wants && acting {
        let mut room = room;
        for (relative, resolved) in &candidates {
            if room < PREFETCH_MIN_ROOM / 3 {
                break;
            }
            let Some(field) = prefetch(relative, resolved, room.min(PREFETCH_FILE_TOKENS)) else {
                continue;
            };
            room = room.saturating_sub(crate::runtime::preview::estimate_tokens(&field.text()));
            fetched.push(relative.clone());
            fields.push(field);
        }
    }
    task_state.prefetches.push(serde_json::json!({
        "cell": turn.record.cell,
        "enough": answer.noul,
        "latency_ms": answer.latency_ms,
        "candidates": names,
        "prefetched": fetched,
        "would_prefetch": wants && !acting,
    }));
    Terminal::Fields(fields)
}

/// The in-project files `text` names that the program does not already
/// hold: every token with a path shape that the profile admits for reading,
/// exists as a regular file under the root, and is not a `File` in the
/// handle table -- first mention first, at most [`PREFETCH_FILES`].
fn candidate_paths(text: &str, profile: &Profile, table: &str) -> Vec<(String, PathBuf)> {
    let root = profile.root();
    let mut seen = std::collections::BTreeSet::new();
    let mut found = Vec::new();
    for raw in text.split(|c: char| {
        c.is_whitespace()
            || matches!(
                c,
                '"' | '\'' | '`' | '(' | ')' | '[' | ']' | '{' | '}' | '<' | '>' | ',' | ';'
            )
    }) {
        let token = raw.trim_matches(|c: char| matches!(c, '.' | ':' | '*' | '#'));
        // `path:12` and `path:12:3` name a line in a file.
        let token = token.split_once(':').map_or(token, |(path, _)| path);
        if token.len() < 3
            || token.len() > 200
            || token.contains("://")
            || token.starts_with('-')
            || token.starts_with('$')
            || !looks_like_a_path(token)
            || !seen.insert(token.to_string())
        {
            continue;
        }
        let Ok(resolved) = profile.check("read", Access::Read, Path::new(token)) else {
            continue;
        };
        if !resolved.starts_with(root) || !resolved.is_file() {
            continue;
        }
        let relative = resolved
            .strip_prefix(root)
            // One spelling on every platform: the model and `read()` both
            // name project paths with `/`.
            .map(|p| p.to_string_lossy().replace('\\', "/"))
            .unwrap_or_else(|_| token.to_string());
        if table.contains(&relative) || table.contains(token) {
            continue;
        }
        found.push((relative, resolved));
        if found.len() == PREFETCH_FILES {
            break;
        }
    }
    found
}

/// A token with a directory separator, or a file extension of one to six
/// word characters after a stem.
fn looks_like_a_path(token: &str) -> bool {
    if token.contains('/') {
        return !token.ends_with('/');
    }
    match token.rsplit_once('.') {
        Some((stem, extension)) => {
            !stem.is_empty()
                && (1..=6).contains(&extension.len())
                && extension.chars().all(|c| c.is_ascii_alphanumeric())
                && stem.chars().any(|c| c.is_ascii_alphabetic())
        }
        None => false,
    }
}

/// One prefetched file as a returned field: a numbered block of its first
/// lines within `tokens`, headed and footed so the model knows what it is
/// reading and how to get the rest.
fn prefetch(relative: &str, resolved: &Path, tokens: usize) -> Option<ReturnedField> {
    let size = std::fs::metadata(resolved).ok()?.len();
    if size > PREFETCH_MAX_BYTES {
        return None;
    }
    let text = String::from_utf8(std::fs::read(resolved).ok()?).ok()?;
    let lines: Vec<&str> = text.lines().collect();
    if lines.is_empty() {
        return None;
    }
    let width = lines.len().to_string().len();
    let mut body = String::new();
    let mut chars = 0;
    let mut shown = 0;
    for (index, line) in lines
        .iter()
        .enumerate()
        .take(crate::runtime::excerpt::DEFAULT_LINES)
    {
        let line: String = line
            .chars()
            .take(crate::runtime::excerpt::MAX_LINE_UNITS)
            .collect();
        let row = format!("{:>width$} | {line}\n", index + 1);
        chars += row.chars().count();
        if shown > 0 && chars.div_ceil(4) > tokens {
            break;
        }
        body.push_str(&row);
        shown += 1;
    }
    let mut out = format!(
        "[lines 1-{shown} of {} · prefetched, not held]\n",
        lines.len()
    );
    out.push_str(&body);
    if shown < lines.len() {
        out.push_str(&format!(
            "[read({{path: {relative:?}}}) holds the whole file]\n"
        ));
    } else {
        out.push_str("[end of file]\n");
    }
    Some(ReturnedField {
        name: format!("{PREFETCHED} {relative}"),
        body: FieldBody::Text(out),
        whole: true,
    })
}
