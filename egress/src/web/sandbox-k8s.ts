// Sandbox provider "kubernetes": one gVisor pod per sandbox in config.sandbox.namespace, reached at <pod IP>:7070.
// The pod's env carries the proxy settings and the exec token; NetworkPolicy keeps its only way out the egress
// Service. Used by the tests (a fake Kubernetes API that runs the real agent) and by a self-hosted sandbox cluster.
import type { ChatConfig } from "../common/config.ts";
import { AgentClient } from "./agent.ts";
import { cpuCores, memBytes, type K8sApi, type Pod } from "./k8s.ts";
import type { AgentEndpoint, HostStats, ListedSandbox, ProviderStats, SandboxProvider, SandboxStats } from "./provider.ts";
import type { SandboxRow } from "./sandbox.ts";

/** The env every sandbox gets (docs/CHAT.md, "No secrets in a sandbox"): proxy, mirrors, exec token. */
export function sandboxEnv(proxyBase: string, proxyHost: string, egressToken: string): Record<string, string> {
  const proxy = proxyBase.replace("://", `://sbx:${egressToken}@`);
  return {
    HTTP_PROXY: proxy,
    HTTPS_PROXY: proxy,
    http_proxy: proxy,
    https_proxy: proxy,
    NO_PROXY: "",
    no_proxy: "",
    PIP_INDEX_URL: `${proxy}/mirror/pypi/simple`,
    PIP_TRUSTED_HOST: proxyHost,
    npm_config_registry: `${proxy}/mirror/npm/`,
  };
}

export class KubernetesProvider implements SandboxProvider {
  readonly kind = "kubernetes" as const;
  readonly #k8s: K8sApi;
  readonly #config: ChatConfig;
  /** The namespace egress runs in (its Service is the sandboxes' proxy). */
  readonly #egressNs: string;
  readonly #agentUrlFor: (pod: Pod) => string;

  constructor(o: { k8s: K8sApi; config: ChatConfig; egressNamespace: string; agentUrlFor?: (pod: Pod) => string }) {
    this.#k8s = o.k8s;
    this.#config = o.config;
    this.#egressNs = o.egressNamespace;
    this.#agentUrlFor =
      o.agentUrlFor ??
      ((pod) => {
        const ip = pod.status?.podIP || "";
        return `http://${ip.includes(":") ? `[${ip}]` : ip}:7070`;
      });
  }

