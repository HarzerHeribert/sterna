//! Which session a run is, and where its turns go.
//!
//! **A session became addressable here.** `rollout.rs` always said "one file
//! per session"; the path it was given was `<root>/.sterna/rollout.jsonl`, one
//! per *folder*, so every run in a directory appended to one conversation and
//! there was nothing to name, list or go back to. This module gives a session
//! an id a person can read off an exit line and type back in, and resolves
//! that id to the file `rollout.rs` already knew how to resume from.
//!
//! **A folder is the whole of the scoping, and that is deliberate.** The root
//! is the directory `sterna` was spawned in -- `project::load` joins names onto
//! it and walks up from it for nothing -- so sessions live in `<that
//! folder>/.sterna/sessions/` and belong to it. There is no project registry,
//! no id derived from a remote, and nothing to keep in sync: cd somewhere
//! else and you are in that folder's sessions instead.
//!
//! It decides no policy about what a session *is* — `session.rs` owns that —
//! and it never reads or writes a rollout's contents.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use super::SessionArgs;
use crate::contract::SessionId;

/// What `--sessions` prints: every resumable session, newest first, with the
/// line that resumes it.
///
/// Separate from the printing so a test drives the text rather than a
/// terminal, and so an empty project says what to do instead of nothing.
pub(super) fn print_listing(root: &Path) -> Result<(), String> {
    print!("{}", session_listing(root));
    Ok(())
}

fn session_listing(root: &Path) -> String {
    let found = resumable(root);
    if found.is_empty() {
        return "no resumable sessions in this folder yet\n".to_string();
    }
    let mut out = String::new();
    for (index, (id, path, modified)) in found.iter().enumerate() {
        let age = SystemTime::now()
            .duration_since(*modified)
            .map_or_else(|_| "just now".to_string(), |since| ago(since.as_secs()));
        let turns = fs::read_to_string(path).map_or(0, |text| text.lines().count());
        out.push_str(&format!(
            "{} {id:<14} {age:>12}  {turns} line{}\n",
            if index == 0 { "*" } else { " " },
            if turns == 1 { "" } else { "s" }
        ));
    }
    out.push_str("\nresume with:  sterna --resume <id>   (or bare --resume for the newest)\n");
    out
}

/// Coarse on purpose: the question a listing answers is which of these is the
/// one you were just in, and a timestamp makes that arithmetic.
fn ago(seconds: u64) -> String {
    match seconds {
        0..=90 => "just now".to_string(),
        s if s < 3600 => format!("{} min ago", s / 60),
        s if s < 86_400 => format!("{} hr ago", s / 3600),
        s => format!("{} days ago", s / 86_400),
    }
}

/// A short, sortable, typeable id.
///
/// **`sterna-<pid>` was none of those.** A pid repeats after a reboot, so two
/// sessions could claim one rollout file; it sorts meaninglessly, so "the
/// most recent" could not be answered from the names; and it carries no time,
/// so a list of them told you nothing about which was which. This is the
/// starting second and the pid, both base 36 — 11 characters, unique without
/// coordination, and ordered by when the session began.
fn default_session_id() -> String {
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_secs());
    format!(
        "{}-{}",
        base36(seconds),
        base36(u64::from(std::process::id()))
    )
}

fn base36(mut value: u64) -> String {
    const DIGITS: &[u8] = b"0123456789abcdefghijklmnopqrstuvwxyz";
    if value == 0 {
        return "0".to_string();
    }
    let mut out = Vec::new();
    while value > 0 {
        out.push(DIGITS[(value % 36) as usize]);
        value /= 36;
    }
    out.reverse();
    String::from_utf8(out).expect("base 36 digits are ASCII")
}

fn sessions_dir(root: &Path) -> PathBuf {
    root.join(".sterna").join("sessions")
}

fn rollout_path_for(root: &Path, id: &str) -> PathBuf {
    sessions_dir(root).join(format!("{id}.jsonl"))
}

