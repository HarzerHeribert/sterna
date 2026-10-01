// A mock of `sterna host` and the sessions it starts, speaking the same
// protocol (docs/engine.md): loopback TCP, one JSON object per line, a hello
// with the token first. Its sessions play back scripted turns (plans.mjs)
// so the desktop app can be developed and tested before the real engine
// runs. Nothing here reaches a model, a network or the user's data folder.

import net from "node:net";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { randomBytes } from "node:crypto";
import * as R from "./record.mjs";
import * as plans from "./plans.mjs";

const PROTOCOL = 1;
const hexToken = () => randomBytes(32).toString("hex");
const newId = () => randomBytes(4).toString("hex").slice(0, 6);
const now = () => Date.now();
const firstLine = (text) => (text.split("\n").map((l) => l.trim()).find(Boolean) || "").slice(0, 120);
const LEVELS = {
  ask: ["Ask", "Every edit and command asks first. Nothing leaves the project."],
  sandboxed: ["Sandboxed", "Everything in the project runs. Leaving the sandbox asks."],
  full: ["Full access", "No sandbox, nothing asks. Refused commands stay refused."],
};
const STOPPED = Symbol("stopped"), ENDED = Symbol("ended"), CANCELLED = Symbol("cancelled");

/** Reads newline-delimited lines from a socket. */
function onLines(socket, fn) {
  let buf = "";
  socket.setEncoding("utf8");
  socket.on("data", (chunk) => {
    buf += chunk;
    let at;
    while ((at = buf.indexOf("\n")) >= 0) {
      const line = buf.slice(0, at);
      buf = buf.slice(at + 1);
      if (line.trim()) fn(line);
    }
  });
}
const write = (socket, value) => { if (!socket.destroyed) socket.write(JSON.stringify(value) + "\n"); };

/** The hello every port reads first; `then` gets the hello and every later line. */
function greet(socket, token, welcome, then) {
  let first = true;
  onLines(socket, (line) => {
    if (!first) return then.line(line);
    first = false;
    let hello = null;
    try { hello = JSON.parse(line).hello; } catch { /* not JSON */ }
    const refuse = (reason) => { write(socket, { refused: { reason } }); socket.end(); };
    if (!hello) return refuse("the first line must be a hello");
    if (hello.protocol !== PROTOCOL) return refuse(`this session speaks protocol ${PROTOCOL}`);
    if (hello.token !== token) return refuse("wrong token");
    write(socket, welcome);
    then.hello(hello);
  });
}

/** How a running session stands, in the list's words (engine/host.rs::live_word). */
function liveWord(activity) {
  if (["thinking", "compacting", "waiting", "starting"].includes(activity)) return "thinking";
  if (activity === "streaming") return "writing";
  if (["executing", "searching"].includes(activity)) return "running";
  if (activity === "awaiting_you") return "waiting";
  return "idle";
}

const CATALOGUE = {
  groups: [
    { provider: "openai", account: "ChatGPT Pro", scope: "subscription", models: ["gpt-6.1-sol", "gpt-6.1", "gpt-6-codex", "gpt-6.1-mini"], selectable: true, unavailable_reason: null, connect: null, pooled: true, note: "Pro · 12% of this week used" },
    { provider: "anthropic", account: "Claude Max", scope: "subscription", models: ["claude-opus-5-5", "claude-sonnet-5-5", "claude-haiku-4-5"], selectable: false, unavailable_reason: "Not connected · sign in above", connect: "anthropic", pooled: null, note: null },
    { provider: "google", account: "Google", scope: "api key", models: ["gemini-3.1-pro", "gemini-3.1-flash"], selectable: true, unavailable_reason: null, connect: null, pooled: null, note: null },
    { provider: "deepseek", account: "DeepSeek", scope: "api key", models: ["deepseek-v4", "deepseek-v4-flash"], selectable: true, unavailable_reason: null, connect: null, pooled: null, note: null },
  ],
  intelligence: { "gpt-6.1-sol": 74, "gpt-6.1": 71, "gpt-6-codex": 66, "gpt-6.1-mini": 58, "claude-opus-5-5": 77, "claude-sonnet-5-5": 72, "claude-haiku-4-5": 55, "gemini-3.1-pro": 73, "deepseek-v4": 63 },
};

