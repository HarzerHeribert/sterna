// Plan goal 16, on the real engine: each setup sheet -- models, sign-in, an
// API key, the sandbox level, settings -- opens and saves through the host's
// setup commands, with a fake gateway that records what it is handed.
import { test, expect, newSession } from "./world.mjs";

const KEY = "sk-fixture-desktop-check-0123456789abcdef"; // glasshouse:not-a-secret

test("the models sheet lists the gateway's accounts, and a choice changes the session's model and effort, and saves", async ({ app, world }) => {
  await newSession(app, "harbor");
  await app.locator(".toolbar .pillbtn").first().click();
  const sheet = app.getByRole("dialog", { name: "Models" });
  await expect(sheet.locator(".ghd", { hasText: "chatgpt-pro" })).toContainText("Signed in");
  await expect(sheet.locator(".ghd", { hasText: "claude-max" }).getByRole("button", { name: "Sign in" })).toBeVisible();
  await sheet.locator(".mrow").filter({ has: app.locator(".nm", { hasText: /^fixture-two$/ }) }).click();
  // The sheet is modal: the toolbar behind it is read by its class.
  const pill = app.locator(".toolbar .pillbtn").first();
  await expect(pill).toHaveText(/^fixture-two/);
  await sheet.getByRole("button", { name: "high", exact: true }).click();
  await expect(pill).toHaveText(/^fixture-two·\s*high/);
  await expect.poll(async () => (await world.host({ do: "settings" })).values["model.parent"]).toBe("fixture-two");
});

test("signing in from the models sheet runs on the host: the link, the pasted address, connected", async ({ app, world }) => {
  await newSession(app, "harbor");
  await app.locator(".toolbar .pillbtn").first().click();
  await app.getByRole("dialog", { name: "Models" }).locator(".ghd", { hasText: "claude-max" }).getByRole("button", { name: "Sign in" }).click();
  const sheet = app.getByRole("dialog", { name: "Sign in to claude-max" });
  await expect(sheet.locator(".linkbox code")).toHaveText("https://example.invalid/sign-in");
  await sheet.getByRole("textbox", { name: "Address after sign-in" }).fill("http://localhost:1455/auth/callback?code=fixture");
  await sheet.getByRole("button", { name: "Finish" }).click();
  const models = app.getByRole("dialog", { name: "Models" });
  await expect(models.locator(".ghd", { hasText: "claude-max" })).toContainText("Signed in", { timeout: 30000 });
  expect(world.read("gateway-pasted.txt")).toBe("http://localhost:1455/auth/callback?code=fixture");
  expect(world.read("gateway-argv.txt")).toMatch(/subscriptions connect anthropic .*--no-browser/);
});

test("an API key reaches the gateway on its stdin, and never the page", async ({ app, world }) => {
  await newSession(app, "harbor");
  await app.locator(".toolbar .pillbtn").first().click();
  await app.getByRole("dialog", { name: "Models" }).getByRole("button", { name: "Add an API key" }).click();
  const sheet = app.getByRole("dialog", { name: "Add an API key" });
  await sheet.getByRole("button", { name: "OpenAI" }).click();
  await sheet.getByLabel("API key").fill(KEY);
  expect(await app.content()).not.toContain(KEY);
  await sheet.getByRole("button", { name: "Save the key" }).click();
  await expect(app.getByRole("dialog", { name: "Models" })).toBeVisible();
  await expect.poll(() => world.handed()).toContain(KEY);
  expect(world.read("gateway-argv.txt")).not.toContain(KEY);
  expect(await app.content()).not.toContain(KEY);
});

test("the sandbox sheet sets a session's level and saves it; Full access asks first", async ({ app, world }) => {
  await newSession(app, "harbor");
  await app.locator(".toolbar .pillbtn").nth(1).click();
  const sheet = app.getByRole("dialog", { name: "Sandbox" });
  await sheet.getByRole("button", { name: /^Sandboxed/ }).click();
  await expect(app.locator(".toolbar .pillbtn").nth(1)).toHaveText(/Sandboxed/);
  await expect.poll(async () => (await world.host({ do: "settings" })).values["sandbox.level"]).toBe("sandboxed");
  await sheet.getByRole("button", { name: /^Full access/ }).click();
  await expect(sheet.locator(".confirm")).toContainText("nothing asks");
  await sheet.getByRole("button", { name: "Choose Full access" }).click();
  await expect(app.locator(".toolbar .pillbtn.warn")).toContainText("Full access");
});

