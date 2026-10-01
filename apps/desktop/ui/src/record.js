// Reads a session's record -- `conversation`, `notebook` and `reading` as a
// transcript or a snapshot carries them (docs/engine.md) -- into what the
// conversation draws: turns, each a person's message and then Sterna's
// prose, cells and answer. Nothing here decides a word the engine decides:
// a cell's state and its line come from `reading`, and the calls from the
// cell's own `execution` record.

/** One message's blocks, whichever way the engine spelled them. */
export function blocksOf(message) {
  const content = message?.content;
  if (typeof content === "string") return [{ type: "text", text: content }];
  if (!Array.isArray(content)) return [];
  return content.map((b) => {
    if (typeof b === "string") return { type: "text", text: b };
    if (b && typeof b.type === "string") {
      const type = b.type.toLowerCase();
      if (type === "tool_use" || type === "tooluse") return { type: "tool_use", id: b.id, name: b.name, input: b.input || {} };
      if (type === "tool_result" || type === "toolresult") return { type: "tool_result", id: b.tool_use_id, text: textOf(b.content), error: !!b.is_error };
      if (type === "text") return { type: "text", text: b.text ?? "" };
      return { type };
    }
    // Serde's own spelling of an enum, as the engine sends it:
    // {"text": "…"}, {"tool_use": {…}}, {"tool_result": {…}}, {"image": {…}}.
    const [key, value] = Object.entries(b || {})[0] || [];
    switch (String(key || "").toLowerCase().replace(/_/g, "")) {
      case "text": return { type: "text", text: String(value ?? "") };
      case "tooluse": return { type: "tool_use", id: value?.id, name: value?.name, input: value?.input || {} };
      case "toolresult": return { type: "tool_result", id: value?.tool_use_id, text: textOf(value?.content), error: !!value?.is_error };
      case "image": return { type: "image" };
      case "thinking": case "redactedthinking": return { type: "thinking" };
      default: return { type: "other" };
    }
  });
}
const textOf = (c) => (typeof c === "string" ? c : Array.isArray(c) ? c.map((x) => x?.text ?? "").join("\n") : "");
const roleOf = (m) => String(m?.role || "").toLowerCase();

/** A person's message as they wrote it: its first text, and a marker per image. */
export function asWritten(blocks) {
  const text = blocks.find((b) => b.type === "text")?.text ?? "";
  const images = blocks.filter((b) => b.type === "image").length;
  return [text, ...Array(images).fill("[image attachment]")].filter(Boolean).join("\n");
}

/** The calls a cell made, from its execution record (`├─ bash cargo test · returned`). */
export function callsOf(execution) {
  if (!execution) return [];
  const lines = String(execution).split("\n").map((l) => l.trim()).filter(Boolean);
  if (lines.length === 1 && !/^[├└]─/.test(lines[0])) return [];
  return lines.map((line) => {
    const body = line.replace(/^[├└]─\s*/, "");
    const [head, status = "", ...rest] = body.split(" · ");
    const at = head.indexOf(" ");
    const tool = at < 0 ? head : head.slice(0, at), arg = at < 0 ? "" : head.slice(at + 1);
    const word = status.trim().toLowerCase();
    const mark = word === "returned" ? "ok" : word === "failed" ? "fail" : word === "denied" ? "deny" : word === "started" ? "run" : "ok";
    return { tool, arg, word: status.trim(), mark, detail: rest.join(" · ") };
  });
}

const MARKS = { returned: "ok", failed: "fail", denied: "deny", started: "run" };
/** A cell's calls: the engine's reading of them when it sends one, else read from its record. */
export function cellCalls(read, view) {
  if (read && Array.isArray(read.calls)) return read.calls.map((c) => ({ tool: c.tool || "", arg: c.target || "", word: c.outcome || "", mark: MARKS[c.outcome] || "ok", detail: c.detail || "" }));
  return callsOf(view?.execution);
}

/** Whether a cell's record says it ran no calls. */
export const noCalls = (execution) => !!execution && callsOf(execution).length === 0;

/** The code a cell ran: the corrected source when the engine kept one. */
export const codeOf = (block, view) => view?.executed_source ?? block?.input?.code ?? (typeof block?.input === "string" ? block.input : "");

/**
 * The turns of a conversation. Each is `{ you, items }`: the person's words
 * (null for what came before any) and, in order, `{kind:"prose"}`,
 * `{kind:"cell"}` and `{kind:"answer"}` items. Cells are numbered across the
 * whole conversation, as the notebook holds them.
 */
