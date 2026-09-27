//! Version-bound, exact text replacement for an already existing file.
//!
//! The invariant: **nothing is written until every hunk has been checked
//! against the original text.** One hunk or several, the file is read once,
//! compared against the version the caller saw, every replacement is located
//! in that one reading, and the whole result is installed by a single
//! same-directory rename — so a hunk that does not match leaves the file
//! byte-identical rather than half-edited.

use crate::sandbox::profile::{Access, Profile};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

/// The largest file `edit` will rewrite, and the same number
/// [`crate::project::source_context::SOURCE_CAP`] packs, because a file this
/// can write and `context` cannot pack is writable in principle and
/// unreachable in practice: `edit` needs a delivered context first.
use crate::project::source_context::SOURCE_CAP as MAX_FILE_BYTES;
static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct EditResult {
    pub path: PathBuf,
    pub before_sha256: String,
    pub after_sha256: String,
    /// The first hunk's lines, kept for callers that predate `hunks`.
    pub changed_lines: ChangedLines,
    /// One entry per hunk, in the order the caller gave them; `start` is
    /// the line in the original text.
    pub hunks: Vec<ChangedLines>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ChangedLines {
    pub start: usize,
    pub before: usize,
    pub after: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct EditError {
    pub kind: String,
    pub message: String,
}

impl EditError {
    fn new(kind: &str, message: impl Into<String>) -> Self {
        Self {
            kind: kind.into(),
            message: message.into(),
        }
    }
}
impl std::fmt::Display for EditError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}
impl std::error::Error for EditError {}
impl From<&str> for EditError {
    fn from(message: &str) -> Self {
        Self::new("edit_refused", message)
    }
}
impl From<String> for EditError {
    fn from(message: String) -> Self {
        Self::new("edit_refused", message)
    }
}

/// Replace the one exact occurrence of `expected` when the current file has
/// `expected_sha256`. The replacement is installed by a same-directory rename.
pub fn apply(
    profile: &Profile,
    path: &Path,
    expected_sha256: &str,
    expected: &str,
    replacement: &str,
) -> Result<EditResult, EditError> {
    if expected.is_empty() {
        return Err("exact match must be nonempty".into());
    }
    if expected == replacement {
        return Err("replacement would make no change".into());
    }
    let source = open(profile, path, expected_sha256)?;
    // The kind is named in the message as well as the field: `Display` is
    // all a `ToolError` carries, and `abi::telemetry` reads these words to
    // tell a mutation conflict from an ordinary runtime failure.
    let offset = locate(&source.before, expected).map_err(|reason| {
        EditError::new(
            reason.kind(),
            format!("{}: {}", reason.kind(), reason.sentence()),
        )
    })?;
    let mut after = String::with_capacity(source.before.len() - expected.len() + replacement.len());
    after.push_str(&source.before[..offset]);
    after.push_str(replacement);
    after.push_str(&source.before[offset + expected.len()..]);
    let changed = changed_lines(&source.before, offset, expected, replacement);
    finish(source, after, vec![changed])
}

/// Replace every `olds[i]` with `replacements[i]` as one checked mutation.
///
/// Each hunk must match exactly once in the **original** text and the
/// matched ranges must not overlap; a refusal names the hunk by index and
/// the reason (`hunk_count_mismatch`, `missing_match`, `ambiguous_match`,
/// `overlapping_hunks`) and writes nothing.
pub fn apply_hunks(
    profile: &Profile,
    path: &Path,
    expected_sha256: &str,
    olds: &[String],
    replacements: &[String],
) -> Result<EditResult, EditError> {
    if olds.is_empty() {
        return Err(EditError::new(
            "hunk_count_mismatch",
            "a multi-hunk edit needs at least one hunk",
        ));
    }
    if olds.len() != replacements.len() {
        return Err(EditError::new(
            "hunk_count_mismatch",
            format!(
                "olds has {} hunk(s) and replacements has {}; they must pair up",
                olds.len(),
                replacements.len()
            ),
        ));
    }
    for (index, (old, replacement)) in olds.iter().zip(replacements).enumerate() {
        if old.is_empty() {
            return Err(format!("hunk {index}: exact match must be nonempty").into());
        }
        if old == replacement {
            return Err(format!("hunk {index}: replacement would make no change").into());
        }
    }
    let source = open(profile, path, expected_sha256)?;
    // Every hunk is located against the original text, so a hunk's match
    // cannot depend on what an earlier hunk changed.
    let mut located: Vec<(usize, usize)> = Vec::with_capacity(olds.len());
    for (index, old) in olds.iter().enumerate() {
        let offset = locate(&source.before, old).map_err(|reason| {
            EditError::new(
                reason.kind(),
                format!("hunk {index} ({}): {}", reason.kind(), reason.sentence()),
            )
        })?;
        located.push((offset, index));
    }
    let mut ordered = located.clone();
    ordered.sort_unstable();
    for pair in ordered.windows(2) {
        let (first_offset, first) = pair[0];
        let (second_offset, second) = pair[1];
        if first_offset + olds[first].len() > second_offset {
            return Err(EditError::new(
                "overlapping_hunks",
                format!(
                    "hunks {first} and {second} (overlapping_hunks): their matches overlap at \
                     line {}; narrow one of them so they cover different text",
                    line_at(&source.before, second_offset)
                ),
            ));
        }
    }
    let mut after = String::with_capacity(source.before.len());
    let mut cursor = 0usize;
    for (offset, index) in &ordered {
        after.push_str(&source.before[cursor..*offset]);
        after.push_str(&replacements[*index]);
        cursor = offset + olds[*index].len();
    }
    after.push_str(&source.before[cursor..]);
    let hunks = located
        .iter()
        .map(|(offset, index)| {
            changed_lines(
                &source.before,
                *offset,
                &olds[*index],
                &replacements[*index],
            )
        })
        .collect();
    finish(source, after, hunks)
}

/// The file as read once, with the version check already passed.
struct Source {
    readable: PathBuf,
    metadata: fs::Metadata,
    before: String,
    before_hash: String,
}

/// Why a hunk's text is not at exactly one place, and where it is instead.
///
/// **A refusal is a diagnosis, never a second matching mode.** Only an exact,
/// unique match is ever written; everything here exists so the next attempt
/// can be a correction rather than another reading of the file.
enum Match {
    /// The text occurs nowhere, with a proven near miss when one exists.
    Missing(Option<NearMiss>),
    /// The text occurs more than once, at these 1-based lines.
    Ambiguous {
        /// The first [`SHOWN_MATCHES`] lines a match starts on.
        lines: Vec<usize>,
        /// How many matches were counted, which stops at [`COUNTED_MATCHES`].
        total: usize,
        /// Whether counting stopped there rather than at the last match.
        capped: bool,
    },
}

/// Where the same text sits under one whitespace or line-ending difference.
///
/// The cause is proven by re-finding the text under that one normalisation,
/// not guessed from the shape of the anchor.
struct NearMiss {
    cause: &'static str,
    line: usize,
}

/// How many match positions a refusal names before it stops listing them.
/// Five is enough to see the pattern a sixth would repeat.
const SHOWN_MATCHES: usize = 5;

/// Where counting matches stops. Past this an anchor is hopeless rather than
/// merely ambiguous, and the exact figure would not change what to do about
/// it; stopping also bounds the scan a one-character anchor would otherwise
/// run over a 16 MiB file.
const COUNTED_MATCHES: usize = 1_000;

/// The longest anchor the near-miss probe will try to place. A refusal is
/// rare, but it must not become the expensive path.
const PROBE_LINES: usize = 40;

/// How many candidate start lines the probe verifies before giving up. An
/// anchor whose first line is blank matches everywhere, and that must not
/// turn a diagnosis into a scan of the file for every one of them.
const PROBE_CANDIDATES: usize = 64;

impl Match {
    fn kind(&self) -> &'static str {
        match self {
            Self::Missing(_) => "missing_match",
            Self::Ambiguous { .. } => "ambiguous_match",
        }
    }

    /// What the refusal says, without the hunk prefix a caller may add.
    fn sentence(&self) -> String {
        match self {
            Self::Missing(None) => "exact match was not found".to_string(),
            Self::Missing(Some(near)) => format!(
                "exact match was not found, but the same text is at line {} with {}; \
                 copy it from the file rather than retyping it",
                near.line, near.cause
            ),
            Self::Ambiguous {
                lines,
                total,
                capped,
            } => format!(
                "this text occurs {}{} times, at {}; extend the anchor with a \
                 neighbouring line so it occurs once",
                if *capped { "more than " } else { "" },
                total,
                places(lines, *total)
            ),
        }
    }
}