test("with no session open, the sandbox level and the model are saved for the sessions to come", async ({ app, world }) => {
  await app.getByRole("dialog", { name: "Choose a folder" }).getByRole("button", { name: "Cancel" }).click({ timeout: 30000 });
  await app.getByRole("button", { name: "Settings" }).click();
  const settings = app.getByRole("dialog", { name: "Settings" });
  await settings.getByRole("button", { name: "Sessions" }).click();
  await settings.getByRole("button", { name: /^Sandbox level/ }).click();
  const sheet = app.getByRole("dialog", { name: "Sandbox" });
  await sheet.getByRole("button", { name: /^Full access/ }).click();
  await sheet.getByRole("button", { name: "Choose Full access" }).click();
  await expect.poll(async () => (await world.host({ do: "settings" })).values["sandbox.level"]).toBe("full");
  await sheet.getByRole("button", { name: "Done" }).click();
  await app.getByRole("button", { name: "Settings" }).click();
  await app.getByRole("dialog", { name: "Settings" }).getByRole("button", { name: "Sessions" }).click();
  await app.getByRole("dialog", { name: "Settings" }).getByRole("button", { name: /^Model/ }).click();
  await app.getByRole("dialog", { name: "Models" }).locator(".mrow").filter({ has: app.locator(".nm", { hasText: /^fixture-two$/ }) }).click();
  await expect.poll(async () => (await world.host({ do: "settings" })).values["model.parent"]).toBe("fixture-two");
});

test("settings save at once and come back: the theme is the person's, dark is the app's own", async ({ app, world, page }) => {
  await app.getByRole("dialog", { name: "Choose a folder" }).getByRole("button", { name: "Cancel" }).click({ timeout: 30000 });
  await app.getByRole("button", { name: "Settings" }).click();
  const sheet = app.getByRole("dialog", { name: "Settings" });
  await sheet.getByRole("button", { name: "Dark" }).click();
  await expect(page.locator("body")).toHaveClass(/dark/);
  await sheet.getByRole("button", { name: "Hyacinth Macaw" }).click();
  await sheet.getByRole("button", { name: "Window" }).click();
  await expect(sheet.getByRole("switch", { name: /The bird/ })).toContainText("The theme's bird, the Hyacinth Macaw, at the top of the card");
  await sheet.getByRole("button", { name: "Appearance" }).click();
  await expect.poll(() => page.evaluate(() => document.body.style.getPropertyValue("--accent"))).toBe("#5b7cf0");
  await expect.poll(async () => (await world.host({ do: "settings" })).values["ui.theme"]).toBe("hyacinth");
  await expect.poll(async () => (await world.host({ do: "preferences" })).appearance).toBe("dark");
  await page.reload();
  await expect(page.getByRole("dialog", { name: "Choose a folder" })).toBeVisible({ timeout: 30000 });
  await expect(page.locator("body")).toHaveClass(/dark/);
  await expect.poll(() => page.evaluate(() => document.body.style.getPropertyValue("--accent"))).toBe("#5b7cf0");
  // On a light ground the same theme's accent is darkened until it reads.
  await page.evaluate(() => { const a = window.__sterna.app; a.prefs.appearance = "light"; a.changed(); });
  await expect.poll(() => page.evaluate(() => document.body.style.getPropertyValue("--accent"))).not.toBe("#5b7cf0");
});

test("a copy the engine does not move itself says why in Settings, with the releases page", async ({ app }) => {
  await app.getByRole("dialog", { name: "Choose a folder" }).getByRole("button", { name: "Cancel" }).click({ timeout: 30000 });
  // A build tree's engine answers the check with `updates: false` and its reason; nothing is said in the sidebar.
  await app.getByRole("button", { name: "Settings" }).click();
  const sheet = app.getByRole("dialog", { name: "Settings" });
  await sheet.getByRole("button", { name: "Updates" }).click();
  const why = sheet.locator(".relrow").nth(1);
  await expect(why).toContainText("This copy of Sterna was not installed with the install line, so it does not update itself.", { timeout: 30000 });
  await expect(app.locator(".sbfoot")).toBeEmpty();
  // The releases page opens in the browser; here the page is answered locally.
  await app.context().route("https://github.com/**", (route) => route.fulfill({ body: "the releases page" }));
  const [opened] = await Promise.all([app.waitForEvent("popup"), why.getByRole("button", { name: "Releases page" }).click()]);
  expect(opened.url()).toBe("https://github.com/HarzerHeribert/sterna/releases");
});
