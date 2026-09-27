//! `console.*` rendering: the bounded inspection of a cell's own output
//! (moved out of `bindings.rs` for the Phase 59 size ratchet, 2026-09-13;
//! nothing here is new).

use super::*;

/// One ordinary source-sized string fits intact, but a single argument cannot
/// consume the whole cell output budget.
pub(super) const CONSOLE_ARGUMENT_CHARS: usize = 24 * 1024;
pub(super) const CONSOLE_DEPTH: usize = 3;
pub(super) const CONSOLE_KEYS: usize = 12;
/// Leave enough room to name the unvisited part of an object rather than
/// beginning another value whose own omission marker would consume the rest.
pub(super) const CONSOLE_INSPECTION_STOP_CHARS: usize = 128;

pub(super) fn console_string(scope: &mut v8::PinScope, text: v8::Local<v8::String>) -> String {
    console_string_bounded(scope, text, CONSOLE_ARGUMENT_CHARS)
}

pub(super) fn console_string_bounded(
    scope: &mut v8::PinScope,
    text: v8::Local<v8::String>,
    cap: usize,
) -> String {
    let total = text.length();
    // A JavaScript string is counted in UTF-16 units, while the public cap is
    // Unicode scalar values. Reading at most two units per allowed scalar is
    // enough to decide whether the whole value fits.
    if total <= cap.saturating_mul(2) {
        let mut units = vec![0u16; total];
        text.write_v2(scope, 0, &mut units, v8::WriteFlags::empty());
        let whole = String::from_utf16_lossy(&units);
        if whole.chars().count() <= cap {
            return whole;
        }
    }

    let mut omitted = total;
    let mut retained = String::new();
    for _ in 0..4 {
        let marker = format!("[console: {omitted} UTF-16 units omitted; showing true suffix] ");
        let wanted_chars = cap.saturating_sub(marker.chars().count());
        let read_units = total.min(wanted_chars.saturating_mul(2).saturating_add(1));
        let read_start = total - read_units;
        let mut units = vec![0u16; read_units];
        text.write_v2(
            scope,
            read_start as u32,
            &mut units,
            v8::WriteFlags::empty(),
        );
        if units
            .first()
            .is_some_and(|unit| (0xDC00..=0xDFFF).contains(unit))
        {
            units.remove(0);
        }
        let window = String::from_utf16_lossy(&units);
        retained = window
            .chars()
            .rev()
            .take(wanted_chars)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        let next_omitted = total.saturating_sub(retained.encode_utf16().count());
        if next_omitted == omitted {
            break;
        }
        omitted = next_omitted;
    }
    format!(
        "[console: {omitted} UTF-16 units omitted; showing true suffix] {}",
        retained
    )
}

/// Apply the same true-tail contract to a rendered structured argument. The
/// recursive inspector bounds what it reads; this final bound also accounts
/// for JSON quoting and structural punctuation added after that accounting.
pub(super) fn console_rendered_bounded(text: String, cap: usize) -> String {
    let total = text.chars().count();
    if total <= cap {
        return text;
    }

    let mut omitted = total;
    let mut retained = String::new();
    for _ in 0..4 {
        let marker =
            format!("[console: {omitted} rendered characters omitted; showing true suffix] ");
        let wanted = cap.saturating_sub(marker.chars().count());
        retained = text
            .chars()
            .rev()
            .take(wanted)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        let next_omitted = total.saturating_sub(retained.chars().count());
        if next_omitted == omitted {
            break;
        }
        omitted = next_omitted;
    }
    format!("[console: {omitted} rendered characters omitted; showing true suffix] {retained}")
}

pub(super) fn charge_console_inspection(remaining: &mut usize, rendered: &str) {
    *remaining = remaining.saturating_sub(rendered.chars().count());
}

pub(super) fn console_atom(remaining: &mut usize, rendered: impl Into<String>) -> String {
    let rendered = rendered.into();
    charge_console_inspection(remaining, &rendered);
    rendered
}

/// A bounded inspection of one console argument. Objects are read through
/// own property descriptors, so an accessor is named rather than invoked;
/// proxies are not inspected because even asking for their keys runs a trap.
pub(super) fn inspect_console(
    scope: &mut v8::PinScope,
    value: v8::Local<v8::Value>,
    depth: usize,
    seen: &mut Vec<i32>,
    remaining: &mut usize,
) -> String {
    if value.is_string() {
        let string = v8::Local::<v8::String>::try_from(value).expect("string checked");
        let text = if depth == 0 {
            console_string(scope, string)
        } else {
            console_string_bounded(scope, string, *remaining)
        };
        return if depth == 0 {
            text
        } else {
            console_atom(
                remaining,
                serde_json::to_string(&text).unwrap_or_else(|_| "\"<unprintable>\"".into()),
            )
        };
    }
    if value.is_null() {
        return console_atom(remaining, "null");
    }
    if value.is_undefined() {
        return console_atom(remaining, "undefined");
    }
    if value.is_function() {
        return console_atom(remaining, "[Function]");
    }
    if value.is_proxy() {
        return console_atom(remaining, "[Proxy]");
    }
    if !value.is_object() {
        let rendered = value
            .to_string(scope)
            .map(|text| text.to_rust_string_lossy(scope))
            .unwrap_or_else(|| "<unprintable>".into());
        return console_atom(remaining, rendered);
    }
    let object: v8::Local<v8::Object> = match value.try_into() {
        Ok(object) => object,
        Err(_) => return console_atom(remaining, "<unprintable>"),
    };
    let identity = object.get_identity_hash().get();
    if seen.contains(&identity) {
        return console_atom(remaining, "[Circular]");
    }
    let is_array = value.is_array();
    if depth >= CONSOLE_DEPTH {
        return console_atom(remaining, if is_array { "[…]" } else { "{…}" });
    }
    seen.push(identity);
    let rendered = inspect_console_object(scope, object, is_array, depth, seen, remaining);
    seen.pop();
    rendered
}

