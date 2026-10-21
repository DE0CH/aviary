// Sandboxes on this host: create, delete, list, and both ends of a pre-copy live migration (docs/SANDBOX-VM.md,
// "Migration"). State per sandbox in /var/lib/sbx/sandboxes/<name>.json (mode 600: the spec holds its tokens), so a
// restart of sandboxd finds its sandboxes again (they run on in their own cgroups; sandboxd's unit uses
// KillMode=process).
import { spawn } from "node:child_process";
import { closeSync, existsSync, mkdirSync, openSync, readdirSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import type { HostConfig, SandboxSpec } from "./config.ts";
import { ensureImage, imageEnv } from "./images.ts";
import { addSandboxNet, removeSandboxNet, routeSandbox } from "./net.ts";
import { BUNDLES, CKPT, IMAGES, LOGS, RUNSC_FLAGS, STATE_DIR, runsc, writeBundle } from "./runsc.ts";
import { KeyedLock, log, logError, run, sleep } from "./util.ts";

const SANDBOXES = `${STATE_DIR}/sandboxes`;

export interface Entry {
  spec: SandboxSpec;
  state: "running" | "receiving";
  createdAt: string;
  /** receiving: the port the restore listens on */
  port?: number;
  /** the last migration stream's statistics (target side) */
  migrated?: unknown;
}

export interface Listed {
  name: string;
  ip: string;
  state: Entry["state"];
  createdAt: string;
  cpu: number;
  memory: number;
  /** cores used, averaged over the last ~10 s */
  cpuUsage: number;
  memoryCurrent: number;
}

export class HttpError extends Error {
  readonly status: number;
  constructor(status: number, msg: string) {
    super(msg);
    this.status = status;
  }
}

const PORTS = { min: 7100, max: 7199 };

export class Sandboxes {
  readonly #cfg: HostConfig;
  readonly #lock = new KeyedLock();
  readonly #entries = new Map<string, Entry>();
  readonly #receivers = new Map<string, { child: ReturnType<typeof spawn>; done: Promise<void>; error?: string }>();
  readonly #cpuSamples = new Map<string, { at: number; usec: number; rate: number }>();

  constructor(cfg: HostConfig) {
    this.#cfg = cfg;
  }

  #save(e: Entry): void {
    this.#entries.set(e.spec.name, e);
    writeFileSync(`${SANDBOXES}/${e.spec.name}.json`, JSON.stringify(e), { mode: 0o600 });
  }

  #forget(name: string): void {
    this.#entries.delete(name);
    this.#cpuSamples.delete(name);
    rmSync(`${SANDBOXES}/${name}.json`, { force: true });
  }

  async #status(name: string): Promise<string> {
    const r = await runsc(["state", name], { timeoutMs: 10_000 });
    if (r.code !== 0) return "missing";
    try {
      return JSON.parse(r.stdout).status ?? "unknown";
    } catch {
      return "unknown";
    }
  }

  /** At start: read state files; drop sandboxes that no longer run (e.g. after a reboot). */
  async init(): Promise<void> {
    for (const d of [SANDBOXES, BUNDLES, IMAGES, LOGS, CKPT]) mkdirSync(d, { recursive: true, mode: 0o700 });
    for (const f of readdirSync(SANDBOXES)) {
      if (!f.endsWith(".json")) continue;
      let e: Entry;
      try {
        e = JSON.parse(readFileSync(`${SANDBOXES}/${f}`, "utf8"));
      } catch {
        rmSync(`${SANDBOXES}/${f}`, { force: true });
        continue;
      }
      const st = await this.#status(e.spec.name);
      if (st === "running") {
        if (e.state === "receiving") {
          e.state = "running";
          await routeSandbox(e.spec.name, e.spec.ip);
        }
        this.#entries.set(e.spec.name, e);
      } else {
        log("sandbox gone while sandboxd was down", { sandbox: e.spec.name, status: st });
        await this.#cleanup(e.spec.name, e.spec.ip);
      }
    }
    setInterval(() => this.#sampleCpu(), 10_000).unref();
  }

  #sampleCpu(): void {
    const now = Date.now();
    for (const name of this.#entries.keys()) {
      try {
        const m = /usage_usec (\d+)/.exec(readFileSync(`/sys/fs/cgroup/sbx/${name}/cpu.stat`, "utf8"));
        const usec = Number(m?.[1] ?? 0);
        const prev = this.#cpuSamples.get(name);
        const rate = prev ? (usec - prev.usec) / 1000 / Math.max(1, now - prev.at) : 0;
        this.#cpuSamples.set(name, { at: now, usec, rate });
      } catch {
        // not running here (yet)
      }
    }
  }

  get(name: string): Entry | undefined {
    return this.#entries.get(name);
  }

  list(): Listed[] {
    return [...this.#entries.values()].map((e) => {
      let memoryCurrent = 0;
      try {
        memoryCurrent = Number(readFileSync(`/sys/fs/cgroup/sbx/${e.spec.name}/memory.current`, "utf8").trim());
      } catch {}
      return {
        name: e.spec.name,
        ip: e.spec.ip,
        state: e.state,
        createdAt: e.createdAt,
        cpu: e.spec.cpu,
        memory: e.spec.memory,
        cpuUsage: Math.round((this.#cpuSamples.get(e.spec.name)?.rate ?? 0) * 1000) / 1000,
        memoryCurrent,
      };
    });
  }

  #checkIp(spec: SandboxSpec): void {
    for (const e of this.#entries.values()) {
      if (e.spec.name !== spec.name && e.spec.ip === spec.ip) throw new HttpError(409, `ip ${spec.ip} is used by ${e.spec.name}`);
    }
  }

  async #prepare(spec: SandboxSpec, route: boolean): Promise<string> {
    await ensureImage(this.#cfg, spec.image, spec.imageRef);
    await runsc(["delete", "-force", spec.name]);
    await addSandboxNet(this.#cfg, spec.name, spec.ip, route);
    return writeBundle(spec, imageEnv(spec.image));
  }

  create(spec: SandboxSpec): Promise<Listed> {
    return this.#lock.with(spec.name, async () => {
      const cur = this.#entries.get(spec.name);
      if (cur) {
        if (cur.spec.ip === spec.ip && cur.state === "running") return this.list().find((l) => l.name === spec.name)!;
        throw new HttpError(409, `${spec.name} exists (${cur.state})`);
      }
      this.#checkIp(spec);
      const t0 = Date.now();
      try {
        const bundle = await this.#prepare(spec, true);
        const out = openSync(`${LOGS}/${spec.name}.log`, "a");
        const code = await new Promise<number>((resolve, reject) => {
          // --detach: runsc returns once the sandbox runs; the sandbox keeps the log file, not a pipe of ours
          const p = spawn(process.env.SBX_RUNSC || "runsc", [...RUNSC_FLAGS, "run", "--bundle", bundle, "--detach", spec.name], { stdio: ["ignore", out, out] });
          p.on("error", reject);
          p.on("exit", (c) => resolve(c ?? -1));
        });
        closeSync(out);
        if (code !== 0) throw new Error(`runsc run exited ${code} (see ${LOGS}/${spec.name}.log)`);
        this.#save({ spec, state: "running", createdAt: new Date().toISOString() });
        log("sandbox created", { sandbox: spec.name, ip: spec.ip, ms: Date.now() - t0 });
        return this.list().find((l) => l.name === spec.name)!;
      } catch (e) {
        await this.#cleanup(spec.name, spec.ip);
        throw e;
      }
    });
  }

  async #cleanup(name: string, ip?: string): Promise<void> {
    await runsc(["kill", name, "KILL"], { timeoutMs: 10_000 });
    await runsc(["delete", "-force", name], { timeoutMs: 30_000 });
    await removeSandboxNet(name, ip);
    rmSync(`${BUNDLES}/${name}`, { recursive: true, force: true });
    rmSync(`${CKPT}/in-${name}`, { recursive: true, force: true });
    // the root overlay's upper layer (--overlay2=root:self) is a file in the image's rootfs, named after the sandbox
    try {
      for (const d of readdirSync(IMAGES)) rmSync(`${IMAGES}/${d}/rootfs/.gvisor.filestore.${name}`, { force: true });
    } catch {}
    this.#forget(name);
  }

  delete(name: string): Promise<void> {
    const r = this.#receivers.get(name);
    if (r) r.child.kill("SIGKILL");
    return this.#lock.with(name, async () => {
      const e = this.#entries.get(name);
      await this.#cleanup(name, e?.spec.ip);
      log("sandbox deleted", { sandbox: name });
    });
  }

  #freePort(): number {
    const used = new Set([...this.#entries.values()].map((e) => e.port).filter(Boolean));
    for (let p = PORTS.min; p <= PORTS.max; p++) if (!used.has(p)) return p;
    throw new HttpError(503, "no free migration port");
  }

  /**
   * Target side of a migration: same network (without the /32 route: traffic still goes to the source until the
   * restore is done) and bundle, then `runsc restore --migrate-listen` on the overlay address. The sandbox is created
   * while the stream arrives and restored when it ends; then its /32 points here. Returns the port to send to.
   */
  receive(spec: SandboxSpec): Promise<{ port: number; address: string }> {
    return this.#lock.with(spec.name, async () => {
      if (this.#entries.has(spec.name)) throw new HttpError(409, `${spec.name} is already here`);
      this.#checkIp(spec);
      const port = this.#freePort();
      const bundle = await this.#prepare(spec, false);
      const dir = `${CKPT}/in-${spec.name}`;
      rmSync(dir, { recursive: true, force: true });
      mkdirSync(dir, { recursive: true });
      const e: Entry = { spec, state: "receiving", createdAt: new Date().toISOString(), port };
      this.#save(e);
      const outPath = `${LOGS}/${spec.name}.restore.out`;
      const out = openSync(outPath, "w");
      const err = openSync(`${LOGS}/${spec.name}.log`, "a");
      const child = spawn(
        process.env.SBX_RUNSC || "runsc",
        [...RUNSC_FLAGS, "restore", "--migrate-listen", `${this.#cfg.wgAddress}:${port}`, "--image-path", dir, "--background", "--bundle", bundle, "--detach", spec.name],
        { stdio: ["ignore", out, err] },
      );
      closeSync(out);
      closeSync(err);
      const rec: { child: typeof child; done: Promise<void>; error?: string } = { child, done: Promise.resolve() };
      rec.done = new Promise<void>((resolve) => {
        child.on("exit", (code) => {
          void this.#lock
            .with(spec.name, async () => {
              if (code === 0) {
                await routeSandbox(spec.name, spec.ip);
                let stats: unknown = null;
                try {
                  const line = readFileSync(outPath, "utf8").split("\n").find((l) => l.startsWith('{"from"'));
                  stats = line ? JSON.parse(line) : null;
                } catch {}
                rmSync(dir, { recursive: true, force: true });
                this.#save({ spec, state: "running", createdAt: e.createdAt, migrated: stats });
                log("sandbox received", { sandbox: spec.name, stats });
              } else {
                rec.error = `restore exited ${code}`;
                logError("sandbox receive failed", rec.error, { sandbox: spec.name });
                await this.#cleanup(spec.name, spec.ip);
              }
            })
            .finally(() => {
              setTimeout(() => this.#receivers.delete(spec.name), 60_000).unref();
              resolve();
            });
        });
      });
      this.#receivers.set(spec.name, rec);
      // listening?
      const until = Date.now() + 30_000;
      while (Date.now() < until) {
        const r = await run("ss", ["-Hltn", `sport = :${port}`]);
        if (r.stdout.trim()) return { port, address: this.#cfg.wgAddress };
        if (child.exitCode !== null) break;
        await sleep(50);
      }
      child.kill("SIGKILL");
      throw new Error("the restore did not start listening");
    });
  }

  /** Target: waits (up to timeoutMs) for a receive to finish; running = restored and routed here. */
  async receiveResult(name: string, timeoutMs: number): Promise<{ state: string; error?: string; stats?: unknown }> {
    const r = this.#receivers.get(name);
    if (r) await Promise.race([r.done, sleep(timeoutMs)]);
    const e = this.#entries.get(name);
    if (e?.state === "running") return { state: "running", stats: e.migrated };
    if (e?.state === "receiving") return { state: "receiving" };
    return { state: "failed", error: r?.error ?? "not here" };
  }

  /**
   * Source side: copies the sandbox to `to` (host:port of a receive) while it runs, then stops it for the last dirty
   * pages and its kernel state. Afterwards it is gone from here. If the copy fails before the stop, it keeps running
   * here and this throws 409 (also when it would not converge within maxDowntimeMs); if it fails after, the sandbox
   * is lost (502).
   */
  send(name: string, to: string, downtimeMs = 100, maxRounds = 20, maxDowntimeMs = 0): Promise<{ ms: number }> {
    return this.#lock.with(name, async () => {
      const e = this.#entries.get(name);
      if (!e || e.state !== "running") throw new HttpError(404, `${name} is not running here`);
      const t0 = Date.now();
      const args = ["checkpoint", "--migrate-to", to, "--migrate-downtime", `${downtimeMs}ms`, "--migrate-max-rounds", String(maxRounds)];
      // a migration that won't converge is aborted before the sandbox stops (the fork's --migrate-max-downtime)
      if (maxDowntimeMs > 0) args.push("--migrate-max-downtime", `${maxDowntimeMs}ms`);
      const r = await runsc([...args, name], { timeoutMs: 15 * 60_000 });
      if (r.code !== 0) {
        const st = await this.#status(name);
        logError("sandbox send failed", r.stderr.trim().slice(-400), { sandbox: name, status: st });
        if (st === "running") throw new HttpError(409, `migration failed before the switch; the sandbox still runs here: ${r.stderr.trim().slice(-300)}`);
        await this.#cleanup(name, e.spec.ip);
        throw new HttpError(502, `migration failed after the switch; the sandbox is lost: ${r.stderr.trim().slice(-300)}`);
      }
      await this.#cleanup(name, e.spec.ip);
      log("sandbox sent", { sandbox: name, to, ms: Date.now() - t0 });
      return { ms: Date.now() - t0 };
    });
  }

  /** Every runsc container this host has, also ones without a state file (for cleanup). */
  async runscContainers(): Promise<string[]> {
    const r = await runsc(["list", "-format", "json"]);
    try {
      return (JSON.parse(r.stdout || "[]") as { id: string }[] | null ?? []).map((c) => c.id);
    } catch {
      return [];
    }
  }

  /** Removes runsc containers that have no state file (left by a crash between run and save). */
  async sweep(): Promise<void> {
    for (const id of await this.runscContainers()) {
      if (!this.#entries.has(id) && !this.#receivers.has(id) && /^sbx-/.test(id)) {
        await this.#lock.with(id, async () => {
          if (this.#entries.has(id) || this.#receivers.has(id)) return;
          log("sweeping a container without state", { sandbox: id });
          await this.#cleanup(id);
        });
      }
    }
  }

  static exists(name: string): boolean {
    return existsSync(`${SANDBOXES}/${name}.json`);
  }
}
