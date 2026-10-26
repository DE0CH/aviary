import { createInstance, type BackendModule } from "i18next";
import { initReactI18next } from "react-i18next";
import { observeI18nInit } from "./init";
import { en as paseoEn } from "./resources/en";
import { zhHK as paseoZhHK } from "./resources/zh-HK";
import { enOverrides, withOverrides, zhHKOverrides } from "./resources/yyc";

const en = withOverrides(paseoEn, enOverrides);
const zhHK = withOverrides(paseoZhHK, zhHKOverrides);

// Both shipped locales are bundled and loaded up front. Synchronous reads preserve the language
// provider's contract: translations are ready before it renders children.
const bundledTranslations: BackendModule = {
  type: "backend",
  init() {},
  read(language, _namespace, callback) {
    switch (language) {
      case "en":
        callback(null, en);
        return;
      case "zh-HK":
        callback(null, zhHK);
        return;
      default:
        callback(null, undefined);
    }
  },
};

const i18n = createInstance();

observeI18nInit(
  i18n
    .use(bundledTranslations)
    .use(initReactI18next)
    .init({
      compatibilityJSON: "v4",
      fallbackLng: "en",
      lng: "zh-HK",
      partialBundledLanguages: true,
      resources: {
        en: { translation: en },
        "zh-HK": { translation: zhHK },
      },
      interpolation: {
        escapeValue: false,
      },
      react: {
        useSuspense: false,
      },
    }),
);

export { i18n };
