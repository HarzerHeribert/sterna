use std::path::PathBuf;

use sha2::{Digest, Sha256};
use sterna::sandbox::profile::Profile;
use sterna::tools::exact_edit;

fn fixture(label: &str) -> PathBuf {
    let root =
        std::env::temp_dir().join(format!("sterna-exact-edit-{label}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    root
}

/// A directory outside every writable place: the machine's temp folders are
/// writable, so an "outside" fixture lives under the test target's own tmpdir.
fn outside_fixture(label: &str) -> PathBuf {
    let root = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("sterna-exact-edit-{label}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    root
}

fn hash(text: &str) -> String {
    format!("{:x}", Sha256::digest(text.as_bytes()))
}

#[test]
fn exact_versioned_edit_is_atomic_and_reports_hashes_and_lines() {
    let root = fixture("success");
    let path = root.join("file.txt");
    std::fs::write(&path, "one\ntwo\nthree\n").unwrap();
    let profile = Profile::compile(&root, None);
    let result =
        exact_edit::apply(&profile, &path, &hash("one\ntwo\nthree\n"), "two", "2\nII").unwrap();
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "one\n2\nII\nthree\n"
    );
    assert_eq!(result.before_sha256, hash("one\ntwo\nthree\n"));
    assert_eq!(result.after_sha256, hash("one\n2\nII\nthree\n"));
    assert_eq!(
        result.changed_lines,
        exact_edit::ChangedLines {
            start: 2,
            before: 1,
            after: 2
        }
    );
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn stale_missing_ambiguous_and_noop_edits_do_not_write() {
    let root = fixture("refusals");
    let path = root.join("file.txt");
    std::fs::write(&path, "same same\n").unwrap();
    let profile = Profile::compile(&root, None);
    // The stale case matches the whole file exactly once, so the version check
    // is the only thing that can refuse it: without it the edit would land.
    let stale = exact_edit::apply(&profile, &path, &hash("old"), "same same\n", "new").unwrap_err();
    assert_eq!(stale.kind, "stale_hash");
    assert_eq!(
        stale.message,
        "The source version changed; refresh context before editing."
    );
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "same same\n");
    for result in [
        exact_edit::apply(&profile, &path, &hash("same same\n"), "missing", "new"),
        exact_edit::apply(&profile, &path, &hash("same same\n"), "same", "new"),
        exact_edit::apply(&profile, &path, &hash("same same\n"), "same", "same"),
    ] {
        assert!(result.is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "same same\n");
    }
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn denied_outside_and_invalid_utf8_targets_are_refused() {
    let root = fixture("confined");
    let outside = outside_fixture("outside");
    let denied = root.join("denied.txt");
    let external = outside.join("external.txt");
    let binary = root.join("binary.txt");
    std::fs::write(&denied, "secret").unwrap();
    std::fs::write(&external, "outside").unwrap();
    std::fs::write(&binary, [0xff, 0xfe]).unwrap();
    let profile = Profile::compile(
        &root,
        Some(r#"{"permissions":{"deny":["Write(denied.txt)"]}}"#),
    );
    assert!(exact_edit::apply(&profile, &denied, &hash("secret"), "secret", "x").is_err());
    assert!(exact_edit::apply(&profile, &external, &hash("outside"), "outside", "x").is_err());
    assert!(exact_edit::apply(&profile, &binary, &hash_bytes(&[0xff, 0xfe]), "x", "y").is_err());
    let _ = std::fs::remove_dir_all(root);
    let _ = std::fs::remove_dir_all(outside);
}

fn hash_bytes(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

/// Several hunks are one mutation: located against the original text,
/// installed once, and reported per hunk in the caller's order.
#[test]
fn multi_hunk_edit_applies_every_hunk_against_the_original_text() {
    let root = fixture("hunks");
    let path = root.join("file.txt");
    std::fs::write(&path, "one\ntwo\nthree\nfour\n").unwrap();
    let profile = Profile::compile(&root, None);
    // Given out of file order, so the result proves hunks keep the caller's
    // order while the text is rebuilt in offset order.
    let result = exact_edit::apply_hunks(
        &profile,
        &path,
        &hash("one\ntwo\nthree\nfour\n"),
        &["four".to_string(), "one".to_string()],
        &["4\nIV".to_string(), "1".to_string()],
    )
    .unwrap();
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "1\ntwo\nthree\n4\nIV\n"
    );
    assert_eq!(result.hunks.len(), 2);
    assert_eq!(
        result.hunks[0],
        exact_edit::ChangedLines {
            start: 4,
            before: 1,
            after: 2
        }
    );
    assert_eq!(
        result.hunks[1],
        exact_edit::ChangedLines {
            start: 1,
            before: 1,
            after: 1
        }
    );
    assert_eq!(result.changed_lines, result.hunks[0]);
    assert_eq!(result.after_sha256, hash("1\ntwo\nthree\n4\nIV\n"));
    let _ = std::fs::remove_dir_all(root);
}

/// Every refusal names the hunk and the reason, and writes nothing.
#[test]
fn multi_hunk_refusals_name_the_hunk_and_leave_the_file_untouched() {
    let root = fixture("hunk-refusals");
    let path = root.join("file.txt");
    let text = "one\ntwo\ntwo\nthree\n";
    std::fs::write(&path, text).unwrap();
    let profile = Profile::compile(&root, None);
    let s = |items: &[&str]| {
        items
            .iter()
            .map(|item| item.to_string())
            .collect::<Vec<_>>()
    };
    let cases: Vec<(Vec<String>, Vec<String>, &str, &str)> = vec![
        (
            s(&["one", "three"]),
            s(&["1"]),
            "hunk_count_mismatch",
            "2 hunk(s)",
        ),
        (s(&[]), s(&[]), "hunk_count_mismatch", "at least one"),
        (
            s(&["one", "absent"]),
            s(&["1", "?"]),
            "missing_match",
            "hunk 1",
        ),
        (
            s(&["one", "two"]),
            s(&["1", "2"]),
            "ambiguous_match",
            "hunk 1",
        ),
        (
            s(&["one\ntwo", "two\ntwo"]),
            s(&["a", "b"]),
            "overlapping_hunks",
            "hunks 0 and 1",
        ),
        (
            s(&["one", "three"]),
            s(&["1", "three"]),
            "edit_refused",
            "hunk 1",
        ),
        (s(&["one", ""]), s(&["1", "x"]), "edit_refused", "hunk 1"),
    ];
    for (olds, replacements, kind, names) in cases {
        let error = exact_edit::apply_hunks(&profile, &path, &hash(text), &olds, &replacements)
            .unwrap_err();
        assert_eq!(error.kind, kind, "{olds:?}: {error:?}");
        assert!(error.message.contains(names), "{olds:?}: {error:?}");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), text);
    }
    // The version check is the same one the single form makes.
    let stale = exact_edit::apply_hunks(&profile, &path, &hash("old"), &s(&["one"]), &s(&["1"]))
        .unwrap_err();
    assert_eq!(stale.kind, "stale_hash");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), text);
    let _ = std::fs::remove_dir_all(root);
}

