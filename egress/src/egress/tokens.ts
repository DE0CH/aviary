// Sandbox tokens at the gateway (aviary platform spec 3.11.2, hermit spec 6.1-6.3, gateway-api): the proxy credential
// `Basic base64("sbx:" + token)`, the `hst1.` token checked locally against hermit's published Ed25519 keys, and
// hermit's liveness answer per sandbox, cached 30 s and served stale for as long as hermit fails.
//
// Token: `hst1.<kid>.<payload>.<sig>`; payload = base64url of `version 1 (1) | sandbox id (16) | conversation id (16) |
// source IPv4 (4; 0.0.0.0 = any) | expiry (u64 BE Unix seconds) | person (UTF-8, 1-254 bytes)`; sig = base64url of the
// Ed25519 signature over `"hermit-sandbox-token\0" || payload`. The codec is hermit-api's (crates/hermit-api token.rs);
// the conformance suite signs its tokens with that code, so a mismatch here fails CI.
import { createPrivateKey, createPublicKey, hkdfSync, sign as edSign, verify as edVerify, type KeyObject } from "node:crypto";
import { request as httpRequest, type IncomingHttpHeaders } from "node:http";
import { logError, log } from "../common/log.ts";
import { uuidv7 } from "./ids.ts";

export const PROXY_REALM = 'Basic realm="aviary-gateway"';
const PREFIX = "hst1";
const PAYLOAD_VERSION = 1;
const CONTEXT = Buffer.from("hermit-sandbox-token\0", "latin1");
const FIXED_LEN = 1 + 16 + 16 + 4 + 8;
const MAX_TOKEN_LEN = 600;
const B64URL = /^[A-Za-z0-9_-]*$/;

/** What a verified token says. */
export interface Claims {
  sandboxId: string;
  conversationId: string;
  /** dotted IPv4; "0.0.0.0" = any source */
  source: string;
  /** Unix seconds */
  expiresAt: number;
  /** the owner's trimmed, lowercased email */
  person: string;
}

export type VerifyError = "malformed" | "unknown_kid" | "bad_signature" | "expired" | "wrong_source";

/** The token in a `Proxy-Authorization` (or, on self-host paths, `Authorization`) value: "missing" answers 407,
 * "malformed" (Basic, but not `sbx:<token>`) 403. */
export function credentialToken(value: string | string[] | undefined): string | "missing" | "malformed" {
  const v = (Array.isArray(value) ? value[0] : value)?.trim();
  if (!v) return "missing";
  const i = v.indexOf(" ");
  if (i < 0 || v.slice(0, i).toLowerCase() !== "basic") return "missing";
  const b64 = v.slice(i + 1).trim();
  if (!/^[A-Za-z0-9+/]*={0,2}$/.test(b64)) return "malformed";
  const text = Buffer.from(b64, "base64").toString("utf8");
  const j = text.indexOf(":");
  if (j < 0 || text.slice(0, j) !== "sbx" || j === text.length - 1) return "malformed";
  return text.slice(j + 1);
}

/** The credential a request presents: Proxy-Authorization, else (self-host paths only: pip and npm send the index
 * URL's userinfo) Authorization. */
export function presentedToken(h: IncomingHttpHeaders, selfHost: boolean): string | "missing" | "malformed" {
  const p = credentialToken(h["proxy-authorization"]);
  if (p !== "missing" || !selfHost) return p;
  return credentialToken(h.authorization);
}

function b64url(s: string): Buffer | null {
  return B64URL.test(s) ? Buffer.from(s, "base64url") : null;
}

function parseKid(s: string): number | null {
  if (!s || s.length > 10 || !/^[0-9]+$/.test(s) || (s.length > 1 && s.startsWith("0"))) return null;
  const n = Number(s);
  return n <= 0xffff_ffff ? n : null;
}

const uuidOf = (b: Buffer): string => {
  const h = b.toString("hex");
  return `${h.slice(0, 8)}-${h.slice(8, 12)}-${h.slice(12, 16)}-${h.slice(16, 20)}-${h.slice(20)}`;
};

