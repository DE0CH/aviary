// The tools server's own tools (docs/CHAT.md, "tools"): all read-only (search and read; nothing that sends, posts or
// changes anything outside). Descriptions are what the model reads: written for a Hong Kong news / PR assistant,
// saying when to use each and what comes back.
import { BlockedAddress, htmlToText, safeGet } from "./netguard.ts";
import { exaContents, exaSearch, PLATFORMS, tikhubSearch, wisersSearch } from "./providers.ts";
import { int, str, text, ToolInputError, type Deps, type ToolDef } from "./types.ts";

const DATE = { type: "string", pattern: "^\\d{4}-\\d{2}-\\d{2}$" };
const isDate = (s: string) => !s || /^\d{4}-\d{2}-\d{2}$/.test(s);

function domains(v: unknown): string[] | undefined {
  if (v === undefined || v === null || v === "") return undefined;
  const xs = Array.isArray(v) ? v : String(v).split(",");
  const out = xs.map((x) => String(x).trim().toLowerCase().replace(/^https?:\/\//, "").replace(/\/.*$/, "")).filter(Boolean);
  return out.length ? out.slice(0, 20) : undefined;
}

export const webSearch: ToolDef = {
  name: "web_search",
  title: "Search the web",
  description:
    "Search the open web (news sites, government pages, company sites, blogs) and get titles, links, dates and short " +
    "excerpts. Use it for anything current or factual you are not sure of, and to find coverage of a person, company or " +
    "event. Works in Traditional/Simplified Chinese and English; for Hong Kong topics search in Chinese too (e.g. 「李家超 施政報告」). " +
    "Narrow with `since`/`until` (YYYY-MM-DD) for recent news and `include_domains` (e.g. info.gov.hk, rthk.hk, " +
    "hk01.com, mingpao.com). Follow up with web_fetch to read a result in full. For Hong Kong print/newspaper archives " +
    "use news_search (Wisers); for 小紅書/抖音/微博/X/Threads posts use social_search.",
  inputSchema: {
    type: "object",
    properties: {
      query: { type: "string", description: "What to search for, in natural language or keywords." },
      num_results: { type: "integer", minimum: 1, maximum: 25, default: 8 },
      since: { ...DATE, description: "Only pages published on/after this date (YYYY-MM-DD)." },
      until: { ...DATE, description: "Only pages published on/before this date (YYYY-MM-DD)." },
      include_domains: { type: "array", items: { type: "string" }, description: "Only these sites (domains)." },
      exclude_domains: { type: "array", items: { type: "string" }, description: "Leave these sites out." },
      category: { type: "string", enum: ["news", "company", "research paper", "pdf", "personal site", "financial report"], description: "Restrict to one kind of page." },
    },
    required: ["query"],
    additionalProperties: false,
  },
  annotations: { title: "Search the web", readOnlyHint: true, openWorldHint: true },
  async run(args, ctx) {
    const since = str(args, "since");
    const until = str(args, "until");
    if (!isDate(since) || !isDate(until)) throw new ToolInputError("dates are YYYY-MM-DD");
    const category = str(args, "category");
    const r = await exaSearch(ctx.deps, {
      query: str(args, "query", true),
      numResults: int(args, "num_results", 8, 1, 25),
      ...(since ? { since } : {}),
      ...(until ? { until } : {}),
      ...(domains(args.include_domains) ? { includeDomains: domains(args.include_domains)! } : {}),
      ...(domains(args.exclude_domains) ? { excludeDomains: domains(args.exclude_domains)! } : {}),
      ...(category ? { category } : {}),
    });
    if (!r.hits.length) return { ...text("No results. Try other words (or Chinese/English), or a wider date range."), ...(r.costUsd !== undefined ? { costUsd: r.costUsd } : {}) };
    const lines = r.hits.map((h, i) =>
      [`${i + 1}. ${h.title || "(no title)"}`, `   ${h.url}`, h.publishedDate ? `   Published: ${h.publishedDate.slice(0, 10)}` : "", h.author ? `   By: ${h.author}` : "", h.snippet ? `   ${h.snippet}` : ""].filter(Boolean).join("\n"),
    );
    return { content: [{ type: "text", text: lines.join("\n\n") }], structuredContent: { results: r.hits }, ...(r.costUsd !== undefined ? { costUsd: r.costUsd } : {}) };
  },
};

interface OwnCapture {
  kind: string;
  url?: string;
  title?: string;
  text?: string;
  author?: string;
  source?: string;
  published?: string;
  captured_at?: string;
  excerpt?: boolean;
  dashboard_url?: string;
}

/** The instance's own capture of a page (config tools.capture): its stored copy, a capture now, or kind "none". */
async function ownCapture(deps: Deps, cfg: { url: string; tokenEnv: string }, url: string): Promise<OwnCapture> {
  const token = deps.env[cfg.tokenEnv];
  if (!token) throw new Error(`${cfg.tokenEnv} is not set`);
  const r = await deps.fetch(cfg.url, {
    method: "POST",
    headers: { authorization: `Bearer ${token}`, "content-type": "application/json" },
    body: JSON.stringify({ url }),
    signal: AbortSignal.timeout(90_000),
  });
  if (!r.ok) throw new Error(`capture answered HTTP ${r.status}`);
  return (await r.json()) as OwnCapture;
}

/** web_fetch's answer from the instance's own copy or capture: labelled as such on its first line and in structuredContent. */
function ownCaptureResult(c: OwnCapture, name: string, url: string, cut: (s: string) => string) {
  const when = String(c.captured_at ?? "").slice(0, 16).replace("T", " ");
  const how = c.kind === "cache" ? `${name}'s stored copy (captured ${when})` : `${name}'s capture, read just now`;
  const head = [
    `Source: ${how}. Ask with original: true for the page as it is.`,
    c.title ? `Title: ${c.title}` : "",
    `URL: ${c.url ?? url}`,
    c.source ? `Outlet: ${c.source}` : "",
    c.author ? `By: ${c.author}` : "",
    c.published ? `Published: ${String(c.published).slice(0, 10)}` : "",
    c.excerpt ? "Only the page's free excerpt (paywalled)." : "",
    c.dashboard_url ? `${name}'s page for it: ${c.dashboard_url}` : "",
  ].filter(Boolean).join("\n");
  return {
    content: [{ type: "text" as const, text: `${head}\n\n${cut(String(c.text ?? ""))}` }],
    structuredContent: {
      via: c.kind === "cache" ? "own-cache" : "own-capture",
      by: name,
      url: c.url ?? url,
      ...(c.dashboard_url ? { dashboard_url: c.dashboard_url } : {}),
      ...(c.captured_at ? { captured_at: c.captured_at } : {}),
    },
  };
}

export const webFetch: ToolDef = {
  name: "web_fetch",
  title: "Read a web page",
  description:
    "Read one web page (an article, press release, government notice, PDF link) and get its text. Use it after " +
    "web_search or when the user gives a link. Long pages are cut at `max_chars` (default 20000); ask again with a " +
    "larger value if the part you need is missing. `fresh: true` reads the live page instead of a cached copy (for pages " +
    "that change, e.g. a live blog). `raw_html: true` returns the HTML itself (tables, structured data). Paywalled " +
    "pages may give only their excerpt. Where the organisation captures a site itself, the text comes from its own copy " +
    "or capture and the answer's first line says so; `original: true` reads the page as it is instead.",
  inputSchema: {
    type: "object",
    properties: {
      url: { type: "string", description: "The page's full http(s) URL." },
      max_chars: { type: "integer", minimum: 1000, maximum: 200000, default: 20000 },
      fresh: { type: "boolean", default: false },
      raw_html: { type: "boolean", default: false },
      original: { type: "boolean", default: false, description: "Read the page as it is, not the organisation's own copy or capture." },
    },
    required: ["url"],
    additionalProperties: false,
  },
  annotations: { title: "Read a web page", readOnlyHint: true, openWorldHint: true },
  async run(args, ctx) {
    const url = str(args, "url", true);
    let u: URL;
    try {
      u = new URL(url);
    } catch {
      throw new ToolInputError("url must be a full http(s) URL");
    }
    if (u.protocol !== "http:" && u.protocol !== "https:") throw new ToolInputError("url must be http or https");
    const max = int(args, "max_chars", 20000, 1000, 200000);
    const cut = (s: string) => (s.length > max ? `${s.slice(0, max)}\n\n[… cut at ${max} characters of ${s.length}; ask with a larger max_chars for more]` : s);
    let costUsd: number | undefined;
    const own = ctx.deps.config.tools.capture;
    if (own && !args.raw_html && args.original !== true) {
      const c = await ownCapture(ctx.deps, own, u.toString()).catch(() => null);  // unreachable or failed: read the page as before
      if (c && (c.kind === "cache" || c.kind === "capture")) return ownCaptureResult(c, own.name, u.toString(), cut);
    }
    if (!args.raw_html && ctx.deps.config.tools.exa) {
      try {
        const p = await exaContents(ctx.deps, u.toString(), args.fresh === true);
        if (p) {
          costUsd = p.costUsd;
          const head = [p.title ? `Title: ${p.title}` : "", `URL: ${p.url}`, p.publishedDate ? `Published: ${p.publishedDate.slice(0, 10)}` : ""].filter(Boolean).join("\n");
          return { content: [{ type: "text", text: `${head}\n\n${cut(p.text)}` }], ...(costUsd !== undefined ? { costUsd } : {}) };
        }
      } catch {
        /* Exa could not read it: read the page directly below */
      }
    }
    try {
      const page = await safeGet(u.toString(), {
        maxBytes: 8_000_000,
        ...(ctx.deps.resolve ? { resolve: ctx.deps.resolve } : {}),
        ...(ctx.deps.fetchPorts ? { ports: ctx.deps.fetchPorts } : {}),
      });
      const ct = page.contentType.toLowerCase();
      const body = page.body.toString("utf8");
      if (page.status >= 400) return text(`The page answered HTTP ${page.status}.${body ? `\n\n${cut(htmlToText(body).text).slice(0, 2000)}` : ""}`, true);
      if (args.raw_html || !(ct.includes("html") || ct.includes("xml"))) {
        if (!ct.startsWith("text/") && !ct.includes("json") && !ct.includes("xml") && !ct.includes("html"))
          return text(`The URL is a ${ct || "binary"} file (${page.body.length} bytes), not a page. Download it in the sandbox with bash (curl) to work with it (the sandbox reaches widely used public sites only).`, true);
        return { content: [{ type: "text", text: `URL: ${page.url}\nContent-Type: ${ct}\n\n${cut(body)}` }], ...(costUsd !== undefined ? { costUsd } : {}) };
      }
      const t = htmlToText(body);
      return { content: [{ type: "text", text: `${t.title ? `Title: ${t.title}\n` : ""}URL: ${page.url}\n\n${cut(t.text)}` }], ...(costUsd !== undefined ? { costUsd } : {}) };
    } catch (e) {
      if (e instanceof BlockedAddress) return text(`Not allowed: ${e.message}.`, true);
      return text(`Could not read the page: ${(e as Error).message}`, true);
    }
  },
};

export const newsSearch: ToolDef = {
  name: "news_search",
  title: "Search Wisers news archive",
  description:
    "Search Wisers (慧科), the archive of Hong Kong and Greater China newspapers, magazines and news sites — the " +
    "authoritative source for what HK print media (明報, 星島, 東方, 信報, SCMP, 經濟日報…) published. Use it for media " +
    "coverage questions: who reported X, how a story was covered, press clippings for a period. Every search uses YYC's " +
    "Wisers quota, so make each query count (no near-duplicate searches; the same search within a day is answered from " +
    "the saved result). Keywords in Traditional Chinese work best (e.g. 「施政報告 AND 房屋」); `period` is days back " +
    "(today, 2, 3, 7, 30, 182, 365) or give `from`/`to`. Returns headline, outlet, date, author and the matching excerpt " +
    "(**bold** = the hit); print items have no web link. Results come 20 per page (`page`). Say in `reason` why you search.",
  inputSchema: {
    type: "object",
    properties: {
      query: { type: "string", description: "Keywords (Wisers syntax: AND, OR, NOT, quotes)." },
      period: { type: "string", enum: ["today", "2", "3", "7", "30", "182", "365"], default: "7" },
      from: { ...DATE, description: "Start date (YYYY-MM-DD); with `to`, instead of period." },
      to: { ...DATE, description: "End date (YYYY-MM-DD)." },
      lang: { type: "string", enum: ["all", "cn", "en"], default: "all" },
      sort: { type: "string", enum: ["DESC:date", "DESC:score"], default: "DESC:date", description: "Newest first, or most relevant first." },
      range: { type: "string", enum: ["all", "title", "content"], default: "all", description: "Match in headline, body, or both." },
      page: { type: "integer", minimum: 1, maximum: 50, default: 1 },
      reason: { type: "string", description: "One line: why this search (kept in the search log)." },
    },
    required: ["query"],
    additionalProperties: false,
  },
  annotations: { title: "Search Wisers news archive", readOnlyHint: true, openWorldHint: true },
  async run(args, ctx) {
    const from = str(args, "from");
    const to = str(args, "to");
    if (!isDate(from) || !isDate(to)) throw new ToolInputError("dates are YYYY-MM-DD");
    const r = await wisersSearch(ctx.deps, ctx.caller.owner, {
      query: str(args, "query", true),
      period: str(args, "period") || "7",
      from,
      to,
      lang: str(args, "lang"),
      sort: str(args, "sort"),
      range: str(args, "range"),
      page: int(args, "page", 1, 1, 50),
      reason: (str(args, "reason") || "YYC AI").slice(0, 200),
    });
    if (!r.items.length) return text(`No articles found (total ${r.total ?? 0}). Try other keywords or a longer period.`);
    const first = (r.page - 1) * r.pageSize + 1;
    const lines = r.items.map((x, i) =>
      [
        `${first + i}. ${x.headline}`,
        `   ${[x.source, x.publishDate, x.author, x.section].filter(Boolean).join(" · ")}`,
        x.link ? `   ${x.link}` : "   (print; no web link)",
        x.excerpt ? `   ${x.excerpt}` : "",
      ].filter(Boolean).join("\n"),
    );
    const head = `Wisers: ${r.total ?? "?"} articles in total; showing ${first}–${first + r.items.length - 1}${r.cached ? " (saved result from earlier today)" : ""}.`;
    return { content: [{ type: "text", text: `${head}\n\n${lines.join("\n\n")}` }], structuredContent: r };
  },
};

export const socialSearch: ToolDef = {
  name: "social_search",
  title: "Search social platforms",
  description:
    "Search one social platform for the newest public posts matching a keyword: xiaohongshu (小紅書), douyin (抖音), " +
    "weibo (微博), x (X/Twitter), threads, youtube, instagram (hashtag), reddit. Use it to see what people are saying " +
    "online about a person, brand, policy or incident (sentiment, viral posts, KOLs). Each search is a paid call, so pick " +
    "the platforms that matter for the audience (mainland audiences: 小紅書/抖音/微博; Hong Kong: Threads, Instagram, " +
    "YouTube; international: X, Reddit) rather than all of them. Returns author, text, date, link and engagement counts.",
  inputSchema: {
    type: "object",
    properties: {
      platform: { type: "string", enum: Object.keys(PLATFORMS) },
      query: { type: "string", description: "Keyword(s); for instagram one hashtag word." },
      limit: { type: "integer", minimum: 1, maximum: 30, default: 15 },
    },
    required: ["platform", "query"],
    additionalProperties: false,
  },
  annotations: { title: "Search social platforms", readOnlyHint: true, openWorldHint: true },
  async run(args, ctx) {
    const platform = str(args, "platform", true).toLowerCase();
    const allowed = (ctx.deps.config.tools.tikhub as { platforms?: string[] } | undefined)?.platforms ?? Object.keys(PLATFORMS);
    if (!PLATFORMS[platform] || !allowed.includes(platform)) throw new ToolInputError(`platform must be one of ${allowed.join(", ")}`);
    const r = await tikhubSearch(ctx.deps, platform, str(args, "query", true));
    const posts = r.posts.slice(0, int(args, "limit", 15, 1, 30));
    if (!posts.length) return { ...text(`No ${PLATFORMS[platform]!.name} posts found.`), costUsd: r.costUsd };
    const lines = posts.map((p, i) =>
      [
        `${i + 1}. ${p.author || "?"}${p.published ? ` · ${p.published.slice(0, 16).replace("T", " ")} UTC` : ""}`,
        `   ${p.text ?? p.title}`,
        `   ${p.url}`,
        p.engagement ? `   ${Object.entries(p.engagement).map(([k, v]) => `${k} ${v}`).join(", ")}` : "",
      ].filter(Boolean).join("\n"),
    );
    return { content: [{ type: "text", text: `${PLATFORMS[platform]!.name}: ${posts.length} posts, newest first.\n\n${lines.join("\n\n")}` }], structuredContent: { posts }, costUsd: r.costUsd };
  },
};

export const BUILTIN: ToolDef[] = [webSearch, webFetch, newsSearch, socialSearch];
