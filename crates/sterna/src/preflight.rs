//! The scouting preflight -- `smarter-cheaper-roadmap.md`, *Preflight Helper*
//! and *Adaptive orchestration*.
//!
//! The invariant: **the scout never impersonates the parent.** It is handed a
//! brief that quotes the request under a heading saying not to perform it and
//! asks for five sections of facts; the pilot's scouts that "did not run
//! Valgrind or modify files" were handed the raw request and tried to be the
//! parent. The second invariant: **direct execution stays the fast path.**
//! Under `PreflightScope::Auto` a task pays for a scout only when its request
//! carries an uncertainty signal [`should_scout`] can name.

use std::path::Path;

use crate::config::PreflightScope;
use crate::manifest::Manifest;

/// Whether to run the scout, and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    /// Run, for these signals (at least one).
    Run(Vec<&'static str>),
    /// Skip, for this reason.
    Skip(&'static str),
}

/// The signal names `Decision::Run` carries, spelled once so the machine
/// output and the inspector agree with the tests.
pub const SIGNAL_ALWAYS: &str = "preflight_scope = always";
pub const SIGNAL_MISSING_PATH: &str = "names a path that does not exist";
pub const SIGNAL_ABSENT_EXECUTABLE: &str = "names an executable the session lacks";
pub const SIGNAL_UNSEEN_VERIFICATION: &str = "asks for verification with no checks configured";
pub const SIGNAL_LONG_REQUEST: &str = "exceeds 80 words";
/// The decision model's complexity answer, when it reads `needs_exploration`
/// at or above `scout_above` (F2, map 2614/2615's paragraph). This signal can
/// only add to `Run` -- the four signals above stay authoritative on their
/// own, and a `trivial` answer never removes one of them.
pub const SIGNAL_DECIDED_EXPLORATION: &str =
    "the decision model reads this request as needing exploration";

/// The one reason to skip: nothing above held.
pub const SKIP_NO_SIGNAL: &str = "no uncertainty signal: named paths exist, no absent executable or unseen verification, under 80 words";

/// A request longer than this is assumed to carry more than the parent should
/// orient on unaided.
pub const LONG_REQUEST_WORDS: usize = 80;

/// The most lines of the scout's five sections [`render`] carries.
pub const RENDER_LINE_BOUND: usize = 24;

/// The heading the brief quotes the request under, and the one the rendered
/// block leads with.
pub const REQUEST_HEADING: &str = "## Request (do not perform it)";
/// Ends the quoted request in the brief, so [`request_in`] needs no guess.
const ANSWER_HEADING: &str = "## Answer with exactly these five sections";
const RENDER_REQUEST_HEADING: &str = "## Request (verbatim, authoritative)";

/// The sentence that keeps a scout a scout, stated once in the brief.
pub const DO_NOT_PERFORM: &str = "You are scouting for the model that will act. Do not attempt the task, do not \
     build, test or fix anything, and do not report on work you did not do.";

/// The Scout's two briefs (2026-09-23): the span brief, which asks where
/// things are, and the dissection brief, which asks what the request takes
/// -- tasks, the files each needs first, how to verify each, and what the
/// acting model must have before it starts. The decision model's `explore`
/// answer picks the second (`decision-model.md` §12); measured on luna, the
/// dissection is right at ~300 words and ten seconds, and longer answers
/// were both slower and worse.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Brief {
    Spans,
    Dissection,
}

impl Brief {
    /// The brief's sections, by exact heading, with what each may hold.
    #[must_use]
    pub fn sections(self) -> &'static [(&'static str, &'static str)] {
        match self {
            Brief::Spans => &SECTIONS,
            Brief::Dissection => &DISSECTION_SECTIONS,
        }
    }

    /// The name the scouting record and telemetry use.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Brief::Spans => "spans",
            Brief::Dissection => "dissection",
        }
    }
}

/// The dissection brief's sections (2026-09-23).
const DISSECTION_SECTIONS: [(&str, &str); 5] = [
    (
        "## Tasks",
        "the request dissected into at most four tasks, one per line as `N. <name> — what done looks like`, in the order to do them",
    ),
    (
        "## Files",
        "for each task the files to read first, at most three, as `path/to/file.rs — task N, what is there`; add `:120` only for a line you opened, never one you inferred",
    ),
    (
        "## Verify",
        "the commands that would show each task done, one per line, only ones this environment can run",
    ),
    (
        "## Needs",
        "what the acting model must have in hand before it starts and cannot find in the files: a choice only the person can make, a value, an environment fact",
    ),
    (
        "## Skip",
        "paths deliberately not worth reading for this request, each with the reason",
    ),
];