/// The single form reports its one hunk under `hunks` too, so a reader of
/// the result has one field to look at.
#[test]
fn the_single_form_reports_its_hunk_in_both_fields() {
    let root = fixture("single-hunks");
    let path = root.join("file.txt");
    std::fs::write(&path, "one\ntwo\n").unwrap();
    let profile = Profile::compile(&root, None);
    let result = exact_edit::apply(&profile, &path, &hash("one\ntwo\n"), "two", "2").unwrap();
    assert_eq!(result.hunks, vec![result.changed_lines.clone()]);
    let _ = std::fs::remove_dir_all(root);
}

/// A refusal is only worth a round trip if the next attempt can be a
/// correction. These pin what a reader is actually told.
#[test]
fn an_ambiguous_anchor_names_the_lines_it_matched() {
    let root = fixture("ambiguous-lines");
    let path = root.join("file.txt");
    // Two character-identical lines, the shape a "one field read in many
    // places" refactor meets constantly.
    let text = "alpha\nsame line\nbeta\ngamma\nsame line\ndelta\n";
    std::fs::write(&path, text).unwrap();
    let profile = Profile::compile(&root, None);

    let error =
        exact_edit::apply(&profile, &path, &hash(text), "same line\n", "other\n").unwrap_err();
    assert_eq!(error.kind, "ambiguous_match");
    assert!(error.message.contains("occurs 2 times"), "{error:?}");
    assert!(error.message.contains("lines 2 and 5"), "{error:?}");
    // The kind is in the message too: `Display` is all a ToolError carries.
    assert!(error.message.contains("ambiguous_match"), "{error:?}");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), text);

    let s = |items: &[&str]| items.iter().map(|i| i.to_string()).collect::<Vec<_>>();
    let hunked = exact_edit::apply_hunks(
        &profile,
        &path,
        &hash(text),
        &s(&["alpha", "same line\n"]),
        &s(&["a", "other\n"]),
    )
    .unwrap_err();
    assert!(
        hunked.message.starts_with("hunk 1 (ambiguous_match):"),
        "{hunked:?}"
    );
    assert!(hunked.message.contains("lines 2 and 5"), "{hunked:?}");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), text);
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn a_hopeless_anchor_names_a_bounded_list_and_says_how_many() {
    let root = fixture("ambiguous-many");
    let path = root.join("file.txt");
    let text: String = (0..3_000).map(|_| "x\n").collect();
    std::fs::write(&path, &text).unwrap();
    let profile = Profile::compile(&root, None);
    let error = exact_edit::apply(&profile, &path, &hash(&text), "x", "y").unwrap_err();
    assert_eq!(error.kind, "ambiguous_match");
    assert!(error.message.contains("more than 1000 times"), "{error:?}");
    assert!(
        error.message.contains("lines 1, 2, 3, 4, 5 and 995 more"),
        "{error:?}"
    );
    // Bounded: five named, never three thousand.
    assert!(
        error.message.len() < 200,
        "{} chars: {}",
        error.message.len(),
        error.message
    );
    assert_eq!(std::fs::read_to_string(&path).unwrap(), text);
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn a_missing_anchor_names_the_whitespace_that_made_it_miss() {
    let root = fixture("missing-whitespace");
    let path = root.join("file.txt");
    let text = "fn main() {\n    let answer = 42;\n}\n";
    std::fs::write(&path, text).unwrap();
    let profile = Profile::compile(&root, None);

    // Right text, wrong indentation — what retyping from memory produces.
    let error = exact_edit::apply(
        &profile,
        &path,
        &hash(text),
        "\t\tlet answer = 42;\n",
        "x\n",
    )
    .unwrap_err();
    assert_eq!(error.kind, "missing_match");
    assert!(error.message.contains("different indentation"), "{error:?}");
    assert!(error.message.contains("line 2"), "{error:?}");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), text);

    // Right text, wrong line endings.
    let crlf = "alpha\r\nbeta\r\n";
    let crlf_path = root.join("crlf.txt");
    std::fs::write(&crlf_path, crlf).unwrap();
    let ending =
        exact_edit::apply(&profile, &crlf_path, &hash(crlf), "alpha\nbeta\n", "x\n").unwrap_err();
    assert!(ending.message.contains("CRLF line endings"), "{ending:?}");
    assert!(ending.message.contains("line 1"), "{ending:?}");

    // Nothing provable: the old sentence, unchanged.
    let blank = exact_edit::apply(&profile, &path, &hash(text), "nowhere at all", "x").unwrap_err();
    assert!(
        blank.message.contains("exact match was not found"),
        "{blank:?}"
    );
    assert!(!blank.message.contains("but the same text"), "{blank:?}");
    let _ = std::fs::remove_dir_all(root);
}
