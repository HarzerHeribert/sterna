// Every click, and the keys that do the same: one interaction model for
// every surface. An element names its action in `data-act`.
import { $ } from "./util.js";
import { RELEASES_PAGE } from "./views/releases.js";
import { cellStates } from "./views/convo.js";
import { actionOf, kindOf } from "./views/panel-sheets.js";
import { loadAccounts, loadSettings, chooseModel, chooseEffort, chooseLevel, startSignIn, cancelSignIn, saveKey, saveSetting } from "./choices.js";
import { FOLD, panelsAt } from "./prefs.js";

export function wire(app) {
  const S = app.S;
  const cur = () => app.cur();
  const redraw = () => app.changed();
  const closeSheet = () => {
    if (S.sheet === "form" && app.form) return A.formcancel();
    // A panel whose leaving is itself an answer says so as it closes.
    const back = S.sheet === "panel" ? S.panel?.panel?.back : null;
    S.sheet = null; S.confirmFull = false; S.signinConfirm = false; S.panelConfirm = null;
    if (!app.cur() && S.view !== "overview") S.view = "nofolder";
    if (back) perform(back, { closing: true });
    redraw();
  };
  const togglePref = (k) => {
    // A side panel's button turns over what is shown, and that becomes the person's choice.
    // A window too narrow for both holds one at a time: showing one folds the other, unsaved.
    if (k in S.shown) {
      S.shown[k] = !S.shown[k];
      app.prefs[k] = S.shown[k];
      if (S.shown[k] && innerWidth < FOLD.card) for (const other of Object.keys(FOLD)) if (other !== k) S.shown[other] = false;
    } else app.prefs[k] = !app.prefs[k];
    app.savePref(k); redraw();
  };
  const TABS = { code: "program", diff: "changes", output: "output" };

  /** A session-built row's action, as far as a client can take it: commands go to the session, the rest open what they name. */
  function perform(action, { closing = false, stay = false } = {}) {
    const a = actionOf(action);
    if (!a) return;
    const s = app.sessionById(S.panel?.session) || cur();
    const leave = () => { if (!closing && !stay) { S.sheet = null; S.panelConfirm = null; } };
    switch (a.type) {
      case "Command": s?.control(String(a.value)); leave(); break;
      case "Level": case "ConfirmLevel": chooseLevel(app, String(a.value)); leave(); break;
      case "Choose": s?.control(`/model ${a.value}`); leave(); break;
      case "Path": A.path({ dataset: { v: String(a.value) } }); break;
      case "Insert": case "Draft": S.sheet = null; A.insert({ dataset: { v: String(a.value) } }); break;
      case "Cell": if (s) { S.open[`${s.id}:c${a.value}`] = true; S.sel = `${s.id}:c${a.value}`; } S.sheet = null; break;
      case "Tab": if (s && Array.isArray(a.value)) { const k = `${s.id}:c${a.value[0]}`; S.open[k] = true; S.tab[k] = TABS[String(a.value[1]).toLowerCase()] || "program"; S.sel = k; } S.sheet = null; break;
      case "Models": A.models(); break;
      case "Settings": case "SettingsAt": A.settings(); break;
      case "Sandbox": case "Hosts": A.level(); break;
      case "Help": S.sheet = "help"; break;
      case "Activity": S.sheet = "activity"; break;
      case "Telemetry": S.sheet = "telemetry"; break;
      case "Close": if (!closing) { S.sheet = null; } break;
      case "Resume": S.sheet = null; app.open(String(a.value)); break;
      case "RemoveHost": s?.host(String(a.value), false); break;
      case "Latest": S.sheet = null; app.scrollBottom(); break;
      default: break;
    }
    redraw();
  }

  const A = {
    noop: () => {},
    retry: () => app.start(),
    "retry-host": () => { app.S.hostLost = ""; app.restartHost(); redraw(); },
    // Newer releases
    "rel-check": () => app.checkReleases({ asked: true }),
    "rel-move": () => app.moveToNewest(),
    "rel-restart": () => app.restartIntoNewest(),
    "rel-page": () => app.bridge.openUrl(RELEASES_PAGE).catch((e) => app.say(`The releases page did not open: ${e.message}`)),
    scrim: (el, e) => { if (e.target === el && S.sheet !== "quit" && S.sheet !== "form") closeSheet(); },
    close: () => closeSheet(),
    // Sessions and folders
    newsession: () => {
      const s = cur();
      S.pickFolder = s?.root || app.folders()[0]?.root || null;
      S.sheet = "folder";
      redraw();
    },
    pickfolder: (el) => { S.pickFolder = el.dataset.f; redraw(); },
    browse: async () => {
      const root = await app.bridge.chooseFolder().catch((e) => { app.say(`The folder chooser did not open: ${e.message}`); return null; });
      if (!root) return;
      if (!app.folders().some((f) => f.root === root)) S.extraFolders.push({ root, at: Date.now() });
      S.pickFolder = root;
      redraw();
    },
    startsession: () => { if (S.pickFolder && !S.starting) app.newIn(S.pickFolder); },
    newin: (el) => app.newIn(el.dataset.f),
    open: (el) => { S.sheet = S.sheet === "folder" ? null : S.sheet; app.open(el.dataset.id); },
    fold: (el) => { const f = el.dataset.f; S.folded[f] = !S.folded[f]; redraw(); },
    allrows: (el) => { const f = el.dataset.f; S.allRows[f] = !S.allRows[f]; redraw(); },
    overview: () => { S.view = "overview"; S.sheet = null; app.pollUsage(); redraw(); requestAnimationFrame(() => { $("#scroll").scrollTop = 0; }); },
    // The conversation
    cell: (el) => { const k = el.dataset.k; S.sel = k; S.open[k] = !(el.getAttribute("aria-expanded") === "true"); redraw(); },
    tab: (el) => { S.tab[el.dataset.k] = el.dataset.v; S.sel = el.dataset.k; redraw(); },
    toggle: (el) => { S.open[el.dataset.k] = !app.isOpen(el.dataset.k, false); redraw(); },
    // Every cell folded, or, when every one already is, every one open.
    foldall: () => {
      const s = cur();
      if (!s) return;
      const cells = cellStates(app, s), open = !cells.some((c) => c.open);
      for (const c of cells) S.open[c.key] = open;
      redraw();
    },
    raw: (el) => { S.raw[el.dataset.k] = !S.raw[el.dataset.k]; redraw(); },
    path: (el) => {
      const s = cur(), p = el.dataset.v;
      if (!s) return;
      // A path an answer names opens inside the session's folder, never outside it.
      app.bridge.openPath(p, s.root).then(() => app.say(`Opened ${p}`), (e) => app.say(`Not opened: ${e?.message || e}`));
    },
    showdiff: (el) => {
      const k = el.dataset.k;
      S.sel = k; S.open[k] = true; S.tab[k] = "changes";
      redraw();
      requestAnimationFrame(() => document.getElementById(`cell-${k.split(":c")[1]}`)?.scrollIntoView({ block: "start", behavior: "smooth" }));
    },
    insert: (el) => {
      const ta = $("#draft");
      ta.value = ta.value ? ta.value + "\n\n" + el.dataset.v : el.dataset.v;
      app.grow(); ta.focus(); redraw();
    },
    latest: () => app.scrollBottom(),
    approve: (el) => app.answerApproval(el.dataset.s, +el.dataset.p, el.dataset.a),
    choose: (el) => { app.sessionById(el.dataset.s)?.answer(+el.dataset.p, { choice: el.dataset.v }); },
    dismiss: (el) => { app.sessionById(el.dataset.s)?.answer(+el.dataset.p, { dismiss: null }); },
    // The dock
    send: () => app.send(),
    stop: () => { cur()?.stop(); redraw(); },
    cancel: () => { cur()?.cancel(); redraw(); },
    takeback: async () => {
      const s = cur(), items = s?.state.queue || [];
      if (!items.length) return;
      const last = items[items.length - 1];
      if (await s.takeBack()) {
        const ta = $("#draft");
        ta.value = ta.value ? `${ta.value}\n\n${last}` : last;
        app.grow();
        app.say("Taken back into the draft");
      }
    },
    undo: () => { const u = S.undo; S.undo = null; S.notice = ""; u?.run?.(); redraw(); },
    // Panels and sheets
    pref: (el) => togglePref(el.dataset.k),
    setpref: (el) => { app.prefs[el.dataset.k] = el.dataset.v; app.savePref(el.dataset.k); redraw(); },
    settingseg: (el) => saveSetting(app, el.dataset.k, el.dataset.v, null),
    settings: () => { S.sheet = "settings"; loadSettings(app); redraw(); },
    section: (el) => { S.set = el.dataset.v; redraw(); requestAnimationFrame(() => { const p = $("#setpane"); if (p) p.scrollTop = 0; }); },
    help: () => { S.sheet = "help"; redraw(); },
    sheet: (el) => { S.sheet = el.dataset.v; if (el.dataset.v === "telemetry") app.pollUsage(); redraw(); },
    level: () => { S.sheet = "level"; S.confirmFull = false; if (!cur()) loadSettings(app); redraw(); },
    setlevel: (el) => {
      const s = cur(), word = el.dataset.v;
      const was = String(s && !s.ended ? s.state.facts.level : S.settings?.["sandbox.level"] || "").toLowerCase();
      if (was === word) return;
      if (word === "full") { S.confirmFull = true; return redraw(); }
      S.confirmFull = false;
      chooseLevel(app, word);
    },
    "setlevel-full": () => { S.confirmFull = false; chooseLevel(app, "full"); redraw(); },
    "setlevel-cancel": () => { S.confirmFull = false; redraw(); },
    forget: (el) => cur()?.forget(el.dataset.v),
    unhost: (el) => cur()?.host(el.dataset.v, false),
    models: () => { S.sheet = "models"; S.m.focus = -1; loadAccounts(app); if (!cur()) loadSettings(app); redraw(); },
    accounts: () => loadAccounts(app),
    role: (el) => { S.m.role = el.dataset.v; S.m.focus = -1; redraw(); },
    sources: (el) => { S.m.all = el.dataset.v === "1"; S.m.focus = -1; redraw(); },
    order: (el) => { S.m.byScore = el.dataset.v === "1"; S.m.focus = -1; redraw(); },
    provider: (el) => { S.m.provider = el.dataset.v || null; S.m.focus = -1; redraw(); },
    effort: (el) => chooseEffort(app, el.dataset.v),
    model: (el) => { const r = S.modelRows[+el.dataset.i]; if (r?.avail) { S.m.focus = +el.dataset.i; chooseModel(app, r.model); } },
    signin: (el) => startSignIn(app, el.dataset.v, el.dataset.l || el.dataset.v),
    "signin-reopen": () => { if (S.signin) { S.sheet = "signin"; redraw(); } },
    "signin-again": () => { const si = S.signin; if (si) startSignIn(app, si.provider, si.label); },
    "signin-ask": () => { S.signinConfirm = true; redraw(); },
    "signin-noopen": () => { S.signinConfirm = false; redraw(); },
    "signin-open": (el) => {
      S.signinConfirm = false;
      const url = el.dataset.v;
      app.bridge.openUrl(url).catch(() => app.say("The browser did not open. Copy the link instead."));
      app.say("Opened the sign-in page in your browser");
    },
    copy: (el) => { navigator.clipboard?.writeText(el.dataset.v).then(() => app.say("Sign-in link copied"), () => app.say("Could not copy the link")); },
    "signin-finish": () => {
      const field = $("#sifield"), text = (field?.value || "").trim();
      if (!text) { app.say("Paste the address first: the page you land on after signing in."); return; }
      // The address carries the sign-in's one-time code: sent once, then gone from the field.
      if (field) field.value = "";
      const via = S.signin?.via ? app.sessionById(S.signin.via) : null;
      if (via) via.signInPaste(text);
      else if (app.signInRun) app.signInRun.paste(text);
      else { app.say("No sign-in is running. Start it again."); return; }
      if (S.signin) S.signin.notes = [];
      app.say("Sent. Waiting for the account to connect.");
    },
    "signin-cancel": () => {
      const via = S.signin?.via ? app.sessionById(S.signin.via) : null;
      if (via) { via.signInCancel(); S.signin = null; S.sheet = null; return redraw(); }
      cancelSignIn(app); A.models();
    },
    "signin-back": () => A.models(),
    keyprovider: (el) => { S.keyProvider = el.dataset.v; const f = $("#keyprov"); if (f) f.value = el.dataset.v; redraw(); },
    savekey: () => saveKey(app, ($("#keyprov")?.value || S.keyProvider || "").trim(), $("#keyfield")),
    control: (el) => { const s = cur(); if (s) { if (S.sheet === "models") S.modelsWaiting = true; s.control(el.dataset.v); redraw(); } },
    panelrow: (el) => {
      const row = S.panel?.panel?.rows?.[+el.dataset.r];
      if (!row) return;
      const { kind } = kindOf(row);
      if (kind === "danger" && !el.dataset.sure) { S.panelConfirm = +el.dataset.r; return redraw(); }
      // A choice, a toggle or a value applies and the sheet stays, as in the terminal.
      perform(row.action, { stay: ["choice", "toggle", "value", "open"].includes(kind) });
    },
    panelvalue: (el) => {
      const row = S.panel?.panel?.rows?.[+el.dataset.r];
      const [, action] = (Object.values(row?.kind || {})[0]?.values || [])[+el.dataset.vi] || [];
      if (action) perform(action, { stay: true });
    },
    panelconfirm: (el) => { S.panelConfirm = +el.dataset.r >= 0 ? +el.dataset.r : null; redraw(); },
    formchoice: (el) => {
      const i = +el.dataset.i, f = app.form;
      if (!f) return;
      const words = Object.values(f.prompt.form.fields[i].kind || {})[0] || [];
      f.values[i] = words[+el.dataset.v];
      redraw();
    },
    formsubmit: () => {
      const f = app.form;
      if (!f) return;
      // The answer is made from the fields themselves: a secret goes from its field to the answer, and nowhere else.
      const answer = (f.prompt.form.fields || []).map((_, i) => {
        const input = document.querySelector(`[data-form="${i}"]`);
        return String(input ? input.value : f.values[i] ?? "");
      });
      document.querySelectorAll("[data-secret]").forEach((input) => { input.value = ""; });
      app.sessionById(f.session)?.answer(f.prompt.id, { form: answer });
      S.forms.delete(f.session); S.sheet = null; redraw();
    },
    formcancel: () => {
      const f = app.form;
      if (f) app.sessionById(f.session)?.answer(f.prompt.id, { dismiss: null });
      if (f) S.forms.delete(f.session);
      S.sheet = null; redraw();
    },
    reattach: (el) => { const s = app.sessionById(el.dataset.s); if (s) app.reattach(s); },
    // Quitting
    "quit-keep": () => app.quitNow(true),
    "quit-stop": () => app.quitNow(false),
  };
  app.actions = A;

  document.addEventListener("click", (e) => {
    const el = e.target.closest("[data-act]");
    if (!el || el.disabled || el.getAttribute("aria-disabled") === "true") return;
    const act = A[el.dataset.act];
    if (act) act(el, e);
  });

  document.addEventListener("input", (e) => {
    const t = e.target;
    if (t.id === "draft") { app.grow(); return; }
    if (t.id === "msearch") { S.m.q = t.value; S.m.focus = -1; redraw(); return; }
    if (t.id === "sessq") { S.sessQ = t.value; redraw(); return; }
    // A secret stays in its field: it never reaches the store, the page's HTML or a log.
    if (t.dataset.form != null && app.form && !t.dataset.secret) app.form.values[+t.dataset.form] = t.value;
  });

  $("#scroll").addEventListener("scroll", () => {
    const sc = $("#scroll");
    S.follow = sc.scrollTop + sc.clientHeight >= sc.scrollHeight - 30;
    $("#latest").hidden = S.follow || S.view === "overview";
  });

  // Every card's selection, for the keys that act on the selected cell.
  const cellKeys = () => [...document.querySelectorAll("#record article.cell[data-n]")].map((a) => `${S.current}:c${a.dataset.n}`);
  const selectCell = (d) => {
    const ks = cellKeys();
    if (!ks.length) return;
    const i = ks.indexOf(S.sel);
    S.sel = ks[Math.max(0, Math.min(ks.length - 1, i < 0 ? (d > 0 ? 0 : ks.length - 1) : i + d))];
    redraw();
    requestAnimationFrame(() => document.getElementById(`cell-${S.sel.split(":c")[1]}`)?.scrollIntoView({ block: "nearest" }));
  };

  let lastEsc = 0;
  document.addEventListener("keydown", (e) => {
    const key = e.key.toLowerCase(), mod = e.metaKey || e.ctrlKey;
    if (S.sheet) {
      if (e.key === "Escape") {
        e.preventDefault();
        if (S.sheet === "models" && S.m.q) { S.m.q = ""; redraw(); } else closeSheet();
        return;
      }
      if (S.sheet === "models" && (e.key === "ArrowDown" || e.key === "ArrowUp")) {
        e.preventDefault();
        const rows = S.modelRows;
        let i = S.m.focus;
        for (let step = 0; step < rows.length; step++) {
          i = e.key === "ArrowDown" ? Math.min(rows.length - 1, i + 1) : Math.max(0, i - 1);
          if (rows[i]?.avail) break;
        }
        S.m.focus = i; redraw();
        requestAnimationFrame(() => document.querySelector(".mrow.focus")?.scrollIntoView({ block: "nearest" }));
        return;
      }
      if (S.sheet === "models" && e.key === "Enter" && S.m.focus >= 0) { e.preventDefault(); A.model({ dataset: { i: S.m.focus } }); return; }
      if (S.sheet === "signin" && e.key === "Enter" && e.target.id === "sifield") { e.preventDefault(); A["signin-finish"](); return; }
      if (S.sheet === "key" && e.key === "Enter" && e.target.id === "keyfield") { e.preventDefault(); A.savekey(); return; }
      if (S.sheet === "form" && e.key === "Enter" && e.target.dataset.form != null) { e.preventDefault(); A.formsubmit(); return; }
      return;
    }
    // Esc in the search leaves it: the search is cleared, and nothing else is stopped.
    if (e.target.id === "sessq" && e.key === "Escape") {
      e.preventDefault();
      e.target.value = ""; S.sessQ = ""; e.target.blur(); redraw();
      return;
    }
    const s = cur();
    // A question waits: with it in focus, the keyboard answers it too, though
    // nothing on screen asks for a key. Elsewhere a letter is only a letter.
    const card = document.activeElement?.closest?.(".ask[data-s]");
    if (card && !mod && !e.altKey) {
      const owner = app.sessionById(card.dataset.s), pid = +card.dataset.p;
      const prompt = owner?.state.prompts.find((p) => p.id === pid);
      if (prompt?.type === "approval") {
        // The keys do what the card's buttons do, and only what they do.
        if (e.key === "Escape") { e.preventDefault(); app.answerApproval(owner.id, pid, "notnow"); return; }
        if ({ o: 1, s: 1, d: 1 }[key]) { e.preventDefault(); app.answerApproval(owner.id, pid, { o: "once", s: "session", d: "deny" }[key]); return; }
      }
      // With a question or an approval in focus, Esc never stops the turn.
      if (e.key === "Escape") { e.preventDefault(); return; }
    }
    if (e.key === "F3" || (key === "m" && mod && e.shiftKey)) { e.preventDefault(); A.models(); return; }
    if (e.key === "F2" || (e.key === "," && mod)) { e.preventDefault(); A.settings(); return; }
    if (key === "n" && mod && !e.shiftKey) { e.preventDefault(); A.newsession(); return; }
    if (e.altKey && (e.key === "ArrowUp" || e.key === "ArrowDown")) { e.preventDefault(); selectCell(e.key === "ArrowDown" ? 1 : -1); return; }
    if (e.ctrlKey && e.shiftKey && key === "o") { e.preventDefault(); A.foldall(); return; }
    if (e.ctrlKey && key === "o" && S.sel) { e.preventDefault(); S.open[S.sel] = !app.isOpen(S.sel, true); redraw(); return; }
    if (e.key === "F4" && S.sel) { e.preventDefault(); A.showdiff({ dataset: { k: S.sel } }); return; }
    if (e.key === "Escape" && s?.busy) {
      e.preventDefault();
      const twice = performance.now() - lastEsc < 600;
      lastEsc = performance.now();
      if (twice || s.stopAsked) s.cancel(); else s.stop();
      redraw();
      return;
    }
    if (e.target.id === "draft" && e.key === "Enter" && !e.shiftKey && !e.altKey && !e.isComposing) { e.preventDefault(); app.send(); return; }
    if (e.target.id === "draft" && e.key === "?" && !e.target.value) { e.preventDefault(); A.help(); }
  });

  // Like an iPad split view: a window made narrow folds the side panels away,
  // made wide again it shows those the person keeps, and the toolbar buttons
  // bring one back meanwhile. Only crossing a width changes what is shown.
  let lastW = innerWidth;
  addEventListener("resize", () => {
    const w = innerWidth, now = panelsAt(app.prefs, w);
    for (const k of Object.keys(FOLD)) if ((w < FOLD[k]) !== (lastW < FOLD[k])) S.shown[k] = now[k];
    lastW = w;
    redraw();
  });
  matchMedia("(prefers-color-scheme: light)").addEventListener("change", () => { if (app.prefs.appearance === "system") redraw(); });
  document.addEventListener("visibilitychange", () => { if (!document.hidden && S.current) { S.unread.delete(S.current); redraw(); } });

  app.bridge.on("quit", () => app.requestQuit());
  app.bridge.on("drop", (paths) => {
    const root = paths[0];
    if (!root || S.phase !== "ready") return;
    if (!app.folders().some((f) => f.root === root)) S.extraFolders.push({ root, at: Date.now() });
    S.pickFolder = root;
    S.sheet = "folder";
    redraw();
  });
  return A;
}
