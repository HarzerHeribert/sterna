//! `.sterna/learned.md`: what Sterna learned about a project by working in it,
//! written by Sterna behind the answer and read into the next task's prompt
//! (2026-09-23, the user's option A: *Sterna writes, Glasshouse only reads*).
//!
//! **The invariant: a line reaches the file only when it names a path that
//! exists in the project and is not already in the file, and the file never
//! holds more than [`MAX_LINES`] lines.** The writer is a cheap model asked
//! once, toolless; everything it says is checked here and nothing it says is
//! trusted. What a Scout would find again next session is the saving: the
//! same search, paid once.

use std::collections::BTreeMap;
use std::path::Path;

use crate::sandbox::profile::{Access, Profile};

pub const PATH: &str = ".sterna/learned.md";
/// The file's bound: the oldest lines leave first.
pub const MAX_LINES: usize = 200;
/// The most lines one task adds.
pub const MAX_NEW: usize = 3;
/// A task that opened fewer distinct files than this learned nothing worth
/// a request: it went straight to what it needed.
pub const MIN_OPENED: usize = 3;
const MAX_LINE_CHARS: usize = 240;

const HEADER: &str = "# Learned by Sterna\n\nWritten by Sterna after tasks in this project; edit or delete lines freely.\n\n";

/// The writer: one toolless answer over what a finished task had to open.
pub const WRITER: crate::helpers::HelperSpec = crate::helpers::HelperSpec {
    name: "learn",
    summary: "Note what a finished task had to search for, so the next one need not.",
    verb: "noting",
    preamble: "You keep a project's learned notes: short facts a coding agent would otherwise \
        rediscover by searching. You are shown a finished task's request, the files it had to \
        open (with how many times), the files it changed, and the notes and instruction \
        headings that already exist. Answer with at most three lines, each exactly \
        `- <fact> (<path>)`, where <path> is one of the files shown: where something lives, \
        which file owns a behaviour, or a command or convention the task had to discover. Only \
        what stays true beyond this task, and nothing the existing notes or instructions \
        already say. No task narrative, no opinions. If there is nothing durable, answer \
        exactly NONE.",
    tools: &[],
    max_tokens: 400,
    max_turns: 1,
    input: crate::helpers::InputKind::Text,
    output: crate::helpers::OutputKind::Checklist,
    call_sites: &[],
};

/// The file's current text, read through the profile, or empty.
#[must_use]
pub fn read(profile: &Profile) -> String {
    profile
        .check("read", Access::Read, Path::new(PATH))
        .ok()
        .and_then(|path| std::fs::read_to_string(path).ok())
        .unwrap_or_default()
}

/// The prompt section, or empty when nothing has been learned.
#[must_use]
pub fn section(profile: &Profile) -> String {
    let text = read(profile);
    let lines: Vec<&str> = text.lines().filter(|l| l.starts_with("- ")).collect();
    if lines.is_empty() {
        return String::new();
    }
    format!(
        "\n\n## Learned about this project\n\nNotes Sterna wrote after earlier tasks here ({PATH}). \
         Each names the file it was seen in: use them as leads and open the file when the detail \
         matters.\n{}\n",
        lines.join("\n")
    )
}

/// What the writer is asked, or `None` when the task opened too little to
/// have learned anything.
#[must_use]
pub fn ask(
    request: &str,
    opened: &BTreeMap<String, u32>,
    changed: &[String],
    existing: &str,
    instruction_headings: &[String],
) -> Option<String> {
    if opened.len() < MIN_OPENED {
        return None;
    }
    let mut ranked: Vec<(&String, &u32)> = opened.iter().collect();
    ranked.sort_by(|a, b| b.1.cmp(a.1).then(a.0.cmp(b.0)));
    let opened: Vec<String> = ranked
        .iter()
        .take(30)
        .map(|(path, times)| format!("- {path} ({times}x)"))
        .collect();
    let existing: Vec<&str> = existing.lines().filter(|l| l.starts_with("- ")).collect();
    Some(format!(
        "## Request\n{}\n\n## Files it opened\n{}\n\n## Files it changed\n{}\n\n## Existing notes\n{}\n\n## Instruction headings\n{}\n",
        crate::helper_context::bounded_string(request, 2000),
        opened.join("\n"),
        if changed.is_empty() {
            "(none)".to_string()
        } else {
            changed
                .iter()
                .map(|c| format!("- {c}"))
                .collect::<Vec<_>>()
                .join("\n")
        },
        if existing.is_empty() {
            "(none)".to_string()
        } else {
            existing.join("\n")
        },
        if instruction_headings.is_empty() {
            "(none)".to_string()
        } else {
            instruction_headings.join("\n")
        },
    ))
}

