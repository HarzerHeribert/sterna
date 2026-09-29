//! Shortening an oversized command result by rules, and saying what that cost.
//!
//! Above `[limits] reduce_above_tokens`, [`super::reduce_rules`] removes what
//! a predicate can recognise for free -- passing test lines, a page of
//! `Compiling`, a warning repeated forty times. The program reads the ruled
//! text behind one line saying what it left out; `stdout` and `stderr` stay
//! complete on the result. An output no rule recognises is left as it is.

use std::rc::Rc;

use super::preview::{estimate_tokens, thousands};
use super::state::RuntimeState;
use crate::tools::invoke::ToolResult;

/// What became of a reduction: nothing was needed or no rule applied, or the
/// ruled text with its provenance line.
pub(super) enum Reduction {
    NotAttempted,
    Made(String),
}

/// The one line a reduction carries about itself.
///
/// **A summary that looks complete is never re-checked.** The argument that
/// made a reduction safe -- `stdout` and `stderr` stay whole, so the parent
/// can always read the original -- only holds if the parent knows it has
/// reason to, and a reduction with no provenance gives it none: three
/// failures summarised out of two hundred read the same as three out of
/// three.
///
/// One line, because this text is spent on every reduced result. It leads
/// rather than trails so a reader frames the content before trusting it, and
/// it opens with a marker no build log produces, so a reduction *of* a log
/// that discusses reductions cannot be mistaken for it.
fn lossiness_line(input: &str, output: &str, note: Option<&str>) -> String {
    let lines = |text: &str| {
        let n = text.lines().count();
        format!(
            "{} line{}",
            thousands(n as u64),
            if n == 1 { "" } else { "s" }
        )
    };
    let bytes = |text: &str| thousands(text.len() as u64);
    let note = note.map(|note| format!(" · {note}")).unwrap_or_default();
    format!(
        "[sterna:reduction {} / {} bytes → {} / {} bytes{note} · \
         `stdout` and `stderr` on this result are complete and unchanged]",
        lines(input),
        bytes(input),
        lines(output),
        bytes(output),
    )
}

/// [`lossiness_line`] joined to the reduction it describes.
fn with_lossiness(input: &str, output: &str, note: Option<&str>) -> String {
    format!("{}\n{output}", lossiness_line(input, output, note))
}

/// A command result over `[limits] reduce_above_tokens`, shortened by the
/// rules. Only [`typed_result`]'s command-output arm reaches here, so
/// `read`, `context`, `edit`, `grep` and `glob` -- every shape a model edits
/// or quotes from -- are excluded structurally.
pub(super) fn reduce_oversized(result: &ToolResult, state: &Rc<RuntimeState>) -> Reduction {
    let threshold = state.reduce_above_tokens();
    let tokens = estimate_tokens(&result.stdout) + estimate_tokens(&result.stderr);
    if tokens <= threshold {
        return Reduction::NotAttempted;
    }
    // Both streams, because a build writes its failures to whichever it
    // likes and the reduction is of the output, not of one pipe.
    let mut text = result.stdout.clone();
    text.push_str(&result.stderr);
    reduce_text(text, state)
}

/// The same rules for a returned field the decision model read as a log
/// (`runtime/isolate/returned.rs`): the size gate above is the model's
/// answer, so none is applied here.
pub(super) fn reduce_asked(text: String, state: &Rc<RuntimeState>) -> Reduction {
    reduce_text(text, state)
}

fn reduce_text(original: String, state: &Rc<RuntimeState>) -> Reduction {
    let ruled = crate::runtime::reduce_rules::apply(&original);
    if ruled.applied.is_empty() {
        return Reduction::NotAttempted;
    }
    let note = format!("rules: {}", ruled.applied.join(", "));
    let reduced = with_lossiness(&original, &ruled.text, Some(&note));
    state.count_reduction(|stats| {
        stats.ruled += 1;
        stats.bytes_in += original.len() as u64;
        stats.bytes_out += reduced.len() as u64;
    });
    Reduction::Made(reduced)
}
