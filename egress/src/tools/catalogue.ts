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
// Unicode White_Space and the Cc control characters, as hermit's Rust (`char::is_whitespace`, `char::is_control`)
// sees them; JavaScript's `\s` and `trim()` differ (U+0085 is not `\s`, U+FEFF is)
const WHITE = "\t\n\v\f\r \u0085\u00a0\u1680\u2000-\u200a\u2028\u2029\u202f\u205f\u3000";
const WHITE_OR_CONTROL = new RegExp(`[${WHITE}\u0000-\u001f\u007f-\u009f]`, "u");
const EDGES = new RegExp(`^[${WHITE}]+|[${WHITE}]+$`, "gu");

export interface Staff {
  /** explicit addresses, normalised */
  users: Set<string>;
  /** `@domain` entries, without the `@`, lowercased */
  domains: Set<string>;
  admins: Set<string>;
}

const trim = (s: string): string => s.replace(EDGES, "");

/** hermit_api::people::normalise_email: trimmed and lowercased, at most 254 bytes, no whitespace or control
 * characters, exactly one `@` with something on both sides; null when it is not an address. */
export function normaliseEmail(raw: string): string | null {
  const e = trim(raw).toLowerCase();
  if (!e || Buffer.byteLength(e) > 254 || WHITE_OR_CONTROL.test(e)) return null;
  const parts = e.split("@");
  if (parts.length !== 2 || !parts[0] || !parts[1]) return null;
  return e;
}

/** hermit's hostname_ok for an `@domain` entry (without the `@`). */
function hostnameOk(d: string): boolean {
  return d.length > 0 && d.length <= 253 && /^[A-Za-z0-9.-]+$/.test(d) && !d.startsWith(".") && !d.endsWith(".") && !d.includes("..");
}

/** hermit 3.4.1's validation, rule for rule (aviary crates/hermit/src/catalogue.rs `Loaded::parse`; the shared cases
 * are test/fixtures/catalogue-cases.json); returns the reason a file is refused, or the catalogue. */
export function parseCatalogue(text: string): Staff | { refused: string } {
  if (Buffer.byteLength(text) > MAX_BYTES) return { refused: "the file is over 1 MiB" };
  let raw: unknown;
  try {
    raw = JSON.parse(text);
  } catch {
    return { refused: "not valid JSON" };
  }
  if (!raw || typeof raw !== "object" || Array.isArray(raw)) return { refused: "not a JSON object" };
  const obj = raw as Record<string, unknown>;
  const strings = (key: string): string[] | { refused: string } => {
    const v = obj[key];
    if (v === undefined) return { refused: `${key} is missing` };
    if (!Array.isArray(v)) return { refused: `${key} is not a list` };
    if (v.some((x) => typeof x !== "string")) return { refused: `${key} has an entry that is not a string` };
    return v as string[];
  };
  const users = strings("users");
  if ("refused" in users) return users;
  const admins = strings("admins");
  if ("refused" in admins) return admins;
  if (!users.length) return { refused: "users is empty" };
  if (users.length + admins.length > MAX_ENTRIES) return { refused: `more than ${MAX_ENTRIES} entries` };
  const staff: Staff = { users: new Set(), domains: new Set(), admins: new Set() };
  for (const x of users) {
    const t = trim(x);
    if (t.startsWith("@")) {
      const d = t.slice(1).toLowerCase();
      if (!hostnameOk(d)) return { refused: `users: ${JSON.stringify(x)} is neither an email nor @domain` };
      staff.domains.add(d);
    } else {
      const e = normaliseEmail(t);
      if (!e) return { refused: `users: ${JSON.stringify(x)} is neither an email nor @domain` };
      staff.users.add(e);
    }
  }
  for (const x of admins) {
    const e = normaliseEmail(x);
    if (!e) return { refused: `admins: ${JSON.stringify(x)} is not an email` };
    if (!allowedBy(staff, e)) return { refused: `admins: ${JSON.stringify(x)} is not matched by users` };
    staff.admins.add(e);
  }
  return staff;
}

/** Exact match in `users`, or `@<domain>` for the part after the one `@` (no subdomain matching); the address is
 * normalised first, so anything that is not an address is not allowed. */
export function allowedBy(staff: Staff, email: string): boolean {
  const e = normaliseEmail(email);
  if (!e) return false;
  return staff.users.has(e) || staff.domains.has(e.slice(e.indexOf("@") + 1));
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
    log("catalogue loaded", { users: r.users.size, domains: r.domains.size, admins: r.admins.size, sha256: sha.slice(0, 12) });
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
