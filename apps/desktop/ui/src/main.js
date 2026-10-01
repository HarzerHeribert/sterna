// The app's start: fonts and styles, the bridge to the machine, the store.
import "@fontsource/barlow-condensed/latin-600.css";
import "@fontsource/ibm-plex-mono/latin-400.css";
import "@fontsource/ibm-plex-mono/latin-600.css";
import "./style.css";
import { connectBridge } from "./bridge/index.js";
import { App } from "./app.js";
import { wire } from "./actions.js";
import { applyTheme } from "./theme.js";
import { startingPrefs } from "./prefs.js";

async function main() {
  applyTheme(startingPrefs());
  let bridge;
  try {
    bridge = await connectBridge();
  } catch (e) {
    document.getElementById("splash").innerHTML = `<div class="splash"><div class="why"><b>Sterna could not reach its engine.</b><pre></pre></div></div>`;
    document.querySelector("#splash pre").textContent = String(e?.message || e);
    return;
  }
  const app = new App(bridge);
  wire(app);
  setInterval(() => app.tick(), 100);
  if (import.meta.env.DEV) {
    // A state switcher, and a handle for the checks: development only, never in a built app.
    if (new URLSearchParams(location.search).has("dev")) import("./dev/switcher.js").then((m) => m.mount(app));
    window.__sterna = { app, requestQuit: () => app.requestQuit() };
  }
  await app.start();
}

main();
