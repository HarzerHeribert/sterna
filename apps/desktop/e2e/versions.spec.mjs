// Plan goal 17, in the window: a newer release is found when the app opens,
// moved to beside the running one, and opened by Restart with every session
// left running. The engine's own checks place a release from real archives
// (crates/sterna/tests/release_channel.rs, desktop_release.rs); here the mock
// host answers the host's `update` command with each answer docs/engine.md
// lists, and the browser bridge counts the restart the app would make. A
// copy that does not move itself is checked on the real engine (sheets.spec).
import { test, expect, hook } from "./fixtures.mjs";

const card = (app) => app.locator(".sbfoot .relcard");
const pastTheFolderChoice = (app) => app.getByRole("dialog", { name: "Choose a folder" }).getByRole("button", { name: "Cancel" }).click();
const releases = (mode) => ({ bridgeArgs: ["--mock", "--scenario", "busy", "--pace", "0.5", "--releases", mode] });

async function updatesPane(app) {
  await app.getByRole("button", { name: "Settings" }).click();
  const sheet = app.getByRole("dialog", { name: "Settings" });
  await sheet.getByRole("button", { name: "Updates" }).click();
  return sheet;
}

test.describe("with a newer release out", () => {
  test.use(releases("newer"));
  test("it is offered at the foot of the sidebar, moved to, and Restart opens it with every session left running", async ({ app }) => {
    await pastTheFolderChoice(app);
    await expect(card(app)).toContainText("Sterna 0.1.0-pre.31 is available");
    await expect(card(app)).toContainText("This copy is 0.1.0-pre.30.");
    await card(app).getByRole("button", { name: "Update" }).click();
    await expect(card(app)).toContainText("Updating to Sterna 0.1.0-pre.31…");
    await expect(card(app).getByRole("button", { name: "Updating…" })).toBeDisabled();
    await expect(card(app)).toContainText("Sterna 0.1.0-pre.31 is installed", { timeout: 15000 });
    expect(await hook(app, "test_releases")).toEqual({ moves: 1 });
    await card(app).getByRole("button", { name: "Restart" }).click();
    await expect(app.getByText("Sterna is opening again.")).toBeVisible();
    expect(await hook(app, "test_app_restarts")).toBe(1);
    expect(await hook(app, "test_host_quits"), "the host was told to keep every session running").toEqual([true]);
    expect(await hook(app, "test_app_quits")).toBe(0);
  });
});

test.describe("with a move that fails", () => {
  test.use(releases("fails"));
  test("the card says what went wrong in plain words, and Try again moves", async ({ app }) => {
    await pastTheFolderChoice(app);
    await card(app).getByRole("button", { name: "Update" }).click();
    await expect(card(app)).toContainText("Sterna 0.1.0-pre.31 was not installed", { timeout: 15000 });
    await expect(card(app)).toContainText("The engine said: The archive's checksum does not match.");
    await card(app).getByRole("button", { name: "Try again" }).click();
    await expect(card(app)).toContainText("Sterna 0.1.0-pre.31 is installed", { timeout: 15000 });
    expect(await hook(app, "test_releases")).toEqual({ moves: 2 });
  });
});

test.describe("with nothing new", () => {
  test.use(releases("current"));
  test("nothing about releases is said; Settings says this is the newest", async ({ app }) => {
    await pastTheFolderChoice(app);
    const sheet = await updatesPane(app);
    await expect(sheet.locator(".relrow")).toHaveText(["InstalledSterna 0.1.0-pre.30Check now", "This is the newest release"]);
    await sheet.getByRole("button", { name: "Done" }).click();
    await expect(app.locator(".sbfoot")).toBeEmpty();
  });
});

test.describe("with automatic checks off", () => {
  test.use(releases("manual"));
  test("the newer release waits for Check now", async ({ app }) => {
    await pastTheFolderChoice(app);
    const sheet = await updatesPane(app);
    await expect(sheet).toContainText("Automatic checks are off (STERNA_DISABLE_AUTOUPDATE is set)");
    await expect(app.locator(".sbfoot")).toBeEmpty();
    await sheet.getByRole("button", { name: "Check now" }).click();
    await expect(card(app)).toContainText("Sterna 0.1.0-pre.31 is available");
  });
});

test.describe("with GitHub out of reach", () => {
  test.use(releases("unreachable"));
  test("the check on open stays quiet; Settings says why, and Try again checks again", async ({ app }) => {
    await pastTheFolderChoice(app);
    const sheet = await updatesPane(app);
    const found = sheet.locator(".relrow").nth(1);
    await expect(found).toContainText("The check did not finish");
    await expect(found).toContainText("GitHub could not be reached. Check the connection and try again.");
    await expect(app.locator(".sbfoot")).toBeEmpty();
    await hook(app, "test_releases", { mode: "newer" });
    await found.getByRole("button", { name: "Try again" }).click();
    await expect(found).toContainText("Sterna 0.1.0-pre.31 is available");
    await expect(card(app)).toContainText("Sterna 0.1.0-pre.31 is available");
  });
});
