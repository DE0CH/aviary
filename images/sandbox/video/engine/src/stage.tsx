// The timeline's items. Footage is plain <video> elements in the page (HyperFrames times them and mixes their
// sound); this file moves, grades and transitions their wrappers at every frame, and draws the cards.
import React from "react";
import { createPortal } from "react-dom";
import { AbsoluteFill, random, useCurrentFrame, useVideoConfig } from "./rt";
import { backOut, clamp, ease, easeOut, font, isLight, parseRich, rnd, Theme } from "./theme";
import { Backdrop } from "./graphics";
import lottie from "lottie-web";

export type Punch = { at: number; frames: number; scale: number; focus?: [number, number] };
export type Item = {
  kind: "video" | "card";
  from: number; frames: number;                 // where it sits on the output timeline, in frames
  motion?: { type: string; amount?: number; focus?: [number, number] } | null;
  punches?: Punch[]; look?: string; inset?: number; insetY?: number;
  background?: string; backdrop?: string; title?: string; subtitle?: string; color?: string; emoji?: string; lottie?: string;
  transition?: string | null; transitionFrames?: number; transitionColor?: string;   // into the NEXT item
};

const LOOKS: Record<string, string> = {
  vivid: "saturate(1.35) contrast(1.08)", warm: "sepia(.2) saturate(1.25) brightness(1.03)", cool: "hue-rotate(-10deg) saturate(1.1) brightness(1.02)",
  bw: "grayscale(1) contrast(1.15)", vintage: "sepia(.38) contrast(.95) saturate(.85) brightness(1.04)", dreamy: "brightness(1.08) saturate(1.2) contrast(.94)",
  punchy: "contrast(1.2) saturate(1.2)", dark: "brightness(.72) contrast(1.1)",
};

type S = Partial<CSSStyleDeclaration> & Record<string, string>;
// how one side of a transition looks at progress p (0 → 1)
const side = (kind: string, p: number, entering: boolean): S => {
  const bell = Math.sin(Math.PI * p), e = ease(p);
  const move = (x: number, y: number): S => ({ transform: `translate(${x}%,${y}%)` });
  switch (kind) {
    case "slide-left": return entering ? move((1 - e) * 100, 0) : move(-e * 100, 0);
    case "slide-right": return entering ? move((e - 1) * 100, 0) : move(e * 100, 0);
    case "slide-up": return entering ? move(0, (1 - e) * 100) : move(0, -e * 100);
    case "slide-down": return entering ? move(0, (e - 1) * 100) : move(0, e * 100);
    case "wipe": return entering ? { clipPath: `inset(0 ${(1 - p) * 100}% 0 0)` } : {};
    case "iris": return entering ? { clipPath: `circle(${p * 75}% at 50% 50%)` } : {};
    case "clock-wipe": return entering ? { maskImage: `conic-gradient(#000 ${p * 360}deg, transparent ${p * 360}deg)`, webkitMaskImage: `conic-gradient(#000 ${p * 360}deg, transparent ${p * 360}deg)` } : {};
    case "flip": return entering ? { transform: `perspective(2400px) rotateY(${(e - 1) * 180}deg)`, opacity: p >= 0.5 ? "1" : "0" } : { transform: `perspective(2400px) rotateY(${e * 180}deg)`, opacity: p < 0.5 ? "1" : "0" };
    case "flash": return { opacity: (entering ? p >= 0.5 : p < 0.5) ? "1" : "0", transform: `scale(${1 + 0.06 * bell})` };
    case "zoom": return entering ? { opacity: String(e), transform: `scale(${0.6 + 0.4 * e})`, filter: `blur(${(1 - e) * 14}px)` } : { opacity: String(1 - e), transform: `scale(${1 + 1.4 * e})`, filter: `blur(${e * 18}px)` };
    case "whip-left": case "whip-right": case "whip-up": case "whip-down": {
      const sign = kind === "whip-left" || kind === "whip-up" ? 1 : -1, vertical = kind === "whip-up" || kind === "whip-down";
      const off = (entering ? 1 - e : -e) * 100 * sign;
      return { transform: vertical ? `translateY(${off}%) scaleY(${1 + 0.25 * bell})` : `translateX(${off}%) scaleX(${1 + 0.25 * bell})`, filter: `blur(${bell * 22}px)` };
    }
    case "glitch": {
      const f = Math.round(p * 60), j = (n: number) => random(`g${f}-${n}`) - 0.5, band = 20 + random(`b${f}`) * 60;
      return {
        opacity: (entering ? p >= 0.5 : p < 0.5) ? "1" : "0", transform: `translate(${j(1) * 60 * bell}px,${j(2) * 16 * bell}px) skewX(${j(3) * 10 * bell}deg)`,
        filter: `hue-rotate(${j(4) * 180 * bell}deg) saturate(${1 + 2 * bell}) contrast(${1 + 0.4 * bell})`,
        clipPath: bell > 0.3 && random(`c${f}`) > 0.5 ? `polygon(0 0,100% 0,100% ${band}%,0 ${band}%,0 ${band + 6}%,100% ${band + 6}%,100% 100%,0 100%)` : "none",
      };
    }
    case "spin": return entering ? { opacity: String(e), transform: `rotate(${(1 - e) * -90}deg) scale(${0.5 + 0.5 * e})` } : { opacity: String(1 - e), transform: `rotate(${e * 90}deg) scale(${1 + e})` };
    default: return entering ? { opacity: String(p) } : {};          // fade
  }
};
const RESET: S = { transform: "none", opacity: "1", filter: "none", clipPath: "none", maskImage: "none", webkitMaskImage: "none" };
const put = (el: HTMLElement | null, s: S) => { if (el) Object.assign(el.style, s); };