interface Parts {
  kid: number;
  payload: Buffer;
  sig: Buffer;
}

function split(token: string): Parts | null {
  if (token.length > MAX_TOKEN_LEN || !/^[\x20-\x7e]*$/.test(token)) return null;
  const p = token.split(".");
  if (p.length !== 4 || p[0] !== PREFIX) return null;
  const kid = parseKid(p[1]!);
  const payload = b64url(p[2]!);
  const sig = b64url(p[3]!);
  if (kid === null || !payload || !sig || sig.length !== 64) return null;
  return { kid, payload, sig };
}

/** The kid a token names, unverified (to decide whether to refetch the key set); null when malformed. */
export function peekKid(token: string): number | null {
  return split(token)?.kid ?? null;
}

function decodeClaims(b: Buffer): Claims | null {
  if (b.length <= FIXED_LEN || b.length > FIXED_LEN + 254 || b[0] !== PAYLOAD_VERSION) return null;
  let person: string;
  try {
    person = new TextDecoder("utf-8", { fatal: true }).decode(b.subarray(FIXED_LEN));
  } catch {
    return null;
  }
  const exp = b.readBigUInt64BE(37);
  return {
    sandboxId: uuidOf(b.subarray(1, 17)),
    conversationId: uuidOf(b.subarray(17, 33)),
    source: [...b.subarray(33, 37)].join("."),
    expiresAt: exp > BigInt(Number.MAX_SAFE_INTEGER) ? Number.MAX_SAFE_INTEGER : Number(exp),
    person,
  };
}

/** The IPv4 of a socket's remote address (an IPv4-mapped IPv6 address counts as its IPv4), else null. */
export function ipv4Of(addr: string | undefined): string | null {
  const a = (addr ?? "").replace(/^::ffff:/i, "");
  return /^\d{1,3}(\.\d{1,3}){3}$/.test(a) ? a : null;
}

/** hermit's published keys (`GET /v1/sandbox-token-keys`). */
export class KeySet {
  readonly #keys = new Map<number, KeyObject>();

  static from(json: unknown): KeySet {
    const s = new KeySet();
    const keys = (json as { keys?: unknown })?.keys;
    if (!Array.isArray(keys)) throw new Error("token keys: no keys array");
    for (const k of keys as Array<{ kid?: unknown; alg?: unknown; publicKey?: unknown }>) {
      if (k?.alg !== "Ed25519" || typeof k.kid !== "number" || typeof k.publicKey !== "string") continue;
      const raw = b64url(k.publicKey);
      if (!raw || raw.length !== 32) continue;
      s.#keys.set(k.kid, createPublicKey({ key: { kty: "OKP", crv: "Ed25519", x: raw.toString("base64url") }, format: "jwk" }));
    }
    return s;
  }

  get size(): number {
    return this.#keys.size;
  }

  has(kid: number): boolean {
    return this.#keys.has(kid);
  }

  /** 3.11.2 step 2: parses, kid known, signature, not expired (`now < expiresAt`), source address. */
  verify(token: string, nowSec: number, from: string | undefined): { claims: Claims } | { error: VerifyError } {
    const parts = split(token);
    if (!parts) return { error: "malformed" };
    const key = this.#keys.get(parts.kid);
    if (!key) return { error: "unknown_kid" };
    if (!edVerify(null, Buffer.concat([CONTEXT, parts.payload]), key, parts.sig)) return { error: "bad_signature" };
    const claims = decodeClaims(parts.payload);
    if (!claims) return { error: "malformed" };
    if (nowSec >= claims.expiresAt) return { error: "expired" };
    if (claims.source !== "0.0.0.0" && ipv4Of(from) !== claims.source) return { error: "wrong_source" };
    return { claims };
  }
}

/** A master key `kid:base64url(32 bytes)` (hermit's key list) as its Ed25519 signing key: what hermit signs with. For
 * tests and the CI fake hermit only; egress never holds one. */
