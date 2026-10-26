// YYC AI is a Hong Kong product (DE0CH/claudecodeui docs/paseo-ui.md): Traditional Chinese (Hong Kong) first, English
// for whoever prefers it. Paseo's other locales are not shipped.
export type SupportedLocale = "en" | "zh-HK";
export type AppLanguage = "system" | SupportedLocale;

export interface LanguageOption {
  value: AppLanguage;
  labelKey: string;
}

export const DEFAULT_LOCALE: SupportedLocale = "zh-HK";

export const LANGUAGE_OPTIONS: LanguageOption[] = [
  { value: "system", labelKey: "settings.general.language.options.system" },
  { value: "zh-HK", labelKey: "settings.general.language.options.zhHK" },
  { value: "en", labelKey: "settings.general.language.options.en" },
];

const SUPPORTED_LANGUAGES = new Set<AppLanguage>(["system", "en", "zh-HK"]);
const LANGUAGE_NATIVE_NAMES: Record<SupportedLocale, string> = {
  en: "English",
  "zh-HK": "繁體中文（香港）",
};
const LANGUAGE_NAMES_BY_LOCALE: Record<SupportedLocale, Record<SupportedLocale, string>> = {
  en: {
    en: "English",
    "zh-HK": "Traditional Chinese (Hong Kong)",
  },
  "zh-HK": {
    en: "英文",
    "zh-HK": "繁體中文（香港）",
  },
};

export function parseAppLanguage(value: unknown): AppLanguage | null {
  return typeof value === "string" && SUPPORTED_LANGUAGES.has(value as AppLanguage)
    ? (value as AppLanguage)
    : null;
}

export function formatLanguageOptionLabel(
  option: LanguageOption,
  activeLocale: SupportedLocale,
  systemLabel: string,
): string {
  if (option.value === "system") {
    return systemLabel;
  }

  const nativeName = LANGUAGE_NATIVE_NAMES[option.value];
  const activeLanguageName = LANGUAGE_NAMES_BY_LOCALE[activeLocale][option.value];
  if (nativeName === activeLanguageName) {
    return nativeName;
  }

  return `${nativeName} - ${activeLanguageName}`;
}

export function resolveSupportedLocale(
  language: AppLanguage,
  systemLocales: readonly string[],
): SupportedLocale {
  if (language !== "system") {
    return language;
  }

  // Any Chinese and anything unrecognised gets zh-HK; only an English system locale gets English.
  for (const locale of systemLocales) {
    const baseLanguage = locale.toLowerCase().split("-", 1)[0];
    if (baseLanguage === "zh") return "zh-HK";
    if (baseLanguage === "en") return "en";
  }

  return DEFAULT_LOCALE;
}
