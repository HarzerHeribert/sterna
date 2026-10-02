// What only the mock can hold still: several sessions at work at once, each
// in a state the real engine passes through in a moment -- a cell being
// written, a cell running -- so the sidebar's live words and the overview's
// rows can be read. Everything else is checked on the real engine.
import { test, expect, newSession, say } from "./fixtures.mjs";

test.use({ scenario: "busy", pace: 1 });

test("sessions at work show how each stands, in the list and in the overview", async ({ app }) => {
  await app.getByRole("dialog", { name: "Choose a folder" }).getByRole("button", { name: "Cancel" }).click();
  const row = (title) => app.locator(".sitem").filter({ hasText: title });
  await expect(row("Document the CSV export").locator(".sg.run")).toHaveCount(1);
  await expect(row("Document the CSV export")).toHaveAttribute("title", /\nRunning cell 2, for \d+:\d\d$/);
  await expect(row("Add rate limiting")).toHaveAttribute("title", /\nWriting cell 5/);
  await expect(row("Fix the broken links").locator(".sg.wait")).toHaveCount(1);
  await expect(row("Fix the broken links")).toHaveAttribute("title", /\nWaiting for you/);
  await expect(row("Add CSV export to report").locator(".sg.unread")).toHaveCount(1);
  await expect(row("Why does cli start slowly?").locator(".sg i")).toHaveCount(0);
  await expect(row("Why does cli start slowly?")).toHaveAttribute("title", /\nLast used 1 day ago/);
  await app.getByRole("button", { name: /^Overview/ }).click();
  await expect(app.locator(".ovt")).toHaveText("2 sessions are working. 1 is waiting for you.");
  await expect(app.locator(".ov .ghd")).toHaveText(["Needs you", "Running", "Finished, not read yet", "Today"]);
  const running = app.locator(".ovrow", { hasText: "Document the CSV export" });
  await expect(running.locator(".s1")).toHaveText("Running cell 2");
  await expect(running.locator(".sf")).toHaveText("quill");
  // A finished row's meta is the answer's reading parts: counts muted, the change as a diff stat.
  const done = app.locator(".ovrow", { hasText: "Add CSV export to report" });
  await expect(done.locator(".s1 span")).toHaveText(["1 file", "+1", "−0", "1 call"]);
  await expect(done.locator(".s1 .ds.add")).toHaveText("+1");
  await expect(done.locator(".s1 .ds.del")).toHaveText("−0");
  await expect(done.locator(".s2")).toHaveText("just now");
});

// Per folder the five newest are listed, and any other that runs or waits; the rest wait behind "Show all".
test("a folder shows its five newest sessions, and Show all opens the rest in place", async ({ app }) => {
  await app.getByRole("dialog", { name: "Choose a folder" }).getByRole("button", { name: "Cancel" }).click();
  const quill = app.locator(".fgroup", { has: app.locator(".fn", { hasText: /^quill$/ }) });
  await expect(quill.locator(".sitem:not(.more)")).toHaveCount(5);
  const more = quill.locator(".sitem.more");
  await expect(more).toHaveText("Show all (7)");
  await more.click();
  await expect(quill.locator(".sitem:not(.more)")).toHaveCount(7);
  await expect(more).toHaveText("Show fewer");
  await more.click();
  await expect(quill.locator(".sitem:not(.more)")).toHaveCount(5);
});

