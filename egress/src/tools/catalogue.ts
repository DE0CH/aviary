// YYC's staff catalogue as tools reads it (aviary platform spec 17.2 tools item 1, 9.7; the file's rules are hermit
// spec 3.4.1): `/etc/tools/catalogue/catalogue.json` = `{"users": [...], "admins": [...]}`, mounted from ConfigMap
// `catalogue` as a directory (no subPath), so the kubelet updates it in place. Re-read every 30 s; a file whose
// SHA-256 is unchanged is skipped; a file that fails validation is refused and the last good catalogue kept. tools
// is not ready until it has a good copy once.
//
// tools' own access check: a call is served only for a person the catalogue allows. hermit's liveness answer already
// says `live: false` for a person who is not allowed, but tools serves a stale liveness answer while hermit is away,
// so this check keeps a removed person out within the catalogue's own propagation time even then.
import { createHash } from "node:crypto";
import { readFile } from "node:fs/promises";
import { log, logError } from "../common/log.ts";

export const CATALOGUE_PATH = "/etc/tools/catalogue/catalogue.json";
const REREAD_MS = 30_000;
const REFUSED_LOG_MS = 300_000;
const MAX_BYTES = 1 << 20;
const MAX_ENTRIES = 10_000;
const EMAIL = /^[^\s@]{1,64}@[a-z0-9-]+(\.[a-z0-9-]+)+$/;
const DOMAIN = /^@[a-z0-9-]+(\.[a-z0-9-]+)+$/;

export interface Staff {
  users: string[];
  admins: string[];
}

/** hermit 3.4.1's validation; returns the reason a file is refused, or the catalogue. */
export function parseCatalogue(text: string): Staff | { refused: string } {
  if (Buffer.byteLength(text) > MAX_BYTES) return { refused: "the file is over 1 MiB" };
  let raw: unknown;
  try {
    raw = JSON.parse(text);
  } catch {
    return { refused: "not valid JSON" };
  }
  if (!raw || typeof raw !== "object" || Array.isArray(raw)) return { refused: "not a JSON object" };
  const { users, admins = [] } = raw as { users?: unknown; admins?: unknown };
  if (!Array.isArray(users) || !users.length) return { refused: "users must be a non-empty list" };
  if (!Array.isArray(admins)) return { refused: "admins must be a list" };
  if (users.length + admins.length > MAX_ENTRIES) return { refused: "more than 10000 entries" };
  const u: string[] = [];
  for (const x of users) {
    if (typeof x !== "string") return { refused: "users must hold strings" };
    const v = x.trim().toLowerCase();
    if (!EMAIL.test(v) && !DOMAIN.test(v)) return { refused: `users: ${JSON.stringify(x)} is neither an email nor @domain` };
    u.push(v);
  }
  const staff: Staff = { users: u, admins: [] };
  for (const x of admins) {
    if (typeof x !== "string") return { refused: "admins must hold strings" };
    const v = x.trim().toLowerCase();
    if (!EMAIL.test(v)) return { refused: `admins: ${JSON.stringify(x)} is not an email` };
    if (!allowedBy(staff, v)) return { refused: `admins: ${JSON.stringify(x)} is not matched by users` };
    staff.admins.push(v);
  }
  return staff;
}

/** Exact match in `users`, or `@<domain>` with the part after the last `@` (no subdomain matching). */
export function allowedBy(staff: Staff, email: string): boolean {
  const e = email.trim().toLowerCase();
  if (!e || e.startsWith("@")) return false;
  if (staff.users.includes(e)) return true;
  const at = e.lastIndexOf("@");
  return at > 0 && staff.users.includes(e.slice(at));
}

export class Catalogue {
  readonly #path: string;
  #staff: Staff | null = null;
  #sha = "";
  #timer: NodeJS.Timeout | null = null;
  #lastRefusedLog = -Infinity;
  /** files refused since start (for /metrics) */
  refused = 0;

  constructor(path = CATALOGUE_PATH) {
    this.#path = path;
  }

  /** Whether a good catalogue has been read once. */
  get loaded(): boolean {
    return this.#staff !== null;
  }

  allowed(email: string): boolean {
    return this.#staff !== null && allowedBy(this.#staff, email);
  }

  /** Reads the file now. Never throws; true when a good catalogue is in use afterwards. */
  async reload(): Promise<boolean> {
    let text: string;
    try {
      text = await readFile(this.#path, "utf8");
    } catch (e) {
      this.#refuse(`unreadable: ${(e as NodeJS.ErrnoException).code ?? (e as Error).message}`);
      return this.loaded;
    }
    const sha = createHash("sha256").update(text).digest("hex");
    if (sha === this.#sha) return this.loaded;
    const r = parseCatalogue(text);
    if ("refused" in r) {
      this.#refuse(r.refused);
      return this.loaded;
    }
    this.#staff = r;
    this.#sha = sha;
    log("catalogue loaded", { users: r.users.length, admins: r.admins.length, sha256: sha.slice(0, 12) });
    return true;
  }

  #refuse(reason: string): void {
    this.refused++;
    const now = Date.now();
    if (now - this.#lastRefusedLog < REFUSED_LOG_MS) return;
    this.#lastRefusedLog = now;
    logError("catalogue refused", reason, { keep: this.loaded });
  }

  start(everyMs = REREAD_MS): void {
    this.#timer = setInterval(() => void this.reload(), everyMs);
    this.#timer.unref();
  }

  stop(): void {
    if (this.#timer) clearInterval(this.#timer);
    this.#timer = null;
  }
}
