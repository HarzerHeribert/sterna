//! The TypeScript the model is given instead of prose about how sterna works.
//!
//! The governing rule, and the reason this file exists rather than a longer
//! preamble: **a fact the model must act on belongs in a type it can read,
//! not in a sentence it must remember.** The load-bearing instance is
//! `complete` — sterna may hold ten thousand exact matches and put a hundred in
//! the working set, so a result that were a bare array would teach a model
//! that iterating it covers everything. It is an object with `count` and
//! `complete`, and the guard writes itself.

/// The shared result vocabulary, declared once ahead of the capabilities.
///
/// `ArtifactRef` is a name, never content: it is how a bounded or derived
/// view keeps its complete observation reachable (`tool-abi.md` §12).
pub const PRELUDE: &str = "\
type ArtifactRef = string;
type Evidence = \"exact\" | \"bounded_exact\" | \"derived\";
// `complete` is false when sterna holds more of this observation than the value
// carries. `artifact` then names the whole, and `source` says what this is.
type Bounded = {count: number; complete: boolean; source: Evidence; artifact?: ArtifactRef};

type ReadInput = {file_path: string};
type ReadResult = Bounded & {path: string; text: string; bytes: number; lineCount: number};

type SearchInput = {pattern: string; path?: string};
type SearchMatch = {path: string; line: number; text: string};
type SearchResult = Bounded & {matches: SearchMatch[]};

type GlobInput = {pattern: string};
type GlobResult = Bounded & {paths: string[]};

// Give one pair: `old_string`/`new_string` for one hunk, or `old_strings`/
// `new_strings` (same length, in order) for several applied together or not
// at all. Every hunk must match exactly once in the file as it is now.
type EditInput = {file_path: string; old_string?: string; new_string?: string; old_strings?: string[]; new_strings?: string[]};
type EditResult = {path: string; before_sha256: string; after_sha256: string; hunks: {start: number; before: number; after: number}[]};

type WriteInput = {file_path: string; content?: string; lines?: string[]};
type WriteResult = {path: string};

type CommandInput = {command: string};
// `ok` is exit_code === 0, decided by sterna rather than by reading stdout.
// `reduced` is a summary of a large output. `reduction_error` means one was
// attempted and could not be made: stdout and stderr are still complete, and
// narrowing what the command prints is what makes a summary possible.
type CommandResult = Bounded & {stdout: string; stderr: string; exit_code: number | null; ok: boolean; reduced?: string; reduction_error?: string};

type CheckInput = {name: string; force?: boolean};
// `executed` and `reused` are never both true; a reused observation does not
// claim a fresh run.
type CheckResult = {name: string; command: string; stdout: string; stderr: string; exit_code: number | null; executed: boolean; reused: boolean; reuse_scope: string};";

/// The whole conceptual contract, in the words the model needs and no more.
///
/// Everything the model does not need to know to act correctly — lowering,
/// the intent IR, routing tiers, helper escalation, the artifact store, the
/// ledger — is deliberately absent. Those are Sterna's business, and a model
/// that reproduced them would be doing Sterna's job with worse information.
pub const GUIDANCE: &str = "\
The familiar tools above are available directly and inside `execute_cell`.
Use a direct call for a simple or independent operation. Use `execute_cell`
when a later operation depends on an earlier result, or when loops, branching,
batching or local transformation would otherwise cost extra turns. Inside a
cell the same tools are the same typed async functions, with the same
arguments and the same result semantics as their direct forms. Prefer ordinary
tool calls unless composition gives a concrete advantage, and write the
smallest cell that expresses the dependency or control flow you need.

