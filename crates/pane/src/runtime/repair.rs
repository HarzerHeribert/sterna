//! A narrowly scoped edit of the most recent cell that failed to parse.

use serde::Deserialize;

pub const SOURCE_BYTE_CAP: usize = 128 * 1024;

/// Lines of the failed source shown on each side of the offending one.
const CONTEXT_LINES: u32 = 2;

/// The widest a quoted line is rendered. A minified bundle pasted into a
/// cell would otherwise put the whole program back in the turn this hint
/// exists to keep small.
const MAX_QUOTED_WIDTH: usize = 200;

/// How much shorter an accepted amendment may leave the source.
///
/// **A syntax repair is close to length-neutral; deleting code is not.**
/// Measured on real parse failures: the stacked-escape case that prompted
/// this went 10 characters to 8, an unterminated string gains 2, a missing
/// brace gains 2, a bad `\u` escape goes 8 to 1. Against that, removing one
/// ordinary statement -- `await write({path: target, content: rendered});`
/// -- is 48 characters gone and nothing back.
///
/// So the bound is on *shortening* rather than on the size of the change. A
/// bound on characters changed would have to be large enough to allow
/// re-escaping a long string, and would then be large enough to permit a
/// deletion; this refuses the deletion while leaving a whole tangled line
/// free to be rewritten, because rewriting it costs roughly what it saves.
const MAX_SHORTENING: usize = 16;

