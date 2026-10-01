//! `around` on `grep` and `rg`: each match with the lines either side,
//! printed by Sterna with their numbers and recorded as shown, so the next
//! cell can `edit` them without a `context`. The user, 2026-10-01: "a read
//! with an if condition ... if line contains this string ... return it and
//! the 10 lines or 5 lines around it".
//!
//! The windows are cut from the file by the runtime, never from what a
//! program returns, because only lines Sterna itself printed can bind an
//! edit: a program could hand back lines it had changed.

use super::search::{in_root, parse_match};
use super::*;

/// The most lines `around` takes either side.
const MOST: usize = 20;

/// The `around` a search asked for, taken off its arguments: Sterna honours
/// it, and the search tool never sees it.
pub(super) fn take(tool: &str, args: &mut Args) -> Result<Option<usize>, String> {
    if !matches!(tool, "grep" | "rg") {
        return Ok(None);
    }
    let Some(given) = args.take("around") else {
        return Ok(None);
    };
    match given.trim().parse::<usize>() {
        Ok(lines) if (1..=MOST).contains(&lines) => Ok(Some(lines)),
        _ => Err(format!(
            "`around` takes a whole number of lines from 1 to {MOST}; it was `{given}`"
        )),
    }
}

/// Prints every located match of a finished search with `around` lines
/// either side, file by file in the order the search reported them. A file
/// whose windows do not fit what is left of the turn is left out whole and
/// counted.
pub(super) fn deliver(state: &RuntimeState, args: &Args, result: &ToolResult, around: usize) {
    let root = state.profile.root();
    let fallback = args.get("path").unwrap_or_default();
    let mut files: Vec<(String, Vec<usize>)> = Vec::new();
    for line in result.stdout.lines().filter(|line| !line.is_empty()) {
        let found = parse_match(line, fallback);
        let Some(number) = found.line else {
            continue;
        };
        let path = in_root(&found.path, root).to_string();
        match files.iter_mut().find(|(known, _)| *known == path) {
            Some((_, lines)) => lines.push(number as usize),
            None => files.push((path, vec![number as usize])),
        }
    }
    let left = files
        .iter()
        .filter(|(path, lines)| !state.note_around(path, lines, around))
        .count();
    if left > 0 {
        state.note_around_left(left);
    }
}