/// `lines 610 and 618`, or `lines 3, 5, 7, 9, 11 and 2 more`.
fn places(lines: &[usize], total: usize) -> String {
    let named: Vec<String> = lines.iter().map(usize::to_string).collect();
    if named.is_empty() {
        return "no line this refusal could name".to_string();
    }
    let rest = total.saturating_sub(named.len());
    let noun = if named.len() == 1 && rest == 0 {
        "line"
    } else {
        "lines"
    };
    let list = if rest > 0 {
        format!("{} and {rest} more", named.join(", "))
    } else if let Some((last, head)) = named.split_last().filter(|(_, head)| !head.is_empty()) {
        format!("{} and {last}", head.join(", "))
    } else {
        named.join(", ")
    };
    format!("{noun} {list}")
}

fn open(profile: &Profile, path: &Path, expected_sha256: &str) -> Result<Source, EditError> {
    if expected_sha256.len() != 64 || !expected_sha256.bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        return Err("expected SHA-256 must be 64 hexadecimal characters".into());
    }
    let readable = profile
        .check("edit", Access::Read, path)
        .map_err(|error| EditError::new("permission_denied", error.to_string()))?;
    let writable = profile
        .check("edit", Access::Write, path)
        .map_err(|error| EditError::new("permission_denied", error.to_string()))?;
    if readable != writable {
        return Err("read and write checks resolved to different files".into());
    }
    let metadata =
        fs::metadata(&readable).map_err(|error| format!("could not inspect file: {error}"))?;
    if !metadata.is_file() {
        return Err("edit target is not a regular file".into());
    }
    if metadata.len() > MAX_FILE_BYTES {
        return Err(format!("edit target exceeds {MAX_FILE_BYTES} bytes").into());
    }
    let before_bytes =
        fs::read(&readable).map_err(|error| format!("could not read file: {error}"))?;
    if before_bytes.len() as u64 > MAX_FILE_BYTES {
        return Err(format!("edit target exceeds {MAX_FILE_BYTES} bytes").into());
    }
    let before = String::from_utf8(before_bytes).map_err(|_| "edit target is not UTF-8")?;
    let before_hash = sha256(before.as_bytes());
    if !before_hash.eq_ignore_ascii_case(expected_sha256) {
        return Err(EditError::new(
            "stale_hash",
            "The source version changed; refresh context before editing.",
        ));
    }
    Ok(Source {
        readable,
        metadata,
        before,
        before_hash,
    })
}

