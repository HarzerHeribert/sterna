// The conversation: the opening, each turn's message, prose, cells and
// answer, and the live edge -- the reasoning row, the cell being written,
// a question waiting for you -- drawn from the session's events. A cell's
// state and its line are the engine's words (`reading`).
import { esc, plural, pad3, clock, secs, prose, splitAnswer, inlineHTML, timeOf, agoMs } from "../util.js";
import { icon } from "../icons.js";
import { birdArt } from "../birds.js";
import { noCalls, writing, diffRows, diffFacts, settledCell } from "../record.js";
import { working } from "../session.js";

const dots = () => `<span class="dots" aria-hidden="true"><i></i><i></i><i></i></span>`;
const THINKING = new Set(["thinking", "starting", "waiting", "compacting", "searching"]);
const CALLS = /\b(bash|read|edit|grep|glob|write|answer|ask|fetch|search|agent\.run)\(/g;
const LEVEL_WORDS = {
  ask: ["Ask", "Asks before every edit and command"],
  sandboxed: ["Sandboxed", "Asks only to leave the sandbox"],
  full: ["Full access", "Nothing is asked"],
};
export const levelOf = (word) => LEVEL_WORDS[String(word || "").toLowerCase().replace("full access", "full")] || [word || "Unknown", ""];

function hl(code) {
  return String(code).split("\n").map((line) => line.trim().startsWith("//")
    ? `<span class="cm">${esc(line)}</span>`
    : esc(line).replace(/\b(bash|read|edit|grep|glob|write|answer|ask|fetch)(?=\()/g, '<span class="fn">$1</span>')).join("\n");
}
// The answer is the block under the card, so in the program the call is
// folded to its opening words.
function foldAnswer(code, answered) {
  return hl(String(code).replace(/answer\((["'`])((?:\\.|(?!\1).){0,34})(?:\\.|(?!\1)[\s\S])*\1\);/, (_, q, head) => `answer(${q}${head}…${q});${answered ? "\u0001" : ""}`))
    .replace("\u0001", '<span class="cm">  · the answer is below</span>');
}

function callRow(app, c) {
  const [ic, tone] = {
    ok: [icon("ok"), "ok"], fail: [icon("fail"), "fail"], deny: [icon("deny"), "warn"],
    run: ['<span class="spin"></span>', "acc"], wait: [icon("wait"), "warn"],
  }[c.mark] || [icon("ok"), "ok"];
  const word = c.word ? c.word[0].toUpperCase() + c.word.slice(1) : "";
  const tone2 = c.mark === "wait" ? "warn" : tone;
  return `<div class="call"><span class="ic ${tone}">${ic}</span>` +
    `<div><div class="c1"><span class="tool">${esc(c.tool)}</span>${inlineHTML(c.arg)}</div>${c.detail ? `<div class="c2">${esc(c.detail)}</div>` : ""}</div>` +
    `<span class="word ${tone2 === "ok" ? "mut" : tone2}">${esc(word)}</span></div>`;
}

function diffHTML(diff) {
  return diffRows(diff).map((f) => {
    const rows = f.rows.map((r) => r.kind === "hunk" ? `<div class="dhunk">${esc(r.text)}</div>`
      : `<div class="dl${r.kind === "add" ? " add" : r.kind === "del" ? " del" : ""}"><span class="n">${r.o}</span><span class="n">${r.n}</span><span class="s">${r.kind === "add" ? "+" : r.kind === "del" ? "−" : " "}</span><span class="t">${esc(r.text)}</span></div>`).join("");
    return `<div class="dhead"><span>${f.path ? `<a class="path" data-act="path" data-v="${esc(f.path)}">${esc(f.path)}</a>` : ""}</span><span>Before → after this cell · already applied</span></div><div class="diff">${rows}</div>`;
  }).join('<div style="height:10px"></div>');
}

/** An approval or a question, where the call waits: answered by a click. */
export function askHTML(app, session, prompt, { overview = false } = {}) {
  const sid = esc(session.id), pid = prompt.id;
  if (prompt.type === "question") {
    const choices = (prompt.choices || []).map((c) => `<button class="btn" data-act="choose" data-s="${sid}" data-p="${pid}" data-v="${esc(c)}">${esc(c)}${prompt.guess === c ? `<span class="guess">Sterna's guess</span>` : ""}</button>`).join("");
    return `<div class="ask" role="group" aria-label="A question for you" tabindex="-1" data-s="${sid}" data-p="${pid}"><div class="h">${overview ? "" : icon("help")}A question for you</div><p>${esc(prompt.question)}</p>` +
      `<div class="choices">${choices}</div><div class="row2"><span class="grow"></span>` +
      (overview ? `<button class="plain" data-act="open" data-id="${sid}">Open the session${icon("right", "s")}</button>` : `<button class="plain m" data-act="dismiss" data-s="${sid}" data-p="${pid}">Leave it to Sterna</button>`) + `</div></div>`;
  }
  if (prompt.type !== "approval") return "";
  const hosts = (prompt.hosts || []).filter(Boolean), host = hosts.join(", ");
  const raw = app.S.raw[`${session.id}:${pid}`];
  const head = hosts.length ? `The sandbox refused ${esc(host)}` : esc(prompt.label || prompt.tool || "A call waits for you");
  const can = prompt.complete !== false;
  const caption = hosts.length
    ? `Allow once runs the command outside the sandbox this time only. Allow for this session lets ${esc(host)} through until the session ends, with the command still inside the sandbox. Deny refuses this call and every identical one this session; Sterna is told and works on.`
    : "Allow once runs this call this time only. Allow for this session allows every identical call until the session ends. Deny refuses this call and every identical one this session; Sterna is told and works on.";
  const once = " Refuse this once refuses only this call: the next identical one asks again.";
  const btn = (a, words) => `<button class="btn" data-act="approve" data-s="${sid}" data-p="${pid}" data-a="${a}"${!can && (a === "once" || a === "session") ? " disabled" : ""}>${words}</button>`;
  return `<div class="ask" role="group" aria-label="A call waits for you" tabindex="-1" data-s="${sid}" data-p="${pid}">
    <div class="h">${overview ? "" : icon("shield")}${head}</div>
    ${prompt.reason ? `<p>${inlineHTML(prompt.reason)}</p>` : ""}
    ${prompt.confirmation ? `<pre class="code wrap">${esc(prompt.confirmation)}</pre>` : ""}
    ${raw ? `<pre class="code wrap" style="margin-top:8px">${esc(JSON.stringify(prompt.arguments || {}, null, 2))}</pre>` : ""}
    ${can ? "" : `<p class="cap2 fail">This call does more than the confirmation shows, so it cannot be allowed.</p>`}
    <div class="row2">${btn("once", "Allow once")}${btn("session", "Allow for this session")}${btn("deny", "Deny")}${btn("notnow", "Refuse this once")}<span class="grow"></span>` +
    (overview ? `<button class="plain" data-act="open" data-id="${sid}">Open the session${icon("right", "s")}</button>` : `<button class="plain m" data-act="raw" data-k="${sid}:${pid}">${raw ? "Hide" : "Show"} the exact arguments</button>`) + `</div>
    ${overview ? "" : `<p class="cap2">${caption}${once}</p>`}
  </div>`;
}

const pillOf = (mode, reading, since) => ({
  running: `<span class="pill live"><span class="pulse"></span>Running <span class="tnum" data-clock="clock" data-since="${since}">${clock(Date.now() - since)}</span></span>`,
  waiting: `<span class="pill warn">Waiting for you <span class="tnum" data-clock="clock" data-since="${since}">${clock(Date.now() - since)}</span></span>`,
}[mode] || (() => {
  const tone = { success: "ok", failure: "fail", warning: "warn" }[reading?.tone] || "mut";
  const word = String(reading?.state || "Recorded").toLowerCase();
  const ic = { ok: icon("check", "s"), fail: icon("x", "s"), warn: icon("left", "s") }[tone] || "";
  return `<span class="pill ${tone === "mut" ? "" : tone}" style="${tone === "mut" ? "color:var(--muted);background:var(--press)" : ""}">${ic}${esc(word[0].toUpperCase() + word.slice(1))}</span>`;
})());

/** One cell's card. `mode` is done, running or waiting. */
function cellHTML(app, session, item, { mode, latest, prompts }) {
  const { n, view, reading, code } = item;
  const key = `${session.id}:c${n}`;
  const calls = [...(item.calls || [])];
  const answerOnly = !calls.length && view.returned != null && !view.error;
  const open = app.isOpen(key, mode !== "done" || (latest && !answerOnly));
  const failed = reading?.tone === "failure";
  const since = session.state.since || Date.now();
  const cls = mode === "done" ? (failed ? "failed" : "") : mode;
  // What the cell is for, in the model's words; a cell the model did not
  // name is described by its own size, a fact rather than a guess.
  const lines = String(code || "").split("\n").length;
  const named = String(view.description || "").trim();
  const purpose = named || plural(lines, "line", "lines");
  let h = `<article class="cell ${cls}${app.S.sel === key ? " sel" : ""}" id="cell-${n}" data-n="${n}">${mode === "running" ? '<div class="progress"><i></i></div>' : ""}` +
    `<button class="chead" data-act="cell" data-k="${key}" aria-expanded="${open}"><span class="num">${pad3(n)}</span>` +
    `<span class="purpose${named ? "" : " mut"}">${esc(purpose)}</span>${pillOf(mode, reading, since)}<span class="chev">${icon(open ? "up" : "down", "s")}</span></button>`;
  if (!open) return h + "</article>";
  // While a call waits on you, its row says so: the tool and what it acts on, from the prompt.
  if (mode === "waiting" && !calls.length) {
    for (const p of prompts.filter((q) => q.type === "approval")) {
      const host = (p.hosts || []).join(", ");
      calls.push({ tool: p.tool || "", arg: p.target || "", mark: "wait", word: "Waiting for you", detail: host ? `The sandbox refused ${host}` : "" });
    }
  }
  if (calls.length) h += `<div class="calls">${calls.map((c) => callRow(app, c)).join("")}</div>`;
  else if (noCalls(view.execution)) h += `<div class="calls"><div class="call"><span class="ic mut">${icon("ok")}</span><div class="c2" style="line-height:20px;color:var(--muted)">No tool calls ran in this cell</div><span></span></div></div>`;
  if (view.error) h += `<div class="note fail">${icon("deny", "s")}<span>${esc(view.error.class)}${view.error.message ? `: ${esc(view.error.message)}` : ""}${view.error.line ? ` · line ${view.error.line}` : ""}</span></div>`;
  if (view.yield_reason) h += `<div class="note live"><span>${esc(view.yield_reason)}</span></div>`;
  if (view.asked) h += `<div class="note live"><span class="pre-note">${esc(view.asked)}</span></div>`;
  if (mode === "running") h += `<div class="note live"><span>Executing this cell · <span class="tnum" data-clock="clock" data-since="${since}">${clock(Date.now() - since)}</span> elapsed · nothing is assumed complete${session.state.facts.model ? ` · <span class="mut">${esc(session.state.facts.model)}</span>` : ""}</span></div>`;
  if (mode === "waiting") h += prompts.map((p) => askHTML(app, session, p)).join("");
  const out = [view.stdout, view.output].filter((x) => x != null && String(x).trim()).join("\n");
  const tabs = [];
  if (out) tabs.push(["output", "Output", ""]);
  if (code) tabs.push(["program", "Program", plural(String(code).split("\n").length, "line", "lines")]);
  if (view.changes) { const d = diffFacts(view.changes); tabs.push(["changes", "Changes", `+${d.added} −${d.removed}`]); }
  if (tabs.length) {
    const fallback = view.changes && mode === "done" ? "changes" : out ? "output" : "program";
    const want = app.S.tab[key];
    const tab = tabs.some((t) => t[0] === want) ? want : tabs.some((t) => t[0] === fallback) ? fallback : tabs[0][0];
    // One view needs no switch: it is named, not offered.
    h += tabs.length > 1
      ? `<div class="tabs" role="tablist">${tabs.map(([k, l, c]) => `<button role="tab" data-act="tab" data-k="${key}" data-v="${k}" aria-selected="${tab === k}">${l}${c ? `<span class="n">${c}</span>` : ""}</button>`).join("")}</div>`
      : `<div class="tabcap">${tabs[0][1]}<span class="n">${tabs[0][2]}</span></div>`;
    let body = "";
    if (tab === "program") body = `<pre class="code">${foldAnswer(code, view.returned != null && mode === "done")}</pre>`;
    if (tab === "output") {
      const lines = out.split("\n"), full = app.isOpen(key + ".full", false), long = lines.length > 14;
      body = `<pre class="code wrap">${esc(long && !full ? lines.slice(-14).join("\n") : out)}</pre>`;
      if (long) body += `<button class="linkbtn" data-act="toggle" data-k="${key}.full">${full ? "Show the last lines only" : `Show all ${lines.length} lines`}${icon(full ? "up" : "down", "s")}</button>`;
    }
    if (tab === "changes") body = diffHTML(view.changes);
    h += `<div class="tabbody">${body}</div>`;
  }
  if (app.prefs.handles && view.table) h += `<div class="tablebox"><div class="tabcap">Handles kept live</div><pre class="code">${esc(view.table)}</pre></div>`;
  if (mode === "done" && reading) {
    const tone = { success: "ok", failure: "fail", warning: "warn", muted: "mut", line: "" };
    const words = (reading.parts?.length ? reading.parts : [{ text: reading.line, tone: "muted" }])
      .map((p) => `<span class="${tone[p.tone] ?? ""}">${esc(p.text)}</span>`).join("");
    h += `<div class="cfoot"><span>${words}</span></div>`;
  }
  return h + "</article>";
}

function answerHTML(app, session, item, turnItems) {
  const [first, rest] = splitAnswer(item.text);
  const changed = turnItems.filter((i) => i.kind === "cell" && i.view.changes && !i.view.rolled_back);
  const facts = item.facts;
  return `<div class="answer"><p class="first">${inlineHTML(first)}</p>${rest ? `<div class="rest md">${prose(rest)}</div>` : ""}` +
    (facts ? `<div class="facts"><span class="${facts.failed ? "fail" : "ok"}">${icon(facts.failed ? "x" : "check", "s")}</span>${esc(facts.facts)}</div>` : "") +
    `<div class="actions">${changed.length ? `<button class="btn" data-act="showdiff" data-k="${esc(session.id)}:c${changed[changed.length - 1].n}">Show the diff</button><button class="btn" data-act="insert" data-v="Commit this.">Commit this</button>` : ""}</div></div>`;
}

/** The newest sentence of what the model is reasoning. */
function latestSentence(text) {
  const parts = String(text || "").replace(/\s+/g, " ").trim().split(/(?<=[.!?])\s+/).filter(Boolean);
  return parts.length ? parts[parts.length - 1] : "";
}

function reasonRow(app, session, done) {
  const st = session.state, text = st.streaming.reasoning;
  // Whether the requests ask the model to reason is the engine's fact; an older engine's effort stands in.
  const asked = !!text || (typeof st.facts.reasoning === "boolean" ? st.facts.reasoning : !!st.facts.effort && st.facts.effort !== "auto");
  const said = app.prefs.summary && text ? `<span class="rs">${esc(latestSentence(text))}</span>` : "";
  if (done) return asked && session.reasoned ? `<div class="reason done"><span class="rh">Reasoned for ${secs(session.reasoned)}</span>${said}</div>` : "";
  const since = session.thinkingSince || st.since;
  const clockEl = `<span class="tnum" data-clock="s" data-since="${since}">${secs(Date.now() - since)}</span>`;
  const head = st.activity === "compacting" ? "Compacting the context" : asked ? "Reasoning" : "Waiting for the model";
  return `<div class="reason" id="reasoning"><span class="rh">${dots()}${head} · ${clockEl}</span>${said}</div>`;
}

function streamBody(app, tool) {
  const { code } = writing(tool);
  if (app.prefs.stream === "raw") return `<pre class="code wrap">${esc(tool)}</pre>`;
  if (app.prefs.stream === "actions") {
    const at = [...code.matchAll(CALLS)];
    const rows = [];
    if ((at[0]?.index ?? code.length) > 0) rows.push(["code", "", at[0]?.index ?? code.length]);
    at.forEach((m, i) => {
      const end = at[i + 1]?.index ?? code.length, seg = code.slice(m.index, end);
      const arg = (seg.match(/["'`]([^"'`]*)["'`]?/) || [])[1] || "";
      rows.push([m[1], arg.length > 46 ? arg.slice(0, 45) + "…" : arg, end - m.index]);
    });
    return `<div class="acts">${rows.map(([n, a, c], i) => `<div><span class="nm"${n === "code" ? ' style="color:var(--muted)"' : ""}>${n}</span> ${esc(a)} <span class="mut">· ${c} chars</span>${i === rows.length - 1 ? ' <span class="caret"></span>' : ""}</div>`).join("")}</div>`;
  }
  return `<pre class="code">${hl(code)}<span class="caret"></span></pre>`;
}

function writingHTML(app, session, n) {
  const tool = session.state.streaming.tool || "";
  const { code, description } = writing(tool);
  const actions = (code.match(CALLS) || []).length;
  return `<article class="cell running" id="cell-writing"><div class="chead" style="cursor:default"><span class="num">${pad3(n)}</span>` +
    `<span class="purpose${description ? "" : " mut"}">${esc(description || plural(code.split("\n").length, "line", "lines"))}</span><span class="pill live"><span class="pulse"></span>Writing <span class="tnum">${plural(code.length, "char", "chars")}</span></span></div>` +
    `<div class="wl">${dots()}<span>The model is writing this cell · ${plural(actions, "action", "actions")} so far · not executed</span></div>` +
    `<div class="tabbody">${streamBody(app, tool)}</div><div class="cfoot">Not executed yet</div></article>`;
}

/** The opening of a session nobody has asked anything in yet. */
function openingHTML(app, session) {
  const hr = new Date().getHours(), t = app.theme;
  const greet = hr >= 5 && hr <= 11 ? "Good morning. " : hr >= 12 && hr <= 17 ? "Good afternoon. " : hr >= 18 && hr <= 22 ? "Good evening. " : "";
  const [level, asks] = levelOf(session.state.facts.level);
  const chips = (session.state.suggestions || []).slice(0, 3);
  return `<div class="opening"><div class="perch">${birdArt(t, app.mood(), app.light, 7)}<div><h1 class="greet">${greet}What should we build?</h1>` +
    `<div class="who">${t.art ? `${esc(t.name)} · ${esc(t.nest)}<br>` : ""}${session.state.facts.level ? `<a class="path" data-act="level">${esc(level)} · ${esc(asks.toLowerCase())}</a>` : ""}</div></div></div>` +
    (chips.length
      ? `<p class="lead">Describe a task, or pick one of these:</p><div class="suggest">${chips.map(([label, types]) => `<button data-act="insert" data-v="${esc(types)}">${esc(label)}<span>${esc(types.length > 70 ? types.slice(0, 69) + "…" : types)}</span></button>`).join("")}</div>`
      : `<p class="lead">Describe a task in the box below.</p>`) + `</div>`;
}

function noFolderHTML(app) {
  return `<div class="opening"><div class="perch">${birdArt(app.theme, app.mood(), app.light, 7)}<div><h1 class="greet">Choose a folder to work in</h1>` +
    `<div class="who">Every session runs in one folder. Sterna reads it, and the sandbox lets it write there and nowhere else.</div></div></div>` +
    `<button class="btn primary" data-act="newsession">${icon("folder")}Choose a folder</button></div>`;
}

/** The record: everything the session has written down, turn by turn. */
export function recordHTML(app) {
  const v = app.S.view;
  if (v === "overview") return overviewHTML(app);
  const session = app.cur();
  if (v === "nofolder" || !session) return noFolderHTML(app);
  const { turns } = app.turnsOf(session);
  if (!turns.length) return session.busy ? "" : openingHTML(app, session);
  const st = session.state, act = st.activity;
  const prompts = st.prompts.filter((p) => p.type === "approval" || p.type === "question");
  // The newest cell that has no outcome yet is the one that runs.
  let liveN = 0;
  if (["executing", "awaiting_you", "searching"].includes(act)) {
    const all = turns.flatMap((t) => t.items.filter((i) => i.kind === "cell"));
    const last = all[all.length - 1];
    if (last && !settledCell(last.view)) liveN = last.n;
  }
  app.S.promptsInCell = liveN && act === "awaiting_you" && prompts.length ? new Set(prompts.map((p) => p.id)) : new Set();
  let h = "";
  turns.forEach((turn, ti) => {
    const latest = ti === turns.length - 1;
    if (turn.you != null) {
      const at = session.stamps.get(turn.index);
      h += `<div class="you"><div class="bubble">${esc(turn.you)}</div>${at ? `<div class="cap">${timeOf(at)}</div>` : ""}</div>`;
    }
    if (turn.items.length || (latest && working(act))) {
      const at = session.stamps.get(turn.index + 1);
      h += `<div class="turnhead"><b>Sterna</b>${at ? `<span>${timeOf(at)}</span>` : ""}</div>`;
    }
    turn.items.forEach((item, at) => {
      const next = turn.items[at + 1];
      // The card's title is most often the same sentence: said above it too, it is read twice.
      if (item.kind === "prose" && !(next?.kind === "cell" && String(next.view.description || "").trim() === item.text.trim())) h += `<div class="prose md">${prose(item.text)}</div>`;
      if (item.kind === "cell") {
        const mode = item.n === liveN ? (act === "awaiting_you" && prompts.length ? "waiting" : "running") : "done";
        h += cellHTML(app, session, item, { mode, latest, prompts });
      }
      if (item.kind === "answer") h += answerHTML(app, session, item, turn.items);
    });
  });
  return h;
}

/** The live edge: what is arriving now and is not yet part of the record. */
export function liveHTML(app) {
  const session = app.cur();
  if (!session || !["main", "new"].includes(app.S.view)) return "";
  const st = session.state, act = st.activity;
  let h = "";
  if (st.streaming.text) h += `<div class="prose md">${prose(st.streaming.text)}</div>`;
  if (THINKING.has(act)) h += reasonRow(app, session, false);
  if (act === "streaming" && st.streaming.tool != null) {
    h += reasonRow(app, session, true);
    h += writingHTML(app, session, app.turnsOf(session).cells + 1);
  }
  const loose = st.prompts.filter((p) => (p.type === "approval" || p.type === "question") && !app.S.promptsInCell?.has(p.id));
  h += loose.map((p) => askHTML(app, session, p)).join("");
  if (session.lost) h += `<div class="note fail" style="margin:8px 0 24px">${icon("warn", "s")}<span>This session's connection was lost. ${esc(session.lost)} <button class="btn small" data-act="reattach" data-s="${esc(session.id)}">Try again</button></span></div>`;
  else if (session.ended && !working(act)) h += `<p class="cap2" style="margin:8px 4px 24px">This session has ended: ${esc(session.ended)}.</p>`;
  return h;
}

// -- the overview ---------------------------------------------------------

export function liveMeta(l) {
  if (!l) return "";
  if (l.kind === "waiting") return `<span class="state warn">Waiting for you</span>`;
  if (l.kind === "unread") return `<span class="state"><span class="unread"></span>Finished · not read yet</span>`;
  const said = { running: l.cell ? `Executing cell ${pad3(l.cell)}` : "Executing", writing: l.cell ? `Writing cell ${pad3(l.cell)}` : "Writing", thinking: "Thinking" }[l.kind];
  return `<span class="state acc">${dots()}${said}</span>`;
}

/** A meta line: its parts joined by a middle dot. */
const metaLine = (...parts) => parts.filter(Boolean).join(`<span class="sep" aria-hidden="true">·</span>`);

/**
 * One session as a row, the same in every list of the overview: its title
 * (two lines at most) over its folder, its state and its time, in one text
 * column; `end` is the fixed column at the right.
 */
function sessionRowHTML(title, meta, end) {
  return `<span class="sx"><span class="st">${esc(title)}</span><span class="sm">${meta}</span></span><span class="se">${end}</span>`;
}

function overviewHTML(app) {
  const all = app.listSessions();
  const of = (x) => app.liveOf(x);
  const waiting = all.filter((x) => of(x)?.kind === "waiting");
  const busy = all.filter((x) => ["running", "writing", "thinking"].includes(of(x)?.kind));
  const unread = all.filter((x) => of(x)?.kind === "unread");
  const nfold = new Set(all.filter((x) => of(x) && of(x).kind !== "unread").map((x) => x.root)).size;
  const folder = (x) => `<span class="fw">${esc(app.folderName(x.root))}</span>`;
  const chev = `<span class="chev">${icon("right", "s")}</span>`;
  const ticking = (since) => `<span class="tnum" data-clock="clock" data-since="${since}">${clock(Date.now() - since)}</span>`;
  // The heading says what it counts: sessions at work, and apart from them, the ones waiting for you.
  const parts = [busy.length && `${plural(busy.length, "session", "sessions")} at work`,
    waiting.length && (busy.length ? `${waiting.length} ${waiting.length === 1 ? "waits" : "wait"} for you` : `${plural(waiting.length, "session waits", "sessions wait")} for you`)].filter(Boolean);
  let h = `<div class="ovhead"><h1 class="greet ovt">${parts.length ? parts.join(" · ") : "Nothing is running"}</h1>` +
    `<p class="lead">${parts.length ? `In ${plural(nfold, "folder", "folders")}. Answer a question here, or open a session to see it.` : "Start a session, or open one from the list."}</p></div>`;
  if (waiting.length) {
    h += `<div class="ghd">Needs you</div>` + waiting.map((x) => {
      const s = app.sessionById(x.id);
      const prompt = s?.state.prompts.find((p) => p.type === "approval" || p.type === "question");
      const since = s?.state.since || x.live?.since || Date.now();
      return `<article class="ovcard"><div class="srow">${sessionRowHTML(x.title, metaLine(folder(x)), `<span class="pill warn">Waiting for you ${ticking(since)}</span>`)}</div>` +
        `<div class="ovbody">${prompt ? askHTML(app, s, prompt, { overview: true }) : `<div class="row2"><span class="grow"></span><button class="plain" data-act="open" data-id="${esc(x.id)}">Open the session${icon("right", "s")}</button></div>`}</div></article>`;
    }).join("");
  }
  if (busy.length) {
    h += `<div class="ghd">Running</div><div class="group">` + busy.map((x) => {
      const since = app.sessionById(x.id)?.turnStart || x.live?.since;
      return `<button class="srow ovrow" data-act="open" data-id="${esc(x.id)}">${sessionRowHTML(x.title, metaLine(folder(x), liveMeta(of(x)), since && ticking(since)), chev)}</button>`;
    }).join("") + `</div>`;
  }
  if (unread.length) {
    h += `<div class="ghd">Finished, not read yet</div><div class="group">` + unread.map((x) => {
      const s = app.sessionById(x.id), facts = s?.state.reading?.answer?.facts;
      const state = `<span class="state"><span class="unread"></span>Finished</span>`;
      return `<button class="srow ovrow" data-act="open" data-id="${esc(x.id)}">${sessionRowHTML(x.title, metaLine(folder(x), state, s?.turnEnd && esc(agoMs(s.turnEnd)), ...String(facts || "").split(" · ").map(esc)), chev)}</button>`;
    }).join("") + `</div>`;
  }
  const midnight = new Date(); midnight.setHours(0, 0, 0, 0);
  const today = all.filter((x) => (x.last_used || 0) >= midnight.getTime()).length;
  const total = app.S.usage?.total;
  const cells = [...app.S.sessions.values()].reduce((n, s) => n + (s.state.reading?.cells?.length || 0), 0);
  const fmt = (n) => (n == null ? "–" : n >= 1000 ? (n / 1000).toFixed(n >= 100000 ? 0 : 1) + "k" : String(n));
  h += `<div class="ghd">Today, across every folder</div><div class="group ovstats">` +
    `<div><b>${today}</b><span>sessions worked in</span></div>` +
    `<div><b>${cells}</b><span>cells run</span></div>` +
    `<div><b>${fmt(total ? total.input_tokens + total.output_tokens : null)}</b><span>tokens used</span></div>` +
    `<div><b>${fmt(total?.requests)}</b><span>requests</span></div></div>`;
  return `<div class="ov">${h}</div>`;
}
