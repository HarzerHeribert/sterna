// The record reader against the shapes the engine sends (probed from
// `sterna host` on 2026-10-01): blocks in serde's spelling, a person's
// message with the task's context after their words, the runtime's answer
// to each cell, and the answer said once more as the model's last words.
import test from "node:test";
import assert from "node:assert/strict";
import { blocksOf, turnsOf, callsOf, writing, diffFacts, diffRows, asWritten } from "../src/record.js";

const conversation = {
  system: "You are Sterna…",
  messages: [
    { role: "user", content: [{ text: "Write note.txt in this folder." }, { text: "## Environment orientation\nplatform: macos" }], historical: null },
    { role: "assistant", content: [{ text: "I'll write the note first, then read it back." }, { tool_use: { id: "cell-1", name: "execute_cell", input: { code: "await write({ path: \"note.txt\" });\nconsole.log(1);" } } }], historical: null },
    { role: "user", content: [{ tool_result: { tool_use_id: "cell-1", content: "[cell 1 yielded in 40 ms]", is_error: false } }], historical: null },
    { role: "assistant", content: [{ tool_use: { id: "end-1", name: "execute_cell", input: { code: "answer(\"note.txt is written.\");" } } }], historical: null },
    { role: "user", content: [{ tool_result: { tool_use_id: "end-1", content: "## Return\nnote.txt is written.", is_error: false } }], historical: null },
    { role: "assistant", content: [{ text: "note.txt is written." }], historical: null },
  ],
};
const notebook = {
  cells: [
    { executed_source: "await write({ path: \"note.txt\" });\nconsole.log(1);", changes: "--- /dev/null\n+++ b/note.txt\n@@ -1,0 +1,1 @@\n+written\n", stdout: "1\n", execution: "├─ write note.txt · returned\n└─ read note.txt · returned", call_count: 2, returned: null, answered: true },
    { executed_source: "answer(\"note.txt is written.\");", execution: "No tool calls ran in this cell.", call_count: 0, returned: "note.txt is written.", answered: true },
  ],
};
const reading = {
  cells: [
    { cell: 1, state: "EXECUTED", mark: "✓", tone: "success", line: "✓ executed · 1 file changed", parts: [], facts: null },
    { cell: 2, state: "EXECUTED", mark: "✓", tone: "success", line: "✓ executed · no files changed", parts: [], facts: { cell: 2, mark: "✓", failed: false, facts: "complete." } },
  ],
  answer: { cell: 2, mark: "✓", failed: false, facts: "complete." },
};

test("blocks in serde's spelling read as text, a cell call and its result", () => {
  assert.deepEqual(blocksOf(conversation.messages[1]).map((b) => b.type), ["text", "tool_use"]);
  assert.equal(blocksOf(conversation.messages[1])[1].input.code.startsWith("await write"), true);
  assert.deepEqual(blocksOf(conversation.messages[2]).map((b) => b.type), ["tool_result"]);
  // The provider's own spelling reads the same.
  assert.deepEqual(blocksOf({ content: [{ type: "text", text: "a" }, { type: "tool_use", id: "x", name: "execute_cell", input: { code: "1" } }] }).map((b) => b.type), ["text", "tool_use"]);
  assert.deepEqual(blocksOf({ content: "plain" }), [{ type: "text", text: "plain" }]);
});

test("a person's message is their own words, never the task's context after them", () => {
  assert.equal(asWritten(blocksOf(conversation.messages[0])), "Write note.txt in this folder.");
  assert.equal(asWritten(blocksOf({ content: [{ text: "look" }, { image: { media_type: "image/png", data: "" } }] })), "look\n[image attachment]");
});

test("a turn is the person's words, then prose, cells and one answer, in order", () => {
  const { turns, cells } = turnsOf(conversation, notebook, reading);
  assert.equal(cells, 2);
  assert.equal(turns.length, 1);
  assert.equal(turns[0].you, "Write note.txt in this folder.");
  assert.deepEqual(turns[0].items.map((i) => i.kind), ["prose", "cell", "cell", "answer"]);
  const [, first, second, answer] = turns[0].items;
  assert.equal(first.n, 1);
  assert.equal(first.reading.state, "EXECUTED");
  assert.equal(second.code, "answer(\"note.txt is written.\");");
  // The answer is drawn once, under its cell, with the engine's facts.
  assert.equal(answer.text, "note.txt is written.");
  assert.equal(answer.facts.facts, "complete.");
});

test("a cell's calls come from its execution record, with the words it ended in", () => {
  assert.deepEqual(callsOf(notebook.cells[0].execution).map((c) => [c.tool, c.arg, c.word, c.mark]), [["write", "note.txt", "returned", "ok"], ["read", "note.txt", "returned", "ok"]]);
  assert.deepEqual(callsOf("No tool calls ran in this cell."), []);
  const [failed, denied] = callsOf("├─ read amount.rs · failed · NotFound\n└─ bash cargo test · denied · static.crates.io");
  assert.deepEqual([failed.mark, failed.detail, denied.mark, denied.detail], ["fail", "NotFound", "deny", "static.crates.io"]);
});

test("a cell being written is read from its input as it arrives, escapes and all", () => {
  assert.deepEqual(writing('{"code":"await bash({ command: \\"ls'), { code: 'await bash({ command: "ls', description: null });
  assert.deepEqual(writing('{"description":"List it","code":"a\\nb"}'), { code: "a\nb", description: "List it" });
  assert.equal(writing("plain code so far").code, "plain code so far");
  assert.equal(writing('{"code":"tail \\').code, "tail ");
});

test("a diff gives its files, its lines added and removed, and numbered rows", () => {
  assert.deepEqual(diffFacts(notebook.cells[0].changes), { files: ["note.txt"], added: 1, removed: 0 });
  const [file] = diffRows("--- a/x.rs\n+++ b/x.rs\n@@ -17,2 +17,3 @@\n keep\n-old\n+new\n+more\n");
  assert.equal(file.path, "x.rs");
  assert.deepEqual(file.rows.map((r) => [r.kind, r.o, r.n]), [["hunk", undefined, undefined], ["same", 17, 17], ["del", 18, ""], ["add", "", 18], ["add", "", 19]]);
});

test("a cell's calls are the engine's reading of them when it sends one", async () => {
  const { cellCalls } = await import("../src/record.js");
  const read = { calls: [{ tool: "write", target: "note.txt", outcome: "returned", detail: "" }, { tool: "bash", target: "cargo test", outcome: "denied", detail: "static.crates.io" }] };
  assert.deepEqual(cellCalls(read, { execution: "├─ ignored · returned" }).map((c) => [c.tool, c.arg, c.word, c.mark, c.detail]),
    [["write", "note.txt", "returned", "ok", ""], ["bash", "cargo test", "denied", "deny", "static.crates.io"]]);
  // An engine that sends no calls yet: they are read from the cell's record.
  assert.deepEqual(cellCalls({ cell: 1 }, { execution: "└─ read amount.rs · failed · NotFound" }).map((c) => c.mark), ["fail"]);
  assert.deepEqual(cellCalls({ calls: [] }, { execution: "No tool calls ran in this cell." }), []);
});
