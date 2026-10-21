// The admin monitor's pure parts: redaction of error lines, Kubernetes quantities, tool services, and the vm
// provider's stats mapped onto the common hosts/sandboxes shape.
import assert from "node:assert/strict";
import { test } from "node:test";
import { recentErrors, logError, redact } from "../src/common/log.ts";
import { cpuCores, memBytes } from "../src/web/k8s.ts";
import { normalizeStats, toolService } from "../src/web/monitor.ts";

test("redact removes credentials, keeps ids and words", () => {
  const conv = "4943ad9e-c41c-4560-a32d-2c1dabe4d593";
  const out = redact(
    `fetch http://sbx:AbC123def456GHI789jkl012@egress:3128 failed; Authorization: Bearer eyJhbGciOiJ.x.y token=abc123 conv ${conv} key: s3cr3t ` +
      "blob 0123456789abcdef0123456789abcdef0123 upstream unavailable",
  );
  assert.doesNotMatch(out, /AbC123|eyJhbG|abc123|s3cr3t|0123456789abcdef0123/);
  assert.match(out, new RegExp(conv));
  assert.match(out, /upstream unavailable/);
  const prev = process.stderr.write;
  process.stderr.write = () => true;
  try {
    logError("test", new Error("Bearer secretvalue123 went wrong"));
  } finally {
    process.stderr.write = prev;
  }
  assert.match(recentErrors(1)[0]!.error, /Bearer \[redacted\] went wrong/);
});

test("Kubernetes quantities", () => {
  assert.equal(cpuCores("250m"), 0.25);
  assert.equal(cpuCores("1500000000n"), 1.5);
  assert.equal(cpuCores("2"), 2);
  assert.equal(cpuCores("x"), null);
  assert.equal(memBytes("1Gi"), 1024 ** 3);
  assert.equal(memBytes("524288Ki"), 512 * 1024 ** 2);
  assert.equal(memBytes(undefined), null);
});

test("tool services", () => {
  assert.equal(toolService("web_search", ["yyc"]), "Exa");
  assert.equal(toolService("mcp__tools__social_search", ["yyc"]), "TikHub");
  assert.equal(toolService("news_search", ["yyc"]), "Wisers");
  assert.equal(toolService("yyc_list_reports", ["yyc"]), "yyc-mcp");
  assert.equal(toolService("web_fetch", []), "web fetch");
});

test("vm stats map onto hosts and sandboxes", () => {
  const now = new Date().toISOString();
  const s = normalizeStats({
    provider: "vm",
    at: now,
    scaler: { spareSlots: 1, minHostLifetimeSeconds: 900, maxAwsHosts: 3, freeSlots: 2, demand: 0, memoryPerSandbox: 1024 ** 3, cpuPerSandbox: 1, lastPassAt: now },
    hosts: [
      {
        name: "hub", kind: "hub", status: "active", instanceId: null, cpus: 8, memTotal: 32e9, memAvailable: 20e9, loadAvg: [0.5, 0.4, 0.3], slots: 12, used: 1, underPressure: false,
        maxSandboxes: null, lastSeenAt: now, upSince: now, upHours: 1, costUsd: null, gvisorRelease: "x",
        sandboxes: [{ name: "sbx-1", id: "i", conversationId: "c", ip: "10.0.0.1", state: "running", cpuUsage: 0.3, memoryCurrent: 2e8, memoryLimit: 1024 ** 3, busy: false }],
      },
      {
        name: "aws-1", kind: "aws", status: "draining", instanceId: "i-1", cpus: 4, memTotal: 16e9, memAvailable: null, loadAvg: null, slots: 6, used: 0, underPressure: false,
        maxSandboxes: null, lastSeenAt: null, upSince: now, upHours: 2, costUsd: 0.5, gvisorRelease: null, sandboxes: [],
      },
    ],
    awsCostLast30dUsd: 3.2,
    events: [],
    migrations: [],
  });
  assert.equal(s.capacity, 12);
  assert.deepEqual(s.hosts.map((h) => [h.name, h.reachable, h.load1, h.memUsed, h.costUsd]), [["hub", true, 0.5, 12e9, null], ["aws-1", false, null, null, 0.5]]);
  assert.deepEqual(s.sandboxes, [{ name: "sbx-1", host: "hub", state: "running", cpuUsed: 0.3, cpuLimit: 1, memUsed: 2e8, memLimit: 1024 ** 3, restarts: null }]);
  assert.equal((s.extra as any).awsCostLast30dUsd, 3.2);
});
