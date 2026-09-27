use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use sterna::helper_context::{EvidenceKind, HelperRole, PreparedContext, prepare};
use sterna::sandbox::profile::Profile;
use sterna::tools::invoke::CancellationToken;

static NEXT: AtomicU64 = AtomicU64::new(0);

struct Fixture {
    root: PathBuf,
}

impl Fixture {
    fn new(label: &str) -> Self {
        let root = std::env::temp_dir().join(format!(
            "sterna-helper-context-{}-{label}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&root).unwrap();
        Self { root }
    }

    fn profile(&self) -> Profile {
        Profile::compile(&self.root, Some(r#"{"permissions":{}}"#))
    }

    fn prepare(&self, role: HelperRole, input: &str) -> PreparedContext {
        prepare(role, input, &self.profile(), &CancellationToken::new())
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn tree(packet: &PreparedContext) -> &str {
    packet
        .evidence
        .iter()
        .find(|item| item.kind == EvidenceKind::Tree)
        .map(|item| item.text.as_str())
        .unwrap_or("")
}

fn pattern_path(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

#[test]
fn helper_names_map_only_to_the_three_preparation_roles() {
    assert_eq!(
        HelperRole::from_helper_name("find"),
        Some(HelperRole::Scout)
    );
    assert_eq!(
        HelperRole::from_helper_name("check"),
        Some(HelperRole::Checker)
    );
    assert_eq!(
        HelperRole::from_helper_name("reduce"),
        Some(HelperRole::Reducer)
    );
    assert_eq!(HelperRole::from_helper_name("other"), None);
}

#[test]
fn scout_is_sorted_role_specific_and_prunes_generated_secret_and_huge_content() {
    let fixture = Fixture::new("scout");
    std::fs::write(
        fixture.root.join("Cargo.toml"),
        "[package]\nname = \"demo\"\n[dev-dependencies]\n",
    )
    .unwrap();
    std::fs::write(fixture.root.join("z.rs"), "fn unrelated() {}\n").unwrap();
    std::fs::write(fixture.root.join("a.rs"), "fn needle_handler() {}\n").unwrap();
    std::fs::write(fixture.root.join(".env"), "TOKEN=do-not-render\n").unwrap();
    std::fs::write(fixture.root.join("large.rs"), vec![b'x'; 300 * 1024]).unwrap();
    for generated in ["node_modules", "build", "vendor"] {
        let directory = fixture.root.join(generated);
        std::fs::create_dir(&directory).unwrap();
        for index in 0..300 {
            std::fs::write(directory.join(format!("noise-{index:03}.rs")), "needle\n").unwrap();
        }
    }

    let packet = fixture.prepare(HelperRole::Scout, "find the needle handler");
    let listed = tree(&packet);
    assert!(listed.find("a.rs") < listed.find("z.rs"), "{listed}");
    assert!(listed.contains("Cargo.toml"), "{packet:#?}");
    assert!(!listed.contains("node_modules"), "{listed}");
    assert!(!listed.contains("build/"), "{listed}");
    assert!(!listed.contains("vendor/"), "{listed}");
    assert!(
        !packet.rendered.contains("do-not-render"),
        "{}",
        packet.rendered
    );
    assert!(
        !packet.rendered.contains("large.rs\n"),
        "{}",
        packet.rendered
    );
    assert!(
        packet
            .omissions
            .iter()
            .any(|item| { item.subject == "large.rs" && item.reason == "file size limit reached" })
    );
    assert!(
        packet
            .evidence
            .iter()
            .any(|item| { item.kind == EvidenceKind::Match && item.subject == "a.rs:1" })
    );
    assert!(
        packet
            .rendered
            .starts_with("Deterministic starting evidence.")
    );
    assert!(packet.rendered.contains("untrusted data"));
    assert_eq!(
        packet,
        fixture.prepare(HelperRole::Scout, "find the needle handler"),
        "unchanged inputs must produce identical prepared evidence"
    );
}

#[test]
fn scout_applies_supported_gitignore_and_reports_unsupported_scope() {
    let fixture = Fixture::new("ignore");
    std::fs::write(fixture.root.join(".gitignore"), "ignored/\n").unwrap();
    std::fs::create_dir(fixture.root.join("ignored")).unwrap();
    std::fs::write(fixture.root.join("ignored/hidden.rs"), "needle\n").unwrap();
    std::fs::create_dir(fixture.root.join("kept")).unwrap();
    std::fs::write(fixture.root.join("kept/visible.rs"), "needle\n").unwrap();
    std::fs::write(fixture.root.join("kept/.gitignore"), "!visible.rs\n").unwrap();

    let packet = fixture.prepare(HelperRole::Scout, "find needle");
    assert!(!tree(&packet).contains("ignored/hidden.rs"), "{packet:#?}");
    assert!(!tree(&packet).contains("kept/visible.rs"), "{packet:#?}");
    assert!(
        packet.omissions.iter().any(|item| {
            item.subject == "kept" && item.reason.contains("unsupported gitignore")
        }),
        "{packet:#?}"
    );
}

#[test]
fn scout_stops_at_wide_and_deep_directories_with_explicit_omissions() {
    let fixture = Fixture::new("bounds");
    let wide = fixture.root.join("wide");
    std::fs::create_dir(&wide).unwrap();
    for index in 0..257 {
        std::fs::write(wide.join(format!("entry-{index:03}.rs")), "needle\n").unwrap();
    }
    let mut deep = fixture.root.join("deep");
    for _ in 0..9 {
        std::fs::create_dir_all(&deep).unwrap();
        deep = deep.join("next");
    }

    let packet = fixture.prepare(HelperRole::Scout, "find needle");
    assert!(
        packet.omissions.iter().any(|item| {
            item.subject == "wide" && item.reason == "directory width limit reached"
        }),
        "{packet:#?}"
    );
    assert!(
        packet
            .omissions
            .iter()
            .any(|item| item.reason == "depth limit reached"),
        "{packet:#?}"
    );
    assert!(!tree(&packet).contains("entry-"), "{packet:#?}");
}

#[test]
fn unreadable_ignore_semantics_never_turn_into_unfiltered_discovery() {
    let fixture = Fixture::new("oversized-ignore");
    std::fs::write(fixture.root.join(".gitignore"), "# padding\n".repeat(1000)).unwrap();
    std::fs::write(fixture.root.join("hidden.rs"), "needle-private\n").unwrap();
    let packet = fixture.prepare(HelperRole::Scout, "find needle");
    assert!(!packet.rendered.contains("needle-private"));
    assert!(!tree(&packet).contains("hidden.rs"));
    assert!(
        packet
            .omissions
            .iter()
            .any(|o| o.reason == "gitignore size or type unsupported")
    );
}

/// A reducer's input is never truncated — user ruling, 2026-09-10.
///
/// A truncated log makes it answer about the part that survived, which can be
/// "no failures" when the failures were in the omitted middle. Sending the
/// whole thing lets an oversized input fail the request out loud instead, and
/// that failure is true information: the output needed filtering before it
/// reached a reducer at all.
#[test]
fn a_reducer_is_never_truncated_and_a_question_is() {
    use sterna::helper_context::{HelperRole, MAX_INPUT_BYTES, payload_bound};
    assert_eq!(payload_bound(HelperRole::Scout), Some(MAX_INPUT_BYTES));
    assert_eq!(payload_bound(HelperRole::Checker), Some(MAX_INPUT_BYTES));
    assert_eq!(
        payload_bound(HelperRole::Reducer),
        None,
        "a truncated reduction is a quiet wrong answer; a failed one is a true signal"
    );
}

/// An over-long payload keeps both ends: a log's verdict is at the end, and a
/// head-only cut throws away the half that says what failed.
#[test]
fn an_oversized_payload_keeps_the_verdict_at_the_end() {
    use sterna::helper_context::bounded_payload;
    let log = format!(
        "FIRST-ERROR at the top\n{}\nFAILED (failures=2) at the very end\n",
        "noise noise noise\n".repeat(60_000)
    );
    let bound = 16 * 1024;
    let bounded = bounded_payload(&log, bound);

    assert!(bounded.len() <= bound, "the bound holds: {}", bounded.len());
    assert!(
        bounded.contains("FIRST-ERROR at the top"),
        "the first failure survives"
    );
    assert!(
        bounded.contains("FAILED (failures=2) at the very end"),
        "the verdict survives, which a head-only cut would have dropped"
    );
    assert!(
        bounded.contains("omitted from the middle"),
        "and the cut says what it dropped"
    );
}

/// Under the bound nothing is touched at all.
#[test]
fn a_payload_within_the_bound_is_passed_through_whole() {
    use sterna::helper_context::bounded_payload;
    let log = "error: one thing went wrong\n";
    assert_eq!(bounded_payload(log, 64 * 1024), log);
}

/// And `helpers::run` is what applies the role's bound to the appended
/// original, rather than sending it whole beneath an omission notice that
/// says otherwise.
#[test]
fn a_request_bounds_the_original_it_appends() {
    const SOURCE: &str = include_str!("../src/helpers.rs");
    let after = SOURCE
        .split_once("Original helper request:")
        .expect("the appended-original format string must still exist")
        .1;
    assert!(
        after.contains("carried"),
        "the appended original goes through the role's bound, not raw"
    );
    assert!(
        SOURCE.contains("payload_bound(role)")
            || SOURCE.contains("and_then(crate::helper_context::payload_bound)"),
        "the bound applied must be the role's"
    );
}

#[test]
fn discovery_does_not_derive_search_terms_beyond_the_input_byte_bound() {
    let fixture = Fixture::new("bounded-input");
    std::fs::write(
        fixture.root.join("match.rs"),
        "fn unique_tail_needle() {}\n",
    )
    .unwrap();
    let input = format!("{} unique_tail_needle", "x".repeat(1024 * 1024));
    let packet = fixture.prepare(HelperRole::Scout, &input);
    assert!(
        !packet
            .evidence
            .iter()
            .any(|item| item.kind == EvidenceKind::Match)
    );
    assert!(
        packet
            .omissions
            .iter()
            .any(|item| item.subject == "scout input" && item.reason == "input byte limit reached")
    );
}

#[test]
fn scout_obeys_parent_denial_and_never_follows_symlinks() {
    let fixture = Fixture::new("access");
    std::fs::create_dir(fixture.root.join("denied")).unwrap();
    std::fs::write(fixture.root.join("denied/secret.rs"), "needle-secret\n").unwrap();
    std::fs::write(fixture.root.join("visible.rs"), "needle-visible\n").unwrap();
    let denied = pattern_path(&fixture.root.join("denied"));
    let settings =
        format!(r#"{{"permissions":{{"deny":["Read({denied})","Read({denied}/**)"]}}}}"#);
    let profile = Profile::compile(&fixture.root, Some(&settings));

    let packet = prepare(
        HelperRole::Scout,
        "find needle",
        &profile,
        &CancellationToken::new(),
    );
    assert!(tree(&packet).contains("visible.rs"), "{packet:#?}");
    assert!(
        !packet.rendered.contains("needle-secret"),
        "{}",
        packet.rendered
    );
    assert!(
        packet.omissions.iter().any(|item| {
            item.subject == "denied" && item.reason == "parent profile denied read"
        }),
        "{packet:#?}"
    );

    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(
            fixture.root.join("visible.rs"),
            fixture.root.join("link.rs"),
        )
        .unwrap();
        let packet = fixture.prepare(HelperRole::Scout, "find needle");
        assert!(!tree(&packet).contains("link.rs"), "{packet:#?}");
        assert!(
            packet.omissions.iter().any(|item| {
                item.subject == "link.rs" && item.reason == "symbolic link not followed"
            }),
            "{packet:#?}"
        );
    }
}

#[test]
fn cancelled_preparation_reads_nothing_and_reports_cancellation() {
    let fixture = Fixture::new("cancelled");
    std::fs::write(fixture.root.join("visible.rs"), "needle\n").unwrap();
    let token = CancellationToken::new();
    token.cancel();
    let packet = prepare(HelperRole::Scout, "find needle", &fixture.profile(), &token);
    assert!(packet.cancelled);
    assert!(packet.evidence.is_empty(), "{packet:#?}");
    assert!(packet.operations.is_empty(), "{packet:#?}");
    assert!(
        packet
            .omissions
            .iter()
            .any(|item| item.reason == "cancelled")
    );
}

#[test]
fn checker_uses_supplied_change_and_original_contract_without_running_tests() {
    let fixture = Fixture::new("checker");
    std::fs::write(
        fixture.root.join("README.md"),
        "The parser must preserve caller input exactly.\n",
    )
    .unwrap();
    let supplied = "diff --git a/src/parser.rs b/src/parser.rs\n--- a/src/parser.rs\n+++ b/src/parser.rs\n@@\n+TOKEN=hidden\n+preserve caller input\n";
    let packet = fixture.prepare(HelperRole::Checker, supplied);
    assert!(
        packet.evidence.iter().any(|item| {
            item.kind == EvidenceKind::ChangedFile && item.subject == "src/parser.rs"
        }),
        "{packet:#?}"
    );
    assert!(
        packet
            .evidence
            .iter()
            .any(|item| { item.kind == EvidenceKind::Contract && item.subject == "README.md" }),
        "{packet:#?}"
    );
    assert!(
        !packet.rendered.contains("TOKEN=hidden"),
        "{}",
        packet.rendered
    );
    assert!(
        !packet
            .operations
            .iter()
            .any(|item| item.action.contains("run"))
    );
}

#[test]
fn checker_keeps_current_source_and_scopes_a_missing_diff_to_history_claims() {
    let fixture = Fixture::new("checker-current-source");
    std::fs::write(
        fixture.root.join("README.md"),
        "The parser must preserve caller input exactly.\n",
    )
    .unwrap();
    let supplied = r#"{"current_source":"src/parser.rs:7 preserve caller input exactly","question":"Does the current implementation satisfy the contract?"}"#;
    let packet = fixture.prepare(HelperRole::Checker, supplied);

    assert!(packet.evidence.iter().any(|item| {
        item.kind == EvidenceKind::Supplied
            && item.subject == "supplied checker data"
            && item
                .text
                .contains("src/parser.rs:7 preserve caller input exactly")
    }));
    assert!(packet.evidence.iter().any(|item| {
        item.kind == EvidenceKind::Contract
            && item.subject == "README.md"
            && item.text.contains("preserve caller input exactly")
    }));
    assert!(packet.omissions.iter().any(|item| {
        item.subject == "changed files"
            && item.reason
                == "no unified-diff paths; change-history claims need other baseline evidence"
    }));
    assert!(packet.rendered.contains("current_source"));
    assert!(packet.rendered.contains("[Supplied] supplied checker data"));
    assert!(!packet.rendered.contains("[Diff] supplied checker data"));
}

#[test]
fn reducer_extracts_real_failure_windows_and_clean_logs_are_negative() {
    let fixture = Fixture::new("reducer");
    std::fs::write(
        fixture.root.join("must-not-read.rs"),
        "error: filesystem bait\n",
    )
    .unwrap();
    let failed = fixture.prepare(
        HelperRole::Reducer,
        "$ cargo test\ncompiling demo\nerror[E0308]: wrong type\n  detail\nexit code: 1\n",
    );
    let window = failed
        .evidence
        .iter()
        .find(|item| item.kind == EvidenceKind::Failure)
        .expect("failure window");
    assert!(window.text.contains("compiling demo"), "{window:#?}");
    assert!(window.text.contains("error[E0308]"), "{window:#?}");
    assert!(!failed.rendered.contains("filesystem bait"));
    assert!(
        !failed
            .operations
            .iter()
            .any(|item| item.action == "enumerate")
    );

    let clean = fixture.prepare(
        HelperRole::Reducer,
        "$ cargo test\ntest result: ok. 12 passed; 0 failed; 0 ignored\nexit code: 0\n",
    );
    assert!(
        !clean
            .evidence
            .iter()
            .any(|item| item.kind == EvidenceKind::Failure),
        "{clean:#?}"
    );
    assert!(
        clean.evidence.iter().any(|item| {
            item.kind == EvidenceKind::Status && item.text == "no failure marker found"
        }),
        "{clean:#?}"
    );
}

/// Exact structural compression: a run of lines differing only in their
/// numbers is one observation repeated, and encoding it as a shape plus an
/// exact count discards no distinct thing that was printed.
#[test]
fn a_run_of_identical_shapes_collapses_without_losing_a_distinct_line() {
    use sterna::helper_context::collapse_runs;
    let mut log = String::from("starting run\n");
    for i in 0..6_000 {
        log.push_str(&format!(
            "[probe] step {i:05} ok cache=hit region=eu-central-1\n"
        ));
    }
    log.push_str("ERROR: the one thing that actually failed\n");
    log.push_str("FAILED (failures=1)\n");

    let collapsed = collapse_runs(&log);
    assert_eq!(collapsed.lines_in, 6_003);
    assert_eq!(
        collapsed.lines_out, 6,
        "one shape becomes first + count + last: {}",
        collapsed.text
    );

    // Every distinct line survives, and both ends of the run stay readable.
    assert!(collapsed.text.contains("starting run"));
    assert!(
        collapsed
            .text
            .contains("ERROR: the one thing that actually failed")
    );
    assert!(collapsed.text.contains("FAILED (failures=1)"));
    assert!(collapsed.text.contains("[probe] step 00000 ok"));
    assert!(collapsed.text.contains("[probe] step 05999 ok"));
    assert!(collapsed.text.contains("5998 more lines of the same shape"));
    assert!(
        collapsed.text.len() * 20 < log.len(),
        "it must actually save bulk"
    );
}

/// Distinct lines are never merged, however similar they look: only a line
/// whose neighbour has the same shape is ever folded away.
#[test]
fn distinct_lines_are_never_collapsed_into_one_another() {
    use sterna::helper_context::collapse_runs;
    let log = "error: alpha failed\nerror: beta failed\nerror: gamma failed\n";
    let collapsed = collapse_runs(log);
    assert_eq!(collapsed.lines_in, collapsed.lines_out);
    for name in ["alpha", "beta", "gamma"] {
        assert!(collapsed.text.contains(name), "{name} was lost");
    }
}

/// A short run costs more to describe than to send, so it is sent.
#[test]
fn a_short_run_is_left_exactly_as_it_was() {
    use sterna::helper_context::collapse_runs;
    let log = "step 1\nstep 2\nstep 3\n";
    let collapsed = collapse_runs(log);
    assert_eq!(collapsed.text, log);
    assert_eq!(collapsed.lines_in, collapsed.lines_out);
}

/// Only digits vary within a shape. Two lines whose words differ are two
/// observations, not one repeated.
#[test]
fn only_numbers_may_vary_within_one_shape() {
    use sterna::helper_context::collapse_runs;
    let log = "ok id=1\nok id=2\nok id=3\nok id=4\nFAIL id=5\nok id=6\nok id=7\nok id=8\nok id=9\n";
    let collapsed = collapse_runs(log);
    assert!(collapsed.text.contains("FAIL id=5"), "{}", collapsed.text);
}
