//! Pane is now Sterna, and a start moves what Pane saved: the global folder
//! and the project folder are renamed once, with every byte in them, and the
//! person is told in one line each. When the new folder already exists it is
//! the one used, and the old one is left exactly as it was.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

struct Scratch(PathBuf);

impl Scratch {
    fn new(label: &str) -> Self {
        let path =
            std::env::temp_dir().join(format!("sterna-relocate-{label}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(path.join("home")).unwrap();
        fs::create_dir_all(path.join("project")).unwrap();
        Self(path)
    }

    fn home(&self) -> PathBuf {
        self.0.join("home")
    }

    fn project(&self) -> PathBuf {
        self.0.join("project")
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn write(path: &Path, text: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, text).unwrap();
}

fn read(path: &Path) -> String {
    fs::read_to_string(path).unwrap_or_else(|error| panic!("{}: {error}", path.display()))
}

/// One start, as a person makes it: `sterna config` in the project, with the
/// scratch home as `HOME` and no `XDG_CONFIG_HOME`, so the global folder is
/// `<home>/.config/sterna` and the developer's own is never touched.
fn start(scratch: &Scratch) -> (String, String) {
    let output: Output = Command::new(env!("CARGO_BIN_EXE_sterna"))
        .arg("config")
        .arg("--root")
        .arg(scratch.project())
        .env("HOME", scratch.home())
        .env("USERPROFILE", scratch.home())
        .env_remove("XDG_CONFIG_HOME")
        .env_remove("ANTHROPIC_API_KEY")
        .env_remove("ANTHROPIC_AUTH_TOKEN")
        .env_remove("ANTHROPIC_BASE_URL")
        .output()
        .expect("sterna runs");
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert!(
        output.status.success(),
        "stdout: {stdout}\nstderr: {stderr}"
    );
    (stdout, stderr)
}

/// `sterna <words> <project>`, with the same scratch environment as
/// [`start`], whatever its exit status.
fn sterna(scratch: &Scratch, words: &[&str]) -> (String, String) {
    let output: Output = Command::new(env!("CARGO_BIN_EXE_sterna"))
        .args(words)
        .arg(scratch.project())
        .current_dir(scratch.project())
        .env("HOME", scratch.home())
        .env("USERPROFILE", scratch.home())
        .env_remove("XDG_CONFIG_HOME")
        .env_remove("ANTHROPIC_API_KEY")
        .env_remove("ANTHROPIC_AUTH_TOKEN")
        .env_remove("ANTHROPIC_BASE_URL")
        .output()
        .expect("sterna runs");
    (
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

#[test]
fn a_start_moves_the_old_folders_once_with_everything_in_them() {
    let scratch = Scratch::new("once");
    let old_global = scratch.home().join(".config").join("pane");
    let old_project = scratch.project().join(".pane");
    write(
        &old_global.join("config.toml"),
        "[model]\nparent = \"global-model\"\n",
    );
    write(&old_global.join("AGENTS.md"), "Prefer small commits.\n");
    write(
        &old_project.join("config.toml"),
        "[helpers]\nmodel = \"project-helper\"\n",
    );
    write(
        &old_project.join("sessions/s1.jsonl"),
        "{\"kind\":\"system\"}\n",
    );
    write(
        &old_project.join("learned.md"),
        "# Learned\n- src/gate.rs\n",
    );
    write(
        &scratch.project().join(".glasshouse/checks.toml"),
        "[checks.tests]\ncommand = \"true\"\n",
    );

    let (stdout, stderr) = start(&scratch);

    let new_global = scratch.home().join(".config").join("sterna");
    let new_project = scratch.project().join(".sterna");
    for (old, new) in [(&old_global, &new_global), (&old_project, &new_project)] {
        let said = format!("moved {} to {}", old.display(), new.display());
        assert_eq!(stderr.matches(&said).count(), 1, "{said}\n{stderr}");
        assert!(!old.exists(), "{} was moved, not copied", old.display());
    }
    assert_eq!(stderr.matches("Pane is now Sterna").count(), 3, "{stderr}");
    assert_eq!(
        read(&new_global.join("config.toml")),
        "[model]\nparent = \"global-model\"\n"
    );
    assert_eq!(
        read(&new_global.join("AGENTS.md")),
        "Prefer small commits.\n"
    );
    assert_eq!(
        read(&new_project.join("config.toml")),
        "[helpers]\nmodel = \"project-helper\"\n"
    );
    assert_eq!(
        read(&new_project.join("sessions/s1.jsonl")),
        "{\"kind\":\"system\"}\n"
    );
    assert_eq!(
        read(&new_project.join("learned.md")),
        "# Learned\n- src/gate.rs\n"
    );
    assert_eq!(
        read(&new_project.join("checks.toml")),
        "[checks.tests]\ncommand = \"true\"\n"
    );
    assert!(
        !scratch.project().join(".glasshouse").exists(),
        "the folder the checks left is empty and goes"
    );
    assert!(
        stdout.contains("global-model") && stdout.contains("project-helper"),
        "the moved settings are the ones read: {stdout}"
    );

    let (again, said_again) = start(&scratch);
    assert!(
        !said_again.contains("Pane is now Sterna"),
        "a second start has nothing to move and says nothing: {said_again}"
    );
    assert!(again.contains("global-model"), "{again}");
}

#[test]
fn a_new_folder_that_exists_is_used_and_the_old_one_is_left_alone() {
    let scratch = Scratch::new("both");
    let old_global = scratch.home().join(".config").join("pane");
    let new_global = scratch.home().join(".config").join("sterna");
    let old_project = scratch.project().join(".pane");
    let new_project = scratch.project().join(".sterna");
    write(
        &old_global.join("config.toml"),
        "[model]\nparent = \"old-model\"\n",
    );
    write(
        &new_global.join("config.toml"),
        "[model]\nparent = \"new-model\"\n",
    );
    write(&old_project.join("sessions/old.jsonl"), "old\n");
    write(&new_project.join("sessions/new.jsonl"), "new\n");

    let (stdout, stderr) = start(&scratch);

    assert!(
        !stderr.contains("Pane is now Sterna"),
        "a start says nothing about a pair it leaves alone: {stderr}"
    );
    let (_, again) = start(&scratch);
    assert!(!again.contains("Pane is now Sterna"), "{again}");
    // `doctor` names each pair it left alone; it prints the project by its
    // canonical path, so the names are matched by their tails, in the
    // platform's own separator.
    let (doctor, _) = sterna(&scratch, &["doctor", "--root"]);
    let tail = |a: &str, b: &str| format!("{a}{}{b}", std::path::MAIN_SEPARATOR);
    for (old, new) in [
        (tail(".config", "pane"), tail(".config", "sterna")),
        (tail("project", ".pane"), tail("project", ".sterna")),
    ] {
        let said = format!("{new} is used; the older ");
        assert!(
            doctor.lines().any(|line| line.contains(&said)
                && line.contains(&format!("{old} from before the rename is still there"))),
            "{said}\n{doctor}"
        );
    }
    assert_eq!(
        read(&old_global.join("config.toml")),
        "[model]\nparent = \"old-model\"\n"
    );
    assert_eq!(
        read(&new_global.join("config.toml")),
        "[model]\nparent = \"new-model\"\n"
    );
    assert_eq!(read(&old_project.join("sessions/old.jsonl")), "old\n");
    assert_eq!(read(&new_project.join("sessions/new.jsonl")), "new\n");
    assert!(
        !new_project.join("sessions/old.jsonl").exists(),
        "nothing is merged into the folder in use"
    );
    assert!(
        stdout.contains("new-model") && !stdout.contains("old-model"),
        "the new folder is the one read: {stdout}"
    );
}

/// A session start moves the project folder before it looks for anything in
/// it: the session Pane saved is the one `--sessions` lists.
#[test]
fn the_sessions_pane_saved_are_listed_after_the_move() {
    let scratch = Scratch::new("sessions");
    let old_project = scratch.project().join(".pane");
    // A session somebody asked something in: an empty one is not listed.
    write(
        &old_project.join("sessions/k3v9ab.jsonl"),
        "{\"kind\":\"turn\",\"role\":\"user\",\"text\":\"hello\",\"blocks\":[]}\n",
    );

    let output = Command::new(env!("CARGO_BIN_EXE_sterna"))
        .arg("--sessions")
        .current_dir(scratch.project())
        .env("HOME", scratch.home())
        .env("USERPROFILE", scratch.home())
        .env_remove("XDG_CONFIG_HOME")
        .output()
        .expect("sterna runs");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(
        output.status.success(),
        "stdout: {stdout}\nstderr: {stderr}"
    );
    assert!(stdout.contains("k3v9ab"), "{stdout}");
    assert!(
        stderr.contains("Pane is now Sterna: moved") && stderr.contains(".sterna"),
        "{stderr}"
    );
    assert!(
        scratch
            .project()
            .join(".sterna/sessions/k3v9ab.jsonl")
            .is_file()
    );
}

/// A start that ends before its session opens -- here a `--resume` id that
/// does not exist -- still says what it moved. The next start finds nothing
/// left to move, so a line lost here would never be said.
#[test]
fn a_start_refused_before_the_session_opens_still_says_what_it_moved() {
    let scratch = Scratch::new("refused");
    let old_project = scratch.project().join(".pane");
    write(&old_project.join("sessions/20260901-abc.jsonl"), "{}\n");

    let output = Command::new(env!("CARGO_BIN_EXE_sterna"))
        .args(["--resume", "zzzz-nope"])
        .current_dir(scratch.project())
        .env("HOME", scratch.home())
        .env("USERPROFILE", scratch.home())
        .env_remove("XDG_CONFIG_HOME")
        .env_remove("ANTHROPIC_BASE_URL")
        .output()
        .expect("sterna runs");
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(
        !output.status.success(),
        "the unknown id is refused: {stderr}"
    );
    assert!(stderr.contains("zzzz-nope"), "{stderr}");
    // The session names the project as it was given: here, the current
    // directory, joined the way this platform joins a path.
    let here = Path::new(".");
    assert!(
        stderr.contains(&format!(
            "Pane is now Sterna: moved {} to {}.",
            here.join(".pane").display(),
            here.join(".sterna").display()
        )),
        "{stderr}"
    );
    assert!(!old_project.exists(), "{stderr}");
}

fn git(dir: &Path, words: &[&str]) -> bool {
    Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(words)
        .output()
        .expect("git runs")
        .status
        .success()
}

/// A project that kept `.pane/` or `.pane/sessions/` out of git keeps the
/// moved folder out of git too, and is told how.
#[test]
fn a_moved_folder_stays_ignored_when_the_old_one_was() {
    for (rule, config_ignored) in [(".pane/\n", true), (".pane/sessions/\n", false)] {
        let scratch = Scratch::new(if config_ignored {
            "ignored-all"
        } else {
            "ignored-sessions"
        });
        let project = scratch.project();
        assert!(git(&project, &["init", "-q"]));
        write(&project.join(".gitignore"), rule);
        write(
            &project.join(".pane/config.toml"),
            "[model]\nparent = \"m\"\n",
        );
        write(&project.join(".pane/sessions/s1.jsonl"), "{}\n");

        let (_, stderr) = start(&scratch);

        assert!(
            stderr.contains("Your git ignore rules named") && stderr.contains(".gitignore"),
            "{rule:?}: {stderr}"
        );
        assert!(
            git(
                &project,
                &["check-ignore", "-q", ".sterna/sessions/s1.jsonl"]
            ),
            "{rule:?}: the moved sessions are still ignored"
        );
        assert_eq!(
            git(&project, &["check-ignore", "-q", ".sterna/config.toml"]),
            config_ignored,
            "{rule:?}: the settings are ignored exactly when they were before"
        );
    }
}
