// Sandboxes (docs/CHAT.md, "Sandbox lifecycle"): one sandbox per active conversation, made when the CloudCLI UI
// runs Claude Code in it (/internal/cc/sandbox), kept while it is used (/internal/cc/touch), released when idle, at its
// hard maximum lifetime or when the conversation is deleted. Nothing is saved on release: /workspace and ~/.claude live
// on JuiceFS. Postgres `sandboxes` is the record; the sandboxes themselves come from the configured provider
// (provider.ts) and are disposable.
import { randomUUID } from "node:crypto";
import { tx } from "../common/db.ts";
import { logError, log, redact } from "../common/log.ts";
import { opsEvent } from "../common/ops.ts";
import { newToken, sha256hex } from "../common/tokens.ts";
import { AgentClient } from "./agent.ts";
import type { WebDeps } from "./ctx.ts";
import type { AgentEndpoint, SandboxProvider } from "./provider.ts";

export interface SandboxRow {
  id: string;
  conversation_id: string;
  /** The provider's name for the sandbox: the pod name, or E2B's sandbox id once it exists. */
  pod: string;
  exec_token: string;
  status: "starting" | "ready" | "releasing" | "gone";
  created_at: Date;
  last_active_at: Date;
  expires_at: Date;
  /** How to reach its agent, set when it became ready. */
  agent: AgentEndpoint | null;
  /** Its owner, and the key of the owner's ~/.claude folder (docs/CLAUDE-CODE.md); the provider runs it with the
   * conversation's /workspace and Claude Code's settings. Null only on rows from before Claude Code in the sandbox. */
  owner?: string | null;
  user_key?: string | null;
}

/** The ~/.claude folder name of a user (docs/CLAUDE-CODE.md): stable, no email in paths. */
export function userKey(email: string): string {
  return "u" + sha256hex(email.trim().toLowerCase()).slice(0, 24);
}

export interface LiveSandbox {
  id: string;
  pod: string;
  url: string;
  /** Provider headers for every agent call (E2B's traffic access token). */
  headers: Record<string, string>;
  execToken: string;
}

export class PoolFullError extends Error {}

/** How long a typing signal (noteSandboxDemand) counts for the scaler's launch decision. */
export const DEMAND_WINDOW_S = 120;

type Q = { query: (q: string, v: unknown[]) => Promise<unknown> };

/**
 * Someone may need a sandbox soon. Nothing is created: the vm scaler counts these and starts an AWS host at once when
 * no host has a free slot (docs/SANDBOX-VM.md, "Scaler"). One key per person, so a busy typist counts once. Nothing in
 * production sends this signal since the old chat UI was retired; the CloudCLI UI does not (yet) report typing.
 */
export async function noteSandboxDemand(db: Q, who: string): Promise<void> {
  await db.query("insert into kv (key, value) values ($1, $2) on conflict (key) do update set value = excluded.value", [
    `sandbox_demand:${who}`,
    JSON.stringify({ at: new Date().toISOString() }),
  ]);
}

const CAPACITY_LOCK = 4_242_002;
const POOL_WAIT_MS = 60_000;
const START_TIMEOUT_MS = 180_000;
/** A provider's own timeout runs this long past the moment the janitor would release the sandbox. */
const PROVIDER_GRACE_MS = 5 * 60_000;
const KEEPALIVE_EVERY_MS = 30_000;

export class SandboxManager {
  readonly #deps: WebDeps;
  readonly #inflight = new Map<string, Promise<LiveSandbox>>();
  readonly #kept = new Map<string, number>();
  constructor(deps: WebDeps) {
    this.#deps = deps;
  }

  get provider(): SandboxProvider {
    return this.#deps.sandboxProvider;
  }

