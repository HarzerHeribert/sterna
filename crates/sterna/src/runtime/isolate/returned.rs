//! A top-level `return`'s value, read out of the isolate for §9.2: an
//! object's fields kept apart, anything else as its JSON with values.
//!
//! Moved out of `isolate.rs` on 2026-09-23 when paged returns replaced the
//! 2 KiB cut and pushed that file past the size ratchet. The one bound here
//! is memory -- [`TERMINAL_WALK_CAP`] bytes per return -- and it is never a
//! context cut: `Terminal::render_within` pages what it renders, and the
//! usage line names the budget it pages within.

use super::*;
use crate::runtime::outcome::{FieldBody, ReturnedField};

/// §9.2's reading of a non-string result: an object's top-level fields kept
/// apart, anything else as its JSON with values.
///
/// Written by the host rather than by `JSON.stringify` because only the host
/// can read a tool object's private tag, and §4 says such an object
/// contributes its preview and never its payload. `cap` bounds the bytes read
/// out of the isolate -- a memory limit, never a context one: the session
/// pages what it renders (`Terminal::render_within`), and nothing is cut
/// here to a number. A field is read as what it is -- a string as its text,
/// an array of strings as lines, a tool result as its preview -- so the
/// renderer can show an excerpt as an excerpt. `JSON.stringify`'s shape
/// otherwise: an `undefined`, a function or a symbol is skipped in an object
/// and `null` in an array, a non-finite number is `null`; a `toJSON` method
/// is not consulted.
pub(super) fn terminal_json(
    scope: &mut v8::PinScope,
    state: &Rc<RuntimeState>,
    value: v8::Local<v8::Value>,
    cap: usize,
) -> Result<Terminal, ReadFailed> {
    if is_plain_object(scope, state, value) {
        let object: v8::Local<v8::Object> = value.try_into().expect("is_object");
        // `return doc.excerpt(…)` bare: the block is the value, and its
        // bookkeeping fields would only wrap it in JSON.
        if let Some((text, whole)) = excerpt_text(scope, object, cap) {
            return Ok(Terminal::Fields(vec![ReturnedField {
                name: "excerpt".into(),
                body: FieldBody::Text(text),
                whole,
            }]));
        }
        return fields_of(scope, state, object, cap);
    }
    let (text, over) = json_within(scope, state, value, cap, 0)?;
    Ok(Terminal::Json { text, cut: over })
}

/// The block of a `File.excerpt` result -- an object whose `text` is the
/// line-numbered block and whose `start` and `lineCount` are numbers -- read
/// within `room`, or `None` for any other object. The `[next: …]` footer the
/// block carries is what the renderer pages by.
fn excerpt_text(
    scope: &mut v8::PinScope,
    object: v8::Local<v8::Object>,
    room: usize,
) -> Option<(String, bool)> {
    let text_key = v8::String::new(scope, "text")?;
    let text = object.get(scope, text_key.into())?;
    let start_key = v8::String::new(scope, "start")?;
    let start = object.get(scope, start_key.into())?;
    let count_key = v8::String::new(scope, "lineCount")?;
    let count = object.get(scope, count_key.into())?;
    if !text.is_string() || !start.is_number() || !count.is_number() {
        return None;
    }
    let string: v8::Local<v8::String> = text.try_into().ok()?;
    let (text, whole) = bounded_utf8(scope, string, room);
    text.starts_with("[lines ").then_some((text, whole))
}

/// An object that is none of the shapes with their own JSON arm: not an
/// array, a collection, an error or a tool result.
fn is_plain_object(
    scope: &mut v8::PinScope,
    state: &Rc<RuntimeState>,
    value: v8::Local<v8::Value>,
) -> bool {
    value.is_object()
        && !value.is_array()
        && !value.is_map()
        && !value.is_set()
        && !value.is_native_error()
        && !value.is_function()
        && bindings::recorded_call(scope, state, value).is_none()
}

