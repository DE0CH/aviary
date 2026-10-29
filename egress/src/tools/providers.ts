// The outside services behind the built-in tools: Exa (web search, page text), TikHub (social platforms) and AnyAPI.
// Each call reports its cost
// where the provider says it (Exa's costDollars; TikHub's list price per successful request).
import type { Deps } from "./types.ts";

async function callJson<T>(f: typeof fetch, what: string, url: string, init: RequestInit, timeoutMs = 80_000): Promise<T> {
  const ac = new AbortController();
  const t = setTimeout(() => ac.abort(), timeoutMs);
  try {
    const r = await f(url, { ...init, signal: ac.signal });
    const body = await r.text();
    if (!r.ok) throw new Error(`${what} answered HTTP ${r.status}: ${body.slice(0, 300)}`);
    try {
      return JSON.parse(body) as T;
    } catch {
      throw new Error(`${what} answered something that is not JSON: ${body.slice(0, 200)}`);
    }
  } catch (e) {
    if ((e as Error).name === "AbortError") throw new Error(`${what} did not answer within ${Math.round(timeoutMs / 1000)} s`);
    throw e;
  } finally {
    clearTimeout(t);
  }
}

function key(deps: Deps, env: string | undefined, what: string): string {
  const k = env ? deps.env[env] : undefined;
  if (!k) throw new Error(`${what} is not configured on this server (${env ?? "no key"} is not set)`);
  return k;
}

// ---- Exa ----------------------------------------------------------------------------------------------------------

export interface ExaHit {
  title: string;
  url: string;
  publishedDate?: string;
  author?: string;
  snippet: string;
  text?: string;
}

interface ExaResponse {
  results?: { title?: string | null; url: string; publishedDate?: string | null; author?: string | null; text?: string; highlights?: string[]; summary?: string }[];
  costDollars?: { total?: number };
  statuses?: { id: string; status: string; error?: { httpStatusCode?: number; tag?: string } }[];
}

export interface ExaSearchArgs {
  query: string;
  numResults: number;
  since?: string;
  until?: string;
  includeDomains?: string[];
  excludeDomains?: string[];
  category?: string;
  textChars?: number;
}

const exaBase = (deps: Deps) => (deps.config.tools.exa as { baseUrl?: string } | undefined)?.baseUrl ?? "https://api.exa.ai";

export async function exaSearch(deps: Deps, a: ExaSearchArgs): Promise<{ hits: ExaHit[]; costUsd?: number }> {
  const k = key(deps, deps.config.tools.exa?.keyEnv, "Web search (Exa)");
  const body: Record<string, unknown> = {
    query: a.query,
    type: "auto",
    numResults: a.numResults,
    contents: a.textChars ? { text: { maxCharacters: a.textChars }, highlights: { maxCharacters: 400 } } : { highlights: { maxCharacters: 400 } },
  };
  if (a.since) body.startPublishedDate = `${a.since}T00:00:00.000Z`;
  if (a.until) body.endPublishedDate = `${a.until}T23:59:59.999Z`;
  if (a.includeDomains?.length) body.includeDomains = a.includeDomains;
  if (a.excludeDomains?.length) body.excludeDomains = a.excludeDomains;
  if (a.category) body.category = a.category;
  const r = await callJson<ExaResponse>(deps.fetch, "Exa", `${exaBase(deps)}/search`, {
    method: "POST",
    headers: { "x-api-key": k, "content-type": "application/json" },
    body: JSON.stringify(body),
  });
  const hits = (r.results ?? []).map((x) => ({
    title: x.title ?? "",
    url: x.url,
    ...(x.publishedDate ? { publishedDate: x.publishedDate } : {}),
    ...(x.author ? { author: x.author } : {}),
    snippet: (x.highlights?.join(" … ") || x.summary || x.text || "").slice(0, 600),
    ...(a.textChars && x.text ? { text: x.text } : {}),
  }));
  return { hits, ...(typeof r.costDollars?.total === "number" ? { costUsd: r.costDollars.total } : {}) };
}

