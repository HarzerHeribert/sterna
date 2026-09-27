//! The three pilot fixtures that ended "complete" with a wrong tree, plus
//! the contract-driven checks, reproduced in temp directories.

use std::path::{Path, PathBuf};

use sterna::completion::{
    Contract, Finding, FindingKind, TaskFiles, check, fresh_checker_evidence, load_contract,
};

fn fixture(name: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!(
        "sterna-final-state-{name}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&root).unwrap();
    root
}

fn write(root: &Path, relative: &str, bytes: &[u8]) {
    let path = root.join(relative);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, bytes).unwrap();
}

/// A `/`-written relative key as a finding names it: the platform's own
/// separator, so `polyglot\cmain` on Windows.
fn native(key: &str) -> String {
    Path::new(key)
        .components()
        .collect::<PathBuf>()
        .display()
        .to_string()
}

fn kinds(findings: &[Finding]) -> Vec<FindingKind> {
    findings.iter().map(|f| f.kind).collect()
}

#[test]
fn a_compiled_binary_beside_the_deliverable_is_an_unexpected_artifact() {
    let root = fixture("polyglot");
    write(&root, "polyglot/main.py.c", b"int main(){}\n");
    write(&root, "polyglot/cmain", b"\x7fELFxxxx");
    let mut files = TaskFiles::default();
    files.modified.insert("polyglot/main.py.c".into());
    files.created.insert("polyglot/cmain".into());
    let findings = check(&Contract::default(), &root, &files, Some(4), Some(3));
    assert_eq!(kinds(&findings), vec![FindingKind::UnexpectedArtifact]);
    let sentence = &findings[0].sentence;
    assert!(sentence.starts_with("Remove "), "{sentence}");
    assert!(sentence.contains(&native("polyglot/cmain")), "{sentence}");
    assert!(sentence.contains("name it as a deliverable"), "{sentence}");
    assert_eq!(
        findings[0].path.as_deref(),
        Some(root.join("polyglot/cmain").as_path())
    );

    // Naming it as a deliverable resolves the finding.
    let contract = Contract {
        required_paths: vec!["polyglot/cmain".into()],
        ..Contract::default()
    };
    assert!(check(&contract, &root, &files, Some(4), Some(3)).is_empty());
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn coverage_data_away_from_its_source_is_outside_the_tree() {
    let root = fixture("sqlite");
    write(&root, "sqlite/src/btree.c", b"/* sqlite */\n");
    write(&root, "sqlite-gcov-build/btree.gcno", b"gcno");
    write(&root, "sqlite-gcov-build/btree.o", b"obj");
    let mut files = TaskFiles::default();
    files.created.insert("sqlite-gcov-build/btree.gcno".into());
    files.created.insert("sqlite-gcov-build/btree.o".into());
    let findings = check(&Contract::default(), &root, &files, Some(2), Some(1));
    assert_eq!(kinds(&findings), vec![FindingKind::CoverageOutsideTree]);
    assert!(
        findings[0].sentence.starts_with("Move "),
        "{}",
        findings[0].sentence
    );
    assert!(
        findings[0].sentence.contains(&native("sqlite/src")),
        "{}",
        findings[0].sentence
    );

    // With a declared coverage tree the expected place is the contract's.
    let contract = Contract {
        coverage_tree: Some("sqlite".into()),
        ..Contract::default()
    };
    let findings = check(&contract, &root, &files, Some(2), Some(1));
    assert_eq!(kinds(&findings), vec![FindingKind::CoverageOutsideTree]);
    assert!(
        findings[0]
            .sentence
            .contains(&root.join("sqlite").display().to_string()),
        "{}",
        findings[0].sentence
    );

    // Coverage beside its source is fine.
    let mut beside = TaskFiles::default();
    write(&root, "sqlite/src/btree.gcno", b"gcno");
    beside.created.insert("sqlite/src/btree.gcno".into());
    assert!(check(&contract, &root, &beside, Some(2), Some(1)).is_empty());
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn a_mutation_after_the_last_verification_is_stale() {
    let root = fixture("stale");
    write(&root, "src/x.rs", b"fn x(){}\n");
    let mut files = TaskFiles::default();
    files.modified.insert("src/x.rs".into());
    let findings = check(&Contract::default(), &root, &files, Some(3), Some(5));
    assert_eq!(kinds(&findings), vec![FindingKind::StaleVerification]);
    assert!(
        findings[0].sentence.contains("cell 5"),
        "{}",
        findings[0].sentence
    );
    assert!(
        findings[0].sentence.contains("cell 3"),
        "{}",
        findings[0].sentence
    );

    // Without any declared verification a mutating task is not held for
    // never having run one; with checks or a `[contract]` declared it is.
    let findings = check(&Contract::default(), &root, &files, None, Some(5));
    assert!(findings.is_empty(), "{findings:?}");
    let expecting = Contract {
        verification_expected: true,
        ..Contract::default()
    };
    let findings = check(&expecting, &root, &files, None, Some(5));
    assert_eq!(kinds(&findings), vec![FindingKind::NoVerification]);

    let relaxed = Contract {
        require_fresh_verification: false,
        ..Contract::default()
    };
    assert!(check(&relaxed, &root, &files, None, Some(5)).is_empty());
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn a_clean_tree_has_no_findings() {
    let root = fixture("clean");
    write(&root, "src/lib.rs", b"pub fn f(){}\n");
    write(&root, "src/new.rs", b"pub fn g(){}\n");
    write(&root, "target/debug/lib.o", b"obj");
    let mut files = TaskFiles::default();
    files.modified.insert("src/lib.rs".into());
    files.created.insert("src/new.rs".into());
    assert!(check(&Contract::default(), &root, &files, Some(6), Some(5)).is_empty());
    assert!(
        check(
            &Contract::default(),
            &root,
            &TaskFiles::default(),
            None,
            None
        )
        .is_empty()
    );
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn contract_driven_findings_name_the_path_and_the_fix() {
    let root = fixture("contract");
    write(&root, "polyglot/main.py.c", b"c\n");
    write(&root, "polyglot/notes.txt", b"n\n");
    write(&root, "build/x.log", b"log\n");
    write(
        &root,
        ".sterna/checks.toml",
        b"[contract]\nrequired = [\"polyglot/main.py.c\", \"README.md\"]\nforbidden = [\"build/**\"]\n[contract.exclusive]\n\"polyglot\" = [\"main.py.c\"]\n",
    );
    let contract = load_contract(&root).unwrap();
    assert!(contract.require_fresh_verification);
    let mut files = TaskFiles::default();
    files.modified.insert("polyglot/main.py.c".into());
    files.created.insert("polyglot/notes.txt".into());
    files.created.insert("build/x.log".into());
    let findings = check(&contract, &root, &files, Some(2), Some(1));
    assert_eq!(
        kinds(&findings),
        vec![
            FindingKind::RequiredMissing,
            FindingKind::ForbiddenPresent,
            FindingKind::ExclusiveDirExtra,
        ]
    );
    assert!(
        findings[0].sentence.contains("README.md"),
        "{}",
        findings[0].sentence
    );
    assert!(
        findings[1].sentence.contains(&native("build/x.log")),
        "{}",
        findings[1].sentence
    );
    assert!(
        findings[2].sentence.contains(&native("polyglot/notes.txt")),
        "{}",
        findings[2].sentence
    );
    assert!(
        findings[2].sentence.contains("may contain only main.py.c"),
        "{}",
        findings[2].sentence
    );
    assert!(load_contract(&fixture("no-contract")).is_ok());
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn fresh_checker_evidence_carries_no_narrative_and_bounds_the_diff() {
    let control = "PARENT_RATIONALE_MUST_NOT_APPEAR";
    let findings = vec![Finding {
        kind: FindingKind::StaleVerification,
        path: None,
        sentence: "Re-run the verification.".into(),
    }];
    let facts = vec!["edited /app/x.c (cell 1)".to_string()];
    let diff = "+line\n".repeat(10_000);
    let evidence = fresh_checker_evidence("Add coverage to sqlite", &diff, &facts, &findings, &[]);
    let _ = control;
    assert!(!evidence.contains(control));
    assert!(evidence.starts_with("## Original request\nAdd coverage to sqlite\n"));
    assert!(evidence.contains("## Diff\n"));
    assert!(
        evidence.contains("[diff truncated:"),
        "{}",
        &evidence[evidence.len() - 300..]
    );
    assert!(evidence.contains("## Facts\n- edited /app/x.c (cell 1)\n"));
    assert!(evidence.contains("## Final-state findings\n- Re-run the verification.\n"));
    assert!(evidence.ends_with("Does the current state satisfy the original request?\n"));
    assert!(evidence.len() < 26 * 1024, "{}", evidence.len());
}
