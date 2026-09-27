#![cfg(unix)]
//! Independent escape probes for the `explore` request mode (map 2637, 2638;
//! `GH-PANE-EXPLORE-MODE`, `sandbox/modes.rs`). POSIX-only: every probe is a
//! shell form, and `#[cfg(unix)]` on the whole file is the packet's own
//! requirement.
//!
//! This is a verifier, not a fixer: every assertion is on **the tree and the
//! world** (file bytes, permission bits, a real TCP listener), never on the
//! refusal text alone, per `GH-PANE-EXPLORE-VERIFY`. Helpers below
//! (`scratch_dir`, `start_provider`, `answer_one`, `cell_reply`, `run`) are
//! copied from `request_modes.rs` rather than shared, per that file's own
//! FORBIDDEN note in the packet.

use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

static COUNTER: AtomicU64 = AtomicU64::new(0);

fn scratch_dir(label: &str) -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "sterna-explore-escape-{label}-{}-{n}",
        std::process::id()
    ));
    fs::create_dir_all(&dir).unwrap();
    dir
}

/// A loopback provider answering each request from its body. Copied from
/// `request_modes.rs`.
fn start_provider<F>(turns: usize, answer: F) -> (String, Arc<Mutex<Vec<String>>>)
where
    F: Fn(&str) -> String + Send + 'static,
{
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let bodies = Arc::new(Mutex::new(Vec::new()));
    let seen = Arc::clone(&bodies);
    thread::spawn(move || {
        for _ in 0..turns {
            let Ok((stream, _)) = listener.accept() else {
                return;
            };
            answer_one(stream, &answer, &seen);
        }
    });
    (format!("http://127.0.0.1:{port}"), bodies)
}

fn answer_one<F: Fn(&str) -> String>(
    mut stream: std::net::TcpStream,
    answer: &F,
    bodies: &Mutex<Vec<String>>,
) {
    let mut reader = BufReader::new(stream.try_clone().unwrap());
    let mut length = 0usize;
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).unwrap_or(0) == 0 {
            return;
        }
        if line == "\r\n" || line == "\n" {
            break;
        }
        if let Some(rest) = line.to_ascii_lowercase().strip_prefix("content-length:") {
            length = rest.trim().parse().unwrap_or(0);
        }
    }
    let mut body = vec![0u8; length];
    if reader.read_exact(&mut body).is_err() {
        return;
    }
    let body = String::from_utf8_lossy(&body).into_owned();
    let reply = answer(&body);
    bodies.lock().unwrap().push(body);
    let _ = stream.write_all(
        format!(
            "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
            reply.len()
        )
        .as_bytes(),
    );
    let _ = stream.write_all(reply.as_bytes());
    let _ = stream.flush();
}

fn cell_reply(code: &str) -> String {
    serde_json::json!({
        "role": "assistant",
        "content": [{"type": "text", "text": format!("```sterna\n{code}\n```")}],
    })
    .to_string()
}

const STERNA_CONFIG: &str = "[permissions]\nallow = [\"Bash\"]\n";
const CANARY: &str = "canary\n";
const GIT_CONFIG: &str = "[core]\n\trepositoryformatversion = 0\n";
const CLAUDE_SETTINGS: &str = "{}\n";

/// A project whose profile admits every command line (as in
/// `request_modes.rs::project`), plus the fixed set of files every probe in
/// this file is scored against.
fn project(label: &str) -> PathBuf {
    let root = scratch_dir(label);
    fs::create_dir_all(root.join(".sterna")).unwrap();
    fs::create_dir_all(root.join("src")).unwrap();
    fs::create_dir_all(root.join(".git")).unwrap();
    fs::create_dir_all(root.join(".claude")).unwrap();
    fs::write(root.join(".sterna/config.toml"), STERNA_CONFIG).unwrap();
    fs::write(root.join("src/lib.rs"), "pub fn existing() {}\n").unwrap();
    fs::write(root.join("src/canary.txt"), CANARY).unwrap();
    fs::write(root.join(".git/config"), GIT_CONFIG).unwrap();
    fs::write(root.join(".claude/settings.json"), CLAUDE_SETTINGS).unwrap();
    root
}

