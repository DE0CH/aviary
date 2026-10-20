// Outbound fetches the tools server makes for the agent (web_fetch's direct path): only public addresses. The host is
// resolved here, every address is checked, and the connection goes to the address that was checked (no DNS rebinding
// between check and connect). Redirects are followed by hand, each hop checked again.
import { lookup as dnsLookup } from "node:dns/promises";
import { request as httpRequest, type IncomingMessage } from "node:http";
import { request as httpsRequest } from "node:https";
import { isIP } from "node:net";

function v4(a: string): number[] | null {
  const p = a.split(".").map(Number);
  return p.length === 4 && p.every((x) => Number.isInteger(x) && x >= 0 && x <= 255) ? p : null;
}

/** True for loopback, private, link-local, CGNAT, ULA, multicast, documentation and other non-public addresses. */
export function isPrivateAddress(addr: string): boolean {
  let a = addr.toLowerCase().replace(/^\[|\]$/g, "");
  const mapped = /^::ffff:(\d+\.\d+\.\d+\.\d+)$/.exec(a);
  if (mapped) a = mapped[1]!;
  const p = v4(a);
  if (p) {
    const [x, y] = p as [number, number, number, number];
    return (
      x === 0 || x === 10 || x === 127 || x >= 224 ||
      (x === 100 && y >= 64 && y <= 127) ||
      (x === 169 && y === 254) ||
      (x === 172 && y >= 16 && y <= 31) ||
      (x === 192 && y === 168) ||
      (x === 192 && y === 0) ||
      (x === 198 && (y === 18 || y === 19))
    );
  }
  if (isIP(a) !== 6) return true; // not an address at all: refuse
  if (a === "::" || a === "::1") return true;
  const first = parseInt(a.split(":")[0] || "0", 16);
  return (
    (first & 0xfe00) === 0xfc00 || // fc00::/7 unique local
    (first & 0xffc0) === 0xfe80 || // fe80::/10 link-local
    (first & 0xff00) === 0xff00 || // multicast
    a.startsWith("64:ff9b:") || // NAT64
    a.startsWith("2001:db8:") // documentation
  );
}

export class BlockedAddress extends Error {}

export async function publicAddress(host: string): Promise<{ address: string; family: number }> {
  const h = host.replace(/^\[|\]$/g, "");
  const all = isIP(h) ? [{ address: h, family: isIP(h) }] : await dnsLookup(h, { all: true, verbatim: true });
  if (!all.length) throw new BlockedAddress(`${host} does not resolve`);
  const bad = all.find((x) => isPrivateAddress(x.address));
  if (bad) throw new BlockedAddress(`${host} resolves to a non-public address (${bad.address})`);
  return all[0]!;
}

export interface FetchedPage {
  status: number;
  url: string;
  contentType: string;
  body: Buffer;
  truncated: boolean;
}

/**
 * GET a public http(s) URL: at most `maxBytes` of body, `timeoutMs` overall, up to 5 redirects. `resolve` is the
 * address check (tests replace it to reach a local fake).
 */
export async function safeGet(
  url: string,
  o: { maxBytes?: number; timeoutMs?: number; resolve?: (host: string) => Promise<{ address: string; family: number }>; headers?: Record<string, string> } = {},
): Promise<FetchedPage> {
  const maxBytes = o.maxBytes ?? 5_000_000;
  const deadline = Date.now() + (o.timeoutMs ?? 30_000);
  const resolve = o.resolve ?? publicAddress;
  let u = new URL(url);
  for (let hop = 0; hop <= 5; hop++) {
    if (u.protocol !== "http:" && u.protocol !== "https:") throw new BlockedAddress(`only http and https URLs (${u.protocol})`);
    if (u.port && !["80", "443", "8080", "8443"].includes(u.port)) throw new BlockedAddress(`port ${u.port} is not allowed`);
    const { address, family } = await resolve(u.hostname);
    const res = await new Promise<IncomingMessage>((ok, fail) => {
      const req = (u.protocol === "https:" ? httpsRequest : httpRequest)(
        u,
        {
          method: "GET",
          headers: {
            "user-agent": "Mozilla/5.0 (compatible; YYC-AI/1.0)",
            accept: "text/html,application/xhtml+xml,application/json;q=0.9,*/*;q=0.8",
            "accept-language": "zh-HK,zh;q=0.9,en;q=0.8",
            ...o.headers,
          },
          lookup: (_h, _opts, cb) => (cb as (e: null, a: string, f: number) => void)(null, address, family),
          servername: isIP(u.hostname) ? undefined : u.hostname,
          timeout: Math.max(1000, deadline - Date.now()),
        },
        ok,
      );
      req.on("timeout", () => req.destroy(new Error("timed out")));
      req.on("error", fail);
      req.end();
    });
    const status = res.statusCode ?? 0;
    if (status >= 300 && status < 400 && res.headers.location) {
      res.resume();
      u = new URL(res.headers.location, u);
      continue;
    }
    const chunks: Buffer[] = [];
    let size = 0;
    let truncated = false;
    await new Promise<void>((ok, fail) => {
      res.on("data", (c: Buffer) => {
        if (size >= maxBytes) {
          truncated = true;
          res.destroy();
          return;
        }
        chunks.push(c);
        size += c.length;
      });
      res.on("end", ok);
      res.on("close", ok);
      res.on("error", (e) => (truncated ? ok() : fail(e)));
    });
    return { status, url: u.toString(), contentType: String(res.headers["content-type"] || ""), body: Buffer.concat(chunks).subarray(0, maxBytes), truncated };
  }
  throw new Error("too many redirects");
}

/** Readable text of an HTML page: scripts, styles and tags dropped, block elements as line breaks, entities decoded. */
export function htmlToText(html: string): { title: string; text: string } {
  const title = decodeEntities((/<title[^>]*>([\s\S]*?)<\/title>/i.exec(html)?.[1] ?? "").trim());
  let s = html
    .replace(/<(script|style|noscript|svg|template)[^>]*>[\s\S]*?<\/\1>/gi, " ")
    .replace(/<!--[\s\S]*?-->/g, " ")
    .replace(/<(br|\/p|\/div|\/li|\/h[1-6]|\/tr|\/section|\/article|\/blockquote)[^>]*>/gi, "\n")
    .replace(/<li[^>]*>/gi, "\n• ")
    .replace(/<[^>]+>/g, " ");
  s = decodeEntities(s)
    .replace(/[ \t\f\v ]+/g, " ")
    .replace(/ *\n */g, "\n")
    .replace(/\n{3,}/g, "\n\n")
    .trim();
  return { title, text: s };
}

function decodeEntities(s: string): string {
  const named: Record<string, string> = { amp: "&", lt: "<", gt: ">", quot: '"', apos: "'", nbsp: " " };
  return s.replace(/&(#x[0-9a-f]+|#\d+|[a-z]+);/gi, (m, e: string) => {
    if (e[0] === "#") {
      const n = e[1] === "x" || e[1] === "X" ? parseInt(e.slice(2), 16) : parseInt(e.slice(1), 10);
      return Number.isFinite(n) && n > 0 && n < 0x110000 ? String.fromCodePoint(n) : m;
    }
    return named[e.toLowerCase()] ?? m;
  });
}
