// The walk through the app that the screenshots and the product page's
// showcase both take, against the mock host: the start, the folder choice,
// a new session, each state of a turn, the setup sheets, the overview and a
// newer release's card. `shot(name)` is called at each of them.
export async function tour(page, shot) {
  await page.locator("#tern").waitFor();
  await page.waitForTimeout(500);
  await shot("start");
  const sheet = page.getByRole("dialog", { name: "Choose a folder" });
  await sheet.waitFor();
  await page.waitForTimeout(400);
  await shot("folder");
  await sheet.getByRole("button", { name: /choose another folder/i }).click();
  await sheet.getByRole("button", { name: "Start session" }).click();
  await page.getByRole("heading", { name: /what should we build/i }).waitFor();
  await shot("new");
  const draft = page.getByRole("textbox", { name: "Message" });
  await draft.fill("Fix the failing test in crates/tally. Keep the change small.");
  await draft.press("Enter");
  await page.locator(".reason").waitFor();
  await page.waitForTimeout(1600);
  await shot("reasoning");
  await page.locator("#cell-writing").waitFor();
  await page.waitForTimeout(1800);
  await shot("writing");
  await page.locator("article.cell.running .pill", { hasText: "Running" }).waitFor();
  await page.waitForTimeout(700);
  await shot("running");
  await page.locator("#cell-3 .ask").waitFor({ timeout: 60000 });
  await page.waitForTimeout(400);
  await shot("approval");
  await page.locator("#cell-3 .ask").getByRole("button", { name: "Allow for this session" }).click();
  await page.locator(".answer").waitFor({ timeout: 60000 });
  await page.waitForTimeout(5500);
  await shot("done");
  await page.locator(".toolbar .pillbtn").first().click();
  await page.getByRole("dialog", { name: "Models" }).locator(".mrow").first().waitFor();
  await page.waitForTimeout(400);
  await shot("models");
  await page.getByRole("dialog", { name: "Models" }).getByRole("button", { name: "Done" }).click();
  await page.locator(".toolbar .pillbtn").nth(1).click();
  await page.getByRole("dialog", { name: "Sandbox" }).waitFor();
  await page.waitForTimeout(400);
  await shot("sandbox");
  await page.getByRole("dialog", { name: "Sandbox" }).getByRole("button", { name: "Done" }).click();
  await page.getByRole("button", { name: "Settings" }).click();
  await page.getByRole("dialog", { name: "Settings" }).waitFor();
  await page.waitForTimeout(400);
  await shot("settings");
  await page.getByRole("dialog", { name: "Settings" }).getByRole("button", { name: "Done" }).click();
  await page.getByRole("button", { name: /^Overview/ }).click();
  await page.waitForTimeout(600);
  await shot("overview");
  // A newer release, found by Check now: the quiet card at the foot of the list.
  await page.evaluate(() => window.__sterna.app.bridge.test("test_releases", { mode: "newer" }));
  await page.getByRole("button", { name: "Settings" }).click();
  await page.getByRole("dialog", { name: "Settings" }).getByRole("button", { name: "Updates" }).click();
  await page.getByRole("dialog", { name: "Settings" }).getByRole("button", { name: "Check now" }).click();
  await page.locator(".sbfoot .relcard").waitFor();
  await page.waitForTimeout(300);
  await shot("settings-releases");
  await page.getByRole("dialog", { name: "Settings" }).getByRole("button", { name: "Done" }).click();
  await page.waitForTimeout(400);
  await shot("release-card");
}