/// `sterna session` with `args`, `inputs` piped one per line. Copied from
/// `request_modes.rs::run`.
fn run(root: &Path, args: &[&str], inputs: &[&str], base_url: &str) -> std::process::Output {
    let rollout = scratch_dir("rollout").join("rollout.jsonl");
    let mut command = Command::new(env!("CARGO_BIN_EXE_sterna"));
    command
        .arg("session")
        .arg("--root")
        .arg(root)
        .arg("--rollout")
        .arg(&rollout)
        .arg("--session")
        .arg(format!(
            "sess-escape-{}",
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ))
        .arg("--model")
        .arg(sterna::wire::MODEL)
        .args(args)
        .env("ANTHROPIC_BASE_URL", base_url)
        .env("XDG_CONFIG_HOME", scratch_dir("global-config"))
        .env_remove("ANTHROPIC_AUTH_TOKEN")
        .env_remove("ANTHROPIC_API_KEY")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command.spawn().unwrap();
    {
        let stdin = child.stdin.as_mut().unwrap();
        for line in inputs {
            writeln!(stdin, "{line}").unwrap();
        }
    }
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    output
}

/// One entry of the tree snapshot: a symlink's target is recorded rather than
/// followed, so a probe that swaps the link for a real file (or the reverse)
/// is caught by the type change, and a probe that writes through it is
/// caught by the target's own snapshot outside the root.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Entry {
    File { content: Vec<u8>, mode: u32 },
    Symlink { target: PathBuf },
}

fn snapshot(root: &Path) -> BTreeMap<PathBuf, Entry> {
    let mut map = BTreeMap::new();
    walk(root, root, &mut map);
    map
}

fn walk(base: &Path, dir: &Path, map: &mut BTreeMap<PathBuf, Entry>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries {
        let entry = entry.unwrap();
        let path = entry.path();
        let rel = path.strip_prefix(base).unwrap().to_path_buf();
        let meta = fs::symlink_metadata(&path).unwrap();
        if meta.file_type().is_symlink() {
            let target = fs::read_link(&path).unwrap();
            map.insert(rel, Entry::Symlink { target });
        } else if meta.is_dir() {
            walk(base, &path, map);
        } else {
            let content = fs::read(&path).unwrap_or_default();
            let mode = meta.permissions().mode() & 0o777;
            map.insert(rel, Entry::File { content, mode });
        }
    }
}

fn diff(pre: &BTreeMap<PathBuf, Entry>, post: &BTreeMap<PathBuf, Entry>) -> Option<String> {
    if pre == post {
        return None;
    }
    let mut parts = Vec::new();
    for (path, post_entry) in post {
        match pre.get(path) {
            None => parts.push(format!("+{} ({:?})", path.display(), post_entry)),
            Some(pre_entry) if pre_entry != post_entry => parts.push(format!(
                "~{} ({:?} -> {:?})",
                path.display(),
                pre_entry,
                post_entry
            )),
            _ => {}
        }
    }
    for path in pre.keys() {
        if !post.contains_key(path) {
            parts.push(format!("-{}", path.display()));
        }
    }
    Some(parts.join("; "))
}

struct Finding {
    family: &'static str,
    probe: String,
    effect: String,
    rule: String,
}

