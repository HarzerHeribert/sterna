// Draws the app icon from the traced tern (art/birds/tern-mark): the bird
// in flight on the window's dark ground, rendered to a 1024 px PNG that
// `tauri icon` turns into every platform's sizes.
import { chromium } from "@playwright/test";
import { fileURLToPath } from "node:url";
import { bird } from "./birds.mjs";

const out = fileURLToPath(new URL("../src-tauri/icons/source.png", import.meta.url));
const tern = bird("tern-mark");
const hex = (n) => "#" + n.toString(16).padStart(6, "0");
let rects = "";
tern.rows.forEach((row, y) => {
  for (let x = 0; x < row.length;) {
    const c = row[x];
    if (c === "." || tern.pal[c] == null) { x++; continue; }
    let e = x + 1;
    while (e < row.length && row[e] === c) e++;
    rects += `<rect x="${x}" y="${y}" width="${e - x}" height="1.02" fill="${hex(tern.pal[c])}"/>`;
    x = e;
  }
});
const scale = 17, w = tern.w * scale, h = tern.rows.length * scale;
const html = `<html><body style="margin:0;background:transparent">
<svg width="1024" height="1024" viewBox="0 0 1024 1024" xmlns="http://www.w3.org/2000/svg">
  <rect x="100" y="100" width="824" height="824" rx="186" fill="#15191f"/>
  <rect x="100" y="100" width="824" height="824" rx="186" fill="none" stroke="#2a3038" stroke-width="4"/>
  <svg x="${(1024 - w) / 2 + 10}" y="${(1024 - h) / 2 - 10}" width="${w}" height="${h}" viewBox="0 0 ${tern.w} ${tern.rows.length}" shape-rendering="crispEdges">${rects}</svg>
</svg></body></html>`;
const browser = await chromium.launch();
const page = await browser.newPage({ viewport: { width: 1024, height: 1024 } });
await page.setContent(html);
await page.locator("svg").first().screenshot({ path: out, omitBackground: true });
await browser.close();
console.log("drew", out);
