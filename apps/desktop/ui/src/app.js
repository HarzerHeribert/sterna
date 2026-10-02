// The app: one store for every session the window knows -- whichever folder
// it runs in -- the host's list, and what the window shows. Events from the
// engine change the store; the views draw it; a click becomes a command.
import { $, baseName, morph, tilde, spaced } from "./util.js";
import { applyTheme, themeOf } from "./theme.js";
import { redrawBirds } from "./birds.js";
import { startingPrefs, fromHost, ownPart, SHARED } from "./prefs.js";
import { loadSettings } from "./choices.js";
import { Engine } from "./engine.js";
import { Session, working, ENDINGS } from "./session.js";
import { turnsOf } from "./record.js";
import { recordHTML, liveHTML } from "./views/convo.js";
import { sidebarHTML, sessionList, toolbarHTML, statusHTML, cbarHTML, queueHTML, toastHTML } from "./views/chrome.js";
import { inspectorHTML } from "./views/inspector.js";
import { releaseCard } from "./views/releases.js";
import { overlayHTML } from "./views/sheets.js";
import { splashHTML, flight } from "./views/splash.js";
import { clock, secs } from "./util.js";

/** The least time the flight is shown, so the start reads as one motion. */
const MIN_FLIGHT = 1400;

export class App {
  constructor(bridge) {
    this.bridge = bridge;
    this.engine = new Engine(bridge);
    this.prefs = startingPrefs();
    this.S = {
      phase: "starting", startError: "", startAt: performance.now(),
      list: { folders: [] }, usage: null,
      sessions: new Map(), attaching: new Set(), current: null, view: "nofolder",
      fresh: null, sheet: null, pickFolder: null, extraFolders: [], starting: false,
      open: {}, tab: {}, sel: null, raw: {},
      notice: "", noticeAt: 0, undo: null,
      m: { role: "main", all: true, byScore: false, q: "", focus: -1 },
      catalogue: null, modelsWaiting: false, modelsNote: null, modelRows: [],
      set: "appearance", sessQ: "", folded: {}, allRows: {}, unread: new Set(),
      signinConfirm: false, confirmFull: false,
      panel: null, forms: new Map(), follow: true, blink: false, gone: false,
      settings: {}, accounts: null, signin: null, keyProvider: "", keySaving: false,
      hostLost: "", started: new Set(),
      // Releases: `phase` is idle, checking, checked, failed, moving, moved or unmoved (a move that failed).
      releases: { phase: "idle", answer: null, error: "", asked: false, moved: "" },
    };
    this.memo = new WeakMap();
    this.dirty = false;
  }

  // -- what the views read ------------------------------------------------

  get theme() { return themeOf(this.prefs.theme); }
  cur() { return this.S.current ? this.S.sessions.get(this.S.current) || null : null; }
  sessionById(id) { return this.S.sessions.get(id) || null; }
  isOpen(k, d) { return k in this.S.open ? this.S.open[k] : d; }
  folderName(root) { return baseName(root); }
  tilde(root) { return tilde(root, this.bridge.home); }
  draft() { return $("#draft")?.value || ""; }
  noticeOn() { return !!this.S.notice && performance.now() - this.S.noticeAt < 5000; }

  turnsOf(session) {
    const st = session.state, key = st.conversation;
    const hit = this.memo.get(key);
    if (hit && hit.notebook === st.notebook && hit.reading === st.reading) return hit.result;
    const result = turnsOf(st.conversation, st.notebook, st.reading);
    this.memo.set(key, { notebook: st.notebook, reading: st.reading, result });
    return result;
  }

  /** Every session the list holds, flat. */
  listSessions() {
    return (this.S.list.folders || []).flatMap((f) => (f.sessions || []).map((s) => ({ ...s, root: f.root })));
  }

  /** The folders, the one used last first, each with its sessions, used last first. */
  folders() {
    const listed = (this.S.list.folders || []).map((f) => ({ root: f.root, last_used: f.last_used, sessions: (f.sessions || []).map((s) => ({ ...s, root: f.root })), fresh: null }));
    const known = new Set(listed.map((f) => f.root));
    const extra = this.S.extraFolders.filter((e) => !known.has(e.root)).map((e) => ({ root: e.root, last_used: e.at, sessions: [], fresh: null }));
    let all = [...extra.sort((a, b) => b.last_used - a.last_used), ...listed];
    const fresh = this.S.fresh;
    if (fresh) {
      let f = all.find((x) => x.root === fresh.root);
      if (!f) { f = { root: fresh.root, last_used: fresh.started, sessions: [], fresh: null }; all.unshift(f); }
      else all = [f, ...all.filter((x) => x !== f)];
      f.fresh = { id: fresh.id, root: fresh.root, fresh: true };
    }
    return all;
  }