/// The object's own enumerable fields in property order, each read within
/// what is left of `cap`.
fn fields_of(
    scope: &mut v8::PinScope,
    state: &Rc<RuntimeState>,
    object: v8::Local<v8::Object>,
    cap: usize,
) -> Result<Terminal, ReadFailed> {
    let names = object
        .get_own_property_names(scope, v8::GetPropertyNamesArgs::default())
        .ok_or(ReadFailed)?;
    let mut fields = Vec::new();
    let mut room = cap;
    for index in 0..names.length() {
        let key = names.get_index(scope, index).ok_or(ReadFailed)?;
        let property = object.get(scope, key).ok_or(ReadFailed)?;
        if json_skips(property) {
            continue;
        }
        let name = key.to_rust_string_lossy(scope);
        let (body, whole, used) = field_body(scope, state, property, room)?;
        room = room.saturating_sub(used);
        fields.push(ReturnedField { name, body, whole });
    }
    Ok(Terminal::Fields(fields))
}

/// One field's body, whether the read was whole, and the bytes it took.
fn field_body(
    scope: &mut v8::PinScope,
    state: &Rc<RuntimeState>,
    value: v8::Local<v8::Value>,
    room: usize,
) -> Result<(FieldBody, bool, usize), ReadFailed> {
    if value.is_string() {
        let string: v8::Local<v8::String> = value.try_into().expect("is_string");
        let (text, whole) = bounded_utf8(scope, string, room);
        let used = text.len();
        return Ok((FieldBody::Text(text), whole, used));
    }
    if let Some(call) = bindings::recorded_call(scope, state, value) {
        let text = preview::render_preview(&call.preview, PREVIEW_TOKEN_CAP);
        let used = text.len();
        return Ok((FieldBody::Text(text), true, used));
    }
    if is_plain_object(scope, state, value) {
        let object: v8::Local<v8::Object> = value.try_into().expect("is_object");
        if let Some((text, whole)) = excerpt_text(scope, object, room) {
            let used = text.len();
            return Ok((FieldBody::Text(text), whole, used));
        }
    }
    if value.is_array() {
        let array: v8::Local<v8::Array> = value.try_into().expect("is_array");
        let length = array.length();
        let mut all_strings = length > 0;
        for index in 0..length {
            let element = array.get_index(scope, index).ok_or(ReadFailed)?;
            if !element.is_string() {
                all_strings = false;
                break;
            }
        }
        if all_strings {
            let mut lines = Vec::with_capacity(length as usize);
            let mut used = 0;
            let mut whole = true;
            for index in 0..length {
                if used >= room {
                    whole = false;
                    break;
                }
                let element = array.get_index(scope, index).ok_or(ReadFailed)?;
                let string: v8::Local<v8::String> = element.try_into().expect("is_string");
                let (text, complete) = bounded_utf8(scope, string, room - used);
                used += text.len() + 1;
                whole &= complete;
                lines.push(text);
                if !complete {
                    break;
                }
            }
            return Ok((FieldBody::Lines(lines), whole, used));
        }
    }
    let (text, over) = json_within(scope, state, value, room, 1)?;
    let used = text.len();
    Ok((FieldBody::Json(text), !over, used))
}

/// `value` as JSON within `cap` bytes, and whether the walk stopped short.
fn json_within(
    scope: &mut v8::PinScope,
    state: &Rc<RuntimeState>,
    value: v8::Local<v8::Value>,
    cap: usize,
    depth: u32,
) -> Result<(String, bool), ReadFailed> {
    let mut json = JsonText {
        out: String::new(),
        cap,
        over: false,
    };
    write_json(scope, state, value, &mut json, depth)?;
    let over = json.over || json.out.len() > cap;
    let mut text = json.out;
    if text.len() > cap {
        let mut end = cap;
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        text.truncate(end);
    }
    Ok((text, over))
}

