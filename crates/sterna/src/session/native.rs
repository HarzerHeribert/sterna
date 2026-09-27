//! What a provider-native `execute_cell` call carries, and where a cell's
//! descriptor comes from.
//!
//! One responsibility, moved out of `session.rs` whole (2026-09-17) when the
//! descriptor pushed that file past its size baseline. The user's rule:
//! *"Pushing a file past 2.5k lines is a split signal not a shorten signal."*
//!
//! **The schema declares two keys and this module is the only reader of
//! them** (`crate::wire::Surface::tool_definitions`): `code`, and the
//! `description` the person reads above the cell.

use serde_json::Value;

/// The refusal for an input that is not the declared shape. One sentence,
/// naming both keys, because the model repairs from this text alone.
pub(super) const MALFORMED_CELL_INPUT: &str = "ProtocolError: execute_cell input must be {\"code\": string, \"description\": string}; nothing ran.";

/// The program a native call carries, or `None` when the input is not the
/// declared shape.
///
/// **`description` is optional here although the schema requires it**: a
/// model that omits the line still runs, and the screen falls back to the
/// cell's first source line (`docs/workbench.md`). The
/// change degrades, it does not break — including for a session already in
/// flight when this shipped.
pub(super) fn cell_source(input: &Value) -> Option<&str> {
    input
        .as_object()
        .filter(|object| {
            object
                .keys()
                .all(|key| key == "code" || key == "description")
        })
        .and_then(|object| object.get("code"))
        .and_then(Value::as_str)
}

/// What the model said this cell is for, from whichever channel it used: the
/// native call's `description` argument, or the last prose line before the
/// fence.
///
/// A **lowered** direct frame gets none: its source is Sterna's spelling of the
/// person's own tool call, so there is no model-authored cell for a sentence
/// to be about (`tool-abi.md` §19 — the screen may not imply the model wrote
/// what it did not).
pub(super) fn descriptor(
    lowered: bool,
    native: Option<(&String, &String, &Value)>,
    assistant_text: &str,
) -> Option<String> {
    if lowered {
        return None;
    }
    match native {
        Some((_, _, input)) => input
            .as_object()
            .and_then(|object| object.get("description"))
            .and_then(Value::as_str)
            .and_then(crate::prompt::bound_description),
        None => crate::prompt::descriptor_of(assistant_text),
    }
}
