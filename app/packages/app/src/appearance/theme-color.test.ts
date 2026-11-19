import { readFileSync } from "node:fs";
import path from "node:path";
import { JSDOM } from "jsdom";
import { describe, expect, it } from "vitest";
import { darkTheme, lightTheme } from "@/styles/theme";
import { setThemeColor } from "./theme-color";

const indexHtml = readFileSync(path.resolve(__dirname, "../../public/index.html"), "utf8");

function themeColors(doc: Document): Array<[string | null, string | null]> {
  return Array.from(doc.querySelectorAll('meta[name="theme-color"]')).map((meta) => [
    meta.getAttribute("media"),
    meta.getAttribute("content"),
  ]);
}

function page(local: Record<string, string> = {}) {
  return new JSDOM(indexHtml, {
    url: "https://ai.example/new",
    runScripts: "dangerously",
    pretendToBeVisual: true,
    beforeParse(window) {
      for (const [key, value] of Object.entries(local)) window.localStorage.setItem(key, value);
      Object.assign(window, { matchMedia: () => ({ matches: true }) });
    },
  }).window.document;
}

describe("theme-color (the browser's bar, an installed app's status bar)", () => {
  it("starts as the page's background in the system's scheme", () => {
    expect(themeColors(page())).toEqual([
      ["(prefers-color-scheme: light)", lightTheme.colors.surface0.toLowerCase()],
      ["(prefers-color-scheme: dark)", darkTheme.colors.surface0.toLowerCase()],
    ]);
  });

  it("is the saved theme's background before the app arrives", () => {
    const light = page({ "@paseo:app-settings": JSON.stringify({ theme: "light" }) });
    expect(themeColors(light).map(([, color]) => color)).toEqual(["#ffffff", "#ffffff"]);
    const dark = page({ "@paseo:app-settings": JSON.stringify({ theme: "dark" }) });
    expect(themeColors(dark).map(([, color]) => color)).toEqual(["#181b1a", "#181b1a"]);
    const auto = page({ "@paseo:app-settings": JSON.stringify({ theme: "auto" }) });
    expect(themeColors(auto).map(([, color]) => color)).toEqual(["#ffffff", "#181b1a"]);
  });

  it("takes the app's theme in every scheme", () => {
    const doc = page();
    setThemeColor(doc, "#000000");
    expect(themeColors(doc).map(([, color]) => color)).toEqual(["#000000", "#000000"]);
  });
});
