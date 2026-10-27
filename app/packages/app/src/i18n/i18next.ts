import { createInstance, type BackendModule } from "i18next";
import { initReactI18next } from "react-i18next";
import { observeI18nInit } from "./init";
import { en as paseoEn } from "./resources/en";
import { enOverrides, withOverrides, withYycBrand, zhHKOverrides } from "./resources/yyc";

// Paseo's text with YYC AI's wording on top and its name throughout (resources/yyc.ts)
const en = withYycBrand(withOverrides(paseoEn, enOverrides));

// A literal require keeps zh-HK bundled for offline use, while evaluating its resource module only
// when selected. Synchronous reads preserve the language provider's contract: translations are
// ready before it renders children.
const bundledTranslations: BackendModule = {
  type: "backend",
  init() {},
  read(language, _namespace, callback) {
    switch (language) {
      case "en":
        callback(null, en);
        return;
      case "zh-HK":
        callback(null, withYycBrand(withOverrides(require("./resources/zh-HK").zhHK, zhHKOverrides)));
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
      lng: "en",
      partialBundledLanguages: true,
      resources: {
        en: { translation: en },
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
