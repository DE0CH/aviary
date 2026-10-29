import { beforeAll, describe, it, expect } from "vitest";
import {
  isLanguageSupported,
  isGrammarLoaded,
  getGrammarForFile,
  getSupportedExtensions,
  getParserForFile,
  loadGrammarForFile,
} from "../parsers.js";
import { codeMirrorLanguage } from "../codemirror.js";

describe("loadGrammarForFile", () => {
  it("loads a language's grammar on first use, and only that family", async () => {
    expect(isGrammarLoaded("main.go")).toBe(false);
    expect(getParserForFile("main.go")).toBeNull();
    const grammar = await loadGrammarForFile("main.go");
    expect(grammar?.parser).toBeTruthy();
    expect(getParserForFile("main.go")).toBe(grammar?.parser);
    expect(isGrammarLoaded("Main.java")).toBe(false);
  });

  it("resolves to null for a language without a grammar", async () => {
    await expect(loadGrammarForFile("notes.txt")).resolves.toBeNull();
    await expect(loadGrammarForFile("Makefile")).resolves.toBeNull();
  });
});

describe("isLanguageSupported", () => {
  it("returns true for supported file extensions", () => {
    expect(isLanguageSupported("test.js")).toBe(true);
    expect(isLanguageSupported("test.ts")).toBe(true);
    expect(isLanguageSupported("test.tsx")).toBe(true);
    expect(isLanguageSupported("test.py")).toBe(true);
    expect(isLanguageSupported("test.go")).toBe(true);
    expect(isLanguageSupported("test.rs")).toBe(true);
    expect(isLanguageSupported("test.json")).toBe(true);
    expect(isLanguageSupported("test.css")).toBe(true);
    expect(isLanguageSupported("test.html")).toBe(true);
    expect(isLanguageSupported("test.java")).toBe(true);
    expect(isLanguageSupported("test.swift")).toBe(true);
    expect(isLanguageSupported("test.dart")).toBe(true);
    expect(isLanguageSupported("test.cs")).toBe(true);
    expect(isLanguageSupported("test.nix")).toBe(true);
    expect(isLanguageSupported("test.ex")).toBe(true);
    expect(isLanguageSupported("Counter.svelte")).toBe(true);
    expect(isLanguageSupported("Page.astro")).toBe(true);
    expect(isLanguageSupported("App.vue")).toBe(true);
  });

  it("returns false for unsupported file extensions", () => {
    expect(isLanguageSupported("test.xyz")).toBe(false);
    expect(isLanguageSupported("test.txt")).toBe(false);
    expect(isLanguageSupported("test.csv")).toBe(false);
  });

  it("returns false for files without extensions", () => {
    expect(isLanguageSupported("Makefile")).toBe(false);
  });

  it("handles nested paths", () => {
    expect(isLanguageSupported("src/utils/test.ts")).toBe(true);
    expect(isLanguageSupported("deep/nested/path/file.py")).toBe(true);
  });
});

describe("getSupportedExtensions", () => {
  it("returns an array of extension strings", () => {
    const extensions = getSupportedExtensions();

    expect(Array.isArray(extensions)).toBe(true);
    expect(extensions.length).toBeGreaterThan(0);
  });

  it("includes common extensions", () => {
    const extensions = getSupportedExtensions();

    expect(extensions).toContain("js");
    expect(extensions).toContain("ts");
    expect(extensions).toContain("tsx");
    expect(extensions).toContain("py");
    expect(extensions).toContain("go");
    expect(extensions).toContain("rs");
    expect(extensions).toContain("swift");
    expect(extensions).toContain("dart");
    expect(extensions).toContain("cs");
    expect(extensions).toContain("nix");
    expect(extensions).toContain("json");
    expect(extensions).toContain("svelte");
    expect(extensions).toContain("astro");
    expect(extensions).toContain("vue");
  });
});

describe("getParserForFile", () => {
  beforeAll(async () => {
    await Promise.all(getSupportedExtensions().map((ext) => loadGrammarForFile(`source.${ext}`)));
  });

  it("projects the parser retained by the editor language", () => {
    for (const extension of getSupportedExtensions()) {
      const filename = `source.${extension}`;
      const grammar = getGrammarForFile(filename);
      expect(grammar).not.toBeNull();
      expect(getParserForFile(filename)).toBe(codeMirrorLanguage(grammar!).parser);
    }
  });

  it("returns a parser for supported files", () => {
    expect(getParserForFile("test.js")).not.toBeNull();
    expect(getParserForFile("test.py")).not.toBeNull();
  });

  it("returns null for unsupported files", () => {
    expect(getParserForFile("test.xyz")).toBeNull();
  });

  it("returns null for files without extension", () => {
    expect(getParserForFile("noext")).toBeNull();
  });

  it("is case-insensitive for extensions", () => {
    expect(getParserForFile("test.JS")).not.toBeNull();
    expect(getParserForFile("test.Py")).not.toBeNull();
  });
});
