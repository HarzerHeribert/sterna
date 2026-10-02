// The engine's two ports as the app speaks them (docs/engine.md): the host
// port -- the list, start, locate, stop, usage, quit, watch -- and a session
// port per session. Every connection says its hello first; every line is
// one JSON object.

export const PROTOCOL = 1;
export const CLIENT = "desktop";

/** One connection past its hello. */
class Conn {
  constructor(engine, id) {
    this.engine = engine;
    this.id = id;
    this.queue = [];
    this.waiters = [];
    this.onMessage = null;
    this.onClosed = null;
    this.closed = false;
  }
  line(text) {
    let value;
    try { value = JSON.parse(text); } catch { return; }
    if (this.waiters.length) return this.waiters.shift().ok(value);
    if (this.onMessage) return this.onMessage(value);
    this.queue.push(value);
  }
  end() {
    this.closed = true;
    for (const w of this.waiters.splice(0)) w.fail(new Error("the connection closed"));
    this.onClosed?.();
  }
  /** The next line, within `ms`; with `ms` null, whenever it comes or the connection closes. */
  next(ms = 15000) {
    if (this.queue.length) return Promise.resolve(this.queue.shift());
    if (this.closed) return Promise.reject(new Error("the connection closed"));
    return new Promise((ok, fail) => {
      const waiter = { ok: (v) => { clearTimeout(t); ok(v); }, fail: (e) => { clearTimeout(t); fail(e); } };
      const t = ms == null ? null : setTimeout(() => { this.waiters = this.waiters.filter((w) => w !== waiter); fail(new Error("no answer in time")); }, ms);
      this.waiters.push(waiter);
    });
  }
  /** From now on every line goes to `fn`, the ones already read first. */
  listen(fn) {
    this.onMessage = fn;
    for (const v of this.queue.splice(0)) fn(v);
  }
  send(value) {
    if (this.closed) return Promise.resolve(false);
    return this.engine.bridge.send(this.id, JSON.stringify(value)).then(() => true, () => false);
  }
  close() { if (!this.closed) this.engine.bridge.close(this.id).catch(() => {}); }
}

export class Engine {
  constructor(bridge) {
    this.bridge = bridge;
    this.conns = new Map();
    this.ready = null;
    // A line can arrive before `open` has said which connection it is on:
    // it waits here until the connection is known.
    this.early = new Map();
    bridge.on("line", (id, line) => {
      const conn = this.conns.get(id);
      if (conn) return conn.line(line);
      if (!this.early.has(id)) this.early.set(id, []);
      this.early.get(id).push(line);
    });
    bridge.on("closed", (id) => {
      const conn = this.conns.get(id);
      this.conns.delete(id);
      if (conn) conn.end(); else this.early.set(id, [...(this.early.get(id) || []), null]);
    });
  }

  /** Starts (or finds) the user's host: `sterna host --background`. */
  async start() {
    this.ready = await this.bridge.startHost();
    if (!this.ready?.listening || !this.ready?.token) throw new Error("the host's ready line has no address or token");
    if (this.ready.protocol != null && this.ready.protocol !== PROTOCOL) throw new Error(`the host speaks protocol ${this.ready.protocol}; this app speaks ${PROTOCOL}`);
    return this.ready;
  }

  /** Opens `address` and says the hello; the welcome, or the refusal as an error. */
  async connect(address, token) {
    const id = await this.bridge.open(address);
    const conn = new Conn(this, id);
    this.conns.set(id, conn);
    for (const line of this.early.get(id) || []) { if (line === null) { this.conns.delete(id); conn.end(); } else conn.line(line); }
    this.early.delete(id);
    await conn.send({ hello: { token, protocol: PROTOCOL, client: CLIENT } });
    const first = await conn.next(10000).catch((e) => { conn.close(); throw e; });
    if (!first?.welcome) {
      conn.close();
      throw new Error(first?.refused?.reason ? `refused: ${first.refused.reason}` : "the port did not welcome this app");
    }
    conn.welcome = first.welcome;
    return conn;
  }

  /** One host command on a connection of its own: its `ok`, or its `error` thrown. */
  async ask(command, ms = 30000) {
    if (!this.ready) throw new Error("the host has not started");
    const conn = await this.connect(this.ready.listening, this.ready.token);
    try {
      await conn.send(command);
      const answer = await conn.next(ms);
      if (answer && "error" in answer) throw new Error(String(answer.error));
      return answer?.ok ?? {};
    } finally {
      conn.close();
    }
  }

  list() { return this.ask({ do: "list" }); }
  /** Starts a session in `root`; with `resume`, reopens that session under its own id. */
  startSession(root, { task, model, resume } = {}) {
    return this.ask({ do: "start", root, ...(task ? { task } : {}), ...(model ? { model } : {}), ...(resume ? { resume } : {}) }, 60000);
  }
  locate(id) { return this.ask({ do: "locate", id }); }
  stop(id) { return this.ask({ do: "stop", id }); }
  usage() { return this.ask({ do: "usage" }); }
  quit(keep) { return this.ask({ do: "quit", keep: !!keep }); }
  /** Whether a newer release is out, and whether this copy can move to it. */
  releases(automatic) { return this.ask({ do: "update", check: true, automatic: !!automatic }, 60000); }
  /** Places the newest release beside the running one: a download, so on a connection of its own with no timeout. */
  moveToNewest() { return this.ask({ do: "update" }, null); }

  // -- setup, on the host (docs/engine.md) --------------------------------
  settings() { return this.ask({ do: "settings" }).then((ok) => ok.values || {}); }
  setSetting(key, value) { return this.ask({ do: "set_setting", key, value: String(value) }); }
  preferences() { return this.ask({ do: "preferences" }); }
  setPreferences(preferences) { return this.ask({ do: "set_preferences", preferences }); }
  accounts() { return this.ask({ do: "accounts" }, 30000).then((ok) => ok.accounts || []); }
  /** Hands the gateway a provider's key; the key is in this one line and nowhere else. */
  setKey(provider, key) { return this.ask({ do: "set_key", provider, key }, 30000); }

  /**
   * A sign-in run by the host: `onProgress` hears each `sign_in` line and
   * `done` settles with whether it connected. The connection stays open for
   * a pasted address or a cancel.
   */
  async signIn(provider, onProgress) {
    const conn = await this.connect(this.ready.listening, this.ready.token);
    let settle;
    const done = new Promise((ok) => { settle = ok; });
    conn.onClosed = () => settle({ connected: false, closed: true });
    await conn.send({ do: "sign_in", provider });
    const first = await conn.next(30000).catch((e) => ({ error: e.message }));
    if (first && "error" in first) { conn.close(); throw new Error(String(first.error)); }
    conn.listen((line) => {
      if (line?.sign_in) onProgress(line.sign_in);
      if (line?.done) { settle(line.done); conn.close(); }
    });
    return {
      done,
      paste: (address) => conn.send({ paste: address }),
      cancel: () => conn.send({ cancel: true }),
    };
  }

  /** Sends the list to `fn` now and on every change, until the host goes. */
  async watch(fn, onGone) {
    const conn = await this.connect(this.ready.listening, this.ready.token);
    await conn.send({ do: "watch" });
    const first = await conn.next();
    if (first && "error" in first) throw new Error(String(first.error));
    conn.listen((value) => { if (value?.list) fn(value.list); });
    conn.onClosed = () => onGone?.();
    return conn;
  }

  /** A session's port, past its hello. */
  session(address, token) { return this.connect(address, token); }
}