  /** How a listed session stands now: from its own events when attached, else from the list. */
  liveOf(x) {
    const s = this.S.sessions.get(x.id);
    if (s && s.attached && !s.ended) {
      const act = s.state.activity, n = this.turnsOf(s).cells;
      if (working(act) && s.waiting) return { kind: "waiting" };
      if (act === "streaming") return { kind: "writing", cell: s.state.streaming.tool != null ? n + 1 : null };
      if (act === "executing" || act === "searching") return { kind: "running", cell: n };
      if (working(act)) return { kind: "thinking" };
      return this.S.unread.has(x.id) ? { kind: "unread" } : null;
    }
    if (this.S.unread.has(x.id)) return { kind: "unread" };
    const word = x.live?.state;
    if (!word || word === "idle") return null;
    return { kind: { running: "running", writing: "writing", waiting: "waiting" }[word] || "thinking" };
  }

  /** What the bird is doing: the session's face. */
  mood() {
    const s = this.cur();
    if (!s || !["main", "new"].includes(this.S.view)) return this.S.blink ? "blink" : "idle";
    const act = s.state.activity;
    if (act === "awaiting_you" || ["thinking", "starting", "waiting", "compacting", "searching"].includes(act)) return "think";
    if (act === "streaming" || act === "executing") return "work";
    if (act === "complete") return "done";
    if (act === "failed") return "oops";
    return this.S.blink ? "blink" : "idle";
  }

  // -- the start ----------------------------------------------------------

  async start() {
    this.S.phase = "starting";
    this.S.startAt = performance.now();
    this.render();
    try {
      // The flight is shown its whole length, a start that fails included.
      const [started] = await Promise.allSettled([this.engine.start(), new Promise((ok) => setTimeout(ok, MIN_FLIGHT))]);
      if (started.status === "rejected") throw started.reason;
      // The window as it was left: the app's own preferences and the shared settings, from the host.
      const [saved, values] = await Promise.all([this.engine.preferences().catch(() => ({})), this.engine.settings().catch(() => ({}))]);
      this.prefs = fromHost(saved, values);
      this.S.settings = values;
      await this.watch();
    } catch (e) {
      this.S.phase = "failed";
      this.S.startError = String(e?.message || e);
      this.render();
      return;
    }
    $("#splashbox")?.classList.add("leaving");
    await new Promise((ok) => setTimeout(ok, 350));
    this.S.phase = "ready";
    // The first thing asked: which folder to work in. The one used last is chosen.
    this.S.pickFolder = this.folders()[0]?.root || null;
    this.S.sheet = "folder";
    this.S.view = "nofolder";
    this.render();
    this.checkReleases();
  }

  async watch() {
    this.S.list = await this.engine.list();
    this.attachLive();
    this.watchConn = await this.engine.watch((list) => {
      this.S.list = list;
      this.attachLive();
      this.changed();
    }, () => {
      if (this.S.gone) return;
      this.say("The engine's host went away. Starting it again.");
      setTimeout(() => this.restartHost(), 1000);
    });
    this.S.hostLost = "";
    this.pollUsage();
  }

  /** Starts the host again after it went; when that fails, the status line offers Try again. */
  async restartHost() {
    try {
      await this.engine.start();
      await this.watch();
      this.say("The engine's host is running again");
    } catch (e) {
      this.S.hostLost = String(e?.message || e);
      this.changed();
    }
  }

  pollUsage() {
    clearTimeout(this.usageTimer);
    this.engine.usage().then((u) => {
      this.S.usage = u;
      for (const x of u.sessions || []) this.S.started.add(x.id);
      this.changed();
    }).catch(() => {});
    this.usageTimer = setTimeout(() => this.pollUsage(), 10000);
  }

  /** Attaches to every running session, so the overview and the list know how each stands. */
  attachLive() {
    for (const x of this.listSessions()) {
      if (!x.live) continue;
      const s = this.S.sessions.get(x.id);
      if ((s && s.attached && !s.ended) || this.S.attaching.has(x.id)) continue;
      this.attachTo(x).catch(() => {});
    }
  }

