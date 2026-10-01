// The session card: the bird on its perch and this session's facts --
// its id and the turn's clock, what it has done so far, its guardrails and
// how full the model's context is. Each group can be turned off in Settings.
import { esc, clock, kilo, plural } from "../util.js";
import { icon } from "../icons.js";
import { birdArt } from "../birds.js";
import { levelOf } from "./convo.js";
import { diffFacts } from "../record.js";

export function inspectorHTML(app) {
  const p = app.prefs, t = app.theme, s = app.cur();
  if (!s) return "";
  const f = s.state.facts, [level, asks] = levelOf(f.level), full = String(f.level).toLowerCase().startsWith("full");
  let h = "";
  // The bird perches on the session card.
  if (p.cardBird && t.art) h += `<div class="group birdcard">${birdArt(t, app.mood(), app.light, 4)}<div><b>${esc(t.name)}</b><span>${esc(t.latin)}</span><span>${esc(t.nest)}</span></div></div>`;
  const ctx = s.state.notebook?.context;
  const ctxHTML = () => {
    if (!ctx || ctx.used == null) return "";
    const cap = ctx.cap, pct = cap ? Math.round((ctx.used / cap) * 100) : null;
    return `<div><div class="ghd">Context</div><div class="group ctx">${pct != null ? `<div class="big">${pct}% full</div><div class="meter"><i style="width:${Math.max(pct, 1)}%"></i></div>` : `<div class="big">${kilo(ctx.used)} tokens</div>`}` +
      `<div class="mut" style="font-size:13px">${kilo(ctx.used)}${cap ? ` of ${kilo(cap)}` : ""} tokens</div></div></div>`;
  };
  const guard = `<div><div class="ghd">Guardrails</div><div class="group">` +
    `<button class="row" data-act="level"><span class="${full ? "warn" : "mut"}">${icon(full ? "warn" : "shield")}</span><span><span class="${full ? "warn" : ""}">${esc(level)}</span>${asks ? `<span class="sub">${esc(asks)}</span>` : ""}</span><span class="v chev">${icon("right", "s")}</span></button>` +
    (f.network ? `<div class="row">Network<span class="v">${esc(f.network)}</span></div>` : "") + `</div></div>`;
  const tools = p.cardTools ? `<div><div class="ghd">Instruments</div><div class="group"><button class="row" data-act="sheet" data-v="activity">Activity<span class="v chev">${icon("right", "s")}</span></button><button class="row" data-act="sheet" data-v="telemetry">Telemetry<span class="v chev">${icon("right", "s")}</span></button></div></div>` : "";
  if (app.S.view === "new" && !app.turnsOf(s).turns.length) return (p.cardGuard && f.level ? guard : "") + (p.cardContext ? ctxHTML() : "") + tools;
  const cells = s.state.reading?.cells || [];
  const ran = cells.filter((c) => c.tone === "success").length, failed = cells.filter((c) => c.tone === "failure").length;
  const running = s.busy && cells.length < app.turnsOf(s).cells ? 1 : 0;
  const breakdown = [ran && `${ran} ran`, running && `${running} running`, failed && `${failed} failed`].filter(Boolean).join(", ");
  const files = new Set((s.state.notebook?.cells || []).filter((c) => !c.rolled_back).flatMap((c) => diffFacts(c.changes).files));
  const u = s.state.usage || {};
  const turn = s.busy && s.turnStart
    ? `<span class="tnum" data-clock="clock" data-since="${s.turnStart}"${s.state.activity === "awaiting_you" ? ` data-until="${s.state.since}"` : ""}>${clock((s.state.activity === "awaiting_you" ? s.state.since : Date.now()) - s.turnStart)}</span>`
    : s.state.activity === "complete" ? "Complete" : s.state.activity === "idle" ? "Not started" : esc(s.state.activity[0].toUpperCase() + s.state.activity.slice(1));
  if (p.cardSession) h += `<div><div class="ghd">This session</div><div class="group"><div class="row">Session<span class="v mono" style="font-size:13px">${esc(s.id)}</span></div><div class="row">This turn<span class="v">${turn}</span></div></div></div>`;
  if (p.cardSoFar) h += `<div><div class="ghd">So far</div><div class="group">` +
    `<div class="row"><span>Cells${breakdown ? `<span class="sub">${breakdown}</span>` : ""}</span><span class="v">${app.turnsOf(s).cells}</span></div>` +
    `<div class="row">Files changed<span class="v">${files.size}</span></div>` +
    `<div class="row">Tokens used<span class="v">${kilo((u.input_tokens || 0) + (u.output_tokens || 0))}</span></div>` +
    // Where a provider reports no reasoning count, the card has no such line.
    (u.reasoned_tokens ? `<div class="row">Reasoned<span class="v">${kilo(u.reasoned_tokens)} tok</span></div>` : "") +
    (u.requests ? `<div class="row">Requests<span class="v">${plural(u.requests, "request", "requests").split(" ")[0]}</span></div>` : "") + `</div></div>`;
  if (p.cardGuard && f.level) h += guard;
  if (p.cardContext) h += ctxHTML();
  return h + tools;
}