/// An absolute ceiling on the changed region, against a length-neutral
/// rewrite of the whole program. [`MAX_SHORTENING`] is the load-bearing
/// bound; this only says that a repair is a repair and not an authorship.
const MAX_CHANGED: usize = 512;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyntaxFailure {
    pub cell: u64,
    pub source: String,
    /// The parser's own position, as the `## Error` block reports it: `line`
    /// is 1-based and `column` counts characters from the start of that
    /// line. `line == 0` means the position is unknown, which is the one
    /// case [`SyntaxFailure::hint`] quotes nothing for.
    pub line: u32,
    pub column: u32,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Edit {
    cell: u64,
    replace: String,
    with: String,
}

impl SyntaxFailure {
    pub fn new(cell: u64, source: &str, line: u32, column: u32) -> Option<Self> {
        (source.len() <= SOURCE_BYTE_CAP).then(|| Self {
            cell,
            source: source.to_string(),
            line,
            column,
        })
    }

    pub fn apply(&self, json: &str) -> Result<String, String> {
        if json.len() > SOURCE_BYTE_CAP {
            return Err("pane-edit exceeds the 128 KiB limit".into());
        }
        let edit: Edit = serde_json::from_str(json)
            .map_err(|error| format!("invalid pane-edit JSON: {error}"))?;
        if edit.cell != self.cell {
            return Err(format!(
                "stale pane-edit: cell {} is not the eligible cell {}",
                edit.cell, self.cell
            ));
        }
        if edit.replace.is_empty() {
            return Err("pane-edit `replace` must be nonempty".into());
        }
        let Some(start) = self.source.find(&edit.replace) else {
            return Err("pane-edit `replace` does not occur in the failed source".into());
        };
        // Count overlapping matches too: "aa" is ambiguous inside "aaa".
        let next = start + edit.replace.chars().next().expect("nonempty").len_utf8();
        if self.source[next..].contains(&edit.replace) {
            return Err("pane-edit `replace` must occur exactly once in the failed source".into());
        }
        let end = start + edit.replace.len();
        let resulting_len = self.source.len() - edit.replace.len() + edit.with.len();
        if resulting_len > SOURCE_BYTE_CAP {
            return Err("amended source exceeds the 128 KiB limit".into());
        }
        let mut amended = String::with_capacity(resulting_len);
        amended.push_str(&self.source[..start]);
        amended.push_str(&edit.with);
        amended.push_str(&self.source[end..]);
        if amended == self.source {
            return Err("pane-edit must change the failed source".into());
        }
        Ok(amended)
    }

    /// **The hint quotes the source it is asking about.** `apply` demands
    /// text occurring exactly once in a program that never ran, so nothing
    /// of it came back in the result -- the model would be quoting from
    /// memory of what it meant to write, against bytes only this struct
    /// still holds. A cell measured on 2026-09-19 was abandoned for exactly
    /// that reason: `SyntaxError: Unterminated string, line 15, column 15`
    /// and no way to see line 15. The excerpt is bounded on both axes so a
    /// long program cannot spend the turn it is trying to save.
    pub fn hint(&self) -> String {
        let quoted = match self.excerpt() {
            Some(excerpt) => format!("Its source around line {}:\n\n{excerpt}\n\n", self.line),
            None => String::new(),
        };
        format!(
            "Nothing in cell {} ran. {quoted}Amend its source with one fence:\n```pane-edit\n{{\"cell\":{},\"replace\":\"exact text occurring once\",\"with\":\"replacement\"}}\n```",
            self.cell, self.cell
        )
    }

    /// What the mender is shown: the error, the quoted line, and the whole
    /// failed program.
    ///
    /// The whole program rather than the excerpt, because `apply` demands
    /// text occurring **exactly once** and only the full source says whether
    /// a candidate anchor does. A mender given the excerpt alone would keep
    /// proposing anchors that turn out to be ambiguous, and each refusal
    /// would cost the request this exists to save. It is bounded already:
    /// [`SyntaxFailure::new`] refuses a source over [`SOURCE_BYTE_CAP`].
    pub fn brief(&self, class: &str, message: &str) -> String {
        let quoted = match self.excerpt() {
            Some(excerpt) => format!("\n\n{excerpt}"),
            None => String::new(),
        };
        format!(
            "## The parser said\n\n{class}: {message}\nline {}, column {}{quoted}\n\n\
             ## The whole program, which never ran\n\n{}\n\n\
             ## Answer\n\nOne ```pane-edit fence for cell {}.",
            self.line, self.column, self.source, self.cell
        )
    }

    /// [`SyntaxFailure::apply`] under the bounds a *mended* amendment owes on
    /// top of an authored one.
    ///
    /// The parent writing its own `pane-edit` is the author of its program
    /// and may change it however it likes. A helper is not: it was asked to
    /// fix punctuation, and the two bounds here are what separates that from
    /// rewriting the program. See [`MAX_SHORTENING`] for why the load-bearing
    /// one is on shortening rather than on size.
    pub fn mend(&self, json: &str) -> Result<Mended, String> {
        let amended = self.apply(json)?;
        let shorter = self.source.len().saturating_sub(amended.len());
        if shorter > MAX_SHORTENING {
            return Err(format!(
                "a mend may leave the source at most {MAX_SHORTENING} characters shorter; this one removes {shorter}"
            ));
        }
        let (before, after) = changed_core(&self.source, &amended);
        if before.max(after) > MAX_CHANGED {
            return Err(format!(
                "a mend may change at most {MAX_CHANGED} characters; this one changes {}",
                before.max(after)
            ));
        }
        Ok(Mended {
            amended,
            before,
            after,
        })
    }

    /// The offending line with a caret under the reported column, a couple
    /// of lines either side for orientation, and a line-number gutter whose
    /// numbers are the ones the error message names.
    ///
    /// `None` when there is no position to point at, or when the position
    /// names a line the source does not have -- a caret under nothing is
    /// worse than no caret, and a wrong line number would send the next
    /// `pane-edit` at text that is not there.
    fn excerpt(&self) -> Option<String> {
        if self.line == 0 {
            return None;
        }
        let lines: Vec<&str> = self.source.lines().collect();
        let index = usize::try_from(self.line - 1).ok()?;
        if index >= lines.len() {
            return None;
        }
        let first = index.saturating_sub(CONTEXT_LINES as usize);
        let last = index
            .saturating_add(CONTEXT_LINES as usize)
            .min(lines.len() - 1);
        let width = (last + 1).to_string().len();
        let mut out = String::new();
        for (offset, text) in lines[first..=last].iter().enumerate() {
            let number = first + offset + 1;
            let (shown, truncated) = clip(text);
            out.push_str(&format!("{number:>width$} | {shown}"));
            if truncated {
                out.push_str(" …");
            }
            out.push('\n');
            if number == self.line as usize
                && let Some(column) = usize::try_from(self.column)
                    .ok()
                    .filter(|c| !truncated || *c < MAX_QUOTED_WIDTH)
            {
                out.push_str(&format!(
                    "{:>width$} | {}^\n",
                    "",
                    " ".repeat(column.min(shown.chars().count()))
                ));
            }
        }
        Some(out.trim_end().to_string())
    }
}

/// The entry condition, and the whole of what the caller decides.
///
/// **Only a parse failure reaches [`mend`].** `syntax_failure` is set by one
/// branch of the runtime, the `CellError::Parse` arm, and cleared at the top
/// of every model cell — so a cell that parsed and then threw is a real
/// result and stays the parent's, without anything here classifying it.
///
/// **The single attempt is structural rather than counted.** The caller runs
/// the program, asks this once, and runs what comes back; nothing in the
/// second run can reach this function again. A second mend would need an
/// edit to that call site rather than a loop wearing out a budget — which is
/// the point, because a cheap model retrying against an expensive model's
/// program without supervision is how this feature would become a runaway.
pub(crate) fn mend_after(
    state: &super::state::RuntimeState,
    failure: Option<&SyntaxFailure>,
    outcome: &super::outcome::CellOutcome,
) -> Option<MendedCell> {
    let super::outcome::CellOutcome::Threw { error, .. } = outcome else {
        return None;
    };
    mend(state, failure?, &error.class, &error.message)
}

/// Puts the mend's own line at the head of the repaired cell's output.
///
/// **A mended cell says so.** A cheap model silently rewriting an expensive
/// one's program is a worse defect than the typo it fixes: the parent would
/// reason about code it did not write and could not see. The cell's console
/// output is where it goes because that is what reaches both the model's
/// next turn and the screen, with no third rendering to keep in step.
pub(crate) fn announce(outcome: &mut super::outcome::CellOutcome, note: &str) {
    use super::outcome::CellOutcome;
    let turn = match outcome {
        CellOutcome::Returned { turn, .. }
        | CellOutcome::Yielded { turn }
        | CellOutcome::Threw { turn, .. } => turn,
    };
    turn.stdout_tail = match turn.stdout_tail.is_empty() {
        true => note.to_string(),
        false => format!("{note}\n{}", turn.stdout_tail),
    };
}

/// A repair a helper made, and the one line the parent is owed for it.
pub(crate) struct MendedCell {
    pub amended: String,
    pub note: String,
}

/// One cheap-model repair of a program that did not parse, or `None`.
///
/// **A parse failure is the one thing a cheap model may fix unsupervised,
/// and the reason holds for almost nothing else: nothing ran.** There is no
/// effect to undo, and the result is checkable by a compiler rather than by
/// a judgement — the amended source either parses or the caller is exactly
/// where it was. Every refusal here is silent and falls through to the
/// parent's own `pane-edit` offer, because a mender that cannot help must
/// cost nothing.
///
/// The caller makes exactly one of these per cell and the structure says so;
/// see [`crate::runtime::isolate::Runtime::run_cell`].
pub(crate) fn mend(
    state: &super::state::RuntimeState,
    failure: &SyntaxFailure,
    class: &str,
    message: &str,
) -> Option<MendedCell> {
    let spec = crate::helpers::lookup("mend")?;
    let (model, effort) = state.helper_route(spec.name).ok()?;

    let slot = state.begin_helper(crate::helpers::HelperRecord {
        helper: spec.name.to_string(),
        verb: spec.verb.to_string(),
        asked: format!("cell {} did not parse", failure.cell),
        ..crate::helpers::HelperRecord::default()
    });
    let token = state.token.borrow().clone();
    // A helper thinking is the cell waiting, the same as every other helper
    // call: without this the mend would spend the clock of the cell it is
    // trying to rescue.
    let away = state.away_from_js();
    let call = crate::helpers::run(
        spec,
        crate::helpers::HelperRoute {
            model: &model,
            effort,
            cap: None,
        },
        &failure.brief(class, message),
        &state.profile,
        &state.session,
        &token,
    );
    drop(away);
    let ok = call.outcome.ok;
    let answered = call.outcome.text.clone();
    state.finish_helper(slot, call);
    if !ok {
        return None;
    }

    // The same parser the parent's own repair goes through, so a mender that
    // sends two fences, or prose with a fence in it, is refused here exactly
    // as the parent would be rather than by a second reading of the protocol
    // that could drift from the first.
    let crate::prompt::Extracted::Edit(fence) = crate::prompt::extract_program(&answered) else {
        return None;
    };
    let mended = failure.mend(&fence).ok()?;
    Some(MendedCell {
        note: format!(
            "[pane:mend cell {} did not parse ({class}: {message}) · `{}` repaired {} character(s) \
             into {} · the program is otherwise unchanged and this cell is the repaired run]",
            failure.cell, spec.name, mended.before, mended.after,
        ),
        amended: mended.amended,
    })
}

/// An amendment a helper proposed and the bounds accepted, with the size of
/// what it actually changed so the parent can be told in one line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mended {
    pub amended: String,
    /// Characters of the original that the change replaced.
    pub before: usize,
    /// Characters that stand there now.
    pub after: usize,
}

