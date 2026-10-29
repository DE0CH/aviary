// The tools server's own tools (docs/CHAT.md, "tools"): all read-only (search and read; nothing that sends, posts or
// changes anything outside). Descriptions are what the model reads: written for a Hong Kong news / PR assistant,
// saying when to use each and what comes back.
import { BlockedAddress, htmlToText, safeGet } from "./netguard.ts";
import { ANYAPI_SKU, anyapiDescribe, anyapiResult, anyapiRun, anyapiSearch, exaContents, exaSearch, PLATFORMS, tikhubSearch, type AnyapiRun, type AnyapiShape } from "./providers.ts";
import { int, str, text, ToolInputError, type Deps, type ToolDef, type ToolResult } from "./types.ts";

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
    "use the WiseSearch tools when listed (yyc_wisers_search, then yyc_wisers_doc); for 小紅書/抖音/微博/X/Threads posts use social_search; for Facebook, LinkedIn and other " +
    "platforms' data use anyapi_search then anyapi_run.",
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

// ---- AnyAPI: its whole catalog of read-only data APIs, found with anyapi_search and run with anyapi_run ------------

/** The most result text one call hands the model; past it the model narrows with fields / max_items / jq. */
const ANYAPI_TEXT_MAX = 40_000;

const SHAPE_PROPS = {
  fields: { type: "string", description: "Keep only these output fields, comma-separated (e.g. text,url,authorName,createdUtc)." },
  max_items: { type: "integer", minimum: 1, maximum: 500, description: "Keep at most this many result rows." },
  summary: { type: "boolean", description: "Return a compact summary of the result instead of every row." },
  jq: { type: "string", description: "A jq expression to reshape the output (e.g. [.data.items[] | {url, text}])." },
};

function shapeOf(args: Record<string, unknown>): AnyapiShape {
  const fields = str(args, "fields");
  const jq = str(args, "jq");
  return {
    ...(fields ? { fields } : {}),
    ...(args.max_items !== undefined ? { maxItems: int(args, "max_items", 50, 1, 500) } : {}),
    ...(args.summary === true ? { summary: true } : {}),
    ...(jq ? { jq } : {}),
  };
}

/** AnyAPI's static pricing ({from: {maxUsd, unit}, failoverMaxUsd}) in a line. */
function priceText(p: unknown): string {
  if (p === undefined || p === null) return "";
  if (typeof p !== "object") return String(p);
  const o = p as { from?: { maxUsd?: number; unit?: string; maxPer1kUsd?: number; model?: string }; failoverMaxUsd?: number };
  if (!o.from || typeof o.from.maxUsd !== "number") return JSON.stringify(p);
  const unit = o.from.unit ?? "request";
  return `up to USD ${o.from.maxUsd} per ${unit}${typeof o.failoverMaxUsd === "number" ? ` (up to USD ${o.failoverMaxUsd} when it fails over to a backup source)` : ""}`;
}

function skuOf(args: Record<string, unknown>, deps: Deps): string {
  const sku = str(args, "api", true).trim().toLowerCase();
  if (!ANYAPI_SKU.test(sku)) throw new ToolInputError("api is a catalog id like facebook.search_posts (find one with anyapi_search)");
  if ((deps.config.tools.anyapi?.denySkus ?? []).includes(sku)) throw new ToolInputError(`${sku} is not offered here`);
  return sku;
}

function runText(r: AnyapiRun, how: string): ToolResult {
  const body = JSON.stringify(r.output ?? null, null, 1);
  const cut = body.length > ANYAPI_TEXT_MAX;
  const head = [
    how,
    r.costUsd !== undefined ? `Cost: USD ${r.costUsd}.` : "",
    typeof r.items === "number" ? `Rows: ${r.items}.` : "",
    r.resultId ? `result_id: ${r.resultId} (anyapi_result re-reads or reshapes it free for about 15 minutes).` : "",
    r.hint ? `Hint: ${r.hint}` : "",
    cut ? `The output is ${body.length} characters; showing the first ${ANYAPI_TEXT_MAX}. Narrow it with fields, max_items or jq (anyapi_result does that free with the result_id).` : "",
  ].filter(Boolean).join("\n");
  return { content: [{ type: "text", text: `${head}\n\n${cut ? body.slice(0, ANYAPI_TEXT_MAX) : body}` }], ...(r.costUsd !== undefined ? { costUsd: r.costUsd } : {}) };
}

