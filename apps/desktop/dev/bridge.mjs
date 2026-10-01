#!/usr/bin/env node
// The development bridge: what the Tauri shell does for the UI, done by a
// Node process over a WebSocket, so the UI runs in a plain browser (and
// under Playwright). It starts the host, opens loopback TCP connections to
// the host and session ports, writes lines to them and relays every line it
// reads back, under the same names the Tauri shell emits.
//
//   node dev/bridge.mjs --mock [--scenario busy] [--pace 0.4]   a mock host, in this process
//   node dev/bridge.mjs --sterna ../../target/debug/sterna      the real `sterna host --background`
//   … --shutdown-host                                           and end that host when the bridge stops
//
// It prints one ready line, {"bridge":"ws://127.0.0.1:<port>/?secret=…",
// "open":"<the page to open>"}, and serves until it is stopped. It takes a
// WebSocket only from the dev server's page (--origin, by default
// http://127.0.0.1:5199) that names its secret, and connects only to the
// ports the host announced. The folder chooser answers with --folders
// (comma separated, in turn), else a fresh scratch folder.

import { spawn } from "node:child_process";
import fs from "node:fs";
import http from "node:http";
import net from "node:net";
import os from "node:os";
import path from "node:path";
import { randomBytes } from "node:crypto";
import { WebSocketServer } from "ws";
import { startMockHost } from "./mock-host.mjs";

const argv = process.argv.slice(2);
const flag = (name, fallback = null) => { const i = argv.indexOf(`--${name}`); return i >= 0 ? argv[i + 1] : fallback; };
const has = (name) => argv.includes(`--${name}`);
const port = Number(flag("port", process.env.STERNA_BRIDGE_PORT || 5198));
const sterna = flag("sterna", process.env.STERNA_BIN || null);
const mock = has("mock") || !sterna;
const folders = (flag("folders", "") || "").split(",").filter(Boolean);
// Test hooks: addresses whose connections are cut every time they open.
const cutting = new Set();
const origins = (flag("origin", "http://127.0.0.1:5199,http://localhost:5199") || "").split(",").filter(Boolean);
const secret = randomBytes(24).toString("hex");
// The addresses the host has said: its own, and every session it started or located.
const announced = new Set();
function hear(line) {
  if (!line.includes('"listening"')) return;
  try { const v = JSON.parse(line); if (typeof v?.ok?.listening === "string") announced.add(v.ok.listening); } catch { /* not JSON */ }
}
const OS = { darwin: "macos", win32: "windows" }[process.platform] || "linux";

let mockHost = null, realReady = null;
const quits = [];

async function hostStart() {
  if (mock) {
    if (!mockHost) mockHost = await startMockHost({ scenario: flag("scenario", "empty"), pace: Number(flag("pace", "1")), anyFolder: has("any-folder") });
    announced.add(mockHost.ready.listening);
    return mockHost.ready;
  }
  return new Promise((ok, fail) => {
    const node = /\.(mjs|js)$/.test(sterna);
    const child = spawn(node ? process.execPath : sterna, [...(node ? [sterna] : []), "host", "--background"], { stdio: ["ignore", "pipe", "pipe"] });
    let out = "", err = "";
    child.stdout.on("data", (d) => {
      out += d;
      const at = out.indexOf("\n");
      if (at < 0) return;
      try { realReady = JSON.parse(out.slice(0, at)); announced.add(realReady.listening); ok(realReady); } catch { fail(new Error(`the host printed no ready line: ${out.slice(0, 200)}`)); }
    });
    child.stderr.on("data", (d) => { err += d; });
    child.on("exit", (code) => { if (!out.includes("\n")) fail(new Error(`sterna host exited (${code}): ${err.trim().split("\n").slice(-10).join("\n")}`)); });
    child.on("error", (e) => fail(new Error(`could not run ${sterna}: ${e.message}`)));
  });
}

function loopback(address) {
  const at = address.lastIndexOf(":");
  const hostname = address.slice(0, at).replace(/^\[|\]$/g, ""), p = Number(address.slice(at + 1));
  if (!(hostname === "127.0.0.1" || hostname === "::1" || hostname === "localhost") || !p) throw new Error(`${address} is not a loopback address`);
  return { host: hostname, port: p };
}

function scratchFolder() {
  const base = fs.mkdtempSync(path.join(os.tmpdir(), "sterna-dev-"));
  const dir = path.join(base, "ledger-app");
  fs.mkdirSync(dir);
  return fs.realpathSync(dir);
}

const server = http.createServer((_, res) => { res.writeHead(200, { "content-type": "text/plain" }); res.end("Sterna development bridge\n"); });
// Only the dev server's page, and only with the secret this bridge printed.
const wss = new WebSocketServer({
  server,
  verifyClient: ({ origin, req }) => origins.includes(origin) && new URL(req.url, "http://bridge").searchParams.get("secret") === secret,
});
const clients = new Set();

