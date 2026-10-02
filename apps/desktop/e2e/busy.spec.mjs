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
  await expect(app.locator(".ovt")).toHaveText("2 sessions at work · 1 waits for you");
  await expect(app.locator(".ovrow", { hasText: "Document the CSV export" })).toContainText("Executing cell 002");
});

// One grid: every session row starts its title at one x, in the overview and
// in the list, whatever the folder's name; nothing runs under its status line.
for (const width of [1480, 980]) {
  test.describe(`at ${width} px`, () => {
    test.use({ viewport: { width, height: 940 } });
    test("every session row lines up, in the overview and in the list", async ({ app }) => {
      await app.getByRole("dialog", { name: "Choose a folder" }).getByRole("button", { name: "Cancel" }).click();
      await app.getByRole("button", { name: /^Overview/ }).click();
      await expect(app.locator(".ov .ovrow")).toHaveCount(3);
      const lefts = (sel) => app.locator(sel).evaluateAll((els) => els.map((e) => Math.round(e.getBoundingClientRect().left)));
      const rights = (sel) => app.locator(sel).evaluateAll((els) => els.map((e) => Math.round(e.getBoundingClientRect().right)));
      const one = (xs) => [...new Set(xs)];
      const titles = await lefts(".ov .st");
      expect(titles.length).toBe(4);
      expect(one(titles), "titles in every section start at one x").toHaveLength(1);
      expect(one(await lefts(".ov .sm")), "meta lines start there too").toEqual(one(titles));
      expect(one(await lefts(".ovbody .ask .h, .ovbody .ask .row2")), "the waiting card's body is indented to the titles").toEqual(one(titles));
      expect(one([...await lefts(".ov .ghd"), ...await lefts(".ov .group, .ov .ovcard")]), "section labels sit at the cards' edge").toHaveLength(1);
      const lines = await app.locator(".ovt").evaluate((e) => Math.round(e.getBoundingClientRect().height / parseFloat(getComputedStyle(e).lineHeight)));
      expect(lines, "the heading is one line").toBe(1);
      const row = await app.locator(".ovbody .ask .row2").evaluate((r) => [...r.querySelectorAll("button")].map((b) => { const r = b.getBoundingClientRect(); return Math.round(r.top + r.height / 2); }));
      expect(one(row), "the buttons and Open the session are one row").toHaveLength(1);
      await expect(app.locator(".ov .progress, .ov .fchip")).toHaveCount(0);
      // The list: rows at the folder name's x, the count pills and New session in one column each.
      expect(one([...await lefts(".sidebar .fn"), ...await lefts(".sidebar .sitem .t"), ...await lefts(".sidebar .sitem .m")])).toHaveLength(1);
      expect(one(await rights(".sidebar .fc"))).toHaveLength(1);
      expect(one(await lefts(".sidebar .fhead .iconbtn"))).toHaveLength(1);
      const under = await app.locator(".sidebar .sitem").evaluateAll((els) => els.filter((e) => e.querySelector(".t").getBoundingClientRect().bottom > e.querySelector(".m").getBoundingClientRect().top + 0.5).length);
      expect(under, "no title runs under its status line").toBe(0);
      const centred = await app.locator(".sidebar .fhead").evaluateAll((heads) => heads.every((h) => {
        const mid = (el) => { const r = el.getBoundingClientRect(); return r.top + r.height / 2; };
        const m = mid(h);
        return [...h.querySelectorAll(".chev, .fn, .fp, .fc, .iconbtn")].every((el) => Math.abs(mid(el) - m) <= 1.5);
      }));
      expect(centred, "a folder header's items sit on one line").toBe(true);
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
