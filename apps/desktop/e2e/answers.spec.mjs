// Answering, on the real engine: every answer has a button, a key does
// only what a button does, a sign-in a session runs is the same sheet as
// the host's, and a form belongs to its session.
import { test, expect, newSession, say } from "./world.mjs";

const FORM_KEY = "sk-fixture-desktop-form-fedcba9876543210"; // glasshouse:not-a-secret

test("Refuse this once refuses only this call, and nothing is remembered for the session", async ({ app }) => {
  await newSession(app, "harbor");
  await say(app, "Write note.txt in this folder.");
  const ask = app.locator("#cell-1 .ask");
  await expect(ask.getByRole("button", { name: "Refuse this once" })).toBeVisible({ timeout: 30000 });
  await expect(ask).toContainText("the next identical one asks again");
  await ask.getByRole("button", { name: "Refuse this once" }).click();
  await expect(app.locator("#cell-1 .pill")).toHaveText("Failed", { timeout: 30000 });
  await expect(app.locator("#status")).toContainText("Complete", { timeout: 30000 });
  await app.locator(".toolbar .pillbtn").nth(1).click();
  await expect(app.getByRole("dialog", { name: "Sandbox" })).toContainText("Nothing answered yet");
});

test("with a question in focus Esc does not stop the turn, and a click answers it", async ({ app }) => {
  await newSession(app, "harbor");
  await say(app, "Ask me which lantern.");
  const ask = app.locator(".ask", { hasText: "Which lantern?" });
  await expect(ask).toBeVisible({ timeout: 30000 });
  await ask.focus();
  await app.keyboard.press("Escape");
  await expect(app.locator("#status")).not.toContainText("Stop requested");
  await expect(ask).toBeVisible();
  await ask.getByRole("button", { name: "copper" }).click();
  await expect(ask).toHaveCount(0);
  await expect(app.locator("#status")).toContainText("Complete", { timeout: 30000 });
  await expect(app.locator("#cell-1")).toContainText("copper");
});

test("a /login typed in the composer is the sign-in sheet: the link, the pasted address, gone once sent", async ({ app, world }) => {
  await newSession(app, "harbor");
  await say(app, "/login openai");
  const sheet = app.getByRole("dialog", { name: /^Sign in to/ });
  await expect(sheet).toBeVisible({ timeout: 30000 });
  await expect(sheet.locator(".linkbox code")).toHaveText("https://example.invalid/sign-in", { timeout: 30000 });
  const field = sheet.getByRole("textbox", { name: "Address after sign-in" });
  await field.fill("http://localhost:1455/auth/callback?code=session-fixture");
  await sheet.getByRole("button", { name: "Finish" }).click();
  await expect.poll(() => world.read("gateway-pasted.txt"), { timeout: 30000 }).toBe("http://localhost:1455/auth/callback?code=session-fixture");
  await expect.poll(() => app.evaluate(() => JSON.stringify(window.__sterna.app.S.signin ?? null))).not.toContain("session-fixture");
  if (await field.isVisible()) await expect(field).toHaveValue("");
});

test("a form belongs to its session, and its secret goes from the field to the engine and nowhere else", async ({ app, world }) => {
  await newSession(app, "harbor");
  await say(app, "Answer at once, before a key.");
  await expect(app.locator(".answer .first")).toHaveText("Answered at once.", { timeout: 30000 });
  await say(app, "/key openai");
  const form = app.getByRole("dialog").filter({ has: app.locator("input[data-form]") });
  await expect(form).toBeVisible({ timeout: 30000 });
  // Another session opened meanwhile does not show this one's form.
  await form.getByRole("button", { name: "Put it away" }).isVisible();
  await app.evaluate(() => { const a = window.__sterna.app; a.S.sheet = null; a.changed(); });
  await app.locator(".newbtn").click();
  await app.getByRole("dialog", { name: "New session" }).getByRole("button", { name: /choose another folder/i }).click();
  await app.getByRole("dialog", { name: "New session" }).getByRole("button", { name: "Start session" }).click();
  await expect(app.getByRole("heading", { name: /what should we build/i })).toBeVisible({ timeout: 30000 });
  await expect(app.locator("input[data-form]")).toHaveCount(0);
  await expect(app.locator(".toolbar .project")).toHaveText("alpha");
  await app.locator(".sitem", { hasText: "Answer at once, before a key." }).click();
  await expect(form).toBeVisible();
  const secret = form.locator("input[data-secret]").first();
  await secret.fill(FORM_KEY);
  expect(await app.content()).not.toContain(FORM_KEY);
  await form.getByRole("button", { name: /save|connect|set/i }).last().click();
  await expect(form).toHaveCount(0);
  await expect.poll(() => world.handed(), { timeout: 30000 }).toContain(FORM_KEY);
  expect(await app.content()).not.toContain(FORM_KEY);
  expect(await app.evaluate(() => [...window.__sterna.app.S.forms.values()].length)).toBe(0);
});
