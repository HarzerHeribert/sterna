// Newer releases, as the host's `update` command reports them
// (docs/engine.md): a quiet card at the foot of the sidebar while one is
// out, being moved to or ready to open, and the Updates row in Settings.
import { esc } from "../util.js";
import { icon } from "../icons.js";

export const RELEASES_PAGE = "https://github.com/HarzerHeribert/sterna/releases";

/** A tag as a person reads a version: "v0.1.0-pre.31" is 0.1.0-pre.31. */
export const bare = (tag) => String(tag || "").replace(/^v(?=\d)/, "");

/** What went wrong, in plain words: the engine's own message stays in the title. */
export function plainError(error) {
  const e = String(error || "");
  if (/could not be reached|connect|resolve|dns|network|timed? ?out|unreachable|offline/i.test(e)) return "GitHub could not be reached. Check the connection and try again.";
  const said = e.replace(/https?:\/\/\S+?:\s*/g, "").trim();
  return said ? `The engine said: ${said[0].toUpperCase()}${said.slice(1)}${/[.!?]$/.test(said) ? "" : "."}` : "The engine gave no reason.";
}

/** The version this copy runs: from the check when it said, else from the host's ready line. */
export const installedVersion = (app) => bare(app.S.releases.answer?.installed || app.engine.ready?.version || "");

/** Whether a newer release is worth a word: out, and either looked for by hand or looked for on open with automatic checks on. */
function offered(R) {
  const a = R.answer;
  return !!(a?.updates && a.available && (a.automatic !== false || R.asked));
}

/** The card at the foot of the sidebar; nothing at all while there is nothing new. */
export function releaseCard(app) {
  const R = app.S.releases, latest = bare(R.answer?.latest);
  const card = (title, sub, button, tone = "") => `<div class="relcard${tone}" role="status" aria-label="A newer release"><div class="rx"><b>${title}</b><span>${sub}</span></div>${button}</div>`;
  if (R.phase === "moving") return card(`Updating to Sterna ${esc(latest)}…`, "The download can take a minute. Every session keeps running.", `<button class="btn small" disabled>Updating…</button>`);
  if (R.phase === "moved") return card(`Sterna ${esc(bare(R.moved) || latest)} is installed`, "Restart to open it. Every session keeps running.", `<button class="btn small primary" data-act="rel-restart">Restart</button>`);
  if (R.phase === "unmoved") return card(`Sterna ${esc(latest)} was not installed`, `<span title="${esc(R.error)}">${esc(plainError(R.error))}</span>`, `<button class="btn small" data-act="rel-move">Try again</button>`, " fail");
  if (R.phase === "checked" && offered(R)) return card(`Sterna ${esc(latest)} is available`, `This copy is ${esc(installedVersion(app) || "older")}.`, `<button class="btn small" data-act="rel-move">Update</button>`);
  return "";
}

/** Settings: the version this copy runs, Check now, and what the last check found. */
export function releasesPane(app) {
  const R = app.S.releases, a = R.answer, latest = bare(a?.latest);
  const row = (words, sub, button = "") => `<div class="row relrow"><span class="rx">${words}${sub ? `<span class="sub">${sub}</span>` : ""}</span>${button ? `<span class="v">${button}</span>` : ""}</div>`;
  let found = "";
  if (R.phase === "checking") found = row("Looking for a newer release…", "");
  else if (R.phase === "failed") found = row("The check did not finish", `<span title="${esc(R.error)}">${esc(plainError(R.error))}</span>`, `<button class="btn small" data-act="rel-check">Try again</button>`);
  else if (R.phase === "moving") found = row(`Updating to Sterna ${esc(latest)}…`, "The download can take a minute. Every session keeps running.", `<button class="btn small" disabled>Updating…</button>`);
  else if (R.phase === "moved") found = row(`Sterna ${esc(bare(R.moved) || latest)} is installed`, "Restart to open it. Every session keeps running.", `<button class="btn small primary" data-act="rel-restart">Restart</button>`);
  else if (R.phase === "unmoved") found = row(`Sterna ${esc(latest)} was not installed`, `<span title="${esc(R.error)}">${esc(plainError(R.error))}</span>`, `<button class="btn small" data-act="rel-move">Try again</button>`);
  else if (a && a.updates === false) found = row(esc(a.why || "This copy does not update itself."), "A newer release can be downloaded from the releases page.", `<button class="btn small" data-act="rel-page">${icon("link", "s")}Releases page</button>`);
  else if (a?.updates && a.available) found = row(`Sterna ${esc(latest)} is available`, "It is placed beside this one; Restart opens it.", `<button class="btn small primary" data-act="rel-move">Update</button>`);
  else if (a?.updates) found = row("This is the newest release", "");
  const when = a?.updates && a.automatic === false
    ? "Automatic checks are off (STERNA_DISABLE_AUTOUPDATE is set): Sterna looks for a newer release only when you press Check now."
    : "Sterna looks for a newer release each time it opens, and says so at the foot of the sessions list when there is one.";
  return `<div class="ghd">Updates</div><div class="group">` +
    row("Installed", installedVersion(app) ? `Sterna ${esc(installedVersion(app))}` : "", `<button class="btn small" data-act="rel-check"${R.phase === "checking" || R.phase === "moving" ? " disabled" : ""}>Check now</button>`) +
    found + `</div><p class="cap2">${when}</p>`;
}
