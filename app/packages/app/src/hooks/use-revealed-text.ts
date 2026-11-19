export type MarkdownPhase = "streaming" | "complete";

/**
 * The text of a streaming message to draw: all of it, as it has arrived (Deyao, 2026-11-13: the streaming is not
 * faked). The model's text reaches the app in pieces as it is written; each piece is on screen when it lands, with
 * nothing typed out over time. The name and the `phase` argument are what plugins were given (plugins/evaluate.ts).
 */
export function useRevealedText(text: string, _phase: MarkdownPhase): string {
  return text;
}
