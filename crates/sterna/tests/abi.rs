//! Acceptance tests for the Tool ABI against `docs/tools.md`.
//!
//! The load-bearing ones are the equivalence tests: they run the *same*
//! capability through a direct provider call and through a model-authored
//! cell and assert the two produce the same trajectory. That is §1's
//! prohibition on a second executor stated as a test rather than as a rule,
//! and criterion 20.
//!
//! Every program here is assembled as a string by this file; no model is
//! called anywhere in this package. The spawning tests are gated the way
//! `runtime_cells.rs` gates its own: on Windows `tools::invoke` refuses to
//! spawn rather than running unconfined, so a "the tool ran" assertion would
//! fail there for a reason that has nothing to do with the ABI.

use serde_json::json;
use sterna::abi::dialect::{self, Target};
use sterna::abi::{EvidenceClass, Interface, Presentation, Provenance, Router, encode_result};

// Gated like their only callers. The tests that build a runtime and run a
// capability are macOS/Linux, because `tools::invoke` refuses to spawn on
// Windows rather than running unconfined; on Windows these names would be
// unused, which is an error under denied warnings.
#[cfg(any(target_os = "macos", target_os = "linux"))]
use sterna::abi::{Dialect, lower};
#[cfg(any(target_os = "macos", target_os = "linux"))]
use sterna::contract::SessionId;
#[cfg(any(target_os = "macos", target_os = "linux"))]
#[cfg(any(target_os = "macos", target_os = "linux"))]
use sterna::runtime::isolate::Runtime;
#[cfg(any(target_os = "macos", target_os = "linux"))]
use sterna::runtime::outcome::{CellOutcome, CellRecord};
#[cfg(any(target_os = "macos", target_os = "linux"))]
use sterna::sandbox::profile::Profile;

/// Criterion 1 and 2: hybrid shows both entry points, and the ablation modes
/// change visibility without choosing an architecture.
#[test]
fn the_interface_modes_are_visibility_only() {
    assert!(Interface::Hybrid.declares_cell());
    assert!(Interface::Hybrid.declares_direct_tools());
    assert!(Interface::Cells.declares_cell() && !Interface::Cells.declares_direct_tools());
    assert!(Interface::Tools.declares_direct_tools() && !Interface::Tools.declares_cell());
    // The evidence for "visibility only": no mode carries an executor, a
    // strategy or a kernel, so there is nothing for a mode to fork. The type
    // has three unit variants and no payload.
    assert_eq!(Interface::parse("hybrid").unwrap(), Interface::Hybrid);
    assert_eq!(std::mem::size_of::<Interface>(), 1);
}

/// Criterion 3 and 6: every dialect row compiles through one descriptor to a
/// capability that exists, and the two façades differ in spelling only.
#[test]
fn every_dialect_row_reaches_an_existing_capability() {
    for dialect in dialect::ALL {
        assert!(
            !dialect.shapes().is_empty(),
            "{} declares no tools",
            dialect.as_str()
        );
        for shape in dialect.shapes() {
            assert!(
                dialect::target_exists(shape.target),
                "{}::{} targets {:?}, which nothing implements",
                dialect.as_str(),
                shape.provider_name,
                shape.target
            );
        }
    }
}

/// Criterion 5: a dialect row carries a spelling and nothing executable, so
/// provider-specific code cannot reach into execution.
#[test]
fn a_dialect_row_carries_no_execution_of_its_own() {
    for dialect in dialect::ALL {
        for shape in dialect.shapes() {
            match shape.target {
                Target::Tool(name) => {
                    assert!(sterna::tools::registry::lookup(name).is_some());
                }
                Target::HostCall(name) => {
                    assert!(dialect::HOST_CALLS.contains(&name));
                }
            }
        }
    }
}

/// No spelling may mean two capabilities, because inside a cell both
/// dialects' names are bound and a collision would resolve silently to
/// whichever dialect is listed first.
#[test]
fn no_spelling_means_two_things() {
    let mut seen: Vec<(&str, Target)> = Vec::new();
    for dialect in dialect::ALL {
        for shape in dialect.shapes() {
            if let Some((_, target)) = seen.iter().find(|(name, _)| *name == shape.provider_name) {
                assert_eq!(
                    *target, shape.target,
                    "`{}` names two different capabilities",
                    shape.provider_name
                );
            } else {
                seen.push((shape.provider_name, shape.target));
            }
        }
    }
}

/// Criterion 23, and §11's closing rule: only an exact result can carry an
/// exact-content claim, and a helper view never can however good it is.
#[test]
fn a_derived_result_cannot_substantiate_an_exact_content_claim() {
    assert!(EvidenceClass::Exact.substantiates_exact_claim());
    assert!(!EvidenceClass::BoundedExact.substantiates_exact_claim());
    assert!(!EvidenceClass::Derived.substantiates_exact_claim());

    let helper_view = Presentation {
        content: json!({"summary": "21 relevant failures"}),
        provenance: Provenance::derived("tests_4_1", "test-log-reducer-v2"),
        count: 21,
    };
    let encoded = encode_result(&helper_view);
    assert_eq!(encoded["source"], json!("derived"));
    // Criterion 12: linked to its source evidence, always. And `complete` is
    // false, so a model cannot iterate or quote it as the whole.
    assert_eq!(encoded["artifact"], json!("tests_4_1"));
    assert_eq!(encoded["complete"], json!(false));
    assert!(!helper_view.provenance.class.substantiates_exact_claim());
    assert!(helper_view.provenance.keeps_exact_reachable());
}

