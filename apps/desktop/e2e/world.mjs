// A world of its own for checks against the real engine, the way the
// engine's own checks run it (crates/sterna/tests/support/engine.rs,
// tests/engine_settings.rs): a scratch data folder and settings, a scripted
// model endpoint (dev/provider.mjs), a fake gateway that records what it is
// handed, no credential -- and `sterna host` itself, from STERNA_E2E_BIN
// (else target/debug/sterna), behind the development bridge.
import { test as base, expect } from "@playwright/test";
import fs from "node:fs";
import net from "node:net";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { spawn } from "node:child_process";
import { startBridge, newSession, say } from "./fixtures.mjs";
import { startProvider } from "../dev/provider.mjs";

export const STERNA = process.env.STERNA_E2E_BIN || fileURLToPath(new URL("../../../target/debug/sterna", import.meta.url));
/** Whether that binary has the host and its setup commands. */
export const engineReady = () => { try { const bin = fs.readFileSync(STERNA); return bin.includes("sterna host: unknown option") && bin.includes("set_preferences"); } catch { return false; } };

/** A gateway that lists two accounts, keeps a key it is handed on stdin, and runs a sign-in that takes a pasted address. */
function fakeGateway(dir) {
  const account = (name, provider, models, authenticated) => ({ account: name, provider, models, scope: "account-declared", selectable: true, unavailable_reason: null, authenticated, connect_with: provider, pooled: true });
  const listed = (signedIn) => JSON.stringify({ version: 1, accounts: [account("chatgpt-pro", "openai", ["fixture-model", "fixture-two"], true), account("claude-max", "anthropic", ["claude-opus-5-5"], signedIn)] });
  const script = `#!/bin/sh
echo "$@" >> '${dir}/gateway-argv.txt'
case "$1 $2" in
  "credentials set")
    cat > "${dir}/gateway-stdin-$$.txt"
    printf '%s\\n' '{"provider":"'"$3"'","variable":"OPENAI_API_KEY","stored_in":"credentials.toml","model_lists":[]}'
    ;;
  "subscriptions connect")
    printf '%s\\n' '{"state":"opened","authorize_url":"https://example.invalid/sign-in"}'
    read -r pasted
    printf '%s' "$pasted" > '${dir}/gateway-pasted.txt'
    touch '${dir}/gateway-connected'
    printf '%s\\n' '{"state":"connected","account":"me@example.com"}'
    ;;
  *)
    if [ -f '${dir}/gateway-connected' ]; then
      printf '%s\\n' '${listed(true)}'
    else
      printf '%s\\n' '${listed(false)}'
    fi
    ;;
esac
exit 0
`;
  const file = path.join(dir, "no-gateway");
  fs.writeFileSync(file, script, { mode: 0o755 });
  return file;
}

/** One line to the host and its one answer, on a connection of the check's own. */
export function hostAsk(ready, command) {
  return new Promise((ok, fail) => {
    const [h, p] = ready.listening.split(":");
    const socket = net.connect({ host: h, port: +p });
    let buf = "", said = 0;
    socket.setEncoding("utf8");
    socket.on("connect", () => {
      socket.write(JSON.stringify({ hello: { token: ready.token, protocol: 1, client: "check" } }) + "\n");
      socket.write(JSON.stringify(command) + "\n");
    });
    socket.on("data", (d) => {
      buf += d;
      let at;
      while ((at = buf.indexOf("\n")) >= 0) {
        const line = JSON.parse(buf.slice(0, at));
        buf = buf.slice(at + 1);
        if (said++ === 0) continue;
        socket.destroy();
        return "error" in line ? fail(new Error(line.error)) : ok(line.ok);
      }
    });
    socket.on("error", fail);
    setTimeout(() => { socket.destroy(); fail(new Error(`no answer to ${JSON.stringify(command)}`)); }, 60000);
  });
}

