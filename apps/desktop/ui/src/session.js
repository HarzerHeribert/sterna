// One session as the app holds it: attached to its port, its state built
// from the snapshot and then every numbered event (docs/engine.md), the way
// the engine's hub keeps `wire::State`. The views draw from this and nothing
// else; commands go back as `{"do": …}` lines.

const WORKING = new Set(["starting", "thinking", "streaming", "executing", "searching", "waiting", "compacting", "awaiting_you"]);
export const working = (activity) => WORKING.has(activity);
export const ENDINGS = new Set(["complete", "failed", "stopped", "interrupted"]);

export function emptyState() {
  return {
    session: "", seq: 0,
    facts: { model: null, effort: "", level: "", root: "", subagents: null, settings_models: [] },
    conversation: { system: "", messages: [] },
    notebook: { cells: [] },
    served: {}, activity: "idle", since: Date.now(),
    streaming: { text: null, tool: null, reasoning: null },
    queue: [], prompts: [], notes: [], memory: [], hosts: null, sign_in: null, suggestions: [],
    reading: { cells: [], answer: null },
    usage: { input_tokens: 0, output_tokens: 0, reasoned_tokens: 0, requests: 0 },
  };
}

export class Session {
  constructor(app, { id, root, title = null }) {
    this.app = app;
    this.id = id;
    this.root = root;
    this.title = title;
    this.state = emptyState();
    this.conn = null;
    this.attached = false;
    this.ended = null;
    this.stamps = new Map();
    this.turnStart = null;
    this.reasoned = null;
    this.thinkingSince = null;
    this.stopAsked = false;
    this.lastSeq = 0;
    this.panel = null;
    this.signIn = null;
    this.hints = new Map();
  }

  get activity() { return this.state.activity; }
  get busy() { return working(this.state.activity); }
  get connected() { return !!this.conn && !this.conn.closed; }
  get waiting() { return this.state.activity === "awaiting_you" || this.state.prompts.some((p) => p.type !== "form"); }

  /**
   * The session no longer runs: a turn it was in is over, and nothing it
   * asked can be answered any more. Said once; the first reason stands.
   */
  gone(reason) {
    this.ended = this.ended || reason;
    const s = this.state;
    if (working(s.activity)) this.setActivity("interrupted", Date.now());
    s.streaming = { text: null, tool: null, reasoning: null };
    s.prompts = [];
    s.queue = [];
    this.app.promptsNow(this);
  }

  /**
   * Attaches to the session's port. A session is known by the id its port
   * welcomes with; `from` takes up again after the last event this window
   * saw, when a connection dropped.
   */
  async attach(address, token, from = null) {
    const conn = await this.app.engine.session(address, token);
    // One connection per session: one it replaces is closed, and its closing is not a drop.
    const was = this.conn;
    this.conn = conn;
    if (was && !was.closed) was.close();
    const said = conn.welcome?.session;
    if (said && said !== this.id) this.app.rekey(this, said);
    conn.listen((e) => this.event(e));
    let heard = false;
    const closed = () => {
      if (heard || this.conn !== conn) return;
      heard = true;
      this.attached = false;
      // A connection this window closed on purpose is not a drop.
      if (this.ended || this.closing) return this.app.sessionChanged(this);
      // A dropped connection is not an ended session: the window looks again.
      this.app.dropped(this);
    };
    conn.onClosed = closed;
    await conn.send(from ? { do: "attach", from } : { do: "attach" });
    // One that closed while it was being taken up is a drop like any other, and never counts as attached.
    if (conn.closed) closed();
    else if (this.conn === conn) this.attached = true;
  }

  /** Closes this window's connection on purpose: nothing takes it up again. */
  detach() { this.closing = true; this.conn?.close(); }

  send(command) {
    if (!this.conn || this.conn.closed) {
      this.app.say(this.ended ? "This session has ended" : "This session is not connected");
      return Promise.resolve(false);
    }
    return this.conn.send(command);
  }

