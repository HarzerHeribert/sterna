// Newer releases, as the host's `update` reports them (docs/engine.md): the
// sidebar's card speaks only when there is something new, or a move under
// way, done or failed; errors are said in plain words.
import test from "node:test";
import assert from "node:assert/strict";
import { releaseCard, releasesPane, plainError, bare } from "../src/views/releases.js";

const app = (releases) => ({ S: { releases: { phase: "checked", answer: null, error: "", asked: false, moved: "", ...releases } }, engine: { ready: { version: "0.1.0-pre.30" } } });
const newer = { updates: true, installed: "v0.1.0-pre.30", latest: "v0.1.0-pre.31", available: true, automatic: true };

test("a newer release is offered with Update; nothing new, nothing said", () => {
  assert.match(releaseCard(app({ answer: newer })), /Sterna 0\.1\.0-pre\.31 is available.*data-act="rel-move">Update</s);
  assert.equal(releaseCard(app({ answer: { ...newer, available: false, latest: newer.installed } })), "");
  assert.equal(releaseCard(app({ answer: { updates: false, why: "A hand download." } })), "");
  assert.equal(releaseCard(app({ phase: "failed", error: "offline" })), "", "a check that failed on open stays quiet");
  assert.equal(releaseCard(app({ phase: "idle" })), "");
});

test("with automatic checks off, the card waits for Check now", () => {
  assert.equal(releaseCard(app({ answer: { ...newer, automatic: false } })), "");
  assert.match(releaseCard(app({ answer: { ...newer, automatic: false }, asked: true })), /is available/);
});

test("the move shows its progress, then Restart, or what went wrong with Try again", () => {
  assert.match(releaseCard(app({ answer: newer, phase: "moving" })), /Updating to Sterna 0\.1\.0-pre\.31….*disabled>Updating…/s);
  assert.match(releaseCard(app({ answer: newer, phase: "moved", moved: "v0.1.0-pre.31" })), /Sterna 0\.1\.0-pre\.31 is installed.*data-act="rel-restart">Restart</s);
  assert.match(releaseCard(app({ answer: newer, phase: "unmoved", error: "x: connection refused" })), /was not installed.*GitHub could not be reached.*data-act="rel-move">Try again</s);
});

test("Settings shows the version, Check now, and for a copy that does not move, why and the releases page", () => {
  const pane = releasesPane(app({ answer: { updates: false, why: "This copy of Sterna was not installed with the install line, so it does not update itself." } }));
  assert.match(pane, /Sterna 0\.1\.0-pre\.30.*data-act="rel-check">Check now/s);
  assert.match(pane, /not installed with the install line.*data-act="rel-page"/s);
  assert.match(releasesPane(app({ phase: "failed", error: "dns error" })), /GitHub could not be reached.*data-act="rel-check">Try again/s);
});

test("errors and versions are said plainly", () => {
  assert.equal(bare("v0.1.0-pre.31"), "0.1.0-pre.31");
  assert.equal(plainError("https://api.github.com/x: io: Connection refused (os error 61)"), "GitHub could not be reached. Check the connection and try again.");
  assert.equal(plainError("https://github.com/a.tar.gz: the archive's checksum does not match"), "The engine said: The archive's checksum does not match.");
});