  get #ns(): string {
    return this.#config.sandbox.namespace;
  }

  async create(row: SandboxRow, egressToken: string): Promise<{ name: string }> {
    // a namespace ResourceQuota still counts a pod the pool just released while it terminates: retry for a minute
    const until = Date.now() + QUOTA_WAIT_MS;
    for (;;) {
      try {
        await this.#k8s.createPod(this.#ns, this.podManifest(row, egressToken));
        return { name: row.pod };
      } catch (e) {
        if (!/exceeded quota/.test((e as Error).message) || Date.now() > until) throw e;
        await sleep(1000);
      }
    }
  }

  async waitReady(row: SandboxRow, deadline: number): Promise<AgentEndpoint> {
    while (Date.now() < deadline) {
      const pod = await this.#k8s.getPod(this.#ns, row.pod);
      if (!pod) throw new Error("the sandbox pod disappeared while starting");
      const phase = pod.status?.phase;
      if (phase === "Failed" || phase === "Succeeded") throw new Error(`the sandbox pod ended (${pod.status?.reason || phase})`);
      const waiting = pod.status?.containerStatuses?.[0]?.state?.waiting?.reason;
      if (waiting && /ErrImagePull|ImagePullBackOff|CreateContainerConfigError|InvalidImageName/.test(waiting)) throw new Error(`the sandbox pod can't start: ${waiting}`);
      if (phase === "Running" && pod.status?.podIP) {
        const url = this.#agentUrlFor(pod);
        const ok = await new AgentClient(url, row.exec_token).health(2000).then((h) => h.ok, () => false);
        if (ok) return { url, headers: {} };
      }
      await sleep(500);
    }
    throw new Error("the sandbox did not become ready in time");
  }

  async keepAlive(): Promise<void> {}

  async destroy(name: string): Promise<void> {
    await this.#k8s.deletePod(this.#ns, name);
  }

  async list(): Promise<ListedSandbox[]> {
    const pods = await this.#k8s.listPods(this.#ns, "app=jarvis-sandbox");
    return pods.map((p) => ({ name: p.metadata.name, createdAt: p.metadata.creationTimestamp ? Date.parse(p.metadata.creationTimestamp) : NaN }));
  }

  /**
   * The admin monitor's view: sandbox pods with their use (metrics.k8s.io, when metrics-server runs and the role may
   * read it) and the nodes that can run them (the RuntimeClass's nodeSelector) or do. Read-only; what the web role's
   * RBAC doesn't allow ends up in `notes`.
   */
  async stats(): Promise<ProviderStats> {
    const notes: string[] = [];
    const ns = this.#ns;
    const get = this.#k8s.getJson?.bind(this.#k8s);
    const why = (what: string, e: unknown) => notes.push(`${what}: ${(e as Error).message}`);
    const pods = (await this.#k8s.listPods(ns, "app=jarvis-sandbox")) as (Pod & { spec?: any; status?: any })[];
    const use = new Map<string, { cpu: number; mem: number }>();
    const nodeUse = new Map<string, { cpu: number; mem: number }>();
    let nodes: any[] = [];
    let selector: Record<string, string> | null = null;
    if (!get) notes.push("node and pod metrics: this Kubernetes client can't read them");
    else {
      await Promise.all([
        get(`/apis/metrics.k8s.io/v1beta1/namespaces/${ns}/pods`).then(
          (m) => {
            for (const it of m?.items ?? []) {
              const cs: any[] = it.containers ?? [];
              use.set(it.metadata?.name, { cpu: cs.reduce((a, c) => a + (cpuCores(c.usage?.cpu) ?? 0), 0), mem: cs.reduce((a, c) => a + (memBytes(c.usage?.memory) ?? 0), 0) });
            }
          },
          (e) => why("pod metrics", e),
        ),
        get("/api/v1/nodes").then((r) => (nodes = r?.items ?? []), (e) => why("nodes", e)),
        get("/apis/metrics.k8s.io/v1beta1/nodes").then(
          (m) => {
            for (const it of m?.items ?? []) nodeUse.set(it.metadata?.name, { cpu: cpuCores(it.usage?.cpu) ?? 0, mem: memBytes(it.usage?.memory) ?? 0 });
          },
          (e) => why("node metrics", e),
        ),
        this.#config.sandbox.runtimeClass
          ? get(`/apis/node.k8s.io/v1/runtimeclasses/${encodeURIComponent(this.#config.sandbox.runtimeClass)}`).then(
              (rc) => (selector = rc?.scheduling?.nodeSelector ?? null),
              (e) => why("runtime class", e),
            )
          : Promise.resolve(),
      ]);
    }
    const sandboxes: SandboxStats[] = pods.map((p) => {
      const c = p.spec?.containers?.[0];
      const u = use.get(p.metadata.name);
      const waiting = p.status?.containerStatuses?.[0]?.state?.waiting?.reason;
      return {
        name: p.metadata.name,
        host: p.spec?.nodeName ?? null,
        state: p.metadata.deletionTimestamp ? "Terminating" : waiting || p.status?.phase || "Unknown",
        cpuUsed: u ? u.cpu : null,
        cpuLimit: cpuCores(c?.resources?.limits?.cpu),
        memUsed: u ? u.mem : null,
        memLimit: memBytes(c?.resources?.limits?.memory),
        restarts: (p.status?.containerStatuses ?? []).reduce((a: number, s: any) => a + (s.restartCount ?? 0), 0),
      };
    });
    const perNode = new Map<string, number>();
    for (const s of sandboxes) if (s.host) perNode.set(s.host, (perNode.get(s.host) ?? 0) + 1);
    const sel = selector as Record<string, string> | null;
    const hosts: HostStats[] = nodes
      .filter((n) => perNode.has(n.metadata?.name) || (sel && Object.entries(sel).every(([k, v]) => n.metadata?.labels?.[k] === v)))
      .map((n) => {
        const name = n.metadata.name as string;
        const ready = (n.status?.conditions ?? []).some((c: any) => c.type === "Ready" && c.status === "True");
        const u = nodeUse.get(name);
        return {
          name,
          kind: "node",
          status: (ready ? "ready" : "not ready") + (n.spec?.unschedulable ? ", cordoned" : ""),
          reachable: ready,
          cpus: cpuCores(n.status?.allocatable?.cpu),
          cpuUsed: u ? u.cpu : null,
          memTotal: memBytes(n.status?.allocatable?.memory),
          memUsed: u ? u.mem : null,
          load1: null,
          sandboxes: perNode.get(name) ?? 0,
          maxSandboxes: null,
          startedAt: n.metadata.creationTimestamp ?? null,
          costUsd: null,
          detail: { kubelet: n.status?.nodeInfo?.kubeletVersion ?? null },
        };
      });
    return { provider: this.kind, hosts, sandboxes, capacity: null, notes };
  }

  podManifest(row: SandboxRow, egressToken: string): object {
    const cfg = this.#config.sandbox;
    const proxy = new URL(cfg.egressProxyUrl || `http://egress.${this.#egressNs}.svc.cluster.local:3128`);
    const env: Record<string, string> = {
      ...sandboxEnv(proxy.origin, proxy.hostname, egressToken),
      JARVIS_EXEC_TOKEN: row.exec_token,
      HOME: "/home/sandbox",
    };
    return {
      apiVersion: "v1",
      kind: "Pod",
      metadata: {
        name: row.pod,
        namespace: cfg.namespace,
        labels: { app: "jarvis-sandbox", "jarvis.conversation": row.conversation_id, "jarvis.sandbox": row.id },
      },
      spec: {
        runtimeClassName: cfg.runtimeClass || undefined,
        restartPolicy: "Never",
        automountServiceAccountToken: false,
        enableServiceLinks: false,
        terminationGracePeriodSeconds: 5,
        securityContext: { runAsNonRoot: true, runAsUser: 1000, runAsGroup: 1000, fsGroup: 1000, seccompProfile: { type: "RuntimeDefault" } },
        containers: [
          {
            name: "sandbox",
            image: cfg.image,
            imagePullPolicy: "IfNotPresent",
            ports: [{ containerPort: 7070, name: "agent" }],
            env: Object.entries(env).map(([name, value]) => ({ name, value })),
            securityContext: { allowPrivilegeEscalation: false, capabilities: { drop: ["ALL"] }, runAsNonRoot: true, runAsUser: 1000 },
            // request = limit: a sandbox holds exactly what it may use, so the namespace quota caps concurrency
            // and a runaway sandbox can only hurt itself (Deyao, 2026-10-20)
            resources: {
              requests: { cpu: cfg.cpu, memory: cfg.memory, "ephemeral-storage": "5Gi" },
              limits: { cpu: cfg.cpu, memory: cfg.memory, "ephemeral-storage": "5Gi" },
            },
          },
        ],
        ...(cfg.priorityClass ? { priorityClassName: cfg.priorityClass } : {}),
        ...(cfg.imagePullSecret ? { imagePullSecrets: [{ name: cfg.imagePullSecret }] } : {}),
      },
    };
  }
}

const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));
export const QUOTA_WAIT_MS = 60_000;
