import { readdirSync, readFileSync, statSync } from "node:fs";
import path from "node:path";
import { describe, expect, it } from "vitest";
import {
  COMPOSER_ROUTE_PATTERN,
  CONVERSATION_ROUTE_PATTERN,
  isComposerRoute,
} from "./early-input-routes";

const appDir = path.resolve(__dirname, "../app");
const indexHtml = readFileSync(path.resolve(__dirname, "../../public/index.html"), "utf8");

/** Every route file as a sample path: [param] segments become a value, index files their folder. */
function routeSamples(dir: string, prefix = ""): string[] {
  return readdirSync(dir).flatMap((name) => {
    const full = path.join(dir, name);
    if (statSync(full).isDirectory()) return routeSamples(full, `${prefix}/${name}`);
    if (!/\.tsx?$/.test(name) || name.startsWith("_") || name.includes(".test.")) return [];
    const base = name.replace(/\.tsx?$/, "");
    const route = base === "index" ? prefix : `${prefix}/${base}`;
    return [(route || "/").replace(/\[[^\]]+\]/g, "x")];
  });
}

// A new route file must be placed here: does it show a message box?
const COMPOSER_ROUTES = new Set(["/", "/new", "/h/x", "/h/x/workspace/x", "/h/x/agent/x"]);

describe("early message box routes", () => {
  it("public/index.html holds the same patterns", () => {
    const literal = (pattern: string) => `/${pattern.replace(/\//g, "\\/")}/`;
    expect(indexHtml).toContain(`var COMPOSER = ${literal(COMPOSER_ROUTE_PATTERN)};`);
    expect(indexHtml).toContain(`var CONV = ${literal(CONVERSATION_ROUTE_PATTERN)};`);
  });

  it("classifies every route file", () => {
    const samples = routeSamples(appDir);
    expect(samples.length).toBeGreaterThan(20);
    for (const sample of samples) {
      expect([sample, isComposerRoute(sample)]).toEqual([sample, COMPOSER_ROUTES.has(sample)]);
      expect(isComposerRoute(`${sample}/`)).toBe(COMPOSER_ROUTES.has(sample));
    }
  });
});
