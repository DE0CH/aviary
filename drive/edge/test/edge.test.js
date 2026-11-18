// The edge as the page uses it: node --test (Node 22's fetch, Request, Response and WebCrypto stand in for the
// Workers runtime). Access's keys are a key pair made here; the server and the edge cache are stand-ins.
import { test } from "node:test";
import assert from "node:assert/strict";
import worker from "../src/index.js";
import { resetKeyCache } from "../src/access.js";
import { DRIVE, USER_HEADER } from "../src/settings.js";

const TEAM = "yyc-test";
const AUD = "drive-aud";
const BASE = "https://drive.example.com";
const env = { ACCESS_TEAM: TEAM, ACCESS_AUD: AUD, DRIVE_NAME: "YYC Drive" };

const pair = await crypto.subtle.generateKey(
  { name: "RSASSA-PKCS1-v1_5", modulusLength: 2048, publicExponent: new Uint8Array([1, 0, 1]), hash: "SHA-256" }, true, ["sign", "verify"]);
const jwk = { ...(await crypto.subtle.exportKey("jwk", pair.publicKey)), kid: "k1" };
const b64 = (bytes) => Buffer.from(bytes).toString("base64url");

async function token(claims, key = pair.privateKey) {
  const now = Math.floor(Date.now() / 1000);
  const head = b64(JSON.stringify({ alg: "RS256", kid: "k1" }));
  const body = b64(JSON.stringify({ iss: `https://${TEAM}.cloudflareaccess.com`, aud: [AUD], exp: now + 600, iat: now, ...claims }));
  const sig = await crypto.subtle.sign("RSASSA-PKCS1-v1_5", key, new TextEncoder().encode(`${head}.${body}`));
  return `${head}.${body}.${b64(new Uint8Array(sig))}`;
}

/** A stand-in for everything the edge reaches: Access's keys, and the server (each request it gets is kept). */
function world(server = () => new Response("from the server")) {
  const seen = [];
  const fetcher = async (req) => {
    const url = typeof req === "string" ? req : req.url;
    if (url === `https://${TEAM}.cloudflareaccess.com/cdn-cgi/access/certs`) return Response.json({ keys: [jwk] });
    seen.push(req);
    return server(req);
  };
  return { fetcher, seen };
}

/** A stand-in for the edge's cache (caches.default). */
function edgeCache() {
  const m = new Map();
  return {
    m,
    match: async (k) => m.get(k.url)?.clone(),
    put: async (k, r) => { m.set(k.url, new Response(await r.arrayBuffer(), r)); },
  };
}
const ctx = () => { const waits = []; return { waits, waitUntil: (p) => waits.push(p) }; };

async function ask(method, path, { as = "amy@example.com", headers = {}, body, w = world(), cache = edgeCache(), c = ctx() } = {}) {
  resetKeyCache();
  const h = new Headers(headers);
  if (as) h.set("Cf-Access-Jwt-Assertion", await token({ email: as }));
  const res = await worker.fetch(new Request(BASE + path, { method, headers: h, body }), env, c, { fetcher: w.fetcher, cache });
  await Promise.all(c.waits);
  return res;
}

test("nothing is answered without Access's token for this app", async () => {
  assert.equal((await ask("GET", "/config.json", { as: null })).status, 403);
  resetKeyCache();
  const other = await crypto.subtle.generateKey(
    { name: "RSASSA-PKCS1-v1_5", modulusLength: 2048, publicExponent: new Uint8Array([1, 0, 1]), hash: "SHA-256" }, true, ["sign"]);
  for (const t of [await token({ email: "amy@example.com" }, other.privateKey), await token({ email: "amy@example.com", aud: ["another-app"] }),
    await token({ email: "amy@example.com", exp: 1 }), await token({})]) {
    const res = await worker.fetch(new Request(BASE + "/graph/v1.0/me", { headers: { "Cf-Access-Jwt-Assertion": t } }), env, ctx(), { fetcher: world().fetcher, cache: edgeCache() });
    assert.equal(res.status, 403);
  }
  const w = world();
  assert.equal((await ask("PROPFIND", `/dav/spaces/${DRIVE}/`, { as: null, w })).status, 403);
  assert.equal(w.seen.length, 0, "the server never hears of it");
});

