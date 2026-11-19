import { Buffer } from "buffer";
import type { CryptoPolyfillTarget } from "./install-crypto-polyfills";

// Ensure TextEncoder/TextDecoder exist for shared E2EE code (tweetnacl + relay transport).
// Hermes may not provide them in all configurations. On web (text-codec-fallback.ts) the browser has both.
export function installTextCodecFallback(target: CryptoPolyfillTarget): void {
  if (typeof target.TextEncoder !== "function") {
    class BufferTextEncoder {
      encode(input = ""): Uint8Array {
        return Uint8Array.from(Buffer.from(input, "utf8"));
      }
    }
    target.TextEncoder = BufferTextEncoder as unknown as typeof TextEncoder;
  }

  if (typeof target.TextDecoder !== "function") {
    class BufferTextDecoder {
      decode(input?: ArrayBuffer | ArrayBufferView): string {
        if (input == null) return "";
        if (input instanceof ArrayBuffer) {
          return Buffer.from(input).toString("utf8");
        }
        if (ArrayBuffer.isView(input)) {
          return Buffer.from(input.buffer, input.byteOffset, input.byteLength).toString("utf8");
        }
        return Buffer.from(String(input), "utf8").toString("utf8");
      }
    }
    target.TextDecoder = BufferTextDecoder as unknown as typeof TextDecoder;
  }
}
