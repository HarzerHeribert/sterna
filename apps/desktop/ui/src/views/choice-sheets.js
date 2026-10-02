// The setup sheets (plan goal 16), each a laid-out sheet: Models from the
// gateway's accounts, a sign-in with its pasted address, a provider's key,
// and the sandbox level. With a session open they change it; with none,
// they save the setting for the sessions to come.
import { esc } from "../util.js";
import { icon } from "../icons.js";
import { levelOf } from "./convo.js";

export const EFFORTS = ["auto", "low", "medium", "high", "xhigh", "max"];
export const LEVELS = [
  ["ask", "Ask", "Every edit and command asks first. Nothing leaves the project."],
  ["sandboxed", "Sandboxed", "Everything in the project runs. Leaving the sandbox asks."],
  ["full", "Full access", "No sandbox, nothing asks. Refused commands stay refused."],
];
const lower = (v) => String(v ?? "").toLowerCase();
export const head = (id, title, sub, done = "Done") => `<div class="shd"><div><div class="t" id="${id}">${esc(title)}</div>${sub ? `<div class="st">${sub}</div>` : ""}</div>${done ? `<button class="done" data-act="close">${done}</button>` : ""}</div>`;

/** The session the sheets act on, when one is open and running. */
const live = (app) => { const s = app.cur(); return s && !s.ended ? s : null; };

// -- models ----------------------------------------------------------------

/** An account's kind and what it needs: a subscription signs in, a key account has its key. */
function accountState(a) {
  if (a.authenticated === false) return a.connect_with ? "signin" : "locked";
  if (a.selectable === false) return "locked";
  return a.authenticated === true ? "signed" : "key";
}

/** The models the accounts serve, as rows: usable ones first, in account order. */
function modelRows(app) {
  const m = app.S.m, q = m.q.trim().toLowerCase(), rows = [];
  const scores = app.S.catalogue?.intelligence || {};
  const score = (name) => scores[name] ?? scores[lower(name)] ?? null;
  for (const [ai, a] of (app.S.accounts?.list || []).entries()) {
    const state = accountState(a), avail = state === "signed" || state === "key";
    if (!m.all && !avail) continue;
    const name = a.provider && a.provider !== a.account ? `${a.account} (${a.provider})` : a.account;
    for (const model of a.models || []) {
      if (q && !lower(model).includes(q) && !lower(name).includes(q)) continue;
      rows.push({ a, ai, name, model, score: score(model), avail, state });
    }
  }
  if (m.byScore) rows.sort((x, y) => (y.score ?? -1) - (x.score ?? -1) || y.avail - x.avail);
  else rows.sort((x, y) => y.avail - x.avail || x.ai - y.ai);
  return rows;
}

