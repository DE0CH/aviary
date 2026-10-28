#!/usr/bin/env node
// The first process of a sandbox on our own hosts (provider "vm", docs/SANDBOX-VM.md "Docker in sandboxes",
// "Installing packages"). It runs as root INSIDE gVisor (its capabilities are the sandbox's own; the gVisor sentry has
// none on the host, and the sandbox's uids are the sentry's, not the host's):
//
// - the agent (agent.mjs) is started through setpriv as uid/gid 1000 with no capabilities (effective, permitted,
//   inheritable and ambient sets empty). The bounding set is kept and no_new_privs is NOT set, so a command can become
//   root inside the sandbox through sudo (passwordless for uid 1000; runsc --allow-suid) to install system packages;
//   that root is the same one dockerd already has. What isolates a sandbox is gVisor, the netns with the host's nft
//   lock, the cgroup and the absence of host mounts and secrets, not the uid inside;
// - it writes /etc/hosts (localhost and the hostname) and apt's proxy setting (/etc/apt/apt.conf.d/80sandbox-proxy)
//   from the sandbox's own proxy variables, so apt reaches the Debian mirrors through egress even from a root shell
//   that dropped the environment;
// - it listens on /run/sbx-docker/start.sock (HTTP over a unix socket). The `docker` wrapper (/usr/local/bin/docker)
//   asks it to start dockerd on the first docker command; dockerd's socket belongs to group 1000 (as on any host where
//   the user is in the `docker` group). Sandboxes that never use Docker pay nothing for it.
//
// The kubernetes and e2b providers keep starting agent.mjs directly as uid 1000 (no Docker or sudo there).
import { spawn } from "node:child_process";
import { existsSync, mkdirSync, chmodSync, writeFileSync, openSync, unlinkSync } from "node:fs";
import { createServer, request } from "node:http";

const DIR = "/run/sbx-docker";
const SOCK = `${DIR}/start.sock`;
const DOCKER_SOCK = "/var/run/docker.sock";

// Claude Code's managed instructions, MCP servers and settings (its hooks; docs/CLAUDE-CODE.md): the controller passes
// them base64-encoded in JARVIS_CLAUDE_MD, JARVIS_CLAUDE_MCP and JARVIS_CLAUDE_SETTINGS; they are written root-owned,
// mode 644, and removed from the environment
for (const [key, file] of [
  ["JARVIS_CLAUDE_MD", "/etc/claude-code/CLAUDE.md"],
  ["JARVIS_CLAUDE_MCP", "/etc/claude-code/managed-mcp.json"],
  ["JARVIS_CLAUDE_SETTINGS", "/etc/claude-code/managed-settings.json"],
]) {
  const v = process.env[key];
  delete process.env[key];
  if (!v) continue;
  try {
    mkdirSync("/etc/claude-code", { recursive: true });
    writeFileSync(file, Buffer.from(v, "base64"), { mode: 0o644 });
  } catch (e) {
    console.error(JSON.stringify({ at: "sbx-init claude config", file, error: String(e?.message || e) }));
  }
}

// the agent's environment: the sandbox's (proxy, exec token); dockerd gets it without the exec token
const agentEnv = { ...process.env };
const dockerEnv = { ...process.env };
delete dockerEnv.JARVIS_EXEC_TOKEN;

/** apt.conf lines for the sandbox's proxy; "" without one. */
function aptProxyConf(env) {
  const q = (v) => String(v).replace(/["\\\n]/g, "");
  const http = env.http_proxy || env.HTTP_PROXY || "";
  const https = env.https_proxy || env.HTTPS_PROXY || http;
  return http ? `Acquire::http::Proxy "${q(http)}";\nAcquire::https::Proxy "${q(https)}";\n` : "";
}

// apt's proxy (mode 644: uid 1000 has the same proxy URL in its environment)
try {
  const conf = aptProxyConf(process.env);
  if (conf) writeFileSync("/etc/apt/apt.conf.d/80sandbox-proxy", conf, { mode: 0o644 });
} catch (e) {
  console.error(JSON.stringify({ at: "sbx-init apt proxy", error: String(e?.message || e) }));
}

// /etc/hosts (an image has none: the build mounts its own): localhost, and the sandbox's hostname, which sudo looks up
// on every call (without it each sudo waits for DNS, which the sandbox doesn't have, for over a minute)
try {
  if (!existsSync("/etc/hosts")) writeFileSync("/etc/hosts", "127.0.0.1\tlocalhost\n::1\tlocalhost ip6-localhost ip6-loopback\n127.0.1.1\tsandbox\n", { mode: 0o644 });
} catch (e) {
  console.error(JSON.stringify({ at: "sbx-init hosts", error: String(e?.message || e) }));
}

const agent = spawn(
  "setpriv",
  ["--reuid=1000", "--regid=1000", "--clear-groups", "--inh-caps=-all", "--ambient-caps=-all", "--", "node", "/opt/agent/agent.mjs"],
  { stdio: "inherit", env: agentEnv, cwd: "/work" },
);
agent.on("exit", (code, sig) => process.exit(code ?? (sig ? 128 : 1)));
for (const s of ["SIGTERM", "SIGINT"]) process.on(s, () => agent.kill(s));

let dockerd = null;
let starting = null;

function dockerAnswers(timeoutMs) {
  return new Promise((resolve) => {
    const r = request({ socketPath: DOCKER_SOCK, path: "/_ping", timeout: timeoutMs }, (res) => {
      res.resume();
      resolve(res.statusCode === 200);
    });
    r.on("error", () => resolve(false));
    r.on("timeout", () => {
      r.destroy();
      resolve(false);
    });
    r.end();
  });
}

async function startDockerd() {
  if (dockerd && dockerd.exitCode === null && (await dockerAnswers(2000))) return;
  mkdirSync("/etc/docker", { recursive: true });
  // no iptables and no bridge inside gVisor (gvisor.dev/docs/tutorials/docker-in-gvisor): containers use the
  // sandbox's own network (--network=host, added by the wrapper), so the egress lock still holds for them
  writeFileSync("/etc/docker/daemon.json", JSON.stringify({ iptables: false, ip6tables: false, bridge: "none", group: "node", "log-level": "warn" }));
  const log = openSync("/var/log/dockerd.log", "a");
  dockerd = spawn("dockerd", [], { stdio: ["ignore", log, log], env: dockerEnv, detached: false });
  dockerd.on("exit", () => (dockerd = null));
  const until = Date.now() + 60_000;
  while (Date.now() < until) {
    if (await dockerAnswers(1000)) return;
    if (!dockerd) throw new Error("dockerd exited at start (see /var/log/dockerd.log)");
    await new Promise((r) => setTimeout(r, 100));
  }
  throw new Error("dockerd did not answer within 60 s (see /var/log/dockerd.log)");
}

mkdirSync(DIR, { recursive: true });
chmodSync(DIR, 0o755);
const server = createServer((req, res) => {
  if (req.url !== "/start") {
    res.writeHead(404).end();
    return;
  }
  starting ??= startDockerd().finally(() => (starting = null));
  starting.then(
    () => res.writeHead(200).end("ok\n"),
    (e) => res.writeHead(500).end(`${e.message}\n`),
  );
});
if (existsSync(SOCK)) unlinkSync(SOCK);
server.listen(SOCK, () => chmodSync(SOCK, 0o666));
