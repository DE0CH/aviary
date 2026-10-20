// Sandboxes, design "B" (docs/CHAT.md, "Sandbox lifecycle"): one gVisor pod per active conversation, made when the
// staff member starts typing or a turn needs it, kept while active, released when idle or at its hard maximum
// lifetime (after a final save of what it changed). Postgres `sandboxes` is the record; the pods are disposable.
import { randomUUID } from "node:crypto";
import { tx } from "../common/db.ts";
import { logError, log } from "../common/log.ts";
import { newToken, sha256hex } from "../common/tokens.ts";
import { AgentClient } from "./agent.ts";
import type { WebDeps } from "./ctx.ts";
import { storeEntries } from "./files.ts";
import type { Pod } from "./k8s.ts";

export interface SandboxRow {
  id: string;
  conversation_id: string;
  pod: string;
  exec_token: string;
  status: "starting" | "ready" | "releasing" | "gone";
  created_at: Date;
  last_active_at: Date;
  expires_at: Date;
}

export interface LiveSandbox {
  id: string;
  pod: string;
  url: string;
  execToken: string;
}

export class PoolFullError extends Error {}

const CAPACITY_LOCK = 4_242_002;
const POOL_WAIT_MS = 60_000;
const START_TIMEOUT_MS = 180_000;

export class SandboxManager {
  readonly #deps: WebDeps;
  readonly #urls = new Map<string, string>();
  readonly #inflight = new Map<string, Promise<LiveSandbox>>();
  constructor(deps: WebDeps) {
    this.#deps = deps;
  }