export function modelsSheet(app) {
  const s = live(app), m = app.S.m, main = m.role === "main", f = s?.state.facts || {}, saved = app.S.settings || {};
  const cur = s ? (main ? f.model : f.subagents) : (main ? saved["model.parent"] : saved["agents.model"]);
  const effortNow = s ? f.effort : saved["session.effort"];
  const accounts = app.S.accounts;
  const rows = accounts?.list ? modelRows(app) : [];
  app.S.modelRows = rows;
  const scored = rows.some((r) => r.score != null);
  const mrow = (r, i) => `<button class="mrow${r.avail ? "" : " locked"}${m.focus === i ? " focus" : ""}" data-act="${r.avail ? "model" : "noop"}" data-i="${i}"${r.avail ? "" : ' aria-disabled="true"'}>` +
    `<span class="nm">${esc(r.model)}</span><span class="sc">${[!r.avail && "Locked", r.score != null && `★ ${Math.round(r.score)}`, m.byScore && r.name].filter(Boolean).map((w) => `<span>${esc(w)}</span>`).join(" ")}</span>` +
    `<span class="ck">${r.model === cur && r.avail ? icon("check") : ""}</span></button>`;
  let list = "";
  if (!accounts || (accounts.loading && !accounts.list?.length)) list = `<p class="cap2" style="padding:18px 0">Asking the gateway which accounts it serves…</p>`;
  else if (accounts.error && !accounts.list?.length) {
    list = `<div class="ghd">No accounts</div><p class="cap2" style="font-size:13.5px;color:var(--text)">The gateway did not list its accounts: ${esc(accounts.error)}</p><button class="btn small" style="margin:8px 6px 0" data-act="accounts">Try again</button>`;
  } else if (m.byScore && scored) {
    list = rows.length ? `<div class="ghd">By measured intelligence</div><div class="group">${rows.map(mrow).join("")}</div>` : "";
  } else {
    // Every account, its sign-in or key, and its models; an account with none listed says so.
    const shown = (accounts.list || []).map((a, ai) => ({ a, ai, state: accountState(a) })).filter(({ state }) => m.all || state === "signed" || state === "key");
    for (const { a, ai, state } of shown) {
      const mine = rows.map((r, i) => [r, i]).filter(([r]) => r.ai === ai);
      if (m.q && !mine.length) continue;
      const kind = a.authenticated == null ? "API key" : "subscription";
      const st = state === "signin" ? `<button class="btn small" data-act="signin" data-v="${esc(a.connect_with)}" data-l="${esc(a.account)}">Sign in</button>`
        : state === "signed" ? `<span class="ok">${icon("check", "s")} Signed in</span>`
        : state === "locked" ? `<span class="mut">${esc(a.unavailable_reason || "Not available")}</span>` : `<span class="mut">Key set</span>`;
      list += `<div class="ghd"><span>${esc(a.account)}${a.provider && a.provider !== a.account ? ` <span class="mut">${esc(a.provider)}</span>` : ""} <span class="mut">${kind}</span></span>${st}</div>` +
        (mine.length ? `<div class="group">${mine.map(([r, i]) => mrow(r, i)).join("")}</div>` : `<p class="cap2" style="margin:0 6px">No models listed yet${state === "signin" ? ": sign in first" : ""}.</p>`);
    }
    if (!shown.length) list = `<p class="cap2" style="padding:12px 0">${m.all ? "The gateway has no accounts yet." : "No account is connected yet."} Sign in to a subscription, or add an API key below.</p>`;
    else if (m.q && !rows.length) list = `<p class="cap2" style="padding:18px 0">No models match. Clear the search to see them all.</p>`;
  }
  const total = (accounts?.list || []).reduce((n, a) => n + (a.models || []).length, 0);
  const effort = main && !m.q
    ? `<div class="ghd">Effort</div><div class="segctl full big">${EFFORTS.map((e) => `<button data-act="effort" data-v="${e}" aria-pressed="${effortNow === e}">${e}</button>`).join("")}</div><p class="cap2">auto lets the model choose; higher thinks longer and costs more.</p>` : "";
  const scope = s ? "" : `<p class="cap2" style="margin-top:10px">No session is open: a choice here is saved for the sessions you start next.</p>`;
  const pinned = !main ? `<p class="cap2" style="margin-top:12px">Every subagent runs on the model ticked below; Main picks the effort for each job.</p>` : "";
  return `<div class="sheet wide" role="dialog" aria-modal="true" aria-labelledby="m-t">
    ${head("m-t", "Models", `Which model answers, and how hard.${scored ? " ★ is intelligence the gateway measured." : ""}`)}
    <div class="sbd">
      <div class="segctl full big" style="margin-top:6px"><button data-act="role" data-v="main" aria-pressed="${main}">Main, answers you</button><button data-act="role" data-v="sub" aria-pressed="${!main}">Subagents, work in parallel</button></div>
      <div style="margin-top:14px"><label class="searchbox">${icon("search")}<input id="msearch" placeholder="Search models" value="${esc(m.q)}" aria-label="Search models"></label></div>
      <div class="filters">
        <div class="segctl"><button data-act="sources" data-v="0" aria-pressed="${!m.all}">Connected</button><button data-act="sources" data-v="1" aria-pressed="${m.all}">All accounts</button></div>
        ${scored ? `<div class="segctl"><button data-act="order" data-v="0" aria-pressed="${!m.byScore}">By account</button><button data-act="order" data-v="1" aria-pressed="${m.byScore}">By intelligence</button></div>` : ""}
      </div>
      ${scope}${effort}${pinned}${list}
      <div class="group" style="margin-top:16px"><button class="row" data-act="sheet" data-v="key"><span class="mut">${icon("plus")}</span>Add an API key<span class="v chev">${icon("right", "s")}</span></button></div>
    </div>
    <div class="sft"><span>${accounts?.list ? `${rows.length} of ${total} models` : ""}</span><span class="grow"></span><span>${app.noticeOn() ? `<b style="color:var(--text)">${esc(app.S.notice)}</b>` : "A choice saves at once"}</span></div>
  </div>`;
}

