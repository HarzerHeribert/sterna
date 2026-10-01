// The window's frame: the list of every folder and session, the toolbar,
// the status line, the composer's buttons, the queue and the toast.
import { esc, plural, agoMs, pad3 } from "../util.js";
import { icon } from "../icons.js";
import { birdArt } from "../birds.js";
import { liveMeta, levelOf } from "./convo.js";
import { working } from "../session.js";

const lights = (app) => `<div class="lights${app.bridge.os === "macos" ? " space" : ""}" aria-hidden="true"><i></i><i></i><i></i></div>`;

function counts(app, entries) {
  const ls = entries.map((x) => app.liveOf(x)).filter(Boolean);
  return {
    needs: ls.filter((l) => l.kind === "waiting").length,
    busy: ls.filter((l) => ["running", "writing", "thinking"].includes(l.kind)).length,
  };
}

function sessionRow(app, x) {
  const cur = app.S.view !== "overview" && app.S.current === x.id;
  if (x.fresh) return `<button class="sitem${cur ? " cur" : ""}" data-act="open" data-id="${esc(x.id)}"><span class="t">New session</span><span class="m">Nothing asked yet</span></button>`;
  const l = app.liveOf(x);
  const meta = l ? liveMeta(l) : `${agoMs(x.last_used)}`;
  return `<button class="sitem${cur ? " cur" : ""}" data-act="open" data-id="${esc(x.id)}"><span class="t">${esc(x.title)}</span><span class="m">${meta}</span></button>`;
}

/** Every session runs in a folder: listed under it, the folder used last first, and in each folder the session used last first. */
export function sessionList(app) {
  const q = app.S.sessQ.trim().toLowerCase();
  const folders = app.folders();
  const body = folders.map((f) => {
    const name = app.folderName(f.root), where = app.tilde(f.root);
    const hit = (x) => !q || (x.title || "").toLowerCase().includes(q) || name.toLowerCase().includes(q) || where.toLowerCase().includes(q);
    const rows = f.sessions.filter(hit);
    if (q && !rows.length) return "";
    if (!f.sessions.length && !f.fresh) return "";
    const closed = app.S.folded[f.root] && !q, c = counts(app, f.sessions);
    const busy = c.needs ? `<span class="fc warn">${c.needs} ${c.needs === 1 ? "needs" : "need"} you</span>` : c.busy ? `<span class="fc">${c.busy} running</span>` : "";
    return `<div class="fgroup"><div class="fhead"><button class="fbtn" data-act="fold" data-f="${esc(f.root)}" aria-expanded="${!closed}"><span class="chev">${icon(closed ? "right" : "down", "s")}</span><span class="fn">${esc(name)}</span><span class="fp">${esc(where)}</span>${busy}</button>` +
      `<button class="iconbtn sm" data-act="newin" data-f="${esc(f.root)}" aria-label="New session in ${esc(name)}" title="New session in ${esc(name)}">${icon("plus")}</button></div>` +
      (closed ? "" : (f.fresh ? sessionRow(app, f.fresh) : "") + rows.map((x) => sessionRow(app, x)).join("")) + `</div>`;
  }).join("");
  if (body) return body;
  return q ? `<p class="cap2" style="padding:8px 12px">No session matches “${esc(app.S.sessQ)}”.</p>`
    : `<p class="cap2" style="padding:8px 12px">No sessions yet. A session is listed once you ask it something.</p>`;
}

export function sidebarHTML(app) {
  const c = counts(app, app.listSessions());
  // One badge: what needs you outranks what runs.
  const badge = c.needs ? `<span class="badge warn">${c.needs} ${c.needs === 1 ? "needs" : "need"} you</span>` : c.busy ? `<span class="badge">${c.busy} running</span>` : "";
  return `<div class="sbtop" data-tauri-drag-region>${lights(app)}<button class="iconbtn" data-act="pref" data-k="sessions" aria-label="Hide the sessions" title="Hide the sessions">${icon("sidebar")}</button></div>` +
    `<button class="newbtn" data-act="newsession">${icon("compose")}New session</button>` +
    `<button class="navrow${app.S.view === "overview" ? " cur" : ""}" data-act="overview">${icon("grid")}<span>Overview</span><span class="grow"></span>${badge}</button>` +
    `<div class="sbtools"><label class="searchbox">${icon("search", "s")}<input id="sessq" placeholder="Search sessions" value="${esc(app.S.sessQ)}" aria-label="Search sessions"></label></div>` +
    `<div class="slist" id="slist">${sessionList(app)}</div>`;
}