/// The writer's lines that pass: the fixed form, a path that exists in the
/// project, not already in the file. At most [`MAX_NEW`].
#[must_use]
pub fn accept(answer: &str, existing: &str, root: &Path) -> Vec<String> {
    let known: Vec<String> = existing.lines().map(normalized).collect();
    let mut kept: Vec<String> = Vec::new();
    for line in answer.lines().map(str::trim) {
        if kept.len() == MAX_NEW {
            break;
        }
        if !line.starts_with("- ") || line.chars().count() > MAX_LINE_CHARS {
            continue;
        }
        let Some(open) = line.rfind('(') else {
            continue;
        };
        let path = line[open + 1..].trim_end_matches(')').trim();
        let path = path.split(':').next().unwrap_or(path);
        if path.is_empty() || path.contains("..") || !root.join(path).exists() {
            continue;
        }
        let key = normalized(line);
        if known.contains(&key) || kept.iter().any(|k| normalized(k) == key) {
            continue;
        }
        kept.push(line.to_string());
    }
    kept
}

fn normalized(line: &str) -> String {
    line.to_lowercase()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// Appends `lines`, dropping the oldest past [`MAX_LINES`]. Sterna writes it
/// as the host: `.sterna/**` is host-owned and never writable by an agent's
/// tools, which is exactly why a model cannot edit its own notes' store.
pub fn append(profile: &Profile, lines: &[String]) -> Result<(), String> {
    if lines.is_empty() {
        return Ok(());
    }
    let path = profile.root().join(PATH);
    let existing = std::fs::read_to_string(&path).unwrap_or_default();
    let mut notes: Vec<String> = existing
        .lines()
        .filter(|l| l.starts_with("- "))
        .map(str::to_string)
        .collect();
    notes.extend(lines.iter().cloned());
    let drop = notes.len().saturating_sub(MAX_LINES);
    let text = format!("{HEADER}{}\n", notes[drop..].join("\n"));
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    let temporary = path.with_extension(format!("md.{}", std::process::id()));
    std::fs::write(&temporary, text).map_err(|e| e.to_string())?;
    std::fs::rename(&temporary, &path).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fixture directory removed when the test ends.
    struct Dir(std::path::PathBuf);
    impl Dir {
        fn path(&self) -> &Path {
            &self.0
        }
    }
    impl Drop for Dir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn only_well_formed_new_lines_naming_real_files_are_kept_and_the_file_is_bounded() {
        let root = std::env::temp_dir().join(format!("sterna-learned-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let dir = Dir(root);
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        std::fs::write(dir.path().join("src/gate.rs"), "fn gate() {}\n").unwrap();
        let answer = "- the completion gate lives in gate() (src/gate.rs)\n\
                      - invented (src/nowhere.rs)\n\
                      prose without a dash (src/gate.rs)\n\
                      - The completion gate lives in gate()  (src/gate.rs)\n";
        let kept = accept(answer, "", dir.path());
        assert_eq!(
            kept,
            vec!["- the completion gate lives in gate() (src/gate.rs)"]
        );
        assert!(
            accept(answer, &kept[0], dir.path()).is_empty(),
            "a known line is not added twice"
        );

        let profile = Profile::compile(dir.path(), None);
        let many: Vec<String> = (0..MAX_LINES + 5)
            .map(|n| format!("- fact {n} (src/gate.rs)"))
            .collect();
        append(&profile, &many).unwrap();
        let text = std::fs::read_to_string(dir.path().join(PATH)).unwrap();
        let notes: Vec<&str> = text.lines().filter(|l| l.starts_with("- ")).collect();
        assert_eq!(notes.len(), MAX_LINES);
        assert_eq!(notes[0], "- fact 5 (src/gate.rs)", "the oldest leave first");
        assert!(section(&profile).contains("## Learned about this project"));
    }

    #[test]
    fn a_task_that_opened_little_asks_nothing() {
        let opened: BTreeMap<String, u32> = [("a.rs".to_string(), 3)].into();
        assert!(ask("fix it", &opened, &[], "", &[]).is_none());
    }
}
