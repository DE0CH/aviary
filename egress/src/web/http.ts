// Plain node:http plumbing: a route table with :params, JSON in/out and errors with a status.
import type { IncomingMessage, ServerResponse } from "node:http";

export class HttpError extends Error {
  readonly status: number;
  constructor(status: number, message: string) {
    super(message);
    this.status = status;
  }
}

export const bad = (m: string) => new HttpError(400, m);
export const notFound = (m = "not found") => new HttpError(404, m);

export function sendJson(res: ServerResponse, status: number, body: unknown): void {
  const data = Buffer.from(JSON.stringify(body));
  res.writeHead(status, { "content-type": "application/json; charset=utf-8", "content-length": data.length, "cache-control": "no-store" });
  res.end(data);
}

export function sendHtml(res: ServerResponse, status: number, html: string): void {
  const data = Buffer.from(html);
  res.writeHead(status, { "content-type": "text/html; charset=utf-8", "content-length": data.length, "cache-control": "no-store" });
  res.end(data);
}

export async function readBody(req: IncomingMessage, limit = 1 << 20): Promise<Buffer> {
  const chunks: Buffer[] = [];
  let n = 0;
  for await (const c of req) {
    n += (c as Buffer).length;
    if (n > limit) throw new HttpError(413, "request too large");
    chunks.push(c as Buffer);
  }
  return Buffer.concat(chunks);
}

export async function readJson(req: IncomingMessage): Promise<Record<string, any>> {
  const b = await readBody(req);
  if (!b.length) return {};
  try {
    const v = JSON.parse(b.toString("utf8"));
    if (!v || typeof v !== "object" || Array.isArray(v)) throw new Error();
    return v;
  } catch {
    throw bad("body must be a JSON object");
  }
}

export type Handler<C> = (req: IncomingMessage, res: ServerResponse, params: Record<string, string>, ctx: C) => Promise<void>;

export class Router<C> {
  #routes: { method: string; re: RegExp; keys: string[]; h: Handler<C> }[] = [];
  add(method: string, pattern: string, h: Handler<C>): this {
    const keys: string[] = [];
    const re = new RegExp(
      "^" +
        pattern
          .split("/")
          .map((seg) => {
            if (seg === "*") {
              keys.push("rest");
              return "(.+)";
            }
            if (seg.startsWith(":")) {
              keys.push(seg.slice(1));
              return "([^/]+)";
            }
            return seg.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
          })
          .join("/") +
        "$",
    );
    this.#routes.push({ method, re, keys, h });
    return this;
  }
  match(method: string, path: string): { h: Handler<C>; params: Record<string, string> } | "method" | null {
    let methodMismatch = false;
    for (const r of this.#routes) {
      const m = r.re.exec(path);
      if (!m) continue;
      if (r.method !== method) {
        methodMismatch = true;
        continue;
      }
      const params: Record<string, string> = {};
      r.keys.forEach((k, i) => (params[k] = decodeURIComponent(m[i + 1]!)));
      return { h: r.h, params };
    }
    return methodMismatch ? "method" : null;
  }
}

export const UUID_RE = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i;

export function uuidParam(v: string | undefined): string {
  if (!v || !UUID_RE.test(v)) throw notFound();
  return v.toLowerCase();
}