Sterna decides execution strategy, output sizing, evidence storage and helper
escalation. Do not reproduce those mechanisms yourself.

    const tests = await Bash({ command: \"cargo test\" });
    if (!tests.ok) {
      const files = await Grep({ pattern: \"SomeError\", path: \"src\" });
      return { tests, files };
    }
    return tests;";

/// [`GUIDANCE`] for the cells interface, where `execute_cell` is the only
/// provider-native tool.
///
/// The general text says the familiar tools are "available directly" and asks
/// for "the smallest cell that expresses the dependency": both are false or
/// harmful when there is no direct call to prefer, and they contradicted the
/// preamble's "spend the turn on a whole step". On six SWE-bench tasks
/// (2026-09-29) cells-only sessions wrote ~300-character programs, one look
/// per turn -- 47 requests where Codex used 32 on the same task.
pub const CELLS_GUIDANCE: &str = "\
The familiar tools above are typed async functions inside `execute_cell`,
with the arguments and results their declarations name. A turn is one
program, so spend it on a whole step: fetch every independent file, symbol,
search and check the step needs together, act on what comes back in the same
program, and yield only at a decision that needs evidence you do not have yet.
Several small cells cost several turns.

A slow command whose result this step does not need yet -- a whole test
suite, a build -- need not hold the turn: `const suite = bg.run(\"...\")`
starts it, you keep reading and editing in this cell and the next, and
`await suite.result()` collects it where you need it.

Sterna decides execution strategy, output sizing, evidence storage and helper
escalation. Do not reproduce those mechanisms yourself.

    const [tests, uses] = await Promise.all([
      Bash({ command: \"cargo test\" }),
      Grep({ pattern: \"SomeError\", path: \"src\" }),
    ]);
    if (!tests.ok) return { tests, uses };
    return tests;";

/// How the model reports and how it asks — a user ruling of 2026-09-10.
///
/// It is in the prompt rather than in a type because it governs prose, which
/// is the one thing a schema cannot constrain. It is short for the same
/// reason [`GUIDANCE`] is: a rule the model must apply to every sentence has
/// to be recallable, not looked up.
pub const REPORTING: &str = "\
Report in product behaviour, not in coordinates. A file name, a symbol, an
identifier or a line number may support an explanation and must never stand in
for one: say what changed for the person using this project, then cite the
place if it helps.

When you need a decision, do not ask the person to choose between internal
names or representations. Explain the behaviour each option produces, why the
choice exists, what each one costs, and which you recommend. If a decision has
already been made and only an internal representation is left, choose the
smallest coherent one and keep going without asking.";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_bounded_result_can_be_guarded_before_it_is_iterated() {
        // The property the type exists for: a model can decide from the value
        // alone whether iterating it covers the whole observation.
        for name in ["ReadResult", "SearchResult", "GlobResult", "CommandResult"] {
            assert!(PRELUDE.contains(&format!("type {name} = Bounded &")));
        }
        assert!(PRELUDE.contains("complete: boolean"));
        assert!(PRELUDE.contains("artifact?: ArtifactRef"));
    }

    #[test]
    fn the_iterable_field_is_never_a_bare_array_result() {
        // A bare array would be iterable with no way to know it is partial.
        assert!(PRELUDE.contains("matches: SearchMatch[]"));
        assert!(PRELUDE.contains("paths: string[]"));
        assert!(!PRELUDE.contains("type SearchResult = SearchMatch[]"));
    }

    #[test]
    fn a_command_result_says_ok_rather_than_asking_for_stdout_to_be_read() {
        assert!(PRELUDE.contains("ok: boolean"));
        assert!(PRELUDE.contains("exit_code: number | null"));
    }

    /// A failed summary is a state of its own in the type, because "no
    /// summary was needed" and "a summary could not be made" are opposite
    /// instructions to the model and an absent key cannot say which.
    #[test]
    fn a_command_result_distinguishes_a_missing_summary_from_a_failed_one() {
        assert!(PRELUDE.contains("reduced?: string"));
        assert!(PRELUDE.contains("reduction_error?: string"));
    }

    #[test]
    fn a_check_result_keeps_fresh_and_reused_distinguishable() {
        assert!(PRELUDE.contains("executed: boolean"));
        assert!(PRELUDE.contains("reused: boolean"));
    }

    /// The guidance stays a conceptual contract, not an architecture lecture:
    /// the implementation vocabulary must not appear in it at all.
    #[test]
    fn the_guidance_never_explains_the_implementation() {
        for leaked in [
            "Cell IR",
            "lowering",
            "canonical intent",
            "reducer",
            "Little Helper",
            "ledger",
            "artifact store",
            "router",
        ] {
            assert!(
                !GUIDANCE.contains(leaked),
                "the model prompt explains `{leaked}`, which is Sterna's business"
            );
        }
    }

    #[test]
    fn the_reporting_rule_asks_for_behaviour_and_bans_bare_coordinates() {
        assert!(REPORTING.contains("product behaviour"));
        assert!(REPORTING.contains("recommend"));
        // The rule is only useful if it is short enough to apply every turn.
        let words = REPORTING.split_whitespace().count();
        assert!(words < 140, "the reporting rule is {words} words");
    }

    /// Cells mode has no direct call to prefer, so its guidance may neither
    /// promise one nor ask for small cells; the preamble asks for a whole
    /// step per turn and the two must agree.
    #[test]
    fn the_cells_guidance_asks_for_whole_steps_and_promises_no_direct_call() {
        for contradiction in [
            "smallest cell",
            "available directly",
            "Prefer ordinary tool calls",
            "direct call",
        ] {
            assert!(
                !CELLS_GUIDANCE.contains(contradiction),
                "the cells guidance says `{contradiction}`"
            );
        }
        assert!(CELLS_GUIDANCE.contains("whole step"));
        assert!(CELLS_GUIDANCE.contains("Promise.all"));
        assert!(CELLS_GUIDANCE.split_whitespace().count() < 200);
        for leaked in ["lowering", "reducer", "ledger", "router"] {
            assert!(!CELLS_GUIDANCE.contains(leaked));
        }
    }

    #[test]
    fn the_guidance_stays_short() {
        // The budget this file exists to defend. Prose is the fallback for
        // knowledge that could not be typed, so it is capped rather than
        // trusted.
        let words = GUIDANCE.split_whitespace().count();
        assert!(words < 200, "the guidance is {words} words");
    }
}
