import type { Resource } from "i18next";
import type { TranslationResources } from "./resources/en";
import { enOverrides, withOverrides, withYycBrand, zhHKOverrides } from "./resources/yyc";
import { readInitialWebLocale } from "./initial-locale";
import { isChunkLoadError, loadWithRetry } from "../utils/lazy-with-retry";

// The web page fetches only its own language, as its own chunk, from the moment the entry runs, alongside the
// connection; index.ts renders the app once it has arrived (the early box is typable meanwhile). Another language is
// fetched when chosen, and the page keeps the current one until it lands. A failed fetch goes through loadWithRetry
// (retries, then one page reload, which covers a deploy that replaced the build) and is then tried again with a
// capped backoff until it arrives, so the app never renders without its text. Every language has every key
// (resources.test.ts); the fallback is the language loaded at start, already in the store, so a switch that fails
// anyway shows that language rather than raw keys (docs/paseo-ui.md "Loading").

export const INITIAL_LANGUAGE = readInitialWebLocale();
export const FALLBACK_LANGUAGE: string | false = INITIAL_LANGUAGE;
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
      return await loadWithRetry(load);
    } catch (error) {
      // An error the chunk threw while it evaluated is a bug: fetching it again cannot help
      if (!isChunkLoadError(error)) throw error;
      await new Promise((resolve) => setTimeout(resolve, Math.min(30_000, 1000 * 2 ** attempt)));
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