test("a local run's person is only taken without Access", async () => {
  const local = await worker.fetch(new Request(BASE + "/graph/v1.0/me"), { DRIVE_NAME: "x", DEV_USER: "dev@example.com" }, ctx(), world());
  assert.equal((await local.json()).mail, "dev@example.com");
  const live = await worker.fetch(new Request(BASE + "/graph/v1.0/me"), { ...env, DEV_USER: "dev@example.com" }, ctx(), { fetcher: world().fetcher, cache: edgeCache() });
  assert.equal(live.status, 403);
});

test("the page's settings", async () => {
  const config = await (await ask("GET", "/config.json")).json();
  assert.equal(config.server, `${BASE}/`);
  assert.equal(config.openIdConnect.metadata_url, `${BASE}/.well-known/openid-configuration`);
  assert.equal(config.theme, `${BASE}/themes/opencloud/theme.json`);
});

test("sign-in is answered for the person Access signed in", async () => {
  const meta = await (await ask("GET", "/.well-known/openid-configuration")).json();
  assert.equal(meta.token_endpoint, `${BASE}/oidc/token`);
  const r = await ask("GET", "/oidc/authorize?redirect_uri=http%3A%2F%2Fx%2Foidc-callback.html&state=abc");
  assert.equal(r.status, 302);
  assert.equal(r.headers.get("location"), "http://x/oidc-callback.html?code=drive&state=abc");
  const tok = await (await ask("POST", "/oidc/token", { body: "grant_type=authorization_code&code=drive" })).json();
  const claims = JSON.parse(Buffer.from(tok.id_token.split(".")[1], "base64url").toString());
  assert.equal(claims.email, "amy@example.com");
  assert.equal(claims.preferred_username, "amy");
  const me = await (await ask("GET", "/graph/v1.0/me", { as: "Bob@Example.com" })).json();
  assert.equal(me.mail, "bob@example.com");
  assert.equal(me.displayName, "bob");
  const info = await (await ask("GET", "/oidc/userinfo")).json();
  assert.equal(info.email, "amy@example.com");
});

test("one drive and nothing else on offer", async () => {
  const mine = await (await ask("GET", "/graph/v1beta1/me/drives?%24orderby=name+asc&%24filter=driveType+eq+personal")).json();
  assert.equal(mine.value.length, 1);
  assert.equal(mine.value[0].name, "YYC Drive");
  assert.equal(mine.value[0].id, DRIVE);
  assert.equal(mine.value[0].root.webDavUrl, `${BASE}/dav/spaces/${DRIVE}`);
  const projects = await (await ask("GET", "/graph/v1beta1/me/drives?%24orderby=name+asc&%24filter=driveType+eq+project")).json();
  assert.deepEqual(projects.value, []);
  const caps = await (await ask("GET", "/ocs/v1.php/cloud/capabilities?format=json")).json();
  const c = caps.ocs.data.capabilities;
  assert.equal(c.files_sharing.api_enabled, false);
  assert.equal(c.files.favorites, false);
  assert.equal(c.spaces.projects, false);
  assert.equal(c.files.tus_support.max_chunk_size, 10_000_000);
  const key = await (await ask("GET", "/ocs/v1.php/cloud/user/signing-key")).text();
  assert.match(key, /<user>amy@example.com<\/user>/);
  for (const [m, p] of [["POST", "/api/v0/settings/roles-list"], ["POST", "/api/v0/settings/permissions-list"], ["GET", "/app/list"],
    ["GET", "/graph/v1beta1/roleManagement/permissions/roleDefinitions"], ["GET", "/ocs/v2.php/apps/notifications/api/v1/notifications"]])
    assert.ok((await ask(m, p)).ok, `${m} ${p}`);
  assert.equal((await ask("GET", "/ocs/v1.php/cloud/users")).status, 404);
});