  async attachTo(x) {
    this.S.attaching.add(x.id);
    try {
      const where = await this.engine.locate(x.id);
      let s = this.S.sessions.get(x.id);
      if (!s || s.ended) { s = new Session(this, { id: x.id, root: x.root, title: x.title }); this.S.sessions.set(x.id, s); }
      // Again after a drop: from the event after the last one seen.
      await s.attach(where.listening, where.token, s.lastSeq ? s.lastSeq + 1 : null);
      return s;
    } finally {
      this.S.attaching.delete(x.id);
    }
  }

  /** A session's port names it: the window keys it by that id from then on. */
  rekey(session, id) {
    const was = session.id;
    if (was === id) return;
    this.S.sessions.delete(was);
    session.id = id;
    this.S.sessions.set(id, session);
    if (this.S.current === was) this.S.current = id;
    if (this.S.fresh?.id === was) this.S.fresh.id = id;
    if (this.S.unread.delete(was)) this.S.unread.add(id);
    this.changed();
  }

  /**
   * A connection dropped: if the list still says the session runs, take it
   * up again where it stopped -- after a wait that doubles each time, and
   * after a few tries in a row, not again until the person asks.
   */
  async dropped(session) {
    this.sessionChanged(session);
    if (this.S.sessions.get(session.id) !== session) return;
    const now = Date.now();
    session.drops = (session.lastDrop && now - session.lastDrop < 30000 ? session.drops || 0 : 0) + 1;
    session.lastDrop = now;
    if (session.drops > 4) {
      session.lost = "Its connection keeps closing as soon as it opens.";
      return this.changed();
    }
    await new Promise((ok) => setTimeout(ok, 400 * 2 ** (session.drops - 1)));
    if (this.S.sessions.get(session.id) !== session || session.closing) return;
    try { this.S.list = await this.engine.list(); } catch { /* the host's own watch says more */ }
    const x = this.listSessions().find((e) => e.id === session.id);
    if (x?.live) {
      try { await this.attachTo(x); return; } catch { /* falls through: ended */ }
    } else if (this.S.fresh?.id === session.id) {
      this.S.fresh = null;
    }
    if (!session.attached && !session.ended) session.ended = "its connection closed and it no longer runs";
    this.changed();
  }

  /** Try again, after a connection kept closing. */
  async reattach(session) {
    session.drops = 0;
    session.lost = "";
    const x = this.listSessions().find((e) => e.id === session.id);
    if (!x?.live) { session.ended = session.ended || "it no longer runs"; return this.changed(); }
    try { await this.attachTo(x); } catch (e) { this.say(`Not reconnected: ${e.message}`); }
    this.changed();
  }

  /**
   * Is a newer release out? Asked once on open, and again by Check now
   * (`asked`). On open, nothing is said unless one is and the person has
   * automatic checks on; a check that fails on open stays quiet too, and
   * Settings says why.
   */
  async checkReleases({ asked = false } = {}) {
    const R = this.S.releases;
    if (R.phase === "checking" || R.phase === "moving") return;
    Object.assign(R, { phase: "checking", error: "", asked: R.asked || asked });
    this.changed();
    try {
      R.answer = await this.engine.releases(!asked);
      R.phase = "checked";
    } catch (e) {
      R.phase = "failed";
      R.error = String(e?.message || e);
    }
    this.changed();
  }

  /** Moves to the newest release, beside the running one; the app opens it on Restart. */
  async moveToNewest() {
    const R = this.S.releases;
    if (R.phase === "moving") return;
    Object.assign(R, { phase: "moving", error: "" });
    this.changed();
    try {
      const done = await this.engine.moveToNewest();
      R.moved = done.installed || R.answer?.latest || "";
      R.phase = "moved";
    } catch (e) {
      R.phase = "unmoved";
      R.error = String(e?.message || e);
    }
    this.changed();
  }

  /** Opens the new release: every session is left running, as Keep running leaves them, and the app opens again from where it is opened. */
  async restartIntoNewest() {
    this.S.sheet = null;
    try { await this.engine.quit(true); } catch { /* the host is gone already: its sessions run on */ }
    this.S.gone = "Sterna is opening again.";
    this.changed();
    try {
      await this.bridge.restart();
    } catch (e) {
      this.S.gone = false;
      this.say(`Sterna could not open again: ${e.message}. Quit it and open it yourself.`, null, 10000);
    }
  }

