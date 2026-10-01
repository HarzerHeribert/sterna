#!/usr/bin/env node
// A stand-in for the `sterna` binary's `host` command, for development
// before the real engine runs: `mock-sterna.mjs host` serves a mock host
// (mock-host.mjs) and prints its ready line; `--background` starts one
// detached unless one answers already, prints its ready line and returns --
// what `sterna host --background` does (docs/engine.md).
//
// Its data folder is never the user's: STERNA_MOCK_DATA, else
// $XDG_DATA_HOME/sterna-mock, else a folder in the system's temp folder.
// STERNA_MOCK_SCENARIO is "empty" (a first run) or "busy"; STERNA_MOCK_PACE
// scales every scripted wait (1 is the default, 0.3 is quick).

import { spawn } from "node:child_process";
import fs from "node:fs";
import net from "node:net";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { startMockHost } from "./mock-host.mjs";

const data = process.env.STERNA_MOCK_DATA
  || (process.env.XDG_DATA_HOME ? path.join(process.env.XDG_DATA_HOME, "sterna-mock") : path.join(os.tmpdir(), `sterna-mock-${os.userInfo().uid}`));
const args = process.argv.slice(2);

if (args[0] !== "host") {
  console.error("mock sterna: only `host` and `host --background` are here");
  process.exit(2);
}

function answers(ready) {
  return new Promise((ok) => {
    const [h, p] = String(ready?.listening || "").split(":");
    if (!p) return ok(false);
    const s = net.connect({ host: h, port: +p }, () => { s.destroy(); ok(true); });
    s.on("error", () => ok(false));
    s.setTimeout(500, () => { s.destroy(); ok(false); });
  });
}

if (args.includes("--background")) {
  let ready = null;
  try { ready = JSON.parse(fs.readFileSync(path.join(data, "host.json"), "utf8")); } catch { /* none yet */ }
  if (ready && (await answers(ready))) {
    console.log(JSON.stringify(ready));
    process.exit(0);
  }
  fs.mkdirSync(path.join(data, "logs"), { recursive: true });
  const log = fs.openSync(path.join(data, "logs", "host.log"), "a");
  const child = spawn(process.execPath, [fileURLToPath(import.meta.url), "host"], { detached: true, stdio: ["ignore", "pipe", log], env: process.env });
  let buf = "";
  child.stdout.setEncoding("utf8");
  child.stdout.on("data", (chunk) => {
    buf += chunk;
    const at = buf.indexOf("\n");
    if (at < 0) return;
    console.log(buf.slice(0, at));
    child.stdout.destroy();
    child.unref();
    process.exit(0);
  });
  child.on("exit", (code) => { console.error(`mock sterna: the host exited (${code}) before it was ready`); process.exit(1); });
} else {
  const { ready, host, close } = await startMockHost({
    scenario: process.env.STERNA_MOCK_SCENARIO || "empty",
    pace: Number(process.env.STERNA_MOCK_PACE || 1),
    data,
  });
  host.onShutdown = () => { close(); process.exit(0); };
  process.on("SIGTERM", () => { close(); process.exit(0); });
  console.log(JSON.stringify(ready));
}
