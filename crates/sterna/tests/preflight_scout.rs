//! The scouting preflight -- `smarter-cheaper-roadmap.md`, *Preflight Helper*
//! and *Adaptive orchestration*: which tasks pay for a scout, what the scout
//! is handed so it never impersonates the parent, and what the parent reads.

use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use sterna::config::PreflightScope;
use sterna::manifest::{Executable, Manifest};
use sterna::preflight::{
    self, DO_NOT_PERFORM, Decision, RENDER_LINE_BOUND, SIGNAL_ABSENT_EXECUTABLE, SIGNAL_ALWAYS,
    SIGNAL_LONG_REQUEST, SIGNAL_MISSING_PATH, SIGNAL_UNSEEN_VERIFICATION,
};

fn unique() -> u64 {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    NEXT.fetch_add(1, Ordering::Relaxed)
}

/// A project root holding `src/lib.rs` and `README.md`, with a manifest whose
/// `gdb` is absent and `rg` present.
struct Project {
    root: PathBuf,
    manifest: Manifest,
}

impl Project {
    fn new(label: &str) -> Self {
        let root = std::env::temp_dir().join(format!(
            "sterna-preflight-scout-{label}-{}-{}",
            std::process::id(),
            unique()
        ));
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(root.join("src/lib.rs"), "pub fn entry() {}\n").unwrap();
        fs::write(root.join("README.md"), "# fixture\n").unwrap();
        let manifest = Manifest {
            root: root.display().to_string(),
            readable_roots: vec![root.display().to_string()],
            writable_roots: vec![root.display().to_string()],
            executables: vec![
                Executable {
                    name: "gdb".into(),
                    path: None,
                },
                Executable {
                    name: "rg".into(),
                    path: Some("/usr/bin/rg".into()),
                },
            ],
            ..Manifest::default()
        };
        Self { root, manifest }
    }

    fn decide(&self, task: &str, checks_configured: bool) -> Decision {
        preflight::should_scout(
            task,
            &self.manifest,
            PreflightScope::Auto,
            checks_configured,
            None,
            0.85,
        )
    }
}

impl Drop for Project {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn signals(decision: &Decision) -> Vec<&'static str> {
    match decision {
        Decision::Run(signals) => signals.clone(),
        Decision::Skip(reason) => panic!("expected Run, got Skip({reason})"),
    }
}

// --- the decision matrix -------------------------------------------------

#[test]
fn a_short_request_naming_only_existing_files_skips() {
    let project = Project::new("skip");
    let decision = project.decide(
        "Rename the entry function in src/lib.rs and README.md",
        true,
    );
    assert!(
        matches!(decision, Decision::Skip(_)),
        "{}",
        preflight::signals_summary(&decision)
    );
    assert!(preflight::signals_summary(&decision).starts_with("preflight: skipped ("));
}

#[test]
fn a_missing_path_alone_runs_with_that_signal() {
    let project = Project::new("missing-path");
    let decision = project.decide("Rename the entry function in src/missing.rs", true);
    assert_eq!(signals(&decision), vec![SIGNAL_MISSING_PATH]);

    // An absolute path that does not exist is the pilot's `/build` case.
    let decision = project.decide("Inspect /build/sterna-fixture-does-not-exist", true);
    assert_eq!(signals(&decision), vec![SIGNAL_MISSING_PATH]);
}

#[test]
fn an_absent_executable_alone_runs_with_that_signal() {
    let project = Project::new("absent-exe");
    let decision = project.decide("Step through src/lib.rs under gdb.", true);
    assert_eq!(signals(&decision), vec![SIGNAL_ABSENT_EXECUTABLE]);

    // A present executable is not a signal.
    let decision = project.decide("Search src/lib.rs with rg", true);
    assert!(matches!(decision, Decision::Skip(_)), "{decision:?}");
}

#[test]
fn a_verification_word_runs_only_while_no_checks_are_configured() {
    let project = Project::new("verification");
    for task in [
        "Make the tests in src/lib.rs pass",
        "Verify src/lib.rs compiles",
        "Run cargo test on src/lib.rs",
        "Report coverage for src/lib.rs",
        "Benchmark src/lib.rs",
        "Run src/lib.rs under valgrind",
        "Run pytest against src/lib.rs",
    ] {
        let decision = project.decide(task, false);
        assert_eq!(
            signals(&decision),
            vec![SIGNAL_UNSEEN_VERIFICATION],
            "{task}"
        );
        let decision = project.decide(task, true);
        assert!(
            matches!(decision, Decision::Skip(_)),
            "{task}: {decision:?}"
        );
    }
}

#[test]
fn a_request_over_eighty_words_runs_with_that_signal() {
    let project = Project::new("long");
    let long = "word ".repeat(81);
    let decision = project.decide(&long, true);
    assert_eq!(signals(&decision), vec![SIGNAL_LONG_REQUEST]);

    let short = "word ".repeat(80);
    assert!(matches!(project.decide(&short, true), Decision::Skip(_)));
}

