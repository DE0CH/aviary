// The iPhone app on a simulator against the stand-in for pigeon (docs/paseo-ui.md "The iOS app"), the Fold walk's
// (scripts/android-app/emulator-test.mjs) iPhone half: sign in, a long press on a conversation (a plain one and a
// pinned one, which can be dragged) opens its menu, and the Activity panel's header sits where the chat's does, below
// the status bar (Deyao, 2026-11-20: CI walks the Fold and the iPhone).
//
//   node scripts/ios-app/simulator-walk.mjs APP OUT
//
// APP: a simulator build of the app whose server (YYC_SITE) is http://127.0.0.1:8787, where this script starts the
// stand-in (scripts/touch-targets/stand-in.mjs); the simulator shares the Mac's network. OUT gets Maestro's
// screenshots, the UI trees and results.json. Exit 0: every check passed; 1: some failed (a GitHub annotation per
// failure); 2: setup.
import { execFileSync } from "node:child_process";
import fs from "node:fs";
import path from "node:path";
import { startStandIn } from "../touch-targets/stand-in.mjs";

const [app, out] = process.argv.slice(2);
if (!app || !out || !fs.existsSync(app)) {
  console.error("usage: simulator-walk.mjs APP OUT");
  process.exit(2);
}
fs.mkdirSync(out, { recursive: true });
const here = path.dirname(new URL(import.meta.url).pathname);
const flows = path.join(here, "../../ios/maestro");
const PORT = 8787;
// the stand-in's conversations (scripts/touch-targets/content.mjs), as in the Fold walk
const TEXTS = { PLAIN: "新產品命名諗法", PINNED: "員工旅行行程", TASKS: "客戶電郵草稿" };

const run = (cmd, args, opts = {}) =>
  execFileSync(cmd, args, { encoding: "utf8", maxBuffer: 64 * 1024 * 1024, timeout: 900_000, ...opts });
const plist = (key) => run("plutil", ["-extract", key, "raw", "-o", "-", path.join(app, "Info.plist")]).trim();

const results = [];
function check(name, ok, detail = "") {
  results.push({ name, ok: Boolean(ok), detail });
  console.log(`${ok ? "PASS" : "FAIL"} ${name}${detail ? ` (${detail})` : ""}`);
  if (!ok) console.log(`::error title=ios simulator walk::${name}${detail ? `: ${detail}` : ""}`);
  return Boolean(ok);
}

function maestroBin() {
  const home = path.join(process.env.HOME, ".maestro/bin/maestro");
  if (fs.existsSync(home)) return home;
  try {
    return run("which", ["maestro"]).trim();
  } catch {
    run("bash", ["-c", "curl -fsSL https://get.maestro.mobile.dev | bash"]);
    return home;
  }
}

/** The newest iOS runtime and an iPhone 17 (or the newest iPhone), booted with the app installed. */
function bootSimulator() {
  const runtimes = JSON.parse(run("xcrun", ["simctl", "list", "runtimes", "-j"])).runtimes.filter(
    (r) => r.isAvailable && r.platform === "iOS",
  );
  const version = (r) => r.version.split(".").map(Number);
  runtimes.sort((a, b) => {
    const [x, y] = [version(a), version(b)];
    for (let i = 0; i < Math.max(x.length, y.length); i += 1) if ((x[i] ?? 0) !== (y[i] ?? 0)) return (x[i] ?? 0) - (y[i] ?? 0);
    return 0;
  });
  const types = JSON.parse(run("xcrun", ["simctl", "list", "devicetypes", "-j"])).devicetypes
    .map((t) => t.identifier)
    .filter((t) => t.includes("iPhone"));
  const type = types.find((t) => t.endsWith("iPhone-17")) ?? types.at(-1);
  const udid = run("xcrun", ["simctl", "create", "yyc-walk", type, runtimes.at(-1).identifier]).trim();
  console.log(`simulator ${udid} (${type}, ${runtimes.at(-1).identifier})`);
  run("xcrun", ["simctl", "boot", udid]);
  run("xcrun", ["simctl", "bootstatus", udid, "-b"]);
  run("xcrun", ["simctl", "install", udid, app]);
  return udid;
}

