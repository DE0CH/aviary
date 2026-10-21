#!/usr/bin/env node
// The first process of a sandbox on our own hosts (provider "vm", docs/SANDBOX-VM.md "Docker in sandboxes"). It runs
// as root INSIDE gVisor (its capabilities are the sandbox's own; the gVisor sentry has none on the host) for one job:
// starting dockerd on demand. Everything the server talks to runs as uid 1000 without capabilities:
//
// - the agent (agent.mjs) is started through setpriv as uid/gid 1000, no capabilities (bounding and inheritable sets
//   cleared) and no_new_privs, so neither it nor any command it runs can gain root;
// - this process listens on /run/sbx-docker/start.sock (HTTP over a unix socket). The `docker` wrapper
//   (/usr/local/bin/docker) asks it to start dockerd on the first docker command; dockerd's socket belongs to group
//   1000 (as on any host where the user is in the `docker` group). Sandboxes that never use Docker pay nothing for it.
//
// The kubernetes and e2b providers keep starting agent.mjs directly as uid 1000 (no Docker there).
import { spawn } from "node:child_process";
import { existsSync, mkdirSync, chmodSync, writeFileSync, openSync, unlinkSync } from "node:fs";
import { createServer, request } from "node:http";

const DIR = "/run/sbx-docker";
const SOCK = `${DIR}/start.sock`;
const DOCKER_SOCK = "/var/run/docker.sock";

// the agent's environment: the sandbox's (proxy, exec token); dockerd gets it without the exec token
const agentEnv = { ...process.env };
const dockerEnv = { ...process.env };
delete dockerEnv.JARVIS_EXEC_TOKEN;

const agent = spawn(
  "setpriv",
  ["--reuid=1000", "--regid=1000", "--clear-groups", "--inh-caps=-all", "--bounding-set=-all", "--no-new-privs", "--", "node", "/opt/agent/agent.mjs"],
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
