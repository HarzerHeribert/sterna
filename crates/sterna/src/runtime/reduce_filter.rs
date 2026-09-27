//! Checking a filter the model wrote before its output is believed.
//!
//! A model-written filter is only safe because what it returns is checked
//! against what it was given. The check is the whole reason the design is an
//! improvement rather than a new way to be wrong: a reducer that retypes its
//! evidence can misquote it, and a filter that *selects* lines cannot —
//! provided something confirms it selected rather than composed.
//!
//! **Every line of a filter's output must occur verbatim in its input. There
//! is no exception, and that is deliberate.** The obvious design gives the
//! filter a way to say "N lines removed here", and that one carve-out is a
//! hole the size of the whole guarantee: anything a filter wants to invent,
//! it prefixes with an elision marker. So the filter returns selected lines
//! and nothing else, and [`elision`] is computed here, from the difference
//! between what went in and what came out, by code that had no part in
//! choosing.

use std::collections::HashSet;

use super::preview::{estimate_tokens, thousands};

/// The info string of the one fence a reducer's answer may carry.
///
/// The grammar is `prompt::protocol`'s, deliberately: three backticks at the
/// start of a line, an exactly matching info string, and a closing line of
/// exactly three backticks. It is parsed here rather than there because that
/// parser reads the *parent's* executable channel, where `sterna-filter` is
/// not a thing a turn may contain and must keep being refused.
const FENCE: &str = "sterna-filter";

/// The largest filter this accepts. A filter is a few lines; anything at
/// this size is a program that wandered in, and running it is the one thing
/// this module exists to be careful about.
const MAX_FILTER_BYTES: usize = 8 * 1024;

/// What a reducer answered: the filter to run, and the prose beside it.
///
/// **The two halves are kept apart from here to the result.** A filter's
/// output is evidence — every line of it occurred in the input, and
/// [`validate`] is what makes that true. Prose is the model's reading of the
/// evidence, which is the half a filter genuinely cannot produce ("two
/// hundred failures, but only three distinct shapes") and the half that can
/// be wrong. A reader who cannot tell them apart has the worse of both.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Answer {
    pub filter: String,
    /// Everything outside the fence, trimmed. Empty is ordinary.
    pub prose: String,
}

/// Pull the one `sterna-filter` fence out of a reducer's answer.
///
/// The error is the sentence the model is shown on its retry, for the same
/// reason [`Rejected::sentence`] is.
pub fn parse(answer: &str) -> Result<Answer, String> {
    let lines: Vec<&str> = answer.lines().collect();
    let mut filter: Option<String> = None;
    let mut prose: Vec<&str> = Vec::new();
    let mut index = 0usize;
    while index < lines.len() {
        let Some(info) = lines[index].strip_prefix("```") else {
            prose.push(lines[index]);
            index += 1;
            continue;
        };
        let info = info.trim();
        let mut body: Vec<&str> = Vec::new();
        let mut end = index + 1;
        while end < lines.len() && lines[end] != "```" {
            body.push(lines[end]);
            end += 1;
        }
        let closed = end < lines.len();
        if info == FENCE {
            if !closed {
                return Err(format!(
                    "your ```{FENCE} fence was never closed. Close it with a line of \
                     exactly three backticks."
                ));
            }
            if filter.is_some() {
                return Err(format!(
                    "you sent more than one ```{FENCE} fence. Send exactly one, \
                     holding the whole filter."
                ));
            }
            filter = Some(body.join("\n"));
        }
        // A fence of any other language is an example and is prose.
        index = if closed { end + 1 } else { end };
    }

    let Some(filter) = filter else {
        return Err(format!(
            "your answer carried no ```{FENCE} fence. Answer with one fence holding a \
             JavaScript function of the text, and any notes outside it."
        ));
    };
    if filter.trim().is_empty() {
        return Err(format!("your ```{FENCE} fence was empty."));
    }
    if filter.len() > MAX_FILTER_BYTES {
        return Err(format!(
            "your filter is {} bytes against a limit of {}. A filter is a few lines \
             that select; it is not a program.",
            thousands(filter.len() as u64),
            thousands(MAX_FILTER_BYTES as u64),
        ));
    }
    Ok(Answer {
        filter,
        prose: prose.join("\n").trim().to_string(),
    })
}

/// Why a filter's output was not accepted.
///
/// The text is handed back to the model on its one retry, so each variant
/// says what to do differently rather than only what was wrong.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Rejected {
    /// Nothing, or only whitespace.
    Empty,
    /// A line that was not in the input. The first one, with its number.
    Composed { line: String, at: usize },
    /// More lines out than in: a filter must select, never repeat.
    Longer { output: usize, input: usize },
    /// A line the caller required is missing.
    Dropped(String),
    /// It ran, it selected, and it is still too big to be worth sending.
    TooLarge { tokens: usize, threshold: usize },
}

