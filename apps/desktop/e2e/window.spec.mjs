// The window holds its shape whatever it is given: the side panels hidden
// or folded by a narrow window, long names, words with no space in them, a
// button pressed twice. On the mock host, which holds its sessions still.
import { test, expect, say } from "./fixtures.mjs";

test.use({ scenario: "busy", pace: 1 });

const pastTheFolderChoice = (app) => app.getByRole("dialog", { name: "Choose a folder" }).getByRole("button", { name: "Cancel" }).click();

/** Whether the page or the conversation scrolls sideways, and the toolbar's buttons past its right edge. */
const fits = (app) => app.evaluate(() => {
  const tb = document.querySelector("#toolbar").getBoundingClientRect();
  return {
    page: document.documentElement.scrollWidth - innerWidth,
    convo: document.querySelector("#scroll").scrollWidth - document.querySelector("#scroll").clientWidth,
    outside: [...document.querySelectorAll("#toolbar > *")].filter((e) => e.getBoundingClientRect().right > tb.right + 0.5).map((e) => e.className),
  };
});

test.describe("at the width of a laptop's window", () => {
  test.use({ viewport: { width: 1000, height: 800 } });

  // The sidebar hidden once left the content in the sidebar's empty column,
  // under a box that took every click.
  test("with the sessions list hidden, the content keeps its place and every button answers", async ({ app }) => {
    await pastTheFolderChoice(app);
    await app.getByRole("button", { name: "Hide the sessions" }).click();
    await expect(app.locator(".sidebar")).toBeHidden();
    const width = await app.locator(".content").evaluate((e) => e.getBoundingClientRect().width);
    expect(width, "the content takes the whole window").toBe(1000);
    await app.getByRole("button", { name: "Settings" }).click();
    await expect(app.getByRole("dialog", { name: "Settings" })).toBeVisible();
    await app.keyboard.press("Escape");
    await app.getByRole("button", { name: "Show the sessions" }).click();
    await app.locator(".sitem", { hasText: "Document the CSV export" }).click();
    await expect(app.locator(".toolbar .project")).toHaveText("quill");
    // Each step waits for the toolbar it changed: hiding the list moves the toolbar's buttons.
    await app.getByRole("button", { name: "Hide the sessions" }).click();
    await expect(app.getByRole("button", { name: "Show the sessions" })).toBeVisible();
    await app.getByRole("button", { name: "Show the session card" }).click();
    await expect(app.locator(".inspector")).toBeVisible();
    await app.locator(".toolbar [data-act=level]").click();
    await expect(app.getByRole("dialog", { name: "Sandbox" })).toBeVisible();
    expect(await fits(app)).toEqual({ page: 0, convo: 0, outside: [] });
  });
});

// Folding is the window's: the person's choice of panels is what comes back,
// and what is saved, however often the window is made narrow.
test("a narrow window folds the side panels without changing what the person chose", async ({ app }) => {
  await pastTheFolderChoice(app);
  await app.locator(".sitem", { hasText: "Document the CSV export" }).click();
  await expect(app.locator(".inspector")).toBeVisible();
  await app.setViewportSize({ width: 800, height: 700 });
  await expect(app.locator(".sidebar")).toBeHidden();
  await expect(app.locator(".inspector")).toBeHidden();
  // Another choice saves the app's preferences whole, the folded panels among them.
  await app.getByRole("button", { name: "Settings" }).click();
  const settings = app.getByRole("dialog", { name: "Settings" });
  await settings.getByRole("button", { name: "Window" }).click();
  await expect(settings.getByRole("switch", { name: /Sessions list/ })).toHaveAttribute("aria-checked", "false");
  await settings.getByRole("button", { name: "Cells" }).click();
  await settings.getByRole("switch", { name: /Reasoning summary/ }).click();
  await app.keyboard.press("Escape");
  await expect.poll(() => app.evaluate(() => window.__sterna.app.engine.preferences()), { message: "saved as chosen, not as folded" })
    .toMatchObject({ summary: false, sessions: true, card: true });
  await app.setViewportSize({ width: 1480, height: 940 });
  await expect(app.locator(".sidebar")).toBeVisible();
  await expect(app.locator(".inspector")).toBeVisible();
  // Made narrow, a panel the person brings back is shown, and that is their choice;
  // too narrow for both, the other one folds away meanwhile.
  await app.setViewportSize({ width: 800, height: 700 });
  await app.getByRole("button", { name: "Show the sessions" }).click();
  await expect(app.locator(".sidebar")).toBeVisible();
  await app.getByRole("button", { name: "Show the session card" }).click();
  await expect(app.locator(".inspector")).toBeVisible();
  await expect(app.locator(".sidebar")).toBeHidden();
  expect(await app.locator(".centre").evaluate((e) => e.getBoundingClientRect().width), "the conversation keeps a reading width").toBeGreaterThan(480);
  expect((await fits(app)).page).toBe(0);
  await expect.poll(() => app.evaluate(() => window.__sterna.app.engine.preferences())).toMatchObject({ sessions: true, card: true });
});

