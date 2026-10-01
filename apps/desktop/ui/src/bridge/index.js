// The one seam between the UI and the machine: the Tauri shell in the app,
// or the development bridge (dev/bridge.mjs) over a WebSocket in a browser.
// Both offer the same calls: start the host, open loopback connections and
// send lines on them, and hear every line that comes back.

export const inTauri = () => typeof window !== "undefined" && !!window.__TAURI_INTERNALS__;

export async function connectBridge() {
  if (inTauri()) return (await import("./tauri.js")).tauriBridge();
  const url = new URLSearchParams(location.search).get("bridge") || "ws://127.0.0.1:5198";
  return (await import("./socket.js")).socketBridge(url);
}
