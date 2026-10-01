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