wss.on("connection", (ws) => {
  clients.add(ws);
  const conns = new Map();
  let next = 0;
  const emit = (msg) => { if (ws.readyState === 1) ws.send(JSON.stringify(msg)); };
  emit({ ev: "hello", os: OS, home: os.homedir() });
  const ops = {
    host_start: () => hostStart(),
    conn_open: ({ address }) => new Promise((ok, fail) => {
      const target = loopback(address);
      if (!announced.has(address)) throw new Error(`${address} is not a port the host announced`);
      const socket = net.connect(target);
      const id = ++next;
      let buf = "", open = false;
      socket.setNoDelay(true);
      socket.setEncoding("utf8");
      socket.setTimeout(3000, () => { if (!open) { socket.destroy(); fail(new Error(`no answer at ${address}`)); } });
      socket.on("connect", () => { open = true; socket.setTimeout(0); conns.set(id, socket); socket.address_ = address; ok(id); });
      socket.on("data", (chunk) => {
        buf += chunk;
        let at;
        while ((at = buf.indexOf("\n")) >= 0) {
          hear(buf.slice(0, at));
          emit({ ev: "engine-line", id, line: buf.slice(0, at) });
          // A cut address: its connection goes as soon as the session has said its welcome.
          if (cutting.has(address)) { socket.destroy(); return; }
          buf = buf.slice(at + 1);
        }
      });
      socket.on("error", (e) => { if (!open) fail(new Error(`could not connect to ${address}: ${e.message}`)); });
      socket.on("close", () => { if (conns.delete(id)) emit({ ev: "engine-closed", id }); });
    }),
    conn_send: ({ id, line }) => {
      const socket = conns.get(id);
      if (!socket) throw new Error("no such connection");
      socket.write(line + "\n");
      return null;
    },
    conn_close: ({ id }) => { conns.get(id)?.end(); return null; },
    choose_folder: () => folders.shift() || scratchFolder(),
    open_path: ({ path: p, root }) => {
      // As the app does: a relative path stays inside the session's folder.
      const full = root && !path.isAbsolute(p) ? path.resolve(root, p) : p;
      if (root && !(full === path.resolve(root) || full.startsWith(path.resolve(root) + path.sep))) throw new Error(`${p} is outside the session's folder`);
      console.error(`bridge: open ${full}`);
      return null;
    },
    app_quit: () => { quits.push(Date.now()); return null; },
    // Test hooks: what the window's close button does, and what the host heard.
    test_request_quit: () => { for (const c of clients) c.send(JSON.stringify({ ev: "quit-requested" })); return null; },
    test_host_quits: () => (mockHost ? mockHost.host.quits : []),
    // A dropped connection, once; or every connection to `address` cut as it opens, until `off`.
    test_drop: ({ address }) => { let n = 0; for (const s of conns.values()) if (s.address_ === address) { s.destroy(); n++; } return n; },
    test_cut: ({ address, off }) => { if (off) cutting.delete(address); else { cutting.add(address); for (const s of conns.values()) if (s.address_ === address) s.destroy(); } return null; },
    test_app_quits: () => quits.length,
  };
  ws.on("message", async (data) => {
    let msg;
    try { msg = JSON.parse(String(data)); } catch { return; }
    try {
      const op = ops[msg.op];
      if (!op) throw new Error(`no operation ${msg.op}`);
      emit({ re: msg.n, ok: (await op(msg)) ?? null });
    } catch (e) {
      emit({ re: msg.n, err: String(e?.message || e) });
    }
  });
  ws.on("close", () => { clients.delete(ws); for (const s of conns.values()) s.destroy(); });
});

server.listen(port, "127.0.0.1", () => {
  const url = `ws://127.0.0.1:${server.address().port}/?secret=${secret}`;
  console.log(JSON.stringify({ bridge: url, open: `${origins[0]}/?bridge=${encodeURIComponent(url)}` }));
});
/** Asks a real host this bridge started to end, with its own `shutdown`. */
function shutdownReal() {
  if (!realReady || !has("shutdown-host")) return Promise.resolve();
  return new Promise((done) => {
    const { host, port: p } = loopback(realReady.listening);
    const socket = net.connect({ host, port: p }, () => {
      socket.write(JSON.stringify({ hello: { token: realReady.token, protocol: 1, client: "desktop" } }) + "\n");
      socket.write(JSON.stringify({ do: "shutdown" }) + "\n");
    });
    socket.on("data", () => {});
    socket.on("close", done);
    socket.on("error", done);
    setTimeout(() => { socket.destroy(); done(); }, 3000);
  });
}
const stop = async () => { mockHost?.close(); await shutdownReal(); server.close(); process.exit(0); };
process.on("SIGTERM", stop);
process.on("SIGINT", stop);