/// The heading whose lines go to the Ask step.
const NEEDS_HEADING: &str = "## Needs";

/// How the acting model uses a dissection: the tasks are the checklist its
/// answer is held to, and the files above are already read for it.
pub const DISSECTION_USE: &str = "\nWork the tasks above in order and answer every one of them; \
     start from the files served below instead of listing the tree again.\n";

/// The Ask step for a dissection's needs: a need that blocks the request and
/// that no file can answer is asked of the person before any change; every
/// other need becomes an assumption stated in the answer.
pub const NEEDS_USE: &str = "Before you change anything, ask the person with `ask` only for a need \
     above that blocks the request and that no file can answer; for every other need, state the \
     assumption you made in your answer.\n";

/// The most paths a one-shot dissection's listing carries: past this the
/// listing costs more to read than a search loop costs to run.
pub const LISTING_PATHS: usize = 3000;

/// The project's tracked files as a brief section, for the one-shot
/// dissection (`helpers::DISSECTOR`): `git ls-files` under `root`, at most
/// [`LISTING_PATHS`] of them. `None` outside a git repository or when git
/// fails -- the Scout's search loop then runs instead.
#[must_use]
pub fn listing_section(root: &Path) -> Option<String> {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["ls-files", "--cached", "--others", "--exclude-standard"])
        .output()
        .ok()
        .filter(|output| output.status.success())?;
    let text = String::from_utf8_lossy(&output.stdout);
    let paths: Vec<&str> = text.lines().filter(|line| !line.is_empty()).collect();
    if paths.is_empty() {
        return None;
    }
    let shown = paths.len().min(LISTING_PATHS);
    let mut section = format!(
        "\n## Project files ({} of {} tracked)\n",
        shown,
        paths.len()
    );
    for path in &paths[..shown] {
        section.push_str(path);
        section.push('\n');
    }
    Some(section)
}

/// The dissection brief's own instruction, after [`DO_NOT_PERFORM`]: the
/// measured lever on a dissection's latency and correctness is its length.
pub const DISSECT: &str = "Dissect the request into the tasks it takes and what each needs first. Prefer few, \
     decisive files over many, and keep the whole answer under 300 words.";

/// The signal the decision model's `explore` answer adds (2026-09-23).
pub const SIGNAL_DECIDED_EXPLORE: &str = "the decision model read the request as exploration";

/// The five sections, by exact heading, with what each may hold.
const SECTIONS: [(&str, &str); 5] = [
    (
        "## Constraints",
        "limits the request states or the environment imposes, as spans or facts",
    ),
    (
        "## Files",
        "the files the request concerns, as `path/to/file.rs:120 — what is there`",
    ),
    (
        "## Tests",
        "tests and check commands that already exist for this area, as spans",
    ),
    (
        "## Capabilities",
        "what the environment below does or does not provide that the request needs",
    ),
    (
        "## Risks",
        "facts that could make the request fail, each as a span or a fact you read",
    ),
];

/// Words that ask for verification the session may not be able to see.
const VERIFICATION_WORDS: [&str; 7] = [
    "test",
    "tests",
    "verify",
    "coverage",
    "benchmark",
    "valgrind",
    "pytest",
];

/// Dotted tokens that are prose, not paths.
const DOTTED_ABBREVIATIONS: [&str; 4] = ["e.g", "i.e", "etc", "vs"];

/// Phrases that make a section an open question rather than an answer.
const OPEN_QUESTION: [&str; 6] = [
    "could not determine",
    "cannot determine",
    "can't determine",
    "unable to determine",
    "could not find",
    "not sure",
];

