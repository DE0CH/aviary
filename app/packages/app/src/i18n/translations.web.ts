import type { Resource } from "i18next";
import type { TranslationResources } from "./resources/en";
import { enOverrides, withOverrides, withYycBrand, zhHKOverrides } from "./resources/yyc";
import { readInitialWebLocale } from "./initial-locale";

// The web page fetches only its own language, as its own chunk, from the moment the entry runs, alongside the
// connection; index.ts renders the app once it has arrived (the early box is typable meanwhile). Another language is
// fetched when chosen, and the page keeps the current one until it lands. Every language has every key
// (resources.test.ts), so there is no English fallback to fetch (docs/paseo-ui.md "Loading").

export const INITIAL_LANGUAGE = readInitialWebLocale();
export const FALLBACK_LANGUAGE: string | false = false;
export const INITIAL_RESOURCES: Resource | undefined = undefined;

function mark(language: string): void {
  try {
    performance.mark(`yyc:chunk:language-${language}`);
  } catch {
    // performance marks are for load-timing only
  }
}

async function withRetry<T>(load: () => Promise<T>): Promise<T> {
  for (let attempt = 0; ; attempt++) {
    try {
      return await load();
    } catch (error) {
      if (attempt >= 3) throw error;
      await new Promise((resolve) => setTimeout(resolve, 1000 * 2 ** attempt));
    }
  }
}

export function readTranslations(language: string): Promise<TranslationResources | undefined> {
  switch (language) {
    case "en":
      return withRetry(() => import("./resources/en")).then((module) => {
        mark(language);
        return withYycBrand(withOverrides(module.en, enOverrides));
      });
    case "zh-HK":
      return withRetry(() => import("./resources/zh-HK")).then((module) => {
        mark(language);
        return withYycBrand(withOverrides(module.zhHK, zhHKOverrides));
      });
    default:
      return Promise.resolve(undefined);
  }
}
