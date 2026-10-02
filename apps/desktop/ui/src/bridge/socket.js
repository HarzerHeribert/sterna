// The bridge in a browser: dev/bridge.mjs does over a WebSocket what the
// Tauri shell does in the app, under the same names.

export function socketBridge(url) {
  return new Promise((resolve, reject) => {
    const ws = new WebSocket(url);
    const waiting = new Map();
    const handlers = { line: [], closed: [], quit: [], drop: [] };
    const fire = (name, ...args) => handlers[name].forEach((fn) => fn(...args));
    let n = 0, bridge = null;
    const call = (op, args = {}) => new Promise((ok, fail) => {
      if (ws.readyState !== 1) return fail(new Error("the development bridge is not connected"));
      const id = ++n;
      waiting.set(id, { ok, fail });
      ws.send(JSON.stringify({ n: id, op, ...args }));
    });
    ws.onerror = () => { if (!bridge) reject(new Error(`No development bridge answers at ${url}. Start one with npm run bridge:mock.`)); };
    ws.onclose = () => { for (const w of waiting.values()) w.fail(new Error("the development bridge went away")); waiting.clear(); };
    ws.onmessage = (m) => {
      const msg = JSON.parse(m.data);
      if (msg.re != null) {
        const w = waiting.get(msg.re);
        waiting.delete(msg.re);
        if (msg.err != null) w?.fail(new Error(msg.err)); else w?.ok(msg.ok);
        return;
      }
      if (msg.ev === "engine-line") return fire("line", msg.id, msg.line);
      if (msg.ev === "engine-closed") return fire("closed", msg.id);
      if (msg.ev === "quit-requested") return fire("quit");
      if (msg.ev === "hello") {
        bridge = {
          kind: "socket",
          os: "web",
          home: msg.home,
          drops: false,
          startHost: () => call("host_start"),
          open: (address) => call("conn_open", { address }),
          send: (id, line) => call("conn_send", { id, line }),
          close: (id) => call("conn_close", { id }),
          on: (name, fn) => handlers[name].push(fn),
          chooseFolder: () => call("choose_folder"),
          openPath: (path, root) => call("open_path", { path, root }),
          openUrl: async (url) => { window.open(url, "_blank", "noopener"); },
          notify: async () => {},
          setBadge: async () => {},
          quit: () => call("app_quit"),
          restart: () => call("app_restart"),
          test: (op, args) => call(op, args),
        };
        resolve(bridge);
      }
    };
  });
}
