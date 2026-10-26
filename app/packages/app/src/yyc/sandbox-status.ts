import { create } from "zustand";
import type { DaemonClient } from "@getpaseo/client/internal/daemon-client";

/**
 * Conversations whose sandbox is being made (DE0CH/claudecodeui docs/paseo-ui.md): the server's
 * `yyc.sandbox_status` messages, kept per host and agent so the chat can show "preparing the workspace" from the
 * moment the server says so, even before the chat screen has mounted.
 */
interface SandboxStatusState {
  preparing: Record<string, number>;
  set: (key: string, preparing: boolean) => void;
}

const useSandboxStatusStore = create<SandboxStatusState>((set) => ({
  preparing: {},
  set: (key, preparing) =>
    set((state) => {
      if (preparing === key in state.preparing) return state;
      const next = { ...state.preparing };
      if (preparing) next[key] = Date.now();
      else delete next[key];
      return { preparing: next };
    }),
}));

function statusKey(serverId: string, agentId: string): string {
  return `${serverId}:${agentId}`;
}

/** When the sandbox started being made, or null when the conversation is not waiting for one. */
export function useSandboxPreparingSince(serverId: string, agentId: string | null): number | null {
  return useSandboxStatusStore((state) =>
    agentId ? (state.preparing[statusKey(serverId, agentId)] ?? null) : null,
  );
}

export function mountSandboxStatusHandler(client: DaemonClient, serverId: string): () => void {
  return client.on("yyc.sandbox_status", (message) => {
    useSandboxStatusStore
      .getState()
      .set(statusKey(serverId, message.payload.agentId), message.payload.state === "preparing");
  });
}
