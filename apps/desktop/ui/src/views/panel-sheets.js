// What a session builds for a person: a panel (a sheet of rows, each of a
// kind and with a typed action, as the terminal draws it), a form, and the
// instruments -- every notice, and the requests and tokens.
import { esc, spaced } from "../util.js";
import { icon } from "../icons.js";
import { head } from "./choice-sheets.js";

const sw = (on) => `<span class="switch${on ? " on" : ""}" aria-hidden="true"></span>`;
const lower = (v) => String(v ?? "").toLowerCase();

/** A row's kind and what it carries: `"Info"`, `{"Choice":{"current":true}}`, `{"Toggle":true}`… */
export function kindOf(row) {
  const k = row?.kind;
  if (typeof k === "string") return { kind: lower(k), data: null };
  const [name, data] = Object.entries(k || {})[0] || ["info", null];
  return { kind: lower(name), data };
}

/** An action as the session spelled it: `"Models"`, `{"Command":"/effort low"}`, `{"Tab":[3,"Diff"]}`. */
export function actionOf(action) {
  if (!action) return null;
  if (typeof action === "string") return { type: action, value: null };
  const [type, value] = Object.entries(action)[0] || [];
  return type ? { type, value } : null;
}

/** The actions a client can take over the seam; any other stays the terminal's. */
const DOABLE = new Set(["Command", "Level", "ConfirmLevel", "Choose", "Path", "Insert", "Draft", "Cell", "Tab", "Models", "Settings", "SettingsAt", "Sandbox", "Hosts", "Help", "Activity", "Telemetry", "Close", "Resume", "RemoveHost", "Latest"]);
export const doable = (action) => { const a = actionOf(action); return !!a && DOABLE.has(a.type); };

/**
 * A panel's rows as the window lays them out. The terminal wraps its text
 * rows to its width, so a run of them is one paragraph here, a blank row
 * ends it; every other row is what its kind makes it.
 */
export function panelBlocks(rows) {
  const blocks = [];
  let para = null;
  for (const [i, row] of (rows || []).entries()) {
    const { kind } = kindOf(row), text = String(row.text ?? "");
    if (kind === "info" && !row.value && !doable(row.action)) {
      if (!text.trim()) { para = null; continue; }
      if (para && !/^\s{2,}/.test(text) && /[.:;!?]$/.test(para.text) && /^[A-Z]/.test(text.trim())) para = null;
      if (para) para.text += " " + text.trim();
      else { para = { kind: "para", text: text.trim() }; blocks.push(para); }
      continue;
    }
    para = null;
    blocks.push({ kind: kind === "heading" ? "heading" : "row", row, index: i });
  }
  return blocks;
}

function rowHTML(app, row, index) {
  const { kind, data } = kindOf(row);
  const can = doable(row.action);
  const value = row.value ? `<span class="v">${esc(spaced(row.value))}</span>` : "";
  const act = `data-act="panelrow" data-r="${index}"`;
  switch (kind) {
    case "choice": {
      const current = !!data?.current;
      return `<button class="row" ${can ? act : 'data-act="noop" aria-disabled="true"'} aria-pressed="${current}"><span>${esc(spaced(row.text))}</span>${value}<span class="v" style="color:var(--accent)">${current ? icon("check") : ""}</span></button>`;
    }
    case "toggle": {
      const on = data === true;
      return `<button class="row" ${can ? act : 'data-act="noop" aria-disabled="true"'} role="switch" aria-checked="${on}"><span>${esc(spaced(row.text))}</span>${sw(on)}</button>`;
    }
    case "value": {
      const values = Array.isArray(data?.values) ? data.values : [];
      return `<div class="row" style="flex-wrap:wrap"><span>${esc(spaced(row.text))}</span><div class="segctl" style="margin-left:auto">${values.map(([label, action], vi) =>
        `<button data-act="${doable(action) ? "panelvalue" : "noop"}" data-r="${index}" data-vi="${vi}" aria-pressed="${data?.current === vi}">${esc(label)}</button>`).join("")}</div></div>`;
    }
    case "danger": {
      const asking = app.S.panelConfirm === index;
      if (!can) return `<div class="row"><span class="fail">${esc(spaced(row.text))}</span>${value}</div>`;
      return asking
        ? `<div class="confirm" style="margin:6px 8px"><span>${esc(spaced(row.text))}: this cannot be taken back.</span><span class="grow"></span><button class="btn small" data-act="panelrow" data-r="${index}" data-sure="1" style="color:var(--fail)">${esc(spaced(row.text))}</button><button class="btn small" data-act="panelconfirm" data-r="-1">Cancel</button></div>`
        : `<button class="row" data-act="panelconfirm" data-r="${index}"><span class="fail">${esc(spaced(row.text))}</span>${value}<span class="v chev">${icon("right", "s")}</span></button>`;
    }
    case "field": {
      const shown = data?.text ?? data?.value ?? "";
      return `<div class="row"><span>${esc(spaced(row.text))}</span><span class="v mono" style="font-size:13px">${esc(shown)}</span></div>`;
    }
    case "open": case "run":
      if (can) return `<button class="row" ${act}><span>${esc(spaced(row.text))}</span>${value || `<span class="v chev">${icon("right", "s")}</span>`}</button>`;
      return `<div class="row mut"><span>${esc(spaced(row.text))}</span>${value}</div>`;
    default:
      if (can) return `<button class="row" ${act}><span class="pre-note">${esc(spaced(row.text))}</span>${value || `<span class="v chev">${icon("right", "s")}</span>`}</button>`;
      return `<div class="row"><span class="pre-note">${esc(spaced(row.text))}</span>${value}</div>`;
  }
}

