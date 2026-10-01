// Development only (vite dev with ?dev): shortcuts to the states a mock
// session can be put in, so each can be looked at without playing a whole
// turn. Loaded by main.js behind import.meta.env.DEV; a built app has none.

export function mount(app) {
  const bar = document.createElement("div");
  bar.className = "devbar";
  const button = (label, fn) => { const b = document.createElement("button"); b.className = "btn small"; b.textContent = label; b.onclick = fn; bar.append(b); };
  const name = document.createElement("span");
  name.className = "name";
  name.textContent = "DEV";
  bar.append(name);
  button("Light", () => { app.prefs.appearance = "light"; app.changed(); });
  button("Dark", () => { app.prefs.appearance = "dark"; app.changed(); });
  button("Tally task", () => { const ta = document.getElementById("draft"); ta.value = "Fix the failing test in crates/tally. Keep the change small."; app.send(); });
  button("Overview", () => app.actions.overview());
  button("Quit", () => app.requestQuit());
  document.body.append(bar);
}