/// Decide whether this task pays for a scout.
///
/// `Always` runs. `Auto` runs when any signal holds: a path-like token that
/// does not exist under the manifest's root, an executable the manifest lists
/// as absent, a verification word while no checks are configured, a request
/// over [`LONG_REQUEST_WORDS`] words, or `decided` reading `needs_exploration`
/// at or above `scout_above` (F2). A short request naming only files that
/// exist, with no exploration signal from the decision model, is the fast
/// path and skips.
///
/// `decided` is the caller's own choice, not this function's: pass `None`
/// whenever the decision model should not affect this task (no model
/// configured, `mode` is `off` or `shadow`, or the request failed) --
/// `should_scout` never removes a deterministic signal for `trivial`, but it
/// also never second-guesses why `decided` was withheld.
#[must_use]
pub fn should_scout(
    task: &str,
    manifest: &Manifest,
    scope: PreflightScope,
    checks_configured: bool,
    decided: Option<&crate::decide::Complexity>,
    scout_above: f64,
) -> Decision {
    if scope == PreflightScope::Always {
        return Decision::Run(vec![SIGNAL_ALWAYS]);
    }
    let words: Vec<&str> = task.split_whitespace().collect();
    let mut signals = Vec::new();
    if words
        .iter()
        .map(|word| trim_token(word))
        .any(|token| path_like(token) && !exists(token, manifest))
    {
        signals.push(SIGNAL_MISSING_PATH);
    }
    if words
        .iter()
        .any(|word| names_absent_executable(word, manifest))
    {
        signals.push(SIGNAL_ABSENT_EXECUTABLE);
    }
    if !checks_configured && asks_for_verification(task, &words) {
        signals.push(SIGNAL_UNSEEN_VERIFICATION);
    }
    if words.len() > LONG_REQUEST_WORDS {
        signals.push(SIGNAL_LONG_REQUEST);
    }
    if let Some(complexity) = decided
        && complexity.choice == crate::decide::NEEDS_EXPLORATION
        && complexity.confidence >= scout_above
    {
        signals.push(SIGNAL_DECIDED_EXPLORATION);
    }
    if signals.is_empty() {
        Decision::Skip(SKIP_NO_SIGNAL)
    } else {
        Decision::Run(signals)
    }
}

/// One line for the machine output and the `HELPERS` inspector.
#[must_use]
pub fn signals_summary(decision: &Decision) -> String {
    match decision {
        Decision::Run(signals) => format!("preflight: run ({})", signals.join("; ")),
        Decision::Skip(reason) => format!("preflight: skipped ({reason})"),
    }
}

/// The scout's input: the request quoted verbatim under a heading that says
/// not to perform it, the five sections asked for by exact heading, and the
/// manifest's environment lines so the scout knows what is readable and
/// available. [`DO_NOT_PERFORM`] is stated once.
#[must_use]
pub fn scouting_brief(task: &str, manifest: &Manifest) -> String {
    scouting_brief_for(Brief::Spans, task, manifest)
}

/// [`scouting_brief`] for either brief: the dissection brief carries
/// [`DISSECT`] after [`DO_NOT_PERFORM`] and asks for its own five sections.
#[must_use]
pub fn scouting_brief_for(kind: Brief, task: &str, manifest: &Manifest) -> String {
    let mut brief = String::new();
    brief.push_str(DO_NOT_PERFORM);
    if kind == Brief::Dissection {
        brief.push(' ');
        brief.push_str(DISSECT);
    }
    brief.push_str("\n\n");
    brief.push_str(REQUEST_HEADING);
    brief.push('\n');
    brief.push_str(task);
    brief.push_str("\n\n");
    brief.push_str(ANSWER_HEADING);
    brief.push_str(
        "\nBy these headings, one span or fact per line, and `(none found)` under a heading you \
         have nothing for. A span is `path/to/file.rs:120` followed by one short sentence in the \
         file's own words. Never list candidates you rejected and never pose a question back.\n",
    );
    for (heading, holds) in kind.sections() {
        brief.push('\n');
        brief.push_str(heading);
        brief.push('\n');
        brief.push_str(holds);
        brief.push('\n');
    }
    brief.push('\n');
    brief.push_str(&manifest.render());
    brief.push('\n');
    brief
}

/// The verbatim request inside a brief [`scouting_brief`] produced, so a
/// record can say what was asked rather than quoting the brief's first line.
#[must_use]
pub fn request_in(brief: &str) -> Option<&str> {
    let start = brief.find(REQUEST_HEADING)? + REQUEST_HEADING.len();
    let rest = brief[start..].strip_prefix('\n')?;
    let end = rest
        .find(&format!("\n\n{ANSWER_HEADING}"))
        .unwrap_or(rest.len());
    Some(rest[..end].trim_end())
}

