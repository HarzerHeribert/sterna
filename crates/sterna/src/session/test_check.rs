//! Whether a task's tests exercise what it changed, checked by the host and
//! costing the model nothing unless the answer is no.
//!
//! When a test command passes and the task has changed both test files and
//! code, the same command runs again on a copy of the tree with the code put
//! back as it was and the tests kept ([`RedCheck`]). A test that passes there
//! too does not exercise the change, and the model is told so; a test that
//! fails there is what a test should do, and nothing is said. It runs on its
//! own thread in the system's temp folder, so no turn waits for it.
//!
//! It knows no language or test runner: a test file is recognised by its
//! path, the command is the model's own, and the verdict is an exit code.

use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, TryRecvError};
use std::time::{Duration, Instant};

use crate::contract::SessionId;
use crate::sandbox::profile::Profile;
use crate::tools::invoke::{self, Args, CancellationToken, ToolContext};

/// How long a red run may take before it is abandoned.
const RED_DEADLINE: Duration = Duration::from_secs(600);
/// The largest file copied into a red run's tree.
const COPY_CAP: u64 = 64 * 1024 * 1024;

/// Whether `path` (relative, `/`-separated) is a test file by the
/// conventions most ecosystems share.
pub(super) fn is_test_path(path: &str) -> bool {
    crate::project::source_context::is_test(path)
        || path.contains(".test.")
        || path.contains(".spec.")
        || path.ends_with("Test.java")
        || path.ends_with("Tests.java")
}

/// Whether `command` names the test file `path` -- by its path, its path
/// without the extension, the dotted module that path spells, its stem, or
/// its directory: the ways runners are pointed at tests.
pub(super) fn command_names(command: &str, path: &str) -> bool {
    let without_extension = path.rsplit_once('.').map_or(path, |(head, _)| head);
    let module = without_extension.replace('/', ".");
    if command.contains(path) || command.contains(without_extension) || command.contains(&module) {
        return true;
    }
    let generic = ["tests", "test", "spec", "specs", "__tests__", "src", "lib"];
    let specific = |word: &str| word.len() >= 4 && !generic.contains(&word);
    let stem = Path::new(path)
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or("");
    let directory = Path::new(path)
        .parent()
        .and_then(|parent| parent.file_name())
        .and_then(|name| name.to_str())
        .unwrap_or("");
    (specific(stem) && names_word(command, stem))
        || (specific(directory) && names_word(command, directory))
}

/// Whether `command` runs the test file `path` rather than writing it: it
/// names the file (see [`command_names`]) and does not redirect into it or
/// hand it to a command that rewrites files. A cell that writes a test and
/// then runs it is two commands, and only the second is a run.
pub(super) fn runs_test(command: &str, path: &str) -> bool {
    let writes = command.match_indices(path).any(|(at, _)| {
        let before = command[..at].trim_end_matches(['\'', '"', ' ']);
        before.ends_with('>')
    }) || ["tee ", "sed -i", "cp ", "mv ", "patch "]
        .iter()
        .any(|writer| command.contains(writer) && command.contains(path));
    !writes && command_names(command, path)
}

/// Whether `word` occurs in `text` bounded by characters no identifier has.
fn names_word(text: &str, word: &str) -> bool {
    let ident = |c: char| c.is_alphanumeric() || c == '_';
    text.match_indices(word).any(|(at, _)| {
        let before = text[..at].chars().next_back();
        let after = text[at + word.len()..].chars().next();
        !before.is_some_and(ident) && !after.is_some_and(ident)
    })
}

/// A red run in flight.
pub(super) struct RedCheck {
    receiver: Receiver<Option<String>>,
}

impl RedCheck {
    /// Its notice once it has finished, `Some(None)` when it finished with
    /// nothing to say, `None` while it still runs.
    pub(super) fn poll(&self) -> Option<Option<String>> {
        match self.receiver.try_recv() {
            Ok(notice) => Some(notice),
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) => Some(None),
        }
    }
}