/// The characters that actually differ between two texts, ignoring the
/// common head and tail.
///
/// `replace` may have to be long to occur exactly once, so its length says
/// nothing about how much of the program moved. This does: strip what both
/// texts agree on at each end and measure what is left.
fn changed_core(before: &str, after: &str) -> (usize, usize) {
    let (b, a) = (before.as_bytes(), after.as_bytes());
    let mut head = 0;
    while head < b.len().min(a.len()) && b[head] == a[head] {
        head += 1;
    }
    let mut tail = 0;
    while tail < b.len().min(a.len()) - head && b[b.len() - 1 - tail] == a[a.len() - 1 - tail] {
        tail += 1;
    }
    (b.len() - head - tail, a.len() - head - tail)
}

/// One quoted line, clipped to [`MAX_QUOTED_WIDTH`] characters. Returns
/// whether anything was dropped, because a caret past the clip would point
/// at the wrong character.
fn clip(line: &str) -> (String, bool) {
    let mut shown = String::new();
    for (count, character) in line.chars().enumerate() {
        if count == MAX_QUOTED_WIDTH {
            return (shown, true);
        }
        // A tab in a quoted line puts the caret at the wrong column on every
        // terminal that expands it differently; one space is the width the
        // caret arithmetic above assumes.
        shown.push(if character == '\t' { ' ' } else { character });
    }
    (shown, false)
}
