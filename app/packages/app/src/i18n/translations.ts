import type { Resource } from "i18next";
import { en as paseoEn, type TranslationResources } from "./resources/en";
import { enOverrides, withOverrides, withYycBrand, zhHKOverrides } from "./resources/yyc";

// Native (one bundle) and unit tests: English is there from the start and zh-HK is read synchronously when
// selected, so translations are ready before the language provider renders. The web has its own file
// (translations.web.ts): only the page's language, fetched as its own chunk.

// Paseo's text with YYC AI's wording on top and its name throughout (resources/yyc.ts)
const en = withYycBrand(withOverrides(paseoEn, enOverrides));

export const INITIAL_LANGUAGE = "en";
export const FALLBACK_LANGUAGE: string | false = "en";
export const INITIAL_RESOURCES: Resource | undefined = { en: { translation: en } };

export function readTranslations(
  language: string,
): TranslationResources | undefined | Promise<TranslationResources | undefined> {
  switch (language) {
    case "en":
      return en;
    case "zh-HK":
      // A literal require keeps zh-HK bundled for offline use, evaluated only when selected
      return withYycBrand(withOverrides(require("./resources/zh-HK").zhHK, zhHKOverrides));
    default:
      return undefined;
  }
}