  // -- what the sessions say ----------------------------------------------

  sessionChanged(session, kind) {
    if (this.S.fresh?.id === session.id && this.turnsOf(session).turns.length) this.S.fresh = null;
    if (session.id === this.S.current && this.S.view === "new" && this.turnsOf(session).turns.length) this.S.view = "main";
    const quiet = kind === "delta" || kind === "tool_delta" || kind === "reasoning";
    this.changed(quiet && session.id === this.S.current ? "live" : "all");
  }

  activityChanged(session, before, after) {
    const away = session.id !== this.S.current || document.hidden || this.S.view === "overview";
    const name = `${baseName(session.root)}: ${session.title || this.listSessions().find((x) => x.id === session.id)?.title || "a session"}`;
    if (ENDINGS.has(after) && working(before)) {
      if (away) this.S.unread.add(session.id);
      if (away && this.prefs.notifyDone) this.bridge.notify("A session finished", name);
    }
    if (after === "awaiting_you" && away && this.prefs.notifyAsk) this.bridge.notify("A session needs you", name);
    this.badge();
  }

  /** A form waits for this window: kept with its session, so another session's form never takes its place. */
  prompted(session, prompt) {
    if (prompt.type !== "form") return;
    const values = (prompt.form?.fields || []).map((f) => (typeof f.kind === "object" && f.kind ? (Object.values(f.kind)[0] || [])[f.chosen || 0] : f.value) ?? "");
    this.S.forms.set(session.id, { session: session.id, prompt, values });
    if (session.id === this.S.current) this.S.sheet = "form";
  }

  /** A prompt was answered, here or elsewhere: its form, if it was one, is put away. */
  settled(session, id) {
    const form = this.S.forms.get(session.id);
    if (!form || form.prompt.id !== id) return;
    this.S.forms.delete(session.id);
    if (this.S.sheet === "form" && session.id === this.S.current) this.S.sheet = null;
  }

  /** A snapshot says which prompts still wait: a form it no longer holds is put away. */
  promptsNow(session) {
    const form = this.S.forms.get(session.id);
    if (form && !session.state.prompts.some((p) => p.id === form.prompt.id)) this.settled(session, form.prompt.id);
  }

  /** The form the open session waits on, if any. */
  get form() { return this.S.current ? this.S.forms.get(this.S.current) || null : null; }

  /** A word about a session: said plainly for the one open, named for any other. */
  sayAbout(session, text) {
    const words = String(text || "");
    if (session.id === this.S.current && this.S.view !== "overview") return this.say(words[0].toUpperCase() + words.slice(1));
    const title = session.title || this.listSessions().find((x) => x.id === session.id)?.title || "a session";
    this.say(`In ${baseName(session.root)}, “${title.length > 48 ? title.slice(0, 47) + "…" : title}”: ${words}`);
  }

  noticed(session, text) {
    if (session.id !== this.S.current) return;
    const keep = this.S.undo && performance.now() - this.S.noticeAt < 2000 ? this.S.undo : null;
    this.say(text, keep);
  }

  paneled(session, panel) {
    if (session.id !== this.S.current) return;
    // The models sheet asked: the catalogue fills it, or the session's own
    // words say why there is none.
    if (this.S.modelsWaiting || this.S.sheet === "models") {
      this.S.modelsWaiting = false;
      if (panel?.catalogue) { this.S.catalogue = panel.catalogue; this.S.modelsNote = null; }
      else this.S.modelsNote = { session: session.id, panel };
      return;
    }
    this.S.panel = { session: session.id, panel };
    this.S.sheet = "panel";
  }