export function signingKey(entry: string): { kid: number; privateKey: KeyObject; publicKey: string } {
  const [kidText, secretText] = entry.trim().split(":");
  const kid = parseKid(kidText ?? "");
  const secret = b64url(secretText ?? "");
  if (kid === null || !secret || secret.length !== 32) throw new Error("bad master key (want kid:base64url(32 bytes))");
  const seed = Buffer.from(hkdfSync("sha256", secret, Buffer.alloc(0), Buffer.from("hermit sandbox token ed25519"), 32));
  const der = Buffer.concat([Buffer.from("302e020100300506032b657004220420", "hex"), seed]);
  const privateKey = createPrivateKey({ key: der, format: "der", type: "pkcs8" });
  const jwk = createPublicKey(privateKey).export({ format: "jwk" }) as { x: string };
  return { kid, privateKey, publicKey: jwk.x };
}

/** Signs a token for `claims` (tests and the CI fake hermit). */
export function signToken(key: { kid: number; privateKey: KeyObject }, c: Claims): string {
  const person = Buffer.from(c.person, "utf8");
  const b = Buffer.alloc(FIXED_LEN + person.length);
  b[0] = PAYLOAD_VERSION;
  Buffer.from(c.sandboxId.replace(/-/g, ""), "hex").copy(b, 1);
  Buffer.from(c.conversationId.replace(/-/g, ""), "hex").copy(b, 17);
  Buffer.from(c.source.split(".").map(Number)).copy(b, 33);
  b.writeBigUInt64BE(BigInt(c.expiresAt), 37);
  person.copy(b, FIXED_LEN);
  const sig = edSign(null, Buffer.concat([CONTEXT, b]), key.privateKey);
  return `${PREFIX}.${key.kid}.${b.toString("base64url")}.${sig.toString("base64url")}`;
}

// ---- hermit: the key set and liveness ---------------------------------------------------------------------------

/** One GET to hermit with the gateway→hermit pair bearer: [status, body] or null (refused, timed out, reset). */
export type HermitGet = (path: string, timeoutMs: number) => Promise<[number, string] | null>;

export function hermitGetter(base: string, bearer: string): HermitGet {
  const u = new URL(base);
  return (path, timeoutMs) =>
    new Promise((resolve) => {
      let done = false;
      const finish = (v: [number, string] | null) => {
        if (done) return;
        done = true;
        clearTimeout(timer);
        resolve(v);
      };
      const req = httpRequest(
        {
          hostname: u.hostname,
          port: u.port || 80,
          path,
          method: "GET",
          headers: { host: u.host, authorization: `Bearer ${bearer}`, "aviary-protocol": "hermit-core=1", "aviary-request": uuidv7() },
        },
        (res) => {
          const chunks: Buffer[] = [];
          let n = 0;
          res.on("data", (c: Buffer) => {
            n += c.length;
            if (n <= 1 << 20) chunks.push(c);
          });
          res.on("end", () => finish([res.statusCode ?? 0, Buffer.concat(chunks).toString("utf8")]));
          res.on("error", () => finish(null));
        },
      );
      const timer = setTimeout(() => {
        req.destroy();
        finish(null);
      }, timeoutMs);
      req.on("error", () => finish(null));
      req.end();
    });
}

/** Key set refreshed every 5 min and at most every 10 s on an unknown kid; the last set is kept on a failed fetch, and
 * a fetched set with no usable key counts as a failed fetch (a hermit problem never turns a valid token into a 403).
 * A token naming a kid the set lacks waits for at most one refetch: one at once when none began in the last 10 s (or
 * joining the one running), else none: the 403 is at once. A hermit key rotation can land a refetch on a replica that
 * does not publish the new kid yet; the client's retry after the 10 s mark refetches again. The unknown-kid clock is its own: the 5-minute refresh never delays a new kid's refetch. */
const KEYS_REFRESH_MS = 300_000;
const KEYS_UNKNOWN_KID_MIN_MS = 10_000;
const KEYS_TIMEOUT_MS = 2000;
/** Liveness: 1 s timeout, answers cached 30 s; a failed lookup is retried after 2 s; an entry unused 10 min is dropped. */
const LIVENESS_TIMEOUT_MS = 1000;
const LIVENESS_TTL_MS = 30_000;
const LIVENESS_RETRY_MS = 2000;
const LIVENESS_FORGET_MS = 600_000;