// One grid: in the overview every row has its glyph, its title and its meta
// in one column each, whatever the folder; in the list every title starts at
// the nav labels' x and every glyph at the icons'.
for (const width of [1480, 980]) {
  test.describe(`at ${width} px`, () => {
    test.use({ viewport: { width, height: 940 } });
    test("every session row lines up, in the overview and in the list", async ({ app }) => {
      await app.getByRole("dialog", { name: "Choose a folder" }).getByRole("button", { name: "Cancel" }).click();
      await app.getByRole("button", { name: /^Overview/ }).click();
      await expect(app.locator(".ov .ovrow")).toHaveCount(3);
      const box = (sel, side) => app.locator(sel).evaluateAll((els, k) => els.map((e) => Math.round(e.getBoundingClientRect()[k])), side);
      const one = (xs) => [...new Set(xs)];
      const titles = await box(".ov .st", "left");
      expect(titles.length).toBe(4);
      expect(one(titles), "titles in every section start at one x").toHaveLength(1);
      expect(one(await box(".ov .sf", "left")), "folder names under them").toEqual(one(titles));
      expect(one(await box(".ov .srow .sg", "left")), "glyphs in one column").toHaveLength(1);
      expect(one(await box(".ov .srow .sm", "right")), "the meta column ends at one x").toHaveLength(1);
      expect(one(await box(".ov .srow .sm", "left")), "and is one width").toHaveLength(1);
      expect(one(await box(".ovbody .ask .h, .ovbody .ask .row2", "left")), "the waiting card's body is indented to the titles").toEqual(one(titles));
      expect(one([...await box(".ov .ghd", "left"), ...await box(".ov .group, .ov .ovcard", "left")]), "section labels sit at the cards' edge").toHaveLength(1);
      const lines = await app.locator(".ovt").evaluate((e) => Math.round(e.getBoundingClientRect().height / parseFloat(getComputedStyle(e).lineHeight)));
      expect(lines, "the heading is one line").toBe(1);
      const row = await app.locator(".ovbody .ask .row2").evaluate((r) => [...r.querySelectorAll("button")].map((b) => { const q = b.getBoundingClientRect(); return Math.round(q.top + q.height / 2); }));
      expect(one(row), "the buttons and Open the session are one row").toHaveLength(1);
      await expect(app.locator(".ov .progress, .ov .fchip")).toHaveCount(0);
      // The list: icons, glyphs and folder names at one x; labels and titles at the next.
      expect(one([...await box(".sbnav .navrow > .i", "left"), ...await box(".sidebar .sitem .sg", "left"), ...await box(".sidebar .fn", "left")])).toHaveLength(1);
      expect(one([...await box(".sbnav .navrow:not(.search) > span:not(.navcount)", "left"), ...await box(".sidebar .sitem .t", "left"), ...await box(".sbnav .navrow.search input", "left")])).toHaveLength(1);
      expect(one(await app.locator(".sidebar .sitem, .sbnav .navrow").evaluateAll((els) => els.map((e) => Math.round(e.getBoundingClientRect().height)))), "every row is one 32 px line").toEqual([32]);
      await expect(app.locator(".sidebar .fhead .fc, .sidebar .fhead .fp")).toHaveCount(0);
    });
  });
}

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

// The app writes no middle dot of its own anywhere: not in the list, the
// overview, a session in any state, Settings or a sheet. The engine's own
// separators (a reading part of tone `line`) are spaced instead.
async function noDots(app, where) {
  const text = await app.evaluate(() => document.body.innerText);
  expect(text.split("\n").filter((line) => line.includes("·")), `${where}: no middle dot`).toEqual([]);
}

test("no visible text has a middle dot: the list, the overview, every session, Settings and the sheets", async ({ app }) => {
  await app.getByRole("dialog", { name: "Choose a folder" }).getByRole("button", { name: "Cancel" }).click();
  await noDots(app, "the list");
  await app.getByRole("button", { name: /^Overview/ }).click();
  await noDots(app, "the overview");
  for (const more of await app.locator(".sitem.more").all()) await more.click();
  const titles = await app.locator(".sidebar .sitem:not(.more) .t").allTextContents();
  expect(titles.length).toBeGreaterThan(10);
  for (const title of titles) {
    await app.locator(".sidebar .sitem", { hasText: title }).first().click();
    await app.waitForTimeout(250);
    await noDots(app, title);
  }
  await app.getByRole("button", { name: "Settings" }).click();
  const settings = app.getByRole("dialog", { name: "Settings" });
  for (const section of await settings.locator(".setnav button").allTextContents()) {
    await settings.getByRole("button", { name: section, exact: true }).click();
    await noDots(app, `Settings, ${section}`);
  }
  await settings.getByRole("button", { name: "Done" }).click();
  await app.getByRole("button", { name: "Help" }).click();
  await noDots(app, "Help");
  await app.getByRole("dialog", { name: "Help" }).getByRole("button", { name: "Done" }).click();
  await app.locator(".toolbar .pillbtn").first().click();
  await app.getByRole("dialog", { name: "Models" }).locator(".mrow").first().waitFor();
  await noDots(app, "Models");
  await app.getByRole("dialog", { name: "Models" }).getByRole("button", { name: "Done" }).click();
  await app.locator(".toolbar .pillbtn").nth(1).click();
  await noDots(app, "Sandbox");
});

test.describe("through a whole turn", () => {
  test.use({ scenario: "empty", pace: 0.5 });
  test("no visible text has a middle dot while it reasons, writes, runs, waits and answers", async ({ app }) => {
    await newSession(app);
    await noDots(app, "a new session");
    await say(app, "Fix the failing test in crates/tally. Keep the change small.");
    const seen = new Set();
    for (const deadline = Date.now() + 45000; Date.now() < deadline;) {
      const text = await app.evaluate(() => document.body.innerText);
      for (const line of text.split("\n")) if (line.includes("·")) seen.add(line);
      const allow = app.locator(".ask").getByRole("button", { name: "Allow for this session" });
      if (await allow.count()) await allow.first().click();
      if (await app.locator(".answer").count()) break;
      await app.waitForTimeout(120);
    }
    await expect(app.locator(".answer")).toBeVisible();
    await noDots(app, "the answer");
    expect([...seen]).toEqual([]);
  });
});
