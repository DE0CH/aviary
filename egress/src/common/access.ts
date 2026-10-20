// Cloudflare Access JWT verification (the server never trusts that Access ran): RS256 against the team's JWKS,
// issuer, audience and expiry. Same checks as server/src/auth.ts AccessVerifier.
import { createLocalJWKSet, createRemoteJWKSet, jwtVerify, type JSONWebKeySet, type JWTVerifyGetKey } from "jose";
import type { IncomingMessage } from "node:http";

export class AccessVerifier {
  readonly #getKey: JWTVerifyGetKey;
  readonly #issuer: string;
  readonly #audience: string;
  constructor(o: { teamDomain: string; audience: string; jwks?: JSONWebKeySet }) {
    this.#issuer = `https://${o.teamDomain}`;
    this.#audience = o.audience;
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
}

export function accessToken(req: IncomingMessage): string {
  const h = req.headers["cf-access-jwt-assertion"];
  if (typeof h === "string" && h) return h;
  const m = /(?:^|;\s*)CF_Authorization=([^;]+)/.exec(req.headers.cookie || "");
  return m ? decodeURIComponent(m[1]!) : "";
}