/** Styles every item's wrappers for `frame`: which are visible, the transition between two, the camera, the look. */
export const applyItems = (items: Item[], frame: number, width: number, fps: number) => {
  const fx = document.getElementById("fx");
  let fxStyle: S = { opacity: "0", background: "none" };
  items.forEach((it, i) => {
    const el = document.getElementById(`item-${i}`);
    if (!el) return;
    const t = frame - it.from, on = t >= 0 && t < it.frames;
    el.style.visibility = on ? "visible" : "hidden";
    if (!on) return;
    let s: S = { ...RESET };
    const prev = items[i - 1], tin = prev?.transition && prev.transitionFrames ? prev.transitionFrames : 0;
    if (tin && t < tin) s = { ...s, ...side(prev.transition!, clamp((t + 0.5) / tin), true) };
    const tout = it.transition && it.transitionFrames && i < items.length - 1 ? it.transitionFrames : 0;
    if (tout && t >= it.frames - tout) {
      const p = clamp((t - (it.frames - tout) + 0.5) / tout), bell = Math.sin(Math.PI * p);
      s = { ...s, ...side(it.transition!, p, false) };
      if (it.transition === "flash") fxStyle = { opacity: String(Math.pow(bell, 0.6)), background: it.transitionColor || "#ffffff" };
      if (it.transition === "glitch") fxStyle = { opacity: String(bell), background: "repeating-linear-gradient(0deg, rgba(255,255,255,.10) 0 2px, transparent 2px 7px)" };
    }
    put(el, s);
    if (it.kind !== "video") return;
    // the camera: a slow move over the whole clip, plus punch-ins
    const p = clamp(t / Math.max(1, it.frames - 1)), m = it.motion, amt = m?.amount ?? 1.15, lim = ((amt - 1) / 2) * 100;
    let scale = 1, tx = 0, ty = 0, origin = m?.focus || [50, 50];
    switch (m?.type) {
      case "zoom-in": scale = 1 + (amt - 1) * p; break;
      case "zoom-out": scale = amt - (amt - 1) * p; break;
      case "pan-left": scale = amt; tx = lim * (1 - 2 * p); break;
      case "pan-right": scale = amt; tx = lim * (2 * p - 1); break;
      case "pan-up": scale = amt; ty = lim * (1 - 2 * p); break;
      case "pan-down": scale = amt; ty = lim * (2 * p - 1); break;
      case "shake": scale = 1.07; tx = (rnd("sx", t) - 0.5) * 2.4; ty = (rnd("sy", t) - 0.5) * 2.4; break;
      case "pulse": scale = 1 + 0.035 * Math.pow(Math.abs(Math.sin((t / fps) * Math.PI * (amt > 3 ? amt / 60 : 2))), 6); break;
      case "handheld": scale = 1.05; tx = Math.sin(t / 23) * 1.2 + Math.sin(t / 7.3) * 0.4; ty = Math.cos(t / 19) * 1.2; break;
    }
    for (const pu of it.punches || []) {
      const k0 = t - pu.at;
      if (k0 < 0 || k0 >= pu.frames) continue;
      scale *= 1 + (pu.scale - 1) * Math.min(easeOut(k0 / 3), ease((pu.frames - k0) / Math.min(8, pu.frames / 2)));
      if (pu.focus) origin = pu.focus;
    }
    put(document.getElementById(`cam-${i}`), { transform: `translate(${tx}%,${ty}%) scale(${scale})`, transformOrigin: `${origin[0]}% ${origin[1]}%`, filter: it.look ? LOOKS[it.look] : "none" });
    const frameEl = document.getElementById(`frame-${i}`);
    if (it.inset) {
      const e = backOut(clamp(t / (fps * 0.45)));
      put(frameEl, { transform: `translateY(${it.insetY ?? 0}%) scale(${it.inset * (0.9 + 0.1 * e)})`, borderRadius: `${width * 0.05}px`, boxShadow: `0 ${width * 0.03}px ${width * 0.08}px rgba(0,0,0,.55)`, border: `${Math.max(4, width * 0.008)}px solid #ffffff` });
    }
    const vig = document.getElementById(`vig-${i}`);
    if (vig) vig.style.opacity = it.look === "vintage" ? "1" : "0";
  });
  put(fx, fxStyle);
};

