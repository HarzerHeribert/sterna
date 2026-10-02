//! Setup without a terminal, on the host port (`docs/engine.md`): the
//! person's settings read and saved, the desktop's own preferences kept,
//! the gateway's accounts listed, a key handed to the gateway, and a sign-in
//! run with its progress on the connection that asked for it.
//!
//! **A key travels once**: from the client's line to the gateway's stdin.
//! Nothing here prints, logs or keeps it, and no argument carries it.

use std::io::{BufRead, BufReader, Write};
use std::net::TcpStream;
use std::path::Path;
use std::process::{Command as Process, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use serde_json::{Value, json};

use crate::settings::{Scope, Store};

/// The person's settings store, as a host reads it: the global scope, with
/// no project of its own.
fn store(folder: &Path) -> Result<Store, String> {
    Store::with_global(folder, crate::project::workflows::user_directory())
}

/// Every global setting as saved, by its dotted key.
pub(crate) fn settings(folder: &Path) -> Result<Value, String> {
    let snapshot = store(folder)?.read(Scope::Global)?;
    let mut values = serde_json::Map::new();
    flatten("", &snapshot.values, &mut values);
    Ok(json!({ "values": values }))
}

fn flatten(prefix: &str, value: &toml::Value, out: &mut serde_json::Map<String, Value>) {
    match value {
        toml::Value::Table(table) => {
            for (key, value) in table {
                let key = if prefix.is_empty() {
                    key.clone()
                } else {
                    format!("{prefix}.{key}")
                };
                flatten(&key, value, out);
            }
        }
        toml::Value::String(text) => {
            out.insert(prefix.to_string(), Value::String(text.clone()));
        }
        other => {
            out.insert(
                prefix.to_string(),
                serde_json::to_value(other).unwrap_or(Value::Null),
            );
        }
    }
}

/// Saves one global setting, as the settings store checks it.
pub(crate) fn set_setting(folder: &Path, key: &str, value: &str) -> Result<Value, String> {
    let store = store(folder)?;
    let snapshot = store.read(Scope::Global)?;
    store.save(
        Scope::Global,
        &snapshot,
        &[(key.to_string(), Some(value.to_string()))],
    )?;
    Ok(json!({}))
}

fn preferences_path(folder: &Path) -> std::path::PathBuf {
    folder.join("preferences.json")
}

/// The desktop's own preferences, whole; `{}` before it has any.
pub(crate) fn preferences(folder: &Path) -> Value {
    std::fs::read_to_string(preferences_path(folder))
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_else(|| json!({}))
}

/// Keeps the desktop's preferences, whole.
pub(crate) fn set_preferences(folder: &Path, preferences: &Value) -> Result<Value, String> {
    if !preferences.is_object() {
        return Err("preferences are one object".into());
    }
    super::data::write_private(
        &preferences_path(folder),
        preferences.to_string().as_bytes(),
    )
    .map_err(|e| format!("the preferences could not be kept: {e}"))?;
    Ok(json!({}))
}

/// The gateway the host's setup runs through.
fn gateway() -> Result<Process, String> {
    let binary = crate::gateway::installed()
        .ok_or("no inference gateway is installed beside Sterna or on PATH")?;
    let mut command = Process::new(binary);
    command.stderr(Stdio::null());
    Ok(command)
}

/// The gateway's accounts, as `entitlements` lists them, each with only the
/// models that answer a conversation: an account's list also carries image,
/// speech, batch and approval-review ids, which a client must not offer as a
/// session's model (the terminal's picker leaves them out by the same rule).
pub(crate) fn accounts() -> Result<Value, String> {
    let output = gateway()?
        .args(["entitlements", "--json"])
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("the gateway could not be run: {e}"))?;
    let mut listed: Value = String::from_utf8_lossy(&output.stdout)
        .lines()
        .find_map(|line| serde_json::from_str(line).ok())
        .ok_or("the gateway did not list its accounts")?;
    for account in listed["accounts"].as_array_mut().into_iter().flatten() {
        if let Some(models) = account["models"].as_array_mut() {
            models.retain(|id| id.as_str().is_some_and(crate::models::chat_capable));
        }
    }
    Ok(json!({ "accounts": listed["accounts"].take() }))
}

