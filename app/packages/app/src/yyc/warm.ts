/**
 * Typing warms a sandbox host (DE0CH/jarvis3 docs/CLAUDE-CODE.md, "Warming"): with no host up, a conversation's first
 * message waits about two minutes for one to boot; a signal sent while the person types lets the scaler start it
 * early. At most one signal per conversation every 30 s (a new chat counts as one conversation). Fire and forget.
 */
import { api, sessionIdOfAgent } from "@/yyc/api";

const WARM_INTERVAL_MS = 30_000;
const lastSent = new Map<string, number>();

export function warmSandbox(agentId: string | null): void {
  const sessionId = sessionIdOfAgent(agentId);
  const key = sessionId ?? "";
  const now = Date.now();
  const previous = lastSent.get(key);
  if (previous !== undefined && now - previous < WARM_INTERVAL_MS) return;
  lastSent.set(key, now);
  void api("/api/jarvis/warm", {
    method: "POST",
    body: JSON.stringify(sessionId ? { sessionId } : {}),
  }).catch(() => undefined);
}
