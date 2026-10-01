// How the window looks and what it shows. The terminal and the app share
// the theme, the motion and how a cell being written is shown: those are
// the person's settings (`ui.theme`, `ui.motion`, `ui.stream`), saved with
// the host's `set_setting`. Everything else here is the app's own, kept
// whole by the host's `set_preferences` (docs/engine.md). A choice saves at
// once.

export const DEFAULTS = {
  theme: "amazon", appearance: "system", motion: "full", stream: "code",
  sessions: true, card: true,
  cardBird: true, cardSession: true, cardSoFar: true, cardGuard: true, cardContext: true, cardTools: false,
  summary: true, handles: false, notifyAsk: true, notifyDone: true, dockBadge: true,
};

/** The app's keys that are the person's shared settings, by their dotted key. */
export const SHARED = { theme: "ui.theme", motion: "ui.motion", stream: "ui.stream" };

/** The preferences before the host has answered: the defaults, panels by the window's width. */
export function startingPrefs() {
  return { ...DEFAULTS, sessions: innerWidth > 860, card: innerWidth > 1120 };
}

/** The preferences from the host's `preferences` and `settings` answers. */
export function fromHost(saved, values) {
  const prefs = startingPrefs();
  for (const [k, v] of Object.entries(saved || {})) if (k in DEFAULTS && !(k in SHARED) && typeof v === typeof DEFAULTS[k]) prefs[k] = v;
  for (const [k, key] of Object.entries(SHARED)) if (typeof values?.[key] === "string" && values[key]) prefs[k] = values[key];
  return prefs;
}

/** The app's own part, as `set_preferences` keeps it. */
export function ownPart(prefs) {
  return Object.fromEntries(Object.keys(DEFAULTS).filter((k) => !(k in SHARED)).map((k) => [k, prefs[k]]));
}
