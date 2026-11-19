/**
 * A deployment's branding (Deyao, 2026-11-13). The app is one build for every deployment; what names a deployment
 * to the eye is a set of files under `/branding/` of the server the app talks to. The bundle holds neutral ones
 * (`public/branding/`); a deployment puts its own in their place when it deploys the bundle (docs/paseo-ui.md
 * "Branding"). Its name is the server's too (yyc/product.ts).
 */
import { siteOrigin } from "@/yyc/site";

/** The files a deployment may replace, by name. */
export const BRAND_FILES = [
  /** The logo: square, transparent background, 512 px. In the app wherever it shows its mark. */
  "logo.png",
  /** The tab icon, and the tab icon while a reply runs and while a conversation waits for the person. */
  "favicon.png",
  "favicon-running.png",
  "favicon-attention.png",
  /** The home-screen icons (iOS: 180 px; the manifest's: 192 and 512 px, maskable). */
  "apple-touch-icon.png",
  "icon-192.png",
  "icon-512.png",
  /** The web app manifest: the name on the home screen, the colours, the two icons above. */
  "manifest.json",
] as const;

export type BrandFile = (typeof BRAND_FILES)[number];

/** Where a branding file is; null when the server is not known (a native build with no site). */
export function brandAssetUrl(file: BrandFile, origin: string | null = siteOrigin()): string | null {
  return origin ? `${origin}/branding/${file}` : null;
}

/** The tab icon for a state, as a path of the page's own origin. */
export function brandFaviconPath(status: "none" | "running" | "attention"): string {
  return status === "none" ? "/branding/favicon.png" : `/branding/favicon-${status}.png`;
}
