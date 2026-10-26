/**
 * The iOS app's Cloudflare Access token (docs/paseo-ui.md). The browser has Access's cookie; the app gets the same
 * token from the router's sign-in (GET /api/jarvis/app-signin, which redirects to `<scheme>://signed-in#token=…`),
 * keeps it in the Keychain and sends it as the cf-access-token header on REST and the WebSocket. It lasts 720 h;
 * an answer of 401/403 means signing in again.
 */
import * as SecureStore from "expo-secure-store";
import { isWeb } from "@/constants/platform";

const KEY = "yyc.access-token";
let token: string | null = null;
const listeners = new Set<() => void>();

export function accessToken(): string | null {
  return token;
}

/** Reads the saved token once at start (native only). */
export async function loadAccessToken(): Promise<string | null> {
  if (isWeb) return null;
  try {
    token = (await SecureStore.getItemAsync(KEY)) ?? null;
  } catch {
    token = null;
  }
  return token;
}

export async function saveAccessToken(value: string | null): Promise<void> {
  token = value;
  if (!isWeb) {
    try {
      if (value) await SecureStore.setItemAsync(KEY, value);
      else await SecureStore.deleteItemAsync(KEY);
    } catch {
      // the token stays in memory for this launch
    }
  }
  for (const listener of listeners) listener();
}

export function subscribeAccessToken(listener: () => void): () => void {
  listeners.add(listener);
  return () => listeners.delete(listener);
}

/**
 * Headers that carry the token, for everything the app sends itself (empty on the web: the cookie does it). Both
 * forms, as the SwiftUI app sent them: Access at the edge reads either, the router the cookie.
 */
export function accessHeaders(): Record<string, string> {
  return token ? { "cf-access-token": token, Cookie: `CF_Authorization=${token}` } : {};
}
