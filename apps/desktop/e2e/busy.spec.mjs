// What only the mock can hold still: several sessions at work at once, each
// in a state the real engine passes through in a moment -- a cell being
// written, a cell running -- so the sidebar's live words and the overview's
// rows can be read. Everything else is checked on the real engine.
import { test, expect, newSession, say } from "./fixtures.mjs";

test.use({ scenario: "busy", pace: 1 });

test("sessions at work show how each stands, in the list and in the overview", async ({ app }) => {
  await app.getByRole("dialog", { name: "Choose a folder" }).getByRole("button", { name: "Cancel" }).click();
  const row = (title) => app.locator(".sitem").filter({ hasText: title });
  await expect(row("Document the CSV export").locator(".m")).toContainText("Executing cell 002");
  await expect(row("Add rate limiting").locator(".m")).toContainText("Writing cell 005");
  await expect(row("Fix the broken links").locator(".m")).toHaveText("Waiting for you");
  await app.getByRole("button", { name: /^Overview/ }).click();
  await expect(app.locator(".ovt")).toContainText("sessions at work · 1 session waits for you");
  await expect(app.locator(".ovrow", { hasText: "Document the CSV export" })).toContainText("Executing cell 002");
});

test.describe("a call too large to confirm whole", () => {
  test.use({ scenario: "empty", pace: 0.5 });
  test("can be refused and never allowed, by a button or a key, and the card says why", async ({ app }) => {
    await newSession(app);
    await say(app, "Too large a file to show.");
    const ask = app.locator("#cell-1 .ask");
    await expect(ask).toContainText("cannot be allowed");
    await expect(ask.getByRole("button", { name: "Allow once" })).toBeDisabled();
    await expect(ask.getByRole("button", { name: "Allow for this session" })).toBeDisabled();
    await ask.focus();
    await app.keyboard.press("o");
    await app.keyboard.press("s");
    await expect(app.locator(".toast")).toContainText("This call cannot be allowed");
    await expect(ask).toBeVisible();
    await ask.getByRole("button", { name: "Refuse this once" }).click();
    await expect(app.locator(".answer .first")).toHaveText("Nothing was written: the call was refused.");
  });
});
