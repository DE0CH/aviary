// Captions and on-screen texts: styles, entrance / exit / looping animations, "*emphasis*" marks.
import React from "react";
import { AbsoluteFill, useCurrentFrame, useVideoConfig } from "./rt";
import { backOut, CaptionStyle, Ch, clamp, easeOut, font, isLight, outline, parseRich, Theme } from "./theme";

export type Caption = { from: number; to: number; text: string };
export type TextItem = {
  from: number; to: number; text: string; sub?: string;
  position?: string; x?: number; y?: number;
  style?: string; size?: number; color?: string; accent?: string; background?: string; font?: string; rotate?: number;
  animate?: string; out?: string; loop?: string;
  count?: [number, number]; prefix?: string; suffix?: string; tail?: "left" | "right" | "none";
};

// ---- rich text: characters with emphasis, optional per-character treatment
const tokens = (chars: Ch[]) => {            // Latin words stay whole so a line never breaks inside one
  const out: Ch[][] = [];
  for (const ch of chars) {
    const last = out[out.length - 1];
    if (last && /[A-Za-z0-9'’%.,!?$+-]/.test(ch.c) && /[A-Za-z0-9'’%.,!?$+-]/.test(last[last.length - 1].c) && last[0].em === ch.em) last.push(ch);
    else out.push([ch]);
  }
  return out;
};
const Rich: React.FC<{ chars: Ch[]; emStyle: React.CSSProperties; charStyle?: (i: number, n: number) => React.CSSProperties | undefined; block?: boolean }> = ({ chars, emStyle, charStyle, block }) => {
  const n = chars.filter((c) => c.i >= 0).length;
  return <>{tokens(chars).map((tok, k) => {
    if (tok[0].c === "\n") return <br key={k} />;
    const st = { ...(charStyle ? charStyle(tok[0].i, n) : null), ...(tok[0].em ? emStyle : null) } as React.CSSProperties;
    const s = tok.map((c) => c.c).join("");
    return <span key={k} style={block ? { display: "inline-block", whiteSpace: "pre", ...st } : st}>{block && s === " " ? " " : s}</span>;
  })}</>;
};

// ---- captions
export const Captions: React.FC<{ captions: Caption[]; style: CaptionStyle; th: Theme }> = ({ captions, style: given, th }) => {
  const frame = useCurrentFrame();
  const { width, height, fps } = useVideoConfig();
  const c = captions.find((x) => frame >= x.from && frame < x.to);
  if (!c) return null;
  const style = { ...th.caption, ...given };
  const size = style.size || Math.round(width * 0.06);
  const pos = style.position || "bottom";
  const look = style.look || "outline", accent = style.accent || th.accent;
  const t = frame - c.from, p = clamp(t / Math.max(1, c.to - c.from));
  let transform = "", opacity = 1;
  if (style.animate === "pop") transform = `scale(${0.8 + 0.2 * backOut(t / (fps * 0.28))})`;
  else if (style.animate === "rise") { const e = easeOut(t / (fps * 0.25)); transform = `translateY(${(1 - e) * size * 0.6}px)`; opacity = e; }
  else if (style.animate === "slam") { const e = clamp(t / (fps * 0.12)); transform = `scale(${2.2 - 1.2 * e * e})`; opacity = clamp(e * 2); }
  const chars = parseRich(c.text);
  const emStyle: React.CSSProperties = look === "pill" ? { fontSize: "1.12em", color: isLight(accent) ? th.accent2 : "#ffffff" } : { color: accent, fontSize: "1.12em" };
  const charStyle = look === "karaoke" ? (i: number, n: number) => (i / n <= p * 1.08 ? { color: accent } : undefined)
    : look === "reveal" ? (i: number, n: number) => (i / n <= p * 1.7 ? undefined : { opacity: 0 }) : undefined;
  const boxed: React.CSSProperties = look === "box" ? { background: "rgba(0,0,0,.72)", padding: `${size * 0.2}px ${size * 0.45}px` }
    : look === "pill" ? { background: accent, color: isLight(accent) ? "#111111" : "#ffffff", padding: `${size * 0.22}px ${size * 0.55}px`, borderRadius: size * 0.5, boxShadow: `0 ${size * 0.1}px 0 rgba(0,0,0,.35)` }
    : { textShadow: outline(Math.max(3, size * 0.07)) };
  return (
    <AbsoluteFill style={{ justifyContent: style.y != null ? "flex-start" : pos === "top" ? "flex-start" : pos === "center" ? "center" : "flex-end", alignItems: "center", paddingTop: style.y != null ? `${(style.y / 100) * height}px` : height * 0.12, paddingBottom: height * 0.16 }}>
      <div style={{
        fontFamily: font(style.font, th.font), fontWeight: 900, fontSize: size, lineHeight: 1.3, color: style.color || "#ffffff", textAlign: "center",
        maxWidth: `${style.maxWidth || 86}%`, whiteSpace: "pre-wrap", transform, opacity, ...boxed,
      }}><Rich chars={chars} emStyle={emStyle} charStyle={charStyle} /></div>
    </AbsoluteFill>
  );
};

