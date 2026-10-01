//! The native half of the Sterna desktop app.
//!
//! It holds no agent logic. The engine is a separate program (`sterna host`,
//! shipped beside this one as a sidecar); this crate starts it, carries
//! newline-delimited lines between the engine's loopback port and the web
//! UI, opens files with the system's default app and web pages in the
//! browser, and decides when the app may quit. Everything the person sees is
//! the web UI in `apps/desktop/ui`.
//!
//! What the UI calls (`invoke`): `host_start`, `conn_open`, `conn_send`,
//! `conn_close`, `open_path`, `open_url`, `app_quit`, `updater_enabled`,
//! `platform`.
//! What it hears (`listen`): `engine-line`, `engine-closed`,
//! `quit-requested`.

mod conn;
mod engine;
mod host;
mod open;

use std::sync::atomic::{AtomicBool, Ordering};

use serde_json::{Value, json};
use tauri::utils::config::PluginConfig;
use tauri::{AppHandle, Emitter, Manager, RunEvent, Runtime, State, WindowEvent};

/// The app event that asks the UI whether the person really means to quit.
const QUIT_REQUESTED: &str = "quit-requested";

/// Set by `app_quit`: from then on a close or an exit request goes through.
#[derive(Default)]
struct Quitting(AtomicBool);

/// Whether the updater plugin was registered (see [`updater_on`]).
struct Updater(bool);

/// The update channel stays shut until the release key exists.
///
/// `plugins.updater.pubkey` in tauri.conf.json is empty until the person who
/// cuts releases generates an updater key pair (`npx tauri signer generate`)
/// and pastes the public half there. Until then the updater plugin is not
/// registered at all, so nothing can fetch or install an update that no key
/// could verify, and `updater_enabled` answers `false` so the UI never offers
/// to look for one. The capability still lists `updater:default`: the
/// permission is resolved at build time from the plugin crate, and with the
/// plugin unregistered its commands simply do not exist at run time.
fn updater_on(plugins: &PluginConfig) -> bool {
    plugins
        .0
        .get("updater")
        .and_then(|updater| updater.get("pubkey"))
        .and_then(Value::as_str)
        .is_some_and(|key| !key.trim().is_empty())
}

/// Asks the UI, unless `app_quit` already said the app may go.
///
/// The app never decides on its own: the UI shows the person what is still
/// running and calls `app_quit` once they choose. Every request is passed on,
/// a second one while the question is still open included; the UI keeps a
/// single question on screen. There is deliberately no timeout that quits
/// behind the UI's back. On macOS the Dock's Quit and logging out are not
/// routed through here (the window library ends the app on
/// `applicationWillTerminate` without asking); the engine host runs on its
/// own, so its sessions keep running and the app reattaches next time.
fn ask_to_quit<R: Runtime>(app: &AppHandle<R>) -> bool {
    if app.state::<Quitting>().0.load(Ordering::SeqCst) {
        return false;
    }
    let _ = app.emit(QUIT_REQUESTED, json!({}));
    true
}

/// The person chose to quit: let the exit through and leave.
#[tauri::command]
fn app_quit(app: AppHandle, quitting: State<'_, Quitting>) {
    quitting.0.store(true, Ordering::SeqCst);
    app.exit(0);
}

#[tauri::command]
fn updater_enabled(updater: State<'_, Updater>) -> bool {
    updater.0
}

/// `{"os": "macos" | "windows" | "linux", "home": "<home folder or empty>"}`.
#[tauri::command]
fn platform(app: AppHandle) -> Value {
    let home = app
        .path()
        .home_dir()
        .map(|home| home.to_string_lossy().into_owned())
        .unwrap_or_default();
    json!({"os": std::env::consts::OS, "home": home})
}