/// The legacy single-file rollout, kept resumable under a name.
///
/// Every session before per-session files appended to `.sterna/rollout.jsonl`.
/// It is one conversation and it is somebody's history, so it is listed and
/// resumable as `rollout` rather than orphaned by the change.
///
/// It is also the only id a person did not get from an exit line, which is
/// why it reads as a word rather than as a generated one.
const LEGACY_ID: &str = "rollout";

/// This folder's resumable sessions, newest first.
///
/// Ordered by the rollout file's own modification time rather than by the id,
/// because a resumed session keeps its id and its *last* turn is what makes
/// it the most recent thing to go back to.
fn resumable(root: &Path) -> Vec<(String, PathBuf, SystemTime)> {
    let mut found: Vec<(String, PathBuf, SystemTime)> = fs::read_dir(sessions_dir(root))
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|entry| {
            let path = entry.path();
            let id = path.file_stem()?.to_str()?.to_string();
            (path.extension()? == "jsonl").then_some(())?;
            // `<id>.events.jsonl` sits beside every session and is not one:
            // counted, it was the newest file and `--resume` resumed it.
            (!id.contains('.')).then_some(())?;
            let modified = entry.metadata().ok()?.modified().ok()?;
            Some((id, path, modified))
        })
        .collect();
    let legacy = root.join(".sterna").join("rollout.jsonl");
    if let Ok(modified) = fs::metadata(&legacy).and_then(|meta| meta.modified()) {
        found.push((LEGACY_ID.to_string(), legacy, modified));
    }
    found.sort_by(|a, b| b.2.cmp(&a.2).then_with(|| b.0.cmp(&a.0)));
    found
}

/// The one line a person needs to get back in.
pub(super) fn resume_hint(id: &SessionId) -> String {
    format!("session {id} — resume it with:  sterna --resume {id}")
}

/// One row of the picker: a session, and what a person recognises it by.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Pickable {
    id: String,
    age: String,
    /// The first thing the person asked in it.
    title: String,
    prompts: usize,
}

/// This folder's sessions as the picker lists them, newest first. A person's
/// own prompts are the turns that carry `blocks`; the synthetic ones Sterna
/// writes (a cell's result, a stop) do not.
fn pickable(root: &Path) -> Vec<Pickable> {
    resumable(root)
        .into_iter()
        .map(|(id, path, modified)| {
            let asked: Vec<String> = fs::read_to_string(&path)
                .unwrap_or_default()
                .lines()
                .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
                .filter(|entry| {
                    entry["kind"] == "turn"
                        && entry["role"] == "user"
                        && entry.get("blocks").is_some()
                })
                .filter_map(|entry| entry["text"].as_str().map(str::to_string))
                .collect();
            let age = SystemTime::now()
                .duration_since(modified)
                .map_or_else(|_| "just now".to_string(), |since| ago(since.as_secs()));
            Pickable {
                id,
                age,
                title: asked
                    .first()
                    .map(|text| text.split_whitespace().collect::<Vec<_>>().join(" "))
                    .unwrap_or_else(|| "(nothing asked yet)".into()),
                prompts: asked.len(),
            }
        })
        .collect()
}

/// A bare `--resume` at a terminal asks which session; anywhere else it is
/// the newest. `false` is a person who closed the picker without choosing.
pub(super) fn choose(args: &mut SessionArgs) -> Result<bool, String> {
    use std::io::IsTerminal;
    if args.resume.as_deref() != Some("")
        || args.task.is_some()
        || !std::io::stdin().is_terminal()
        || !std::io::stdout().is_terminal()
    {
        return Ok(true);
    }
    match pick(&args.root)? {
        Some(id) => {
            args.resume = Some(id);
            Ok(true)
        }
        None => Ok(false),
    }
}

