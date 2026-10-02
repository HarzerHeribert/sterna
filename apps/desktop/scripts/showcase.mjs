// The windows the product page's showcase slants and fans out
// (sites/public/showcase/): the app at work, recorded against the mock host,
// each window in a theme of its own and in light or dark. A clip is a short
// H.264 loop with its first frame as its poster; a still is a JPEG at one
// and a half times the pixels, so it stays sharp when the page slants it.
//   npm run showcase [name…] writes sites/public/showcase/<name>.{mp4,jpg}
// Needs ffmpeg with libx264 on PATH.
import { chromium } from "@playwright/test";
import { createServer } from "vite";
import { execFileSync } from "node:child_process";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { startBridge } from "../e2e/fixtures.mjs";
import { tour } from "./tour.mjs";

const ROOT = fileURLToPath(new URL("..", import.meta.url));
const OUT = fileURLToPath(new URL("../../../sites/public/showcase/", import.meta.url));
fs.mkdirSync(OUT, { recursive: true });

// Back to front, as the page stacks them. A clip records from the tour's
// state `from`: for `hold` milliseconds, or until the state `to` and `tail`
// milliseconds more, waiting `pause[state]` at a state on the way so a
// person could read it; `speed` plays it faster than it ran.
const WINDOWS = [
  { name: "running", theme: "arctic-tern", scheme: "dark", from: "running", hold: 6000 },
  { name: "overview", theme: "amazon", scheme: "light", from: "overview", hold: 9000 },
  { name: "session", theme: "hyacinth", scheme: "dark", from: "new", to: "done", tail: 3000, pause: { new: 800, approval: 2600 }, speed: 1.6 },
  { name: "done", theme: "sun-conure", scheme: "light", still: "done" },
];

const vite = await createServer({ configFile: `${ROOT}/vite.config.js`, server: { port: 5298, strictPort: true } });
await vite.listen();
const browser = await chromium.launch();
const wait = (ms) => new Promise((done) => setTimeout(done, ms));

/** Chromium's screencast: a frame each time the screen changes, with its time. */
async function screencast(page) {
  const cdp = await page.context().newCDPSession(page);
  const frames = [];
  cdp.on("Page.screencastFrame", ({ data, metadata, sessionId }) => {
    frames.push({ data: Buffer.from(data, "base64"), at: metadata.timestamp });
    cdp.send("Page.screencastFrameAck", { sessionId }).catch(() => {});
  });
  await cdp.send("Page.startScreencast", { format: "jpeg", quality: 90, maxWidth: 1480, maxHeight: 940 });
  return async () => {
    await cdp.send("Page.stopScreencast");
    frames.push({ data: frames.at(-1).data, at: Date.now() / 1000 });
    return frames;
  };
}

/** The frames as an H.264 loop at 30 frames a second, each shown as long as it was on screen. */
function encode(frames, name, speed = 1) {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), "sterna-showcase-"));
  const list = ["ffconcat version 1.0"];
  frames.forEach((frame, i) => {
    const file = `f${String(i).padStart(5, "0")}.jpg`;
    fs.writeFileSync(path.join(dir, file), frame.data);
    const next = frames[i + 1];
    list.push(`file '${file}'`, `duration ${next ? Math.max(next.at - frame.at, 0.001).toFixed(4) : "0.04"}`);
  });
  fs.writeFileSync(path.join(dir, "list.txt"), list.join("\n") + "\n");
  const mp4 = path.join(OUT, `${name}.mp4`);
  execFileSync("ffmpeg", ["-y", "-loglevel", "error", "-f", "concat", "-safe", "0", "-i", path.join(dir, "list.txt"),
    "-vf", `setpts=PTS/${speed},fps=30,scale=1480:-2:flags=lanczos,format=yuv420p`, "-c:v", "libx264", "-preset", "slow", "-crf", "23",
    "-movflags", "+faststart", "-an", mp4]);
  execFileSync("ffmpeg", ["-y", "-loglevel", "error", "-i", mp4, "-frames:v", "1", "-q:v", "3", path.join(OUT, `${name}.jpg`)]);
  fs.rmSync(dir, { recursive: true, force: true });
}

const only = process.argv.slice(2);
for (const win of WINDOWS.filter((w) => !only.length || only.includes(w.name))) {
  const folder = path.join(os.homedir(), "code", "ledger-app");
  const bridge = await startBridge(["--mock", "--scenario", "busy", "--pace", "2", "--any-folder", "--folders", folder, "--origin", "http://127.0.0.1:5298"]);
  const page = await browser.newPage({ viewport: { width: 1480, height: 940 }, deviceScaleFactor: win.still ? 1.5 : 1, colorScheme: win.scheme });
  await page.addInitScript((s) => localStorage.setItem("sterna.desktop.prefs", JSON.stringify({ appearance: s })), win.scheme);
  const url = `http://127.0.0.1:5298/?bridge=${encodeURIComponent(bridge.url)}`;
  await page.goto(url);
  // The theme is the person's setting, kept by the host: chosen as Settings
  // chooses it, then the window opens again on it.
  await page.getByRole("dialog", { name: "Choose a folder" }).waitFor();
  await page.evaluate((theme) => { const app = window.__sterna.app; app.prefs.theme = theme; app.savePref("theme"); }, win.theme);
  await wait(400);
  await page.goto(url);
  let stop = null;
  await tour(page, async (name) => {
    if (win.still) {
      if (name === win.still) await page.screenshot({ path: path.join(OUT, `${win.name}.jpg`), type: "jpeg", quality: 84 });
      return;
    }
    if (name === win.from && !stop) stop = await screencast(page);
    if (!stop) return;
    if (win.pause?.[name]) await wait(win.pause[name]);
    if ((win.to && name === win.to) || (!win.to && name === win.from)) {
      await wait(win.to ? win.tail : win.hold);
      encode(await stop(), win.name, win.speed);
      stop = null;
    }
  });
  await page.close();
  bridge.child.kill("SIGTERM");
  console.log(`showcase: ${win.name} (${win.theme}, ${win.scheme})`);
}
await browser.close();
await vite.close();
