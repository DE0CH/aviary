// Cloudflare Access JWT verification (the server never trusts that Access ran): RS256 against the team's JWKS,
// issuer, audience and expiry. Same checks as server/src/auth.ts AccessVerifier.
import { createLocalJWKSet, createRemoteJWKSet, jwtVerify, type JSONWebKeySet, type JWTVerifyGetKey } from "jose";
import type { IncomingMessage } from "node:http";

export class AccessVerifier {
  readonly #getKey: JWTVerifyGetKey;
  readonly #issuer: string;
  readonly #audience: string;
  readonly #identity: ((token: string) => Promise<Record<string, unknown> | null>) | null;
  readonly #names = new Map<string, { name: string | null; until: number }>();
  /** `identity` fetches the Access identity for a token (default: the team's get-identity endpoint, only with the
   * remote JWKS; tests pass their own or none). */
  constructor(o: { teamDomain: string; audience: string; jwks?: JSONWebKeySet; identity?: (token: string) => Promise<Record<string, unknown> | null> }) {
    this.#issuer = `https://${o.teamDomain}`;
    this.#audience = o.audience;
    this.#identity = o.identity ?? (o.jwks ? null : (token) => remoteIdentity(this.#issuer, token));
    this.#getKey = o.jwks
      ? createLocalJWKSet(o.jwks)
      : createRemoteJWKSet(new URL(`${this.#issuer}/cdn-cgi/access/certs`), { cacheMaxAge: 3600_000, cooldownDuration: 30_000 });
  }
  /** The signed-in email; throws on anything but a valid, current identity token for this application. */
  async email(token: string): Promise<string> {
    const { payload } = await jwtVerify(token, this.#getKey, {
      issuer: this.#issuer,
      audience: this.#audience,
      algorithms: ["RS256"],
      clockTolerance: 5,
      requiredClaims: ["exp"],
    });
    const email = typeof payload.email === "string" ? payload.email.trim().toLowerCase() : "";
    if (!email) throw new Error("Access token has no email (service tokens are not staff)");
    return email;
  }

  /** The person's first name as their identity provider gives it (Google: given_name, else the first word of name),
   * for the empty screen's greeting; null when there is none (e-mailed code). Never derived from the address.
   * Best effort: cached per email (6 h; 5 min after a failure), 1.5 s timeout. */
  async firstName(token: string, email: string): Promise<string | null> {
    const hit = this.#names.get(email);
    if (hit && hit.until > Date.now()) return hit.name;
    let name: string | null = null;
    let ttl = 6 * 3600_000;
    try {
      const { payload } = await jwtVerify(token, this.#getKey, { issuer: this.#issuer, audience: this.#audience, algorithms: ["RS256"], clockTolerance: 5 });
      name = pickFirstName(payload) ?? (this.#identity ? pickFirstName(await this.#identity(token)) : null);
    } catch {
      ttl = 5 * 60_000;
    }
    this.#names.set(email, { name, until: Date.now() + ttl });
    return name;
  }
}

/** given_name, else the first word of name (a CJK name is kept whole); trimmed to 40 characters; null otherwise. */
export function pickFirstName(id: Record<string, unknown> | null | undefined): string | null {
  if (!id) return null;
  const given = typeof id.given_name === "string" ? id.given_name.trim() : "";
  const full = typeof id.name === "string" ? id.name.trim() : "";
  const n = given || (/[\u3400-\u9fff]/.test(full) ? full : full.split(/\s+/)[0] || "");
  if (!n || n.includes("@")) return null;
  return [...n].slice(0, 40).join("");
}

async function remoteIdentity(issuer: string, token: string): Promise<Record<string, unknown> | null> {
  const r = await fetch(`${issuer}/cdn-cgi/access/get-identity`, {
    headers: { cookie: `CF_Authorization=${token}` },
    signal: AbortSignal.timeout(1500),
  });
  if (!r.ok) return null;
  const j = (await r.json()) as Record<string, unknown>;
  // Google's claims arrive as `name` and, depending on the IdP setup, under oidc_fields / idp claims.
  const oidc = (j.oidc_fields ?? {}) as Record<string, unknown>;
  return { given_name: oidc.given_name ?? j.given_name, name: oidc.name ?? j.name };
}

export function accessToken(req: IncomingMessage): string {
  const h = req.headers["cf-access-jwt-assertion"];
  if (typeof h === "string" && h) return h;
  const m = /(?:^|;\s*)CF_Authorization=([^;]+)/.exec(req.headers.cookie || "");
  return m ? decodeURIComponent(m[1]!) : "";
}