// ---- texts
const POS: Record<string, [number, number]> = {
  top: [50, 12], center: [50, 50], bottom: [50, 86], "top-left": [6, 7], "top-right": [94, 7], "bottom-left": [6, 93], "bottom-right": [94, 93], upper: [50, 28], lower: [50, 72], left: [6, 50], right: [94, 50],
};
const DEFAULT_ANIM: Record<string, string> = { tag: "pop", bubble: "pop", burst: "pop", impact: "pop", stamp: "stamp", "lower-third": "slide-right", neon: "chars", gradient: "chars", counter: "pop", highlight: "fade" };

const motion = (kind: string, t: number, fps: number, width: number, entering: boolean) => {
  // t = frames since the start (entering) or until the end (leaving); returns opacity + transform
  const d = (s: number) => clamp(t / (fps * s));
  switch (kind) {
    case "none": return { o: 1, tr: "" };
    case "pop": { const e = entering ? backOut(d(0.35)) : d(0.15); return { o: d(0.1), tr: `scale(${0.3 + 0.7 * e})` }; }
    case "slide-left": { const e = easeOut(d(0.35)); return { o: d(0.15), tr: `translateX(${(1 - e) * width * 0.5}px)` }; }
    case "slide-right": { const e = easeOut(d(0.35)); return { o: d(0.15), tr: `translateX(${(e - 1) * width * 0.5}px)` }; }
    case "slide-up": { const e = easeOut(d(0.35)); return { o: d(0.15), tr: `translateY(${(1 - e) * width * 0.25}px)` }; }
    case "drop": { const e = entering ? backOut(d(0.45)) : d(0.2); return { o: d(0.1), tr: `translateY(${(e - 1) * width * 0.7}px)` }; }
    case "stamp": { const e = d(0.14); const shake = entering && t > fps * 0.14 && t < fps * 0.3 ? Math.sin(t * 2.6) * 6 : 0; return { o: clamp(e * 3), tr: `translate(${shake}px,0) scale(${3 - 2 * e * e})` }; }
    case "spin": { const e = easeOut(d(0.4)); return { o: d(0.15), tr: `rotate(${(1 - e) * -200}deg) scale(${0.2 + 0.8 * e})` }; }
    default: return { o: d(0.2), tr: "" };          // fade, chars, typewriter (their own detail is per character)
  }
};
const looping = (kind: string | undefined, frame: number, fps: number) => {
  const s = frame / fps;
  switch (kind) {
    case "wiggle": return `rotate(${Math.sin(s * 9) * 4}deg)`;
    case "pulse": return `scale(${1 + Math.sin(s * 7) * 0.06})`;
    case "float": return `translateY(${Math.sin(s * 3) * 14}px)`;
    case "shake": return `translate(${Math.sin(s * 47) * 5}px,${Math.cos(s * 53) * 5}px)`;
    default: return "";
  }
};

const Burst: React.FC<{ color: string; spikes?: number; turn: number }> = ({ color, spikes = 14, turn }) => {
  const pts: string[] = [];
  for (let i = 0; i < spikes * 2; i++) { const r = i % 2 ? 38 : 50, a = (i * Math.PI) / spikes; pts.push(`${50 + r * Math.cos(a)},${50 + r * Math.sin(a)}`); }
  return <svg viewBox="0 0 100 100" style={{ position: "absolute", left: "-22%", top: "-45%", width: "144%", height: "190%", transform: `rotate(${turn}deg)`, overflow: "visible" }} preserveAspectRatio="none">
    <polygon points={pts.join(" ")} fill={color} stroke="#111111" strokeWidth={1.6} strokeLinejoin="round" /></svg>;
};

