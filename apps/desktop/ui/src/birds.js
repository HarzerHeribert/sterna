// The bird and its moods, as workbench/plumage.rs edits them: the traced
// sprite from art/birds, a blink, a red eye when something failed, and a
// mark beside it -- never over it -- for thinking, working and done.
import BIRDS from "virtual:birds";
import { readable, hex } from "./theme.js";

const MARKS = { think: 0xc9ced8, work: 0xe8c35a, done: 0x5fd07a, oops: 0xff5a52 };

export function pixels(key, mood, light) {
  const a = BIRDS[key], rows = a.rows;
  const colour = (L) => { const d = a.pal[L]; if (d == null) return null; return light ? readable(a.light[L] ?? d, true, 1.4) : d; };
  const top = rows.findIndex((r) => /[^.]/.test(r));
  const [ex, ey] = a.eye, by = a.tip[1];
  let right = 0;
  for (let y = top; y <= by; y++) right = Math.max(right, rows[y].search(/[^.][.]*$/));
  const width = key === "tern-mark" ? a.w : Math.max(a.w, right + 5);
  const grid = rows.map((r) => Array.from({ length: width }, (_, x) => (r[x] && r[x] !== "." ? colour(r[x]) : null)));
  const markColour = (c) => (light ? readable(c, true, 1.6) : c);
  const mark = (pts, c) => pts.forEach(([x, y]) => { if (grid[y] && x < width && grid[y][x] == null) grid[y][x] = markColour(c); });
  if (mood === "blink") {
    const seen = {};
    [[ex - 1, ey], [ex + 1, ey], [ex, ey - 1], [ex, ey + 1]].forEach(([x, y]) => { const c = grid[y]?.[x]; if (c != null) seen[c] = (seen[c] || 0) + 1; });
    const head = Object.entries(seen).sort((p, q) => q[1] - p[1])[0];
    if (head) grid[ey][ex] = +head[0];
  }
  if (mood === "oops") grid[ey][ex] = MARKS.oops;
  if (mood === "think") mark([[right + 2, top + 2], [right + 3, top + 1], [right + 4, top]], MARKS.think);
  if (mood === "work") mark([[right + 1, by + 2], [right + 3, by + 3], [right + 2, by + 4]], MARKS.work);
  if (mood === "done") mark([[right + 1, top + 2], [right + 2, top + 3], [right + 3, top + 2], [right + 4, top + 1]], MARKS.done);
  grid.splice(0, top - (top % 2));
  return grid;
}

export function headPixels(key, mood, light) {
  const a = BIRDS[key], rows = a.rows, top = rows.findIndex((r) => /[^.]/.test(r)), [bx, by] = a.tip;
  let from, to, left;
  if (a.crop) { [from, to] = [a.crop.rows[0], a.crop.rows[1] + 1]; left = a.crop.cols[0]; }
  else { let s = Math.max(by + 2 - 8, top); s -= s % 2; [from, to] = [s, s + 8]; left = Math.max(0, bx - 12); }
  const dropped = top - (top % 2);
  return pixels(key, mood, light).slice(from - dropped, to - dropped).map((r) => r.slice(left));
}

export function svg(grid, scale, label) {
  const w = Math.max(...grid.map((r) => r.length)), h = grid.length;
  let rects = "";
  grid.forEach((row, y) => {
    for (let x = 0; x < row.length;) {
      const c = row[x];
      if (c == null) { x++; continue; }
      let e = x + 1;
      while (e < row.length && row[e] === c) e++;
      rects += `<rect x="${x}" y="${y}" width="${e - x}" height="1.02" fill="${hex(c)}"/>`;
      x = e;
    }
  });
  return `<svg class="sprite" viewBox="0 0 ${w} ${h}" width="${w * scale}" height="${h * scale}" shape-rendering="crispEdges" role="img" aria-label="${label}">${rects}</svg>`;
}

/** The theme's bird, whole or its head, in `mood`; nothing for a palette theme. */
export function birdArt(theme, mood, light, scale, whole = true) {
  if (!theme.art) return "";
  return `<span data-bird="${whole ? "whole" : "head"}" data-scale="${scale}">${svg((whole ? pixels : headPixels)(theme.art, mood, light), scale, theme.name)}</span>`;
}

/** Redraws every bird on the page in `mood`: the blink, a turn's face. */
export function redrawBirds(theme, mood, light) {
  if (!theme.art) return;
  document.querySelectorAll("[data-bird]").forEach((el) => {
    el.innerHTML = svg((el.dataset.bird === "whole" ? pixels : headPixels)(theme.art, mood, light), +el.dataset.scale, theme.name);
  });
}

export const ternMark = (light, scale) => svg(pixels("tern-mark", "idle", light), scale, "Arctic tern in flight");