export const anyapiSearchTool: ToolDef = {
  name: "anyapi_search",
  title: "Find a data API (AnyAPI)",
  description:
    "Search AnyAPI's catalog of about 470 read-only data APIs for one that gets what you need, then run it with anyapi_run. " +
    "It covers what the other tools don't: Facebook (search posts or pages, a page's or profile's posts, a post's comments, " +
    "group posts, events, ads library), LinkedIn (company and people pages, posts), Instagram, TikTok, YouTube, X, Reddit, " +
    "Threads, Bluesky, Douyin, 小紅書 and 微博 (profiles, posts, comments, followers, video transcripts), Google (web, news, " +
    "images, Maps, Scholar), app stores, Amazon and other shops, job postings, company data, and any web page as Markdown. " +
    "Describe the task in plain words (e.g. \"facebook page posts\", \"youtube video transcript\"); results list each API's " +
    "id, what it does and its price per call. Searching is free.",
  inputSchema: {
    type: "object",
    properties: {
      query: { type: "string", description: "What you want to get, in plain words." },
      platform: { type: "string", description: "Only APIs for this platform (e.g. facebook, linkedin, google)." },
      limit: { type: "integer", minimum: 1, maximum: 30, default: 10 },
    },
    required: ["query"],
    additionalProperties: false,
  },
  annotations: { title: "Find a data API (AnyAPI)", readOnlyHint: true, openWorldHint: true },
  async run(args, ctx) {
    const r = await anyapiSearch(ctx.deps, { query: str(args, "query", true), platform: str(args, "platform") || undefined, limit: int(args, "limit", 10, 1, 30) });
    const deny = new Set(ctx.deps.config.tools.anyapi?.denySkus ?? []);
    const hits = r.results.filter((h) => !deny.has(String(h.id ?? h.sku ?? h.slug ?? "")));
    if (!hits.length) return text("No matching API. Try other words, or a wider description of the data.");
    const lines = hits.map((h, i) => {
      const id = h.id ?? h.sku ?? h.slug ?? "?";
      const price = priceText(h.pricing ?? h.price ?? h.priceUsd);
      return [`${i + 1}. ${id}${h.name ? ` — ${h.name}` : ""}`, h.description ? `   ${String(h.description).slice(0, 300)}` : "", price ? `   Price: ${price}` : ""].filter(Boolean).join("\n");
    });
    return { content: [{ type: "text", text: `${r.total} matching APIs; showing ${hits.length}. Read one's input with anyapi_describe before running it.\n\n${lines.join("\n\n")}` }] };
  },
};

export const anyapiDescribeTool: ToolDef = {
  name: "anyapi_describe",
  title: "Read a data API's input (AnyAPI)",
  description:
    "Get one AnyAPI API's full contract: its input fields (the JSON Schema anyapi_run's `input` must match exactly; unknown " +
    "fields are refused), what it returns, and its price per call. Free.",
  inputSchema: {
    type: "object",
    properties: { api: { type: "string", description: "The catalog id, e.g. facebook.profile_posts." } },
    required: ["api"],
    additionalProperties: false,
  },
  annotations: { title: "Read a data API's input (AnyAPI)", readOnlyHint: true, openWorldHint: true },
  async run(args, ctx) {
    const d = await anyapiDescribe(ctx.deps, skuOf(args, ctx.deps));
    // the contract the model needs; AnyAPI's source lanes, latency windows and the like are left out
    const keep = ["id", "slug", "name", "description", "howItWorks", "heavy", "pricing", "inputSchema", "outputSchema"];
    const body = JSON.stringify(Object.fromEntries(keep.filter((k) => d[k] !== undefined).map((k) => [k, d[k]])), null, 1);
    return text(body.length > ANYAPI_TEXT_MAX ? body.slice(0, ANYAPI_TEXT_MAX) : body);
  },
};

export const anyapiRunTool: ToolDef = {
  name: "anyapi_run",
  title: "Get data with an API (AnyAPI)",
  description:
    "Run one AnyAPI API (found with anyapi_search, input read with anyapi_describe) and get its data as JSON: e.g. " +
    "facebook.search_posts {query}, facebook.profile_posts {url}, facebook.post_comments {url}, linkedin.company_posts, " +
    "youtube.transcript. It only reads public data; nothing is posted or changed anywhere. Each run is a paid call (the " +
    "price anyapi_describe shows), so run what the task needs rather than every variant. Use fields / max_items / jq to " +
    "keep big results small; the result_id it returns lets anyapi_result reshape the same result again for free.",
  inputSchema: {
    type: "object",
    properties: {
      api: { type: "string", description: "The catalog id, e.g. facebook.search_posts." },
      input: { type: "object", description: "The API's input, exactly as its schema (anyapi_describe) says." },
      ...SHAPE_PROPS,
    },
    required: ["api", "input"],
    additionalProperties: false,
  },
  annotations: { title: "Get data with an API (AnyAPI)", readOnlyHint: true, openWorldHint: true },
  async run(args, ctx) {
    const sku = skuOf(args, ctx.deps);
    const input = args.input;
    if (!input || typeof input !== "object" || Array.isArray(input)) throw new ToolInputError("input is a JSON object (the API's input fields)");
    const r = await anyapiRun(ctx.deps, sku, input as Record<string, unknown>, shapeOf(args));
    return runText(r, `AnyAPI ${sku}.`);
  },
};

export const anyapiResultTool: ToolDef = {
  name: "anyapi_result",
  title: "Re-read an AnyAPI result",
  description:
    "Read a result anyapi_run returned again, free, for about 15 minutes after the run, optionally reshaped with fields / " +
    "max_items / summary / jq (e.g. to see the rows a long result left out, or just the fields you need).",
  inputSchema: {
    type: "object",
    properties: { result_id: { type: "string", description: "The result_id anyapi_run gave." }, ...SHAPE_PROPS },
    required: ["result_id"],
    additionalProperties: false,
  },
  annotations: { title: "Re-read an AnyAPI result", readOnlyHint: true, openWorldHint: true },
  async run(args, ctx) {
    const id = str(args, "result_id", true).trim();
    if (!/^[A-Za-z0-9_-]{1,128}$/.test(id)) throw new ToolInputError("result_id is the id anyapi_run returned");
    const r = await anyapiResult(ctx.deps, id, shapeOf(args));
    const { costUsd: _original, ...free } = r;
    return runText(free, "AnyAPI result (re-read, free).");
  },
};

export const BUILTIN: ToolDef[] = [webSearch, webFetch, socialSearch, anyapiSearchTool, anyapiDescribeTool, anyapiRunTool, anyapiResultTool];
