import { isComposerRoute } from "@/yyc/early-input-routes";
import type { EarlyInputTarget } from "@/yyc/early-input";

export type { EarlyInputTarget } from "@/yyc/early-input";

/**
 * The early message box (Deyao, 2026-10-29; docs/paseo-ui.md "Loading"): public/index.html puts a plain textarea over
 * the page frame's message box before any script arrives, so people can type at once; send stays off until the app
 * has loaded. The first focused message box to mount takes its text over (this file), and the box goes away.
 */
interface EarlyTake {
  text: string;
  selStart: number;
  selEnd: number;
  base: string;
}

interface EarlyInputApi {
  take(path: string): EarlyTake | null;
  /** The live box's text without taking it (null once the box is gone). */
  peek(): EarlyTake | null;
  release(reason: string): void;
  ready(handoff: {
    element: HTMLElement | null;
    adopt: (focus?: boolean) => void;
    submit: () => void;
    preview: () => void;
  }): void;
  state(): { active: boolean; composing: boolean; focused: boolean };
  whenIdle(run: () => void): void;
}

function early(): EarlyInputApi | null {
  if (typeof window === "undefined") return null;
  return (window as { __yycEarly?: EarlyInputApi }).__yycEarly ?? null;
}

function hasFinePointer(): boolean {
  try {
    return window.matchMedia("(pointer: fine)").matches;
  } catch {
    return true;
  }
}

/**
 * The stored draft and the early text, together: the early text replaces the draft only when it was typed on top of
 * it (`base`), otherwise it goes after it on a new line, with the caret at the end.
 */
export function mergeEarlyText(
  stored: string,
  taken: EarlyTake,
): { text: string; selection: { start: number; end: number } } | null {
  if (taken.text === taken.base) return null;
  if (stored === taken.base || stored === "" || stored === taken.text) {
    return { text: taken.text, selection: { start: taken.selStart, end: taken.selEnd } };
  }
  // Already merged once (a phone's preview kept in the draft, then a reload): not twice
  if (stored.endsWith(`\n${taken.text}`)) {
    return { text: stored, selection: { start: stored.length, end: stored.length } };
  }
  const text = `${stored}\n${taken.text}`;
  return { text, selection: { start: text.length, end: text.length } };
}

function adoptNow(
  target: EarlyInputTarget,
  focus: boolean,
  focusIfTaken = true,
  stored?: string,
): void {
  const api = early();
  if (!api) return;
  const taken = api.take(window.location.pathname);
  const merged = taken ? mergeEarlyText(stored ?? target.getText(), taken) : null;
  if (merged) target.replaceText(merged.text, merged.selection);
  if (focus || (merged && focusIfTaken)) target.focus();
  api.release("adopted");
}

/**
 * Called by a focused message box once its draft has loaded. Desktop takes the text over at once. On a phone whose
 * keyboard is up in the early box, the swap waits for the next typed character (only then may a script move the
 * keyboard to the real box), a tap anywhere else or a blur; until then the early box sits over the real one, and the
 * real box already holds a copy of the text (no focus), so the app's own send button shows beside it. Returns a
 * cleanup for an unmount before that, which drops the box and keeps its text for this page.
 */
export function adoptEarlyInput(target: EarlyInputTarget): (() => void) | void {
  const api = early();
  if (!api) return;
  // Gone already (removed on a page without a message box): its text is kept for this page, or /new within 60 s
  if (!api.state().active) return adoptNow(target, false);
  let cancelled = false;
  let handed = false;
  api.whenIdle(() => {
    if (cancelled) return;
    const { focused } = api.state();
    if (focused && !hasFinePointer()) {
      handed = true;
      // The draft as it was stored, before any preview: the merge always starts from it
      const stored = target.getText();
      api.ready({
        element: target.element,
        // A tap elsewhere or a blur hands over without focus (no keyboard pulled back up)
        adopt: (focus = true) => adoptNow(target, focus, focus, stored),
        submit: target.submit,
        preview: () => {
          const live = api.peek();
          const merged = live ? mergeEarlyText(stored, live) : null;
          target.replaceText(merged ? merged.text : stored);
        },
      });
      return;
    }
    adoptNow(target, focused);
  });
  return () => {
    cancelled = true;
    // Its composer is gone (another conversation): the text stays in the mirror for this page only
    if (handed) api.release("composer-gone");
  };
}

/** A screen that has its own input (sign-in, a shared chat, the offline screen) removes the early box. */
export function releaseEarlyInput(reason: string): void {
  early()?.release(reason);
}

/** The route guard: a page without a message box removes the early box (its text stays for 60 s on /new). */
export function releaseEarlyInputIfNoComposer(pathname: string): void {
  if (!isComposerRoute(pathname)) early()?.release("no-composer");
}