// -- sign-in ---------------------------------------------------------------

export function signinSheet(app) {
  const si = app.S.signin;
  if (!si) return "";
  const link = si.link, ended = si.state === "ended";
  const host = (() => { try { return new URL(link).host; } catch { return "the sign-in page"; } })();
  return `<div class="sheet narrow" role="dialog" aria-modal="true" aria-labelledby="si-t">
    ${head("si-t", `Sign in to ${si.label}`, "A subscription, signed in through your browser", "Hide")}
    <div class="sbd"><div class="steps">
      <div class="step"><div class="h"><span>01</span>Open the sign-in page in your browser</div>
        ${link ? `<div class="linkbox"><code>${esc(link)}</code><button class="btn small" data-act="copy" data-v="${esc(link)}">Copy link</button><button class="btn small" data-act="signin-ask">Open…</button></div>` : `<p class="cap2">${ended ? "The sign-in did not start." : "Waiting for the sign-in page's address…"}</p>`}
        ${app.S.signinConfirm && link ? `<div class="confirm"><span>Open ${esc(host)} in your browser?</span><span class="grow"></span><button class="btn small primary" data-act="signin-open" data-v="${esc(link)}">Open</button><button class="btn small" data-act="signin-noopen">Not now</button></div>` : ""}
      </div>
      <div class="step"><div class="h"><span>02</span>Sign in there, then paste the address the page sends you to</div>
        <div class="field"><label class="searchbox"><input id="sifield" data-keep="1" placeholder="Paste the address here" aria-label="Address after sign-in" autocomplete="off" spellcheck="false"${ended ? " disabled" : ""}></label><button class="btn primary" data-act="signin-finish"${ended ? " disabled" : ""}>Finish</button></div>
        <p class="cap2" style="margin-top:6px">The address carries a one-time code: it is sent once and the field is emptied.</p>
        ${si.notes.length ? `<div class="err">${esc(si.notes[si.notes.length - 1])}</div>` : ""}
      </div>
    </div>
    <p class="cap2" style="margin-top:12px">The sign-in runs in the engine${si.via ? "'s session" : "'s host"}: Hide leaves it running, and everything else keeps working.</p></div>
    <div class="sft"><button class="plain m" data-act="signin-back">${icon("left", "s")}Models</button><span class="grow"></span>${ended && !si.via ? `<button class="btn" data-act="signin-again">Try again</button>` : `<button class="plain" style="color:var(--fail)" data-act="signin-cancel">Cancel sign-in</button>`}</div>
  </div>`;
}

// -- an API key ------------------------------------------------------------

const PROVIDERS = [["anthropic", "Anthropic"], ["openai", "OpenAI"], ["deepseek", "DeepSeek"], ["openrouter", "OpenRouter"], ["mistral", "Mistral"], ["xai", "xAI"]];