#[test]
fn always_runs_whatever_the_request_says() {
    let project = Project::new("always");
    let decision = preflight::should_scout(
        "Rename the entry function in src/lib.rs",
        &project.manifest,
        PreflightScope::Always,
        true,
        None,
        0.85,
    );
    assert_eq!(signals(&decision), vec![SIGNAL_ALWAYS]);
}

#[test]
fn every_signal_that_holds_is_named_in_the_summary() {
    let project = Project::new("all-signals");
    let mut task = "Test src/gone.rs under gdb ".to_string();
    task.push_str(&"word ".repeat(80));
    let decision = project.decide(&task, false);
    assert_eq!(
        signals(&decision),
        vec![
            SIGNAL_MISSING_PATH,
            SIGNAL_ABSENT_EXECUTABLE,
            SIGNAL_UNSEEN_VERIFICATION,
            SIGNAL_LONG_REQUEST,
        ]
    );
    let summary = preflight::signals_summary(&decision);
    assert_eq!(summary.lines().count(), 1, "{summary}");
    for signal in signals(&decision) {
        assert!(summary.contains(signal), "{summary}");
    }
}

// --- the scouting brief --------------------------------------------------

#[test]
fn the_brief_quotes_the_request_verbatim_and_says_not_to_perform_it_once() {
    let project = Project::new("brief");
    let task =
        "Fix the crash in src/lib.rs by implementing the missing branch,\nthen run the tests.";
    let brief = preflight::scouting_brief(task, &project.manifest);

    let request_block = format!("## Request (do not perform it)\n{task}\n");
    assert!(brief.contains(&request_block), "{brief}");
    assert_eq!(brief.matches(DO_NOT_PERFORM).count(), 1, "{brief}");
    for heading in [
        "## Constraints",
        "## Files",
        "## Tests",
        "## Capabilities",
        "## Risks",
    ] {
        assert_eq!(brief.matches(heading).count(), 1, "{heading}: {brief}");
    }
    // The manifest's environment lines ride along so the scout knows what is
    // readable and available.
    assert!(brief.contains("## Environment"), "{brief}");
    assert!(brief.contains("Absent executables: gdb"), "{brief}");
    assert_eq!(preflight::request_in(&brief), Some(task));
}

#[test]
fn the_brief_never_instructs_the_scout_to_implement_or_fix() {
    let project = Project::new("brief-instructions");
    let task = "Rename the entry function in src/lib.rs";
    let brief = preflight::scouting_brief(task, &project.manifest);
    // Outside the quoted request and the one do-not-perform sentence, the
    // brief's own instructions never say implement or fix.
    let own_words = brief.replace(task, "").replace(DO_NOT_PERFORM, "");
    let lower = own_words.to_ascii_lowercase();
    for forbidden in ["implement", "fix"] {
        let found = lower
            .split(|c: char| !c.is_ascii_alphanumeric())
            .any(|word| word.starts_with(forbidden));
        assert!(!found, "the brief instructs `{forbidden}`: {brief}");
    }
}

// --- the rendered block --------------------------------------------------

/// A file the scout named and the bounds refused is stated with its reason,
/// so the model can read it itself. Silence here is what makes a model
/// re-derive what it was almost handed.
#[test]
fn a_named_file_that_was_not_served_is_stated_with_its_reason() {
    let report = "## Files\nsrc/big.rs:1 — the whole thing\n## Risks\nnone read\n";
    let served = vec![(
        "src/small.rs".to_string(),
        "pub fn small() {}\n".to_string(),
    )];
    let unserved = vec![(
        "src/big.rs".to_string(),
        "not served whole: 99999 bytes, over the 32768-byte limit".to_string(),
    )];
    let block = preflight::render_serving("Do the thing", report, &served, &unserved, None);

    assert!(
        block.contains("## Named but not served (1)"),
        "the section exists when something was refused:\n{block}"
    );
    assert!(
        block.contains("- src/big.rs — not served whole: 99999 bytes, over the 32768-byte limit"),
        "the file is named with the bound that refused it:\n{block}"
    );
    assert!(
        block.contains("1 named but not served"),
        "the scouting record counts it too:\n{block}"
    );
    let served_at = block.find("## Served in full (1)").unwrap();
    let unserved_at = block.find("## Named but not served (1)").unwrap();
    let record_at = block.find("## Scouting record").unwrap();
    assert!(
        served_at < unserved_at && unserved_at < record_at,
        "it sits with the material it is about:\n{block}"
    );
}

/// Nothing refused, nothing said: the ordinary block is unchanged.
#[test]
fn nothing_refused_adds_no_section() {
    let report = "## Files\nsrc/small.rs:1 — it\n## Risks\nnone read\n";
    let served = vec![(
        "src/small.rs".to_string(),
        "pub fn small() {}\n".to_string(),
    )];
    let block = preflight::render("Do the thing", report, &served, None);
    assert!(
        !block.contains("Named but not served"),
        "no notice when every named file was served:\n{block}"
    );
}