/// `sterna --resume` in a terminal: this folder's sessions to choose from, the
/// way a person finds one -- by when, and by what they asked. `None` is a
/// person who closed it without choosing.
pub(super) fn pick(root: &Path) -> Result<Option<String>, String> {
    use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
    use crossterm::terminal::{
        EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
    };
    use ratatui::layout::Rect;
    use ratatui::style::{Modifier, Style};
    use ratatui::text::{Line, Span};
    use ratatui::widgets::{Block, Borders, Paragraph};

    let sessions = pickable(root);
    if sessions.is_empty() {
        return Err("nothing to resume in this folder yet".into());
    }
    let folder = root
        .canonicalize()
        .ok()
        .and_then(|path| {
            path.file_name()
                .map(|name| name.to_string_lossy().into_owned())
        })
        .unwrap_or_else(|| ".".into());
    enable_raw_mode().map_err(|e| format!("could not open the picker: {e}"))?;
    let _ = crossterm::execute!(std::io::stdout(), EnterAlternateScreen);
    let restore = || {
        let _ = crossterm::execute!(std::io::stdout(), LeaveAlternateScreen);
        let _ = disable_raw_mode();
    };
    let mut terminal =
        match ratatui::Terminal::new(ratatui::backend::CrosstermBackend::new(std::io::stdout())) {
            Ok(terminal) => terminal,
            Err(error) => {
                restore();
                return Err(format!("could not open the picker: {error}"));
            }
        };
    let mut query = String::new();
    let mut selected = 0usize;
    let chosen = loop {
        let shown: Vec<&Pickable> = sessions
            .iter()
            .filter(|s| {
                let q = query.to_lowercase();
                q.is_empty() || s.title.to_lowercase().contains(&q) || s.id.contains(&q)
            })
            .collect();
        selected = selected.min(shown.len().saturating_sub(1));
        let _ = terminal.draw(|frame| {
            let area = frame.area();
            let block = Block::default()
                .borders(Borders::ALL)
                .title(format!(" Resume a session · {folder} "));
            let inner = block.inner(area);
            frame.render_widget(block, area);
            let width = inner.width as usize;
            let mut lines = vec![
                Line::from(vec![
                    Span::styled(" Search  ", Style::default().add_modifier(Modifier::DIM)),
                    Span::raw(format!("{query}▏")),
                ]),
                Line::from(""),
            ];
            let room = inner.height.saturating_sub(4) as usize;
            let start = selected.saturating_sub(room.saturating_sub(1));
            for (i, s) in shown.iter().enumerate().skip(start).take(room) {
                let right = format!(
                    "{} prompt{}  {}",
                    s.prompts,
                    if s.prompts == 1 { "" } else { "s" },
                    s.id
                );
                let left = format!(" {} {:<12} ", if i == selected { "›" } else { " " }, s.age);
                let space = width.saturating_sub(left.chars().count() + right.chars().count() + 2);
                let title: String = if s.title.chars().count() > space {
                    s.title
                        .chars()
                        .take(space.saturating_sub(1))
                        .chain(['…'])
                        .collect()
                } else {
                    s.title.clone()
                };
                let pad = " ".repeat(space.saturating_sub(title.chars().count()));
                let style = if i == selected {
                    Style::default().add_modifier(Modifier::REVERSED)
                } else {
                    Style::default()
                };
                lines.push(Line::from(vec![
                    Span::styled(left, style),
                    Span::styled(format!("{title}{pad}  "), style),
                    Span::styled(right, style.add_modifier(Modifier::DIM)),
                ]));
            }
            if shown.is_empty() {
                lines.push(Line::from("   nothing matches"));
            }
            frame.render_widget(Paragraph::new(lines), inner);
            let foot = Rect::new(inner.x, inner.bottom().saturating_sub(1), inner.width, 1);
            frame.render_widget(
                Paragraph::new(" ↑↓ choose · Enter resume · type to search · Esc cancel")
                    .style(Style::default().add_modifier(Modifier::DIM)),
                foot,
            );
        });
        let Ok(Event::Key(key)) = event::read() else {
            continue;
        };
        if key.kind == KeyEventKind::Release {
            continue;
        }
        match key.code {
            KeyCode::Esc => break None,
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => break None,
            KeyCode::Enter => break shown.get(selected).map(|s| s.id.clone()),
            KeyCode::Up => selected = selected.saturating_sub(1),
            KeyCode::Down => selected = (selected + 1).min(shown.len().saturating_sub(1)),
            KeyCode::PageUp => selected = selected.saturating_sub(10),
            KeyCode::PageDown => selected = (selected + 10).min(shown.len().saturating_sub(1)),
            KeyCode::Backspace => {
                query.pop();
                selected = 0;
            }
            KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                query.push(c);
                selected = 0;
            }
            _ => {}
        }
    };
    restore();
    Ok(chosen)
}

