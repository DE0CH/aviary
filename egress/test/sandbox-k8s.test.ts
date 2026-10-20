// Provider "kubernetes" without a cluster: create retries while the namespace's ResourceQuota still counts a pod the
// pool just released (it terminates for a few seconds), and gives up on any other error at once.
import assert from "node:assert/strict";
import { test } from "node:test";
import { parseConfig } from "../src/common/config.ts";
import type { K8sApi, Pod } from "../src/web/k8s.ts";
import { KubernetesProvider } from "../src/web/sandbox-k8s.ts";
import type { SandboxRow } from "../src/web/sandbox.ts";

const config = parseConfig({
  product: { name: "Test" }, publicUrl: "https://x.example.org", access: { teamDomain: "t.example.org", audience: "a" },
  users: ["a@example.org"], admins: [], models: [{ id: "m", label: "M" }], defaultModel: "m", titleModel: "m",
  systemPromptFile: "/dev/null", sandbox: { provider: "kubernetes", image: "img" },
  egress: { defaultAllowedHosts: [], routes: [] }, tools: {}, s3: { endpoint: "http://s3", region: "r", bucket: "b", prefix: "" },
});
const row = { id: "00000000-0000-0000-0000-000000000001", conversation_id: "c", pod: "sbx-1", exec_token: "e" } as unknown as SandboxRow;

function fake(errors: string[]): K8sApi & { calls: number } {
  const k = {
    calls: 0,
    async createPod(_ns: string, pod: object): Promise<Pod> {
      k.calls++;
      const e = errors.shift();
      if (e) throw new Error(e);
      return pod as Pod;
    },
    async getPod() { return null; },
    async deletePod() {},
    async listPods() { return []; },
  };
  return k;
}

test("create waits out a quota still counting a terminating pod", async () => {
  const k = fake(['k8s create pod: 403 pods "sbx-1" is forbidden: exceeded quota: sandboxes, requested: pods=1']);
  const p = new KubernetesProvider({ k8s: k, config, egressNamespace: "jarvis" });
  assert.deepEqual(await p.create(row, "tok"), { name: "sbx-1" });
  assert.equal(k.calls, 2);
});

test("create fails at once on other errors", async () => {
  const k = fake(["k8s create pod: 422 invalid"]);
  const p = new KubernetesProvider({ k8s: k, config, egressNamespace: "jarvis" });
  await assert.rejects(p.create(row, "tok"), /422/);
  assert.equal(k.calls, 1);
});
