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
//! `conn_close`, `open_path`, `open_url`, `app_quit`, `app_restart`,
//! `platform`.
//! What it hears (`listen`): `engine-line`, `engine-closed`,
//! `quit-requested`.

mod conn;
mod engine;
mod host;
mod open;
mod restart;

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};

use serde_json::{Value, json};
use tauri::{AppHandle, Emitter, Manager, RunEvent, Runtime, State, WindowEvent};

/// The app event that asks the UI whether the person really means to quit.
const QUIT_REQUESTED: &str = "quit-requested";

/// Set by `app_quit`: from then on a close or an exit request goes through.
#[derive(Default)]
struct Quitting(AtomicBool);

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

/// A newer release is in place (the host's `update`): open it once this app
/// has gone, and go. The UI has told the host to keep every session running.
#[tauri::command]
fn app_restart(app: AppHandle, quitting: State<'_, Quitting>) -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let places = engine::Places::of(&app);
    let appimage = std::env::var_os("APPIMAGE")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from);
    let os = std::env::consts::OS;
    let target = restart::target(
        os,
        &exe,
        places.install_root.as_deref(),
        appimage.as_deref(),
    )
    .ok_or("there is no copy of the app here to open again")?;
    restart::open_after_exit(os, &target).map_err(|e| format!("{}: {e}", target.display()))?;
    quitting.0.store(true, Ordering::SeqCst);
    app.exit(0);
    Ok(())
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
    let builder = tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_opener::init())
        .manage(Quitting::default())
        .manage(conn::Connections::default())
        .manage(conn::Announced::default())
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
            app_restart,
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