/// Hands the gateway `provider`'s key on its stdin.
pub(crate) fn set_key(provider: &str, key: &str) -> Result<Value, String> {
    if provider.is_empty() || provider.starts_with('-') {
        return Err("set_key needs a provider".into());
    }
    let mut child = gateway()?
        .args(["credentials", "set", provider, "--json"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .map_err(|e| format!("the gateway could not be run: {e}"))?;
    if let Some(mut stdin) = child.stdin.take() {
        stdin
            .write_all(key.as_bytes())
            .map_err(|e| format!("the gateway did not take the key: {e}"))?;
    }
    let output = child
        .wait_with_output()
        .map_err(|e| format!("the gateway did not answer: {e}"))?;
    if !output.status.success() {
        return Err("the gateway refused the key".into());
    }
    let said = String::from_utf8_lossy(&output.stdout)
        .lines()
        .find_map(|line| serde_json::from_str::<Value>(line).ok())
        .unwrap_or_else(|| json!({}));
    Ok(json!({"provider": provider, "stored_in": said["stored_in"].clone()}))
}

/// Runs a sign-in to `provider`: the gateway's progress goes to `writer`
/// as it comes, an address the client pastes goes back to the gateway, and
/// the last line says whether it connected. Returns when it is over.
pub(crate) fn sign_in(
    provider: &str,
    reader: &mut BufReader<TcpStream>,
    writer: &mut TcpStream,
) -> Result<(), String> {
    if provider.is_empty() || provider.starts_with('-') {
        return Err("sign_in needs a provider".into());
    }
    let mut child = gateway()?
        .args([
            "subscriptions",
            "connect",
            provider,
            "--json",
            "--no-browser",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .map_err(|e| format!("the gateway could not be run: {e}"))?;
    let _ = writeln!(writer, "{}", json!({"ok": {}}));
    let mut stdin = child.stdin.take();
    let stdout = child.stdout.take().ok_or("no output from the gateway")?;
    let over = Arc::new(AtomicBool::new(false));
    let mut out = writer.try_clone().map_err(|e| e.to_string())?;
    let finished = Arc::clone(&over);
    let progress = std::thread::spawn(move || {
        let mut connected = false;
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            let Ok(said) = serde_json::from_str::<Value>(line.trim()) else {
                continue;
            };
            connected |= said["state"] == "connected";
            if writeln!(out, "{}", json!({ "sign_in": said })).is_err() {
                break;
            }
        }
        finished.store(true, Ordering::SeqCst);
        connected
    });
    // The client's lines while it runs: a pasted address, or a cancel.
    let _ = reader
        .get_ref()
        .set_read_timeout(Some(Duration::from_millis(200)));
    let mut line = String::new();
    while !over.load(Ordering::SeqCst) {
        match reader.read_line(&mut line) {
            Ok(0) => {
                let _ = child.kill();
                break;
            }
            Ok(_) => {
                let said: Value = serde_json::from_str(line.trim()).unwrap_or(Value::Null);
                line.clear();
                if let Some(address) = said["paste"].as_str()
                    && let Some(stdin) = stdin.as_mut()
                {
                    let _ = writeln!(stdin, "{}", address.trim());
                    let _ = stdin.flush();
                } else if said["cancel"] == true {
                    let _ = child.kill();
                }
            }
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) => {}
            Err(_) => {
                let _ = child.kill();
                break;
            }
        }
    }
    let _ = reader.get_ref().set_read_timeout(None);
    drop(stdin);
    let connected = progress.join().unwrap_or(false);
    let _ = child.wait();
    let _ = writeln!(writer, "{}", json!({"done": {"connected": connected}}));
    Ok(())
}

/// The login shell's `PATH` joined to `path`: a desktop app is started with
/// almost none, and a session's tools need the person's own.
pub(crate) fn login_path(path: &str) -> String {
    #[cfg(unix)]
    {
        let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".into());
        let Ok(mut child) = Process::new(&shell)
            .args(["-l", "-c", "printf %s \"$PATH\""])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
        else {
            return path.to_string();
        };
        let deadline = std::time::Instant::now() + Duration::from_secs(3);
        while child.try_wait().ok().flatten().is_none() {
            if std::time::Instant::now() > deadline {
                let _ = child.kill();
                let _ = child.wait();
                return path.to_string();
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        let mut said = String::new();
        if let Some(mut stdout) = child.stdout.take() {
            let _ = std::io::Read::read_to_string(&mut stdout, &mut said);
        }
        let mut joined: Vec<String> = Vec::new();
        for entry in said.split(':').chain(path.split(':')) {
            if !entry.is_empty() && !joined.iter().any(|seen| seen == entry) {
                joined.push(entry.to_string());
            }
        }
        joined.join(":")
    }
    #[cfg(not(unix))]
    {
        path.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preferences_are_kept_whole_and_only_as_one_object() {
        let folder = std::env::temp_dir().join(format!(
            "sterna-preferences-{}",
            crate::engine::data::token()
        ));
        assert_eq!(preferences(&folder), json!({}));
        assert!(set_preferences(&folder, &json!(["theme", "amber"])).is_err());
        assert_eq!(preferences(&folder), json!({}), "nothing was kept");
        let kept = json!({"theme": "amber", "card": {"bird": false}});
        set_preferences(&folder, &kept).unwrap();
        assert_eq!(preferences(&folder), kept);
        let _ = std::fs::remove_dir_all(&folder);
    }

    #[test]
    fn settings_read_by_their_dotted_keys() {
        let table: toml::Value = toml::from_str(
            "[model]\nparent = \"m\"\n[sandbox]\nlevel = \"ask\"\nhosts = [\"a\"]\n",
        )
        .unwrap();
        let mut out = serde_json::Map::new();
        flatten("", &table, &mut out);
        assert_eq!(out["model.parent"], "m");
        assert_eq!(out["sandbox.level"], "ask");
        assert_eq!(out["sandbox.hosts"], json!(["a"]));
    }
}
