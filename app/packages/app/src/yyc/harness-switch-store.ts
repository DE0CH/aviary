import { create } from "zustand";

// A model pick that moves a conversation to another harness (yyc/model-menu.tsx): the question asked first, and
// the conversations whose next reply begins by summing up what came before (docs/paseo-ui.md).

/** How long a confirmed move waits for its turn before it is forgotten (the person never sent a message). */
export const MOVE_WAITS_MS = 30 * 60_000;

export interface HarnessSwitchQuestion {
  agentId: string;
  /** The model picked, as the menu names it. */
  modelLabel: string;
  /** Makes the pick. */
  confirm: () => void;
}

/** A conversation that moved: waiting for its next turn, or in it. */
export interface Move {
  phase: "waiting" | "running";
  at: number;
}

/** A move after the conversation's turn was seen running or not; `null` once it is over. */
export function moveAfter(move: Move | null, running: boolean, now: number): Move | null {
  if (!move) return null;
  if (running) return move.phase === "running" ? move : { phase: "running", at: move.at };
  if (move.phase === "running") return null;
  return now - move.at > MOVE_WAITS_MS ? null : move;
}

export const useHarnessSwitchStore = create<{
  question: HarnessSwitchQuestion | null;
  moves: Record<string, Move>;
  ask: (question: HarnessSwitchQuestion) => void;
  answer: (confirmed: boolean) => void;
  seen: (agentId: string, running: boolean) => void;
}>((set, get) => ({
  question: null,
  moves: {},
  ask: (question) => set({ question }),
  answer: (confirmed) => {
    const question = get().question;
    if (!question) return;
    if (!confirmed) {
      set({ question: null });
      return;
    }
    set((state) => ({
      question: null,
      moves: { ...state.moves, [question.agentId]: { phase: "waiting", at: Date.now() } },
    }));
    question.confirm();
  },
  seen: (agentId, running) => {
    const before = get().moves[agentId] ?? null;
    const after = moveAfter(before, running, Date.now());
    if (after === before) return;
    set((state) => {
      const moves = { ...state.moves };
      if (after) moves[agentId] = after;
      else delete moves[agentId];
      return { moves };
    });
  },
}));