class Session {
  constructor(host, { id, root, title = null, lastUsed = now(), model = "gpt-6.1-sol" }) {
    this.host = host;
    this.id = id;
    this.root = root;
    this.title = title;
    this.lastUsed = lastUsed;
    this.token = hexToken();
    this.seq = 0;
    this.log = [];
    this.clients = new Set();
    this.pending = new Map();
    this.promptCount = 0;
    this.busy = false;
    this.ended = false;
    this.turns = 0;
    this.warm = 0;
    this.prose = null;
    this.state = {
      session: id, seq: 0,
      facts: {
        model, effort: "low", level: "sandboxed", root, project: path.basename(root),
        sandbox: process.platform === "darwin" ? "macOS Seatbelt" : process.platform === "linux" ? "Landlock and seccomp" : "AppContainer",
        confinement: `${root} and temporary files`, network: "Allowed hosts only", subagents: "gpt-6.1-mini",
        settings_models: ["gpt-6.1-sol", "gpt-6.1-mini"], reasoning: true,
      },
      conversation: { system: "", messages: [] },
      notebook: { cells: [], tokens: null, context: { used: 4100, cap: 272000 }, requests: [] },
      served: {}, activity: "idle", since: now(),
      streaming: { text: null, tool: null, reasoning: null },
      queue: [], prompts: [], notes: [], memory: [], hosts: null, sign_in: null,
      suggestions: [["Run the tests", "Run the tests and tell me what fails."], ["Explain", `Explain how ${path.basename(root)} is laid out, and where to start reading.`]],
      reading: { cells: [], answer: null },
      usage: { input_tokens: 0, output_tokens: 0, reasoned_tokens: 0, requests: 0 },
    };
  }

  get pace() { return this.warm > 0 ? 0.01 : this.host.pace; }

  async listen() {
    this.server = net.createServer((socket) => this.serve(socket));
    await new Promise((ok) => this.server.listen(0, "127.0.0.1", ok));
    this.listening = `127.0.0.1:${this.server.address().port}`;
  }

  serve(socket) {
    const client = { socket, name: "", attached: false };
    socket.on("error", () => {});
    socket.on("close", () => this.clients.delete(client));
    greet(socket, this.token, { welcome: { protocol: PROTOCOL, session: this.id } }, {
      hello: (hello) => { client.name = hello.client || ""; this.clients.add(client); },
      line: (line) => {
        let command;
        try { command = JSON.parse(line); } catch (e) { return this.refuse(client, "", `not a command this session takes: ${e.message}`); }
        this.command(client, command);
      },
    });
  }

  refuse(client, to, reason) { this.emit("refused", { to, reason }, client); }

  /** One event: numbered, kept for a client that attaches later, sent to every attached client. */
  emit(kind, fields = {}, only = null) {
    const envelope = { seq: ++this.seq, at: now(), kind, ...fields };
    this.apply(envelope);
    this.state.seq = this.seq;
    if (!only) this.log.push(envelope);
    for (const client of this.clients) if (client.attached && (!only || client === only)) write(client.socket, envelope);
    this.host.changed();
  }

  apply(e) {
    const s = this.state;
    switch (e.kind) {
      case "transcript":
        s.conversation = e.conversation; s.notebook = e.notebook; s.reading = e.reading;
        s.streaming = { text: null, tool: null, reasoning: null };
        if (e.activity) s.activity = e.activity;
        break;
      case "activity": s.activity = e.activity; s.since = e.since; break;
      case "delta": s.streaming.text = (s.streaming.text || "") + e.text; break;
      case "tool_delta": s.streaming.tool = (s.streaming.tool || "") + e.text; break;
      case "reasoning": s.streaming.reasoning = (s.streaming.reasoning || "") + e.text; break;
      case "prompt": s.prompts.push(e.prompt); break;
      case "settled": s.prompts = s.prompts.filter((p) => p.id !== e.id); break;
      case "queue": s.queue = e.items; break;
      case "notice": s.notes.push(e.text); break;
      case "facts": s.facts = e.facts; break;
      case "usage": s.usage = e.usage; break;
      case "memory": s.memory = e.entries; break;
      case "hosts": s.hosts = e.hosts; break;
    }
  }

  attach(client, from) {
    client.attached = true;
    if (from != null) {
      for (const e of this.log) if (e.seq >= from) write(client.socket, e);
      return;
    }
    write(client.socket, { seq: this.seq, at: now(), kind: "snapshot", state: this.state });
  }