export function toolbarHTML(app) {
  const s = app.cur(), v = app.S.view, mood = app.mood();
  let h = app.prefs.sessions ? "" : `${lights(app)}<button class="iconbtn" data-act="pref" data-k="sessions" aria-label="Show the sessions" title="Show the sessions">${icon("sidebar")}</button>`;
  if (v === "overview") h += `<div class="ttl" data-tauri-drag-region>${birdArt(app.theme, mood, app.light, 3, false)}<span class="project">Overview</span><span class="where">every session, in every folder</span></div><span class="grow" data-tauri-drag-region></span>`;
  else if (v === "nofolder" || !s) h += `<div class="ttl" data-tauri-drag-region>${birdArt(app.theme, mood, app.light, 3, false)}<button class="pillbtn" data-act="newsession">${icon("folder")}Choose a folder</button></div><span class="grow" data-tauri-drag-region></span>`;
  else h += `<div class="ttl" data-tauri-drag-region>${birdArt(app.theme, mood, app.light, 3, false)}<span class="project">${esc(app.folderName(s.root))}</span><span class="where">${esc(app.tilde(s.root))}</span></div><span class="grow" data-tauri-drag-region></span>`;
  if (s && v !== "overview" && v !== "nofolder") {
    const f = s.state.facts, [level] = levelOf(f.level), full = String(f.level).toLowerCase().startsWith("full");
    h += `<button class="pillbtn" data-act="models" title="Models">${esc(f.model || "No model")}${f.effort ? `<span class="mut">· ${esc(f.effort)}</span>` : ""}${icon("down", "s")}</button>`;
    if (f.level) h += `<button class="pillbtn${full ? " warn" : ""}" data-act="level" title="Sandbox level">${icon(full ? "warn" : "shield")}${esc(level)}${icon("down", "s")}</button>`;
  }
  h += `<button class="iconbtn" data-act="settings" aria-label="Settings" title="Settings">${icon("settings")}</button>` +
    `<button class="iconbtn" data-act="help" aria-label="Help" title="Help">${icon("help")}</button>`;
  if (v !== "overview" && v !== "nofolder") h += `<button class="iconbtn" data-act="pref" data-k="card" aria-pressed="${app.prefs.card}" aria-label="${app.prefs.card ? "Hide" : "Show"} the session card" title="Session card">${icon("inspector")}</button>`;
  return h;
}

const dots = () => `<span class="dots" aria-hidden="true"><i></i><i></i><i></i></span>`;

export function statusHTML(app) {
  const s = app.cur(), v = app.S.view;
  let left;
  if (v === "nofolder" || !s) left = `<span>No folder chosen</span>`;
  else if (s.ended && !s.busy) left = `<span>This session has ended</span>`;
  else {
    const act = s.state.activity, n = app.turnsOf(s).cells;
    if (act === "awaiting_you") left = `<span class="warn">Waiting for you · answer the question above</span>`;
    else if (working(act)) {
      const said = act === "streaming" ? (s.state.streaming.tool != null ? `Writing cell ${pad3(n + 1)}` : "Writing") : act === "executing" ? `Executing cell ${pad3(n)}` : act === "compacting" ? "Compacting the context" : "Thinking";
      left = `${dots()}<span class="live">${said}${s.stopAsked ? " · stop requested" : ""}</span>`;
    } else if (act === "complete") left = `<span class="ok">${icon("check", "s")}</span><span>Complete</span>`;
    else if (act === "failed") left = `<span class="fail">${icon("x", "s")}</span><span>Failed</span>`;
    else if (act === "stopped") left = `<span>Stopped</span>`;
    else if (act === "interrupted") left = `<span>Interrupted</span>`;
    else left = `<span>Ready</span>`;
  }
  const signing = app.S.signin && app.S.sheet !== "signin" ? `<button class="txt" data-act="signin-reopen">Signing in to ${esc(app.S.signin.label)}${icon("right", "s")}</button>` : "";
  // The host went and did not come back: say so, with the way to start it again.
  const lost = app.S.hostLost ? `<button class="txt warn" data-act="retry-host" title="${esc(app.S.hostLost)}">The engine stopped · Try again${icon("right", "s")}</button>` : "";
  const others = app.listSessions().filter((x) => x.id !== s?.id && app.liveOf(x)?.kind === "waiting").length;
  const elsewhere = others ? `<button class="txt warn" data-act="overview">${plural(others, "other session needs", "other sessions need")} you${icon("right", "s")}</button>` : "";
  return `${left}<span class="grow"></span>${lost}${signing}${elsewhere}`;
}

export function cbarHTML(app) {
  const s = app.cur(), draft = app.draft();
  const empty = !draft.trim() || app.S.view === "nofolder" || !s;
  const busy = s && s.busy;
  const stop = busy && s.state.activity !== "awaiting_you"
    ? (s.stopAsked
      ? `<button class="sendlabel" style="background:var(--press);color:var(--text)" data-act="cancel" title="Cancel the call in flight">${icon("x", "s")}Cancel the call</button>`
      : `<button class="round stop" data-act="stop" aria-label="Stop after this cell" title="Stop after this cell">${icon("stop")}</button>`)
    : "";
  const send = busy
    ? `<button class="sendlabel" id="send" data-act="send" ${empty ? "disabled" : ""} title="Held until this turn ends">${icon("send", "s")}Queue</button>`
    : `<button class="round primary" id="send" data-act="send" ${empty ? "disabled" : ""} aria-label="Send" title="Send">${icon("send")}</button>`;
  return `<span class="grow"></span>${stop}${send}`;
}

export function queueHTML(app) {
  const s = app.cur();
  const items = s?.state.queue || [];
  if (!items.length) return "";
  // The session takes back the newest; that one carries the button.
  return `<div class="queue"><div class="qh">Queued · sent when this turn ends</div>` +
    items.map((q, i) => `<div class="qitem"><span class="qt">${esc(q)}</span>${i === items.length - 1 ? `<button class="btn small" data-act="takeback">Take back</button>` : ""}</div>`).join("") + `</div>`;
}

export function toastHTML(app) {
  if (!app.noticeOn() || (app.S.sheet && app.S.sheet !== "quit")) return "";
  const u = app.S.undo;
  return `<div class="toast${u ? "" : " solo"}" role="status">${esc(app.S.notice)}${u ? `<button data-act="undo">${esc(u.label || "Undo")}</button>` : ""}</div>`;
}