  /** Applies one line from the session port. */
  event(e) {
    const s = this.state;
    const before = s.activity;
    let first = false;
    switch (e.kind) {
      case "snapshot":
        this.state = { ...emptyState(), ...e.state, streaming: { text: null, tool: null, reasoning: null, ...(e.state?.streaming || {}) } };
        this.app.promptsNow(this);
        if (this.state.session && this.state.session !== this.id) this.app.rekey(this, this.state.session);
        this.lastSeq = Math.max(this.lastSeq, this.state.seq || 0);
        if (working(this.state.activity)) this.turnStart = this.state.since;
        this.stampAll(e.at);
        break;
      case "transcript":
        s.conversation = e.conversation || s.conversation;
        s.notebook = e.notebook || s.notebook;
        if (e.served && Object.keys(e.served).length) s.served = e.served;
        s.reading = e.reading || s.reading;
        s.streaming = { text: null, tool: null, reasoning: null };
        if (e.activity) this.setActivity(e.activity, e.at);
        this.stampAll(e.at);
        break;
      case "activity": this.setActivity(e.activity, e.since ?? e.at); break;
      // The first piece of each changes what the status line and the list say; the rest only the live edge.
      case "delta": first = s.streaming.text == null; s.streaming.text = (s.streaming.text || "") + e.text; break;
      case "tool_delta": first = s.streaming.tool == null; s.streaming.tool = (s.streaming.tool || "") + e.text; break;
      case "reasoning": first = s.streaming.reasoning == null; s.streaming.reasoning = (s.streaming.reasoning || "") + e.text; break;
      case "prompt":
        s.prompts = [...s.prompts.filter((p) => p.id !== e.prompt.id), e.prompt];
        this.app.prompted(this, e.prompt);
        break;
      case "hint": this.hints.set(e.id, e.fits); break;
      case "settled":
        s.prompts = s.prompts.filter((p) => p.id !== e.id);
        this.app.settled(this, e.id);
        if (e.by && e.by !== "desktop") this.app.sayAbout(this, e.by === "session" ? "Sterna answered it" : `answered in the ${e.by}`);
        break;
      case "queue": s.queue = e.items || []; break;
      case "notice":
        s.notes = [...s.notes, e.text];
        this.app.noticed(this, e.text);
        break;
      case "panel": this.panel = e.panel; this.app.paneled(this, e.panel); break;
      case "facts": s.facts = { ...s.facts, ...e.facts }; break;
      case "usage": s.usage = e.usage; break;
      case "memory": s.memory = e.entries || []; break;
      case "hosts": s.hosts = e.hosts || []; break;
      case "sign_in": this.signedIn(e.sign_in); break;
      case "suggest":
        s.suggestions = [[e.label, e.types], ...s.suggestions.filter(([, t]) => t !== e.types)];
        break;
      case "unsuggest": s.suggestions = s.suggestions.filter(([, t]) => t !== e.types); break;
      case "refused": this.app.refused(this, e.to, e.reason); break;
      case "ended":
        this.gone(e.reason || "ended");
        break;
      default: break;
    }
    // A line for this client alone carries seq 0 and is never replayed.
    if (typeof e.seq === "number" && e.seq > 0) { this.lastSeq = Math.max(this.lastSeq, e.seq); this.state.seq = this.lastSeq; }
    if (before !== this.state.activity) this.app.activityChanged(this, before, this.state.activity);
    this.app.sessionChanged(this, first ? "first" : e.kind);
  }

  setActivity(activity, since) {
    const s = this.state, was = s.activity;
    if (activity === was && since == null) return;
    if (!working(was) && working(activity)) { this.turnStart = since ?? Date.now(); this.reasoned = null; this.stopAsked = false; }
    if (activity === "thinking") this.thinkingSince = since ?? Date.now();
    else if (was === "thinking" && this.thinkingSince) this.reasoned = (since ?? Date.now()) - this.thinkingSince;
    if (ENDINGS.has(activity) || activity === "idle") { this.turnEnd = since ?? Date.now(); this.stopAsked = false; }
    s.activity = activity;
    if (since != null) s.since = since;
  }

  /** When each message was first seen, for the times under them. */
  stampAll(at) {
    (this.state.conversation.messages || []).forEach((_, i) => { if (!this.stamps.has(i)) this.stamps.set(i, this.attachedAt ? at : null); });
    this.attachedAt = this.attachedAt || at;
  }

  signedIn(step) {
    if (!step) return;
    if (step.step === "started") this.signIn = { label: step.label, notes: [], panel: null, error: "" };
    else if (step.step === "note") { this.signIn = this.signIn || { label: "", notes: [], panel: null }; this.signIn.notes.push(step.text); }
    else if (step.step === "panel") { this.signIn = this.signIn || { label: "", notes: [], panel: null }; this.signIn.panel = step.panel; }
    else if (step.step === "done") this.signIn = null;
    this.app.signInChanged(this, step);
  }

  // -- what a person does -------------------------------------------------
  submit(text) { return this.send({ do: "submit", text }); }
  takeBack() { return this.send({ do: "take_back" }); }
  stop() { this.stopAsked = true; return this.send({ do: "stop" }); }
  cancel() { return this.send({ do: "cancel" }); }
  answer(prompt, answer) { return this.send({ do: "answer", prompt, answer }); }
  control(line) { return this.send({ do: "control", line }); }
  setLevel(level, save = true) { return this.send({ do: "set_level", level, save }); }
  forget(id) { return this.send({ do: "forget", id }); }
  host(host, allow) { return this.send({ do: "host", host, allow }); }
  signInPaste(text) { return this.send({ do: "sign_in_paste", text }); }
  signInCancel() { return this.send({ do: "sign_in_cancel" }); }
  rollback() { return this.send({ do: "rollback" }); }
  resume(id) { return this.send({ do: "resume", id }); }
  end() { return this.send({ do: "end" }); }
}
