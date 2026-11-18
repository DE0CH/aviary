import { create } from "zustand";
import type { DaemonClient } from "@getpaseo/client/internal/daemon-client";

/**
 * Conversations whose sandbox is being made (DE0CH/claudecodeui docs/paseo-ui.md): the server's
 * `yyc.sandbox_status` messages, kept per host and agent so the chat can show "preparing the workspace" from the
 * moment the server says so, even before the chat screen has mounted. A start that waits for its person's
 * approval elsewhere (`waiting: "approval"`) shows "waiting for your approval" instead.
 */

/** What the chip says while the sandbox is made. */
export type SandboxWaiting = "approval" | null;

export interface SandboxPreparing {
  /** When the chip first showed (its "this can take a minute or two" hint counts from here). */
  since: number;
  waiting: SandboxWaiting;
}

type Preparing = Record<string, SandboxPreparing>;

interface SandboxStatusState {
  preparing: Preparing;
  apply: (key: string, status: SandboxStatus, now: number) => void;
}

export interface SandboxStatus {
  state: "preparing" | "ready";
  waiting?: string;
}

/**
 * The chips after one status message: `ready` ends the conversation's chip; `preparing` shows it, saying what it
 * waits for (a value the app does not know reads as plain preparing). The chip's clock starts when the sandbox
 * starts being made, or when it stops waiting for an approval: the hint is about the making, which can take a minute
 * or two, not about the person's answer. Unchanged state is answered as the same object.
 */
export function nextPreparing(state: Preparing, key: string, status: SandboxStatus, now: number): Preparing {
  const current = state[key];
  if (status.state === "ready") {
    if (!current) return state;
    const next = { ...state };
    delete next[key];
    return next;
  }
  const waiting: SandboxWaiting = status.waiting === "approval" ? "approval" : null;
  if (current && current.waiting === waiting) return state;
  const since = current && current.waiting === null ? current.since : now;
  return { ...state, [key]: { since, waiting } };
}

const useSandboxStatusStore = create<SandboxStatusState>((set) => ({
  preparing: {},
  apply: (key, status, now) =>
    set((state) => {
      const preparing = nextPreparing(state.preparing, key, status, now);
      return preparing === state.preparing ? state : { preparing };
    }),
}));

function statusKey(serverId: string, agentId: string): string {
  return `${serverId}:${agentId}`;
}

/** The chip of a conversation whose sandbox is being made, or null when it is not waiting for one. */
export function useSandboxPreparing(serverId: string, agentId: string | null): SandboxPreparing | null {
  return useSandboxStatusStore((state) =>
    agentId ? (state.preparing[statusKey(serverId, agentId)] ?? null) : null,
  );
}

export function mountSandboxStatusHandler(client: DaemonClient, serverId: string): () => void {
  return client.on("yyc.sandbox_status", (message) => {
    useSandboxStatusStore
      .getState()
      .apply(statusKey(serverId, message.payload.agentId), message.payload, Date.now());
  });
}
