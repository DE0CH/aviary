// A fake Kubernetes API for the sandbox manager: creating a pod starts the REAL sandbox agent (chat/sandbox/agent.mjs)
// as a local process on its own port and workdir; deleting the pod kills it.
import { spawn, type ChildProcess } from "node:child_process";
import { mkdtempSync, rmSync } from "node:fs";
import { createServer as netServer } from "node:net";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { fileURLToPath } from "node:url";
import type { K8sApi, Pod } from "../../src/web/k8s.ts";

const AGENT = fileURLToPath(new URL("../../sandbox/agent.mjs", import.meta.url));

async function freePort(): Promise<number> {
  return new Promise((resolve) => {
    const s = netServer();
    s.listen(0, "127.0.0.1", () => {
      const p = (s.address() as { port: number }).port;
      s.close(() => resolve(p));
    });
  });
}

interface FakePod {
  manifest: any;
  child: ChildProcess;
  port: number;
  work: string;
  created: string;
}

export class FakeK8s implements K8sApi {
  readonly pods = new Map<string, FakePod>();
  readonly created: any[] = [];
  /** Pods that exist in the API with no agent behind them (orphan tests). */
  readonly ghosts = new Map<string, Pod>();

  async createPod(ns: string, pod: any): Promise<Pod> {
    this.created.push(pod);
    const port = await freePort();
    const work = mkdtempSync(join(tmpdir(), "sbx-work-"));
    const env: Record<string, string> = { PATH: process.env.PATH!, HOME: work, JARVIS_WORK_DIR: work, PORT: String(port) };
    for (const e of pod.spec.containers[0].env) if (e.name === "JARVIS_EXEC_TOKEN") env[e.name] = e.value;
    const child = spawn(process.execPath, [AGENT], { env, stdio: "ignore" });
    this.pods.set(pod.metadata.name, { manifest: pod, child, port, work, created: new Date().toISOString() });
    return { metadata: { name: pod.metadata.name } };
  }
  async getPod(ns: string, name: string): Promise<Pod | null> {
    const p = this.pods.get(name);
    if (!p) return this.ghosts.get(name) ?? null;
    return {
      metadata: { name, labels: p.manifest.metadata.labels, creationTimestamp: p.created },
      status: { phase: "Running", podIP: "127.0.0.1" },
    };
  }
  async deletePod(ns: string, name: string): Promise<void> {
    this.ghosts.delete(name);
    const p = this.pods.get(name);
    if (!p) return;
    this.pods.delete(name);
    p.child.kill("SIGKILL");
    rmSync(p.work, { recursive: true, force: true });
  }
  async listPods(): Promise<Pod[]> {
    const out: Pod[] = [];
    for (const name of this.pods.keys()) out.push((await this.getPod("", name))!);
    for (const g of this.ghosts.values()) out.push(g);
    return out;
  }
  agentUrlFor = (pod: Pod): string => `http://127.0.0.1:${this.pods.get(pod.metadata.name)?.port ?? 1}`;
  workOf(name: string): string {
    return this.pods.get(name)!.work;
  }
  async closeAll(): Promise<void> {
    for (const name of [...this.pods.keys()]) await this.deletePod("", name);
  }
}
