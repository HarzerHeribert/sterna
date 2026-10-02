// The themes, as /theme lists them (crates/sterna/src/workbench/theme.rs and
// plumage.rs), each bird with one plain fact about it, as plumage.rs says it. Only the accent moves with the theme; on a light ground it
// is darkened until it reads at 4.5:1, as workbench/look.rs reads colour.
// The roles -- muted, failure, warning, success, evidence, you -- are one
// fixed colour per ground (style.css).

export const THEMES = [
  { id: "neon", fam: "Classic", name: "Neon", accent: 0xdaff50 },
  { id: "amber", fam: "Classic", name: "Amber", accent: 0xffce72 },
  { id: "ice", fam: "Classic", name: "Ice", accent: 0x8be3ff },
  { id: "mono", fam: "Classic", name: "Mono", accent: null },
  { id: "violet", fam: "Classic", name: "Violet", accent: 0xd4b4ff },
  { id: "cobalt", fam: "Classic", name: "Cobalt", accent: 0x9ec9ff },
  { id: "mint", fam: "Classic", name: "Mint", accent: 0x86f1d0 },
  { id: "rose", fam: "Classic", name: "Rose", accent: 0xffb3d4 },
  { id: "amazon", fam: "Parrots", name: "Blue-fronted Amazon", latin: "Amazona aestiva", nest: "from Brazil to northern Argentina", accent: 0x45b653, ground: 0x16241a, art: "amazon" },
  { id: "sun-conure", fam: "Parrots", name: "Sun Conure", latin: "Aratinga solstitialis", nest: "endangered in the wild", accent: 0xffb020, ground: 0x2a1f10, art: "sun-conure" },
  { id: "hyacinth", fam: "Parrots", name: "Hyacinth Macaw", latin: "Anodorhynchus hyacinthinus", nest: "the largest flying parrot", accent: 0x5b7cf0, ground: 0x161c34, art: "hyacinth" },
  { id: "scarlet", fam: "Parrots", name: "Scarlet Macaw", latin: "Ara macao", nest: "from southern Mexico to the Amazon", accent: 0xf5c21b, ground: 0x2c1515, art: "scarlet" },
  { id: "blue-gold", fam: "Parrots", name: "Blue-and-gold Macaw", latin: "Ara ararauna", nest: "from Panama to Paraguay", accent: 0x2fa0ea, ground: 0x122230, art: "blue-gold" },
  { id: "green-wing", fam: "Parrots", name: "Green-winged Macaw", latin: "Ara chloropterus", nest: "the largest of the Ara macaws", accent: 0x43a867, ground: 0x2a1519, art: "green-wing" },
  { id: "military", fam: "Parrots", name: "Military Macaw", latin: "Ara militaris", nest: "from Mexico to Argentina", accent: 0x8fbd4f, ground: 0x1c2412, art: "military" },
  { id: "cockatoo", fam: "Parrots", name: "Sulphur-crested Cockatoo", latin: "Cacatua galerita", nest: "of Australia and New Guinea", accent: 0xf7d23a, ground: 0x24221a, art: "cockatoo" },
  { id: "arctic-tern", fam: "Seabird", name: "Arctic Tern", latin: "Sterna paradisaea", nest: "the longest migration of any bird", accent: 0xdce1e6, ground: 0x14181d, art: "tern-perched" },
];
export const FAMILIES = ["Classic", "Parrots", "Seabird"];
export const themeOf = (id) => THEMES.find((t) => t.id === id) || THEMES.find((t) => t.id === "amazon");

export const chan = (n) => [(n >> 16) & 255, (n >> 8) & 255, n & 255];
export const lum = (n) => chan(n).map((c) => { c /= 255; return c <= 0.03928 ? c / 12.92 : ((c + 0.055) / 1.055) ** 2.4; }).reduce((s, c, i) => s + c * [0.2126, 0.7152, 0.0722][i], 0);
export const contrast = (a, b) => { const [x, y] = [lum(a), lum(b)].sort((p, q) => q - p); return (x + 0.05) / (y + 0.05); };
export const hex = (n) => "#" + n.toString(16).padStart(6, "0");
export const LIGHT_GROUND = 0xf4f6f8, DARK_GROUND = 0x0a0e12;