export function turnsOf(conversation, notebook, reading) {
  const messages = conversation?.messages || [];
  const views = notebook?.cells || [];
  const readings = reading?.cells || [];
  const turns = [];
  let turn = null, n = 0;
  const open = (you, index) => { turn = { you, index, items: [] }; turns.push(turn); };
  messages.forEach((message, index) => {
    const blocks = blocksOf(message);
    if (roleOf(message) === "user") {
      if (!blocks.length || blocks.every((b) => b.type === "tool_result")) return;
      if (message.historical) return;
      const words = asWritten(blocks);
      if (!words) return;
      open(words, index);
      return;
    }
    if (!turn) open(null, index);
    for (const block of blocks) {
      // The turn's answer comes back once more as the model's last words:
      // it is drawn once, under the cell that returned it.
      const answered = turn.items.filter((i) => i.kind === "answer").pop();
      if (block.type === "text" && answered && block.text.trim() === String(answered.text).trim()) continue;
      if (block.type === "text" && block.text.trim()) turn.items.push({ kind: "prose", text: block.text });
      if (block.type !== "tool_use") continue;
      n++;
      const view = views[n - 1] || {};
      const read = readings.find((r) => r.cell === n) || readings[n - 1] || null;
      turn.items.push({ kind: "cell", n, code: codeOf(block, view), view, reading: read, calls: cellCalls(read, view), message: index });
      if (view.returned != null) {
        const facts = read?.facts || (reading?.answer?.cell === n ? reading.answer : null);
        turn.items.push({ kind: "answer", n, text: view.returned, facts });
      }
    }
  });
  return { turns, cells: n };
}

/** Whether a cell has an outcome on record: it ran, failed, or was rolled back. */
export const settledCell = (view) => !!(view && (view.execution || view.error || view.returned != null || view.rolled_back));

/**
 * The code being written, from a tool_delta stream: the stream is the
 * call's input as JSON as it arrives (`{"description":…,"code":"…`), or the
 * code itself. Returns what can be read of it so far.
 */
export function writing(stream) {
  const text = String(stream || "");
  if (!/^\s*\{/.test(text)) return { code: text, description: null };
  return { code: partialString(text, "code") ?? "", description: partialString(text, "description") };
}
function partialString(json, key) {
  const m = json.match(new RegExp(`"${key}"\\s*:\\s*"`));
  if (!m) return null;
  let out = "", i = m.index + m[0].length;
  const ESC = { n: "\n", t: "\t", r: "\r", b: "\b", f: "\f", '"': '"', "\\": "\\", "/": "/" };
  while (i < json.length) {
    const c = json[i];
    if (c === '"') return out;
    if (c === "\\") {
      const d = json[i + 1];
      if (d === undefined) return out;
      if (d === "u") {
        const code = json.slice(i + 2, i + 6);
        if (code.length < 4) return out;
        out += String.fromCharCode(parseInt(code, 16));
        i += 6;
        continue;
      }
      out += ESC[d] ?? d;
      i += 2;
      continue;
    }
    out += c;
    i++;
  }
  return out;
}

/** The files a diff names, and its lines added and removed. */
export function diffFacts(diff) {
  const files = [], text = String(diff || "");
  let added = 0, removed = 0;
  for (const l of text.split("\n")) {
    if (l.startsWith("+++ ")) { files.push(l.slice(4).replace(/^b\//, "")); continue; }
    if (l.startsWith("---")) continue;
    if (l.startsWith("+")) added++;
    else if (l.startsWith("-")) removed++;
  }
  return { files, added, removed };
}

/** The diff as hunks of rows, for the Changes tab. */
export function diffRows(diff) {
  const files = [];
  let file = null, o = 0, n = 0;
  for (const line of String(diff || "").split("\n")) {
    if (line.startsWith("--- ")) continue;
    if (line.startsWith("+++ ")) { file = { path: line.slice(4).replace(/^b\//, ""), rows: [] }; files.push(file); continue; }
    if (!file) { file = { path: "", rows: [] }; files.push(file); }
    if (line.startsWith("@@")) {
      const m = line.match(/-(\d+)(?:,\d+)?\s+\+(\d+)/);
      if (m) { o = +m[1] - 1; n = +m[2] - 1; }
      file.rows.push({ kind: "hunk", text: line });
      continue;
    }
    const c = line[0], body = line.slice(1);
    if (c === "+") { n++; file.rows.push({ kind: "add", o: "", n, text: body }); }
    else if (c === "-") { o++; file.rows.push({ kind: "del", o, n: "", text: body }); }
    else if (c === "\\") file.rows.push({ kind: "hunk", text: line });
    else if (line !== "") { o++; n++; file.rows.push({ kind: "same", o, n, text: line.slice(1) }); }
  }
  return files.filter((f) => f.rows.length || f.path);
}
