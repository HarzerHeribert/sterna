// The sheets: every setup surface is a laid-out sheet, opened by a click,
// closed by Done. The folder choice, Settings, Help and the quit question
// are here; the setup sheets and what a session builds have files of their own.
import { esc, plural, agoMs } from "../util.js";
import { icon } from "../icons.js";
import { birdArt, svg, pixels } from "../birds.js";
import { THEMES, FAMILIES, accentOf, hex } from "../theme.js";
import { levelOf } from "./convo.js";
import { head, EFFORTS, modelsSheet, signinSheet, keySheet, levelSheet } from "./choice-sheets.js";
import { panelSheet, formSheet, activitySheet, telemetrySheet } from "./panel-sheets.js";
import { releasesPane } from "./releases.js";

const sw = (on) => `<span class="switch${on ? " on" : ""}" aria-hidden="true"></span>`;

// -- folder ----------------------------------------------------------------

function folderSheet(app) {
  const sel = app.S.pickFolder, first = !app.cur() && app.S.view === "nofolder";
  const rows = app.folders().map((f) => {
    const n = f.sessions.length, last = f.sessions[0]?.last_used || f.last_used;
    const used = n ? `${plural(n, "session", "sessions")} · last used ${agoMs(last)}` : "no sessions yet";
    return `<button class="frow" data-act="pickfolder" data-f="${esc(f.root)}" aria-pressed="${sel === f.root}"><span class="fi">${icon("folder")}</span>` +
      `<span class="fx"><b>${esc(app.folderName(f.root))}</b><span class="p">${esc(app.tilde(f.root))}</span><span class="d">${used}</span></span><span class="ck">${sel === f.root ? icon("check") : ""}</span></button>`;
  }).join("");
  const foot = app.noticeOn() ? `<b style="color:var(--text)">${esc(app.S.notice)}</b>` : `<span>${sel ? `Starts in ${esc(app.tilde(sel))}` : "Pick a folder to start"}</span>`;
  return `<div class="sheet narrow" role="dialog" aria-modal="true" aria-labelledby="fo-t">
    ${head("fo-t", first ? "Choose a folder" : "New session", "Every session runs in one folder. Sterna reads it, and the sandbox lets it write there and nowhere else.", null)}
    <div class="sbd">${rows ? `<div class="ghd">Recent folders</div><div class="group">${rows}</div>` : ""}
      <div class="group" style="margin-top:12px"><button class="row" data-act="browse"><span class="mut">${icon("folderplus")}</span>Choose ${rows ? "another" : "a"} folder…<span class="v chev">${icon("right", "s")}</span></button></div>
      ${app.bridge.drops ? `<p class="cap2">Or drop a folder onto this window.</p>` : ""}</div>
    <div class="sft">${foot}<span class="grow"></span>
      <button class="btn" data-act="close">Cancel</button><button class="btn primary" data-act="startsession"${sel && !app.S.starting ? "" : " disabled"}>${app.S.starting ? "Starting…" : "Start session"}</button></div>
  </div>`;
}

// -- help ------------------------------------------------------------------

function helpSheet() {
  const K = [["Send", "Enter"], ["New line", "Shift-Enter"], ["Stop after this cell, unless a question or an approval is in focus", "Esc"], ["Cancel the call in flight", "Esc twice"], ["Models", "F3"], ["Settings", "⌘ ,"], ["New session", "⌘ N"], ["Select the previous or next cell", "Alt ↑ ↓"], ["Fold or open the selected cell", "Ctrl O"], ["The selected cell's changes", "F4"], ["With an approval in focus: allow once, for the session, deny", "O · S · D"], ["With an approval in focus: refuse this once", "Esc"]];
  return `<div class="sheet narrow" role="dialog" aria-modal="true" aria-labelledby="h-t">
    ${head("h-t", "Help", "Everything is a tap or a click. With a keyboard, these do the same.")}
    <div class="sbd"><div class="group">${K.map(([d, k]) => `<div class="row">${d}<span class="v"><kbd>${k}</kbd></span></div>`).join("")}</div></div>
  </div>`;
}

// -- settings --------------------------------------------------------------

const SECTIONS = [["appearance", "Appearance", "palette"], ["sessions", "Sessions", "shield"], ["window", "Window", "window"], ["notify", "Notifications", "bell"], ["cells", "Cells", "cell"], ["motion", "Motion", "motion"], ["instruments", "Instruments", "gauge"], ["releases", "Updates", "release"]];