/** Exa /contents: the page's text from Exa's crawl; `fresh` asks for a live crawl. Null when Exa has no text for it. */
export async function exaContents(deps: Deps, url: string, fresh: boolean): Promise<{ title: string; url: string; text: string; publishedDate?: string; costUsd?: number } | null> {
  const k = key(deps, deps.config.tools.exa?.keyEnv, "Page reading (Exa)");
  const r = await callJson<ExaResponse>(deps.fetch, "Exa", `${exaBase(deps)}/contents`, {
    method: "POST",
    headers: { "x-api-key": k, "content-type": "application/json" },
    body: JSON.stringify({ urls: [url], text: true, ...(fresh ? { maxAgeHours: 0 } : {}) }),
  });
  const st = r.statuses?.[0];
  const page = r.results?.[0];
  if ((st && st.status !== "success") || !page?.text) return null;
  return {
    title: page.title ?? "",
    url: page.url,
    text: page.text,
    ...(page.publishedDate ? { publishedDate: page.publishedDate } : {}),
    ...(typeof r.costDollars?.total === "number" ? { costUsd: r.costDollars.total } : {}),
  };
}

// ---- TikHub ---------------------------------------------------------------------------------------------------------
// Endpoints and parsing follow YYC's news-stream/tikhub_fetch.py (the adapters that run there every day).

export interface SocialPost {
  platform: string;
  author: string;
  title: string;
  text?: string;
  url: string;
  published: string | null;
  engagement?: Record<string, number>;
}

export const PLATFORMS: Record<string, { name: string; cost: number }> = {
  xiaohongshu: { name: "小紅書", cost: 0.01 },
  douyin: { name: "抖音", cost: 0.01 },
  weibo: { name: "微博", cost: 0.001 },
  x: { name: "X", cost: 0.001 },
  threads: { name: "Threads", cost: 0.002 },
  youtube: { name: "YouTube", cost: 0.002 },
  instagram: { name: "Instagram", cost: 0.002 },
  reddit: { name: "Reddit", cost: 0.001 },
};

function* walk(o: unknown): Generator<Record<string, any>> {
  if (Array.isArray(o)) for (const v of o) yield* walk(v);
  else if (o && typeof o === "object") {
    yield o as Record<string, any>;
    for (const v of Object.values(o)) yield* walk(v);
  }
}