export const Texts: React.FC<{ texts: TextItem[]; th: Theme }> = ({ texts, th }) => {
  const frame = useCurrentFrame();
  const { width, fps } = useVideoConfig();
  return (
    <AbsoluteFill>
      {texts.filter((t) => frame >= t.from && frame < t.to).map((t, key) => {
        let st = t.style || "title"; if (st === "title" && th.title !== "title" && !t.style) st = th.title;
        const lt = st === "lower-third";
        const named = POS[t.position || (lt ? "" : "top")] || (lt ? [6, 74] : POS.top);
        const px = t.x ?? (t.y != null && !t.position && !lt ? 50 : named[0]), py = t.y ?? (t.x != null && !t.position && !lt ? 50 : named[1]);
        const accent = t.accent || th.accent, accent2 = th.accent2;
        const big = st === "title" || st === "neon" || st === "gradient" || st === "highlight";
        const size = t.size || Math.round(width * (st === "impact" ? 0.13 : st === "counter" ? 0.2 : st === "stamp" ? 0.1 : st === "burst" ? 0.08 : big ? 0.08 : st === "plain" ? 0.045 : st === "label" ? 0.05 : 0.06));
        const anim = t.animate || DEFAULT_ANIM[st] || "fade", outAnim = t.out || "fade";
        const tin = frame - t.from, tout = t.to - frame;
        const a = motion(anim, tin, fps, width, true), b = motion(outAnim, tout, fps, width, false);
        const leaving = tout < fps * 0.25;
        const m = leaving ? b : a;
        const tx = px <= 10 ? "0%" : px >= 90 ? "-100%" : "-50%", ty = py <= 10 ? "0%" : py >= 90 ? "-100%" : "-50%";
        const align = px <= 10 ? "left" : px >= 90 ? "right" : "center";
        const rot = t.rotate ?? (st === "impact" ? -4 : st === "stamp" ? -8 : st === "burst" ? -6 : 0);
        const chars = parseRich(st === "counter" ? "" : t.text);
        const perChar: ((i: number) => React.CSSProperties | undefined) | undefined = anim === "chars" ? (i: number) => { const e = backOut(clamp((tin - i * 1.6) / (fps * 0.3))); return { transform: `translateY(${(1 - e) * size * 0.9}px) scale(${0.4 + 0.6 * e})`, opacity: clamp((tin - i * 1.6) / 4) }; }
          : anim === "typewriter" ? (i: number) => (tin >= i * (fps / 14) ? undefined : { opacity: 0 }) : undefined;
        const fam = font(t.font, st === "impact" || st === "burst" || st === "stamp" ? th.titleFont : st === "counter" ? "impact" : th.font);
        let body: React.CSSProperties = { color: t.color || th.ink, textShadow: outline(Math.max(3, size * 0.06)) };
        let em: React.CSSProperties = { color: accent };
        let behind: React.ReactNode = null, after: React.ReactNode = null;
        if (st === "label" || (t.background && st !== "tag")) body = { color: t.color || "#ffffff", background: t.background || "rgba(0,0,0,.72)", padding: `${size * 0.25}px ${size * 0.5}px` };
        if (st === "plain") body = { ...body, fontWeight: 600 };
        if (st === "tag") { body = { color: t.color || (isLight(t.background || accent) ? "#111111" : "#ffffff"), background: t.background || accent, padding: `${size * 0.22}px ${size * 0.6}px`, borderRadius: size, boxShadow: `0 ${size * 0.12}px 0 rgba(0,0,0,.35)` }; em = { color: isLight(t.background || accent) ? accent2 : "#ffe066" }; }
        if (st === "impact") body = { color: t.color || th.ink, textShadow: `${outline(Math.max(5, size * 0.07))}, ${size * 0.07}px ${size * 0.07}px 0 ${accent}`, lineHeight: 1.08, letterSpacing: "0.01em" };
        if (st === "neon") { body = { color: t.color || "#ffffff", textShadow: `0 0 ${size * 0.08}px #fff, 0 0 ${size * 0.25}px ${accent}, 0 0 ${size * 0.6}px ${accent}, 0 0 ${size}px ${accent2}` }; em = { color: accent }; }
        if (st === "gradient") { body = { backgroundImage: `linear-gradient(100deg, ${accent}, ${accent2})`, WebkitBackgroundClip: "text", backgroundClip: "text", color: "transparent", filter: `drop-shadow(0 ${size * 0.05}px ${size * 0.05}px rgba(0,0,0,.75))` }; em = { color: "#ffffff" }; }
        if (st === "stamp") { const c = t.color || accent2; body = { color: c, border: `${size * 0.09}px solid ${c}`, padding: `${size * 0.1}px ${size * 0.4}px`, borderRadius: size * 0.16, letterSpacing: "0.06em", background: "rgba(255,255,255,.06)", textShadow: "none" }; em = { color: accent }; }
        if (st === "bubble") {
          const tail = t.tail || "left";
          body = { color: t.color || "#111111", background: t.background || "#ffffff", padding: `${size * 0.35}px ${size * 0.6}px`, borderRadius: size * 0.7, boxShadow: `0 ${size * 0.12}px 0 rgba(0,0,0,.3)`, border: `${Math.max(3, size * 0.06)}px solid #111111`, textShadow: "none" }; em = { color: accent };
          if (tail !== "none") after = <div style={{ position: "absolute", bottom: -size * 0.42, [tail]: size * 0.9, width: size * 0.6, height: size * 0.6, background: t.background || "#ffffff", borderRight: `${Math.max(3, size * 0.06)}px solid #111111`, borderBottom: `${Math.max(3, size * 0.06)}px solid #111111`, transform: "rotate(45deg) skew(12deg,12deg)" } as React.CSSProperties} />;
        }
        if (st === "burst") { body = { color: t.color || "#111111", textShadow: "none", padding: `${size * 0.5}px ${size * 0.9}px`, lineHeight: 1.1 }; em = { color: accent2 }; behind = <Burst color={t.background || accent} turn={(frame - t.from) * 0.35} />; }
        if (st === "highlight") {
          const e = easeOut(tin / (fps * 0.35));
          body = { color: t.color || (isLight(t.background || accent) ? "#111111" : "#ffffff"), textShadow: "none", padding: `${size * 0.08}px ${size * 0.3}px`, backgroundImage: `linear-gradient(${t.background || accent}, ${t.background || accent})`, backgroundRepeat: "no-repeat", backgroundSize: `${e * 100}% 100%` }; em = { color: isLight(t.background || accent) ? accent2 : "#ffe066" };
        }
        const grad: React.CSSProperties = { backgroundImage: `linear-gradient(100deg, ${accent}, ${accent2})`, WebkitBackgroundClip: "text", backgroundClip: "text", color: "transparent" };
        const charStyle = st === "gradient" && anim === "chars" ? (i: number, n: number) => ({ ...grad, ...(perChar ? perChar(i) : null) }) : perChar;
        if (st === "gradient") em = { ...em, backgroundImage: "none", WebkitTextFillColor: "#ffffff" };
        let inner: React.ReactNode = <Rich chars={chars} emStyle={em} charStyle={charStyle} block={anim === "chars"} />;
        if (st === "counter") {
          const [c0, c1] = t.count || [0, 100], e = easeOut(tin / Math.max(1, (t.to - t.from) * 0.6));
          const v = c0 + (c1 - c0) * e, digits = Number.isInteger(c0) && Number.isInteger(c1) ? 0 : 1;
          body = { color: t.color || accent, textShadow: outline(Math.max(5, size * 0.05)), lineHeight: 1 };
          inner = <><div>{t.prefix || ""}{v.toLocaleString("en-US", { minimumFractionDigits: digits, maximumFractionDigits: digits })}{t.suffix || ""}</div>
            {t.text ? <div style={{ fontFamily: font(t.font, th.font), fontSize: size * 0.26, color: "#ffffff", marginTop: size * 0.08, textShadow: outline(Math.max(3, size * 0.02)) }}>{t.text}</div> : null}</>;
        }
        if (lt) {
          const e = easeOut(tin / (fps * 0.45));
          return (
            <div key={key} style={{ position: "absolute", left: `${px}%`, top: `${py}%`, transform: `translate(0,-50%) ${m.tr}`, opacity: m.o, display: "flex", alignItems: "stretch", fontFamily: font(t.font, th.font), maxWidth: "88%" }}>
              <div style={{ width: size * 0.22, background: accent, transform: `scaleY(${e})`, transformOrigin: "bottom" }} />
              <div style={{ background: "rgba(0,0,0,.78)", padding: `${size * 0.28}px ${size * 0.6}px ${size * 0.3}px`, clipPath: `inset(0 ${(1 - e) * 100}% 0 0)` }}>
                <div style={{ fontWeight: 900, fontSize: size, lineHeight: 1.2, color: t.color || "#ffffff", whiteSpace: "pre-wrap" }}><Rich chars={chars} emStyle={{ color: accent }} /></div>
                {t.sub ? <div style={{ fontWeight: 600, fontSize: size * 0.58, lineHeight: 1.3, color: accent, marginTop: size * 0.08, whiteSpace: "pre-wrap" }}>{t.sub}</div> : null}
              </div>
            </div>
          );
        }
        return (
          <div key={key} style={{ position: "absolute", left: `${px}%`, top: `${py}%`, transform: `translate(${tx},${ty})`, maxWidth: "90%", width: "max-content" }}>
            <div style={{ transform: `${m.tr} ${looping(t.loop, frame, fps)} rotate(${rot}deg)`, opacity: m.o, position: "relative" }}>
              {behind}
              <div style={{ position: "relative", fontFamily: fam, fontWeight: 900, fontSize: size, lineHeight: 1.25, textAlign: align as any, whiteSpace: "pre-wrap", ...body }}>{inner}</div>
              {after}
            </div>
          </div>
        );
      })}
    </AbsoluteFill>
  );
};