/// Starts the red run of `command`: the project copied into a temp folder,
/// each of `code` put back to what it held at task start (`None`: it did not
/// exist), the tests left as they are now, and `command` run there.
pub(super) fn start_red_check(
    profile: &Profile,
    session: &SessionId,
    command: &str,
    code: Vec<(PathBuf, Option<Vec<u8>>)>,
) -> RedCheck {
    let (sender, receiver) = std::sync::mpsc::channel();
    let (profile, session, command) = (profile.clone(), session.clone(), command.to_string());
    std::thread::spawn(move || {
        let _ = sender.send(red_run(&profile, &session, &command, &code));
    });
    RedCheck { receiver }
}

fn red_run(
    profile: &Profile,
    session: &SessionId,
    command: &str,
    code: &[(PathBuf, Option<Vec<u8>>)],
) -> Option<String> {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let root = profile.root();
    let scratch = std::env::temp_dir().join(format!(
        "sterna-test-check-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    let result = copy_tree(root, &scratch)
        .and_then(|()| put_back(&scratch, code))
        .and_then(|()| {
            let moved =
                command.replace(&root.display().to_string(), &scratch.display().to_string());
            let quoted = scratch.display().to_string().replace('\'', "'\\''");
            run_bounded(profile, session, &format!("cd '{quoted}' && {moved}"))
        });
    let _ = std::fs::remove_dir_all(&scratch);
    let exit = result?;
    let shown: String = command.chars().take(160).collect();
    let files = code.len();
    (exit == 0).then(|| {
        format!(
            "## Test check\n`{shown}` also passes on the code as it was before your changes to \
             {files} non-test file(s), with your test changes in place: those tests do not \
             exercise what you changed. Make a test that fails on the old behaviour."
        )
    })
}

/// Copies every file git tracks or would track into `to`.
fn copy_tree(root: &Path, to: &Path) -> Option<()> {
    let listed = std::process::Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["ls-files", "-co", "--exclude-standard", "-z"])
        .output()
        .ok()
        .filter(|output| output.status.success())?;
    for relative in listed.stdout.split(|byte| *byte == 0) {
        let Ok(relative) = std::str::from_utf8(relative) else {
            continue;
        };
        if relative.is_empty() {
            continue;
        }
        let from = root.join(relative);
        let Ok(metadata) = std::fs::metadata(&from) else {
            continue;
        };
        if !metadata.is_file() || metadata.len() > COPY_CAP {
            continue;
        }
        let target = to.join(relative);
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent).ok()?;
        }
        std::fs::copy(&from, &target).ok()?;
    }
    Some(())
}

fn put_back(scratch: &Path, code: &[(PathBuf, Option<Vec<u8>>)]) -> Option<()> {
    for (relative, bytes) in code {
        let target = scratch.join(relative);
        match bytes {
            Some(bytes) => {
                if let Some(parent) = target.parent() {
                    std::fs::create_dir_all(parent).ok()?;
                }
                std::fs::write(&target, bytes).ok()?;
            }
            None => {
                let _ = std::fs::remove_file(&target);
            }
        }
    }
    Some(())
}

