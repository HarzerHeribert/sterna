use std::path::PathBuf;

use sterna::project::instructions;
use sterna::sandbox::profile::Profile;

fn fixture(label: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!(
        "sterna-instructions-{label}-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&root).unwrap();
    root
}

#[test]
fn nested_scopes_apply_root_to_target_and_exclude_siblings() {
    let root = fixture("nested");
    std::fs::create_dir_all(root.join("app/deep")).unwrap();
    std::fs::create_dir_all(root.join("sibling")).unwrap();
    std::fs::write(root.join("AGENTS.md"), "root agents").unwrap();
    std::fs::write(root.join("CLAUDE.md"), "root claude").unwrap();
    std::fs::write(root.join("app/AGENTS.md"), "app agents").unwrap();
    std::fs::write(root.join("sibling/CLAUDE.md"), "sibling only").unwrap();
    let profile = Profile::compile(&root, None);
    let rendered = instructions::for_paths(&profile, &[root.join("app/deep/new.rs")]);
    assert!(
        rendered.contains("root agents")
            && rendered.contains("root claude")
            && rendered.contains("app agents")
    );
    assert!(!rendered.contains("sibling only"));
    assert!(rendered.contains("equal scope") && rendered.contains("scope `app`"));
    let loaded = instructions::docs_for_paths(&profile, &[root.join("app/deep/new.rs")]);
    assert!(loaded.complete);
    assert_eq!(loaded.documents.len(), 3);
    assert!(loaded.documents.iter().all(|doc| doc.path.is_absolute()));
    assert_eq!(loaded.documents[0].scope, profile.root());
    assert_eq!(loaded.documents[2].scope, profile.root().join("app"));
    let index = instructions::root(&profile);
    assert!(index.contains("app/AGENTS.md") && index.contains("sibling/CLAUDE.md"));
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn an_agents_md_copied_as_claude_md_is_sent_once() {
    let root = fixture("copies");
    std::fs::write(root.join("AGENTS.md"), "RUN_THE_GATE\n").unwrap();
    std::fs::write(root.join("CLAUDE.md"), "RUN_THE_GATE\n\n").unwrap();
    let profile = Profile::compile(&root, None);
    let rendered = instructions::root(&profile);
    assert_eq!(rendered.matches("RUN_THE_GATE").count(), 1, "{rendered}");
    let _ = std::fs::remove_dir_all(root);
}

#[test]
#[cfg(unix)]
fn symlink_escape_and_denied_scope_are_never_read() {
    use std::os::unix::fs::symlink;
    let root = fixture("confined");
    let outside = fixture("outside");
    std::fs::create_dir_all(root.join("private")).unwrap();
    std::fs::create_dir_all(root.join("external-scope")).unwrap();
    std::fs::write(root.join("private/AGENTS.md"), "DENIED_SECRET").unwrap();
    std::fs::write(outside.join("AGENTS.md"), "OUTSIDE_SECRET").unwrap();
    symlink(outside.join("AGENTS.md"), root.join("linked-AGENTS.md")).unwrap();
    symlink(&outside, root.join("linked-dir")).unwrap();
    symlink(
        outside.join("AGENTS.md"),
        root.join("external-scope/AGENTS.md"),
    )
    .unwrap();
    let settings = r#"{"permissions":{"deny":["Read(private/**)"]}}"#;
    let profile = Profile::compile(&root, Some(settings));
    let rendered = instructions::root(&profile);
    assert!(!rendered.contains("DENIED_SECRET") && !rendered.contains("OUTSIDE_SECRET"));
    assert!(rendered.contains("Instruction coverage is incomplete"));
    assert!(!instructions::index(&profile).complete);
    let scoped = instructions::for_paths(
        &profile,
        &[
            root.join("private/file.rs"),
            root.join("linked-dir/file.rs"),
        ],
    );
    assert!(!scoped.contains("DENIED_SECRET") && !scoped.contains("OUTSIDE_SECRET"));
    let _ = std::fs::remove_dir_all(root);
    let _ = std::fs::remove_dir_all(outside);
}

#[test]
fn oversized_documents_are_omitted_with_a_clear_bounded_notice() {
    let root = fixture("oversized");
    std::fs::write(root.join("AGENTS.md"), vec![b'x'; 64 * 1024 + 1]).unwrap();
    let profile = Profile::compile(&root, None);
    let rendered = instructions::root(&profile);
    assert!(!rendered.contains(&"x".repeat(1024)));
    assert!(rendered.contains("Instruction coverage is incomplete: per-document byte limit"));
    // The reason line says *that* something was dropped; this says *which*,
    // so the model can read it for itself instead of proceeding as though the
    // policy it was handed were whole.
    assert!(
        rendered.contains("Instruction documents not loaded:")
            && rendered.contains("`AGENTS.md` — per-document byte limit"),
        "the omitted document is named, not merely counted:\n{rendered}"
    );
    let loaded = instructions::docs_for_paths(&profile, &[root.join("file.rs")]);
    assert!(!loaded.complete && loaded.documents.is_empty());
    assert!(loaded.omissions.iter().any(|omission| {
        omission.path.as_deref() == Some(profile.root().join("AGENTS.md").as_path())
            && omission.reason == "per-document byte limit"
    }));
    let _ = std::fs::remove_dir_all(root);
}

/// The ordinary session gains no notice: a project whose instructions all fit
/// says nothing about bounds at all. This is the risk of the whole package --
/// a notice that fires when nothing was dropped is noise in every session.
#[test]
fn a_complete_load_says_nothing_about_omissions() {
    let root = fixture("complete-load");
    std::fs::write(root.join("AGENTS.md"), "root policy\n").unwrap();
    let profile = Profile::compile(&root, None);
    let rendered = instructions::root(&profile);
    assert!(rendered.contains("root policy"));
    assert!(
        !rendered.contains("Instruction coverage is incomplete")
            && !rendered.contains("Instruction documents not loaded"),
        "nothing was dropped, so nothing is said:\n{rendered}"
    );
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn denied_known_document_makes_allowed_target_coverage_incomplete() {
    let root = fixture("denied-doc");
    std::fs::create_dir_all(root.join("app")).unwrap();
    std::fs::write(root.join("app/AGENTS.md"), "hidden rules").unwrap();
    let settings = r#"{"permissions":{"deny":["Read(app/AGENTS.md)"]}}"#;
    let profile = Profile::compile(&root, Some(settings));
    let loaded = instructions::docs_for_paths(&profile, &[root.join("app/new.rs")]);
    assert!(!loaded.complete);
    assert!(loaded.documents.is_empty());
    assert!(loaded.omissions.iter().any(|omission| {
        omission.reason.contains("denied")
            && omission.path.as_deref() == Some(profile.root().join("app/AGENTS.md").as_path())
    }));
    let _ = std::fs::remove_dir_all(root);
}

#[test]
#[cfg(unix)]
fn internal_document_symlink_keeps_its_declaring_scope() {
    use std::os::unix::fs::symlink;
    let root = fixture("linked-scope");
    std::fs::create_dir_all(root.join("app")).unwrap();
    std::fs::create_dir_all(root.join("team")).unwrap();
    std::fs::create_dir_all(root.join("shared")).unwrap();
    std::fs::write(root.join("shared/rules.md"), "linked rules").unwrap();
    symlink("../shared/rules.md", root.join("app/AGENTS.md")).unwrap();
    symlink("../shared/rules.md", root.join("team/AGENTS.md")).unwrap();
    let profile = Profile::compile(&root, None);
    let indexed = instructions::index(&profile);
    assert!(indexed.complete);
    assert_eq!(indexed.paths.len(), 2);
    assert!(
        indexed
            .paths
            .contains(&profile.root().join("app/AGENTS.md"))
    );
    assert!(
        indexed
            .paths
            .contains(&profile.root().join("team/AGENTS.md"))
    );
    let loaded = instructions::docs_for_paths(&profile, &indexed.paths);
    assert!(loaded.complete);
    assert_eq!(loaded.documents.len(), 2);
    assert!(
        loaded
            .documents
            .iter()
            .all(|doc| doc.path == profile.root().join("shared/rules.md"))
    );
    let scopes: Vec<_> = loaded.documents.iter().map(|doc| &doc.scope).collect();
    assert!(scopes.contains(&&profile.root().join("app")));
    assert!(scopes.contains(&&profile.root().join("team")));
    let _ = std::fs::remove_dir_all(root);
}
