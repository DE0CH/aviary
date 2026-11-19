// Base64 and UTF-8 helpers on the engine's own atob/btoa (web and Hermes), so the entry carries no Buffer
// polyfill (docs/paseo-ui.md "Loading").
import { decodeUtf8 } from "@/utils/decode-utf8";

const CHUNK = 0x8000;

export function base64ToBytes(base64: string): Uint8Array<ArrayBuffer> {
  const binary = atob(base64);
  const bytes = new Uint8Array(binary.length);
  for (let index = 0; index < binary.length; index++) bytes[index] = binary.charCodeAt(index);
  return bytes;
}

export function bytesToBase64(bytes: Uint8Array): string {
  let binary = "";
  for (let index = 0; index < bytes.length; index += CHUNK) {
    binary += String.fromCharCode.apply(
      null,
      bytes.subarray(index, index + CHUNK) as unknown as number[],
    );
  }
  return btoa(binary);
}

export function utf8ToBase64(text: string): string {
  return bytesToBase64(new TextEncoder().encode(text));
}

export function base64ToUtf8(base64: string): string {
  return decodeUtf8(base64ToBytes(base64));
}

export function utf8ByteLength(text: string): number {
  let length = 0;
  for (let index = 0; index < text.length; index++) {
    const code = text.charCodeAt(index);
    if (code < 0x80) length += 1;
    else if (code < 0x800) length += 2;
    else if (code >= 0xd800 && code <= 0xdbff && index + 1 < text.length) {
      const next = text.charCodeAt(index + 1);
      if (next >= 0xdc00 && next <= 0xdfff) {
        length += 4;
        index++;
      } else length += 3;
    } else length += 3;
  }
  return length;
}