interface LiveEntry {
  /** the last definitive answer (200's `live`, or false for a 404) and when it came */
  live: boolean | null;
  at: number;
  refreshing: boolean;
  retryAt: number;
  used: number;
}

export type Verified = { claims: Claims } | { error: VerifyError | "not_live" };

/** Where the last key set hermit published is kept (egress's database, table token_keys), so a restart while hermit
 * is away can still verify tokens and become ready. The keys are public; nothing secret is stored. */
export interface KeyStore {
  load(): Promise<string | null>;
  save(body: string): Promise<void>;
}

export interface TokensOptions {
  now?: () => number;
  store?: KeyStore;
}

/** The gateway's view of hermit: verification of tokens and the revocation view (3.11.2 steps 2-3). */
export class SandboxTokens {
  readonly #get: HermitGet;
  #keys = new KeySet();
  #loaded = false;
  /** the key set in use came from the store, not (yet) from hermit */
  #fromStore = false;
  #savedBody: string | null = null;
  /** when the last unknown-kid refetch began (ms; its own clock, see KEYS_UNKNOWN_KID_MIN_MS) */
  #lastUnknownKid = -Infinity;
  /** the unknown-kid refetch that is waiting for its turn or running (shared by every request that needs one) */
  #kidFetch: Promise<void> | null = null;
  #fetching: Promise<boolean> | null = null;
  readonly #store: KeyStore | undefined;
  readonly #live = new Map<string, LiveEntry>();
  #timers: NodeJS.Timeout[] = [];
  /** liveness lookups that got no answer (counted for /metrics) */
  lookupFailures = 0;
  #now: () => number;

  constructor(get: HermitGet, o: TokensOptions | (() => number) = {}) {
    const opts = typeof o === "function" ? { now: o } : o;
    this.#get = get;
    this.#now = opts.now ?? Date.now;
    this.#store = opts.store;
  }

  /** Whether a key set has been fetched from hermit once. */
  get loaded(): boolean {
    return this.#loaded;
  }

  /** Whether tokens can be verified: a key set from hermit, or the stored one (readiness). */
  get usable(): boolean {
    return this.#loaded || this.#fromStore;
  }