/// Which session this run is, and where its turns go.
///
/// The order is the whole policy and each step is somebody's expectation: an
/// explicit `--rollout` is a test's or a script's and wins outright;
/// `--resume <id>` is a name that must already exist; `--resume` bare is the
/// most recent in this folder; and a bare `sterna` is **always a new session**,
/// which is what typing the name of a program means everywhere else.
pub(super) fn resolve_session(args: &SessionArgs) -> Result<(SessionId, PathBuf), String> {
    if let Some(path) = &args.rollout {
        let id = args.session.clone().unwrap_or_else(default_session_id);
        return Ok((SessionId::new(id), path.clone()));
    }
    if let Some(id) = &args.session {
        return Ok((SessionId::new(id.clone()), rollout_path_for(&args.root, id)));
    }
    let existing = resumable(&args.root);
    let wanted = match &args.resume {
        // A named session that is not there is a refusal that says what is,
        // rather than a fresh session silently wearing the name.
        Some(id) if !id.is_empty() => {
            let found = existing.iter().find(|(name, _, _)| name == id);
            let Some((name, path, _)) = found else {
                let known: Vec<&str> = existing.iter().map(|(n, _, _)| n.as_str()).collect();
                return Err(if known.is_empty() {
                    format!("no session `{id}` here, and this folder has none to resume")
                } else {
                    format!(
                        "no session `{id}` here; this folder has {}",
                        known.join(", ")
                    )
                });
            };
            Some((name.clone(), path.clone()))
        }
        Some(_) => {
            let Some((name, path, _)) = existing.first() else {
                return Err("nothing to resume in this folder yet".to_string());
            };
            Some((name.clone(), path.clone()))
        }
        None => None,
    };
    let (id, path) = match wanted {
        Some((id, path)) => (SessionId::new(id), path),
        None => {
            let id = default_session_id();
            let path = rollout_path_for(&args.root, &id);
            (SessionId::new(id), path)
        }
    };
    // The directory is part of deciding where the turns go, so it is made
    // here rather than by the caller: a resolved path nobody can open is not
    // a resolution.
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|e| format!("could not create {}: {e}", parent.display()))?;
    }
    Ok((id, path))
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[test]
    fn a_session_id_is_short_sortable_and_typeable() {
        assert_eq!(base36(0), "0");
        assert_eq!(base36(35), "z");
        assert_eq!(base36(36), "10");
        let id = default_session_id();
        assert!(id.len() <= 16, "{id} is too long to read off an exit line");
        assert!(
            id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-'),
            "{id} must be typeable without quoting"
        );
        // Base 36 of a growing second count sorts as it counts, which is what
        // makes "the most recent" answerable from the names alone.
        assert!(base36(1_800_000_000) > base36(1_700_000_000));
    }

    fn args_for(root: &std::path::Path) -> SessionArgs {
        SessionArgs::try_parse_from(["sterna session", "--root", root.to_str().unwrap()]).unwrap()
    }

    #[test]
    fn resume_resolves_by_name_the_newest_or_a_refusal_that_says_what_is_there() {
        let root = std::env::temp_dir().join(format!("sterna-resume-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(sessions_dir(&root)).unwrap();

        // Nothing yet: a bare run is a new session, and `--resume` refuses
        // rather than inventing one.
        let (fresh, path) = resolve_session(&args_for(&root)).unwrap();
        assert_eq!(path, rollout_path_for(&root, fresh.as_str()));
        let mut bare = args_for(&root);
        bare.resume = Some(String::new());
        assert!(
            resolve_session(&bare)
                .unwrap_err()
                .contains("nothing to resume")
        );

        fs::write(rollout_path_for(&root, "older"), "{}\n").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(1100));
        fs::write(rollout_path_for(&root, "newer"), "{}\n").unwrap();

        // A bare `sterna` is always a new session, never one of these two.
        let (id, _) = resolve_session(&args_for(&root)).unwrap();
        assert!(id.as_str() != "newer" && id.as_str() != "older", "{id}");

        // `--resume` with no value is how you ask for the most recent.
        let mut newest = args_for(&root);
        newest.resume = Some(String::new());
        let (id, _) = resolve_session(&newest).unwrap();
        assert_eq!(id.as_str(), "newer");

        // A name reaches the older one even though it is not the newest.
        let mut named = args_for(&root);
        named.resume = Some("older".into());
        let (id, path) = resolve_session(&named).unwrap();
        assert_eq!(id.as_str(), "older");
        assert_eq!(path, rollout_path_for(&root, "older"));

        // And a name that is not there names what is, rather than silently
        // starting a fresh session wearing it.
        let mut wrong = args_for(&root);
        wrong.resume = Some("typo".into());
        let refusal = resolve_session(&wrong).unwrap_err();
        assert!(
            refusal.contains("typo") && refusal.contains("older"),
            "{refusal}"
        );

        assert!(session_listing(&root).contains("newer"));
        let _ = fs::remove_dir_all(&root);
    }

    /// The folder is the whole of the scoping: two directories are two sets
    /// of sessions and neither can see the other.
    #[test]
    fn sessions_belong_to_the_folder_sterna_was_spawned_in() {
        let base = std::env::temp_dir().join(format!("sterna-folders-{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        let (one, two) = (base.join("alpha"), base.join("beta"));
        fs::create_dir_all(sessions_dir(&one)).unwrap();
        fs::create_dir_all(sessions_dir(&two)).unwrap();
        fs::write(rollout_path_for(&one, "in-alpha"), "{}\n").unwrap();

        let mut newest = args_for(&one);
        newest.resume = Some(String::new());
        assert_eq!(resolve_session(&newest).unwrap().0.as_str(), "in-alpha");

        // The same flag one directory over sees nothing, rather than the
        // neighbour's session or some registry's idea of a project.
        let mut elsewhere = args_for(&two);
        elsewhere.resume = Some(String::new());
        assert!(
            resolve_session(&elsewhere)
                .unwrap_err()
                .contains("nothing to resume")
        );
        assert!(session_listing(&two).contains("no resumable sessions"));
        let _ = fs::remove_dir_all(&base);
    }

    /// Every session before per-session files wrote one `.sterna/rollout.jsonl`.
    /// That is somebody's history and it stays reachable.
    #[test]
    fn the_legacy_single_rollout_is_still_listed_and_resumable() {
        let root = std::env::temp_dir().join(format!("sterna-legacy-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join(".sterna")).unwrap();
        fs::write(root.join(".sterna").join("rollout.jsonl"), "{}\n").unwrap();

        let found = resumable(&root);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].0, LEGACY_ID);
        let mut newest = args_for(&root);
        newest.resume = Some(String::new());
        let (id, path) = resolve_session(&newest).unwrap();
        assert_eq!(id.as_str(), LEGACY_ID);
        assert_eq!(path, root.join(".sterna").join("rollout.jsonl"));
        let _ = fs::remove_dir_all(&root);
    }
}