  /**
   * A sign-in a session runs itself (a `/login` typed in the composer),
   * drawn as the same sheet as the host's: its link, the paste field, Cancel.
   */
  signInChanged(session, step) {
    if (session.id !== this.S.current && this.S.signin?.via !== session.id) return;
    const si = session.signIn;
    if (step.step === "done") {
      if (this.S.signin?.via === session.id) {
        this.S.signin = null;
        if (this.S.sheet === "signin") this.S.sheet = null;
        this.say("The sign-in is over");
      }
      return;
    }
    if (!si) return;
    const URL_RE = /https?:\/\/[^\s"'<>]+/;
    const rows = si.panel?.rows || [];
    const link = rows.map((r) => String(r.text || "").match(URL_RE)?.[0] || (typeof r.action?.Path === "string" && URL_RE.test(r.action.Path) ? r.action.Path : null)).find(Boolean)
      || si.notes.map((n) => String(n).match(URL_RE)?.[0]).find(Boolean) || "";
    const notes = si.notes.filter((n) => !URL_RE.test(n));
    this.S.signin = { via: session.id, provider: "", label: si.label || "your account", link, notes, state: "opened", account: "" };
    if (step.step === "started" && session.id === this.S.current) { this.S.signinConfirm = false; this.S.sheet = "signin"; }
  }

  refused(session, to, reason) {
    if (session.id !== this.S.current && to !== "answer") return;
    this.sayAbout(session, String(reason || "not done"));
  }

  badge() {
    if (!this.prefs.dockBadge) return this.bridge.setBadge(0);
    const n = this.listSessions().filter((x) => this.liveOf(x)?.kind === "waiting").length;
    if (n !== this.lastBadge) { this.lastBadge = n; this.bridge.setBadge(n); }
  }

  say(text, undo = null, ms = 5000) {
    this.S.notice = spaced(text);
    this.S.noticeAt = performance.now() - (5000 - ms);
    this.S.undo = undo;
    this.changed();
  }

  // -- what a person does -------------------------------------------------

  show(session, view) {
    this.S.current = session.id;
    this.S.view = view || (this.turnsOf(session).turns.length || session.busy ? "main" : "new");
    this.S.unread.delete(session.id);
    if (this.S.forms.has(session.id)) this.S.sheet = "form";
    this.S.follow = true;
    this.changed();
    requestAnimationFrame(() => this.scrollBottom());
    this.badge();
  }

  /** A session started and left with nothing asked is ended, not left running. */
  async dropFresh(except) {
    const fresh = this.S.fresh;
    if (!fresh || fresh.id === except) return;
    this.S.fresh = null;
    const s = this.S.sessions.get(fresh.id);
    if (s && !this.turnsOf(s).turns.length) {
      this.engine.stop(fresh.id).catch(() => {});
      s.detach();
      this.S.sessions.delete(fresh.id);
      if (this.S.current === fresh.id) this.S.current = null;
    }
  }

  async newIn(root) {
    if (this.S.fresh && this.S.fresh.root === root) { this.S.sheet = null; return this.show(this.S.sessions.get(this.S.fresh.id), "new"); }
    await this.dropFresh();
    this.S.starting = true;
    this.changed();
    try {
      const started = await this.engine.startSession(root);
      const s = new Session(this, { id: started.id, root });
      this.S.sessions.set(started.id, s);
      await s.attach(started.listening, started.token);
      this.S.fresh = { id: started.id, root, started: Date.now() };
      this.S.extraFolders = this.S.extraFolders.filter((e) => e.root !== root);
      this.S.sheet = null;
      this.show(s, "new");
      requestAnimationFrame(() => $("#draft")?.focus());
    } catch (e) {
      this.say(`Could not start a session in ${baseName(root)}: ${e.message}`);
    } finally {
      this.S.starting = false;
      this.changed();
    }
  }

  async open(id) {
    if (this.S.fresh?.id === id) return this.show(this.S.sessions.get(id), "new");
    await this.dropFresh(id);
    const known = this.S.sessions.get(id);
    if (known && known.attached && !known.ended) return this.show(known);
    const x = this.listSessions().find((e) => e.id === id);
    if (!x) { if (known) this.show(known); return; }
    try {
      if (x.live) return this.show(await this.attachTo(x));
      // A session that is not running opens again in its folder, under its own id, with its record.
      const started = await this.engine.startSession(x.root, { resume: x.id });
      const s = new Session(this, { id: x.id, root: x.root, title: x.title });
      this.S.sessions.set(x.id, s);
      await s.attach(started.listening, started.token);
      if (s.id !== x.id) this.say("This engine started a new session in that folder: it cannot reopen one yet");
      this.show(s, "main");
    } catch (e) {
      this.say(`Could not open that session: ${e.message}`);
      if (known) this.show(known);
    }
  }

  send() {
    const ta = $("#draft"), text = ta.value.trim(), s = this.cur();
    if (!text || !s) return;
    if (s.busy) this.say("Queued for when this turn ends");
    s.submit(text);
    ta.value = "";
    this.grow();
    if (this.S.view === "new") this.S.view = "main";
    this.S.follow = true;
    this.changed();
  }


  answerApproval(sessionId, promptId, a) {
    const s = this.S.sessions.get(sessionId);
    const prompt = s?.state.prompts.find((p) => p.id === promptId);
    if (!s || !prompt) return;
    // A call that does more than its confirmation shows cannot be allowed, by a button or a key.
    if (prompt.complete === false && (a === "once" || a === "session")) {
      this.say("This call cannot be allowed: it does more than its confirmation shows. Deny it, or refuse it this once.");
      return;
    }
    const hosts = (prompt.hosts || []).length > 0;
    const approval = { once: "allow_once", session: hosts ? "allow_host_session" : "allow_for_session", deny: "deny", notnow: "deny_once" }[a];
    s.answer(promptId, { approval });
    const host = (prompt.hosts || []).join(", ");
    this.say({ once: "Allowed once", session: hosts ? `Allowed ${host} for this session` : "Allowed for this session", deny: "Denied for this session", notnow: "Refused this once. The next identical call asks again." }[a]);
  }

  /** Leaves: `keep` says what happens to the sessions this app started; null says nothing to the host. */
  async quitNow(keep) {
    this.S.sheet = null;
    if (keep != null) { try { await this.engine.quit(keep); } catch { /* the host is gone already */ } }
    this.S.gone = true;
    this.changed();
    await this.bridge.quit();
  }

  /** The sessions this app's host started that still run: what the quit question is about. */
  startedRunning() {
    return this.listSessions().filter((x) => x.live && this.S.started.has(x.id));
  }

  async requestQuit() {
    // Before the list was shown nothing is known: the sessions are left as they are.
    if (this.S.phase !== "ready") return this.quitNow(null);
    try { const u = await this.engine.usage(); for (const x of u.sessions || []) this.S.started.add(x.id); } catch { /* as last heard */ }
    if (!this.startedRunning().length) return this.quitNow(null);
    this.S.sheet = "quit";
    this.changed();
  }

  grow() {
    const ta = $("#draft");
    if (!ta) return;
    ta.style.height = "auto";
    ta.style.height = Math.min(ta.scrollHeight, 220) + "px";
    const b = $("#send");
    if (b) b.disabled = !ta.value.trim();
  }

  scrollBottom() {
    const sc = $("#scroll");
    if (!sc) return;
    sc.scrollTop = sc.scrollHeight;
    this.S.follow = true;
    $("#latest").hidden = true;
  }

  // -- drawing ------------------------------------------------------------

  /** Asks for a redraw on the next frame: `live` redraws only the live edge. */
  changed(what = "all") {
    this.dirtyAll = this.dirtyAll || what === "all";
    if (this.dirty) return;
    this.dirty = true;
    requestAnimationFrame(() => { this.dirty = false; const all = this.dirtyAll; this.dirtyAll = false; this.render(!all); });
  }

  render(liveOnly = false) {
    const { light } = applyTheme(this.prefs);
    this.light = light;
    const S = this.S, p = this.prefs, win = $("#win");
    document.body.classList.toggle("web", this.bridge.os === "web");
    document.body.classList.toggle(this.bridge.os, true);
    const put = (el, html) => { if (el && el._html !== html) { el.innerHTML = html; el._html = html; return true; } return false; };
    const splash = S.phase !== "ready" && !S.gone ? splashHTML(this) : "";
    if (put($("#splash"), splash) && splash) flight(this, performance.now());
    if (S.gone) { put($("#overlay"), `<div class="gone"><b>${typeof S.gone === "string" ? S.gone : "Sterna has quit."}</b><span>You can close this window.</span></div>`); return; }
    const sc = $("#scroll");
    const atBottom = S.follow;
    if (!liveOnly) {
      win.classList.toggle("nosb", !p.sessions);
      win.classList.toggle("noin", !p.card || S.view === "nofolder" || S.view === "overview" || !this.cur());
      win.classList.toggle("calm", p.motion === "calm");
      win.classList.toggle("still", p.motion === "off");
      $("#dock").hidden = S.view === "overview";
      const ta = $("#draft"), s = this.cur();
      ta.disabled = S.view === "nofolder" || !s || !!s.ended;
      ta.placeholder = S.view === "nofolder" || !s ? "Choose a folder first" : s.ended ? "This session has ended" : "Describe the next step — a message, or / for commands";
      if (document.activeElement?.id !== "sessq") put($("#sidebar"), sidebarHTML(this));
      else put($("#slist"), sessionList(this));
      put($("#sbfoot"), releaseCard(this));
      put($("#toolbar"), toolbarHTML(this));
      put($("#record"), recordHTML(this));
      put($("#inspector"), inspectorHTML(this));
      put($("#status"), statusHTML(this));
      put($("#queue"), queueHTML(this));
      put($("#cbar"), cbarHTML(this));
      const toast = toastHTML(this);
      put($("#toast"), S.view === "overview" ? "" : toast);
      put($("#ovtoast"), S.view === "overview" ? toast : "");
      this.renderOverlay();
    }
    put($("#live"), liveHTML(this));
    if (atBottom && sc) sc.scrollTop = sc.scrollHeight;
    win.style.setProperty("--dock-h", ($("#dock").offsetHeight || 0) + "px");
    this.tick(true);
  }

  renderOverlay() {
    const host = $("#overlay"), html = overlayHTML(this);
    const wrapped = html ? `<div class="overlay" data-act="scrim">${html}</div>` : "";
    if (host._html === wrapped) return;
    const had = !!host._html;
    const active = document.activeElement, focusId = host.contains(active) ? active.id : null;
    const sel = focusId && "selectionStart" in active ? [active.selectionStart, active.selectionEnd] : null;
    const scrolls = [...host.querySelectorAll(".sbd,.setpane")].map((e) => e.scrollTop);
    // What is typed in a field that keeps its own value -- a key, a secret --
    // lives in the field alone: carried across a redraw, never in the page's HTML.
    const kept = [...host.querySelectorAll("input[data-keep]")].map((e) => [e.id, e.value]);
    // In place, never afresh: a sheet drawn anew would fade in and rise again on every click.
    morph(host, wrapped);
    host._html = wrapped;
    for (const [id, value] of kept) { const e = document.getElementById(id); if (e) e.value = value; }
    if (had && wrapped) host.querySelectorAll(".sbd,.setpane").forEach((e, i) => { e.scrollTop = scrolls[i] || 0; });
    if (focusId) {
      const again = document.getElementById(focusId);
      if (again) { again.focus({ preventScroll: true }); if (sel && again.setSelectionRange) again.setSelectionRange(sel[0], sel[1]); }
    } else if (wrapped && !had) host.querySelector(".sheet input, .sheet .done, .sheet button")?.focus({ preventScroll: true });
  }

  /** Clocks, the flight and the blink, ten times a second. */
  tick(fresh = false) {
    const now = Date.now();
    document.querySelectorAll("[data-clock]").forEach((el) => {
      const ms = (el.dataset.until ? +el.dataset.until : now) - +el.dataset.since;
      el.textContent = el.dataset.clock === "s" ? secs(ms) : clock(ms);
    });
    if (this.S.phase !== "ready") flight(this, performance.now());
    if (fresh) return;
    const view = this.S.view, motion = this.prefs.motion;
    if (!["main", "new"].includes(view) || !this.cur()?.busy) {
      this.lastBlink = this.lastBlink || performance.now();
      if (motion === "full" && performance.now() - this.lastBlink > 2900 && this.mood() === "idle") {
        this.lastBlink = performance.now();
        this.S.blink = true;
        redrawBirds(this.theme, "blink", this.light);
        setTimeout(() => { this.S.blink = false; redrawBirds(this.theme, this.mood(), this.light); }, 160);
      }
    }
    if (this.S.notice && !this.noticeOn() && !this.faded) { this.faded = true; this.S.undo = null; this.changed(); }
    if (this.noticeOn()) this.faded = false;
  }

  /** A choice saves at once: a shared one as the person's setting, the rest as the app's preferences. */
  savePref(key) {
    if (key in SHARED) {
      const value = this.prefs[key];
      this.engine.setSetting(SHARED[key], value).then(() => { this.S.settings = { ...this.S.settings, [SHARED[key]]: value }; }, (e) => this.say(`Not saved: ${e.message}`));
    } else {
      clearTimeout(this.prefsTimer);
      this.prefsTimer = setTimeout(() => this.engine.setPreferences(ownPart(this.prefs)).catch((e) => this.say(`Not saved: ${e.message}`)), 250);
    }
    this.badge();
  }

  reloadSettings() { return loadSettings(this); }
}