/** A colour moved toward black (light) or white (dark) until it reads at `ratio` on `ground`. */
export function readable(rgb, light, ratio, ground = light ? LIGHT_GROUND : DARK_GROUND) {
  const toward = light ? 0 : 255;
  let c = rgb;
  for (let i = 0; i < 40 && contrast(c, ground) < ratio; i++) {
    const [r, g, b] = chan(c).map((v) => Math.round(v + (toward - v) * 0.08));
    c = (r << 16) | (g << 8) | b;
  }
  return c;
}
export const mixHex = (a, b, t) => { const [x, y] = [chan(a), chan(b)]; return x.map((v, i) => Math.round(v + (y[i] - v) * t)).reduce((s, v) => s * 256 + v, 0); };
export const accentOf = (t, light) => (t.accent == null ? null : light ? readable(t.accent, true, 4.5) : t.accent);
const TEXT = (light) => (light ? 0x15191f : 0xeceef1);
/** The sidebar's ground: the bird's own on dark, a breath of its accent on light. */
export function sideOf(t, light) {
  const a = accentOf(t, light), base = light ? 0xe9ecef : 0x101317;
  return t.ground && !light ? t.ground : a == null ? base : mixHex(base, a, light ? 0.07 : 0.05);
}
/** The accent as the sidebar draws it: moved until it reads at 4.5:1 on that ground. */
export function sideAccentOf(t, light) {
  const a = accentOf(t, light);
  return a == null ? TEXT(light) : readable(a, light, 4.5, sideOf(t, light));
}
/**
 * The accent as text: moved until it reads at 4.5:1 on every ground it is
 * drawn on in the window -- the window, a card, a sheet's card, and its own
 * pale tint behind a pill.
 */
export function accentTextOf(t, light) {
  const a = accentOf(t, light);
  if (a == null) return TEXT(light);
  let c = a;
  for (const ground of accentGrounds(a, light)) c = readable(c, light, 4.5, ground);
  return c;
}
export const accentGrounds = (a, light) => {
  const [win, card, card2] = light ? [0xf3f4f6, 0xffffff, 0xffffff] : [0x15191f, 0x1c2128, 0x242a33];
  return [win, card, card2, mixHex(card, a, 0.12), mixHex(win, a, 0.12), mixHex(card2, a, 0.12)];
};
/** Black or white, whichever reads better on a fill of `rgb`. */
export const inkOf = (rgb) => (contrast(rgb, 0) >= contrast(rgb, 0xffffff) ? 0 : 0xffffff);

/** Whether the window draws on a light ground now. */
export function isLight(appearance) {
  return appearance === "light" || (appearance === "system" && matchMedia("(prefers-color-scheme: light)").matches);
}

/** Sets the ground and the accent's custom properties on the page. */
export function applyTheme(prefs) {
  const light = isLight(prefs.appearance), t = themeOf(prefs.theme), body = document.body, st = body.style;
  body.classList.toggle("light", light);
  body.classList.toggle("dark", !light);
  const a = accentOf(t, light);
  const text = light ? 0x15191f : 0xeceef1, card = light ? 0xffffff : 0x1c2128;
  // Mono's accent is the text itself; a filled control reverses.
  st.setProperty("--accent", hex(a == null ? text : a));
  const fill = a == null ? text : a;
  st.setProperty("--accent-ink", hex(inkOf(fill)));
  st.setProperty("--fill", hex(text));
  st.setProperty("--on-fill", hex(card));
  st.setProperty("--accent-text", hex(accentTextOf(t, light)));
  st.setProperty("--side", hex(sideOf(t, light)));
  st.setProperty("--side-accent", hex(sideAccentOf(t, light)));
  st.setProperty("--side-accent-ink", hex(inkOf(sideAccentOf(t, light))));
  if (t.id === "mono") { st.setProperty("--evid", hex(text)); st.setProperty("--you", hex(text)); }
  else { st.removeProperty("--evid"); st.removeProperty("--you"); }
  return { light, theme: t };
}
