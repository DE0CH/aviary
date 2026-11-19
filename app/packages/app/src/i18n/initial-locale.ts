import { APP_SETTINGS_KEY, LEGACY_SETTINGS_KEY } from "@/hooks/use-settings/keys";
import { parseAppLanguage, resolveSupportedLocale, type SupportedLocale } from "./locales";

/**
 * The language the web page starts in, read synchronously from the saved settings (as public/index.html's early box
 * does), so the entry can fetch that one language while the connection starts (docs/paseo-ui.md "Loading").
 */
export function readInitialWebLocale(): SupportedLocale {
  let language: unknown = null;
  for (const key of [APP_SETTINGS_KEY, LEGACY_SETTINGS_KEY]) {
    try {
      const raw = window.localStorage.getItem(key);
      const parsed = raw ? (JSON.parse(raw) as { language?: unknown } | null) : null;
      if (parsed && typeof parsed === "object") {
        language = parsed.language;
        break;
      }
    } catch {
      // blocked or malformed storage: the system language
    }
  }
  const languages =
    typeof navigator !== "undefined" && navigator.languages?.length
      ? [...navigator.languages]
      : [typeof navigator !== "undefined" ? navigator.language : ""];
  return resolveSupportedLocale(parseAppLanguage(language) ?? "system", languages);
}
