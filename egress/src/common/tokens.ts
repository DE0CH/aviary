// Hashing of bearer tokens and cache keys.
import { createHash } from "node:crypto";

export const sha256hex = (s: string | Uint8Array): string => createHash("sha256").update(s).digest("hex");