  #live(row: SandboxRow): LiveSandbox | null {
    if (!row.agent?.url) return null;
    return { id: row.id, pod: row.pod, url: row.agent.url, headers: row.agent.headers ?? {}, execToken: row.exec_token };
  }

  /** How long the provider should keep a sandbox that was active just now: past the idle release, never past its end. */
  #keepUntil(row: SandboxRow): Date {
    const idle = Date.now() + this.#deps.config.sandbox.idleMinutes * 60_000 + PROVIDER_GRACE_MS;
    return new Date(Math.min(idle, new Date(row.expires_at).getTime() + PROVIDER_GRACE_MS));
  }

  async #keepAlive(row: SandboxRow, force = false): Promise<void> {
    const last = this.#kept.get(row.id) ?? 0;
    if (!force && Date.now() - last < KEEPALIVE_EVERY_MS) return;
    this.#kept.set(row.id, Date.now());
    if (this.#kept.size > 1000) this.#kept.clear();
    await this.provider.keepAlive(row, this.#keepUntil(row)).catch((e) => logError("sandbox keep-alive", e, { sandbox: row.id }));
  }

  /** The sandbox was used just now (Claude Code runs in it: the UI touches it every few seconds): its idle time
   * starts again. */
  async touch(conversationId: string): Promise<void> {
    const { rows } = await this.#deps.db.query<SandboxRow>(
      "update sandboxes set last_active_at = now() where conversation_id = $1 and status in ('starting','ready') returning *",
      [conversationId],
    );
    for (const r of rows) if (r.status === "ready") await this.#keepAlive(r);
  }

  /** The conversation's live sandbox for `owner`, made if there is none. Concurrent callers in this process share one
   * attempt. */
  ensure(conversationId: string, owner: string): Promise<LiveSandbox> {
    let p = this.#inflight.get(conversationId);
    if (!p) {
      p = this.#ensure(conversationId, owner).finally(() => this.#inflight.delete(conversationId));
      this.#inflight.set(conversationId, p);
    }
    return p;
  }

  async #ensure(conv: string, owner: string): Promise<LiveSandbox> {
    const t0 = Date.now();
    const deadline = Date.now() + START_TIMEOUT_MS;
    const poolDeadline = Date.now() + POOL_WAIT_MS;
    while (Date.now() < deadline) {
      const { rows } = await this.#deps.db.query<SandboxRow>("select * from sandboxes where conversation_id = $1 and status <> 'gone'", [conv]);
      const row = rows[0];
      if (row?.status === "ready") {
        const live = this.#live(row);
        if (live) {
          const ok = await new AgentClient(live.url, live.execToken, live.headers).health().then((h) => h.ok, () => false);
          if (ok) {
            await this.touch(conv);
            return live;
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
      const created = await this.#claimSlot(conv, owner);
      if (created === "full") {
        if (Date.now() > poolDeadline) {
          void opsEvent(this.#deps.db, "sandbox_start", { ok: false, ms: Date.now() - t0, conversationId: conv, detail: { reason: "pool full", provider: this.provider.kind } });
          throw new PoolFullError("the sandbox pool is full, try again in a minute");
        }
        await sleep(2000);
        continue;
      }
      if (created === "raced") continue;
      return await this.#start(created, t0);
    }
    void opsEvent(this.#deps.db, "sandbox_start", { ok: false, ms: Date.now() - t0, conversationId: conv, detail: { reason: "did not start in time", provider: this.provider.kind } });
    throw new Error("the sandbox did not start in time");
  }

  /** Inserts a `starting` row if the pool has room (freeing the least recently used idle sandbox if not). */
  async #claimSlot(conv: string, owner: string): Promise<SandboxRow | "full" | "raced"> {
    const cfg = this.#deps.config.sandbox;
    const egressToken = newToken();
    let victim: SandboxRow | null = null;
    const r = await tx(this.#deps.db, async (c) => {
      await c.query("select pg_advisory_xact_lock($1)", [CAPACITY_LOCK]);
      const { rows: live } = await c.query<{ n: string }>("select count(*) as n from sandboxes where status <> 'gone'");
      if (Number(live[0]!.n) >= cfg.maxActive) {
        const { rows: idle } = await c.query<SandboxRow>(
          `select s.* from sandboxes s where s.status = 'ready' and s.conversation_id <> $1
             and s.last_active_at < now() - interval '2 minutes' -- in use: the UI touches it every minute
           order by s.last_active_at limit 1`,
          [conv],
        );
        victim = idle[0] ?? null;
        return "full" as const;
      }
      const id = randomUUID();
      try {
        const { rows } = await c.query<SandboxRow>(
          `insert into sandboxes (id, conversation_id, pod, egress_token_hash, exec_token, status, expires_at, owner, user_key)
           values ($1, $2, $3, $4, $5, 'starting', now() + make_interval(hours => $6), $7, $8) returning *`,
          [id, conv, `sbx-${id.replace(/-/g, "").slice(0, 16)}`, sha256hex(egressToken), newToken(), cfg.maxLifetimeHours, owner, userKey(owner)],
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

  /** `t0`: when the caller started asking (the recorded start latency includes waiting for the pool). */
  async #start(row: SandboxRow, t0 = Date.now()): Promise<LiveSandbox> {
    const egressToken = this.#egressTokens.get(row.id)!;
    this.#egressTokens.delete(row.id);
    try {
      const { name } = await this.provider.create(row, egressToken);
      if (name !== row.pod) {
        await this.#deps.db.query("update sandboxes set pod = $2 where id = $1", [row.id, name]);
        row.pod = name;
      }
      const agent = await this.provider.waitReady(row, Date.now() + START_TIMEOUT_MS);
      const { rows } = await this.#deps.db.query<SandboxRow>(
        "update sandboxes set status = 'ready', last_active_at = now(), agent = $2 where id = $1 and status = 'starting' returning *",
        [row.id, JSON.stringify(agent)],
      );
      if (!rows[0]) throw new Error("the sandbox was released while it started");
      await this.#keepAlive(rows[0], true);
      log("sandbox ready", { sandbox: row.id, conversation: row.conversation_id, pod: row.pod, provider: this.provider.kind });
      void opsEvent(this.#deps.db, "sandbox_start", {
        ok: true,
        ms: Date.now() - t0,
        sandboxId: row.id,
        conversationId: row.conversation_id,
        host: (rows[0] as SandboxRow & { host?: string | null }).host ?? null,
        detail: { provider: this.provider.kind, startMs: Date.now() - new Date(row.created_at).getTime() },
      });
      return this.#live(rows[0])!;
    } catch (e) {
      void opsEvent(this.#deps.db, "sandbox_start", {
        ok: false,
        ms: Date.now() - t0,
        sandboxId: row.id,
        conversationId: row.conversation_id,
        detail: { reason: e instanceof PoolFullError ? "pool full" : "start failed", error: redact((e as Error).message).slice(0, 300), provider: this.provider.kind },
      });
      await this.#forget(row, `start failed: ${(e as Error).message}`);
      throw e;
    }
  }

  async #forget(row: SandboxRow, reason: string): Promise<void> {
    this.#kept.delete(row.id);
    await this.provider.destroy(row.pod).catch((e) => logError("sandbox delete", e, { pod: row.pod }));
    await this.#deps.db.query("update sandboxes set status = 'gone', released_at = now(), release_reason = $2 where id = $1 and status <> 'gone'", [row.id, reason]);
  }

  /** Removes it (nothing to save: /workspace and ~/.claude are on JuiceFS). Safe to call twice. */
  async release(row: SandboxRow, reason: string): Promise<void> {
    const { rows } = await this.#deps.db.query<SandboxRow>(
      "update sandboxes set status = 'releasing' where id = $1 and status in ('ready','starting') returning *",
      [row.id],
    );
    const r = rows[0];
    if (!r) return;
    await this.#forget(r, reason);
    log("sandbox released", { sandbox: r.id, conversation: r.conversation_id, reason });
  }

  async releaseConversation(conv: string, reason: string): Promise<void> {
    const { rows } = await this.#deps.db.query<SandboxRow>("select * from sandboxes where conversation_id = $1 and status <> 'gone'", [conv]);
    for (const r of rows) await this.release(r, reason);
  }
}

export const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));