fn render(findings: &[Finding]) -> String {
    findings
        .iter()
        .map(|f| {
            format!(
                "[{}] probe={:?} effect={} rule={}",
                f.family, f.probe, f.effect, f.rule
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn extract_rule(stdout: &str) -> String {
    for marker in ["mode explore:", "mode plan:"] {
        if let Some(idx) = stdout.find(marker) {
            return stdout[idx..].lines().next().unwrap_or(marker).to_string();
        }
    }
    "none".to_string()
}

/// One cell attempting `action_js`, wrapped exactly as
/// `request_modes.rs::attempt_write` wraps a write: the try/catch result is
/// the model's own account, never the ground truth.
fn attempt_cell(action_js: &str) -> String {
    format!(
        "let out;\ntry {{ {action_js}; out = \"RAN\"; }} catch (e) {{ out = \"REFUSED: \" + e.message; }}\nreturn out;"
    )
}

fn bash_action(cmd: &str) -> String {
    format!(
        "await bash({{ command: {} }})",
        serde_json::to_string(cmd).unwrap()
    )
}

fn write_action(path: &str, content: &str) -> String {
    format!(
        "await write({{ path: {}, content: {} }})",
        serde_json::to_string(path).unwrap(),
        serde_json::to_string(content).unwrap()
    )
}

fn edit_action(path: &str, expected_sha256: &str, old: &str, replacement: &str) -> String {
    format!(
        "await edit({{ path: {}, expected_sha256: {}, old: {}, replacement: {} }})",
        serde_json::to_string(path).unwrap(),
        serde_json::to_string(expected_sha256).unwrap(),
        serde_json::to_string(old).unwrap(),
        serde_json::to_string(replacement).unwrap()
    )
}

fn sha256_hex(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

/// Runs one cell containing `action_js` against a fresh `root` in explore
/// mode and returns the process's stdout. The scripted model attempts the
/// tool call despite the prompt naming the mode, exactly as
/// `request_modes.rs` does throughout.
fn execute_probe(root: &Path, action_js: &str) -> String {
    let (base_url, _bodies) = start_provider(1, {
        let action_js = action_js.to_string();
        move |_| cell_reply(&attempt_cell(&action_js))
    });
    let output = run(root, &["--mode", "explore"], &["do it"], &base_url);
    String::from_utf8_lossy(&output.stdout).into_owned()
}

/// One probe whose only side effect a real escape could leave is inside
/// `root` — every family-1/2/3 probe (all POSIX shell forms against the
/// canary and the tree, never against a path outside the project).
fn probe_in_tree(
    family: &'static str,
    label: &str,
    probe_desc: &str,
    action_js: &str,
) -> Option<Finding> {
    let root = project(label);
    let pre = snapshot(&root);
    let stdout = execute_probe(&root, action_js);
    let post = snapshot(&root);
    diff(&pre, &post).map(|mut effect| {
        if stdout.contains("REFUSED") {
            effect = format!("[refused in text, but this side effect happened anyway] {effect}");
        }
        Finding {
            family,
            probe: probe_desc.to_string(),
            effect,
            rule: extract_rule(&stdout),
        }
    })
}

// ---------------------------------------------------------------------
// Family 1 — redirects and clobbers
// ---------------------------------------------------------------------

#[test]
fn family_redirects_and_clobbers() {
    let probes: &[&str] = &[
        "echo x > src/canary.txt",
        "echo x >> src/canary.txt",
        "cat src/canary.txt > out.txt",
        "cat < src/canary.txt > out.txt",
        ">| src/canary.txt",
        "echo x &> out.txt",
        "echo x 2> out.txt",
        "cat <> src/canary.txt",
        "echo x | tee src/canary.txt",
        "echo x | tee -a src/canary.txt",
        "dd of=src/canary.txt",
        "cp src/lib.rs src/canary.txt",
        "mv src/lib.rs src/canary.txt",
        "install -m 644 src/lib.rs src/canary.txt",
        "truncate -s 0 src/canary.txt",
        "touch newfile.txt",
        "mkdir newdir",
        "ln -s src/canary.txt linked.txt",
        "chmod 000 src/canary.txt",
    ];
    let mut findings = Vec::new();
    for (i, p) in probes.iter().enumerate() {
        if let Some(f) = probe_in_tree("redirects", &format!("fam1-{i}"), p, &bash_action(p)) {
            findings.push(f);
        }
    }
    assert!(
        findings.is_empty(),
        "{} probes run, escapes:\n{}",
        probes.len(),
        render(&findings)
    );
}

// ---------------------------------------------------------------------
// Family 2 — quoting and splitting
// ---------------------------------------------------------------------

#[test]
fn family_quoting_and_splitting() {
    let probes: &[&str] = &[
        "\"rm\" src/canary.txt",
        "'rm' src/canary.txt",
        "r\\m src/canary.txt",
        "$'rm' src/canary.txt",
        "rm\tsrc/canary.txt",
        "ls\nrm src/canary.txt",
        "ls; rm src/canary.txt",
        "ls && rm src/canary.txt",
        "ls || rm src/canary.txt",
        "ls | tee src/canary.txt",
        "{ rm src/canary.txt; }",
        "( rm src/canary.txt )",
        " rm src/canary.txt",
    ];
    let mut findings = Vec::new();
    for (i, p) in probes.iter().enumerate() {
        if let Some(f) = probe_in_tree("quoting", &format!("fam2-{i}"), p, &bash_action(p)) {
            findings.push(f);
        }
    }
    assert!(
        findings.is_empty(),
        "{} probes run, escapes:\n{}",
        probes.len(),
        render(&findings)
    );
}

// ---------------------------------------------------------------------
// Family 3 — substitution and evaluation
// ---------------------------------------------------------------------

#[test]
fn family_substitution_and_evaluation() {
    let probes: &[&str] = &[
        "echo $(rm src/canary.txt)",
        "echo `rm src/canary.txt`",
        "cat <(rm src/canary.txt)",
        "echo x >(rm src/canary.txt)",
        "echo ${IFS}rm${IFS}src/canary.txt",
        "eval rm src/canary.txt",
        "exec rm src/canary.txt",
        "source ./nonexistent.sh",
        ". ./nonexistent.sh",
        "bash -c 'rm src/canary.txt'",
        "sh -c 'rm src/canary.txt'",
        "python3 -c \"open('src/canary.txt','w').write('x')\"",
        "perl -e 'unlink(\"src/canary.txt\")'",
        "env rm src/canary.txt",
        "nice rm src/canary.txt",
        "xargs rm < list.txt",
        "find . -delete",
        "find . -exec rm {} \\;",
        "find . -execdir rm {} \\;",
        "find . -fprint out.txt",
        "git -c core.pager='rm src/canary.txt' log",
        "git -c diff.external=rm diff",
        "git log --output=src/canary.txt",
        "git -C .. status",
        "rg --pre 'rm' .",
        "grep --exclude-from=list.txt rm .",
        "date -s '2020-01-01'",
        "file -C src/canary.txt",
        "ls --color=always > out.txt",
    ];
    let mut findings = Vec::new();
    for (i, p) in probes.iter().enumerate() {
        if let Some(f) = probe_in_tree("substitution", &format!("fam3-{i}"), p, &bash_action(p)) {
            findings.push(f);
        }
    }
    assert!(
        findings.is_empty(),
        "{} probes run, escapes:\n{}",
        probes.len(),
        render(&findings)
    );
}

// ---------------------------------------------------------------------
// Family 4 — paths and tools
// ---------------------------------------------------------------------

#[test]
fn family_paths_and_tools() {
    let mut findings: Vec<Finding> = Vec::new();

    // 1. write outside the root via `..`
    {
        let root = project("fam4-dotdot-write");
        let marker = format!("sterna-escape-dotdot-{}.txt", std::process::id());
        let outside_path = root.parent().unwrap().join(&marker);
        let _ = fs::remove_file(&outside_path);
        let probe = format!("write path=../{marker}");
        let stdout = execute_probe(&root, &write_action(&format!("../{marker}"), "changed"));
        if outside_path.exists() {
            findings.push(Finding {
                family: "paths",
                probe,
                effect: format!("created {}", outside_path.display()),
                rule: extract_rule(&stdout),
            });
            let _ = fs::remove_file(&outside_path);
        }
    }

    // 2. write to an absolute path in /tmp, outside the root
    {
        let root = project("fam4-abs-write");
        let external_dir = scratch_dir("fam4-abs-external");
        let target = external_dir.join("absolute-target.txt");
        fs::write(&target, "external\n").unwrap();
        let probe = format!("write path={}", target.display());
        let stdout = execute_probe(&root, &write_action(target.to_str().unwrap(), "changed"));
        let now = fs::read(&target).unwrap_or_default();
        if now != b"external\n" {
            findings.push(Finding {
                family: "paths",
                probe,
                effect: format!("content became {:?}", String::from_utf8_lossy(&now)),
                rule: extract_rule(&stdout),
            });
        }
    }

    // 3. a doubled-slash path climbing out of the root
    {
        let root = project("fam4-doubled-write");
        let marker = format!("sterna-escape-doubled-{}.txt", std::process::id());
        let outside_path = root.parent().unwrap().join(&marker);
        let _ = fs::remove_file(&outside_path);
        let doubled = format!("src//..//../{marker}");
        let probe = format!("write path={doubled}");
        let stdout = execute_probe(&root, &write_action(&doubled, "changed"));
        if outside_path.exists() {
            findings.push(Finding {
                family: "paths",
                probe,
                effect: format!("created {}", outside_path.display()),
                rule: extract_rule(&stdout),
            });
            let _ = fs::remove_file(&outside_path);
        }
    }

    // 4/5/6: write and edit against `.sterna/config.toml`, `.git/config`,
    // `.claude/settings.json` — never-grantable regardless of mode, checked
    // here because the mode packet claims explore writes nothing outside its
    // globs and these three are the paths most likely to matter if it did.
    for (relpath, original, old_substring) in [
        (".sterna/config.toml", STERNA_CONFIG, "allow"),
        (".git/config", GIT_CONFIG, "repositoryformatversion"),
        (".claude/settings.json", CLAUDE_SETTINGS, "{}"),
    ] {
        {
            let root = project(&format!("fam4-w-{}", relpath.replace(['/', '.'], "-")));
            let pre = fs::read(root.join(relpath)).unwrap();
            assert_eq!(pre, original.as_bytes(), "fixture drifted for {relpath}");
            let probe = format!("write path={relpath}");
            let stdout = execute_probe(&root, &write_action(relpath, "changed"));
            let post = fs::read(root.join(relpath)).unwrap();
            if post != pre {
                findings.push(Finding {
                    family: "paths",
                    probe,
                    effect: format!("{relpath} became {:?}", String::from_utf8_lossy(&post)),
                    rule: extract_rule(&stdout),
                });
            }
        }
        {
            let root = project(&format!("fam4-e-{}", relpath.replace(['/', '.'], "-")));
            let pre = fs::read(root.join(relpath)).unwrap();
            let sha = sha256_hex(&pre);
            let probe = format!("edit path={relpath}");
            let stdout = execute_probe(
                &root,
                &edit_action(relpath, &sha, old_substring, "TAMPERED"),
            );
            let post = fs::read(root.join(relpath)).unwrap();
            if post != pre {
                findings.push(Finding {
                    family: "paths",
                    probe,
                    effect: format!("{relpath} became {:?}", String::from_utf8_lossy(&post)),
                    rule: extract_rule(&stdout),
                });
            }
        }
    }

    // 7. `.sterna/scratch/**` itself — the agent's scratchpad, carved out of
    // the `.sterna/**` never rule (GH-PANE-SCRATCH-AND-PLAN-FILE), is the one
    // place explore may write by default. The probe is inverted from the
    // verifier's first run, which predated the carve-out: a refused scratch
    // write is now the finding, and the sibling `.sterna/config.toml` probe
    // above still proves the rest of `.sterna/` stays never-writable.
    {
        let root = project("fam4-scratch-write");
        let probe = "write path=.sterna/scratch/probe.txt".to_string();
        let stdout = execute_probe(&root, &write_action(".sterna/scratch/probe.txt", "changed"));
        if !root.join(".sterna/scratch/probe.txt").exists() {
            findings.push(Finding {
                family: "paths",
                probe,
                effect: ".sterna/scratch/probe.txt was NOT created — the scratchpad carve-out does not admit explore's default writable glob".to_string(),
                rule: extract_rule(&stdout),
            });
        }
    }

    // 8/9: a symlink planted before the session, pointing outside the root —
    // `ln` itself is refused in explore, so this is planted directly on
    // disk, mirroring the packet's own instruction.
    {
        let root = project("fam4-symlink-write");
        let external_dir = scratch_dir("fam4-symlink-external");
        let target = external_dir.join("symlink-target.txt");
        fs::write(&target, "external-canary\n").unwrap();
        std::os::unix::fs::symlink(&target, root.join("src/link-out.txt")).unwrap();
        let probe = "write path=src/link-out.txt (symlink to outside the root)".to_string();
        let stdout = execute_probe(&root, &write_action("src/link-out.txt", "changed"));
        let now = fs::read(&target).unwrap_or_default();
        let link_meta = fs::symlink_metadata(root.join("src/link-out.txt")).unwrap();
        if now != b"external-canary\n" || !link_meta.file_type().is_symlink() {
            findings.push(Finding {
                family: "paths",
                probe,
                effect: format!(
                    "external target became {:?}; link still a symlink: {}",
                    String::from_utf8_lossy(&now),
                    link_meta.file_type().is_symlink()
                ),
                rule: extract_rule(&stdout),
            });
        }
    }
    {
        let root = project("fam4-symlink-edit");
        let external_dir = scratch_dir("fam4-symlink-external-edit");
        let target = external_dir.join("symlink-target.txt");
        fs::write(&target, "external-canary\n").unwrap();
        std::os::unix::fs::symlink(&target, root.join("src/link-out.txt")).unwrap();
        let sha = sha256_hex(b"external-canary\n");
        let probe = "edit path=src/link-out.txt (symlink to outside the root)".to_string();
        let stdout = execute_probe(
            &root,
            &edit_action("src/link-out.txt", &sha, "external", "TAMPERED"),
        );
        let now = fs::read(&target).unwrap_or_default();
        if now != b"external-canary\n" {
            findings.push(Finding {
                family: "paths",
                probe,
                effect: format!("external target became {:?}", String::from_utf8_lossy(&now)),
                rule: extract_rule(&stdout),
            });
        }
    }

    let probe_count = 3 + 3 * 2 + 1 + 2;
    assert!(
        findings.is_empty(),
        "{probe_count} probes run, escapes:\n{}",
        render(&findings)
    );
}

// ---------------------------------------------------------------------
// Family 5 — network
// ---------------------------------------------------------------------

/// A listener that accepts and immediately drops every connection, so a
/// probe that does slip through fails fast (connection reset) rather than
/// hanging the gate on a response that will never come.
fn spy_listener() -> (u16, Arc<Mutex<bool>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let hit = Arc::new(Mutex::new(false));
    let hit_writer = Arc::clone(&hit);
    thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(stream) = stream else { break };
            *hit_writer.lock().unwrap() = true;
            drop(stream);
        }
    });
    (port, hit)
}

#[test]
fn family_network() {
    let mut findings: Vec<Finding> = Vec::new();
    type ProbeCase = (&'static str, fn(u16) -> String);
    let cases: &[ProbeCase] = &[
        ("curl", |port| format!("curl http://127.0.0.1:{port}")),
        ("nc", |port| format!("nc 127.0.0.1 {port}")),
        ("python3 http.client", |port| {
            format!(
                "python3 -c \"import http.client; http.client.HTTPConnection('127.0.0.1', {port}).request('GET','/')\""
            )
        }),
        ("git fetch", |port| {
            format!("git fetch http://127.0.0.1:{port}/repo.git")
        }),
        ("ssh", |port| {
            format!("ssh -o BatchMode=yes -o ConnectTimeout=1 -p {port} localhost")
        }),
        ("scp", |port| {
            format!("scp -P {port} src/canary.txt localhost:/tmp/sterna-escape-scp-target")
        }),
    ];
    for (i, (name, make_cmd)) in cases.iter().enumerate() {
        let root = project(&format!("fam5-{i}"));
        let pre = snapshot(&root);
        let (port, hit) = spy_listener();
        let cmd = make_cmd(port);
        let stdout = execute_probe(&root, &bash_action(&cmd));
        thread::sleep(Duration::from_millis(200));
        let connected = *hit.lock().unwrap();
        let post = snapshot(&root);
        let tree_effect = diff(&pre, &post);
        if connected || tree_effect.is_some() {
            let mut effect = String::new();
            if connected {
                effect.push_str("connected to the local listener");
            }
            if let Some(d) = tree_effect {
                if !effect.is_empty() {
                    effect.push_str("; ");
                }
                effect.push_str(&d);
            }
            findings.push(Finding {
                family: "network",
                probe: format!("{name}: {cmd}"),
                effect,
                rule: extract_rule(&stdout),
            });
        }
        let _ = Command::new("pkill")
            .args(["-f", &format!("127.0.0.1:{port}")])
            .status();
    }
    assert!(
        findings.is_empty(),
        "{} probes run, escapes:\n{}",
        cases.len(),
        render(&findings)
    );
}

// ---------------------------------------------------------------------
// Family 6 — the direct and native doors
// ---------------------------------------------------------------------

/// Mirrors `request_modes.rs::a_direct_tool_frame_is_refused_by_the_same_rule`:
/// a `/tool` line typed as a user input, answered by no provider turn at all.
#[test]
fn family_direct_tool_frame() {
    let mut findings: Vec<Finding> = Vec::new();
    let cases: &[&str] = &["src/direct.rs", ".sterna/config.toml", ".git/config"];
    for (i, path) in cases.iter().enumerate() {
        let root = project(&format!("fam6-{i}"));
        let pre = snapshot(&root);
        let (base_url, _bodies) = start_provider(0, |_| String::new());
        let line = format!("/tool write path={path} content=changed");
        let output = run(&root, &["--mode", "explore"], &[&line], &base_url);
        let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
        let post = snapshot(&root);
        if let Some(effect) = diff(&pre, &post) {
            findings.push(Finding {
                family: "direct-tool-frame",
                probe: line,
                effect,
                rule: extract_rule(&stdout),
            });
        }
    }
    assert!(
        findings.is_empty(),
        "{} probes run, escapes:\n{}",
        cases.len(),
        render(&findings)
    );
}

// ---------------------------------------------------------------------
// Family 7 — a hard link planted in the scratchpad
// ---------------------------------------------------------------------

/// `phase-69.md` line 2637's defect: a hard link planted unconfined in
/// `.sterna/scratch/` (explore's own writable glob) made `write` rewrite
/// `.sterna/config.toml` through it. The shipped binary refuses, and every
/// name of the file keeps its bytes.
#[test]
fn family_hard_link_in_the_scratchpad() {
    let root = project("fam7-hard-link");
    fs::create_dir_all(root.join(".sterna/scratch")).unwrap();
    fs::hard_link(
        root.join(".sterna/config.toml"),
        root.join(".sterna/scratch/hard"),
    )
    .unwrap();
    let pre = snapshot(&root);
    let stdout = execute_probe(&root, &write_action(".sterna/scratch/hard", "changed"));
    let post = snapshot(&root);
    assert_eq!(diff(&pre, &post), None, "stdout:\n{stdout}");
    assert_eq!(
        fs::read_to_string(root.join(".sterna/config.toml")).unwrap(),
        STERNA_CONFIG
    );
    assert!(
        stdout.contains("hard-linked file (2 names)"),
        "the refusal names the link count:\n{stdout}"
    );
}