/// Runs `command` confined, as a foreground `bash` would, answering its exit
/// code; `None` when it could not run or ran past [`RED_DEADLINE`].
fn run_bounded(profile: &Profile, session: &SessionId, command: &str) -> Option<i32> {
    let token = CancellationToken::new();
    let finished = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    {
        let (token, finished) = (token.clone(), finished.clone());
        std::thread::spawn(move || {
            let started = Instant::now();
            while !finished.load(std::sync::atomic::Ordering::Relaxed) {
                if started.elapsed() > RED_DEADLINE {
                    token.cancel();
                    return;
                }
                std::thread::sleep(Duration::from_millis(200));
            }
        });
    }
    let context = ToolContext { profile, session };
    let outcome = invoke::run_cancellable(
        &context,
        &token,
        "bash",
        &Args::new().with("command", command),
    );
    finished.store(true, std::sync::atomic::Ordering::Relaxed);
    outcome.ok()?.exit_code
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_command_names_a_test_file_the_ways_runners_are_pointed_at_tests() {
        let django = "tests/prefetch_related/tests.py";
        assert!(command_names(
            "python tests/runtests.py prefetch_related --parallel 1",
            django
        ));
        assert!(!command_names("python tests/runtests.py schema", django));
        let rust = "crates/sterna/tests/context_tools.rs";
        assert!(command_names(
            "cargo test -p sterna --test context_tools",
            rust
        ));
        assert!(!command_names("cargo test -p sterna --test context", rust));
        let python = "tests/test_parser.py";
        assert!(command_names(
            "pytest tests/test_parser.py::test_one -q",
            python
        ));
        assert!(command_names("python -m pytest tests.test_parser", python));
        assert!(!command_names("pytest tests/test_lexer.py", python));
    }

    #[test]
    fn writing_a_test_file_is_not_running_it() {
        let path = "tests/check_value.sh";
        assert!(runs_test("sh tests/check_value.sh", path));
        assert!(!runs_test("printf 'grep x f' > tests/check_value.sh", path));
        assert!(!runs_test("echo x >> 'tests/check_value.sh'", path));
        assert!(!runs_test("echo x | tee tests/check_value.sh", path));
        assert!(!runs_test("cp /tmp/t tests/check_value.sh", path));
    }

    /// A git repository in a fresh temp folder, everything committed.
    #[cfg(unix)]
    fn repo(label: &str, files: &[(&str, &str)]) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "sterna-test-check-fixture-{label}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        for (path, text) in files {
            let path = root.join(path);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, text).unwrap();
        }
        for args in [
            &["init", "-q"][..],
            &["add", "-A"],
            &[
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@t",
                "commit",
                "-q",
                "-m",
                "start",
            ],
        ] {
            let ok = std::process::Command::new("git")
                .arg("-C")
                .arg(&root)
                .args(args)
                .status()
                .unwrap()
                .success();
            assert!(ok, "git {args:?}");
        }
        root
    }

    /// The red run's answer, once it has one.
    #[cfg(unix)]
    fn wait(check: &RedCheck) -> Option<String> {
        let started = Instant::now();
        loop {
            if let Some(done) = check.poll() {
                return done;
            }
            assert!(
                started.elapsed() < Duration::from_secs(60),
                "the red run never finished"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// The red run puts the code back and keeps the tests: a test that
    /// passes there does not exercise the change, and is the one reported;
    /// a test that fails there says nothing.
    #[cfg(unix)]
    #[test]
    fn a_red_run_reports_only_a_test_that_passes_without_the_change() {
        let root = repo(
            "red",
            &[("src/value.txt", "value = 1\n"), ("tests/README", "t\n")],
        );
        std::fs::write(root.join("src/value.txt"), "value = 2\n").unwrap();
        std::fs::write(
            root.join("tests/check.sh"),
            "grep -q 'value = 2' src/value.txt\n",
        )
        .unwrap();
        std::fs::write(root.join("tests/always.sh"), "exit 0\n").unwrap();
        let profile = Profile::compile(&root, None);
        let session = SessionId::new("red-run");
        let code = vec![(
            PathBuf::from("src/value.txt"),
            Some(b"value = 1\n".to_vec()),
        )];

        let exercised = wait(&start_red_check(
            &profile,
            &session,
            "sh tests/check.sh",
            code.clone(),
        ));
        assert_eq!(exercised, None);
        let idle = wait(&start_red_check(
            &profile,
            &session,
            "sh tests/always.sh",
            code,
        ));
        assert!(
            idle.as_deref()
                .is_some_and(|n| n.contains("also passes on the code as it was before")),
            "{idle:?}"
        );
        assert_eq!(
            std::fs::read_to_string(root.join("src/value.txt")).unwrap(),
            "value = 2\n"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn test_files_are_recognised_by_path_in_the_common_ecosystems() {
        for path in [
            "tests/foo.py",
            "pkg/foo_test.go",
            "src/test_foo.py",
            "web/button.test.tsx",
            "web/button.spec.ts",
            "src/main/java/FooTest.java",
        ] {
            assert!(is_test_path(path), "{path}");
        }
        for path in ["src/foo.py", "pkg/latest.go", "web/button.tsx"] {
            assert!(!is_test_path(path), "{path}");
        }
    }
}
