import { nativeWebSocketFactory } from "@getpaseo/client/internal/daemon-client-websocket-transport";
import type { WebSocketFactory } from "@getpaseo/client/internal/daemon-client-transport-types";
import { accessHeaders } from "@/yyc/access-token";

/** The native socket carries the app's Cloudflare Access token (yyc/access-token.ts, docs/paseo-ui.md). */
export function createAppWebSocketFactory(): WebSocketFactory {
  return (url, options) =>
    nativeWebSocketFactory(url, { ...options, headers: { ...options?.headers, ...accessHeaders() } });
}
