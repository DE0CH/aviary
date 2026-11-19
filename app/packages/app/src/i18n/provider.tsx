import * as Localization from "expo-localization";
import { type ReactNode, useMemo } from "react";
import { I18nextProvider } from "react-i18next";
import { isWeb } from "@/constants/platform";
import { useAppSettings } from "@/hooks/use-settings";
import { i18n } from "./i18next";
import { resolveSupportedLocale } from "./locales";
import { ensureI18nLanguageForRender } from "./sync-language";

interface I18nProviderProps {
  children: ReactNode;
}

function getSystemLocales(): string[] {
  if (isWeb && typeof navigator !== "undefined" && navigator.languages.length > 0) {
    return [...navigator.languages];
  }

  return Localization.getLocales().map((locale) => locale.languageTag);
}

export function I18nProvider({ children }: I18nProviderProps) {
  const { settings, isLoading } = useAppSettings();
  const systemLocales = useMemo(() => getSystemLocales(), []);
  const locale = resolveSupportedLocale(settings.language, systemLocales);

  // On the web the page starts in the saved language, read before the first render (i18n/translations.web.ts); the
  // settings' defaults while they load would fetch the other language for nothing
  if (!(isWeb && isLoading)) ensureI18nLanguageForRender(locale, i18n);

  return <I18nextProvider i18n={i18n}>{children}</I18nextProvider>;
}
