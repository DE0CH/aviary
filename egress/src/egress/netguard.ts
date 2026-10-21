// Where a sandbox may connect (the host allowlist is allowlist.ts): refusal of every address that is not the public
// internet (private, loopback, link-local, CGNAT, ULA, multicast, cluster ranges, extra CIDRs such as the node's own
// /64). The check runs on the RESOLVED address and the connection goes to exactly that address, so a DNS answer that
// changes between check and connect (rebinding) can't reach the cluster.
import { lookup } from "node:dns/promises";
import { isIP } from "node:net";

/** Lower-case, no trailing dot, no brackets. */
export function normHost(h: string): string {
  return h.trim().toLowerCase().replace(/^\[|\]$/g, "").replace(/\.$/, "");
}

type Cidr = { v: 4 | 6; bits: bigint; prefix: number };

function v4ToBig(ip: string): bigint {
  return ip.split(".").reduce((a, o) => (a << 8n) | BigInt(Number(o)), 0n);
}

function v6ToBig(ip: string): bigint {
  let s = ip.toLowerCase();
  const zone = s.indexOf("%");
  if (zone >= 0) s = s.slice(0, zone);
  // a trailing dotted IPv4 (::ffff:1.2.3.4)
  const m = /^(.*:)(\d+\.\d+\.\d+\.\d+)$/.exec(s);
  if (m) {
    const v4 = v4ToBig(m[2]!);
    s = `${m[1]}${(v4 >> 16n).toString(16)}:${(v4 & 0xffffn).toString(16)}`;
  }
  const [head, tail] = s.split("::") as [string, string | undefined];
  const hs = head ? head.split(":") : [];
  const ts = tail !== undefined && tail ? tail.split(":") : [];
  const fill = tail !== undefined ? 8 - hs.length - ts.length : 0;
  const groups = [...hs, ...Array(fill).fill("0"), ...ts];
  if (groups.length !== 8) throw new Error(`bad IPv6 ${ip}`);
  return groups.reduce((a, g) => (a << 16n) | BigInt(parseInt(g || "0", 16)), 0n);
}

export function parseCidr(c: string): Cidr {
  const [addr, p] = c.trim().split("/") as [string, string | undefined];
  const v = isIP(addr);
  if (!v) throw new Error(`bad CIDR ${c}`);
  const prefix = p === undefined ? (v === 4 ? 32 : 128) : Number(p);
  return { v: v as 4 | 6, bits: v === 4 ? v4ToBig(addr) : v6ToBig(addr), prefix };
}

function inCidr(v: 4 | 6, bits: bigint, c: Cidr): boolean {
  if (c.v !== v) return false;
  const width = v === 4 ? 32 : 128;
  const shift = BigInt(width - c.prefix);
  return bits >> shift === c.bits >> shift;
}

const BLOCKED = [
  "0.0.0.0/8", "10.0.0.0/8", "100.64.0.0/10", "127.0.0.0/8", "169.254.0.0/16", "172.16.0.0/12", "192.0.0.0/24",
  "192.0.2.0/24", "192.168.0.0/16", "198.18.0.0/15", "198.51.100.0/24", "203.0.113.0/24", "224.0.0.0/4", "240.0.0.0/4",
  "::/128", "::1/128", "64:ff9b::/96", "64:ff9b:1::/48", "100::/64", "2001:db8::/32", "fc00::/7", "fe80::/10", "fec0::/10",
  "ff00::/8",
].map(parseCidr);

export class AddressGuard {
  readonly #blocked: Cidr[];
  readonly #permitted: Cidr[];
  /** extra: more blocked CIDRs (EGRESS_BLOCK_CIDRS); permit: exceptions, for tests only. */
  constructor(extra: string[] = [], permit: string[] = []) {
    this.#blocked = [...BLOCKED, ...extra.filter(Boolean).map(parseCidr)];
    this.#permitted = permit.filter(Boolean).map(parseCidr);
  }
  blocked(ip: string): boolean {
    const v = isIP(ip);
    if (!v) return true;
    let bits = v === 4 ? v4ToBig(ip) : v6ToBig(ip);
    let ver = v as 4 | 6;
    // IPv4-mapped (::ffff:a.b.c.d) and IPv4-compatible addresses are judged as the IPv4 address
    if (ver === 6 && (bits >> 32n === 0xffffn || (bits >> 32n === 0n && bits > 1n))) {
      bits &= 0xffffffffn;
      ver = 4;
    }
    if (this.#permitted.some((c) => inCidr(ver, bits, c))) return false;
    return this.#blocked.some((c) => inCidr(ver, bits, c));
  }
  /** The public addresses of `host` (in resolver order); throws when there are none. */
  async resolve(host: string, resolver: (h: string) => Promise<string[]> = defaultResolve): Promise<string[]> {
    const h = normHost(host);
    const addrs = isIP(h) ? [h] : await resolver(h);
    const ok = addrs.filter((a) => !this.blocked(a));
    if (!ok.length) throw new GuardError(addrs.length ? `${h} resolves only to non-public addresses` : `${h} does not resolve`);
    return ok;
  }
}

export class GuardError extends Error {}

async function defaultResolve(h: string): Promise<string[]> {
  const r = await lookup(h, { all: true, verbatim: true });
  return r.map((x) => x.address);
}
