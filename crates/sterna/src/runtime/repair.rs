//! A narrowly scoped edit of the most recent cell that failed to parse.

use serde::Deserialize;

pub const SOURCE_BYTE_CAP: usize = 128 * 1024;

/// Lines of the failed source shown on each side of the offending one.
const CONTEXT_LINES: u32 = 2;

/// The widest a quoted line is rendered. A minified bundle pasted into a
/// cell would otherwise put the whole program back in the turn this hint
/// exists to keep small.
const MAX_QUOTED_WIDTH: usize = 200;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyntaxFailure {
    pub cell: u64,
    pub source: String,
    /// The parser's own position, as the `## Error` block reports it: `line`
    /// is 1-based and `column` counts characters from the start of that
    /// line. `line == 0` means the position is unknown, which is the one
    /// case [`SyntaxFailure::hint`] quotes nothing for.
    pub line: u32,
    pub column: u32,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Edit {
    cell: u64,
    replace: String,
    with: String,
}

impl SyntaxFailure {
    pub fn new(cell: u64, source: &str, line: u32, column: u32) -> Option<Self> {
        (source.len() <= SOURCE_BYTE_CAP).then(|| Self {
            cell,
            source: source.to_string(),
            line,
            column,
        })
    }

    pub fn apply(&self, json: &str) -> Result<String, String> {
        if json.len() > SOURCE_BYTE_CAP {
            return Err("sterna-edit exceeds the 128 KiB limit".into());
        }
        let edit: Edit = serde_json::from_str(json)
            .map_err(|error| format!("invalid sterna-edit JSON: {error}"))?;
        if edit.cell != self.cell {
            return Err(format!(
                "stale sterna-edit: cell {} is not the eligible cell {}",
                edit.cell, self.cell
            ));
        }
        if edit.replace.is_empty() {
            return Err("sterna-edit `replace` must be nonempty".into());
        }
        let Some(start) = self.source.find(&edit.replace) else {
            return Err("sterna-edit `replace` does not occur in the failed source".into());
        };
        // Count overlapping matches too: "aa" is ambiguous inside "aaa".
        let next = start + edit.replace.chars().next().expect("nonempty").len_utf8();
        if self.source[next..].contains(&edit.replace) {
            return Err(
                "sterna-edit `replace` must occur exactly once in the failed source".into(),
            );
        }
        let end = start + edit.replace.len();
        let resulting_len = self.source.len() - edit.replace.len() + edit.with.len();
        if resulting_len > SOURCE_BYTE_CAP {
            return Err("amended source exceeds the 128 KiB limit".into());
        }
        let mut amended = String::with_capacity(resulting_len);
        amended.push_str(&self.source[..start]);
        amended.push_str(&edit.with);
        amended.push_str(&self.source[end..]);
        if amended == self.source {
            return Err("sterna-edit must change the failed source".into());
        }
        Ok(amended)
    }

    /// **The hint quotes the source it is asking about.** `apply` demands
    /// text occurring exactly once in a program that never ran, so nothing
    /// of it came back in the result -- the model would be quoting from
    /// memory of what it meant to write, against bytes only this struct
    /// still holds. A cell measured on 2026-09-19 was abandoned for exactly
    /// that reason: `SyntaxError: Unterminated string, line 15, column 15`
    /// and no way to see line 15. The excerpt is bounded on both axes so a
    /// long program cannot spend the turn it is trying to save.
    pub fn hint(&self) -> String {
        let quoted = match self.excerpt() {
            Some(excerpt) => format!("Its source around line {}:\n\n{excerpt}\n\n", self.line),
            None => String::new(),
        };
        format!(
            "Nothing in cell {} ran. {quoted}Amend its source with one fence:\n```sterna-edit\n{{\"cell\":{},\"replace\":\"exact text occurring once\",\"with\":\"replacement\"}}\n```",
            self.cell, self.cell
        )
    }

    /// The offending line with a caret under the reported column, a couple
    /// of lines either side for orientation, and a line-number gutter whose
    /// numbers are the ones the error message names.
    ///
    /// `None` when there is no position to point at, or when the position
    /// names a line the source does not have -- a caret under nothing is
    /// worse than no caret, and a wrong line number would send the next
    /// `sterna-edit` at text that is not there.
    fn excerpt(&self) -> Option<String> {
        if self.line == 0 {
            return None;
        }
        let lines: Vec<&str> = self.source.lines().collect();
        let index = usize::try_from(self.line - 1).ok()?;
        if index >= lines.len() {
            return None;
        }
        let first = index.saturating_sub(CONTEXT_LINES as usize);
        let last = index
            .saturating_add(CONTEXT_LINES as usize)
            .min(lines.len() - 1);
        let width = (last + 1).to_string().len();
        let mut out = String::new();
        for (offset, text) in lines[first..=last].iter().enumerate() {
            let number = first + offset + 1;
            let (shown, truncated) = clip(text);
            out.push_str(&format!("{number:>width$} | {shown}"));
            if truncated {
                out.push_str(" …");
            }
            out.push('\n');
            if number == self.line as usize
                && let Some(column) = usize::try_from(self.column)
                    .ok()
                    .filter(|c| !truncated || *c < MAX_QUOTED_WIDTH)
            {
                out.push_str(&format!(
                    "{:>width$} | {}^\n",
                    "",
                    " ".repeat(column.min(shown.chars().count()))
                ));
            }
        }
        Some(out.trim_end().to_string())
    }
}

/// One quoted line, clipped to [`MAX_QUOTED_WIDTH`] characters. Returns
/// whether anything was dropped, because a caret past the clip would point
/// at the wrong character.
fn clip(line: &str) -> (String, bool) {
    let mut shown = String::new();
    for (count, character) in line.chars().enumerate() {
        if count == MAX_QUOTED_WIDTH {
            return (shown, true);
        }
        // A tab in a quoted line puts the caret at the wrong column on every
        // terminal that expands it differently; one space is the width the
        // caret arithmetic above assumes.
        shown.push(if character == '\t' { ' ' } else { character });
    }
    (shown, false)
}