const CardEmoji: React.FC<{ data: any; px: number }> = ({ data, px }) => {
  const frame = useCurrentFrame();
  const { fps } = useVideoConfig();
  const box = React.useRef<HTMLDivElement>(null), anim = React.useRef<any>(null);
  React.useLayoutEffect(() => {
    if (!box.current) return;
    anim.current = lottie.loadAnimation({ container: box.current, renderer: "svg", loop: false, autoplay: false, animationData: JSON.parse(JSON.stringify(data)) });
    return () => { anim.current?.destroy(); anim.current = null; };
  }, [data]);
  React.useLayoutEffect(() => { const a = anim.current; if (a) a.goToAndStop(Math.floor(frame * ((data?.fr || 30) / fps)) % Math.max(1, Math.floor(a.totalFrames)), true); });
  return <div ref={box} style={{ width: px, height: px }} />;
};

const Card: React.FC<{ it: Item; th: Theme; lotties: Record<string, any> }> = ({ it, th, lotties }) => {
  const f = useCurrentFrame() - it.from;
  const { width: W, fps } = useVideoConfig();
  const bg = it.background || th.cardBg, kind = it.backdrop || (it.background ? "plain" : th.cardStyle);
  const color = it.color || (isLight(bg) ? "#111111" : "#ffffff");
  const size = W * 0.1, chars = parseRich(it.title || "");
  const glow = !isLight(bg) && kind === "rings" ? `0 0 ${size * 0.3}px ${th.accent}, 0 0 ${size * 0.8}px ${th.accent2}` : "none";
  const bar = easeOut((f - fps * 0.25) / (fps * 0.4));
  return (
    <AbsoluteFill>
      <Backdrop kind={kind} color={bg} th={th} />
      <AbsoluteFill style={{ alignItems: "center", justifyContent: "center", padding: W * 0.08 }}>
        {it.emoji || it.lottie ? <div style={{ marginBottom: W * 0.04, transform: `scale(${backOut(clamp(f / (fps * 0.4)))})` }}>
          {it.lottie && lotties[it.lottie] ? <CardEmoji data={lotties[it.lottie]} px={W * 0.3} /> : <div style={{ fontFamily: "'Noto Color Emoji'", fontSize: W * 0.22 }}>{it.emoji}</div>}</div> : null}
        <div style={{ fontFamily: font(undefined, th.titleFont), fontWeight: 900, fontSize: size, lineHeight: 1.22, color, textAlign: "center", whiteSpace: "pre-wrap", textShadow: glow }}>
          {chars.map((ch, i) => {
            if (ch.c === "\n") return <br key={i} />;
            const t = f - ch.i * 1.5, e = backOut(clamp(t / (fps * 0.32)));
            return <span key={i} style={{ display: "inline-block", whiteSpace: "pre", transform: `translateY(${(1 - e) * size * 0.8}px) scale(${0.5 + 0.5 * e})`, opacity: clamp(t / 4), color: ch.em ? (isLight(bg) ? th.accent2 : th.accent) : undefined }}>{ch.c === " " ? " " : ch.c}</span>;
          })}
        </div>
        <div style={{ width: W * 0.34 * bar, height: Math.max(6, W * 0.012), background: `linear-gradient(90deg, ${isLight(bg) ? "#111111" : th.accent}, ${isLight(bg) ? "#111111" : th.accent2})`, marginTop: W * 0.035, borderRadius: 99 }} />
        {it.subtitle ? <div style={{ fontFamily: font(undefined, th.font), fontWeight: 700, fontSize: W * 0.056, lineHeight: 1.4, color, opacity: 0.92 * clamp((f - fps * 0.35) / (fps * 0.3)), transform: `translateY(${(1 - easeOut((f - fps * 0.35) / (fps * 0.4))) * 30}px)`, textAlign: "center", marginTop: W * 0.035, whiteSpace: "pre-wrap" }}>{it.subtitle}</div> : null}
      </AbsoluteFill>
    </AbsoluteFill>
  );
};

/** The drawn parts of the items: each card, and the animated background behind an inset clip, into their wrappers. */
export const Items: React.FC<{ items: Item[]; th: Theme; lotties: Record<string, any> }> = ({ items, th, lotties }) => {
  const frame = useCurrentFrame();
  return <>{items.map((it, i) => {
    if (frame < it.from || frame >= it.from + it.frames) return null;
    if (it.kind === "card") { const el = document.getElementById(`card-${i}`); return el ? createPortal(<Card it={it} th={th} lotties={lotties} />, el, `c${i}`) : null; }
    if (it.inset) { const el = document.getElementById(`bg-${i}`); return el ? createPortal(<Backdrop kind={it.backdrop || th.cardStyle} color={it.background || th.cardBg} th={th} />, el, `b${i}`) : null; }
    return null;
  })}</>;
};
