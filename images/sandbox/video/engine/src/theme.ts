// Looks shared by every part of the composition: fonts, themes, small helpers.
import { random } from "./rt";

const EMOJI = "'Noto Color Emoji'";
const HEI = "'Chiron Hei HK','Noto Sans CJK HK','Noto Sans CJK TC'";
export const FONTS: Record<string, string> = {
  hei: `${HEI},${EMOJI},sans-serif`,                                   // clean sans, every weight; the default
  round: `'Chiron GoRound TC',${HEI},${EMOJI},sans-serif`,              // rounded, friendly
  serif: `'Chiron Sung HK','Noto Serif CJK HK',${EMOJI},serif`,         // print serif
  kai: `'LXGW WenKai TC',${HEI},${EMOJI},serif`,                        // handwritten brush
  cute: `'Huninn','Chiron GoRound TC',${HEI},${EMOJI},sans-serif`,      // soft round
  display: `'Dela Gothic One',${HEI},${EMOJI},sans-serif`,              // very heavy poster face (Cantonese-only characters fall back to hei)
  pixel: `'DotGothic16',${HEI},${EMOJI},monospace`,                     // pixel / retro game
  impact: `'Anton',${HEI},${EMOJI},sans-serif`,                         // tall condensed Latin and digits
  poppins: `'Poppins',${HEI},${EMOJI},sans-serif`,                      // geometric Latin and digits
  bungee: `'Bungee',${HEI},${EMOJI},sans-serif`,                        // chunky Latin sign lettering
};

export type CaptionStyle = {
  size?: number; color?: string; position?: "bottom" | "center" | "top"; y?: number;
  look?: "outline" | "box" | "pill" | "karaoke" | "reveal"; animate?: "none" | "pop" | "rise" | "slam";
  maxWidth?: number; font?: string; accent?: string;
};
export type Theme = {
  font: string; titleFont: string; accent: string; accent2: string; ink: string;
  cardBg: string; cardStyle: "plain" | "rings" | "gradient" | "dots" | "grid" | "rays";
  caption: CaptionStyle; title: string;          // default text style for "title"
};
export const THEMES: Record<string, Theme> = {
  clean: { font: "hei", titleFont: "hei", accent: "#ffd60a", accent2: "#ffffff", ink: "#ffffff", cardBg: "#111111", cardStyle: "plain", caption: { look: "outline", animate: "pop" }, title: "title" },
  bold: { font: "hei", titleFont: "hei", accent: "#ffe600", accent2: "#ff3b30", ink: "#ffffff", cardBg: "#0b0b0b", cardStyle: "rays", caption: { look: "outline", animate: "slam", size: 76 }, title: "impact" },
  neon: { font: "hei", titleFont: "hei", accent: "#19e3ff", accent2: "#ff3df0", ink: "#ffffff", cardBg: "#070b1f", cardStyle: "rings", caption: { look: "karaoke", animate: "rise" }, title: "neon" },
  cute: { font: "round", titleFont: "cute", accent: "#ff7eb6", accent2: "#7a5cff", ink: "#ffffff", cardBg: "#ffd1e3", cardStyle: "dots", caption: { look: "pill", animate: "pop" }, title: "bubble" },
  news: { font: "hei", titleFont: "hei", accent: "#e10600", accent2: "#ffffff", ink: "#ffffff", cardBg: "#0a1a3a", cardStyle: "grid", caption: { look: "box", animate: "none", accent: "#ffd60a" }, title: "tag" },
  comic: { font: "hei", titleFont: "display", accent: "#ffdd00", accent2: "#ff2d55", ink: "#ffffff", cardBg: "#ffdd00", cardStyle: "rays", caption: { look: "outline", animate: "slam" }, title: "burst" },
};
export const theme = (name?: string, over?: Partial<Theme>): Theme => ({ ...(THEMES[name || "clean"] || THEMES.clean), ...Object.fromEntries(Object.entries(over || {}).filter(([, v]) => v != null)) });
export const font = (name: string | undefined, fallback: string) => FONTS[name || fallback] || (name ? `'${name}',${FONTS.hei}` : FONTS[fallback]) || FONTS.hei;

// a solid outline made of shadows (works on every glyph, emoji included)
export const outline = (px: number, c = "#000", glow = "") => {
  const s: string[] = [];
  for (let a = 0; a < 16; a++) s.push(`${(Math.cos((a * Math.PI) / 8) * px).toFixed(1)}px ${(Math.sin((a * Math.PI) / 8) * px).toFixed(1)}px 0 ${c}`);
  return s.join(",") + `, 0 ${px}px ${px * 2}px rgba(0,0,0,.5)` + (glow ? `,${glow}` : "");
};
export const isLight = (hex: string) => { const m = /^#?([0-9a-f]{6})$/i.exec(hex || ""); if (!m) return false; const n = parseInt(m[1], 16); return (0.299 * (n >> 16) + 0.587 * ((n >> 8) & 255) + 0.114 * (n & 255)) > 150; };
export const clamp = (v: number, a = 0, b = 1) => Math.max(a, Math.min(b, v));
export const ease = (t: number) => { t = clamp(t); return t < 0.5 ? 4 * t * t * t : 1 - Math.pow(-2 * t + 2, 3) / 2; };
export const easeOut = (t: number) => 1 - Math.pow(1 - clamp(t), 3);
export const backOut = (t: number) => { t = clamp(t); const c = 1.9; return 1 + (c + 1) * Math.pow(t - 1, 3) + c * Math.pow(t - 1, 2); };
export const rnd = (seed: string | number, i: number) => random(`${seed}-${i}`);

// "*word*" marks an emphasised word. Returns the characters with their flags; "\n" is kept as a character.
export type Ch = { c: string; em: boolean; i: number };
export const parseRich = (text: string): Ch[] => {
  const out: Ch[] = []; let em = false, i = 0;
  for (const c of Array.from(text)) { if (c === "*") { em = !em; continue; } out.push({ c, em, i: c === "\n" ? -1 : i }); if (c !== "\n") i++; }
  return out;
};
