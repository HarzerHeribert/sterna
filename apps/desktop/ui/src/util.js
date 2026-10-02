// Small words and numbers the views share.

export const $ = (s, root = document) => root.querySelector(s);
/** The engine's own words, which join their parts with the terminal's middle dot: here a dash joins them. */
export const spaced = (s) => String(s ?? "").replace(/ · /g, " — ");
export const esc = (s) => String(s ?? "").replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;").replace(/"/g, "&quot;");
export const plural = (n, one, many) => `${n} ${n === 1 ? one : many}`;
export const secs = (ms) => (ms < 10000 ? (ms / 1000).toFixed(1) + " s" : ms < 60000 ? Math.floor(ms / 1000) + " s" : clock(ms));
export const clock = (ms) => { const t = Math.max(0, Math.floor(ms / 1000)); return Math.floor(t / 60) + ":" + String(t % 60).padStart(2, "0"); };
/** As the resume picker says it. */
export const ago = (s) => s <= 90 ? "just now" : s < 3600 ? `${Math.floor(s / 60)} min ago` : s < 86400 ? `${Math.floor(s / 3600)} hr ago` : `${plural(Math.floor(s / 86400), "day", "days")} ago`;
export const agoMs = (unixMs) => ago(Math.max(0, (Date.now() - unixMs) / 1000));
export const kilo = (n) => (n >= 1e6 ? (n / 1e6).toFixed(1) + "M" : n >= 1000 ? (n / 1000).toFixed(1) + "k" : String(n));
export const timeOf = (unixMs) => new Date(unixMs).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" });

/**
 * Brings `host`'s content to `html` in place: a node still there is kept,
 * with what it holds -- hover, focus, its scroll, a running transition, an
 * entrance already played -- and only what changed is written.
 */
export function morph(host, html) {
  const next = document.createElement("template");
  next.innerHTML = html;
  patchChildren(host, next.content);
}

function patchChildren(from, to) {
  const was = [...from.childNodes], now = [...to.childNodes];
  now.forEach((node, i) => {
    const old = was[i];
    if (!old) from.appendChild(node);
    else if (old.nodeType !== node.nodeType || old.nodeName !== node.nodeName) from.replaceChild(node, old);
    else if (node.nodeType === Node.ELEMENT_NODE) { patchAttributes(old, node); patchChildren(old, node); }
    else if (old.nodeValue !== node.nodeValue) old.nodeValue = node.nodeValue;
  });
  for (const old of was.slice(now.length)) old.remove();
}

function patchAttributes(old, node) {
  for (const { name } of [...old.attributes]) if (!node.hasAttribute(name)) old.removeAttribute(name);
  for (const { name, value } of [...node.attributes]) {
    if (old.getAttribute(name) === value) continue;
    old.setAttribute(name, value);
    // A field the person has typed in no longer follows its attribute: a
    // value the state changed is put in the field itself.
    if (name === "value" && "value" in old) old.value = value;
  }
}

/** A folder's name: the last part of its path. */
export const baseName = (root) => String(root || "").replace(/[\\/]+$/, "").split(/[\\/]/).pop() || root;
/** A path as a person reads it: the home folder as ~. */
export function tilde(root, home) {
  if (home && root && (root === home || root.startsWith(home + "/") || root.startsWith(home + "\\"))) return "~" + root.slice(home.length);
  return root;
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
