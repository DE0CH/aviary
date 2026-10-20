// The package cache on the pod's disk (JARVIS_CACHE_DIR): one body file plus one meta file per URL (named by the
// URL's sha256), least recently used entries evicted once the total passes the limit (egress.cacheGiB). Writes go
// to a temp file and are renamed into place only when complete, so a reader never sees half a file.
import { createWriteStream, mkdirSync, readdirSync, readFileSync, renameSync, rmSync, statSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { randomBytes } from "node:crypto";
import { sha256hex } from "../common/tokens.ts";

export interface CacheMeta {
  url: string;
  status: number;
  contentType: string;
  storedAt: number;
}

export interface CacheHit {
  meta: CacheMeta;
  path: string;
  size: number;
}

export class DiskCache {
  readonly #dir: string;
  readonly #limit: number;
  /** key -> { size, last use }; Map order is not trusted, eviction sorts by last use */
  readonly #entries = new Map<string, { size: number; used: number }>();
  #total = 0;
  hits = 0;
  misses = 0;

  constructor(dir: string, limitBytes: number) {
    this.#dir = dir;
    this.#limit = limitBytes;
    mkdirSync(join(dir, "tmp"), { recursive: true });
    rmSync(join(dir, "tmp"), { recursive: true, force: true });
    mkdirSync(join(dir, "tmp"), { recursive: true });
    for (const f of readdirSync(dir)) {
      if (!f.endsWith(".body")) continue;
      const key = f.slice(0, -5);
      try {
        const st = statSync(join(dir, f));
        statSync(join(dir, `${key}.meta`));
        this.#entries.set(key, { size: st.size, used: st.atimeMs });
        this.#total += st.size;
      } catch {
        rmSync(join(dir, f), { force: true });
      }
    }
  }

  get totalBytes(): number {
    return this.#total;
  }

  static key(url: string): string {
    return sha256hex(url);
  }

  get(url: string, maxAgeMs = Infinity): CacheHit | null {
    const key = DiskCache.key(url);
    const e = this.#entries.get(key);
    if (!e) {
      this.misses++;
      return null;
    }
    try {
      const meta = JSON.parse(readFileSync(join(this.#dir, `${key}.meta`), "utf8")) as CacheMeta;
      if (Date.now() - meta.storedAt > maxAgeMs) {
        this.misses++;
        return null;
      }
      e.used = Date.now();
      this.hits++;
      return { meta, path: join(this.#dir, `${key}.body`), size: e.size };
    } catch {
      this.#drop(key);
      this.misses++;
      return null;
    }
  }

  putBuffer(meta: CacheMeta, body: Buffer): void {
    const w = this.writer(meta);
    w.write(body);
    void w.end();
  }

  /** A streaming writer; end() commits, abort() throws the partial file away. */
  writer(meta: CacheMeta): { write(chunk: Buffer): void; end(): Promise<void>; abort(): void } {
    const key = DiskCache.key(meta.url);
    const tmp = join(this.#dir, "tmp", `${key}.${randomBytes(4).toString("hex")}`);
    const ws = createWriteStream(tmp);
    let size = 0;
    let dead = false;
    ws.on("error", () => (dead = true));
    return {
      write: (chunk) => {
        if (dead) return;
        size += chunk.length;
        if (size > this.#limit / 4) {
          dead = true; // one entry may not take over the cache
          ws.destroy();
          return;
        }
        ws.write(chunk);
      },
      end: () =>
        new Promise<void>((resolve) => {
          if (dead) {
            rmSync(tmp, { force: true });
            return resolve();
          }
          ws.end(() => {
            try {
              writeFileSync(join(this.#dir, `${key}.meta`), JSON.stringify(meta));
              renameSync(tmp, join(this.#dir, `${key}.body`));
              const old = this.#entries.get(key);
              if (old) this.#total -= old.size;
              this.#entries.set(key, { size, used: Date.now() });
              this.#total += size;
              this.#evict();
            } catch {
              rmSync(tmp, { force: true });
            }
            resolve();
          });
        }),
      abort: () => {
        dead = true;
        ws.destroy();
        rmSync(tmp, { force: true });
      },
    };
  }

  #evict(): void {
    if (this.#total <= this.#limit) return;
    const byUse = [...this.#entries.entries()].sort((a, b) => a[1].used - b[1].used);
    for (const [key] of byUse) {
      if (this.#total <= this.#limit * 0.9) break;
      this.#drop(key);
    }
  }

  #drop(key: string): void {
    const e = this.#entries.get(key);
    if (e) this.#total -= e.size;
    this.#entries.delete(key);
    rmSync(join(this.#dir, `${key}.body`), { force: true });
    rmSync(join(this.#dir, `${key}.meta`), { force: true });
  }
}
