// The window's preferences: the theme, the motion and the stream are the
// person's shared settings; the rest is the app's own, kept whole by the host.
import test from "node:test";
import assert from "node:assert/strict";

globalThis.innerWidth = 1480;
const { fromHost, ownPart, SHARED, DEFAULTS } = await import("../src/prefs.js");

test("the shared settings come from the person's settings, the rest from the app's preferences", () => {
  const prefs = fromHost({ appearance: "dark", card: false, theme: "rose" }, { "ui.theme": "hyacinth", "ui.motion": "calm" });
  assert.equal(prefs.theme, "hyacinth", "the app's own copy of a shared key is not read");
  assert.equal(prefs.motion, "calm");
  assert.equal(prefs.stream, DEFAULTS.stream);
  assert.equal(prefs.appearance, "dark");
  assert.equal(prefs.card, false);
});

test("a preference of the wrong kind is left at its default", () => {
  assert.equal(fromHost({ card: "yes", sessions: 1 }, {}).card, true);
});

test("what the app keeps for itself leaves the shared settings out", () => {
  const own = ownPart({ ...DEFAULTS, appearance: "light" });
  for (const k of Object.keys(SHARED)) assert.ok(!(k in own), k);
  assert.equal(own.appearance, "light");
});
