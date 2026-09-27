//! The deterministic rung of the reduction ladder: what rules can remove
//! before a model is asked to remove anything.
//!
//! The invariant: **this is a reduction, not a summary.** Every line it emits
//! occurred in the input, in the order it occurred. The only text it adds is
//! an elision marker saying how much was removed and by which rule, so a
//! reader who doubts the reduction can say exactly what is missing. It never
//! paraphrases, never counts something the output did not already count, and
//! never removes a line that looks like a failure.
//!
//! **Why rules first.** `reduce_oversized` spends a cheap model's tokens only
//! to remove the parent's, and for the output that actually trips it -- a
//! `cargo test` run, a build log, a linker failure -- most of the bulk is
//! removable by a predicate. A thousand `test foo ... ok` lines are already
//! counted by the `test result:` line beneath them, so removing them loses
//! nothing and costs nothing. A request is worth making for what is left,
//! not for what a rule could have deleted for free.
//!
//! Each rule below names what it drops and what it keeps in one sentence. A
//! rule that recognises nothing does nothing, and an input no rule recognises
//! comes back byte-identical, so the rung is invisible where it does not
//! help.

/// A run of identical lines is folded only when it is at least this long.
/// Two adjacent duplicates are not noise, they are output.
const FOLD_RUN: usize = 3;

/// Substrings that make a line ineligible for a *dropping* rule, whatever
/// else it looks like.
///
/// Folding does not consult this list: a run of identical lines folded with
/// its own count keeps every word it had, which is the form `REDUCER`'s
/// preamble asks for anyway ("and how many times it repeated").
///
/// It is also the caller's own definition of a failure line, which is what
/// [`super::reduce`] builds a filter's `must_survive` list from: the rung
/// that may not drop these and the validator that may not accept a filter
/// dropping them are then reading one list rather than two that could drift.
pub(crate) const NEVER_DROP: [&str; 10] = [
    "error",
    "Error",
    "ERROR",
    "panic",
    "FAILED",
    "failures",
    "Traceback",
    "assertion",
    "warning:",
    "thread '",
];

/// The outcome of the deterministic rung.
pub struct Reduced {
    /// The reduced text. Equal to the input when no rule fired.
    pub text: String,
    /// The rules that removed something, in the order they are defined.
    /// Empty means the input came back untouched.
    pub applied: Vec<&'static str>,
}

impl Reduced {
    /// Whether any rule fired. A caller that wants to know if the rung is
    /// worth reporting asks this rather than comparing strings.
    #[must_use]
    pub fn changed(&self) -> bool {
        !self.applied.is_empty()
    }
}

/// Whether a line carries one of [`NEVER_DROP`]'s markers.
pub(crate) fn never_drop(line: &str) -> bool {
    NEVER_DROP.iter().any(|marker| line.contains(marker))
}

/// Drops `test <name> ... ok` and `... ignored` lines, keeps every other
/// line including `test result:`, which already carries their counts.
fn is_passing_test_line(line: &str) -> bool {
    let line = line.trim_end();
    line.starts_with("test ")
        && (line.ends_with(" ... ok") || line.ends_with(" ... ignored"))
        && !never_drop(line)
}

/// Drops cargo's per-crate progress chatter, keeps `Running`, `Finished`,
/// and everything that is not a progress verb.
fn is_build_progress_line(line: &str) -> bool {
    const VERBS: [&str; 7] = [
        "Compiling ",
        "Downloading ",
        "Downloaded ",
        "Updating ",
        "Fresh ",
        "Blocking ",
        "Installing ",
    ];
    let trimmed = line.trim_start();
    VERBS.iter().any(|verb| trimmed.starts_with(verb)) && !never_drop(line)
}