#[test]
fn render_leads_with_the_request_and_stays_under_the_line_bound() {
    let mut report = String::from("## Constraints\n");
    for index in 0..10 {
        report.push_str(&format!("- constraint {index}\n"));
    }
    report.push_str("## Files\n");
    for index in 0..10 {
        report.push_str(&format!("src/f{index}.rs:{index} — file {index}\n"));
    }
    report.push_str("## Tests\n");
    for index in 0..10 {
        report.push_str(&format!("tests/t{index}.rs:{index} — test {index}\n"));
    }
    report.push_str("## Capabilities\nrg is available\n## Risks\nnone read\n");
    let served = vec![("src/f0.rs".to_string(), "pub fn f0() {}\n".to_string())];
    let block = preflight::render("Do the thing", &report, &served, None);

    assert!(
        block
            .trim_start()
            .starts_with("## Request (verbatim, authoritative)\nDo the thing\n"),
        "{block}"
    );
    let constraints = block.find("## Constraints").unwrap();
    let served_at = block.find("## Served in full (1)").unwrap();
    let record_at = block.find("## Scouting record").unwrap();
    assert!(constraints < served_at && served_at < record_at, "{block}");
    let section_lines = block[constraints..served_at]
        .lines()
        .filter(|line| !line.is_empty() && !line.starts_with("## ") && !line.starts_with('('))
        .count();
    assert_eq!(section_lines, RENDER_LINE_BOUND, "{block}");
    // Dropped from the end: Constraints and Files survive whole, Tests is
    // cut, and the cut is said.
    assert!(block.contains("- constraint 9\n"), "{block}");
    assert!(block.contains("src/f9.rs:9"), "{block}");
    assert!(!block.contains("tests/t9.rs:9"), "{block}");
    assert!(
        block.contains("8 lines of the scout's report were cut"),
        "{block}"
    );
    assert!(
        block.contains("### src/f0.rs\n```\npub fn f0() {}\n```"),
        "{block}"
    );
    let record = block[record_at..].lines().nth(1).unwrap();
    assert!(
        record.contains("8 cut") && record.contains("1 served in full"),
        "{record}"
    );
}

#[test]
fn an_open_question_renders_as_none_found_not_a_menu() {
    let report = "## Constraints\nnone read\n## Files\nsrc/lib.rs:1 — entry\n\
                  ## Tests\nCould not determine; candidates: tests/a.rs, tests/b.rs\n\
                  ## Capabilities\nIs valgrind available?\n## Risks\n";
    let block = preflight::render("Do the thing", report, &[], None);
    let tests = section(&block, "## Tests");
    assert_eq!(tests, "(none found)", "{block}");
    assert!(!block.contains("candidates"), "{block}");
    let capabilities = section(&block, "## Capabilities");
    assert_eq!(capabilities, "(none found)", "{block}");
    // An absent section is (none found) too, and the record counts answers.
    assert_eq!(section(&block, "## Risks"), "(none found)", "{block}");
    assert!(block.contains("2 of 5 sections answered"), "{block}");
}

/// The Scouting record names the ranking (2644) when the caller passes one,
/// and stays exactly as it was today when it does not.
#[test]
fn the_scouting_record_carries_the_ranking_note_when_one_is_given() {
    let report = "## Constraints\nnone read\n## Files\nsrc/lib.rs:1 — entry\n\
                  ## Tests\nnone\n## Capabilities\nrg\n## Risks\nnone\n";
    let ranked = preflight::render(
        "Do the thing",
        report,
        &[],
        Some("ranked 2, skipped 1, top: a.rs 0.94"),
    );
    let record_at = ranked.find("## Scouting record").unwrap();
    let record = ranked[record_at..].lines().nth(1).unwrap();
    assert!(
        record.contains("ranked 2, skipped 1, top: a.rs 0.94"),
        "{record}"
    );

    let unranked = preflight::render("Do the thing", report, &[], None);
    let record_at = unranked.find("## Scouting record").unwrap();
    let record = unranked[record_at..].lines().nth(1).unwrap();
    assert!(!record.contains("ranked"), "{record}");
}

fn section(block: &str, heading: &str) -> String {
    let start = block.find(heading).unwrap() + heading.len();
    let rest = &block[start..];
    let end = rest.find("\n## ").unwrap_or(rest.len());
    rest[..end].trim().to_string()
}

// --- spans ---------------------------------------------------------------

#[test]
fn spans_parse_both_separators_and_ignore_prose() {
    let report = "## Files\nsrc/a.rs:12 — the entry point\nsrc/b.rs:3: a helper\n\
                  Nothing else was read, see line 12:3 of the log.\nsrc/a.rs:40 — again\n\
                  Makefile:9 is not a span\n";
    let spans = preflight::spans(report);
    assert_eq!(
        spans,
        vec![
            ("src/a.rs".to_string(), "the entry point".to_string()),
            ("src/b.rs".to_string(), "a helper".to_string()),
        ]
    );
}
