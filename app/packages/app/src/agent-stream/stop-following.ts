/**
 * The reasons the chat stops following the newest text (Deyao, 2026-11-20: one category, each reason a hint of it):
 * the person scrolled away from the bottom, opened a turn, or opened a row's details (a tool call). The list then holds
 * what is on screen; the ↓ button or a scroll back to the bottom follows again.
 */
import { createContext, useContext } from "react";

export type StopFollowingHint =
  | "scrolled-away"
  | "opened-turn"
  | "opened-details";

/** A row in the chat gives its hint here; the chat passes it to the list (StreamViewportHandle.stopFollowing). */
export const StopFollowingContext = createContext<
  ((hint: StopFollowingHint) => void) | null
>(null);

export function useStopFollowing(): ((hint: StopFollowingHint) => void) | null {
  return useContext(StopFollowingContext);
}
