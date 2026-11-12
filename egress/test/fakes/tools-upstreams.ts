// Fakes for the tools role's tests: Exa, TikHub, AnyAPI and an upstream MCP server, all on one local HTTP server (routes by path prefix). Each records what it was asked.
import { createServer, type IncomingMessage, type Server } from "node:http";
import type { AddressInfo } from "node:net";

export interface Seen {
  path: string;
  method: string;
  headers: IncomingMessage["headers"];
  body: any;
}

export interface Fakes {
  url: string;
  seen: Seen[];
  exaHasText: boolean;
  anyapiBroke: boolean; // AnyAPI answers 402 insufficient_balance
  close(): Promise<void>;
}

const body = (req: IncomingMessage) =>
  new Promise<string>((ok) => {
    const c: Buffer[] = [];
    req.on("data", (x: Buffer) => c.push(x));
    req.on("end", () => ok(Buffer.concat(c).toString("utf8")));
  });

export async function startFakes(): Promise<Fakes> {
  const f: Fakes = { url: "", seen: [], exaHasText: true, anyapiBroke: false, close: async () => {} };
  const server: Server = createServer(async (req, res) => {
    const raw = await body(req);
    let parsed: any = raw;
    try {
      parsed = raw ? JSON.parse(raw) : null;
    } catch {}
    const u = new URL(req.url || "/", "http://x");
    f.seen.push({ path: u.pathname + u.search, method: req.method || "", headers: req.headers, body: parsed });
    const json = (status: number, o: unknown, h: Record<string, string> = {}) => {
      res.writeHead(status, { "content-type": "application/json", ...h });
      res.end(JSON.stringify(o));
    };
    // ---- a page that is a file (web_fetch's own reader)
    if (u.pathname === "/report.pdf") {
      res.writeHead(200, { "content-type": "application/pdf" });
      return void res.end("%PDF-1.7 fake");
    }
    // ---- pages not in UTF-8: Big5 named in the header, GBK named only in the page's <meta>
    if (u.pathname === "/big5") {
      res.writeHead(200, { "content-type": "text/html; charset=Big5" });
      return void res.end(Buffer.concat([Buffer.from("<html><title>"), Buffer.from("adbbb4e4b773bb44", "hex"), Buffer.from("</title><body><p>"), Buffer.from("adbbb4e4b773bb44", "hex"), Buffer.from("</p></body></html>")]));
    }
    if (u.pathname === "/gbk") {
      res.writeHead(200, { "content-type": "text/html" });
      return void res.end(Buffer.concat([Buffer.from('<html><head><meta http-equiv="Content-Type" content="text/html; charset=gb2312"></head><body><p>'), Buffer.from("b1b1bea9d0c2cec5", "hex"), Buffer.from("</p></body></html>")]));
    }
    // ---- Exa
    if (u.pathname === "/exa/search") {
      if (req.headers["x-api-key"] !== "exa-key") return json(401, { error: "bad key" });
      return json(200, {
        results: [
          { title: "施政報告 2026", url: "https://www.info.gov.hk/a", publishedDate: "2026-10-15T00:00:00Z", highlights: ["房屋供應"] },
          { title: "Policy address", url: "https://news.example.com/b", highlights: ["housing"] },
        ],
        costDollars: { total: 0.007 },
      });
    }
    if (u.pathname === "/exa/contents") {
      if (!f.exaHasText) return json(200, { results: [], statuses: [{ id: parsed.urls[0], status: "error", error: { httpStatusCode: 403 } }], costDollars: { total: 0.001 } });
      return json(200, { results: [{ title: "Page", url: parsed.urls[0], text: "全文內容 ".repeat(10) }], costDollars: { total: 0.001 } });
    }
    // ---- the instance's own capture (config tools.capture): a known article is its stored copy, a known site a
    // capture, anything else none
    if (u.pathname === "/capture") {
      if (req.headers.authorization !== "Bearer cap-token") return json(403, { error: "forbidden" });
      const target = String(parsed?.url ?? "");
      if (target.includes("known-article")) return json(200, { kind: "cache", url: target, title: "已存文章", text: "存檔全文 ".repeat(10), source: "香港電台", captured_at: "2026-10-27T10:00:00+08:00", dashboard_url: "https://yyc.example/item/7" });
      if (target.includes("known-site")) return json(200, { kind: "capture", url: target, title: "新讀文章", text: "即時擷取 ".repeat(10), source: "明報", captured_at: "2026-10-27T15:00:00+08:00" });
      if (target.includes("capture-down")) return json(502, { error: "down" });
      return json(200, { kind: "none", why: "no source reads this site" });
    }
    // ---- AnyAPI (shapes from its OpenAPI document)
    if (u.pathname.startsWith("/anyapi/")) {
      if (req.headers.authorization !== "Bearer any-key") return json(401, { error: "invalid API key", code: "unauthorized" });
      if (f.anyapiBroke) return json(402, { error: "this API key is paused because your balance ran out; it resumes when you add funds", code: "insufficient_balance" });
      const p = u.pathname.slice("/anyapi".length);
      if (p === "/catalog/search") {
        return json(200, {
          ranking: "semantic",
          total: 2,
          results: [
            { slug: "facebook.profile_posts", name: "Facebook Profile Posts", description: "List a Facebook page's recent posts.", pricing: { from: { model: "flat", unit: "request", maxUsd: 0.0012 }, failoverMaxUsd: 0.0067 } },
            { slug: "facebook.search_posts", name: "Facebook Post Search", description: "Search public posts.", pricing: { from: { model: "flat", unit: "request", maxUsd: 0.03 } } },
          ],
        });
      }
      if (p === "/v1/apis/facebook.profile_posts") {
        return json(200, { id: "facebook.profile_posts", name: "Facebook Profile Posts", inputSchema: { type: "object", properties: { url: { type: "string" } }, required: ["url"] }, lanes: [{ source: "x" }], latency: { p50: 3 } });
      }
      if (p === "/v1/run/facebook.profile_posts" && req.method === "POST") {
        if (!parsed?.url) return json(400, { error: "input.url is required", code: "invalid_input" });
        const items = Array.from({ length: 3 }, (_, i) => ({ text: `帖文 ${i}`, url: `https://facebook.com/p/${i}` }));
        return json(200, { costUsd: 0.0012, resultId: "res_123", items: 3, output: { found: true, data: { posts: u.searchParams.get("max_items") ? items.slice(0, Number(u.searchParams.get("max_items"))) : items } } });
      }
      if (p === "/v1/results/res_123") return json(200, { costUsd: 0.0012, items: 3, output: { found: true, data: { posts: [{ text: "帖文 0" }] } } });
      return json(404, { error: "unknown API", code: "not_found" });
    }
    // ---- TikHub
    if (u.pathname.startsWith("/tikhub/api/v1/")) {
      if (req.headers.authorization !== "Bearer tik-key") return json(401, { code: 401 });
      if (u.pathname.includes("threads/web/search_recent")) return json(400, { code: 400, message: "upstream failed" });
      if (u.pathname.includes("threads/web/search_posts"))
        return json(200, { code: 200, data: { posts: [{ code: "ABC", taken_at: 1792000000, user: { username: "hkuser" }, caption: { text: "Threads 帖子" }, like_count: 12 }] } });
      if (u.pathname.includes("weibo/app/fetch_search_all"))
        return json(200, { code: 200, data: { cards: [{ mblog: { mid: "M1", created_at: "Wed Oct 15 10:00:00 +0800 2026", text: "<b>微博</b> 內容", user: { screen_name: "博主" }, attitudes_count: "1.2万" } }] } });
      return json(200, { code: 200, data: {} });
    }
    // ---- upstream REST API (yyc-mcp's /api/v1 shape)
    if (u.pathname === "/rest-up/openapi.json") {
      const op = (summary: string) => ({ post: { summary, description: summary, requestBody: { content: { "application/json": { schema: { type: "object", properties: { query: { type: "string" } } } } } } } });
      return json(200, { openapi: "3.1.0", paths: { "/api/v1/posts_search": op("Search the stream"), "/api/v1/send_mail": op("Send"), "/api/v1/stream_items": op("New, not listed yet") } });
    }
    if (u.pathname.startsWith("/rest-up/")) {
      const name = u.pathname.slice("/rest-up/".length);
      if (name === "posts_search" && !parsed?.query) return json(400, { error: "query is required" });
      return json(200, { called: name, for: req.headers["x-yyc-user"], query: parsed?.query });
    }
    json(404, { error: "no fake for " + u.pathname });
  });
  await new Promise<void>((r) => server.listen(0, "127.0.0.1", r));
  f.url = `http://127.0.0.1:${(server.address() as AddressInfo).port}`;
  f.close = () => new Promise((r) => server.close(() => r()));
  return f;
}
