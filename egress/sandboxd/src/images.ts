// The sandbox image on a host: unpacked once per digest into /var/lib/sbx/images/<digest>/{rootfs,config.json};
// every sandbox shares it read-only (gVisor's overlay keeps each sandbox's writes). The hub pulls from the registry
// (crane, with the registry login in registryConfigDir); burst hosts fetch the unpacked image from the hub over the
// overlay (their AMI already has the current one).
import { existsSync, mkdirSync, readdirSync, readFileSync, renameSync, rmSync, writeFileSync } from "node:fs";
import { spawn } from "node:child_process";
import type { HostConfig } from "./config.ts";
import { IMAGES } from "./runsc.ts";
import { KeyedLock, log, run } from "./util.ts";

const lock = new KeyedLock();

export function haveImage(digest: string): boolean {
  return existsSync(`${IMAGES}/${digest}/.done`);
}

export function listImages(): string[] {
  try {
    return readdirSync(IMAGES).filter((d) => /^[a-f0-9]{64}$/.test(d) && haveImage(d));
  } catch {
    return [];
  }
}

export function imageEnv(digest: string): string[] {
  const c = JSON.parse(readFileSync(`${IMAGES}/${digest}/config.json`, "utf8"));
  return c.config?.Env ?? [];
}

function pipe(cmdA: string, argsA: string[], cmdB: string, argsB: string[], env: NodeJS.ProcessEnv): Promise<void> {
  return new Promise((resolve, reject) => {
    const a = spawn(cmdA, argsA, { stdio: ["ignore", "pipe", "pipe"], env });
    const b = spawn(cmdB, argsB, { stdio: ["pipe", "ignore", "pipe"] });
    let err = "";
    a.stderr.on("data", (d) => (err += d));
    b.stderr.on("data", (d) => (err += d));
    a.stdout.pipe(b.stdin);
    let n = 0;
    let failed = false;
    const done = (code: number | null) => {
      if (code !== 0) failed = true;
      if (++n === 2) failed ? reject(new Error(`${cmdA} | ${cmdB}: ${err.trim().slice(0, 500)}`)) : resolve();
    };
    a.on("close", done);
    b.on("close", done);
  });
}

/** Makes sure the image is unpacked here; returns its digest. Hub: resolves `ref` with the registry. */
export async function ensureImage(cfg: HostConfig, digest: string | null, ref?: string): Promise<string> {
  const env = { ...process.env, ...(cfg.registryConfigDir ? { DOCKER_CONFIG: cfg.registryConfigDir } : {}) };
  if (!digest) {
    if (!ref || cfg.role !== "hub") throw new Error("image: need a digest (or a ref, on the hub)");
    const r = await run("crane", ["digest", ref], { env, check: true, timeoutMs: 60_000 });
    digest = r.stdout.trim().replace(/^sha256:/, "");
  }
  if (!/^[a-f0-9]{64}$/.test(digest)) throw new Error("image: bad digest");
  const d = digest;
  return lock.with(d, async () => {
    if (haveImage(d)) return d;
    const tmp = `${IMAGES}/.tmp-${d}-${process.pid}`;
    rmSync(tmp, { recursive: true, force: true });
    mkdirSync(`${tmp}/rootfs`, { recursive: true });
    const t0 = Date.now();
    try {
      if (cfg.role === "hub") {
        const src = `${(ref ?? "").replace(/[:@].*$/, "") || "ghcr.io/de0ch/jarvis3-sandbox"}@sha256:${d}`;
        await pipe("crane", ["export", src, "-"], "tar", ["-x", "-C", `${tmp}/rootfs`], env);
        const c = await run("crane", ["config", src], { env, check: true, timeoutMs: 60_000 });
        writeFileSync(`${tmp}/config.json`, c.stdout);
      } else {
        const url = `${cfg.imageSource ?? `http://10.201.0.1:${cfg.imagePort}`}/images/${d}.tar.zst`;
        await pipe("curl", ["-fsS", "--max-time", "1800", url], "tar", ["--zstd", "-x", "-C", tmp], process.env);
      }
      writeFileSync(`${tmp}/.done`, "");
      rmSync(`${IMAGES}/${d}`, { recursive: true, force: true });
      renameSync(tmp, `${IMAGES}/${d}`);
      log("image ready", { digest: d, ms: Date.now() - t0 });
      return d;
    } catch (e) {
      rmSync(tmp, { recursive: true, force: true });
      throw e;
    }
  });
}

/** hub: streams an unpacked image as tar.zst (rootfs/, config.json, .done) */
export function imageTarStream(digest: string) {
  return spawn("tar", ["--zstd", "-C", `${IMAGES}/${digest}`, "--exclude=./rootfs/.gvisor.filestore.*", "-cf", "-", "."], { stdio: ["ignore", "pipe", "ignore"] });
}
