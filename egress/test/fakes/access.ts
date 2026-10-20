// Cloudflare Access stand-in: a local key pair, its JWKS for the verifier, and signed identity tokens.
import { exportJWK, generateKeyPair, SignJWT, type JSONWebKeySet } from "jose";

export const TEAM = "team.cloudflareaccess.com";
export const AUD = "test-aud";

export async function fakeAccess(): Promise<{ jwks: JSONWebKeySet; token(email: string, opts?: { aud?: string; expSec?: number }): Promise<string> }> {
  const { publicKey, privateKey } = await generateKeyPair("RS256");
  const jwk = await exportJWK(publicKey);
  return {
    jwks: { keys: [{ ...jwk, kid: "k1", alg: "RS256", use: "sig" }] },
    token: (email, opts = {}) =>
      new SignJWT({ email })
        .setProtectedHeader({ alg: "RS256", kid: "k1" })
        .setIssuer(`https://${TEAM}`)
        .setAudience(opts.aud ?? AUD)
        .setIssuedAt()
        .setExpirationTime(Math.floor(Date.now() / 1000) + (opts.expSec ?? 600))
        .sign(privateKey),
  };
}
