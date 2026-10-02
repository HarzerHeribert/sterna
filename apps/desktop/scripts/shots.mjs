// Screenshots of the running UI, light and dark, at 1480 and 980 px wide:
// the start, the folder choice, a new session, each state of a turn, the
// overview, the setup sheets and a newer release's card, against the mock
// host (dev/bridge.mjs).
//   npm run shots            writes shots/<state>-<scheme>-<width>.png
import { chromium } from "@playwright/test";
import { createServer } from "vite";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { startBridge } from "../e2e/fixtures.mjs";
import { tour } from "./tour.mjs";

const ROOT = fileURLToPath(new URL("..", import.meta.url));
const OUT = fileURLToPath(new URL("../shots/", import.meta.url));
fs.mkdirSync(OUT, { recursive: true });

const vite = await createServer({ configFile: `${ROOT}/vite.config.js`, server: { port: 5299, strictPort: true } });
await vite.listen();
const browser = await chromium.launch();

async function run(scheme, width) {
  // A folder as a person's would read, under the home folder; the mock needs it not to exist.
  const folder = path.join(os.homedir(), "code", "ledger-app");
  const bridge = await startBridge(["--mock", "--scenario", "busy", "--pace", "2", "--any-folder", "--folders", folder, "--origin", "http://127.0.0.1:5299"]);
  const page = await browser.newPage({ viewport: { width, height: 940 }, colorScheme: scheme });
  await page.addInitScript((s) => localStorage.setItem("sterna.desktop.prefs", JSON.stringify({ appearance: s })), scheme);
  const shot = (name) => page.screenshot({ path: `${OUT}/${name}-${scheme}-${width}.png` });
  await page.goto(`http://127.0.0.1:5299/?bridge=${encodeURIComponent(bridge.url)}`);
  await tour(page, shot);
  await page.close();
  bridge.child.kill("SIGTERM");
}

for (const scheme of ["light", "dark"]) for (const width of [1480, 980]) {
  await run(scheme, width);
  console.log(`shots: ${scheme} ${width}`);
}
await browser.close();
await vite.close();
console.log(`written to ${OUT}`);
