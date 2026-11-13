import { readFileSync } from "node:fs";
import path from "node:path";
import { describe, expect, it } from "vitest";
import { darkTheme } from "@/styles/theme";

// The web app manifest behind Chrome's "Install app" on Android (docs/paseo-ui.md "Installed on a phone").
const manifest = JSON.parse(readFileSync(path.resolve(__dirname, "../../public/branding/manifest.json"), "utf8"));

/** Width and height from a PNG's IHDR. */
function pngSize(file: string): string {
  const bytes = readFileSync(path.resolve(__dirname, "../../public", file.replace(/^\//, "")));
  expect(bytes.subarray(1, 4).toString("latin1")).toBe("PNG");
  return `${bytes.readUInt32BE(16)}x${bytes.readUInt32BE(20)}`;
}

describe("public/branding/manifest.json (the bundle's neutral one)", () => {
  it("meets Chrome's install criteria", () => {
    expect(manifest.name).toBeTruthy();
    expect(manifest.short_name).toBeTruthy();
    expect(manifest.start_url).toBe("/");
    expect(manifest.scope).toBe("/");
    expect(manifest.id).toBe("/");
    expect(manifest.display).toBe("standalone");
    expect(manifest.prefer_related_applications ?? false).toBe(false);
    const sizes = manifest.icons.map((icon: { sizes: string }) => icon.sizes);
    expect(sizes).toEqual(expect.arrayContaining(["192x192", "512x512"]));
  });

  it("names icons that exist at the size they claim, usable as maskable ones", () => {
    for (const icon of manifest.icons) {
      expect(icon.type).toBe("image/png");
      expect(pngSize(icon.src)).toBe(icon.sizes);
      expect(icon.purpose.split(" ")).toContain("maskable");
    }
  });

  it("leaves the orientation free, for tablets and foldables", () => {
    expect(manifest).not.toHaveProperty("orientation");
  });

  it("starts on the dark theme's background", () => {
    expect(manifest.background_color).toBe(darkTheme.colors.surface0.toLowerCase());
    expect(manifest.theme_color).toBe(darkTheme.colors.surface0.toLowerCase());
  });
});
