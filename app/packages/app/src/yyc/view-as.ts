/**
 * View as (Deyao, 2026-10-28; DE0CH/claudecodeui jarvis/router.mjs "View as"): the operator page's 「View as」 opens
 * this app in a new tab at `/#jarvis-view-as=<token>`. The tab keeps the token in sessionStorage (this tab only; the
 * admin's other tabs stay themselves) and sends it as `?jarvis_view_as=` on every API call, file link and the socket,
 * so the router serves the person's own data. The tab is read-only (the send button and the new-conversation buttons
 * are disabled) until the admin switches on the override in the box only they see (`view-as-box.tsx`), which resets
 * on reload. The router checks the token against the caller's own Access identity and `admins` on every request; this
 * file only decides what the tab sends and shows.
 */
import { create } from "zustand";
import { isWeb } from "@/constants/platform";

export const VIEW_AS_PARAM = "jarvis_view_as";
const HASH_KEY = "jarvis-view-as";
const STORAGE_KEY = "yyc.view-as";

export interface ViewAs {
  token: string;
  /** The person's email. */
  user: string;
  /** The admin's email. */
  admin: string;
  /** When the token stops working (ms). */
  expiresAt: number;
}

/** The token's claims (not checked here: the router does that). */
export function parseViewAsToken(token: string): ViewAs | null {
  const body = token.split(".")[0];
  if (!body) return null;
  try {
    const json = atob(body.replace(/-/g, "+").replace(/_/g, "/"));
    const claims = JSON.parse(json) as { a?: unknown; u?: unknown; e?: unknown };
    if (typeof claims.a !== "string" || typeof claims.u !== "string" || typeof claims.e !== "number") return null;
    return { token, user: claims.u, admin: claims.a, expiresAt: claims.e * 1000 };
  } catch {
    return null;
  }
}

/** Reads the token once: from the address (then removed from it) or this tab's sessionStorage. */
function readInitial(): ViewAs | null {
  if (!isWeb || typeof window === "undefined") return null;
  let token: string | null = null;
  try {
    const hash = window.location.hash.replace(/^#/, "");
    const fromHash = new URLSearchParams(hash).get(HASH_KEY);
    if (fromHash) {
      token = fromHash;
      window.sessionStorage.setItem(STORAGE_KEY, fromHash);
      window.history.replaceState(window.history.state, "", window.location.pathname + window.location.search);
    } else {
      token = window.sessionStorage.getItem(STORAGE_KEY);
    }
  } catch {
    token = null;
  }
  return token ? parseViewAsToken(token) : null;
}

const initial = readInitial();

interface ViewAsState {
  viewAs: ViewAs | null;
  /** The admin switched on sending as the person (this page load only). */
  override: boolean;
  setOverride: (on: boolean) => void;
}

export const useViewAsStore = create<ViewAsState>((set) => ({
  viewAs: initial,
  override: false,
  setOverride: (on) => set({ override: on }),
}));

/** The tab's view-as token, or null in a normal tab. */
export function viewAsToken(): string | null {
  return useViewAsStore.getState().viewAs?.token ?? null;
}

/** The URL (absolute or a path) with the tab's view-as token added; unchanged in a normal tab. */
export function withViewAs(url: string): string {
  const token = viewAsToken();
  if (!token) return url;
  const hashAt = url.indexOf("#");
  const base = hashAt >= 0 ? url.slice(0, hashAt) : url;
  const hash = hashAt >= 0 ? url.slice(hashAt) : "";
  return `${base}${base.includes("?") ? "&" : "?"}${VIEW_AS_PARAM}=${encodeURIComponent(token)}${hash}`;
}

/** Viewing as someone with the override off: nothing may be sent or started. */
export function useViewAsReadOnly(): boolean {
  return useViewAsStore((state) => state.viewAs !== null && !state.override);
}

export function viewAsReadOnly(): boolean {
  const state = useViewAsStore.getState();
  return state.viewAs !== null && !state.override;
}
