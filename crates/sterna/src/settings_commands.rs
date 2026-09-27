//! Shared parser and renderer for `/config` and `sterna config`.
//!
//! This module deliberately contains no terminal or provider integration. Both
//! callers pass already-tokenized arguments and receive text suitable for their
//! respective output surfaces.

use std::path::{Path, PathBuf};

use crate::settings::{self, Scope, Store};

/// Execute a settings command relative to `root`.
pub fn execute(root: &Path, args: &[String]) -> Result<String, String> {
    let mut args = args.to_vec();
    if matches!(args.first().map(String::as_str), Some("config" | "/config")) {
        args.remove(0);
    }

    if args
        .iter()
        .any(|arg| arg == "--profile" || arg.starts_with("--profile="))
    {
        return Ok("Named profile settings are read-only here. Edit global or local base settings; a selected profile may mask that value.".into());
    }
    if args.iter().any(|arg| arg == "--help" || arg == "-h") {
        return Ok(help());
    }

    let store = Store::new(root)?;
    if matches!(args.first().map(String::as_str), Some("import")) {
        return import(&store, &args[1..]);
    }

    let (scope, rest) = take_scope(&args)?;
    if rest.is_empty() {
        return display(&store);
    }

    if rest[0] == "--unset" {
        if rest.len() != 2 {
            return Err("usage: config [global|local|project] --unset <key>".into());
        }
        ensure_key(&rest[1])?;
        let snapshot = store.read(scope)?;
        let loaded = store.save(scope, &snapshot, &[(rest[1].clone(), None)])?;
        return Ok(format!(
            "Unset {} in {}.\n{}",
            rest[1],
            store.path(scope).display(),
            render_key(&loaded.values, &loaded.origins, &rest[1])
        ));
    }
    if rest[0].starts_with('-') {
        return Err(format!(
            "unknown option `{}`; use --help for syntax",
            rest[0]
        ));
    }

    let key = &rest[0];
    ensure_key(key)?;
    if rest.len() == 1 {
        let loaded = store.load(None)?;
        return Ok(render_key(&loaded.values, &loaded.origins, key));
    }

    let value = rest[1..].join(" ");
    settings::validate(key, &value)?;
    let snapshot = store.read(scope)?;
    let loaded = store.save(scope, &snapshot, &[(key.clone(), Some(value))])?;
    Ok(format!(
        "Saved {} to {}.\n{}",
        key,
        store.path(scope).display(),
        render_key(&loaded.values, &loaded.origins, key)
    ))
}

/// Parse process-style arguments, removing `--root PATH` or `--root=PATH`
/// wherever it occurs, then execute the remaining config arguments.
pub fn cli(args: &[String]) -> Result<String, String> {
    let mut root = PathBuf::from(".");
    let mut command = Vec::with_capacity(args.len());
    let mut index = 0;
    while index < args.len() {
        if args[index] == "--root" {
            let path = args
                .get(index + 1)
                .ok_or_else(|| "--root requires a path".to_string())?;
            root = PathBuf::from(path);
            index += 2;
        } else if let Some(path) = args[index].strip_prefix("--root=") {
            if path.is_empty() {
                return Err("--root requires a path".into());
            }
            root = PathBuf::from(path);
            index += 1;
        } else {
            command.push(args[index].clone());
            index += 1;
        }
    }
    execute(&root, &command)
}

fn take_scope(args: &[String]) -> Result<(Scope, &[String]), String> {
    match args.first().map(String::as_str) {
        Some("global") => Ok((Scope::Global, &args[1..])),
        Some("local" | "project") => Ok((Scope::Local, &args[1..])),
        Some(value) if value.starts_with("global=") || value.starts_with("local=") => {
            Err("scope is positional: use `config global <key> <value>`".into())
        }
        _ => Ok((Scope::Local, args)),
    }
}

fn import(store: &Store, args: &[String]) -> Result<String, String> {
    let apply = args.iter().any(|arg| arg == "--apply");
    let sources: Vec<_> = args
        .iter()
        .filter(|arg| arg.as_str() != "--apply")
        .collect();
    if sources.len() != 1 || !matches!(sources[0].as_str(), "claude" | "legacy") {
        return Err("usage: config import <claude|legacy> [--apply]".into());
    }
    if args
        .iter()
        .any(|arg| arg.starts_with('-') && arg != "--apply")
    {
        return Err("the only import option is --apply".into());
    }
    store.import(sources[0], apply)
}

fn display(store: &Store) -> Result<String, String> {
    let loaded = store.load(None)?;
    let mut lines = vec![
        format!("Global: {}", store.path(Scope::Global).display()),
        format!("Local: {}", store.path(Scope::Local).display()),
        "Effective settings:".into(),
    ];
    for spec in settings::specs() {
        lines.push(render_key(&loaded.values, &loaded.origins, spec.key));
    }
    lines.extend(
        loaded
            .notices
            .into_iter()
            .map(|notice| format!("Notice: {notice}")),
    );
    Ok(lines.join("\n"))
}

fn ensure_key(key: &str) -> Result<(), String> {
    if settings::specs().iter().any(|spec| spec.key == key) {
        Ok(())
    } else {
        Err(format!(
            "unknown setting `{key}`; use --help to list supported keys"
        ))
    }
}

fn render_key(
    values: &toml::Value,
    origins: &std::collections::BTreeMap<String, String>,
    key: &str,
) -> String {
    let value = dotted(values, key)
        .map(display_value)
        .unwrap_or_else(|| "<unset>".into());
    let origin = origins.get(key).map(String::as_str).unwrap_or("built-in");
    format!("{key} = {value} ({origin})")
}

fn dotted<'a>(mut value: &'a toml::Value, key: &str) -> Option<&'a toml::Value> {
    for component in key.split('.') {
        value = value.get(component)?;
    }
    Some(value)
}

fn display_value(value: &toml::Value) -> String {
    match value {
        toml::Value::String(value) => value.clone(),
        other => other.to_string(),
    }
}

fn help() -> String {
    let mut lines = vec![
        "Usage:".into(),
        "  config [global|local|project] [<key> [<value>...]]".into(),
        "  config [global|local|project] --unset <key>".into(),
        "  config import <claude|legacy> [--apply]".into(),
        "".into(),
        "Scope defaults to local. Imports preview unless --apply is given.".into(),
        "Values are literal tokens: no shell expansion or evaluation is performed.".into(),
        "".into(),
        "Settings:".into(),
    ];
    for spec in settings::specs() {
        let choices = spec.choices.join("|");
        let value_help = match (type_hint(spec.key), choices.is_empty()) {
            (kind, true) => kind.to_string(),
            (kind, false) => format!("{kind}: {choices}"),
        };
        let timing = if spec.key.starts_with("ui.") {
            "new session here; immediate in /settings"
        } else {
            "new session; /models supports live model changes"
        };
        lines.push(format!(
            "  {:<32} {} — {} [{}; {}]",
            spec.key, spec.label, spec.description, value_help, timing
        ));
    }
    lines.join("\n")
}

fn type_hint(key: &str) -> &'static str {
    if key == "permissions.allow" || key == "permissions.deny" {
        "array of strings"
    } else if key.ends_with(".enabled") {
        "boolean"
    } else if key.starts_with("limits.") || key.ends_with(".every") || key.ends_with(".parallelism")
    {
        "integer"
    } else {
        "string"
    }
}
