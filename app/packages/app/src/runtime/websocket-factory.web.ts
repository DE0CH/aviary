import { defaultWebSocketFactory } from "@getpaseo/client/internal/daemon-client-websocket-transport";
import type { WebSocketFactory } from "@getpaseo/client/internal/daemon-client-transport-types";
import { withViewAs } from "@/yyc/view-as";

/** The page's socket; a view-as tab adds its token (yyc/view-as.ts). */
export function createAppWebSocketFactory(): WebSocketFactory {
  return (url, options) => defaultWebSocketFactory(withViewAs(url), options);
}
