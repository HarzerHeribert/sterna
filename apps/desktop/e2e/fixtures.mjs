// What every check shares: a development bridge of its own, with a mock
// host in the scenario the check names, and the page opened on it.
import { test as base, expect } from "@playwright/test";
import { spawn } from "node:child_process";
import { fileURLToPath } from "node:url";

const DESKTOP = fileURLToPath(new URL("..", import.meta.url));

export function startBridge(args) {
  const child = spawn(process.execPath, ["dev/bridge.mjs", "--port", "0", ...args], { cwd: DESKTOP, stdio: ["ignore", "pipe", "pipe"] });
  let err = "";
  child.stderr.on("data", (d) => { err += d; });
  return new Promise((ok, fail) => {
    let out = "";
    child.stdout.on("data", (d) => {
      out += d;
      const at = out.indexOf("\n");
      if (at >= 0) ok({ url: JSON.parse(out.slice(0, at)).bridge, child, stderr: () => err });
    });
    child.on("exit", (code) => fail(new Error(`the bridge exited (${code}): ${err}`)));
  });
}

export const test = base.extend({
  scenario: ["empty", { option: true }],
  pace: [0.5, { option: true }],
  bridgeArgs: [null, { option: true }],
  bridge: async ({ scenario, pace, bridgeArgs }, use) => {
    const bridge = await startBridge(bridgeArgs || ["--mock", "--scenario", scenario, "--pace", String(pace)]);
    await use(bridge);
    // The bridge this check started, by its own handle.
    bridge.child.kill("SIGTERM");
  },
  app: async ({ page, bridge }, use) => {
    await page.goto(`/?bridge=${encodeURIComponent(bridge.url)}`);
    await use(page);
  },
});
export { expect };

/** Past the flight and the folder sheet, into a new session in the folder the chooser gives. */
export async function newSession(page, name = "ledger-app") {
  const sheet = page.getByRole("dialog", { name: /choose a folder|new session/i });
  await expect(sheet).toBeVisible({ timeout: 30000 });
  await sheet.getByRole("button", { name: /choose (another|a) folder/i }).click();
  await expect(sheet.locator(".frow", { hasText: name })).toHaveAttribute("aria-pressed", "true");
  await sheet.getByRole("button", { name: "Start session" }).click();
  await expect(page.getByRole("heading", { name: /what should we build\?/i })).toBeVisible({ timeout: 30000 });
}

/** Types a message in the composer and sends it with Enter. */
export async function say(page, text) {
  const draft = page.getByRole("textbox", { name: "Message" });
  await draft.fill(text);
  await draft.press("Enter");
}

/** The development bridge's own test hooks. */
export const hook = (page, op, args = {}) => page.evaluate(([o, a]) => window.__sterna.app.bridge.test(o, a), [op, args]);