test.describe("at the smallest window", () => {
  test.use({ viewport: { width: 760, height: 560 } });

  test("a long model name gives way before the toolbar's buttons do, and a long word wraps", async ({ app }) => {
    await pastTheFolderChoice(app);
    await app.getByRole("button", { name: "Show the sessions" }).click();
    await app.locator(".sitem", { hasText: "Why does cli start slowly?" }).click();
    await app.getByRole("button", { name: "Hide the sessions" }).click();
    const model = "openrouter/anthropic/claude-opus-5-5-20261001-with-a-very-long-suffix";
    await say(app, `/model ${model}`);
    await expect(app.locator(".pillbtn.model")).toHaveAttribute("title", new RegExp(`^${model}`));
    const path = `/Users/someone/${"deeply/".repeat(12)}nested/file-with-no-break-${"x".repeat(80)}.rs`;
    await say(app, `Please read ${path} and https://example.com/${"a".repeat(120)}`);
    await expect(app.locator(".you .bubble").last()).toContainText("file-with-no-break");
    await app.getByRole("button", { name: "Show the session card" }).click();
    await expect(app.locator(".inspector")).toBeVisible();
    expect(await fits(app)).toEqual({ page: 0, convo: 0, outside: [] });
    // The hint in an empty composer is shown whole, on as many lines as it takes.
    const hint = await app.locator("#draft").evaluate((ta) => ({ height: ta.clientHeight, lines: Math.round(ta.clientHeight / parseFloat(getComputedStyle(ta).lineHeight)) }));
    expect(hint.lines, "the hint is not cut to its first line").toBeGreaterThanOrEqual(2);
  });
});

test.describe("on a first run", () => {
  test.use({ scenario: "empty", pace: 0.5 });

  test("Start session pressed twice starts one session", async ({ app }) => {
    const sheet = app.getByRole("dialog", { name: "Choose a folder" });
    await sheet.getByRole("button", { name: /choose (another|a) folder/i }).click();
    await expect(sheet.locator(".frow[aria-pressed=true]")).toHaveCount(1);
    // Two presses before the window has drawn again.
    await sheet.getByRole("button", { name: "Start session" }).evaluate((b) => { b.click(); b.click(); });
    await expect(app.getByRole("heading", { name: /what should we build\?/i })).toBeVisible({ timeout: 30000 });
    expect(await app.evaluate(() => window.__sterna.app.S.sessions.size)).toBe(1);
    await say(app, "Hello there");
    await expect(app.locator(".sidebar .sitem:not(.more)")).toHaveCount(1);
  });
});

// The clocks tick in rows and pills; the window around them stays put.
test.describe("in a short window, while sessions run", () => {
  test.use({ viewport: { width: 1100, height: 600 } });

  test("a ticking clock leaves the list's scroll and a question's focus where they are", async ({ app }) => {
    await pastTheFolderChoice(app);
    const list = app.locator("#slist");
    await list.evaluate((e) => { e.scrollTop = e.scrollHeight; });
    const bottom = await list.evaluate((e) => e.scrollTop);
    expect(bottom, "the list is long enough to scroll").toBeGreaterThan(0);
    await app.waitForTimeout(2500);
    expect(await list.evaluate((e) => e.scrollTop)).toBe(bottom);
    // The question in a cell, in focus while its "Waiting for you" clock runs: Esc refuses it this once.
    await app.locator(".sitem", { hasText: "Fix the broken links" }).click();
    const ask = app.locator("#record .ask, #live .ask").first();
    await ask.focus();
    await app.waitForTimeout(2500);
    await expect(ask).toBeFocused();
    await app.keyboard.press("Escape");
    await expect(app.locator("#toast")).toContainText("Refused this once");
  });

  test("Esc in the search clears it and stops nothing", async ({ app }) => {
    await pastTheFolderChoice(app);
    await app.locator(".sitem", { hasText: "Document the CSV export" }).click();
    await expect(app.locator("#status")).toContainText("Running cell");
    const search = app.getByRole("textbox", { name: "Search sessions" });
    await search.fill("csv");
    await expect(app.locator(".sidebar .sitem")).toHaveCount(2);
    await search.press("Escape");
    await expect(search).toHaveValue("");
    await expect(search).not.toBeFocused();
    await expect(app.locator(".sidebar .sitem:not(.more)")).not.toHaveCount(2);
    expect(await app.evaluate(() => window.__sterna.app.cur().stopAsked)).toBe(false);
    await expect(app.locator("#status")).not.toContainText("Stop requested");
  });

  test("the overview opens at its top, whatever was on screen before", async ({ app }) => {
    await pastTheFolderChoice(app);
    await app.getByRole("button", { name: /^Overview/ }).click();
    await expect(app.locator(".ovt")).toBeVisible();
    await app.waitForTimeout(600);
    expect(await app.locator("#scroll").evaluate((e) => ({ top: e.scrollTop, scrolls: e.scrollHeight > e.clientHeight }))).toEqual({ top: 0, scrolls: true });
    await expect(app.locator(".ovcard").first()).toBeInViewport();
  });

  test("a session that is not running, opened twice at once, starts once", async ({ app }) => {
    await pastTheFolderChoice(app);
    await app.evaluate(() => {
      const e = window.__sterna.app.engine, start = e.startSession.bind(e);
      e.starts = 0;
      e.startSession = (...a) => { e.starts++; return start(...a); };
    });
    await app.locator(".sitem", { hasText: "Why does cli start slowly?" }).evaluate((b) => { b.click(); b.click(); });
    await expect(app.locator(".toolbar .project")).toHaveText("quill");
    await expect(app.locator(".you .bubble").first()).toBeVisible();
    expect(await app.evaluate(() => window.__sterna.app.engine.starts)).toBe(1);
  });

  test("a long notice wraps inside the window", async ({ app }) => {
    await pastTheFolderChoice(app);
    await app.locator(".sitem", { hasText: "Document the CSV export" }).click();
    await app.evaluate(() => window.__sterna.app.say(`Could not start a session in ${"/very/long/folder".repeat(12)}: the engine said something long about why, and then some more words.`));
    const toast = app.locator("#toast .toast");
    await expect(toast).toBeVisible();
    const box = await toast.boundingBox(), column = await app.locator("#dock .column").boundingBox();
    expect(box.x).toBeGreaterThanOrEqual(column.x);
    expect(box.x + box.width).toBeLessThanOrEqual(column.x + column.width);
  });
});
