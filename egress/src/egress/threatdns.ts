// Threat-intel DNS for every sandbox connection (docs/CHAT.md, "The sandbox allowlist"): names are resolved through
// malware-blocking resolvers, Cloudflare for Families "security" (1.1.1.2, 1.0.0.2: a blocked name answers 0.0.0.0 /
// ::) with Quad9 (9.9.9.9: a blocked name answers NXDOMAIN) when Cloudflare does not answer. A blocked or nonexistent
// name is refused; a resolver failure is refused too (fail closed). Answers are cached for their TTL (30 s to 5 min).
import { Resolver } from "node:dns/promises";
import { isIP } from "node:net";
import { normHost } from "./netguard.ts";

export const THREAT_DNS_SERVERS = ["1.1.1.2", "1.0.0.2", "9.9.9.9"];

/** Why a name was refused: blocked as a threat (0.0.0.0/::), no such name (NXDOMAIN), or no resolver answered. */
export class DnsRefusal extends Error {
  readonly reason: "deny:threat" | "deny:nxdomain" | "deny:dns";
  constructor(reason: DnsRefusal["reason"], message: string) {
    super(message);
    this.reason = reason;
  }
}

type Answer = { addrs: string[] } | { refusal: DnsRefusal };

export class ThreatResolver {
  readonly #r: Resolver;
  readonly #cache = new Map<string, { until: number; a: Answer }>();
  constructor(servers: string[] = THREAT_DNS_SERVERS, timeoutMs = 2000) {
    this.#r = new Resolver({ timeout: timeoutMs, tries: 2 });
    this.#r.setServers(servers);
  }

  /** The addresses of `host` (IPv4 first); throws DnsRefusal. An IP literal is returned as is. */
  async resolve(host: string): Promise<string[]> {
    const h = normHost(host);
    if (isIP(h)) return [h];
    const hit = this.#cache.get(h);
    let a: Answer;
    if (hit && hit.until > Date.now()) a = hit.a;
    else {
      const r = await this.#lookup(h);
      a = r.a;
      if (this.#cache.size > 10_000) this.#cache.clear();
      this.#cache.set(h, { until: Date.now() + Math.min(300, Math.max(30, r.ttl)) * 1000, a });
    }
    if ("refusal" in a) throw a.refusal;
    return a.addrs;
  }

  async #lookup(h: string): Promise<{ a: Answer; ttl: number }> {
    const [v4, v6] = await Promise.all([this.#r.resolve4(h, { ttl: true }).then(ok, fail), this.#r.resolve6(h, { ttl: true }).then(ok, fail)]);
    const recs = [...(v4.recs ?? []), ...(v6.recs ?? [])];
    const ttl = recs.length ? Math.min(...recs.map((x) => x.ttl)) : 60;
    if (recs.some((x) => x.address === "0.0.0.0" || x.address === "::"))
      return { a: { refusal: new DnsRefusal("deny:threat", `${h} is blocked as a security threat (malware or phishing)`) }, ttl };
    if (recs.length) return { a: { addrs: recs.map((x) => x.address) }, ttl };
    const codes = [v4.code, v6.code];
    if (codes.includes("ENOTFOUND")) return { a: { refusal: new DnsRefusal("deny:nxdomain", `${h} does not exist (or is blocked as a security threat)`) }, ttl: 60 };
    if (codes.every((c) => c === "ENODATA")) return { a: { refusal: new DnsRefusal("deny:nxdomain", `${h} has no address`) }, ttl: 60 };
    return { a: { refusal: new DnsRefusal("deny:dns", `could not resolve ${h} (${codes.filter(Boolean).join(", ")})`) }, ttl: 5 };
  }
}

type Got = { recs?: { address: string; ttl: number }[]; code?: string };
const ok = (recs: { address: string; ttl: number }[]): Got => ({ recs });
const fail = (e: unknown): Got => ({ code: String((e as { code?: string }).code ?? "ERROR") });
