// Plan goal 15, on the real engine: every folder and session in the
// sidebar, sorted by last use, with how each stands, and an overview of
// them all. The sessions are started on the host the way another client
// would; the window finds them in the host's list.
import { test, expect, newSession, listed } from "./world.mjs";

/** Three sessions in three folders, used in this order: alpha answered, beta waiting for you, gamma answered. */
async function threeSessions(world) {
  const [, alpha, beta, gamma] = world.projects;
  await world.host({ do: "start", root: alpha, task: "Answer at once, please." });
  await listed(world, "Answer at once", (s) => s.live?.state === "idle");
  await world.host({ do: "start", root: beta, task: "Write note.txt in this folder." });
  await listed(world, "Write note.txt", (s) => s.live?.state === "waiting");
  await world.host({ do: "start", root: gamma, task: "Answer at once, from gamma." });
  await listed(world, "Answer at once, from gamma", (s) => s.live?.state === "idle");
}

async function pastTheFolderChoice(app) {
  const sheet = app.getByRole("dialog", { name: "Choose a folder" });
  await expect(sheet).toBeVisible({ timeout: 30000 });
  await sheet.getByRole("button", { name: "Cancel" }).click();
  await expect(sheet).toHaveCount(0);
}

test("every folder is listed, the one used last first, with how each session stands", async ({ page, world }) => {
  await threeSessions(world);
  await page.goto(`/?bridge=${encodeURIComponent(world.bridge.url)}`);
  await pastTheFolderChoice(page);
  await expect(page.locator(".sidebar .fn")).toHaveText(["gamma", "beta", "alpha"]);
  await expect(page.locator(".sitem", { hasText: "Write note.txt" }).locator(".m")).toHaveText("Waiting for you");
  await expect(page.locator(".fgroup", { hasText: "beta" }).locator(".fc")).toHaveText("1 needs you");
  await expect(page.locator(".navrow .badge")).toHaveText("1 needs you");
  await expect(page.locator("#status")).toContainText("1 other session needs you");
});

test("the overview says what it counts and answers a waiting session in place", async ({ page, world }) => {
  await threeSessions(world);
  await page.goto(`/?bridge=${encodeURIComponent(world.bridge.url)}`);
  await pastTheFolderChoice(page);
  await page.getByRole("button", { name: /^Overview/ }).click();
  await expect(page.locator(".ovt")).toHaveText("1 session waits for you");
  const card = page.locator(".ovcard", { hasText: "Write note.txt" });
  await expect(card.locator(".ask")).toContainText("write note.txt");
  await card.getByRole("button", { name: "Allow once" }).click();
  await expect(page.locator(".ovcard")).toHaveCount(0);
  await listed(world, "Write note.txt", (s) => s.live?.state === "idle");
  expect(world.read("beta/note.txt")).toBe("written by the desktop check");
});

test("opening a running session shows its record and the cell that waits", async ({ page, world }) => {
  await threeSessions(world);
  await page.goto(`/?bridge=${encodeURIComponent(world.bridge.url)}`);
  await pastTheFolderChoice(page);
  await page.locator(".sitem", { hasText: "Write note.txt" }).click();
  await expect(page.locator(".toolbar .project")).toHaveText("beta");
  await expect(page.locator(".you .bubble")).toHaveText("Write note.txt in this folder.");
  await expect(page.locator("#cell-1 .ask")).toContainText("write note.txt");
});

test("a session that finishes while you look at another is marked until you open it", async ({ page, world }) => {
  await threeSessions(world);
  await page.goto(`/?bridge=${encodeURIComponent(world.bridge.url)}`);
  await pastTheFolderChoice(page);
  await page.locator(".sitem", { hasText: "Answer at once, please." }).click();
  await expect(page.locator(".toolbar .project")).toHaveText("alpha");
  await world.host({ do: "start", root: world.harbor, task: "Take your time with harbor." });
  const row = page.locator(".sitem", { hasText: "Take your time with harbor." });
  await expect(row.locator(".m")).toContainText("Thinking", { timeout: 30000 });
  await expect(row.locator(".m")).toHaveText("Finished · not read yet", { timeout: 30000 });
  await row.click();
  await expect(page.locator(".answer .first")).toHaveText("Done, in my own time.");
  await expect(row.locator(".m")).not.toContainText("not read yet");
});

test("a session that no longer runs opens again under its own id, with its record", async ({ page, world }) => {
  const [, alpha] = world.projects;
  const started = await world.host({ do: "start", root: alpha, task: "Answer at once, then rest." });
  await listed(world, "Answer at once, then rest", (s) => s.live?.state === "idle");
  await world.host({ do: "stop", id: started.id });
  await listed(world, "Answer at once, then rest", (s) => s.live === null);
  await page.goto(`/?bridge=${encodeURIComponent(world.bridge.url)}`);
  await pastTheFolderChoice(page);
  const row = page.locator(".sitem", { hasText: "Answer at once, then rest." });
  await expect(row.locator(".m")).not.toContainText("Thinking");
  await row.click();
  await expect(page.locator(".you .bubble")).toHaveText("Answer at once, then rest.", { timeout: 30000 });
  await expect(page.locator(".answer .first")).toHaveText("Answered at once.");
  await expect(page.locator(".inspector")).toContainText(started.id);
  // The host lists the reopened session as running, under the same id.
  const again = await listed(world, "Answer at once, then rest", (s) => s.live !== null);
  expect(again.id).toBe(started.id);
});

test("a new session runs beside the others, in a folder of its own", async ({ page, world }) => {
  await threeSessions(world);
  await page.goto(`/?bridge=${encodeURIComponent(world.bridge.url)}`);
  await newSession(page, "harbor");
  await expect(page.locator(".sidebar .fn").first()).toHaveText("harbor");
  await expect(page.locator(".navrow .badge")).toHaveText("1 needs you");
});