test("the folder's calls go to the server, naming the person Access signed in", async () => {
  const w = world();
  const res = await ask("PROPFIND", `/dav/spaces/${DRIVE}/a%20b/`, { w, headers: { Depth: "1", [USER_HEADER]: "mallory@example.com" }, body: "<propfind/>" });
  assert.equal(await res.text(), "from the server");
  assert.equal(w.seen.length, 1);
  const got = w.seen[0];
  assert.equal(got.method, "PROPFIND");
  assert.equal(got.url, `${BASE}/dav/spaces/${DRIVE}/a%20b/`);
  assert.equal(got.headers.get(USER_HEADER), "amy@example.com", "never a name the browser sent");
  assert.equal(got.headers.get("depth"), "1");
  assert.ok(got.headers.get("cf-access-jwt-assertion"), "the tunnel checks Access's token too");
  assert.equal(await got.text(), "<propfind/>");
  for (const p of ["/data/tus/abc", "/dav/spaces"]) {
    const w2 = world();
    await ask(p === "/dav/spaces" ? "REPORT" : "PATCH", p, { w: w2, body: "x" });
    assert.equal(w2.seen.length, 1, p);
  }
});

test("a local run sends the folder's calls to the server it names", async () => {
  const w = world();
  resetKeyCache();
  const req = new Request(`${BASE}/dav/spaces/${DRIVE}/x.txt?y=1`, { headers: { "Cf-Access-Jwt-Assertion": await token({ email: "amy@example.com" }) } });
  await worker.fetch(req, { ...env, ORIGIN: "http://127.0.0.1:8201" }, ctx(), { fetcher: w.fetcher, cache: edgeCache() });
  assert.equal(w.seen[0].url, `http://127.0.0.1:8201/dav/spaces/${DRIVE}/x.txt?y=1`);
});

test("a preview asked for by the file's etag is drawn once and then kept at the edge", async () => {
  let drawn = 0;
  const w = world(() => { drawn++; return new Response("jpeg", { headers: { "Content-Type": "image/jpeg", "Cache-Control": "private, max-age=31536000, immutable" } }); });
  const cache = edgeCache();
  const path = `/dav/spaces/${DRIVE}/clip.mp4?preview=1&x=256&y=256&c=abc`;
  const first = await ask("GET", path, { w, cache });
  assert.equal(await first.text(), "jpeg");
  assert.equal(first.headers.get("cache-control"), "private, max-age=31536000, immutable");
  const second = await ask("GET", path, { w, cache, as: "bob@example.com" });
  assert.equal(await second.text(), "jpeg");
  assert.equal(second.headers.get("cache-control"), "private, max-age=31536000, immutable");
  assert.equal(second.headers.get("content-type"), "image/jpeg");
  assert.equal(drawn, 1);
  // without the etag the preview may change: asked again each time; a missing preview is never kept
  await ask("GET", `/dav/spaces/${DRIVE}/clip.mp4?preview=1&x=256&y=256`, { w, cache });
  await ask("GET", `/dav/spaces/${DRIVE}/clip.mp4?preview=1&x=256&y=256`, { w, cache });
  assert.equal(drawn, 3);
  const none = world(() => new Response(null, { status: 404 }));
  await ask("GET", `/dav/spaces/${DRIVE}/a.zip?preview=1&c=1`, { w: none, cache });
  assert.equal(cache.m.size, 1);
  // a file itself is never kept here
  await ask("GET", `/dav/spaces/${DRIVE}/clip.mp4`, { w, cache });
  await ask("GET", `/dav/spaces/${DRIVE}/clip.mp4`, { w, cache });
  assert.equal(drawn, 5);
});
