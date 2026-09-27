//! Bounded parsing of the assistant's action channel.
//!
//! Several complete `sterna` fences form one cell, in message order.  The
//! runtime compiles that one source before executing it, so accepting several
//! fences does not turn them into separately validated cells.

/// Keep one response comfortably below the runtime's existing 128 KiB repair
/// bound, even when framing separators are added.
pub const MAX_PROGRAM_BYTES: usize = 128 * 1024;

/// Prevent an adversarial response from making parsing cost depend on an
/// unbounded number of tiny fences.
pub const MAX_STERNA_BLOCKS: usize = 32;

/// The most a cell's descriptor may carry.
///
/// One line, because that is what it is for: the person reads it above the
/// cell and the model reads it back after every compaction, and a descriptor
/// that needs two lines is a descriptor that is wrong
/// (`docs/workbench.md`). 200 bytes is a full line at a
/// wide terminal with room for multi-byte characters; beyond it the text is
/// cut at a character boundary rather than refused, because a long
/// description is a style problem and never a reason to run nothing.
pub const MAX_DESCRIPTION_BYTES: usize = 200;

/// The one line the model wrote about the cell it is about to run, from the
/// fence channel: the last non-empty prose line before the first `sterna`
/// fence, bounded by [`MAX_DESCRIPTION_BYTES`].
///
/// **The last line, not the first.** A model that writes a paragraph before
/// its program ends that paragraph with the sentence about what it is doing
/// now; taking the first line would carry the preamble of the thought rather
/// than its conclusion.
///
/// Prose *after* the fence is not a descriptor: the cell had already been
/// written by then, so it cannot be what the cell is for. Text inside any
/// fence is skipped, so an example in a ```ts block is never mistaken for a
/// sentence about the cell.
#[must_use]
pub fn descriptor_of(assistant_text: &str) -> Option<String> {
    let mut prose: Vec<&str> = Vec::new();
    let lines: Vec<&str> = assistant_text.lines().collect();
    let mut i = 0;
    while i < lines.len() {
        if let Some(info) = lines[i].strip_prefix("```") {
            if matches!(info.trim(), "sterna" | "sterna-edit") {
                return last_sentence(&prose);
            }
            // Any other fence is an example: skip its body so nothing inside
            // it reads as the model's own sentence.
            let mut end = i + 1;
            while end < lines.len() && lines[end] != "```" {
                end += 1;
            }
            i = if end < lines.len() {
                end + 1
            } else {
                lines.len()
            };
            continue;
        }
        prose.push(lines[i]);
        i += 1;
    }
    None
}

/// The descriptor of a native call's `description` argument, bounded the same
/// way, so both channels answer to one rule.
#[must_use]
pub fn bound_description(text: &str) -> Option<String> {
    last_sentence(&[text])
}

/// The last non-empty line of `prose`, trimmed, flattened to one line and cut
/// to [`MAX_DESCRIPTION_BYTES`] at a character boundary.
fn last_sentence(prose: &[&str]) -> Option<String> {
    let line = prose
        .iter()
        .rev()
        .map(|line| line.trim())
        .find(|line| !line.is_empty())?;
    // A markdown bullet or heading marker is the model formatting a sentence,
    // not part of what it said.
    let line = line
        .trim_start_matches(['#', '-', '*', '>'])
        .trim_start_matches(char::is_whitespace);
    if line.is_empty() {
        return None;
    }
    let mut cut = line.len().min(MAX_DESCRIPTION_BYTES);
    while !line.is_char_boundary(cut) {
        cut -= 1;
    }
    Some(line[..cut].to_string())
}

/// What one assistant message contained.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Extracted {
    /// One or more complete `sterna` fences, composed in message order.
    Program(String),
    /// One complete `sterna-edit` fence: JSON amending a parse-failed cell.
    Edit(String),
    /// Multiple edits, or an edit mixed with executable source. Neither runs.
    TwoBlocks,
    /// No attempt to use the executable channel.
    Prose,
    /// An attempted executable channel was malformed or exceeded a bound.
    Invalid(String),
}

#[derive(Debug)]
struct Fence<'a> {
    info: &'a str,
    body: String,
    complete: bool,
}