export function keySheet(app) {
  const provider = app.S.keyProvider || "";
  return `<div class="sheet narrow" role="dialog" aria-modal="true" aria-labelledby="k-t">
    ${head("k-t", "Add an API key", "The key goes to the model gateway, which keeps it with your accounts. Sterna writes it nowhere else.", null)}
    <div class="sbd">
      <div class="ghd">Provider</div>
      <div class="filters" style="margin-top:0">${PROVIDERS.map(([id, label]) => `<button class="btn small${provider === id ? " primary" : ""}" data-act="keyprovider" data-v="${id}" aria-pressed="${provider === id}">${label}</button>`).join("")}</div>
      <div class="fld" style="margin-top:12px"><label for="keyprov">Or its name</label><label class="searchbox"><input id="keyprov" data-keep="1" value="${esc(provider)}" placeholder="openai" aria-label="Provider" autocomplete="off" spellcheck="false"></label></div>
      <div class="ghd">Key</div>
      <div class="fld"><label class="searchbox"><input id="keyfield" data-keep="1" type="password" placeholder="Paste the key" aria-label="API key" autocomplete="off" spellcheck="false"></label>
        <div class="hint">It is sent once, on the engine's own connection, and the field is emptied as soon as it goes.</div></div>
    </div>
    <div class="sft"><button class="plain m" data-act="sheet" data-v="models">${icon("left", "s")}Models</button><span class="grow"></span><button class="btn" data-act="close">Cancel</button><button class="btn primary" data-act="savekey"${app.S.keySaving ? " disabled" : ""}>${app.S.keySaving ? "Saving…" : "Save the key"}</button></div>
  </div>`;
}

// -- sandbox ---------------------------------------------------------------

export function levelSheet(app) {
  const s = live(app), f = s?.state.facts || {};
  const now = lower(s ? f.level : app.S.settings?.["sandbox.level"]).replace("full access", "full");
  const memory = s?.state.memory || [], hosts = s?.state.hosts;
  const confirm = app.S.confirmFull;
  const home = app.bridge.home;
  const answered = memory.length
    ? memory.map((m) => `<div class="row"><span>${esc(m.label)}<span class="sub">${m.allowed ? "allowed for this session" : "denied for this session"}</span></span><button class="btn small" style="margin-left:auto" data-act="forget" data-v="${esc(m.id)}">Forget</button></div>`).join("")
    : `<div class="row mut">Nothing answered yet</div>`;
  const enforced = s && (f.sandbox || f.confinement || f.network)
    ? `<div class="ghd">How it is enforced</div><div class="group">
        ${f.sandbox ? `<div class="row">Sandbox<span class="v">${esc(f.sandbox)}</span></div>` : ""}
        ${f.confinement ? `<div class="row">Writes<span class="v wrap">${esc(home ? f.confinement.split(home).join("~") : f.confinement)}</span></div>` : ""}
        ${f.network ? `<div class="row">Network<span class="v">${esc(f.network)}</span></div>` : ""}</div>` : "";
  return `<div class="sheet narrow" role="dialog" aria-modal="true" aria-labelledby="l-t">
    ${head("l-t", "Sandbox", s ? "What Sterna may do, and what you allowed" : "What sessions may do, from the next one you start")}
    <div class="sbd">
      <div class="ghd">Level</div><div class="group">${LEVELS.map(([w, l, sentence]) => `<button class="lvl" data-act="setlevel" data-v="${w}"><span><b class="${w === "full" ? "warn" : ""}">${l}</b><span>${sentence}</span></span><span class="ck">${now === w ? icon("check") : ""}</span></button>`).join("")}</div>
      ${confirm ? `<div class="confirm"><span>Full access runs every call outside the sandbox, and nothing asks. Choose it?</span><span class="grow"></span><button class="btn small primary" data-act="setlevel-full">Choose Full access</button><button class="btn small" data-act="setlevel-cancel">Keep ${esc(levelOf(now)[0])}</button></div>` : ""}
      <p class="cap2">${s ? "A choice saves at once and applies from this session's next request." : "A choice saves at once; a session already running keeps its own level."}</p>
      ${enforced}
      ${s ? `<div class="ghd">Answered this session</div><div class="group">${answered}</div>` : ""}
      ${s && Array.isArray(hosts) ? `<div class="ghd">Allowed hosts</div><div class="group">${hosts.length ? hosts.map((h) => `<div class="row"><span class="mono" style="font-size:13.5px">${esc(h)}</span><button class="btn small" style="margin-left:auto" data-act="unhost" data-v="${esc(h)}">Remove</button></div>`).join("") : `<div class="row mut">No host is let through yet</div>`}</div>` : ""}
    </div>
  </div>`;
}
