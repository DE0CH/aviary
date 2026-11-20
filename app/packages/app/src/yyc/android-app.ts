/**
 * The Android app's download in Settings → General (app/docs/paseo-ui.md "The Android app"): each deployment serves its
 * own signed APK at /apps/android.apk on its own host, from Cloudflare's edge (an R2 bucket behind a Worker and the
 * deployment's Access login; CI replaces it after every green build on main). Shown on every device (Deyao,
 * 2026-11-20: telling an Android phone from its user agent is not reliable), so it can also be fetched on a computer.
 */
export const ANDROID_APK_PATH = "/apps/android.apk";

/** The APK's URL, or null without a known server (the row is then left out). */
export function androidAppDownloadUrl({ origin }: { origin: string | null }): string | null {
  return origin ? `${origin}${ANDROID_APK_PATH}` : null;
}
