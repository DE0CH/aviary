// Native has no early message box (it is public/index.html's, on web); see early-input.web.ts.
export interface EarlyInputTarget {
  element: HTMLElement | null;
  getText: () => string;
  /** Without a selection it only sets the text (no caret move, no focus). */
  replaceText: (text: string, selection?: { start: number; end: number }) => void;
  focus: () => void;
  /** Sends as the app's send button would, or does nothing while send is off. */
  submit: () => void;
}

export function adoptEarlyInput(_target: EarlyInputTarget): (() => void) | void {}

export function releaseEarlyInput(_reason: string): void {}

export function releaseEarlyInputIfNoComposer(_pathname: string): void {}