fn locate(before: &str, expected: &str) -> Result<usize, Match> {
    let mut matches = before.match_indices(expected);
    let Some((first, _)) = matches.next() else {
        return Err(Match::Missing(near_miss(before, expected)));
    };
    let Some((second, _)) = matches.next() else {
        return Ok(first);
    };
    // Offsets arrive ascending, so one forward walk counts every line rather
    // than each one counting from the start of the file again.
    let mut walk = Lines::new(before);
    let mut lines = vec![walk.at(first), walk.at(second)];
    let mut total = 2;
    for (offset, _) in matches {
        total += 1;
        if lines.len() < SHOWN_MATCHES {
            lines.push(walk.at(offset));
        }
        if total == COUNTED_MATCHES {
            return Err(Match::Ambiguous {
                lines,
                total,
                capped: true,
            });
        }
    }
    Err(Match::Ambiguous {
        lines,
        total,
        capped: false,
    })
}

/// The 1-based line `offset` falls on, counted the way a reader counts.
///
/// `\n` cannot occur inside a multi-byte sequence, so counting bytes is
/// counting lines whatever the text holds.
fn line_at(before: &str, offset: usize) -> usize {
    before[..offset]
        .bytes()
        .filter(|byte| *byte == b'\n')
        .count()
        + 1
}

