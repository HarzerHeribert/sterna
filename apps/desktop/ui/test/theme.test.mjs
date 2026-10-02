// Every theme's accent reads on the ground it is drawn on: on a light ground
// it is darkened until it reaches 4.5:1, as the mockup's readable() does.
import test from "node:test";
import assert from "node:assert/strict";
import { THEMES, accentOf, contrast, LIGHT_GROUND, readable } from "../src/theme.js";

test("there are the seventeen themes /theme lists", () => {
  assert.equal(THEMES.length, 17);
  assert.equal(THEMES.filter((t) => t.art).length, 9);
});

test("on a light ground every accent reads at 4.5:1 or better", () => {
  for (const t of THEMES.filter((x) => x.accent != null)) {
    const a = accentOf(t, true);
    assert.ok(contrast(a, LIGHT_GROUND) >= 4.5, `${t.id}: ${contrast(a, LIGHT_GROUND).toFixed(2)}`);
  }
});

test("on a dark ground the accent is the theme's own", () => {
  for (const t of THEMES.filter((x) => x.accent != null)) assert.equal(accentOf(t, false), t.accent);
  assert.equal(accentOf(THEMES.find((t) => t.id === "mono"), true), null);
});

test("a colour that already reads is left as it is", () => {
  assert.equal(readable(0x15191f, true, 4.5), 0x15191f);
});

test("each bird carries one plain fact, as plumage.rs says it", () => {
  const facts = Object.fromEntries(THEMES.filter((t) => t.art).map((t) => [t.id, t.nest]));
  assert.equal(facts.amazon, "from Brazil to northern Argentina");
  assert.equal(facts.hyacinth, "the largest flying parrot");
  assert.equal(facts["arctic-tern"], "the longest migration of any bird");
  assert.equal(Object.values(facts).filter(Boolean).length, 9);
});

test("on the sidebar's own ground every accent reads at 4.5:1, and so does a count on its fill", async () => {
  const { sideOf, sideAccentOf, inkOf } = await import("../src/theme.js");
  for (const light of [true, false]) for (const t of THEMES) {
    const a = sideAccentOf(t, light), where = `${t.id} ${light ? "light" : "dark"}`;
    assert.ok(contrast(a, sideOf(t, light)) >= 4.5, `${where}: ${contrast(a, sideOf(t, light)).toFixed(2)}`);
    assert.ok(contrast(inkOf(a), a) >= 4.5, `${where}: the count's ink ${contrast(inkOf(a), a).toFixed(2)}`);
  }
  for (const [warn, ink] of [[0x7f5300, 0xffffff], [0xffca80, 0x15191f]]) assert.ok(contrast(warn, ink) >= 4.5);
});

test("the diff stat's added and removed counts read at 4.5:1 on a card, light and dark", () => {
  // --ok and --fail on --card, as style.css sets them.
  for (const [ok, fail, card] of [[0x1a6833, 0xb3261e, 0xffffff], [0xa4f1bd, 0xff8494, 0x1c2128]]) {
    assert.ok(contrast(ok, card) >= 4.5, `added ${contrast(ok, card).toFixed(2)}`);
    assert.ok(contrast(fail, card) >= 4.5, `removed ${contrast(fail, card).toFixed(2)}`);
  }
});

test("the accent as text reads at 4.5:1 on every ground it is drawn on, its own pill tint included", async () => {
  const { accentTextOf, accentGrounds } = await import("../src/theme.js");
  for (const light of [true, false]) for (const t of THEMES.filter((x) => x.accent != null)) {
    const a = accentTextOf(t, light);
    for (const ground of accentGrounds(accentOf(t, light), light)) assert.ok(contrast(a, ground) >= 4.5, `${t.id} ${light ? "light" : "dark"}: ${contrast(a, ground).toFixed(2)}`);
  }
});

test("on light, the state colours read at 4.5:1 on their own pale tint, as a pill draws them", () => {
  // --ok, --warn and --fail on light, as style.css sets them, on the window and on a card.
  for (const c of [0x1a6833, 0x7f5300, 0xb3261e]) for (const g of [0xf3f4f6, 0xffffff]) {
    const tint = [0, 1, 2].map((i) => Math.round(((g >> (16 - 8 * i)) & 255) * 0.86 + ((c >> (16 - 8 * i)) & 255) * 0.14)).reduce((s, v) => s * 256 + v, 0);
    assert.ok(contrast(c, tint) >= 4.5, `${c.toString(16)} on its tint: ${contrast(c, tint).toFixed(2)}`);
  }
});
