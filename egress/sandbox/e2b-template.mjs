#!/usr/bin/env node
// Builds this directory's Dockerfile as the E2B template the "e2b" sandbox provider starts from (docs/CHAT.md,
// "Sandbox providers"): name `jarvis3-sandbox`, tagged with the commit (`jarvis3-sandbox:<tag>`, what an instance pins
// in sandbox.e2b.template), 1 vCPU / 1024 MB. The start command runs the agent in JARVIS_INIT mode (the server
// configures each sandbox with POST /init); the template is ready when /livez answers. E2B builds remotely: no Docker
// needed here. Run with the E2B SDK installed next to a copy of this file (the chat workflow does this):
//
//   E2B_API_KEY=… node e2b-template.mjs <chat/sandbox dir> <tag> [--name jarvis3-sandbox] [--smoke]
//
// --smoke: then starts one sandbox from the new build with all internet denied, initialises its agent, runs a
// command (python with pandas, as uid 1000) through the agent port and kills it.
import { randomBytes } from "node:crypto";
import { join, resolve } from "node:path";
import { defaultBuildLogger, Template, waitForURL } from "e2b";

const [dir, tag] = process.argv.slice(2);
const i = process.argv.indexOf("--name");
const name = i > 0 ? process.argv[i + 1] : "jarvis3-sandbox";
if (!dir || !tag || !/^[a-z0-9._-]+$/i.test(tag)) {
  console.error("usage: e2b-template.mjs <chat/sandbox dir> <tag> [--name NAME]");
  process.exit(2);
}
if (!process.env.E2B_API_KEY) {
  console.error("E2B_API_KEY is not set");
  process.exit(2);
}
const ctx = resolve(dir);
const template = Template({ fileContextPath: ctx })
  .fromDockerfile(join(ctx, "Dockerfile"))
  .setStartCmd("JARVIS_INIT=1 node /opt/agent/agent.mjs", waitForURL("http://localhost:7070/livez"));
const info = await Template.build(template, `${name}:${tag}`, { cpuCount: 1, memoryMB: 1024, onBuildLogs: defaultBuildLogger() });
console.log(JSON.stringify({ name: info.name, tags: info.tags, templateId: info.templateId, buildId: info.buildId }));

if (process.argv.includes("--smoke")) {
  const api = (process.env.E2B_API_URL || "https://api.e2b.app").replace(/\/$/, "");
  const h = { "x-api-key": process.env.E2B_API_KEY, "content-type": "application/json" };
  const r = await fetch(`${api}/v2/sandboxes`, {
    method: "POST",
    headers: h,
    body: JSON.stringify({ templateID: `${name}:${tag}`, timeout: 300, metadata: { jarvis: "ci-smoke" }, network: { allowPublicTraffic: false, denyOut: ["0.0.0.0/0"] } }),
  });
  if (!r.ok) throw new Error(`create: HTTP ${r.status} ${await r.text()}`);
  const sb = await r.json();
  const t0 = Date.now();
  try {
    const base = `https://7070-${sb.sandboxID}.${sb.domain || "e2b.app"}`;
    const token = randomBytes(24).toString("hex");
    const th = { "e2b-traffic-access-token": sb.trafficAccessToken, "content-type": "application/json" };
    const init = await fetch(`${base}/init`, { method: "POST", headers: th, body: JSON.stringify({ token, env: { SMOKE: "1" } }) });
    if (!init.ok) throw new Error(`init: HTTP ${init.status} ${await init.text()}`);
    const ex = await fetch(`${base}/exec`, {
      method: "POST",
      headers: { ...th, "x-jarvis-exec-token": token },
      body: JSON.stringify({ command: 'python3 -c "import pandas; print(pandas.__version__)" && id -u && echo $SMOKE' }),
    });
    const out = await ex.json();
    console.log(JSON.stringify({ smoke: out, msToExec: Date.now() - t0 }));
    if (out.exit_code !== 0 || !/\n1000\n1\n?$/.test(out.stdout)) throw new Error("smoke: unexpected output");
  } finally {
    await fetch(`${api}/sandboxes/${sb.sandboxID}`, { method: "DELETE", headers: h });
  }
}
