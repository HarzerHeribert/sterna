// Screenshots of the running UI, light and dark, at 1480 and 980 px wide:
// the start, the folder choice, a new session, each state of a turn, the
// overview and the setup sheets, against the mock host (dev/bridge.mjs).
//   npm run shots            writes shots/<state>-<scheme>-<width>.png
import { chromium } from "@playwright/test";
import { createServer } from "vite";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { startBridge } from "../e2e/fixtures.mjs";

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
  await page.locator("#tern").waitFor();
  await page.waitForTimeout(500);
  await shot("start");
  const sheet = page.getByRole("dialog", { name: "Choose a folder" });
  await sheet.waitFor();
  await page.waitForTimeout(400);
  await shot("folder");
  await sheet.getByRole("button", { name: /choose another folder/i }).click();
  await sheet.getByRole("button", { name: "Start session" }).click();
  await page.getByRole("heading", { name: /what should we build/i }).waitFor();
  await shot("new");
  const draft = page.getByRole("textbox", { name: "Message" });
  await draft.fill("Fix the failing test in crates/tally. Keep the change small.");
  await draft.press("Enter");
  await page.locator(".reason").waitFor();
  await page.waitForTimeout(1600);
  await shot("reasoning");
  await page.locator("#cell-writing").waitFor();
  await page.waitForTimeout(1800);
  await shot("writing");
  await page.locator("article.cell.running .pill", { hasText: "Running" }).waitFor();
  await page.waitForTimeout(700);
  await shot("running");
  await page.locator("#cell-3 .ask").waitFor({ timeout: 60000 });
  await page.waitForTimeout(400);
  await shot("approval");
  await page.locator("#cell-3 .ask").getByRole("button", { name: "Allow for this session" }).click();
  await page.locator(".answer").waitFor({ timeout: 60000 });
  await page.waitForTimeout(5500);
  await shot("done");
  await page.locator(".toolbar .pillbtn").first().click();
  await page.getByRole("dialog", { name: "Models" }).locator(".mrow").first().waitFor();
  await page.waitForTimeout(400);
  await shot("models");
  await page.getByRole("dialog", { name: "Models" }).getByRole("button", { name: "Done" }).click();
  await page.locator(".toolbar .pillbtn").nth(1).click();
  await page.getByRole("dialog", { name: "Sandbox" }).waitFor();
  await page.waitForTimeout(400);
  await shot("sandbox");
  await page.getByRole("dialog", { name: "Sandbox" }).getByRole("button", { name: "Done" }).click();
  await page.getByRole("button", { name: "Settings" }).click();
  await page.getByRole("dialog", { name: "Settings" }).waitFor();
  await page.waitForTimeout(400);
  await shot("settings");
  await page.getByRole("dialog", { name: "Settings" }).getByRole("button", { name: "Done" }).click();
  await page.getByRole("button", { name: /^Overview/ }).click();
  await page.waitForTimeout(600);
  await shot("overview");
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
