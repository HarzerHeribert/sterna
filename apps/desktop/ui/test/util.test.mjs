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