/// Extracts the action channel from an assistant response.
///
/// The fence grammar deliberately stays smaller than general Markdown: three
/// backticks at the start of a line, an exactly matching `sterna` or
/// `sterna-edit` info string, and a closing line containing exactly three
/// backticks. Other fenced languages are examples and never execute.
pub fn extract_program(assistant_text: &str) -> Extracted {
    let fences = fences(assistant_text);
    let mut programs = Vec::new();
    let mut edits = Vec::new();
    let mut program_bytes = 0usize;

    for fence in fences {
        match fence.info {
            "sterna" => {
                if !fence.complete {
                    return Extracted::Invalid("unfinished sterna block".into());
                }
                if programs.len() == MAX_STERNA_BLOCKS {
                    return Extracted::Invalid(format!(
                        "too many sterna blocks; maximum is {MAX_STERNA_BLOCKS}"
                    ));
                }
                let separator_bytes = usize::from(!programs.is_empty()) * "\n;\n".len();
                program_bytes = match program_bytes
                    .checked_add(separator_bytes)
                    .and_then(|bytes| bytes.checked_add(fence.body.len()))
                {
                    Some(bytes) if bytes <= MAX_PROGRAM_BYTES => bytes,
                    _ => {
                        return Extracted::Invalid(format!(
                            "sterna source exceeds {MAX_PROGRAM_BYTES} bytes"
                        ));
                    }
                };
                programs.push(fence.body);
            }
            "sterna-edit" => {
                if !fence.complete {
                    return Extracted::Invalid("unfinished sterna-edit block".into());
                }
                if fence.body.len() > MAX_PROGRAM_BYTES {
                    return Extracted::Invalid(format!(
                        "sterna-edit exceeds {MAX_PROGRAM_BYTES} bytes"
                    ));
                }
                edits.push(fence.body);
            }
            // `pane` and `pane-edit` are the fences sessions saved before the
            // rename carry; a resumed session's model may still write one, and
            // it gets the repair instead of silently running nothing.
            info if info.starts_with("sterna") || matches!(info, "pane" | "pane-edit") => {
                return Extracted::Invalid(format!(
                    "malformed executable fence `{info}`; expected `sterna` or `sterna-edit`"
                ));
            }
            _ => {}
        }
    }

    if malformed_xml_attempt(assistant_text) {
        return Extracted::Invalid("malformed executable sterna attempt".into());
    }
    if !edits.is_empty() {
        return if edits.len() == 1 && programs.is_empty() {
            Extracted::Edit(edits.pop().expect("one edit counted above"))
        } else {
            Extracted::TwoBlocks
        };
    }
    match programs.len() {
        0 => Extracted::Prose,
        1 => Extracted::Program(programs.pop().expect("one program counted above")),
        _ => {
            // A statement boundary protects the next block from a trailing
            // line comment and from automatic-semicolon-insertion surprises.
            Extracted::Program(programs.join("\n;\n"))
        }
    }
}

/// Returns a natural prose answer, without its trailing line breaks. Any
/// attempted executable channel remains outside this path.
pub fn completion_text(text: &str) -> Option<String> {
    if !matches!(extract_program(text), Extracted::Prose) {
        return None;
    }
    let visible = text.trim_end_matches(['\r', '\n']);
    (!visible.trim().is_empty()).then(|| visible.to_string())
}

fn fences(text: &str) -> Vec<Fence<'_>> {
    let lines: Vec<&str> = text.lines().collect();
    let mut found = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        let Some(rest) = lines[i].strip_prefix("```") else {
            i += 1;
            continue;
        };
        let info = rest.trim();
        let mut body = Vec::new();
        let mut j = i + 1;
        while j < lines.len() && lines[j] != "```" {
            body.push(lines[j]);
            j += 1;
        }
        found.push(Fence {
            info,
            body: body.join("\n"),
            complete: j < lines.len(),
        });
        i = if j < lines.len() { j + 1 } else { lines.len() };
    }
    found
}

fn malformed_xml_attempt(text: &str) -> bool {
    let mut in_fence = false;
    text.lines().any(|line| {
        if line.starts_with("```") {
            if in_fence {
                if line == "```" {
                    in_fence = false;
                }
            } else {
                in_fence = true;
            }
            return false;
        }
        if in_fence {
            return false;
        }
        let line = line.trim_start().to_ascii_lowercase();
        line.starts_with("<sterna")
            || line.starts_with("</sterna")
            || (line.starts_with('<') && line.contains("-sterna"))
    })
}

#[cfg(test)]
mod descriptor_tests {
    use super::*;

    #[test]
    fn the_line_before_the_fence_is_what_the_cell_is_for() {
        let text = "Reading the ssh design to find what I have to change.\n\
                    ```sterna\nconst design = await read({path: \"d.md\"});\n```\n";
        assert_eq!(
            descriptor_of(text).as_deref(),
            Some("Reading the ssh design to find what I have to change.")
        );
    }

    #[test]
    fn a_paragraph_contributes_its_last_line_not_its_first() {
        // The conclusion of the thought is the sentence about this cell; the
        // opening is the preamble to it.
        let text = "I have two candidates for where the refusal lives.\n\
                    Searching both for the command-refusal grammar.\n\
                    ```sterna\nawait rg({pattern: \"refuse\"});\n```";
        assert_eq!(
            descriptor_of(text).as_deref(),
            Some("Searching both for the command-refusal grammar.")
        );
    }

    #[test]
    fn an_example_in_another_fence_is_never_mistaken_for_the_sentence() {
        let text = "Checking the shape first.\n\
                    ```ts\nconst wrong = \"this is an example, not a descriptor\";\n```\n\
                    ```sterna\nreturn 1;\n```";
        assert_eq!(
            descriptor_of(text).as_deref(),
            Some("Checking the shape first.")
        );
    }

    #[test]
    fn prose_after_the_fence_describes_nothing_the_cell_could_be_for() {
        let text = "```sterna\nreturn 1;\n```\nThat is what I will do next.";
        assert_eq!(descriptor_of(text), None);
    }

    #[test]
    fn a_bullet_is_formatting_and_not_part_of_what_was_said() {
        let text = "- Formatting the changed files.\n```sterna\nreturn 1;\n```";
        assert_eq!(
            descriptor_of(text).as_deref(),
            Some("Formatting the changed files.")
        );
    }

    #[test]
    fn a_long_description_is_cut_at_a_character_boundary_and_never_refused() {
        let long = "ä".repeat(400);
        let bound = bound_description(&long).expect("a long line is cut, not dropped");
        assert!(bound.len() <= MAX_DESCRIPTION_BYTES);
        assert!(
            bound.chars().all(|c| c == 'ä'),
            "cut mid-character: {bound}"
        );
    }

    #[test]
    fn nothing_said_is_no_descriptor_rather_than_an_empty_one() {
        assert_eq!(descriptor_of("```sterna\nreturn 1;\n```"), None);
        assert_eq!(bound_description("   "), None);
    }
}
