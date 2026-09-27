//! What a search tool's output becomes in a cell: `grep`/`rg` as
//! `Grep.Match[]`, `glob`/`fd` as `string[]`, each path relative to the
//! project root when it lies inside it. Moved out of `bindings.rs` for the
//! size ratchet, 2026-09-23; the root-relative paths are that day's change.

use super::*;

pub(super) fn build_grep<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    args: &Args,
    result: &ToolResult,
    root: &std::path::Path,
) -> (v8::Local<'s, v8::Value>, Value) {
    let fallback = args.get("path").unwrap_or_default();
    let matches: Vec<GrepMatch> = result
        .stdout
        .lines()
        .filter(|line| !line.is_empty())
        .map(|line| parse_match(line, fallback))
        .map(|found| GrepMatch {
            path: in_root(&found.path, root).to_string(),
            ..found
        })
        .collect();

    let array = v8::Array::new(scope, matches.len() as i32);
    for (index, found) in matches.iter().enumerate() {
        let object = v8::Object::new(scope);
        let path = js_string(scope, &found.path);
        set_key(scope, object, "path", path);
        // `null`, not `0`: `grep -r` prints lines that are not located
        // matches — `Binary file … matches` is the routine one — and giving
        // them a line number makes them indistinguishable from a hit at the
        // top of a file, so a program cannot filter them out.
        let line: v8::Local<v8::Value> = match found.line {
            Some(line) => v8::Number::new(scope, line as f64).into(),
            None => v8::null(scope).into(),
        };
        set_key(scope, object, "line", line);
        let text = js_string(scope, &found.text);
        set_key(scope, object, "text", text);
        array.set_index(scope, index as u32, object.into());
    }

    // The preview shows the match, not the object's shape: §3's depth-1
    // object rendering would say `Object n_keys=3`, and §6's worked table
    // shows `path:line "text"`.
    let render = |found: &GrepMatch| {
        Value::String(StringValue::sampled(
            found.path.chars().count() + found.text.chars().count(),
            match_line(found),
        ))
    };
    let head: Vec<Value> = matches.iter().take(3).map(render).collect();
    let last = if matches.len() > 3 {
        matches.last().map(render)
    } else {
        None
    };
    (
        array.into(),
        Value::Array(ArrayValue::sampled(matches.len(), head, last)),
    )
}

pub(super) struct GrepMatch {
    pub(super) path: String,
    /// The line the match is on, and `None` for a line `grep` printed that
    /// is not a located match at all.
    pub(super) line: Option<u64>,
    pub(super) text: String,
}

/// One match as `runtime-contract.md` §6's worked table shows it.
pub(super) fn match_line(found: &GrepMatch) -> String {
    let text: String = found.text.chars().take(160).collect();
    match found.line {
        Some(line) => format!("{}:{}  {text}", found.path, line),
        None => text,
    }
}

/// Splits one `grep -r -n` line into `path`, `line` and `text`.
///
/// The split is on the first colon whose following segment is entirely
/// digits, so a path that itself contains a colon does not move it. A line
/// grep printed without a filename — which BSD `grep` does for a single file
/// argument — takes the requested path.
/// A searched path as the project knows it: relative to the root when it is
/// inside it, as every tool resolves a relative path against the root
/// (`Profile::check`). Measured 2026-09-23: a 405-hit search repeated a
/// 90-character temporary root in every match, the largest single part of
/// the tool results the parent re-read each turn.
pub(crate) fn in_root<'a>(path: &'a str, root: &std::path::Path) -> &'a str {
    let Some(root) = root.to_str().map(|r| r.trim_end_matches('/')) else {
        return path;
    };
    match path.strip_prefix(root) {
        Some(rest) if rest.starts_with('/') && rest.len() > 1 => &rest[1..],
        _ => path,
    }
}

pub(super) fn parse_match(line: &str, fallback: &str) -> GrepMatch {
    let mut start = 0usize;
    while let Some(offset) = line[start..].find(':') {
        let colon = start + offset;
        let rest = &line[colon + 1..];
        if let Some(next) = rest.find(':') {
            let digits = &rest[..next];
            if !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit()) {
                return GrepMatch {
                    path: line[..colon].to_string(),
                    line: digits.parse().ok(),
                    text: rest[next + 1..].to_string(),
                };
            }
        }
        start = colon + 1;
        if start >= line.len() {
            break;
        }
    }
    // `<line>:<text>`, which BSD `grep` prints when its one argument was a
    // file rather than a directory: the path is the one that was asked for.
    if let Some(colon) = line.find(':') {
        let digits = &line[..colon];
        if !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit()) {
            return GrepMatch {
                path: fallback.to_string(),
                line: digits.parse().ok(),
                text: line[colon + 1..].to_string(),
            };
        }
    }
    // Nothing the colon heuristic can place: `Binary file … matches`, a
    // permission notice, anything `grep` says that is not a hit. It is kept
    // rather than dropped — silently losing a line grep printed is worse —
    // and it is marked as unlocated so a program can tell the two apart.
    GrepMatch {
        path: fallback.to_string(),
        line: None,
        text: line.to_string(),
    }
}

pub(super) fn build_glob<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    result: &ToolResult,
    root: &std::path::Path,
) -> (v8::Local<'s, v8::Value>, Value) {
    let paths: Vec<&str> = result
        .stdout
        .lines()
        .filter(|line| !line.is_empty())
        .map(|line| in_root(line, root))
        .collect();
    let array = v8::Array::new(scope, paths.len() as i32);
    for (index, path) in paths.iter().enumerate() {
        let value = js_string(scope, path);
        array.set_index(scope, index as u32, value);
    }
    let head: Vec<Value> = paths.iter().take(3).map(Value::string).collect();
    let last = if paths.len() > 3 {
        paths.last().map(Value::string)
    } else {
        None
    };
    (
        array.into(),
        Value::Array(ArrayValue::sampled(paths.len(), head, last)),
    )
}
