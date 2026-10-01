// End-to-end checks of the UI in Chromium against the development bridge:
// each test starts its own bridge with a mock host (dev/bridge.mjs --mock),
// so no test shares a host, a list or a session with another.
import { defineConfig, devices } from "@playwright/test";

export default defineConfig({
  testDir: "e2e",
  timeout: 60000,
  expect: { timeout: 10000 },
  fullyParallel: true,
  workers: process.env.CI ? 2 : 4,
  reporter: [["list"]],
  use: {
    ...devices["Desktop Chrome"],
    baseURL: "http://127.0.0.1:5199",
    viewport: { width: 1480, height: 940 },
    trace: "retain-on-failure",
  },
  webServer: {
    command: "npm run dev",
    url: "http://127.0.0.1:5199",
    reuseExistingServer: !process.env.CI,
    timeout: 60000,
  },
});
