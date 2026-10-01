// Plan goal 13, on the real engine: the app opens with the tern's flight
// while `sterna host` starts, then asks which folder to work in.
import { test, expect, newSession } from "./world.mjs";
import { test as bare, startBridge } from "./fixtures.mjs";

test("the tern flies while the engine starts, then the folder choice opens", async ({ app }) => {
  await expect(app.locator("#tern svg[aria-label='Arctic tern in flight']")).toBeVisible();
  await expect(app.getByText("Starting the model gateway")).toBeVisible();
  const sheet = app.getByRole("dialog", { name: "Choose a folder" });
  await expect(sheet).toBeVisible({ timeout: 30000 });
  await expect(app.locator("#tern")).toHaveCount(0);
  await expect(sheet.getByText("Every session runs in one folder.")).toBeVisible();
});

test("a first run chooses a folder in the system's chooser and starts a session there", async ({ app }) => {
  await newSession(app, "harbor");
  await expect(app.locator(".toolbar .project")).toHaveText("harbor");
  await expect(app.locator(".sidebar .fn")).toContainText(["harbor"]);
  await expect(app.locator(".sidebar .sitem.cur")).toContainText("New session");
  // The session's own facts reach the toolbar: its model and its level.
  await expect(app.locator(".toolbar .pillbtn").first()).toContainText("fixture-model");
  await expect(app.locator(".toolbar .pillbtn").nth(1)).toContainText("Ask");
});

bare.describe("an engine that does not start", () => {
  bare("says it could not start, in plain words, and Try again tries again", async ({ page }) => {
    const bridge = await startBridge(["--sterna", "/nonexistent/sterna"]);
    try {
      await page.goto(`/?bridge=${encodeURIComponent(bridge.url)}`);
      await expect(page.getByText("Sterna could not start its engine.")).toBeVisible();
      await expect(page.locator(".splash pre")).toContainText("/nonexistent/sterna");
      await page.getByRole("button", { name: "Try again" }).click();
      await expect(page.getByText("Starting the model gateway")).toBeVisible();
      await expect(page.getByText("Sterna could not start its engine.")).toBeVisible();
    } finally {
      bridge.child.kill("SIGTERM");
    }
  });
});