function toggleRow(app, k, label, sub, disabled) {
  const on = app.prefs[k];
  return `<button class="row" data-act="${disabled ? "noop" : "pref"}" data-k="${k}" role="switch" aria-checked="${on && !disabled}"${disabled ? ' aria-disabled="true"' : ""}><span>${label}${sub ? `<span class="sub">${sub}</span>` : ""}</span>${sw(on && !disabled)}</button>`;
}
const seg = (app, k, opts) => `<div class="segctl full big">${opts.map(([v, l]) => `<button data-act="setpref" data-k="${k}" data-v="${v}" aria-pressed="${app.prefs[k] === v}">${l}</button>`).join("")}</div>`;
function tile(app, t) {
  const a = accentOf(t, app.light);
  const face = t.art ? svg(pixels(t.art, "idle", app.light), 2, t.name) : `<span class="sw" style="background:${a == null ? `linear-gradient(90deg,${app.light ? "#15191f" : "#eceef1"} 50%,transparent 50%)` : hex(a)}"></span>`;
  return `<button class="tile" data-act="setpref" data-k="theme" data-v="${t.id}" aria-pressed="${app.prefs.theme === t.id}">${face}<span>${esc(t.name)}</span></button>`;
}

function settingsPane(app) {
  const t = app.theme, bird = !!t.art, saved = app.S.settings || {};
  switch (app.S.set) {
    case "appearance": return `<div class="ghd">Appearance</div>${seg(app, "appearance", [["light", "Light"], ["dark", "Dark"], ["system", "Match the system"]])}
      <div class="ghd">Theme</div>
      <div class="preview">${t.art ? birdArt(t, "idle", app.light, 4) : `<span class="sw" style="width:56px;height:56px;background:var(--accent)"></span>`}
        <div class="pt"><b>${esc(t.name)}</b><span>${t.art ? `${esc(t.latin)} · ${esc(t.nest)}` : "A palette alone: the accent is the whole of it."}</span>
        <div class="sample"><span class="pill live"><span class="pulse"></span>Running 0:07</span><span class="pill ok">${icon("check", "s")}Executed</span><span class="pill fail">${icon("x", "s")}Failed</span>${sw(true)}<button class="btn small primary" data-act="noop">Send</button></div></div></div>
      <p class="cap2">Only the accent moves with the theme. Failure, warning and success keep their own colours in every theme, so nothing live can read as an error. The theme is the one /theme sets in the terminal.</p>
      ${FAMILIES.map((fam) => `<div class="ghd">${fam}</div><div class="tiles">${THEMES.filter((x) => x.fam === fam).map((x) => tile(app, x)).join("")}</div>`).join("")}
      <p class="cap2" style="margin-top:14px">The birds are traced from photographs; the credits are in the README.</p>`;
    case "sessions": {
      const [level, asks] = levelOf(saved["sandbox.level"]);
      return `<div class="ghd">For the sessions you start next</div><div class="group">
        <button class="row" data-act="models"><span>Model<span class="sub">Main answers you</span></span><span class="v mono" style="font-size:13px">${esc(saved["model.parent"] || "Not set")}</span><span class="v chev" style="margin-left:6px">${icon("right", "s")}</span></button>
        <button class="row" data-act="level"><span>Sandbox level<span class="sub">${esc(asks || "")}</span></span><span class="v">${esc(saved["sandbox.level"] ? level : "Not set")}</span><span class="v chev" style="margin-left:6px">${icon("right", "s")}</span></button></div>
      <div class="ghd">Effort</div><div class="segctl full big">${EFFORTS.map((e) => `<button data-act="settingseg" data-k="session.effort" data-v="${e}" aria-pressed="${saved["session.effort"] === e}">${e}</button>`).join("")}</div>
      <p class="cap2">These are your settings, the same the terminal reads. A session already running keeps what it has; change it there from its toolbar.</p>`;
    }
    case "window": return `<div class="ghd">Panels</div><div class="group">${toggleRow(app, "sessions", "Sessions list", "Every folder and its sessions, on the left")}${toggleRow(app, "card", "Session card", "This session's facts, on the right")}</div>
      <div class="ghd">On the session card</div><div class="group">
        ${toggleRow(app, "cardBird", "The bird", bird ? `The theme's bird, the ${esc(t.name)}, at the top of the card` : "Choose a bird theme to have one", !bird)}
        ${toggleRow(app, "cardSession", "This session", "Its id and the turn's clock")}
        ${toggleRow(app, "cardSoFar", "So far", "Cells, files changed, tokens used and reasoned")}
        ${toggleRow(app, "cardGuard", "Guardrails", "The sandbox level and the network")}
        ${toggleRow(app, "cardContext", "Context", "How full the model's context is")}</div>`;
    case "notify": return `<div class="ghd">Tell me, while I am in another app or session</div><div class="group">
        ${toggleRow(app, "notifyAsk", "When a session needs me", "A question or an approval waits, in any folder")}
        ${toggleRow(app, "notifyDone", "When a session finishes", "Its answer is in, and nobody has read it yet")}
        ${toggleRow(app, "dockBadge", "Count them on the app icon", "How many sessions wait on you")}</div>
      <p class="cap2">Every session keeps running when you switch to another; the overview shows them all.</p>`;
    case "cells": return `<div class="ghd">While a cell is being written, show</div>${seg(app, "stream", [["actions", "Each action"], ["code", "The program"], ["raw", "Raw"]])}
      <p class="cap2">Each action lists every call with its size as it arrives. Raw is the provider's own text, for debugging the protocol. The terminal shows the same.</p>
      <div class="ghd">On cards and rows</div><div class="group">
        ${toggleRow(app, "summary", "Reasoning summary", "The newest sentence of what the model is thinking, beside the clock")}
        ${toggleRow(app, "handles", "Handles kept live", "The names a cell's results stay under, at the foot of its card")}</div>`;
    case "motion": return `<div class="ghd">Motion</div>${seg(app, "motion", [["full", "Full"], ["calm", "Calm"], ["off", "Off"]])}
      <p class="cap2">Motion is decoration: the start's flight, the moving marks, the bird's blink. Clocks and states stay on screen at every setting.</p>`;
    case "releases": return releasesPane(app);
    case "instruments": return `<div class="ghd">Instruments</div><div class="group">
        <button class="row" data-act="sheet" data-v="activity">Activity<span class="sub" style="margin-left:8px">every notice this session</span><span class="v chev">${icon("right", "s")}</span></button>
        <button class="row" data-act="sheet" data-v="telemetry">Telemetry<span class="sub" style="margin-left:8px">requests, tokens and time</span><span class="v chev">${icon("right", "s")}</span></button></div>
      <div class="group" style="margin-top:12px">${toggleRow(app, "cardTools", "Show them on the session card", "Off keeps the card to the session's own facts")}</div>`;
  }
  return "";
}

function settingsSheet(app) {
  return `<div class="sheet settings" role="dialog" aria-modal="true" aria-labelledby="s-t">
    ${head("s-t", "Settings", "A choice saves at once.")}
    <div class="setgrid"><nav class="setnav">${SECTIONS.map(([k, l, ic]) => `<button data-act="section" data-v="${k}" aria-current="${app.S.set === k}">${icon(ic)}${l}</button>`).join("")}</nav>
    <div class="setpane" id="setpane">${settingsPane(app)}</div></div>
  </div>`;
}

// -- quit ------------------------------------------------------------------

function quitSheet(app) {
  // Only what this app's host started: a terminal's session is not this window's to stop.
  const running = app.startedRunning();
  const n = running.length;
  return `<div class="sheet narrow" role="dialog" aria-modal="true" aria-labelledby="q-t">
    ${head("q-t", "Quit Sterna", `${plural(n, "session this app started is", "sessions this app started are")} still running.`, null)}
    <div class="sbd"><div class="group">${running.slice(0, 6).map((x) => `<div class="row"><span>${esc(x.title)}<span class="sub">${esc(app.folderName(x.root))}</span></span></div>`).join("")}${n > 6 ? `<div class="row mut">and ${n - 6} more</div>` : ""}</div>
      <p class="cap2">Kept running, they go on without the window; open Sterna again to see them. Stopped, each ends after what it is doing now, and what ran stands.</p></div>
    <div class="sft"><button class="btn" data-act="close">Cancel</button><span class="grow"></span><button class="btn" data-act="quit-stop">Stop them</button><button class="btn primary" data-act="quit-keep">Keep running in the background</button></div>
  </div>`;
}

export function overlayHTML(app) {
  const make = { folder: folderSheet, models: modelsSheet, signin: signinSheet, key: keySheet, level: levelSheet, help: helpSheet, settings: settingsSheet, quit: quitSheet, panel: panelSheet, form: formSheet, activity: activitySheet, telemetry: telemetrySheet }[app.S.sheet];
  return make ? make(app) : "";
}