  command(client, c) {
    const busyOnly = (to) => { if (!this.busy) { this.refuse(client, to, "no turn is running"); return false; } return true; };
    switch (c.do) {
      case "attach": return this.attach(client, c.from);
      case "submit":
        if (c.images?.length) return this.refuse(client, "submit", "this session takes no images yet");
        if (!String(c.text || "").trim()) return this.refuse(client, "submit", "there is nothing to send");
        return this.submit(client, c.text);
      case "take_back":
        if (!this.state.queue.length) return this.refuse(client, "take_back", "nothing is queued");
        return this.emit("queue", { items: this.state.queue.slice(0, -1) });
      case "stop":
        if (!busyOnly("stop")) return;
        this.stopping = true;
        return this.emit("notice", { text: "Stops after this cell · what ran stands" });
      case "cancel":
        if (!busyOnly("cancel")) return;
        this.cancelling = true;
        return this.emit("notice", { text: "Cancelled the call in flight · what ran stands" });
      case "answer": return this.answer(client, c.prompt, c.answer);
      case "control": return this.control(client, String(c.line || ""));
      case "set_level":
        if (c.save && LEVELS[String(c.level || "").toLowerCase()]) this.host.settings["sandbox.level"] = String(c.level).toLowerCase();
        return this.setLevel(client, c.level);
      case "forget":
        this.emit("memory", { entries: this.state.memory.filter((m) => m.id !== c.id) });
        return this.emit("notice", { text: "Forgotten · the next identical call asks again" });
      case "host": {
        const hosts = new Set(this.state.hosts || []);
        if (c.allow) hosts.add(c.host); else hosts.delete(c.host);
        return this.emit("hosts", { hosts: [...hosts] });
      }
      case "sign_in_paste": return this.signInPaste(client, String(c.text || ""));
      case "sign_in_cancel":
        this.state.sign_in = null;
        this.emit("sign_in", { sign_in: { step: "done" } }, client);
        return this.emit("notice", { text: "Sign-in cancelled · no account was added" });
      case "rollback": return this.rollback(client);
      // As the hub: only the terminal ends its session to start another in its place.
      case "resume": return client.name === "terminal" ? this.resume(client, c.id) : this.refuse(client, "resume", "only the terminal resumes a session in its place; a client reopens one with the host's start");
      case "end": return this.end(`ended by ${client.name || "a client"}`);
      default: return this.refuse(client, c.do || "", `not a command this session takes: unknown variant \`${c.do}\``);
    }
  }

  submit(client, text) {
    if (text.trim().startsWith("/")) return this.control(client, text);
    if (this.busy) return this.emit("queue", { items: [...this.state.queue, text] });
    this.run(text);
  }

  messages() { return this.state.conversation.messages; }
  cells() { return this.state.notebook.cells; }

  transcript(activity) {
    const notebook = this.state.notebook;
    this.emit("transcript", {
      conversation: { system: "", messages: [...this.messages()] },
      notebook: { ...notebook, cells: notebook.cells.map((c) => ({ ...c })) },
      served: { provider: "openai", model: this.state.facts.model },
      activity: activity ?? null,
      reading: R.reading(notebook),
    });
  }

  activity(activity) { this.emit("activity", { activity, since: now() }); }

  /** Waits `seconds` at this session's pace; a cancel or an end cuts it short. */
  sleep(seconds, { cancellable = false } = {}) {
    const until = now() + seconds * this.pace * 1000;
    return new Promise((ok, fail) => {
      const tick = () => {
        if (this.ended) return fail(ENDED);
        if (cancellable && this.cancelling) return fail(CANCELLED);
        if (now() >= until) return ok();
        setTimeout(tick, Math.min(50, Math.max(1, until - now())));
      };
      tick();
    });
  }

  /** Sends `text` in pieces over `seconds`, as `kind` events. */
  async drip(kind, text, seconds) {
    const pieces = Math.max(1, Math.min(60, Math.ceil(text.length / 14)));
    const size = Math.ceil(text.length / pieces);
    for (let i = 0; i < text.length; i += size) {
      this.emit(kind, { text: text.slice(i, i + size) });
      await this.sleep(seconds / pieces);
    }
  }

  async run(text, plan, { seeded = false } = {}) {
    this.busy = true;
    this.stopping = false;
    this.cancelling = false;
    if (!this.title) this.title = firstLine(text);
    if (!seeded) {
      this.lastUsed = now();
      this.host.used(this);
    }
    this.messages().push(R.user(text));
    this.transcript("thinking");
    this.activity("thinking");
    plan = plan || (text.startsWith("Too large") ? plans.tooLarge : this.turns === 0 ? plans.tally : plans.followup(text));
    this.turns++;
    let ending = "complete";
    try {
      for (let i = 0; i < plan.length; i++) {
        if (this.stopping && i > 0) { ending = "stopped"; break; }
        const said = await this.step(plan[i]);
        if (this.warm > 0) this.warm--;
        if (said === "answered") break;
      }
    } catch (e) {
      if (e === ENDED) return;
      ending = e === CANCELLED ? "stopped" : "failed";
      if (e !== CANCELLED) this.emit("notice", { text: String(e?.message || e) });
    }
    this.transcript(ending);
    this.activity(ending);
    this.busy = false;
    if (this.state.queue.length && !this.ended) {
      const [next, ...rest] = this.state.queue;
      this.emit("queue", { items: rest });
      setTimeout(() => this.run(next), 30);
    }
  }