  /** Before hermit has answered once: the key set from the store, if it has one. Never throws. */
  async loadStored(): Promise<boolean> {
    if (this.usable || !this.#store) return this.usable;
    try {
      const body = await this.#store.load();
      if (!body || this.#loaded) return this.usable;
      const keys = KeySet.from(JSON.parse(body));
      if (!keys.size) return false;
      this.#keys = keys;
      this.#savedBody = body;
      this.#fromStore = true;
      log("token keys from the database (hermit not reached yet)", { keys: keys.size });
      return true;
    } catch (e) {
      logError("token keys load", e);
      return false;
    }
  }

  /** Fetches the key set now and starts the 5-minute refresh and the liveness sweeper. */
  start(): Promise<boolean> {
    this.#timers.push(setInterval(() => void this.refreshKeys(false), KEYS_REFRESH_MS));
    this.#timers.push(setInterval(() => this.#sweep(), 1000));
    for (const t of this.#timers) t.unref();
    return this.refreshKeys(false);
  }

  stop(): void {
    for (const t of this.#timers) clearInterval(t);
    this.#timers = [];
  }

  /** Fetches the key set now (single flight). Never throws. Unknown kids go through #refetchForKid. */
  refreshKeys(_unknownKid = false): Promise<boolean> {
    if (this.#fetching) return this.#fetching;
    this.#fetching = (async () => {
      const r = await this.#get("/v1/sandbox-token-keys", KEYS_TIMEOUT_MS);
      if (!r || r[0] !== 200) {
        logError("token keys fetch", r ? `status ${r[0]}` : "no answer", { keep: this.#keys.size });
        return false;
      }
      try {
        const keys = KeySet.from(JSON.parse(r[1]));
        if (!keys.size) {
          logError("token keys fetch", "hermit published no usable key", { keep: this.#keys.size });
          return false;
        }
        this.#keys = keys;
        if (!this.#loaded) log("token keys loaded", { keys: this.#keys.size });
        this.#loaded = true;
        this.#fromStore = false;
        if (this.#store && r[1] !== this.#savedBody) {
          const body = r[1];
          void this.#store.save(body).then(
            () => (this.#savedBody = body),
            (e) => logError("token keys save", e),
          );
        }
        return true;
      } catch (e) {
        logError("token keys fetch", e);
        return false;
      }
    })().finally(() => (this.#fetching = null));
    return this.#fetching;
  }

  /** Steps 2 and 3: the token verified locally (an unknown kid waits for at most one refetch), then the cached
   * liveness view. A hermit problem never refuses a valid token: with no answer yet it is admitted and the lookup runs
   * in the background. */
  async verify(token: string, from: string | undefined): Promise<Verified> {
    let r = this.#keys.verify(token, Math.floor(this.#now() / 1000), from);
    const kid = "error" in r && r.error === "unknown_kid" ? peekKid(token) : null;
    if (kid !== null) {
      const fetch = this.#refetchForKid();
      if (fetch) await fetch;
      r = this.#keys.verify(token, Math.floor(this.#now() / 1000), from);
    }
    if ("error" in r) return r;
    return this.admits(r.claims.sandboxId) ? r : { error: "not_live" };
  }

  /** The unknown-kid refetch a request may wait for: the running one, or a new one when none began in the last 10 s;
   * otherwise null and the request's 403 is at once (its retry after the 10 s mark refetches). */
  #refetchForKid(): Promise<void> | null {
    if (this.#kidFetch) return this.#kidFetch;
    if (this.#now() - this.#lastUnknownKid < KEYS_UNKNOWN_KID_MIN_MS) return null;
    this.#lastUnknownKid = this.#now();
    this.#kidFetch = (async () => {
      // a fetch already running began before this request needed one: wait for it, then fetch afresh
      if (this.#fetching) await this.#fetching;
      await this.refreshKeys(true);
    })().finally(() => (this.#kidFetch = null));
    return this.#kidFetch;
  }

  /** Whether the liveness view admits a sandbox whose token verified, starting a refresh when one is due. */
  admits(sandboxId: string): boolean {
    const now = this.#now();
    let e = this.#live.get(sandboxId);
    if (!e) {
      e = { live: null, at: 0, refreshing: false, retryAt: 0, used: now };
      this.#live.set(sandboxId, e);
    }
    e.used = now;
    this.#maybeRefresh(sandboxId, e, now);
    return e.live !== false;
  }

  #maybeRefresh(id: string, e: LiveEntry, now: number): void {
    const due = e.live === null || now - e.at >= LIVENESS_TTL_MS;
    if (!due || e.refreshing || now < e.retryAt) return;
    e.refreshing = true;
    void this.#lookup(id).then((v) => {
      e.refreshing = false;
      if (v === null) {
        this.lookupFailures++;
        e.retryAt = this.#now() + LIVENESS_RETRY_MS;
        return;
      }
      e.live = v;
      e.at = this.#now();
      e.retryAt = 0;
    });
  }

  /** true / false (200 `live`, or 404), null when hermit gave no answer. */
  async #lookup(id: string): Promise<boolean | null> {
    const r = await this.#get(`/v1/sandbox-tokens/${encodeURIComponent(id)}`, LIVENESS_TIMEOUT_MS);
    if (!r) return null;
    if (r[0] === 404) return false;
    if (r[0] !== 200) return null;
    try {
      const v = JSON.parse(r[1]) as { live?: unknown };
      return typeof v.live === "boolean" ? v.live : null;
    } catch {
      return null;
    }
  }

  #sweep(): void {
    const now = this.#now();
    for (const [id, e] of this.#live) {
      if (now - e.used >= LIVENESS_FORGET_MS) this.#live.delete(id);
      else this.#maybeRefresh(id, e, now);
    }
  }
}
