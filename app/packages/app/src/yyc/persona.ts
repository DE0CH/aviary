/**
 * The persona rig's hooks (aviary WP-54, DESIGN 9.9): only in a web build made with EXPO_PUBLIC_YYC_PERSONA=1
 * (jarvis/test/persona/build.sh), never in the staff build (the constant is false and the bundler drops the rest).
 * `window.__yycPersona` reports what the drivers cannot read from the page alone: how many times the app's socket
 * became connected (a move is exactly one reconnect). A seeded defect, `localStorage["yyc-persona-defect"]`, proves
 * the drivers catch what they claim to: `lost-draft` empties the composer whenever the socket (re)connects. The
 * count is of transitions into "connected" the client reports after its first connect; the drivers read differences.
 */
const ENABLED = process.env.EXPO_PUBLIC_YYC_PERSONA === "1";

interface PersonaState {
  connects: number;
  statuses: string[];
}

function state(): PersonaState | null {
  if (!ENABLED || typeof window === "undefined") return null;
  const w = window as unknown as { __yycPersona?: PersonaState };
  w.__yycPersona ??= { connects: 0, statuses: [] };
  return w.__yycPersona;
}

function defect(): string | null {
  try {
    return window.localStorage.getItem("yyc-persona-defect");
  } catch {
    return null;
  }
}

/** The composer's text box loses what was typed (the seeded `lost-draft` defect). */
function emptyComposer(): void {
  const box = document.querySelector<HTMLTextAreaElement>('[data-testid="message-input-root"] textarea');
  if (!box) return;
  const set = Object.getOwnPropertyDescriptor(HTMLTextAreaElement.prototype, "value")?.set;
  set?.call(box, "");
  box.dispatchEvent(new Event("input", { bubbles: true }));
}

/** Every connection status the active client reports (host-runtime's subscription). */
export function personaConnectionStatus(status: string): void {
  const s = state();
  if (!s) return;
  const last = s.statuses.at(-1);
  s.statuses.push(status);
  if (status !== "connected" || last === "connected") return;
  s.connects++;
  if (defect() === "lost-draft") setTimeout(emptyComposer, 0);
}
