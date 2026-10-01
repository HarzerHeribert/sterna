//! Reading the options object a program passes a host function: one string,
//! one millisecond count, a `{kind, source}` filter, an array of event ids.
//! Moved out of `bindings.rs` for the size ratchet, 2026-10-01.

use super::*;

/// One string property of an options object, or `None` when the object, the
/// property or its value is absent.
pub(super) fn read_option(
    scope: &mut v8::PinScope,
    value: v8::Local<v8::Value>,
    key: &str,
) -> Option<String> {
    let object = v8::Local::<v8::Object>::try_from(value).ok()?;
    let key = v8::String::new(scope, key)?;
    let given = object.get(scope, key.into())?;
    if given.is_undefined() || given.is_null() {
        return None;
    }
    Some(given.to_rust_string_lossy(scope))
}

/// One millisecond count off an options object. A value that is not a finite
/// non-negative number is `None` rather than zero: a zero deadline would
/// cancel the job it was meant to bound.
pub(super) fn read_millis(
    scope: &mut v8::PinScope,
    value: v8::Local<v8::Value>,
    key: &str,
) -> Option<u64> {
    let object = v8::Local::<v8::Object>::try_from(value).ok()?;
    let key = v8::String::new(scope, key)?;
    let given = object.get(scope, key.into())?;
    let number = given.number_value(scope)?;
    (number.is_finite() && number >= 1.0).then_some(number as u64)
}

/// `{kind, source}`, both optional.
pub(super) fn read_filter(
    scope: &mut v8::PinScope,
    value: v8::Local<v8::Value>,
) -> (Option<String>, Option<String>) {
    (
        read_option(scope, value, "kind"),
        read_option(scope, value, "source"),
    )
}

/// An array of event ids. A value that is not a finite id is skipped here
/// rather than becoming `0`, which is not an id any window ever assigns.
pub(super) fn read_ids(scope: &mut v8::PinScope, value: v8::Local<v8::Value>) -> Vec<EventId> {
    let Ok(array) = v8::Local::<v8::Array>::try_from(value) else {
        return Vec::new();
    };
    let mut ids = Vec::with_capacity(array.length() as usize);
    for index in 0..array.length() {
        if let Some(item) = array.get_index(scope, index)
            && let Some(number) = item.number_value(scope)
            && number.is_finite()
            && number >= 1.0
        {
            ids.push(number as EventId);
        }
    }
    ids
}
