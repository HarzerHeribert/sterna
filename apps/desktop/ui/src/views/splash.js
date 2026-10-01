// The start: the traced tern flies over a running sea while the host and
// its model gateway start, and says plainly what went wrong if they don't.
import { esc } from "../util.js";
import { ternMark } from "../birds.js";

export const SEA = "▁▁▁▂▁▁▁▁▁ ▁▁▁▁▂▂▁▁▁▁▁▁  ▁▁▁▁▂▁▁▁▁▁▁▁▁ ▁▁▁▂▂▁▁▁▁▁▁▁▁▁▁  ▁▁▂▁▁▁▁▁▁ ▁▁▁▁▁▁▂▁▁▁▁▁";
export const FRAME = { full: 120, calm: 360, off: 1000 };

export function splashHTML(app) {
  const lights = `<div class="lights${app.bridge?.os === "macos" ? " space" : ""}" aria-hidden="true"><i></i><i></i><i></i></div>`;
  if (app.S.phase === "failed") {
    return `<div class="splash" id="splashbox" data-tauri-drag-region>${lights}<div id="tern">${ternMark(app.light, 6)}</div>` +
      `<div class="why"><b>Sterna could not start its engine.</b><pre>${esc(app.S.startError)}</pre>` +
      `<button class="btn primary" data-act="retry">Try again</button></div></div>`;
  }
  return `<div class="splash" id="splashbox" data-tauri-drag-region>${lights}<div id="tern">${ternMark(app.light, 6)}</div>` +
    `<div class="sea" id="sea"></div><div class="said" id="said">Starting the model gateway</div></div>`;
}

/** One frame of the flight: the mark rises and falls a pixel every four frames, and the sea runs under it. */
export function flight(app, now) {
  const motion = app.prefs.motion, el = now - app.S.startAt;
  const frame = motion === "off" ? 0 : Math.floor(el / FRAME[motion]);
  const tern = document.getElementById("tern"), sea = document.getElementById("sea"), said = document.getElementById("said");
  if (tern) tern.style.transform = `translateY(${(Math.floor(frame / 4) % 2) * 6}px)`;
  if (sea) sea.textContent = Array.from({ length: 64 }, (_, i) => SEA[(frame + i) % SEA.length]).join("");
  if (said) said.textContent = el < 3000 ? "Starting the model gateway" : `Starting the model gateway · ${Math.floor(el / 1000)} s`;
}
