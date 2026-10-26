import { describe, expect, it } from "vitest";
import {
  LANGUAGE_OPTIONS,
  formatLanguageOptionLabel,
  parseAppLanguage,
  resolveSupportedLocale,
} from "./locales";

describe("parseAppLanguage", () => {
  it("accepts system and the shipped locales", () => {
    expect(["system", "en", "zh-HK"].map(parseAppLanguage)).toEqual(["system", "en", "zh-HK"]);
  });

  it("returns null for locales that are not shipped and for other values", () => {
    expect(parseAppLanguage("zh-CN")).toBeNull();
    expect(parseAppLanguage("de")).toBeNull();
    expect(parseAppLanguage(null)).toBeNull();
  });

  it("offers system, Traditional Chinese (Hong Kong) and English", () => {
    expect(LANGUAGE_OPTIONS.map((option) => option.value)).toEqual(["system", "zh-HK", "en"]);
  });
});

describe("formatLanguageOptionLabel", () => {
  const english = LANGUAGE_OPTIONS.find((option) => option.value === "en")!;
  const chinese = LANGUAGE_OPTIONS.find((option) => option.value === "zh-HK")!;
  const system = LANGUAGE_OPTIONS.find((option) => option.value === "system")!;

  it("shows the native name and the name in the UI language", () => {
    expect(formatLanguageOptionLabel(chinese, "en", "System")).toBe(
      "繁體中文（香港） - Traditional Chinese (Hong Kong)",
    );
    expect(formatLanguageOptionLabel(english, "zh-HK", "系統")).toBe("English - 英文");
  });

  it("shows a name once when both are the same", () => {
    expect(formatLanguageOptionLabel(english, "en", "System")).toBe("English");
    expect(formatLanguageOptionLabel(chinese, "zh-HK", "系統")).toBe("繁體中文（香港）");
  });

  it("uses the system label for the system option", () => {
    expect(formatLanguageOptionLabel(system, "zh-HK", "系統")).toBe("系統");
  });
});

describe("resolveSupportedLocale", () => {
  it("returns an explicit choice", () => {
    expect(resolveSupportedLocale("en", ["zh-HK"])).toBe("en");
    expect(resolveSupportedLocale("zh-HK", ["en-US"])).toBe("zh-HK");
  });

  it("maps any Chinese system locale to zh-HK and English to English", () => {
    for (const locale of ["zh", "zh-CN", "zh-TW", "zh-Hant-HK", "zh-Hans-US"]) {
      expect(resolveSupportedLocale("system", [locale])).toBe("zh-HK");
    }
    expect(resolveSupportedLocale("system", ["en-GB"])).toBe("en");
  });

  it("uses the first recognised system locale", () => {
    expect(resolveSupportedLocale("system", ["fr-FR", "en-US", "zh-HK"])).toBe("en");
  });

  it("falls back to zh-HK", () => {
    expect(resolveSupportedLocale("system", ["fr-FR", "ja-JP"])).toBe("zh-HK");
    expect(resolveSupportedLocale("system", [])).toBe("zh-HK");
  });
});
