// Cloudflare Access's token (Cf-Access-Jwt-Assertion): RS256, signed by the team's keys
// (https://<team>.cloudflareaccess.com/cdn-cgi/access/certs), issued by the team's domain for this app's audience.
// A person's sign-in carries their email; that email is who they are to the drive.

const SKEW = 60; // seconds of clock difference allowed on exp and nbf
const KEYS_TTL = 3600 * 1000;
let cache = { team: null, at: 0, keys: new Map() };

const b64url = (s) => {
  const bin = atob(s.replace(/-/g, "+").replace(/_/g, "/") + "=".repeat((4 - (s.length % 4)) % 4));
  return Uint8Array.from(bin, (c) => c.charCodeAt(0));
};
const json = (s) => JSON.parse(new TextDecoder().decode(b64url(s)));

async function loadKeys(team, fetcher) {
  const r = await fetcher(`https://${team}.cloudflareaccess.com/cdn-cgi/access/certs`);
  if (!r.ok) throw new Error(`certs ${r.status}`);
  const { keys = [] } = await r.json();
  const m = new Map();
  for (const k of keys) {
    if (k.kty !== "RSA" || !k.kid) continue;
    m.set(k.kid, await crypto.subtle.importKey("jwk", { kty: k.kty, n: k.n, e: k.e, alg: "RS256", ext: true },
      { name: "RSASSA-PKCS1-v1_5", hash: "SHA-256" }, false, ["verify"]));
  }
  cache = { team, at: Date.now(), keys: m };
}

async function keyFor(kid, team, fetcher) {
  const fresh = cache.team === team && Date.now() - cache.at < KEYS_TTL;
  if (!fresh || !cache.keys.has(kid)) await loadKeys(team, fetcher); // a kid not seen: Access turned its keys over
  return cache.keys.get(kid);
}

export function resetKeyCache() { cache = { team: null, at: 0, keys: new Map() }; }

/** The signed-in person's email; throws Error(reason) when the token is not good. */
export async function personOf(token, { team, aud, fetcher = fetch, now = Date.now() / 1000 }) {
  if (!team || !aud) throw new Error("access not configured");
  if (!token) throw new Error("no access token");
  const parts = token.split(".");
  if (parts.length !== 3) throw new Error("malformed token");
  let header, claims;
  try { header = json(parts[0]); claims = json(parts[1]); } catch { throw new Error("malformed token"); }
  if (header.alg !== "RS256") throw new Error("bad alg");
  const key = await keyFor(header.kid, team, fetcher);
  if (!key) throw new Error("unknown key");
  const ok = await crypto.subtle.verify("RSASSA-PKCS1-v1_5", key, b64url(parts[2]),
    new TextEncoder().encode(parts[0] + "." + parts[1]));
  if (!ok) throw new Error("bad signature");
  if (claims.iss !== `https://${team}.cloudflareaccess.com`) throw new Error("bad issuer");
  const auds = Array.isArray(claims.aud) ? claims.aud : [claims.aud];
  if (!auds.includes(aud)) throw new Error("bad audience");
  if (typeof claims.exp !== "number" || claims.exp + SKEW < now) throw new Error("expired");
  if (typeof claims.nbf === "number" && claims.nbf - SKEW > now) throw new Error("not yet valid");
  if (!claims.email) throw new Error("no person in token");
  return String(claims.email).toLowerCase();
}
