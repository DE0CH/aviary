// Small helpers for sandboxd: running host commands, JSON logs, a per-key lock.
import { spawn, type SpawnOptions } from "node:child_process";

export interface RunResult {
  code: number;
  stdout: string;
  stderr: string;
}

/** Runs a command (no shell); never throws on a non-zero exit unless `check`. Input, if given, goes to stdin. */
export function run(cmd: string, args: string[], o: { input?: string; check?: boolean; timeoutMs?: number; env?: NodeJS.ProcessEnv } = {}): Promise<RunResult> {
  return new Promise((resolve, reject) => {
    const opts: SpawnOptions = { stdio: ["pipe", "pipe", "pipe"], env: o.env ?? process.env };
    const p = spawn(cmd, args, opts);
    let stdout = "";
    let stderr = "";
    p.stdout!.on("data", (d) => (stdout += d));
    p.stderr!.on("data", (d) => (stderr += d));
    const t = o.timeoutMs ? setTimeout(() => p.kill("SIGKILL"), o.timeoutMs) : null;
    p.on("error", (e) => {
      if (t) clearTimeout(t);
      reject(e);
    });
    p.on("close", (code) => {
      if (t) clearTimeout(t);
      const r = { code: code ?? -1, stdout, stderr };
      if (o.check && r.code !== 0) reject(new Error(`${cmd} ${args.join(" ")}: exit ${r.code}: ${(stderr || stdout).trim().slice(0, 500)}`));
      else resolve(r);
    });
    p.stdin!.on("error", () => {}); // a command that exits without reading its input (EPIPE)
    p.stdin!.end(o.input ?? "");
  });
}

/** run() that throws on failure and returns stdout. */
export async function sh(cmd: string, ...args: string[]): Promise<string> {
  return (await run(cmd, args, { check: true })).stdout;
}

export function log(msg: string, extra: Record<string, unknown> = {}): void {
  console.log(JSON.stringify({ t: new Date().toISOString(), msg, ...extra }));
}

export function logError(msg: string, e: unknown, extra: Record<string, unknown> = {}): void {
  console.error(JSON.stringify({ t: new Date().toISOString(), msg, error: e instanceof Error ? e.message : String(e), ...extra }));
}

/** Serialises work per key (one operation per sandbox at a time). */
export class KeyedLock {
  readonly #tails = new Map<string, Promise<unknown>>();
  async with<T>(key: string, fn: () => Promise<T>): Promise<T> {
    const prev = this.#tails.get(key) ?? Promise.resolve();
    const next = prev.catch(() => {}).then(fn);
    this.#tails.set(key, next);
    try {
      return await next;
    } finally {
      if (this.#tails.get(key) === next) this.#tails.delete(key);
    }
  }
}

export const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));

/** IPv4 dotted quad <-> number */
export function ipToInt(ip: string): number {
  const p = ip.split(".").map(Number);
  if (p.length !== 4 || p.some((x) => !Number.isInteger(x) || x < 0 || x > 255)) throw new Error(`bad IPv4 address ${ip}`);
  return ((p[0]! << 24) | (p[1]! << 16) | (p[2]! << 8) | p[3]!) >>> 0;
}
export function intToIp(n: number): string {
  return [n >>> 24, (n >>> 16) & 255, (n >>> 8) & 255, n & 255].join(".");
}
export function inCidr(ip: string, cidr: string): boolean {
  const [base, bits] = cidr.split("/");
  const b = Number(bits);
  const mask = b === 0 ? 0 : (~0 << (32 - b)) >>> 0;
  try {
    return ((ipToInt(ip) & mask) >>> 0) === ((ipToInt(base!) & mask) >>> 0);
  } catch {
    return false;
  }
}
