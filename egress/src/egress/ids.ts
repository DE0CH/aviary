// Ids and keys egress puts in headers and logs (aviary platform spec 3.5, 3.8).
import { createHash, randomBytes } from "node:crypto";

/** A new lowercase RFC 9562 uuid v7 (ms timestamp + random). */
export function uuidv7(now = Date.now()): string {
  const b = randomBytes(16);
  b.writeUIntBE(now, 0, 6);
  b[6] = (b[6]! & 0x0f) | 0x70;
  b[8] = (b[8]! & 0x3f) | 0x80;
  const h = b.toString("hex");
  return `${h.slice(0, 8)}-${h.slice(8, 12)}-${h.slice(12, 16)}-${h.slice(16, 20)}-${h.slice(20)}`;
}

/** Whether a client-sent `Aviary-Request` may be kept: a lowercase uuid v7 of the RFC 4122 variant. */
export function isRequestId(v: string | undefined): boolean {
  return !!v && /^[0-9a-f]{8}-[0-9a-f]{4}-7[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/.test(v);
}

/** `person_key`: "u" + the first 24 hex digits of SHA-256 of the trimmed, lowercased email (never log the email). */
export function personKey(email: string | null | undefined): string | null {
  const e = String(email ?? "").trim().toLowerCase();
  return e ? "u" + createHash("sha256").update(e).digest("hex").slice(0, 24) : null;
}