  get #ns(): string {
    return this.#deps.config.sandbox.namespace;
  }

  #agentUrl(pod: Pod): string {
    if (this.#deps.agentUrlFor) return this.#deps.agentUrlFor(pod);
    const ip = pod.status?.podIP || "";
    return `http://${ip.includes(":") ? `[${ip}]` : ip}:7070`;
  }

  async #urlOf(row: SandboxRow): Promise<string | null> {
    const cached = this.#urls.get(row.pod);
    if (cached) return cached;
    const pod = await this.#deps.k8s.getPod(this.#ns, row.pod);
    if (!pod?.status?.podIP || pod.metadata.deletionTimestamp) return null;
    const url = this.#agentUrl(pod);
    this.#urls.set(row.pod, url);
    return url;
  }

  async touch(conversationId: string): Promise<void> {
    await this.#deps.db.query("update sandboxes set last_active_at = now() where conversation_id = $1 and status in ('starting','ready')", [conversationId]);
  }

  /** The conversation's live sandbox, made if there is none. Concurrent callers in this process share one attempt. */
  ensure(conversationId: string): Promise<LiveSandbox> {
    let p = this.#inflight.get(conversationId);
    if (!p) {
      p = this.#ensure(conversationId).finally(() => this.#inflight.delete(conversationId));
      this.#inflight.set(conversationId, p);
    }
    return p;
  }

  async #ensure(conv: string): Promise<LiveSandbox> {
    const deadline = Date.now() + START_TIMEOUT_MS;
    const poolDeadline = Date.now() + POOL_WAIT_MS;
    while (Date.now() < deadline) {
      const { rows } = await this.#deps.db.query<SandboxRow>("select * from sandboxes where conversation_id = $1 and status <> 'gone'", [conv]);
      const row = rows[0];
      if (row?.status === "ready") {
        const url = await this.#urlOf(row).catch(() => null);
        if (url) {
          const ok = await new AgentClient(url, row.exec_token).health().then((h) => h.ok, () => false);
          if (ok) {
            await this.touch(conv);
            return { id: row.id, pod: row.pod, url, execToken: row.exec_token };
          }
        }
        await this.release(row, "unhealthy");
        continue;
      }
      if (row?.status === "starting" && Date.now() - row.created_at.getTime() > START_TIMEOUT_MS) {
        await this.#forget(row, "start timed out");
        continue;
      }
      if (row) {
        // starting elsewhere, or being released: wait for it to settle
        await sleep(500);
        continue;
      }
      const created = await this.#claimSlot(conv);
      if (created === "full") {
        if (Date.now() > poolDeadline) throw new PoolFullError("the sandbox pool is full, try again in a minute");
        await sleep(2000);
        continue;
      }
      if (created === "raced") continue;
      return await this.#start(created);
    }
    throw new Error("the sandbox did not start in time");
  }

  /** Inserts a `starting` row if the pool has room (freeing the least recently used idle sandbox if not). */
  async #claimSlot(conv: string): Promise<SandboxRow | "full" | "raced"> {
    const cfg = this.#deps.config.sandbox;
    const egressToken = newToken();
    let victim: SandboxRow | null = null;
    const r = await tx(this.#deps.db, async (c) => {
      await c.query("select pg_advisory_xact_lock($1)", [CAPACITY_LOCK]);
      const { rows: live } = await c.query<{ n: string }>("select count(*) as n from sandboxes where status <> 'gone'");
      if (Number(live[0]!.n) >= cfg.maxActive) {
        const { rows: idle } = await c.query<SandboxRow>(
          `select s.* from sandboxes s where s.status = 'ready' and s.conversation_id <> $1
             and not exists (select 1 from turns t where t.conversation_id = s.conversation_id and t.status in ('queued','running','waiting'))
           order by s.last_active_at limit 1`,
          [conv],
        );
        victim = idle[0] ?? null;
        return "full" as const;
      }
      const id = randomUUID();
      try {
        const { rows } = await c.query<SandboxRow>(
          `insert into sandboxes (id, conversation_id, pod, egress_token_hash, exec_token, status, expires_at)
           values ($1, $2, $3, $4, $5, 'starting', now() + make_interval(hours => $6)) returning *`,
          [id, conv, `sbx-${id.replace(/-/g, "").slice(0, 16)}`, sha256hex(egressToken), newToken(), cfg.maxLifetimeHours],
        );
        return { row: rows[0]!, egressToken };
      } catch (e) {
        if ((e as { code?: string }).code === "23505") return "raced" as const;
        throw e;
      }
    });
    if (r === "full") {
      if (victim) {
        await this.release(victim, "pool");
        return "raced";
      }
      return "full";
    }
    if (r === "raced") return r;
    this.#egressTokens.set(r.row.id, r.egressToken);
    return r.row;
  }

  readonly #egressTokens = new Map<string, string>();

  async #start(row: SandboxRow): Promise<LiveSandbox> {
    const egressToken = this.#egressTokens.get(row.id)!;
    this.#egressTokens.delete(row.id);
    try {
      await this.#deps.k8s.createPod(this.#ns, this.podManifest(row, egressToken));
      const deadline = Date.now() + START_TIMEOUT_MS;
      let url = "";
      while (Date.now() < deadline) {
        const pod = await this.#deps.k8s.getPod(this.#ns, row.pod);
        if (!pod) throw new Error("the sandbox pod disappeared while starting");
        const phase = pod.status?.phase;
        if (phase === "Failed" || phase === "Succeeded") throw new Error(`the sandbox pod ended (${pod.status?.reason || phase})`);
        const waiting = pod.status?.containerStatuses?.[0]?.state?.waiting?.reason;
        if (waiting && /ErrImagePull|ImagePullBackOff|CreateContainerConfigError|InvalidImageName/.test(waiting)) throw new Error(`the sandbox pod can't start: ${waiting}`);
        if (phase === "Running" && pod.status?.podIP) {
          url = this.#agentUrl(pod);
          const ok = await new AgentClient(url, row.exec_token).health(2000).then((h) => h.ok, () => false);
          if (ok) break;
        }
        await sleep(500);
        url = "";
      }
      if (!url) throw new Error("the sandbox did not become ready in time");
      this.#urls.set(row.pod, url);
      await this.#deps.db.query("update sandboxes set status = 'ready', last_active_at = now() where id = $1", [row.id]);
      log("sandbox ready", { sandbox: row.id, conversation: row.conversation_id, pod: row.pod });
      return { id: row.id, pod: row.pod, url, execToken: row.exec_token };
    } catch (e) {
      await this.#forget(row, `start failed: ${(e as Error).message}`);
      throw e;
    }
  }

  async #forget(row: SandboxRow, reason: string): Promise<void> {
    this.#urls.delete(row.pod);
    await this.#deps.k8s.deletePod(this.#ns, row.pod).catch((e) => logError("sandbox delete", e, { pod: row.pod }));
    await this.#deps.db.query("update sandboxes set status = 'gone', released_at = now(), release_reason = $2 where id = $1 and status <> 'gone'", [row.id, reason]);
  }

  /** Final save of what the sandbox changed (files only), then delete its pod. Safe to call twice. */
  async release(row: SandboxRow, reason: string): Promise<void> {
    const { rows } = await this.#deps.db.query<SandboxRow>(
      "update sandboxes set status = 'releasing' where id = $1 and status in ('ready','starting') returning *",
      [row.id],
    );
    const r = rows[0];
    if (!r) return;
    {
      try {
        const url = await this.#urlOf(r);
        if (url) {
          const { entries } = await new AgentClient(url, r.exec_token).changes("");
          const n = await storeEntries(this.#deps, r.conversation_id, entries);
          if (n) log("sandbox final save", { sandbox: r.id, files: n });
        }
      } catch (e) {
        logError("sandbox final save", e, { sandbox: r.id });
      }
    }
    await this.#forget(r, reason);
    log("sandbox released", { sandbox: r.id, conversation: r.conversation_id, reason });
  }

  async releaseConversation(conv: string, reason: string): Promise<void> {
    const { rows } = await this.#deps.db.query<SandboxRow>("select * from sandboxes where conversation_id = $1 and status <> 'gone'", [conv]);
    for (const r of rows) await this.release(r, reason);
  }

  podManifest(row: SandboxRow, egressToken: string): object {
    const cfg = this.#deps.config.sandbox;
    const egressHost = `egress.${this.#deps.namespace}.svc.cluster.local`;
    const proxy = `http://sbx:${egressToken}@${egressHost}:3128`;
    const env: Record<string, string> = {
      HTTP_PROXY: proxy,
      HTTPS_PROXY: proxy,
      http_proxy: proxy,
      https_proxy: proxy,
      NO_PROXY: "",
      no_proxy: "",
      PIP_INDEX_URL: `${proxy}/mirror/pypi/simple`,
      PIP_TRUSTED_HOST: egressHost,
      npm_config_registry: `${proxy}/mirror/npm/`,
      JARVIS_EXEC_TOKEN: row.exec_token,
      HOME: "/home/user",
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

export const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));
