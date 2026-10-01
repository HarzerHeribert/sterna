// The traced birds, read from the repository's art/birds the way the
// mockup's build.py reads them: each sprite's rows, its palette on a dark
// and on a light ground, and the landmarks the moods are drawn from.
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { join } from "node:path";

export const ART = fileURLToPath(new URL("../../../art/birds/", import.meta.url));

export const NAMES = ["amazon", "sun-conure", "hyacinth", "scarlet", "blue-gold", "green-wing",
  "military", "cockatoo", "tern-perched", "tern-mark"];

const colours = (table) => Object.fromEntries(Object.entries(table || {})
  .filter(([k, v]) => k.length === 1 && typeof v === "string")
  .map(([k, v]) => [k, parseInt(v.slice(1), 16)]));

export function bird(name) {
  const rows = readFileSync(join(ART, name, "sprite.txt"), "utf8").split(/\r?\n/).filter((r, i, all) => r.length || i < all.length - 1);
  const palette = JSON.parse(readFileSync(join(ART, name, "palette.json"), "utf8"));
  const marks = JSON.parse(readFileSync(join(ART, name, "landmarks.json"), "utf8"));
  const crop = marks.head_crop;
  return {
    w: Math.max(...rows.map((r) => r.length)),
    rows,
    pal: colours(palette),
    light: colours(palette._light),
    eye: marks.eye,
    tip: marks.beak_tip,
    crop: crop ? { rows: crop.rows, cols: crop.cols } : null,
  };
}

export const birds = () => Object.fromEntries(NAMES.map((n) => [n, bird(n)]));