/// Criterion 10: the three classes are produced by the router from
/// mechanical facts, not declared by a test.
#[test]
fn the_router_produces_exact_and_bounded_from_mechanical_facts_alone() {
    let router = Router::default();

    let small = json!({"path": "a.rs", "text": "fn main() {}", "bytes": 12});
    assert_eq!(
        router.present("read", "read_1_1", &small).provenance.class,
        EvidenceClass::Exact
    );

    let big_text = "x".repeat(40 * 1024);
    let big = json!({"path": "b.rs", "text": big_text, "bytes": 40 * 1024});
    let bounded = router.present("read", "read_1_2", &big);
    assert_eq!(bounded.provenance.class, EvidenceClass::BoundedExact);
    // Criterion 8: the omitted exact content stays addressable.
    assert_eq!(bounded.provenance.handle.as_deref(), Some("read_1_2"));
    assert_eq!(bounded.provenance.exact_bytes, Some(40 * 1024));
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn record_of(outcome: &CellOutcome) -> &CellRecord {
    match outcome {
        CellOutcome::Yielded { turn }
        | CellOutcome::Returned { turn, .. }
        | CellOutcome::Threw { turn, .. } => &turn.record,
    }
}

/// A fixture tree with one source file whose body names `marker`.
///
/// Scoped by test name and process id the way `handles.rs` scopes its own,
/// so two tests in one binary cannot share a root and a rerun starts clean.
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn fixture(test: &str, marker: &str) -> std::path::PathBuf {
    let root = std::env::temp_dir().join(format!("sterna-abi-{test}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("target.rs"), format!("fn {marker}() {{}}\n")).unwrap();
    root
}

/// Criterion 4, 7 and 20 — the load-bearing test.
///
/// `Read({file_path})` as a lowered direct call and `read({path})` as an
/// authored cell must produce the same trajectory: the same capability, the
/// same checked arguments, the same result. Only the binding name may
/// differ, because that is the one thing the two forms legitimately disagree
/// about.
#[test]
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn a_direct_call_and_a_cell_call_are_one_capability() {
    let root = fixture("equivalence", "equivalence");
    let target = root.join("target.rs");
    let profile = Profile::compile(&root, Some(r#"{"permissions":{"allow":[]}}"#));

    // The direct form, lowered exactly as a provider call would be.
    let calls = vec![(
        "call-1".to_string(),
        "Read".to_string(),
        json!({"file_path": target.to_string_lossy()}),
    )];
    let lowered = lower(Dialect::Anthropic, &calls, 1).unwrap();
    let mut direct_runtime = Runtime::new(&profile, &SessionId::new("abi-direct"));
    let direct = direct_runtime.run_cell(&lowered.source);

    // The authored form, in Sterna's own spelling.
    let authored_source = format!(
        "const mine = await read({{ path: {:?} }});\n",
        target.to_string_lossy()
    );
    let mut authored_runtime = Runtime::new(&profile, &SessionId::new("abi-authored"));
    let authored = authored_runtime.run_cell(&authored_source);

    let direct_record = record_of(&direct);
    let authored_record = record_of(&authored);

    // One capability, one checked argument set: the trajectory is identical.
    assert_eq!(direct_record.calls.len(), 1, "{direct:?}");
    assert_eq!(authored_record.calls.len(), 1, "{authored:?}");
    assert_eq!(direct_record.calls[0].tool, authored_record.calls[0].tool);
    assert_eq!(direct_record.calls[0].args, authored_record.calls[0].args);
    assert_eq!(direct_record.calls[0].ended, authored_record.calls[0].ended);

    // One result: same type, same preview, different binding name only.
    assert_eq!(direct_record.handles.len(), 1);
    assert_eq!(authored_record.handles.len(), 1);
    assert_eq!(
        direct_record.handles[0].type_name,
        authored_record.handles[0].type_name
    );
    assert_eq!(
        direct_record.handles[0].preview,
        authored_record.handles[0].preview
    );
    assert_eq!(direct_record.handles[0].name, "read_1_1");
    assert_eq!(authored_record.handles[0].name, "mine");
}

/// Criterion 7: the familiar spelling works inside a cell, with the
/// provider's own parameter names — §14's `Read({file_path})`.
#[test]
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn the_familiar_spelling_and_its_parameters_work_inside_a_cell() {
    let root = fixture("in-cell", "in_cell");
    let target = root.join("target.rs");
    let profile = Profile::compile(&root, Some(r#"{"permissions":{"allow":[]}}"#));
    let mut runtime = Runtime::new(&profile, &SessionId::new("abi-in-cell"));

    let source = format!(
        "const viaAlias = await Read({{ file_path: {path:?} }});\n\
         const viaCanonical = await read({{ path: {path:?} }});\n",
        path = target.to_string_lossy()
    );
    let outcome = runtime.run_cell(&source);
    let record = record_of(&outcome);

    assert_eq!(record.calls.len(), 2, "{outcome:?}");
    // Both spellings recorded the same capability under the registry name,
    // so the ledger has one identity for the capability however it was
    // spelled (criterion 17).
    assert_eq!(record.calls[0].tool, "read");
    assert_eq!(record.calls[1].tool, "read");
    assert_eq!(record.calls[0].args, record.calls[1].args);
}

/// Criterion 21: dependent control flow inside one parent-authored cell,
/// branching on an earlier capability's actual result.
#[test]
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn dependent_control_flow_branches_on_an_earlier_capability_result() {
    let root = fixture("branch", "legacyAuth");
    let target = root.join("target.rs");
    let profile = Profile::compile(&root, Some(r#"{"permissions":{"allow":[]}}"#));
    let mut runtime = Runtime::new(&profile, &SessionId::new("abi-branch"));

    // The branch is taken only if the first capability's real result contains
    // the marker, and the second call is a different capability, so the
    // trajectory proves the guard held rather than that both ran.
    let source = format!(
        "const first = await Read({{ file_path: {path:?} }});\n\
         let matched = null;\n\
         if (first.text.includes(\"legacyAuth\")) {{\n\
           matched = await Grep({{ pattern: \"legacyAuth\", path: {root:?} }});\n\
         }}\n",
        path = target.to_string_lossy(),
        root = root.to_string_lossy()
    );
    let outcome = runtime.run_cell(&source);
    let record = record_of(&outcome);

    assert_eq!(
        record.calls.len(),
        2,
        "the guard held, so both capabilities ran: {outcome:?}"
    );
    assert_eq!(record.calls[0].tool, "read");
    assert_eq!(record.calls[1].tool, "grep");
}

/// The other half of the branch: an untaken guard runs nothing, so a cell
/// cannot be read as having executed a speculative branch
/// (`runtime-contract.md` §9.1, and §20's requested-vs-executed).
#[test]
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn an_untaken_branch_records_no_call() {
    let root = fixture("untaken", "absent_marker");
    let target = root.join("target.rs");
    let profile = Profile::compile(&root, Some(r#"{"permissions":{"allow":[]}}"#));
    let mut runtime = Runtime::new(&profile, &SessionId::new("abi-untaken"));

    let source = format!(
        "const first = await Read({{ file_path: {path:?} }});\n\
         if (first.text.includes(\"legacyAuth\")) {{\n\
           await Grep({{ pattern: \"legacyAuth\", path: {root:?} }});\n\
         }}\n",
        path = target.to_string_lossy(),
        root = root.to_string_lossy()
    );
    let outcome = runtime.run_cell(&source);
    let record = record_of(&outcome);

    assert_eq!(record.calls.len(), 1, "{outcome:?}");
    assert_eq!(record.calls[0].tool, "read");
}

/// Criterion 9: a fused turn of independent direct calls is one execution
/// frame whose results are handles, so a provider-native call keeps Sterna's
/// context advantage instead of being forced inline.
#[test]
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn independent_direct_calls_fuse_into_one_frame_of_handles() {
    let root = fixture("fusion", "fusion");
    let target = root.join("target.rs");
    let profile = Profile::compile(&root, Some(r#"{"permissions":{"allow":[]}}"#));
    let mut runtime = Runtime::new(&profile, &SessionId::new("abi-fusion"));

    let calls = vec![
        (
            "a".to_string(),
            "Read".to_string(),
            json!({"file_path": target.to_string_lossy()}),
        ),
        (
            "b".to_string(),
            "Glob".to_string(),
            json!({"pattern": "*.rs"}),
        ),
    ];
    let lowered = lower(Dialect::Anthropic, &calls, 1).unwrap();
    let outcome = runtime.run_cell(&lowered.source);
    let record = record_of(&outcome);

    // Two provider calls, one frame, two handles — and each lowered call
    // knows the handle its result is reachable under, which is what the
    // result envelope hands back to the model.
    assert_eq!(record.calls.len(), 2, "{outcome:?}");
    assert_eq!(record.handles.len(), 2);
    assert_eq!(lowered.calls[0].binding, "read_1_1");
    assert_eq!(lowered.calls[1].binding, "glob_1_2");
    let names: Vec<&str> = record
        .handles
        .iter()
        .map(|handle| handle.name.as_str())
        .collect();
    assert!(names.contains(&"read_1_1"));
    assert!(names.contains(&"glob_1_2"));
}

/// A top-level binding may not take a dialect spelling, for the same reason
/// it may not take a registry name: the capture that makes a handle persist
/// would overwrite the capability for the whole task.
#[test]
fn a_program_cannot_shadow_a_dialect_spelling() {
    assert!(sterna::runtime::cell::is_host_function("Read"));
    assert!(sterna::runtime::cell::is_host_function("shell"));
    assert!(sterna::runtime::cell::is_host_function("read"));
    assert!(!sterna::runtime::cell::is_host_function("myOwnBinding"));
}
