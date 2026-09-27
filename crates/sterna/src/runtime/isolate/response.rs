//! What a cell's returned string becomes when it is over the response cap.
//!
//! **The cap bounds the view, and the value is never lost.** A returned
//! string that fits is rendered byte for byte; one that does not used to
//! render as nothing at all, the cell yielding with the size as its reason.
//! That is still the floor, but it is now the last rung rather than the only
//! one (the user, 2026-09-18: *"dafür ist doch reducer da? Warum limitieren?
//! Erst deterministisch Müll entsorgen und dann mit Modellen verkleinern?"*).
//!
//! The first rung is free and deterministic: an over-sized response is
//! stripped of what is provably noise — trailing whitespace, and runs of
//! blank lines — and rendered if that is enough. Nothing a model would miss
//! is removed, and **a response under the cap is never touched**, so the
//! ordinary case is byte-identical to what it was.
//!
//! [`returned`] moved here from `isolate.rs` on 2026-09-18 with the ladder it
//! now walks: what §9.2 makes of a returned value is one responsibility.

use super::returned::terminal_json;
use super::*;

/// The most a response may exceed the cap and still be worth normalising.
///
/// Normalising means holding the whole string in host memory, and whitespace
/// is a few percent of a text — so a response many times the cap cannot be
/// brought under it by this rung and the attempt would cost its own size for
/// nothing. Eight times leaves room for a log padded with blank lines, which
/// is the shape this rung exists for.
pub(super) const NORMALISE_WITHIN: usize = 8;

/// What the cell does with the string it returned.
#[derive(Debug, PartialEq)]
pub(super) enum Response {
    /// Render it exactly as the program returned it.
    Whole,
    /// Render it with provable noise removed, and say so.
    Normalised { text: String, note: String },
    /// Still too large to carry: the cell yields with this reason.
    OverCap { reason: String },
}

/// Removes what no reader would miss: trailing whitespace on every line, and
/// any run of blank lines longer than one.
fn without_noise(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut blank_run = 0usize;
    for line in text.lines() {
        let trimmed = line.trim_end();
        if trimmed.is_empty() {
            blank_run += 1;
            if blank_run > 1 {
                continue;
            }
        } else {
            blank_run = 0;
        }
        out.push_str(trimmed);
        out.push('\n');
    }
    while out.ends_with("\n\n") {
        out.pop();
    }
    if !text.ends_with('\n') {
        out.pop();
    }
    out
}

/// The reason a response too large to be worth normalising yields with.
///
/// Split out so the caller can refuse a string before reading it into host
/// memory and still say the same sentence.
pub(super) fn too_large(bytes: usize, cap: usize) -> String {
    format!(
        "the response is {} bytes, over the cap of {} bytes. The bindings this cell completed are \
         still live, so return a shorter view of the value — a count, a slice, the lines that \
         matter — or inspect it in the next cell rather than returning it whole",
        crate::runtime::preview::thousands(bytes as u64),
        crate::runtime::preview::thousands(cap as u64)
    )
}

/// §9.2's decision for one returned string.
pub(super) fn settle(text: &str, cap: usize) -> Response {
    let bytes = text.len();
    if bytes <= cap {
        return Response::Whole;
    }
    if bytes <= cap.saturating_mul(NORMALISE_WITHIN) {
        let trimmed = without_noise(text);
        if trimmed.len() <= cap && trimmed.len() < bytes {
            let note = format!(
                "[sterna: {} bytes of trailing whitespace and blank lines were removed to fit the \
                 {}-byte response cap; the value your program returned is unchanged]",
                crate::runtime::preview::thousands((bytes - trimmed.len()) as u64),
                crate::runtime::preview::thousands(cap as u64)
            );
            return Response::Normalised {
                text: trimmed,
                note,
            };
        }
    }
    Response::OverCap {
        reason: format!(
            "the response is {} bytes, over the cap of {} bytes, and removing blank lines and \
             trailing whitespace did not bring it under. The bindings this cell completed are \
             still live, so return a shorter view of the value — a count, a slice, the lines \
             that matter — or inspect it in the next cell rather than returning it whole",
            crate::runtime::preview::thousands(bytes as u64),
            crate::runtime::preview::thousands(cap as u64)
        ),
    }
}