  async step(step) {
    if (step.wait) return this.sleep(step.wait);
    if (step.slow) { this.slow = step.slow; return; }
    if (step.reason) {
      if (this.state.activity !== "thinking") this.activity("thinking");
      return this.drip("reasoning", step.reason, 1.6);
    }
    if (step.prose) {
      this.activity("streaming");
      this.prose = step.prose;
      return this.drip("delta", step.prose, 1.2);
    }
    if (step.cell) return this.cell(step.cell);
  }

  async cell(c) {
    const n = this.cells().length + 1, id = `cell-${this.id}-${n}`;
    if (this.state.activity !== "streaming") this.activity("streaming");
    const written = JSON.stringify({ description: c.description, code: c.code });
    await this.drip("tool_delta", written, this.slow || 1.8);
    this.slow = 0;
    this.messages().push(R.assistant(this.prose, id, c.code));
    this.prose = null;
    this.cells().push(R.started(c.description));
    this.transcript("executing");
    this.activity("executing");
    let outcome = c;
    if (c.approval) {
      await this.sleep(0.6);
      const prompt = { id: ++this.promptCount, type: "approval", root: this.root, complete: true, fits: null, ...c.approval };
      if (c.approval.complete === false) prompt.complete = false;
      const answered = this.waitFor(prompt.id);
      this.emit("prompt", { prompt });
      this.activity("awaiting_you");
      const answer = await answered;
      const word = answer?.approval;
      const allowed = ["allow_once", "allow_for_session", "allow_host_session", "allow_host_always"].includes(word);
      if (word === "allow_for_session" || word === "allow_host_session" || word === "allow_host_always") {
        const host = c.approval.hosts[0];
        this.emit("memory", { entries: [...this.state.memory, { id: `host:${host}`, label: host, allowed: true }] });
        this.emit("hosts", { hosts: [...new Set([...(this.state.hosts || []), host])] });
      }
      if (word === "deny") {
        this.emit("memory", { entries: [...this.state.memory, { id: `call:${c.approval.target}`, label: c.approval.target, allowed: false }] });
      }
      if (!allowed) outcome = { ...c, ...c.denied, stdout: null, returned: null, changes: null };
      this.activity("executing");
    }
    await this.sleep(outcome.error ? 0.2 : c.runFor ?? 1, { cancellable: true }).catch((e) => {
      if (e !== CANCELLED) throw e;
      outcome = { ...c, calls: [], stdout: null, returned: null, changes: null, error: { class: "Cancelled", message: "the call in flight was cancelled" } };
    });
    const view = {
      ...this.cells()[n - 1],
      execution: R.execution(outcome.calls || []),
      call_count: (outcome.calls || []).length,
      stdout: outcome.stdout ?? null,
      changes: outcome.changes ?? null,
      returned: outcome.returned ?? null,
      error: outcome.error ?? null,
      answered: true,
    };
    this.cells()[n - 1] = view;
    this.messages().push(R.result(id, outcome.error ? `${outcome.error.class}: ${outcome.error.message}` : outcome.stdout || "ok", !!outcome.error));
    const u = this.state.usage;
    this.emit("usage", { usage: { input_tokens: u.input_tokens + 9000 + n * 2400, output_tokens: u.output_tokens + 410, reasoned_tokens: u.reasoned_tokens + 160, requests: u.requests + 1 } });
    this.state.notebook.tokens = { used: this.state.usage.input_tokens + this.state.usage.output_tokens, counted: "Gateway", reasoned: this.state.usage.reasoned_tokens };
    this.state.notebook.context = { used: 4100 + n * 7800, cap: 272000 };
    if (outcome.error?.class === "Cancelled") { this.transcript(null); throw CANCELLED; }
    if (outcome.returned != null) { this.messages().push(R.said(outcome.returned)); return "answered"; }
    this.transcript("thinking");
    if (outcome.then) {
      this.activity("thinking");
      await this.sleep(0.5);
      return this.cell(outcome.then);
    }
    this.activity("thinking");
    await this.sleep(0.4);
  }

  waitFor(id) {
    return new Promise((ok) => this.pending.set(id, ok));
  }

  answer(client, id, answer) {
    const waiting = this.pending.get(id);
    if (!waiting) {
      return this.refuse(client, "answer", id > 0 && id <= this.promptCount ? "it was already answered" : "no prompt waits with that id");
    }
    this.pending.delete(id);
    this.emit("settled", { id, by: client.name || "desktop", answer });
    waiting(answer);
  }