/// The block appended to the system prompt: the request first and
/// authoritative, the scout's five sections bounded to [`RENDER_LINE_BOUND`]
/// lines, the files the session chose to serve (as `(path, contents)`), and
/// one line of record.
///
/// A section that reads as an open question renders as `(none found)`: a
/// question in the prompt gets answered and a list of rejected candidates is
/// a menu to browse, so neither is carried (`little-helpers.md`).
///
/// `ranking` is `helpers::ScoutRanking::note()`'s line when the candidate
/// files the Scout was served were ranked (2644), or `None` when nothing was
/// ranked -- no model configured, `mode` not `on`, or the ranking question
/// itself failed.
#[must_use]
pub fn render(
    task: &str,
    report: &str,
    served: &[(String, String)],
    ranking: Option<&str>,
) -> String {
    render_serving(task, report, served, &[], ranking)
}

/// [`render`], and beside the files served in full the files that were named
/// and **not** served, each with the reason.
///
/// **The section says what it is not giving you.** A named file the bounds
/// refused is one the model can still read for itself -- but only if it is
/// told the file exists, which is the whole difference between a bounded
/// offer and an invisible one.
pub fn render_serving(
    task: &str,
    report: &str,
    served: &[(String, String)],
    unserved: &[(String, String)],
    ranking: Option<&str>,
) -> String {
    render_brief(Brief::Spans, task, report, served, unserved, ranking)
}

/// [`render_serving`] for either brief: the report is read by that brief's
/// own headings and the record names which brief it was.
#[must_use]
pub fn render_brief(
    kind: Brief,
    task: &str,
    report: &str,
    served: &[(String, String)],
    unserved: &[(String, String)],
    ranking: Option<&str>,
) -> String {
    let sections = sections_of(kind, report);
    let mut block = String::from("\n\n");
    block.push_str(RENDER_REQUEST_HEADING);
    block.push('\n');
    block.push_str(task);
    block.push('\n');

    let mut kept = 0usize;
    let mut cut = 0usize;
    let mut answered = 0usize;
    for (heading, lines) in &sections {
        block.push('\n');
        block.push_str(heading);
        block.push('\n');
        // A need is often a question for the person, so `## Needs` keeps its
        // questions; every other section reads one as the scout not knowing.
        let unanswered = if *heading == NEEDS_HEADING {
            says_nothing(lines)
        } else {
            says_nothing(lines) || is_open_question(lines)
        };
        let lines: Vec<&str> = if unanswered {
            vec!["(none found)"]
        } else {
            answered += 1;
            lines.iter().map(String::as_str).collect()
        };
        let room = RENDER_LINE_BOUND.saturating_sub(kept);
        let carried = lines.len().min(room);
        cut += lines.len() - carried;
        if carried == 0 {
            block.push_str(&format!("({} lines cut)\n", lines.len()));
            continue;
        }
        for line in &lines[..carried] {
            block.push_str(line);
            block.push('\n');
        }
        kept += carried;
    }
    if cut > 0 {
        block.push_str(&format!(
            "\n({cut} lines of the scout's report were cut at the {RENDER_LINE_BOUND}-line bound)\n"
        ));
    }
    if kind == Brief::Dissection {
        block.push_str(DISSECTION_USE);
        if sections
            .iter()
            .any(|(heading, lines)| *heading == NEEDS_HEADING && !says_nothing(lines))
        {
            block.push_str(NEEDS_USE);
        }
    }

    block.push_str(&format!("\n## Served in full ({})\n", served.len()));
    for (path, text) in served {
        block.push_str(&format!("### {path}\n```\n{}\n```\n\n", text.trim_end()));
    }
    if !unserved.is_empty() {
        block.push_str(&format!("## Named but not served ({})\n", unserved.len()));
        block.push_str("Read any of these yourself if the task needs them.\n");
        for (path, reason) in unserved {
            block.push_str(&format!("- {path} — {reason}\n"));
        }
        block.push('\n');
    }
    block.push_str(&format!(
        "## Scouting record\nscout ({}) · {answered} of {} sections answered · {kept} lines carried · {cut} cut · {} spans named · {} served in full{}{}\n",
        kind.as_str(),
        kind.sections().len(),
        spans(report).len(),
        served.len(),
        if unserved.is_empty() {
            String::new()
        } else {
            format!(" · {} named but not served", unserved.len())
        },
        ranking
            .map(|note| format!(" · {note}"))
            .unwrap_or_default(),
    ));
    block
}

