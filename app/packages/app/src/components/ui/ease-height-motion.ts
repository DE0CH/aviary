/**
 * How something at the bottom of the chat on a phone changes height (Deyao, 2026-11-20, chosen from mocks): its height
 * eases and its content stays pinned to one edge (components/ui/ease-height.tsx), so nothing jumps. The decision for
 * one layout change is here, alone; the animation is the component's.
 */

export type HeightCurve = "ease" | "follow" | "open";

export type HeightMotion =
  | { kind: "set"; to: number }
  | { kind: "timing"; to: number; duration: number }
  | { kind: "spring"; to: number; spring: "follow" | "open" };

export const EASE_DURATION_MS = 240;

/** The follow spring keeps up with a stream without overshoot; the open spring settles a little, as iOS's disclosure. */
export const SPRINGS = {
  follow: { damping: 26, stiffness: 220, mass: 1, overshootClamping: true },
  open: { damping: 22, stiffness: 260, mass: 1, overshootClamping: false },
} as const;

export interface HeightChange {
  /** The content's height, or 0 for an item that is going. */
  to: number;
  /** Nothing has been shown yet: the first layout. */
  first: boolean;
  /** A new item eases in from zero; otherwise its first layout shows at full height (history, a remount). */
  fromZero: boolean;
  curve: HeightCurve;
  reduceMotion: boolean;
}

export function heightMotion(change: HeightChange): HeightMotion {
  if (change.reduceMotion || (change.first && !change.fromZero)) return { kind: "set", to: change.to };
  if (change.curve === "ease") return { kind: "timing", to: change.to, duration: EASE_DURATION_MS };
  return { kind: "spring", to: change.to, spring: change.curve };
}

export interface Shown<T> {
  item: T;
  leaving: boolean;
}

/**
 * The items to draw: the current ones in order, and one that went kept where it was, marked leaving, so it can ease
 * out before it is dropped (its row says when).
 */
export function withLeaving<T>(shown: Shown<T>[], current: T[], key: (item: T) => string): Shown<T>[] {
  const now = new Map(current.map((item) => [key(item), item]));
  const out: Shown<T>[] = [];
  const placed = new Set<string>();
  let next = 0;
  for (const entry of shown) {
    const k = key(entry.item);
    const still = now.get(k);
    if (still === undefined) {
      out.push({ item: entry.item, leaving: true });
      continue;
    }
    // current items up to and including this one, in their order
    while (next < current.length) {
      const item = current[next++]!;
      const ik = key(item);
      if (placed.has(ik)) continue;
      placed.add(ik);
      out.push({ item, leaving: false });
      if (ik === k) break;
    }
  }
  for (; next < current.length; next++) {
    const item = current[next]!;
    if (!placed.has(key(item))) out.push({ item, leaving: false });
  }
  return out;
}