export function clean(s: unknown, n = 160): string {
  let t = String(s ?? "")
    .replace(/<[^>]+>/g, "")
    .replace(/&nbsp;|&#160;/g, " ")
    .replace(/&amp;/g, "&")
    .replace(/&lt;/g, "<")
    .replace(/&gt;/g, ">")
    .replace(/&quot;/g, '"')
    .replace(/&#39;/g, "'")
    .replace(/\s+/g, " ")
    .trim();
  if (t.length > n) t = t.slice(0, n - 1).trimEnd() + "…";
  return t;
}

/** epoch s/ms, RFC 2822, Twitter's "Wed Oct 10 20:19:24 +0000 2018" or ISO → ISO 8601 in UTC; else null. */
export function iso(t: unknown): string | null {
  if (t === null || t === undefined || t === "" || t === 0) return null;
  if (typeof t === "number" || (typeof t === "string" && /^\d+(\.\d+)?$/.test(t.trim()))) {
    let v = Number(t);
    if (v > 1e12) v /= 1000;
    if (v < 1e9) return null;
    return new Date(Math.floor(v) * 1000).toISOString();
  }
  const s = String(t).trim();
  const tw = /^\w{3} (\w{3}) (\d{1,2}) (\d\d:\d\d:\d\d) ([+-]\d{4}) (\d{4})$/.exec(s);
  const d = new Date(tw ? `${tw[1]} ${tw[2]} ${tw[5]} ${tw[3]} GMT${tw[4]}` : s);
  return Number.isNaN(d.getTime()) ? null : d.toISOString();
}

/** "4 hours ago" / "3小時前" → an approximate ISO time at the unit's precision. */
export function isoRelative(t: unknown, now = Date.now()): string | null {
  const units: Record<string, number> = { second: 1, minute: 60, hour: 3600, day: 86400, week: 604800, month: 2592000, year: 31536000, 秒: 1, 分钟: 60, 分鐘: 60, 小时: 3600, 小時: 3600, 天: 86400, 周: 604800, 週: 604800, 个月: 2592000, 個月: 2592000, 年: 31536000 };
  const m = /(\d+)\s*(second|minute|hour|day|week|month|year|秒|分钟|分鐘|小时|小時|天|周|週|个月|個月|年)/.exec(String(t ?? ""));
  return m ? new Date(now - Number(m[1]) * units[m[2]!]! * 1000).toISOString() : null;
}

function num(v: unknown): number | null {
  if (typeof v === "boolean" || v === null || v === undefined) return null;
  if (typeof v === "number") return Math.trunc(v);
  const m = /(\d[\d,]*(?:\.\d+)?)\s*([KkMmBb萬万億亿千w]?)/.exec(String(v));
  if (!m) return null;
  const mult: Record<string, number> = { k: 1e3, K: 1e3, 千: 1e3, m: 1e6, M: 1e6, b: 1e9, B: 1e9, 萬: 1e4, 万: 1e4, w: 1e4, 億: 1e8, 亿: 1e8 };
  return Math.trunc(Number(m[1]!.replace(/,/g, "")) * (mult[m[2]!] ?? 1));
}

function counts(d: Record<string, any>, spec: Record<string, string[]>): Record<string, number> {
  const out: Record<string, number> = {};
  for (const [name, paths] of Object.entries(spec))
    for (const p of paths) {
      let v: any = d;
      for (const k of p.split(".")) v = v && typeof v === "object" ? v[k] : undefined;
      const n = num(v);
      if (n !== null) {
        out[name] = n;
        break;
      }
    }
  return out;
}

function post(platform: string, author: unknown, title: unknown, url: string, published: string | null, text?: unknown, engagement?: Record<string, number>): SocialPost {
  const full = clean(text ?? title, 1500);
  const t = clean(title);
  return {
    platform: PLATFORMS[platform]!.name,
    author: clean(author, 60),
    title: t,
    ...(full && full !== t ? { text: full } : {}),
    url,
    published,
    ...(engagement && Object.keys(engagement).length ? { engagement } : {}),
  };
}

type Adapter = { path: string; method: "GET" | "POST"; params: (kw: string) => Record<string, unknown>; parse: (d: unknown) => SocialPost[]; fallback?: Adapter };

const ADAPTERS: Record<string, Adapter> = {
  douyin: {
    path: "/api/v1/douyin/search/fetch_video_search_v2",
    method: "POST",
    params: (kw) => ({ keyword: kw, cursor: 0, sort_type: "2", publish_time: "0", filter_duration: "0", content_type: "0", search_id: "", backtrace: "" }),
    parse: (d) =>
      [...walk(d)]
        .filter((a) => a.aweme_id && "desc" in a && a.author && typeof a.author === "object")
        .map((a) => post("douyin", a.author.nickname, a.desc, `https://www.douyin.com/video/${a.aweme_id}`, iso(a.create_time), undefined,
          counts(a, { likes: ["statistics.digg_count"], comments: ["statistics.comment_count"], shares: ["statistics.share_count"] }))),
  },
  xiaohongshu: {
    path: "/api/v1/xiaohongshu/app_v2/search_notes",
    method: "GET",
    params: (kw) => ({ keyword: kw, sort_type: "time_descending" }),
    parse: (d) =>
      [...walk(d)]
        .filter((n) => n.id && n.user && typeof n.user === "object" && ("title" in n || "desc" in n) && ("timestamp" in n || "time" in n))
        .map((n) => post("xiaohongshu", n.user.nickname, n.title || n.desc, `https://www.xiaohongshu.com/explore/${n.id}`, iso(n.timestamp || n.time),
          [n.title, n.desc].filter(Boolean).join("\n") || undefined,
          counts(n, { likes: ["liked_count", "likes", "interact_info.liked_count"], comments: ["comments_count", "interact_info.comment_count"], shares: ["shared_count", "share_count", "interact_info.share_count"] }))),
  },
  weibo: {
    path: "/api/v1/weibo/app/fetch_search_all",
    method: "GET",
    params: (kw) => ({ query: kw, search_type: "61", page: "1" }),
    parse: (d) =>
      [...walk(d)]
        .filter((m) => m.mid && m.user && typeof m.user === "object" && ("text" in m || "text_raw" in m) && m.created_at)
        .map((m) => post("weibo", m.user.screen_name, m.text_raw || m.text, `https://m.weibo.cn/detail/${m.mid}`, iso(m.created_at), undefined,
          counts(m, { likes: ["attitudes_count"], comments: ["comments_count"], shares: ["reposts_count"], views: ["reads_count"] }))),
  },
  x: {
    path: "/api/v1/twitter/web/fetch_search_timeline",
    method: "GET",
    params: (kw) => ({ keyword: kw, search_type: "Latest" }),
    parse: (d) =>
      (((d as any)?.timeline ?? []) as Record<string, any>[])
        .filter((t) => (t.tweet_id || t.id_str) && t.text != null && t.created_at)
        .map((t) => {
          const u = t.user_info || t.author || t.user || {};
          const handle = u.screen_name || u.username || t.screen_name || "";
          const id = String(t.tweet_id || t.id_str);
          return post("x", u.name || (handle ? `@${handle}` : ""), t.text, `https://x.com/${handle || "i"}/status/${id}`, iso(t.created_at), undefined,
            counts(t, { views: ["views", "view_count"], likes: ["favorites", "likes", "favorite_count"], replies: ["replies", "reply_count"], shares: ["retweets", "retweet_count"] }));
        }),
  },
  threads: {
    path: "/api/v1/threads/web/search_recent",
    method: "GET",
    params: (kw) => ({ query: kw }),
    parse: parseThreads,
    fallback: { path: "/api/v1/threads/web/search_posts", method: "GET", params: (kw) => ({ query: kw }), parse: parseThreads },
  },
  youtube: {
    path: "/api/v1/youtube/web_v2/get_general_search_v2",
    method: "GET",
    params: (kw) => ({ keyword: kw, upload_date: "this_week", sort_by: "upload_date", type: "video" }),
    parse: (d) =>
      [...walk(d)]
        .filter((v) => (v.video_id || v.videoId) && v.title && typeof v.title === "string")
        .map((v) => post("youtube", v.author || v.channel_name || v.channel_title, v.title, `https://www.youtube.com/watch?v=${v.video_id || v.videoId}`,
          isoRelative(v.published_time || v.publish_time), [v.title, typeof v.description === "string" ? v.description : ""].filter(Boolean).join("\n"),
          counts(v, { views: ["view_count", "views", "number_of_views", "viewCount"] }))),
  },
  instagram: {
    path: "/api/v1/instagram/v2/fetch_hashtag_posts",
    method: "GET",
    params: (kw) => ({ keyword: kw.replace(/[\W_]+/gu, "").toLowerCase(), feed_type: "recent" }),
    parse: (d) =>
      [...walk(d)]
        .filter((p) => p.code && p.taken_at && p.user && typeof p.user === "object")
        .map((p) => {
          const cap = p.caption && typeof p.caption === "object" ? p.caption.text : p.caption;
          return post("instagram", p.user.full_name || p.user.username, cap || "(post without caption)", `https://www.instagram.com/p/${p.code}/`, iso(p.taken_at), undefined,
            counts(p, { views: ["play_count", "view_count", "ig_play_count"], likes: ["like_count"], comments: ["comment_count"] }));
        }),
  },
  reddit: {
    path: "/api/v1/reddit/app/fetch_dynamic_search",
    method: "GET",
    params: (kw) => ({ query: kw, search_type: "post", sort: "NEW", need_format: "true" }),
    parse: (d) =>
      [...walk(d)]
        .filter((p) => p.postTitle && (p.permalink || p.url))
        .map((p) => {
          let link = String(p.permalink || p.url);
          if (link.startsWith("/")) link = "https://www.reddit.com" + link;
          const sub = p.subreddit && typeof p.subreddit === "object" ? p.subreddit.name : p.subreddit;
          const au = p.authorInfo && typeof p.authorInfo === "object" ? p.authorInfo.name : p.author;
          return post("reddit", sub ? `r/${sub}` : au, p.postTitle, link, iso(p.createdAt),
            [p.postTitle, typeof p.content === "string" ? p.content : ""].filter(Boolean).join("\n"),
            counts(p, { likes: ["score", "upvoteCount"], comments: ["commentCount", "numComments"] }));
        }),
  },
};

function parseThreads(d: unknown): SocialPost[] {
  return [...walk(d)]
    .filter((p) => p.code && p.user && typeof p.user === "object" && p.taken_at)
    .map((p) => {
      const un = p.user.username || "";
      const cap = p.caption && typeof p.caption === "object" ? p.caption.text : p.caption;
      return post("threads", un, cap, `https://www.threads.com/@${un}/post/${p.code}`, iso(p.taken_at), undefined,
        counts(p, { likes: ["like_count"], replies: ["text_post_app_info.direct_reply_count"], shares: ["text_post_app_info.repost_count"] }));
    });
}

function dedupeNewest(items: SocialPost[]): SocialPost[] {
  const seen = new Set<string>();
  return items
    .filter((p) => p.url && p.title && !seen.has(p.url) && (seen.add(p.url), true))
    .sort((a, b) => (b.published ?? "").localeCompare(a.published ?? ""));
}

export async function tikhubSearch(deps: Deps, platform: string, keyword: string): Promise<{ posts: SocialPost[]; costUsd: number }> {
  const k = key(deps, deps.config.tools.tikhub?.keyEnv, "Social platform search (TikHub)");
  const base = (deps.config.tools.tikhub as { baseUrl?: string } | undefined)?.baseUrl ?? "https://api.tikhub.io";
  let ad: Adapter | undefined = ADAPTERS[platform];
  if (!ad) throw new Error(`unknown platform ${platform}`);
  let lastErr: Error | null = null;
  while (ad) {
    const params = ad.params(keyword);
    const url = new URL(base + ad.path);
    if (ad.method === "GET") for (const [k2, v] of Object.entries(params)) url.searchParams.set(k2, String(v));
    try {
      const j = await callJson<{ code?: number; message?: string; data?: unknown }>(deps.fetch, "TikHub", url.toString(), {
        method: ad.method,
        headers: { authorization: `Bearer ${k}`, accept: "application/json", "user-agent": "yyc-ai/1", ...(ad.method === "POST" ? { "content-type": "application/json" } : {}) },
        ...(ad.method === "POST" ? { body: JSON.stringify(params) } : {}),
      }, 45_000);
      if (j.code !== undefined && j.code !== 200) throw new Error(`TikHub code ${j.code}: ${String(j.message ?? "").slice(0, 200)}`);
      return { posts: dedupeNewest(ad.parse(j.data)), costUsd: PLATFORMS[platform]!.cost };
    } catch (e) {
      lastErr = e as Error;
      ad = ad.fallback;
    }
  }
  throw lastErr ?? new Error("TikHub failed");
}

// ---- AnyAPI ---------------------------------------------------------------------------------------------------------
// REST (api.getanyapi.com, OpenAPI at /openapi.json): GET /catalog/search, GET /v1/apis/{sku}, POST /v1/run/{sku},
// GET /v1/results/{id} (a paid result re-read free for about 15 minutes). Every run reports its own costUsd.

/** A catalog id: platform.action (e.g. facebook.search_posts). */
export const ANYAPI_SKU = /^[a-z0-9][a-z0-9_-]*\.[a-z0-9][a-z0-9_.-]*$/;

function anyapiBase(deps: Deps): string {
  return (deps.config.tools.anyapi?.baseUrl ?? "https://api.getanyapi.com").replace(/\/$/, "");
}

/** AnyAPI's refusals in words the model can pass on (its JSON error body: {error, code}). */
async function anyapiCall<T>(deps: Deps, path: string, init: RequestInit, timeoutMs: number): Promise<T> {
  const k = key(deps, deps.config.tools.anyapi?.keyEnv, "AnyAPI");
  const ac = new AbortController();
  const t = setTimeout(() => ac.abort(), timeoutMs);
  try {
    const r = await deps.fetch(anyapiBase(deps) + path, {
      ...init,
      headers: { authorization: `Bearer ${k}`, accept: "application/json", "user-agent": "yyc-ai/1", ...(init.body ? { "content-type": "application/json" } : {}) },
      signal: ac.signal,
    });
    const body = await r.text();
    let j: unknown;
    try {
      j = JSON.parse(body);
    } catch {
      throw new Error(`AnyAPI answered HTTP ${r.status} with something that is not JSON: ${body.slice(0, 200)}`);
    }
    if (!r.ok) {
      const e = j as { error?: unknown; code?: unknown; message?: unknown };
      const code = String(e.code ?? "");
      if (r.status === 402 || code === "insufficient_balance") {
        throw new Error("AnyAPI's balance has run out, so its APIs can't be used until it is topped up (the admin has to add funds).");
      }
      throw new Error(`AnyAPI answered HTTP ${r.status}${code ? ` (${code})` : ""}: ${String(e.error ?? e.message ?? body).slice(0, 400)}`);
    }
    return j as T;
  } catch (e) {
    if ((e as Error).name === "AbortError") throw new Error(`AnyAPI did not answer within ${Math.round(timeoutMs / 1000)} s`);
    throw e;
  } finally {
    clearTimeout(t);
  }
}

export interface AnyapiCatalogHit {
  id?: string;
  sku?: string;
  slug?: string;
  name?: string;
  description?: string;
  category?: string;
  platform?: string;
  price?: unknown;
  pricing?: unknown;
  priceUsd?: unknown;
}

export async function anyapiSearch(deps: Deps, q: { query: string; platform?: string; category?: string; limit: number }): Promise<{ results: AnyapiCatalogHit[]; total: number }> {
  const u = new URLSearchParams({ q: q.query, limit: String(q.limit) });
  if (q.platform) u.set("platform", q.platform);
  if (q.category) u.set("category", q.category);
  const r = await anyapiCall<{ results?: AnyapiCatalogHit[]; total?: number }>(deps, `/catalog/search?${u}`, { method: "GET" }, 30_000);
  return { results: r.results ?? [], total: r.total ?? (r.results ?? []).length };
}

export async function anyapiDescribe(deps: Deps, sku: string): Promise<Record<string, unknown>> {
  return await anyapiCall<Record<string, unknown>>(deps, `/v1/apis/${encodeURIComponent(sku)}`, { method: "GET" }, 30_000);
}

export interface AnyapiRun {
  output?: unknown;
  costUsd?: number;
  resultId?: string;
  items?: number;
  hint?: string;
  error?: unknown;
  message?: unknown;
}

/** Shaping AnyAPI applies to a result (its fields / max_items / summary / jq query parameters). */
export interface AnyapiShape {
  fields?: string;
  maxItems?: number;
  summary?: boolean;
  jq?: string;
}

function shapeQuery(s: AnyapiShape): string {
  const u = new URLSearchParams();
  if (s.fields) u.set("fields", s.fields);
  if (s.maxItems) u.set("max_items", String(s.maxItems));
  if (s.summary) u.set("summary", "true");
  if (s.jq) u.set("jq", s.jq);
  const q = u.toString();
  return q ? `?${q}` : "";
}

export async function anyapiRun(deps: Deps, sku: string, input: Record<string, unknown>, shape: AnyapiShape): Promise<AnyapiRun> {
  // AnyAPI's published latency for slow scrapes reaches about a minute; it fails over between sources by itself
  return await anyapiCall<AnyapiRun>(deps, `/v1/run/${encodeURIComponent(sku)}${shapeQuery(shape)}`, { method: "POST", body: JSON.stringify(input) }, 120_000);
}

export async function anyapiResult(deps: Deps, id: string, shape: AnyapiShape): Promise<AnyapiRun> {
  return await anyapiCall<AnyapiRun>(deps, `/v1/results/${encodeURIComponent(id)}${shapeQuery(shape)}`, { method: "GET" }, 30_000);
}

