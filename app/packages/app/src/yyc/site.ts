/**
 * The YYC AI server this app talks to (docs/paseo-ui.md). The web build is served by it, so it is the page's own
 * origin. The iOS build names it at build time (app.config.js `extra.yycSite`, from the YYC_SITE build secret: the
 * repo is public, so the address stays out of it).
 */
import Constants from "expo-constants";
import { isWeb } from "@/constants/platform";

/** e.g. https://example.org, without a trailing slash; null when unknown. */
export function siteOrigin(): string | null {
  if (isWeb) return typeof window === "undefined" ? null : window.location.origin;
  const site = (Constants.expoConfig?.extra as { yycSite?: unknown } | undefined)?.yycSite;
  return typeof site === "string" && site.trim() ? site.trim().replace(/\/+$/, "") : null;
}

/** The app's URL scheme, which the server's sign-in sends the Access token back to (iOS only). */
export function appScheme(): string | null {
  const scheme = Constants.expoConfig?.scheme;
  return typeof scheme === "string" ? scheme : Array.isArray(scheme) ? (scheme[0] ?? null) : null;
}
