//! Git remains a standard shell workflow; exercise its useful outcomes in the
//! same sandbox the agent uses instead of introducing a duplicate Git API.
#[cfg(unix)]
#[test]
fn admitted_git_can_inspect_changes_commit_and_read_history() {
    use sterna::contract::SessionId;
    use sterna::sandbox::profile::Profile;
    use sterna::tools::invoke::{self, Args, ToolContext};
    let root = std::env::temp_dir().join(format!(
        "sterna-git-workflow-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&root).unwrap();
    let root = root.canonicalize().unwrap();
    let profile = Profile::compile(&root, Some(r#"{"permissions":{"allow":["Bash"]}}"#));
    let session = SessionId::new("git-workflow");
    let context = ToolContext {
        profile: &profile,
        session: &session,
    };
    let run = |command: &str| {
        let result = invoke::run(&context, "bash", &Args::new().with("command", command)).unwrap();
        assert_eq!(
            result.exit_code,
            Some(0),
            "{command}: {} {}",
            result.stdout,
            result.stderr
        );
        result.stdout
    };
    run("test \"$GIT_CONFIG_GLOBAL\" = /dev/null && test \"$GIT_CONFIG_SYSTEM\" = /dev/null");
    run("git -c init.templateDir= init -q");
    std::fs::write(root.join("example.txt"), "first\n").unwrap();
    assert!(run("git status --porcelain").contains("example.txt"));
    run("git add example.txt");
    assert!(run("git diff --cached -- example.txt").contains("+first"));
    run(
        "git -c user.name=Sterna -c user.email=sterna@example.invalid -c commit.gpgsign=false -c core.hooksPath=/dev/null commit -qm baseline",
    );
    assert_eq!(run("git log -1 --format=%s").trim(), "baseline");
    assert!(run("git status --porcelain").trim().is_empty());
    std::fs::write(root.join("example.txt"), "second\n").unwrap();
    let diff = run("git diff -- example.txt");
    assert!(diff.contains("-first") && diff.contains("+second"));
    std::fs::remove_dir_all(root).unwrap();
}
