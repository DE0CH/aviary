/**
 * The Android app's download in Settings → General (docs/paseo-ui.md "The Android app"): each deployment serves its own
 * signed APK at /apps/android.apk on its own host, from Cloudflare's edge (an R2 bucket behind a Worker and the
 * deployment's Access login; CI replaces it after every green build on main). Shown on Android only: the site in an
 * Android browser, and the Android app itself, where it fetches the newest build.
 */
export const ANDROID_APK_PATH = "/apps/android.apk";

export interface AndroidAppContext {
  /** React Native's Platform.OS */
  platform: string;
  /** the browser's user agent (web only) */
  userAgent: string | null;
  /** the deployment's origin (yyc/site.ts siteOrigin) */
  origin: string | null;
}

/** The APK's URL where the row shows, else null. */
export function androidAppDownloadUrl({ platform, userAgent, origin }: AndroidAppContext): string | null {
  if (!origin) return null;
  const android = platform === "android" || (platform === "web" && /\bAndroid\b/.test(userAgent ?? ""));
  return android ? `${origin}${ANDROID_APK_PATH}` : null;
}
