import { beforeAll, describe, expect, it } from "vitest";
import { loadAllGrammarsForTests } from "./highlight-grammars.test-support";
import {
  extensionFromPath,
  highlightToKeyedLines,
  MAX_HIGHLIGHT_CHARS,
  plainKeyedLines,
  tokenizeToLines,
} from "./highlight-cache";

beforeAll(loadAllGrammarsForTests);

describe("extensionFromPath", () => {
  it("extracts a lowercased extension regardless of absolute/relative path", () => {
    expect(extensionFromPath("/repo/src/Index.TS")).toBe("ts");
    expect(extensionFromPath("src/index.ts")).toBe("ts");
    expect(extensionFromPath("a.b/c.tsx")).toBe("tsx");
  });

  it("returns null for paths without a usable extension", () => {
    expect(extensionFromPath(null)).toBeNull();
    expect(extensionFromPath(undefined)).toBeNull();
    expect(extensionFromPath("Makefile")).toBeNull();
    expect(extensionFromPath(".gitignore")).toBeNull();
    expect(extensionFromPath("trailingdot.")).toBeNull();
  });
});

describe("tokenizeToLines", () => {
  it("returns one token array per line for a supported language", () => {
    const lines = tokenizeToLines("const a = 1;\nconst b = 2;", "ts");
    expect(lines).not.toBeNull();
    expect(lines).toHaveLength(2);
    expect(lines?.[0].some((token) => token.style === "keyword")).toBe(true);
  });

  it("returns null when there is no extension", () => {
    expect(tokenizeToLines("whatever", null)).toBeNull();
  });

  it("returns null (callers render plain text) for an extension without a grammar", () => {
    expect(tokenizeToLines("line one\nline two", "unknownext")).toBeNull();
  });

  it("returns null above the size cap so callers fall back to plain text", () => {
    const huge = "x".repeat(MAX_HIGHLIGHT_CHARS + 1);
    expect(tokenizeToLines(huge, "ts")).toBeNull();
  });

  it("serves a cached result on repeat calls (identity-stable)", () => {
    const first = tokenizeToLines("const cached = true;", "ts");
    const second = tokenizeToLines("const cached = true;", "ts");
    expect(first).toBe(second);
  });
});

describe("highlightToKeyedLines", () => {
  it("produces stable keys for lines and tokens", () => {
    const keyed = highlightToKeyedLines("const a = 1;", "ts");
    expect(keyed?.[0].key).toBe("line-0");
    expect(keyed?.[0].tokens[0].key).toBe("0-0");
  });

  it("returns null when highlighting is unavailable", () => {
    expect(highlightToKeyedLines("text", null)).toBeNull();
  });
});

describe("plainKeyedLines", () => {
  it("splits code into style-less lines with the highlighted lines' keys", () => {
    const keyed = plainKeyedLines("a\nb");
    expect(keyed).toHaveLength(2);
    expect(keyed[1].key).toBe("line-1");
    expect(keyed.map((line) => line.tokens.map((t) => t.token))).toEqual([
      [{ text: "a", style: null }],
      [{ text: "b", style: null }],
    ]);
  });
});
