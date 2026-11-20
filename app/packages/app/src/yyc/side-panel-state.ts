export type SidePanelKind = "activity" | "schedules";

/**
 * A conversation's panel is on screen when it is the one chosen and, on a phone, the right-hand overlay is out (a
 * swipe or the backdrop puts it away without telling the chip).
 */
export function sidePanelOpen(input: {
  shown: { sessionId: string; kind: SidePanelKind } | null;
  sessionId: string | null;
  kind: SidePanelKind;
  compact: boolean;
  overlayOut: boolean;
}): boolean {
  const chosen =
    !!input.sessionId &&
    input.shown?.sessionId === input.sessionId &&
    input.shown.kind === input.kind;
  return chosen && (!input.compact || input.overlayOut);
}