  control(client, line) {
    const [word, ...rest] = line.trim().replace(/^\//, "").split(/\s+/);
    const arg = rest.join(" ");
    const facts = () => this.emit("facts", { facts: { ...this.state.facts } });
    switch (word) {
      case "model": {
        if (!arg) return this.emit("panel", { panel: this.modelsPanel() }, client);
        const [tier, model] = rest.length > 1 ? rest : ["parent", rest[0]];
        if (tier === "subagent" || tier === "subagents") {
          this.state.facts.subagents = model; facts();
          this.host.settings["agents.model"] = model;
          return this.emit("notice", { text: `Subagent model set to ${model}` });
        }
        this.state.facts.model = model; facts();
        this.host.settings["model.parent"] = model;
        return this.emit("notice", { text: `Main model set to ${model}` });
      }
      case "models": return this.emit("panel", { panel: this.modelsPanel() }, client);
      case "effort":
        if (!["auto", "low", "medium", "high", "xhigh", "max"].includes(arg)) return this.emit("notice", { text: "Use /effort auto|low|medium|high|xhigh|max" });
        this.state.facts.effort = arg; facts();
        return this.emit("notice", { text: `Effort is now ${arg}` });
      case "sandbox": return this.setLevel(client, arg);
      case "login": return this.signIn(client, arg || "anthropic");
      case "status":
        return this.emit("panel", { panel: { title: "Status", rows: [
          { id: null, text: "Model", kind: "Info", action: null, value: this.state.facts.model },
          { id: null, text: "Effort", kind: "Info", action: null, value: this.state.facts.effort },
          { id: null, text: "Sandbox", kind: "Info", action: null, value: LEVELS[this.state.facts.level]?.[0] },
          { id: null, text: "ChatGPT Pro", kind: "Info", action: null, value: "12% of this week used" },
        ], selected: 0, assignment: null, catalogue: null, back: null } }, client);
      default:
        return this.emit("notice", { text: `The mock engine does not run /${word}` });
    }
  }

  modelsPanel() {
    return {
      title: "Models", rows: [], selected: 0, back: null,
      assignment: { active: "parent", models: { parent: this.state.facts.model, subagent: this.state.facts.subagents } },
      catalogue: { ...CATALOGUE, groups: CATALOGUE.groups.map((g) => (this.host.connected.has(g.connect) ? { ...g, selectable: true, connect: null, unavailable_reason: null } : g)) },
    };
  }

  setLevel(client, level) {
    const word = String(level || "").toLowerCase().replace("full access", "full");
    if (!LEVELS[word]) return this.refuse(client, "set_level", `no level ${level}: ask, sandboxed or full`);
    this.state.facts.level = word;
    this.emit("facts", { facts: { ...this.state.facts } });
    this.emit("notice", { text: `Sandbox is now ${LEVELS[word][0]} · ${LEVELS[word][1]}` });
  }

  signIn(client, provider) {
    const label = provider === "anthropic" ? "Claude Max" : provider;
    this.signingIn = provider;
    this.state.sign_in = label;
    this.emit("sign_in", { sign_in: { step: "started", label } }, client);
    this.emit("sign_in", { sign_in: { step: "panel", panel: { title: `Sign in to ${label}`, selected: 0, assignment: null, catalogue: null, back: null, rows: [
      { id: null, text: "Open the sign-in page in your browser", kind: "Info", action: null, value: null },
      { id: null, text: "https://claude.ai/oauth/authorize?client_id=sterna&response_type=code&state=4f1c9a", kind: "Open", action: { Path: "https://claude.ai/oauth/authorize?client_id=sterna&response_type=code&state=4f1c9a" }, value: null },
      { id: null, text: "Sign in there, then paste the address the page sends you to", kind: "Info", action: null, value: null },
    ] } } }, client);
  }

  signInPaste(client, text) {
    if (!this.state.sign_in) return this.refuse(client, "sign_in_paste", "no sign-in is running");
    if (!/code=/.test(text)) return this.emit("sign_in", { sign_in: { step: "note", text: "That address has no code in it. Paste the whole address the page sent you to." } }, client);
    const label = this.state.sign_in;
    this.host.connected.add(this.signingIn);
    this.state.sign_in = null;
    this.emit("sign_in", { sign_in: { step: "done" } }, client);
    this.emit("notice", { text: `Signed in to ${label}` });
  }

  rollback(client) {
    const cells = this.cells();
    const at = cells.map((c, i) => [c, i]).reverse().find(([c]) => c.changes && !c.rolled_back);
    if (!at) return this.refuse(client, "rollback", "no cell changed files");
    cells[at[1]] = { ...at[0], rolled_back: true };
    this.transcript(null);
    this.emit("notice", { text: `Rolled back cell ${String(at[1] + 1).padStart(3, "0")} · its changes are undone` });
  }

  /** Takes a finished session's record, as `sterna session --serve --resume <id>` does. */
  load(record) {
    this.state.conversation = { system: "", messages: [...record.messages] };
    this.state.notebook = { ...this.state.notebook, cells: record.cells.map((c) => ({ ...c })) };
    this.state.reading = R.reading(this.state.notebook);
    this.turns = 1;
  }

  resume(client, id) {
    const old = this.host.sessions.get(id);
    if (!old || !old.record) return this.refuse(client, "resume", `no session ${id} in this folder`);
    this.host.alias(this, old);
    this.state.conversation = { system: "", messages: [...old.record.messages] };
    this.state.notebook = { ...this.state.notebook, cells: old.record.cells.map((c) => ({ ...c })) };
    this.turns = 1;
    this.transcript("idle");
    this.emit("notice", { text: `Resumed session ${id}` });
  }

  end(reason) {
    if (this.ended) return;
    this.emit("ended", { reason });
    this.ended = true;
    const entry = this.host.sessions.get(this.id);
    if (entry) entry.record = { messages: [...this.messages()], cells: this.cells().map((c) => ({ ...c })) };
    for (const client of this.clients) client.socket.end();
    this.server?.close();
    this.host.changed();
  }

  /** Plays `plan` with no waits: a finished record, for the list's history. */
  static record(title, plan) {
    const messages = [R.user(title)], cells = [];
    plan.forEach(({ cell }, i) => {
      if (!cell) return;
      const id = `h${i}`;
      messages.push(R.assistant(null, id, cell.code));
      cells.push({ ...R.started(cell.description), execution: R.execution(cell.calls), call_count: cell.calls.length, stdout: cell.stdout ?? null, returned: cell.returned ?? null, answered: true });
      messages.push(R.result(id, cell.stdout || "ok"));
      if (cell.returned != null) messages.push(R.said(cell.returned));
    });
    return { messages, cells };
  }
}

class Host {
  constructor({ pace = 1 }) {
    this.token = hexToken();
    this.pace = pace;
    this.sessions = new Map();
    this.folders = new Map();
    this.watchers = new Set();
    this.connected = new Set(["openai"]);
    this.quits = [];
    this.settings = { "model.parent": "gpt-6.1-sol", "session.effort": "low", "sandbox.level": "sandboxed", "ui.theme": "amazon" };
    this.preferences = {};
    this.keys = [];
  }