export const test = base.extend({
  level: ["ask", { option: true }],
  world: async ({ level }, use) => {
    const root = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), "sterna-desktop-e2e-")));
    const folder = (name) => {
      const dir = path.join(root, name);
      fs.mkdirSync(path.join(dir, ".sterna"), { recursive: true });
      // Questions on, answered by a person (as crates/sterna/tests/engine_prompts.rs has them).
      fs.writeFileSync(path.join(dir, ".sterna/config.toml"), '[model]\nparent = "fixture-model"\n\n[ask]\nenabled = true\njev = "off"\n');
      return dir;
    };
    const projects = ["harbor", "alpha", "beta", "gamma"].map(folder);
    // `sandbox.level` is global only.
    fs.mkdirSync(path.join(root, "global-config/sterna"), { recursive: true });
    fs.writeFileSync(path.join(root, "global-config/sterna/config.toml"), `[sandbox]\nlevel = "${level}"\n`);
    const provider = await startProvider({ pace: 0.8 });
    const gateway = fakeGateway(root);
    const saved = { ...process.env };
    Object.assign(process.env, {
      XDG_DATA_HOME: path.join(root, "data"), XDG_CONFIG_HOME: path.join(root, "global-config"),
      ANTHROPIC_BASE_URL: provider.url, INFERENCE_GATEWAY_BIN: gateway,
      INFERENCE_GATEWAY_CONFIG: path.join(root, "gateway-config", "gateway.toml"), INFERENCE_GATEWAY_DATA_DIR: path.join(root, "gateway-data"),
    });
    for (const k of ["ANTHROPIC_API_KEY", "ANTHROPIC_AUTH_TOKEN", "OPENAI_API_KEY", "COLORTERM", "STERNA_HOME"]) delete process.env[k];
    // The folder chooser gives harbor, then alpha, then scratch folders.
    const bridge = await startBridge(["--sterna", STERNA, "--shutdown-host", "--folders", `${projects[0]},${projects[1]}`]);
    const env = { ...process.env };
    process.env = saved;
    // The host the window will find: started here when a check speaks to it first, as `sterna host --background` does.
    const hostFile = path.join(root, "data", "sterna", "host.json");
    const ready = () => new Promise((ok, fail) => {
      if (fs.existsSync(hostFile)) return ok(JSON.parse(fs.readFileSync(hostFile, "utf8")));
      const child = spawn(STERNA, ["host", "--background"], { env, stdio: ["ignore", "pipe", "pipe"] });
      let out = "", err = "";
      child.stdout.on("data", (d) => { out += d; });
      child.stderr.on("data", (d) => { err += d; });
      child.on("exit", () => { try { ok(JSON.parse(out.trim().split("\n")[0])); } catch { fail(new Error(`sterna host --background: ${err || out}`)); } });
    });
    await use({
      root, env, provider, bridge, harbor: projects[0], projects,
      host: async (command) => hostAsk(await ready(), command),
      read: (name) => { try { return fs.readFileSync(path.join(root, name), "utf8"); } catch { return null; } },
      handed: () => fs.readdirSync(root).filter((n) => n.startsWith("gateway-stdin-")).map((n) => fs.readFileSync(path.join(root, n), "utf8")).join(""),
    });
    // The bridge this check started, by its own handle; it ends the host it started.
    bridge.child.kill("SIGTERM");
    await new Promise((ok) => bridge.child.on("exit", ok));
    provider.server.close();
    if (process.env.STERNA_E2E_KEEP) console.log(`kept ${root}`); else fs.rmSync(root, { recursive: true, force: true });
  },
  app: async ({ page, world }, use) => {
    await page.goto(`/?bridge=${encodeURIComponent(world.bridge.url)}`);
    await use(page);
  },
});

test.beforeEach(() => {
  test.fixme(!engineReady(), `the real engine's checks need a sterna with \`sterna host\` and its setup commands at ${STERNA}`);
});

/** Waits until the host's list has a session whose title starts with `title`, and returns it. */
export async function listed(world, title, wanted = () => true) {
  const deadline = Date.now() + 60000;
  for (;;) {
    const list = await world.host({ do: "list" });
    const found = list.folders.flatMap((f) => f.sessions.map((s) => ({ ...s, root: f.root }))).find((s) => s.title.startsWith(title));
    if (found && wanted(found)) return found;
    if (Date.now() > deadline) throw new Error(`no session "${title}" as wanted in ${JSON.stringify(list)}`);
    await new Promise((ok) => setTimeout(ok, 200));
  }
}

export { expect, newSession, say };