pub(super) fn inspect_console_object(
    scope: &mut v8::PinScope,
    object: v8::Local<v8::Object>,
    is_array: bool,
    depth: usize,
    seen: &mut Vec<i32>,
    remaining: &mut usize,
) -> String {
    if is_array {
        let Ok(array) = v8::Local::<v8::Array>::try_from(object) else {
            return console_atom(remaining, "<unprintable>");
        };
        let total = array.length() as usize;
        let mut parts = Vec::new();
        let shown_total = total.min(CONSOLE_KEYS);
        let mut stopped = None;
        for index in 0..shown_total {
            if *remaining < CONSOLE_INSPECTION_STOP_CHARS {
                stopped = Some(index);
                break;
            }
            let Some(key) = v8::String::new(scope, &index.to_string()) else {
                continue;
            };
            let shown = array
                .get_own_property_descriptor(scope, key.into())
                .and_then(|descriptor| v8::Local::<v8::Object>::try_from(descriptor).ok())
                .and_then(|descriptor| {
                    v8::String::new(scope, "value")
                        .and_then(|name| descriptor.get(scope, name.into()))
                })
                .map(|value| inspect_console(scope, value, depth + 1, seen, remaining))
                .unwrap_or_else(|| console_atom(remaining, "<empty>"));
            charge_console_inspection(remaining, ", ");
            parts.push(shown);
        }
        if let Some(index) = stopped {
            parts.push(format!("… {} more", total - index));
        } else if total > CONSOLE_KEYS {
            parts.push(format!("… {} more", total - CONSOLE_KEYS));
        }
        charge_console_inspection(remaining, "[]");
        return format!("[{}]", parts.join(", "));
    }
    let Some(names) = object.get_own_property_names(scope, v8::GetPropertyNamesArgs::default())
    else {
        return "<unprintable>".into();
    };
    let total = names.length() as usize;
    let mut parts = Vec::new();
    let shown_total = total.min(CONSOLE_KEYS);
    let mut stopped = None;
    for index in 0..shown_total {
        if *remaining < CONSOLE_INSPECTION_STOP_CHARS {
            stopped = Some(index);
            break;
        }
        let Some(key_value) = names.get_index(scope, index as u32) else {
            continue;
        };
        let Ok(property) = v8::Local::<v8::Name>::try_from(key_value) else {
            continue;
        };
        let key_text = key_value
            .to_string(scope)
            .map(|key| console_string_bounded(scope, key, *remaining))
            .unwrap_or_default();
        let rendered_key = serde_json::to_string(&key_text).unwrap_or_else(|_| "\"?\"".into());
        charge_console_inspection(remaining, &rendered_key);
        charge_console_inspection(remaining, ": , ");
        // A hostile key can consume the argument before its value is even
        // reached. Do not descend into that value (and, for an object, ask V8
        // to enumerate its complete own-key set) after the display is full.
        if *remaining < CONSOLE_INSPECTION_STOP_CHARS {
            stopped = Some(index);
            break;
        }
        let Some(descriptor_value) = object.get_own_property_descriptor(scope, property) else {
            continue;
        };
        let Ok(descriptor) = v8::Local::<v8::Object>::try_from(descriptor_value) else {
            continue;
        };
        let getter = v8::String::new(scope, "get")
            .and_then(|name| descriptor.get(scope, name.into()))
            .is_some_and(|value| !value.is_undefined());
        let shown = if getter {
            console_atom(remaining, "[Getter]")
        } else {
            v8::String::new(scope, "value")
                .and_then(|name| descriptor.get(scope, name.into()))
                .map(|value| inspect_console(scope, value, depth + 1, seen, remaining))
                .unwrap_or_else(|| console_atom(remaining, "undefined"))
        };
        parts.push(format!("{rendered_key}: {shown}"));
    }
    if let Some(index) = stopped {
        parts.push(format!("… {} more", total - index));
    } else if total > CONSOLE_KEYS {
        parts.push(format!("… {} more", total - CONSOLE_KEYS));
    }
    charge_console_inspection(remaining, "{}");
    format!("{{{}}}", parts.join(", "))
}

pub(super) fn console_callback(
    scope: &mut v8::PinScope,
    args: v8::FunctionCallbackArguments,
    _retval: v8::ReturnValue,
) {
    let mut parts: Vec<String> = Vec::new();
    for index in 0..args.length().min(CONSOLE_KEYS as i32) {
        let value = args.get(index);
        let mut remaining = CONSOLE_ARGUMENT_CHARS;
        parts.push(console_rendered_bounded(
            inspect_console(scope, value, 0, &mut Vec::new(), &mut remaining),
            CONSOLE_ARGUMENT_CHARS,
        ));
    }
    if args.length() > CONSOLE_KEYS as i32 {
        parts.push("… more arguments".into());
    }
    state(scope)
        .current
        .borrow_mut()
        .console
        .write_line(&parts.join(" "));
}

// --- refusals ----------------------------------------------------------
