// A scripted Messages endpoint for running the real engine with no model
// and no network: answers in the shape the engine's own checks use
// (crates/sterna/tests/support/{engine,sse}.rs), as JSON or as Server-Sent
// Events, whichever the request asked for. Streamed answers come in paced
// pieces, so a client sees each state of a turn. Development and tests
// only; the engine it serves is started with no credential.
//
//   node dev/provider.mjs [--port N] [--pace seconds]   prints {"provider":"http://127.0.0.1:<port>"}
//
// What it answers follows the task's words:
//   "Write note.txt …"     a cell that writes note.txt (Ask puts it to you), then an answer
//   "Fix the note …"       reasoning, a cell written slowly that runs a while, then writes note.txt
//   "Answer at once …"     an answer
//   "Take your time …"     an answer, after three seconds
//   "Two steps …"          after two seconds a cell that only prints, then an answer
//   "Ask me …"             a cell that asks which of two, then an answer naming the choice
//   anything else          an answer

import http from "node:http";

const textOf = (content) => (typeof content === "string" ? content : Array.isArray(content) ? content.map((b) => b?.text ?? "").join("\n") : "");
const taskOf = (request) => textOf((request.messages || []).find((m) => m.role === "user")?.content);
const turnOf = (request) => (request.messages || []).filter((m) => m.role === "assistant").length;
const cell = (id, code, extra = []) => ({ role: "assistant", stop_reason: "tool_use", content: [...extra, { type: "tool_use", id, name: "execute_cell", input: { code } }] });
const answer = (turn, text) => cell(`end-${turn}`, `answer(${JSON.stringify(text)});`);

/** What the model says to `request`, and how long it waits before it starts. */
export function reply(request, pace = 0.8) {
  const task = taskOf(request), turn = turnOf(request);
  const usage = { input_tokens: 1200 + turn * 300, output_tokens: 80 };
  const said = (whole, wait = pace, gap = 0) => ({ whole: { ...whole, usage }, wait, gap });
  if (task.startsWith("Write note.txt")) {
    if (turn === 0) {
      return said(cell("cell-1", `await write({ path: "note.txt", content: "written by the desktop check" });\nconst back = await read({ path: "note.txt" });\nconsole.log(back.content ?? back);`,
        [{ type: "text", text: "I'll write the note first, then read it back." }]));
    }
    return said(answer(turn, "note.txt is written and reads back as it should."));
  }
  if (task.startsWith("Fix the note")) {
    if (turn === 0) {
      const code = `// Wait a moment, as a long build would, then write the note.\nconst started = Date.now();\nlet spins = 0;\nwhile (Date.now() - started < 1500) { spins++; }\nawait write({ path: "note.txt", content: "fixed" });\nconsole.log("spun " + (spins > 0));`;
      return said(cell("cell-1", code, [
        { type: "thinking", thinking: "The note needs fixing. I will wait a moment for the build, then write it.", signature: "fixture-signature" },
        { type: "text", text: "First the build, then the note." },
      ]), 1.0, 0.25);
    }
    return said(answer(turn, "The note is fixed."));
  }
  if (task.startsWith("Take your time")) return said(answer(turn, "Done, in my own time."), 3);
  if (task.startsWith("Ask me")) return turn === 0 ? said(cell("cell-1", `const pick = await ask("Which lantern?", ["copper", "slate"]);\nanswer("You chose " + pick + ".");`)) : said(answer(turn, "Asked and answered."));
  if (task.startsWith("Two steps")) return turn === 0 ? said(cell("cell-1", `console.log("step one");`), 2) : said(answer(turn, "Both steps are done."));
  return said(answer(turn, "Answered at once."), Math.min(pace, 0.3));
}

/** The answer as Server-Sent Events: each block in pieces, `gap` seconds apart. */
function events(whole) {
  const out = [{ type: "message_start", message: { role: "assistant", usage: whole.usage } }, { type: "ping" }];
  const pieces = (text, n) => { const size = Math.max(1, Math.ceil(text.length / n)); const all = []; for (let i = 0; i < text.length; i += size) all.push(text.slice(i, i + size)); return all; };
  whole.content.forEach((block, index) => {
    if (block.type === "text") {
      out.push({ type: "content_block_start", index, content_block: { type: "text", text: "" } });
      for (const p of pieces(block.text, 3)) out.push({ type: "content_block_delta", index, delta: { type: "text_delta", text: p } });
    } else if (block.type === "thinking") {
      out.push({ type: "content_block_start", index, content_block: { type: "thinking", thinking: "" } });
      for (const p of pieces(block.thinking, 4)) out.push({ type: "content_block_delta", index, delta: { type: "thinking_delta", thinking: p } });
      out.push({ type: "content_block_delta", index, delta: { type: "signature_delta", signature: block.signature } });
    } else {
      out.push({ type: "content_block_start", index, content_block: { type: "tool_use", id: block.id, name: block.name, input: {} } });
      for (const p of pieces(JSON.stringify(block.input), 5)) out.push({ type: "content_block_delta", index, delta: { type: "input_json_delta", partial_json: p } });
    }
    out.push({ type: "content_block_stop", index });
  });
  out.push({ type: "message_delta", delta: { stop_reason: whole.stop_reason } });
  out.push({ type: "message_stop" });
  return out;
}

export function startProvider({ pace = 0.8, port = 0 } = {}) {
  const seen = [];
  const server = http.createServer((req, res) => {
    let body = "";
    req.on("data", (d) => { body += d; });
    req.on("end", async () => {
      let request = {};
      try { request = JSON.parse(body || "{}"); } catch { /* not JSON */ }
      seen.push({ path: req.url, request });
      if (!String(req.url).includes("messages")) {
        res.writeHead(200, { "content-type": "application/json" });
        return res.end(JSON.stringify({ data: [] }));
      }
      const { whole, wait, gap } = reply(request, pace);
      await new Promise((ok) => setTimeout(ok, wait * 1000));
      if (!request.stream) {
        res.writeHead(200, { "content-type": "application/json" });
        return res.end(JSON.stringify(whole));
      }
      res.writeHead(200, { "content-type": "text/event-stream" });
      for (const e of events(whole)) {
        if (res.destroyed) return;
        res.write(`data: ${JSON.stringify(e)}\n\n`);
        if (gap && e.type === "content_block_delta") await new Promise((ok) => setTimeout(ok, gap * 1000));
      }
      res.end();
    });
  });
  return new Promise((ok) => server.listen(port, "127.0.0.1", () => ok({ url: `http://127.0.0.1:${server.address().port}`, server, seen })));
}

if (import.meta.url === `file://${process.argv[1]}`) {
  const flag = (n, d) => { const i = process.argv.indexOf(`--${n}`); return i >= 0 ? process.argv[i + 1] : d; };
  const { url } = await startProvider({ pace: Number(flag("pace", "0.8")), port: Number(flag("port", "0")) });
  console.log(JSON.stringify({ provider: url }));
}