/// The JSON being written, and whether the walk stopped short of the value.
struct JsonText {
    out: String,
    cap: usize,
    over: bool,
}

impl JsonText {
    fn full(&self) -> bool {
        self.out.len() > self.cap
    }

    fn push(&mut self, text: &str) {
        if self.full() {
            self.over = true;
            return;
        }
        self.out.push_str(text);
    }
}

/// Deeper than this and the walk stops: with the byte cap it is not what
/// ends a cycle, only what bounds the stack while the cap does.
const JSON_MAX_DEPTH: u32 = 64;

/// A value that `JSON.stringify` leaves out of an object.
fn json_skips(value: v8::Local<v8::Value>) -> bool {
    value.is_undefined() || value.is_function() || value.is_symbol()
}

fn json_string(text: &str) -> String {
    serde_json::Value::String(text.to_string()).to_string()
}

fn write_json(
    scope: &mut v8::PinScope,
    state: &Rc<RuntimeState>,
    value: v8::Local<v8::Value>,
    json: &mut JsonText,
    depth: u32,
) -> Result<(), ReadFailed> {
    if json.full() || depth > JSON_MAX_DEPTH {
        json.over = true;
        return Ok(());
    }
    if value.is_null() {
        json.push("null");
        return Ok(());
    }
    if json_skips(value) {
        json.push(if depth == 0 { "undefined" } else { "null" });
        return Ok(());
    }
    if value.is_boolean() {
        json.push(if value.boolean_value(scope) {
            "true"
        } else {
            "false"
        });
        return Ok(());
    }
    if value.is_number() {
        let number = value.number_value(scope).unwrap_or(f64::NAN);
        if number.is_finite() {
            json.push(&value.to_rust_string_lossy(scope));
        } else {
            json.push("null");
        }
        return Ok(());
    }
    if value.is_string() {
        let string: v8::Local<v8::String> = value.try_into().expect("is_string");
        let room = json.cap.saturating_sub(json.out.len()) + 4;
        let (text, whole) = bounded_utf8(scope, string, room);
        json.push(&json_string(&text));
        if !whole {
            json.over = true;
        }
        return Ok(());
    }
    if value.is_native_error() {
        let error = marshal::error_of(scope, value);
        json.push(&format!(
            "{{\"name\":{},\"message\":{}}}",
            json_string(&error.class),
            json_string(&error.message)
        ));
        return Ok(());
    }
    // Before the array and object arms: a tool result is one of those, and
    // its tag is what says it renders as its preview.
    if let Some(call) = bindings::recorded_call(scope, state, value) {
        json.push(&json_string(&preview::render_preview(
            &call.preview,
            PREVIEW_TOKEN_CAP,
        )));
        return Ok(());
    }
    // Before the array and object arms: a `Map` is neither, and the object
    // arm rendered both collections as `{}` — `return new Set([1,2,3])` was
    // the task's answer with `cut: false`, so nothing said anything was
    // dropped. §9.2's own shape for each: a `Map` is an object, a `Set` is
    // an array.
    if value.is_map() {
        let map: v8::Local<v8::Map> = value.try_into().expect("is_map");
        if map.size() > marshal::COLLECTION_MEASURE_WALK_LIMIT {
            json.push("{}");
            json.over = true;
            return Ok(());
        }
        // `as_array` is `[k0, v0, k1, v1, …]`.
        let flat = map.as_array(scope);
        json.push("{");
        let mut first = true;
        let mut index = 0;
        while index + 1 < flat.length() {
            if json.full() {
                json.over = true;
                break;
            }
            let key = flat.get_index(scope, index).ok_or(ReadFailed)?;
            let property = flat.get_index(scope, index + 1).ok_or(ReadFailed)?;
            index += 2;
            if json_skips(property) {
                continue;
            }
            if !first {
                json.push(",");
            }
            first = false;
            json.push(&json_string(&key.to_rust_string_lossy(scope)));
            json.push(":");
            write_json(scope, state, property, json, depth + 1)?;
        }
        json.push("}");
        return Ok(());
    }
    if value.is_set() {
        let set: v8::Local<v8::Set> = value.try_into().expect("is_set");
        if set.size() > marshal::COLLECTION_MEASURE_WALK_LIMIT {
            json.push("[]");
            json.over = true;
            return Ok(());
        }
        let flat = set.as_array(scope);
        json.push("[");
        for index in 0..flat.length() {
            if json.full() {
                json.over = true;
                break;
            }
            if index > 0 {
                json.push(",");
            }
            let element = flat.get_index(scope, index).ok_or(ReadFailed)?;
            if json_skips(element) {
                json.push("null");
            } else {
                write_json(scope, state, element, json, depth + 1)?;
            }
        }
        json.push("]");
        return Ok(());
    }
    if value.is_array() {
        let array: v8::Local<v8::Array> = value.try_into().expect("is_array");
        json.push("[");
        for index in 0..array.length() {
            if json.full() {
                json.over = true;
                break;
            }
            if index > 0 {
                json.push(",");
            }
            let element = array.get_index(scope, index).ok_or(ReadFailed)?;
            if json_skips(element) {
                json.push("null");
            } else {
                write_json(scope, state, element, json, depth + 1)?;
            }
        }
        json.push("]");
        return Ok(());
    }
    if value.is_object() {
        let object: v8::Local<v8::Object> = value.try_into().expect("is_object");
        json.push("{");
        let mut first = true;
        let names = object
            .get_own_property_names(scope, v8::GetPropertyNamesArgs::default())
            .ok_or(ReadFailed)?;
        for index in 0..names.length() {
            if json.full() {
                json.over = true;
                break;
            }
            let key = names.get_index(scope, index).ok_or(ReadFailed)?;
            let property = object.get(scope, key).ok_or(ReadFailed)?;
            if json_skips(property) {
                continue;
            }
            if !first {
                json.push(",");
            }
            first = false;
            json.push(&json_string(&key.to_rust_string_lossy(scope)));
            json.push(":");
            write_json(scope, state, property, json, depth + 1)?;
        }
        json.push("}");
        return Ok(());
    }
    // A bigint: `JSON.stringify` throws on one; its canonical spelling,
    // quoted, is the honest rendering of a value a person asked to see.
    json.push(&json_string(&value.to_rust_string_lossy(scope)));
    Ok(())
}

