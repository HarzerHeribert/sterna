// What the mock engine records, in the shapes the real engine sends
// (docs/engine.md, crates/sterna/src/engine/wire.rs): a conversation of
// messages and blocks, a notebook of cell views, and the reading -- the
// words a client shows -- decided here by the same rules as
// engine/reading.rs. Development and tests only; the app never decides
// these words itself.

// Blocks in serde's spelling, as the engine sends them: {"text": …},
// {"tool_use": {…}}, {"tool_result": {…}}. A person's message carries the
// task's context for the model after their own words.
export const user = (text) => ({ role: "user", content: [{ text }, { text: "## Environment orientation\n(the mock engine's)" }], historical: null });
export const assistant = (prose, id, code) => ({
  role: "assistant",
  content: [...(prose ? [{ text: prose }] : []), ...(id ? [{ tool_use: { id, name: "execute_cell", input: { code } } }] : [])],
  historical: null,
});
/** The turn's answer, said once more as the model's last words. */
export const said = (text) => ({ role: "assistant", content: [{ text }], historical: null });
export const result = (id, content, isError = false) => ({
  role: "user",
  content: [{ tool_result: { tool_use_id: id, content, is_error: isError } }],
  historical: null,
});

/** One cell's view as it starts: what it is for, nothing observed yet. */
export const started = (description) => ({ description, origin: "authored_cell", answered: false, rolled_back: false });

/** The execution string engine/session/cell_view.rs writes: one line per call. */
export function execution(calls) {
  if (!calls.length) return "No tool calls ran in this cell.";
  return calls.map((c, i) => `${i + 1 === calls.length ? "└─" : "├─"} ${c.tool}${c.arg ? " " + c.arg : ""} · ${c.status}`).join("\n");
}

function changedFiles(v) {
  return v.changes ? v.changes.split("\n").filter((l) => l.startsWith("+++ ")).length : 0;
}
function countChanges(diff) {
  let a = 0, r = 0;
  for (const l of diff.split("\n")) {
    if (l.startsWith("+++") || l.startsWith("---")) continue;
    if (l.startsWith("+")) a++;
    else if (l.startsWith("-")) r++;
  }
  return [a, r];
}
function line(v) {
  const out = [];
  if (v.error) out.push({ text: `✕ ${v.error.class}`, tone: "failure" });
  else if (v.execution) out.push({ text: "✓ executed", tone: "success" });
  const n = changedFiles(v);
  const files = v.rolled_back ? "its changes were rolled back" : n === 0 ? "no files changed" : n === 1 ? "1 file changed" : `${n} files changed`;
  if (out.length) out.push({ text: " · ", tone: "line" });
  out.push({ text: files, tone: "muted" });
  return out;
}
function answer(ordinal, v) {
  const files = changedFiles(v);
  const [added, removed] = v.changes ? countChanges(v.changes) : [0, 0];
  const facts = [];
  if (files > 0) facts.push(`${files} ${files === 1 ? "file" : "files"} · +${added} −${removed}`);
  if (v.call_count > 0) facts.push(`${v.call_count} ${v.call_count === 1 ? "call" : "calls"}`);
  const failed = !!v.error;
  return { cell: ordinal, mark: failed ? "✕" : "✓", failed, facts: facts.length ? facts.join(" · ") : failed ? "failed." : "complete." };
}
/** The calls a cell made, as engine/reading.rs::calls reads them from its record. */
function calls(execution) {
  if (!execution || execution.startsWith("No tool calls")) return [];
  return execution.split("\n").map((l) => l.trim().replace(/^[├└]─\s*/, "").trim()).filter(Boolean).map((l) => {
    const at = l.indexOf(" · "), what = at < 0 ? l : l.slice(0, at), status = at < 0 ? "" : l.slice(at + 3);
    const sp = what.indexOf(" "), d = status.indexOf(" · ");
    return { tool: sp < 0 ? what : what.slice(0, sp), target: sp < 0 ? "" : what.slice(sp + 1), outcome: d < 0 ? status : status.slice(0, d), detail: d < 0 ? "" : status.slice(d + 3) };
  });
}
function cell(ordinal, v) {
  const [mark, state, tone] = v.error ? ["✕", "FAILED", "failure"] : v.rolled_back ? ["↶", "ROLLED BACK", "warning"]
    : v.execution ? ["✓", "EXECUTED", "success"] : ["", "RECORDED", "muted"];
  const parts = line(v);
  const record = v.execution || "";
  return {
    cell: ordinal, state, mark, tone, line: parts.map((p) => p.text).join(""), parts,
    clean: !v.error && !!v.execution && !record.includes(" · failed") && !record.includes(" · denied"),
    calls: calls(record), facts: v.returned != null ? answer(ordinal, v) : null,
  };
}

/** The reading of a notebook, as engine/reading.rs::of gives it. */
export function reading(notebook) {
  const cells = notebook.cells.map((v, i) => cell(i + 1, v));
  let found = null;
  notebook.cells.forEach((v, i) => { if (v.returned != null) found = answer(i + 1, v); });
  return { cells, answer: found };
}
