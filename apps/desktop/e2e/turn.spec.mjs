// Plan goal 14, on the real engine: a turn's states -- reasoning, a cell
// being written, a cell running, an approval answered in the conversation,
// the answer -- drawn from the session's events.
import { test, expect, newSession, say } from "./world.mjs";

test("a turn reads as reasoning, writing, running, waiting for you, then its answer", async ({ app, world }) => {
  await newSession(app, "harbor");
  await say(app, "Fix the note in this folder.");
  await expect(app.locator(".you .bubble")).toHaveText("Fix the note in this folder.");
  await expect(app.locator(".reason .rh")).toContainText(/Reasoning ·|Waiting for the model ·/);
  await expect(app.locator(".reason .rh")).toContainText("Reasoning ·");
  await expect(app.locator("#cell-writing .pill")).toContainText("Writing");
  await expect(app.locator("#status")).toContainText("Writing cell 001");
  await expect(app.locator("article.cell.running .pill")).toContainText("Running");
  await expect(app.locator("#status")).toContainText("Executing cell 001");
  // The approval waits inline, inside the cell that asked.
  const ask = app.locator("#cell-1 .ask");
  await expect(ask).toContainText("write note.txt", { timeout: 30000 });
  await expect(app.locator("#cell-1 .pill")).toContainText("Waiting for you");
  await expect(app.locator("#status")).toContainText("Waiting for you");
  await ask.getByRole("button", { name: "Allow once" }).click();
  await expect(app.locator(".answer .first")).toHaveText("The note is fixed.", { timeout: 30000 });
  await expect(app.locator("#status")).toContainText("Complete");
  // The card takes the engine's words: its state, its line, its calls.
  await expect(app.locator("#cell-1 .pill")).toHaveText("Executed");
  await expect(app.locator("#cell-1 .cfoot")).toHaveText("✓ executed · 1 file changed");
  await expect(app.locator("#cell-1 .call .word")).toHaveText(["Returned"]);
  expect(world.read("harbor/note.txt")).toBe("fixed");
});

test("a denied call fails its cell, and Sterna works on", async ({ app }) => {
  await newSession(app, "harbor");
  await say(app, "Write note.txt in this folder.");
  await app.locator("#cell-1 .ask").getByRole("button", { name: "Deny" }).click({ timeout: 30000 });
  await expect(app.locator("#cell-1 .pill")).toHaveText("Failed", { timeout: 30000 });
  await expect(app.locator("#cell-1 .note.fail")).toBeVisible();
  await expect(app.locator("#status")).toContainText("Complete", { timeout: 30000 });
});

test("a message sent while a turn runs is queued, and Take back puts it back in the draft", async ({ app }) => {
  await newSession(app, "harbor");
  await say(app, "Take your time over this one.");
  await expect(app.getByRole("button", { name: "Queue" })).toBeVisible();
  await say(app, "And then this.");
  await expect(app.locator(".queue")).toContainText("Queued · sent when this turn ends");
  await expect(app.locator(".queue .qt")).toHaveText("And then this.");
  await app.getByRole("button", { name: "Take back" }).click();
  await expect(app.locator(".queue")).toHaveCount(0);
  await expect(app.getByRole("textbox", { name: "Message" })).toHaveValue("And then this.");
});

test("stop after this cell ends the turn once the cell is done", async ({ app }) => {
  await newSession(app, "harbor");
  await say(app, "Two steps, please.");
  await expect(app.locator("#status")).toContainText("Thinking");
  await app.getByRole("button", { name: "Stop after this cell" }).click();
  await expect(app.locator("#status")).toContainText("stop requested");
  await expect(app.locator("#status")).toContainText("Stopped", { timeout: 30000 });
  await expect(app.locator(".answer")).toHaveCount(0);
});

test("a letter answers an approval only while the question is in focus", async ({ app }) => {
  await newSession(app, "harbor");
  await say(app, "Write note.txt in this folder.");
  const ask = app.locator("#cell-1 .ask");
  await expect(ask).toBeVisible({ timeout: 30000 });
  const draft = app.getByRole("textbox", { name: "Message" });
  await draft.focus();
  await app.keyboard.type("so");
  await expect(draft).toHaveValue("so");
  await expect(ask).toBeVisible();
  await ask.focus();
  await app.keyboard.press("o");
  await expect(app.locator(".answer .first")).toHaveText("note.txt is written and reads back as it should.", { timeout: 30000 });
});