impl Rejected {
    /// The sentence the model is shown on its retry.
    #[must_use]
    pub fn sentence(&self) -> String {
        match self {
            Self::Empty => "your filter returned nothing. It must return the lines that \
                 carry the failures, selected from the text it was given."
                .to_string(),
            Self::Composed { line, at } => format!(
                "line {at} of your filter's output does not occur in the input: {line:?}. \
                 A filter selects whole lines from the text it is given and never \
                 composes, reformats or trims them. Return the line as it stands or \
                 leave it out."
            ),
            Self::Longer { output, input } => format!(
                "your filter returned {} lines from an input of {}. A filter removes \
                 lines; it never repeats them.",
                thousands(*output as u64),
                thousands(*input as u64),
            ),
            Self::Dropped(line) => format!(
                "your filter dropped a line the caller requires: {line:?}. Every line \
                 listed as one that must be kept has to appear in the output exactly as \
                 it is written there."
            ),
            Self::TooLarge { tokens, threshold } => format!(
                "your filter's output is still about {} tokens against a budget of {}. \
                 Select fewer lines: keep the distinct failures and drop the rest.",
                thousands(*tokens as u64),
                thousands(*threshold as u64),
            ),
        }
    }
}

/// How a filter's line is compared to the input's.
///
/// **Trailing whitespace only.** A line differing solely in trailing spaces
/// carries the same evidence and no editor or pipe preserves them reliably,
/// so requiring them would reject correct filters for nothing. Leading
/// whitespace is *not* forgiven: indentation is meaning in a diff, a nested
/// test report and a stack trace, and a filter that re-indents a line has
/// changed what it says about where it came from.
fn comparable(line: &str) -> &str {
    line.trim_end()
}

/// Check a filter's output against the text it was given.
///
/// `must_survive` is the caller's own list of lines that carry failures; the
/// model was shown all of them, so a filter that drops one is wrong rather
/// than unlucky.
pub fn validate(
    input: &str,
    output: &str,
    must_survive: &[String],
    threshold_tokens: usize,
) -> Result<(), Rejected> {
    if output.trim().is_empty() {
        return Err(Rejected::Empty);
    }

    let input_lines: HashSet<&str> = input.lines().map(comparable).collect();
    let output_lines: Vec<&str> = output.lines().collect();

    let input_count = input.lines().count();
    if output_lines.len() > input_count {
        return Err(Rejected::Longer {
            output: output_lines.len(),
            input: input_count,
        });
    }

    for (index, line) in output_lines.iter().enumerate() {
        if !input_lines.contains(comparable(line)) {
            return Err(Rejected::Composed {
                line: (*line).to_string(),
                at: index + 1,
            });
        }
    }

    let kept: HashSet<&str> = output_lines.iter().copied().map(comparable).collect();
    for required in must_survive {
        if !kept.contains(comparable(required)) {
            return Err(Rejected::Dropped(required.clone()));
        }
    }

    let tokens = estimate_tokens(output);
    if tokens > threshold_tokens {
        return Err(Rejected::TooLarge {
            tokens,
            threshold: threshold_tokens,
        });
    }
    Ok(())
}