/// The `path:line` spans a scout named, in its own order, one entry per path
/// with the rest of that line as its single line of why.
///
/// Text is what a scout returns, so this reads lines: `src/a.rs:12 — why`
/// and `src/a.rs:12: why` are spans, a line naming no `path.ext:N` is prose
/// and is not one.
#[must_use]
pub fn spans(report: &str) -> Vec<(String, String)> {
    let mut named: Vec<(String, String)> = Vec::new();
    for line in report.lines() {
        let Some((path, why)) = span(line) else {
            continue;
        };
        if named.iter().any(|(seen, _)| *seen == path) {
            continue;
        }
        named.push((path, why));
    }
    named
}

/// Every file a dissection's `## Files` section names, with or without a
/// line number, in its order: a one-shot dissection cannot open a file, so
/// it names paths, and a path it names must be served like a span or the
/// dissection's main output never reaches the acting model (the first
/// one-shot run, 2026-09-23, served nothing and cost the parent more).
#[must_use]
pub fn dissection_files(report: &str) -> Vec<(String, String)> {
    let mut named: Vec<(String, String)> = Vec::new();
    let files = sections_of(Brief::Dissection, report)
        .into_iter()
        .find(|(heading, _)| *heading == "## Files")
        .map(|(_, lines)| lines)
        .unwrap_or_default();
    for line in files {
        let words: Vec<&str> = line.split_whitespace().collect();
        let Some((index, path)) = words.iter().enumerate().find_map(|(index, word)| {
            let token = trim_token(word).trim_end_matches([':', '.', ',', ';']);
            let path = token.split_once(':').map_or(token, |(path, _)| path);
            path_like(path).then(|| (index, path.to_string()))
        }) else {
            continue;
        };
        if named.iter().any(|(seen, _)| *seen == path) {
            continue;
        }
        let why = words[index + 1..]
            .join(" ")
            .trim_start_matches(['-', '—', '–', ':', ' '])
            .trim()
            .to_string();
        named.push((
            path,
            if why.is_empty() {
                "named by the scout.".to_string()
            } else {
                why
            },
        ));
    }
    named
}

/// The first `path:line` on one line, and the rest of that line as its why.
///
/// A path must carry an extension: `line 12:3` and a bare `Makefile:9` are
/// not spans, and serving nothing is the safe direction.
/// Whether `number` is a line (`120`) or a range (`120-148`).
fn first_line(number: &str) -> bool {
    let first = number.split_once('-').map_or(number, |(first, _)| first);
    !first.is_empty() && first.chars().all(|c| c.is_ascii_digit())
}

fn span(line: &str) -> Option<(String, String)> {
    let words: Vec<&str> = line.split_whitespace().collect();
    for (index, word) in words.iter().enumerate() {
        let token = word
            .trim_matches(|c: char| {
                !c.is_ascii_alphanumeric() && !matches!(c, '.' | '/' | '_' | '-' | ':')
            })
            .trim_end_matches([':', '.', ',', ';']);
        let Some((path, number)) = token.rsplit_once(':') else {
            continue;
        };
        if path.is_empty() || !path.contains('.') || number.is_empty() || !first_line(number) {
            continue;
        }
        let rest = words[index + 1..].join(" ");
        let why = rest.trim_start_matches(['-', '—', '–', ':', ' ']).trim();
        return Some((
            path.to_string(),
            if why.is_empty() {
                "named by the scout.".to_string()
            } else {
                why.to_string()
            },
        ));
    }
    None
}