/// [`line_at`] for ascending offsets, counted once across the text instead
/// of from the start for each. The two agree by construction, and
/// `the_walking_counter_agrees_with_the_direct_one` keeps them that way.
struct Lines<'a> {
    text: &'a str,
    counted_to: usize,
    line: usize,
}

impl<'a> Lines<'a> {
    fn new(text: &'a str) -> Self {
        Self {
            text,
            counted_to: 0,
            line: 1,
        }
    }
    fn at(&mut self, offset: usize) -> usize {
        self.line += self.text[self.counted_to..offset]
            .bytes()
            .filter(|byte| *byte == b'\n')
            .count();
        self.counted_to = offset;
        self.line
    }
}

/// Where the anchor sits under one proven whitespace or line-ending
/// difference, or `None` when nothing places it.
///
/// **Nothing here feeds [`locate`].** It produces a line number for a
/// sentence; the edit that follows still has to match exactly and uniquely.
fn near_miss(before: &str, expected: &str) -> Option<NearMiss> {
    line_ending_miss(before, expected).or_else(|| whitespace_miss(before, expected))
}

fn line_ending_miss(before: &str, expected: &str) -> Option<NearMiss> {
    if expected.contains('\n') && !expected.contains('\r') {
        let crlf = expected.replace('\n', "\r\n");
        if let Some(offset) = before.find(&crlf) {
            return Some(NearMiss {
                cause: "CRLF line endings",
                line: line_at(before, offset),
            });
        }
    }
    if expected.contains("\r\n") {
        let lf = expected.replace("\r\n", "\n");
        if let Some(offset) = before.find(&lf) {
            return Some(NearMiss {
                cause: "LF line endings",
                line: line_at(before, offset),
            });
        }
    }
    None
}

/// A named way of reading a line that makes two spellings of it the same.
type Normaliser = (&'static str, fn(&str) -> &str);
/// The three differences worth naming, in the order a reader meets them.
const NORMALISERS: [Normaliser; 3] = [
    ("different trailing whitespace", str::trim_end),
    ("different indentation", str::trim_start),
    ("different leading and trailing whitespace", str::trim),
];

fn whitespace_miss(before: &str, expected: &str) -> Option<NearMiss> {
    let wanted: Vec<&str> = expected.lines().collect();
    if wanted.is_empty() || wanted.len() > PROBE_LINES {
        return None;
    }
    let have: Vec<&str> = before.lines().collect();
    let last_start = have.len().checked_sub(wanted.len())?;
    for (cause, normalise) in NORMALISERS {
        let first = normalise(wanted[0]);
        let mut tried = 0usize;
        for start in 0..=last_start {
            if normalise(have[start]) != first {
                continue;
            }
            tried += 1;
            if tried > PROBE_CANDIDATES {
                break;
            }
            if wanted
                .iter()
                .zip(&have[start..])
                .all(|(want, has)| normalise(want) == normalise(has))
            {
                return Some(NearMiss {
                    cause,
                    line: start + 1,
                });
            }
        }
    }
    None
}

fn changed_lines(before: &str, offset: usize, expected: &str, replacement: &str) -> ChangedLines {
    ChangedLines {
        start: line_at(before, offset),
        before: line_count(expected),
        after: line_count(replacement),
    }
}

/// Installs `after` over the file `source` was read from, rechecking that
/// nothing else changed it in between.
fn finish(
    source: Source,
    after: String,
    hunks: Vec<ChangedLines>,
) -> Result<EditResult, EditError> {
    if after.len() as u64 > MAX_FILE_BYTES {
        return Err(format!("edited file exceeds {MAX_FILE_BYTES} bytes").into());
    }
    let after_hash = sha256(after.as_bytes());
    let parent = source
        .readable
        .parent()
        .ok_or("edit target has no parent directory")?;
    let name = source
        .readable
        .file_name()
        .ok_or("edit target has no file name")?;
    let temp = parent.join(format!(
        ".{}.sterna-edit-{}-{}",
        name.to_string_lossy(),
        std::process::id(),
        TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed)
    ));
    let write_result = (|| -> Result<(), EditError> {
        let mut output = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp)
            .map_err(|error| format!("could not create temporary edit file: {error}"))?;
        output
            .set_permissions(source.metadata.permissions())
            .map_err(|error| format!("could not preserve file permissions: {error}"))?;
        output
            .write_all(after.as_bytes())
            .and_then(|()| output.sync_all())
            .map_err(|error| format!("could not write temporary edit file: {error}"))?;
        let latest = fs::read(&source.readable)
            .map_err(|error| format!("could not recheck file: {error}"))?;
        if sha256(&latest) != source.before_hash {
            return Err("file changed while the edit was being prepared".into());
        }
        install(&temp, &source.readable)
    })();
    if write_result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    write_result?;
    let changed_lines = hunks
        .first()
        .cloned()
        .expect("an edit carries at least one hunk");
    Ok(EditResult {
        path: source.readable,
        before_sha256: source.before_hash,
        after_sha256: after_hash,
        changed_lines,
        hunks,
    })
}

