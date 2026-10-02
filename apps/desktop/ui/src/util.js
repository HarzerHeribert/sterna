// Small words and numbers the views share.

export const $ = (s, root = document) => root.querySelector(s);
export const esc = (s) => String(s ?? "").replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;").replace(/"/g, "&quot;");
export const plural = (n, one, many) => `${n} ${n === 1 ? one : many}`;
export const pad3 = (n) => String(n).padStart(3, "0");
export const secs = (ms) => (ms < 10000 ? (ms / 1000).toFixed(1) + " s" : ms < 60000 ? Math.floor(ms / 1000) + " s" : clock(ms));
export const clock = (ms) => { const t = Math.max(0, Math.floor(ms / 1000)); return Math.floor(t / 60) + ":" + String(t % 60).padStart(2, "0"); };
/** As the resume picker says it. */
export const ago = (s) => s <= 90 ? "just now" : s < 3600 ? `${Math.floor(s / 60)} min ago` : s < 86400 ? `${Math.floor(s / 3600)} hr ago` : `${plural(Math.floor(s / 86400), "day", "days")} ago`;
export const agoMs = (unixMs) => ago(Math.max(0, (Date.now() - unixMs) / 1000));
export const kilo = (n) => (n >= 1e6 ? (n / 1e6).toFixed(1) + "M" : n >= 1000 ? (n / 1000).toFixed(1) + "k" : String(n));
export const timeOf = (unixMs) => new Date(unixMs).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" });

/** A folder's name: the last part of its path. */
export const baseName = (root) => String(root || "").replace(/[\\/]+$/, "").split(/[\\/]/).pop() || root;
/** A path as a person reads it: the home folder as ~. */
export function tilde(root, home) {
  if (home && root && (root === home || root.startsWith(home + "/") || root.startsWith(home + "\\"))) return "~" + root.slice(home.length);
  return root;
}

/** The folder a path sits in, as written: "~/code" for "~/code/quill"; "" for a root of its own. */
export function parentPath(path) {
  const p = String(path || "").replace(/[\\/]+$/, "");
  const at = Math.max(p.lastIndexOf("/"), p.lastIndexOf("\\"));
  if (at < 0) return "";
  return at === 0 ? p[0] : p.slice(0, at);
}

/**
 * A path that has to fit, cut at its start a whole folder at a time:
 * "…/work/clients" rather than "~/wo…". `fits` says whether a text fits;
 * when not even the last folder does, nothing is shown.
 */
export function clipStart(path, fits) {
  const p = String(path || "");
  if (!p || fits(p)) return p;
  const sep = p.includes("/") ? "/" : "\\";
  const parts = p.split(sep);
  for (let i = 1; i < parts.length; i++) {
    const rest = parts.slice(i).join(sep);
    if (!rest) continue;
    const cut = "…" + sep + rest;
    if (fits(cut)) return cut;
  }
  return "";
}

/** Does this text look like a relative path to a file, the way answers name them? */
const PATH = /(?<![\w/.~-])((?:[\w.-]+\/)+[\w.-]+\.[A-Za-z0-9]{1,8})(?![\w/])/g;

/** Inline words: `code`, **bold**, and paths a click opens. */
function inline(text) {
  const parts = String(text).split(/(`[^`]+`)/g);
  return parts.map((part) => {
    if (part.startsWith("`") && part.endsWith("`") && part.length > 1) return `<code>${esc(part.slice(1, -1))}</code>`;
    return esc(part)
      .replace(/\*\*([^*]+)\*\*/g, "<b>$1</b>")
      .replace(PATH, (m) => `<a class="path" data-act="path" data-v="${m}">${m}</a>`);
  }).join("");
}

/** A model's prose, as paragraphs, lists and code blocks. */
export function prose(text) {
  const out = [];
  const lines = String(text ?? "").replace(/\r\n/g, "\n").split("\n");
  let para = [], list = null, fence = null;
  const flush = () => {
    if (para.length) out.push(`<p>${inline(para.join(" "))}</p>`);
    para = [];
    if (list) out.push(`<${list.tag}>${list.items.map((i) => `<li>${inline(i)}</li>`).join("")}</${list.tag}>`);
    list = null;
  };
  for (const line of lines) {
    if (fence) {
      if (/^\s*```/.test(line)) { out.push(`<pre class="code">${esc(fence.join("\n"))}</pre>`); fence = null; } else fence.push(line);
      continue;
    }
    if (/^\s*```/.test(line)) { flush(); fence = []; continue; }
    const bullet = line.match(/^\s*[-*•]\s+(.*)$/), num = line.match(/^\s*\d+[.)]\s+(.*)$/);
    if (bullet || num) {
      const tag = bullet ? "ul" : "ol";
      if (para.length || (list && list.tag !== tag)) flush();
      list = list || { tag, items: [] };
      list.items.push((bullet || num)[1]);
      continue;
    }
    if (!line.trim()) { flush(); continue; }
    if (list) flush();
    para.push(line.trim());
  }
  if (fence) out.push(`<pre class="code">${esc(fence.join("\n"))}</pre>`);
  flush();
  return out.join("");
}

/** The first paragraph of an answer, and the rest. */
export function splitAnswer(text) {
  const t = String(text ?? "").trim();
  const at = t.search(/\n\s*\n/);
  return at < 0 ? [t, ""] : [t.slice(0, at).trim(), t.slice(at).trim()];
}
export const inlineHTML = inline;
