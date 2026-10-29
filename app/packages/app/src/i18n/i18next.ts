import { createInstance, type BackendModule } from "i18next";
import { initReactI18next } from "react-i18next";
import { observeI18nInit } from "./init";
import {
  FALLBACK_LANGUAGE,
  INITIAL_LANGUAGE,
  INITIAL_RESOURCES,
  readTranslations,
} from "./translations";

// The translations come from ./translations: on native both languages are in the bundle and read synchronously,
// so they are ready before the language provider renders; on the web (translations.web.ts) only the page's
// language is fetched, and index.ts renders the app once `i18nReady` has settled.
const bundledTranslations: BackendModule = {
  type: "backend",
  init() {},
  read(language, _namespace, callback) {
    const result = readTranslations(language);
    if (result instanceof Promise) {
      result.then(
        (translation) => callback(null, translation),
        (error: unknown) => callback(error instanceof Error ? error : new Error(String(error)), null),
      );
      return;
    }
    callback(null, result);
  },
};

const i18n = createInstance();

const initPromise = i18n
  .use(bundledTranslations)
  .use(initReactI18next)
  .init({
    compatibilityJSON: "v4",
    fallbackLng: FALLBACK_LANGUAGE,
    lng: INITIAL_LANGUAGE,
    partialBundledLanguages: true,
    resources: INITIAL_RESOURCES,
    interpolation: {
      escapeValue: false,
    },
    react: {
      useSuspense: false,
    },
  });
observeI18nInit(initPromise);

/** Settles once the first language's text is there (at once on native). */
export const i18nReady: Promise<void> = initPromise.then(
  () => undefined,
  () => undefined,
);

export { i18n };
