use std::path::PathBuf;
use sterna::contract::SessionId;
use sterna::runtime::isolate::Runtime;
use sterna::runtime::outcome::CellOutcome;
use sterna::sandbox::profile::Profile;

struct Fixture {
    root: PathBuf,
}
impl Fixture {
    fn new(label: &str) -> Self {
        let root = std::env::temp_dir().join(format!(
            "sterna-instruction-boundary-{}-{label}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("nested")).unwrap();
        std::fs::write(root.join("AGENTS.md"), "root policy").unwrap();
        std::fs::write(root.join("nested/AGENTS.md"), "nested policy").unwrap();
        Self { root }
    }
    fn runtime(&self, bash: bool) -> Runtime {
        let allow = if bash {
            r#"["Read(**)","Write(**)","Bash"]"#
        } else {
            r#"["Read(**)","Write(**)"]"#
        };
        let profile = Profile::compile(
            &self.root,
            Some(&format!(r#"{{"permissions":{{"allow":{allow}}}}}"#)),
        );
        Runtime::new(&profile, &SessionId::new("instructions")).with_instruction_context()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

#[test]
fn caught_boundary_cannot_write_and_prior_effect_is_not_replayed() {
    let fixture = Fixture::new("structured");
    let mut runtime = fixture.runtime(false);
    let first = fixture.root.join("first.txt");
    let blocked = fixture.root.join("nested/blocked.txt");
    let skipped = fixture.root.join("nested/skipped.txt");
    let source = format!(
        r#"
await write({{path:{first:?}, content:"once"}});
try {{ await write({{path:{blocked:?}, content:"blocked"}}); }} catch (_) {{}}
await write({{path:{skipped:?}, content:"skipped"}});
"#
    );
    assert!(matches!(
        runtime.run_cell(&source),
        CellOutcome::Yielded { .. }
    ));
    assert_eq!(std::fs::read_to_string(&first).unwrap(), "once");
    assert!(!blocked.exists());
    assert!(!skipped.exists(), "termination must not be catchable");
    let pending = runtime
        .pending_instructions()
        .expect("nested policy pending");
    assert!(!pending.fatal);
    assert!(pending.text.contains("nested policy"));

    runtime.acknowledge_instructions();
    let next = format!(r#"await write({{path:{blocked:?}, content:"done"}});"#);
    assert!(matches!(
        runtime.run_cell(&next),
        CellOutcome::Yielded { .. }
    ));
    assert_eq!(std::fs::read_to_string(&blocked).unwrap(), "done");
    assert_eq!(std::fs::read_to_string(&first).unwrap(), "once");
}

#[test]
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn opaque_bash_waits_for_all_indexed_instructions_before_spawn() {
    let fixture = Fixture::new("bash");
    let mut runtime = fixture.runtime(true);
    let marker = fixture.root.join("bash-ran.txt");
    let source = format!(
        "await bash({{command: 'echo ran > {}'}});",
        marker.display()
    );
    assert!(matches!(
        runtime.run_cell(&source),
        CellOutcome::Yielded { .. }
    ));
    assert!(!marker.exists());
    assert!(
        runtime
            .pending_instructions()
            .unwrap()
            .text
            .contains("nested policy")
    );
    runtime.acknowledge_instructions();
    let _ = runtime.run_cell(&source);
    assert!(marker.exists());
}

#[test]
fn incomplete_instruction_policy_cannot_be_acknowledged_into_permission() {
    let fixture = Fixture::new("oversized");
    std::fs::write(fixture.root.join("nested/AGENTS.md"), "x".repeat(70 * 1024)).unwrap();
    let mut runtime = fixture.runtime(false);
    let target = fixture.root.join("nested/no.txt");
    let source = format!(r#"await write({{path:{target:?}, content:"no"}});"#);
    assert!(matches!(
        runtime.run_cell(&source),
        CellOutcome::Yielded { .. }
    ));
    assert!(runtime.pending_instructions().unwrap().fatal);
    runtime.acknowledge_instructions();
    assert!(runtime.pending_instructions().unwrap().fatal);
    assert!(matches!(
        runtime.run_cell(&source),
        CellOutcome::Yielded { .. }
    ));
    assert!(!target.exists());
}

#[test]
fn changing_an_instruction_file_stops_later_calls_until_new_text_is_delivered() {
    let fixture = Fixture::new("changed-policy");
    let mut runtime = fixture.runtime(false);
    let policy = fixture.root.join("nested/AGENTS.md");
    let target = fixture.root.join("nested/after.txt");
    let discover = format!(r#"await read({{path:{policy:?}}});"#);
    let _ = runtime.run_cell(&discover);
    runtime.acknowledge_instructions();
    let source = format!(
        r#"await write({{path:{policy:?}, content:"new nested policy"}}); await write({{path:{target:?}, content:"too early"}});"#
    );
    assert!(matches!(
        runtime.run_cell(&source),
        CellOutcome::Yielded { .. }
    ));
    assert_eq!(
        std::fs::read_to_string(&policy).unwrap(),
        "new nested policy"
    );
    assert!(!target.exists());
    assert!(
        runtime
            .pending_instructions()
            .unwrap()
            .text
            .contains("new nested policy")
    );
}

#[test]
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn opaque_bash_accepts_more_than_eight_small_instruction_documents() {
    let fixture = Fixture::new("many-documents");
    for number in 0..9 {
        let directory = fixture.root.join(format!("scope-{number}"));
        std::fs::create_dir_all(&directory).unwrap();
        std::fs::write(
            directory.join("AGENTS.md"),
            format!("policy number {number}"),
        )
        .unwrap();
    }
    let mut runtime = fixture.runtime(true);
    let marker = fixture.root.join("many-ran.txt");
    let source = format!(
        "await bash({{command: 'echo ran > {}'}});",
        marker.display()
    );

    assert!(matches!(
        runtime.run_cell(&source),
        CellOutcome::Yielded { .. }
    ));
    let pending = runtime.pending_instructions().expect("policies pending");
    assert!(!pending.fatal);
    assert!(pending.text.contains("policy number 0"));
    assert!(pending.text.contains("policy number 8"));
    assert!(!marker.exists());

    runtime.acknowledge_instructions();
    let _ = runtime.run_cell(&source);
    assert!(marker.exists());
}

#[test]
fn externally_changed_instruction_text_is_delivered_before_a_dependent_write() {
    let fixture = Fixture::new("external-change");
    let mut runtime = fixture.runtime(false);
    let policy = fixture.root.join("nested/AGENTS.md");
    let target = fixture.root.join("nested/blocked-by-change.txt");
    let discover = format!(r#"await read({{path:{policy:?}}});"#);
    assert!(matches!(
        runtime.run_cell(&discover),
        CellOutcome::Yielded { .. }
    ));
    runtime.acknowledge_instructions();

    std::fs::write(&policy, "replacement nested policy").unwrap();
    let write = format!(r#"await write({{path:{target:?}, content:"too early"}});"#);
    assert!(matches!(
        runtime.run_cell(&write),
        CellOutcome::Yielded { .. }
    ));
    assert!(!target.exists());
    let pending = runtime.pending_instructions().expect("replacement pending");
    assert!(!pending.fatal);
    assert!(pending.text.contains("replacement nested policy"));
    assert!(pending.text.contains("replaces the earlier version"));
    assert!(!pending.text.contains("### `root policy`"));
}

/// A cell whose one `bash` call creates `path`, written for the interpreter
/// that answers `bash` on this host: `cmd.exe` on Windows
/// (`tools::registry::BASH`), which has no `touch`, and a POSIX shell
/// elsewhere. The line is a JS string literal through `{:?}`, so a Windows
/// path's backslashes arrive escaped rather than read as JS escapes.
fn create_file_cell(path: &std::path::Path) -> String {
    let line = if cfg!(windows) {
        format!("type nul > \"{}\"", path.display())
    } else {
        format!("touch {}", path.display())
    };
    format!("await bash({{command: {line:?}}});")
}

/// The 2026-09-13 full-suite run: a downloaded dataset of tens of thousands
/// of files exhausted the instruction index's entry budget and ended a
/// 32-cell task as "could not be loaded completely". A scan budget is a
/// notice, delivered once: the first shell call is blocked to deliver it and
/// runs when repeated; a document that exists and cannot be read (the
/// oversized test above) still stops the task.
#[test]
fn an_exhausted_index_budget_is_a_notice_once_and_never_a_stop() {
    let fixture = Fixture::new("index-budget");
    let data = fixture.root.join("data");
    std::fs::create_dir_all(&data).unwrap();
    for i in 0..10_050u32 {
        std::fs::write(data.join(format!("{i}.bin")), b"x").unwrap();
    }
    let mut runtime = fixture.runtime(true);
    let marker = fixture.root.join("ran.txt");
    let source = create_file_cell(&marker);
    assert!(matches!(
        runtime.run_cell(&source),
        CellOutcome::Yielded { .. }
    ));
    let pending = runtime.pending_instructions().unwrap();
    assert!(!pending.fatal, "a budget is not a stop: {}", pending.text);
    assert!(pending.text.contains("scan budget"), "{}", pending.text);
    assert!(
        pending.text.contains("directory entry limit"),
        "{}",
        pending.text
    );
    assert!(!marker.exists(), "the blocked call did not run");
    runtime.acknowledge_instructions();
    assert!(runtime.pending_instructions().is_none());
    // A cell without a `return` yields; what matters is that nothing is
    // pending and the command ran.
    let _ = runtime.run_cell(&source);
    assert!(
        runtime.pending_instructions().is_none(),
        "the repeated call must not block again: {:?}",
        runtime.pending_instructions().map(|pending| pending.text)
    );
    assert!(marker.exists(), "the repeated call ran");
    let again = create_file_cell(&fixture.root.join("again.txt"));
    let _ = runtime.run_cell(&again);
    assert!(
        runtime.pending_instructions().is_none(),
        "the notice is delivered once"
    );
    assert!(fixture.root.join("again.txt").exists());
}