  /** The gateway's accounts, as `entitlements --json` lists them. */
  accounts() {
    const row = (account, provider, models, connect) => ({
      account, provider, models, scope: "account-declared", selectable: true, unavailable_reason: null,
      authenticated: connect ? this.connected.has(connect) : null, connect_with: connect, pooled: true,
    });
    return [
      row("chatgpt-pro", "openai", ["gpt-6-codex", "gpt-6.1", "gpt-6.1-mini", "gpt-6.1-sol"], "openai"),
      row("claude-max", "anthropic", ["claude-haiku-4-5", "claude-opus-5-5", "claude-sonnet-5-5"], "anthropic"),
      row("google", "google", ["gemini-3.1-flash", "gemini-3.1-pro"], null),
      ...this.keys.map((p) => row(p, p, [], null)),
    ];
  }

  setSetting(key, value) {
    const allowed = {
      "sandbox.level": ["ask", "sandboxed", "full"], "session.effort": ["auto", "low", "medium", "high", "xhigh", "max"],
      "ui.motion": ["full", "calm", "off"], "ui.stream": ["actions", "code", "raw"],
      "ui.theme": ["neon", "amber", "ice", "mono", "violet", "cobalt", "mint", "rose", "amazon", "sun-conure", "hyacinth", "scarlet", "blue-gold", "green-wing", "military", "cockatoo", "arctic-tern"],
    };
    if (allowed[key] && !allowed[key].includes(value)) throw new Error(`${key} takes ${allowed[key].join(", ")}, not ${value}`);
    if (!/^[a-z_]+(\.[a-z_]+)+$/.test(key)) throw new Error(`there is no setting ${key}`);
    this.settings[key] = value;
    for (const s of this.sessions.values()) {
      if (key === "sandbox.level" && s.live && !s.live.ended) { /* a running session keeps its own level, as the real one does */ }
    }
  }

  /** A sign-in on this connection: progress lines, a pasted address, a cancel. */
  signIn(socket, provider) {
    const label = provider === "anthropic" ? "Claude Max" : provider;
    write(socket, { ok: {} });
    write(socket, { sign_in: { state: "opened", provider, authorize_url: `https://claude.ai/oauth/authorize?client_id=sterna&response_type=code&state=4f1c9a&for=${encodeURIComponent(label)}` } });
    return (line) => {
      if (line.cancel === true) { write(socket, { done: { connected: false } }); return true; }
      if (typeof line.paste === "string") {
        if (!/code=/.test(line.paste)) { write(socket, { sign_in: { state: "waiting", message: "That address has no code in it. Paste the whole address the page sent you to." } }); return false; }
        this.connected.add(provider);
        write(socket, { sign_in: { state: "connected", account: "me@example.com" } });
        write(socket, { done: { connected: true } });
        return true;
      }
      return false;
    };
  }

  async listen() {
    this.server = net.createServer((socket) => this.serve(socket));
    await new Promise((ok) => this.server.listen(0, "127.0.0.1", ok));
    this.listening = `127.0.0.1:${this.server.address().port}`;
  }