/// What the filter removed, counted here rather than claimed there.
///
/// The filter cannot say this itself — a line it emits has to be a line it
/// was given, so it has no way to write a sentence about its own work. That
/// is the point: the number a reader relies on is computed by code that did
/// not choose what to keep.
#[must_use]
pub fn elision(input: &str, output: &str) -> String {
    let input_lines = input.lines().count();
    let output_lines = output.lines().count();
    let removed = input_lines.saturating_sub(output_lines);
    format!(
        "… {} of {} lines not selected by the filter; `stdout` and `stderr` on this \
         result are complete and unchanged …",
        thousands(removed as u64),
        thousands(input_lines as u64),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const LOG: &str = "\
Compiling thing v1.0
test a ... ok
test b ... ok
test c ... FAILED
assertion failed: left == right
test result: FAILED. 2 passed; 1 failed
";

    fn must() -> Vec<String> {
        vec!["test c ... FAILED".to_string()]
    }

    #[test]
    fn a_filter_that_selects_real_lines_is_accepted() {
        let output = "test c ... FAILED\nassertion failed: left == right\n";
        assert_eq!(validate(LOG, output, &must(), 4_000), Ok(()));
    }

    /// The guarantee. A line that reads like a perfectly good summary, and
    /// never occurred, is refused on that ground alone.
    #[test]
    fn a_line_the_input_never_held_is_refused_however_true_it_looks() {
        let output = "test c ... FAILED\n1 test failed out of 3\n";
        let Err(Rejected::Composed { line, at }) = validate(LOG, output, &must(), 4_000) else {
            panic!("a composed line must be refused");
        };
        assert_eq!(line, "1 test failed out of 3");
        assert_eq!(at, 2);
        assert!(line_is_plausible(&line));
    }

    /// A composed line is refused for not occurring, never for looking
    /// wrong — so the check cannot be talked out of by a better-worded lie.
    fn line_is_plausible(line: &str) -> bool {
        !line.is_empty()
    }

    #[test]
    fn an_elision_marker_is_no_longer_a_way_in() {
        // The shape a carve-out would have admitted.
        let output = "test c ... FAILED\n… and 40 similar failures elsewhere …\n";
        assert!(
            matches!(
                validate(LOG, output, &must(), 4_000),
                Err(Rejected::Composed { .. })
            ),
            "a filter has no marker of its own, so this is simply a composed line",
        );
    }

    #[test]
    fn a_filter_that_drops_a_required_line_is_refused() {
        let output = "assertion failed: left == right\n";
        assert_eq!(
            validate(LOG, output, &must(), 4_000),
            Err(Rejected::Dropped("test c ... FAILED".to_string()))
        );
    }

    #[test]
    fn trailing_space_is_forgiven_and_indentation_is_not() {
        let trailing = "test c ... FAILED   \nassertion failed: left == right\n";
        assert_eq!(validate(LOG, trailing, &must(), 4_000), Ok(()));

        let reindented = "  test c ... FAILED\n";
        assert!(
            matches!(
                validate(LOG, reindented, &must(), 4_000),
                Err(Rejected::Composed { .. })
            ),
            "indentation is meaning in a diff and a trace",
        );
    }

    #[test]
    fn a_filter_that_repeats_lines_is_refused() {
        let output = LOG.repeat(2);
        assert!(matches!(
            validate(LOG, &output, &must(), 40_000),
            Err(Rejected::Longer { .. })
        ));
    }

    #[test]
    fn an_empty_filter_is_refused() {
        assert_eq!(validate(LOG, "  \n ", &must(), 4_000), Err(Rejected::Empty));
    }

    #[test]
    fn a_selection_still_over_budget_is_refused_with_its_numbers() {
        let big: String = (0..2_000).map(|n| format!("line {n}\n")).collect();
        let Err(Rejected::TooLarge { tokens, threshold }) = validate(&big, &big, &[], 100) else {
            panic!("an over-budget selection is refused");
        };
        assert!(tokens > threshold);
    }

    #[test]
    fn every_refusal_tells_the_model_what_to_do_differently() {
        for rejected in [
            Rejected::Empty,
            Rejected::Composed {
                line: "x".into(),
                at: 1,
            },
            Rejected::Longer {
                output: 2,
                input: 1,
            },
            Rejected::Dropped("y".into()),
            Rejected::TooLarge {
                tokens: 10,
                threshold: 5,
            },
        ] {
            let sentence = rejected.sentence();
            assert!(sentence.len() > 40, "{rejected:?} says too little");
            assert!(
                sentence.contains("filter"),
                "{rejected:?} must name what it is about",
            );
        }
    }

    #[test]
    fn one_fence_is_the_filter_and_everything_else_is_prose() {
        let answer = parse(
            "Three distinct failures.\n\
             ```sterna-filter\n(t) => t\n```\n\
             All of them in one crate.",
        )
        .expect("one fence");
        assert_eq!(answer.filter, "(t) => t");
        assert_eq!(
            answer.prose,
            "Three distinct failures.\nAll of them in one crate."
        );
    }

    /// A fenced example in another language is prose, exactly as it is in the
    /// parent's own channel.
    #[test]
    fn a_fence_of_another_language_is_not_a_filter() {
        let error = parse("```js\n(t) => t\n```").expect_err("no filter fence");
        assert!(error.contains("no ```sterna-filter fence"), "{error}");
    }

    #[test]
    fn two_filters_are_ambiguous_and_neither_runs() {
        let error =
            parse("```sterna-filter\na\n```\n```sterna-filter\nb\n```").expect_err("two fences");
        assert!(error.contains("more than one"), "{error}");
    }

    #[test]
    fn an_unclosed_fence_says_so_rather_than_running_half_a_filter() {
        let error = parse("```sterna-filter\n(t) => t").expect_err("unclosed");
        assert!(error.contains("never closed"), "{error}");
    }

    #[test]
    fn a_filter_larger_than_a_filter_is_refused() {
        let huge = "x".repeat(MAX_FILTER_BYTES + 1);
        let error = parse(&format!("```sterna-filter\n{huge}\n```")).expect_err("oversized");
        assert!(error.contains("not a program"), "{error}");
    }

    #[test]
    fn the_elision_is_counted_here_not_claimed_by_the_filter() {
        let output = "test c ... FAILED\n";
        let note = elision(LOG, output);
        assert!(note.contains("5 of 6 lines"), "{note}");
        assert!(note.contains("complete and unchanged"));
    }
}