/** Runs a flow; false when it failed (its log and screenshots are in OUT). */
function flow(maestro, udid, name, env) {
  const args = ["--device", udid, "test", "--test-output-dir", path.join(out, name)];
  for (const [k, v] of Object.entries(env)) args.push("-e", `${k}=${v}`);
  args.push(path.join(flows, `${name}.yaml`));
  try {
    fs.writeFileSync(path.join(out, `${name}.log`), run(maestro, args, { env: { ...process.env, MAESTRO_DRIVER_STARTUP_TIMEOUT: "600000" } }));
    return true;
  } catch (e) {
    fs.writeFileSync(path.join(out, `${name}.log`), `${e.stdout ?? ""}\n${e.stderr ?? ""}`);
    return false;
  }
}

/** The top edge of the element whose identifier is `id` in Maestro's UI tree, or null. */
function topOf(maestro, udid, id, label) {
  const tree = JSON.parse(run(maestro, ["--device", udid, "hierarchy"]).replace(/^[^{]*/, ""));
  fs.writeFileSync(path.join(out, `tree-${label}.json`), JSON.stringify(tree, null, 1));
  const stack = [tree];
  while (stack.length) {
    const node = stack.pop();
    const attrs = node.attributes ?? {};
    if (Object.values(attrs).includes(id)) {
      const bounds = String(attrs.bounds ?? "").match(/\[(-?\d+),(-?\d+)\]\[(-?\d+),(-?\d+)\]/);
      if (bounds && +bounds[4] > +bounds[2]) return +bounds[2];
    }
    stack.push(...(node.children ?? []));
  }
  return null;
}

async function main() {
  const appId = plist("CFBundleIdentifier");
  const scheme = plist("CFBundleURLTypes.0.CFBundleURLSchemes.0");
  const standIn = await startStandIn({ appDir: path.join(here, "../../packages/app/public"), port: PORT, appScheme: scheme, quiet: true });
  const maestro = maestroBin();
  const udid = bootSimulator();
  try {
    const env = { APP_ID: appId, ...TEXTS };
    const listed = flow(maestro, udid, "walk-list", env);
    check("sign-in, the list, and a long press on a plain and a pinned conversation opening its menu", listed,
      "walk-list.log names the step that failed");
    if (!listed) return;
    const chatTop = topOf(maestro, udid, "workspace-header-title", "conversation");
    const opened = flow(maestro, udid, "walk-activity", env);
    check("the Activity panel opens", opened);
    if (!opened) return;
    const panelTop = topOf(maestro, udid, "activity-title", "activity");
    // both headers are the app's phone header below the status bar: the panel's title where the chat's is
    check("the Activity panel's header is below the status bar, where the chat's is",
      chatTop !== null && panelTop !== null && panelTop >= chatTop - 4,
      `chat title top ${chatTop}, Activity title top ${panelTop}`);
  } finally {
    try {
      run("xcrun", ["simctl", "io", udid, "screenshot", path.join(out, "final.png")]);
    } catch {}
    try {
      run("xcrun", ["simctl", "shutdown", udid]);
      run("xcrun", ["simctl", "delete", udid]);
    } catch {}
    await standIn.close();
  }
}

try {
  await main();
} catch (e) {
  check("the walk ran to its end", false, e instanceof Error ? e.message : String(e));
}
fs.writeFileSync(path.join(out, "results.json"), JSON.stringify(results, null, 2) + "\n");
const failed = results.filter((r) => !r.ok);
console.log(`${results.length - failed.length} of ${results.length} checks passed`);
process.exit(failed.length || results.length === 0 ? 1 : 0);