fn sha256(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn line_count(text: &str) -> usize {
    text.bytes().filter(|byte| *byte == b'\n').count() + usize::from(!text.is_empty())
}

#[cfg(not(target_os = "windows"))]
fn install(temp: &Path, target: &Path) -> Result<(), EditError> {
    fs::rename(temp, target)
        .map_err(|error| EditError::new("io", format!("could not install edit: {error}")))
}

#[cfg(target_os = "windows")]
fn install(temp: &Path, target: &Path) -> Result<(), EditError> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::{
        MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, MoveFileExW,
    };
    let mut from: Vec<u16> = temp.as_os_str().encode_wide().collect();
    let mut to: Vec<u16> = target.as_os_str().encode_wide().collect();
    from.push(0);
    to.push(0);
    // SAFETY: both buffers are NUL-terminated and live for the duration of
    // the call. The flags request the replacement semantics Unix rename has.
    let ok = unsafe {
        MoveFileExW(
            from.as_ptr(),
            to.as_ptr(),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    };
    if ok == 0 {
        return Err(EditError::new(
            "io",
            format!(
                "could not install edit: {}",
                std::io::Error::last_os_error()
            ),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The walking counter exists only so a run of ascending offsets is not
    /// counted from the start of the file each time. If it ever disagreed
    /// with [`line_at`], a refusal would name a line the result does not.
    #[test]
    fn the_walking_counter_agrees_with_the_direct_one() {
        let text = "a\nbb\n\nccc\ndddd\ne\n";
        let mut walk = Lines::new(text);
        let mut offset = 0;
        while offset <= text.len() {
            assert_eq!(
                walk.at(offset),
                line_at(text, offset),
                "offset {offset} of {text:?}"
            );
            offset += 1;
        }
    }

    /// `line_at` counts `\n` bytes, and a multi-byte character cannot hold
    /// one — so a file of them still reports the line a reader counts.
    #[test]
    fn a_multi_byte_file_reports_the_line_a_reader_counts() {
        let text = "straße\nΩmega\n🙂🙂🙂\nziel\n";
        let offset = text.find("ziel").expect("present");
        assert_eq!(line_at(text, offset), 4);
        let mut walk = Lines::new(text);
        assert_eq!(walk.at(offset), 4);
    }

    #[test]
    fn places_names_a_pair_a_list_and_a_remainder() {
        assert_eq!(places(&[610, 618], 2), "lines 610 and 618");
        assert_eq!(
            places(&[3, 5, 7, 9, 11], 7),
            "lines 3, 5, 7, 9, 11 and 2 more"
        );
        assert_eq!(places(&[42], 1), "line 42");
        assert_eq!(places(&[], 0), "no line this refusal could name");
    }
}
