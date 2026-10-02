import test from "node:test";
import assert from "node:assert/strict";
import { prose, splitAnswer, ago, tilde, baseName, inlineHTML } from "../src/util.js";

test("prose keeps paragraphs, lists and code blocks, and escapes everything else", () => {
  assert.equal(prose("one\ntwo\n\nthree"), "<p>one two</p><p>three</p>");
  assert.equal(prose("- a\n- b"), "<ul><li>a</li><li>b</li></ul>");
  assert.equal(prose("```\n<x>\n```"), '<pre class="code">&lt;x&gt;</pre>');
  assert.equal(prose("<b>"), "<p>&lt;b&gt;</p>");
});

test("a path in prose is a link a click opens, and code stays code", () => {
  assert.match(inlineHTML("in crates/tally/src/amount.rs."), /data-act="path" data-v="crates\/tally\/src\/amount.rs"/);
  assert.equal(inlineHTML("`a/b.rs`"), "<code>a/b.rs</code>");
  assert.doesNotMatch(inlineHTML("https://example.com/a.html"), /data-act="path"/);
});

test("an answer's first paragraph stands apart from the rest", () => {
  assert.deepEqual(splitAnswer("Done.\n\nMore here."), ["Done.", "More here."]);
  assert.deepEqual(splitAnswer("Only this."), ["Only this.", ""]);
});

test("times and paths read as the resume picker says them", () => {
  assert.equal(ago(30), "just now");
  assert.equal(ago(600), "10 min ago");
  assert.equal(ago(93000), "1 day ago");
  assert.equal(tilde("/Users/a/code/quill", "/Users/a"), "~/code/quill");
  assert.equal(tilde("/Users/ab/x", "/Users/a"), "/Users/ab/x");
  assert.equal(baseName("/Users/a/code/quill/"), "quill");
});

test("a folder's parent path is what the sidebar shows beside its name", async () => {
  const { parentPath } = await import("../src/util.js");
  assert.equal(parentPath("~/code/quill"), "~/code");
  assert.equal(parentPath("~/quill"), "~");
  assert.equal(parentPath("/srv/x"), "/srv");
  assert.equal(parentPath("/x"), "/");
  assert.equal(parentPath("C:\\Users\\a\\code\\q"), "C:\\Users\\a\\code");
  assert.equal(parentPath("~"), "");
});

test("a path that does not fit is cut at its start, a whole folder at a time", async () => {
  const { clipStart } = await import("../src/util.js");
  const fits = (n) => (text) => text.length <= n;
  assert.equal(clipStart("~/work/clients", fits(20)), "~/work/clients");
  assert.equal(clipStart("~/work/clients/acme", fits(15)), "…/clients/acme");
  assert.equal(clipStart("~/work/clients/acme", fits(8)), "…/acme");
  assert.equal(clipStart("~/work/clients/acme", fits(3)), "", "not even the last folder fits: nothing, not a cut word");
  assert.equal(clipStart("C:\\Users\\a\\work", fits(8)), "…\\a\\work");
});
