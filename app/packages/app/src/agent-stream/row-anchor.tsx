/**
 * How a row in the chat list keeps its top still while it grows (Deyao, 2026-11-20, chosen from mocks: a tool call
 * opens below its header, the content under it moving down). The phone's list is inverted (strategy-native.tsx), so a
 * row that grows pushes everything above it up, its own header too; the list gives each frame's change of height back
 * as scroll, on the UI thread, so the header stays where the finger was. Opening also stops following the newest text
 * (stop-following.ts, "opened-details"), which would otherwise undo it. Web has no list anchor (null).
 */
import { createContext, useContext } from "react";

export interface RowAnchor {
  /** Scroll by this frame's change of a row's height (a worklet, on the UI thread). */
  keepTop: (delta: number) => void;
}

export const RowAnchorContext = createContext<RowAnchor | null>(null);

export function useRowAnchor(): RowAnchor | null {
  return useContext(RowAnchorContext);
}
