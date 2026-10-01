// Quitting, on the real engine: with sessions the app's host started still
// running, the window asks; what the person chooses is what the host does.
import { test, expect, newSession, say, listed } from "./world.mjs";
import { hook } from "./fixtures.mjs";

async function oneRunning(app) {
  await newSession(app, "harbor");
  await say(app, "Answer at once, and keep running.");
  await expect(app.locator(".answer .first")).toHaveText("Answered at once.", { timeout: 30000 });
}

test("asks, and Keep running leaves the session to the host", async ({ app, world }) => {
  await oneRunning(app);
  await hook(app, "test_request_quit");
  const sheet = app.getByRole("dialog", { name: "Quit Sterna" });
  await expect(sheet).toContainText("1 session this app started is still running.");
  await sheet.getByRole("button", { name: "Keep running in the background" }).click();
  await expect(app.getByText("Sterna has quit.")).toBeVisible();
  expect((await listed(world, "Answer at once, and keep running")).live).not.toBeNull();
});

test("asks, and Stop them ends the session", async ({ app, world }) => {
  await oneRunning(app);
  await hook(app, "test_request_quit");
  await app.getByRole("dialog", { name: "Quit Sterna" }).getByRole("button", { name: "Stop them" }).click();
  await expect(app.getByText("Sterna has quit.")).toBeVisible();
  await listed(world, "Answer at once, and keep running", (s) => s.live === null);
});

test("asks, and Cancel keeps the window", async ({ app }) => {
  await oneRunning(app);
  await hook(app, "test_request_quit");
  await app.getByRole("dialog", { name: "Quit Sterna" }).getByRole("button", { name: "Cancel" }).click();
  await expect(app.getByRole("dialog")).toHaveCount(0);
  await expect(app.locator(".answer .first")).toHaveText("Answered at once.");
});

test("quitting before the list was shown leaves every session as it was", async ({ page, world }) => {
  await world.host({ do: "start", root: world.harbor, task: "Answer at once, before the window." });
  await listed(world, "Answer at once, before the window", (s) => s.live?.state === "idle");
  await page.goto(`/?bridge=${encodeURIComponent(world.bridge.url)}`);
  await page.waitForFunction(() => window.__sterna?.app);
  await page.evaluate(() => window.__sterna.requestQuit());
  await expect(page.getByText("Sterna has quit.")).toBeVisible();
  expect((await listed(world, "Answer at once, before the window")).live).not.toBeNull();
});
