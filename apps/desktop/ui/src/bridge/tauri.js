// The bridge inside the app: the Tauri shell's commands (src-tauri) and its
// events, the dialog plugin's folder chooser, and the system's notifications.
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { getCurrentWebview } from "@tauri-apps/api/webview";
import { open } from "@tauri-apps/plugin-dialog";
import { isPermissionGranted, requestPermission, sendNotification } from "@tauri-apps/plugin-notification";

export async function tauriBridge() {
  const handlers = { line: [], closed: [], quit: [], drop: [] };
  const fire = (name, ...args) => handlers[name].forEach((fn) => fn(...args));
  await listen("engine-line", (e) => fire("line", e.payload.id, e.payload.line));
  await listen("engine-closed", (e) => fire("closed", e.payload.id));
  await listen("quit-requested", () => fire("quit"));
  await getCurrentWebview().onDragDropEvent((e) => {
    if (e.payload.type === "drop" && e.payload.paths?.length) fire("drop", e.payload.paths);
  }).catch(() => {});
  const platform = await invoke("platform").catch(() => ({ os: "macos", home: "" }));
  let allowed = null;
  return {
    kind: "tauri",
    os: platform.os,
    home: platform.home,
    drops: true,
    startHost: () => invoke("host_start"),
    open: (address) => invoke("conn_open", { address }),
    send: (id, line) => invoke("conn_send", { id, line }),
    close: (id) => invoke("conn_close", { id }),
    on: (name, fn) => handlers[name].push(fn),
    chooseFolder: async () => {
      const chosen = await open({ directory: true, multiple: false, title: "Choose a folder to work in" });
      return typeof chosen === "string" ? chosen : null;
    },
    openPath: (path, root) => invoke("open_path", { path, root: root ?? null }),
    openUrl: (url) => invoke("open_url", { url }),
    notify: async (title, body) => {
      if (allowed === null) allowed = (await isPermissionGranted()) || (await requestPermission()) === "granted";
      if (allowed) sendNotification({ title, body });
    },
    setBadge: (n) => getCurrentWindow().setBadgeCount(n > 0 ? n : undefined).catch(() => {}),
    quit: () => invoke("app_quit"),
    restart: () => invoke("app_restart"),
  };
}