/// The macOS app menu with its Quit item routed through [`ask_to_quit`].
///
/// The stock Quit item calls `terminate:`, which ends the app without an
/// `ExitRequested` event, so Cmd-Q could not be asked about. This is the
/// default menu with that one item swapped for an ordinary one on the same
/// shortcut.
#[cfg(target_os = "macos")]
fn app_menu<R: Runtime>(app: &AppHandle<R>) -> tauri::Result<tauri::menu::Menu<R>> {
    use tauri::menu::{Menu, MenuItem, MenuItemKind};
    let menu = Menu::default(app)?;
    if let Some(MenuItemKind::Submenu(first)) = menu.items()?.into_iter().next() {
        let items = first.items()?;
        if let Some(MenuItemKind::Predefined(_)) = items.last() {
            first.remove_at(items.len() - 1)?;
        }
        let name = &app.package_info().name;
        first.append(&MenuItem::with_id(
            app,
            QUIT_MENU_ID,
            format!("Quit {name}"),
            true,
            Some("CmdOrCtrl+Q"),
        )?)?;
    }
    Ok(menu)
}

#[cfg(target_os = "macos")]
const QUIT_MENU_ID: &str = "sterna-quit";

/// Builds and runs the app; returns when it exits.
pub fn run() {
    let context = tauri::generate_context!();
    let updater = updater_on(&context.config().plugins);
    let mut builder = tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_opener::init())
        .manage(Quitting::default())
        .manage(Updater(updater))
        .manage(conn::Connections::default())
        .manage(conn::Announced::default());
    if updater {
        builder = builder.plugin(tauri_plugin_updater::Builder::new().build());
    }
    let builder = builder
        .on_window_event(|window, event| {
            if let WindowEvent::CloseRequested { api, .. } = event
                && window.label() == "main"
                && ask_to_quit(window.app_handle())
            {
                api.prevent_close();
            }
        })
        .invoke_handler(tauri::generate_handler![
            host::host_start,
            conn::conn_open,
            conn::conn_send,
            conn::conn_close,
            open::open_path,
            open::open_url,
            app_quit,
            updater_enabled,
            platform,
        ]);

    #[cfg(target_os = "macos")]
    let builder = builder.menu(app_menu).on_menu_event(|app, event| {
        if event.id() == QUIT_MENU_ID && !ask_to_quit(app) {
            app.exit(0);
        }
    });

    let app = match builder.build(context) {
        Ok(app) => app,
        // Nothing can be shown without a window; say why on stderr and stop.
        Err(e) => {
            eprintln!("Sterna could not start: {e}");
            std::process::exit(1);
        }
    };
    app.run(|app, event| {
        // `code` is `None` when the system or the last closed window asks;
        // `app_quit`'s own `exit(0)` carries a code and always goes through.
        if let RunEvent::ExitRequested {
            code: None, api, ..
        } = event
            && ask_to_quit(app)
        {
            api.prevent_exit();
        }
    });
}

/// A scratch folder of one test's own, removed when the test ends.
#[cfg(test)]
pub(crate) struct Scratch(pub std::path::PathBuf);

#[cfg(test)]
impl Scratch {
    pub(crate) fn new(name: &str) -> Self {
        let dir =
            std::env::temp_dir().join(format!("sterna-desktop-test-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("a scratch folder");
        Self(std::fs::canonicalize(&dir).expect("a scratch folder"))
    }
}

#[cfg(test)]
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[cfg(test)]
mod tests {
    use super::{PluginConfig, updater_on};
    use serde_json::json;

    #[test]
    fn the_release_feed_opens_only_with_a_public_key() {
        let plugins = |updater| PluginConfig([("updater".to_string(), updater)].into());
        assert!(!updater_on(&PluginConfig::default()));
        assert!(!updater_on(&plugins(json!({"pubkey": ""}))));
        assert!(!updater_on(&plugins(json!({"pubkey": "  "}))));
        assert!(!updater_on(&plugins(json!({"endpoints": []}))));
        assert!(updater_on(&plugins(json!({"pubkey": "a-public-key"}))));
    }
}
