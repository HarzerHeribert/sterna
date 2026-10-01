// Connections, on the real engine: a drop is taken up again where it
// stopped, a connection that keeps closing is said plainly with Try again,
// and a session the window closes on purpose stays closed.
import { test, expect, newSession, say, listed } from "./world.mjs";
import { hook } from "./fixtures.mjs";

async function addressOf(world, title) {
  const s = await listed(world, title, (x) => x.live !== null);
  return (await world.host({ do: "locate", id: s.id })).listening;
}

test("a form put away while the connection was down is closed when the window takes the session up again", async ({ app, world }) => {
  await newSession(app, "harbor");
  await say(app, "Answer at once, then a key.");
  await expect(app.locator(".answer .first")).toHaveText("Answered at once.", { timeout: 30000 });
  await say(app, "/key openai");
  const form = app.getByRole("dialog").filter({ has: app.locator("input[data-form]") });
  await expect(form).toBeVisible({ timeout: 30000 });
  // The form's client leaves; the session puts the form away and says so; the window hears it on its way back.
  await hook(app, "test_drop", { address: await addressOf(world, "Answer at once, then a key") });
  await expect(form).toHaveCount(0, { timeout: 30000 });
  await expect(app.locator(".answer .first")).toHaveText("Answered at once.");
});

test("a connection that keeps closing is said plainly, and Try again takes the session up again", async ({ app, world }) => {
  await newSession(app, "harbor");
  await say(app, "Answer at once, and stay.");
  await expect(app.locator(".answer .first")).toHaveText("Answered at once.", { timeout: 30000 });
  const address = await addressOf(world, "Answer at once, and stay");
  await hook(app, "test_cut", { address });
  await expect(app.locator(".note.fail", { hasText: "connection was lost" })).toBeVisible({ timeout: 30000 });
  await hook(app, "test_cut", { address, off: true });
  await app.getByRole("button", { name: "Try again" }).click();
  await expect(app.locator(".note.fail", { hasText: "connection was lost" })).toHaveCount(0, { timeout: 30000 });
  await say(app, "Answer at once, once more.");
  await expect(app.locator(".answer .first").last()).toHaveText("Answered at once.", { timeout: 30000 });
});

test("a new session left with nothing asked is closed on purpose, and not taken up again", async ({ app }) => {
  await newSession(app, "harbor");
  const first = await app.evaluate(() => window.__sterna.app.S.current);
  await app.locator(".newbtn").click();
  const sheet = app.getByRole("dialog", { name: "New session" });
  await sheet.getByRole("button", { name: /choose another folder/i }).click();
  await sheet.getByRole("button", { name: "Start session" }).click();
  await expect(app.getByRole("heading", { name: /what should we build/i })).toBeVisible({ timeout: 30000 });
  await app.waitForTimeout(2500);
  const known = await app.evaluate((id) => ({ has: window.__sterna.app.S.sessions.has(id), size: window.__sterna.app.S.sessions.size }), first);
  expect(known).toEqual({ has: false, size: 1 });
  await expect(app.locator(".note.fail", { hasText: "connection was lost" })).toHaveCount(0);
});
