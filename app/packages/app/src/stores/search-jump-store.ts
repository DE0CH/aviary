import { create } from "zustand";

/**
 * A message the conversation search opened (the palette's hit): the conversation's view jumps to it once its
 * timeline can be addressed, and takes it so it happens once.
 */
export interface SearchJump {
  agentId: string;
  /** The message's timeline cursor seq. */
  seq: number;
}

interface SearchJumpState {
  pending: SearchJump | null;
  request(jump: SearchJump): void;
  /** Takes the pending jump of `agentId` (null when there is none for it). */
  take(agentId: string): SearchJump | null;
}

export const useSearchJumpStore = create<SearchJumpState>((set, get) => ({
  pending: null,
  request: (jump) => set({ pending: jump }),
  take: (agentId) => {
    const pending = get().pending;
    if (pending?.agentId !== agentId) return null;
    set({ pending: null });
    return pending;
  },
}));