  used(session) {
    const entry = this.sessions.get(session.id) || {};
    this.sessions.set(session.id, { ...entry, id: session.id, root: session.root, title: session.title, lastUsed: session.lastUsed, live: session });
    const folder = this.folders.get(session.root) || { root: session.root, lastUsed: 0 };
    folder.lastUsed = Math.max(folder.lastUsed, session.lastUsed);
    this.folders.set(session.root, folder);
    this.changed();
  }

  alias(session, old) {
    this.sessions.delete(session.id);
    session.id = old.id;
    session.title = old.title;
    session.lastUsed = now();
    this.used(session);
  }

  list() {
    const live = (s) => (s.live && !s.live.ended ? { state: liveWord(s.live.state.activity), since: s.live.state.since } : null);
    return {
      folders: [...this.folders.values()].sort((a, b) => b.lastUsed - a.lastUsed).map((f) => ({
        root: f.root, last_used: f.lastUsed,
        sessions: [...this.sessions.values()].filter((s) => s.root === f.root && s.title).sort((a, b) => b.lastUsed - a.lastUsed)
          .map((s) => ({ id: s.id, title: s.title, last_used: s.lastUsed, live: live(s) })),
      })).filter((f) => f.sessions.length),
    };
  }

  changed() {
    if (this.pendingChange) return;
    this.pendingChange = setTimeout(() => {
      this.pendingChange = null;
      const list = JSON.stringify({ list: this.list() });
      for (const w of this.watchers) if (w.last !== list) { w.last = list; w.socket.write(list + "\n"); }
    }, 60);
  }

  serve(socket) {
    socket.on("error", () => {});
    socket.on("close", () => { for (const w of this.watchers) if (w.socket === socket) this.watchers.delete(w); });
    greet(socket, this.token, { welcome: { protocol: PROTOCOL, host: "0.0.0-mock" } }, {
      hello: () => {},
      line: async (line) => {
        let command;
        try { command = JSON.parse(line); } catch (e) { return write(socket, { error: `not a command: ${e.message}` }); }
        if (socket.signingIn) { if (socket.signingIn(command)) socket.signingIn = null; return; }
        if (command.do === "sign_in") {
          if (!command.provider) return write(socket, { error: "sign_in needs a provider" });
          socket.signingIn = this.signIn(socket, command.provider);
          return;
        }
        if (command.do === "watch") {
          write(socket, { ok: {} });
          const watcher = { socket, last: JSON.stringify({ list: this.list() }) };
          socket.write(watcher.last + "\n");
          this.watchers.add(watcher);
          return;
        }
        try {
          write(socket, { ok: await this.answer(command) });
        } catch (e) {
          write(socket, { error: String(e?.message || e) });
        }
        if (command.do === "shutdown") this.shutdown();
      },
    });
  }

  async answer(c) {
    switch (c.do) {
      case "list": return this.list();
      case "start": return this.start(c);
      case "locate": {
        const s = this.sessions.get(c.id)?.live;
        if (!s || s.ended) throw new Error(`session ${c.id} is not running`);
        return { id: s.id, listening: s.listening, token: s.token };
      }
      case "stop": this.sessions.get(c.id)?.live?.end("stopped by the host"); return {};
      case "usage": {
        const total = { input_tokens: 0, output_tokens: 0, requests: 0 };
        const sessions = [...this.sessions.values()].filter((s) => s.live).map((s) => {
          const u = s.live.state.usage;
          total.input_tokens += u.input_tokens; total.output_tokens += u.output_tokens; total.requests += u.requests;
          return { id: s.id, root: s.root, input_tokens: u.input_tokens, output_tokens: u.output_tokens, requests: u.requests };
        });
        return { sessions, total };
      }
      case "quit":
        this.quits.push(c.keep === true);
        if (c.keep !== true) for (const s of this.sessions.values()) s.live?.end("the desktop app quit");
        return {};
      case "shutdown": return {};
      case "settings": return { values: { ...this.settings } };
      case "set_setting": this.setSetting(String(c.key || ""), String(c.value ?? "")); return {};
      case "preferences": return this.preferences;
      case "set_preferences":
        if (!c.preferences || typeof c.preferences !== "object" || Array.isArray(c.preferences)) throw new Error("preferences are one object");
        this.preferences = c.preferences;
        return {};
      case "accounts": return { accounts: this.accounts() };
      case "set_key":
        if (!c.provider) throw new Error("set_key needs a provider");
        if (!c.key) throw new Error("set_key needs a key");
        if (!this.keys.includes(c.provider)) this.keys.push(c.provider);
        // The key itself goes nowhere: the mock keeps only that one was given.
        return { provider: c.provider, stored_in: "credentials.toml" };
      case "mock_quits": return { quits: this.quits };
      default: throw new Error(`the host takes no command "${c.do}"`);
    }
  }