/// Apply every rule, in order, and report which ones fired.
///
/// The rules that drop run first and the fold runs last, so a fold reports
/// the runs that survive rather than runs of lines that were about to go.
#[must_use]
pub fn apply(text: &str) -> Reduced {
    let mut applied = Vec::new();
    let lines: Vec<&str> = text.lines().collect();

    let mut kept: Vec<&str> = Vec::with_capacity(lines.len());
    let mut passing = 0usize;
    let mut progress = 0usize;
    for line in lines {
        if is_passing_test_line(line) {
            passing += 1;
        } else if is_build_progress_line(line) {
            progress += 1;
        } else {
            kept.push(line);
        }
    }

    let mut out = String::with_capacity(text.len());
    let mut folded = 0usize;
    let mut index = 0usize;
    while index < kept.len() {
        let line = kept[index];
        let mut run = 1usize;
        while index + run < kept.len() && kept[index + run] == line {
            run += 1;
        }
        out.push_str(line);
        out.push('\n');
        if run >= FOLD_RUN {
            out.push_str(&format!(
                "    … the line above repeated {run} times in all …\n"
            ));
            folded += run - 1;
        } else {
            for repeat in kept.iter().skip(index + 1).take(run - 1) {
                out.push_str(repeat);
                out.push('\n');
            }
        }
        index += run;
    }

    if passing > 0 {
        applied.push("passing-test-lines");
        out.push_str(&format!(
            "… {passing} passing or ignored test lines removed; the `test result:` lines above count them …\n"
        ));
    }
    if progress > 0 {
        applied.push("build-progress");
        out.push_str(&format!(
            "… {progress} build progress lines removed (Compiling, Downloading, Updating, Fresh) …\n"
        ));
    }
    if folded > 0 {
        applied.push("repeated-lines");
    }

    if applied.is_empty() {
        // Byte-identical, not merely equal: an input no rule recognised must
        // reach the next rung exactly as it arrived, trailing newline and all.
        return Reduced {
            text: text.to_string(),
            applied,
        };
    }
    Reduced { text: out, applied }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CARGO_RUN: &str = "\
   Compiling sterna v0.1.0 (/Users/x/sterna)
   Compiling serde v1.0.0
    Finished `dev` profile [unoptimized] target(s) in 13.01s
     Running tests/session.rs (target/debug/deps/session-015e)
test the_binary_runs_a_turn ... ok
test the_look_carries_the_purpose ... ok
test a_skipped_one ... ignored
test the_sandbox_line_agrees ... FAILED

failures:

---- the_sandbox_line_agrees stdout ----
thread 'the_sandbox_line_agrees' panicked at crates/sterna/tests/session.rs:4228:5:
  the Sandbox: line must name the writable root
test result: FAILED. 113 passed; 1 failed; 1 ignored; 0 measured
";

    #[test]
    fn a_failure_block_survives_verbatim_while_the_passing_lines_go() {
        let reduced = apply(CARGO_RUN);
        assert!(reduced.changed());
        for line in [
            "test the_sandbox_line_agrees ... FAILED",
            "thread 'the_sandbox_line_agrees' panicked at crates/sterna/tests/session.rs:4228:5:",
            "  the Sandbox: line must name the writable root",
            "test result: FAILED. 113 passed; 1 failed; 1 ignored; 0 measured",
            "     Running tests/session.rs (target/debug/deps/session-015e)",
        ] {
            assert!(reduced.text.contains(line), "{line:?} must survive");
        }
        assert!(!reduced.text.contains("test the_binary_runs_a_turn ... ok"));
        assert!(!reduced.text.contains("Compiling serde"));
        assert!(reduced.text.contains("Finished `dev` profile"));
        assert_eq!(
            reduced.applied,
            vec!["passing-test-lines", "build-progress"]
        );
    }

    #[test]
    fn a_line_naming_an_error_is_never_dropped_however_it_is_shaped() {
        // Shaped exactly like a droppable line, but it says error.
        let text = "   Compiling thing v1.0 error: could not compile\n".to_string();
        let reduced = apply(&text);
        assert!(!reduced.changed(), "a line naming an error is not progress");
        assert_eq!(reduced.text, text);
    }

    #[test]
    fn an_input_no_rule_recognises_comes_back_byte_identical() {
        let text = "some output\nthat looks like nothing in particular\n";
        let reduced = apply(text);
        assert!(!reduced.changed());
        assert_eq!(reduced.text, text);
    }

    #[test]
    fn a_run_of_identical_lines_is_folded_with_its_count_kept() {
        let text = format!("{}done\n", "the same warning: thing\n".repeat(9));
        let reduced = apply(&text);
        assert!(reduced.applied.contains(&"repeated-lines"));
        assert!(reduced.text.contains("the same warning: thing"));
        assert!(
            reduced.text.contains("repeated 9 times in all"),
            "the count replaces the copies, it does not discard them: {}",
            reduced.text
        );
        assert!(reduced.text.contains("done"));
    }

    #[test]
    fn two_adjacent_duplicates_are_output_not_noise() {
        let text = "a line\na line\nnext\n";
        let reduced = apply(text);
        assert!(!reduced.changed());
        assert_eq!(reduced.text, text);
    }
}