/// A top-level `return`'s value, read for what §9.2 makes of it.
///
/// A string is read **in full** — the terminal response is never `marshal`'s
/// sample — unless it is over the response cap, in which case the cell
/// yields with the cap as its reason and nothing of the string is rendered
/// (§9.2: a response is never silently truncated). Any other value becomes
/// its fields, or its JSON, read within [`TERMINAL_WALK_CAP`] and paged by the
/// session when it is rendered.
pub(super) fn returned(
    scope: &mut v8::PinScope,
    state: &Rc<RuntimeState>,
    response_byte_cap: usize,
    value: v8::Local<v8::Value>,
) -> Result<Ending, ReadFailed> {
    if value.is_string() {
        let string: v8::Local<v8::String> = value.try_into().expect("is_string");
        let bytes = string.utf8_length(scope);
        // Far over the cap, the string is not read into host memory at all:
        // `settle` could not bring it under and holding it to try
        // would cost its own size.
        if bytes > response_byte_cap.saturating_mul(NORMALISE_WITHIN) {
            return Ok(Ending::Yielded {
                reason: Some(too_large(bytes, response_byte_cap)),
            });
        }
        let text = string.to_rust_string_lossy(scope);
        return Ok(match settle(&text, response_byte_cap) {
            Response::Whole => {
                Ending::Returned(marshal::marshal(scope, value), Terminal::Text(text))
            }
            Response::Normalised { text, note } => Ending::Returned(
                marshal::marshal(scope, value),
                Terminal::Text(format!("{text}\n{note}")),
            ),
            Response::OverCap { reason } => Ending::Yielded {
                reason: Some(reason),
            },
        });
    }
    // The walk first: it reads every property, so a getter that throws or
    // never returns is found here, and `marshal` -- which would read the
    // same getters again -- runs only once every read has answered.
    let terminal = terminal_json(scope, state, value, TERMINAL_WALK_CAP)?;
    Ok(Ending::Returned(marshal::marshal(scope, value), terminal))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_response_that_fits_is_never_touched() {
        assert_eq!(settle("small", 16), Response::Whole);
        // Exactly at the cap is still whole: the cap is what may be carried.
        assert_eq!(settle("0123456789", 10), Response::Whole);
    }

    #[test]
    fn provable_noise_is_removed_before_any_model_is_asked() {
        // Within `NORMALISE_WITHIN` of the cap, which is the window this
        // rung is for: 213 bytes against a 40-byte cap.
        let padded = format!("head{}\n\n\n\n\ntail", " ".repeat(200));
        assert!(padded.len() <= 40 * NORMALISE_WITHIN);
        let settled = settle(&padded, 40);
        match settled {
            Response::Normalised { text, note } => {
                assert_eq!(text, "head\n\ntail");
                assert!(note.contains("whitespace"), "{note}");
                assert!(note.contains("unchanged"), "{note}");
            }
            other => panic!("the free rung must handle padding: {other:?}"),
        }
    }

    #[test]
    fn a_response_that_is_all_content_says_where_the_value_still_is() {
        let dense = "x".repeat(100);
        match settle(&dense, 10) {
            Response::OverCap { reason } => {
                assert!(reason.contains("still live"), "{reason}");
                assert!(reason.contains("next cell"), "{reason}");
            }
            other => panic!("nothing to remove, so the cap stands: {other:?}"),
        }
    }

    #[test]
    fn a_response_far_over_the_cap_is_not_walked_for_whitespace() {
        // Whitespace is a few percent of a text; a response this far over
        // could not be saved by removing it, and holding it to try costs its
        // own size.
        let huge = format!("{}\n\n\n\n", " ".repeat(10 * 8 * 16));
        assert!(matches!(settle(&huge, 16), Response::OverCap { .. }));
    }
}
