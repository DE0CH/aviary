import { describe, expect, it, vi } from "vitest";
import { ensureI18nLanguageForRender } from "./sync-language";

describe("ensureI18nLanguageForRender", () => {
  it("changes the i18n language before callers render children", () => {
    const calls: string[] = [];
    const i18n = {
      language: "en",
      changeLanguage: (locale: string) => {
        calls.push(locale);
        i18n.language = locale;
        return Promise.resolve();
      },
    };

    ensureI18nLanguageForRender("zh-HK", i18n);

    expect(i18n.language).toBe("zh-HK");
    expect(calls).toEqual(["zh-HK"]);
  });

  it("does not call changeLanguage when the current language already matches", () => {
    const changeLanguage = vi.fn<() => Promise<void>>().mockResolvedValue(undefined);

    ensureI18nLanguageForRender("en", {
      language: "en",
      changeLanguage,
    });

    expect(changeLanguage).not.toHaveBeenCalled();
  });
});
