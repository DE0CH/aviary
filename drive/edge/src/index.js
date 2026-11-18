// YYC Drive at the edge (YYC-DRIVE.md "The edge"): a Cloudflare Worker on the drive's whole host, behind its Access
// app. The built page is the Worker's static assets, served by Cloudflare before this code runs. This code answers
// everything the page asks that does not need the folder (settings, sign-in, who is signed in: settings.js) and
// passes the folder's calls (/dav/, /data/) to the server, naming the person Access signed in. A preview the page
// asks for by the file's etag is the same for everyone and never changes, so it is kept in the edge's cache.
//
// Settings (vars): ACCESS_TEAM, ACCESS_AUD (the Access app's audience tag), DRIVE_NAME. A local run (no Access in
// front) has no ACCESS_AUD and names its person in DEV_USER and the server's address in ORIGIN; without ORIGIN a call
// goes to this host's origin, the server behind the tunnel.

import { personOf } from "./access.js";
import { answer, USER_HEADER } from "./settings.js";

const FOLDER = ["/dav/", "/data/"];

const text = (status, body) => new Response(body, { status, headers: { "Content-Type": "text/plain; charset=utf-8", "Cache-Control": "no-store" } });

function decoded(pathname) {
  try { return decodeURIComponent(pathname); } catch { return pathname; }
}

/** A preview asked for by the file's etag (`c`): one picture for that version of the file, whoever asks. */
const lasting = (req, url) => req.method === "GET" && url.searchParams.get("preview") === "1" && url.searchParams.has("c");

async function toServer(req, url, user, env, ctx, { fetcher, cache }) {
  const target = env.ORIGIN ? new URL(url.pathname + url.search, env.ORIGIN) : url;
  const headers = new Headers(req.headers);
  headers.set(USER_HEADER, user);
  const forward = () => fetcher(new Request(target, { method: req.method, headers, body: req.body, redirect: "manual", duplex: "half" }));
  if (!lasting(req, url) || !cache) return forward();

  const key = new Request(url.toString(), { method: "GET" });
  const kept = await cache.match(key);
  if (kept) return personal(kept);
  const res = await forward();
  if (res.status !== 200) return res;
  // the edge keeps it a year
  const edge = new Response(res.body, res);
  edge.headers.set("Cache-Control", "public, max-age=31536000, immutable");
  ctx.waitUntil(cache.put(key, edge.clone()));
  return personal(edge);
}

/** The browser's copy of a preview: kept for good, for this person only. */
function personal(res) {
  const mine = new Response(res.body, res);
  mine.headers.set("Cache-Control", "private, max-age=31536000, immutable");
  return mine;
}

export default {
  async fetch(req, env, ctx, { fetcher = fetch, cache = globalThis.caches?.default } = {}) {
    const url = new URL(req.url);
    let user = !env.ACCESS_AUD && env.DEV_USER;
    if (!user) try {
      user = await personOf(req.headers.get("Cf-Access-Jwt-Assertion"), { team: env.ACCESS_TEAM, aud: env.ACCESS_AUD, fetcher });
    } catch (e) {
      return text(e.message === "access not configured" ? 500 : 403, `forbidden: ${e.message}`);
    }
    const path = decoded(url.pathname);
    if (FOLDER.some((p) => path.startsWith(p))) return toServer(req, url, user, env, ctx, { fetcher, cache });
    const res = answer(req.method, path, url.search, { base: url.origin, user, name: env.DRIVE_NAME || "Drive" });
    if (res) return res;
    console.log(`${req.method} ${path} -> 404`);
    return text(404, "not found");
  },
};
