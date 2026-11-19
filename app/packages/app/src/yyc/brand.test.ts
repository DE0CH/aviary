import { readFileSync, readdirSync, statSync } from "node:fs";
import { join, relative } from "node:path";
import { describe, expect, it } from "vitest";
import { BRAND_FILES, brandAssetUrl, brandFaviconPath } from "./brand";

const APP = join(__dirname, "..", "..");

function sources(dir: string, out: string[] = []): string[] {
  for (const name of readdirSync(dir)) {
    const path = join(dir, name);
    if (statSync(path).isDirectory()) sources(path, out);
    // (generated files, megabytes each, hold no imports of their own)
    else if (/\.(ts|tsx)$/.test(name) && !/\.(test|gen)\.tsx?$/.test(name) && statSync(path).size < 500_000)
      out.push(path);
  }
  return out;
}

// The app is one build for every deployment (Deyao, 2026-11-13: his own chat showed YYC's logo and favicon). What
// names a deployment (its logo, tab icon, home-screen icons, manifest) is a file under /branding/ that the deployment
// puts there; the bundle holds neutral ones.
describe("a deployment's branding", () => {
  it("is addressed under /branding/ of the server the app talks to", () => {
    expect(brandAssetUrl("logo.png", "https://chat.example.org")).toBe(
      "https://chat.example.org/branding/logo.png",
    );
    expect(brandAssetUrl("logo.png", null)).toBeNull();
    expect(brandFaviconPath("none")).toBe("/branding/favicon.png");
    expect(brandFaviconPath("running")).toBe("/branding/favicon-running.png");
    expect(brandFaviconPath("attention")).toBe("/branding/favicon-attention.png");
  });

  it("has a neutral file in the bundle for every name a deployment may replace", () => {
    const shipped = readdirSync(join(APP, "public", "branding")).sort();
    expect(shipped).toEqual([...BRAND_FILES].sort());
    const manifest = JSON.parse(readFileSync(join(APP, "public", "branding", "manifest.json"), "utf8"));
    expect(JSON.stringify(manifest)).not.toMatch(/YYC/i);
    for (const icon of manifest.icons) expect(icon.src).toMatch(/^\/branding\//);
  });

  it("is the only place the page takes its icons and manifest from", () => {
    const html = readFileSync(join(APP, "public", "index.html"), "utf8");
    expect(html).toContain('<link rel="manifest" href="/branding/manifest.json" />');
    expect(html).toContain('<link rel="apple-touch-icon" href="/branding/apple-touch-icon.png" />');
    expect(html).toContain('<link rel="icon" type="image/png" href="/branding/favicon.png" />');
    // nothing of one deployment at the bundle's root
    expect(readdirSync(join(APP, "public")).sort()).toEqual(["branding", "index.html", "robots.txt"]);
  });

  it("is not drawn from an asset built into the app", () => {
    const offenders = sources(join(APP, "src"))
      .filter((path) => /yyc-logo|assets\/images\/favicon/.test(readFileSync(path, "utf8")))
      .map((path) => relative(APP, path));
    expect(offenders).toEqual([]);
  }, 60_000);
});
