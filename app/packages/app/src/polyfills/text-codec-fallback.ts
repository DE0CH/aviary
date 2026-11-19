import type { CryptoPolyfillTarget } from "./install-crypto-polyfills";

// Every browser the web app supports has TextEncoder and TextDecoder, so the web build carries no Buffer-based
// fallback (text-codec-fallback.native.ts is the native one; docs/paseo-ui.md "Loading").
export function installTextCodecFallback(_target: CryptoPolyfillTarget): void {}