/// The five sections in [`SECTIONS`] order, each with its non-empty lines;
/// a heading the report lacks is present and empty. Text before the first
/// recognised heading belongs to no section and is not carried.
fn sections_of(kind: Brief, report: &str) -> Vec<(&'static str, Vec<String>)> {
    let mut sections: Vec<(&'static str, Vec<String>)> = kind
        .sections()
        .iter()
        .map(|(heading, _)| (*heading, Vec::new()))
        .collect();
    let mut current: Option<usize> = None;
    for raw in report.lines() {
        let line = raw.trim();
        if let Some(index) = heading_index(kind, line) {
            current = Some(index);
            continue;
        }
        if line.is_empty() {
            continue;
        }
        if let Some(index) = current {
            sections[index].1.push(line.to_string());
        }
    }
    sections
}

/// Which of the five headings this line is, tolerating `#` depth, a trailing
/// colon and case.
fn heading_index(kind: Brief, line: &str) -> Option<usize> {
    let bare = line.trim_start_matches('#').trim().trim_end_matches(':');
    if bare.len() == line.len() {
        return None;
    }
    kind.sections()
        .iter()
        .position(|(heading, _)| heading.trim_start_matches("## ").eq_ignore_ascii_case(bare))
}

/// Whether a section's lines are empty or only say there is nothing:
/// `(none)`, `none.`, `n/a`, `-`.
fn says_nothing(lines: &[String]) -> bool {
    lines.iter().all(|line| {
        let word = line
            .trim()
            .trim_matches(|c: char| matches!(c, '(' | ')' | '.' | '-' | '*' | ' '))
            .to_ascii_lowercase();
        word.is_empty() || matches!(word.as_str(), "none" | "none found" | "n/a" | "nothing")
    })
}

fn is_open_question(lines: &[String]) -> bool {
    lines.iter().any(|line| {
        let lower = line.to_ascii_lowercase();
        lower.ends_with('?') || OPEN_QUESTION.iter().any(|phrase| lower.contains(phrase))
    })
}

/// A word with its quoting and sentence punctuation removed: `` `gdb`. `` is
/// `gdb`, `src/lib.rs,` is `src/lib.rs`.
fn trim_token(word: &str) -> &str {
    word.trim_matches(|c: char| !c.is_ascii_alphanumeric() && !matches!(c, '.' | '/' | '_' | '-'))
        .trim_end_matches(['.', ',', ';', ':'])
}

/// A token is path-like when it holds a `/` or ends in a file extension: a
/// dotted last segment whose extension is one to eight alphanumerics with a
/// letter in it, and is not a spelled abbreviation.
fn path_like(token: &str) -> bool {
    if token.is_empty() || !token.chars().any(|c| c.is_ascii_alphabetic()) {
        return false;
    }
    if token.contains('/') {
        return token.len() > 1;
    }
    if DOTTED_ABBREVIATIONS
        .iter()
        .any(|abbreviation| token.eq_ignore_ascii_case(abbreviation))
    {
        return false;
    }
    let Some((stem, extension)) = token.rsplit_once('.') else {
        return false;
    };
    !stem.is_empty()
        && (1..=8).contains(&extension.len())
        && extension.chars().all(|c| c.is_ascii_alphanumeric())
        && extension.chars().any(|c| c.is_ascii_alphabetic())
}

/// Whether a path-like token exists: as given when absolute, and under the
/// manifest's root and each readable root otherwise. An empty root is not
/// joined, so a default manifest never resolves against the process's cwd.
fn exists(token: &str, manifest: &Manifest) -> bool {
    let path = Path::new(token);
    if path.is_absolute() && path.exists() {
        return true;
    }
    let relative = token.trim_start_matches('/');
    std::iter::once(manifest.root.as_str())
        .chain(manifest.readable_roots.iter().map(String::as_str))
        .filter(|root| !root.is_empty())
        .any(|root| Path::new(root).join(relative).exists())
}

fn names_absent_executable(word: &str, manifest: &Manifest) -> bool {
    let token = trim_token(word);
    !token.is_empty()
        && manifest.executables.iter().any(|executable| {
            executable.path.is_none() && executable.name.eq_ignore_ascii_case(token)
        })
}

fn asks_for_verification(task: &str, words: &[&str]) -> bool {
    task.to_ascii_lowercase().contains("cargo test")
        || words.iter().any(|word| {
            let token = trim_token(word).to_ascii_lowercase();
            VERIFICATION_WORDS.contains(&token.as_str())
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_dotted_abbreviation_and_a_version_number_are_not_paths() {
        assert!(!path_like("e.g"));
        assert!(!path_like("1.2.3"));
        assert!(!path_like("3.14"));
        assert!(path_like("main.rs"));
        assert!(path_like("src/lib"));
        assert!(path_like("/build"));
        assert!(!path_like("/"));
    }

    #[test]
    fn a_heading_is_recognised_at_any_depth_and_case() {
        assert_eq!(heading_index(Brief::Spans, "## Files"), Some(1));
        assert_eq!(heading_index(Brief::Spans, "### files:"), Some(1));
        assert_eq!(heading_index(Brief::Spans, "Files"), None);
        assert_eq!(heading_index(Brief::Spans, "## Reading"), None);
        assert_eq!(heading_index(Brief::Dissection, "## Needs"), Some(3));
        assert_eq!(heading_index(Brief::Dissection, "## Files"), Some(1));
    }

    /// A dissection's files are served whether or not they carry a line:
    /// a bare path from the listing and a `path:line` both count, prose
    /// under the heading does not, and a path named twice is served once.
    #[test]
    fn a_dissection_names_files_with_or_without_a_line() {
        let report = "## Tasks\n1. read it\n## Files\nscripts/setup.sh — task 1, the setup\n\
            - `src/main.rs:12` — task 1, the entry\nsee the docs for more\nscripts/setup.sh — again\n## Verify\nbash -n scripts/setup.sh\n";
        assert_eq!(
            dissection_files(report),
            vec![
                (
                    "scripts/setup.sh".to_string(),
                    "task 1, the setup".to_string()
                ),
                ("src/main.rs".to_string(), "task 1, the entry".to_string()),
            ]
        );
    }

    /// A dissection's needs go to the Ask step only when it named one; the
    /// tasks-in-order line rides every dissection, and a span brief gets
    /// neither.
    #[test]
    fn a_dissection_with_needs_carries_the_ask_step() {
        let with_needs =
            "## Tasks\n1. read — know it\n## Needs\n- which backend does the person run?\n";
        let block = render_brief(Brief::Dissection, "explore", with_needs, &[], &[], None);
        assert!(block.contains(DISSECTION_USE), "{block}");
        assert!(block.contains(NEEDS_USE), "{block}");
        assert!(
            block.contains("which backend does the person run?"),
            "{block}"
        );

        let without = "## Tasks\n1. read — know it\n## Needs\n(none)\n";
        let block = render_brief(Brief::Dissection, "explore", without, &[], &[], None);
        assert!(block.contains(DISSECTION_USE), "{block}");
        assert!(!block.contains(NEEDS_USE), "{block}");

        let block = render_brief(Brief::Spans, "fix it", with_needs, &[], &[], None);
        assert!(
            !block.contains(DISSECTION_USE) && !block.contains(NEEDS_USE),
            "{block}"
        );
    }

    #[test]
    fn an_empty_manifest_root_never_resolves_against_the_cwd() {
        let manifest = Manifest::default();
        assert!(!exists("Cargo.toml", &manifest));
        assert!(!exists("src", &manifest));
    }

    #[test]
    fn a_needs_exploration_answer_at_or_above_threshold_runs_with_that_signal_alone() {
        let manifest = Manifest::default();
        let complexity = crate::decide::Complexity {
            choice: crate::decide::NEEDS_EXPLORATION.to_string(),
            confidence: 0.90,
        };
        let decision = should_scout(
            "carry on",
            &manifest,
            PreflightScope::Auto,
            true,
            Some(&complexity),
            0.85,
        );
        assert_eq!(decision, Decision::Run(vec![SIGNAL_DECIDED_EXPLORATION]));
    }

    #[test]
    fn a_needs_exploration_answer_below_threshold_never_holds() {
        let manifest = Manifest::default();
        let complexity = crate::decide::Complexity {
            choice: crate::decide::NEEDS_EXPLORATION.to_string(),
            confidence: 0.80,
        };
        let decision = should_scout(
            "carry on",
            &manifest,
            PreflightScope::Auto,
            true,
            Some(&complexity),
            0.85,
        );
        assert_eq!(decision, Decision::Skip(SKIP_NO_SIGNAL));
    }

    #[test]
    fn a_trivial_answer_never_suppresses_a_deterministic_signal() {
        let manifest = Manifest::default();
        let complexity = crate::decide::Complexity {
            choice: "trivial".to_string(),
            confidence: 0.99,
        };
        let decision = should_scout(
            "Rename the entry function in src/missing.rs",
            &manifest,
            PreflightScope::Auto,
            true,
            Some(&complexity),
            0.85,
        );
        assert_eq!(decision, Decision::Run(vec![SIGNAL_MISSING_PATH]));
    }
}