/// At most `max_bytes` of `string`, whole characters only, and whether that
/// was all of it — so a string the program built out of a payload is read
/// to the cap and not to its length.
fn bounded_utf8(
    scope: &mut v8::PinScope,
    string: v8::Local<v8::String>,
    max_bytes: usize,
) -> (String, bool) {
    if string.utf8_length(scope) <= max_bytes {
        return (string.to_rust_string_lossy(scope), true);
    }
    let mut buffer = vec![0u8; max_bytes];
    let written = string.write_utf8_v2(
        scope,
        &mut buffer,
        v8::WriteFlags::kReplaceInvalidUtf8,
        None,
    );
    buffer.truncate(written);
    (String::from_utf8_lossy(&buffer).into_owned(), false)
}

impl Runtime {
    /// Reduce a returned field's text the decision model read as a log:
    /// `reduce.rs`'s ladder from the rules rung down, with this task's
    /// caches and helper route, and its lossiness line at the head of what
    /// comes back. `None` when nothing was made -- no helper configured, the
    /// economics test refused, the filter was rejected -- and the field is
    /// then paged as it would have been.
    pub fn reduce_returned(&self, text: &str) -> Option<String> {
        match crate::runtime::reduce::reduce_asked(text.to_string(), &self.state) {
            crate::runtime::reduce::Reduction::Made(reduced) => Some(reduced),
            crate::runtime::reduce::Reduction::NotAttempted
            | crate::runtime::reduce::Reduction::Failed(_) => None,
        }
    }
}