export function panelSheet(app) {
  const panel = app.S.panel?.panel;
  if (!panel) return "";
  let h = "", group = [];
  const flush = () => { if (group.length) h += `<div class="group">${group.join("")}</div>`; group = []; };
  for (const b of panelBlocks(panel.rows)) {
    if (b.kind === "para") { flush(); h += `<p class="cap2" style="font-size:13.5px;color:var(--text);margin:10px 6px">${esc(spaced(b.text))}</p>`; continue; }
    if (b.kind === "heading") { flush(); h += `<div class="ghd">${esc(spaced(b.row.text))}</div>`; continue; }
    group.push(rowHTML(app, b.row, b.index));
  }
  flush();
  return `<div class="sheet narrow" role="dialog" aria-modal="true" aria-labelledby="p-t">
    ${head("p-t", panel.title || "Sterna", "")}
    <div class="sbd">${h}</div>
  </div>`;
}

// -- a form ----------------------------------------------------------------

export const fieldKind = (field) => {
  const k = field.kind;
  if (typeof k === "string") return { kind: lower(k), words: [] };
  const [name, words] = Object.entries(k || {})[0] || ["text", []];
  return { kind: lower(name), words: Array.isArray(words) ? words : [] };
};

export function formSheet(app) {
  const at = app.form;
  const form = at?.prompt?.form;
  if (!form) return "";
  const values = at.values;
  const fields = (form.fields || []).map((field, i) => {
    const { kind, words } = fieldKind(field);
    // A secret's value is never written into the page: the field keeps it, and only the answer carries it.
    const input = kind === "choice"
      ? `<div class="segctl full big">${words.map((w, wi) => `<button data-act="formchoice" data-i="${i}" data-v="${wi}" aria-pressed="${values[i] === words[wi]}">${esc(w)}</button>`).join("")}</div>`
      : kind === "secret"
        ? `<label class="searchbox"><input id="form-${i}" data-form="${i}" data-keep="1" data-secret="1" type="password" aria-label="${esc(field.label)}" autocomplete="off" spellcheck="false"></label>`
        : `<label class="searchbox"><input id="form-${i}" data-form="${i}" type="text" value="${esc(values[i] ?? "")}" aria-label="${esc(field.label)}" autocomplete="off" spellcheck="false"></label>`;
    const err = form.error && form.error[0] === i ? `<div class="err">${esc(form.error[1])}</div>` : "";
    return `<div class="fld"><label for="form-${i}">${esc(field.label)}${field.optional ? ` <span class="mut" style="font-weight:400">(optional)</span>` : ""}</label>${input}${field.hint ? `<div class="hint">${esc(field.hint)}</div>` : ""}${err}</div>`;
  }).join("");
  const step = Array.isArray(form.step) ? `Step ${form.step[0]} of ${form.step[1]}` : "";
  return `<div class="sheet narrow" role="dialog" aria-modal="true" aria-labelledby="f-t">
    ${head("f-t", form.title || "Sterna asks", esc([step && step + ".", form.intro].filter(Boolean).join(" ")), null)}
    <div class="sbd">${form.warning ? `<div class="warnbox">${esc(form.warning)}</div>` : ""}<div class="fields">${fields}</div>${form.help ? `<p class="cap2" style="margin-top:12px">${esc(form.help)}</p>` : ""}</div>
    <div class="sft"><span class="grow"></span><button class="btn" data-act="formcancel">Put it away</button><button class="btn primary" data-act="formsubmit">${esc(form.submit ? form.submit[0].toUpperCase() + form.submit.slice(1) : "Save")}</button></div>
  </div>`;
}

// -- instruments -----------------------------------------------------------

export function activitySheet(app) {
  const notes = [...(app.cur()?.state.notes || [])].reverse();
  return `<div class="sheet narrow" role="dialog" aria-modal="true" aria-labelledby="a-t">
    ${head("a-t", "Activity", "Every notice this session gave, newest first")}
    <div class="sbd"><div class="group">${notes.length ? notes.map((n) => `<div class="row"><span class="pre-note">${esc(spaced(n))}</span></div>`).join("") : `<div class="row mut">No notices yet</div>`}</div></div>
  </div>`;
}

export function telemetrySheet(app) {
  const s = app.cur(), u = s?.state.usage || {}, total = app.S.usage?.total;
  const row = (l, v) => `<div class="row">${l}<span class="v">${v}</span></div>`;
  return `<div class="sheet narrow" role="dialog" aria-modal="true" aria-labelledby="te-t">
    ${head("te-t", "Telemetry", "Requests and tokens, as the providers counted them")}
    <div class="sbd">${s ? `<div class="ghd">This session</div><div class="group">${row("Requests", u.requests ?? 0)}${row("Input tokens", (u.input_tokens ?? 0).toLocaleString())}${row("Output tokens", (u.output_tokens ?? 0).toLocaleString())}${u.reasoned_tokens ? row("Reasoned tokens", u.reasoned_tokens.toLocaleString()) : ""}${s.state.served?.model ? row("Served by", esc(s.state.served.provider ? `${s.state.served.model} by ${s.state.served.provider}` : s.state.served.model)) : ""}</div>` : ""}
      ${total ? `<div class="ghd">Every session the app's host started</div><div class="group">${row("Requests", total.requests ?? 0)}${row("Input tokens", (total.input_tokens ?? 0).toLocaleString())}${row("Output tokens", (total.output_tokens ?? 0).toLocaleString())}</div>` : ""}</div>
  </div>`;
}