  async start({ root, task, model, resume }) {
    if (typeof root !== "string" || !path.isAbsolute(root)) throw new Error(`${root} is not a folder`);
    // --any-folder (screenshots): a folder under the home folder need not exist.
    const known = this.folders.has(root) || (this.anyFolder && root.startsWith(os.homedir() + path.sep));
    if (!known && !(fs.existsSync(root) && fs.statSync(root).isDirectory())) throw new Error(`${root} is not a folder`);
    // Reopening: a running session is found; a finished one starts again under its own id, with its record.
    const old = resume ? this.sessions.get(resume) : null;
    if (resume && !old) throw new Error(`no session ${resume} in ${root}`);
    if (old?.live && !old.live.ended) return { id: old.id, listening: old.live.listening, token: old.live.token };
    const session = new Session(this, { id: old ? old.id : newId(), root, title: old?.title ?? null, lastUsed: old?.lastUsed ?? now(), model: model || this.settings["model.parent"] });
    session.state.facts.effort = this.settings["session.effort"] || "low";
    session.state.facts.level = this.settings["sandbox.level"] || "sandboxed";
    if (old?.record) session.load(old.record);
    await session.listen();
    this.sessions.set(session.id, { ...(old || {}), id: session.id, root, title: old?.title ?? null, lastUsed: old?.lastUsed ?? now(), live: session });
    if (task) session.run(task);
    return { id: session.id, listening: session.listening, token: session.token };
  }

  shutdown() {
    for (const s of this.sessions.values()) s.live?.end("the host shut down");
    setTimeout(() => { this.server.close(); this.onShutdown?.(); }, 50);
  }

  /** The busy scenario: four folders, eleven sessions, four of them running. */
  async seed(scenario) {
    if (scenario !== "busy") return;
    const H = os.homedir(), t = now();
    const folders = { quill: path.join(H, "code", "quill"), harbor: path.join(H, "code", "harbor-site"), notes: path.join(H, "work", "notes-api"), dot: path.join(H, "dotfiles") };
    const SESSIONS = [
      ["q8f4hs", "quill", "Document the CSV export in the README, with one example per format.", 600, "csvdocs", 1],
      ["k2m9qa", "harbor", "Fix the broken links in the docs, then rebuild the sitemap.", 8400, "links", 3],
      ["h7d2wp", "quill", "Add CSV export to report", 11000, "csvexport", 0],
      ["p4x8tz", "dot", "Why does zsh take two seconds to start?", 93000],
      ["c1v5rn", "quill", "Why does cli start slowly?", 108000],
      ["m9q3bd", "notes", "Add rate limiting to /search: 20 requests a minute per key, and a test that proves it.", 140000, "ratelimit", 4],
      ["a6j1ke", "harbor", "Move the blog to Astro 6", 207000],
      ["r2t8yf", "quill", "Rename Ledger to Book across the workspace", 276000],
      ["w5n0gc", "quill", "Bump rust_decimal to 1.36", 527000],
      ["e3s7lu", "notes", "Explain the auth middleware", 648000],
    ];
    for (const [id, f, title, ago, plan, warm] of SESSIONS) {
      const root = folders[f], lastUsed = t - ago * 1000;
      const folder = this.folders.get(root) || { root, lastUsed: 0 };
      folder.lastUsed = Math.max(folder.lastUsed, lastUsed);
      this.folders.set(root, folder);
      if (!plan) {
        this.sessions.set(id, { id, root, title, lastUsed, live: null, record: Session.record(title, plans.history(title)) });
        continue;
      }
      const session = new Session(this, { id, root, title, lastUsed });
      await session.listen();
      this.sessions.set(id, { id, root, title, lastUsed, live: session });
      session.warm = warm;
      session.turns = 1;
      session.run(title, plans.busy[plan], { seeded: true });
    }
    this.changed();
  }
}

/** Starts a mock host; `scenario` is "empty" (a first run) or "busy". */
export async function startMockHost({ scenario = "empty", pace = 1, data = null, anyFolder = false } = {}) {
  const host = new Host({ pace });
  host.anyFolder = anyFolder;
  await host.listen();
  await host.seed(scenario);
  const ready = { listening: host.listening, token: host.token, pid: process.pid, version: "0.0.0-mock", protocol: PROTOCOL };
  if (data) {
    fs.mkdirSync(data, { recursive: true, mode: 0o700 });
    fs.writeFileSync(path.join(data, "host.json"), JSON.stringify(ready), { mode: 0o600 });
    host.hostFile = path.join(data, "host.json");
  }
  const close = () => {
    host.shutdown();
    if (host.hostFile) fs.rmSync(host.hostFile, { force: true });
  };
  return { ready, host, close };
}
